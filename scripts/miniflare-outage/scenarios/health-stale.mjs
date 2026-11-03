// `/health` reports `stale` (IP-C; D1 outage plan rev 3.1 §6 step 5) on a
// wafer-site-shaped deployment: a warm runtime degraded by a D1 outage, then
// an admin save moves the KV config stamp. The rebuild after the window sees
// the moved stamp, hands the plan to the dynamic path, fails on D1 and keeps
// the runtime it was replacing, now serving config older than the
// database's:
//
// - that rebuild's own `/health` is 503 `stale`, `since` the failed rebuild
//   (inside its request), the episode's `rebuilds` one more, and the blocks
//   the kept runtime was published without still listed (stale outranks
//   degraded, and keeps the list);
// - the next `/health`, inside the new window, the same report with no D1,
//   KV or build;
// - the site's pages keep serving from the kept runtime.

import assert from "node:assert/strict";

import { assertHealth, assertServed, attemptedBuild, pollUntil, RECOVERY_DEADLINE_MS } from "./common.mjs";

export default {
  name: "health-stale",
  shape: "wafer-site",
  async run(d) {
    await d.deploy();
    await d.restart({ mode: "quota" });
    const cold = await d.fetchCounted("/health");
    const degraded = assertHealth(cold, "outage cold /health", "degraded");
    assert.ok(cold.built, `the cold /health built (${cold.cacheOutcome})`);

    await d.bumpConfigStamp();
    // The first request after the window is the rebuild: it probes the
    // stamp, finds it moved, and tries the dynamic build, which fails on D1.
    const { answer: rebuild, answers } = await pollUntil(d, "/health", (a) => attemptedBuild(a.d1Calls), {
      withinMs: RECOVERY_DEADLINE_MS,
    });
    for (const [i, a] of answers.slice(0, -1).entries()) {
      assertHealth(a, `inside the window, /health ${i}`, "degraded");
      assert.deepEqual(a.d1Calls, [], `inside the window, /health ${i} sent D1 nothing`);
    }
    const stale = assertHealth(rebuild, "the failed rebuild's /health", "stale");
    const since = Date.parse(stale.since);
    assert.ok(
      since >= rebuild.sentAt && since <= rebuild.doneAt,
      `stale since the failed rebuild: ${stale.since} is outside its request`,
    );
    assert.equal(stale.rebuilds, 2, "the failed rebuild is one more in the episode");
    assert.deepEqual(
      stale.unavailable,
      degraded.unavailable.map((b) => ({ ...b, retry_in: stale.retry_in })),
      "the kept runtime's degraded blocks are still listed, their runs as they were",
    );

    const next = await d.fetchCounted("/health");
    assert.deepEqual(
      assertHealth(next, "the next /health", "stale"),
      { ...stale, retry_in: next.retryAfter, unavailable: stale.unavailable.map((b) => ({ ...b, retry_in: next.retryAfter })) },
      "the next /health: the same stale report",
    );
    assert.equal(next.built, false, `the next /health built nothing (${next.cacheOutcome})`);
    assert.deepEqual(next.d1Calls, [], "the next /health sent D1 nothing");
    assert.deepEqual(next.kvCalls, [], "the next /health sent KV nothing");
    assertServed(await d.fetch("/"), "the kept runtime serves /", "wafer-site-shaped fixture");
  },
};
