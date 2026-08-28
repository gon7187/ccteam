//! `ccteam` binary entry point.

mod clipboard;
mod commands;
// v0.9.7 — `ccteam daemon <start|stop|restart|status|logs>` handlers
// (pid-detach lifecycle over `ccteam_core::daemon`) + the one-time
// legacy systemd/launchd takeover (PRD F4; single Rust implementation).
mod daemon_cli;
mod legacy_takeover;
mod mcp_serve;
// v0.9.7 — `ccteam update`: channel-aware self-update (standalone replays
// install.sh + upgrade-restart contract; source/npm print guidance).
mod update;
// ToolGroup enum + CCTEAM_DISABLE_TOOLS filter + chat send_file schema.
// v0.8.7 W1 — `session_*` tools (agent scheduling). Stdio side
// forwards to the daemon over mcp.sock; the daemon-side handler
// (`ccteam_im::mcp::McpDispatch`) holds the gateway + enforces the cto gate.
mod mcp_tool_groups;
mod web_chat_bridge;
// Bare `ccteam doctor` readiness checkup: one consolidated row per vendor,
// grouped ccteam/project advisories, a summary, and a final daemon-start hint
// when down. Only a missing Claude binary FAILs. `main::run_doctor` runs it for
// every invocation except `--verify-mcp`; historical one-shot repair flags were
// removed (pre-v1.0 = no back-compat shims).
mod doctor;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use ccteam_core::CcteamPaths;
use commands::{InitOptions, OutputFormat};

/// Version string shown by `ccteam --version`: the crate version plus the
/// git commit it was built from (e.g. `0.8.4 (<commit>)`), so a running
/// binary's exact build is identifiable. `CCTEAM_GIT_COMMIT` is set by
/// `build.rs` (falls back to "unknown" when git isn't available at build).
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CCTEAM_GIT_COMMIT"),
    ")"
);

#[derive(Parser)]
#[command(
    name = "ccteam",
    version = VERSION,
    about = "Командный мост для агентных команд и уровень управления"
)]
struct Cli {
    /// Переопределить домашний каталог ccteam (по умолчанию `~/.ccteam`); приоритет над `CCTEAM_HOME`.
    #[arg(long, value_name = "DIR", global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Зарегистрировать каталог как проект ccteam и создать состояние `.ccteam/`. Безопасно запускать повторно.
    Init {
        /// Установить в этот каталог вместо cwd (создаётся при отсутствии).
        #[arg(long, value_name = "PATH")]
        r#in: Option<PathBuf>,
        /// Имя зарегистрированного проекта (по умолчанию: имя каталога установки); установку не перемещает.
        #[arg(long, value_name = "NAME")]
        slug: Option<String>,
        /// Перезаписать файлы под управлением ccteam (state.json, workflow.yaml); `.claude/agents/` не затрагивается.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Идентификатор владельца проекта (например, `user:alice`; простое имя становится `user:<name>`).
        #[arg(long, value_name = "OWNER")]
        owner: Option<String>,
    },
    /// Запустить gateway-демон в foreground: IM-шлюз и встроенный web UI.
    Start {
        /// Запустить без встроенного web UI (только IM-шлюз).
        #[arg(long, default_value_t = false)]
        no_web: bool,
        /// Запустить без IM-шлюза (моста Telegram / Slack / Discord); только web.
        #[arg(long, default_value_t = false)]
        no_imd: bool,
        /// Адрес привязки web UI; для не-loopback адресов токенная авторизация включается автоматически.
        #[arg(long, value_name = "ADDR", default_value = "0.0.0.0:7331")]
        web_bind: String,
        /// Адрес привязки прокси DSH web companion (по умолчанию: порт web-bind + 1; `off` отключает).
        #[arg(long, value_name = "ADDR|off")]
        dsh_web_bind: Option<String>,
        /// Отключить токенную авторизацию web. ОПАСНО на не-loopback адресах.
        #[arg(long, default_value_t = false)]
        web_no_auth: bool,
        /// Прочитать токен авторизации из файла (по умолчанию `~/.ccteam/web-token`).
        #[arg(long, value_name = "PATH")]
        web_token_file: Option<PathBuf>,
        /// Не копировать web-токен в буфер обмена (для CI / headless-запусков).
        #[arg(long, default_value_t = false)]
        no_clipboard: bool,
    },
    /// Краткий статус: демон, проекты, сессии, web URL и токен.
    Status,
    /// Внутренние обработчики хуков и низкоуровневые утилиты (не для ежедневного использования).
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        cmd: InternalCommand,
    },
    /// Остановить управляемый gateway-демон (псевдоним `daemon stop`); сессии агентов продолжают работать.
    Stop,
    /// Управлять фоновым демоном: start, stop, restart, status, logs.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCommand,
    },
    /// Обновить ccteam до последнего релиза и перезапустить демон с новым бинарником.
    Update {
        /// Пропустить ожидание незавершённых задач и немедленно перезапустить демон.
        #[arg(long, default_value_t = false)]
        now: bool,
        /// Заменить только бинарник; работающий демон не перезапускать.
        #[arg(long, default_value_t = false)]
        no_restart: bool,
        /// Вывести одну машиночитаемую JSON-строку в stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Переустановить даже на последнем релизе (восстановить повреждённую установку).
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Управлять зарегистрированными проектами: ls, show, new, stop, rm.
    Project {
        #[command(subcommand)]
        cmd: ProjectCommand,
    },
    /// Вывести список или подключиться к активным сессиям: ls, attach.
    Session {
        #[command(subcommand)]
        cmd: SessionCommand,
    },
    /// Искать и устанавливать плагины агентов из каталога ccteam-hub.
    Role {
        #[command(subcommand)]
        cmd: RoleCommand,
    },
    /// Управлять пользовательской библиотекой skills и ссылками skills в проектах.
    Skill {
        #[command(subcommand)]
        cmd: SkillCommand,
    },
    /// Операции с несколькими хостами: подключить satellite, выпустить join-токен, удалить, вывести список.
    Host {
        #[command(subcommand)]
        cmd: HostCommand,
    },
    /// Проверить готовность: по строке на вендора агентов и рекомендации ccteam/проектов.
    Doctor {
        /// Проверка CI: убедиться, что поверхность MCP-инструментов полностью подключена (код 1 при любом STUB).
        #[arg(long, default_value_t = false)]
        verify_mcp: bool,
        /// Исправить повреждённые строки журналов progress (сначала создаётся резервная копия каждого файла).
        #[arg(long, default_value_t = false, conflicts_with = "verify_mcp")]
        repair_progress: bool,
        /// Вывести машиночитаемый JSON (имеет смысл только с `--verify-mcp`).
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Настроить ccteam (регистрация MCP, IM-токен), показать или задать предпочтения.
    Config {
        /// Пусто = интерактивное меню; `show`; `get <key>`; `<key> <value>` для установки.
        #[arg(value_name = "ARGS", num_args = 0..=2)]
        args: Vec<String>,
    },
}

/// Управление жизненным циклом `ccteam daemon`: фоновый запуск, владение pid и
/// проверка версии через сокет; единый механизм для Linux / macOS / WSL.
#[derive(Subcommand)]
enum DaemonCommand {
    /// Запустить демон в фоне и дождаться готовности (идемпотентно).
    Start {
        /// Адрес привязки встроенного web UI, передаётся фоновому
        /// `ccteam start`.
        #[arg(long, value_name = "ADDR", default_value = "0.0.0.0:7331")]
        web_bind: String,
        /// Адрес привязки DSH web companion (по умолчанию порт web-bind + 1; `off` отключает).
        #[arg(long, value_name = "ADDR|off")]
        dsh_web_bind: Option<String>,
        /// Вывести одну машиночитаемую JSON-строку в stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Остановить управляемый демон (SIGTERM + ожидание); неуправляемый экземпляр будет отклонён.
    Stop {
        /// После ожидания перейти к SIGKILL (только демон; сессии агентов не затрагиваются).
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Вывести ровно одну машиночитаемую JSON-строку в stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Перезапустить управляемый демон (остановка и запуск под одной блокировкой операции).
    Restart {
        /// Адрес привязки встроенного web UI, передаётся фоновому
        /// `ccteam start`.
        #[arg(long, value_name = "ADDR", default_value = "0.0.0.0:7331")]
        web_bind: String,
        /// Адрес привязки DSH web companion (по умолчанию порт web-bind + 1; `off` отключает).
        #[arg(long, value_name = "ADDR|off")]
        dsh_web_bind: Option<String>,
        /// Вывести ровно одну машиночитаемую JSON-строку в stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Если запущенный демон не управляется ccteam, предупредить и успешно завершиться без перезапуска.
        #[arg(long, default_value_t = false)]
        if_managed: bool,
    },
    /// Показать готовность, статус управления и версии запущенного демона и бинарника.
    Status {
        /// Вывести ровно одну машиночитаемую JSON-строку в stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Вывести конец `~/.ccteam/daemon.log`.
    Logs {
        /// Число выводимых строк с конца.
        #[arg(short = 'n', long = "lines", default_value_t = 50)]
        n: usize,
        /// Продолжать вывод добавляемых строк (как `tail -f`).
        #[arg(short = 'f', long = "follow", default_value_t = false)]
        follow: bool,
        /// Вывести JSON-строку (`{path, lines}`) вместо исходных строк.
        /// Несовместимо с `--follow`.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

/// Команды `ccteam session`: перечисление только для чтения (`ls`) и подключение tmux (`attach`).
#[derive(Subcommand)]
enum SessionCommand {
    /// Вывести список активных чат-сессий gateway.
    Ls,
    /// Подключиться к tmux-панели активной сессии (у stream-json сессий панели нет).
    Attach {
        slug: String,
        /// ID чат-сессии; не указывайте для автоматического выбора единственной активной сессии slug.
        sid: Option<String>,
    },
}

/// Утилиты backend Mux (`hook-emit`).
#[derive(Subcommand)]
enum MuxCommand {
    /// Передать срабатывание хука Claude Code демону через `~/.ccteam/run/hook.sock`.
    HookEmit {
        /// Тип диспетчеризации, например `chat-progress`.
        #[arg(long, value_name = "KIND")]
        kind: String,
        /// Действие диспетчеризации (аргумент события), например `session-start`.
        #[arg(long, value_name = "ACTION")]
        action: Option<String>,
        /// Явный ID сессии (по умолчанию из `CCTEAM_CHAT_SLUG` / `CCTEAM_CHAT_ROLE`).
        #[arg(long, value_name = "SID")]
        session: Option<String>,
        /// JSON-полезная нагрузка хука inline; если отсутствует, читать stdin (`-` тоже работает).
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
    },
}

/// Команды жизненного цикла `ccteam project`: `ls` / `show` просматривают, `new` создаёт,
/// `stop` останавливает активные сессии, `rm` снимает регистрацию (`--purge` удаляет следы).
#[derive(Subcommand)]
enum ProjectCommand {
    /// Вывести список всех известных проектов.
    Ls {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Показать состояние проекта, последние события и артефакты.
    Show {
        slug: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Создать новый проект в `<projects_root>/<slug>/`.
    New {
        /// Slug проекта. Станет именем каталога в `projects_root`.
        slug: String,
    },
    /// Снять проект с регистрации; `--purge` также удаляет файлы ccteam.
    Rm {
        /// Slug проекта (как в `ccteam project ls`).
        slug: String,
        /// Также удалить следы ccteam в проекте (`.ccteam/`, секцию настроек хуков).
        #[arg(long, default_value_t = false)]
        purge: bool,
        /// Вывести все планируемые изменения без их применения.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Пропустить проверки отказа при активной работе (tmux-панели, фоновые задачи, открытые запуски).
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Остановить активные сессии проекта без его удаления (с продолжением позже).
    Stop {
        /// Slug останавливаемого проекта.
        slug: String,
    },
}

/// Команды marketplace `ccteam role`. В отличие от переключения роли живой
/// сессии, это разовые операции установки файлов проекта.
#[derive(Subcommand)]
enum RoleCommand {
    /// Искать в marketplace ccteam-hub (пустой запрос выводит всё).
    Search {
        /// Подстрока запроса. Не указывайте (или передайте ""), чтобы вывести всё.
        #[arg(default_value = "")]
        query: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Установить агента/плагин hub в `.claude/` проекта.
    Add {
        /// ID плагина (как в `ccteam role search`).
        id: String,
        /// Переименовать установленный плагин (основа имени файла). По умолчанию —
        /// `id` плагина. Нормализуется в `[a-z0-9_-]`.
        #[arg(long = "as", value_name = "ROLE")]
        as_role: Option<String>,
        /// Slug целевого проекта (по умолчанию текущий каталог).
        #[arg(long, value_name = "SLUG")]
        project: Option<String>,
        /// Перезаписать существующий файл роли с тем же именем.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Вывести роли, установленные в `.claude/agents/` проекта.
    List {
        /// Slug целевого проекта. По умолчанию текущий рабочий каталог.
        #[arg(long, value_name = "SLUG")]
        project: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Искать skill в отобранном каталоге ccteam-hub.
    Search {
        #[arg(default_value = "")]
        query: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Установить один skill hub в пользовательскую библиотеку ccteam.
    Add {
        id: String,
        #[arg(long = "as", value_name = "STEM")]
        as_stem: Option<String>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Рекурсивно вывести пользовательскую библиотеку skills.
    Ls {
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Удалить один skill или целое дерево skills с явным --force.
    Rm {
        id: String,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Обновить закреплённый skill hub, если sha в каталоге отличается от диска.
    Update {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        id: Option<String>,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
    /// Зарегистрировать, обновить, вывести или удалить внешние источники skills.
    Source {
        #[command(subcommand)]
        cmd: SkillSourceCommand,
    },
    /// Обеспечить `.agents/skills` и символьную ссылку обнаружения Claude.
    EnsureProject {
        #[arg(long, value_name = "SLUG")]
        project: Option<String>,
    },
    /// Переместить устаревшие skills проекта в `.agents/skills` и связать с Claude.
    MigrateProject {
        #[arg(long, value_name = "SLUG")]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum SkillSourceCommand {
    /// Один раз клонировать git-источник или скопировать локальный каталог в библиотеку.
    Add {
        origin: String,
        #[arg(long, value_name = "STEM")]
        name: Option<String>,
        #[arg(long = "ref", value_name = "REV")]
        r#ref: Option<String>,
    },
    /// Обновить один зарегистрированный источник или все источники.
    Update {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        stem: Option<String>,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
    /// Вывести метаданные зарегистрированных источников.
    Ls {
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Удалить дерево зарегистрированного источника и снять его с регистрации.
    Rm { stem: String },
}

/// Операции `ccteam host` с несколькими хостами.
#[derive(Subcommand)]
enum HostCommand {
    /// Зарегистрировать эту машину как satellite основного демона.
    Join {
        /// Базовый URL основного демона (например, `http://192.168.1.10:7331`).
        #[arg(long)]
        daemon: String,
        /// Join-токен, выпущенный администратором основного демона.
        #[arg(long)]
        token: String,
        /// Необязательный предпочтительный ID хоста (по умолчанию slug hostname).
        #[arg(long)]
        host_id: Option<String>,
    },
    /// Выпустить join-токен через REST API основного демона (admin web-token).
    MintToken {
        /// Базовый URL основного демона.
        #[arg(long)]
        daemon: String,
        /// Hex admin web-token (или `ccteam:<hex>`). Запасной источник — `CCTEAM_WEB_TOKEN`.
        #[arg(long)]
        web_token: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        max_uses: Option<u32>,
    },
    /// Снять satellite с регистрации у основного демона (активный хост отклоняется без `--force`).
    Rm {
        /// Базовый URL основного демона.
        #[arg(long)]
        daemon: String,
        /// Hex admin web-token (или `ccteam:<hex>`). Запасной источник — `CCTEAM_WEB_TOKEN`.
        #[arg(long)]
        web_token: Option<String>,
        /// ID снимаемого хоста (см. `ccteam host ls` или страницу Team в web).
        host_id: String,
        /// Снять с регистрации, даже если хост сейчас онлайн.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Показать локальные учётные данные satellite (если подключён).
    Ls,
}

/// Подкоманды, скрытые в `ccteam internal`: обработчики хуков, низкоуровневые
/// утилиты сессий (peek / progress / attach), mux hook-emit, web-сервер.
#[derive(Subcommand)]
enum InternalCommand {
    /// Обработчики хуков Claude Code (читают JSON-полезную нагрузку хука из stdin).
    Hook {
        #[command(subcommand)]
        cmd: HookCommand,
    },
    /// Регистрация MCP-сервера ccteam для установленных вендоров в best-effort режиме.
    #[command(hide = true)]
    RegisterMcp {
        /// Вывести одну машиночитаемую JSON-строку.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Подключиться к tmux-панели активной сессии (та же логика, что у `session attach`).
    Attach {
        slug: String,
        /// ID чат-сессии; не указывайте для автоматического выбора единственной активной сессии slug.
        sid: Option<String>,
    },
    /// Вывести содержимое панели сессии без подключения.
    Peek {
        slug: String,
        /// ID чат-сессии; не указывайте для автоматического выбора единственной активной чат-сессии.
        sid: Option<String>,
    },
    /// Вывести progress.jsonl проекта, при необходимости с продолжением.
    Progress {
        slug: String,
        #[arg(long)]
        tail: bool,
    },
    /// Утилиты backend Mux (`hook-emit`).
    Mux {
        #[command(subcommand)]
        cmd: MuxCommand,
    },
    /// Запустить web UI отдельно (gateway-демон встраивает его по умолчанию).
    Web {
        /// Адрес прослушивания; для не-loopback привязок требуется токенная авторизация без `--no-auth`.
        #[arg(long, default_value = "0.0.0.0:7331")]
        bind: String,
        /// Адрес привязки DSH web companion. Не указывайте для порта `--bind` + 1; передайте
        /// `off` для отключения.
        #[arg(long, value_name = "ADDR|off")]
        dsh_web_bind: Option<String>,
        /// Отключить токенную авторизацию для write-endpoint. ОПАСНО на не-loopback привязках.
        #[arg(long, default_value_t = false)]
        no_auth: bool,
        /// Прочитать токен авторизации из файла (по умолчанию `~/.ccteam/web-token`).
        #[arg(long, value_name = "PATH")]
        token_file: Option<PathBuf>,
    },
    /// Пересобрать производный индекс experience.jsonl (только offline-исправление).
    Experience {
        #[command(subcommand)]
        cmd: ExperienceCommand,
    },
}

/// Команды `ccteam internal experience`.
#[derive(Subcommand)]
enum ExperienceCommand {
    /// Пересоздать experience.jsonl из turns и progress (вердикты сохраняются).
    Rebuild {
        /// Slug проекта.
        slug: String,
    },
}

#[derive(Subcommand)]
enum HookCommand {
    /// Добавить одну строку события в журнал progress проекта.
    ProgressAppend { event_type: String },
    /// Хук SessionStart: проверяемая no-op точка (без побочных эффектов в файловой системе).
    LoadContext,
    /// Хук PreToolUse для `AskUserQuestion`: отклонить, направить в асинхронный clarify flow.
    InterceptAsk,
    /// Хук PermissionRequest: вызов инструмента не из allowlist → IM round-trip approve/deny.
    PermissionRequest,
    /// Callback хука chat-режима: сопоставить каждое событие хука с отправкой progress.
    ChatProgress {
        /// Аргумент события хука (например, `session-start`, `stop`, `tool-use`, `session-end`).
        event: String,
    },
}

fn main() -> Result<()> {
    // V0.8 W2a — rmux SDK daemon spawn protocol. BEFORE clap parses
    // argv, intercept the `--__internal-daemon <socket>` form emitted
    // by `rmux_sdk::Rmux::connect_or_start` so the same ccteam binary
    // can host the rmux daemon. See
    // `docs/versions/v0-8-rmux/w2-daemon-spawn-protocol.md` and
    // `references/rmux/crates/rmux-sdk/src/handles/rmux/connect.rs`
    // (lines 150-180) for the upstream invariant this implementation
    // tracks.
    //
    // TODO(V0.8-W2c): `RmuxBackend::new()` sets `RMUX_SDK_DAEMON_BINARY`
    // lazily in its constructor — enough for the orchestrator's own
    // use, but child processes ccteam spawns (claude / codex subagents)
    // inherit env at fork time. When W2c migrates `claude_tui.rs` /
    // `codex_exec.rs` to the trait, set `RMUX_SDK_DAEMON_BINARY =
    // current_exe()` here at `main()` entry so inherited-env propagation
    // covers any subagent that later uses rmux-sdk. No subagent uses it
    // today, so W2a leaves this as a TODO rather than a behavior change.
    {
        let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
        if raw_args.len() >= 3 && raw_args[1] == ccteam_harness::daemon::INTERNAL_DAEMON_FLAG {
            let socket = raw_args[2].clone();
            ccteam_harness::daemon::run_internal_daemon(socket)
                .context("ccteam internal rmux daemon")?;
            return Ok(());
        }
    }

    // V0.8 W2c — set `RMUX_SDK_DAEMON_BINARY` to this binary at main()
    // entry (not lazily in RmuxBackend::new) so the value is in the
    // process env BEFORE any child process is forked. Child agents
    // (claude / codex sessions) that later use rmux-sdk inherit it and
    // resolve the same ccteam binary as their daemon host, rather than
    // falling back to a `rmux` binary on PATH that may not exist.
    // Idempotent — honors an explicit operator override.
    if std::env::var_os(ccteam_harness::daemon::SDK_DAEMON_BINARY_ENV).is_none() {
        if let Ok(exe) = std::env::current_exe() {
            std::env::set_var(
                ccteam_harness::daemon::SDK_DAEMON_BINARY_ENV,
                exe.as_os_str(),
            );
        }
    }

    let cli = Cli::parse();

    // v0.8.20 — `--home <dir>` overrides CCTEAM_HOME for this invocation (a
    // second, fully isolated instance, e.g. `~/.ccteam2`). Set before the F173
    // pin below so every path resolution + child spawn sees it.
    if let Some(home) = &cli.home {
        std::env::set_var("CCTEAM_HOME", home);
    }

    // F173 — pin `CCTEAM_HOME` early so child spawn paths
    // (CodexExecAdapter ledger hook, etc.) see the same root the CLI
    // resolved. Production users almost never set `CCTEAM_HOME`
    // explicitly; without this seed the cost-budget.json ledger hook
    // in CodexExecAdapter would silently no-op (the adapter gates the
    // hook on `CCTEAM_HOME` explicitly to avoid cargo-test pollution).
    // Idempotent — if the operator already set it, we honour their
    // override.
    if std::env::var_os("CCTEAM_HOME").is_none() {
        if let Ok(paths) = CcteamPaths::from_env() {
            std::env::set_var("CCTEAM_HOME", &paths.root);
        }
    }

    // No subcommand → print help instead of silently exiting. Prior
    // behavior (V0.4.0 and earlier) was Ok(()) with nothing on stdout,
    // which left users wondering whether ccteam was installed at all.
    let Some(command) = cli.command else {
        use clap::CommandFactory;
        Cli::command().print_help().context("print help")?;
        println!();
        return Ok(());
    };

    match command {
        Command::Init {
            r#in,
            slug,
            force,
            owner,
        } => {
            let paths = CcteamPaths::from_env()?;
            let report = commands::run_init(
                &paths,
                InitOptions {
                    install_in: r#in,
                    slug,
                    force,
                    owner,
                    ..InitOptions::default()
                },
            )?;
            print!("{report}");
            Ok(())
        }
        Command::Start {
            no_web,
            no_imd,
            web_bind,
            dsh_web_bind,
            web_no_auth,
            web_token_file,
            no_clipboard,
        } => run_start(
            StartWebOpts {
                disabled: no_web,
                bind: web_bind,
                dsh_bind: dsh_web_bind,
                no_auth: web_no_auth,
                token_file: web_token_file,
                no_clipboard,
            },
            StartImdOpts { disabled: no_imd },
        ),
        Command::Status => run_status(),
        Command::Internal { cmd } => run_internal(cmd),
        // v0.9.7 — `ccteam stop` is a delegating alias for `daemon stop`
        // (the trigger-file channel is retired).
        Command::Stop => daemon_cli::run_daemon_stop(false, false),
        Command::Daemon { cmd } => match cmd {
            DaemonCommand::Start {
                web_bind,
                dsh_web_bind,
                json,
            } => daemon_cli::run_daemon_start(&web_bind, dsh_web_bind.as_deref(), json),
            DaemonCommand::Stop { force, json } => daemon_cli::run_daemon_stop(force, json),
            DaemonCommand::Restart {
                web_bind,
                dsh_web_bind,
                json,
                if_managed,
            } => {
                daemon_cli::run_daemon_restart(&web_bind, dsh_web_bind.as_deref(), json, if_managed)
            }
            DaemonCommand::Status { json } => daemon_cli::run_daemon_status(json),
            DaemonCommand::Logs { n, follow, json } => daemon_cli::run_daemon_logs(n, follow, json),
        },
        // v0.9.7 — `ccteam update`: channel-aware self-update + upgrade-restart.
        Command::Update {
            now,
            no_restart,
            json,
            force,
        } => update::run_update(now, no_restart, json, force),
        // v0.8.6 W3/W4a — `ccteam project <ls|show|new|stop|rm>` group.
        Command::Project { cmd } => match cmd {
            ProjectCommand::Ls { format } => run_ls(format),
            ProjectCommand::Show { slug, format } => match slug {
                Some(s) => run_show(&s, format),
                None => show_slug_picker(),
            },
            ProjectCommand::New { slug } => run_new(slug),
            ProjectCommand::Rm {
                slug,
                purge,
                dry_run,
                force,
            } => {
                let paths = CcteamPaths::from_env()?;
                let report = commands::run_remove(
                    &paths,
                    &slug,
                    commands::RemoveOptions {
                        purge,
                        dry_run,
                        force,
                    },
                )?;
                print!("{report}");
                // Best-effort must not mean fail-open (same decision as
                // `doctor --repair-progress`): a post-ACK cleanup that could not
                // complete leaves ccteam state on disk, so a scripted
                // `project rm … && …` must stop. Code 2 is distinct from the
                // code-1 "removal refused / nothing committed" failures so a
                // wrapper can tell a half-finished sweep from a no-op.
                if !report.cleanup_failures.is_empty() {
                    std::process::exit(2);
                }
                Ok(())
            }
            ProjectCommand::Stop { slug } => {
                let paths = CcteamPaths::from_env()?;
                let report = commands::run_project_stop(&paths, &slug)?;
                print!("{report}");
                Ok(())
            }
        },
        // v0.8.6 W4a — `ccteam session <...>` group.
        Command::Session { cmd } => run_session(cmd),
        // v0.8.7 W3 — `ccteam role <search|add|list>` group.
        Command::Role { cmd } => run_role(cmd),
        Command::Skill { cmd } => run_skill(cmd),
        Command::Host { cmd } => run_host(cmd),
        Command::Doctor {
            verify_mcp,
            repair_progress,
            json,
        } => run_doctor(commands::DoctorOptions {
            verify_mcp,
            verify_mcp_json: json,
            repair_progress,
        }),
        Command::Config { args } => run_config(args),
    }
}

/// Dispatch `ccteam config`. The interactive menu and the headless forms
/// both live in `commands`; this only routes the positional words. Forms:
///
/// - (no args) → interactive menu (`commands::run_config_menu`)
/// - `mcp` → register/refresh the ccteam MCP server (headless escape hatch
///   for the menu's item 1, so installers / CI can wire MCP without a TTY)
/// - `show` → print preferences
/// - `get <key>` → read one preference
/// - `<key>` → read one preference (single non-keyword word)
/// - `<key> <value>` → set one preference (the bare two-arg form)
fn run_config(args: Vec<String>) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    match args.as_slice() {
        [] => {
            let out = commands::run_config_menu(&paths)?;
            print!("{out}");
            Ok(())
        }
        [one] if one == "mcp" => {
            let out = commands::run_config_install_mcp()?;
            print!("{out}");
            Ok(())
        }
        [one] if one == "show" => {
            let out = commands::run_prefs_show(&paths)?;
            print!("{out}");
            Ok(())
        }
        [one] => {
            // Single non-keyword word → read that preference key.
            let out = commands::run_prefs_get(&paths, one)?;
            println!("{out}");
            Ok(())
        }
        [first, second] if first == "get" => {
            let out = commands::run_prefs_get(&paths, second)?;
            println!("{out}");
            Ok(())
        }
        [key, value] => {
            // Bare two-arg form `config <key> <value>` → set the pref.
            let out = commands::run_prefs_set(&paths, key, value)?;
            println!("{out}");
            Ok(())
        }
        // clap caps this at 2 positionals (`num_args = 0..=2`), so the
        // 3+ arm is unreachable; keep it total for the compiler.
        _ => anyhow::bail!(
            "ccteam config: слишком много аргументов (ожидается не более `<key> <value>`)"
        ),
    }
}

/// Dispatch `ccteam session <subcmd>` — enumeration + terminal attach only;
/// everything else about a session is driven through the daemon (IM / web).
fn run_session(cmd: SessionCommand) -> Result<()> {
    match cmd {
        SessionCommand::Ls => commands::run_sessions(),
        SessionCommand::Attach { slug, sid } => run_internal_attach(&slug, sid.as_deref()),
    }
}

/// Multi-host CLI surface.
fn run_host(cmd: HostCommand) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("создать runtime tokio")?;
    match cmd {
        HostCommand::Join {
            daemon,
            token,
            host_id,
        } => rt.block_on(host_join(&paths, daemon, token, host_id)),
        HostCommand::MintToken {
            daemon,
            web_token,
            label,
            max_uses,
        } => rt.block_on(host_mint_token(daemon, web_token, label, max_uses)),
        HostCommand::Rm {
            daemon,
            web_token,
            host_id,
            force,
        } => rt.block_on(host_rm(daemon, web_token, host_id, force)),
        HostCommand::Ls => host_ls(&paths),
    }
}

async fn host_join(
    paths: &CcteamPaths,
    daemon: String,
    token: String,
    host_id: Option<String>,
) -> Result<()> {
    let hostname = ccteam_core::read_hostname().unwrap_or_else(|| "satellite".into());
    let body = ccteam_core::HostJoinRequest {
        token: token.clone(),
        host_id,
        hostname: hostname.clone(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        ccteam_version: env!("CARGO_PKG_VERSION").to_string(),
        agents: vec![],
    };
    let url = format!("{}/api/v1/hosts/join", daemon.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header(
            "Authorization",
            format!("Bearer ccteam:{}", token.trim_start_matches("ccteam:")),
        )
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("подключение не удалось: HTTP {status}: {text}");
    }
    let join: ccteam_core::HostJoinResponse = serde_json::from_str(&text)
        .with_context(|| format!("разобрать ответ подключения: {text}"))?;
    let self_rec = ccteam_core::SatelliteSelf {
        daemon_url: daemon.trim_end_matches('/').to_string(),
        host: join.host.clone(),
        agent_token: join.agent_token.clone(),
        heartbeat_ttl_secs: join.heartbeat_ttl_secs,
        joined_at: chrono::Utc::now().to_rfc3339(),
    };
    let self_path = ccteam_core::SatelliteSelf::path_in(&paths.root);
    self_rec.save(&self_path)?;
    println!(
        "подключён как хост `{}` (токен агента сохранён в {})",
        join.host,
        self_path.display()
    );
    println!("heartbeat_ttl_secs={}", join.heartbeat_ttl_secs);
    println!(
        "работающий `ccteam start` на этой машине подключится к демону в течение 30 с \
         (обратное соединение — satellite не открывает порт; запустите `ccteam start`, \
         если он не запущен)"
    );
    Ok(())
}

async fn host_mint_token(
    daemon: String,
    web_token: Option<String>,
    label: Option<String>,
    max_uses: Option<u32>,
) -> Result<()> {
    let tok = web_token
        .or_else(|| std::env::var("CCTEAM_WEB_TOKEN").ok())
        .ok_or_else(|| anyhow::anyhow!("требуется --web-token или CCTEAM_WEB_TOKEN"))?;
    let bare = tok.trim().trim_start_matches("ccteam:");
    let url = format!("{}/api/v1/hosts/join-token", daemon.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer ccteam:{bare}"))
        .json(&serde_json::json!({"label": label, "max_uses": max_uses}))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("выпуск токена не удался: HTTP {status}: {text}");
    }
    println!("{text}");
    Ok(())
}

/// TEAM-5 — `ccteam host rm`: deregister a satellite via the main daemon's
/// `DELETE /api/v1/hosts/{host}`. Mirrors [`host_mint_token`]'s shape.
async fn host_rm(
    daemon: String,
    web_token: Option<String>,
    host_id: String,
    force: bool,
) -> Result<()> {
    let tok = web_token
        .or_else(|| std::env::var("CCTEAM_WEB_TOKEN").ok())
        .ok_or_else(|| anyhow::anyhow!("требуется --web-token или CCTEAM_WEB_TOKEN"))?;
    let bare = tok.trim().trim_start_matches("ccteam:");
    let mut url = format!("{}/api/v1/hosts/{host_id}", daemon.trim_end_matches('/'));
    if force {
        url.push_str("?force=true");
    }
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer ccteam:{bare}"))
        .send()
        .await
        .with_context(|| format!("DELETE {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("удаление хоста не удалось: HTTP {status}: {text}");
    }
    println!("{text}");
    Ok(())
}

// v0.9.0 reverse-connection — the satellite is a CLIENT
// (`ccteam_web::satellite::run_satellite_client`, spawned by `run_start`):
// it dials OUT to the main daemon and exposes no listener, so the old
// `ccteam host serve` / `host heartbeat` commands, the `:7332` exec-bridge
// bind, and the `CCTEAM_EXEC_BIND`/`CCTEAM_EXEC_ADVERTISE_URL` knobs are
// gone. Every node just runs `ccteam start`; whether it is ALSO a
// satellite depends only on `state/hosts/self.json` (polled in-client, so
// a `host join` after startup activates without a restart).

fn host_ls(paths: &CcteamPaths) -> Result<()> {
    let self_path = ccteam_core::SatelliteSelf::path_in(&paths.root);
    if !self_path.exists() {
        println!("(не подключён — нет {})", self_path.display());
        return Ok(());
    }
    let me = ccteam_core::SatelliteSelf::load(&self_path)?;
    println!("хост:          {}", me.host);
    println!("демон:         {}", me.daemon_url);
    println!("подключён:     {}", me.joined_at);
    println!("heartbeat_ttl: {} с", me.heartbeat_ttl_secs);
    println!(
        "токен агента:  {}…",
        &me.agent_token[..8.min(me.agent_token.len())]
    );
    Ok(())
}

/// `ccteam role <search|add|list>` dispatcher. `search` is offline (no
/// project / no network); `add` / `list` resolve a project dir from
/// `--project <slug>` (or the cwd) and call into the commands layer.
fn run_role(cmd: RoleCommand) -> Result<()> {
    match cmd {
        RoleCommand::Search { query, format } => {
            let paths = CcteamPaths::from_env()?;
            let out = commands::run_role_search(&paths, &query, format)?;
            print!("{out}");
            Ok(())
        }
        RoleCommand::Add {
            id,
            as_role,
            project,
            force,
        } => {
            let paths = CcteamPaths::from_env()?;
            let out =
                commands::run_role_add(&paths, &id, as_role.as_deref(), project.as_deref(), force)?;
            print!("{out}");
            Ok(())
        }
        RoleCommand::List { project, format } => {
            let paths = CcteamPaths::from_env()?;
            let out = commands::run_role_list(&paths, project.as_deref(), format)?;
            print!("{out}");
            Ok(())
        }
    }
}

fn run_skill(cmd: SkillCommand) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let out = match cmd {
        SkillCommand::Search { query, format } => {
            commands::run_skill_search(&paths, &query, format)?
        }
        SkillCommand::Add { id, as_stem, force } => {
            commands::run_skill_add(&paths, &id, as_stem.as_deref(), force)?
        }
        SkillCommand::Ls { json } => commands::run_skill_list(&paths, json)?,
        SkillCommand::Rm { id, force } => commands::run_skill_remove(&paths, &id, force)?,
        SkillCommand::Update { id, all } => commands::run_skill_update(&paths, id.as_deref(), all)?,
        SkillCommand::Source { cmd } => match cmd {
            SkillSourceCommand::Add {
                origin,
                name,
                r#ref,
            } => {
                commands::run_skill_source_add(&paths, &origin, name.as_deref(), r#ref.as_deref())?
            }
            SkillSourceCommand::Update { stem, all } => {
                commands::run_skill_source_update(&paths, stem.as_deref(), all)?
            }
            SkillSourceCommand::Ls { json } => commands::run_skill_source_list(&paths, json)?,
            SkillSourceCommand::Rm { stem } => commands::run_skill_source_remove(&paths, &stem)?,
        },
        SkillCommand::EnsureProject { project } => {
            commands::run_skill_ensure_project(&paths, project.as_deref())?
        }
        SkillCommand::MigrateProject { project } => {
            commands::run_skill_migrate_project(&paths, project.as_deref())?
        }
    };
    print!("{out}");
    Ok(())
}

/// Dispatch `ccteam internal <subcmd>` — the hidden low-level surface
/// (hook handlers, MCP server, session utilities, mux hook-emit,
/// standalone web server).
fn run_internal(cmd: InternalCommand) -> Result<()> {
    match cmd {
        InternalCommand::Hook { cmd } => run_hook(cmd),
        InternalCommand::RegisterMcp { json } => run_internal_register_mcp(json),
        InternalCommand::Attach { slug, sid } => run_internal_attach(&slug, sid.as_deref()),
        InternalCommand::Peek { slug, sid } => run_internal_peek(&slug, sid.as_deref()),
        InternalCommand::Progress { slug, tail } => run_progress(&slug, tail),
        InternalCommand::Mux { cmd } => run_mux(cmd),
        InternalCommand::Web {
            bind,
            dsh_web_bind,
            no_auth,
            token_file,
        } => {
            init_tracing();
            commands::run_web(commands::WebOptions {
                bind,
                dsh_bind: dsh_web_bind,
                no_auth,
                token_file,
            })
        }
        InternalCommand::Experience { cmd } => run_experience(cmd),
    }
}

fn run_internal_register_mcp(json_output: bool) -> Result<()> {
    let mut rows = Vec::new();
    let mut failures = 0usize;
    for (vendor, result) in mcp_serve::auto_register_vendor_mcp() {
        match result {
            Ok(Some(path)) => rows.push(serde_json::json!({
                "vendor": vendor,
                "status": "registered",
                "path": path,
            })),
            Ok(None) => rows.push(serde_json::json!({
                "vendor": vendor,
                "status": "skipped",
            })),
            Err(err) => {
                failures += 1;
                rows.push(serde_json::json!({
                    "vendor": vendor,
                    "status": "error",
                    "error": err.to_string(),
                }));
            }
        }
    }
    if json_output {
        println!("{}", serde_json::json!({"results": rows}));
    } else {
        for row in &rows {
            let vendor = row["vendor"].as_str().unwrap_or("неизвестный");
            match row["status"].as_str().unwrap_or("error") {
                "registered" => println!(
                    "{vendor}: зарегистрирован {}",
                    row["path"].as_str().unwrap_or("<неизвестно>")
                ),
                "skipped" => println!("{vendor}: пропущен"),
                _ => println!(
                    "{vendor}: ошибка: {}",
                    row["error"].as_str().unwrap_or("неизвестная ошибка")
                ),
            }
        }
    }
    if failures > 0 {
        anyhow::bail!("не удалось зарегистрировать MCP для вендоров: {failures}");
    }
    Ok(())
}

/// `ccteam internal experience rebuild <slug>`.
///
/// Regenerates the derived experience index offline. A live daemon may
/// still be appending concurrent turn rows — use only for disaster
/// recovery / offline repair.
fn run_experience(cmd: ExperienceCommand) -> Result<()> {
    match cmd {
        ExperienceCommand::Rebuild { slug } => {
            let paths = CcteamPaths::from_env()?;
            let project_dir = paths.project_dir(&slug);
            if !project_dir.exists() {
                anyhow::bail!("проект не найден: {slug} ({})", project_dir.display());
            }
            let progress = paths.progress_jsonl(&slug);
            let progress_arg = (progress.exists()
                || ccteam_harness::execution::progress_bridge::progress_archive_path(&progress)
                    .exists())
            .then_some(progress.as_path());
            let (turns, verdicts) = ccteam_harness::execution::experience::rebuild_experience(
                &project_dir,
                progress_arg,
            )?;
            println!(
                "Индекс опыта для {slug} пересобран: ответов {turns}, проецировано вердиктов {verdicts} → {}",
                ccteam_harness::execution::experience::experience_jsonl_path(&project_dir)
                    .display()
            );
            Ok(())
        }
    }
}

/// `ccteam mux <subcommand>` dispatch.
fn run_mux(cmd: MuxCommand) -> Result<()> {
    match cmd {
        MuxCommand::HookEmit {
            kind,
            action,
            session,
            json,
        } => run_mux_hook_emit(kind, action, session, json),
    }
}

/// Connect to `~/.ccteam/run/hook.sock` and forward one
/// hook firing to the orchestrator (single-writer path).
///
/// QUIET-on-failure contract: when the sink isn't listening (no
/// orchestrator running with the flag set, stale socket), we exit
/// non-zero WITHOUT printing to stderr. A Claude Code hook subprocess
/// that error-spammed stderr would pollute the chat UI; a silent
/// non-zero exit is the documented behaviour (Claude Code tolerates a
/// failed fire-and-forget hook).
fn run_mux_hook_emit(
    kind: String,
    action: Option<String>,
    session: Option<String>,
    json: Option<String>,
) -> Result<()> {
    // Hookless session (stream-json): no-op, same as `run_hook`. This is the
    // W6 daemon-bus form that bypasses hook.sh's guard.
    if std::env::var_os("CCTEAM_HOOKLESS").is_some() {
        return Ok(());
    }
    // Derive the session id from env when not given explicitly. The hook
    // subprocess inherits CCTEAM_CHAT_SLUG / CCTEAM_CHAT_ROLE from the
    // chat tmux session (claude_tui::chat_spawn_env_owned). Either may
    // be empty if env propagation failed; that's fine — the orchestrator
    // re-derives routing from the payload the same way the legacy hook
    // handler did, so session_id is informational.
    let session_id = session.unwrap_or_else(|| {
        let slug = std::env::var("CCTEAM_CHAT_SLUG").unwrap_or_default();
        let role = std::env::var("CCTEAM_CHAT_ROLE").unwrap_or_default();
        format!("{slug}-{role}")
    });

    // Payload: `--json` inline (unless it's `-`), else read stdin.
    let payload_json = match json {
        Some(s) if s != "-" => s,
        _ => {
            use std::io::Read;
            let mut buf = String::new();
            // A hook may fire with no stdin (e.g. SessionStart in some
            // configs); tolerate an empty read.
            let _ = std::io::stdin().lock().read_to_string(&mut buf);
            buf
        }
    };

    let event = ccteam_harness::HookEvent {
        session_id,
        kind,
        action,
        payload_json,
    };
    let socket = ccteam_harness::default_ccteam_hook_socket_path();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("создать runtime tokio для mux hook-emit")?;
    match runtime.block_on(ccteam_harness::HookSinkClient::emit(&socket, &event)) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Quiet non-zero exit — no stderr. See contract above.
            std::process::exit(1);
        }
    }
}

/// `ccteam session attach <slug> [sid]` / `ccteam internal attach <slug>
/// [sid]` — reach gateway chat-mode bot
/// sessions first (the project-oriented [`commands::run_attach`] cannot see
/// `ccteam-chat-*`), then fall back to the project session when no live chat
/// session matches `slug`.
fn run_internal_attach(slug: &str, sid: Option<&str>) -> Result<()> {
    if commands::try_attach_chat_session(slug, sid)? {
        return Ok(());
    }
    let paths = CcteamPaths::from_env()?;
    commands::run_attach(&paths, slug)
}

fn run_progress(slug: &str, tail: bool) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    commands::run_progress(&paths, slug, tail)
}

fn run_doctor(opts: commands::DoctorOptions) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    // V0.6.6 F171 — `--verify-mcp` carries a non-zero exit code (1) when
    // any STUB tool is registered so CI can gate the "0 STUB" invariant:
    // a deterministic outcome → structured non-error exit.
    if opts.verify_mcp {
        let report = commands::run_verify_mcp();
        let body = if opts.verify_mcp_json {
            report.render_json()
        } else {
            report.render_text()
        };
        print!("{body}");
        if !report.ok() {
            std::process::exit(1);
        }
        return Ok(());
    }
    // The repair sweep is per-slug best effort, but a slug it could not sweep
    // must never look like a clean run: print the whole report, then let the
    // failure count feed the exit code so scripted `--repair-progress && …`
    // flows stop instead of proceeding over an unswept home.
    let mut repair_failed = 0_u64;
    if opts.repair_progress {
        let (body, failed) = doctor::repair_progress(&paths)?;
        print!("{body}");
        repair_failed = failed;
    }
    // Any other invocation is the full readiness checkup.
    let (body, any_fail) = doctor::run_readiness_checkup(&paths);
    print!("{body}");
    if any_fail || repair_failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn run_hook(cmd: HookCommand) -> Result<()> {
    // Hookless session (stream-json protocol): the spawn injects
    // CCTEAM_HOOKLESS=1 so the hook chain never fires (no SessionStart-POST
    // init deadlock, no double-emit — events come from the child's stdout).
    // `hook.sh` already short-circuits before reaching here, but the W6
    // `mux hook-emit` form and any direct CLI invocation bypass hook.sh, so
    // guard here too. A no-op (Ok / empty stdout) = "no opinion" for any
    // decision hook → the tool proceeds.
    if std::env::var_os("CCTEAM_HOOKLESS").is_some() {
        return Ok(());
    }
    let paths = CcteamPaths::from_env()?;
    // V0.6.1 F139: dispatch through the shared `ccteam_hooks::dispatch`
    // entry so the CLI fallback and the daemon HTTP route share one
    // codepath. `intercept-ask` returns a JSON decision that we echo to
    // stdout; the other hooks are side-effect-only.
    let (kind, action, needs_stdin): (&str, Option<&str>, bool) = match &cmd {
        HookCommand::ProgressAppend { event_type } => {
            ("progress-append", Some(event_type.as_str()), true)
        }
        HookCommand::LoadContext => ("load-context", None, true),
        // v0.8.5 D6 — intercept-ask now reads the AskUserQuestion `tool_input`
        // from stdin (the chat variant routes the question to IM); the bg
        // variant ignores the payload and still denies.
        HookCommand::InterceptAsk => ("intercept-ask", None, true),
        // v0.8.7 W2 (DB.3) — reads the PermissionRequest payload from stdin
        // and prints the allow/deny decision JSON.
        HookCommand::PermissionRequest => ("permission-request", None, true),
        HookCommand::ChatProgress { event } => ("chat-progress", Some(event.as_str()), true),
    };
    let stdin = if needs_stdin {
        parse_hook_stdin_json()?
    } else {
        serde_json::Value::Null
    };
    let response = ccteam_hooks::dispatch(&paths, kind, action, &stdin)?;
    if let Some(value) = response {
        println!("{}", serde_json::to_string(&value)?);
    }
    Ok(())
}

fn parse_hook_stdin_json() -> Result<serde_json::Value> {
    serde_json::from_reader(std::io::stdin().lock()).context("разобрать stdin hook как JSON")
}

struct StartWebOpts {
    disabled: bool,
    bind: String,
    dsh_bind: Option<String>,
    no_auth: bool,
    token_file: Option<PathBuf>,
    /// When true, skip the clipboard probe in `print_web_banner` and
    /// just print the token. CI / unattended.
    no_clipboard: bool,
}

/// IM supervisor task knobs. Today the only operator switch is
/// `disabled` (mirror of `--no-imd`); future knobs (custom credentials
/// path, custom registry root) attach here without changing `run_start`'s
/// signature.
struct StartImdOpts {
    disabled: bool,
}

const DAEMON_MAX_BLOCKING_THREADS: usize = 16;

fn build_daemon_runtime(workers: usize) -> Result<tokio::runtime::Runtime> {
    // `daemon.workers=1` (or CCTEAM_DAEMON_WORKERS=1) is the rollback switch:
    // it keeps the runtime topology while restoring one async worker thread.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(DAEMON_MAX_BLOCKING_THREADS)
        .enable_all()
        .build()
        .context("создать runtime демона tokio")
}

fn run_start(web: StartWebOpts, imd: StartImdOpts) -> Result<()> {
    init_tracing();

    // V0.8 rmux — the mux backend default is rmux, resolved in the
    // library (`ccteam_harness::from_env` / `default_backend`): rmux is the
    // bundled always-available backend so ccteam works with no external
    // tmux. An operator opts out with `CCTEAM_MUX_BACKEND=tmux`. Log the
    // effective choice for ops visibility at orchestrator startup (no
    // set_var here — the library is the single source of truth).
    //
    // Validated end-to-end on Linux (rmux-smoke CI job: spawn→send→
    // capture→kill + reconnect-after-daemon-death). macOS/Windows run on
    // the CI matrix; real-claude mode-3 burn-in is the merge-to-main gate
    // (this is a no-merge evaluation branch). See
    // docs/versions/v0-8-rmux/w-flip-default-migration-plan.md.
    tracing::info!(
        backend = ?ccteam_harness::backend_kind_from_env(),
        "ccteam start: mux backend (задайте CCTEAM_MUX_BACKEND=tmux для использования tmux)"
    );

    let paths = CcteamPaths::from_env()?;
    let daemon_workers = ccteam_core::config::load(&paths.root)
        .context("загрузить конфигурацию runtime демона")?
        .daemon
        .effective_workers()
        .context("определить daemon.workers")?;

    // Ensure the global `~/.ccteam/` home (canonical dirs +
    // `hooks/hook.sh` dispatcher) is complete BEFORE the daemon starts
    // serving, so any session it spawns — including projects created via
    // the web / IM path while the daemon is up — finds the hook.sh the
    // project `.claude/settings.local.json` references. Idempotent.
    // Best-effort: a failure here must not crash the daemon (the
    // per-create-path `ensure_ccteam_home` is the real safety net).
    if let Err(err) = ccteam_core::ensure_ccteam_home(&paths) {
        tracing::warn!(error = %err, "не удалось подготовить ~/.ccteam/ при запуске демона; сессии могут не найти hook.sh до `ccteam doctor --install-hooks`");
    }

    // Record the MCP URL this daemon's own web bind implies BEFORE registering,
    // so every vendor entry written below — and every later out-of-band
    // `ccteam config mcp` — targets the port we actually listen on. The old
    // stdio entry was port-agnostic; an HTTP one is not, so guessing the default
    // here would break exactly the operators who bind somewhere else.
    if let Err(err) = ccteam_harness::execution::mcp_config::record_daemon_mcp_url(
        &paths.root.join("run"),
        &web.bind,
    ) {
        tracing::warn!(error = %err, bind = %web.bind, "не удалось записать URL MCP демона; регистрация вендоров использует адрес привязки по умолчанию");
    }

    let mut registered = Vec::new();
    for (vendor, result) in crate::mcp_serve::auto_register_vendor_mcp() {
        match result {
            Ok(Some(path)) => registered.push(format!("{vendor}={}", path.display())),
            Ok(None) => {}
            Err(err) => tracing::warn!(
                vendor,
                error = %err,
                "не удалось автоматически зарегистрировать MCP вендора при запуске демона"
            ),
        }
    }
    tracing::info!(
        ?registered,
        "автоматическая регистрация MCP вендоров завершена"
    );

    // V0.4.0 F60: the shipped team seed writer was deleted with the
    // phase machinery (F63 will reintroduce a workflow seed). Daemon
    // start no longer self-heals — operators supply their own
    // `~/.ccteam/teams/<name>/team.yaml`.
    //
    // v0.8.6 (review-fix #3): the old "phases dir not found; run ccteam
    // init to unpack templates" hint was orchestrator-era dead code —
    // `ccteam init` no longer creates `phases/` (and templates are no
    // longer unpacked), so it printed spuriously on every clean start.
    // Removed.

    if !paths.projects_root.exists()
        || std::fs::read_dir(&paths.projects_root)
            .map(|mut it| it.next().is_none())
            .unwrap_or(true)
    {
        eprintln!(
            "ccteam: проектов в {} пока нет.\n  запустите в другом терминале: ccteam project new <slug> (например, `ccteam project new demo`)",
            paths.projects_root.display()
        );
    }

    // v0.9.7 — the trigger-file stop channel is retired (SIGTERM is the
    // only stop signal). Sweep a stale legacy trigger left by a pre-0.9.7
    // `ccteam stop` so nothing on disk suggests it still does anything.
    {
        let user = std::env::var("USER").unwrap_or_else(|_| "ccteam".into());
        let legacy_trigger = PathBuf::from("/tmp").join(format!("ccteam-{user}.shutdown"));
        let _ = std::fs::remove_file(&legacy_trigger);
    }

    // Re-publish the Telegram command menu (`setMyCommands`) on EVERY
    // `ccteam start`, BEFORE the single-instance pidfile guard below.
    // The daemon also registers the menu at its own startup
    // (`run_daemon_with_shutdown`), but that only fires when a *fresh*
    // daemon boots: if one is already running, the `write_pidfile` guard
    // aborts this `ccteam start` before any registration happens, and a
    // token configured *after* first start never re-registers. Doing it
    // here makes the menu refresh reliably on every start regardless —
    // idempotent (`setMyCommands` replaces the whole menu) and inert to a
    // live daemon/sessions (it is one HTTPS POST to the Bot API). Skipped
    // when IM is off (`--no-imd`); warn-only so a Bot-API hiccup never
    // blocks startup. A short throwaway runtime keeps this self-contained
    // ahead of the main runtime built below.
    if !imd.disabled {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => {
                if let Err(err) = rt.block_on(ccteam_im::refresh_telegram_command_menu(None)) {
                    tracing::warn!(
                        error = %err,
                        "ccteam start: не удалось обновить меню команд Telegram (setMyCommands); меню может быть устаревшим"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "ccteam start: не удалось создать runtime для обновления меню Telegram; пропускаю");
            }
        }
    }

    // v0.9.7 (F1.6) — double-instance guard: if an instance already
    // serves the MCP socket (managed, foreground, or user-supervised),
    // exit loudly instead of fighting over sockets. This replaces the
    // retired pidfile mutex: the daemon process itself never writes the
    // pid record — `state/orchestrator.pid` is launcher-written ONLY
    // (`ccteam daemon start`), so "managed" means exactly "spawned by
    // the launcher". Deliberately AFTER the Telegram menu refresh above
    // (that refresh must run even when a daemon is already up).
    let preflight = ccteam_core::check_daemon_health(&paths);
    if preflight.is_healthy() {
        anyhow::bail!(
            "ccteam start: another ccteam daemon is already serving {} — \
             use `ccteam daemon status` to inspect it, `ccteam daemon stop` to stop a \
             managed one, or Ctrl-C the other foreground instance first.",
            ccteam_core::daemon_socket_path(&paths).display()
        );
    }

    // Print a single banner up front so the operator can paste the web
    // URL into a browser without grepping mid-log noise. Skip when
    // web is disabled. Token is resolved here (read or pre-generate)
    // only when bind is non-loopback so the loopback fast-path stays
    // zero-IO.
    if !web.disabled {
        print_web_banner(&paths, &web);
    }

    let hook_sink_paths = paths.clone();
    let runtime = build_daemon_runtime(daemon_workers)?;
    let result = runtime.block_on(async move {
        // v8.1 gateway + web + hook sink share one shutdown signal
        // (Ctrl-C, SIGTERM, or `ccteam stop` trigger file). No flow
        // orchestrator is constructed in this path.
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let web_chat_bridge = if !web.disabled && !imd.disabled {
            Some(web_chat_bridge::build())
        } else {
            None
        };

        // V0.8.6 W5b — composition root for the resource-API spine. When BOTH
        // web and the IM gateway run in this process, build the shared
        // `Arc<Mutex<Gateway>>` ONCE here, before either task spawns, and hand
        // the SAME handle to the web `AppState` (read/drive sessions over HTTP)
        // and the daemon (owns the session map). Building it pre-spawn removes
        // the web-vs-IM spawn-order race. When web is off (standalone "internal
        // web" has no daemon gateway) or IM is off, leave it `None`: the daemon
        // then builds + owns its own gateway exactly as before, and any web
        // session endpoint returns 503 (gateway is `None` in `AppState`).
        // v0.8.22 P0-2 — `build_gateway_for_daemon` now also returns the
        // stream-json Claude adapter singleton it baked into the gateway, so
        // it can be threaded into `DaemonArgs::claude_stream_json_adapter`
        // below: the daemon wires the production HITL resolver onto this
        // EXACT adapter once pending + the event sink exist.
        // v0.9.0 reverse-connection — ONE hub per daemon process: satellite
        // control channels register into it (web WS handlers) and remote
        // spawns open exec dial-backs through it (gateway proxy). Built
        // before the gateway so it can be baked into both.
        let host_hub = std::sync::Arc::new(ccteam_harness::HostChannelHub::default());

        // v0.10.3 — ONE DSH runtime manager per daemon process, built here in
        // the composition root: "one identity, one `dsh web` process" is a
        // property of the SHARED instance, not a convention each consumer has
        // to honor. Handed to the web task below (which `configure`s it once
        // the bind is known); the DSH adapter takes the same Arc.
        let dsh_runtime = ccteam_web::dsh_web::new_runtime_manager(paths.root.clone());

        let (shared_gateway, shared_claude_stream_json, shared_pi_rpc) = if web.disabled
            || imd.disabled
        {
            (None, None, None)
        } else {
            match ccteam_im::build_gateway_for_daemon(None, std::sync::Arc::clone(&dsh_runtime)) {
                Ok((mut g, adapter, pi_rpc)) => {
                    g.set_remote_host_proxy(std::sync::Arc::new(
                        ccteam_im::remote_host::HubRemoteHostProxy::new(host_hub.clone()),
                    ));
                    (
                        Some(std::sync::Arc::new(tokio::sync::Mutex::new(g))),
                        Some(adapter),
                        Some(pi_rpc),
                    )
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "ccteam start: не удалось создать общий gateway; API web-сессий будет недоступен (503), демон создаст собственный"
                    );
                    (None, None, None)
                }
            }
        };

        let signal_task = tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            let _ = shutdown_tx.send(true);
        });

        // V0.8.4 P2b — shared gateway-event channel: the IM daemon
        // consumes it; the mcp.sock handler + `POST /mcp` clone the sender so
        // `chat_send_file` reuses the same outbound funnel. Only created
        // when IM is enabled (no consumer ⇒ nothing to deliver to).
        // Built BEFORE the web task so the web state factory can hand the
        // same pieces into `AppState::with_mcp` (v0.9 T4).
        let (gw_event_tx, gw_event_rx) = if imd.disabled {
            (None, None)
        } else {
            let (t, r) =
                tokio::sync::mpsc::unbounded_channel::<ccteam_im::gateway::GatewayEvent>();
            (Some(t), Some(r))
        };

        // v0.8.5 D6 — one shared pending-interaction registry handed to BOTH
        // the gateway (resolves inbound clicks) and the mcp.sock / `POST /mcp`
        // handlers (registers External-origin `interaction/ask` prompts from the
        // AskUserQuestion hook). The shared `Arc` is the bridge across the two
        // scopes. Only when IM is enabled (no gateway ⇒ no one to resolve).
        let pending_registry: Option<
            std::sync::Arc<tokio::sync::Mutex<ccteam_im::pending::PendingInteractions>>,
        > = if imd.disabled {
            None
        } else {
            Some(std::sync::Arc::new(tokio::sync::Mutex::new(
                ccteam_im::pending::PendingInteractions::new(),
            )))
        };

        let web_handle = if web.disabled {
            None
        } else {
            let opts = match parse_web_opts(&web) {
                Ok(o) => o,
                Err(err) => {
                    signal_task.abort();
                    return Err(err);
                }
            };
            let mut rx = shutdown_rx.clone();
            let web_bridge = web_chat_bridge
                .as_ref()
                .map(|bridge| {
                    (
                        bridge.inbound_tx.clone(),
                        bridge.outbound_tx.clone(),
                        bridge.backlog.clone(),
                        bridge.conns.clone(),
                    )
                });
            // V0.8.6 W5b — clone the shared gateway handle into the web state
            // factory so HTTP session endpoints drive the same session map the
            // daemon owns.
            let web_gateway = shared_gateway.clone();
            // The `(sid, secret)` registry travels beside the gateway handle,
            // taken here (async context) because `/mcp` must verify a managed
            // session's principal WITHOUT the gateway lock — see
            // `ccteam_im::principals`.
            let web_principals = match web_gateway.as_ref() {
                Some(gw) => Some(gw.lock().await.principals()),
                None => None,
            };
            // v0.9 T4 — same sink/pending the mcp.sock handler gets, so
            // `POST /mcp` can drive session_* / chat_send_file / HITL.
            let web_mcp_sink = gw_event_tx.clone();
            let web_mcp_pending = pending_registry.clone();
            let web_host_hub = host_hub.clone();
            let web_dsh_runtime = std::sync::Arc::clone(&dsh_runtime);
            Some(tokio::spawn(async move {
                ccteam_web::serve_with_state_factory_and_shutdown(
                    opts,
                    move |paths, auth| {
                        let mut state = ccteam_web::AppState::with_auth(paths, auth);
                        if let Some((inbound, outbound, backlog, conns)) = web_bridge {
                            state = state.with_chat_bridge(inbound, outbound, backlog, conns);
                        }
                        if let (Some(gw), Some(principals)) = (web_gateway, web_principals) {
                            state = state.with_gateway(gw, principals);
                        }
                        state = state.with_mcp(web_mcp_sink, web_mcp_pending);
                        state = state.with_host_hub(web_host_hub);
                        state
                    },
                    Some(web_dsh_runtime),
                    async move {
                        let _ = rx.changed().await;
                    },
                )
                .await
            }))
        };

        let imd_handle = if imd.disabled {
            tracing::info!("ccteam start: задан --no-imd; задача IM gateway пропущена");
            None
        } else {
            let mut rx = shutdown_rx.clone();
            let mut args = ccteam_im::DaemonArgs {
                gateway_event_tx: gw_event_tx.clone(),
                gateway_event_rx: gw_event_rx,
                pending: pending_registry.clone(),
                // V0.8.6 W5b — reuse the gateway the composition root built so
                // the daemon and the web `AppState` drive ONE session map. When
                // `None` (web off), the daemon builds + owns its own.
                gateway: shared_gateway.clone(),
                // v0.8.22 P0-2 — pairs with `gateway`: the stream-json Claude
                // adapter singleton `build_gateway_for_daemon` baked into it,
                // so the daemon can wire the production HITL resolver onto
                // the EXACT adapter this gateway spawns sessions through.
                claude_stream_json_adapter: shared_claude_stream_json.clone(),
                pi_rpc_adapter: shared_pi_rpc.clone(),
                // v0.10.3 — same reasoning for DSH: when the daemon has to
                // build its own gateway (web off), its DSH adapter must still
                // hold THIS process's one runtime manager.
                dsh_runtime: Some(std::sync::Arc::clone(&dsh_runtime)),
                ..Default::default()
            };
            if let Some(bridge) = web_chat_bridge.as_ref() {
                let mut channels = ccteam_im::daemon::ChannelMap::new();
                channels.insert("web".to_string(), bridge.channel.clone());
                args.extra_channels = Some(channels);
            }
            Some(tokio::spawn(async move {
                ccteam_im::run_daemon_with_shutdown(args, async move {
                    let _ = rx.changed().await;
                })
                .await
            }))
        };

        let mut rx = shutdown_rx.clone();
        let mcp_sink = gw_event_tx.clone();
        let mcp_pending = pending_registry.clone();
        // v0.8.7 W1 — hand the SAME shared gateway Arc to the mcp.sock handler
        // so the cto's `session_*` tools drive the daemon's session map
        // directly (cloning an Arc is cheap + acyclic; the daemon and web
        // already hold the same handle). `None` when web/IM is off → the
        // session tools then report "gateway not running".
        let mcp_gateway = shared_gateway.clone();
        let mcp_handle = tokio::spawn(async move {
            serve_mcp_socket(paths, mcp_sink, mcp_pending, mcp_gateway, async move {
                let _ = rx.changed().await;
            })
            .await
        });

        // v0.9.0 reverse-connection — if this machine is (or later becomes)
        // a satellite of another daemon (`host join` → self.json), keep an
        // outbound `ccteam-host.v1` control channel to it from THIS process.
        // No listener: the same `ccteam start` is main daemon and satellite.
        let satellite_handle = tokio::spawn(ccteam_web::satellite::run_satellite_client(
            hook_sink_paths.clone(),
            shutdown_rx.clone(),
        ));

            // V0.8 rmux W6 — flag-gated hook-sink listener (Option C).
            // ONLY when `CCTEAM_HOOK_VIA_DAEMON=1`: bind the ccteam-owned
            // `~/.ccteam/run/hook.sock`, consume each forwarded HookEvent,
            // and translate it to the same progress.jsonl line the legacy
            // `ccteam internal hook <kind> <action>` path would have
            // written — making the orchestrator process the SINGLE writer
            // and closing the two-writer race. When the flag is unset
            // (the default) we do NOT bind the sink: nothing changes, the
            // hook subprocess keeps writing progress.jsonl directly via
            // the `hook.sh chat-progress <arg>` path.
            //
            // The consumer awaits each dispatch (on the blocking pool, so
            // the file IO doesn't stall the runtime) before the next
            // recv, preserving append order — the single-writer invariant
            // the whole reroute exists to provide.
        let hook_sink_handle = if ccteam_core::hooks_dispatcher::hook_via_daemon_enabled() {
            let socket = ccteam_harness::default_ccteam_hook_socket_path();
            match ccteam_harness::HookSink::bind(&socket) {
                Ok(mut sink) => {
                    tracing::info!(
                        socket = %socket.display(),
                        "W6 hook-sink подключён (CCTEAM_HOOK_VIA_DAEMON=1): демон — единственный записывающий progress.jsonl"
                    );
                    let dispatch_paths = hook_sink_paths.clone();
                    let mut rx = shutdown_rx.clone();
                    Some(tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                _ = rx.changed() => break,
                                maybe = sink.recv() => {
                                    let Some(event) = maybe else { break };
                                    ccteam_harness::execution::typed_events::enrich_session_from_hook(&event);
                                    let dispatch_paths = dispatch_paths.clone();
                                    let res = tokio::task::spawn_blocking(move || {
                                        let stdin: serde_json::Value =
                                            serde_json::from_str(&event.payload_json)
                                                .unwrap_or(serde_json::Value::Null);
                                        ccteam_hooks::dispatch(
                                            &dispatch_paths,
                                            &event.kind,
                                            event.action.as_deref(),
                                            &stdin,
                                        )
                                    })
                                    .await;
                                    match res {
                                        Ok(Ok(_)) => {}
                                        Ok(Err(err)) => tracing::warn!(
                                            error = %err,
                                            "W6 hook-sink: отправка не удалась; событие отброшено"
                                        ),
                                        Err(je) => tracing::warn!(
                                            ?je,
                                            "W6 hook-sink: задача отправки аварийно завершилась"
                                        ),
                                    }
                                }
                            }
                        }
                        drop(sink);
                    }))
                }
                Err(err) => {
                    tracing::warn!(
                        socket = %socket.display(),
                        error = %err,
                        "W6 hook-sink не удалось привязать; chat-mode хуки не смогут маршрутизироваться (задавайте CCTEAM_HOOK_VIA_DAEMON только намеренно)"
                    );
                    None
                }
            }
        } else {
            None
        };

        let mut gateway_shutdown = shutdown_rx.clone();
        let _ = gateway_shutdown.changed().await;

        const TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

        if let Some(h) = web_handle {
            match tokio::time::timeout(TASK_DRAIN_TIMEOUT, h).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(err))) => tracing::warn!(?err, "ccteam web завершился с ошибкой"),
                Ok(Err(je)) if je.is_cancelled() => {}
                Ok(Err(je)) => tracing::warn!(?je, "задача ccteam web аварийно завершилась"),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                        "ожидание завершения ccteam web истекло; прерываю (ОС освободит порт)"
                    );
                }
            }
        }
        if let Some(h) = imd_handle {
            match tokio::time::timeout(TASK_DRAIN_TIMEOUT, h).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(err))) => tracing::warn!(?err, "ccteam-im завершился с ошибкой"),
                Ok(Err(je)) if je.is_cancelled() => {}
                Ok(Err(je)) => tracing::warn!(?je, "задача ccteam-im аварийно завершилась"),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                        "ожидание завершения ccteam-im истекло; прерываю"
                    );
                }
            }
        }
        match tokio::time::timeout(TASK_DRAIN_TIMEOUT, mcp_handle).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(err))) => tracing::warn!(?err, "сокет MCP ccteam завершился с ошибкой"),
            Ok(Err(je)) if je.is_cancelled() => {}
            Ok(Err(je)) => tracing::warn!(?je, "задача сокета MCP ccteam аварийно завершилась"),
            Err(_) => {
                tracing::warn!(
                    timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                    "ожидание завершения сокета MCP ccteam истекло; прерываю"
                );
            }
        }
        if let Some(h) = hook_sink_handle {
            match tokio::time::timeout(TASK_DRAIN_TIMEOUT, h).await {
                Ok(Ok(())) => {}
                Ok(Err(je)) if je.is_cancelled() => {}
                Ok(Err(je)) => tracing::warn!(?je, "задача W6 hook-sink аварийно завершилась"),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                        "ожидание завершения W6 hook-sink истекло; прерываю"
                    );
                }
            }
        }
        match tokio::time::timeout(TASK_DRAIN_TIMEOUT, satellite_handle).await {
            Ok(Ok(())) => {}
            Ok(Err(je)) if je.is_cancelled() => {}
            Ok(Err(je)) => tracing::warn!(?je, "задача встроенного satellite аварийно завершилась"),
            Err(_) => {
                tracing::warn!(
                    timeout_secs = TASK_DRAIN_TIMEOUT.as_secs(),
                    "ожидание завершения встроенного satellite истекло; прерываю"
                );
            }
        }
        signal_task.abort();

        tracing::info!(
            "graceful shutdown complete; agent session bodies were let go, not killed — idle ones \
             exit on their own, a mid-turn one finishes its turn; the next `ccteam start` finds \
             any survivor by its body record (one sid, one body), waits for it, and recovers \
             what it said"
        );

        Ok(())
    });
    // Keep runtime teardown bounded even if a blocking hook dispatch is
    // mid-flight during shutdown.
    runtime.shutdown_timeout(Duration::from_secs(5));
    result
}

#[cfg(unix)]
async fn serve_mcp_socket<F>(
    paths: CcteamPaths,
    sink: Option<ccteam_im::mcp::GatewayEventSink>,
    pending: Option<ccteam_im::mcp::PendingRegistry>,
    gateway: Option<ccteam_im::mcp::GatewayHandle>,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let socket = ccteam_core::daemon_socket_path(&paths);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("создать каталог сокета MCP {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&socket);
    let listener = tokio::net::UnixListener::bind(&socket)
        .with_context(|| format!("привязать сокет MCP {}", socket.display()))?;
    tracing::info!(socket = %socket.display(), "сокет MCP ccteam ожидает подключений");

    let mut shutdown = Box::pin(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                let _ = std::fs::remove_file(&socket);
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, _addr) = accepted
                    .with_context(|| format!("принять подключение сокета MCP {}", socket.display()))?;
                let paths = paths.clone();
                let sink = sink.clone();
                let pending = pending.clone();
                let gateway = gateway.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        handle_mcp_socket_connection(paths, sink, pending, gateway, stream).await
                    {
                        tracing::warn!(error = %err, "соединение с сокетом MCP не удалось");
                    }
                });
            }
        }
    }
}

#[cfg(not(unix))]
async fn serve_mcp_socket<F>(
    _paths: CcteamPaths,
    _sink: Option<ccteam_im::mcp::GatewayEventSink>,
    _pending: Option<ccteam_im::mcp::PendingRegistry>,
    _gateway: Option<ccteam_im::mcp::GatewayHandle>,
    shutdown: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    shutdown.await;
    Ok(())
}

#[cfg(unix)]
async fn handle_mcp_socket_connection(
    paths: CcteamPaths,
    sink: Option<ccteam_im::mcp::GatewayEventSink>,
    pending: Option<ccteam_im::mcp::PendingRegistry>,
    gateway: Option<ccteam_im::mcp::GatewayHandle>,
    stream: tokio::net::UnixStream,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).lines();
    let dispatch = ccteam_im::mcp::McpDispatch {
        paths,
        sink,
        pending,
        gateway,
    };
    while let Some(line) = lines
        .next_line()
        .await
        .context("прочитать строку сокета MCP")?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {err}") },
                });
                let mut out = serde_json::to_string(&response)?;
                out.push('\n');
                writer.write_all(out.as_bytes()).await?;
                writer.flush().await?;
                continue;
            }
        };
        if let Some(response) = dispatch.dispatch(req).await {
            let mut out = serde_json::to_string(&response)?;
            out.push('\n');
            writer.write_all(out.as_bytes()).await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

async fn wait_for_shutdown_signal() {
    // v0.9.7 — SIGTERM (`ccteam daemon stop` / `ccteam stop`) and Ctrl-C
    // are the only shutdown signals; the historical trigger-file poll is
    // retired with the trigger channel.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(?err, "не удалось установить обработчик SIGTERM");
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("получен ctrl+c");
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("получен ctrl+c"),
            _ = sigterm.recv() => tracing::info!("получен SIGTERM (`ccteam stop`)"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("получен ctrl+c");
    }
}

fn print_web_banner(paths: &CcteamPaths, web: &StartWebOpts) {
    let bind: std::net::SocketAddr = match web.bind.parse() {
        Ok(a) => a,
        Err(_) => {
            // Defer the error to parse_web_opts inside the async block;
            // banner is best-effort.
            return;
        }
    };
    let loopback = ccteam_web::auth::is_loopback(&bind);
    let scheme = "http";
    // When bound to the unspecified address (0.0.0.0), substitute the
    // host's hostname in the displayed URL so the line is clickable;
    // 0.0.0.0:port isn't a real destination. Fall back to the literal
    // socket addr if gethostname() fails.
    let display_url_host = if bind.ip().is_unspecified() {
        read_hostname().unwrap_or_else(|| bind.ip().to_string())
    } else {
        bind.ip().to_string()
    };
    let display_url = format!("{scheme}://{display_url_host}:{}", bind.port());
    eprintln!();
    eprintln!("┌─ ccteam web ─────────────────────────────────────────────");
    if loopback || web.no_auth {
        eprintln!("│  АДРЕС: {display_url}/");
        if !loopback && web.no_auth {
            eprintln!(
                "│  АВТОРИЗАЦИЯ: ОТКЛЮЧЕНА (--web-no-auth для не-loopback — доступна всей LAN!)"
            );
        } else {
            eprintln!("│  АВТОРИЗАЦИЯ: отключена (привязка loopback)");
        }
    } else {
        let token_path = web
            .token_file
            .clone()
            .unwrap_or_else(|| ccteam_web::token::default_token_path(paths));
        match ccteam_web::token::generate_or_load_token(&token_path) {
            Ok(hex) => {
                eprintln!("│  АДРЕС: {display_url}/?token=ccteam:{hex}");
                // V0.4.6 F88 — auto-copy the token (full `ccteam:<hex>`
                // form so it round-trips into a curl `-H` or a
                // browser URL bar) unless --no-clipboard.
                let token_str = format!("ccteam:{hex}");
                let suffix = if web.no_clipboard {
                    String::new()
                } else {
                    match clipboard::copy_to_clipboard(&token_str) {
                        Some(provider) => {
                            format!("  (скопировано в буфер обмена через {provider})")
                        }
                        None => "  (буфер обмена недоступен; скопируйте вручную)".to_string(),
                    }
                };
                eprintln!("│  ТОКЕН: {token_str}{suffix}");
                eprintln!("│  ФАЙЛ:  {}", token_path.display());
            }
            Err(err) => {
                eprintln!("│  АДРЕС: {display_url}/  (не удалось инициализировать токен: {err})");
            }
        }
    }
    eprintln!("│  ПРИВЯЗКА: {bind}");
    eprintln!("└──────────────────────────────────────────────────────────");
    eprintln!();
}

/// Read the OS hostname via libc `gethostname`. Returns `None` on
/// syscall failure or a non-UTF8 result.
#[cfg(unix)]
fn read_hostname() -> Option<String> {
    use std::ffi::CStr;
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `len-1` bytes and NUL-terminates.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) };
    if rc != 0 {
        return None;
    }
    // Ensure NUL-termination before scanning the buffer.
    buf[buf.len() - 1] = 0;
    let cstr = CStr::from_bytes_until_nul(&buf).ok()?;
    let s = cstr.to_str().ok()?.to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
#[cfg(not(unix))]
fn read_hostname() -> Option<String> {
    None
}

fn parse_web_opts(web: &StartWebOpts) -> Result<ccteam_web::ServeOpts> {
    let bind: std::net::SocketAddr = web.bind.parse().with_context(|| {
        format!(
            "--web-bind {} не является корректным адресом сокета",
            web.bind
        )
    })?;
    let dsh_web_bind = parse_dsh_web_bind(&bind, web.dsh_bind.as_deref())?;
    Ok(ccteam_web::ServeOpts {
        bind,
        no_auth: web.no_auth,
        token_file: web.token_file.clone(),
        dsh_web_bind,
        no_auth_grace_secs: Some(5),
    })
}

fn parse_dsh_web_bind(
    web_bind: &std::net::SocketAddr,
    raw: Option<&str>,
) -> Result<Option<std::net::SocketAddr>> {
    match raw {
        Some(value) if value.eq_ignore_ascii_case("off") => Ok(None),
        Some(value) => value.parse().map(Some).with_context(|| {
            format!("--dsh-web-bind {value} не является корректным адресом сокета или `off`")
        }),
        None => {
            let port = web_bind.port().checked_add(1).context(
                "значение --dsh-web-bind по умолчанию нельзя вывести: --web-bind использует порт 65535",
            )?;
            Ok(Some(std::net::SocketAddr::new(web_bind.ip(), port)))
        }
    }
}

fn run_status() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    use ccteam_core::check_daemon_health;

    println!("статус ccteam");
    println!();

    // ① Daemon health.
    let health = check_daemon_health(&paths);
    let daemon_up = health.is_healthy();
    println!("  демон:   {}", health.describe());
    println!();

    // ①b v0.9.7 (F3.4) — lazy "a newer ccteam is available" line. The
    // ≥20h-gated refresh uses the injected GitHub-latest fetcher (stubbed
    // until the orchestrator wires it; a failure degrades silently to the
    // cache), and display reads the (possibly refreshed) cache. Gated by
    // the `check_for_update` preference.
    let binary_version = env!("CARGO_PKG_VERSION");
    let prefs = ccteam_core::preferences::load_or_default(&paths.root);
    let version_cache = ccteam_core::version_check::maybe_refresh_latest(
        &paths,
        prefs.check_for_update,
        chrono::Utc::now(),
        update::fetch_latest_version,
    );
    if let Some(latest) =
        ccteam_core::version_check::update_available(&version_cache, binary_version)
    {
        println!(
            "  обновление: доступна новая версия ccteam ({binary_version} → {latest}); выполните `ccteam update`"
        );
        println!();
    }

    // ①c v0.9.7 (F3.6) — fleet version skew: one line per registered
    // satellite whose ccteam version differs from this daemon's. The
    // common no-satellite / all-aligned case stays silent.
    let host_skew = update::fleet_version_skew(&paths, binary_version);
    if !host_skew.is_empty() {
        println!("  хосты (с расхождением версий: {}):", host_skew.len());
        for line in &host_skew {
            println!("    {line}");
        }
        println!();
    }

    // ② Projects, each with its tracked sessions nested underneath.
    //
    // v0.8.8 F3 — the old flat "projects" / "running sessions" / "recent
    // events" trio is collapsed into one tree: a project row (slug · age ·
    // last-event · STUCK/OK verdict) followed by its sessions, grouped from
    // the daemon's persisted records via `tracked_chat_sessions` (same
    // out-of-process source `session ls` uses, so the two never drift).
    let projects = ccteam_core::queries::collect_projects(&paths).unwrap_or_default();

    // Group tracked sessions by project slug. A missing / unreadable registry
    // is non-fatal — just an empty map (no sessions shown).
    let tracked = ccteam_im::gateway::tracked_chat_sessions(&paths.root).unwrap_or_default();
    let mut sessions_by_project: std::collections::BTreeMap<
        String,
        Vec<ccteam_im::gateway::TrackedSessionRow>,
    > = std::collections::BTreeMap::new();
    for row in tracked {
        sessions_by_project
            .entry(row.project.clone())
            .or_default()
            .push(row);
    }

    if projects.is_empty() {
        println!("  проекты: (нет — создайте через `ccteam project new <slug>`, например `ccteam project new demo`)");
    } else {
        println!("  проекты ({}):", projects.len());
        // Session activity comes from the same file-backed progress truth the
        // JSON status and web Status rail read. Aggregation here follows the
        // resume-by-sid architecture: an idle session silent for days is the
        // NORMAL resting state, never an alarm. Only a `stuck` session (the
        // watchdog wrote a timeout event) or a `stale` one (turn started,
        // no boundary, long silence) escalates the project verdict or earns
        // a check-in hint.
        let mut needs_attention: Vec<(String, String, &'static str, String)> = Vec::new();
        for p in &projects {
            let age = humanize_secs(p.age_seconds);
            let events =
                ccteam_core::progress::read_all_events(&paths.progress_jsonl(&p.state.slug))
                    .unwrap_or_default();
            let now = chrono::Utc::now();
            // `last-event` = the project's most recent progress event, any
            // session (what the label naturally means to a reader).
            let last_event_h = events
                .last()
                .and_then(|e| ccteam_core::stall::progress_event_age_seconds(e, now))
                .map(humanize_secs)
                .unwrap_or_else(|| "-".to_string());

            let mut verdict = "OK";
            let mut session_lines: Vec<(u64, String, String, String, String, String)> = Vec::new();
            if let Some(rows) = sessions_by_project.get(&p.state.slug) {
                for s in rows {
                    let activity = ccteam_core::stall::classify_progress_activity_for_sid(
                        &events,
                        &s.sid,
                        p.stall_silent_seconds,
                        now,
                    );
                    let act = activity.status.activity;
                    if act == "stuck" || act == "stale" {
                        let silent = humanize_secs(
                            activity.event_age_seconds.unwrap_or(p.stall_silent_seconds),
                        );
                        needs_attention.push((p.state.slug.clone(), s.sid.clone(), act, silent));
                        if act == "stuck" {
                            verdict = "STUCK";
                        } else if verdict == "OK" {
                            verdict = "warn";
                        }
                    }
                    let role = if s.role.is_empty() { "-" } else { &s.role };
                    let session_status = if daemon_up {
                        act.to_string()
                    } else {
                        "зарегистрирована (демон недоступен)".to_string()
                    };
                    let last_event = activity
                        .event_age_seconds
                        .map(humanize_secs)
                        .unwrap_or_else(|| "-".to_string());
                    session_lines.push((
                        activity.event_age_seconds.unwrap_or(u64::MAX),
                        role.to_string(),
                        s.vendor.clone(),
                        session_status,
                        s.sid.clone(),
                        last_event,
                    ));
                }
            }
            // Most recently active first; never-seen sessions sink to the end.
            session_lines.sort_by_key(|l| l.0);

            println!(
                "    {:<32}  возраст {:>8}  последнее-событие {:>8}  {}",
                p.state.slug, age, last_event_h, verdict
            );

            for (_, role, vendor, session_status, sid, last_event) in session_lines {
                println!(
                    "        {:<10}  {:<7}  {:<26}  {:<6}  последнее-событие {}",
                    role, vendor, session_status, sid, last_event
                );
            }
        }
        // An `attention:` section appears only when a session is stuck or
        // stale — one terse line each (idle projects and sessions stay quiet).
        if !needs_attention.is_empty() {
            println!();
            println!("  требует внимания:");
            for (slug, sid, act, silent) in &needs_attention {
                let hint = commands::stall_takeover_hint_for_session(slug, sid, act, silent);
                println!("    {hint}");
            }
        }
    }
    println!();

    // ④ Web token + URL (two lines).
    //
    // - `web token:` prints the BARE hex (no `ccteam:` prefix) so it can be
    //   copy-pasted into tools that add their own prefixing.
    // - `web url:` embeds the token WITH the `ccteam:` prefix (the form the web
    //   console expects in the query string) at the LAN IP + port 7331.
    let token_path = ccteam_web::token::default_token_path(&paths);
    if token_path.exists() {
        match ccteam_web::token::load_existing(&token_path) {
            Ok(hex) => {
                println!("  web-токен: {hex}");
                match first_lan_ipv4() {
                    Some(ip) => {
                        println!("  web-адрес: http://{ip}:7331/?token=ccteam:{hex}")
                    }
                    None => println!("  web-адрес: <LAN IP не найден> (используйте http://localhost:7331/?token=ccteam:{hex})"),
                }
            }
            Err(_) => println!("  web-токен: <не читается> ({})", token_path.display()),
        }
    } else {
        println!(
            "  web-токен: <пока нет — будет создан при первом не-loopback `ccteam start`> ({})",
            token_path.display()
        );
    }

    // ⑤ v0.8.20 F3 — per-user (tenant) login links. `ccteam status` runs on the
    //    box = the admin/operator context, so it surfaces every tenant's personal
    //    `?token=ccteam:<hex>` link (the value the admin hands out / re-sends).
    //    Tenants never see each other's; this local admin view is the CLI peer of
    //    the web user-management "copy link" action (GET /api/v1/users/{id}/link).
    let tenants = ccteam_core::tenants::TenantRegistry::load(&paths.users_dir());
    if !tenants.list().is_empty() {
        println!();
        println!("  web-пользователи ({}):", tenants.list().len());
        let host = first_lan_ipv4()
            .map(|ip| format!("http://{ip}:7331"))
            .unwrap_or_else(|| "http://localhost:7331".to_string());
        for t in tenants.list() {
            println!("    {:<14}  {host}/?token=ccteam:{}", t.handle, t.web_token);
        }
    }
    Ok(())
}

/// Predicate for a LAN-reachable IPv4: a private
/// (RFC1918) address that is neither loopback (`127.0.0.0/8`) nor
/// link-local (`169.254.0.0/16`). Pure so the interface-walk in
/// [`first_lan_ipv4`] can be unit-tested without real interfaces.
fn is_lan_ipv4(ip: &std::net::Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && ip.is_private()
}

/// First non-loopback, non-link-local, private IPv4 of any local
/// interface, for the `ccteam status` web URL line (so the operator gets a
/// LAN-reachable address, not `127.0.0.1`). Returns `None` when no private
/// IPv4 is configured (the URL line then degrades to a localhost hint).
///
/// Uses `getifaddrs(3)` directly — `ccteam-cli` already depends on `libc`
/// (zero new deps). The unsafe FFI is fully contained here: it walks the
/// linked list, reads only `AF_INET` entries, converts the network-order
/// `s_addr` to host order for `Ipv4Addr`, and always `freeifaddrs`-frees the
/// list before returning.
#[cfg(unix)]
fn first_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    use std::net::Ipv4Addr;

    // SAFETY: `getifaddrs` writes a heap-allocated linked list into `ifap` on
    // success (returns 0) which we must `freeifaddrs`. We only read fields
    // through valid non-null pointers and free exactly once before returning.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut found: Option<Ipv4Addr> = None;
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            let addr = ifa.ifa_addr;
            if !addr.is_null() && (*addr).sa_family as i32 == libc::AF_INET {
                // The generic `sockaddr` for an AF_INET entry is really a
                // `sockaddr_in`; read its network-order `s_addr`.
                let sin = &*(addr as *const libc::sockaddr_in);
                let be = sin.sin_addr.s_addr; // network byte order
                let ip = Ipv4Addr::from(u32::from_be(be));
                if is_lan_ipv4(&ip) {
                    found = Some(ip);
                    break;
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        found
    }
}

#[cfg(not(unix))]
fn first_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    None
}

fn humanize_secs(s: u64) -> String {
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

fn show_slug_picker() -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let projects = ccteam_core::queries::collect_projects(&paths).unwrap_or_default();
    if projects.is_empty() {
        println!("проектов пока нет — создайте через `ccteam project new <slug>`, например `ccteam project new demo`.");
        return Ok(());
    }
    println!("`ccteam show` требует slug. Доступны:");
    for p in &projects {
        println!("  {}", p.state.slug);
    }
    println!();
    println!("повторите как `ccteam show <slug>`.");
    Ok(())
}

fn run_new(slug: String) -> Result<()> {
    // Fail loud at the CLI boundary on invalid slug grammar (whitespace /
    // unicode / leading dash etc.) so we don't spawn `~/projects/<garbage>/`
    // and leave junk for the user to clean up.
    let base = ccteam_core::validate_slug_format(&slug)
        .with_context(|| format!("ccteam project new {slug:?}"))?;
    let paths = CcteamPaths::from_env()?;
    // `ccteam project new <slug>` installs under `<projects_root>/<slug>/`:
    // the slug is the directory name verbatim, with a numeric suffix on
    // collision (demo / demo2 / demo3) and NO team prefix — the same
    // `pick_unused_project_slug` helper the IM `/newproject` + web create
    // paths use, so all three entry points behave identically.
    let final_slug = ccteam_core::pick_unused_project_slug(&paths.root, &base)?;
    let target = paths.projects_root.join(&final_slug);
    let report = commands::run_init(
        &paths,
        commands::InitOptions {
            install_in: Some(target),
            slug: Some(final_slug),
            ..commands::InitOptions::default()
        },
    )?;
    print!("{report}");
    Ok(())
}

fn run_ls(format: OutputFormat) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_ls(&paths, format)?;
    print!("{body}");
    Ok(())
}

fn run_show(slug: &str, format: OutputFormat) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_show(&paths, slug, format)?;
    print!("{body}");
    Ok(())
}

fn run_internal_peek(slug: &str, sid: Option<&str>) -> Result<()> {
    let paths = CcteamPaths::from_env()?;
    let body = commands::run_peek_with_role(&paths, slug, sid)?;
    print!("{body}");
    Ok(())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ccteam_core=info")),
        )
        .try_init();
}

#[cfg(test)]
mod lan_ip_tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// The LAN predicate accepts only private addresses and rejects
    /// loopback / link-local (192.168 / 127.0.0.1 / 169.254 tiers).
    #[test]
    fn is_lan_ipv4_filters_loopback_and_linklocal() {
        // Private (RFC1918) → accepted.
        assert!(is_lan_ipv4(&Ipv4Addr::new(192, 168, 1, 5)));
        assert!(is_lan_ipv4(&Ipv4Addr::new(10, 0, 0, 7)));
        assert!(is_lan_ipv4(&Ipv4Addr::new(172, 16, 3, 9)));
        // Loopback / link-local / public → rejected.
        assert!(!is_lan_ipv4(&Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_lan_ipv4(&Ipv4Addr::new(169, 254, 10, 10)));
        assert!(!is_lan_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
    }

    /// Querying real interfaces must never panic, and any address it
    /// returns must satisfy the LAN predicate (no loopback / link-local /
    /// public leaking into the web URL).
    #[test]
    fn first_lan_ipv4_does_not_panic_and_is_lan() {
        if let Some(ip) = first_lan_ipv4() {
            assert!(is_lan_ipv4(&ip), "returned non-LAN ip: {ip}");
        }
    }
}

#[cfg(test)]
#[test]
fn cli_help_is_russian_but_flags_remain_stable() {
    use clap::CommandFactory;
    let help = Cli::command().render_help().to_string();
    assert!(help.contains("Командный мост"));
    assert!(help.contains("--home"));
    assert!(help.contains("init"));
}

#[cfg(test)]
mod daemon_runtime_tests {
    use super::*;

    #[test]
    fn daemon_runtime_is_multi_thread_with_requested_worker_count() {
        let runtime = build_daemon_runtime(3).unwrap();
        assert_eq!(
            runtime.handle().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        );
        assert_eq!(runtime.metrics().num_workers(), 3);
    }
}
