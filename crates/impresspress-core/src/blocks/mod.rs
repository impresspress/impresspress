pub mod admin;
pub mod auth;
pub mod auth_ui;
pub mod body_limit;
pub mod config;
pub mod crud;
// The browser development sandbox control plane. Deliberately absent from
// `default` features and from `feature_block_manifest!` below: its
// constructor takes a `RuntimeControl` handle, and the sandbox's security
// model turns on the block not existing outside `examples/dev-sandbox`
// rather than on an admin toggle. Registration comes from the consumer via
// `ImpresspressBuilder::extra_block` + `add_route`.
#[cfg(feature = "block-dev")]
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

/// The single `(feature-cfg, name, constructor)` manifest of impresspress feature
/// blocks whose constructors take **no arguments** (every `impresspress/*` block
/// except the three special cases below).
///
/// This macro is the one place enumerating that block set. It generates, from
/// the same entries:
///
/// - [`all_block_infos`] — `.info()` over every entry (config-var discovery,
///   inspector route granularity, the route/auth policy table);
/// - [`register_feature_blocks`] — `register_block(name, Arc::new(Ctor::new()))`
///   over every entry, called from `ImpresspressBuilder::build` on **both** native
///   and wasm32.
///
/// Replaces the three formerly hand-synced lists (per-block `register_static_block!`
/// linkme sites on native, the `register_all_static_blocks` wasm32 list, and
/// the `all_block_infos` push list) — audit findings #12/#13. A block is now
/// added in exactly one place.
///
/// Each entry's `cfg` gates the block on its `block-*` Cargo feature; the
/// dual-target blocks compile with no `cfg` (always on).
///
/// Special cases stay **out** of the manifest and are registered explicitly by
/// `ImpresspressBuilder::build`, because their constructors are not zero-argument:
/// `impresspress/llm` (`Arc<dyn ProviderAdmin>`, via [`register_llm`]),
/// `wafer-run/auth` (framework `AuthBlock` wrapping `AuthServiceImpl`, via
/// [`register_auth`]), `impresspress/transformers-embed` (injected
/// `Arc<dyn EmbeddingService>`) and `impresspress/fastembed` (the embedder's
/// model cache directory). `llm`'s `BlockInfo` is still added to
/// [`all_block_infos`] below via a `NoopProviderAdmin` handle, and
/// `fastembed`'s via an empty cache path (info is declarative and never
/// drives the provider surface or loads a model).
macro_rules! feature_block_manifest {
    ( $( $(#[$cfg:meta])? $ctor:path ),+ $(,)? ) => {
        /// `BlockInfo` for every zero-arg impresspress feature block, plus the
        /// two registered outside the manifest whose declarative `info()`
        /// still belongs in the discovery set: `impresspress/admin`
        /// (constructed unwired — see [`register_admin`]) and
        /// `impresspress/llm` (constructed with a `NoopProviderAdmin`).
        ///
        /// Used by `collect_all_config_vars()` to discover declared config
        /// variables, by the inspector route table, and by the routing/auth
        /// policy, before block registration runs.
        #[expect(
            clippy::vec_init_then_push,
            reason = "each push is individually `#[cfg]`-gated by the manifest entry \
                      it comes from, which a `vec![..]` literal cannot express"
        )]
        pub fn all_block_infos() -> Vec<wafer_run::BlockInfo> {
            use wafer_run::Block as _;
            let mut infos: Vec<wafer_run::BlockInfo> = Vec::new();
            $(
                $(#[$cfg])?
                infos.push(<$ctor>::new().info());
            )+

            // `impresspress/admin` is registered separately (its ctor takes
            // the runtime's live `BlockSettings` handle — see
            // [`register_admin`]), but its declarative `info()` belongs in the
            // discovery set exactly as it did when it sat in the manifest:
            // this set feeds `collect_all_config_vars()`, the inspector route
            // table and the routing/auth policy, none of which care about the
            // handle. The unwired `new()` is therefore the right constructor
            // here, and the wrong one for registration.
            infos.push(admin::AdminBlock::new().info());

            // `impresspress/llm` is registered separately (its ctor takes
            // `Arc<dyn ProviderAdmin>`), but its declarative `info()` belongs
            // in the discovery set. A no-op provider-admin handle suffices.
            #[cfg(feature = "block-llm")]
            infos.push(
                llm::LlmBlock::new(std::sync::Arc::new(llm::provider_admin::NoopProviderAdmin))
                    .info(),
            );

            // `impresspress/fastembed` is registered separately (its ctor
            // takes the builder's model cache directory). `info()` loads no
            // model, so the directory it is given here is never read. Native
            // only: `block-fastembed` pulls ONNX Runtime and is never enabled
            // on wasm32.
            #[cfg(feature = "block-fastembed")]
            infos.push(fastembed::FastembedBlock::new(std::path::PathBuf::new()).info());

            infos
        }

        /// Register every zero-arg impresspress feature block on the runtime.
        ///
        /// Called from `ImpresspressBuilder::build` on **both** native and wasm32 —
        /// there is no longer a native (linkme) / wasm32 (manual list) split.
        /// The `impresspress/llm`, `wafer-run/auth` and
        /// `impresspress/transformers-embed` blocks are registered explicitly by
        /// the builder afterwards (non-zero-arg constructors).
        pub fn register_feature_blocks(
            wafer: &mut wafer_run::Wafer,
        ) -> Result<(), wafer_run::RuntimeError> {
            use std::sync::Arc;
            $(
                $(#[$cfg])?
                wafer.register_block(
                    <$ctor>::BLOCK_NAME,
                    Arc::new(<$ctor>::new()),
                )?;
            )+
            Ok(())
        }
    };
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
/// writes into `impresspress__admin__block_settings`.
///
/// Derived, not listed: a block declares whether an operator may turn it off
/// (`can_disable`) and what it ships as (`default_enabled`) in its own
/// `info()`, and this is the only place those declarations are collected.
/// Passed to [`crate::platform_state::block_settings::load_and_seed`] by each
/// target's boot path (the CLI, the Cloudflare deploy-init hook, the browser
/// config loader), which is what keeps the planner itself free of any
/// dependency on the block registry.
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
///   `#[cfg]`-gated, so a bundle compiled without `block-tickets` seeds no
///   `tickets` row. That block is not registered in such a build either, so
///   its routes 404 whether the row says enabled or not.
pub fn block_enabled_defaults() -> Vec<(String, bool)> {
    enabled_defaults_from(&all_block_infos())
}

/// The rule behind [`block_enabled_defaults`], over an explicit slice so it can
/// be exercised on constructed `BlockInfo`s rather than only on whatever the
/// current feature set happens to register.
fn enabled_defaults_from(infos: &[wafer_run::BlockInfo]) -> Vec<(String, bool)> {
    infos
        .iter()
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

/// Register the admin feature block with the WAFER runtime.
///
/// `AdminBlock` is not in the feature-block manifest because its production
/// constructor takes the runtime's live enablement snapshot — the same
/// `Arc<RwLock<BlockSettings>>` the builder hands the router as its
/// `Arc<dyn FeatureConfig>`. Call this from `ImpresspressBuilder::build()` in
/// place of a manifest entry.
///
/// That handle is what makes the block toggle take effect without a restart.
/// `routing::route_to_block` reads the snapshot on every request, so the
/// toggle updates it after persisting the row; the manifest's zero-arg
/// `AdminBlock::new()` would leave the field at `Default` — a private
/// snapshot nothing reads — and the toggle would reach the table and stop
/// there. `tests/autoreg_smoke.rs` pins that admin is NOT in the manifest so
/// that cannot regress silently.
pub fn register_admin(
    w: &mut wafer_run::Wafer,
    block_settings: std::sync::Arc<std::sync::RwLock<crate::features::BlockSettings>>,
) -> Result<(), wafer_run::RuntimeError> {
    w.register_block(
        admin::ADMIN_BLOCK_ID.to_string(),
        std::sync::Arc::new(admin::AdminBlock::with_block_settings(block_settings)),
    )
}

/// Register the LLM feature block with the WAFER runtime.
///
/// LlmBlock is not in the feature-block manifest because its constructor takes
/// `Arc<dyn ProviderAdmin>`. Call this after the LLM service router is
/// registered in `ImpresspressBuilder::build()`.
///
/// `provider_admin` is the provider-management seam: the concrete
/// `ProviderLlmService` on native (`feature = "llm"`) or a `NoopProviderAdmin`
/// on wasm32 (where the browser configures providers inside its own
/// `BrowserLlmService`).
#[cfg(feature = "block-llm")]
pub fn register_llm(
    w: &mut wafer_run::Wafer,
    provider_admin: std::sync::Arc<dyn llm::provider_admin::ProviderAdmin>,
) -> Result<(), wafer_run::RuntimeError> {
    w.register_block(
        "impresspress/llm".to_string(),
        std::sync::Arc::new(llm::LlmBlock::new(provider_admin)),
    )
}

/// Register the framework `wafer-run/auth` block — wafer-core's `AuthBlock`
/// wrapping impresspress's `AuthServiceImpl`.
///
/// Cannot self-register via the feature-block manifest because the framework
/// `AuthBlock::new` takes `Arc<dyn AuthService>`. Called explicitly from
/// `ImpresspressBuilder::build` (both targets) to install both the block and the
/// service.
///
/// The `AuthServiceImpl`'s context cell starts empty here; it gets populated
/// when the runtime fires the framework AuthBlock's `lifecycle(Init)` event,
/// which calls into `AuthService::init` and stashes `ctx.clone_arc()` for
/// later `require_*` dispatches.
pub fn register_auth(wafer: &mut wafer_run::Wafer) -> Result<(), wafer_run::RuntimeError> {
    use std::sync::Arc;
    let state = auth::service::BlockState::new();
    let svc = Arc::new(auth::service::AuthServiceImpl::new(state));
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
