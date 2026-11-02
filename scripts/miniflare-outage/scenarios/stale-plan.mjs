// U-B1 (b), IP-B addendum §4.3, end to end: a cold isolate whose packaged
// plan is stale (an admin save moved the KV config stamp after the deploy)
// serves that plan during an outage, and commits the bypass to the dynamic
// path only once a dynamic build succeeds (§4.1).
//
// 1. Request 1 tries the dynamic build (the stamp moved), fails, answers 503 +
//    Retry-After, and arms the window, leaving the plan un-bypassed.
// 2. Request 2, inside the window, hydrates the plan: 200, a prepared build.
// 4. Request 3 is served with no build and no D1 call.
// 6. After the window with D1 still down: one dynamic attempt, the stale
//    runtime kept (200), still not bypassed, so the request after it is
//    served too. With D1 back: the dynamic build publishes (the bypass), and
//    every route serves from it.

import assert from "node:assert/strict";

import { assertServed, assertUnavailable, attemptedBuild, pollUntil, WINDOW_CAP_MS } from "./common.mjs";

export default {
  name: "stale-plan",
  shape: "wafer-site",
  async run(d) {
    await d.deploy();
    await d.bumpConfigStamp();
    await d.restart({ mode: "quota" });

    // 1.
    let mark = d.mark();
    const first = await d.fetchCounted("/");
    assertUnavailable(first, "request 1 /");
    assert.ok(attemptedBuild(first.d1Calls), "request 1 attempts the dynamic build");
    // 2.
    const second = await d.fetchCounted("/");
    assertServed(second, "request 2 /", "wafer-site-shaped fixture");
    assert.ok(second.built, `request 2 hydrated the plan (${second.cacheOutcome})`);
    // 4.
    const third = await d.fetchCounted("/about.html");
    assertServed(third, "request 3 /about.html", "about the fixture");
    assert.equal(third.built, false, `request 3 built nothing (${third.cacheOutcome})`);
    assert.deepEqual(third.d1Calls, [], "request 3 sent D1 nothing");
    await d.settleLogs();
    assert.deepEqual(
      d.d1Costs(mark).map((c) => c.build),
      ["dynamic", "prepared"],
      "request 1 tried the dynamic build, request 2 hydrated the plan",
    );

    // 6, D1 still down: after the window, one dynamic attempt; the stale
    //    runtime is kept and the plan stays un-bypassed.
    const login = await d.fetch("/b/auth/login");
    assertUnavailable(login, "the stale runtime's D1 route");
    const { answer: down } = await pollUntil(d, "/", (a) => attemptedBuild(a.d1Calls), {
      withinMs: login.retryAfter * 1000 + 1500,
    });
    assertServed(down, "the failed post-window attempt /", "wafer-site-shaped fixture");
    const afterDown = await d.fetchCounted("/");
    assertServed(afterDown, "after the failed attempt /", "wafer-site-shaped fixture");
    assert.deepEqual(afterDown.d1Calls, [], "after the failed attempt nothing is built");

    // 6, D1 back: the next post-window build is dynamic and publishes.
    await d.setMode("ok");
    mark = d.mark();
    const { answer: up } = await pollUntil(d, "/", (a) => a.built, { withinMs: WINDOW_CAP_MS });
    assertServed(up, "the recovering build /", "wafer-site-shaped fixture");
    await d.settleLogs();
    assert.deepEqual(
      d.d1Costs(mark).filter((c) => c.build !== "none").map((c) => c.build),
      ["dynamic"],
      "the recovery is the dynamic build",
    );
    assertServed(await d.fetch("/b/auth/login"), "after recovery /b/auth/login");
    const hit = await d.fetchCounted("/");
    assertServed(hit, "after recovery /");
    assert.equal(hit.cacheOutcome, "hit", "the dynamic runtime is served from the cache");
  },
};
