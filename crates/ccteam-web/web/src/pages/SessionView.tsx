// v0.8.24 Track A — the Conversation view (prototype `#view-conv`), keyed by
// sid from the shell (`<SessionView key={sid} …/>` — a fresh instance mounts
// on every switch, so all per-sid state resets atomically; the v0.8.9
// structural guarantee is unchanged).
//
// Skin = prototype: conv-head (status dot · title · meta chips · Chat|终端
// tabs · cost pill) + chat-scroll message stream (user right-aligned soft
// bubble 14/14/4/14, agent left with NO fill + full Markdown rendering,
// streaming cursor while a turn is in flight) + the same composer as Home
// (sans ctx-bar).
//
// Data spine unchanged (红线 §1.6-7): localStorage seed → getHistory mirror →
// live SSE fold; drafts persist per-sid; IME composition guard; Stop
// interrupts the running turn (session kept); HITL approvals resolve through
// the gateway pending machinery (never a fake turn). The terminal tab renders
// ONLY for a claude session that owns a pane (protocol ≠ stream-json) — the
// byte-exact `ccteam-pty.v1` relay via TerminalView.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, Clock, Pencil, X } from "lucide-react";
import { ChatComposer } from "../components/ChatComposer";
import { InlineRename } from "../components/InlineRename";
import type { TurnAttachment } from "../lib/attachmentsApi";
import CostPill from "../components/CostPill";
import { Markdown } from "../components/Markdown";
import { TerminalView } from "../components/TerminalView";
import { VendorChip } from "../components/VendorChip";
import { foldSessionLiveness, useSessionEvents } from "../hooks/useSessionEvents";
import { makeT, tr, WEB_LOCALE, type Lang } from "../lib/i18n";
import { defaultDraft, normalizeDraft, vendorSpec, type ComposerDraft } from "../lib/vendors";
import {
  getHistory,
  getDaemonTimezone,
  getSessionStatus,
  listScheduled,
  createScheduled,
  cancelScheduled,
  createSession,
  interruptSession as apiInterruptSession,
  projectUploadUrl,
  putTurnVerdict,
  resolveApproval as apiResolveApproval,
  submitTurn,
  type OutboundAttachmentRef,
  type SessionHistoryEvent,
  type SessionView as SessionSummary,
  type ScheduledItem,
  type TurnVerdict,
  type TurnVerdictRecord,
} from "../lib/sessionsApi";
import {
  appendEvent,
  appendRow,
  historyToRows,
  loadRows,
  mergeAuthoritativeTurnMetadata,
  nextRowId,
  saveRows,
  type TranscriptRow,
} from "./chatTranscript";
import { railSessionLabel } from "./railHelpers";
import TurnVerdictControls from "./TurnVerdictControls";

function formatAttachmentSize(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

function overlayHistoryEvents(
  windowEvents: SessionHistoryEvent[],
  metadataEvents: SessionHistoryEvent[],
): SessionHistoryEvent[] {
  if (metadataEvents.length === 0) return windowEvents;
  const merged = [...windowEvents];
  const indexByTurnId = new Map(merged.map((event, index) => [event.turn_id, index]));
  for (const event of metadataEvents) {
    const index = indexByTurnId.get(event.turn_id);
    if (index === undefined) {
      indexByTurnId.set(event.turn_id, merged.length);
      merged.push(event);
    } else {
      merged[index] = event;
    }
  }
  return merged;
}

function reseedTranscriptRows(
  windowEvents: SessionHistoryEvent[],
  metadataEvents: SessionHistoryEvent[],
  currentRows: TranscriptRow[],
  rowsAtRequest: ReadonlyMap<string, TranscriptRow>,
): { rows: TranscriptRow[]; shouldApplyOnMount: boolean } {
  const authoritativeEvents = overlayHistoryEvents(windowEvents, metadataEvents);
  const seeded = historyToRows(authoritativeEvents);
  const changedSinceRequest = currentRows.filter(
    (row) => rowsAtRequest.get(row.id) !== row,
  );
  const annotated = mergeAuthoritativeTurnMetadata(
    changedSinceRequest,
    authoritativeEvents,
  );
  const seededTurnIds = new Set(
    seeded.flatMap((row) => (row.turnId ? [row.turnId] : [])),
  );
  const preserved = annotated.filter(
    (row) => !row.turnId || !seededTurnIds.has(row.turnId),
  );
  return {
    rows: [...seeded, ...preserved],
    // An empty mount history intentionally keeps the localStorage fallback,
    // unless something live arrived while that initial request was in flight.
    shouldApplyOnMount: seeded.length > 0 || changedSinceRequest.length > 0,
  };
}

/** A row's server-side timestamp (WEB-TS-1, `row.ts` — RFC 3339) rendered as
 *  local `HH:MM` for today, `MM-DD HH:MM` within the current year, and
 *  `YYYY-MM-DD HH:MM` for older rows; the full date-time rides the tooltip.
 *  Sits on its own line under the bubble, so it never squeezes the bubble on
 *  narrow viewports. Absent/unparseable `ts` (old daemons, rows persisted
 *  before this field) renders nothing. */
function rowTimeParts(ts: string, now: Date): { date: boolean; year: boolean } | null {
  const when = new Date(ts);
  if (Number.isNaN(when.getTime())) return null;
  return {
    date: when.toDateString() !== now.toDateString(),
    year: when.getFullYear() !== now.getFullYear(),
  };
}

export function RowTime({ ts, lang }: { ts?: string; lang: Lang }) {
  if (!ts) return null;
  const when = new Date(ts);
  const parts = rowTimeParts(ts, new Date());
  if (!parts) return null;
  const locale = WEB_LOCALE[lang];
  const text = when.toLocaleString(locale, {
    ...(parts.date
      ? parts.year
        ? { year: "numeric" as const, month: "2-digit" as const, day: "2-digit" as const }
        : { month: "2-digit" as const, day: "2-digit" as const }
      : {}),
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  return (
    <time className="msg-time" dateTime={ts} title={when.toLocaleString(locale)}>
      {text}
    </time>
  );
}

function OutboundAttachments({
  project,
  attachments,
}: {
  project?: string;
  attachments?: OutboundAttachmentRef[];
}) {
  if (!project || !attachments || attachments.length === 0) return null;
  return (
    <div className="outbound-attachments" data-testid="outbound-attachments">
      {attachments.map((attachment) => {
        const url = projectUploadUrl(project, attachment.id);
        return attachment.kind === "image" ? (
          <a
            key={attachment.id}
            className="outbound-attachment image"
            href={url}
            aria-label={attachment.name}
          >
            <img loading="lazy" src={url} alt={attachment.name} />
            <span>{attachment.name}</span>
          </a>
        ) : (
          <a
            key={attachment.id}
            className="outbound-attachment file"
            href={url}
            download={attachment.name}
          >
            <span>📎 {attachment.name}</span>
            <small>{formatAttachmentSize(attachment.size)}</small>
          </a>
        );
      })}
    </div>
  );
}

export default function SessionView({
  sid,
  session,
  lang = "zh",
  onRename,
  onOpenSession,
}: {
  sid: string;
  session: SessionSummary | null;
  lang?: Lang;
  /** Rename this session's title (the shell owns the request + toast, so the
   *  rail and this header stay in lockstep). Omitted ⇒ the title is plain
   *  text, no edit affordance. */
  onRename?: (sid: string, title: string) => void | Promise<void>;
  /** Open a newly-created related session in the shell. */
  onOpenSession?: (sid: string) => void;
}) {
  const t = makeT(lang);
  const [view, setView] = useState<"chat" | "terminal">("chat");
  // Inline title edit in the header. `pendingTitle` is the optimistic value
  // shown only WHILE the shell's rename is in flight — without it the header
  // visibly snaps back to the old title for a beat after Enter. It is cleared
  // the moment the request settles (the shell refreshes the session list
  // before resolving), so a server-side truncation — or a failure — always
  // wins over what the user typed.
  const [editingTitle, setEditingTitle] = useState(false);
  const [pendingTitle, setPendingTitle] = useState<string | null>(null);
  const [busyMark, setBusyMark] = useState<number | null>(null);
  const [rows, setRows] = useState<TranscriptRow[]>(() => loadRows(sid));
  const rowsRef = useRef(rows);
  const verdictPendingRef = useRef(new Set<string>());
  const verdictMutationVersionRef = useRef(new Map<string, number>());
  const [verdictBusy, setVerdictBusy] = useState<Record<string, boolean>>({});

  const { events, lastError, gatewayUnavailable, connectionEpoch } =
    useSessionEvents(sid);

  const preserveNewerVerdicts = useCallback(
    (
      candidate: TranscriptRow[],
      current: TranscriptRow[],
      requestedVersions: ReadonlyMap<string, number>,
    ): TranscriptRow[] => {
      const currentByTurnId = new Map(
        current
          .filter(
            (row): row is TranscriptRow & { turnId: string } =>
              row.kind === "assistant" && typeof row.turnId === "string",
          )
          .map((row) => [row.turnId, row]),
      );
      return candidate.map((row) => {
        if (row.kind !== "assistant" || !row.turnId) return row;
        const currentRow = currentByTurnId.get(row.turnId);
        if (!currentRow) return row;
        const requestedVersion = requestedVersions.get(row.turnId) ?? 0;
        const currentVersion = verdictMutationVersionRef.current.get(row.turnId) ?? 0;
        if (
          !verdictPendingRef.current.has(row.turnId) &&
          currentVersion <= requestedVersion
        ) {
          return row;
        }
        const protectedRow = { ...row };
        if (currentRow.verdict) protectedRow.verdict = currentRow.verdict;
        else delete protectedRow.verdict;
        return protectedRow;
      });
    },
    [],
  );

  // The SSE buffer survives reconnects. Keep both the fold cursor and a live
  // view of its current length so an authoritative history reseed can install
  // a precise barrier: everything already buffered is represented by history;
  // only frames arriving after the reseed append from then on.
  const foldedRef = useRef(0);
  const eventsRef = useRef(events);
  // Full/metadata refreshes and backwards pagination are independent request
  // lanes. A live answer may refresh canonical turn ids while an older page is
  // in flight; it must not invalidate that page or strand its loading flag.
  const historyWindowRequestRef = useRef(0);
  const historyMetadataRequestRef = useRef(0);
  const historyPaginationRequestRef = useRef(0);
  const historyMetadataEventsRef = useRef<SessionHistoryEvent[]>([]);
  const historyWindowKey = `${sid}:${Math.max(connectionEpoch, 1)}`;
  const historyWindowKeyRef = useRef(historyWindowKey);
  useLayoutEffect(() => {
    historyWindowKeyRef.current = historyWindowKey;
  }, [historyWindowKey]);
  useLayoutEffect(() => {
    rowsRef.current = rows;
  }, [rows]);
  const [historyPage, setHistoryPage] = useState({
    windowKey: "",
    hasMore: false,
    nextBefore: null as string | null,
    loadingEarlier: false,
  });
  const historyWindowReady = historyPage.windowKey === historyWindowKey;
  useEffect(() => {
    eventsRef.current = events;
  }, [events]);

  // ---- authoritative seed: mirrored history (mount-scoped) -----------------
  useEffect(() => {
    let cancelled = false;
    const mountWindowKey = `${sid}:1`;
    // ChatConsole keys this component by sid, but keep the component safe for
    // direct prop reuse too: an old sid's backwards page must never mutate the
    // new sid's rows or cursor after its deferred request settles.
    const paginationRequest = ++historyPaginationRequestRef.current;
    ++historyMetadataRequestRef.current;
    historyMetadataEventsRef.current = [];
    const rowsAtRequest = new Map(rowsRef.current.map((row) => [row.id, row]));
    queueMicrotask(() => {
      if (cancelled || paginationRequest !== historyPaginationRequestRef.current) return;
      setHistoryPage({
        windowKey: mountWindowKey,
        hasMore: false,
        nextBefore: null,
        loadingEarlier: false,
      });
    });
    const request = ++historyWindowRequestRef.current;
    const verdictVersions = new Map(verdictMutationVersionRef.current);
    getHistory(sid)
      .then((h) => {
        if (
          cancelled
          || request !== historyWindowRequestRef.current
          || mountWindowKey !== historyWindowKeyRef.current
        ) return;
        setRows((current) => {
          const seeded = reseedTranscriptRows(
            h.events,
            historyMetadataEventsRef.current,
            current,
            rowsAtRequest,
          );
          return seeded.shouldApplyOnMount
            ? preserveNewerVerdicts(seeded.rows, current, verdictVersions)
            : current;
        });
        setHistoryPage({
          windowKey: mountWindowKey,
          hasMore: h.has_more === true,
          nextBefore: h.next_before ?? null,
          loadingEarlier: false,
        });
      })
      .catch(() => {
        /* best-effort — keep the localStorage rows (or empty) on error */
      });
    return () => {
      cancelled = true;
    };
  }, [sid, preserveNewerVerdicts]);

  // The first successful open races the mount seed above, so epoch 1 needs no
  // second fetch. Every later open follows a real disconnect: the server's
  // turns.jsonl mirror is authoritative even when the SSE replay ring no
  // longer contains every missed answer.
  useEffect(() => {
    if (connectionEpoch <= 1) return;
    let cancelled = false;
    // A reconnect replaces the whole window and its cursor. Invalidate any
    // older page and pre-reconnect metadata overlay immediately. The metadata
    // lane remains independent: a fresh live answer after this point can
    // start a new overlay without cancelling the full-window request.
    const paginationRequest = ++historyPaginationRequestRef.current;
    ++historyMetadataRequestRef.current;
    historyMetadataEventsRef.current = [];
    const rowsAtRequest = new Map(rowsRef.current.map((row) => [row.id, row]));
    queueMicrotask(() => {
      if (
        cancelled
        || paginationRequest !== historyPaginationRequestRef.current
        || historyWindowKey !== historyWindowKeyRef.current
      ) return;
      setHistoryPage({
        windowKey: historyWindowKey,
        hasMore: false,
        nextBefore: null,
        loadingEarlier: false,
      });
    });
    const request = ++historyWindowRequestRef.current;
    const verdictVersions = new Map(verdictMutationVersionRef.current);
    getHistory(sid)
      .then((h) => {
        if (
          cancelled
          || request !== historyWindowRequestRef.current
          || historyWindowKey !== historyWindowKeyRef.current
        ) return;
        const foldBarrier = eventsRef.current.length;
        setRows((current) => {
          const seeded = reseedTranscriptRows(
            h.events,
            historyMetadataEventsRef.current,
            current,
            rowsAtRequest,
          );
          return preserveNewerVerdicts(seeded.rows, current, verdictVersions);
        });
        foldedRef.current = foldBarrier;
        setHistoryPage({
          windowKey: historyWindowKey,
          hasMore: h.has_more === true,
          nextBefore: h.next_before ?? null,
          loadingEarlier: false,
        });
      })
      .catch(() => {
        /* best-effort — keep the current transcript on reseed failure */
      });
    return () => {
      cancelled = true;
    };
  }, [sid, connectionEpoch, historyWindowKey, preserveNewerVerdicts]);

  const loadEarlier = useCallback(() => {
    if (
      !historyWindowReady
      || historyWindowKey !== historyWindowKeyRef.current
      || !historyPage.hasMore
      || !historyPage.nextBefore
      || historyPage.loadingEarlier
    ) return;
    const before = historyPage.nextBefore;
    const request = ++historyPaginationRequestRef.current;
    const requestWindowKey = historyWindowKey;
    const verdictVersions = new Map(verdictMutationVersionRef.current);
    setHistoryPage((current) => ({ ...current, loadingEarlier: true }));
    getHistory(sid, { before })
      .then((history) => {
        if (
          request !== historyPaginationRequestRef.current
          || requestWindowKey !== historyWindowKeyRef.current
        ) return;
        const earlier = historyToRows(history.events);
        if (earlier.length > 0) {
          setRows((current) => [
            ...preserveNewerVerdicts(earlier, current, verdictVersions),
            ...current,
          ]);
        }
        setHistoryPage({
          windowKey: requestWindowKey,
          hasMore: history.has_more === true,
          nextBefore: history.next_before ?? null,
          loadingEarlier: false,
        });
      })
      .catch(() => {
        /* The cursor remains retryable; finally releases the affordance. */
      })
      .finally(() => {
        if (
          request !== historyPaginationRequestRef.current
          || requestWindowKey !== historyWindowKeyRef.current
        ) return;
        setHistoryPage((current) =>
          current.loadingEarlier ? { ...current, loadingEarlier: false } : current,
        );
      });
  }, [sid, historyPage, historyWindowKey, historyWindowReady, preserveNewerVerdicts]);

  // ---- live SSE → append into this sid's transcript ------------------------
  useEffect(() => {
    if (events.length <= foldedRef.current) return;
    const fresh = events.slice(foldedRef.current);
    foldedRef.current = events.length;
    setRows((current) => {
      let next = current;
      for (const ev of fresh) {
        next = appendEvent(next, ev);
      }
      return next;
    });
  }, [events]);

  // ---- persist this sid's transcript ---------------------------------------
  useEffect(() => {
    saveRows(sid, rows);
  }, [sid, rows]);

  // ---- per-session status (model + effort + ctx%) --------------------------
  const [statusModel, setStatusModel] = useState<string | null>(null);
  const [statusEffort, setStatusEffort] = useState<string | null>(null);
  const [ctxPct, setCtxPct] = useState<number | null>(null);
  const doneCount = events.reduce((n, ev) => (ev.done ? n + 1 : n), 0);
  const busy = busyMark !== null && doneCount === busyMark;
  const latestAnswer = (() => {
    for (let index = events.length - 1; index >= 0; index -= 1) {
      if (events[index]?.kind === "answer") return events[index];
    }
    return undefined;
  })();
  const refreshedAnswerRef = useRef(latestAnswer);
  useEffect(() => {
    if (!latestAnswer || latestAnswer === refreshedAnswerRef.current) {
      refreshedAnswerRef.current = latestAnswer;
      return;
    }
    refreshedAnswerRef.current = latestAnswer;
    let cancelled = false;
    const request = ++historyMetadataRequestRef.current;
    const verdictVersions = new Map(verdictMutationVersionRef.current);
    getHistory(sid)
      .then((history) => {
        if (cancelled || request !== historyMetadataRequestRef.current) return;
        historyMetadataEventsRef.current = history.events;
        setRows((current) =>
          preserveNewerVerdicts(
            mergeAuthoritativeTurnMetadata(current, history.events),
            current,
            verdictVersions,
          ),
        );
      })
      .catch(() => {
        /* best-effort — the next reconnect/history open can supply turn ids */
      });
    return () => {
      cancelled = true;
    };
  }, [sid, latestAnswer, preserveNewerVerdicts]);
  useEffect(() => {
    let cancelled = false;
    getSessionStatus(sid)
      .then((s) => {
        if (cancelled) return;
        setStatusModel(s.model);
        setStatusEffort(s.effort ?? null);
        setCtxPct(s.context ? s.context.pct : null);
      })
      .catch(() => {
        if (!cancelled) {
          setStatusModel(null);
          setStatusEffort(null);
          setCtxPct(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [sid, doneCount]);

  const pushRow = useCallback((row: Omit<TranscriptRow, "id">) => {
    setRows((current) => appendRow(current, { ...row, id: nextRowId(row.kind) }));
  }, []);

  // ---- delayed user-message queue ------------------------------------------
  const [scheduled, setScheduled] = useState<ScheduledItem[]>([]);
  const [daemonTimezone, setDaemonTimezone] = useState("local");
  const scheduledRevision = events.reduce(
    (count, event) => count + (event.kind === "scheduled_changed" ? 1 : 0),
    0,
  );
  const reloadScheduled = useCallback(() => {
    listScheduled(sid)
      .then(setScheduled)
      .catch(() => {
        /* best-effort: retain the last authoritative queue */
      });
  }, [sid]);
  useEffect(() => {
    reloadScheduled();
  }, [reloadScheduled, scheduledRevision]);
  useEffect(() => {
    getDaemonTimezone().then(setDaemonTimezone).catch(() => setDaemonTimezone("local"));
  }, []);

  const scheduleText = useCallback(
    (content: string, when: string) => {
      createScheduled(sid, content, when)
        .then(() => reloadScheduled())
        .catch((error) => {
          pushRow({
            kind: "system",
            content: `${t("scheduleFailed")}: ${error instanceof Error ? error.message : "unknown"}`,
          });
        });
    },
    [sid, reloadScheduled, pushRow, t],
  );

  const removeScheduled = useCallback(
    (id: string) => {
      cancelScheduled(sid, id)
        .then(() => reloadScheduled())
        .catch((error) => {
          pushRow({
            kind: "system",
            content: `${t("scheduleCancelFailed")}: ${error instanceof Error ? error.message : "unknown"}`,
          });
        });
    },
    [sid, reloadScheduled, pushRow, t],
  );

  // ---- send a turn ----------------------------------------------------------
  const submitText = useCallback(
    (content: string, attachments: TurnAttachment[] = []) => {
      // Optimistic transcript row: show the text plus a compact attachment
      // note (the server-side turn text carries the full attachment lines).
      const names = attachments
        .map((a) => (a.kind === "skill" ? `skill:${a.name}` : (a.name ?? a.path ?? "")))
        .filter(Boolean);
      const shown = names.length > 0 ? `${content}\n📎 ${names.join(", ")}` : content;
      pushRow({ kind: "user", content: shown });
      setBusyMark(doneCount);
      submitTurn(sid, content, attachments).catch((e) => {
        setBusyMark(null);
        const detail = e instanceof Error ? e.message : "unknown";
        pushRow({
          kind: "system",
          content: detail.startsWith("发送失败") ? detail : `发送失败: ${detail}`,
        });
      });
    },
    [sid, pushRow, doneCount],
  );

  const setRowVerdict = useCallback(
    (turnId: string, verdict: TurnVerdictRecord | undefined) => {
      setRows((current) =>
        current.map((row) => {
          if (row.kind !== "assistant" || row.turnId !== turnId) return row;
          const next = { ...row };
          if (verdict) next.verdict = verdict;
          else delete next.verdict;
          return next;
        }),
      );
    },
    [],
  );

  const saveTurnVerdict = useCallback(
    (row: TranscriptRow, verdict: TurnVerdict, feedback?: string) => {
      const turnId = row.turnId;
      if (!turnId || verdictPendingRef.current.has(turnId)) return;
      verdictMutationVersionRef.current.set(
        turnId,
        (verdictMutationVersionRef.current.get(turnId) ?? 0) + 1,
      );
      verdictPendingRef.current.add(turnId);
      setVerdictBusy((current) => ({ ...current, [turnId]: true }));

      const previous = row.verdict;
      const optimistic: TurnVerdictRecord = {
        verdict,
        feedback: verdict === "revise" ? (feedback ?? null) : null,
        ts: new Date().toISOString(),
      };
      setRowVerdict(turnId, optimistic);

      putTurnVerdict(sid, turnId, {
        verdict,
        ...(verdict === "revise" ? { feedback } : {}),
      })
        .then((response) => {
          verdictMutationVersionRef.current.set(
            turnId,
            (verdictMutationVersionRef.current.get(turnId) ?? 0) + 1,
          );
          setRowVerdict(turnId, {
            verdict: response.verdict,
            feedback: response.feedback,
            ts: optimistic.ts,
          });
        })
        .catch((error) => {
          verdictMutationVersionRef.current.set(
            turnId,
            (verdictMutationVersionRef.current.get(turnId) ?? 0) + 1,
          );
          setRowVerdict(turnId, previous);
          const detail = error instanceof Error ? error.message : "unknown";
          pushRow({
            kind: "system",
            content: tr(
              lang,
              `保存 turn ${turnId} 的评价失败:${detail}`,
              `Failed to save feedback for turn ${turnId}: ${detail}`,
              `Не удалось сохранить оценку хода ${turnId}: ${detail}`,
            ),
          });
        })
        .finally(() => {
          verdictPendingRef.current.delete(turnId);
          setVerdictBusy((current) => {
            const next = { ...current };
            delete next[turnId];
            return next;
          });
        });
    },
    [lang, pushRow, setRowVerdict, sid],
  );

  const requestImprovement = useCallback(
    (row: TranscriptRow, feedback: string) => {
      if (!row.turnId || !session) return;
      const protocol = ["stream-json", "terminal", "acp"].includes(session.protocol ?? "")
        ? (session.protocol as "stream-json" | "terminal" | "acp")
        : undefined;
      void createSession(session.project, {
        role: session.role,
        vendor: session.vendor,
        permission_mode: "hitl",
        ...(protocol ? { protocol } : {}),
      })
        .then(async ({ sid: proposalSid }) => {
          const prompt = [
            `Human feedback for completed turn ${row.turnId}:`,
            feedback,
            "",
            "This is a proposal-only turn. Analyze the feedback and propose concrete role, skill, or instruction changes that would prevent this issue.",
            "Do not claim that any change was applied. This dedicated session uses HITL: ccteam auto-approves nothing, and any write/apply tool request must wait for explicit human approval.",
          ].join("\n");
          await submitTurn(proposalSid, prompt);
          pushRow({
            kind: "system",
            content: tr(
              lang,
              `已创建 HITL 提案会话 ${proposalSid}。ccteam 不会自动批准;任何写入/应用操作都必须等待你的明确批准。正在打开该会话。`,
              `Created HITL proposal session ${proposalSid}. ccteam auto-approves nothing; any write/apply action must wait for your explicit approval. Opening it now.`,
              `Создана HITL-сессия предложений ${proposalSid}. ccteam ничего не одобряет автоматически; любая запись или применение ждёт вашего явного подтверждения. Открываю её.`,
            ),
          });
          onOpenSession?.(proposalSid);
        })
        .catch((error) => {
          const detail = error instanceof Error ? error.message : "unknown";
          pushRow({
            kind: "system",
            content: tr(
              lang,
              `启动 HITL 提案会话失败:${detail}`,
              `Failed to start the HITL proposal session: ${detail}`,
              `Не удалось запустить HITL-сессию предложений: ${detail}`,
            ),
          });
        });
    },
    [lang, onOpenSession, pushRow, session],
  );

  // ---- resolve a HITL approval prompt (gateway pending machinery) ----------
  const resolveApproval = useCallback(
    (row: TranscriptRow, optionIndex: number) => {
      const option = row.options?.[optionIndex];
      if (!row.token || !option?.id) {
        pushRow({
          kind: "system",
          content: "无法批准: 该提示缺少 token/选项 id(请在 IM 批准,或重开会话)",
        });
        return;
      }
      setRows((current) => current.map((r) => (r.id === row.id ? { ...r, resolved: true } : r)));
      pushRow({ kind: "user", content: `→ ${option.label}` });
      apiResolveApproval(sid, row.token, option.id).catch((e) => {
        setRows((current) =>
          current.map((r) => (r.id === row.id ? { ...r, resolved: false } : r)),
        );
        pushRow({
          kind: "system",
          content: `批准提交失败: ${e instanceof Error ? e.message : "unknown"}`,
        });
      });
    },
    [sid, pushRow],
  );

  // ---- interrupt the running turn (session kept) ----------------------------
  const interruptActive = useCallback(() => {
    apiInterruptSession(sid)
      .then(() => {
        pushRow({ kind: "system", content: "已中断当前 turn(会话保留)" });
      })
      .catch((e) => {
        pushRow({
          kind: "system",
          content: `中断失败: ${e instanceof Error ? e.message : "unknown"}`,
        });
      });
  }, [sid, pushRow]);

  // Terminal tab: only a claude session that owns a pane (protocol ≠
  // stream-json). A stream-json session has NO pane → the tab does not exist.
  const isStreamJson = session?.protocol === "stream-json";
  const canTerminal = session?.vendor === "claude" && !isStreamJson;
  const showTerminal = view === "terminal" && canTerminal;

  // Auto-scroll only when already near the bottom; 「回到最新」 appears when
  // the reader scrolled up (a streaming reply never yanks them down).
  const scrollRef = useRef<HTMLDivElement>(null);
  const atBottomRef = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const onTranscriptScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    atBottomRef.current = atBottom;
    setShowJump(!atBottom);
  }, []);
  const jumpToBottom = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    atBottomRef.current = true;
    setShowJump(false);
  }, []);
  useLayoutEffect(() => {
    if (!showTerminal && atBottomRef.current && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [rows, showTerminal]);

  // conv-head status dot: busy amber › SESSION state. The base is the rail's
  // REST `session.status`; this sid's live `session_lifecycle` frames fold on
  // top, so a capacity eviction greys the dot immediately, without waiting
  // for the rail's next REST reconcile. The SSE CONNECTION state is a
  // different fact and no longer drives this dot (an open stream on a dead
  // session is what made it "always green").
  const sessionLive = foldSessionLiveness(session?.status === "live", events);
  const headDot = busy ? "dot busy" : sessionLive ? "dot on" : "dot off";
  // A broken stream (retries exhausted / no gateway) stays visible as its own
  // red connection dot next to the status dot.
  const connLost = gatewayUnavailable || lastError !== null;

  const serverTitle = session ? railSessionLabel(session) : sid;
  const title = pendingTitle ?? serverTitle;
  const vendor = session?.vendor ?? "claude";
  const who = `${vendor} · ${sid}${statusModel ? ` · ${statusModel}` : ""}`;

  // The conversation composer reflects this session's FIXED spawn parameters
  // (locked: picking toasts; /model via the input still works). What the live
  // session REPORTS rides the display props (`modelLabel` / `effortLabel`)
  // rather than the draft, because the draft is menu state — normalizeDraft
  // holds it to rows the menu offers, while the pill must print the session's
  // own tokens verbatim (unreported ⇒ 默认 / `default`, never a made-up 中).
  const lockedDraft: ComposerDraft = useMemo(
    () =>
      normalizeDraft({
        ...defaultDraft(),
        vendor: vendorSpec(vendor).id,
        model: statusModel ?? "",
        hitl: session?.permission_mode === "hitl",
        effort: statusEffort ?? "",
      }),
    [vendor, statusModel, statusEffort, session?.permission_mode],
  );

  return (
    <section className="view active" data-testid="conversation-view">
      <div className="conv-head">
        <span className={headDot} data-testid="conv-dot" />
        {connLost ? (
          <span className="dot err" data-testid="conn-dot" title={t("connLost")} />
        ) : null}
        {editingTitle && onRename ? (
          <InlineRename
            className="title rename-input"
            initial={title}
            ariaLabel={`${t("renameTip")} ${sid}`}
            onSubmit={(next) => {
              setEditingTitle(false);
              setPendingTitle(next);
              void Promise.resolve(onRename(sid, next)).finally(() => setPendingTitle(null));
            }}
            onCancel={() => setEditingTitle(false)}
          />
        ) : (
          <span
            className="title"
            data-testid="conv-title"
            title={onRename ? `${t("renameTip")} · ${t("renameHint")}` : undefined}
            onDoubleClick={() => {
              if (onRename) setEditingTitle(true);
            }}
          >
            {title}
          </span>
        )}
        {onRename && !editingTitle ? (
          <button
            type="button"
            className="icon-btn sm rename-title"
            data-testid="conv-rename"
            title={t("renameTip")}
            aria-label={`${t("renameTip")} ${sid}`}
            onClick={() => setEditingTitle(true)}
          >
            <Pencil />
          </button>
        ) : null}
        <div className="meta">
          <span className="chip sid">{sid}</span>
          {session ? <span className="chip">{session.project}</span> : null}
          {session ? (
            <span className="chip" data-testid="session-role">
              {session.role || t("noRole")}
            </span>
          ) : null}
          <VendorChip vendor={vendor} />
          {session?.host && session.host !== "local" ? (
            <span className="chip">@ {session.host}</span>
          ) : null}
          {statusModel ? (
            <span className="chip" title="model · context window">
              {statusModel}
              {ctxPct !== null ? ` · ctx ${Math.round(ctxPct)}%` : ""}
            </span>
          ) : null}
        </div>
        <div className="tabs">
          <button
            type="button"
            className={`tab ${!showTerminal ? "active" : ""}`}
            onClick={() => setView("chat")}
          >
            {t("chatTab")}
          </button>
          {canTerminal ? (
            <button
              type="button"
              className={`tab ${showTerminal ? "active" : ""}`}
              onClick={() => setView("terminal")}
              data-testid="terminal-tab"
            >
              {t("terminal")}
            </button>
          ) : null}
        </div>
        <CostPill />
      </div>

      {showTerminal && session?.project ? (
        <div className="term-wrap">
          <TerminalView lang={lang} slug={session.project} sid={sid} className="flex-1 min-h-0" />
        </div>
      ) : (
        <>
          <div style={{ position: "relative", flex: 1, minHeight: 0, display: "flex" }}>
            <div
              ref={scrollRef}
              onScroll={onTranscriptScroll}
              className="chat-scroll"
              data-testid="chat-scroll"
            >
              <div className="chat-inner">
                {historyWindowReady && historyPage.hasMore ? (
                  <button
                    type="button"
                    className="btn ghost mini self-center"
                    data-testid="load-earlier"
                    disabled={historyPage.loadingEarlier}
                    onClick={loadEarlier}
                  >
                    {historyPage.loadingEarlier ? t("loading") : tr(lang, "加载更早消息", "Load earlier", "Загрузить ранее")}
                  </button>
                ) : null}
                {rows.map((row) => {
                  if (row.kind === "approval") {
                    return (
                      <div key={row.id} className="msg approval fade-in">
                        <span className="who">{t("approve")}</span>
                        <div className="bubble">
                          {row.content}
                          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
                            {(row.options ?? []).map((opt, i) => (
                              <button
                                key={`${row.id}-${i}`}
                                type="button"
                                className="btn primary mini"
                                disabled={row.resolved}
                                style={row.resolved ? { opacity: 0.45 } : undefined}
                                onClick={() => resolveApproval(row, i)}
                              >
                                {opt.label}
                              </button>
                            ))}
                          </div>
                          {row.resolved ? (
                            <div style={{ marginTop: 6, fontSize: 11, color: "var(--text-faint)" }}>
                              已回应
                            </div>
                          ) : null}
                        </div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "system") {
                    return (
                      <div key={row.id} className="msg system fade-in">
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "error") {
                    return (
                      <div key={row.id} className="msg error fade-in">
                        <span className="who">
                          {tr(lang, "错误", "Error", "Ошибка")}
                          {row.errorKind ? ` · ${row.errorKind}` : ""}
                        </span>
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "activity") {
                    return (
                      <div key={row.id} className="msg activity">
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "user") {
                    return (
                      <div key={row.id} className="msg user fade-in">
                        <span className="who">you</span>
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "tool") {
                    return (
                      <div key={row.id} className="msg tool fade-in">
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  // assistant — full Markdown document (红线: never plain text).
                  return (
                    <div key={row.id} className="msg agent fade-in">
                      <span className="who">{who}</span>
                      <div className="bubble md">
                        {row.content ? <Markdown content={row.content} /> : null}
                        <OutboundAttachments
                          project={session?.project}
                          attachments={row.attachments}
                        />
                        {row.outcome === "completed" ? (
                          <TurnVerdictControls
                            lang={lang}
                            row={row}
                            busy={row.turnId ? verdictBusy[row.turnId] === true : false}
                            onVerdict={(verdict, feedback) =>
                              saveTurnVerdict(row, verdict, feedback)
                            }
                            onImprove={(feedback) => requestImprovement(row, feedback)}
                          />
                        ) : null}
                      </div>
                      <RowTime ts={row.ts} lang={lang} />
                    </div>
                  );
                })}
                {busy ? (
                  <div className="msg agent" aria-label={tr(lang, "生成中", "Generating", "Генерация")} data-testid="streaming-cursor">
                    <div className="bubble">
                      <span className="cursor" />
                    </div>
                  </div>
                ) : null}
              </div>
            </div>
            {showJump ? (
              <button type="button" className="jump-latest" onClick={jumpToBottom}>
                <ArrowDown /> {tr(lang, "回到最新", "Back to latest", "К последним")}
              </button>
            ) : null}
          </div>

          <div className="conv-composer-wrap">
            <div className="composer-group">
              {scheduled.length > 0 ? (
                <div className="scheduled-queue" data-testid="scheduled-queue">
                  <div className="scheduled-head">
                    <Clock />
                    <span>{t("scheduleQueue")}</span>
                  </div>
                  {scheduled.map((item) => (
                    <div key={item.id} className={`scheduled-row ${item.status}`}>
                      <time
                        dateTime={item.send_at}
                        title={`${t("scheduleLocalTime")}: ${item.send_at}`}
                      >
                        {new Date(item.send_at).toLocaleString(WEB_LOCALE[lang], {
                          month: "2-digit",
                          day: "2-digit",
                          hour: "2-digit",
                          minute: "2-digit",
                        })}
                      </time>
                      <span className="scheduled-preview" title={item.text}>
                        {item.text.replace(/\s+/g, " ").trim().slice(0, 80)}
                      </span>
                      {item.status === "failed" ? (
                        <span className="scheduled-error" title={item.fail_reason ?? ""}>
                          {t("scheduleFailed")}
                        </span>
                      ) : item.status === "dispatching" ? (
                        <span className="scheduled-error" title={item.fail_reason ?? ""}>
                          {t("scheduleUnknown")}
                        </span>
                      ) : null}
                      <button
                        type="button"
                        aria-label={`${t("scheduleCancel")} ${item.id}`}
                        title={`${t("scheduleCancel")} ${item.id}`}
                        onClick={() => removeScheduled(item.id)}
                      >
                        <X />
                      </button>
                    </div>
                  ))}
                </div>
              ) : null}
              <ChatComposer
                draftKey={sid}
                lang={lang}
                placeholderKey="convPh"
                busy={busy}
                onStop={interruptActive}
                onSend={submitText}
                draft={lockedDraft}
                onDraftChange={() => {}}
                locked
                modelLabel={statusModel ?? ""}
                effortLabel={statusEffort ?? ""}
                uploadSlug={session?.project}
                onSchedule={scheduleText}
                scheduleTimezone={daemonTimezone}
              />
            </div>
          </div>
        </>
      )}
    </section>
  );
}
