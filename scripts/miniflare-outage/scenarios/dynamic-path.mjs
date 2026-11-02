// D1 outage plan rev 3.1 §6, Miniflare dynamic path: a version with no
// prepared plan (the deploy's candidate) on a cold isolate during an outage.
// No runtime: every route answers 503 + Retry-After until the retry window
// ends, and nothing is built inside it; each attempt after a window fails
// again and doubles the window (1 s, then 2 s, before jitter).
//
// The windows are measured from the requests' own clocks, never from the
// rounded-up Retry-After: a build attempt happens between its request's send
// and its answer, so two attempts are at most `later.doneAt - earlier.sentAt`
// apart and at least `later.sentAt - earlier.doneAt`.

import assert from "node:assert/strict";

import { sleep } from "../harness.mjs";
import { assertUnavailable, attemptedBuild, RECOVERY_DEADLINE_MS, WINDOW_BASE_MS } from "./common.mjs";

const PATHS = ["/", "/about.html", "/b/auth/login"];
/** Requests this far apart sample every window several times. */
const GAP_MS = 50;

export default {
  name: "dynamic-path",
  shape: "wafer-site",
  async run(d) {
    await d.deploy();
    await d.start("candidate", { mode: "quota" });

    // Poll the routes until three build attempts have happened: the cold one
    // and the first two after a window.
    const attempts = [];
    const between = [];
    const started = Date.now();
    for (let i = 0; attempts.length < 3; i++) {
      assert.ok(Date.now() - started < RECOVERY_DEADLINE_MS, "three build attempts in time");
      const path = PATHS[i % PATHS.length];
      const answer = await d.fetchCounted(path);
      assertUnavailable(answer, `dynamic ${path}`);
      if (attemptedBuild(answer.d1Calls)) {
        attempts.push(answer);
      } else {
        assert.deepEqual(answer.d1Calls, [], `inside a window ${path} sends D1 nothing`);
        if (attempts.length > 0) between.push(answer);
      }
      await sleep(GAP_MS);
    }
    assert.ok(between.length >= 3, `only ${between.length} requests fell inside the windows`);

    const [cold, second, third] = attempts;
    // At least the base window between the cold attempt and the next:
    // nothing is built inside it.
    const firstWindowMaxMs = second.doneAt - cold.sentAt;
    assert.ok(
      firstWindowMaxMs >= WINDOW_BASE_MS,
      `the second attempt came ${firstWindowMaxMs} ms after the first; the window is at least ${WINDOW_BASE_MS} ms`,
    );
    // The window after the second failure is at least twice the base.
    const secondWindowMaxMs = third.doneAt - second.sentAt;
    assert.ok(
      secondWindowMaxMs >= 2 * WINDOW_BASE_MS,
      `the third attempt came ${secondWindowMaxMs} ms after the second; a doubled window is at least ${2 * WINDOW_BASE_MS} ms`,
    );
    console.log(
      `     attempts ${second.sentAt - cold.doneAt}..${firstWindowMaxMs} ms apart, then ` +
        `${third.sentAt - second.doneAt}..${secondWindowMaxMs} ms; ` +
        `${between.length} requests inside the windows sent D1 nothing`,
    );
  },
};
