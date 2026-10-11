//! `GET /b/dev/api/export` and `GET /b/dev/api/export/manifest` — the sandbox
//! as a folder anyone can serve.
//!
//! Design §10.1. The archive is the deployment's EXPORT SHELL — the site
//! runtime's own bundle, which a sandbox deployment carries at
//! `impresspress-bundle`'s `EXPORT_SHELL_ROOT` — plus a `seed/` tree in
//! exactly the format [`super::seed`] reads on a cold boot. That symmetry is
//! the whole design: there is one mechanism for "a fresh instance gets a
//! site", and an export is a bundle that feeds it.
//!
//! ```text
//!     README.md                     how to serve it, what is inside, and the
//!                                   disclosure about `data.json`
//!     index.html sw.js loader.js …  the export shell, listed by its own
//!                                   `asset-manifest.json`'s `files`
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
//! without downloading the archive. The cost is that the preview reads the shell
//! too; it is a browser-local read of a handful of files the service worker
//! already has, on an explicit call.
//!
//! # The shell is built as a site, not rewritten into one
//!
//! The exported site is a plain ImpressPress deployment: no `/b/dev`, no
//! in-browser compiler, no cross-origin-isolation headers on static files —
//! and no workspace code in its wasm at all. The sandbox's own shell cannot
//! be made into that by editing it: its `sw.js` can be told to boot a site,
//! but its wasm is compiled with the whole workspace. So the deployment
//! carries a second bundle, built by the same bundler around a wasm compiled
//! without the workspace (`[sandbox] mode = "site"`, no compiler bypass, no
//! boot notice — `examples/dev-sandbox/export-shell/`), and the export copies
//! THAT one, file for file, each body checked against the digest its
//! manifest lists ([`ShellListing`]).
//!
//! One edit remains, and it is the site's data rather than the build's: the
//! boot page shows the shell's placeholder title in `<title>` and twice in
//! the body, and the exported site has a name of its own — the one the
//! README is headed with. See [`index_with_title`].
//!
//! A deployment without an export shell does not export. It never falls
//! back to the sandbox's own shell: that would ship the workspace — the
//! compiler adapter, the scaffold templates, this module — to everyone the
//! site is handed to, which is what the export shell exists to prevent.
//!
//! # `llms.txt`
//!
//! Two different files have that name, and the export treats them
//! differently by construction rather than by a rule about the name.
//!
//! The SANDBOX's — what the sandbox tells a reader about itself — is not
//! exported, because nothing the export reads holds it. The static host's
//! copy is an overlay of the sandbox's deployment, and the export reads none
//! of that deployment's files, only its export shell's, whose listing names
//! the shell's own (the CLI refuses an overlay under the shell's root). The
//! runtime's copy is published by `super::publisher` without being in any
//! generation's manifest, and `seed/site/**` is that manifest. A shell file
//! the listing DOES name is the shell's own bundled file and is copied like
//! any other.
//!
//! The SITE's — `site/llms.txt`, if the site has one — is exported twice,
//! from one read of one blob: as `seed/site/llms.txt`, which the exported
//! runtime imports and serves once its worker controls the page, and as
//! `llms.txt` at the archive's root, which the static host serves to a
//! reader that runs no JavaScript and so never gets a worker. `/llms.txt`
//! is not a path the exported worker leaves to the static host (the
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
    contracts::{ExportFile, ExportManifest},
    control::{ShellListing, SHELL_MANIFEST},
    data_snapshot, generation, no_store, no_store_db_error_internal, no_store_error_status, repo,
    scaffold,
    seed::{self, SeedBlock, SeedManifest},
    workspace,
    zip::ZipWriter,
    DevShared,
};
use crate::{config_vars::APP_NAME_KEY, http::err_internal};

/// The shell's boot page, the one shell file this export edits: its title
/// becomes the exported site's ([`index_with_title`]).
const INDEX_PATH: &str = "index.html";

/// What the bundler wraps each showing of the app's title in, in the boot
/// page's body (`impresspress-bundle`'s `APP_TITLE_OPEN` /
/// `APP_TITLE_CLOSE`). Restated because the bundler is not a dependency of
/// this crate; `crates/impresspress/tests/seed_bypass_prefix.rs` compares the
/// spellings.
pub const APP_TITLE_OPEN: &str = "<span data-app-title>";
pub const APP_TITLE_CLOSE: &str = "</span>";

/// Where the data snapshot lands, relative to [`seed::ROOT`].
///
/// `seed::data_url(DATA_PATH)` is the URL the importer fetches and
/// `seed/{DATA_PATH}` is the archive entry — the two are the same string
/// because [`seed`] owns the layout and this writes what it reads.
const DATA_PATH: &str = "data.json";

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

    // The export shell's listing: one small read, and the rules the EXPORTED
    // worker applies, which the check below needs. A deployment that carries
    // no shell is refused here, before anything else is read.
    let Some(source) = shared.workspace.export_shell.as_deref() else {
        return Err(Refusal::Shell(
            "is not part of this deployment: its service worker handed initialize() no \
             exportShell"
                .to_string(),
        ));
    };
    let (listing, listing_bytes) = ShellListing::read(source).await.map_err(Refusal::Shell)?;

    // A site file the EXPORTED worker would shadow, before any content is
    // read. The exported runtime's seed import refuses a whole bundle over
    // one such file (`seed::import`), so an export carrying it would be a
    // folder that boots with no site at all. Checked against the export
    // shell's own rules — the ones its `sw.js` was rendered with — not this
    // sandbox's: the two workers differ (the sandbox's leaves its compiler
    // tree to the static host, the site's has none), and the sandbox
    // refuses a write by its own rules, so this fires for a file at a path
    // only the exported worker bypasses, or one that predates the check.
    for entry in &manifest.site.files {
        let workspace_path = format!("{}{}", workspace::SITE_PREFIX, entry.path);
        listing
            .bypass
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
        let _serialized = shared.workspace_manifest.lock().await;
        workspace::load(ctx).await.map_err(Refusal::Internal)?
    };

    // --- the data snapshot -----------------------------------------------
    //
    // First, before the shell's files and before any stored content: it is
    // the one part of the bundle with a size limit, and an export that is
    // going to be refused should cost one snapshot (and the shell's small
    // manifest, read above), not the runtime's wasm and the whole site as
    // well.
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
    //
    // Every file the export shell lists, in its order, each body checked
    // against the digest the listing gives it — and the manifest itself as
    // it was read above, the one file that cannot carry its own digest.
    let mut shell: Vec<Entry> = Vec::with_capacity(listing.files.len());
    for path in &listing.files {
        // The archive writes its own `seed/`. A shell that listed one would
        // put two entries under one name; a bundle never lists one (the
        // sandbox's seed is an overlay the bundler does not list), so this
        // is a shell built some other way.
        if path.starts_with(archive_seed_prefix()) {
            return Err(Refusal::Shell(format!(
                "lists {path:?}, under the {} the export writes itself",
                archive_seed_prefix()
            )));
        }
        let bytes = if path == SHELL_MANIFEST {
            listing_bytes.clone()
        } else {
            listing.fetch(source, path).await.map_err(Refusal::Shell)?
        };
        let bytes = if path == INDEX_PATH {
            index_with_title(bytes, &title)?
        } else {
            bytes
        };
        shell.push(Entry {
            path: path.clone(),
            bytes,
        });
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
        }),
        // An exported site has no workspace — no `/b/dev`, no reference tool
        // — so a guide would describe tools the bundle does not have.
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

/// The boot page with `title` everywhere the bundler showed the shell's
/// placeholder.
///
/// The bundler shows the title in `<title>` and, in the body, inside
/// [`APP_TITLE_OPEN`]…[`APP_TITLE_CLOSE`] (the heading and the `<noscript>`
/// line), always as escaped text — so the closing tag that ends each one is
/// the first one after it opens.
///
/// The page is the export shell's, which is always the bundler's: the CLI
/// lays the shell down whole and refuses an overlay under its root, and
/// [`ShellListing::fetch`] has verified these bytes against its digest. So a
/// page with no wrapped title, more or fewer than one `<title>`, or a wrapper
/// that does not close is a shell built by some other bundler, and refused —
/// never shipped with the placeholder.
fn index_with_title(bytes: Vec<u8>, title: &str) -> Result<Vec<u8>, Refusal> {
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Err(index_malformed("is not valid UTF-8"));
    };
    if !text.contains(APP_TITLE_OPEN) {
        return Err(index_malformed(&format!(
            "shows no title wrapped in {APP_TITLE_OPEN:?}"
        )));
    }
    let title = html_text(title);
    let replaced = replace_between(text, "<title>", "</title>", &title)?;
    if replaced.1 != 1 {
        return Err(index_malformed(&format!(
            "has {} <title> elements where the export needs one",
            replaced.1
        )));
    }
    Ok(
        replace_between(&replaced.0, APP_TITLE_OPEN, APP_TITLE_CLOSE, &title)?
            .0
            .into_bytes(),
    )
}

/// The refusal for a boot page the export cannot title safely.
fn index_malformed(what: &str) -> Refusal {
    Refusal::Shell(format!(
        "'s {INDEX_PATH} {what}, so the export cannot give the exported site its name; it was \
         built by a different impresspress-bundle than this runtime expects"
    ))
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
    // `export_zip_contains_shell_seed_sources_and_data_booted_as_a_site` asserts
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
/// data), a deployment the agent cannot export from until it is rebuilt (its
/// export shell is missing or broken), and everything the storage or ledger
/// refused, which is already a [`WaferError`].
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
    /// The deployment's export shell is missing, could not be read, or is
    /// not the bundle its manifest describes; the message completes "its
    /// export shell …". Nothing the caller sent can fix it — the deployment
    /// has to be rebuilt — but the caller is the one who has to be told, so
    /// it is answered in full rather than as a sanitized internal error.
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

/// What [`Refusal::Shell`] says, on both surfaces. `detail` completes "its
/// export shell …".
fn export_shell_unavailable(detail: &str) -> String {
    format!(
        "this sandbox cannot export: its export shell {detail}. The export shell is the site \
         runtime an export ships; rebuild and redeploy the sandbox with \
         examples/dev-sandbox/build.sh, which builds it."
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
/// [`assemble`] reads the manifest under `DevShared::workspace_manifest` and then reads
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
            // 503 and `Unavailable`: the request is fine and the deployment
            // cannot serve it until it is rebuilt.
            Self::Shell(detail) => no_store_error_status(
                ErrorCode::Unavailable,
                503,
                &export_shell_unavailable(&detail),
            ),
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
            Self::Shell(detail) => {
                WaferError::new(ErrorCode::Unavailable, export_shell_unavailable(&detail))
            }
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
}
