# Командир编队 × GLM 侦察员(scout)实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在「Командир」编队的四份 prompt(Telegram 编译内置 + web zh/ru/en)里加入 GLM 侦察门:每个可能改代码的顶层任务在首次编码前必须先拿到只读 GLM 侦察报告,配套字面锁测试与用户文档同步。

**Architecture:** 纯 prompt-soft 改动。引擎零改动:不加 skill、不改 MCP schema、不改 gateway、不加运行时类型、不做引擎侧执法。行为合同全部写进 Commander prompt 文本,测试沿用既有「字面锁」模式——纯字符串断言,零 LLM。

**Tech Stack:** Rust(`ccteam-core` 编译内置模板 + `#[cfg(test)]` 字面锁)、TypeScript(`i18n.ts` 三语 prompt、`playbooks.ts` roster、vitest)、Markdown 用户文档。

**Spec:** `docs/superpowers/specs/2026-08-31-commander-glm-scout-design.md`

## Global Constraints

- 侦察员固定为 vendor `opencode`、模型 `zai-coding-plan/glm-5.3-flash`,侦察任务标注 read-only。
- effort 取 `status` 通告的该 vendor 最高档;`max` 仅在 vendor 确实通告时使用,绝不猜 wire token。
- 侦察报告 = **2–5 个 GitHub 先例**,每个必含 URL、pin(commit sha 或 tag)、相关路径、license、可复用思路、不兼容点;末尾附独立建议与风险;license 不兼容 = 只取思路、绝不抄代码。
- 起不来可用 GLM 会话 → **恰好一次** Codex Luna 兜底(最高通告 effort);Luna 也失败 → 停止编码并如实上报。未知结局先用 `session_list` + `session_collect` 对账,确认无可用会话才烧 Luna 名额。
- GLM 复用边界:允许二级意见 / 图谱 / 文档 / 廉价机械活 / 显式圈定边界的小改;禁止主编码 / 网络与生产运维 / Opus+Sol 终审双票。
- 现有 Commander lead 的 capability-driven fallback 行文原样保留。
- 不做版本号 bump、不发版;commit message 用英语;完工后进入 `dev` 与 dev→main PR。
- 写权:Task 1/2 是代码面,dev 会话可执行;Task 3 的 `docs/` 由 Fable 规划会话执笔。

---

### Task 1: Rust 编译内置 Telegram Commander 模板 + 相邻 config 测试

**Files:**
- Modify/Test: `crates/ccteam-core/src/config.rs` (`commander_quick_template()` 与相邻 tests)

**Interfaces:**
- Consumes: `commander_quick_template() -> QuickTemplate`。
- Produces: 不改签名、不加类型;只扩展 prefix 字符串。既有 roster、capability-code、拒绝族与 `ends_with("Задача:")` 断言继续通过。

- [ ] **Step 1: 写失败测试**

在 `default_im_quick_templates_include_six_templates` 后新增:

```rust
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
        "второе мнение",
        "никогда — основной код",
        "финальные ревью Opus и Sol",
    ] {
        assert!(prefix.contains(phrase), "Commander scout contract lacks {phrase}");
    }
    assert!(prefix.contains("Fallback разрешён только если явный ответ status"));
    assert!(prefix.ends_with("Задача:"));
}
```

- [ ] **Step 2: 确认测试红**

Run: `cargo test -p ccteam-core commander_template_gates_first_code_edit_on_glm_scout`
Expected: FAIL on missing model id.

- [ ] **Step 3: 最小实现**

在 Commander roster 的 Haiku 行之后、`Финальный гейт` 之前插入:

```rust
"• OpenCode GLM (модель zai-coding-plan/glm-5.3-flash), максимальный доступный effort — дешёвый разведчик и помощник; на этапе разведки только чтение (read-only).\n\n",
"Разведка перед кодом: для каждой верхнеуровневой задачи, которая может менять код, до первой правки кода запусти через session_spawn разведчика opencode с моделью zai-coding-plan/glm-5.3-flash с задачей read-only; effort бери верхней ступенью, которую status реально объявил для opencode (max — только если vendor его действительно объявил). Дождись завершения (wait_seconds или session_dispatch + session_collect) и получи отчёт: 2–5 прецедентов с GitHub, для каждого URL, pin (commit sha или tag), релевантный путь, license, применимые идеи и несовместимости, в конце — независимая рекомендация разведчика и риски. Только после разбора отчёта запускай исполнителей кода; из прецедентов с несовместимой license бери только идеи, код не копируй. Эту же GLM-сессию можно переиспользовать дальше по задаче; новая верхнеуровневая задача с кодом проходит разведку заново.\n",
"Если пригодную GLM-сессию поднять не удалось (spawn явно упал, модель недоступна или подтверждено, что первый ответ пуст) — сделай ровно одну попытку разведки через Codex Luna с максимальным объявленным effort; если и Luna не справилась — останови работу над кодом и честно сообщи. При неизвестном исходе spawn или dispatch сначала сверься через session_list и session_collect, жива ли GLM-сессия и есть ли у неё вывод; трать попытку Luna только подтвердив, что пригодной сессии нет.\n",
"После разведки GLM можно давать второе мнение, операции с графом, черновики документации, дешёвую механику и мелкие правки с явно очерченными границами; никогда — основной код, сетевые и производственные операции, финальные ревью Opus и Sol.\n\n",
```

开头、终审、lead fallback 与 `Задача:` 段逐字保留。

- [ ] **Step 4: 跑绿与自检**

Run: `cargo test -p ccteam-core commander_template_gates_first_code_edit_on_glm_scout && cargo test -p ccteam-core default_im_quick_templates_include_six_templates && cargo test -p ccteam-core im_quick_templates_yaml_override_parses`
Expected: all PASS.

Run: `cargo fmt --all -- --check && cargo clippy -p ccteam-core --tests`
Expected: fmt clean, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/ccteam-core/src/config.rs
git commit -m "feat(core): gate Commander coding on GLM scouting"
```

---

### Task 2: web 三语 Commander prompt + opencode roster chip + 定向 vitest

**Files:**
- Modify: `crates/ccteam-web/web/src/lib/i18n.ts`
- Modify: `crates/ccteam-web/web/src/lib/playbooks.ts`
- Test: `crates/ccteam-web/web/src/lib/playbooks.test.ts`
- Test: `crates/ccteam-web/web/src/lib/i18n.test.ts`
- Test: `crates/ccteam-web/web/src/pages/HomeView.test.tsx`

**Interfaces:**
- `PLAYBOOKS.commander.vendors` 变为 `["claude", "codex", "opencode"]`;首项仍是 Claude,故 lead `claude/opus/max` 与现有 bootstrap fallback 逻辑不变。
- 不加导出、不改函数签名。

- [ ] **Step 1: 写失败测试**

在 `playbooks.test.ts` 的 Commander contract 测试中加入三语 scout 字面锁:

```ts
for (const [lang, phrases] of [
  ["zh", ["zai-coding-plan/glm-5.3-flash", "在第一次改动代码之前", "read-only", "2–5", "commit sha 或 tag", "license", "Codex Luna", "停止编码并如实上报", "session_list 与 session_collect", "绝不承担主编码"]],
  ["ru", ["zai-coding-plan/glm-5.3-flash", "до первой правки кода", "read-only", "2–5", "commit sha или tag", "license", "Codex Luna", "останови работу над кодом", "session_list и session_collect", "никогда — основной код"]],
  ["en", ["zai-coding-plan/glm-5.3-flash", "before the first code edit", "read-only", "2–5", "commit sha or tag", "license", "Codex Luna", "stop coding and report honestly", "session_list and session_collect", "never primary coding"]],
] as const) {
  for (const phrase of phrases) expect(I18N[lang].tplCommanderP).toContain(phrase);
  expect(I18N[lang].tplCommanderD).toContain("GLM");
}
```

在 `i18n.test.ts` 加:

```ts
for (const lang of ["zh", "ru", "en"] as const) {
  expect(t(lang, "tplCommanderP")).toContain("zai-coding-plan/glm-5.3-flash");
}
```

在 `HomeView.test.tsx` Commander chip 断言改为:

```ts
for (const vendor of ["claude", "codex", "opencode"]) {
  expect(commander).toContain(`data-vendor="${vendor}"`);
}
expect(commander).not.toContain('data-vendor="grok"');
```

- [ ] **Step 2: 确认测试红**

Run: `cd crates/ccteam-web/web && npx vitest run src/lib/playbooks.test.ts src/lib/i18n.test.ts src/pages/HomeView.test.tsx`
Expected: FAIL on missing model text, GLM description, and opencode chip.

- [ ] **Step 3: 最小实现**

`playbooks.ts`:

```ts
vendors: ["claude", "codex", "opencode"],
```

三语 `tplCommanderD` 分别说明 GLM scout;三语 `tplCommanderP` 在 roster 与 final gate 之间加入与 Rust 模板等义的以下合同:

- exact `zai-coding-plan/glm-5.3-flash`, `opencode`, read-only, highest status-advertised effort;
- wait before first edit; 2–5 GitHub results with URL/pin/path/license/reuse/incompatibilities and independent recommendation/risks;
- incompatible-license ideas only;
- exactly one Luna fallback, unknown-outcome reconciliation, Luna failure stops coding;
- allowed cheap work and forbidden primary coding/network-production/final votes.

俄语段与 Task 1 逐字一致。既有开头、final gate、lead fallback 段逐字保留。

- [ ] **Step 4: 跑绿与全 web 门禁**

Run: `cd crates/ccteam-web/web && npx vitest run src/lib/playbooks.test.ts src/lib/i18n.test.ts src/pages/HomeView.test.tsx`
Expected: all PASS; existing lead/fallback negative tests remain green.

Run: `make web-check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ccteam-web/web/src/lib/i18n.ts crates/ccteam-web/web/src/lib/playbooks.ts crates/ccteam-web/web/src/lib/playbooks.test.ts crates/ccteam-web/web/src/lib/i18n.test.ts crates/ccteam-web/web/src/pages/HomeView.test.tsx
git commit -m "feat(web): add GLM scout to Commander playbook"
```

---

### Task 3: 用户文档 + 集成验证

**Files:**
- Modify: `README.md` Commander formation paragraph
- Modify: `docs/usage.md` Telegram quick-template section
- Modify: `docs/orchestration.md` Commander & crews formation
- Modify: `docs/orchestration-cn.md` 对称 formation

**Interfaces:**
- 文档只描述当前能力,不写版本进展措辞。
- `docs/` 由 Fable 规划会话执笔;不动 AGENTS.md、`.loop/`、workspace version 或 release archive。

- [ ] **Step 1: 同步当前能力文案**

README Commander 段明确:read-only OpenCode GLM scout、exact model、2–5 pinned/licensed precedents、single Luna fallback、Opus+Sol final gate。

`docs/usage.md` `/keys` 段加入:

```markdown
The Commander template also fields a read-only OpenCode GLM scout (`zai-coding-plan/glm-5.3-flash`) that collects 2–5 pinned GitHub precedents (URL, commit pin, path, license) before the first code edit of each coding task, with exactly one Codex Luna fallback when no usable GLM session exists.
```

`docs/orchestration.md` 与 `docs/orchestration-cn.md` 的 Commander formation 把旧 `grok scouts the ecosystem` 改为 OpenCode GLM pre-code scout,并保留「编排发生在 session 内」语义。

- [ ] **Step 2: 文档一致性检查**

Run: `rg -n "Commander|Командир|GLM|zai-coding-plan/glm-5.3-flash|grok scouts" README.md docs/usage.md docs/orchestration.md docs/orchestration-cn.md`
Expected: Commander current-capability surfaces mention GLM; stale `grok scouts` is absent from the Commander line.

- [ ] **Step 3: 全仓验证**

Run: `cargo fmt --all -- --check`
Expected: PASS.

Run: `cargo clippy --workspace --tests`
Expected: zero warnings.

Run: `cargo test --workspace`
Expected: PASS; registered environment flakes are isolated and compared against the pristine base before classification.

Run: `make web-check`
Expected: PASS.

Run: `.loop/verify/writeback.sh`
Expected: GREEN.

- [ ] **Step 4: Commit**

```bash
git add README.md docs/usage.md docs/orchestration.md docs/orchestration-cn.md
git commit -m "docs: document Commander GLM scouting"
```

---

## 任务间依赖与并行性

Task 1(`crates/ccteam-core`)与 Task 2(`crates/ccteam-web`)冲突域不相交,各自在独立 worktree 由一个 dev 写者并行执行。Task 3 等 1/2 合入集成分支后由 Fable 规划会话执行;最终全仓验证只跑一份,避免并发重负载。
