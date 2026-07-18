//! Async storage service backed by Cloudflare R2.
//!
//! Implements the shared `StorageService` trait from wafer-core so R2Block
//! can reuse the shared message handler.

use futures::StreamExt;
use impresspress_core::streaming::UploadPartBuffer;
use wafer_block::{common::ErrorCode, InputStream, OutputStream, WaferError};
use wafer_core::interfaces::storage::service::{
    FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
};
use worker::*;

/// R2 multipart part size for streaming uploads. Every part except the last
/// must be identical in size and at least 5 MiB (R2's minimum); 8 MiB keeps
/// clear of that floor while bounding peak isolate memory to a single buffered
/// part. At R2's 10 000-part cap this still permits objects up to ~78 GiB —
/// far beyond any body a Worker upload could plausibly carry.
const PART_SIZE: usize = 8 * 1024 * 1024;

/// Async storage service wrapping Cloudflare R2.
/// Each project has its own R2 bucket — no tenant prefix needed.
pub struct R2StorageService {
    bucket: Bucket,
}

// SAFETY: `R2StorageService` only holds a `worker::Bucket` handle, which is
// scoped to a single Worker isolate. wasm32-unknown-unknown has no threads,
// so the `Send`/`Sync` bounds required by `Arc<dyn StorageService>` are
// satisfied trivially — no cross-thread aliasing is possible.
unsafe impl Send for R2StorageService {}
unsafe impl Sync for R2StorageService {}

impl R2StorageService {
    pub fn new(bucket: Bucket) -> Self {
        Self { bucket }
    }

    fn prefixed_key(&self, folder: &str, key: &str) -> String {
        format!("{folder}/{key}")
    }

    fn folder_prefix(&self, folder: &str) -> String {
        format!("{folder}/")
    }

    /// Start an R2 multipart upload for `r2_key`, recording `content_type` so
    /// the completed object carries the same MIME type the buffered
    /// [`put`](StorageService::put) path sets. Used by
    /// [`put_streaming`](StorageService::put_streaming) once a body outgrows a
    /// single [`PART_SIZE`] part.
    async fn begin_multipart(
        &self,
        r2_key: &str,
        content_type: &str,
    ) -> Result<MultipartUpload, StorageError> {
        self.bucket
            .create_multipart_upload(r2_key)
            .http_metadata(HttpMetadata {
                content_type: Some(content_type.to_string()),
                ..Default::default()
            })
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))
    }

    /// Map one R2 list page into [`ObjectInfo`]s, stripping the folder prefix
    /// from each key (R2 stores and returns fully `folder/`-prefixed keys).
    /// Shared by both the cursor and offset branches of
    /// [`list`](StorageService::list) so their per-object shaping cannot drift.
    fn page_objects(&self, folder: &str, listed: &Objects) -> Vec<ObjectInfo> {
        let folder_prefix_len = self.folder_prefix(folder).len();
        listed
            .objects()
            .iter()
            .map(|obj| {
                let full_key = obj.key();
                let key = if full_key.len() > folder_prefix_len {
                    full_key[folder_prefix_len..].to_string()
                } else {
                    full_key
                };

                ObjectInfo {
                    key,
                    size: obj.size() as i64,
                    content_type: "application/octet-stream".to_string(),
                    last_modified: r2_date_to_chrono(obj.uploaded()),
                }
            })
            .collect()
    }
}

/// Convert an R2 `Date` (JS milliseconds since epoch) into a chrono UTC time.
/// Falls back to `Utc::now()` only if R2 returns a value outside chrono's
/// representable range, which in practice cannot happen for real objects.
fn r2_date_to_chrono(d: worker::Date) -> chrono::DateTime<chrono::Utc> {
    let millis = d.as_millis() as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis).unwrap_or_else(chrono::Utc::now)
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl StorageService for R2StorageService {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        self.bucket
            .put(&r2_key, data.to_vec())
            .http_metadata(HttpMetadata {
                content_type: Some(content_type.to_string()),
                ..Default::default()
            })
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    /// Stream an [`InputStream`] body into R2 without buffering the whole
    /// object, overriding the default `put_streaming` (which collapses the
    /// stream to one `Vec` and forwards to [`put`](Self::put) — exactly the
    /// full buffering the streaming request path exists to avoid).
    ///
    /// R2's `put` cannot take an unbounded `ReadableStream`: it rejects a
    /// stream of unknown length ("Provided readable stream must have a known
    /// length (request/response body or readable half of FixedLengthStream)"),
    /// and the streaming-upload path carries no content length to build a
    /// `FixedLengthStream` from. So a body that outgrows a single part is sent
    /// via R2 **multipart** — [`UploadPartBuffer`] repacks the arriving chunks
    /// into uniform [`PART_SIZE`] parts (R2 requires every part but the last be
    /// the same size and ≥ 5 MiB) and each part is `upload_part`ed as it fills,
    /// so peak isolate memory is one part, never the whole object.
    ///
    /// A body that never fills a part takes the buffered [`put`](Self::put)
    /// path instead: its length is known (the accumulated `Vec`) and it skips
    /// all multipart round trips. Content-type is preserved on both paths. Any
    /// R2 error after the multipart upload begins aborts it, so a failed upload
    /// never leaks an incomplete multipart upload into the bucket.
    ///
    /// Caveat (shared with the default and the local-storage override):
    /// [`InputStream`] has no failure terminal, so a producer that aborts
    /// mid-body is indistinguishable from a clean end — a truncated-then-ended
    /// stream is committed as if complete. Distinguishing the two needs an
    /// error terminal on `InputStream` (tracked as a wafer-block follow-up).
    async fn put_streaming(
        &self,
        folder: &str,
        key: &str,
        mut data: InputStream,
        content_type: &str,
    ) -> Result<(), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        let mut acc = UploadPartBuffer::new(PART_SIZE);
        let mut upload: Option<MultipartUpload> = None;
        let mut parts: Vec<UploadedPart> = Vec::new();
        // R2 part numbers are 1-based (and cap at 10 000, well within u16).
        let mut next_part: u16 = 1;

        // Pump the body into uniform full-size parts, promoting to a multipart
        // upload the first time a part fills. `?` short-circuits on any R2
        // error; the abort cleanup below then runs on the started upload.
        let pump = async {
            while let Some(chunk) = data.next().await {
                acc.push(&chunk);
                while let Some(part_bytes) = acc.take_part() {
                    if upload.is_none() {
                        upload = Some(self.begin_multipart(&r2_key, content_type).await?);
                    }
                    let uploaded = upload
                        .as_ref()
                        .expect("multipart upload promoted above")
                        .upload_part(next_part, part_bytes)
                        .await
                        .map_err(|e| {
                            StorageError::Internal(format!("R2 upload_part {r2_key}: {e}"))
                        })?;
                    parts.push(uploaded);
                    next_part += 1;
                }
            }
            Ok::<(), StorageError>(())
        }
        .await;

        if let Err(e) = pump {
            if let Some(mp) = upload {
                // Best-effort cleanup; keep the original error, not an abort error.
                let _ = mp.abort().await;
            }
            return Err(e);
        }

        let tail = acc.finish();

        // Body fit inside a single part — one buffered write of a known length,
        // no multipart round trips.
        let Some(mp) = upload else {
            return self.put(folder, key, &tail, content_type).await;
        };

        // Flush the trailing partial part (empty only when the total length was
        // an exact multiple of PART_SIZE), then commit the upload.
        if !tail.is_empty() {
            match mp.upload_part(next_part, tail).await {
                Ok(uploaded) => parts.push(uploaded),
                Err(e) => {
                    let _ = mp.abort().await;
                    return Err(StorageError::Internal(format!(
                        "R2 upload_part {r2_key}: {e}"
                    )));
                }
            }
        }

        // `complete` consumes `mp`, so capture the upload id first: if the
        // commit fails we resume a fresh handle to abort the now-orphaned
        // upload (R2 bills for an incomplete upload's parts until it is aborted
        // or a lifecycle rule expires it). Best-effort — the resume/abort must
        // not mask the original completion error.
        let upload_id = mp.upload_id().await;
        if let Err(e) = mp.complete(parts).await {
            if let Ok(resumed) = self
                .bucket
                .resume_multipart_upload(r2_key.as_str(), upload_id)
            {
                let _ = resumed.abort().await;
            }
            return Err(StorageError::Internal(format!(
                "R2 complete multipart {r2_key}: {e}"
            )));
        }
        Ok(())
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        let obj = self
            .bucket
            .get(&r2_key)
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?
            .ok_or(StorageError::NotFound)?;

        let body = obj
            .body()
            .ok_or_else(|| StorageError::Internal("no body".into()))?;
        let bytes = body
            .bytes()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        let info = ObjectInfo {
            key: key.to_string(),
            size: bytes.len() as i64,
            content_type: obj
                .http_metadata()
                .content_type
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            last_modified: r2_date_to_chrono(obj.uploaded()),
        };

        Ok((bytes, info))
    }

    /// Stream the R2 object body straight through the Worker's native
    /// `ReadableStream` (`ObjectBody::stream` → `worker::ByteStream`) into an
    /// [`OutputStream`], instead of buffering the whole object into a `Vec`
    /// like the default `get_streaming` (which forwards to the buffered
    /// [`get`](Self::get)). `ObjectInfo` — including the authoritative
    /// `obj.size()` from the object head, not a post-read byte count — is
    /// resolved eagerly and returned as the header; body chunks flow verbatim
    /// as R2 delivers them. A body-read failure is surfaced as an `Error`
    /// terminal after whatever bytes already streamed (never a silent
    /// truncation reported as a clean `Complete`); a dropped consumer aborts
    /// the blocked R2 read promptly via the paired cancellation token.
    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        let obj = self
            .bucket
            .get(&r2_key)
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?
            .ok_or(StorageError::NotFound)?;

        let info = ObjectInfo {
            key: key.to_string(),
            // Authoritative size from the object head (the default `get` path
            // reports `bytes.len()` only because it has already buffered).
            size: obj.size() as i64,
            content_type: obj
                .http_metadata()
                .content_type
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            last_modified: r2_date_to_chrono(obj.uploaded()),
        };

        let body = obj
            .body()
            .ok_or_else(|| StorageError::Internal("no body".into()))?;
        // `ByteStream` owns a `'static` handle to the JS ReadableStream, so it
        // outlives `obj` (dropped at the end of this function). Box-pin it so
        // the producer loop can `.next()` it regardless of the stream's own
        // `Unpin`-ness.
        let mut byte_stream = Box::pin(
            body.stream()
                .map_err(|e| StorageError::Internal(e.to_string()))?,
        );

        let stream = OutputStream::from_producer(move |sink, cancel| async move {
            loop {
                // Race the R2 read against cancellation so a dropped consumer
                // aborts a blocked read promptly rather than after the next
                // chunk resolves.
                let Some(next) = cancel.run_until_cancelled(byte_stream.next()).await else {
                    return;
                };
                match next {
                    None => break,
                    Some(Ok(chunk)) => {
                        if sink.send_chunk(chunk).await.is_err() {
                            // Consumer dropped the stream — stop reading.
                            return;
                        }
                    }
                    Some(Err(e)) => {
                        let _ = sink
                            .error(WaferError::new(
                                ErrorCode::Internal,
                                format!("R2 read body {r2_key}: {e}"),
                            ))
                            .await;
                        return;
                    }
                }
            }
            let _ = sink.complete(vec![]).await;
        });

        Ok((stream, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        self.bucket
            .delete(&r2_key)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let full_prefix = if opts.prefix.is_empty() {
            self.folder_prefix(folder)
        } else {
            format!("{}{}", self.folder_prefix(folder), opts.prefix)
        };

        let limit = if opts.limit > 0 {
            opts.limit as u32
        } else {
            100
        };

        // Cursor mode: thread R2's own native continuation cursor straight
        // through and skip the offset walk entirely. `opts.offset` is ignored
        // here — the wafer-core contract makes cursor take precedence over
        // offset — so a deep page costs a single R2 list call with no prefix
        // re-walk (the whole point of the cursor field vs. the offset path
        // below).
        if let Some(token) = &opts.cursor {
            let mut builder = self.bucket.list().prefix(&full_prefix).limit(limit);
            // An empty token means "before the first object" → the first page,
            // which R2 returns when no cursor is set. A non-empty token is R2's
            // own opaque cursor from a prior page's `next_cursor`, fed back
            // verbatim.
            if !token.is_empty() {
                builder = builder.cursor(token.clone());
            }
            let listed = builder
                .execute()
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            let objects = self.page_objects(folder, &listed);
            // R2 only hands back a cursor while the listing is truncated;
            // surface it as `next_cursor` so the caller pages on, or `None`
            // once the final page is reached (no more objects).
            let next_cursor = if listed.truncated() {
                match listed.cursor() {
                    Some(c) => Some(c),
                    None => {
                        // `truncated() == true` is documented to always carry a
                        // cursor (same invariant the offset/`delete_folder`
                        // guards rely on). Fail loudly rather than silently
                        // dropping the continuation and stalling pagination.
                        return Err(StorageError::Internal(
                            "R2 reported truncated results with no cursor".into(),
                        ));
                    }
                }
            } else {
                None
            };

            // In cursor mode `total_count` is only the current page's count (a
            // lower bound that doesn't walk the rest of the keyspace); the
            // wafer-core contract says cursor-paging callers signal has-more
            // via `next_cursor`, not `total_count`.
            return Ok(ObjectList {
                total_count: objects.len() as i64,
                objects,
                next_cursor,
            });
        }

        // Offset mode (cursor is `None`): R2's list API has no numeric offset —
        // only an opaque cursor (see `delete_folder` above for the same
        // cursor/`truncated()` shape) — so the only correct way to honor a
        // nonzero offset is to walk cursors up to it. Hop in R2's own max page
        // size (1000) rather than the caller's usually-much-smaller page size,
        // so satisfying a given offset costs the fewest possible extra R2 round
        // trips this path allows — still not free (a deep offset costs
        // `offset / 1000` extra list calls per request), which is exactly why a
        // caller should prefer cursor mode above for deep pagination.
        let offset = opts.offset.max(0) as u64;
        const R2_LIST_MAX_PAGE: u32 = 1000;
        let mut cursor: Option<String> = None;
        let mut skipped: u64 = 0;
        while skipped < offset {
            let hop = (offset - skipped).min(u64::from(R2_LIST_MAX_PAGE)) as u32;
            let mut builder = self.bucket.list().prefix(&full_prefix).limit(hop);
            if let Some(c) = cursor.take() {
                builder = builder.cursor(c);
            }
            let page = builder
                .execute()
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            let got = page.objects().len() as u64;
            skipped += got;

            if !page.truncated() || got == 0 {
                // Ran out of objects before reaching `offset` — the
                // requested page is past the end of the keyspace. Return
                // an empty page with an exact total (we've now walked the
                // whole prefix) rather than wrapping back to page 1.
                // Terminal page (whole prefix walked) — no more objects, so
                // `next_cursor` is genuinely `None`.
                return Ok(ObjectList {
                    objects: Vec::new(),
                    total_count: skipped as i64,
                    next_cursor: None,
                });
            }
            cursor = page.cursor();
            if cursor.is_none() {
                // `truncated() == true` is documented to always come with
                // a cursor (see `delete_folder`'s identical guard below).
                return Err(StorageError::Internal(
                    "R2 reported truncated results with no cursor".into(),
                ));
            }
        }

        let mut builder = self.bucket.list().prefix(&full_prefix).limit(limit);
        if let Some(c) = cursor {
            builder = builder.cursor(c);
        }
        let listed = builder
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        let objects = self.page_objects(folder, &listed);

        // R2 may return fewer objects than requested even while more exist
        // (`truncated() == true`) — reporting `objects.len()` as the total
        // (the prior behavior) silently under-counts and breaks
        // has-more-pages checks beyond page 1. Per
        // `ObjectList::total_count`'s documented contract, backends where
        // an exact total would require walking the whole keyspace may
        // return a lower bound that's always strictly greater than
        // `offset + limit` when more objects exist, which keeps
        // `total_count > offset + limit` a correct has-more-pages check
        // without an extra R2 call.
        let total_count = if listed.truncated() {
            offset as i64 + limit as i64 + 1
        } else {
            offset as i64 + objects.len() as i64
        };

        // Offset mode deliberately returns no `next_cursor`: has-more is
        // signaled via `total_count` here, and a caller mixing a fed-back
        // cursor with a numeric offset would be ambiguous. Deep pagination that
        // wants R2's native cursor uses cursor mode (the `opts.cursor` branch
        // above) instead.
        Ok(ObjectList {
            objects,
            total_count,
            next_cursor: None,
        })
    }

    async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
        // R2 doesn't need explicit folder creation — objects create the path
        Ok(())
    }

    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        // R2 has no native folder/directory concept — deleting a "folder"
        // means listing every object under its prefix and deleting each one.
        // `list()` returns at most 1000 objects per page (R2's own cap), so
        // we page through with the cursor until `truncated` is false,
        // batch-deleting each page via `delete_multiple` (also capped at
        // 1000 keys per call — same limit, so one `delete_multiple` per
        // page is always within bounds).
        let prefix = self.folder_prefix(name);
        let mut cursor: Option<String> = None;

        loop {
            let mut list_builder = self.bucket.list().prefix(&prefix).limit(1000);
            if let Some(c) = cursor.take() {
                list_builder = list_builder.cursor(c);
            }

            let listed = list_builder
                .execute()
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            let keys: Vec<String> = listed.objects().iter().map(|obj| obj.key()).collect();
            if !keys.is_empty() {
                self.bucket
                    .delete_multiple(keys)
                    .await
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
            }

            if !listed.truncated() {
                break;
            }
            cursor = listed.cursor();
            if cursor.is_none() {
                // `truncated() == true` is documented to always come with a
                // cursor. If that contract is ever violated, we cannot tell
                // whether more objects remain under this prefix — falling
                // through to `Ok(())` would silently report success on a
                // partial delete. Fail loudly instead.
                return Err(StorageError::Internal(
                    "R2 reported truncated results with no cursor".into(),
                ));
            }
        }

        Ok(())
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        // R2 doesn't have a native folder concept
        Ok(Vec::new())
    }
}
