//! Minimal progress.jsonl helpers used by harness-owned adapters.
//!
//! `ccteam-core` owns the richer query surface, but harness cannot depend
//! on core without reintroducing a cargo cycle. Keep only the small append
//! and row-builder subset needed by execution adapters here.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
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
const CHECKPOINT_SCHEMA_VERSION: u32 = 4;
const VERDICT_INDEX_SCHEMA_VERSION: u32 = 4;
const SPECIAL_PROJECTION_SCHEMA_VERSION: u32 = 1;
const MAX_BOUNDED_VERDICT_INDEX_BYTES: u64 = 64 * 1024;
const MAX_SPECIAL_RECORD_BYTES: u64 = 64 * 1024;
const RECEIPT_INTEGRITY_BYTES: u64 = 64;
const MAX_BOUNDED_VERDICT_LOOKUP_BYTES: u64 = 8 * 1024 * 1024;
const PROGRESS_LOCK_ACTIVE_MARKER: &[u8; 32] = b"CCTEAM_PROGRESS_LOCK:ACTIVE_:V1\n";
const PROGRESS_LOCK_RETIRED_MARKER: &[u8; 32] = b"CCTEAM_PROGRESS_LOCK:RETIRED:V1\n";

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
    /// Stable identity of the immutable retained generation. This lets hot
    /// readers trust an already-covered archive without hashing it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_identity: Option<PendingActiveFileIdentity>,
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
    /// Legacy v2/v3 verdict payloads. Schema v4 migrates these once into the
    /// append-only verdict projection and always writes this map empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub turn_verdicts: BTreeMap<String, BTreeMap<String, TurnVerdict>>,
    /// Legacy v2/v3 terminal payloads. Schema v4 migrates these once into the
    /// append-only terminal projection and always writes this map empty.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_file_identity: Option<PendingActiveFileIdentity>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        projection: Option<PendingProjectionTarget>,
    },
    TerminalTurn {
        event: Value,
        active_offset: u64,
        line_len: u64,
        line_sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_file_identity: Option<PendingActiveFileIdentity>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        projection: Option<PendingProjectionTarget>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingActiveFileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingProjectionTarget {
    offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_identity: Option<PendingActiveFileIdentity>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ProjectionCoverage {
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    committed_end_offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_identity: Option<PendingActiveFileIdentity>,
}

/// Active-journal bytes already folded into the compact verdict projection.
/// The file identity prevents a same-size rotation/replacement from being
/// mistaken for an unchanged append-only generation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct ActiveVerdictCoverage {
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    checkpoint_rotation_sequence: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    offset: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    corrupt_line_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_identity: Option<PendingActiveFileIdentity>,
}

const fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Small durable projection used by verdict GET/PUT. `progress.jsonl` remains
/// authoritative; `pending` makes the two-file update process-crash-recoverable
/// by verifying one exact bounded line at its recorded active-file offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ProgressVerdictIndex {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    verdicts: BTreeMap<String, BTreeMap<String, TurnVerdict>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    terminal_turns: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingProgressIndexWrite>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    checkpoint_corrupt_line_count: u64,
    #[serde(default, skip_serializing_if = "active_verdict_coverage_is_empty")]
    active: ActiveVerdictCoverage,
    #[serde(default, skip_serializing_if = "active_verdict_coverage_is_empty")]
    archive: ActiveVerdictCoverage,
    #[serde(default, skip_serializing_if = "projection_coverage_is_empty")]
    terminal_projection: ProjectionCoverage,
    #[serde(default, skip_serializing_if = "projection_coverage_is_empty")]
    verdict_projection: ProjectionCoverage,
}

#[derive(Deserialize)]
struct ProgressVerdictIndexVersion {
    schema_version: u32,
}

fn active_verdict_coverage_is_empty(coverage: &ActiveVerdictCoverage) -> bool {
    coverage == &ActiveVerdictCoverage::default()
}

fn active_verdict_coverage_has_raw_state(coverage: &ActiveVerdictCoverage) -> bool {
    coverage.offset != 0 || coverage.corrupt_line_count != 0 || coverage.file_identity.is_some()
}

fn projection_coverage_is_empty(coverage: &ProjectionCoverage) -> bool {
    coverage == &ProjectionCoverage::default()
}

impl Default for ProgressVerdictIndex {
    fn default() -> Self {
        Self {
            schema_version: VERDICT_INDEX_SCHEMA_VERSION,
            verdicts: BTreeMap::new(),
            terminal_turns: BTreeMap::new(),
            pending: None,
            checkpoint_corrupt_line_count: 0,
            active: ActiveVerdictCoverage::default(),
            archive: ActiveVerdictCoverage::default(),
            terminal_projection: ProjectionCoverage::default(),
            verdict_projection: ProjectionCoverage::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TerminalProjectionRecord {
    schema_version: u32,
    source_id: String,
    event: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct VerdictProjectionRecord {
    schema_version: u32,
    source_id: String,
    verdict: TurnVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProjectionRecordLocation {
    offset: u64,
    line_len: u64,
    line_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TerminalReceipt {
    schema_version: u32,
    sid: String,
    turn_id: String,
    source_id: String,
    projection: ProjectionRecordLocation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct VerdictReceipt {
    schema_version: u32,
    sid: String,
    turn_id: String,
    source_id: String,
    projection: ProjectionRecordLocation,
    verdict: TurnVerdict,
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

/// Resolve the append-only lifetime terminal projection. It retains the first
/// canonical payload while the fixed verdict index remains a bounded fence.
pub fn progress_terminal_projection_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".terminals.jsonl")
}

/// Resolve the append-only lifetime human-verdict projection.
pub fn progress_verdict_projection_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".turn-verdicts.jsonl")
}

fn terminal_receipt_root(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".terminal-keys")
}

fn verdict_receipt_root(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".verdict-keys")
}

fn durable_tmp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.tmp"))
}

fn corrupt_verdict_index_path(active_path: &Path) -> PathBuf {
    progress_sibling_path(active_path, ".verdicts.corrupt.json")
}

/// Fixed canonical progress-state surfaces except the stable lock inode.
/// Cleanup extends these with generated repair artifacts discovered beside
/// the active journal and archive.
fn progress_state_manifest(active_path: &Path) -> Vec<PathBuf> {
    let checkpoint = progress_checkpoint_path(active_path);
    let verdict_index = progress_verdict_index_path(active_path);
    let corrupt_verdict_index = corrupt_verdict_index_path(active_path);
    vec![
        active_path.to_path_buf(),
        progress_archive_path(active_path),
        checkpoint.clone(),
        durable_tmp_path(&checkpoint),
        verdict_index.clone(),
        durable_tmp_path(&verdict_index),
        corrupt_verdict_index.clone(),
        durable_tmp_path(&corrupt_verdict_index),
        progress_terminal_projection_path(active_path),
        progress_verdict_projection_path(active_path),
        terminal_receipt_root(active_path),
        verdict_receipt_root(active_path),
    ]
}

fn progress_cleanup_state_paths(active_path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = progress_state_manifest(active_path);
    let mut maintenance = Vec::new();
    for target in [
        active_path.to_path_buf(),
        progress_archive_path(active_path),
    ] {
        maintenance.extend(progress_maintenance_artifacts(&target)?);
    }
    maintenance.sort();
    maintenance.dedup();
    paths.extend(maintenance);
    Ok(paths)
}

fn progress_maintenance_artifacts(target_path: &Path) -> Result<Vec<PathBuf>> {
    let parent = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_metadata = match std::fs::symlink_metadata(parent) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_progress_state_absence_ancestors(parent)?;
            return Ok(Vec::new());
        }
        Err(error) => return Err(error).with_context(|| format!("stat {}", parent.display())),
    };
    if parent_metadata.file_type().is_symlink() {
        let target = std::fs::metadata(parent)
            .with_context(|| format!("resolve progress state parent {}", parent.display()))?;
        if !target.is_dir() {
            anyhow::bail!(
                "progress state parent does not resolve to a directory: {}",
                parent.display()
            );
        }
    } else if !parent_metadata.is_dir() {
        anyhow::bail!(
            "progress state parent is not a directory: {}",
            parent.display()
        );
    }
    let target_name = target_path
        .file_name()
        .context("progress maintenance target is missing its file name")?;
    let mut repair_prefix = target_name.as_encoded_bytes().to_vec();
    repair_prefix.extend_from_slice(b".repair-tmp-");
    let mut backup_prefix = target_name.as_encoded_bytes().to_vec();
    backup_prefix.extend_from_slice(b".bak-");
    let entries =
        std::fs::read_dir(parent).with_context(|| format!("read {}", parent.display()))?;
    let mut artifacts = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry in {}", parent.display()))?;
        let name = entry.file_name();
        let bytes = name.as_encoded_bytes();
        if bytes.starts_with(&repair_prefix) || bytes.starts_with(&backup_prefix) {
            artifacts.push(entry.path());
        }
    }
    artifacts.sort();
    Ok(artifacts)
}

/// Conservatively detect every durable progress-state surface without
/// scanning sharded receipt contents. Missing means truly absent; permission
/// and other metadata failures must not collapse unknown state to clean empty.
fn progress_state_entry(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_progress_state_absence_ancestors(path)?;
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
}

fn validate_progress_state_absence_ancestors(path: &Path) -> Result<()> {
    let mut ancestor = path.parent();
    while let Some(candidate) = ancestor {
        match std::fs::symlink_metadata(candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    let target = std::fs::metadata(candidate).with_context(|| {
                        format!("resolve progress state ancestor {}", candidate.display())
                    })?;
                    if !target.is_dir() {
                        anyhow::bail!(
                            "progress state ancestor does not resolve to a directory: {}",
                            candidate.display()
                        );
                    }
                } else if !metadata.is_dir() {
                    anyhow::bail!(
                        "progress state ancestor is not a directory: {}",
                        candidate.display()
                    );
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = candidate.parent();
            }
            Err(error) => {
                return Err(error).with_context(|| format!("stat {}", candidate.display()));
            }
        }
    }
    Ok(())
}

fn progress_state_path_is_absent(path: &Path) -> Result<bool> {
    Ok(progress_state_entry(path)?.is_none())
}

/// Run one optional state access without letting a dangling symlink, a
/// permission failure, or a post-lstat disappearance masquerade as absence.
/// Only lexical `ENOENT` from `symlink_metadata` is authoritative absence.
fn access_optional_progress_state<T>(
    path: &Path,
    action: &str,
    access: impl FnOnce(&Path) -> std::io::Result<T>,
) -> Result<Option<T>> {
    if progress_state_entry(path)?.is_none() {
        return Ok(None);
    }
    access(path)
        .map(Some)
        .with_context(|| format!("{action} {}", path.display()))
}

fn optional_receipt_directory(path: &Path, label: &str) -> Result<bool> {
    let Some(metadata) = progress_state_entry(path)? else {
        return Ok(false);
    };
    if !metadata.file_type().is_dir() {
        anyhow::bail!("{label} is not a regular directory: {}", path.display());
    }
    Ok(true)
}

fn receipt_path_is_absent(root: &Path, path: &Path) -> Result<bool> {
    if !optional_receipt_directory(root, "receipt root")? {
        return Ok(true);
    }
    let shard = path
        .parent()
        .context("receipt path is missing its shard directory")?;
    if !optional_receipt_directory(shard, "receipt shard")? {
        return Ok(true);
    }
    let Some(metadata) = progress_state_entry(path)? else {
        return Ok(true);
    };
    if !metadata.file_type().is_file() {
        anyhow::bail!("receipt is not a regular file: {}", path.display());
    }
    Ok(false)
}

fn progress_state_exists(active_path: &Path) -> Result<bool> {
    for path in progress_state_manifest(active_path) {
        if !progress_state_path_is_absent(&path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Mark one progress generation retired without deleting its state.
///
/// Retirement is durably published into the existing stable lock inode while
/// holding its flock. A writer that opened that inode before retirement but
/// acquires the flock afterwards therefore observes the tombstone and fails
/// before recreating state. Callers may release the flock, join live producers,
/// and only then invoke [`cleanup_retired_progress_state`].
pub fn mark_progress_retired(active_path: &Path) -> Result<()> {
    ensure_progress_lock_parent(active_path)?;
    let lock_file = open_progress_lock(active_path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    write_progress_lock_marker_locked(&lock_file, active_path, PROGRESS_LOCK_RETIRED_MARKER)
}

/// Return whether the stable progress generation has been retired.
///
/// Missing locks and legacy empty locks are active/non-retired. This read does
/// not create a parent or lock file. Unknown or torn marker bytes fail closed.
pub fn progress_state_is_retired(active_path: &Path) -> Result<bool> {
    let Some(lock_file) = open_existing_progress_lock(active_path, false)? else {
        return Ok(false);
    };
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    match read_progress_lock_state_locked(&lock_file, active_path)? {
        ProgressLockState::LegacyEmpty | ProgressLockState::Active => Ok(false),
        ProgressLockState::Retired => Ok(true),
    }
}

/// Read-only sibling of [`progress_state_is_retired`] that takes the stable
/// lock in shared mode, so concurrent readers do not serialize against each
/// other. Writer semantics are unchanged: this still blocks behind an
/// exclusive holder, so it is NOT safe on hot async paths — callers on those
/// paths must consult a cached fence instead.
pub fn progress_state_is_retired_shared(active_path: &Path) -> Result<bool> {
    let Some(lock_file) = open_existing_progress_lock(active_path, false)? else {
        return Ok(false);
    };
    let _lock = ProgressFileLock::lock_shared(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    match read_progress_lock_state_locked(&lock_file, active_path)? {
        ProgressLockState::LegacyEmpty | ProgressLockState::Active => Ok(false),
        ProgressLockState::Retired => Ok(true),
    }
}

/// Bounded variant of [`progress_state_is_retired_shared`] for callers that
/// must never park a thread behind an exclusive writer (a rotation, a
/// repair). The shared lock is acquired non-blocking in a short retry loop
/// until `deadline`; `Ok(None)` means the verdict could not be read in time
/// and the caller keeps whatever fail-closed state it already holds. Real
/// I/O errors and torn markers are still `Err`.
pub fn progress_state_is_retired_shared_try(
    active_path: &Path,
    deadline: std::time::Instant,
) -> Result<Option<bool>> {
    const RETRY_INTERVAL: Duration = Duration::from_millis(10);
    let Some(lock_file) = open_existing_progress_lock(active_path, false)? else {
        return Ok(Some(false));
    };
    let lock_path = progress_lock_path(active_path);
    loop {
        if let Some(_lock) = ProgressFileLock::try_lock_shared(&lock_file)
            .with_context(|| format!("lock {}", lock_path.display()))?
        {
            return match read_progress_lock_state_locked(&lock_file, active_path)? {
                ProgressLockState::LegacyEmpty | ProgressLockState::Active => Ok(Some(false)),
                ProgressLockState::Retired => Ok(Some(true)),
            };
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        std::thread::sleep(RETRY_INTERVAL.min(deadline - now));
    }
}

/// Why a slug cannot host a fresh progress generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressSlugReservation {
    /// Nothing owns the slug: no lock inode at all, or a bare legacy/active
    /// lock whose generation left no journal, checkpoint, index or receipts.
    Free,
    /// A durable retired tombstone owns the slug permanently.
    Retired,
    /// A non-retired generation still owns durable state under this slug.
    ActiveState,
}

/// Classify who owns a slug's progress generation.
///
/// A bare lock inode is *not* an owner: locks are created eagerly by readers,
/// repair sweeps and probes, so pre-existing empty locks must not reserve
/// slugs. Ownership is the retired tombstone, or surviving durable state. The
/// check does not create or mutate state; unsafe lock types and metadata/open
/// failures remain errors.
pub fn progress_slug_reservation(active_path: &Path) -> Result<ProgressSlugReservation> {
    let state_reservation = |active_path: &Path| -> Result<ProgressSlugReservation> {
        if progress_state_exists(active_path)? {
            Ok(ProgressSlugReservation::ActiveState)
        } else {
            Ok(ProgressSlugReservation::Free)
        }
    };
    let Some(lock_file) = open_existing_progress_lock(active_path, false)? else {
        return state_reservation(active_path);
    };
    let _lock = ProgressFileLock::lock_shared(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    match read_progress_lock_state_locked(&lock_file, active_path)? {
        ProgressLockState::Retired => Ok(ProgressSlugReservation::Retired),
        ProgressLockState::LegacyEmpty | ProgressLockState::Active => {
            state_reservation(active_path)
        }
    }
}

/// Return whether a slug is owned by a retired tombstone or by surviving
/// progress state. See [`progress_slug_reservation`] for the distinction.
pub fn progress_slug_is_reserved(active_path: &Path) -> Result<bool> {
    Ok(progress_slug_reservation(active_path)? != ProgressSlugReservation::Free)
}

/// Enumerate or delete every durable state surface owned by a retired progress
/// generation. The stable tombstone inode is intentionally retained and is not
/// included in the returned paths. Both dry-run and deletion require a durable
/// retired marker; dry-run never creates or rewrites anything.
pub fn cleanup_retired_progress_state(active_path: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let existing = || -> Result<Vec<PathBuf>> {
        progress_cleanup_state_paths(active_path)?
            .into_iter()
            .filter_map(|path| match progress_state_entry(&path) {
                Ok(Some(_)) => Some(Ok(path)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    };

    let Some(lock_file) = open_existing_progress_lock(active_path, !dry_run)? else {
        let paths = existing()?;
        if paths.is_empty() {
            return Ok(paths);
        }
        anyhow::bail!(
            "progress state is not retired: missing lock {}",
            progress_lock_path(active_path).display()
        );
    };
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    require_retired_progress_lock_locked(&lock_file, active_path)?;

    if dry_run {
        return existing();
    }

    let mut removed = Vec::new();
    for path in progress_cleanup_state_paths(active_path)? {
        let Some(metadata) = progress_state_entry(&path)? else {
            continue;
        };
        if metadata.file_type().is_dir() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("remove progress state tree {}", path.display()))?;
        } else {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove progress state {}", path.display()))?;
        }
        removed.push(path);
    }
    Ok(removed)
}

/// Retire and then clean one progress generation for callers that have already
/// stopped its producers. Removal flows that must join producers use the two
/// explicit phases instead.
///
/// Symlinks are unlinked at the owned path and never followed. Directories are
/// removed recursively; `std::fs::remove_dir_all` likewise does not follow
/// symlinks found inside them. The returned paths are the owned entries that
/// existed, in stable order, for CLI reporting.
pub fn cleanup_progress_state(active_path: &Path, dry_run: bool) -> Result<Vec<PathBuf>> {
    let existing = || -> Result<Vec<PathBuf>> {
        progress_cleanup_state_paths(active_path)?
            .into_iter()
            .filter_map(|path| match progress_state_entry(&path) {
                Ok(Some(_)) => Some(Ok(path)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    };
    if dry_run {
        if let Some(lock_file) = open_existing_progress_lock(active_path, false)? {
            let _lock = ProgressFileLock::lock(&lock_file)
                .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
            read_progress_lock_state_locked(&lock_file, active_path)?;
            return existing();
        }
        return existing();
    }

    mark_progress_retired(active_path)?;
    cleanup_retired_progress_state(active_path, false)
}

fn receipt_root_has_committed_receipt(root: &Path) -> Result<bool> {
    if !optional_receipt_directory(root, "receipt root")? {
        return Ok(false);
    }
    let shards = std::fs::read_dir(root).with_context(|| format!("read {}", root.display()))?;
    for shard in shards {
        let shard = shard.with_context(|| format!("read entry in {}", root.display()))?;
        let file_type = shard
            .file_type()
            .with_context(|| format!("stat {}", shard.path().display()))?;
        if !file_type.is_dir() {
            anyhow::bail!(
                "receipt shard is not a regular directory: {}",
                shard.path().display()
            );
        }
        let mut receipts = std::fs::read_dir(shard.path())
            .with_context(|| format!("read {}", shard.path().display()))?;
        if let Some(receipt) = receipts.next() {
            let receipt =
                receipt.with_context(|| format!("read entry in {}", shard.path().display()))?;
            let file_type = receipt
                .file_type()
                .with_context(|| format!("stat {}", receipt.path().display()))?;
            if file_type.is_file() {
                return Ok(true);
            }
            anyhow::bail!(
                "receipt is not a regular file: {}",
                receipt.path().display()
            );
        }
    }
    Ok(false)
}

fn special_receipt_path(root: &Path, sid: &str, turn_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(sid.as_bytes());
    hasher.update([0]);
    hasher.update(turn_id.as_bytes());
    let digest = hex_digest(hasher.finalize().as_slice());
    root.join(&digest[..2]).join(format!("{digest}.json"))
}

fn receipt_integrity_path(receipt_path: &Path) -> PathBuf {
    let file_name = receipt_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    receipt_path.with_file_name(format!("{file_name}.sha256"))
}

#[derive(Clone, Copy)]
enum ReceiptIntegrityPolicy {
    Strict,
    CanonicalRepair,
}

fn read_receipt_pair_bytes(
    root: &Path,
    path: &Path,
    label: &str,
    policy: ReceiptIntegrityPolicy,
) -> Result<Option<Vec<u8>>> {
    let integrity_path = receipt_integrity_path(path);
    let receipt_absent = receipt_path_is_absent(root, path)?;
    let integrity_absent = receipt_path_is_absent(root, &integrity_path)?;
    match (receipt_absent, integrity_absent) {
        (true, true) => return Ok(None),
        (true, false) | (false, true) => {
            if matches!(policy, ReceiptIntegrityPolicy::CanonicalRepair) {
                return Ok(None);
            }
            anyhow::bail!("{label} integrity pair is incomplete in {}", path.display());
        }
        (false, false) => {}
    }

    let bytes = read_bounded_state_bytes(path, MAX_SPECIAL_RECORD_BYTES, label)?
        .context("receipt disappeared during integrity validation")?;
    let integrity = read_bounded_state_bytes(
        &integrity_path,
        RECEIPT_INTEGRITY_BYTES,
        "receipt integrity marker",
    )?
    .context("receipt integrity marker disappeared during validation")?;
    let expected = hex_digest(Sha256::digest(&bytes).as_slice());
    if integrity != expected.as_bytes() {
        if matches!(policy, ReceiptIntegrityPolicy::CanonicalRepair) {
            return Ok(None);
        }
        anyhow::bail!("{label} integrity mismatch in {}", path.display());
    }
    Ok(Some(bytes))
}

fn bounded_receipt_pair_len(root: &Path, path: &Path, label: &str) -> Result<Option<u64>> {
    let integrity_path = receipt_integrity_path(path);
    let receipt_absent = receipt_path_is_absent(root, path)?;
    let integrity_absent = receipt_path_is_absent(root, &integrity_path)?;
    match (receipt_absent, integrity_absent) {
        (true, true) => return Ok(None),
        (true, false) | (false, true) => {
            anyhow::bail!("{label} integrity pair is incomplete in {}", path.display());
        }
        (false, false) => {}
    }
    let receipt_len = bounded_state_file_len(path, MAX_SPECIAL_RECORD_BYTES, label)?
        .context("receipt disappeared during bounded lookup")?;
    let integrity_len = bounded_state_file_len(
        &integrity_path,
        RECEIPT_INTEGRITY_BYTES,
        "receipt integrity marker",
    )?
    .context("receipt integrity marker disappeared during bounded lookup")?;
    Ok(Some(receipt_len.saturating_add(integrity_len)))
}

fn read_terminal_receipt(
    active_path: &Path,
    sid: &str,
    turn_id: &str,
) -> Result<Option<TerminalReceipt>> {
    read_terminal_receipt_with_policy(active_path, sid, turn_id, ReceiptIntegrityPolicy::Strict)
}

fn read_terminal_receipt_with_policy(
    active_path: &Path,
    sid: &str,
    turn_id: &str,
    policy: ReceiptIntegrityPolicy,
) -> Result<Option<TerminalReceipt>> {
    let root = terminal_receipt_root(active_path);
    let path = special_receipt_path(&root, sid, turn_id);
    let Some(bytes) = read_receipt_pair_bytes(&root, &path, "terminal receipt", policy)? else {
        return Ok(None);
    };
    let receipt = serde_json::from_slice::<TerminalReceipt>(&bytes)
        .with_context(|| format!("parse {}", path.display()))?;
    if receipt.schema_version != SPECIAL_PROJECTION_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported terminal receipt schema {} in {}",
            receipt.schema_version,
            path.display()
        );
    }
    if receipt.sid != sid || receipt.turn_id != turn_id {
        anyhow::bail!("terminal receipt hash collision in {}", path.display());
    }
    Ok(Some(receipt))
}

fn read_verdict_receipt(
    active_path: &Path,
    sid: &str,
    turn_id: &str,
) -> Result<Option<VerdictReceipt>> {
    read_verdict_receipt_with_policy(active_path, sid, turn_id, ReceiptIntegrityPolicy::Strict)
}

fn read_verdict_receipt_with_policy(
    active_path: &Path,
    sid: &str,
    turn_id: &str,
    policy: ReceiptIntegrityPolicy,
) -> Result<Option<VerdictReceipt>> {
    let root = verdict_receipt_root(active_path);
    let path = special_receipt_path(&root, sid, turn_id);
    let Some(bytes) = read_receipt_pair_bytes(&root, &path, "verdict receipt", policy)? else {
        return Ok(None);
    };
    let receipt = serde_json::from_slice::<VerdictReceipt>(&bytes)
        .with_context(|| format!("parse {}", path.display()))?;
    if receipt.schema_version != SPECIAL_PROJECTION_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported verdict receipt schema {} in {}",
            receipt.schema_version,
            path.display()
        );
    }
    if receipt.sid != sid || receipt.turn_id != turn_id {
        anyhow::bail!("verdict receipt hash collision in {}", path.display());
    }
    if receipt.verdict.sid != receipt.sid || receipt.verdict.turn_id != receipt.turn_id {
        anyhow::bail!("verdict receipt payload key mismatch in {}", path.display());
    }
    Ok(Some(receipt))
}

fn read_bounded_state_bytes(path: &Path, max_bytes: u64, label: &str) -> Result<Option<Vec<u8>>> {
    let Some(file) = access_optional_progress_state(path, "open", |path| File::open(path))? else {
        return Ok(None);
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    if metadata.len() > max_bytes {
        anyhow::bail!(
            "{label} exceeds bounded lookup limit: {} bytes",
            metadata.len()
        );
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        anyhow::bail!("{label} grew beyond bounded lookup limit");
    }
    journal::record_raw_read(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    Ok(Some(bytes))
}

#[cfg(test)]
fn write_terminal_receipt(active_path: &Path, receipt: &TerminalReceipt) -> Result<()> {
    let bytes = serialize_bounded_receipt(receipt, "terminal receipt")?;
    let path = special_receipt_path(
        &terminal_receipt_root(active_path),
        &receipt.sid,
        &receipt.turn_id,
    );
    persist_receipt_bytes(&path, &bytes)
}

#[cfg(test)]
fn write_verdict_receipt(active_path: &Path, receipt: &VerdictReceipt) -> Result<()> {
    let bytes = serialize_bounded_receipt(receipt, "verdict receipt")?;
    let path = special_receipt_path(
        &verdict_receipt_root(active_path),
        &receipt.sid,
        &receipt.turn_id,
    );
    persist_receipt_bytes(&path, &bytes)
}

fn serialize_bounded_receipt<T: Serialize>(receipt: &T, label: &str) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(receipt).with_context(|| format!("serialize {label}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!("{label} exceeds storage limit");
    }
    Ok(bytes)
}

fn persist_receipt_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let shard = path.parent().context("receipt path is missing its shard")?;
    let root = shard
        .parent()
        .context("receipt shard is missing its root")?;
    let integrity_path = receipt_integrity_path(path);
    let _ = receipt_path_is_absent(root, path)?;
    let _ = receipt_path_is_absent(root, &integrity_path)?;
    std::fs::create_dir_all(shard).with_context(|| format!("create {}", shard.display()))?;
    atomic_write_durable(path, bytes)?;
    let integrity = hex_digest(Sha256::digest(bytes).as_slice());
    atomic_write_durable(&integrity_path, integrity.as_bytes())
}

/// Read a progress checkpoint without mutating or recovering it.
pub fn read_progress_checkpoint(active_path: &Path) -> Result<Option<ProgressCheckpoint>> {
    let path = progress_checkpoint_path(active_path);
    let Some(bytes) = access_optional_progress_state(&path, "read", |path| std::fs::read(path))?
    else {
        return Ok(None);
    };
    journal::record_raw_read(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    let checkpoint = serde_json::from_slice::<ProgressCheckpoint>(&bytes)
        .with_context(|| format!("parse {}", path.display()))?;
    if !matches!(
        checkpoint.schema_version,
        1 | 2 | 3 | CHECKPOINT_SCHEMA_VERSION
    ) {
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
    if !progress_state_exists(active_path)? {
        if progress_state_is_retired(active_path)? {
            anyhow::bail!("progress state is retired: {}", active_path.display());
        }
        return Ok(None);
    }
    if let Some(parent) = active_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = open_progress_lock(active_path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(active_path).display()))?;
    require_active_progress_lock_locked(&lock_file, active_path)?;
    let checkpoint = recover_progress_checkpoint_locked(active_path)?;
    ensure_verdict_index_locked(active_path, checkpoint.as_ref())?;
    read_progress_checkpoint(active_path)
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
    if terminal_turn_identity(event).is_some() {
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
    validate_terminal_special_record_size(event, sid, turn_id)?;
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
    require_active_progress_lock_locked(&lock_file, path)?;
    let checkpoint = recover_progress_checkpoint_locked(path)?;
    let mut index = reconcile_verdict_index_locked(path, checkpoint.as_ref())?;
    if let Some(receipt) = read_terminal_receipt(path, &sid, &turn_id)? {
        let first = terminal_event_from_receipt(path, &receipt)?;
        drop(lock);
        record_suppressed(CHAT_TURN_COMPLETED, false, byte_count);
        return Ok(CanonicalTerminalAppend {
            appended: false,
            event: first,
        });
    }

    let current_size =
        access_optional_progress_state(path, "stat", |path| std::fs::metadata(path))?
            .map(|metadata| metadata.len())
            .unwrap_or(0);
    let mut rotated = false;
    if current_size > 0 && current_size.saturating_add(byte_count) > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
        rotated = true;
        let checkpoint = recover_progress_checkpoint_locked(path)?;
        index = reconcile_verdict_index_locked(path, checkpoint.as_ref())?;
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
    let active_file_identity = pending_active_file_identity(&active_metadata);
    let projection = projection_append_target(&progress_terminal_projection_path(path))?;
    index.pending = Some(PendingProgressIndexWrite::TerminalTurn {
        event: event.clone(),
        active_offset,
        line_len: byte_count,
        line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
        active_file_identity: active_file_identity.clone(),
        projection: Some(projection.clone()),
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

    let line_sha256 = hex_digest(Sha256::digest(&line).as_slice());
    let source_id =
        projection_source_id(active_file_identity.as_ref(), active_offset, &line_sha256);
    let receipt = apply_terminal_projection_locked(path, event, &source_id, Some(&projection))?;
    advance_projection_coverage(
        &progress_terminal_projection_path(path),
        &mut index.terminal_projection,
        &receipt.projection,
        "terminal",
    )?;
    validate_projection_coverage_exact(
        &progress_terminal_projection_path(path),
        &index.terminal_projection,
        "terminal",
    )?;
    index.pending = None;
    index.active.offset = size;
    index.active.file_identity = active_file_identity;
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

fn validate_terminal_special_record_size(event: &Value, sid: &str, turn_id: &str) -> Result<()> {
    let source_id = "x".repeat(256);
    let projection = terminal_projection_line(&source_id, event)?;
    let projection_len = u64::try_from(projection.len()).unwrap_or(u64::MAX);
    if projection_len > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!("terminal projection record exceeds storage limit");
    }
    let receipt = TerminalReceipt {
        schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
        sid: sid.to_string(),
        turn_id: turn_id.to_string(),
        source_id,
        projection: ProjectionRecordLocation {
            offset: u64::MAX,
            line_len: projection_len,
            line_sha256: "f".repeat(64),
        },
    };
    if u64::try_from(
        serde_json::to_vec(&receipt)
            .context("serialize bounded terminal receipt")?
            .len(),
    )
    .unwrap_or(u64::MAX)
        > MAX_SPECIAL_RECORD_BYTES
    {
        anyhow::bail!("terminal receipt exceeds storage limit");
    }
    Ok(())
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
    if !progress_state_exists(path)? {
        if progress_state_is_retired(path)? {
            anyhow::bail!("progress state is retired: {}", path.display());
        }
        return Ok(TurnVerdictRead {
            verdicts: BTreeMap::new(),
            corrupt_line_count: 0,
        });
    }
    let lock_file = open_progress_lock(path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;
    require_active_progress_lock_locked(&lock_file, path)?;
    let checkpoint = recover_progress_checkpoint_locked(path)?;
    let index = reconcile_verdict_index_locked(path, checkpoint.as_ref())?;
    validate_projection_coverage_exact(
        &progress_verdict_projection_path(path),
        &index.verdict_projection,
        "verdict",
    )?;
    Ok(TurnVerdictRead {
        verdicts: scan_verdict_projection(path)?,
        corrupt_line_count: checkpoint
            .map(|checkpoint| checkpoint.corrupt_line_count)
            .unwrap_or(0)
            .saturating_add(index.active.corrupt_line_count),
    })
}

/// Read verdicts only for one session and one already-paginated set of turn
/// ids. The filesystem's sharded receipt index is the durable O(page) lookup;
/// the append-only projection is touched only at the exact receipt offsets.
pub fn latest_turn_verdicts_for_turns_detailed(
    path: &Path,
    sid: &str,
    turn_ids: &BTreeSet<String>,
) -> Result<TurnVerdictRead> {
    if !progress_state_exists(path)? {
        if progress_state_is_retired(path)? {
            anyhow::bail!("progress state is retired: {}", path.display());
        }
        return Ok(TurnVerdictRead {
            verdicts: BTreeMap::new(),
            corrupt_line_count: 0,
        });
    }
    let lock_file = open_progress_lock(path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;
    require_active_progress_lock_locked(&lock_file, path)?;
    let mut bounded_bytes = bounded_state_file_len(
        &progress_verdict_index_path(path),
        MAX_BOUNDED_VERDICT_INDEX_BYTES,
        "progress verdict index",
    )?
    .unwrap_or(0);
    let index = read_verdict_index_locked(path)?
        .context("bounded verdict lookup requires a hydrated progress verdict index")?;
    validate_bounded_verdict_lookup_state(path, &index)?;
    let mut verdicts = BTreeMap::new();
    let receipt_root = verdict_receipt_root(path);
    for turn_id in turn_ids {
        let receipt_path = special_receipt_path(&receipt_root, sid, turn_id);
        let Some(receipt_bytes) =
            bounded_receipt_pair_len(&receipt_root, &receipt_path, "verdict receipt")?
        else {
            continue;
        };
        bounded_bytes = reserve_bounded_verdict_lookup(bounded_bytes, receipt_bytes)?;
        let receipt = read_verdict_receipt(path, sid, turn_id)?
            .context("verdict receipt disappeared during bounded lookup")?;
        if receipt.projection.line_len > MAX_SPECIAL_RECORD_BYTES {
            anyhow::bail!("verdict projection record exceeds bounded lookup limit");
        }
        bounded_bytes = reserve_bounded_verdict_lookup(bounded_bytes, receipt.projection.line_len)?;
        verify_verdict_receipt_projection(path, &receipt)?;
        verdicts.insert((sid.to_string(), turn_id.clone()), receipt.verdict);
    }
    Ok(TurnVerdictRead {
        verdicts,
        corrupt_line_count: index
            .checkpoint_corrupt_line_count
            .saturating_add(index.active.corrupt_line_count),
    })
}

fn bounded_state_file_len(path: &Path, max_bytes: u64, label: &str) -> Result<Option<u64>> {
    let Some(metadata) =
        access_optional_progress_state(path, "stat", |path| std::fs::metadata(path))?
    else {
        return Ok(None);
    };
    if metadata.len() <= max_bytes {
        Ok(Some(metadata.len()))
    } else {
        anyhow::bail!(
            "{label} exceeds bounded lookup limit: {} bytes",
            metadata.len()
        )
    }
}

fn reserve_bounded_verdict_lookup(consumed: u64, additional: u64) -> Result<u64> {
    let total = consumed.saturating_add(additional);
    if total > MAX_BOUNDED_VERDICT_LOOKUP_BYTES {
        anyhow::bail!("verdict page exceeds bounded lookup byte budget");
    }
    Ok(total)
}

/// Prove that a request can trust the small derived state without scanning a
/// journal generation. Startup hydration, rotation, and special writers own
/// catch-up; a stale request fails closed so one history page never becomes a
/// project-global progress scan.
fn validate_bounded_verdict_lookup_state(path: &Path, index: &ProgressVerdictIndex) -> Result<()> {
    if index.schema_version != VERDICT_INDEX_SCHEMA_VERSION
        || !index.verdicts.is_empty()
        || !index.terminal_turns.is_empty()
        || index.pending.is_some()
    {
        anyhow::bail!("progress verdict index requires offline hydration");
    }
    if index.active.checkpoint_rotation_sequence != index.archive.checkpoint_rotation_sequence {
        anyhow::bail!("progress verdict index rotation is stale");
    }

    let archive_path = progress_archive_path(path);
    if !active_progress_cursor_is_current(path, &index.active)?
        || !progress_cursor_is_exact(&archive_path, &index.archive)?
    {
        anyhow::bail!("progress verdict index generation is stale");
    }
    validate_projection_coverage_exact(
        &progress_verdict_projection_path(path),
        &index.verdict_projection,
        "verdict",
    )?;
    Ok(())
}

fn progress_cursor_is_exact(path: &Path, cursor: &ActiveVerdictCoverage) -> Result<bool> {
    let Some(metadata) =
        access_optional_progress_state(path, "stat", |path| std::fs::metadata(path))?
    else {
        return Ok(cursor.offset == 0
            && cursor.corrupt_line_count == 0
            && cursor.file_identity.is_none());
    };
    let file_identity = pending_active_file_identity(&metadata);
    Ok(file_identity.is_some()
        && cursor.file_identity == file_identity
        && cursor.offset == metadata.len())
}

fn active_progress_cursor_is_current(path: &Path, cursor: &ActiveVerdictCoverage) -> Result<bool> {
    let Some(mut file) = access_optional_progress_state(path, "open", |path| File::open(path))?
    else {
        return Ok(cursor.offset == 0
            && cursor.corrupt_line_count == 0
            && cursor.file_identity.is_none());
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let file_identity = pending_active_file_identity(&metadata);
    if file_identity.is_none()
        || cursor.file_identity != file_identity
        || cursor.offset > metadata.len()
    {
        return Ok(false);
    }
    if metadata.len() == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::End(-1))
        .with_context(|| format!("seek progress tail in {}", path.display()))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)
        .with_context(|| format!("read progress tail from {}", path.display()))?;
    journal::record_raw_read(1);
    Ok(last[0] == b'\n')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionCoverageRelation {
    Exact,
    AppendOnlyExtension,
    UninitializedNonEmpty,
    Invalid,
}

struct UntrustedProjectionPrefix {
    path: PathBuf,
    file: Option<File>,
    len: u64,
    verified_end: u64,
}

impl UntrustedProjectionPrefix {
    fn open(path: PathBuf) -> Result<Self> {
        let file = access_optional_progress_state(&path, "open", |path| File::open(path))?;
        let len = match &file {
            Some(file) => file
                .metadata()
                .with_context(|| format!("stat {}", path.display()))?
                .len(),
            None => 0,
        };
        Ok(Self {
            path,
            file,
            len,
            verified_end: 0,
        })
    }

    fn is_nonempty(&self) -> bool {
        self.len > 0
    }

    fn consume_if_present(&mut self, expected: &[u8], label: &str) -> Result<()> {
        if self.verified_end == self.len {
            return Ok(());
        }
        let expected_len = u64::try_from(expected.len()).context("projection record too large")?;
        let read_len = self.len.saturating_sub(self.verified_end).min(expected_len);
        let read_len = usize::try_from(read_len).context("projection prefix too large")?;
        let file = self
            .file
            .as_mut()
            .context("non-empty bootstrap projection is not open")?;
        file.seek(SeekFrom::Start(self.verified_end))
            .with_context(|| format!("seek {}", self.path.display()))?;
        let mut actual = vec![0_u8; read_len];
        file.read_exact(&mut actual)
            .with_context(|| format!("read {}", self.path.display()))?;
        journal::record_raw_read(u64::try_from(read_len).unwrap_or(u64::MAX));
        if actual != expected[..read_len] {
            anyhow::bail!(
                "{label} bootstrap projection prefix mismatch in {}",
                self.path.display()
            );
        }
        self.verified_end = self
            .verified_end
            .saturating_add(u64::try_from(read_len).unwrap_or(u64::MAX));
        Ok(())
    }

    fn validate_exhausted(&self, label: &str) -> Result<()> {
        if self.verified_end != self.len {
            anyhow::bail!(
                "{label} bootstrap projection contains bytes not proven by retained progress in {}",
                self.path.display()
            );
        }
        Ok(())
    }
}

fn validate_bootstrap_projection_generation(
    source_path: &Path,
    terminal: &mut UntrustedProjectionPrefix,
    verdict: &mut UntrustedProjectionPrefix,
    terminal_keys: &mut BTreeSet<(String, String)>,
    latest_verdicts: &mut BTreeMap<(String, String), TurnVerdict>,
) -> Result<()> {
    let Some(file) = access_optional_progress_state(source_path, "open", |path| File::open(path))?
    else {
        return Ok(());
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", source_path.display()))?;
    let file_identity = pending_active_file_identity(&metadata);
    let mut reader = BufReader::new(file);
    let mut offset = 0_u64;
    loop {
        let row_offset = offset;
        let mut line = Vec::new();
        let read = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("read {}", source_path.display()))?;
        if read == 0 {
            break;
        }
        let read = u64::try_from(read).unwrap_or(u64::MAX);
        offset = offset.saturating_add(read);
        if line.last() != Some(&b'\n') {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let event = match serde_json::from_slice::<Value>(&line) {
            Ok(event) => event,
            Err(_) => continue,
        };
        let line_sha256 = hex_digest(Sha256::digest(&line).as_slice());
        let source_id = projection_source_id(file_identity.as_ref(), row_offset, &line_sha256);
        if let Some((sid, turn_id)) = terminal_turn_identity(&event) {
            if terminal_keys.insert((sid.to_string(), turn_id.to_string())) {
                terminal.consume_if_present(
                    &terminal_projection_line(&source_id, &event)?,
                    "terminal",
                )?;
            }
        } else if let Some(candidate) = parse_turn_verdict_event(&event) {
            let key = (candidate.sid.clone(), candidate.turn_id.clone());
            let changed = latest_verdicts
                .get(&key)
                .is_none_or(|current| !verdict_content_eq(current, &candidate));
            if changed {
                verdict.consume_if_present(
                    &verdict_projection_line(&source_id, &candidate)?,
                    "verdict",
                )?;
                latest_verdicts.insert(key, candidate);
            }
        }
    }
    journal::record_raw_read(offset);
    if offset != metadata.len() {
        anyhow::bail!(
            "progress generation changed while validating bootstrap projection {}",
            source_path.display()
        );
    }
    Ok(())
}

/// A missing index is recoverable only while the retained raw generations can
/// prove every already-landed projection byte from offset zero. The candidate
/// remains read-only until this proof succeeds; normal reconciliation then
/// adopts its exact records (and creates any missing receipts) deterministically.
fn validate_missing_index_projection_bootstrap_locked(path: &Path) -> Result<bool> {
    let mut terminal = UntrustedProjectionPrefix::open(progress_terminal_projection_path(path))?;
    let mut verdict = UntrustedProjectionPrefix::open(progress_verdict_projection_path(path))?;
    if !terminal.is_nonempty() && receipt_root_has_committed_receipt(&terminal_receipt_root(path))?
    {
        anyhow::bail!("terminal projection is empty while committed receipts exist");
    }
    if !verdict.is_nonempty() && receipt_root_has_committed_receipt(&verdict_receipt_root(path))? {
        anyhow::bail!("verdict projection is empty while committed receipts exist");
    }
    if !terminal.is_nonempty() && !verdict.is_nonempty() {
        return Ok(false);
    }

    let mut terminal_keys = BTreeSet::new();
    let mut latest_verdicts = BTreeMap::new();
    validate_bootstrap_projection_generation(
        &progress_archive_path(path),
        &mut terminal,
        &mut verdict,
        &mut terminal_keys,
        &mut latest_verdicts,
    )?;
    validate_bootstrap_projection_generation(
        path,
        &mut terminal,
        &mut verdict,
        &mut terminal_keys,
        &mut latest_verdicts,
    )?;
    terminal.validate_exhausted("terminal")?;
    verdict.validate_exhausted("verdict")?;
    for (sid, turn_id) in terminal_keys {
        if let Some(receipt) = read_terminal_receipt(path, &sid, &turn_id)? {
            terminal_event_from_receipt(path, &receipt)?;
        }
    }
    for (sid, turn_id) in latest_verdicts.keys() {
        if let Some(receipt) = read_verdict_receipt(path, sid, turn_id)? {
            verify_verdict_receipt_projection(path, &receipt)?;
        }
    }
    Ok(true)
}

fn projection_coverage_relation(
    path: &Path,
    coverage: &ProjectionCoverage,
) -> Result<ProjectionCoverageRelation> {
    let Some(metadata) =
        access_optional_progress_state(path, "stat", |path| std::fs::metadata(path))?
    else {
        return Ok(if projection_coverage_is_empty(coverage) {
            ProjectionCoverageRelation::Exact
        } else {
            ProjectionCoverageRelation::Invalid
        });
    };
    if metadata.len() == 0 && projection_coverage_is_empty(coverage) {
        return Ok(ProjectionCoverageRelation::Exact);
    }
    if projection_coverage_is_empty(coverage) {
        return Ok(ProjectionCoverageRelation::UninitializedNonEmpty);
    }
    let identity = pending_active_file_identity(&metadata);
    if identity.is_none() || coverage.file_identity != identity {
        return Ok(ProjectionCoverageRelation::Invalid);
    }
    Ok(match metadata.len().cmp(&coverage.committed_end_offset) {
        std::cmp::Ordering::Equal => ProjectionCoverageRelation::Exact,
        std::cmp::Ordering::Greater => ProjectionCoverageRelation::AppendOnlyExtension,
        std::cmp::Ordering::Less => ProjectionCoverageRelation::Invalid,
    })
}

fn validate_projection_coverage_exact(
    path: &Path,
    coverage: &ProjectionCoverage,
    label: &str,
) -> Result<()> {
    if projection_coverage_relation(path, coverage)? != ProjectionCoverageRelation::Exact {
        anyhow::bail!("{label} projection coverage mismatch in {}", path.display());
    }
    Ok(())
}

fn validate_projection_coverage_for_reconcile(
    path: &Path,
    coverage: &ProjectionCoverage,
    raw_cursors_exact: bool,
    uninitialized_has_authority: bool,
    label: &str,
) -> Result<()> {
    match projection_coverage_relation(path, coverage)? {
        ProjectionCoverageRelation::Exact => Ok(()),
        ProjectionCoverageRelation::AppendOnlyExtension if !raw_cursors_exact => Ok(()),
        ProjectionCoverageRelation::UninitializedNonEmpty if uninitialized_has_authority => Ok(()),
        ProjectionCoverageRelation::AppendOnlyExtension
        | ProjectionCoverageRelation::UninitializedNonEmpty
        | ProjectionCoverageRelation::Invalid => {
            anyhow::bail!("{label} projection coverage mismatch in {}", path.display())
        }
    }
}

fn projection_target_from_coverage(
    path: &Path,
    coverage: &ProjectionCoverage,
    label: &str,
) -> Result<PendingProjectionTarget> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let identity = pending_active_file_identity(&metadata).with_context(|| {
        format!(
            "{label} projection identity unavailable in {}",
            path.display()
        )
    })?;
    if coverage
        .file_identity
        .as_ref()
        .is_some_and(|known| known != &identity)
    {
        anyhow::bail!(
            "{label} projection coverage identity changed in {}",
            path.display()
        );
    }
    if coverage.file_identity.is_none() && coverage.committed_end_offset != 0 {
        anyhow::bail!("{label} projection coverage identity is missing");
    }
    if coverage.committed_end_offset > metadata.len() {
        anyhow::bail!("{label} projection coverage exceeds {}", path.display());
    }
    Ok(PendingProjectionTarget {
        offset: coverage.committed_end_offset,
        file_identity: Some(identity),
    })
}

fn advance_projection_coverage(
    path: &Path,
    coverage: &mut ProjectionCoverage,
    location: &ProjectionRecordLocation,
    label: &str,
) -> Result<()> {
    let metadata = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let identity = pending_active_file_identity(&metadata).with_context(|| {
        format!(
            "{label} projection identity unavailable in {}",
            path.display()
        )
    })?;
    if coverage
        .file_identity
        .as_ref()
        .is_some_and(|known| known != &identity)
    {
        anyhow::bail!(
            "{label} projection coverage identity changed in {}",
            path.display()
        );
    }
    let record_end = location
        .offset
        .checked_add(location.line_len)
        .context("projection record end overflow")?;
    if record_end > metadata.len() {
        anyhow::bail!("{label} projection receipt exceeds {}", path.display());
    }
    if record_end <= coverage.committed_end_offset {
        return Ok(());
    }
    if location.offset > coverage.committed_end_offset {
        anyhow::bail!("{label} projection coverage gap in {}", path.display());
    }
    if location.offset < coverage.committed_end_offset {
        anyhow::bail!(
            "{label} projection record overlaps committed coverage in {}",
            path.display()
        );
    }
    coverage.file_identity = Some(identity);
    coverage.committed_end_offset = record_end;
    Ok(())
}

fn read_bounded_projection_line<R: BufRead>(
    reader: &mut R,
    projection: &Path,
) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let read = reader
        .take(MAX_SPECIAL_RECORD_BYTES.saturating_add(1))
        .read_until(b'\n', &mut line)
        .with_context(|| format!("read {}", projection.display()))?;
    let read = u64::try_from(read).unwrap_or(u64::MAX);
    journal::record_raw_read(read);
    if read == 0 {
        return Ok(None);
    }
    if read > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!(
            "projection record exceeds storage limit in {}",
            projection.display()
        );
    }
    if line.last() != Some(&b'\n') {
        anyhow::bail!("unterminated projection record in {}", projection.display());
    }
    Ok(Some(line))
}

fn scan_verdict_projection(path: &Path) -> Result<BTreeMap<(String, String), TurnVerdict>> {
    let projection = progress_verdict_projection_path(path);
    let Some(file) = access_optional_progress_state(&projection, "open", |path| File::open(path))?
    else {
        if receipt_root_has_committed_receipt(&verdict_receipt_root(path))? {
            anyhow::bail!("verdict projection is missing while committed receipts exist");
        }
        return Ok(BTreeMap::new());
    };
    if file
        .metadata()
        .with_context(|| format!("stat {}", projection.display()))?
        .len()
        == 0
        && receipt_root_has_committed_receipt(&verdict_receipt_root(path))?
    {
        anyhow::bail!("verdict projection is empty while committed receipts exist");
    }
    let mut latest = BTreeMap::new();
    let mut reader = BufReader::new(file);
    while let Some(line) = read_bounded_projection_line(&mut reader, &projection)? {
        let record = serde_json::from_slice::<VerdictProjectionRecord>(&line)
            .with_context(|| format!("parse {}", projection.display()))?;
        if record.schema_version != SPECIAL_PROJECTION_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported verdict projection schema {} in {}",
                record.schema_version,
                projection.display()
            );
        }
        latest.insert(
            (record.verdict.sid.clone(), record.verdict.turn_id.clone()),
            record.verdict,
        );
    }
    Ok(latest)
}

/// Snapshot every first-wins terminal payload for explicit offline experience
/// rebuild. Unlike session history this operation is intentionally O(N).
pub fn terminal_turns_for_rebuild(path: &Path) -> Result<Vec<Value>> {
    let lock_file = open_progress_lock(path)?;
    let _lock = ProgressFileLock::lock(&lock_file)
        .with_context(|| format!("lock {}", progress_lock_path(path).display()))?;
    require_active_progress_lock_locked(&lock_file, path)?;
    let checkpoint = recover_progress_checkpoint_locked(path)?;
    let index = reconcile_verdict_index_locked(path, checkpoint.as_ref())?;
    validate_projection_coverage_exact(
        &progress_terminal_projection_path(path),
        &index.terminal_projection,
        "terminal",
    )?;
    let corrupt_line_count = checkpoint
        .map(|checkpoint| checkpoint.corrupt_line_count)
        .unwrap_or(0)
        .saturating_add(index.active.corrupt_line_count);
    if corrupt_line_count > 0 {
        anyhow::bail!("canonical progress history contains corrupt lines");
    }

    let projection = progress_terminal_projection_path(path);
    let Some(file) = access_optional_progress_state(&projection, "open", |path| File::open(path))?
    else {
        if receipt_root_has_committed_receipt(&terminal_receipt_root(path))? {
            anyhow::bail!("terminal projection is missing while committed receipts exist");
        }
        return Ok(Vec::new());
    };
    if file
        .metadata()
        .with_context(|| format!("stat {}", projection.display()))?
        .len()
        == 0
        && receipt_root_has_committed_receipt(&terminal_receipt_root(path))?
    {
        anyhow::bail!("terminal projection is empty while committed receipts exist");
    }
    let mut first = BTreeMap::new();
    let mut reader = BufReader::new(file);
    while let Some(line) = read_bounded_projection_line(&mut reader, &projection)? {
        let record = serde_json::from_slice::<TerminalProjectionRecord>(&line)
            .with_context(|| format!("parse {}", projection.display()))?;
        if record.schema_version != SPECIAL_PROJECTION_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported terminal projection schema {} in {}",
                record.schema_version,
                projection.display()
            );
        }
        let (sid, turn_id) = terminal_turn_identity(&record.event)
            .context("terminal projection contains malformed event")?;
        first
            .entry((sid.to_string(), turn_id.to_string()))
            .or_insert(record.event);
    }
    Ok(first.into_values().collect())
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
    validate_verdict_special_record_size(verdict)?;
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
    require_active_progress_lock_locked(&lock_file, path)?;
    let checkpoint = recover_progress_checkpoint_locked(path)?;
    let mut index = reconcile_verdict_index_locked(path, checkpoint.as_ref())?;
    if read_verdict_receipt(path, &verdict.sid, &verdict.turn_id)?
        .as_ref()
        .is_some_and(|latest| verdict_content_eq(&latest.verdict, verdict))
    {
        drop(lock);
        record_suppressed(TURN_VERDICT, false, byte_count);
        return Ok(false);
    }

    let current_size =
        access_optional_progress_state(path, "stat", |path| std::fs::metadata(path))?
            .map(|metadata| metadata.len())
            .unwrap_or(0);
    if current_size > 0 && current_size.saturating_add(byte_count) > progress_rotate_bytes() {
        rotate_progress_locked(path)?;
        let checkpoint = recover_progress_checkpoint_locked(path)?;
        index = reconcile_verdict_index_locked(path, checkpoint.as_ref())?;
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
    let active_file_identity = pending_active_file_identity(&active_metadata);
    let projection = projection_append_target(&progress_verdict_projection_path(path))?;
    index.pending = Some(PendingProgressIndexWrite::Verdict {
        verdict: verdict.clone(),
        active_offset,
        line_len: byte_count,
        line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
        active_file_identity: active_file_identity.clone(),
        projection: Some(projection.clone()),
    });
    write_verdict_index(path, &index)?;

    file.write_all(&line)
        .with_context(|| format!("write verdict event to {}", path.display()))?;
    let size = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    file.sync_data()
        .with_context(|| format!("sync verdict event to {}", path.display()))?;
    drop(file);

    let line_sha256 = hex_digest(Sha256::digest(&line).as_slice());
    let source_id =
        projection_source_id(active_file_identity.as_ref(), active_offset, &line_sha256);
    let receipt = apply_verdict_projection_locked(path, verdict, &source_id, Some(&projection))?;
    advance_projection_coverage(
        &progress_verdict_projection_path(path),
        &mut index.verdict_projection,
        &receipt.projection,
        "verdict",
    )?;
    validate_projection_coverage_exact(
        &progress_verdict_projection_path(path),
        &index.verdict_projection,
        "verdict",
    )?;
    index.pending = None;
    index.active.offset = size;
    index.active.file_identity = active_file_identity;
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

fn validate_verdict_special_record_size(verdict: &TurnVerdict) -> Result<()> {
    let source_id = "x".repeat(256);
    let projection = verdict_projection_line(&source_id, verdict)?;
    let projection_len = u64::try_from(projection.len()).unwrap_or(u64::MAX);
    if projection_len > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!("verdict projection record exceeds storage limit");
    }
    let receipt = VerdictReceipt {
        schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
        sid: verdict.sid.clone(),
        turn_id: verdict.turn_id.clone(),
        source_id,
        projection: ProjectionRecordLocation {
            offset: u64::MAX,
            line_len: projection_len,
            line_sha256: "f".repeat(64),
        },
        verdict: verdict.clone(),
    };
    if u64::try_from(
        serde_json::to_vec(&receipt)
            .context("serialize bounded verdict receipt")?
            .len(),
    )
    .unwrap_or(u64::MAX)
        > MAX_SPECIAL_RECORD_BYTES
    {
        anyhow::bail!("verdict receipt exceeds storage limit");
    }
    Ok(())
}

fn read_verdict_index_locked(path: &Path) -> Result<Option<ProgressVerdictIndex>> {
    let index_path = progress_verdict_index_path(path);
    let Some(bytes) =
        access_optional_progress_state(&index_path, "read", |path| std::fs::read(path))?
    else {
        return Ok(None);
    };
    journal::record_raw_read(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    let version = match serde_json::from_slice::<ProgressVerdictIndexVersion>(&bytes) {
        Ok(envelope) => envelope.schema_version,
        Err(_) => return Ok(None),
    };
    if version > VERDICT_INDEX_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported progress verdict index schema {} in {}",
            version,
            index_path.display()
        );
    }
    let mut index = match serde_json::from_slice::<ProgressVerdictIndex>(&bytes) {
        Ok(index) => index,
        // This file is only a compact projection. A torn or otherwise invalid
        // copy must never take the authoritative progress journal (or web
        // startup) down; callers holding the journal lock rebuild it below.
        Err(_) => return Ok(None),
    };
    let Some(pending) = index.pending.take() else {
        return Ok(Some(index));
    };
    match pending {
        PendingProgressIndexWrite::Verdict {
            verdict,
            active_offset,
            line_len,
            line_sha256,
            active_file_identity,
            projection,
        } => {
            let mut event = serde_json::to_value(&verdict)
                .context("serialize verdict from pending progress index")?;
            event
                .as_object_mut()
                .context("pending verdict must serialize as an object")?
                .insert("event".into(), Value::String(TURN_VERDICT.into()));
            let mut expected = serde_json::to_vec(&event)
                .context("serialize verdict event from pending progress index")?;
            expected.push(b'\n');
            if recover_pending_exact_append(
                path,
                "verdict",
                &expected,
                active_offset,
                line_len,
                &line_sha256,
                active_file_identity.as_ref(),
            )? == PendingTerminalAppendState::Committed
            {
                let source_id = projection_source_id(
                    active_file_identity.as_ref(),
                    active_offset,
                    &line_sha256,
                );
                let receipt = apply_verdict_projection_locked(
                    path,
                    &verdict,
                    &source_id,
                    projection.as_ref(),
                )?;
                advance_projection_coverage(
                    &progress_verdict_projection_path(path),
                    &mut index.verdict_projection,
                    &receipt.projection,
                    "verdict",
                )?;
                validate_projection_coverage_exact(
                    &progress_verdict_projection_path(path),
                    &index.verdict_projection,
                    "verdict",
                )?;
            }
        }
        PendingProgressIndexWrite::TerminalTurn {
            event,
            active_offset,
            line_len,
            line_sha256,
            active_file_identity,
            projection,
        } => {
            terminal_turn_identity(&event)
                .context("malformed terminal turn in pending progress index")?;
            match recover_pending_terminal_append(
                path,
                &event,
                active_offset,
                line_len,
                &line_sha256,
                active_file_identity.as_ref(),
            )? {
                PendingTerminalAppendState::Committed => {
                    let source_id = projection_source_id(
                        active_file_identity.as_ref(),
                        active_offset,
                        &line_sha256,
                    );
                    let receipt = apply_terminal_projection_locked(
                        path,
                        &event,
                        &source_id,
                        projection.as_ref(),
                    )?;
                    advance_projection_coverage(
                        &progress_terminal_projection_path(path),
                        &mut index.terminal_projection,
                        &receipt.projection,
                        "terminal",
                    )?;
                    validate_projection_coverage_exact(
                        &progress_terminal_projection_path(path),
                        &index.terminal_projection,
                        "terminal",
                    )?;
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
    let mut expected =
        serde_json::to_vec(event).context("serialize terminal turn from pending progress index")?;
    expected.push(b'\n');
    recover_pending_exact_append(
        path,
        "terminal",
        &expected,
        active_offset,
        line_len,
        line_sha256,
        expected_identity,
    )
}

fn recover_pending_exact_append(
    path: &Path,
    kind: &str,
    expected: &[u8],
    active_offset: u64,
    line_len: u64,
    line_sha256: &str,
    expected_identity: Option<&PendingActiveFileIdentity>,
) -> Result<PendingTerminalAppendState> {
    let expected_len = usize::try_from(line_len).context("pending progress line too large")?;
    if expected.len() != expected_len
        || hex_digest(Sha256::digest(expected).as_slice()) != line_sha256
    {
        anyhow::bail!("ambiguous pending {kind} append: index line identity mismatch");
    }

    let Some(mut file) = access_optional_progress_state(path, "open", |path| {
        OpenOptions::new().read(true).write(true).open(path)
    })?
    else {
        if active_offset == 0 && expected_identity.is_none() {
            return Ok(PendingTerminalAppendState::Absent);
        }
        anyhow::bail!(
            "ambiguous pending {kind} append: active file is absent before offset {active_offset}"
        );
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let active_len = metadata.len();
    if active_len < active_offset {
        anyhow::bail!(
            "ambiguous pending {kind} append: active file ends before offset {active_offset}"
        );
    }

    let available = active_len.saturating_sub(active_offset);
    if available == 0 {
        ensure_pending_file_identity(kind, expected_identity, &metadata)?;
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
        if expected_identity.is_some() {
            ensure_pending_file_identity(kind, expected_identity, &metadata)?;
        }
        return Ok(PendingTerminalAppendState::Committed);
    }
    if available < line_len && raw == expected[..read_len] {
        ensure_pending_file_identity(kind, expected_identity, &metadata)?;
        file.set_len(active_offset)
            .with_context(|| format!("truncate torn terminal tail in {}", path.display()))?;
        file.sync_data()
            .with_context(|| format!("sync truncated terminal tail in {}", path.display()))?;
        return Ok(PendingTerminalAppendState::Absent);
    }

    anyhow::bail!("ambiguous pending {kind} append: active bytes do not match the recorded line");
}

fn ensure_pending_file_identity(
    kind: &str,
    expected: Option<&PendingActiveFileIdentity>,
    metadata: &std::fs::Metadata,
) -> Result<()> {
    let Some(expected) = expected else {
        anyhow::bail!("ambiguous pending {kind} append: active file identity is unavailable");
    };
    if pending_active_file_identity(metadata).as_ref() != Some(expected) {
        anyhow::bail!("ambiguous pending {kind} append: active file identity changed");
    }
    Ok(())
}

fn projection_source_id(
    active_file_identity: Option<&PendingActiveFileIdentity>,
    active_offset: u64,
    line_sha256: &str,
) -> String {
    match active_file_identity {
        Some(identity) => format!(
            "progress:{}:{}:{active_offset}:{line_sha256}",
            identity.device, identity.inode
        ),
        None => format!("progress:portable:{active_offset}:{line_sha256}"),
    }
}

fn projection_append_target(path: &Path) -> Result<PendingProjectionTarget> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    if metadata.len() > 0 {
        file.seek(SeekFrom::End(-1))
            .with_context(|| format!("seek projection tail in {}", path.display()))?;
        let mut last = [0_u8; 1];
        file.read_exact(&mut last)
            .with_context(|| format!("read projection tail from {}", path.display()))?;
        journal::record_raw_read(1);
        if last[0] != b'\n' {
            truncate_unterminated_projection_tail(&mut file, path, &metadata)?;
            metadata = file
                .metadata()
                .with_context(|| format!("stat repaired {}", path.display()))?;
        }
    }
    Ok(PendingProjectionTarget {
        offset: metadata.len(),
        file_identity: pending_active_file_identity(&metadata),
    })
}

/// Repair an exceptional fence-less derived append interrupted before its
/// newline. Normal projection appends inspect only the final byte above; this
/// backwards scan runs solely after detecting a torn tail. Receipts are
/// published only after the complete newline-terminated record is synced, so
/// no committed receipt can legitimately point into the truncated suffix.
fn truncate_unterminated_projection_tail(
    file: &mut File,
    path: &Path,
    original: &std::fs::Metadata,
) -> Result<()> {
    const SEARCH_CHUNK: u64 = 64 * 1024;

    let original_len = original.len();
    let original_identity = pending_active_file_identity(original);
    let mut search_end = original_len;
    let truncate_to = loop {
        let search_start = search_end.saturating_sub(SEARCH_CHUNK);
        let read_len = usize::try_from(search_end - search_start)
            .context("projection repair chunk is too large")?;
        let mut chunk = vec![0_u8; read_len];
        file.seek(SeekFrom::Start(search_start))
            .with_context(|| format!("seek projection repair in {}", path.display()))?;
        file.read_exact(&mut chunk)
            .with_context(|| format!("read projection repair from {}", path.display()))?;
        journal::record_raw_read(u64::try_from(read_len).unwrap_or(u64::MAX));
        if let Some(index) = chunk.iter().rposition(|byte| *byte == b'\n') {
            break search_start.saturating_add(index as u64).saturating_add(1);
        }
        if search_start == 0 {
            break 0;
        }
        search_end = search_start;
    };

    let path_metadata = std::fs::metadata(path)
        .with_context(|| format!("stat projection repair target {}", path.display()))?;
    if path_metadata.len() != original_len
        || pending_active_file_identity(&path_metadata) != original_identity
    {
        anyhow::bail!("projection changed while repairing {}", path.display());
    }
    file.set_len(truncate_to)
        .with_context(|| format!("truncate projection tail in {}", path.display()))?;
    file.sync_data()
        .with_context(|| format!("sync repaired projection {}", path.display()))?;
    Ok(())
}

fn ensure_exact_projection_line(
    path: &Path,
    target: &PendingProjectionTarget,
    line: &[u8],
) -> Result<()> {
    let location = projection_record_location(target, line);
    match recover_pending_exact_append(
        path,
        "projection",
        line,
        location.offset,
        location.line_len,
        &location.line_sha256,
        target.file_identity.as_ref(),
    )? {
        PendingTerminalAppendState::Committed => {}
        PendingTerminalAppendState::Absent => {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(path)
                .with_context(|| format!("open {}", path.display()))?;
            let metadata = file
                .metadata()
                .with_context(|| format!("stat {}", path.display()))?;
            if metadata.len() != target.offset {
                anyhow::bail!(
                    "ambiguous pending projection append: expected offset {}, found {}",
                    target.offset,
                    metadata.len()
                );
            }
            if target.file_identity.is_some()
                && pending_active_file_identity(&metadata) != target.file_identity
            {
                anyhow::bail!("ambiguous pending projection append: file identity changed");
            }
            file.write_all(line)
                .with_context(|| format!("append {}", path.display()))?;
            file.sync_data()
                .with_context(|| format!("sync {}", path.display()))?;
        }
    }
    Ok(())
}

fn projection_record_location(
    target: &PendingProjectionTarget,
    line: &[u8],
) -> ProjectionRecordLocation {
    ProjectionRecordLocation {
        offset: target.offset,
        line_len: u64::try_from(line.len()).unwrap_or(u64::MAX),
        line_sha256: hex_digest(Sha256::digest(line).as_slice()),
    }
}

fn terminal_projection_line(source_id: &str, event: &Value) -> Result<Vec<u8>> {
    let mut line = serde_json::to_vec(&TerminalProjectionRecord {
        schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
        source_id: source_id.to_string(),
        event: event.clone(),
    })
    .context("serialize terminal projection")?;
    line.push(b'\n');
    Ok(line)
}

fn verdict_projection_line(source_id: &str, verdict: &TurnVerdict) -> Result<Vec<u8>> {
    let mut line = serde_json::to_vec(&VerdictProjectionRecord {
        schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
        source_id: source_id.to_string(),
        verdict: verdict.clone(),
    })
    .context("serialize verdict projection")?;
    line.push(b'\n');
    Ok(line)
}

fn read_exact_projection_record<T: serde::de::DeserializeOwned>(
    path: &Path,
    location: &ProjectionRecordLocation,
) -> Result<T> {
    if location.line_len > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!("projection record exceeds bounded lookup limit");
    }
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    file.seek(SeekFrom::Start(location.offset))
        .with_context(|| format!("seek {}", path.display()))?;
    let len = usize::try_from(location.line_len).context("projection record too large")?;
    let mut line = vec![0_u8; len];
    file.read_exact(&mut line)
        .with_context(|| format!("read {}", path.display()))?;
    journal::record_raw_read(location.line_len);
    if hex_digest(Sha256::digest(&line).as_slice()) != location.line_sha256 {
        anyhow::bail!("projection record hash mismatch in {}", path.display());
    }
    serde_json::from_slice(&line).with_context(|| format!("parse {}", path.display()))
}

fn terminal_event_from_receipt(active_path: &Path, receipt: &TerminalReceipt) -> Result<Value> {
    let record = read_exact_projection_record::<TerminalProjectionRecord>(
        &progress_terminal_projection_path(active_path),
        &receipt.projection,
    )?;
    if record.schema_version != SPECIAL_PROJECTION_SCHEMA_VERSION
        || record.source_id != receipt.source_id
    {
        anyhow::bail!("terminal receipt does not match its projection record");
    }
    let (sid, turn_id) = terminal_turn_identity(&record.event)
        .context("terminal projection contains malformed event")?;
    if sid != receipt.sid || turn_id != receipt.turn_id {
        anyhow::bail!("terminal receipt points at a different projection key");
    }
    Ok(record.event)
}

fn verify_verdict_receipt_projection(active_path: &Path, receipt: &VerdictReceipt) -> Result<()> {
    if receipt.projection.line_len > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!("verdict projection record exceeds bounded lookup limit");
    }
    let record = read_exact_projection_record::<VerdictProjectionRecord>(
        &progress_verdict_projection_path(active_path),
        &receipt.projection,
    )?;
    if record.schema_version != SPECIAL_PROJECTION_SCHEMA_VERSION
        || record.source_id != receipt.source_id
        || record.verdict != receipt.verdict
    {
        anyhow::bail!("verdict receipt does not match its projection record");
    }
    Ok(())
}

fn apply_terminal_projection_locked(
    active_path: &Path,
    event: &Value,
    source_id: &str,
    target: Option<&PendingProjectionTarget>,
) -> Result<TerminalReceipt> {
    let (sid, turn_id) =
        terminal_turn_identity(event).context("malformed terminal projection event")?;
    if let Some(receipt) = read_terminal_receipt_with_policy(
        active_path,
        sid,
        turn_id,
        ReceiptIntegrityPolicy::CanonicalRepair,
    )? {
        terminal_event_from_receipt(active_path, &receipt)?;
        return Ok(receipt);
    }
    let owned_target;
    let target = match target {
        Some(target) => target,
        None => {
            owned_target =
                projection_append_target(&progress_terminal_projection_path(active_path))?;
            &owned_target
        }
    };
    let line = terminal_projection_line(source_id, event)?;
    if u64::try_from(line.len()).unwrap_or(u64::MAX) > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!("terminal projection record exceeds storage limit");
    }
    let projection = projection_record_location(target, &line);
    let receipt = TerminalReceipt {
        schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
        sid: sid.to_string(),
        turn_id: turn_id.to_string(),
        source_id: source_id.to_string(),
        projection,
    };
    let receipt_bytes = serialize_bounded_receipt(&receipt, "terminal receipt")?;
    ensure_exact_projection_line(
        &progress_terminal_projection_path(active_path),
        target,
        &line,
    )?;
    let receipt_path = special_receipt_path(
        &terminal_receipt_root(active_path),
        &receipt.sid,
        &receipt.turn_id,
    );
    persist_receipt_bytes(&receipt_path, &receipt_bytes)?;
    Ok(receipt)
}

fn apply_verdict_projection_locked(
    active_path: &Path,
    verdict: &TurnVerdict,
    source_id: &str,
    target: Option<&PendingProjectionTarget>,
) -> Result<VerdictReceipt> {
    if let Some(receipt) = read_verdict_receipt_with_policy(
        active_path,
        &verdict.sid,
        &verdict.turn_id,
        ReceiptIntegrityPolicy::CanonicalRepair,
    )? {
        verify_verdict_receipt_projection(active_path, &receipt)?;
        if receipt.source_id == source_id || verdict_content_eq(&receipt.verdict, verdict) {
            return Ok(receipt);
        }
    }
    let owned_target;
    let target = match target {
        Some(target) => target,
        None => {
            owned_target =
                projection_append_target(&progress_verdict_projection_path(active_path))?;
            &owned_target
        }
    };
    let line = verdict_projection_line(source_id, verdict)?;
    if u64::try_from(line.len()).unwrap_or(u64::MAX) > MAX_SPECIAL_RECORD_BYTES {
        anyhow::bail!("verdict projection record exceeds storage limit");
    }
    let projection = projection_record_location(target, &line);
    let receipt = VerdictReceipt {
        schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
        sid: verdict.sid.clone(),
        turn_id: verdict.turn_id.clone(),
        source_id: source_id.to_string(),
        projection,
        verdict: verdict.clone(),
    };
    let receipt_bytes = serialize_bounded_receipt(&receipt, "verdict receipt")?;
    ensure_exact_projection_line(
        &progress_verdict_projection_path(active_path),
        target,
        &line,
    )?;
    let receipt_path = special_receipt_path(
        &verdict_receipt_root(active_path),
        &receipt.sid,
        &receipt.turn_id,
    );
    persist_receipt_bytes(&receipt_path, &receipt_bytes)?;
    Ok(receipt)
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

fn write_verdict_index(path: &Path, index: &ProgressVerdictIndex) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(index).context("serialize progress verdict index")?;
    atomic_write_durable(&progress_verdict_index_path(path), &bytes)
}

fn ensure_verdict_index_locked(path: &Path, checkpoint: Option<&ProgressCheckpoint>) -> Result<()> {
    reconcile_verdict_index_locked(path, checkpoint).map(drop)
}

fn reconcile_verdict_index_locked(
    path: &Path,
    checkpoint: Option<&ProgressCheckpoint>,
) -> Result<ProgressVerdictIndex> {
    let index_path = progress_verdict_index_path(path);
    let index_file_existed = !progress_state_path_is_absent(&index_path)?;
    let loaded_index = read_verdict_index_locked(path)?;
    let corrupt_existing_index = index_file_existed && loaded_index.is_none();
    let missing_index_bootstrap = loaded_index.is_none() && !index_file_existed;
    let checkpoint_has_legacy_maps = checkpoint.is_some_and(|checkpoint| {
        !checkpoint.turn_verdicts.is_empty() || !checkpoint.terminal_turns.is_empty()
    });
    let mut index = match loaded_index {
        Some(index) => index,
        None => {
            let invalid_index = if index_file_existed {
                Some(
                    std::fs::read(&index_path)
                        .with_context(|| format!("read {}", index_path.display()))?,
                )
            } else {
                None
            };
            if let Some(invalid_index) = invalid_index {
                atomic_write_durable(&corrupt_verdict_index_path(path), &invalid_index)?;
            }
            ProgressVerdictIndex::default()
        }
    };
    if corrupt_existing_index && checkpoint_has_legacy_maps {
        anyhow::bail!("corrupt progress verdict index cannot migrate legacy checkpoint");
    }
    let legacy_migration = index.schema_version < VERDICT_INDEX_SCHEMA_VERSION
        || (!index_file_existed && checkpoint_has_legacy_maps);
    let original_index = index.clone();
    let _ = finish_unterminated_progress_tail_locked(path)?;
    let checkpoint_rotation_sequence = checkpoint
        .map(|checkpoint| checkpoint.rotation_sequence)
        .unwrap_or(0);
    let archive_path = progress_archive_path(path);
    let active_was_absent = progress_state_path_is_absent(path)?;
    let active_had_coverage = active_verdict_coverage_has_raw_state(&index.active);
    let mut active_coverage_transferred = false;
    if progress_cursor_is_exact(&archive_path, &index.active)? {
        active_coverage_transferred = active_had_coverage;
        index.archive = index.active.clone();
        index.active = ActiveVerdictCoverage::default();
    }
    if active_was_absent && active_had_coverage && !active_coverage_transferred {
        anyhow::bail!(
            "active progress journal is missing while its verdict-index coverage remains"
        );
    }
    // Archive rows are logically older than every active row. If an archive
    // replacement or extension makes us scan any of it again, replay active
    // afterwards as well; otherwise an older archive verdict can replace the
    // latest receipt while an exact active cursor skips the restoring row.
    if !progress_cursor_is_exact(&archive_path, &index.archive)? {
        index.active = ActiveVerdictCoverage::default();
    }
    let bootstrap_projection = if missing_index_bootstrap && !legacy_migration {
        validate_missing_index_projection_bootstrap_locked(path)?
    } else {
        false
    };
    let uninitialized_projection_has_authority = legacy_migration || bootstrap_projection;
    let raw_cursors_exact = index.active.checkpoint_rotation_sequence
        == checkpoint_rotation_sequence
        && index.archive.checkpoint_rotation_sequence == checkpoint_rotation_sequence
        && progress_cursor_is_exact(path, &index.active)?
        && progress_cursor_is_exact(&archive_path, &index.archive)?;
    validate_projection_coverage_for_reconcile(
        &progress_terminal_projection_path(path),
        &index.terminal_projection,
        raw_cursors_exact,
        uninitialized_projection_has_authority,
        "terminal",
    )?;
    validate_projection_coverage_for_reconcile(
        &progress_verdict_projection_path(path),
        &index.verdict_projection,
        raw_cursors_exact,
        uninitialized_projection_has_authority,
        "verdict",
    )?;
    let mut terminal_projection = index.terminal_projection.clone();
    let mut verdict_projection = index.verdict_projection.clone();

    if legacy_migration {
        if let Some(checkpoint) = checkpoint {
            for (sid, turns) in &checkpoint.terminal_turns {
                for (turn_id, event) in turns {
                    let source_id = format!(
                        "legacy-checkpoint:{}:terminal:{sid}:{turn_id}",
                        checkpoint.rotation_sequence
                    );
                    let projection_path = progress_terminal_projection_path(path);
                    let target = projection_target_from_coverage(
                        &projection_path,
                        &terminal_projection,
                        "terminal",
                    )?;
                    let receipt =
                        apply_terminal_projection_locked(path, event, &source_id, Some(&target))?;
                    advance_projection_coverage(
                        &projection_path,
                        &mut terminal_projection,
                        &receipt.projection,
                        "terminal",
                    )?;
                }
            }
            for (sid, turns) in &checkpoint.turn_verdicts {
                for (turn_id, verdict) in turns {
                    let source_id = format!(
                        "legacy-checkpoint:{}:verdict:{sid}:{turn_id}",
                        checkpoint.rotation_sequence
                    );
                    let projection_path = progress_verdict_projection_path(path);
                    let target = projection_target_from_coverage(
                        &projection_path,
                        &verdict_projection,
                        "verdict",
                    )?;
                    let receipt =
                        apply_verdict_projection_locked(path, verdict, &source_id, Some(&target))?;
                    advance_projection_coverage(
                        &projection_path,
                        &mut verdict_projection,
                        &receipt.projection,
                        "verdict",
                    )?;
                }
            }
        }
        for (sid, turns) in &index.terminal_turns {
            for (turn_id, event) in turns {
                let source_id = format!("legacy-index:terminal:{sid}:{turn_id}");
                let projection_path = progress_terminal_projection_path(path);
                let target = projection_target_from_coverage(
                    &projection_path,
                    &terminal_projection,
                    "terminal",
                )?;
                let receipt =
                    apply_terminal_projection_locked(path, event, &source_id, Some(&target))?;
                advance_projection_coverage(
                    &projection_path,
                    &mut terminal_projection,
                    &receipt.projection,
                    "terminal",
                )?;
            }
        }
        for (sid, turns) in &index.verdicts {
            for (turn_id, verdict) in turns {
                let source_id = format!("legacy-index:verdict:{sid}:{turn_id}");
                let projection_path = progress_verdict_projection_path(path);
                let target = projection_target_from_coverage(
                    &projection_path,
                    &verdict_projection,
                    "verdict",
                )?;
                let receipt =
                    apply_verdict_projection_locked(path, verdict, &source_id, Some(&target))?;
                advance_projection_coverage(
                    &projection_path,
                    &mut verdict_projection,
                    &receipt.projection,
                    "verdict",
                )?;
            }
        }
    }
    index.terminal_turns.clear();
    index.verdicts.clear();

    scan_progress_special_generation_locked(
        path,
        &archive_path,
        &mut index.archive,
        &mut terminal_projection,
        &mut verdict_projection,
    )?;
    scan_progress_special_generation_locked(
        path,
        path,
        &mut index.active,
        &mut terminal_projection,
        &mut verdict_projection,
    )?;
    validate_projection_coverage_exact(
        &progress_terminal_projection_path(path),
        &terminal_projection,
        "terminal",
    )?;
    validate_projection_coverage_exact(
        &progress_verdict_projection_path(path),
        &verdict_projection,
        "verdict",
    )?;
    index.terminal_projection = terminal_projection;
    index.verdict_projection = verdict_projection;
    index.checkpoint_corrupt_line_count = checkpoint
        .map(|checkpoint| checkpoint.corrupt_line_count)
        .unwrap_or(0);
    index.active.checkpoint_rotation_sequence = checkpoint_rotation_sequence;
    index.archive.checkpoint_rotation_sequence = checkpoint_rotation_sequence;
    index.schema_version = VERDICT_INDEX_SCHEMA_VERSION;
    if index != original_index {
        write_verdict_index(path, &index)?;
    }

    if let Some(checkpoint) = checkpoint {
        if checkpoint.schema_version != CHECKPOINT_SCHEMA_VERSION
            || !checkpoint.turn_verdicts.is_empty()
            || !checkpoint.terminal_turns.is_empty()
        {
            let mut compact = checkpoint.clone();
            compact.schema_version = CHECKPOINT_SCHEMA_VERSION;
            compact.turn_verdicts.clear();
            compact.terminal_turns.clear();
            write_progress_checkpoint(path, &compact)?;
        }
    }
    Ok(index)
}

fn scan_progress_special_generation_locked(
    active_path: &Path,
    source_path: &Path,
    cursor: &mut ActiveVerdictCoverage,
    terminal_projection: &mut ProjectionCoverage,
    verdict_projection: &mut ProjectionCoverage,
) -> Result<()> {
    let Some(mut file) =
        access_optional_progress_state(source_path, "open", |path| File::open(path))?
    else {
        *cursor = ActiveVerdictCoverage::default();
        return Ok(());
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", source_path.display()))?;
    let file_identity = pending_active_file_identity(&metadata);
    let same_generation =
        cursor_covers_progress_generation(cursor, file_identity.as_ref(), metadata.len());
    let mut offset = if same_generation { cursor.offset } else { 0 };
    let mut corrupt = if same_generation {
        cursor.corrupt_line_count
    } else {
        0
    };
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("seek {}", source_path.display()))?;
    let mut reader = BufReader::new(file);
    let mut bytes_read = 0_u64;
    loop {
        let row_offset = offset;
        let mut line = Vec::new();
        let read = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("read {}", source_path.display()))?;
        if read == 0 {
            break;
        }
        let read_u64 = u64::try_from(read).unwrap_or(u64::MAX);
        bytes_read = bytes_read.saturating_add(read_u64);
        if line.last() != Some(&b'\n') {
            break;
        }
        offset = offset.saturating_add(read_u64);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let event = match serde_json::from_slice::<Value>(&line) {
            Ok(event) => event,
            Err(_) => {
                corrupt = corrupt.saturating_add(1);
                continue;
            }
        };
        let line_sha256 = hex_digest(Sha256::digest(&line).as_slice());
        if terminal_turn_identity(&event).is_some() {
            let source_id = projection_source_id(file_identity.as_ref(), row_offset, &line_sha256);
            let projection_path = progress_terminal_projection_path(active_path);
            let target =
                projection_target_from_coverage(&projection_path, terminal_projection, "terminal")?;
            let receipt =
                apply_terminal_projection_locked(active_path, &event, &source_id, Some(&target))?;
            advance_projection_coverage(
                &projection_path,
                terminal_projection,
                &receipt.projection,
                "terminal",
            )?;
        } else if let Some(verdict) = parse_turn_verdict_event(&event) {
            let source_id = projection_source_id(file_identity.as_ref(), row_offset, &line_sha256);
            let projection_path = progress_verdict_projection_path(active_path);
            let target =
                projection_target_from_coverage(&projection_path, verdict_projection, "verdict")?;
            let receipt =
                apply_verdict_projection_locked(active_path, &verdict, &source_id, Some(&target))?;
            advance_projection_coverage(
                &projection_path,
                verdict_projection,
                &receipt.projection,
                "verdict",
            )?;
        }
    }
    journal::record_raw_read(bytes_read);
    if offset != metadata.len() {
        anyhow::bail!(
            "progress generation changed while projecting {}",
            source_path.display()
        );
    }
    cursor.offset = offset;
    cursor.corrupt_line_count = corrupt;
    cursor.file_identity = file_identity;
    Ok(())
}

fn cursor_covers_progress_generation(
    cursor: &ActiveVerdictCoverage,
    file_identity: Option<&PendingActiveFileIdentity>,
    file_len: u64,
) -> bool {
    file_identity.is_some()
        && cursor.file_identity.as_ref() == file_identity
        && cursor.offset <= file_len
}

fn archive_progress_state(path: &Path) -> Result<Option<(u64, Option<PendingActiveFileIdentity>)>> {
    let Some(metadata) =
        access_optional_progress_state(path, "stat", |path| std::fs::metadata(path))?
    else {
        return Ok(None);
    };
    Ok(Some((
        metadata.len(),
        pending_active_file_identity(&metadata),
    )))
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
    require_active_progress_lock_locked(&lock_file, path)?;

    append_serialized_locked(path, line)
}

fn append_serialized_locked(path: &Path, line: &[u8]) -> Result<bool> {
    settle_pending_progress_write_locked(path)?;
    if !progress_state_exists(path)? {
        write_verdict_index(path, &ProgressVerdictIndex::default())?;
    }
    // A real crash in the rename -> checkpoint window leaves `.1` present and
    // active absent. Recover before accepting another row so an uncovered
    // archive can never survive until a later rotation replaces it.
    if progress_state_path_is_absent(path)?
        && !progress_state_path_is_absent(&progress_archive_path(path))?
    {
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

fn ensure_progress_lock_parent(active_path: &Path) -> Result<()> {
    let lock_path = progress_lock_path(active_path);
    let parent = lock_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let metadata =
        std::fs::symlink_metadata(parent).with_context(|| format!("stat {}", parent.display()))?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::metadata(parent)
            .with_context(|| format!("resolve progress lock parent {}", parent.display()))?;
        if !target.is_dir() {
            anyhow::bail!(
                "progress lock parent does not resolve to a directory: {}",
                parent.display()
            );
        }
    } else if !metadata.is_dir() {
        anyhow::bail!(
            "progress lock parent is not a directory: {}",
            parent.display()
        );
    }
    Ok(())
}

fn validate_progress_lock_metadata(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        anyhow::bail!("progress lock is a symlink: {}", path.display());
    }
    if !metadata.file_type().is_file() {
        anyhow::bail!("progress lock is not a regular file: {}", path.display());
    }
    Ok(())
}

fn open_existing_progress_lock(active_path: &Path, writable: bool) -> Result<Option<File>> {
    let path = progress_lock_path(active_path);
    let Some(metadata) = progress_state_entry(&path)? else {
        return Ok(None);
    };
    validate_progress_lock_metadata(&path, &metadata)?;
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("stat open progress lock {}", path.display()))?;
    validate_progress_lock_metadata(&path, &opened_metadata)?;
    Ok(Some(file))
}

fn open_progress_lock(active_path: &Path) -> Result<File> {
    let path = progress_lock_path(active_path);
    if let Some(metadata) = progress_state_entry(&path)? {
        validate_progress_lock_metadata(&path, &metadata)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("stat open progress lock {}", path.display()))?;
    validate_progress_lock_metadata(&path, &opened_metadata)?;
    Ok(file)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressLockState {
    LegacyEmpty,
    Active,
    Retired,
}

fn read_progress_lock_state_locked(file: &File, active_path: &Path) -> Result<ProgressLockState> {
    let lock_path = progress_lock_path(active_path);
    let length = file
        .metadata()
        .with_context(|| format!("stat open progress lock {}", lock_path.display()))?
        .len();
    if length == 0 {
        return Ok(ProgressLockState::LegacyEmpty);
    }
    if length != PROGRESS_LOCK_ACTIVE_MARKER.len() as u64 {
        anyhow::bail!(
            "invalid or torn progress lock marker in {}",
            lock_path.display()
        );
    }

    let mut marker = [0_u8; PROGRESS_LOCK_ACTIVE_MARKER.len()];
    let mut handle = file;
    handle
        .seek(SeekFrom::Start(0))
        .with_context(|| format!("seek progress lock marker {}", lock_path.display()))?;
    handle
        .read_exact(&mut marker)
        .with_context(|| format!("read progress lock marker {}", lock_path.display()))?;
    if &marker == PROGRESS_LOCK_ACTIVE_MARKER {
        Ok(ProgressLockState::Active)
    } else if &marker == PROGRESS_LOCK_RETIRED_MARKER {
        Ok(ProgressLockState::Retired)
    } else {
        anyhow::bail!("unknown progress lock marker in {}", lock_path.display())
    }
}

fn write_progress_lock_marker_locked(
    file: &File,
    active_path: &Path,
    marker: &[u8; PROGRESS_LOCK_ACTIVE_MARKER.len()],
) -> Result<()> {
    let lock_path = progress_lock_path(active_path);
    let mut handle = file;
    handle
        .seek(SeekFrom::Start(0))
        .with_context(|| format!("seek progress lock marker {}", lock_path.display()))?;
    handle
        .write_all(marker)
        .with_context(|| format!("write progress lock marker {}", lock_path.display()))?;
    file.set_len(marker.len() as u64)
        .with_context(|| format!("truncate progress lock marker {}", lock_path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync progress lock marker {}", lock_path.display()))?;
    sync_progress_lock_parent(active_path)
}

fn sync_progress_lock_parent(active_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let lock_path = progress_lock_path(active_path);
        let parent = lock_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)
            .with_context(|| format!("open progress lock parent {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("sync progress lock parent {}", parent.display()))?;
    }
    Ok(())
}

fn require_active_progress_lock_locked(file: &File, active_path: &Path) -> Result<()> {
    match read_progress_lock_state_locked(file, active_path)? {
        ProgressLockState::LegacyEmpty => {
            write_progress_lock_marker_locked(file, active_path, PROGRESS_LOCK_ACTIVE_MARKER)
        }
        ProgressLockState::Active => Ok(()),
        ProgressLockState::Retired => {
            anyhow::bail!("progress state is retired: {}", active_path.display())
        }
    }
}

fn require_retired_progress_lock_locked(file: &File, active_path: &Path) -> Result<()> {
    match read_progress_lock_state_locked(file, active_path)? {
        ProgressLockState::Retired => Ok(()),
        ProgressLockState::LegacyEmpty | ProgressLockState::Active => {
            anyhow::bail!("progress state is not retired: {}", active_path.display())
        }
    }
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
    let checkpoint = recover_progress_checkpoint_locked(active_path)?;
    File::create(active_path).with_context(|| format!("create {}", active_path.display()))?;
    // Publish an empty-generation cursor tied to the newly incremented
    // checkpoint sequence. A crash before this write is repaired by the same
    // sequence mismatch on startup/read.
    reconcile_verdict_index_locked(active_path, checkpoint.as_ref())?;
    Ok(())
}

fn settle_pending_progress_write_locked(path: &Path) -> Result<()> {
    // Resolve an exact pending verdict/terminal fence before touching the raw
    // tail. Ordinary telemetry appends deliberately do not refresh the full
    // compact index here: the next startup/read/special writer streams only
    // the uncovered delta, avoiding a sidecar rewrite on every progress row.
    // Checkpoint recovery runs first because its missing-checkpoint preflight
    // is pure: unknown lifetime history must fail before pending recovery,
    // tail delimiting, quarantine, or any other state mutation.
    let checkpoint = recover_progress_checkpoint_locked(path)?;
    let index = read_verdict_index_locked(path)?;
    let repaired_unterminated_tail = finish_unterminated_progress_tail_locked(path)?;
    let absent_active_requires_reconcile = progress_state_path_is_absent(path)?
        && index
            .as_ref()
            .is_some_and(|index| active_verdict_coverage_has_raw_state(&index.active));
    if index.is_none() || repaired_unterminated_tail || absent_active_requires_reconcile {
        // Missing/malformed derived state is an exceptional repair path, not
        // the ordinary append hot path. Canonical progress still exists, so
        // quarantine and deterministically rebuild it under the same flock.
        reconcile_verdict_index_locked(path, checkpoint.as_ref())?;
    }
    Ok(())
}

fn settle_pending_progress_write_for_repair_locked(path: &Path) -> Result<bool> {
    // A valid index may carry an exact pending fence that must be resolved
    // before replacing active bytes. Missing or malformed derived state is not
    // a prerequisite for canonical byte repair, though: delimit the raw tail
    // and leave the index stale/fail-closed instead of forcing checkpoint
    // hydration that an independently damaged checkpoint cannot provide.
    let index_path_existed = !progress_state_path_is_absent(&progress_verdict_index_path(path))?;
    let index = read_verdict_index_locked(path)?;
    let archive_exists = archive_progress_state(&progress_archive_path(path))?.is_some();
    let _ = finish_unterminated_progress_tail_locked(path)?;
    let checkpoint_absence_is_authoritative = !archive_exists
        && match index {
            Some(index) => {
                index.schema_version == VERDICT_INDEX_SCHEMA_VERSION
                    && index.checkpoint_corrupt_line_count == 0
                    && index.active.checkpoint_rotation_sequence == 0
                    && index.archive == ActiveVerdictCoverage::default()
                    && index.verdicts.is_empty()
                    && index.terminal_turns.is_empty()
            }
            None => !index_path_existed,
        };
    Ok(checkpoint_absence_is_authoritative)
}

/// Durably delimit an unterminated active-journal tail while holding the
/// stable progress lock. Existing bytes are never truncated or rewritten: a
/// valid JSON value becomes a complete row, while a torn/invalid fragment
/// becomes an explicit corrupt row that scanners count and skip. Either way,
/// the next append cannot concatenate onto ambiguous bytes and wedge startup.
fn finish_unterminated_progress_tail_locked(path: &Path) -> Result<bool> {
    let Some(mut file) = access_optional_progress_state(path, "open", |path| {
        OpenOptions::new().read(true).write(true).open(path)
    })?
    else {
        return Ok(false);
    };
    let len = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if len == 0 {
        return Ok(false);
    }

    file.seek(SeekFrom::End(-1))
        .with_context(|| format!("seek to end of {}", path.display()))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)
        .with_context(|| format!("read tail of {}", path.display()))?;
    if last[0] == b'\n' {
        return Ok(false);
    }

    file.seek(SeekFrom::End(0))
        .with_context(|| format!("seek to append boundary of {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("finish canonical progress tail in {}", path.display()))?;
    file.sync_data()
        .with_context(|| format!("sync canonical progress tail in {}", path.display()))?;
    Ok(true)
}

fn recover_progress_checkpoint_locked(active_path: &Path) -> Result<Option<ProgressCheckpoint>> {
    let archive_path = progress_archive_path(active_path);
    let mut checkpoint = read_progress_checkpoint(active_path)?;
    let archive_state = archive_progress_state(&archive_path)?;
    if checkpoint.is_none() {
        validate_missing_checkpoint_against_index_locked(active_path, archive_state.is_some())?;
    }
    if checkpoint.as_ref().is_some_and(|checkpoint| {
        checkpoint.schema_version == CHECKPOINT_SCHEMA_VERSION
            && checkpoint_covers_archive_state(checkpoint, archive_state.as_ref())
    }) {
        return Ok(checkpoint);
    }
    let archive = archive_coverage_for_path(&archive_path)?;

    if checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.schema_version == 1)
    {
        let mut upgraded = checkpoint.take().expect("checked above");
        if coverage_matches_legacy(upgraded.coverage.as_ref(), archive.as_ref()) {
            if archive.is_some() {
                journal::scan_stream(&archive_path, |_| {})?;
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
                && progress_state_path_is_absent(&progress_checkpoint_path(active_path))?
            {
                write_progress_checkpoint(active_path, checkpoint)?;
            }
        }
        return Ok(checkpoint);
    };
    if checkpoint
        .as_ref()
        .and_then(|checkpoint| checkpoint.coverage.as_ref())
        == Some(&archive)
    {
        return Ok(checkpoint);
    }
    if checkpoint.as_ref().is_some_and(|checkpoint| {
        checkpoint.schema_version == CHECKPOINT_SCHEMA_VERSION
            && checkpoint
                .coverage
                .as_ref()
                .is_some_and(|coverage| coverage.file_identity.is_none())
            && checkpoint_covers_archive(checkpoint, Some(&archive))
    }) {
        let mut enriched = checkpoint.take().expect("checked above");
        enriched.coverage = Some(archive);
        write_progress_checkpoint(active_path, &enriched)?;
        return Ok(Some(enriched));
    }

    let mut next = checkpoint.take().unwrap_or_default();
    let summary = journal::scan_stream(&archive_path, |event| {
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

/// Prove that an absent lifetime checkpoint really means zero folded
/// generations before recovery is allowed to synthesize one (or reconcile
/// derived state against `None`). A cursor-bearing v3/v4 index is the only
/// remaining durable witness after the checkpoint file is lost; treating its
/// history as virgin would silently erase lifetime corruption and rotation
/// counters.
fn validate_missing_checkpoint_against_index_locked(
    active_path: &Path,
    archive_exists: bool,
) -> Result<()> {
    let index_path = progress_verdict_index_path(active_path);
    let Some(bytes) =
        access_optional_progress_state(&index_path, "read", |path| std::fs::read(path))?
    else {
        return Ok(());
    };
    journal::record_raw_read(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    let version =
        serde_json::from_slice::<ProgressVerdictIndexVersion>(&bytes).with_context(|| {
            format!(
                "missing progress checkpoint cannot be validated against {}",
                index_path.display()
            )
        })?;
    if version.schema_version > VERDICT_INDEX_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported progress verdict index schema {} in {}",
            version.schema_version,
            index_path.display()
        );
    }
    let index = serde_json::from_slice::<ProgressVerdictIndex>(&bytes).with_context(|| {
        format!(
            "missing progress checkpoint cannot be validated against {}",
            index_path.display()
        )
    })?;
    let zero_checkpoint_history = index.checkpoint_corrupt_line_count == 0
        && index.active.checkpoint_rotation_sequence == 0
        && index.archive == ActiveVerdictCoverage::default();
    let schema_state_is_safe = match index.schema_version {
        3 => true,
        VERDICT_INDEX_SCHEMA_VERSION => {
            index.verdicts.is_empty() && index.terminal_turns.is_empty()
        }
        _ => false,
    };
    let retained_archive_is_first_generation = !archive_exists
        || (progress_state_path_is_absent(active_path)?
            && index.pending.is_none()
            && progress_cursor_is_exact(&progress_archive_path(active_path), &index.active)?);
    if !zero_checkpoint_history || !schema_state_is_safe || !retained_archive_is_first_generation {
        anyhow::bail!(
            "missing progress checkpoint cannot replace known checkpoint history in {}",
            index_path.display()
        );
    }
    Ok(())
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

fn checkpoint_covers_archive_state(
    checkpoint: &ProgressCheckpoint,
    archive: Option<&(u64, Option<PendingActiveFileIdentity>)>,
) -> bool {
    match (checkpoint.coverage.as_ref(), archive) {
        (None, None) => true,
        (Some(coverage), Some((byte_size, file_identity))) => {
            coverage.byte_size == *byte_size
                && coverage.full_file_sha256.is_some()
                && coverage.file_identity.is_some()
                && coverage.file_identity.as_ref() == file_identity.as_ref()
        }
        _ => false,
    }
}

fn write_progress_checkpoint(active_path: &Path, checkpoint: &ProgressCheckpoint) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(checkpoint).context("serialize progress checkpoint")?;
    atomic_write_durable(&progress_checkpoint_path(active_path), &bytes)
}

fn archive_coverage_for_path(path: &Path) -> Result<Option<ArchiveCoverage>> {
    let Some(mut file) = access_optional_progress_state(path, "open", |path| File::open(path))?
    else {
        return Ok(None);
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    let byte_size = metadata.len();
    let file_identity = pending_active_file_identity(&metadata);
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
    let final_metadata = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?;
    if final_metadata.len() != byte_size
        || pending_active_file_identity(&final_metadata) != file_identity
    {
        anyhow::bail!("archive changed while hashing {}", path.display());
    }
    journal::record_raw_read(byte_size);
    Ok(Some(ArchiveCoverage {
        byte_size,
        first_line_sha256: found_bytes.then(|| hex_digest(first_hasher.finalize().as_slice())),
        full_file_sha256: Some(hex_digest(full_hasher.finalize().as_slice())),
        file_identity,
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
    require_active_progress_lock_locked(&lock_file, active_path)?;
    let checkpoint_absence_is_authoritative =
        settle_pending_progress_write_for_repair_locked(active_path)?;
    repair_progress_journal_locked(
        active_path,
        target_path,
        checkpoint_absence_is_authoritative,
    )
}

fn repair_progress_journal_locked(
    active_path: &Path,
    target_path: &Path,
    checkpoint_absence_is_authoritative: bool,
) -> Result<Option<ProgressRepairReport>> {
    let Some(input) = access_optional_progress_state(target_path, "open", |path| File::open(path))?
    else {
        return Ok(None);
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

    // The replacement inode invalidates the compact cursors immediately. Keep
    // bounded history available when repair returns by reconciling while this
    // caller still owns the stable journal lock. A pre-existing malformed
    // checkpoint still cannot block byte repair, but it must leave the old
    // cursor stale so bounded readers fail closed instead of turning unknown
    // checkpoint quality into a clean zero.
    if let Ok(checkpoint) = read_progress_checkpoint(active_path) {
        if checkpoint.is_some() || checkpoint_absence_is_authoritative {
            // Derived reconciliation is best-effort after the durable replacement:
            // a failure leaves the pre-repair cursor stale, so bounded readers fail
            // closed, while the caller still receives the truthful backup/report.
            let _ = reconcile_verdict_index_locked(active_path, checkpoint.as_ref());
        }
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
        Self::flock(file, libc::LOCK_EX)
    }

    /// Shared (reader) acquisition. Multiple holders coexist; an exclusive
    /// holder still excludes them.
    fn lock_shared(file: &std::fs::File) -> std::io::Result<Self> {
        Self::flock(file, libc::LOCK_SH)
    }

    /// Non-blocking shared acquisition: `Ok(None)` when an exclusive holder
    /// currently owns the lock, so a bounded reader can retry until its own
    /// deadline instead of parking a thread.
    fn try_lock_shared(file: &std::fs::File) -> std::io::Result<Option<Self>> {
        match Self::flock(file, libc::LOCK_SH | libc::LOCK_NB) {
            Ok(lock) => Ok(Some(lock)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn flock(file: &std::fs::File, operation: libc::c_int) -> std::io::Result<Self> {
        let fd = file.as_raw_fd();
        let rc = unsafe { libc::flock(fd, operation) };
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
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "progress state locking is unsupported on this platform",
        ))
    }

    fn lock_shared(_file: &std::fs::File) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "progress state locking is unsupported on this platform",
        ))
    }

    fn try_lock_shared(_file: &std::fs::File) -> std::io::Result<Option<Self>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "progress state locking is unsupported on this platform",
        ))
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

    fn append_raw_verdict_without_index(path: &Path, verdict: &TurnVerdict) -> String {
        let mut event = serde_json::to_value(verdict).unwrap();
        event
            .as_object_mut()
            .unwrap()
            .insert("event".into(), Value::String(TURN_VERDICT.into()));
        let mut line = serde_json::to_vec(&event).unwrap();
        line.push(b'\n');
        let metadata = std::fs::metadata(path).unwrap();
        let active_offset = metadata.len();
        let active_identity = pending_active_file_identity(&metadata).unwrap();
        let line_sha256 = hex_digest(Sha256::digest(&line).as_slice());
        let source_id = projection_source_id(Some(&active_identity), active_offset, &line_sha256);
        let mut active = OpenOptions::new().append(true).open(path).unwrap();
        active.write_all(&line).unwrap();
        active.sync_data().unwrap();
        source_id
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

    fn write_truncated_current_verdict_index(path: &Path) {
        let mut bytes = serde_json::to_vec(&ProgressVerdictIndex::default()).unwrap();
        assert_eq!(bytes.pop(), Some(b'}'));
        std::fs::write(progress_verdict_index_path(path), bytes).unwrap();
    }

    fn write_garbled_current_verdict_index(path: &Path) {
        std::fs::write(
            progress_verdict_index_path(path),
            serde_json::to_vec(&json!({
                "schema_version": VERDICT_INDEX_SCHEMA_VERSION,
                "pending": "garbled",
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn terminal_receipt_writer_rejects_oversized_serialized_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let receipt = TerminalReceipt {
            schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
            sid: "s".repeat(MAX_SPECIAL_RECORD_BYTES as usize),
            turn_id: "turn-1".into(),
            source_id: "source".into(),
            projection: ProjectionRecordLocation {
                offset: 0,
                line_len: 1,
                line_sha256: "f".repeat(64),
            },
        };

        let error = write_terminal_receipt(&path, &receipt)
            .unwrap_err()
            .to_string();

        assert!(error.contains("terminal receipt exceeds storage limit"));
        assert!(!terminal_receipt_root(&path).exists());
    }

    #[test]
    fn verdict_receipt_writer_rejects_oversized_serialized_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let sid = "s".repeat(MAX_SPECIAL_RECORD_BYTES as usize);
        let receipt = VerdictReceipt {
            schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
            sid: sid.clone(),
            turn_id: "turn-1".into(),
            source_id: "source".into(),
            projection: ProjectionRecordLocation {
                offset: 0,
                line_len: 1,
                line_sha256: "f".repeat(64),
            },
            verdict: TurnVerdict {
                sid,
                turn_id: "turn-1".into(),
                ts: "2026-08-28T00:00:00Z".parse().unwrap(),
                verdict: Verdict::Accept,
                feedback: None,
            },
        };

        let error = write_verdict_receipt(&path, &receipt)
            .unwrap_err()
            .to_string();

        assert!(error.contains("verdict receipt exceeds storage limit"));
        assert!(!verdict_receipt_root(&path).exists());
    }

    #[test]
    fn bounded_projection_reader_rejects_a_parseable_unterminated_record() {
        let mut reader = std::io::Cursor::new(br#"{"schema_version":1}"#);

        let error = read_bounded_projection_line(&mut reader, Path::new("projection.jsonl"))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unterminated projection record"));
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
    fn append_event_keeps_legacy_completion_without_turn_id_as_an_ordinary_fact() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "ts": "2026-08-28T00:00:00Z",
            "vendor": "claude",
            "model": "claude-sonnet-4-6",
            "usage": {"output_tokens": 1_000_000},
        });

        append_event(&path, &event).unwrap();

        assert_eq!(read_rows(&path), vec![event.clone()]);
        assert!(progress_cost_contribution(&event).is_some());
        assert!(progress_state_path_is_absent(&progress_terminal_projection_path(&path)).unwrap());
        assert!(progress_state_path_is_absent(&terminal_receipt_root(&path)).unwrap());
        assert!(
            append_chat_turn_completed_if_absent(&path, &event).is_err(),
            "the canonical API must still reject a terminal fact without a full identity"
        );
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
    fn terminal_projection_fails_closed_when_a_lost_index_outlives_retained_raw() {
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
        let projection_path = progress_terminal_projection_path(&path);
        let projection_before = std::fs::read(&projection_path).unwrap();
        std::fs::remove_file(&archive).unwrap();
        std::fs::rename(&path, &archive).unwrap();
        File::create(&path).unwrap();
        std::fs::remove_file(progress_verdict_index_path(&path)).unwrap();

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T02:00:00Z",
            "outcome": "completed",
        });
        let error = append_chat_turn_completed_if_absent(&path, &replay)
            .unwrap_err()
            .to_string();

        assert!(error.contains("terminal bootstrap projection prefix mismatch"));
        assert!(read_rows(&path).is_empty());
        assert_eq!(read_rows(&archive), vec![stale_active]);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
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
                projection: None,
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
                projection: None,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&missing_path, &missing).unwrap();
        let admitted = append_chat_turn_completed_if_absent(&missing_path, &replay).unwrap();
        assert!(admitted.appended);
        assert_eq!(read_rows(&missing_path), vec![replay]);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_pending_index_rejects_exact_bytes_on_a_replacement_inode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
        });
        let mut line = serde_json::to_vec(&event).unwrap();
        line.push(b'\n');
        File::create(&path).unwrap();
        let original_identity =
            pending_active_file_identity(&std::fs::metadata(&path).unwrap()).unwrap();
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event,
                active_offset: 0,
                line_len: line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
                active_file_identity: Some(original_identity.clone()),
                projection: None,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        let replacement = tmp.path().join("replacement.jsonl");
        std::fs::write(&replacement, &line).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_ne!(
            pending_active_file_identity(&std::fs::metadata(&path).unwrap()).unwrap(),
            original_identity
        );

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
        });
        let error = append_chat_turn_completed_if_absent(&path, &replay)
            .unwrap_err()
            .to_string();

        assert!(error.contains("active file identity changed"));
        assert_eq!(std::fs::read(&path).unwrap(), line);
        assert!(read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn terminal_pending_index_recovers_a_landed_projection_before_receipt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
            "outcome": "failed",
        });
        let mut progress_line = serde_json::to_vec(&event).unwrap();
        progress_line.push(b'\n');
        File::create(&path).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let projection_path = progress_terminal_projection_path(&path);
        let projection = projection_append_target(&projection_path).unwrap();
        let line_sha256 = hex_digest(Sha256::digest(&progress_line).as_slice());
        let source_id = projection_source_id(active_file_identity.as_ref(), 0, &line_sha256);
        let projection_line = terminal_projection_line(&source_id, &event).unwrap();
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: event.clone(),
                active_offset: 0,
                line_len: progress_line.len() as u64,
                line_sha256,
                active_file_identity,
                projection: Some(projection.clone()),
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        std::fs::write(&path, &progress_line).unwrap();
        ensure_exact_projection_line(&projection_path, &projection, &projection_line).unwrap();
        assert!(read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
            "outcome": "completed",
        });
        let recovered = append_chat_turn_completed_if_absent(&path, &replay).unwrap();

        assert!(!recovered.appended);
        assert_eq!(recovered.event, event);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_line);
        assert!(read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_some());
        let persisted = read_verdict_index_locked(&path).unwrap().unwrap();
        assert!(persisted.pending.is_none());
        validate_projection_coverage_exact(
            &projection_path,
            &persisted.terminal_projection,
            "terminal",
        )
        .unwrap();
    }

    #[test]
    fn terminal_pending_index_clears_after_projection_and_receipt_are_durable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
            "outcome": "failed",
        });
        let mut progress_line = serde_json::to_vec(&event).unwrap();
        progress_line.push(b'\n');
        File::create(&path).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let projection_path = progress_terminal_projection_path(&path);
        let projection = projection_append_target(&projection_path).unwrap();
        let line_sha256 = hex_digest(Sha256::digest(&progress_line).as_slice());
        let source_id = projection_source_id(active_file_identity.as_ref(), 0, &line_sha256);
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: event.clone(),
                active_offset: 0,
                line_len: progress_line.len() as u64,
                line_sha256,
                active_file_identity,
                projection: Some(projection.clone()),
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        std::fs::write(&path, &progress_line).unwrap();
        apply_terminal_projection_locked(&path, &event, &source_id, Some(&projection)).unwrap();
        let projection_before = std::fs::read(&projection_path).unwrap();
        let receipt_path = special_receipt_path(&terminal_receipt_root(&path), "s1", "turn-1");
        let receipt_before = std::fs::read(&receipt_path).unwrap();

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
            "outcome": "completed",
        });
        let recovered = append_chat_turn_completed_if_absent(&path, &replay).unwrap();

        assert!(!recovered.appended);
        assert_eq!(recovered.event, event);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        assert_eq!(std::fs::read(receipt_path).unwrap(), receipt_before);
        let persisted = read_verdict_index_locked(&path).unwrap().unwrap();
        assert!(persisted.pending.is_none());
        validate_projection_coverage_exact(
            &projection_path,
            &persisted.terminal_projection,
            "terminal",
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn terminal_pending_index_rejects_projection_bytes_on_a_replacement_inode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
            "outcome": "failed",
        });
        let mut progress_line = serde_json::to_vec(&event).unwrap();
        progress_line.push(b'\n');
        File::create(&path).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let projection_path = progress_terminal_projection_path(&path);
        let projection = projection_append_target(&projection_path).unwrap();
        let original_projection_identity = projection.file_identity.clone().unwrap();
        let line_sha256 = hex_digest(Sha256::digest(&progress_line).as_slice());
        let source_id = projection_source_id(active_file_identity.as_ref(), 0, &line_sha256);
        let projection_line = terminal_projection_line(&source_id, &event).unwrap();
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event,
                active_offset: 0,
                line_len: progress_line.len() as u64,
                line_sha256,
                active_file_identity,
                projection: Some(projection),
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        std::fs::write(&path, &progress_line).unwrap();
        let replacement = tmp.path().join("replacement-projection.jsonl");
        std::fs::write(&replacement, &projection_line).unwrap();
        std::fs::rename(&replacement, &projection_path).unwrap();
        assert_ne!(
            pending_active_file_identity(&std::fs::metadata(&projection_path).unwrap()).unwrap(),
            original_projection_identity
        );

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
            "outcome": "completed",
        });
        let error = append_chat_turn_completed_if_absent(&path, &replay)
            .unwrap_err()
            .to_string();

        assert!(error.contains("ambiguous pending projection append"));
        assert!(error.contains("active file identity changed"));
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_line);
        assert!(read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());
        let persisted: ProgressVerdictIndex =
            serde_json::from_slice(&std::fs::read(progress_verdict_index_path(&path)).unwrap())
                .unwrap();
        assert!(persisted.pending.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_pending_index_repairs_an_exact_torn_projection_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
            "outcome": "failed",
        });
        let mut progress_line = serde_json::to_vec(&event).unwrap();
        progress_line.push(b'\n');
        File::create(&path).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let projection_path = progress_terminal_projection_path(&path);
        let projection = projection_append_target(&projection_path).unwrap();
        let line_sha256 = hex_digest(Sha256::digest(&progress_line).as_slice());
        let source_id = projection_source_id(active_file_identity.as_ref(), 0, &line_sha256);
        let projection_line = terminal_projection_line(&source_id, &event).unwrap();
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: event.clone(),
                active_offset: 0,
                line_len: progress_line.len() as u64,
                line_sha256,
                active_file_identity,
                projection: Some(projection),
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        std::fs::write(&path, &progress_line).unwrap();
        std::fs::write(
            &projection_path,
            &projection_line[..projection_line.len() / 2],
        )
        .unwrap();

        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
            "outcome": "completed",
        });
        let recovered = append_chat_turn_completed_if_absent(&path, &replay).unwrap();

        assert!(!recovered.appended);
        assert_eq!(recovered.event, event);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_line);
        assert!(read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_some());
        assert!(read_verdict_index_locked(&path)
            .unwrap()
            .unwrap()
            .pending
            .is_none());
    }

    #[test]
    fn fence_less_torn_projection_fails_closed_without_recovery_authority() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
            "outcome": "completed",
        });
        let mut progress_line = serde_json::to_vec(&event).unwrap();
        progress_line.push(b'\n');
        std::fs::write(&path, progress_line).unwrap();
        let projection_path = progress_terminal_projection_path(&path);
        std::fs::write(
            &projection_path,
            b"{\"schema_version\":1,\"source_id\":\"torn",
        )
        .unwrap();
        let projection_before = std::fs::read(&projection_path).unwrap();

        let error = load_or_recover_progress_checkpoint(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("terminal bootstrap projection prefix mismatch"));
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        assert!(read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn identityless_cursor_never_reuses_a_progress_generation() {
        let cursor = ActiveVerdictCoverage {
            offset: 1024,
            file_identity: None,
            ..ActiveVerdictCoverage::default()
        };

        assert!(!cursor_covers_progress_generation(&cursor, None, 2048));
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
                projection: None,
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
                projection: None,
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
                    projection: None,
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

    #[cfg(unix)]
    #[test]
    fn ordinary_append_resolves_a_torn_verdict_pending_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let existing = json!({"event": "session_started", "sid": "s1"});
        let mut existing_line = serde_json::to_vec(&existing).unwrap();
        existing_line.push(b'\n');
        let verdict = sample_verdict(Verdict::Accept, None, Utc::now());
        let mut verdict_event = serde_json::to_value(&verdict).unwrap();
        verdict_event
            .as_object_mut()
            .unwrap()
            .insert("event".into(), Value::String(TURN_VERDICT.into()));
        let mut verdict_line = serde_json::to_vec(&verdict_event).unwrap();
        verdict_line.push(b'\n');
        let active_offset = existing_line.len() as u64;
        let mut active = existing_line;
        active.extend_from_slice(&verdict_line[..verdict_line.len() / 2]);
        std::fs::write(&path, active).unwrap();
        let active_file_identity = pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let index = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::Verdict {
                verdict,
                active_offset,
                line_len: verdict_line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&verdict_line).as_slice()),
                active_file_identity,
                projection: None,
            }),
            ..ProgressVerdictIndex::default()
        };
        write_verdict_index(&path, &index).unwrap();
        let ordinary = json!({"event": "ordinary_fact", "sid": "s1"});

        append_event(&path, &ordinary).unwrap();

        assert_eq!(read_rows(&path), vec![existing, ordinary]);
    }

    #[test]
    fn ordinary_append_reconciles_an_unterminated_unfenced_tail_before_writing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(Verdict::Accept, None, Utc::now());
        append_turn_verdict_if_changed(&path, &verdict).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"corrupt-unfenced-tail")
            .unwrap();

        append_event(&path, &json!({"event": "ordinary_after_repair"})).unwrap();

        let requested = [verdict.turn_id.clone()].into_iter().collect();
        let read =
            latest_turn_verdicts_for_turns_detailed(&path, &verdict.sid, &requested).unwrap();
        assert_eq!(read.corrupt_line_count, 1);
        assert_eq!(
            read.verdicts
                .get(&(verdict.sid.clone(), verdict.turn_id.clone())),
            Some(&verdict)
        );
    }

    #[test]
    fn corrupt_current_verdict_index_rebuilds_before_ordinary_append() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let existing = json!({"event": "session_started", "sid": "s1"});
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&existing).unwrap()),
        )
        .unwrap();
        write_progress_checkpoint(&path, &ProgressCheckpoint::default()).unwrap();
        write_garbled_current_verdict_index(&path);
        let corrupt_index = std::fs::read(progress_verdict_index_path(&path)).unwrap();
        let ordinary = json!({"event": "ordinary_fact", "sid": "s1"});

        append_event(&path, &ordinary).unwrap();

        assert_eq!(read_rows(&path), vec![existing, ordinary]);
        assert!(read_verdict_index_locked(&path).unwrap().is_some());
        assert_eq!(
            std::fs::read(progress_sibling_path(&path, ".verdicts.corrupt.json")).unwrap(),
            corrupt_index
        );
    }

    #[test]
    fn missing_verdict_index_rebuilds_before_ordinary_append() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let prior = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-0",
        });
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&prior).unwrap()),
        )
        .unwrap();
        let ordinary = json!({"event": "ordinary_fact", "sid": "s1"});

        append_event(&path, &ordinary).unwrap();

        assert_eq!(read_rows(&path), vec![prior.clone(), ordinary]);
        let receipt = read_terminal_receipt(&path, "s1", "turn-0")
            .unwrap()
            .unwrap();
        assert_eq!(terminal_event_from_receipt(&path, &receipt).unwrap(), prior);
    }

    #[test]
    fn corrupt_current_verdict_index_rebuilds_before_terminal_append() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let prior = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-0",
            "ts": "2026-08-28T00:00:00Z",
        });
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&prior).unwrap()),
        )
        .unwrap();
        write_progress_checkpoint(&path, &ProgressCheckpoint::default()).unwrap();
        write_truncated_current_verdict_index(&path);
        let terminal = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
        });

        let admitted = append_chat_turn_completed_if_absent(&path, &terminal).unwrap();

        assert!(admitted.appended);
        assert_eq!(read_rows(&path), vec![prior.clone(), terminal.clone()]);
        let prior_receipt = read_terminal_receipt(&path, "s1", "turn-0")
            .unwrap()
            .unwrap();
        let terminal_receipt = read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            terminal_event_from_receipt(&path, &prior_receipt).unwrap(),
            prior
        );
        assert_eq!(
            terminal_event_from_receipt(&path, &terminal_receipt).unwrap(),
            terminal
        );
    }

    #[test]
    fn corrupt_current_verdict_index_blocks_verdict_append_without_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let first = sample_verdict(
            Verdict::Accept,
            Some("first"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &first).unwrap();
        let progress_before = std::fs::read(&path).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let projection_before = std::fs::read(&projection_path).unwrap();
        write_progress_checkpoint(&path, &ProgressCheckpoint::default()).unwrap();
        write_truncated_current_verdict_index(&path);
        let revised = sample_verdict(
            Verdict::Revise,
            Some("revised"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );

        let error = append_turn_verdict_if_changed(&path, &revised)
            .unwrap_err()
            .to_string();

        assert!(error.contains("verdict projection coverage mismatch"));
        assert_eq!(std::fs::read(&path).unwrap(), progress_before);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
    }

    #[test]
    fn corrupt_index_cannot_discard_legacy_checkpoint_maps() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        File::create(&path).unwrap();
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("rotated away"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let mut checkpoint = ProgressCheckpoint::default();
        checkpoint
            .turn_verdicts
            .entry(verdict.sid.clone())
            .or_default()
            .insert(verdict.turn_id.clone(), verdict);
        write_progress_checkpoint(&path, &checkpoint).unwrap();
        let checkpoint_path = progress_checkpoint_path(&path);
        let checkpoint_before = std::fs::read(&checkpoint_path).unwrap();
        write_truncated_current_verdict_index(&path);
        let index_path = progress_verdict_index_path(&path);
        let index_before = std::fs::read(&index_path).unwrap();

        let error = latest_turn_verdicts_detailed(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("corrupt progress verdict index cannot migrate legacy checkpoint"));
        assert_eq!(std::fs::read(&checkpoint_path).unwrap(), checkpoint_before);
        assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
        assert!(!progress_verdict_projection_path(&path).exists());
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn corrupt_verdict_index_finishes_a_valid_unterminated_active_row() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let existing = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-0",
        });
        std::fs::write(&path, serde_json::to_vec(&existing).unwrap()).unwrap();
        write_progress_checkpoint(&path, &ProgressCheckpoint::default()).unwrap();
        write_truncated_current_verdict_index(&path);
        let ordinary = json!({"event": "ordinary_fact", "sid": "s1"});

        append_event(&path, &ordinary).unwrap();

        assert_eq!(read_rows(&path), vec![existing, ordinary]);
    }

    #[test]
    fn corrupt_verdict_index_delimits_an_ambiguous_unterminated_active_tail() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let active = b"{\"event\":\"session_started\"}\n{\"event\":\"chat_turn_completed\"";
        std::fs::write(&path, active).unwrap();
        write_progress_checkpoint(&path, &ProgressCheckpoint::default()).unwrap();
        write_truncated_current_verdict_index(&path);
        let corrupt_index = std::fs::read(progress_verdict_index_path(&path)).unwrap();
        let ordinary = json!({"event": "ordinary_fact"});

        append_event(&path, &ordinary).unwrap();

        let mut expected = active.to_vec();
        expected.push(b'\n');
        expected.extend_from_slice(&serde_json::to_vec(&ordinary).unwrap());
        expected.push(b'\n');
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert_eq!(
            std::fs::read(progress_sibling_path(&path, ".verdicts.corrupt.json")).unwrap(),
            corrupt_index
        );
    }

    #[test]
    fn missing_verdict_index_delimits_an_ambiguous_unterminated_active_tail() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let active = b"{\"event\":\"session_started\"}\n{corrupt-trailing-line";
        std::fs::write(&path, active).unwrap();

        load_or_recover_progress_checkpoint(&path).unwrap();

        let mut expected = active.to_vec();
        expected.push(b'\n');
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(read_verdict_index_locked(&path).unwrap().is_some());
        assert_eq!(
            latest_turn_verdicts_detailed(&path)
                .unwrap()
                .corrupt_line_count,
            1
        );
    }

    #[test]
    fn verdict_quality_projection_catches_up_only_the_new_active_delta() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        std::fs::write(&path, b"{\"event\":\"session_started\"}\ncorrupt-initial\n").unwrap();
        load_or_recover_progress_checkpoint(&path).unwrap();
        let initial = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(
            initial.active.offset,
            std::fs::metadata(&path).unwrap().len()
        );
        assert_eq!(initial.active.corrupt_line_count, 1);

        let verdict = sample_verdict(
            Verdict::Accept,
            Some("delta"),
            "2026-08-28T02:00:00Z".parse().unwrap(),
        );
        let mut verdict_event = serde_json::to_value(&verdict).unwrap();
        verdict_event
            .as_object_mut()
            .unwrap()
            .insert("event".into(), Value::String(TURN_VERDICT.into()));
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&verdict_event).unwrap()).unwrap();
        writeln!(file, "corrupt-delta").unwrap();
        drop(file);

        let read = latest_turn_verdicts_detailed(&path).unwrap();
        assert_eq!(read.corrupt_line_count, 2);
        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], verdict);
        let caught_up = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(
            caught_up.active.offset,
            std::fs::metadata(&path).unwrap().len()
        );
        assert_eq!(caught_up.active.corrupt_line_count, 2);

        let index_bytes = std::fs::read(progress_verdict_index_path(&path)).unwrap();
        assert_eq!(latest_turn_verdicts_detailed(&path).unwrap(), read);
        assert_eq!(
            std::fs::read(progress_verdict_index_path(&path)).unwrap(),
            index_bytes,
            "an unchanged journal must not rewrite or rescan the compact projection"
        );
    }

    #[test]
    fn stale_raw_cursor_accepts_only_a_fully_landed_projection_delta() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let first = sample_verdict(
            Verdict::Accept,
            Some("first"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &first).unwrap();
        let covered = read_verdict_index_locked(&path).unwrap().unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        validate_projection_coverage_exact(
            &projection_path,
            &covered.verdict_projection,
            "verdict",
        )
        .unwrap();

        let revised = sample_verdict(
            Verdict::Revise,
            Some("fully landed"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &revised);
        apply_verdict_projection_locked(&path, &revised, &source_id, None).unwrap();
        let projection_before = std::fs::read(&projection_path).unwrap();

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], revised);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        let caught_up = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(
            caught_up.active.offset,
            std::fs::metadata(&path).unwrap().len()
        );
        validate_projection_coverage_exact(
            &projection_path,
            &caught_up.verdict_projection,
            "verdict",
        )
        .unwrap();
    }

    #[test]
    fn stale_raw_cursor_rejects_an_unreceipted_projection_gap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let first = sample_verdict(
            Verdict::Accept,
            Some("first"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &first).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let mut gap = first.clone();
        gap.turn_id = "unreceipted-gap".into();
        let gap_line = verdict_projection_line("unreceipted-gap", &gap).unwrap();
        let mut projection = OpenOptions::new()
            .append(true)
            .open(&projection_path)
            .unwrap();
        projection.write_all(&gap_line).unwrap();
        projection.sync_data().unwrap();
        drop(projection);

        let revised = sample_verdict(
            Verdict::Revise,
            Some("landed after gap"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &revised);
        apply_verdict_projection_locked(&path, &revised, &source_id, None).unwrap();
        let projection_before = std::fs::read(&projection_path).unwrap();

        let error = latest_turn_verdicts_detailed(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("projection coverage gap"));
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
    }

    #[test]
    fn stale_raw_cursor_adopts_an_exact_orphan_projection_without_growth() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let first = sample_verdict(
            Verdict::Accept,
            Some("first"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &first).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let revised = sample_verdict(
            Verdict::Revise,
            Some("orphan"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &revised);
        let orphan = verdict_projection_line(&source_id, &revised).unwrap();
        let mut projection = OpenOptions::new()
            .append(true)
            .open(&projection_path)
            .unwrap();
        projection.write_all(&orphan).unwrap();
        projection.sync_data().unwrap();
        drop(projection);
        let projection_before = std::fs::read(&projection_path).unwrap();

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], revised);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_some());
    }

    #[test]
    fn missing_index_bootstrap_adopts_a_projection_before_its_receipt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        File::create(&path).unwrap();
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("bootstrap orphan"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &verdict);
        let projection_path = progress_verdict_projection_path(&path);
        std::fs::write(
            &projection_path,
            verdict_projection_line(&source_id, &verdict).unwrap(),
        )
        .unwrap();
        let projection_before = std::fs::read(&projection_path).unwrap();
        assert!(!progress_verdict_index_path(&path).exists());
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], verdict);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_some());
        let rebuilt = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(rebuilt.schema_version, VERDICT_INDEX_SCHEMA_VERSION);
        assert_eq!(
            rebuilt.active.offset,
            std::fs::metadata(&path).unwrap().len()
        );
        validate_projection_coverage_exact(
            &projection_path,
            &rebuilt.verdict_projection,
            "verdict",
        )
        .unwrap();
    }

    #[test]
    fn missing_index_bootstrap_adopts_a_public_append_after_rotation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("public append"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &verdict).unwrap();
        rotate_progress_locked(&path).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let projection_before = std::fs::read(&projection_path).unwrap();
        std::fs::remove_file(progress_verdict_index_path(&path)).unwrap();

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], verdict);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        let rebuilt = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(
            rebuilt.archive.offset,
            std::fs::metadata(progress_archive_path(&path))
                .unwrap()
                .len()
        );
        validate_projection_coverage_exact(
            &projection_path,
            &rebuilt.verdict_projection,
            "verdict",
        )
        .unwrap();
    }

    #[test]
    fn missing_index_bootstrap_repairs_an_exact_torn_projection_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        File::create(&path).unwrap();
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("bootstrap torn prefix"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &verdict);
        let projection_path = progress_verdict_projection_path(&path);
        let projection = verdict_projection_line(&source_id, &verdict).unwrap();
        std::fs::write(&projection_path, &projection[..projection.len() / 2]).unwrap();
        assert!(!progress_verdict_index_path(&path).exists());
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], verdict);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection);
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_some());
        let rebuilt = read_verdict_index_locked(&path).unwrap().unwrap();
        validate_projection_coverage_exact(
            &projection_path,
            &rebuilt.verdict_projection,
            "verdict",
        )
        .unwrap();
    }

    #[test]
    fn missing_index_bootstrap_rejects_a_wrong_torn_prefix_without_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        File::create(&path).unwrap();
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("authoritative raw"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &verdict);
        let projection_path = progress_verdict_projection_path(&path);
        let expected = verdict_projection_line(&source_id, &verdict).unwrap();
        let mut wrong = expected[..expected.len() / 2].to_vec();
        wrong[0] ^= 1;
        std::fs::write(&projection_path, &wrong).unwrap();

        let error = latest_turn_verdicts_detailed(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("verdict bootstrap projection prefix mismatch"));
        assert_eq!(std::fs::read(&projection_path).unwrap(), wrong);
        assert!(!progress_verdict_index_path(&path).exists());
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn missing_index_bootstrap_adopts_a_projection_and_receipt_before_index_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        File::create(&path).unwrap();
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("bootstrap receipt"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &verdict);
        apply_verdict_projection_locked(&path, &verdict, &source_id, None).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let projection_before = std::fs::read(&projection_path).unwrap();
        let receipt_before = read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .unwrap();
        assert!(!progress_verdict_index_path(&path).exists());

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], verdict);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        assert_eq!(
            read_verdict_receipt(&path, "s1", "turn-1")
                .unwrap()
                .unwrap(),
            receipt_before
        );
        let rebuilt = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(rebuilt.schema_version, VERDICT_INDEX_SCHEMA_VERSION);
        assert_eq!(
            rebuilt.active.offset,
            std::fs::metadata(&path).unwrap().len()
        );
        validate_projection_coverage_exact(
            &projection_path,
            &rebuilt.verdict_projection,
            "verdict",
        )
        .unwrap();
    }

    #[test]
    fn missing_index_bootstrap_rejects_a_wrong_projection_prefix_without_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        File::create(&path).unwrap();
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("authoritative raw"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let source_id = append_raw_verdict_without_index(&path, &verdict);
        let wrong = sample_verdict(
            Verdict::Revise,
            Some("wrong projection"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        let projection_path = progress_verdict_projection_path(&path);
        std::fs::write(
            &projection_path,
            verdict_projection_line(&source_id, &wrong).unwrap(),
        )
        .unwrap();
        let projection_before = std::fs::read(&projection_path).unwrap();

        let error = latest_turn_verdicts_detailed(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("projection"));
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        assert!(!progress_verdict_index_path(&path).exists());
        assert!(read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn legacy_v3_migration_adopts_a_landed_receipt_before_index_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        File::create(&path).unwrap();
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("legacy"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let mut legacy = ProgressVerdictIndex {
            schema_version: 3,
            ..ProgressVerdictIndex::default()
        };
        legacy
            .verdicts
            .entry(verdict.sid.clone())
            .or_default()
            .insert(verdict.turn_id.clone(), verdict.clone());
        legacy.active.file_identity =
            pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        write_verdict_index(&path, &legacy).unwrap();
        let source_id = format!("legacy-index:verdict:{}:{}", verdict.sid, verdict.turn_id);
        apply_verdict_projection_locked(&path, &verdict, &source_id, None).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let projection_before = std::fs::read(&projection_path).unwrap();

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], verdict);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        let migrated = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(migrated.schema_version, VERDICT_INDEX_SCHEMA_VERSION);
        validate_projection_coverage_exact(
            &projection_path,
            &migrated.verdict_projection,
            "verdict",
        )
        .unwrap();
    }

    #[test]
    fn committed_v4_migration_does_not_replay_an_uncleared_legacy_checkpoint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let accepted = sample_verdict(
            Verdict::Accept,
            Some("legacy"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let revised = sample_verdict(
            Verdict::Revise,
            Some("current"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &revised).unwrap();
        let mut stale_checkpoint = ProgressCheckpoint {
            schema_version: 3,
            ..ProgressCheckpoint::default()
        };
        stale_checkpoint
            .turn_verdicts
            .entry(accepted.sid.clone())
            .or_default()
            .insert(accepted.turn_id.clone(), accepted);
        write_progress_checkpoint(&path, &stale_checkpoint).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let projection_before = std::fs::read(&projection_path).unwrap();

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], revised);
        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        let compact = read_progress_checkpoint(&path).unwrap().unwrap();
        assert!(compact.turn_verdicts.is_empty());
        assert_eq!(compact.schema_version, CHECKPOINT_SCHEMA_VERSION);
    }

    #[test]
    fn rotation_transfers_the_fully_projected_active_cursor_without_duplicates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let accepted = sample_verdict(
            Verdict::Accept,
            Some("first"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let revised = sample_verdict(
            Verdict::Revise,
            Some("second"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &accepted).unwrap();
        append_turn_verdict_if_changed(&path, &revised).unwrap();
        let projection_path = progress_verdict_projection_path(&path);
        let projection_before = std::fs::read(&projection_path).unwrap();
        assert_eq!(
            projection_before
                .iter()
                .filter(|byte| **byte == b'\n')
                .count(),
            2
        );

        rotate_progress_locked(&path).unwrap();

        assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
        assert_eq!(
            latest_turn_verdicts(&path).unwrap()[&("s1".into(), "turn-1".into())],
            revised
        );
    }

    #[test]
    fn ordinary_append_leaves_a_valid_verdict_projection_for_lazy_catch_up() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        std::fs::write(&path, b"{\"event\":\"session_started\",\"sid\":\"s1\"}\n").unwrap();
        load_or_recover_progress_checkpoint(&path).unwrap();
        let index_path = progress_verdict_index_path(&path);
        let index_before = std::fs::read(&index_path).unwrap();
        let covered_before = read_verdict_index_locked(&path).unwrap().unwrap();

        append_event(
            &path,
            &json!({"event": "ordinary_fact", "sid": "s1", "value": 1}),
        )
        .unwrap();

        assert_eq!(
            std::fs::read(&index_path).unwrap(),
            index_before,
            "ordinary telemetry must not rewrite the compact verdict projection"
        );
        assert!(covered_before.active.offset < std::fs::metadata(&path).unwrap().len());

        latest_turn_verdicts_detailed(&path).unwrap();
        let caught_up = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(
            caught_up.active.offset,
            std::fs::metadata(&path).unwrap().len()
        );
    }

    #[test]
    fn legacy_v2_verdict_index_with_checkpoint_rebuilds_into_cursor_schema() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("canonical"),
            "2026-08-28T03:00:00Z".parse().unwrap(),
        );
        let mut event = serde_json::to_value(&verdict).unwrap();
        event
            .as_object_mut()
            .unwrap()
            .insert("event".into(), Value::String(TURN_VERDICT.into()));
        std::fs::write(&path, format!("{event}\n")).unwrap();
        let legacy = serde_json::to_vec(&json!({
            "schema_version": 2,
            "verdicts": {},
            "terminal_turns": {},
        }))
        .unwrap();
        std::fs::write(progress_verdict_index_path(&path), &legacy).unwrap();
        write_progress_checkpoint(&path, &ProgressCheckpoint::default()).unwrap();

        let read = latest_turn_verdicts_detailed(&path).unwrap();

        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], verdict);
        let rebuilt = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(rebuilt.schema_version, VERDICT_INDEX_SCHEMA_VERSION);
        assert_eq!(
            rebuilt.active.offset,
            std::fs::metadata(&path).unwrap().len()
        );
        assert!(
            !progress_sibling_path(&path, ".verdicts.corrupt.json").exists(),
            "a supported legacy schema is migrated, not quarantined"
        );
    }

    #[cfg(unix)]
    #[test]
    fn legacy_v2_pending_waits_for_checkpoint_authority_before_cursor_schema_rebuild() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let existing = json!({"event": "session_started", "sid": "s1"});
        let mut existing_line = serde_json::to_vec(&existing).unwrap();
        existing_line.push(b'\n');
        let pending_event = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T00:00:00Z",
        });
        let mut pending_line = serde_json::to_vec(&pending_event).unwrap();
        pending_line.push(b'\n');
        let active_offset = existing_line.len() as u64;
        let mut active = existing_line;
        active.extend_from_slice(&pending_line[..pending_line.len() / 2]);
        std::fs::write(&path, active).unwrap();
        let mut legacy = ProgressVerdictIndex {
            pending: Some(PendingProgressIndexWrite::TerminalTurn {
                event: pending_event,
                active_offset,
                line_len: pending_line.len() as u64,
                line_sha256: hex_digest(Sha256::digest(&pending_line).as_slice()),
                active_file_identity: pending_active_file_identity(
                    &std::fs::metadata(&path).unwrap(),
                ),
                projection: None,
            }),
            ..ProgressVerdictIndex::default()
        };
        legacy.schema_version = 2;
        write_verdict_index(&path, &legacy).unwrap();
        let replay = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "turn-1",
            "ts": "2026-08-28T01:00:00Z",
        });
        let active_before = std::fs::read(&path).unwrap();
        let index_path = progress_verdict_index_path(&path);
        let index_before = std::fs::read(&index_path).unwrap();

        let error = append_chat_turn_completed_if_absent(&path, &replay)
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing progress checkpoint"));
        assert_eq!(std::fs::read(&path).unwrap(), active_before);
        assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
        assert!(!progress_terminal_projection_path(&path).exists());

        write_progress_checkpoint(&path, &ProgressCheckpoint::default()).unwrap();

        let admitted = append_chat_turn_completed_if_absent(&path, &replay).unwrap();

        assert!(admitted.appended);
        assert_eq!(read_rows(&path), vec![existing, replay]);
        assert_eq!(
            latest_turn_verdicts_detailed(&path)
                .unwrap()
                .corrupt_line_count,
            0
        );
    }

    #[test]
    fn pre_cursor_index_without_checkpoint_fails_before_mutation() {
        for schema_version in [1, 2] {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("progress.jsonl");
            let active = b"{\"event\":\"session_started\",\"sid\":\"s1\"}";
            std::fs::write(&path, active).unwrap();
            let index_path = progress_verdict_index_path(&path);
            let legacy_index = serde_json::to_vec(&json!({
                "schema_version": schema_version,
                "verdicts": {},
                "terminal_turns": {},
            }))
            .unwrap();
            std::fs::write(&index_path, &legacy_index).unwrap();

            let error = load_or_recover_progress_checkpoint(&path)
                .unwrap_err()
                .to_string();

            assert!(error.contains("missing progress checkpoint"));
            assert_eq!(std::fs::read(&path).unwrap(), active);
            assert_eq!(std::fs::read(&index_path).unwrap(), legacy_index);
            assert!(!progress_checkpoint_path(&path).exists());
        }
    }

    #[test]
    fn future_verdict_index_schema_blocks_every_writer() {
        for writer in ["ordinary", "terminal", "verdict"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("progress.jsonl");
            let existing = json!({"event": "session_started", "sid": "s1"});
            std::fs::write(
                &path,
                format!("{}\n", serde_json::to_string(&existing).unwrap()),
            )
            .unwrap();
            std::fs::write(
                progress_verdict_index_path(&path),
                serde_json::to_vec(&json!({
                    "schema_version": VERDICT_INDEX_SCHEMA_VERSION + 1,
                    "pending": {"kind": "future_kind", "opaque": true},
                }))
                .unwrap(),
            )
            .unwrap();

            let result: Result<()> = match writer {
                "ordinary" => append_event(&path, &json!({"event": "ordinary_fact"})),
                "terminal" => append_chat_turn_completed_if_absent(
                    &path,
                    &json!({
                        "event": CHAT_TURN_COMPLETED,
                        "sid": "s1",
                        "turn_id": "turn-1",
                    }),
                )
                .map(|_| ()),
                "verdict" => append_turn_verdict_if_changed(
                    &path,
                    &sample_verdict(Verdict::Accept, None, Utc::now()),
                )
                .map(|_| ()),
                _ => unreachable!(),
            };

            let error = result.unwrap_err().to_string();
            assert!(error.contains("unsupported progress verdict index schema"));
            assert_eq!(read_rows(&path), vec![existing]);
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
                projection: None,
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
    fn archive_repair_replays_active_after_the_older_archive_generation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let archive = progress_archive_path(&path);
        let accepted = sample_verdict(
            Verdict::Accept,
            Some("older archive verdict"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        let revised = sample_verdict(
            Verdict::Revise,
            Some("newer active verdict"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &accepted).unwrap();
        let mut active = OpenOptions::new().append(true).open(&path).unwrap();
        active.write_all(b"corrupt archive row\n").unwrap();
        active.sync_data().unwrap();
        drop(active);
        rotate_progress_locked(&path).unwrap();
        append_turn_verdict_if_changed(&path, &revised).unwrap();
        let archive_identity_before =
            pending_active_file_identity(&std::fs::metadata(&archive).unwrap());

        let report = repair_progress_journal(&path, &archive).unwrap().unwrap();

        assert_eq!(report.dropped_count, 1);
        assert_ne!(
            pending_active_file_identity(&std::fs::metadata(&archive).unwrap()),
            archive_identity_before,
            "archive repair must exercise the replacement-generation path"
        );
        let bounded = latest_turn_verdicts_for_turns_detailed(
            &path,
            "s1",
            &BTreeSet::from(["turn-1".to_string()]),
        )
        .unwrap();
        assert_eq!(bounded.verdicts[&("s1".into(), "turn-1".into())], revised);
        let read = latest_turn_verdicts_detailed(&path).unwrap();
        assert_eq!(read.verdicts[&("s1".into(), "turn-1".into())], revised);
        assert_eq!(
            read_verdict_receipt(&path, "s1", "turn-1")
                .unwrap()
                .unwrap()
                .verdict,
            revised
        );
        let index = read_verdict_index_locked(&path).unwrap().unwrap();
        assert!(progress_cursor_is_exact(&archive, &index.archive).unwrap());
        assert!(progress_cursor_is_exact(&path, &index.active).unwrap());
    }

    #[test]
    fn active_repair_with_a_bad_checkpoint_keeps_bounded_history_fail_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("survives active repair"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &verdict).unwrap();
        let checkpoint = ProgressCheckpoint {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            corrupt_line_count: 7,
            rotation_sequence: 3,
            ..ProgressCheckpoint::default()
        };
        write_progress_checkpoint(&path, &checkpoint).unwrap();
        latest_turn_verdicts_detailed(&path).unwrap();
        assert_eq!(
            read_verdict_index_locked(&path)
                .unwrap()
                .unwrap()
                .checkpoint_corrupt_line_count,
            7
        );
        let mut active = OpenOptions::new().append(true).open(&path).unwrap();
        active.write_all(b"corrupt active row").unwrap();
        active.sync_data().unwrap();
        drop(active);
        let active_identity_before =
            pending_active_file_identity(&std::fs::metadata(&path).unwrap());
        let checkpoint_path = progress_checkpoint_path(&path);
        let bad_checkpoint = b"{not-json";
        std::fs::write(&checkpoint_path, bad_checkpoint).unwrap();

        let report = repair_progress_journal(&path, &path).unwrap().unwrap();

        assert_eq!(report.dropped_count, 1);
        assert_ne!(
            pending_active_file_identity(&std::fs::metadata(&path).unwrap()),
            active_identity_before,
            "active repair must exercise the replacement-generation path"
        );
        assert_eq!(std::fs::read(&checkpoint_path).unwrap(), bad_checkpoint);
        let error = latest_turn_verdicts_for_turns_detailed(
            &path,
            "s1",
            &BTreeSet::from(["turn-1".to_string()]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("progress verdict index generation is stale"));
        assert_eq!(
            read_verdict_index_locked(&path)
                .unwrap()
                .unwrap()
                .checkpoint_corrupt_line_count,
            7,
            "unreadable checkpoint quality must not become a clean zero"
        );
    }

    #[test]
    fn active_repair_with_a_lost_checkpoint_preserves_known_quality_fail_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("known quality survives"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &verdict).unwrap();
        let checkpoint = ProgressCheckpoint {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            corrupt_line_count: 7,
            rotation_sequence: 3,
            ..ProgressCheckpoint::default()
        };
        let checkpoint_path = progress_checkpoint_path(&path);
        write_progress_checkpoint(&path, &checkpoint).unwrap();
        latest_turn_verdicts_detailed(&path).unwrap();
        let hydrated = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(hydrated.checkpoint_corrupt_line_count, 7);
        assert_eq!(hydrated.active.checkpoint_rotation_sequence, 3);
        std::fs::remove_file(&checkpoint_path).unwrap();
        let mut active = OpenOptions::new().append(true).open(&path).unwrap();
        active.write_all(b"corrupt active row\n").unwrap();
        active.sync_data().unwrap();
        drop(active);

        let report = repair_progress_journal(&path, &path).unwrap().unwrap();

        assert_eq!(report.dropped_count, 1);
        assert!(!checkpoint_path.exists());
        assert!(latest_turn_verdicts_for_turns_detailed(
            &path,
            "s1",
            &BTreeSet::from(["turn-1".to_string()]),
        )
        .is_err());
        let preserved = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(preserved.checkpoint_corrupt_line_count, 7);
        assert_eq!(preserved.active.checkpoint_rotation_sequence, 3);
        assert!(!progress_cursor_is_exact(&path, &preserved.active).unwrap());
        let index_path = progress_verdict_index_path(&path);
        let index_before_restart = std::fs::read(&index_path).unwrap();

        let error = load_or_recover_progress_checkpoint(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing progress checkpoint"));
        assert!(!checkpoint_path.exists());
        assert_eq!(std::fs::read(&index_path).unwrap(), index_before_restart);
        let after_restart = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(after_restart.checkpoint_corrupt_line_count, 7);
        assert_eq!(after_restart.active.checkpoint_rotation_sequence, 3);
        assert!(latest_turn_verdicts_for_turns_detailed(
            &path,
            "s1",
            &BTreeSet::from(["turn-1".to_string()]),
        )
        .is_err());
    }

    #[test]
    fn active_repair_without_checkpoint_keeps_virgin_bounded_history_available() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("virgin project"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &verdict).unwrap();
        assert!(!progress_checkpoint_path(&path).exists());
        assert!(!progress_archive_path(&path).exists());
        let mut active = OpenOptions::new().append(true).open(&path).unwrap();
        active.write_all(b"corrupt active row\n").unwrap();
        active.sync_data().unwrap();
        drop(active);

        let report = repair_progress_journal(&path, &path).unwrap().unwrap();

        assert_eq!(report.dropped_count, 1);
        let bounded = latest_turn_verdicts_for_turns_detailed(
            &path,
            "s1",
            &BTreeSet::from(["turn-1".to_string()]),
        )
        .unwrap();
        assert_eq!(bounded.corrupt_line_count, 0);
        assert_eq!(bounded.verdicts[&("s1".into(), "turn-1".into())], verdict);
        let index = read_verdict_index_locked(&path).unwrap().unwrap();
        assert!(progress_cursor_is_exact(&path, &index.active).unwrap());
    }

    #[test]
    fn missing_checkpoint_with_a_malformed_existing_index_fails_before_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let active = b"{\"event\":\"session_started\",\"sid\":\"s1\"}";
        std::fs::write(&path, active).unwrap();
        let index_path = progress_verdict_index_path(&path);
        let malformed_index = b"{\"schema_version\":4,\"active\":";
        std::fs::write(&index_path, malformed_index).unwrap();

        let hydration_error = load_or_recover_progress_checkpoint(&path)
            .unwrap_err()
            .to_string();
        let append_error = append_event(&path, &json!({"event": "ordinary_fact"}))
            .unwrap_err()
            .to_string();

        assert!(hydration_error.contains("missing progress checkpoint"));
        assert!(append_error.contains("missing progress checkpoint"));
        assert_eq!(std::fs::read(&path).unwrap(), active);
        assert_eq!(std::fs::read(&index_path).unwrap(), malformed_index);
        assert!(!progress_checkpoint_path(&path).exists());
        assert!(!progress_sibling_path(&path, ".verdicts.corrupt.json").exists());
    }

    #[test]
    fn receipt_only_state_never_collapses_to_a_clean_empty_project() {
        for receipt_kind in ["verdict", "terminal"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("progress.jsonl");
            match receipt_kind {
                "verdict" => {
                    append_turn_verdict_if_changed(
                        &path,
                        &sample_verdict(
                            Verdict::Accept,
                            Some("committed receipt"),
                            "2026-08-28T00:00:00Z".parse().unwrap(),
                        ),
                    )
                    .unwrap();
                    assert!(
                        receipt_root_has_committed_receipt(&verdict_receipt_root(&path)).unwrap()
                    );
                }
                "terminal" => {
                    append_chat_turn_completed_if_absent(
                        &path,
                        &json!({
                            "event": CHAT_TURN_COMPLETED,
                            "sid": "s1",
                            "turn_id": "turn-1",
                            "ts": "2026-08-28T00:00:00Z",
                            "outcome": "completed",
                        }),
                    )
                    .unwrap();
                    assert!(
                        receipt_root_has_committed_receipt(&terminal_receipt_root(&path)).unwrap()
                    );
                }
                _ => unreachable!(),
            }
            for state_path in [
                path.clone(),
                progress_archive_path(&path),
                progress_checkpoint_path(&path),
                progress_verdict_index_path(&path),
                progress_terminal_projection_path(&path),
                progress_verdict_projection_path(&path),
            ] {
                match std::fs::remove_file(&state_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => panic!("remove {}: {error}", state_path.display()),
                }
            }
            let requested = BTreeSet::from(["turn-1".to_string()]);

            assert!(load_or_recover_progress_checkpoint(&path).is_err());
            assert!(latest_turn_verdicts_detailed(&path).is_err());
            assert!(latest_turn_verdicts_for_turns_detailed(&path, "s1", &requested).is_err());
        }
    }

    #[test]
    fn cleanup_progress_state_uses_one_complete_manifest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let manifest = progress_state_manifest(&path);
        for state_path in &manifest {
            if state_path == &terminal_receipt_root(&path)
                || state_path == &verdict_receipt_root(&path)
                || state_path == &durable_tmp_path(&progress_checkpoint_path(&path))
            {
                std::fs::create_dir_all(state_path.join("nested")).unwrap();
                std::fs::write(state_path.join("nested").join("state"), b"stale").unwrap();
            } else {
                std::fs::write(state_path, b"stale").unwrap();
            }
        }

        let preview = cleanup_progress_state(&path, true).unwrap();

        assert_eq!(preview, manifest);
        assert!(manifest
            .iter()
            .all(|state_path| std::fs::symlink_metadata(state_path).is_ok()));
        assert!(!progress_lock_path(&path).exists());

        let removed = cleanup_progress_state(&path, false).unwrap();

        assert_eq!(removed, manifest);
        assert!(manifest
            .iter()
            .all(|state_path| std::fs::symlink_metadata(state_path).is_err()));
        assert!(!progress_state_exists(&path).unwrap());
        assert_eq!(
            std::fs::read(progress_lock_path(&path)).unwrap(),
            PROGRESS_LOCK_RETIRED_MARKER
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_progress_state_waits_for_the_stable_lock_before_first_enumeration() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let lock_file = open_progress_lock(&path).unwrap();
        let writer_lock = ProgressFileLock::lock(&lock_file).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(0);
        let cleanup_path = path.clone();
        let cleanup = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(cleanup_progress_state(&cleanup_path, false))
                .unwrap();
        });
        started_rx.recv().unwrap();

        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(250)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "cleanup returned before the writer released the stable lock"
        );
        std::fs::write(&path, b"published while writer owns the lock\n").unwrap();
        drop(writer_lock);
        drop(lock_file);

        let removed = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cleanup remained blocked after stable-lock release")
            .unwrap();
        cleanup.join().unwrap();
        assert_eq!(removed, vec![path.clone()]);
        assert!(std::fs::symlink_metadata(&path).is_err());
        assert!(progress_lock_path(&path).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_progress_state_fences_a_writer_that_opened_the_lock_before_retirement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        append_event(&path, &json!({"event": "before_cleanup"})).unwrap();

        // Model a writer that opened the stable inode while the project was
        // active, but did not acquire its flock until removal had completed.
        let queued_lock_file = open_progress_lock(&path).unwrap();
        cleanup_progress_state(&path, false).unwrap();

        let queued_result = (|| -> Result<()> {
            let _lock = ProgressFileLock::lock(&queued_lock_file)?;
            require_active_progress_lock_locked(&queued_lock_file, &path)?;
            append_serialized_locked(&path, b"{\"event\":\"late_writer\"}\n")?;
            Ok(())
        })();

        let error = queued_result.unwrap_err().to_string();
        assert!(error.contains("retired"), "{error}");
        assert!(progress_state_is_retired(&path).unwrap());
        assert!(!progress_state_exists(&path).unwrap());
        assert!(std::fs::symlink_metadata(&path).is_err());
        assert!(std::fs::symlink_metadata(progress_verdict_index_path(&path)).is_err());
        assert!(load_or_recover_progress_checkpoint(&path)
            .unwrap_err()
            .to_string()
            .contains("retired"));
        assert!(latest_turn_verdicts_detailed(&path)
            .unwrap_err()
            .to_string()
            .contains("retired"));
    }

    #[test]
    fn retired_progress_cleanup_is_idempotent_and_keeps_the_tombstone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        append_event(&path, &json!({"event": "before_cleanup"})).unwrap();

        mark_progress_retired(&path).unwrap();
        let removed = cleanup_retired_progress_state(&path, false).unwrap();
        let removed_again = cleanup_retired_progress_state(&path, false).unwrap();

        assert!(!removed.is_empty());
        assert!(removed_again.is_empty());
        assert!(progress_state_is_retired(&path).unwrap());
        assert!(progress_lock_path(&path).is_file());
    }

    #[test]
    fn progress_slug_reservation_tracks_ownership_not_bare_lock_inodes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let absent = tmp.path().join("absent").join("alpha.jsonl");
        assert_eq!(
            progress_slug_reservation(&absent).unwrap(),
            ProgressSlugReservation::Free
        );
        assert!(!progress_slug_is_reserved(&absent).unwrap());
        assert!(!absent.parent().unwrap().exists());

        // A bare lock inode is an artifact of readers/probes, not an owner: it
        // must leave the slug free for a fresh generation.
        for (case, marker) in [
            ("bare-legacy", b"".as_slice()),
            ("bare-active", PROGRESS_LOCK_ACTIVE_MARKER.as_slice()),
        ] {
            let path = tmp.path().join(case).join("alpha.jsonl");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(progress_lock_path(&path), marker).unwrap();
            let before = std::fs::read(progress_lock_path(&path)).unwrap();

            assert_eq!(
                progress_slug_reservation(&path).unwrap(),
                ProgressSlugReservation::Free,
                "{case}"
            );
            assert!(!progress_slug_is_reserved(&path).unwrap(), "{case}");
            assert_eq!(std::fs::read(progress_lock_path(&path)).unwrap(), before);
        }

        // The same lock states reserve once their generation still owns state.
        for (case, marker) in [
            ("owning-legacy", b"".as_slice()),
            ("owning-active", PROGRESS_LOCK_ACTIVE_MARKER.as_slice()),
        ] {
            let path = tmp.path().join(case).join("alpha.jsonl");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"{\"event\":\"orphan\"}\n").unwrap();
            std::fs::write(progress_lock_path(&path), marker).unwrap();

            assert_eq!(
                progress_slug_reservation(&path).unwrap(),
                ProgressSlugReservation::ActiveState,
                "{case}"
            );
            assert!(progress_slug_is_reserved(&path).unwrap(), "{case}");
        }

        // State without any lock inode still reserves the slug.
        let lockless = tmp.path().join("lockless").join("alpha.jsonl");
        std::fs::create_dir_all(lockless.parent().unwrap()).unwrap();
        std::fs::write(&lockless, b"{\"event\":\"orphan\"}\n").unwrap();
        assert_eq!(
            progress_slug_reservation(&lockless).unwrap(),
            ProgressSlugReservation::ActiveState
        );

        // A tombstone reserves permanently, with or without surviving state.
        let retired = tmp.path().join("retired").join("alpha.jsonl");
        std::fs::create_dir_all(retired.parent().unwrap()).unwrap();
        std::fs::write(
            progress_lock_path(&retired),
            PROGRESS_LOCK_RETIRED_MARKER.as_slice(),
        )
        .unwrap();
        let before = std::fs::read(progress_lock_path(&retired)).unwrap();
        assert_eq!(
            progress_slug_reservation(&retired).unwrap(),
            ProgressSlugReservation::Retired
        );
        assert!(progress_slug_is_reserved(&retired).unwrap());
        assert_eq!(std::fs::read(progress_lock_path(&retired)).unwrap(), before);

        // Unknown markers stay fail-closed rather than silently freeing a slug.
        let unknown = tmp.path().join("unknown").join("alpha.jsonl");
        std::fs::create_dir_all(unknown.parent().unwrap()).unwrap();
        std::fs::write(progress_lock_path(&unknown), b"future-marker").unwrap();
        assert!(progress_slug_reservation(&unknown).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let unsafe_path = tmp.path().join("unsafe").join("alpha.jsonl");
            std::fs::create_dir_all(unsafe_path.parent().unwrap()).unwrap();
            let outside = tmp.path().join("outside-lock");
            std::fs::write(&outside, b"keep").unwrap();
            symlink(&outside, progress_lock_path(&unsafe_path)).unwrap();
            assert!(progress_slug_is_reserved(&unsafe_path).is_err());
            assert_eq!(std::fs::read(outside).unwrap(), b"keep");
        }
    }

    #[test]
    fn bounded_shared_read_yields_none_behind_an_exclusive_holder_and_a_verdict_once_free() {
        let tmp = tempfile::TempDir::new().unwrap();
        let active = tmp.path().join("state").join("progress").join("demo.jsonl");
        append_event(&active, &json!({"event": "fixture"})).unwrap();

        let writer = open_progress_lock(&active).unwrap();
        let held = ProgressFileLock::lock(&writer).unwrap();
        let started = std::time::Instant::now();
        let verdict = progress_state_is_retired_shared_try(
            &active,
            std::time::Instant::now() + Duration::from_millis(150),
        )
        .unwrap();
        assert_eq!(verdict, None, "an exclusive holder must not be waited out");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the bounded reader must return near its deadline, not park"
        );
        drop(held);

        assert_eq!(
            progress_state_is_retired_shared_try(
                &active,
                std::time::Instant::now() + Duration::from_millis(150),
            )
            .unwrap(),
            Some(false)
        );
        mark_progress_retired(&active).unwrap();
        assert_eq!(
            progress_state_is_retired_shared_try(
                &active,
                std::time::Instant::now() + Duration::from_millis(150),
            )
            .unwrap(),
            Some(true)
        );
        assert_eq!(
            progress_state_is_retired_shared_try(
                &tmp.path()
                    .join("state")
                    .join("progress")
                    .join("absent.jsonl"),
                std::time::Instant::now(),
            )
            .unwrap(),
            Some(false),
            "no lock inode = not retired, no waiting"
        );
    }

    #[test]
    fn shared_retired_reads_report_both_marker_states_and_do_not_serialize() {
        let tmp = tempfile::TempDir::new().unwrap();
        let absent = tmp.path().join("absent").join("alpha.jsonl");
        assert!(!progress_state_is_retired_shared(&absent).unwrap());

        let active = tmp.path().join("active").join("alpha.jsonl");
        append_event(&active, &json!({"event": "live"})).unwrap();
        assert!(!progress_state_is_retired_shared(&active).unwrap());

        let retired = tmp.path().join("retired").join("alpha.jsonl");
        append_event(&retired, &json!({"event": "live"})).unwrap();
        mark_progress_retired(&retired).unwrap();
        assert!(progress_state_is_retired_shared(&retired).unwrap());

        let torn = tmp.path().join("torn").join("alpha.jsonl");
        std::fs::create_dir_all(torn.parent().unwrap()).unwrap();
        std::fs::write(progress_lock_path(&torn), b"torn").unwrap();
        assert!(progress_state_is_retired_shared(&torn)
            .unwrap_err()
            .to_string()
            .contains("progress lock marker"));

        // Hold one shared reader open; a second must not serialize behind it.
        let held = open_existing_progress_lock(&retired, false)
            .unwrap()
            .unwrap();
        let guard = ProgressFileLock::lock_shared(&held).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let probe = retired.clone();
        let reader = std::thread::spawn(move || {
            let _ = tx.send(progress_state_is_retired_shared(&probe));
        });
        let observed = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("a concurrent shared reader must not block behind another shared reader");
        assert!(observed.unwrap());
        drop(guard);
        reader.join().unwrap();
    }

    #[test]
    fn ordinary_progress_operations_fail_closed_on_torn_or_unknown_lock_markers() {
        let tmp = tempfile::TempDir::new().unwrap();
        for (case, marker) in [
            ("torn", b"torn".as_slice()),
            ("unknown", b"CCTEAM_PROGRESS_LOCK:UNKNOWN:V1\n".as_slice()),
        ] {
            let path = tmp.path().join(case).join("alpha.jsonl");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(progress_lock_path(&path), marker).unwrap();

            let query_error = progress_state_is_retired(&path).unwrap_err().to_string();
            assert!(
                query_error.contains("progress lock marker"),
                "{case}: {query_error}"
            );

            let error = append_event(&path, &json!({"event": "must_not_land"}))
                .unwrap_err()
                .to_string();

            assert!(error.contains("progress lock marker"), "{case}: {error}");
            assert!(std::fs::symlink_metadata(&path).is_err());
            assert!(std::fs::symlink_metadata(progress_verdict_index_path(&path)).is_err());

            mark_progress_retired(&path).unwrap();
            assert!(progress_state_is_retired(&path).unwrap());
        }
    }

    #[test]
    fn progress_cleanup_dry_runs_never_rewrite_the_lock_or_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        append_event(&path, &json!({"event": "kept"})).unwrap();
        let active_lock = std::fs::read(progress_lock_path(&path)).unwrap();
        let active_state = std::fs::read(&path).unwrap();

        assert!(cleanup_retired_progress_state(&path, true).is_err());
        assert!(!cleanup_progress_state(&path, true).unwrap().is_empty());
        assert_eq!(
            std::fs::read(progress_lock_path(&path)).unwrap(),
            active_lock
        );
        assert_eq!(std::fs::read(&path).unwrap(), active_state);

        mark_progress_retired(&path).unwrap();
        let retired_lock = std::fs::read(progress_lock_path(&path)).unwrap();
        assert!(!cleanup_retired_progress_state(&path, true)
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read(progress_lock_path(&path)).unwrap(),
            retired_lock
        );
        assert_eq!(std::fs::read(&path).unwrap(), active_state);
    }

    #[cfg(unix)]
    #[test]
    fn retired_progress_cleanup_supports_a_valid_symlinked_parent_directory() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let real_parent = tmp.path().join("progress-real");
        let linked_parent = tmp.path().join("progress");
        std::fs::create_dir(&real_parent).unwrap();
        symlink(&real_parent, &linked_parent).unwrap();
        let path = linked_parent.join("alpha.jsonl");
        append_event(&path, &json!({"event": "before_cleanup"})).unwrap();

        mark_progress_retired(&path).unwrap();
        let removed = cleanup_retired_progress_state(&path, false).unwrap();

        assert!(!removed.is_empty());
        assert!(progress_state_is_retired(&path).unwrap());
        assert!(std::fs::symlink_metadata(&linked_parent)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::symlink_metadata(real_parent.join("alpha.jsonl")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_progress_state_dry_run_validates_existing_lock_without_creating_one() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let absent = tmp.path().join("absent").join("alpha.jsonl");
        assert!(cleanup_progress_state(&absent, true).unwrap().is_empty());
        assert!(!absent.parent().unwrap().exists());
        assert!(!progress_lock_path(&absent).exists());

        for damage in ["symlink", "directory"] {
            let parent = tmp.path().join(damage);
            std::fs::create_dir_all(&parent).unwrap();
            let path = parent.join("alpha.jsonl");
            let lock_path = progress_lock_path(&path);
            let outside = tmp.path().join(format!("outside-{damage}"));
            std::fs::write(&outside, b"keep").unwrap();
            match damage {
                "symlink" => symlink(&outside, &lock_path).unwrap(),
                "directory" => {
                    std::fs::create_dir(&lock_path).unwrap();
                    std::fs::write(lock_path.join("keep"), b"keep").unwrap();
                }
                _ => unreachable!(),
            }

            let error = cleanup_progress_state(&path, true).unwrap_err().to_string();

            assert!(error.contains("progress lock"), "{damage}: {error}");
            assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
            assert!(std::fs::symlink_metadata(&lock_path).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_progress_state_non_dry_rejects_unsafe_lock_with_no_state() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        for damage in ["symlink", "directory"] {
            let parent = tmp.path().join(format!("non-dry-{damage}"));
            std::fs::create_dir_all(&parent).unwrap();
            let path = parent.join("alpha.jsonl");
            let lock_path = progress_lock_path(&path);
            let outside = tmp.path().join(format!("non-dry-outside-{damage}"));
            std::fs::write(&outside, b"keep").unwrap();
            match damage {
                "symlink" => symlink(&outside, &lock_path).unwrap(),
                "directory" => {
                    std::fs::create_dir(&lock_path).unwrap();
                    std::fs::write(lock_path.join("keep"), b"keep").unwrap();
                }
                _ => unreachable!(),
            }

            let error = cleanup_progress_state(&path, false)
                .unwrap_err()
                .to_string();

            assert!(error.contains("progress lock"), "{damage}: {error}");
            assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
            assert!(std::fs::symlink_metadata(&lock_path).is_ok());
            assert!(std::fs::symlink_metadata(&path).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_progress_state_includes_active_and_archive_repair_artifacts_only() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let archive = progress_archive_path(&path);
        let artifacts = vec![
            unique_maintenance_path(&path, "repair-tmp"),
            unique_backup_path(&path),
            unique_maintenance_path(&archive, "repair-tmp"),
            unique_backup_path(&archive),
        ];
        std::fs::write(&artifacts[0], b"active tmp").unwrap();
        std::fs::create_dir_all(artifacts[1].join("nested")).unwrap();
        std::fs::write(artifacts[1].join("nested/state"), b"active backup").unwrap();
        let outside = tmp.path().join("outside-archive-tmp");
        std::fs::write(&outside, b"keep").unwrap();
        symlink(&outside, &artifacts[2]).unwrap();
        std::fs::write(&artifacts[3], b"archive backup").unwrap();
        let unrelated = path.with_file_name("alpha.jsonl.repair-tmp");
        std::fs::write(&unrelated, b"unrelated").unwrap();
        let sibling_slug = path.with_file_name("alpha-other.jsonl.repair-tmp-fixture");
        std::fs::write(&sibling_slug, b"sibling").unwrap();
        let nested_parent = path.with_file_name("nested-lookalikes");
        let nested_lookalike = nested_parent.join("alpha.jsonl.repair-tmp-fixture");
        std::fs::create_dir_all(&nested_parent).unwrap();
        std::fs::write(&nested_lookalike, b"nested").unwrap();
        let mut expected = artifacts.clone();
        expected.sort();

        let preview = cleanup_progress_state(&path, true).unwrap();

        assert_eq!(preview, expected);
        assert!(artifacts
            .iter()
            .all(|artifact| std::fs::symlink_metadata(artifact).is_ok()));
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
        assert_eq!(std::fs::read(&sibling_slug).unwrap(), b"sibling");
        assert_eq!(std::fs::read(&nested_lookalike).unwrap(), b"nested");

        let removed = cleanup_progress_state(&path, false).unwrap();

        assert_eq!(removed, expected);
        assert!(artifacts
            .iter()
            .all(|artifact| std::fs::symlink_metadata(artifact).is_err()));
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated");
        assert_eq!(std::fs::read(&sibling_slug).unwrap(), b"sibling");
        assert_eq!(std::fs::read(&nested_lookalike).unwrap(), b"nested");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_progress_state_unlinks_symlinks_without_following_targets() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let outside_file = tmp.path().join("outside-file");
        let outside_dir = tmp.path().join("outside-dir");
        std::fs::write(&outside_file, b"keep").unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("keep"), b"keep").unwrap();
        let file_link = progress_terminal_projection_path(&path);
        let dir_link = terminal_receipt_root(&path);
        symlink(&outside_file, &file_link).unwrap();
        symlink(&outside_dir, &dir_link).unwrap();

        let removed = cleanup_progress_state(&path, false).unwrap();

        assert_eq!(removed, vec![file_link.clone(), dir_link.clone()]);
        assert!(std::fs::symlink_metadata(file_link).is_err());
        assert!(std::fs::symlink_metadata(dir_link).is_err());
        assert_eq!(std::fs::read(outside_file).unwrap(), b"keep");
        assert_eq!(std::fs::read(outside_dir.join("keep")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_progress_state_rejects_a_symlinked_lock() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress").join("alpha.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"old state").unwrap();
        let outside_lock = tmp.path().join("outside-lock");
        std::fs::write(&outside_lock, b"keep").unwrap();
        symlink(&outside_lock, progress_lock_path(&path)).unwrap();

        let error = cleanup_progress_state(&path, false)
            .unwrap_err()
            .to_string();

        assert!(error.contains("progress lock is a symlink"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"old state");
        assert_eq!(std::fs::read(outside_lock).unwrap(), b"keep");
    }

    #[test]
    fn genuinely_absent_progress_state_keeps_readers_lock_free() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("missing").join("progress.jsonl");
        let requested = BTreeSet::from(["turn-1".to_string()]);

        assert!(load_or_recover_progress_checkpoint(&path)
            .unwrap()
            .is_none());
        assert!(latest_turn_verdicts_detailed(&path)
            .unwrap()
            .verdicts
            .is_empty());
        assert!(
            latest_turn_verdicts_for_turns_detailed(&path, "s1", &requested)
                .unwrap()
                .verdicts
                .is_empty()
        );
        assert!(!progress_lock_path(&path).exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn progress_state_metadata_errors_never_become_clean_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let not_a_directory = tmp.path().join("not-a-directory");
        std::fs::write(&not_a_directory, b"occupied").unwrap();
        let path = not_a_directory.join("progress.jsonl");
        let requested = BTreeSet::from(["turn-1".to_string()]);

        assert!(load_or_recover_progress_checkpoint(&path).is_err());
        assert!(latest_turn_verdicts_detailed(&path).is_err());
        assert!(latest_turn_verdicts_for_turns_detailed(&path, "s1", &requested).is_err());
        assert_eq!(std::fs::read(&not_a_directory).unwrap(), b"occupied");
    }

    #[test]
    fn first_rotation_crash_with_zero_history_index_recovers_missing_checkpoint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("first generation"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &verdict).unwrap();
        let before = read_verdict_index_locked(&path).unwrap().unwrap();
        assert_eq!(before.active.checkpoint_rotation_sequence, 0);
        assert_eq!(before.archive, ActiveVerdictCoverage::default());
        let archive = progress_archive_path(&path);
        std::fs::rename(&path, &archive).unwrap();

        let recovered = load_or_recover_progress_checkpoint(&path).unwrap().unwrap();

        assert_eq!(recovered.rotation_sequence, 1);
        assert!(checkpoint_covers_archive(
            &recovered,
            progress_archive_coverage(&path).unwrap().as_ref(),
        ));
        assert_eq!(
            latest_turn_verdicts(&path).unwrap()[&("s1".into(), "turn-1".into())],
            verdict
        );
    }

    #[test]
    fn missing_checkpoint_with_archive_and_active_is_ambiguous_before_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        append_turn_verdict_if_changed(
            &path,
            &sample_verdict(
                Verdict::Accept,
                Some("archive"),
                "2026-08-28T00:00:00Z".parse().unwrap(),
            ),
        )
        .unwrap();
        let archive = progress_archive_path(&path);
        std::fs::rename(&path, &archive).unwrap();
        let active = b"{\"event\":\"session_started\",\"sid\":\"s2\"}\n";
        std::fs::write(&path, active).unwrap();
        let index_path = progress_verdict_index_path(&path);
        let index_before = std::fs::read(&index_path).unwrap();
        let archive_before = std::fs::read(&archive).unwrap();

        let error = load_or_recover_progress_checkpoint(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing progress checkpoint"));
        assert_eq!(std::fs::read(&path).unwrap(), active);
        assert_eq!(std::fs::read(&archive).unwrap(), archive_before);
        assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
        assert!(!progress_checkpoint_path(&path).exists());
    }

    #[test]
    fn missing_checkpoint_with_archive_and_pending_index_is_ambiguous_before_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        append_turn_verdict_if_changed(
            &path,
            &sample_verdict(
                Verdict::Accept,
                Some("archive"),
                "2026-08-28T00:00:00Z".parse().unwrap(),
            ),
        )
        .unwrap();
        let archive = progress_archive_path(&path);
        std::fs::rename(&path, &archive).unwrap();
        let pending_verdict = sample_verdict(
            Verdict::Revise,
            Some("pending"),
            "2026-08-28T01:00:00Z".parse().unwrap(),
        );
        let mut event = serde_json::to_value(&pending_verdict).unwrap();
        event
            .as_object_mut()
            .unwrap()
            .insert("event".into(), Value::String(TURN_VERDICT.into()));
        let mut line = serde_json::to_vec(&event).unwrap();
        line.push(b'\n');
        let mut index = read_verdict_index_locked(&path).unwrap().unwrap();
        index.pending = Some(PendingProgressIndexWrite::Verdict {
            verdict: pending_verdict,
            active_offset: 0,
            line_len: u64::try_from(line.len()).unwrap(),
            line_sha256: hex_digest(Sha256::digest(&line).as_slice()),
            active_file_identity: None,
            projection: None,
        });
        write_verdict_index(&path, &index).unwrap();
        let index_path = progress_verdict_index_path(&path);
        let index_before = std::fs::read(&index_path).unwrap();
        let archive_before = std::fs::read(&archive).unwrap();

        let error = load_or_recover_progress_checkpoint(&path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing progress checkpoint"));
        assert!(!path.exists());
        assert_eq!(std::fs::read(&archive).unwrap(), archive_before);
        assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
        assert!(!progress_checkpoint_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn active_symlink_loop_never_authorizes_first_rotation_recovery_before_a_writer() {
        for writer in ["ordinary", "verdict"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("progress.jsonl");
            append_turn_verdict_if_changed(
                &path,
                &sample_verdict(
                    Verdict::Accept,
                    Some("archive"),
                    "2026-08-28T00:00:00Z".parse().unwrap(),
                ),
            )
            .unwrap();
            let archive = progress_archive_path(&path);
            std::fs::rename(&path, &archive).unwrap();
            std::os::unix::fs::symlink("progress.jsonl", &path).unwrap();
            let index_path = progress_verdict_index_path(&path);
            let index_before = std::fs::read(&index_path).unwrap();
            let archive_before = std::fs::read(&archive).unwrap();

            let result = match writer {
                "ordinary" => append_event(&path, &json!({"event": "ordinary_fact"})),
                "verdict" => append_turn_verdict_if_changed(
                    &path,
                    &sample_verdict(
                        Verdict::Revise,
                        Some("must not land"),
                        "2026-08-28T01:00:00Z".parse().unwrap(),
                    ),
                )
                .map(drop),
                _ => unreachable!(),
            };

            assert!(result.is_err());
            assert!(!progress_checkpoint_path(&path).exists());
            assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
            assert_eq!(std::fs::read(&archive).unwrap(), archive_before);
            assert!(std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_state_directory_never_turns_populated_state_into_clean_empty() {
        for operation in ["startup", "global", "bounded", "ordinary"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let state_dir = tmp.path().join("state");
            let path = state_dir.join("progress.jsonl");
            append_turn_verdict_if_changed(
                &path,
                &sample_verdict(
                    Verdict::Accept,
                    Some("hidden canonical state"),
                    "2026-08-28T00:00:00Z".parse().unwrap(),
                ),
            )
            .unwrap();
            let hidden_dir = tmp.path().join("state-real");
            std::fs::rename(&state_dir, &hidden_dir).unwrap();
            let missing_target = tmp.path().join("missing-state");
            std::os::unix::fs::symlink(&missing_target, &state_dir).unwrap();
            let hidden_path = hidden_dir.join("progress.jsonl");
            let hidden_index = progress_verdict_index_path(&hidden_path);
            let hidden_projection = progress_verdict_projection_path(&hidden_path);
            let active_before = std::fs::read(&hidden_path).unwrap();
            let index_before = std::fs::read(&hidden_index).unwrap();
            let projection_before = std::fs::read(&hidden_projection).unwrap();

            let result = match operation {
                "startup" => load_or_recover_progress_checkpoint(&path).map(drop),
                "global" => latest_turn_verdicts_detailed(&path).map(drop),
                "bounded" => latest_turn_verdicts_for_turns_detailed(
                    &path,
                    "s1",
                    &BTreeSet::from(["turn-1".to_string()]),
                )
                .map(drop),
                "ordinary" => append_event(&path, &json!({"event": "ordinary_fact"})),
                _ => unreachable!(),
            };

            assert!(result.is_err(), "{operation} unexpectedly succeeded");
            assert!(!missing_target.exists());
            assert!(std::fs::symlink_metadata(&state_dir)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read(&hidden_path).unwrap(), active_before);
            assert_eq!(std::fs::read(&hidden_index).unwrap(), index_before);
            assert_eq!(
                std::fs::read(&hidden_projection).unwrap(),
                projection_before
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn valid_symlinked_state_directory_preserves_authoritative_absence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("state-real");
        std::fs::create_dir(&real).unwrap();
        let linked = tmp.path().join("state");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let path = linked.join("progress.jsonl");

        assert!(load_or_recover_progress_checkpoint(&path)
            .unwrap()
            .is_none());
        assert!(!progress_lock_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_checkpoint_or_index_never_authorizes_first_rotation_recovery() {
        for dangling_state in ["checkpoint", "index"] {
            for operation in ["startup", "ordinary"] {
                let tmp = tempfile::TempDir::new().unwrap();
                let path = tmp.path().join("progress.jsonl");
                append_turn_verdict_if_changed(
                    &path,
                    &sample_verdict(
                        Verdict::Accept,
                        Some("archive"),
                        "2026-08-28T00:00:00Z".parse().unwrap(),
                    ),
                )
                .unwrap();
                let archive = progress_archive_path(&path);
                std::fs::rename(&path, &archive).unwrap();
                let checkpoint_path = progress_checkpoint_path(&path);
                let index_path = progress_verdict_index_path(&path);
                let dangling_path = match dangling_state {
                    "checkpoint" => &checkpoint_path,
                    "index" => {
                        std::fs::remove_file(&index_path).unwrap();
                        &index_path
                    }
                    _ => unreachable!(),
                };
                let dangling_target = format!("missing-{dangling_state}.json");
                std::os::unix::fs::symlink(&dangling_target, dangling_path).unwrap();
                let archive_before = std::fs::read(&archive).unwrap();
                let regular_index_before =
                    (dangling_state == "checkpoint").then(|| std::fs::read(&index_path).unwrap());

                let result = match operation {
                    "startup" => load_or_recover_progress_checkpoint(&path).map(drop),
                    "ordinary" => append_event(&path, &json!({"event": "ordinary_fact"})),
                    _ => unreachable!(),
                };

                assert!(result.is_err());
                assert!(!path.exists());
                assert_eq!(std::fs::read(&archive).unwrap(), archive_before);
                assert_eq!(
                    std::fs::read_link(dangling_path).unwrap(),
                    Path::new(&dangling_target)
                );
                if let Some(index_before) = regular_index_before {
                    assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
                }
                let other_state_path = if dangling_state == "checkpoint" {
                    &index_path
                } else {
                    &checkpoint_path
                };
                if dangling_state == "index" {
                    assert!(progress_state_path_is_absent(other_state_path).unwrap());
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn missing_or_dangling_active_never_discards_hydrated_quality_coverage() {
        for active_state in ["missing", "dangling"] {
            for operation in ["global", "ordinary"] {
                let tmp = tempfile::TempDir::new().unwrap();
                let path = tmp.path().join("progress.jsonl");
                append_turn_verdict_if_changed(
                    &path,
                    &sample_verdict(
                        Verdict::Accept,
                        Some("canonical"),
                        "2026-08-28T00:00:00Z".parse().unwrap(),
                    ),
                )
                .unwrap();
                let mut active = OpenOptions::new().append(true).open(&path).unwrap();
                active.write_all(b"corrupt row\n").unwrap();
                active.sync_data().unwrap();
                drop(active);
                assert_eq!(
                    latest_turn_verdicts_detailed(&path)
                        .unwrap()
                        .corrupt_line_count,
                    1
                );
                let index_path = progress_verdict_index_path(&path);
                let projection_path = progress_verdict_projection_path(&path);
                let index_before = std::fs::read(&index_path).unwrap();
                let projection_before = std::fs::read(&projection_path).unwrap();
                std::fs::remove_file(&path).unwrap();
                let dangling_target = tmp.path().join("missing-active-target.jsonl");
                if active_state == "dangling" {
                    std::os::unix::fs::symlink(&dangling_target, &path).unwrap();
                }

                let result = match operation {
                    "global" => latest_turn_verdicts_detailed(&path).map(drop),
                    "ordinary" => append_event(&path, &json!({"event": "ordinary_fact"})),
                    _ => unreachable!(),
                };

                assert!(
                    result.is_err(),
                    "{active_state}/{operation} unexpectedly succeeded"
                );
                assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
                assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
                assert!(!dangling_target.exists());
                if active_state == "dangling" {
                    assert!(std::fs::symlink_metadata(&path)
                        .unwrap()
                        .file_type()
                        .is_symlink());
                } else {
                    assert!(progress_state_path_is_absent(&path).unwrap());
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_archive_never_authorizes_clean_recovery_or_append() {
        for operation in ["startup", "ordinary"] {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("progress.jsonl");
            append_turn_verdict_if_changed(
                &path,
                &sample_verdict(
                    Verdict::Accept,
                    Some("canonical"),
                    "2026-08-28T00:00:00Z".parse().unwrap(),
                ),
            )
            .unwrap();
            let archive = progress_archive_path(&path);
            std::fs::rename(&path, &archive).unwrap();
            std::fs::remove_file(&archive).unwrap();
            let dangling_target = tmp.path().join("missing-archive-target.jsonl");
            std::os::unix::fs::symlink(&dangling_target, &archive).unwrap();
            let index_path = progress_verdict_index_path(&path);
            let projection_path = progress_verdict_projection_path(&path);
            let index_before = std::fs::read(&index_path).unwrap();
            let projection_before = std::fs::read(&projection_path).unwrap();

            let result = match operation {
                "startup" => load_or_recover_progress_checkpoint(&path).map(drop),
                "ordinary" => append_event(&path, &json!({"event": "ordinary_fact"})),
                _ => unreachable!(),
            };

            assert!(result.is_err());
            assert!(progress_state_path_is_absent(&path).unwrap());
            assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
            assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
            assert!(!dangling_target.exists());
            assert!(std::fs::symlink_metadata(&archive)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(progress_state_path_is_absent(&progress_checkpoint_path(&path)).unwrap());
        }
    }

    #[test]
    fn receipt_integrity_damage_blocks_terminal_and_verdict_retries_before_mutation() {
        for kind in ["terminal", "verdict"] {
            for damage in ["receipt_missing", "integrity_missing", "integrity_mismatch"] {
                let tmp = tempfile::TempDir::new().unwrap();
                let path = tmp.path().join("progress.jsonl");
                let (root, sid, turn_id) = if kind == "terminal" {
                    let event = json!({
                        "event": CHAT_TURN_COMPLETED,
                        "sid": "s1",
                        "turn_id": "turn-1",
                        "ts": "2026-08-28T00:00:00Z",
                        "outcome": "first",
                    });
                    append_chat_turn_completed_if_absent(&path, &event).unwrap();
                    (terminal_receipt_root(&path), "s1", "turn-1")
                } else {
                    append_turn_verdict_if_changed(
                        &path,
                        &sample_verdict(
                            Verdict::Accept,
                            Some("first"),
                            "2026-08-28T00:00:00Z".parse().unwrap(),
                        ),
                    )
                    .unwrap();
                    (verdict_receipt_root(&path), "s1", "turn-1")
                };
                let receipt_path = special_receipt_path(&root, sid, turn_id);
                let integrity_path = receipt_integrity_path(&receipt_path);
                match damage {
                    "receipt_missing" => std::fs::remove_file(&receipt_path).unwrap(),
                    "integrity_missing" => std::fs::remove_file(&integrity_path).unwrap(),
                    "integrity_mismatch" => {
                        std::fs::write(&integrity_path, b"0".repeat(64)).unwrap()
                    }
                    _ => unreachable!(),
                }
                let active_before = std::fs::read(&path).unwrap();
                let index_path = progress_verdict_index_path(&path);
                let index_before = std::fs::read(&index_path).unwrap();
                let projection_path = if kind == "terminal" {
                    progress_terminal_projection_path(&path)
                } else {
                    progress_verdict_projection_path(&path)
                };
                let projection_before = std::fs::read(&projection_path).unwrap();

                let result = if kind == "terminal" {
                    append_chat_turn_completed_if_absent(
                        &path,
                        &json!({
                            "event": CHAT_TURN_COMPLETED,
                            "sid": "s1",
                            "turn_id": "turn-1",
                            "ts": "2026-08-28T01:00:00Z",
                            "outcome": "second",
                        }),
                    )
                    .map(drop)
                } else {
                    append_turn_verdict_if_changed(
                        &path,
                        &sample_verdict(
                            Verdict::Accept,
                            Some("first"),
                            "2026-08-28T01:00:00Z".parse().unwrap(),
                        ),
                    )
                    .map(drop)
                };

                assert!(result.is_err(), "{kind}/{damage} unexpectedly succeeded");
                assert_eq!(std::fs::read(&path).unwrap(), active_before);
                assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
                assert_eq!(std::fs::read(&projection_path).unwrap(), projection_before);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn malformed_receipt_hierarchy_never_bootstraps_clean_state() {
        for damage in [
            "root_symlink",
            "shard_file",
            "leaf_directory",
            "leaf_symlink",
        ] {
            for operation in ["global", "ordinary"] {
                let tmp = tempfile::TempDir::new().unwrap();
                let path = tmp.path().join("progress.jsonl");
                let root = verdict_receipt_root(&path);
                match damage {
                    "root_symlink" => {
                        std::os::unix::fs::symlink("missing-receipt-root", &root).unwrap();
                    }
                    "shard_file" => {
                        std::fs::create_dir_all(&root).unwrap();
                        std::fs::write(root.join("aa"), b"not a shard").unwrap();
                    }
                    "leaf_directory" => {
                        std::fs::create_dir_all(root.join("aa/receipt.json")).unwrap();
                    }
                    "leaf_symlink" => {
                        std::fs::create_dir_all(root.join("aa")).unwrap();
                        std::os::unix::fs::symlink(
                            "missing-receipt.json",
                            root.join("aa/receipt.json"),
                        )
                        .unwrap();
                    }
                    _ => unreachable!(),
                }

                let result = match operation {
                    "global" => latest_turn_verdicts_detailed(&path).map(drop),
                    "ordinary" => append_event(&path, &json!({"event": "ordinary_fact"})),
                    _ => unreachable!(),
                };

                assert!(
                    result.is_err(),
                    "{damage}/{operation} unexpectedly succeeded"
                );
                assert!(progress_state_path_is_absent(&path).unwrap());
                assert!(
                    progress_state_path_is_absent(&progress_verdict_index_path(&path)).unwrap()
                );
                assert!(
                    progress_state_path_is_absent(&progress_verdict_projection_path(&path))
                        .unwrap()
                );
                assert!(
                    progress_state_path_is_absent(&progress_terminal_projection_path(&path))
                        .unwrap()
                );
            }
        }
    }

    #[test]
    fn repair_with_a_bad_checkpoint_does_not_require_a_valid_verdict_index() {
        for malformed_index in [false, true] {
            let tmp = tempfile::TempDir::new().unwrap();
            let path = tmp.path().join("progress.jsonl");
            let verdict = sample_verdict(
                Verdict::Accept,
                Some("canonical row survives"),
                "2026-08-28T00:00:00Z".parse().unwrap(),
            );
            let mut event = serde_json::to_value(&verdict).unwrap();
            event
                .as_object_mut()
                .unwrap()
                .insert("event".into(), Value::String(TURN_VERDICT.into()));
            let mut original = serde_json::to_vec(&event).unwrap();
            original.push(b'\n');
            original.extend_from_slice(b"corrupt active row\n");
            std::fs::write(&path, &original).unwrap();
            let checkpoint_path = progress_checkpoint_path(&path);
            let bad_checkpoint = b"{not-json";
            std::fs::write(&checkpoint_path, bad_checkpoint).unwrap();
            let index_path = progress_verdict_index_path(&path);
            let bad_index = b"{not-an-index";
            if malformed_index {
                std::fs::write(&index_path, bad_index).unwrap();
            }

            let report = repair_progress_journal(&path, &path).unwrap().unwrap();

            assert_eq!(report.kept_count, 1);
            assert_eq!(report.dropped_count, 1);
            assert_eq!(std::fs::read(&report.backup_path).unwrap(), original);
            assert_eq!(read_rows(&path), vec![event]);
            assert_eq!(std::fs::read(&checkpoint_path).unwrap(), bad_checkpoint);
            if malformed_index {
                assert_eq!(std::fs::read(&index_path).unwrap(), bad_index);
            } else {
                assert!(!index_path.exists());
            }
            assert!(latest_turn_verdicts_for_turns_detailed(
                &path,
                "s1",
                &BTreeSet::from(["turn-1".to_string()]),
            )
            .is_err());
        }
    }

    #[test]
    fn repair_report_survives_a_post_repair_projection_reconcile_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let verdict = sample_verdict(
            Verdict::Accept,
            Some("canonical row survives"),
            "2026-08-28T00:00:00Z".parse().unwrap(),
        );
        append_turn_verdict_if_changed(&path, &verdict).unwrap();
        let mut active = OpenOptions::new().append(true).open(&path).unwrap();
        active.write_all(b"corrupt active row\n").unwrap();
        active.sync_data().unwrap();
        drop(active);
        std::fs::write(progress_verdict_projection_path(&path), b"broken\n").unwrap();

        let report = repair_progress_journal(&path, &path).unwrap().unwrap();

        assert_eq!(report.dropped_count, 1);
        assert_eq!(read_rows(&path).len(), 1);
        assert!(latest_turn_verdicts_for_turns_detailed(
            &path,
            "s1",
            &BTreeSet::from(["turn-1".to_string()]),
        )
        .is_err());
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
    fn verdict_receipt_rejects_a_payload_for_a_different_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let mut verdict = sample_verdict(Verdict::Accept, None, Utc::now());
        verdict.sid = "s2".into();
        let receipt = VerdictReceipt {
            schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
            sid: "s1".into(),
            turn_id: "turn-1".into(),
            source_id: "fixture".into(),
            projection: ProjectionRecordLocation {
                offset: 0,
                line_len: 1,
                line_sha256: "00".into(),
            },
            verdict,
        };
        write_verdict_receipt(&path, &receipt).unwrap();

        let error = read_verdict_receipt(&path, "s1", "turn-1")
            .unwrap_err()
            .to_string();

        assert!(error.contains("verdict receipt payload key mismatch"));
    }

    #[test]
    fn terminal_receipt_rejects_an_oversized_projection_claim_before_allocation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("progress.jsonl");
        let receipt = TerminalReceipt {
            schema_version: SPECIAL_PROJECTION_SCHEMA_VERSION,
            sid: "s1".into(),
            turn_id: "turn-1".into(),
            source_id: "fixture".into(),
            projection: ProjectionRecordLocation {
                offset: 0,
                line_len: u64::MAX,
                line_sha256: "00".into(),
            },
        };
        write_terminal_receipt(&path, &receipt).unwrap();
        let receipt = read_terminal_receipt(&path, "s1", "turn-1")
            .unwrap()
            .unwrap();

        let error = terminal_event_from_receipt(&path, &receipt)
            .unwrap_err()
            .to_string();

        assert!(error.contains("projection record exceeds bounded lookup limit"));
    }

    #[test]
    fn oversized_special_records_fail_before_any_canonical_mutation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let oversized_feedback_path = tmp.path().join("oversized-feedback.jsonl");
        let oversized_feedback = TurnVerdict {
            feedback: Some("x".repeat(70 * 1024)),
            ..sample_verdict(Verdict::Accept, None, Utc::now())
        };
        assert!(
            append_turn_verdict_if_changed(&oversized_feedback_path, &oversized_feedback)
                .unwrap_err()
                .to_string()
                .contains("storage limit")
        );
        assert!(!oversized_feedback_path.exists());
        assert!(!progress_verdict_index_path(&oversized_feedback_path).exists());
        assert!(!progress_verdict_projection_path(&oversized_feedback_path).exists());

        let oversized_verdict_id_path = tmp.path().join("oversized-verdict-id.jsonl");
        let oversized_verdict_id = TurnVerdict {
            turn_id: "t".repeat(70 * 1024),
            ..sample_verdict(Verdict::Accept, None, Utc::now())
        };
        assert!(
            append_turn_verdict_if_changed(&oversized_verdict_id_path, &oversized_verdict_id)
                .unwrap_err()
                .to_string()
                .contains("storage limit")
        );
        assert!(!oversized_verdict_id_path.exists());

        let oversized_terminal_id_path = tmp.path().join("oversized-terminal-id.jsonl");
        let oversized_terminal = json!({
            "event": CHAT_TURN_COMPLETED,
            "sid": "s1",
            "turn_id": "t".repeat(70 * 1024),
            "ts": "2026-08-28T00:00:00Z",
        });
        assert!(append_chat_turn_completed_if_absent(
            &oversized_terminal_id_path,
            &oversized_terminal,
        )
        .unwrap_err()
        .to_string()
        .contains("storage limit"));
        assert!(!oversized_terminal_id_path.exists());
        assert!(!progress_verdict_index_path(&oversized_terminal_id_path).exists());
        assert!(!progress_terminal_projection_path(&oversized_terminal_id_path).exists());
    }

    #[test]
    fn global_analytics_fail_when_a_committed_projection_is_deleted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let verdict_path = tmp.path().join("verdict.jsonl");
        append_turn_verdict_if_changed(
            &verdict_path,
            &sample_verdict(Verdict::Accept, None, Utc::now()),
        )
        .unwrap();
        std::fs::remove_file(progress_verdict_projection_path(&verdict_path)).unwrap();
        assert!(latest_turn_verdicts(&verdict_path)
            .unwrap_err()
            .to_string()
            .contains("verdict projection coverage mismatch"));

        let terminal_path = tmp.path().join("terminal.jsonl");
        append_chat_turn_completed_if_absent(
            &terminal_path,
            &json!({
                "event": CHAT_TURN_COMPLETED,
                "sid": "s1",
                "turn_id": "turn-1",
                "ts": "2026-08-28T00:00:00Z",
            }),
        )
        .unwrap();
        std::fs::remove_file(progress_terminal_projection_path(&terminal_path)).unwrap();
        assert!(terminal_turns_for_rebuild(&terminal_path)
            .unwrap_err()
            .to_string()
            .contains("terminal projection coverage mismatch"));
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
