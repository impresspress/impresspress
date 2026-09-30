# Build Sandboxes D — Batch Writes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `dev_write_files` — write up to 64 files as one change: one generation for a `site/` batch, staging only for a `blocks/<name>/` batch, all-or-nothing on conflicts.

**Architecture:** `POST /b/dev/api/files/write-batch` beside the single write, in `files.rs`. It reuses the single write's helpers (`read_body`, `min_decoded_len`, `decode_content`, `hash_matches`, `check_quotas`, `publish_if_site`) and the multi-file discipline `scaffold::handle_create` already documents: every hash, collision and quota is checked against a projection before any blob is stored; blobs are stored, entries inserted, the workspace saved once, then one `publish_if_site`.

**Tech Stack:** Rust (`impresspress-core`, feature `block-dev`), serde/schemars, Playwright.

**Spec:** `docs/superpowers/specs/2026-09-30-build-sandboxes-design.md` §6.5, §12 (PR D). Independent of Plans B and C.

## Global Constraints

- Branch from `main` after Plan A has merged (it only needs the e2e paths of A).
- 1 to 64 entries (`paths::MAX_BATCH_FILES = 64`); no duplicate paths; every entry in one area — all under `site/`, or all under the same `blocks/<name>/`. Mixed areas, duplicates, an empty or over-long batch are `400`.
- Every `expected_sha256` is checked before any byte is stored; any mismatch is a `409` `{ conflicts: [FileConflict…] }` listing every conflicting entry, and nothing is written.
- Per-file limit (512 KiB) and workspace quotas apply exactly as for a single write, evaluated over the batch in order.
- The response is `{ files: [FileEntry…] (path order), generation: GenerationSummary | null }`.
- `dev_write_file` is unchanged.
- Test commands: `cargo test --locked -p impresspress-core --features block-dev,wasm`; clippy `cargo clippy --locked -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings`; `cargo +nightly fmt`.
- Snapshots regenerated deliberately: `UPDATE_DEV_TOOLS_SNAPSHOT=1 … --test dev_tools_manifest`, `UPDATE_OPENAPI_SNAPSHOTS=1 … --test openapi_snapshot` and `--test endpoint_surface`.
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; PR, never direct to `main`.

## Review Focus

1. Two entries in one batch that collide as file-and-directory (`site/a` and `site/a/b.css`) — refused before anything is stored, naming both. (Task 1, test `a_batch_that_collides_with_itself_is_refused`.)
2. A batch that overwrites a file and, later in the same batch, writes identical content at another path — charged once (the projection sees the first entry's blob). (Task 1, test `identical_content_in_one_batch_is_charged_once`.)
3. A blob write that fails on the third of five files — the first two blobs are charged and the workspace saved so `blob_bytes` stays honest, no entry names any of them, the response is a 500. (Task 1, mirrors scaffold; asserted by reading the code path, not a test — `FakeControl` has no failing blob store.)
4. A `blocks/` batch for a block name that would be the 17th — `409` from `check_quotas` (`TooManyBlocks`) before anything is stored. (Task 1, test `a_block_batch_stages_without_publishing` covers the happy path; the quota path is `check_quotas`'s own test.)
5. A batch of 64 files each just under 512 KiB — 32 MiB decoded in memory at once. Accepted by design (the quota is 64 MiB); noted in the handler comment. (Task 1, step 4.)

---

### Task 1: The endpoint

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/paths.rs` (`MAX_BATCH_FILES`)
- Modify: `crates/impresspress-core/src/blocks/dev/contracts.rs` (three types after `FileWriteResponse`)
- Modify: `crates/impresspress-core/src/blocks/dev/mod.rs` (`Route::ApiFilesWriteBatch`, its `ROUTES` entry, its dispatch arm)
- Modify: `crates/impresspress-core/src/blocks/dev/files.rs` (`handle_write_batch`)
- Test: `crates/impresspress-core/tests/dev_files.rs`

**Interfaces:**
- Produces: `contracts::{FileWriteBatchRequest { files: Vec<FileWriteRequest> }, FileWriteBatchResponse { files: Vec<FileEntry>, generation: Option<GenerationSummary> }, FileWriteBatchConflict { conflicts: Vec<FileConflict> }}`, `files::handle_write_batch(ctx, shared, input)`, `paths::MAX_BATCH_FILES`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/dev_files.rs` (its imports already cover `dev_post`, `dev_status`, `output_json`, `output_status`, `blobs`, `paths`, `json`, `list_msg`, `write_new`; add `dev_get` to the `test_support::{…}` list):

```rust
// ---------------------------------------------------------------------------
// POST /b/dev/api/files/write-batch
// ---------------------------------------------------------------------------

const BATCH: &str = "/b/dev/api/files/write-batch";

#[tokio::test]
async fn a_batch_of_site_files_publishes_one_generation() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let body = output_json(
        dev_post(
            &ctx,
            BATCH,
            json!({"files": [
                {"path": "site/index.html", "content": "<h1>hi</h1>", "expected_sha256": null},
                {"path": "site/styles.css", "content": "h1{}", "expected_sha256": null},
                {"path": "site/about/index.html", "content": "<h1>about</h1>", "expected_sha256": null},
            ]}),
        )
        .await,
    )
    .await;
    let paths: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        vec!["site/about/index.html", "site/index.html", "site/styles.css"],
        "path order: {body}"
    );
    assert_eq!(body["files"][1]["sha256"], json!(blobs::sha256_hex(b"<h1>hi</h1>")));
    assert_eq!(body["files"][1]["content_type"], "text/html; charset=utf-8");
    assert_eq!(body["generation"]["site_files"], 3, "{body}");
    // The one generation is live, and it is the first — no other was minted.
    let status = dev_status(&ctx).await;
    assert_eq!(status["active_generation"]["id"], body["generation"]["id"]);
    assert_eq!(status["active_generation"]["parent_id"], serde_json::Value::Null);
    let listing = output_json(dev_get(&ctx, "/b/dev/api/generations").await).await;
    assert_eq!(listing["generations"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_batch_with_a_stale_hash_writes_nothing_and_names_every_conflict() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let current = write_new(&ctx, "site/a.css", "a{}").await;
    let batch = json!({"files": [
        {"path": "site/a.css", "content": "b{}", "expected_sha256": "deadbeef"},
        {"path": "site/new.css", "content": "n{}", "expected_sha256": null},
        {"path": "site/ghost.css", "content": "g{}", "expected_sha256": "cafebabe"},
    ]});
    assert_eq!(output_status(dev_post(&ctx, BATCH, batch.clone()).await).await, 409);
    let body = output_json(dev_post(&ctx, BATCH, batch).await).await;
    let conflicts = body["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 2, "{body}");
    assert_eq!(conflicts[0]["path"], "site/a.css");
    assert_eq!(conflicts[0]["current_sha256"], json!(current));
    assert_eq!(conflicts[0]["current_size"], 3);
    assert_eq!(conflicts[1]["path"], "site/ghost.css");
    assert_eq!(conflicts[1]["current_sha256"], serde_json::Value::Null);
    // Nothing was written — not even the entry whose hash was right.
    let listing = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    let paths: Vec<&str> = listing["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["site/a.css"]);
    assert_eq!(listing["files"][0]["sha256"], json!(current));
}

#[tokio::test]
async fn a_block_batch_stages_without_publishing() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let body = output_json(
        dev_post(
            &ctx,
            BATCH,
            json!({"files": [
                {"path": "blocks/hello/Cargo.toml", "content": "[package]\nname = \"hello\"\n"},
                {"path": "blocks/hello/src/lib.rs", "content": "// hi\n"},
            ]}),
        )
        .await,
    )
    .await;
    assert_eq!(body["files"].as_array().unwrap().len(), 2, "{body}");
    assert_eq!(body["generation"], serde_json::Value::Null);
    assert_eq!(dev_status(&ctx).await["active_generation"], serde_json::Value::Null);
}

#[tokio::test]
async fn a_batch_that_mixes_areas_is_refused() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    for files in [
        json!([
            {"path": "site/index.html", "content": "x"},
            {"path": "blocks/hello/src/lib.rs", "content": "y"},
        ]),
        json!([
            {"path": "blocks/hello/src/lib.rs", "content": "x"},
            {"path": "blocks/other/src/lib.rs", "content": "y"},
        ]),
    ] {
        let out = dev_post(&ctx, BATCH, json!({"files": files})).await;
        assert_eq!(output_status(out).await, 400);
    }
    let listing = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    assert_eq!(listing["files"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn duplicate_paths_and_bad_sizes_of_batch_are_refused() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let dup = json!({"files": [
        {"path": "site/a.css", "content": "a{}"},
        {"path": "site/a.css", "content": "b{}"},
    ]});
    assert_eq!(output_status(dev_post(&ctx, BATCH, dup).await).await, 400);
    assert_eq!(
        output_status(dev_post(&ctx, BATCH, json!({"files": []})).await).await,
        400
    );
    let too_many: Vec<serde_json::Value> = (0..=paths::MAX_BATCH_FILES)
        .map(|i| json!({"path": format!("site/f{i}.css"), "content": "x{}"}))
        .collect();
    assert_eq!(
        output_status(dev_post(&ctx, BATCH, json!({"files": too_many})).await).await,
        400
    );
}

#[tokio::test]
async fn a_batch_that_collides_with_itself_is_refused() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let out = dev_post(
        &ctx,
        BATCH,
        json!({"files": [
            {"path": "site/a", "content": "file"},
            {"path": "site/a/b.css", "content": "b{}"},
        ]}),
    )
    .await;
    assert_eq!(output_status(out).await, 400);
    let listing = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    assert_eq!(listing["files"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn an_oversized_file_in_a_batch_is_refused_before_anything_is_stored() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let big = "x".repeat(paths::MAX_FILE_BYTES + 1);
    let out = dev_post(
        &ctx,
        BATCH,
        json!({"files": [
            {"path": "site/ok.css", "content": "a{}"},
            {"path": "site/big.css", "content": big},
        ]}),
    )
    .await;
    assert_eq!(output_status(out).await, 413);
    let listing = output_json(ctx.dispatch_resolved(list_msg(None)).await).await;
    assert_eq!(listing["files"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn identical_content_in_one_batch_is_charged_once() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    output_json(
        dev_post(
            &ctx,
            BATCH,
            json!({"files": [
                {"path": "site/a.css", "content": "same{}"},
                {"path": "site/b.css", "content": "same{}"},
            ]}),
        )
        .await,
    )
    .await;
    let ws = workspace::load(&ctx).await.expect("workspace");
    assert_eq!(ws.files.len(), 2);
    assert_eq!(ws.blob_count, 1);
    assert_eq!(ws.blob_bytes, "same{}".len() as u64);
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_files batch`
Expected: compile error on `paths::MAX_BATCH_FILES`; after adding only that constant, every batch test fails with a 404 body (no route).

- [ ] **Step 3: Contracts, constant and route**

`paths.rs`, beside `MAX_FILES`:
```rust
/// Most files one `POST /b/dev/api/files/write-batch` may carry. A scaffold
/// is a handful; 64 is room for a whole small site in one generation while
/// keeping the decoded batch (64 × [`MAX_FILE_BYTES`] at worst) well inside
/// what the workspace quota already permits.
pub const MAX_BATCH_FILES: usize = 64;
```
`contracts.rs`, after `FileWriteResponse`:
```rust
/// Request of `POST /b/dev/api/files/write-batch`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileWriteBatchRequest {
    /// The files to write, 1 to 64, all under `site/` or all under one
    /// `blocks/<name>/`. Each entry is exactly a `dev_write_file` request.
    pub files: Vec<FileWriteRequest>,
}

/// Response of `POST /b/dev/api/files/write-batch`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileWriteBatchResponse {
    /// Every file written, in path order, each with the hash to pass as its
    /// next `expected_sha256`.
    pub files: Vec<FileEntry>,
    /// The one generation the batch published, or null for a `blocks/`
    /// batch — only a compile turns block source into a published block.
    pub generation: Option<GenerationSummary>,
}

/// The `409` of a batch: every entry whose `expected_sha256` did not
/// describe the file as it stands. Nothing was written.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileWriteBatchConflict {
    /// In request order.
    pub conflicts: Vec<FileConflict>,
}
```
`mod.rs`: in `Route` after `ApiFilesWrite` add `/// `POST /b/dev/api/files/write-batch`` `ApiFilesWriteBatch,`; in `ROUTES` after the write entry:
```rust
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/dev/api/files/write-batch",
        Route::ApiFilesWriteBatch,
    )
    .summary("Write several workspace files as one change")
    .input(request_schema_of::<contracts::FileWriteBatchRequest>)
    .output(response_schema_of::<contracts::FileWriteBatchResponse>),
```
and in `handle`'s match: `Route::ApiFilesWriteBatch => files::handle_write_batch(ctx, &self.shared, input).await,`.

- [ ] **Step 4: The handler**

In `files.rs`, after `handle_write` (add `FileWriteBatchConflict, FileWriteBatchRequest, FileWriteBatchResponse` to the `contracts::{…}` import and `std::collections::BTreeSet`):

```rust
/// `POST /b/dev/api/files/write-batch` — create or replace several files as
/// one change.
///
/// The multi-file discipline is `scaffold::handle_create`'s: every hash,
/// collision and quota is checked against a projection before any blob is
/// stored, so a refusal on the fourth file has stored nothing for the first
/// three. A `site/` batch then publishes ONE generation; a `blocks/` batch
/// stages, exactly as a single write under `blocks/` does.
///
/// The whole batch is decoded up front — at worst `MAX_BATCH_FILES` ×
/// `MAX_FILE_BYTES`, 32 MiB — because the hash check needs every file's
/// bytes before the first store, and holding them beats hashing twice.
pub async fn handle_write_batch(
    ctx: &dyn Context,
    shared: &DevShared,
    input: InputStream,
) -> OutputStream {
    let request: FileWriteBatchRequest = match read_body(input).await {
        Ok(request) => request,
        Err(refusal) => return refusal,
    };
    if request.files.is_empty() || request.files.len() > paths::MAX_BATCH_FILES {
        return no_store_error(
            ErrorCode::InvalidArgument,
            &format!(
                "a batch writes 1 to {} files; this one has {}",
                paths::MAX_BATCH_FILES,
                request.files.len()
            ),
        );
    }

    // Validate, pin the area, and decode every file before the workspace is
    // touched. One area per call: a `site/` batch publishes and a `blocks/`
    // batch only stages, and one call has to mean one of those.
    let mut area: Option<WorkspaceArea> = None;
    let mut seen = BTreeSet::new();
    let mut decoded: Vec<(String, Vec<u8>, Option<String>)> =
        Vec::with_capacity(request.files.len());
    for file in &request.files {
        let this_area = match paths::validate_path(&file.path) {
            Ok(area) => area,
            Err(e) => {
                return no_store_error(ErrorCode::InvalidArgument, &format!("{:?}: {e}", file.path))
            }
        };
        match &area {
            None => area = Some(this_area),
            Some(first) if *first != this_area => {
                return no_store_error(
                    ErrorCode::InvalidArgument,
                    &format!(
                        "a batch writes one area — all under site/, or all under one \
                         blocks/<name>/: {:?} is not in the same area as {:?}",
                        file.path, request.files[0].path
                    ),
                );
            }
            Some(_) => {}
        }
        if !seen.insert(file.path.clone()) {
            return no_store_error(
                ErrorCode::InvalidArgument,
                &format!("{:?} appears twice in the batch", file.path),
            );
        }
        if min_decoded_len(file.encoding, &file.content) > paths::MAX_FILE_BYTES {
            return too_large(&format!(
                "{:?}: the {} body decodes to more than the {}-byte file limit",
                file.path,
                encoding_label(file.encoding),
                paths::MAX_FILE_BYTES
            ));
        }
        let bytes = match decode_content(file.encoding, &file.content) {
            Ok(bytes) => bytes,
            Err(detail) => {
                return no_store_error(ErrorCode::InvalidArgument, &format!("{:?}: {detail}", file.path))
            }
        };
        if bytes.len() > paths::MAX_FILE_BYTES {
            return too_large(&format!(
                "{:?} is {} bytes; the limit is {} bytes",
                file.path,
                bytes.len(),
                paths::MAX_FILE_BYTES
            ));
        }
        decoded.push((file.path.clone(), bytes, file.expected_sha256.clone()));
    }
    let area = area.expect("a non-empty batch has an area");

    let mut written = {
        let _serialized = shared.workspace.lock().await;
        let mut ws = match workspace::load(ctx).await {
            Ok(ws) => ws,
            Err(e) => return no_store_db_error_internal(e, "dev workspace load"),
        };

        // Every hash first. One stale entry refuses the whole batch and names
        // every stale entry, so a caller that has fallen behind re-reads once.
        let conflicts: Vec<FileConflict> = decoded
            .iter()
            .filter(|(path, _, expected)| !hash_matches(ws.get(path), expected.as_deref()))
            .map(|(path, _, _)| FileConflict::new(path, ws.get(path)))
            .collect();
        if !conflicts.is_empty() {
            return no_store()
                .status(409)
                .json(&FileWriteBatchConflict { conflicts });
        }

        // Then every collision and quota, against a projection that already
        // holds the entries ahead in this batch — so `site/a` followed by
        // `site/a/b.css` is refused here, and identical content at two paths
        // is charged once, as the store will charge it.
        let mut projected = ws.clone();
        let mut planned = Vec::with_capacity(decoded.len());
        for (path, bytes, _) in &decoded {
            if let Some(clash) = projected.path_collision(path) {
                return no_store_error(
                    ErrorCode::InvalidArgument,
                    &format!(
                        "{path:?} cannot be stored: {clash:?} already uses part of that path as a \
                         directory, or is a file this path would need as one. A name is a file or \
                         a directory, never both — rename one of them."
                    ),
                );
            }
            let sha = blobs::sha256_hex(bytes);
            let new_blob_bytes = if projected.references(&sha) {
                0
            } else {
                bytes.len() as u64
            };
            if let Err(e) = check_quotas(&projected, path, &area, new_blob_bytes) {
                return e.into_response();
            }
            projected.insert(path, sha.clone(), bytes.len() as u64);
            if new_blob_bytes > 0 {
                projected.record_blob_stored(new_blob_bytes);
            }
            planned.push((path, sha, bytes));
        }

        // Store every blob, then record every entry, then save once — the
        // order `handle_write` and `scaffold::handle_create` use, for the
        // reason they give: a manifest must never name a blob that was not
        // written, and a half-written batch must not be named at all.
        for (_, sha, bytes) in &planned {
            match blobs::put_hashed(ctx, sha, bytes).await {
                Ok(blobs::Stored::New) => ws.record_blob_stored(bytes.len() as u64),
                Ok(blobs::Stored::Deduplicated) => {}
                Err(e) => {
                    // Blobs already written are charged for even though no
                    // entry will name them; the next collection frees them.
                    if let Err(save) = workspace::save(ctx, &ws).await {
                        tracing::error!(
                            error = %save,
                            "dev workspace: a batch blob write failed and the bytes already \
                             stored could not be recorded — blob_bytes under-reports the store \
                             until the next collection"
                        );
                    }
                    return no_store_db_error_internal(e, "dev workspace blob write");
                }
            }
        }
        let written: Vec<FileEntry> = planned
            .into_iter()
            .map(|(path, sha, bytes)| ws.insert(path, sha, bytes.len() as u64))
            .collect();
        if let Err(e) = workspace::save(ctx, &ws).await {
            return no_store_db_error_internal(e, "dev workspace save");
        }
        written
    };
    written.sort_by(|a, b| a.path.cmp(&b.path));

    let generation = match publish_if_site(ctx, shared, &area, GenerationCause::SiteWrite).await {
        Ok(generation) => generation,
        Err(refusal) => return refusal,
    };
    no_store().json(&FileWriteBatchResponse {
        files: written,
        generation,
    })
}
```
(`FileEntry` and `WorkspaceArea` are already in scope in `files.rs`; `tracing` is a crate dependency — `scaffold.rs` uses it the same way.)

- [ ] **Step 5: Run the tests**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_files`
Expected: every batch test passes and every existing test still passes.

- [ ] **Step 6: Snapshots**

```bash
cargo test --locked -p impresspress-core --features block-dev,wasm --test openapi_snapshot --test endpoint_surface
UPDATE_OPENAPI_SNAPSHOTS=1 cargo test --locked -p impresspress-core --features block-dev,wasm --test openapi_snapshot
UPDATE_OPENAPI_SNAPSHOTS=1 cargo test --locked -p impresspress-core --features block-dev,wasm --test endpoint_surface
git diff --stat crates/impresspress-core/tests/snapshots
```
Expected: `dev.endpoints.json` gains exactly one line (`POST /b/dev/api/files/write-batch` at the admin level); `dev.openapi.json` gains the path and the three schemas. Anything else is a defect.

- [ ] **Step 7: Commit**

```bash
cargo +nightly fmt
git add crates/impresspress-core
git commit -m "dev: POST /b/dev/api/files/write-batch — several files, one generation

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: The tool, the page's tool set, the e2e and the docs

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/tools.rs` (`SELECTIONS`)
- Regenerate: `crates/impresspress-core/tests/snapshots/dev.tools.json`
- Modify: `crates/impresspress-web/tests/e2e/fixtures/dev-sandbox.ts` (`DEV_TOOLS`)
- Modify: `crates/impresspress-web/tests/e2e/dev-workspace.spec.ts` (a batch call after the single write)
- Modify: `docs/dev-sandbox.md` ("The workspace")

- [ ] **Step 1: The selection**

In `tools.rs`, after the `dev_write_file` row:
```rust
    (
        "impresspress/dev",
        HttpMethod::Post,
        "/b/dev/api/files/write-batch",
        "dev_write_files",
        "Write several workspace files as ONE change: all under `site/` (publishes one \
         generation) or all under one `blocks/<name>/` (stages only). Every `expected_sha256` \
         is checked before anything is written; any mismatch refuses the whole batch and lists \
         every conflict. Use it for a scaffold of several pages; `dev_write_file` for one file.",
    ),
```

- [ ] **Step 2: Regenerate the tools snapshot and confirm the manifest test**

```bash
cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_tools_manifest
UPDATE_DEV_TOOLS_SNAPSHOT=1 cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_tools_manifest
cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_tools_manifest
git diff --stat crates/impresspress-core/tests/snapshots/dev.tools.json
```
Expected: fail, regenerate, pass; the diff adds one tool whose `inputSchema` has a `files` array of the single-write shape.

- [ ] **Step 3: The page's tool set**

In `fixtures/dev-sandbox.ts`, add `'dev_write_files',` after `'dev_write_file',` in `DEV_TOOLS`, and change the doc comment's "The twelve `dev_*` tools" to "The thirteen `dev_*` tools".

- [ ] **Step 4: The scenario writes a batch**

In `dev-workspace.spec.ts`, after the single-write block that ends with `expect(wrote.generation, JSON.stringify(wrote)).not.toBeNull();` (around line 200), add:
```ts
  // A scaffold is several files and ONE generation: the batch tool.
  type FileWriteBatch = { files: { path: string; sha256: string }[]; generation: Generation | null };
  const batch = structured<FileWriteBatch>(await execute(page, 'dev_write_files', {
    files: [
      { path: 'site/about.html', content: '<!doctype html><title>About</title><h1>About</h1>', expected_sha256: null },
      { path: 'site/about.css', content: 'h1 { color: teal }', expected_sha256: null },
    ],
  }));
  expect(batch.files.map((f) => f.path)).toEqual(['site/about.css', 'site/about.html']);
  expect(batch.generation, JSON.stringify(batch)).not.toBeNull();
  expect(batch.generation!.id).not.toBe(wrote.generation!.id);
```
Then search the rest of that test for assertions that pin the exact site file list or an export's file count (`site_files`, `dev_list_files`, `dev_export_manifest`); extend each by the two new files.

- [ ] **Step 5: Docs**

In `docs/dev-sandbox.md`, "The workspace", after the sentence about `expected_sha256`, add:

> `dev_write_files` writes several files at once — all under `site/`, publishing one generation, or all under one `blocks/<name>/`, staging only. Every hash is checked before anything is written; any mismatch refuses the whole batch and lists every conflict.

- [ ] **Step 6: Commit**

```bash
git add crates/impresspress-core crates/impresspress-web/tests/e2e docs/dev-sandbox.md
git commit -m "dev: dev_write_files on /b/dev — batch writes for the agent

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Verification and the PR

- [ ] **Step 1: Rust**

```bash
cargo test --locked -p impresspress-core --features block-dev,wasm
cargo clippy --locked -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings
cargo +nightly fmt --check
```
Expected: pass, clean.

- [ ] **Step 2: The workspace e2e**

```bash
(cd crates/impresspress-web && wasm-pack build --target web --release --out-dir pkg -- --locked)
cargo install --path crates/impresspress --locked --root ./out
examples/dev-sandbox/compiler/fetch-dist.sh
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh | tail -1
python3 -m http.server 8082 -d examples/dev-sandbox/dist --bind 127.0.0.1 & echo $! > /tmp/dev-http-pid
(cd crates/impresspress-web && npm ci && npx playwright install chromium && TEST_PORT=8082 npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-workspace.spec.ts)
kill "$(cat /tmp/dev-http-pid)"
```
Expected: `3 passed` — the tool-set assertion sees `dev_write_files` registered from `tools.json`, and the batch publishes one generation through a real service worker.

- [ ] **Step 3: PR**

```bash
git push -u origin HEAD
gh pr create --title "dev: dev_write_files — batch writes, one generation" --body "$(cat <<'EOF'
Plan D of the build-sandboxes design (docs/superpowers/specs/2026-09-30-build-sandboxes-design.md §6.5, §12).

- `POST /b/dev/api/files/write-batch` / tool `dev_write_files`: 1–64 files, one area per call, every hash checked before anything is stored, `409 { conflicts }` listing every stale entry, collisions and quotas over a projection, one `publish_if_site`.
- Snapshots: `dev.endpoints.json` (+1 admin route), `dev.openapi.json`, `dev.tools.json` (+1 tool).
- e2e: the page registers the tool; the scenario writes a two-file batch and gets one generation.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
