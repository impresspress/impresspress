//! Central config variable definitions.
//!
//! Shared (`WAFER_RUN_SHARED__`) variables are defined here — the single source
//! of truth. Block-scoped variables are declared in each block's `BlockInfo`.
//!
//! Use `collect_all_config_vars()` to get the complete set of all known config
//! variables (shared + block-declared) for seeding, validation, and UI rendering.

use wafer_run::{ConfigVar, InputType};

/// Worker-secret name for the deploy-time `/_deploy/init` bearer token.
///
/// One canonical name shared by both sides of the deploy handshake: the CLI
/// (`impresspress deploy` / `impresspress deploy secret`) reads it from the
/// same-named env var and provisions it via `wrangler secret put`, and the
/// Cloudflare worker reads it via `env.secret(DEPLOY_TOKEN_KEY)` to gate the
/// endpoint. Not a `ConfigVar` (never lives in D1 or the admin UI) — it is a
/// deploy-time worker secret, so it is a plain const rather than a
/// `WAFER_RUN_SHARED__*` entry.
pub const DEPLOY_TOKEN_KEY: &str = "IMPRESSPRESS_DEPLOY_TOKEN";

/// Shared config key: cross-origin origins allowed to call the API.
///
/// Fed to the `wafer-run/cors` middleware block's `allowed_origins` at boot
/// (see `flows::register_site_main`). Empty by default: the CORS block fails
/// closed and denies all cross-origin requests until an operator lists the
/// static-storefront/SPA origins that embed the products widget. Comma-
/// separated (e.g. `https://shop.example,https://www.shop.example`) or `*`.
pub const CORS_ALLOWED_ORIGINS_KEY: &str = "WAFER_RUN_SHARED__CORS_ALLOWED_ORIGINS";

/// Shared config key: extra Content-Security-Policy directives, merged over
/// the security-headers block's hard baseline (which can only be *widened*,
/// never weakened — see `wafer-block-security-headers::merge_csp`).
///
/// Defaults to [`DEFAULT_CSP_DIRECTIVES`] so embedded Stripe Checkout works
/// out of the box on first-party pages. Operators extend this to allow
/// additional embeds; the baseline `default-src`/`script-src` guarantees
/// survive regardless of what is set here.
pub const CSP_DIRECTIVES_KEY: &str = "WAFER_RUN_SHARED__CSP_DIRECTIVES";

/// Default value for [`CSP_DIRECTIVES_KEY`] — the Stripe origins that
/// embedded Checkout and Stripe.js require, per
/// `docs/products-stripe-commerce.md`. Additive only: these widen
/// `script-src`/`frame-src`/`connect-src` to the named Stripe hosts. Hosted
/// Checkout and Payment Links are top-level navigations and need no CSP
/// allowance; only embedded Stripe.js does.
pub const DEFAULT_CSP_DIRECTIVES: &str = "script-src https://js.stripe.com; \
     frame-src https://js.stripe.com https://hooks.stripe.com https://checkout.stripe.com; \
     connect-src https://api.stripe.com https://r.stripe.com";

/// Shared config variables readable by all blocks, writable only by admin.
///
/// These are NOT owned by any block — they're platform-level settings.
/// Blocks should NOT declare `WAFER_RUN_SHARED__` vars in their `config_keys`.
pub fn shared_config_vars() -> Vec<ConfigVar> {
    let mut vars = vec![
        ConfigVar::new(
            "WAFER_RUN_SHARED__APP_NAME",
            "Display name shown in UI and emails",
            "Impresspress",
        )
        .name("App Name")
        .input_type(InputType::Text),
        ConfigVar::new(
            "WAFER_RUN_SHARED__ALLOW_SIGNUP",
            "Allow new user registration",
            "true",
        )
        .name("Allow Signup")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            "WAFER_RUN_SHARED__ENABLE_OAUTH",
            "Enable third-party OAuth login",
            "false",
        )
        .name("Enable OAuth")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            "WAFER_RUN_SHARED__POST_LOGIN_REDIRECT",
            "URL to redirect to after login",
            "/b/admin/",
        )
        .name("Post-Login Redirect")
        .input_type(InputType::Text),
        ConfigVar::new(
            "WAFER_RUN_SHARED__FRONTEND_URL",
            "Frontend URL for checkout redirects",
            "http://localhost:5173",
        )
        .name("Frontend URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "WAFER_RUN_SHARED__SITE_URL",
            "Marketing site URL for docs and pricing links",
            "https://impresspress.org",
        )
        .name("Site URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "WAFER_RUN_SHARED__LOGO_URL",
            "Logo shown in header and emails",
            crate::ui::assets::logo_long_url(),
        )
        .name("Logo URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "WAFER_RUN_SHARED__PRIMARY_COLOR",
            "Brand accent (CSS color) for buttons, links, and highlights; blank keeps the default",
            "",
        )
        .name("Primary Color")
        .input_type(InputType::Text),
        ConfigVar::new(
            "WAFER_RUN_SHARED__LOGO_ICON_URL",
            "Small icon logo (used when sidebar is collapsed)",
            crate::ui::assets::logo_icon_url(),
        )
        .name("Logo Icon URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "WAFER_RUN_SHARED__AUTH_LOGO_URL",
            "Logo on login/signup pages (falls back to Logo URL)",
            "",
        )
        .name("Auth Logo URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "WAFER_RUN_SHARED__FAVICON_URL",
            "Browser tab icon",
            crate::ui::assets::favicon_url(),
        )
        .name("Favicon URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS",
            "Allow users to create their own products",
            "false",
        )
        .name("User Products")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            "WAFER_RUN_SHARED__ENVIRONMENT",
            "Runtime environment (development/production)",
            "development",
        )
        .name("Environment")
        .input_type(InputType::Text),
        ConfigVar::new(
            "WAFER_RUN_SHARED__HAS_DISPATCHER_BINDING",
            "Whether this project has a dispatcher service binding",
            "false",
        )
        .name("Dispatcher Binding")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            "WAFER_RUN_SHARED__HAS_LANDING_PAGE",
            "Serve a static landing page (wafer-run/web) at `/` instead of \
             redirecting anonymous visitors to the login page",
            "false",
        )
        .name("Has Landing Page")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            "WAFER_RUN_SHARED__EMBEDDED_SCRIPTS",
            "Comma-separated module-script URLs injected into every SSR page \
             (e.g. /webllm-engine.js for browser WebLLM). Native deployments \
             leave this empty.",
            "",
        )
        .name("Embedded Scripts")
        .input_type(InputType::Text),
        ConfigVar::new(
            CORS_ALLOWED_ORIGINS_KEY,
            "Origins permitted to make cross-origin API requests (comma-\
             separated, or `*`). Required for a static/cross-origin site to \
             embed the products storefront widget. Empty denies all cross-\
             origin requests (fail closed).",
            "",
        )
        .name("CORS Allowed Origins")
        .input_type(InputType::Text),
        ConfigVar::new(
            CSP_DIRECTIVES_KEY,
            "Extra Content-Security-Policy directives, merged over a hard \
             baseline that can only be widened. Defaults to the Stripe origins \
             embedded Checkout requires; extend to allow additional embeds.",
            DEFAULT_CSP_DIRECTIVES,
        )
        .name("CSP Directives")
        .input_type(InputType::Text),
    ];
    // Auth-scoped shared vars (wafer-run/auth reads these; admin writes them).
    // Declared here rather than in the auth block's BlockInfo::config_keys because
    // WAFER_RUN_SHARED__* vars must not be claimed by any single block.
    vars.extend(crate::blocks::auth::config::auth_config_vars());
    vars
}

/// Look up a single `WAFER_RUN_SHARED__*` config var by key.
///
/// The settings pages assemble their sections by pulling the exact
/// [`ConfigVar`] metadata they want to show — shared vars come from here,
/// block-owned vars come from the block's own `info().config_keys` (via
/// [`var_in`]). This keeps [`ConfigVar`] the single source of truth: no page
/// re-declares a key's label/default/input_type in a parallel tuple table.
///
/// Panics in debug if the key isn't a known shared var — that's a programming
/// error (a settings page asking for a var that was never declared), caught at
/// the first test run rather than silently rendering an empty field.
pub fn shared_var(key: &str) -> ConfigVar {
    shared_config_vars()
        .into_iter()
        .find(|v| v.key == key)
        .unwrap_or_else(|| {
            debug_assert!(false, "settings page requested unknown shared var: {key}");
            ConfigVar::new(key, "", "")
        })
}

/// Look up a single config var by key within a block's own declared
/// `config_keys`. The companion to [`shared_var`] for block-owned vars.
///
/// Panics in debug if the key isn't declared by the block.
pub fn var_in(vars: &[ConfigVar], key: &str) -> ConfigVar {
    vars.iter()
        .find(|v| v.key == key)
        .cloned()
        .unwrap_or_else(|| {
            debug_assert!(false, "settings page requested undeclared block var: {key}");
            ConfigVar::new(key, "", "")
        })
}

/// Collect all known config variables: shared + all block-declared.
pub fn collect_all_config_vars(block_infos: &[wafer_run::BlockInfo]) -> Vec<ConfigVar> {
    let mut all = shared_config_vars();
    for info in block_infos {
        all.extend(info.config_keys.iter().cloned());
    }
    all
}

/// Derive the SCREAMING_SNAKE block prefix written to the
/// `impresspress__admin__variables.block` column from a `{org}/{block}` name.
///
/// This is the single source of truth for the `block` column value: the
/// boot-time auto-generated-secret seeder ([`crate::boot::seed_auto_generated`])
/// writes it, the `D1ConfigSource` queries by it, and admin migration 002
/// backfills the same shape from the `key` column's first two `__`-delimited
/// segments. All three must agree, so they all funnel through here.
///
/// Conversion rules:
/// - `-` → `_` (within each segment)
/// - `/` → `__` (segment separator)
/// - uppercase
///
/// Examples:
/// - `"wafer-run/auth"` → `"WAFER_RUN__AUTH"`
/// - `"wafer-run/sqlite"` → `"WAFER_RUN__SQLITE"`
/// - `"impresspress"` (org only) → `"IMPRESSPRESS"`
pub fn screaming_block(name: &str) -> String {
    let (org, block) = name.split_once('/').unwrap_or((name, ""));
    let org_upper = org.replace('-', "_").to_uppercase();
    if block.is_empty() {
        org_upper
    } else {
        let block_upper = block.replace('-', "_").to_uppercase();
        format!("{org_upper}__{block_upper}")
    }
}

/// Derive the `variables.block` column value from a *config key* (rather than
/// a block name), matching the SQL backfill in admin migration 002.
///
/// The block prefix is the key's first two `__`-delimited segments — e.g.
/// `WAFER_RUN__AUTH__JWT_SECRET` → `WAFER_RUN__AUTH`. A key with fewer than
/// two `__` separators (a shared `WAFER_RUN_SHARED__*` var, or any legacy
/// single-segment key) has no block and returns `""`. The empty string is the
/// in-memory stand-in for the migration's `NULL`: the boot seeder omits the
/// `block` column entirely when this is empty, leaving the row's `block` NULL,
/// exactly as the backfill would.
///
/// This MUST stay byte-for-byte equivalent to migration 002's `CASE` so a
/// row seeded by [`crate::boot`] and a row backfilled by the migration land on
/// the same `block` value (and therefore the same `D1ConfigSource` per-block
/// cache key).
pub fn key_block_prefix(key: &str) -> String {
    let Some(first) = key.find("__") else {
        return String::new();
    };
    // Look for a second `__` after the first separator.
    match key[first + 2..].find("__") {
        Some(rel) => key[..first + 2 + rel].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod shared_vars_tests {
    use super::{
        shared_config_vars, CORS_ALLOWED_ORIGINS_KEY, CSP_DIRECTIVES_KEY, DEFAULT_CSP_DIRECTIVES,
    };

    /// Every shared var must be declared exactly once. A duplicate key means
    /// two competing defaults for the same setting — which one the seeder
    /// writes and which one `shared_var()` (first-match `.find()`) shows in
    /// the settings UI silently diverge. This happened for real:
    /// `WAFER_RUN_SHARED__PRIMARY_COLOR` was declared twice, once with the
    /// pre-rebrand indigo `#6366f1` and once blank, leaking blue accents.
    #[test]
    fn shared_config_vars_have_unique_keys() {
        let vars = shared_config_vars();
        let mut seen = std::collections::HashSet::new();
        for v in &vars {
            assert!(
                seen.insert(v.key.clone()),
                "duplicate shared config var declaration: {}",
                v.key
            );
        }
    }

    /// The CORS and CSP middleware keys must be declared shared vars, and the
    /// CSP default must carry the Stripe origins embedded Checkout needs — the
    /// builder injects these into the wafer-run/cors and security-headers steps
    /// (`flows::register_site_main`), which have no other config channel. If a
    /// rename or a trimmed default slips through, cross-origin embeds break and
    /// embedded Stripe.js is CSP-blocked, exactly the regression these keys fix.
    #[test]
    fn cors_and_csp_middleware_vars_are_declared_with_stripe_defaults() {
        let vars = shared_config_vars();
        let keys: std::collections::HashSet<&str> = vars.iter().map(|v| v.key.as_str()).collect();
        assert!(
            keys.contains(CORS_ALLOWED_ORIGINS_KEY),
            "CORS allow-origins var must be a declared shared var"
        );
        let csp = vars
            .iter()
            .find(|v| v.key == CSP_DIRECTIVES_KEY)
            .expect("CSP directives var must be a declared shared var");
        assert_eq!(csp.default, DEFAULT_CSP_DIRECTIVES);
        for host in [
            "https://js.stripe.com",
            "https://checkout.stripe.com",
            "https://api.stripe.com",
        ] {
            assert!(
                DEFAULT_CSP_DIRECTIVES.contains(host),
                "default CSP must allow {host} for embedded Checkout"
            );
        }
    }
}

#[cfg(test)]
mod screaming_block_tests {
    use super::{key_block_prefix, screaming_block};

    #[test]
    fn two_segment_name() {
        assert_eq!(screaming_block("wafer-run/auth"), "WAFER_RUN__AUTH");
        assert_eq!(screaming_block("wafer-run/sqlite"), "WAFER_RUN__SQLITE");
    }

    #[test]
    fn org_only_name() {
        assert_eq!(screaming_block("impresspress"), "IMPRESSPRESS");
    }

    #[test]
    fn key_block_prefix_two_segments() {
        // Block-scoped key → first two `__`-segments, matching migration 002.
        assert_eq!(
            key_block_prefix("WAFER_RUN__AUTH__JWT_SECRET"),
            "WAFER_RUN__AUTH"
        );
        assert_eq!(
            key_block_prefix("IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET"),
            "IMPRESSPRESS__PRODUCTS"
        );
    }

    #[test]
    fn key_block_prefix_shared_and_legacy_are_null() {
        // One `__` (shared var) → NULL/empty.
        assert_eq!(key_block_prefix("WAFER_RUN_SHARED__ALLOW_SIGNUP"), "");
        // No `__` → NULL/empty.
        assert_eq!(key_block_prefix("LEGACY_KEY"), "");
    }

    #[test]
    fn key_block_prefix_matches_screaming_block_for_owned_keys() {
        // A block's auto-gen key prefix derived from the key must equal the
        // prefix derived from the block name, so the seeder and the migration
        // backfill agree.
        assert_eq!(
            key_block_prefix("WAFER_RUN__AUTH__JWT_SECRET"),
            screaming_block("wafer-run/auth")
        );
    }
}
