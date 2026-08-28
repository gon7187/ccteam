//! Source gate: `ccteam_core::collect_projects` may be called from exactly ONE
//! place in `ccteam-web` — the `AppState` accessor pair in `src/state.rs`.
//!
//! Why a source gate and not a runtime test: the hazard is a *blocking* call on
//! an async worker. The catalog walk takes the stable per-project progress lock
//! and reads config/registry files; an inline call parks a tokio worker inside
//! `flock`, and with as many such requests in flight as the runtime has workers
//! the whole HTTP surface stalls — including the `DELETE /api/v1/projects/{slug}`
//! that would release the lock. That never shows up as a failing unit test, it
//! shows up as a wedged daemon, and it regressed once already: three call sites
//! were wrapped in `spawn_blocking` one at a time while four others kept calling
//! inline.
//!
//! `AppState::collect_projects_blocking` (sync callers, already on a blocking
//! thread) and `AppState::collect_projects` / `visible_project_slugs` (async
//! callers, the accessor owns the `spawn_blocking`) are the only two doors, so a
//! new handler cannot reintroduce the hazard by forgetting a wrapper.

use std::path::{Path, PathBuf};

/// Only this file may name the core function.
const ALLOWED: &str = "state.rs";

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read ccteam-web/src") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn core_collect_projects_is_called_only_through_the_app_state_accessor() {
    let root = src_root();
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        root.display()
    );

    let mut offenders = Vec::new();
    let mut allowed_hits = 0usize;
    for file in &files {
        let body = std::fs::read_to_string(file).expect("read source");
        for (idx, line) in body.lines().enumerate() {
            // Both spellings of the core call: the re-export and the module path.
            if !(line.contains("ccteam_core::collect_projects(")
                || line.contains("queries::collect_projects("))
            {
                continue;
            }
            let name = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name == ALLOWED {
                allowed_hits += 1;
                continue;
            }
            offenders.push(format!(
                "{}:{}: {}",
                file.strip_prefix(&root).unwrap_or(file).display(),
                idx + 1,
                line.trim()
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "`ccteam_core::collect_projects` must only be called from src/{ALLOWED} \
         (use `AppState::collect_projects().await` on async paths, or \
         `collect_projects_blocking()` when already on a blocking thread); found:\n{}",
        offenders.join("\n"),
    );
    assert_eq!(
        allowed_hits, 1,
        "src/{ALLOWED} should hold exactly one call to the core function \
         (the accessor); a second one means the accessor was forked",
    );
}
