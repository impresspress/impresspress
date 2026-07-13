pub mod admin;
pub mod auth;
pub mod auth_ui;
pub mod crud;
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
pub mod storage;
pub mod system;
#[cfg(target_arch = "wasm32")]
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
/// dual-target blocks compile with no `cfg` (always on). `fastembed` carries
/// `feature = "block-fastembed"`, which is never enabled on wasm32 (it pulls
/// ONNX Runtime — see the `pub mod fastembed` cfg above), so the same gate
/// covers "native-only" without a redundant `not(target_arch = "wasm32")`.
///
/// Special cases stay **out** of the manifest and are registered explicitly by
/// `ImpresspressBuilder::build`, because their constructors are not zero-argument:
/// `impresspress/llm` (`Arc<dyn ProviderAdmin>`, via [`register_llm`]),
/// `wafer-run/auth` (framework `AuthBlock` wrapping `AuthServiceImpl`, via
/// [`register_auth`]), and `impresspress/transformers-embed` (wasm32-only,
/// injected `Arc<dyn EmbeddingService>`). `llm`'s `BlockInfo` is still added to
/// [`all_block_infos`] below via a `NoopProviderAdmin` handle (info is
/// declarative and never drives the provider surface).
macro_rules! feature_block_manifest {
    ( $( $(#[$cfg:meta])? $ctor:path ),+ $(,)? ) => {
        /// `BlockInfo` for every zero-arg impresspress feature block, plus the
        /// `impresspress/llm` block (constructed with a `NoopProviderAdmin`).
        ///
        /// Used by `collect_all_config_vars()` to discover declared config
        /// variables, by the inspector route table, and by the routing/auth
        /// policy, before block registration runs.
        // Each push in the body below is individually gated by an optional
        // `#[cfg]` from the macro's entry list, so the set of pushed elements
        // varies by feature flags — a `vec![..]` literal can't express
        // per-element `#[cfg]` gating, so this can't collapse to the
        // suggested rewrite.
        #[allow(clippy::vec_init_then_push)]
        pub fn all_block_infos() -> Vec<wafer_run::BlockInfo> {
            use wafer_run::Block as _;
            #[allow(unused_mut)]
            let mut infos: Vec<wafer_run::BlockInfo> = Vec::new();
            $(
                $(#[$cfg])?
                infos.push(<$ctor>::new().info());
            )+

            // `impresspress/llm` is registered separately (its ctor takes
            // `Arc<dyn ProviderAdmin>`), but its declarative `info()` belongs
            // in the discovery set. A no-op provider-admin handle suffices.
            #[cfg(feature = "block-llm")]
            infos.push(
                llm::LlmBlock::new(std::sync::Arc::new(llm::provider_admin::NoopProviderAdmin))
                    .info(),
            );

            infos
        }

        /// Register every zero-arg impresspress feature block on the runtime.
        ///
        /// Called from `ImpresspressBuilder::build` on **both** native and wasm32 —
        /// there is no longer a native (linkme) / wasm32 (manual list) split.
        /// The `impresspress/llm`, `wafer-run/auth`, and (wasm32)
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
    admin::AdminBlock,
    auth_ui::AuthUiBlock,
    email::EmailBlock,
    system::SystemBlock,
    #[cfg(feature = "block-files")]
    files::FilesBlock,
    #[cfg(feature = "block-legalpages")]
    legalpages::LegalPagesBlock,
    #[cfg(feature = "block-messages")]
    messages::MessagesBlock,
    #[cfg(feature = "block-products")]
    products::ProductsBlock,
    #[cfg(feature = "block-userportal")]
    userportal::UserPortalBlock,
    #[cfg(feature = "block-vector")]
    vector::VectorBlock,
    // Native-only: fastembed pulls ONNX Runtime; `block-fastembed` is never
    // enabled on wasm32, so this gate doubles as "not wasm32".
    #[cfg(feature = "block-fastembed")]
    fastembed::FastembedBlock,
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
// `LlmBlock` holds `Arc<dyn ProviderAdmin>`, which only requires
// `MaybeSend + MaybeSync` (real `Send + Sync` on native, a no-op marker on
// wasm32 — see wafer_block::compat), so this `Arc` doesn't promise
// cross-thread safety on wasm32; wasm32 is single-threaded.
#[cfg(feature = "block-llm")]
#[allow(clippy::arc_with_non_send_sync)]
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
// `AuthServiceImpl` holds a `BlockState` whose `dyn Context` cell only
// requires `MaybeSend + MaybeSync` (real `Send + Sync` on native, a no-op
// marker on wasm32 — see wafer_block::compat), so this `Arc` doesn't promise
// cross-thread safety on wasm32; it's a shared handle, not a thread-safety
// claim, and wasm32 is single-threaded.
#[allow(clippy::arc_with_non_send_sync)]
pub fn register_auth(wafer: &mut wafer_run::Wafer) -> Result<(), wafer_run::RuntimeError> {
    use std::sync::Arc;
    let state = auth::service::BlockState::new();
    let svc = Arc::new(auth::service::AuthServiceImpl::new(state));
    wafer_core::service_blocks::auth::register_with(wafer, svc)
}
