// U-B1 (b), IP-B addendum §4.3, end to end: a cold isolate whose packaged
// plan is stale (an admin save moved the KV config stamp after the deploy)
// serves that plan during an outage, and commits the bypass to the dynamic
// path only once a dynamic build succeeds (§4.1).
//
// 1. Request 1 goes dynamic (the stamp moved); its check of D1 fails, so it
//    builds nothing (CPU-GATE review M1: a dynamic boot cannot publish past
//    a failed read), answers 503 + Retry-After, and arms the window, leaving
//    the plan un-bypassed.
// 2. Request 2, inside the window, hydrates the plan: 200, a prepared build.
// 4. Request 3 is served with no build and no D1 call.
// 6. After the window with D1 still down: one dynamic attempt, which builds
//    nothing, the stale runtime kept (200), still not bypassed, so the request
//    after it is served too. With D1 back: the dynamic build publishes and commits the
//    bypass, and every route serves from it. A committed bypass is what lets
//    the next requests go straight to the cached dynamic runtime; an isolate
//    whose plan is not bypassed takes the prepared route, finds no current
//    prepared runtime and probes the KV config stamp on every request. So the
//    requests after the recovery read neither D1 nor KV.

import assert from "node:assert/strict";

import {
  assertCauseLogged,
  assertRefused,
  assertServed,
  assertUnavailable,
  buildsSince,
  probedBackend,
  AUTH_UI,
  QUOTA_CAUSE,
  pollUntil,
  RECOVERY_DEADLINE_MS,
} from "./common.mjs";

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
    assert.ok(probedBackend(first.d1Calls), "request 1 checks D1 for the dynamic build");
    // 2.
    const second = await d.fetchCounted("/");
    assertServed(second, "request 2 /", "wafer-site-shaped fixture");
    assert.ok(second.built, `request 2 hydrated the plan (${second.cacheOutcome})`);
    // 4.
    const third = await d.fetchCounted("/about.html");
    assertServed(third, "request 3 /about.html", "about the fixture");
    assert.equal(third.built, false, `request 3 built nothing (${third.cacheOutcome})`);
    assert.deepEqual(third.d1Calls, [], "request 3 sent D1 nothing");
    assert.deepEqual(
      await buildsSince(d, mark),
      ["gate", "prepared"],
      "request 1 assembled nothing, request 2 hydrated the plan: the isolate's one build",
    );

    // 6, D1 still down: after the window, one dynamic attempt; the stale
    //    runtime is kept and the plan stays un-bypassed.
    mark = d.mark();
    const login = await d.fetch("/b/auth/login");
    const loginRef = assertRefused(login, "the stale runtime's D1 route", AUTH_UI);
    await assertCauseLogged(d, loginRef, QUOTA_CAUSE, mark);
    const { answer: down, answers: waiting } = await pollUntil(
      d,
      "/",
      (a) => probedBackend(a.d1Calls),
      { withinMs: RECOVERY_DEADLINE_MS },
    );
    for (const a of waiting.slice(0, -1)) {
      assertServed(a, "inside the window /", "wafer-site-shaped fixture");
      assert.deepEqual(a.d1Calls, [], "inside the window nothing is built");
    }
    assertServed(down, "the failed post-window attempt /", "wafer-site-shaped fixture");
    assert.equal(down.built, false, `the failed attempt built nothing (${down.cacheOutcome})`);
    const afterDown = await d.fetchCounted("/");
    assertServed(afterDown, "after the failed attempt /", "wafer-site-shaped fixture");
    assert.deepEqual(afterDown.d1Calls, [], "after the failed attempt nothing is built");

    // 6, D1 back: the next post-window build is dynamic and publishes.
    await d.setMode("ok");
    mark = d.mark();
    const { answer: up } = await pollUntil(d, "/", (a) => a.built, {
      withinMs: RECOVERY_DEADLINE_MS,
    });
    assertServed(up, "the recovering build /", "wafer-site-shaped fixture");
    assert.deepEqual(await buildsSince(d, mark), ["dynamic"], "the recovery is the dynamic build");
    assertServed(await d.fetch("/b/auth/login"), "after recovery /b/auth/login");
    for (let i = 0; i < 3; i++) {
      const hit = await d.fetchCounted("/");
      assertServed(hit, `after recovery / (${i})`);
      assert.equal(hit.cacheOutcome, "hit", "the dynamic runtime is served from the cache");
      assert.deepEqual(hit.d1Calls, [], "after recovery / reads no D1");
      assert.deepEqual(hit.kvCalls, [], "the plan is bypassed: no KV config-stamp probe");
    }
  },
};
