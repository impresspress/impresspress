//! Browser-side variable + block-settings seeding.
//!
//! Thin wrappers over the shared `impresspress_core::boot` / `impresspress_core::features`
//! seeders, driving the browser's `BrowserDatabaseService` instead of the JS
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

use wafer_core::interfaces::database::service::DatabaseService;

/// Seed the browser-only default variables, auto-generate declared secrets,
/// and return the full variable map. Browser-equivalent of the native
/// `seed_and_load_variables()` — there are no process env vars in the browser,
/// only the local defaults below plus auto-generated secrets.
pub async fn seed_and_load_variables(
    db: &Arc<dyn DatabaseService>,
) -> Result<HashMap<String, String>, String> {
    // Browser-only defaults. These are not declared `ConfigVar`s (so the
    // auto-gen pass won't seed them) and there's no env to source them from —
    // the browser build ships a self-contained local admin + WebLLM wiring.
    // `INSERT OR IGNORE`: a prior boot or admin-UI edit always wins.
    impresspress_core::boot::seed_variable_if_absent(
        db,
        "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL",
        "admin@example.com",
        "Admin Email",
        "Admin account email",
        false,
    )
    .await?;
    impresspress_core::boot::seed_variable_if_absent(
        db,
        "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD",
        "admin123",
        "Admin Password",
        "Admin account password",
        true,
    )
    .await?;
    // Inject the page-side WebLLM engine into every SSR-rendered page.
    // Native/server targets leave this var unset and skip the injection.
    impresspress_core::boot::seed_variable_if_absent(
        db,
        "WAFER_RUN_SHARED__EMBEDDED_SCRIPTS",
        "/webllm-engine.js",
        "Embedded Scripts",
        "Module-type script URLs embedded in every page",
        false,
    )
    .await?;

    // Auto-generate declared secrets (incl. the auth JWT secret) and load the
    // full set back — the shared core path, over BrowserDatabaseService.
    impresspress_core::boot::seed_and_load_variables(db, &[]).await
}

/// Load + hash-gate-seed block settings from the browser database. Delegates to
/// the shared `impresspress_core::features::load_and_seed_block_settings` over
/// `BrowserDatabaseService`, so the browser runs the exact #222 hash-gate
/// Cloudflare and native do.
pub async fn load_block_settings(
    db: &Arc<dyn DatabaseService>,
) -> impresspress_core::features::BlockSettings {
    impresspress_core::features::load_and_seed_block_settings(db).await
}
