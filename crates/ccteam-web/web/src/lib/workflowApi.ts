// v0.8.24 A3 — Workflow APIs: evolution and project MCP panels.

import { httpError } from "./httpError";

export interface EvolutionMetrics {
  accepted_turns?: number;
  revised_turns?: number;
  unrated_turns?: number;
  completed_turns?: number;
  failed_turns?: number;
  outcome_unknown_turns?: number;
  priced_turns?: number;
  unpriced_turns?: number;
  avg_duration_ms?: number | null;
}

export interface EvolutionBucket extends EvolutionMetrics {
  kind: string;
  id: string;
  sha: string;
  turn_count: number;
  avg_cost_usd?: number | null;
  total_cost_usd?: number | null;
}

export interface EvolutionSummary extends EvolutionMetrics {
  slug: string;
  turn_records: number;
  verdict_records: number;
  /** v0.8.24 — turn records written in the last 7 days (trend stat). */
  turn_records_7d: number;
  roles: EvolutionBucket[];
  skills: EvolutionBucket[];
  /** Skill hashes describe what was available when the session spawned; they
   * do not prove that the agent invoked a skill. */
  skill_attribution?: "available_at_spawn" | string;
  empty: boolean;
}

// ── project MCP servers (v0.8.24 F1.12) ──────────────────────────────────────

export interface McpServerView {
  name: string;
  /** `stdio` | `http` | `sse` | `unknown`. */
  kind: string;
  command?: string | null;
  args?: string[] | null;
  url?: string | null;
  /** Env var NAMES only — values never echo. */
  env_keys?: string[] | null;
  is_ccteam: boolean;
}

export interface McpServersResponse {
  servers: McpServerView[];
  ccteam_registered: boolean;
}

/** POST body: exactly one of `url` (http/sse) or `command` (stdio). */
export interface RegisterMcpServerForm {
  name: string;
  url?: string;
  command?: string;
  args?: string[];
}

export function evolutionUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/evolution`;
}

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, {
    headers: { Accept: "application/json" },
    credentials: "same-origin",
  });
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}

async function postJson<T>(url: string, body: unknown): Promise<T> {
  const res = await fetch(url, {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify(body),
  });
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as T;
}

export function getEvolution(slug: string): Promise<EvolutionSummary> {
  return getJson<EvolutionSummary>(evolutionUrl(slug));
}

export function mcpServersUrl(slug: string): string {
  return `/api/v1/projects/${encodeURIComponent(slug)}/mcp-servers`;
}

/** `GET .../mcp-servers` — the project `.mcp.json` (masked view). */
export function getMcpServers(slug: string): Promise<McpServersResponse> {
  return getJson<McpServersResponse>(mcpServersUrl(slug));
}

/** `POST .../mcp-servers` — idempotently merge one third-party server into
 *  the project `.mcp.json` (project-owner ACL; config write, never executes). */
export function registerMcpServer(
  slug: string,
  form: RegisterMcpServerForm,
): Promise<{ ok: boolean; name: string; path: string }> {
  return postJson(mcpServersUrl(slug), form);
}
