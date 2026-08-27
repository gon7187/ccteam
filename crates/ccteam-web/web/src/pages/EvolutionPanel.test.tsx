import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";

import EvolutionPanel from "./EvolutionPanel";
import type { EvolutionSummary } from "../lib/workflowApi";

const SUMMARY: EvolutionSummary = {
  slug: "demo",
  turn_records: 12,
  verdict_records: 7,
  turn_records_7d: 5,
  accepted_turns: 4,
  revised_turns: 3,
  unrated_turns: 5,
  completed_turns: 9,
  failed_turns: 2,
  outcome_unknown_turns: 1,
  priced_turns: 8,
  unpriced_turns: 4,
  avg_duration_ms: 1520,
  skill_attribution: "available_at_spawn",
  roles: [
    {
      kind: "role",
      id: "reviewer",
      sha: "abcdef0123456789",
      turn_count: 7,
      accepted_turns: 3,
      revised_turns: 2,
      unrated_turns: 2,
      completed_turns: 5,
      failed_turns: 1,
      outcome_unknown_turns: 1,
      priced_turns: 4,
      unpriced_turns: 3,
      avg_duration_ms: 2200,
      avg_cost_usd: 0.25,
      total_cost_usd: 1,
    },
  ],
  skills: [],
  empty: false,
};

describe("EvolutionPanel", () => {
  it("renders honest feedback, outcome, pricing, duration, and fingerprint revision metrics", () => {
    const html = renderToString(<EvolutionPanel lang="en" evolution={SUMMARY} loading={false} />)
      .replace(/<!-- -->/g, "");

    expect(html).toContain('data-testid="evolution-feedback"');
    expect(html).toContain("4 accepted · 3 revised · 5 unrated");
    expect(html).toContain("9 completed · 2 failed · 1 unknown");
    expect(html).toContain("8 priced · 4 unpriced");
    expect(html).toContain("1.5 s");
    expect(html).toContain('data-testid="evolution-bucket-role-0"');
    expect(html).toContain("role:reviewer");
    expect(html).toContain("abcdef0123");
    expect(html).toContain("3 accepted · 2 revised · 2 unrated");
    expect(html).toContain("$1.00 total · $0.25 avg");
    expect(html).toContain("Skills are attributed to what was available when the session spawned");
    expect(html).not.toContain("read-only");
  });

  it("localizes the human-feedback metrics in Chinese and Russian", () => {
    const zh = renderToString(<EvolutionPanel lang="zh" evolution={SUMMARY} loading={false} />);
    const ru = renderToString(<EvolutionPanel lang="ru" evolution={SUMMARY} loading={false} />);
    expect(zh).toContain("已接受");
    expect(zh).toContain("需修改");
    expect(zh).toContain("未评价");
    expect(ru).toContain("принято");
    expect(ru).toContain("на доработку");
    expect(ru).toContain("без оценки");
  });

  it("keeps optional, null, and invalid analytics counts unknown while preserving numeric zero", () => {
    const partial = {
      slug: "old",
      turn_records: 0,
      verdict_records: 0,
      turn_records_7d: null,
      accepted_turns: 0,
      revised_turns: null,
      unrated_turns: Number.POSITIVE_INFINITY,
      failed_turns: 1.5,
      outcome_unknown_turns: "3",
      priced_turns: 0,
      unpriced_turns: -1,
      roles: [
        {
          kind: "role",
          id: "",
          sha: "",
          accepted_turns: 0,
          revised_turns: null,
          unrated_turns: Number.NaN,
        },
      ],
      skills: [],
      empty: false,
    } as unknown as EvolutionSummary;
    const html = renderToString(
      <EvolutionPanel lang="en" evolution={partial} loading={false} />,
    ).replace(/<!-- -->/g, "");

    expect(html).toContain("0 accepted · — revised · — unrated");
    expect(html).toContain("— completed · — failed · — unknown");
    expect(html).toContain("0 priced · — unpriced");
    expect(html).toContain("last 7 days +—");
    expect(html).toContain("0 turn records");
    expect(html).toContain("turns=—");
    expect(html).toContain('data-testid="evolution-duration"');
    expect(html).toContain(">—<");
    expect(html).not.toContain("0 ms");
    expect(html).not.toContain("$0.00");
    expect(html).toContain("— total · — avg");
    expect(html).toContain("Skill attribution is unavailable on this daemon");
  });

  it("renders a localized honest empty state", () => {
    const empty = { ...SUMMARY, empty: true };
    expect(
      renderToString(<EvolutionPanel lang="ru" evolution={empty} loading={false} />),
    ).toContain("Данных об опыте пока нет");
  });

  it("labels the honest empty role id as the default role bucket", () => {
    const roleless = {
      ...SUMMARY,
      roles: [{ ...SUMMARY.roles[0]!, id: "", sha: "unknown" }],
    };
    const html = renderToString(
      <EvolutionPanel lang="en" evolution={roleless} loading={false} />,
    ).replace(/<!-- -->/g, "");
    expect(html).toContain("role:(default)");
    expect(html).not.toContain("role:__default__");
  });
});
