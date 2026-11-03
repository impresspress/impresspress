// `impresspress deploy`'s pre-promotion smoke (PR277-review m2, IPC-report
// Miniflare scenario 9) against the final version, run by the CLI's own smoke
// code (`miniflare_deploy smoke`: the authenticated `/health` GET, then the
// 160-request, 32-in-flight concurrency smoke over `deploy_smoke_paths`,
// which for this fixture is the default, `/health`):
//
// - with D1 down, the smoke fails at its `/health` GET (503 `degraded`), so
//   a deploy stops before promotion and the live version is untouched;
// - with D1 healthy, on cold isolates, the full smoke passes twice in a row:
//   `/health` answering 503 on any isolate that is not serving would stop a
//   healthy deploy.
//
// What is not driven: the step after the smoke. `cli/flows/embed_cloudflare.rs`
// promotes only once both smoke calls returned `Ok` (each is `?`-propagated
// before `authorize_promotion`); that ordering is the flow's code, read, not
// run here.

import assert from "node:assert/strict";

export default {
  name: "deploy-smoke",
  shape: "wafer-site",
  async run(d) {
    await d.deploy({ mode: "quota" });
    const down = await d.smoke();
    assert.equal(down.ok, false, `the smoke passed with D1 down:\n${down.output}`);
    assert.match(
      down.output,
      /final candidate smoke GET \/health failed with 503/,
      `the smoke stops at its /health GET:\n${down.output}`,
    );
    assert.ok(
      !/concurrency smoke/.test(down.output),
      `the concurrency smoke never ran after the /health GET failed:\n${down.output}`,
    );

    for (const run of [1, 2]) {
      await d.restart({ mode: "ok" });
      const healthy = await d.smoke();
      assert.ok(healthy.ok, `healthy smoke ${run} failed:\n${healthy.output}`);
    }
  },
};
