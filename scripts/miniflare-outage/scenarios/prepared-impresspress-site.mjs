// D1 outage plan rev 3.1 §6, Miniflare prepared-path step 3 on an
// impresspress-site deployment: on a cold isolate during an outage, `/` is a
// 503 + Retry-After (its landing switch is a stored-config read that fails
// closed, I4b), never a 500; the site's static files and `/b/static/` still
// serve. The D1 route is wafer-run's refusal of a block it published without:
// fixed text and a reference, the cause only in the log.

import assert from "node:assert/strict";

import {
  assertCauseLogged,
  assertRefused,
  assertServed,
  assertUnavailable,
  AUTH_UI,
  QUOTA_CAUSE,
  staticAssetPath,
} from "./common.mjs";

export default {
  name: "prepared-impresspress-site",
  shape: "impresspress-site",
  async run(d) {
    await d.deploy();
    assert.equal((await d.fetch("/")).status, 302, "healthy /: the landing switch redirects");

    await d.restart({ mode: "quota" });
    const home = await d.fetch("/");
    assertUnavailable(home, "outage /");
    assert.equal(JSON.parse(home.body).error, "Unavailable", "outage /: error code");
    assertServed(await d.fetch("/hello.html"), "outage /hello.html", "a static site file");
    assertServed(await d.fetch(staticAssetPath(d)), "outage /b/static/");
    const mark = d.mark();
    const login = await d.fetch("/b/auth/login");
    await assertCauseLogged(d, assertRefused(login, "outage /b/auth/login", AUTH_UI), QUOTA_CAUSE, mark);
    assertUnavailable(await d.fetch("/"), "outage / again");
  },
};
