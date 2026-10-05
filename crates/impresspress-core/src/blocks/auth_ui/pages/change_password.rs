//! GET /b/auth/change-password — the address of the page that used to change
//! a password, kept so links and bookmarks to it still land somewhere.
//!
//! A signed-in account has ONE change-password form: the Password section of
//! the portal's Security page (`blocks/userportal/pages/security.rs`), inside
//! the portal shell with every other account page. This page was a second,
//! different form in the signed-out marketing frame (`auth_split`), with a
//! private script that enforced its own 8-character minimum instead of the
//! configured one. It now redirects there.

use wafer_run::OutputStream;

use crate::http::redirect;

/// Where a password is changed. The userportal block's Security page; spelled
/// out rather than imported because `impresspress/userportal` is a feature-gated
/// block and this one is not.
pub const SECURITY_PAGE: &str = "/b/userportal/security";

/// Redirect to [`SECURITY_PAGE`]. A signed-out caller is sent on by that page
/// to sign in, the same as for any other account page.
pub fn handle() -> OutputStream {
    redirect(302, SECURITY_PAGE)
}

#[cfg(test)]
mod tests {
    use wafer_run::{Block, InputStream};

    use crate::{
        blocks::auth_ui::{AuthUiBlock, AUTH_UI_BLOCK_ID},
        test_support::{anon_msg, auth_msg, output_header, output_status, TestContext},
    };

    /// Through the block's router, the path a browser's GET takes: signed in
    /// or not, the old address answers with the Security page's.
    #[tokio::test]
    async fn the_old_page_redirects_to_the_security_page() {
        let ctx = TestContext::with_auth().await.running_as(AUTH_UI_BLOCK_ID);
        ctx.seed_auth_user("user-a").await;
        for msg in [
            auth_msg("retrieve", "/b/auth/change-password", "user-a"),
            anon_msg("retrieve", "/b/auth/change-password"),
        ] {
            let block = AuthUiBlock::default();
            let status =
                output_status(block.handle(&ctx, msg.clone(), InputStream::empty()).await).await;
            assert_eq!(status, 302);
            let location = output_header(
                block.handle(&ctx, msg, InputStream::empty()).await,
                "Location",
            )
            .await;
            assert_eq!(location.as_deref(), Some(super::SECURITY_PAGE));
        }
    }
}
