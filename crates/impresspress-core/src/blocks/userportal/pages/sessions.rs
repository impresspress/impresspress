//! `/b/userportal/sessions` — list the caller's signed-in devices and revoke
//! individual ones.
//!
//! [B12] A row is a login family, so the list has one entry per device rather
//! than one per access token the device was ever issued, and revoking an entry
//! actually signs that device out: the revoke burns the refresh family in
//! `tokens` before deleting the row. Before this it removed the list entry and
//! nothing else, so the device kept refreshing and simply re-appeared.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        auth::repo::{sessions, tokens},
        crud,
    },
    crypto::META_AUTH_FAMILY,
    http::{redirect, ResponseBuilder},
    ui::{
        components::{self, badge, BadgeVariant},
        SiteConfig,
    },
};

pub async fn sessions_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    // An unreadable list is an error page, not "No active sessions.": this is
    // where a user looks for a device to sign out, and an empty list tells
    // them there is nothing to revoke.
    let rows = match sessions::list_for_user(ctx, &user_id).await {
        Ok(r) => r,
        Err(e) => return crud::db_error_page(msg, e, "userportal sessions: list_for_user failed"),
    };

    let current_family = current_session_family(msg);

    let body = html! {
        p .text-muted .m-0 .mb-4 .text-sm {
            "Sessions signed in to your account. Revoke any you don't recognize."
        }
        (render_table(&rows, current_family))
    };

    let config = match SiteConfig::load(ctx).await {
        Ok(site) => site,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: site config read failed")
        }
    };
    super::account_page(&config, "Sessions", Some("/b/userportal/"), body)
}

/// The login family the request's own access token belongs to, or `None` when
/// the request carries no verified token. Used to mark one row with a
/// "Current session" badge.
///
/// Read from `auth.family` meta, which `crypto::extract_auth_meta` sets only
/// from a token it accepted. Taking the family off the request's own cookie
/// instead would let any caller paint the badge on another user's row — a UX
/// signal, but one worth not letting a stranger forge.
fn current_session_family(msg: &Message) -> Option<&str> {
    let family = msg.get_meta(META_AUTH_FAMILY);
    (!family.is_empty()).then_some(family)
}

fn render_table(rows: &[sessions::SessionRow], current_family: Option<&str>) -> Markup {
    if rows.is_empty() {
        return html! {
            div .empty-state { p { "No active sessions." } }
        };
    }
    html! {
        table .data-table {
            thead {
                tr {
                    th { "Started" }
                    th { "Last used" }
                    th { "Expires" }
                    th { "" }
                }
            }
            tbody {
                @for r in rows {
                    @let is_current = current_family == Some(r.family.as_str());
                    tr .session-row {
                        // Timestamps render as semantic <time> elements —
                        // correct HTML for datetimes, and the visual-baseline
                        // suite masks `time` so per-run session times don't
                        // make the screenshots unreproducible.
                        td data-label="Started" {
                            (components::timestamp(&r.created_at))
                            @if is_current {
                                " "
                                (badge(BadgeVariant::Success, "Current session"))
                            }
                        }
                        td data-label="Last used" { (components::timestamp(&r.last_used_at)) }
                        td data-label="Expires" { (components::timestamp(&r.expires_at)) }
                        td data-label="" {
                            button .btn .btn--ghost .btn--sm
                                hx-delete=(format!("/b/userportal/sessions/{}", r.family))
                                hx-target="closest tr"
                                hx-swap="outerHTML"
                            { "Revoke" }
                        }
                    }
                }
            }
        }
    }
}

/// DELETE `/b/userportal/sessions/{family}` — sign one device out.
///
/// [B12] Three steps, in this order and all of them load-bearing:
///
/// 1. Resolve the family *scoped to the caller*. `tokens::revoke_family` takes
///    a family and no user, so ownership has to be established before it is
///    called. A family that is not the caller's looks exactly like one that
///    does not exist (200, no body — htmx removes the row either way), so the
///    response never reveals which.
/// 2. Revoke every refresh row in the family. This is what actually signs the
///    device out; skipping it (what this handler used to do) removed the list
///    entry while the device kept rotating tokens and re-appeared on the next
///    refresh.
/// 3. Delete the session row.
///
/// Steps 2 and 3 propagate their errors (a WRAP denial as 403, a quota as
/// 429, anything else as 500). "Revoked" that silently did
/// not revoke is the failure mode this whole change exists to remove, so a
/// user must not be told a device is signed out when it is not. Returns 401 if
/// anonymous, 400 if the bound `{family}` is missing.
pub async fn handle_revoke(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return ResponseBuilder::new()
            .status(401)
            .set_header("WWW-Authenticate", crate::http::WWW_AUTHENTICATE)
            .body(b"unauthenticated".to_vec(), "text/plain");
    }
    let family = msg.var("family").to_string();
    if family.is_empty() {
        return ResponseBuilder::new()
            .status(400)
            .body(b"bad family".to_vec(), "text/plain");
    }

    match sessions::find_for_user(ctx, &user_id, &family).await {
        Ok(Some(_)) => {}
        // No such session for this caller — indistinguishable from someone
        // else's, on purpose.
        Ok(None) => {
            return ResponseBuilder::new()
                .status(200)
                .body(Vec::new(), "text/html")
        }
        Err(e) => return crud::db_error_internal(e, "Could not look up the session"),
    }

    if let Err(e) = tokens::revoke_family(ctx, &family).await {
        return crud::db_error_internal(e, "Could not revoke the session");
    }
    if let Err(e) = sessions::delete(ctx, &family).await {
        return crud::db_error_internal(e, "Could not remove the session");
    }

    // Empty 200 — htmx swaps the row out via outerHTML.
    ResponseBuilder::new()
        .status(200)
        .body(Vec::new(), "text/html")
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        blocks::{
            auth::repo::sessions::{insert, NewSession},
            userportal::test_support::routed,
        },
        crypto::META_AUTH_FAMILY,
        test_support::{anon_msg, auth_msg, output_html, output_status, TestContext},
    };

    /// Present a login family on the message the way `extract_auth_meta` does
    /// after verifying the request's access token.
    fn with_family(mut msg: wafer_run::Message, family: &str) -> wafer_run::Message {
        msg.set_meta(META_AUTH_FAMILY, family);
        msg
    }

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    fn fake_session(user_id: &str, family: &str) -> NewSession {
        NewSession {
            family: family.into(),
            user_id: user_id.into(),
            auth_method: "password".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        }
    }

    /// A live refresh row in `family`, so the revoke path has something to
    /// burn and the test can prove it burned it.
    async fn seed_refresh_row(ctx: &TestContext, user_id: &str, family: &str) {
        tokens::insert(ctx, user_id, family, family, 0, "2099-01-01T00:00:00Z")
            .await
            .expect("seed refresh row");
    }

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        let msg = anon_msg("retrieve", "/b/userportal/sessions");
        let resp = sessions_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn empty_renders_empty_state() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/sessions", "user-a");
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("No active sessions"));
    }

    #[tokio::test]
    async fn populated_renders_one_row_per_session_with_revoke() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();
        insert(&ctx, fake_session("user-a", "fam-2")).await.unwrap();

        let msg = auth_msg("retrieve", "/b/userportal/sessions", "user-a");
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;

        assert!(html.contains("Sessions"), "missing page title");
        // Two Revoke buttons (one per row).
        assert!(
            html.matches(">Revoke<").count() >= 2,
            "expected \u{2265}2 Revoke buttons, got: {}",
            html.matches(">Revoke<").count()
        );
        // The revoke control addresses the family, which is what the route
        // binds and what `tokens::revoke_family` takes.
        assert!(
            html.contains("/b/userportal/sessions/fam-1"),
            "the revoke URL must carry the family: {html}"
        );
    }

    #[tokio::test]
    async fn revoke_anonymous_returns_401() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        let msg = routed(anon_msg("delete", "/b/userportal/sessions/fam-1"));
        let resp = handle_revoke(&ctx, &msg).await;
        let parts = wafer_block::http_codec::collect_http_response(resp).await;
        assert_eq!(parts.status, 401);
        assert!(
            parts.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("WWW-Authenticate")
                    && value == crate::http::WWW_AUTHENTICATE
            }),
            "{:?}",
            parts.headers
        );
    }

    /// [B12] The point of the change: revoking a device signs it out. The
    /// family's refresh rows are revoked *and* the row is removed; before
    /// this the handler removed the row and the device simply re-appeared on
    /// its next rotation.
    #[tokio::test]
    async fn revoke_own_session_revokes_the_family_and_deletes_the_row() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();
        seed_refresh_row(&ctx, "user-a", "fam-1").await;
        assert!(tokens::family_has_live_row(&ctx, "fam-1").await.unwrap());

        let msg = routed(auth_msg("delete", "/b/userportal/sessions/fam-1", "user-a"));
        let resp = handle_revoke(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 200);

        assert!(
            !tokens::family_has_live_row(&ctx, "fam-1").await.unwrap(),
            "revoking a device must burn its refresh family, not just the list entry"
        );
        assert!(sessions::list_for_user(&ctx, "user-a")
            .await
            .unwrap()
            .is_empty());
    }

    /// `handle_revoke` reads `{family}` only as the route table bound it: the
    /// same message is refused unrouted and revokes the session once it has
    /// been through `ROUTES`.
    #[tokio::test]
    async fn revoke_reads_only_the_bound_family() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();
        let path = "/b/userportal/sessions/fam-1";

        let unrouted = handle_revoke(&ctx, &auth_msg("delete", path, "user-a")).await;
        assert_eq!(
            output_status(unrouted).await,
            400,
            "nothing bound means nothing to revoke"
        );
        assert_eq!(
            sessions::list_for_user(&ctx, "user-a").await.unwrap().len(),
            1
        );

        let through_table = handle_revoke(&ctx, &routed(auth_msg("delete", path, "user-a"))).await;
        assert_eq!(output_status(through_table).await, 200);
        assert!(sessions::list_for_user(&ctx, "user-a")
            .await
            .unwrap()
            .is_empty());
    }

    /// Another user's family is not revocable, and the response does not say
    /// so — it is indistinguishable from a family that does not exist. This
    /// is the ownership gate `tokens::revoke_family`, which takes no user id,
    /// cannot provide for itself.
    #[tokio::test]
    async fn revoke_other_users_session_is_a_no_op_returning_200() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        for u in ["user-a", "user-b"] {
            seed_user(&ctx, u).await;
        }
        insert(&ctx, fake_session("user-b", "fam-b")).await.unwrap();
        seed_refresh_row(&ctx, "user-b", "fam-b").await;

        let msg = routed(auth_msg("delete", "/b/userportal/sessions/fam-b", "user-a"));
        let resp = handle_revoke(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 200);

        assert_eq!(
            sessions::list_for_user(&ctx, "user-b").await.unwrap().len(),
            1,
            "user-b's device is still listed"
        );
        assert!(
            tokens::family_has_live_row(&ctx, "fam-b").await.unwrap(),
            "and still signed in — a stranger cannot revoke it"
        );
    }

    /// A failed revoke is reported as one. Answering 200 while the family
    /// stayed live is exactly the lie this change removes.
    #[tokio::test]
    async fn a_failed_family_revoke_is_a_500_not_a_silent_success() {
        use crate::test_support::{output_is_error, FailingDbOpContext};

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();
        seed_refresh_row(&ctx, "user-a", "fam-1").await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.update_where", tokens::TABLE)]);

        let msg = routed(auth_msg("delete", "/b/userportal/sessions/fam-1", "user-a"));
        let resp = handle_revoke(&failing, &msg).await;
        assert!(output_is_error(resp, "Internal").await);
        assert!(
            tokens::family_has_live_row(&failing, "fam-1")
                .await
                .unwrap(),
            "precondition for the assertion above: the family really is still live"
        );
    }

    /// An unreadable session list is the 500 page, not "No active
    /// sessions." — the empty state tells a user looking for a device to
    /// sign out that there is nothing to revoke.
    #[tokio::test]
    async fn a_failed_list_read_is_a_500_not_the_empty_state() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();
        let ctx = ctx.break_reads();

        let (status, html) = crate::blocks::userportal::test_support::browser_request(
            &ctx,
            auth_msg("retrieve", "/b/userportal/sessions", "user-a"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(!html.contains("No active sessions"), "{html}");
    }

    /// The row whose family matches the request's *verified* token gets the
    /// badge. Other rows do not.
    #[tokio::test]
    async fn current_session_row_gets_badge() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-here"))
            .await
            .unwrap();
        insert(&ctx, fake_session("user-a", "fam-elsewhere"))
            .await
            .unwrap();

        let msg = with_family(
            auth_msg("retrieve", "/b/userportal/sessions", "user-a"),
            "fam-here",
        );
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;

        assert_eq!(
            html.matches("Current session").count(),
            1,
            "expected exactly one 'Current session' badge, got: {}",
            html.matches("Current session").count()
        );
        assert!(
            html.contains("badge-success"),
            "expected success-variant badge class in HTML"
        );
    }

    /// No verified family on the request means no badge, but the list still
    /// renders. Guards the "page must not crash for a caller whose token
    /// predates the `family` claim" requirement.
    #[tokio::test]
    async fn no_family_meta_renders_no_badge() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();

        let msg = auth_msg("retrieve", "/b/userportal/sessions", "user-a");
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;

        assert!(
            !html.contains("Current session"),
            "no badge expected when the request carries no verified family"
        );
        assert!(html.contains(">Revoke<"), "row body still present");
    }

    /// A family that matches none of the caller's rows produces no badge —
    /// the case where a device was revoked in another tab while this one's
    /// token is still live.
    #[tokio::test]
    async fn a_family_with_no_matching_row_renders_no_badge() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();

        let msg = with_family(
            auth_msg("retrieve", "/b/userportal/sessions", "user-a"),
            "fam-unrelated",
        );
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;

        assert!(
            !html.contains("Current session"),
            "no badge expected when the family matches no row"
        );
    }

    // --- WRAP regression: catches a future removal of the userportal
    // grant on `auth::repo::sessions::TABLE`. Without it, /b/userportal/
    // sessions answers the 500 page for every authenticated user. PR #77
    // added the grant; the `wrap_allows_*` tests fail closed if it's
    // removed, and the denial below shows the read needs one.

    #[tokio::test]
    async fn wrap_denies_sessions_list_to_a_block_without_a_grant() {
        // Seeded from the fixture's own frame — `seed_user` uses raw SQL,
        // which WRAP restricts to the admin block. In production, rows are
        // seeded by owner/admin paths; userportal only reads them.
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();

        let ctx = ctx.running_as("test/ungranted");

        let err = sessions::list_for_user(&ctx, "user-a")
            .await
            .expect_err("WRAP must deny list_for_user without grant");
        assert!(
            format!("{err:?}").contains("WRAP"),
            "error must mention WRAP, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn wrap_allows_sessions_list_with_auth_block_grants() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();

        let ctx = ctx.running_as("impresspress/userportal");

        let rows = sessions::list_for_user(&ctx, "user-a")
            .await
            .expect("auth's production grants must cover userportal sessions read");
        assert_eq!(rows.len(), 1);
    }

    /// The revoke path now writes to `tokens` as well as `sessions`, so
    /// auth's grant list has to cover both for the userportal — a missing
    /// `tokens` grant would turn every revoke into a 500.
    #[tokio::test]
    async fn wrap_allows_the_whole_revoke_path_with_auth_block_grants() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();
        seed_refresh_row(&ctx, "user-a", "fam-1").await;

        let ctx = ctx.running_as("impresspress/userportal");

        let msg = routed(auth_msg("delete", "/b/userportal/sessions/fam-1", "user-a"));
        assert_eq!(output_status(handle_revoke(&ctx, &msg).await).await, 200);
        assert!(!tokens::family_has_live_row(&ctx, "fam-1").await.unwrap());
    }
}
