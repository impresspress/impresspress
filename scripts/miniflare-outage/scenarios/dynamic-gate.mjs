// A warm, degraded **dynamic** runtime whose retry meets a D1 outage, on the
// `wafer-site-init-reader` shape's candidate version (no
// packaged plan: every build is dynamic, as on an isolate after an admin
// save).
//
// 1. `init-reset`: D1 answers everything but `miniflare/init-reader`'s own
//    `Init` statement, so the cold dynamic build publishes without that
//    block: `/health` 503 `degraded`.
// 2. D1 goes down. The retry after the window is the gate alone, and the
//    gate is its first D1 read (a warm dynamic retry also reads
//    `block_settings` through to D1; were that read first, the outage would
//    fail it before the gate and mark the current-config runtime `stale`).
//    The retry builds nothing (`build=gate`), its request reports `gated`,
//    and `/health` stays `degraded` (not `stale`), `retries` 2, listing the
//    same block, and `backend` `failing`. The cold build's `Init` failure
//    (D1's reset) started the failing run, and the failed gate extends it:
//    `since` stays, `checked` moves to the gate. Before `backend`, this body
//    read as a block failing its own `Init` while D1 answered (FOLLOWUPS
//    "health body not naming a failed backend check").

import assert from "node:assert/strict";

import {
  assertHealth,
  buildsSince,
  pollUntil,
  probedBackend,
  RECOVERY_DEADLINE_MS,
} from "./common.mjs";

export default {
  name: "dynamic-gate",
  shape: "wafer-site-init-reader",
  async run(d) {
    await d.deploy();
    await d.start("candidate", { mode: "init-reset" });

    // 1.
    let mark = d.mark();
    const cold = await d.fetchCounted("/health");
    const degraded = assertHealth(cold, "cold /health", "degraded");
    assert.ok(cold.built, `the cold dynamic build published (${cold.cacheOutcome})`);
    assert.deepEqual(await buildsSince(d, mark), ["dynamic"], "the cold isolate's one build");
    assert.equal(degraded.retries, 1, "the degraded publish armed the window");
    assert.equal(degraded.backend?.status, "failing", `D1 failed the cold build's Init call: ${cold.body}`);
    const failedAt = Date.parse(degraded.backend.since);
    assert.ok(
      failedAt >= cold.sentAt && failedAt <= cold.doneAt,
      `backend.since ${degraded.backend.since} is the cold build's Init failure, inside its request`,
    );

    // 2.
    await d.setMode("quota");
    mark = d.mark();
    const { answer: retry, answers } = await pollUntil(d, "/health", (a) => probedBackend(a.d1Calls), {
      withinMs: RECOVERY_DEADLINE_MS,
    });
    for (const [i, a] of answers.slice(0, -1).entries()) {
      assertHealth(a, `inside the window, /health ${i}`, "degraded");
      assert.deepEqual(a.d1Calls, [], `inside the window, /health ${i} sent D1 nothing`);
    }
    const body = assertHealth(retry, "the retry's /health", "degraded");
    assert.equal(retry.built, false, `the retry built nothing (${retry.cacheOutcome})`);
    assert.equal(retry.cacheOutcome, "gated", "the retry reports its check");
    assert.equal(body.retries, 2, "the failed gate is one more retry");
    assert.equal(body.since, degraded.since, "the same episode");
    assert.deepEqual(
      body.unavailable.map((b) => b.block),
      degraded.unavailable.map((b) => b.block),
      "the kept runtime lists what it listed",
    );
    assert.equal(body.backend?.status, "failing", `the retry's check failed: ${retry.body}`);
    assert.equal(body.backend.since, degraded.backend.since, "the failed check extends the run");
    const checked = Date.parse(body.backend.checked);
    assert.ok(
      checked >= retry.sentAt && checked <= retry.doneAt,
      `backend.checked ${body.backend.checked} is the retry's own check`,
    );
    assert.deepEqual(await buildsSince(d, mark), ["gate"], "the retry assembled no runtime");
    assert.ok(
      retry.d1Calls[0].statements.every((sql) => !sql.includes("block_settings")),
      `the gate is the retry's first D1 read: ${JSON.stringify(retry.d1Calls[0])}`,
    );
  },
};
