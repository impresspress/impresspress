//! `/b/auth/orgs` — read-only list of orgs claimed by the current user.
//! Rendered through the shared userportal account-card layout so it
//! visually matches profile / sessions / security.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::auth::repo::orgs,
    http::redirect,
    ui::{self, SiteConfig},
};

/// GET `/b/auth/orgs`. Anonymous users redirected to login.
pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return redirect(302, "/b/auth/login");
    }

    let orgs_list = orgs::list_for_user(ctx, &user_id).await.unwrap_or_default();
    let body = html! {
        p .text-muted style="margin:0 0 1rem;font-size:0.875rem" {
            "Orgs you've claimed via GitHub, Google, or Microsoft sign-in."
        }
        (render_orgs_body(&orgs_list))
    };

    let config = SiteConfig::load(ctx).await;
    let markup = ui::layout::page(
        "Organizations",
        &config,
        ui::templates::account_card_page(
            ui::templates::AccountCard {
                logo_url: &config.logo_url,
                title: "Organizations",
                back_href: Some("/b/userportal/"),
            },
            body,
        ),
    );
    ui::html_response(markup)
}

fn render_orgs_body(orgs: &[orgs::OrgRow]) -> Markup {
    if orgs.is_empty() {
        return html! {
            p .text-muted style="margin:0" {
                "No claimed organizations. Sign in with GitHub, Google, or Microsoft to claim one."
            }
        };
    }
    html! {
        ul .orgs-list {
            @for o in orgs {
                li .orgs-list-row {
                    span .orgs-list-row__provider {
                        (o.verified_via.as_deref().unwrap_or("manual"))
                    }
                    span .orgs-list-row__name { (o.name) }
                    span .orgs-list-row__date { "claimed " (o.created_at) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wafer_core::clients::database as db;

    use super::*;
    use crate::{
        blocks::auth::repo::orgs::{upsert_claimed, NewClaim},
        test_support::{
            anon_msg, auth_msg, output_header, output_html, output_status, TestContext,
        },
    };

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        db::exec_raw(
            ctx,
            "INSERT INTO wafer_run__auth__users (id, email, display_name, role, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                json!(user_id),
                json!(format!("{user_id}@example.com")),
                json!(user_id),
                json!("user"),
                json!("2026-01-01T00:00:00Z"),
                json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn anonymous_redirects_to_login() {
        let ctx = TestContext::with_auth().await;
        let msg = anon_msg("retrieve", "/b/auth/orgs");
        let resp = handle(&ctx, &msg).await;
        assert_eq!(output_status(resp).await, 302);
    }

    #[tokio::test]
    async fn anonymous_redirect_sets_location() {
        let ctx = TestContext::with_auth().await;
        let msg = anon_msg("retrieve", "/b/auth/orgs");
        let resp = handle(&ctx, &msg).await;
        assert_eq!(
            output_header(resp, "Location").await.as_deref(),
            Some("/b/auth/login")
        );
    }

    #[tokio::test]
    async fn empty_renders_empty_state_copy() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        let msg = auth_msg("retrieve", "/b/auth/orgs", "user-a");
        let resp = handle(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("No claimed organizations"));
        assert!(html.contains("Sign in with GitHub"));
    }

    #[tokio::test]
    async fn populated_renders_one_row_per_org() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        upsert_claimed(
            &ctx,
            NewClaim {
                name: "alpha",
                owner_user_id: "user-a",
                verified_via: "github",
                verified_ref: "gh-1",
            },
        )
        .await
        .unwrap();
        upsert_claimed(
            &ctx,
            NewClaim {
                name: "beta",
                owner_user_id: "user-a",
                verified_via: "google",
                verified_ref: "gg-2",
            },
        )
        .await
        .unwrap();

        let msg = auth_msg("retrieve", "/b/auth/orgs", "user-a");
        let resp = handle(&ctx, &msg).await;
        let html = output_html(resp).await;
        assert!(html.contains("alpha"));
        assert!(html.contains("beta"));
        assert!(html.contains("github"));
        assert!(html.contains("google"));
    }
}
