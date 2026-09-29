//! `GET /b/dev/api/export` and `GET /b/dev/api/export/manifest` — the sandbox
//! as a folder anyone can serve.
//!
//! Design §10.1. The archive is the running deployment's own static shell,
//! with development mode switched off, plus a `seed/` tree in exactly the
//! format [`super::seed`] reads on a cold boot. That symmetry is the whole
//! design: there is one mechanism for "a fresh instance gets a site", and an
//! export is a bundle that feeds it.
//!
//! ```text
//!     README.md                     how to serve it, what is inside, and the
//!                                   disclosure about `data.json`
//!     index.html sw.js loader.js …  the runtime shell, listed by
//!                                   `/asset-manifest.json`'s `files`
//!     seed/manifest.json            a `SeedManifest`: every file below with
//!                                   its hash, size and served type
//!     seed/site/**                  the site's files
//!     seed/blocks/<name>.wasm       each compiled block
//!     seed/blocks/<name>/**         and its full source, so the export is
//!                                   editable and re-compilable
//!     seed/data.json                the data snapshot
//! ```
//!
//! # One list, two answers
//!
//! [`assemble`] produces the whole entry list with its bytes; [`build`] zips
//! that and [`manifest_preview`] summarizes it. They are not two derivations
//! of "what an export contains" — a preview computed from a second reading of
//! the same stores is a preview that can drift from the archive it claims to
//! describe, and the whole point of the preview is that an agent can trust it
//! without downloading 15 MB. The cost is that the preview reads the shell
//! too; it is a browser-local read of a handful of files the service worker
//! already has, on an explicit call.
//!
//! # Why the shell is rewritten rather than re-rendered
//!
//! The exported site is a plain ImpressPress deployment: no `/b/dev`, no
//! in-browser compiler, no cross-origin-isolation headers on static files.
//! All three follow from one build-time constant in `sw.js`
//! (`const DEV_ENABLED = true;`, `impresspress-bundle`'s `sw.js.tmpl`), which
//! `initialize({ dev: DEV_ENABLED })` and the isolation-header passthrough
//! both read — so turning the sandbox off is one line rewritten, and its
//! absence is an [`ErrorCode::Internal`], never a silent pass-through of a
//! shell that would come up as a second sandbox.

use std::collections::BTreeMap;

use wafer_run::{context::Context, ErrorCode, OutputStream, WaferError};

use super::{
    artifacts, blobs,
    contracts::{ExportFile, ExportManifest},
    data_snapshot, generation, no_store, no_store_db_error_internal, no_store_error_status, repo,
    seed::{self, SeedBlock, SeedManifest},
    workspace,
    zip::ZipWriter,
    DevShared,
};
use crate::{config_vars::APP_NAME_KEY, http::err_internal};

/// The line `sw.js` carries when it was built for a dev deployment, and the
/// one it must carry after an export.
///
/// Stated as a pair rather than as a `replace` call inline so the assertion
/// and the rewrite cannot disagree about the text. `impresspress-bundle`'s
/// `sw_passes_the_dev_flag_to_initialize` pins the producing half.
const SW_DEV_ON: &str = "const DEV_ENABLED = true;";
const SW_DEV_OFF: &str = "const DEV_ENABLED = false;";

/// The one shell file whose content this export edits.
const SW_PATH: &str = "sw.js";

/// Where the data snapshot lands, relative to [`seed::ROOT`].
///
/// `seed::data_url(DATA_PATH)` is the URL the importer fetches and
/// `seed/{DATA_PATH}` is the archive entry — the two are the same string
/// because [`seed`] owns the layout and this writes what it reads.
const DATA_PATH: &str = "data.json";

/// URL prefix the in-browser Rust toolchain's static assets are served under.
///
/// Two things in the exported bundle refer to it and both have to go: the
/// files themselves (excluded from the shell listing by
/// [`SHELL_EXCLUDED_PREFIXES`]) and the service worker's bypass clause for
/// them ([`strip_compiler_bypass`]). Restated here rather than imported from
/// the bundle's `impresspress.toml`, which is a deployment's file and not
/// something this crate can read — `page.rs` and `dev.js` state the same
/// prefix for the same reason.
const COMPILER_ROOT: &str = "/__impresspress_dev/compiler/";

/// Shell paths the export never copies, by prefix.
///
/// Both are things a DEPLOYMENT overlays on top of the bundler's output
/// (`impresspress`'s `apply_overlays`, run after `bundle::run` returns), so
/// neither is in `/asset-manifest.json`'s `files` today. They are excluded
/// explicitly all the same, because "the manifest happens not to list them"
/// is a property of the order two CLI steps run in, and this is a property of
/// what an export MEANS:
///
/// * `seed/` — the exporting deployment's own starter bundle. The archive
///   writes its own `seed/`, and a copied one would either be overwritten or
///   (worse, if it sorted later) overwrite the export's.
/// * `__impresspress_dev/` — the in-browser Rust toolchain: 72 MiB of
///   compiler that only `/b/dev` loads, and the exported site has no `/b/dev`.
const SHELL_EXCLUDED_PREFIXES: &[&str] = &["seed/", "__impresspress_dev/"];

/// The README template, rendered with this export's own numbers.
const README_TEMPLATE: &str = include_str!("templates/export-readme.md");

/// Where the README lands in the archive.
const README_PATH: &str = "README.md";

/// One entry of the archive, with its bytes.
struct Entry {
    path: String,
    bytes: Vec<u8>,
}

/// Everything the README template has a hole for.
///
/// A struct rather than eight positional arguments: they are all numbers and
/// short strings from the same assembly, and a caller that swapped
/// `site_files` for `shell_files` would produce a plausible, wrong README
/// that no type would catch.
struct ReadmeFacts<'a> {
    generation_id: &'a str,
    /// The ACTIVE GENERATION's `created_at`, never the wall clock — see
    /// [`render_readme`].
    created_at: &'a str,
    shell_files: u32,
    site_files: u32,
    blocks: u32,
    tables: &'a BTreeMap<String, usize>,
    source_verdicts: &'a [(String, SourcesMatch)],
}

/// What [`assemble`] produced: the archive's entries, plus the counts the
/// preview reports.
struct Assembled {
    generation_id: String,
    entries: Vec<Entry>,
    shell_files: u32,
    site_files: u32,
    blocks: u32,
    tables: BTreeMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /b/dev/api/export` — the zip.
pub async fn handle_export(ctx: &dyn Context, shared: &DevShared) -> OutputStream {
    let assembled = match assemble(ctx, shared).await {
        Ok(assembled) => assembled,
        Err(refusal) => return refusal.into_response(),
    };
    let short = short_id(&assembled.generation_id);
    let bytes = match archive(assembled) {
        Ok(bytes) => bytes,
        Err(e) => return err_internal("dev export archive", e),
    };
    no_store()
        .set_header(
            "Content-Disposition",
            &format!("attachment; filename=\"impresspress-site-{short}.zip\""),
        )
        // The uncompressed content total is already in the manifest; this is
        // the size of the archive the client is about to read, which is what
        // a progress indicator (and the e2e's download assertion) needs and
        // what `Content-Length` would otherwise be the only source of.
        .set_header("X-Export-Bytes", &bytes.len().to_string())
        .body(bytes, "application/zip")
}

/// `GET /b/dev/api/export/manifest` — what the zip would contain.
pub async fn handle_manifest(ctx: &dyn Context, shared: &DevShared) -> OutputStream {
    match assemble(ctx, shared).await {
        Ok(assembled) => no_store().json(&preview(&assembled)),
        Err(refusal) => refusal.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Building the bundle
// ---------------------------------------------------------------------------

/// The complete archive, as bytes.
///
/// Public because the round-trip test (`tests/dev_export.rs`) exports from
/// one instance and seeds another from the result without going through HTTP,
/// which is the property the format's whole design rests on.
pub async fn build(ctx: &dyn Context, shared: &DevShared) -> Result<Vec<u8>, WaferError> {
    let assembled = assemble(ctx, shared).await.map_err(Refusal::into_error)?;
    archive(assembled)
}

/// What the archive would contain, without producing it.
pub async fn manifest_preview(
    ctx: &dyn Context,
    shared: &DevShared,
) -> Result<ExportManifest, WaferError> {
    let assembled = assemble(ctx, shared).await.map_err(Refusal::into_error)?;
    Ok(preview(&assembled))
}

/// Every entry of the archive, in the order it is written, with its bytes.
async fn assemble(ctx: &dyn Context, shared: &DevShared) -> Result<Assembled, Refusal> {
    // The export is a snapshot of what is LIVE, not of the workspace: a block
    // whose source has been edited since it was compiled exports the compiled
    // one, because that is what the exported folder will run. The workspace
    // supplies only the sources that go alongside it.
    let Some((row, manifest)) = generation::active(ctx).await.map_err(Refusal::Internal)? else {
        return Err(Refusal::NothingPublished);
    };
    // The MANIFEST read is locked; the content reads below are not, and the
    // split is the whole of this module's concurrency position (see
    // [`content_gone`]). `workspace::load` snapshots `workspace.json` and then
    // reads its bytes, so a save landing between those two steps invalidates
    // the snapshot and the browser storage layer reports that as an internal
    // error — `super::files`' failure mode 1, and a `500` on an export that
    // asked for nothing unusual. The section is one small JSON read, which is
    // not what the "do not hold this across an export" argument is about.
    let ws = {
        let _serialized = shared.workspace.lock().await;
        workspace::load(ctx).await.map_err(Refusal::Internal)?
    };

    // --- the data snapshot -----------------------------------------------
    //
    // First, before the shell and before any stored content: it is the one
    // part of the bundle with a size limit, and an export that is going to be
    // refused should cost one snapshot, not the runtime's wasm and the whole
    // site as well.
    //
    // Compact, not pretty-printed: the file's reader is the importer, and
    // indentation repeated on every row would spend a sizeable share of
    // [`seed::MAX_DATA_BYTES`] on whitespace.
    let snapshot = data_snapshot::export(ctx)
        .await
        .map_err(Refusal::Internal)?;
    let data_bytes = serde_json::to_vec(&snapshot)
        .map_err(|e| Refusal::Internal(encoding_error("the data snapshot", e)))?;
    // The importer's bound, applied here so an export is never a bundle its
    // own importer refuses.
    if data_bytes.len() > seed::MAX_DATA_BYTES {
        return Err(Refusal::DataTooLarge {
            bytes: data_bytes.len(),
        });
    }

    // --- the shell -------------------------------------------------------
    let listed = shared.shell.list().await.map_err(Refusal::Shell)?;
    let mut shell: Vec<Entry> = Vec::new();
    for path in listed {
        if SHELL_EXCLUDED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
        {
            continue;
        }
        let bytes = shared
            .shell
            .fetch(&path)
            .await
            .map_err(|e| Refusal::Shell(format!("{path}: {e}")))?;
        let bytes = if path == SW_PATH {
            strip_compiler_bypass(sw_with_dev_off(&bytes)?)
        } else {
            bytes
        };
        shell.push(Entry { path, bytes });
    }
    if shell.is_empty() {
        return Err(Refusal::Shell(
            "/asset-manifest.json listed no files, so there is no runtime to export".to_string(),
        ));
    }

    // --- the seed --------------------------------------------------------
    let tables: BTreeMap<String, usize> = snapshot
        .tables
        .iter()
        .map(|(table, rows)| (table.clone(), rows.len()))
        .collect();

    let mut seed_entries: Vec<Entry> = Vec::new();
    let mut site: Vec<seed::SeedFile> = Vec::new();
    for entry in &manifest.site.files {
        let bytes = blobs::get(ctx, &entry.sha256).await.map_err(content_gone)?;
        seed_entries.push(Entry {
            path: format!("seed/site/{}", entry.path),
            bytes,
        });
        site.push(entry.clone());
    }

    let mut blocks: Vec<SeedBlock> = Vec::new();
    let mut source_verdicts: Vec<(String, SourcesMatch)> = Vec::new();
    for spec in &manifest.blocks {
        let name = seed::short_name(&spec.name);
        let artifact = artifacts::get(ctx, &spec.artifact_sha256)
            .await
            .map_err(content_gone)?;
        seed_entries.push(Entry {
            path: format!("seed/blocks/{name}.wasm"),
            bytes: artifact,
        });
        // The source tree lives in the workspace and in no generation at all,
        // so this reads what is there NOW. A block whose sources were deleted
        // after it was compiled exports as a `.wasm` with no `src/` — the
        // honest answer, and the manifest says so by carrying no source
        // entries for it rather than by failing the export.
        let sources = workspace::block_sources(&ws, name);
        for source in &sources {
            let bytes = blobs::get(ctx, &source.sha256)
                .await
                .map_err(content_gone)?;
            seed_entries.push(Entry {
                path: format!("seed/blocks/{name}/{}", source.path),
                bytes,
            });
        }
        // …which is exactly why the README says, per block, whether the
        // sources it ships are the ones the artifact was built from. The two
        // halves come from different places on purpose (the artifact from the
        // generation, the sources from the live workspace), so they CAN
        // disagree — an agent that edited `blocks/hello/src/lib.rs` and did
        // not recompile leaves an export whose `.wasm` and `src/` describe
        // different programs. Silent is the one thing that must not be.
        let recorded = repo::builds::latest_valid_for_artifact(ctx, &spec.artifact_sha256)
            .await
            .map_err(Refusal::Internal)?
            .map(|build| build.source_manifest_sha256);
        source_verdicts.push((
            spec.name.clone(),
            sources_match(&sources, recorded.as_deref()),
        ));
        blocks.push(SeedBlock {
            spec: spec.clone(),
            sources,
        });
    }

    let seed_manifest = SeedManifest {
        schema_version: seed::SCHEMA_VERSION,
        source_generation: Some(manifest.generation_id.clone()),
        site,
        blocks,
        data: Some(seed::SeedFile {
            path: DATA_PATH.to_string(),
            sha256: blobs::sha256_hex(&data_bytes),
            size: data_bytes.len() as u64,
            content_type: seed::DATA_CONTENT_TYPE.to_string(),
        }),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&seed_manifest)
        .map_err(|e| Refusal::Internal(encoding_error("the seed manifest", e)))?;

    // --- the whole archive, in order -------------------------------------
    //
    // README first (it is what a human opening the folder sees), then the
    // shell, then `seed/manifest.json` ahead of the files it describes, then
    // those files, then the data snapshot. Fixed order plus `ZipWriter`'s
    // fixed timestamps means two exports of the same generation are
    // byte-identical archives.
    let shell_files = shell.len() as u32;
    let site_files = manifest.site.files.len() as u32;
    let block_count = manifest.blocks.len() as u32;
    let readme = render_readme(
        ctx,
        &ReadmeFacts {
            generation_id: &manifest.generation_id,
            created_at: &row.created_at,
            shell_files,
            site_files,
            blocks: block_count,
            tables: &tables,
            source_verdicts: &source_verdicts,
        },
    )
    .await
    .map_err(Refusal::Internal)?;

    let mut entries = Vec::with_capacity(shell.len() + seed_entries.len() + 3);
    entries.push(Entry {
        path: README_PATH.to_string(),
        bytes: readme.into_bytes(),
    });
    entries.extend(shell);
    entries.push(Entry {
        path: format!("{}manifest.json", archive_seed_prefix()),
        bytes: manifest_bytes,
    });
    entries.extend(seed_entries);
    entries.push(Entry {
        path: format!("{}{DATA_PATH}", archive_seed_prefix()),
        bytes: data_bytes,
    });

    Ok(Assembled {
        generation_id: manifest.generation_id,
        entries,
        shell_files,
        site_files,
        blocks: block_count,
        tables,
    })
}

/// The archive's `seed/` prefix, derived from the URL prefix the importer
/// fetches ([`seed::ROOT`] is `/seed/`) rather than restated — the archive
/// entry and the URL are the same path with and without its leading `/`, and
/// spelling that twice is how they come apart.
fn archive_seed_prefix() -> &'static str {
    seed::ROOT.trim_start_matches('/')
}

/// Zip what [`assemble`] produced.
fn archive(assembled: Assembled) -> Result<Vec<u8>, WaferError> {
    let mut zip = ZipWriter::new();
    for entry in &assembled.entries {
        zip.add(&entry.path, &entry.bytes).map_err(|e| {
            WaferError::new(
                ErrorCode::Internal,
                format!("the export bundle could not be written: {e}"),
            )
        })?;
    }
    Ok(zip.finish())
}

/// Summarize what [`assemble`] produced.
fn preview(assembled: &Assembled) -> ExportManifest {
    ExportManifest {
        generation_id: assembled.generation_id.clone(),
        files: assembled
            .entries
            .iter()
            .map(|entry| ExportFile {
                path: entry.path.clone(),
                bytes: entry.bytes.len() as u64,
            })
            .collect(),
        total_bytes: assembled
            .entries
            .iter()
            .map(|entry| entry.bytes.len() as u64)
            .sum(),
        shell_files: assembled.shell_files,
        site_files: assembled.site_files,
        blocks: assembled.blocks,
        tables: assembled.tables.clone(),
    }
}

/// `sw.js` with the sandbox turned off.
///
/// A missing marker is [`ErrorCode::Internal`], never a pass-through: this
/// runs on a shell the deployment built for itself, so its absence means the
/// bundler and this function disagree about the constant — and the failure
/// mode of guessing is an exported folder that comes up as a second
/// development sandbox, with `/b/dev` and an in-browser compiler on a site
/// its owner meant to hand to someone else.
fn sw_with_dev_off(bytes: &[u8]) -> Result<Vec<u8>, Refusal> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        Refusal::Internal(WaferError::new(
            ErrorCode::Internal,
            "the deployment's sw.js is not valid UTF-8, so its dev flag cannot be turned off",
        ))
    })?;
    // EXACTLY one occurrence, not "at least one". Two would mean the marker
    // is ambiguous — a comment quoting the declaration, say — and a blanket
    // `replace` would edit both, leaving a file whose prose contradicts its
    // code and, worse, no longer telling this function which line it is
    // meant to own. `sw.js.tmpl` is written so the declaration is the only
    // occurrence; this is what keeps that true.
    let occurrences = text.matches(SW_DEV_ON).count();
    if occurrences != 1 {
        return Err(Refusal::Internal(WaferError::new(
            ErrorCode::Internal,
            format!(
                "the deployment's sw.js contains {occurrences} occurrences of {SW_DEV_ON:?} \
                 and the export needs exactly one, so it cannot turn development mode off; \
                 the bundle was built by a different impresspress-bundle than this runtime \
                 expects"
            ),
        )));
    }
    Ok(text.replace(SW_DEV_ON, SW_DEV_OFF).into_bytes())
}

/// `sw.js` with the compiler's bypass clause removed, if it has one.
///
/// A deployment that ships the in-browser toolchain adds
/// `/__impresspress_dev/compiler/` to the service worker's bypass list (the
/// bundle's `extra_bypass_prefix`, rendered by `impresspress-bundle`'s
/// `build_template_vars` as one ` || url.pathname.startsWith('…')` clause).
/// The export does not copy those assets — there is no `/b/dev` in an
/// exported site to load them — so the clause would be a bypass for a tree
/// that is not there: every request under the prefix waved past the runtime
/// to a 404 from the static host, instead of the runtime's own answer.
///
/// Unlike the dev flag this is OPTIONAL and its absence is not an error: the
/// prefix is an app's own bypass entry (CI's foundations bundle ships no
/// compiler and never adds it), so a shell without the clause is an ordinary
/// shell, not a mismatched one. Removing the whole rendered clause rather
/// than editing the prefix keeps the remaining expression exactly as the
/// bundler would have rendered it for a bundle that never asked.
fn strip_compiler_bypass(bytes: Vec<u8>) -> Vec<u8> {
    // The exact text `build_template_vars` emits per `extra_bypass_prefix`
    // entry. Built here rather than matched loosely so a clause this does not
    // recognise is left alone instead of half-edited.
    let clause = format!(" || url.pathname.startsWith('{COMPILER_ROOT}')");
    match String::from_utf8(bytes) {
        Ok(text) if text.contains(&clause) => text.replace(&clause, "").into_bytes(),
        Ok(text) => text.into_bytes(),
        // Unreachable in practice — `sw_with_dev_off` has already parsed the
        // same bytes as UTF-8 and would have refused otherwise. Returning
        // them untouched rather than panicking keeps this function total.
        Err(e) => e.into_bytes(),
    }
}

/// The first eight characters of a generation id — what the downloaded file
/// is named after.
fn short_id(generation_id: &str) -> String {
    generation_id.chars().take(8).collect()
}

/// The README, with this export's own numbers substituted in.
///
/// `created_at` is the ACTIVE GENERATION's timestamp, not the wall clock. An
/// export is a function of what is live, and `ZipWriter` already fixes every
/// entry's timestamp for the same reason — dating the README by when the
/// download happened would have made the one entry that changes between two
/// otherwise identical exports the README, which is both useless and the
/// exact thing `two_exports_of_the_same_generation_are_identical` exists to
/// deny. The generation's own creation time is also the more useful fact: it
/// is when the site being exported came to be.
async fn render_readme(ctx: &dyn Context, facts: &ReadmeFacts<'_>) -> Result<String, WaferError> {
    use wafer_core::clients::config;

    // Through the config client, not `ctx.config_get`: that snapshot is
    // frozen at boot, so an export made after an admin renamed the site still
    // carried the old name, and on Cloudflare carried the default whatever
    // the name was. The literal is spelled as every other reader spells it
    // (`ui::SiteConfig`, `pipeline`): `config_vars` declares it in
    // `shared_config_vars()` without exporting a constant for the key.
    let title = config::get_default(ctx, APP_NAME_KEY, "").await?;
    let title = if title.is_empty() {
        "Your ImpressPress site".to_string()
    } else {
        title
    };
    let admin_email = config::get_default(
        ctx,
        crate::blocks::auth::config::BOOTSTRAP_ADMIN_EMAIL_KEY,
        "",
    )
    .await?;
    let admin_email = if admin_email.is_empty() {
        "the account you signed in with".to_string()
    } else {
        admin_email
    };
    let rows: usize = facts.tables.values().sum();
    // A plain textual substitution, not a template engine: every value is a
    // number or a short string this function produced, and
    // `export_zip_contains_shell_seed_sources_and_data_with_dev_off` asserts
    // no `{{` survives.
    Ok(README_TEMPLATE
        .replace("{{TITLE}}", &title)
        .replace("{{DATE}}", facts.created_at)
        .replace("{{GENERATION_ID}}", facts.generation_id)
        .replace("{{SHELL_FILES}}", &facts.shell_files.to_string())
        .replace("{{SITE_FILES}}", &facts.site_files.to_string())
        .replace("{{BLOCKS}}", &facts.blocks.to_string())
        .replace("{{TABLE_ROWS}}", &rows.to_string())
        .replace("{{ADMIN_EMAIL}}", &admin_email)
        .replace(
            "{{BLOCK_SOURCES}}",
            &render_source_verdicts(facts.source_verdicts),
        ))
}

/// Whether the sources an export ships for one block are the ones its
/// artifact was compiled from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourcesMatch {
    /// The workspace's current source digest equals the one recorded on the
    /// build row that produced this artifact.
    Current,
    /// They differ: the sources have been edited since the block was last
    /// compiled, so the `.wasm` in this bundle is not built from the `src/`
    /// beside it. Re-compile before exporting to make them agree.
    Stale,
    /// There is no digest to compare against. Either the block was SEEDED
    /// (a seeded build row records no source digest — the bundle it came from
    /// carried the sources, and no compile ran here), or its build row is
    /// gone. Not a verdict either way, and reported as such rather than
    /// guessed at.
    Unknown,
}

impl SourcesMatch {
    /// The README line for this verdict.
    fn describe(self) -> &'static str {
        match self {
            Self::Current => "sources match the compiled artifact",
            Self::Stale => {
                "SOURCES DIFFER from the compiled artifact — the .wasm here was built from an \
                 earlier version of src/; recompile and re-export to make them agree"
            }
            Self::Unknown => {
                "no source digest recorded (a seeded block, or one whose build record is gone) \
                 — the sources are shipped as found and were not checked against the artifact"
            }
        }
    }
}

/// The digest a compile records for a block's sources, recomputed from what
/// the workspace holds now.
///
/// One `"<crate-relative path>\0<sha256>\n"` line per file, sorted, hashed —
/// byte for byte what `dev.js`'s `snapshotBlock` computes and sends as
/// `source_manifest_sha256`. The two definitions have to agree or every
/// comparison below reads "stale"; NUL is the separator on both sides because
/// a path may contain anything but that, and the paths are crate-relative on
/// both sides because that is what the compiler was given.
fn source_digest(sources: &[workspace::FileEntry]) -> String {
    let mut lines: Vec<String> = sources
        .iter()
        .map(|entry| format!("{}\0{}\n", entry.path, entry.sha256))
        .collect();
    lines.sort();
    blobs::sha256_hex(lines.concat().as_bytes())
}

/// Compare the workspace's current sources against the digest the build row
/// recorded, if there is one.
fn sources_match(sources: &[workspace::FileEntry], recorded: Option<&str>) -> SourcesMatch {
    // A seeded build row records the empty string, and so does a stage
    // request that reported no digest — neither is something to compare
    // against, and treating "" as a digest would call every seeded block
    // stale.
    match recorded.filter(|digest| !digest.is_empty()) {
        None => SourcesMatch::Unknown,
        Some(recorded) if recorded == source_digest(sources) => SourcesMatch::Current,
        Some(_) => SourcesMatch::Stale,
    }
}

/// The README's per-block source verdicts, one line each.
fn render_source_verdicts(verdicts: &[(String, SourcesMatch)]) -> String {
    if verdicts.is_empty() {
        return "    (this site has no backend blocks)".to_string();
    }
    verdicts
        .iter()
        .map(|(name, verdict)| format!("    {name}: {}", verdict.describe()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `serde_json` failure encoding one of the bundle's own JSON files.
fn encoding_error(what: &str, error: serde_json::Error) -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        format!("{what} could not be encoded: {error}"),
    )
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// What [`Refusal::WorkspaceChanged`] says, on both surfaces.
///
/// One string: the HTTP refusal and the `WaferError` the non-HTTP callers
/// ([`build`], [`manifest_preview`]) get are the same answer to the same
/// question, and an agent that retries on one wording should retry on the
/// other.
const WORKSPACE_CHANGED: &str = "the workspace changed while the export was being built; try again";

/// Why an export could not be produced.
///
/// Three shapes, because they reach the caller three different ways: a
/// precondition the agent can act on (publish something first, trim the
/// data), a host-side failure the agent cannot (the shell would not read),
/// and everything the storage or ledger refused, which is already a
/// [`WaferError`].
enum Refusal {
    /// Nothing has been published, so there is no site to export.
    NothingPublished,
    /// The data snapshot serializes to more than [`seed::MAX_DATA_BYTES`],
    /// so the bundle would be refused by the importer it exists to feed.
    DataTooLarge {
        /// How large `data.json` would have been.
        bytes: usize,
    },
    /// A blob or artifact the manifest names is no longer in the store: the
    /// workspace was edited (and collected) while this export was being
    /// assembled. See [`content_gone`].
    WorkspaceChanged,
    /// The static shell could not be listed or read.
    Shell(String),
    /// A storage, ledger or encoding failure.
    Internal(WaferError),
}

/// What [`Refusal::DataTooLarge`] says, on both surfaces — one wording for
/// the same reason [`WORKSPACE_CHANGED`] is one string.
fn data_too_large(bytes: usize) -> String {
    format!(
        "the data snapshot (seed/data.json) would be {bytes} bytes, and a bundle may carry at \
         most {} — the limit the importer enforces — so this export could not be imported. \
         Remove rows the snapshot carries (products, offers, config variables, accounts) and \
         export again.",
        seed::MAX_DATA_BYTES
    )
}

/// A content read that came back [`ErrorCode::NotFound`] is the export losing
/// a race, not an internal fault.
///
/// [`assemble`] reads the manifest under `DevShared::workspace` and then reads
/// each blob **outside** it — deliberately, because a 10 MB read under that
/// mutex would block editing for the length of an export. This is the one
/// place in the block where a read of stored content is not covered by the
/// lock: `files::handle_read` holds it across its blob fetch precisely so an
/// entry and its content are one view, and it can afford to because one file
/// is capped at `paths::MAX_FILE_BYTES`. An export is not capped at anything.
///
/// What the split admits is a `blocks/`-source delete landing between the
/// manifest and the blob: `files::handle_delete` collects after a `blocks/`
/// delete (nothing was published, so no activation will), and the blob this
/// loop is about to read can be freed underneath it. That is the *same* race
/// `files::handle_read` closes with the lock, answered the other way — as a
/// retry-able `409` rather than as a sanitized `500` — because the cost of
/// closing it here is unbounded. `super::files`' header names both decisions
/// so a maintainer finds them together.
///
/// The site half cannot lose this race — the active generation is always
/// retained and a compile finishing mid-export leaves the old generation
/// `Superseded` but inside the retention window — so the archive is still a
/// consistent snapshot of one generation whenever it is produced at all. This
/// only names the case where it cannot be produced, so the caller is told to
/// try again rather than handed a 500 that reads like a bug in the exporter.
fn content_gone(error: WaferError) -> Refusal {
    if error.code == ErrorCode::NotFound {
        Refusal::WorkspaceChanged
    } else {
        Refusal::Internal(error)
    }
}

impl Refusal {
    /// The refusal as an already-sealed response.
    fn into_response(self) -> OutputStream {
        match self {
            // `FailedPrecondition` names the condition exactly — nothing has
            // been published here yet — while the status is 400 rather than
            // the code's default 412, which HTTP reserves for the conditional
            // request headers no caller of this endpoint sends.
            Self::NothingPublished => no_store_error_status(
                ErrorCode::FailedPrecondition,
                400,
                "there is nothing to export yet: no generation is active. Write a site file or \
                 compile a block first.",
            ),
            // 409, and `Aborted` — "often due to a concurrency conflict" — is
            // exactly what this is. Retrying is the whole remedy: the export
            // is a function of what is live, and what is live is consistent
            // again the moment the delete that raced it has finished.
            Self::WorkspaceChanged => {
                no_store_error_status(ErrorCode::Aborted, 409, WORKSPACE_CHANGED)
            }
            // 413 and `ResourceExhausted`, as every other over-a-limit refusal
            // in `/b/dev` is (`super::files`' header).
            Self::DataTooLarge { bytes } => {
                no_store_error_status(ErrorCode::ResourceExhausted, 413, &data_too_large(bytes))
            }
            Self::Shell(message) => err_internal("dev export shell", message),
            Self::Internal(error) => no_store_db_error_internal(error, "dev export"),
        }
    }

    /// The refusal as a [`WaferError`], for the non-HTTP callers
    /// ([`build`], [`manifest_preview`]).
    fn into_error(self) -> WaferError {
        match self {
            Self::NothingPublished => WaferError::new(
                ErrorCode::FailedPrecondition,
                "there is nothing to export yet: no generation is active",
            ),
            Self::WorkspaceChanged => WaferError::new(ErrorCode::Aborted, WORKSPACE_CHANGED),
            Self::DataTooLarge { bytes } => {
                WaferError::new(ErrorCode::ResourceExhausted, data_too_large(bytes))
            }
            Self::Shell(message) => WaferError::new(
                ErrorCode::Internal,
                format!("the static shell could not be read: {message}"),
            ),
            Self::Internal(error) => error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_prefix_is_the_url_prefix_without_its_slash() {
        assert_eq!(archive_seed_prefix(), "seed/");
        assert_eq!(format!("/{}", archive_seed_prefix()), seed::ROOT);
    }

    #[test]
    fn the_download_is_named_after_the_first_eight_characters() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
        assert_eq!(short_id("short"), "short");
        assert_eq!(short_id(""), "");
    }

    #[test]
    fn turning_the_dev_flag_off_rewrites_exactly_one_line() {
        let sw = b"const DEV_ENABLED = true;\nif (DEV_ENABLED && x) {}\n";
        let Ok(out) = sw_with_dev_off(sw) else {
            panic!("a dev shell must rewrite");
        };
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains("const DEV_ENABLED = false;"), "{text}");
        // The passthrough branch is untouched: it reads the constant, so
        // flipping the declaration flips it too. That is the whole reason
        // the bundler renders one constant instead of two literals.
        assert!(text.contains("if (DEV_ENABLED && x) {}"), "{text}");
        assert!(!text.contains("= true;"), "{text}");
    }

    /// A second occurrence — a comment quoting the declaration, most likely
    /// — makes the marker ambiguous, and a blanket replace would edit both.
    #[test]
    fn a_shell_with_two_markers_is_refused() {
        let sw = b"// flip const DEV_ENABLED = true; to false\nconst DEV_ENABLED = true;\n";
        let Err(refusal) = sw_with_dev_off(sw) else {
            panic!("an ambiguous marker must be refused");
        };
        assert!(
            refusal.into_error().message.contains("2 occurrences"),
            "the refusal must say how many it found"
        );
    }

    /// The README's disclosure about `data.json` is the archive's only
    /// statement about the one secret it deliberately carries, so it has to
    /// name the hash that is actually in there. Every export is produced by
    /// the browser sandbox, whose `CryptoService` is
    /// `impresspress_browser::crypto::BrowserCryptoService` — PBKDF2-HMAC-SHA256,
    /// because Argon2id is too slow in wasm. This crate cannot depend on that
    /// one (the dependency runs the other way), so what is pinned here is the
    /// property that goes wrong on its own: the template may mention Argon2
    /// only to say it is NOT what ran.
    #[test]
    fn the_readme_names_the_hash_the_browser_actually_writes() {
        assert!(
            README_TEMPLATE.contains("PBKDF2-HMAC-SHA256"),
            "the disclosure must name the hash the sandbox writes"
        );
        for (index, _) in README_TEMPLATE.match_indices("Argon2") {
            assert!(
                README_TEMPLATE[index..].starts_with("Argon2id is too slow"),
                "the README may name Argon2 only to say the sandbox does not use it"
            );
        }
    }

    /// The starter password is printed in `docs/dev-sandbox.md` and on the
    /// sandbox's own welcome page, so an export made with it still set ships a
    /// working admin login whose password is public knowledge. The README has
    /// to say so, and say where to fix it.
    #[test]
    fn the_readme_says_to_change_the_public_starter_password() {
        assert!(
            README_TEMPLATE.contains("Change the admin password"),
            "the README must tell its holder to change the admin password"
        );
        assert!(
            README_TEMPLATE.contains("/b/auth/change-password"),
            "…and where: the auth block's own change-password page"
        );
    }

    /// The one thing this must never do is pass a shell through unchanged.
    #[test]
    fn a_shell_without_the_marker_is_refused() {
        let Err(refusal) = sw_with_dev_off(b"await initialize({ dev: true });") else {
            panic!("a shell with no marker must be refused");
        };
        let error = refusal.into_error();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(error.message.contains("DEV_ENABLED"), "{}", error.message);
    }
}
