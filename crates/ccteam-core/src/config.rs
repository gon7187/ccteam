//! V0.4.2 F73 — global ccteam configuration file `~/.ccteam/config.yaml`.
//!
//! Single source of truth for user-level preferences AND the project
//! registry. Replaces (and consolidates) the V0.4.1 layout where
//! `projects_root` came only from env vars and project discovery
//! relied on walking the filesystem.
//!
//! ## Shape
//!
//! ```yaml
//! projects_root: ~/projects        # optional; default ~/projects
//! projects:                         # canonical SoT for daemon roster
//!   - slug: myapp
//!     path: /home/rob/code/my-fastapi-app
//!     team: dev
//!     installed_at: 2026-05-15T14:00:00Z
//! ```
//!
//! ## Read priority (CcteamPaths::from_env)
//!
//! 1. `CCTEAM_PROJECTS_ROOT` env (ad-hoc / test override)
//! 2. `~/.ccteam/config.yaml::projects_root`
//! 3. `~/projects` (hardcoded default)
//!
//! ## Atomic save
//!
//! `save()` writes to `config.yaml.tmp`, renames into place, and copies
//! the prior contents to `config.yaml.bak` first — same shape as
//! `ProjectState::save` so a crash mid-write doesn't corrupt the SoT.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::fd::AsRawFd as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// File name relative to `paths.root` (`~/.ccteam/`).
pub const CONFIG_FILENAME: &str = "config.yaml";
const CONFIG_LOCK_FILENAME: &str = "config.lock";
/// Environment override for [`DaemonConfig::workers`].
pub const DAEMON_WORKERS_ENV: &str = "CCTEAM_DAEMON_WORKERS";

/// Top-level config schema. Future fields plug in as their own
/// optional sections without breaking existing files — `serde(default)`
/// on every collection guarantees an older config.yaml still parses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CcteamConfig {
    /// Canonical base for `ccteam init --in <slug>`. When absent,
    /// `CcteamPaths::from_env` falls back to `$HOME/projects`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_root: Option<PathBuf>,

    /// Optional project slug used by admin MCP `session_spawn` after the
    /// explicit/cwd/sole-project tiers. The slug is validated against the
    /// live catalog at use time; an absent or stale value is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_project: Option<String>,

    /// Every project under ccteam management — daemon roster reads
    /// this list instead of walking the filesystem. Empty on a fresh
    /// install; `ccteam init` appends one entry per successful install.
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,

    /// V0.4.2 F74: watchdog tunables, folded in from the legacy
    /// `~/.ccteam/watchdog.yaml`. When absent, watchdog uses defaults
    /// (or, on V0.4.1 systems pre-migration, falls back to reading
    /// `watchdog.yaml` directly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchdog: Option<crate::watchdog::WatchdogConfig>,

    /// Daemon runtime sizing. Absent → documented defaults.
    #[serde(default, skip_serializing_if = "DaemonConfig::is_default")]
    pub daemon: DaemonConfig,

    /// V0.4.6 F85: how many days a terminated `~/.claude/jobs/<id>/`
    /// directory may live before the daemon's startup GC sweep (or
    /// `ccteam doctor --gc-claude-jobs --apply`) reclaims it. Default
    /// 7 days. Setting `0` disables GC entirely (every entry is
    /// preserved), which is useful for forensic captures or shared
    /// hosts where ccteam shouldn't touch sibling tools' state.
    #[serde(default = "default_claude_jobs_retention_days")]
    pub claude_jobs_retention_days: u32,

    /// v0.9.2 — daemon-wide live-session capacity. Absent → the documented
    /// default; the gateway gracefully evicts the least-recently-active live
    /// session before admitting a fresh or revived one.
    #[serde(default, skip_serializing_if = "SessionsConfig::is_default")]
    pub sessions: SessionsConfig,

    /// v0.9.0 W2 (F5) — delegation guardrails. Absent → all documented
    /// defaults (zero-config runs safely). Global engine policy the gateway
    /// enforces on every agent-initiated (Ambient) spawn/dispatch.
    #[serde(default, skip_serializing_if = "DelegationConfig::is_default")]
    pub delegation: DelegationConfig,

    /// Настройки обмена сообщениями, включая кнопки быстрых шаблонов Telegram.
    #[serde(default)]
    pub im: ImConfig,
}

/// Настройки обмена сообщениями, общие для всех настроенных каналов.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImConfig {
    /// Постоянные шаблоны клавиатуры ответов, доступные в Telegram.
    #[serde(default = "default_quick_templates")]
    pub quick_templates: Vec<QuickTemplate>,
}

impl Default for ImConfig {
    fn default() -> Self {
        Self {
            quick_templates: default_quick_templates(),
        }
    }
}

/// Одна настраиваемая пользователем кнопка быстрого шаблона.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuickTemplate {
    /// Текст кнопки для пользователя. Начальные и конечные пробелы игнорируются
    /// при отображении и сопоставлении с входящим текстом.
    pub label: String,
    /// Префикс, добавляемый перед следующим обычным сообщением пользователя.
    pub prefix: String,
}

/// Reserved Telegram action. Unlike configurable quick templates this prompt
/// is compiled into the current binary, so an old serialized default cannot
/// pin Commander to stale routing or fallback policy.
pub const COMMANDER_QUICK_TEMPLATE_LABEL: &str = "🎯 Командир";

pub fn commander_quick_template() -> QuickTemplate {
    QuickTemplate {
        label: COMMANDER_QUICK_TEMPLATE_LABEL.to_string(),
        prefix: concat!(
            "Call status first. If it reports project_required, name the project explicitly in status; use the workspace the user identified, never infer one from cwd. Orchestrate through session_spawn, session_dispatch, session_list and session_collect. The commander is Claude Opus at medium effort (high for a difficult deviation). If this session is neither Opus nor an explicitly selected Codex Sol commander fallback, delegate command once to Opus and hand over the task; a confirmed capability fallback must not spawn Opus again. Keep the commander focused on task boundaries, dependencies, budgets and acceptance. Delegate implementation; keep working context compact with file paths, commits, checks and unresolved decisions.\n\n",
            "Size by risk: a small independent fix needs a short brief, one implementer and independent acceptance. A medium task needs a one-page Fable high plan and bounded implementation tasks. Cross-module changes, money, credentials, ACL, data migrations, concurrency and project red lines are complex regardless of file count: use a Fable high plan, one independent Sol high plan review, and the project's full acceptance gates. Record plan, task owners, dependencies, allowed paths, acceptance checks and deviations in <project>/.ccteam/plans/<date>-<topic>.md. Keep the approved plan stable; append amendments with reasons. Research unfamiliar interfaces with a read-only GLM scout; GitHub precedents are useful when needed, never a mandatory ritual for an understood fix.\n\n",
            "Roster: GLM Flash through OpenCode, model zai-coding-plan/glm-5.3-flash, handles preparation, reproduction, tests, documentation, templates and isolated implementation under a checkable contract. Give it substantial bounded work; do not limit it to summaries. Codex Luna medium handles small independent fixes. Claude Sonnet medium or Codex Terra medium handles ordinary development requiring independent decisions; choose by task fit and the remaining allowance of that subscription. Codex Sol high or Claude Fable high handles difficult implementation. Fable high plans; Opus medium commands. Independent final acceptance uses a fresh Codex Astra: low for obvious changes without new logic, medium for modest logic, high for complex or risky diffs. Review complete phases with the required source context. Any mandatory repository review pair remains mandatory: Astra does not waive an Opus + Sol gate or any other project rule. The final reviewer must not be the implementer or its advisor. Resolve model ids and supported effort from status/runtime evidence; never interpret a stale advisory catalog as a whitelist, invent max, or choose the highest effort by default.\n\n",
            "Subscription control: use the real quota windows from status, including plan, source, observed_at, used_percent, model scope and resets_at. Codex currently has a general weekly window, not a general 5-hour window; Claude has both 5-hour and weekly windows. Follow the reported duration, never assume primary means 5 hours. A separate Spark window applies only to Spark; a Fable scoped weekly limit and Claude's general limits all constrain Fable. Remaining percent = 100 - used_percent for each applicable window. Missing, unavailable, malformed or stale data is unknown, never zero use or unlimited capacity. A cached reading keeps its original observed_at; if older than 60 seconds, request status again before new paid work. An authorization error means restore the normal session identity or report it, never bypass the gate or claim the subscription is absent.\n\n",
            "At run start, record a fixed allowance separately for Claude, Codex and every relevant model scope in the plan. Honor the user's explicit allowance; otherwise pace each weekly pool until reset: this run may consume at most remaining weekly percentage points / max(1, ceil(hours until reset / 24)). Also stay within Claude's remaining 5-hour window. Reserve one quarter of each run allowance for necessary review and corrections; do not rebase the allowance after each wave or session restart. Save baseline percentages and reset timestamps, then compare new observations before spawning and after every completed task. If a window resets, re-read it and record the new baseline explicitly. GLM is the preferred inexpensive lane for suitable work; raw tokens_24h and API dollar prices are diagnostics, not subscription balances, and must not be equalized across vendors. This is observed pacing, not a promise of exact per-task quota cost: start with one bounded task when cost is unknown and measure the delta. If allowance or an applicable quota is exhausted, stop new work on that pool, preserve commits and handoff, and wait until its reset; use another capable lane only if its own allowance and the required quality gates permit it. Never enable paid overage, buy credits, redeem reset credits or lower acceptance to keep running. If paid quota is unknown, continue suitable GLM work and report what remains blocked.\n\n",
            "Execution: use one writer per isolated git worktree and non-overlapping conflict domain, following the project's branch/worktree rules; never overwrite another session's work. Without git, work sequentially and do not initialize a repository. Each brief states the exact goal, allowed paths, acceptance criterion, dependencies and chosen model/effort, plus the absolute plan path. Return a concise status, commit, checks with outcomes and unresolved issues, without code or diff dumps. Workers run relevant checks and commit completed steps; one expensive test run at a time. A GLM pre-gate checks the phase diff, acceptance checklist and relevant tests before expensive final review. Reuse that validator for the phase, not a fresh agent for every command. After one unsuccessful correction of the same defect, hand it to the next capable implementation tier with reproduction and evidence; do not raise effort blindly on every retry. Ask Sol/Fable for a bounded difficult decision only when needed, not a permanent advisor on every edit. Default concurrency: at most one Claude and one Codex implementation worker plus three GLM workers; include commander, planner and reviews in the subscription accounting. Lower concurrency under memory/load pressure; do not kill unrelated sessions.\n\n",
            "Integration and acceptance: a GLM git agent integrates completed branches into the project's development branch via an isolated integration worktree and runs the full required checks. Conflicts or subsequent edits invalidate approval of affected code. Reviewers approve the same immutable revision; a repeat review receives the delta plus enough source context and verifies that earlier findings were closed. Cap unresolved review loops at two, then report the concrete disagreement and preserve progress. After green checks and required approvals, the git agent pushes development, opens/updates the PR and merges with a merge commit only when project policy and user authorization permit it. Never push main directly or bypass branch protection. Remove only our safely merged temporary worktrees/branches. Tag, release and deployment require their own explicit authorization.\n\n",
            "Fallback and monitoring: a typed vendor_unavailable, model_unavailable or effort_unavailable permits one capability fallback. Opus commander -> Sol high; Fable planner/implementer -> Sol high; Sol -> Fable high; Luna -> Sonnet medium; Sonnet <-> Terra medium; GLM -> Luna medium; Astra acceptance -> a fresh Sol at appropriate effort or the required independent project gate. Keep task scope, quality and subscription allowance intact. Authentication/ACL, quota/budget, depth/cycle, timeout, transport and unknown outcomes do not authorize blind retry or downgrade: collect/list first, report the original error, and resolve the cause. Two confirmed server_overloaded errors or an unproductive result after one correction may justify a capable handoff. Before replacing a silent session, inspect activity, last_active and waiting_approval; waiting for approval is not stuck, and a lost response does not prove a failed task. React to completion notifications instead of polling in a tick loop. Stop our finished delegates when no longer needed; save the plan and branch before any explicit handoff. The final report lists delivered commits/PR, checks, remaining branches, actual quota changes per subscription/model window with reset times, unknown readings and any blocked work.\n\n",
            "Task:",
        )
        .to_string(),
    }
}

/// Встроенные кнопки быстрых шаблонов для новой конфигурации.
pub fn default_quick_templates() -> Vec<QuickTemplate> {
    vec![
        commander_quick_template(),
        QuickTemplate {
            label: "🚗 Водитель+советник".to_string(),
            prefix: "Ты ведёшь задачу сам: двигай её вперёд напрямую. Если застрял или столкнулся с неопределённым решением, запусти через session_spawn сессию советника claude в этом же проекте, передай ей контекст, а затем сам выполни её рекомендации. Задача:".to_string(),
        },
        QuickTemplate {
            label: "🔁 Кросс-ревью".to_string(),
            prefix: "Собери решение и проведи кросс-ревью: передай требование ниже сессии codex (небольшие шаги, тесты не должны падать); после завершения передай diff сессии другого провайдера на независимую проверку (корректность / безопасность / риск регрессий). Спорные места оставь себе на арбитраж; попроси автора исправить серьёзные замечания и повторно проверить результат, затем сообщи сводку изменений и вердикт ревью. Требование:".to_string(),
        },
        QuickTemplate {
            label: "⚔️ Батл".to_string(),
            prefix: "Отправь сложную задачу ниже 2–3 свежим сессиям разных провайдеров, чтобы они независимо решили её (не подглядывая). Когда все закончат, сравни подходы и подтверждения, собери лучший итоговый ответ и укажи компромиссы. Проблема:".to_string(),
        },
        QuickTemplate {
            label: "🔺 Триангуляция".to_string(),
            prefix: "Проведи триангуляцию темы ниже: session_spawn для grok — поиск по X и актуальным обсуждениям, claude — глубокий веб-анализ, codex — проверка по исходному коду. Сопоставь три направления и объедини их в один вывод с источниками. Тема:".to_string(),
        },
        QuickTemplate {
            label: "🏗 Пирамида".to_string(),
            prefix: "Ниже — набор механических задач (массовые переименования / уборка форматирования / разбор тестов). Через session_spawn поручи недорогим провайдерам (kimi / opencode) пройти их по одной; ошибки и решения, требующие суждения, эскалируй более сильной модели, а прогресс отмечай в общем чек-листе. Список задач:".to_string(),
        },
    ]
}

/// Daemon runtime sizing. Every field defaults so existing config files remain
/// valid and zero-config installs get a bounded multi-thread runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaemonConfig {
    /// Tokio async worker threads used by `ccteam start`.
    #[serde(default = "default_daemon_workers")]
    pub workers: usize,
}

pub fn default_daemon_workers() -> usize {
    4
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            workers: default_daemon_workers(),
        }
    }
}

impl DaemonConfig {
    /// Resolve the worker count, with the environment taking precedence over
    /// `config.yaml`. Zero is rejected before it reaches Tokio's builder.
    pub fn effective_workers(&self) -> Result<usize> {
        let configured = match std::env::var(DAEMON_WORKERS_ENV) {
            Ok(raw) => raw.parse::<usize>().with_context(|| {
                format!("parse {DAEMON_WORKERS_ENV}={raw:?} as a positive integer")
            })?,
            Err(std::env::VarError::NotPresent) => self.workers,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(anyhow!("{DAEMON_WORKERS_ENV} is not valid UTF-8"));
            }
        };
        if configured == 0 {
            return Err(anyhow!(
                "daemon.workers must be at least 1 (set in config.yaml or {DAEMON_WORKERS_ENV})"
            ));
        }
        Ok(configured)
    }

    /// True when this section matches the built-in default, allowing config
    /// serialization to omit it for byte-stability on untouched installs.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// v0.9.2 — daemon-wide live-session capacity. Every field defaults so older
/// config files remain valid and zero-config installs get the standard cap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionsConfig {
    /// Maximum number of concurrently live sessions daemon-wide.
    #[serde(default = "default_sessions_max_live")]
    pub max_live: u32,
}

pub fn default_sessions_max_live() -> u32 {
    50
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            max_live: default_sessions_max_live(),
        }
    }
}

impl SessionsConfig {
    /// True when this section matches the built-in default, allowing config
    /// serialization to omit it for byte-stability on untouched installs.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// v0.9.0 W2 (F5) — delegation guardrail knobs. Every field defaults, so an
/// absent `delegation:` section (or absent individual keys) yields the
/// documented anti-runaway posture without any config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelegationConfig {
    /// Max delegation depth. A delegated child's depth is `parent.depth + 1`
    /// (a human-created session is depth 0); a spawn that would exceed this is
    /// rejected.
    #[serde(default = "default_delegation_max_depth")]
    pub max_depth: u32,
    /// Max active (non-stopped) DIRECT children a single parent may hold.
    #[serde(default = "default_delegation_max_children")]
    pub max_children: u32,
    /// Max active delegated sessions (any `parent_sid`) in one project — the
    /// runaway-minting ceiling.
    #[serde(default = "default_delegation_max_delegated")]
    pub max_delegated: u32,
}

pub fn default_delegation_max_depth() -> u32 {
    2
}
pub fn default_delegation_max_children() -> u32 {
    10
}
pub fn default_delegation_max_delegated() -> u32 {
    50
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            max_depth: default_delegation_max_depth(),
            max_children: default_delegation_max_children(),
            max_delegated: default_delegation_max_delegated(),
        }
    }
}

impl DelegationConfig {
    /// True when this equals the built-in default posture — lets the config
    /// writer omit the section so an untouched `config.yaml` stays byte-stable.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Default value for `claude_jobs_retention_days` when the field is
/// absent from `config.yaml`. Kept as a free function so serde's
/// `#[serde(default = "...")]` can reference it.
pub fn default_claude_jobs_retention_days() -> u32 {
    7
}

impl Default for CcteamConfig {
    fn default() -> Self {
        Self {
            projects_root: None,
            default_project: None,
            projects: Vec::new(),
            watchdog: None,
            daemon: DaemonConfig::default(),
            claude_jobs_retention_days: default_claude_jobs_retention_days(),
            sessions: SessionsConfig::default(),
            delegation: DelegationConfig::default(),
            im: ImConfig::default(),
        }
    }
}

/// One project registry entry. `path` is absolute; `team` mirrors
/// `state.json::team` so the registry can answer `ccteam ls` without
/// loading every state.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectEntry {
    pub slug: String,
    pub path: PathBuf,
    /// Execution host bound to this project. Sessions inherit this value;
    /// callers can no longer choose a host per spawn.
    #[serde(default = "default_project_host")]
    pub host: String,
    /// Satellite-local project slug used on the exec wire. `None` for local
    /// projects (and old config entries, which deserialize as local).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_slug: Option<String>,
    /// Satellite-local working-tree path, retained for display only. Daemon
    /// bookkeeping always uses [`Self::path`], the local data home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<PathBuf>,
    pub team: String,
    pub installed_at: DateTime<Utc>,
}

pub fn default_project_host() -> String {
    "local".to_string()
}

/// Absolute path to the config file under the given `~/.ccteam/`
/// root. Pure path arithmetic; never touches disk.
pub fn config_path(ccteam_root: &Path) -> PathBuf {
    ccteam_root.join(CONFIG_FILENAME)
}

/// Load `<root>/config.yaml`. Missing file → `Default::default()`
/// (zero projects, no `projects_root` override). An empty file (e.g.
/// the user `touch`ed it) is also treated as defaults.
///
/// Parse errors propagate — a corrupt config.yaml is a fail-loud
/// condition (we don't silently fall back to defaults because that
/// would erase the user's registry on a YAML typo).
pub fn load(ccteam_root: &Path) -> Result<CcteamConfig> {
    let path = config_path(ccteam_root);
    if !path.exists() {
        return Ok(CcteamConfig::default());
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(CcteamConfig::default());
    }
    serde_yaml::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Persist `cfg` atomically. Steps:
///
/// 1. Ensure `<root>/` exists.
/// 2. If `config.yaml` already exists, copy it to `config.yaml.bak`.
/// 3. Write serialized YAML to `config.yaml.tmp`.
/// 4. `rename` tmp → final.
///
/// This mirrors `ProjectState::save` so a crash between steps leaves
/// either the prior `.bak` or the next-version `.tmp` recoverable.
pub fn save(ccteam_root: &Path, cfg: &CcteamConfig) -> Result<()> {
    let _lock = ConfigFileLock::acquire(ccteam_root)?;
    save_unlocked(ccteam_root, cfg)
}

fn save_unlocked(ccteam_root: &Path, cfg: &CcteamConfig) -> Result<()> {
    std::fs::create_dir_all(ccteam_root)
        .with_context(|| format!("create {}", ccteam_root.display()))?;
    let path = config_path(ccteam_root);
    let yaml = serde_yaml::to_string(cfg).context("serialize ccteam config")?;

    if path.exists() {
        let bak = path.with_extension("yaml.bak");
        std::fs::copy(&path, &bak)
            .with_context(|| format!("backup {} → {}", path.display(), bak.display()))?;
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    std::fs::File::open(ccteam_root)
        .with_context(|| format!("open config directory {}", ccteam_root.display()))?
        .sync_all()
        .with_context(|| format!("sync config directory {}", ccteam_root.display()))?;
    Ok(())
}

#[cfg(unix)]
struct ConfigFileLock(std::fs::File);

#[cfg(unix)]
impl ConfigFileLock {
    fn acquire(ccteam_root: &Path) -> Result<Self> {
        let state = ccteam_root.join("state");
        std::fs::create_dir_all(&state)
            .with_context(|| format!("create config lock directory {}", state.display()))?;
        let path = state.join(CONFIG_LOCK_FILENAME);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("open config lock {}", path.display()))?;
        if !file
            .metadata()
            .with_context(|| format!("stat config lock {}", path.display()))?
            .is_file()
        {
            return Err(anyhow!(
                "config lock is not a regular file: {}",
                path.display()
            ));
        }
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("lock config {}", path.display()));
        }
        Ok(Self(file))
    }
}

#[cfg(unix)]
impl Drop for ConfigFileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct ConfigFileLock(std::sync::MutexGuard<'static, ()>);

#[cfg(not(unix))]
impl ConfigFileLock {
    fn acquire(_ccteam_root: &Path) -> Result<Self> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        Ok(Self(
            LOCK.get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        ))
    }
}

fn project_progress_path(ccteam_root: &Path, slug: &str) -> PathBuf {
    ccteam_root
        .join("state")
        .join("progress")
        .join(format!("{slug}.jsonl"))
}

fn validate_project_progress_generation(
    ccteam_root: &Path,
    slug: &str,
    already_registered: bool,
) -> Result<()> {
    use ccteam_harness::execution::progress_bridge::ProgressSlugReservation;

    let progress_path = project_progress_path(ccteam_root, slug);
    if already_registered {
        if ccteam_harness::execution::progress_bridge::progress_state_is_retired(&progress_path)? {
            return Err(anyhow!(
                "project slug `{slug}` is permanently retired; create the project under a fresh numeric slug"
            ));
        }
        return Ok(());
    }
    match ccteam_harness::execution::progress_bridge::progress_slug_reservation(&progress_path)? {
        ProgressSlugReservation::Free => Ok(()),
        ProgressSlugReservation::Retired => Err(anyhow!(
            "project slug `{slug}` is permanently retired; choose a fresh numeric slug"
        )),
        ProgressSlugReservation::ActiveState => Err(anyhow!(
            "project slug `{slug}` is reserved by existing progress state; choose a fresh numeric slug"
        )),
    }
}

/// Fail before a project scaffold/refresh touches project-local files when its
/// durable progress generation has already been retired. Registry writers run
/// the same check again as their last line of defense.
pub fn preflight_project_upsert(ccteam_root: &Path, slug: &str) -> Result<()> {
    let cfg = load(ccteam_root)?;
    validate_project_progress_generation(
        ccteam_root,
        slug,
        cfg.projects.iter().any(|entry| entry.slug == slug),
    )
}

/// Append `entry` to `config.yaml::projects`. Fails loud on slug
/// collision — the caller (e.g. `ccteam init`) should detect the
/// collision earlier so the user gets a clearer error, but this is
/// the last line of defense.
/// Register a local project at `path`. `collect_projects` reads config.yaml
/// only, so anything that materializes `<path>/.ccteam/state.json` and wants
/// it listed must also call this.
pub fn register_local_project(
    ccteam_root: &Path,
    slug: &str,
    path: PathBuf,
    team: &str,
) -> Result<()> {
    append_project(
        ccteam_root,
        ProjectEntry {
            slug: slug.to_string(),
            path,
            host: default_project_host(),
            remote_slug: None,
            remote_path: None,
            team: team.to_string(),
            installed_at: Utc::now(),
        },
    )
}

pub fn append_project(ccteam_root: &Path, entry: ProjectEntry) -> Result<()> {
    let _lock = ConfigFileLock::acquire(ccteam_root)?;
    let mut cfg = load(ccteam_root)?;
    if cfg.projects.iter().any(|p| p.slug == entry.slug) {
        return Err(anyhow!(
            "slug `{}` already registered in {}",
            entry.slug,
            config_path(ccteam_root).display()
        ));
    }
    validate_project_progress_generation(ccteam_root, &entry.slug, false)?;
    cfg.projects.push(entry);
    save_unlocked(ccteam_root, &cfg)
}

/// Update or insert `entry`. Used by `ccteam init` re-runs against an
/// already-registered slug — refresh `path` / `team` / `installed_at`
/// without erroring on collision.
pub fn upsert_project(ccteam_root: &Path, entry: ProjectEntry) -> Result<()> {
    let _lock = ConfigFileLock::acquire(ccteam_root)?;
    let mut cfg = load(ccteam_root)?;
    let already_registered = cfg.projects.iter().any(|p| p.slug == entry.slug);
    validate_project_progress_generation(ccteam_root, &entry.slug, already_registered)?;
    if let Some(existing) = cfg.projects.iter_mut().find(|p| p.slug == entry.slug) {
        *existing = entry;
    } else {
        cfg.projects.push(entry);
    }
    save_unlocked(ccteam_root, &cfg)
}

/// Remove `slug` from the registry. Returns `true` iff the slug was
/// present.
pub fn remove_project(ccteam_root: &Path, slug: &str) -> Result<bool> {
    let _lock = ConfigFileLock::acquire(ccteam_root)?;
    let mut cfg = load(ccteam_root)?;
    let before = cfg.projects.len();
    cfg.projects.retain(|p| p.slug != slug);
    if cfg.projects.len() == before {
        return Ok(false);
    }
    save_unlocked(ccteam_root, &cfg)?;
    Ok(true)
}

/// Find a registered project by slug.
pub fn lookup_project(ccteam_root: &Path, slug: &str) -> Result<Option<ProjectEntry>> {
    let cfg = load(ccteam_root)?;
    Ok(cfg.projects.into_iter().find(|p| p.slug == slug))
}

/// Pick a daemon-catalog slug by appending readable numeric suffixes on
/// registry collision (`demo`, `demo2`, `demo3`, ...). The caller validates
/// the base grammar before calling; this helper owns only catalog uniqueness.
pub fn pick_unused_project_slug(ccteam_root: &Path, base: &str) -> Result<String> {
    let cfg = load(ccteam_root)?;
    let used: std::collections::HashSet<&str> = cfg
        .projects
        .iter()
        .map(|entry| entry.slug.as_str())
        .collect();
    if !used.contains(base)
        && !ccteam_harness::execution::progress_bridge::progress_slug_is_reserved(
            &project_progress_path(ccteam_root, base),
        )?
    {
        return Ok(base.to_string());
    }
    for n in 2u32.. {
        let candidate = format!("{base}{n}");
        if !used.contains(candidate.as_str())
            && !ccteam_harness::execution::progress_bridge::progress_slug_is_reserved(
                &project_progress_path(ccteam_root, &candidate),
            )?
        {
            return Ok(candidate);
        }
    }
    unreachable!("integer accumulation always finds a free project slug")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn sample_entry(slug: &str, path: &Path) -> ProjectEntry {
        ProjectEntry {
            slug: slug.into(),
            path: path.to_path_buf(),
            host: default_project_host(),
            remote_slug: None,
            remote_path: None,
            team: "dev".into(),
            installed_at: now(),
        }
    }

    #[test]
    fn load_returns_default_on_missing_file() {
        let tmp = TempDir::new().unwrap();
        let cfg = load(tmp.path()).unwrap();
        assert!(cfg.projects_root.is_none());
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn load_returns_default_on_empty_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(config_path(tmp.path()), "").unwrap();
        let cfg = load(tmp.path()).unwrap();
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn legacy_project_entry_defaults_to_local_binding() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            config_path(tmp.path()),
            "projects:\n  - slug: demo\n    path: /srv/demo\n    team: dev\n    installed_at: 2026-01-01T00:00:00Z\n",
        )
        .unwrap();
        let entry = load(tmp.path()).unwrap().projects.remove(0);
        assert_eq!(entry.host, "local");
        assert!(entry.remote_slug.is_none());
        assert!(entry.remote_path.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let entry = sample_entry("foo", &PathBuf::from("/home/rob/code/foo"));
        let cfg = CcteamConfig {
            projects_root: Some(PathBuf::from("/work/repos")),
            default_project: Some("foo".to_string()),
            projects: vec![entry.clone()],
            watchdog: None,
            daemon: DaemonConfig::default(),
            claude_jobs_retention_days: default_claude_jobs_retention_days(),
            sessions: SessionsConfig::default(),
            delegation: DelegationConfig::default(),
            im: ImConfig::default(),
        };
        save(tmp.path(), &cfg).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn save_writes_bak_on_overwrite() {
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &CcteamConfig::default()).unwrap();
        let entry = sample_entry("bar", &PathBuf::from("/x/bar"));
        save(
            tmp.path(),
            &CcteamConfig {
                projects: vec![entry],
                ..Default::default()
            },
        )
        .unwrap();
        let bak = config_path(tmp.path()).with_extension("yaml.bak");
        assert!(
            bak.is_file(),
            "save must keep a .bak after the first overwrite"
        );
    }

    #[test]
    fn append_project_rejects_collision() {
        let tmp = TempDir::new().unwrap();
        let entry = sample_entry("dup", &PathBuf::from("/x/dup"));
        append_project(tmp.path(), entry.clone()).unwrap();
        let err = append_project(tmp.path(), entry).unwrap_err();
        assert!(format!("{err:#}").contains("already registered"));
    }

    #[test]
    fn concurrent_project_appends_do_not_lose_registry_rows() {
        let tmp = TempDir::new().unwrap();
        let root = std::sync::Arc::new(tmp.path().to_path_buf());
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers = (0..8)
            .map(|n| {
                let root = std::sync::Arc::clone(&root);
                let start = std::sync::Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    append_project(
                        &root,
                        sample_entry(&format!("p{n}"), Path::new(&format!("/x/p{n}"))),
                    )
                    .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }

        let loaded = load(&root).unwrap();
        assert_eq!(loaded.projects.len(), 8);
        assert_eq!(
            loaded
                .projects
                .iter()
                .map(|entry| entry.slug.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            8
        );
    }

    #[test]
    fn upsert_project_overwrites_existing_entry() {
        let tmp = TempDir::new().unwrap();
        let first = sample_entry("foo", &PathBuf::from("/old/path"));
        upsert_project(tmp.path(), first).unwrap();
        let updated = sample_entry("foo", &PathBuf::from("/new/path"));
        upsert_project(tmp.path(), updated.clone()).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0], updated);
    }

    #[test]
    fn remove_project_returns_true_on_hit_false_on_miss() {
        let tmp = TempDir::new().unwrap();
        append_project(tmp.path(), sample_entry("a", &PathBuf::from("/x/a"))).unwrap();
        assert!(remove_project(tmp.path(), "a").unwrap());
        assert!(!remove_project(tmp.path(), "a").unwrap());
    }

    #[test]
    fn lookup_project_finds_or_returns_none() {
        let tmp = TempDir::new().unwrap();
        let e = sample_entry("hit", &PathBuf::from("/x/hit"));
        append_project(tmp.path(), e.clone()).unwrap();
        assert_eq!(lookup_project(tmp.path(), "hit").unwrap(), Some(e));
        assert_eq!(lookup_project(tmp.path(), "miss").unwrap(), None);
    }

    #[test]
    fn pick_unused_project_slug_uses_numeric_suffixes() {
        let tmp = TempDir::new().unwrap();
        append_project(tmp.path(), sample_entry("demo", Path::new("/x/demo"))).unwrap();
        append_project(tmp.path(), sample_entry("demo2", Path::new("/x/demo2"))).unwrap();
        assert_eq!(
            pick_unused_project_slug(tmp.path(), "demo").unwrap(),
            "demo3"
        );
        assert_eq!(
            pick_unused_project_slug(tmp.path(), "free").unwrap(),
            "free"
        );
    }

    #[test]
    fn retired_project_slug_is_never_reused_or_refreshed() {
        let tmp = TempDir::new().unwrap();
        let entry = sample_entry("demo", Path::new("/x/demo"));
        append_project(tmp.path(), entry.clone()).unwrap();
        let progress = project_progress_path(tmp.path(), "demo");
        ccteam_harness::execution::progress_bridge::mark_progress_retired(&progress).unwrap();

        let refresh_error = upsert_project(tmp.path(), entry).unwrap_err().to_string();
        assert!(
            refresh_error.contains("permanently retired"),
            "{refresh_error}"
        );
        let preflight_error = preflight_project_upsert(tmp.path(), "demo")
            .unwrap_err()
            .to_string();
        assert!(
            preflight_error.contains("permanently retired"),
            "{preflight_error}"
        );

        assert!(remove_project(tmp.path(), "demo").unwrap());
        assert_eq!(
            pick_unused_project_slug(tmp.path(), "demo").unwrap(),
            "demo2"
        );
        let append_error =
            append_project(tmp.path(), sample_entry("demo", Path::new("/x/recreated")))
                .unwrap_err()
                .to_string();
        assert!(
            append_error.contains("permanently retired"),
            "{append_error}"
        );
    }

    #[test]
    fn a_bare_legacy_progress_lock_does_not_reserve_a_slug() {
        let tmp = TempDir::new().unwrap();
        // Every pre-retirement install carries leftover empty `.lock` inodes
        // for slugs that were removed the old way. They own no state and must
        // not block reuse of the base slug.
        let progress = project_progress_path(tmp.path(), "demo");
        std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
        std::fs::write(progress.with_file_name("demo.lock"), b"").unwrap();

        assert_eq!(
            pick_unused_project_slug(tmp.path(), "demo").unwrap(),
            "demo"
        );
        append_project(tmp.path(), sample_entry("demo", Path::new("/x/demo"))).unwrap();
        assert!(lookup_project(tmp.path(), "demo").unwrap().is_some());
    }

    #[test]
    fn orphan_progress_state_reserves_an_unregistered_slug() {
        let tmp = TempDir::new().unwrap();
        let progress = project_progress_path(tmp.path(), "demo");
        ccteam_harness::execution::progress_bridge::append_event(
            &progress,
            &serde_json::json!({"event": "orphan"}),
        )
        .unwrap();

        assert_eq!(
            pick_unused_project_slug(tmp.path(), "demo").unwrap(),
            "demo2"
        );
        let error = append_project(tmp.path(), sample_entry("demo", Path::new("/x/demo")))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("reserved by existing progress state"),
            "{error}"
        );
        assert!(!error.contains("retired"), "{error}");
    }

    #[test]
    fn load_fails_loud_on_garbled_yaml() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(config_path(tmp.path()), "projects: [not a list\n").unwrap();
        let err = load(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("config.yaml"));
    }

    #[test]
    fn daemon_session_and_delegation_defaults_are_documented_values() {
        let cfg = CcteamConfig::default();
        assert_eq!(cfg.daemon.workers, 4);
        assert_eq!(cfg.sessions.max_live, 50);
        assert_eq!(cfg.delegation.max_depth, 2);
        assert_eq!(cfg.delegation.max_children, 10);
        assert_eq!(cfg.delegation.max_delegated, 50);
    }

    #[test]
    fn default_im_quick_templates_include_six_templates() {
        let templates = &CcteamConfig::default().im.quick_templates;
        assert_eq!(templates.len(), 6);
        assert_eq!(templates[0].label, "🎯 Командир");
        assert!(templates[0].prefix.ends_with("Task:"));
        for role in [
            "Opus", "Fable", "Astra", "Sol", "Luna", "Terra", "Sonnet", "GLM",
        ] {
            assert!(templates[0].prefix.contains(role), "missing role {role}");
        }
        assert_eq!(templates[5].label, "🏗 Пирамида");
    }

    #[test]
    fn commander_assigns_bounded_work_and_independent_acceptance() {
        let prefix = commander_quick_template().prefix;
        for contract in [
            "Claude Opus at medium",
            "Fable high plan",
            "zai-coding-plan/glm-5.3-flash",
            "isolated implementation under a checkable contract",
            "Codex Luna medium",
            "Claude Sonnet medium or Codex Terra medium",
            "Codex Sol high or Claude Fable high",
            "fresh Codex Astra",
            "mandatory repository review pair remains mandatory",
            "one writer per isolated git worktree",
            "same immutable revision",
        ] {
            assert!(prefix.contains(contract), "missing {contract}");
        }
    }

    #[test]
    fn commander_paces_subscriptions_without_guessing_or_unsafe_retries() {
        let prefix = commander_quick_template().prefix;
        for contract in [
            "general weekly window, not a general 5-hour window",
            "Claude has both 5-hour and weekly windows", "scope", "observed_at", "resets_at",
            "unknown, never zero use or unlimited capacity", "do not rebase the allowance",
            "must not be equalized across vendors", "one quarter", "stop new work on that pool",
            "never bypass the gate", "vendor_unavailable", "model_unavailable", "effort_unavailable",
            "Authentication/ACL, quota/budget, depth/cycle, timeout, transport and unknown outcomes",
            "Never enable paid overage", "deployment require their own explicit authorization",
        ] {
            assert!(prefix.contains(contract), "missing {contract}");
        }
        assert!(!prefix.contains("38,3"));
    }

    #[test]
    fn im_quick_templates_yaml_override_parses() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            config_path(tmp.path()),
            "im:\n  quick_templates:\n    - label: Custom\n      prefix: 'Do this:'\n",
        )
        .unwrap();

        let cfg = load(tmp.path()).unwrap();
        assert_eq!(
            cfg.im.quick_templates,
            vec![QuickTemplate {
                label: "Custom".to_string(),
                prefix: "Do this:".to_string(),
            }]
        );
    }

    #[test]
    fn daemon_workers_yaml_override_parses() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(config_path(tmp.path()), "daemon:\n  workers: 9\n").unwrap();
        let cfg = load(tmp.path()).unwrap();
        assert_eq!(cfg.daemon.workers, 9);
    }

    #[test]
    fn daemon_workers_one_is_a_valid_rollback_setting() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(config_path(tmp.path()), "daemon:\n  workers: 1\n").unwrap();
        let cfg = load(tmp.path()).unwrap();
        assert_eq!(cfg.daemon.workers, 1);
    }

    #[test]
    fn session_and_delegation_yaml_overrides_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let cfg = CcteamConfig {
            sessions: SessionsConfig { max_live: 7 },
            delegation: DelegationConfig {
                max_depth: 3,
                max_children: 12,
                max_delegated: 60,
            },
            ..Default::default()
        };
        save(tmp.path(), &cfg).unwrap();
        assert_eq!(load(tmp.path()).unwrap(), cfg);

        let yaml = std::fs::read_to_string(config_path(tmp.path())).unwrap();
        assert!(yaml.contains("sessions:\n  max_live: 7"));
        assert!(yaml.contains("delegation:\n  max_depth: 3"));
    }
}
