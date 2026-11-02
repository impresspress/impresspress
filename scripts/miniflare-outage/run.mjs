// Run the D1-outage scenarios: `node run.mjs [scenario-name...]` (all when
// none is named). Builds the fixture once, then runs each scenario on its own
// deployment. Exits non-zero if any scenario fails.
//
// A scenario is a module under `scenarios/` exporting
// `{ name, shape, run(deployment) }`, listed in `SCENARIOS` below; `run`
// throws (node:assert) to fail. `MINIFLARE_OUTAGE_VERBOSE=1` echoes the
// Worker's log. Prerequisites and environment: README.md.

import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { buildCandidate, Deployment } from "./harness.mjs";
import SCENARIOS from "./scenarios/index.mjs";

const wanted = process.argv.slice(2);
const unknown = wanted.filter((name) => !SCENARIOS.some((s) => s.name === name));
if (unknown.length > 0) {
  console.error(`unknown scenario(s): ${unknown.join(", ")}`);
  console.error(`known: ${SCENARIOS.map((s) => s.name).join(", ")}`);
  process.exit(2);
}
const selected = wanted.length === 0 ? SCENARIOS : SCENARIOS.filter((s) => wanted.includes(s.name));

// Each run its own state (local D1, KV, R2, staged release), so runs from two
// worktrees never share it: a fresh temporary directory, removed when every
// scenario passed and kept for a look when one failed. A named directory
// (`MINIFLARE_OUTAGE_STATE`) is emptied first and always kept.
const named = process.env.MINIFLARE_OUTAGE_STATE;
const stateRoot = named ?? mkdtempSync(join(tmpdir(), "impresspress-miniflare-outage-"));
if (named) {
  rmSync(stateRoot, { recursive: true, force: true });
  mkdirSync(stateRoot, { recursive: true });
}

const candidate = buildCandidate(stateRoot);

const failures = [];
for (const scenario of selected) {
  const deployment = new Deployment({
    name: scenario.name,
    shape: scenario.shape,
    candidate,
    stateRoot,
    vars: scenario.vars,
  });
  const started = Date.now();
  try {
    await scenario.run(deployment);
    console.log(`ok   ${scenario.name} (${((Date.now() - started) / 1000).toFixed(1)} s)`);
  } catch (error) {
    failures.push(scenario.name);
    console.log(`FAIL ${scenario.name}: ${error.stack ?? error}`);
    console.log(`---- last Worker log lines (${scenario.name})\n${deployment.tail()}\n----`);
  } finally {
    await deployment.stop();
  }
}

console.log(`\n${selected.length - failures.length}/${selected.length} scenarios passed`);
if (failures.length > 0) {
  console.log(`failed: ${failures.join(", ")}; state kept in ${stateRoot}`);
  process.exit(1);
}
if (!named) rmSync(stateRoot, { recursive: true, force: true });
