// `/health` (IP-C, D1 outage plan I6) on a wafer-site-shaped deployment
// whose runtime is degraded by a D1 outage and rides it out over several
// retry windows:
//
// 1. healthy: 200 `{"status":"ok"}`, no `Retry-After`, and a warm `/health`
//    sends D1 and KV nothing;
// 2. outage, cold isolate, `/health` the first request: the prepared plan
//    hydrates without its config-declaring blocks, so the answer is the
//    report, not the edge's plain 503: 503 `degraded`, listing exactly the
//    blocks the runtime was published without (its own log line names
//    them), each `retrying`, with no D1 text. The request's D1 calls are
//    the build's two and nothing of the handler's;
// 3. inside the window, `/health` sends D1 and KV nothing and builds nothing;
// 4. each rebuild after a window, still in the outage: `rebuilds` grows, the
//    episode's `since` and each block's `since` stay where they were, each
//    block's `runtimes` grows (I2g);
// 5. the window elapsed while its rebuild runs: a `/health` served from the
//    cached runtime meanwhile still reports the episode, with a `retry_in`
//    that is a real wait (PR277-review m1: an underflow here answered
//    `Retry-After: 4294967295`);
// 6. D1 back: the first rebuild after the window answers 200 ok, and no
//    request is answered from the window after it must have ended.
//
// Every report's `retry_in` is checked against the window its runtime armed
// (the `retry_in_ms` the Worker logs), from the requests' own clocks
// (PR277-review m4: the wasm test cannot, its window runs on a fake timer).

import assert from "node:assert/strict";

import { sleep } from "../harness.mjs";

import {
  armedWindowMs,
  assertHealth,
  attemptedBuild,
  readsVariables,
  RECOVERY_DEADLINE_MS,
  windowRetryIn,
} from "./common.mjs";

/** The blocks the runtime's log says it was published without, since `since`. */
async function publishedWithout(d, since) {
  const line = await d.logLine((l) => l.includes("runtime published without blocks"), since);
  return JSON.parse(/degraded=(\[.*\])/.exec(line)[1]);
}

/** `fetchCounted(path)`, with the log position it started at. */
async function fetchMarked(d, path) {
  const mark = d.mark();
  const answer = await d.fetchCounted(path);
  answer.mark = mark;
  return answer;
}

/**
 * `/health` every `everyMs` until a request rebuilds; every answer, the
 * rebuilding one last. Deadline only against a hung run.
 */
async function untilRebuilt(d, everyMs = 100) {
  const answers = [];
  const started = Date.now();
  for (;;) {
    const answer = await fetchMarked(d, "/health");
    answers.push(answer);
    if (answer.built) return answers;
    assert.ok(Date.now() - started < RECOVERY_DEADLINE_MS, "a rebuild after the window, in time");
    await sleep(everyMs);
  }
}

/** `retry_in`, the top-level one and each block's, is inside `bounds`. */
function assertRetryIn(body, bounds, what) {
  for (const retryIn of [body.retry_in, ...body.unavailable.map((b) => b.retry_in)]) {
    assert.ok(
      retryIn >= bounds.min && retryIn <= bounds.max,
      `${what}: retry_in ${retryIn} s; the armed window leaves ${bounds.min}..${bounds.max} s`,
    );
  }
}

/**
 * A report answered from inside the window `armedBy` armed (`windowMs`
 * long): degraded, built nothing, sent D1 and KV nothing, the episode as
 * `episode` describes it.
 */
function assertInsideWindow(answer, armedBy, windowMs, episode, what) {
  const body = assertHealth(answer, what, "degraded");
  assert.equal(answer.built, false, `${what}: built nothing (${answer.cacheOutcome})`);
  assert.deepEqual(answer.d1Calls, [], `${what}: sent D1 nothing`);
  assert.deepEqual(answer.kvCalls, [], `${what}: sent KV nothing`);
  assert.deepEqual(body, { ...episode, retry_in: body.retry_in, unavailable: body.unavailable }, `${what}: the episode`);
  assertRetryIn(body, windowRetryIn(armedBy, windowMs, answer), what);
}

export default {
  name: "health-degraded",
  shape: "wafer-site",
  async run(d) {
    // 1.
    await d.deploy();
    assertHealth(await d.fetch("/health"), "healthy cold /health", "ok");
    const warm = await d.fetchCounted("/health");
    assertHealth(warm, "healthy warm /health", "ok");
    assert.deepEqual(warm.d1Calls, [], "healthy warm /health sends D1 nothing");
    assert.deepEqual(warm.kvCalls, [], "healthy warm /health sends KV nothing");

    // 2.
    await d.restart({ mode: "quota" });
    const cold = await fetchMarked(d, "/health");
    const first = assertHealth(cold, "outage cold /health", "degraded");
    assert.ok(cold.built, `the cold /health built (${cold.cacheOutcome})`);
    assert.ok(attemptedBuild(cold.d1Calls), "the cold build read the variables table, which failed");
    assert.equal(cold.d1Calls.length, 2, `the build's two calls and none of the handler's: ${JSON.stringify(cold.d1Calls)}`);
    const without = await publishedWithout(d, cold.mark);
    assert.ok(without.includes("wafer-run/auth"), `wafer-run/auth is degraded: ${without}`);
    assert.ok(without.includes("impresspress/auth-ui"), `impresspress/auth-ui is degraded: ${without}`);
    assert.deepEqual(
      first.unavailable.map((b) => b.block),
      without,
      "/health lists exactly the blocks the runtime was published without",
    );
    for (const block of first.unavailable) {
      assert.equal(block.class, "retrying", `${block.block}: class`);
      assert.equal(block.runtimes, 1, `${block.block}: runtimes`);
    }
    assert.equal(first.rebuilds, 1, "rebuilds: the cold build");
    const since = Date.parse(first.since);
    assert.ok(
      since >= cold.sentAt && since <= cold.doneAt,
      `the episode began with the cold build: since ${first.since} is outside its request`,
    );
    let armedBy = cold;
    let windowMs = await armedWindowMs(d, cold.mark);
    assertRetryIn(first, windowRetryIn(cold, windowMs, cold), "outage cold /health");
    const episode = { status: "degraded", since: first.since, rebuilds: 1 };
    const blockSince = Object.fromEntries(first.unavailable.map((b) => [b.block, b.since]));

    // 3. A request that is answered before the window can have ended is
    //    inside it: the window ends `windowMs` after an instant of the cold
    //    request's flight, so not before `cold.sentAt + windowMs`.
    let inside = 0;
    for (let i = 0; i < 20 && Date.now() < cold.sentAt + windowMs; i++) {
      const answer = await d.fetchCounted("/health");
      if (answer.doneAt >= cold.sentAt + windowMs) break;
      assertInsideWindow(answer, cold, windowMs, episode, `inside the first window, /health ${i}`);
      inside += 1;
    }
    assert.ok(inside >= 3, `only ${inside} /health requests were answered inside the first window`);

    // 4. One rebuild after the window, still in the outage.
    for (const rebuilds of [2]) {
      const endsBy = armedBy.doneAt + armedBy.retryAfter * 1000;
      const answers = await untilRebuilt(d);
      for (const [i, answer] of answers.slice(0, -1).entries()) {
        assertInsideWindow(answer, armedBy, windowMs, episode, `waiting for rebuild ${rebuilds}, /health ${i}`);
        assert.ok(answer.sentAt <= endsBy, `waiting for rebuild ${rebuilds}: /health ${i} was sent after the window ended`);
      }
      const rebuilt = answers.at(-1);
      const body = assertHealth(rebuilt, `rebuild ${rebuilds} /health`, "degraded");
      episode.rebuilds = rebuilds;
      assert.deepEqual(
        body,
        { ...episode, retry_in: body.retry_in, unavailable: body.unavailable },
        `rebuild ${rebuilds}: the same episode, one more rebuild`,
      );
      assert.deepEqual(body.unavailable.map((b) => b.block), without, `rebuild ${rebuilds}: the same blocks`);
      for (const block of body.unavailable) {
        assert.equal(block.since, blockSince[block.block], `rebuild ${rebuilds}: ${block.block}'s run began where it did`);
        assert.equal(block.runtimes, rebuilds, `rebuild ${rebuilds}: ${block.block}'s run spans every runtime`);
      }
      armedBy = rebuilt;
      windowMs = await armedWindowMs(d, rebuilt.mark);
      assertRetryIn(body, windowRetryIn(rebuilt, windowMs, rebuilt), `rebuild ${rebuilds} /health`);
    }

    // 5. Let the window end with no request, then hold the rebuild's D1
    //    calls: a `/health` sent meanwhile is served from the cached runtime
    //    (stale-while-revalidate) with the episode elapsed.
    await sleep(Math.max(0, armedBy.doneAt + armedBy.retryAfter * 1000 - Date.now()) + 100);
    await d.hold();
    const owner = fetchMarked(d, "/health");
    const heldSince = Date.now();
    while ((await d.held()) === 0) {
      assert.ok(Date.now() - heldSince < 10_000, "the rebuild reached D1");
      await sleep(20);
    }
    // A held call is counted once released, so what `meanwhile` sent is
    // what the ledger gained plus what is held beside the owner's one call.
    const callsBefore = (await d.calls()).length;
    const meanwhile = await d.fetch("/health");
    const sentMeanwhile = (await d.calls()).length - callsBefore + (await d.held()) - 1;
    const during = assertHealth(meanwhile, "/health while the rebuild runs", "degraded");
    assert.equal(meanwhile.built, false, `served from the cached runtime (${meanwhile.cacheOutcome})`);
    assert.equal(sentMeanwhile, 0, "the /health served meanwhile sent D1 nothing");
    assert.deepEqual(
      during,
      { ...episode, retry_in: during.retry_in, unavailable: during.unavailable },
      "/health while the rebuild runs: the elapsed episode",
    );
    await d.release();
    const rebuilt = await owner;
    const third = assertHealth(rebuilt, "rebuild 3 /health", "degraded");
    assert.ok(rebuilt.built, `the held request rebuilt (${rebuilt.cacheOutcome})`);
    episode.rebuilds = 3;
    assert.deepEqual(third, { ...episode, retry_in: third.retry_in, unavailable: third.unavailable }, "rebuild 3");
    for (const block of third.unavailable) {
      assert.equal(block.since, blockSince[block.block], `rebuild 3: ${block.block}'s run began where it did`);
      assert.equal(block.runtimes, 3, `rebuild 3: ${block.block}'s run spans every runtime`);
    }
    windowMs = await armedWindowMs(d, rebuilt.mark);
    assertRetryIn(third, windowRetryIn(rebuilt, windowMs, rebuilt), "rebuild 3 /health");

    // 6. D1 back. The window ends no later than the last report's answer
    //    plus its `Retry-After`; every request before the recovering one is
    //    inside it.
    const endsBy = rebuilt.doneAt + rebuilt.retryAfter * 1000;
    await d.setMode("ok");
    const answers = await untilRebuilt(d);
    for (const [i, answer] of answers.slice(0, -1).entries()) {
      assertInsideWindow(answer, rebuilt, windowMs, episode, `D1 back, inside the window, /health ${i}`);
      assert.ok(answer.sentAt <= endsBy, `D1 back: /health ${i} was sent after the window ended, and not rebuilt`);
    }
    const recovered = answers.at(-1);
    assertHealth(recovered, "the recovering /health", "ok");
    assert.ok(readsVariables(recovered.d1Calls), "the recovering rebuild read the variables table");
    const after = await d.fetchCounted("/health");
    assertHealth(after, "after recovery /health", "ok");
    assert.equal(after.cacheOutcome, "hit", "after recovery the runtime is served from the cache");
    assert.deepEqual(after.d1Calls, [], "after recovery /health sends D1 nothing");
    console.log(
      `     ${without.length} blocks degraded; rebuilds 1..3 over the windows; recovered after ` +
        `${answers.length - 1} request(s) inside the last window; ` +
        `/health while the rebuild ran: retry_in ${during.retry_in}`,
    );
  },
};
