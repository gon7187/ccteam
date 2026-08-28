//! Bare `ccteam doctor` readiness checkup.
//!
//! A bare invocation reports one consolidated row per vendor plus ccteam and
//! project readiness advisories. It exits 1 only when the required Claude Code
//! binary is missing; warnings are informational because daemon startup can
//! self-heal vendor MCP registration.
//!
//! Bare probes are read-only: doctor never starts or changes the daemon and
//! never writes vendor configuration. The explicit `--repair-progress` path
//! only rewrites corrupt ccteam-owned journals after preserving a backup.
//! `--verify-mcp` remains a separate dev/CI invariant handled by
//! `main::run_doctor` before this module is called.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use ccteam_core::host_registry::AgentProbeSpec;
use ccteam_core::{CcteamPaths, Vendor};
use ccteam_harness::execution::{journal, progress_bridge};

/// Severity of one readiness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Skip => 1,
            Self::Warn => 2,
            Self::Fail => 3,
        }
    }

    fn worst(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// One rendered checklist row.
struct CheckLine {
    status: CheckStatus,
    name: &'static str,
    detail: String,
}

impl CheckLine {
    fn new(status: CheckStatus, name: &'static str, detail: impl Into<String>) -> Self {
        debug_assert!(name.chars().count() <= 9, "doctor row name is too wide");
        Self {
            status,
            name,
            detail: detail.into(),
        }
    }
}

#[derive(Default)]
struct Counts {
    pass: usize,
    warn: usize,
    fail: usize,
    skip: usize,
}

impl Counts {
    fn add(&mut self, status: CheckStatus) {
        match status {
            CheckStatus::Pass => self.pass += 1,
            CheckStatus::Warn => self.warn += 1,
            CheckStatus::Fail => self.fail += 1,
            CheckStatus::Skip => self.skip += 1,
        }
    }

    fn summary(&self) -> String {
        let mut parts = vec![format!("{} успешно", self.pass)];
        if self.warn > 0 {
            parts.push(format!("{} предупреждение", self.warn));
        }
        if self.fail > 0 {
            parts.push(format!("{} ошибка", self.fail));
        }
        if self.skip > 0 {
            parts.push(format!("{} пропущено", self.skip));
        }
        parts.join(", ")
    }
}

struct ReportRow {
    line: CheckLine,
    suppress_when_pass: bool,
}

impl ReportRow {
    fn visible(line: CheckLine) -> Self {
        Self {
            line,
            suppress_when_pass: false,
        }
    }

    fn advisory(line: CheckLine) -> Self {
        Self {
            line,
            suppress_when_pass: true,
        }
    }

    fn is_visible(&self) -> bool {
        !(self.suppress_when_pass && self.line.status == CheckStatus::Pass)
    }
}

/// Fully probed readiness state. Rendering this value is pure: it performs no
/// environment, filesystem, process, or daemon access.
struct ReadinessReport {
    agents: Vec<ReportRow>,
    ccteam: Vec<ReportRow>,
    projects: Vec<ReportRow>,
    progress: Vec<ReportRow>,
    daemon_healthy: bool,
}

/// Run the full readiness checkup and render it. Returns `(report, any_fail)`;
/// the caller exits 1 iff `any_fail`.
pub fn run_readiness_checkup(paths: &CcteamPaths) -> (String, bool) {
    let report = gather_readiness(paths);
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    render_readiness(&report, color)
}

fn gather_readiness(paths: &CcteamPaths) -> ReadinessReport {
    let agents = vec![
        ReportRow::visible(check_agent(
            "claude",
            ccteam_core::CLAUDE_BIN_ENV,
            "claude",
            true,
            check_vendor_auth_claude,
        )),
        ReportRow::visible(check_agent(
            "codex",
            ccteam_core::CODEX_BIN_ENV,
            "codex",
            false,
            check_vendor_auth_codex,
        )),
        ReportRow::visible(check_agent(
            "grok",
            ccteam_core::GROK_BIN_ENV,
            "grok",
            false,
            check_vendor_auth_grok,
        )),
        ReportRow::visible(check_agent(
            "opencode",
            ccteam_core::OPENCODE_BIN_ENV,
            "opencode",
            false,
            check_vendor_auth_opencode,
        )),
        ReportRow::visible(check_agent(
            "kimi",
            ccteam_core::KIMI_BIN_ENV,
            "kimi",
            false,
            check_vendor_auth_kimi,
        )),
        ReportRow::visible(check_agent(
            "pi",
            ccteam_harness::PI_BIN_ENV,
            "pi",
            false,
            check_vendor_auth_pi,
        )),
        ReportRow::visible(check_agent(
            "dsh",
            ccteam_harness::DSH_BIN_ENV,
            // Falls back to a cached `npx` copy when bare `dsh` isn't on
            // PATH — DSH's own quickstart (`npx @deepseek-ai/dsh …`) never
            // puts one there. Same resolver the status/hosts panel and the
            // actual spawn path use, so doctor never disagrees with them.
            &ccteam_harness::resolve_dsh_default_bin(),
            false,
            check_vendor_auth_dsh,
        )),
    ];

    let daemon_probe = ccteam_core::daemon::probe_daemon(paths);
    let daemon_healthy = daemon_probe.ready;
    let daemon_version = daemon_probe.version;
    let daemon = if daemon_probe.ready {
        CheckLine::new(
            CheckStatus::Pass,
            "daemon",
            daemon_version
                .clone()
                .map(|version| format!("запущен (v{version})"))
                .unwrap_or_else(|| "запущен".to_string()),
        )
    } else {
        CheckLine::new(CheckStatus::Warn, "daemon", "не запущен")
    };
    let version = check_version(paths, daemon_healthy.then_some(daemon_version).flatten());
    let host_skew = check_host_skew(paths);
    let mut ccteam = vec![
        ReportRow::visible(daemon),
        ReportRow::advisory(check_legacy_service()),
        ReportRow::visible(version),
    ];
    if host_skew.is_empty() {
        ccteam.push(ReportRow::advisory(CheckLine::new(
            CheckStatus::Pass,
            "hosts",
            "версии fleet согласованы",
        )));
    } else {
        ccteam.extend(host_skew.into_iter().map(ReportRow::visible));
    }
    ccteam.push(ReportRow::visible(check_pricing()));
    ccteam.push(ReportRow::visible(check_home_layout(paths)));

    ReadinessReport {
        agents,
        ccteam,
        projects: vec![ReportRow::advisory(check_project_skill_faces(paths))],
        progress: check_progress_journals(paths),
        daemon_healthy,
    }
}

fn render_readiness(report: &ReadinessReport, color: bool) -> (String, bool) {
    let mut counts = Counts::default();
    let mut out = String::from("ccteam doctor — проверка готовности\n\n");

    render_section(&mut out, "агенты", &report.agents, color, &mut counts);
    render_section(&mut out, "ccteam", &report.ccteam, color, &mut counts);
    render_section(&mut out, "проекты", &report.projects, color, &mut counts);
    render_section(&mut out, "прогресс", &report.progress, color, &mut counts);

    let any_fail = counts.fail > 0;
    out.push_str("сводка: ");
    out.push_str(&counts.summary());
    if any_fail {
        out.push_str(" — НЕ ГОТОВО (исправьте строки FAIL выше)\n");
    } else {
        out.push_str(" — ГОТОВО (WARN носит информационный характер)\n");
    }
    if !report.daemon_healthy {
        out.push_str("\nдемон не запущен — запустите:  ccteam daemon start\n");
    }
    (out, any_fail)
}

fn render_section(
    out: &mut String,
    title: &str,
    rows: &[ReportRow],
    color: bool,
    counts: &mut Counts,
) {
    for row in rows {
        counts.add(row.line.status);
    }
    if !rows.iter().any(ReportRow::is_visible) {
        return;
    }
    out.push_str(title);
    out.push('\n');
    for row in rows.iter().filter(|row| row.is_visible()) {
        let token = status_token(row.line.status, color);
        out.push_str(&format!(
            "  {token} {:<10}{}\n",
            row.line.name, row.line.detail
        ));
    }
    out.push('\n');
}

fn status_token(status: CheckStatus, color: bool) -> String {
    let plain = format!("[{}]", status.label());
    if !color {
        return plain;
    }
    let ansi = match status {
        CheckStatus::Pass => "32",
        CheckStatus::Warn => "33",
        CheckStatus::Fail => "31",
        CheckStatus::Skip => "2",
    };
    format!("\x1b[{ansi}m{plain}\x1b[0m")
}

struct BinaryCheck {
    installed: bool,
    status: CheckStatus,
    detail: String,
}

struct AuthCheck {
    ok: Option<bool>,
    login_hint: &'static str,
    /// Overrides the generic "auth ok" detail text when `ok == Some(true)`.
    /// DSH has two Pass sources (env vs mirrored vendor credentials, K23/D13)
    /// that must read differently, not just "auth ok" either way.
    ok_detail: Option<&'static str>,
}

impl AuthCheck {
    const fn simple(ok: Option<bool>, login_hint: &'static str) -> Self {
        Self {
            ok,
            login_hint,
            ok_detail: None,
        }
    }
}

struct McpCheck {
    status: CheckStatus,
    detail: String,
}

fn check_agent(
    name: &'static str,
    env_var: &str,
    default_bin: &str,
    required: bool,
    auth_check: fn() -> AuthCheck,
) -> CheckLine {
    let binary = probe_binary(env_var, default_bin, required);
    if !binary.installed {
        return CheckLine::new(binary.status, name, binary.detail);
    }

    let auth = auth_check();
    let mcp = check_vendor_mcp(name);
    let mut status = binary.status.worst(mcp.status);
    let mut details = vec![binary.detail];
    match auth.ok {
        Some(true) => details.push(auth.ok_detail.unwrap_or("авторизация пройдена").to_string()),
        Some(false) => {
            status = status.worst(CheckStatus::Warn);
            details.push(format!("нет авторизации — {}", auth.login_hint));
        }
        None => {}
    }
    details.push(mcp.detail);
    CheckLine::new(status, name, details.join(" · "))
}

/// Shared `<bin> --version` probe. Doctor executes only this read-only version
/// command; daemon-start's registration gate uses a separate no-exec resolver.
fn probe_binary(env_var: &str, default_bin: &str, required: bool) -> BinaryCheck {
    let bin = std::env::var(env_var).unwrap_or_else(|_| default_bin.to_string());
    match Command::new(&bin).arg("--version").output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let detail = if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else {
                format!("найден `{bin}`")
            };
            BinaryCheck {
                installed: true,
                status: CheckStatus::Pass,
                detail,
            }
        }
        Ok(out) => {
            let reason = out
                .status
                .code()
                .map(|code| format!("код выхода {code}"))
                .unwrap_or_else(|| "завершён сигналом".to_string());
            BinaryCheck {
                installed: false,
                status: if required {
                    CheckStatus::Fail
                } else {
                    CheckStatus::Warn
                },
                detail: format!(
                    "`{bin} --version` завершилась ошибкой ({reason}) — переустановите или укажите в {env_var} рабочий бинарник"
                ),
            }
        }
        Err(_) => {
            let detail = if required {
                format!("не установлен — установите Claude Code CLI или укажите его в {env_var}")
            } else {
                format!("не установлен (необязательно) — установите его или укажите в {env_var}")
            };
            BinaryCheck {
                installed: false,
                status: if required {
                    CheckStatus::Fail
                } else {
                    CheckStatus::Warn
                },
                detail,
            }
        }
    }
}

fn check_vendor_auth_claude() -> AuthCheck {
    let home = std::env::var("CLAUDE_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    let ok = home.as_ref().is_some_and(|h| {
        h.join(".credentials.json").exists() || h.join("credentials.json").exists()
    }) || std::env::var("ANTHROPIC_API_KEY").is_ok();
    AuthCheck::simple(
        Some(ok),
        "выполните `claude auth login` или задайте ANTHROPIC_API_KEY",
    )
}

// Daemon-start MCP auto-registration creates the vendor config files, so those
// files alone must never impersonate a successful vendor login.
fn check_vendor_auth_codex() -> AuthCheck {
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")));
    let ok = home.as_ref().is_some_and(|h| h.join("auth.json").exists())
        || std::env::var("OPENAI_API_KEY").is_ok();
    AuthCheck::simple(
        Some(ok),
        "выполните `codex login` или задайте OPENAI_API_KEY",
    )
}

fn check_vendor_auth_grok() -> AuthCheck {
    let ok = std::env::var("XAI_API_KEY").is_ok()
        || dirs::home_dir().is_some_and(|home| {
            home.join(".config/grok").exists()
                || dir_contains_entry_other_than(&home.join(".grok"), "config.toml")
        });
    AuthCheck::simple(ok.then_some(true), "")
}

fn check_vendor_auth_opencode() -> AuthCheck {
    let ok = std::env::var("OPENAI_API_KEY").is_ok()
        || dirs::home_dir().is_some_and(|home| {
            home.join(".local/share/opencode").exists()
                || home.join(".opencode").exists()
                || dir_contains_entry_other_than(&home.join(".config/opencode"), "opencode.json")
        });
    AuthCheck::simple(ok.then_some(true), "")
}

fn dir_contains_entry_other_than(dir: &std::path::Path, excluded: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name() != std::ffi::OsStr::new(excluded))
    })
}

fn check_vendor_auth_kimi() -> AuthCheck {
    let kimi_home = std::env::var_os("KIMI_CODE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".kimi-code")));
    let ok = std::env::var("MOONSHOT_API_KEY").is_ok()
        || kimi_home
            .as_ref()
            .is_some_and(|h| h.join("credentials").exists() || h.join("oauth").exists());
    AuthCheck::simple(
        Some(ok),
        "выполните `kimi login` или задайте MOONSHOT_API_KEY",
    )
}

fn check_vendor_auth_pi() -> AuthCheck {
    // Pi may use any configured provider. The RPC feature handshake is the
    // authoritative model/auth verdict when a managed session starts.
    AuthCheck::simple(None, "")
}

/// Two Pass sources, checked in order: explicit env wins (matches DSH's
/// own resolution order), else a mirrorable `~/.dsh/.credentials.yaml`
/// counts (the real spawn-time credential mirror — `check_vendor_mcp`'s
/// bridge short-circuit skips the generic vendor-config probe for a
/// `ManagedSessionBridge` vendor, so this is the only auth signal `dsh`'s
/// doctor row gets). Neither present → Fail with both fixes named (matches
/// the two-hint convention above).
fn check_vendor_auth_dsh() -> AuthCheck {
    if std::env::var("DEEPSEEK_API_KEY").is_ok() {
        return AuthCheck {
            ok: Some(true),
            login_hint: "",
            ok_detail: Some("авторизация пройдена (источник: env)"),
        };
    }
    let mirrored =
        dirs::home_dir().is_some_and(|home| home.join(".dsh").join(".credentials.yaml").exists());
    if mirrored {
        return AuthCheck {
            ok: Some(true),
            login_hint: "",
            ok_detail: Some(
                "авторизация пройдена (источник: учётные данные dsh, копируются при запуске)",
            ),
        };
    }
    AuthCheck::simple(
        Some(false),
        "задайте DEEPSEEK_API_KEY или один раз выполните `dsh web`, чтобы создать ~/.dsh/.credentials.yaml",
    )
}

fn check_vendor_mcp(vendor: &str) -> McpCheck {
    if AgentProbeSpec::by_vendor(vendor).is_some_and(|spec| {
        spec.tool_surface == ccteam_core::host_registry::ToolSurfaceMode::ManagedSessionBridge
    }) {
        return McpCheck {
            status: CheckStatus::Pass,
            detail: AgentProbeSpec::by_vendor(vendor)
                .and_then(AgentProbeSpec::tool_surface_notice)
                .unwrap_or_else(|| {
                    "инструменты поступают через управляемый мост сессии".to_string()
                }),
        };
    }
    let resolved = match vendor {
        "claude" => ccteam_core::projects::resolve_claude_json_path().map(|path| {
            let registered = ccteam_core::mcp_register::claude_mcp_registered(&path);
            (path, registered)
        }),
        "codex" => ccteam_core::mcp_register::resolve_codex_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::codex_mcp_registered(&path);
            (path, registered)
        }),
        "grok" => ccteam_core::mcp_register::resolve_grok_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::grok_mcp_registered(&path);
            (path, registered)
        }),
        "opencode" => ccteam_core::mcp_register::resolve_opencode_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::opencode_mcp_registered(&path);
            (path, registered)
        }),
        "kimi" => ccteam_core::mcp_register::resolve_kimi_config_path().map(|path| {
            let registered = ccteam_core::mcp_register::kimi_mcp_registered(&path);
            (path, registered)
        }),
        _ => unreachable!("doctor vendor list is closed"),
    };

    match resolved {
        Ok((_, true)) => McpCheck {
            status: CheckStatus::Pass,
            detail: "MCP зарегистрирован".to_string(),
        },
        Ok((_, false)) => McpCheck {
            status: CheckStatus::Warn,
            detail: "MCP не зарегистрирован — регистрируется автоматически через `ccteam daemon start` (или `ccteam config mcp`)".to_string(),
        },
        Err(err) => McpCheck {
            status: CheckStatus::Warn,
            detail: format!("состояние MCP неизвестно ({err})"),
        },
    }
}

/// Residual legacy systemd/launchd unit detection. The clean result is hidden;
/// detected units keep the existing migration text.
fn check_legacy_service() -> CheckLine {
    let paths = crate::legacy_takeover::LegacyServicePaths::from_env();
    match crate::legacy_takeover::detect_legacy_unit(&paths) {
        None => CheckLine::new(
            CheckStatus::Pass,
            "legacy",
            "устаревший unit службы отсутствует",
        ),
        Some((path, true)) => CheckLine::new(
            CheckStatus::Warn,
            "legacy",
            format!(
                "устаревший unit ccteam от установщика в {} — управление systemd/launchd больше не используется; перейдите на `ccteam daemon start` (автоперехват: останавливает и удаляет unit, затем запускает в фоне)",
                path.display()
            ),
        ),
        Some((path, false)) => CheckLine::new(
            CheckStatus::Warn,
            "legacy",
            format!(
                "unit службы в {} создан не установщиком ccteam — ccteam не будет им управлять или удалять его; его экземпляр считается \"неуправляемым\" для `ccteam daemon stop`. Удалите его вручную для самостоятельного управления ccteam",
                path.display()
            ),
        ),
    }
}

fn check_version(paths: &CcteamPaths, daemon_version: Option<String>) -> CheckLine {
    let channel = ccteam_core::install_channel::detect(paths);
    let binary_version = env!("CARGO_PKG_VERSION");
    let cache = ccteam_core::version_check::cached_latest(paths).unwrap_or_default();
    let update_available = ccteam_core::version_check::update_available(&cache, binary_version);
    let mut status = CheckStatus::Pass;
    let latest = match update_available {
        Some(latest) => {
            status = CheckStatus::Warn;
            format!("доступно обновление → {latest}: выполните `ccteam update`")
        }
        None if cache.latest_version.is_some() => "актуально".to_string(),
        None => "последняя версия ещё не проверялась".to_string(),
    };
    let mut detail = format!("{binary_version} ({}) · {latest}", channel.as_str());
    if let Some(version) = daemon_version.filter(|version| version != binary_version) {
        status = CheckStatus::Warn;
        detail.push_str(&format!(
            " · демон использует {version} — перезапустите: `ccteam daemon restart`"
        ));
    }
    CheckLine::new(status, "version", detail)
}

fn check_host_skew(paths: &CcteamPaths) -> Vec<CheckLine> {
    crate::update::fleet_version_skew(paths, env!("CARGO_PKG_VERSION"))
        .into_iter()
        .map(|detail| CheckLine::new(CheckStatus::Warn, "hosts", detail))
        .collect()
}

fn check_pricing() -> CheckLine {
    const WARN_DAYS: i64 = 180;
    let today = chrono::Utc::now().date_naive();
    let mut worst: Option<i64> = None;
    for &vendor in Vendor::ALL {
        let raw = ccteam_core::pricing_schema_version_for(vendor);
        if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
            let age = (today - date).num_days();
            if worst.is_none_or(|current| age > current) {
                worst = Some(age);
            }
        }
    }
    match worst {
        Some(age) if age > WARN_DAYS => CheckLine::new(
            CheckStatus::Warn,
            "pricing",
            format!("таблице тарифов {age} дн. (>{WARN_DAYS} дн.) — обновите ccteam для актуальных тарифов"),
        ),
        Some(age) => CheckLine::new(
            CheckStatus::Pass,
            "pricing",
            format!("таблице тарифов {age} дн. (актуальна)"),
        ),
        None => CheckLine::new(
            CheckStatus::Skip,
            "pricing",
            "не удалось разобрать встроенный schema_version",
        ),
    }
}

fn check_home_layout(paths: &CcteamPaths) -> CheckLine {
    if !paths.root.exists() {
        return CheckLine::new(
            CheckStatus::Skip,
            "home",
            format!(
                "{} пока не существует (свежая установка)",
                paths.root.display()
            ),
        );
    }
    let Ok(entries) = std::fs::read_dir(&paths.root) else {
        return CheckLine::new(
            CheckStatus::Skip,
            "home",
            format!("не удалось прочитать {}", paths.root.display()),
        );
    };
    let mut unexpected = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !ccteam_core::canonical_home_dirs().contains(&name) {
            unexpected.push(name.to_string());
        }
    }
    if unexpected.is_empty() {
        CheckLine::new(
            CheckStatus::Pass,
            "home",
            format!(
                "{} соответствует канонической структуре",
                paths.root.display()
            ),
        )
    } else {
        unexpected.sort();
        CheckLine::new(
            CheckStatus::Warn,
            "home",
            format!(
                "{} непредвиденных каталогов в {} (остатки эпохи orchestrator, можно `rm -rf`): {}",
                unexpected.len(),
                paths.root.display(),
                unexpected.join(", ")
            ),
        )
    }
}

/// Advisory-only scan for registered projects that still use a real legacy
/// `.claude/skills` directory. A clean result is counted but hidden.
fn check_project_skill_faces(paths: &CcteamPaths) -> CheckLine {
    let config = match ccteam_core::load_ccteam_config(&paths.root) {
        Ok(config) => config,
        Err(err) => {
            return CheckLine::new(
                CheckStatus::Skip,
                "skills",
                format!("не удалось прочитать зарегистрированные проекты: {err}"),
            );
        }
    };
    let mut legacy = Vec::new();
    for project in config.projects {
        let face = project.path.join(".claude/skills");
        if matches!(
            std::fs::symlink_metadata(&face),
            Ok(metadata) if metadata.file_type().is_dir()
        ) {
            legacy.push(project.slug);
        }
    }
    if legacy.is_empty() {
        CheckLine::new(
            CheckStatus::Pass,
            "skills",
            "устаревших каталогов skills проекта нет",
        )
    } else {
        legacy.sort();
        CheckLine::new(
            CheckStatus::Warn,
            "skills",
            format!(
                "устаревший каталог .claude/skills в: {} — перенесите: `ccteam skill migrate-project --project <slug>`",
                legacy.join(", ")
            ),
        )
    }
}

#[derive(Default)]
struct ProgressFileScan {
    size: u64,
    corrupt_count: usize,
    first_corrupt_offset: Option<u64>,
    kind_bytes: BTreeMap<String, u64>,
}

fn check_progress_journals(paths: &CcteamPaths) -> Vec<ReportRow> {
    let slugs = progress_slugs(paths);
    if slugs.is_empty() {
        return vec![ReportRow::advisory(CheckLine::new(
            CheckStatus::Pass,
            "progress",
            "журналов progress пока нет",
        ))];
    }
    slugs
        .into_iter()
        .map(|slug| ReportRow::visible(check_progress_journal(paths, &slug)))
        .collect()
}

fn check_progress_journal(paths: &CcteamPaths, slug: &str) -> CheckLine {
    let active = paths.progress_jsonl(slug);
    let archive = progress_bridge::progress_archive_path(&active);
    let mut status = CheckStatus::Pass;
    let mut details = Vec::new();
    let threshold = progress_bridge::progress_rotate_bytes();
    let warning_size = threshold.saturating_mul(4) / 5;

    let active_scan = match scan_progress_file(&active) {
        Ok(scan) => scan,
        Err(error) => {
            status = CheckStatus::Warn;
            details.push(format!("ошибка сканирования active: {error}"));
            ProgressFileScan::default()
        }
    };
    if active_scan.size > warning_size {
        status = CheckStatus::Warn;
        details.push(format!(
            "active={}B ПРЕДУПРЕЖДЕНИЕ О РАЗМЕРЕ (>{}B, 80% порога ротации {}B)",
            active_scan.size, warning_size, threshold
        ));
    } else {
        details.push(format!(
            "active={}B (предупреждение >{}B; ротация >{}B)",
            active_scan.size, warning_size, threshold
        ));
    }
    if active_scan.corrupt_count > 0 {
        status = CheckStatus::Warn;
    }
    details.push(format!(
        "active повреждено={} first_offset={}",
        active_scan.corrupt_count,
        format_optional_offset(active_scan.first_corrupt_offset)
    ));

    let archive_scan = match scan_progress_file(&archive) {
        Ok(scan) => scan,
        Err(error) => {
            status = CheckStatus::Warn;
            details.push(format!("ошибка сканирования archive: {error}"));
            ProgressFileScan::default()
        }
    };
    if archive_scan.corrupt_count > 0 {
        status = CheckStatus::Warn;
    }
    if archive_scan.size > 0 || archive.exists() {
        details.push(format!(
            "archive={}B повреждено={} first_offset={}",
            archive_scan.size,
            archive_scan.corrupt_count,
            format_optional_offset(archive_scan.first_corrupt_offset)
        ));
    }

    let mut histogram = active_scan.kind_bytes;
    for (kind, bytes) in archive_scan.kind_bytes {
        *histogram.entry(kind).or_insert(0) += bytes;
    }
    details.push(format!(
        "ТОП ТИПОВ ПО БАЙТАМ: {}",
        render_top_kinds(histogram)
    ));

    let archive_coverage = match progress_bridge::progress_archive_coverage(&active) {
        Ok(coverage) => coverage,
        Err(error) => {
            status = CheckStatus::Warn;
            details.push(format!("ошибка маркера archive: {error}"));
            None
        }
    };
    match progress_bridge::read_progress_checkpoint(&active) {
        Ok(Some(checkpoint)) => {
            if progress_bridge::checkpoint_covers_archive(&checkpoint, archive_coverage.as_ref()) {
                details.push(format!(
                    "checkpoint=согласован seq={} events={}",
                    checkpoint.rotation_sequence, checkpoint.event_count
                ));
                details.push(if archive_coverage.is_some() {
                    "статус archive=покрыт".to_string()
                } else {
                    "статус archive=отсутствует".to_string()
                });
            } else {
                status = CheckStatus::Warn;
                details.push(format!(
                    "checkpoint=НЕСОГЛАСОВАН seq={} (маркер покрытия не соответствует .1)",
                    checkpoint.rotation_sequence
                ));
                details.push(if archive_coverage.is_some() {
                    "статус archive=СИРОТА/не покрыт".to_string()
                } else {
                    "статус archive=отсутствует, но checkpoint всё ещё помечает покрытие"
                        .to_string()
                });
            }
        }
        Ok(None) => {
            details.push("checkpoint=отсутствует".to_string());
            if archive_coverage.is_some() {
                status = CheckStatus::Warn;
                details.push("статус archive=СИРОТА/не покрыт".to_string());
            } else {
                details.push("статус archive=отсутствует".to_string());
            }
        }
        Err(error) => {
            status = CheckStatus::Warn;
            details.push(format!("checkpoint=ОШИБКА РАЗБОРА ({error})"));
            details.push(if archive_coverage.is_some() {
                "статус archive=СИРОТА/покрытие неизвестно".to_string()
            } else {
                "статус archive=отсутствует".to_string()
            });
        }
    }

    CheckLine::new(
        status,
        "progress",
        format!("{slug}: {}", details.join(" · ")),
    )
}

fn scan_progress_file(path: &Path) -> Result<ProgressFileScan> {
    let size = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(error).with_context(|| format!("получить статус {}", path.display()))
        }
    };
    let mut kind_bytes = BTreeMap::new();
    let summary = journal::scan_stream_detailed(path, |event, bytes| {
        let kind = event
            .get("event")
            .and_then(serde_json::Value::as_str)
            .or_else(|| event.get("kind").and_then(serde_json::Value::as_str))
            .unwrap_or("<unknown>");
        *kind_bytes.entry(kind.to_string()).or_insert(0_u64) += bytes;
    })?;
    Ok(ProgressFileScan {
        size,
        corrupt_count: summary.corrupt_count,
        first_corrupt_offset: summary.first_corrupt_offset,
        kind_bytes,
    })
}

fn render_top_kinds(histogram: BTreeMap<String, u64>) -> String {
    let mut values = histogram.into_iter().collect::<Vec<_>>();
    values.sort_unstable_by(|(left_kind, left_bytes), (right_kind, right_bytes)| {
        right_bytes
            .cmp(left_bytes)
            .then_with(|| left_kind.cmp(right_kind))
    });
    if values.is_empty() {
        return "нет".to_string();
    }
    values
        .into_iter()
        .take(5)
        .map(|(kind, bytes)| format!("{kind}={bytes}B"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_optional_offset(offset: Option<u64>) -> String {
    offset.map_or_else(|| "нет".to_string(), |offset| offset.to_string())
}

fn progress_slugs(paths: &CcteamPaths) -> BTreeSet<String> {
    let mut slugs = ccteam_core::load_ccteam_config(&paths.root)
        .map(|config| {
            config
                .projects
                .into_iter()
                .map(|project| project.slug)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let Ok(entries) = std::fs::read_dir(paths.progress_dir()) else {
        return slugs;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let slug = name
            .strip_suffix(".checkpoint.json")
            .or_else(|| name.strip_suffix(".1.jsonl"))
            .or_else(|| name.strip_suffix(".jsonl"));
        if let Some(slug) = slug.filter(|slug| !slug.is_empty()) {
            slugs.insert(slug.to_string());
        }
    }
    slugs
}

/// Repair every corrupt active/archive progress journal under this home.
///
/// The sweep is per-slug best effort: a retired generation is skipped (its
/// tombstone legitimately refuses writes while the config row is still being
/// removed) and any single slug's failure is reported without aborting the
/// remaining slugs.
pub fn repair_progress(paths: &CcteamPaths) -> Result<String> {
    let mut out = String::from("исправление progress\n");
    let mut repaired = 0_u64;
    let mut failed = 0_u64;
    for slug in progress_slugs(paths) {
        let active = paths.progress_jsonl(&slug);
        match progress_bridge::progress_state_is_retired_shared(&active) {
            Ok(true) => {
                out.push_str(&format!(
                    "  {slug}: пропущено — поколение снято с эксплуатации (retired)\n"
                ));
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                failed = failed.saturating_add(1);
                out.push_str(&format!(
                    "  {slug}: ОШИБКА — не удалось прочитать маркер поколения: {error:#}\n"
                ));
                continue;
            }
        }
        let targets = [
            active.clone(),
            progress_bridge::progress_archive_path(&active),
        ];
        for target in targets {
            let name = target
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match progress_bridge::repair_progress_journal(&active, &target) {
                Ok(Some(report)) => {
                    repaired = repaired.saturating_add(1);
                    out.push_str(&format!(
                        "  {slug} {name}: сохранено {}, отброшено {}, резервная копия {}\n",
                        report.kept_count,
                        report.dropped_count,
                        report.backup_path.display()
                    ));
                }
                Ok(None) => {}
                Err(error) => {
                    failed = failed.saturating_add(1);
                    out.push_str(&format!("  {slug} {name}: ОШИБКА — {error:#}\n"));
                }
            }
        }
    }
    if failed > 0 {
        out.push_str(&format!(
            "  слагов с ошибками: {failed}; остальные обработаны\n"
        ));
    }
    if repaired == 0 {
        out.push_str("  повреждённых строк progress не найдено; журналы не менялись\n");
    } else {
        out.push_str(
            "  примечание: оборванная строка обычно теряет 2 записи (обрезанную и следующую, склеенную с ней)\n",
        );
    }
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with_daemon(daemon_healthy: bool) -> ReadinessReport {
        ReadinessReport {
            agents: vec![ReportRow::visible(CheckLine::new(
                CheckStatus::Pass,
                "claude",
                "test",
            ))],
            ccteam: vec![ReportRow::visible(CheckLine::new(
                if daemon_healthy {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Warn
                },
                "daemon",
                if daemon_healthy {
                    "запущен"
                } else {
                    "не запущен"
                },
            ))],
            projects: Vec::new(),
            progress: Vec::new(),
            daemon_healthy,
        }
    }

    #[test]
    fn renderer_healthy_daemon_ends_on_summary_without_start_hint() {
        let (output, any_fail) = render_readiness(&report_with_daemon(true), false);
        assert!(!any_fail);
        assert!(!output.contains("daemon not running — start it"));
        assert!(output
            .lines()
            .rfind(|line| !line.is_empty())
            .is_some_and(|line| line.starts_with("сводка:")));
    }

    #[test]
    fn renderer_uses_russian_human_headings() {
        let (output, _) = render_readiness(&report_with_daemon(true), false);
        assert!(output.starts_with("ccteam doctor — проверка готовности"));
        assert!(output.contains("сводка:"));
        assert!(output.contains("запущен"));
    }

    #[test]
    fn renderer_unhealthy_daemon_ends_on_start_hint() {
        let (output, any_fail) = render_readiness(&report_with_daemon(false), false);
        assert!(!any_fail);
        assert_eq!(
            output.lines().rfind(|line| !line.is_empty()),
            Some("демон не запущен — запустите:  ccteam daemon start")
        );
    }

    #[test]
    fn renderer_counts_a_suppressed_clean_advisory() {
        let report = ReadinessReport {
            agents: Vec::new(),
            ccteam: vec![ReportRow::advisory(CheckLine::new(
                CheckStatus::Pass,
                "legacy",
                "clean but hidden",
            ))],
            projects: Vec::new(),
            progress: Vec::new(),
            daemon_healthy: true,
        };
        let (output, any_fail) = render_readiness(&report, false);
        assert!(!any_fail);
        assert!(output.contains("сводка: 1 успешно — ГОТОВО"));
        assert!(!output.contains("clean but hidden"));
    }
}
