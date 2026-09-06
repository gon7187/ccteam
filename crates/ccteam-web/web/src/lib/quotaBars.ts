// VENDOR-QUOTA-1 — pure presentation helpers for the vendor quota mini bars
// on the Ops & Hosts rows. One window renders as one compact line:
//   `5h ▓▓░░░ 42% · resets in 3h12m`   `Week ▓░░░░ 15% · resets Tue`
// Everything takes `now` explicitly so the vitest suite is deterministic;
// the component passes `new Date()`. Local timezone + app language come from
// `Date` / explicit `lang` — never from the browser locale alone.

import { WEB_LOCALE, type Lang } from "./i18n";
import type { QuotaWindow, QuotaWindowKind, VendorQuota } from "./vendorQuotaApi";

/** The 5-cell bar: round(percent/20) filled cells, clamped to [0, 5]. */
export function quotaBar(usedPercent: number): string {
  const filled = Math.max(0, Math.min(5, Math.round(usedPercent / 20)));
  return "▓".repeat(filled) + "░".repeat(5 - filled);
}

function windowLabel(kind: QuotaWindowKind, lang: Lang): string {
  if (kind === "five_hour") return "5h";
  if (kind === "weekly") return lang === "ru" ? "Неделя" : lang === "en" ? "Week" : "周";
  if (kind === "monthly") return lang === "ru" ? "Месяц" : lang === "en" ? "Month" : "月";
  return lang === "ru" ? "длительность неизвестна" : lang === "en" ? "duration unknown" : "时长未知";
}

function durationLabel(seconds: number | null | undefined, lang: Lang): string | null {
  if (!Number.isFinite(seconds) || !seconds || seconds < 0) return null;
  const duration = compactDuration(seconds * 1_000);
  return lang === "ru" ? `длительность ${duration}` : lang === "en" ? `duration ${duration}` : `时长${duration}`;
}

function usageLabel(usedPercent: number, lang: Lang): string {
  const used = Math.max(0, Math.min(100, Math.round(usedPercent)));
  const remaining = 100 - used;
  if (lang === "ru") return `${used}% использовано · осталось ${remaining}%`;
  if (lang === "zh") return `${used}% 已用 · 剩余 ${remaining}%`;
  return `${used}% used · ${remaining}% remaining`;
}

function observationLine(quota: VendorQuota, now: Date, lang: Lang): string {
  const source = quota.source?.trim();
  const observed = quota.observed_at ? new Date(quota.observed_at) : null;
  const validObserved = observed && !Number.isNaN(observed.getTime());
  const timestamp = validObserved ? observed.toISOString() : null;
  const stale = validObserved && now.getTime() - observed.getTime() > 5 * 60_000;
  const observation = !timestamp
    ? lang === "ru" ? "время наблюдения неизвестно" : lang === "zh" ? "观测时间未知" : "observation time unknown"
    : stale
      ? lang === "ru" ? `устарело с ${timestamp}` : lang === "zh" ? `自 ${timestamp} 起已过期` : `stale since ${timestamp}`
      : lang === "ru" ? `наблюдалось ${timestamp}` : lang === "zh" ? `观测于 ${timestamp}` : `observed ${timestamp}`;
  if (!source) return observation;
  return lang === "ru" ? `источник: ${source} · ${observation}` : lang === "zh" ? `来源：${source} · ${observation}` : `source: ${source} · ${observation}`;
}

/** Compact duration: `42m` / `3h12m` / `2d05h`; negative clamps to `0m`. */
export function compactDuration(ms: number): string {
  const totalMin = Math.max(0, Math.round(ms / 60_000));
  if (totalMin < 60) return `${totalMin}m`;
  const days = Math.floor(totalMin / (60 * 24));
  const hours = Math.floor((totalMin % (60 * 24)) / 60);
  const minutes = totalMin % 60;
  const mm = String(minutes).padStart(2, "0");
  if (days > 0) return `${days}d${String(hours).padStart(2, "0")}h`;
  return minutes > 0 ? `${hours}h${mm}m` : `${hours}h`;
}

/** The reset hint, or null when the vendor reports no reset time.
 *  < 24h → relative (`resets in 3h12m`); < 7d → weekday (`resets Tue`);
 *  beyond → short date. Past timestamps read "resets soon". */
export function resetHint(
  resetsAt: string | null | undefined,
  now: Date,
  lang: Lang,
): string | null {
  if (!resetsAt) return null;
  const at = new Date(resetsAt);
  if (Number.isNaN(at.getTime())) return null;
  const diff = at.getTime() - now.getTime();
  const locale = WEB_LOCALE[lang];
  if (diff <= 0) return lang === "ru" ? "скоро сброс" : lang === "en" ? "resets soon" : "即将重置";
  if (diff < 24 * 3_600_000) {
    if (lang === "ru") {
      const total = Math.max(0, Math.round(diff / 60_000));
      return `сброс через ${Math.floor(total / 60)} ч ${total % 60} мин`;
    }
    return lang === "en" ? `resets in ${compactDuration(diff)}` : `${compactDuration(diff)}后重置`;
  }
  if (diff < 7 * 24 * 3_600_000) {
    const weekday = at.toLocaleDateString(locale, { weekday: "short" });
    return lang === "ru" ? `сброс ${weekday}` : lang === "en" ? `resets ${weekday}` : `${weekday}重置`;
  }
  const date = at.toLocaleDateString(locale, { month: "short", day: "numeric" });
  return lang === "ru" ? `сброс ${date}` : lang === "en" ? `resets ${date}` : `${date}重置`;
}

/** One window's render line, preserving provider scope and only reported duration. */
export function quotaWindowLine(w: QuotaWindow, now: Date, lang: Lang): string {
  const scope = w.scope?.trim();
  const duration = w.kind === "unknown" ? durationLabel(w.duration_seconds, lang) : null;
  const label = [scope, duration ?? windowLabel(w.kind, lang)].filter(Boolean).join(" · ");
  const head = `${label} ${quotaBar(w.used_percent)} ${usageLabel(w.used_percent, lang)}`;
  const hint = resetHint(w.resets_at, now, lang);
  return hint ? `${head} · ${hint}` : head;
}

/** The lines a vendor row renders for its quota. Missing data stays missing;
 * unavailable data must not masquerade as no subscription. */
export function quotaLines(quota: VendorQuota | null | undefined, now: Date, lang: Lang): string[] {
  if (!quota) return [];
  if (quota.state === "not_subscription") {
    return [lang === "ru" ? "нет подписки" : lang === "zh" ? "无订阅" : "no subscription", observationLine(quota, now, lang)];
  }
  if (quota.state === "unavailable") {
    const reason = quota.reason?.trim();
    const unavailable = lang === "ru"
      ? `квота недоступна${reason ? `: ${reason}` : ""}`
      : lang === "zh"
        ? `配额不可用${reason ? `：${reason}` : ""}`
        : `quota unavailable${reason ? `: ${reason}` : ""}`;
    return [unavailable, observationLine(quota, now, lang)];
  }
  return [...(quota.windows ?? []).map((w) => quotaWindowLine(w, now, lang)), observationLine(quota, now, lang)];
}

/** The plan badge text, only for an available row that carries one. */
export function quotaPlan(quota: VendorQuota | null | undefined): string | null {
  if (!quota || quota.state !== "available") return null;
  const plan = quota.plan?.trim();
  return plan ? plan : null;
}
