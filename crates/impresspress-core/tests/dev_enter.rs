//! `GET /b/dev/enter` — the sandbox's one-click entry page.
//!
//! What a host test can reach: who may load the page, what it is rendered
//! with, that those credentials really do open the workspace through the auth
//! block's login route, and that an exported bundle has no such page. The
//! script's half — the `fetch`, the cookie, the navigation — runs only in a
//! browser, and `crates/impresspress-web/tests/e2e/dev-enter.spec.ts` drives
//! it there.
#![cfg(feature = "block-dev")]

use std::sync::Arc;

use impresspress_core::{
    blocks::{
        auth::config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
        dev::{
            self,
            test_support::{dev_with_accounts, fake_bypass_rules, FakeControl, FakeShell},
            DevBlock, DevShared,
        },
    },
    test_support::{anon_msg, output_html, output_http_header, output_http_status, TestContext},
};
use wafer_run::{Block as _, Message};

/// The sandbox admin a browser build seeds (`impresspress-web/src/config.rs`),
/// under other values so no assertion here can be satisfied by the address
/// the workspace guide prints.
const EMAIL: &str = "sandbox-owner@example.com";
const PASSWORD: &str = "seeded-sandbox-password";

/// A browser navigation, as `dev_page.rs` builds one.
fn navigation(mut msg: Message) -> Message {
    msg.set_meta(
        "http.header.accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    );
    msg
}

/// A workspace whose bootstrap admin exists and is configured, as a booted
/// sandbox's is: the two config keys, and the account they created.
async fn sandbox() -> TestContext {
    let mut ctx = dev_with_accounts(FakeControl::new()).await;
    ctx.set_config(BOOTSTRAP_ADMIN_EMAIL_KEY, EMAIL);
    ctx.set_config(BOOTSTRAP_ADMIN_PASSWORD_KEY, PASSWORD);
    ctx.seed_account(EMAIL, PASSWORD, "admin").await;
    ctx
}

/// The value of `attribute` on the page, HTML-unescaped enough for the plain
/// values these tests use.
fn attribute<'a>(html: &'a str, attribute: &str) -> &'a str {
    let open = format!("{attribute}=\"");
    let start = html
        .find(&open)
        .unwrap_or_else(|| panic!("no {attribute} in {html}"))
        + open.len();
    let end = html[start..].find('"').expect("unterminated attribute");
    &html[start..start + end]
}

#[tokio::test]
async fn the_entry_page_is_public_uncached_and_carries_the_configured_admin() {
    let ctx = sandbox().await;

    // No session, and no redirect to the login form: this page is what
    // replaces the form.
    assert_eq!(
        output_http_status(
            ctx.request(navigation(anon_msg("retrieve", dev::ENTER_PATH)))
                .await
        )
        .await,
        200
    );
    // It carries a password, so it is never stored — the block-wide rule.
    assert_eq!(
        output_http_header(
            ctx.request(navigation(anon_msg("retrieve", dev::ENTER_PATH)))
                .await,
            "cache-control"
        )
        .await
        .as_deref(),
        Some("no-store")
    );

    let html = output_html(
        ctx.request(navigation(anon_msg("retrieve", dev::ENTER_PATH)))
            .await,
    )
    .await;
    // The credentials are the CONFIGURED ones — read at request time from the
    // keys the auth block reads — not a second copy of the seeded defaults.
    assert_eq!(attribute(&html, "data-email"), EMAIL);
    assert_eq!(attribute(&html, "data-password"), PASSWORD);
    assert!(!html.contains("admin123"), "{html}");
    assert_eq!(attribute(&html, "data-login"), "/b/auth/api/login");
    assert_eq!(attribute(&html, "data-workspace"), "/b/dev");
}

/// The rest of `/b/dev` is exactly as closed as it was: the public row is one
/// path, not a hole in the prefix.
#[tokio::test]
async fn nothing_else_under_the_prefix_became_public() {
    let ctx = sandbox().await;
    for (action, path, expected) in [
        // A page navigation is sent to the login page…
        ("retrieve", "/b/dev", 302),
        // …and an API call is refused outright.
        ("retrieve", "/b/dev/api/status", 401),
        ("retrieve", "/b/dev/api/tools.json", 401),
        ("retrieve", "/b/dev/static/dev.js", 401),
        // Under the entry path there is the page and nothing else.
        ("retrieve", "/b/dev/enter/anything", 404),
        ("create", "/b/dev/enter", 404),
    ] {
        let msg = anon_msg(action, path);
        let msg = if path == "/b/dev" {
            navigation(msg)
        } else {
            msg
        };
        assert_eq!(
            output_http_status(ctx.request(msg).await).await,
            expected,
            "{action} {path}"
        );
    }
}

/// What the page's script does, done by hand: the credentials on the page,
/// posted to the login route the page names, yield a session the workspace
/// accepts. No token is minted anywhere but in the auth block's own login.
#[tokio::test]
async fn the_credentials_on_the_page_open_the_workspace_through_the_login_route() {
    let ctx = sandbox().await;
    let html = output_html(
        ctx.request(navigation(anon_msg("retrieve", dev::ENTER_PATH)))
            .await,
    )
    .await;
    assert_eq!(attribute(&html, "data-login"), "/b/auth/api/login");

    // `sign_in` posts to `/b/auth/api/login` and asserts the 200.
    let session = ctx
        .sign_in(
            attribute(&html, "data-email"),
            attribute(&html, "data-password"),
        )
        .await;
    assert_eq!(
        output_http_status(
            ctx.request(navigation(
                session.cookie(anon_msg("retrieve", attribute(&html, "data-workspace")))
            ))
            .await
        )
        .await,
        200
    );
}

/// Once the admin's password is changed in the instance, the seeded one the
/// page still carries is refused by the login route with the 401 the script
/// turns into "one-click entry is off" plus a link to the login page.
#[tokio::test]
async fn a_changed_password_is_refused_by_the_login_route() {
    let mut ctx = dev_with_accounts(FakeControl::new()).await;
    ctx.set_config(BOOTSTRAP_ADMIN_EMAIL_KEY, EMAIL);
    ctx.set_config(BOOTSTRAP_ADMIN_PASSWORD_KEY, PASSWORD);
    // The account exists under a password that is no longer the seeded one.
    ctx.seed_account(EMAIL, "a-password-the-owner-chose", "admin")
        .await;

    let html = output_html(
        ctx.request(navigation(anon_msg("retrieve", dev::ENTER_PATH)))
            .await,
    )
    .await;
    let mut login = anon_msg("create", attribute(&html, "data-login"));
    login.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.51");
    let refused = ctx
        .request_json(
            login,
            &serde_json::json!({
                "email": attribute(&html, "data-email"),
                "password": attribute(&html, "data-password"),
            }),
        )
        .await;
    assert_eq!(output_http_status(refused).await, 401);
    // The fallback the script reveals is already in the document.
    assert!(
        html.contains(r#"id="dev-enter-login""#) && html.contains("/b/auth/login?redirect=/b/dev"),
        "{html}"
    );
}

/// An instance with no bootstrap password configured — the row can be cleared
/// once the account exists — renders the fallback alone: nothing to try, so
/// no script and no credential.
#[tokio::test]
async fn without_a_configured_password_the_page_only_offers_the_login_page() {
    let mut ctx = dev_with_accounts(FakeControl::new()).await;
    ctx.set_config(BOOTSTRAP_ADMIN_EMAIL_KEY, EMAIL);
    ctx.set_config(BOOTSTRAP_ADMIN_PASSWORD_KEY, "");

    let html = output_html(
        ctx.request(navigation(anon_msg("retrieve", dev::ENTER_PATH)))
            .await,
    )
    .await;
    assert!(!html.contains("data-password"), "{html}");
    assert!(!html.contains("data-email"), "{html}");
    assert!(html.contains("One-click entry is off"), "{html}");
    assert!(html.contains("/b/auth/login?redirect=/b/dev"), "{html}");
}

/// An exported bundle registers the block and routes none of it, so the
/// entry page does not exist there — neither declared, nor routed, nor
/// answered by the block if something reached it anyway.
#[tokio::test]
async fn an_exported_bundle_has_no_entry_page() {
    let shared = DevShared::new(
        FakeControl::new(),
        Arc::new(FakeShell::new()),
        fake_bypass_rules(),
    );
    let exported = Arc::new(DevBlock::runtime_only(Arc::clone(&shared)));

    // 1. Not declared: nothing in `/openapi.json`, nothing for the router to
    //    refine a route with.
    assert!(exported.info().endpoints.is_empty());
    // …while the workspace declares it, `Public`.
    let workspace = DevBlock::with_workspace(shared).info();
    let entry = workspace
        .endpoints
        .iter()
        .find(|endpoint| endpoint.path == dev::ENTER_PATH)
        .expect("the workspace declares the entry page");
    assert_eq!(entry.auth, wafer_run::AuthLevel::Public);

    // 2. Not routed: registered exactly as the browser runtime registers an
    //    exported bundle's — the block, and no route — the path is the
    //    site's ordinary 404, with the bootstrap credentials configured.
    let mut ctx = TestContext::with_admin()
        .await
        .with_auth_added()
        .await
        .with_sign_in_added();
    ctx.set_config(BOOTSTRAP_ADMIN_EMAIL_KEY, EMAIL);
    ctx.set_config(BOOTSTRAP_ADMIN_PASSWORD_KEY, PASSWORD);
    ctx.register_block(dev::BLOCK_NAME, exported.clone());
    let routed = ctx
        .request(navigation(anon_msg("retrieve", dev::ENTER_PATH)))
        .await;
    assert_eq!(output_http_status(routed).await, 404);

    // 3. Not answered: handed the request directly, past any router, the
    //    runtime-only block still refuses — and discloses nothing.
    let ctx = ctx.running_as(dev::BLOCK_NAME);
    let direct = exported
        .handle(
            &ctx,
            anon_msg("retrieve", dev::ENTER_PATH),
            wafer_run::InputStream::empty(),
        )
        .await;
    assert_eq!(output_http_status(direct).await, 404);
}
