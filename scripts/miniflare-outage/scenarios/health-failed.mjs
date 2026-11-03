// `/health` reports `failed` (IP-C; D1 outage plan rev 3.1 §6 step 7) on a
// wafer-site-shaped deployment: a warm runtime degraded by a D1 outage, whose
// rebuild after the window then fails permanently (every statement `no such
// table`).
//
// 1. The rebuild's own request answers the edge's opaque 500, its cause only
//    in the log under the reference it names.
// 2. The next `/health`, inside the `Fault` window, is served by the runtime
//    the failed rebuild left cached: 503 `failed`, `since` the episode's
//    start (the degraded cold build), `rebuilds` 2, the degraded blocks
//    still listed, and no D1 call. The site's pages keep serving from it.
// 3. A cold isolate inside a `Fault` window has no runtime to report from:
//    its `/health` is the edge's opaque 500, as every route is (documented
//    in PR277-review's state table; the site gate counts a 500 a rollback).

import assert from "node:assert/strict";

import { assertHealth, assertNoCause, assertServed, attemptedBuild, pollUntil, RECOVERY_DEADLINE_MS } from "./common.mjs";

/** The edge's opaque 500, its reference, and the cause logged under it. */
async function assertOpaque500(d, answer, what, since) {
  assert.equal(answer.status, 500, `${what}: status (${answer.body.slice(0, 200)})`);
  const reference = /^internal server error \(reference: ([0-9a-f-]{36})\)$/.exec(answer.body)?.[1];
  assert.ok(reference, `${what}: the opaque body names a reference: ${answer.body}`);
  assertNoCause(answer.body, what);
  return d.logLine((l) => l.includes(`[${reference}]`), since);
}

export default {
  name: "health-failed",
  shape: "wafer-site",
  async run(d) {
    await d.deploy();
    await d.restart({ mode: "quota" });
    const cold = await d.fetchCounted("/health");
    const degraded = assertHealth(cold, "outage cold /health", "degraded");

    // 1.
    await d.setMode("no-such-table");
    const mark = d.mark();
    const { answer: rebuild } = await pollUntil(d, "/health", (a) => attemptedBuild(a.d1Calls), {
      withinMs: RECOVERY_DEADLINE_MS,
    });
    const line = await assertOpaque500(d, rebuild, "the failed rebuild's /health", mark);
    assert.ok(line.includes("no such table"), `the log under the reference names the cause: ${line}`);

    // 2.
    const next = await d.fetchCounted("/health");
    const failed = assertHealth(next, "the next /health", "failed");
    assert.equal(failed.since, degraded.since, "failed since the episode's start");
    assert.equal(failed.rebuilds, 2, "the failed rebuild is one more in the episode");
    assert.deepEqual(
      failed.unavailable.map((b) => b.block),
      degraded.unavailable.map((b) => b.block),
      "the cached runtime's degraded blocks are still listed",
    );
    assert.deepEqual(next.d1Calls, [], "the next /health sent D1 nothing");
    assertServed(await d.fetch("/"), "the cached runtime serves /", "wafer-site-shaped fixture");

    // 3.
    await d.restart({ mode: "no-such-table" });
    const coldMark = d.mark();
    const coldFault = await d.fetchCounted("/");
    await assertOpaque500(d, coldFault, "a cold isolate's first request", coldMark);
    const healthMark = d.mark();
    const coldHealth = await d.fetchCounted("/health");
    await assertOpaque500(d, coldHealth, "a cold isolate's /health inside the Fault window", healthMark);
    assert.deepEqual(coldHealth.d1Calls, [], "inside the Fault window nothing is built");
  },
};
