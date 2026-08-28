//! Durable pending-turn queue for cold/resume windows.
//!
//! The queue has one source of truth: an atomically replaced file beside the
//! session transcript. A row is claimed by a durable `Pending -> Dispatching`
//! transition and is removed only after the adapter acknowledges acceptance.
//! A surviving `Dispatching` row is an unknown outcome and is never retried.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::fd::AsRawFd;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// One drain worker retries a proven pre-dispatch rejection at most this many
/// times. The row stays queued after the budget is exhausted and a later
/// explicit drain may try again.
pub const MAX_RETRIES_PER_DRAIN: u32 = 3;
const RETRY_BASE_MS: i64 = 100;
const RETRY_MAX_MS: i64 = 1_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Durable dispatch state of one pending turn.
pub enum PendingTurnStatus {
    /// Ready for a proven-safe dispatch attempt.
    Pending,
    /// The dispatch boundary may have been crossed. This is a durable crash
    /// fence, not a synonym for failure.
    Dispatching,
}

/// One queued user turn waiting for the session to become live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingTurn {
    /// Session-local monotonic row id (`p{n}`).
    pub id: String,
    /// User text payload (not a directive — directives are not queued).
    pub text: String,
    /// ISO-8601 enqueue time (diagnostic only).
    pub enqueued_at: String,
    /// Optional origin channel tag (`im` / `web` / `mcp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Bypass vendor slash-directive parsing when the row is submitted.
    #[serde(default)]
    pub literal: bool,
    /// Whether ccteam, rather than a human, authored this row.
    #[serde(default)]
    pub internal: bool,
    /// A delegated completion must still reach the parent IM thread.
    #[serde(default)]
    pub delegation_completion: bool,
    /// Durable dispatch state.
    pub status: PendingTurnStatus,
    #[serde(default)]
    /// Proven pre-dispatch rejections recorded for bounded retry/backoff.
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Earliest safe retry instant after a proven pre-dispatch rejection.
    pub retry_at: Option<DateTime<Utc>>,
    /// Human/operator diagnostic. For `Dispatching`, this explicitly says the
    /// outcome is unknown and automatic retry is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_note: Option<String>,
}

impl PendingTurn {
    fn claim(&mut self, now: DateTime<Utc>) -> Result<()> {
        if self.status != PendingTurnStatus::Pending {
            return Err(anyhow!("pending turn {} is already dispatching", self.id));
        }
        if self.retry_at.is_some_and(|retry_at| retry_at > now) {
            return Err(anyhow!("pending turn {} is not ready for retry", self.id));
        }
        self.status = PendingTurnStatus::Dispatching;
        self.retry_at = None;
        self.outcome_note = Some(
            "Отправка начата; при аварийном перезапуске автоматический повтор отключён".to_string(),
        );
        Ok(())
    }

    fn retry_after_proven_rejection(
        &mut self,
        now: DateTime<Utc>,
        reason: String,
    ) -> Result<DateTime<Utc>> {
        if self.status != PendingTurnStatus::Dispatching {
            return Err(anyhow!("pending turn {} is not dispatching", self.id));
        }
        self.attempts = self.attempts.saturating_add(1);
        let shift = self.attempts.saturating_sub(1).min(4);
        let delay_ms = (RETRY_BASE_MS.saturating_mul(1_i64 << shift)).min(RETRY_MAX_MS);
        let retry_at = now + Duration::milliseconds(delay_ms);
        self.status = PendingTurnStatus::Pending;
        self.retry_at = Some(retry_at);
        self.outcome_note = Some(format!(
            "Отправка доказанно не началась: {reason}; безопасный повтор не раньше {}",
            retry_at.to_rfc3339()
        ));
        Ok(retry_at)
    }

    fn keep_unknown(&mut self, reason: String) -> Result<()> {
        if self.status != PendingTurnStatus::Dispatching {
            return Err(anyhow!("pending turn {} is not dispatching", self.id));
        }
        self.retry_at = None;
        self.outcome_note = Some(format!(
            "Результат отправки неизвестен: {reason}; автоматический повтор отключён, нужна ручная сверка"
        ));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result of atomically inspecting and, when safe, claiming the FIFO head.
pub enum PendingClaim {
    /// The queue has no rows.
    Empty,
    /// The FIFO head was durably fenced before dispatch.
    Claimed(PendingTurn),
    /// The FIFO head is retryable, but its backoff has not elapsed.
    Waiting(DateTime<Utc>),
    /// The FIFO head has an ambiguous delivery. Later rows must not overtake it.
    Unknown(PendingTurn),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Backoff recorded after returning a proven rejection to `Pending`.
pub struct PendingRetry {
    /// Number of proven pre-dispatch rejections recorded for this row.
    pub attempts: u32,
    /// Earliest safe time for another dispatch attempt.
    pub retry_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingFile {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    items: VecDeque<PendingTurn>,
}

fn pending_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("pending_turns.json")
}

fn pending_lock_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join(".pending_turns.lock")
}

/// Process-wide and cross-process exclusion for one queue file. The lock lives
/// beside the atomically replaced data file, so replacing the data inode never
/// invalidates mutual exclusion. Platforms without `flock` fail closed instead
/// of silently permitting unlocked read-modify-write cycles.
struct PendingFileLock {
    file: File,
}

impl PendingFileLock {
    fn acquire(project_dir: &Path, sid: &str) -> Result<Self> {
        let path = pending_lock_path(project_dir, sid);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        #[cfg(not(unix))]
        {
            return Err(anyhow!(
                "pending-turn durable locking is unavailable on this platform: {}",
                path.display()
            ));
        }
        #[cfg(unix)]
        {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("lock {}", path.display()));
            }
        }
        Ok(Self { file })
    }
}

impl Drop for PendingFileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let rc = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
            if rc != 0 {
                tracing::warn!(
                    error = %std::io::Error::last_os_error(),
                    "unlock pending-turn queue failed"
                );
            }
        }
    }
}

fn read_file_unlocked(project_dir: &Path, sid: &str) -> Result<PendingFile> {
    let path = pending_path(project_dir, sid);
    if !path.exists() {
        return Ok(PendingFile::default());
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn write_file_unlocked(project_dir: &Path, sid: &str, queue: &PendingFile) -> Result<()> {
    let path = pending_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(queue)?;
    ccteam_harness::atomic_write_durable(&path, &bytes)
}

/// Append one pending turn under the queue's stable sibling file lock.
pub fn enqueue_pending_turn(
    project_dir: &Path,
    sid: &str,
    text: impl Into<String>,
    origin: Option<String>,
    literal: bool,
    internal: bool,
    delegation_completion: bool,
) -> Result<PendingTurn> {
    let _lock = PendingFileLock::acquire(project_dir, sid)?;
    let mut queue = read_file_unlocked(project_dir, sid)?;
    queue.next_id = queue.next_id.saturating_add(1);
    let row = PendingTurn {
        id: format!("p{}", queue.next_id),
        text: text.into(),
        enqueued_at: Utc::now().to_rfc3339(),
        origin,
        literal,
        internal,
        delegation_completion,
        status: PendingTurnStatus::Pending,
        attempts: 0,
        retry_at: None,
        outcome_note: None,
    };
    queue.items.push_back(row.clone());
    write_file_unlocked(project_dir, sid, &queue)?;
    Ok(row)
}

/// Durably claim the FIFO head without removing it.
pub fn claim_next_pending_turn(
    project_dir: &Path,
    sid: &str,
    now: DateTime<Utc>,
) -> Result<PendingClaim> {
    let _lock = PendingFileLock::acquire(project_dir, sid)?;
    let mut queue = read_file_unlocked(project_dir, sid)?;
    let Some(front) = queue.items.front_mut() else {
        return Ok(PendingClaim::Empty);
    };
    match front.status {
        PendingTurnStatus::Dispatching => return Ok(PendingClaim::Unknown(front.clone())),
        PendingTurnStatus::Pending => {
            if let Some(retry_at) = front.retry_at.filter(|retry_at| *retry_at > now) {
                return Ok(PendingClaim::Waiting(retry_at));
            }
        }
    }
    front.claim(now)?;
    let claimed = front.clone();
    write_file_unlocked(project_dir, sid, &queue)?;
    Ok(PendingClaim::Claimed(claimed))
}

/// Remove one acknowledged FIFO head. Anything else is a transition error.
pub fn ack_pending_turn(project_dir: &Path, sid: &str, id: &str) -> Result<()> {
    let _lock = PendingFileLock::acquire(project_dir, sid)?;
    let mut queue = read_file_unlocked(project_dir, sid)?;
    let front = queue
        .items
        .front()
        .ok_or_else(|| anyhow!("pending queue is empty"))?;
    if front.id != id || front.status != PendingTurnStatus::Dispatching {
        return Err(anyhow!(
            "pending turn {id} is not the dispatching FIFO head"
        ));
    }
    queue.items.pop_front();
    write_file_unlocked(project_dir, sid, &queue)
}

/// Return a proven pre-dispatch rejection to Pending with bounded backoff.
pub fn retry_pending_turn(
    project_dir: &Path,
    sid: &str,
    id: &str,
    now: DateTime<Utc>,
    reason: String,
) -> Result<PendingRetry> {
    let _lock = PendingFileLock::acquire(project_dir, sid)?;
    let mut queue = read_file_unlocked(project_dir, sid)?;
    let front = queue
        .items
        .front_mut()
        .filter(|front| front.id == id)
        .ok_or_else(|| anyhow!("pending turn {id} is not the FIFO head"))?;
    let retry_at = front.retry_after_proven_rejection(now, reason)?;
    let attempts = front.attempts;
    write_file_unlocked(project_dir, sid, &queue)?;
    Ok(PendingRetry { attempts, retry_at })
}

/// Keep a post-dispatch error fenced as unknown/manual reconciliation.
pub fn mark_pending_turn_unknown(
    project_dir: &Path,
    sid: &str,
    id: &str,
    reason: String,
) -> Result<()> {
    let _lock = PendingFileLock::acquire(project_dir, sid)?;
    let mut queue = read_file_unlocked(project_dir, sid)?;
    let front = queue
        .items
        .front_mut()
        .filter(|front| front.id == id)
        .ok_or_else(|| anyhow!("pending turn {id} is not the FIFO head"))?;
    front.keep_unknown(reason)?;
    write_file_unlocked(project_dir, sid, &queue)
}

/// Best-effort queue depth for diagnostics.
pub fn pending_turn_count(project_dir: &Path, sid: &str) -> usize {
    let Ok(_lock) = PendingFileLock::acquire(project_dir, sid) else {
        return 0;
    };
    read_file_unlocked(project_dir, sid)
        .map(|queue| queue.items.len())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn claim_ack_preserves_fifo() {
        let tmp = TempDir::new().unwrap();
        let first = enqueue_pending_turn(
            tmp.path(),
            "s1",
            "first",
            Some("web".into()),
            false,
            false,
            false,
        )
        .unwrap();
        let second = enqueue_pending_turn(
            tmp.path(),
            "s1",
            "second",
            Some("web".into()),
            true,
            true,
            false,
        )
        .unwrap();

        let PendingClaim::Claimed(claimed) =
            claim_next_pending_turn(tmp.path(), "s1", Utc::now()).unwrap()
        else {
            panic!("first row must be claimable");
        };
        assert_eq!(claimed.id, first.id);
        assert_eq!(pending_turn_count(tmp.path(), "s1"), 2);
        assert!(matches!(
            claim_next_pending_turn(tmp.path(), "s1", Utc::now()).unwrap(),
            PendingClaim::Unknown(row) if row.id == first.id
        ));
        ack_pending_turn(tmp.path(), "s1", &first.id).unwrap();

        let PendingClaim::Claimed(claimed) =
            claim_next_pending_turn(tmp.path(), "s1", Utc::now()).unwrap()
        else {
            panic!("second row must follow the ack");
        };
        assert_eq!(claimed.id, second.id);
        assert!(claimed.literal);
        assert!(claimed.internal);
        ack_pending_turn(tmp.path(), "s1", &second.id).unwrap();
        assert_eq!(pending_turn_count(tmp.path(), "s1"), 0);
    }

    #[test]
    fn claiming_a_pending_turn_keeps_the_durable_row_until_ack() {
        let tmp = TempDir::new().unwrap();
        enqueue_pending_turn(
            tmp.path(),
            "s1",
            "survive cancellation",
            Some("web".into()),
            false,
            false,
            false,
        )
        .unwrap();
        assert!(matches!(
            claim_next_pending_turn(tmp.path(), "s1", Utc::now()).unwrap(),
            PendingClaim::Claimed(_)
        ));
        assert_eq!(pending_turn_count(tmp.path(), "s1"), 1);
    }

    #[test]
    fn proven_rejection_requeues_with_backoff_but_unknown_never_retries() {
        let tmp = TempDir::new().unwrap();
        let row =
            enqueue_pending_turn(tmp.path(), "s1", "first", None, false, false, false).unwrap();
        let now = Utc::now();
        let _ = claim_next_pending_turn(tmp.path(), "s1", now).unwrap();
        let retry =
            retry_pending_turn(tmp.path(), "s1", &row.id, now, "lookup miss".into()).unwrap();
        assert!(retry.retry_at > now);
        assert!(matches!(
            claim_next_pending_turn(tmp.path(), "s1", now).unwrap(),
            PendingClaim::Waiting(at) if at == retry.retry_at
        ));

        let PendingClaim::Claimed(claimed) =
            claim_next_pending_turn(tmp.path(), "s1", retry.retry_at).unwrap()
        else {
            panic!("retry must become claimable");
        };
        mark_pending_turn_unknown(tmp.path(), "s1", &claimed.id, "transport timeout".into())
            .unwrap();
        assert!(matches!(
            claim_next_pending_turn(tmp.path(), "s1", retry.retry_at).unwrap(),
            PendingClaim::Unknown(row) if row.id == claimed.id
        ));
    }

    #[test]
    fn missing_queue_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            claim_next_pending_turn(tmp.path(), "nope", Utc::now()).unwrap(),
            PendingClaim::Empty
        );
    }

    #[test]
    fn delegation_completion_survives_durable_queue() {
        let tmp = TempDir::new().unwrap();
        enqueue_pending_turn(tmp.path(), "s1", "done", None, false, true, true).unwrap();
        let PendingClaim::Claimed(turn) =
            claim_next_pending_turn(tmp.path(), "s1", Utc::now()).unwrap()
        else {
            panic!("row must be claimable");
        };
        let serialized = serde_json::to_value(turn).unwrap();
        assert_eq!(serialized["delegation_completion"], true);
    }

    #[test]
    fn concurrent_enqueue_and_ack_share_one_durable_transition_lock() {
        use std::sync::{mpsc, Arc, Barrier};
        use std::time::Duration as StdDuration;

        let tmp = TempDir::new().unwrap();
        let root = Arc::new(tmp.path().to_path_buf());
        let first = enqueue_pending_turn(&root, "s1", "first", None, false, false, false).unwrap();
        let _ = claim_next_pending_turn(&root, "s1", Utc::now()).unwrap();

        // Hold the stable sibling lock while both mutations start. If either
        // API bypasses it, that thread finishes while this guard is alive and
        // the assertion below fails deterministically.
        let file_lock = PendingFileLock::acquire(&root, "s1").unwrap();
        let start = Arc::new(Barrier::new(3));
        let (done_tx, done_rx) = mpsc::channel();

        let ack_root = Arc::clone(&root);
        let ack_start = Arc::clone(&start);
        let ack_done = done_tx.clone();
        let ack_id = first.id.clone();
        let ack = std::thread::spawn(move || {
            ack_start.wait();
            ack_pending_turn(&ack_root, "s1", &ack_id).unwrap();
            ack_done.send(()).unwrap();
        });

        let enqueue_root = Arc::clone(&root);
        let enqueue_start = Arc::clone(&start);
        let enqueue_done = done_tx;
        let enqueue = std::thread::spawn(move || {
            enqueue_start.wait();
            enqueue_pending_turn(&enqueue_root, "s1", "second", None, false, false, false).unwrap();
            enqueue_done.send(()).unwrap();
        });

        start.wait();
        assert!(done_rx.recv_timeout(StdDuration::from_millis(30)).is_err());
        drop(file_lock);
        done_rx.recv_timeout(StdDuration::from_secs(1)).unwrap();
        done_rx.recv_timeout(StdDuration::from_secs(1)).unwrap();
        ack.join().unwrap();
        enqueue.join().unwrap();

        assert_eq!(pending_turn_count(&root, "s1"), 1);
        let PendingClaim::Claimed(row) = claim_next_pending_turn(&root, "s1", Utc::now()).unwrap()
        else {
            panic!("the concurrent enqueue must survive the ack");
        };
        assert_eq!(row.id, "p2");
        assert_eq!(row.text, "second");
    }
}
