//! Registration-completeness test for the `feature_block_manifest!` set.
//!
//! S5-V replaced the three hand-synced registration lists (native linkme
//! `register_static_block!`, the wasm32 `register_all_static_blocks` list, and
//! the `all_block_infos` push list) with ONE `(feature, name, constructor)`
//! manifest in `blocks/mod.rs`. This test pins the exact block set the manifest
//! must register, on the host target, so a block silently dropped from the
//! manifest (a missing block at runtime) fails CI here.
//!
//! `register_feature_blocks` is called from `ImpresspressBuilder::build()` on BOTH
//! native and wasm32; there is no longer a link-time linkme step, so a bare
//! `Wafer::new()` starts with zero `impresspress/*` blocks until the builder runs
//! it — this test drives `register_feature_blocks` directly.

use std::sync::Arc;

use impresspress_core::blocks;
use wafer_run::{StaticConfigSource, Wafer};

/// Zero-arg blocks the manifest registers on every host build (none of these
/// is feature-gated off by default). `fastembed` is feature-gated under
/// `native-embedding` and checked separately; `llm` / framework `auth` /
/// `transformers-embed` take non-zero-arg constructors and are NOT in the
/// manifest (the builder installs them explicitly).
const MANIFEST_ZERO_ARG_BLOCKS: &[&str] = &[
    "impresspress/admin",
    "impresspress/auth-ui",
    "impresspress/email",
    "impresspress/files",
    "impresspress/legalpages",
    "impresspress/messages",
    "impresspress/products",
    "impresspress/system",
    "impresspress/userportal",
    "impresspress/vector",
];

#[test]
fn register_feature_blocks_installs_exactly_the_manifest_set() {
    let mut w = Wafer::new(Arc::new(StaticConfigSource::default()))
        .expect("Wafer::new should succeed with no lockfile present");

    // Before registration the runtime has no impresspress feature blocks (the
    // old link-time linkme registration is gone).
    for name in MANIFEST_ZERO_ARG_BLOCKS {
        assert!(
            !w.has_block(name),
            "block {name} must NOT be present before register_feature_blocks runs \
             (no link-time auto-registration anymore)"
        );
    }

    blocks::register_feature_blocks(&mut w).expect("register_feature_blocks should succeed");

    for name in MANIFEST_ZERO_ARG_BLOCKS {
        assert!(
            w.has_block(name),
            "block {name} must be registered by the feature-block manifest"
        );
    }

    // `fastembed` (native-only) is in the manifest only under its feature.
    #[cfg(feature = "native-embedding")]
    assert!(
        w.has_block("impresspress/fastembed"),
        "fastembed must register from the manifest when native-embedding is on"
    );

    // Special cases are NOT in the manifest — their constructors are not
    // zero-argument, so the builder installs them explicitly afterwards.
    assert!(
        !w.has_block("impresspress/llm"),
        "LlmBlock (Arc<dyn ProviderAdmin>) must not be in the feature-block manifest"
    );
    assert!(
        !w.has_block("wafer-run/auth"),
        "framework AuthBlock (Arc<dyn AuthService>) must not be in the feature-block manifest"
    );
}

/// `all_block_infos()` must enumerate the SAME zero-arg manifest set (so
/// config-var discovery / inspector route granularity / the routing-auth
/// policy see every block), PLUS `impresspress/llm` (its declarative `info()`
/// belongs in the discovery set even though its constructor is special).
///
/// This is the before/after block-set proof: the manifest, the registration
/// fn, and the info discovery set can no longer drift.
#[test]
fn all_block_infos_covers_the_manifest_set_plus_llm() {
    let infos = blocks::all_block_infos();
    let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();

    for name in MANIFEST_ZERO_ARG_BLOCKS {
        assert!(
            names.contains(name),
            "all_block_infos() is missing manifest block {name}"
        );
    }

    // `llm` is the one block registered outside the manifest whose info() is
    // still discovered (via a NoopProviderAdmin handle).
    #[cfg(feature = "block-llm")]
    assert!(
        names.contains(&"impresspress/llm"),
        "all_block_infos() must include impresspress/llm"
    );

    #[cfg(feature = "native-embedding")]
    assert!(
        names.contains(&"impresspress/fastembed"),
        "all_block_infos() must include impresspress/fastembed under native-embedding"
    );

    // No duplicates — a block listed twice would double-register at boot.
    let mut sorted = names.clone();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(
        before,
        sorted.len(),
        "all_block_infos() has duplicate block names"
    );

    // The framework auth block declares its config vars via
    // `shared_config_vars()` (not `BlockInfo::config_keys`), so it is
    // deliberately absent from this discovery set.
    assert!(
        !names.contains(&"wafer-run/auth"),
        "framework auth block must not be in all_block_infos() (its vars come from shared_config_vars)"
    );
}
