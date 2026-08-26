import type { StatusSnapshot } from "../lib/statusApi";
import { useStatusStore } from "../hooks/useStatusStore";
import { SkeletonRows } from "../components/ui";
import {
  budgetSeverity,
  formatCostBudget,
  formatUsd,
  vendorCostSplit,
} from "../lib/marketplaceFormat";
import { tr, type Lang } from "../lib/i18n";

/** `embedded` — hide the page header (Ops panel already owns the title). */
export default function StatusView({ lang, embedded = false }: { lang: Lang; embedded?: boolean }) {
  const { data: status, loading, error } = useStatusStore();

  return (
    <div data-testid="status-view" className="flex flex-col gap-3">
      {embedded ? null : (
        <header>
          <h1>{tr(lang, "Status · 运维总览", "Status · Operations overview", "Статус · обзор операций")}</h1>
          <p>{tr(lang, "daemon 健康 · 会话 · 今日成本 / 预算。", "Daemon health · sessions · today's cost / budget.", "Состояние daemon · сессии · сегодняшние расходы / бюджет.")}</p>
        </header>
      )}
      <div className="space-y-3">
        {loading && status === null ? (
          <div data-testid="status-loading"><SkeletonRows rows={3} /></div>
        ) : status === null ? (
          <div data-testid="status-error" role="alert" className="rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-4 text-sm text-status-error">
            {tr(lang, "加载状态失败", "Failed to load status", "Не удалось загрузить статус")}: {error ?? tr(lang, "加载失败", "Load failed", "Загрузка не удалась")}
          </div>
        ) : (
          <StatusCards lang={lang} status={status} />
        )}
      </div>
    </div>
  );
}

export function StatusCards({ lang, status }: { lang: Lang; status: StatusSnapshot }) {
  const severity = budgetSeverity(status.cost_24h_usd, status.budget_cap_24h);
  const vendorSplit = vendorCostSplit(status.cost_24h_by_vendor);
  return (
    <>
      {/* Daemon health leads — full-width strip so operators see it first. */}
      <div
        className={`daemon-strip ${status.daemon_healthy ? "ok" : "down"}`}
        data-testid="status-daemon"
      >
        <span className={`dot ${status.daemon_healthy ? "on" : "off"}`} />
        <span className="daemon-strip-title">
          {status.daemon_healthy
            ? tr(lang, "daemon 正常", "daemon healthy", "daemon работает")
            : tr(lang, "daemon 已停止", "daemon down", "daemon недоступен")}
        </span>
        <span className="daemon-strip-sub">
          {status.daemon_healthy
            ? tr(lang, "MCP sock 正常", "MCP sock OK", "сокет MCP работает")
            : tr(lang, "MCP sock 不可达", "MCP sock unreachable", "сокет MCP недоступен")}
        </span>
      </div>
      <div className="stat-grid">
        <div className="stat" data-testid="status-session-stat">
          <span className="k">{tr(lang, "会话", "Sessions", "Сессии")}</span>
          <span className="v">
            {status.sessions_live} <span className="u">{tr(lang, "活跃 ·", "live ·", "активно ·")}</span> {status.sessions_idle}{" "}
            <span className="u">{tr(lang, "空闲", "idle", "ожидают")}</span>
          </span>
          <span className="k">{tr(lang, "共", "Total", "Всего")} {status.sessions_live + status.sessions_idle}</span>
        </div>
        <div className="stat" data-testid="status-cost">
          <span className="k">{tr(lang, "今日成本", "Today's cost", "Расходы сегодня")}</span>
          <span className="v" style={{ color: severity === "over" ? "var(--red-text)" : severity === "warn" ? "#B45309" : undefined }}>
            {formatCostBudget(status.cost_24h_usd, status.budget_cap_24h)}
          </span>
          <span className="k">{vendorSplit.length > 0 ? vendorSplit.join(" · ") : tr(lang, "本窗口暂无计费记录。", "No billed usage in this window.", "За это время нет учтённых расходов.")}</span>
        </div>
      </div>
      {status.budget_cap_24h !== null && severity !== "ok" ? (
        <div data-testid="status-budget-warn" role="status" className={`badge ${severity === "over" ? "warn" : ""}`} style={{ padding: "8px 12px", borderRadius: 10, fontSize: 12 }}>
          {severity === "over"
            ? tr(lang, `已达/超 24h 预算（${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}）— 接近上限会自停（红线）。`, `24h budget reached/exceeded (${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}) — work stops near the limit.`, `Достигнут/превышен бюджет за 24 ч (${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}) — работа остановится у лимита.`)
            : tr(lang, `接近 24h 预算（${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}）。`, `Approaching 24h budget (${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}).`, `Приближение к бюджету за 24 ч (${formatUsd(status.cost_24h_usd)} / ${formatUsd(status.budget_cap_24h)}).`)}
        </div>
      ) : null}
    </>
  );
}
