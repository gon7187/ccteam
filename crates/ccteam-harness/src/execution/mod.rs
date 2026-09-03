//! Execution support modules shared by concrete harness adapters.
//!
pub mod acp;
pub mod claude_bg;
pub mod claude_common;
pub mod claude_stream_json;
pub mod claude_tui;
pub mod codex_app_server;
pub mod codex_exec;
pub mod codex_jsonrpc;
pub mod codex_typed_events;
pub mod delegation;
pub mod dsh_acp;
pub mod dsh_runtime;
pub mod experience;
pub mod fs_atomic;
pub mod grok_acp;
pub mod host_channel;
pub mod journal;
pub mod kimi_acp;
pub mod mcp_config;
pub mod opencode_acp;
pub mod pi_rpc;
pub mod process_inspect;
pub mod progress_bridge;
pub mod remote_exec;
pub mod satellite_exec;
pub mod session_body;
pub mod session_meta;
pub mod session_recovery;
pub mod session_status;
pub mod transcript_tail;
pub mod turns_mirror;
pub mod typed_events;
pub mod vendor_pids;
pub mod vendor_title;

pub use claude_bg::ClaudeBgAdapter;
pub use claude_stream_json::ClaudeStreamJsonAdapter;
pub use claude_tui::ClaudeTuiAdapter;
pub use codex_app_server::CodexAppServerAdapter;
pub use codex_exec::CodexExecAdapter;
pub use dsh_acp::DshAcpAdapter;
pub use grok_acp::GrokAcpAdapter;
pub use kimi_acp::KimiAcpAdapter;
pub use opencode_acp::OpencodeAcpAdapter;
pub use pi_rpc::PiRpcAdapter;

/// A nonce unique across daemon incarnations AND within one process: hex
/// unix-nanos at call time joined with a process-wide counter.
///
/// Adapters that synthesize turn ids from a per-process counter must bake
/// this in. `turn_id` is the durable dedup key of the terminal boundary
/// (`turns.jsonl` `completed`/`failed` rows, `chat_turn_completed` in
/// progress, delegation `notified_turns`), and that history outlives the
/// child process: a `--resume` after a daemon restart builds a new
/// translator whose bare counter restarts at 1, so every post-resume
/// boundary read as a replay of the pre-resume turn with the same number
/// and was dropped — no `completed` row, no completion notification to
/// the parent, the turn stayed live until the next inbound message.
pub fn incarnation_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{seq}")
}

#[cfg(test)]
mod incarnation_tests {
    #[test]
    fn incarnation_nonce_is_unique_even_within_one_instant() {
        let a = super::incarnation_nonce();
        let b = super::incarnation_nonce();
        assert_ne!(
            a, b,
            "process-wide counter must separate same-instant nonces"
        );
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }
}
