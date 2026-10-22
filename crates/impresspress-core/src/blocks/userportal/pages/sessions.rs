//! `/b/userportal/sessions` — list the caller's signed-in devices and revoke
//! individual ones.
//!
//! [B12] A row is a login family, so the list has one entry per device rather
//! than one per access token the device was ever issued, and revoking an entry
//! actually signs that device out: the row is the family's liveness, and the
//! revoke deletes it and then burns the refresh family in `tokens`
//! ([`handle_revoke`] says why in that order). Before B12 it removed the list
//! entry and nothing else, so the device kept refreshing and simply
//! re-appeared.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        auth::{
            helpers::{clear_auth_cookie, end_family, end_presented_access_token},
            repo::sessions,
        },
        auth_ui::oauth,
        crud,
    },
    crypto::META_AUTH_FAMILY,
    http::{redirect, ResponseBuilder},
    ui::{
        self,
        components::{self, badge, BadgeVariant, DataTable, TableCol, TableRow},
        Shell,
    },
};

/// The sessions table: the sign-in method names the row (the card title on a
/// phone), the three dates follow, and Revoke sits under a screen-reader-only
/// "Actions" header. Narrow, the dates move into one line under the method
/// (table.css `.session-table`, which hides headers and cells by their label: a
/// column renamed here is renamed there).
const COLUMNS: [TableCol<'static>; 5] = [
    TableCol::new("Signed in with").primary(),
    TableCol::new("Started"),
    TableCol::new("Last used"),
    TableCol::new("Expires"),
    TableCol::new("Actions").actions(),
];

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

    ui::shell_page(
        ctx,
        msg,
        Shell::portal("Sessions", "Sessions")
            .subtitle("Devices signed in to your account. Revoke any you don't recognize."),
        render_table(&rows, current_family),
    )
    .await
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

/// How a session was established (`SessionRow::auth_method`), as a person
/// reads it: `password`, `oauth.<provider>` or `bootstrap`.
fn sign_in_method(auth_method: &str) -> String {
    match auth_method {
        "password" => "Password".to_string(),
        "bootstrap" => "Setup link".to_string(),
        other => match other.strip_prefix("oauth.") {
            Some(provider) => oauth::spec::label(provider).to_string(),
            None => other.to_string(),
        },
    }
}

fn render_table(rows: &[sessions::SessionRow], current_family: Option<&str>) -> Markup {
    let table_rows = rows
        .iter()
        .map(|r| {
            let is_current = current_family == Some(r.family.as_str());
            // Revoking the session this page was loaded with signs the
            // reader out, which the confirmation says, and its button too.
            let (confirm, confirm_label) = if is_current {
                (
                    "Revoke your current session? You will be signed out on this device.",
                    "Revoke and sign out",
                )
            } else {
                (
                    "Revoke this session? That device will be signed out.",
                    "Revoke session",
                )
            };
            TableRow::new(vec![
                html! {
                    span .data-table__title id=(format!("session-{}", r.family)) { (sign_in_method(&r.auth_method)) }
                    @if is_current {
                        " "
                        (badge(BadgeVariant::Success, "Current session"))
                    }
                    // The three date columns, when the table is too narrow
                    // for them. Each separator ends the pair before it, so a
                    // line never starts with one.
                    span .session-table__meta {
                        span { "Last used " (components::timestamp(&r.last_used_at)) " ·" } " "
                        span { "Started " (components::timestamp(&r.created_at)) " ·" } " "
                        span { "Expires " (components::timestamp(&r.expires_at)) }
                    }
                },
                // Timestamps render as `components::timestamp`'s `<time>`,
                // which the visual-baseline suite masks, so per-run session
                // times don't make the screenshots unreproducible.
                components::timestamp(&r.created_at),
                html! { span id=(format!("last-used-{}", r.family)) { (components::timestamp(&r.last_used_at)) } },
                components::timestamp(&r.expires_at),
                // Quiet in the row: the confirmation is the guard, and a
                // filled danger button on every row shouted over the list.
                // Named by its session ("Revoke Password 2026-10-22 09:14
                // UTC"), so a list of them is not six bare "Revoke"s.
                html! {
                    button .btn .btn--quiet-danger .btn--sm
                        type="button"
                        id=(format!("revoke-{}", r.family))
                        aria-labelledby=(format!("revoke-{0} session-{0} last-used-{0}", r.family))
                        hx-delete=(format!("/b/userportal/sessions/{}", r.family))
                        hx-target="closest tr"
                        hx-swap="outerHTML"
                        hx-confirm=(confirm)
                        data-confirm-label=(confirm_label)
                        data-confirm-tone="danger"
                    { "Revoke" }
                },
            ])
        })
        .collect();
    let table = DataTable::new(&COLUMNS)
        .rows(table_rows)
        .empty_state(
            "No active sessions",
            "Every device you sign in on is listed here.",
            None,
        )
        .render();
    html! { div .session-table { (table) } }
}

/// DELETE `/b/userportal/sessions/{family}` — sign one device out.
///
/// [B12] These steps, in this order and all of them load-bearing:
///
/// 1. Resolve the family *scoped to the caller*. `tokens::revoke_family` takes
///    a family and no user, so ownership has to be established before it is
///    called. A family that is not the caller's looks exactly like one that
///    does not exist (200, no body — htmx removes the row either way), so the
///    response never reveals which.
/// 2. Delete the session row. The row is the family's liveness
///    (`auth::repo::sessions`, module doc): from here on, a rotation of this
///    family finds no row to touch, revokes the family and refuses.
/// 3. Revoke every refresh row in the family, so the rows agree with the
///    liveness: a token the device still holds reads as revoked.
/// 4. When that was the session the request itself belongs to, end the access
///    token it presented and clear the cookie, and send the page to sign in.
///
/// Steps 2 and 3 are `auth::helpers::end_family`, which every revocation of a
/// family shares, and they must run in that order. A refresh rotates by
/// claiming its token, inserting the successor's refresh row and then
/// touching the session row (`auth::helpers::establish_family`). With the row
/// deleted first, every interleaving ends the family: a touch after the delete
/// finds nothing and the rotation revokes and refuses; a touch before it
/// means the successor row already existed when step 3 revoked the family.
/// The other order lets a rotation claim before step 2, insert after it and
/// touch before step 3, leaving a live successor row behind. No transaction
/// spans the two.
///
/// Steps 2 to 4 propagate their errors (a WRAP denial as 403, a quota as
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

    // Row first, refresh rows second: see the order note in the doc above.
    if let Err(e) = end_family(ctx, &family).await {
        return crud::db_error_internal(e, "Could not revoke the session");
    }

    // Revoking the session this request belongs to signs the reader out
    // here and now, as the confirmation said: burning the refresh family
    // alone left the access token in the cookie working until its expiry.
    // So that token is ended too, the cookie cleared, and the page sent to
    // sign in (`HX-Redirect`, a full navigation).
    if current_session_family(msg) == Some(family.as_str()) {
        if let Err(e) = end_presented_access_token(ctx, msg).await {
            return crud::db_error_internal(e, "Could not end the current session");
        }
        let cookie = match clear_auth_cookie(ctx).await {
            Ok(cookie) => cookie,
            Err(e) => return crud::db_error_internal(e, "Could not clear the session cookie"),
        };
        return ResponseBuilder::new()
            .status(200)
            .set_cookie(&cookie)
            .set_header("HX-Redirect", "/b/auth/login")
            .body(Vec::new(), "text/html");
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
            auth::repo::{
                sessions::{insert, NewSession},
                tokens,
            },
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

    /// A session started through a provider names it as people read it.
    #[test]
    fn sign_in_method_names_the_oauth_provider() {
        assert_eq!(super::sign_in_method("oauth.github"), "GitHub");
        assert_eq!(super::sign_in_method("oauth.microsoft"), "Microsoft");
        assert_eq!(super::sign_in_method("oauth.unknown"), "unknown");
        assert_eq!(super::sign_in_method("password"), "Password");
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

    /// The list is the shared table: an `.data-table` wrapper, a
    /// screen-reader-only header over the Revoke column (axe
    /// `empty-table-header`), dates through `components::timestamp`, and
    /// Revoke as a quiet danger action behind a confirmation — worded for
    /// the session the page itself is using, which revoking signs out.
    #[tokio::test]
    async fn the_list_is_the_shared_table_with_a_confirmed_quiet_danger_revoke() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-here"))
            .await
            .unwrap();
        insert(&ctx, fake_session("user-a", "fam-other"))
            .await
            .unwrap();

        let msg = with_family(
            auth_msg("retrieve", "/b/userportal/sessions", "user-a"),
            "fam-here",
        );
        let html = output_html(sessions_page(&ctx, &msg).await).await;

        assert!(html.contains(r#"<div class="data-table">"#), "{html}");
        assert!(
            html.contains(r#"<th data-label="Actions"><span class="sr-only">Actions</span></th>"#),
            "the actions column has a screen-reader header: {html}"
        );
        assert!(!html.contains("<th></th>"), "no empty header: {html}");
        assert!(html.contains(r#"<time class="datetime""#), "{html}");
        assert!(
            html.contains(">Password<"),
            "the sign-in method names the row: {html}"
        );
        // Quiet in the row: the confirmation is the guard.
        assert_eq!(
            html.matches(r#"class="btn btn--quiet-danger btn--sm""#)
                .count(),
            2,
            "{html}"
        );
        // The only danger button is the shared confirm dialog's.
        let (list, _) = html
            .split_once(r#"<dialog class="modal" id="confirm-dialog""#)
            .expect("the page carries the shared confirm dialog");
        assert!(!list.contains("btn--danger"), "{html}");
        // Each Revoke is named by its session: its own label, the method
        // and when the device was last used.
        assert!(
            html.contains(
                r#"id="revoke-fam-here" aria-labelledby="revoke-fam-here session-fam-here last-used-fam-here""#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<span class="data-table__title" id="session-fam-here">Password</span>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<span id="last-used-fam-here"><time"#),
            "{html}"
        );
        // Narrow, the three dates are one line under the method, each
        // beside its label.
        assert!(
            html.contains(
                r#"<span class="session-table__meta"><span>Last used <time class="datetime""#
            ) && html.contains(r#" UTC</time> ·</span> <span>Started <time"#),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"hx-confirm="Revoke your current session?"#)
                .count(),
            1,
            "{html}"
        );
        assert_eq!(
            html.matches(r#"hx-confirm="Revoke this session?"#).count(),
            1,
            "{html}"
        );
        // Asked in the shared confirm dialog, in danger red, on a button
        // that says what it does.
        assert!(
            html.contains(
                r#"signed out on this device." data-confirm-label="Revoke and sign out" data-confirm-tone="danger">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"That device will be signed out." data-confirm-label="Revoke session" data-confirm-tone="danger">"#
            ),
            "{html}"
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

    /// Revoking the session the request belongs to signs the reader out at
    /// once, as the confirmation says: the presented access token is
    /// blocklisted (so the next request with it is refused), the cookie is
    /// cleared, and the page is sent to sign in. Run as the userportal, so
    /// the blocklist write is proved to be within its grants.
    #[tokio::test]
    async fn revoking_the_current_session_ends_the_presented_token() {
        use crate::{
            blocks::auth::repo::jwt_blocklist,
            crypto::{META_AUTH_EXP, META_AUTH_JTI},
        };

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-here"))
            .await
            .unwrap();
        seed_refresh_row(&ctx, "user-a", "fam-here").await;

        let mut msg = with_family(
            routed(auth_msg(
                "delete",
                "/b/userportal/sessions/fam-here",
                "user-a",
            )),
            "fam-here",
        );
        msg.set_meta(META_AUTH_JTI, "jti-here");
        msg.set_meta(META_AUTH_EXP, "4102444800");
        let parts =
            wafer_block::http_codec::collect_http_response(handle_revoke(&ctx, &msg).await).await;

        assert_eq!(parts.status, 200);
        let header = |name: &str| {
            parts
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        assert_eq!(header("HX-Redirect").as_deref(), Some("/b/auth/login"));
        let cookie = header("Set-Cookie").expect("the session cookie is cleared");
        assert!(
            cookie.starts_with("auth_token=;") && cookie.contains("Max-Age=0"),
            "{cookie}"
        );
        assert!(
            jwt_blocklist::contains(&ctx.fixture(), "jti-here")
                .await
                .unwrap(),
            "the presented access token must stop working now, not at its expiry"
        );
        assert!(!tokens::family_has_live_row(&ctx, "fam-here").await.unwrap());
    }

    /// Revoking ANOTHER device leaves the reader signed in: no blocklist
    /// row for their token, no cookie, no redirect.
    #[tokio::test]
    async fn revoking_another_session_leaves_the_reader_signed_in() {
        use crate::{
            blocks::auth::repo::jwt_blocklist,
            crypto::{META_AUTH_EXP, META_AUTH_JTI},
        };

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::userportal::UserPortalBlock::BLOCK_NAME);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-other"))
            .await
            .unwrap();

        let mut msg = with_family(
            routed(auth_msg(
                "delete",
                "/b/userportal/sessions/fam-other",
                "user-a",
            )),
            "fam-here",
        );
        msg.set_meta(META_AUTH_JTI, "jti-here");
        msg.set_meta(META_AUTH_EXP, "4102444800");
        let parts =
            wafer_block::http_codec::collect_http_response(handle_revoke(&ctx, &msg).await).await;

        assert_eq!(parts.status, 200);
        assert!(
            !parts.headers.iter().any(|(n, _)| {
                n.eq_ignore_ascii_case("HX-Redirect") || n.eq_ignore_ascii_case("Set-Cookie")
            }),
            "{:?}",
            parts.headers
        );
        assert!(!jwt_blocklist::contains(&ctx.fixture(), "jti-here")
            .await
            .unwrap());
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

    /// A device revocation racing that device's own token refresh, driven
    /// through the real handlers on both sides: two sign-ins through
    /// `login::handle`, the refresh through `refresh::handle`, and the revoke
    /// through [`handle_revoke`] running as the userportal. The interleaving is
    /// forced with [`AfterDbOpContext`], never mocked.
    mod revoke_racing_a_refresh {
        use super::*;
        use crate::{
            blocks::{
                auth::repo::users::InitialRole,
                auth_ui::api::{login, refresh},
                userportal::UserPortalBlock,
            },
            test_support::{json_post_msg, output_is_error, output_json, AfterDbOpContext},
        };

        const EMAIL: &str = "two-devices@example.com";
        const PASSWORD: &str = "correct-horse-battery";

        struct Devices {
            ctx: TestContext,
            user_id: String,
            /// Device A's refresh token: the device that does the revoking.
            a_refresh: String,
            a_family: String,
            /// Device B's refresh token: the device being revoked.
            b_refresh: String,
            b_family: String,
        }

        fn json_input(value: serde_json::Value) -> wafer_run::InputStream {
            wafer_run::InputStream::from_bytes(value.to_string().into_bytes())
        }

        async fn sign_in(ctx: &TestContext) -> (String, String) {
            let resp = output_json(
                login::handle(
                    ctx,
                    &json_post_msg("/b/auth/api/login"),
                    json_input(serde_json::json!({"email": EMAIL, "password": PASSWORD})),
                )
                .await,
            )
            .await;
            let refresh_token = resp["refresh_token"]
                .as_str()
                .unwrap_or_else(|| panic!("the sign-in succeeds: {resp}"))
                .to_string();
            let family = tokens::find_by_token(ctx, &refresh_token)
                .await
                .expect("token lookup")
                .expect("the sign-in wrote its refresh row")
                .family;
            (refresh_token, family)
        }

        /// One account signed in on two devices, A and B.
        async fn two_devices() -> Devices {
            let ctx = TestContext::with_auth_and_crypto().await;
            let user_id = ctx.seed_account(EMAIL, PASSWORD, InitialRole::User).await;
            let (a_refresh, a_family) = sign_in(&ctx).await;
            let (b_refresh, b_family) = sign_in(&ctx).await;
            assert_ne!(a_family, b_family);
            Devices {
                ctx,
                user_id,
                a_refresh,
                a_family,
                b_refresh,
                b_family,
            }
        }

        /// Device A revoking device B through the sessions page, run as the
        /// userportal. Asserts the page answered 200.
        fn revoke_b(d: &Devices) -> impl std::future::Future<Output = ()> + Send + 'static {
            let ctx = d.ctx.clone().running_as(UserPortalBlock::BLOCK_NAME);
            let msg = with_family(
                routed(auth_msg(
                    "delete",
                    &format!("/b/userportal/sessions/{}", d.b_family),
                    &d.user_id,
                )),
                &d.a_family,
            );
            async move {
                assert_eq!(output_status(handle_revoke(&ctx, &msg).await).await, 200);
            }
        }

        async fn refresh_with(ctx: &dyn Context, token: &str) -> OutputStream {
            refresh::handle(
                ctx,
                &json_post_msg("/b/auth/api/refresh"),
                json_input(serde_json::json!({ "refresh_token": token })),
            )
            .await
        }

        async fn b_is_gone(d: &Devices) {
            assert!(
                !tokens::family_has_live_row(&d.ctx, &d.b_family)
                    .await
                    .unwrap(),
                "every refresh row of the revoked device is revoked"
            );
            assert!(
                sessions::find_for_user(&d.ctx, &d.user_id, &d.b_family)
                    .await
                    .unwrap()
                    .is_none(),
                "the revoked device is not listed again"
            );
            let a_refreshed = output_json(refresh_with(&d.ctx, &d.a_refresh).await).await;
            assert!(
                a_refreshed["refresh_token"].is_string(),
                "the device that revoked is untouched: {a_refreshed}"
            );
        }

        /// The whole revoke lands after the refresh claimed its token and
        /// before it wrote the successor's row, so the revoke's refresh-row
        /// revocation cannot reach the successor. The touch then finds no
        /// row, and the refresh revokes the family itself, the successor
        /// included, before it refuses. Left live, that row would read as the
        /// family's current generation to the replay check, which would
        /// report a replay of the old token as theft.
        #[tokio::test]
        async fn a_revoke_between_the_claim_and_the_successor_insert_revokes_the_successor() {
            let d = two_devices().await;
            let racing = AfterDbOpContext::new(
                d.ctx.clone(),
                "database.update_where_count",
                tokens::TABLE,
                revoke_b(&d),
            );

            let out = refresh_with(&racing, &d.b_refresh).await;
            assert!(racing.fired(), "the revoke must land inside the refresh");
            assert!(
                output_is_error(out, "Unauthenticated").await,
                "a refresh of a device revoked mid-rotation is refused"
            );
            b_is_gone(&d).await;
        }

        /// The revoke lands after the refresh wrote its successor's row and
        /// before it touched the session row. The touch finds no row, so the
        /// refresh revokes the family and is refused. It used to re-insert the
        /// row it found missing, which brought the revoked device back onto
        /// the list with a live successor token.
        #[tokio::test]
        async fn a_revoke_between_the_successor_insert_and_the_touch_ends_the_family() {
            let d = two_devices().await;
            let racing = AfterDbOpContext::new(
                d.ctx.clone(),
                "database.create",
                tokens::TABLE,
                revoke_b(&d),
            );

            let out = refresh_with(&racing, &d.b_refresh).await;
            assert!(racing.fired(), "the revoke must land inside the refresh");
            assert!(
                output_is_error(out, "Unauthenticated").await,
                "a refresh of a device revoked mid-rotation is refused"
            );
            b_is_gone(&d).await;
        }

        /// The revoke lands after the refresh touched the session row. The
        /// refresh has handed out a successor, but the revoke deletes the row
        /// and then revokes the family, the successor's row included, so the
        /// successor cannot refresh.
        #[tokio::test]
        async fn a_revoke_after_the_touch_revokes_the_successor() {
            let d = two_devices().await;
            let racing = AfterDbOpContext::new(
                d.ctx.clone(),
                "database.update_where_count",
                sessions::TABLE,
                revoke_b(&d),
            );

            let rotated = output_json(refresh_with(&racing, &d.b_refresh).await).await;
            assert!(racing.fired(), "the revoke must land inside the refresh");
            let successor = rotated["refresh_token"].as_str().unwrap_or_else(|| {
                panic!("the touch came first, so the refresh answered: {rotated}")
            });
            assert!(
                output_is_error(refresh_with(&d.ctx, successor).await, "Unauthenticated").await,
                "the successor of a revoked device cannot refresh"
            );
            b_is_gone(&d).await;
        }

        /// Device B's refresh, interleaved with `revoke` at the revoke's
        /// FIRST write: the refresh claims its token, then the revoke makes its
        /// first write (the session-row delete or the refresh-row revoke,
        /// whichever it makes first), then the refresh inserts its successor
        /// and touches the session row, then the revoke makes its second write.
        /// Returns the refresh's answer once the revoke has finished; the
        /// revoke must answer `revoke_status`.
        ///
        /// `revoke` is given the context to run on, paused at that point.
        async fn refresh_across_the_first_revoke_write<F>(
            d: &Devices,
            revoke_ctx: TestContext,
            revoke_status: u16,
            revoke: impl FnOnce(AfterDbOpContext) -> F,
        ) -> OutputStream
        where
            F: std::future::Future<Output = OutputStream> + Send + 'static,
        {
            let (first_write_done, first_write_seen) = tokio::sync::oneshot::channel::<()>();
            let (finish, finish_seen) = tokio::sync::oneshot::channel::<()>();
            let paused = AfterDbOpContext::new(
                revoke_ctx,
                "database.delete_where_count",
                sessions::TABLE,
                async move {
                    first_write_done.send(()).expect("the test is waiting");
                    finish_seen.await.expect("the test releases the revoke");
                },
            )
            .or_after("database.update_where", tokens::TABLE);
            let revoke = revoke(paused.clone());
            let task = std::sync::Arc::new(std::sync::Mutex::new(None));

            // The refresh starts the revoke once it has claimed its token, and
            // continues once the revoke's first write has landed.
            let started = task.clone();
            let racing = AfterDbOpContext::new(
                d.ctx.clone(),
                "database.update_where_count",
                tokens::TABLE,
                async move {
                    let revoke = tokio::spawn(async move { output_status(revoke.await).await });
                    *started.lock().unwrap() = Some(revoke);
                    first_write_seen
                        .await
                        .expect("the revoke made its first write");
                },
            );

            let out = refresh_with(&racing, &d.b_refresh).await;
            assert!(
                racing.fired() && paused.fired(),
                "both sides reached their points"
            );
            finish.send(()).expect("the revoke is paused");
            let revoke = task.lock().unwrap().take().expect("the revoke was started");
            assert_eq!(revoke.await.expect("the revoke ran"), revoke_status);
            out
        }

        /// The order the device revoke writes in.
        ///
        /// With the session row deleted first, the touch finds nothing and the
        /// refresh is refused. Revoking the refresh rows first instead lets
        /// the refresh's successor be written after the revocation and touch a
        /// row that still exists: it answers 200 and leaves a live successor
        /// row behind.
        #[tokio::test]
        async fn the_revoke_deletes_the_row_before_it_revokes_the_tokens() {
            let d = two_devices().await;
            let msg = with_family(
                routed(auth_msg(
                    "delete",
                    &format!("/b/userportal/sessions/{}", d.b_family),
                    &d.user_id,
                )),
                &d.a_family,
            );
            let out = refresh_across_the_first_revoke_write(
                &d,
                d.ctx.clone().running_as(UserPortalBlock::BLOCK_NAME),
                200,
                |ctx| async move { handle_revoke(&ctx, &msg).await },
            )
            .await;

            assert!(
                output_is_error(out, "Unauthenticated").await,
                "a refresh whose touch came after the revoke's first write is refused"
            );
            b_is_gone(&d).await;
        }

        /// Logout ends every device by the same rule, so it writes in the
        /// same order: every session row, then every refresh row.
        #[tokio::test]
        async fn logout_deletes_the_rows_before_it_revokes_the_tokens() {
            use crate::blocks::auth_ui::api::logout;

            let d = two_devices().await;
            let msg = auth_msg("update", "/b/auth/api/logout", &d.user_id);
            let out =
                refresh_across_the_first_revoke_write(&d, d.ctx.clone(), 303, |ctx| async move {
                    logout::handle(&ctx, &msg).await
                })
                .await;

            assert!(
                output_is_error(out, "Unauthenticated").await,
                "a refresh whose touch came after logout's first write is refused"
            );
            for family in [&d.a_family, &d.b_family] {
                assert!(
                    !tokens::family_has_live_row(&d.ctx, family).await.unwrap(),
                    "logout leaves no live refresh row in {family}"
                );
            }
            assert!(sessions::list_for_user(&d.ctx, &d.user_id)
                .await
                .unwrap()
                .is_empty());
        }
    }
}
