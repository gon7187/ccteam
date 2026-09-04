# Командир v3 (полосы по вендорам, три размера, динамический effort, балансировка по `status`): план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Один PR dev→main: `status` отдаёт per-vendor `tokens_24h` / `spend_24h_usd` / окна подписок; четыре копии промпта «🎯 Командир» (Telegram вкомпилен + web zh/ru/en) переписаны под v3 и залочены литеральными тестами; доки синхронизированы; после merge routing.md четырёх проектов и бриф-файл переписаны, демон пересобран.

**Architecture:** Движок получает только observability (ledger по вендору в токенах + переезд пробы квот из `ccteam-web` в `ccteam-im`, чтобы MCP `status` и REST жрали один кэш). Поведение командира остаётся prompt-soft: размеры, полосы, effort, балансировка и триггеры фолбека живут в тексте шаблона, тесты = строковые локи без LLM. Web-карта Commander меняет только posture лида: `fable` / `high` вместо `opus` / `max`.

**Tech Stack:** Rust (`ccteam-im` projection + MCP panel + quota probe, `ccteam-core` шаблон, `ccteam-web` тонкий REST-хендлер), TypeScript (`i18n.ts`, `playbooks.ts`, vitest), Markdown.

**Spec:** `docs/superpowers/specs/2026-09-04-commander-v3-lanes-effort-design.md`

## Global Constraints

- Рабочая ветка `dev`, worktree `/root/projects/ccteam-wt/dev`; главный worktree `/root/projects/ccteam` (main) остаётся чистым. Без bump версии. Задачи 1–3 и 4–6 в разных путях; параллелить можно только по правилу «одна задача — один worktree» из AGENTS.md §五.
- Commit messages и текст PR на английском; новые доки на русском.
- Красные линии AGENTS.md §三: движок ничего не решает и не вызывает LLM; `status` только показывает цифры.
- Ростер v3 (спек §2): Командир Claude Fable high (фолбек Codex Sol); Разведчик GLM `zai-coding-plan/glm-5.3-flash` (фолбек Luna medium); Планировщик Fable max (фолбек Sol xhigh); Ревьюер плана и советник Sol high (фолбек свежий Fable); Полоса A Luna high (фолбек Sonnet); Полоса B Sonnet high (фолбек Terra); Полоса C GLM (фолбек Luna medium); Пре-гейт GLM (фолбек Sonnet medium); Гейт 1 Fable max (фолбек свежий Sol); Гейт 2 Sol high (фолбек свежий Fable); Git-агент GLM (фолбек Luna medium).
- Размеры: мелочь = до 3 файлов без жёстких признаков, без Claude, полоса B → Terra, гейт Sol high; средняя = план на страницу, один круг ревью плана, единственный гейт = свежий Sol high; крупная = полный цикл v2. Апгрейд размера только вверх.
- Effort: ступени из `status`; второй круг +1 ступень; boilerplate на нижней; жёсткая задача не ниже `high`; гейт на повторе не поднимается; ниже `medium` у Claude/Codex не опускаться.
- Балансировка: жёсткие (деньги, секреты, scope/ACL, миграции данных, красные линии) по полосе; гибкие вендору с наименьшей долей `tokens_24h` из `status`; окно подписки `used_percent` выше 80 делает вендора лидером; порог перекоса 15 % относительных (лидер выше 38,3 %).
- Фолбек: четыре триггера (capability `error_code`; два подряд `error_kind=server_overloaded` или текст про session/rate limit; `stale`/`stuck` + `last_active` старше 30 минут без `waiting_approval`, переключение сразу; отчёт без «Статус: готово» после одного возврата); один переход на задачу; исходную сессию `session_stop` до spawn фолбека.
- Пре-гейт GLM: линт, затронутые тесты, секрет-скан (`gitleaks detect` или grep), чек-лист плана; fail = один возврат исполнителю. Повторный гейт смотрит только дифф; потолок 2 круга.
- Бриф: `<project>/.ccteam/briefs/executor.md`, одна задача — одна свежая сессия — один ход; результат начинается с «Статус: готово» / «Статус: не готово».
- Потолки параллелизма: 3 Claude, 3 Codex, 5 GLM. Вопросы советнику пачкой.
- Сохраняются v2-фразы capability-правила (`Fallback разрешён только если явный ответ status`, три `error_code`, шесть отказов), `ends_with("Задача:")`, все v2-локи, которые остаются истинными (см. Task 4 шаг 1).
- Проверки перед push: `cargo fmt --all -- --check`, `make check`, `make test`, `make web-check`, `.loop/verify/writeback.sh`. Один `make test` за раз.

---

## Карта файлов

| Файл | Что делает |
|---|---|
| `crates/ccteam-im/src/progress_projection.rs` | `tokens_24h_by_vendor` в бакетах и снапшоте |
| `crates/ccteam-im/src/vendor_quota_probe.rs` (новый) | `VendorQuotaService` + `global()` (переезд из web) |
| `crates/ccteam-im/src/lib.rs` | `pub mod vendor_quota_probe;` |
| `crates/ccteam-web/src/routes/vendor_quota.rs` | только хендлер, зовёт `ccteam_im::vendor_quota_probe::global()` |
| `crates/ccteam-web/src/state.rs` | поле `vendor_quotas` удалено |
| `crates/ccteam-im/src/mcp/vendor_panel.rs` | `PanelRow` + три поля, рендер, `render_section(.., quotas)` |
| `crates/ccteam-im/src/mcp/dispatch.rs` | два вызова `status`: проба квот до `spawn_blocking` |
| `crates/ccteam-im/src/mcp/protocol.rs` | базовый JSON `status`: `vendors_24h` + per-project by_vendor |
| `crates/ccteam-core/src/config.rs` | шаблон v3 + тесты |
| `crates/ccteam-web/web/src/lib/i18n.ts`, `playbooks.ts`, `*.test.ts` | web-шаблон zh/ru/en, posture `fable`/`high`, локи |
| `README.md`, `docs/usage.md`, `docs/orchestration.md`, `docs/orchestration-cn.md` | текущая способность |
| вне репо: `.ccteam/routing.md` ×4, `.ccteam/briefs/executor.md` ×4 | пользовательское пространство |

---

### Task 1: `tokens_24h_by_vendor` в проекции ledger

**Files:**
- Modify: `crates/ccteam-im/src/progress_projection.rs:108-140` (снапшот), `:197-240` (бакет и `FoldedTurn`), `:686-760` (fold/unfold), `:795-812` (`fold_tokens`), `:895-945` (`snapshot`)
- Test: там же, `mod tests`

**Interfaces:**
- Produces: `ProjectProjectionSnapshot.tokens_24h_by_vendor: BTreeMap<String, u64>` (сумма input+output токенов за 24h по строке `vendor` события; события без `vendor` в карту не попадают, но в `tokens_24h` входят как раньше).

- [ ] **Step 1: Падающий тест**

В `mod tests` рядом с `codex_agent_done_...` (около строки 1150) добавить:

```rust
    #[test]
    fn tokens_24h_split_by_vendor_including_unpriced_opencode() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let now = fixed_now();
        write_rows(
            &paths.progress_jsonl("by-vendor"),
            &[
                json!({
                    "event": AGENT_DONE,
                    "session_id": "s1",
                    "turn_id": "t1",
                    "vendor": "codex",
                    "cost_usd": 1.0,
                    "usage": {"input_tokens": 100, "output_tokens": 50},
                    "ts": now.to_rfc3339(),
                }),
                json!({
                    "event": CHAT_TURN_COMPLETED,
                    "sid": "s2",
                    "turn_id": "t2",
                    "vendor": "opencode",
                    "model": "zai-coding-plan/glm-5.3-flash",
                    "usage": {"input_tokens": 1000, "output_tokens": 0},
                    "ts": now.to_rfc3339(),
                }),
                json!({
                    "event": CHAT_TURN_COMPLETED,
                    "sid": "s3",
                    "turn_id": "t3",
                    "vendor": "opencode",
                    "model": "zai-coding-plan/glm-5.3-flash",
                    "usage": {"input_tokens": 7, "output_tokens": 0},
                    "ts": (now - Duration::hours(25)).to_rfc3339(),
                }),
            ],
        );
        let projection = projection(paths);
        projection.hydrate_now(&["by-vendor".to_string()]).unwrap();
        let snapshot = projection.project_snapshot("by-vendor");
        assert_eq!(snapshot.tokens_24h, 1150);
        assert_eq!(snapshot.tokens_24h_by_vendor["codex"], 150);
        assert_eq!(snapshot.tokens_24h_by_vendor["opencode"], 1000);
        assert!(snapshot.cost.cost_24h_by_vendor.get("opencode").is_none(), "opencode stays unpriced in USD");
    }
```

Если хелпера `write_rows` в тестах нет, использовать тот, которым пишет фикстуру тест `codex_agent_done_...` (см. строки 1140–1160; имя подставить по факту).

- [ ] **Step 2: Убедиться, что падает**

Run: `cd /root/projects/ccteam-wt/dev && cargo test -p ccteam-im tokens_24h_split_by_vendor -- --nocapture`
Expected: ошибка компиляции `no field tokens_24h_by_vendor`.

- [ ] **Step 3: Реализация**

```rust
// ProjectProjectionSnapshot: после `pub tokens_24h: u64,`
    /// Raw input+output tokens in the trailing 24-hour window, keyed by the
    /// event's `vendor` string. Present for unpriced vendors too (opencode,
    /// pi, dsh) — this is the only per-vendor spend signal they have.
    pub tokens_24h_by_vendor: BTreeMap<String, u64>,

// CostBucket: после `tokens: u64,`
    tokens_by_vendor: BTreeMap<String, u64>,

// FoldedTurn: поле `vendor` теперь берётся из события, не из cost contribution
// (для unpriced turn contribution = None и vendor терялся).
```

В `fold_turn`: `let event_vendor = event.get("vendor").and_then(Value::as_str);` и `vendor: event_vendor.map(str::to_string)` в `FoldedTurn`. В обе ветки вызова `fold_tokens` добавить аргумент `event_vendor`.

```rust
fn fold_tokens(
    state: &mut SlugState,
    tokens: u64,
    vendor: Option<&str>,
    priced: bool,
    event_minute: Option<i64>,
    now: DateTime<Utc>,
) {
    let oldest = minute(now).saturating_sub(MINUTES_24H);
    state.minute_cost.retain(|minute, _| *minute >= oldest);
    if let Some(event_minute) = event_minute {
        let bucket = state.minute_cost.entry(event_minute).or_default();
        bucket.tokens = bucket.tokens.saturating_add(tokens);
        if let Some(vendor) = vendor {
            let slot = bucket.tokens_by_vendor.entry(vendor.to_string()).or_insert(0);
            *slot = slot.saturating_add(tokens);
        }
        if !priced {
            bucket.unpriced = bucket.unpriced.saturating_add(1);
        }
    }
}
```

В `unfold_turn`, внутри `if turn.tokens > 0 { ... }` после `bucket.tokens = ...`:

```rust
                if let Some(vendor) = turn.vendor.as_deref() {
                    if let Some(slot) = bucket.tokens_by_vendor.get_mut(vendor) {
                        *slot = slot.saturating_sub(turn.tokens);
                    }
                }
```

В `snapshot()` перед `ProjectProjectionSnapshot { ... }`:

```rust
    let mut tokens_24h_by_vendor: BTreeMap<String, u64> = BTreeMap::new();
    for bucket in state.minute_cost.range(oldest..).map(|(_, bucket)| bucket) {
        for (vendor, tokens) in &bucket.tokens_by_vendor {
            let slot = tokens_24h_by_vendor.entry(vendor.clone()).or_insert(0);
            *slot = slot.saturating_add(*tokens);
        }
    }
```

и поле `tokens_24h_by_vendor,` в конструкторе. `unfold_turn` для USD по-прежнему использует `turn.vendor` — семантика не меняется (у priced событий `vendor` события и contribution совпадают).

- [ ] **Step 4: Зелёный прогон крейта**

Run: `cargo test -p ccteam-im progress_projection`
Expected: все PASS, включая новый.

- [ ] **Step 5: Commit**

```bash
git add crates/ccteam-im/src/progress_projection.rs
git commit -m "feat(im): split 24h token ledger by vendor in the progress projection"
```

---

### Task 2: проба квот переезжает из `ccteam-web` в `ccteam-im`

**Files:**
- Create: `crates/ccteam-im/src/vendor_quota_probe.rs`
- Modify: `crates/ccteam-im/src/lib.rs:76` (модуль), `crates/ccteam-im/Cargo.toml` (dev-dep `axum` если нет)
- Modify: `crates/ccteam-web/src/routes/vendor_quota.rs` (оставить только хендлер), `crates/ccteam-web/src/state.rs:126,202` (убрать поле)

**Interfaces:**
- Produces: `ccteam_im::vendor_quota_probe::{VendorQuotaService, global}`; `pub fn global() -> &'static VendorQuotaService`; `pub async fn quotas(&self) -> Vec<VendorQuota>` (сигнатура и порядок строк те же, что в web).
- Consumes: `ccteam_core::vendor_quota::*` без изменений.

- [ ] **Step 1: Перенос модуля**

```bash
cd /root/projects/ccteam-wt/dev
git mv crates/ccteam-web/src/routes/vendor_quota.rs crates/ccteam-im/src/vendor_quota_probe.rs
```

В новом файле: удалить `use axum::{...}`, `use crate::auth::...`, `use crate::state::AppState`, функцию `handle_vendor_quota` с её `#[utoipa::path]`; заголовочный doc-комментарий заменить на:

```rust
//! Vendor subscription-quota probe (claude / codex / kimi usage APIs): the
//! process-lifetime, per-vendor 5-minute cache shared by the REST route
//! `GET /api/v1/vendors/quota` (ccteam-web) and the MCP `status` panel.
//! Read-only credential files, no OAuth refresh; every failure is an
//! `unavailable` row. Pure parsers live in `ccteam_core::vendor_quota`.
```

Добавить глобальный экземпляр:

```rust
/// One service per process: both callers must share the cache, otherwise
/// each MCP `status` call would hit three vendor APIs.
pub fn global() -> &'static VendorQuotaService {
    static SERVICE: std::sync::OnceLock<VendorQuotaService> = std::sync::OnceLock::new();
    SERVICE.get_or_init(VendorQuotaService::default)
}
```

В `crates/ccteam-im/src/lib.rs` после `pub mod transport;` (строка 87): `pub mod vendor_quota_probe;`.

Тесты модуля едут вместе с ним. Проверить dev-deps: `grep -n '^axum' crates/ccteam-im/Cargo.toml`; если в `[dependencies]` axum есть — ничего не делать, если нет — добавить в `[dev-dependencies]` `axum = { workspace = true }` (версия как в web).

- [ ] **Step 2: Web становится тонким**

Новое содержимое `crates/ccteam-web/src/routes/vendor_quota.rs`:

```rust
//! VENDOR-QUOTA-1 — `GET /api/v1/vendors/quota`: the local machine's vendor
//! subscription-quota snapshot for the Ops & Hosts page. Admin-only: it reads
//! the daemon user's vendor credential files. The probe + cache live in
//! `ccteam_im::vendor_quota_probe` (shared with the MCP `status` panel).

use axum::{
    response::{IntoResponse, Response},
    Extension, Json,
};

use crate::auth::{deny_non_admin, Identity};

/// `GET /api/v1/vendors/quota` — the local machine's per-vendor quota rows.
/// Vendors with no probe surface (opencode/pi/dsh) are absent from the list.
#[utoipa::path(
    get,
    path = "/api/v1/vendors/quota",
    tag = "hosts",
    responses(
        (status = 200, description = "Per-vendor quota rows `{quotas: [{vendor, state, plan?, windows?}]}`", body = serde_json::Value),
        (status = 403, description = "Not the admin/owner"),
    ),
)]
pub(crate) async fn handle_vendor_quota(Extension(identity): Extension<Identity>) -> Response {
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let quotas = ccteam_im::vendor_quota_probe::global().quotas().await;
    Json(serde_json::json!({ "quotas": quotas })).into_response()
}
```

В `state.rs` удалить поле `vendor_quotas` (строка 126 с комментарием и строка 202). Если `State(app)` больше нигде в хендлере не нужен — `cargo check -p ccteam-web` покажет неиспользуемые импорты; убрать.

- [ ] **Step 3: Проверка**

Run: `cargo test -p ccteam-im vendor_quota_probe && cargo test -p ccteam-web vendor_quota && cargo clippy -p ccteam-im -p ccteam-web --all-targets -- -D warnings`
Expected: тесты кэша/401/timeout/ok-body PASS в `ccteam-im`; web компилируется, clippy без warning. Существующие web-интеграционные тесты на `/api/v1/vendors/quota` (если есть: `grep -rn "vendors/quota" crates/ccteam-web/tests`) не меняются и зелёные.

- [ ] **Step 4: Commit**

```bash
git add -A crates/ccteam-im crates/ccteam-web/src
git commit -m "refactor(im,web): move the vendor quota probe into ccteam-im behind a process-wide cache"
```

---

### Task 3: `status` показывает расход и квоты по вендору

**Files:**
- Modify: `crates/ccteam-im/src/mcp/vendor_panel.rs:163-215` (`PanelRow`, `render_panel`), `:480-530` (`budget_row`, `local_rows`, `satellite_rows`), `:540-560` (`render_section`), `:660-740` (`build_project_panel`, `build_local_panel`, `satellite_panel`), тесты `:951-998`
- Modify: `crates/ccteam-im/src/mcp/dispatch.rs:1557-1565`, `:1602-1618`
- Modify: `crates/ccteam-im/src/mcp/protocol.rs:328-347` (`tool_ls_matching`), тест `:935`

**Interfaces:**
- Consumes: `ProjectProjectionSnapshot.tokens_24h_by_vendor` (Task 1), `vendor_quota_probe::global().quotas()` (Task 2).
- Produces: строка панели вида `claude  installed=yes claude 1.2.3  auth=unknown  budget=ok  spend_24h=$1.23  tokens_24h=123456  quota=five_hour:42%,reset=2026-09-04T18:00Z;weekly:10%`; для unpriced `spend_24h=n/a`; без пробы `quota=n/a`. Базовый JSON `status` получает `vendors_24h: {"<vendor>": {"tokens": u64, "spend_usd": f64|null}}` (сумма по видимым проектам) и в каждом `projects[]` поле `tokens_24h_by_vendor`.

- [ ] **Step 1: Падающий литеральный тест панели**

В `panel_renders_installed_and_missing_rows` (строка 951) дополнить каждый `PanelRow` тремя полями и добавить проверки:

```rust
        // claude: priced, с квотой
        PanelRow {
            vendor: "claude".to_string(),
            installed: true,
            version: Some("claude 1.2.3".to_string()),
            last_session_ok: None,
            budget: BudgetState::Ok,
            spend_24h_usd: Some(1.23),
            tokens_24h: 123_456,
            quota: Some(ccteam_core::vendor_quota::VendorQuota::available(
                "claude",
                Some("max".into()),
                vec![
                    ccteam_core::vendor_quota::QuotaWindow {
                        kind: ccteam_core::vendor_quota::QuotaWindowKind::FiveHour,
                        used_percent: 42.0,
                        resets_at: Some("2026-09-04T18:00:00Z".parse().unwrap()),
                    },
                    ccteam_core::vendor_quota::QuotaWindow {
                        kind: ccteam_core::vendor_quota::QuotaWindowKind::Weekly,
                        used_percent: 10.0,
                        resets_at: None,
                    },
                ],
            )),
        },
        // codex: spend_24h_usd Some(9.0), tokens_24h 5, quota None
        // grok: не установлен — поля любые, строка всё равно `installed=no`
        // kimi: Unpriced → spend_24h_usd None, tokens_24h 77, quota Some(VendorQuota::unavailable("kimi"))
```

Ассерты:

```rust
        let claude_line = out.lines().find(|l| l.trim_start().starts_with("claude")).unwrap();
        assert!(claude_line.contains("spend_24h=$1.23"));
        assert!(claude_line.contains("tokens_24h=123456"));
        assert!(claude_line.contains("quota=five_hour:42%,reset=2026-09-04T18:00Z;weekly:10%"));
        let codex_line = out.lines().find(|l| l.trim_start().starts_with("codex")).unwrap();
        assert!(codex_line.contains("spend_24h=$9.00") && codex_line.contains("tokens_24h=5") && codex_line.contains("quota=n/a"));
        let kimi_line = out.lines().find(|l| l.trim_start().starts_with("kimi")).unwrap();
        assert!(kimi_line.contains("spend_24h=n/a") && kimi_line.contains("tokens_24h=77") && kimi_line.contains("quota=n/a"));
        assert!(!grok_line.contains("tokens_24h"));
```

`QuotaWindowKind` варианты см. `crates/ccteam-core/src/vendor_quota.rs:55-62` (`FiveHour`, `Weekly`, `Monthly` и что там ещё есть; в тесте использовать реально существующие).

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p ccteam-im panel_renders_installed_and_missing_rows`
Expected: ошибка компиляции `missing fields spend_24h_usd, tokens_24h, quota`.

- [ ] **Step 3: Реализация панели**

`PanelRow`:

```rust
    pub budget: BudgetState,
    /// Trailing-24h USD from the daemon ledger; `None` for unpriced vendors.
    pub spend_24h_usd: Option<f64>,
    /// Trailing-24h input+output tokens from the daemon ledger (all vendors).
    pub tokens_24h: u64,
    /// Subscription quota windows when this vendor has a probe (claude /
    /// codex / kimi); `None` = no probe surface.
    pub quota: Option<ccteam_core::vendor_quota::VendorQuota>,
```

`PanelRow` держит `#[derive(PartialEq, Eq)]` — `f64` не `Eq`: снять `Eq` с derive (оставить `PartialEq`). Рендер строки в `render_panel`:

```rust
        let spend = match row.spend_24h_usd {
            Some(v) => format!("${v:.2}"),
            None => "n/a".to_string(),
        };
        out.push_str(&format!(
            "\n  {:<vendor_w$}  {:<28}  {}  budget={}  spend_24h={}  tokens_24h={}  quota={}",
            row.vendor,
            installed_seg,
            auth,
            row.budget.render(),
            spend,
            row.tokens_24h,
            render_quota(row.quota.as_ref()),
        ));
```

```rust
/// `five_hour:42%,reset=2026-09-04T18:00Z;weekly:10%` or `n/a` when the vendor
/// has no probe, is not a subscription, or the probe failed.
fn render_quota(quota: Option<&ccteam_core::vendor_quota::VendorQuota>) -> String {
    use ccteam_core::vendor_quota::QuotaState;
    let Some(quota) = quota else {
        return "n/a".to_string();
    };
    if quota.state != QuotaState::Available || quota.windows.is_empty() {
        return "n/a".to_string();
    }
    quota
        .windows
        .iter()
        .map(|w| {
            let kind = serde_json::to_value(&w.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{:?}", w.kind).to_lowercase());
            let mut seg = format!("{kind}:{}%", w.used_percent.round() as i64);
            if let Some(reset) = w.resets_at {
                seg.push_str(&format!(",reset={}", reset.format("%Y-%m-%dT%H:%MZ")));
            }
            seg
        })
        .collect::<Vec<_>>()
        .join(";")
}
```

Если `QuotaWindowKind` не сериализуется в snake_case (проверить `#[serde(rename_all = ...)]` на enum в `vendor_quota.rs`), добавить ему `#[serde(rename_all = "snake_case")]` — это меняет JSON REST-роута, поэтому сначала `grep -rn "five_hour\|FiveHour" crates/ccteam-web/web/src` и убедиться, что SPA ждёт snake_case; если SPA ждёт другой формат — рендерить через `match` по вариантам вручную, enum не трогать.

`local_rows` / `satellite_rows` получают ещё два аргумента `tokens_24h: &BTreeMap<String, u64>` и `quotas: &[VendorQuota]`:

```rust
            spend_24h_usd: vendor_is_priced(&a.vendor).then(|| spend_24h.get(&*a.vendor).copied().unwrap_or(0.0)),
            tokens_24h: tokens_24h.get(&*a.vendor).copied().unwrap_or(0),
            quota: quotas.iter().find(|q| q.vendor == a.vendor).cloned(),
```

(`a.vendor` в `local_rows` — `&str`, в `satellite_rows` — `String`; подобрать `&*`/`.as_str()` по месту.)

`build_project_panel(paths, slug, quotas)`, `build_local_panel(paths, slug, note, quotas)`, `satellite_panel(paths, slug, host, budgets, spend_24h, tokens_24h, quotas)`: рядом с `spend_24h` брать `let snapshot = ProgressProjection::new(paths.clone()).project_snapshot(slug); let spend_24h = snapshot.cost.cost_24h_by_vendor; let tokens_24h = snapshot.tokens_24h_by_vendor;` (один снапшот вместо двух). `render_section(paths, slug, hub, quotas: &[VendorQuota])` прокидывает дальше. Прочие тесты, конструирующие `PanelRow` (grep `PanelRow {` в файле) дополнить `spend_24h_usd: None, tokens_24h: 0, quota: None`.

- [ ] **Step 4: Вызовы в `dispatch.rs`**

В обоих местах (строки 1557 и 1602) перед `spawn_blocking`:

```rust
    let quotas = crate::vendor_quota_probe::global().quotas().await;
```

и передать `&quotas` в `render_section` (в замыкание переехать по `move`, как `hub_models`).

- [ ] **Step 5: Базовый JSON `status`**

В `tool_ls_matching` (`protocol.rs:328`):

```rust
    let mut vendors_24h: BTreeMap<String, (u64, Option<f64>)> = BTreeMap::new();
    let arr: Vec<Value> = projects
        .iter()
        .filter(|project| visible(&project.state))
        .map(|p| {
            let snapshot = projection.project_snapshot(&p.state.slug);
            for (vendor, tokens) in &snapshot.tokens_24h_by_vendor {
                let slot = vendors_24h.entry(vendor.clone()).or_insert((0, None));
                slot.0 = slot.0.saturating_add(*tokens);
            }
            for (vendor, usd) in &snapshot.cost.cost_24h_by_vendor {
                let slot = vendors_24h.entry(vendor.clone()).or_insert((0, None));
                slot.1 = Some(slot.1.unwrap_or(0.0) + usd);
            }
            json!({
                "slug": p.state.slug,
                "cost_24h_usd": snapshot.cost.cost_24h_usd,
                "tokens_24h_by_vendor": snapshot.tokens_24h_by_vendor,
            })
        })
        .collect();
    let vendors_24h: serde_json::Map<String, Value> = vendors_24h
        .into_iter()
        .map(|(vendor, (tokens, usd))| (vendor, json!({"tokens": tokens, "spend_usd": usd})))
        .collect();
    let body = json!({
        "projects": arr,
        "vendors_24h": vendors_24h,
        "daemon": daemon_health_json(&health),
    });
```

Тест на строке 935: `project_keys` теперь `BTreeSet::from(["cost_24h_usd", "slug", "tokens_24h_by_vendor"])`; рядом добавить `assert!(body.get("vendors_24h").is_some())`.

- [ ] **Step 6: Зелёный прогон**

Run: `cargo test -p ccteam-im mcp:: && cargo clippy -p ccteam-im --all-targets -- -D warnings`
Expected: PASS, 0 warnings. Затем `cargo test -p ccteam-im` целиком — baseline не падает.

- [ ] **Step 7: Commit**

```bash
git add crates/ccteam-im/src/mcp
git commit -m "feat(mcp): status shows per-vendor 24h spend, tokens and subscription quota windows"
```

---

### Task 4: шаблон «🎯 Командир» v3 (Telegram, вкомпилен)

**Files:**
- Modify: `crates/ccteam-core/src/config.rs:138-162` (`commander_quick_template`), тесты `:917-1075`

**Interfaces:**
- Produces: `commander_quick_template().prefix` = текст ниже; локи из шага 1.

- [ ] **Step 1: Переписать тесты**

`default_im_quick_templates_include_six_templates`: ростер `["Fable", "Sol", "Luna", "Terra", "Sonnet", "GLM"]` (Opus и Haiku из ростера убраны; добавить `assert!(!prefix.contains("Claude Opus"))`); оставить проверки `ends_with("Задача:")`, `status`, `Codex`, `Fallback разрешён только если явный ответ status`, три `error_code`, шесть отказов, `!contains("недоступны либо spawn отклонён")`. Убрать `contains("не более 3")` и `contains("максимальн")` (v3 не гонит всех на max), взамен `contains("3 Claude, 3 Codex, 5 GLM")`.

`commander_template_gates_first_code_edit_on_glm_scout`: без изменений, кроме фразы `общий capability-fallback ниже и правила fallback для lead и остальных ролей не меняются` → оставить как есть (текст ниже её сохраняет).

`commander_template_runs_plan_gate_worktrees_git_agent_and_monitoring` переименовать в `commander_template_v3_sizes_lanes_effort_balance_fallback` и заменить тело:

```rust
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
            let at = prefix.find(phase).unwrap_or_else(|| panic!("Commander v3 lacks section {phase}"));
            assert!(at >= last, "section {phase} is out of order");
            last = at;
        }
        for phrase in [
            // командир
            "Ты — командир: Claude Fable с effort high",
            // размеры
            "до 3 файлов", "Мелочь", "Средняя", "Крупная", "Claude не участвует", "уходит Terra",
            "единственный гейт — свежий Codex Sol", "апгрейд только вверх",
            // полосы
            "Полоса A", "Полоса B", "Полоса C", "Codex Luna", "Claude Sonnet", "GLM",
            "Пре-гейт", "Гейт 1 — Claude Fable", "Гейт 2 — Codex Sol", "Git-агент — GLM",
            "Гейты всегда два разных вендора",
            // effort
            "второй круг", "на одну ступень", "boilerplate", "не ниже high", "гейт на повторе не поднимается", "ниже medium",
            // балансировка
            "tokens_24h", "quota", "выше 80", "15 % относительных", "38", "двум отстающим",
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
            "только дифф", "советник в приёмке не участвует",
            // фолбек
            "error_kind=server_overloaded", "два подряд", "session limit", "30 минут", "переключайся сразу",
            "Статус: готово», после одного возврата", "один переход на задачу", "session_stop до spawn фолбека",
            "Fable → Codex Sol", "Sol → свежий Claude Fable", "Luna → Claude Sonnet", "Sonnet → Codex Terra",
            "GLM → Codex Luna", "пара стала одновендорной",
            // мониторинг (v2)
            "session_list", "free -m", "15 процентов", "waiting_approval: true зависшей не считается",
            "delegation.max_children", "останови разведчика через session_stop", "git init не делай",
            // замер
            "первый прогон", "доля токенов по вендору",
        ] {
            assert!(prefix.contains(phrase), "Commander v3 lacks {phrase}");
        }
        assert!(prefix.ends_with("Задача:"));
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test -p ccteam-core commander_template`
Expected: FAIL на `Commander v3 lacks Размер задачи:`.

- [ ] **Step 3: Новый текст шаблона**

Заменить `prefix: concat!(...)` целиком на:

```rust
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
            "Мониторинг: ты не тикаешь — проверяй перед каждым spawn и на каждом уведомлении о завершении. Хост — nproc, uptime, free -m через свой shell; сессии — session_list (activity, last_active, waiting_approval; last_active — метка RFC3339, тишину считай от неё). Пороги по умолчанию (routing.md проекта может переопределить): load1 выше числа ядер, свободной памяти меньше 15 процентов, swap в работе. При перегрузе по возрастанию: 1) не спавнь новых исполнителей, пока не отпустит; 2) через dispatch попроси работающих снизить -j и --test-threads и отложить тяжёлые тесты; 3) останови самого свежего исполнителя через session_stop и верни задачу в план (ветка в worktree остаётся); 4) git-агента посреди интеграции и гейт не трогай никогда. waiting_approval: true зависшей не считается. Отработавших делегатов (разведчик после отчёта, Fable после одобрения плана, пре-гейт после вердикта, исполнители после одобрения кода гейтом, git-агент и гейт-сессии после merge) останавливай: delegation.max_children считает активных прямых детей, и idle-сессия занимает слот. Не рассылай статусы, только точки решения. Первый прогон по этой схеме — замер: в итог добавь таблицу — доля токенов по вендору, effort по каждой задаче, сработавшие триггеры фолбека, число кругов гейта. Уведомления о завершении возвращаются тебе; ты собираешь итог.\n\n",
            "Задача:"
        )
```

- [ ] **Step 4: Зелёный прогон**

Run: `cargo test -p ccteam-core config::`
Expected: PASS (все три commander-теста + `default_im_quick_templates_include_six_templates`). Если какая-то фраза из локов не совпала с текстом байт в байт — править тест или текст так, чтобы смысл спека сохранился; локи не ослаблять до `contains("Sol")`.

- [ ] **Step 5: Commit**

```bash
git add crates/ccteam-core/src/config.rs
git commit -m "feat(core): commander v3 template — task sizes, vendor lanes, dynamic effort, status-driven balancing, four fallback triggers"
```

---

### Task 5: web-шаблон zh/ru/en и posture лида

**Files:**
- Modify: `crates/ccteam-web/web/src/lib/i18n.ts:40-43` (zh), `:516-520` (ru), `:563-567` (en)
- Modify: `crates/ccteam-web/web/src/lib/playbooks.ts:34-40`, `:68-100` (`commanderClaudePosture`)
- Modify: `crates/ccteam-web/web/src/lib/playbooks.test.ts:67-231`, `i18n.test.ts:62-67`

**Interfaces:**
- Consumes: текст Task 4 как ru-источник.
- Produces: `applyPlaybook("commander", lang)` → `{ text, vendor: "claude", model: "fable", effort: "high" }`; `commanderClaudePosture` → `model: "fable"`, effort `"high"` если объявлен, иначе верхняя ступень как раньше.

- [ ] **Step 1: Переписать vitest-локи**

В `playbooks.test.ts`:

- `applyPlaybook computes the composer patch`: ожидание `{ text: I18N.zh.tplCommanderP, vendor: "claude", model: "fable", effort: "high" }`.
- `the commander prefill carries the full roster...`: ростер `["Fable", "Sol", "Luna", "Terra", "Sonnet", "GLM"]`; удалить `toContain("не более 3")`, `toContain("максимальн")`; добавить `expect(prompt).not.toContain("Claude Opus")` и `toContain("3 Claude, 3 Codex, 5 GLM")`. В матрицах `required` заменить Opus-фразы: ru `"Если Opus доступен"` → `"если текущая сессия не Fable"`, `"текущая сессия намеренно запущена как Codex fallback"` → `"намеренно запущена как Codex Sol fallback"`, `"не создавай Opus повторно"` → `"Fable повторно не создавай"`; zh `"如果 Opus 可用"` → `"如果当前会话不是 Fable"`, `"不要再创建 Opus"` → `"不要再创建 Fable"`; en `"If Opus is available"` → `"if the current session is not Fable"`, `"do not spawn Opus again"` → `"do not spawn Fable again"`. Списки `broadFallback`/`unconditionalRespawn` оставить (они отрицательные и остаются истинными). Матрицу GLM-разведки оставить.
- Тест `carries the v2 contract...` переименовать в `carries the v3 contract in every language: sizes, lanes, effort, balancing, pre-gate, fallback triggers` с матрицей:

```ts
      zh: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id、执行者、依赖、文件、完成标准",
        "上限 2 轮", "Fable 的立场、Sol 的立场、需要决定什么", "不设人工暂停",
        "任务大小：", "最多 3 个文件", "小任务", "中任务", "大任务", "Claude 不参与", "交给 Terra", "唯一门 — 全新的 Codex Sol", "只能升级",
        "泳道 A", "泳道 B", "泳道 C", "预门", "门 1 — Claude Fable", "门 2 — Codex Sol", "git 代理 — GLM", "两个门始终是不同厂商",
        "第二轮", "提高一级", "boilerplate", "不低于 high", "重复门不提升", "低于 medium",
        "tokens_24h", "quota", "高于 80", "相对 15 %", "38", "两个落后者", "资金、密钥", "数据迁移",
        "-wt/", "task/", "--test-threads=4", ".ccteam/briefs/executor.md", "状态：完成", "一个任务、一个全新会话、一个回合", "进展：",
        "3 Claude, 3 Codex, 5 GLM", "密钥扫描", "gitleaks", "计划清单", "退回执行者一次",
        "integration/", "merge commit", "两位终审都批准同一修订且全量本地检查为绿", "CI 只有在项目里存在", "tag 与 release 不在本任务内",
        "只看 diff", "顾问不参与验收",
        "error_kind=server_overloaded", "连续两次", "session limit", "30 分钟", "立即切换", "状态：完成», 退回一次后", "每个任务只切换一次", "spawn 后备之前先 session_stop",
        "Fable → Codex Sol", "Sol → 全新的 Claude Fable", "Luna → Claude Sonnet", "Sonnet → Codex Terra", "GLM → Codex Luna", "同一厂商",
        "session_list", "free -m", "15%", "waiting_approval: true 不算挂起", "delegation.max_children", "用 session_stop 停止侦察员", "git init",
        "首次运行", "各厂商 token 占比",
      ],
      ru: [ /* те же фразы, что в Task 4 шаг 1, дословно */ ],
      en: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id, implementer, dependencies, files, definition of done",
        "cap of 2 rounds", "Fable's position, Sol's position, what needs deciding", "No human pause",
        "Task size:", "up to 3 files", "Small", "Medium", "Large", "Claude does not take part", "goes to Terra", "the only gate is a fresh Codex Sol", "upgrade only upward",
        "Lane A", "Lane B", "Lane C", "Pre-gate", "Gate 1 — Claude Fable", "Gate 2 — Codex Sol", "Git agent — GLM", "Gates are always two different vendors",
        "second round", "one rung", "boilerplate", "not below high", "a repeat gate does not raise", "below medium",
        "tokens_24h", "quota", "above 80", "15 % relative", "38", "the two lagging vendors", "money, secrets", "data migrations",
        "-wt/", "task/", "--test-threads=4", ".ccteam/briefs/executor.md", "Status: done", "one task, one fresh session, one turn", "Progress:",
        "3 Claude, 3 Codex, 5 GLM", "secret scan", "gitleaks", "plan checklist", "one return to the implementer",
        "integration/", "merge commit, not squash", "both final reviewers approved the same revision and the full local check suite is green", "CI is a third condition only if the project has one", "Tag and release are outside",
        "only the diff", "advisor does not take part in acceptance",
        "error_kind=server_overloaded", "two consecutive", "session limit", "30 minutes", "switch immediately", "Status: done”, after one return", "one switch per task", "session_stop before spawning the fallback",
        "Fable → Codex Sol", "Sol → fresh Claude Fable", "Luna → Claude Sonnet", "Sonnet → Codex Terra", "GLM → Codex Luna", "single-vendor pair",
        "session_list", "free -m", "15 percent", "waiting_approval: true does not count as hung", "delegation.max_children", "stop the scout with session_stop", "never run git init",
        "first run", "token share per vendor",
      ],
```

Порядок секций: zh `["任务大小：", "泳道与 effort：", "均衡：", "第 1 步", "第 2 步", "第 3 步", "第 4 步", "第 5 步", "第 6 步", "后备：", "监控"]`, ru как в Task 4, en `["Task size:", "Lanes and effort:", "Balancing:", "Step 1", "Step 2", "Step 3", "Step 4", "Step 5", "Step 6", "Fallback:", "Monitoring"]`. Гейт-фразы: zh `["上限 2 轮", "三段报告", "只看 diff"]`, ru `["потолок 2 круга", "трёх абзацев", "только дифф"]`, en `["cap of 2 rounds", "three-paragraph report", "only the diff"]`. `tplCommanderD` во всех языках содержит `"Fable"`, `"Sol"`, `"GLM"`.

`i18n.test.ts:62-67` не меняется (GLM-модель и `.ccteam/plans/` остаются).

- [ ] **Step 2: Убедиться, что падает**

Run: `cd crates/ccteam-web/web && npx vitest run src/lib/playbooks.test.ts`
Expected: FAIL на posture `opus`/`max` и на `任务大小：`.

- [ ] **Step 3: Тексты и posture**

`i18n.ts` ru `tplCommanderP` = текст Task 4 дословно (тот же `concat`, склеенный в одну TS-строку), с одним web-дополнением в первом абзаце после «Fable повторно не создавай.»: сохранить существующие web-фразы про capability-fallback лида, заменив Opus на Fable: «Только явное сообщение status или capability-ошибка spawn, что Claude или Fable не подключены или недоступны, переводит лида на Codex Sol; при ошибке аутентификации, ACL, тайм-ауте, квоте, глубины делегирования, бюджете или цикле не повторяй запрос и fail closed.» (фразы `Только явное сообщение`, `не подключены или недоступны`, `не повторяй запрос`, `аутентификации, ACL, тайм-ауте, квоте`, `fail closed`, `глубины делегирования`, `бюджете`, `цикле` — локи существующего теста, сохранить как подстроки). `tplCommanderD` ru: «Fable командует на high; GLM ведёт read-only разведку; три размера задачи и три полосы: Luna бэкенд, Sonnet фронт, GLM тесты и boilerplate; Sol ставит гейт плана и советует; пре-гейт GLM; гейты Fable + Sol; git-агент GLM; effort по размеру и кругу; балансировка по tokens_24h из status; четыре триггера фолбека, один переход на задачу».

zh и en — перевод того же текста; исполнитель переводит абзац за абзацем, сверяя матрицу локов шага 1 (каждая фраза матрицы обязана появиться дословно). Существующие zh/en локи GLM-разведки и capability-правила лида (`只有 status 或 spawn 的 capability 错误`, `Only an explicit status result or spawn capability error`, `fail closed`, `委派深度`, `delegation-depth`, и т. д.) сохранить как подстроки, заменив только Opus → Fable по списку шага 1. `tplCommanderD` zh/en — перевод ru-описания.

`playbooks.ts`: строка 38–39 `model: "fable", effort: "high"`; в `commanderClaudePosture` заменить `"opus"` на `"fable"` (тип `model: "fable"`), а `efforts.at(-1)` на:

```ts
  const effort = efforts.includes("high") ? "high" : efforts.at(-1);
```

с комментарием `// v3: the commander runs on high; fall back to the top advertised rung only when high is not advertised.` Проверить `effortRowsFor("claude", null, "fable")` в `vendors.ts:63-65` — алиас `fable` там уже объявлен.

- [ ] **Step 4: Зелёный прогон**

Run: `cd crates/ccteam-web/web && npx vitest run && cd /root/projects/ccteam-wt/dev && make web-check`
Expected: PASS, `web-check` зелёный (lint + typecheck + build).

- [ ] **Step 5: Commit**

```bash
git add crates/ccteam-web/web/src/lib
git commit -m "feat(web): commander v3 prefill in zh/ru/en; commander lead posture fable/high"
```

---

### Task 6: доки (текущая способность)

**Files:**
- Modify: `README.md:64`, `docs/usage.md:261`, `docs/orchestration.md:113`, `docs/orchestration-cn.md` (абзац «Commander & crews», найти по `Commander` / `总控`)

- [ ] **Step 1: README.md:64** — заменить описание Commander на:

> Commander starts with Claude Fable at `high` effort and sizes every top-level task first: small (≤3 files, no money/secrets/scope risk) runs one lane → GLM pre-gate → one Codex Sol gate with no Claude involved; medium gets a one-page Fable plan, one Sol review round, lane implementers and a single fresh Sol gate; large runs the full pipeline — a read-only OpenCode GLM (`zai-coding-plan/glm-5.3-flash`) scout, a Fable plan under `.ccteam/plans/`, a Sol plan gate that stays on as advisor, three vendor lanes (Codex Luna backend/money/ETL, Claude Sonnet frontend/routes/glue, GLM tests/docs/boilerplate) in per-task git worktrees, a GLM pre-gate (lint, targeted tests, secret scan, plan checklist), a GLM git agent, and a Fable + Sol dual gate on the integrated diff. Effort is per size and per round (second round +1 rung, boilerplate stays low, hard tasks never below `high`); flexible tasks go to the vendor with the smallest `tokens_24h` share reported by MCP `status` (15 % relative skew threshold, subscription windows above 80 % override); four typed fallback triggers (capability error, two `server_overloaded` in a row, 30 minutes of silence, a report without `Status: done`) switch a role to its fallback vendor exactly once per task. `status` now shows per-vendor 24h spend, tokens and subscription quota windows.

- [ ] **Step 2: docs/usage.md:261** — тот же смысл в стиле абзаца (существующий текст про GLM-разведку, план, советника, worktree, git-агента и merge-условие сохранить, заменив Opus-командира на Fable high, git-агента Sonnet на GLM, добавив размеры, полосы, пре-гейт, effort, балансировку по `status`, четыре триггера и бриф `.ccteam/briefs/executor.md`). Рядом с описанием MCP `status` (grep `budget=` в `docs/usage.md`) добавить строку панели с `spend_24h= tokens_24h= quota=` и поле `vendors_24h` базового JSON.

- [ ] **Step 3: docs/orchestration.md:113 и orchestration-cn.md** — обновить пункт «Commander & crews» по README (cn — перевод), плюс одна строка в разделе про `status`: per-vendor `tokens_24h` / `spend_24h_usd` / `quota` для балансировки.

- [ ] **Step 4: Commit**

```bash
git add README.md docs/usage.md docs/orchestration.md docs/orchestration-cn.md
git commit -m "docs: commander v3 — sizes, lanes, effort, status-driven balancing; status per-vendor ledger"
```

---

### Task 7: полные проверки, push, PR

- [ ] **Step 1:** `cd /root/projects/ccteam-wt/dev && cargo fmt --all && cargo fmt --all -- --check && make check && make test && make web-check && .loop/verify/writeback.sh`
Expected: всё зелёное; число тестов ≥ baseline из `.loop/state.md`; clippy 0 warnings. Красное — чинить в соответствующей задаче, не пушить.

- [ ] **Step 2:** `git push origin dev`, затем `gh pr create --base main --head dev --title "Commander v3: task sizes, vendor lanes, dynamic effort, status-driven balancing" --body-file -` с телом (английский): Summary (три пункта: status per-vendor ledger + quota probe moved to ccteam-im; commander v3 template in four copies with literal locks; docs), Spec link, Test plan (команды шага 1). Хвост тела: `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

- [ ] **Step 3:** Merge — по правилам AGENTS.md §五: owner руками или git-агент Командира после двойного гейта. Этот план merge не делает.

---

### Task 8 (после merge, вне репо): пользовательское пространство и демон

- [ ] **Step 1:** `cd /root/projects/ccteam && git pull --ff-only && make daemon-restart` (шаблон вкомпилен; простой `ccteam stop/start` оставит старый промпт).

- [ ] **Step 2:** `.ccteam/routing.md` в проектах ccteam (`/root/projects/ccteam`), DASHIGOR, 4G, all (пути по `~/.ccteam/config.yaml`): таблица ростера v3 (спек §2, с моделями `claude-fable-5-1`, `gpt-5.6-sol`, `gpt-5.6-luna`, `gpt-5.6-terra`, `sonnet`, `zai-coding-plan/glm-5.3-flash`) + строка «Размеры, полосы, effort, балансировка и триггеры фолбека — по шаблону Командир; здесь только модели и специфика проекта»; секции «Project specifics», «Monitoring on this host», «Rules» существующего файла сохранить, обновив Rules: fallback по четырём триггерам, замены как в шаблоне.

- [ ] **Step 3:** `.ccteam/briefs/executor.md` в каждом из четырёх проектов:

```markdown
# Бриф исполнителя (заполняет командир, передаётся в task целиком)

**Размер:** <мелочь|средняя|крупная> · **Полоса:** <A|B|C> · **Effort:** <ступень> · **Задача:** <id>
**План:** <абсолютный путь> · **Ветка:** task/<id> · **Worktree:** <абсолютный путь> · **Советник:** <sid или нет>

**Задача (критерий готовности дословно):**
<...>

**Границы:** файлы домена конфликта: <...>. Не трогать: <...>.

**Инкрементальность:** коммит на своей ветке после каждого законченного шага. После каждого шага дописать в секцию своей задачи в файле плана строку `Ход: <дата> <что сделано> <что дальше>`.

**Советник:** вопросы пачкой, один `session_dispatch` с `wait_seconds`. Спросить обязательно перед любым отклонением от плана. Совет не обязателен; если советник считает план кривым — не править план, эскалировать командиру.

**Формат результата (обязателен, возврат стоит как вся задача):**
1. Первая строка: `Статус: готово` или `Статус: не готово`.
2. Таблица файлов: путь — что изменено.
3. Прогнанные тесты и их выход (команда + итог).
4. Отклонения от плана и ответ советника.
5. Открытые вопросы.
Без кода и диффов в ответе.
```

- [ ] **Step 4:** Первый прогон по схеме = замер (спек §10); по его таблице owner двигает доли, правится только routing.md.

---

## Self-review

- **Покрытие спека:** §2 ростер → Task 4/5 текст + Task 8 routing; §3 размеры → Task 4 «Размер задачи»; §4 effort → «Полосы и effort»; §5 балансировка → «Балансировка» + Task 3 `status`; §5.1 → Task 1–3; §6 фолбек → «Фолбек»; §7 пре-гейт → «Шаг 5»; §8 бриф → «Шаг 4» + Task 8 файл; §9 потолки → «Полосы и effort»; §10 замер → «Мониторинг» хвост; §12 touch-points → Task 5/6/8; §13 решения D1–D6 отражены (D1 Task 3, D2/D3 «Размер задачи», D4 «Балансировка», D5 «Фолбек» триггер 3, D6 один PR).
- **Плейсхолдеры:** zh/en тексты шаблона не выписаны целиком — задан ru-источник и полная матрица обязательных подстрок на каждый язык, перевод = работа исполнителя Task 5 под литеральными локами; это тот же режим, что в плане v2.
- **Согласованность имён:** `tokens_24h_by_vendor` (Task 1) читается в Task 3; `vendor_quota_probe::global()` (Task 2) вызывается в Task 2 web и Task 3 dispatch; `render_section(paths, slug, hub, quotas)` — одна сигнатура в Task 3 для обоих вызовов; posture `fable`/`high` совпадает в `playbooks.ts` и тесте.
