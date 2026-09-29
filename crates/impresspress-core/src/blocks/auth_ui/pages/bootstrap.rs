//! GET /b/auth/bootstrap — bootstrap admin token redemption form.
//!
//! When `BOOTSTRAP_ADMIN_TOKEN` was set on first boot, no admin user was
//! created — instead, a sha256(token) row was written to
//! `wafer_run__auth__bootstrap_tokens` with a 24h expiry. This page is
//! where the holder of that raw token redeems it: paste the token, choose
//! an email + password, submit. The POST handler verifies, creates the
//! admin user, consumes the token, and logs the caller in.

use maud::html;
use wafer_run::{context::Context, Message, OutputStream};

use super::site_config;
use crate::ui::{self, components::auth_panel, templates::auth_split};

pub async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let config = match site_config(ctx).await {
        Ok(site) => site,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: site config read failed")
        }
    };

    // Optional convenience: if the holder shared a `?token=...` link, pre-fill
    // the field. The value is rendered as an attribute (maud HTML-escapes it).
    let prefill_token = msg.get_meta("req.query.token").to_string();

    let markup = ui::layout::page(
        "Bootstrap Admin",
        &config,
        auth_split(
            auth_panel(&config, Some("Set up your admin account.")),
            html! {
                div .login-container {
                    p .bootstrap-hint {
                        "Paste the bootstrap token from your "
                        code { "BOOTSTRAP_ADMIN_TOKEN" }
                        " env var, then pick the admin email and password."
                    }

                    form method="post" action="/b/auth/api/bootstrap" .login-form {
                        (crate::csrf::hidden_field(ctx, msg))

                        div .form-group {
                            label .form-label for="token" { "Bootstrap Token" }
                            input
                                .form-input
                                type="text"
                                id="token"
                                name="token"
                                placeholder="Paste the token here"
                                value=(prefill_token)
                                required;
                        }

                        div .form-group {
                            label .form-label for="email" { "Admin Email" }
                            input
                                .form-input
                                type="email"
                                id="email"
                                name="email"
                                placeholder="admin@example.com"
                                required;
                        }

                        div .form-group {
                            label .form-label for="password" { "Admin Password" }
                            input
                                .form-input
                                type="password"
                                id="password"
                                name="password"
                                placeholder="Min 8 characters"
                                minlength="8"
                                required;
                        }

                        button .login-button type="submit" { "Redeem" }
                    }
                }
            },
        ),
    );

    ui::html_response(markup)
}
