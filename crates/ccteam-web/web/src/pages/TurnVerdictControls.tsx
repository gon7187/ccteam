import { tr, type Lang } from "../lib/i18n";
import type { TurnVerdict } from "../lib/sessionsApi";
import type { TranscriptRow } from "./chatTranscript";

const FEEDBACK_MAX = 4000;

export default function TurnVerdictControls({
  lang,
  row,
  busy,
  onVerdict,
  onImprove,
}: {
  lang: Lang;
  row: TranscriptRow;
  busy: boolean;
  onVerdict: (verdict: TurnVerdict, feedback?: string) => void;
  onImprove: (feedback: string) => void;
}) {
  if (!row.turnId) return null;
  const current = row.verdict?.verdict;
  const feedback = row.verdict?.feedback?.trim() ?? "";
  const groupLabel = tr(
    lang,
    `已完成 turn ${row.turnId} 的评价`,
    `Feedback for completed turn ${row.turnId}`,
    `Оценка завершённого хода ${row.turnId}`,
  );

  const requestRevision = () => {
    const raw = window.prompt(
      tr(
        lang,
        `需要修改什么?反馈必填,最多 ${FEEDBACK_MAX} 个字符。`,
        `What needs revision? Feedback is required (max ${FEEDBACK_MAX} characters).`,
        `Что нужно доработать? Отзыв обязателен, максимум ${FEEDBACK_MAX} символов.`,
      ),
      feedback,
    );
    if (raw === null) return;
    const next = raw.trim();
    if (!next || Array.from(next).length > FEEDBACK_MAX) {
      window.alert(
        tr(
          lang,
          `反馈必须包含 1–${FEEDBACK_MAX} 个字符。`,
          `Feedback must contain 1–${FEEDBACK_MAX} characters.`,
          `Отзыв должен содержать от 1 до ${FEEDBACK_MAX} символов.`,
        ),
      );
      return;
    }
    onVerdict("revise", next);
  };

  const requestImprovement = () => {
    if (!feedback) return;
    const confirmed = window.confirm(
      tr(
        lang,
        "向同一会话发送普通消息,请 agent 根据这条反馈提出 role / skill / instruction 改进方案?agent 不得在你明确批准前应用任何更改。",
        "Send one ordinary message asking this session to propose role, skill, or instruction improvements from the feedback? The agent must not apply changes without your explicit approval.",
        "Отправить в эту сессию обычное сообщение с просьбой предложить улучшения ролей, навыков или инструкций по отзыву? Агент не должен применять изменения без вашего явного одобрения.",
      ),
    );
    if (confirmed) onImprove(feedback);
  };

  return (
    <div
      role="group"
      aria-label={groupLabel}
      className="turn-verdict-controls"
      style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap", marginTop: 8 }}
    >
      <button
        type="button"
        className="btn ghost mini"
        data-testid={`turn-accept-${row.turnId}`}
        aria-label={tr(lang, `接受 turn ${row.turnId}`, `Accept turn ${row.turnId}`, `Принять ход ${row.turnId}`)}
        aria-pressed={current === "accept"}
        disabled={busy}
        onClick={() => onVerdict("accept")}
      >
        {tr(lang, "接受", "Accept", "Принять")}
      </button>
      <button
        type="button"
        className="btn ghost mini"
        data-testid={`turn-revise-${row.turnId}`}
        aria-label={tr(lang, `修改 turn ${row.turnId}`, `Revise turn ${row.turnId}`, `Отправить ход ${row.turnId} на доработку`)}
        aria-pressed={current === "revise"}
        disabled={busy}
        onClick={requestRevision}
      >
        {tr(lang, "需修改", "Revise", "Доработать")}
      </button>
      {current ? (
        <span className={`badge ${current === "accept" ? "ok" : "warn"}`} aria-live="polite">
          {current === "accept"
            ? tr(lang, "已接受", "Accepted", "Принято")
            : tr(lang, "需修改", "Revised", "На доработку")}
        </span>
      ) : null}
      {current === "revise" && feedback ? (
        <>
          <span className="sub" title={feedback}>{feedback}</span>
          <button
            type="button"
            className="btn primary mini"
            data-testid={`turn-improve-${row.turnId}`}
            aria-label={tr(
              lang,
              `为 turn ${row.turnId} 提出改进`,
              `Propose improvement for turn ${row.turnId}`,
              `Предложить улучшение для хода ${row.turnId}`,
            )}
            disabled={busy}
            onClick={requestImprovement}
          >
            {tr(lang, "改进", "Improve", "Улучшить")}
          </button>
        </>
      ) : null}
    </div>
  );
}
