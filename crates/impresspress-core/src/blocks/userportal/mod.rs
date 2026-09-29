use maud::html;
use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::{config, database as db};
use wafer_run::{
    context::Context, BlockInfo, CollectionSchema, HttpMethod, InputStream, InstanceMode, Message,
    OutputStream,
};

use crate::{
    blocks::crud,
    config_vars::{
        ALLOW_SIGNUP_KEY, ALLOW_USER_PRODUCTS_KEY, APP_NAME_KEY, AUTH_LOGO_URL_KEY,
        DEFAULT_APP_NAME, ENABLE_OAUTH_KEY, FAVICON_URL_KEY, LOGO_ICON_URL_KEY, LOGO_URL_KEY,
        PRIMARY_COLOR_KEY,
    },
    endpoint_match::{self, EndpointRoute},
    http::{err_bad_request, err_forbidden, err_not_found, err_unauthenticated, ok_json},
    ui::{self, components, icons, settings_form},
    util::parse_form_body,
};

pub(crate) mod migrations;
// `pub(crate)`: `ui::sidebar`'s ICON_OPTIONS-coverage test reads
// `pages::admin_buttons::ICON_OPTIONS` to keep the icon dropdown and the
// `nav_icon` resolver in lockstep.
pub(crate) mod pages;

#[cfg(test)]
mod error_mapping_tests;

const TABLE: &str = "impresspress__userportal__buttons";

/// Handler for one row of [`ROUTES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Dashboard,
    Profile,
    UpdateProfile,
    Sessions,
    RevokeSession,
    Security,
    UnlinkProvider,
    Config,
    AdminSettingsPage,
    AdminSaveSettings,
    AdminButtonsPage,
    AdminCreateButton,
    AdminEditButtonForm,
    AdminUpdateButton,
    AdminDeleteButton,
}

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. Wire paths; `{family}` / `{id}` are
/// bound into `req.param.*` for the handlers' `msg.var` readers. The
/// `/admin/*` rows are declared `Admin` so the central router enforces the
/// tier; the block hand-checks nothing.
const ROUTES: &[EndpointRoute<Route>] = &[
    EndpointRoute::authenticated(HttpMethod::Get, "/b/userportal/", Route::Dashboard)
        .summary("Portal home (apps + orgs)"),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/userportal/profile", Route::Profile)
        .summary("Profile page"),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/userportal/update-profile",
        Route::UpdateProfile,
    )
    .summary("Update profile"),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/userportal/sessions", Route::Sessions)
        .summary("Active sessions"),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/userportal/sessions/{family}",
        Route::RevokeSession,
    )
    .summary("Revoke session"),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/userportal/security", Route::Security)
        .summary("Account security"),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/userportal/security/providers/{provider}",
        Route::UnlinkProvider,
    )
    .summary("Unlink an OAuth provider"),
    EndpointRoute::public(HttpMethod::Get, "/b/userportal/config", Route::Config)
        .summary("Portal configuration"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/userportal/admin/settings",
        Route::AdminSettingsPage,
    )
    .summary("Branding settings"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/userportal/admin/settings",
        Route::AdminSaveSettings,
    )
    .summary("Save branding settings"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/userportal/admin/buttons",
        Route::AdminButtonsPage,
    )
    .summary("Manage portal buttons"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/userportal/admin/buttons",
        Route::AdminCreateButton,
    )
    .summary("Create button"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/userportal/admin/buttons/{id}/edit",
        Route::AdminEditButtonForm,
    )
    .summary("Edit button form"),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/userportal/admin/buttons/{id}",
        Route::AdminUpdateButton,
    )
    .summary("Update button"),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/userportal/admin/buttons/{id}",
        Route::AdminDeleteButton,
    )
    .summary("Delete button"),
];

crate::impresspress_feature_block! {
    /// User-facing portal dashboard + admin button config (`impresspress/userportal`).
    pub struct UserPortalBlock;
    name: "impresspress/userportal",
    info: |_this| {
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
        .endpoints(endpoint_match::declare(ROUTES))
        .config_keys(vec![])
        .admin_url("/b/userportal/admin/settings")
        .can_disable(true)
        // Ships enabled — see the note on `legalpages`. Same divergence, same
        // resolution: the declaration is corrected to the value production has
        // been running, not the other way round.
        .default_enabled(true)
    },
    handle: |this, ctx, mut msg, input| {
        // Auth is enforced centrally by `route_to_block` from each row's
        // declared `AuthLevel`; the block holds no `user_id` / `is_admin`
        // preamble. `{family}` / `{id}` are bound into `req.param.*` for the
        // handlers' `msg.var` readers.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };
        match route {
            Route::Dashboard => pages::dashboard::dashboard_page(ctx, &msg).await,
            Route::Profile => pages::profile::profile_page(ctx, &msg).await,
            Route::UpdateProfile => handle_update_profile(ctx, &msg, input).await,
            Route::Sessions => pages::sessions::sessions_page(ctx, &msg).await,
            Route::RevokeSession => pages::sessions::handle_revoke(ctx, &msg).await,
            Route::Security => pages::security::security_page(ctx, &msg).await,
            Route::UnlinkProvider => pages::security::handle_unlink(ctx, &msg).await,
            Route::Config => this.handle_config(ctx, &msg).await,
            Route::AdminSettingsPage => admin_settings_page(ctx, &msg).await,
            Route::AdminSaveSettings => handle_save_settings(ctx, &msg, input).await,
            Route::AdminButtonsPage => pages::admin_buttons::admin_buttons_page(ctx, &msg).await,
            Route::AdminCreateButton => {
                pages::admin_buttons::handle_create_button(ctx, &msg, input).await
            }
            Route::AdminEditButtonForm => {
                pages::admin_buttons::handle_edit_button_form(ctx, msg.var("id")).await
            }
            Route::AdminUpdateButton => {
                pages::admin_buttons::handle_update_button(ctx, &msg, input, msg.var("id")).await
            }
            Route::AdminDeleteButton => {
                pages::admin_buttons::handle_delete_button(ctx, &msg, msg.var("id")).await
            }
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
    async fn handle_config(&self, ctx: &dyn Context, msg: &Message) -> OutputStream {
        // The gate the ROUTER published for this request, not the boot config
        // snapshot: the snapshot is frozen at `build()`
        // (`RuntimeConfig::republish` needs `&mut Wafer`), so after an admin
        // toggle this JSON would keep telling the portal that Files is on
        // while the router answers `/b/storage/` with "endpoint not found" —
        // a tab that renders and then 404s. See `routing::gate_from_request`.
        let settings = crate::routing::gate_from_request(ctx, msg);

        match portal_config(ctx, &settings).await {
            Ok(config_val) => ok_json(&config_val),
            Err(e) => crud::db_error_internal(e, "Failed to read the portal config"),
        }
    }
}

/// The public portal config: branding, the signup and OAuth switches, and
/// which feature tabs the router serves. A setting that cannot be read fails
/// the whole answer rather than showing a default in its place.
async fn portal_config(
    ctx: &dyn Context,
    settings: &crate::features::BlockSettings,
) -> Result<serde_json::Value, wafer_run::WaferError> {
    use crate::features::FeatureConfig;
    let is_enabled = |name: &str| settings.is_block_enabled(name);
    Ok(serde_json::json!({
        "logo_url": config::get_default(ctx, crate::config_vars::LOGO_URL_KEY, "").await?,
        "app_name": config::get_default(ctx, APP_NAME_KEY, DEFAULT_APP_NAME).await?,
        // Blank = "use the built-in brand accent" (same contract as the
        // admin chrome; see layout::page).
        "primary_color": config::get_default(ctx, PRIMARY_COLOR_KEY, "").await?,
        "enable_oauth": config::get_default(ctx, ENABLE_OAUTH_KEY, "false").await?,
        "allow_signup": config::get_default(ctx, ALLOW_SIGNUP_KEY, "true").await?,
        "show_powered_by": true,
        "features": {
            "files": is_enabled("impresspress/files"),
            "products": is_enabled("impresspress/products"),
            "user_products": config::get_default(ctx, ALLOW_USER_PRODUCTS_KEY, "false").await?,
            "legal_pages": is_enabled("impresspress/legalpages"),
            "userportal": is_enabled("impresspress/userportal"),
        }
    }))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// The configured portal buttons, in display order. A failed read is an
/// error, not an empty list: callers render "no buttons" only for a table that
/// was actually read and is empty.
async fn load_buttons(
    ctx: &dyn Context,
) -> Result<Vec<wafer_core::clients::database::Record>, wafer_run::WaferError> {
    db::list(
        ctx,
        TABLE,
        &ListOptions {
            sort: vec![SortField {
                field: "sort_order".into(),
                desc: false,
            }],
            limit: Some(50),
            ..Default::default()
        },
    )
    .await
    .map(|r| r.records)
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
        return err_unauthenticated("Not authenticated");
    }

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body = parse_form_body(&raw);

    // CSRF defense-in-depth: this is a plain (no-JS) `<form>` POST (see
    // `pages::profile::profile_page`, which embeds the matching token via
    // `crate::csrf::hidden_field`). The Fetch-Metadata/Origin layer
    // (`crate::csrf::enforce_origin_policy`) already covers this request
    // since it's cookie-authenticated and unsafe-method; this is the
    // additional per-form check.
    let submitted_csrf = body
        .get(crate::csrf::FIELD_NAME)
        .map(String::as_str)
        .unwrap_or("");
    if !crate::csrf::verify(ctx, msg, submitted_csrf) {
        return err_forbidden("invalid or missing csrf token");
    }

    // A blank name is refused rather than written: nothing legitimately
    // clears a display name through this form, and a missing or empty field
    // is what a form rendered without the user's row would post back.
    let name = body.get("name").map(|s| s.trim()).unwrap_or("");
    if name.is_empty() {
        return err_bad_request("Display name is required");
    }

    // `update_profile` dual-writes `display_name` and the `name` alias, so
    // the typed row and the raw column cannot drift apart.
    if let Err(e) =
        crate::blocks::auth::repo::users::update_profile(ctx, &user_id, Some(name), None).await
    {
        // `db_error_internal`, not `db_error`: this row is the signed-in
        // user's own, so a `NotFound` here means their account vanished
        // mid-session — an internal inconsistency, not a 404 the form can
        // act on. What the classification is here for is the arm above it:
        // a WRAP refusal on `wafer_run__auth__users` now answers 403 instead
        // of the 500 that made a missing grant look like an outage.
        return crud::db_error_internal(e, "Failed to update profile");
    }

    // Plain form POST → 303 See Other so the browser follows up with a GET
    // and the back/forward stack stays clean.
    crate::http::redirect(303, "/b/userportal/profile")
}

#[cfg(test)]
mod update_profile_csrf_tests {
    use super::*;
    use crate::test_support::{auth_msg, output_status, TestContext};

    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
        crate::blocks::auth::repo::users::update_profile(ctx, user_id, Some("Old Name"), None)
            .await
            .expect("seed profile name");
    }

    /// The `display_name`/`name` pair as the repo reports it.
    async fn profile_name(ctx: &TestContext, user_id: &str) -> String {
        crate::blocks::auth::repo::users::find_by_id(ctx, user_id)
            .await
            .expect("read user")
            .expect("user exists")
            .display_name
    }

    #[tokio::test]
    async fn valid_csrf_token_is_accepted() {
        let ctx = TestContext::with_userportal().await;
        seed_user(&ctx, "user-1").await;
        let msg = auth_msg("create", "/b/userportal/update-profile", "user-1");

        let form = format!(
            "name=New+Name&csrf_token={}",
            crate::csrf::token(&ctx, &msg)
        );
        let out =
            handle_update_profile(&ctx, &msg, InputStream::from_bytes(form.into_bytes())).await;
        assert_eq!(output_status(out).await, 303, "valid token must succeed");

        assert_eq!(profile_name(&ctx, "user-1").await, "New Name");
    }

    /// With no identity the form is told to sign in (401 and its challenge),
    /// not refused as a forbidden action. The router's gate answers first in
    /// a deployment; this is the handler's own check behind it.
    #[tokio::test]
    async fn an_anonymous_profile_post_is_401() {
        let ctx = TestContext::with_userportal().await;
        let msg = crate::test_support::anon_msg("create", "/b/userportal/update-profile");

        let out = handle_update_profile(
            &ctx,
            &msg,
            InputStream::from_bytes(b"name=New+Name".to_vec()),
        )
        .await;
        let parts = wafer_block::http_codec::collect_http_response(out).await;
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

    #[tokio::test]
    async fn missing_csrf_token_is_rejected() {
        let ctx = TestContext::with_userportal().await;
        seed_user(&ctx, "user-1").await;
        let msg = auth_msg("create", "/b/userportal/update-profile", "user-1");

        let form = "name=New+Name".to_string();
        let out =
            handle_update_profile(&ctx, &msg, InputStream::from_bytes(form.into_bytes())).await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "a form POST with no csrf_token must be rejected"
        );

        // Row must be unchanged — rejection happens before the update.
        assert_eq!(profile_name(&ctx, "user-1").await, "Old Name");
    }

    #[tokio::test]
    async fn wrong_csrf_token_is_rejected() {
        let ctx = TestContext::with_userportal().await;
        seed_user(&ctx, "user-1").await;
        let msg = auth_msg("create", "/b/userportal/update-profile", "user-1");

        let form = "name=New+Name&csrf_token=not-the-right-value".to_string();
        let out =
            handle_update_profile(&ctx, &msg, InputStream::from_bytes(form.into_bytes())).await;
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    /// A blank name is refused, not written. The profile page pre-fills
    /// this field from the user's row; when that read failed the page
    /// rendered `value=""`, and the user's next Save posted it here, which
    /// wrote "" over their real name. Driven through the block's route table
    /// with the exact form bytes the page posts.
    #[tokio::test]
    async fn an_empty_name_is_refused_and_the_row_keeps_its_name() {
        let ctx = TestContext::with_userportal().await;
        seed_user(&ctx, "user-1").await;
        let msg = auth_msg("create", "/b/userportal/update-profile", "user-1");

        for blank in ["", "+++"] {
            let form = format!("csrf_token={}&name={blank}", crate::csrf::token(&ctx, &msg));
            let (status, _) = super::test_support::browser_request(&ctx, msg.clone(), &form).await;
            assert_eq!(status, 400, "name={blank:?} must be refused");
            assert_eq!(profile_name(&ctx, "user-1").await, "Old Name");
        }

        // A form with no `name` field at all is the same blank.
        let form = format!("csrf_token={}", crate::csrf::token(&ctx, &msg));
        let (status, _) = super::test_support::browser_request(&ctx, msg.clone(), &form).await;
        assert_eq!(status, 400, "a missing name must be refused");
        assert_eq!(profile_name(&ctx, "user-1").await, "Old Name");
    }

    /// Control for the test above: the same route, a real name, is written
    /// (trimmed) and answered with the form's 303.
    #[tokio::test]
    async fn a_real_name_is_written_trimmed() {
        let ctx = TestContext::with_userportal().await;
        seed_user(&ctx, "user-1").await;
        let msg = auth_msg("create", "/b/userportal/update-profile", "user-1");

        let form = format!(
            "csrf_token={}&name=+New+Name+",
            crate::csrf::token(&ctx, &msg)
        );
        let (status, _) = super::test_support::browser_request(&ctx, msg, &form).await;
        assert_eq!(status, 303);
        assert_eq!(profile_name(&ctx, "user-1").await, "New Name");
    }

    #[tokio::test]
    async fn another_users_token_is_rejected() {
        // The token is per-identity — user B's valid token must not authorize
        // a mutation submitted as user A.
        let ctx = TestContext::with_userportal().await;
        seed_user(&ctx, "user-1").await;
        let msg_a = auth_msg("create", "/b/userportal/update-profile", "user-1");
        let msg_b = auth_msg("create", "/b/userportal/update-profile", "user-2");

        let form = format!(
            "name=New+Name&csrf_token={}",
            crate::csrf::token(&ctx, &msg_b)
        );
        let out =
            handle_update_profile(&ctx, &msg_a, InputStream::from_bytes(form.into_bytes())).await;
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }
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
        APP_NAME_KEY,
        LOGO_URL_KEY,
        LOGO_ICON_URL_KEY,
        AUTH_LOGO_URL_KEY,
        FAVICON_URL_KEY,
        PRIMARY_COLOR_KEY,
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
    let form = match settings_form::settings_form(
        ctx,
        "/b/userportal/admin/settings",
        &sections,
        html! {},
    )
    .await
    {
        Ok(form) => form,
        Err(e) => {
            return crud::db_error_page(
                msg,
                e,
                "userportal branding settings: current values read failed",
            )
        }
    };
    let content = html! {
        (components::page_header("Branding Settings", Some("Customize your application appearance"), None))
        (form)
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Settings", ui::NavKind::Portal, "Settings"),
        content,
    )
    .await
}

async fn handle_save_settings(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    settings_form::save_settings(ctx, msg, input, &branding_vars(), "userportal").await
}

#[cfg(test)]
mod test_support {
    use wafer_run::{context::Context, InputStream, Message};

    /// Run `msg` through the block's own route table so `{family}` / `{id}` is
    /// bound the way it is on the wire, then hand the message to a handler
    /// directly. Panics when no row matches: a test that sends an unroutable
    /// path would otherwise exercise the handler's "nothing bound" branch by
    /// accident.
    pub(super) fn routed(mut msg: Message) -> Message {
        let route = crate::endpoint_match::dispatch(&mut msg, super::ROUTES);
        assert!(
            route.is_some(),
            "no userportal route matches {} {}",
            msg.action(),
            msg.path()
        );
        msg
    }

    /// Send a browser request through the block's own `handle` — the route
    /// table dispatch included — with `body` as the request body, and return
    /// what the HTTP adapter would send: status and body, error terminals
    /// rendered by the same `http_codec` the real adapters use.
    pub(super) async fn browser_request(
        ctx: &dyn Context,
        mut msg: Message,
        body: &str,
    ) -> (u16, String) {
        msg.set_meta("http.header.accept", "text/html");
        let out = wafer_run::Block::handle(
            &super::UserPortalBlock::new(),
            ctx,
            msg,
            InputStream::from_bytes(body.as_bytes().to_vec()),
        )
        .await;
        let parts = wafer_block::http_codec::collect_http_response(out).await;
        (
            parts.status,
            String::from_utf8(parts.body).expect("response body is UTF-8"),
        )
    }
}

#[cfg(test)]
mod table_tests {
    use wafer_run::Block as _;

    use super::*;
    use crate::config_vars::APP_NAME_KEY;

    /// The branding page is a settings form whose Save posts every field.
    /// When the stored branding cannot be read it is a 500, never the form
    /// filled from the boot map and the declared defaults.
    #[tokio::test]
    async fn a_failed_branding_read_renders_no_form() {
        let ctx = crate::test_support::TestContext::with_userportal()
            .await
            .break_reads();

        let (status, html) = test_support::browser_request(
            &ctx,
            crate::test_support::admin_msg("retrieve", "/b/userportal/admin/settings"),
            "",
        )
        .await;

        assert_eq!(status, 500);
        assert!(!html.contains("<form"), "{html}");
    }

    /// Control: a healthy read renders the form with the stored value.
    #[tokio::test]
    async fn the_branding_form_shows_the_stored_app_name() {
        let ctx = crate::test_support::TestContext::with_userportal().await;
        let app_name = crate::test_support::unique_config_value();
        // Staged as the operator would: the shared key is the admin block's
        // to write, not the portal's.
        wafer_core::clients::config::set(&ctx.fixture(), APP_NAME_KEY, &app_name)
            .await
            .expect("store the app name");

        let (status, html) = test_support::browser_request(
            &ctx,
            crate::test_support::admin_msg("retrieve", "/b/userportal/admin/settings"),
            "",
        )
        .await;

        assert_eq!(status, 200);
        assert!(html.contains("<form"), "{html}");
        assert!(html.contains(&format!(r#"value="{app_name}""#)), "{html}");
    }

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = UserPortalBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }
}
