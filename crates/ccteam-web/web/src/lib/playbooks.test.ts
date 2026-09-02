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
      model: "opus",
      effort: "max",
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
    for (const role of ["Opus", "Luna", "Terra", "Sonnet", "Sol", "Fable", "Haiku"]) {
      expect(prompt, role).toContain(role);
    }
    expect(prompt).toContain("не более 3");
    expect(prompt).not.toContain("до 10 Luna");
    expect(prompt).toContain("максимальн");
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
          "如果 Opus 可用",
          "当前会话是有意启动的 Codex fallback",
          "不要再创建 Opus",
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
          "Если Opus доступен",
          "текущая сессия намеренно запущена как Codex fallback",
          "не создавай Opus повторно",
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
          "If Opus is available",
          "this session is the deliberate Codex fallback",
          "do not spawn Opus again",
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
      ["ru", ["zai-coding-plan/glm-5.3-flash", "до первой правки кода", "read-only", "2–5", "commit sha или tag", "license", "Codex Luna", "останови работу над кодом", "session_list и session_collect", "ответ не соответствует требованиям к отчёту", "Это исключение только для разведки перед кодом"]],
      ["en", ["zai-coding-plan/glm-5.3-flash", "before the first code edit", "read-only", "2–5", "commit sha or tag", "license", "Codex Luna", "stop coding and report honestly", "session_list and session_collect", "response is confirmed non-compliant with the report requirements", "This exception applies only to pre-code scouting"]],
    ] as const) {
      for (const phrase of phrases) expect(I18N[lang].tplCommanderP).toContain(phrase);
      expect(I18N[lang].tplCommanderD).toContain("GLM");
    }
  });

  it("the commander prefill carries the v2 contract in every language: plan file, Sol gate+advisor, worktrees, git agent, dual fresh gate, monitoring", () => {
    const matrix = {
      zh: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id、执行者、依赖、文件、完成标准",
        "上限 2 轮", "Fable 的立场、Sol 的立场、需要决定什么", "不设人工暂停",
        "最多 3 个并行", "-wt/", "task/", "integration/", "--test-threads=4", "顾问 sid", "绝对路径",
        "任何偏离计划之前必须问", "建议不具约束力", "升级给指挥官", "明确清单",
        "git 代理 Claude Sonnet", "merge commit", "两位终审都批准同一修订且全量本地检查为绿", "CI 只有在项目里存在", "tag 与 release 不在本任务内",
        "全新的 Claude Opus 和全新的 Codex Sol", "顾问不参与验收", "重新构建 integration/",
        "session_list", "last_active", "free -m", "15%", "session_stop", "stale", "stuck", "activity 为 stale", "30 分钟", "waiting_approval: true 不算挂起", "delegation.max_children", "执行者在终审门批准代码后",
        "用 session_stop 停止侦察员", "git init", "Fable 改用 Claude Opus", "任何 Sol 角色改用全新的 Claude Opus",
      ],
      ru: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id, исполнитель, зависимости, файлы, критерий готовности",
        "потолок 2 круга", "позиция Fable, позиция Sol, что нужно решить", "Пауз на человека нет",
        "не более 3 параллельно", "-wt/", "task/", "integration/", "--test-threads=4", "sid советника", "абсолютный путь плана",
        "обязательно перед любым отклонением от плана", "совет не обязателен к исполнению", "эскалирует командиру", "явным списком задач",
        "git-агента Claude Sonnet", "merge commit, не squash", "оба финальных ревьюера одобрили одну ревизию и полный набор локальных проверок зелёный", "CI — третье условие, только если он в проекте есть", "Tag и release вне",
        "свежие сессии — Claude Opus и Codex Sol", "советник в приёмке не участвует", "заново собирает integration/",
        "session_list", "last_active", "free -m", "15 процентов", "session_stop", "stale", "stuck", "activity stale", "30 минут", "waiting_approval: true зависшей не считается", "delegation.max_children", "исполнители после одобрения кода финальным гейтом",
        "останови разведчика через session_stop", "git init не делай", "Fable — Claude Opus", "любой роли Sol — свежий Claude Opus",
      ],
      en: [
        "Claude Fable", ".ccteam/plans/", "Amendments", "id, implementer, dependencies, files, definition of done",
        "cap of 2 rounds", "Fable's position, Sol's position, what needs deciding", "No human pause",
        "at most 3 in parallel", "-wt/", "task/", "integration/", "--test-threads=4", "advisor's sid", "absolute plan path",
        "mandatory before any deviation from the plan", "advice is not binding", "escalates to the commander", "explicit list of tasks",
        "git agent, Claude Sonnet", "merge commit, not squash", "both final reviewers approved the same revision and the full local check suite is green", "CI is a third condition only if the project has one", "Tag and release are outside",
        "fresh sessions — Claude Opus and Codex Sol", "advisor does not take part in acceptance", "rebuilds integration/",
        "session_list", "last_active", "free -m", "15 percent", "session_stop", "stale", "stuck", "activity stale", "30 minutes", "waiting_approval: true does not count as hung", "delegation.max_children", "implementers after the final gate approves the code",
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
      const gate = prompt.slice(prompt.indexOf(order[5]));
      const gatePhrases = lang === "zh"
        ? ["上限 2 轮", "三段报告", "重新构建 integration/"]
        : lang === "ru"
          ? ["потолок 2 круга", "трёх абзацев", "заново собирает integration/"]
          : ["cap of 2 rounds", "three-paragraph report", "rebuilds integration/"];
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
    const posture = { vendor: "claude", model: "opus", effort: "max" } as const;
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
    const posture = { vendor: "claude", model: "opus", effort: "max" } as const;
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
    const posture = { vendor: "claude", model: "opus", effort: "max" } as const;
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
    const posture = { vendor: "claude", model: "opus", effort: "high" } as const;
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
    const posture = { vendor: "claude", model: "opus", effort: "max" } as const;
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
