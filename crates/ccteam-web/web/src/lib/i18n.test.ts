// v0.8.18 柱2/UI + v0.8.24 Track A — i18n helper + whole-shell dictionary tests.

import { describe, expect, it } from "vitest";

import { I18N, WEB_LOCALE, makeT, navLabel, t, tHostsCount, tr, tShowMore, tStopped } from "./i18n";

describe("i18n", () => {
  it("tr defaults to zh and picks en when chosen", () => {
    expect(tr("zh", "主机", "Hosts", "Хосты")).toBe("主机");
    expect(tr("en", "主机", "Hosts", "Хосты")).toBe("Hosts");
    expect(tr("ru", "主机", "Hosts", "Хосты")).toBe("Хосты");
  });

  it("navLabel returns the per-language nav label", () => {
    expect(navLabel("hosts", "zh")).toBe("主机");
    expect(navLabel("hosts", "en")).toBe("Hosts");
    expect(navLabel("marketplace", "zh")).toBe("插件市场");
    expect(navLabel("marketplace", "en")).toBe("Plugins");
    expect(navLabel("status", "zh")).toBe("Status");
    expect(navLabel("settings", "en")).toBe("Settings");
    expect(navLabel("settings", "zh")).toBe("设置");
    expect(navLabel("workflow", "zh")).toBe("工作流");
    expect(navLabel("workflow", "en")).toBe("Workflow");
  });

  it("navLabel falls back to the key for an unknown view", () => {
    expect(navLabel("nope", "zh")).toBe("nope");
    expect(navLabel("nope", "en")).toBe("nope");
  });
});

// v0.8.24 Track A — table-driven whole-shell dictionary (prototype I18N keys).
describe("I18N dictionary", () => {
  it("keeps Russian, Chinese, and English dictionaries in lockstep", () => {
    const keys = Object.keys(I18N.ru).sort();
    expect(keys).toEqual(Object.keys(I18N.zh).sort());
    expect(keys).toEqual(Object.keys(I18N.en).sort());
  });

  it("localizes workflow labels", () => {
    expect(t("zh", "workflowSkillsTitle")).toBe("Skills");
    expect(t("en", "workflowRolesTitle")).toBe("Roles");
    expect(t("ru", "workflowMcpTitle")).toBe("MCP-серверы");
    expect(t("ru", "workflowBuiltIn")).toBe("встроенная");
  });

  it("resolves Russian text and locale", () => {
    expect(t("ru", "homeTitle")).toBe("За работу!");
    expect(WEB_LOCALE.ru).toBe("ru-RU");
    expect(tStopped("ru", "s9")).toContain("s9");
  });

  it("keeps long-form Russian copy in the core surfaces", () => {
    expect(t("ru", "tplCommanderP")).toContain("session_spawn / session_dispatch");
    expect(t("ru", "accessMcpDesc")).toContain("MCP");
    expect(t("ru", "scheduleTzNote")).toContain("daemon");
    expect(t("ru", "dshDesc")).toContain("DeepSeek Harness");
    expect(t("ru", "teamDesc")).toContain("делегирования");
    expect(t("ru", "charterHonesty")).toContain("MCP status");
  });

  it("keeps the Commander GLM scout model and plan-file path in every language", () => {
    for (const lang of ["zh", "ru", "en"] as const) {
      expect(t(lang, "tplCommanderP")).toContain("zai-coding-plan/glm-5.3-flash");
      expect(t(lang, "tplCommanderP")).toContain(".ccteam/plans/");
    }
  });

  it("covers zh and en with the same key set", () => {
    const zhKeys = Object.keys(I18N.zh).sort();
    const enKeys = Object.keys(I18N.en).sort();
    expect(enKeys).toEqual(zhKeys);
    expect(zhKeys.length).toBeGreaterThan(60);
  });

  it("t() resolves per language and falls back to the key when unknown", () => {
    expect(t("zh", "homeTitle")).toBe("开工吧!");
    expect(t("en", "homeTitle")).toBe("Let's build!");
    expect(t("en", "definitely-not-a-key")).toBe("definitely-not-a-key");
  });

  it("makeT curries the language", () => {
    const tt = makeT("en");
    expect(tt("quickStart")).toBe("Quick start");
    expect(t("zh", "setOps")).toBe("运维总览");
    expect(tt("setOps")).toBe("Ops & Hosts");
    expect(t("zh", "setAdmin")).toBe("管理员");
    expect(tt("setAdmin")).toBe("Admin");
  });

  it("parameterized phrases interpolate per language", () => {
    expect(tShowMore("zh", 3)).toBe("展开显示(还有 3 个)");
    expect(tShowMore("en", 3)).toBe("Show more (3 more)");
    expect(tStopped("zh", "s9")).toContain("s9");
    expect(tStopped("en", "s9")).toContain("Stopped s9");
  });

  it("uses Russian plural forms for host counts", () => {
    expect(tHostsCount("ru", 21)).toBe("21 хост");
    expect(tHostsCount("ru", 22)).toBe("22 хоста");
    expect(tHostsCount("ru", 25)).toBe("25 хостов");
  });

  it("keeps the HITL permission-mode semantics in both languages", () => {
    expect(t("zh", "hitlOn")).toContain("--permission-mode default");
    expect(t("en", "hitlOff")).toContain("--dangerously-skip-permissions");
  });
});
