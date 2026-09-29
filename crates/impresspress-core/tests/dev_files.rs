//! The dev sandbox's workspace store — `/b/dev/api/files*`.
//!
//! Gated on `block-dev` for the same reason `dev_status.rs` is: the block does
//! not exist in a default-feature build, so these tests must not compile
//! there.
#![cfg(feature = "block-dev")]

use base64ct::{Base64, Encoding};
use impresspress_core::{
    blocks::dev::{
        activation::{self, ActivationIntent},
        blobs, gc, paths,
        repo::generations::GenerationCause,
        test_support::{dev_post, dev_with_accounts, signed_in_as, FakeControl, FakeShell},
        workspace, DevBlock, DevShared,
    },
    test_support::{
        admin_msg, anon_msg, output_http_header, output_http_status, output_json, output_status,
        HeldGet, TestContext,
    },
};
use serde_json::json;
use wafer_run::{Block as _, Message};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `GET /b/dev/api/files`, optionally with a `?prefix=` filter.
///
/// The filter is set as `req.query.prefix` meta rather than appended to the
/// resource path: that is where the HTTP boundary puts a query parameter
/// (`wafer_block::http_codec` splits the URI once, into `req.resource` +
/// `req.query.*`), and it is where `Message::query` reads it back. A `?…`
/// glued onto the resource would not match any route template.
fn list_msg(prefix: Option<&str>) -> Message {
    let mut msg = admin_msg("retrieve", "/b/dev/api/files");
    if let Some(prefix) = prefix {
        msg.set_meta("req.query.prefix", prefix);
    }
    msg
}

/// Write `content` (utf8) at `path` as a new file and return its sha256.
async fn write_new(ctx: &TestContext, path: &str, content: &str) -> String {
    let out = dev_post(
        ctx,
        "/b/dev/api/files/write",
        json!({"path": path, "content": content, "expected_sha256": null}),
    )
    .await;
    let body = output_json(out).await;
    body["sha256"]
        .as_str()
        .unwrap_or_else(|| panic!("write {path} did not return a sha256: {body}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_then_list_then_read_round_trips_with_hashes() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let w = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": "site/index.html", "content": "<h1>hi</h1>", "expected_sha256": null}),
        )
        .await,
    )
    .await;
    assert_eq!(w["path"], "site/index.html");
    assert_eq!(w["size"], 11);
    // A `site/` write publishes: the response carries the generation it
    // created. What that generation does is `dev_activation.rs`'s subject;
    // here it only has to be *there*, so this file's round trip is not
    // silently exercising an unpublished write.
    assert_eq!(w["generation"]["cause"], "site_write");
    assert_eq!(w["generation"]["status"], "active");
    let sha = w["sha256"].as_str().expect("sha256").to_string();
    assert_eq!(sha.len(), 64);

    let l = output_json(ctx.dispatch_resolved(list_msg(Some("site/"))).await).await;
    assert_eq!(l["files"][0]["path"], "site/index.html");
    assert_eq!(l["files"][0]["sha256"], serde_json::json!(sha));
    assert_eq!(l["files"][0]["size"], 11);
    assert_eq!(l["files"][0]["content_type"], "text/html; charset=utf-8");

    let r = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/index.html"}),
        )
        .await,
    )
    .await;
    assert_eq!(r["encoding"], "utf8");
    assert_eq!(r["content"], "<h1>hi</h1>");
    assert_eq!(r["sha256"], serde_json::json!(sha));
    assert_eq!(r["size"], 11);
}

#[tokio::test]
async fn the_hash_a_write_returns_is_the_sha256_of_the_bytes() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let sha = write_new(&ctx, "site/index.html", "<h1>hi</h1>").await;
    assert_eq!(sha, blobs::sha256_hex(b"<h1>hi</h1>"));
}

// ---------------------------------------------------------------------------
// Optimistic concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_requires_null_expected_hash_for_a_new_file() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/a.css", "content": "a{}", "expected_sha256": "00"}),
    )
    .await;
    assert_eq!(output_status(out).await, 409);
}

#[tokio::test]
async fn write_over_an_existing_file_requires_its_hash() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_new(&ctx, "site/a.css", "a{}").await;

    // `null` now means "I believe this file does not exist" — and it does.
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/a.css", "content": "b{}", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_status(out).await, 409);
}

#[tokio::test]
async fn stale_hash_is_a_conflict_that_reports_the_current_hash() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let current = write_new(&ctx, "site/a.css", "a{}").await;

    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/a.css", "content": "b{}", "expected_sha256": "deadbeef"}),
    )
    .await;
    assert_eq!(output_status(out).await, 409);

    // Re-dispatch to read the body: draining the stream above consumed it.
    let body = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": "site/a.css", "content": "b{}", "expected_sha256": "deadbeef"}),
        )
        .await,
    )
    .await;
    assert_eq!(body["path"], "site/a.css");
    assert_eq!(body["current_sha256"], serde_json::json!(current));
    assert_eq!(body["current_size"], 3);

    // The refused write must not have landed.
    let r =
        output_json(dev_post(&ctx, "/b/dev/api/files/read", json!({"path": "site/a.css"})).await)
            .await;
    assert_eq!(r["content"], "a{}");
}

#[tokio::test]
async fn a_matching_hash_replaces_the_content() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let first = write_new(&ctx, "site/a.css", "a{}").await;

    let w = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": "site/a.css", "content": "b{}", "expected_sha256": first}),
        )
        .await,
    )
    .await;
    assert_eq!(w["sha256"], serde_json::json!(blobs::sha256_hex(b"b{}")));

    let r =
        output_json(dev_post(&ctx, "/b/dev/api/files/read", json!({"path": "site/a.css"})).await)
            .await;
    assert_eq!(r["content"], "b{}");

    // Content is never edited in place: the superseded blob is still there,
    // which is what makes a generation that references it replayable.
    assert_eq!(
        blobs::get(&ctx, &first).await.expect("first blob survives"),
        b"a{}".to_vec()
    );
}

// ---------------------------------------------------------------------------
// Encodings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn binary_files_round_trip_as_base64() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let raw = [0x89u8, b'P', b'N', b'G', 0, 1, 2];
    let png = Base64::encode_string(&raw);
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/assets/dot.png", "content": png, "encoding": "base64", "expected_sha256": null}),
    )
    .await;
    let w = output_json(out).await;
    // The recorded size and hash are of the DECODED bytes, not the envelope.
    assert_eq!(w["size"], raw.len());
    assert_eq!(w["sha256"], serde_json::json!(blobs::sha256_hex(&raw)));

    let r = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/assets/dot.png"}),
        )
        .await,
    )
    .await;
    assert_eq!(r["encoding"], "base64");
    assert_eq!(r["content"], serde_json::json!(png));
}

#[tokio::test]
async fn malformed_base64_is_rejected() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/assets/dot.png", "content": "not base64!!", "encoding": "base64", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 400);
}

#[tokio::test]
async fn block_sources_read_back_as_utf8_text() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_new(&ctx, "blocks/hello/src/lib.rs", "fn main() {}").await;
    let r = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "blocks/hello/src/lib.rs"}),
        )
        .await,
    )
    .await;
    assert_eq!(r["encoding"], "utf8");
    assert_eq!(r["content"], "fn main() {}");
}

// ---------------------------------------------------------------------------
// Path validation
// ---------------------------------------------------------------------------

/// A name is a file or a directory, never both, in the store the workspace is
/// published to. Two paths that disagree about that are refused on the way in
/// — where the message can name both of them — rather than at publish time,
/// where the backend's type mismatch would recur on every later publish and
/// wedge the sandbox for good.
#[tokio::test]
async fn a_path_that_clashes_with_an_existing_file_or_directory_is_rejected() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_new(&ctx, "site/blog/index.html", "<h1>hi</h1>").await;

    // `site/blog` is a directory, so it cannot also be a file.
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/blog", "content": "x", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 400);

    // And the reverse, on a path that is already a file.
    write_new(&ctx, "site/style.css", "a{}").await;
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/style.css/extra", "content": "x", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 400);

    // A merely textual prefix is not a clash, and must still be accepted.
    write_new(&ctx, "site/blogroll", "ok").await;

    // Nothing the refusals touched went missing.
    let l = output_json(ctx.dispatch_resolved(list_msg(Some("site/"))).await).await;
    let paths: Vec<&str> = l["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    assert_eq!(
        paths,
        vec!["site/blog/index.html", "site/blogroll", "site/style.css"]
    );
}

#[tokio::test]
async fn paths_outside_site_and_blocks_are_rejected() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    for bad in [
        "../x",
        "site/../../etc",
        "sw.js",
        "site//a",
        "blocks/Bad Name/src/lib.rs",
        "site/a\\b",
        "",
        "site",
        "blocks/hello",
        "/site/a.css",
    ] {
        let out = dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": bad, "content": "x", "expected_sha256": null}),
        )
        .await;
        assert_eq!(output_http_status(out).await, 400, "{bad}");
    }
}

#[tokio::test]
async fn a_space_inside_a_segment_is_a_legitimate_path() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let sha = write_new(&ctx, "site/my page.html", "<p>ok</p>").await;
    let l = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    assert_eq!(l["files"][0]["path"], "site/my page.html");
    assert_eq!(l["files"][0]["sha256"], serde_json::json!(sha));
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_listing_is_sorted_by_path_and_filtered_by_prefix() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_new(&ctx, "site/z.css", "z{}").await;
    write_new(&ctx, "site/a.css", "a{}").await;
    write_new(&ctx, "blocks/hello/src/lib.rs", "fn main() {}").await;

    let all = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    let paths: Vec<&str> = all["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    assert_eq!(
        paths,
        vec!["blocks/hello/src/lib.rs", "site/a.css", "site/z.css"]
    );

    let site = output_json(ctx.dispatch_resolved(list_msg(Some("site/"))).await).await;
    let paths: Vec<&str> = site["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    assert_eq!(paths, vec!["site/a.css", "site/z.css"]);
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_with_matching_hash_removes_the_entry_and_keeps_the_blob_for_history() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let sha = write_new(&ctx, "site/a.css", "a{}").await;

    let out = dev_post(
        &ctx,
        "/b/dev/api/files/delete",
        json!({"path": "site/a.css", "expected_sha256": sha}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);

    let l = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    assert!(l["files"].as_array().expect("files array").is_empty());

    // The blob outlives the manifest entry: an earlier generation still
    // names it, and Plan 4's GC — not the delete handler — reclaims it.
    assert_eq!(
        blobs::get(&ctx, &sha).await.expect("blob survives delete"),
        b"a{}".to_vec()
    );
}

#[tokio::test]
async fn delete_with_a_stale_hash_is_a_conflict() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let sha = write_new(&ctx, "site/a.css", "a{}").await;

    let out = dev_post(
        &ctx,
        "/b/dev/api/files/delete",
        json!({"path": "site/a.css", "expected_sha256": "deadbeef"}),
    )
    .await;
    assert_eq!(output_status(out).await, 409);

    let body = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/delete",
            json!({"path": "site/a.css", "expected_sha256": "deadbeef"}),
        )
        .await,
    )
    .await;
    assert_eq!(body["current_sha256"], serde_json::json!(sha));

    let l = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    assert_eq!(l["files"].as_array().expect("files array").len(), 1);
}

#[tokio::test]
async fn deleting_a_file_that_is_not_there_is_a_conflict_with_no_current_hash() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let body = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/delete",
            json!({"path": "site/a.css", "expected_sha256": "deadbeef"}),
        )
        .await,
    )
    .await;
    assert_eq!(body["current_sha256"], serde_json::Value::Null);
    assert_eq!(body["current_size"], serde_json::Value::Null);
}

#[tokio::test]
async fn reading_a_file_that_is_not_there_is_a_404() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let out = dev_post(&ctx, "/b/dev/api/files/read", json!({"path": "site/a.css"})).await;
    assert_eq!(output_http_status(out).await, 404);
}

// ---------------------------------------------------------------------------
// Content addressing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn identical_content_at_two_paths_shares_one_blob() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let a = write_new(&ctx, "site/a.html", "<p>same</p>").await;
    let b = write_new(&ctx, "site/b.html", "<p>same</p>").await;
    assert_eq!(a, b);

    // Deleting one path must not take the shared blob with it.
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/delete",
        json!({"path": "site/a.html", "expected_sha256": a}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);
    let r = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "site/b.html"}),
        )
        .await,
    )
    .await;
    assert_eq!(r["content"], "<p>same</p>");
}

// ---------------------------------------------------------------------------
// Quotas
// ---------------------------------------------------------------------------

#[tokio::test]
async fn file_size_quota_is_enforced() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let big = "x".repeat(paths::MAX_FILE_BYTES + 1);
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/big.txt", "content": big, "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 413);

    // The limit itself is not the failing case.
    let at_limit = "x".repeat(paths::MAX_FILE_BYTES);
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/big.txt", "content": at_limit, "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);
}

// ---------------------------------------------------------------------------
// Access and caching
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_files_api_is_admin_only() {
    let ctx = dev_with_accounts(FakeControl::new()).await;
    let member = signed_in_as(&ctx, "user").await;
    // No identity is told to sign in; a member is refused the admin tier.
    for (msg, status) in [
        (anon_msg("retrieve", "/b/dev/api/files"), 401),
        (member.bearer(anon_msg("retrieve", "/b/dev/api/files")), 403),
        (anon_msg("create", "/b/dev/api/files/write"), 401),
        (
            member.bearer(anon_msg("create", "/b/dev/api/files/write")),
            403,
        ),
        (anon_msg("create", "/b/dev/api/files/read"), 401),
        (anon_msg("create", "/b/dev/api/files/delete"), 401),
    ] {
        let path = msg.path().to_string();
        assert_eq!(
            output_http_status(ctx.request(msg).await).await,
            status,
            "{path}"
        );
    }
}

#[tokio::test]
async fn every_files_response_is_never_cached() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_new(&ctx, "site/a.css", "a{}").await;

    // One request per assertion: reading an `OutputStream` consumes it.
    assert_eq!(
        output_http_header(ctx.dispatch_resolved(list_msg(None)).await, "Cache-Control")
            .await
            .as_deref(),
        Some("no-store"),
    );
    assert_eq!(
        output_http_header(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                json!({"path": "site/a.css", "content": "b{}", "expected_sha256": "stale"}),
            )
            .await,
            "Cache-Control"
        )
        .await
        .as_deref(),
        Some("no-store"),
        "a 409 must be no-store too",
    );
    assert_eq!(
        output_http_header(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                json!({"path": "../nope", "content": "x", "expected_sha256": null}),
            )
            .await,
            "Cache-Control"
        )
        .await
        .as_deref(),
        Some("no-store"),
        "a 400 must be no-store too",
    );
}

// ---------------------------------------------------------------------------
// The published contract and the deserializer must agree
// ---------------------------------------------------------------------------

/// The schema a client reads and the body the handler accepts must name the
/// same required fields: a schema that is stricter turns a valid call into a
/// client-side error, and one that is laxer turns it into a 400.
#[test]
fn the_write_schema_and_the_handler_agree_on_what_is_required() {
    let info = DevBlock::with_workspace(DevShared::new(
        FakeControl::new(),
        std::sync::Arc::new(FakeShell::new()),
    ))
    .info();
    let write = info
        .endpoints
        .iter()
        .find(|e| e.path == "/b/dev/api/files/write")
        .expect("write endpoint is declared");
    let required: Vec<&str> = write.input_schema.as_ref().expect("input schema")["required"]
        .as_array()
        .expect("required list")
        .iter()
        .map(|v| v.as_str().expect("field name"))
        .collect();
    assert_eq!(required, vec!["path", "content"]);
}

/// Omitting `expected_sha256` is the same as sending `null`: "I expect no
/// file here". Over a file that exists that is a conflict, so a caller cannot
/// clobber an edit by forgetting the field.
#[tokio::test]
async fn an_omitted_expected_hash_means_the_file_should_not_exist_yet() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/a.css", "content": "a{}"}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);

    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/a.css", "content": "b{}"}),
    )
    .await;
    assert_eq!(output_status(out).await, 409);
}

// ---------------------------------------------------------------------------
// The block-count quota, end to end
// ---------------------------------------------------------------------------

/// A block quota refusal is a 409, not a 413: nothing about the request is too
/// large — the workspace's shape conflicts with a limit on how much the
/// runtime can be asked to rebuild.
#[tokio::test]
async fn the_block_count_quota_is_a_conflict_not_a_payload_error() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    for i in 0..paths::MAX_BLOCKS {
        write_new(&ctx, &format!("blocks/b{i}/src/lib.rs"), "fn main() {}").await;
    }
    // A second file in a block that already exists is not a new block.
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "blocks/b0/Cargo.toml", "content": "[package]", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);

    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "blocks/extra/src/lib.rs", "content": "fn main() {}", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 409);
    // A site write is unaffected by the block count.
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/index.html", "content": "<p>hi</p>", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);
}

// ---------------------------------------------------------------------------
// The workspace quota bounds STORED blobs, not the live manifest
// ---------------------------------------------------------------------------

/// Content is never edited in place, so every overwrite leaves the previous
/// blob behind. The quota has to see those bytes: a limit read off the live
/// manifest would report one small file here while the store held six copies.
#[tokio::test]
async fn overwriting_one_path_accumulates_stored_blob_bytes() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let mut sha = write_new(&ctx, "site/a.css", "a0{}").await;
    for i in 1..6 {
        let body = format!("a{i}{{}}");
        let out = dev_post(
            &ctx,
            "/b/dev/api/files/write",
            json!({"path": "site/a.css", "content": body, "expected_sha256": sha}),
        )
        .await;
        sha = output_json(out).await["sha256"]
            .as_str()
            .expect("sha256")
            .to_string();
    }

    let ws = workspace::load(&ctx).await.expect("load workspace");
    assert_eq!(ws.files.len(), 1, "one path is reachable");
    assert_eq!(ws.total_bytes(), 4, "and it is four bytes long");
    // Six distinct four-byte bodies were stored.
    assert_eq!(ws.blob_count, 6);
    assert_eq!(ws.blob_bytes, 24);
    // Every superseded blob is still readable — that is what makes an earlier
    // generation replayable.
    assert_eq!(
        blobs::get(&ctx, &blobs::sha256_hex(b"a0{}"))
            .await
            .expect("the first blob survives"),
        b"a0{}".to_vec()
    );
}

#[tokio::test]
async fn identical_content_is_charged_once_however_many_paths_name_it() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_new(&ctx, "site/a.html", "<p>same</p>").await;
    write_new(&ctx, "site/b.html", "<p>same</p>").await;

    let ws = workspace::load(&ctx).await.expect("load workspace");
    assert_eq!(ws.files.len(), 2);
    assert_eq!(ws.total_bytes(), 22, "two entries of eleven bytes each");
    assert_eq!(ws.blob_count, 1, "but only one blob was stored");
    assert_eq!(ws.blob_bytes, 11);
}

/// The end of the accumulation above: a workspace whose stored blobs already
/// fill the quota refuses the next write, even though nothing is reachable.
/// Staged rather than driven through 128 real overwrites — the accumulation
/// itself is covered above, and this pins the refusal without spending 64 MB.
#[tokio::test]
async fn a_workspace_full_of_unreachable_blobs_refuses_the_next_write() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let mut ws = workspace::Workspace::default();
    // 128 largest-possible files' worth of stored blobs: what 128 overwrites
    // of one page leave behind.
    for _ in 0..(paths::MAX_WORKSPACE_BYTES / paths::MAX_FILE_BYTES as u64) {
        ws.record_blob_stored(paths::MAX_FILE_BYTES as u64);
    }
    workspace::save(&ctx, &ws)
        .await
        .expect("stage a full workspace");

    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/a.css", "content": "a{}", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 413);

    // Collecting one blob's worth makes room again — the counters the
    // collector resets are the ones the quota reads.
    ws.reset_blob_totals(
        paths::MAX_WORKSPACE_BYTES - paths::MAX_FILE_BYTES as u64,
        ws.blob_count - 1,
    );
    workspace::save(&ctx, &ws).await.expect("stage room");
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/a.css", "content": "a{}", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);
}

/// Re-writing content the workspace already reaches stores nothing, so it
/// needs no headroom — an undo must not be the write that hits the wall.
#[tokio::test]
async fn a_write_of_already_stored_content_needs_no_headroom() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let sha = write_new(&ctx, "site/a.html", "<p>same</p>").await;

    // Fill the quota exactly, keeping the entry (and so its blob) reachable.
    let mut ws = workspace::load(&ctx).await.expect("load");
    ws.blob_bytes = paths::MAX_WORKSPACE_BYTES;
    workspace::save(&ctx, &ws)
        .await
        .expect("stage a full workspace");

    // Anything new is refused. Asked first: the allowed write below publishes,
    // and the collection that follows every activation resets the counters to
    // what the store really holds — a few bytes, not the staged 64 MiB.
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/c.html", "content": "<p>different</p>", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 413);

    // The same bytes at a second path: already stored, so allowed.
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/b.html", "content": "<p>same</p>", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_status(out).await, 200);
    assert_eq!(
        output_json(
            dev_post(
                &ctx,
                "/b/dev/api/files/read",
                json!({"path": "site/b.html"})
            )
            .await
        )
        .await["sha256"],
        serde_json::json!(sha)
    );
}

/// A hostile body must be refused from its encoded length, before it is
/// decoded into a second copy of itself.
#[tokio::test]
async fn an_oversized_body_is_refused_in_both_encodings() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let hostile = "x".repeat(paths::MAX_FILE_BYTES * 2);
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/big.txt", "content": hostile, "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 413);

    let encoded = Base64::encode_string(&vec![b'x'; paths::MAX_FILE_BYTES * 2]);
    let out = dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "site/big.bin", "content": encoded, "encoding": "base64", "expected_sha256": null}),
    )
    .await;
    assert_eq!(output_http_status(out).await, 413);

    // Nothing was stored on the way to either refusal.
    let ws = workspace::load(&ctx).await.expect("load workspace");
    assert_eq!(ws.blob_count, 0);
    assert_eq!(ws.blob_bytes, 0);
}

// ---------------------------------------------------------------------------
// Files with no recognized extension
// ---------------------------------------------------------------------------

/// `.gitignore`, `README` and `LICENSE` are text a user edits, and the
/// extension table cannot say so. They are stored as
/// `application/octet-stream` — that is what the site publisher serves — but
/// read back as `utf8`, because an unknown type is not a claim that the bytes
/// are binary.
#[tokio::test]
async fn a_file_with_no_known_extension_reads_back_as_text() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    for (path, body) in [
        ("blocks/hello/.gitignore", "target/\n"),
        ("blocks/hello/README", "# hello\n"),
        ("site/LICENSE", "MIT\n"),
    ] {
        write_new(&ctx, path, body).await;

        let listed = output_json(ctx.dispatch_resolved(list_msg(Some(path))).await).await;
        assert_eq!(
            listed["files"][0]["content_type"], "application/octet-stream",
            "{path} stores as octet-stream"
        );

        let r =
            output_json(dev_post(&ctx, "/b/dev/api/files/read", json!({"path": path})).await).await;
        assert_eq!(r["encoding"], "utf8", "{path}");
        assert_eq!(r["content"], body, "{path}");
    }
}

/// The other half of the same rule: a known-binary type is never offered as
/// text, and neither is an unknown type whose bytes are not valid UTF-8.
#[tokio::test]
async fn unknown_types_that_are_not_utf8_still_come_back_as_base64() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let raw = [0xffu8, 0xfe, 0x00];
    let encoded = Base64::encode_string(&raw);
    dev_post(
        &ctx,
        "/b/dev/api/files/write",
        json!({"path": "blocks/hello/blob.bin", "content": encoded, "encoding": "base64", "expected_sha256": null}),
    )
    .await;
    let r = output_json(
        dev_post(
            &ctx,
            "/b/dev/api/files/read",
            json!({"path": "blocks/hello/blob.bin"}),
        )
        .await,
    )
    .await;
    assert_eq!(r["encoding"], "base64");
    assert_eq!(r["content"], serde_json::json!(encoded));
}

// ---------------------------------------------------------------------------
// Every read of the manifest is serialized against the mutations that run
// underneath it
// ---------------------------------------------------------------------------
//
// Four tests, one per locked reader, and they are two different kinds of
// assertion because the two failure modes are not equally expressible on the
// host (`blocks::dev::files`' header names both):
//
// * Failure mode 2 — a read holding a stale manifest whose blob the collector
//   then reclaims — is a REAL outcome a fixture can produce, and
//   `a_read_holds_off_the_write_and_the_collection_that_would_pull_its_blob`
//   produces it: without the lock the read answers a sanitized 500.
// * Failure mode 1 — a save invalidating `workspace::load`'s snapshot between
//   its two storage steps — is not reproducible here at all: a `get` on
//   `InMemoryStorageService` is atomic, so no interleaving tears it. What the
//   other three tests pin instead is LOCK PRESENCE: park the manifest read,
//   run a write against it, and assert the reader answered from the manifest
//   that was current when it started. Under the fix the write cannot land
//   first; with the lock deleted it does, and each of the three fails. That is
//   the regression guard the locks would otherwise not have.
//
// All four share one shape. `hold_next_storage_get` parks one named `get`, the
// other half of a `tokio::join!` drives a mutation, and the park's release sits
// at the end of that half — unreachable while the mutation is blocked on the
// lock the parked read is holding. So the correct outcome is the one where the
// budget expires, and every test asserts `budget_expired()` for it, alongside
// `was_reached()` for the seam having fired at all.
//
// Two traps found while writing these, both of which produced a test that
// passed against unfixed code, and both of which the assertions above now
// catch rather than hide:
//
// 1. The mutating half must not start until the read half has parked
//    (`once_parked`). The reader reaches its `get` only after some database
//    reads, so whichever half suspends first is not a property of the code
//    under test — and a mutation that ran first took the hold with its own
//    manifest load, leaving both `was_reached` and `budget_expired` true for
//    an interleaving that never happened.
// 2. `HeldGet::POLL_BUDGET` must be far larger than the mutation, not merely
//    "big enough". The parked read is re-polled on every suspension of the
//    other half, so a budget in the hundreds RACES the mutation and expires
//    for reasons unrelated to the lock. See the constant's own comment.

/// Suspend the mutating half of a join until the read half has actually parked.
///
/// Without this the test depends on which future the join polls into a
/// suspension first, and that is not a property of the code under test: the
/// reader reaches its parked `get` only after some database reads, and if the
/// mutation runs first it takes the hold with its OWN manifest load. Both
/// halves then look right — the seam fired, the budget expired — while the
/// interleaving under test never happened. Which is the same class of vacuous
/// pass `was_reached` exists to prevent, one level up.
///
/// Bounded, and the bound panics rather than returning: a mutation that
/// silently gave up waiting would run unraced and the test would report on
/// nothing.
///
/// The bound is sized against `HeldGet::POLL_BUDGET`, for the same reason that
/// constant is sized the way it is. This is the FIRST assertion starvation
/// trips: the read half reaches its `get` only after some database reads, and
/// every yield here is a yield the executor may spend elsewhere, so a bound
/// two orders of magnitude under the park's own budget turns a slow machine
/// into a failure that says "never parked" about a read that parks fine. The
/// yields are self-waking and do no work, so a budget of this size costs
/// milliseconds in the failing case and nothing at all in the passing one —
/// the helper returns on the first poll after the park.
///
/// The panic reports the count it actually burned, so the next failure says
/// whether the bound was reached or something else went wrong.
async fn once_parked(hold: &HeldGet) {
    const YIELD_BUDGET: u32 = 100_000;
    let mut yields = 0u32;
    while !hold.was_reached() {
        assert!(
            yields < YIELD_BUDGET,
            "the read half never parked its `get` after {yields} yields, so nothing could be \
             interleaved with it"
        );
        yields += 1;
        tokio::task::yield_now().await;
    }
}

/// A read holds the manifest it loaded until it has the blob that manifest
/// names — so a write and a collection that land in between cannot turn it
/// into a `500`.
///
/// The defect this pins is real and user-facing, not a fixture artifact.
/// `DevShared::workspace` orders the manifest's writers against each other and
/// orders nothing against a read, so before the fix `handle_read` loaded the
/// manifest unlocked and fetched the blob afterwards. Every storage call is a
/// suspension point and the sandbox's fetch events dispatch concurrently, so
/// the two steps really do straddle a mutation: the entry is repointed, the
/// blob the *old* entry named becomes unreachable, the collector reclaims it,
/// and the fetch comes back `NotFound` — which the handler can only report as
/// corruption, i.e. a sanitized internal error on a read that asked for
/// nothing unusual.
///
/// The path is a `blocks/` one on purpose. A `site/` write publishes a
/// generation, and a retained generation's site manifest is a collector root
/// that would keep the superseded blob alive — the race would still be there
/// and nothing would observe it. Block sources live in the workspace and in no
/// generation at all (`blocks::dev::gc`'s header), so overwriting one makes
/// the previous blob collectable immediately.
#[tokio::test]
async fn a_read_holds_off_the_write_and_the_collection_that_would_pull_its_blob() {
    const PATH: &str = "blocks/demo/src/lib.rs";

    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();
    let v1 = write_new(&ctx, PATH, "// v1").await;

    // Park the read between its manifest load and its blob fetch.
    let hold = ctx.hold_next_storage_get("impresspress/dev", blobs::FOLDER, &v1);

    let read = dev_post(&ctx, "/b/dev/api/files/read", json!({"path": PATH}));
    let mutate = async {
        once_parked(&hold).await;
        // Checked, not discarded. A write that started REFUSING would leave
        // the old blob referenced, the collector would reclaim nothing, the
        // read would succeed and every assertion below would pass — against
        // unfixed code, for good.
        let write = output_json(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                json!({"path": PATH, "content": "// v2", "expected_sha256": v1}),
            )
            .await,
        )
        .await;
        assert_eq!(
            write["path"], PATH,
            "the racing write must SUCCEED, or it repoints nothing and there is \
             no stale entry for the read to be holding: {write}",
        );
        assert_ne!(
            write["sha256"], v1,
            "the racing write must land new content, or the old blob stays \
             reachable and the collector has nothing to take",
        );
        // Likewise checked for what it DID, not only that it did not error: a
        // collection that reclaimed nothing leaves the read's blob in place
        // and the test proves nothing about holding a stale manifest.
        let report = gc::collect(&ctx, &shared)
            .await
            .expect("the collector must run");
        assert_eq!(
            report.blobs_deleted, 1,
            "the collector must reclaim exactly the blob the read is holding a \
             reference to: {report:?}",
        );
        // Only reachable when the read did NOT hold the lock across its two
        // steps; under the fix the write above is still waiting for it.
        hold.release();
    };
    let (read_out, ()) = tokio::join!(read, mutate);

    assert!(
        hold.was_reached(),
        "the read never fetched a blob, so nothing was interleaved and this \
         test proved nothing",
    );
    assert!(
        hold.budget_expired(),
        "the park must end on its budget, not on the release: the release is \
         the last line of the mutating half, and reaching it means the write \
         was NOT blocked on the lock the read holds. A park that was released \
         proves nothing about serialization, and every assertion below would \
         still pass",
    );
    let body = match read_out.collect_buffered().await {
        Ok(buf) => buf.body,
        Err(terminal) => panic!(
            "the read must survive a write and a collection landing underneath \
             it, but it terminated {terminal:?}",
        ),
    };
    let read: serde_json::Value = serde_json::from_slice(&body).expect("a JSON body");
    assert_eq!(
        read["sha256"], v1,
        "the read answered from the manifest it loaded, so it must return that \
         manifest's content",
    );
    assert_eq!(read["content"], "// v1");
}

/// **The activation path.** A publish composes its site manifest from the
/// workspace as it stood when the publish started, not from a manifest a
/// concurrent write replaced underneath it.
///
/// This is the reader that backs every ordinary site write and delete, and it
/// was the last one to take the lock. `files::handle_write` and
/// `handle_delete` release `DevShared::workspace` *before* they publish —
/// deliberately, so editing never waits behind a runtime rebuild — and
/// `activation::workspace_site` then reads the manifest back inside the
/// activation that publish asked for. That released gap is where a second
/// concurrent write takes the lock and saves, and an unlocked read straddling
/// that save is failure mode 1: in the browser `workspace::load`'s snapshot is
/// invalidated, the storage layer reports an internal error, and the FIRST
/// write answers a sanitized `500` for content it has already durably saved.
///
/// Mode 1 itself is not reproducible on the host (`InMemoryStorageService`'s
/// `get` is atomic), so what is asserted is that the read was serialized:
/// under the lock the composed generation carries the one file the workspace
/// held when `workspace_site` started, and the racing write's file arrives in
/// a generation of its own. With the lock removed the parked read resumes on
/// the post-write manifest and composes two.
///
/// The activation is requested directly rather than through a file write so
/// that `workspace_site`'s load is the FIRST `workspace.json` read in the
/// parked future — a write's own read-modify-write would take the hold, and
/// blocking a writer on a lock it already holds proves nothing.
#[tokio::test]
async fn an_activation_composes_its_site_from_the_manifest_a_racing_write_has_not_replaced() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();
    write_new(&ctx, "site/index.html", "<h1>one</h1>").await;

    // Park `workspace_site`'s manifest load, inside the activation queue.
    let hold = ctx.hold_next_storage_get("impresspress/dev", workspace::FOLDER, workspace::KEY);

    let publish = activation::request(
        &ctx,
        &shared,
        GenerationCause::SiteWrite,
        ActivationIntent::SiteOnly,
    );
    let racer = async {
        once_parked(&hold).await;
        let write = output_json(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                json!({"path": "site/late.html", "content": "<p>late</p>",
                       "expected_sha256": null}),
            )
            .await,
        )
        .await;
        assert_eq!(
            write["path"], "site/late.html",
            "the racing write must SUCCEED — a refusal would save no manifest \
             and there would be nothing for the parked read to straddle: {write}",
        );
        // Unreachable while the write is blocked on the lock the parked read
        // holds.
        hold.release();
    };
    let (composed, ()) = tokio::join!(publish, racer);

    assert!(
        hold.was_reached(),
        "no `workspace.json` read was parked, so nothing was interleaved and \
         this test proved nothing",
    );
    assert!(
        hold.budget_expired(),
        "the park must end on its budget: reaching the release means the \
         racing write was NOT held off, which is the unfixed behaviour",
    );
    let outcome = composed.expect("the activation must succeed");
    assert_eq!(
        outcome.generation.site_files, 1,
        "the generation must project the manifest that was current when the \
         activation read it — one file. Two means the read resumed on a \
         manifest the racing write had saved underneath it, which is the \
         unlocked read that answers a 500 in the browser",
    );
}

/// **The listing.** `GET /b/dev/api/files` answers from the manifest that was
/// current when it started, not from one a concurrent write replaced under it.
///
/// A lock-presence guard, and the reason one is needed: failure mode 1 is not
/// reproducible in a fixture, so without this the `handle_list` lock could be
/// deleted and the suite would stay green. "Not expressible" is true of the
/// torn read itself and false of the ordering the lock imposes, which is what
/// this pins.
#[tokio::test]
async fn a_listing_answers_from_the_manifest_a_racing_write_has_not_replaced() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    write_new(&ctx, "blocks/demo/src/lib.rs", "// one").await;

    let hold = ctx.hold_next_storage_get("impresspress/dev", workspace::FOLDER, workspace::KEY);

    let list = ctx.dispatch_resolved(list_msg(None));
    let racer = async {
        once_parked(&hold).await;
        let write = output_json(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                json!({"path": "blocks/demo/src/two.rs", "content": "// two",
                       "expected_sha256": null}),
            )
            .await,
        )
        .await;
        assert_eq!(
            write["path"], "blocks/demo/src/two.rs",
            "the racing write must SUCCEED, or it saves no manifest: {write}",
        );
        hold.release();
    };
    let (listed, ()) = tokio::join!(list, racer);

    assert!(hold.was_reached(), "no `workspace.json` read was parked");
    assert!(
        hold.budget_expired(),
        "the park must end on its budget: reaching the release means the \
         racing write was NOT held off, which is the unfixed behaviour",
    );
    let body = output_json(listed).await;
    let paths: Vec<&str> = body["files"]
        .as_array()
        .expect("a files array")
        .iter()
        .map(|file| file["path"].as_str().expect("a path"))
        .collect();
    assert_eq!(
        paths,
        vec!["blocks/demo/src/lib.rs"],
        "the listing must answer the manifest it started from; seeing the \
         racing write's file means the load resumed on a manifest saved \
         underneath it",
    );
}

/// **The status poll.** `gc::storage_usage` — which backs
/// `GET /b/dev/api/status`, polled about three times a second while a tool
/// call is outstanding — reads the manifest under the same lock.
///
/// The same lock-presence guard as the listing, on the read most likely to
/// land inside a mutation: it is the only one that fires on a timer.
#[tokio::test]
async fn the_status_poll_reads_the_manifest_a_racing_write_has_not_replaced() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let shared = ctx.dev_shared();
    write_new(&ctx, "blocks/demo/src/lib.rs", "// one").await;

    let hold = ctx.hold_next_storage_get("impresspress/dev", workspace::FOLDER, workspace::KEY);

    let poll = gc::storage_usage(&ctx, &shared);
    let racer = async {
        once_parked(&hold).await;
        let write = output_json(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                json!({"path": "blocks/demo/src/two.rs", "content": "// two",
                       "expected_sha256": null}),
            )
            .await,
        )
        .await;
        assert_eq!(
            write["path"], "blocks/demo/src/two.rs",
            "the racing write must SUCCEED, or it saves no manifest: {write}",
        );
        hold.release();
    };
    let (usage, ()) = tokio::join!(poll, racer);

    assert!(hold.was_reached(), "no `workspace.json` read was parked");
    assert!(
        hold.budget_expired(),
        "the park must end on its budget: reaching the release means the \
         racing write was NOT held off, which is the unfixed behaviour",
    );
    let usage = usage.expect("the status poll must succeed");
    assert_eq!(
        usage.workspace_files, 1,
        "the poll must report the manifest it started from; seeing the racing \
         write's file means the load resumed on a manifest saved underneath it",
    );
}
