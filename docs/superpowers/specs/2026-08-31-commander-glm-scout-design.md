# Командир编队 × GLM 侦察员(scout)设计

> 日期:2026-08-31 · 状态:owner 已批 · 形态:**纯 prompt,不新增 skill、不做引擎编排**(守 §三「引擎零 LLM / daemon 无自主内容决策循环」)。

## 一、范围

在「Командир」编队 prompt 中加入一名廉价侦察员:opencode 会话,模型 `zai-coding-plan/glm-5.3-flash`,侦察阶段只读。目标 = 每个可能改代码的顶层任务在首次编码前先拿 GitHub 先例,降低重复造轮子与许可证风险,同时把二级意见、图谱、文档、机械杂活从贵模型卸载到 GLM。

**不在范围**:引擎任何改动;新 MCP 工具;workflow.yaml;新 skill;引擎侧侦察门禁。

## 二、核心流程(写进 Commander prompt 的行为合同)

1. **status 之后、首次编码改动之前**,Commander 用 `session_spawn` 起 opencode / `zai-coding-plan/glm-5.3-flash` 会话,任务标注 read-only,effort 取 `status` 通告的该 vendor 最高档(`max` 仅在 vendor 确实通告时使用)。
2. **等待完成**(`wait_seconds` 或 dispatch+collect),收侦察报告:**2–5 个 GitHub 先例**,每个必含 URL、pin(commit sha/tag)、相关路径、license、可复用思路、不兼容点;报告末尾附侦察员**独立建议与风险**。
3. Commander 消化报告后才可启动编码执行者;**license 不兼容的先例只读思路、绝不抄代码**。
4. 同一 GLM 会话可在本任务后续阶段复用,不必每次重起;新顶层编码任务重新执行本门。

## 三、失败与未知态

- **起不来可用 GLM 会话**(spawn 明确失败、模型不可用、已确认首轮无有效输出)→ **恰好一次** Codex Luna 兜底,effort 同样取 status 通告的最高档;Luna 也失败 → **停止编码并如实上报**,不无限重试、不换第三家。
- **未知结局先对账再兜底**:spawn/dispatch 结果不明(超时、连接断或未返回可用 sid)时,先用 `session_list` + `session_collect` 确认 GLM 是否实际存活/已产出;确认没有可用会话才烧 Luna 名额,避免双开。
- 现有 Commander lead 的 capability-driven fallback 原样保留;本条只约束 pre-code scout,不放宽其他 fallback。

## 四、GLM 复用边界

| 允许交给 GLM | 禁止交给 GLM |
|---|---|
| 二级意见(second opinion) | **主编码**(primary coding) |
| graphify 图谱操作 | 网络/生产运维操作 |
| 文档起草与整理 | Opus+Sol 终审双票(终门不可替) |
| 廉价机械活(格式、清单、比对) | |
| **显式圈定边界的小改**(点名文件与行为) | |

现有能力驱动的 lead fallback 与全部架构红线**原样保留**,本设计只叠加,不改写。

## 五、诚实边界

全部行为是 **prompt-soft**:靠 Commander prompt 文本约束,引擎不校验「是否先侦察了」「Luna 是否只烧一次」。违约的后果是编排质量下降,不是硬错误。这与 ccteam「编排智能 100% 用户空间」一致,勿在 gateway 加执法。

## 六、触及面

- Telegram 编译内置模板:`crates/ccteam-core/src/config.rs`。
- web 三语 Commander prompt 与说明:`crates/ccteam-web/web/src/lib/i18n.ts`。
- web Commander roster chip:`crates/ccteam-web/web/src/lib/playbooks.ts`。
- 现有相邻测试:`config.rs` tests、`playbooks.test.ts`、`HomeView.test.tsx`、`i18n.test.ts`。
- 用户文档按 ship gate 同步;无 gateway、adapter、MCP schema 或新文件格式改动。

## 七、最小确定性测试

沿用「字面锁」模式,纯字符串断言,零 LLM:

1. Telegram 与 zh/ru/en web 模板都含精确 model id、read-only、首次编码前等待完成的顺序合同;
2. 模板都含先例字段清单(URL/pin/path/license)与「2–5」数量约束;
3. 模板都含「恰好一次 Codex Luna 兜底、失败即停止编码」与「未知态先对账」;
4. 模板都含禁区(主编码 / 网络生产操作 / Opus+Sol 终审),并保留原 lead fallback 行文;
5. Commander web roster 显示 opencode,但 lead 仍是 Claude Opus;
6. Rust 定向测试、相关 vitest、`make web-check`、workspace baseline、clippy 与 fmt 全绿。
