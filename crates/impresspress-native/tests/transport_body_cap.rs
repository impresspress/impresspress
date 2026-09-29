//! The request-body cap is agreed with the transport that enforces it natively.
//!
//! `impresspress_core::streaming::MAX_REQUEST_BODY_BYTES` is what the two wasm
//! adapters enforce and what the files block clamps its per-file quota to. On
//! native, nothing in this repo enforces it: `wafer-block-http-listener` owns
//! `max_body_bytes` and refuses a larger body with its own 413. The two numbers
//! agree today, and the clamp is only honest while they do — if wafer-run
//! raised its default and this constant stayed put, impresspress would enforce
//! a per-file cap *below* what the native listener happily accepts, and the
//! number the files block advertises would be wrong in the other direction.
//!
//! wafer-run's default is a private const, so this reads the block's own
//! declared `ConfigVar` — the value an operator sees and the value Init uses
//! when no config is supplied — through the static registration the native
//! binary links anyway.

use impresspress_core::streaming::MAX_REQUEST_BODY_BYTES;

/// The listener's declared `max_body_bytes` default, read from the block
/// itself.
fn listener_declared_default() -> usize {
    // `impresspress_native` carries the `use_static_blocks!` that puts this
    // registration in the binary; naming the crate keeps it linked here too.
    let _ = impresspress_native::serve::register_http_listener;

    let registration = wafer_block::STATIC_BLOCK_REGISTRATIONS
        .iter()
        .find(|r| r.name == "wafer-run/http-listener")
        .expect("the native binary links wafer-block-http-listener's registration");

    let info = (registration.factory)().info();
    let var = info
        .flow_config
        .iter()
        .find(|v| v.key == "max_body_bytes")
        .expect("the listener declares its body cap as a ConfigVar");

    var.default
        .parse::<usize>()
        .expect("a byte count the listener would itself parse")
}

#[test]
fn the_shared_cap_matches_the_native_listeners_own_default() {
    assert_eq!(
        MAX_REQUEST_BODY_BYTES,
        listener_declared_default(),
        "impresspress_core::streaming::MAX_REQUEST_BODY_BYTES and \
         wafer-block-http-listener's `max_body_bytes` default have parted \
         company. Whichever moved, the files block's per-file quota clamp and \
         the wasm adapters' 413 now describe a different limit from the one \
         native enforces — reconcile them (and re-read the cross-repo note on \
         MAX_REQUEST_BODY_BYTES) rather than editing this assertion.",
    );
}
