//! `/b/dev/api/files*` — read, write, list and delete workspace files.
//!
//! # Lost updates, and how far the guard reaches
//!
//! Every mutation states the hash it believes the file currently has
//! (`expected_sha256`, `null` for "no file yet"). A mismatch is a `409`
//! carrying the hash it actually has, so the caller re-reads rather than
//! silently overwriting an edit it never saw. The sandbox's clients are an
//! agent and a human editing the same workspace at the same time; making the
//! check optional would make that race invisible.
//!
//! That guard is **per path**. A write is read-modify-write over the whole
//! manifest — `workspace::load`, mutate one entry, `workspace::save` — so two
//! writers interleaving between the load and the save would each save a
//! manifest built from a snapshot that predates the other, and the later save
//! would drop the earlier writer's entry even though both passed their own
//! hash check: the hash check is about the path each writer named and says
//! nothing about the rest of the file.
//!
//! The whole load-mutate-save therefore runs under `DevShared::workspace`, an
//! async mutex held across those storage calls. It guards the *manifest*, not
//! the publish: the guard is released before the activation the mutation
//! requests, so editing never waits behind a runtime rebuild. Any handler
//! added to this module that mutates `workspace.json` must take it.
//!
//! # Readers take it too
//!
//! A lock only writers take orders writers against each other and orders
//! nothing against a read. Every storage call is a suspension point and the
//! sandbox's fetch events dispatch concurrently, so an unlocked read really
//! does interleave with a mutation — and both of a read's failure modes are
//! that interleaving:
//!
//! * `workspace::load` takes a snapshot of `workspace.json` and then reads its
//!   bytes. A save landing between those two steps replaces the file, the
//!   snapshot no longer names anything, and the browser storage layer reports
//!   that as an internal error rather than a miss — a sanitized `500` on a
//!   read that asked for nothing unusual.
//! * A read that loaded the manifest and has not yet fetched its blob holds a
//!   *stale* manifest. A write to the same path repoints the entry, the
//!   collector reclaims the blob the old entry named, and the blob fetch is a
//!   `NotFound` the handler can only report as corruption — the manifest it is
//!   holding names content that no longer exists.
//!
//! ## Every read of `workspace.json` is under the lock
//!
//! Without exception, and the list is short enough to state:
//! [`handle_read`], [`handle_list`], `gc::storage_usage` (the `/b/dev` page's
//! ~3 Hz status poll), `activation::workspace_site` and `export::assemble`,
//! alongside the five mutators. `workspace_site` is the one that matters most
//! and was the last to get it: [`handle_write`] and [`handle_delete`] release
//! the guard before they publish, and `workspace_site` is what reads the
//! manifest back inside the activation that publish asked for — so it backs
//! **every ordinary site write and delete**, not a rare path, and an unlocked
//! read there answers a `500` on a write whose content is already saved.
//!
//! ## Reading a file's CONTENT is a different question, answered twice
//!
//! There are two deliberate and opposite strategies in this block, and they
//! are opposite because the two reads are not the same size:
//!
//! * [`handle_read`] holds the lock across the blob fetch as well as the load,
//!   which is what makes an entry and the content it names one view. It can
//!   afford to: one file is capped at [`paths::MAX_FILE_BYTES`].
//! * `export::assemble` deliberately does **not**. An export is the whole
//!   site, every block artifact and every block source — megabytes, uncapped —
//!   and holding the mutex across it would block editing for the export's
//!   duration. It admits the second failure mode above and maps it to a
//!   retry-able `409` (`export::content_gone`) rather than to an internal
//!   error, which is the honest answer for a read that can be repeated.
//!
//! So the rule is not "readers take the lock" but: *the manifest read is
//! always locked; a content read is locked when it is bounded and mapped to a
//! conflict when it is not.* `export`'s `content_gone` states the same pair
//! from its side.
//!
//! ## What it costs
//!
//! A read paces behind a mutation instead of failing. The status poll pays the
//! most for that: it runs on a fixed interval, so during a collector pass —
//! which holds the lock from its blob listing through a loop of sequential
//! deletes — a waiter accrues
//! per interval and `futures::lock::Mutex` is first-in-first-out, which would
//! queue the user's next save behind all of them. That is why the page's poll
//! now carries an in-flight guard (`assets/dev.js`): at most one status
//! request is outstanding, so the pass costs one waiter rather than one per
//! tick.
//!
//! The deadlock rule is unchanged and the readers keep it: the lock is only
//! ever held around `workspace.json` access, never across an
//! `activation::request`, so nothing holding it waits on the queue that takes
//! it in `activation::adopt_site` and in `activation::workspace_site` — the
//! latter carries the full argument, being the only acquisition that happens
//! inside the queue.
//!
//! # Refusal shapes
//!
//! * A hash mismatch is a real `409` *response* whose body is
//!   [`FileConflict`] — the caller needs the current state, and an error
//!   terminal renders `{error, message}` instead.
//! * Everything else is an error terminal built by the block's own
//!   `no_store_error` / `no_store_error_status`: a bad path or body is `400`,
//!   a size or count over quota is `413`, a missing file is `404`, and the
//!   block-count quota is `409` (the workspace conflicts with a limit — the
//!   payload is not too large). All of them carry `Cache-Control: no-store`,
//!   as design §12 requires of every `/b/dev` response.
//! * A failed storage call — the manifest, a blob — is classified by
//!   `super::no_store_db_error_internal`: a WRAP denial is a `403` and a
//!   storage quota a `429`, both `no-store`; anything else is the sanitized
//!   `500`. A blob the manifest names but the store does not have is that
//!   `500`, never a `404`: the path exists and its content has gone.

use base64ct::{Base64, Encoding};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::{
    activation::{self, ActivationIntent},
    blobs,
    contracts::{
        FileConflict, FileDeleteRequest, FileDeleteResponse, FileEncoding, FileListQuery,
        FileListResponse, FileReadRequest, FileReadResponse, FileWriteRequest, FileWriteResponse,
        GenerationSummary,
    },
    no_store, no_store_db_error_internal, no_store_error, no_store_error_status,
    paths::{self, WorkspaceArea},
    repo::generations::GenerationCause,
    workspace::{self, FileEntry, Workspace},
    DevShared,
};
use crate::blocks::crud;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /b/dev/api/files` — the workspace manifest, optionally prefix-filtered.
pub async fn handle_list(ctx: &dyn Context, shared: &DevShared, msg: &Message) -> OutputStream {
    let query = FileListQuery::from_message(msg);
    // Under the mutation lock: `workspace::load` snapshots the object and then
    // reads it, and a save landing in between invalidates the snapshot (see
    // this module's header).
    let ws = {
        let _serialized = shared.workspace.lock().await;
        match workspace::load(ctx).await {
            Ok(ws) => ws,
            Err(e) => return no_store_db_error_internal(e, "dev workspace load"),
        }
    };
    let prefix = query.prefix.unwrap_or_default();
    // `Workspace::files` is a `BTreeMap`, so this is already path-ordered.
    let files: Vec<FileEntry> = ws
        .files
        .values()
        .filter(|entry| entry.path.starts_with(&prefix))
        .cloned()
        .collect();
    no_store().json(&FileListResponse { files })
}

/// `POST /b/dev/api/files/read` — one file's content.
pub async fn handle_read(
    ctx: &dyn Context,
    shared: &DevShared,
    input: InputStream,
) -> OutputStream {
    let request: FileReadRequest = match read_body(input).await {
        Ok(request) => request,
        Err(refusal) => return refusal,
    };
    if let Err(e) = paths::validate_path(&request.path) {
        return no_store_error(ErrorCode::InvalidArgument, &e.to_string());
    }
    // The lock spans the manifest load AND the blob fetch, which is the whole
    // point: an entry and the content it names are one fact, and a read that
    // released between them would be holding a manifest a mutation has since
    // replaced — the blob it names is exactly what the collector reclaims.
    let _serialized = shared.workspace.lock().await;
    let ws = match workspace::load(ctx).await {
        Ok(ws) => ws,
        Err(e) => return no_store_db_error_internal(e, "dev workspace load"),
    };
    let Some(entry) = ws.get(&request.path) else {
        return no_store_error(
            ErrorCode::NotFound,
            &format!("no file at {:?}", request.path),
        );
    };
    let bytes = match blobs::get(ctx, &entry.sha256).await {
        Ok(bytes) => bytes,
        // A manifest entry naming a blob that is not there is corruption, not
        // a missing file: the path exists, its content has gone. Under the
        // lock this can no longer mean "a mutation moved underneath the read".
        Err(e) => return no_store_db_error_internal(e, "dev workspace blob read"),
    };
    let (encoding, content) = encode_content(&entry.content_type, bytes);
    no_store().json(&FileReadResponse {
        path: entry.path.clone(),
        sha256: entry.sha256.clone(),
        size: entry.size,
        encoding,
        content,
    })
}

/// `POST /b/dev/api/files/write` — create or replace one file.
pub async fn handle_write(
    ctx: &dyn Context,
    shared: &DevShared,
    input: InputStream,
) -> OutputStream {
    let request: FileWriteRequest = match read_body(input).await {
        Ok(request) => request,
        Err(refusal) => return refusal,
    };
    let area = match paths::validate_path(&request.path) {
        Ok(area) => area,
        Err(e) => return no_store_error(ErrorCode::InvalidArgument, &e.to_string()),
    };
    // Refuse an over-large body BEFORE decoding it. `content` is already in
    // memory as part of the parsed request; decoding would allocate the same
    // payload a second time, so a hostile body must not get that far. The
    // bound is a *lower* bound on the decoded length, so this only ever
    // refuses what the exact check below would refuse anyway.
    if min_decoded_len(request.encoding, &request.content) > paths::MAX_FILE_BYTES {
        return too_large(&format!(
            "the {} body decodes to more than the {}-byte file limit",
            encoding_label(request.encoding),
            paths::MAX_FILE_BYTES
        ));
    }
    let bytes = match decode_content(request.encoding, &request.content) {
        Ok(bytes) => bytes,
        Err(detail) => return no_store_error(ErrorCode::InvalidArgument, &detail),
    };
    if bytes.len() > paths::MAX_FILE_BYTES {
        return too_large(&format!(
            "file is {} bytes; the limit is {} bytes",
            bytes.len(),
            paths::MAX_FILE_BYTES
        ));
    }

    // The manifest is read, changed and written back under `shared.workspace`,
    // so two writers cannot each save a snapshot that predates the other. The
    // guard is dropped before the publish below: the mutation is what has to
    // be serialized, not the activation it asks for.
    let entry = {
        let _serialized = shared.workspace.lock().await;
        let mut ws = match workspace::load(ctx).await {
            Ok(ws) => ws,
            Err(e) => return no_store_db_error_internal(e, "dev workspace load"),
        };
        let current = ws.get(&request.path);
        if !hash_matches(current, request.expected_sha256.as_deref()) {
            return conflict(&request.path, current);
        }

        // One name cannot be both a file and a directory in the store the
        // workspace is published to. Refused here, with both paths named,
        // rather than at publish time as an opaque backend type mismatch that
        // then recurs on every later publish — see
        // [`workspace::Workspace::path_collision`].
        if let Some(clash) = ws.path_collision(&request.path) {
            return no_store_error(
                ErrorCode::InvalidArgument,
                &format!(
                    "{:?} cannot be stored: {clash:?} already uses part of that path as a                      directory, or is a file this path would need as one. A name is a file or a                      directory, never both — rename one of them.",
                    request.path
                ),
            );
        }

        // How much the blob store would grow by. Content some entry already
        // names is certainly stored (the collector only reclaims unreachable
        // blobs), so re-writing it — an undo, or the same asset at a second
        // path — needs no headroom. Content that is stored but unreferenced is
        // treated as if it were new: those bytes are garbage awaiting
        // collection, and refusing rather than assuming they will be there is
        // the safe direction.
        let sha = blobs::sha256_hex(&bytes);
        let new_blob_bytes = if ws.references(&sha) {
            0
        } else {
            bytes.len() as u64
        };
        if let Err(e) = check_quotas(&ws, &request.path, &area, new_blob_bytes) {
            return e.into_response();
        }

        // Store, then save. The other order would let a manifest name a blob
        // that was never written, and every later read of that path would be a
        // 500. In this order a save that fails leaves a blob in the store that
        // no counter includes — and a retry of the same write finds it stored
        // and charges nothing either — so the quota under-counts the store
        // until the next collection, which sets both counters from the store's
        // own listing (`super::gc`) rather than from what was charged.
        match blobs::put_hashed(ctx, &sha, &bytes).await {
            // Charge the workspace only when the store actually grew.
            Ok(blobs::Stored::New) => ws.record_blob_stored(bytes.len() as u64),
            Ok(blobs::Stored::Deduplicated) => {}
            Err(e) => return no_store_db_error_internal(e, "dev workspace blob write"),
        }
        let entry = ws.insert(&request.path, sha, bytes.len() as u64);
        if let Err(e) = workspace::save(ctx, &ws).await {
            return no_store_db_error_internal(e, "dev workspace save");
        }
        entry
    };

    // Publish, if the file that changed is one the site serves. The order is
    // load-bearing: the workspace is saved first, and the activation reads it
    // back, so the generation is composed from persisted state. An activation
    // that published content the workspace had lost would be unreproducible
    // from the workspace it claims to project.
    let generation = match publish_if_site(ctx, shared, &area, GenerationCause::SiteWrite).await {
        Ok(generation) => generation,
        Err(refusal) => return refusal,
    };
    no_store().json(&FileWriteResponse {
        path: entry.path,
        sha256: entry.sha256,
        size: entry.size,
        generation,
    })
}

/// `POST /b/dev/api/files/delete` — drop one file from the manifest.
pub async fn handle_delete(
    ctx: &dyn Context,
    shared: &DevShared,
    input: InputStream,
) -> OutputStream {
    let request: FileDeleteRequest = match read_body(input).await {
        Ok(request) => request,
        Err(refusal) => return refusal,
    };
    let area = match paths::validate_path(&request.path) {
        Ok(area) => area,
        Err(e) => return no_store_error(ErrorCode::InvalidArgument, &e.to_string()),
    };
    {
        // Same serialization as a write, for the same reason: a delete is a
        // read-modify-write of the whole manifest.
        let _serialized = shared.workspace.lock().await;
        let mut ws = match workspace::load(ctx).await {
            Ok(ws) => ws,
            Err(e) => return no_store_db_error_internal(e, "dev workspace load"),
        };
        let current = ws.get(&request.path);
        if !hash_matches(current, Some(&request.expected_sha256)) {
            return conflict(&request.path, current);
        }
        // The blob stays: a generation that can still be rolled back to names
        // it.
        ws.remove(&request.path);
        if let Err(e) = workspace::save(ctx, &ws).await {
            return no_store_db_error_internal(e, "dev workspace save");
        }
    }
    let generation = match publish_if_site(ctx, shared, &area, GenerationCause::SiteDelete).await {
        Ok(generation) => generation,
        Err(refusal) => return refusal,
    };
    collect_if_unpublished(ctx, shared, &area).await;
    no_store().json(&FileDeleteResponse {
        path: request.path,
        generation,
    })
}

/// Publish the workspace's `site/` half as a new generation, when the file
/// that changed was one the site serves.
///
/// A `blocks/` edit publishes nothing (design §7.2): block source only reaches
/// the runtime through a compile, and republishing on every keystroke in a
/// `.rs` file would rebuild the runtime from an artifact that has not been
/// rebuilt.
///
/// Nothing about the manifest is decided here. [`ActivationIntent::SiteOnly`]
/// says only "publish what the workspace holds", and both halves are resolved
/// inside the queue once this request is the one running: the site from the
/// workspace file this handler has already saved, the blocks from the
/// generation that is active at that moment. Composing a manifest here instead
/// would freeze the block set as it looked *before* an activation that is
/// still in flight — and publishing that would tear the in-flight block back
/// out of the runtime.
async fn publish_if_site(
    ctx: &dyn Context,
    shared: &DevShared,
    area: &WorkspaceArea,
    cause: GenerationCause,
) -> Result<Option<GenerationSummary>, OutputStream> {
    if !matches!(area, WorkspaceArea::Site) {
        return Ok(None);
    }
    match activation::request(ctx, shared, cause, ActivationIntent::SiteOnly).await {
        Ok(outcome) => Ok(Some(outcome.generation)),
        Err(e) => Err(e.into_response()),
    }
}

/// Reclaim the blob a delete may have orphaned, when the delete published
/// nothing.
///
/// A `site/` delete activates, and the activation prunes and collects. A
/// `blocks/` delete does not activate at all (design §7.2: block source only
/// reaches the runtime through a compile), and a block source is named by the
/// workspace and by nothing else — no generation carries a crate, only the
/// artifact compiled from it. So dropping the entry makes its blob garbage
/// immediately, and without this it would stay charged against the workspace's
/// quota until some unrelated site write happened to publish.
///
/// [`activation::maintain`] itself, not a bare `gc::collect`: the rule that
/// pruning comes first and collection second is a property of the two steps,
/// not of the caller that runs them, and this path was safe only for as long
/// as every successful activation happened to leave the ledger inside the
/// window — an invariant nothing stated and that a prune failure (which
/// `maintain` logs and carries on from) breaks.
///
/// After the manifest is saved and outside the lock that saved it: the
/// collector takes that lock itself, and it reads the workspace it is about to
/// collect against rather than the one this handler was holding.
///
/// Failures are logged, not returned — `maintain`'s own contract: the file
/// *is* deleted, and reporting the delete as failed because the reclaim did
/// would be untrue. The next activation collects instead.
async fn collect_if_unpublished(ctx: &dyn Context, shared: &DevShared, area: &WorkspaceArea) {
    if matches!(area, WorkspaceArea::Site) {
        return;
    }
    activation::maintain(ctx, shared).await;
}

// ---------------------------------------------------------------------------
// Bodies and encodings
// ---------------------------------------------------------------------------

/// Deserialize a request body, refusing a malformed one with a `no-store` 400.
async fn read_body<T: serde::de::DeserializeOwned>(input: InputStream) -> Result<T, OutputStream> {
    crud::read_json_body_or(input, |detail| {
        no_store_error(
            ErrorCode::InvalidArgument,
            &format!("invalid request body: {detail}"),
        )
    })
    .await
}

/// A lower bound on how many bytes `content` decodes to.
///
/// Used to refuse an over-large body before it is decoded into a second
/// allocation. It must never over-estimate, or a legal write would be
/// refused: utf8 is exact (a JSON string's UTF-8 bytes are the file's bytes),
/// and the base64 bound is [`paths::min_base64_decoded_len`] — shared with
/// the artifact-size guard so one formula backs both limits.
fn min_decoded_len(encoding: FileEncoding, content: &str) -> usize {
    match encoding {
        FileEncoding::Utf8 => content.len(),
        FileEncoding::Base64 => paths::min_base64_decoded_len(content),
    }
}

/// How an encoding is named in a refusal.
fn encoding_label(encoding: FileEncoding) -> &'static str {
    match encoding {
        FileEncoding::Utf8 => "utf8",
        FileEncoding::Base64 => "base64",
    }
}

/// Decode a request's `content` field per its declared encoding.
fn decode_content(encoding: FileEncoding, content: &str) -> Result<Vec<u8>, String> {
    match encoding {
        FileEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        FileEncoding::Base64 => {
            Base64::decode_vec(content).map_err(|e| format!("`content` is not valid base64: {e}"))
        }
    }
}

/// Choose how to hand `bytes` back to the caller.
///
/// `utf8` only when the type *could* be text ([`paths::may_be_text`], which
/// includes the unknown-extension fallback so a `.gitignore` or a `README`
/// comes back editable) **and** the bytes really are UTF-8: a `.txt` holding
/// latin-1 would otherwise be lossily re-encoded by the JSON serializer, and
/// the caller would write back something other than what it read. The stored
/// content type is untouched either way — this decides the envelope, not the
/// file's type.
fn encode_content(content_type: &str, bytes: Vec<u8>) -> (FileEncoding, String) {
    if !paths::may_be_text(content_type) {
        return (FileEncoding::Base64, Base64::encode_string(&bytes));
    }
    // `into_bytes` hands the buffer back on failure, so the UTF-8 check costs
    // no copy of a body that can be half a megabyte.
    match String::from_utf8(bytes) {
        Ok(text) => (FileEncoding::Utf8, text),
        Err(e) => (FileEncoding::Base64, Base64::encode_string(&e.into_bytes())),
    }
}

// ---------------------------------------------------------------------------
// Conflicts and quotas
// ---------------------------------------------------------------------------

/// Whether `expected` describes `current`: `None` means "no file here".
fn hash_matches(current: Option<&FileEntry>, expected: Option<&str>) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(entry), Some(expected)) => entry.sha256 == expected,
        _ => false,
    }
}

/// The `413` every size refusal sends.
fn too_large(message: &str) -> OutputStream {
    no_store_error_status(ErrorCode::ResourceExhausted, 413, message)
}

/// The `409` for a hash that does not describe the file as it stands.
fn conflict(path: &str, current: Option<&FileEntry>) -> OutputStream {
    no_store()
        .status(409)
        .json(&FileConflict::new(path, current))
}

/// A workspace-wide limit a write would have crossed.
///
/// `pub(super)` because [`super::scaffold`] writes into the same workspace
/// through the same manifest and must be held to the same limits — one
/// definition of the rules, not a second copy that could disagree about what
/// a full workspace is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuotaError {
    /// The workspace already holds [`paths::MAX_FILES`] files.
    TooManyFiles,
    /// Storing the new blob would take the workspace's blob store past
    /// [`paths::MAX_WORKSPACE_BYTES`].
    WorkspaceFull { would_be: u64 },
    /// The write would define a [`paths::MAX_BLOCKS`]-plus-first block.
    TooManyBlocks,
}

impl QuotaError {
    /// The refusal to send.
    ///
    /// Size limits are `413`; the block count is `409`, because nothing about
    /// the request is too large — the workspace's shape conflicts with a limit
    /// on how much the runtime can be asked to rebuild.
    pub(super) fn into_response(self) -> OutputStream {
        match self {
            Self::TooManyFiles => too_large(&format!(
                "the workspace already holds {} files, which is the limit",
                paths::MAX_FILES
            )),
            Self::WorkspaceFull { would_be } => too_large(&format!(
                "the write would take the workspace's stored blobs to {would_be} bytes; the \
                 limit is {} bytes. Superseded and deleted content still counts until it is \
                 collected.",
                paths::MAX_WORKSPACE_BYTES
            )),
            Self::TooManyBlocks => no_store_error(
                ErrorCode::AlreadyExists,
                &format!(
                    "the workspace already defines {} blocks, which is the limit",
                    paths::MAX_BLOCKS
                ),
            ),
        }
    }
}

/// Whether a write that adds `new_blob_bytes` to the blob store at `path`
/// keeps the workspace inside every limit.
///
/// `new_blob_bytes` is what the *store* would grow by, not the file's size:
/// re-writing content that is already stored grows nothing, and the 64 MiB
/// limit is on stored blobs — including the ones no entry names any more —
/// because that is what actually occupies the user's storage. A limit
/// computed from the live manifest would let two hundred overwrites of one
/// 512 KiB page consume 100 MB while reporting 512 KiB.
///
/// Pure, and separate from the handler, because the interesting cases are the
/// boundaries — a workspace full of unreachable blobs, a block that already
/// exists — and driving hundreds of HTTP writes to reach one of them would
/// test the loop rather than the rule.
pub(super) fn check_quotas(
    ws: &Workspace,
    path: &str,
    area: &WorkspaceArea,
    new_blob_bytes: u64,
) -> Result<(), QuotaError> {
    if ws.get(path).is_none() && ws.files.len() >= paths::MAX_FILES {
        return Err(QuotaError::TooManyFiles);
    }
    let would_be = ws.blob_bytes.saturating_add(new_blob_bytes);
    if would_be > paths::MAX_WORKSPACE_BYTES {
        return Err(QuotaError::WorkspaceFull { would_be });
    }
    if let WorkspaceArea::Block(name) = area {
        let names = ws.block_names();
        if !names.iter().any(|known| known == name) && names.len() >= paths::MAX_BLOCKS {
            return Err(QuotaError::TooManyBlocks);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: &str, size: u64) -> FileEntry {
        FileEntry {
            path: "site/a.css".to_string(),
            sha256: sha.to_string(),
            size,
            content_type: "text/css; charset=utf-8".to_string(),
        }
    }

    #[test]
    fn a_hash_describes_a_file_only_when_both_agree_it_exists() {
        assert!(hash_matches(None, None));
        assert!(hash_matches(Some(&entry("abc", 1)), Some("abc")));
        // A new-file write over a file that exists.
        assert!(!hash_matches(Some(&entry("abc", 1)), None));
        // A replacement of a file that is not there.
        assert!(!hash_matches(None, Some("abc")));
        assert!(!hash_matches(Some(&entry("abc", 1)), Some("def")));
        // Hex is compared verbatim — no case folding, no prefix match.
        assert!(!hash_matches(Some(&entry("abc", 1)), Some("ABC")));
        assert!(!hash_matches(Some(&entry("abc", 1)), Some("ab")));
    }

    #[test]
    fn utf8_text_comes_back_as_text_and_everything_else_as_base64() {
        assert_eq!(
            encode_content("text/plain; charset=utf-8", b"hi".to_vec()),
            (FileEncoding::Utf8, "hi".to_string())
        );
        assert_eq!(
            encode_content("image/png", vec![0x89, 0x50]),
            (FileEncoding::Base64, Base64::encode_string(&[0x89, 0x50]))
        );
        // Textual by extension, but not actually UTF-8: base64 rather than a
        // lossy re-encode.
        let latin1 = vec![0xffu8, 0xfe];
        assert_eq!(
            encode_content("text/plain; charset=utf-8", latin1.clone()),
            (FileEncoding::Base64, Base64::encode_string(&latin1))
        );
        // Unknown type, valid UTF-8: still handed back as text. This is the
        // `.gitignore` / `README` case.
        assert_eq!(
            encode_content(paths::UNKNOWN_CONTENT_TYPE, b"target/\n".to_vec()),
            (FileEncoding::Utf8, "target/\n".to_string())
        );
        // Unknown type, not UTF-8: base64.
        assert_eq!(
            encode_content(paths::UNKNOWN_CONTENT_TYPE, latin1.clone()),
            (FileEncoding::Base64, Base64::encode_string(&latin1))
        );
    }

    #[test]
    fn content_decodes_per_the_declared_encoding() {
        assert_eq!(decode_content(FileEncoding::Utf8, "hi"), Ok(b"hi".to_vec()));
        assert_eq!(
            decode_content(FileEncoding::Base64, &Base64::encode_string(&[1, 2, 3])),
            Ok(vec![1, 2, 3])
        );
        assert!(decode_content(FileEncoding::Base64, "not base64!!").is_err());
        // A base64 body is never silently taken as text.
        assert_eq!(
            decode_content(FileEncoding::Utf8, "aGk="),
            Ok(b"aGk=".to_vec())
        );
    }

    /// Build a workspace whose entries are `files`, charged as if each had
    /// been stored once — the state a run of successful writes leaves behind.
    fn workspace_with(files: &[(&str, u64)]) -> Workspace {
        let mut ws = Workspace::default();
        for (path, size) in files {
            ws.insert(path, format!("sha-{path}"), *size);
            ws.record_blob_stored(*size);
        }
        ws
    }

    #[test]
    fn a_write_inside_every_limit_is_allowed() {
        let ws = workspace_with(&[("site/a.css", 10)]);
        assert_eq!(
            check_quotas(&ws, "site/b.css", &WorkspaceArea::Site, 10),
            Ok(())
        );
    }

    /// A body far too large must be refused from its encoded length alone,
    /// before it is decoded into a second copy. That is what the guard is
    /// for — a hostile body, not a body one byte over.
    #[test]
    fn an_over_large_body_is_refused_from_its_encoded_length() {
        let hostile = paths::MAX_FILE_BYTES * 2;
        assert!(min_decoded_len(FileEncoding::Utf8, &"x".repeat(hostile)) > paths::MAX_FILE_BYTES);
        // 4 base64 characters per 3 bytes.
        let encoded = "A".repeat(hostile.div_ceil(3) * 4);
        assert!(min_decoded_len(FileEncoding::Base64, &encoded) > paths::MAX_FILE_BYTES);

        // utf8 is exact, so even one byte over is caught before decoding.
        assert!(
            min_decoded_len(FileEncoding::Utf8, &"x".repeat(paths::MAX_FILE_BYTES + 1))
                > paths::MAX_FILE_BYTES
        );
        // base64's bound is short by up to two bytes (it cannot know how much
        // of the tail is padding), so a body a byte or two over slips past
        // this guard and is caught by the exact check after decoding. Both
        // refuse; only the first avoids the allocation.
        let just_over = vec![b'z'; paths::MAX_FILE_BYTES + 1];
        assert!(
            min_decoded_len(FileEncoding::Base64, &Base64::encode_string(&just_over))
                <= paths::MAX_FILE_BYTES
        );
        assert_eq!(
            decode_content(FileEncoding::Base64, &Base64::encode_string(&just_over))
                .expect("decodes")
                .len(),
            paths::MAX_FILE_BYTES + 1
        );
    }

    /// The bound must never over-estimate, or a legal write is refused before
    /// anything looks at the real bytes.
    #[test]
    fn the_pre_decode_bound_never_exceeds_the_real_decoded_length() {
        for len in 0..64usize {
            let bytes = vec![b'z'; len];
            let encoded = Base64::encode_string(&bytes);
            assert!(
                min_decoded_len(FileEncoding::Base64, &encoded) <= len,
                "base64 bound over-estimated at len {len}"
            );
            let text = "z".repeat(len);
            assert_eq!(min_decoded_len(FileEncoding::Utf8, &text), len);
        }
        // A file exactly at the limit must survive the pre-check in both
        // encodings.
        let at_limit = vec![b'z'; paths::MAX_FILE_BYTES];
        assert!(
            min_decoded_len(FileEncoding::Base64, &Base64::encode_string(&at_limit))
                <= paths::MAX_FILE_BYTES
        );
        assert!(
            min_decoded_len(FileEncoding::Utf8, &"z".repeat(paths::MAX_FILE_BYTES))
                <= paths::MAX_FILE_BYTES
        );
    }

    #[test]
    fn the_file_count_limit_counts_new_paths_only() {
        let files: Vec<(String, u64)> = (0..paths::MAX_FILES)
            .map(|i| (format!("site/f{i}.txt"), 1))
            .collect();
        let ws = workspace_with(
            &files
                .iter()
                .map(|(p, s)| (p.as_str(), *s))
                .collect::<Vec<_>>(),
        );
        assert_eq!(ws.files.len(), paths::MAX_FILES);
        assert_eq!(
            check_quotas(&ws, "site/one-more.txt", &WorkspaceArea::Site, 1),
            Err(QuotaError::TooManyFiles)
        );
        // Replacing a file the workspace already holds adds no path, so a
        // full workspace stays editable.
        assert_eq!(
            check_quotas(&ws, "site/f0.txt", &WorkspaceArea::Site, 1),
            Ok(())
        );
    }

    #[test]
    fn the_byte_limit_bounds_stored_blobs_not_the_live_manifest() {
        let ws = workspace_with(&[("site/a.css", paths::MAX_WORKSPACE_BYTES)]);
        assert_eq!(
            check_quotas(&ws, "site/b.css", &WorkspaceArea::Site, 1),
            Err(QuotaError::WorkspaceFull {
                would_be: paths::MAX_WORKSPACE_BYTES + 1
            })
        );
        // A write that stores nothing new — content some entry already names
        // — needs no headroom even at the limit.
        assert_eq!(
            check_quotas(&ws, "site/a.css", &WorkspaceArea::Site, 0),
            Ok(())
        );
        // Exactly at the limit is allowed.
        let ws = workspace_with(&[("site/a.css", paths::MAX_WORKSPACE_BYTES - 1)]);
        assert_eq!(
            check_quotas(&ws, "site/b.css", &WorkspaceArea::Site, 1),
            Ok(())
        );
    }

    /// The regression this quota exists for: content that is stored but no
    /// longer reachable still costs. A limit read off the live manifest would
    /// see an empty workspace here and wave the write through.
    #[test]
    fn unreferenced_blobs_still_count_against_the_limit() {
        let mut ws = Workspace::default();
        // 128 overwrites of a 512 KiB page, then a delete: nothing reachable,
        // 64 MiB stored.
        for _ in 0..(paths::MAX_WORKSPACE_BYTES / paths::MAX_FILE_BYTES as u64) {
            ws.record_blob_stored(paths::MAX_FILE_BYTES as u64);
        }
        assert!(ws.files.is_empty());
        assert_eq!(ws.total_bytes(), 0);
        assert_eq!(ws.blob_bytes, paths::MAX_WORKSPACE_BYTES);

        assert_eq!(
            check_quotas(&ws, "site/a.css", &WorkspaceArea::Site, 1),
            Err(QuotaError::WorkspaceFull {
                would_be: paths::MAX_WORKSPACE_BYTES + 1
            })
        );
        // Collecting one blob makes room again: the collector resets the
        // counters to what is left in the store.
        let (bytes, count) = (ws.blob_bytes, ws.blob_count);
        ws.reset_blob_totals(bytes - paths::MAX_FILE_BYTES as u64, count - 1);
        assert_eq!(
            check_quotas(&ws, "site/a.css", &WorkspaceArea::Site, 1),
            Ok(())
        );
    }

    #[test]
    fn the_block_limit_counts_distinct_blocks_not_files() {
        let files: Vec<(String, u64)> = (0..paths::MAX_BLOCKS)
            .map(|i| (format!("blocks/b{i}/src/lib.rs"), 1))
            .collect();
        let ws = workspace_with(
            &files
                .iter()
                .map(|(p, s)| (p.as_str(), *s))
                .collect::<Vec<_>>(),
        );
        assert_eq!(ws.block_names().len(), paths::MAX_BLOCKS);
        assert_eq!(
            check_quotas(
                &ws,
                "blocks/extra/src/lib.rs",
                &WorkspaceArea::Block("extra".to_string()),
                1
            ),
            Err(QuotaError::TooManyBlocks)
        );
        // A second file in a block that already exists is not a new block.
        assert_eq!(
            check_quotas(
                &ws,
                "blocks/b0/Cargo.toml",
                &WorkspaceArea::Block("b0".to_string()),
                1
            ),
            Ok(())
        );
        // Nor is a site file affected by the block count.
        assert_eq!(
            check_quotas(&ws, "site/a.css", &WorkspaceArea::Site, 1),
            Ok(())
        );
    }
}
