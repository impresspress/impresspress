//! `impresspress/auth-ui`: the sign-in, sign-up, password and organisation
//! pages, and the auth admin settings page.

use std::sync::Arc;

use wafer_run::{Block, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::{auth::repo::users, auth_ui::AuthUiBlock},
    test_support::{
        admin_msg, anon_msg, auth_msg,
        htmx::{Fixture, Page, Site},
        TestContext,
    },
    util::sha256_hex,
};

/// The signed-in user the authenticated pages are rendered for.
const USER: &str = "auth-page-user";

/// A user whose address is not yet proven, and the raw token of the
/// verification link mailed to them: the email's link is a page.
const UNVERIFIED: &str = "auth-unverified-user";
const VERIFY_TOKEN: &str = "htmx-guard-verification-token";

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/auth-ui",
        fixture: Some(fixture),
        exempt: &[
            ("/b/auth/api/api-keys", Exempt::JsonApi),
            ("/b/auth/api/oauth/providers", Exempt::JsonApi),
            (
                "/b/auth/oauth/login",
                Exempt::NotAPage("redirects the browser to the OAuth provider"),
            ),
            (
                "/b/auth/oauth/callback",
                Exempt::NotAPage(
                    "the OAuth provider's return leg: redeems the provider's code, then \
                     redirects",
                ),
            ),
        ],
        must_reach: &[],
        cannot_succeed: &[(
            "/b/auth/oauth/callback",
            "its success exchanges the provider's one-time code with the provider over the \
             network, which no fixture can answer",
        )],
        must_fire: &[],
    }
}

/// The admin settings page as the admin, the account pages as a signed-in
/// user, and the sign-in pages as a visitor — a signed-in visitor is sent on
/// from those.
fn caller(action: &str, path: &str) -> Message {
    if path.starts_with("/b/auth/admin/") {
        admin_msg(action, path)
    } else if matches!(
        path,
        "/b/auth/change-password" | "/b/auth/orgs" | "/b/auth/api/me" | "/b/auth/api/api-keys"
    ) {
        auth_msg(action, path, USER)
    } else {
        anon_msg(action, path)
    }
}

fn fixture() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture>>> {
    Box::pin(async {
        let mut ctx = TestContext::with_auth_and_crypto().await;
        // One provider configured, so the OAuth start hands off to it.
        ctx.set_config(crate::config_vars::ENABLE_OAUTH_KEY, "true");
        ctx.set_config(
            crate::blocks::auth_ui::OAUTH_GOOGLE_CLIENT_ID_KEY,
            "htmx-guard-client",
        );
        let ctx = ctx;
        ctx.seed_auth_user(USER).await;
        ctx.seed_auth_user(UNVERIFIED).await;
        users::set_verification_token(
            &ctx,
            UNVERIFIED,
            &sha256_hex(VERIFY_TOKEN.as_bytes()),
            "2026-01-01T00:00:00Z",
        )
        .await
        .expect("seed the verification token");
        Fixture {
            ctx,
            site: Site(vec![Arc::new(AuthUiBlock::new()) as Arc<dyn Block>]),
            caller,
            pages: vec![
                Page::at("/b/auth/login"),
                Page::at("/b/auth/signup"),
                Page::at("/b/auth/reset-password"),
                Page::at("/b/auth/bootstrap"),
                Page::at("/b/auth/change-password"),
                Page::at("/b/auth/orgs"),
                Page::at("/b/auth/admin/settings"),
                Page::at("/b/auth/api/verify").with("token", VERIFY_TOKEN),
            ],
            probes: vec![(
                "/b/auth/oauth/login",
                "/b/auth/oauth/login?provider=google".to_string(),
            )],
            operator_input: &[],
        }
    })
}
