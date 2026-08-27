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
    expect(prompt).toContain("до 10");
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

  it("recognizes only capability failures as Commander fallback candidates", () => {
    const posture = { vendor: "claude", model: "opus", effort: "max" } as const;
    expect(
      isCommanderBootstrapCapabilityError(
        new Error("会话启动失败: invalid reasoning effort `max`"),
        posture,
        ["claude", "codex"],
      ),
    ).toBe(true);
    expect(
      isCommanderBootstrapCapabilityError(
        new Error("spawn failed: No such file or directory"),
        posture,
        ["codex"],
      ),
    ).toBe(true);

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
          ["codex"],
        ),
        message,
      ).toBe(false);
    }

    // A manual posture change is not Commander bootstrap anymore.
    expect(
      isCommanderBootstrapCapabilityError(
        new Error("invalid model"),
        { vendor: "codex", model: "gpt-5.6-codex", effort: "xhigh" },
        ["codex"],
      ),
    ).toBe(false);
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
