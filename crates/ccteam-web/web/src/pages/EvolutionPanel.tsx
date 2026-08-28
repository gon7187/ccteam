import { tr, type Lang } from "../lib/i18n";
import type {
  EvolutionBucket,
  EvolutionMetrics,
  EvolutionSummary,
} from "../lib/workflowApi";

function count(value: unknown): string {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? String(value)
    : "—";
}

function duration(value: number | null | undefined): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return "—";
  if (value < 1000) return `${Math.round(value)} ms`;
  if (value < 60_000) {
    const seconds = (value / 1000).toFixed(value < 10_000 ? 1 : 0).replace(/\.0$/, "");
    return `${seconds} s`;
  }
  const minutes = Math.floor(value / 60_000);
  const seconds = Math.round((value % 60_000) / 1000);
  return seconds ? `${minutes}m ${seconds}s` : `${minutes}m`;
}

function usd(value: number | null | undefined): string | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? `$${value.toFixed(2)}`
    : null;
}

function feedbackLine(lang: Lang, metrics: EvolutionMetrics): string {
  const accepted = count(metrics.accepted_turns);
  const revised = count(metrics.revised_turns);
  const unrated = count(metrics.unrated_turns);
  return tr(
    lang,
    `${accepted} 已接受 · ${revised} 需修改 · ${unrated} 未评价`,
    `${accepted} accepted · ${revised} revised · ${unrated} unrated`,
    `${accepted} принято · ${revised} на доработку · ${unrated} без оценки`,
  );
}

function outcomeLine(lang: Lang, metrics: EvolutionMetrics): string {
  const completed = count(metrics.completed_turns);
  const failed = count(metrics.failed_turns);
  const unknown = count(metrics.outcome_unknown_turns);
  return tr(
    lang,
    `${completed} 已完成 · ${failed} 失败 · ${unknown} 未知`,
    `${completed} completed · ${failed} failed · ${unknown} unknown`,
    `${completed} завершено · ${failed} с ошибкой · ${unknown} неизвестно`,
  );
}

function pricingLine(lang: Lang, metrics: EvolutionMetrics): string {
  const priced = count(metrics.priced_turns);
  const unpriced = count(metrics.unpriced_turns);
  return tr(
    lang,
    `${priced} 已计价 · ${unpriced} 未计价`,
    `${priced} priced · ${unpriced} unpriced`,
    `${priced} с ценой · ${unpriced} без цены`,
  );
}

function costLine(lang: Lang, bucket: EvolutionBucket): string {
  const known = usd(bucket.known_cost_usd);
  const total = usd(bucket.total_cost_usd);
  const average = usd(bucket.priced_avg_cost_usd);
  const mixed = typeof bucket.unpriced_turns === "number" && bucket.unpriced_turns > 0;
  if (mixed && known) {
    return tr(
      lang,
      `≥${known} 已知 · 总计 — · ${average ?? "—"} 已计价平均`,
      `≥${known} known · total — · ${average ?? "—"} priced avg`,
      `≥${known} известно · всего — · ${average ?? "—"} среднее с ценой`,
    );
  }
  return tr(
    lang,
    `${known ?? "—"} 已知 · 总计 ${total ?? "—"} · ${average ?? "—"} 已计价平均`,
    `${known ?? "—"} known · total ${total ?? "—"} · ${average ?? "—"} priced avg`,
    `${known ?? "—"} известно · всего ${total ?? "—"} · ${average ?? "—"} среднее с ценой`,
  );
}

function bucketDetail(lang: Lang, bucket: EvolutionBucket): string {
  const parts = [
    tr(lang, `turns=${count(bucket.turn_count)}`, `turns=${count(bucket.turn_count)}`, `ходов=${count(bucket.turn_count)}`),
    feedbackLine(lang, bucket),
    outcomeLine(lang, bucket),
    pricingLine(lang, bucket),
    tr(lang, `平均 ${duration(bucket.avg_duration_ms)}`, `avg ${duration(bucket.avg_duration_ms)}`, `среднее ${duration(bucket.avg_duration_ms)}`),
  ];
  parts.push(costLine(lang, bucket));
  return parts.join(" · ");
}

export default function EvolutionPanel({
  lang,
  evolution,
  loading,
  error,
}: {
  lang: Lang;
  evolution: EvolutionSummary | null;
  loading: boolean;
  error?: string | null;
}) {
  if (error && !loading) {
    return (
      <p style={{ fontSize: 13, color: "var(--danger)" }} data-testid="evolution-degraded">
        {tr(
          lang,
          `自进化数据不可用:${error}。为避免显示不完整统计,此处不会伪装成空数据。`,
          `Evolution data is unavailable: ${error}. Partial analytics are not shown as an empty dataset.`,
          `Данные самообучения недоступны: ${error}. Неполная аналитика не выдаётся за пустой набор.`,
        )}
      </p>
    );
  }
  if (!evolution || evolution.empty) {
    return !loading ? (
      <p style={{ fontSize: 13, color: "var(--text-faint)" }} data-testid="evolution-empty">
        {tr(
          lang,
          "尚无 experience 数据(诚实空态)。",
          "No experience data yet (honest empty state).",
          "Данных об опыте пока нет (честное пустое состояние).",
        )}
      </p>
    ) : null;
  }

  const buckets = [...evolution.roles, ...evolution.skills];
  return (
    <>
      <div className="stat-grid" data-testid="evolution-summary">
        <div className="stat" data-testid="evolution-feedback">
          <span className="k">{tr(lang, "人工评价", "human verdicts", "оценки человека")}</span>
          <span className="v">{feedbackLine(lang, evolution)}</span>
          <span className="k">
            {tr(lang, "最近 7 天", "last 7 days", "за последние 7 дней")} +{count(evolution.turn_records_7d)}
          </span>
        </div>
        <div className="stat" data-testid="evolution-outcomes">
          <span className="k">{tr(lang, "结果", "outcomes", "результаты")}</span>
          <span className="v">{outcomeLine(lang, evolution)}</span>
          <span className="k">{count(evolution.turn_records)} {tr(lang, "条记录", "turn records", "записей ходов")}</span>
        </div>
        <div className="stat" data-testid="evolution-pricing">
          <span className="k">{tr(lang, "计价覆盖", "pricing coverage", "покрытие ценами")}</span>
          <span className="v">{pricingLine(lang, evolution)}</span>
        </div>
        <div className="stat" data-testid="evolution-duration">
          <span className="k">{tr(lang, "平均时长", "average duration", "средняя длительность")}</span>
          <span className="v">{duration(evolution.avg_duration_ms)}</span>
        </div>
      </div>

      <p style={{ fontSize: 12, color: "var(--text-faint)", margin: "12px 0" }} data-testid="skill-attribution">
        {evolution.skill_attribution === "available_at_spawn"
          ? tr(
              lang,
              "技能仅按会话启动时可用的版本归因;这不证明 agent 实际调用过它。",
              "Skills are attributed to what was available when the session spawned; this does not prove the agent used them.",
              "Навыки привязаны к версиям, доступным при запуске сессии; это не доказывает, что агент их использовал.",
            )
          : tr(
              lang,
              "此 daemon 不提供技能归因口径。",
              "Skill attribution is unavailable on this daemon.",
              "Этот daemon не сообщает методику атрибуции навыков.",
            )}
      </p>

      <div
        className="flow-rows"
        aria-label={tr(lang, "指纹版本", "fingerprint revisions", "ревизии отпечатков")}
      >
        {buckets.map((bucket, index) => (
          <div
            className="flow-row"
            key={`${bucket.kind}-${bucket.id}-${bucket.sha}-${index}`}
            data-testid={`evolution-bucket-${bucket.kind}-${index}`}
          >
            <span className="n">{bucket.kind}:{bucket.id || "(default)"}</span>
            <span className="d">{bucketDetail(lang, bucket)}</span>
            <span className="end">
              <span className="badge">{bucket.sha ? bucket.sha.slice(0, 10) : tr(lang, "未知版本", "unknown revision", "неизвестная ревизия")}</span>
            </span>
          </div>
        ))}
      </div>
    </>
  );
}
