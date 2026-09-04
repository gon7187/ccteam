// v0.9.11 TEAM-3 — formation playbook invariants. `lib/playbooks.ts` is the
// ONE definition module behind both the Home launcher grid and the Team page
// 分工 tab cards, so the shape is pinned here once: 6 owner-approved
// formations, unique ids, known vendors only, and a complete zh+en i18n
// triple per card (a hole would silently fall back zh → key in the UI).
// The pure helpers carry the whole Team→Home handoff chain: router state →
// playbook id (`playbookFromState`) → composer patch (`applyPlaybook`).

import { describe, expect, it } from "vitest";

import {
  applyPlaybook,
  bestCommanderCodexPosture,
  isCommanderBootstrapCapabilityError,
  playbookFromState,
  PLAYBOOKS,
} from "./playbooks";
import { I18N } from "./i18n";
import { VENDORS } from "./vendors";

describe("PLAYBOOKS (shared home/team formation definitions)", () => {
  it("holds the 6 owner-approved formations, unique ids, display order pinned", () => {
    expect(PLAYBOOKS.map((p) => p.id)).toEqual([
      "commander",
      "advisor",
      "crossreview",
      "bakeoff",
      "triangulate",
      "pyramid",
    ]);
    expect(new Set(PLAYBOOKS.map((p) => p.id)).size).toBe(PLAYBOOKS.length);
    // The retired single-vendor cards must not resurface.
    for (const dead of ["team", "compare", "review", "code", "fast", "bulk"]) {
      expect(PLAYBOOKS.some((p) => p.id === dead)).toBe(false);
    }
  });

  it("every lineup entry is a known VendorId and every lineup has a lead", () => {
    const known = new Set<string>(VENDORS.map((v) => v.id));
    for (const pb of PLAYBOOKS) {
      expect(pb.vendors.length, pb.id).toBeGreaterThan(0);
      for (const vendor of pb.vendors) {
        expect(known.has(vendor), `${pb.id}: ${vendor}`).toBe(true);
      }
    }
    // Multi-vendor delegation is the point: every formation fields ≥2 vendors.
    for (const pb of PLAYBOOKS) {
      expect(pb.vendors.length, pb.id).toBeGreaterThanOrEqual(2);
    }
  });

  it("each T/D/P i18n key resolves in zh AND en (no zh-fallback holes)", () => {
    for (const pb of PLAYBOOKS) {
      for (const suffix of ["T", "D", "P"] as const) {
        const key = `${pb.key}${suffix}`;
        expect(I18N.zh[key], `zh ${key}`).toBeTruthy();
        expect(I18N.en[key], `en ${key}`).toBeTruthy();
      }
    }
    // The Team page section chrome resolves in both languages too.
    for (const key of ["playbookSection", "playbookLaunch", "playbookHonesty"]) {
      expect(I18N.zh[key], `zh ${key}`).toBeTruthy();
      expect(I18N.en[key], `en ${key}`).toBeTruthy();
    }
  });

  it("applyPlaybook computes the composer patch: `<key>P` prefill + lead vendor", () => {
    const patch = applyPlaybook("commander", "zh");
    expect(patch).toEqual({
      text: I18N.zh.tplCommanderP,
      vendor: "claude",
      model: "fable",
      effort: "high",
    });
    // The pyramid formation leads with the cheap harness, per the escalation
    // story; language picks the localized prefill.
    expect(applyPlaybook("pyramid", "en")).toEqual({
      text: I18N.en.tplPyramidP,
      vendor: "kimi",
    });
    // Unknown id → null (the handoff simply no-ops; nothing is invented).
    expect(applyPlaybook("nope", "zh")).toBeNull();
  });

  it("the commander prefill carries the full roster, dual gate, and capability-only Codex fallback", () => {
    const prompt = I18N.ru.tplCommanderP;
    for (const role of ["Fable", "Sol", "Luna", "Terra", "Sonnet", "GLM"]) {
      expect(prompt, role).toContain(role);
    }
    expect(prompt).not.toContain("Claude Opus");
    expect(prompt).toContain("3 Claude, 3 Codex, 5 GLM");
    expect(prompt).not.toContain("до 10 Luna");
    expect(prompt).toContain("status");
    expect(prompt).toContain("Codex");
    expect(prompt).toContain("session_spawn / session_dispatch");
    expect(prompt).toContain("Только явное сообщение");
    expect(prompt).toContain("не подключены или недоступны");
    expect(prompt).toContain("не повторяй запрос");
    for (const guard of ["ACL", "глубины делегирования", "бюджете", "цикле"]) {
      expect(prompt).toContain(guard);
    }
    for (const [lang, required] of [
      [
        "zh",
        [
          "session_spawn / session_dispatch",
          "只有 status 或 spawn 的 capability 错误",
          "明确报告为未连接或不可用",
          "认证、ACL、超时、配额",
          "fail closed",
          "委派深度",
          "如果当前会话不是 Fable",
          "当前会话是有意启动的 Codex fallback",
          "不要再创建 Fable",
        ],
      ],
      [
        "ru",
        [
          "session_spawn / session_dispatch",
          "Только явное сообщение status или capability-ошибка spawn",
          "не подключены или недоступны",
          "аутентификации, ACL, тайм-ауте, квоте",
          "fail closed",
          "глубины делегирования",
          "если текущая сессия не Fable",
          "текущая сессия намеренно запущена как Codex Sol fallback",
          "Fable повторно не создавай",
        ],
      ],
      [
        "en",
        [
          "session_spawn / session_dispatch",
          "Only an explicit status result or spawn capability error",
          "disconnected or unavailable",
          "authentication, ACL, timeout, quota",
          "fail closed",
          "delegation-depth",
          "if the current session is not Fable",
          "this session is the deliberate Codex fallback",
          "do not spawn Fable again",
        ],
      ],
    ] as const) {
      for (const phrase of required) expect(I18N[lang].tplCommanderP).toContain(phrase);
    }
    for (const [lang, broadFallback] of [
      ["zh", "Claude 或 Opus 不可用时，由 Codex 接管"],
      ["ru", "если Claude или Opus недоступен — эту роль берёт Codex"],
      ["en", "if Claude or Opus is unavailable, Codex takes command"],
    ] as const) {
      expect(I18N[lang].tplCommanderP).not.toContain(broadFallback);
    }
    for (const [lang, unconditionalRespawn] of [
      ["zh", "如果当前会话不是 Opus，请创建独立的 Opus 指挥官"],
      ["ru", "Если текущая сессия не Opus, создай отдельного Opus-командира"],
      ["en", "If the current session is not Opus, create a separate Opus commander"],
    ] as const) {
      expect(I18N[lang].tplCommanderP).not.toContain(unconditionalRespawn);
    }
    for (const [lang, phrases] of [
      ["zh", ["zai-coding-plan/glm-5.3-flash", "在第一次改动代码之前", "read-only", "2–5", "commit sha 或 tag", "license", "Codex Luna", "停止编码并如实上报", "session_list 与 session_collect", "已确认回复不符合上述报告要求", "此例外仅限编码前侦察"]],
      ["ru", ["zai-coding-plan/glm-5.3-flash", "до первой правки кода", "read-only", "2–5", "commit sha или tag", "license", "Codex Luna", "останови работу над кодом", "session_list и session_collect", "не соответствует контракту", "Это исключение только для предкодовой разведки"]],
      ["en", ["zai-coding-plan/glm-5.3-flash", "before the first code edit", "read-only", "2–5", "commit sha or tag", "license", "Codex Luna", "stop coding and report honestly", "session_list and session_collect", "response is confirmed non-compliant with the report requirements", "This exception applies only to pre-code scouting"]],
    ] as const) {
      for (const phrase of phrases) expect(I18N[lang].tplCommanderP).toContain(phrase);
      expect(I18N[lang].tplCommanderD).toContain("GLM");
    }
  });

  it("carries the v3 contract in every language: sizes, lanes, effort, balancing, pre-gate, fallback triggers", () => {
    const matrix = {
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
        "按 sid resume",
      ],
      ru: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id, исполнитель, зависимости, файлы, критерий готовности",
        "потолок 2 круга", "позиция Fable, позиция Sol, что нужно решить", "Пауз на человека нет",
        "Размер задачи:", "до 3 файлов", "Мелочь", "Средняя", "Крупная", "Claude не участвует", "уходит Terra", "единственный гейт — свежий Codex Sol", "Апгрейд только вверх",
        "Полоса A", "Полоса B", "Полоса C", "Пре-гейт", "Гейт 1 — Claude Fable", "Гейт 2 — Codex Sol", "Git-агент — GLM", "Гейты всегда два разных вендора",
        "второй круг", "на одну ступень", "boilerplate", "не ниже high", "гейт на повторе не поднимается", "ниже medium",
        "tokens_24h", "quota", "выше 80", "15 % относительных", "38", "двум отстающим", "деньги, секреты", "миграции данных",
        "-wt/", "task/", "--test-threads=4", ".ccteam/briefs/executor.md", "Статус: готово", "одна задача, одна свежая сессия, один ход", "Ход:",
        "3 Claude, 3 Codex, 5 GLM", "секрет-скан", "gitleaks", "чек-лист плана", "один возврат исполнителю",
        "integration/", "merge commit, не squash", "оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный", "CI — третье условие, только если он в проекте есть", "Tag и release вне",
        "только дифф", "Советник в приёмке не участвует",
        "error_kind=server_overloaded", "два подряд", "session limit", "30 минут", "переключайся сразу", "Статус: готово», после одного возврата", "Один переход на задачу", "session_stop до spawn фолбека",
        "Fable → Codex Sol", "Sol → свежий Claude Fable", "Luna → Claude Sonnet", "Sonnet → Codex Terra", "GLM → Codex Luna", "пара стала одновендорной",
        "session_list", "free -m", "15 процентов", "waiting_approval: true зависшей не считается", "delegation.max_children", "останови разведчика через session_stop", "git init не делай",
        "Первый прогон", "доля токенов по вендору",
        "resume по sid",
      ],
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
        "resume by sid",
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
      const gate = prompt.slice(prompt.indexOf(order[5]));
      const gatePhrases = lang === "zh"
        ? ["上限 2 轮", "三段报告", "只看 diff"]
        : lang === "ru"
          ? ["потолок 2 круга", "трёх абзацев", "только дифф"]
          : ["cap of 2 rounds", "three-paragraph report", "only the diff"];
      for (const phrase of gatePhrases) expect(gate, `${lang} final gate: ${phrase}`).toContain(phrase);
      expect(I18N[lang].tplCommanderD).toContain("Fable");
      expect(I18N[lang].tplCommanderD).toContain("Sol");
    }
  });

  it("builds the Commander fallback from the installed Codex catalog", () => {
    expect(
      bestCommanderCodexPosture(["claude", "codex"], {
        codex: {
          models: [
            { id: "gpt-5.6-codex", efforts: ["low", "high", "xhigh"] },
            { id: "gpt-5.5-codex", efforts: ["low", "high"] },
          ],
          efforts: ["low", "medium", "high", "xhigh"],
        },
      }),
    ).toEqual({ vendor: "codex", model: "gpt-5.6-codex", effort: "xhigh" });

    // No host proof means no speculative retry against a vendor that might
    // be absent too. An observed model with no effort axis stays effortless.
    expect(bestCommanderCodexPosture(null, {})).toBeNull();
    expect(
      bestCommanderCodexPosture(["codex"], {
        codex: { models: [{ id: "gpt-no-effort", efforts: [] }], efforts: ["xhigh"] },
      }),
    ).toEqual({ vendor: "codex", model: "gpt-no-effort" });
  });

  it("requires a typed capability code for a Commander runtime fallback", () => {
    const posture = { vendor: "claude", model: "fable", effort: "max" } as const;
    expect(
      isCommanderBootstrapCapabilityError(
        Object.assign(new Error("会话启动失败: invalid reasoning effort `max`"), {
          status: 422,
          errorCode: "EFFORT_UNAVAILABLE",
        }),
        posture,
      ),
    ).toBe(true);
    expect(
      isCommanderBootstrapCapabilityError(
        new Error("spawn failed: No such file or directory"),
        posture,
      ),
    ).toBe(false);

    for (const message of [
      "UNAUTHENTICATED",
      "FORBIDDEN",
      "NOT_FOUND",
      "network: connection failed",
      "HTTP 403: project is not visible",
      "HTTP 500: model opus is unavailable",
      "会话启动失败: internal state corrupt",
      "vendor is not authenticated",
      "unauthorized: model opus is not available for this subscription",
      "network failure: model opus is unavailable",
      "request timed out while creating the session",
      "quota exceeded: model opus is unavailable",
      "budget guard rejected spawn: model opus is unavailable",
      "delegation depth limit reached: model opus is unavailable",
      "delegation cycle detected: model opus is unavailable",
    ]) {
      expect(
        isCommanderBootstrapCapabilityError(
          new Error(message),
          posture,
        ),
        message,
      ).toBe(false);
    }

    // A manual posture change is not Commander bootstrap anymore.
    expect(
      isCommanderBootstrapCapabilityError(
        new Error("invalid model"),
        { vendor: "codex", model: "gpt-5.6-codex", effort: "xhigh" },
      ),
    ).toBe(false);
  });

  it("fails closed for HTTP 429 and overloaded errors with capability wording", () => {
    const posture = { vendor: "claude", model: "fable", effort: "max" } as const;
    for (const error of [
      new Error("HTTP 429: model opus is unavailable"),
      new Error("provider overloaded: model opus is unavailable"),
      Object.assign(new Error("model opus is unavailable"), {
        status: 429,
        code: "RATE_LIMITED",
      }),
    ]) {
      expect(
        isCommanderBootstrapCapabilityError(error, posture),
        error.message,
      ).toBe(false);
    }
  });

  it("fails closed for typed timeout, network, and internal failures", () => {
    const posture = { vendor: "claude", model: "fable", effort: "max" } as const;
    for (const details of [
      { status: 408, errorCode: "TIMEOUT" },
      { status: 422, errorCode: "ETIMEDOUT" },
      { status: 422, errorCode: "ECONNRESET" },
      { status: 500, errorCode: "INTERNAL_ERROR" },
      { status: 422, errorCode: "BUDGET_EXCEEDED" },
      { status: 403, errorCode: "ACL_DENIED" },
    ]) {
      const error = Object.assign(new Error("model opus is unavailable"), details);
      expect(
        isCommanderBootstrapCapabilityError(error, posture),
        JSON.stringify(details),
      ).toBe(false);
    }
  });

  it("allows only an explicit typed capability code from an API failure", () => {
    const posture = { vendor: "claude", model: "fable", effort: "high" } as const;
    expect(
      isCommanderBootstrapCapabilityError(
        Object.assign(new Error("spawn rejected"), {
          status: 422,
          errorCode: "MODEL_UNAVAILABLE",
        }),
        posture,
      ),
    ).toBe(true);
    expect(
      isCommanderBootstrapCapabilityError(
        Object.assign(new Error("model opus is unavailable"), {
          status: 500,
          errorCode: "MODEL_UNAVAILABLE",
        }),
        posture,
      ),
    ).toBe(false);
  });

  it("allows one fallback for an explicit Claude vendor-unavailable signal", () => {
    const posture = { vendor: "claude", model: "fable", effort: "max" } as const;
    expect(
      isCommanderBootstrapCapabilityError(
        new Error("Claude vendor is unavailable on this host"),
        posture,
      ),
    ).toBe(false);
    expect(
      isCommanderBootstrapCapabilityError(
        Object.assign(new Error("vendor unavailable"), {
          code: "VENDOR_UNAVAILABLE",
          vendor: "claude",
        }),
        posture,
      ),
    ).toBe(true);
  });

  it("playbookFromState extracts only a string playbook id from router state", () => {
    expect(playbookFromState({ playbook: "advisor" })).toBe("advisor");
    expect(playbookFromState(null)).toBeNull();
    expect(playbookFromState(undefined)).toBeNull();
    expect(playbookFromState({})).toBeNull();
    expect(playbookFromState({ playbook: 7 })).toBeNull();
    expect(playbookFromState("advisor")).toBeNull();
  });
});
