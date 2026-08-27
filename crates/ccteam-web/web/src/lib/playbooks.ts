// v0.9.11 TEAM-3 — 编队起手 formation playbooks: the ONE definition module
// for ccteam's multi-vendor delegation patterns, consumed by BOTH the Home
// launcher's 快速开始 template grid (HomeView) AND the Team page 分工 tab's
// card section (CharterPanel), which hands off here via router state.
//
// These are explicit user-picked composer prompts, never daemon/system prompt
// injection or a built-in agent persona. Picking one prefills the composer and
// aims the vendor draft; the spawned session performs the orchestration through
// the ordinary session_* tool surface.

import { Crown, Lightbulb, Pyramid, Radar, ShieldCheck, Trophy } from "lucide-react";
import type { TurnAttachment } from "./attachmentsApi";
import { makeT, type Lang } from "./i18n";
import type { VendorCatalog } from "./modelsApi";
import type { CreateSessionOpts } from "./sessionsApi";
import { effortRowsFor, type VendorId } from "./vendors";

export interface Playbook {
  /** Stable id — card testids (`tpl-<id>`), Team→Home router-state handoff. */
  id: string;
  /** i18n key stem: `<key>T` title, `<key>D` description, `<key>P` prefill. */
  key: string;
  Icon: typeof Crown;
  /** Brand-chip lineup; `vendors[0]` is the harness the spawn aims at. */
  vendors: readonly VendorId[];
  /** Optional explicit lead posture for playbooks that require one. */
  model?: string;
  effort?: string;
}

/** The 6 owner-approved formations (v0.9.11). Order = display order. */
export const PLAYBOOKS: ReadonlyArray<Playbook> = [
  {
    id: "commander",
    key: "tplCommander",
    Icon: Crown,
    vendors: ["claude", "codex"],
    model: "opus",
    effort: "max",
  },
  { id: "advisor", key: "tplAdvisor", Icon: Lightbulb, vendors: ["grok", "claude"] },
  { id: "crossreview", key: "tplCrossreview", Icon: ShieldCheck, vendors: ["claude", "codex"] },
  { id: "bakeoff", key: "tplBakeoff", Icon: Trophy, vendors: ["claude", "codex", "grok"] },
  { id: "triangulate", key: "tplTriangulate", Icon: Radar, vendors: ["grok", "claude", "codex"] },
  { id: "pyramid", key: "tplPyramid", Icon: Pyramid, vendors: ["kimi", "opencode", "claude"] },
];

/** The composer patch a playbook applies — prefill text + the lead vendor to
 *  aim the spawn at. Pure and node-env testable; BOTH entry paths (Home card
 *  click, Team page 起手 handoff) go through it. Unknown id → null. */
export function applyPlaybook(
  id: string,
  lang: Lang,
): { text: string; vendor: VendorId; model?: string; effort?: string } | null {
  const pb = PLAYBOOKS.find((p) => p.id === id);
  if (!pb) return null;
  return {
    text: makeT(lang)(`${pb.key}P`),
    vendor: pb.vendors[0]!,
    ...(pb.model ? { model: pb.model } : {}),
    ...(pb.effort ? { effort: pb.effort } : {}),
  };
}

export interface CommanderSpawnPosture {
  vendor: "codex";
  model?: string;
  effort?: string;
}

/** Pick the best Codex posture ccteam can substantiate for this host.
 *
 * Host installation is a hard prerequisite: an absent/failed host probe must
 * not trigger a speculative second spawn. The live catalog's first model is
 * the vendor-preferred model (the catalog preserves vendor order), and the
 * last effort is the highest advertised rung. If no model has been observed,
 * omit it and let Codex resolve its own default; the pinned Codex effort ladder
 * remains the honest cold-start fallback used by the composer itself. */
export function bestCommanderCodexPosture(
  installedVendors: readonly VendorId[] | null,
  catalog: VendorCatalog,
): CommanderSpawnPosture | null {
  if (!installedVendors?.includes("codex")) return null;

  const preferredModel = catalog.codex?.models[0];
  const efforts =
    preferredModel?.efforts !== undefined
      ? preferredModel.efforts
      : effortRowsFor("codex", catalog, preferredModel?.id ?? null).slice(1);
  const effort = efforts.at(-1);
  return {
    vendor: "codex",
    ...(preferredModel ? { model: preferredModel.id } : {}),
    ...(effort ? { effort } : {}),
  };
}

/** True only for an unavailable Commander bootstrap capability.
 *
 * The retry seam is deliberately narrower than "create failed": auth, ACL,
 * network, timeout, quota, and generic server failures retain their original
 * error. A missing executable additionally requires host-probe evidence that
 * Claude is absent, so an unrelated ENOENT cannot silently switch vendors. */
export function isCommanderBootstrapCapabilityError(
  error: unknown,
  posture: { vendor?: string; model?: string; effort?: string },
  installedVendors: readonly VendorId[] | null,
): boolean {
  if (posture.vendor !== "claude" || posture.model !== "opus" || posture.effort !== "max") {
    return false;
  }

  const message = error instanceof Error ? error.message : String(error);
  if (
    /(?:^|\b)(?:UNAUTHENTICATED|FORBIDDEN|NOT_FOUND)(?:\b|$)/i.test(message)
    || /\bHTTP\s+(?:401|403|404)\b/i.test(message)
    || /\b(?:authentication|authorization|unauthorized|not authenticated|access denied|permission denied|credentials?|api key|subscription|rate.?limit|quota|budget|timed?\s*out|timeout)\b/i.test(message)
    || /^network:/i.test(message.trim())
  ) {
    return false;
  }

  const unavailable = "(?:invalid|unknown|unsupported|unavailable|not available|not found|does not support|is not supported)";
  const axis = "(?:model|reasoning[ _-]?effort|effort)";
  const capabilityPattern = new RegExp(
    `(?:${unavailable})[^\\n]{0,100}\\b${axis}\\b|\\b${axis}\\b[^\\n]{0,100}(?:${unavailable})`,
    "i",
  );
  if (capabilityPattern.test(message)) return true;

  const claudeAbsent = installedVendors !== null && !installedVendors.includes("claude");
  return claudeAbsent
    && /(?:not installed|command not found|executable[^\n]*not found|binary[^\n]*not found|\bENOENT\b|no such file or directory)/i.test(message);
}

interface HomeTurnLaunchInput {
  slug: string;
  options: CreateSessionOpts;
  text: string;
  attachments: TurnAttachment[];
  commander: boolean;
  installedVendors: readonly VendorId[] | null;
  catalog: VendorCatalog;
}

interface HomeTurnLaunchDeps {
  createSession: (
    slug: string,
    options: CreateSessionOpts,
  ) => Promise<{ sid: string }>;
  submitTurn: (
    sid: string,
    text: string,
    attachments?: TurnAttachment[],
  ) => Promise<unknown>;
}

/** Create the lazy session and submit its first user turn.
 *
 * Commander gets one tightly-classified bootstrap fallback: when its exact
 * Claude/Opus/max posture is rejected as unavailable, retry once through an
 * installed Codex posture derived from the live catalog. The first user turn
 * is sent only after one of those creates succeeds, so it reaches exactly the
 * session returned to the caller. */
export async function createAndSubmitHomeTurn(
  input: HomeTurnLaunchInput,
  deps: HomeTurnLaunchDeps,
): Promise<string> {
  let created: { sid: string };
  try {
    created = await deps.createSession(input.slug, input.options);
  } catch (error) {
    const fallback = input.commander
      && isCommanderBootstrapCapabilityError(
        error,
        input.options,
        input.installedVendors,
      )
      ? bestCommanderCodexPosture(input.installedVendors, input.catalog)
      : null;
    if (!fallback) throw error;
    created = await deps.createSession(input.slug, {
      ...input.options,
      vendor: fallback.vendor,
      protocol: "stream-json",
      model: fallback.model,
      effort: fallback.effort,
    });
  }
  await deps.submitTurn(created.sid, input.text, input.attachments);
  return created.sid;
}

/** One-shot router-state extraction for the Team→Home handoff: the 起手 CTA
 *  navigates to `/` with `{ state: { playbook: id } }`; anything else → null. */
export function playbookFromState(state: unknown): string | null {
  if (state && typeof state === "object" && "playbook" in state) {
    const id = (state as { playbook?: unknown }).playbook;
    if (typeof id === "string") return id;
  }
  return null;
}
