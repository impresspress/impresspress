// What several scenarios assert, in one place.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { sleep } from "../harness.mjs";

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

/** An outage answer: 503, a `Retry-After` of at least a second, a body a person can read. */
export function assertUnavailable(answer, what) {
  assert.equal(answer.status, 503, `${what}: status (body: ${answer.body.slice(0, 200)})`);
  assert.ok(answer.retryAfter >= 1, `${what}: Retry-After is ${answer.retryAfter}`);
  assert.ok(answer.body.trim().length > 0, `${what}: an empty body`);
}

/** 200, and the body is the page asked for. */
export function assertServed(answer, what, contains) {
  assert.equal(answer.status, 200, `${what}: status (body: ${answer.body.slice(0, 200)})`);
  if (contains !== undefined) {
    assert.ok(answer.body.includes(contains), `${what}: body lacks ${JSON.stringify(contains)}`);
  }
}

/**
 * Whether a request's D1 calls include a runtime-build attempt. A build over
 * a failing D1 reads the variables table (its first touch introspects it),
 * and after the failed read probes whether the table exists, to tell a fresh
 * database from an outage (`impresspress-core`'s variables snapshot,
 * `read_unfiltered`): that probe is the build's mark.
 */
export function attemptedBuild(calls) {
  return calls.some((call) =>
    call.statements.some((sql) => sql.startsWith("SELECT EXISTS(SELECT 1 FROM sqlite_master")),
  );
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
