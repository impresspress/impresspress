// What several scenarios assert, in one place.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { sleep } from "../harness.mjs";

/** The isolate's retry window doubles from 1 s (`RETRY_WINDOW_BASE_MS`) to 60 s. */
export const WINDOW_CAP_MS = 60_000;

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
 * GET `path` every `everyMs` until `done(answer)` or `withinMs` has passed;
 * the answer that satisfied it, and when.
 */
export async function pollUntil(deployment, path, done, { withinMs, everyMs = 200 }) {
  const started = Date.now();
  for (;;) {
    const answer = await deployment.fetchCounted(path);
    const elapsedMs = Date.now() - started;
    if (done(answer)) return { answer, elapsedMs };
    if (elapsedMs > withinMs) {
      assert.fail(
        `GET ${path}: not done within ${withinMs} ms; last ${answer.status} ` +
          `${answer.cacheOutcome ?? ""} ${answer.body.slice(0, 200)}`,
      );
    }
    await sleep(everyMs);
  }
}
