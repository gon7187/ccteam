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
import { makeT, tr, type Lang } from "./i18n";
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

interface CommanderClaudePosture {
  vendor: "claude";
  model: "opus";
  effort?: string;
}

function commanderClaudePosture(catalog: VendorCatalog): CommanderClaudePosture {
  const claude = catalog.claude;
  const opus = claude?.models.find((model) => model.id === "opus");
  let efforts: string[];
  if (opus?.efforts !== undefined) {
    efforts = opus.efforts;
  } else if (opus && claude?.efforts.length) {
    efforts = claude.efforts;
  } else if (!claude || claude.models.length === 0) {
    // No live evidence exists yet. Use the same CLI-verified cold ladder as
    // the composer; once the catalog says anything about Opus, never guess.
    efforts = effortRowsFor("claude", null, "opus").slice(1);
  } else {
    efforts = [];
  }
  const effort = efforts.at(-1);
  return {
    vendor: "claude",
    model: "opus",
    ...(effort ? { effort } : {}),
  };
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
 * The retry seam is an allowlist, not a failure denylist: a typed transport
 * failure must carry one of the explicit capability codes below. Generic
 * server/network/auth failures and untyped prose therefore cannot become a
 * retry merely because their message also mentions a model. */
export function isCommanderBootstrapCapabilityError(
  error: unknown,
  posture: { vendor?: string; model?: string; effort?: string },
): boolean {
  if (posture.vendor !== "claude" || posture.model !== "opus") {
    return false;
  }

  const shape = error && typeof error === "object" ? (error as Record<string, unknown>) : null;
  const status = typeof shape?.status === "number" ? shape.status : null;
  const rawCode = [shape?.errorCode, shape?.error_code, shape?.code]
    .find((value): value is string => typeof value === "string");
  const code = rawCode?.trim().toUpperCase().replace(/[.-]/g, "_") ?? "";
  const vendor = typeof shape?.vendor === "string" ? shape.vendor : "";
  const capabilityCodes = new Set([
    "VENDOR_UNAVAILABLE",
    "MODEL_UNAVAILABLE",
    "EFFORT_UNAVAILABLE",
  ]);
  const capabilityStatus = status === null || status === 400 || status === 422;
  const vendorMatches = !vendor || vendor.toLowerCase() === "claude";
  return capabilityStatus && vendorMatches && capabilityCodes.has(code);
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

/** The posture that actually created the session. This is deliberately not a
 * copy of the user's initial draft: Commander may have taken its one allowed
 * Codex fallback, and the UI must report that fact instead of implying Opus. */
export interface HomeTurnLaunchReceipt {
  sid: string;
  vendor: string;
  model?: string;
  effort?: string;
  fallback: boolean;
}

function sanitizeLaunchCause(error: unknown): string {
  const raw = (error instanceof Error ? error.message : String(error)).replace(/\r\n?/g, "\n");
  const printable = Array.from(raw, (character) => {
    if (character === "\n") return character;
    const code = character.charCodeAt(0);
    return code < 32 || code === 127 ? " " : character;
  }).join("");
  const redacted = printable
    .split("\n")
    .map((line) =>
      line
        .replace(
          /\b(authorization|proxy-authorization|set-cookie|cookie)\s*:\s*.*$/i,
          "$1: [redacted]",
        )
        .replace(
          /["']?\b(access_token|refresh_token|client_secret|api[ _-]?key|password|passwd|secret|token|set-cookie|cookie)\b["']?(\s*[:=]\s*)(?:"[^"]*"|'[^']*'|[^\s,;]+)/gi,
          "$1$2[redacted]",
        )
        .replace(/\b(Bearer|Basic)\s+[^\s,;]+/gi, "$1 [redacted]"),
    )
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
  return redacted.slice(0, 240) || "unknown";
}

function receiptFor(
  created: { sid: string },
  options: CreateSessionOpts,
  fallback: boolean,
): HomeTurnLaunchReceipt {
  return {
    sid: created.sid,
    vendor: options.vendor ?? "claude",
    ...(options.model ? { model: options.model } : {}),
    ...(options.effort ? { effort: options.effort } : {}),
    fallback,
  };
}

/** Report the posture that really launched, then navigate to that sid. The
 * caller supplies the visible notification seam so this helper stays usable
 * outside React and cannot mutate the user's first-turn prompt. */
export function completeHomeLaunch(
  receipt: HomeTurnLaunchReceipt,
  lang: Lang,
  notify: (message: string) => void,
  onLaunched: (sid: string) => void,
): void {
  const details = [receipt.sid, receipt.vendor, receipt.model, receipt.effort].filter(Boolean);
  if (receipt.fallback) {
    details.push(tr(lang, "Commander 回退", "Commander fallback", "fallback Commander"));
  }
  notify(`${tr(lang, "已启动", "Launched", "Запущено")} ${details.join(" · ")}`);
  onLaunched(receipt.sid);
}

/** Create the lazy session and submit its first user turn.
 *
 * Commander gets one tightly-classified bootstrap fallback: when its exact
 * Claude/Opus posture is rejected as unavailable, retry once through an
 * installed Codex posture derived from the live catalog. The first user turn
 * is sent only after one of those creates succeeds, so it reaches exactly the
 * session returned to the caller. */
export async function createAndSubmitHomeTurn(
  input: HomeTurnLaunchInput,
  deps: HomeTurnLaunchDeps,
): Promise<HomeTurnLaunchReceipt> {
  // Commander owns its bootstrap posture before HomeView's generic host
  // normalization: a host proven to lack Claude may use the best confirmed
  // Codex posture, but an unrelated installed vendor must never become the
  // Commander merely because it is first in the host menu.
  let initialOptions = input.options;
  let directFallback = false;
  if (input.commander) {
    const claudeConfirmedAbsent =
      input.installedVendors !== null && !input.installedVendors.includes("claude");
    const codex = claudeConfirmedAbsent
      ? bestCommanderCodexPosture(input.installedVendors, input.catalog)
      : null;
    if (codex) {
      initialOptions = {
        ...input.options,
        vendor: codex.vendor,
        protocol: "stream-json",
        model: codex.model,
        effort: codex.effort,
      };
    } else {
      const claude = commanderClaudePosture(input.catalog);
      initialOptions = {
        ...input.options,
        ...claude,
        protocol: "stream-json",
      };
      if (!claude.effort) delete initialOptions.effort;
    }
    directFallback = codex !== null;
  }

  let created: { sid: string };
  let actualOptions = initialOptions;
  let fallbackUsed = directFallback;
  try {
    created = await deps.createSession(input.slug, initialOptions);
  } catch (primaryError) {
    const fallback = input.commander
      && isCommanderBootstrapCapabilityError(
        primaryError,
        initialOptions,
      )
      ? bestCommanderCodexPosture(input.installedVendors, input.catalog)
      : null;
    if (!fallback) throw primaryError;
    actualOptions = {
      ...initialOptions,
      vendor: fallback.vendor,
      protocol: "stream-json",
      model: fallback.model,
      effort: fallback.effort,
    };
    fallbackUsed = true;
    try {
      created = await deps.createSession(input.slug, actualOptions);
    } catch (fallbackError) {
      throw new Error(
        `Commander bootstrap failed; primary: ${sanitizeLaunchCause(primaryError)}; fallback: ${sanitizeLaunchCause(fallbackError)}`,
      );
    }
  }
  await deps.submitTurn(created.sid, input.text, input.attachments);
  return receiptFor(created, actualOptions, fallbackUsed);
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
