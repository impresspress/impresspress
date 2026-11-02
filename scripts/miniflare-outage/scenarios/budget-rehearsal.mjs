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
// - **deploy**: `/_deploy/prepare` itself runs under the budget, so a deploy
//   that carries the lowered var fails at prepare.
//
// The live check's preconditions follow, and are written into the plan:
// apply the var to the *final* version's config (not through
// `impresspress deploy`), and make no admin save between the deploy and the
// check. Otherwise expect the candidate row.
//
// Below `LIVE_CHECK_VALUE` the Worker refuses the var outright: a permanent
// misconfiguration answering 500 on every dispatched route, the site's static
// files included. The scenario pins that too, because it is how the live check
// would break dev-test.

import assert from "node:assert/strict";

import { staticAssetPath } from "./common.mjs";

/** The value the live check sets on dev-test. */
const LIVE_CHECK_VALUE = 4;
/** Budgets the matrix covers. */
const VALUES = [3, 4, 5, 6, 8, 12];

const VAR = "IMPRESSPRESS_D1_CALLS_PER_INVOCATION";

function branch(answer) {
  if (answer.status === 500) return "500 permanent";
  if (answer.status === 503) return `503 transient, Retry-After ${answer.retryAfter}`;
  return `${answer.status}`;
}

/** Each budget's answers on `stage`'s cold isolate, route by route. */
async function row(d, stage, routes) {
  const answers = new Map();
  for (const value of VALUES) {
    await d.start(stage, { vars: { [VAR]: String(value) } });
    const byRoute = [];
    for (const path of routes) byRoute.push(await d.fetch(path));
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
          if (value < LIVE_CHECK_VALUE) {
            // Refused: every dispatched route is the permanent 500; only
            // `/b/static/`, served from R2 before any runtime, is untouched.
            assert.equal(answer.status, isStatic(i) ? 200 : 500, `${label} ${VAR}=${value} ${routes[i]}`);
          } else {
            assert.notEqual(answer.status, 500, `${label} ${VAR}=${value} ${routes[i]}: never the permanent branch`);
          }
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

    // A deploy under the live check's value fails at prepare.
    await d.start("candidate", { vars: { [VAR]: String(LIVE_CHECK_VALUE) } });
    const prepare = await d.prepare();
    console.log(`     /_deploy/prepare under ${VAR}=${LIVE_CHECK_VALUE}: ${prepare.status}`);
    assert.notEqual(prepare.status, 200, "a deploy under the lowered budget does not prepare");
  },
};
