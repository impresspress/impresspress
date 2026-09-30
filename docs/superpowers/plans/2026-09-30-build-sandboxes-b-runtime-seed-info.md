# Build Sandboxes B — Runtime Seed Info Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A seed bundle can carry a `sandbox` block (template name, suggested prompt, a site-authoring guide); the runtime stores it at import and serves it through `dev_read_reference.site_markdown`, `dev_status.template` and the `/b/dev` page, and the blank seed carries one.

**Architecture:** `SeedManifest` gains an optional `sandbox: Option<SandboxSeed>`; `seed::import` verifies the guide before storing anything and writes one row into a new singleton table `impresspress__dev__seed_info` (migration 003, repo module `repo/seed_info.rs`, modelled on `repo/runtime_state.rs`). Three readers: the reference handler, the status handler and the workspace page. The seed generator from Plan A emits the block when `seeds/<name>/sandbox.json` and `guide.md` exist.

**Tech Stack:** Rust (`impresspress-core`, feature `block-dev`), serde/schemars contracts, Maud markup, the typed `wafer_core::clients::database` client, Python seed tooling, Playwright.

**Spec:** `docs/superpowers/specs/2026-09-30-build-sandboxes-design.md` §5.2, §5.3, §6.1–§6.4, §7.2 (blank guide), §12 (PR B).

## Global Constraints

- Branch from `main` after Plan A has merged.
- `SCHEMA_VERSION` stays `1`; `sandbox` is `#[serde(default)]` and every existing bundle still parses.
- `template` follows the block-name rule (`paths::block_name_is_valid`); `suggested_prompt` ≤ 4096 bytes; the guide is exactly `guide.md` beside the manifest, ≤ 262 144 bytes, declared `text/markdown; charset=utf-8`, valid UTF-8.
- A bundle whose `sandbox` block fails any check is refused as a whole and stores nothing.
- `export.rs` writes `sandbox: None`.
- Test commands: `cargo test --locked -p impresspress-core --features block-dev,wasm`; clippy `cargo clippy --locked -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings`; format with `cargo +nightly fmt`.
- Snapshots are regenerated deliberately and reviewed: `UPDATE_DEV_TOOLS_SNAPSHOT=1 … --test dev_tools_manifest`, `UPDATE_OPENAPI_SNAPSHOTS=1 … --test openapi_snapshot` and `--test endpoint_surface` (all with `--features block-dev,wasm`).
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; PR, never direct to `main`.

## Review Focus

1. A manifest with a `sandbox` block whose `guide.md` is not in the bundle — refused with the URL named, nothing stored. (Task 2, step 5.)
2. A guide whose bytes are not UTF-8 but whose hash and size match — refused, not stored as garbage. (Task 2, step 5.)
3. A suggested prompt containing `</pre><script>` — the page escapes it; the `<pre>` must not be broken out of. (Task 4, step 3.)
4. An instance whose seed was refused (`dev_status.seed_error` set) — `dev_read_reference` still answers, with `site_markdown` and `template` null. (Task 3, step 4, the fresh-instance test.)
5. A `SeedManifest` literal somewhere that does not name `sandbox` — the compiler finds every one; the export's must say `None` with the reason. (Task 2, step 3.)

---

### Task 1: The `seed_info` table and repo module

**Files:**
- Create: `crates/impresspress-core/src/blocks/dev/migrations/003_seed_info.sqlite.sql`
- Create: `crates/impresspress-core/src/blocks/dev/migrations/003_seed_info.postgres.sql`
- Modify: `crates/impresspress-core/src/blocks/dev/migrations/mod.rs`
- Create: `crates/impresspress-core/src/blocks/dev/repo/seed_info.rs`
- Modify: `crates/impresspress-core/src/blocks/dev/repo/mod.rs` (add `pub mod seed_info;`)

**Interfaces:**
- Produces: `repo::seed_info::{TABLE, SeedInfo { template: String, suggested_prompt: String, guide_markdown: String }, read(ctx) -> Result<Option<SeedInfo>, WaferError>, write(ctx, &SeedInfo) -> Result<(), WaferError>}`.

- [ ] **Step 1: The migrations**

`003_seed_info.sqlite.sql`:
```sql
-- The single-row record of what the seed bundle said about this sandbox:
-- which template seeded it, the prompt the workspace page suggests, and the
-- site-authoring guide `dev_read_reference` serves as `site_markdown`.
--
-- Seeded at rest with every column NULL, the way `runtime_state` is seeded
-- idle: `seed::import` UPDATEs the row on the boot that seeds the instance,
-- and a NULL `template` is how the row says the seed carried no `sandbox`
-- block (an exported bundle never does). One row, never inserted by code.
CREATE TABLE IF NOT EXISTS impresspress__dev__seed_info (
    singleton_id     INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    template         TEXT,
    suggested_prompt TEXT,
    guide_markdown   TEXT,
    imported_at      TEXT
);

INSERT OR IGNORE INTO impresspress__dev__seed_info
    (singleton_id, template, suggested_prompt, guide_markdown, imported_at)
VALUES (1, NULL, NULL, NULL, NULL);
```

`003_seed_info.postgres.sql`:
```sql
-- What the seed bundle said about this sandbox. See the SQLite variant for
-- why the row is seeded empty rather than inserted by code.
CREATE TABLE IF NOT EXISTS impresspress__dev__seed_info (
    singleton_id     INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    template         TEXT,
    suggested_prompt TEXT,
    guide_markdown   TEXT,
    imported_at      TEXT
);

INSERT INTO impresspress__dev__seed_info
    (singleton_id, template, suggested_prompt, guide_markdown, imported_at)
VALUES (1, NULL, NULL, NULL, NULL)
ON CONFLICT (singleton_id) DO NOTHING;
```

`migrations/mod.rs`: add
```rust
const SQL_003_SQLITE: &str = include_str!("003_seed_info.sqlite.sql");
#[cfg(feature = "postgres")]
const SQL_003_POSTGRES: &str = include_str!("003_seed_info.postgres.sql");
```
append `("003_seed_info", SQL_003_SQLITE)` to `SQLITE_MIGRATIONS` and `SQL_003_POSTGRES` to `POSTGRES_MIGRATIONS`.

- [ ] **Step 2: Write the failing repo tests**

Create `crates/impresspress-core/src/blocks/dev/repo/seed_info.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    /// The migration seeds the row empty; empty reads as "no sandbox block".
    #[tokio::test]
    async fn migration_seeds_an_empty_row_that_reads_as_none() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        assert_eq!(read(&ctx).await.expect("read"), None);
    }

    #[tokio::test]
    async fn write_then_read_round_trips_every_field() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let info = SeedInfo {
            template: "bootstrap".to_string(),
            suggested_prompt: "Build me a shop.".to_string(),
            guide_markdown: "# Guide\n\nWrite HTML.\n".to_string(),
        };
        write(&ctx, &info).await.expect("write");
        assert_eq!(read(&ctx).await.expect("read"), Some(info));
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm seed_info`
Expected: compile error — `read`, `write`, `SeedInfo` not found.

- [ ] **Step 4: Implement the module**

Above the tests in `repo/seed_info.rs`:

```rust
//! The single-row record of what the seed bundle said about this sandbox
//! (`impresspress__dev__seed_info`): which template seeded it, the prompt
//! the workspace page suggests, and the site-authoring guide
//! `dev_read_reference` serves as `site_markdown`.
//!
//! Written once, by `seed::import`, on the boot that seeds the instance;
//! read on every reference call, status poll and workspace page render. The
//! migration seeds the row with every column `NULL`, and a `NULL` template is
//! how the row says "this instance's seed carried no `sandbox` block" — an
//! exported bundle never does (`export` writes `sandbox: None`).

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use crate::util::RecordExt;

pub const TABLE: &str = "impresspress__dev__seed_info";

/// Primary-key value of the one row this table holds.
const SINGLETON_COLUMN: &str = "singleton_id";
const SINGLETON_ID: i64 = 1;

/// What the seed's `sandbox` block carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedInfo {
    /// The template's name, e.g. `bootstrap`.
    pub template: String,
    /// The paragraph the workspace page offers for copying.
    pub suggested_prompt: String,
    /// The site-authoring guide, Markdown.
    pub guide_markdown: String,
}

/// The row, or `None` until an import has written it.
pub async fn read(ctx: &dyn Context) -> Result<Option<SeedInfo>, WaferError> {
    let record = db::get_by_field(
        ctx,
        TABLE,
        SINGLETON_COLUMN,
        serde_json::json!(SINGLETON_ID),
    )
    .await?;
    let Some(template) = record.opt_str_field("template") else {
        return Ok(None);
    };
    Ok(Some(SeedInfo {
        template,
        suggested_prompt: record.str_field("suggested_prompt").to_string(),
        guide_markdown: record.str_field("guide_markdown").to_string(),
    }))
}

/// Overwrite the row with `info`, stamping `imported_at`.
pub async fn write(ctx: &dyn Context, info: &SeedInfo) -> Result<(), WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "template": info.template,
        "suggested_prompt": info.suggested_prompt,
        "guide_markdown": info.guide_markdown,
        "imported_at": super::now(),
    }));
    db::update_by_filters(
        ctx,
        TABLE,
        vec![Filter {
            field: SINGLETON_COLUMN.to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(SINGLETON_ID),
        }],
        data,
    )
    .await
}
```
Add `pub mod seed_info;` to `repo/mod.rs` between `generations` and `runtime_state`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm seed_info`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/impresspress-core/src/blocks/dev/migrations crates/impresspress-core/src/blocks/dev/repo
git commit -m "dev: seed_info table — what the seed said about this sandbox

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: The `sandbox` manifest block and the import step

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/seed.rs` (struct, constants, `fetch_sandbox`, two lines in `import_bundle`)
- Modify: `crates/impresspress-core/src/blocks/dev/export.rs:345-355` (`sandbox: None`)
- Modify: every other `SeedManifest { … }` literal the compiler names (`grep -rn 'SeedManifest {' crates/impresspress-core`)
- Test: `crates/impresspress-core/tests/dev_seed.rs`

**Interfaces:**
- Consumes: `repo::seed_info::{SeedInfo, write}` (Task 1).
- Produces: `seed::SandboxSeed { template: String, suggested_prompt: String, guide: SeedFile }`, `SeedManifest.sandbox: Option<SandboxSeed>`, `seed::{GUIDE_PATH, GUIDE_CONTENT_TYPE, MAX_GUIDE_BYTES, MAX_PROMPT_BYTES, guide_url(path)}`.

- [ ] **Step 1: Write the failing tests**

In `tests/dev_seed.rs`, add to the imports `repo::seed_info` (inside the existing `repo::{…}` list) and `seed::SandboxSeed` (inside `seed::{…}`), then add after the `bundle()` fixture:

```rust
const GUIDE: &[u8] = b"# Building the site\n\nWrite semantic HTML.\n";

fn guide_file() -> seed::SeedFile {
    seed::SeedFile {
        path: seed::GUIDE_PATH.to_string(),
        sha256: blobs::sha256_hex(GUIDE),
        size: GUIDE.len() as u64,
        content_type: seed::GUIDE_CONTENT_TYPE.to_string(),
    }
}

fn sandbox() -> SandboxSeed {
    SandboxSeed {
        template: "blank".to_string(),
        suggested_prompt: "Build me a shop.".to_string(),
        guide: guide_file(),
    }
}

fn manifest_with(sandbox: SandboxSeed) -> SeedManifest {
    SeedManifest {
        sandbox: Some(sandbox),
        ..manifest()
    }
}

fn bundle_with_guide() -> MapFetch {
    bundle().with(&seed::guide_url(seed::GUIDE_PATH), GUIDE)
}

/// Import `manifest` and expect a refusal; nothing may have been stored.
async fn refused(manifest: &SeedManifest, bundle: &MapFetch) -> String {
    let (ctx, control) = fixture().await;
    let err = seed::import(&ctx, control.as_ref(), manifest, bundle)
        .await
        .expect_err("refused");
    let ws = workspace::load(&ctx).await.expect("workspace");
    assert!(ws.files.is_empty(), "stored files: {:?}", ws.files.keys());
    assert_eq!(ws.blob_count, 0);
    assert_eq!(seed_info::read(&ctx).await.expect("read"), None);
    err
}

// ---------------------------------------------------------------------------
// The sandbox block
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_sandbox_block_is_recorded_for_the_reference_and_the_page() {
    let (ctx, control) = fixture().await;
    seed::import(&ctx, control.as_ref(), &manifest_with(sandbox()), &bundle_with_guide())
        .await
        .expect("import")
        .expect("fresh");
    let info = seed_info::read(&ctx).await.expect("read").expect("a row");
    assert_eq!(info.template, "blank");
    assert_eq!(info.suggested_prompt, "Build me a shop.");
    assert_eq!(info.guide_markdown, std::str::from_utf8(GUIDE).unwrap());
}

#[tokio::test]
async fn a_bundle_without_a_sandbox_block_records_nothing() {
    let (ctx, control) = fixture().await;
    seed::import(&ctx, control.as_ref(), &manifest(), &bundle())
        .await
        .expect("import")
        .expect("fresh");
    assert_eq!(seed_info::read(&ctx).await.expect("read"), None);
}

#[tokio::test]
async fn a_guide_over_the_limit_is_refused_before_anything_is_stored() {
    let mut declared = sandbox();
    declared.guide.size = (seed::MAX_GUIDE_BYTES + 1) as u64;
    let err = refused(&manifest_with(declared), &bundle_with_guide()).await;
    assert!(err.contains("/seed/guide.md") && err.contains("limit"), "{err}");
}

#[tokio::test]
async fn a_guide_not_in_the_bundle_is_refused() {
    let err = refused(&manifest_with(sandbox()), &bundle()).await;
    assert!(err.contains("/seed/guide.md"), "{err}");
}

#[tokio::test]
async fn a_guide_that_is_not_utf8_is_refused() {
    let bytes: &[u8] = b"# Guide\n\xff\xfe";
    let mut declared = sandbox();
    declared.guide.sha256 = blobs::sha256_hex(bytes);
    declared.guide.size = bytes.len() as u64;
    let bundle = bundle().with(&seed::guide_url(seed::GUIDE_PATH), bytes);
    let err = refused(&manifest_with(declared), &bundle).await;
    assert!(err.contains("UTF-8"), "{err}");
}

#[tokio::test]
async fn a_template_name_that_is_not_a_block_name_is_refused() {
    let mut declared = sandbox();
    declared.template = "Boot strap".to_string();
    let err = refused(&manifest_with(declared), &bundle_with_guide()).await;
    assert!(err.contains("sandbox.template"), "{err}");
}

#[tokio::test]
async fn a_prompt_over_the_limit_is_refused() {
    let mut declared = sandbox();
    declared.suggested_prompt = "x".repeat(seed::MAX_PROMPT_BYTES + 1);
    let err = refused(&manifest_with(declared), &bundle_with_guide()).await;
    assert!(err.contains("suggested_prompt"), "{err}");
}

#[tokio::test]
async fn a_guide_declared_with_another_content_type_is_refused() {
    let mut declared = sandbox();
    declared.guide.content_type = "text/plain; charset=utf-8".to_string();
    let err = refused(&manifest_with(declared), &bundle_with_guide()).await;
    assert!(err.contains("content type"), "{err}");
}

#[tokio::test]
async fn a_guide_not_named_guide_md_is_refused() {
    let mut declared = sandbox();
    declared.guide.path = "README.md".to_string();
    let bundle = bundle().with(&seed::guide_url("README.md"), GUIDE);
    let err = refused(&manifest_with(declared), &bundle).await;
    assert!(err.contains("guide.md"), "{err}");
}

/// The field is additive: a manifest written before it existed parses, and
/// a manifest without it serializes `null` (what every export writes).
#[test]
fn the_sandbox_field_is_optional_in_both_directions() {
    let value = serde_json::to_value(manifest()).expect("serialize");
    assert!(value["sandbox"].is_null());
    let mut without = value.clone();
    without.as_object_mut().unwrap().remove("sandbox");
    let parsed: SeedManifest = serde_json::from_value(without).expect("parse without the field");
    assert!(parsed.sandbox.is_none());
    let round: SeedManifest =
        serde_json::from_value(serde_json::to_value(manifest_with(sandbox())).unwrap())
            .expect("round trip");
    assert_eq!(round.sandbox.expect("sandbox").template, "blank");
}
```
Also add `sandbox: None,` to the existing `manifest()` fixture's struct literal.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_seed`
Expected: compile error — no field `sandbox`, no `SandboxSeed`.

- [ ] **Step 3: Implement in `seed.rs`**

After the `SeedBlock` struct add:

```rust
/// What a seed bundle says about the sandbox it seeds — the template that
/// produced it, the prompt the workspace page suggests, and the
/// site-authoring guide `dev_read_reference` serves (build-sandboxes design
/// §5.2). Never present on an exported bundle: an export boots with no
/// `/b/dev`, so a guide there would describe tools the bundle does not have.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxSeed {
    /// The template's name, e.g. `bootstrap`. Same rule as a block name.
    pub template: String,
    /// One paragraph the page shows verbatim; at most [`MAX_PROMPT_BYTES`].
    pub suggested_prompt: String,
    /// The guide, a Markdown file named [`GUIDE_PATH`] beside the manifest,
    /// verified like every other file: hash, size, content type.
    pub guide: SeedFile,
}
```
In `SeedManifest`, after `data`:
```rust
    /// The sandbox block, when this bundle seeds a workspace sandbox rather
    /// than an exported site. `#[serde(default)]` so a manifest written before
    /// the field existed still imports; `SCHEMA_VERSION` is unchanged.
    #[serde(default)]
    pub sandbox: Option<SandboxSeed>,
```
After `MAX_DATA_BYTES`:
```rust
/// The one name the guide may have, beside the manifest.
pub const GUIDE_PATH: &str = "guide.md";

/// What the guide is declared and served as. Not `paths::content_type_for`:
/// the guide is not a workspace file and is never published.
pub const GUIDE_CONTENT_TYPE: &str = "text/markdown; charset=utf-8";

/// Largest guide a bundle may carry. Sized for a document an agent reads
/// once per session, not for a manual.
pub const MAX_GUIDE_BYTES: usize = 256 * 1024;

/// Largest suggested prompt: one paragraph, shown verbatim on the page.
pub const MAX_PROMPT_BYTES: usize = 4 * 1024;

/// URL of the guide.
pub fn guide_url(path: &str) -> String {
    format!("{ROOT}{path}")
}
```
Add `seed_info` beside `runtime_state` in the `use super::{ … repo::{…} … }` block at the top of the file.

In `import_bundle`, right after the `MAX_BLOCKS` check:
```rust
    // The sandbox block, if any — verified before a single site byte is
    // stored, so a bundle whose guide is wrong stores nothing at all. Held
    // until the end, when the row is written beside the workspace it
    // describes.
    let sandbox = match &manifest.sandbox {
        Some(declared) => Some(fetch_sandbox(fetch, declared).await?),
        None => None,
    };
```
Immediately before the final `Ok(Some(GenerationManifest::staged(`:
```rust
    if let Some(info) = &sandbox {
        seed_info::write(ctx, info)
            .await
            .map_err(|e| format!("recording the seed's sandbox block: {}", e.message))?;
    }
```
After `fetch_verified` add:
```rust
/// Check and fetch a bundle's sandbox block: the template name, the prompt
/// length, then the guide through [`fetch_and_verify`] like any other file.
async fn fetch_sandbox(
    fetch: &dyn SeedFetch,
    declared: &SandboxSeed,
) -> Result<seed_info::SeedInfo, String> {
    if !paths::block_name_is_valid(&declared.template) {
        return Err(format!(
            "the seed bundle's sandbox.template {:?} is not allowed: {}",
            declared.template,
            paths::BLOCK_NAME_RULE
        ));
    }
    if declared.suggested_prompt.len() > MAX_PROMPT_BYTES {
        return Err(format!(
            "the seed bundle's sandbox.suggested_prompt is {} bytes; the limit is {MAX_PROMPT_BYTES}",
            declared.suggested_prompt.len()
        ));
    }
    if declared.guide.path != GUIDE_PATH {
        return Err(format!(
            "the seed bundle's sandbox.guide is named {:?}; it must be {GUIDE_PATH:?} beside the manifest",
            declared.guide.path
        ));
    }
    let url = guide_url(&declared.guide.path);
    let bytes =
        fetch_and_verify(fetch, &url, &declared.guide, GUIDE_CONTENT_TYPE, MAX_GUIDE_BYTES).await?;
    let guide_markdown =
        String::from_utf8(bytes).map_err(|_| format!("{url}: the guide is not valid UTF-8"))?;
    Ok(seed_info::SeedInfo {
        template: declared.template.clone(),
        suggested_prompt: declared.suggested_prompt.clone(),
        guide_markdown,
    })
}
```
In `export.rs`, in the `SeedManifest { … }` literal after `data: Some(…)`, add:
```rust
        // An export boots with the workspace off — no `/b/dev`, no reference
        // tool — so a guide would describe tools the bundle does not have.
        sandbox: None,
```
Then `cargo build -p impresspress-core --features block-dev,wasm --all-targets` and add `sandbox: None` to every other `SeedManifest {` literal it names.

- [ ] **Step 4: Run the seed tests**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_seed`
Expected: all pass, including the ten new ones. Then the whole suite: `cargo test --locked -p impresspress-core --features block-dev,wasm` — expected: pass (the `dev_export` and `dev_data_snapshot` round trips still import).

- [ ] **Step 5: Confirm the refusal tests store nothing**

They do, by construction (`refused` asserts an empty workspace and no row). Re-read the assertion once: `a_guide_not_in_the_bundle_is_refused` fails at `fetch.get` with `/seed/guide.md: not in the bundle`, and `a_guide_that_is_not_utf8_is_refused` passes hash and size and fails on `from_utf8` — both before the site loop runs.

- [ ] **Step 6: Commit**

```bash
cargo +nightly fmt
git add crates/impresspress-core
git commit -m "dev: seed bundles may carry a sandbox block; the import records it

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `dev_read_reference.site_markdown`, `dev_status.template`, the tool description

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/contracts.rs` (`ReferenceResponse` ~line 512, `StatusResponse` line 26)
- Modify: `crates/impresspress-core/src/blocks/dev/scaffold.rs:326-332` (`handle_reference`)
- Modify: `crates/impresspress-core/src/blocks/dev/status.rs:24-66` (`build`)
- Modify: `crates/impresspress-core/src/blocks/dev/tools.rs` (the `dev_read_reference` description)
- Test: `crates/impresspress-core/tests/dev_scaffold.rs`, `crates/impresspress-core/tests/dev_status.rs`
- Regenerate: `crates/impresspress-core/tests/snapshots/dev.tools.json`, `dev.openapi.json`

**Interfaces:**
- Consumes: `repo::seed_info::{read, write, SeedInfo}`.
- Produces: `ReferenceResponse { …, template: Option<String>, site_markdown: Option<String> }`, `StatusResponse { …, template: Option<String> }`.

- [ ] **Step 1: Failing tests**

In `tests/dev_scaffold.rs` add `repo::seed_info::{self, SeedInfo}` to the `blocks::dev::{…}` import and append:

```rust
/// A sandbox seeded from a template serves that template's site guide beside
/// the Rust one, under its own field, so an agent building a static site
/// reads the right document first.
#[tokio::test]
async fn reference_returns_the_site_guide_the_seed_carried() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    seed_info::write(
        &ctx,
        &SeedInfo {
            template: "bootstrap".to_string(),
            suggested_prompt: "Build me a shop.".to_string(),
            guide_markdown: "# Building the site\n\nLink /vendor/bootstrap/bootstrap.min.css.\n"
                .to_string(),
        },
    )
    .await
    .expect("seed info");
    let body = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/reference"))
            .await,
    )
    .await;
    assert_eq!(body["template"], "bootstrap");
    assert_eq!(
        body["site_markdown"],
        "# Building the site\n\nLink /vendor/bootstrap/bootstrap.min.css.\n"
    );
    // The Rust guide is untouched by the seed.
    assert!(body["markdown"].as_str().unwrap().contains("Block::new"));
}

/// No sandbox block (an instance whose seed was refused, or carried none):
/// both fields are null and the call still answers.
#[tokio::test]
async fn reference_without_a_seed_guide_answers_null_fields() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let body = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/reference"))
            .await,
    )
    .await;
    assert_eq!(body["template"], serde_json::Value::Null);
    assert_eq!(body["site_markdown"], serde_json::Value::Null);
}
```
In `tests/dev_status.rs` add `repo::seed_info::{self, SeedInfo}` to the imports (`repo` is already imported there — extend that list) and append:

```rust
#[tokio::test]
async fn status_reports_the_template_the_seed_named() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    assert_eq!(dev_status(&ctx).await["template"], serde_json::Value::Null);
    seed_info::write(
        &ctx,
        &SeedInfo {
            template: "bootstrap".to_string(),
            suggested_prompt: String::new(),
            guide_markdown: String::new(),
        },
    )
    .await
    .expect("seed info");
    assert_eq!(dev_status(&ctx).await["template"], "bootstrap");
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_scaffold --test dev_status`
Expected: the three new tests fail (`template` is `null`/absent).

- [ ] **Step 3: Implement**

`contracts.rs`, `ReferenceResponse` — add after `wafer_guest_module`:
```rust
    /// The template this sandbox was seeded from (`dev_status.template`), or
    /// null when the seed carried no sandbox block.
    pub template: Option<String>,
    /// This sandbox's site-authoring guide, as Markdown: the CSS framework
    /// it ships, the page skeleton, the storefront element, the catalog API,
    /// what a write refuses. Read it before writing under `site/`. Null when
    /// the seed carried none.
    pub site_markdown: Option<String>,
```
`StatusResponse` — add after `seed_error`:
```rust
    /// The template this sandbox was seeded from, or null when the seed
    /// carried no sandbox block. What an agent reads to know which guide
    /// `dev_read_reference.site_markdown` will be.
    pub template: Option<String>,
```
`scaffold.rs` `handle_reference`:
```rust
/// `GET /b/dev/api/reference` — the authoring guides: the Rust one this
/// crate ships, and the site one the seed carried.
pub async fn handle_reference(ctx: &dyn Context) -> OutputStream {
    let seed = match seed_info::read(ctx).await {
        Ok(seed) => seed,
        Err(e) => return no_store_db_error_internal(e, "dev reference: seed info read"),
    };
    no_store().json(&ReferenceResponse {
        wafer_guest_version: WAFER_GUEST_VERSION,
        markdown: reference_markdown(),
        wafer_guest_module: Template::WAFER_GUEST.to_string(),
        template: seed.as_ref().map(|seed| seed.template.clone()),
        site_markdown: seed.map(|seed| seed.guide_markdown),
    })
}
```
(add `repo::seed_info` to scaffold.rs's `use super::{…}`; `no_store_db_error_internal` is already imported there.)

`status.rs` `build`: add `template: seed_info::read(ctx).await?.map(|seed| seed.template),` as the last field, with the comment `// One indexed singleton read per poll, like seed_error.`; import `repo::seed_info` (the file already imports `repo`).

`tools.rs`, the `dev_read_reference` row's description:
```rust
        "The authoring guides. `markdown` is the backend-block guide (API, host services, \
         limits, the two templates) — read it before writing Rust. `site_markdown` is this \
         sandbox's site-authoring guide (the CSS framework it ships, the page skeleton, the \
         storefront element, the catalog API, what a write refuses) — read it before writing \
         under `site/`.",
```

- [ ] **Step 4: Run the tests, then regenerate the snapshots**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_scaffold --test dev_status`
Expected: pass.
Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_tools_manifest --test openapi_snapshot --test endpoint_surface`
Expected: `dev_tools_manifest` and `openapi_snapshot` fail with "regenerate deliberately". Then:
```bash
UPDATE_DEV_TOOLS_SNAPSHOT=1 cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_tools_manifest
UPDATE_OPENAPI_SNAPSHOTS=1 cargo test --locked -p impresspress-core --features block-dev,wasm --test openapi_snapshot
git diff --stat crates/impresspress-core/tests/snapshots
```
Expected diff: only the `dev_read_reference` description in `dev.tools.json`, and the two new response properties in `dev.openapi.json`. `dev.endpoints.json` unchanged (no new route). Anything else in the diff is a defect — stop and look.

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add crates/impresspress-core
git commit -m "dev: serve the seed's site guide and template through the reference and status

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: The workspace page renders the seed's prompt

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/page.rs` (`SUGGESTED_PROMPT` removed; `handle`; `body(seed)`; the unit test)
- Test: `crates/impresspress-core/tests/dev_page.rs`

**Interfaces:**
- Consumes: `repo::seed_info::{read, SeedInfo}`.
- Produces: `page::body(seed: Option<&SeedInfo>) -> Markup` (crate-private).

- [ ] **Step 1: Failing tests**

In `tests/dev_page.rs` (it already has `navigation`, `output_html`, `dev_with_accounts`, `signed_in_as`, `anon_msg`; add `repo::seed_info::{self, SeedInfo}` to the `blocks::dev::{…}` import), append:

```rust
/// The prompt an operator copies comes from the seed, not from the crate:
/// each template's sandbox suggests its own. Without a sandbox block there is
/// no disclosure at all — an empty `<pre>` would be a prompt that says
/// nothing.
#[tokio::test]
async fn the_suggested_prompt_and_template_come_from_the_seed() {
    let ctx = dev_with_accounts(FakeControl::new()).await;
    let operator = signed_in_as(&ctx, "admin").await;

    let before = output_html(
        ctx.request(navigation(operator.cookie(anon_msg("retrieve", "/b/dev"))))
            .await,
    )
    .await;
    assert!(!before.contains("dev-suggested-prompt"), "{before}");
    assert!(!before.contains("Suggested prompt"));

    seed_info::write(
        &ctx,
        &SeedInfo {
            template: "bootstrap".to_string(),
            // Hostile on purpose: the page must escape it, not render it.
            suggested_prompt: "Build me a shop </pre><script>alert(1)</script>".to_string(),
            guide_markdown: String::new(),
        },
    )
    .await
    .expect("seed info");

    let after = output_html(
        ctx.request(navigation(operator.cookie(anon_msg("retrieve", "/b/dev"))))
            .await,
    )
    .await;
    assert!(after.contains(r#"<pre id="dev-suggested-prompt">"#), "{after}");
    assert!(
        after.contains("Build me a shop &lt;/pre&gt;&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{after}"
    );
    assert!(!after.contains("<script>alert(1)</script>"));
    assert!(after.contains("<strong>bootstrap</strong>"), "names the template: {after}");
    assert!(after.contains("site_markdown"), "points at the site guide: {after}");
}
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_page the_suggested_prompt`
Expected: fails on the first assertion (the disclosure is rendered from the constant today).

- [ ] **Step 3: Implement**

In `page.rs`:
- Delete the `SUGGESTED_PROMPT` constant and its doc comment (its text moves to `seeds/blank/sandbox.json` in Task 5 — copy it out first).
- Add `use super::repo::seed_info::{self, SeedInfo};`.
- `handle`:
```rust
pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let seed = match seed_info::read(ctx).await {
        Ok(seed) => seed,
        Err(e) => return super::no_store_db_error_internal(e, "workspace page: seed info read failed"),
    };
    let shell = ui::Shell::simple("Workspace", ui::NavKind::Admin, "Workspace");
    let markup = match ui::shell_document(ctx, msg, shell, body(seed.as_ref())).await {
```
- `body`:
```rust
/// The page body: six panes with stable ids, then the assets that drive them.
///
/// `seed` is what the seed bundle said about this sandbox — the template
/// and the prompt to suggest. `None` renders no prompt disclosure at all.
fn body(seed: Option<&SeedInfo>) -> Markup {
    html! {
        div .dev-workspace {
            section #dev-guide .dev-pane {
                h2 { "How this workspace works" }
                @if let Some(seed) = seed {
                    p {
                        "This sandbox was seeded from the " strong { (seed.template) } " template. "
                        code { "dev_read_reference" } " returns two guides: " code { "markdown" }
                        " for backend blocks and " code { "site_markdown" } " for the site — read \
                         the second before writing under " code { "site/" } "."
                    }
                }
                p {
                    … (the existing "This page is a WebMCP workspace…" paragraph, unchanged)
                }
                p {
                    … (the existing "Start with dev_status…" paragraph, unchanged)
                }
                @if let Some(seed) = seed {
                    details {
                        summary { "Suggested prompt" }
                        pre #dev-suggested-prompt { (seed.suggested_prompt) }
                    }
                }
            }
            … (everything else unchanged)
```
- The unit test `every_id_dev_js_looks_up_is_in_the_document`: `body()` → `body(None)`.

- [ ] **Step 4: Run the page tests**

Run: `cargo test --locked -p impresspress-core --features block-dev,wasm --test dev_page` and `cargo test --locked -p impresspress-core --features block-dev,wasm page::`
Expected: pass. Maud escapes `(seed.suggested_prompt)` by default, which is what the hostile-prompt assertion pins.

- [ ] **Step 5: Commit**

```bash
cargo +nightly fmt
git add crates/impresspress-core
git commit -m "dev: the workspace page suggests the seed's prompt and names its template

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: The blank seed carries a sandbox block; the tooling emits and stages it

**Files:**
- Create: `examples/dev-sandbox/seeds/blank/sandbox.json`
- Create: `examples/dev-sandbox/seeds/blank/guide.md`
- Modify: `examples/dev-sandbox/seeds/seedlib.py` (`sandbox_block`, limits), `examples/dev-sandbox/seeds/check-seeds.py` (nothing — equality covers it), `examples/dev-sandbox/build.sh` (`stage_seed` copies `guide.md`)
- Regenerate: `examples/dev-sandbox/seeds/blank/manifest.json`
- Modify: `crates/impresspress-web/tests/e2e/dev-workspace.spec.ts` (one assertion)
- Modify: `docs/dev-sandbox.md` ("Starting one")

- [ ] **Step 1: `sandbox.json`** — the prompt is the former `SUGGESTED_PROMPT` constant, verbatim:

```json
{
  "template": "blank",
  "suggested_prompt": "Build me a small online shop for handmade ceramics. Create a home page at site/index.html that lists products from /b/products/catalog and lets a visitor open one, using the storefront widget from /b/products/storefront.js, and include <script src=\"/b/webmcp/webmcp.js\" defer></script> in its <head> so a visitor's agent can use the shop's tools. Then create three products with shop_create_product, give each a published offer with shop_create_offer and shop_publish_offer, and set their status to active with shop_update_product. Show me the live site when you are done."
}
```

- [ ] **Step 2: `guide.md`**

```markdown
# Building the site in this sandbox

This sandbox seeded a blank site: `site/index.html` and `site/styles.css`,
and no CSS framework. Write your own stylesheet, or replace `styles.css`.

## How `site/` works

- Every file under `site/` is published verbatim, and every write publishes
  a new generation immediately — there is no separate deploy.
- `site/index.html` is the entrypoint, served at `/`. A subdirectory is a
  route: `site/blog/index.html` serves at `/blog/`, `site/about.html` at
  `/about.html`.
- Read a file before overwriting it: `dev_write_file` takes the file's
  current `sha256` as `expected_sha256`; a new file takes `null`.
- `dev_rollback` republishes an earlier generation when something regresses.

## Page skeleton

Every page you write carries this `<head>`:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Page title</title>
  <link rel="stylesheet" href="/styles.css" />
  <script src="/b/webmcp/webmcp.js" defer></script>
</head>
```

`/b/webmcp/webmcp.js` gives a visitor's own browser agent the site's public
tools — the shop's, and any compiled block's agent tools. Without the tag a
visitor's agent sees a plain page.

## The shop

Products are managed with the `shop_*` tools on this page. A page reads them
through two public pieces:

- `GET /b/products/catalog` lists active products as JSON:
  `{"records": [...], "total_count": N, "page": 1, "page_size": M}`. Each
  record carries `id`, `name`, `slug`, `description`, `image_url`, `tags`,
  `category`, `currency`, `stock`, `metadata` and `fulfillment_kind`. Pass
  `?page=2` for the next page; `?page_size=` goes up to 100.
- `<impresspress-product product-id="…"></impresspress-product>` renders one
  product's price and buy button. Load
  `<script src="/b/products/storefront.js" defer></script>` once per page.
  Attributes: `product-id` (required), `presentation` (`hosted`, `embedded`
  or `payment_link`; default `hosted`), `api-base` (default: this origin),
  `credentials` (`same-origin`, `omit` or `include`).

A product appears in the catalog once `shop_update_product` sets
`status: "active"`; it can be bought once it has a published offer
(`shop_create_offer`, then `shop_publish_offer`).

## Calling a backend block from a page

A block you compiled serves under `/b/<name>/`. Call it with
`fetch('/b/<name>/…')` — same origin, so no CORS or credentials setup — and
send and read JSON.

## What a write refuses

- A path outside `site/` or `blocks/<name>/`, a `..` segment, or a name that
  clashes with an existing file or directory.
- A file over 512 KiB; more than 2,000 files; more than 64 MiB of stored
  content in the workspace.
- A stale `expected_sha256`: the refusal carries the current hash, so
  re-read and retry.

## Workflow

1. `dev_status`, then this reference.
2. Read `site/index.html`, then overwrite it with your page.
3. Add pages and assets with further writes; each write is one generation.
4. Stock the shop with `shop_*`, then check the live site at `/`.
5. `dev_export` when done.
```
The line beginning `Attributes:` is a convention Plan C's drift test reads: every backticked `name-with-hyphen` on that line must be an attribute `storefront.js` reads.

- [ ] **Step 3: Extend `seedlib.py`**

Add after `render`:

```python
GUIDE_CONTENT_TYPE = "text/markdown; charset=utf-8"
MAX_GUIDE_BYTES = 256 * 1024
MAX_PROMPT_BYTES = 4 * 1024
TEMPLATE_NAME = re.compile(r"[a-z][a-z0-9-]{1,31}")  # a block name (paths::block_name_is_valid)


def sandbox_block(seed_dir: pathlib.Path):
    """The `sandbox` block of a seed that carries sandbox.json and guide.md,
    or None. Checked here to the runtime's own limits, so a seed the
    importer would refuse at boot is refused at generation time instead."""
    sandbox_path = seed_dir / "sandbox.json"
    if not sandbox_path.is_file():
        return None
    sandbox = json.loads(sandbox_path.read_text())
    if set(sandbox) != {"template", "suggested_prompt"}:
        raise SystemExit(f"{sandbox_path}: needs exactly the keys template and suggested_prompt")
    if not TEMPLATE_NAME.fullmatch(sandbox["template"]):
        raise SystemExit(f"{sandbox_path}: template {sandbox['template']!r} is not a valid name")
    if len(sandbox["suggested_prompt"].encode()) > MAX_PROMPT_BYTES:
        raise SystemExit(f"{sandbox_path}: suggested_prompt is over {MAX_PROMPT_BYTES} bytes")
    guide_path = seed_dir / "guide.md"
    if not guide_path.is_file():
        raise SystemExit(f"{seed_dir}: sandbox.json is present but guide.md is missing")
    data = guide_path.read_bytes()
    if len(data) > MAX_GUIDE_BYTES:
        raise SystemExit(f"{guide_path}: {len(data)} bytes is over the {MAX_GUIDE_BYTES}-byte limit")
    data.decode("utf-8")  # a guide that is not UTF-8 is refused at boot
    return {
        "template": sandbox["template"],
        "suggested_prompt": sandbox["suggested_prompt"],
        "guide": {
            "path": "guide.md",
            "sha256": sha256_hex(data),
            "size": len(data),
            "content_type": GUIDE_CONTENT_TYPE,
        },
    }
```
Add `import re` at the top, and in `build_manifest` after `"data": None,` — since the field must come last and only when present — restructure the return:
```python
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "source_generation": None,
        "site": site_entries(site_dir),
        "blocks": [],
        "data": None,
    }
    sandbox = sandbox_block(seed_dir)
    if sandbox is not None:
        manifest["sandbox"] = sandbox
    return manifest
```

- [ ] **Step 4: Stage the guide in `build.sh`**

In `stage_seed()` after the `cp -R "$src/site" …` line:
```bash
  # The guide rides the bundle when the seed carries a sandbox block; the
  # manifest names it, so a seed with one and no file fails the check above.
  if [ -f "$src/guide.md" ]; then cp "$src/guide.md" "$HERE/seed/guide.md"; fi
```

- [ ] **Step 5: Regenerate and check**

```bash
python3 examples/dev-sandbox/seeds/write-manifest.py blank
python3 examples/dev-sandbox/seeds/check-seeds.py
python3 -c "import json; m=json.load(open('examples/dev-sandbox/seeds/blank/manifest.json')); print(list(m), m['sandbox']['template'], m['sandbox']['guide']['content_type'])"
```
Expected: the check passes; the last line prints `['schema_version', 'source_generation', 'site', 'blocks', 'data', 'sandbox'] blank text/markdown; charset=utf-8`.

- [ ] **Step 6: The workspace e2e reads the template**

In `crates/impresspress-web/tests/e2e/dev-workspace.spec.ts`, in the scenario test, right after the first `execute(page, 'dev_status', …)` call (search `'dev_status'`): if the result is bound, add
```ts
  expect((status as { template: string | null }).template).toBe('blank');
```
and if it is discarded, bind it first:
```ts
  const status = structured<{ template: string | null }>(await execute(page, 'dev_status', {}));
  expect(status.template).toBe('blank');
```

- [ ] **Step 7: Docs**

In `docs/dev-sandbox.md`, "Starting one", after the paragraph that begins "`dev_read_reference` returns the authoring guide", add:

> The same response carries `site_markdown` — the site-authoring guide this sandbox's seed ships: the page skeleton, the shop pieces, what a write refuses — and `template`, the seed's name (`dev_status` reports it too). Read `site_markdown` before writing under `site/`.

- [ ] **Step 8: Commit**

```bash
git add examples/dev-sandbox crates/impresspress-web/tests/e2e/dev-workspace.spec.ts docs/dev-sandbox.md
git commit -m "dev-sandbox: the blank seed carries its prompt and site guide

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Verification and the PR

- [ ] **Step 1: The Rust suite, clippy, fmt**

```bash
cargo test --locked -p impresspress-core --features block-dev,wasm
cargo clippy --locked -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings
cargo +nightly fmt --check
```
Expected: all pass; no diff.

- [ ] **Step 2: A fresh blank bundle boots with the guide**

```bash
(cd crates/impresspress-web && wasm-pack build --target web --release --out-dir pkg -- --locked)
cargo install --path crates/impresspress --locked --root ./out
examples/dev-sandbox/compiler/fetch-dist.sh
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh --seed blank | tail -1
[ -f examples/dev-sandbox/dist/seed/guide.md ] && echo GUIDE_STAGED
python3 -m http.server 8082 -d examples/dev-sandbox/dist --bind 127.0.0.1 & echo $! > /tmp/dev-http-pid
(cd crates/impresspress-web && npm ci && npx playwright install chromium && TEST_PORT=8082 npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-workspace.spec.ts)
kill "$(cat /tmp/dev-http-pid)"
```
Expected: `GUIDE_STAGED`; `3 passed` — the scenario now asserts `template === 'blank'`, which proves the seed import fetched and verified `/seed/guide.md` in a real service worker.

- [ ] **Step 3: PR**

```bash
git push -u origin HEAD
gh pr create --title "dev: seeds carry a sandbox block — template, prompt and site guide" --body "$(cat <<'EOF'
Plan B of the build-sandboxes design (docs/superpowers/specs/2026-09-30-build-sandboxes-design.md §5.2, §6.1–§6.4, §12).

- `SeedManifest.sandbox` (optional, additive): template name, suggested prompt, `guide.md` verified like every other file, refused as a whole before anything is stored.
- New singleton table `impresspress__dev__seed_info` (migration 003) written by the import.
- `dev_read_reference` gains `template` + `site_markdown`; `dev_status` gains `template`; the `/b/dev` page suggests the seed's prompt (the constant is gone) and names the template.
- The blank seed carries `sandbox.json` + `guide.md`; the generator emits the block; `build.sh` stages the guide.
- Snapshots regenerated: `dev.tools.json` (one description), `dev.openapi.json` (three new optional properties).

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
