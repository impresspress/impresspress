//! The vendored guest module against the real `wafer_block` types.
//!
//! `src/blocks/dev/templates/wafer_guest.rs` writes three wire shapes by hand
//! — `BlockInfo`, `GuestResult` and `Result<(), WaferError>` — and reads a
//! fourth, the `__wafer_handle` call frame. Nothing in a wasm build checks
//! that they agree with the types the host parses: a field renamed upstream
//! would surface as a trap inside wasmi, or worse as a `BlockInfo` that
//! parsed with a capability silently missing.
//!
//! So the module is compiled **natively** here (its `extern` block is
//! `#[cfg(target_arch = "wasm32")]`; a shim panics for every host call) and
//! what it renders is parsed with the producer's own types. That is a
//! compile-time check of every field name and an assertion on every value —
//! no runtime, no wasm, and it runs in the ordinary `block-dev` suite.
//!
//! The end-to-end half — a real wasm build driven by wasmi against real
//! SQLite — is `wafer_guest_golden.rs`.
#![cfg(feature = "block-dev")]

/// The canonical support module, compiled for the host.
///
/// It carries its own `#![expect(dead_code)]` — a template uses only part of
/// the API — so this declaration must not add a second one.
#[path = "../src/blocks/dev/templates/wafer_guest.rs"]
mod wafer_guest;

/// The `hello` template's `src/lib.rs`.
///
/// Its own `mod wafer_guest;` is `#[cfg(target_arch = "wasm32")]`-gated and
/// its `use crate::wafer_guest::*;` is not — which resolves to the module
/// above when the file is compiled as part of this test crate, and to the
/// template's own symlinked copy when it is compiled as a block crate's root.
/// One line, two contexts, no cfg on the import.
#[path = "../src/blocks/dev/templates/hello/src/lib.rs"]
#[expect(
    dead_code,
    reason = "a template exports a whole guest ABI; this test reads only the \
              part it compares"
)]
mod hello_template;

/// The `table` template's `src/lib.rs`. See [`hello_template`].
#[path = "../src/blocks/dev/templates/table/src/lib.rs"]
#[expect(
    dead_code,
    reason = "a template exports a whole guest ABI; this test reads only the \
              part it compares"
)]
mod table_template;

use wafer_guest::json::Json;

// ---------------------------------------------------------------------------
// BlockInfo
// ---------------------------------------------------------------------------

/// What the guest renders is what the host parses, field for field — and the
/// capabilities it derives are the ones the sandbox's rules expect.
#[test]
fn rendered_block_info_parses_and_matches_the_typed_builder() {
    let rendered = wafer_guest::render_block_info(&table_template::block());
    let parsed: wafer_block::BlockInfo = serde_json::from_str(&rendered)
        .unwrap_or_else(|e| panic!("BlockInfo JSON ({e}): {rendered}"));

    assert_eq!(parsed.name, "site/newsletter");
    assert_eq!(parsed.interface, "http-handler@v1");
    parsed
        .validate("site/newsletter")
        .expect("a valid BlockInfo");

    let subscribe = parsed
        .endpoints
        .iter()
        .find(|e| e.path == "/b/newsletter/subscribe")
        .expect("the subscribe endpoint");
    assert_eq!(subscribe.method, wafer_block::HttpMethod::Post);
    assert_eq!(subscribe.auth, wafer_block::AuthLevel::Public);
    assert_eq!(
        subscribe.agent_tool.as_ref().expect("agent tool").name,
        "subscribe_newsletter"
    );
    assert_eq!(
        subscribe.input_schema.as_ref().expect("input schema")["properties"]["email"]["type"],
        "string"
    );
    assert_eq!(
        subscribe.output_schema.as_ref().expect("output schema")["properties"]["ok"]["type"],
        "boolean"
    );

    // Admin endpoints stay admin, and the `{id}` template survives the round
    // trip — the router binds the parameter off this string.
    let one = parsed
        .endpoints
        .iter()
        .find(|e| e.path == "/b/newsletter/subscribers/{id}")
        .expect("the by-id endpoint");
    assert_eq!(one.auth, wafer_block::AuthLevel::Admin);

    let caps = parsed.capabilities.as_ref().expect("capabilities");
    assert!(caps.allows_collection("site__newsletter__subscribers"));
    assert!(!caps.allows_collection("site__other__rows"));
    // Claiming a collection turns `schema` on — that is what authorizes
    // `db::ensure_table` — and never turns `ddl` or `raw_sql` on.
    assert!(caps.schema);
    assert!(!caps.ddl);
    assert!(!caps.raw_sql);
    assert!(!caps.crypto);
    assert!(!caps.network.is_enabled());
    assert!(!caps.vector_indexes.is_enabled());
    assert!(!caps.storage_folders.is_enabled());
    assert!(!caps.config.is_enabled());
    // `callable_blocks` and `requires` are rendered from one field, so the
    // `cap-requires-mismatch` rule cannot fire on a template.
    assert!(caps.callable_blocks.allows("wafer-run/database"));
    assert_eq!(parsed.requires, vec!["wafer-run/database".to_string()]);
}

/// A block that claims nothing gets a capability set that permits nothing.
#[test]
fn a_block_that_claims_nothing_is_fully_sandboxed() {
    let rendered = wafer_guest::render_block_info(&hello_template::block());
    let parsed: wafer_block::BlockInfo = serde_json::from_str(&rendered)
        .unwrap_or_else(|e| panic!("BlockInfo JSON ({e}): {rendered}"));
    assert_eq!(parsed.name, "site/hello");
    parsed.validate("site/hello").expect("a valid BlockInfo");
    assert!(parsed.requires.is_empty());

    let caps = parsed.capabilities.as_ref().expect("capabilities");
    assert!(!caps.collections.is_enabled());
    assert!(!caps.callable_blocks.is_enabled());
    // Deny-by-default is a *declared* value here, not an absence the host has
    // to interpret: `schema` is off because no collection was claimed.
    assert!(!caps.schema);
    assert!(!caps.ddl);
}

/// The two rules the sandbox's static validation applies to a scaffolded
/// block must pass on both templates as written.
#[test]
fn both_templates_pass_the_sandbox_rules_unmodified() {
    for (name, rendered) in [
        (
            "hello",
            wafer_guest::render_block_info(&hello_template::block()),
        ),
        (
            "newsletter",
            wafer_guest::render_block_info(&table_template::block()),
        ),
    ] {
        let info: wafer_block::BlockInfo = serde_json::from_str(&rendered).expect("BlockInfo JSON");
        let spec = impresspress_core::blocks::dev::validation::validate_static(
            name,
            &info,
            "sha",
            &impresspress_core::blocks::dev::validation::builtin_route_prefixes(),
            &[],
            &std::collections::BTreeSet::new(),
        );
        let spec = spec.unwrap_or_else(|found| panic!("{name} was refused: {found:?}"));
        assert_eq!(spec.name, format!("site/{name}"));
        assert_eq!(spec.routes[0].prefix, format!("/b/{name}/"));
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// The hand-written codec round-trips the shapes the wire actually carries.
#[test]
fn json_codec_round_trips_wire_shapes() {
    let text = r#"{"id":"n1","data":{"email":"a@b.c","n":3,"ok":true,"none":null,"bytes":[104,105],"nested":{"x":[1.5,"y"]}}}"#;
    let parsed = Json::parse(text).expect("parse");
    let data = parsed.get("data").expect("data");
    assert_eq!(data.get("email").and_then(Json::as_str), Some("a@b.c"));
    assert_eq!(data.get("n").and_then(Json::as_i64), Some(3));
    assert_eq!(data.get("ok").and_then(Json::as_bool), Some(true));
    assert_eq!(data.get("none"), Some(&Json::Null));
    // A fractional number is not an integer, and `as_i64` says so rather than
    // truncating it.
    let nested = data.get("nested").and_then(|n| n.get("x")).expect("nested");
    assert_eq!(nested.as_array().expect("array")[0].as_i64(), None);
    assert_eq!(nested.as_array().expect("array")[0].as_f64(), Some(1.5));

    // Rendering and re-parsing is the identity.
    let rendered = parsed.render();
    assert_eq!(Json::parse(&rendered).expect("re-parse"), parsed);
    // An integral number must not gain a `.0` tail: a wire field typed `i64`
    // refuses a float.
    assert!(rendered.contains(r#""n":3"#), "{rendered}");

    assert!(Json::parse("{bad").is_err());
    assert!(Json::parse("{}{}").is_err(), "trailing data is an error");
    assert_eq!(
        Json::parse(r#""a\"b\\c\né""#).expect("escapes").as_str(),
        Some("a\"b\\c\né")
    );
    // A 4-byte UTF-8 sequence survives the copy path verbatim. (The
    // `\uD83D\uDE80` *escape* form is pinned by the codec's own tests, in
    // `wafer_guest.rs`.)
    assert_eq!(
        Json::parse(r#""🚀""#).expect("astral character").as_str(),
        Some("🚀")
    );
    // What the guest renders is what `serde_json` reads.
    let value: serde_json::Value =
        serde_json::from_str(&Json::str("a\"b\n\u{1}é").render()).expect("serde_json");
    assert_eq!(value, serde_json::json!("a\"b\n\u{1}é"));
}

// ---------------------------------------------------------------------------
// The request frame
// ---------------------------------------------------------------------------

/// The frame the host writes decodes into the request a handler sees, and the
/// router in the guest binds the same `{id}` the template declared.
#[test]
fn request_frame_is_decoded_and_routed_with_path_params() {
    // Built with the producer's own encoder, so the bytes are the host's
    // rather than this test's idea of them.
    let mut message = wafer_block::Message::new("POST:/b/newsletter/subscribe");
    message.set_meta("http.method", "POST");
    message.set_meta("http.path", "/b/newsletter/subscribe");
    message.set_meta("http.query.src", "footer");
    message.set_meta("http.header.content-type", "application/json");
    message.set_meta("auth.user_id", "u1");
    message.set_meta("auth.user_roles", "admin,editor");
    let body = br#"{"email":"a@b.c"}"#;
    let frame = serde_json::to_vec(&wafer_block::abi::CallFrameRef(&message, body))
        .expect("encode the call frame");

    let request = wafer_guest::Request::from_frame(&frame).expect("decode the call frame");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/b/newsletter/subscribe");
    assert_eq!(request.query("src"), Some("footer"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.user_id.as_deref(), Some("u1"));
    assert_eq!(request.roles, vec!["admin", "editor"]);
    assert!(request.has_role("editor"));
    assert_eq!(
        request
            .json()
            .expect("body")
            .get("email")
            .and_then(Json::as_str),
        Some("a@b.c")
    );

    let block = table_template::block();
    let (endpoint, params) = block
        .route("GET", "/b/newsletter/subscribers/abc")
        .expect("a route with a parameter");
    assert_eq!(endpoint.path, "/b/newsletter/subscribers/{id}");
    assert_eq!(params, vec![("id".to_string(), "abc".to_string())]);

    // The listing route is a different endpoint, not the by-id one with an
    // empty capture.
    let (listing, params) = block
        .route("GET", "/b/newsletter/subscribers")
        .expect("the listing route");
    assert_eq!(listing.path, "/b/newsletter/subscribers");
    assert!(params.is_empty());

    assert!(block.route("GET", "/b/other").is_none());
    assert!(block
        .route("GET", "/b/newsletter/subscribers/abc/tags")
        .is_none());
    // The method is part of the match: `subscribe` is a POST.
    assert!(block.route("GET", "/b/newsletter/subscribe").is_none());
}

/// An empty `auth.user_id` is how the host spells "nobody is signed in".
#[test]
fn an_anonymous_request_has_no_user() {
    let mut message = wafer_block::Message::new("GET:/b/hello/");
    message.set_meta("http.method", "GET");
    message.set_meta("http.path", "/b/hello/");
    message.set_meta("auth.user_id", "");
    message.set_meta("auth.user_roles", "");
    let frame = serde_json::to_vec(&wafer_block::abi::CallFrameRef(&message, b"")).expect("encode");
    let request = wafer_guest::Request::from_frame(&frame).expect("decode");
    assert_eq!(request.user_id, None);
    assert!(request.roles.is_empty());
    assert!(request.body.is_empty());
}

// ---------------------------------------------------------------------------
// The two result frames
// ---------------------------------------------------------------------------

/// The response the guest renders is the `GuestResult` the host decodes.
#[test]
fn response_renders_the_guest_result_shape() {
    let response = wafer_guest::Response::json(201, &Json::parse(r#"{"ok":true}"#).expect("json"))
        .header("x-a", "b");
    let text = wafer_guest::render_result(&response);

    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(value["action"], "Respond");
    assert_eq!(
        value["response"]["data"],
        serde_json::json!([123, 34, 111, 107, 34, 58, 116, 114, 117, 101, 125]),
    );
    let meta = value["response"]["meta"].as_array().expect("meta");
    assert!(meta
        .iter()
        .any(|m| m["key"] == "resp.status" && m["value"] == "201"));
    assert!(meta
        .iter()
        .any(|m| m["key"] == "resp.content_type" && m["value"] == "application/json"));
    assert!(meta
        .iter()
        .any(|m| m["key"] == "resp.header.x-a" && m["value"] == "b"));

    // The host decodes it as the real type — the check the assertions above
    // cannot make, because a field the host requires and the guest omits
    // would still leave every one of them true.
    let parsed: wafer_block::abi::GuestResult = serde_json::from_str(&text).expect("GuestResult");
    assert_eq!(parsed.action, wafer_block::abi::GuestAction::Respond);
    let body = parsed.response.expect("a response arm").data;
    assert_eq!(body, br#"{"ok":true}"#);
    // The keys the response meta uses are the producer's own constants.
    assert!(meta
        .iter()
        .any(|m| m["key"] == wafer_block::meta::META_RESP_STATUS));
    assert!(meta
        .iter()
        .any(|m| m["key"] == wafer_block::meta::META_RESP_CONTENT_TYPE));
}

/// The lifecycle frame is serde's external tagging of `Result<(), WaferError>`
/// — including the `ErrorCode` spelling, which is the variant name and not a
/// snake_case one.
#[test]
fn the_lifecycle_result_shape_is_the_hosts_own_result_type() {
    let ok: Result<(), wafer_block::WaferError> =
        serde_json::from_str(r#"{"Ok":null}"#).expect("the Ok arm");
    assert!(ok.is_ok());

    // Built exactly as `__wafer_lifecycle` builds it, from an `init` that
    // returned `Err`.
    let message = "could not create site__newsletter__subscribers: \"quoted\"";
    let rendered = format!(
        r#"{{"Err":{{"code":"Internal","message":{},"meta":[]}}}}"#,
        wafer_guest::json::escape(message)
    );
    let err: Result<(), wafer_block::WaferError> =
        serde_json::from_str(&rendered).unwrap_or_else(|e| panic!("the Err arm ({e}): {rendered}"));
    let err = err.expect_err("the Err arm");
    assert_eq!(err.code, wafer_block::ErrorCode::Internal);
    assert_eq!(err.message, message);
}

// ---------------------------------------------------------------------------
// The vendored copies
// ---------------------------------------------------------------------------

/// Both templates carry the canonical module, and the module carries the
/// version the block publishes.
///
/// The templates' `src/wafer_guest.rs` are symlinks, so this is an assertion
/// about the checkout as well as about the bytes: a clone made without
/// symlink support would leave three files that could drift.
#[test]
fn templates_carry_the_canonical_module_byte_for_byte() {
    let canonical = include_str!("../src/blocks/dev/templates/wafer_guest.rs");
    assert_eq!(
        include_str!("../src/blocks/dev/templates/hello/src/wafer_guest.rs"),
        canonical
    );
    assert_eq!(
        include_str!("../src/blocks/dev/templates/table/src/wafer_guest.rs"),
        canonical
    );
    // The version the module declares and the version the block reports are
    // one number; a scaffolded block's `wafer-guest-version` check compares
    // exactly these two.
    assert!(canonical.contains(&format!(
        "pub const WAFER_GUEST_VERSION: u32 = {};",
        impresspress_core::blocks::dev::WAFER_GUEST_VERSION
    )));
    assert_eq!(
        wafer_guest::WAFER_GUEST_VERSION,
        impresspress_core::blocks::dev::WAFER_GUEST_VERSION
    );
}
