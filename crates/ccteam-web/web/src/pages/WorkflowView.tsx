// v0.8.24 Track A — 工作流 top-level view (prototype `#view-flow`):
// a set-nav second column (232px, 「工作流」) with five sub-pages —
// Skills / Roles / Plugins / MCP Servers / 自进化.

import { useCallback, useEffect, useMemo, useState } from "react";
import { Activity, Package, Server, ShoppingBag, User } from "lucide-react";
import { listProjectRoles, type RoleSummary } from "../lib/sessionsApi";
import { getProjectMarketplace, type DecoratedPlugin } from "../lib/marketplaceApi";
import {
  getEvolution,
  getMcpServers,
  registerMcpServer,
  type EvolutionSummary,
  type McpServersResponse,
} from "../lib/workflowApi";
import { useProjectsStore } from "../hooks/useProjectsStore";
import { makeT, tr, type Lang } from "../lib/i18n";
import { toastBus } from "../lib/toastBus";
import EvolutionPanel from "./EvolutionPanel";
import MarketplaceView from "./MarketplaceView";

type TabId = "skills" | "roles" | "market" | "mcp" | "evolution";

const TABS: { id: TabId; labelKey: string; subKey: string; icon: React.ReactNode }[] = [
  { id: "skills", labelKey: "workflowSkillsTitle", subKey: "skillsSub", icon: <Package /> },
  { id: "roles", labelKey: "workflowRolesTitle", subKey: "rolesSub", icon: <User /> },
  { id: "market", labelKey: "marketTab", subKey: "marketSub", icon: <ShoppingBag /> },
  { id: "mcp", labelKey: "workflowMcpTitle", subKey: "mcpSub", icon: <Server /> },
  { id: "evolution", labelKey: "evolve", subKey: "evolveSub", icon: <Activity /> },
];

function isTab(v: string | undefined): v is TabId {
  return !!v && TABS.some((t) => t.id === v);
}

export default function WorkflowView({
  tab: routeTab,
  onNav,
  onOpenMarket,
  lang: langProp,
}: {
  tab?: string;
  onNav?: (tab: TabId) => void;
  onOpenMarket?: () => void;
  lang?: Lang;
} = {}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  const [localTab, setLocalTab] = useState<TabId>("skills");
  const tab: TabId = isTab(routeTab) ? routeTab : localTab;
  const setTab = (next: TabId) => {
    setLocalTab(next);
    onNav?.(next);
  };

  const { projects: projectRows } = useProjectsStore();
  const projects = useMemo(
    () => (projectRows ?? []).map((project) => project.slug).filter(Boolean),
    [projectRows],
  );
  const [selectedSlug, setSelectedSlug] = useState("");
  const slug = projects.includes(selectedSlug) ? selectedSlug : (projects[0] ?? "");
  const [roles, setRoles] = useState<RoleSummary[]>([]);
  const [skills, setSkills] = useState<DecoratedPlugin[]>([]);
  const [evolution, setEvolution] = useState<EvolutionSummary | null>(null);
  const [loading, setLoading] = useState(false);
  // v0.8.24 gap-fill — MCP servers page.
  const [mcp, setMcp] = useState<McpServersResponse | null>(null);
  const [mcpForm, setMcpForm] = useState({ name: "", url: "", command: "", args: "" });
  const [mcpBusy, setMcpBusy] = useState(false);

  const refreshTab = useCallback(async () => {
    if (!slug) return;
    setLoading(true);
    try {
      if (tab === "roles") {
        setRoles(await listProjectRoles(slug));
      } else if (tab === "skills") {
        const idx = await getProjectMarketplace(slug);
        setSkills((idx.plugins ?? []).filter((p) => p.type === "skill"));
      } else if (tab === "market") {
        return;
      } else if (tab === "evolution") {
        setEvolution(await getEvolution(slug));
      } else if (tab === "mcp") {
        setMcp(await getMcpServers(slug));
      }
    } catch (e) {
      toastBus.handler?.error(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [slug, tab]);

  useEffect(() => {
    let cancelled = false;
    queueMicrotask(() => {
      if (!cancelled) void refreshTab();
    });
    return () => {
      cancelled = true;
    };
  }, [refreshTab]);

  // v0.8.24 F1.12 — register a third-party MCP server (idempotent config
  // write server-side; ccteam never executes/downloads it). Templates below
  // only PREFILL this form — nothing runs until 注册 is clicked, and even
  // then only the project `.mcp.json` is written.
  const onRegisterMcp = async () => {
    const name = mcpForm.name.trim();
    const url = mcpForm.url.trim();
    const command = mcpForm.command.trim();
    if (!slug || !name || (!url && !command)) {
      toastBus.handler?.error(
        tr(lang, "填写 name + url(或 command)", "Fill name + url (or command)", "Заполните name и url (или command)"),
      );
      return;
    }
    setMcpBusy(true);
    try {
      await registerMcpServer(slug, {
        name,
        url: url || undefined,
        command: command || undefined,
        args: mcpForm.args.trim() ? mcpForm.args.trim().split(/\s+/) : undefined,
      });
      setMcpForm({ name: "", url: "", command: "", args: "" });
      setMcp(await getMcpServers(slug));
      toastBus.handler?.info(
        tr(lang, `已写入 .mcp.json:${name}(vendor 下次启动生效)`, `Wrote .mcp.json: ${name}`, `Записано в .mcp.json: ${name}`),
      );
    } catch (e) {
      toastBus.handler?.error(e instanceof Error ? e.message : String(e));
    } finally {
      setMcpBusy(false);
    }
  };

  const projectSelect = useMemo(
    () => (
      <select
        value={slug}
        onChange={(e) => setSelectedSlug(e.target.value)}
        data-testid="workflow-project"
        className="btn ghost"
        style={{ padding: "6px 10px", fontSize: 12.5 }}
      >
        {projects.length === 0 ? <option value="">{tr(lang, "(无项目)", "(no projects)", "(нет проектов)")}</option> : null}
        {projects.map((p) => (
          <option key={p} value={p}>
            {p}
          </option>
        ))}
      </select>
    ),
    [projects, slug, lang],
  );

  const detailHeader = (title: React.ReactNode, desc: React.ReactNode) => (
    <header style={{ display: "flex", alignItems: "flex-start", gap: 14 }}>
      <div style={{ flex: 1 }}>
        <h1>{title}</h1>
        <p>{desc}</p>
      </div>
      {projectSelect}
    </header>
  );

  const flowRow = (name: React.ReactNode, desc: React.ReactNode, end: React.ReactNode, key: string) => (
    <div className="flow-row" key={key}>
      <span className="n">{name}</span>
      <span className="d">{desc}</span>
      <span className="end">{end}</span>
    </div>
  );

  return (
    <section className="view active row" data-testid="workflow-view">
      <div className="set-nav" data-testid="flow-nav">
        <h2>{t("flowTitle")}</h2>
        {TABS.map((it) => (
          <button
            key={it.id}
            type="button"
            data-testid={`workflow-tab-${it.id}`}
            className={`set-item ${tab === it.id ? "active" : ""}`}
            onClick={() => setTab(it.id)}
          >
            {it.icon}
            {t(it.labelKey)}
            <span className="sub">{t(it.subKey)}</span>
          </button>
        ))}
      </div>

      <div className="set-detail">
        <div className="set-detail-inner fade-in" key={tab}>
          {loading ? <p style={{ color: "var(--text-faint)", fontSize: 13 }}>{t("loading")}</p> : null}

          {tab === "skills" ? (
            <>
              {detailHeader(
                t("workflowSkillsTitle"),
                lang === "zh" ? (
                  <>
                    当前项目的技能库(<code>.claude/skills/</code>)—— 会话内按触发词自动调用;可从插件市场安装。
                  </>
                ) : lang === "ru" ? (
                  <>
                    Библиотека навыков текущего проекта (<code>.claude/skills/</code>) — навыки автоматически
                    вызываются в сессиях; установить их можно из маркетплейса.
                  </>
                ) : (
                  <>
                    This project&apos;s skill library (<code>.claude/skills/</code>) — auto-triggered in
                    sessions; installable from the marketplace.
                  </>
                ),
              )}
              {skills.length === 0 && !loading ? (
                <p style={{ fontSize: 13, color: "var(--text-faint)" }}>
                  {tr(
                    lang,
                    "暂无已装 skill(可从设置→插件市场安装)。",
                    "No installed skills (install from Settings → Marketplace).",
                    "Установленных навыков нет (установите их в разделе «Настройки → Маркетплейс»).",
                  )}
                </p>
              ) : (
                <div className="flow-rows">
                  {skills.map((s) =>
                    flowRow(
                      s.id,
                      s.name || s.description || "",
                      s.installed_status === "installed" ? (
                        <span className="badge ok">{t("installed")}</span>
                      ) : (
                        <button
                          type="button"
                          className="btn primary mini"
                          onClick={() => onOpenMarket?.()}
                        >
                          {t("goMarket")}
                        </button>
                      ),
                      s.id,
                    ),
                  )}
                </div>
              )}
              <div>
                <button type="button" className="btn ghost" onClick={() => onOpenMarket?.()}>
                  {t("browseMarket")}
                </button>
              </div>
            </>
          ) : null}

          {tab === "roles" ? (
            <>
              {detailHeader(
                t("workflowRolesTitle"),
                lang === "zh" ? (
                  <>
                    角色库(<code>.claude/agents/&lt;role&gt;.md</code>)—— spawn 时绑 <code>--agent</code>
                    ,会话内 <code>/role</code> 原地切换。
                  </>
                ) : lang === "ru" ? (
                  <>
                    Библиотека ролей (<code>.claude/agents/&lt;role&gt;.md</code>) — при spawn роль
                    привязывается через <code>--agent</code>; в сессии её можно сменить командой <code>/role</code>.
                  </>
                ) : (
                  <>
                    Role library (<code>.claude/agents/&lt;role&gt;.md</code>) — bound at spawn via{" "}
                    <code>--agent</code>; switch in-session with <code>/role</code>.
                  </>
                ),
              )}
              {roles.length === 0 && !loading ? (
                <p style={{ fontSize: 13, color: "var(--text-faint)" }}>
                  {tr(lang, "暂无 role 文件。", "No role files.", "Файлов ролей нет.")}
                </p>
              ) : (
                <div className="flow-rows">
                  {roles.map((r) =>
                    flowRow(
                      r.role,
                      r.description || "",
                      r.role === "cto" ? (
                        <span className="badge brand">{t("workflowBuiltIn")}</span>
                      ) : (
                        <span className="badge ok">{t("installed")}</span>
                      ),
                      r.role,
                    ),
                  )}
                </div>
              )}
              <div>
                <button type="button" className="btn ghost" onClick={() => onOpenMarket?.()}>
                  {t("installMarket")}
                </button>
              </div>
            </>
          ) : null}

          {tab === "market" ? (
            <>
              <header>
                <h1>{t("setMarket")}</h1>
                <p>{t("marketDesc")}</p>
              </header>
              <MarketplaceView embedded />
            </>
          ) : null}

          {tab === "mcp" ? (
            <>
              {detailHeader(
                t("workflowMcpTitle"),
                lang === "zh" ? (
                  <>
                    注册进各 vendor 配置的工具服务器;ccteam 自身 = 8 个 <code>mcp__ccteam__*</code> 工具,默认
                    stream-json 会话经 curated mcp-config 注入。
                  </>
                ) : lang === "ru" ? (
                  <>
                    Серверы инструментов, зарегистрированные в конфигурациях vendor; сам ccteam предоставляет
                    8 инструментов <code>mcp__ccteam__*</code>, которые добавляются в stream-json-сессии через
                    curated mcp-config.
                  </>
                ) : (
                  <>
                    Tool servers registered into each vendor&apos;s config; ccteam itself = 8{" "}
                    <code>mcp__ccteam__*</code> tools, injected into stream-json sessions via the curated
                    mcp-config.
                  </>
                ),
              )}
              <div className="flow-rows" data-testid="mcp-rows">
                {flowRow(
                  "ccteam",
                  tr(
                    lang,
                    "8 tools · status(+grok_claude_codex_kimi) / chat_send_file / session_* · doctor --verify-mcp 自检",
                    "8 tools · status(+grok_claude_codex_kimi) / chat_send_file / session_* · doctor --verify-mcp",
                    "8 инструментов · status(+grok_claude_codex_kimi) / chat_send_file / session_* · doctor --verify-mcp",
                  ),
                  mcp?.ccteam_registered ? (
                    <span className="badge ok">{t("mcpOk")}</span>
                  ) : (
                    <span
                      className="badge"
                      title={tr(
                        lang,
                        "默认 stream-json 会话经 curated mcp-config 注入,不依赖项目 .mcp.json",
                        "curated mcp-config injects it per session",
                        "По умолчанию добавляется в каждую stream-json-сессию через curated mcp-config и не зависит от .mcp.json",
                      )}
                    >
                      {tr(lang, "随会话注入", "per-session", "в каждой сессии")}
                    </span>
                  ),
                  "ccteam",
                )}
                {(mcp?.servers ?? [])
                  .filter((sv) => !sv.is_ccteam)
                  .map((sv) =>
                    flowRow(
                      sv.name,
                      sv.url
                        ? `${sv.kind} · ${sv.url}`
                        : `${sv.kind} · ${sv.command ?? ""} ${(sv.args ?? []).join(" ")}`.trim(),
                      <span className="badge ok">{tr(lang, "已注册", "registered", "зарегистрирован")}</span>,
                      sv.name,
                    ),
                  )}
              </div>
              <div className="form" data-testid="mcp-register-form">
                  <label style={{ fontSize: 13, fontWeight: 600 }}>
                    {tr(lang, "注册第三方 MCP server", "Register a third-party MCP server", "Зарегистрировать сторонний MCP-сервер")}
                  </label>
                  <p style={{ fontSize: 12.5, color: "var(--text-faint)", margin: 0 }}>
                    {tr(
                      lang,
                      "幂等写入项目根 .mcp.json(vendor 原生配置,Claude Code 下次启动读取);ccteam 不下载、不执行任何内容。",
                      "Idempotently writes the project .mcp.json (vendor-native; Claude Code reads it on next start); ccteam downloads/executes nothing.",
                      "Идемпотентно записывает .mcp.json в корень проекта (нативная конфигурация vendor; Claude Code прочитает её при следующем запуске); ccteam ничего не скачивает и не выполняет.",
                    )}
                  </p>
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                    <input
                      type="text"
                      data-testid="mcp-name"
                      placeholder="name"
                      value={mcpForm.name}
                      onChange={(e) => setMcpForm((f) => ({ ...f, name: e.target.value }))}
                      style={{ width: 140 }}
                    />
                    <input
                      type="text"
                      data-testid="mcp-url"
                      placeholder={tr(lang, "url(http 型)", "url (http)", "url (http)")}
                      value={mcpForm.url}
                      onChange={(e) => setMcpForm((f) => ({ ...f, url: e.target.value }))}
                      style={{ width: 240 }}
                    />
                    <input
                      type="text"
                      data-testid="mcp-command"
                      placeholder={tr(lang, "command(stdio 型)", "command (stdio)", "command (stdio)")}
                      value={mcpForm.command}
                      onChange={(e) => setMcpForm((f) => ({ ...f, command: e.target.value }))}
                      style={{ width: 160 }}
                    />
                    <input
                      type="text"
                      data-testid="mcp-args"
                      placeholder="args…"
                      value={mcpForm.args}
                      onChange={(e) => setMcpForm((f) => ({ ...f, args: e.target.value }))}
                      style={{ width: 200 }}
                    />
                    <button
                      type="button"
                      className="btn primary mini"
                      data-testid="mcp-register"
                      disabled={mcpBusy}
                      onClick={() => void onRegisterMcp()}
                    >
                      {mcpBusy
                        ? tr(lang, "写入中…", "Writing…", "Запись…")
                        : tr(lang, "注册", "Register", "Зарегистрировать")}
                    </button>
                  </div>
                  <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                    <span style={{ fontSize: 12, color: "var(--text-faint)" }}>
                      {tr(lang, "建议模板(仅填表,不执行):", "Templates (prefill only):", "Шаблоны (только заполнение, без запуска):")}
                    </span>
                    <button
                      type="button"
                      className="btn ghost mini"
                      data-testid="mcp-tpl-context7"
                      onClick={() =>
                        setMcpForm({
                          name: "context7",
                          url: "https://mcp.context7.com/mcp",
                          command: "",
                          args: "",
                        })
                      }
                    >
                      context7
                    </button>
                    <button
                      type="button"
                      className="btn ghost mini"
                      data-testid="mcp-tpl-playwright"
                      onClick={() =>
                        setMcpForm({
                          name: "playwright",
                          url: "",
                          command: "npx",
                          args: "@playwright/mcp@latest",
                        })
                      }
                    >
                      playwright
                    </button>
                  </div>
              </div>
            </>
          ) : null}

          {tab === "evolution" ? (
            <>
              {detailHeader(
                t("evolve"),
                tr(
                  lang,
                  "人工评价闭合反馈循环:接受/需修改会被记录,并按 role / skill 指纹版本展示。只有你确认后才会请求 agent 提出改进;不会自动修改任何内容。",
                  "Human verdicts close the feedback loop: accepted/revised results are recorded by role and skill fingerprint revision. Improve asks the agent for a proposal only after your confirmation. Nothing changes automatically.",
                  "Оценки человека замыкают обратную связь: принятое и отправленное на доработку учитывается по ревизиям отпечатков ролей и навыков. Улучшение запрашивает у агента только предложение и только после вашего подтверждения. Ничего не меняется автоматически.",
                ),
              )}
              <EvolutionPanel lang={lang} evolution={evolution} loading={loading} />
            </>
          ) : null}

        </div>
      </div>
    </section>
  );
}
