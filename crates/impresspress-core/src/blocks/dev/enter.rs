//! `GET /b/dev/enter` — one-click entry to the workspace.
//!
//! The sandbox is a browser-local instance: every visitor gets a private
//! runtime and database, and the admin account in it is a throwaway whose
//! credentials are public (the workspace page prints them). Making the visitor
//! type them into a form bought nothing and cost a great deal — an agent driving a cloud
//! browser could not get past the form at all — so the welcome page's "Open
//! workspace" link comes here instead (2026-10-02 amendment to design §4,
//! reversing its "no auto sign-in" decision).
//!
//! # How the session is obtained
//!
//! Through the front door. The page is rendered with the bootstrap admin's
//! email and password — read from the same two config keys the auth block
//! reads to create that account ([`BOOTSTRAP_ADMIN_EMAIL_KEY`],
//! [`BOOTSTRAP_ADMIN_PASSWORD_KEY`]), never from a second copy of their
//! values — and its script `POST`s them to the auth block's own login
//! endpoint, exactly as the login form does. Nothing here mints a token, and
//! every rule that endpoint applies (the credential check, the rate limit,
//! the session lifetime) applies to this sign-in too.
//!
//! The script, rather than a redirect carrying `Set-Cookie`, because the
//! sandbox's runtime answers from a service worker and a synthetic response's
//! `Set-Cookie` is not persisted: the login page keeps its session from the
//! login response's JSON for that reason, and this page does the same — with
//! the login page's own functions. `apiPost` and `keepSession`
//! (`auth_ui/assets/api_post.js`) are emitted ahead of the script, so there
//! is one request path and one cookie writer, and a sign-in that fails here
//! says why in the words the login form would use.
//!
//! # When it does not work
//!
//! The seeded password stops opening the account the moment somebody changes
//! it in this instance, and the config row can be cleared outright once the
//! account exists (`config_vars::is_provisioning_only_key`). Neither is an
//! error: the page says one-click entry is off and offers the normal login
//! page, which still works with whatever the password now is.
//!
//! # Where it exists
//!
//! Only in a sandbox **workspace**. Two things make that so, and neither is
//! the row's presence in `ROUTES` (the dispatch table is the same in every
//! mode): `WORKSPACE_ROUTES`, which only a workspace hands the router, and
//! the `workspace` guard at the top of `DevBlock::handle`, which refuses
//! every request on a runtime-only block. So an exported bundle — which
//! registers the block without routing it — has no such page, and a
//! deployment built without `block-dev` has no such code. That boundary is what makes a
//! public page carrying a password acceptable: it is public only to the one
//! visitor whose browser the instance lives in, and the password it carries
//! is the one that instance's own workspace page prints.

use maud::{html, Markup, PreEscaped};
use wafer_core::clients::config;
use wafer_run::{context::Context, OutputStream, WaferError};

use super::{no_store, no_store_db_error_internal, ROUTE_PREFIX};
use crate::{
    blocks::auth::config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
    ui::{self, components::auth_panel, templates::auth_split},
};

/// The auth block's login endpoint — the one the login form posts to.
const LOGIN_API: &str = "/b/auth/api/login";

/// What the script asks first: an endpoint only a signed-in admin is
/// answered by. A 200 means there is already a session to use.
const SESSION_PROBE: &str = "/b/dev/api/status";

/// The page's script: `assets/enter.js`, inlined.
const ENTER_JS: &str = include_str!("assets/enter.js");

/// The normal login page, returning to the workspace afterwards.
fn login_page_url() -> String {
    format!("/b/auth/login?redirect={ROUTE_PREFIX}")
}

/// The bootstrap admin's email and password, when this instance has both
/// configured.
///
/// The one place the sandbox's pages learn the credentials — this page signs
/// in with them and the workspace guide prints them (`page.rs`). Read through
/// the config client, as `AuthConfig::from_ctx` reads them: the same keys,
/// from the same store, at request time.
pub(super) async fn bootstrap_credentials(
    ctx: &dyn Context,
) -> Result<Option<(String, String)>, WaferError> {
    let email = config::get_default(ctx, BOOTSTRAP_ADMIN_EMAIL_KEY, "").await?;
    let password = config::get_default(ctx, BOOTSTRAP_ADMIN_PASSWORD_KEY, "").await?;
    Ok((!email.is_empty() && !password.is_empty()).then_some((email, password)))
}

/// Serve the entry page.
pub async fn handle(ctx: &dyn Context) -> OutputStream {
    let site = match ui::SiteConfig::load(ctx).await {
        Ok(site) => site,
        Err(e) => return no_store_db_error_internal(e, "entry page: site config read failed"),
    };
    let credentials = match bootstrap_credentials(ctx).await {
        Ok(credentials) => credentials,
        Err(e) => {
            return no_store_db_error_internal(e, "entry page: admin credentials read failed")
        }
    };
    let markup = ui::layout::page(
        "Open workspace",
        &site,
        auth_split(
            auth_panel(&site, None),
            body(
                credentials
                    .as_ref()
                    .map(|(email, password)| (email.as_str(), password.as_str())),
            ),
        ),
    );
    no_store().body(
        markup.into_string().into_bytes(),
        "text/html; charset=utf-8",
    )
}

/// The page body.
///
/// `credentials` is the bootstrap admin's email and password when the
/// instance still has both configured. `None` renders the fallback alone, with
/// no script: there is nothing to try.
///
/// The ids are the contract with `assets/enter.js` and the end-to-end test.
fn body(credentials: Option<(&str, &str)>) -> Markup {
    let login_page = login_page_url();
    html! {
        div .auth-form {
            h2 .auth-form__title { "Opening your workspace" }
            @if let Some((email, password)) = credentials {
                div #dev-enter
                    data-login=(LOGIN_API)
                    data-session-probe=(SESSION_PROBE)
                    data-email=(email)
                    data-password=(password)
                    data-workspace=(ROUTE_PREFIX) {
                    p #dev-enter-status .auth-form__subtitle { "Signing in as this sandbox's admin…" }
                    p #dev-enter-fallback hidden {
                        a #dev-enter-login .btn .btn--primary href=(login_page) { "Sign in" }
                    }
                }
                noscript {
                    p { "This page needs JavaScript. " a href=(login_page) { "Sign in" } " instead." }
                }
                // The auth forms' own helpers first — `apiPost` and
                // `keepSession` — which the entry script signs in through.
                script { (PreEscaped(crate::blocks::auth_ui::pages::api_post_script())) }
                script { (PreEscaped(ENTER_JS)) }
            } @else {
                div #dev-enter {
                    p #dev-enter-status .auth-form__subtitle {
                        "One-click entry is off for this sandbox: it has no bootstrap admin \
                         password configured. Sign in with your own account instead."
                    }
                    p #dev-enter-fallback {
                        a #dev-enter-login .btn .btn--primary href=(login_page) { "Sign in" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id and attribute the script reads is in the markup it is inlined
    /// into.
    #[test]
    fn the_script_and_the_markup_agree() {
        let html = body(Some(("someone@example.com", "a-password"))).into_string();
        for id in ["dev-enter", "dev-enter-status", "dev-enter-fallback"] {
            assert!(ENTER_JS.contains(&format!("'{id}'")), "{id} unused");
            assert!(html.contains(&format!("id=\"{id}\"")), "{id} missing");
        }
        for attribute in [
            "data-login",
            "data-session-probe",
            "data-email",
            "data-password",
            "data-workspace",
        ] {
            assert!(ENTER_JS.contains(&format!("'{attribute}'")), "{attribute}");
            assert!(html.contains(&format!("{attribute}=\"")), "{attribute}");
        }
        assert!(html.contains(r#"data-login="/b/auth/api/login""#), "{html}");
        assert!(html.contains(r#"data-workspace="/b/dev""#), "{html}");
        assert!(
            html.contains(r#"data-email="someone@example.com""#),
            "{html}"
        );
        // The fallback link is there from the start, hidden until needed.
        assert!(html.contains(r#"id="dev-enter-fallback" hidden"#), "{html}");
        assert!(
            html.contains(r#"href="/b/auth/login?redirect=/b/dev""#),
            "{html}"
        );
    }

    /// The script signs in through the login endpoint it was handed and sets
    /// the session cookie the way the login page does. Pinned as source
    /// because only a browser can run it (`dev-enter.spec.ts` does).
    #[test]
    fn the_script_uses_the_login_endpoint_and_the_login_pages_cookie() {
        assert!(ENTER_JS.contains("apiPost(root.getAttribute('data-login')"));
        // The helpers it calls are on the page, ahead of it.
        let html = body(Some(("a@b.c", "pw"))).into_string();
        let helpers = html
            .find("async function apiPost(path,body){")
            .expect("the page emits the shared auth helpers");
        assert!(html.find("function keepSession(d){").is_some(), "{html}");
        let own = html
            .find("function giveUp(message)")
            .expect("the entry script");
        assert!(
            helpers < own,
            "the helpers must be defined before the script runs"
        );
        // An existing session is used, not replaced: the probe comes first,
        // and it is a route this block serves at `Admin`.
        assert!(ENTER_JS.contains("fetch(root.getAttribute('data-session-probe')"));
        assert!(ENTER_JS.contains("if (!hasKeptSession()) {"));
        assert!(super::super::ROUTES
            .iter()
            .any(|row| row.template == SESSION_PROBE && row.auth == wafer_run::AuthLevel::Admin));
        // …through the forms' one cookie writer, never one of its own.
        assert!(ENTER_JS.contains("keepSession(answer);"));
        assert!(!ENTER_JS.contains("document.cookie"));
        assert!(ENTER_JS.contains("location.replace(root.getAttribute('data-workspace'))"));
        assert!(ENTER_JS.contains("return signedIn ? openWorkspace() : signIn();"));
        // Inlined in a `<script>` element, so it must not be able to end it.
        assert!(!ENTER_JS.contains("</script"));
    }

    /// A credential is an attribute value, so whatever it contains is escaped
    /// rather than allowed to close the attribute.
    #[test]
    fn a_credential_cannot_break_out_of_its_attribute() {
        let html = body(Some(("a@b.c", r#"p"><script>alert(1)</script>"#))).into_string();
        assert!(!html.contains("<script>alert(1)"), "{html}");
        assert!(html.contains("&quot;&gt;&lt;script&gt;"), "{html}");
    }

    /// With no credentials configured there is nothing to try: no script, no
    /// credential attributes, and the fallback visible from the start.
    #[test]
    fn without_credentials_the_page_is_the_fallback_alone() {
        let html = body(None).into_string();
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("data-password"), "{html}");
        assert!(html.contains(r#"<p id="dev-enter-fallback">"#), "{html}");
        assert!(html.contains("One-click entry is off"), "{html}");
        assert!(
            html.contains(r#"href="/b/auth/login?redirect=/b/dev""#),
            "{html}"
        );
    }
}
