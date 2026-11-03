// `/health` for a block whose own `Init` makes D1 reset (D1 outage plan I2g;
// IP-C's `RetryStreaks`), on the `wafer-site-init-reader` shape: wafer-site
// plus `miniflare/init-reader`, whose `Init` sends D1 one statement of its
// own (`fixture/src/lib.rs`). The D1 wrapper fails that statement alone with
// D1's CPU-limit reset text, which the platform classifies as transient; D1
// answers everything else.
//
// 1. A cold isolate publishes without that block only: `/health` is 503
//    `degraded` listing exactly it, `retrying`, `runtimes` 1. The rest of the
//    site serves, the stored-config route included.
// 2. Each rebuild after a window fails the block again: its run's `since`
//    stays put and `runtimes` grows, so the failure reads as persistent
//    rather than as a blip.

import assert from "node:assert/strict";

import { sleep } from "../harness.mjs";

import { assertHealth, assertServed, RECOVERY_DEADLINE_MS } from "./common.mjs";

const BLOCK = "miniflare/init-reader";

/** `/health` every 100 ms until a request rebuilds; that answer. */
async function nextRebuild(d) {
  const started = Date.now();
  for (;;) {
    const answer = await d.fetchCounted("/health");
    if (answer.built) return answer;
    assertHealth(answer, "inside the window /health", "degraded");
    assert.deepEqual(answer.d1Calls, [], "inside the window /health sends D1 nothing");
    assert.ok(Date.now() - started < RECOVERY_DEADLINE_MS, "a rebuild after the window, in time");
    await sleep(100);
  }
}

/** The one listed block of a `/health` that lists only `BLOCK`. */
function onlyBlock(answer, what) {
  const body = assertHealth(answer, what, "degraded");
  assert.deepEqual(body.unavailable.map((b) => b.block), [BLOCK], `${what}: only ${BLOCK} is listed`);
  const [block] = body.unavailable;
  assert.equal(block.class, "retrying", `${what}: class`);
  return { body, block };
}

export default {
  name: "health-block-init",
  shape: "wafer-site-init-reader",
  async run(d) {
    await d.deploy();
    assertHealth(await d.fetch("/health"), "healthy /health", "ok");

    // 1.
    await d.restart({ mode: "init-reset" });
    const cold = await d.fetchCounted("/health");
    assert.ok(cold.built, `the cold /health built (${cold.cacheOutcome})`);
    assert.ok(
      cold.d1Calls.some((c) => c.refused),
      `the block's Init statement was the one refused: ${JSON.stringify(cold.d1Calls)}`,
    );
    const first = onlyBlock(cold, "cold /health");
    assert.equal(first.block.runtimes, 1, "cold: runtimes");
    assert.equal(first.body.rebuilds, 1, "cold: rebuilds");
    assertServed(await d.fetch("/"), "the site serves /", "wafer-site-shaped fixture");
    assertServed(await d.fetch("/b/auth/login"), "the stored-config route serves");

    // 2.
    for (const runtimes of [2, 3]) {
      const { body, block } = onlyBlock(await nextRebuild(d), `rebuild ${runtimes} /health`);
      assert.equal(block.since, first.block.since, `rebuild ${runtimes}: the run began where it did`);
      assert.equal(block.runtimes, runtimes, `rebuild ${runtimes}: the run spans every runtime`);
      assert.equal(body.since, first.body.since, `rebuild ${runtimes}: the episode began where it did`);
      assert.equal(body.rebuilds, runtimes, `rebuild ${runtimes}: rebuilds`);
    }
  },
};
