//! `/b/userportal/sessions` — list active sessions, revoke individual ones.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::auth::{repo::sessions, service::hash_token},
    http::{redirect, ResponseBuilder},
    ui::{
        components::{badge, BadgeVariant},
        SiteConfig,
    },
    util::hex_encode,
};

pub async fn sessions_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    // DB errors are tracing::warn'd (per repo convention) and we render the
    // empty-state — the page is a UX surface, not a security gate.
    let rows = match sessions::list_for_user(ctx, &user_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(user_id = %user_id, "userportal sessions list_for_user failed: {e}");
            Vec::new()
        }
    };

    let current_hash = current_session_hash(msg);

    let body = html! {
        p .text-muted style="margin:0 0 1rem;font-size:0.875rem" {
            "Sessions signed in to your account. Revoke any you don't recognize."
        }
        (render_table(&rows, current_hash.as_deref()))
    };

    let config = SiteConfig::load(ctx).await;
    super::account_page(&config, "Sessions", Some("/b/userportal/"), body)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Compute the SHA-256 hash of the request's `auth_token` cookie (the JWT
/// issued at login), or `None` if no cookie is present. Used to mark the row
/// matching the current request with a "Current session" badge — a UX
/// signal, not a security gate.
fn current_session_hash(msg: &Message) -> Option<Vec<u8>> {
    let cookie = msg.cookie("auth_token");
    if cookie.is_empty() {
        return None;
    }
    Some(hash_token(cookie))
}

fn render_table(rows: &[sessions::SessionRow], current_hash: Option<&[u8]>) -> Markup {
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
                    @let is_current = current_hash.is_some_and(|h| h == r.token_hash.as_slice());
                    tr .session-row {
                        td data-label="Started" {
                            (r.created_at)
                            @if is_current {
                                " "
                                (badge(BadgeVariant::Success, "Current session"))
                            }
                        }
                        td data-label="Last used" { (r.last_used_at) }
                        td data-label="Expires" { (r.expires_at) }
                        td data-label="" {
                            button .btn .btn-ghost .btn-sm
                                hx-delete=(format!("/b/userportal/sessions/{}", hex_encode(&r.token_hash)))
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

/// DELETE `/b/userportal/sessions/{token_hash_hex}`. Scoped to caller's
/// user_id — refusing to revoke another user's session looks indistinguishable
/// from "no such session" (returns 200 with no body either way; htmx removes
/// the row). Returns 401 if anonymous, 400 if hex is malformed.
pub async fn handle_revoke(ctx: &dyn Context, msg: &Message, sub: &str) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return ResponseBuilder::new()
            .status(401)
            .body(b"unauthenticated".to_vec(), "text/plain");
    }
    let hex_part = sub.strip_prefix("/sessions/").unwrap_or("");
    let hash = match decode_hex(hex_part) {
        Some(h) if !h.is_empty() => h,
        _ => {
            return ResponseBuilder::new()
                .status(400)
                .body(b"bad token_hash".to_vec(), "text/plain");
        }
    };
    let _ = sessions::delete_for_user(ctx, &user_id, &hash).await;
    // Empty 200 — htmx swaps the row out via outerHTML.
    ResponseBuilder::new()
        .status(200)
        .body(Vec::new(), "text/html")
}

#[cfg(test)]
mod tests {
    use wafer_core::clients::database as db;

    use super::*;
    use crate::{
        blocks::auth::{
            repo::sessions::{insert, NewSession},
            service::hash_token,
        },
        test_support::{anon_msg, auth_msg, output_html, output_status, TestContext},
    };

    /// Inject an `auth_token` cookie into a request `Message` by setting the
    /// `http.header.cookie` meta — mirroring how a real HTTP frontend
    /// surfaces cookies to handlers.
    fn with_auth_cookie(mut msg: wafer_run::Message, token: &str) -> wafer_run::Message {
        msg.set_meta("http.header.cookie", format!("auth_token={token}"));
        msg
    }

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        db::exec_raw(
            ctx,
            "INSERT INTO wafer_run__auth__users (id, email, display_name, role, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                serde_json::json!(user_id),
                serde_json::json!(format!("{user_id}@example.com")),
                serde_json::json!(user_id),
                serde_json::json!("user"),
                serde_json::json!("2026-01-01T00:00:00Z"),
                serde_json::json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .unwrap();
    }

    fn fake_session(user_id: &str, hash_byte: u8) -> NewSession {
        NewSession {
            token_hash: vec![hash_byte; 32],
            user_id: user_id.into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = TestContext::with_auth().await;
        let msg = anon_msg("retrieve", "/b/userportal/sessions");
        let resp = sessions_page(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn empty_renders_empty_state() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/userportal/sessions", "user-a");
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("No active sessions"));
    }

    #[tokio::test]
    async fn populated_renders_one_row_per_session_with_revoke() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();
        insert(&ctx, fake_session("user-a", 0x02)).await.unwrap();

        let msg = auth_msg("retrieve", "/b/userportal/sessions", "user-a");
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;

        assert!(html.contains("Sessions"), "missing page title");
        // Two Revoke buttons (one per row).
        assert!(
            html.matches(">Revoke<").count() >= 2,
            "expected ≥2 Revoke buttons, got: {}",
            html.matches(">Revoke<").count()
        );
    }

    #[tokio::test]
    async fn revoke_anonymous_returns_401() {
        let ctx = TestContext::with_auth().await;
        let msg = anon_msg("delete", "/b/userportal/sessions/aabb");
        let resp = handle_revoke(&ctx, &msg, "/sessions/aabb").await;
        assert_eq!(output_status(resp).await, 401);
    }

    #[tokio::test]
    async fn revoke_malformed_hex_returns_400() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("delete", "/b/userportal/sessions/zzz", "user-a");
        let resp = handle_revoke(&ctx, &msg, "/sessions/zzz").await;
        assert_eq!(output_status(resp).await, 400);
    }

    #[tokio::test]
    async fn revoke_own_session_deletes_it() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();
        assert_eq!(
            sessions::list_for_user(&ctx, "user-a").await.unwrap().len(),
            1
        );

        let hex_hash: String = (0..32).map(|_| "01".to_string()).collect();
        let msg = auth_msg(
            "delete",
            &format!("/b/userportal/sessions/{hex_hash}"),
            "user-a",
        );
        let resp = handle_revoke(&ctx, &msg, &format!("/sessions/{hex_hash}")).await;
        assert_eq!(output_status(resp).await, 200);
        assert_eq!(
            sessions::list_for_user(&ctx, "user-a").await.unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn revoke_other_users_session_is_no_op_returns_200() {
        let ctx = TestContext::with_auth().await;
        for u in ["user-a", "user-b"] {
            seed_user(&ctx, u).await;
        }
        insert(&ctx, fake_session("user-b", 0x02)).await.unwrap();

        // user-a tries to revoke user-b's session.
        let hex_hash: String = (0..32).map(|_| "02".to_string()).collect();
        let msg = auth_msg(
            "delete",
            &format!("/b/userportal/sessions/{hex_hash}"),
            "user-a",
        );
        let resp = handle_revoke(&ctx, &msg, &format!("/sessions/{hex_hash}")).await;
        assert_eq!(output_status(resp).await, 200);
        // user-b's session is still there — no leak.
        assert_eq!(
            sessions::list_for_user(&ctx, "user-b").await.unwrap().len(),
            1
        );
    }

    /// When the request's `auth_token` cookie hashes to one of the user's
    /// session rows, that row gets a "Current session" badge. Other rows
    /// don't. Mirrors how the JWT login path stores `hash_token(jwt)` in
    /// `token_hash`.
    #[tokio::test]
    async fn current_session_row_gets_badge() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;

        // Two sessions: one matches the request's cookie, one doesn't.
        let current_jwt = "eyJfake.jwt.value";
        let current_hash = hash_token(current_jwt);
        insert(
            &ctx,
            NewSession {
                token_hash: current_hash,
                user_id: "user-a".into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
            },
        )
        .await
        .unwrap();
        insert(&ctx, fake_session("user-a", 0xee)).await.unwrap();

        let msg = with_auth_cookie(
            auth_msg("retrieve", "/b/userportal/sessions", "user-a"),
            current_jwt,
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

    /// No `auth_token` cookie means no row matches — page renders without
    /// any current-session badge but still lists rows normally. Guards the
    /// "page must not crash for cookie-less callers" requirement.
    #[tokio::test]
    async fn no_cookie_renders_no_badge() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();

        // auth_msg sets user_id meta but no cookie header.
        let msg = auth_msg("retrieve", "/b/userportal/sessions", "user-a");
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;

        assert!(
            !html.contains("Current session"),
            "no badge expected when no auth_token cookie present"
        );
        // Row still rendered — page didn't degrade.
        assert!(html.contains(">Revoke<"), "row body still present");
    }

    /// A cookie that doesn't hash to any session row produces no badge.
    /// Catches the case where a stale/invalid token sneaks into the request
    /// (e.g. expired-but-not-yet-cleared client cookie).
    #[tokio::test]
    async fn cookie_with_no_matching_row_renders_no_badge() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();

        let msg = with_auth_cookie(
            auth_msg("retrieve", "/b/userportal/sessions", "user-a"),
            "eyJ.unrelated.jwt",
        );
        let resp = sessions_page(&ctx, &msg).await;
        let html = output_html(resp).await;

        assert!(
            !html.contains("Current session"),
            "no badge expected when cookie hash doesn't match any row"
        );
    }

    // --- WRAP regression: catches a future removal of the userportal
    // grant on `auth::repo::sessions::TABLE`. Without it, /b/userportal/
    // sessions silently returns the empty state for every authenticated
    // user. PR #77 added the grant; these tests fail closed if it's removed.

    #[tokio::test]
    async fn wrap_denies_sessions_list_without_grant() {
        // Seed BEFORE enabling WRAP — `seed_user` uses raw SQL, which WRAP
        // restricts to the admin block. In production, rows are seeded by
        // owner/admin paths; userportal only reads them. The test mirrors
        // that lifecycle.
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();

        let ctx = ctx.with_wrap("impresspress/userportal", Vec::new(), "impresspress/admin");

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
        use crate::blocks::auth::service::auth_grants;

        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();

        let ctx = ctx.with_wrap("impresspress/userportal", auth_grants(), "impresspress/admin");

        let rows = sessions::list_for_user(&ctx, "user-a")
            .await
            .expect("auth's production grants must cover userportal sessions read");
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn wrap_allows_sessions_delete_with_auth_block_grants() {
        use crate::blocks::auth::service::auth_grants;

        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", 0x01)).await.unwrap();

        let ctx = ctx.with_wrap("impresspress/userportal", auth_grants(), "impresspress/admin");

        let removed = sessions::delete_for_user(&ctx, "user-a", &[0x01u8; 32])
            .await
            .expect("auth's production grants must cover userportal sessions write");
        assert_eq!(removed, 1);
    }
}
