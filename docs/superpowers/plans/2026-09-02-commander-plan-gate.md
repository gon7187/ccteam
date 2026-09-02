# Командир v2 (план Fable → гейт Sol → worktree → git-агент → двойной гейт): план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Перекроить четыре копии промпта «🎯 Командир» (Telegram вкомпилен + web zh/ru/en) под схему из spec, залочить контракт литеральными тестами, синхронизировать доки и governance, выставить колпаки нагрузки на машине owner.

**Architecture:** Чисто prompt-soft. Движок ноль правок: ни gateway, ни MCP schema, ни `status`/`session_list`, ни новых тулов. Весь поведенческий контракт живёт в тексте шаблона; тесты = строковые локи без LLM. Web-карта Commander (`playbooks.ts`) не меняется: lead по-прежнему Claude Opus max с capability-fallback на Codex.

**Tech Stack:** Rust (`ccteam-core` вкомпиленный шаблон + `#[cfg(test)]` локи), TypeScript (`i18n.ts` три языка, vitest), Markdown (доки, AGENTS.md), локальный конфиг WSL/cargo/ccteam.

**Spec:** `docs/superpowers/specs/2026-09-02-commander-plan-gate-design.md`

## Global Constraints

- Все новые документы репо на русском (owner 2026-09-02); commit messages и текст PR на английском.
- Рабочая ветка `dev`, worktree `/root/projects/ccteam-wt/dev`; главный worktree `/root/projects/ccteam` (main) остаётся чистым. Без bump версии.
- Разведчик: vendor `opencode`, модель `zai-coding-plan/glm-5.3-flash`, read-only, гасится после отчёта. Один Luna-fallback разведки, дальше стоп.
- Ростер и effort: все max; Luna не более 3 параллельно (было «до 10»).
- План: `<project>/.ccteam/plans/<YYYY-MM-DD>-<тема>.md`; поля задачи id, исполнитель, зависимости, файлы, критерий готовности; секция Amendments.
- Гейты: план (Sol) и код (свежие Opus + Sol), оба с потолком 2 круга и докладом из трёх абзацев.
- Советник = Sol-сессия, ревьюившая план; совет не обязателен; отклонение от плана только через советника; эскалация командиру; второй советник со списком задач; гасится после одобрения кода.
- Исполнение в worktree `../<repo>-wt/<id>`, ветка `task/<id>`, `--test-threads=4`, коммит на своей ветке; без git = последовательно, без git-агента, без `git init`.
- Git-агент Sonnet: `integration/<тема>`, полные проверки, конфликты сам, re-gate при задетых отревьюенных строках, PR draft→ready, merge commit; условие merge = оба одобрили **и** локальные проверки зелёные, CI только если реально крутится; tag/release вне.
- Мониторинг в точках решения; хост через `nproc`/`uptime`/`free -m`; пороги load1 > ядер, память < 15 процентов, swap; лестница из четырёх ступеней; живость по `stale`/`stuck`/15/30 минут; `waiting_approval` не зависание; гасить отработавших делегатов (`delegation.max_children`).
- Fallback: существующие фразы capability-правила сохраняются; Fable → Opus, любая роль Sol → свежий Opus.
- Проверки перед push: `cargo fmt --all -- --check`, `make check`, `cargo test -p ccteam-core`, `make web-check`, `.loop/verify/writeback.sh`.
- Написание: код (Task 1–2) выполняют субагенты dev; доки и governance (Task 3–4) от имени регламентной сессии Fable 5 (это она и ведёт план).

---

### Task 0: Подготовка ветки dev

**Files:**
- Worktree: `/root/projects/ccteam-wt/dev` (ветка `dev`, чистый, отстаёт от `origin/dev` на 29 коммитов)
- Move: `docs/superpowers/specs/2026-09-02-commander-plan-gate-design.md`, `docs/superpowers/plans/2026-09-02-commander-plan-gate.md` из главного worktree в dev-worktree

- [ ] **Step 1: Fast-forward dev**

Run:
```bash
git -C /root/projects/ccteam-wt/dev pull --ff-only origin dev
git -C /root/projects/ccteam-wt/dev log --oneline -1
```
Expected: HEAD = `e0031491 docs: clarify Commander scout fallback scope`.

- [ ] **Step 2: Перенести spec и план в dev-worktree, главный worktree очистить**

```bash
mkdir -p /root/projects/ccteam-wt/dev/docs/superpowers/specs /root/projects/ccteam-wt/dev/docs/superpowers/plans
mv /root/projects/ccteam/docs/superpowers/specs/2026-09-02-commander-plan-gate-design.md /root/projects/ccteam-wt/dev/docs/superpowers/specs/
mv /root/projects/ccteam/docs/superpowers/plans/2026-09-02-commander-plan-gate.md /root/projects/ccteam-wt/dev/docs/superpowers/plans/
git -C /root/projects/ccteam status --short
```
Expected: в главном worktree только `?? .mcp.json`.

- [ ] **Step 3: Коммит spec + план**

```bash
cd /root/projects/ccteam-wt/dev
git add docs/superpowers/specs/2026-09-02-commander-plan-gate-design.md docs/superpowers/plans/2026-09-02-commander-plan-gate.md
git commit -m "docs: add Commander v2 plan-gate spec and implementation plan"
```

---

### Task 1: Rust вкомпиленный Telegram-шаблон + локи в `config.rs`

**Files:**
- Modify: `crates/ccteam-core/src/config.rs:138-164` (`commander_quick_template()`)
- Modify (tests): `crates/ccteam-core/src/config.rs:896-973` (`default_im_quick_templates_include_six_templates`, `commander_template_gates_first_code_edit_on_glm_scout`) + новый тест

**Interfaces:**
- Consumes: `commander_quick_template() -> QuickTemplate` (сигнатура не меняется).
- Produces: тот же `QuickTemplate { label: "🎯 Командир", prefix }`; gateway (`crates/ccteam-im/src/gateway.rs:7567`) продолжает брать вкомпиленный prefix по label. Никаких новых типов.

- [ ] **Step 1: Обновить существующие локи под новый ростер**

В `default_im_quick_templates_include_six_templates` заменить строку
```rust
        assert!(templates[0].prefix.contains("до 10"));
```
на
```rust
        assert!(templates[0].prefix.contains("не более 3"));
        assert!(!templates[0].prefix.contains("до 10"));
```
В `commander_template_gates_first_code_edit_on_glm_scout` из массива фраз удалить три строки: `"второе мнение"`, `"никогда — основной код"`, `"финальные ревью Opus и Sol"` (GLM после разведки гасится, границ переиспользования больше нет). Остальные 15 фраз остаются.

- [ ] **Step 2: Добавить новый лок-тест v2 после `commander_template_gates_first_code_edit_on_glm_scout`**

```rust
    #[test]
    fn commander_template_runs_plan_gate_worktrees_git_agent_and_monitoring() {
        let prefix = commander_quick_template().prefix;
        // Порядок фаз: разведка → план → ревью плана → исполнение → интеграция → финальный гейт.
        let order = [
            "Шаг 1, разведка",
            "Шаг 2, план",
            "Шаг 3, ревью плана",
            "Шаг 4, исполнение",
            "Шаг 5, интеграция",
            "Шаг 6, финальный гейт",
            "Мониторинг:",
        ];
        let mut last = 0;
        for phase in order {
            let at = prefix.find(phase).unwrap_or_else(|| panic!("Commander v2 lacks phase {phase}"));
            assert!(at >= last, "phase {phase} is out of order");
            last = at;
        }
        for phrase in [
            // ростер
            "Планировщик — Claude Fable",
            "Ревьюер плана и советник — Codex Sol",
            "не более 3 параллельно",
            "Git-агент — Claude Sonnet",
            "советник в приёмке не участвует",
            // разведка гасится
            "останови разведчика через session_stop",
            // план
            ".ccteam/plans/",
            "id, исполнитель, зависимости, файлы, критерий готовности",
            "Amendments",
            "единственный источник истины",
            // гейт плана
            "правом блокировать",
            "потолок 2 круга",
            "позиция Fable, позиция Sol, что нужно решить",
            "останови Fable",
            "Пауз на человека нет",
            // исполнение
            "-wt/",
            "task/",
            "никогда в /tmp",
            "--test-threads=4",
            "не более 3 Luna",
            "sid советника",
            "обязательно перед любым отклонением от плана",
            "совет не обязателен к исполнению",
            "эскалирует командиру",
            "дописываешь поправку в Amendments",
            "свежий Fable перепланирует",
            "явным списком задач",
            "Проект без git",
            "git init не делай",
            // git-агент
            "git-агента Claude Sonnet",
            "integration/",
            "уже отревьюенные строки",
            "draft → ready",
            "merge commit, не squash",
            "оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный",
            "CI — третье условие, только если он в проекте есть",
            "Tag и release вне",
            // финальный гейт
            "свежие сессии — Claude Opus и Codex Sol",
            "теми же гейт-сессиями",
            "останови советника",
            // мониторинг
            "перед каждым spawn и на каждом уведомлении о завершении",
            "free -m",
            "session_list (status, last_activity_seconds, waiting_approval)",
            "15 процентов",
            "не спавнь новых исполнителей",
            "session_stop и верни задачу в план",
            "git-агента посреди интеграции и гейт не трогай",
            "stale",
            "stuck",
            "30 минут",
            "waiting_approval: true зависшей не считается",
            "delegation.max_children",
            "Fable после одобрения плана",
            // fallback ролей
            "для Fable замена — Claude Opus",
            "для любой роли Sol — свежий Claude Opus",
        ] {
            assert!(prefix.contains(phrase), "Commander v2 contract lacks {phrase}");
        }
        assert!(prefix.contains("Fallback разрешён только если явный ответ status"));
        assert!(prefix.ends_with("Задача:"));
    }
```

- [ ] **Step 3: Убедиться, что тесты красные**

Run: `cd /root/projects/ccteam-wt/dev && cargo test -p ccteam-core commander_template`
Expected: FAIL (`lacks Планировщик — Claude Fable`, `contains("не более 3")`).

- [ ] **Step 4: Заменить тело `commander_quick_template()` целиком**

```rust
pub fn commander_quick_template() -> QuickTemplate {
    QuickTemplate {
        label: COMMANDER_QUICK_TEMPLATE_LABEL.to_string(),
        prefix: concat!(
            "Сначала вызови status и используй только доступные в проекте vendor, модели и уровни effort. Делегируй работу через session_spawn / session_dispatch. Ты — командир: предпочтительно Claude Opus с максимальным доступным effort. Занимайся декомпозицией, распределением, контролем и приёмкой; сам не планируй и не пиши код. Если Opus доступен, а текущая сессия не Opus, создай отдельного Opus-командира и передай ему задачу; если status или capability-ошибка уже доказали недоступность Opus и текущая сессия намеренно запущена как Codex fallback, возьми роль командира на себя и не создавай Opus повторно. Не меняй vendor только потому, что spawn отклонён; fallback разрешён лишь по capability-правилу ниже.\n\n",
            "Состав команды (все с максимальным доступным effort, кроме особо оговорённых):\n",
            "• Разведчик — OpenCode GLM (модель zai-coding-plan/glm-5.3-flash), только чтение (read-only); effort — верхняя ступень, которую status реально объявил для opencode.\n",
            "• Планировщик — Claude Fable: пишет план в файл.\n",
            "• Ревьюер плана и советник — Codex Sol: одна сессия на обе роли.\n",
            "• Исполнители — Codex Luna (не более 3 параллельно), Codex Terra (фронтенд, визуальные задачи, сложная реализация), Claude Sonnet (документация, граф, недорогие задачи), Claude Haiku (быстрый поиск, короткие правки). Кому какая задача — помечает Fable в плане; без метки код, исследования и сетевое окружение идут Luna.\n",
            "• Git-агент — Claude Sonnet: интеграция веток, полные проверки, push, PR, merge.\n",
            "• Финальный гейт — свежий Claude Opus и свежий Codex Sol, две независимые сессии; советник в приёмке не участвует.\n\n",
            "Шаг 1, разведка: до первой правки кода запусти через session_spawn разведчика opencode с моделью zai-coding-plan/glm-5.3-flash с задачей read-only; effort бери верхней ступенью, которую status реально объявил для opencode (max — только если vendor его действительно объявил). Дождись завершения (wait_seconds или session_dispatch + session_collect) и получи отчёт: 2–5 прецедентов с GitHub — похожие репозитории, сценарии, куски кода; для каждого URL, pin (commit sha или tag), релевантный путь, license, применимые идеи и несовместимости, в конце — независимая рекомендация разведчика и риски. Из прецедентов с несовместимой license бери только идеи, код не копируй. Получив отчёт, останови разведчика через session_stop. Новая верхнеуровневая задача с кодом проходит разведку заново.\n",
            "Если пригодную GLM-сессию поднять не удалось (spawn явно упал, модель недоступна или подтверждено, что первый результат непригоден или не соответствует контракту) — сделай ровно одну попытку разведки через Codex Luna с максимальным объявленным effort; если и Luna не справилась — останови работу над кодом и честно сообщи. При неизвестном исходе spawn или dispatch сначала сверься через session_list и session_collect, жива ли GLM-сессия и есть ли у неё вывод; трать попытку Luna только подтвердив, что пригодной сессии нет. Это исключение только для предкодовой разведки: общий capability-fallback ниже и правила fallback для lead и остальных ролей не меняются.\n\n",
            "Шаг 2, план: передай отчёт разведки Claude Fable и получи план в файле <project>/.ccteam/plans/<YYYY-MM-DD>-<тема>.md. Каждая задача плана: id, исполнитель, зависимости, файлы, критерий готовности. В конце файла секция Amendments: любая поправка после одобрения — отдельная запись с датой, автором и причиной, тело плана не переписывается. План в файле — единственный источник истины.\n",
            "Шаг 3, ревью плана: свежая сессия Codex Sol ревьюит план с правом блокировать. Отклонила — Fable правит, Sol смотрит заново; потолок 2 круга. Третье расхождение — останови работу и пришли пользователю доклад из трёх абзацев: позиция Fable, позиция Sol, что нужно решить. После одобрения останови Fable; сессия Sol остаётся советником. Пауз на человека нет: план лежит в файле, пользователь может остановить в любой момент.\n\n",
            "Шаг 4, исполнение: если проект под git, каждая задача плана идёт в свой git worktree ../<repo>-wt/<id> на ветке task/<id> от базовой ветки проекта (ветка разработки по правилам проекта, иначе default branch), никогда в /tmp; исполнитель гоняет затронутые тесты с --test-threads=4 и коммитит на своей ветке сам. Спавни исполнителей по меткам плана с учётом зависимостей, не более 3 Luna одновременно; в каждую задачу положи путь плана, id задачи, sid советника и правила советника. Правила советника, одинаковые для всех исполнителей: спросить советника через session_dispatch с wait_seconds обязательно перед любым отклонением от плана и по желанию, когда застрял; совет не обязателен к исполнению; если советник считает план кривым — исполнитель план не правит, а эскалирует командиру. По эскалации ты сам дописываешь поправку в Amendments, советник её визирует; если советник говорит, что поправка ломает структуру плана — свежий Fable перепланирует затронутый кусок, Sol ревьюит его тем же циклом с потолком 2 круга. Если по session_list очередь к советнику тормозит — подними второго Sol-советника с файлом плана и явным списком задач, которые он обслуживает. Исполнитель, не закрывший критерий готовности, докладывает тебе; ты решаешь: повторить, спросить советника или поправка плана. Проект без git: worktree невозможны, исполнители работают по одному последовательно прямо в каталоге, git-агент не поднимается, git init не делай, укажи это в итоге.\n\n",
            "Шаг 5, интеграция: когда все ветки задач закоммичены, подними git-агента Claude Sonnet. Он сливает ветки задач в integration/<тема> от базовой ветки и гоняет полный набор проверок проекта по его CLAUDE.md/AGENTS.md; красное — ошибка интеграции, идёт тебе. Конфликты он разруливает сам и перегоняет проверки; если разрешение тронуло уже отревьюенные строки — верни diff финальному гейту. После одобрения гейтом git-агент вливает integration/<тема> в базовую ветку, пушит, открывает или обновляет PR в main (draft → ready, описание на английском) и делает merge в main (merge commit, не squash) — только когда оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный; CI — третье условие, только если он в проекте есть и реально запускается, иначе не ждать. Worktree убираются после интеграции, ветки задач — после merge PR. Tag и release вне этой задачи: только по отдельной явной команде owner.\n\n",
            "Шаг 6, финальный гейт: две независимые свежие сессии — Claude Opus и Codex Sol, обе с максимальным доступным effort, каждая получает файл плана и diff integration/<тема> против базовой ветки. Решение принято только когда оба одобряют одну и ту же ревизию; при расхождении верни замечания исполнителю, исправь и повтори оба ревью теми же гейт-сессиями; потолок 2 круга, дальше — доклад пользователю из трёх абзацев. После одобрения останови советника.\n\n",
            "Мониторинг: ты не тикаешь — проверяй перед каждым spawn и на каждом уведомлении о завершении. Хост — nproc, uptime, free -m через свой shell; сессии — session_list (status, last_activity_seconds, waiting_approval). Пороги по умолчанию (routing.md проекта может переопределить): load1 выше числа ядер, свободной памяти меньше 15 процентов, swap в работе. При перегрузе по возрастанию: 1) не спавнь новых исполнителей, пока не отпустит; 2) через dispatch попроси работающих снизить -j и --test-threads и отложить тяжёлые тесты; 3) останови самого свежего исполнителя через session_stop и верни задачу в план (ветка в worktree остаётся); 4) git-агента посреди интеграции и гейт не трогай никогда. Живость: status stale — глянь; stuck или тишина от 15 минут — dispatch «доложи статус» с wait_seconds; молчание от 30 минут — session_stop и respawn с той же задачей из плана; процесс вышел с ошибкой — один dispatch на resume по sid, снова упал — свежий spawn. waiting_approval: true зависшей не считается. Отработавших делегатов (разведчик после отчёта, Fable после одобрения плана, исполнители после интеграции их веток) останавливай: delegation.max_children считает активных прямых детей, и idle-сессия занимает слот.\n\n",
            "Fallback разрешён только если явный ответ status либо capability-ошибка spawn с error_code=vendor_unavailable, error_code=model_unavailable или error_code=effort_unavailable доказывает, что нужный vendor, модель или effort недоступны: сделай ровно одну попытку — для Fable замена — Claude Opus, для любой роли Sol — свежий Claude Opus, для остальных — Codex с лучшей доступной моделью и максимальным уровнем effort из status. Если spawn вернул любую другую ошибку — авторизация/ACL, квота или бюджет, depth/cycle guard, timeout, network/transport, internal либо общий отказ — не запускай fallback и не повторяй вслепую: верни исходную ошибку. Не угадывай wire-токен max: используй верхнюю ступень, которую реально объявил vendor. Уведомления о завершении возвращаются тебе; ты собираешь итог.\n\n",
            "Задача:"
        )
        .to_string(),
    }
}
```

- [ ] **Step 5: Тесты зелёные**

Run: `cd /root/projects/ccteam-wt/dev && cargo test -p ccteam-core quick_template && cargo test -p ccteam-core commander_template`
Expected: все PASS (включая `im_quick_templates_yaml_override_parses`, `default_im_quick_templates_include_six_templates`).

- [ ] **Step 6: fmt + clippy по crate**

Run: `cargo fmt --all -- --check && cargo clippy -p ccteam-core --all-targets -- -D warnings`
Expected: чисто.

- [ ] **Step 7: Коммит**

```bash
git add crates/ccteam-core/src/config.rs
git commit -m "feat(core): Commander v2 prompt — Fable plan, Sol gate+advisor, worktrees, git agent, monitoring"
```

---

### Task 2: Web-шаблон zh/ru/en + локи vitest

**Files:**
- Modify: `crates/ccteam-web/web/src/lib/i18n.ts:43-49` (zh `tplCommanderD`/`tplCommanderP`), `:515-521` (ru), `:558-564` (en)
- Modify (tests): `crates/ccteam-web/web/src/lib/i18n.test.ts:62-66`, `crates/ccteam-web/web/src/lib/playbooks.test.ts:86-167`
- Не трогать: `playbooks.ts` (карточка `commander`, vendors `["claude","codex","opencode"]`, posture Opus max), `HomeView.test.tsx`, `CharterPanel.test.tsx` (проверяют только наличие карточки)

**Interfaces:**
- Consumes: ключи `I18N.<lang>.tplCommanderT/D/P` (имена не меняются; `applyPlaybook("commander", lang)` возвращает `text: I18N[lang].tplCommanderP`).
- Produces: те же ключи с новым текстом. Тест `covers zh and en with the same key set` требует одинаковый набор ключей zh/en — новых ключей не добавляем.

- [ ] **Step 1: Обновить существующие локи в `playbooks.test.ts`**

В тесте `the commander prefill carries the full roster, dual gate, and capability-only Codex fallback`:
- `expect(prompt).toContain("до 10");` → `expect(prompt).toContain("не более 3"); expect(prompt).not.toContain("до 10");`
- В матрице scout-фраз удалить последний элемент каждого языка: zh `"绝不承担主编码"`, ru `"никогда — основной код"`, en `"never primary coding"`.

- [ ] **Step 2: Добавить новый vitest-лок v2 в `playbooks.test.ts` сразу после этого теста**

```ts
  it("the commander prefill carries the v2 contract in every language: plan file, Sol gate+advisor, worktrees, git agent, dual fresh gate, monitoring", () => {
    const matrix = {
      zh: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id、执行者、依赖、文件、完成标准",
        "上限 2 轮", "Fable 的立场、Sol 的立场、需要决定什么", "不设人工暂停",
        "最多 3 个并行", "-wt/", "task/", "integration/", "--test-threads=4", "顾问 sid",
        "任何偏离计划之前必须问", "建议不具约束力", "升级给指挥官", "明确清单",
        "git 代理 Claude Sonnet", "merge commit", "两位终审都批准同一修订且全量本地检查为绿", "CI 只有在项目里存在", "tag 与 release 不在本任务内",
        "全新的 Claude Opus 和全新的 Codex Sol", "顾问不参与验收",
        "session_list", "free -m", "15%", "session_stop", "stale", "stuck", "30 分钟", "waiting_approval: true 不算挂起", "delegation.max_children",
        "用 session_stop 停止侦察员", "git init", "Fable 改用 Claude Opus", "任何 Sol 角色改用全新的 Claude Opus",
      ],
      ru: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id, исполнитель, зависимости, файлы, критерий готовности",
        "потолок 2 круга", "позиция Fable, позиция Sol, что нужно решить", "Пауз на человека нет",
        "не более 3 параллельно", "-wt/", "task/", "integration/", "--test-threads=4", "sid советника",
        "обязательно перед любым отклонением от плана", "совет не обязателен к исполнению", "эскалирует командиру", "явным списком задач",
        "git-агента Claude Sonnet", "merge commit, не squash", "оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный", "CI — третье условие, только если он в проекте есть", "Tag и release вне",
        "свежие сессии — Claude Opus и Codex Sol", "советник в приёмке не участвует",
        "session_list", "free -m", "15 процентов", "session_stop", "stale", "stuck", "30 минут", "waiting_approval: true зависшей не считается", "delegation.max_children",
        "останови разведчика через session_stop", "git init не делай", "Fable — Claude Opus", "любой роли Sol — свежий Claude Opus",
      ],
      en: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id, implementer, dependencies, files, definition of done",
        "cap of 2 rounds", "Fable's position, Sol's position, what needs deciding", "No human pause",
        "at most 3 in parallel", "-wt/", "task/", "integration/", "--test-threads=4", "advisor's sid",
        "mandatory before any deviation from the plan", "advice is not binding", "escalates to the commander", "explicit list of tasks",
        "git agent, Claude Sonnet", "merge commit, not squash", "both final reviewers approved the same revision and the full local check suite is green", "CI is a third condition only if the project has one", "Tag and release are outside",
        "fresh sessions — Claude Opus and Codex Sol", "advisor does not take part in acceptance",
        "session_list", "free -m", "15 percent", "session_stop", "stale", "stuck", "30 minutes", "waiting_approval: true does not count as hung", "delegation.max_children",
        "stop the scout with session_stop", "never run git init", "Claude Opus for Fable", "fresh Claude Opus for any Sol role",
      ],
    } as const;
    for (const [lang, phrases] of Object.entries(matrix) as [keyof typeof matrix, readonly string[]][]) {
      const prompt = I18N[lang].tplCommanderP;
      for (const phrase of phrases) expect(prompt, `${lang}: ${phrase}`).toContain(phrase);
      // Порядок фаз держится во всех языках.
      const order = lang === "zh"
        ? ["第 1 步", "第 2 步", "第 3 步", "第 4 步", "第 5 步", "第 6 步", "监控"]
        : lang === "ru"
          ? ["Шаг 1", "Шаг 2", "Шаг 3", "Шаг 4", "Шаг 5", "Шаг 6", "Мониторинг"]
          : ["Step 1", "Step 2", "Step 3", "Step 4", "Step 5", "Step 6", "Monitoring"];
      let last = -1;
      for (const marker of order) {
        const at = prompt.indexOf(marker);
        expect(at, `${lang}: ${marker}`).toBeGreaterThan(last);
        last = at;
      }
      expect(I18N[lang].tplCommanderD).toContain("Fable");
      expect(I18N[lang].tplCommanderD).toContain("Sol");
    }
  });
```

- [ ] **Step 3: В `i18n.test.ts` расширить лок GLM-модели**

Заменить тест `keeps the Commander GLM scout model in every language` на:
```ts
  it("keeps the Commander GLM scout model and plan-file path in every language", () => {
    for (const lang of ["zh", "ru", "en"] as const) {
      expect(t(lang, "tplCommanderP")).toContain("zai-coding-plan/glm-5.3-flash");
      expect(t(lang, "tplCommanderP")).toContain(".ccteam/plans/");
    }
  });
```

- [ ] **Step 4: Убедиться, что vitest красный**

Run: `cd /root/projects/ccteam-wt/dev/crates/ccteam-web/web && npx vitest run src/lib/playbooks.test.ts src/lib/i18n.test.ts`
Expected: FAIL на `не более 3` и `.ccteam/plans/`.

- [ ] **Step 5: Заменить ru `tplCommanderD` и `tplCommanderP` (`i18n.ts:515-521`)**

```ts
  tplCommanderD: "Opus руководит; GLM ведёт read-only разведку; Fable пишет план в файл; Sol ставит гейт плана и советует исполнителям; Luna, Terra, Sonnet и Haiku исполняют в worktree; git-агент Sonnet интегрирует и мержит; свежие Opus + Sol ставят двойной финальный гейт; только явно недоступные роли переходят на замену",
  tplCommanderP:
    "Сначала вызови status и используй только доступные в проекте vendor, модели и уровни effort. Делегируй работу через session_spawn / session_dispatch. Ты — командир: предпочтительно Claude Opus с максимальным доступным effort. Занимайся декомпозицией, распределением, контролем и приёмкой; сам не планируй и не пиши код. Если Opus доступен, а текущая сессия не Opus, создай отдельного Opus-командира и передай ему задачу; если status или capability-ошибка уже доказали недоступность Opus и текущая сессия намеренно запущена как Codex fallback, возьми роль командира на себя и не создавай Opus повторно. Не меняй vendor только потому, что spawn отклонён; fallback разрешён лишь по capability-правилу ниже.\n\n" +
    "Состав команды (все с максимальным доступным effort, кроме особо оговорённых):\n• Разведчик — OpenCode GLM (модель zai-coding-plan/glm-5.3-flash), только чтение (read-only); effort — верхняя ступень, которую status реально объявил для opencode.\n• Планировщик — Claude Fable: пишет план в файл.\n• Ревьюер плана и советник — Codex Sol: одна сессия на обе роли.\n• Исполнители — Codex Luna (не более 3 параллельно), Codex Terra (фронтенд, визуальные задачи, сложная реализация), Claude Sonnet (документация, граф, недорогие задачи), Claude Haiku (быстрый поиск, короткие правки). Кому какая задача — помечает Fable в плане; без метки код, исследования и сетевое окружение идут Luna.\n• Git-агент — Claude Sonnet: интеграция веток, полные проверки, push, PR, merge.\n• Финальный гейт — свежий Claude Opus и свежий Codex Sol, две независимые сессии; советник в приёмке не участвует.\n\n" +
    "Шаг 1, разведка: до первой правки кода запусти через session_spawn разведчика opencode с моделью zai-coding-plan/glm-5.3-flash с задачей read-only; effort бери верхней ступенью, которую status реально объявил для opencode (max — только если vendor его действительно объявил). Дождись завершения (wait_seconds или session_dispatch + session_collect) и получи отчёт: 2–5 прецедентов с GitHub — похожие репозитории, сценарии, куски кода; для каждого URL, pin (commit sha или tag), релевантный путь, license, применимые идеи и несовместимости, в конце — независимая рекомендация разведчика и риски. Из прецедентов с несовместимой license бери только идеи, код не копируй. Получив отчёт, останови разведчика через session_stop. Новая верхнеуровневая задача с кодом проходит разведку заново.\n" +
    "Если пригодную GLM-сессию поднять не удалось (spawn явно упал, модель недоступна, подтверждено, что первый ответ пуст, или подтверждено, что ответ не соответствует требованиям к отчёту) — сделай ровно одну попытку разведки через Codex Luna с максимальным объявленным effort; если и Luna не справилась — останови работу над кодом и честно сообщи. Это исключение только для разведки перед кодом по отношению к общему capability-fallback ниже; правила fallback для lead и остальных ролей не меняются. При неизвестном исходе spawn или dispatch сначала сверься через session_list и session_collect, жива ли GLM-сессия и есть ли у неё вывод; трать попытку Luna только подтвердив, что пригодной сессии нет.\n\n" +
    "Шаг 2, план: передай отчёт разведки Claude Fable и получи план в файле <project>/.ccteam/plans/<YYYY-MM-DD>-<тема>.md. Каждая задача плана: id, исполнитель, зависимости, файлы, критерий готовности. В конце файла секция Amendments: любая поправка после одобрения — отдельная запись с датой, автором и причиной, тело плана не переписывается. План в файле — единственный источник истины.\n" +
    "Шаг 3, ревью плана: свежая сессия Codex Sol ревьюит план с правом блокировать. Отклонила — Fable правит, Sol смотрит заново; потолок 2 круга. Третье расхождение — останови работу и пришли пользователю доклад из трёх абзацев: позиция Fable, позиция Sol, что нужно решить. После одобрения останови Fable; сессия Sol остаётся советником. Пауз на человека нет: план лежит в файле, пользователь может остановить в любой момент.\n\n" +
    "Шаг 4, исполнение: если проект под git, каждая задача плана идёт в свой git worktree ../<repo>-wt/<id> на ветке task/<id> от базовой ветки проекта (ветка разработки по правилам проекта, иначе default branch), никогда в /tmp; исполнитель гоняет затронутые тесты с --test-threads=4 и коммитит на своей ветке сам. Спавни исполнителей по меткам плана с учётом зависимостей, не более 3 Luna одновременно; в каждую задачу положи путь плана, id задачи, sid советника и правила советника. Правила советника, одинаковые для всех исполнителей: спросить советника через session_dispatch с wait_seconds обязательно перед любым отклонением от плана и по желанию, когда застрял; совет не обязателен к исполнению; если советник считает план кривым — исполнитель план не правит, а эскалирует командиру. По эскалации ты сам дописываешь поправку в Amendments, советник её визирует; если советник говорит, что поправка ломает структуру плана — свежий Fable перепланирует затронутый кусок, Sol ревьюит его тем же циклом с потолком 2 круга. Если по session_list очередь к советнику тормозит — подними второго Sol-советника с файлом плана и явным списком задач, которые он обслуживает. Исполнитель, не закрывший критерий готовности, докладывает тебе; ты решаешь: повторить, спросить советника или поправка плана. Проект без git: worktree невозможны, исполнители работают по одному последовательно прямо в каталоге, git-агент не поднимается, git init не делай, укажи это в итоге.\n\n" +
    "Шаг 5, интеграция: когда все ветки задач закоммичены, подними git-агента Claude Sonnet. Он сливает ветки задач в integration/<тема> от базовой ветки и гоняет полный набор проверок проекта по его CLAUDE.md/AGENTS.md; красное — ошибка интеграции, идёт тебе. Конфликты он разруливает сам и перегоняет проверки; если разрешение тронуло уже отревьюенные строки — верни diff финальному гейту. После одобрения гейтом git-агент вливает integration/<тема> в базовую ветку, пушит, открывает или обновляет PR в main (draft → ready, описание на английском) и делает merge в main (merge commit, не squash) — только когда оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный; CI — третье условие, только если он в проекте есть и реально запускается, иначе не ждать. Worktree убираются после интеграции, ветки задач — после merge PR. Tag и release вне этой задачи: только по отдельной явной команде owner.\n\n" +
    "Шаг 6, финальный гейт: две независимые свежие сессии — Claude Opus и Codex Sol, обе с максимальным доступным effort, каждая получает файл плана и diff integration/<тема> против базовой ветки. Решение принято только когда оба одобряют одну и ту же ревизию; при расхождении верни замечания исполнителю, исправь и повтори оба ревью теми же гейт-сессиями; потолок 2 круга, дальше — доклад пользователю из трёх абзацев. После одобрения останови советника.\n\n" +
    "Мониторинг: ты не тикаешь — проверяй перед каждым spawn и на каждом уведомлении о завершении. Хост — nproc, uptime, free -m через свой shell; сессии — session_list (status, last_activity_seconds, waiting_approval). Пороги по умолчанию (routing.md проекта может переопределить): load1 выше числа ядер, свободной памяти меньше 15 процентов, swap в работе. При перегрузе по возрастанию: 1) не спавнь новых исполнителей, пока не отпустит; 2) через dispatch попроси работающих снизить -j и --test-threads и отложить тяжёлые тесты; 3) останови самого свежего исполнителя через session_stop и верни задачу в план (ветка в worktree остаётся); 4) git-агента посреди интеграции и гейт не трогай никогда. Живость: status stale — глянь; stuck или тишина от 15 минут — dispatch «доложи статус» с wait_seconds; молчание от 30 минут — session_stop и respawn с той же задачей из плана; процесс вышел с ошибкой — один dispatch на resume по sid, снова упал — свежий spawn. waiting_approval: true зависшей не считается. Отработавших делегатов (разведчик после отчёта, Fable после одобрения плана, исполнители после интеграции их веток) останавливай: delegation.max_children считает активных прямых детей, и idle-сессия занимает слот.\n\n" +
    "Только явное сообщение status или capability-ошибка spawn о том, что нужные vendor, модель или effort не подключены или недоступны, разрешает одну попытку замены: для Fable — Claude Opus, для любой роли Sol — свежий Claude Opus, для остальных — лучшая доступная модель Codex с максимальным effort из status. При ошибке аутентификации, ACL, тайм-ауте, квоте, бюджете, ограничении глубины делегирования, цикле, сетевой или внутренней ошибке либо общем отказе spawn работай fail closed: не повторяй запрос, не применяй fallback и сообщи исходную ошибку. Не угадывай wire-токен max: используй верхнюю ступень, которую реально объявил vendor. Уведомления о завершении возвращаются тебе; ты собираешь итог.\n\nЗадача:",
```

- [ ] **Step 6: Заменить en `tplCommanderD` и `tplCommanderP` (`i18n.ts:558-564`)**

```ts
    tplCommanderD: "Opus commands; GLM scouts read-only; Fable writes the plan to a file; Sol gates the plan and advises implementers; Luna, Terra, Sonnet, and Haiku execute in worktrees; a Sonnet git agent integrates and merges; a fresh Opus + Sol pair forms the dual final gate; only explicitly unavailable roles fall back",
    tplCommanderP:
      "Call status first and use only vendors, models, and effort levels available in this project. Delegate through session_spawn / session_dispatch. You are the commander, preferably Claude Opus at the highest available effort. Own decomposition, delegation, control, and acceptance; do not plan or write code yourself. If Opus is available and the current session is not Opus, create a separate Opus commander and hand it the task; if status or a capability error has already proved Opus unavailable and this session is the deliberate Codex fallback, take the commander role yourself and do not spawn Opus again. Do not switch vendors merely because spawn was rejected; fallback is allowed only by the capability rule below.\n\n" +
      "Team (everyone at the highest available effort unless stated otherwise):\n• Scout — OpenCode GLM (model zai-coding-plan/glm-5.3-flash), read-only; effort = the top level status actually advertises for opencode.\n• Planner — Claude Fable: writes the plan to a file.\n• Plan reviewer and advisor — Codex Sol: one session for both roles.\n• Implementers — Codex Luna (at most 3 in parallel), Codex Terra (frontend, visual work, complex implementation), Claude Sonnet (documentation, graphs, cost-effective work), Claude Haiku (fast search, quick fixes). Fable tags each task with its implementer in the plan; untagged code, research, and network work goes to Luna.\n• Git agent — Claude Sonnet: branch integration, full checks, push, PR, merge.\n• Final gate — a fresh Claude Opus and a fresh Codex Sol, two independent sessions; the advisor does not take part in acceptance.\n\n" +
      "Step 1, scouting: before the first code edit spawn an opencode scout through session_spawn with model zai-coding-plan/glm-5.3-flash and a read-only task; take the highest effort status actually advertises for opencode (use max only if the vendor really advertises it). Wait for completion (wait_seconds or session_dispatch + session_collect) and obtain 2–5 GitHub precedents — similar repositories, scenarios, code fragments — each with URL, pin (commit sha or tag), relevant path, license, reusable ideas, and incompatibilities, ending with the scout's independent recommendation and risks. Use only ideas from incompatible-license precedents, never copy code. Once the report is in, stop the scout with session_stop. A new top-level coding task scouts again.\n" +
      "If no usable GLM session can be started (spawn explicitly fails, the model is unavailable, the first response is confirmed empty, or the response is confirmed non-compliant with the report requirements), make exactly one scouting attempt through Codex Luna at its highest advertised effort; if Luna also fails, stop coding and report honestly. This exception applies only to pre-code scouting against the general capability fallback below; lead and other-role fallback rules remain unchanged. When a spawn or dispatch outcome is unknown, reconcile first through session_list and session_collect whether the GLM session is alive and has output; spend the Luna attempt only after confirming no usable session exists.\n\n" +
      "Step 2, plan: hand the scouting report to Claude Fable and get the plan as a file at <project>/.ccteam/plans/<YYYY-MM-DD>-<topic>.md. Every task in the plan carries: id, implementer, dependencies, files, definition of done. The file ends with an Amendments section: every change after approval is a separate entry with date, author, and reason; the plan body is never rewritten. The plan file is the single source of truth.\n" +
      "Step 3, plan review: a fresh Codex Sol session reviews the plan with the power to block. If it rejects, Fable revises and Sol reviews again; cap of 2 rounds. On the third disagreement stop and send the user a three-paragraph report: Fable's position, Sol's position, what needs deciding. After approval stop Fable; the Sol session stays on as the advisor. No human pause: the plan is in the file and the user can stop at any moment.\n\n" +
      "Step 4, execution: if the project is under git, each plan task runs in its own git worktree ../<repo>-wt/<id> on branch task/<id> from the project's base branch (its development branch by the project's rules, otherwise the default branch), never in /tmp; the implementer runs the affected tests with --test-threads=4 and commits on its own branch. Spawn implementers by the plan's tags respecting dependencies, at most 3 Luna at once; give every task the plan path, the task id, the advisor's sid, and the advisor rules. Advisor rules, identical for every implementer: ask the advisor through session_dispatch with wait_seconds — mandatory before any deviation from the plan, optional when stuck; advice is not binding; if the advisor thinks the plan is wrong there, the implementer does not edit the plan but escalates to the commander. On escalation you write the amendment into Amendments yourself and the advisor signs it off; if the advisor says the amendment breaks the plan's structure, a fresh Fable replans the affected part and Sol reviews it under the same cap of 2 rounds. If session_list shows the advisor's queue lagging, spawn a second Sol advisor with the plan file and an explicit list of tasks it serves. An implementer that cannot meet its definition of done reports to you; you decide: retry, ask the advisor, or amend the plan. Project without git: no worktrees, implementers work one at a time directly in the directory, no git agent, never run git init, and say so in the summary.\n\n" +
      "Step 5, integration: once every task branch is committed, spawn the git agent, Claude Sonnet. It merges the task branches into integration/<topic> from the base branch and runs the project's full check suite per its CLAUDE.md/AGENTS.md; red means an integration failure and comes to you. It resolves conflicts itself and reruns the checks; if the resolution touched already-reviewed lines, send the diff back to the final gate. After the gate approves, the git agent merges integration/<topic> into the base branch, pushes, opens or updates the PR to main (draft → ready, description in English), and merges into main (merge commit, not squash) — only when both final reviewers approved the same revision and the full local check suite is green; CI is a third condition only if the project has one that actually runs, otherwise do not wait. Worktrees are removed after integration, task branches after the PR merges. Tag and release are outside this task: only on a separate explicit owner command.\n\n" +
      "Step 6, final gate: two independent fresh sessions — Claude Opus and Codex Sol, both at their highest available effort, each given the plan file and the diff of integration/<topic> against the base branch. Accept only when both approve the same revision; on disagreement, return findings to the implementer, fix them, and repeat both reviews with the same gate sessions; cap of 2 rounds, then the three-paragraph report to the user. After approval stop the advisor.\n\n" +
      "Monitoring: you do not tick — check before every spawn and on every completion notification. Host via nproc, uptime, free -m in your own shell; sessions via session_list (status, last_activity_seconds, waiting_approval). Default thresholds (the project's routing.md may override): load1 above the core count, free memory below 15 percent, swap in use. Under overload, in ascending order: 1) spawn no new implementers until it clears; 2) ask running ones through dispatch to lower -j and --test-threads and postpone heavy tests; 3) stop the newest implementer with session_stop and return its task to the plan (its worktree branch stays); 4) never touch the git agent mid-integration or the gate. Liveness: status stale — take a look; stuck or 15 minutes of silence — dispatch a \"report status\" with wait_seconds; 30 minutes of silence — session_stop and respawn with the same task from the plan; process exited with an error — one dispatch to resume by sid, fails again — a fresh spawn. waiting_approval: true does not count as hung. Stop delegates that are done (the scout after its report, Fable after plan approval, implementers after their branches are integrated): delegation.max_children counts active direct children, and an idle session holds a slot.\n\n" +
      "Only an explicit status result or spawn capability error reporting the required vendor, model, or effort as disconnected or unavailable permits one substitution attempt: Claude Opus for Fable, a fresh Claude Opus for any Sol role, otherwise the best available Codex model and highest effort from status. On authentication, ACL, timeout, quota, budget, delegation-depth, cycle, network, internal, or general spawn failures, fail closed: do not retry or substitute, and surface the original failure. Never guess the wire token `max`; use the top level the vendor actually advertises. Completion notifications return to you; assemble the final result.\n\nTask:",
```

- [ ] **Step 7: Заменить zh `tplCommanderD` и `tplCommanderP` (`i18n.ts:43-49`)**

```ts
    tplCommanderD: "Opus 统筹；GLM 只读侦察；Fable 把计划写成文件；Sol 评审计划并为执行者提供顾问；Luna/Terra/Sonnet/Haiku 在 worktree 中执行；Sonnet git 代理集成并合并；全新 Opus + Sol 双重终审；仅明确不可用的角色回退",
    tplCommanderP:
      "先调用 status，只使用本项目可用的 vendor、模型和 effort 等级。通过 session_spawn / session_dispatch 分派工作。你是指挥官，优先使用最高可用 effort 的 Claude Opus；只负责拆解、分工、控制和验收，自己不做规划、不写代码。如果 Opus 可用而当前会话不是 Opus，请创建独立的 Opus 指挥官并把任务交给它；如果 status 或 capability 错误已证明 Opus 不可用且当前会话是有意启动的 Codex fallback，就由当前会话自己担任指挥官，不要再创建 Opus。不要仅因 spawn 被拒绝就切换 vendor；fallback 只按下面的 capability 规则执行。\n\n" +
      "团队（除特别说明外均用最高可用 effort）：\n• 侦察员 — OpenCode GLM（模型 zai-coding-plan/glm-5.3-flash），只读（read-only）；effort 取 status 为 opencode 实际声明的最高档。\n• 规划者 — Claude Fable：把计划写成文件。\n• 计划评审兼顾问 — Codex Sol：一个会话承担两个角色。\n• 执行者 — Codex Luna（最多 3 个并行）、Codex Terra（前端、视觉任务、复杂实现）、Claude Sonnet（文档、图谱、低成本任务）、Claude Haiku（快速搜索、短修复）。每个任务的执行者由 Fable 在计划中标注；未标注的代码、研究和网络环境工作交给 Luna。\n• git 代理 — Claude Sonnet：分支集成、全量检查、push、PR、merge。\n• 终审门 — 全新的 Claude Opus 和全新的 Codex Sol，两个独立会话；顾问不参与验收。\n\n" +
      "第 1 步，侦察：在第一次改动代码之前，通过 session_spawn 启动 opencode 侦察员，模型 zai-coding-plan/glm-5.3-flash，任务为 read-only；effort 使用 status 为 opencode 实际声明的最高档（仅当 vendor 真的声明时才用 max）。等待其完成（wait_seconds 或 session_dispatch + session_collect），取得 2–5 个 GitHub 先例——相似仓库、相似场景、代码片段——每个包括 URL、pin（commit sha 或 tag）、相关路径、license、可复用思路和不兼容点，最后给出侦察员独立建议和风险。license 不兼容的先例只能借鉴思路，不要复制代码。拿到报告后用 session_stop 停止侦察员。新的顶层编码任务重新侦察。\n" +
      "如果无法启动可用的 GLM 会话（spawn 明确失败、模型不可用、已确认首个回复为空，或已确认回复不符合上述报告要求），恰好一次通过 Codex Luna 以最高已声明 effort 侦察；Luna 也失败时，停止编码并如实上报。此例外仅限编码前侦察，是对下述通用 capability fallback 的例外；指挥官 lead 和其他角色的 fallback 规则保持不变。spawn 或 dispatch 结果未知时，先用 session_list 与 session_collect 核对 GLM 会话是否存活且有输出；仅确认没有可用会话才使用 Luna。\n\n" +
      "第 2 步，计划：把侦察报告交给 Claude Fable，得到计划文件 <project>/.ccteam/plans/<YYYY-MM-DD>-<主题>.md。计划中的每个任务包含：id、执行者、依赖、文件、完成标准。文件末尾是 Amendments 段：批准后的每次修改都是一条独立记录，带日期、作者和原因；计划正文不重写。计划文件是唯一事实来源。\n" +
      "第 3 步，计划评审：一个全新的 Codex Sol 会话评审计划，有权否决。否决则 Fable 修改、Sol 再审；上限 2 轮。第三次分歧时停止工作，给用户发三段报告：Fable 的立场、Sol 的立场、需要决定什么。批准后停止 Fable；该 Sol 会话留下担任顾问。不设人工暂停：计划在文件里，用户随时可以叫停。\n\n" +
      "第 4 步，执行：若项目在 git 之下，计划中的每个任务都在自己的 git worktree ../<repo>-wt/<id> 中、分支 task/<id>、从项目基线分支切出（按项目规则的开发分支，否则 default branch），绝不放在 /tmp；执行者用 --test-threads=4 跑受影响的测试，并在自己的分支上自行提交。按计划标注并考虑依赖启动执行者，Luna 同时最多 3 个；给每个任务附上计划路径、任务 id、顾问 sid 和顾问规则。顾问规则对所有执行者一致：通过 session_dispatch 带 wait_seconds 询问顾问——任何偏离计划之前必须问，卡住时可以问；建议不具约束力；若顾问认为计划在此处有误，执行者不改计划，而是升级给指挥官。升级后由你亲自把修正写入 Amendments，顾问签认；若顾问认为该修正破坏了计划结构，则由全新的 Fable 重新规划受影响部分，Sol 以同样的上限 2 轮评审。若 session_list 显示顾问排队拖慢，就再启动一个 Sol 顾问，给它计划文件和它所服务任务的明确清单。未达成完成标准的执行者向你报告；由你决定：重试、问顾问或修正计划。项目不在 git 之下：没有 worktree，执行者逐个在目录中直接工作，不启动 git 代理，不要执行 git init，并在总结中说明。\n\n" +
      "第 5 步，集成：所有任务分支都已提交后，启动 git 代理 Claude Sonnet。它把任务分支合并进从基线分支切出的 integration/<主题>，按项目 CLAUDE.md/AGENTS.md 跑全量检查；红色即集成失败，交给你。冲突由它自己解决并重跑检查；若解决冲突改动了已评审的行，把 diff 退回终审门。终审门批准后，git 代理把 integration/<主题> 合并进基线分支、push、打开或更新到 main 的 PR（draft → ready，描述用英文），并合并进 main（merge commit，不用 squash）——仅当两位终审都批准同一修订且全量本地检查为绿；CI 只有在项目里存在且真的会跑时才是第三个条件，否则不等待。worktree 在集成后清理，任务分支在 PR 合并后删除。tag 与 release 不在本任务内：只按 owner 单独明确的命令执行。\n\n" +
      "第 6 步，终审门：两个独立的全新会话——Claude Opus 和 Codex Sol，均使用各自最高可用 effort，各自拿到计划文件和 integration/<主题> 相对基线分支的 diff。只有两者批准同一修订时才通过；若意见不一致，把问题退回执行者，修复后由同样的终审会话重新进行两次评审；上限 2 轮，之后给用户发三段报告。批准后停止顾问。\n\n" +
      "监控：你不会自动定时——在每次 spawn 之前和每个完成通知上检查。主机用自己的 shell 跑 nproc、uptime、free -m；会话看 session_list（status、last_activity_seconds、waiting_approval）。默认阈值（项目 routing.md 可覆盖）：load1 高于核数、空闲内存低于 15%、swap 在使用。过载时按由轻到重：1) 不再启动新执行者，直到恢复；2) 通过 dispatch 请正在运行的执行者降低 -j 与 --test-threads 并推迟重测试；3) 用 session_stop 停止最新的执行者，把任务退回计划（worktree 里的分支保留）；4) 集成中的 git 代理和终审门绝不碰。存活：status 为 stale——看一眼；stuck 或静默 15 分钟——dispatch「报告状态」并带 wait_seconds；静默 30 分钟——session_stop 并用计划中的同一任务重新 spawn；进程报错退出——按 sid dispatch 一次以 resume，再失败则全新 spawn。waiting_approval: true 不算挂起。已完成的委派要停止（侦察员交报告后、Fable 计划批准后、执行者分支集成后）：delegation.max_children 统计的是活跃直接子会话，idle 会话也占一个名额。\n\n" +
      "只有 status 或 spawn 的 capability 错误明确报告为未连接或不可用时，所需 vendor、模型或 effort 才允许一次替换：Fable 改用 Claude Opus，任何 Sol 角色改用全新的 Claude Opus，其余改用 status 中 Codex 最佳可用模型及其最高 effort。遇到认证、ACL、超时、配额、预算、委派深度、循环、网络、内部错误或一般 spawn 拒绝时必须 fail closed：不要重试或 fallback，原样报告失败。不要猜测 wire token `max`，使用 vendor 实际声明的最高等级。完成通知会返回给你，由你汇总最终结果。\n\n任务:",
```

- [ ] **Step 8: vitest зелёный**

Run: `cd /root/projects/ccteam-wt/dev/crates/ccteam-web/web && npx vitest run src/lib/playbooks.test.ts src/lib/i18n.test.ts src/pages/HomeView.test.tsx src/pages/CharterPanel.test.tsx`
Expected: PASS.

- [ ] **Step 9: Полная веб-проверка**

Run: `cd /root/projects/ccteam-wt/dev && make web-check`
Expected: vitest + tsc чисто.

- [ ] **Step 10: Коммит**

```bash
git add crates/ccteam-web/web/src/lib/i18n.ts crates/ccteam-web/web/src/lib/i18n.test.ts crates/ccteam-web/web/src/lib/playbooks.test.ts
git commit -m "feat(web): Commander v2 prompt in zh/ru/en with literal-lock tests"
```

---

### Task 3: Пользовательские доки (README, usage, orchestration)

**Files:**
- Modify: `README.md:64`
- Modify: `docs/usage.md:261`
- Modify: `docs/orchestration.md:113` (bullet «Commander & crews»)
- Modify: `docs/orchestration-cn.md:113` (bullet «总控-工班»)
- Проверить: `docs/usage-cn.md` (grep `Commander|Командир|指挥官` даёт пусто → не трогать)

**Interfaces:**
- Consumes: ничего из кода; текст описывает текущую способность без «v0.X добавил».
- Produces: доки, соответствующие §13 spec.

- [ ] **Step 1: README.md:64, заменить bullet целиком**

```markdown
- six formation playbooks (commander & crews, driver & advisor, cross review, bake-off, research triangulation, cost pyramid) that prefill the launcher with a vendor lineup. Commander starts with Claude Opus at maximum available effort and runs each top-level task as a pipeline: a read-only OpenCode GLM (`zai-coding-plan/glm-5.3-flash`) scout returns 2–5 pinned GitHub precedents (URL, commit/tag, path, license), Claude Fable turns the report into a plan file under `.ccteam/plans/` (per-task id, implementer, dependencies, files, definition of done, plus an Amendments log), Codex Sol gates the plan (cap of two rounds, then a three-paragraph report to you) and stays on as the implementers' advisor, Luna/Terra/Sonnet/Haiku execute each task in its own git worktree and branch, a Sonnet git agent integrates the branches, runs the full check suite, opens the PR and merges only after a fresh Opus + Sol pair approves the same revision, and the commander watches host load and session liveness at every decision point; only a typed capability error lets a role fall back
```

- [ ] **Step 2: docs/usage.md:261, заменить абзац про Commander**

```markdown
Telegram also has a persistent quick-template keyboard. Send `/keys` to show the configured templates, tap one to arm its prefix for the next plain message, or send `/keys off` to remove the keyboard and discard an armed prefix. The default templates are Commander, Driver+advisor, Cross review, Bake-off, Triangulate, and Pyramid. The Commander template runs every top-level task as a pipeline: a read-only OpenCode GLM (`zai-coding-plan/glm-5.3-flash`) scout returns 2–5 pinned GitHub precedents (URL, commit/tag, path, license; exactly one Codex Luna scout fallback when no usable GLM session exists); Claude Fable writes the plan to `<project>/.ccteam/plans/<date>-<topic>.md` (every task carries id, implementer, dependencies, files, definition of done; changes after approval go to an Amendments section); Codex Sol gates the plan with a cap of two rounds and then advises implementers (mandatory before any deviation from the plan, advice not binding, disagreements escalate to the commander); Luna (at most 3 in parallel), Terra, Sonnet, and Haiku each work in their own git worktree and `task/<id>` branch; a Claude Sonnet git agent merges the branches into `integration/<topic>`, runs the project's full checks, resolves conflicts, opens the PR and merges into main only when a fresh Opus + fresh Sol pair approves the same revision and local checks are green (CI only if the project actually runs one); the commander checks host load and session liveness before every spawn and on every completion notification and stops delegates it no longer needs. Customize the templates in `~/.ccteam/config.yaml`; changes are picked up without restarting the daemon:
```

- [ ] **Step 3: docs/orchestration.md:113, заменить bullet «Commander & crews»**

```markdown
- **Commander & crews** (总控-工班) — a strong-reasoning controller decomposes, delegates, watches and accepts; per top-level task: a read-only OpenCode GLM (`zai-coding-plan/glm-5.3-flash`) scout brings 2–5 pinned GitHub precedents (URL, commit/tag, path, license), Claude Fable writes the plan to `.ccteam/plans/` (id / implementer / dependencies / files / definition of done per task, Amendments log for later changes), Codex Sol gates the plan (two rounds max, then a three-paragraph report to the human) and stays as the implementers' advisor, Luna/Terra/Sonnet/Haiku execute in per-task git worktrees, a Sonnet git agent integrates and merges after a fresh Opus + Sol pair approves the same revision, and the controller checks host load and session liveness at every decision point. The expensive model pays only for decomposition, gating and acceptance — volume work rides cheaper specialists.
```

- [ ] **Step 4: docs/orchestration-cn.md:113, заменить bullet «总控-工班»**

```markdown
- **总控-工班** —— 强推理总控做拆解/分工/监控/验收;每个顶层任务:只读 OpenCode GLM(`zai-coding-plan/glm-5.3-flash`)侦察员回报 2–5 个钉版 GitHub 先例(URL、commit/tag、路径、license),Claude Fable 把计划写进 `.ccteam/plans/`(每任务 id / 执行者 / 依赖 / 文件 / 完成标准,批准后的修改进 Amendments 段),Codex Sol 评审计划(上限两轮,之后给人三段报告)并留任执行者的顾问,Luna/Terra/Sonnet/Haiku 各在自己的 git worktree 里执行,Sonnet git 代理在全新 Opus + Sol 批准同一修订后集成并合并,总控在每个决策点检查主机负载与会话存活。贵模型只花在拆解、把关与验收上,量活走便宜的专长工。
```

- [ ] **Step 5: Проверить usage-cn**

Run: `grep -n "Commander\|Командир\|指挥官" docs/usage-cn.md || echo "no commander mention"`
Expected: `no commander mention` → файл не трогаем.

- [ ] **Step 6: Коммит**

```bash
git add README.md docs/usage.md docs/orchestration.md docs/orchestration-cn.md
git commit -m "docs: describe Commander v2 pipeline (plan file, Sol gate, worktrees, git agent, dual gate)"
```

---

### Task 4: Governance в AGENTS.md (merge-правило и язык доков)

**Files:**
- Modify: `AGENTS.md:107` (§五.2 язык) и `AGENTS.md:137` (§五 «分支与推送», merge-правило)
- `CLAUDE.md` = symlink на AGENTS.md, отдельно не трогать

**Interfaces:**
- Consumes: решения owner Q27(b)/Q32(a) и «всё в репо на русском» (spec §13).
- Produces: обновлённые правила; остальной текст AGENTS.md байт в байт.

- [ ] **Step 1: §五.2, заменить строку 107**

Было:
```
2. **commit 用英语;agent prompt 用英语**(**产品化、简洁,非冗长**;hub vendored prompt 随上游);项目文档(CLAUDE.md / `docs/`)用中文
```
Стало:
```
2. **commit 用英语;agent prompt 用英语**(**产品化、简洁,非冗长**;hub vendored prompt 随上游);**新文档一律用俄语**(owner 决策 2026-09-02:`docs/`、`docs/superpowers/` spec 与 plan、AGENTS.md 新增段落;既有中文文档不回译,改动到哪段就哪段改俄语)
```

- [ ] **Step 2: §五 «分支与推送», заменить фрагмент в строке 137**

Было:
```
**merge 仅由 owner 执行,方式 = merge commit(非 squash)**
```
Стало:
```
**merge 到 main 由 owner 手动执行,或由「Командир」编队的 git-агент 执行(owner 决策 2026-09-02;条件 = 全新 Opus + Sol 终审门都批准同一修订 **且** 本地全量检查绿,CI 仅在仓库真的会跑时作第三条件);其余任何场景仍只有 owner;方式 = merge commit(非 squash)**
```

- [ ] **Step 3: Проверить, что файл не вырос за лимит**

Run: `wc -l AGENTS.md`
Expected: ≤ 200 строк (сейчас около 170; строки заменяются, не добавляются).

- [ ] **Step 4: Коммит**

```bash
git add AGENTS.md
git commit -m "docs(governance): allow Commander git agent to merge under the dual gate; new docs in Russian"
```

---

### Task 5: Гейт, push, PR

**Files:**
- Ничего нового; прогон проверок в `/root/projects/ccteam-wt/dev`

- [ ] **Step 1: Минимальный гейт + Rust**

Run:
```bash
cd /root/projects/ccteam-wt/dev
cargo fmt --all -- --check
.loop/verify/writeback.sh
make check
cargo test -p ccteam-core
```
Expected: всё зелёное, clippy 0 warnings.

- [ ] **Step 2: Web**

Run: `make web-check`
Expected: зелёное.

- [ ] **Step 3: Push dev и PR**

```bash
git push origin dev
gh pr create --base main --head dev --draft --title "Commander v2: plan file, Sol gate+advisor, worktrees, git agent, dual final gate" --body "$(cat <<'EOF'
## Summary
- Commander template (Telegram compiled + web zh/ru/en) now runs each top-level task as: GLM read-only scout → Fable plan file under `.ccteam/plans/` → Codex Sol plan gate (2 rounds) that stays on as the implementers' advisor → per-task git worktrees (`task/<id>`, at most 3 Luna) → Sonnet git agent (`integration/<topic>`, full checks, PR, merge) → fresh Opus + fresh Sol final gate (2 rounds) → host-load and session-liveness monitoring at every decision point.
- Literal-lock tests in `config.rs`, `playbooks.test.ts`, `i18n.test.ts`; docs (README, usage, orchestration) and AGENTS.md governance (git-agent merge under the dual gate; new docs in Russian).
- Pure prompt change: no engine, MCP, or schema edits.

Spec: `docs/superpowers/specs/2026-09-02-commander-plan-gate-design.md`

## Test plan
- [ ] `cargo fmt --all -- --check`, `make check`, `cargo test -p ccteam-core`
- [ ] `make web-check`
- [ ] `.loop/verify/writeback.sh`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
Expected: draft PR dev→main открыт. Перевод в ready и merge = owner (merge правило для ручного PR не меняется).

---

### Task 6: Колпаки на машине owner (вне репо, после merge)

**Files:**
- Modify: `/mnt/c/Users/gon71/.wslconfig`
- Modify: `~/.bashrc` (или профиль, который читает интерактивный shell `gon71`/`root`)
- Modify: `~/.ccteam/config.yaml` (секция `delegation`; удалить протухший `im.quick_templates[0]` «🎯 Командир», движок его всё равно подменяет вкомпиленным)

- [ ] **Step 1: `.wslconfig`**

Записать целиком:
```ini
[wsl2]
memory=40GB
processors=12
swap=8GB
```
Сказать owner: выполнить `wsl --shutdown` из PowerShell между сессиями (убивает все WSL-сессии, включая текущую).

- [ ] **Step 2: Профиль shell**

Дописать в `~/.bashrc` (проверить, что строк ещё нет):
```bash
export CARGO_TARGET_DIR=/root/projects/.cargo-target/ccteam
export CARGO_BUILD_JOBS=8
```
Run: `mkdir -p /root/projects/.cargo-target/ccteam && source ~/.bashrc && echo "$CARGO_TARGET_DIR $CARGO_BUILD_JOBS"`
Expected: `/root/projects/.cargo-target/ccteam 8`. Старый `target/` (26G) удалить после первой успешной сборки в новом каталоге: `rm -rf /root/projects/ccteam/target` (подтвердить с owner, необратимо).

- [ ] **Step 3: `~/.ccteam/config.yaml`**

Добавить верхнеуровневую секцию (рядом с `claude_jobs_retention_days`):
```yaml
delegation:
  max_depth: 2
  max_children: 16
  max_delegated: 50
```
Удалить блок `im.quick_templates` с label `🎯 Командир` (оставить остальные шаблоны, если есть). Проверить парсинг: `ccteam status` возвращает 0.

- [ ] **Step 4: Перезапуск daemon**

Run: `ccteam stop && ccteam start`
Expected: daemon поднялся; `ccteam status` показывает проекты. Live-сессии поднимутся по sid при следующем сообщении.

---

### Task 7: Charter `routing.md` этого проекта (после merge)

**Files:**
- Overwrite: `/root/projects/ccteam/.ccteam/routing.md` (сейчас там charter проекта 4G с корнем `/root/projects/4G`, для ccteam чужой)

- [ ] **Step 1: Записать новый charter**

```markdown
# ccteam routing — ccteam
Fixed roster (owner's decision, rev. 2026-09-02), matches the built-in Commander template:
| Role | vendor / model / effort | Note |
|---|---|---|
| Commander (decompose, delegate, watch, accept) | claude / opus / max | one session; never plans or codes itself |
| Scout | opencode / zai-coding-plan/glm-5.3-flash / top advertised | read-only; 2–5 pinned GitHub precedents; stopped after the report |
| Planner | claude / claude-fable-5 / max | plan file `.ccteam/plans/<date>-<topic>.md`; stopped after approval; fresh Fable for structural replans |
| Plan gate + advisor | codex / gpt-5.6-sol / max | one session: blocks the plan (2 rounds), then advises implementers until the code gate approves |
| Implementation | codex / gpt-5.6-luna / max | max 3 concurrent; own worktree `../ccteam-wt/<id>`, branch `task/<id>`, base `origin/dev` |
| Frontend / complex implementation | codex / gpt-5.6-terra / max | own worktree as above |
| Docs, graph, cheap work | claude / sonnet / max | own worktree as above |
| Fast search, small fixes | claude / haiku / max | own worktree as above |
| Git agent | claude / sonnet / max | `integration/<topic>`, full checks (`cargo fmt --all -- --check`, `make check`, `make test`, `make web-check`), PR dev→main, merge commit; merge only after the dual gate + green local checks (no CI runs on this fork) |
| Final gate | claude / opus / max + codex / gpt-5.6-sol / max | two fresh sessions, both approve the same revision; 2 rounds then report |

Thresholds for the commander's monitoring on this host: 16 vCPU, 40GB WSL cap — load1 > 16, free memory < 15%, swap in use. Stop delegates that are done; `delegation.max_children` is 16 here.

Rules: every spawn sets vendor+model explicitly. Repo root is `/root/projects/ccteam`; read AGENTS.md first; worktrees never in `/tmp`.
```

- [ ] **Step 2: Проверить, что charter виден**

Run: `ccteam status` (или MCP `status` из любой сессии) — routing notes показывают новый текст.

---

## Самопроверка плана по spec

- §2 ростер → Task 1/2 текст, Task 7 charter. §3 поток → «Шаг 1–6» во всех четырёх копиях + лок порядка фаз. §4 файл плана → фразы `.ccteam/plans/`, поля, Amendments. §5 гейт плана и доклад → «потолок 2 круга», «трёх абзацев». §6 советник → блок правил в «Шаг 4». §7 исполнители → worktree/ветка/`--test-threads=4`/без git. §8 git-агент → «Шаг 5». §9 финальный гейт → «Шаг 6». §10 мониторинг → блок «Мониторинг». §11 fallback/лимит детей → хвост fallback + `delegation.max_children`. §13 репо → Task 1–5. §14 машина → Task 6–7. §15 тесты → Task 1 Step 2, Task 2 Step 2–3.
- Плейсхолдеров нет: все тексты, тесты и команды выписаны.
- Имена фраз в тестах Task 1/2 сверены с текстами Task 1 Step 4 и Task 2 Step 5–7 (ru: «для Fable замена — Claude Opus» в Telegram-копии, «для Fable — Claude Opus» в web-копии, потому что web-копия сохраняет своё капабилити-предложение «Только явное сообщение status…»; лок Task 2 Step 2 для ru ищет `"Fable — Claude Opus"`, что входит в обе формулировки).
