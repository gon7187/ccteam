// Shared non-2xx → Error mapping for every REST client in `lib/`.
//
// Why this exists: each client used to throw a bare `HTTP ${res.status}` and
// drop the response body on the floor. Every ccteam handler answers with
// `{"error": "..."}` saying exactly what went wrong ("project not found: x",
// "unknown tenant: y", "no Telegram bot configured; save the bot token
// first") — and none of it ever reached the user, who saw only "404" and had
// no way to tell an ACL denial from a typo'd slug. A status code alone is not
// a report; the server already wrote the report, so deliver it.

interface ErrorPayload {
  detail: string | null;
  errorCode?: string;
}

/** A non-2xx API response with its machine-readable transport facts intact. */
export class ApiError extends Error {
  readonly status: number;
  readonly errorCode?: string;

  constructor(status: number, message: string, errorCode?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    if (errorCode) this.errorCode = errorCode;
  }
}

function safeErrorCode(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const code = value.trim();
  return /^[A-Za-z0-9][A-Za-z0-9_.-]{0,79}$/.test(code) ? code : undefined;
}

/** Best-effort read of a ccteam error body. Never throws: a non-JSON or empty
 * body (or one already consumed) just yields no detail or machine code. */
async function errorPayload(res: Response): Promise<ErrorPayload> {
  try {
    const text = await res.text();
    if (!text) return { detail: null };
    try {
      const body: unknown = JSON.parse(text);
      if (body && typeof body === "object") {
        const record = body as Record<string, unknown>;
        const errorCode = safeErrorCode(record.error_code);
        if (typeof record.error === "string" && record.error.trim()) {
          return { detail: record.error.trim(), ...(errorCode ? { errorCode } : {}) };
        }
      }
    } catch {
      // Not JSON (axum extractor rejections are plain text) — use it verbatim.
    }
    return { detail: text.slice(0, 300).trim() || null };
  } catch {
    return { detail: null };
  }
}

export async function errorDetail(res: Response): Promise<string | null> {
  return (await errorPayload(res)).detail;
}

/** `HTTP <status>: <server's reason>`, falling back to `HTTP <status>` when
 *  the response carried none. Callers `throw await httpError(res)`. */
export async function httpError(res: Response): Promise<ApiError> {
  const { detail, errorCode } = await errorPayload(res);
  const message = detail ? `HTTP ${res.status}: ${detail}` : `HTTP ${res.status}`;
  return new ApiError(res.status, message, errorCode);
}
