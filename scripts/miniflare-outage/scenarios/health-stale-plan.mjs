// `/health` on a cold isolate whose packaged plan is stale (U-B1 (b), IP-B
// addendum §4.3), on a wafer-site-shaped deployment: an admin save moved the
// KV config stamp after the deploy, then D1 went down.
//
// 1. `/health` as request 1 goes dynamic (the stamp moved), fails, and arms
//    the window with nothing cached: the edge's own plain-text 503, not a
//    report — there is no runtime to run the handler. The site gate's
//    reading of `/_health` depends on this shape (PR277-review S1 item 3).
// 2. Request 2 hydrates the plan and serves it: 503 `stale` since the window
//    was armed (inside request 1), listing the blocks the hydrate was
//    published without.
// 3. D1 comes back inside the window: still `stale`, with no build, until the
//    window ends.
// 4. The first request after the window builds dynamically from D1: 200
//    `{"status":"ok"}`. No request sent after the window must have ended is
//    answered from it.

import assert from "node:assert/strict";

import {
  assertHealth,
  assertUnavailable,
  attemptedBuild,
  pollUntil,
  readsVariables,
  RECOVERY_DEADLINE_MS,
  WINDOW_BASE_MS,
} from "./common.mjs";

export default {
  name: "health-stale-plan",
  shape: "wafer-site",
  async run(d) {
    await d.deploy();
    await d.bumpConfigStamp();
    await d.restart({ mode: "quota" });

    // 1.
    const first = await d.fetchCounted("/health");
    assertUnavailable(first, "request 1 /health");
    assert.ok(attemptedBuild(first.d1Calls), "request 1 attempts the dynamic build");
    assert.match(
      first.body,
      /^Service temporarily unavailable: .* \(reference: [0-9a-f-]{36}\)$/,
      "request 1: the edge's plain-text 503, no runtime to report",
    );
    assert.equal(first.headers.get("cache-control"), "no-store", "request 1: Cache-Control");

    // 2.
    const second = await d.fetchCounted("/health");
    const stale = assertHealth(second, "request 2 /health", "stale");
    assert.ok(second.built, `request 2 hydrated the plan (${second.cacheOutcome})`);
    const since = Date.parse(stale.since);
    assert.ok(
      since >= first.sentAt && since <= first.doneAt,
      `stale since the window request 1 armed: ${stale.since} is outside request 1`,
    );
    assert.ok(stale.unavailable.length > 0, "the hydrate's degraded blocks are listed");
    for (const block of stale.unavailable) assert.equal(block.class, "retrying", `${block.block}: class`);

    // 3. and 4. The window ends no later than request 2's answer plus its
    //    `Retry-After`.
    const endsBy = second.doneAt + second.retryAfter * 1000;
    await d.setMode("ok");
    const { answer: recovered, answers } = await pollUntil(d, "/health", (a) => a.built, {
      withinMs: RECOVERY_DEADLINE_MS,
    });
    // The window was armed inside request 1 and lasts at least the base
    // delay, so an answer before `first.sentAt + WINDOW_BASE_MS` is inside it.
    const surelyInside = answers.filter((a) => a.doneAt < first.sentAt + WINDOW_BASE_MS);
    assert.ok(surelyInside.length >= 1, "a /health was answered inside the window with D1 back");
    for (const [i, a] of answers.slice(0, -1).entries()) {
      const body = assertHealth(a, `D1 back, inside the window, /health ${i}`, "stale");
      assert.equal(body.since, stale.since, `D1 back, /health ${i}: stale since the same instant`);
      assert.deepEqual(a.d1Calls, [], `D1 back, /health ${i}: sent D1 nothing`);
      assert.ok(a.sentAt <= endsBy, `D1 back: /health ${i} was sent after the window ended, and not rebuilt`);
    }
    assertHealth(recovered, "the first /health after the window", "ok");
    assert.ok(readsVariables(recovered.d1Calls), "the recovery built from D1");
    assertHealth(await d.fetch("/health"), "after recovery /health", "ok");
  },
};
