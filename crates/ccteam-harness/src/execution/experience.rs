//! Per-delegation experience records — a **derived index**, not a new SoT.
//!
//! Lives at `<project>/.ccteam/experience.jsonl` (project-level, shared across
//! sids). Each line is one JSON object: either a terminal-turn summary
//! (`kind: "turn"`) or a human verdict (`kind: "verdict"`).
//!
//! **Authority**: `turns.jsonl` + `progress.jsonl` remain the only state-of-
//! truth sources. This file is a rebuildable projection for self-evolution /
//! analytics. The live daemon's event pump is the sole online writer of
//! `kind: "turn"` rows; canonical verdicts live in `progress.jsonl`.
//! `ccteam internal experience rebuild <slug>` regenerates both projections
//! offline (disaster recovery).
//!
//! On Unix, ccteam-owned appends, reads, and rebuild replacement share the
//! project `experience.lock` flock, so their record and snapshot writes cannot
//! interleave. The compatibility reader returns intact rows, while the detailed
//! analytics/rebuild path reports every corrupt non-empty line and fails closed
//! rather than publishing partial aggregates.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt as _;
#[cfg(all(unix, not(target_os = "linux")))]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ccteam_cost::UnifiedTokenUsage;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use super::progress_bridge::{TurnSignals, Verdict};

/// Relative path of the project-level experience index.
const EXPERIENCE_REL: &str = ".ccteam/experience.jsonl";
const EXPERIENCE_LOCK_REL: &str = ".ccteam/experience.lock";

/// One line in `experience.jsonl` — tagged by `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // turn carries usage + skills map; line-oriented
pub enum ExperienceRecord {
    Turn(TurnExperience),
    Verdict(VerdictExperience),
}

/// Per-terminal-turn summary (live pump + rebuild both emit this shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnExperience {
    pub sid: String,
    pub turn_id: String,
    pub ts: DateTime<Utc>,
    /// Vendor scalar (`"claude"` / `"codex"` / `"grok"`).
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Bot role name (empty string for roleless).
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UnifiedTokenUsage>,
    /// Deterministic per-turn cost; `None` when unpriceable (never a faked 0
    /// for an unknown model — same honesty contract as status cost rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// First 12 hex of sha256(`.claude/agents/<role>.md`) at spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_sha: Option<String>,
    /// Per-skill content digests at spawn (see [`skills_fingerprint`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_sha: Option<BTreeMap<String, String>>,
    /// Skill ids deterministically observed as invoked during the turn,
    /// validated against the spawn-time `skills_sha` key set. `None` =
    /// detection unavailable or nothing observed — availability stays in
    /// `skills_sha`; this field upgrades attribution, never guesses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoked_skills: Option<Vec<String>>,
    pub signals: TurnSignals,
}

/// Human accept/revise on a completed turn (schema only in this task).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictExperience {
    pub sid: String,
    pub turn_id: String,
    pub ts: DateTime<Utc>,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
}

/// Resolve `<project>/.ccteam/experience.jsonl`.
pub fn experience_jsonl_path(project_dir: &Path) -> PathBuf {
    project_dir.join(EXPERIENCE_REL)
}

/// Cross-process exclusion shared by canonical terminal persistence,
/// projection append, and projection rebuild.
pub struct ExperienceLock {
    file: fs::File,
    path: PathBuf,
}

impl Drop for ExperienceLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

pub fn lock_experience(project_dir: &Path) -> Result<ExperienceLock> {
    let path = project_dir.join(EXPERIENCE_LOCK_REL);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    #[cfg(unix)]
    {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("lock {}", path.display()));
        }
    }
    Ok(ExperienceLock { file, path })
}

/// Append `record` as one JSONL line. Creates parent dir + file when missing.
/// Returns the absolute path written for caller logging.
pub fn append_experience(project_dir: &Path, record: &ExperienceRecord) -> Result<PathBuf> {
    let lock = lock_experience(project_dir)?;
    append_experience_locked(project_dir, &lock, record)
}

/// Append while holding [`lock_experience`]. This lets the live gateway cover
/// canonical progress/turn writes and the derived append with one exclusion
/// window, so a concurrent rebuild cannot duplicate or lose the boundary.
pub fn append_experience_locked(
    project_dir: &Path,
    lock: &ExperienceLock,
    record: &ExperienceRecord,
) -> Result<PathBuf> {
    let expected_lock = project_dir.join(EXPERIENCE_LOCK_REL);
    if lock.path != expected_lock {
        anyhow::bail!(
            "experience lock {} does not guard {}",
            lock.path.display(),
            project_dir.display()
        );
    }
    let path = experience_jsonl_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(record)? + "\n";
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    Ok(path)
}

/// Compatibility reader returning every parseable record. Analytics must use
/// [`read_all_experience_detailed`] and reject a non-zero corruption count.
pub fn read_all_experience(project_dir: &Path) -> Result<Vec<ExperienceRecord>> {
    Ok(read_all_experience_detailed(project_dir)?.records)
}

pub fn read_all_experience_detailed(
    project_dir: &Path,
) -> Result<super::fs_atomic::JsonlRead<ExperienceRecord>> {
    let path = experience_jsonl_path(project_dir);
    let lock_path = project_dir.join(EXPERIENCE_LOCK_REL);
    if !path.exists() && !lock_path.exists() {
        return Ok(super::fs_atomic::JsonlRead {
            records: Vec::new(),
            corrupt_line_count: 0,
        });
    }
    let _lock = lock_experience(project_dir)?;
    super::fs_atomic::read_jsonl_detailed(&path)
}

// ── fingerprints ─────────────────────────────────────────────────────────────

/// First 12 hex of sha256 of `.claude/agents/<role>.md`.
/// `None` for roleless (empty role) or a missing file.
pub fn role_fingerprint(project_dir: &Path, role: &str) -> Option<String> {
    let role = valid_fingerprint_role(role)?;
    #[cfg(target_os = "linux")]
    let bytes = linux_role_bytes(project_dir, role).ok()?;
    #[cfg(not(target_os = "linux"))]
    let bytes = portable_role_bytes(project_dir, role).ok()?;
    Some(short_sha256(&bytes))
}

fn valid_fingerprint_role(role: &str) -> Option<&str> {
    let role = role.trim();
    (!role.is_empty()
        && matches!(
            Path::new(role).components().collect::<Vec<_>>().as_slice(),
            [std::path::Component::Normal(_)]
        ))
    .then_some(role)
}

/// Per-skill digests under `.claude/skills/<id>/`.
///
/// For each skill directory, digest = first 12 hex of sha256 over the sorted
/// lines `"<relpath>:<sha256(content)>"` of every regular file under that
/// skill (recursive, deterministic). Returns `None` when no skill directories
/// exist; `Some(map)` when at least one skill dir is present (values may
/// still hash empty dirs as the digest of zero lines). One aggregate budget
/// covers every root/nested entry and byte across the whole operation. Linux
/// traversal is relative to pinned directory descriptors and refuses symlinks.
pub fn skills_fingerprint(project_dir: &Path) -> Option<BTreeMap<String, String>> {
    skills_fingerprint_with_hook(project_dir, || {})
}

fn skills_fingerprint_with_hook(
    project_dir: &Path,
    after_claude_open: impl FnOnce(),
) -> Option<BTreeMap<String, String>> {
    #[cfg(target_os = "linux")]
    {
        linux_skills_fingerprint(project_dir, after_claude_open)
            .ok()
            .flatten()
    }
    #[cfg(not(target_os = "linux"))]
    {
        portable_skills_fingerprint(project_dir, after_claude_open)
            .ok()
            .flatten()
    }
}

const MAX_SKILL_FINGERPRINT_DEPTH: usize = 16;
const MAX_SKILL_FINGERPRINT_ENTRIES: usize = 4_096;
const MAX_SKILL_FINGERPRINT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROLE_FINGERPRINT_BYTES: u64 = 1024 * 1024;

#[derive(Default)]
struct SkillFingerprintBudget {
    entries: usize,
    bytes: u64,
}

impl SkillFingerprintBudget {
    fn spend_entry(&mut self) -> Result<()> {
        self.entries = self.entries.saturating_add(1);
        if self.entries > MAX_SKILL_FINGERPRINT_ENTRIES {
            anyhow::bail!("skill fingerprint directory entry limit exceeded");
        }
        Ok(())
    }
}

fn digest_skill_pairs(mut pairs: Vec<(String, String)>) -> String {
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, content_sha) in &pairs {
        hasher.update(rel.as_bytes());
        hasher.update(b":");
        hasher.update(content_sha.as_bytes());
        hasher.update(b"\n");
    }
    hex12(hasher.finalize())
}

fn read_regular_handle_bounded(file: fs::File, max_bytes: u64) -> Result<Vec<u8>> {
    let metadata = file.metadata().context("stat fingerprint file")?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        anyhow::bail!("fingerprint file exceeds limit or is not regular");
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .context("read fingerprint file")?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        anyhow::bail!("fingerprint file exceeds limit");
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn linux_role_bytes(project_dir: &Path, role: &str) -> Result<Vec<u8>> {
    let root = linux_open_project_root(project_dir)?;
    let claude = linux_open_child(&root, OsStr::new(".claude"), libc::O_DIRECTORY)?;
    let agents = linux_open_child(&claude, OsStr::new("agents"), libc::O_DIRECTORY)?;
    let role_name = format!("{role}.md");
    let file = linux_open_child(&agents, OsStr::new(&role_name), 0)?;
    read_regular_handle_bounded(file, MAX_ROLE_FINGERPRINT_BYTES)
}

#[cfg(target_os = "linux")]
fn linux_skills_fingerprint(
    project_dir: &Path,
    after_claude_open: impl FnOnce(),
) -> Result<Option<BTreeMap<String, String>>> {
    let root = linux_open_project_root(project_dir)?;
    let claude = linux_open_child(&root, OsStr::new(".claude"), libc::O_DIRECTORY)?;
    after_claude_open();
    let skills_meta = fs::symlink_metadata(linux_fd_path(&claude).join("skills"))?;
    let skills = if skills_meta.file_type().is_symlink() {
        let target = fs::read_link(linux_fd_path(&claude).join("skills"))?;
        if target != Path::new("../.agents/skills") {
            anyhow::bail!("unmanaged skills root symlink");
        }
        let agents = linux_open_child(&root, OsStr::new(".agents"), libc::O_DIRECTORY)?;
        linux_open_child(&agents, OsStr::new("skills"), libc::O_DIRECTORY)?
    } else if skills_meta.is_dir() {
        linux_open_child(&claude, OsStr::new("skills"), libc::O_DIRECTORY)?
    } else {
        return Ok(None);
    };

    let mut budget = SkillFingerprintBudget::default();
    let names = linux_read_dir_names(&skills, &mut budget)?;
    let mut map = BTreeMap::new();
    for name in names {
        let metadata = fs::symlink_metadata(linux_fd_path(&skills).join(&name))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(id) = name.to_str().map(str::to_owned) else {
            continue;
        };
        let skill = linux_open_child(&skills, &name, libc::O_DIRECTORY)?;
        map.insert(id, linux_skill_digest(&skill, &mut budget));
    }
    Ok((!map.is_empty()).then_some(map))
}

#[cfg(target_os = "linux")]
fn linux_skill_digest(skill: &fs::File, budget: &mut SkillFingerprintBudget) -> String {
    let mut pairs = Vec::new();
    if linux_collect_skill_files(skill, Path::new(""), 0, budget, &mut pairs).is_err() {
        return "unavailable".to_string();
    }
    digest_skill_pairs(pairs)
}

#[cfg(target_os = "linux")]
fn linux_collect_skill_files(
    dir: &fs::File,
    relative_dir: &Path,
    depth: usize,
    budget: &mut SkillFingerprintBudget,
    out: &mut Vec<(String, String)>,
) -> Result<()> {
    if depth > MAX_SKILL_FINGERPRINT_DEPTH {
        anyhow::bail!("skill fingerprint depth limit exceeded");
    }
    for name in linux_read_dir_names(dir, budget)? {
        let metadata = fs::symlink_metadata(linux_fd_path(dir).join(&name))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let relative = relative_dir.join(&name);
        if metadata.is_dir() {
            let child = linux_open_child(dir, &name, libc::O_DIRECTORY)?;
            linux_collect_skill_files(&child, &relative, depth + 1, budget, out)?;
        } else if metadata.is_file() {
            let remaining = MAX_SKILL_FINGERPRINT_BYTES.saturating_sub(budget.bytes);
            let file = linux_open_child(dir, &name, 0)?;
            let bytes = read_regular_handle_bounded(file, remaining)?;
            budget.bytes = budget
                .bytes
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            if budget.bytes > MAX_SKILL_FINGERPRINT_BYTES {
                anyhow::bail!("skill fingerprint byte limit exceeded");
            }
            out.push((
                relative.to_string_lossy().replace('\\', "/"),
                full_sha256_hex(&bytes),
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_read_dir_names(
    dir: &fs::File,
    budget: &mut SkillFingerprintBudget,
) -> Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(linux_fd_path(dir)).context("read pinned skill directory")? {
        budget.spend_entry()?;
        names.push(
            entry
                .context("read pinned skill directory entry")?
                .file_name(),
        );
    }
    names.sort();
    Ok(names)
}

#[cfg(target_os = "linux")]
fn linux_fd_path(file: &fs::File) -> PathBuf {
    PathBuf::from("/proc/self/fd").join(file.as_raw_fd().to_string())
}

#[cfg(target_os = "linux")]
fn linux_open_project_root(project_dir: &Path) -> Result<fs::File> {
    let canonical = fs::canonicalize(project_dir)
        .with_context(|| format!("resolve project root {}", project_dir.display()))?;
    linux_openat2(
        libc::AT_FDCWD,
        canonical.as_os_str(),
        libc::O_DIRECTORY,
        libc::RESOLVE_NO_SYMLINKS,
    )
}

#[cfg(target_os = "linux")]
fn linux_open_child(parent: &fs::File, name: &OsStr, extra_flags: i32) -> Result<fs::File> {
    linux_openat2(
        parent.as_raw_fd(),
        name,
        extra_flags,
        libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS,
    )
}

#[cfg(target_os = "linux")]
fn linux_openat2(dirfd: i32, path: &OsStr, extra_flags: i32, resolve: u64) -> Result<fs::File> {
    let path = CString::new(path.as_bytes()).context("fingerprint path contains NUL")?;
    // SAFETY: Linux defines `open_how` as three integer fields and requires
    // unknown/future fields to be zero for forward-compatible calls.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (libc::O_RDONLY | libc::O_CLOEXEC | extra_flags) as u64;
    how.resolve = resolve;
    // SAFETY: `path` is NUL-terminated, `how` points to an initialized
    // `open_how`, and a successful descriptor is transferred exactly once.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open fingerprint path safely");
    }
    // SAFETY: the successful syscall returned a new owned descriptor.
    Ok(unsafe { fs::File::from_raw_fd(fd as i32) })
}

#[cfg(not(target_os = "linux"))]
fn portable_role_bytes(project_dir: &Path, role: &str) -> Result<Vec<u8>> {
    let project_root = fs::canonicalize(project_dir)?;
    let claude_dir = project_dir.join(".claude");
    let claude_metadata = fs::symlink_metadata(&claude_dir)?;
    if claude_metadata.file_type().is_symlink() || !claude_metadata.is_dir() {
        anyhow::bail!("refuse non-directory role parent");
    }
    let agents_dir = claude_dir.join("agents");
    let agents_metadata = fs::symlink_metadata(&agents_dir)?;
    if agents_metadata.file_type().is_symlink() || !agents_metadata.is_dir() {
        anyhow::bail!("refuse non-directory role parent");
    }
    let path = agents_dir.join(format!("{role}.md"));
    portable_read_regular(&project_root, &path, MAX_ROLE_FINGERPRINT_BYTES)
}

#[cfg(not(target_os = "linux"))]
fn portable_skills_fingerprint(
    project_dir: &Path,
    after_claude_open: impl FnOnce(),
) -> Result<Option<BTreeMap<String, String>>> {
    let project_root = fs::canonicalize(project_dir)?;
    let claude_dir = project_dir.join(".claude");
    let claude_metadata = fs::symlink_metadata(&claude_dir)?;
    if claude_metadata.file_type().is_symlink() || !claude_metadata.is_dir() {
        return Ok(None);
    }
    after_claude_open();
    if fs::canonicalize(&claude_dir)? != project_root.join(".claude") {
        anyhow::bail!("fingerprint parent changed during scan");
    }
    let skills_root = claude_dir.join("skills");
    let root_metadata = fs::symlink_metadata(&skills_root)?;
    if root_metadata.file_type().is_symlink() {
        let agents_dir = project_dir.join(".agents");
        let agents_metadata = fs::symlink_metadata(&agents_dir)?;
        if agents_metadata.file_type().is_symlink() || !agents_metadata.is_dir() {
            return Ok(None);
        }
        let managed_root = agents_dir.join("skills");
        let managed_metadata = fs::symlink_metadata(&managed_root)?;
        let managed_real = fs::canonicalize(&managed_root)?;
        if managed_metadata.file_type().is_symlink()
            || !managed_metadata.is_dir()
            || !managed_real.starts_with(&project_root)
            || fs::canonicalize(&skills_root)? != managed_real
        {
            return Ok(None);
        }
    } else if !root_metadata.is_dir() {
        return Ok(None);
    }
    let skills_real = fs::canonicalize(&skills_root)?;
    if !skills_real.starts_with(&project_root) {
        anyhow::bail!("skills root escaped project root");
    }
    let mut budget = SkillFingerprintBudget::default();
    let entries = portable_read_dir(&skills_root, &mut budget)?;
    if fs::canonicalize(&skills_root)? != skills_real {
        anyhow::bail!("skills root changed during scan");
    }
    let mut map = BTreeMap::new();
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        map.insert(id, portable_skill_digest(&project_root, &path, &mut budget));
    }
    Ok((!map.is_empty()).then_some(map))
}

#[cfg(not(target_os = "linux"))]
fn portable_skill_digest(
    project_root: &Path,
    skill_dir: &Path,
    budget: &mut SkillFingerprintBudget,
) -> String {
    let mut pairs = Vec::new();
    if portable_collect_skill_files(project_root, skill_dir, skill_dir, 0, budget, &mut pairs)
        .is_err()
    {
        return "unavailable".to_string();
    }
    digest_skill_pairs(pairs)
}

#[cfg(not(target_os = "linux"))]
fn portable_read_dir(dir: &Path, budget: &mut SkillFingerprintBudget) -> Result<Vec<fs::DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))? {
        budget.spend_entry()?;
        entries.push(entry.with_context(|| format!("read directory entry in {}", dir.display()))?);
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    Ok(entries)
}

#[cfg(not(target_os = "linux"))]
fn portable_collect_skill_files(
    project_root: &Path,
    root: &Path,
    dir: &Path,
    depth: usize,
    budget: &mut SkillFingerprintBudget,
    out: &mut Vec<(String, String)>,
) -> Result<()> {
    if depth > MAX_SKILL_FINGERPRINT_DEPTH {
        anyhow::bail!("skill fingerprint depth limit exceeded");
    }
    let before = fs::canonicalize(dir)?;
    if !before.starts_with(project_root) {
        anyhow::bail!("skill directory escaped project root");
    }
    for entry in portable_read_dir(dir, budget)? {
        let path = entry.path();
        let meta = entry.file_type()?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            portable_collect_skill_files(project_root, root, &path, depth + 1, budget, out)?;
        } else if meta.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let relpath = rel.to_string_lossy().replace('\\', "/");
            let remaining = MAX_SKILL_FINGERPRINT_BYTES.saturating_sub(budget.bytes);
            let bytes = portable_read_regular(project_root, &path, remaining)?;
            budget.bytes = budget
                .bytes
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            if budget.bytes > MAX_SKILL_FINGERPRINT_BYTES {
                anyhow::bail!("skill fingerprint byte limit exceeded");
            }
            out.push((relpath, full_sha256_hex(&bytes)));
        }
    }
    if fs::canonicalize(dir)? != before {
        anyhow::bail!("skill directory changed during scan");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn portable_read_regular(project_root: &Path, path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let before = fs::canonicalize(path)?;
    if !before.starts_with(project_root) {
        anyhow::bail!("fingerprint file escaped project root");
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(all(unix, not(target_os = "linux")))]
    options.custom_flags(libc::O_NOFOLLOW);
    #[cfg(not(unix))]
    if fs::symlink_metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .file_type()
        .is_symlink()
    {
        anyhow::bail!("refuse symlink {}", path.display());
    }
    let bytes = read_regular_handle_bounded(
        options
            .open(path)
            .with_context(|| format!("open {}", path.display()))?,
        max_bytes,
    )?;
    if fs::canonicalize(path)? != before {
        anyhow::bail!("fingerprint file changed during scan");
    }
    Ok(bytes)
}

fn short_sha256(bytes: &[u8]) -> String {
    hex12(Sha256::digest(bytes))
}

fn full_sha256_hex(bytes: &[u8]) -> String {
    hex_full(Sha256::digest(bytes))
}

fn hex_full(digest: impl AsRef<[u8]>) -> String {
    let d = digest.as_ref();
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex12(digest: impl AsRef<[u8]>) -> String {
    hex_full(digest).chars().take(12).collect()
}

// ── rebuild (offline / disaster recovery) ────────────────────────────────────

/// Regenerate all `kind: "turn"` records from `chat/<sid>/turns.jsonl` +
/// retained `progress.jsonl` `chat_turn_completed` and `turn_verdict` events.
/// Existing derived rows are ignored. Writes atomically (tmp + rename).
///
/// The shared experience lock excludes the live canonical+projection writer
/// for the full read/replace window, so the generated snapshot is lossless.
///
/// Returns `(turns_written, verdicts_written)`.
pub fn rebuild_experience(
    project_dir: &Path,
    progress_path: Option<&Path>,
) -> Result<(usize, usize)> {
    let _lock = lock_experience(project_dir)?;
    // Index retained progress events by (sid, turn_id). The first canonical
    // terminal fact wins across checkpoint/archive/active generations.
    let mut progress_by_key: BTreeMap<(String, String), serde_json::Value> = BTreeMap::new();
    let mut verdicts: Vec<ExperienceRecord> = Vec::new();
    if let Some(pp) = progress_path {
        for ev in read_retained_progress_events(pp)? {
            if ev.get("event").and_then(|v| v.as_str()) != Some("chat_turn_completed") {
                continue;
            }
            let Some(sid) = ev.get("sid").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(turn_id) = ev.get("turn_id").and_then(|v| v.as_str()) else {
                continue;
            };
            progress_by_key
                .entry((sid.to_string(), turn_id.to_string()))
                .or_insert(ev);
        }
        verdicts = super::progress_bridge::latest_turn_verdicts(pp)?
            .into_values()
            .map(|verdict| {
                ExperienceRecord::Verdict(VerdictExperience {
                    sid: verdict.sid,
                    turn_id: verdict.turn_id,
                    ts: verdict.ts,
                    verdict: verdict.verdict,
                    feedback: verdict.feedback,
                })
            })
            .collect();
    }
    let verdicts_written = verdicts.len();

    let mut canonical_turns: BTreeMap<(String, String), super::turns_mirror::TurnRecord> =
        BTreeMap::new();
    let chat_base = project_dir.join(".ccteam").join("chat");
    if let Ok(entries) = fs::read_dir(&chat_base) {
        let mut sids: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.is_dir() {
                    e.file_name().to_str().map(str::to_string)
                } else {
                    None
                }
            })
            .collect();
        sids.sort();
        for sid in sids {
            let turn_read = super::turns_mirror::read_all_turns_detailed(project_dir, &sid)
                .with_context(|| format!("read canonical turns for {sid}"))?;
            if turn_read.corrupt_line_count > 0 {
                anyhow::bail!(
                    "canonical turns for {sid} contain {} corrupt line(s)",
                    turn_read.corrupt_line_count
                );
            }
            // Only canonical terminal rows are rebuild authority. User-only,
            // interim, and legacy rows without an explicit outcome are skipped:
            // guessing their boundary would resurrect drafts as completed work.
            for tr in turn_read.records {
                if matches!(tr.outcome.as_deref(), Some("completed" | "failed")) {
                    canonical_turns
                        .entry((sid.clone(), tr.turn_id.clone()))
                        .or_insert(tr);
                }
            }
        }
    }

    let keys = canonical_turns
        .keys()
        .chain(progress_by_key.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut turns: Vec<ExperienceRecord> = Vec::with_capacity(keys.len());
    for (sid, turn_id) in keys {
        let tr = canonical_turns.get(&(sid.clone(), turn_id.clone()));
        let progress = progress_by_key.get(&(sid.clone(), turn_id.clone()));
        let vendor = progress_field::<String>(progress, "vendor", &sid, &turn_id)?
            .or_else(|| tr.map(|turn| turn.vendor.clone()))
            .with_context(|| format!("terminal turn {sid}/{turn_id} has no vendor"))?;
        let role = progress_field::<String>(progress, "role", &sid, &turn_id)?
            .or_else(|| tr.map(|turn| turn.role.clone()))
            .with_context(|| format!("terminal turn {sid}/{turn_id} has no role"))?;
        let ts = progress_field::<String>(progress, "ts", &sid, &turn_id)?
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .with_context(|| format!("terminal turn {sid}/{turn_id} has invalid ts"))?
            .map(|value| value.with_timezone(&Utc))
            .or_else(|| tr.map(|turn| turn.ts))
            .with_context(|| format!("terminal turn {sid}/{turn_id} has no ts"))?;
        let usage = progress_field::<UnifiedTokenUsage>(progress, "usage", &sid, &turn_id)?
            .or_else(|| {
                tr.and_then(|turn| {
                    (!turn.usage.is_null())
                        .then(|| serde_json::from_value(turn.usage.clone()).ok())
                        .flatten()
                })
            });
        let model = progress_field::<String>(progress, "model", &sid, &turn_id)?;
        let cost_usd = usage.as_ref().and_then(|usage| {
            cost_vendor_from_label(&vendor).and_then(|cost_vendor| {
                ccteam_cost::resolve_turn_cost(usage, cost_vendor, model.as_deref().unwrap_or(""))
            })
        });
        let outcome = progress_field::<String>(progress, "outcome", &sid, &turn_id)?
            .or_else(|| tr.and_then(|turn| turn.outcome.clone()));
        let duration_ms = progress_field::<u64>(progress, "duration_ms", &sid, &turn_id)?;
        let role_sha = progress_field::<String>(progress, "role_sha", &sid, &turn_id)?;
        let skills_sha =
            progress_field::<BTreeMap<String, String>>(progress, "skills_sha", &sid, &turn_id)?;
        let invoked_skills =
            progress_field::<Vec<String>>(progress, "invoked_skills", &sid, &turn_id)?;
        let signals = progress_field::<TurnSignals>(progress, "signals", &sid, &turn_id)?
            .unwrap_or_else(|| TurnSignals {
                tool_calls: tr.map_or(0, |turn| turn.tool_calls.len() as u64),
                steered: false,
                error_recovered: None,
            });
        turns.push(ExperienceRecord::Turn(TurnExperience {
            sid,
            turn_id,
            ts,
            vendor,
            model,
            role,
            usage,
            cost_usd,
            outcome,
            duration_ms,
            role_sha,
            skills_sha,
            invoked_skills,
            signals,
        }));
    }

    let turns_written = turns.len();
    // Stable order: turns (by sid, then ts) then projected verdicts.
    turns.sort_by(|a, b| match (a, b) {
        (ExperienceRecord::Turn(x), ExperienceRecord::Turn(y)) => {
            x.sid.cmp(&y.sid).then_with(|| x.ts.cmp(&y.ts))
        }
        _ => std::cmp::Ordering::Equal,
    });

    let mut body = String::new();
    for rec in turns.iter().chain(verdicts.iter()) {
        body.push_str(&serde_json::to_string(rec)?);
        body.push('\n');
    }

    let path = experience_jsonl_path(project_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    super::fs_atomic::atomic_write_durable(&path, body.as_bytes())
        .with_context(|| format!("replace {}", path.display()))?;

    Ok((turns_written, verdicts_written))
}

fn progress_field<T: serde::de::DeserializeOwned>(
    event: Option<&serde_json::Value>,
    field: &str,
    sid: &str,
    turn_id: &str,
) -> Result<Option<T>> {
    let Some(value) = event.and_then(|event| event.get(field)) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    serde_json::from_value(value.clone())
        .map(Some)
        .with_context(|| format!("terminal turn {sid}/{turn_id} has invalid {field}"))
}

fn cost_vendor_from_label(vendor: &str) -> Option<ccteam_cost::Vendor> {
    match vendor {
        "claude" => Some(ccteam_cost::Vendor::Claude),
        "codex" => Some(ccteam_cost::Vendor::Codex),
        "grok" => Some(ccteam_cost::Vendor::Grok),
        "opencode" => Some(ccteam_cost::Vendor::Opencode),
        "kimi" => Some(ccteam_cost::Vendor::Kimi),
        "pi" => Some(ccteam_cost::Vendor::Pi),
        "dsh" => Some(ccteam_cost::Vendor::Dsh),
        _ => None,
    }
}

fn read_retained_progress_events(path: &Path) -> Result<Vec<serde_json::Value>> {
    super::progress_bridge::terminal_turns_for_rebuild(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_turn(sid: &str, turn_id: &str) -> ExperienceRecord {
        ExperienceRecord::Turn(TurnExperience {
            sid: sid.into(),
            turn_id: turn_id.into(),
            ts: Utc::now(),
            vendor: "claude".into(),
            model: Some("claude-sonnet-4-6".into()),
            role: "cto".into(),
            usage: Some(UnifiedTokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }),
            cost_usd: Some(0.001),
            outcome: Some("completed".into()),
            duration_ms: Some(100),
            role_sha: Some("ab12cd34ef56".into()),
            skills_sha: None,
            invoked_skills: None,
            signals: TurnSignals {
                tool_calls: 3,
                steered: false,
                error_recovered: None,
            },
        })
    }

    #[test]
    fn schema_round_trip_turn_and_verdict() {
        let turn = sample_turn("s1", "s1-1");
        let json = serde_json::to_string(&turn).unwrap();
        assert!(json.contains(r#""kind":"turn""#));
        let back: ExperienceRecord = serde_json::from_str(&json).unwrap();
        match (&turn, &back) {
            (ExperienceRecord::Turn(a), ExperienceRecord::Turn(b)) => {
                assert_eq!(a.sid, b.sid);
                assert_eq!(a.turn_id, b.turn_id);
                assert_eq!(a.vendor, b.vendor);
                assert_eq!(a.outcome, b.outcome);
                assert_eq!(a.role_sha, b.role_sha);
                assert_eq!(a.signals.tool_calls, b.signals.tool_calls);
            }
            _ => panic!("expected turn"),
        }

        let verdict = ExperienceRecord::Verdict(VerdictExperience {
            sid: "s1".into(),
            turn_id: "s1-1".into(),
            ts: Utc::now(),
            verdict: Verdict::Accept,
            feedback: Some("lgtm".into()),
        });
        let vjson = serde_json::to_string(&verdict).unwrap();
        assert!(vjson.contains(r#""kind":"verdict""#));
        assert!(vjson.contains(r#""verdict":"accept""#));
        let vback: ExperienceRecord = serde_json::from_str(&vjson).unwrap();
        match vback {
            ExperienceRecord::Verdict(v) => {
                assert!(matches!(v.verdict, Verdict::Accept));
                assert_eq!(v.feedback.as_deref(), Some("lgtm"));
            }
            _ => panic!("expected verdict"),
        }
    }

    #[test]
    fn append_and_read_tolerant_of_corrupt_line() {
        let tmp = TempDir::new().unwrap();
        let t1 = sample_turn("s1", "s1-1");
        append_experience(tmp.path(), &t1).unwrap();
        // Inject a corrupt line between good records.
        let path = experience_jsonl_path(tmp.path());
        let good = serde_json::to_string(&sample_turn("s1", "s1-2")).unwrap();
        let mut body = fs::read_to_string(&path).unwrap();
        body.push_str("{not-json\n");
        body.push_str(&good);
        body.push('\n');
        fs::write(&path, body).unwrap();

        let read = read_all_experience(tmp.path()).unwrap();
        assert_eq!(read.len(), 2);
        match &read[0] {
            ExperienceRecord::Turn(t) => assert_eq!(t.turn_id, "s1-1"),
            _ => panic!("expected turn"),
        }
        match &read[1] {
            ExperienceRecord::Turn(t) => assert_eq!(t.turn_id, "s1-2"),
            _ => panic!("expected turn"),
        }

        let detailed = read_all_experience_detailed(tmp.path()).unwrap();
        assert_eq!(detailed.records.len(), 2);
        assert_eq!(detailed.corrupt_line_count, 1);
    }

    #[test]
    fn read_missing_file_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(read_all_experience(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn role_fingerprint_deterministic_and_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        let agents = tmp.path().join(".claude").join("agents");
        fs::create_dir_all(&agents).unwrap();
        let role_path = agents.join("cto.md");
        fs::write(&role_path, b"you are cto v1").unwrap();
        let a = role_fingerprint(tmp.path(), "cto").unwrap();
        let b = role_fingerprint(tmp.path(), "cto").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        fs::write(&role_path, b"you are cto v2").unwrap();
        let c = role_fingerprint(tmp.path(), "cto").unwrap();
        assert_ne!(a, c);
        assert!(role_fingerprint(tmp.path(), "").is_none());
        assert!(role_fingerprint(tmp.path(), "missing").is_none());
        assert!(role_fingerprint(tmp.path(), "../../outside").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn role_fingerprint_refuses_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let agents = tmp.path().join(".claude/agents");
        fs::create_dir_all(&agents).unwrap();
        let outside = tmp.path().join("outside.md");
        fs::write(&outside, b"outside role").unwrap();
        symlink(&outside, agents.join("cto.md")).unwrap();

        assert!(role_fingerprint(tmp.path(), "cto").is_none());
    }

    #[test]
    fn role_fingerprint_refuses_oversized_files() {
        let tmp = TempDir::new().unwrap();
        let agents = tmp.path().join(".claude/agents");
        fs::create_dir_all(&agents).unwrap();
        fs::File::create(agents.join("cto.md"))
            .unwrap()
            .set_len(MAX_ROLE_FINGERPRINT_BYTES + 1)
            .unwrap();

        assert!(role_fingerprint(tmp.path(), "cto").is_none());
    }

    #[test]
    fn skills_fingerprint_deterministic_and_changes_with_content() {
        let tmp = TempDir::new().unwrap();
        assert!(skills_fingerprint(tmp.path()).is_none());

        let skill = tmp.path().join(".claude").join("skills").join("ci-watcher");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), b"watch ci").unwrap();
        let a = skills_fingerprint(tmp.path()).unwrap();
        let b = skills_fingerprint(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.get("ci-watcher").unwrap().len(), 12);

        fs::write(skill.join("SKILL.md"), b"watch ci harder").unwrap();
        let c = skills_fingerprint(tmp.path()).unwrap();
        assert_ne!(a.get("ci-watcher"), c.get("ci-watcher"));
    }

    #[cfg(unix)]
    #[test]
    fn skills_fingerprint_ignores_nested_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let skill = tmp.path().join(".claude").join("skills").join("safe");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), b"stable body").unwrap();
        let outside = tmp.path().join("outside-secret");
        fs::write(&outside, b"secret-v1").unwrap();
        symlink(&outside, skill.join("linked-secret")).unwrap();

        let before = skills_fingerprint(tmp.path()).unwrap();
        fs::write(&outside, b"secret-v2-with-different-content").unwrap();
        let after = skills_fingerprint(tmp.path()).unwrap();

        assert_eq!(
            before, after,
            "skill digests must neither follow nor hash nested symlinks"
        );
    }

    #[test]
    fn skills_fingerprint_fails_closed_at_the_depth_limit() {
        let tmp = TempDir::new().unwrap();
        let skill = tmp.path().join(".claude/skills/deep");
        let mut nested = skill.clone();
        for index in 0..=MAX_SKILL_FINGERPRINT_DEPTH {
            nested = nested.join(format!("d{index}"));
        }
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("SKILL.md"), b"too deep").unwrap();

        let fingerprint = skills_fingerprint(tmp.path()).unwrap();
        assert_eq!(
            fingerprint.get("deep").map(String::as_str),
            Some("unavailable")
        );
    }

    #[test]
    fn skills_fingerprint_fails_closed_at_file_and_byte_limits() {
        let tmp = TempDir::new().unwrap();
        let skill = tmp.path().join(".claude/skills/bounded");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("one.md"), b"one").unwrap();
        let mut exhausted_entries = SkillFingerprintBudget {
            entries: MAX_SKILL_FINGERPRINT_ENTRIES,
            bytes: 0,
        };
        assert!(exhausted_entries.spend_entry().is_err());

        fs::File::create(skill.join("huge.bin"))
            .unwrap()
            .set_len(MAX_SKILL_FINGERPRINT_BYTES + 1)
            .unwrap();
        let fingerprints = skills_fingerprint(tmp.path()).unwrap();
        assert_eq!(
            fingerprints.get("bounded").map(String::as_str),
            Some("unavailable")
        );
    }

    #[test]
    fn skills_fingerprint_enforces_aggregate_limits_across_skills() {
        let file_tmp = TempDir::new().unwrap();
        let skills = file_tmp.path().join(".claude/skills");
        let first = skills.join("a-first");
        let second = skills.join("b-second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        for index in 0..(MAX_SKILL_FINGERPRINT_ENTRIES / 2) {
            fs::write(first.join(format!("{index:04}.md")), b"a").unwrap();
        }
        for index in 0..=(MAX_SKILL_FINGERPRINT_ENTRIES / 2) {
            fs::write(second.join(format!("{index:04}.md")), b"b").unwrap();
        }

        let fingerprints = skills_fingerprint(file_tmp.path()).unwrap();
        assert_ne!(
            fingerprints.get("a-first").map(String::as_str),
            Some("unavailable")
        );
        assert_eq!(
            fingerprints.get("b-second").map(String::as_str),
            Some("unavailable"),
            "the file budget must not reset for each top-level skill"
        );

        let byte_tmp = TempDir::new().unwrap();
        let first = byte_tmp.path().join(".claude/skills/a-first");
        let second = byte_tmp.path().join(".claude/skills/b-second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::File::create(first.join("half.bin"))
            .unwrap()
            .set_len(MAX_SKILL_FINGERPRINT_BYTES / 2)
            .unwrap();
        fs::File::create(second.join("over-half.bin"))
            .unwrap()
            .set_len(MAX_SKILL_FINGERPRINT_BYTES / 2 + 1)
            .unwrap();

        let fingerprints = skills_fingerprint(byte_tmp.path()).unwrap();
        assert_ne!(
            fingerprints.get("a-first").map(String::as_str),
            Some("unavailable")
        );
        assert_eq!(
            fingerprints.get("b-second").map(String::as_str),
            Some("unavailable"),
            "the byte budget must not reset for each top-level skill"
        );
    }

    #[test]
    fn skills_fingerprint_bounds_top_level_enumeration() {
        let tmp = TempDir::new().unwrap();
        let skills = tmp.path().join(".claude/skills");
        fs::create_dir_all(&skills).unwrap();
        for index in 0..=MAX_SKILL_FINGERPRINT_ENTRIES {
            fs::create_dir(skills.join(format!("skill-{index:04}"))).unwrap();
        }

        assert!(
            skills_fingerprint(tmp.path()).is_none(),
            "top-level skill enumeration must be bounded before hashing"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn skills_fingerprint_counts_nested_empty_dirs_and_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let skill = tmp.path().join(".claude/skills/bounded");
        fs::create_dir_all(&skill).unwrap();
        let half = MAX_SKILL_FINGERPRINT_ENTRIES / 2;
        for index in 0..half {
            fs::create_dir(skill.join(format!("dir-{index:04}"))).unwrap();
            symlink("missing", skill.join(format!("link-{index:04}"))).unwrap();
        }

        let fingerprints = skills_fingerprint(tmp.path()).unwrap();
        assert_eq!(
            fingerprints.get("bounded").map(String::as_str),
            Some("unavailable"),
            "every root and nested entry must spend the aggregate traversal budget"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn skills_fingerprint_keeps_scanning_the_pinned_parent_after_a_swap() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let inside = claude.join("skills/inside");
        fs::create_dir_all(&inside).unwrap();
        fs::write(inside.join("SKILL.md"), b"inside project").unwrap();
        let outside_claude = tmp.path().join("outside-claude");
        let outside = outside_claude.join("skills/outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("SKILL.md"), b"must never be hashed").unwrap();
        let displaced = tmp.path().join(".claude-pinned");

        let fingerprints = skills_fingerprint_with_hook(tmp.path(), || {
            fs::rename(&claude, &displaced).unwrap();
            symlink(&outside_claude, &claude).unwrap();
        })
        .unwrap();

        assert!(fingerprints.contains_key("inside"));
        assert!(!fingerprints.contains_key("outside"));
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    #[test]
    fn skills_fingerprint_fails_closed_after_a_parent_swap_on_portable_unix() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        fs::create_dir_all(claude.join("skills/inside")).unwrap();
        let outside_claude = tmp.path().join("outside-claude");
        fs::create_dir_all(outside_claude.join("skills/outside")).unwrap();
        let displaced = tmp.path().join(".claude-displaced");

        let fingerprints = skills_fingerprint_with_hook(tmp.path(), || {
            fs::rename(&claude, &displaced).unwrap();
            symlink(&outside_claude, &claude).unwrap();
        });

        assert!(fingerprints.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn skills_fingerprint_allows_root_link_but_skips_linked_skill_entries() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let agents_skills = tmp.path().join(".agents").join("skills");
        let real = agents_skills.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("SKILL.md"), b"real skill").unwrap();
        let outside = tmp.path().join("outside-skill");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("SKILL.md"), b"must not be attributed").unwrap();
        symlink(&outside, agents_skills.join("linked")).unwrap();
        let claude = tmp.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        symlink("../.agents/skills", claude.join("skills")).unwrap();

        let fingerprints = skills_fingerprint(tmp.path()).unwrap();

        assert!(fingerprints.contains_key("real"));
        assert!(!fingerprints.contains_key("linked"));
    }

    #[cfg(unix)]
    #[test]
    fn skills_fingerprint_refuses_an_unmanaged_root_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let outside = tmp.path().join("outside-skills");
        fs::create_dir_all(outside.join("leak")).unwrap();
        fs::write(outside.join("leak/SKILL.md"), b"outside").unwrap();
        let claude = tmp.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        symlink(&outside, claude.join("skills")).unwrap();

        assert!(skills_fingerprint(tmp.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn skills_fingerprint_refuses_symlinked_managed_parent_directories() {
        use std::os::unix::fs::symlink;

        let claude_parent = TempDir::new().unwrap();
        let outside_claude = claude_parent.path().join("outside-claude");
        fs::create_dir_all(outside_claude.join("skills/leak")).unwrap();
        fs::write(outside_claude.join("skills/leak/SKILL.md"), b"outside").unwrap();
        symlink(&outside_claude, claude_parent.path().join(".claude")).unwrap();
        assert!(skills_fingerprint(claude_parent.path()).is_none());

        let agents_parent = TempDir::new().unwrap();
        let outside_agents = agents_parent.path().join("outside-agents");
        fs::create_dir_all(outside_agents.join("skills/leak")).unwrap();
        fs::write(outside_agents.join("skills/leak/SKILL.md"), b"outside").unwrap();
        symlink(&outside_agents, agents_parent.path().join(".agents")).unwrap();
        fs::create_dir_all(agents_parent.path().join(".claude")).unwrap();
        symlink(
            "../.agents/skills",
            agents_parent.path().join(".claude/skills"),
        )
        .unwrap();
        assert!(skills_fingerprint(agents_parent.path()).is_none());
    }

    #[test]
    fn rebuild_fails_closed_when_a_canonical_turn_line_is_corrupt() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let turns = super::super::turns_mirror::turns_jsonl_path(project, "s1");
        fs::create_dir_all(turns.parent().unwrap()).unwrap();
        fs::write(&turns, b"{torn-canonical-turn\n").unwrap();

        let error = rebuild_experience(project, None).unwrap_err().to_string();
        assert!(error.contains("corrupt") && error.contains("s1"), "{error}");
    }

    #[test]
    fn rebuild_is_lossless_from_a_rich_canonical_terminal_fact_alone() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let progress = project.join("progress.jsonl");
        let usage = UnifiedTokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        let event = super::super::progress_bridge::build_chat_turn_completed_event_with_metadata(
            "reviewer",
            "s1",
            "turn-1",
            &usage,
            Some("claude-sonnet-4-6"),
            Some("claude"),
            &super::super::progress_bridge::ChatTurnCompletionMetadata {
                outcome: Some("completed".into()),
                duration_ms: Some(321),
                role_sha: Some("role-sha".into()),
                skills_sha: Some(BTreeMap::from([("research".into(), "skill-sha".into())])),
                invoked_skills: Some(vec!["research".into()]),
                signals: Some(TurnSignals {
                    tool_calls: 9,
                    steered: true,
                    error_recovered: None,
                }),
            },
        );
        super::super::progress_bridge::append_event(&progress, &event).unwrap();

        assert_eq!(
            rebuild_experience(project, Some(&progress)).unwrap(),
            (1, 0)
        );
        let record = read_all_experience(project).unwrap().remove(0);
        let ExperienceRecord::Turn(turn) = record else {
            panic!("expected rebuilt turn");
        };
        assert_eq!(turn.sid, "s1");
        assert_eq!(turn.role, "reviewer");
        assert_eq!(turn.role_sha.as_deref(), Some("role-sha"));
        assert_eq!(
            turn.skills_sha
                .as_ref()
                .and_then(|map| map.get("research"))
                .map(String::as_str),
            Some("skill-sha")
        );
        assert_eq!(turn.signals.tool_calls, 9);
        assert!(turn.signals.steered);
        assert_eq!(turn.duration_ms, Some(321));
        assert_eq!(
            turn.invoked_skills.as_deref(),
            Some(&["research".into()][..])
        );
    }

    #[test]
    fn rebuild_prefers_vendor_reported_turn_cost() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        let now = Utc::now();
        let meta = super::super::session_meta::SessionMeta {
            mode: None,
            managed_by: Default::default(),
            sid: "s1".into(),
            slug: "demo".into(),
            vendor: crate::AgentVendor::Claude,
            protocol: crate::SessionProtocol::Acp,
            role: "current-role".into(),
            permission_mode: crate::PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".into(),
            created_at: now.to_rfc3339(),
            last_active: now.to_rfc3339(),
            origin: super::super::session_meta::SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 1,
            cost_usd: None,
            tokens_total: None,
            role_sha: Some("current-role-sha".into()),
            skills_sha: Some(BTreeMap::from([(
                "current-skill".into(),
                "current-skill-sha".into(),
            )])),
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        super::super::session_meta::write_session_meta(project, &meta).unwrap();
        super::super::turns_mirror::append_turn(
            project,
            "s1",
            &super::super::turns_mirror::TurnRecord {
                turn_id: "turn-1".into(),
                ts: now,
                vendor: "opencode".into(),
                role: "historical-role".into(),
                user: "hi".into(),
                assistant: "partial".into(),
                usage: serde_json::to_value(UnifiedTokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    reported_cost_usd: Some(0.73),
                    ..Default::default()
                })
                .unwrap(),
                tool_calls: Vec::new(),
                attachments: Vec::new(),
                outcome: Some("failed".into()),
                error_kind: Some("max_tokens".into()),
                error: Some("output truncated".into()),
            },
        )
        .unwrap();

        assert_eq!(rebuild_experience(project, None).unwrap(), (1, 0));
        let record = read_all_experience(project).unwrap().remove(0);
        match record {
            ExperienceRecord::Turn(turn) => {
                assert_eq!(turn.cost_usd, Some(0.73));
                assert_eq!(turn.vendor, "opencode");
                assert_eq!(turn.role, "historical-role");
                assert_eq!(turn.outcome.as_deref(), Some("failed"));
                assert_eq!(turn.duration_ms, None);
                assert_eq!(turn.role_sha, None);
                assert_eq!(turn.skills_sha, None);
                assert_eq!(turn.invoked_skills, None);
            }
            other => panic!("expected turn, got {other:?}"),
        }
    }

    #[test]
    fn rebuild_projects_completion_metadata_and_latest_canonical_verdict() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path();
        // Seed meta + turns for s1.
        let now = Utc::now();
        let meta = super::super::session_meta::SessionMeta {
            mode: None,
            managed_by: Default::default(),
            sid: "s1".into(),
            slug: "demo".into(),
            vendor: crate::AgentVendor::Claude,
            protocol: crate::SessionProtocol::StreamJson,
            role: "cto".into(),
            permission_mode: crate::PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".into(),
            created_at: now.to_rfc3339(),
            last_active: now.to_rfc3339(),
            origin: super::super::session_meta::SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 1,
            cost_usd: None,
            tokens_total: None,
            role_sha: Some("deadbeef0001".into()),
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        };
        super::super::session_meta::write_session_meta(project, &meta).unwrap();
        super::super::turns_mirror::append_turn(
            project,
            "s1",
            &super::super::turns_mirror::TurnRecord {
                turn_id: "s1-1".into(),
                ts: now,
                vendor: "claude".into(),
                role: "cto".into(),
                user: "hi".into(),
                assistant: "hello".into(),
                usage: serde_json::Value::Null,
                tool_calls: vec![],
                attachments: vec![],
                outcome: Some("completed".into()),
                error_kind: None,
                error: None,
            },
        )
        .unwrap();

        let progress_path = project.join("progress.jsonl");
        let completion =
            super::super::progress_bridge::build_chat_turn_completed_event_with_metadata(
                "cto",
                "s1",
                "s1-1",
                &UnifiedTokenUsage::default(),
                Some("claude-opus-4-8"),
                Some("claude"),
                &super::super::progress_bridge::ChatTurnCompletionMetadata {
                    outcome: Some("completed".into()),
                    duration_ms: Some(250),
                    role_sha: Some("turn-role-sha".into()),
                    skills_sha: Some(BTreeMap::from([(
                        "research".into(),
                        "turn-skill-sha".into(),
                    )])),
                    invoked_skills: None,
                    signals: None,
                },
            );
        super::super::progress_bridge::append_event(&progress_path, &completion).unwrap();
        super::super::progress_bridge::append_turn_verdict_if_changed(
            &progress_path,
            &super::super::progress_bridge::TurnVerdict {
                sid: "s1".into(),
                turn_id: "s1-1".into(),
                ts: now,
                verdict: Verdict::Accept,
                feedback: None,
            },
        )
        .unwrap();
        super::super::progress_bridge::append_turn_verdict_if_changed(
            &progress_path,
            &super::super::progress_bridge::TurnVerdict {
                sid: "s1".into(),
                turn_id: "s1-1".into(),
                ts: now + chrono::Duration::seconds(1),
                verdict: Verdict::Revise,
                feedback: Some("try again".into()),
            },
        )
        .unwrap();

        // Stale derived data must never outrank canonical progress facts.
        append_experience(
            project,
            &ExperienceRecord::Verdict(VerdictExperience {
                sid: "s1".into(),
                turn_id: "s1-1".into(),
                ts: now,
                verdict: Verdict::Accept,
                feedback: Some("stale".into()),
            }),
        )
        .unwrap();

        let (n1, v1) = rebuild_experience(project, Some(&progress_path)).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(v1, 1);
        let recs = read_all_experience(project).unwrap();
        assert_eq!(recs.len(), 2);
        assert!(matches!(
            &recs[0],
            ExperienceRecord::Turn(t)
                if t.turn_id == "s1-1"
                    && t.outcome.as_deref() == Some("completed")
                    && t.duration_ms == Some(250)
                    && t.role_sha.as_deref() == Some("turn-role-sha")
                    && t.skills_sha.as_ref().and_then(|skills| skills.get("research")).map(String::as_str) == Some("turn-skill-sha")
        ));
        assert!(matches!(
            &recs[1],
            ExperienceRecord::Verdict(v)
                if matches!(v.verdict, Verdict::Revise)
                    && v.feedback.as_deref() == Some("try again")
        ));

        // Second rebuild is idempotent (same shape).
        let (n2, v2) = rebuild_experience(project, Some(&progress_path)).unwrap();
        assert_eq!((n2, v2), (n1, v1));
        let recs2 = read_all_experience(project).unwrap();
        assert_eq!(recs2.len(), recs.len());
        match (&recs[0], &recs2[0]) {
            (ExperienceRecord::Turn(a), ExperienceRecord::Turn(b)) => {
                assert_eq!(a.sid, b.sid);
                assert_eq!(a.turn_id, b.turn_id);
                assert_eq!(a.role_sha, b.role_sha);
            }
            _ => panic!("expected turns"),
        }
    }
}
