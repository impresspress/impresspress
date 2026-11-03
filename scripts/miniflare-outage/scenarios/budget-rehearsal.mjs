// The rehearsal for the live check (D1 outage plan rev 3.1 §6, "Rehearsal for
// the live check"; Decision 3): with D1 healthy, lower
// `IMPRESSPRESS_D1_CALLS_PER_INVOCATION` on an impresspress-site deployment
// (dev-test's shape) and record which branch each matrix route reaches.
//
// The live check wants the transient branch — 503 + Retry-After on the boot
// (`/`, the landing switch) and on a D1 route, a static site file still 200 —
// and never the permanent one (500). Which branch a route reaches depends on
// the path the isolate is on, so the matrix has a row per path:
//
// - **final (prepared)**: the final version with a plan that matches the KV
//   config stamp. Its cold boot is D1-light, so a small budget degrades only
//   what reads stored config. The live check's value (`LIVE_CHECK_VALUE`) is
//   the smallest that takes the transient branch here.
// - **candidate (dynamic)**: no plan, or a plan an admin save has made stale
//   (its first request per cold isolate goes dynamic). The dynamic boot
//   spends far more, so at the live check's value every route is 503, the
//   static site file included, until a much larger budget.
// - **stale plan (dynamic, then U-B1)**: the final version after an admin
//   save moved the KV config stamp. Its cold isolate goes dynamic, whose
//   check of D1 fails under the budget, so request 1 is a 503 and builds
//   nothing (the D1 gate, CPU-GATE review M1); request 2, inside the window,
//   hydrates the stale plan and serves the static site file 200.
// - **deploy**: `/_deploy/prepare` itself runs under the budget, so a deploy
//   that carries the lowered var fails at prepare: a 503 with `Retry-After`,
//   every failure in it the spent budget (transient), never the 500 a fault
//   answers.
//
// Every 503 a client gets names no cause: a refused block is wafer-run's
// fixed text and a reference, and the spent budget ("call budget exhausted",
// with its numbers) is only in the Worker's log under that reference.
//
// The live check's preconditions follow, and are written into the plan:
// apply the var to the *final* version's config (not through
// `impresspress deploy`), and make no admin save between the deploy and the
// check. Otherwise expect the candidate row.
//
// Below `PARSE_FLOOR` the Worker refuses the var outright: a permanent
// misconfiguration answering 500 on every dispatched route, the site's static
// files included. The scenario pins that too, because it is how the live check
// would break dev-test.

import assert from "node:assert/strict";

import {
  assertRefused,
  assertUnavailable,
  AUTH_UI,
  buildsSince,
  staticAssetPath,
} from "./common.mjs";

/**
 * The smallest value the Worker accepts ("must be a whole number from 4 to
 * 1000", `impresspress-cloudflare`'s environment parse). A different quantity
 * from `LIVE_CHECK_VALUE`, which happens to equal it today.
 */
const PARSE_FLOOR = 4;
/** The value the live check sets on dev-test: the smallest prepared value that degrades transiently. */
const LIVE_CHECK_VALUE = 4;
/** What the adapter's spent budget says, in the log only. */
const BUDGET_CAUSE = "call budget exhausted";
/** What the edge logs for a request the retry window answered. */
const WINDOW_CAUSE = "no runtime while the retry window lasts";
/** Budgets the matrix covers. */
const VALUES = [3, 4, 5, 6, 8, 12];

const VAR = "IMPRESSPRESS_D1_CALLS_PER_INVOCATION";

/** Which 503 a client got: what its body is, never what caused it. */
function shape(answer) {
  if (answer.body.startsWith("Service temporarily unavailable")) return "edge, no runtime";
  try {
    const { message } = JSON.parse(answer.body);
    if (/^block `[^`]+` is temporarily unavailable \(ref: [0-9a-f]{16}\)$/.test(message)) {
      return "block refused (ref)";
    }
    return "Unavailable";
  } catch {
    return "other";
  }
}

function branch(answer) {
  if (answer.status === 500) return "500 permanent";
  if (answer.status === 503) {
    return `503 transient, Retry-After ${answer.retryAfter}: ${shape(answer)}`;
  }
  return `${answer.status}`;
}

/**
 * Each budget's answers on `stage`'s cold isolate, route by route; each
 * answer's `mark` is where its log lines start.
 */
async function row(d, stage, routes) {
  const answers = new Map();
  for (const value of VALUES) {
    await d.start(stage, { vars: { [VAR]: String(value) } });
    const byRoute = [];
    for (const path of routes) {
      const mark = d.mark();
      const answer = await d.fetch(path);
      answer.mark = mark;
      byRoute.push(answer);
    }
    answers.set(value, byRoute);
  }
  return answers;
}

function print(label, routes, answers) {
  console.log(`     ${label}:`);
  console.log(`     | ${VAR} | ${routes.join(" | ")} |`);
  for (const [value, byRoute] of answers) {
    console.log(`     | ${value} | ${byRoute.map(branch).join(" | ")} |`);
  }
}

export default {
  name: "budget-rehearsal",
  shape: "impresspress-site",
  async run(d) {
    await d.deploy();
    const routes = ["/", "/hello.html", staticAssetPath(d), "/b/auth/login"];
    const [home, site, asset, login] = routes.map((_, i) => i);
    const isStatic = (i) => routes[i].startsWith("/b/static/");

    const prepared = await row(d, "final", routes);
    const dynamic = await row(d, "candidate", routes);
    print("final version, current plan (prepared)", routes, prepared);
    print("candidate version, or a stale plan's first request (dynamic)", routes, dynamic);

    // The prepared row: the smallest transient value is the live check's.
    const transient = (byRoute) =>
      byRoute[home].status === 503 &&
      byRoute[home].retryAfter >= 1 &&
      byRoute[login].status === 503 &&
      byRoute[login].retryAfter >= 1 &&
      byRoute[site].status === 200 &&
      byRoute[asset].status === 200;
    const smallest = [...prepared].find(([, byRoute]) => transient(byRoute))?.[0];
    assert.equal(smallest, LIVE_CHECK_VALUE, "the smallest prepared value taking the transient branch");

    for (const [label, answers] of [
      ["prepared", prepared],
      ["dynamic", dynamic],
    ]) {
      for (const [value, byRoute] of answers) {
        for (const [i, answer] of byRoute.entries()) {
          if (value < PARSE_FLOOR) {
            // Refused: every dispatched route is the permanent 500; only
            // `/b/static/`, served from R2 before any runtime, is untouched.
            assert.equal(answer.status, isStatic(i) ? 200 : 500, `${label} ${VAR}=${value} ${routes[i]}`);
          } else {
            assert.notEqual(answer.status, 500, `${label} ${VAR}=${value} ${routes[i]}: never the permanent branch`);
          }
          if (answer.status === 503) assertUnavailable(answer, `${label} ${VAR}=${value} ${routes[i]}`);
        }
      }
    }

    // The dynamic row at the live check's value: everything but `/b/static/`
    // is a transient 503, the static site file included.
    for (const [i, answer] of dynamic.get(LIVE_CHECK_VALUE).entries()) {
      if (isStatic(i)) continue;
      assert.equal(answer.status, 503, `dynamic ${VAR}=${LIVE_CHECK_VALUE} ${routes[i]}`);
      assert.ok(answer.retryAfter >= 1, `dynamic ${VAR}=${LIVE_CHECK_VALUE} ${routes[i]}: Retry-After`);
    }

    // At the live check's value, every 503 that names a reference has its
    // cause logged under it: the spent budget for the request whose build or
    // dispatch spent it, the retry window for a request the window answered.
    // Each row's spent budget is logged at least once.
    for (const [label, answers] of [
      ["prepared", prepared],
      ["dynamic", dynamic],
    ]) {
      let budgetLogged = 0;
      for (const [i, answer] of answers.get(LIVE_CHECK_VALUE).entries()) {
        if (answer.status !== 503) continue;
        const what = `${label} ${VAR}=${LIVE_CHECK_VALUE} ${routes[i]}`;
        let line;
        if (shape(answer) === "block refused (ref)") {
          const reference = assertRefused(answer, what, AUTH_UI);
          line = await d.logLine((l) => l.includes(`correlation_id=${reference} `), answer.mark);
        } else if (shape(answer) === "edge, no runtime") {
          const reference = /\(reference: ([0-9a-f-]{36})\)$/.exec(answer.body)?.[1];
          assert.ok(reference, `${what}: the edge's 503 names a reference: ${answer.body}`);
          line = await d.logLine((l) => l.includes(`[${reference}]`), answer.mark);
        } else {
          continue; // A stored-config read failing closed names no reference (I4b).
        }
        assert.ok(
          line.includes(BUDGET_CAUSE) || line.includes(WINDOW_CAUSE),
          `${what}: the log under its reference names the spent budget or the window: ${line}`,
        );
        if (line.includes(BUDGET_CAUSE)) budgetLogged += 1;
      }
      assert.ok(budgetLogged >= 1, `${label} ${VAR}=${LIVE_CHECK_VALUE}: the spent budget is logged under a reference`);
    }

    // The stale-plan row at the live check's value: request 1 is a 503 that
    // assembled nothing, request 2 serves the static site file from the
    // stale plan's hydrate, the isolate's one build.
    await d.start("final", { vars: { [VAR]: String(LIVE_CHECK_VALUE) } });
    await d.bumpConfigStamp();
    await d.restart({ vars: { [VAR]: String(LIVE_CHECK_VALUE) } });
    const staleMark = d.mark();
    const staleFirst = await d.fetch(routes[site]);
    const staleSecond = await d.fetch(routes[site]);
    console.log(
      `     stale plan at ${VAR}=${LIVE_CHECK_VALUE}, ${routes[site]}: request 1 ${branch(staleFirst)}, ` +
        `request 2 ${branch(staleSecond)}`,
    );
    assertUnavailable(staleFirst, `stale plan ${VAR}=${LIVE_CHECK_VALUE} request 1 ${routes[site]}`);
    assert.equal(staleSecond.status, 200, `stale plan ${VAR}=${LIVE_CHECK_VALUE} request 2 ${routes[site]}`);
    assert.deepEqual(
      await buildsSince(d, staleMark),
      ["gate", "prepared"],
      "request 1 assembled nothing; request 2 hydrated the stale plan",
    );

    // A deploy under the live check's value fails at prepare, transiently:
    // every failure in the funnel is the spent budget, which a fresh
    // invocation may clear (#278).
    await d.start("candidate", { vars: { [VAR]: String(LIVE_CHECK_VALUE) } });
    const prepare = await d.prepare();
    console.log(
      `     /_deploy/prepare under ${VAR}=${LIVE_CHECK_VALUE}: ${prepare.status}, ` +
        `Retry-After ${prepare.retryAfter}: ${prepare.body.replace(/\s+/g, " ").slice(0, 200)}`,
    );
    assert.equal(prepare.status, 503, `a deploy under the lowered budget does not prepare (${prepare.body.slice(0, 300)})`);
    assert.ok(prepare.retryAfter >= 1, `prepare's 503 names a Retry-After: ${prepare.retryAfter}`);
  },
};
