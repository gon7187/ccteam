// @vitest-environment jsdom

import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EvolutionSummary } from "../lib/workflowApi";

const mocks = vi.hoisted(() => ({
  getEvolution: vi.fn(),
}));

vi.mock("../hooks/useProjectsStore", () => ({
  useProjectsStore: () => ({
    projects: [{ slug: "alpha" }, { slug: "beta" }],
    loading: false,
    error: null,
  }),
}));

vi.mock("../lib/workflowApi", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/workflowApi")>()),
  getEvolution: mocks.getEvolution,
}));

import WorkflowView from "./WorkflowView";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

function summary(slug: string, marker: string): EvolutionSummary {
  return {
    slug,
    turn_records: 1,
    verdict_records: 0,
    turn_records_7d: 1,
    accepted_turns: 0,
    revised_turns: 0,
    unrated_turns: 1,
    completed_turns: 1,
    failed_turns: 0,
    outcome_unknown_turns: 0,
    priced_turns: 0,
    unpriced_turns: 1,
    avg_duration_ms: null,
    roles: [
      {
        kind: "role",
        id: marker,
        sha: `${marker}-sha`,
        turn_count: 1,
      },
    ],
    skills: [],
    skill_attribution: "available_at_spawn",
    empty: false,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("WorkflowView project-scoped Evolution state", () => {
  let container: HTMLDivElement;
  let root: ReturnType<typeof createRoot>;

  beforeEach(() => {
    mocks.getEvolution.mockReset();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
  });

  it("accepts beta then ignores a late alpha response", async () => {
    const alpha = deferred<EvolutionSummary>();
    const beta = deferred<EvolutionSummary>();
    mocks.getEvolution.mockReturnValueOnce(alpha.promise).mockReturnValueOnce(beta.promise);

    await act(async () => {
      root.render(<WorkflowView tab="evolution" lang="en" />);
    });
    expect(mocks.getEvolution).toHaveBeenNthCalledWith(1, "alpha");

    const project = container.querySelector<HTMLSelectElement>(
      '[data-testid="workflow-project"]',
    );
    expect(project).not.toBeNull();
    await act(async () => {
      if (!project) return;
      project.value = "beta";
      project.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(mocks.getEvolution).toHaveBeenNthCalledWith(2, "beta");

    await act(async () => beta.resolve(summary("beta", "BETA-MARKER")));
    expect(container.textContent).toContain("BETA-MARKER");

    await act(async () => alpha.resolve(summary("alpha", "ALPHA-MARKER")));
    expect(container.textContent).toContain("BETA-MARKER");
    expect(container.textContent).not.toContain("ALPHA-MARKER");
  });

  it("drops already-rendered alpha data as soon as beta starts loading", async () => {
    const beta = deferred<EvolutionSummary>();
    mocks.getEvolution
      .mockResolvedValueOnce(summary("alpha", "ALPHA-MARKER"))
      .mockReturnValueOnce(beta.promise);

    await act(async () => {
      root.render(<WorkflowView tab="evolution" lang="en" />);
    });
    expect(container.textContent).toContain("ALPHA-MARKER");

    const project = container.querySelector<HTMLSelectElement>(
      '[data-testid="workflow-project"]',
    );
    await act(async () => {
      if (!project) return;
      project.value = "beta";
      project.dispatchEvent(new Event("change", { bubbles: true }));
    });

    expect(container.textContent).not.toContain("ALPHA-MARKER");
    expect(container.textContent).toContain("Loading");
  });
});
