//! `impresspress-web`'s own seeds: the [`AppSeed`] it hands the browser boot
//! funnel (`impresspress_browser::boot`).
//!
//! The funnel runs this after `wafer.init_block(impresspress/admin)` — so
//! admin's migration has created the canonical `impresspress__admin__variables`
//! table, which this never creates or pre-creates (the lesson of the
//! #210/#211 schema-drift outage) — and before it auto-generates the declared
//! secrets and loads the variables back, so what this writes is in the map
//! the remaining blocks' `Init` resolves from. The shared seeding (the JWT
//! secret, the #222 block-settings hash-gate) is the funnel's, not this
//! crate's.

use std::sync::Arc;

use impresspress_browser::boot::AppSeed;
use impresspress_core::{
    blocks::auth::config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
    config_vars::{shared_var, HAS_LANDING_PAGE_KEY},
    sandbox_mode::SandboxMode,
};
use wafer_core::interfaces::database::service::DatabaseService;

/// The bootstrap admin a browser build ships, and whether `/` serves a site.
///
/// What the sandbox contributes is this build's [`SandboxMode::COMPILED`]: on
/// a build without the sandbox (neither `browser-site-runtime` nor
/// `browser-devtools`) it does not exist, so such a boot must leave a
/// database indistinguishable from one that never had a sandbox — which is
/// why `WAFER_RUN_SHARED__HAS_LANDING_PAGE` below is written by EVERY build
/// rather than only by the ones with a site. It follows
/// [`SandboxMode::runtime_present`]: an exported site — a site build —
/// always has a site (its `seed/` guarantees one), so `/` must serve it
/// there exactly as it does in the workspace.
pub struct WebSeeds;

#[wafer_block::wafer_async_trait]
impl AppSeed for WebSeeds {
    async fn seed(&self, db: &Arc<dyn DatabaseService>) -> Result<(), String> {
        // Browser-only defaults. Both keys ARE declared, by
        // `auth::config::auth_config_vars`, but each declares an EMPTY default,
        // so nothing else supplies a value: `admin::settings::seed_defaults` skips
        // an empty declared default, `seed_auto_generated` seeds only
        // `auto_generate` vars, and the browser has no process environment to
        // source one from. It ships a self-contained local admin instead, so it
        // seeds those values here. (The page-side engine scripts are not seeded
        // config: the boot funnel publishes them as the runtime-owned
        // `ui::PAGE_ENGINE_SCRIPTS_CONFIG_KEY`.)
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
        // sandbox's runtime (one compiled without `browser-site-runtime`, which
        // `browser-devtools` implies) to an origin a sandbox or site bundle
        // previously ran on would otherwise leave a stale `"true"` in OPFS, and
        // `/` would keep serving a site nothing can publish to any more instead
        // of redirecting anonymous visitors to the login page. That directly
        // contradicts what `SandboxMode::Absent` promises — a runtime
        // indistinguishable from one that was never asked for a sandbox — and a
        // value only ever written in one direction cannot deliver it.
        //
        // Writing `"false"` here overrides nothing an operator would have set: in
        // the browser target the sandbox is the ONLY producer of a landing page
        // (no web flow publishes a site — see `cli/flows/{sealed,embed}_web.rs`,
        // unlike their native counterparts), so without it there is nothing at `/`
        // to serve. `variables::set` writes nothing when the value already matches,
        // so the common case is a read.
        //
        // The row's name and description are the declaration's, read from it
        // rather than written out again here.
        let declared = shared_var(HAS_LANDING_PAGE_KEY);
        impresspress_core::platform_state::variables::set(
            db,
            HAS_LANDING_PAGE_KEY,
            if SandboxMode::COMPILED.runtime_present() {
                "true"
            } else {
                "false"
            },
            &declared.name,
            &declared.description,
            // `Some(false)`, not `None`: this caller knows the key — a declared
            // boolean naming whether `/` serves a page — so it speaks for it
            // rather than taking the create default meant for a key nothing
            // declares.
            Some(false),
        )
        .await?;

        Ok(())
    }
}

/// [`WebSeeds`] through the real boot: the runtime factory booted on a fresh
/// database leaves the bootstrap admin and the `HAS_LANDING_PAGE` value this
/// build promises. CI runs it in the plain configuration (no sandbox, so `/`
/// redirects to the login page) and with `browser-site-runtime` (`/` serves
/// the site) — the two directions of the force-set.
#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use std::sync::Arc;

    use impresspress_browser::{boot::BrowserServices, database::test_support::fresh_db};
    use wafer_core::interfaces::database::service::DatabaseService;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;
    use crate::RuntimeFactory;

    #[wasm_bindgen_test]
    async fn a_booted_runtime_carries_the_web_seeds() {
        let db: Arc<dyn DatabaseService> = Arc::new(fresh_db().await);
        let factory = RuntimeFactory::new(
            BrowserServices::new(Vec::new()).expect("the browser services construct"),
        );
        factory.build(&[]).await.expect("the runtime boots");

        let vars = impresspress_core::platform_state::variables::load_all(&db)
            .await
            .expect("the variables load");
        assert_eq!(
            vars.get(BOOTSTRAP_ADMIN_EMAIL_KEY).map(String::as_str),
            Some("admin@example.com"),
        );
        assert_eq!(
            vars.get(BOOTSTRAP_ADMIN_PASSWORD_KEY).map(String::as_str),
            Some("admin123"),
        );
        // Stated from the cargo feature rather than from
        // `SandboxMode::COMPILED`, which the seed itself reads.
        let expected = if cfg!(feature = "browser-site-runtime") {
            "true"
        } else {
            "false"
        };
        assert_eq!(
            vars.get(HAS_LANDING_PAGE_KEY).map(String::as_str),
            Some(expected),
        );
    }
}
