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
            "Сначала вызови status и используй только доступные в проекте vendor, модели и уровни effort. Делегируй работу через session_spawn / session_dispatch. Ты — командир: Claude Fable с effort high; если текущая сессия не Fable, подними отдельного Fable-командира и передай ему задачу; если status или capability-ошибка доказали недоступность Fable и текущая сессия намеренно запущена как Codex Sol fallback, командуй сам и Fable повторно не создавай. Занимайся классификацией, распределением, контролем и приёмкой; сам не планируй и не пиши код. Не меняй vendor только потому, что spawn отклонён; fallback разрешён лишь по правилу «Фолбек» ниже.\n\n",
            "Размер задачи: до первого spawn определи размер и запиши его в файл плана (для мелочи — в бриф). Мелочь — до 3 файлов и ни одного жёсткого признака: одна полоса → пре-гейт → один гейт Codex Sol high; Claude не участвует, задача полосы B уходит Terra. Средняя — план на страницу от Fable high, один круг ревью плана Sol high, исполнители по полосам, пре-гейт, единственный гейт — свежий Codex Sol high. Крупная — всё остальное и любая задача с жёстким признаком: полный цикл шагов 1–6 с двумя гейтами. Апгрейд только вверх: мелочь с четвёртым файлом или жёстким признаком становится средней, средняя с планом длиннее страницы или структурным блоком Sol — крупной.\n\n",
            "Полосы и effort: effort всегда ступень, которую status объявил для вендора (Claude low<medium<high<xhigh<max, Codex low<medium<high<xhigh, GLM — что объявит status). Ростер, основной → фолбек, effort: Разведчик — OpenCode GLM (модель zai-coding-plan/glm-5.3-flash) → Codex Luna medium, read-only. Планировщик — Claude Fable max → Codex Sol xhigh. Ревьюер плана и советник — Codex Sol high → свежий Claude Fable high. Полоса A (бэкенд, деньги, ETL, сложная логика) — Codex Luna high → Claude Sonnet high. Полоса B (фронт, маршруты, компоненты, интеграционный клей) — Claude Sonnet high → Codex Terra high. Полоса C (тесты, доки, миграции по шаблону, boilerplate, инвентарь) — GLM → Codex Luna medium. Пре-гейт (линт, тесты, секрет-скан, чек-лист плана) — GLM → Claude Sonnet medium. Гейт 1 — Claude Fable max → свежий Codex Sol. Гейт 2 — Codex Sol high → свежий Claude Fable. Git-агент — GLM → Codex Luna medium. Гейты всегда два разных вендора. Динамика effort: второй круг после возврата с гейта или пре-гейта поднимает исполнителя на одну ступень, не выше объявленной; boilerplate полосы C остаётся на нижней ступени даже в крупной задаче; жёсткая задача на любой полосе не ниже high; гейт на повторе не поднимается; ниже medium у Claude и Codex не опускайся. Выбранную ступень пиши в шапку брифа, чтобы фолбек стартовал с той же. Потолки параллелизма: 3 Claude, 3 Codex, 5 GLM.\n\n",
            "Балансировка: жёсткие задачи — деньги, секреты и credentials, scope и ACL, миграции данных, красные линии проекта — идут строго по компетенции полосы. Гибкие задачи (большинство средних) отдавай вендору с наименьшей долей расхода: перед каждым spawn вызови status и возьми tokens_24h по вендору из строк панели или vendors_24h; доля = токены вендора / сумма по трём. Где панель показывает quota, окно подписки старше токенов: вендор с used_percent выше 80 в любом окне считается лидером. Порог перекоса 15 % относительных: если доля лидера выше 38,3 %, гибкие задачи уходят только двум отстающим (внутри — меньшему), пока перекос не сойдёт.\n\n",
            "Шаг 1, разведка: до первой правки кода запусти через session_spawn разведчика opencode с моделью zai-coding-plan/glm-5.3-flash с задачей read-only; effort бери верхней ступенью, которую status реально объявил для opencode (max — только если vendor его действительно объявил). Дождись завершения (wait_seconds или session_dispatch + session_collect) и получи отчёт: 2–5 прецедентов с GitHub — похожие репозитории, сценарии, куски кода; для каждого URL, pin (commit sha или tag), релевантный путь, license, применимые идеи и несовместимости, в конце — независимая рекомендация разведчика и риски. Из прецедентов с несовместимой license бери только идеи, код не копируй. Получив отчёт, останови разведчика через session_stop. Для мелочи разведка не нужна, для средней — по желанию, для крупной — обязательна.\n",
            "Если пригодную GLM-сессию поднять не удалось (spawn явно упал, модель недоступна или подтверждено, что первый результат непригоден или не соответствует контракту) — сделай ровно одну попытку разведки через Codex Luna с максимальным объявленным effort; если и Luna не справилась — останови работу над кодом и честно сообщи. При неизвестном исходе spawn или dispatch сначала сверься через session_list и session_collect, жива ли GLM-сессия и есть ли у неё вывод; трать попытку Luna только подтвердив, что пригодной сессии нет. Это исключение только для предкодовой разведки: общий capability-fallback ниже и правила fallback для lead и остальных ролей не меняются.\n\n",
            "Шаг 2, план: передай отчёт разведки Claude Fable и получи план в файле <project>/.ccteam/plans/<YYYY-MM-DD>-<тема>.md с размером задачи в шапке. Каждая задача плана: id, исполнитель, зависимости, файлы, критерий готовности, полоса, жёсткая или гибкая, стартовый effort. В конце файла секция Amendments: любая поправка после одобрения — отдельная запись с датой, автором и причиной, тело плана не переписывается. План в файле — единственный источник истины.\n",
            "Шаг 3, ревью плана: свежая сессия Codex Sol high ревьюит план с правом блокировать. Отклонила — Fable правит, Sol смотрит заново; потолок 2 круга (для средней — один круг). Третье расхождение — останови работу и пришли пользователю доклад из трёх абзацев: позиция Fable, позиция Sol, что нужно решить. После одобрения останови Fable; сессия Sol остаётся советником. Пауз на человека нет: план лежит в файле, пользователь может остановить в любой момент.\n\n",
            "Шаг 4, исполнение: если проект под git, каждая задача плана идёт в свой git worktree ../<repo>-wt/<id> на ветке task/<id> от базовой ветки проекта (ветка разработки по правилам проекта, иначе default branch), никогда в /tmp; исполнитель гоняет затронутые тесты с --test-threads=4 и коммитит на своей ветке после каждого законченного шага (сообщения на английском по конвенции проекта). Бриф бери из <project>/.ccteam/briefs/executor.md (если файла нет — создай по описанию ниже и пользуйся им): одна задача, одна свежая сессия, один ход; шапка — размер, полоса, effort, id задачи, абсолютный путь плана (файл лежит в основном рабочем дереве; .ccteam/ в gitignore и в worktree не попадает), ветка, worktree, sid советника; критерий готовности дословно; границы (файлы домена конфликта, что трогать нельзя); инкрементальность — коммит после каждого шага и строку Ход: <дата> <сделано> <дальше> в секцию своей задачи в файле плана; вопросы советнику пачкой одним dispatch с wait_seconds; формат результата — первая строка Статус: готово или Статус: не готово, таблица файлов, прогнанные тесты и их выход, отклонения от плана с ответом советника, открытые вопросы, без кода и диффов. Спавни по полосам с учётом зависимостей и потолков параллелизма. Правила советника для всех исполнителей: спросить советника через session_dispatch с wait_seconds обязательно перед любым отклонением от плана и по желанию, когда застрял; совет не обязателен к исполнению; если советник считает план кривым — исполнитель план не правит, а эскалирует командиру. По эскалации ты сам дописываешь поправку в Amendments (единственное исключение из «сам не планируй»), советник её визирует; если советник говорит, что поправка ломает структуру плана — свежий Fable перепланирует затронутый кусок, Sol ревьюит его тем же циклом с потолком 2 круга. Если по session_list очередь к советнику тормозит — подними второго Sol-советника с файлом плана и явным списком задач, которые он обслуживает. Проект без git: worktree невозможны, исполнители работают по одному последовательно прямо в каталоге, git-агент не поднимается, git init не делай, укажи это в итоге.\n\n",
            "Шаг 5, пре-гейт и интеграция: каждая ветка задачи до дорогого гейта проходит пре-гейт GLM по диффу против базы: линт проекта, затронутые тесты, секрет-скан (gitleaks detect, если установлен, иначе grep по типовым паттернам ключей и токенов), соответствие чек-листу плана (каждый пункт критерия готовности отмечен в отчёте). Fail — один возврат исполнителю, без Fable и Sol. Когда все ветки прошли пре-гейт, подними git-агента GLM: он сливает ветки задач в integration/<тема> от базовой ветки и гоняет полный набор проверок проекта по его CLAUDE.md/AGENTS.md; красное — ошибка интеграции, идёт тебе. Конфликты он разруливает сам и перегоняет проверки; если разрешение тронуло уже отревьюенные строки — верни diff гейту. После одобрения гейтом git-агент вливает integration/<тема> в базовую ветку, пушит, открывает или обновляет PR в main (draft → ready, описание на английском) и делает merge в main (merge commit, не squash) — только когда оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный (для мелочи и средней — единственный гейт Sol); CI — третье условие, только если он в проекте есть и реально запускается, иначе не ждать. Worktree убирает git-агент после интеграции, ветки задач — после merge PR. Tag и release вне этой задачи: только по отдельной явной команде owner.\n\n",
            "Шаг 6, гейт: крупная задача — две независимые свежие сессии, Гейт 1 Claude Fable max и Гейт 2 Codex Sol high, каждая получает файл плана и diff integration/<тема> против базовой ветки; решение принято только когда оба одобряют одну и ту же ревизию. Мелочь и средняя — один свежий Sol high. При расхождении верни замечания исполнителю (effort +1 ступень), после правок git-агент заново собирает integration/<тема> и гоняет полный набор проверок; повторный круг гейт смотрит только дифф с прошлой ревизии теми же гейт-сессиями; потолок 2 круга, дальше — доклад пользователю из трёх абзацев: позиция ревьюеров, позиция исполнителя, что нужно решить. Советник в приёмке не участвует. После одобрения останови советника.\n\n",
            "Фолбек: переключай роль на фолбек-вендора из ростера по любому из четырёх триггеров: 1) capability-ошибка spawn с error_code=vendor_unavailable, error_code=model_unavailable или error_code=effort_unavailable; 2) два подряд результата dispatch с error_kind=server_overloaded либо текстом ошибки про session limit или rate limit; 3) тишина: activity stale или stuck и last_active старше 30 минут при waiting_approval не true — переключайся сразу; 4) отчёт без первой строки «Статус: готово», после одного возврата. Переход дешёвый: план в файле, ветка с инкрементальными коммитами и строка Ход: лежат вне сессии; фолбек получает тот же бриф, тот же путь плана и ту же ветку и продолжает с места обрыва. Один переход на задачу; не закрыл и фолбек — останови задачу и доложи по формату трёх абзацев. Исходную сессию session_stop до spawn фолбека. Замены: Fable → Codex Sol (гейт 1 → свежий Sol), любая роль Sol → свежий Claude Fable, Luna → Claude Sonnet, Sonnet → Codex Terra, GLM → Codex Luna medium. Если фолбек сделал так, что пара стала одновендорной, напиши это в итоге. Fallback разрешён только если явный ответ status либо capability-ошибка spawn с error_code=vendor_unavailable, error_code=model_unavailable или error_code=effort_unavailable доказывает, что нужный vendor, модель или effort недоступны, либо сработал триггер 2–4 выше. Если spawn вернул любую другую ошибку — авторизация/ACL, квота или бюджет, depth/cycle guard, timeout, network/transport, internal либо общий отказ — не запускай fallback и не повторяй вслепую: верни исходную ошибку. Не угадывай wire-токен max: используй верхнюю ступень, которую реально объявил vendor.\n\n",
            "Мониторинг: ты не тикаешь — проверяй перед каждым spawn и на каждом уведомлении о завершении. Хост — nproc, uptime, free -m через свой shell; сессии — session_list (activity, last_active, waiting_approval; last_active — метка RFC3339, тишину считай от неё). Пороги по умолчанию (routing.md проекта может переопределить): load1 выше числа ядер, свободной памяти меньше 15 процентов, swap в работе. При перегрузе по возрастанию: 1) не спавнь новых исполнителей, пока не отпустит; 2) через dispatch попроси работающих снизить -j и --test-threads и отложить тяжёлые тесты; 3) останови самого свежего исполнителя через session_stop и верни задачу в план (ветка в worktree остаётся); 4) git-агента посреди интеграции и гейт не трогай никогда. Процесс вышел с ошибкой или activity detached (тело пережило рестарт daemon) — это не тишина: один dispatch на resume по sid, снова упал — свежий spawn той же роли; триггеры фолбека на это не тратятся. waiting_approval: true зависшей не считается. Отработавших делегатов (разведчик после отчёта, Fable после одобрения плана, пре-гейт после вердикта, исполнители после одобрения кода гейтом, git-агент и гейт-сессии после merge) останавливай: delegation.max_children считает активных прямых детей, и idle-сессия занимает слот. Не рассылай статусы, только точки решения. Первый прогон по этой схеме — замер: в итог добавь таблицу — доля токенов по вендору, effort по каждой задаче, сработавшие триггеры фолбека, число кругов гейта. Уведомления о завершении возвращаются тебе; ты собираешь итог.\n\n",
            "Задача:"
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
        assert!(templates[0].prefix.ends_with("Задача:"));
        for role in ["Fable", "Sol", "Luna", "Terra", "Sonnet", "GLM"] {
            assert!(
                templates[0].prefix.contains(role),
                "commander roster must include {role}"
            );
        }
        assert!(!templates[0].prefix.contains("Claude Opus"));
        assert!(!templates[0].prefix.contains("Haiku"));
        assert!(templates[0].prefix.contains("3 Claude, 3 Codex, 5 GLM"));
        assert!(templates[0].prefix.contains("status"));
        assert!(templates[0].prefix.contains("Codex"));
        assert!(templates[0]
            .prefix
            .contains("Fallback разрешён только если явный ответ status"));
        for capability_code in [
            "vendor_unavailable",
            "model_unavailable",
            "effort_unavailable",
        ] {
            assert!(
                templates[0].prefix.contains(capability_code),
                "commander must accept the typed capability proof {capability_code}"
            );
        }
        for rejection in [
            "авторизация/ACL",
            "квота или бюджет",
            "depth/cycle",
            "timeout",
            "network",
            "internal",
        ] {
            assert!(
                templates[0].prefix.contains(rejection),
                "commander must forbid fallback for {rejection}"
            );
        }
        assert!(!templates[0]
            .prefix
            .contains("недоступны либо spawn отклонён"));
        assert_eq!(templates[5].label, "🏗 Пирамида");
    }

    #[test]
    fn commander_template_gates_first_code_edit_on_glm_scout() {
        let prefix = commander_quick_template().prefix;
        for phrase in [
            "zai-coding-plan/glm-5.3-flash",
            "opencode",
            "read-only",
            "до первой правки кода",
            "wait_seconds",
            "2–5",
            "commit sha или tag",
            "license",
            "которую status реально объявил для opencode",
            "ровно одну попытку разведки через Codex Luna",
            "останови работу над кодом",
            "session_list и session_collect",
            "код не копируй",
            "непригоден или не соответствует контракту",
            "общий capability-fallback ниже и правила fallback для lead и остальных ролей не меняются",
        ] {
            assert!(
                prefix.contains(phrase),
                "Commander scout contract lacks {phrase}"
            );
        }
        assert!(prefix.contains("Fallback разрешён только если явный ответ status"));
        assert!(prefix.ends_with("Задача:"));
    }

    #[test]
    fn commander_template_v3_sizes_lanes_effort_balance_fallback() {
        let prefix = commander_quick_template().prefix;
        let order = [
            "Размер задачи:",
            "Полосы и effort:",
            "Балансировка:",
            "Шаг 1, разведка",
            "Шаг 2, план",
            "Шаг 3, ревью плана",
            "Шаг 4, исполнение",
            "Шаг 5, пре-гейт и интеграция",
            "Шаг 6, гейт",
            "Фолбек:",
            "Мониторинг:",
        ];
        let mut last = 0;
        for phase in order {
            let at = prefix
                .find(phase)
                .unwrap_or_else(|| panic!("Commander v3 lacks section {phase}"));
            assert!(at >= last, "section {phase} is out of order");
            last = at;
        }
        let step6 = prefix.find("Шаг 6, гейт").expect("step 6 present");
        let gate = &prefix[step6..];
        for phrase in [
            "потолок 2 круга",
            "трёх абзацев",
            "заново собирает integration/",
            "только дифф",
        ] {
            assert!(gate.contains(phrase), "gate section lacks {phrase}");
        }
        for phrase in [
            // командир
            "Ты — командир: Claude Fable с effort high",
            // размеры
            "до 3 файлов", "Мелочь", "Средняя", "Крупная", "Claude не участвует", "уходит Terra",
            "единственный гейт — свежий Codex Sol", "Апгрейд только вверх",
            // полосы
            "Полоса A", "Полоса B", "Полоса C", "Codex Luna", "Claude Sonnet", "GLM",
            "Пре-гейт", "Гейт 1 — Claude Fable", "Гейт 2 — Codex Sol", "Git-агент — GLM",
            "Гейты всегда два разных вендора",
            // effort
            "второй круг", "на одну ступень", "boilerplate", "не ниже high", "гейт на повторе не поднимается", "ниже medium",
            // балансировка
            "tokens_24h", "quota", "выше 80", "15 % относительных", "38,3 %", "двум отстающим",
            "деньги, секреты", "миграции данных",
            // план и советник (v2 сохраняется)
            ".ccteam/plans/", "id, исполнитель, зависимости, файлы, критерий готовности", "Amendments",
            "потолок 2 круга", "трёх абзацев", "Пауз на человека нет",
            "обязательно перед любым отклонением от плана", "совет не обязателен к исполнению", "эскалирует командиру",
            "вопросы советнику пачкой",
            // исполнение
            "-wt/", "task/", "никогда в /tmp", "--test-threads=4", ".ccteam/briefs/executor.md",
            "Статус: готово", "одна задача, одна свежая сессия, один ход", "строку Ход:",
            "3 Claude, 3 Codex, 5 GLM",
            // пре-гейт и git
            "секрет-скан", "gitleaks", "чек-лист плана", "один возврат исполнителю",
            "integration/", "merge commit, не squash",
            "оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный",
            "CI — третье условие, только если он в проекте есть", "Tag и release вне",
            // гейт
            "только дифф", "Советник в приёмке не участвует",
            // фолбек
            "error_kind=server_overloaded", "два подряд", "session limit", "30 минут", "переключайся сразу",
            "Статус: готово», после одного возврата", "Один переход на задачу", "session_stop до spawn фолбека",
            "Fable → Codex Sol", "Sol → свежий Claude Fable", "Luna → Claude Sonnet", "Sonnet → Codex Terra",
            "GLM → Codex Luna", "пара стала одновендорной",
            // мониторинг (v2)
            "session_list", "free -m", "15 процентов", "waiting_approval: true зависшей не считается",
            "delegation.max_children", "останови разведчика через session_stop", "git init не делай",
            "resume по sid",
            // замер
            "Первый прогон", "доля токенов по вендору",
        ] {
            assert!(prefix.contains(phrase), "Commander v3 lacks {phrase}");
        }
        assert!(prefix.ends_with("Задача:"));
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
