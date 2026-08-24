import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import StatusView, { StatusCards } from "./StatusView";
import type { StatusSnapshot } from "../lib/statusApi";

const { statusStore } = vi.hoisted(() => ({
  statusStore: { data: null, loading: true, error: null as string | null },
}));
vi.mock("../hooks/useStatusStore", () => ({ useStatusStore: () => statusStore }));

const realFetch = globalThis.fetch;
const SNAP: StatusSnapshot = {
  daemon_healthy: true,
  sessions_live: 2,
  sessions_idle: 1,
  cost_24h_usd: 2.27,
  cost_24h_by_vendor: { claude: 1.62, codex: 0.52, pi: 0.13 },
  budget_cap_24h: 20,
};

describe("StatusView initial render", () => {
  beforeEach(() => {
    statusStore.data = null;
    statusStore.loading = true;
    statusStore.error = null;
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the loading shell", () => {
    const html = renderToString(<StatusView lang="ru" />);
    expect(html).toContain('data-testid="status-view"');
    expect(html).toContain('data-testid="status-loading"');
  });
});

describe("StatusCards", () => {
  it("leads with the daemon strip, then session/cost tiles (no fleet table)", () => {
    const html = renderToString(<StatusCards lang="ru" status={SNAP} />);
    expect(html).toContain('data-testid="status-daemon"');
    expect(html).toContain("daemon-strip");
    expect(html).toContain('data-testid="status-session-stat"');
    expect(html).toContain('data-testid="status-cost"');
    expect(html).not.toContain('data-testid="status-sessions"');
    expect(html).not.toContain('data-testid="fleet-table"');
    // Daemon health is the first element.
    expect(html.indexOf('data-testid="status-daemon"')).toBeLessThan(
      html.indexOf('data-testid="status-session-stat"'),
    );
  });

  it("shows aggregate live/idle counts and vendor cost split", () => {
    const html = renderToString(<StatusCards lang="ru" status={SNAP} />);
    expect(html).toContain("активно ·");
    expect(html).toContain("ожидают");
    expect(html).toContain("$2.27 / $20.00");
    expect(html).toContain("claude $1.62 · codex $0.52 · pi $0.13");
  });

  it("shows daemon-down and the empty cost line", () => {
    const html = renderToString(<StatusCards lang="ru" status={{ ...SNAP, daemon_healthy: false, cost_24h_usd: 0, cost_24h_by_vendor: {}, budget_cap_24h: null }} />);
    expect(html).toContain("daemon недоступен");
    expect(html).toContain("сокет MCP недоступен");
    expect(html).toContain("За это время нет учтённых расходов.");
  });

  it("retains budget warnings", () => {
    const html = renderToString(<StatusCards lang="ru" status={{ ...SNAP, cost_24h_usd: 21 }} />);
    expect(html).toContain('data-testid="status-budget-warn"');
    expect(html).toContain("Достигнут/превышен бюджет за 24 ч");
  });

  it("keeps Chinese and English status and budget labels localized", () => {
    const zh = renderToString(<StatusCards lang="zh" status={{ ...SNAP, cost_24h_usd: 21 }} />);
    expect(zh).toContain("daemon 正常");
    expect(zh).toContain("已达/超 24h 预算");
    const en = renderToString(<StatusCards lang="en" status={{ ...SNAP, daemon_healthy: false, cost_24h_usd: 21 }} />);
    expect(en).toContain("daemon down");
    expect(en).toContain("24h budget reached/exceeded");
  });

  it("keeps Chinese and English status errors localized", () => {
    statusStore.loading = false;
    statusStore.error = "network error";
    const zh = renderToString(<StatusView lang="zh" />);
    expect(zh).toContain("加载状态失败");
    expect(zh).toContain("network error");
    const en = renderToString(<StatusView lang="en" />);
    expect(en).toContain("Failed to load status");
    expect(en).toContain("network error");
  });
});
