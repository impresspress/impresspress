// D1 outage plan rev 3.1 §6, Miniflare prepared-path step 7, the permanent
// row the live check cannot reach: every D1 statement fails `no such table`.
// The cold isolate answers the opaque 500 with a reference, the cause is in
// the Worker's log under that reference, and the next request inside the
// retry window answers 500 without building again.

import assert from "node:assert/strict";

export default {
  name: "permanent-fault",
  shape: "wafer-site",
  async run(d) {
    await d.deploy();
    await d.restart({ mode: "no-such-table" });

    const first = await d.fetchCounted("/");
    assert.equal(first.status, 500, `a permanent fault: status (body: ${first.body.slice(0, 200)})`);
    const reference = /reference: ([0-9a-f-]{36})/.exec(first.body)?.[1];
    assert.ok(reference, `the 500 names a reference: ${first.body}`);
    assert.ok(!first.body.includes("no such table"), "the cause stays out of the response");
    await d.settleLogs();
    const logged = d.logs.find((line) => line.includes(`[${reference}]`));
    assert.ok(logged?.includes("no such table"), `the log names the cause under ${reference}: ${logged}`);

    const second = await d.fetchCounted("/");
    assert.equal(second.status, 500, "the next request: status");
    assert.deepEqual(second.d1Calls, [], "the next request inside the window builds nothing");
  },
};
