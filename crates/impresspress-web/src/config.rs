//! Browser-side variable + block-settings seeding.
//!
//! Thin wrappers over the shared `impresspress_core::platform_state` /
//! `impresspress_core::features` seeders, driving the browser's `BrowserDatabaseService` instead of the JS
//! `bridge::db_exec_raw` / `db_query_raw` strings the prior implementation used
//! (which hardcoded the `impresspress__admin__*` table names 17×). The seeding
//! logic — env/auto-gen/JWT vars and the #222 block-settings hash-gate — now
//! lives once in `impresspress-core`, shared by all three targets.
//!
//! PRECONDITION for both functions: `wafer.init_block(impresspress/admin)` must
//! have already run, so admin's migration has created the canonical
//! `impresspress__admin__variables` + `block_settings` tables. These functions
//! never create or pre-create the tables — admin's migration is the single
//! source of schema truth (the lesson of the #210/#211 schema-drift outage).

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    blocks::auth::config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
    config_vars::{EMBEDDED_SCRIPTS_KEY, HAS_LANDING_PAGE_KEY},
};
use wafer_core::interfaces::database::service::DatabaseService;

use crate::SandboxMode;

/// Seed the browser-only default variables, auto-generate declared secrets,
/// and return the full variable map. Browser-equivalent of the native
/// `variables::seed_and_load()` — there are no process env vars in the browser,
/// only the local defaults below plus auto-generated secrets.
/// `mode` is the *resolved* verdict from [`SandboxMode::resolve`], never the
/// raw `initialize({ dev })` request: on a build without `browser-devtools`
/// the sandbox does not exist, so a feature-off boot must leave a database
/// indistinguishable from one that never asked for a sandbox — which is why
/// `WAFER_RUN_SHARED__HAS_LANDING_PAGE` below is written on EVERY verdict
/// rather than only on the affirmative one. It follows
/// [`SandboxMode::runtime_present`]: an exported bundle always has a site (its
/// `seed/` guarantees one), so `/` must serve it there exactly as it does in
/// the workspace.
pub async fn seed_and_load_variables(
    db: &Arc<dyn DatabaseService>,
    mode: SandboxMode,
) -> Result<HashMap<String, String>, String> {
    // Browser-only defaults. All three keys ARE declared — the bootstrap pair
    // by `auth::config::auth_config_vars`, `EMBEDDED_SCRIPTS` by
    // `config_vars::shared_config_vars` — but every one of them declares an
    // EMPTY default, so nothing else supplies a value: `admin::settings::
    // seed_defaults` skips an empty declared default, `seed_auto_generated`
    // seeds only `auto_generate` vars, and the browser has no process
    // environment to source one from. It ships a self-contained local admin +
    // WebLLM wiring instead, so it seeds those values here.
    //
    // A stated flag rather than the create default, and the declarations are
    // why that is safe: `seed_if_absent` builds the `NewVariable` itself, and
    // `into_row` raises the password's flag from its `InputType::Password`
    // declaration whatever is passed.
    //
    // `INSERT OR IGNORE`: a prior boot or admin-UI edit always wins.
    impresspress_core::platform_state::variables::seed_if_absent(
        db,
        BOOTSTRAP_ADMIN_EMAIL_KEY,
        "admin@example.com",
        "Admin Email",
        "Admin account email",
        false,
    )
    .await?;
    impresspress_core::platform_state::variables::seed_if_absent(
        db,
        BOOTSTRAP_ADMIN_PASSWORD_KEY,
        "admin123",
        "Admin Password",
        "Admin account password",
        true,
    )
    .await?;
    // Inject the page-side WebLLM engine into every SSR-rendered page.
    // Native/server targets leave this var unset and skip the injection.
    impresspress_core::platform_state::variables::seed_if_absent(
        db,
        EMBEDDED_SCRIPTS_KEY,
        "/webllm-engine.js",
        "Embedded Scripts",
        "Module-type script URLs embedded in every page",
        false,
    )
    .await?;

    // The sandbox owns `/` — an editable site is the point of it, and an
    // exported one ships a site in its `seed/` — so the router must serve
    // `wafer-run/web` there instead of bouncing anonymous visitors to the
    // login page (design §7.3).
    //
    // FORCE-SET, not seeded, and force-set in BOTH directions.
    //
    // Not seeded, because this hook runs *after* the admin block's
    // `lifecycle(Init)`, which has already written every declared
    // `config_vars` default — and `WAFER_RUN_SHARED__HAS_LANDING_PAGE` is
    // declared, defaulting to `"false"`. `seed_if_absent` is
    // therefore a guaranteed no-op on this key, which is precisely the bug
    // Plan 1 Task 10's e2e caught: the sandbox published a site that `/` then
    // refused to serve.
    //
    // Both directions, because the browser database is keyed by ORIGIN and
    // outlives the bundle that wrote it. Serving a bundle without the
    // sandbox's runtime (one compiled without `browser-devtools`) to an
    // origin a sandbox bundle previously ran on would otherwise leave a stale
    // `"true"` in OPFS, and `/` would keep serving a site nothing can publish
    // to any more instead of redirecting anonymous visitors to the login
    // page. That directly contradicts what `SandboxMode::resolve` promises —
    // "a build without the feature must produce a runtime indistinguishable
    // from one that was never asked for a sandbox" — and a value only ever
    // written in one direction cannot deliver it.
    //
    // Writing `"false"` here overrides nothing an operator would have set: in
    // the browser target the sandbox is the ONLY producer of a landing page
    // (no web flow publishes a site — see `cli/flows/{sealed,embed}_web.rs`,
    // unlike their native counterparts), so without it there is nothing at `/`
    // to serve. `variables::set` writes nothing when the value already matches,
    // so the common case is a read.
    impresspress_core::platform_state::variables::set(
        db,
        HAS_LANDING_PAGE_KEY,
        if mode.runtime_present() {
            "true"
        } else {
            "false"
        },
        "Has Landing Page",
        "Serve a static landing page (wafer-run/web) at `/` instead of \
         redirecting anonymous visitors to the login page",
        // `Some(false)`, not `None`: this caller knows the key — a declared
        // boolean naming whether `/` serves a page — so it speaks for it
        // rather than taking the create default meant for a key nothing
        // declares.
        Some(false),
    )
    .await?;

    // Auto-generate declared secrets (incl. the auth JWT secret) and load the
    // full set back — the shared core path, over BrowserDatabaseService.
    impresspress_core::platform_state::variables::seed_and_load(db, &[]).await
}

/// Load + hash-gate-seed block settings from the browser database. Delegates to
/// the shared `impresspress_core::platform_state::block_settings::load_and_seed` over
/// `BrowserDatabaseService`, so the browser runs the exact #222 hash-gate
/// Cloudflare and native do.
///
/// A read error is always a genuine operational failure (OPFS/sql.js
/// corruption, quota exhaustion) — never the missing-table cold-start case,
/// which `DatabaseService::list` already tolerates internally. Propagates
/// rather than fabricating "every block enabled".
pub async fn load_block_settings(
    db: &Arc<dyn DatabaseService>,
) -> Result<impresspress_core::features::BlockSettings, String> {
    impresspress_core::platform_state::block_settings::load_and_seed(
        db,
        &impresspress_core::blocks::block_enabled_defaults(),
    )
    .await
    .map_err(|e| format!("load block settings: {e}"))
}
