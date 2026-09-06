import { describe, expect, it } from "vitest";

import {
  applyPlaybook,
  bestCommanderCodexPosture,
  isCommanderBootstrapCapabilityError,
  playbookFromState,
  PLAYBOOKS,
} from "./playbooks";
import { COMMANDER_PROMPT, I18N } from "./i18n";
import { VENDORS } from "./vendors";

describe("PLAYBOOKS", () => {
  it("keeps the approved formations and known vendor lineups", () => {
    expect(PLAYBOOKS.map((p) => p.id)).toEqual([
      "commander", "advisor", "crossreview", "bakeoff", "triangulate", "pyramid",
    ]);
    const known = new Set<string>(VENDORS.map((v) => v.id));
    for (const playbook of PLAYBOOKS) {
      expect(playbook.vendors.length).toBeGreaterThan(0);
      expect(playbook.vendors.every((vendor) => known.has(vendor))).toBe(true);
    }
  });

  it("starts Commander as Opus medium and uses the exact shared English instruction", () => {
    expect(applyPlaybook("commander", "zh")).toEqual({
      text: COMMANDER_PROMPT,
      vendor: "claude",
      model: "opus",
      effort: "medium",
    });
    for (const lang of ["zh", "ru", "en"] as const) {
      expect(I18N[lang].tplCommanderP).toBe(COMMANDER_PROMPT);
    }
    for (const phrase of [
      "Claude Opus at medium effort",
      "zai-coding-plan/glm-5.3-flash",
      "Codex Luna medium",
      "Codex Astra",
      "required source context",
      "unknown, never zero use or unlimited capacity",
      "authorization error",
      "typed vendor_unavailable, model_unavailable or effort_unavailable",
      "Authentication/ACL, quota/budget, depth/cycle, timeout, transport and unknown outcomes do not authorize blind retry or downgrade",
    ]) {
      expect(COMMANDER_PROMPT).toContain(phrase);
    }
  });

  it("keeps localized card summaries while the agent prompt remains one English constant", () => {
    expect(I18N.zh.tplCommanderD).toContain("Opus");
    expect(I18N.ru.tplCommanderD).toContain("Opus");
    expect(I18N.en.tplCommanderD).toContain("Opus");
  });

  it("keeps quota uncertainty and required acceptance gates in the shared instruction", () => {
    expect(COMMANDER_PROMPT).toContain("Missing, unavailable, malformed or stale data is unknown");
    expect(COMMANDER_PROMPT).toContain("Never enable paid overage, buy credits, redeem reset credits or lower acceptance");
    expect(COMMANDER_PROMPT).toContain("Astra does not waive an Opus + Sol gate");
  });

  it("resolves every playbook title, summary, and prompt in every locale", () => {
    for (const playbook of PLAYBOOKS) {
      for (const suffix of ["T", "D", "P"]) {
        for (const lang of ["zh", "ru", "en"] as const) {
          expect(I18N[lang][`${playbook.key}${suffix}`]).toBeTruthy();
        }
      }
    }
  });

  it("keeps ordinary playbook handoff parsing", () => {
    expect(applyPlaybook("pyramid", "en")?.vendor).toBe("kimi");
    expect(applyPlaybook("nope", "en")).toBeNull();
    expect(playbookFromState({ playbook: "advisor" })).toBe("advisor");
    expect(playbookFromState({ playbook: 7 })).toBeNull();
  });

  it("rejects every non-string playbook router-state value", () => {
    for (const state of [null, undefined, {}, { playbook: 7 }, "advisor"]) {
      expect(playbookFromState(state)).toBeNull();
    }
  });

  it("keeps Sol high when the advisory catalog omits Sol", () => {
    expect(bestCommanderCodexPosture(["claude", "codex"], {
      codex: {
        models: [
          { id: "gpt-5.6-sol", efforts: ["low", "medium", "high"] },
          { id: "gpt-5.6-codex", efforts: ["low", "high", "xhigh"] },
        ],
        efforts: ["low", "medium", "high", "xhigh"],
      },
    })).toEqual({ vendor: "codex", model: "gpt-5.6-sol", effort: "high" });
    expect(bestCommanderCodexPosture(["codex"], {
      codex: {
        models: [{ id: "gpt-5.6-codex", efforts: ["low", "high", "xhigh"] }],
        efforts: ["low", "medium", "high", "xhigh"],
      },
    })).toEqual({ vendor: "codex", model: "gpt-5.6-sol", effort: "high" });
    expect(bestCommanderCodexPosture(null, {})).toBeNull();
  });

  it("sends the prescribed Sol high request for vendor validation despite missing advisory effort", () => {
    expect(bestCommanderCodexPosture(["codex"], {})).toEqual({ vendor: "codex", model: "gpt-5.6-sol", effort: "high" });
    expect(bestCommanderCodexPosture(["codex"], {
      codex: { models: [{ id: "gpt-5.6-sol", efforts: [] }], efforts: [] },
    })).toEqual({ vendor: "codex", model: "gpt-5.6-sol", effort: "high" });
  });

  it("permits the one fallback only for a typed Opus capability error", () => {
    const posture = { vendor: "claude", model: "opus", effort: "medium" } as const;
    expect(isCommanderBootstrapCapabilityError(
      Object.assign(new Error("model unavailable"), { status: 422, errorCode: "MODEL_UNAVAILABLE" }),
      posture,
    )).toBe(true);
    expect(isCommanderBootstrapCapabilityError(
      Object.assign(new Error("model unavailable"), { status: 422, errorCode: "MODEL_UNAVAILABLE" }),
      { vendor: "claude", model: "opus[1m]", effort: "medium" },
    )).toBe(true);
  });

  it("fails closed for authentication, ACL, and quota capability-looking errors", () => {
    const posture = { vendor: "claude", model: "opus", effort: "medium" } as const;
    for (const details of [
      { status: 401, errorCode: "MODEL_UNAVAILABLE" },
      { status: 403, errorCode: "ACL_DENIED" },
      { status: 429, errorCode: "MODEL_UNAVAILABLE" },
      { status: 422, errorCode: "BUDGET_EXCEEDED" },
    ]) expect(isCommanderBootstrapCapabilityError(Object.assign(new Error("failed"), details), posture)).toBe(false);
  });

  it("fails closed for typed timeout, transport, and internal errors", () => {
    const posture = { vendor: "claude", model: "opus", effort: "medium" } as const;
    for (const details of [
      { status: 408, errorCode: "TIMEOUT" },
      { status: 422, errorCode: "ECONNRESET" },
      { status: 500, errorCode: "INTERNAL_ERROR" },
    ]) expect(isCommanderBootstrapCapabilityError(Object.assign(new Error("failed"), details), posture)).toBe(false);
  });

  it("requires a Claude-scoped 400 or 422 capability response", () => {
    const posture = { vendor: "claude", model: "opus", effort: "medium" } as const;
    expect(isCommanderBootstrapCapabilityError(
      Object.assign(new Error("failed"), { status: 500, errorCode: "MODEL_UNAVAILABLE" }), posture,
    )).toBe(false);
    expect(isCommanderBootstrapCapabilityError(
      Object.assign(new Error("failed"), { status: 422, errorCode: "MODEL_UNAVAILABLE", vendor: "codex" }), posture,
    )).toBe(false);
  });
});
