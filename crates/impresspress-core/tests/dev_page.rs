//! `GET /b/dev` — the workspace document, and the two static assets it pulls.
//!
//! Gated on `block-dev` for the same reason `dev_status.rs` is: the block
//! does not exist in a default-feature build, so these tests must not even
//! compile there.
#![cfg(feature = "block-dev")]

use impresspress_core::{
    blocks::dev::{
        assets,
        test_support::{dev_with_accounts, signed_in_as, FakeControl},
        validation::MAX_ARTIFACT_BYTES,
    },
    test_support::{
        admin_msg, anon_msg, output_html, output_http_header, output_http_status, TestContext,
    },
};
use wafer_run::Message;

/// Turn a request into an HTML *navigation* — what a browser sends when the
/// visitor types the URL, as opposed to what `fetch` sends for the JSON API.
///
/// The distinction is load-bearing and not something `anon_msg` can carry on
/// its own: [`impresspress_core::ui::unauthenticated_response`] keys the
/// login redirect off `Accept: text/html`, and answers a plain `403` to
/// everything else so an API caller keeps the JSON contract. A message with
/// no `Accept` at all is therefore the API case, not the page case.
fn navigation(mut msg: Message) -> Message {
    msg.set_meta(
        "http.header.accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    );
    msg
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dev_page_is_admin_only_cross_origin_isolated_and_uncached() {
    // A browser navigation carries the session cookie, so every request here
    // presents one, and the router resolves it the way it resolves a
    // visitor's.
    let ctx = dev_with_accounts(FakeControl::new()).await;
    let member = signed_in_as(&ctx, "user").await;
    let operator = signed_in_as(&ctx, "admin").await;

    // Anonymous navigation → the login page, not the workspace. The router
    // is the gate (the page route is declared `Admin` like every other
    // `/b/dev` route), so this is the same 302 any admin page answers.
    assert_eq!(
        output_http_status(
            ctx.request(navigation(anon_msg("retrieve", "/b/dev")))
                .await
        )
        .await,
        302
    );
    // Signed in but not an admin is a genuine refusal, not a login problem.
    assert_eq!(
        output_http_status(
            ctx.request(navigation(member.cookie(anon_msg("retrieve", "/b/dev"))))
                .await
        )
        .await,
        403
    );

    // An `OutputStream` is consumed by reading it, so each header assertion
    // needs its own request.
    for (header, expected) in [
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-embedder-policy", "credentialless"),
        ("cache-control", "no-store"),
    ] {
        assert_eq!(
            output_http_header(
                ctx.request(navigation(operator.cookie(anon_msg("retrieve", "/b/dev"))))
                    .await,
                header
            )
            .await
            .as_deref(),
            Some(expected),
            "{header}"
        );
    }

    let html = output_html(
        ctx.request(navigation(operator.cookie(anon_msg("retrieve", "/b/dev"))))
            .await,
    )
    .await;
    for id in [
        "dev-guide",
        "dev-files",
        "dev-editor",
        "dev-preview",
        "dev-progress",
        "dev-actions",
    ] {
        assert!(html.contains(&format!("id=\"{id}\"")), "{id}");
    }
    // The preview frame's exact opening tag: the `sandbox` attribute is what
    // keeps a page the agent wrote (or a page a shopper's content ends up
    // in) from reaching this document's tools, so it is pinned here rather
    // than left to the markup's shape.
    assert!(html.contains(
        r#"<iframe id="dev-preview-frame" src="/" sandbox="allow-scripts allow-same-origin allow-forms allow-popups""#
    ));
    // `type="module"` is load-bearing, not decoration: the composed script
    // opens with an `import` of the compiler adapter, which a classic script
    // cannot parse at all. A `defer` here (the shape this tag had before the
    // adapter existed) would take the whole workspace page down.
    assert!(
        html.contains(r#"<script type="module" src="/b/dev/static/dev.js">"#),
        "the page must load dev.js as a module: {html}"
    );
    assert!(html.contains("/b/dev/static/dev.css"));
    assert!(
        html.contains("admin@example.com"),
        "the guide shows the local credentials"
    );
}

/// COOP/COEP are deployment-wide now (amendment 14: the browser runtime sets
/// `cross_origin_isolation = credentialless` on the whole sandbox deployment,
/// not just this page — the preview iframe needs the site itself to carry a
/// compatible COEP too). This test does not contradict that: it pins that
/// `/b/dev`'s own JSON API does not set the pair itself — `page::handle` sets
/// it explicitly on the workspace document (see the module doc comment for
/// why: cross-origin isolation is what `SharedArrayBuffer`, and so the
/// in-browser compiler, needs), and nothing upstream of this route adds it a
/// second time.
#[tokio::test]
async fn other_pages_are_not_cross_origin_isolated() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    assert_eq!(
        output_http_header(
            ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/status"))
                .await,
            "cross-origin-opener-policy"
        )
        .await,
        None
    );
}

// ---------------------------------------------------------------------------
// The page's own assets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_page_assets_are_served_admin_only_and_revalidated() {
    let ctx = dev_with_accounts(FakeControl::new()).await;
    let operator = signed_in_as(&ctx, "admin").await;

    for (path, content_type) in [
        (
            "/b/dev/static/dev.js",
            "application/javascript; charset=utf-8",
        ),
        ("/b/dev/static/dev.css", "text/css; charset=utf-8"),
        (
            "/b/dev/static/compiler-adapter.js",
            "application/javascript; charset=utf-8",
        ),
    ] {
        assert_eq!(
            output_http_status(ctx.request(anon_msg("retrieve", path)).await).await,
            401,
            "{path} must be admin-only like the page it belongs to"
        );
        assert_eq!(
            output_http_status(
                ctx.request(operator.cookie(anon_msg("retrieve", path)))
                    .await
            )
            .await,
            200,
            "{path}"
        );
        assert_eq!(
            output_http_header(
                ctx.request(operator.cookie(anon_msg("retrieve", path)))
                    .await,
                "content-type"
            )
            .await
            .as_deref(),
            Some(content_type),
            "{path}"
        );
        // `no-cache` (revalidate), not `no-store`: these bytes are fixed for
        // a given build, so a conditional request is the right cost — but
        // they carry no content hash, so they must never be held past a
        // rebuild the way the hashed `/b/static/*` bundle is.
        assert_eq!(
            output_http_header(
                ctx.request(operator.cookie(anon_msg("retrieve", path)))
                    .await,
                "cache-control"
            )
            .await
            .as_deref(),
            Some("no-cache"),
            "{path}"
        );
    }

    assert_eq!(
        output_html(
            ctx.request(operator.cookie(anon_msg("retrieve", "/b/dev/static/dev.js")))
                .await
        )
        .await,
        assets::dev_js()
    );
    assert_eq!(
        output_html(
            ctx.request(operator.cookie(anon_msg("retrieve", "/b/dev/static/dev.css")))
                .await
        )
        .await,
        assets::dev_css()
    );
    assert_eq!(
        output_html(
            ctx.request(operator.cookie(anon_msg("retrieve", "/b/dev/static/compiler-adapter.js")))
                .await
        )
        .await,
        assets::compiler_adapter_js()
    );
}

/// The two page assets each carry an `ETag`, and a matching `If-None-Match`
/// gets a bodyless `304` — the same conditional-GET comparison
/// `/b/webmcp/webmcp.js`'s stable route uses, via `http::conditional`.
#[tokio::test]
async fn the_page_assets_answer_conditional_get() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;

    for (path, hash) in [
        ("/b/dev/static/dev.js", assets::dev_js_hash()),
        ("/b/dev/static/dev.css", assets::dev_css_hash()),
    ] {
        let etag = format!("\"{hash}\"");
        assert_eq!(
            output_http_header(
                ctx.dispatch_resolved(admin_msg("retrieve", path)).await,
                "etag"
            )
            .await
            .as_deref(),
            Some(etag.as_str()),
            "{path}"
        );

        let mut fresh = admin_msg("retrieve", path);
        fresh.set_meta("http.header.if-none-match", &etag);
        assert_eq!(
            output_http_status(ctx.dispatch_resolved(fresh).await).await,
            304,
            "{path}: a matching If-None-Match must produce a 304"
        );
        let mut fresh_body = admin_msg("retrieve", path);
        fresh_body.set_meta("http.header.if-none-match", &etag);
        assert_eq!(
            output_html(ctx.dispatch_resolved(fresh_body).await).await,
            "",
            "{path}: a 304 must carry no body"
        );

        let mut stale = admin_msg("retrieve", path);
        stale.set_meta("http.header.if-none-match", "\"not-the-current-hash\"");
        assert_eq!(
            output_http_status(ctx.dispatch_resolved(stale).await).await,
            200,
            "{path}: a mismatching If-None-Match must fall through to the full response"
        );
    }
}

// ---------------------------------------------------------------------------
// The script itself
// ---------------------------------------------------------------------------

#[test]
fn dev_js_registers_only_from_tools_json_and_never_touches_the_global_manifest() {
    let js = assets::dev_js();
    assert!(js.contains("/b/dev/api/tools.json"));
    assert!(
        !js.contains("/b/webmcp/manifest.json"),
        "the global manifest is webmcp.js's job"
    );
    assert!(js.contains("AbortController"));
    assert!(js.contains("pagehide"));
}

/// The tail is authored as a fragment and only ever served composed with
/// `webmcp-core.js` inside one IIFE — so `buildRequest`/`toolOptions` are in
/// scope, and nothing the tail declares leaks onto `window`. Since the
/// compiler adapter landed the whole thing is a MODULE: one `import`
/// declaration, which may only stand at the top level, and then the same IIFE
/// the classic `webmcp.js` composition produces.
#[test]
fn dev_js_is_one_composed_iife_over_the_shared_webmcp_core() {
    let js = assets::dev_js();
    assert!(
        js.starts_with(
            "import { BrowserRustCompiler } from '/b/dev/static/compiler-adapter.js';\n"
        ),
        "the import must come first — nothing may precede a module's imports: {js:.120}"
    );
    let after_imports = js
        .split_once('\n')
        .expect("the composed module has more than one line")
        .1;
    assert!(
        after_imports.starts_with("(function () {\n  'use strict';\n"),
        "the IIFE must follow the imports unchanged: {after_imports:.60}"
    );
    assert!(js.ends_with("})();\n"));
    // Exactly one import — the leading one `starts_with` just pinned. An
    // `import` further down would be a declaration smuggled into the tail,
    // where it cannot legally stand.
    assert_eq!(js.matches("\nimport ").count(), 0);
    // The core half…
    assert!(js.contains("function buildRequest(invocation, args)"));
    assert!(js.contains("function toolOptions(tool)"));
    // …and the tail half, which must not re-open an IIFE of its own. The
    // strict directive is counted WITH its semicolon: both files talk about
    // `'use strict'` in prose, and only the wrapper emits the statement.
    assert!(js.contains("'/b/dev/api/files/write'"));
    assert_eq!(js.matches("'use strict';").count(), 1);
}

/// Everything a module changes about the tail, checked on the bytes that ship.
///
/// A module's top level is strict, has no `arguments`, and — the one that
/// actually bites — gives `document.currentScript` as `null`, so a script
/// that located its own tag would break silently on the way from `defer` to
/// `type="module"`. The composed bytes are also parsed as a module by
/// `node --check --input-type=module` in the same run that produced this
/// test; this is the cheap standing guard.
#[test]
fn the_composed_module_relies_on_nothing_a_module_takes_away() {
    let js = assets::dev_js();
    assert!(
        !js.contains("document.currentScript"),
        "currentScript is null in a module"
    );
    // `arguments` outside a function, and `with`, are both illegal in strict
    // code; the tail was already strict inside the IIFE, so this only pins
    // that nobody reintroduces them along with a module rewrite.
    assert!(!js.contains("with ("), "`with` is illegal in a module");
}

/// The adapter is a standalone module, not a second WebMCP tail.
#[test]
fn the_compiler_adapter_is_a_bare_module_that_exports_the_class() {
    let js = assets::compiler_adapter_js();
    assert!(
        js.contains("export class BrowserRustCompiler {"),
        "the class must be the module's named export"
    );
    // It imports nothing. `webmcp-core.js` is about calling HTTP tools; a
    // worker session has no HTTP in it, and an import here would tie the
    // adapter to a fragment it does not use.
    assert!(!js.contains("\nimport "), "the adapter imports nothing");
    assert!(!js.starts_with("import "), "the adapter imports nothing");
    // No global. A `window.` anything would put the compiler where the
    // preview iframe's contents could reach it.
    assert!(
        !js.contains("window."),
        "the adapter must not reach for a global"
    );
    // Nor is it composed: the IIFE wrapper belongs to the WebMCP scripts.
    assert!(!js.contains("'use strict';"), "a module is already strict");
}

/// The artifact ceiling the adapter enforces on the page is the one
/// `POST /b/dev/api/builds/stage` enforces on the other side.
///
/// Two limits that could drift apart would mean either a compile the page
/// refuses and the server would have taken, or — the bad direction — a
/// multi-megabyte base64 body built up in the browser only to be refused.
#[test]
fn the_adapter_and_the_stage_endpoint_agree_on_the_artifact_ceiling() {
    let js = assets::compiler_adapter_js();
    assert!(
        js.contains(&format!("const MAX_ARTIFACT_BYTES = {MAX_ARTIFACT_BYTES};")),
        "compiler-adapter.js must spell out validation::MAX_ARTIFACT_BYTES ({MAX_ARTIFACT_BYTES})"
    );
}

/// The protocol the adapter speaks is the one `compiler/src/protocol.ts`
/// defines. The two halves are in the same repo but not the same crate — the
/// worker is built by `examples/dev-sandbox/compiler/build-compiler.sh` — so
/// nothing but a test like this notices a message name that drifted.
#[test]
fn the_adapter_speaks_every_message_the_protocol_defines() {
    let js = assets::compiler_adapter_js();
    for sent in ["type: 'init'", "type: 'compile'", "type: 'cancel'"] {
        assert!(js.contains(sent), "the adapter never sends {sent}");
    }
    for received in [
        "case 'progress':",
        "case 'ready':",
        "case 'result':",
        "case 'error':",
    ] {
        assert!(
            js.contains(received),
            "the adapter never handles {received}"
        );
    }
}
