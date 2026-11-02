// The rehearsal for the live check (D1 outage plan rev 3.1 §6, "Rehearsal for
// the live check"; Decision 3): with D1 healthy, lower
// `IMPRESSPRESS_D1_CALLS_PER_INVOCATION` on an impresspress-site deployment
// (dev-test's shape) and record which branch each matrix route reaches.
//
// The live check needs the smallest value whose cold boot spends past the
// budget and degrades — the transient branch, 503 + Retry-After on both the
// boot (`/`, the landing switch) and a D1 route — and never the permanent one
// (500). The scenario prints the matrix and pins that value
// (`LIVE_CHECK_VALUE`): if it moves, the live check's value moves with it.
//
// Below the floor the Worker refuses the var outright: a permanent
// misconfiguration answering 500 on every dispatched route, the site's static
// files included. The scenario pins that too, because it is how the live check
// would break dev-test.

import assert from "node:assert/strict";

import { staticAssetPath } from "./common.mjs";

/** The value the live check sets on dev-test. */
const LIVE_CHECK_VALUE = 4;

const VAR = "IMPRESSPRESS_D1_CALLS_PER_INVOCATION";

/** The routes the live check looks at. */
function matrixRoutes(d) {
  return ["/", "/hello.html", staticAssetPath(d), "/b/auth/login"];
}

function branch(answer) {
  if (answer.status === 500) return "500 permanent";
  if (answer.status === 503) return `503 transient, Retry-After ${answer.retryAfter}`;
  return `${answer.status}`;
}

export default {
  name: "budget-rehearsal",
  shape: "impresspress-site",
  async run(d) {
    await d.deploy();
    const routes = matrixRoutes(d);
    const matrix = [];
    for (const value of [LIVE_CHECK_VALUE - 1, LIVE_CHECK_VALUE, LIVE_CHECK_VALUE + 1, LIVE_CHECK_VALUE + 2]) {
      await d.restart({ vars: { [VAR]: String(value) } });
      const answers = [];
      for (const path of routes) answers.push(await d.fetch(path));
      matrix.push({ value, answers });
    }

    console.log(`     ${VAR} rehearsal (impresspress-site, D1 healthy):`);
    console.log(`     | value | ${routes.join(" | ")} |`);
    for (const { value, answers } of matrix) {
      console.log(`     | ${value} | ${answers.map(branch).join(" | ")} |`);
    }

    const degradesTransiently = ({ answers }) => {
      const [home, site, asset, login] = answers;
      return (
        home.status === 503 &&
        home.retryAfter >= 1 &&
        login.status === 503 &&
        login.retryAfter >= 1 &&
        site.status === 200 &&
        asset.status === 200
      );
    };
    const smallest = matrix.find(degradesTransiently)?.value;
    assert.equal(
      smallest,
      LIVE_CHECK_VALUE,
      `the smallest value whose boot and D1 route take the transient branch`,
    );

    // Below the floor every dispatched route is the permanent 500; only
    // `/b/static/`, served from R2 before any runtime, is untouched.
    const below = matrix.find(({ value }) => value === LIVE_CHECK_VALUE - 1);
    for (const [i, answer] of below.answers.entries()) {
      const expected = routes[i].startsWith("/b/static/") ? 200 : 500;
      assert.equal(answer.status, expected, `${VAR}=${below.value} ${routes[i]}`);
    }
    for (const { value, answers } of matrix.filter(({ value }) => value >= LIVE_CHECK_VALUE)) {
      for (const [i, answer] of answers.entries()) {
        assert.notEqual(answer.status, 500, `${VAR}=${value} ${routes[i]}: never the permanent branch`);
      }
    }
  },
};
