// v0.8.24 Track A — Home landing page (prototype `#view-home`): SSR smoke.
// The lazy-create funnel's pure pieces (slugFromPath / wireProtocol /
// modelSwitchFor) are unit-tested in lib/vendors.test.ts; here we prove the
// page structure: 开工吧! + ctx-bar (项目/角色; 主机 hidden until real host
// data resolves; 分支 hidden — no backend data, never mocked) + composer +
// the 快速开始 template grid (recents live in the sidebar rail).
//
// v0.9.11 TEAM-3: the grid renders the shared 编队起手 formation playbooks
// (HomeView now reads router state → MemoryRouter wraps every render); the
// Team→Home handoff's applied patch is pure-helper-tested in
// lib/playbooks.test.ts (`applyPlaybook` / `playbookFromState`) because SSR
// renderToString never runs effects.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  if (typeof g.window === "undefined") {
    g.window = { innerWidth: 1024, addEventListener() {}, removeEventListener() {} };
  }
  if (typeof g.localStorage === "undefined") {
    g.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  }
});

import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";

import HomeView, { NewProjectFields } from "./HomeView";
import type { HostSummary } from "../lib/hostsApi";
import { completeHomeLaunch, createAndSubmitHomeTurn } from "../lib/playbooks";
import { toastBus } from "../lib/toastBus";

function render() {
  return renderToString(
    <MemoryRouter>
      <HomeView
        lang="zh"
        projects={["ccteam", "demo"]}
        projectPaths={{ ccteam: "~/rob/ccteam" }}
        onLaunched={() => {}}
        onOpenSettings={() => {}}
      />
    </MemoryRouter>,
  );
}

describe("HomeView (landing page)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders 开工吧! + the lazy-create subtitle", () => {
    const html = render();
    expect(html).toContain('data-testid="home-view"');
    expect(html).toContain("开工吧!");
    expect(html).toContain("会话在第一条消息发出时创建");
  });

  it("ctx-bar: 项目 + bound 主机 + 角色 render; 分支 hides without data", () => {
    const html = render();
    expect(html).toContain('data-testid="ctx-project"');
    expect(html).toContain('data-testid="ctx-role"');
    // Project identity is available before the shared host probe resolves.
    expect(html).toContain('data-testid="ctx-host"');
    // v0.8.24 Q7 — 分支 renders ONLY from real backend data (current_branch);
    // without it the dimension stays hidden (never mocked).
    expect(html).not.toContain('data-testid="ctx-branch"');
  });

  it("ctx-bar: 分支 shows READ-ONLY when the project reports current_branch", () => {
    const html = renderToString(
      <MemoryRouter>
        <HomeView
          lang="zh"
          projects={["ccteam"]}
          projectPaths={{ ccteam: "~/rob/ccteam" }}
          projectBranches={{ ccteam: "dev" }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="ctx-branch"');
    const seg = html.slice(html.indexOf('data-testid="ctx-branch"'));
    expect(seg).toContain("dev");
    // Display-only: a <span>, not a dropdown trigger button.
    expect(seg.slice(0, 200)).not.toContain("<button");
  });

  it("角色 picker is available to tenants", () => {
    const tenant = render(false);
    expect(tenant).toContain('data-testid="ctx-role"');
    expect(tenant).toContain('data-testid="ctx-project"');
  });

  it("composer carries the HITL pill + model button + 随心输入 placeholder", () => {
    const html = render();
    expect(html).toContain('data-testid="hitl-toggle"');
    expect(html).toContain("请求批准");
    expect(html).toContain('data-testid="model-btn"');
    expect(html).toContain("随心输入");
    expect(html).toContain('data-testid="home-send"');
  });

  it("shows the inline 新建项目 row (hidden until opened) with the path input", () => {
    const html = render();
    expect(html).toContain('data-testid="newproj"');
    expect(html).toContain("新建项目路径");
    expect(html).toContain('id="newproj-path"');
    expect(html).toContain('data-testid="newproj-host"');
  });

  it("new-project fields render every eligible host and a remote absolute-path hint", () => {
    const hosts: HostSummary[] = [
      { host: "local", hostname: "box", is_local: true, status: "online", agent_count: 1, agents_ready: 1 },
      { host: "claude-dev-04", hostname: "dev04", is_local: false, status: "online", agent_count: 1, agents_ready: 1 },
    ];
    const html = renderToString(
      <NewProjectFields
        lang="en"
        open
        hosts={hosts}
        host="claude-dev-04"
        onHostChange={() => {}}
        onPathChange={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(html).toContain('value="local"');
    expect(html).toContain('value="claude-dev-04" selected=""');
    expect(html).toContain('placeholder="Absolute path on claude-dev-04"');
  });

  it("project options wear their remote host and offline state", () => {
    const html = renderToString(
      <MemoryRouter>
        <HomeView
          lang="en"
          projects={["remote-proj"]}
          projectPaths={{ "remote-proj": "/srv/remote-proj" }}
          projectHosts={{ "remote-proj": { host: "sat-2", online: false } }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html.replace(/<!-- -->/g, "")).toContain("@ sat-2");
    expect(html).toContain("offline");
    expect(html).toContain('disabled=""');
  });

  it("renders the 快速开始 grid: the 6 shared 编队起手 formation playbooks", () => {
    const html = render();
    expect(html).toContain('data-testid="template-grid"');
    expect(html).toContain("快速开始");
    for (const id of ["commander", "advisor", "crossreview", "bakeoff", "triangulate", "pyramid"]) {
      expect(html).toContain(`data-testid="tpl-${id}"`);
    }
    expect(html).toContain("指挥官");
    expect(html).toContain("主力-顾问");
    expect(html).toContain("交叉互审");
    expect(html).toContain("并行竞标");
    expect(html).toContain("调研三角");
    expect(html).toContain("金字塔用工");
    // The card carries its composer prompt as the hover title; the commander
    // flagship's prompt drives real A2A delegation (session_spawn/dispatch).
    expect(html).toContain("session_spawn");
    expect(html).toContain("择优合并成最终答案");
    // The old recents grid is gone (recents live in the sidebar rail), and
    // the retired single-vendor cards (code/fast/bulk era) don't resurface.
    expect(html).not.toContain('data-testid="recent-grid"');
    expect(html).not.toContain('data-testid="tpl-team"');
    expect(html).not.toContain('data-testid="tpl-code"');
  });

  it("formation cards wear their harness brand chips", () => {
    const html = render();
    const grid = html.slice(html.indexOf('data-testid="template-grid"'));
    // The deck spans all five harnesses.
    for (const vendor of ["claude", "codex", "grok", "kimi", "opencode"]) {
      expect(grid).toContain(`data-vendor="${vendor}"`);
    }
    // The commander fields the Claude brain + Codex crews + GLM scout.
    const commander = grid.slice(
      grid.indexOf('data-testid="tpl-commander"'),
      grid.indexOf('data-testid="tpl-advisor"'),
    );
    for (const vendor of ["claude", "codex", "opencode"]) {
      expect(commander).toContain(`data-vendor="${vendor}"`);
    }
    expect(commander).not.toContain('data-vendor="grok"');
    // 金字塔用工 leads cheap (kimi/opencode) and escalates to claude.
    const pyramid = grid.slice(grid.indexOf('data-testid="tpl-pyramid"'));
    for (const vendor of ["kimi", "opencode", "claude"]) {
      expect(pyramid).toContain(`data-vendor="${vendor}"`);
    }
  });

  it("template cards speak the shell language (en)", () => {
    const html = renderToString(
      <MemoryRouter>
        <HomeView
          lang="en"
          projects={["ccteam"]}
          projectPaths={{ ccteam: "~/rob/ccteam" }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html).toContain("Quick start");
    expect(html).toContain("Commander");
    expect(html).toContain("Driver + advisor");
    expect(html).toContain("Cross review");
    expect(html).toContain("Pyramid staffing");
  });

  it("mounts under the Team page 起手 handoff state (one-shot router state)", () => {
    // SSR never runs effects, so the applied composer patch itself is
    // pure-helper-tested in lib/playbooks.test.ts (applyPlaybook /
    // playbookFromState); this proves the page accepts the handoff entry.
    const html = renderToString(
      <MemoryRouter initialEntries={[{ pathname: "/", state: { playbook: "commander" } }]}>
        <HomeView
          lang="zh"
          projects={["ccteam"]}
          projectPaths={{ ccteam: "~/rob/ccteam" }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="home-view"');
    expect(html).toContain('data-testid="tpl-commander"');
  });

  it("retries a failed Commander bootstrap once through the best installed Codex posture", async () => {
    const createSession = vi
      .fn()
      .mockRejectedValueOnce(
        Object.assign(new Error("会话启动失败: invalid reasoning effort `max`"), {
          status: 422,
          errorCode: "EFFORT_UNAVAILABLE",
        }),
      )
      .mockResolvedValueOnce({ sid: "s42" });
    const submit = vi.fn().mockResolvedValue(undefined);
    const prompt = "Commander prompt with the user's concrete task";

    await expect(
      createAndSubmitHomeTurn(
        {
          slug: "ccteam",
          options: {
            role: "",
            vendor: "claude",
            permission_mode: "skip",
            protocol: "stream-json",
            model: "opus",
            effort: "max",
          },
          text: prompt,
          attachments: [],
          commander: true,
          installedVendors: ["claude", "codex"],
          catalog: {
            codex: {
              models: [{ id: "gpt-5.6-codex", efforts: ["low", "high", "xhigh"] }],
              efforts: ["low", "medium", "high", "xhigh"],
            },
          },
        },
        { createSession, submitTurn: submit },
      ),
    ).resolves.toEqual({
      sid: "s42",
      vendor: "codex",
      model: "gpt-5.6-codex",
      effort: "xhigh",
      fallback: true,
    });

    expect(createSession).toHaveBeenCalledTimes(2);
    expect(createSession.mock.calls[0]).toEqual([
      "ccteam",
      {
        role: "",
        vendor: "claude",
        permission_mode: "skip",
        protocol: "stream-json",
        model: "fable",
        effort: "high",
      },
    ]);
    expect(createSession.mock.calls[1]).toEqual([
      "ccteam",
      {
        role: "",
        vendor: "codex",
        permission_mode: "skip",
        protocol: "stream-json",
        model: "gpt-5.6-codex",
        effort: "xhigh",
      },
    ]);
    expect(submit).toHaveBeenCalledOnce();
    expect(submit).toHaveBeenCalledWith("s42", prompt, []);
  });

  it("starts Commander directly on the best confirmed Codex posture when Claude is absent", async () => {
    const createSession = vi.fn().mockResolvedValue({ sid: "s43" });
    const submit = vi.fn().mockResolvedValue(undefined);

    await expect(
      createAndSubmitHomeTurn(
        {
          slug: "ccteam",
          // This is the generic host-normalized posture HomeView currently
          // derives before the Commander policy gets a say.
          options: {
            role: "",
            vendor: "codex",
            permission_mode: "skip",
            protocol: "stream-json",
          },
          text: "task",
          attachments: [],
          commander: true,
          installedVendors: ["codex"],
          catalog: {
            codex: {
              models: [{ id: "gpt-5.6-codex", efforts: ["low", "high", "xhigh"] }],
              efforts: ["low", "medium", "high", "xhigh"],
            },
          },
        },
        { createSession, submitTurn: submit },
      ),
    ).resolves.toEqual({
      sid: "s43",
      vendor: "codex",
      model: "gpt-5.6-codex",
      effort: "xhigh",
      fallback: true,
    });

    expect(createSession).toHaveBeenCalledOnce();
    expect(createSession).toHaveBeenCalledWith("ccteam", {
      role: "",
      vendor: "codex",
      permission_mode: "skip",
      protocol: "stream-json",
      model: "gpt-5.6-codex",
      effort: "xhigh",
    });
    expect(submit).toHaveBeenCalledWith("s43", "task", []);
  });

  it("launches Commander at high effort even when the live catalog advertises a higher Fable rung", async () => {
    const createSession = vi.fn().mockResolvedValue({ sid: "s44" });
    const submit = vi.fn().mockResolvedValue(undefined);

    await expect(
      createAndSubmitHomeTurn(
        {
          slug: "ccteam",
          options: {
            role: "",
            vendor: "claude",
            permission_mode: "skip",
            protocol: "stream-json",
            model: "fable",
            effort: "xhigh",
          },
          text: "task",
          attachments: [],
          commander: true,
          installedVendors: ["claude", "codex"],
          catalog: {
            claude: {
              models: [{ id: "fable", efforts: ["low", "medium", "high", "xhigh"] }],
              efforts: ["low", "medium", "high", "xhigh"],
            },
          },
        },
        { createSession, submitTurn: submit },
      ),
    ).resolves.toEqual({
      sid: "s44",
      vendor: "claude",
      model: "fable",
      effort: "high",
      fallback: false,
    });

    expect(createSession).toHaveBeenCalledOnce();
    expect(createSession).toHaveBeenCalledWith("ccteam", {
      role: "",
      vendor: "claude",
      permission_mode: "skip",
      protocol: "stream-json",
      model: "fable",
      effort: "high",
    });
    expect(submit).toHaveBeenCalledWith("s44", "task", []);
  });

  it("does not apply a cold Fable effort when the live catalog observed only other models", async () => {
    const createSession = vi.fn().mockResolvedValue({ sid: "s45" });
    const submit = vi.fn().mockResolvedValue(undefined);

    await createAndSubmitHomeTurn(
      {
        slug: "ccteam",
        options: {
          role: "",
          vendor: "claude",
          permission_mode: "skip",
          protocol: "stream-json",
          model: "fable",
          effort: "high",
        },
        text: "task",
        attachments: [],
        commander: true,
        installedVendors: ["claude", "codex"],
        catalog: {
          claude: {
            models: [{ id: "sonnet", efforts: ["low", "high", "max"] }],
            efforts: ["low", "medium", "high", "max"],
          },
        },
      },
      { createSession, submitTurn: submit },
    );

    expect(createSession).toHaveBeenCalledWith("ccteam", {
      role: "",
      vendor: "claude",
      permission_mode: "skip",
      protocol: "stream-json",
      model: "fable",
    });
  });

  it("retains the unavailable Commander error instead of launching an unrelated Grok lead", async () => {
    const unavailable = new Error("Claude executable not found");
    const createSession = vi.fn().mockRejectedValue(unavailable);
    const submit = vi.fn();

    await expect(
      createAndSubmitHomeTurn(
        {
          slug: "ccteam",
          // Generic host normalization would otherwise turn Commander into
          // Grok merely because Grok is the first installed vendor.
          options: {
            role: "",
            vendor: "grok",
            permission_mode: "skip",
            protocol: "acp",
          },
          text: "task",
          attachments: [],
          commander: true,
          installedVendors: ["grok"],
          catalog: {},
        },
        { createSession, submitTurn: submit },
      ),
    ).rejects.toBe(unavailable);

    expect(createSession).toHaveBeenCalledOnce();
    expect(createSession).toHaveBeenCalledWith("ccteam", {
      role: "",
      vendor: "claude",
      permission_mode: "skip",
      protocol: "stream-json",
      model: "fable",
      effort: "high",
    });
    expect(submit).not.toHaveBeenCalled();
  });

  it("does not blindly retry Commander auth, network, ACL, or general failures", async () => {
    for (const message of [
      "UNAUTHENTICATED",
      "network: connection failed",
      "network failure: model opus is unavailable",
      "HTTP 403: project is not visible",
      "HTTP 429: model opus is unavailable",
      "HTTP 500: model opus is unavailable",
      "provider overloaded: model opus is unavailable",
      "request timed out while creating the session",
      "quota exceeded: model opus is unavailable",
      "budget guard rejected spawn: model opus is unavailable",
      "delegation depth limit reached: model opus is unavailable",
      "delegation cycle detected: model opus is unavailable",
      "会话启动失败: internal state corrupt",
    ]) {
      const createSession = vi.fn().mockRejectedValue(new Error(message));
      const submit = vi.fn();
      await expect(
        createAndSubmitHomeTurn(
          {
            slug: "ccteam",
            options: {
              role: "",
              vendor: "claude",
              permission_mode: "skip",
              protocol: "stream-json",
              model: "opus",
              effort: "max",
            },
            text: "task",
            attachments: [],
          commander: true,
          installedVendors: ["claude", "codex"],
          catalog: {},
          },
          { createSession, submitTurn: submit },
        ),
      ).rejects.toThrow(message);
      expect(createSession).toHaveBeenCalledOnce();
      expect(submit).not.toHaveBeenCalled();
    }
  });

  it("does not retry the fallback create itself and preserves both sanitized causes", async () => {
    const createSession = vi
      .fn()
      .mockRejectedValueOnce(
        Object.assign(new Error("invalid model `opus`\nBearer primary-secret"), {
          status: 422,
          errorCode: "MODEL_UNAVAILABLE",
        }),
      )
      .mockRejectedValueOnce(new Error("codex start failed\u0000 token=fallback-secret"));
    const submit = vi.fn();
    const failure = await createAndSubmitHomeTurn(
      {
        slug: "ccteam",
        options: {
          role: "",
          vendor: "claude",
          protocol: "stream-json",
          model: "opus",
          effort: "max",
        },
        text: "task",
        attachments: [],
        commander: true,
        installedVendors: ["claude", "codex"],
        catalog: {},
      },
      { createSession, submitTurn: submit },
    ).catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(Error);
    const message = (failure as Error).message;
    expect(message).toContain("primary: invalid model `opus` Bearer [redacted]");
    expect(message).toContain("fallback: codex start failed token=[redacted]");
    expect(message).not.toContain("primary-secret");
    expect(message).not.toContain("fallback-secret");
    expect(message).not.toContain("\n");
    expect(createSession).toHaveBeenCalledTimes(2);
    expect(submit).not.toHaveBeenCalled();
  });

  it("redacts auth headers, cookies, and named credentials from both launch causes", async () => {
    const createSession = vi
      .fn()
      .mockRejectedValueOnce(
        Object.assign(
          new Error(
            "invalid model `opus`\naccess_token=access-secret password=hunter2 secret=shared-secret\n"
            + '{"access_token":"json-access","password":"json-pass","secret":"json-secret","cookie":"json-cookie","set-cookie":"json-set-cookie"}',
          ),
          { status: 422, errorCode: "MODEL_UNAVAILABLE" },
        ),
      )
      .mockRejectedValueOnce(
        new Error(
          "codex start failed\nAuthorization: Basic YmFzaWMtc2VjcmV0\nCookie: sid=cookie-secret; theme=dark\nSet-Cookie: refresh=set-cookie-secret; HttpOnly",
        ),
      );
    const failure = await createAndSubmitHomeTurn(
      {
        slug: "ccteam",
        options: {
          role: "",
          vendor: "claude",
          protocol: "stream-json",
          model: "opus",
          effort: "max",
        },
        text: "task",
        attachments: [],
        commander: true,
        installedVendors: ["claude", "codex"],
        catalog: {},
      },
      { createSession, submitTurn: vi.fn() },
    ).catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(Error);
    const message = (failure as Error).message;
    expect(message).toContain("primary: invalid model `opus`");
    expect(message).toContain("fallback: codex start failed");
    for (const label of [
      "access_token=[redacted]",
      "password=[redacted]",
      "secret=[redacted]",
      "Authorization: [redacted]",
      "Cookie: [redacted]",
      "Set-Cookie: [redacted]",
    ]) {
      expect(message).toContain(label);
    }
    for (const secret of [
      "access-secret",
      "hunter2",
      "shared-secret",
      "json-access",
      "json-pass",
      "json-secret",
      "json-cookie",
      "json-set-cookie",
      "YmFzaWMtc2VjcmV0",
      "cookie-secret",
      "set-cookie-secret",
    ]) {
      expect(message).not.toContain(secret);
    }
  });

  it("surfaces the actual successful posture before navigating", () => {
    const info = vi.fn();
    const onLaunched = vi.fn();
    toastBus.handler = { push: vi.fn(), error: vi.fn(), info };
    try {
      completeHomeLaunch(
        {
          sid: "s42",
          vendor: "codex",
          model: "gpt-5.6-codex",
          effort: "xhigh",
          fallback: true,
        },
        "en",
        info,
        onLaunched,
      );
      expect(info).toHaveBeenCalledWith(
        "Launched s42 · codex · gpt-5.6-codex · xhigh · Commander fallback",
      );
      expect(onLaunched).toHaveBeenCalledWith("s42");
    } finally {
      toastBus.handler = null;
    }
  });

  it("still returns the original non-capability error object without a retry", async () => {
    const original = new Error("HTTP 403: project is not visible");
    const createSession = vi.fn().mockRejectedValue(original);
    const submit = vi.fn();
    await expect(
      createAndSubmitHomeTurn(
        {
          slug: "ccteam",
          options: {
            role: "",
            vendor: "claude",
            protocol: "stream-json",
            model: "opus",
            effort: "max",
          },
          text: "task",
          attachments: [],
          commander: true,
          installedVendors: ["claude", "codex"],
          catalog: {},
        },
        { createSession, submitTurn: submit },
      ),
    ).rejects.toBe(original);
    expect(createSession).toHaveBeenCalledOnce();
    expect(submit).not.toHaveBeenCalled();
  });
});
