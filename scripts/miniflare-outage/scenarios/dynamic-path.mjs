// D1 outage plan rev 3.1 §6, Miniflare dynamic path: a version with no
// prepared plan (the deploy's candidate) on a cold isolate during an outage.
// No runtime: every route answers 503 + Retry-After until the retry window
// ends, and nothing is built inside it; the attempt after the window fails
// again and the window doubles.

import assert from "node:assert/strict";

import { assertUnavailable, attemptedBuild, pollUntil } from "./common.mjs";

export default {
  name: "dynamic-path",
  shape: "wafer-site",
  async run(d) {
    await d.deploy();
    await d.start("candidate", { mode: "quota" });

    const first = await d.fetchCounted("/");
    assertUnavailable(first, "dynamic /");
    assert.ok(attemptedBuild(first.d1Calls), `the first request attempts the build: ${JSON.stringify(first.d1Calls)}`);
    const windowEnds = Date.now() + first.retryAfter * 1000;

    // Inside the window: 503 everywhere, and no D1 call at all.
    let inside = 0;
    for (const path of ["/", "/about.html", "/b/auth/login", "/", "/about.html"]) {
      const answer = await d.fetchCounted(path);
      if (Date.now() >= windowEnds - 250) break;
      assertUnavailable(answer, `inside the window ${path}`);
      assert.deepEqual(answer.d1Calls, [], `inside the window ${path} builds nothing`);
      inside++;
    }
    assert.ok(inside >= 3, `only ${inside} requests fit inside the window`);

    // After it: one attempt, and the window it arms is no shorter.
    const { answer: again } = await pollUntil(d, "/", (a) => attemptedBuild(a.d1Calls), {
      withinMs: first.retryAfter * 1000 + 1500,
    });
    assertUnavailable(again, "after the window /");
    assert.ok(again.retryAfter >= first.retryAfter, `the window doubles: ${first.retryAfter} s, then ${again.retryAfter} s`);
    const next = await d.fetchCounted("/");
    assertUnavailable(next, "inside the second window /");
    assert.deepEqual(next.d1Calls, [], "inside the second window nothing is built");
  },
};
