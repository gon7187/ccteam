//! V0.8 W2c — shared OS-level process-liveness probe.
//!
//! Both `ClaudeTuiAdapter` (F164 reattach) and future Codex liveness
//! checks need to answer "is the process behind this mux session's pane
//! actually the vendor binary we expect?" — distinct from "does the mux
//! session exist?" (a tmux session can outlive its dead pane via
//! `remain-on-exit on`).
//!
//! The probe walks every pane PID the backend reports (`list_pane_pids`)
//! and asks the OS for the command name via `ps -p <pid> -o comm=`.
//!
//! ## Red-line compliance
//!
//! This reads only the process **command name** (`comm`), never pane
//! **text content**. `ps` is an OS-level inspection — NOT a mux-level
//! concern — so it stays a direct subprocess rather than a trait method
//! (the `ProcessBackend` surface deliberately exposes pane PIDs but not
//! "what binary is the PID running"). The banned `tmux capture-pane`
//! pane-scrape is never invoked here.

use std::time::Duration;

const PROCESS_SETTLE_TIMEOUT: Duration = Duration::from_millis(250);
const PROCESS_SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Async liveness probe: returns `true` iff any pane PID reported by
/// `backend.list_pane_pids(id)` has a `ps -p <pid> -o comm=` command
/// name that **contains** `needle` (e.g. `"claude"` matches `claude`,
/// `claude-code`, `fake-claude`; `"codex"` matches `codex`).
///
/// A mux may publish a pane before the pane process has completed `exec`, so
/// a non-matching live PID is re-probed for a short bounded settle window.
/// Pane PIDs are refreshed on every attempt so a respawned pane is never
/// identified from a stale PID snapshot. An empty PID list still returns
/// `false` immediately.
///
/// Returns `false` when the session has no panes, all pids are gone, or none
/// match before the settle deadline. PID `0` (the sentinel some tmux states
/// surface) is skipped. A non-successful `ps` result means that PID is already
/// gone; if no reported PID is live, the probe returns `false` immediately.
pub async fn pane_runs_process(
    backend: &dyn crate::PaneBackend,
    id: &crate::MuxSessionId,
    needle: &str,
) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + PROCESS_SETTLE_TIMEOUT;
    loop {
        let pids = backend.list_pane_pids(id).await?;
        if pids.is_empty() {
            return Ok(false);
        }

        let mut saw_live_pid = false;
        for pid in pids {
            if pid == 0 {
                continue;
            }
            let out = tokio::process::Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "comm="])
                .output()
                .await?;
            if !out.status.success() {
                continue;
            }
            saw_live_pid = true;
            if String::from_utf8_lossy(&out.stdout).trim().contains(needle) {
                return Ok(true);
            }
        }

        if !saw_live_pid || tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(PROCESS_SETTLE_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BackendKind, MuxEventStream, MuxSessionId, MuxSessionSpec, PaneBackend, ProcessBackend,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestPaneBackend {
        first_pids: Vec<u32>,
        settled_pids: Option<Vec<u32>>,
        list_calls: AtomicUsize,
    }

    impl TestPaneBackend {
        fn fixed(pids: Vec<u32>) -> Self {
            Self {
                first_pids: pids,
                settled_pids: None,
                list_calls: AtomicUsize::new(0),
            }
        }

        fn settling(first_pids: Vec<u32>, settled_pids: Vec<u32>) -> Self {
            Self {
                first_pids,
                settled_pids: Some(settled_pids),
                list_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProcessBackend for TestPaneBackend {
        async fn spawn(&self, spec: MuxSessionSpec) -> anyhow::Result<MuxSessionId> {
            Ok(MuxSessionId::new(spec.name))
        }

        async fn exists(&self, _id: &MuxSessionId) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn send_text(&self, _id: &MuxSessionId, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn send_enter(&self, _id: &MuxSessionId) -> anyhow::Result<()> {
            Ok(())
        }

        async fn subscribe(&self, _id: &MuxSessionId) -> anyhow::Result<MuxEventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn register_pattern(
            &self,
            _id: &MuxSessionId,
            _regex_id: String,
            _regex: String,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn kill(&self, _id: &MuxSessionId) -> anyhow::Result<()> {
            Ok(())
        }

        async fn list_sessions(&self) -> anyhow::Result<Vec<MuxSessionId>> {
            Ok(Vec::new())
        }

        fn backend_kind(&self) -> BackendKind {
            BackendKind::Tmux
        }
    }

    #[async_trait::async_trait]
    impl PaneBackend for TestPaneBackend {
        async fn capture(
            &self,
            _id: &MuxSessionId,
            _lines: usize,
            _with_ansi: bool,
        ) -> anyhow::Result<Vec<u8>> {
            Ok(Vec::new())
        }

        async fn pane_dims(&self, _id: &MuxSessionId) -> anyhow::Result<Option<(u16, u16)>> {
            Ok(None)
        }

        async fn pane_pid(&self, _id: &MuxSessionId) -> anyhow::Result<Option<i32>> {
            Ok(None)
        }

        async fn list_pane_pids(&self, _id: &MuxSessionId) -> anyhow::Result<Vec<u32>> {
            let call = self.list_calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                return Ok(self.first_pids.clone());
            }
            Ok(self
                .settled_pids
                .as_ref()
                .unwrap_or(&self.first_pids)
                .clone())
        }

        async fn resize(&self, _id: &MuxSessionId, _cols: u16, _rows: u16) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Empty pane PID lists short-circuit to `false` without ever
    /// shelling out to `ps`. Confirms the empty-pids fast path + that
    /// the helper is wired to the pane trait correctly.
    #[tokio::test]
    async fn empty_pids_returns_false() {
        let backend = TestPaneBackend::fixed(Vec::new());
        let id = MuxSessionId::new("nonexistent-session");
        let runs = pane_runs_process(&backend, &id, "claude")
            .await
            .expect("probe must not error on empty pids");
        assert!(!runs, "no panes ⇒ no matching process");
    }

    /// PID 0 is skipped; with the only entry being 0, the probe never
    /// calls `ps` and returns false.
    #[tokio::test]
    async fn pid_zero_returns_false() {
        let backend = TestPaneBackend::fixed(vec![0]);
        let id = MuxSessionId::new("x");
        assert!(!pane_runs_process(&backend, &id, "definitely-not-a-comm")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn gone_pid_is_not_retried() {
        let backend = TestPaneBackend::fixed(vec![u32::MAX]);
        let id = MuxSessionId::new("gone-session");

        assert!(!pane_runs_process(&backend, &id, "claude").await.unwrap());
        assert_eq!(
            backend.list_calls.load(Ordering::Relaxed),
            1,
            "a definitively gone pid must fail without a settle delay"
        );
    }

    #[tokio::test]
    async fn waits_for_pane_process_to_settle() {
        struct ChildGuard(std::process::Child);

        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep fixture");
        let child = ChildGuard(child);
        let backend = TestPaneBackend::settling(vec![std::process::id()], vec![child.0.id()]);
        let id = MuxSessionId::new("settling-session");

        assert!(
            pane_runs_process(&backend, &id, "sleep").await.unwrap(),
            "a newly published pane must be re-probed while its process settles"
        );
        assert!(
            backend.list_calls.load(Ordering::Relaxed) >= 2,
            "the probe must refresh pane process identity"
        );
    }
}
