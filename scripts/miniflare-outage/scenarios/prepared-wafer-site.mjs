// D1 outage plan rev 3.1 §6, Miniflare prepared-path steps 1–4 and 6, on a
// wafer-site-shaped deployment:
//
// 1. deploy healthy;
// 2. toggle the outage on;
// 3. on a cold isolate, the site's pages and `/b/static/` serve 200 and the
//    D1 route answers 503 + Retry-After: wafer-run's fixed refusal text and a
//    reference, the cause (D1's text) only in the Worker's log under it;
// 4. 50 requests paced over several windows: at most one runtime-build attempt per retry window,
//    each the failed variables read plus its `EXISTS` probe (two D1 calls,
//    plan note 2026-11-01); a request that builds nothing sends D1 at most its
//    audit row;
// 6. toggle off: the D1 route recovers by the first rebuild after the
//    window, and nothing is rebuilt inside it. The window ends no later than
//    the last refusal's answer plus its `Retry-After` (the ceiling of what
//    was left), so no request sent after that instant may be answered from
//    inside it.
//
// Step 5 (a moved KV stamp under a warm prepared runtime, `/health` stale) and
// every `/health` assertion belong to IP-C's `/health`.

import assert from "node:assert/strict";

import { sleep } from "../harness.mjs";

import {
  assertCauseLogged,
  assertRefused,
  assertServed,
  attemptedBuild,
  AUTH_UI,
  QUOTA_CAUSE,
  pollUntil,
  readsVariables,
  RECOVERY_DEADLINE_MS,
  staticAssetPath,
} from "./common.mjs";

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
    let mark = d.mark();
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
    mark = d.mark();
    const login = await d.fetch("/b/auth/login");
    const loginRef = assertRefused(login, "outage /b/auth/login", AUTH_UI);
    await assertCauseLogged(d, loginRef, QUOTA_CAUSE, mark);

    // 4. The burst.
    mark = d.mark();
    const started = Date.now();
    const answers = [];
    for (let i = 0; i < BURST; i++) {
      answers.push(await d.fetchCounted(i % 2 === 0 ? "/" : "/b/auth/login"));
      await sleep(BURST_GAP_MS);
    }
    const spanS = (Date.now() - started) / 1000;
    const refs = [];
    for (const [i, a] of answers.entries()) {
      if (i % 2 === 0) assertServed(a, `burst ${i} /`);
      else refs.push(assertRefused(a, `burst ${i} /b/auth/login`, AUTH_UI));
    }
    assert.equal(new Set(refs).size, refs.length, "every refusal has a reference of its own");
    for (const ref of refs) await assertCauseLogged(d, ref, QUOTA_CAUSE, mark);
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

    // 6. Recovery. D1 is back, so any build would succeed: the requests
    //    before the recovering one are inside the window, answer 503 and
    //    build nothing; the recovering one is the rebuild. The deadline only
    //    stops a hung run; it is not what is asserted.
    mark = d.mark();
    const last = await d.fetch("/b/auth/login");
    await assertCauseLogged(d, assertRefused(last, "before recovery /b/auth/login", AUTH_UI), QUOTA_CAUSE, mark);
    const windowEndsBy = last.doneAt + last.retryAfter * 1000;
    await d.setMode("ok");
    const { answer, answers: waiting, elapsedMs } = await pollUntil(
      d,
      "/b/auth/login",
      (a) => a.status === 200,
      { withinMs: RECOVERY_DEADLINE_MS },
    );
    for (const [i, a] of waiting.slice(0, -1).entries()) {
      assertRefused(a, `inside the window, request ${i}`, AUTH_UI);
      assert.ok(!readsVariables(a.d1Calls), `inside the window, request ${i} built: ${JSON.stringify(a.d1Calls)}`);
      assert.ok(
        a.sentAt <= windowEndsBy,
        `request ${i} was sent ${a.sentAt - windowEndsBy} ms after the window had ended, and still was not rebuilt`,
      );
    }
    assert.ok(answer.built, `the recovering request rebuilt (${answer.cacheOutcome})`);
    assert.ok(readsVariables(answer.d1Calls), "the rebuild read the variables table");
    console.log(
      `     recovered in ${(elapsedMs / 1000).toFixed(1)} s, after ${waiting.length - 1} ` +
        `request(s) inside the window (Retry-After was ${last.retryAfter} s)`,
    );
    const after = await d.fetchCounted("/b/auth/login");
    assertServed(after, "after recovery /b/auth/login");
    assert.equal(after.cacheOutcome, "hit", "after recovery the runtime is served from the cache");
  },
};
