use maud::html;
use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::{config, database as db};
use wafer_run::{
    context::Context, BlockEndpoint, BlockInfo, CollectionSchema, InputStream, InstanceMode,
    Message, OutputStream,
};

use crate::{
    http::{err_forbidden, err_internal, err_not_found, ok_json},
    ui::{self, components, icons, settings_form},
    util::{parse_form_body, stamp_updated, RecordExt},
};

pub(crate) mod migrations;
// `pub(crate)`: `ui::sidebar`'s ICON_OPTIONS-coverage test reads
// `pages::admin_buttons::ICON_OPTIONS` to keep the icon dropdown and the
// `nav_icon` resolver in lockstep.
pub(crate) mod pages;

const TABLE: &str = "impresspress__userportal__buttons";

crate::impresspress_feature_block! {
    /// User-facing portal dashboard + admin button config (`impresspress/userportal`).
    pub struct UserPortalBlock;
    name: "impresspress/userportal",
    info: |_this| {
        use wafer_run::AuthLevel;

        BlockInfo::new(
            "impresspress/userportal",
            "0.0.1",
            "http-handler@v1",
            "User profile and account hub with admin-configurable navigation buttons",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec!["wafer-run/database".into(), "wafer-run/config".into()])
        // Advisory table list — admin "Database tables" discovery + the WRAP
        // grant-UI read only `CollectionSchema::name`. The schema itself
        // (columns, indexes) lives solely in the block's hand-authored
        // `migrations/*.sqlite.sql` files (the single source for both runtime
        // `migrations::apply()` and the Cloudflare D1 build).
        .collections(vec![CollectionSchema::new(TABLE)])
        .category(wafer_run::BlockCategory::Feature)
        .description("User-facing profile page with editable display name, admin-configurable navigation buttons, and portal configuration endpoint.")
        .endpoints(vec![
            BlockEndpoint::get("/b/userportal/").summary("Portal home (apps + orgs)").auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/userportal/profile").summary("Profile page").auth(AuthLevel::Authenticated),
            BlockEndpoint::post("/b/userportal/update-profile").summary("Update profile").auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/userportal/sessions").summary("Active sessions").auth(AuthLevel::Authenticated),
            BlockEndpoint::delete("/b/userportal/sessions/:hash").summary("Revoke session").auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/userportal/security").summary("Account security").auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/userportal/config").summary("Portal configuration"),
            // Admin surface — declared in full so the central router enforces
            // the `Admin` tier (the block no longer hand-checks `is_admin` for
            // the `/admin/` subtree).
            BlockEndpoint::get("/b/userportal/admin/settings").summary("Branding settings").auth(AuthLevel::Admin),
            BlockEndpoint::post("/b/userportal/admin/settings").summary("Save branding settings").auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/userportal/admin/buttons").summary("Manage portal buttons").auth(AuthLevel::Admin),
            BlockEndpoint::post("/b/userportal/admin/buttons").summary("Create button").auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/userportal/admin/buttons/{id}/edit").summary("Edit button form").auth(AuthLevel::Admin),
            BlockEndpoint::patch("/b/userportal/admin/buttons/{id}").summary("Update button").auth(AuthLevel::Admin),
            BlockEndpoint::delete("/b/userportal/admin/buttons/{id}").summary("Delete button").auth(AuthLevel::Admin),
        ])
        .config_keys(vec![])
        .admin_url("/b/userportal/admin/settings")
        .can_disable(true)
        .default_enabled(false)
    },
    handle: |this, ctx, msg, input| {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        if !path.starts_with("/b/userportal") {
            return this.handle_config(ctx).await;
        }

        let sub = path
            .strip_prefix("/b/userportal")
            .unwrap_or("/")
            .to_string();

        // Admin routes. The `Admin` tier is enforced centrally from the
        // declared `/b/userportal/admin/*` endpoints, so no inline `is_admin`
        // check is needed; the normalized `sub` path is still passed
        // explicitly to the admin sub-dispatcher (no `req.resource` rewrite).
        if sub.starts_with("/admin/") {
            return this.handle_admin(ctx, msg, input, &action, &sub).await;
        }

        match (action.as_str(), sub.as_str()) {
            ("retrieve", "" | "/") => pages::dashboard::dashboard_page(ctx, &msg).await,
            ("retrieve", "/profile") => pages::profile::profile_page(ctx, &msg).await,
            ("create", "/update-profile") => handle_update_profile(ctx, &msg, input).await,
            ("retrieve", "/sessions") => pages::sessions::sessions_page(ctx, &msg).await,
            ("retrieve", "/security") => pages::security::security_page(ctx, &msg).await,
            ("delete", s) if s.starts_with("/sessions/") => {
                pages::sessions::handle_revoke(ctx, &msg, s).await
            }
            ("retrieve", "/config") => this.handle_config(ctx).await,
            ("retrieve", "/internal/list-buttons") => this.handle_list_buttons(ctx).await,
            _ => err_not_found("not found"),
        }
    },
    lifecycle: |_this, ctx, event| {
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/userportal",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await
    },
}

impl UserPortalBlock {
    /// Internal cross-block action — returns the configured portal buttons as
    /// a JSON array. Not user-routable. Consumed by the auth block's dashboard
    /// page via `ctx.call_block` to avoid raw cross-block SQL.
    async fn handle_list_buttons(&self, ctx: &dyn Context) -> OutputStream {
        let records = load_buttons(ctx).await;
        let arr: Vec<serde_json::Value> = records
            .iter()
            .map(|r| {
                serde_json::json!({
                    "label": r.str_field("label"),
                    "icon": r.str_field("icon"),
                    "path": r.str_field("path"),
                    "sort_order": r.data.get("sort_order").cloned().unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();
        ok_json(&serde_json::Value::Array(arr))
    }

    async fn handle_config(&self, ctx: &dyn Context) -> OutputStream {
        let settings = ctx
            .config_get(crate::features::BLOCK_SETTINGS_CONFIG_KEY)
            .map(crate::features::BlockSettings::from_config_json)
            .unwrap_or_else(|| crate::features::BlockSettings::from_map(Default::default()));

        let is_enabled = |name: &str| -> bool {
            use crate::features::FeatureConfig;
            settings.is_block_enabled(name)
        };

        let config_val = serde_json::json!({
            "logo_url": config::get_default(ctx, "WAFER_RUN_SHARED__LOGO_URL", crate::ui::assets::logo_long_url()).await,
            "app_name": config::get_default(ctx, "WAFER_RUN_SHARED__APP_NAME", "Impresspress").await,
            "primary_color": config::get_default(ctx, "WAFER_RUN_SHARED__PRIMARY_COLOR", "#6366f1").await,
            "enable_oauth": config::get_default(ctx, "WAFER_RUN_SHARED__ENABLE_OAUTH", "false").await,
            "allow_signup": config::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_SIGNUP", "true").await,
            "show_powered_by": true,
            "features": {
                "files": is_enabled("impresspress/files"),
                "products": is_enabled("impresspress/products"),
                "user_products": config::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false").await,
                "legal_pages": is_enabled("impresspress/legalpages"),
                "userportal": is_enabled("impresspress/userportal"),
            }
        });
        ok_json(&config_val)
    }

    async fn handle_admin(
        &self,
        ctx: &dyn Context,
        msg: Message,
        input: InputStream,
        action: &str,
        sub: &str,
    ) -> OutputStream {
        match (action, sub) {
            ("retrieve", "/admin/settings") => admin_settings_page(ctx, &msg).await,
            ("create", "/admin/settings") => handle_save_settings(ctx, input).await,
            ("retrieve", "/admin/buttons") => {
                pages::admin_buttons::admin_buttons_page(ctx, &msg).await
            }
            ("create", "/admin/buttons") => {
                pages::admin_buttons::handle_create_button(ctx, input).await
            }
            ("retrieve", s) if s.starts_with("/admin/buttons/") && s.ends_with("/edit") => {
                let id = s
                    .strip_prefix("/admin/buttons/")
                    .and_then(|s| s.strip_suffix("/edit"))
                    .unwrap_or("");
                if id.is_empty() {
                    return err_not_found("not found");
                }
                pages::admin_buttons::handle_edit_button_form(ctx, id).await
            }
            ("update", s) if s.starts_with("/admin/buttons/") => {
                let id = s.strip_prefix("/admin/buttons/").unwrap_or("");
                if id.is_empty() {
                    return err_not_found("not found");
                }
                pages::admin_buttons::handle_update_button(ctx, input, id).await
            }
            ("delete", s) if s.starts_with("/admin/buttons/") => {
                let id = s.strip_prefix("/admin/buttons/").unwrap_or("");
                if id.is_empty() {
                    return err_not_found("not found");
                }
                pages::admin_buttons::handle_delete_button(ctx, id).await
            }
            _ => err_not_found("not found"),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async fn load_buttons(ctx: &dyn Context) -> Vec<wafer_core::clients::database::Record> {
    db::list(
        ctx,
        TABLE,
        &ListOptions {
            sort: vec![SortField {
                field: "sort_order".into(),
                desc: false,
            }],
            limit: 50,
            ..Default::default()
        },
    )
    .await
    .map(|r| r.records)
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// User-facing: Update profile
// ---------------------------------------------------------------------------

async fn handle_update_profile(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_forbidden("Not authenticated");
    }

    let raw = input.collect_to_bytes().await;
    let body = parse_form_body(&raw);
    let name = body.get("name").map(|s| s.as_str()).unwrap_or("");

    let mut data = std::collections::HashMap::new();
    data.insert("name".to_string(), serde_json::json!(name));
    data.insert("display_name".to_string(), serde_json::json!(name));
    stamp_updated(&mut data);

    if let Err(e) = db::update(ctx, crate::blocks::auth::USERS_TABLE, &user_id, data).await {
        // Pass the full WaferError (code + meta + message) so the
        // helper logs structured info instead of just the rendered string.
        return err_internal("Failed to update profile", e);
    }

    // Plain form POST → 303 See Other so the browser follows up with a GET
    // and the back/forward stack stays clean.
    crate::http::redirect(303, "/b/userportal/profile")
}

// ---------------------------------------------------------------------------
// Admin: Branding Settings
// ---------------------------------------------------------------------------

/// The shared branding config vars rendered on the portal settings page,
/// pulled from their central `config_vars::shared_var` declarations (single
/// source of truth — no parallel tuple table that had drifted on the logo-URL
/// input types and the favicon default).
fn branding_vars() -> Vec<wafer_run::ConfigVar> {
    [
        "WAFER_RUN_SHARED__APP_NAME",
        "WAFER_RUN_SHARED__LOGO_URL",
        "WAFER_RUN_SHARED__LOGO_ICON_URL",
        "WAFER_RUN_SHARED__AUTH_LOGO_URL",
        "WAFER_RUN_SHARED__FAVICON_URL",
        "WAFER_RUN_SHARED__PRIMARY_COLOR",
    ]
    .into_iter()
    .map(crate::config_vars::shared_var)
    .collect()
}

async fn admin_settings_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let vars = branding_vars();
    let sections = [settings_form::SettingsSection::new(
        "Branding",
        icons::settings(),
        &vars,
    )];
    let content = html! {
        (components::page_header("Branding Settings", Some("Customize your application appearance"), None))
        (settings_form::settings_form(ctx, "/b/userportal/admin/settings", &sections, html! {}).await)
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Settings", ui::NavKind::Portal, "Settings"),
        content,
    )
    .await
}

async fn handle_save_settings(ctx: &dyn Context, input: InputStream) -> OutputStream {
    settings_form::save_settings(ctx, input, &branding_vars(), "userportal").await
}

#[cfg(test)]
mod cross_block_tests {
    use std::collections::HashMap;

    use serde_json::json;
    use wafer_core::clients::database as db;
    use wafer_run::Block;

    use super::*;
    use crate::test_support::{anon_msg, output_json, TestContext};

    fn button_data(
        label: &str,
        icon: &str,
        path: &str,
        sort_order: i64,
    ) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("label".to_string(), json!(label));
        m.insert("icon".to_string(), json!(icon));
        m.insert("path".to_string(), json!(path));
        m.insert("sort_order".to_string(), json!(sort_order));
        m
    }

    #[tokio::test]
    async fn list_buttons_action_returns_json_array_in_sort_order() {
        let ctx = TestContext::with_userportal().await;

        // Seed two buttons through the userportal-owned `buttons` table.
        db::create(
            &ctx,
            TABLE,
            button_data("Impresspress", "shield", "/b/admin/", 0),
        )
        .await
        .expect("seed first button");
        db::create(
            &ctx,
            TABLE,
            button_data("Inspector", "search", "/b/inspector/ui", 1),
        )
        .await
        .expect("seed second button");

        let block = UserPortalBlock::new();
        let msg = anon_msg("retrieve", "/b/userportal/internal/list-buttons");
        let resp = block
            .handle(&ctx, msg, wafer_run::InputStream::empty())
            .await;
        let parsed = output_json(resp).await;

        let arr = parsed.as_array().expect("response is JSON array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["label"], "Impresspress");
        assert_eq!(arr[0]["icon"], "shield");
        assert_eq!(arr[0]["path"], "/b/admin/");
        assert_eq!(arr[1]["label"], "Inspector");
        assert_eq!(arr[1]["icon"], "search");
    }

    #[tokio::test]
    async fn list_buttons_action_returns_empty_array_when_none_configured() {
        let ctx = TestContext::new().await;
        let block = UserPortalBlock::new();
        let msg = anon_msg("retrieve", "/b/userportal/internal/list-buttons");
        let resp = block
            .handle(&ctx, msg, wafer_run::InputStream::empty())
            .await;
        let parsed = output_json(resp).await;
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }
}
