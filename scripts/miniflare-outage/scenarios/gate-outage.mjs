// The D1 gate over a long outage (CPU-GATE-PLAN-review, acceptance 9):
// an impresspress-site deployment, its final version, D1 failing every call
// (the 2026-10-30 quota text) for about 70 s across the live check's routes.
// Miniflare neither enforces nor reports CPU, so what is asserted is what
// the CPU follows: how many runtimes are assembled.
//
// 1. The cold isolate builds once (`d1_cost build=prepared`): its gate fails,
//    and it publishes degraded anyway (Decision 1).
// 2. Every retry after a window is the gate alone: `build=gate`, no runtime
//    assembled, the request served the cached runtime (`gated`). Each arms
//    the next window, so the "runtime retry window armed" lines count
//    1, 2, 3, … with one gate per window after the first and none inside one.
//    Before the gate each of these was a full build (~320 ms of CPU on
//    Cloudflare), which is what got the 2026-11-03 live check's isolates
//    throttled.
// 3. Throughout, the static site file and `/b/static/` answer 200, `/` and
//    the D1 route 503 with `Retry-After`, `/health` 503 `degraded` with
//    `retries` counting the windows and `backend` `failing`, its run begun
//    by the cold build's failed check.
// 4. The wall time of the cold build and of the slowest request after it is
//    reported. It is not asserted: under Miniflare a cold build can take
//    tens of milliseconds, so wall time cannot tell it from a gate; the
//    counts are the assertion, and `d1_read_cost` pins the CPU ratio.
// 5. D1 back: the next retry passes the gate and is the one more build;
//    `/health` 200.
// 6. The live check's own method, `IMPRESSPRESS_D1_CALLS_PER_INVOCATION=4`
//    on the final version, is not a lasting outage any more. Three of the
//    four calls are reserved, so each invocation may send one. Each failed
//    gate leaves one more introspection of the variables table memoized
//    (schema facts survive until a runtime is stored), until the gate's read
//    is the one list and fits. A prepared boot over the read it was handed
//    sends nothing else, so the isolate rebuilds healthy: one more build,
//    then `/health` 200, with no window armed after it.

import assert from "node:assert/strict";

import { sleep } from "../harness.mjs";

import {
  assertHealth,
  assertServed,
  assertUnavailable,
  buildsSince,
  pollUntil,
  probedBackend,
  RECOVERY_DEADLINE_MS,
  staticAssetPath,
} from "./common.mjs";

const VAR = "IMPRESSPRESS_D1_CALLS_PER_INVOCATION";
/** The live check's value (`budget-rehearsal.mjs`'s `LIVE_CHECK_VALUE`). */
const LIVE_CHECK_VALUE = "4";
/** How long the outage is driven: windows of 1, 2, 4, 8, 16 and 32 s end inside it. */
const DRIVE_MS = 70_000;
/** Between requests: about 45 over the drive. */
const GAP_MS = 1_500;

/** The `failures=` of every "runtime retry window armed" line since `since`. */
function armedSince(d, since) {
  return d.logs
    .slice(since)
    .filter((l) => l.includes("runtime retry window armed"))
    .map((l) => Number(/failures=(\d+)/.exec(l)[1]));
}

export default {
  name: "gate-outage",
  shape: "impresspress-site",
  async run(d) {
    await d.deploy();
    const asset = staticAssetPath(d);
    const paths = ["/hello.html", asset, "/b/auth/login", "/health", "/"];

    // 1.
    await d.restart({ mode: "quota" });
    let mark = d.mark();
    const cold = await d.fetchCounted("/");
    assertUnavailable(cold, "cold /");
    assert.ok(cold.built, `the cold isolate built (${cold.cacheOutcome})`);
    const coldWallMs = cold.doneAt - cold.sentAt;

    // 2–3.
    const answers = [];
    for (let i = 0; Date.now() - cold.sentAt < DRIVE_MS; i++) {
      await sleep(GAP_MS);
      const path = paths[i % paths.length];
      const answer = await d.fetchCounted(path);
      answer.path = path;
      answers.push(answer);
      const what = `${path} at +${((answer.sentAt - cold.sentAt) / 1000).toFixed(1)} s`;
      if (path === "/hello.html") assertServed(answer, what, "a static site file");
      else if (path === asset) assertServed(answer, what);
      else if (path === "/health") {
        const body = assertHealth(answer, what, "degraded");
        assert.equal(body.backend?.status, "failing", `${what}: backend (${answer.body.slice(0, 300)})`);
        const since = Date.parse(body.backend.since);
        assert.ok(
          since >= cold.sentAt && since <= cold.doneAt,
          `${what}: backend.since ${body.backend.since} is the cold build's failed check, inside its request`,
        );
        assert.ok(Date.parse(body.backend.checked) >= since, `${what}: backend.checked is not before since`);
      }
      else assertUnavailable(answer, what);
      assert.equal(answer.built, false, `${what}: built nothing (${answer.cacheOutcome})`);
    }
    const gated = answers.filter((a) => probedBackend(a.d1Calls));
    for (const a of gated) {
      assert.equal(a.cacheOutcome, "gated", `${a.path}: the retry reports its check`);
    }
    assert.deepEqual(
      await buildsSince(d, mark),
      ["prepared", ...gated.map(() => "gate")],
      "one runtime assembled, by the cold isolate; every retry after it a gate check",
    );
    assert.ok(gated.length >= 4, `only ${gated.length} retries in ${DRIVE_MS / 1000} s of windows`);
    const armed = armedSince(d, mark);
    assert.deepEqual(
      armed,
      armed.map((_, i) => i + 1),
      "each window is armed once, one more than the last: no retry inside a window",
    );
    assert.equal(armed.length, 1 + gated.length, "the cold build's window, then one per gate");
    const retries = answers.filter((a) => a.path === "/health").map((a) => JSON.parse(a.body).retries);
    assert.deepEqual(retries, [...retries].sort((a, b) => a - b), `/health's retries only grow: ${retries}`);

    // 4. Reported, not asserted: Miniflare's wall time does not tell a build
    //    from a gate (a cold build measured 44 ms on a fast machine), so the
    //    counts above are the assertion, and the CPU ratio is pinned natively
    //    (`d1_read_cost`'s `a_failed_gate_costs_a_sliver_of_a_cold_build`).
    const slowest = Math.max(...answers.map((a) => a.doneAt - a.sentAt));
    console.log(
      `     ${answers.length + 1} requests over ${DRIVE_MS / 1000} s: 1 build, ${gated.length} gate checks ` +
        `(windows armed ${armed.join(", ")}); cold ${coldWallMs} ms, slowest after it ${slowest} ms`,
    );

    // 5.
    await d.setMode("ok");
    mark = d.mark();
    const { answer: recovered } = await pollUntil(d, "/health", (a) => a.built, {
      withinMs: RECOVERY_DEADLINE_MS,
    });
    assertHealth(recovered, "the recovering /health", "ok");
    assert.deepEqual(await buildsSince(d, mark), ["prepared"], "the recovery is one build");

    // 6.
    await d.restart({ vars: { [VAR]: LIVE_CHECK_VALUE } });
    mark = d.mark();
    assertUnavailable(await d.fetch("/"), "budget 4, cold /");
    const healed = await pollUntil(d, "/health", (a) => a.status === 200, {
      withinMs: RECOVERY_DEADLINE_MS,
    });
    assertHealth(healed.answer, "budget 4, the rebuilt /health", "ok");
    const budgetBuilds = await buildsSince(d, mark);
    assert.equal(budgetBuilds[0], "prepared", `budget 4: the cold build first (${budgetBuilds})`);
    assert.equal(budgetBuilds.at(-1), "prepared", `budget 4: the healthy rebuild last (${budgetBuilds})`);
    assert.deepEqual(
      budgetBuilds.slice(1, -1),
      budgetBuilds.slice(1, -1).map(() => "gate"),
      `budget 4: only gate checks between the two builds (${budgetBuilds})`,
    );
    const budgetArmed = armedSince(d, mark);
    assert.equal(budgetArmed.length, budgetBuilds.length - 1, "budget 4: the healthy rebuild armed nothing");
    assertServed(await d.fetch("/hello.html"), "budget 4, after the rebuild", "a static site file");
    console.log(
      `     budget ${LIVE_CHECK_VALUE}: builds ${budgetBuilds.join(", ")}, then /health 200 ` +
        `after ${((healed.answer.doneAt - healed.answers[0].sentAt) / 1000).toFixed(1)} s`,
    );
  },
};
