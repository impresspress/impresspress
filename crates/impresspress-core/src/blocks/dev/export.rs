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
//!     seed/wafer_guest/**           the guest crate every block depends on
//!                                   by path, present iff there is a block;
//!                                   not in `seed/manifest.json`
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
//! `initialize({ dev: DEV_ENABLED, … })` and the isolation-header passthrough
//! both read — so turning the sandbox off is one line rewritten, and its
//! absence is an [`ErrorCode::Internal`], never a silent pass-through of a
//! shell that would come up as a second sandbox.
//!
//! The boot page is the second edit: a deployment's boot notice (the sandbox
//! says what it is and where its `llms.txt` is) describes the deployment, not
//! the site, and comes out.
//!
//! Its title is the third: the page shows the DEPLOYMENT's title ("…dev
//! sandbox") in `<title>` and twice in the body, and the exported site has a
//! name of its own — the one the README is headed with. See
//! [`index_for_export`].
//!
//! # `llms.txt`
//!
//! Two different files have that name, and the export treats them
//! differently by construction rather than by a rule about the name.
//!
//! The SANDBOX's — what the sandbox tells a reader about itself — is not
//! exported, because nothing the export reads holds it. The static host's
//! copy is a deployment overlay, laid down after the bundler listed the
//! shell, so it is not in `/asset-manifest.json`'s `files` (the CLI's
//! `flow_sealed_web` test pins that for the sandbox's own configuration).
//! The runtime's copy is published by `super::publisher` without being in
//! any generation's manifest, and `seed/site/**` is that manifest. A shell
//! file the listing DOES name is the deployment's own bundled file and is
//! copied like any other.
//!
//! The SITE's — `site/llms.txt`, if the site has one — is exported twice,
//! from one read of one blob: as `seed/site/llms.txt`, which the exported
//! runtime imports and serves once its worker controls the page, and as
//! `llms.txt` at the archive's root, which the static host serves to a
//! reader that runs no JavaScript and so never gets a worker. `/llms.txt`
//! is not a path the worker leaves to the static host (the
//! shadowed-site-file refusal below is about exactly those paths, and a
//! site file at one of them is refused), so the root copy shadows nothing:
//! it answers only where the runtime cannot. The two cannot say different
//! things in the archive — they are one read written in the same assembly —
//! and the exported instance has no workspace to edit the site with
//! afterwards.
//!
//! They are not always the same BYTES. The root copy, like the README, is
//! read through the static host, which may serve it with no charset
//! (Cloudflare's asset server and `python3 -m http.server` do), and a
//! browser decodes a charset-less `text/plain` in a legacy encoding. A root
//! text file that is UTF-8 and not ASCII therefore starts with a byte order
//! mark ([`for_a_charsetless_host`]), which a browser honours ahead of the
//! Content-Type (the WHATWG encoding standard's BOM sniff). The seed copy is
//! the site's file byte for byte: the importer verifies its hash, and the
//! exported runtime serves it with `charset=utf-8`.

use std::collections::BTreeMap;

use wafer_run::{context::Context, ErrorCode, OutputStream, WaferError};

use super::{
    artifacts, blobs,
    bypass::BypassRules,
    contracts::{ExportFile, ExportManifest},
    data_snapshot, generation, no_store, no_store_db_error_internal, no_store_error_status, repo,
    scaffold,
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

/// The shell's service worker: development mode off, the compiler's bypass
/// gone.
const SW_PATH: &str = "sw.js";

/// The shell's boot page, the other shell file this export edits: the
/// deployment's boot notice comes out and its title becomes the exported
/// site's ([`index_for_export`]).
const INDEX_PATH: &str = "index.html";

/// The two comments the bundler renders a deployment's boot notice between
/// (`impresspress-bundle`'s `BOOT_NOTICE_START` / `BOOT_NOTICE_END`,
/// `index.html.tmpl`). Restated for the reason [`COMPILER_ROOT`] is: the
/// bundler is not a dependency of this crate.
/// `crates/impresspress/tests/seed_bypass_prefix.rs` compares the spellings.
pub const BOOT_NOTICE_START: &str = "<!--boot-notice-->";
pub const BOOT_NOTICE_END: &str = "<!--/boot-notice-->";

/// What the bundler wraps each showing of the deployment's title in, in the
/// boot page's body (`impresspress-bundle`'s `APP_TITLE_OPEN` /
/// `APP_TITLE_CLOSE`). Restated and compared like the notice markers.
pub const APP_TITLE_OPEN: &str = "<span data-app-title>";
pub const APP_TITLE_CLOSE: &str = "</span>";

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
/// [`SHELL_EXCLUDED_PREFIXES`]) and the service worker's bypass for them
/// ([`sw_without_compiler`]). Restated here rather than imported from
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

/// Whether the export leaves a listed shell file behind.
fn shell_excluded(path: &str) -> bool {
    SHELL_EXCLUDED_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// The README template, rendered with this export's own numbers.
const README_TEMPLATE: &str = include_str!("templates/export-readme.md");

/// Where the README lands in the archive.
const README_PATH: &str = "README.md";

/// UTF-8's byte order mark: U+FEFF, encoded.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// A text file the archive writes at its root, as a static host must be
/// handed it for a browser to read it as UTF-8.
///
/// The static host serves the root, and it types a file by its own table:
/// Cloudflare's asset server (checked with curl on `dev.impresspress.org`
/// and `build-bootstrap.impresspress.org`) and `python3 -m http.server`
/// both send `.txt` as `text/plain` with no charset, and the latter sends
/// `.md` as `text/markdown` the same way. A browser decodes such a document
/// in a legacy encoding (windows-1252 in Chromium), so `—` shows as `â€”`.
/// What overrides that on every host, with no header config the bundle
/// cannot carry, is a byte order mark: the WHATWG encoding sniff honours a
/// BOM ahead of any Content-Type, and `fetch`'s `text()` strips one.
///
/// Only for a file that needs it: one that is UTF-8 and not ASCII. ASCII
/// decodes the same in every encoding a host could imply, so it is left as
/// it is. Bytes that are not UTF-8 are left as they are too, because a
/// UTF-8 BOM would claim an encoding they are not in. A file that already
/// starts with a BOM keeps the one it has.
///
/// The cost falls on a reader that decodes UTF-8 but does not strip a BOM:
/// httpx, Go, Rust's `String::from_utf8`, Python's `.decode("utf-8")`
/// without `utf-8-sig`. That reader gets a leading U+FEFF, so both files'
/// first line reads `\u{FEFF}# …` and a strict `^# ` match, or a Markdown
/// parser that does not strip the mark, misses the H1. A host that can set
/// headers should still serve these files as `text/plain; charset=utf-8`
/// (or `text/markdown; charset=utf-8`): a browser then decodes them as
/// UTF-8 either way, and the README says so to whoever deploys the folder.
fn for_a_charsetless_host(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.is_ascii() || bytes.starts_with(UTF8_BOM) || std::str::from_utf8(&bytes).is_err() {
        return bytes;
    }
    [UTF8_BOM, &bytes].concat()
}

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
    /// The exported site's name ([`exported_title`]).
    title: &'a str,
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

    // A site file the EXPORTED worker would shadow, before anything is read.
    // The exported runtime's seed import refuses a whole bundle over one such
    // file (`seed::import`), so an export carrying it would be a folder that
    // boots with no site at all. The sandbox refuses the write itself, so
    // this only fires for a file that predates the check — written under an
    // older `sw.js` that handed over no rules.
    let exported_rules = without_compiler(shared.bypass.clone());
    for entry in &manifest.site.files {
        let workspace_path = format!("{}{}", workspace::SITE_PREFIX, entry.path);
        exported_rules
            .refuse_shadowed(&workspace_path)
            .map_err(Refusal::ShadowedSiteFile)?;
    }
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

    // The exported site's name: the README's heading and the boot page's
    // title are the same string, read once.
    let title = exported_title(ctx).await.map_err(Refusal::Internal)?;

    // --- the shell -------------------------------------------------------
    let listed = shared.shell.list().await.map_err(Refusal::Shell)?;
    let mut shell: Vec<Entry> = Vec::new();
    for path in listed {
        if shell_excluded(&path) {
            continue;
        }
        let bytes = shared
            .shell
            .fetch(&path)
            .await
            .map_err(|e| Refusal::Shell(format!("{path}: {e}")))?;
        let bytes = if path == SW_PATH {
            sw_without_compiler(sw_with_dev_off(&bytes)?)?
        } else if path == INDEX_PATH {
            index_for_export(bytes, &title)?
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
        // The site's own `llms.txt` also goes to the root, for the static
        // host to serve where there is no worker — the entry below's bytes,
        // from this one read, with a byte order mark if the static host
        // needs one to be read as UTF-8 (see the module docs). It takes
        // the place of a shell file of that name: once the worker runs, the
        // site's is what `/llms.txt` answers, and the static host must not
        // say something else to a reader without one.
        if entry.path == seed::LLMS_PATH {
            shell.retain(|shell_entry| shell_entry.path != seed::LLMS_PATH);
            shell.push(Entry {
                path: seed::LLMS_PATH.to_string(),
                bytes: for_a_charsetless_host(bytes.clone()),
            });
        }
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
    // The crate every block depends on, once, beside the blocks. It is not a
    // seed entry: `SeedManifest` lists what `seed::import` installs — site
    // files, blocks and the data snapshot — and the crate is none of those,
    // only an input to rebuilding the blocks' sources. So it is an archive
    // entry the manifest above never lists. From `seed/blocks/<name>/` the
    // template's `path = "../../wafer_guest"` resolves here, which is what
    // makes an exported block buildable on a host toolchain.
    if !manifest.blocks.is_empty() {
        for (path, content) in scaffold::guest_files() {
            seed_entries.push(Entry {
                path: format!("{}wafer_guest/{path}", archive_seed_prefix()),
                bytes: content.into_bytes(),
            });
        }
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
        // An export boots with the workspace off — no `/b/dev`, no reference
        // tool — so a guide would describe tools the bundle does not have.
        sandbox: None,
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
            title: &title,
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
        bytes: for_a_charsetless_host(readme.into_bytes()),
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

/// The boot page as the exported site's: the deployment's boot notice taken
/// out, and the deployment's title replaced by the site's name.
///
/// Both are things the bundler rendered from the DEPLOYMENT's configuration
/// (`[app] boot_notice`, `[app] title`), and both are found by the exact
/// text the bundler renders them in — never by guessing at the sandbox's
/// wording, which this crate does not know.
fn index_for_export(bytes: Vec<u8>, title: &str) -> Result<Vec<u8>, Refusal> {
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Err(index_malformed("is not valid UTF-8"));
    };
    let text = index_without_boot_notice(text)?;
    let text = index_with_title(&text, title)?;
    Ok(text.into_bytes())
}

/// The refusal for a boot page the export cannot edit safely.
fn index_malformed(what: &str) -> Refusal {
    Refusal::Internal(WaferError::new(
        ErrorCode::Internal,
        format!(
            "the deployment's index.html {what}, so the export cannot make it the exported \
             site's boot page; the bundle was built by a different impresspress-bundle than \
             this runtime expects"
        ),
    ))
}

/// The page with its boot notice region emptied.
///
/// The sandbox's boot page says what the sandbox is and sends an agent to
/// `/llms.txt` and `/b/dev/enter` — none of which the exported site has. The
/// bundler renders that text between two comments
/// ([`BOOT_NOTICE_START`], [`BOOT_NOTICE_END`]) and renders the comments with
/// nothing between them for an app that has no notice, so emptying the region
/// leaves exactly what a plain bundle of this shell would have had there.
///
/// A page with neither comment has no notice to remove — a deployment may
/// overlay a boot page of its own. One comment without the other, either of
/// them twice, or the pair out of order is [`ErrorCode::Internal`] rather
/// than a guess at where the sandbox's text ends.
fn index_without_boot_notice(text: &str) -> Result<String, Refusal> {
    let starts = text.matches(BOOT_NOTICE_START).count();
    let ends = text.matches(BOOT_NOTICE_END).count();
    if starts == 0 && ends == 0 {
        return Ok(text.to_string());
    }
    if starts != 1 || ends != 1 {
        return Err(index_malformed(&format!(
            "contains {starts} of {BOOT_NOTICE_START:?} and {ends} of {BOOT_NOTICE_END:?} \
             where the export needs one of each"
        )));
    }
    let (before, rest) = text
        .split_once(BOOT_NOTICE_START)
        .expect("counted exactly one");
    let Some((_notice, after)) = rest.split_once(BOOT_NOTICE_END) else {
        return Err(index_malformed(&format!(
            "closes its boot notice ({BOOT_NOTICE_END:?}) before opening it"
        )));
    };
    Ok(format!(
        "{before}{BOOT_NOTICE_START}{BOOT_NOTICE_END}{after}"
    ))
}

/// The page with `title` everywhere the bundler showed the deployment's.
///
/// The bundler shows the title in `<title>` and, in the body, inside
/// [`APP_TITLE_OPEN`]…[`APP_TITLE_CLOSE`] (the heading and the `<noscript>`
/// line), always as escaped text — so the closing tag that ends each one is
/// the first one after it opens.
///
/// A page with no wrapped title in its body is not the bundler's page (an
/// overlaid boot page) and is left exactly as it is, `<title>` included: its
/// author chose that title. A bundler page must have exactly one `<title>`,
/// and every wrapper must close; anything else is [`ErrorCode::Internal`].
fn index_with_title(text: &str, title: &str) -> Result<String, Refusal> {
    if !text.contains(APP_TITLE_OPEN) {
        return Ok(text.to_string());
    }
    let title = html_text(title);
    let replaced = replace_between(text, "<title>", "</title>", &title)?;
    if replaced.1 != 1 {
        return Err(index_malformed(&format!(
            "has {} <title> elements where the export needs one",
            replaced.1
        )));
    }
    Ok(replace_between(&replaced.0, APP_TITLE_OPEN, APP_TITLE_CLOSE, &title)?.0)
}

/// `text` with the content of every `open`…`close` span replaced by `with`,
/// and how many there were.
fn replace_between(
    text: &str,
    open: &str,
    close: &str,
    with: &str,
) -> Result<(String, usize), Refusal> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut count = 0;
    while let Some((before, after_open)) = rest.split_once(open) {
        let Some((_old, after_close)) = after_open.split_once(close) else {
            return Err(index_malformed(&format!(
                "opens {open:?} without closing it"
            )));
        };
        out.push_str(before);
        out.push_str(open);
        out.push_str(with);
        out.push_str(close);
        rest = after_close;
        count += 1;
    }
    out.push_str(rest);
    Ok((out, count))
}

/// `text` as HTML text content — the bundler's own escaping of a title.
fn html_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The rules the exported worker applies: the deployment's, minus the
/// compiler prefix [`sw_without_compiler`] removes from the exported `sw.js`.
fn without_compiler(mut rules: BypassRules) -> BypassRules {
    rules.prefixes.retain(|prefix| prefix != COMPILER_ROOT);
    rules
}

/// The declaration `sw.js.tmpl` renders the rules the worker hands
/// `initialize({ bypass })` into: one line, `const BYPASS_RULES = {…};`.
const SW_BYPASS_RULES: &str = "const BYPASS_RULES = ";

/// `sw.js` with the compiler's bypass removed, if it has one — from BOTH
/// places the bundler renders it: the fetch handler's condition and the
/// `BYPASS_RULES` the worker hands its runtime.
///
/// A deployment that ships the in-browser toolchain adds
/// `/__impresspress_dev/compiler/` to the service worker's bypass list (the
/// bundle's `extra_bypass_prefix`). The export does not copy those assets —
/// there is no `/b/dev` in an exported site to load them — so a clause for it
/// would be a bypass for a tree that is not there: every request under the
/// prefix waved past the runtime to a 404 from the static host. And the rules
/// the exported worker hands its runtime must say what its condition does:
/// that runtime's seed import refuses a site file at any path they list.
///
/// # The condition
///
/// `impresspress-bundle`'s `BypassRules::render_condition` renders the prefix
/// as one `url.pathname.startsWith('…')` clause on a line of its own, led by
/// its `||` (exact rules always precede it, so it is never the first clause).
/// The clause is matched by that exact text rather than loosely, so a clause
/// this does not recognise is left alone instead of half-edited, and removing
/// the whole clause keeps the remaining expression exactly as the bundler
/// would have rendered it for a bundle that never asked.
///
/// # The data
///
/// The declaration is found by its exact text and must occur at most once:
/// absent, it is a `sw.js` from a bundler that predates it and is left alone
/// (its runtime gets no rules); present twice, this cannot know which one the
/// worker uses, and refuses rather than guess.
///
/// # They must agree
///
/// The prefix's absence is not an error — it is an app's own bypass entry
/// (CI's foundations bundle ships no compiler and never adds it). But when
/// the declaration exists, the data listing the prefix and the condition
/// carrying the clause are one fact rendered twice: one without the other
/// means the clause's text is not what this expects, and exporting anyway
/// would ship a worker whose condition and stated rules differ. That is an
/// export error, never a silent half-strip.
fn sw_without_compiler(bytes: Vec<u8>) -> Result<Vec<u8>, Refusal> {
    let text = String::from_utf8(bytes)
        .map_err(|e| Refusal::Shell(format!("the deployment's sw.js is not UTF-8: {e}")))?;

    let clause = format!(" ||\n        url.pathname.startsWith('{COMPILER_ROOT}')");
    let had_clause = text.contains(&clause);
    let text = text.replace(&clause, "");

    match text.matches(SW_BYPASS_RULES).count() {
        0 => return Ok(text.into_bytes()),
        1 => {}
        n => {
            return Err(Refusal::Shell(format!(
                "the deployment's sw.js declares {SW_BYPASS_RULES:?} {n} times; the export \
                 needs exactly one to remove the compiler's rule from it"
            )))
        }
    }
    let start = text.find(SW_BYPASS_RULES).expect("counted once") + SW_BYPASS_RULES.len();
    let len = text[start..].find(";\n").ok_or_else(|| {
        Refusal::Shell("the deployment's sw.js leaves BYPASS_RULES unterminated".to_string())
    })?;
    let rules: BypassRules = serde_json::from_str(&text[start..start + len]).map_err(|e| {
        Refusal::Shell(format!(
            "the deployment's sw.js BYPASS_RULES did not parse: {e}"
        ))
    })?;
    let listed = rules.prefixes.iter().any(|prefix| prefix == COMPILER_ROOT);
    if listed != had_clause {
        return Err(Refusal::Shell(format!(
            "the deployment's sw.js disagrees with itself about {COMPILER_ROOT:?}: its \
             BYPASS_RULES {} the prefix and its fetch condition {} the clause the export \
             removes, so the export cannot remove the compiler's bypass from both; the bundle \
             was built by a different impresspress-bundle than this runtime expects",
            if listed { "list" } else { "do not list" },
            if had_clause { "has" } else { "does not have" },
        )));
    }
    let rendered = serde_json::to_string(&without_compiler(rules))
        .map_err(|e| Refusal::Shell(format!("BYPASS_RULES did not serialize: {e}")))?;
    Ok(format!("{}{rendered}{}", &text[..start], &text[start + len..]).into_bytes())
}

/// The first eight characters of a generation id — what the downloaded file
/// is named after.
fn short_id(generation_id: &str) -> String {
    generation_id.chars().take(8).collect()
}

/// The exported site's name: what its README is headed with and what its
/// boot page is titled.
async fn exported_title(ctx: &dyn Context) -> Result<String, WaferError> {
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
    Ok(title)
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
        .replace("{{TITLE}}", facts.title)
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
    /// A site file of the active generation is at a path the exported worker
    /// would shadow; the message names it and the rule.
    ShadowedSiteFile(String),
    /// The static shell could not be listed or read.
    Shell(String),
    /// A storage, ledger or encoding failure.
    Internal(WaferError),
}

/// What [`Refusal::ShadowedSiteFile`] says, on both surfaces. `refused` is
/// [`BypassRules::refuse_shadowed`]'s own sentence — the path, the URL and
/// the rule.
fn shadowed_site_file(refused: &str) -> String {
    format!(
        "this site cannot be exported: {refused} The exported site would refuse to import a \
         bundle that carries it. Delete the file or move it to another path, then export again."
    )
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
            // 400 and `FailedPrecondition`, as `NothingPublished` is: the
            // caller can fix it, and nothing about the request is malformed.
            Self::ShadowedSiteFile(refused) => no_store_error_status(
                ErrorCode::FailedPrecondition,
                400,
                &shadowed_site_file(&refused),
            ),
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
            Self::ShadowedSiteFile(refused) => {
                WaferError::new(ErrorCode::FailedPrecondition, shadowed_site_file(&refused))
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
            README_TEMPLATE.contains("/b/userportal/security"),
            "…and where: the account's Security page, which holds the change-password form"
        );
    }

    /// A byte order mark only where a charset-less host needs one.
    #[test]
    fn a_root_text_file_gets_a_byte_order_mark_only_when_it_is_non_ascii_utf8() {
        // ASCII reads the same in any encoding a host implies.
        assert_eq!(for_a_charsetless_host(b"# Kiln\n".to_vec()), b"# Kiln\n");
        // UTF-8 beyond ASCII gets exactly one.
        assert_eq!(
            for_a_charsetless_host("caf\u{e9} \u{2014}".as_bytes().to_vec()),
            [UTF8_BOM, "caf\u{e9} \u{2014}".as_bytes()].concat()
        );
        // A file that already has one keeps it, and only it.
        let marked = [UTF8_BOM, "caf\u{e9}".as_bytes()].concat();
        assert_eq!(for_a_charsetless_host(marked.clone()), marked);
        // Bytes that are not UTF-8 get no UTF-8 BOM: it would be a false
        // claim about their encoding.
        assert_eq!(for_a_charsetless_host(b"caf\xe9".to_vec()), b"caf\xe9");
        assert_eq!(for_a_charsetless_host(Vec::new()), b"");
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
