//! `ccteam project rm|stop` integration tests.
//!
//! Covers the reusable remove engine (`run_remove`) reached via the
//! v0.8.6 W3 `ccteam project` group (the flat `ccteam remove` alias was
//! deleted in v0.8.6 W4a — `project rm` is the only path now):
//!   t01–t02   dry-run + basic deregister (no fs change / config drop)
//!   t03       --purge deletes ccteam footprint ONLY (W2 layout): .ccteam/
//!             + ccteam hooks in settings.local.json; keeps all user roles
//!             (including cto.md), root workflow.yaml, CLAUDE.md, .env,
//!             settings.json, business code
//!   t03b/c    surgical settings.local.json hook strip / empty-file delete
//!   t04–t06   refusal gate (tmux / claude bg / open spawn) + --force
//!   t07–t08   daemon unroster trigger ack / timeout
//!   t09–t11   state/im/registry/<slug>/ purge semantics
//!   t12–t13   `ccteam project rm` group routing + dry-run
//!   t14–t15   `ccteam project stop` (no-session ok / kills matching
//!             `ccteam-chat-<slug>-*` panes, dash-aware slug match)
//!   t16–t17   `ccteam project rm` non-purge keeps project files /
//!             --purge deletes footprint + provably keeps user content
//!   t18       `ccteam project rm --dry-run` lists stop targets, acts
//!             on nothing
//!   t19–t20   every durable progress sidecar/tree is previewed and removed;
//!             re-init of the same slug cannot recover old projections
//!
//! All tests sandbox `HOME`, `CCTEAM_HOME`, `CCTEAM_PROJECTS_ROOT`, and
//! `CCTEAM_CLAUDE_JOBS_DIR` so they never touch the developer's real
//! filesystem.

use std::path::PathBuf;
use std::process::Command;
#[cfg(unix)]
use std::{
    io::{BufRead as _, BufReader, Write as _},
    os::unix::net::UnixListener,
};

use ccteam_core::{config, CcteamPaths, ProjectEntry, ProjectState};
use ccteam_harness::execution::progress_bridge::{
    append_chat_turn_completed_if_absent, append_turn_verdict_if_changed, cleanup_progress_state,
    latest_turn_verdicts, progress_archive_path, terminal_turns_for_rebuild, TurnVerdict, Verdict,
    CHAT_TURN_COMPLETED,
};
use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;

fn cct_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ccteam")
}

struct Fixture {
    _tmp: TempDir,
    ccteam_home: PathBuf,
    projects_root: PathBuf,
    claude_jobs_dir: PathBuf,
    slug: String,
    project_dir: PathBuf,
}

impl Fixture {
    /// Build a sandboxed project layout:
    ///   <tmp>/ccteam-home/                    -- `CCTEAM_HOME`
    ///   <tmp>/projects/<slug>/.ccteam/        -- the project dir
    ///   <tmp>/jobs/                           -- `CCTEAM_CLAUDE_JOBS_DIR`
    /// Registers the slug in `~/.ccteam/config.yaml::projects[]`.
    fn new(slug: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let ccteam_home = tmp.path().join("ccteam-home");
        let projects_root = tmp.path().join("projects");
        let claude_jobs_dir = tmp.path().join("jobs");
        std::fs::create_dir_all(&ccteam_home).unwrap();
        std::fs::create_dir_all(&projects_root).unwrap();
        std::fs::create_dir_all(&claude_jobs_dir).unwrap();
        let project_dir = projects_root.join(slug);
        std::fs::create_dir_all(project_dir.join(".ccteam")).unwrap();
        std::fs::create_dir_all(project_dir.join(".claude").join("agents")).unwrap();
        // Drop a state.json so `project_dir` resolution works under
        // CcteamPaths::project_dir even before we register.
        let state = ProjectState::initial_for_team(slug.into(), "dev".into());
        state
            .save(&CcteamPaths::project_state_in(&project_dir))
            .unwrap();
        // Register in config.yaml so the runtime path matches what
        // `ccteam init` would have done.
        config::upsert_project(
            &ccteam_home,
            ProjectEntry {
                slug: slug.into(),
                path: project_dir.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".into(),
                installed_at: Utc::now(),
            },
        )
        .unwrap();
        Self {
            _tmp: tmp,
            ccteam_home,
            projects_root,
            claude_jobs_dir,
            slug: slug.into(),
            project_dir,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(cct_bin());
        c.env("HOME", self._tmp.path())
            .env("CCTEAM_HOME", &self.ccteam_home)
            .env("CCTEAM_PROJECTS_ROOT", &self.projects_root)
            .env("CCTEAM_CLAUDE_JOBS_DIR", &self.claude_jobs_dir);
        c
    }

    fn paths(&self) -> CcteamPaths {
        CcteamPaths {
            root: self.ccteam_home.clone(),
            projects_root: self.projects_root.clone(),
        }
    }

    /// Drop a progress.jsonl with one closed agent_spawn pair so the
    /// liveness probe sees no open sessions (refusal gate must not fire).
    fn seed_closed_progress(&self) {
        let p = self.paths().progress_jsonl(&self.slug);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            "{\"event\":\"workflow_start\",\"slug\":\"x\",\"ts\":\"2026-01-01T00:00:00Z\"}\n",
        )
        .unwrap();
    }

    /// Seed `<jobs>/<id>/state.json` describing a *live* claude --bg
    /// job whose `cwd` points at the project dir → triggers the
    /// claude bg refusal branch.
    fn seed_live_claude_bg(&self, job_id: &str) {
        let dir = self.claude_jobs_dir.join(job_id);
        std::fs::create_dir_all(&dir).unwrap();
        let body = json!({
            "state": "working",
            "cwd": self.project_dir.to_string_lossy(),
            "firstTerminalAt": null,
            "cost_usd": 0.05,
        });
        std::fs::write(dir.join("state.json"), body.to_string()).unwrap();
    }

    /// Seed an *open* agent_spawn row (no matching agent_done) backed
    /// by a job_id whose state.json reports state=working. F81 refusal
    /// gate fires on this — the orchestrator would clean it up on the
    /// next tick, but the user shouldn't blindly remove with active
    /// session activity in flight.
    fn seed_open_agent_spawn(&self, sid: &str, job_id: &str) {
        // Live state.json for the spawn's job_id.
        let dir = self.claude_jobs_dir.join(job_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("state.json"),
            json!({
                "state": "working",
                "cwd": "/some/other/path",  // unrelated cwd: stays out of branch (2)
                "firstTerminalAt": null,
                "cost_usd": 0.01,
            })
            .to_string(),
        )
        .unwrap();
        let p = self.paths().progress_jsonl(&self.slug);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let line = json!({
            "event": "agent_spawn",
            "slug": self.slug,
            "role": "explorer",
            "session_id": sid,
            "job_id": job_id,
            "ts": "2026-01-01T00:00:00Z",
        });
        std::fs::write(&p, format!("{line}\n")).unwrap();
    }

    fn owned_progress_state_paths(&self) -> Vec<PathBuf> {
        let active = self.paths().progress_jsonl(&self.slug);
        let parent = active.parent().unwrap().to_path_buf();
        let slug = &self.slug;
        let archive = progress_archive_path(&active);
        let active_name = active.file_name().unwrap().to_string_lossy().into_owned();
        let archive_name = archive.file_name().unwrap().to_string_lossy().into_owned();
        let mut paths = vec![
            active,
            archive,
            parent.join(format!("{slug}.checkpoint.json")),
            parent.join(format!("{slug}.checkpoint.json.tmp")),
            parent.join(format!("{slug}.verdicts.json")),
            parent.join(format!("{slug}.verdicts.json.tmp")),
            parent.join(format!("{slug}.verdicts.corrupt.json")),
            parent.join(format!("{slug}.verdicts.corrupt.json.tmp")),
            parent.join(format!("{slug}.terminals.jsonl")),
            parent.join(format!("{slug}.turn-verdicts.jsonl")),
            parent.join(format!("{slug}.terminal-keys")),
            parent.join(format!("{slug}.verdict-keys")),
            parent.join(format!("{active_name}.repair-tmp-fixture")),
            parent.join(format!("{active_name}.bak-fixture")),
            parent.join(format!("{archive_name}.repair-tmp-fixture")),
            parent.join(format!("{archive_name}.bak-fixture")),
        ];
        paths.sort();
        paths
    }

    fn seed_all_owned_progress_state(&self) -> Vec<PathBuf> {
        let active = self.paths().progress_jsonl(&self.slug);
        let old_terminal = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s-old",
            "turn_id": "turn-old",
            "ts": "2026-08-28T00:00:00Z",
            "outcome": "completed",
        });
        append_chat_turn_completed_if_absent(&active, &old_terminal).unwrap();
        append_turn_verdict_if_changed(
            &active,
            &TurnVerdict {
                sid: "s-old".into(),
                turn_id: "turn-old".into(),
                ts: "2026-08-28T00:00:01Z".parse().unwrap(),
                verdict: Verdict::Revise,
                feedback: Some("old lifetime state".into()),
            },
        )
        .unwrap();
        assert_eq!(
            latest_turn_verdicts(&active).unwrap().len(),
            1,
            "fixture must expose the old verdict before removal",
        );
        assert_eq!(
            terminal_turns_for_rebuild(&active).unwrap().len(),
            1,
            "fixture must expose the old terminal turn before removal",
        );

        let paths = self.owned_progress_state_paths();
        for path in &paths {
            if std::fs::symlink_metadata(path).is_ok() {
                continue;
            }
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-keys")
            {
                let receipt = path.join("ab").join("stale.json");
                std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
                std::fs::write(receipt, b"stale receipt").unwrap();
            } else if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("checkpoint.json.tmp")
            {
                // A crashed or hostile writer can leave the expected file
                // path as a directory. Cleanup must still stay inside it.
                std::fs::create_dir_all(path.join("nested")).unwrap();
                std::fs::write(path.join("nested").join("state"), b"stale").unwrap();
            } else {
                std::fs::write(path, b"stale durable state\n").unwrap();
            }
        }
        let enumerated = cleanup_progress_state(&active, true).unwrap();
        assert_eq!(enumerated.len(), paths.len());
        assert!(
            paths.iter().all(|path| enumerated.contains(path)),
            "public cleanup seam omitted seeded state"
        );
        enumerated
    }
}

#[test]
fn t01_remove_dry_run_prints_only() {
    let fx = Fixture::new("dex-ui");
    // Seed orchestration state to confirm dry-run reports them as
    // "would remove" but leaves them on disk.
    let progress = fx.paths().progress_jsonl(&fx.slug);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(&progress, "{}\n").unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--dry-run"])
        .output()
        .expect("spawn ccteam remove --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --dry-run should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run header missing; got: {stdout}",
    );
    assert!(
        stdout.contains("будет удалена запись config.yaml::projects"),
        "missing config-drop preview; got: {stdout}",
    );
    assert!(
        stdout.contains("будет удалён progress.jsonl"),
        "missing progress.jsonl preview; got: {stdout}",
    );
    // Filesystem must be untouched.
    assert!(
        progress.exists(),
        "progress.jsonl was deleted under --dry-run; bug",
    );
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert_eq!(
        cfg.projects.len(),
        1,
        "config.yaml::projects must keep the entry under --dry-run",
    );
}

#[test]
fn t02_remove_basic_drops_config_entry() {
    let fx = Fixture::new("dex-ui");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);
    assert!(progress.exists(), "fixture: progress.jsonl seeded");

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove should succeed when no active sessions; stderr: {stderr}; stdout: {stdout}",
    );
    // Config entry gone.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects still contains `{}`; cfg: {cfg:?}",
        fx.slug,
    );
    // Progress.jsonl gone.
    assert!(
        !progress.exists(),
        "progress.jsonl should be deleted; still at {}",
        progress.display(),
    );
    // Project dir + .ccteam/ untouched (no --purge).
    assert!(
        fx.project_dir.join(".ccteam").is_dir(),
        ".ccteam should survive without --purge",
    );
}

#[test]
fn t19_project_rm_owned_progress_state_dry_run_is_complete_and_non_mutating() {
    let fx = Fixture::new("progress-dry");
    let state_paths = fx.seed_all_owned_progress_state();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--dry-run"])
        .output()
        .expect("spawn ccteam project rm --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "dry-run should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    for path in &state_paths {
        assert!(
            stdout.contains(&path.display().to_string()),
            "dry-run omitted owned progress state {}; stdout: {stdout}",
            path.display(),
        );
        assert!(
            std::fs::symlink_metadata(path).is_ok(),
            "dry-run mutated owned progress state {}",
            path.display(),
        );
    }
}

#[test]
fn t20_project_rm_owned_progress_state_cannot_reuse_retired_slug() {
    let fx = Fixture::new("progress-reinit");
    let state_paths = fx.seed_all_owned_progress_state();
    let active = fx.paths().progress_jsonl(&fx.slug);

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    for path in &state_paths {
        assert!(
            std::fs::symlink_metadata(path).is_err(),
            "owned progress state survived removal: {}; stdout: {stdout}",
            path.display(),
        );
        assert!(
            stdout.contains(&path.display().to_string()),
            "remove report omitted owned progress state {}; stdout: {stdout}",
            path.display(),
        );
    }

    let init = fx
        .cmd()
        .current_dir(&fx.project_dir)
        .args(["init", "--slug", &fx.slug])
        .output()
        .expect("re-init removed slug");
    assert!(
        !init.status.success(),
        "same-slug re-init must fail after permanent retirement; stdout: {}",
        String::from_utf8_lossy(&init.stdout),
    );
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("prior generation is retired"),
        "same-slug rejection must explain the durable retirement; stderr: {}",
        String::from_utf8_lossy(&init.stderr),
    );
    assert_eq!(
        ccteam_core::pick_unused_project_slug(&fx.ccteam_home, &fx.slug).unwrap(),
        "progress-reinit2",
        "automatic allocation must move to a fresh generation slug"
    );
    assert!(!active.exists(), "retired progress body must stay deleted");
}

#[test]
fn t03_purge_clears_ccteam_footprint_only() {
    // `--purge` deletes exactly ccteam's footprint (.ccteam/ + ccteam hooks
    // in settings.local.json) and leaves every user role, root workflow.yaml,
    // CLAUDE.md, .env, settings.json, and business code in place.
    let fx = Fixture::new("dex-ui");
    fx.seed_closed_progress();
    // .ccteam/ already exists from the fixture; drop a state.json inside
    // so we can confirm the whole dir goes.
    std::fs::write(
        fx.project_dir.join(".ccteam").join("workflow.yaml"),
        "name: x\n",
    )
    .unwrap();
    // KEEP set:
    std::fs::write(fx.project_dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(fx.project_dir.join("README.md"), "# real code\n").unwrap();
    std::fs::write(fx.project_dir.join("CLAUDE.md"), "# project memory\n").unwrap();
    // A root-level workflow.yaml is NOT ccteam's footprint post-W2 (it
    // lives under .ccteam/) — must survive.
    std::fs::write(fx.project_dir.join("workflow.yaml"), "user: stuff\n").unwrap();
    // User's committed settings.json — ccteam never touches it.
    std::fs::write(
        fx.project_dir.join(".claude").join("settings.json"),
        "{\"permissions\":{}}\n",
    )
    .unwrap();
    // KEEP set: v0.9.0 seeds no roles, so cto.md and reviewer.md are both
    // user-owned files.
    let agents = fx.project_dir.join(".claude").join("agents");
    std::fs::write(agents.join("cto.md"), "---\n---\nuser cto").unwrap();
    std::fs::write(agents.join("reviewer.md"), "---\n---\nuser role").unwrap();

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug, "--purge"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // .ccteam/ gone (covers .ccteam/workflow.yaml too).
    assert!(
        !fx.project_dir.join(".ccteam").exists(),
        ".ccteam/ should be purged",
    );
    // Every role file survives, including a user-authored cto.md.
    assert!(
        agents.join("cto.md").exists(),
        "user-owned .claude/agents/cto.md must survive --purge",
    );
    assert!(
        agents.join("reviewer.md").exists(),
        "user work-role .claude/agents/reviewer.md must survive --purge",
    );
    assert!(
        agents.is_dir(),
        ".claude/agents/ dir must survive (all role files are user-owned)",
    );
    // root workflow.yaml is NOT ccteam's footprint post-W2 — must survive.
    assert!(
        fx.project_dir.join("workflow.yaml").exists(),
        "root workflow.yaml must survive --purge (W2: ccteam's lives under .ccteam/)",
    );
    // .env preserved — CLAUDE.md §三 red line.
    assert!(
        fx.project_dir.join(".env").exists(),
        ".env must NEVER be deleted; still expected at {}",
        fx.project_dir.join(".env").display(),
    );
    // CLAUDE.md + business code preserved.
    assert!(
        fx.project_dir.join("CLAUDE.md").exists(),
        "project CLAUDE.md must survive --purge",
    );
    assert!(
        fx.project_dir.join("README.md").exists(),
        "business code must survive --purge",
    );
    // User's committed settings.json untouched.
    assert!(
        fx.project_dir
            .join(".claude")
            .join("settings.json")
            .exists(),
        "user settings.json must NEVER be touched by ccteam",
    );
}

#[test]
fn t03b_purge_strips_chat_hooks_surgically_keeps_other_keys() {
    // settings.local.json holds ccteam chat hooks + an operator key.
    // --purge must strip only the ccteam hooks and keep the rest (file
    // survives because it still has a non-ccteam key).
    let fx = Fixture::new("dex-hooks");
    fx.seed_closed_progress();
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "permissions": {"allow": ["Bash"]},
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}],
    "PreToolUse": [
      {"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress pre-tool-use"}]},
      {"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": "/h/hook.sh intercept-ask", "timeout": 660}]},
      {"matcher": "Edit", "hooks": [{"type": "command", "command": "/h/my-own-linter.sh"}]}
    ]
  }
}
"#,
    )
    .unwrap();

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug, "--purge"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    assert!(
        settings_local.exists(),
        "settings.local.json must survive (operator key + own hook remain)",
    );
    let body = std::fs::read_to_string(&settings_local).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Operator key preserved.
    assert!(
        v.get("permissions").is_some(),
        "operator `permissions` key must survive; got: {body}",
    );
    // ccteam chat hooks gone.
    assert!(
        !body.contains("chat-progress") && !body.contains("intercept-ask"),
        "ccteam chat hooks must be stripped; got: {body}",
    );
    // SessionStart event array (only had ccteam's hook) pruned entirely.
    assert!(
        v.get("hooks").and_then(|h| h.get("SessionStart")).is_none(),
        "emptied SessionStart event must be pruned; got: {body}",
    );
    // Operator's own PreToolUse Edit hook preserved.
    assert!(
        body.contains("my-own-linter.sh"),
        "operator's own PreToolUse hook must survive; got: {body}",
    );
}

#[test]
fn t03b2_purge_strips_hitl_permission_request_hook() {
    // v0.8.7 review-fix (R-M4) — a hitl-spawned session installs the
    // `PermissionRequest` hook (`{hook_sh} permission-request`). `project rm
    // --purge` is the `init` inverse and MUST clear it (red line), or the
    // deregistered project keeps a live HITL approval gate. End-to-end through
    // the real `ccteam project rm --purge` binary, not just the predicate.
    let fx = Fixture::new("dex-hitl");
    fx.seed_closed_progress();
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "permissions": {"allow": ["Bash"]},
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}],
    "PermissionRequest": [{"hooks": [{"type": "command", "command": "/h/hook.sh permission-request"}]}]
  }
}
"#,
    )
    .unwrap();

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug, "--purge"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // The file survives (operator `permissions` key remains) but the HITL hook
    // and its now-empty PermissionRequest section are gone.
    assert!(
        settings_local.exists(),
        "settings.local.json survives (operator key remains)",
    );
    let body = std::fs::read_to_string(&settings_local).unwrap();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v.get("permissions").is_some(), "operator key kept: {body}");
    assert!(
        !body.contains("permission-request"),
        "HITL PermissionRequest hook must be purged (R-M4); got: {body}",
    );
    assert!(
        v.get("hooks")
            .and_then(|h| h.get("PermissionRequest"))
            .is_none(),
        "emptied PermissionRequest section must be pruned; got: {body}",
    );
}

#[test]
fn t03c_purge_deletes_settings_local_when_it_collapses_to_empty() {
    // settings.local.json that holds ONLY ccteam chat hooks is the file
    // ccteam created — after stripping it collapses to {} and --purge
    // deletes the vestigial file.
    let fx = Fixture::new("dex-empty");
    fx.seed_closed_progress();
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}],
    "Stop": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress stop"}]}]
  }
}
"#,
    )
    .unwrap();

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug, "--purge"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    assert!(
        !settings_local.exists(),
        "settings.local.json that held only ccteam hooks should be deleted; stdout: {stdout}",
    );
    assert!(
        stdout.contains("removed now-empty"),
        "step list must report the now-empty file deletion; got: {stdout}",
    );
}

/// t12 — the grouped `ccteam project rm` is the same engine as the flat
/// `ccteam remove`: it drops the config entry and (with --purge) clears
/// the footprint.
#[test]
fn t12_project_rm_alias_drops_config_entry() {
    let fx = Fixture::new("dex-grp");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project rm should succeed; stderr: {stderr}; stdout: {stdout}",
    );
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects still contains `{}` after project rm",
        fx.slug,
    );
    assert!(
        !progress.exists(),
        "progress.jsonl should be deleted by project rm",
    );
}

/// t13 — `ccteam project rm --dry-run` acts on nothing.
#[test]
fn t13_project_rm_dry_run_acts_on_nothing() {
    let fx = Fixture::new("dex-grp-dry");
    let progress = fx.paths().progress_jsonl(&fx.slug);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(&progress, "{}\n").unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--dry-run"])
        .output()
        .expect("spawn ccteam project rm --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "project rm --dry-run should succeed");
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run header missing; got: {stdout}"
    );
    assert!(progress.exists(), "dry-run must not delete progress.jsonl");
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert_eq!(cfg.projects.len(), 1, "dry-run must keep config entry");
}

/// t14 — `ccteam project stop <slug>` with no live sessions succeeds and
/// reports zero stopped (stop is not an error when nothing is running).
/// This is the tmux-free baseline; t15 proves the actual kill path.
#[test]
fn t14_project_stop_no_sessions_is_ok() {
    let fx = Fixture::new("dex-stop");
    let out = fx
        .cmd()
        .args(["project", "stop", &fx.slug])
        .output()
        .expect("spawn ccteam project stop");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project stop must succeed even with no live sessions; stderr: {stderr}",
    );
    assert!(
        stdout.contains("остановлено чат-сессий: 0"),
        "stop must report zero sessions stopped; got: {stdout}",
    );
}

/// t15 — `ccteam project stop <slug>` kills the matching
/// `ccteam-chat-<slug>-*` tmux sessions and leaves a sibling project's
/// session (a slug that is a prefix of ours, to prove the dash-aware
/// parse) untouched. Guarded by tmux availability.
#[test]
fn t15_project_stop_kills_matching_chat_sessions() {
    use ccteam_harness::chat_session_name;
    use ccteam_harness::tmux_ops::{tmux_available, TmuxSession};

    if !tmux_available() {
        eprintln!("skipping t15: tmux not available");
        return;
    }

    // Pick a slug whose dash structure would alias a sibling under a
    // naive `starts_with`: `dex-stop` vs the sibling `dex`. The CLI must
    // stop only `dex-stop`'s sessions and leave `dex`'s alone.
    let suffix = std::process::id();
    let slug = format!("dexstop-{suffix}");
    let sibling = format!("dexstop-{suffix}-extra"); // longer slug, NOT ours
    let fx = Fixture::new(&slug);

    let ours_a = chat_session_name(&slug, "cto");
    let ours_b = chat_session_name(&slug, "reviewer");
    // sibling's role session — note its (slug, role) parses to
    // (`dexstop-<id>-extra`, `bob`), a DIFFERENT slug than ours.
    let sib = chat_session_name(&sibling, "bob");

    // Best-effort pre-clean in case a prior crashed run left them.
    for name in [&ours_a, &ours_b, &sib] {
        TmuxSession::from_name(name.clone()).kill().ok();
    }

    // Create three live detached sessions running a long-lived `sleep`.
    for name in [&ours_a, &ours_b, &sib] {
        let status = std::process::Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "sleep", "300"])
            .status()
            .expect("tmux new-session");
        assert!(status.success(), "pre-create tmux session {name} failed");
    }
    assert!(TmuxSession::from_name(ours_a.clone()).exists());
    assert!(TmuxSession::from_name(ours_b.clone()).exists());
    assert!(TmuxSession::from_name(sib.clone()).exists());

    let out = fx
        .cmd()
        // This test seeds + asserts on real `tmux` sessions, so pin the CLI to
        // the tmux backend (the default is now `rmux`, which wouldn't see them).
        .env("CCTEAM_MUX_BACKEND", "tmux")
        .args(["project", "stop", &slug])
        .output()
        .expect("spawn ccteam project stop");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Always clean the sibling so we don't leak it past the assertions.
    let sib_alive_after = TmuxSession::from_name(sib.clone()).exists();
    TmuxSession::from_name(sib.clone()).kill().ok();

    assert!(
        out.status.success(),
        "project stop must succeed; stderr: {stderr}; stdout: {stdout}",
    );
    assert!(
        !TmuxSession::from_name(ours_a.clone()).exists(),
        "our chat session {ours_a} must be killed; stdout: {stdout}",
    );
    assert!(
        !TmuxSession::from_name(ours_b.clone()).exists(),
        "our chat session {ours_b} must be killed; stdout: {stdout}",
    );
    assert!(
        sib_alive_after,
        "sibling slug's session {sib} must NOT be killed (dash-aware slug match)",
    );
    assert!(
        stdout.contains("остановлено чат-сессий: 2"),
        "stop must report two sessions stopped; got: {stdout}",
    );
}

/// t16 — `ccteam project rm` (non-purge) drops the registry + ~/.ccteam
/// per-slug state but PROVABLY keeps the project's on-disk files
/// (.ccteam/, seeded cto.md, user role, CLAUDE.md, .env, settings.json).
#[test]
fn t16_project_rm_nonpurge_keeps_project_files() {
    let fx = Fixture::new("dex-keep");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);
    // Seed the ccteam footprint + user content that non-purge must keep.
    std::fs::write(
        fx.project_dir.join(".ccteam").join("workflow.yaml"),
        "name: x\n",
    )
    .unwrap();
    std::fs::write(fx.project_dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(fx.project_dir.join("CLAUDE.md"), "# memory\n").unwrap();
    let agents = fx.project_dir.join(".claude").join("agents");
    std::fs::write(agents.join("cto.md"), "---\n---\nseeded cto").unwrap();
    std::fs::write(agents.join("reviewer.md"), "---\n---\nuser role").unwrap();
    std::fs::write(
        fx.project_dir.join(".claude").join("settings.json"),
        "{\"permissions\":{}}\n",
    )
    .unwrap();

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project rm should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Registry + ~/.ccteam state gone …
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects must drop the slug under project rm",
    );
    assert!(
        !progress.exists(),
        "progress.jsonl should be deleted by project rm",
    );
    // … but EVERY project-dir file survives (non-purge keeps them all).
    assert!(
        fx.project_dir.join(".ccteam").is_dir(),
        ".ccteam/ must survive without --purge",
    );
    assert!(
        agents.join("cto.md").exists(),
        "seeded cto.md must survive without --purge",
    );
    assert!(
        agents.join("reviewer.md").exists(),
        "user role must survive without --purge",
    );
    assert!(
        fx.project_dir.join(".env").exists(),
        ".env must survive without --purge",
    );
    assert!(
        fx.project_dir.join("CLAUDE.md").exists(),
        "CLAUDE.md must survive without --purge",
    );
    assert!(
        fx.project_dir
            .join(".claude")
            .join("settings.json")
            .exists(),
        "user settings.json must survive without --purge",
    );
}

/// t17 — `ccteam project rm --purge` deletes ccteam's footprint via the
/// GROUP path (.ccteam/ + settings.local.json hook section +
/// config entry + ~/.ccteam/{progress,state/im/registry}/<slug>) and PROVABLY
/// keeps all user roles, CLAUDE.md, .env, and the user's settings.json.
#[test]
fn t17_project_rm_purge_via_group() {
    let fx = Fixture::new("dex-grp-purge");
    fx.seed_closed_progress();
    // state/im/registry/<slug>/ — must be purged.
    let (reg, hb) = seed_imd_registry(&fx, "helper");
    let slug_dir = ccteam_im::registry_root_in(&fx.ccteam_home).join(&fx.slug);
    // ccteam footprint.
    std::fs::write(
        fx.project_dir.join(".ccteam").join("workflow.yaml"),
        "name: x\n",
    )
    .unwrap();
    let agents = fx.project_dir.join(".claude").join("agents");
    // KEEP set: v0.9.0 seeds no cto role; both files are user-owned.
    std::fs::write(agents.join("cto.md"), "---\n---\nuser cto").unwrap();
    std::fs::write(agents.join("reviewer.md"), "---\n---\nuser role").unwrap();
    std::fs::write(fx.project_dir.join(".env"), "SECRET=hunter2\n").unwrap();
    std::fs::write(fx.project_dir.join("CLAUDE.md"), "# memory\n").unwrap();
    std::fs::write(
        fx.project_dir.join(".claude").join("settings.json"),
        "{\"permissions\":{}}\n",
    )
    .unwrap();
    // settings.local.json with only ccteam hooks → collapses + deleted.
    let settings_local = fx.project_dir.join(".claude").join("settings.local.json");
    std::fs::write(
        &settings_local,
        r#"{
  "hooks": {
    "SessionStart": [{"matcher": "*", "hooks": [{"type": "command", "command": "/h/hook.sh chat-progress session-start"}]}]
  }
}
"#,
    )
    .unwrap();

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug, "--purge"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "project rm --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // DELETE set.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config entry must be dropped",
    );
    assert!(
        !fx.project_dir.join(".ccteam").exists(),
        ".ccteam/ must be purged",
    );
    assert!(
        !settings_local.exists(),
        "settings.local.json (only ccteam hooks) must be deleted",
    );
    assert!(
        !reg.exists(),
        "state/im/registry/<slug>/helper.json must be purged"
    );
    assert!(!hb.exists(), "state/im/registry heartbeat must be purged");
    assert!(
        !slug_dir.exists(),
        "state/im/registry/<slug>/ dir must be purged"
    );
    assert!(
        !fx.paths().progress_jsonl(&fx.slug).exists(),
        "progress.jsonl must be purged",
    );

    // KEEP set — provably untouched.
    assert!(
        agents.join("cto.md").exists(),
        "user-owned .claude/agents/cto.md must survive --purge",
    );
    assert!(
        agents.join("reviewer.md").exists(),
        "user role .claude/agents/reviewer.md must survive --purge",
    );
    assert!(agents.is_dir(), ".claude/agents/ dir must survive");
    assert!(
        fx.project_dir.join(".env").exists(),
        ".env must NEVER be deleted",
    );
    assert!(
        fx.project_dir.join("CLAUDE.md").exists(),
        "CLAUDE.md must survive --purge",
    );
    assert!(
        fx.project_dir
            .join(".claude")
            .join("settings.json")
            .exists(),
        "user settings.json must NEVER be touched",
    );
}

/// t18 — `ccteam project rm --dry-run` with a live chat session lists the
/// session it WOULD stop and the config/state it WOULD drop, and changes
/// nothing on disk or tmux. Guarded by tmux availability.
#[test]
fn t18_project_rm_dry_run_lists_stop_and_acts_on_nothing() {
    use ccteam_harness::chat_session_name;
    use ccteam_harness::tmux_ops::{tmux_available, TmuxSession};

    if !tmux_available() {
        eprintln!("skipping t18: tmux not available");
        return;
    }
    let slug = format!("dexdry-{}", std::process::id());
    let fx = Fixture::new(&slug);
    let progress = fx.paths().progress_jsonl(&slug);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(&progress, "{}\n").unwrap();

    let sess = chat_session_name(&slug, "cto");
    TmuxSession::from_name(sess.clone()).kill().ok();
    let status = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", &sess, "sleep", "300"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "pre-create tmux session failed");

    let out = fx
        .cmd()
        // Seeds + asserts on a real `tmux` session, so pin the CLI to the tmux
        // backend (the default is now `rmux`, which wouldn't enumerate it).
        .env("CCTEAM_MUX_BACKEND", "tmux")
        .args(["project", "rm", &slug, "--dry-run"])
        .output()
        .expect("spawn ccteam project rm --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let still_alive = TmuxSession::from_name(sess.clone()).exists();
    TmuxSession::from_name(sess.clone()).kill().ok(); // cleanup regardless

    assert!(out.status.success(), "dry-run rm should succeed");
    assert!(
        stdout.contains(&format!("будет остановлена чат-сессия `{sess}`")),
        "dry-run must list the chat session it would stop; got: {stdout}",
    );
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run header missing; got: {stdout}",
    );
    // Nothing acted on.
    assert!(still_alive, "dry-run must NOT kill the chat session");
    assert!(progress.exists(), "dry-run must not delete progress.jsonl");
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert_eq!(cfg.projects.len(), 1, "dry-run must keep config entry");
}

#[test]
fn t04_refuses_with_active_tmux() {
    // tmux liveness on CI may or may not be mockable. We force the
    // tmux path by *not* having tmux at all (`tmux has-session` exits
    // non-zero → exists() == false) AND seed an *open* agent_spawn
    // so the third refusal arm fires regardless of tmux availability.
    // This exercises the refusal-message rendering + bail exit shape
    // even when the host has no live tmux server.
    //
    // (A pure tmux test would require a real tmux daemon; we trade
    // coverage of arm 1 for portability.)
    let fx = Fixture::new("dex-ui");
    fx.seed_open_agent_spawn("sid-1", "abc12345");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "remove must refuse with running agent_spawn; stderr: {stderr}",
    );
    assert!(
        stderr.contains("refusing"),
        "stderr must explain refusal; got: {stderr}",
    );
    // Refusal cites either the agent-spawn arm or tmux arm; both
    // mention `--force` as the override knob.
    assert!(
        stderr.contains("--force"),
        "refusal must hint at --force override; got: {stderr}",
    );
    // Config entry must still be present (no mutation on refusal).
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().any(|p| p.slug == fx.slug),
        "config.yaml::projects must keep the slug when refused",
    );
}

#[test]
fn t05_refuses_with_running_claude_bg() {
    let fx = Fixture::new("dex-ui");
    fx.seed_live_claude_bg("deadbeef");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "remove must refuse when a claude --bg job points at the project; stderr: {stderr}",
    );
    assert!(
        stderr.contains("claude --bg job") || stderr.contains("refusing"),
        "refusal message must cite the claude bg branch; got: {stderr}",
    );
    // Config entry intact.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().any(|p| p.slug == fx.slug),
        "config.yaml::projects must keep the slug when refused",
    );
}

const TEST_ADMIN_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[cfg(unix)]
/// Fake `ccteam/project-retire` daemon.
///
/// `response_error` turns the reply into a JSON-RPC error;
/// `error_marker_committed` (when `Some`) attaches the wire's
/// `error.data.marker_committed` flag, i.e. whether the daemon had already
/// written the durable tombstone before it failed.
///
/// The success path mints the tombstone for **the slug the request names**, not
/// for the fixture's slug — that is what makes an unregistered-slug request an
/// irreversible burn, so a test can prove the CLI never sends one.
fn seed_retire_daemon(
    fx: &Fixture,
    response_error: Option<&'static str>,
    error_marker_committed: Option<bool>,
) -> std::thread::JoinHandle<Option<serde_json::Value>> {
    std::fs::create_dir_all(fx.paths().web_token_path().parent().unwrap()).unwrap();
    std::fs::write(fx.paths().web_token_path(), TEST_ADMIN_TOKEN).unwrap();
    let socket = ccteam_core::daemon_socket_path(&fx.paths());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let paths = fx.paths();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("accept fake retirement request: {error}"),
            }
        };
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let slug = request
            .pointer("/params/arguments/slug")
            .and_then(serde_json::Value::as_str)
            .expect("retirement request carries a slug")
            .to_string();
        let progress = paths.progress_jsonl(&slug);
        let response = if let Some(message) = response_error {
            let mut error = json!({ "code": -32000, "message": message });
            if let Some(committed) = error_marker_committed {
                error["data"] = json!({ "slug": slug, "marker_committed": committed });
            }
            json!({
                "jsonrpc": "2.0",
                "id": request.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "error": error,
            })
        } else {
            ccteam_harness::execution::progress_bridge::mark_progress_retired(&progress).unwrap();
            let removed =
                ccteam_harness::execution::progress_bridge::cleanup_retired_progress_state(
                    &progress, false,
                )
                .unwrap()
                .into_iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            json!({
                "jsonrpc": "2.0",
                "id": request.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "result": {
                    "slug": slug,
                    "sessions_stopped": ["s-test"],
                    "progress_removed": removed,
                },
            })
        };
        let mut stream = stream;
        writeln!(stream, "{}", serde_json::to_string(&response).unwrap()).unwrap();
        Some(request)
    })
}

#[cfg(unix)]
fn run_remove_with_retire_daemon(fx: &Fixture, args: &[&str]) -> std::process::Output {
    let daemon = seed_retire_daemon(fx, None, None);
    let output = fx
        .cmd()
        .args(args)
        .output()
        .expect("spawn ccteam project rm");
    let request = daemon
        .join()
        .expect("join fake retirement daemon")
        .unwrap_or_else(|| {
            panic!(
                "project rm never contacted daemon; status={}; stderr={}; stdout={}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            )
        });
    assert_eq!(
        request.get("method").and_then(serde_json::Value::as_str),
        Some("ccteam/project-retire"),
        "mutating project rm must use the daemon retirement spine"
    );
    output
}

/// t07: the CLI requires a truthful daemon retirement acknowledgement before
/// it drops the registry row.
#[test]
#[cfg(unix)]
fn t07_remove_requires_daemon_retire_ack() {
    let fx = Fixture::new("dex-ui-t07");
    fx.seed_closed_progress();
    let daemon = seed_retire_daemon(&fx, None, None);

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove should succeed with daemon alive; stderr: {stderr}; stdout: {stdout}",
    );
    let request = daemon.join().unwrap().expect("retirement request");
    assert_eq!(
        request
            .pointer("/method")
            .and_then(serde_json::Value::as_str),
        Some("ccteam/project-retire")
    );
    assert_eq!(
        request
            .pointer("/params/arguments/slug")
            .and_then(serde_json::Value::as_str),
        Some(fx.slug.as_str())
    );
    assert_eq!(
        request
            .pointer("/params/arguments/_caller_admin_token")
            .and_then(serde_json::Value::as_str),
        Some(TEST_ADMIN_TOKEN)
    );
    assert!(
        stdout.contains("подтверждено демоном") && stdout.contains("s-test"),
        "daemon acknowledgement and stopped sid must be truthful; got: {stdout}",
    );
    assert!(
        config::load(&fx.ccteam_home)
            .unwrap()
            .projects
            .iter()
            .all(|entry| entry.slug != fx.slug),
        "config row must be removed only after ACK"
    );
}

/// t08: a daemon-side retirement failure is fatal and leaves the registry and
/// progress generation untouched for a safe retry.
#[test]
#[cfg(unix)]
fn t08_remove_fails_closed_when_daemon_rejects_retirement() {
    let fx = Fixture::new("dex-ui-t08");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);
    let daemon = seed_retire_daemon(&fx, Some("retirement drain failed"), None);

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "daemon rejection must fail the remove; stderr: {stderr}; stdout: {stdout}",
    );
    let _request = daemon.join().unwrap().expect("retirement request");
    assert!(
        stderr.contains("retirement drain failed"),
        "daemon error must be surfaced verbatim; got: {stderr}",
    );
    assert!(
        config::load(&fx.ccteam_home)
            .unwrap()
            .projects
            .iter()
            .any(|entry| entry.slug == fx.slug),
        "config row must survive a missing ACK"
    );
    assert!(
        progress.exists(),
        "progress must survive a rejected retirement"
    );
}

/// t21: an unregistered slug is refused locally. The daemon mints an
/// irreversible tombstone for whatever slug it is handed, so a typo must never
/// reach it — not even under `--dry-run`.
#[test]
#[cfg(unix)]
fn t21_remove_refuses_unregistered_slug_before_contacting_daemon() {
    let fx = Fixture::new("dex-ui-t21");
    fx.seed_closed_progress();
    let typo = "dex-ui-t21-typo";
    let daemon = seed_retire_daemon(&fx, None, None);

    let dry = fx
        .cmd()
        .args(["project", "rm", typo, "--dry-run"])
        .output()
        .expect("spawn ccteam project rm --dry-run");
    assert!(
        !dry.status.success(),
        "--dry-run on an unregistered slug must refuse; stdout: {}",
        String::from_utf8_lossy(&dry.stdout)
    );
    assert!(
        String::from_utf8_lossy(&dry.stderr).contains("не зарегистрирован"),
        "refusal must name the cause; stderr: {}",
        String::from_utf8_lossy(&dry.stderr)
    );

    let out = fx
        .cmd()
        .args(["project", "rm", typo])
        .output()
        .expect("spawn ccteam project rm");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "unregistered slug must exit non-zero; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("не зарегистрирован"),
        "refusal must name the cause; stderr: {stderr}",
    );

    // The burn we are preventing: no tombstone inode for the typo.
    let burned = fx.paths().progress_dir().join(format!("{typo}.lock"));
    assert!(
        !burned.exists(),
        "a typo must not reserve a progress generation: {}",
        burned.display()
    );
    // And the daemon was never asked in the first place.
    assert!(
        daemon.join().expect("join fake daemon").is_none(),
        "an unregistered slug must never reach the daemon retirement spine"
    );
    // The real project is untouched.
    assert!(
        config::load(&fx.ccteam_home)
            .unwrap()
            .projects
            .iter()
            .any(|entry| entry.slug == fx.slug),
        "config must be untouched by a refused removal"
    );
}

/// t22: a daemon failure that already committed the durable tombstone must be
/// reported as such — telling the user the project survived would be a lie that
/// hides a permanently retired generation.
#[test]
#[cfg(unix)]
fn t22_remove_reports_committed_marker_from_daemon_error_data() {
    let fx = Fixture::new("dex-ui-t22");
    fx.seed_closed_progress();
    let daemon = seed_retire_daemon(&fx, Some("session teardown failed"), Some(true));

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam project rm");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a failed retirement must exit non-zero"
    );
    let _request = daemon.join().unwrap().expect("retirement request");
    assert!(
        stderr.contains("необратимо retired"),
        "committed marker must be reported as permanent; stderr: {stderr}",
    );
    assert!(
        stderr.contains("повторите"),
        "the user must be told to rerun the command; stderr: {stderr}",
    );
    assert!(
        stderr.contains("session teardown failed"),
        "the daemon cause must be surfaced; stderr: {stderr}",
    );
    // Retrying is the documented fix, so the row must still be there.
    assert!(
        config::load(&fx.ccteam_home)
            .unwrap()
            .projects
            .iter()
            .any(|entry| entry.slug == fx.slug),
        "config row must survive so the retry has something to remove"
    );
}

/// t23: the legacy mux sweep runs AFTER the irreversible daemon ACK, so a dead
/// mux control plane must not strand the registry row on an already-retired
/// generation.
#[test]
#[cfg(unix)]
fn t23_remove_survives_legacy_mux_failure_after_daemon_ack() {
    let fx = Fixture::new("dex-ui-t23");
    fx.seed_closed_progress();
    // The rmux control socket lives at `$HOME/.ccteam/run/mux.sock`; a regular
    // file at `$HOME/.ccteam` makes creating that parent fail with ENOTDIR, so
    // every backend call errors deterministically (no mux binary needed).
    std::fs::write(fx._tmp.path().join(".ccteam"), b"not a directory").unwrap();

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a dead mux must not fail a removal the daemon already committed; stderr: {stderr}",
    );
    assert!(
        stdout.contains("legacy чат-сессии: не удалось проверить"),
        "the mux failure must be reported, not swallowed; stdout: {stdout}",
    );
    assert!(
        config::load(&fx.ccteam_home)
            .unwrap()
            .projects
            .iter()
            .all(|entry| entry.slug != fx.slug),
        "config row must still be dropped after the irreversible ACK"
    );
}

#[test]
#[cfg(unix)]
fn t08b_remove_fails_closed_when_daemon_is_offline() {
    let fx = Fixture::new("dex-ui-t08b");
    fx.seed_closed_progress();
    let progress = fx.paths().progress_jsonl(&fx.slug);
    std::fs::create_dir_all(fx.paths().web_token_path().parent().unwrap()).unwrap();
    std::fs::write(fx.paths().web_token_path(), TEST_ADMIN_TOKEN).unwrap();

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug])
        .output()
        .expect("spawn ccteam remove");
    assert!(!out.status.success(), "offline removal must fail closed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("config не изменён"),
        "error must state the commit point; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        config::load(&fx.ccteam_home)
            .unwrap()
            .projects
            .iter()
            .any(|entry| entry.slug == fx.slug),
        "offline removal must keep config"
    );
    assert!(
        progress.exists(),
        "offline removal must keep progress state"
    );
}

#[test]
fn t06_force_overrides_refusal() {
    let fx = Fixture::new("dex-ui");
    fx.seed_live_claude_bg("deadbeef");

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug, "--force"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "--force must bypass refusal; stderr: {stderr}; stdout: {stdout}",
    );
    // Should print the "forced through guard" notice.
    assert!(
        stdout.contains("защита принудительно пройдена"),
        "force should still report the guard it bypassed; got: {stdout}",
    );
    // Config entry gone.
    let cfg = config::load(&fx.ccteam_home).unwrap();
    assert!(
        cfg.projects.iter().all(|p| p.slug != fx.slug),
        "config.yaml::projects must drop the slug under --force",
    );
}

// ─────────────────────────── V0.6.5 F151 ────────────────────────────
//
// `ccteam remove <slug> --purge` must also clean
// `~/.ccteam/state/im/registry/<slug>/` (registration JSON + heartbeat
// sidecars). Without `--purge` the registry stays put so a re-init
// of the same slug can resume the existing chat bots.

/// Seed an state/im/registry/<slug>/<role>.json + matching heartbeat
/// sidecar — mirrors the on-disk shape F146's `register_bot_checked_in`
/// produces.
fn seed_imd_registry(fx: &Fixture, role: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    use ccteam_harness::AgentVendor;
    let outcome = ccteam_im::register_bot_checked_in(
        &fx.ccteam_home,
        &fx.slug,
        role,
        AgentVendor::Claude,
        "telegram",
        "42",
        Some(role),
        None,
        None,
    )
    .expect("seed registry");
    let reg_path = match outcome {
        ccteam_im::RegisterOutcome::Registered(p) => p,
        ccteam_im::RegisterOutcome::AlreadyRegistered(p) => p,
    };
    // Drop a heartbeat sidecar so we can assert it gets cleaned too.
    let hb = ccteam_im::bot_heartbeat_path_in(&fx.ccteam_home, &fx.slug, role);
    std::fs::write(&hb, chrono::Utc::now().to_rfc3339()).unwrap();
    assert!(reg_path.exists(), "fixture: registration JSON seeded");
    assert!(hb.exists(), "fixture: heartbeat seeded");
    (reg_path, hb)
}

#[test]
fn t09_purge_cleans_imd_registry_dir() {
    let fx = Fixture::new("dex-bot");
    fx.seed_closed_progress();
    // Seed two roles under the slug — proves the per-role unregister
    // loop + final rm -rf both work.
    let (reg_a, hb_a) = seed_imd_registry(&fx, "helper");
    let (reg_b, hb_b) = seed_imd_registry(&fx, "critic");
    let slug_dir = ccteam_im::registry_root_in(&fx.ccteam_home).join(&fx.slug);
    assert!(
        slug_dir.is_dir(),
        "fixture: state/im/registry/<slug>/ seeded"
    );

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug, "--purge"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Every role file + heartbeat gone.
    assert!(
        !reg_a.exists(),
        "state/im/registry/<slug>/helper.json should be purged"
    );
    assert!(
        !reg_b.exists(),
        "state/im/registry/<slug>/critic.json should be purged"
    );
    assert!(
        !hb_a.exists(),
        "state/im/registry/<slug>/helper.heartbeat should be purged"
    );
    assert!(
        !hb_b.exists(),
        "state/im/registry/<slug>/critic.heartbeat should be purged"
    );
    // The slug dir itself gone.
    assert!(
        !slug_dir.exists(),
        "state/im/registry/<slug>/ dir should be purged; still at {}",
        slug_dir.display()
    );
    // Progress log mentions the purge so the user can see what happened.
    assert!(
        stdout.contains("state/im/registry/dex-bot/"),
        "purge step must be reported; got: {stdout}",
    );
}

#[test]
fn t10_remove_without_purge_keeps_imd_registry() {
    let fx = Fixture::new("dex-bot");
    fx.seed_closed_progress();
    let (reg_path, hb_path) = seed_imd_registry(&fx, "helper");
    let slug_dir = ccteam_im::registry_root_in(&fx.ccteam_home).join(&fx.slug);

    let out = run_remove_with_retire_daemon(&fx, &["project", "rm", &fx.slug]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "remove (no --purge) should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Without --purge the state/im/registry/<slug>/ tree must survive so a
    // re-`ccteam init` of the same slug picks up where it left off.
    assert!(
        reg_path.exists(),
        "state/im/registry/<slug>/helper.json must survive without --purge",
    );
    assert!(
        hb_path.exists(),
        "state/im/registry/<slug>/helper.heartbeat must survive without --purge",
    );
    assert!(
        slug_dir.is_dir(),
        "state/im/registry/<slug>/ must survive without --purge",
    );
    // Step list must NOT mention the state/im/registry purge step.
    assert!(
        !stdout.contains("state/im/registry/"),
        "non-purge run must not touch state/im/registry/; got: {stdout}",
    );
}

#[test]
fn t11_purge_dry_run_reports_imd_registry_count() {
    let fx = Fixture::new("dex-bot");
    fx.seed_closed_progress();
    let (reg_path, hb_path) = seed_imd_registry(&fx, "helper");

    let out = fx
        .cmd()
        .args(["project", "rm", &fx.slug, "--purge", "--dry-run"])
        .output()
        .expect("spawn ccteam remove --purge --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "dry-run --purge should succeed; stderr: {stderr}; stdout: {stdout}",
    );

    // Filesystem untouched under --dry-run.
    assert!(
        reg_path.exists(),
        "dry-run must not delete registration JSON"
    );
    assert!(
        hb_path.exists(),
        "dry-run must not delete heartbeat sidecar"
    );
    // PRD §F151 acceptance #1 — output names the dir + JSON count.
    assert!(
        stdout.contains("would purge state/im/registry/dex-bot/") && stdout.contains("1 JSON file"),
        "dry-run must preview state/im/registry/<slug>/ with count; got: {stdout}",
    );
}
