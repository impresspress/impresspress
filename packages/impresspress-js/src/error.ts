/**
 * Typed error thrown by every SDK service for a non-2xx server response or a
 * transport failure (network error, timeout, abort).
 *
 * The real impresspress server has no `{success, error: {code, message}}`
 * envelope — every JSON error response is the flat shape produced by
 * `wafer_block::response`'s HTTP error mapping:
 *
 *   { "error": "<WaferErrorCode>", "message": "<human message>" }
 *
 * `code` here is that `error` field (e.g. `"NotFound"`, `"Unauthenticated"`),
 * NOT the finer-grained impresspress `ErrorCode` string (e.g.
 * `"invalid_credentials"`) that some handlers additionally attach as
 * structured meta — that value, when present, is surfaced as `detailCode`.
 */

/**
 * Codes the SDK raises itself, as opposed to the `WaferErrorCode` it echoes
 * from a server error body. `code` on `ImpresspressError` stays a plain
 * `string` because the server side of that vocabulary is open-ended; this
 * union documents the closed client side so a caller can branch on it
 * without matching message text.
 *
 * `"aborted"` and `"timeout"` are the transport's own words for "the caller
 * cancelled" and "the deadline passed", and the OAuth popup reuses them for
 * exactly those two events rather than inventing a parallel vocabulary.
 */
export type SdkErrorCode =
  /** The caller's `AbortSignal` fired (or the OAuth popup session was cancelled). */
  | "aborted"
  /** The request (or the OAuth popup session) ran past its deadline. */
  | "timeout"
  /** `fetch` failed before a response existed (DNS, TLS, offline, CORS). */
  | "network_error"
  /** A non-2xx response whose body named no `error` code. */
  | "internal_error"
  /** A method `HttpClient` cannot send was passed to `request`. */
  | "unsupported_method"
  /** `uploadFile` was handed something that is not a File, Blob, or Buffer. */
  | "invalid_file_type"
  /** `refreshSession` had no cached token and none was passed. */
  | "no_refresh_token"
  /** The OAuth popup posted back an `error` of its own. */
  | "oauth_error"
  /** The OAuth popup completed, but no session existed afterwards. */
  | "authentication_failed"
  /** `window.open` returned null — a popup blocker stopped the flow. */
  | "popup_blocked"
  /** The OAuth popup closed before the flow completed. */
  | "popup_closed";

/**
 * The detail code the database attaches when a request's invocation has no
 * database call left (on Cloudflare D1, its per-invocation call limit, a
 * `db.batch()` counting once). Sent again unchanged, such a request fails the
 * same way, so it must not be retried automatically — a 429 carrying
 * `database.call_budget_exhausted` is NOT a rate limit. Do less per request
 * instead. See {@link isCallBudgetError}.
 */
export const CALL_BUDGET_DETAIL_CODE = "database.call_budget_exhausted";

export class ImpresspressError extends Error {
  /** Coarse wafer error code from the `error` field (e.g. "NotFound"). */
  public readonly code: string;
  /** HTTP status code, or 0 for network/timeout/abort failures. */
  public readonly status: number;
  /**
   * Fine-grained error code, when the server attached one: an impresspress
   * code (`"invalid_credentials"`, `"rate_limit_exceeded"`) or a namespaced
   * runtime code (`"database.call_budget_exhausted"`). Branch on this
   * rather than on `status`: two errors can share a status and differ in
   * whether a retry can succeed.
   */
  public readonly detailCode?: string;
  /** Raw parsed response body, if any. */
  public readonly data: unknown;

  constructor(
    code: string,
    message: string,
    status = 0,
    data: unknown = null,
    detailCode?: string,
  ) {
    super(message);
    this.name = "ImpresspressError";
    this.code = code;
    this.status = status;
    this.data = data;
    this.detailCode = detailCode;
    Object.setPrototypeOf(this, ImpresspressError.prototype);
  }
}

/**
 * True for a response the server represents as "the thing you asked for
 * does not exist" — safe for callers to fold into `null`/absence. Every
 * OTHER failure (auth outage, validation error, 5xx, network failure) must
 * propagate, not be swallowed into a fabricated empty/default value.
 */
export function isNotFoundError(error: unknown): error is ImpresspressError {
  return error instanceof ImpresspressError && error.status === 404;
}

/**
 * True for a response the server represents as "you are not signed in" —
 * the other case callers may fold into absence (e.g. `getUser()` returning
 * `null` for an anonymous caller rather than throwing).
 */
export function isUnauthorizedError(error: unknown): error is ImpresspressError {
  return error instanceof ImpresspressError && error.status === 401;
}

/**
 * True for the database's call-budget refusal (see
 * {@link CALL_BUDGET_DETAIL_CODE}): the request did more database work than
 * one request may. Do not retry it as it stands — unlike a
 * `rate_limit_exceeded` 429, waiting does not help.
 */
export function isCallBudgetError(error: unknown): error is ImpresspressError {
  return error instanceof ImpresspressError && error.detailCode === CALL_BUDGET_DETAIL_CODE;
}
