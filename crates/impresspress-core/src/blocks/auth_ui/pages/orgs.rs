//! `/b/auth/orgs` — read-only list of orgs claimed by the current user, in
//! the portal shell with the userportal's other account pages.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{auth::repo::orgs, crud},
    http::redirect,
    ui::{
        self,
        components::{timestamp, DataTable, TableCol, TableRow},
        Shell,
    },
};

/// The orgs table: the org's name is the row (the card title on a phone).
const COLUMNS: [TableCol<'static>; 3] = [
    TableCol::new("Name").primary(),
    TableCol::new("Verified via"),
    TableCol::new("Claimed"),
];

/// GET `/b/auth/orgs`. Anonymous users redirected to login.
pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    // A failed read is an error page, never the "no claimed organizations"
    // copy: that would tell a user who owns orgs that they own none.
    let orgs_list = match orgs::list_for_user(ctx, &user_id).await {
        Ok(list) => list,
        Err(e) => return crud::db_error_page(msg, e, "orgs page: list_for_user failed"),
    };

    ui::shell_page(
        ctx,
        msg,
        Shell::portal("Organizations", "Organizations")
            .subtitle("Orgs you've claimed via GitHub, Google, or Microsoft sign-in."),
        render_orgs_body(&orgs_list),
    )
    .await
}

fn render_orgs_body(orgs: &[orgs::OrgRow]) -> Markup {
    let rows = orgs
        .iter()
        .map(|o| {
            TableRow::new(vec![
                html! { (o.name) },
                html! { (o.verified_via.as_deref().unwrap_or("manual")) },
                timestamp(&o.created_at),
            ])
        })
        .collect();
    DataTable::new(&COLUMNS)
        .rows(rows)
        .empty_state(
            "No claimed organizations",
            "Sign in with GitHub, Google, or Microsoft to claim one.",
            None,
        )
        .render()
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        blocks::auth::repo::orgs::fixtures::seed_claimed_org,
        test_support::{
            anon_msg, auth_msg, output_header, output_html, output_status, TestContext,
        },
    };

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = anon_msg("retrieve", "/b/auth/orgs");
        let resp = handle(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn anonymous_redirect_sets_location() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = anon_msg("retrieve", "/b/auth/orgs");
        let resp = handle(&ctx, &msg).await;
        assert_eq!(
            output_header(resp, "Location").await.as_deref(),
            Some("/b/auth/login")
        );
    }

    #[tokio::test]
    async fn empty_renders_empty_state_copy() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/auth/orgs", "user-a");
        let resp = handle(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("No claimed organizations"));
        assert!(html.contains("Sign in with GitHub"));
        assert!(
            html.contains(r#"class="empty__title""#),
            "the empty list is the shared empty state: {html}"
        );
        assert!(
            html.contains(r#"<nav class="sidebar""#),
            "portal shell: {html}"
        );
        assert!(
            html.contains(r#"<h1 class="topbar__title">Organizations</h1>"#),
            "{html}"
        );
    }

    #[tokio::test]
    async fn populated_renders_one_row_per_org() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        seed_claimed_org(
            &ctx,
            "alpha",
            "user-a",
            "github",
            "gh-1",
            "2026-01-01T00:00:00Z",
        )
        .await;
        seed_claimed_org(
            &ctx,
            "beta",
            "user-a",
            "google",
            "gg-2",
            "2026-01-02T00:00:00Z",
        )
        .await;

        let msg = auth_msg("retrieve", "/b/auth/orgs", "user-a");
        let resp = handle(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("alpha"));
        assert!(html.contains("beta"));
        assert!(html.contains("github"));
        assert!(html.contains("google"));
    }

    #[tokio::test]
    async fn a_failed_read_is_a_500_not_the_empty_state() {
        use wafer_run::Block;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        let ctx = ctx.break_reads();
        let mut msg = auth_msg("retrieve", "/b/auth/orgs", "user-a");
        msg.set_meta("http.header.accept", "text/html");
        // Through the block's router, the path a browser request takes.
        let resp = crate::blocks::auth_ui::AuthUiBlock::default()
            .handle(&ctx, msg, wafer_run::InputStream::empty())
            .await;
        let parts = wafer_block::http_codec::collect_http_response(resp).await;
        let (status, html) = (parts.status, String::from_utf8_lossy(&parts.body));
        assert_eq!(status, 500, "a failed read must not render a page: {html}");
        assert!(
            !html.contains("No claimed organizations"),
            "a failed read must not claim the user owns no orgs: {html}"
        );
    }
}
