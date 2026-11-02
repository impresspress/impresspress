// D1 outage plan rev 3.1 §6, Miniflare prepared-path steps 1–4 and 6, on a
// wafer-site-shaped deployment:
//
// 1. deploy healthy;
// 2. toggle the outage on;
// 3. on a cold isolate, the site's pages and `/b/static/` serve 200 and the
//    D1 route answers 503 + Retry-After with a readable body;
// 4. 50 requests paced over several windows: at most one runtime-build attempt per retry window,
//    each the failed variables read plus its `EXISTS` probe (two D1 calls,
//    plan note 2026-11-01); a request that builds nothing sends D1 at most its
//    audit row;
// 6. toggle off: the D1 route recovers within the window it was given.
//
// Step 5 (a moved KV stamp under a warm prepared runtime, `/health` stale) and
// every `/health` assertion belong to IP-C's `/health`.

import assert from "node:assert/strict";

import { sleep } from "../harness.mjs";

import { assertServed, assertUnavailable, attemptedBuild, pollUntil, staticAssetPath } from "./common.mjs";

const BURST = 50;
/** Pacing, so the burst spans several retry windows (1 s, then 2 s, ...). */
const BURST_GAP_MS = 120;

export default {
  name: "prepared-wafer-site",
  shape: "wafer-site",
  async run(d) {
    // 1. Deploy, healthy: `/_deploy/prepare` answered 200 (`deploy()` checks),
    //    and the final version serves from its prepared plan.
    await d.deploy();
    const mark = d.mark();
    assertServed(await d.fetch("/"), "healthy /", "wafer-site-shaped fixture");
    await d.settleLogs();
    assert.deepEqual(
      d.d1Costs(mark).map((c) => c.build),
      ["prepared"],
      "the final version's first request builds from the packaged plan",
    );

    // 2–3. Outage, cold isolate.
    await d.restart({ mode: "quota" });
    const home = await d.fetch("/");
    assertServed(home, "outage /", "wafer-site-shaped fixture");
    assert.ok(home.built, `outage /: the cold isolate built (${home.cacheOutcome})`);
    assertServed(await d.fetch("/about.html"), "outage /about.html", "about the fixture");
    assertServed(await d.fetch(staticAssetPath(d)), "outage /b/static/");
    const login = await d.fetch("/b/auth/login");
    assertUnavailable(login, "outage /b/auth/login");
    assert.equal(JSON.parse(login.body).error, "Unavailable", "outage /b/auth/login: error code");

    // 4. The burst.
    const started = Date.now();
    const answers = [];
    for (let i = 0; i < BURST; i++) {
      answers.push(await d.fetchCounted(i % 2 === 0 ? "/" : "/b/auth/login"));
      await sleep(BURST_GAP_MS);
    }
    const spanS = (Date.now() - started) / 1000;
    for (const [i, a] of answers.entries()) {
      if (i % 2 === 0) assertServed(a, `burst ${i} /`);
      else assertUnavailable(a, `burst ${i} /b/auth/login`);
    }
    const attempts = answers.filter((a) => attemptedBuild(a.d1Calls));
    // The window starts at 1 s and doubles after each failed attempt, so k
    // attempts after the cold one need at least 2^(k-1) - 1 s between them.
    const bound = 1 + Math.floor(Math.log2(spanS + 1)) + 1;
    assert.ok(
      attempts.length <= bound,
      `${attempts.length} build attempts in ${spanS.toFixed(1)} s; at most ${bound} fit the retry windows`,
    );
    assert.ok(attempts.length >= 1, `the burst spans a window end (${spanS.toFixed(1)} s)`);
    for (const a of attempts) {
      const reads = a.d1Calls.filter((c) => !c.statements[0].startsWith("SELECT CASE WHEN NOT EXISTS"));
      assert.equal(reads.length, 2, `a build attempt sends the variables read and its probe: ${JSON.stringify(a.d1Calls)}`);
    }
    for (const a of answers.filter((a) => !attemptedBuild(a.d1Calls))) {
      assert.ok(a.d1Calls.length <= 1, `a request that built nothing sent ${JSON.stringify(a.d1Calls)}`);
    }
    console.log(
      `     burst: ${BURST} requests in ${spanS.toFixed(1)} s, ${attempts.length} build attempt(s), ` +
        `${answers.reduce((n, a) => n + a.d1Calls.length, 0)} D1 calls in all`,
    );

    // 6. Recovery within the window the last 503 named.
    const last = await d.fetch("/b/auth/login");
    assertUnavailable(last, "before recovery /b/auth/login");
    await d.setMode("ok");
    const { answer, elapsedMs } = await pollUntil(d, "/b/auth/login", (a) => a.status === 200, {
      withinMs: last.retryAfter * 1000 + 1500,
    });
    assert.ok(answer.built, `the recovering request rebuilt (${answer.cacheOutcome})`);
    console.log(`     recovered in ${(elapsedMs / 1000).toFixed(1)} s (Retry-After was ${last.retryAfter} s)`);
    const after = await d.fetchCounted("/b/auth/login");
    assertServed(after, "after recovery /b/auth/login");
    assert.equal(after.cacheOutcome, "hit", "after recovery the runtime is served from the cache");
  },
};
