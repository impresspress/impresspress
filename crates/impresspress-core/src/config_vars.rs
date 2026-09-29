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

/// Worker var naming how many D1 queries one Worker invocation may run.
///
/// D1 queries are subrequests to a Cloudflare internal service, and the
/// Workers limits page caps those at 1,000 per invocation on Workers Free and
/// at the Worker's configured subrequest limit on Paid
/// (<https://developers.cloudflare.com/workers/platform/limits/#subrequests>).
/// D1's own limits page still lists 50 for Free
/// (<https://developers.cloudflare.com/d1/platform/limits/>), but a Free-plan
/// Worker runs a fresh database's `/_deploy/prepare` — over 250 D1 queries —
/// in one invocation, so 50 is not what D1 enforces there.
/// The default, [`D1_QUERIES_PER_INVOCATION_DEFAULT`], therefore fits every
/// plan. A lower value is for a Worker whose `limits.subrequests` is set
/// below 1,000, and it bounds `/_deploy/prepare` too, which applies every
/// pending migration in one invocation.
///
/// Shared by both sides of a deploy: `impresspress build`/`deploy --target
/// cloudflare` writes it into the generated `wrangler.toml` `[vars]` from
/// `impresspress.toml`'s `[cloudflare].d1_queries_per_invocation`, and the
/// Cloudflare worker reads it once per invocation as the limit every D1
/// service reports its statement budget against. A deploy-time Worker var,
/// never a `variables` row.
pub const D1_QUERIES_PER_INVOCATION_KEY: &str = "IMPRESSPRESS_D1_QUERIES_PER_INVOCATION";

/// The D1 queries per invocation a deploy that does not state
/// [`D1_QUERIES_PER_INVOCATION_KEY`] runs under: 1,000, which Workers Free
/// and Workers Paid both allow.
pub const D1_QUERIES_PER_INVOCATION_DEFAULT: u64 = 1000;

/// The most D1 queries per invocation a deploy may state: D1's limits page
/// gives 1,000 for Workers Paid, so a stated
/// [`D1_QUERIES_PER_INVOCATION_KEY`] above it could have the budget admit
/// writes D1 then refuses part-way.
pub const D1_QUERIES_PER_INVOCATION_MAX: u64 = 1000;

/// The fewest queries per invocation a deploy may state: more than a
/// request's audit-row reservation
/// ([`crate::after_response::AUDIT_ROW_STATEMENTS`]), or its handlers could
/// send nothing at all.
pub const D1_QUERIES_PER_INVOCATION_MIN: u64 = crate::after_response::AUDIT_ROW_STATEMENTS + 1;

/// Parse a [`D1_QUERIES_PER_INVOCATION_KEY`] value: a whole number from
/// [`D1_QUERIES_PER_INVOCATION_MIN`] to [`D1_QUERIES_PER_INVOCATION_MAX`],
/// surrounding whitespace ignored. The Worker applies it to the var it is
/// deployed with — whatever wrote it, a `wrangler_overrides_path` file
/// included — and the CLI to `[cloudflare].d1_queries_per_invocation`, so
/// both refuse a value out of range with the same words instead of running
/// a budget D1 does not honour.
pub fn parse_d1_queries_per_invocation(raw: &str) -> Result<u64, String> {
    match raw.trim().parse::<u64>() {
        Ok(limit)
            if (D1_QUERIES_PER_INVOCATION_MIN..=D1_QUERIES_PER_INVOCATION_MAX).contains(&limit) =>
        {
            Ok(limit)
        }
        _ => Err(format!(
            "{D1_QUERIES_PER_INVOCATION_KEY} is {raw:?}; it must be a whole number from \
             {D1_QUERIES_PER_INVOCATION_MIN} to {D1_QUERIES_PER_INVOCATION_MAX} (D1 runs at most \
             1000 queries per invocation; leave it unset unless the Worker's \
             limits.subrequests is lower)"
        )),
    }
}

/// What `impresspress__admin__request_logs` keeps: `all` (the default),
/// `errors`, or `off`. See [`crate::pipeline::RequestLogPolicy`].
///
/// Infrastructure-prefixed on purpose. `IMPRESSPRESS_*` with no `__` is what
/// [`is_infrastructure_key`] recognises, which makes this a deploy-time
/// operator decision rather than an admin-editable runtime toggle:
/// `blocks::config`'s `served_only_from_boot_map` answers it from the boot map
/// whatever the `variables` table holds, and `CONFIG_SET` refuses to write it.
/// Each target threads it onto both config surfaces at boot — the native CLI
/// from the process environment, the Cloudflare worker from a `wrangler.toml`
/// var through `CfEnvironment`. Absent means `all`, which is the behaviour
/// every existing deployment already has.
///
/// # Why this exists
///
/// A row per request on a public unauthenticated route means anyone can mint
/// rows by sending GETs. On Cloudflare D1 the cost is concrete — one
/// insert is 3 rows written (the row, the `id` PRIMARY KEY autoindex, the
/// `created_at` index), so 33,333 requests exhaust a free tier's entire
/// 100,000 writes/day. Measured on one production site on 2026-09-04: 4,042
/// requests/hour, about 291,000 rows/day written, and request logs were 100%
/// of all writes.
///
/// `errors` keeps the only field an edge log cannot reconstruct — the app's
/// own `error_message` on a 5xx — and drops the rest, which Cloudflare's
/// request analytics already records for free.
pub const REQUEST_LOG_CONFIG_KEY: &str = "IMPRESSPRESS_REQUEST_LOG";

/// Shared config key: the wordmark image shown in the header and on auth
/// pages. Blank means "no wordmark" — the templates then render the app name
/// as text beside the brand icon.
///
/// Named because two other places have to agree with the declaration below:
/// [`crate::ui::SiteConfig::load`] reads it, and
/// [`crate::blocks::admin::settings::seed_defaults`] repairs rows that still
/// carry a built-in asset URL this build no longer serves — the removed
/// raster wordmark included (see
/// [`crate::ui::assets::is_stale_builtin_asset_url`]).
pub const LOGO_URL_KEY: &str = "WAFER_RUN_SHARED__LOGO_URL";

/// Shared config key: whether third-party (OAuth) sign-in is offered.
pub const ENABLE_OAUTH_KEY: &str = "WAFER_RUN_SHARED__ENABLE_OAUTH";

/// Shared config key: the deployment's display name — page titles, the
/// auth pages, and the subject line and From name of every email it sends.
pub const APP_NAME_KEY: &str = "WAFER_RUN_SHARED__APP_NAME";

/// Declared default for [`APP_NAME_KEY`], and the value a reader falls back
/// to when the row is missing or blank. Spelled once so the product name
/// lives here rather than in each block that shows it.
pub const DEFAULT_APP_NAME: &str = "Impresspress";

/// Shared config key: whether new users may sign up.
pub const ALLOW_SIGNUP_KEY: &str = "WAFER_RUN_SHARED__ALLOW_SIGNUP";

/// Shared config key: where a user lands after signing in.
pub const POST_LOGIN_REDIRECT_KEY: &str = "WAFER_RUN_SHARED__POST_LOGIN_REDIRECT";

/// Shared config key: the frontend origin checkout redirects return to.
pub const FRONTEND_URL_KEY: &str = "WAFER_RUN_SHARED__FRONTEND_URL";

/// Shared config key: the brand accent colour; blank keeps the default.
pub const PRIMARY_COLOR_KEY: &str = "WAFER_RUN_SHARED__PRIMARY_COLOR";

/// Shared config key: the small icon logo — the sidebar brand mark.
pub const LOGO_ICON_URL_KEY: &str = "WAFER_RUN_SHARED__LOGO_ICON_URL";

/// Shared config key: the logo on the auth pages; falls back to
/// [`LOGO_URL_KEY`].
pub const AUTH_LOGO_URL_KEY: &str = "WAFER_RUN_SHARED__AUTH_LOGO_URL";

/// Shared config key: the headline on the auth pages' brand panel.
pub const AUTH_HEADLINE_KEY: &str = "WAFER_RUN_SHARED__AUTH_HEADLINE";

/// Shared config key: the sub-line under [`AUTH_HEADLINE_KEY`].
pub const AUTH_TAGLINE_KEY: &str = "WAFER_RUN_SHARED__AUTH_TAGLINE";

/// Shared config key: the browser tab icon.
pub const FAVICON_URL_KEY: &str = "WAFER_RUN_SHARED__FAVICON_URL";

/// Shared config key: the runtime environment, `development` or
/// `production`.
pub const ENVIRONMENT_KEY: &str = "WAFER_RUN_SHARED__ENVIRONMENT";

/// Shared config key: whether this project has a dispatcher service binding.
pub const HAS_DISPATCHER_BINDING_KEY: &str = "WAFER_RUN_SHARED__HAS_DISPATCHER_BINDING";

/// Shared config key: whether `/` serves a static landing page instead of
/// redirecting anonymous visitors to the login page.
pub const HAS_LANDING_PAGE_KEY: &str = "WAFER_RUN_SHARED__HAS_LANDING_PAGE";

/// Shared config key: comma-separated module-script URLs injected into every
/// server-rendered page.
pub const EMBEDDED_SCRIPTS_KEY: &str = "WAFER_RUN_SHARED__EMBEDDED_SCRIPTS";

/// Shared config key: whether signed-in users may create and sell their own
/// products, not only the site admin.
pub const ALLOW_USER_PRODUCTS_KEY: &str = "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS";

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
/// survive regardless of what is set here, and a value the merge would refuse
/// in part is refused whole on write (`check_csp_directives`).
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

/// Default headline for the auth-split brand panel (login/signup/reset/etc.
/// left-hand navy column) — see [`crate::ui::components::auth_panel`].
/// White-label deployments override via [`AUTH_HEADLINE_KEY`].
pub const DEFAULT_AUTH_HEADLINE: &str = "The backend that lifts its own weight.";

/// Default sub-line under [`DEFAULT_AUTH_HEADLINE`] on the login page (the
/// only auth-split page that doesn't already pass its own page-specific
/// tagline). Overridable via [`AUTH_TAGLINE_KEY`]; blank hides
/// the tagline entirely.
pub const DEFAULT_AUTH_TAGLINE: &str = "One binary. Batteries included. No lock-in.";

/// Shared config variables readable by all blocks, writable only by admin.
///
/// These are NOT owned by any block — they're platform-level settings.
/// Blocks should NOT declare `WAFER_RUN_SHARED__` vars in their `config_keys`.
pub fn shared_config_vars() -> Vec<ConfigVar> {
    let mut vars = vec![
        ConfigVar::new(
            APP_NAME_KEY,
            "Display name shown in UI and emails",
            DEFAULT_APP_NAME,
        )
        .name("App Name")
        .input_type(InputType::Text),
        ConfigVar::new(
            ALLOW_SIGNUP_KEY,
            "Allow new user registration",
            "true",
        )
        .name("Allow Signup")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            ENABLE_OAUTH_KEY,
            "Enable third-party OAuth login",
            "false",
        )
        .name("Enable OAuth")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            POST_LOGIN_REDIRECT_KEY,
            "URL to redirect to after login",
            "/b/admin/",
        )
        .name("Post-Login Redirect")
        .input_type(InputType::Text),
        ConfigVar::new(
            FRONTEND_URL_KEY,
            "Frontend URL for checkout redirects",
            "http://localhost:5173",
        )
        .name("Frontend URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            LOGO_URL_KEY,
            "Wordmark image shown in the header and on auth pages; blank shows the app name as text next to the icon",
            "",
        )
        .name("Logo URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            PRIMARY_COLOR_KEY,
            "Brand accent (CSS color) for buttons, links, and highlights; blank keeps the default",
            "",
        )
        .name("Primary Color")
        .input_type(InputType::Text),
        ConfigVar::new(
            LOGO_ICON_URL_KEY,
            "Small icon logo (sidebar brand mark; the only mark shown when the sidebar is collapsed)",
            &crate::ui::assets::logo_icon_url(),
        )
        .name("Logo Icon URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            AUTH_LOGO_URL_KEY,
            "Logo on login/signup pages (falls back to Logo URL)",
            "",
        )
        .name("Auth Logo URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            AUTH_HEADLINE_KEY,
            "Headline on the login/signup/etc. left-hand brand panel",
            DEFAULT_AUTH_HEADLINE,
        )
        .name("Auth Headline")
        .input_type(InputType::Text),
        ConfigVar::new(
            AUTH_TAGLINE_KEY,
            "Sub-line under the brand panel headline, shown on the login page \
             (other auth pages default to their own page-specific line); \
             blank hides it",
            DEFAULT_AUTH_TAGLINE,
        )
        .name("Auth Tagline")
        .input_type(InputType::Text),
        ConfigVar::new(
            FAVICON_URL_KEY,
            "Browser tab icon",
            &crate::ui::assets::favicon_url(),
        )
        .name("Favicon URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            ALLOW_USER_PRODUCTS_KEY,
            "Allow users to create their own products",
            "false",
        )
        .name("User Products")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            ENVIRONMENT_KEY,
            "Runtime environment (development/production)",
            "development",
        )
        .name("Environment")
        .input_type(InputType::Text),
        ConfigVar::new(
            HAS_DISPATCHER_BINDING_KEY,
            "Whether this project has a dispatcher service binding",
            "false",
        )
        .name("Dispatcher Binding")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            HAS_LANDING_PAGE_KEY,
            "Serve a static landing page (wafer-run/web) at `/` instead of \
             redirecting anonymous visitors to the login page",
            "false",
        )
        .name("Has Landing Page")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            EMBEDDED_SCRIPTS_KEY,
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
             embedded Checkout requires; extend to allow additional embeds. \
             Visible ASCII only; script directives take exact https:// hosts \
             (no wildcards), report-uri only a path on this site, and \
             frame-ancestors is not settable here.",
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

/// The value rule a config key's reader applies, declared by the block that
/// reads it: `check` refuses exactly the values the reader cannot use.
///
/// Run by every write surface ([`crate::util::validate_config_value`]) and by
/// both boot seeders (`platform_state::variables::seed_and_load`,
/// `admin::settings::seed_defaults`), so a value that reaches the table from
/// the admin UI, `CONFIG_SET` or the process environment is always one its
/// reader accepts. Unlike the `_URL` SSRF check, which guards untrusted web
/// input and deliberately does not apply to the environment, these rules are
/// about what the value means, whoever supplies it.
pub struct ConfigValueRule {
    pub key: &'static str,
    pub check: fn(&str) -> Result<(), String>,
}

/// Every declared [`ConfigValueRule`], gathered from the blocks that own them
/// and the shared ones this module declares.
pub fn config_value_rules() -> Vec<ConfigValueRule> {
    let mut rules = vec![ConfigValueRule {
        key: CSP_DIRECTIVES_KEY,
        check: check_csp_directives,
    }];
    rules.extend(crate::blocks::auth::config::config_value_rules());
    rules
}

/// [`CSP_DIRECTIVES_KEY`]'s rule: a policy `wafer-run/security-headers`
/// takes whole.
///
/// The block merges this value over its baseline at Init. A character no
/// header can carry (a pasted smart quote, an NBSP) fails that Init, so the
/// runtime does not boot; anything else the merge refuses — a host wildcard
/// or an `http://` host in a script directive, an off-origin `report-uri`,
/// `frame-ancestors`, a repeated directive — is left out of the header with
/// only a log line to say so. Refusing both here, where the admin (or the
/// environment seeder) can still correct the value, is what keeps a saved
/// value and the header sent from disagreeing. The merge is run against an
/// empty baseline, so the one refusal that depends on the block's own
/// baseline — widening its `base-uri` / `form-action` — is still only
/// logged.
fn check_csp_directives(value: &str) -> Result<(), String> {
    let refused = wafer_block_security_headers::merge_csp("", value).refused;
    if refused.is_empty() {
        return Ok(());
    }
    let reasons: Vec<String> = refused.iter().map(ToString::to_string).collect();
    Err(format!(
        "the Content-Security-Policy would leave out {}",
        reasons.join("; ")
    ))
}

/// Run `key`'s declared [`ConfigValueRule`] on `value`; `Ok` for a key that
/// declares none.
pub fn check_config_value(key: &str, value: &str) -> Result<(), String> {
    config_value_rules()
        .iter()
        .filter(|rule| rule.key == key)
        .try_for_each(|rule| (rule.check)(value))
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

/// The one truth table for a boolean flag: trimmed, ASCII-case-insensitive,
/// `1` / `true` / `yes` / `on`.
///
/// Three tables were in the tree before this was the only one — `{1,true,
/// yes,on}` in products and tickets, `{true,1}` case-sensitive in the auth
/// API handlers, and exactly `"true"` on the auth pages and three products
/// paths. The same key therefore had two answers on two surfaces:
/// `WAFER_RUN_SHARED__ALLOW_SIGNUP=1` opened the signup API and hid the
/// signup link, and `IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX=1` turned tax on
/// in the money path while the settings page drew the toggle off.
///
/// Table A wins because it is the superset: every value that was true under
/// either of the others is true here, so no deployment whose config works
/// today stops working.
///
/// This is the predicate, not the reader — [`get_bool`] reads a config key
/// through it, [`form_bool`] reads a posted form field, and
/// [`crate::ui::settings_form`] renders a checkbox from it. Anything holding
/// a string that means yes-or-no asks here.
///
/// Not for *database* values: `auth::repo`, `tickets::service` and
/// `llm::schema` decode stored JSON, where `Bool` and `Number` are the real
/// cases and one of them deliberately fails open. Those stay separate.
pub fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Read a boolean config key through the config client and [`is_truthy`].
///
/// `default` is the value the key carries when it is unset — passed as a
/// `bool` rather than a string so a call site cannot spell a default that
/// the truth table then reads the other way.
///
/// Only an unset key reads as `default`. A failed read — a WRAP denial, a
/// transport or decode failure, an absent config block — is returned, so a
/// flag the caller may not read is never mistaken for its default.
pub async fn get_bool(
    ctx: &dyn wafer_run::context::Context,
    key: &str,
    default: bool,
) -> Result<bool, wafer_run::WaferError> {
    let value =
        wafer_core::clients::config::get_default(ctx, key, if default { "true" } else { "false" })
            .await?;
    Ok(is_truthy(&value))
}

/// Read a posted form field through [`is_truthy`]. An absent field is
/// `false` — an unchecked HTML checkbox posts nothing at all, and a checked
/// one posts `on`.
pub fn form_bool(form: &std::collections::HashMap<String, String>, key: &str) -> bool {
    form.get(key).is_some_and(|value| is_truthy(value))
}

/// Collect all known config variables: shared, block-declared, and the
/// declared vars that belong to no `BlockInfo`.
///
/// That last group is not a corner. `auth::config::auth_identity_config_vars`
/// is `ConfigVar`-declared and rendered by `auth_ui::pages::settings` through
/// `ui::settings_form`, but deliberately contributed to no `BlockInfo` —
/// there is no standalone `wafer-run/auth` block, `auth/` being a library
/// module. Iterating `block_infos` alone therefore called
/// `WAFER_RUN__AUTH__REQUIRE_VERIFICATION` and `..._ALLOWED_EMAIL_DOMAINS`
/// undeclared, and every rule keyed on [`is_declared_key`] treated two
/// ordinary admin toggles as ad hoc keys: stored sensitive by
/// [`is_sensitive_by_default_when_created`], masked on all three read
/// surfaces, unclearable behind the sensitive-empty guard, dropped from every
/// seed bundle, and never lowered again, since the repair pass only lowers a
/// DECLARED key. Recovery was delete-and-recreate.
///
/// So "declared" means declared, wherever the declaration lives. A new group
/// of `ConfigVar`s with no `BlockInfo` belongs in this function.
pub fn collect_all_config_vars(block_infos: &[wafer_run::BlockInfo]) -> Vec<ConfigVar> {
    let mut all = shared_config_vars();
    for info in block_infos {
        all.extend(info.config_keys.iter().cloned());
    }
    all.extend(crate::blocks::auth::config::auth_identity_config_vars());
    all
}

/// Derive the SCREAMING_SNAKE block prefix written to the
/// `impresspress__admin__variables.block` column from a `{org}/{block}` name.
///
/// This is the single source of truth for the `block` column value: the
/// boot-time auto-generated-secret seeder ([`crate::platform_state::variables::seed_auto_generated`])
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
/// row seeded by [`crate::platform_state::variables`] and a row backfilled by the migration land on
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

/// The SEC-060 naming convention: a key ending `_SECRET` or `_KEY` holds a
/// secret whatever else is known about it.
///
/// Spelled once because the write path ([`is_sensitive_for_storage`], deciding
/// what the `sensitive` column gets) and the read path
/// (`util::is_sensitive_key`, deciding what gets masked) have to apply the
/// identical rule. They disagreed once already and a password was served in
/// clear for it.
pub fn has_sensitive_suffix(key: &str) -> bool {
    key.ends_with("_SECRET") || key.ends_with("_KEY")
}

/// Every declared config key whose `ConfigVar` says it holds a secret
/// (`ConfigVar::is_sensitive()`, i.e. `InputType::Password`).
///
/// Memoized: the declared set is fixed for the life of the process (it is
/// assembled from compile-time `BlockInfo`s and cargo features), while building
/// it constructs every block — far too expensive to repeat per row written,
/// and `seed_defaults` writes one row per declared shared var on a fresh boot.
fn declared_sensitive_keys() -> &'static std::collections::HashSet<String> {
    &declared_key_sets().sensitive
}

/// Every config key some `ConfigVar` declares — shared or block-owned.
///
/// Memoized for the same reason as [`declared_sensitive_keys`]: fixed for the
/// process, expensive to build.
fn declared_keys() -> &'static std::collections::HashSet<String> {
    &declared_key_sets().all
}

/// The declared-key sets, built once.
///
/// One memo rather than two: both sets come from the same
/// `collect_all_config_vars(&all_block_infos())`, and building that constructs
/// every block — far too expensive to do twice, and a second copy is a second
/// thing to keep in step.
struct DeclaredKeys {
    /// Every declared config key.
    all: std::collections::HashSet<String>,
    /// Those whose declaration says they hold a secret — `InputType::Password`
    /// or `auto_generate`. `auto_generate` counts because
    /// `variables::seed_one_secret` hard-codes `sensitive: true` for such a
    /// var (it mints a random secret), and without it here the boot repair
    /// pass would read the declaration, see no `Password` type and no
    /// `_SECRET`/`_KEY` suffix, and LOWER the flag that seeder had just
    /// raised — publishing a generated secret and making it KV-cacheable.
    sensitive: std::collections::HashSet<String>,
}

fn declared_key_sets() -> &'static DeclaredKeys {
    static SETS: std::sync::OnceLock<DeclaredKeys> = std::sync::OnceLock::new();
    SETS.get_or_init(|| {
        let vars = collect_all_config_vars(&crate::blocks::all_block_infos());
        DeclaredKeys {
            sensitive: vars
                .iter()
                .filter(|v| v.is_sensitive() || v.auto_generate)
                .map(|v| v.key.clone())
                .collect(),
            all: vars.into_iter().map(|v| v.key).collect(),
        }
    })
}

/// Whether any block or the shared set declares `key`.
///
/// The complement is an **ad hoc** key: a row an operator created by hand,
/// about which this build knows nothing — not its type, not whether it holds a
/// secret. That ignorance is the whole reason
/// [`is_sensitive_by_default_when_created`] answers the way it does.
pub fn is_declared_key(key: &str) -> bool {
    declared_keys().contains(key)
}

/// The `sensitive` flag to store for a NEWLY created row whose creator did not
/// say — the default behind `VariablePatch::into_new`.
///
/// A declared key takes what its declaration implies
/// ([`is_sensitive_for_storage`]): the build knows what the var is, so guessing
/// would only ever contradict it. An **undeclared** key is stored sensitive,
/// because nothing here knows what it holds, and the cost of being wrong runs
/// one way — a masked value an admin can unmask is a nuisance, a published
/// secret is not.
///
/// This is the rule `admin::settings::handle_create` already applies to a POST
/// that omits the field ("Absent means sensitive. A caller that does not say is
/// protected"). It lives here so the PUT path gets it too: a
/// `PATCH /b/admin/api/settings/MY_SERVICE_TOKEN` on a key with no row takes
/// `upsert_by_key`'s create branch, whose patch carries no `sensitive`, and
/// used to store the row unflagged — so the same ad hoc key was protected
/// through POST and published through PUT.
///
/// ## The cost, weighed and accepted
///
/// A sensitive row is not exportable (`dev::data_snapshot::variable_is_exportable`
/// requires a clean `0`), so ad hoc keys default to being left out of a seed
/// bundle. Parity with POST wins over exportability: the export filter is
/// deliberately fail-closed, publishing an unknown value into a bundle that
/// travels to another deployment is the irreversible mistake, and an operator
/// who wants an ad hoc row to travel can clear its sensitive flag in the admin
/// UI — a deliberate act, which is exactly the signal the export filter is
/// looking for. What the export must NOT do is drop such rows in silence, so it
/// logs each one it leaves behind.
///
/// This is also narrower than it looks: POST has defaulted to sensitive since
/// before this rule existed, so ad hoc rows created through the admin UI were
/// already non-exportable. Only the PUT-created ones change, and they change to
/// match.
pub fn is_sensitive_by_default_when_created(key: &str) -> bool {
    is_sensitive_for_storage(key) || !is_declared_key(key)
}

/// The `sensitive` column a variables-table row for `key` must be written
/// with.
///
/// A **declared** var answers for itself — `ConfigVar::is_sensitive()`, i.e.
/// `InputType::Password` — because the declaration is the only thing that
/// knows `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` holds a password
/// despite ending in neither `_SECRET` nor `_KEY`. The suffix convention
/// applies on top, so an undeclared key that SPELLS itself a secret is caught,
/// and so is a declared `*_SECRET` var whose `input_type` was left `Text`.
///
/// An undeclared key that does neither — `MY_SERVICE_TOKEN` — is false here,
/// and deliberately: this answers "what does the build know this key to be",
/// and about that key it knows nothing. Protecting it is a different question,
/// answered at creation by [`is_sensitive_by_default_when_created`].
///
/// It is deliberately a union rather than "ask the declaration, else the
/// suffix": every disagreement between the two resolves to *more* masking,
/// which is the only safe direction for a flag whose whole job is to keep a
/// value out of an API response.
///
/// `util::is_sensitive_key`, the read path, is the stored flag unioned with
/// THIS function — deliberately the same predicate, so the reader never
/// depends on the writer having got the column right. It did once, and a
/// legacy row written before this funnel existed served a bootstrap password
/// in the clear until a boot repaired it. The column is now a cache of the
/// answer rather than the only copy of it.
///
/// Still applied at `platform_state::variables::NewVariable::into_row`, the
/// funnel every row creation passes through, rather than at each call site: the
/// stored flag is what the admin UI's Sensitive control reads back, what
/// `variables::set` preserves for an ad hoc row the declaration knows nothing
/// about, and the only signal an EXPORT has to go on for such a row.
pub fn is_sensitive_for_storage(key: &str) -> bool {
    has_sensitive_suffix(key) || declared_sensitive_keys().contains(key)
}

/// [`is_sensitive_for_storage`] for a caller that already holds the
/// declaration, taking it from the `ConfigVar` in hand rather than looking the
/// key up in the memoized set.
///
/// The same three-way union, term for term: `Password`-typed, `auto_generate`,
/// or a `_SECRET`/`_KEY` suffix. It must be, or the two would disagree about
/// an `auto_generate` var that carries neither of the other markers, and
/// `seed_defaults` (which uses this) would write a flag the boot repair pass
/// (which uses the other) then changed back on the same data.
///
/// Read from the `ConfigVar` in hand rather than looked up by key, because
/// that is the right source for a caller reasoning about a declaration as
/// data: `seed_defaults` hashes declared metadata to decide whether the
/// declarations changed since the last seed, and a key-only rule would stop
/// that hash noticing when a var BECOMES a password.
pub fn is_sensitive_var(var: &ConfigVar) -> bool {
    var.is_sensitive() || var.auto_generate || has_sensitive_suffix(&var.key)
}

/// Whether `key` names infrastructure configuration: `IMPRESSPRESS_*` with no
/// `__` separator (`IMPRESSPRESS_RUN_MIGRATIONS`, `IMPRESSPRESS_DEPLOY_TOKEN`).
///
/// By the repo's naming rule these are infrastructure and never live in the
/// variables table — `impresspress-native`'s env collection deliberately never
/// seeds them. Contrast `IMPRESSPRESS__PRODUCTS__*`, which carries `__` and is
/// block-scoped config that does.
pub fn is_infrastructure_key(key: &str) -> bool {
    key.starts_with("IMPRESSPRESS_") && !key.contains("__")
}

/// Whether `key` is an internal, adapter-injected runtime key: bracketed in
/// double underscores, like `__IMPRESSPRESS_RUNTIME_KIND__` and
/// `__IMPRESSPRESS_BLOCK_SETTINGS_JSON__`.
///
/// A target's boot code sets these directly; they are never set from env or
/// the variables table, which is why they carry no admin-writable prefix.
pub fn is_internal_key(key: &str) -> bool {
    key.len() > 4 && key.starts_with("__") && key.ends_with("__")
}

/// Whether `key` names something the RUNTIME owns rather than stored
/// configuration: infrastructure ([`is_infrastructure_key`]) or internal,
/// adapter-injected ([`is_internal_key`]).
///
/// Neither class is ever variables-table config, so a row carrying one is a
/// mistake or a forgery. Named once here because four surfaces have to agree
/// on it and would otherwise each carry their own copy of the pair:
/// `admin::ops::reject_runtime_owned_key` (the admin write path),
/// `blocks::config`'s `served_only_from_boot_map` (the read path, which
/// answers these from the boot map whatever the table holds),
/// `dev::data_snapshot::import` (the seed-bundle write path), and
/// `platform_state::variables::seed_and_load` (the process-environment write
/// path).
///
/// The last of those is defence in depth rather than the thing standing
/// between the process environment and the table: on native
/// `impresspress_server::filter_to_declared_keys` drops every undeclared key
/// before `seed_and_load` sees it, and no runtime-owned key is declared, so
/// the refusal has already happened upstream. It is still made here for a
/// caller that assembles its own batch.
pub fn is_runtime_owned_key(key: &str) -> bool {
    is_infrastructure_key(key) || is_internal_key(key)
}

/// Whether `key`'s value must come from THIS instance rather than from stored
/// or imported data: everything [`is_runtime_owned_key`] names, plus the JWT
/// signing secret.
///
/// The secret is the one key that is legitimately a variables-table row — an
/// operator rotating it is a real action, which is why
/// `admin::ops::reject_runtime_owned_key` deliberately does NOT refuse it —
/// and which must nevertheless never arrive from somewhere else.
/// `platform_state::variables::seed_jwt_secret` writes through
/// `insert_if_absent`, so a row that is already present wins and
/// auto-generation never fires; boot then signs every session JWT and CSRF
/// token with whatever that row holds. A seed bundle shared between instances
/// would hand each of them one signing secret its author knows.
///
/// That is also why this is a wider set than [`is_runtime_owned_key`] and not
/// a replacement for it: the admin write path wants the narrow one, while
/// `blocks::config`'s `served_only_from_boot_map` (a row must not rotate the
/// key under a running process) and `dev::data_snapshot` (a bundle must not
/// carry it in either direction) want this one.
pub fn is_instance_owned_key(key: &str) -> bool {
    key == crate::blocks::auth::JWT_SECRET_KEY || is_runtime_owned_key(key)
}

/// Whether `key` holds a credential that is consumed ONCE at provisioning and
/// is inert afterwards, so clearing it cannot break anything that is running.
///
/// **Exactly one key: the bootstrap admin password.** The distinction is not
/// about who reads these values but about which branch inside
/// `auth::bootstrap::run` populates `wafer_run__auth__users`, because that
/// table is the early-return that makes a value spent. `run` is called from
/// `AuthServiceImpl::init` on EVERY boot, and it has three branches:
///
/// 1. email + password → `bootstrap_with_email_password` inserts a `users` row
///    and a `local_credentials` row. From the next boot on, `run` returns
///    early and never reads the password again: it survives as an argon2 hash,
///    and the plaintext row is a spent copy no login consults. Clearable.
/// 2. token only → `bootstrap_with_token` inserts into `bootstrap_tokens` and
///    **creates no user at all**. `users` stays empty, so every subsequent boot
///    re-runs this branch, re-reads the plaintext token, and mints a fresh 24h
///    row from it. The stored value is a LIVE credential that is continuously
///    reissued, not a spent copy.
/// 3. neither → nothing.
///
/// So `BOOTSTRAP_ADMIN_TOKEN` is deliberately NOT here. Exempting it from the
/// sensitive-empty guard is a lockout: on a deployment provisioned by token
/// with no admin user yet, clearing the row lets the outstanding
/// `bootstrap_tokens` row expire within 24h with nothing to regenerate it and
/// no admin path left — and on Cloudflare there is no process environment to
/// re-seed from, so there is no route back at all. A token that has already
/// been redeemed into an admin user is inert, but nothing in the row says
/// whether it has, and guessing wrong one way is an inconvenience while
/// guessing wrong the other locks the operator out of their own deployment.
///
/// `BOOTSTRAP_ADMIN_EMAIL` is not here either, for a different reason: it stays
/// live after provisioning — `auth::helpers::TokenGrant::resolve` and
/// `initial_role_for` read it on every signup and token mint to decide who is
/// admin — so it is ordinary config, and not sensitive.
///
/// Exists because `admin::ops::update_variable` refuses to clear a sensitive
/// value ("would break auth"), and that reasoning does not reach a spent
/// password. Once `is_sensitive_for_storage` started flagging it — it is
/// declared `InputType::Password` — the guard began refusing the clear, while
/// `delete_variable` and the Variables page's `key_is_deletable` already refuse
/// to delete a declared `WAFER_RUN_SHARED__*` row. A bootstrapped deployment
/// would have been left holding a plaintext admin password it no longer needs
/// and cannot remove by any route.
pub fn is_provisioning_only_key(key: &str) -> bool {
    key == crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY
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
    /// `WAFER_RUN_SHARED__SITE_URL` was declared with a hardcoded marketing
    /// domain as its default, and its one reader was an email template
    /// nothing sent. A declaration is not inert: `seed_defaults` writes it
    /// into every deployment's `variables` table and the admin Variables page
    /// lists it. Nothing may declare it again — shared or block-owned.
    #[test]
    fn the_retired_site_url_var_is_declared_nowhere() {
        const RETIRED: &str = "WAFER_RUN_SHARED__SITE_URL";
        assert!(
            shared_config_vars().iter().all(|v| v.key != RETIRED),
            "{RETIRED} must not be a shared var"
        );
        assert!(
            !super::is_declared_key(RETIRED),
            "{RETIRED} must not be declared by any block"
        );
    }
}

#[cfg(test)]
mod csp_rule_tests {
    use super::{check_config_value, CSP_DIRECTIVES_KEY, DEFAULT_CSP_DIRECTIVES};
    use crate::test_support::TestContext;

    /// A value rule for an undeclared key would never run where an export is
    /// named: the native seeder only sees declared keys, and
    /// `variables::usable_env_exports` drops a refused export without logging
    /// on the strength of that.
    #[test]
    fn every_value_rule_names_a_declared_key() {
        for rule in super::config_value_rules() {
            assert!(
                super::is_declared_key(rule.key),
                "{} has a value rule but no declaring ConfigVar",
                rule.key
            );
        }
    }

    /// The shipped default is a policy the security-headers merge takes whole.
    #[test]
    fn the_default_policy_passes_its_own_rule() {
        assert_eq!(
            check_config_value(CSP_DIRECTIVES_KEY, DEFAULT_CSP_DIRECTIVES),
            Ok(())
        );
        assert_eq!(check_config_value(CSP_DIRECTIVES_KEY, ""), Ok(()));
    }

    /// Each of these makes `wafer-run/security-headers` fail its Init (the
    /// smart quote) or leave part of the policy out of the header. A saved
    /// value must be the policy that is sent, and never one that stops the
    /// runtime from booting.
    #[test]
    fn a_policy_the_merge_would_cut_is_refused() {
        for bad in [
            "script-src \u{2018}self\u{2019}",
            "script-src\u{a0}https://cdn.example.com",
            "script-src https://*.example.com",
            "script-src http://cdn.example.com",
            "report-uri https://collector.example/r",
            "frame-ancestors *",
            "img-src https:; IMG-SRC data:",
        ] {
            assert!(
                check_config_value(CSP_DIRECTIVES_KEY, bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    /// The rule runs where an admin writes the key: `config.set` refuses the
    /// value and stores nothing, so the next boot still reads the old one.
    #[tokio::test]
    async fn config_set_refuses_a_policy_that_would_break_boot() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let err = wafer_core::clients::config::set(
            &ctx,
            CSP_DIRECTIVES_KEY,
            "script-src \u{2018}self\u{2019}",
        )
        .await
        .expect_err("a smart-quoted policy must not be stored");
        assert_eq!(err.code, wafer_run::ErrorCode::InvalidArgument);
        assert_eq!(
            wafer_core::clients::config::get_optional(&ctx, CSP_DIRECTIVES_KEY)
                .await
                .expect("config read"),
            None,
            "nothing may be stored"
        );
    }
}

#[cfg(test)]
mod truth_table_tests {
    use std::collections::HashMap;

    use super::{form_bool, get_bool, is_truthy};
    use crate::test_support::TestContext;

    /// Truth table A, the superset of the three tables that were in the tree:
    /// trimmed, ASCII-case-insensitive, `1` / `true` / `yes` / `on`.
    #[test]
    fn the_truth_table_is_table_a() {
        for truthy in ["1", "true", "TRUE", "True", " on ", "on", "yes", "YES"] {
            assert!(is_truthy(truthy), "{truthy:?} must be true");
        }
        for falsy in ["0", "false", "FALSE", "", "   ", "bogus", "2", "onward"] {
            assert!(!is_truthy(falsy), "{falsy:?} must be false");
        }
    }

    #[tokio::test]
    async fn get_bool_reads_the_config_client_and_falls_back_to_the_default() {
        const UNSET_FLAG: &str = "WAFER_RUN_SHARED__UNSET_FLAG";
        const FLAG: &str = "WAFER_RUN_SHARED__FLAG";
        // Read as a block: any attributable caller may read a shared key.
        let mut ctx = TestContext::new().await;
        let reader = |ctx: &TestContext| ctx.clone().running_as("impresspress/tickets");
        assert!(get_bool(&reader(&ctx), UNSET_FLAG, true)
            .await
            .expect("read"));
        assert!(!get_bool(&reader(&ctx), UNSET_FLAG, false)
            .await
            .expect("read"));
        ctx.set_config(FLAG, "1");
        assert!(get_bool(&reader(&ctx), FLAG, false).await.expect("read"));
        ctx.set_config(FLAG, "no");
        assert!(!get_bool(&reader(&ctx), FLAG, true).await.expect("read"));
    }

    /// A read the caller is refused is an error, not the flag's default: a
    /// flag nobody may read must not quietly turn a feature on or off.
    #[tokio::test]
    async fn get_bool_returns_a_refused_read_instead_of_the_default() {
        // Block-scoped to `acme/widget`, so another block needs a grant.
        const FOREIGN_FLAG: &str = "ACME__WIDGET__FLAG";
        let mut ctx = TestContext::new().await;
        ctx.set_config(FOREIGN_FLAG, "1");
        let ctx = ctx.running_as("impresspress/tickets");
        let err = get_bool(&ctx, FOREIGN_FLAG, true)
            .await
            .expect_err("a refused read must not answer the default");
        assert_eq!(err.code, wafer_run::ErrorCode::PermissionDenied);
    }

    /// An HTML checkbox posts `on`, and an absent field means unchecked.
    #[test]
    fn form_bool_reads_a_posted_checkbox() {
        let form = HashMap::from([
            ("checked".to_string(), "on".to_string()),
            ("explicit".to_string(), "true".to_string()),
            ("blank".to_string(), String::new()),
        ]);
        assert!(form_bool(&form, "checked"));
        assert!(form_bool(&form, "explicit"));
        assert!(!form_bool(&form, "blank"));
        assert!(!form_bool(&form, "absent"));
    }
}

#[cfg(test)]
mod sensitivity_tests {
    use super::{has_sensitive_suffix, is_sensitive_for_storage, shared_config_vars, APP_NAME_KEY};

    /// The declaration is what knows a suffix-less key holds a password, and
    /// the suffix rule still catches an undeclared ad hoc key. Both halves,
    /// against the real declared set.
    #[test]
    fn the_declaration_and_the_suffix_rule_are_both_consulted() {
        let password_key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        assert!(
            !has_sensitive_suffix(password_key),
            "this key must be one the suffix rule cannot catch, or the test proves nothing"
        );
        assert!(is_sensitive_for_storage(password_key));
        assert!(is_sensitive_for_storage(
            crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY
        ));

        // Undeclared, caught by the suffix rule alone.
        assert!(is_sensitive_for_storage("X__Y__STRIPE_SECRET"));
        assert!(is_sensitive_for_storage("X__Y__MAILGUN_API_KEY"));

        // Declared and plainly not a secret.
        assert!(!is_sensitive_for_storage(APP_NAME_KEY));
        // Undeclared and not a secret.
        assert!(!is_sensitive_for_storage("SITE_TAGLINE"));
    }

    /// A `ConfigVar` attached to no `BlockInfo` is still DECLARED.
    ///
    /// `auth::config::auth_identity_config_vars` is rendered by
    /// `auth_ui::pages::settings` but deliberately contributed to no
    /// `BlockInfo`, so a collector that walked `block_infos` alone called these
    /// two ordinary admin toggles ad hoc. Every rule keyed on
    /// `is_declared_key` then treated them as unknown keys: stored sensitive by
    /// default, masked everywhere, unclearable, dropped from seed bundles, and
    /// never lowered, since the repair pass only lowers a declared key.
    #[test]
    fn a_config_var_with_no_block_info_is_still_declared() {
        for var in crate::blocks::auth::config::auth_identity_config_vars() {
            assert!(
                super::is_declared_key(&var.key),
                "{} is ConfigVar-declared and must not read as an ad hoc key",
                var.key
            );
            assert!(
                !super::is_sensitive_by_default_when_created(&var.key),
                "{} is a plain toggle; treating it as an unknown key would mask it forever",
                var.key
            );
        }
    }

    /// Every declared var the storage rule calls sensitive must also read as
    /// sensitive through `util::is_sensitive_key` once stored with that flag —
    /// the write path and the read path are a pair, and a var that only one of
    /// them recognises is the defect this rule exists to close.
    #[test]
    fn what_the_write_path_flags_the_read_path_masks() {
        let declared = shared_config_vars();
        for var in &declared {
            let stored = is_sensitive_for_storage(&var.key);
            if stored {
                assert!(
                    crate::util::is_sensitive_key(&var.key, 1),
                    "{} is stored sensitive but would not be masked",
                    var.key
                );
            }
            // And the key-only rule and the ConfigVar-in-hand rule must agree
            // term for term, or `seed_defaults` and the boot repair pass would
            // write different flags for the same declaration.
            assert_eq!(
                stored,
                super::is_sensitive_var(var),
                "the two spellings of the storage rule must agree: {}",
                var.key
            );
            assert_eq!(
                stored,
                var.is_sensitive() || var.auto_generate || has_sensitive_suffix(&var.key),
                "the storage rule must be exactly declaration OR auto-generate OR suffix: {}",
                var.key
            );
        }
    }
}

#[cfg(test)]
mod screaming_block_tests {
    use super::{key_block_prefix, screaming_block, ALLOW_SIGNUP_KEY};
    use crate::blocks::email::MAILGUN_API_KEY;

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
            key_block_prefix(crate::blocks::auth::JWT_SECRET_KEY),
            "WAFER_RUN__AUTH"
        );
        assert_eq!(key_block_prefix(MAILGUN_API_KEY), "IMPRESSPRESS__EMAIL");
    }

    #[test]
    fn key_block_prefix_shared_and_legacy_are_null() {
        // One `__` (shared var) → NULL/empty.
        assert_eq!(key_block_prefix(ALLOW_SIGNUP_KEY), "");
        // No `__` → NULL/empty.
        assert_eq!(key_block_prefix("LEGACY_KEY"), "");
    }

    #[test]
    fn key_block_prefix_matches_screaming_block_for_owned_keys() {
        // A block's auto-gen key prefix derived from the key must equal the
        // prefix derived from the block name, so the seeder and the migration
        // backfill agree.
        assert_eq!(
            key_block_prefix(crate::blocks::auth::JWT_SECRET_KEY),
            screaming_block("wafer-run/auth")
        );
    }
}

#[cfg(test)]
mod d1_queries_per_invocation_tests {
    use super::*;
    use crate::after_response::AUDIT_ROW_STATEMENTS;

    /// The shared parser — the Worker's as well as the CLI's — takes a limit
    /// from one more than the audit-row reservation to D1's maximum, and
    /// refuses everything else naming the var. A value at the reservation
    /// would leave a request's handlers nothing; one above 1000 would have
    /// the budget admit writes D1 refuses part-way.
    #[test]
    fn the_limit_must_exceed_the_reservation_and_not_exceed_d1s_maximum() {
        assert_eq!(D1_QUERIES_PER_INVOCATION_MIN, AUDIT_ROW_STATEMENTS + 1);
        let min = AUDIT_ROW_STATEMENTS + 1;
        for (raw, limit) in [
            (min.to_string(), min),
            ("50".to_string(), 50),
            (" 50 ".to_string(), 50),
            ("1000".to_string(), 1000),
        ] {
            assert_eq!(parse_d1_queries_per_invocation(&raw), Ok(limit), "{raw:?}");
        }
        for raw in [
            AUDIT_ROW_STATEMENTS.to_string(),
            "0".to_string(),
            "1001".to_string(),
            "-5".to_string(),
            "fifty".to_string(),
            String::new(),
        ] {
            let err = parse_d1_queries_per_invocation(&raw).expect_err(&raw);
            assert!(
                err.contains(D1_QUERIES_PER_INVOCATION_KEY),
                "{raw:?}: {err}"
            );
            // The refusal points at the default rather than at a per-plan
            // number: 1000 is what D1 allows on Workers Free and Paid alike.
            assert!(
                err.contains("leave it unset") && !err.contains("50 on Free"),
                "{raw:?}: {err}"
            );
        }
    }
}
