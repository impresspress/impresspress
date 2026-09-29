//! Object lifecycle: list, download (streamed), upload, delete. Bucket-name
//! extraction/validation and access control are shared with the bucket
//! handlers via [`super::params`] / [`super::validation`] / [`super::access`].

use wafer_core::clients::storage as store;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::{
    access::require_bucket_access,
    params::{extract_bucket_name, extract_object_key},
    validation::{is_valid_bucket_name, is_valid_storage_key},
};
use crate::{
    blocks::{
        crud,
        files::{
            contracts::{
                DeletedResponse, ObjectInfoResponse, ObjectListResponse, ObjectUploadedResponse,
            },
            repo::{
                self,
                objects::{Completion, ReserveError},
            },
        },
    },
    http::{err_bad_request, err_conflict, err_not_found, ok_json},
};

/// Why [`collect_with_cap`] produced no body.
#[derive(Debug)]
enum CappedBodyError {
    /// The running total passed the cap.
    TooLarge,
    /// The body did not arrive whole — the connection dropped, the transport
    /// hit its own cap or deadline. Carries the transport's error, whose code
    /// says which.
    Body(wafer_run::WaferError),
}

/// Collect an `InputStream` into `Vec<u8>` with a hard size cap. Errors out
/// as soon as the running total exceeds `cap_bytes`, so the copy this makes is
/// never larger than the cap.
///
/// A body that fails part-way is [`CappedBodyError::Body`], never the prefix
/// that arrived: storing that prefix would commit a truncated upload as the
/// user's file.
///
/// The transport-level ceiling on a request body is
/// [`crate::streaming::MAX_REQUEST_BODY_BYTES`], enforced before dispatch;
/// this cap is the caller's quota, which
/// [`crate::blocks::files::quota::get_user_quota`] has already clamped to that
/// ceiling.
async fn collect_with_cap(
    mut input: wafer_run::InputStream,
    cap_bytes: i64,
) -> Result<Vec<u8>, CappedBodyError> {
    use futures::StreamExt;
    let cap = if cap_bytes <= 0 {
        usize::MAX
    } else {
        cap_bytes as usize
    };
    let mut out = Vec::new();
    while let Some(chunk) = input.next().await {
        let chunk = chunk.map_err(CappedBodyError::Body)?;
        if out.len().saturating_add(chunk.len()) > cap {
            return Err(CappedBodyError::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

pub(in crate::blocks::files) async fn handle_list_objects(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let bucket = match extract_bucket_name(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !is_valid_bucket_name(bucket) {
        return err_bad_request("Invalid bucket name");
    }
    if let Err(refusal) = require_bucket_access(ctx, msg, bucket).await {
        return refusal;
    }

    let prefix = msg.query("prefix");
    let (_, page_size, offset) = msg.pagination_params(50);

    // The objects are the bucket's metadata rows, not a listing of storage:
    // storage holds each object's bytes under the key of the upload that
    // stored them (`repo::objects::claim_blob_key`), which is not the object
    // key and names no object a client uploaded.
    let page = match repo::objects::list_page_for_bucket(
        ctx,
        bucket,
        prefix,
        page_size as u32,
        offset as i64,
    )
    .await
    {
        Ok(page) => page,
        Err(e) => return crud::db_error_internal(e, "Object listing failed"),
    };
    let objects = match page.rows.into_iter().map(object_info).collect() {
        Ok(objects) => objects,
        Err(e) => return crud::db_error_internal(e, "Object listing failed"),
    };
    ok_json(&ObjectListResponse {
        objects,
        total_count: page.total,
    })
}

/// One listed object, as its row records it. `last_modified` is when the
/// upload that stored it began (`uploaded_at`), the instant the SSR object
/// browser shows as "modified"; a row with none — the column is nullable in
/// migration 001 — falls back to the row's own `updated_at`, which every
/// write stamps. A row with neither readable is reported, naming it.
fn object_info(row: repo::objects::ObjectRow) -> Result<ObjectInfoResponse, wafer_run::WaferError> {
    let parse = |stamp: &str| {
        chrono::DateTime::parse_from_rfc3339(stamp)
            .ok()
            .map(|t| t.with_timezone(&chrono::Utc))
    };
    let Some(last_modified) = parse(&row.uploaded_at).or_else(|| parse(&row.updated_at)) else {
        return Err(wafer_run::WaferError::new(
            ErrorCode::Internal,
            format!(
                "object row {} has no readable uploaded_at or updated_at ({:?}, {:?})",
                row.id, row.uploaded_at, row.updated_at
            ),
        ));
    };
    Ok(ObjectInfoResponse {
        key: row.key,
        size: row.size,
        content_type: row.content_type,
        last_modified,
    })
}

pub(in crate::blocks::files) async fn handle_get_object(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let bucket = match extract_bucket_name(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let key = match extract_object_key(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !is_valid_storage_key(key) {
        return err_bad_request("Invalid object key");
    }
    if let Err(refusal) = require_bucket_access(ctx, msg, bucket).await {
        return refusal;
    }

    // Track view in DB
    if let Err(e) = repo::views::insert(ctx, bucket, key, msg.user_id()).await {
        tracing::warn!("Failed to track storage object view: {e}");
    }

    // The bytes are wherever the object's row says they are: each upload
    // stores them under a key of its own. No row, no object.
    let blob_key = match repo::objects::find_blob_key(ctx, bucket, key).await {
        Ok(Some(blob_key)) => blob_key,
        Ok(None) => return err_not_found("Object not found"),
        Err(e) => return crud::db_error_internal(e, "Object lookup failed"),
    };

    // Stream the object body straight from storage (R2 `get_streaming` on CF)
    // rather than buffering the whole object into the isolate: `get_stream`
    // returns the `ObjectInfo` header eagerly, then the body flows chunk by
    // chunk. The leading meta carries the streaming opt-in marker + the real
    // content-type so the pipeline and platform adapter take the streaming
    // response path (see `crate::streaming`).
    //
    // The bytes and the content type are both an uploader's, and this route is
    // on the app's own origin, so the disposition and the security headers
    // come from [`crate::blocks::files::serving`] — the same builder the public
    // share link uses. The file name they carry is the object key, never the
    // blob key.
    match store::get_stream(ctx, bucket, &blob_key).await {
        Ok(stream) => {
            // The `application/octet-stream` a backend reporting no type used
            // to get here is not applied twice: the empty string is not a
            // media type, so `serving` substitutes it along with every other
            // type it cannot read.
            let leading = crate::blocks::files::serving::user_object_leading_meta(
                &stream.info().content_type.clone(),
                key,
                &[],
            );
            crate::streaming::stream_download(stream, leading)
        }
        Err(e) => crud::db_error(e, "Object not found", "Storage error"),
    }
}

pub(in crate::blocks::files) async fn handle_upload_object(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let bucket = match extract_bucket_name(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };

    let request_content_type = msg.get_meta("req.content_type").to_string();
    let is_multipart = crate::multipart::multipart_boundary(&request_content_type).is_some();

    let query_key = msg.query("key").to_string();
    // For raw-body uploads the key can only come from the URL, so its absence
    // is fatal before buffering anything. Multipart bodies carry a fallback
    // (the file part's filename), so that check happens after parsing below.
    if query_key.is_empty() && !is_multipart {
        return err_bad_request("Missing object key (pass as ?key=filename)");
    }
    if !query_key.is_empty() && !is_valid_storage_key(&query_key) {
        return err_bad_request("Invalid object key");
    }
    if let Err(refusal) = require_bucket_access(ctx, msg, bucket).await {
        return refusal;
    }

    // Best-effort sweep before the reservation: orphan `pending` rows (see
    // `sweep_stale_pending`) would otherwise inflate this user's quota usage
    // and lock them out.
    crate::blocks::files::quota::sweep_stale_pending(ctx, msg.user_id()).await;

    // Read the upload body under the user's per-file cap,
    // `max_file_size_bytes`, aborting as soon as the collected total exceeds
    // it — the one place that cap is enforced. For multipart bodies the cap
    // applies to the envelope — a slight over-estimate (the extracted file
    // is always smaller than its envelope), never an under-estimate. The
    // caps on what the user stores in total, `max_storage_bytes` and
    // `max_files_per_bucket`, are enforced by the reservation's write below.
    //
    // This is not a streaming upload and it is not a defence against a
    // multi-GB body: the transport has already read the whole request body
    // into memory under `streaming::MAX_REQUEST_BODY_BYTES` (and refused
    // anything larger with a 413), so the `InputStream` here replays bytes
    // that are already resident. `get_user_quota` clamps the per-file cap to
    // that same ceiling, so the size this refuses on is one an upload can
    // actually reach.
    let quota = match crate::blocks::files::quota::get_user_quota(ctx, msg.user_id()).await {
        Ok(quota) => quota,
        // Fail closed: reading the body against the default cap during an
        // outage would admit a file an admin-lowered override forbids.
        Err(e) => return crud::db_error_internal(e, "Quota lookup failed"),
    };
    let body_bytes = match collect_with_cap(input, quota.max_file_size_bytes).await {
        Ok(body_bytes) => body_bytes,
        Err(CappedBodyError::TooLarge) => {
            return err_bad_request(&format!(
                "File exceeds maximum size of {} bytes",
                quota.max_file_size_bytes
            ));
        }
        Err(CappedBodyError::Body(e)) => return OutputStream::error(e),
    };

    // Browser uploads (`FormData` + fetch) arrive as `multipart/form-data`:
    // the body is a boundary envelope AROUND the file, not the file itself.
    // Extract the file part and store ITS bytes/content type/size — storing
    // the raw body would corrupt the object (the pre-fix behavior). Raw-body
    // uploads (programmatic clients POSTing the bytes directly) keep the
    // body as the content.
    let (content, key, content_type) = if is_multipart {
        let Some(file) =
            crate::multipart::extract_multipart_file(&body_bytes, &request_content_type)
        else {
            return err_bad_request("Multipart body contains no file part");
        };
        let key = if query_key.is_empty() {
            file.filename.unwrap_or_default()
        } else {
            query_key
        };
        if key.is_empty() {
            return err_bad_request("Missing object key (pass as ?key=filename)");
        }
        if !is_valid_storage_key(&key) {
            return err_bad_request("Invalid object key");
        }
        // The part's own Content-Type wins; fall back to extension-based
        // detection on the key (which itself falls back to octet-stream).
        let content_type = file
            .content_type
            .filter(|ct| !ct.is_empty())
            .unwrap_or_else(|| {
                wafer_core::mime::mime_for_ext(std::path::Path::new(&key)).to_string()
            });
        (file.content, key, content_type)
    } else {
        let content_type = if request_content_type.is_empty() {
            "application/octet-stream".to_string()
        } else {
            request_content_type
        };
        (body_bytes, query_key, content_type)
    };

    // Claim the key BEFORE uploading, within the user's caps: the
    // reservation's write is refused when it would take them past
    // `max_storage_bytes` or `max_files_per_bucket`, as one atomic step, so
    // concurrent uploads cannot all be admitted against the same usage. A
    // re-upload of a key takes over its one row (`(bucket, key)` is UNIQUE)
    // and is charged the difference when that row is the user's own; an
    // upload that finds another upload of the key still in flight is refused
    // (see `reserve_upload`).
    let reservation = match repo::objects::reserve_upload(
        ctx,
        bucket,
        &key,
        content.len(),
        &content_type,
        msg.user_id(),
        &quota,
    )
    .await
    {
        Ok(reservation) => reservation,
        Err(ReserveError::OverStorageQuota) => return err_bad_request("Storage quota exceeded"),
        Err(ReserveError::OverFileCount) => {
            return err_bad_request(&format!(
                "File count limit reached for this bucket (max {})",
                quota.max_files_per_bucket
            ))
        }
        // Another upload of the key holds it (see `reserve_upload`): a
        // conflict the client resolves by retrying once that upload settles,
        // not a fault. The messages are this handler's own, so no backend
        // text reaches the client.
        Err(ReserveError::Held) => {
            return err_conflict(
                "Another upload of this key is in progress; retry once it finishes",
            )
        }
        // This user's own earlier upload holds it — most often one that
        // answered "Upload stored but could not be recorded" below. Saying
        // "another upload" would send them looking for an upload that is not
        // there, so say what holds the key and when it is released.
        Err(ReserveError::HeldByOwnEarlierUpload { since }) => {
            return err_conflict(&format!(
                "Your earlier upload of this key, started at {since}, has not been \
                 recorded: it is still in progress, or it was stored but could not be \
                 recorded. The key is released {} minutes after that upload started; \
                 retry then",
                repo::objects::PENDING_RESERVATION_TTL_SECONDS / 60
            ))
        }
        // `db_error_internal`, not a bare `err_internal`: a WRAP refusal is a
        // 403 and a quota is a 429, and folding either into a 500 is what left
        // an operator unable to tell a missing grant from a broken row.
        Err(ReserveError::Db(e)) => {
            return crud::db_error_internal(e, "Failed to reserve upload slot")
        }
    };

    // Under this reservation's own blob key, never the object key: an upload
    // that loses the key to another (its reservation outlived its TTL) and
    // stores its bytes late must land them where no row points, not over the
    // bytes of the upload that took the key over.
    match store::put(ctx, bucket, &reservation.blob_key, &content, &content_type).await {
        Ok(()) => {
            // The row is what charges quota and what the object listings read,
            // so an upload that cannot be recorded is not an upload. Left as
            // `pending` it is swept within the hour, and answering
            // `uploaded: true` anyway is how a stored object came to be
            // charged to nobody. Report it instead: the blob is in place, but
            // the row stays `pending`, so the key stays held — a retry by this
            // user is told so (`ReserveError::HeldByOwnEarlierUpload`) — until
            // `sweep_stale_pending` clears it and a retry can claim it.
            match repo::objects::mark_complete(ctx, &reservation).await {
                // The row names this upload's blob now, so the blobs it
                // superseded are named by nothing.
                Ok(Completion::Completed) => {
                    delete_blobs(ctx, bucket, &reservation.superseded_blobs).await;
                }
                // Another upload holds the key now: this reservation outlived
                // its TTL and was taken over, or the object was deleted and
                // the key claimed again. The bytes just stored are under this
                // reservation's own blob key, which no row names, so they are
                // deleted — whatever the other upload has stored is not.
                Ok(Completion::TakenOver) => {
                    delete_blobs(ctx, bucket, &[reservation.blob_key.as_str()]).await;
                    return err_conflict(
                        "Another upload now holds this key, so this upload was not \
                         recorded; retry",
                    );
                }
                // The object or its bucket was deleted mid-upload. The bytes
                // just stored are recorded nowhere, so they are not kept.
                Ok(Completion::Deleted) => {
                    delete_blobs(ctx, bucket, &[reservation.blob_key.as_str()]).await;
                    return err_conflict(
                        "The object or its bucket was deleted while this upload was in \
                         progress, so the upload was not recorded",
                    );
                }
                // The bytes are stored under this reservation's blob key and
                // nothing is recorded. While the row still carries this
                // reservation, the pending sweep reclaims that blob with the
                // row (`StoredRow::blobs`). If another upload has taken the
                // row meanwhile, no row will ever name the blob and the sweep
                // cannot find it — finding it would mean listing storage for
                // blobs no row names — so it is logged here, with its key.
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        bucket,
                        blob = %reservation.blob_key,
                        "upload stored but not recorded; if its reservation was taken over, this blob is named by no row"
                    );
                    return crud::db_error_internal(e, "Upload stored but could not be recorded");
                }
            }
            ok_json(&ObjectUploadedResponse {
                bucket: bucket.to_string(),
                key: key.to_string(),
                uploaded: true,
            })
        }
        Err(e) => {
            // Upload failed — give the claim up so it doesn't block quota. For
            // a replacement that means putting the previous object's row back:
            // its blob is untouched (this upload wrote under its own key), so
            // it must keep being described and charged. Only once the row is
            // settled are the blobs it no longer names deleted; a claim this
            // upload lost settles nothing and leaves every blob but its own
            // to the upload that holds the key.
            match repo::objects::release_reservation(ctx, &reservation).await {
                Ok(()) => {
                    delete_blobs(ctx, bucket, &reservation.blobs_released_by_rollback()).await;
                }
                Err(release_err) => {
                    tracing::warn!("Failed to release upload reservation: {release_err}");
                    delete_blobs(ctx, bucket, &[reservation.blob_key.as_str()]).await;
                }
            }
            crud::db_error_internal(e, "Upload failed")
        }
    }
}

/// Delete `blobs` from `bucket`'s storage, best effort: a blob already gone
/// is what the caller wanted, and any other failure is logged and left, the
/// caller having already decided its answer.
///
/// Only for blob keys no row names — a settled upload's superseded blobs, a
/// lost claim's own, a deleted row's — which is what makes deleting them
/// safe without holding the object key: no other upload stores under them.
pub(in crate::blocks::files) async fn delete_blobs<B: AsRef<str>>(
    ctx: &dyn Context,
    bucket: &str,
    blobs: &[B],
) {
    for blob in blobs {
        let blob = blob.as_ref();
        match store::delete(ctx, bucket, blob).await {
            Ok(()) => {}
            Err(e) if e.code == ErrorCode::NotFound => {}
            // Nothing names this blob any more, so nothing will retry it:
            // error level, with its key, is how it is found.
            Err(e) => {
                tracing::error!(error = %e, bucket, blob, "could not delete a blob no object names; it is left in storage");
            }
        }
    }
}

/// How many times [`handle_delete_object`] re-reads a row that changed
/// under it before answering `409`. A change is an upload of the same key
/// settling in the window between the read and the delete, so a second
/// round almost always finds the row still.
const DELETE_ATTEMPTS: usize = 3;

pub(in crate::blocks::files) async fn handle_delete_object(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let bucket = match extract_bucket_name(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let key = match extract_object_key(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !is_valid_storage_key(key) {
        return err_bad_request("Invalid object key");
    }
    if let Err(refusal) = require_bucket_access(ctx, msg, bucket).await {
        return refusal;
    }

    // The row says which blobs are the object's (`StoredRow::blobs`). No row,
    // no object.
    //
    // The row goes first, conditional on it being as it was read — an upload
    // of the key that settles in between points it at a blob this read did
    // not see, and the next round reads it again — and only then its blobs.
    // In that order no path leaves a row naming a deleted blob: a row that
    // changed under every round is left whole, still served and charged. A
    // blob whose delete fails after its row is gone is named by nothing; it
    // is logged at error level by `delete_blobs` so it can be found.
    for _ in 0..DELETE_ATTEMPTS {
        let stored = match repo::objects::find_stored(ctx, bucket, key).await {
            Ok(Some(stored)) => stored,
            Ok(None) => return err_not_found("Object not found"),
            Err(e) => return crud::db_error_internal(e, "Object lookup failed"),
        };
        match repo::objects::delete_if_unchanged(ctx, &stored).await {
            Ok(true) => {
                delete_blobs(ctx, bucket, &stored.blobs()).await;
                return ok_json(&DeletedResponse { deleted: true });
            }
            Ok(false) => {}
            Err(e) => return crud::db_error_internal(e, "Delete failed"),
        }
    }
    err_conflict("The object changed while it was being deleted; retry")
}

#[cfg(test)]
mod integration_tests {
    use super::{
        super::test_helpers::{
            ctx_with_storage, ctx_with_storage_handle, seed_bucket, seed_object_row,
        },
        *,
    };
    use crate::{
        blocks::files::{
            contracts::ObjectStatus,
            test_support::{seed_legacy_object, stored_object},
        },
        test_support::{auth_msg, output_is_error, output_json, FailingDbOpContext, TestContext},
    };

    /// Collect the body bytes a download `OutputStream` carried, failing with
    /// the stream's error message when it errored instead of serving.
    async fn download_body(out: OutputStream) -> Vec<u8> {
        use futures::StreamExt;
        use wafer_block::stream::StreamEvent;

        let mut body = Vec::new();
        let mut events = out;
        while let Some(evt) = events.next().await {
            match evt {
                StreamEvent::Chunk(bytes) => body.extend_from_slice(&bytes),
                StreamEvent::Error(e) => {
                    panic!("download errored instead of serving bytes: {}", e.message)
                }
                _ => {}
            }
        }
        body
    }

    /// Upload an object through the real upload handler, then download it
    /// through the real download handler and assert the bytes come back.
    ///
    /// The upload is a buffered `storage.put` and the download is
    /// `storage.get_streaming` (`store::get_stream`, from
    /// `handle_get_object`), so both ops must reach the backend through the
    /// registered `wafer-run/storage` block; a test that seeds with
    /// `store::put` and reads with `store::get` would not exercise the op a
    /// download issues.
    ///
    /// Asserting "not an error" would not have been enough either: the bytes
    /// are the contract, so they are what this asserts.
    #[tokio::test]
    async fn uploaded_object_downloads_back_the_same_bytes() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let file_bytes: &[u8] = b"the exact bytes a user uploaded\x00\x01\x02\xff";
        let upload = handle_upload_object(
            &ctx,
            &upload_msg("assets", "report.bin", "application/octet-stream"),
            InputStream::from_bytes(file_bytes.to_vec()),
        )
        .await;
        assert_eq!(
            output_json(upload).await["uploaded"],
            serde_json::json!(true),
            "the upload half of the round trip must succeed"
        );

        let mut download_msg = auth_msg(
            "retrieve",
            "/b/storage/api/buckets/assets/objects/report.bin",
            "alice",
        );
        download_msg.set_meta("req.param.name", "assets");
        download_msg.set_meta("req.param.key", "report.bin");

        let body = download_body(handle_get_object(&ctx, &download_msg).await).await;

        assert_eq!(
            body, file_bytes,
            "the download must return the uploaded bytes"
        );
    }

    /// A download served via `handle_get_object` must take the STREAMING
    /// response shape: the `resp.stream` opt-in marker and the object's real
    /// content-type are emitted as **leading `Meta`** events (before the first
    /// body `Chunk`), and the body bytes are forwarded verbatim. This is what
    /// makes the pipeline + platform adapter stream the object instead of
    /// buffering it whole in the isolate. (`MemStorage` uses the default
    /// `get_streaming`, so this exercises the handler's framing end-to-end
    /// through the real `wafer-run/storage` wire protocol.)
    #[tokio::test]
    async fn get_object_streams_body_with_leading_meta_marker() {
        use futures::StreamExt;
        use wafer_block::stream::StreamEvent;
        use wafer_run::{MetaEntry, MetaGet, META_RESP_CONTENT_TYPE};

        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        seed_legacy_object(&ctx, "assets", "pic.png", b"PNGDATA", "image/png", "alice").await;

        let mut msg = auth_msg(
            "retrieve",
            "/b/storage/api/buckets/assets/objects/pic.png",
            "alice",
        );
        msg.set_meta("req.param.name", "assets");
        msg.set_meta("req.param.key", "pic.png");

        let events: Vec<StreamEvent> = handle_get_object(&ctx, &msg).await.collect().await;

        // Leading meta must PRECEDE the first body chunk (the streaming shape).
        let first_chunk = events
            .iter()
            .position(|e| matches!(e, StreamEvent::Chunk(_)))
            .expect("a body chunk must be streamed");
        let leading: Vec<MetaEntry> = events[..first_chunk]
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Meta(m) => Some(m.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            MetaGet::get(&leading, crate::streaming::META_RESP_STREAM),
            Some(crate::streaming::STREAM_MARKER_VALUE),
            "download must emit the streaming opt-in marker as leading meta"
        );
        assert_eq!(
            MetaGet::get(&leading, META_RESP_CONTENT_TYPE),
            Some("image/png"),
            "download must emit the object's real content-type as leading meta"
        );

        let body: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Chunk(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            body, b"PNGDATA",
            "the object body must be streamed verbatim"
        );
    }

    /// Build a browser-shaped `multipart/form-data` envelope around
    /// `file_bytes` (one `name="file"` part carrying `filename` +
    /// `Content-Type: text/html`), mirroring what `FormData` + fetch send.
    fn multipart_envelope(boundary: &str, filename: &str, file_bytes: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: text/html\r\n\r\n");
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        body
    }

    /// Build the upload request message the router would produce for
    /// `POST /b/storage/api/buckets/{bucket}/objects?key={key}`.
    fn upload_msg(bucket: &str, key: &str, content_type: &str) -> Message {
        let mut msg = auth_msg(
            "create",
            &format!("/b/storage/api/buckets/{bucket}/objects"),
            "alice",
        );
        msg.set_meta("req.param.name", bucket);
        if !key.is_empty() {
            msg.set_meta("req.query.key", key);
        }
        msg.set_meta("req.content_type", content_type);
        msg
    }

    /// Build the message the router produces for
    /// `DELETE /b/storage/api/buckets/{bucket}/objects/{key}`.
    fn delete_msg(bucket: &str, key: &str) -> Message {
        let mut msg = auth_msg(
            "delete",
            &format!("/b/storage/api/buckets/{bucket}/objects/{key}"),
            "alice",
        );
        msg.set_meta("req.param.name", bucket);
        msg.set_meta("req.param.key", key);
        msg
    }

    /// One stored object with its metadata row, owned by `alice`.
    async fn ctx_with_stored_object() -> TestContext {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        store::put(&ctx, "assets", "pic.png", b"PNGDATA", "image/png")
            .await
            .expect("seed object");
        seed_object_row(&ctx, "assets", "pic.png", "alice", 7).await;
        ctx
    }

    /// Both wire ops a filtered metadata delete can use, so the fault
    /// matches whichever the repository issues.
    fn object_row_delete_ops() -> Vec<(&'static str, &'static str)> {
        vec![
            ("database.delete_where", repo::objects::TABLE),
            ("database.delete_where_count", repo::objects::TABLE),
        ]
    }

    #[tokio::test]
    async fn delete_object_removes_blob_and_metadata_row() {
        let ctx = ctx_with_stored_object().await;

        let out = handle_delete_object(&ctx, &delete_msg("assets", "pic.png")).await;

        assert_eq!(output_json(out).await["deleted"], serde_json::json!(true));
        assert!(
            store::get(&ctx, "assets", "pic.png").await.is_err(),
            "the blob must be gone"
        );
        assert_eq!(
            repo::objects::count_for_uploader(&ctx, "alice")
                .await
                .expect("count"),
            0,
            "the metadata row must be gone"
        );
    }

    /// A surviving row keeps charging the uploader's quota for a blob that
    /// no longer exists, so a failed cleanup is reported, never swallowed.
    #[tokio::test]
    async fn delete_object_reports_metadata_cleanup_failure() {
        let ctx = ctx_with_stored_object().await;
        let failing = FailingDbOpContext::new(ctx.clone(), object_row_delete_ops());

        let out = handle_delete_object(&failing, &delete_msg("assets", "pic.png")).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a metadata cleanup failure must not be reported as a successful delete"
        );
    }

    /// A delete whose row delete failed leaves the object whole — row and
    /// blob — so it is still served, and a retry finishes the delete.
    #[tokio::test]
    async fn delete_object_retry_finishes_cleanup_after_partial_failure() {
        let ctx = ctx_with_stored_object().await;
        let failing = FailingDbOpContext::new(ctx.clone(), object_row_delete_ops());
        let first = handle_delete_object(&failing, &delete_msg("assets", "pic.png")).await;
        assert!(output_is_error(first, "Internal").await);
        assert_eq!(
            download_body(handle_get_object(&ctx, &download_msg("assets", "pic.png")).await).await,
            b"PNGDATA",
            "the failed delete left the object served"
        );

        let retry = handle_delete_object(&ctx, &delete_msg("assets", "pic.png")).await;

        assert_eq!(
            output_json(retry).await["deleted"],
            serde_json::json!(true),
            "the retry must complete the cleanup"
        );
        assert_eq!(
            repo::objects::count_for_uploader(&ctx, "alice")
                .await
                .expect("count"),
            0
        );
    }

    #[tokio::test]
    async fn delete_missing_object_is_not_found() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let out = handle_delete_object(&ctx, &delete_msg("assets", "missing.png")).await;

        assert!(output_is_error(out, "NotFound").await);
    }

    /// Fetch the single object-metadata row (asserting there is exactly
    /// one) and return its `(size, content_type, status)`.
    async fn sole_object_row(ctx: &TestContext) -> (i64, String, ObjectStatus) {
        let rows = repo::objects::list_all(ctx)
            .await
            .expect("list object rows");
        assert_eq!(rows.len(), 1, "expected exactly one object metadata row");
        let row = &rows[0];
        (row.size, row.content_type.clone(), row.status)
    }

    /// CRUX regression (found by driving the live app): a browser `FormData`
    /// upload arrives as `multipart/form-data`, and the handler used to store
    /// the RAW multipart envelope as the object content — every browser
    /// upload was corrupted (serving the file returned the envelope, and the
    /// recorded `size` was the envelope size). The handler must store the
    /// extracted FILE PART bytes, the part's content type, and the real
    /// content length.
    #[tokio::test]
    async fn upload_multipart_stores_file_bytes_not_envelope() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "site-assets", "alice").await;

        // An HTML *fragment* (no doctype/page-root tags): storage is
        // content-agnostic, so keeping page-chrome markers out of the fixture
        // keeps the coarse `scripts/grep-guard-html.sh` guard happy.
        let file_bytes: &[u8] = b"<h1>hello from impresspress</h1>\n<p>an uploaded page</p>\n";
        let boundary = "----WebKitFormBoundaryqHHDhrDMqZoc7sHW";
        let envelope = multipart_envelope(boundary, "index.html", file_bytes);
        assert!(
            envelope.len() > file_bytes.len(),
            "envelope must be strictly larger than the file for the size assertion to bite"
        );

        let msg = upload_msg(
            "site-assets",
            "index.html",
            &format!("multipart/form-data; boundary={boundary}"),
        );
        let out = handle_upload_object(&ctx, &msg, InputStream::from_bytes(envelope)).await;
        let resp = output_json(out).await;
        assert_eq!(
            resp.get("uploaded").and_then(|v| v.as_bool()),
            Some(true),
            "upload failed: {resp}"
        );

        let (stored, info) = stored_object(&ctx, "site-assets", "index.html")
            .await
            .expect("stored object");
        assert_eq!(
            stored, file_bytes,
            "stored content must be the file bytes, not the multipart envelope"
        );
        assert_eq!(
            info.content_type, "text/html",
            "stored content type must come from the file part, not the multipart request header"
        );

        let (size, content_type, status) = sole_object_row(&ctx).await;
        assert_eq!(
            size,
            file_bytes.len() as i64,
            "metadata size must be the extracted content length, not the envelope length"
        );
        assert_eq!(content_type, "text/html");
        assert_eq!(status, ObjectStatus::Complete);
    }

    /// Non-multipart (raw body) uploads keep the existing behavior: the body
    /// IS the content — programmatic clients that POST raw bytes with a
    /// concrete content type must not regress.
    #[tokio::test]
    async fn upload_raw_body_stores_body_as_is() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "raw-bucket", "alice").await;

        let body: &[u8] = b"plain bytes, no envelope";
        let msg = upload_msg("raw-bucket", "notes.txt", "text/plain");
        let out = handle_upload_object(&ctx, &msg, InputStream::from_bytes(body.to_vec())).await;
        let resp = output_json(out).await;
        assert_eq!(
            resp.get("uploaded").and_then(|v| v.as_bool()),
            Some(true),
            "upload failed: {resp}"
        );

        let (stored, info) = stored_object(&ctx, "raw-bucket", "notes.txt")
            .await
            .expect("stored object");
        assert_eq!(stored, body, "raw body must be stored unchanged");
        assert_eq!(info.content_type, "text/plain");

        let (size, content_type, status) = sole_object_row(&ctx).await;
        assert_eq!(size, body.len() as i64);
        assert_eq!(content_type, "text/plain");
        assert_eq!(status, ObjectStatus::Complete);
    }

    /// A body that fails part-way — the connection dropped, the transport hit
    /// its deadline — is not the file. The upload answers the transport's own
    /// error and stores neither the prefix that arrived nor an object row.
    #[tokio::test]
    async fn an_upload_whose_body_fails_stores_nothing() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "raw-bucket", "alice").await;

        let body = InputStream::from_stream(futures::stream::iter([
            Ok(b"the first half of the file".to_vec()),
            Err(wafer_run::WaferError::new(
                wafer_run::ErrorCode::DeadlineExceeded,
                "request body read timed out",
            )),
        ]));
        let msg = upload_msg("raw-bucket", "notes.txt", "text/plain");
        let out = handle_upload_object(&ctx, &msg, body).await;

        assert!(
            crate::test_support::output_is_error(out, "DeadlineExceeded").await,
            "the transport's error must be the answer"
        );
        assert!(
            stored_object(&ctx, "raw-bucket", "notes.txt")
                .await
                .is_err(),
            "the prefix must not be stored"
        );
        let rows = repo::objects::list_all(&ctx)
            .await
            .expect("list object rows");
        assert!(rows.is_empty(), "no object row may be written");
    }

    /// Build the message the router produces for
    /// `GET /b/storage/api/buckets/{bucket}/objects/{key}`.
    fn download_msg(bucket: &str, key: &str) -> Message {
        let mut msg = auth_msg(
            "retrieve",
            &format!("/b/storage/api/buckets/{bucket}/objects/{key}"),
            "alice",
        );
        msg.set_meta("req.param.name", bucket);
        msg.set_meta("req.param.key", key);
        msg
    }

    /// The response headers a download emitted, as leading meta (the frame
    /// that precedes the first body chunk — the streaming response shape).
    async fn download_headers(out: OutputStream) -> Vec<wafer_run::MetaEntry> {
        use futures::StreamExt;
        use wafer_block::stream::StreamEvent;

        let events: Vec<StreamEvent> = out.collect().await;
        let first_chunk = events
            .iter()
            .position(|e| matches!(e, StreamEvent::Chunk(_)))
            .expect("a body chunk must be streamed");
        events[..first_chunk]
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Meta(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    fn header<'m>(meta: &'m [wafer_run::MetaEntry], name: &str) -> Option<&'m str> {
        wafer_run::MetaGet::get(meta, &format!("resp.header.{name}"))
    }

    /// Stored XSS: an uploader picks the content type, the bytes are theirs,
    /// and this route serves both from the app's own origin. Uploading an HTML
    /// page and opening its download URL used to render that page on the
    /// origin — no `Content-Disposition`, no `nosniff` — so any script in it
    /// ran with the viewer's session.
    ///
    /// The upload is the one a browser sends (a `multipart/form-data`
    /// envelope with the part's own `Content-Type: text/html`), and the
    /// download is the real handler.
    #[tokio::test]
    async fn an_uploaded_html_page_is_served_as_an_inert_attachment() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let boundary = "XBOUNDARYX";
        let envelope = multipart_envelope(boundary, "payload.html", b"<h1>not a page</h1>");
        let upload = handle_upload_object(
            &ctx,
            &upload_msg(
                "assets",
                "payload.html",
                &format!("multipart/form-data; boundary={boundary}"),
            ),
            InputStream::from_bytes(envelope),
        )
        .await;
        assert_eq!(
            output_json(upload).await["uploaded"],
            serde_json::json!(true)
        );

        let meta = download_headers(
            handle_get_object(&ctx, &download_msg("assets", "payload.html")).await,
        )
        .await;

        assert_eq!(
            header(&meta, "Content-Disposition"),
            Some("attachment; filename=\"payload.html\""),
            "an uploaded HTML page must be downloaded, never rendered on this origin",
        );
        assert_eq!(
            header(&meta, "X-Content-Type-Options"),
            Some("nosniff"),
            "without nosniff the declared type is only a suggestion",
        );
        assert!(
            header(&meta, "Content-Security-Policy").is_some_and(|csp| csp.contains("sandbox")),
            "an attachment a browser renders anyway must render sandboxed: {meta:?}",
        );
    }

    /// The allowlist is what makes the fix compatible with previews: an image
    /// is still served inline, and still with `nosniff` — which is what stops
    /// an HTML body uploaded as `image/png` from being sniffed back into a
    /// page.
    #[tokio::test]
    async fn an_image_still_previews_inline_with_nosniff() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        seed_legacy_object(&ctx, "assets", "pic.png", b"PNGDATA", "image/png", "alice").await;

        let meta =
            download_headers(handle_get_object(&ctx, &download_msg("assets", "pic.png")).await)
                .await;

        assert_eq!(
            header(&meta, "Content-Disposition"),
            Some("inline; filename=\"pic.png\"")
        );
        assert_eq!(header(&meta, "X-Content-Type-Options"), Some("nosniff"));
    }

    /// Nothing restricts an object key to ASCII — `is_valid_storage_key` bans
    /// `..`, backslash, NUL and a leading `/`, and that is all — so a
    /// non-ASCII filename is an ordinary upload. This route had no
    /// `Content-Disposition` at all before it gained one, and on Cloudflare
    /// `Headers.set` throws above U+00FF, so an ASCII-only header would have
    /// turned such a download into a 500. Round-trip one through both real
    /// handlers and assert the header is ASCII and carries the real name.
    #[tokio::test]
    async fn a_non_ascii_key_downloads_with_an_ascii_header_that_still_names_it() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let key = "日本語 memo.txt";
        let upload = handle_upload_object(
            &ctx,
            &upload_msg("assets", key, "text/plain"),
            InputStream::from_bytes(b"bytes".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(upload).await["uploaded"],
            serde_json::json!(true)
        );

        let out = handle_get_object(&ctx, &download_msg("assets", key)).await;
        let events: Vec<wafer_block::stream::StreamEvent> = futures::StreamExt::collect(out).await;
        let first_chunk = events
            .iter()
            .position(|e| matches!(e, wafer_block::stream::StreamEvent::Chunk(_)))
            .expect("the object must still be served, not 500");
        let meta: Vec<wafer_run::MetaEntry> = events[..first_chunk]
            .iter()
            .filter_map(|e| match e {
                wafer_block::stream::StreamEvent::Meta(m) => Some(m.clone()),
                _ => None,
            })
            .collect();

        let disposition = header(&meta, "Content-Disposition").expect("a disposition");
        assert!(
            disposition.contains("filename*=UTF-8''%E6%97%A5%E6%9C%AC%E8%AA%9E%20memo.txt"),
            "the real name must survive as RFC 6266 `filename*`: {disposition}"
        );
        assert!(
            meta.iter().all(|e| e.value.is_ascii()),
            "every header value must be ASCII or the Workers runtime throws: {meta:?}"
        );
    }

    /// **Fails on the pre-fix tree.** The stored per-file quota is 100 MiB and
    /// no transport will carry a request body over
    /// `streaming::MAX_REQUEST_BODY_BYTES` (10 MiB), so an upload between the
    /// two was refused by the transport — as an opaque 500 with a correlation
    /// id on Cloudflare — while this handler, and everything that reports the
    /// limit, still described 100 MiB as allowed. `get_user_quota` now clamps
    /// the per-file cap to the transport ceiling, so the size the block
    /// refuses on and the size it advertises are the same number.
    #[tokio::test]
    async fn an_upload_over_the_transport_cap_is_refused_against_the_enforced_limit() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let body = vec![b'x'; crate::streaming::MAX_REQUEST_BODY_BYTES + 1];
        let out = handle_upload_object(
            &ctx,
            &upload_msg("assets", "big.bin", "application/octet-stream"),
            InputStream::from_bytes(body),
        )
        .await;

        let rendered = crate::test_support::output_http_json(out).await;
        assert_eq!(rendered["error"], serde_json::json!("InvalidArgument"));
        assert_eq!(
            rendered["message"],
            serde_json::json!(format!(
                "File exceeds maximum size of {} bytes",
                crate::streaming::MAX_REQUEST_BODY_BYTES
            )),
            "the refusal must name the limit that is enforced, not the stored 100 MiB: {rendered}"
        );
        assert!(
            storage.blob_keys("assets").is_empty(),
            "nothing may be stored for a refused upload"
        );
    }

    /// `(bucket, key)` is one object, so re-uploading a key REPLACES what is
    /// stored there: the row is pointed at the new upload's bytes. The
    /// metadata row is the same row — inserting a second one is refused by
    /// the unique index, which a re-upload must not answer with a 500.
    #[tokio::test]
    async fn re_uploading_an_existing_key_replaces_the_object() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let first = handle_upload_object(
            &ctx,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(b"version one".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(first).await["uploaded"],
            serde_json::json!(true)
        );

        let second = handle_upload_object(
            &ctx,
            &upload_msg("assets", "notes.txt", "text/markdown"),
            InputStream::from_bytes(b"version two, longer".to_vec()),
        )
        .await;

        assert_eq!(
            output_json(second).await["uploaded"],
            serde_json::json!(true),
            "re-uploading a key the user already owns must replace it, not 500",
        );
        let (stored, info) = stored_object(&ctx, "assets", "notes.txt")
            .await
            .expect("stored object");
        assert_eq!(stored, b"version two, longer");
        assert_eq!(info.content_type, "text/markdown");

        let (size, content_type, status) = sole_object_row(&ctx).await;
        assert_eq!(
            size,
            "version two, longer".len() as i64,
            "the row must describe the object that is stored now",
        );
        assert_eq!(content_type, "text/markdown");
        assert_eq!(status, ObjectStatus::Complete);
    }

    /// One racer of [`race_two_uploads`]: its blob write is held until BOTH
    /// racers are through their reservation — each has either reached its own
    /// write or answered without one.
    ///
    /// Without the hold, the upload that claimed the key can store its bytes
    /// and mark its row `Complete` before the other has finished reserving,
    /// and the other then finds a stored object where it lost a race. Taking
    /// that object over is a replacement, correctly answered 200 — the two
    /// uploads ran one after the other, not against each other — so the race
    /// under test would not have been run at all.
    #[derive(Clone)]
    struct WriteHeldUntilBothReserved {
        inner: crate::test_support::RendezvousDbOpContext,
        /// How many racers are through their reservation. Shared by both.
        reserved: std::sync::Arc<tokio::sync::watch::Sender<usize>>,
        /// Whether THIS racer has been counted in `reserved`. Shared by this
        /// racer's clones only.
        counted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl WriteHeldUntilBothReserved {
        /// Count this racer as through its reservation, once.
        fn through_reservation(&self) {
            if !self.counted.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.reserved.send_modify(|n| *n += 1);
            }
        }
    }

    #[async_trait::async_trait]
    impl wafer_run::context::Context for WriteHeldUntilBothReserved {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), wafer_run::WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if name == "wafer-run/storage" && msg.action() == wafer_block::ServiceOp::STORAGE_PUT {
                self.through_reservation();
                self.reserved
                    .subscribe()
                    .wait_for(|reserved| *reserved == 2)
                    .await
                    .expect("the racers hold the sender");
            }
            self.inner.call_block(name, msg, input).await
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    /// Two uploads of `assets/same.txt` that the database interleaves: both
    /// are held at the reservation's read until both have made it, so both
    /// reserve on the same view of the key's row, and neither stores its
    /// bytes until both are through the reservation
    /// ([`WriteHeldUntilBothReserved`]).
    ///
    /// Each upload lists the table twice — the pending sweep
    /// (`quota::sweep_stale_pending`), then the reservation's read of the
    /// key's row — so each racer lets its own first list through
    /// ([`RendezvousDbOpContext::passing_first`]) and is held on the second. Names `repo::objects::TABLE` only to aim
    /// the rendezvous. Answers each racer's HTTP status, in `uploads` order.
    async fn race_two_uploads(
        ctx: &TestContext,
        uploads: [(&'static [u8], &'static str); 2],
    ) -> Vec<u16> {
        use crate::test_support::{output_http_status, RendezvousDbOpContext};

        let gated =
            RendezvousDbOpContext::new(ctx.clone(), "database.list", repo::objects::TABLE, 2);
        let reserved = std::sync::Arc::new(tokio::sync::watch::Sender::new(0));
        let racers: Vec<_> = uploads
            .into_iter()
            .map(|(bytes, content_type)| {
                let racer = WriteHeldUntilBothReserved {
                    inner: gated.passing_first(1),
                    reserved: reserved.clone(),
                    counted: std::sync::Arc::default(),
                };
                tokio::spawn(async move {
                    let out = handle_upload_object(
                        &racer,
                        &upload_msg("assets", "same.txt", content_type),
                        InputStream::from_bytes(bytes.to_vec()),
                    )
                    .await;
                    // Answered without storing anything: through its
                    // reservation all the same, so the rival may write.
                    racer.through_reservation();
                    output_http_status(out).await
                })
            })
            .collect();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            futures::future::try_join_all(racers),
        )
        .await
        .expect("both uploads must reach the rendezvous and finish")
        .expect("upload task panicked")
    }

    /// After a race: exactly one racer got a 200 and the other a 409 — the
    /// key was taken, not a fault — and the key's one row describes the bytes
    /// that are stored, which are the winner's.
    async fn assert_one_upload_won(
        ctx: &TestContext,
        uploads: [(&'static [u8], &'static str); 2],
        statuses: &[u16],
    ) {
        let mut sorted = statuses.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![200, 409],
            "one upload claims the key; the other is told it is taken, not 500"
        );
        let (winner_bytes, winner_type) = uploads[statuses.iter().position(|s| *s == 200).unwrap()];

        let (stored, info) = stored_object(ctx, "assets", "same.txt")
            .await
            .expect("the winner's object is stored");
        assert_eq!(
            stored, winner_bytes,
            "the refused upload must not write the blob"
        );
        assert_eq!(info.content_type, winner_type);

        let rows = repo::objects::list_all(ctx).await.expect("object rows");
        assert_eq!(rows.len(), 1, "one key, one row: {rows:?}");
        let row = &rows[0];
        assert_eq!(row.status, ObjectStatus::Complete);
        assert_eq!(
            (row.size, row.content_type.as_str()),
            (stored.len() as i64, info.content_type.as_str()),
            "the row must describe the bytes that are stored"
        );
        assert_eq!(row.uploaded_by, "alice");
    }

    const RACING_UPLOADS: [(&[u8], &str); 2] = [
        (b"the first racer's bytes", "text/plain"),
        (
            b"# the second racer, a longer markdown file",
            "text/markdown",
        ),
    ];

    /// Two first uploads of the same NEW key. Both read no row; a plain
    /// insert has the unique index refuse the loser (a 500), and a loser that
    /// joined the winner's row leaves it describing one upload's bytes with
    /// the other's size and type.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_first_uploads_of_a_new_key_store_one_object_and_refuse_the_other() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let statuses = race_two_uploads(&ctx, RACING_UPLOADS).await;

        assert_one_upload_won(&ctx, RACING_UPLOADS, &statuses).await;
    }

    /// Two re-uploads of an EXISTING key. Both read the same `Complete` row;
    /// if both took it over, the row would describe whichever wrote last, and
    /// a failure of the other would put the old object's values back over an
    /// upload in flight.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_re_uploads_of_a_key_store_one_object_and_refuse_the_other() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let first = handle_upload_object(
            &ctx,
            &upload_msg("assets", "same.txt", "text/csv"),
            InputStream::from_bytes(b"the original".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(first).await["uploaded"],
            serde_json::json!(true)
        );

        let statuses = race_two_uploads(&ctx, RACING_UPLOADS).await;

        assert_one_upload_won(&ctx, RACING_UPLOADS, &statuses).await;
    }

    /// An upload of a key whose previous upload is still in flight — its row
    /// `Pending` and fresh — is refused, and leaves that upload's row as it
    /// was. Taking the row over is what let a failed second upload put the
    /// first one's in-flight values back over the first one's finished
    /// upload.
    #[tokio::test]
    async fn an_upload_of_a_key_another_upload_holds_is_refused() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let held = repo::objects::reserve_upload(
            &ctx,
            "assets",
            "same.txt",
            7,
            "text/plain",
            "bob",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await
        .expect("bob's upload claims the key");

        let out = handle_upload_object(
            &ctx,
            &upload_msg("assets", "same.txt", "text/markdown"),
            InputStream::from_bytes(b"alice's longer bytes".to_vec()),
        )
        .await;

        assert_eq!(crate::test_support::output_http_status(out).await, 409);
        let rows = repo::objects::list_all(&ctx).await.expect("object rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, held.id);
        assert_eq!(
            (rows[0].size, rows[0].uploaded_by.as_str(), rows[0].status),
            (7, "bob", ObjectStatus::Pending),
            "the in-flight upload's row must be left alone"
        );
        assert!(
            storage.blob_keys("assets").is_empty(),
            "the refused upload must not write a blob"
        );
    }

    /// Seed another user's `Pending` row for `assets/same.txt` that is past
    /// the reservation TTL — an orphan, not an upload in flight. (Alice's own
    /// orphans would be swept by her upload before it reserves.)
    async fn seed_orphaned_reservation(ctx: &TestContext) {
        let ttl = repo::objects::PENDING_RESERVATION_TTL_SECONDS;
        let stale = (chrono::Utc::now() - chrono::Duration::seconds(2 * ttl)).to_rfc3339();
        repo::objects::seed(
            ctx,
            crate::util::json_map(serde_json::json!({
                "bucket": "assets",
                "key": "same.txt",
                "size": 7,
                "status": ObjectStatus::Pending,
                "uploaded_by": "bob",
                "uploaded_at": stale,
            })),
        )
        .await
        .expect("seed an orphaned reservation");
    }

    /// An upload of a key held only by an orphaned reservation takes the row
    /// over rather than waiting for a sweep that may never reach it.
    #[tokio::test]
    async fn an_orphaned_reservation_is_taken_over() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        seed_orphaned_reservation(&ctx).await;

        let out = handle_upload_object(
            &ctx,
            &upload_msg("assets", "same.txt", "text/plain"),
            InputStream::from_bytes(b"fresh bytes".to_vec()),
        )
        .await;

        assert_eq!(output_json(out).await["uploaded"], serde_json::json!(true));
        let rows = repo::objects::list_all(&ctx).await.expect("object rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].size, rows[0].uploaded_by.as_str(), rows[0].status),
            ("fresh bytes".len() as i64, "alice", ObjectStatus::Complete),
        );
    }

    /// A failed upload over an orphaned reservation does not put the orphan
    /// back: there was no stored object to restore, and restoring a `Pending`
    /// snapshot is what can bury a finished upload. The key is left free.
    #[tokio::test]
    async fn a_failed_upload_over_an_orphan_does_not_restore_it() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        seed_orphaned_reservation(&ctx).await;
        storage.refuse("put");

        let out = handle_upload_object(
            &ctx,
            &upload_msg("assets", "same.txt", "text/plain"),
            InputStream::from_bytes(b"fresh bytes".to_vec()),
        )
        .await;

        assert_eq!(crate::test_support::output_http_status(out).await, 500);
        assert!(
            repo::objects::list_all(&ctx)
                .await
                .expect("object rows")
                .is_empty(),
            "a failed upload over an orphan must leave no row"
        );
    }

    /// A context on which another upload of the key claims it just before
    /// this upload's reservation insert, and gives it up again just after:
    /// the insert is refused as a taken key, and the row that caused that is
    /// gone by the time it is read back.
    #[derive(Clone)]
    struct ChurnedKeyContext {
        inner: TestContext,
        bucket: &'static str,
        key: &'static str,
    }

    #[async_trait::async_trait]
    impl wafer_run::context::Context for ChurnedKeyContext {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), wafer_run::WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if !(name == "wafer-run/database" && msg.action() == "database.insert_guarded") {
                return self.inner.call_block(name, msg, input).await;
            }
            repo::objects::seed(
                &self.inner,
                crate::util::json_map(serde_json::json!({
                    "bucket": self.bucket,
                    "key": self.key,
                    "status": ObjectStatus::Pending,
                    "uploaded_by": "bob",
                })),
            )
            .await
            .expect("the rival upload claims the key");
            let out = self.inner.call_block(name, msg, input).await;
            // Settled before the rival lets go, so the insert sees its row —
            // and is refused by the unique index, which is an error answer.
            let answered = out.collect_buffered().await;
            repo::objects::delete_by_bucket_key(&self.inner, self.bucket, self.key)
                .await
                .expect("the rival upload releases the key");
            match answered {
                Ok(answered) => OutputStream::respond_with_meta(answered.body, answered.meta),
                Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => {
                    OutputStream::error(e)
                }
                Err(_) => panic!("the reservation insert must answer"),
            }
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    /// The reservation lost the key to another upload that has since given it
    /// up: nothing is left to join, so the upload is told to retry — a 409 —
    /// rather than a 500, and nothing is stored.
    #[tokio::test]
    async fn an_upload_whose_rival_released_the_key_is_told_to_retry() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let churned = ChurnedKeyContext {
            inner: ctx.clone(),
            bucket: "assets",
            key: "same.txt",
        };

        let out = handle_upload_object(
            &churned,
            &upload_msg("assets", "same.txt", "text/plain"),
            InputStream::from_bytes(b"bytes".to_vec()),
        )
        .await;

        assert_eq!(
            crate::test_support::output_http_status(out).await,
            409,
            "a key that changed hands mid-reservation is a conflict to retry"
        );
        assert!(
            repo::objects::list_all(&ctx)
                .await
                .expect("object rows")
                .is_empty(),
            "a refused reservation leaves no row"
        );
        assert!(
            storage.blob_keys("assets").is_empty(),
            "nothing may be stored for a refused upload"
        );
    }

    /// A replacement costs the DIFFERENCE, not the whole file: the bytes it
    /// displaces are already counted in this user's usage. Charging the full
    /// size would refuse a user who is merely editing a file in place.
    #[tokio::test]
    async fn a_replacement_is_charged_the_difference_not_the_whole_file() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let mut quota: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        quota.insert("user_id".into(), serde_json::json!("alice"));
        quota.insert("max_storage_bytes".into(), serde_json::json!(24));
        repo::quota::seed(&ctx, quota).await.expect("seed quota");

        let first = handle_upload_object(
            &ctx,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(vec![b'a'; 20]),
        )
        .await;
        assert_eq!(
            output_json(first).await["uploaded"],
            serde_json::json!(true)
        );

        // 22 bytes replacing 20 is +2 against a 24-byte cap: admitted. The
        // same 22 bytes charged whole against 20 already stored would be 42.
        let replace = handle_upload_object(
            &ctx,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(vec![b'b'; 22]),
        )
        .await;
        assert_eq!(
            output_json(replace).await["uploaded"],
            serde_json::json!(true),
            "a replacement that fits the cap after the displaced bytes must be admitted",
        );
        assert_eq!(
            crate::blocks::files::quota::get_used_bytes(&ctx, "alice")
                .await
                .expect("usage"),
            22,
            "usage must follow the object that is stored, not the sum of every upload",
        );

        // The cap is still a cap: 30 bytes replacing 22 is 30 > 24.
        let too_big = handle_upload_object(
            &ctx,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(vec![b'c'; 30]),
        )
        .await;
        assert!(
            output_is_error(too_big, "InvalidArgument").await,
            "a replacement that does not fit even after the displaced bytes must be refused",
        );
    }

    /// Give alice a quota override capping her at `max_files_per_bucket`
    /// objects per bucket, and own buckets `a` and `b`.
    async fn alice_capped_at_files_per_bucket(max_files_per_bucket: i64) -> TestContext {
        alice_capped_at_files_per_bucket_with_storage(max_files_per_bucket)
            .await
            .0
    }

    /// [`alice_capped_at_files_per_bucket`], plus the storage behind it.
    async fn alice_capped_at_files_per_bucket_with_storage(
        max_files_per_bucket: i64,
    ) -> (
        TestContext,
        std::sync::Arc<super::super::test_helpers::MemStorage>,
    ) {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "a", "alice").await;
        seed_bucket(&ctx, "b", "alice").await;
        repo::quota::seed(
            &ctx,
            crate::util::json_map(serde_json::json!({
                "user_id": "alice",
                "max_files_per_bucket": max_files_per_bucket,
            })),
        )
        .await
        .expect("seed quota");
        (ctx, storage)
    }

    /// Upload a small text file as alice through the real handler.
    async fn alice_uploads(ctx: &TestContext, bucket: &str, key: &str) -> OutputStream {
        alice_uploads_as(ctx, bucket, key).await
    }

    /// [`alice_uploads`] through any context, so a race test can pass a
    /// decorated one.
    async fn alice_uploads_as(ctx: &dyn Context, bucket: &str, key: &str) -> OutputStream {
        handle_upload_object(
            ctx,
            &upload_msg(bucket, key, "text/plain"),
            InputStream::from_bytes(b"hello".to_vec()),
        )
        .await
    }

    /// The file-count cap is per bucket, as its name and the admin table's
    /// "Max Files/Bucket" column say: filling bucket `a` to the cap does not
    /// stop an upload into bucket `b`. Counting the user's files across every
    /// bucket refused it.
    #[tokio::test]
    async fn a_full_bucket_does_not_block_uploads_into_another_bucket() {
        let ctx = alice_capped_at_files_per_bucket(2).await;
        for key in ["one.txt", "two.txt"] {
            let out = alice_uploads(&ctx, "a", key).await;
            assert_eq!(output_json(out).await["uploaded"], serde_json::json!(true));
        }

        let other_bucket = alice_uploads(&ctx, "b", "three.txt").await;
        assert_eq!(
            output_json(other_bucket).await["uploaded"],
            serde_json::json!(true),
            "bucket b holds none of alice's files, so the per-bucket cap admits it",
        );
    }

    /// And the cap is still a cap within one bucket: the upload that would be
    /// the N+1th object in it is refused, while replacing an object already
    /// there adds no row and is admitted. Alice's file in bucket `b` is what
    /// makes the count the bucket's: counted across buckets, her second
    /// upload into `a` would already be refused.
    #[tokio::test]
    async fn the_per_bucket_cap_refuses_the_upload_past_it_in_that_bucket() {
        let (ctx, storage) = alice_capped_at_files_per_bucket_with_storage(2).await;
        let elsewhere = alice_uploads(&ctx, "b", "elsewhere.txt").await;
        assert_eq!(
            output_json(elsewhere).await["uploaded"],
            serde_json::json!(true)
        );
        for key in ["one.txt", "two.txt"] {
            let out = alice_uploads(&ctx, "a", key).await;
            assert_eq!(
                output_json(out).await["uploaded"],
                serde_json::json!(true),
                "{key} is within bucket a's cap of two",
            );
        }

        let third = alice_uploads(&ctx, "a", "three.txt").await;
        assert!(
            output_is_error(third, "InvalidArgument").await,
            "a third object in a bucket capped at two must be refused",
        );
        assert!(
            !storage
                .blob_keys("a")
                .iter()
                .any(|blob| blob.ends_with("three.txt")),
            "nothing may be stored for a refused upload"
        );

        let replace = alice_uploads(&ctx, "a", "one.txt").await;
        assert_eq!(
            output_json(replace).await["uploaded"],
            serde_json::json!(true),
            "replacing an object in a full bucket adds no file and is admitted",
        );
    }

    /// A user already over the per-bucket cap — an admin lowered it below
    /// what they hold — can still overwrite their own file: the overwrite adds
    /// no file, so the count guard is not set on it. A new file in that
    /// bucket is still refused.
    #[tokio::test]
    async fn a_user_over_the_bucket_cap_can_still_replace_their_own_file() {
        let (ctx, storage) = alice_capped_at_files_per_bucket_with_storage(2).await;
        for key in ["one.txt", "two.txt", "three.txt"] {
            repo::objects::seed(
                &ctx,
                crate::util::json_map(serde_json::json!({
                    "bucket": "a",
                    "key": key,
                    "size": 5,
                    "content_type": "text/plain",
                    "status": ObjectStatus::Complete,
                    "uploaded_by": "alice",
                    "uploaded_at": "2026-09-01T00:00:00+00:00",
                })),
            )
            .await
            .expect("seed one of alice's files");
        }

        let replace = alice_uploads(&ctx, "a", "one.txt").await;
        assert_eq!(
            output_json(replace).await["uploaded"],
            serde_json::json!(true),
            "replacing her own file adds none, over the cap or not",
        );
        assert_eq!(
            repo::objects::count_for_uploader_in_bucket(&ctx, "alice", "a")
                .await
                .expect("count"),
            3,
        );

        let new_file = alice_uploads(&ctx, "a", "four.txt").await;
        assert!(
            output_is_error(new_file, "InvalidArgument").await,
            "a new file in a bucket over its cap is still refused",
        );
        assert!(
            !storage
                .blob_keys("a")
                .iter()
                .any(|blob| blob.ends_with("four.txt")),
            "nothing may be stored for a refused upload"
        );
    }

    /// An upload still in flight counts against the bucket's cap, as its bytes
    /// count against the storage cap: an upload whose check runs after
    /// another upload's reservation has landed sees that reservation and is
    /// refused. A guard on the chosen semantics — the cross-bucket count
    /// counted pending rows too, so this passes before and after the
    /// per-bucket fix.
    #[tokio::test]
    async fn an_upload_in_flight_counts_against_the_bucket_cap() {
        let ctx = alice_capped_at_files_per_bucket(1).await;
        repo::objects::reserve_upload(
            &ctx,
            "a",
            "in-flight.txt",
            5,
            "text/plain",
            "alice",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await
        .expect("reserve");

        let out = alice_uploads(&ctx, "a", "next.txt").await;
        assert!(
            output_is_error(out, "InvalidArgument").await,
            "the pending reservation is the bucket's one file",
        );
    }

    /// Run `uploads` (bucket, key, uploader, bytes) through the real handler
    /// at once, each held at the reservation's guarded write `op` until every
    /// one has reached it, so every racer has done everything the upload does
    /// before its write when the writes land. Answers each racer's HTTP
    /// status, in `uploads` order.
    async fn race_guarded_writes(
        ctx: &TestContext,
        op: &'static str,
        uploads: Vec<(&'static str, &'static str, &'static str, Vec<u8>)>,
    ) -> Vec<u16> {
        use crate::test_support::{output_http_status, RendezvousDbOpContext};

        let gated =
            RendezvousDbOpContext::new(ctx.clone(), op, repo::objects::TABLE, uploads.len());
        let racers: Vec<_> = uploads
            .into_iter()
            .map(|(bucket, key, uploader, bytes)| {
                let racer = gated.clone();
                tokio::spawn(async move {
                    let out = handle_upload_object(
                        &racer,
                        &upload_msg_from(bucket, key, "text/plain", uploader),
                        InputStream::from_bytes(bytes),
                    )
                    .await;
                    output_http_status(out).await
                })
            })
            .collect();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            futures::future::try_join_all(racers),
        )
        .await
        .expect("every upload must reach the rendezvous and finish")
        .expect("upload task panicked")
    }

    /// The per-bucket cap is exact under concurrency. Two uploads of
    /// different keys into a bucket one short of its cap are held until both
    /// are at their reservation insert, so neither has written when the
    /// other decides: the cap is checked by the insert itself, and exactly
    /// one of them is admitted. A count read before a separate insert would
    /// show both one file and admit both, leaving the bucket one over.
    #[tokio::test]
    async fn racing_uploads_of_different_keys_cannot_overshoot_the_bucket_cap() {
        let ctx = alice_capped_at_files_per_bucket(2).await;
        let first = alice_uploads(&ctx, "a", "one.txt").await;
        assert_eq!(
            output_json(first).await["uploaded"],
            serde_json::json!(true)
        );

        let mut statuses = race_guarded_writes(
            &ctx,
            "database.insert_guarded",
            vec![
                ("a", "two.txt", "alice", b"hello".to_vec()),
                ("a", "three.txt", "alice", b"hello".to_vec()),
            ],
        )
        .await;
        statuses.sort_unstable();

        assert_eq!(
            statuses,
            vec![200, 400],
            "one racer fits under the cap, the other is refused"
        );
        assert_eq!(
            repo::objects::count_for_uploader_in_bucket(&ctx, "alice", "a")
                .await
                .expect("count"),
            2,
            "the bucket ends at its cap of two",
        );
    }

    /// The storage cap is exact for take-overs too, where the row's size AND
    /// its uploader change. An admin replaces two of alice's 3-byte objects
    /// with 6 bytes each at once, against an admin cap of 10 bytes: each
    /// replacement fits alone, both together do not. Each take-over leaves
    /// the row it replaces out of the cap — it was alice's, and is now the
    /// admin's at the new size — and exactly one is admitted. Usage read
    /// before a separate take-over would admit both, leaving the admin at 12.
    #[tokio::test]
    async fn racing_take_overs_cannot_overshoot_the_new_uploaders_storage_cap() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        repo::quota::seed(
            &ctx,
            crate::util::json_map(serde_json::json!({
                "user_id": "admin_1",
                "max_storage_bytes": 10,
            })),
        )
        .await
        .expect("seed quota");
        for key in ["x.txt", "y.txt"] {
            let out = handle_upload_object(
                &ctx,
                &upload_msg("assets", key, "text/plain"),
                InputStream::from_bytes(b"abc".to_vec()),
            )
            .await;
            assert_eq!(output_json(out).await["uploaded"], serde_json::json!(true));
        }

        let statuses = race_guarded_writes(
            &ctx,
            "database.update_guarded",
            vec![
                ("assets", "x.txt", "admin_1", b"xxxxxx".to_vec()),
                ("assets", "y.txt", "admin_1", b"yyyyyy".to_vec()),
            ],
        )
        .await;
        let mut sorted = statuses.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![200, 400],
            "one take-over fits the admin's cap, the other is refused"
        );

        let usage = |user: &'static str| {
            let ctx = ctx.clone();
            async move {
                crate::blocks::files::quota::get_used_bytes(&ctx, user)
                    .await
                    .expect("usage")
            }
        };
        assert_eq!(
            usage("admin_1").await,
            6,
            "the admin is charged one replacement"
        );
        assert_eq!(
            usage("alice").await,
            3,
            "alice keeps the object that was not taken over"
        );
        let refused = if statuses[0] == 400 { "x.txt" } else { "y.txt" };
        let row = repo::objects::find_by_bucket_key(&ctx, "assets", refused)
            .await
            .expect("read")
            .expect("the refused key keeps its row");
        assert_eq!(
            (row.uploaded_by.as_str(), row.size, row.status),
            ("alice", 3, ObjectStatus::Complete),
            "a refused take-over leaves the row as it was",
        );
    }

    /// A user's override of `max_storage_bytes` is what the upload is held
    /// to: a file that fits the 1 GiB default but not the override is
    /// refused with the storage message, and nothing is stored or reserved.
    #[tokio::test]
    async fn an_upload_past_the_storage_cap_is_refused() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        repo::quota::seed(
            &ctx,
            crate::util::json_map(serde_json::json!({
                "user_id": "alice",
                "max_storage_bytes": 8,
            })),
        )
        .await
        .expect("seed quota");

        let fits = handle_upload_object(
            &ctx,
            &upload_msg("assets", "a.txt", "text/plain"),
            InputStream::from_bytes(vec![b'a'; 5]),
        )
        .await;
        assert_eq!(output_json(fits).await["uploaded"], serde_json::json!(true));

        let over = handle_upload_object(
            &ctx,
            &upload_msg("assets", "b.txt", "text/plain"),
            InputStream::from_bytes(vec![b'b'; 4]),
        )
        .await;
        match over.collect_buffered().await {
            Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => assert_eq!(
                (format!("{:?}", e.code), e.message.as_str()),
                ("InvalidArgument".to_string(), "Storage quota exceeded"),
            ),
            other => panic!("5 + 4 bytes is over a cap of 8: {other:?}"),
        }
        assert_eq!(
            repo::objects::list_all(&ctx).await.expect("rows").len(),
            1,
            "a refused upload reserves nothing",
        );
        assert!(
            !storage
                .blob_keys("assets")
                .iter()
                .any(|blob| blob.ends_with("b.txt")),
            "nothing may be stored for a refused upload"
        );
    }

    /// An outage on the quota override lookup refuses the upload with an
    /// internal error rather than admitting it under the defaults, which
    /// would silently lift an admin-lowered cap.
    #[tokio::test]
    async fn an_upload_fails_closed_when_the_quota_lookup_errors() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let failing = crate::test_support::FailingDbOpContext::new(
            ctx,
            vec![("database.list", repo::quota::TABLE)],
        );

        let out = alice_uploads_as(&failing, "assets", "a.txt").await;

        assert!(
            output_is_error(out, "Internal").await,
            "the outage must surface as an error, not as a quota verdict"
        );
    }

    /// The usage the caps are measured against is read by the reservation's
    /// write, so an outage there refuses the upload with an internal error —
    /// never admits it as if the user stored nothing.
    #[tokio::test]
    async fn an_upload_fails_closed_when_the_guarded_reservation_errors() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let failing = crate::test_support::FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.insert_guarded", repo::objects::TABLE)],
        );

        let out = alice_uploads_as(&failing, "assets", "a.txt").await;

        assert!(
            output_is_error(out, "Internal").await,
            "the outage must surface as an error, not as a quota verdict"
        );
        assert!(
            repo::objects::list_all(&ctx)
                .await
                .expect("rows")
                .is_empty(),
            "nothing is reserved"
        );
    }

    /// When the storage write fails, the reservation that took over the
    /// existing row has to put that row back: the previous blob is still
    /// there, so it must keep being described and charged.
    #[tokio::test]
    async fn a_failed_replacement_restores_the_row_of_the_object_it_kept() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let stored = handle_upload_object(
            &ctx,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(b"version one".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(stored).await["uploaded"],
            serde_json::json!(true)
        );

        // Same context, same database — only the storage write now fails.
        storage.refuse("put");
        let out = handle_upload_object(
            &ctx,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(b"much longer replacement".to_vec()),
        )
        .await;
        assert!(output_is_error(out, "Internal").await);

        let (size, content_type, status) = sole_object_row(&ctx).await;
        assert_eq!(
            size,
            "version one".len() as i64,
            "the surviving object's size must be restored, not left charging the failed upload's",
        );
        assert_eq!(content_type, "text/plain");
        assert_eq!(
            status,
            ObjectStatus::Complete,
            "the surviving object must not be left `pending` for the sweep to delete",
        );
        let (bytes, _) = stored_object(&ctx, "assets", "notes.txt")
            .await
            .expect("the surviving object is still served");
        assert_eq!(bytes, b"version one", "and it is the object that was kept");
        assert_eq!(
            storage.blob_keys("assets").len(),
            1,
            "the failed upload leaves no blob of its own: {:?}",
            storage.blob_keys("assets")
        );
    }

    /// The row is what charges quota and what the listings read, so an upload
    /// that could not be recorded is not an upload. It used to answer
    /// `uploaded: true` with the row still `pending`, which the one-hour sweep
    /// then deleted — a stored object charged to nobody, and nothing said so.
    #[tokio::test]
    async fn an_upload_that_cannot_be_recorded_is_reported_not_claimed() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        // `mark_complete` is the only `database.update_where_count` a fresh
        // upload issues.
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.update_where_count", repo::objects::TABLE)],
        );

        let out = handle_upload_object(
            &failing,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(b"bytes".to_vec()),
        )
        .await;

        assert!(
            output_is_error(out, "Internal").await,
            "an upload whose row stayed `pending` must not be reported as uploaded",
        );
        let (_, _, status) = sole_object_row(&ctx).await;
        assert_eq!(
            status,
            ObjectStatus::Pending,
            "the row is still the reservation, held until the sweep clears it",
        );
    }

    /// The retry after an upload that could not be recorded is told that its
    /// OWN earlier upload holds the key, and when that began — not that
    /// "another upload" is in progress, which sent the user looking for an
    /// upload that does not exist. Another user's upload of the key is still
    /// told the key is someone else's.
    #[tokio::test]
    async fn a_retry_after_an_unrecorded_upload_is_told_its_own_upload_holds_the_key() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.update_where_count", repo::objects::TABLE)],
        );
        let unrecorded = handle_upload_object(
            &failing,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(b"bytes".to_vec()),
        )
        .await;
        assert!(output_is_error(unrecorded, "Internal").await);
        let (_, _, status) = sole_object_row(&ctx).await;
        assert_eq!(status, ObjectStatus::Pending);
        let began = repo::objects::find_by_bucket_key(&ctx, "assets", "notes.txt")
            .await
            .expect("read")
            .expect("the reservation")
            .uploaded_at;

        let retry = wafer_block::http_codec::collect_http_response(
            handle_upload_object(
                &ctx,
                &upload_msg("assets", "notes.txt", "text/plain"),
                InputStream::from_bytes(b"bytes".to_vec()),
            )
            .await,
        )
        .await;

        assert_eq!(retry.status, 409, "the key is held, not a fault");
        let message = serde_json::from_slice::<serde_json::Value>(&retry.body)
            .expect("a JSON error body")["message"]
            .as_str()
            .expect("a message")
            .to_string();
        assert!(
            message.starts_with("Your earlier upload of this key"),
            "{message}"
        );
        assert!(message.contains(&began), "names when it began: {message}");
        assert!(!message.contains("Another upload"), "{message}");
    }

    /// A deployment that took this code without running migration 004 has no
    /// `claim_id` column. A fresh upload still works: the database backend
    /// adds a column a write's DATA names. Taking over a row written before
    /// 004 filters on `claim_id`, and a filter never adds a column, so that
    /// replacement is refused — leaving the stored row as it was — until the
    /// column exists: once 004 runs, or once any fresh upload has added it.
    #[tokio::test]
    async fn uploads_before_migration_004_has_run() {
        let (ctx, storage) = super::super::test_helpers::ctx_with_storage_before_004().await;
        seed_bucket(&ctx, "assets", "alice").await;
        seed_object_row(&ctx, "assets", "old.txt", "alice", 3).await;
        let before = repo::objects::raw_rows(&ctx).await.expect("rows");
        assert!(
            !before[0].data.contains_key("claim_id"),
            "the fixture must not have the column yet: {:?}",
            before[0].data
        );

        let refused = handle_upload_object(
            &ctx,
            &upload_msg("assets", "old.txt", "text/plain"),
            InputStream::from_bytes(b"replacement".to_vec()),
        )
        .await;
        assert!(output_is_error(refused, "Internal").await);
        let row = repo::objects::find_by_bucket_key(&ctx, "assets", "old.txt")
            .await
            .expect("read")
            .expect("the row");
        assert_eq!(
            (row.size, row.status),
            (3, ObjectStatus::Complete),
            "a refused take-over leaves the stored row as it was"
        );

        let fresh = handle_upload_object(
            &ctx,
            &upload_msg("assets", "new.txt", "text/plain"),
            InputStream::from_bytes(b"fresh".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(fresh).await["uploaded"],
            serde_json::json!(true)
        );
        let replaced = handle_upload_object(
            &ctx,
            &upload_msg("assets", "old.txt", "text/plain"),
            InputStream::from_bytes(b"replacement".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(replaced).await["uploaded"],
            serde_json::json!(true),
            "with the column in place, a row written before 004 is taken over"
        );

        storage.refuse("put");
        let failed = handle_upload_object(
            &ctx,
            &upload_msg("assets", "old.txt", "text/plain"),
            InputStream::from_bytes(b"a longer failed replacement".to_vec()),
        )
        .await;
        assert!(output_is_error(failed, "Internal").await);
        let row = repo::objects::find_by_bucket_key(&ctx, "assets", "old.txt")
            .await
            .expect("read")
            .expect("the row");
        assert_eq!(
            (row.size, row.status),
            ("replacement".len() as i64, ObjectStatus::Complete),
            "the failed replacement put the stored object's row back"
        );
    }

    /// What happens to the key's row while an upload's bytes are being
    /// stored, for [`DuringPut`].
    #[derive(Clone, Copy)]
    enum MeanwhileTheKey {
        /// Deleted, as `DELETE …/objects/{key}` or a bucket delete does.
        IsDeleted,
        /// Taken over by bob, as if alice's reservation had outlived its TTL.
        IsTakenOverByBob,
        /// Deleted, and then claimed afresh by bob's upload of the same key.
        GetsDeletedThenClaimedByBob,
    }

    /// A context on which, the first time the upload stores its bytes,
    /// something else happens to the key's row just before.
    #[derive(Clone)]
    struct DuringPut {
        inner: TestContext,
        meanwhile: MeanwhileTheKey,
        done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl wafer_run::context::Context for DuringPut {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), wafer_run::WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if name == "wafer-run/storage"
                && msg.action() == wafer_block::ServiceOp::STORAGE_PUT
                && !self.done.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                match self.meanwhile {
                    MeanwhileTheKey::IsDeleted => {
                        repo::objects::delete_by_bucket_key(&self.inner, "assets", "same.txt")
                            .await
                            .expect("the key is deleted mid-upload");
                    }
                    MeanwhileTheKey::IsTakenOverByBob => {
                        let stale = (chrono::Utc::now()
                            - chrono::Duration::seconds(
                                2 * repo::objects::PENDING_RESERVATION_TTL_SECONDS,
                            ))
                        .to_rfc3339();
                        repo::objects::backdate_upload(&self.inner, "assets", "same.txt", &stale)
                            .await
                            .expect("alice's reservation outlives its TTL");
                        repo::objects::reserve_upload(
                            &self.inner,
                            "assets",
                            "same.txt",
                            9,
                            "text/csv",
                            "bob",
                            &crate::blocks::files::models::QuotaConfig::effective_default(),
                        )
                        .await
                        .expect("bob takes the orphan over");
                    }
                    MeanwhileTheKey::GetsDeletedThenClaimedByBob => {
                        repo::objects::delete_by_bucket_key(&self.inner, "assets", "same.txt")
                            .await
                            .expect("the key is deleted mid-upload");
                        repo::objects::reserve_upload(
                            &self.inner,
                            "assets",
                            "same.txt",
                            9,
                            "text/csv",
                            "bob",
                            &crate::blocks::files::models::QuotaConfig::effective_default(),
                        )
                        .await
                        .expect("bob claims the vacant key");
                    }
                }
            }
            self.inner.call_block(name, msg, input).await
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    /// Alice uploads `assets/same.txt` while `meanwhile` happens to its row;
    /// answers the status and the error message the client receives.
    async fn upload_while(ctx: &TestContext, meanwhile: MeanwhileTheKey) -> (u16, String) {
        let during = DuringPut {
            inner: ctx.clone(),
            meanwhile,
            done: std::sync::Arc::default(),
        };
        let out = wafer_block::http_codec::collect_http_response(
            handle_upload_object(
                &during,
                &upload_msg("assets", "same.txt", "text/plain"),
                InputStream::from_bytes(b"alice's bytes".to_vec()),
            )
            .await,
        )
        .await;
        let message = serde_json::from_slice::<serde_json::Value>(&out.body)
            .ok()
            .and_then(|body| body["message"].as_str().map(str::to_string))
            .unwrap_or_default();
        (out.status, message)
    }

    /// The object was deleted while its upload was storing the bytes. The
    /// upload is told so — not that another upload took the key, when none
    /// exists — and the bytes it stored, which nothing records or charges,
    /// are not kept.
    #[tokio::test]
    async fn an_upload_whose_object_was_deleted_meanwhile_is_told_so_and_keeps_nothing() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let (status, message) = upload_while(&ctx, MeanwhileTheKey::IsDeleted).await;

        assert_eq!(status, 409, "{message}");
        assert!(message.contains("was deleted"), "{message}");
        assert!(
            storage.blob_keys("assets").is_empty(),
            "bytes nothing records must not be left in storage"
        );
        assert!(
            repo::objects::list_all(&ctx)
                .await
                .expect("rows")
                .is_empty(),
            "the discard leaves no row behind"
        );
    }

    /// Another upload took the key over while this one was storing its
    /// bytes. This upload is told so, and settles nothing: the row is still
    /// the other upload's, in flight.
    #[tokio::test]
    async fn an_upload_whose_key_was_taken_over_meanwhile_is_told_so() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let (status, message) = upload_while(&ctx, MeanwhileTheKey::IsTakenOverByBob).await;

        assert_eq!(status, 409, "{message}");
        assert!(
            message.starts_with("Another upload now holds this key"),
            "{message}"
        );
        assert!(
            storage.blob_keys("assets").is_empty(),
            "the bytes of an upload that lost the key are named by no row and not kept"
        );
        let rows = repo::objects::list_all(&ctx).await.expect("rows");
        assert_eq!(
            (rows.len(), rows[0].uploaded_by.as_str(), rows[0].status),
            (1, "bob", ObjectStatus::Pending),
            "bob's upload is still in flight"
        );
    }

    /// A fault while recording the upload is a 500, whatever its code. An
    /// `Aborted` also arrives from the runtime itself — a target block that
    /// dropped the request — and is not a lost claim.
    #[tokio::test]
    async fn an_aborted_fault_while_recording_is_a_500_not_a_lost_claim() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let failing = FailingDbOpContext::failing_with(
            ctx.clone(),
            vec![("database.update_where_count", repo::objects::TABLE)],
            wafer_run::WaferError::new(ErrorCode::Aborted, "target block dropped the request"),
        );

        let out = handle_upload_object(
            &failing,
            &upload_msg("assets", "same.txt", "text/plain"),
            InputStream::from_bytes(b"bytes".to_vec()),
        )
        .await;

        assert_eq!(crate::test_support::output_http_status(out).await, 500);
    }

    /// A multipart upload without `?key=` falls back to the file part's
    /// `filename` as the object key (the URL query param still wins when
    /// present).
    #[tokio::test]
    async fn upload_multipart_without_query_key_uses_part_filename() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "site-assets", "alice").await;

        let file_bytes: &[u8] = b"body";
        let boundary = "XBOUNDARYX";
        let envelope = multipart_envelope(boundary, "from-part.html", file_bytes);

        let msg = upload_msg(
            "site-assets",
            "",
            &format!("multipart/form-data; boundary={boundary}"),
        );
        let out = handle_upload_object(&ctx, &msg, InputStream::from_bytes(envelope)).await;
        let resp = output_json(out).await;
        assert_eq!(
            resp.get("key").and_then(|v| v.as_str()),
            Some("from-part.html"),
            "key must fall back to the part filename: {resp}"
        );

        let (stored, _) = stored_object(&ctx, "site-assets", "from-part.html")
            .await
            .expect("stored object");
        assert_eq!(stored, file_bytes);
    }

    /// The upload message for `uploader`. `admin_1` carries the admin role,
    /// as `test_support::admin_msg` gives it — an admin may upload into a
    /// bucket they do not own.
    fn upload_msg_from(bucket: &str, key: &str, content_type: &str, uploader: &str) -> Message {
        let mut msg = upload_msg(bucket, key, content_type);
        msg.set_meta("auth.user_id", uploader);
        if uploader == "admin_1" {
            msg.set_meta("auth.user_roles", "admin");
        }
        msg
    }

    /// The download message for `reader`.
    fn download_msg_as(bucket: &str, key: &str, reader: &str) -> Message {
        let mut msg = download_msg(bucket, key);
        msg.set_meta("auth.user_id", reader);
        msg
    }

    /// A context whose first storage write is held until the test releases
    /// it, after telling the test it has arrived — so the test can run a
    /// whole other upload while this one's bytes are in flight.
    #[derive(Clone)]
    struct PutHeldUntilReleased {
        inner: TestContext,
        arrived: std::sync::Arc<tokio::sync::Notify>,
        release: std::sync::Arc<tokio::sync::Notify>,
        held: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl wafer_run::context::Context for PutHeldUntilReleased {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), wafer_run::WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if name == "wafer-run/storage"
                && msg.action() == wafer_block::ServiceOp::STORAGE_PUT
                && !self.held.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                self.arrived.notify_one();
                self.release.notified().await;
            }
            self.inner.call_block(name, msg, input).await
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    /// **Fails on the pre-fix tree.** A slow upload that lost the key must
    /// not overwrite the bytes of the upload that took it over.
    ///
    /// An admin's upload into bob's bucket claims `report.txt` and stalls
    /// with its bytes in flight until its reservation is past the TTL. Bob
    /// then uploads the same key: he takes the orphaned reservation over,
    /// stores his bytes and completes. Only then do the admin's bytes land.
    /// Every upload used to write the blob at the object key, so the late
    /// write replaced bob's bytes and bob's row — complete, his, describing
    /// his upload — served the admin's content to everyone who can read
    /// bob's file. Each upload now writes a blob of its own, the row names
    /// bob's, and the late bytes land where no row points and are deleted.
    ///
    /// Everything runs through the real upload and download handlers; the
    /// hold on the admin's storage write is what forces the interleaving.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_late_upload_that_lost_the_key_does_not_overwrite_the_upload_that_took_it_over() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "bob").await;
        let held = PutHeldUntilReleased {
            inner: ctx.clone(),
            arrived: std::sync::Arc::default(),
            release: std::sync::Arc::default(),
            held: std::sync::Arc::default(),
        };

        let slow = {
            let held = held.clone();
            tokio::spawn(async move {
                wafer_block::http_codec::collect_http_response(
                    handle_upload_object(
                        &held,
                        &upload_msg_from("assets", "report.txt", "text/plain", "admin_1"),
                        InputStream::from_bytes(b"the admin's late bytes".to_vec()),
                    )
                    .await,
                )
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), held.arrived.notified())
            .await
            .expect("the slow upload reaches its storage write");

        // The slow upload's reservation outlives its TTL, and bob's upload of
        // the key takes it over and completes.
        let stale = (chrono::Utc::now()
            - chrono::Duration::seconds(2 * repo::objects::PENDING_RESERVATION_TTL_SECONDS))
        .to_rfc3339();
        repo::objects::backdate_upload(&ctx, "assets", "report.txt", &stale)
            .await
            .expect("age the slow upload's reservation");
        let bobs = handle_upload_object(
            &ctx,
            &upload_msg_from("assets", "report.txt", "text/plain", "bob"),
            InputStream::from_bytes(b"bob's own bytes".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(bobs).await["uploaded"],
            serde_json::json!(true),
            "bob's upload takes the orphaned reservation over and completes"
        );

        // Now the slow upload's bytes land.
        held.release.notify_one();
        let late = tokio::time::timeout(std::time::Duration::from_secs(30), slow)
            .await
            .expect("the slow upload finishes")
            .expect("the slow upload task");
        assert_eq!(late.status, 409, "the slow upload lost the key");

        let served = download_body(
            handle_get_object(&ctx, &download_msg_as("assets", "report.txt", "bob")).await,
        )
        .await;
        assert_eq!(
            served, b"bob's own bytes",
            "bob's object must serve bob's bytes, not the late upload's"
        );
        assert_eq!(
            storage.blob_keys("assets").len(),
            1,
            "the late bytes are named by no row and are not kept: {:?}",
            storage.blob_keys("assets")
        );
    }

    /// **Fails on the pre-fix tree.** An upload whose object was deleted and
    /// whose key was then claimed by another upload is told the truth — not
    /// that it "took too long" — and leaves nothing behind.
    ///
    /// The bytes it stored used to stay at the object key: the discard ran
    /// only when no row held the key, so here it left them for the other
    /// upload to overwrite, and if that upload failed they stayed in storage,
    /// recorded and charged nowhere.
    #[tokio::test]
    async fn an_upload_whose_key_was_deleted_and_claimed_meanwhile_keeps_nothing() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;

        let (status, message) =
            upload_while(&ctx, MeanwhileTheKey::GetsDeletedThenClaimedByBob).await;

        assert_eq!(status, 409, "{message}");
        assert!(
            message.starts_with("Another upload now holds this key"),
            "{message}"
        );
        assert!(
            !message.contains("took too long"),
            "nothing about this upload was slow: {message}"
        );
        assert!(
            storage.blob_keys("assets").is_empty(),
            "the bytes of an upload that lost the key are not kept: {:?}",
            storage.blob_keys("assets")
        );
    }

    /// An object uploaded before migration 005 — its blob at the object key,
    /// its row without a `blob_key` — is served from where it is, and a
    /// replacement serves the new bytes and deletes the old blob once the
    /// row names the new one.
    #[tokio::test]
    async fn replacing_a_legacy_object_serves_the_new_bytes_and_deletes_the_old_blob() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        seed_legacy_object(
            &ctx,
            "assets",
            "old.txt",
            b"legacy bytes",
            "text/plain",
            "alice",
        )
        .await;
        assert_eq!(
            download_body(handle_get_object(&ctx, &download_msg("assets", "old.txt")).await).await,
            b"legacy bytes",
            "a legacy row is served from the object key"
        );

        let replaced = handle_upload_object(
            &ctx,
            &upload_msg("assets", "old.txt", "text/plain"),
            InputStream::from_bytes(b"replacement bytes".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(replaced).await["uploaded"],
            serde_json::json!(true)
        );

        assert_eq!(
            download_body(handle_get_object(&ctx, &download_msg("assets", "old.txt")).await).await,
            b"replacement bytes"
        );
        let blobs = storage.blob_keys("assets");
        assert_eq!(blobs.len(), 1, "the legacy blob is deleted: {blobs:?}");
        assert_ne!(
            blobs[0], "old.txt",
            "the new bytes are under the upload's own key"
        );
    }

    /// A guard (passes before and after): deleting an object deletes the blob
    /// its row names, whichever key that is.
    #[tokio::test]
    async fn deleting_an_uploaded_object_deletes_its_blob() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let uploaded = handle_upload_object(
            &ctx,
            &upload_msg("assets", "gone.txt", "text/plain"),
            InputStream::from_bytes(b"bytes".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(uploaded).await["uploaded"],
            serde_json::json!(true)
        );

        let out = handle_delete_object(&ctx, &delete_msg("assets", "gone.txt")).await;

        assert_eq!(output_json(out).await["deleted"], serde_json::json!(true));
        assert!(storage.blob_keys("assets").is_empty());
        assert!(repo::objects::list_all(&ctx)
            .await
            .expect("rows")
            .is_empty());
    }

    /// Deleting an object while a replacement of it is in flight deletes both
    /// blobs the row accounts for — the object it still serves, and the one
    /// the replacement is writing — so neither is left behind unnamed.
    #[tokio::test]
    async fn deleting_an_object_mid_replacement_deletes_both_of_its_blobs() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        seed_legacy_object(&ctx, "assets", "doc.txt", b"stored", "text/plain", "alice").await;
        let in_flight = repo::objects::reserve_upload(
            &ctx,
            "assets",
            "doc.txt",
            8,
            "text/plain",
            "alice",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await
        .expect("a replacement claims the key");
        store::put(
            &ctx,
            "assets",
            &in_flight.blob_key,
            b"replaced",
            "text/plain",
        )
        .await
        .expect("its bytes land");
        assert_eq!(storage.blob_keys("assets").len(), 2);

        let out = handle_delete_object(&ctx, &delete_msg("assets", "doc.txt")).await;

        assert_eq!(output_json(out).await["deleted"], serde_json::json!(true));
        assert!(
            storage.blob_keys("assets").is_empty(),
            "{:?}",
            storage.blob_keys("assets")
        );
        assert_eq!(
            repo::objects::mark_complete(&ctx, &in_flight)
                .await
                .expect("the row is read"),
            repo::objects::Completion::Deleted,
            "the replacement learns its object is gone"
        );
    }

    /// **Fails on the pre-fix tree.** The object listing reads the rows, so it
    /// names object keys — never the per-upload blob keys storage holds — and
    /// includes objects stored before migration 005. It used to list storage
    /// itself.
    #[tokio::test]
    async fn the_object_listing_names_object_keys_and_filters_by_prefix() {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        for key in ["notes/b.txt", "notes/a.txt", "other.txt"] {
            let out = alice_uploads(&ctx, "assets", key).await;
            assert_eq!(output_json(out).await["uploaded"], serde_json::json!(true));
        }
        seed_legacy_object(
            &ctx,
            "assets",
            "notes/legacy.txt",
            b"old",
            "text/plain",
            "alice",
        )
        .await;

        let list = |prefix: &str| {
            let mut msg = auth_msg("retrieve", "/b/storage/api/buckets/assets/objects", "alice");
            msg.set_meta("req.param.name", "assets");
            if !prefix.is_empty() {
                msg.set_meta("req.query.prefix", prefix);
            }
            msg
        };
        let keys = |body: &serde_json::Value| -> Vec<String> {
            body["objects"]
                .as_array()
                .expect("objects")
                .iter()
                .map(|o| o["key"].as_str().expect("key").to_string())
                .collect()
        };

        let all = output_json(handle_list_objects(&ctx, &list("")).await).await;
        assert_eq!(
            keys(&all),
            [
                "notes/a.txt",
                "notes/b.txt",
                "notes/legacy.txt",
                "other.txt"
            ]
        );
        assert_eq!(all["total_count"], serde_json::json!(4));
        assert_eq!(all["objects"][0]["size"], serde_json::json!(5));
        assert_eq!(
            all["objects"][0]["content_type"],
            serde_json::json!("text/plain")
        );

        let notes = output_json(handle_list_objects(&ctx, &list("notes/")).await).await;
        assert_eq!(
            keys(&notes),
            ["notes/a.txt", "notes/b.txt", "notes/legacy.txt"]
        );
        assert_eq!(notes["total_count"], serde_json::json!(3));
    }

    /// **Fails on the pre-fix tree.** The pending sweep reclaims the blob of
    /// a reservation it deletes, not only the row. An upload whose bytes were
    /// stored but whose row could not be recorded left them in storage
    /// forever once the sweep removed the row.
    #[tokio::test]
    async fn the_pending_sweep_deletes_the_blobs_of_the_rows_it_removes() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.update_where_count", repo::objects::TABLE)],
        );
        let unrecorded = handle_upload_object(
            &failing,
            &upload_msg("assets", "notes.txt", "text/plain"),
            InputStream::from_bytes(b"stored, never recorded".to_vec()),
        )
        .await;
        assert!(output_is_error(unrecorded, "Internal").await);
        assert_eq!(
            storage.blob_keys("assets").len(),
            1,
            "the bytes were stored"
        );
        let stale = (chrono::Utc::now()
            - chrono::Duration::seconds(2 * repo::objects::PENDING_RESERVATION_TTL_SECONDS))
        .to_rfc3339();
        repo::objects::backdate_upload(&ctx, "assets", "notes.txt", &stale)
            .await
            .expect("age the reservation");

        // Alice's next upload, of another key, sweeps first.
        let next = alice_uploads(&ctx, "assets", "next.txt").await;
        assert_eq!(output_json(next).await["uploaded"], serde_json::json!(true));

        let rows = repo::objects::list_all(&ctx).await.expect("rows");
        assert_eq!(
            rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["next.txt"],
            "the stale reservation is swept"
        );
        let blobs = storage.blob_keys("assets");
        assert_eq!(blobs.len(), 1, "and so are its bytes: {blobs:?}");
        assert!(blobs[0].ends_with("next.txt"), "{blobs:?}");
    }

    /// A deployment that took this code without running migration 005 has no
    /// `blob_key` column. Its objects are served from the object key; a
    /// replacement, a fresh upload and a failed replacement all still work:
    /// the database backend adds the column the first write names.
    #[tokio::test]
    async fn uploads_work_before_migration_005_has_run() {
        let (ctx, storage) = super::super::test_helpers::ctx_with_storage_before_005().await;
        seed_bucket(&ctx, "assets", "alice").await;
        store::put(&ctx, "assets", "old.txt", b"old", "text/plain")
            .await
            .expect("the object's blob, at its key");
        seed_object_row(&ctx, "assets", "old.txt", "alice", 3).await;
        let before = repo::objects::raw_rows(&ctx).await.expect("rows");
        assert!(
            !before[0].data.contains_key("blob_key"),
            "the fixture must not have the column yet: {:?}",
            before[0].data
        );
        assert_eq!(
            download_body(handle_get_object(&ctx, &download_msg("assets", "old.txt")).await).await,
            b"old"
        );

        let replaced = handle_upload_object(
            &ctx,
            &upload_msg("assets", "old.txt", "text/plain"),
            InputStream::from_bytes(b"replacement".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(replaced).await["uploaded"],
            serde_json::json!(true),
            "a row written before 005 is taken over"
        );
        assert_eq!(
            download_body(handle_get_object(&ctx, &download_msg("assets", "old.txt")).await).await,
            b"replacement"
        );
        let fresh = handle_upload_object(
            &ctx,
            &upload_msg("assets", "new.txt", "text/plain"),
            InputStream::from_bytes(b"fresh".to_vec()),
        )
        .await;
        assert_eq!(
            output_json(fresh).await["uploaded"],
            serde_json::json!(true)
        );

        storage.refuse("put");
        let failed = handle_upload_object(
            &ctx,
            &upload_msg("assets", "old.txt", "text/plain"),
            InputStream::from_bytes(b"a longer failed replacement".to_vec()),
        )
        .await;
        assert!(output_is_error(failed, "Internal").await);
        assert_eq!(
            download_body(handle_get_object(&ctx, &download_msg("assets", "old.txt")).await).await,
            b"replacement",
            "the failed replacement leaves the stored object served"
        );
        assert_eq!(
            storage.blob_keys("assets").len(),
            2,
            "one blob per stored object: {:?}",
            storage.blob_keys("assets")
        );
    }

    /// A context on which another upload re-claims the key's row just before
    /// every conditional delete of it, so the delete never finds the row as
    /// it read it.
    #[derive(Clone)]
    struct ReclaimedBeforeEveryDelete {
        inner: TestContext,
    }

    #[async_trait::async_trait]
    impl wafer_run::context::Context for ReclaimedBeforeEveryDelete {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), wafer_run::WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if name == "wafer-run/database" && msg.action() == "database.delete_where_count" {
                repo::objects::reclaim(&self.inner, "assets", "pic.png")
                    .await
                    .expect("another upload claims the row");
            }
            self.inner.call_block(name, msg, input).await
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    /// **Fails on the pre-fix-round tree.** A delete that loses its row to
    /// concurrent uploads on every attempt answers 409 and leaves the object
    /// whole. Deleting the blobs before the conditional row delete left a row
    /// that still listed and still charged quota while its download 404ed.
    #[tokio::test]
    async fn a_delete_that_loses_every_round_leaves_the_object_whole() {
        let ctx = ctx_with_stored_object().await;
        let racing = ReclaimedBeforeEveryDelete { inner: ctx.clone() };

        let out = handle_delete_object(&racing, &delete_msg("assets", "pic.png")).await;

        assert_eq!(crate::test_support::output_http_status(out).await, 409);
        assert_eq!(
            download_body(handle_get_object(&ctx, &download_msg("assets", "pic.png")).await).await,
            b"PNGDATA",
            "the row the delete could not remove must still have its blob"
        );
    }

    /// During a rollout an isolate on the previous release writes a
    /// replacement at the object key while the row names a claim blob. That
    /// object-key blob is one of the row's, so deleting the object removes it.
    #[tokio::test]
    async fn deleting_an_object_also_removes_a_blob_at_its_object_key() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        seed_bucket(&ctx, "assets", "alice").await;
        let uploaded = alice_uploads(&ctx, "assets", "doc.txt").await;
        assert_eq!(
            output_json(uploaded).await["uploaded"],
            serde_json::json!(true)
        );
        store::put(&ctx, "assets", "doc.txt", b"old release", "text/plain")
            .await
            .expect("an old isolate writes at the object key");
        assert_eq!(storage.blob_keys("assets").len(), 2);

        let out = handle_delete_object(&ctx, &delete_msg("assets", "doc.txt")).await;

        assert_eq!(output_json(out).await["deleted"], serde_json::json!(true));
        assert!(
            storage.blob_keys("assets").is_empty(),
            "{:?}",
            storage.blob_keys("assets")
        );
    }
}
