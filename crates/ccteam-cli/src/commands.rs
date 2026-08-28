//! Command handlers for `ccteam {init, new, ls, show, attach, peek,
//! progress, doctor, config, role, skill, ...}`. Pure where possible (`run_ls` /
//! `run_show` return the formatted string instead of printing) so unit
//! tests don't need a real terminal or running daemon.

use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use ccteam_core::tmux::TmuxSession;
use ccteam_core::{
    cost_summary, current_ccteam_bin, session_name_for_project, CcteamPaths, ProjectState,
};

// V0.3 M5.1 — `ProjectSummary` / `collect_projects` /
// `collect_recent_events` live in `ccteam_core::queries` so the web crate
// can reuse them without depending on `ccteam-cli`. Re-exported here so
// existing call sites (`run_ls`, `run_progress`, `mcp_serve.rs`) keep
// their `use ccteam_cli::commands::{collect_projects, ...}` lines.
pub use ccteam_core::{collect_projects, collect_recent_events, ProjectSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Options passed from the `ccteam init` argument parser.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Install in this directory. `None` defaults to the current
    /// working directory. (`slug` does not affect the location — it only
    /// names the registered project.)
    pub install_in: Option<std::path::PathBuf>,
    /// Slug override — sets the *registered project name* only, never
    /// the install location. When absent we derive it from the install
    /// target's dir basename.
    pub slug: Option<String>,
    /// Team for new installs (default `dev`). On refresh the existing
    /// `state.json::team` is preserved unless `force`. No longer a CLI
    /// flag — set internally by `project new` / tests; the CLI always
    /// takes the `dev` default.
    pub team: Option<String>,
    /// Overwrite every ccteam-managed file (state.json, settings.json
    /// marker section, workflow.yaml, helper templates). Without `force`
    /// re-runs preserve the user-edited workflow. Never touches
    /// `.claude/agents/*.md` — ccteam seeds no roles (roleless default).
    pub force: bool,
    /// Set the project owner identity (`ProjectState.owner`,
    /// `"channel:chat_id"` — e.g. `user:<tenant>` / `telegram:<chat_id>`). A
    /// bare value (no `:`) is scoped to the per-user identity namespace
    /// (`alice` → `user:alice`). Present ⇒ override an existing owner on
    /// re-init (without `--force`); absent ⇒ preserve. `None` = unspecified.
    pub owner: Option<String>,
}

/// Unified project install. Scaffolds `.ccteam/state.json` +
/// `.ccteam/workflow.yaml`, seeds NO roles (roleless default — the
/// default session is a bare vendor reading the project
/// `CLAUDE.md`/`AGENTS.md`), and registers the project in
/// `~/.ccteam/config.yaml`.
///
/// Three scenarios, one command:
///
/// 1. **Fresh cwd / fresh dir**: writes `.ccteam/state.json` +
///    `.ccteam/workflow.yaml` and the ccteam-managed hook section in
///    `.claude/settings.local.json`; appends to
///    `~/.ccteam/config.yaml::projects[]`.
/// 2. **Existing repo cwd** (no `.ccteam/` yet): same as (1) — never
///    touches existing user files.
/// 3. **Already-ccteam project cwd**: refreshes state.json + the
///    settings marker section; preserves `workflow.yaml` unless
///    `--force` (overwrite). `.claude/agents/*.md` is always left alone.
///
/// MCP registration lives in `ccteam config` (not here). The global
/// `~/.ccteam/` skeleton + hook dispatcher are ensured idempotently on
/// every invocation.
pub fn run_init(paths: &CcteamPaths, opts: InitOptions) -> Result<String> {
    use std::process::Command;

    // -- 1. Global ~/.ccteam/ skeleton (idempotent) -------------------
    // v0.8.6 D1.1: create exactly the canonical home-layout manifest
    // (`hooks/progress/run/state`) — the same set `ccteam doctor`'s
    // home-drift check tolerates. The orchestrator-era subdirs
    // (`phases/templates/inbox/control`) are no longer written: nothing
    // reads them post-W2, and creating them made a fresh `ccteam init`
    // immediately report self-inflicted drift.
    //
    // `ensure_ccteam_home` (ccteam-core) folds that canonical-dir loop
    // and the V0.6.1 F139 `hook.sh` dispatcher materialization into one
    // idempotent call shared by every create/start path (init here, the
    // web/IM `bootstrap_project_at_dir`, the daemon at `ccteam start`).
    // The hook.sh write must precede `install_project_at` so the
    // freshly-rendered `.claude/settings.local.json` hook commands point
    // at a file that actually exists.
    ccteam_core::ensure_ccteam_home(paths)
        .context("подготовить домашний каталог ~/.ccteam/ (каталоги + hook.sh)")?;

    // -- 2. Resolve project install target ---------------------------
    let target = resolve_install_target(&opts)?;
    let slug_was_explicit = opts.slug.is_some();
    let derived_slug = opts.slug.clone().unwrap_or_else(|| {
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string()
    });
    // V0.4.3 F76: validate slug grammar before anything writes to disk.
    // Catches whitespace / unicode / leading-dash / uppercase cases.
    // When the slug was derived from the install-dir basename (no
    // `--slug`), point the user at `--slug` instead of failing opaquely
    // — a dir like `AgentServe` (caps) can't be a slug verbatim.
    let target_slug = ccteam_core::validate_slug_format(&derived_slug).with_context(|| {
        if slug_was_explicit {
            format!("ccteam init: недопустимый slug {derived_slug:?}")
        } else {
            format!(
                "ccteam init: имя каталога установки {derived_slug:?} не является корректным slug \
                 (только строчные ASCII / цифры / дефисы). Укажите явное имя, например \
                 `ccteam init --slug <lowercase-name>`."
            )
        }
    })?;
    let target_team = opts.team.clone().unwrap_or_else(|| "dev".to_string());

    // -- 3a. Refuse install in the ccteam repo itself ----------------
    // V0.6.8 F203 — `--force` escape: legitimate self-hosting /
    // dogfooding / nested-research-project cases need to install a
    // ccteam project inside the ccteam source tree. The default still
    // refuses to avoid circular-hook surprises for casual users.
    if is_ccteam_repo(&target) && !opts.force {
        return Err(anyhow::anyhow!(
            "отказ от установки ccteam в самом репозитории ccteam: {}\n\n\
             этот каталог содержит исходники ccteam — установка создаст циклическую настройку хуков. Выберите другой каталог (или `cd` в свой проект и повторите) либо передайте `--force`, если действительно нужен проект ccteam внутри исходного репозитория (например, для self-hosting / dogfooding).",
            target.display(),
        ));
    }
    // -- 3b. Refuse sensitive paths (HOME / filesystem root) ---------
    refuse_sensitive_install_target(&target, opts.force)?;
    // -- 3c. Fail-loud slug collision against config.yaml -----------
    if let Some(existing) = ccteam_core::lookup_project_in_config(&paths.root, &target_slug)? {
        let same_target = std::fs::canonicalize(&existing.path)
            .ok()
            .zip(std::fs::canonicalize(&target).ok())
            .is_some_and(|(a, b)| a == b);
        if !same_target && !opts.force {
            return Err(anyhow::anyhow!(
                "slug `{slug}` is already registered in {config} pointing at {existing}, \
                 but this install would point it at {requested}. Refusing to silently retarget.\n\n\
                 Resolve by either:\n  \
                 - pick a different slug:  `ccteam init --slug <other-name>` (or `--in <other-path>`),\n  \
                 - intentionally retarget: re-run with `--force` (the registry entry will be \
                 rewritten to {requested}).",
                slug = target_slug,
                config = ccteam_core::ccteam_config_path(&paths.root).display(),
                existing = existing.path.display(),
                requested = target.display(),
            ));
        }
    }
    // `preflight_project_upsert` distinguishes a permanently retired generation
    // from a slug merely reserved by surviving progress state, and words each
    // case truthfully. Stamping a retirement-flavoured context over both would
    // misreport the reservation, so only a neutral command prefix is added.
    ccteam_core::preflight_project_upsert(&paths.root, &target_slug).context("ccteam init")?;

    // -- 4. Project install pass ------------------------------------
    let project_report = install_project_at(paths, &target, &target_slug, &target_team, &opts)?;

    // -- 5. Upsert config.yaml::projects[] --------------------------
    let entry = ccteam_core::ProjectEntry {
        slug: project_report.slug.clone(),
        path: target.clone(),
        host: ccteam_core::LOCAL_HOST.to_string(),
        remote_slug: None,
        remote_path: None,
        team: project_report.team.clone(),
        installed_at: chrono::Utc::now(),
    };
    ccteam_core::upsert_project_in_config(&paths.root, entry)
        .context("добавить или обновить проект в ~/.ccteam/config.yaml")?;

    // -- 6. Health check + optional wizard --------------------------
    let bin = current_ccteam_bin().ok();
    let claude = Command::new("claude").arg("--version").output();
    let tmux = ccteam_core::tmux_version();

    let mut out = String::new();
    out.push_str(&format!(
        "ccteam init — {}\n\n",
        project_report.action_summary()
    ));
    out.push_str(&format!("  target dir       {}\n", target.display()));
    out.push_str(&format!("  slug             {}\n", project_report.slug));
    out.push_str(&format!("  team             {}\n", project_report.team));
    if let Some(owner) = &project_report.owner {
        out.push_str(&format!("  owner            {owner}\n"));
    }
    out.push_str(&format!(
        "  state.json       {} ({})\n",
        target.join(".ccteam").join("state.json").display(),
        project_report.state_action,
    ));
    out.push_str(&format!(
        "  workflow.yaml    {} ({})\n",
        target.join(".ccteam").join("workflow.yaml").display(),
        project_report.workflow_action,
    ));
    out.push_str(&format!(
        "  каталог agents   {} ({})\n",
        target.join(".claude").join("agents").display(),
        project_report.agents_action,
    ));
    out.push_str(&format!(
        "  config.yaml      {} (добавлено или обновлено)\n",
        ccteam_core::ccteam_config_path(&paths.root).display(),
    ));

    out.push_str("\nпроверка готовности:\n");
    match &claude {
        Ok(o) if o.status.success() => out.push_str(&format!(
            "  claude   : {}\n",
            String::from_utf8_lossy(&o.stdout).trim()
        )),
        _ => out.push_str(
            "  claude   : НЕ НАЙДЕН в PATH (установка: https://claude.com/claude-code)\n",
        ),
    }
    match &tmux {
        Some(version) => out.push_str(&format!("  tmux     : {version}\n")),
        _ => out.push_str("  tmux     : НЕ НАЙДЕН в PATH (apt install tmux / brew install tmux)\n"),
    }
    match &bin {
        Some(p) => out.push_str(&format!("  ccteam   : {}\n", p.display())),
        None => {
            out.push_str("  ccteam   : current_exe() не удался (путь к бинарнику не определён)\n")
        }
    }

    out.push_str("\nдальше:\n");
    out.push_str("  1. install: оставьте `ccteam` в PATH; `ccteam config` зарегистрирует MCP-сервер для Claude и Codex\n");
    out.push_str("  2. init: проект инициализирован\n");
    out.push_str(
        "  3. config: выполните `ccteam config` для регистрации MCP и задания учётных данных IM\n",
    );
    out.push_str("  4. start: выполните `ccteam start` для запуска gateway и web-консоли\n");
    out.push_str(
        "  5. cd: отправьте `/cd <project>`; первое сообщение запустит сессию без роли (читает CLAUDE.md/AGENTS.md проекта)\n",
    );
    out.push_str("  подсказка о ролях: по умолчанию сессии без роли (читают CLAUDE.md/AGENTS.md проекта); установите или создайте рабочие роли в .claude/agents/<role>.md\n");
    out.push_str("  руководство: docs/usage.md\n");
    Ok(out)
}

/// Resolve where to install. Priority:
///   1. `--in <path>`  (absolute or relative; created if absent)
///   2. current working directory
///
/// `--slug` only sets the *registered project name*; it never changes
/// the install location (it used to relocate to `<projects_root>/<slug>/`
/// — a drift from the documented "override the derived slug" intent that
/// silently sent users standing in an existing repo to an empty skeleton
/// elsewhere). To create a fresh project under
/// `<projects_root>/<team>-<slug>/`, use `ccteam project new <slug>`.
fn resolve_install_target(opts: &InitOptions) -> Result<std::path::PathBuf> {
    if let Some(p) = &opts.install_in {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            std::env::current_dir()
                .context("прочитать cwd для разрешения --in")?
                .join(p)
        };
        std::fs::create_dir_all(&abs)
            .with_context(|| format!("создать цель --in {}", abs.display()))?;
        return Ok(abs);
    }
    std::env::current_dir().context("прочитать cwd как цель установки")
}

/// Heuristic to detect the ccteam source repo so we don't accidentally
/// install ccteam inside ccteam (creates circular hook loops per
/// CLAUDE.md §六).
fn is_ccteam_repo(dir: &std::path::Path) -> bool {
    dir.join("Cargo.toml").is_file() && dir.join("crates").join("ccteam-cli").is_dir()
}

/// Refuse to install at the filesystem root or at `$HOME` — installing
/// there would spam the user's home with a `.ccteam/` + `.claude/`
/// skeleton and register every dotfile-bearing directory as one project.
/// `--force` overrides.
fn refuse_sensitive_install_target(target: &std::path::Path, force: bool) -> Result<()> {
    let canonical = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let is_root = canonical.parent().is_none();
    let is_home = dirs::home_dir()
        .and_then(|h| std::fs::canonicalize(&h).ok())
        .is_some_and(|h| h == canonical);
    if (is_root || is_home) && !force {
        return Err(anyhow::anyhow!(
            "refusing to install at {} — this looks like $HOME or the filesystem root.\n\
             Make a subdirectory (`mkdir myapp && cd myapp && ccteam init`) or pass `--force` \
             if you really mean to install here.",
            target.display(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ProjectInstallReport {
    slug: String,
    team: String,
    /// The final `ProjectState.owner` after install (newly set by
    /// `--owner`, an override on re-init, or the preserved existing value).
    /// `None` when the project has no owner. Surfaced in the receipt.
    owner: Option<String>,
    fresh: bool,
    state_action: &'static str,
    workflow_action: &'static str,
    agents_action: &'static str,
}

impl ProjectInstallReport {
    fn action_summary(&self) -> &'static str {
        if self.fresh {
            "fresh install"
        } else {
            "refresh"
        }
    }
}

/// Lay down (or refresh) a ccteam project at `target`.
fn install_project_at(
    paths: &CcteamPaths,
    target: &std::path::Path,
    slug: &str,
    team: &str,
    opts: &InitOptions,
) -> Result<ProjectInstallReport> {
    let ccteam_dir = target.join(".ccteam");
    let state_path = ccteam_dir.join("state.json");
    let fresh = !state_path.exists();

    // v0.8.20 F1: `--owner` stamps `ProjectState.owner` at init. Normalize the
    // raw value (bare → `user:`; `:`-bearing → verbatim) once, then apply it in
    // whichever branch we take. Light, non-blocking validation: an unknown
    // `user:<tenant>` warns on stderr but is still written (the tenant may be
    // created later). Override-on-reinit needs no `--force`.
    let requested_owner = opts.owner.as_deref().and_then(normalize_owner);
    if let Some(owner) = &requested_owner {
        if let Some(tenant_id) = owner.strip_prefix("user:") {
            if tenant_id != "web-api"
                && ccteam_core::tenants::TenantRegistry::load(&paths.users_dir())
                    .by_id(tenant_id)
                    .is_none()
            {
                eprintln!(
                    "предупреждение: --owner {owner} ссылается на неизвестный tenant в {}; всё равно записываю",
                    paths.users_dir().display(),
                );
            }
        }
    }

    let state_action: &'static str;
    let workflow_action: &'static str;
    let agents_action: &'static str;
    let final_team: String;
    let owner_final: Option<String>;

    if fresh {
        ccteam_core::bootstrap_project_at_dir(
            paths,
            target,
            slug,
            "(installed via `ccteam init`)",
            team,
        )?;
        ccteam_core::scaffold_workflow_yaml(target, false)?;
        state_action = "created";
        workflow_action = "scaffolded";
        // v0.9.0 W2 (F6.1) — engine neutralization: init seeds NO role. The
        // default session is roleless (bare vendor reads project CLAUDE.md /
        // AGENTS.md); `.claude/agents/` is left untouched.
        agents_action = "none (roleless default)";
        final_team = team.to_string();
        // Fresh state.json was written with owner = None; stamp it when asked.
        if let Some(owner) = &requested_owner {
            let mut st = ccteam_core::ProjectState::load(&state_path)
                .with_context(|| format!("load {} to set owner", state_path.display()))?;
            st.owner = Some(owner.clone());
            st.save(&state_path)?;
            owner_final = Some(owner.clone());
        } else {
            owner_final = None;
        }
    } else {
        let mut existing_state = ccteam_core::ProjectState::load(&state_path)
            .with_context(|| format!("load existing {}", state_path.display()))?;
        if opts.force || opts.team.is_some() {
            existing_state.team = team.to_string();
        }
        existing_state.slug = slug.to_string();
        existing_state.tmux_session = format!("ccteam-{slug}");
        // Override on re-init only when `--owner` is given; otherwise preserve.
        if let Some(owner) = &requested_owner {
            existing_state.owner = Some(owner.clone());
        }
        existing_state.save(&state_path)?;
        final_team = existing_state.team.clone();
        owner_final = existing_state.owner.clone();
        state_action = "refreshed";

        workflow_action = if opts.force {
            ccteam_core::scaffold_workflow_yaml(target, true)?;
            "overwritten (--force)"
        } else {
            "preserved"
        };

        // v0.9.0 W2 (F6.1) — ccteam seeds no roles, so there is nothing to
        // (re)scaffold or reset; `--force` no longer touches
        // `.claude/agents/` (user files there survive untouched).
        agents_action = if opts.force {
            "none (roleless default)"
        } else {
            "preserved"
        };
    }

    Ok(ProjectInstallReport {
        slug: slug.to_string(),
        team: final_team,
        owner: owner_final,
        fresh,
        state_action,
        workflow_action,
        agents_action,
    })
}

/// Normalize a raw `--owner` value (`ccteam init --owner`) into the
/// `ProjectState.owner` convention. A value already containing `:` is taken
/// verbatim (`user:u123`, `telegram:456`); a bare value is scoped to the
/// per-user identity namespace (`alice` → `user:alice`). Whitespace-only ⇒ `None`.
fn normalize_owner(raw: &str) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() {
        None
    } else if v.contains(':') {
        Some(v.to_string())
    } else {
        Some(format!("user:{v}"))
    }
}

/// `ccteam ls`. Returns either a human table or the interfaces.md §10.3
/// JSON shape (a single string, not printed — caller decides).
pub fn run_ls(paths: &CcteamPaths, format: OutputFormat) -> Result<String> {
    let projects = collect_projects(paths)?;
    let daemon_up = ccteam_core::daemon::daemon_reachable(paths);
    Ok(match format {
        OutputFormat::Text => render_ls_text(paths, &projects, daemon_up),
        OutputFormat::Json => render_ls_json(paths, &projects, daemon_up)?,
    })
}

/// `ccteam show <slug>`. Renders the full project view per
/// interfaces.md §10.3 (json) or a human-readable summary (text).
///
/// Cost figures come from `cost_summary` (progress.jsonl plus live
/// claude state.json) instead of the retired `state.cost_used_usd`
/// accumulator. The old `cost used: $X.XX` line is replaced with
/// `cost (24h)` plus `cost (active)` so the user sees both windowed
/// spend and what's burning right now.
pub fn run_show(paths: &CcteamPaths, slug: &str, format: OutputFormat) -> Result<String> {
    let state_path = paths.project_state(slug);
    if !state_path.exists() {
        bail!("проект не найден: {slug}");
    }
    let state = ProjectState::load(&state_path)?;
    let recent = collect_recent_events(paths, slug, 50)?;
    let artifacts = collect_artifacts(paths, slug);
    let progress_path = paths.progress_jsonl(slug);
    let cost = cost_summary(slug, &progress_path, paths)?;
    let sessions = ccteam_core::active_sessions(slug, paths).unwrap_or_default();

    Ok(match format {
        OutputFormat::Text => render_show_text(&state, &cost, &recent, &artifacts, &sessions),
        OutputFormat::Json => render_show_json(&state, &cost, &recent, &artifacts, &sessions)?,
    })
}

/// `ccteam attach <slug>`. Resolves the underlying session medium and
/// dispatches:
///
/// 1. If the project's workflow.yaml has
///    `mode: agent-team`, read the lead session id from
///    `.ccteam/team-snapshot.json::lead_session_id` and exec
///    `claude attach <id>`. Friendly-error if the snapshot is missing
///    or the lead id is not yet populated (lead not spawned).
/// 2. If a tmux session named `ccteam-<slug>` exists → `tmux attach -t …`
///    (meta-agent + legacy projects).
/// 3. Else if the project's latest `agent_spawn` event in
///    `progress.jsonl` carries a `claude --bg` `job_id` → `claude attach
///    <job_id>` (worker default).
/// 4. Else → fail-loud "no live session for <slug>".
///
/// Always prints the underlying command before exec'ing so the operator
/// learns the lower-level tool.
/// Interactive attach to a session hosted by the
/// ccteam-owned rmux daemon (`~/.ccteam/run/mux.sock`).
///
/// Path A (use `rmux-client` directly): `connect` → `begin_attach`
/// → `into_parts` → `attach_terminal_with_initial_bytes`. The last call
/// drives the local TTY in raw mode (termios) on the CLI's own
/// controlling terminal — exactly the in-process analogue of
/// `tmux attach -t <name>`, so it stays a blocking sync call here
/// (terminal handover doesn't fit the async trait — W0 audit §4-D).
///
/// Detach behavior (chord / banner) is determined by the rmux daemon
/// itself: it emits `AttachMessage::DetachKill` / `DetachExec`, which
/// the client driver handles. ccteam does NOT pick a detach key
/// (e.g. `Ctrl-]`) — that is rmux-owned.
#[cfg(unix)]
fn rmux_interactive_attach(session_name: &str) -> Result<()> {
    use rmux_client::{connect_or_absent, AttachTransition, ConnectResult};
    use rmux_proto::SessionName;

    let socket = ccteam_harness::default_ccteam_harness_socket_path();
    eprintln!("→ rmux attach {session_name} (сокет {})", socket.display());

    let connection = match connect_or_absent(&socket)
        .context("подключиться к rmux-демону ccteam")?
    {
        ConnectResult::Connected(conn) => conn,
        ConnectResult::Absent => bail!(
            "rmux-демон ccteam не запущен (сокет `{}` отсутствует).\n  \
             Сначала запустите проект:  ccteam start {session_name}\n  \
             (демон запускается через `ccteam start`; до этого attach не к чему подключить.)",
            socket.display(),
        ),
    };

    let name = SessionName::new(session_name.to_string())
        .map_err(|e| anyhow::anyhow!("некорректное имя rmux-сессии `{session_name}`: {e}"))?;

    let transition = connection
        .begin_attach(name)
        .with_context(|| format!("начать подключение к rmux-сессии `{session_name}`"))?;

    let upgrade = match transition {
        AttachTransition::Upgraded(upgrade) => upgrade,
        AttachTransition::Rejected(other) => bail!(
            "rmux-демон отклонил подключение к сессии `{session_name}`: {other:?}\n  \
             (сессии может не быть — проверьте `ccteam session ls` или запустите через `ccteam start`.)",
        ),
    };

    // into_parts yields the raw upgraded UnixStream plus any bytes the
    // daemon already streamed past the response frame; the driver
    // replays those `initial_bytes` before entering the poll loop so no
    // pane output is lost on the attach boundary.
    let (stream, initial_bytes) = upgrade.into_parts();
    rmux_client::attach_terminal_with_initial_bytes(stream, initial_bytes)
        .map_err(|e| anyhow::anyhow!("подключение rmux-сессии `{session_name}`: {e}"))?;
    Ok(())
}

/// Non-Unix fallback: the rmux backend is Unix-first (UDS transport),
/// so interactive attach is not yet wired on Windows. Fail loud.
#[cfg(not(unix))]
fn rmux_interactive_attach(session_name: &str) -> Result<()> {
    bail!(
        "интерактивное подключение rmux пока не поддерживается на этой платформе \
         (сессия `{session_name}`); используйте backend tmux (уберите CCTEAM_MUX_BACKEND).",
    )
}

/// Run a future to completion on a throwaway current-thread runtime. The
/// session-listing / chat-attach handlers are synchronous CLI entry points but
/// need the async `ProcessBackend` enumeration; this keeps that bridge in one
/// place.
fn block_on_async<F: std::future::Future>(fut: F) -> Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("создать runtime tokio")?;
    Ok(rt.block_on(fut))
}

/// Best-effort: tell a running daemon to reload IM channels from the
/// freshly-saved `credentials.json` over its mcp.sock (`ccteam/reload`). This
/// makes `ccteam config` take effect WITHOUT a daemon restart or any agent-
/// session restart. Entirely best-effort: a down daemon (no socket) or a send
/// error is swallowed — the "ccteam start will pick these up" message still
/// applies, so the config command MUST NOT fail when this can't be delivered.
fn notify_daemon_im_reload() {
    let Ok(paths) = ccteam_core::CcteamPaths::from_env() else {
        return;
    };
    let socket = ccteam_core::daemon_socket_path(&paths);
    let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ccteam/reload"});
    let _ = block_on_async(crate::mcp_serve::forward_to_socket(&socket, &req));
}

/// Interactively hand the controlling tty over to an existing mux session by
/// its exact name, honoring `CCTEAM_MUX_BACKEND`. Read-only handover — it never
/// captures pane text (R6); it only checks existence and execs the attach.
fn attach_interactive_by_name(session_name: &str) -> Result<()> {
    if ccteam_harness::backend_kind_from_env() == ccteam_harness::BackendKind::Rmux {
        return rmux_interactive_attach(session_name);
    }
    let tmux_session = TmuxSession::from_name(session_name.to_string());
    if !tmux_session.exists() {
        bail!("mux-сессия не запущена: {}", tmux_session.name());
    }
    eprintln!("→ tmux attach -t {}", tmux_session.name());
    let argv = ccteam_harness::interactive_attach_argv(
        ccteam_harness::BackendKind::Tmux,
        tmux_session.name(),
    );
    let (bin, args) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("interactive_attach_argv вернул пустой argv"))?;
    let status = Command::new(bin)
        .args(args)
        .status()
        .context("запустить tmux attach")?;
    if !status.success() {
        bail!("tmux attach завершился с {status}");
    }
    Ok(())
}

/// Resolve a gateway chat-mode bot session (`ccteam-chat-<slug>-<role>`) for
/// `slug_or_name` (+ optional `role`) and interactively attach to it. These
/// sessions are invisible to [`run_attach`], which only resolves the
/// project-level `ccteam-<slug>` session.
///
/// Returns `Ok(true)` when a chat session was matched and the attach was
/// dispatched; `Ok(false)` *only* when `role` is omitted AND no live chat
/// session matches `slug_or_name`, so the caller may fall back to the
/// project-oriented attach. Read-only enumeration (R6) — never captures panes.
/// Resolve a chat-session reference to its canonical tmux name
/// (`ccteam-chat-<slug>-<sid>`):
/// - a full `ccteam-chat-…` name passes through verbatim;
/// - an explicit `sid` yields the deterministic name;
/// - otherwise live chat sessions are enumerated and filtered by slug.
///
/// Returns `Ok(None)` when nothing matches (the caller falls back to the
/// project pane `ccteam-<slug>`); `Err` when `<slug>` is ambiguous across
/// sessions. Shared by `attach` and `peek` so both resolve identically.
///
/// The disambiguator is the **sid** (`s<N>`), not a role: the pane name's
/// trailing segment is the sid, and the same `(project, role)` can host
/// several independent sessions, so a role no longer uniquely names one.
pub fn resolve_chat_session_name(slug_or_name: &str, sid: Option<&str>) -> Result<Option<String>> {
    if slug_or_name.starts_with(ccteam_harness::CHAT_SESSION_PREFIX) {
        return Ok(Some(slug_or_name.to_string()));
    }
    if let Some(sid) = sid {
        return Ok(Some(ccteam_harness::chat_session_name(slug_or_name, sid)));
    }
    let backend = ccteam_harness::default_process_backend();
    let live = block_on_async(ccteam_harness::list_chat_sessions(backend.as_ref()))??;
    let mut matches: Vec<(String, String)> = live
        .iter()
        .filter_map(|name| {
            let (slug, sid) = ccteam_harness::parse_chat_session_name(name)?;
            (slug == slug_or_name).then(|| (sid, name.clone()))
        })
        .collect();
    matches.sort();
    match matches.as_slice() {
        [] => Ok(None),
        [(_sid, name)] => Ok(Some(name.clone())),
        many => {
            let mut msg = format!(
                "у `{slug_or_name}` {} активных чат-сессий; укажите sid:",
                many.len()
            );
            for (sid, name) in many {
                msg.push_str(&format!("\n  {slug_or_name} {sid}   # {name}"));
            }
            bail!("{msg}")
        }
    }
}

pub fn try_attach_chat_session(slug_or_name: &str, sid: Option<&str>) -> Result<bool> {
    match resolve_chat_session_name(slug_or_name, sid)? {
        Some(name) => {
            attach_interactive_by_name(&name)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `ccteam session ls` — read-only snapshot of gateway chat-mode bot sessions.
///
/// The row source is the daemon's **persisted** session records (sid ·
/// project · role · vendor · permission_mode) via
/// [`ccteam_im::gateway::tracked_chat_sessions`], not a process-name
/// enumeration. A tracked record means the daemon owns the session, so it
/// shows **live**: codex sessions (which the process backend can't always
/// confirm by name) are live whenever the gateway tracks them, instead of a
/// false "registered, not running". Live OS panes (`ccteam-chat-*`) that are
/// **not** in the tracked set are surfaced as orphans (a process that outlived
/// the daemon that spawned it). Reading the mux backend is name-enumeration
/// only (never capture-pane); never spawns or kills.
pub fn run_sessions() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let daemon_up = ccteam_core::daemon::daemon_reachable(&paths);

    // Live OS pane names (for orphan detection). Best-effort: a backend error
    // just means we can't flag orphans, not that we refuse to list tracked rows.
    let live = block_on_async(ccteam_harness::list_chat_sessions(
        ccteam_harness::default_process_backend().as_ref(),
    ))
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();

    // A missing / unreadable registry is non-fatal: no tracked rows, every live
    // pane is then an orphan.
    let tracked = ccteam_im::gateway::tracked_chat_sessions(&paths.root).unwrap_or_default();

    print!("{}", render_sessions_table(&tracked, &live, daemon_up));
    Ok(())
}

/// `s<N>` → `N` for a recency-tiebreak sort; a malformed/orphan sid (no
/// number, e.g. one the process backend can't parse) sorts last (`u64::MAX`).
/// Mirrors `ccteam-im`'s private `gateway::session_index` (not exported —
/// this CLI-side copy is tiny and each call site is deterministic).
fn numeric_sid(sid: &str) -> u64 {
    sid.strip_prefix('s')
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

/// `Some(rfc3339)` → `"<age> ago"`; `None`/unparseable → `"-"` (never
/// fabricates an age for a row with no `meta.json` backing, e.g. an orphan).
fn format_last_active(raw: &str) -> String {
    match parse_rfc3339_age_secs(raw) {
        Some(secs) => format!("{} назад", humanize_secs_local(secs)),
        None => "-".to_string(),
    }
}

/// Pure renderer for `ccteam session ls` (testable without a daemon / terminal).
///
/// `tracked` = persisted gateway session records (each shows **live** when the
/// daemon is up, else `registered (daemon down)`); `live_panes` = live
/// `ccteam-chat-*` OS pane names used only to flag **orphans** (live pane ∧ not
/// tracked). Columns: SLUG · SID · ROLE · VENDOR · LAST ACTIVE · STATUS. An
/// orphan has no role/vendor (the pane name only carries slug+sid post-F1) →
/// `-`, and no `meta.json` to read a last-active time from → `-`.
///
/// Rows sort by `last_active` desc (numeric sid desc tiebreak for
/// equal/missing `last_active`, e.g. every orphan), so the CLI view reads
/// recency-first like the IM `/sessions` and REST session list.
fn render_sessions_table(
    tracked: &[ccteam_im::gateway::TrackedSessionRow],
    live_panes: &[String],
    daemon_up: bool,
) -> String {
    // Canonical names the daemon tracks, to subtract from live panes → orphans.
    let tracked_names: std::collections::BTreeSet<String> = tracked
        .iter()
        .map(|r| ccteam_harness::chat_session_name(&r.project, &r.sid))
        .collect();

    struct Row {
        slug: String,
        sid: String,
        role: String,
        // v0.8.22 P1 — user-facing session title (session-title system);
        // `"-"` when the session has none yet (fallback: the ROLE column
        // alongside it already carries the role/sid identity as today).
        title: String,
        vendor: String,
        status: String,
        last_active: String,
    }

    let tracked_status = if daemon_up {
        "активна"
    } else {
        "зарегистрирована (демон недоступен)"
    };

    let mut rows: Vec<Row> = tracked
        .iter()
        .map(|r| Row {
            slug: r.project.clone(),
            sid: r.sid.clone(),
            role: r.role.clone(),
            title: r
                .title
                .as_deref()
                .filter(|t| !t.is_empty())
                .unwrap_or("-")
                .to_string(),
            vendor: r.vendor.clone(),
            status: tracked_status.to_string(),
            last_active: r.last_active.clone(),
        })
        .collect();

    // Orphans: a live pane whose canonical name the daemon doesn't track.
    for name in live_panes {
        if tracked_names.contains(name) {
            continue;
        }
        if let Some((slug, sid)) = ccteam_harness::parse_chat_session_name(name) {
            rows.push(Row {
                slug,
                sid,
                role: "-".to_string(),
                title: "-".to_string(),
                vendor: "-".to_string(),
                status: "сирота (неотслеживаемая активная панель)".to_string(),
                last_active: String::new(),
            });
        }
    }

    rows.sort_by(|a, b| {
        b.last_active
            .cmp(&a.last_active)
            .then_with(|| numeric_sid(&b.sid).cmp(&numeric_sid(&a.sid)))
    });

    if rows.is_empty() {
        let mut out = String::new();
        out.push_str("чат-сессий нет (нет ни отслеживаемых, ни активных).\n");
        out.push_str(
            "  Чат-сессии gateway появятся здесь после запуска бота \
             (например, через Telegram `/new`).\n",
        );
        return out;
    }

    // Column widths mirror the existing `w_sid` algorithm (header-floor max).
    let w_slug = rows.iter().map(|r| r.slug.len()).max().unwrap_or(0).max(4);
    let w_sid = rows.iter().map(|r| r.sid.len()).max().unwrap_or(0).max(3);
    let w_role = rows.iter().map(|r| r.role.len()).max().unwrap_or(0).max(4);
    let w_title = rows.iter().map(|r| r.title.len()).max().unwrap_or(0).max(5);
    let w_vendor = rows
        .iter()
        .map(|r| r.vendor.len())
        .max()
        .unwrap_or(0)
        .max(6);
    let last_active_display: Vec<String> = rows
        .iter()
        .map(|r| format_last_active(&r.last_active))
        .collect();
    let w_last_active = last_active_display
        .iter()
        .map(|s| s.len())
        .max()
        .unwrap_or(0)
        .max(20); // "ПОСЛЕДНЯЯ АКТИВНОСТЬ".chars().count()

    let mut out = String::new();
    let header = format!(
        "{:<w_slug$}  {:<w_sid$}  {:<w_role$}  {:<w_title$}  {:<w_vendor$}  {:<w_last_active$}  СТАТУС",
        "SLUG", "SID", "РОЛЬ", "НАЗВАНИЕ", "ВЕНДОР", "ПОСЛЕДНЯЯ АКТИВНОСТЬ"
    );
    out.push_str(header.trim_end());
    out.push('\n');
    for (r, last_active) in rows.iter().zip(last_active_display.iter()) {
        let line = format!(
            "{:<w_slug$}  {:<w_sid$}  {:<w_role$}  {:<w_title$}  {:<w_vendor$}  {:<w_last_active$}  {}",
            r.slug, r.sid, r.role, r.title, r.vendor, last_active, r.status,
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push('\n');
    out.push_str("подключение: `ccteam internal attach <slug> [sid]`  (Telegram: `/sessions`)\n");
    out
}

pub fn run_attach(paths: &CcteamPaths, slug: &str) -> Result<()> {
    // V0.5.0 F93b: agent-team mode dispatches to the lead session.
    if let Some(lead_id) = read_agent_team_lead_session_id(paths, slug)? {
        eprintln!("→ claude attach {lead_id}");
        let status = Command::new("claude")
            .args(["attach", &lead_id])
            .status()
            .context("запустить claude attach")?;
        if !status.success() {
            bail!("claude attach завершился с {status}");
        }
        return Ok(());
    }

    // V0.8 W5 — backend-aware interactive attach. Branch on the
    // configured mux backend BEFORE the tmux exists-check: under the
    // rmux backend the tmux session never exists, so falling through to
    // the tmux / claude-bg paths would always miss. The rmux path
    // attaches to the ccteam-hosted rmux daemon via rmux-client
    // directly (Path A). The tmux path is unchanged (default).
    let session_name = session_name_for_project(paths, slug);
    if ccteam_harness::backend_kind_from_env() == ccteam_harness::BackendKind::Rmux {
        return rmux_interactive_attach(&session_name);
    }

    let tmux_session = TmuxSession::from_name(session_name);
    if tmux_session.exists() {
        eprintln!("→ tmux attach -t {}", tmux_session.name());
        // V0.8 W1 — argv from `ccteam_harness::interactive_attach_argv`
        // (free fn, not a trait method — terminal handover doesn't fit
        // async; audit delta 6). Caller still spawns blocking
        // Command::status() on the CLI's own controlling tty.
        let argv = ccteam_harness::interactive_attach_argv(
            ccteam_harness::BackendKind::Tmux,
            tmux_session.name(),
        );
        let (bin, args) = argv
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("interactive_attach_argv вернул пустой argv"))?;
        let status = Command::new(bin)
            .args(args)
            .status()
            .context("запустить tmux attach")?;
        if !status.success() {
            bail!("tmux attach завершился с {status}");
        }
        return Ok(());
    }

    // V0.4.0 fallback: walk progress.jsonl for the latest agent_spawn
    // and grab its job_id. The actual bg-session id is the
    // `daemonShort` written to `~/.claude/jobs/<id>/state.json`; in
    // F61 we stamped it onto `SessionHandle::job_id` and wrote it to
    // the `agent_spawn` event's `session_id` field is the orchestrator's
    // internal sid — the real bg short-id lives in state.json.
    if let Some(job_id) = latest_claude_bg_job_id(paths, slug) {
        eprintln!("→ claude attach {job_id}");
        let status = Command::new("claude")
            .args(["attach", &job_id])
            .status()
            .context("запустить claude attach")?;
        if !status.success() {
            bail!("claude attach завершился с {status}");
        }
        return Ok(());
    }

    bail!(
        "для `{slug}` нет активной сессии: tmux-сессия `{}` не запущена, задача `claude --bg` не записана в progress.jsonl. Запустите через `ccteam spawn {slug} <role>`.",
        tmux_session.name()
    )
}

/// Probe whether a project is in agent-team mode and return its lead
/// session id from `.ccteam/team-snapshot.json`.
///
/// Returns:
///   - `Ok(Some(id))` — project is agent-team mode AND has been started
///     (snapshot exists, `lead_session_id` populated).
///   - `Ok(None)` — project is artifact-driven OR no project exists at
///     `<slug>` (let the caller fall through to tmux / bg paths).
///   - `Err(_)` — project is agent-team mode but snapshot is missing /
///     `lead_session_id` not yet written (lead hasn't been started yet).
fn read_agent_team_lead_session_id(paths: &CcteamPaths, slug: &str) -> Result<Option<String>> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        return Ok(None);
    }
    let spec = match ccteam_flow::WorkflowSpec::load_for_project(&project_dir) {
        Ok(spec) => spec,
        Err(_) => return Ok(None),
    };
    if !matches!(spec.mode, ccteam_flow::WorkflowMode::AgentTeam) {
        return Ok(None);
    }
    let snapshot_path = project_dir.join(".ccteam").join("team-snapshot.json");
    if !snapshot_path.exists() {
        bail!(
            "project `{slug}` is in agent-team mode but has no team-snapshot.json yet.\n  \
             Start the lead session first:  ccteam start {slug}\n  \
             (snapshot is written by `ccteam start` after spawning the __lead session.)",
        );
    }
    let body = std::fs::read_to_string(&snapshot_path)
        .with_context(|| format!("read {}", snapshot_path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("parse {}", snapshot_path.display()))?;
    let lead_id = v
        .get("lead_session_id")
        .and_then(|s| s.as_str())
        .map(String::from);
    match lead_id {
        Some(id) if !id.is_empty() => Ok(Some(id)),
        _ => bail!(
            "project `{slug}` team-snapshot.json has no `lead_session_id` yet.\n  \
             The lead session may have failed to spawn. Check `ccteam show {slug}` and\n  \
             `cat {}` for diagnostics.",
            snapshot_path.display(),
        ),
    }
}

/// Walk `progress.jsonl` newest-first, find the most recent
/// `agent_spawn` whose state.json still reports a live bg job (state
/// ∈ {working}). Returns the `daemonShort` id Claude assigned.
fn latest_claude_bg_job_id(paths: &CcteamPaths, slug: &str) -> Option<String> {
    let progress = paths.progress_jsonl(slug);
    let events = ccteam_core::progress::read_all_events(&progress).ok()?;
    let mut sids: Vec<String> = events
        .iter()
        .filter(|e| e.get("event").and_then(|s| s.as_str()) == Some("agent_spawn"))
        .filter_map(|e| {
            e.get("session_id")
                .and_then(|s| s.as_str())
                .map(String::from)
        })
        .collect();
    sids.reverse();
    // For each candidate, look in ~/.claude/jobs/*/state.json with
    // matching state.json sessionId — too expensive. The simpler
    // approach: walk ~/.claude/jobs/<short>/state.json and find the
    // newest whose `cwd` matches the project dir.
    let _ = sids;
    let project_dir = paths.project_dir(slug);
    let canon_cwd = std::fs::canonicalize(&project_dir).ok()?;

    let jobs_dir = std::env::var_os("CCTEAM_CLAUDE_JOBS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("jobs")
        });
    let read = std::fs::read_dir(&jobs_dir).ok()?;
    let mut candidates: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in read.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let state_path = entry.path().join("state.json");
        let Ok(body) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
            continue;
        };
        let cwd = v.get("cwd").and_then(|s| s.as_str()).unwrap_or("");
        let Ok(canon) = std::fs::canonicalize(cwd) else {
            continue;
        };
        if canon != canon_cwd {
            continue;
        }
        let state = v.get("state").and_then(|s| s.as_str()).unwrap_or("");
        if !matches!(state, "working") {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((mtime, id));
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
    candidates.into_iter().next().map(|(_, id)| id)
}

/// `ccteam peek <slug>`. Returns the contents of the session's first
/// pane via `tmux capture-pane -p`.
///
/// Routes through `ccteam_core::capture_pane_tail_from_session`
/// (re-exported over `ccteam_harness::tmux_ops::capture_pane_tail_from_session`,
/// the same primitive `TmuxBackend::capture` calls under the hood).
/// Keeps the peek path sync (sync sites stay sync).
///
/// `ccteam internal peek <slug> [sid]`. Resolves a live chat session
/// (`ccteam-chat-<slug>-<sid>`) first — mirroring `attach` — and falls
/// back to the project pane (`ccteam-<slug>`) when none matches. This is
/// why a bare `peek <slug>` against a chat session used to fail with
/// "rmux session not running: ccteam-<slug>" while `attach` worked.
///
/// The optional disambiguator is the session **sid** (`s<N>`), not a
/// role (the pane's trailing segment is the sid).
pub fn run_peek_with_role(
    paths: &CcteamPaths,
    slug_or_name: &str,
    sid: Option<&str>,
) -> Result<String> {
    let session_name = match resolve_chat_session_name(slug_or_name, sid)? {
        Some(name) => name,
        None => session_name_for_project(paths, slug_or_name),
    };
    peek_session_by_name(&session_name)
}

/// Capture a 1000-line plain-text tail of a session pane by its exact
/// tmux/rmux name (chat or project — the caller already resolved it).
fn peek_session_by_name(session_name: &str) -> Result<String> {
    // V0.8 W5 — backend-aware peek. Under the rmux backend, capture is
    // non-interactive (a plain-text grid snapshot) so it fits the async
    // `ProcessBackend::capture` trait method cleanly; drive it on a
    // current-thread tokio runtime.
    // The tmux path (opt-out) is unchanged.
    if ccteam_harness::backend_kind_from_env() == ccteam_harness::BackendKind::Rmux {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for rmux peek")?;
        let id = ccteam_harness::MuxSessionId::new(session_name.to_string());
        let backend = ccteam_harness::from_env()?;
        let bytes = runtime
            .block_on(async {
                if !backend.exists(&id).await? {
                    bail!("rmux-сессия не запущена: {}", id);
                }
                // 1000-line tail mirrors the tmux default scroll-region
                // window; `with_ansi=false` returns stripped plain text.
                backend.capture(&id, 1000, false).await
            })
            .context("получить вывод rmux")?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }

    let session = TmuxSession::from_name(session_name.to_string());
    if !session.exists() {
        bail!("tmux-сессия не запущена: {}", session.name());
    }
    // 1000-line tail matches the legacy raw `capture-pane -p` default
    // window (no `-S` flag → tmux's default scroll-region tail).
    let text = ccteam_core::capture_pane_tail_from_session(session.name(), 1000, false)
        .unwrap_or_default();
    Ok(text)
}

/// `ccteam progress <slug>`. With `tail = false`, returns the entire
/// progress.jsonl as text. With `tail = true`, reads + writes to
/// stdout in a polling loop until Ctrl-C.
pub fn run_progress(paths: &CcteamPaths, slug: &str, tail: bool) -> Result<()> {
    use std::io::Write;
    let path = paths.progress_jsonl(slug);
    if !path.exists() {
        bail!("progress.jsonl для {slug} пока нет: {}", path.display());
    }
    let mut stdout = std::io::stdout().lock();
    let initial =
        std::fs::read_to_string(&path).with_context(|| format!("прочитать {}", path.display()))?;
    stdout.write_all(initial.as_bytes())?;
    if !tail {
        return Ok(());
    }
    let mut seen = initial.len() as u64;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() <= seen {
            continue;
        }
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(seen))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        stdout.write_all(&buf)?;
        stdout.flush()?;
        seen = meta.len();
    }
}

/// `ccteam doctor` flags. The bare invocation runs the full readiness
/// checkup (`doctor::run_readiness_checkup`); `--verify-mcp` is the one
/// CI-oriented invariant (the MCP tool-surface / STUB self-check). The
/// hidden fossil migration/repair flags from older ccteam versions were
/// removed — pre-v1.0 = no back-compat shims. (Setup actions — MCP
/// register, IM token, prefs — live in `ccteam config`.)
#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    /// Assert the MCP tool surface is fully wired. `main::run_doctor`
    /// short-circuits to `run_verify_mcp`, which counts active + STUB
    /// tools (`mcp_tool_groups::STUB_TOOLS`) and exits 1 when any STUB is
    /// found.
    pub verify_mcp: bool,
    /// Pair with `verify_mcp = true` to emit a single pretty-printed
    /// JSON object on stdout instead of the human-friendly text report.
    /// Ignored when `verify_mcp == false`.
    pub verify_mcp_json: bool,
    /// Repair corrupt progress journals after preserving byte-exact backups.
    pub repair_progress: bool,
}

fn render_install_mcp_report() -> Result<String> {
    // ccteam is a pure CLI (not a vendor plugin), so `ccteam config` is the
    // MCP installer for ALL vendors (v0.9.3 symmetry — any vendor's main
    // session can orchestrate): Claude (`~/.claude.json`), Codex
    // (`~/.codex/config.toml`), Grok (`~/.grok/config.toml`), OpenCode
    // (`~/.config/opencode/opencode.json`), Kimi (`~/.kimi-code/mcp.json`).
    let claude_path = crate::mcp_serve::install_mcp()?;
    let codex_path = crate::mcp_serve::install_codex_mcp()?;
    let grok_path = crate::mcp_serve::install_grok_mcp()?;
    let opencode_path = crate::mcp_serve::install_opencode_mcp()?;
    let kimi_path = crate::mcp_serve::install_kimi_mcp()?;
    Ok(render_install_mcp_body(
        &claude_path,
        &codex_path,
        &grok_path,
        &opencode_path,
        &kimi_path,
    ))
}

/// Pure renderer for the `config mcp` report body, split out from the
/// vendor-config writes so it stays unit-testable without touching the real
/// configs. The `tools surface` line interpolates the live tool count from
/// the same `tool_definitions()` source `run_verify_mcp` introspects — never
/// hard-code it, or the number drifts (it was stuck at "9" while the surface
/// grew to 27).
fn render_install_mcp_body(
    claude_path: &std::path::Path,
    codex_path: &std::path::Path,
    grok_path: &std::path::Path,
    opencode_path: &std::path::Path,
    kimi_path: &std::path::Path,
) -> String {
    let mut out = String::from(
        "ccteam config: регистрация MCP-сервера (Claude + Codex + Grok + OpenCode + Kimi)\n\n",
    );
    out.push_str(&format!(
        "  MCP-сервер ccteam зарегистрирован для Claude   в {}\n",
        claude_path.display()
    ));
    out.push_str(&format!(
        "  MCP-сервер ccteam зарегистрирован для Codex    в {}\n",
        codex_path.display()
    ));
    out.push_str(&format!(
        "  MCP-сервер ccteam зарегистрирован для Grok     в {}\n",
        grok_path.display()
    ));
    out.push_str(&format!(
        "  MCP-сервер ccteam зарегистрирован для OpenCode в {}\n",
        opencode_path.display()
    ));
    out.push_str(&format!(
        "  MCP-сервер ccteam зарегистрирован для Kimi     в {}\n",
        kimi_path.display()
    ));
    for spec in ccteam_core::host_registry::AGENT_PROBE_SPECS
        .iter()
        .filter(|spec| !spec.tool_surface.uses_native_mcp_config())
    {
        if let Some(notice) = spec.tool_surface_notice() {
            out.push_str(&format!(
                "  конфигурация {} не изменена: {notice}\n",
                spec.vendor
            ));
        }
    }
    let total_tools = run_verify_mcp().total_tools;
    out.push_str(&format!("  поверхность инструментов: {total_tools}\n"));
    out.push('\n');
    out.push_str(
        "откройте новую сессию claude / codex, чтобы применить изменение; существующим сессиям claude нужен /reload-mcp.\n",
    );
    out
}

/// Outcome of `ccteam doctor --verify-mcp`. Counts the MCP tool surface
/// registered by `mcp_serve::tool_definitions()` and cross-checks against
/// the `STUB_TOOLS` allow-list declared in `mcp_tool_groups`.
/// `unexpected_stubs` is the set difference between live STUBs and the
/// allow-list (today the allow-list is empty so any STUB is unexpected);
/// `ok()` returns false when that set is non-empty.
#[derive(Debug, Clone)]
pub struct VerifyMcpReport {
    /// Total number of MCP tools registered by `tool_definitions()`.
    pub total_tools: usize,
    /// Number of tools classified as STUB (name appears in
    /// `mcp_tool_groups::STUB_TOOLS`).
    pub stub_count: usize,
    /// Number of tools with a real dispatch (= total - stub_count).
    pub active_count: usize,
    /// Sorted, full tool names (e.g. `session_spawn`) for the
    /// human-readable + JSON reports.
    pub tool_list: Vec<String>,
    /// Per-group counts (`workflow` → 15, `chat` → 8, ...). Sorted by
    /// group name for deterministic output.
    pub per_group: std::collections::BTreeMap<String, GroupStats>,
    /// STUB tool names that are NOT in the `STUB_TOOLS` allow-list.
    /// Empty in a clean build; non-empty → exit code 1.
    pub unexpected_stubs: Vec<String>,
}

/// Per-group active/stub split used by `VerifyMcpReport`.
#[derive(Debug, Clone)]
pub struct GroupStats {
    pub active: usize,
    pub stub: usize,
}

impl VerifyMcpReport {
    /// True when every registered tool has a real dispatch — i.e. the
    /// allow-list and the live STUB set agree. CI uses this to decide
    /// the exit code (`true` → 0, `false` → 1).
    pub fn ok(&self) -> bool {
        self.unexpected_stubs.is_empty()
    }

    /// Human-readable report (default). Mirrors the layout in
    /// `docs/versions/v0-6-6/prd.md` §F171 Sub-3.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("Проверка поверхности MCP-инструментов (V0.6.6 F171)\n");
        out.push_str("===\n");
        out.push_str(&format!("всего инструментов: {}\n", self.total_tools));
        out.push_str(&format!("активных:          {}\n", self.active_count));
        out.push_str(&format!("заглушек:          {}\n", self.stub_count));
        out.push_str("\nразбивка по группам:\n");
        for (group, stats) in &self.per_group {
            out.push_str(&format!(
                "  {:<12} {} активных / {} заглушек\n",
                format!("{group}:"),
                stats.active,
                stats.stub
            ));
        }
        if !self.unexpected_stubs.is_empty() {
            out.push_str("\nнеожиданные STUB (не входят в mcp_tool_groups::STUB_TOOLS):\n");
            for name in &self.unexpected_stubs {
                out.push_str(&format!("  - {name}\n"));
            }
        }
        out.push('\n');
        if self.ok() {
            out.push_str(&format!(
                "вердикт: PASS — все {} инструментов активны, production STUB нет.\n",
                self.total_tools
            ));
        } else {
            out.push_str(&format!(
                "вердикт: FAIL — зарегистрировано неожиданных STUB: {}.\n",
                self.unexpected_stubs.len()
            ));
        }
        out
    }

    /// Single pretty-printed JSON object (trailing newline) for
    /// machine-readable callers (CI, `jq`-driven scripts). Hand-built
    /// via `serde_json::json!` so the report type does not need a
    /// `serde::Serialize` derive (ccteam-cli does not depend on the
    /// `serde` crate directly).
    pub fn render_json(&self) -> String {
        let per_group: Map<String, Value> = self
            .per_group
            .iter()
            .map(|(g, s)| (g.clone(), json!({ "active": s.active, "stub": s.stub })))
            .collect();
        let body = json!({
            "ok": self.ok(),
            "total_tools": self.total_tools,
            "active_count": self.active_count,
            "stub_count": self.stub_count,
            "tool_list": self.tool_list,
            "per_group": Value::Object(per_group),
            "unexpected_stubs": self.unexpected_stubs,
        });
        let mut s = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into());
        s.push('\n');
        s
    }
}

/// Compute the report by introspecting
/// `ccteam_im::mcp::tool_definitions()` (single source of truth for the
/// registered MCP tool surface) and cross-checking against
/// `ccteam_im::mcp::STUB_TOOLS`.
pub fn run_verify_mcp() -> VerifyMcpReport {
    // SoT after v0.9 T3: `ccteam_im::mcp::tool_definitions` (via thin cli wrap).
    let tools = crate::mcp_serve::tool_definitions();
    let mut names: Vec<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(String::from))
        .collect();
    names.sort();

    let stub_set: std::collections::HashSet<&str> =
        crate::mcp_tool_groups::STUB_TOOLS.iter().copied().collect();

    let stub_count = names
        .iter()
        .filter(|n| stub_set.contains(n.as_str()))
        .count();
    let active_count = names.len().saturating_sub(stub_count);

    // Per-group split. Tools whose `group_for_tool` returns `None` are
    // bucketed under "other" so a typo in a new tool name still shows
    // up in the report rather than silently disappearing.
    let mut per_group: std::collections::BTreeMap<String, GroupStats> =
        std::collections::BTreeMap::new();
    for name in &names {
        let group = match crate::mcp_tool_groups::group_for_tool(name) {
            Some(g) => g.as_str().to_string(),
            None => "other".to_string(),
        };
        let entry = per_group
            .entry(group)
            .or_insert(GroupStats { active: 0, stub: 0 });
        if stub_set.contains(name.as_str()) {
            entry.stub += 1;
        } else {
            entry.active += 1;
        }
    }

    // Unexpected STUBs = live STUBs not in the allow-list. Today the
    // allow-list is empty so this equals the full live STUB set; the
    // indirection lets a future PR park a known-stub under the
    // allow-list without forcing CI red.
    let unexpected_stubs: Vec<String> = names
        .iter()
        .filter(|n| stub_set.contains(n.as_str()))
        .cloned()
        .collect();

    VerifyMcpReport {
        total_tools: names.len(),
        stub_count,
        active_count,
        tool_list: names,
        per_group,
        unexpected_stubs,
    }
}

/// Collect every `.md` artifact under `<project>/.ccteam/` so non-dev
/// teams (e.g. product-research with `verdict.md` / `rationale.md` /
/// `next-steps.md` / `brief.md` / `market-survey.md`) get listed in
/// `ccteam show <slug> --format json` without ccteam-cli holding a
/// per-team artifact whitelist (F8 fix, 2026-05-07).
///
/// Key = file stem with `-` → `_` (e.g. `plan-eng.md` → `plan_eng`)
/// so existing JSON consumers (the meta-agent dispatch tree, the
/// ccteam-control skill) keep working without a schema migration.
/// Sub-directories under `.ccteam/` (e.g. `outbox/`, `inbox/`) are
/// not enumerated — those have dedicated views.
///
/// `auto-loop.state.md` is the orchestrator's runtime state file
/// (formerly `fix-loop.state.md`); excluded from artifact reporting.
fn collect_artifacts(paths: &CcteamPaths, slug: &str) -> Map<String, Value> {
    let mut m = Map::new();
    let ccteam_dir = paths.project_ccteam_dir(slug);
    let Ok(entries) = std::fs::read_dir(&ccteam_dir) else {
        return m;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".md") {
            continue;
        }
        // Skip orchestrator-internal runtime state files.
        if name == "auto-loop.state.md" || name == "fix-loop.state.md" {
            continue;
        }
        let stem = name.trim_end_matches(".md");
        let key = stem.replace('-', "_");
        m.insert(key, Value::String(format!(".ccteam/{name}")));
    }
    m
}

fn render_ls_text(paths: &CcteamPaths, projects: &[ProjectSummary], daemon_up: bool) -> String {
    let mut out = String::new();
    // F27 — daemon health one-liner, always emitted (even on the
    // empty-projects path) so users can disambiguate "no projects" from
    // "daemon never came up".
    out.push_str(&format!(
        "демон: {}\n",
        if daemon_up {
            "работает"
        } else {
            "не работает"
        }
    ));
    if projects.is_empty() {
        out.push_str(
            "(в ~/projects/ нет проектов. Создайте: `ccteam project new <slug>`, например `ccteam project new demo`.)\n",
        );
        return out;
    }
    out.push_str("SLUG                                     COST(24H)   ВОЗРАСТ\n");
    for p in projects {
        // V0.4.6 F91 — cost column sources cost_24h_usd from
        // progress.jsonl (best-effort; failure → $0.00 — fresh
        // projects with no progress events show $0.00, same shape
        // as pre-F91 `state.cost_used_usd == 0.0`).
        let cost_24h = cost_summary(&p.state.slug, &paths.progress_jsonl(&p.state.slug), paths)
            .map(|c| c.cost_24h_usd)
            .unwrap_or(0.0);
        out.push_str(&format!(
            "{:<40} ${:<10.2} {}s\n",
            truncate(&p.state.slug, 40),
            cost_24h,
            p.age_seconds,
        ));
    }
    out
}

/// `current_phase` is empty between project creation and the first dispatch;
/// surface that as `pending` in the detailed `show` view.
fn display_phase(phase: &str) -> &str {
    if phase.is_empty() {
        "pending"
    } else {
        phase
    }
}

fn render_ls_json(
    paths: &CcteamPaths,
    projects: &[ProjectSummary],
    daemon_up: bool,
) -> Result<String> {
    let active_count = 0usize;
    let arr: Vec<Value> = projects
        .iter()
        .map(|p| {
            // V0.4.6 F91 — JSON shape preserves the `cost_used_usd`
            // key for callers (MCP / scripts) but populates it from
            // `cost_24h_usd` so the number tracks reality. The legacy
            // serde field still reads as the frozen pre-F91 value if
            // anything in the JSON pipeline needs to differentiate.
            let cost_summary =
                cost_summary(&p.state.slug, &paths.progress_jsonl(&p.state.slug), paths)
                    .unwrap_or_default();
            let last_event =
                ccteam_core::progress::last_event(&paths.progress_jsonl(&p.state.slug))
                    .ok()
                    .flatten();
            let stall = stall_level(last_event.as_ref(), p.stall_silent_seconds);
            json!({
                "slug": p.state.slug,
                "cost_used_usd": cost_summary.cost_24h_usd,
                "cost_24h_usd": cost_summary.cost_24h_usd,
                "cost_active_usd": cost_summary.cost_active_usd,
                "cost_total_usd": cost_summary.cost_total_usd,
                "context_tokens_used": p.state.context_tokens_used,
                "user_attached": p.state.user_attached,
                "age_seconds": p.age_seconds,
                "last_event_ts": p
                    .state
                    .last_progress_event_at
                    .map(|t| t.to_rfc3339()),
                "stall_level": stall,
            })
        })
        .collect();
    let v = json!({
        "projects": arr,
        // `running` is a real bool driven by MCP socket reachability so
        // meta-agent / MCP consumers can gate writes on daemon liveness
        // without trusting a stale side file.
        "orchestrator": {
            "running": daemon_up,
            "active_count": active_count,
            "max_concurrent": 1,
        }
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

fn parse_rfc3339_age_secs(ts: &str) -> Option<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    let now = chrono::Utc::now();
    let secs = now
        .signed_duration_since(dt.with_timezone(&chrono::Utc))
        .num_seconds();
    if secs < 0 {
        Some(0)
    } else {
        Some(secs as u64)
    }
}

fn humanize_secs_local(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

fn render_show_text(
    state: &ProjectState,
    cost: &ccteam_core::CostSummary,
    recent: &[Value],
    artifacts: &Map<String, Value>,
    sessions: &[ccteam_core::ActiveSessionInfo],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} ({})\n\n", state.slug, state.tmux_session));
    out.push_str(&format!(
        "текущая фаза   : {}\n",
        display_phase(&state.current_phase)
    ));
    // V0.4.6 F91 — cost(24h) sums every `agent_done.cost_usd` in the
    // last 24h; cost(active) live-reads each open session's
    // `~/.claude/jobs/<id>/state.json::cost_usd_total`. The pre-F91
    // `cost used: $X.XX` line (sourced from the now-frozen
    // `state.cost_used_usd`) is removed.
    out.push_str(&format!(
        "стоимость (24ч): ${:.2}  ({} сессий)\n",
        cost.cost_24h_usd, cost.session_count_24h,
    ));
    out.push_str(&format!(
        "стоимость (активные): ${:.2}  ({} запущено)\n",
        cost.cost_active_usd, cost.session_count_active,
    ));
    out.push_str(&format!("стоимость (всего): ${:.2}\n", cost.cost_total_usd));
    out.push_str(&format!(
        "токены контекста: {} (сбросов: {})\n",
        state.context_tokens_used, state.context_reset_count
    ));
    out.push_str(&format!(
        "цикл исправления: {}\n",
        state.auto_loop_cycle_count
    ));
    out.push_str("\nистория фаз:\n");
    if state.phase_history.is_empty() {
        out.push_str("  (пусто)\n");
    } else {
        for h in &state.phase_history {
            out.push_str(&format!("  - {} ({})\n", h.phase, h.status));
        }
    }
    out.push_str("\nартефакты:\n");
    if artifacts.is_empty() {
        out.push_str("  (пока нет)\n");
    } else {
        for (k, v) in artifacts {
            out.push_str(&format!("  {:<18} {}\n", k, v.as_str().unwrap_or("<?>")));
        }
    }

    out.push_str(&format!("\nактивные сессии ({}):\n", sessions.len()));
    if sessions.is_empty() {
        out.push_str("  (запущенных нет)\n");
    } else {
        for s in sessions {
            let model = s.model.as_deref().unwrap_or("—");
            let ctx = s
                .context_remaining_pct
                .map(|p| format!("контекст {:>3.0}%", p))
                .unwrap_or_else(|| "контекст —".into());
            let age = parse_rfc3339_age_secs(&s.started_at)
                .map(humanize_secs_local)
                .unwrap_or_else(|| "?".into());
            let short_job = s
                .job_id
                .as_deref()
                .map(|j| j.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "  {:<10}  {:<8}  {:<22}  {}  ${:>6.2}  {} назад\n",
                s.role, short_job, model, ctx, s.cost_usd, age
            ));
        }
        out.push_str("\n  подсказка: `claude attach <id>` для подключения к активной сессии\n");
    }

    out.push_str(&format!("\nпоследние события ({}):\n", recent.len()));
    for e in recent {
        let ts = e.get("ts").and_then(|s| s.as_str()).unwrap_or("???");
        let event = e.get("event").and_then(|s| s.as_str()).unwrap_or("?");
        out.push_str(&format!("  {ts}  {event}\n"));
    }
    out
}

fn render_show_json(
    state: &ProjectState,
    cost: &ccteam_core::CostSummary,
    recent: &[Value],
    artifacts: &Map<String, Value>,
    sessions: &[ccteam_core::ActiveSessionInfo],
) -> Result<String> {
    let v = json!({
        "state": serde_json::to_value(state)?,
        "phase_history": serde_json::to_value(&state.phase_history)?,
        "cost": serde_json::to_value(cost)?,
        "recent_events": recent,
        "artifacts": Value::Object(artifacts.clone()),
        "active_sessions": serde_json::to_value(sessions)?,
        "stall": {
            "level": "ok",
            "silent_seconds": 0,
        },
        "recommendations": Value::Array(Vec::new()),
    });
    Ok(serde_json::to_string_pretty(&v)?)
}

pub(crate) fn stall_level(last_event: Option<&Value>, silent_s: u64) -> &'static str {
    ccteam_core::stall::classify_progress_stall(last_event, silent_s).level
}

/// Collapse the `stall_level` tiers into the short verdict word the
/// `ccteam status` project rows print, so the operator reads "STUCK"
/// instead of decoding raw silence seconds. `escalate`/`suspicious`
/// both surface as `STUCK` (silent long enough to warrant a takeover),
/// `warn` stays `warn`, everything else is `OK`.
#[cfg(test)]
pub(crate) fn stall_verdict(last_event: Option<&Value>, silent_s: u64) -> &'static str {
    ccteam_core::stall::classify_progress_stall(last_event, silent_s).verdict
}

/// One terse attention line for a stuck/stale session: the fact plus the
/// direct web-chat path (no repeated boilerplate — the `attention:` section
/// header carries the "go look" meaning). Idle sessions never earn one
/// (resume-by-sid: idle is the normal resting state).
pub(crate) fn stall_takeover_hint_for_session(
    slug: &str,
    sid: &str,
    activity: &str,
    silent: &str,
) -> String {
    format!("{slug} {sid} {activity} {silent} → /chat/s/{sid}")
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}

/// Knobs forwarded by the `ccteam web` clap struct → axum scaffold.
/// Mirrors `ccteam_web::ServeOpts` 1:1 except `bind` is still a string
/// here (parsed in `run_web`).
#[derive(Debug, Clone)]
pub struct WebOptions {
    pub bind: String,
    pub dsh_bind: Option<String>,
    pub no_auth: bool,
    pub token_file: Option<std::path::PathBuf>,
}

/// `ccteam web --bind <addr>` entry. Translates clap-side string +
/// flags into `ccteam_web::ServeOpts`, then drives a current-thread
/// tokio runtime to `serve(opts)` — the same one-runtime-per-command shape
/// every other long-running `internal` subcommand uses.
pub fn run_web(opts: WebOptions) -> Result<()> {
    use std::net::SocketAddr;

    let bind: SocketAddr = opts
        .bind
        .parse()
        .with_context(|| format!("разобрать --bind `{}` как SocketAddr", opts.bind))?;
    let dsh_web_bind = match opts.dsh_bind.as_deref() {
        Some(value) if value.eq_ignore_ascii_case("off") => None,
        Some(value) => Some(
            value
                .parse()
                .with_context(|| format!("разобрать --dsh-web-bind `{value}` как SocketAddr"))?,
        ),
        None => Some(SocketAddr::new(
            bind.ip(),
            bind.port()
                .checked_add(1)
                .context("вывести --dsh-web-bind из порта --bind")?,
        )),
    };
    let serve_opts = ccteam_web::ServeOpts {
        bind,
        no_auth: opts.no_auth,
        token_file: opts.token_file,
        dsh_web_bind,
        // Production CLI path keeps the 5 s Ctrl-C window so an
        // operator who passes `--no-auth` on a non-loopback bind has
        // a chance to abort before the LAN-RCE surface goes live.
        no_auth_grace_secs: Some(5),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for ccteam web")?;
    runtime.block_on(ccteam_web::serve(serve_opts))
}

/// Options for `ccteam remove <slug>`.
#[derive(Debug, Clone, Default)]
pub struct RemoveOptions {
    /// Also `rm -rf <project>/.ccteam/`, `<project>/.claude/agents/`,
    /// and `<project>/workflow.yaml`. Business code + `.env` untouched.
    pub purge: bool,
    /// Print every step that *would* run, but don't touch filesystem
    /// / config / daemon.
    pub dry_run: bool,
    /// Skip the CLAUDE.md §三 "永不主动 kill 长 session" refusal gate.
    pub force: bool,
}

/// Structured result of `run_remove`. Returned so MCP callers (a
/// future `tool_remove` wire) can branch on the success shape; the CLI
/// just `Display`s the text rendering.
#[derive(Debug, Clone, Default)]
pub struct RemoveReport {
    pub slug: String,
    pub purge: bool,
    pub dry_run: bool,
    pub forced: bool,
    /// One-line entries describing each step that ran (or would run
    /// under `--dry-run`). Surface order matches execution order so
    /// users see the same shape with and without `--dry-run`.
    pub steps: Vec<String>,
    /// Set when the refusal gate fired (and `--force` was not passed).
    /// `--dry-run` still reports the refusal so users can rehearse.
    pub refusal: Option<ccteam_core::ActiveSessionRefusal>,
    /// Post-ACK cleanup steps that could not complete. The removal still ran to
    /// its commit point (never strand a retired generation in the catalog), so
    /// these are surfaced as warnings the operator must clean up by hand.
    pub cleanup_failures: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DaemonProjectRetire {
    slug: String,
    sessions_stopped: Vec<String>,
    progress_removed: Vec<String>,
}

/// Retire one project generation through the daemon's same-uid admin bus.
///
/// This is deliberately fail-closed: config and project-local state stay put
/// unless the daemon confirms that the durable tombstone was written and all
/// live producers were joined before progress cleanup.
fn request_daemon_project_retire(paths: &CcteamPaths, slug: &str) -> Result<DaemonProjectRetire> {
    let admin_token = std::fs::read_to_string(paths.web_token_path())
        .with_context(|| format!("прочитать {}", paths.web_token_path().display()))?;
    let admin_token = admin_token.trim();
    if admin_token.is_empty() {
        bail!(
            "пустой admin token в {}; запустите daemon, чтобы восстановить локальный control plane",
            paths.web_token_path().display()
        );
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "ccteam/project-retire",
        "params": {
            "arguments": {
                "slug": slug,
                "_caller_admin_token": admin_token,
            }
        }
    });
    let socket = ccteam_core::daemon_socket_path(paths);
    let response = block_on_async(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(65),
            crate::mcp_serve::forward_to_socket(&socket, &request),
        )
        .await
    })?
    .map_err(|_| {
        // The daemon may have committed the durable tombstone and simply be
        // slow to answer, so this must NOT claim the project survived.
        anyhow::anyhow!(
            "daemon не подтвердил retirement проекта `{slug}` за 65 с; состояние retirement \
             неизвестно (проект уже мог стать необратимо retired); проверьте daemon и повторите \
             `ccteam project rm {slug}`"
        )
    })?
    .map_err(|error| {
        // `forward_to_socket` fails both when the socket cannot be reached AND
        // when the daemon accepted the request and died before answering
        // ("mcp.sock closed before responding"). `mark_progress_retired` is the
        // daemon's FIRST durable act, so this branch must not claim the project
        // survived — the generation may already be permanently retired.
        anyhow::anyhow!(
            "связь с daemon по retirement проекта `{slug}` оборвалась: {error:#}; \
             состояние retirement неизвестно (проект уже мог стать необратимо retired); \
             config не изменён; повторите `ccteam project rm {slug}`"
        )
    })?;

    if let Some(error) = response.get("error").filter(|value| !value.is_null()) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        // Wire contract with the daemon (`ccteam/project-retire`): a failure
        // that already wrote the durable tombstone reports it as
        // `error.data.marker_committed`. An absent field means "not committed"
        // — the conservative shape for a daemon that failed before the marker.
        let marker_committed = error
            .get("data")
            .and_then(|data| data.get("marker_committed"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if marker_committed {
            bail!(
                "проект `{slug}` уже необратимо retired, но retirement не завершён: {message}; \
                 config не изменён; повторите `ccteam project rm {slug}`"
            );
        }
        bail!("daemon отклонил retirement проекта `{slug}`: {message}; config не изменён");
    }
    let result = response
        .get("result")
        .cloned()
        .context("daemon вернул retirement-ответ без `result`")?;
    let outcome: DaemonProjectRetire =
        serde_json::from_value(result).context("разобрать retirement-ответ daemon")?;
    if outcome.slug != slug {
        bail!(
            "daemon подтвердил retirement чужого проекта `{}` вместо `{slug}`; config не изменён",
            outcome.slug
        );
    }
    Ok(outcome)
}

impl std::fmt::Display for RemoveReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = if self.dry_run { "[dry-run] " } else { "" };
        writeln!(
            f,
            "ccteam remove {}{}{}{}",
            mode,
            self.slug,
            if self.purge { " --purge" } else { "" },
            if self.forced { " --force" } else { "" }
        )?;
        for step in &self.steps {
            writeln!(f, "  - {step}")?;
        }
        if let Some(refusal) = &self.refusal {
            writeln!(f, "refusal: {}", refusal.message(&self.slug))?;
        }
        for failure in &self.cleanup_failures {
            writeln!(f, "warning: {failure}")?;
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------
// V0.6.0 Wave 3 F112 §C — `ccteam prefs` admin surface.
//
// Reads / writes `~/.ccteam/preferences.toml`. Today only the
// fallback section has user-visible keys; V0.7+ can add more by
// extending the parse_key match arm + emitting a friendly diagnostic
// for unknown keys.
// -------------------------------------------------------------------------

/// Format the active preferences for `ccteam prefs show`. Includes
/// the resolved file path so the user knows what was loaded.
pub fn run_prefs_show(paths: &CcteamPaths) -> Result<String> {
    let prefs = ccteam_core::preferences::load_or_default(&paths.root);
    let path = ccteam_core::preferences::preferences_path(&paths.root);
    let exists_marker = if path.exists() {
        "loaded"
    } else {
        "defaults (file not present)"
    };
    let body = toml::to_string_pretty(&prefs).context("serialize preferences for display")?;
    Ok(format!(
        "# ccteam preferences ({exists_marker})\n# path: {}\n\n{body}",
        path.display()
    ))
}

/// Look up a dotted preference key. Returns the textual value or an
/// error if the key is unknown.
pub fn run_prefs_get(paths: &CcteamPaths, key: &str) -> Result<String> {
    let prefs = ccteam_core::preferences::load_or_default(&paths.root);
    match key {
        "fallback.on_claude_quota" => Ok(match prefs.fallback.on_claude_quota {
            ccteam_core::preferences::OnClaudeQuota::Off => "off".to_string(),
            ccteam_core::preferences::OnClaudeQuota::Codex => "codex".to_string(),
        }),
        "fallback.codex.enabled_for_roles" => Ok(prefs.fallback.codex.enabled_for_roles.join(",")),
        other => Err(anyhow::anyhow!(
            "unknown preference key: {other}\n\
             supported keys:\n  - fallback.on_claude_quota  (off|codex)\n  \
             - fallback.codex.enabled_for_roles  (comma list; empty = all roles)"
        )),
    }
}

/// Persist one preference change to `~/.ccteam/preferences.toml`.
/// Returns a one-line confirmation suitable for stdout.
pub fn run_prefs_set(paths: &CcteamPaths, key: &str, value: &str) -> Result<String> {
    let mut prefs = ccteam_core::preferences::load_or_default(&paths.root);
    match key {
        "fallback.on_claude_quota" => {
            prefs.fallback.on_claude_quota = match value.trim().to_lowercase().as_str() {
                "off" => ccteam_core::preferences::OnClaudeQuota::Off,
                "codex" => ccteam_core::preferences::OnClaudeQuota::Codex,
                other => {
                    return Err(anyhow::anyhow!(
                        "fallback.on_claude_quota: неподдерживаемое значение {other:?} \
                         (ожидалось `off` или `codex`)"
                    ));
                }
            };
        }
        "fallback.codex.enabled_for_roles" => {
            prefs.fallback.codex.enabled_for_roles = if value.trim().is_empty() {
                Vec::new()
            } else {
                value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
        }
        other => {
            return Err(anyhow::anyhow!(
                "неизвестный ключ предпочтения: {other}\n\
                 поддерживаемые ключи:\n  - fallback.on_claude_quota  (off|codex)\n  \
                 - fallback.codex.enabled_for_roles  (список через запятую; пусто = все роли)"
            ));
        }
    }
    ccteam_core::preferences::save(&paths.root, &prefs)?;
    Ok(format!("задано {key} = {value}"))
}

// -------------------------------------------------------------------------
// v0.8.6 Item 4 — `ccteam config` setup hub.
//
// `config` is the single setup entrypoint a fresh host runs after
// `ccteam init`. It absorbs three formerly-scattered setup actions:
//   (a) register/refresh the ccteam MCP server (was `doctor --install-mcp`),
//   (b) set the IM (Telegram) bot token (was the `ccteam-im-setup` skill;
//       backed by `ccteam_im::onboarding::telegram_setup`),
//   (c) read/write preferences (the `prefs` get/set/show backend).
//
// Bare `ccteam config` opens a thin numbered-choice interactive menu;
// each menu item dispatches to the SAME action fn the non-interactive
// path calls, so the logic stays testable without a TTY. The
// non-interactive form (`config <key> <value>` / `config get <key>` /
// `config show`) is the headless/CI surface and wraps the prefs backend.
// `preferences.toml` remains the store for the key/value knobs.
// -------------------------------------------------------------------------

/// Number of seconds the IM-token flow long-polls Telegram's
/// `getUpdates` for the first incoming message (to capture the owner's
/// `chat_id`). Kept short so a non-interactive misfire fails fast.
const CONFIG_IM_POLL_SECONDS: u64 = 60;

/// `config` action (a) — register / refresh `mcpServers.ccteam` in
/// `~/.claude.json`. Thin wrapper over the same writer the retired
/// `doctor --install-mcp` flag used (`render_install_mcp_report`), so the
/// rendered report (incl. the live tool count) is identical.
pub fn run_config_install_mcp() -> Result<String> {
    render_install_mcp_report()
}

/// `config` action (b) — validate a Telegram bot token, long-poll for the
/// owner's first message to capture the `chat_id`, and persist the
/// resulting credentials to `~/.ccteam/secrets/im-credentials.json` (mode 0600).
/// Wraps [`ccteam_im::onboarding::telegram_setup`]; the async call is
/// driven on a one-shot current-thread tokio runtime so the sync CLI /
/// menu path stays runtime-agnostic.
///
/// Returns the stdout body to print on success (bot handle + creds path
/// + a "DM the bot now" hint the caller may have already surfaced).
pub fn run_config_set_im_token(token: &str) -> Result<String> {
    let token = token.trim();
    if token.is_empty() {
        bail!("config: empty Telegram bot token (paste the token from @BotFather)");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for IM token onboarding")?;
    let result = runtime
        .block_on(ccteam_im::onboarding::telegram_setup(
            token,
            CONFIG_IM_POLL_SECONDS,
        ))
        .context("Telegram onboarding (token validation + chat_id capture)")?;

    // Read the display fields out before moving `creds` into the doc.
    let bot_username = result.bot_username;
    let owner = result
        .creds
        .allowed_chat_ids
        .first()
        .cloned()
        .unwrap_or_default();

    // Persist: merge into any existing credentials doc so a prior Slack /
    // Discord / Lark entry survives a Telegram (re)config.
    let creds_path = ccteam_im::credentials::default_path();
    let mut creds = ccteam_im::credentials::load(Some(&creds_path))
        .context("load existing IM credentials before merge")?;
    creds.telegram = Some(result.creds);
    ccteam_im::credentials::save(&creds_path, &creds).context("persist IM credentials")?;

    // Best-effort in-place reload of a running daemon's IM listeners (no
    // restart needed). Down daemon → silently skipped.
    notify_daemon_im_reload();

    Ok(format!(
        "ccteam config: Telegram token saved\n\n  \
         bot           {}\n  \
         owner chat_id {}\n  \
         credentials   {}\n\n\
         `ccteam start` will bring the IM gateway up with these credentials.\n",
        bot_username,
        owner,
        creds_path.display(),
    ))
}

/// `config` action — validate Lark/Feishu app credentials (by fetching a
/// `tenant_access_token`) and persist them to
/// `~/.ccteam/secrets/im-credentials.json` (mode 0600). Mirrors
/// [`run_config_set_im_token`] but for the WS-long-connection Lark
/// provider: there is no `chat_id` to poll — the provider keys its
/// allowlist on operator-supplied `open_id`s.
///
/// `allowed_user_ids` is **fail-closed**: an empty list means the bot
/// answers no one (the opposite of Telegram's empty=open). `use_feishu`
/// selects the region (`true` = Feishu/CN, `false` = Lark international).
pub fn run_config_set_lark_creds(
    app_id: &str,
    app_secret: &str,
    allowed_user_ids: Vec<String>,
    use_feishu: bool,
) -> Result<String> {
    let api_base = if use_feishu {
        ccteam_im::onboarding::FEISHU_API_BASE
    } else {
        ccteam_im::onboarding::LARK_API_BASE
    };
    run_config_set_lark_creds_with_base(
        app_id,
        app_secret,
        allowed_user_ids,
        use_feishu,
        api_base,
        None,
    )
}

/// Test seam for [`run_config_set_lark_creds`]: lets callers override the
/// Lark API base (point a deterministic mock server at it) and the
/// credentials-file path (sandbox `~/.ccteam/secrets/im-credentials.json` into a
/// tempdir). Production callers go through [`run_config_set_lark_creds`],
/// which passes the real region base + the default creds path. Mirrors the
/// `_with_base` convention in `ccteam_im::onboarding`.
pub fn run_config_set_lark_creds_with_base(
    app_id: &str,
    app_secret: &str,
    allowed_user_ids: Vec<String>,
    use_feishu: bool,
    api_base: &str,
    creds_path_override: Option<&std::path::Path>,
) -> Result<String> {
    let app_id = app_id.trim();
    let app_secret = app_secret.trim();
    if app_id.is_empty() || app_secret.is_empty() {
        bail!("config: Lark app_id and app_secret are both required");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime for Lark credential onboarding")?;
    let result = runtime
        .block_on(ccteam_im::onboarding::lark_setup_with_base(
            app_id,
            app_secret,
            allowed_user_ids,
            use_feishu,
            api_base,
        ))
        .context("Lark onboarding (app credential validation via tenant_access_token)")?;

    // Persist: merge into any existing credentials doc so a prior
    // Telegram / Slack / Discord entry survives a Lark (re)config.
    let creds_path = match creds_path_override {
        Some(p) => p.to_path_buf(),
        None => ccteam_im::credentials::default_path(),
    };
    let mut creds = ccteam_im::credentials::load(Some(&creds_path))
        .context("load existing IM credentials before merge")?;
    let allow_count = result.creds.allowed_user_ids.len();
    let region = if result.creds.use_feishu {
        "Feishu (CN, open.feishu.cn)"
    } else {
        "Lark (intl, open.larksuite.com)"
    };
    creds.lark = Some(result.creds);
    ccteam_im::credentials::save(&creds_path, &creds).context("persist IM credentials")?;

    // Best-effort in-place reload of a running daemon's IM listeners (no
    // restart needed). Down daemon → silently skipped. Skipped on the test
    // seam (`creds_path_override` set) so unit tests never poke a live daemon
    // socket from `from_env()`.
    if creds_path_override.is_none() {
        notify_daemon_im_reload();
    }

    let allow_note = if allow_count == 0 {
        "  allowlist     EMPTY — fail-closed: the bot answers NO ONE.\n  \
         add open_ids (ou_…) to allowed_user_ids to let users in.\n"
            .to_string()
    } else {
        format!("  allowlist     {allow_count} open_id(s) allowed\n")
    };

    Ok(format!(
        "ccteam config: Lark/Feishu credentials saved\n\n  \
         app_id        {}\n  \
         region        {}\n\
         {}  \
         credentials   {}\n\n\
         `ccteam start` will bring the IM gateway up with these credentials.\n",
        app_id,
        region,
        allow_note,
        creds_path.display(),
    ))
}

/// Bare `ccteam config` — thin numbered-choice interactive menu. Reads a
/// single digit from stdin and dispatches to the same action fn the
/// non-interactive path uses (so all real work stays in testable fns).
/// On a non-TTY stdin we refuse rather than hang, pointing the operator at
/// the non-interactive forms.
pub fn run_config_menu(paths: &CcteamPaths) -> Result<String> {
    use std::io::{IsTerminal, Write};

    if !std::io::stdin().is_terminal() {
        bail!(
            "ccteam config: интерактивному меню нужен TTY.\n\
             формы для headless:\n  \
             ccteam config show                 # вывести предпочтения\n  \
             ccteam config get <key>            # прочитать одно предпочтение\n  \
             ccteam config <key> <value>        # задать одно предпочтение\n  \
             ccteam doctor --verify-mcp         # проверить подключение MCP\n\
             (Регистрация MCP и настройка IM-токена требуют интерактивного запуска.)"
        );
    }

    println!("ccteam config — настройка\n");
    println!("  1) зарегистрировать / обновить MCP-сервер ccteam (~/.claude.json)");
    println!("  2) задать токен IM-бота (Telegram)");
    println!("  3) задать учётные данные приложения Lark/Feishu");
    println!("  4) показать предпочтения");
    println!("  q) выход");
    print!("\nвыберите [1-4/q]: ");
    std::io::stdout().flush().ok();

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("прочитать выбор меню config из stdin")?;

    match line.trim() {
        "1" => run_config_install_mcp(),
        "2" => {
            print!("вставьте токен Telegram-бота (из @BotFather): ");
            std::io::stdout().flush().ok();
            let mut token = String::new();
            std::io::stdin()
                .read_line(&mut token)
                .context("прочитать токен Telegram из stdin")?;
            println!(
                "проверяю токен и жду до {CONFIG_IM_POLL_SECONDS} с, пока вы напишете боту в ЛС…"
            );
            run_config_set_im_token(&token)
        }
        "3" => run_config_lark_menu(),
        "4" => run_prefs_show(paths),
        "q" | "Q" | "" => Ok("ccteam config: ничего не изменено.\n".to_string()),
        other => bail!("ccteam config: неизвестный выбор {other:?} (ожидалось 1-4 или q)"),
    }
}

/// Interactive prompt for the menu's Lark/Feishu item: collect
/// `app_id` / `app_secret`, the region (Feishu/CN default vs Lark intl),
/// and the optional `open_id` allowlist, then hand off to
/// [`run_config_set_lark_creds`] (which validates + persists). Kept
/// separate from [`run_config_menu`] so the stdin reads stay linear and
/// the persistence logic remains unit-testable without a TTY.
fn run_config_lark_menu() -> Result<String> {
    use std::io::Write;

    fn prompt_line(label: &str) -> Result<String> {
        print!("{label}");
        std::io::stdout().flush().ok();
        let mut buf = String::new();
        std::io::stdin()
            .read_line(&mut buf)
            .with_context(|| format!("прочитать {label:?} из stdin"))?;
        Ok(buf.trim().to_string())
    }

    println!(
        "\nУчётные данные приложения Lark/Feishu (консоль разработчика → приложение → Credentials & Basic Info)."
    );
    let app_id = prompt_line("app_id (cli_…): ")?;
    let app_secret = prompt_line("app_secret: ")?;

    // Region: default Feishu/CN (Enter accepts the default).
    let region = prompt_line("регион — [F]eishu CN (по умолчанию) / [L]ark intl: ")?;
    let use_feishu = !matches!(region.to_ascii_lowercase().as_str(), "l" | "lark" | "intl");

    // Provider allowlist is FAIL-CLOSED — make that loud.
    println!(
        "\nallowed_user_ids = open_ids (ou_…), которым разрешено управлять ботом.\n  \
         FAIL-CLOSED: пустой список означает, что бот НЕ ОТВЕЧАЕТ НИКОМУ\n  \
         (в отличие от Telegram, где пусто = открыто). Используйте `*`, чтобы разрешить всем."
    );
    let allow_raw = prompt_line("разрешённые open_ids (через запятую/пробел или пусто): ")?;
    let allowed_user_ids: Vec<String> = allow_raw
        .split([',', ' ', '\t'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    println!("проверяю учётные данные приложения (получаю tenant_access_token)…");
    run_config_set_lark_creds(&app_id, &app_secret, allowed_user_ids, use_feishu)
}

/// Resolve a `--project <slug>` (or, when `None`, the current working
/// directory canonicalized) to an existing project dir. Used by role
/// commands and project-local skill face commands. A `slug` that isn't a
/// registered project (or a cwd that doesn't exist) is a loud error.
fn resolve_project_dir(paths: &CcteamPaths, slug: Option<&str>) -> Result<std::path::PathBuf> {
    let dir = match slug {
        Some(s) => {
            let d = paths.project_dir(s);
            if !d.exists() {
                bail!("проекта с именем `{s}` нет (проверено в {})", d.display());
            }
            d
        }
        None => std::env::current_dir().context("прочитать cwd как цель --project по умолчанию")?,
    };
    std::fs::canonicalize(&dir)
        .with_context(|| format!("канонизировать каталог проекта `{}`", dir.display()))
}

/// `ccteam role search <q>`. Substring search over the
/// curated ccteam-hub marketplace `index.json` (loaded via the
/// `~/.ccteam/hub-cache/` cache; first run fetches it). Matches the
/// (case-insensitive) query against each plugin's id / name / description /
/// tags. An empty query lists everything; official ccteam plugins
/// (`source == "ccteam"`) are featured first, then sorted by id. Text output prints
/// `id` + type + description so the user can copy an `id` into
/// `ccteam role add`. The async load is driven on a throwaway current-thread
/// runtime ([`block_on_async`]) since `main()` is sync.
pub fn run_role_search(paths: &CcteamPaths, query: &str, format: OutputFormat) -> Result<String> {
    let hits = search_hub_catalog(paths, query, None)?;
    render_hub_search(&hits, query, format, "plugin", "ccteam role add <id>")
}

fn search_hub_catalog(
    paths: &CcteamPaths,
    query: &str,
    type_filter: Option<&str>,
) -> Result<Vec<ccteam_im::hub::HubPlugin>> {
    let index = block_on_async(ccteam_im::hub::load_catalog(
        &ccteam_im::hub::hub_base(),
        paths,
        false,
    ))??;
    let q = query.trim().to_lowercase();
    let mut hits: Vec<ccteam_im::hub::HubPlugin> = index
        .plugins
        .into_iter()
        .filter(|p| {
            type_filter.is_none_or(|expected| p.type_ == expected)
                && (q.is_empty()
                    || p.id.to_lowercase().contains(&q)
                    || p.name.to_lowercase().contains(&q)
                    || p.description.to_lowercase().contains(&q)
                    || p.tags.iter().any(|t| t.to_lowercase().contains(&q)))
        })
        .collect();
    // Feature official ccteam plugins (`source == "ccteam"`) first, then by id —
    // mirrors the web marketplace browse order (see `HubIndex::sort_ccteam_first`).
    hits.sort_by(|a, b| {
        (a.source != "ccteam")
            .cmp(&(b.source != "ccteam"))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(hits)
}

fn render_hub_search(
    hits: &[ccteam_im::hub::HubPlugin],
    query: &str,
    format: OutputFormat,
    noun: &str,
    install_hint: &str,
) -> Result<String> {
    Ok(match format {
        OutputFormat::Json => serde_json::to_string_pretty(&hits)?,
        OutputFormat::Text => {
            if hits.is_empty() {
                format!("в marketplace ccteam-hub нет {noun}, соответствующих `{query}`.\n")
            } else {
                let mut out = format!(
                    "{} {noun} в marketplace ccteam-hub{}:\n\n",
                    hits.len(),
                    if query.trim().is_empty() {
                        String::new()
                    } else {
                        format!(", соответствующих `{query}`")
                    }
                );
                for p in hits {
                    out.push_str(&format!("  {}  [{}]\n", p.id, p.type_));
                    if !p.description.is_empty() {
                        // One-line, truncated description for the list view.
                        let desc: String = p.description.chars().take(96).collect();
                        out.push_str(&format!("      {desc}\n"));
                    }
                }
                out.push_str(&format!("\nУстановить: {install_hint}\n"));
                out
            }
        }
    })
}

/// `ccteam role add` installs agent/vendor-plugin entries into a project and
/// refuses skill entries with a pointer to `ccteam skill add`.
pub fn run_role_add(
    paths: &CcteamPaths,
    id: &str,
    as_role: Option<&str>,
    project: Option<&str>,
    force: bool,
) -> Result<String> {
    let base = ccteam_im::hub::hub_base();
    let index = block_on_async(ccteam_im::hub::load_catalog(&base, paths, false))??;
    let plugin = index.find(id).ok_or_else(|| {
        anyhow::anyhow!(
            "в marketplace ccteam-hub нет плагина `{id}` — попробуйте `ccteam role search <q>`"
        )
    })?;
    if plugin.type_ == "skill" {
        bail!("запись hub `{id}` — skill; используйте: ccteam skill add {id}");
    }
    let project_dir = resolve_project_dir(paths, project)?;
    let library_root = paths.skills_dir();
    let result = block_on_async(ccteam_im::hub::install_plugin(
        &project_dir,
        &library_root,
        plugin,
        as_role,
        force,
    ))?
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    // The installed file stem (the `/role` name) is the sanitized override or
    // the plugin id — the same derivation `install_plugin` used. Recovered here
    // for the hint (the path is `.../skills/<stem>/SKILL.md` for a skill, so a
    // raw `file_stem()` would be `SKILL`).
    let stem = ccteam_core::sanitize_role_stem(as_role.unwrap_or(&result.id))
        .unwrap_or_else(|_| result.id.clone());
    let mut out = format!(
        "Установлен {} `{}` из marketplace ccteam-hub{}.\n  {}\n",
        result.type_,
        result.id,
        if result.overwrote {
            " (существующий перезаписан)"
        } else {
            ""
        },
        result.path.display(),
    );
    out.push_str(&format!(
        "\nПереключитесь в чате через `/role {stem}` (или запустите сессию с этой ролью).\n",
    ));
    // The body is third-party content fetched verbatim. Persona text steers an
    // agent that runs with `--dangerously-skip-permissions`, so prompt the
    // operator to read it before use rather than trusting it blind.
    out.push_str(&format!(
        "\nПримечание: это сторонний контент, полученный без изменений — проверьте {} перед использованием.\n",
        result.path.display()
    ));
    Ok(out)
}

/// `ccteam role list [--project <slug>]`. Wraps
/// [`ccteam_core::list_roles`] to show the roles already installed in the
/// project's `.claude/agents/`. An uninitialized project (no `agents/` dir)
/// is a normal "no roles yet" result, not an error.
pub fn run_role_list(
    paths: &CcteamPaths,
    project: Option<&str>,
    format: OutputFormat,
) -> Result<String> {
    let project_dir = resolve_project_dir(paths, project)?;
    let roles = ccteam_core::list_roles(&project_dir)?;
    Ok(match format {
        OutputFormat::Json => serde_json::to_string_pretty(&roles)?,
        OutputFormat::Text => {
            if roles.is_empty() {
                format!(
                    "в {} нет установленных ролей (.claude/agents/ пуст или отсутствует).\n\
                     Откройте каталог: ccteam role search <q>\n",
                    project_dir.display()
                )
            } else {
                let mut out = format!("{} ролей в {}:\n\n", roles.len(), project_dir.display());
                for r in &roles {
                    out.push_str(&format!("  {}", r.role));
                    if !r.description.is_empty() {
                        let desc: String = r.description.chars().take(80).collect();
                        out.push_str(&format!("  — {desc}"));
                    }
                    out.push('\n');
                }
                out
            }
        }
    })
}

/// Search only `type=skill` entries in the curated hub catalog.
pub fn run_skill_search(paths: &CcteamPaths, query: &str, format: OutputFormat) -> Result<String> {
    let hits = search_hub_catalog(paths, query, Some("skill"))?;
    render_hub_search(
        &hits,
        query,
        format,
        "skill",
        "ccteam skill add <id> [--as <stem>]",
    )
}

/// Install one hub skill into the user-level global library.
pub fn run_skill_add(
    paths: &CcteamPaths,
    id: &str,
    as_stem: Option<&str>,
    force: bool,
) -> Result<String> {
    let index = block_on_async(ccteam_im::hub::load_catalog(
        &ccteam_im::hub::hub_base(),
        paths,
        false,
    ))??;
    let plugin = index.find(id).ok_or_else(|| {
        anyhow::anyhow!(
            "в marketplace ccteam-hub нет skill `{id}` — попробуйте `ccteam skill search <q>`"
        )
    })?;
    if plugin.type_ != "skill" {
        bail!(
            "запись hub `{id}` имеет тип `{}`; используйте: ccteam role add {id}",
            plugin.type_
        );
    }
    let project_dir =
        std::env::current_dir().context("прочитать cwd для контекста установки hub")?;
    let library_root = paths.skills_dir();
    let result = block_on_async(ccteam_im::hub::install_plugin(
        &project_dir,
        &library_root,
        plugin,
        as_stem,
        force,
    ))?
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(format!(
        "Skill `{}` установлен в пользовательскую библиотеку{}:\n  {}\n",
        result.id,
        if result.overwrote {
            " (существующий перезаписан)"
        } else {
            ""
        },
        result.path.display()
    ))
}

/// List the user-level library recursively.
pub fn run_skill_list(paths: &CcteamPaths, json: bool) -> Result<String> {
    let skills = ccteam_core::list_library_skills(&paths.skills_dir());
    if json {
        return Ok(serde_json::to_string_pretty(&skills)?);
    }
    if skills.is_empty() {
        return Ok(format!(
            "в пользовательской библиотеке нет skills ({}).\nОткройте каталог: ccteam skill search <q>\n",
            paths.skills_dir().display()
        ));
    }
    let mut out = format!(
        "{} skills в {}:\n\n",
        skills.len(),
        paths.skills_dir().display()
    );
    for skill in skills {
        out.push_str(&format!("  {}", skill.id));
        if !skill.description.is_empty() {
            let description: String = skill.description.chars().take(96).collect();
            out.push_str(&format!("  — {description}"));
        }
        out.push('\n');
    }
    Ok(out)
}

/// Remove one library skill, or an arbitrary skill subtree with explicit
/// `force`. A source root containing only nested skills is deliberately not
/// mistaken for one skill.
pub fn run_skill_remove(paths: &CcteamPaths, id: &str, force: bool) -> Result<String> {
    ccteam_core::validate_skill_library_id(id)?;
    let target = paths.skills_dir().join(id);
    if !target.exists() {
        bail!(
            "запись библиотеки skills `{id}` отсутствует в {}",
            target.display()
        );
    }
    if !target.is_dir() {
        bail!(
            "запись библиотеки skills `{id}` не является каталогом: {}",
            target.display()
        );
    }
    if !target.join("SKILL.md").is_file() && !force {
        bail!(
            "`{id}` — дерево skills, не один skill (нет корневого SKILL.md); для зарегистрированного источника используйте `ccteam skill source rm {id}` или повторите с --force"
        );
    }
    std::fs::remove_dir_all(&target)
        .with_context(|| format!("удалить поддерево библиотеки skills {}", target.display()))?;
    Ok(format!(
        "`{id}` удалён из пользовательской библиотеки skills.\n"
    ))
}

/// Refresh one hub-pinned skill, or every installed hub skill whose catalog
/// sha differs from the library copy.
pub fn run_skill_update(paths: &CcteamPaths, id: Option<&str>, all: bool) -> Result<String> {
    if id.is_some() == all {
        bail!("выберите ровно одно: <hub-id> или --all");
    }
    let index = block_on_async(ccteam_im::hub::load_catalog(
        &ccteam_im::hub::hub_base(),
        paths,
        false,
    ))??;
    let library_root = paths.skills_dir();
    let project_dir = std::env::current_dir().context("прочитать cwd для контекста статуса hub")?;

    let candidates: Vec<&ccteam_im::hub::HubPlugin> = if let Some(id) = id {
        let plugin = index
            .find(id)
            .ok_or_else(|| anyhow::anyhow!("в marketplace ccteam-hub нет skill `{id}`"))?;
        if plugin.type_ != "skill" {
            bail!(
                "запись hub `{id}` имеет тип `{}`; используйте: ccteam role add {id}",
                plugin.type_
            );
        }
        vec![plugin]
    } else {
        index
            .plugins
            .iter()
            .filter(|plugin| plugin.type_ == "skill")
            .collect()
    };

    let mut updated = Vec::new();
    let mut current = Vec::new();
    for plugin in candidates {
        match ccteam_im::hub::installed_status(&project_dir, &library_root, plugin) {
            ccteam_im::hub::InstalledStatus::UpdateAvailable => {
                block_on_async(ccteam_im::hub::install_plugin(
                    &project_dir,
                    &library_root,
                    plugin,
                    None,
                    true,
                ))?
                .map_err(|e| anyhow::anyhow!("{e}"))?;
                updated.push(plugin.id.clone());
            }
            ccteam_im::hub::InstalledStatus::Installed => current.push(plugin.id.clone()),
            ccteam_im::hub::InstalledStatus::NotInstalled => {}
        }
    }

    if let Some(id) = id {
        if updated.iter().any(|updated| updated == id) {
            return Ok(format!(
                "Hub skill `{id}` обновлён в пользовательской библиотеке.\n"
            ));
        }
        if current.iter().any(|current| current == id) {
            return Ok(format!("Hub skill `{id}` уже актуален.\n"));
        }
        return Ok(format!(
            "Hub skill `{id}` не установлен; используйте `ccteam skill add {id}`.\n"
        ));
    }

    Ok(format!(
        "Обновление hub skills завершено: обновлено {}, уже актуальны {}.\n",
        updated.len(),
        current.len()
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SkillSourceKind {
    Git,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SkillSourceRecord {
    kind: SkillSourceKind,
    origin: String,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    r#ref: Option<String>,
}

type SkillSources = std::collections::BTreeMap<String, SkillSourceRecord>;

fn skill_sources_path(paths: &CcteamPaths) -> std::path::PathBuf {
    paths.skills_dir().join(".sources.json")
}

fn load_skill_sources(paths: &CcteamPaths) -> Result<SkillSources> {
    let path = skill_sources_path(paths);
    if !path.exists() {
        return Ok(SkillSources::new());
    }
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("прочитать источники skills {}", path.display()))?;
    serde_json::from_str(&body)
        .with_context(|| format!("разобрать источники skills {}", path.display()))
}

fn save_skill_sources(paths: &CcteamPaths, sources: &SkillSources) -> Result<()> {
    let path = skill_sources_path(paths);
    std::fs::create_dir_all(paths.skills_dir())
        .with_context(|| format!("создать библиотеку skills {}", paths.skills_dir().display()))?;
    let tmp = path.with_extension("json.tmp");
    let mut body = serde_json::to_string_pretty(sources)?;
    body.push('\n');
    std::fs::write(&tmp, body).with_context(|| format!("записать {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("переименовать {} -> {}", tmp.display(), path.display()))
}

fn source_default_stem(origin: &str) -> Result<String> {
    let path = std::path::Path::new(origin);
    let raw = if path.exists() {
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                anyhow::anyhow!("у пути источника нет имени каталога в UTF-8: {origin}")
            })?
            .to_string()
    } else {
        origin
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', ':'])
            .next()
            .unwrap_or("")
            .trim_end_matches(".git")
            .to_string()
    };
    ccteam_core::sanitize_skill_library_id(&raw)
}

fn run_checked_command(mut command: Command, description: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("выполнить {description}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    bail!(
        "{description} завершилась ошибкой{}",
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    )
}

fn clone_git_source(origin: &str, target: &std::path::Path, reference: Option<&str>) -> Result<()> {
    let mut clone = Command::new("git");
    clone.arg("clone").arg("--").arg(origin).arg(target);
    run_checked_command(clone, "git clone skill source")?;
    if let Some(reference) = reference {
        let mut checkout = Command::new("git");
        checkout
            .arg("-C")
            .arg(target)
            .args(["checkout", "--force", reference]);
        run_checked_command(checkout, "git checkout skill source ref")?;
    }
    Ok(())
}

fn copy_source_tree(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    std::fs::create_dir(target)
        .with_context(|| format!("create source target {}", target.display()))?;
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("read source directory {}", source.display()))?
    {
        let entry = entry.with_context(|| format!("read entry under {}", source.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type for {}", entry.path().display()))?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_source_tree(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &destination).with_context(|| {
                format!(
                    "copy source file {} -> {}",
                    entry.path().display(),
                    destination.display()
                )
            })?;
        } else {
            bail!(
            "дерево источника содержит неподдерживаемую символьную ссылку или специальный файл: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

/// Register a git repository or one-time local-directory copy under the skill
/// library and persist its update metadata.
pub fn run_skill_source_add(
    paths: &CcteamPaths,
    origin: &str,
    name: Option<&str>,
    reference: Option<&str>,
) -> Result<String> {
    let origin_path = std::path::Path::new(origin);
    let local = origin_path.exists();
    if local && !origin_path.is_dir() {
        bail!(
            "источник skills должен быть каталогом: {}",
            origin_path.display()
        );
    }
    let is_git = !local || origin_path.join(".git").exists();
    if !is_git && reference.is_some() {
        bail!("--ref допустим только для git-источников skills");
    }
    let raw_stem = name
        .map(str::to_string)
        .unwrap_or(source_default_stem(origin)?);
    let stem = ccteam_core::sanitize_skill_library_id(&raw_stem)?;
    let target = paths.skills_dir().join(&stem);
    if target.exists() {
        bail!(
            "цель источника skills `{stem}` уже существует в {}; сначала удалите её",
            target.display()
        );
    }
    let mut sources = load_skill_sources(paths)?;
    if sources.contains_key(&stem) {
        bail!("источник skills `{stem}` уже зарегистрирован");
    }
    let target_parent = target.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "у цели источника skills нет родительского каталога: {}",
            target.display()
        )
    })?;
    std::fs::create_dir_all(target_parent)
        .with_context(|| format!("create skill source parent {}", target_parent.display()))?;

    let canonical_origin = if local {
        std::fs::canonicalize(origin_path)
            .with_context(|| format!("canonicalize skill source {}", origin_path.display()))?
            .display()
            .to_string()
    } else {
        origin.to_string()
    };
    let install = if is_git {
        clone_git_source(&canonical_origin, &target, reference)
    } else {
        copy_source_tree(origin_path, &target)
    };
    if let Err(err) = install {
        let _ = std::fs::remove_dir_all(&target);
        return Err(err);
    }

    sources.insert(
        stem.clone(),
        SkillSourceRecord {
            kind: if is_git {
                SkillSourceKind::Git
            } else {
                SkillSourceKind::Path
            },
            origin: canonical_origin,
            r#ref: reference.map(str::to_string),
        },
    );
    if let Err(err) = save_skill_sources(paths, &sources) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(err);
    }
    let count = ccteam_core::list_library_skills(&target).len();
    Ok(format!(
        "Добавлен {}-источник skills `{stem}` в {} (обнаружено skills: {count}).\n",
        if is_git { "git" } else { "path" },
        target.display()
    ))
}

fn update_git_source(target: &std::path::Path, reference: Option<&str>) -> Result<()> {
    if let Some(reference) = reference {
        let mut fetch = Command::new("git");
        fetch
            .arg("-C")
            .arg(target)
            .args(["fetch", "origin", reference]);
        run_checked_command(fetch, "git fetch skill source ref")?;
        let mut checkout = Command::new("git");
        checkout
            .arg("-C")
            .arg(target)
            .args(["checkout", "--force", "FETCH_HEAD"]);
        run_checked_command(checkout, "git checkout fetched skill source ref")
    } else {
        let mut pull = Command::new("git");
        pull.arg("-C").arg(target).args(["pull", "--ff-only"]);
        run_checked_command(pull, "git pull skill source")
    }
}

/// Update one or all registered skill sources.
pub fn run_skill_source_update(
    paths: &CcteamPaths,
    stem: Option<&str>,
    all: bool,
) -> Result<String> {
    if stem.is_some() == all {
        bail!("выберите ровно одно: <stem> или --all");
    }
    let sources = load_skill_sources(paths)?;
    let selected: Vec<(&String, &SkillSourceRecord)> = if let Some(stem) = stem {
        ccteam_core::validate_skill_library_id(stem)?;
        let record = sources
            .get_key_value(stem)
            .ok_or_else(|| anyhow::anyhow!("источник skills `{stem}` не зарегистрирован"))?;
        vec![record]
    } else {
        sources.iter().collect()
    };
    let mut out = String::new();
    for (stem, record) in selected {
        let target = paths.skills_dir().join(stem);
        if !target.is_dir() {
            bail!(
                "источник skills `{stem}` отсутствует в {}",
                target.display()
            );
        }
        match record.kind {
            SkillSourceKind::Git => {
                update_git_source(&target, record.r#ref.as_deref())?;
                out.push_str(&format!("Git-источник skills `{stem}` обновлён.\n"));
            }
            SkillSourceKind::Path => out.push_str(&format!(
                "Источник skills типа path `{stem}` управляется самостоятельно; обновление ничего не делает (origin {}).\n",
                record.origin
            )),
        }
    }
    if out.is_empty() {
        out.push_str("Источники skills не зарегистрированы.\n");
    }
    Ok(out)
}

/// List registered skill sources.
pub fn run_skill_source_list(paths: &CcteamPaths, json: bool) -> Result<String> {
    let sources = load_skill_sources(paths)?;
    if json {
        return Ok(serde_json::to_string_pretty(&sources)?);
    }
    if sources.is_empty() {
        return Ok("Источники skills не зарегистрированы.\n".to_string());
    }
    let mut out = format!("Источников skills: {}\n\n", sources.len());
    for (stem, source) in sources {
        out.push_str(&format!(
            "  {stem}  [{}]  {}{}\n",
            match source.kind {
                SkillSourceKind::Git => "git",
                SkillSourceKind::Path => "path",
            },
            source.origin,
            source
                .r#ref
                .as_deref()
                .map(|reference| format!(" @ {reference}"))
                .unwrap_or_default()
        ));
    }
    Ok(out)
}

/// Remove a registered source tree and its metadata. Projects are never
/// inspected or modified.
pub fn run_skill_source_remove(paths: &CcteamPaths, stem: &str) -> Result<String> {
    ccteam_core::validate_skill_library_id(stem)?;
    let mut sources = load_skill_sources(paths)?;
    if sources.remove(stem).is_none() {
        bail!("источник skills `{stem}` не зарегистрирован");
    }
    let target = paths.skills_dir().join(stem);
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("удалить дерево источника skills {}", target.display()))?;
    }
    save_skill_sources(paths, &sources)?;
    Ok(format!(
        "Источник skills `{stem}` и его дерево библиотеки удалены.\n"
    ))
}

const PROJECT_SKILLS_LINK_TARGET: &str = "../.agents/skills";

fn ensure_project_skill_entity(project_dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let entity = project_dir.join(".agents/skills");
    match std::fs::symlink_metadata(&entity) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(entity),
        Ok(_) => bail!(
            "сущность skills проекта должна быть настоящим каталогом, а не символьной ссылкой/файлом: {}",
            entity.display()
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&entity)
                .with_context(|| format!("создать сущность skills проекта {}", entity.display()))?;
            Ok(entity)
        }
        Err(err) => Err(err).with_context(|| format!("проверить {}", entity.display())),
    }
}

fn project_skills_link_is_correct(link: &std::path::Path, entity: &std::path::Path) -> bool {
    let Ok(target) = std::fs::read_link(link) else {
        return false;
    };
    let resolved = if target.is_absolute() {
        target
    } else {
        link.parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(target)
    };
    matches!(
        (std::fs::canonicalize(resolved), std::fs::canonicalize(entity)),
        (Ok(actual), Ok(expected)) if actual == expected
    )
}

#[cfg(unix)]
fn create_project_skills_link(link: &std::path::Path) -> Result<()> {
    std::os::unix::fs::symlink(PROJECT_SKILLS_LINK_TARGET, link)
        .with_context(|| format!("создать ссылку skills проекта {}", link.display()))
}

#[cfg(not(unix))]
fn create_project_skills_link(_link: &std::path::Path) -> Result<()> {
    bail!("управление символьными ссылками skills проекта требует Unix/WSL")
}

/// Ensure the neutral project skill entity and Claude discovery symlink.
pub fn run_skill_ensure_project(paths: &CcteamPaths, project: Option<&str>) -> Result<String> {
    let project_dir = resolve_project_dir(paths, project)?;
    let entity = ensure_project_skill_entity(&project_dir)?;
    let link = project_dir.join(".claude/skills");
    std::fs::create_dir_all(project_dir.join(".claude"))
        .with_context(|| format!("создать {}/.claude", project_dir.display()))?;
    match std::fs::symlink_metadata(&link) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            create_project_skills_link(&link)?;
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if !project_skills_link_is_correct(&link, &entity) {
                bail!(
                    "{} — символьная ссылка на неверную цель; ожидалась {}",
                    link.display(),
                    PROJECT_SKILLS_LINK_TARGET
                );
            }
        }
        Ok(metadata) if metadata.file_type().is_dir() => {
            let empty = std::fs::read_dir(&link)
                .with_context(|| format!("прочитать устаревший каталог skills {}", link.display()))?
                .next()
                .is_none();
            if !empty {
                bail!(
                    "устаревший каталог skills проекта {} не пуст; используйте `ccteam skill migrate-project{}`",
                    link.display(),
                    project
                        .map(|slug| format!(" --project {slug}"))
                        .unwrap_or_default()
                );
            }
            std::fs::remove_dir(&link).with_context(|| {
                format!(
                    "удалить пустой устаревший каталог skills {}",
                    link.display()
                )
            })?;
            create_project_skills_link(&link)?;
        }
        Ok(_) => bail!(
            "{} существует, но не является ни каталогом, ни ожидаемой символьной ссылкой",
            link.display()
        ),
        Err(err) => return Err(err).with_context(|| format!("проверить {}", link.display())),
    }
    Ok(format!(
        "Представление skills проекта готово:\n  сущность: {}\n  claude: {} -> {}\n",
        entity.display(),
        link.display(),
        PROJECT_SKILLS_LINK_TARGET
    ))
}

/// Move legacy project-local skill directories to the neutral entity, then
/// replace the legacy directory with the Claude discovery symlink.
pub fn run_skill_migrate_project(paths: &CcteamPaths, project: Option<&str>) -> Result<String> {
    let project_dir = resolve_project_dir(paths, project)?;
    let entity = ensure_project_skill_entity(&project_dir)?;
    let legacy = project_dir.join(".claude/skills");
    std::fs::create_dir_all(project_dir.join(".claude"))
        .with_context(|| format!("создать {}/.claude", project_dir.display()))?;

    let metadata = match std::fs::symlink_metadata(&legacy) {
        Ok(metadata) => Some(metadata),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("проверить {}", legacy.display())),
    };
    if let Some(metadata) = metadata.as_ref() {
        if metadata.file_type().is_symlink() {
            if project_skills_link_is_correct(&legacy, &entity) {
                return Ok(format!(
                    "Skills проекта уже используют нейтральную сущность в {}.\n",
                    entity.display()
                ));
            }
            bail!(
                "{} — символьная ссылка на неверную цель; перенос отклонён",
                legacy.display()
            );
        }
        if !metadata.file_type().is_dir() {
            bail!(
                "устаревшее представление skills проекта не является каталогом: {}",
                legacy.display()
            );
        }

        let mut moves = Vec::new();
        for entry in std::fs::read_dir(&legacy)
            .with_context(|| format!("прочитать устаревший каталог skills {}", legacy.display()))?
        {
            let entry =
                entry.with_context(|| format!("прочитать запись в {}", legacy.display()))?;
            let file_type = entry
                .file_type()
                .with_context(|| format!("прочитать тип файла {}", entry.path().display()))?;
            if !file_type.is_dir() {
                bail!(
                    "устаревшее представление skills содержит запись не-каталог {}; переместите её вручную перед переносом",
                    entry.path().display()
                );
            }
            let destination = entity.join(entry.file_name());
            if destination.exists() {
                bail!(
                    "конфликт переноса skills проекта в {}; устраните его перед повтором",
                    destination.display()
                );
            }
            moves.push((entry.path(), destination));
        }
        for (source, destination) in &moves {
            std::fs::rename(source, destination).with_context(|| {
                format!(
                    "переместить skill проекта {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
        std::fs::remove_dir(&legacy)
            .with_context(|| format!("удалить устаревший каталог skills {}", legacy.display()))?;
    }
    create_project_skills_link(&legacy)?;
    Ok(format!(
        "Skills проекта перенесены в {}; создана ссылка {} -> {}.\n",
        entity.display(),
        legacy.display(),
        PROJECT_SKILLS_LINK_TARGET
    ))
}

/// `ccteam remove <slug>` implementation.
///
/// Steps (in order):
/// 1. Refusal gate. Calls [`ccteam_core::refuses_active_session`]; if
///    it returns `Some(refusal)` and `opts.force` is false, the command
///    halts before any mutation.
/// 2. Resolve project_dir via `~/.ccteam/config.yaml::projects[]` so
///    arbitrary-path installs are honoured (falling back to the conventional
///    location under `projects_root` when the row is already gone).
/// 3. Ask the live daemon to durably retire the slug, fence every admission
///    path, stop and join its sessions/writers, then clean progress state.
///    Missing/failed acknowledgement is fatal and leaves config untouched.
///    Skipped entirely for a slug with no catalog row: there is no generation
///    to retire and a tombstone must never be minted for a typo.
/// 4. Stop any legacy mux sessions and remove legacy global state directories.
/// 5. `--purge`: delete exactly ccteam's project footprint —
///    `rm -rf <project>/.ccteam/` and ccteam's hook section inside
///    `<project>/.claude/settings.local.json` (surgically; file deleted
///    only if it collapses to empty). See [`purge_project_managed_paths`]
///    for the full KEEP/DELETE contract. Never touches `<project>/.env`,
///    ANY `.claude/agents/*.md` (all user files — ccteam seeds no
///    role), `CLAUDE.md` / `AGENTS.md`, or the user's `settings.json`
///    (CLAUDE.md §三 red line).
/// 6. Drop the config registry row last (a no-op when there was none). Every
///    step between the daemon ACK and this commit point reports its failures
///    and continues, so a retired generation is never stranded in the catalog.
///    A failure before this point leaves a
///    durable retired generation which daemon startup and spawn paths reject;
///    retrying the command is safe. The same slug is never reused.
///
/// This is the reusable remove engine: the flat `ccteam remove` and the
/// grouped `ccteam project rm` both route here (the structured
/// [`RemoveReport`] doubles as the dry-run plan).
pub fn run_remove(paths: &CcteamPaths, slug: &str, opts: RemoveOptions) -> Result<RemoveReport> {
    // 0. Shape is validated before ANY other work, and registration decides
    // whether the daemon is contacted at all. The daemon mints an irreversible
    // tombstone for whatever slug it is handed, so a typo must never reach it —
    // but refusing the whole command would also destroy the only way to clean
    // an already-deregistered project's leftovers (`--purge` after a plain
    // `project rm`, or after a web-console delete). An unregistered slug
    // therefore skips the daemon retire and the config drop, and runs the file
    // sweeps only. `--dry-run` previews exactly that shape.
    ccteam_core::validate_slug_format(slug)?;
    let registered = ccteam_core::lookup_project_in_config(&paths.root, slug)?;

    let mut report = RemoveReport {
        slug: slug.to_string(),
        purge: opts.purge,
        dry_run: opts.dry_run,
        forced: opts.force,
        ..Default::default()
    };

    // 1. Refusal gate (CLAUDE.md §三).
    let refusal = ccteam_core::refuses_active_session(paths, slug)?;
    if let Some(r) = refusal {
        if !opts.force {
            // Halt before any mutation — user must `tmux kill-session`
            // / let claude finish / pass `--force`.
            report.refusal = Some(r.clone());
            bail!(
                "ccteam remove `{slug}`: {}. Повторите с `--force` для принудительного выполнения.",
                r.message(slug)
            );
        } else {
            report.steps.push(format!(
                "защита принудительно пройдена: {}",
                r.message(slug)
            ));
        }
    }

    // 2. The registry row resolved in step 0 owns the project directory, so an
    // arbitrary-path install is deleted correctly even when the slug does not
    // sit under `paths.projects_root`. Without a row the only honest guess is
    // the conventional location under `projects_root`.
    let project_dir = registered
        .as_ref()
        .map(|entry| entry.path.clone())
        .unwrap_or_else(|| paths.project_dir(slug));

    // 3. Dry-run previews the exact destructive surfaces but never contacts
    // the daemon or creates the stable progress lock.
    let backend = ccteam_harness::default_process_backend();
    let progress_path = paths.progress_jsonl(slug);
    let progress_dir = paths.progress_dir().join(slug); // flex shard dir
    let global_inbox_slug_dir = paths.inbox_dir().join(slug);
    let global_control_slug_dir = paths.control_dir().join(slug);

    if registered.is_none() {
        // Nothing to retire: no catalog row means no live generation the daemon
        // owns, and minting a tombstone here is exactly the typo burn step 0
        // protects against. Fall through to the file sweeps.
        report.steps.push(format!(
            "проект `{slug}` не зарегистрирован в config.yaml::projects; \
             retirement через daemon пропущен (tombstone не создаётся), \
             выполняется только очистка файлов"
        ));
    } else if opts.dry_run {
        // Same tolerance as the executing path below: `stop_project_chat_sessions`
        // contacts the backend before it honours `dry_run`, so a dead mux control
        // plane must not make the preview of a working command unavailable.
        match stop_project_chat_sessions(backend.as_ref(), slug, true) {
            Ok(chat_stop) => {
                for name in &chat_stop.would_stop {
                    report
                        .steps
                        .push(format!("будет остановлена чат-сессия `{name}`"));
                }
            }
            Err(error) => report.steps.push(format!(
                "legacy чат-сессии: не удалось проверить ({error:#})"
            )),
        }
        report
            .steps
            .push(if ccteam_core::daemon::daemon_reachable(paths) {
                "daemon получит durable retirement, остановит сессии и дождётся всех writer'ов"
                    .to_string()
            } else {
                "для выполнения потребуется запущенный daemon; без ACK config останется нетронутым"
                    .to_string()
            });
        for path in ccteam_harness::execution::progress_bridge::cleanup_progress_state(
            &progress_path,
            true,
        )? {
            if path == progress_path {
                report
                    .steps
                    .push(format!("будет удалён progress.jsonl {}", path.display()));
            } else {
                report.steps.push(format!(
                    "будет удалено состояние progress {}",
                    path.display()
                ));
            }
        }
    } else {
        // No outer context here: `request_daemon_project_retire` already words
        // each failure truthfully, and a blanket "config не изменён" would lie
        // about a generation whose tombstone the daemon already committed.
        let outcome = request_daemon_project_retire(paths, slug)?;
        report
            .steps
            .push("retirement проекта подтверждено демоном".to_string());
        for sid in outcome.sessions_stopped {
            report
                .steps
                .push(format!("daemon остановил сессию `{sid}`"));
        }
        for path in outcome.progress_removed {
            report
                .steps
                .push(format!("daemon удалил состояние progress {path}"));
        }

        // The gateway owns managed stdio/ACP sessions. Keep the old mux pass
        // after its ACK solely for terminal-protocol leftovers.
        //
        // This runs AFTER the irreversible daemon ACK, so a backend failure
        // (absent tmux, unreachable rmux socket) must never strand the registry
        // row on a generation that is already retired. Report it and continue to
        // the config drop.
        match stop_project_chat_sessions(backend.as_ref(), slug, false) {
            Ok(chat_stop) => {
                for name in &chat_stop.stopped {
                    report
                        .steps
                        .push(format!("остановлена legacy чат-сессия `{name}`"));
                }
            }
            Err(error) => report.steps.push(format!(
                "legacy чат-сессии: не удалось проверить ({error:#})"
            )),
        }
    }

    // 4. Legacy global state is not a producer-owned progress surface.
    for (label, path, is_dir) in [
        ("progress shard dir", progress_dir.clone(), true),
        ("inbox/<slug>/ dir", global_inbox_slug_dir.clone(), true),
        ("control/<slug>/ dir", global_control_slug_dir.clone(), true),
    ] {
        let exists = if is_dir {
            path.is_dir()
        } else {
            path.is_file()
        };
        if !exists {
            continue;
        }
        if opts.dry_run {
            report
                .steps
                .push(format!("будет удалён {label} {}", path.display()));
            continue;
        }
        // Everything from here to the config drop runs AFTER the irreversible
        // daemon ACK. A cleanup failure must never strand the registry row on a
        // generation that is already permanently retired: report it and carry on
        // to the commit point, exactly like the legacy mux sweep above.
        let removed = if is_dir {
            std::fs::remove_dir_all(&path).with_context(|| format!("rm -rf {}", path.display()))
        } else {
            std::fs::remove_file(&path).with_context(|| format!("rm {}", path.display()))
        };
        match removed {
            Ok(()) => report
                .steps
                .push(format!("удалён {label} {}", path.display())),
            Err(error) => {
                let note = format!("{label} {}: не удалось удалить ({error:#})", path.display());
                report.steps.push(note.clone());
                report.cleanup_failures.push(note);
            }
        }
    }

    // 5. Optional project-local cleanup happens after daemon ACK and before
    // config deletion, so any partial failure remains visible and retryable.
    if opts.purge {
        if let Err(error) = purge_project_managed_paths(&project_dir, opts.dry_run, &mut report) {
            let note = format!(
                "очистка {}: не удалось завершить ({error:#})",
                project_dir.display()
            );
            report.steps.push(note.clone());
            report.cleanup_failures.push(note);
        }
        // V0.6.5 F151 — also clean `~/.ccteam/state/im/registry/<slug>/`. The
        // F146 registry is the daemon's bot lifecycle SoT, so without
        // this cleanup `list_bots()` still surfaces stale BotRegistration
        // entries after the workflow.yaml is gone → daemon can spawn an
        // orphan tmux session.
        if let Err(error) =
            purge_imd_registry_for_slug(&paths.root, slug, opts.dry_run, &mut report)
        {
            let note =
                format!("очистка state/im/registry/{slug}/: не удалось завершить ({error:#})");
            report.steps.push(note.clone());
            report.cleanup_failures.push(note);
        }
    }

    // 6. Config registry drop is the commit point and therefore always last.
    // An unregistered slug has nothing to commit — the file sweeps above were
    // the whole job.
    if registered.is_none() {
        report.steps.push(format!(
            "запись config.yaml::projects для `{slug}` отсутствует; удалять нечего"
        ));
    } else if opts.dry_run {
        report.steps.push(format!(
            "будет удалена запись config.yaml::projects для `{slug}` (путь: {})",
            project_dir.display()
        ));
    } else if ccteam_core::remove_project_from_config(&paths.root, slug)? {
        report
            .steps
            .push(format!("удалена запись config.yaml::projects для `{slug}`"));
    }

    Ok(report)
}

/// Outcome of enumerating + (optionally) killing a project's live
/// chat-mode role sessions. Shared by `run_project_stop` (the `project
/// stop` command) and `run_remove` (rm = stop-then-delete).
///
/// `stopped` carries the tmux session names that were actually killed;
/// under `--dry-run` it is empty and `would_stop` carries the targets
/// instead (so the dry-run preview shows the same shape it would act on).
#[derive(Debug, Clone, Default)]
struct ChatSessionStop {
    /// Session names killed this run (empty under dry-run).
    stopped: Vec<String>,
    /// Session names that *would* be killed (only populated under dry-run).
    would_stop: Vec<String>,
}

/// Enumerate the live chat-mode role sessions belonging to `slug`
/// (`ccteam-chat-<slug>-<role>`) and, unless `dry_run`, kill each one.
///
/// Backend-agnostic: enumeration + kill both go through the injected
/// [`ProcessBackend`] (`list_sessions` / `kill`), so the teardown sees
/// whatever mux is live under `CCTEAM_MUX_BACKEND` — the bundled `rmux`
/// daemon by default, or `tmux` when opted in. (The old tmux-only path
/// shelled out to `tmux list-sessions` directly and so saw nothing under
/// the default rmux backend.) The CLI threads `default_process_backend()`
/// in; tests inject a deterministic [`InProcBackend`].
///
/// Process-independent on purpose: the CLI is a separate process from the
/// daemon, so we never consult daemon in-memory state — only the live mux
/// session names. We reuse [`list_chat_sessions`] (names only, never
/// capture-pane) and keep the ones whose parsed slug equals `slug`,
/// parsing via [`parse_chat_session_name`] rather than a raw `starts_with`
/// so a slug that itself contains dashes (e.g. `dev-foo`) matches its own
/// `ccteam-chat-dev-foo-<role>` sessions and not a sibling
/// `ccteam-chat-dev-<role>` one. The slug is always the *first* parsed
/// element, so this stays correct even if the trailing segment changes
/// meaning (role → sid).
///
/// **Red line.** `project stop` / `rm` are EXPLICIT user commands, the
/// allowed exception to "never PROACTIVELY kill a long session": the
/// teardown is user-requested and resumable (the daemon recreates the
/// pane via `--resume` on the next interaction). The kill is idempotent
/// ([`ProcessBackend::kill`] is `Ok(())` for a vanished session).
fn stop_project_chat_sessions(
    backend: &dyn ccteam_harness::ProcessBackend,
    slug: &str,
    dry_run: bool,
) -> Result<ChatSessionStop> {
    use ccteam_harness::{list_chat_sessions, parse_chat_session_name, MuxSessionId};

    // Enumerate live chat sessions via the backend, keep ours, stable-sort
    // so output / kill order is deterministic.
    let live = block_on_async(list_chat_sessions(backend))??;
    let mut matches: Vec<String> = live
        .into_iter()
        .filter(|name| {
            parse_chat_session_name(name)
                .map(|(s, _last)| s == slug)
                .unwrap_or(false)
        })
        .collect();
    matches.sort();

    let mut out = ChatSessionStop::default();
    if dry_run {
        out.would_stop = matches;
        return Ok(out);
    }
    for name in matches {
        block_on_async(backend.kill(&MuxSessionId::new(name.clone())))?
            .with_context(|| format!("остановить чат-сессию `{name}`"))?;
        out.stopped.push(name);
    }
    Ok(out)
}

/// `ccteam project stop <slug>` handler.
///
/// Stops ALL of the project's live chat-mode role sessions (the
/// `ccteam-chat-<slug>-*` tmux sessions) WITHOUT removing the project.
/// This is an explicit, resumable user-requested stop — the project
/// stays registered and the daemon resumes each role by id on the next
/// interaction. `stop` is neither a delete nor a pause.
///
/// Returns a one-line-per-session render (and a tail count); stopping 0
/// sessions is a success, not an error.
pub fn run_project_stop(_paths: &CcteamPaths, slug: &str) -> Result<String> {
    let backend = ccteam_harness::default_process_backend();
    let stop = stop_project_chat_sessions(backend.as_ref(), slug, false)?;

    let mut out = String::new();
    use std::fmt::Write as _;
    writeln!(out, "ccteam project stop {slug}").ok();
    for name in &stop.stopped {
        writeln!(
            out,
            "  - остановлена чат-сессия `{name}` (возобновляется по id)"
        )
        .ok();
    }
    let n = stop.stopped.len();
    writeln!(
        out,
        "остановлено чат-сессий: {n} для `{slug}`{}",
        if n == 0 {
            " (запущенных не было)"
        } else {
            ""
        }
    )
    .ok();
    Ok(out)
}

/// Purge `~/.ccteam/state/im/registry/<slug>/`.
///
/// **Strategy:** for each registered role under the slug, call
/// [`ccteam_im::unregister_bot_in`] first — this is the in-process
/// equivalent of the `chat_unregister_bot` MCP tool. It deletes the
/// `<role>.json` registry file *and* the `<role>.heartbeat` sidecar,
/// which is exactly what the daemon's registry watcher observes to
/// close the tmux session gracefully. After all roles are unregistered
/// (or if the registry dir is empty / malformed), `rm -rf` the
/// `<slug>/` dir to clean any leftover files (e.g. stale heartbeats
/// from a previous unregister/re-register cycle that left a sidecar
/// orphan).
///
/// Works whether the daemon is running or not — `unregister_bot_in`
/// is pure file IO, no daemon RPC.
fn purge_imd_registry_for_slug(
    ccteam_root: &std::path::Path,
    slug: &str,
    dry_run: bool,
    report: &mut RemoveReport,
) -> Result<()> {
    let slug_dir = ccteam_im::registry_root_in(ccteam_root).join(slug);
    if !slug_dir.exists() {
        // Nothing to clean — non-chat slug or already pristine. Stay
        // silent (don't add a "nothing to do" step row that clutters
        // dry-run output).
        return Ok(());
    }

    // Enumerate roles via list_bots_in (so we route through the F146
    // MCP-equivalent surface). Falls back to empty list on parse errors;
    // the final rm -rf still catches whatever remains.
    let bots = ccteam_im::list_bots_in(ccteam_root, Some(slug)).unwrap_or_default();
    let role_count = bots.len();

    if dry_run {
        // Count JSON files even if list_bots_in skipped malformed rows.
        let json_count = std::fs::read_dir(&slug_dir)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
                    .count()
            })
            .unwrap_or(0);
        let noun = if json_count == 1 {
            "JSON file"
        } else {
            "JSON files"
        };
        report.steps.push(format!(
            "would purge state/im/registry/{slug}/ ({json_count} {noun})"
        ));
        return Ok(());
    }

    // Step 6a — per-role unregister (MCP-equivalent in-process call).
    // Idempotent miss is fine; just records the path that *was* there.
    for reg in bots {
        match ccteam_im::unregister_bot_in(ccteam_root, &reg.workflow_slug, &reg.role) {
            Ok((removed, path)) => {
                if removed {
                    report.steps.push(format!(
                        "unregistered bot `{}` (deleted {})",
                        reg.role,
                        path.display()
                    ));
                }
            }
            Err(err) => {
                // Don't bail — the rm -rf below is the fallback.
                report.steps.push(format!(
                    "unregister bot `{}` failed ({err}); will fall back to rm -rf",
                    reg.role
                ));
            }
        }
    }

    // Step 6b — final sweep. Catches: heartbeat sidecars whose
    // registration JSON was already gone, malformed registration
    // files list_bots_in skipped, or the empty slug dir itself.
    if slug_dir.exists() {
        std::fs::remove_dir_all(&slug_dir)
            .with_context(|| format!("rm -rf {}", slug_dir.display()))?;
        report.steps.push(format!(
            "purged state/im/registry/{slug}/ ({role_count} role{} cleared)",
            if role_count == 1 { "" } else { "s" }
        ));
    }

    Ok(())
}

/// Helper for `run_remove --purge` — deletes exactly ccteam's own
/// footprint inside `<project>/` (or, under `--dry-run`, just records
/// the planned step). A project's on-disk ccteam footprint is `.ccteam/`
/// (state.json + workflow.yaml) plus ccteam's hook section inside
/// `.claude/settings.local.json`. (ccteam seeds NO role, so there is
/// no ccteam-owned persona to purge.)
///
/// **DELETE** (ccteam-managed only):
/// - `<project>/.ccteam/` — state.json + workflow.yaml live here (W2).
/// - ccteam's chat-progress + AskUserQuestion hook entries inside
///   `<project>/.claude/settings.local.json` (surgically, via
///   [`ccteam_core::remove_chat_hooks`]). If stripping them leaves the
///   file an empty object, the now-vestigial file is deleted.
///
/// **KEEP (never touched):**
/// - ALL of `<project>/.claude/agents/*.md` — every role file is a user
///   file (including a legacy ccteam-seeded `cto.md`).
/// - `<project>/CLAUDE.md` / `AGENTS.md` (project knowledge = vendor-native).
/// - `<project>/.env` (user-controlled secrets — CLAUDE.md §三 red line).
/// - The user's `<project>/.claude/settings.json` (ccteam manages only
///   the `settings.local.json` layer, never the committed one).
/// - All business code. If the user wants the whole tree gone they can
///   `rm -rf <project>` themselves; the `--purge` contract is strictly
///   ccteam-footprint-only.
fn purge_project_managed_paths(
    project_dir: &std::path::Path,
    dry_run: bool,
    report: &mut RemoveReport,
) -> Result<()> {
    // 1. `.ccteam/` dir (state.json + workflow.yaml). W2 moved
    // workflow.yaml under `.ccteam/`, so this one delete covers both.
    let ccteam_dir = project_dir.join(".ccteam");
    if ccteam_dir.is_dir() {
        if dry_run {
            report
                .steps
                .push(format!("would purge .ccteam/ ({})", ccteam_dir.display()));
        } else {
            std::fs::remove_dir_all(&ccteam_dir)
                .with_context(|| format!("rm -rf {}", ccteam_dir.display()))?;
            report
                .steps
                .push(format!("purged .ccteam/ ({})", ccteam_dir.display()));
        }
    }

    // 2. v0.9.0 W2 (F6.1) — ccteam no longer seeds any role, and a project's
    // `.claude/agents/*.md` (including a legacy `cto.md`) are USER files:
    // `remove` never deletes them.

    // 3. ccteam's hook section inside `.claude/settings.local.json`,
    // removed surgically so any operator-authored keys/hooks survive.
    // If the strip empties the file, delete the vestigial object.
    let settings_local = project_dir.join(".claude").join("settings.local.json");
    let scrub = ccteam_core::remove_chat_hooks(&settings_local, dry_run)?;
    use ccteam_core::ChatHookScrubAction as A;
    match scrub.action {
        A::NotFound | A::NoChangeNeeded => {}
        A::WouldRemove { entries } => {
            report.steps.push(format!(
                "would strip {entries} ccteam hook entr{} from .claude/settings.local.json ({})",
                if entries == 1 { "y" } else { "ies" },
                settings_local.display()
            ));
        }
        A::Removed { entries } => {
            report.steps.push(format!(
                "stripped {entries} ccteam hook entr{} from .claude/settings.local.json ({})",
                if entries == 1 { "y" } else { "ies" },
                settings_local.display()
            ));
        }
        A::RemovedNowEmpty { entries } => {
            // The file existed only to carry ccteam's hooks — delete it.
            std::fs::remove_file(&settings_local)
                .with_context(|| format!("rm {}", settings_local.display()))?;
            report.steps.push(format!(
                "stripped {entries} ccteam hook entr{} and removed now-empty \
                 .claude/settings.local.json ({})",
                if entries == 1 { "y" } else { "ies" },
                settings_local.display()
            ));
        }
    }

    // Paranoia: confirm `.env` survived (if a future refactor widens the
    // purge tree this surfaces in tests immediately).
    let env_file = project_dir.join(".env");
    if env_file.exists() {
        report
            .steps
            .push(format!("preserved {}", env_file.display()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;
    use ccteam_core::{disable_tool_surface_bootstrap_for_tests, progress};
    use tempfile::TempDir;

    /// Disable tool-surface ~/.claude/ mutation for the whole test
    /// binary. These tests exercise CLI command rendering, not the
    /// agent symlink chain — that's tested separately in
    /// crates/ccteam-core/tests/tool_surface_e2e_test.rs with
    /// CLAUDE_CONFIG_HOME isolation.
    static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
    fn ensure_isolation() {
        DISABLE_TOOL_SURFACE.get_or_init(disable_tool_surface_bootstrap_for_tests);
    }

    /// Serialize tests that mutate `CLAUDE_CONFIG_HOME`. Per CLAUDE.md
    /// §六, env-mutating tests really belong under
    /// `crates/*/tests/*.rs` (separate processes), but until that
    /// migration these tests can race against each other since they
    /// run in the same process. The mutex makes them deterministic.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn fresh_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[cfg(unix)]
    fn fake_daemon(paths: &CcteamPaths) -> std::os::unix::net::UnixListener {
        let socket = ccteam_core::daemon::daemon_socket_path(paths);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        std::os::unix::net::UnixListener::bind(socket).unwrap()
    }

    /// Deterministic slug wrapper: bootstrap directly via the core
    /// helper (the LLM/auto-slug path was removed with the rest of
    /// `run_new`).
    fn run_new_t4(paths: &CcteamPaths, request: &str, team: &str) -> Result<String> {
        let slug = ccteam_core::pick_unused_slug(paths, request, team)?;
        ccteam_core::bootstrap_project(paths, &slug, request, team)?;
        Ok(slug)
    }

    #[test]
    fn run_peek_uses_state_tmux_session_for_meta_project() {
        // Serialize the env mutation (default backend is now rmux; this
        // test asserts the tmux peek path, so pin tmux while it runs).
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let mut state = ProjectState::initial_for_team("meta-cto".into(), "meta-agent".into());
        state.tmux_session = "ccteam-meta-cto".into();
        state.save(&paths.project_state("meta-cto")).unwrap();

        std::env::set_var("CCTEAM_MUX_BACKEND", "tmux");
        let result = run_peek_with_role(&paths, "meta-cto", None);
        std::env::remove_var("CCTEAM_MUX_BACKEND");

        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ccteam-meta-cto"),
            "peek should target state.tmux_session, got: {msg}",
        );
    }

    #[test]
    #[cfg(unix)]
    fn run_peek_default_rmux_does_not_shell_out_to_tmux() {
        let _lock = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);

        let old_home = std::env::var_os("CCTEAM_HOME");
        std::env::set_var("CCTEAM_HOME", tmp.path().join("ccteam-home"));
        std::env::set_var("CCTEAM_MUX_BACKEND", "rmux");

        let result = run_peek_with_role(&paths, "missing", None);

        std::env::remove_var("CCTEAM_MUX_BACKEND");
        match old_home {
            Some(path) => std::env::set_var("CCTEAM_HOME", path),
            None => std::env::remove_var("CCTEAM_HOME"),
        }

        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("rmux"),
            "peek should fail through the rmux backend, got: {msg}"
        );
    }

    /// `stop_project_chat_sessions` consults the *injected*
    /// [`ProcessBackend`], not shell `tmux` directly. The deterministic
    /// list+kill+absent semantics (which need a single live tokio runtime
    /// across spawn→list→kill) are verified in the harness layer
    /// (`ccteam_harness::stop_chat_sessions_for_slug_kills_only_that_slug`);
    /// here we only assert the CLI bridge is wired to the backend — an empty
    /// backend yields an empty result with no `tmux list-sessions` shell-out
    /// and no panic (regression: the old tmux-only path returned nothing
    /// under the default rmux backend).
    ///
    /// (We can't drive the kill end-to-end here: `block_on_async` builds a
    /// fresh current-thread runtime per call, so an `InProcBackend`'s parked
    /// `tokio::spawn` task — its liveness signal — dies with the runtime that
    /// spawned it. A single-runtime harness test is the right home.)
    #[test]
    fn stop_project_chat_sessions_consults_injected_backend() {
        use ccteam_harness::InProcBackend;

        let backend = InProcBackend::new();
        // Empty backend → no matches, both modes, no error / no tmux shell-out.
        let dry = stop_project_chat_sessions(&backend, "dev-foo", true).unwrap();
        assert!(dry.would_stop.is_empty() && dry.stopped.is_empty());
        let stop = stop_project_chat_sessions(&backend, "dev-foo", false).unwrap();
        assert!(stop.would_stop.is_empty() && stop.stopped.is_empty());
    }

    /// A tracked row from a fixture gateway state renders one row per
    /// session with its VENDOR + SID, and a **codex** session shows
    /// `live` (tracked ⇒ live regardless of vendor, no false
    /// "registered, not running").
    #[test]
    fn render_sessions_table_codex_tracked_is_live_with_vendor() {
        let tracked = vec![
            ccteam_im::gateway::TrackedSessionRow {
                sid: "s1".into(),
                project: "alpha".into(),
                role: "reviewer".into(),
                vendor: "claude".into(),
                permission_mode: "skip".into(),
                last_active: "2024-01-01T00:00:00Z".into(),
                title: None,
            },
            ccteam_im::gateway::TrackedSessionRow {
                sid: "s2".into(),
                project: "alpha".into(),
                role: "builder".into(),
                vendor: "codex".into(),
                permission_mode: "hitl".into(),
                last_active: "2024-01-02T00:00:00Z".into(),
                title: None,
            },
        ];
        let out = render_sessions_table(&tracked, &[], true);

        // Header carries the new VENDOR + LAST ACTIVE columns alongside
        // SLUG/SID/ROLE.
        assert!(out.contains("SLUG"));
        assert!(out.contains("SID"));
        assert!(out.contains("РОЛЬ"));
        assert!(out.contains("ВЕНДОР"));
        assert!(out.contains("ПОСЛЕДНЯЯ АКТИВНОСТЬ"));

        // Both rows present with their sid + vendor.
        let claude_line = out.lines().find(|l| l.contains("s1")).expect("claude row");
        assert!(claude_line.contains("reviewer"), "{claude_line}");
        assert!(claude_line.contains("claude"), "{claude_line}");
        assert!(claude_line.contains("активна"), "{claude_line}");

        let codex_line = out.lines().find(|l| l.contains("s2")).expect("codex row");
        assert!(codex_line.contains("builder"), "{codex_line}");
        assert!(codex_line.contains("codex"), "{codex_line}");
        // BUG-5: codex tracked session is live, never "registered, not running".
        assert!(codex_line.contains("активна"), "{codex_line}");
        assert!(
            !out.contains("registered, not running"),
            "BUG-5 regression: false not-running note returned: {out}"
        );

        // v0.8.22 P0-3 — rows order by last_active desc: s2 (2024-01-02) is
        // more recent than s1 (2024-01-01), so it must render first.
        let s1_pos = out.find("s1").expect("s1 present");
        let s2_pos = out.find("s2").expect("s2 present");
        assert!(
            s2_pos < s1_pos,
            "more recently active s2 must sort before s1: {out}"
        );
    }

    /// `ccteam session ls` surfaces the session-title system: a titled
    /// session shows its title in a TITLE column; an untitled one falls
    /// back to `-` (role/sid stay exactly as today alongside it).
    #[test]
    fn render_sessions_table_shows_title_with_fallback() {
        let tracked = vec![
            ccteam_im::gateway::TrackedSessionRow {
                sid: "s1".into(),
                project: "alpha".into(),
                role: "reviewer".into(),
                vendor: "claude".into(),
                permission_mode: "skip".into(),
                last_active: "2024-01-02T00:00:00Z".into(),
                title: Some("Fix the login bug".into()),
            },
            ccteam_im::gateway::TrackedSessionRow {
                sid: "s2".into(),
                project: "alpha".into(),
                role: "builder".into(),
                vendor: "codex".into(),
                permission_mode: "skip".into(),
                last_active: "2024-01-01T00:00:00Z".into(),
                title: None,
            },
        ];
        let out = render_sessions_table(&tracked, &[], true);

        assert!(
            out.contains("НАЗВАНИЕ"),
            "header must carry a TITLE column: {out}"
        );
        let titled_line = out.lines().find(|l| l.contains("s1")).expect("titled row");
        assert!(titled_line.contains("Fix the login bug"), "{titled_line}");
        let untitled_line = out
            .lines()
            .find(|l| l.contains("s2"))
            .expect("untitled row");
        assert!(
            untitled_line.contains(" - "),
            "an untitled session's TITLE cell falls back to `-`: {untitled_line}"
        );
    }

    /// A live `ccteam-chat-*` pane the daemon does not track is an
    /// orphan (role/vendor `-`); daemon-down degrades tracked rows to
    /// `registered (daemon down)` rather than erroring.
    #[test]
    fn render_sessions_table_orphan_and_daemon_down() {
        let tracked = vec![ccteam_im::gateway::TrackedSessionRow {
            sid: "s1".into(),
            project: "alpha".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
            last_active: "2024-01-01T00:00:00Z".into(),
            title: None,
        }];
        // One untracked live pane → orphan; the tracked s1's own pane is NOT
        // listed, so it must not double as an orphan.
        let live = vec!["ccteam-chat-ghost-zombie".to_string()];
        let out = render_sessions_table(&tracked, &live, false);

        let tracked_line = out.lines().find(|l| l.contains("s1")).expect("tracked");
        assert!(
            tracked_line.contains("зарегистрирована (демон недоступен)"),
            "{tracked_line}"
        );

        let orphan_line = out
            .lines()
            .find(|l| l.contains("zombie"))
            .expect("orphan row");
        assert!(orphan_line.contains("ghost"), "{orphan_line}");
        assert!(orphan_line.contains("сирота"), "{orphan_line}");
    }

    /// A tracked session's own live pane is reconciled (matched by
    /// canonical name), never re-listed as an orphan.
    #[test]
    fn render_sessions_table_tracked_pane_not_an_orphan() {
        let tracked = vec![ccteam_im::gateway::TrackedSessionRow {
            sid: "s1".into(),
            project: "alpha".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
            last_active: "2024-01-01T00:00:00Z".into(),
            title: None,
        }];
        // The live pane name matches the canonical name of the tracked s1.
        let live = vec!["ccteam-chat-alpha-s1".to_string()];
        let out = render_sessions_table(&tracked, &live, true);
        assert!(!out.contains("сирота"), "tracked pane misflagged: {out}");
        // Exactly one data row (s1), not two.
        assert_eq!(out.matches("s1").count(), 1, "{out}");
    }

    #[test]
    fn stall_verdict_classifies_silence_tiers() {
        // D4: pure age can warn, but only a file-backed watchdog timeout
        // (`chat_turn_timeout` / stuck:true in progress.jsonl) may say STUCK.
        assert_eq!(stall_verdict(None, 0), "OK");
        assert_eq!(stall_verdict(None, 5 * 60), "warn");
        assert_eq!(stall_verdict(None, 15 * 60), "warn");
        assert_eq!(stall_verdict(None, 30 * 60), "warn");
        let timeout = serde_json::json!({
            "event": ccteam_core::progress::CHAT_TURN_TIMEOUT,
            "stuck": true,
        });
        assert_eq!(stall_verdict(Some(&timeout), 0), "STUCK");
        let shint = stall_takeover_hint_for_session("dev-checkout", "s42", "stuck", "31m");
        assert_eq!(
            shint, "dev-checkout s42 stuck 31m → /chat/s/s42",
            "terse attention line: fact + direct web-chat path, no boilerplate",
        );
    }

    #[test]
    fn legacy_state_json_without_team_field_loads_as_dev() {
        // F13 backwards-compat: state.json files written by pre-M3.1
        // ccteam don't have a `team` field; serde default kicks in.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.json");
        // Hand-rolled JSON missing the `team` key:
        let body = r#"{
            "slug": "legacy",
            "created_at": "2026-05-01T00:00:00Z",
            "tmux_session": "ccteam-legacy",
            "claude_session_id": null,
            "claude_pid": null,
            "phase_state": "idle",
            "current_phase": "",
            "parallelism": "solo",
            "phase_history": [],
            "auto_loop_cycle_count": 0,
            "cost_used_usd": 0.0,
            "soft_warn_threshold_usd": 20.0,
            "hard_kill_threshold_usd": 200.0,
            "context_tokens_used": 0,
            "context_reset_threshold_tokens": 600000,
            "context_reset_count": 0,
            "last_progress_event_at": null,
            "last_event_type": null,
            "last_user_interaction_at": "2026-05-01T00:00:00Z",
            "user_attached": false,
            "user_pause_pending": false
        }"#;
        std::fs::write(&path, body).unwrap();
        let s = ProjectState::load(&path).unwrap();
        assert_eq!(s.team, "dev");
    }

    #[test]
    fn run_ls_text_says_no_projects_when_empty() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_ls(&paths, OutputFormat::Text).unwrap();
        assert!(body.contains("нет проектов"));
    }

    #[test]
    fn run_ls_json_emits_orchestrator_block_with_active_count() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_new_t4(&paths, "demo one", "dev").unwrap();
        run_new_t4(&paths, "demo two", "dev").unwrap();

        let body = run_ls(&paths, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["projects"].as_array().unwrap().len(), 2);
        assert_eq!(v["orchestrator"]["active_count"], 0);
        assert_eq!(v["orchestrator"]["max_concurrent"], 1);
    }

    #[test]
    fn f27_run_ls_text_reports_daemon_down_when_socket_unreachable() {
        // F27 — `ls` text output annotates daemon health on its first
        // line so users can disambiguate "no projects" from "daemon
        // never came up".
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_ls(&paths, OutputFormat::Text).unwrap();
        let first_line = body.lines().next().unwrap_or("");
        assert!(
            first_line == "демон: не работает",
            "expected first line `демон: не работает`; got: {first_line}",
        );
    }

    #[test]
    fn f27_run_ls_json_orchestrator_running_is_bool() {
        // F27 — `orchestrator.running` was hardcoded `null` pre-V0.2.1;
        // now it's a real bool gated on socket reachability so MCP /
        // meta-agent consumers can treat it as a status field.
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let body = run_ls(&paths, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let running = &v["orchestrator"]["running"];
        assert!(
            running.is_boolean(),
            "expected orchestrator.running:bool; got: {running:?}",
        );
        // No MCP socket listener → must be false.
        assert_eq!(running.as_bool(), Some(false));
    }

    #[test]
    fn run_ls_text_is_russian_and_json_contract_is_stable() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();

        let text = run_ls(&paths, OutputFormat::Text).unwrap();
        assert!(text.contains("демон:"));
        assert!(text.contains(&slug));

        let json = run_ls(&paths, OutputFormat::Json).unwrap();
        let value: Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["projects"][0]["slug"], slug);
        assert!(value["orchestrator"]["running"].is_boolean());
    }

    #[test]
    fn f27_run_ls_text_reports_daemon_up_on_reachable_socket() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let _daemon = fake_daemon(&paths);
        let body = run_ls(&paths, OutputFormat::Text).unwrap();
        assert!(
            body.starts_with("демон: работает"),
            "expected `демон: работает` head line on reachable socket; got:\n{body}",
        );
    }

    #[test]
    fn run_show_json_includes_state_and_artifacts() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();
        let body = run_show(&paths, &slug, OutputFormat::Json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["state"]["slug"], slug);
        // v0.8.6: `.ccteam/` keeps only state.json + workflow.yaml, so
        // there is no `spec` artifact to surface.
        assert!(
            v["artifacts"].as_object().unwrap().is_empty(),
            "v0.8.6: no .ccteam/*.md artifacts; got: {}",
            v["artifacts"],
        );
    }

    #[test]
    fn run_show_errors_for_missing_slug() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_show(&paths, "ghost", OutputFormat::Text).unwrap_err();
        assert!(format!("{err:#}").contains("ghost"));
    }

    /// Build `InitOptions` that targets a slug inside the tempdir so
    /// tests don't accidentally try to install ccteam in the ccteam repo
    /// cwd (which is fail-loud).
    fn init_opts_targeting_tmp(tmp: &TempDir, slug: &str) -> InitOptions {
        InitOptions {
            install_in: Some(tmp.path().join(slug)),
            ..InitOptions::default()
        }
    }

    /// Bare values are scoped to the per-user identity namespace;
    /// `:`-bearing values pass through verbatim; whitespace-only
    /// collapses to `None`.
    #[test]
    fn normalize_owner_scopes_bare_and_keeps_qualified() {
        assert_eq!(normalize_owner("alice").as_deref(), Some("user:alice"));
        assert_eq!(normalize_owner("user:u1").as_deref(), Some("user:u1"));
        assert_eq!(
            normalize_owner("telegram:123").as_deref(),
            Some("telegram:123")
        );
        assert_eq!(
            normalize_owner("  spaced  ").as_deref(),
            Some("user:spaced")
        );
        assert_eq!(normalize_owner("   "), None);
        assert_eq!(normalize_owner(""), None);
    }

    /// `--owner` stamps `ProjectState.owner`, normalizes bare →
    /// `user:`, and overrides an existing owner on re-init WITHOUT
    /// `--force`; a re-init without `--owner` preserves the existing
    /// owner.
    #[test]
    fn run_init_owner_sets_normalizes_and_overrides() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let dir = tmp.path().join("owned-demo");
        let state_path = dir.join(".ccteam").join("state.json");

        // 1. `--owner user:u1` → verbatim (already qualified).
        run_init(
            &paths,
            InitOptions {
                install_in: Some(dir.clone()),
                owner: Some("user:u1".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let st = ccteam_core::ProjectState::load(&state_path).unwrap();
        assert_eq!(st.owner.as_deref(), Some("user:u1"));

        // 2. re-init WITHOUT `--owner` preserves the existing owner (no force).
        run_init(
            &paths,
            InitOptions {
                install_in: Some(dir.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let st = ccteam_core::ProjectState::load(&state_path).unwrap();
        assert_eq!(
            st.owner.as_deref(),
            Some("user:u1"),
            "re-init without --owner must preserve the existing owner"
        );

        // 3. re-init WITH a new bare `--owner` overrides (no `--force`) and is
        //    scoped to the per-user identity namespace.
        run_init(
            &paths,
            InitOptions {
                install_in: Some(dir.clone()),
                owner: Some("u2".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let st = ccteam_core::ProjectState::load(&state_path).unwrap();
        assert_eq!(
            st.owner.as_deref(),
            Some("user:u2"),
            "bare --owner is scoped to user: and overrides without --force"
        );
    }

    /// A plain `ccteam init` (no `--owner`) leaves `owner == None`.
    #[test]
    fn run_init_without_owner_leaves_owner_none() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_init(&paths, init_opts_targeting_tmp(&tmp, "ownerless-demo")).unwrap();
        let state_path = tmp
            .path()
            .join("ownerless-demo")
            .join(".ccteam")
            .join("state.json");
        let st = ccteam_core::ProjectState::load(&state_path).unwrap();
        assert!(st.owner.is_none(), "no --owner ⇒ owner stays None");
    }

    #[test]
    fn run_init_creates_global_skeleton_and_unpacks_helpers() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let report = run_init(&paths, init_opts_targeting_tmp(&tmp, "scaffold-demo")).unwrap();
        // v0.8.6 D1.1: init creates exactly the canonical home-layout
        // manifest. `hooks/` is materialized by the dispatcher install
        // step rather than the skeleton loop, but it must still exist.
        for sub in ccteam_core::canonical_home_dirs() {
            assert!(
                paths.root.join(sub).is_dir(),
                "init must create canonical home dir {}",
                paths.root.join(sub).display()
            );
        }
        // The orchestrator-era subdirs are no longer created by init —
        // nothing reads them post-W2 and they used to trip the doctor
        // home-drift check on a brand-new install.
        for dead in ["phases", "inbox", "control"] {
            assert!(
                !paths.root.join(dead).exists(),
                "init must NOT create orchestrator-era dir {}",
                paths.root.join(dead).display()
            );
        }
        assert!(report.contains("ccteam init"));
        assert!(report.contains("дальше"));
    }

    /// Regression: a brand-new `ccteam init` must not create any
    /// top-level `~/.ccteam` directory outside the canonical manifest
    /// (`canonical_home_dirs()`). Init once stamped
    /// `phases/templates/inbox/control` — a fresh install reported four
    /// self-inflicted drift dirs on the very next `ccteam doctor`.
    #[test]
    fn run_init_leaves_no_home_layout_drift() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_init(&paths, init_opts_targeting_tmp(&tmp, "drift-demo")).unwrap();
        // Re-derive the same drift set `ccteam doctor` reports: any
        // top-level dir under `~/.ccteam` not in the canonical manifest.
        let drift: Vec<String> = std::fs::read_dir(&paths.root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| !ccteam_core::canonical_home_dirs().contains(&name.as_str()))
            .collect();
        assert!(
            drift.is_empty(),
            "a fresh `ccteam init` must produce zero home drift; got: {drift:?}",
        );
    }

    #[test]
    fn run_init_is_idempotent_and_preserves_user_workflow_yaml() {
        // V0.5.0 F101: HELPER_TEMPLATES is empty so the V0.4.x
        // `~/.ccteam/templates/review-with-user-loop.md` idempotency
        // probe is gone. workflow.yaml is the new canonical
        // "preserve without --force, overwrite with --force" artifact —
        // it's user-territory by design (see `force_overwrites_user_workflow_and_agents`).
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        run_init(&paths, init_opts_targeting_tmp(&tmp, "idem-demo")).unwrap();
        let path = tmp
            .path()
            .join("idem-demo")
            .join(".ccteam")
            .join("workflow.yaml");
        std::fs::write(&path, "USER EDIT").unwrap();
        run_init(&paths, init_opts_targeting_tmp(&tmp, "idem-demo")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDIT");
        run_init(
            &paths,
            InitOptions {
                force: true,
                install_in: Some(tmp.path().join("idem-demo")),
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "USER EDIT");
    }

    /// Fresh install scaffolds the project skeleton AND registers in
    /// config.yaml.
    #[test]
    fn run_init_fresh_install_scaffolds_and_registers() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("f72-fresh");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some("f72-fresh".into()),
                team: Some("dev".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert!(target.join(".ccteam").join("state.json").is_file());
        assert!(
            target.join(".ccteam").join("workflow.yaml").is_file(),
            "V0.4.6 F83: workflow.yaml must land in .ccteam/, not project root",
        );
        assert!(
            !target.join("workflow.yaml").exists(),
            "V0.4.6 F83: workflow.yaml must NOT be at the project root after fresh init",
        );
        // v0.9.0 (engine neutralization): fresh init seeds NO role — the default
        // session is roleless (bare vendor reads the project CLAUDE.md/AGENTS.md).
        assert!(
            !target
                .join(".claude")
                .join("agents")
                .join("cto.md")
                .exists(),
            "v0.9.0: init seeds no cto.md (roleless default)"
        );
        // v0.8.6: init no longer writes the `.ccteam/agents` neutral copy
        // nor the `.ccteam/skills` placeholder layout — `.ccteam/` holds
        // only state.json + workflow.yaml.
        assert!(
            !target.join(".ccteam").join("agents").exists(),
            "v0.8.6: .ccteam/agents neutral copy must NOT be written",
        );
        assert!(
            !target.join(".ccteam").join("skills").exists(),
            "v0.8.6: .ccteam/skills placeholder must NOT be written",
        );

        let cfg = ccteam_core::load_ccteam_config(&paths.root).unwrap();
        assert_eq!(cfg.projects.len(), 1);
        assert_eq!(cfg.projects[0].slug, "f72-fresh");
        assert_eq!(cfg.projects[0].team, "dev");
    }

    #[test]
    fn run_init_next_block_names_shortest_path_and_role_modes() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let out = run_init(
            &paths,
            InitOptions {
                install_in: Some(tmp.path().join("onboard-demo")),
                slug: Some("onboard-demo".into()),
                team: Some("dev".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        for needle in [
            "1. install:",
            "2. init:",
            "3. config:",
            "4. start:",
            "5. cd:",
            "по умолчанию сессии без роли",
            "создайте рабочие роли в .claude/agents/<role>.md",
            "docs/usage.md",
        ] {
            assert!(
                out.contains(needle),
                "init next block missing {needle:?}:\n{out}"
            );
        }
    }

    #[test]
    fn run_show_text_is_russian_while_json_fields_remain_stable() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();
        let text = run_show(&paths, &slug, OutputFormat::Text).unwrap();
        assert!(text.contains("текущая фаза"), "{text}");
        assert!(text.contains("стоимость (24ч)"), "{text}");
        let json = run_show(&paths, &slug, OutputFormat::Json).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["state"]["slug"], slug);
    }

    #[test]
    fn run_show_session_row_uses_russian_context_label() {
        let state = ProjectState::initial("demo".into());
        let sessions = [ccteam_core::ActiveSessionInfo {
            role: "worker".into(),
            session_id: "session-1".into(),
            job_id: Some("job-12345678".into()),
            cwd: None,
            started_at: "2026-01-01T00:00:00Z".into(),
            cost_usd: 1.25,
            model: Some("claude-sonnet".into()),
            context_remaining_pct: Some(42.0),
        }];
        let text = render_show_text(
            &state,
            &ccteam_core::CostSummary::default(),
            &[],
            &Map::new(),
            &sessions,
        );
        assert!(text.contains("контекст  42%"), "{text}");
        assert!(!text.contains("ctx "), "{text}");
    }

    #[test]
    fn project_skill_entity_error_is_russian() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".agents/skills");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not a directory").unwrap();
        let err = ensure_project_skill_entity(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("должна быть настоящим каталогом"));
    }

    /// `ccteam init` without `--mode` defaults to artifact-driven; no
    /// `__lead.md` is scaffolded.
    #[test]
    fn run_init_default_mode_does_not_scaffold_lead() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("artifact-default");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let lead = target.join(".claude").join("agents").join("__lead.md");
        assert!(
            !lead.exists(),
            "__lead.md must NOT be scaffolded in default mode"
        );
        let wf = target.join(".ccteam").join("workflow.yaml");
        let body = std::fs::read_to_string(&wf).unwrap();
        assert!(
            !body.contains("mode: agent-team"),
            "default workflow.yaml must NOT declare agent-team mode",
        );
        // No F94 hooks in the default (managed) settings layer. v0.8.6
        // W2b — ccteam writes to settings.local.json, not settings.json.
        let settings_body =
            std::fs::read_to_string(target.join(".claude").join("settings.local.json")).unwrap();
        assert!(
            !settings_body.contains("TeammateIdle"),
            "default settings.local.json must NOT include team_* hooks",
        );
    }

    /// `ccteam attach <slug>` against an agent-team project without a
    /// written snapshot returns a friendly error telling the user to run
    /// `ccteam start <slug>` first.
    #[test]
    fn run_attach_agent_team_missing_snapshot_errors_with_hint() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "no-snapshot";
        let target = paths.projects_root.join(slug);
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some(slug.into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        // `run_attach`'s agent-team branch keys off `mode: agent-team` in
        // workflow.yaml; `ccteam init` scaffolds artifact-driven, so
        // overwrite with a minimal agent-team spec for this teardown test.
        std::fs::write(
            target.join(".ccteam").join("workflow.yaml"),
            format!(
                "name: {slug}\nmode: agent-team\nagent_team:\n  team_name: {slug}\n  \
                 lead_seed: |\n    test mission\n  cleanup_on_stop: force-kill\nagents: {{}}\n"
            ),
        )
        .unwrap();
        let err = run_attach(&paths, slug).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("team-snapshot") || msg.contains("ccteam start"),
            "error must mention snapshot / hint at `ccteam start`; got: {msg}",
        );
    }

    /// `ccteam attach <slug>` against an agent-team project WITH a
    /// snapshot containing `lead_session_id` reads the lead id and would
    /// exec `claude attach <id>`. We can't actually exec here, but
    /// `read_agent_team_lead_session_id` is testable directly.
    #[test]
    fn read_agent_team_lead_session_id_resolves_from_snapshot() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "with-snapshot";
        let target = paths.projects_root.join(slug);
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some(slug.into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        // `read_agent_team_lead_session_id` keys off `mode: agent-team` in
        // workflow.yaml; `ccteam init` scaffolds artifact-driven, so
        // overwrite with a minimal agent-team spec.
        std::fs::write(
            target.join(".ccteam").join("workflow.yaml"),
            format!(
                "name: {slug}\nmode: agent-team\nagent_team:\n  team_name: {slug}\n  \
                 lead_seed: |\n    test mission\n  cleanup_on_stop: force-kill\nagents: {{}}\n"
            ),
        )
        .unwrap();
        // Fake snapshot writeup.
        let snapshot_path = target.join(".ccteam").join("team-snapshot.json");
        std::fs::write(
            &snapshot_path,
            serde_json::json!({
                "slug": slug,
                "lead_session_id": "deadbeef123",
                "team_name": "with-snapshot",
                "teammate_mode": "in-process",
                "cleanup_on_stop": "force-kill",
                "auto_spawn_teammates": false,
                "suggested_teammates": [],
                "spawned_at": "2026-05-17T12:00:00Z",
            })
            .to_string(),
        )
        .unwrap();
        let lead_id = read_agent_team_lead_session_id(&paths, slug)
            .unwrap()
            .unwrap();
        assert_eq!(lead_id, "deadbeef123");
    }

    /// For artifact-driven projects, `read_agent_team_lead_session_id`
    /// returns Ok(None) so the caller falls through to the tmux / bg
    /// path.
    #[test]
    fn read_agent_team_lead_session_id_returns_none_for_artifact_driven() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = "art-fall";
        let target = paths.projects_root.join(slug);
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target),
                slug: Some(slug.into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let res = read_agent_team_lead_session_id(&paths, slug).unwrap();
        assert!(res.is_none(), "artifact-driven must return None");
    }

    /// Re-running on an existing ccteam project preserves user-edited
    /// workflow.yaml + agents/*.md. workflow.yaml lives in `.ccteam/`,
    /// not the root.
    #[test]
    fn run_init_refresh_preserves_user_workflow_and_agents() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("f72-refresh");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let wf_path = target.join(".ccteam").join("workflow.yaml");
        std::fs::write(&wf_path, "USER WORKFLOW\n").unwrap();
        // v0.9.0: init no longer creates `.claude/agents/`; the user authors it.
        std::fs::create_dir_all(target.join(".claude").join("agents")).unwrap();
        std::fs::write(
            target.join(".claude").join("agents").join("cto.md"),
            "USER AGENT\n",
        )
        .unwrap();

        // Re-run: refresh should preserve user files.
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&wf_path).unwrap(),
            "USER WORKFLOW\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.join(".claude").join("agents").join("cto.md")).unwrap(),
            "USER AGENT\n"
        );
    }

    /// `--force` re-runs overwrite user files. workflow.yaml lives in
    /// `.ccteam/`, not the root.
    #[test]
    fn run_init_force_overwrites_user_workflow_and_agents() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("f72-force");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let wf_path = target.join(".ccteam").join("workflow.yaml");
        std::fs::write(&wf_path, "USER WORKFLOW\n").unwrap();
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                force: true,
                ..InitOptions::default()
            },
        )
        .unwrap();
        assert_ne!(
            std::fs::read_to_string(&wf_path).unwrap(),
            "USER WORKFLOW\n"
        );
    }

    /// Invalid slug grammar fails loud at the CLI boundary, before any
    /// directory is created.
    #[test]
    fn run_init_rejects_invalid_slug_grammar() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let err = run_init(
            &paths,
            InitOptions {
                install_in: Some(tmp.path().join("ok-dir")),
                slug: Some("做一个 todo cli".into()),
                ..InitOptions::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("[a-z0-9-]+"),
            "expected slug-grammar fail-loud; got: {msg}",
        );
        assert!(
            !tmp.path().join("ok-dir").join(".ccteam").exists(),
            "no .ccteam/ should have been created when slug is invalid",
        );
    }

    /// `ccteam init` rejects a slug collision when the existing
    /// registry entry points at a different physical path. Same slug +
    /// same path is OK (refresh).
    #[test]
    fn run_init_rejects_slug_collision_at_different_path() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let first = tmp.path().join("first");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(first.clone()),
                slug: Some("conflicty".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();

        let second = tmp.path().join("second");
        let err = run_init(
            &paths,
            InitOptions {
                install_in: Some(second),
                slug: Some("conflicty".into()),
                ..InitOptions::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already registered"),
            "expected slug-collision error; got: {msg}",
        );
    }

    /// Re-running on the SAME path with the same slug is a legitimate
    /// refresh, not a collision.
    #[test]
    fn run_init_same_slug_same_path_is_refresh_not_collision() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("refreshable");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some("refreshable".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        // Second invocation: should succeed (refresh).
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target),
                slug: Some("refreshable".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn run_init_rejects_retired_slug_before_touching_project_files() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let target = tmp.path().join("retired-project");
        run_init(
            &paths,
            InitOptions {
                install_in: Some(target.clone()),
                slug: Some("retired-project".into()),
                ..InitOptions::default()
            },
        )
        .unwrap();
        let workflow = target.join(".ccteam").join("workflow.yaml");
        std::fs::write(&workflow, "USER SENTINEL\n").unwrap();
        ccteam_harness::execution::progress_bridge::mark_progress_retired(
            &paths.progress_jsonl("retired-project"),
        )
        .unwrap();

        let error = run_init(
            &paths,
            InitOptions {
                install_in: Some(target),
                slug: Some("retired-project".into()),
                force: true,
                ..InitOptions::default()
            },
        )
        .unwrap_err();
        // `{:#}` renders the whole chain: the neutral `ccteam init` prefix plus
        // `preflight_project_upsert`'s own, case-accurate reason.
        let error = format!("{error:#}");

        assert!(error.contains("permanently retired"), "{error}");
        assert!(error.starts_with("ccteam init"), "{error}");
        assert_eq!(
            std::fs::read_to_string(workflow).unwrap(),
            "USER SENTINEL\n"
        );
    }

    /// Installing in the ccteam source repo itself is fail-loud
    /// (CLAUDE.md §六 — avoids circular hook setup).
    #[test]
    fn run_init_refuses_to_install_in_ccteam_repo() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Plant the two markers `is_ccteam_repo` checks for.
        let fake_repo = tmp.path().join("ccteam-mirror");
        std::fs::create_dir_all(fake_repo.join("crates").join("ccteam-cli")).unwrap();
        std::fs::write(fake_repo.join("Cargo.toml"), "[workspace]\n").unwrap();

        let err = run_init(
            &paths,
            InitOptions {
                install_in: Some(fake_repo),
                ..InitOptions::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("самом репозитории ccteam"),
            "expected fail-loud message; got: {msg}",
        );
        assert!(
            msg.contains("--force"),
            "fail-loud message must point to the --force escape; got: {msg}",
        );
    }

    /// `--force` overrides the ccteam-repo refusal so self-hosting /
    /// dogfooding installs inside the ccteam source tree can proceed
    /// when the user explicitly opts in.
    #[test]
    fn run_init_force_overrides_ccteam_repo_refusal() {
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        // Plant the two markers `is_ccteam_repo` checks for.
        let fake_repo = tmp.path().join("ccteam-mirror");
        std::fs::create_dir_all(fake_repo.join("crates").join("ccteam-cli")).unwrap();
        std::fs::write(fake_repo.join("Cargo.toml"), "[workspace]\n").unwrap();

        let out = run_init(
            &paths,
            InitOptions {
                install_in: Some(fake_repo.clone()),
                slug: Some("self-host".into()),
                force: true,
                ..InitOptions::default()
            },
        )
        .expect("--force must override ccteam-repo refusal");
        assert!(
            out.contains("ccteam init"),
            "expected init summary header; got: {out}",
        );

        // Project install must have actually written the ccteam files.
        let state_path = fake_repo.join(".ccteam").join("state.json");
        assert!(
            state_path.exists(),
            "state.json must exist after --force init; checked {}",
            state_path.display()
        );
        let workflow_path = fake_repo.join(".ccteam").join("workflow.yaml");
        assert!(
            workflow_path.exists(),
            "workflow.yaml must exist after --force init; checked {}",
            workflow_path.display()
        );
    }

    #[test]
    fn run_progress_emits_existing_events_without_tail() {
        ensure_isolation();
        let tmp = TempDir::new().unwrap();
        let paths = fresh_paths(&tmp);
        let slug = run_new_t4(&paths, "demo", "dev").unwrap();
        progress::append_event(
            &paths.progress_jsonl(&slug),
            &json!({"event": "session_start", "ts": "2026-05-05T00:00:00Z"}),
        )
        .unwrap();
        // run_progress writes to stdout which is awkward to capture in
        // unit tests; verify the underlying file content as a proxy.
        let path = paths.progress_jsonl(&slug);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("session_start"));
    }

    // V0.6.6 F171 — render-path coverage for the FAIL branch (binary
    // E2E test in `tests/doctor_verify_mcp_test.rs` only exercises the
    // PASS branch since shipping a STUB tool just to fail-test the
    // binary would defeat the gate). These unit tests pin the
    // human-readable + JSON shape for a synthetic STUB scenario so the
    // FAIL message format stays stable across refactors.

    #[test]
    fn verify_mcp_run_on_live_surface_passes_with_zero_stubs() {
        let report = run_verify_mcp();
        assert!(report.ok(), "live MCP surface must be 0-STUB");
        assert_eq!(report.stub_count, 0);
        assert!(report.unexpected_stubs.is_empty());
        // total_tools must match the mcp_serve spec — keeps F171 in
        // sync with `tool_definitions_count_matches_spec` (live truth).
        assert_eq!(report.total_tools, report.active_count);
        assert_eq!(
            report.total_tools, 8,
            "ships 8 tools (v0.9 T1 cull; 2026-07-26 screenshot cull + status beacon alias)"
        );
    }

    #[test]
    fn config_mcp_report_renders_live_tool_count() {
        // The `config mcp` report must print the live tool count from
        // `tool_definitions()`, never a hard-coded number that drifts
        // (it was stuck at "9" while the surface grew to 27). Mirrors
        // `tool_definitions_count_matches_spec` so the rendered string
        // can never silently diverge from the registered surface. Uses
        // the pure body renderer with a synthetic path so it never
        // touches the real ~/.claude.json.
        let total = run_verify_mcp().total_tools;
        let report = render_install_mcp_body(
            std::path::Path::new("/tmp/fake-claude.json"),
            std::path::Path::new("/tmp/fake-codex/config.toml"),
            std::path::Path::new("/tmp/fake-grok/config.toml"),
            std::path::Path::new("/tmp/fake-opencode/opencode.json"),
            std::path::Path::new("/tmp/fake-kimi/mcp.json"),
        );
        assert!(
            report.contains(&format!("поверхность инструментов: {total}")),
            "report must interpolate live tool count {total}: {report}",
        );
        // The stale "(interfaces §12.2)" tag must be gone.
        assert!(
            !report.contains("interfaces §12.2"),
            "stale section tag must be dropped: {report}",
        );
        // All five vendor targets must be named in the report body
        // (vendor symmetry: any vendor's main session can orchestrate).
        for target in [
            "/tmp/fake-claude.json",
            "/tmp/fake-codex/config.toml",
            "/tmp/fake-grok/config.toml",
            "/tmp/fake-opencode/opencode.json",
            "/tmp/fake-kimi/mcp.json",
        ] {
            assert!(
                report.contains(target),
                "report must name the {target} target path: {report}",
            );
        }
        let pi_notice = ccteam_core::host_registry::AgentProbeSpec::by_vendor("pi")
            .and_then(ccteam_core::host_registry::AgentProbeSpec::tool_surface_notice)
            .unwrap();
        assert!(
            report.contains(&pi_notice),
            "Pi must be described honestly without inventing a config target: {report}"
        );
    }

    #[test]
    fn verify_mcp_report_render_text_fail_path_emits_verdict_fail() {
        let mut per_group = std::collections::BTreeMap::new();
        per_group.insert(
            "workflow".to_string(),
            GroupStats {
                active: 14,
                stub: 1,
            },
        );
        let synth = VerifyMcpReport {
            total_tools: 27,
            stub_count: 1,
            active_count: 26,
            tool_list: vec!["ccteam__workflow_synth_stub".to_string()],
            per_group,
            unexpected_stubs: vec!["ccteam__workflow_synth_stub".to_string()],
        };
        assert!(!synth.ok());
        let text = synth.render_text();
        assert!(text.contains("вердикт: FAIL"), "text: {text}");
        assert!(
            text.contains("неожиданные STUB"),
            "text must list unexpected STUBs section: {text}",
        );
        assert!(
            text.contains("ccteam__workflow_synth_stub"),
            "stub tool name must appear in report: {text}",
        );
        assert!(text.contains("14 активных / 1 заглушек"), "got: {text}");
    }

    #[test]
    fn verify_mcp_report_render_json_fail_path_sets_ok_false() {
        let mut per_group = std::collections::BTreeMap::new();
        per_group.insert("advise".to_string(), GroupStats { active: 1, stub: 1 });
        let synth = VerifyMcpReport {
            total_tools: 28,
            stub_count: 1,
            active_count: 27,
            tool_list: vec!["ccteam__advise_synth_stub".to_string()],
            per_group,
            unexpected_stubs: vec!["ccteam__advise_synth_stub".to_string()],
        };
        let j = synth.render_json();
        let v: Value = serde_json::from_str(&j).expect("render_json emits valid JSON");
        assert_eq!(v["ok"], Value::Bool(false));
        assert_eq!(v["stub_count"], Value::Number(1.into()));
        assert_eq!(v["total_tools"], Value::Number(28.into()));
        let unexpected = v["unexpected_stubs"].as_array().unwrap();
        assert_eq!(unexpected.len(), 1);
        assert_eq!(
            unexpected[0],
            Value::String("ccteam__advise_synth_stub".into())
        );
    }

    // --- Lark/Feishu config (run_config_set_lark_creds) ---------------
    //
    // Deterministic: a one-shot std TCP listener stands in for the Feishu
    // `tenant_access_token/internal` endpoint, so the credential validate
    // makes a real HTTP round-trip without touching the network. No env
    // mutation and the creds path is a tempdir, so this is safe in a lib
    // `#[cfg(test)]` module (CLAUDE.md §六).

    /// Spawn a single-shot HTTP/1.1 responder on `127.0.0.1:0` that replies
    /// to the first connection with `body` (status 200, JSON) and exits.
    /// Returns `http://127.0.0.1:<port>` (a Lark `api_base`).
    fn spawn_oneshot_http(body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                // Drain the complete reqwest POST before replying. Returning
                // while the client is still writing can make hyper observe a
                // closed socket and decode an empty response body.
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                let mut header_end = None;
                let mut content_length = 0usize;
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            req.extend_from_slice(&buf[..n]);
                            if header_end.is_none() {
                                if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let end = pos + 4;
                                    header_end = Some(end);
                                    let headers = String::from_utf8_lossy(&req[..end]);
                                    content_length = headers
                                        .lines()
                                        .find_map(|line| {
                                            let (name, value) = line.split_once(':')?;
                                            name.eq_ignore_ascii_case("content-length")
                                                .then(|| value.trim().parse::<usize>().ok())
                                                .flatten()
                                        })
                                        .unwrap_or(0);
                                }
                            }
                            if let Some(end) = header_end {
                                if req.len().saturating_sub(end) >= content_length {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn lark_creds_persist_and_preserve_existing_telegram() {
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");

        // Seed an existing Telegram entry that must survive the Lark write.
        let seed = ccteam_im::credentials::Credentials {
            telegram: Some(ccteam_im::credentials::TelegramCreds {
                bot_token: "tg-seed-token".into(),
                allowed_chat_ids: vec!["111".into()],
            }),
            ..Default::default()
        };
        ccteam_im::credentials::save(&creds_path, &seed).unwrap();

        // Mock the tenant_access_token success shape (code=0 + a token).
        let base = spawn_oneshot_http(
            r#"{"code":0,"msg":"ok","tenant_access_token":"t-tok","expire":7200}"#,
        );

        let out = run_config_set_lark_creds_with_base(
            "cli_app_42",
            "secret_42",
            vec!["ou_alice".into(), "ou_bob".into()],
            true, // Feishu / CN
            &base,
            Some(&creds_path),
        )
        .expect("lark creds validate + persist must succeed against the mock");
        assert!(
            out.contains("Lark/Feishu credentials saved") && out.contains("2 open_id(s) allowed"),
            "summary must confirm the save + allowlist count; got: {out}"
        );

        // Reload and assert the lark block landed with the right fields …
        let reloaded = ccteam_im::credentials::load(Some(&creds_path)).unwrap();
        let lark = reloaded.lark.expect("lark creds must be persisted");
        assert_eq!(lark.app_id, "cli_app_42");
        assert_eq!(lark.app_secret, "secret_42");
        assert_eq!(lark.allowed_user_ids, vec!["ou_alice", "ou_bob"]);
        assert!(lark.use_feishu, "use_feishu must round-trip true");

        // … and that the pre-existing Telegram entry was preserved (the
        // merge must not clobber sibling platforms).
        let tg = reloaded.telegram.expect("telegram must survive the merge");
        assert_eq!(tg.bot_token, "tg-seed-token");
        assert_eq!(tg.allowed_chat_ids, vec!["111"]);
    }

    #[test]
    fn lark_creds_empty_allowlist_warns_fail_closed() {
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");
        let base = spawn_oneshot_http(r#"{"code":0,"msg":"ok","tenant_access_token":"t-tok"}"#);

        // Empty allowlist + Lark international region.
        let out = run_config_set_lark_creds_with_base(
            "cli_x",
            "secret_x",
            vec![],
            false,
            &base,
            Some(&creds_path),
        )
        .expect("empty allowlist still persists (it is a valid, if locked-down, config)");
        assert!(
            out.contains("fail-closed") && out.contains("NO ONE"),
            "empty allowlist must surface the fail-closed warning; got: {out}"
        );
        assert!(
            out.contains("Lark (intl"),
            "region note must reflect use_feishu=false; got: {out}"
        );
        let reloaded = ccteam_im::credentials::load(Some(&creds_path)).unwrap();
        let lark = reloaded.lark.unwrap();
        assert!(lark.allowed_user_ids.is_empty());
        assert!(!lark.use_feishu);
    }

    #[test]
    fn lark_creds_bad_app_creds_error_no_persist() {
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");
        // Feishu signals bad credentials as a 200 with a non-zero `code`.
        let base = spawn_oneshot_http(r#"{"code":10003,"msg":"invalid app_secret"}"#);

        let err = run_config_set_lark_creds_with_base(
            "cli_bad",
            "wrong_secret",
            vec!["ou_a".into()],
            true,
            &base,
            Some(&creds_path),
        )
        .expect_err("a non-zero Feishu code must surface as an error, not a saved token");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid app_secret") || msg.contains("10003"),
            "error must carry the upstream Feishu reason; got: {msg}"
        );
        // Nothing should have been written on the validate failure.
        assert!(
            !creds_path.exists(),
            "credentials file must not be created when validation fails"
        );
    }

    #[test]
    fn lark_creds_empty_app_id_rejected_before_network() {
        // Guard: both app_id + app_secret are required; we never even
        // reach the validate call (a deliberately-unreachable base URL).
        let tmp = TempDir::new().unwrap();
        let creds_path = tmp.path().join("im/credentials.json");
        let err = run_config_set_lark_creds_with_base(
            "   ",
            "secret",
            vec![],
            true,
            "http://127.0.0.1:1", // would refuse-connect if reached
            Some(&creds_path),
        )
        .expect_err("blank app_id must be rejected up front");
        assert!(
            err.to_string()
                .contains("app_id and app_secret are both required"),
            "must name the missing-field guard; got: {err}"
        );
    }
}
