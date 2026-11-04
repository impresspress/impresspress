pub mod admin;
pub mod auth;
pub mod auth_ui;
pub mod body_limit;
pub mod config;
pub mod crud;
// The browser development sandbox's block: its runtime half under
// `site-runtime`, its workspace half under `block-dev` (which implies
// `site-runtime`; `dev`'s own module list says which module is which).
// Deliberately absent from `default` features and from
// `feature_block_manifest!` below: its constructor takes a `RuntimeControl`
// handle, and the sandbox's security model turns on the block not existing
// in a build that does not need it rather than on an admin toggle.
// Registration comes from the consumer via `ImpresspressBuilder::extra_block`
// (+ `add_route` for the workspace's routes).
#[cfg(feature = "site-runtime")]
pub mod dev;
pub mod email;
pub mod errors;
#[macro_use]
pub mod feature_block;
// `native-embedding` always implies `block-fastembed` (see Cargo.toml), so
// the native build still gets this module. wafer-site / wasm32 builds with
// neither feature drop the ONNX-runtime dep entirely.
#[cfg(feature = "block-fastembed")]
pub mod fastembed;
#[cfg(feature = "block-files")]
pub mod files;
#[cfg(feature = "block-legalpages")]
pub mod legalpages;
#[cfg(feature = "block-tickets")]
pub mod tickets;
// The LLM feature block compiles on every target that enables `block-llm`,
// including wasm32. `LlmBlock` holds `Arc<dyn ProviderAdmin>` (the
// provider-management seam), not the concrete reqwest/tokio
// `ProviderLlmService`, so the block module no longer drags the native HTTP
// stack. The `llm` feature is now just "native provider backend": it gates
// `providers::ProviderLlmService` (reqwest/stream + tokio) and is implied by
// nothing the block module itself needs. Browser/CF builds enable `block-llm`
// without `llm` and supply their own backend via
// `ImpresspressBuilder::llm_service` (e.g. `BrowserLlmService` in impresspress-web);
// the block holds a `NoopProviderAdmin` there.
#[cfg(feature = "block-llm")]
pub mod llm;
#[cfg(feature = "block-messages")]
pub mod messages;
#[cfg(feature = "block-products")]
pub mod products;
pub mod rate_limit;
pub mod router;
#[cfg(feature = "block-signal")]
pub mod signal;
pub mod storage;
pub mod system;
// Not gated on a target. The block is a thin wrapper over an injected
// `Arc<dyn EmbeddingService>` and contains nothing platform-specific; which
// runtimes *register* it is decided by whether a caller injected an embedding
// service (`ImpresspressBuilder::embedding_service`), which is a runtime fact,
// not a build-target one. Gating the module on `wasm32` meant a native caller
// that injected an embedding service got the vector backend registered and
// the block that serves it silently dropped.
pub mod transformers_embed;
#[cfg(feature = "block-userportal")]
pub mod userportal;
#[cfg(feature = "block-vector")]
pub mod vector;

/// The single `(feature-cfg, constructor)` manifest of impresspress feature
/// blocks whose constructors take **no arguments**.
///
/// It generates [`application_blocks`], the one list of the application
/// blocks this build compiles in — these, then the three whose constructors
/// take an argument ([`ApplicationBlockArgs`]): `impresspress/admin` (the
/// live `BlockSettings` handle), `impresspress/llm` (an
/// `Arc<dyn ProviderAdmin>`) and `impresspress/fastembed` (the embedder's
/// model cache directory). Everything that enumerates the application blocks
/// derives from that list:
///
/// - [`register_application_blocks`] — registers each on a runtime and
///   answers its name, which is where a prepared plan's application block
///   inventory comes from (`ImpresspressBuilder::register`);
/// - [`all_block_infos`] — `.info()` over each, built with
///   [`ApplicationBlockArgs::declarative`]: config-var discovery, the
///   inspector route table, the route/auth policy table, and a seed before a
///   runtime exists.
///
/// Replaces the formerly hand-synced lists (per-block `register_static_block!`
/// linkme sites on native, the `register_all_static_blocks` wasm32 list, the
/// `all_block_infos` push list, and the explicit admin/llm/fastembed
/// registrations beside them) — audit findings #12/#13. A block is added in
/// exactly one place.
///
/// Each entry's `cfg` gates the block on its `block-*` Cargo feature; the
/// dual-target blocks compile with no `cfg` (always on).
///
/// Not application blocks, and so not here: `wafer-run/auth` (the framework
/// `AuthBlock` over `AuthServiceImpl`, via [`register_auth`]) and
/// `impresspress/transformers-embed` (over an embedding service the browser
/// injects).
macro_rules! feature_block_manifest {
    ( $( $(#[$cfg:meta])? $ctor:path ),+ $(,)? ) => {
        /// Every application block this build compiles in, constructed with
        /// `args`, by the name it registers under: the one list
        /// [`register_application_blocks`] and [`all_block_infos`] derive
        /// from. See the manifest's doc.
        pub fn application_blocks(
            args: &ApplicationBlockArgs,
        ) -> Vec<(&'static str, std::sync::Arc<dyn wafer_run::Block>)> {
            use std::sync::Arc;
            let mut blocks: Vec<(&'static str, Arc<dyn wafer_run::Block>)> = Vec::new();
            $(
                $(#[$cfg])?
                blocks.push((<$ctor>::BLOCK_NAME, Arc::new(<$ctor>::new())));
            )+
            // Its constructor takes the same `Arc<RwLock<BlockSettings>>` the
            // router reads as `Arc<dyn FeatureConfig>`, so the block toggle
            // updates the snapshot the router reads per request instead of
            // only writing the table.
            blocks.push((
                admin::ADMIN_BLOCK_ID,
                Arc::new(admin::AdminBlock::with_block_settings(args.block_settings.clone())),
            ));
            #[cfg(feature = "block-llm")]
            blocks.push((
                llm::LlmBlock::BLOCK_NAME,
                Arc::new(llm::LlmBlock::new(args.provider_admin.clone())),
            ));
            // Native only: `block-fastembed` pulls ONNX Runtime and is never
            // enabled on wasm32.
            #[cfg(feature = "block-fastembed")]
            blocks.push((
                fastembed::FastembedBlock::BLOCK_NAME,
                Arc::new(fastembed::FastembedBlock::new(args.model_cache_dir.clone())),
            ));
            blocks
        }
    };
}

/// What the application blocks whose constructors take an argument are
/// constructed with; see [`application_blocks`].
pub struct ApplicationBlockArgs {
    /// `impresspress/admin`'s live settings handle, shared with the router.
    pub block_settings: std::sync::Arc<std::sync::RwLock<crate::features::BlockSettings>>,
    /// `impresspress/llm`'s provider-management handle: the concrete
    /// `ProviderLlmService` with the `llm` feature, `NoopProviderAdmin`
    /// without it.
    #[cfg(feature = "block-llm")]
    pub provider_admin: std::sync::Arc<dyn llm::provider_admin::ProviderAdmin>,
    /// Where `impresspress/fastembed` caches its model.
    #[cfg(feature = "block-fastembed")]
    pub model_cache_dir: std::path::PathBuf,
}

impl ApplicationBlockArgs {
    /// Arguments for reading declarations only: no `info()` reads any of
    /// them (admin's handle, a provider handle that manages nothing, a model
    /// directory that is never opened), so the declarations are the ones a
    /// registered block reports.
    pub fn declarative() -> Self {
        Self {
            block_settings: std::sync::Arc::default(),
            #[cfg(feature = "block-llm")]
            provider_admin: std::sync::Arc::new(llm::provider_admin::NoopProviderAdmin),
            #[cfg(feature = "block-fastembed")]
            model_cache_dir: std::path::PathBuf::new(),
        }
    }
}

/// `BlockInfo` for every application block this build compiles in
/// ([`application_blocks`], constructed declaratively), before any runtime
/// exists.
///
/// Used by `collect_all_config_vars()` to discover declared config
/// variables, by the inspector route table and the routing/auth policy, and
/// by a test fixture standing in for a runtime's registrations. A runtime
/// reads its own blocks' declarations from its registrations
/// (`Wafer::block_infos()`), never from here.
pub fn all_block_infos() -> Vec<wafer_run::BlockInfo> {
    #[cfg(any(test, feature = "test-support"))]
    note_block_set_sweep();
    application_blocks(&ApplicationBlockArgs::declarative())
        .iter()
        .map(|(_, block)| block.info())
        .collect()
}

/// Register every application block ([`application_blocks`], constructed
/// with `args`) on the runtime, and answer their names, in registration
/// order: a prepared plan's application block inventory.
///
/// Called from `ImpresspressBuilder::register` on **both** native and wasm32.
pub fn register_application_blocks(
    wafer: &mut wafer_run::Wafer,
    args: &ApplicationBlockArgs,
) -> Result<Vec<&'static str>, wafer_run::RuntimeError> {
    let mut registered = Vec::new();
    for (name, block) in application_blocks(args) {
        wafer.register_block(name, block)?;
        registered.push(name);
    }
    Ok(registered)
}

feature_block_manifest! {
    auth_ui::AuthUiBlock,
    email::EmailBlock,
    system::SystemBlock,
    #[cfg(feature = "block-files")]
    files::FilesBlock,
    #[cfg(feature = "block-legalpages")]
    legalpages::LegalPagesBlock,
    #[cfg(feature = "block-tickets")]
    tickets::TicketsBlock,
    #[cfg(feature = "block-messages")]
    messages::MessagesBlock,
    #[cfg(feature = "block-products")]
    products::ProductsBlock,
    #[cfg(feature = "block-userportal")]
    userportal::UserPortalBlock,
    #[cfg(feature = "block-vector")]
    vector::VectorBlock,
    #[cfg(feature = "block-signal")]
    signal::SignalBlock,
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static BLOCK_SET_SWEEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has used the constructed block set. Test
/// support only.
///
/// A runtime build reads each block's declaration once, at registration
/// (wafer-run#478), and every later reader takes the registered copies. A
/// sweep is a use of the block set [`all_block_infos`] constructs —
/// constructing every feature block again — and the request-path tests
/// assert that a build adds none. Two kinds of use count:
///
/// - every call of `all_block_infos()`, directly or through any function
///   that calls it;
/// - every read of `config_vars`' declared-key memo, a process-wide set built
///   from `all_block_infos()` once. A read counts whether or not it is the
///   one that builds the memo: on a cold process (a new Worker isolate) the
///   first read *is* the sweep, so a test that warmed the memo earlier must
///   still see a request path reach it.
///
/// Per thread, so tests running in parallel do not count each other's
/// sweeps. That is also its limit: a use on another thread is not counted
/// here. The tests that rely on it build and boot on the test's own thread
/// (a `#[tokio::test]` current-thread runtime, with no block's `Init`
/// spawning onto another), so every use they cause is on the thread that
/// reads the count; a use moved behind `spawn_blocking` or onto a
/// multi-threaded runtime would escape them.
///
/// It counts the block set, not single reads: a caller that constructs one
/// block and calls its `info()` is not counted, which is what the counting
/// consumer block in `builder::prepared`'s tests is for.
#[cfg(any(test, feature = "test-support"))]
pub fn block_set_sweeps() -> usize {
    BLOCK_SET_SWEEPS.with(std::cell::Cell::get)
}

/// Record one use of the constructed block set; see [`block_set_sweeps`].
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn note_block_set_sweep() {
    BLOCK_SET_SWEEPS.with(|sweeps| sweeps.set(sweeps.get() + 1));
}

/// Bytes for a block-owned entry of the shared `/b/static/` asset manifest,
/// or `None` if no block in this build owns that key.
///
/// The counterpart to `ui::assets::shared_bytes`: `ui::assets::bytes` tries
/// the shared chrome's own files first and falls through to here, so the
/// shared asset module names no block and carries no `block-*` gate. This is
/// the one place that enumerates which blocks own manifest assets, next to
/// the manifest that already enumerates which blocks exist — a block is
/// added to the build in exactly one place, and to the asset set in exactly
/// one more.
///
/// Each block's own `assets` module holds the `include_str!` and the URL
/// accessors, so an asset's declaration and its owner are the same file.
/// The manifest itself is unconditional (`build.rs` hashes every file on
/// disk whatever the feature set), which is what keeps `/b/static/{filename}`
/// resolving identically for a Worker that streams these bytes from R2
/// without compiling any of them in.
#[cfg(feature = "embed-assets")]
pub fn static_asset_bytes(logical: &str) -> Option<&'static [u8]> {
    #[cfg(feature = "block-llm")]
    if let Some(b) = llm::assets::bytes(logical) {
        return Some(b);
    }
    #[cfg(feature = "block-files")]
    if let Some(b) = files::assets::bytes(logical) {
        return Some(b);
    }
    #[cfg(feature = "block-products")]
    if let Some(b) = products::assets::bytes(logical) {
        return Some(b);
    }
    // Bound so a build with neither block still uses the parameter.
    let _ = logical;
    None
}

/// The `(block_name, default_enabled)` pairs the boot-time enablement seed
/// writes into `impresspress__admin__block_settings`, for the blocks
/// `infos` declares — a runtime's registered declarations
/// (`Wafer::block_infos()`), consumer blocks included.
///
/// Derived, not listed: a block declares whether an operator may turn it off
/// (`can_disable`) and what it ships as (`default_enabled`) in its own
/// `info()`, and this is the only place those declarations are collected.
/// Passed to [`crate::platform_state::block_settings::load_and_seed`] by each
/// target's seeding (native's after registration, the Cloudflare deploy
/// funnel's hook, the browser's boot hook), which is what keeps the planner
/// itself free of any dependency on the block registry.
///
/// Three consequences of the `can_disable` filter, each deliberate:
///
/// - **A block that cannot be disabled gets no row.** `impresspress/system`,
///   `impresspress/email` and `impresspress/auth-ui` are always on;
///   [`crate::features::BlockSettings::is_block_enabled`] reports `true` for a
///   block with no row, so an absent row and a row at `true` are the same
///   thing to every reader. Admin renders no toggle for them either (the
///   detail fragment gates on `can_disable`), so nothing can create one.
/// - **`impresspress/admin` is excluded, and must stay excluded.** Its
///   `seed_defaults_hash` column is owned by `admin::settings::seed_defaults`,
///   which stores the shared-variable payload hash there in a different format
///   (raw hex, no `seed:` prefix). Two writers on one column with two formats
///   is an infinite re-seed loop on every cold start. Admin declaring
///   `.can_disable(true)` would reintroduce that, which is why the reason is
///   recorded here rather than in a name-matching special case.
/// - **The set follows the build.** Each entry of the block manifest is
///   `#[cfg]`-gated, so a bundle compiled without `block-tickets` registers
///   no `tickets` block and seeds no row for it; its routes 404 whether a row
///   says enabled or not.
pub fn enabled_defaults_from<B: std::borrow::Borrow<wafer_run::BlockInfo>>(
    infos: &[B],
) -> Vec<(String, bool)> {
    crate::declarations::declared(infos)
        .filter(|i| i.can_disable)
        .map(|i| (i.name.clone(), i.default_enabled))
        .collect()
}

/// The grant an embedding block (`impresspress/fastembed`,
/// `impresspress/transformers-embed`) declares so `impresspress/vector` may
/// call its `embedding.embed` and `embedding.count_tokens`. The embedding
/// handler authorizes each op as a resource in the serving block's own
/// namespace, which only that block can grant.
pub(crate) fn embedding_grant(serving_block: &str) -> wafer_run::ResourceGrant {
    wafer_run::ResourceGrant::read(
        "impresspress/vector",
        &format!("{}*", wafer_block::wrap::resource_prefix(serving_block)),
    )
    .typed(wafer_run::ResourceType::Embedding)
}

/// Every block's migrations compiled into this build, in the order a boot
/// applies them: admin first (its tables hold every block's migration state,
/// and auth's 016 writes admin's `user_roles`), then auth, then the rest by
/// name, as `Wafer::block_names()` orders them.
///
/// The browser applies all of them before it builds a runtime
/// (`impresspress_browser::boot`), so that a refusal of any block stops the
/// boot by name rather than failing that block's `Init` alone. Each entry is
/// the constant the block's own `lifecycle(Init)` applies, so the two cannot
/// disagree.
pub fn all_block_migrations() -> Vec<&'static crate::migration_helper::BlockMigrations> {
    vec![
        &admin::migrations::MIGRATIONS,
        &auth::migrations::MIGRATIONS,
        #[cfg(feature = "site-runtime")]
        &dev::migrations::MIGRATIONS,
        #[cfg(feature = "block-files")]
        &files::migrations::MIGRATIONS,
        #[cfg(feature = "block-legalpages")]
        &legalpages::migrations::MIGRATIONS,
        #[cfg(feature = "block-llm")]
        &llm::migrations::MIGRATIONS,
        #[cfg(feature = "block-messages")]
        &messages::migrations::MIGRATIONS,
        #[cfg(feature = "block-products")]
        &products::migrations::MIGRATIONS,
        #[cfg(feature = "block-signal")]
        &signal::migrations::MIGRATIONS,
        #[cfg(feature = "block-tickets")]
        &tickets::migrations::MIGRATIONS,
        #[cfg(feature = "block-userportal")]
        &userportal::migrations::MIGRATIONS,
        #[cfg(feature = "block-vector")]
        &vector::migrations::MIGRATIONS,
    ]
}

/// Register the framework `wafer-run/auth` block — wafer-core's `AuthBlock`
/// wrapping impresspress's `AuthServiceImpl`.
///
/// Cannot self-register via the feature-block manifest because the framework
/// `AuthBlock::new` takes `Arc<dyn AuthService>`. Called explicitly from
/// `ImpresspressBuilder::build` (both targets) to install both the block and the
/// service.
///
/// The service holds no context: the framework block hands it the `ctx` of
/// each message it serves, and the database calls it makes go through that.
pub fn register_auth(wafer: &mut wafer_run::Wafer) -> Result<(), wafer_run::RuntimeError> {
    let svc = std::sync::Arc::new(auth::service::AuthServiceImpl);
    wafer_core::service_blocks::auth::register_with(wafer, svc)
}

#[cfg(test)]
mod block_enabled_defaults_tests {
    use std::collections::HashMap;

    use wafer_run::BlockInfo;

    use super::*;
    use crate::features::{plan_seed_decisions, seed_hash_for, SeedOp};

    fn info(name: &str, can_disable: bool, default_enabled: bool) -> BlockInfo {
        BlockInfo::new(name, "0.0.1", "http-handler@v1", "fixture")
            .can_disable(can_disable)
            .default_enabled(default_enabled)
    }

    /// The filter keeps exactly the disableable blocks, at the value each one
    /// declares — including a `can_disable` block that ships off, and
    /// excluding a block that ships on but cannot be turned off (which is what
    /// keeps `impresspress/admin` and `impresspress/system` out of the seed).
    #[test]
    fn only_disableable_blocks_carry_a_default() {
        let infos = vec![
            info("org/on", true, true),
            info("org/off", true, false),
            info("org/always", false, true),
        ];
        assert_eq!(
            enabled_defaults_from(&infos),
            vec![("org/on".to_string(), true), ("org/off".to_string(), false)],
        );
    }

    /// End to end over the seam this feeds: a fresh table seeds one row per
    /// disableable block at its declared value, and nothing at all for the
    /// block that cannot be disabled (which then reads as enabled through the
    /// absent-row fallback).
    #[test]
    fn planner_seeds_one_row_per_disableable_block() {
        let infos = vec![
            info("org/on", true, true),
            info("org/off", true, false),
            info("org/always", false, true),
        ];
        let defaults = enabled_defaults_from(&infos);
        let decisions = plan_seed_decisions(&HashMap::new(), &defaults);

        assert_eq!(decisions.len(), 2, "{decisions:?}");
        for (name, expected) in [("org/on", true), ("org/off", false)] {
            let d = decisions
                .iter()
                .find(|d| d.block_name == name)
                .unwrap_or_else(|| panic!("{name} should be seeded: {decisions:?}"));
            assert_eq!(d.op, SeedOp::Insert);
            assert_eq!(d.enabled, expected);
            assert_eq!(d.hash, seed_hash_for(expected));
        }
        assert!(
            decisions.iter().all(|d| d.block_name != "org/always"),
            "a block that cannot be disabled must get no row: {decisions:?}",
        );
    }
}
