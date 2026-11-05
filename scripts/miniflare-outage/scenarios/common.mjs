// What several scenarios assert, in one place.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { sleep } from "../harness.mjs";

/**
 * What the D1 wrapper's `quota` mode throws: part of the text D1 answered in
 * the 2026-10-30 outage (`workers/d1-control.mjs`). The cause a refusal's log
 * line must carry in that mode.
 */
export const QUOTA_CAUSE = "exceeded D1's free tier daily row read limit";

/** The block a request to `/b/auth/login` is dispatched to. */
export const AUTH_UI = "impresspress/auth-ui";

/** The isolate's retry window doubles from 1 s (`RETRY_WINDOW_BASE_MS`) to 60 s. */
export const WINDOW_BASE_MS = 1_000;
export const WINDOW_CAP_MS = 60_000;

/**
 * How long a scenario waits for something that happens after a window ends:
 * the longest window plus a rebuild on a slow runner. A deadline against a
 * hung run, never the property asserted.
 */
export const RECOVERY_DEADLINE_MS = WINDOW_CAP_MS + 30_000;

/**
 * A `/b/static/` path the release uploaded: a UI asset at the bucket root,
 * served by the Worker's R2 read-through without a runtime.
 */
export function staticAssetPath(deployment) {
  const types = JSON.parse(
    readFileSync(join(deployment.candidate.r2, "content-types.json"), "utf8"),
  );
  const css = Object.keys(types).find((key) => !key.includes("/") && key.endsWith(".css"));
  assert.ok(css, "the release uploaded a root-level UI stylesheet");
  return `/b/static/${css}`;
}

/**
 * Text a client must never be sent, because it is the cause and not the
 * answer: D1's own error text, an `Init` failure as wafer-run words it, a
 * table name, the call budget's message and its numbers. A cause belongs in
 * the Worker's log, under the reference the body names.
 */
const CAUSE_TEXTS = [
  /D1_ERROR/i,
  /SQLITE/i,
  /no such table/i,
  /exceeded D1/i,
  /impresspress__/,
  /init (transient|permanent) failure/i,
  /not initialized in this runtime/i,
  /config (fetch failed|source fault)/i,
  /call budget/i,
  /\d+ D1 calls?/i,
];

/** Fail if `body` carries any of the [`CAUSE_TEXTS`]. */
export function assertNoCause(body, what) {
  for (const cause of CAUSE_TEXTS) {
    assert.ok(!cause.test(body), `${what}: the body carries the cause (${cause}): ${body.slice(0, 300)}`);
  }
}

/**
 * An outage answer: 503, a `Retry-After` of at least a second, a body a
 * person can read, and none of the cause in it. Holds for every 503 shape:
 * a refused dispatch (`assertRefused`), the edge's own plain-text body (no
 * runtime), a stored-config read failing closed, and `/health`'s report.
 */
export function assertUnavailable(answer, what) {
  assert.equal(answer.status, 503, `${what}: status (body: ${answer.body.slice(0, 200)})`);
  assert.ok(answer.retryAfter >= 1, `${what}: Retry-After is ${answer.retryAfter}`);
  assert.ok(answer.body.trim().length > 0, `${what}: an empty body`);
  assertNoCause(answer.body, what);
}

/**
 * The detail codes a refusal may carry in its body's `code`: the refused
 * block's last `Init` failure, when the database was down for it (wafer-run
 * #483) — the one class true of another caller's request — never its text.
 */
const REFUSAL_CODES = ["database.unavailable"];

/**
 * The refusal of a dispatch to `block`, which wafer-run answers for a block
 * whose `Init` has not succeeded in this runtime: an outage answer whose body
 * is exactly `{"error":"Unavailable","message":"block `<block>` is
 * temporarily unavailable (ref: <16 hex>)"}`, plus the failure's class as
 * `code` when it has one (`REFUSAL_CODES`). Returns the reference; the cause
 * is in the log under it (`assertCauseLogged`).
 */
export function assertRefused(answer, what, block) {
  assertUnavailable(answer, what);
  const body = JSON.parse(answer.body);
  const reference = /^block `([^`]+)` is temporarily unavailable \(ref: ([0-9a-f]{16})\)$/.exec(
    body.message ?? "",
  );
  assert.ok(reference, `${what}: the message is the fixed refusal text: ${answer.body}`);
  const { code, ...rest } = body;
  assert.deepEqual(
    rest,
    { error: "Unavailable", message: body.message },
    `${what}: the body is the error, the message and the failure's class only`,
  );
  assert.ok(
    code === undefined || REFUSAL_CODES.includes(code),
    `${what}: the code is a failure's class: ${answer.body}`,
  );
  assert.equal(reference[1], block, `${what}: the refusal names the block`);
  return reference[2];
}

/**
 * The Worker's log has one line with `correlation_id=<reference>` that also
 * carries `cause`: what an operator greps for when a client quotes the
 * reference.
 */
export async function assertCauseLogged(deployment, reference, cause, since = 0) {
  const line = await deployment.logLine((l) => l.includes(`correlation_id=${reference} `), since);
  assert.ok(line.includes(cause), `the log line under ${reference} names the cause ${JSON.stringify(cause)}: ${line}`);
}

/** 200, and the body is the page asked for. */
export function assertServed(answer, what, contains) {
  assert.equal(answer.status, 200, `${what}: status (body: ${answer.body.slice(0, 200)})`);
  if (contains !== undefined) {
    assert.ok(answer.body.includes(contains), `${what}: body lacks ${JSON.stringify(contains)}`);
  }
}

/**
 * A `/health` report (IP-C, D1 outage plan I6), checked against its own
 * contract: `Cache-Control: no-store`; for `ok` a 200 whose body is exactly
 * `{"status":"ok"}` and no `Retry-After`; for every other status a 503 whose
 * `Retry-After` is its `retry_in`, at most the longest window, and whose body
 * names no cause. Returns the parsed body.
 */
export function assertHealth(answer, what, status) {
  assert.equal(answer.headers.get("cache-control"), "no-store", `${what}: Cache-Control`);
  const body = JSON.parse(answer.body);
  assert.equal(body.status, status, `${what}: status (${answer.body.slice(0, 300)})`);
  if (status === "ok") {
    assert.equal(answer.status, 200, `${what}: HTTP status`);
    assert.deepEqual(body, { status: "ok" }, `${what}: body`);
    assert.equal(answer.retryAfter, null, `${what}: no Retry-After`);
    return body;
  }
  assertUnavailable(answer, what);
  assert.equal(answer.retryAfter, body.retry_in, `${what}: Retry-After is retry_in`);
  assert.ok(body.retry_in <= WINDOW_CAP_MS / 1000, `${what}: retry_in ${body.retry_in} s is past the longest window`);
  return body;
}

/**
 * The `retry_in` a report answered at some instant of `answer`'s flight may
 * name, for a window that was armed at some instant of `armedBy`'s flight to
 * end `windowMs` later (the `retry_in_ms` its "runtime retry window armed"
 * line logs): whole seconds rounded up, at least 1. Bounds from the two
 * requests' own clocks, so a correct report is inside them however slow the
 * run.
 */
export function windowRetryIn(armedBy, windowMs, answer) {
  const secs = (ms) => Math.max(1, Math.ceil(ms / 1000));
  return {
    min: secs(armedBy.sentAt + windowMs - answer.doneAt),
    max: secs(armedBy.doneAt + windowMs - answer.sentAt),
  };
}

/** The `retry_in_ms` of the first "runtime retry window armed" line since `since`. */
export async function armedWindowMs(deployment, since) {
  const line = await deployment.logLine((l) => l.includes("runtime retry window armed"), since);
  return Number(/retry_in_ms=(\d+)/.exec(line)[1]);
}

/**
 * Whether a request's D1 calls include a failed check of the backend: a read
 * that failed, followed by the probe of whether its table exists, to tell a
 * fresh database from an outage (`impresspress-core`'s `read_unfiltered`,
 * `block_settings::load`). That is what each retry after a window sends
 * during an outage: the gate, the variables read the runtime cache makes
 * first, before anything else and before it builds anything (or, when the
 * gate passed, a `block_settings` read after it that failed): a retry, not
 * a build. Whether a runtime was assembled is the invocation's `d1_cost` line
 * ([`buildsSince`]).
 */
export function probedBackend(calls) {
  return calls.some((call) =>
    call.statements.some((sql) => sql.startsWith("SELECT EXISTS(SELECT 1 FROM sqlite_master")),
  );
}

/**
 * The `build=` of every `metric=d1_cost` line since `since` (a `mark()`),
 * once the log has settled, leaving out `none` (an invocation that only read
 * rows). `prepared` and `dynamic` mean a runtime was assembled — the costly
 * part, failed or not — and `gate` an attempt that assembled nothing
 * (`impresspress-cloudflare`'s `BuildKind`).
 */
export async function buildsSince(deployment, since) {
  await deployment.settleLogs();
  return deployment
    .d1Costs(since)
    .map((cost) => cost.build)
    .filter((build) => build !== "none");
}

/**
 * Whether a request's D1 calls read the variables table: what a runtime build
 * does first (a request served by a built runtime reads stored config from
 * its snapshot, not from D1).
 */
export function readsVariables(calls) {
  return calls.some((call) =>
    call.statements.some((sql) => sql.includes('"impresspress__admin__variables"')),
  );
}

/**
 * GET `path` every `everyMs` until `done(answer)`; every answer, the last one
 * the one that satisfied it. Fails if that takes more than `withinMs`, the
 * satisfying answer included.
 */
export async function pollUntil(deployment, path, done, { withinMs, everyMs = 200 }) {
  const started = Date.now();
  const answers = [];
  for (;;) {
    const answer = await deployment.fetchCounted(path);
    answers.push(answer);
    const elapsedMs = answer.doneAt - started;
    if (elapsedMs > withinMs) {
      assert.fail(
        `GET ${path}: not done within ${withinMs} ms; last ${answer.status} ` +
          `${answer.cacheOutcome ?? ""} ${answer.body.slice(0, 200)}`,
      );
    }
    if (done(answer)) return { answer, answers, elapsedMs };
    await sleep(everyMs);
  }
}
