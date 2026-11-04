//! Registration-completeness test for the `feature_block_manifest!` set.
//!
//! S5-V replaced the three hand-synced registration lists (native linkme
//! `register_static_block!`, the wasm32 `register_all_static_blocks` list, and
//! the `all_block_infos` push list) with ONE `(feature, name, constructor)`
//! manifest in `blocks/mod.rs`. This test pins the exact block set the manifest
//! must register, on the host target, so a block silently dropped from the
//! manifest (a missing block at runtime) fails CI here.
//!
//! `register_application_blocks` is called from `ImpresspressBuilder::build()`
//! on BOTH native and wasm32; there is no longer a link-time linkme step, so a
//! bare `Wafer::new()` starts with zero `impresspress/*` blocks until the
//! builder runs it — this test drives `register_application_blocks` directly.
//! It and `all_block_infos()` both derive from `blocks::application_blocks`,
//! the one table.

use std::sync::Arc;

use impresspress_core::blocks;
use wafer_run::{StaticConfigSource, Wafer};

/// Zero-arg blocks the application table registers on every host build (none
/// of these is feature-gated off by default). `admin` / `fastembed` / `llm`
/// are in the same table but take an argument from `ApplicationBlockArgs`, so
/// they are asserted separately; framework `auth` belongs to the runtime, not
/// the table.
const MANIFEST_ZERO_ARG_BLOCKS: &[&str] = &[
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
fn register_application_blocks_installs_exactly_the_application_set() {
    let mut w = Wafer::new(Arc::new(StaticConfigSource::default()))
        .expect("Wafer::new should succeed with no lockfile present");

    // Before registration the runtime has no impresspress feature blocks (the
    // old link-time linkme registration is gone).
    for name in MANIFEST_ZERO_ARG_BLOCKS {
        assert!(
            !w.has_block(name),
            "block {name} must NOT be present before register_application_blocks runs \
             (no link-time auto-registration anymore)"
        );
    }

    let registered =
        blocks::register_application_blocks(&mut w, &blocks::ApplicationBlockArgs::declarative())
            .expect("register_application_blocks should succeed");

    for name in MANIFEST_ZERO_ARG_BLOCKS {
        assert!(
            w.has_block(name),
            "block {name} must be registered from the application table"
        );
    }
    // The blocks whose constructors take an argument are in the same table,
    // constructed with it.
    assert!(w.has_block("impresspress/admin"));
    #[cfg(feature = "block-llm")]
    assert!(w.has_block("impresspress/llm"));
    #[cfg(feature = "block-fastembed")]
    assert!(w.has_block("impresspress/fastembed"));
    // The framework auth block is the runtime's, not an application block.
    assert!(
        !w.has_block("wafer-run/auth"),
        "framework AuthBlock (Arc<dyn AuthService>) is not an application block"
    );

    // Registration and discovery read one list: the names registered are the
    // names `all_block_infos()` describes, in the same order.
    let described: Vec<String> = blocks::all_block_infos()
        .into_iter()
        .map(|info| info.name)
        .collect();
    let registered: Vec<String> = registered.into_iter().map(str::to_string).collect();
    assert_eq!(registered, described);
}

/// `all_block_infos()` must enumerate every application block — the zero-arg
/// set plus the ones constructed with an argument (`admin`, and `llm` /
/// `fastembed` when enabled) — so config-var discovery, the inspector route
/// table and the routing-auth policy see every block. It derives from the same
/// table as registration, so the two cannot drift.
#[test]
fn all_block_infos_covers_every_application_block() {
    let infos = blocks::all_block_infos();
    let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();

    for name in MANIFEST_ZERO_ARG_BLOCKS {
        assert!(
            names.contains(name),
            "all_block_infos() is missing application block {name}"
        );
    }

    // Admin's constructor takes the live `BlockSettings` handle, so it is not
    // in the zero-arg list above; its declarative `info()` must still be in
    // the discovery set: `collect_all_config_vars()`, the inspector route
    // table and the routing/auth policy all read it.
    assert!(
        names.contains(&"impresspress/admin"),
        "all_block_infos() must include impresspress/admin"
    );

    // `llm`'s constructor takes a provider-admin handle; its declarative
    // `info()` is described with a `NoopProviderAdmin`.
    #[cfg(feature = "block-llm")]
    assert!(
        names.contains(&"impresspress/llm"),
        "all_block_infos() must include impresspress/llm"
    );

    #[cfg(feature = "block-fastembed")]
    assert!(
        names.contains(&"impresspress/fastembed"),
        "all_block_infos() must include impresspress/fastembed under block-fastembed"
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
