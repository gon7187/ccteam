import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  if (typeof g.window === "undefined") {
    g.window = { prompt: () => null, confirm: () => false, alert: () => {} };
  }
});

import TurnVerdictControls from "./TurnVerdictControls";
import type { TranscriptRow } from "./chatTranscript";

function row(overrides: Partial<TranscriptRow> = {}): TranscriptRow {
  return {
    id: "t1-a",
    kind: "assistant",
    content: "answer",
    turnId: "t1",
    ...overrides,
  };
}

function findByTestId(value: unknown, testId: string): { props: Record<string, unknown> } | null {
  if (Array.isArray(value)) {
    for (const child of value) {
      const found = findByTestId(child, testId);
      if (found) return found;
    }
    return null;
  }
  if (!value || typeof value !== "object") return null;
  const props = (value as { props?: Record<string, unknown> }).props;
  if (!props) return null;
  if (props["data-testid"] === testId) return { props };
  return findByTestId(props.children, testId);
}

describe("TurnVerdictControls", () => {
  afterEach(() => vi.restoreAllMocks());

  it("renders accessible Accept and Revise controls only for a completed row", () => {
    const html = renderToString(
      <TurnVerdictControls
        lang="en"
        row={row()}
        busy={false}
        onVerdict={() => {}}
        onImprove={() => {}}
      />,
    );
    expect(html).toContain('role="group"');
    expect(html).toContain('aria-label="Feedback for completed turn t1"');
    expect(html).toContain('data-testid="turn-accept-t1"');
    expect(html).toContain('data-testid="turn-revise-t1"');
    expect(html).not.toContain('data-testid="turn-improve-t1"');
  });

  it("submits trimmed nonempty revise feedback and rejects empty or oversized input", () => {
    const onVerdict = vi.fn();
    const alert = vi.spyOn(window, "alert").mockImplementation(() => {});
    const prompt = vi.spyOn(window, "prompt");
    const tree = TurnVerdictControls({
      lang: "en",
      row: row(),
      busy: false,
      onVerdict,
      onImprove: () => {},
    });
    const revise = findByTestId(tree, "turn-revise-t1");

    prompt.mockReturnValueOnce("  Cover the failure path  ");
    (revise?.props.onClick as () => void)();
    expect(onVerdict).toHaveBeenLastCalledWith("revise", "Cover the failure path");

    prompt.mockReturnValueOnce("   ");
    (revise?.props.onClick as () => void)();
    prompt.mockReturnValueOnce("x".repeat(4001));
    (revise?.props.onClick as () => void)();
    prompt.mockReturnValueOnce("🚀".repeat(4000));
    (revise?.props.onClick as () => void)();
    expect(onVerdict).toHaveBeenCalledTimes(2);
    expect(onVerdict).toHaveBeenLastCalledWith("revise", "🚀".repeat(4000));
    expect(alert).toHaveBeenCalledTimes(2);
  });

  it("offers Improve only for revised feedback and guards it with native confirm", () => {
    const onImprove = vi.fn();
    const confirm = vi.spyOn(window, "confirm");
    const reviewed = row({
      verdict: {
        verdict: "revise",
        feedback: "Cover the failure path",
        ts: "2026-08-28T00:00:00Z",
      },
    });
    const tree = TurnVerdictControls({
      lang: "ru",
      row: reviewed,
      busy: false,
      onVerdict: () => {},
      onImprove,
    });
    const improve = findByTestId(tree, "turn-improve-t1");
    expect(improve).not.toBeNull();
    expect(improve?.props["aria-label"]).toContain("Предложить улучшение");

    confirm.mockReturnValueOnce(false);
    (improve?.props.onClick as () => void)();
    expect(confirm).toHaveBeenLastCalledWith(expect.stringContaining("HITL"));
    expect(confirm).toHaveBeenLastCalledWith(expect.stringContaining("отдельную"));
    expect(onImprove).not.toHaveBeenCalled();
    confirm.mockReturnValueOnce(true);
    (improve?.props.onClick as () => void)();
    expect(onImprove).toHaveBeenCalledOnce();
    expect(onImprove).toHaveBeenCalledWith("Cover the failure path");
  });

  it("marks the current verdict and localizes status in Chinese", () => {
    const html = renderToString(
      <TurnVerdictControls
        lang="zh"
        row={row({
          verdict: { verdict: "accept", feedback: null, ts: "2026-08-28T00:00:00Z" },
        })}
        busy={false}
        onVerdict={() => {}}
        onImprove={() => {}}
      />,
    );
    expect(html).toContain("已接受");
    expect(html).toContain('aria-pressed="true"');
  });
});
