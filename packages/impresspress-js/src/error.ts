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
 * `code` here is that `error` field (e.g. `"NotFound"`, `"Unauthorized"`),
 * NOT the finer-grained impresspress `ErrorCode` string (e.g.
 * `"invalid_credentials"`) that some handlers additionally attach as
 * structured meta — that value, when present, is surfaced as `detailCode`.
 */
export class ImpresspressError extends Error {
  /** Coarse wafer error code from the `error` field (e.g. "NotFound"). */
  public readonly code: string;
  /** HTTP status code, or 0 for network/timeout/abort failures. */
  public readonly status: number;
  /** Fine-grained impresspress error code, when the server attached one. */
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
