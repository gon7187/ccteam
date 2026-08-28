//! Minimal progress.jsonl helpers used by harness-owned adapters.
//!
//! `ccteam-core` owns the richer query surface, but harness cannot depend
//! on core without reintroducing a cargo cycle. Keep only the small append
//! and row-builder subset needed by execution adapters here.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::fs_atomic::atomic_write_durable;
use super::journal;
use crate::ccteam_root_from_env;

type PersistObserver = dyn Fn(&Path, bool) + Send + Sync + 'static;

static PERSIST_OBSERVER: OnceLock<Box<PersistObserver>> = OnceLock::new();

/// Default active progress journal size before single-level rotation.
pub const DEFAULT_PROGRESS_ROTATE_BYTES: u64 = 64 * 1024 * 1024;
const CHECKPOINT_SCHEMA_VERSION: u32 = 3;
const VERDICT_INDEX_SCHEMA_VERSION: u32 = 2;

pub const CHAT_SESSION_RESET: &str = "chat_session_reset";
pub const CHAT_SESSION_STARTED: &str = "chat_session_started";
pub const CHAT_TURN_USER_PROMPT: &str = "chat_turn_user_prompt";
pub const CHAT_TURN_COMPLETED: &str = "chat_turn_completed";
pub const TURN_VERDICT: &str = "turn_verdict";
pub const CHAT_SESSION_RESET_WITH_RECOVERY: &str = "chat_session_reset_with_recovery";
pub const CHAT_COMPACT_DONE: &str = "chat_compact_done";
pub const CHAT_HOP_ESCALATE: &str = "chat_hop_escalate";
pub const CHAT_TOOL_CALL_STARTED: &str = "chat_tool_call_started";
pub const CHAT_BOT_PERMANENT_FAILURE: &str = "chat_bot_permanent_failure";
pub const CHAT_MARKER_SELF_HEAL_ATTEMPT: &str = "chat_marker_self_heal_attempt";
pub const CHAT_TURN_RUNNING_LONG: &str = "chat_turn_running_long";
pub const CHAT_TURN_TIMEOUT: &str = "chat_turn_timeout";
pub const AGENT_DONE: &str = "agent_done";
/// v0.9.2 — a live session was gracefully stopped to admit another session
/// under the daemon-wide capacity limit.
pub const SESSION_EVICTED: &str = "session_evicted";
/// 2026-08-09 — the pump's inbound attachment to a live session ended (the
/// transport under it was replaced, a shared connection was dropped, a child
/// exited). The session is NOT over; the pump is rebuilding the attachment.
pub const SESSION_STREAM_DETACHED: &str = "session_stream_detached";
/// 2026-08-09 — the pump proved a rebuilt attachment by receiving an event on it.
/// Pairs with [`SESSION_STREAM_DETACHED`]; `gap_ms` is the blind window.
pub const SESSION_STREAM_REATTACHED: &str = "session_stream_reattached";
/// 2026-08-19 (one sid, one body) — the daemon let go of a session's OS body
/// without stopping it, or found it still running after a restart. The
/// session is NOT over and NOT driveable by this daemon until the body exits:
/// `reason` = `daemon_shutdown` (graceful stop left it finishing its turn) |
/// `daemon_restart` (found alive by the next daemon).
pub const SESSION_BODY_DETACHED: &str = "session_body_detached";
/// 2026-08-19 — a detached body ended: `reason` = `exited` (finished on its
/// own; `recovered` says whether its unobserved answer was recovered from the
/// vendor's own record) | `stopped` (a user explicitly stopped it). The
/// session is rebuilt by sid right after.
pub const SESSION_BODY_EXITED: &str = "session_body_exited";
/// v0.8.7 review-fix (R-L1) — a HITL session is PARKED awaiting a human
/// approve/deny on a non-allowlist tool call. Emitted when the permission
/// prompt is outstanding so an operator (status / dashboard / `progress`)
/// sees the agent is blocked, not stuck.
pub const CHAT_PERMISSION_PROMPT_OUTSTANDING: &str = "chat_permission_prompt_outstanding";
// v0.9.0 W2 (F2/F5) — delegation lifecycle events. Schema authority for the
// `delegation_*` family lives HERE (progress_bridge); the gateway/dispatch
// layer only calls [`build_delegation_event`] at the corresponding points.
pub const DELEGATION_SPAWNED: &str = "delegation_spawned";
pub const DELEGATION_DISPATCHED: &str = "delegation_dispatched";
pub const DELEGATION_COMPLETED: &str = "delegation_completed";
pub const DELEGATION_NOTIFIED: &str = "delegation_notified";
pub const DELEGATION_COLLECTED: &str = "delegation_collected";
pub const DELEGATION_STOPPED: &str = "delegation_stopped";
pub const DELEGATION_DENIED: &str = "delegation_denied";
/// One-shot human-message scheduler lifecycle events.
pub const SCHEDULED_ENQUEUED: &str = "scheduled_enqueued";
pub const SCHEDULED_CANCELLED: &str = "scheduled_cancelled";
pub const SCHEDULED_FIRED: &str = "scheduled_fired";
pub const SCHEDULED_FAILED: &str = "scheduled_failed";

pub const CODEX_PLAN_UPDATED: &str = "codex_plan_updated";
pub const CODEX_TOKEN_USAGE: &str = "codex_token_usage";
pub const CODEX_THREAD_STATUS: &str = "codex_thread_status";
pub const CODEX_RATE_LIMIT: &str = "codex_rate_limit";
pub const TYPED_EVENT: &str = "typed_event";
pub const MERGER_LOSSY_PARTIAL: &str = "merger_lossy_partial";

/// Every event kind owned by the canonical progress schema.
///
/// Hook fallback and pre-schema rows remain valid unknown facts; they are not
/// promoted into this enum merely because a legacy producer emitted them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    ChatSessionReset,
    ChatSessionStarted,
    ChatTurnUserPrompt,
    ChatTurnCompleted,
    TurnVerdict,
    ChatSessionResetWithRecovery,
    ChatCompactDone,
    ChatHopEscalate,
    ChatToolCallStarted,
    ChatBotPermanentFailure,
    ChatMarkerSelfHealAttempt,
    ChatTurnRunningLong,
    ChatTurnTimeout,
    AgentDone,
    SessionEvicted,
    SessionStreamDetached,
    SessionStreamReattached,
    SessionBodyDetached,
    SessionBodyExited,
    ChatPermissionPromptOutstanding,
    DelegationSpawned,
    DelegationDispatched,
    DelegationCompleted,
    DelegationNotified,
    DelegationCollected,
    DelegationStopped,
    DelegationDenied,
    ScheduledEnqueued,
    ScheduledCancelled,
    ScheduledFired,
    ScheduledFailed,
    CodexPlanUpdated,
    CodexTokenUsage,
    CodexThreadStatus,
    CodexRateLimit,
    TypedEvent,
    MergerLossyPartial,
}

impl EventKind {
    pub const ALL: &'static [EventKind] = &[
        EventKind::ChatSessionReset,
        EventKind::ChatSessionStarted,
        EventKind::ChatTurnUserPrompt,
        EventKind::ChatTurnCompleted,
        EventKind::TurnVerdict,
        EventKind::ChatSessionResetWithRecovery,
        EventKind::ChatCompactDone,
        EventKind::ChatHopEscalate,
        EventKind::ChatToolCallStarted,
        EventKind::ChatBotPermanentFailure,
        EventKind::ChatMarkerSelfHealAttempt,
        EventKind::ChatTurnRunningLong,
        EventKind::ChatTurnTimeout,
        EventKind::AgentDone,
        EventKind::SessionEvicted,
        EventKind::SessionStreamDetached,
        EventKind::SessionStreamReattached,
        EventKind::SessionBodyDetached,
        EventKind::SessionBodyExited,
        EventKind::ChatPermissionPromptOutstanding,
        EventKind::DelegationSpawned,
        EventKind::DelegationDispatched,
        EventKind::DelegationCompleted,
        EventKind::DelegationNotified,
        EventKind::DelegationCollected,
        EventKind::DelegationStopped,
        EventKind::DelegationDenied,
        EventKind::ScheduledEnqueued,
        EventKind::ScheduledCancelled,
        EventKind::ScheduledFired,
        EventKind::ScheduledFailed,
        EventKind::CodexPlanUpdated,
        EventKind::CodexTokenUsage,
        EventKind::CodexThreadStatus,
        EventKind::CodexRateLimit,
        EventKind::TypedEvent,
        EventKind::MergerLossyPartial,
    ];

    pub const fn wire_name(self) -> &'static str {
        match self {
            EventKind::ChatSessionReset => CHAT_SESSION_RESET,
            EventKind::ChatSessionStarted => CHAT_SESSION_STARTED,
            EventKind::ChatTurnUserPrompt => CHAT_TURN_USER_PROMPT,
            EventKind::ChatTurnCompleted => CHAT_TURN_COMPLETED,
            EventKind::TurnVerdict => TURN_VERDICT,
            EventKind::ChatSessionResetWithRecovery => CHAT_SESSION_RESET_WITH_RECOVERY,
            EventKind::ChatCompactDone => CHAT_COMPACT_DONE,
            EventKind::ChatHopEscalate => CHAT_HOP_ESCALATE,
            EventKind::ChatToolCallStarted => CHAT_TOOL_CALL_STARTED,
            EventKind::ChatBotPermanentFailure => CHAT_BOT_PERMANENT_FAILURE,
            EventKind::ChatMarkerSelfHealAttempt => CHAT_MARKER_SELF_HEAL_ATTEMPT,
            EventKind::ChatTurnRunningLong => CHAT_TURN_RUNNING_LONG,
            EventKind::ChatTurnTimeout => CHAT_TURN_TIMEOUT,
            EventKind::AgentDone => AGENT_DONE,
            EventKind::SessionEvicted => SESSION_EVICTED,
            EventKind::SessionStreamDetached => SESSION_STREAM_DETACHED,
            EventKind::SessionStreamReattached => SESSION_STREAM_REATTACHED,
            EventKind::SessionBodyDetached => SESSION_BODY_DETACHED,
            EventKind::SessionBodyExited => SESSION_BODY_EXITED,
            EventKind::ChatPermissionPromptOutstanding => CHAT_PERMISSION_PROMPT_OUTSTANDING,
            EventKind::DelegationSpawned => DELEGATION_SPAWNED,
            EventKind::DelegationDispatched => DELEGATION_DISPATCHED,
            EventKind::DelegationCompleted => DELEGATION_COMPLETED,
            EventKind::DelegationNotified => DELEGATION_NOTIFIED,
            EventKind::DelegationCollected => DELEGATION_COLLECTED,
            EventKind::DelegationStopped => DELEGATION_STOPPED,
            EventKind::DelegationDenied => DELEGATION_DENIED,
            EventKind::ScheduledEnqueued => SCHEDULED_ENQUEUED,
            EventKind::ScheduledCancelled => SCHEDULED_CANCELLED,
            EventKind::ScheduledFired => SCHEDULED_FIRED,
            EventKind::ScheduledFailed => SCHEDULED_FAILED,
            EventKind::CodexPlanUpdated => CODEX_PLAN_UPDATED,
            EventKind::CodexTokenUsage => CODEX_TOKEN_USAGE,
            EventKind::CodexThreadStatus => CODEX_THREAD_STATUS,
            EventKind::CodexRateLimit => CODEX_RATE_LIMIT,
            EventKind::TypedEvent => TYPED_EVENT,
            EventKind::MergerLossyPartial => MERGER_LOSSY_PARTIAL,
        }
    }

    pub fn from_wire_name(value: &str) -> Option<Self> {
        Some(match value {
            CHAT_SESSION_RESET => EventKind::ChatSessionReset,
            CHAT_SESSION_STARTED => EventKind::ChatSessionStarted,
            CHAT_TURN_USER_PROMPT => EventKind::ChatTurnUserPrompt,
            CHAT_TURN_COMPLETED => EventKind::ChatTurnCompleted,
            TURN_VERDICT => EventKind::TurnVerdict,
            CHAT_SESSION_RESET_WITH_RECOVERY => EventKind::ChatSessionResetWithRecovery,
            CHAT_COMPACT_DONE => EventKind::ChatCompactDone,
            CHAT_HOP_ESCALATE => EventKind::ChatHopEscalate,
            CHAT_TOOL_CALL_STARTED => EventKind::ChatToolCallStarted,
            CHAT_BOT_PERMANENT_FAILURE => EventKind::ChatBotPermanentFailure,
            CHAT_MARKER_SELF_HEAL_ATTEMPT => EventKind::ChatMarkerSelfHealAttempt,
            CHAT_TURN_RUNNING_LONG => EventKind::ChatTurnRunningLong,
            CHAT_TURN_TIMEOUT => EventKind::ChatTurnTimeout,
            AGENT_DONE => EventKind::AgentDone,
            SESSION_EVICTED => EventKind::SessionEvicted,
            SESSION_STREAM_DETACHED => EventKind::SessionStreamDetached,
            SESSION_STREAM_REATTACHED => EventKind::SessionStreamReattached,
            SESSION_BODY_DETACHED => EventKind::SessionBodyDetached,
            SESSION_BODY_EXITED => EventKind::SessionBodyExited,
            CHAT_PERMISSION_PROMPT_OUTSTANDING => EventKind::ChatPermissionPromptOutstanding,
            DELEGATION_SPAWNED => EventKind::DelegationSpawned,
            DELEGATION_DISPATCHED => EventKind::DelegationDispatched,
            DELEGATION_COMPLETED => EventKind::DelegationCompleted,
            DELEGATION_NOTIFIED => EventKind::DelegationNotified,
            DELEGATION_COLLECTED => EventKind::DelegationCollected,
            DELEGATION_STOPPED => EventKind::DelegationStopped,
            DELEGATION_DENIED => EventKind::DelegationDenied,
            SCHEDULED_ENQUEUED => EventKind::ScheduledEnqueued,
            SCHEDULED_CANCELLED => EventKind::ScheduledCancelled,
            SCHEDULED_FIRED => EventKind::ScheduledFired,
            SCHEDULED_FAILED => EventKind::ScheduledFailed,
            CODEX_PLAN_UPDATED => EventKind::CodexPlanUpdated,
            CODEX_TOKEN_USAGE => EventKind::CodexTokenUsage,
            CODEX_THREAD_STATUS => EventKind::CodexThreadStatus,
            CODEX_RATE_LIMIT => EventKind::CodexRateLimit,
            TYPED_EVENT => EventKind::TypedEvent,
            MERGER_LOSSY_PARTIAL => EventKind::MergerLossyPartial,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventScope {
    Project,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventClass {
    Fact,
    LatestState {
        min_interval: Duration,
        scope: EventScope,
    },
    Telemetry,
}

/// Human verdict persisted as the canonical `turn_verdict` progress fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnVerdict {
    pub sid: String,
    pub turn_id: String,
    pub ts: DateTime<Utc>,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}

/// Accept the completed turn or request a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accept,
    Revise,
}

/// Lightweight terminal-turn signals captured at the canonical boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSignals {
    /// Delta of the session activity counter across the turn. This is an
    /// approximation, not a precise tool-call count.
    pub tool_calls: u64,
    /// Whether a user message steered a turn already in flight.
    pub steered: bool,
    /// Reserved for a future error-recovery detector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_recovered: Option<bool>,
}

/// Optional facts captured at the exact completion boundary.
///
/// Missing fields stay unknown; callers must not backfill them from current
/// session state when projecting historical turns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurnCompletionMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_sha: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<TurnSignals>,
}

/// Classify every schema-owned kind. Deliberately no wildcard: a new
/// [`EventKind`] cannot compile until its persistence policy is chosen.
pub const fn class(kind: EventKind) -> EventClass {
    match kind {
        EventKind::ChatSessionReset
        | EventKind::ChatSessionStarted
        | EventKind::ChatTurnUserPrompt
        | EventKind::ChatTurnCompleted
        | EventKind::TurnVerdict
        | EventKind::ChatSessionResetWithRecovery
        | EventKind::ChatCompactDone
        | EventKind::ChatHopEscalate
        | EventKind::ChatToolCallStarted
        | EventKind::ChatBotPermanentFailure
        | EventKind::ChatMarkerSelfHealAttempt
        | EventKind::ChatTurnTimeout
        | EventKind::AgentDone
        | EventKind::SessionEvicted
        | EventKind::SessionStreamDetached
        | EventKind::SessionStreamReattached
        | EventKind::SessionBodyDetached
        | EventKind::SessionBodyExited
        | EventKind::ChatPermissionPromptOutstanding
        | EventKind::DelegationSpawned
        | EventKind::DelegationDispatched
        | EventKind::DelegationCompleted
        | EventKind::DelegationNotified
        | EventKind::DelegationCollected
        | EventKind::DelegationStopped
        | EventKind::DelegationDenied
        | EventKind::ScheduledEnqueued
        | EventKind::ScheduledCancelled
        | EventKind::ScheduledFired
        | EventKind::ScheduledFailed
        | EventKind::CodexPlanUpdated
        | EventKind::TypedEvent
        | EventKind::MergerLossyPartial => EventClass::Fact,
        EventKind::CodexTokenUsage | EventKind::CodexThreadStatus | EventKind::CodexRateLimit => {
            EventClass::LatestState {
                min_interval: Duration::from_secs(30),
                scope: EventScope::Project,
            }
        }
        EventKind::ChatTurnRunningLong => EventClass::LatestState {
            min_interval: Duration::from_secs(5 * 60),
            scope: EventScope::Session,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindStat {
    pub kind: String,
    pub unknown: bool,
    pub appended_count: u64,
    pub appended_bytes: u64,
    pub suppressed_count: u64,
    pub suppressed_bytes: u64,
}

/// Stable identity for the single retained `<slug>.1.jsonl` archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveCoverage {
    /// Exact archive byte length.
    pub byte_size: u64,
    /// Legacy v1/v2 marker. Retained only so an existing covered archive can
    /// be upgraded without folding its aggregates twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_line_sha256: Option<String>,
    /// SHA-256 of the complete immutable archive generation (v3+ identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_file_sha256: Option<String>,
}

/// Cumulative lifetime aggregates for data no longer present in the active
/// progress journal.
///
/// Per-sid cost totals are retained because they are effectively free while
/// streaming the archive and make the checkpoint useful to future per-session
/// consumers. Rolling 24-hour minute buckets deliberately remain active-file
/// only; after rotation they can undercount pre-rotation minutes, while every
/// lifetime field here remains exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressCheckpoint {
    /// Checkpoint wire schema.
    pub schema_version: u32,
    /// Monotonic count of archives folded into this checkpoint.
    pub rotation_sequence: u64,
    /// Parseable events folded across all rotated-away generations.
    pub event_count: u64,
    /// Corrupt byte-lines observed across rotated-away generations.
    pub corrupt_line_count: u64,
    /// Lifetime `agent_done.cost_usd` total.
    pub cost_total_usd: f64,
    /// Lifetime cost grouped by event vendor.
    pub cost_total_by_vendor: BTreeMap<String, f64>,
    /// Lifetime cost grouped by `sid` (falling back to legacy `session_id`).
    pub cost_total_by_sid: BTreeMap<String, f64>,
    /// Latest canonical human verdict for each completed `(sid, turn_id)` that
    /// has already rotated out of the retained journals. Nested string maps
    /// keep the checkpoint JSON deterministic and object-key compatible.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub turn_verdicts: BTreeMap<String, BTreeMap<String, TurnVerdict>>,
    /// Latest terminal completion projection for every `(sid, turn_id)` that
    /// has rotated away. Experience rebuild needs the historical model,
    /// duration, role/skill hashes, usage, cost inputs, and outcome.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub terminal_turns: BTreeMap<String, BTreeMap<String, Value>>,
    /// The current `.1` archive already included in these cumulative totals.
    pub coverage: Option<ArchiveCoverage>,
}

impl Default for ProgressCheckpoint {
    fn default() -> Self {
        Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            rotation_sequence: 0,
            event_count: 0,
            corrupt_line_count: 0,
            cost_total_usd: 0.0,
            cost_total_by_vendor: BTreeMap::new(),
            cost_total_by_sid: BTreeMap::new(),
            turn_verdicts: BTreeMap::new(),
            terminal_turns: BTreeMap::new(),
            coverage: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PendingProgressIndexWrite {
    Verdict {
        verdict: TurnVerdict,
        active_offset: u64,
        line_len: u64,
        line_sha256: String,
    },
    TerminalTurn {
        event: Value,
        active_offset: u64,
        line_len: u64,
        line_sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_file_identity: Option<PendingActiveFileIdentity>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct PendingActiveFileIdentity {
    device: u64,
    inode: u64,
}

/// Small durable projection used by verdict GET/PUT. `progress.jsonl` remains
/// authoritative; `pending` makes the two-file update crash-recoverable by
/// verifying one exact bounded line at its recorded active-file offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ProgressVerdictIndex {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    verdicts: BTreeMap<String, BTreeMap<String, TurnVerdict>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    terminal_turns: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingProgressIndexWrite>,
}

impl Default for ProgressVerdictIndex {
    fn default() -> Self {
        Self {
            schema_version: VERDICT_INDEX_SCHEMA_VERSION,
            verdicts: BTreeMap::new(),
            terminal_turns: BTreeMap::new(),
            pending: None,
        }
    }
}

/// Result of atomically admitting one canonical terminal turn fact.
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalTerminalAppend {
    /// `true` only when this call durably appended the first fact.
    pub appended: bool,
    /// The first canonical fact, whether written now or recovered earlier.
    pub event: Value,
}

/// Canonical verdict projection with an explicit progress data-quality signal.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnVerdictRead {
    pub verdicts: BTreeMap<(String, String), TurnVerdict>,
    pub corrupt_line_count: u64,
}

/// Shared cost fields extracted from one canonical progress event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressCostContribution<'a> {
    /// Numeric `cost_usd` on an `agent_done` row (missing means zero).
    pub cost_usd: f64,
    /// Whether the USD value came from a reported or verified price.
    pub is_priced: bool,
    /// Optional vendor label from the same row.
    pub vendor: Option<&'a str>,
    /// Optional canonical or legacy session id from the same row.
    pub sid: Option<&'a str>,
}

/// Result of repairing one corrupt active or archive journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressRepairReport {
    /// Parseable records preserved.
    pub kept_count: u64,
    /// Corrupt byte-lines removed.
    pub dropped_count: u64,
    /// Durable backup containing the exact original bytes.
    pub backup_path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default)]
struct KindCounters {
    unknown: bool,
    appended_count: u64,
    appended_bytes: u64,
    suppressed_count: u64,
    suppressed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AdmissionKey {
    path: PathBuf,
    kind: EventKind,
    scope: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct PersistedState {
    hash: [u8; 32],
    at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct PendingState {
    hash: [u8; 32],
    reservation: u64,
}

#[derive(Debug, Default)]
struct LatestStateEntry {
    persisted: Option<PersistedState>,
    pending: Option<PendingState>,
}

#[derive(Debug, Default)]
struct AdmissionState {
    canonical_paths: HashMap<PathBuf, PathBuf>,
    latest: HashMap<AdmissionKey, LatestStateEntry>,
    stats: HashMap<String, KindCounters>,
    next_reservation: u64,
}

static ADMISSION_STATE: OnceLock<Mutex<AdmissionState>> = OnceLock::new();

fn admission_state() -> MutexGuard<'static, AdmissionState> {
    ADMISSION_STATE
        .get_or_init(|| Mutex::new(AdmissionState::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Snapshot process-global admission counters, sorted by kind for stable
/// doctor/metrics output.
pub fn kind_stats() -> Vec<KindStat> {
    let state = admission_state();
    let mut stats = state
        .stats
        .iter()
        .map(|(kind, counters)| KindStat {
            kind: kind.clone(),
            unknown: counters.unknown,
            appended_count: counters.appended_count,
            appended_bytes: counters.appended_bytes,
            suppressed_count: counters.suppressed_count,
            suppressed_bytes: counters.suppressed_bytes,
        })
        .collect::<Vec<_>>();
    stats.sort_unstable_by(|left, right| left.kind.cmp(&right.kind));
    stats
}

/// Install the process-wide callback invoked after a progress row is durably
/// appended. The boolean marks an append that also rotated the active file.
/// Returns `false` when another daemon component already installed the
/// callback. One-shot CLI processes never call this function.
pub fn set_persist_observer(observer: Box<PersistObserver>) -> bool {
    PERSIST_OBSERVER.set(observer).is_ok()
}

pub fn hooks_script_from_env() -> Option<PathBuf> {
    ccteam_root_from_env().map(|root| root.join("hooks").join("hook.sh"))
}

pub fn progress_jsonl_from_env(slug: &str) -> Option<PathBuf> {
    ccteam_root_from_env().map(|root| {
        root.join("state")
            .join("progress")
            .join(format!("{slug}.jsonl"))
    })
}

/// Resolve the rotation threshold.
///
/// `CCTEAM_PROGRESS_ROTATE_BYTES` is an operational/test override. Unset,
/// non-numeric, and zero values all fall back to the 64 MiB default so a bad
/// environment cannot accidentally turn every append into a rotation.
pub fn progress_rotate_bytes() -> u64 {
    std::env::var("CCTEAM_PROGRESS_ROTATE_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PROGRESS_ROTATE_BYTES)
}

/// Derive the single retained archive path for an active progress journal.
pub fn progress_archive_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".1.jsonl")
}

/// Derive the lifetime checkpoint path for an active progress journal.
pub fn progress_checkpoint_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".checkpoint.json")
}

/// Resolve the compact durable verdict projection for one progress journal.
pub fn progress_verdict_index_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".verdicts.json")
}

/// Read a progress checkpoint without mutating or recovering it.
pub fn read_progress_checkpoint(active_path: &Path) -> Result<Option<ProgressCheckpoint>> {
    let path = progress_checkpoint_path(active_path);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let checkpoint = serde_json::from_slice::<ProgressCheckpoint>(&bytes)
        .with_context(|| format!("parse {}", path.display()))?;
    if !matches!(checkpoint.schema_version, 1 | 2 | CHECKPOINT_SCHEMA_VERSION) {
        anyhow::bail!(
            "unsupported progress checkpoint schema {} in {}",
            checkpoint.schema_version,
            path.display()
        );
    }
    Ok(Some(checkpoint))
}

/// Compute the coverage marker for an archive, or `None` when it is absent.
pub fn progress_archive_coverage(active_path: &Path) -> Result<Option<ArchiveCoverage>> {
    archive_coverage_for_path(&progress_archive_path(active_path))
}

/// Return whether a parsed checkpoint covers the current `.1` archive.
pub fn checkpoint_covers_archive(
    checkpoint: &ProgressCheckpoint,
    archive: Option<&ArchiveCoverage>,
) -> bool {
    match (checkpoint.coverage.as_ref(), archive) {
        (None, None) => true,
        (Some(checkpoint), Some(archive)) => {
            checkpoint.byte_size == archive.byte_size
                && checkpoint.full_file_sha256.is_some()
                && checkpoint.full_file_sha256 == archive.full_file_sha256
        }
        _ => false,
    }
}

/// Load the lifetime checkpoint and close the crash window where active was
/// renamed to `.1` but its aggregates were not checkpointed yet.
///
/// Recovery uses the same stable flock as append/rotation, streams `.1` once,
/// and atomically replaces the checkpoint. A covered archive is never scanned.
pub fn load_or_recover_progress_checkpoint(
    active_path: &Path,
) -> Result<Option<ProgressCheckpoint>> {
    // Read-only callers (projection catch-up runs this for EVERY slug) must
    // not materialize the lock/dir for a project that has no progress state
    // at all — otherwise a mere query mints `.lock` droppings.
    if !active_path.exists()
        && !progress_archive_path(active_path).exists()
        && !progress_checkpoint_path(active_path).exists()
        && !progress_verdict_index_path(active_path).exists()
    {
        return Ok(None);
    }
    if let Some(parent) = active_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = open_progress_lock(active_path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    let checkpoint = recover_progress_checkpoint_locked(active_path)?;
    ensure_verdict_index_locked(active_path, checkpoint.as_ref())?;
    Ok(checkpoint)
}

/// Extract the one lifetime cost formula shared by projection and checkpoint
/// folding. Terminal legacy rows carry their resolved USD amount; paneless
/// `chat_turn_completed` rows carry usage and are priced here instead.
pub fn progress_cost_contribution(event: &Value) -> Option<ProgressCostContribution<'_>> {
    let vendor = event.get("vendor").and_then(Value::as_str);
    let sid = event
        .get("sid")
        .and_then(Value::as_str)
        .or_else(|| event.get("session_id").and_then(Value::as_str));
    match event_kind_name(event) {
        Some(AGENT_DONE) => Some(ProgressCostContribution {
            cost_usd: event.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
            is_priced: event.get("cost_usd").and_then(Value::as_f64).is_some(),
            vendor,
            sid,
        }),
        Some(CHAT_TURN_COMPLETED) => {
            let vendor = vendor?;
            // Invariant: codex is today the only adapter emitting priced
            // `agent_done`; a new vendor bridging `agent_done` must also be
            // excluded from pricing `chat_turn_completed`, or its cost
            // double-counts. Codex's paneless chat mirror is telemetry only.
            if vendor == "codex" {
                return None;
            }
            let cost_vendor = match vendor {
                "claude" => ccteam_cost::Vendor::Claude,
                "codex" => ccteam_cost::Vendor::Codex,
                "grok" => ccteam_cost::Vendor::Grok,
                "opencode" => ccteam_cost::Vendor::Opencode,
                "kimi" => ccteam_cost::Vendor::Kimi,
                "pi" => ccteam_cost::Vendor::Pi,
                "dsh" => ccteam_cost::Vendor::Dsh,
                _ => return None,
            };
            let usage = serde_json::from_value::<ccteam_cost::UnifiedTokenUsage>(
                event.get("usage")?.clone(),
            )
            .ok()?;
            let model = event.get("model").and_then(Value::as_str).unwrap_or("");
            let cost = ccteam_cost::resolve_turn_cost(&usage, cost_vendor, model)?;
            Some(ProgressCostContribution {
                cost_usd: cost,
                is_priced: true,
                vendor: Some(vendor),
                sid,
            })
        }
        _ => None,
    }
}

pub fn append_event(path: &Path, event: &Value) -> Result<()> {
    if event_kind_name(event) == Some(TURN_VERDICT) {
        let verdict =
            parse_turn_verdict_event(event).context("malformed canonical turn_verdict event")?;
        append_turn_verdict_if_changed(path, &verdict)?;
        return Ok(());
    }
    if event_kind_name(event) == Some(CHAT_TURN_COMPLETED) {
        append_chat_turn_completed_if_absent(path, event)?;
        return Ok(());
    }
    append_event_at(path, event, Instant::now(), None)
}

/// Durably append the first canonical terminal fact for `(sid, turn_id)`.
///
/// `progress.jsonl` remains authoritative. The compact projection only makes
/// the identity check bounded and crash-recoverable across daemon restarts.
/// A later replay returns the original fact and never overwrites its receipt
/// timestamp or payload.
pub fn append_chat_turn_completed_if_absent(
    path: &Path,
    event: &Value,
) -> Result<CanonicalTerminalAppend> {
    let (sid, turn_id) =
        terminal_turn_identity(event).context("malformed canonical chat_turn_completed event")?;
    let sid = sid.to_string();
    let turn_id = turn_id.to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut line = serde_json::to_vec(event).context("serialize chat_turn_completed event")?;
    line.push(b'\n');
    let byte_count = u64::try_from(line.len()).unwrap_or(u64::MAX);

    let lock_file = open_progress_lock(path)?;
    let lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;
    settle_pending_progress_write_locked(path)?;
    if read_verdict_index_locked(path)?.is_none() {
        if path.exists()
            || progress_archive_path(path).exists()
            || progress_checkpoint_path(path).exists()
        {
            let checkpoint = recover_progress_checkpoint_locked(path)?;
            ensure_verdict_index_locked(path, checkpoint.as_ref())?;
        } else {
            write_verdict_index(path, &ProgressVerdictIndex::default())?;
        }
    }

    let mut index = read_verdict_index_locked(path)?.ok_or_else(|| {
        anyhow::anyhow!("progress verdict index disappeared for {}", path.display())
    })?;
    if let Some(first) = index
        .terminal_turns
        .get(&sid)
        .and_then(|turns| turns.get(&turn_id))
        .cloned()
    {
        drop(lock);
        record_suppressed(CHAT_TURN_COMPLETED, false, byte_count);
        return Ok(CanonicalTerminalAppend {
            appended: false,
            event: first,
        });
    }

    let current_size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let mut rotated = false;
    if current_size > 0 && current_size.saturating_add(byte_count) > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
        rotated = true;
        index = read_verdict_index_locked(path)?.ok_or_else(|| {
            anyhow::anyhow!("progress verdict index disappeared for {}", path.display())
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let active_metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let active_offset = active_metadata.len();
    index.pending = Some(PendingProgressIndexWrite::TerminalTurn {
        event: event.clone(),
        active_offset,
        line_len: byte_count,
        line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
        active_file_identity: pending_active_file_identity(&active_metadata),
    });
    write_verdict_index(path, &index)?;

    file.write_all(&line)
        .with_context(|| format!("write terminal turn event to {}", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    file.sync_data()
        .with_context(|| format!("sync terminal turn event to {}", path.display()))?;
    drop(file);

    index
        .terminal_turns
        .entry(sid)
        .or_default()
        .insert(turn_id, event.clone());
    index.pending = None;
    write_verdict_index(path, &index)?;
    if size > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
        rotated = true;
    }
    drop(lock);
    record_appended(CHAT_TURN_COMPLETED, false, byte_count);
    if let Some(observer) = PERSIST_OBSERVER.get() {
        observer(path, rotated);
    }
    Ok(CanonicalTerminalAppend {
        appended: true,
        event: event.clone(),
    })
}

fn terminal_turn_identity(event: &Value) -> Option<(&str, &str)> {
    if event_kind_name(event) != Some(CHAT_TURN_COMPLETED) {
        return None;
    }
    let sid = event.get("sid")?.as_str()?.trim();
    let turn_id = event.get("turn_id")?.as_str()?.trim();
    (!sid.is_empty() && !turn_id.is_empty()).then_some((sid, turn_id))
}

/// Parse one canonical `turn_verdict` event. Other or malformed events are
/// ignored so callers can scan mixed and torn progress journals safely.
pub fn parse_turn_verdict_event(event: &Value) -> Option<TurnVerdict> {
    if event_kind_name(event) != Some(TURN_VERDICT) {
        return None;
    }
    let verdict = serde_json::from_value::<TurnVerdict>(event.clone()).ok()?;
    if verdict.sid.trim().is_empty() || verdict.turn_id.trim().is_empty() {
        return None;
    }
    Some(verdict)
}

/// Read the latest verdict for every `(sid, turn_id)` across the retained
/// archive followed by the active progress journal.
pub fn latest_turn_verdicts(path: &Path) -> Result<BTreeMap<(String, String), TurnVerdict>> {
    Ok(latest_turn_verdicts_detailed(path)?.verdicts)
}

/// Read canonical verdicts and corruption quality from one locked progress
/// snapshot, so analytics cannot race a concurrent append between checks.
pub fn latest_turn_verdicts_detailed(path: &Path) -> Result<TurnVerdictRead> {
    if !path.exists()
        && !progress_archive_path(path).exists()
        && !progress_checkpoint_path(path).exists()
        && !progress_verdict_index_path(path).exists()
    {
        return Ok(TurnVerdictRead {
            verdicts: BTreeMap::new(),
            corrupt_line_count: 0,
        });
    }
    let lock_file = open_progress_lock(path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;
    let checkpoint = recover_progress_checkpoint_locked(path)?;
    if read_verdict_index_locked(path)?.is_none() {
        ensure_verdict_index_locked(path, checkpoint.as_ref())?;
    }
    let active = super::fs_atomic::read_jsonl_detailed::<Value>(path)?;
    Ok(TurnVerdictRead {
        verdicts: latest_turn_verdicts_locked(path)?,
        corrupt_line_count: checkpoint
            .map(|checkpoint| checkpoint.corrupt_line_count)
            .unwrap_or(0)
            .saturating_add(active.corrupt_line_count),
    })
}

/// Count known corrupt canonical progress rows across checkpointed history and
/// the active journal. Callers that present analytics use this to fail closed
/// instead of silently aggregating a partial history.
pub fn progress_corrupt_line_count(path: &Path) -> Result<u64> {
    Ok(latest_turn_verdicts_detailed(path)?.corrupt_line_count)
}

/// Append a canonical verdict only when its semantic payload changed.
///
/// The stable journal lock covers both the read and append, so concurrent
/// identical updates produce one row. `ts` is intentionally excluded from
/// equality: retrying the same PUT later is still idempotent.
pub fn append_turn_verdict_if_changed(path: &Path, verdict: &TurnVerdict) -> Result<bool> {
    if verdict.sid.trim().is_empty() || verdict.turn_id.trim().is_empty() {
        anyhow::bail!("turn verdict requires non-empty sid and turn_id");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut event = serde_json::to_value(verdict).context("serialize turn verdict")?;
    event
        .as_object_mut()
        .expect("TurnVerdict serializes as an object")
        .insert("event".into(), Value::String(TURN_VERDICT.into()));
    let mut line = serde_json::to_vec(&event).context("serialize turn verdict event")?;
    line.push(b'\n');
    let byte_count = u64::try_from(line.len()).unwrap_or(u64::MAX);

    let lock_file = open_progress_lock(path)?;
    let lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;
    settle_pending_progress_write_locked(path)?;
    if read_verdict_index_locked(path)?.is_none() {
        if path.exists()
            || progress_archive_path(path).exists()
            || progress_checkpoint_path(path).exists()
        {
            let checkpoint = recover_progress_checkpoint_locked(path)?;
            ensure_verdict_index_locked(path, checkpoint.as_ref())?;
        } else {
            write_verdict_index(path, &ProgressVerdictIndex::default())?;
        }
    }
    let key = (verdict.sid.clone(), verdict.turn_id.clone());
    if latest_turn_verdicts_locked(path)?
        .get(&key)
        .is_some_and(|latest| verdict_content_eq(latest, verdict))
    {
        drop(lock);
        record_suppressed(TURN_VERDICT, false, byte_count);
        return Ok(false);
    }

    let current_size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if current_size > 0 && current_size.saturating_add(byte_count) > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
    }
    let active_offset = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let mut index = read_verdict_index_locked(path)?.ok_or_else(|| {
        anyhow::anyhow!("progress verdict index disappeared for {}", path.display())
    })?;
    index.pending = Some(PendingProgressIndexWrite::Verdict {
        verdict: verdict.clone(),
        active_offset,
        line_len: byte_count,
        line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
    });
    write_verdict_index(path, &index)?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(&line)
        .with_context(|| format!("write verdict event to {}", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    file.sync_data()
        .with_context(|| format!("sync verdict event to {}", path.display()))?;
    drop(file);

    index
        .verdicts
        .entry(verdict.sid.clone())
        .or_default()
        .insert(verdict.turn_id.clone(), verdict.clone());
    index.pending = None;
    write_verdict_index(path, &index)?;
    let rotated = if size > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
        true
    } else {
        current_size > 0 && current_size.saturating_add(byte_count) > progress_rotate_bytes()
    };
    drop(lock);
    record_appended(TURN_VERDICT, false, byte_count);
    if let Some(observer) = PERSIST_OBSERVER.get() {
        observer(path, rotated);
    }
    Ok(true)
}

fn latest_turn_verdicts_locked(path: &Path) -> Result<BTreeMap<(String, String), TurnVerdict>> {
    let mut latest = BTreeMap::new();
    let Some(index) = read_verdict_index_locked(path)? else {
        anyhow::bail!(
            "progress verdict index missing for {}; run progress recovery before serving verdict reads",
            path.display()
        );
    };
    for (sid, turns) in index.verdicts {
        for (turn_id, verdict) in turns {
            latest.insert((sid.clone(), turn_id), verdict);
        }
    }
    Ok(latest)
}

fn read_verdict_index_locked(path: &Path) -> Result<Option<ProgressVerdictIndex>> {
    let index_path = progress_verdict_index_path(path);
    let bytes = match std::fs::read(&index_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", index_path.display())),
    };
    let mut index = match serde_json::from_slice::<ProgressVerdictIndex>(&bytes) {
        Ok(index) => index,
        // This file is only a compact projection. A torn or otherwise invalid
        // copy must never take the authoritative progress journal (or web
        // startup) down; callers holding the journal lock rebuild it below.
        Err(_) => return Ok(None),
    };
    if index.schema_version < VERDICT_INDEX_SCHEMA_VERSION {
        return Ok(None);
    }
    if index.schema_version > VERDICT_INDEX_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported progress verdict index schema {} in {}",
            index.schema_version,
            index_path.display()
        );
    }

    let Some(pending) = index.pending.take() else {
        return Ok(Some(index));
    };
    match pending {
        PendingProgressIndexWrite::Verdict {
            verdict,
            active_offset,
            line_len,
            line_sha256,
        } => {
            let line_len = usize::try_from(line_len).context("verdict pending line too large")?;
            let mut raw = vec![0_u8; line_len];
            let committed = File::open(path)
                .and_then(|mut file| {
                    file.seek(SeekFrom::Start(active_offset))?;
                    file.read_exact(&mut raw)
                })
                .is_ok()
                && hex_digest(Sha256::digest(&raw).as_slice()) == line_sha256
                && serde_json::from_slice::<Value>(trim_ascii_line(&raw))
                    .ok()
                    .and_then(|event| parse_turn_verdict_event(&event))
                    .is_some_and(|candidate| candidate == verdict);
            if committed {
                index
                    .verdicts
                    .entry(verdict.sid.clone())
                    .or_default()
                    .insert(verdict.turn_id.clone(), verdict);
            }
        }
        PendingProgressIndexWrite::TerminalTurn {
            event,
            active_offset,
            line_len,
            line_sha256,
            active_file_identity,
        } => {
            let (sid, turn_id) = terminal_turn_identity(&event)
                .context("malformed terminal turn in pending progress index")?;
            let sid = sid.to_string();
            let turn_id = turn_id.to_string();
            match recover_pending_terminal_append(
                path,
                &event,
                active_offset,
                line_len,
                &line_sha256,
                active_file_identity.as_ref(),
            )? {
                PendingTerminalAppendState::Committed => {
                    index
                        .terminal_turns
                        .entry(sid)
                        .or_default()
                        .entry(turn_id)
                        .or_insert(event);
                }
                PendingTerminalAppendState::Absent => {}
            }
        }
    }
    write_verdict_index(path, &index)?;
    Ok(Some(index))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingTerminalAppendState {
    Committed,
    Absent,
}

fn recover_pending_terminal_append(
    path: &Path,
    event: &Value,
    active_offset: u64,
    line_len: u64,
    line_sha256: &str,
    expected_identity: Option<&PendingActiveFileIdentity>,
) -> Result<PendingTerminalAppendState> {
    let expected_len = usize::try_from(line_len).context("terminal pending line too large")?;
    let mut expected =
        serde_json::to_vec(event).context("serialize terminal turn from pending progress index")?;
    expected.push(b'\n');
    if expected.len() != expected_len
        || hex_digest(Sha256::digest(&expected).as_slice()) != line_sha256
    {
        anyhow::bail!("ambiguous pending terminal append: index line identity mismatch");
    }

    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && active_offset == 0
                && expected_identity.is_none() =>
        {
            return Ok(PendingTerminalAppendState::Absent);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!(
                "ambiguous pending terminal append: active file is absent before offset {active_offset}"
            );
        }
        Err(error) => {
            return Err(error).with_context(|| format!("open {}", path.display()));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let active_len = metadata.len();
    if active_len < active_offset {
        anyhow::bail!(
            "ambiguous pending terminal append: active file ends before offset {active_offset}"
        );
    }

    let available = active_len.saturating_sub(active_offset);
    if available == 0 {
        ensure_pending_file_identity(expected_identity, &metadata)?;
        return Ok(PendingTerminalAppendState::Absent);
    }

    let read_len =
        usize::try_from(available.min(line_len)).context("pending terminal tail is too large")?;
    let mut raw = vec![0_u8; read_len];
    file.seek(SeekFrom::Start(active_offset))
        .with_context(|| format!("seek {}", path.display()))?;
    file.read_exact(&mut raw)
        .with_context(|| format!("read pending terminal tail from {}", path.display()))?;

    if available >= line_len && raw == expected {
        return Ok(PendingTerminalAppendState::Committed);
    }
    if available < line_len && raw == expected[..read_len] {
        ensure_pending_file_identity(expected_identity, &metadata)?;
        file.set_len(active_offset)
            .with_context(|| format!("truncate torn terminal tail in {}", path.display()))?;
        file.sync_data()
            .with_context(|| format!("sync truncated terminal tail in {}", path.display()))?;
        return Ok(PendingTerminalAppendState::Absent);
    }

    anyhow::bail!("ambiguous pending terminal append: active bytes do not match the recorded line");
}

fn ensure_pending_file_identity(
    expected: Option<&PendingActiveFileIdentity>,
    metadata: &std::fs::Metadata,
) -> Result<()> {
    let Some(expected) = expected else {
        anyhow::bail!("ambiguous pending terminal append: active file identity is unavailable");
    };
    if pending_active_file_identity(metadata).as_ref() != Some(expected) {
        anyhow::bail!("ambiguous pending terminal append: active file identity changed");
    }
    Ok(())
}

#[cfg(unix)]
fn pending_active_file_identity(metadata: &std::fs::Metadata) -> Option<PendingActiveFileIdentity> {
    Some(PendingActiveFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn pending_active_file_identity(
    _metadata: &std::fs::Metadata,
) -> Option<PendingActiveFileIdentity> {
    None
}

fn trim_ascii_line(mut raw: &[u8]) -> &[u8] {
    while raw.last().is_some_and(u8::is_ascii_whitespace) {
        raw = &raw[..raw.len() - 1];
    }
    while raw.first().is_some_and(u8::is_ascii_whitespace) {
        raw = &raw[1..];
    }
    raw
}

fn write_verdict_index(path: &Path, index: &ProgressVerdictIndex) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(index).context("serialize progress verdict index")?;
    atomic_write_durable(&progress_verdict_index_path(path), &bytes)
}

fn ensure_verdict_index_locked(path: &Path, checkpoint: Option<&ProgressCheckpoint>) -> Result<()> {
    if read_verdict_index_locked(path)?.is_some() {
        return Ok(());
    }

    let mut index = ProgressVerdictIndex::default();
    if let Some(checkpoint) = checkpoint {
        index.verdicts = checkpoint.turn_verdicts.clone();
        index.terminal_turns = checkpoint.terminal_turns.clone();
    }
    // Recovery folds the retained archive into the checkpoint first. Only the
    // active journal remains to backfill, once, outside GET/PUT request paths.
    journal::scan_stream(path, |event| {
        if let Some(verdict) = parse_turn_verdict_event(&event) {
            index
                .verdicts
                .entry(verdict.sid.clone())
                .or_default()
                .insert(verdict.turn_id.clone(), verdict);
        }
        if let Some((sid, turn_id)) = terminal_turn_identity(&event) {
            index
                .terminal_turns
                .entry(sid.to_string())
                .or_default()
                .entry(turn_id.to_string())
                .or_insert(event);
        }
    })?;
    write_verdict_index(path, &index)
}

fn verdict_content_eq(left: &TurnVerdict, right: &TurnVerdict) -> bool {
    left.verdict == right.verdict && left.feedback == right.feedback
}

fn append_event_at(
    path: &Path,
    event: &Value,
    now: Instant,
    min_interval_override: Option<Duration>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut line = Vec::new();
    serde_json::to_writer(&mut line, event).context("serialize progress event")?;
    line.push(b'\n');
    let byte_count = u64::try_from(line.len()).unwrap_or(u64::MAX);

    let raw_kind = event_kind_name(event);
    let known_kind = raw_kind.and_then(EventKind::from_wire_name);
    let kind_name = raw_kind.unwrap_or("<unknown>");
    let unknown = known_kind.is_none();

    // Warn once per unknown kind per process: hook-fallback kinds are
    // legitimate high-volume facts, and a per-event warn would itself be
    // the log-spam this gate exists to remove. The stats map gains an
    // entry after the first record_* call, so its absence marks first sight.
    if unknown && !admission_state().stats.contains_key(kind_name) {
        tracing::warn!(
            kind = kind_name,
            "progress admission: unknown event kind persisted as a fact"
        );
    }

    let event_class = known_kind.map(class).unwrap_or(EventClass::Fact);
    let reservation = match event_class {
        EventClass::Fact => None,
        EventClass::Telemetry => {
            record_suppressed(kind_name, unknown, byte_count);
            return Ok(());
        }
        EventClass::LatestState {
            min_interval,
            scope,
        } => {
            let content_payload = semantic_content_payload(event);
            if semantic_payload_is_all_null(&content_payload) {
                record_suppressed(kind_name, unknown, byte_count);
                return Ok(());
            }

            let key = AdmissionKey {
                path: canonical_admission_path(path)?,
                kind: known_kind.expect("latest-state classes are schema-owned"),
                scope: match scope {
                    EventScope::Project => None,
                    EventScope::Session => {
                        event.get("sid").and_then(Value::as_str).map(str::to_owned)
                    }
                },
            };
            let hash = semantic_hash(event)?;
            let min_interval = min_interval_override.unwrap_or(min_interval);
            match reserve_latest(key, hash, now, min_interval) {
                Some(reservation) => Some(reservation),
                None => {
                    record_suppressed(kind_name, unknown, byte_count);
                    return Ok(());
                }
            }
        }
    };

    let result = append_serialized(path, &line);
    if let Some(reservation) = reservation {
        finish_latest(reservation, result.is_ok(), now);
    }
    let rotated = result?;
    record_appended(kind_name, unknown, byte_count);
    if let Some(observer) = PERSIST_OBSERVER.get() {
        observer(path, rotated);
    }
    Ok(())
}

fn event_kind_name(event: &Value) -> Option<&str> {
    event
        .get("event")
        .and_then(Value::as_str)
        .or_else(|| event.get("kind").and_then(Value::as_str))
}

fn append_serialized(path: &Path, line: &[u8]) -> Result<bool> {
    let lock_file = open_progress_lock(path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;

    append_serialized_locked(path, line)
}

fn append_serialized_locked(path: &Path, line: &[u8]) -> Result<bool> {
    settle_pending_progress_write_locked(path)?;
    if !path.exists()
        && !progress_archive_path(path).exists()
        && !progress_checkpoint_path(path).exists()
        && !progress_verdict_index_path(path).exists()
    {
        write_verdict_index(path, &ProgressVerdictIndex::default())?;
    }
    // A real crash in the rename -> checkpoint window leaves `.1` present and
    // active absent. Recover before accepting another row so an uncovered
    // archive can never survive until a later rotation replaces it.
    if !path.exists() && progress_archive_path(path).exists() {
        recover_progress_checkpoint_locked(path)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(line)
        .with_context(|| format!("write event to {}", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    drop(file);

    if size > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn progress_sibling_path(active_path: &Path, suffix: &str) -> PathBuf {
    let file_name = active_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let stem = file_name.strip_suffix(".jsonl").unwrap_or(&file_name);
    active_path.with_file_name(format!("{stem}{suffix}"))
}

fn progress_lock_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".lock")
}

fn open_progress_lock(active_path: &Path) -> Result<File> {
    let path = progress_lock_path(active_path);
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))
}

fn rotate_progress_locked(active_path: &Path) -> Result<()> {
    settle_pending_progress_write_locked(active_path)?;
    // Upgrade/backfill the retained archive and materialize the verdict index
    // before `.1` is replaced. In particular, a v1 checkpoint covered `.1`
    // aggregates but carried no verdict/completion projection.
    let checkpoint = recover_progress_checkpoint_locked(active_path)?;
    ensure_verdict_index_locked(active_path, checkpoint.as_ref())?;
    let archive_path = progress_archive_path(active_path);
    match std::fs::remove_file(&archive_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("remove {}", archive_path.display()))
        }
    }
    std::fs::rename(active_path, &archive_path).with_context(|| {
        format!(
            "rotate {} -> {}",
            active_path.display(),
            archive_path.display()
        )
    })?;

    // Crash consistency is marker based: rename first, then fold the now
    // immutable archive and atomically publish the checkpoint. A crash between
    // these operations leaves a marker mismatch that startup hydration repairs.
    recover_progress_checkpoint_locked(active_path)?;
    File::create(active_path).with_context(|| format!("create {}", active_path.display()))?;
    Ok(())
}

fn settle_pending_progress_write_locked(path: &Path) -> Result<()> {
    let index_path = progress_verdict_index_path(path);
    match std::fs::metadata(&index_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("stat {}", index_path.display()));
        }
    }
    if read_verdict_index_locked(path)?.is_none() {
        anyhow::bail!(
            "cannot mutate {} while its progress verdict index is invalid",
            path.display()
        );
    }
    Ok(())
}

fn recover_progress_checkpoint_locked(active_path: &Path) -> Result<Option<ProgressCheckpoint>> {
    let archive_path = progress_archive_path(active_path);
    let archive = archive_coverage_for_path(&archive_path)?;
    let mut checkpoint = read_progress_checkpoint(active_path)?;

    if checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.schema_version == 1)
    {
        let mut upgraded = checkpoint.take().expect("checked above");
        if coverage_matches_legacy(upgraded.coverage.as_ref(), archive.as_ref()) {
            if archive.is_some() {
                journal::scan_stream(&archive_path, |event| {
                    fold_checkpoint_projection(&mut upgraded, event);
                })?;
            }
            // This archive was already included in v1 lifetime aggregates.
            // Backfill only the state projections or costs/counts double.
            upgraded.schema_version = CHECKPOINT_SCHEMA_VERSION;
            upgraded.coverage = archive.clone();
            write_progress_checkpoint(active_path, &upgraded)?;
            checkpoint = Some(upgraded);
        } else {
            // The current archive is new and will be folded normally below.
            upgraded.schema_version = CHECKPOINT_SCHEMA_VERSION;
            checkpoint = Some(upgraded);
        }
    }

    if checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.schema_version == 2)
    {
        let mut upgraded = checkpoint.take().expect("checked above");
        if coverage_matches_legacy(upgraded.coverage.as_ref(), archive.as_ref()) {
            // v2 already contains lifetime aggregates and both projections.
            // Its marker cannot distinguish a same-size/same-first-line
            // generation, so refolding here would corrupt every healthy
            // production checkpoint. Upgrade the covered generation in place;
            // v3's full digest makes every later replacement unambiguous.
            upgraded.schema_version = CHECKPOINT_SCHEMA_VERSION;
            upgraded.coverage = archive.clone();
            write_progress_checkpoint(active_path, &upgraded)?;
            checkpoint = Some(upgraded);
        } else {
            upgraded.schema_version = CHECKPOINT_SCHEMA_VERSION;
            checkpoint = Some(upgraded);
        }
    }

    let Some(archive) = archive else {
        if let Some(checkpoint) = checkpoint.as_ref() {
            if checkpoint.schema_version == CHECKPOINT_SCHEMA_VERSION
                && !progress_checkpoint_path(active_path).exists()
            {
                write_progress_checkpoint(active_path, checkpoint)?;
            }
        }
        return Ok(checkpoint);
    };
    if checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint_covers_archive(checkpoint, Some(&archive)))
    {
        return Ok(checkpoint);
    }

    let mut next = checkpoint.take().unwrap_or_default();
    let summary = journal::scan_stream(&archive_path, |event| {
        fold_checkpoint_projection(&mut next, event.clone());
        if let Some(cost) = progress_cost_contribution(&event) {
            next.cost_total_usd += cost.cost_usd;
            if let Some(vendor) = cost.vendor {
                *next
                    .cost_total_by_vendor
                    .entry(vendor.to_string())
                    .or_insert(0.0) += cost.cost_usd;
            }
            if let Some(sid) = cost.sid {
                *next.cost_total_by_sid.entry(sid.to_string()).or_insert(0.0) += cost.cost_usd;
            }
        }
        next.event_count = next.event_count.saturating_add(1);
    })?;
    next.corrupt_line_count = next
        .corrupt_line_count
        .saturating_add(u64::try_from(summary.corrupt_count).unwrap_or(u64::MAX));
    next.rotation_sequence = next.rotation_sequence.saturating_add(1);
    next.schema_version = CHECKPOINT_SCHEMA_VERSION;
    next.coverage = Some(archive);
    write_progress_checkpoint(active_path, &next)?;
    Ok(Some(next))
}

fn coverage_matches_legacy(
    checkpoint: Option<&ArchiveCoverage>,
    archive: Option<&ArchiveCoverage>,
) -> bool {
    match (checkpoint, archive) {
        (None, None) => true,
        (Some(checkpoint), Some(archive)) => {
            checkpoint.byte_size == archive.byte_size
                && checkpoint.first_line_sha256 == archive.first_line_sha256
        }
        _ => false,
    }
}

fn fold_checkpoint_projection(checkpoint: &mut ProgressCheckpoint, event: Value) {
    if let Some(verdict) = parse_turn_verdict_event(&event) {
        checkpoint
            .turn_verdicts
            .entry(verdict.sid.clone())
            .or_default()
            .insert(verdict.turn_id.clone(), verdict);
    }
    if event_kind_name(&event) == Some(CHAT_TURN_COMPLETED) {
        if let (Some(sid), Some(turn_id)) = (
            event.get("sid").and_then(Value::as_str).map(str::to_string),
            event
                .get("turn_id")
                .and_then(Value::as_str)
                .map(str::to_string),
        ) {
            if !sid.is_empty() && !turn_id.is_empty() {
                checkpoint
                    .terminal_turns
                    .entry(sid)
                    .or_default()
                    .entry(turn_id)
                    .or_insert(event);
            }
        }
    }
}

fn write_progress_checkpoint(active_path: &Path, checkpoint: &ProgressCheckpoint) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(checkpoint).context("serialize progress checkpoint")?;
    atomic_write_durable(&progress_checkpoint_path(active_path), &bytes)
}

fn archive_coverage_for_path(path: &Path) -> Result<Option<ArchiveCoverage>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    let byte_size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    let mut full_hasher = Sha256::new();
    let mut first_hasher = Sha256::new();
    let mut first_line_open = true;
    let mut found_bytes = false;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hash archive {}", path.display()))?;
        if read == 0 {
            break;
        }
        found_bytes = true;
        let chunk = &buffer[..read];
        full_hasher.update(chunk);
        if first_line_open {
            let take = chunk
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(chunk.len(), |index| index + 1);
            first_hasher.update(&chunk[..take]);
            first_line_open = take == chunk.len() && chunk.last() != Some(&b'\n');
        }
    }
    Ok(Some(ArchiveCoverage {
        byte_size,
        first_line_sha256: found_bytes.then(|| hex_digest(first_hasher.finalize().as_slice())),
        full_file_sha256: Some(hex_digest(full_hasher.finalize().as_slice())),
    }))
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

/// Repair corrupt byte-lines in the active journal or its `.1` archive.
///
/// The original is first renamed to a timestamped backup and the directory is
/// synced before the repaired tmp is installed. Clean files are left untouched
/// and return `None`, making repeated repair runs idempotent.
pub fn repair_progress_journal(
    active_path: &Path,
    target_path: &Path,
) -> Result<Option<ProgressRepairReport>> {
    let archive_path = progress_archive_path(active_path);
    if target_path != active_path && target_path != archive_path {
        anyhow::bail!(
            "repair target {} is not active/archive for {}",
            target_path.display(),
            active_path.display()
        );
    }
    if let Some(parent) = active_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = open_progress_lock(active_path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    settle_pending_progress_write_locked(active_path)?;
    repair_progress_journal_locked(active_path, target_path)
}

fn repair_progress_journal_locked(
    active_path: &Path,
    target_path: &Path,
) -> Result<Option<ProgressRepairReport>> {
    let input = match File::open(target_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", target_path.display())),
    };
    let repairing_archive = target_path == progress_archive_path(active_path);
    let old_coverage = repairing_archive
        .then(|| archive_coverage_for_path(target_path))
        .transpose()?
        .flatten();
    // Checkpoint damage must never prevent byte repair. A parse/I/O error is
    // already a doctor warning; leave that sidecar untouched and repair the
    // journal, updating coverage only when a valid checkpoint covered `.1`.
    let checkpoint = if repairing_archive {
        read_progress_checkpoint(active_path).ok().flatten()
    } else {
        None
    };
    let checkpoint_covered_old = old_coverage.as_ref().is_some_and(|coverage| {
        checkpoint.as_ref().is_some_and(|checkpoint| {
            if checkpoint.schema_version >= CHECKPOINT_SCHEMA_VERSION {
                checkpoint_covers_archive(checkpoint, Some(coverage))
            } else {
                coverage_matches_legacy(checkpoint.coverage.as_ref(), Some(coverage))
            }
        })
    });

    let tmp_path = unique_maintenance_path(target_path, "repair-tmp");
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)
        .with_context(|| format!("create {}", tmp_path.display()))?;
    let mut reader = BufReader::new(input);
    let mut raw = Vec::new();
    let mut kept_count = 0_u64;
    let mut dropped_count = 0_u64;
    loop {
        raw.clear();
        let read = reader
            .read_until(b'\n', &mut raw)
            .with_context(|| format!("read {}", target_path.display()))?;
        if read == 0 {
            break;
        }
        let line = trim_ascii_bytes(&raw);
        if line.is_empty() {
            continue;
        }
        if serde_json::from_slice::<Value>(line).is_ok() {
            output
                .write_all(&raw)
                .with_context(|| format!("write {}", tmp_path.display()))?;
            if raw.last() != Some(&b'\n') {
                output
                    .write_all(b"\n")
                    .with_context(|| format!("finish line in {}", tmp_path.display()))?;
            }
            kept_count = kept_count.saturating_add(1);
        } else {
            dropped_count = dropped_count.saturating_add(1);
        }
    }

    if dropped_count == 0 {
        drop(output);
        let _ = std::fs::remove_file(&tmp_path);
        return Ok(None);
    }
    output
        .sync_all()
        .with_context(|| format!("fsync {}", tmp_path.display()))?;
    drop(output);
    drop(reader);

    let backup_path = unique_backup_path(target_path);
    std::fs::rename(target_path, &backup_path).with_context(|| {
        format!(
            "back up {} -> {}",
            target_path.display(),
            backup_path.display()
        )
    })?;
    sync_parent_dir(target_path);
    std::fs::rename(&tmp_path, target_path).with_context(|| {
        format!(
            "replace {} from repaired copy {} (original is safe at {})",
            target_path.display(),
            tmp_path.display(),
            backup_path.display()
        )
    })?;
    sync_parent_dir(target_path);

    if checkpoint_covered_old {
        let mut checkpoint = checkpoint.expect("coverage match requires a checkpoint");
        checkpoint.corrupt_line_count = checkpoint.corrupt_line_count.saturating_sub(dropped_count);
        checkpoint.coverage = archive_coverage_for_path(target_path)?;
        write_progress_checkpoint(active_path, &checkpoint)?;
    }

    Ok(Some(ProgressRepairReport {
        kept_count,
        dropped_count,
        backup_path,
    }))
}

fn unique_maintenance_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%.6fZ");
    path.with_file_name(format!("{name}.{suffix}-{stamp}-{}", std::process::id()))
}

fn unique_backup_path(path: &Path) -> PathBuf {
    let candidate = unique_maintenance_path(path, "bak");
    if !candidate.exists() {
        return candidate;
    }
    for sequence in 1_u32.. {
        let numbered = candidate.with_file_name(format!(
            "{}-{sequence}",
            candidate
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ));
        if !numbered.exists() {
            return numbered;
        }
    }
    unreachable!("u32 backup suffix space exhausted")
}

fn trim_ascii_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[derive(Debug)]
struct Reservation {
    key: AdmissionKey,
    id: u64,
    hash: [u8; 32],
}

fn reserve_latest(
    key: AdmissionKey,
    hash: [u8; 32],
    now: Instant,
    min_interval: Duration,
) -> Option<Reservation> {
    let mut state = admission_state();
    let entry = state.latest.entry(key.clone()).or_default();

    if entry.pending.is_some() {
        // Only one write for a key may be in flight. Periodic latest-state
        // sources recover a changed value on their next notification.
        return None;
    }
    if entry
        .persisted
        .is_some_and(|persisted| persisted.hash == hash)
    {
        return None;
    }
    if entry
        .persisted
        .is_some_and(|persisted| now.saturating_duration_since(persisted.at) < min_interval)
    {
        // Deliberately do not retain a suppressed change: these state kinds
        // are periodic, so the next event after the interval recovers it.
        return None;
    }

    state.next_reservation = state.next_reservation.wrapping_add(1);
    let id = state.next_reservation;
    state
        .latest
        .get_mut(&key)
        .expect("latest entry was inserted above")
        .pending = Some(PendingState {
        hash,
        reservation: id,
    });
    Some(Reservation { key, id, hash })
}

fn finish_latest(reservation: Reservation, persisted: bool, now: Instant) {
    let mut state = admission_state();
    let Some(entry) = state.latest.get_mut(&reservation.key) else {
        return;
    };
    if entry.pending.is_none_or(|pending| {
        pending.reservation != reservation.id || pending.hash != reservation.hash
    }) {
        return;
    }
    entry.pending = None;
    if persisted {
        entry.persisted = Some(PersistedState {
            hash: reservation.hash,
            at: now,
        });
    } else if entry.persisted.is_none() {
        state.latest.remove(&reservation.key);
    }
}

fn canonical_admission_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory for progress admission")?
            .join(path)
    };
    if let Some(canonical) = admission_state().canonical_paths.get(&absolute).cloned() {
        return Ok(canonical);
    }

    let canonical = if absolute.exists() {
        std::fs::canonicalize(&absolute)
            .with_context(|| format!("canonicalize {}", absolute.display()))?
    } else {
        let parent = absolute.parent().unwrap_or(Path::new("/"));
        let canonical_parent = std::fs::canonicalize(parent)
            .with_context(|| format!("canonicalize {}", parent.display()))?;
        match absolute.file_name() {
            Some(name) => canonical_parent.join(name),
            None => canonical_parent,
        }
    };
    admission_state()
        .canonical_paths
        .insert(absolute, canonical.clone());
    Ok(canonical)
}

fn semantic_hash(event: &Value) -> Result<[u8; 32]> {
    let mut payload = event.clone();
    if let Some(object) = payload.as_object_mut() {
        object.remove("ts");
    }
    let bytes = serde_json::to_vec(&payload).context("serialize semantic progress payload")?;
    Ok(Sha256::digest(bytes).into())
}

fn semantic_content_payload(event: &Value) -> Value {
    const METADATA_FIELDS: &[&str] = &[
        "event",
        "kind",
        "vendor",
        "ts",
        "role",
        "sid",
        "slug",
        "thread_id",
        "turn_id",
        "session",
    ];

    let mut payload = event.clone();
    if let Some(object) = payload.as_object_mut() {
        for field in METADATA_FIELDS {
            object.remove(*field);
        }
    }
    payload
}

/// True when a semantic payload has no non-null leaf. Empty objects/arrays and
/// arbitrarily nested null-only structures are empty state snapshots.
pub fn semantic_payload_is_all_null(payload: &Value) -> bool {
    match payload {
        Value::Null => true,
        Value::Array(values) => values.iter().all(semantic_payload_is_all_null),
        Value::Object(values) => values.values().all(semantic_payload_is_all_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn record_appended(kind: &str, unknown: bool, bytes: u64) {
    let mut state = admission_state();
    let counters = state.stats.entry(kind.to_string()).or_default();
    counters.unknown |= unknown;
    counters.appended_count = counters.appended_count.saturating_add(1);
    counters.appended_bytes = counters.appended_bytes.saturating_add(bytes);
}

fn record_suppressed(kind: &str, unknown: bool, bytes: u64) {
    let mut state = admission_state();
    let counters = state.stats.entry(kind.to_string()).or_default();
    counters.unknown |= unknown;
    counters.suppressed_count = counters.suppressed_count.saturating_add(1);
    counters.suppressed_bytes = counters.suppressed_bytes.saturating_add(bytes);
}

#[cfg(unix)]
struct ProgressFileLock(std::os::fd::RawFd);

#[cfg(unix)]
impl ProgressFileLock {
    fn lock(file: &std::fs::File) -> std::io::Result<Self> {
        let fd = file.as_raw_fd();
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc == 0 {
            Ok(Self(fd))
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(unix)]
impl Drop for ProgressFileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct ProgressFileLock;

#[cfg(not(unix))]
impl ProgressFileLock {
    fn lock(_file: &std::fs::File) -> std::io::Result<Self> {
        Ok(Self)
    }
}

pub fn build_chat_tool_call_started_event(role: &str, tool: &str) -> Value {
    json!({
        "event": CHAT_TOOL_CALL_STARTED,
        "role": role,
        "tool": tool,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// v0.8.7 review-fix (R-L1) — a HITL permission prompt is OUTSTANDING: the
/// session is parked awaiting a human approve/deny for `tool` (`summary` is
/// the one-line tool-call preview). Lets an operator see a parked agent
/// instead of mistaking the silence for a stuck/dead session. `ttl_secs` is
/// the prompt's deadline (deny on lapse — fail-safe).
pub fn build_chat_permission_prompt_outstanding_event(
    role: &str,
    tool: &str,
    summary: &str,
    ttl_secs: u64,
) -> Value {
    let trimmed: String = summary.chars().take(256).collect();
    json!({
        "event": CHAT_PERMISSION_PROMPT_OUTSTANDING,
        "role": role,
        "tool": tool,
        "summary": trimmed,
        "ttl_secs": ttl_secs,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_started_event(role: &str, project_dir: &str) -> Value {
    json!({
        "event": CHAT_SESSION_STARTED,
        "role": role,
        "project_dir": project_dir,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_user_prompt_event(
    role: &str,
    sid: &str,
    turn_id: &str,
    prompt_excerpt: &str,
) -> Value {
    let trimmed: String = prompt_excerpt.chars().take(256).collect();
    json!({
        "event": CHAT_TURN_USER_PROMPT,
        "role": role,
        "sid": sid,
        "turn_id": turn_id,
        "prompt_excerpt": trimmed,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// `model` is the turn's canonical model id (e.g. `claude-opus-4-8`) for
/// deterministic per-turn cost pricing — written ONLY when present
/// (`Some`); a `None` (e.g. the tmux Stop hook, which carries no model)
/// omits the key so the cost path treats the turn as unpriced (exposed,
/// never billed at a fallback rate).
pub fn build_chat_turn_completed_event(
    role: &str,
    sid: &str,
    turn_id: &str,
    usage: &ccteam_cost::UnifiedTokenUsage,
    model: Option<&str>,
) -> Value {
    build_chat_turn_completed_event_with_vendor(role, sid, turn_id, usage, model, None)
}

/// Build a paneless terminal turn row with the vendor needed for shared cost
/// pricing. The vendor is additive so older hook-produced rows remain valid.
pub fn build_chat_turn_completed_event_with_vendor(
    role: &str,
    sid: &str,
    turn_id: &str,
    usage: &ccteam_cost::UnifiedTokenUsage,
    model: Option<&str>,
    vendor: Option<&str>,
) -> Value {
    build_chat_turn_completed_event_with_metadata(
        role,
        sid,
        turn_id,
        usage,
        model,
        vendor,
        &ChatTurnCompletionMetadata::default(),
    )
}

/// Build a completed-turn row with facts captured at that exact turn
/// boundary. The optional fields are additive; missing values are omitted.
#[allow(clippy::too_many_arguments)]
pub fn build_chat_turn_completed_event_with_metadata(
    role: &str,
    sid: &str,
    turn_id: &str,
    usage: &ccteam_cost::UnifiedTokenUsage,
    model: Option<&str>,
    vendor: Option<&str>,
    metadata: &ChatTurnCompletionMetadata,
) -> Value {
    let mut ev = json!({
        "event": CHAT_TURN_COMPLETED,
        "role": role,
        "sid": sid,
        "turn_id": turn_id,
        "usage": serde_json::to_value(usage).unwrap_or(Value::Null),
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(model) = model.filter(|m| !m.is_empty()) {
        ev["model"] = Value::String(model.to_string());
    }
    if let Some(vendor) = vendor.filter(|v| !v.is_empty()) {
        ev["vendor"] = Value::String(vendor.to_string());
    }
    if let Some(outcome) = metadata
        .outcome
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        ev["outcome"] = Value::String(outcome.to_string());
    }
    if let Some(duration_ms) = metadata.duration_ms {
        ev["duration_ms"] = Value::from(duration_ms);
    }
    if let Some(role_sha) = metadata
        .role_sha
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        ev["role_sha"] = Value::String(role_sha.to_string());
    }
    if let Some(skills_sha) = &metadata.skills_sha {
        ev["skills_sha"] = serde_json::to_value(skills_sha).unwrap_or(Value::Null);
    }
    if let Some(signals) = &metadata.signals {
        ev["signals"] = serde_json::to_value(signals).unwrap_or(Value::Null);
    }
    ev
}

/// Build the terminal success row consumed by progress and cost queries.
/// Keeping the complete `agent_done` shape here makes this module the sole
/// schema authority; adapters only translate vendor events into these fields.
#[allow(clippy::too_many_arguments)]
pub fn build_agent_done_completed_event(
    role: &str,
    session_id: &str,
    slug: &str,
    vendor: &str,
    thread_id: &str,
    turn_id: &str,
    usage: &ccteam_cost::UnifiedTokenUsage,
    cost_usd: Option<f64>,
) -> Value {
    let mut event = json!({
        "event": AGENT_DONE,
        "role": role,
        "session_id": session_id,
        "slug": slug,
        "status": "completed",
        "vendor": vendor,
        "thread_id": thread_id,
        "turn_id": turn_id,
        "usage": serde_json::to_value(usage).unwrap_or(Value::Null),
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(cost_usd) = cost_usd {
        event["cost_usd"] = json!(cost_usd);
    }
    event
}

/// Build a terminal vendor-error row. `turn_id` is absent for failures that
/// happen before a vendor turn is established (for example, connect errors).
#[allow(clippy::too_many_arguments)]
pub fn build_agent_done_errored_event(
    role: &str,
    session_id: &str,
    slug: &str,
    vendor: &str,
    thread_id: &str,
    turn_id: Option<&str>,
    error_kind: &str,
    error: &str,
) -> Value {
    let mut event = json!({
        "event": AGENT_DONE,
        "role": role,
        "session_id": session_id,
        "slug": slug,
        "status": "errored",
        "vendor": vendor,
        "error_kind": error_kind,
        "error": error,
        "thread_id": thread_id,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(turn_id) = turn_id.filter(|value| !value.is_empty()) {
        event["turn_id"] = Value::String(turn_id.to_string());
    }
    event
}

pub fn build_chat_session_reset_event(role: &str, sid: &str) -> Value {
    json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "sid": sid,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_reset_event_with_reason(role: &str, sid: &str, reason: &str) -> Value {
    json!({
        "event": CHAT_SESSION_RESET,
        "role": role,
        "sid": sid,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_session_reset_with_recovery_event(
    role: &str,
    sid: &str,
    recovered_turns: usize,
) -> Value {
    json!({
        "event": CHAT_SESSION_RESET_WITH_RECOVERY,
        "role": role,
        "sid": sid,
        "recovered_turns": recovered_turns,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_compact_done_event(role: &str) -> Value {
    json!({
        "event": CHAT_COMPACT_DONE,
        "role": role,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_hop_escalate_event(role: &str, hop_count: u32, last_bot: &str) -> Value {
    json!({
        "event": CHAT_HOP_ESCALATE,
        "role": role,
        "hop_count": hop_count,
        "last_bot": last_bot,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_bot_permanent_failure_event(role: &str, reason: &str, attempts: u32) -> Value {
    let trimmed: String = reason.chars().take(512).collect();
    json!({
        "event": CHAT_BOT_PERMANENT_FAILURE,
        "role": role,
        "reason": trimmed,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_marker_self_heal_attempt_event(role: &str, attempt_n: u32) -> Value {
    json!({
        "event": CHAT_MARKER_SELF_HEAL_ATTEMPT,
        "role": role,
        "attempt_n": attempt_n,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// The mid-turn "still working" heartbeat. `sid` is REQUIRED: the read-side
/// activity classifier selects a session's latest event by sid, so an untagged
/// heartbeat is invisible to the session it describes (and would leak onto its
/// siblings through the project-tail fallback). Same field order as
/// [`build_chat_turn_timeout_event`] — the two are the busy/stuck ends of the
/// same turn-liveness family.
pub fn build_chat_turn_running_long_event(
    role: &str,
    sid: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    json!({
        "event": CHAT_TURN_RUNNING_LONG,
        "role": role,
        "sid": sid,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_chat_turn_timeout_event(
    role: &str,
    sid: &str,
    slug: &str,
    turn_id: &str,
    elapsed_sec: u64,
) -> Value {
    json!({
        "event": CHAT_TURN_TIMEOUT,
        "role": role,
        "sid": sid,
        "slug": slug,
        "turn_id": turn_id,
        "elapsed_sec": elapsed_sec,
        "stuck": true,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-09 — the inbound attachment for `sid` ended while the session is still
/// live. `sid` is REQUIRED (the read-side classifier selects by sid) and the
/// row is deliberately an IDLE boundary: a detached session is not working,
/// and claiming otherwise is the exact lie this family exists to stop.
pub fn build_session_stream_detached_event(
    role: &str,
    sid: &str,
    slug: &str,
    reason: &str,
) -> Value {
    json!({
        "event": SESSION_STREAM_DETACHED,
        "role": role,
        "sid": sid,
        "slug": slug,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-09 — the rebuilt attachment for `sid` delivered its first event.
/// `gap_ms` measures how long the session was unobservable, `attempts` how
/// many rebuilds it took.
pub fn build_session_stream_reattached_event(
    role: &str,
    sid: &str,
    slug: &str,
    gap_ms: u64,
    attempts: u32,
) -> Value {
    json!({
        "event": SESSION_STREAM_REATTACHED,
        "role": role,
        "sid": sid,
        "slug": slug,
        "gap_ms": gap_ms,
        "attempts": attempts,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// v0.9.2 — build the durable capacity-eviction lifecycle event. `reason` is
/// currently `"capacity"`; keeping it explicit leaves the schema extensible
/// without inventing a second event family.
pub fn build_session_evicted_event(sid: &str, reason: &str) -> Value {
    json!({
        "event": SESSION_EVICTED,
        "sid": sid,
        "reason": reason,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-19 — a session's body is running without this daemon reading it.
/// `reason` = `daemon_shutdown` | `daemon_restart`; `pid` is the body; `in_flight`
/// says whether a vendor turn was known to be running when the daemon let go
/// (`None` = not known, e.g. found after a restart).
pub fn build_session_body_detached_event(
    sid: &str,
    slug: &str,
    reason: &str,
    pid: Option<u32>,
    in_flight: Option<bool>,
) -> Value {
    json!({
        "event": SESSION_BODY_DETACHED,
        "sid": sid,
        "slug": slug,
        "reason": reason,
        "pid": pid,
        "in_flight": in_flight,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// 2026-08-19 — a detached body ended. `reason` = `exited` | `stopped`;
/// `recovered` = the unobserved answer was recovered from the vendor's record.
pub fn build_session_body_exited_event(
    sid: &str,
    slug: &str,
    reason: &str,
    pid: u32,
    recovered: bool,
) -> Value {
    json!({
        "event": SESSION_BODY_EXITED,
        "sid": sid,
        "slug": slug,
        "reason": reason,
        "pid": pid,
        "recovered": recovered,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_typed_event_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    json!({
        "kind": TYPED_EVENT,
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_merger_lossy_partial_event(
    vendor: &str,
    event_kind: &str,
    captured: &str,
    session: &str,
) -> Value {
    json!({
        "kind": MERGER_LOSSY_PARTIAL,
        "vendor": vendor,
        "event_kind": event_kind,
        "captured": captured,
        "session": session,
        "ts": Utc::now().to_rfc3339(),
    })
}

/// v0.9.0 W2 — build one `delegation_*` progress event. `event` is one of the
/// `DELEGATION_*` consts. The unified payload is `{parent_sid, child_sid,
/// vendor, host, turn?, title?, reason?}` — optional fields are omitted when
/// `None`/empty so a `delegation_denied{reason}` and a `delegation_spawned`
/// share one shape without null noise.
#[allow(clippy::too_many_arguments)]
pub fn build_delegation_event(
    event: &str,
    parent_sid: &str,
    child_sid: &str,
    vendor: &str,
    host: &str,
    turn: Option<&str>,
    title: Option<&str>,
    reason: Option<&str>,
) -> Value {
    let mut ev = json!({
        "event": event,
        "parent_sid": parent_sid,
        "child_sid": child_sid,
        "vendor": vendor,
        "host": host,
        "ts": Utc::now().to_rfc3339(),
    });
    let obj = ev.as_object_mut().expect("json object");
    if let Some(turn) = turn.filter(|t| !t.is_empty()) {
        obj.insert("turn".to_string(), Value::String(turn.to_string()));
    }
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        obj.insert("title".to_string(), Value::String(title.to_string()));
    }
    if let Some(reason) = reason.filter(|r| !r.is_empty()) {
        obj.insert("reason".to_string(), Value::String(reason.to_string()));
    }
    ev
}

/// Build one scheduled-message lifecycle row. The full message body never
/// enters `progress.jsonl`; callers may supply only a hard-capped preview.
pub fn build_scheduled_event(
    event: &str,
    id: &str,
    sid: &str,
    send_at: &str,
    preview: Option<&str>,
    reason: Option<&str>,
) -> Value {
    let mut ev = json!({
        "event": event,
        "id": id,
        "sid": sid,
        "send_at": send_at,
        "ts": Utc::now().to_rfc3339(),
    });
    let obj = ev.as_object_mut().expect("json object");
    if let Some(preview) = preview.filter(|value| !value.is_empty()) {
        obj.insert(
            "preview".to_string(),
            Value::String(preview.chars().take(80).collect()),
        );
    }
    if let Some(reason) = reason.filter(|value| !value.is_empty()) {
        obj.insert(
            "reason".to_string(),
            Value::String(reason.chars().take(256).collect()),
        );
    }
    ev
}

pub fn build_codex_plan_updated_event(
    thread_id: &str,
    turn_id: &str,
    explanation: Option<&str>,
    plan: Value,
) -> Value {
    let mut v = json!({
        "event": CODEX_PLAN_UPDATED,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "plan": plan,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(explanation) = explanation {
        v.as_object_mut().unwrap().insert(
            "explanation".to_string(),
            Value::String(explanation.to_string()),
        );
    }
    v
}

pub fn build_codex_token_usage_event(
    thread_id: &str,
    turn_id: &str,
    total: Value,
    last: Value,
    model_context_window: Option<i64>,
) -> Value {
    let mut v = json!({
        "event": CODEX_TOKEN_USAGE,
        "vendor": "codex",
        "thread_id": thread_id,
        "turn_id": turn_id,
        "total": total,
        "last": last,
        "ts": Utc::now().to_rfc3339(),
    });
    if let Some(window) = model_context_window {
        v.as_object_mut().unwrap().insert(
            "model_context_window".to_string(),
            Value::Number(window.into()),
        );
    }
    v
}

pub fn build_codex_thread_status_event(
    thread_id: &str,
    status: &str,
    active_flags: Vec<String>,
) -> Value {
    json!({
        "event": CODEX_THREAD_STATUS,
        "vendor": "codex",
        "thread_id": thread_id,
        "status": status,
        "active_flags": active_flags,
        "ts": Utc::now().to_rfc3339(),
    })
}

pub fn build_codex_rate_limit_event(snapshot: Value) -> Value {
    json!({
        "event": CODEX_RATE_LIMIT,
        "vendor": "codex",
        "snapshot": snapshot,
        "ts": Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_verdict(
        verdict: Verdict,
        feedback: Option<&str>,
        ts: chrono::DateTime<Utc>,
    ) -> TurnVerdict {
        TurnVerdict {
            sid: "s1".into(),
            turn_id: "turn-1".into(),
            ts,
            verdict,
            feedback: feedback.map(str::to_owned),
        }
    }

    fn read_rows(path: &Path) -> Vec<Value> {
        match std::fs::read_to_string(path) {
            Ok(body) => body
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read {}: {error}", path.display()),
        }
    }

    #[test]
    fn append_event_writes_exactly_one_jsonl_record_for_multiline_values() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": "chat_tool_use",
            "tool": "Bash",
            "cmd": "printf 'a\\nb\\n'",
        });

        append_event(&path, &event).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(serde_json::from_str::<Value>(lines[0]).unwrap(), event);
    }

    #[test]
    fn terminal_turn_identity_is_durable_and_first_canonical_fact_wins() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let first = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
            "vendor": "claude",
            "outcome": "failed",
            "usage": {},
        });
        let stale_replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
            "vendor": "claude",
            "outcome": "completed",
            "usage": {},
        });

        append_event(&path, &first).unwrap();
        append_event(&path, &stale_replay).unwrap();

        let rows = read_rows(&path);
        assert_eq!(
            rows,
            vec![first],
            "a restart replay must not replace the first terminal fact"
        );
    }

    #[test]
    fn terminal_first_wins_across_archive_active_and_index_rebuild() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let archive = progress_archive_path(&path);
        let first = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
            "outcome": "failed",
        });
        let stale_active = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
            "outcome": "completed",
        });
        std::fs::write(
            &archive,
            format!("{}\n", serde_json::to_string(&first).unwrap()),
        )
        .unwrap();
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&stale_active).unwrap()),
        )
        .unwrap();

        load_or_recover_progress_checkpoint(&path).unwrap();
        std::fs::remove_file(progress_verdict_index_path(&path)).unwrap();

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T02:00:00Z",
            "outcome": "completed",
        });
        let admitted = append_chat_turn_completed_if_absent(&path, &replay).unwrap();

        assert!(!admitted.appended);
        assert_eq!(admitted.event, first);
        assert_eq!(read_rows(&path), vec![stale_active]);
    }

    #[test]
    fn terminal_pending_index_recovers_only_an_exact_landed_line() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let first = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
        });
        let mut line = serde_json::to_vec(&first).unwrap();
        line.push(b'\n');
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: first.clone(),
                active_offset: 0,
                line_len: line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
                active_file_identity: None,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        std::fs::write(&path, &line).unwrap();

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
        });
        let recovered = append_chat_turn_completed_if_absent(&path, &replay).unwrap();
        assert!(!recovered.appended);
        assert_eq!(recovered.event, first);
        assert_eq!(read_rows(&path).len(), 1);

        let missing_path = tmp.path().join("missing-line.jsonl");
        let missing = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: first,
                active_offset: 0,
                line_len: line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
                active_file_identity: None,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&missing_path, &missing).unwrap();
        let admitted = append_chat_turn_completed_if_absent(&missing_path, &replay).unwrap();
        assert!(admitted.appended);
        assert_eq!(read_rows(&missing_path), vec![replay]);
    }

    #[test]
    fn terminal_pending_index_truncates_an_exact_torn_tail_before_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let pending_event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
        });
        let mut pending_line = serde_json::to_vec(&pending_event).unwrap();
        pending_line.push(b'\n');
        let existing = json!({"event": "session_started", "sid": "s1"});
        let mut existing_line = serde_json::to_vec(&existing).unwrap();
        existing_line.push(b'\n');
        let active_offset = existing_line.len() as u64;
        let mut torn_file = existing_line.clone();
        torn_file.extend_from_slice(&pending_line[..pending_line.len() / 2]);
        std::fs::write(&path, &torn_file).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: pending_event,
                active_offset,
                line_len: pending_line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&pending_line).as_slice()),
                active_file_identity,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
        });
        let admitted = append_chat_turn_completed_if_absent(&path, &replay).unwrap();

        assert!(admitted.appended);
        assert_eq!(admitted.event, replay);
        let mut expected_file = existing_line;
        expected_file.extend_from_slice(&serde_json::to_vec(&replay).unwrap());
        expected_file.push(b'\n');
        assert_eq!(std::fs::read(&path).unwrap(), expected_file);
    }

    #[test]
    fn terminal_pending_index_preserves_an_ambiguous_tail() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let pending_event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
        });
        let mut pending_line = serde_json::to_vec(&pending_event).unwrap();
        pending_line.push(b'\n');
        let unrelated = b"unrelated later bytes";
        std::fs::write(&path, unrelated).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: pending_event,
                active_offset: 0,
                line_len: pending_line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&pending_line).as_slice()),
                active_file_identity,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
        });

        let error = append_chat_turn_completed_if_absent(&path, &replay)
            .unwrap_err()
            .to_string();

        assert!(error.contains("ambiguous pending terminal append"));
        assert_eq!(std::fs::read(&path).unwrap(), unrelated);
    }

    #[test]
    fn ordinary_append_resolves_absent_or_torn_terminal_pending_first() {
        for torn in [false, true] {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("progress.jsonl");
            let existing = json!({"event": "session_started", "sid": "s1"});
            let mut existing_line = serde_json::to_vec(&existing).unwrap();
            existing_line.push(b'\n');
            let active_offset = existing_line.len() as u64;
            let pending_event = json!({
                "event": CHAT_TURN_COMPLETED,
                "sid": "s1",
                "turn_id": "turn-1",
                "ts": "2026-08-28T00:00:00Z",
            });
            let mut pending_line = serde_json::to_vec(&pending_event).unwrap();
            pending_line.push(b'\n');
            let mut active = existing_line;
            if torn {
                active.extend_from_slice(&pending_line[..pending_line.len() / 2]);
            }
            std::fs::write(&path, active).unwrap();
            let active_file_identity =
                pending_active_file_identity(&std::fs::metadata(&path).unwrap());
            let index = ProgressVerdictIndex {
                pending: Some(PendingProgressIndexWrite::TerminalTurn {
                    event: pending_event,
                    active_offset,
                    line_len: pending_line.len() as u64,
                    line_sha256: hex_digest(Sha256::digest(&pending_line).as_slice()),
                    active_file_identity,
                }),
                ..ProgressVerdictIndex::default()
            };
            write_verdict_index(&path, &index).unwrap();

            let ordinary = json!({"event": "ordinary_fact", "sid": "s1"});
            append_event(&path, &ordinary).unwrap();
            let replay = json!({
                "event": CHAT_TURN_COMPLETED,
                "sid": "s1",
                "turn_id": "turn-1",
                "ts": "2026-08-28T01:00:00Z",
            });
            let admitted = append_chat_turn_completed_if_absent(&path, &replay).unwrap();

            assert!(admitted.appended);
            assert_eq!(read_rows(&path), vec![existing, ordinary, replay]);
        }
    }

    #[test]
    fn repair_settles_a_torn_terminal_pending_write_before_replacing_active() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let existing = json!({"event": "session_started", "sid": "s1"});
        let mut existing_line = serde_json::to_vec(&existing).unwrap();
        existing_line.push(b'\n');
        let corrupt_line = b"not-json\n";
        let pending_event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
        });
        let mut pending_line = serde_json::to_vec(&pending_event).unwrap();
        pending_line.push(b'\n');
        let active_offset = (existing_line.len() + corrupt_line.len()) as u64;
        let mut active = existing_line;
        active.extend_from_slice(corrupt_line);
        active.extend_from_slice(&pending_line[..pending_line.len() / 2]);
        std::fs::write(&path, active).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: pending_event,
                active_offset,
                line_len: pending_line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&pending_line).as_slice()),
                active_file_identity,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();

        let report = repair_progress_journal(&path, &path).unwrap().unwrap();

        assert_eq!(report.dropped_count, 1);
        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
        });
        let admitted = append_chat_turn_completed_if_absent(&path, &replay).unwrap();
        assert!(admitted.appended);
        assert_eq!(read_rows(&path), vec![existing, replay]);
    }

    #[test]
    fn archive_coverage_hashes_the_entire_generation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let active = tmp.path().join("progress.jsonl");
        let archive = progress_archive_path(&active);
        std::fs::write(&archive, b"same-first-line\ntail-generation-a\n").unwrap();
        let first = progress_archive_coverage(&active).unwrap().unwrap();

        std::fs::write(&archive, b"same-first-line\ntail-generation-b\n").unwrap();
        let second = progress_archive_coverage(&active).unwrap().unwrap();

        assert_eq!(first.byte_size, second.byte_size);
        assert_ne!(
            first, second,
            "same-size archives with the same first line are distinct"
        );
    }

    #[test]
    fn chat_turn_completed_contributes_priced_cost_to_project_ledger() {
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "vendor": "claude",
            "model": "claude-sonnet-4-6",
            "usage": {"output_tokens": 1_000_000},
        });

        let contribution = progress_cost_contribution(&event).expect("priced chat turn");
        assert_eq!(contribution.vendor, Some("claude"));
        assert_eq!(contribution.sid, Some("s1"));
        assert!((contribution.cost_usd - 15.0).abs() < 1e-9);
    }

    #[test]
    fn codex_chat_turn_is_not_priced_beside_agent_done() {
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "vendor": "codex",
            "model": "gpt-5.5",
            "usage": {"output_tokens": 1_000_000},
        });

        assert!(progress_cost_contribution(&event).is_none());
    }

    #[test]
    fn chat_turn_completed_builder_carries_vendor_for_pricing() {
        let event = build_chat_turn_completed_event_with_vendor(
            "worker",
            "s1",
            "turn-1",
            &ccteam_cost::UnifiedTokenUsage::default(),
            Some("gpt-5.5"),
            Some("codex"),
        );

        assert_eq!(event["vendor"], "codex");
        assert_eq!(event["model"], "gpt-5.5");
    }

    #[test]
    fn chat_turn_completed_metadata_is_additive_and_absent_stays_unknown() {
        let usage = ccteam_cost::UnifiedTokenUsage::default();
        let legacy = build_chat_turn_completed_event_with_vendor(
            "worker",
            "s1",
            "turn-1",
            &usage,
            Some("gpt-5.5"),
            Some("codex"),
        );
        for key in [
            "outcome",
            "duration_ms",
            "role_sha",
            "skills_sha",
            "signals",
        ] {
            assert!(legacy.get(key).is_none(), "{key} must remain unknown");
        }

        let metadata = ChatTurnCompletionMetadata {
            outcome: Some("failed".into()),
            duration_ms: Some(1_234),
            role_sha: Some("abc123".into()),
            skills_sha: Some(BTreeMap::from([("research".into(), "def456".into())])),
            signals: Some(TurnSignals {
                tool_calls: 7,
                steered: true,
                error_recovered: None,
            }),
        };
        let enriched = build_chat_turn_completed_event_with_metadata(
            "worker",
            "s1",
            "turn-1",
            &usage,
            Some("gpt-5.5"),
            Some("codex"),
            &metadata,
        );

        assert_eq!(enriched["outcome"], "failed");
        assert_eq!(enriched["duration_ms"], 1_234);
        assert_eq!(enriched["role_sha"], "abc123");
        assert_eq!(enriched["skills_sha"]["research"], "def456");
        assert_eq!(enriched["signals"]["tool_calls"], 7);
        assert_eq!(enriched["signals"]["steered"], true);
    }

    #[test]
    fn turn_verdict_writer_deduplicates_concurrent_identical_updates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(Verdict::Accept, Some("good"), Utc::now());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let handles = (0..8)
            .map(|_| {
                let path = path.clone();
                let verdict = verdict.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    append_turn_verdict_if_changed(&path, &verdict).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let appended = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|appended| *appended)
            .count();

        assert_eq!(appended, 1);
        assert_eq!(read_rows(&path).len(), 1);
        let duplicate_with_new_ts = sample_verdict(
            Verdict::Accept,
            Some("good"),
            Utc::now() + chrono::Duration::seconds(1),
        );
        assert!(!append_turn_verdict_if_changed(&path, &duplicate_with_new_ts).unwrap());
        assert_eq!(read_rows(&path).len(), 1);
    }

    #[test]
    fn latest_turn_verdict_reads_archive_then_active_and_change_wins() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let archive = progress_archive_path(&path);
        let accepted = sample_verdict(Verdict::Accept, None, Utc::now());
        std::fs::write(
            &archive,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "event": TURN_VERDICT,
                    "sid": accepted.sid,
                    "turn_id": accepted.turn_id,
                    "ts": accepted.ts,
                    "verdict": accepted.verdict,
                }))
                .unwrap()
            ),
        )
        .unwrap();

        assert!(!append_turn_verdict_if_changed(&path, &accepted).unwrap());
        let revised = sample_verdict(Verdict::Revise, Some("fix edge case"), Utc::now());
        assert!(append_turn_verdict_if_changed(&path, &revised).unwrap());

        let latest = latest_turn_verdicts(&path).unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest.get(&("s1".into(), "turn-1".into())), Some(&revised));
        assert!(parse_turn_verdict_event(&json!({"event": "other"})).is_none());
    }

    #[test]
    fn turn_verdict_rejects_an_empty_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let mut verdict = sample_verdict(Verdict::Accept, None, Utc::now());
        verdict.sid.clear();

        assert!(append_turn_verdict_if_changed(&path, &verdict).is_err());
        assert!(read_rows(&path).is_empty());
        assert!(parse_turn_verdict_event(&json!({
            "event": TURN_VERDICT,
            "sid": "",
            "turn_id": "turn-1",
            "ts": Utc::now(),
            "verdict": "accept",
        }))
        .is_none());
    }

    #[test]
    fn session_evicted_event_has_minimal_capacity_shape() {
        let event = build_session_evicted_event("s9", "capacity");
        assert_eq!(event["event"], SESSION_EVICTED);
        assert_eq!(event["sid"], "s9");
        assert_eq!(event["reason"], "capacity");
        assert!(event["ts"].is_string());
    }

    #[test]
    fn errored_agent_done_preserves_kind_and_optional_turn() {
        let with_turn = build_agent_done_errored_event(
            "worker",
            "s9",
            "demo",
            "codex",
            "thread-1",
            Some("turn-2"),
            "server_overloaded",
            "at capacity",
        );
        assert_eq!(with_turn["event"], AGENT_DONE);
        assert_eq!(with_turn["status"], "errored");
        assert_eq!(with_turn["error_kind"], "server_overloaded");
        assert_eq!(with_turn["turn_id"], "turn-2");

        let without_turn = build_agent_done_errored_event(
            "worker",
            "s9",
            "demo",
            "codex",
            "thread-1",
            None,
            "connect",
            "connection failed",
        );
        assert!(without_turn.get("turn_id").is_none());
    }

    #[test]
    fn scheduled_event_never_carries_more_than_an_80_char_preview() {
        let event = build_scheduled_event(
            SCHEDULED_ENQUEUED,
            "d7",
            "s2",
            "2026-07-26T09:30:00Z",
            Some(&"x".repeat(100)),
            None,
        );
        assert_eq!(event["event"], SCHEDULED_ENQUEUED);
        assert_eq!(event["id"], "d7");
        assert_eq!(event["sid"], "s2");
        assert_eq!(event["preview"].as_str().unwrap().chars().count(), 80);
        assert!(event.get("text").is_none());
    }

    #[test]
    fn every_schema_kind_has_an_exhaustive_classification() {
        let mut wire_names = std::collections::HashSet::new();
        for &kind in EventKind::ALL {
            let _ = class(kind);
            assert!(wire_names.insert(kind.wire_name()));
            assert_eq!(EventKind::from_wire_name(kind.wire_name()), Some(kind));
        }
    }

    #[test]
    fn identical_latest_state_is_deduplicated_and_a_change_is_recovered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let now = Instant::now();

        for sequence in 0..10_000 {
            let event = json!({
                "event": CODEX_RATE_LIMIT,
                "vendor": "codex",
                "snapshot": {"primary": {"usedPercent": 80}},
                "ts": format!("volatile-{sequence}"),
            });
            append_event_at(&path, &event, now, Some(Duration::ZERO)).unwrap();
        }
        assert_eq!(read_rows(&path).len(), 1);

        let changed = json!({
            "event": CODEX_RATE_LIMIT,
            "vendor": "codex",
            "snapshot": {"primary": {"usedPercent": 81}},
            "ts": "another-volatile-value",
        });
        append_event_at(&path, &changed, now, Some(Duration::ZERO)).unwrap();
        assert_eq!(read_rows(&path).len(), 2);

        let stats = kind_stats()
            .into_iter()
            .find(|stat| stat.kind == CODEX_RATE_LIMIT)
            .expect("rate-limit counters");
        assert!(stats.appended_count >= 2);
        assert!(stats.appended_bytes > 0);
        assert!(stats.suppressed_count >= 9_999);
        assert!(stats.suppressed_bytes > 0);
    }

    #[test]
    fn null_only_latest_state_is_never_persisted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CODEX_RATE_LIMIT,
            "vendor": "codex",
            "snapshot": {
                "primary": {"usedPercent": null, "resetsAt": null},
                "secondary": null,
            },
            "ts": "volatile",
        });

        append_event_at(&path, &event, Instant::now(), Some(Duration::ZERO)).unwrap();

        assert!(read_rows(&path).is_empty());
    }

    #[test]
    fn running_long_interval_is_scoped_per_sid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let start = Instant::now();
        let running = |sid: &str, elapsed_sec: u64| {
            json!({
                "event": CHAT_TURN_RUNNING_LONG,
                "sid": sid,
                "slug": "demo",
                "turn_id": format!("turn-{sid}"),
                "elapsed_sec": elapsed_sec,
                "ts": format!("volatile-{elapsed_sec}"),
            })
        };

        append_event_at(&path, &running("s1", 300), start, None).unwrap();
        append_event_at(&path, &running("s2", 300), start, None).unwrap();
        append_event_at(
            &path,
            &running("s1", 599),
            start + Duration::from_secs(299),
            None,
        )
        .unwrap();
        append_event_at(
            &path,
            &running("s1", 600),
            start + Duration::from_secs(300),
            None,
        )
        .unwrap();

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().filter(|row| row["sid"] == "s1").count(), 2);
        assert_eq!(rows.iter().filter(|row| row["sid"] == "s2").count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn latest_state_key_uses_the_canonical_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real_dir = tmp.path().join("real");
        let alias_dir = tmp.path().join("alias");
        std::fs::create_dir(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, &alias_dir).unwrap();
        let real_path = real_dir.join("progress.jsonl");
        let alias_path = alias_dir.join("progress.jsonl");
        let event = json!({
            "event": CODEX_THREAD_STATUS,
            "vendor": "codex",
            "thread_id": "thread-1",
            "status": "idle",
            "active_flags": [],
            "ts": "volatile",
        });

        append_event_at(&real_path, &event, Instant::now(), Some(Duration::ZERO)).unwrap();
        append_event_at(&alias_path, &event, Instant::now(), Some(Duration::ZERO)).unwrap();

        assert_eq!(read_rows(&real_path).len(), 1);
    }

    #[test]
    fn unknown_kind_is_persisted_as_a_counted_fact() {
        const UNKNOWN_FIXTURE: &str = "perf_v1_unknown_fixture";
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let before = kind_stats()
            .into_iter()
            .find(|stat| stat.kind == UNKNOWN_FIXTURE)
            .map(|stat| stat.appended_count)
            .unwrap_or_default();

        append_event(
            &path,
            &json!({"event": UNKNOWN_FIXTURE, "payload": "kept", "ts": "volatile"}),
        )
        .unwrap();

        assert_eq!(read_rows(&path).len(), 1);
        let after = kind_stats()
            .into_iter()
            .find(|stat| stat.kind == UNKNOWN_FIXTURE)
            .expect("unknown kind counter");
        assert!(after.unknown);
        assert_eq!(after.appended_count, before + 1);
        assert_eq!(after.suppressed_count, 0);
        assert!(after.appended_bytes > 0);
    }

    #[test]
    fn event_kind_extraction_prefers_event_then_falls_back_to_kind() {
        assert_eq!(
            event_kind_name(&json!({"kind": TYPED_EVENT})),
            Some(TYPED_EVENT)
        );
        assert_eq!(
            event_kind_name(&json!({"event": "legacy", "kind": TYPED_EVENT})),
            Some("legacy")
        );
        assert_eq!(
            event_kind_name(&json!({"event": null, "kind": MERGER_LOSSY_PARTIAL})),
            Some(MERGER_LOSSY_PARTIAL)
        );
    }
}
