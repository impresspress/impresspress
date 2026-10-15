//! Impresspress storage block: wafer-core's `StorageBlock` plus an access log.
//!
//! Registered as `wafer-run/storage`. Every request is forwarded unchanged to
//! the wrapped `wafer_core::service_blocks::storage::StorageBlock`, whose
//! handler owns the storage rules (`wafer_core::interfaces::storage::handler`):
//!
//! - A plain folder is relative to the calling block's own namespace:
//!   `store::put(ctx, "uploads", …)` from `impresspress/files` lands in
//!   `impresspress/files/uploads`, and the empty folder is the namespace root.
//! - `@{org}/{block}/…` names a namespace explicitly. WRAP admits it for the
//!   block that owns it, the admin block, or a Storage grant covering the
//!   object (`wafer-run/web/site/*` for the dev publisher).
//! - A folder or key with an empty, `.` or `..` segment, or a `\`, is
//!   `InvalidArgument` before anything is authorized.
//!
//! What this block adds is one row per request in
//! [`STORAGE_ACCESS_LOGS_TABLE`] (the admin storage page): the calling block,
//! the op, the resource the handler authorizes, and the outcome. The path is
//! computed with the handler's own public `resolve_folder`, so the log names
//! the path the backend touched rather than a re-derivation of the rule.
//!
//! The request stream is not buffered for `storage.put_streaming`: only its
//! header frame is read for the log, and the body frames follow it to the
//! backend's `put_streaming` as they arrive.

use std::sync::Arc;

use futures::{Stream, StreamExt};
use wafer_block::{codec, stream::StreamEvent, wire::storage as wire, ServiceOp};
use wafer_core::{
    clients::database as db,
    interfaces::storage::{handler::resolve_folder, service::StorageService},
};
use wafer_run::{
    context::Context,
    streams::output::{SinkSendError, TerminalNotResponse},
    Block, BlockInfo, ErrorCode, InputStream, LifecycleEvent, Message, OutputStream, WaferError,
};

use super::admin::STORAGE_ACCESS_LOGS_TABLE;
use crate::{
    after_response::FinalWrite,
    util::{json_map, now_millis},
};

/// `wafer-run/storage`: wafer-core's storage block with an access log.
pub struct ImpresspressStorageBlock {
    inner: wafer_core::service_blocks::storage::StorageBlock,
}

impl ImpresspressStorageBlock {
    pub fn new(service: Arc<dyn StorageService>) -> Self {
        Self {
            inner: wafer_core::service_blocks::storage::StorageBlock::new(service),
        }
    }
}

/// The path an access-log row names for one request.
///
/// For an op the handler resolves, the resource it authorizes: the resolved
/// folder for folder ops, `{folder}/{key}` for object ops. When the request
/// does not decode or its folder does not resolve, the handler refuses it and
/// the row carries what the caller sent, so the refusal is still attributable.
/// `storage.list_folders` authorizes the list-all sentinel, which is what its
/// row names.
fn log_path(ctx: &dyn Context, op: &str, request: &[u8]) -> String {
    fn object(ctx: &dyn Context, op: &str, folder: &str, key: &str) -> String {
        match resolve_folder(ctx, op, "folder", folder) {
            Ok(resolved) => format!("{resolved}/{key}"),
            Err(_) => format!("{folder}/{key}"),
        }
    }
    fn folder(ctx: &dyn Context, op: &str, what: &str, folder: &str) -> String {
        resolve_folder(ctx, op, what, folder).unwrap_or_else(|_| folder.to_string())
    }
    let decoded = match op {
        ServiceOp::STORAGE_PUT => {
            codec::decode::<wire::PutRequest>(request).map(|r| object(ctx, op, &r.folder, &r.key))
        }
        ServiceOp::STORAGE_PUT_STREAMING => codec::decode::<wire::PutStreamingHeader>(request)
            .map(|r| object(ctx, op, &r.folder, &r.key)),
        ServiceOp::STORAGE_GET | ServiceOp::STORAGE_GET_STREAMING => {
            codec::decode::<wire::GetRequest>(request).map(|r| object(ctx, op, &r.folder, &r.key))
        }
        ServiceOp::STORAGE_DELETE => codec::decode::<wire::DeleteRequest>(request)
            .map(|r| object(ctx, op, &r.folder, &r.key)),
        ServiceOp::STORAGE_LIST => codec::decode::<wire::ListRequest>(request)
            .map(|r| folder(ctx, op, "folder", &r.folder)),
        ServiceOp::STORAGE_CREATE_FOLDER => codec::decode::<wire::CreateFolderRequest>(request)
            .map(|r| folder(ctx, op, "name", &r.name)),
        ServiceOp::STORAGE_DELETE_FOLDER => codec::decode::<wire::DeleteFolderRequest>(request)
            .map(|r| folder(ctx, op, "name", &r.name)),
        ServiceOp::STORAGE_LIST_FOLDERS => {
            return wafer_block::wrap::STORAGE_LIST_ALL_RESOURCE.to_string()
        }
        _ => return String::new(),
    };
    decoded.unwrap_or_default()
}

/// Read what the access log needs from `input` and hand back a stream that
/// still carries every byte the caller sent.
///
/// `storage.put_streaming` is framed as a header chunk followed by the body
/// chunks, so only the header is read and the body is chained back behind it,
/// keeping the caller's cancellation token, and a body frame that fails
/// reaches the backend as the failure it is. Every other op is one buffered
/// request the handler collects whole anyway.
///
/// `Err` is a request that failed before its header (or its whole buffered
/// body) arrived; nothing reaches the backend.
async fn peek_request(op: &str, input: InputStream) -> Result<(Vec<u8>, InputStream), WaferError> {
    if op == ServiceOp::STORAGE_PUT_STREAMING {
        let cancel = input.cancel_token().clone();
        let mut input = input;
        let header = match input.next().await {
            Some(header) => header?,
            None => return Ok((Vec::new(), InputStream::empty())),
        };
        let forwarded = futures::stream::iter([Ok(header.clone())]).chain(input);
        Ok((
            header,
            InputStream::from_stream_with_cancel(forwarded, cancel),
        ))
    } else {
        let body = input.collect_to_bytes().await?;
        Ok((body.clone(), InputStream::from_bytes(body)))
    }
}

#[wafer_block::wafer_async_trait]
impl Block for ImpresspressStorageBlock {
    fn info(&self) -> BlockInfo {
        self.inner.info()
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        let caller = ctx.caller_id().unwrap_or("unknown").to_string();
        let (request, input) = match peek_request(&msg.kind, input).await {
            Ok(peeked) => peeked,
            Err(e) => {
                // No request to name a path from: the row records the op and
                // the failure, so the refusal is still attributable.
                let status = format!("ERROR: {}", e.message);
                let _ = log_storage_access(ctx, &caller, &msg.kind, "", status, None).await;
                return OutputStream::error(e);
            }
        };
        let path = log_path(ctx, &msg.kind, &request);
        drop(request);

        let start = now_millis();
        let kind = msg.kind.clone();
        let inner_out = self.inner.handle(ctx, msg, input).await;
        // Taken here, while the request is dispatched, for every op: the
        // producer writes its access row last, and on a cancelled request
        // that write comes after the consumer has gone.
        let final_write = crate::after_response::final_write();
        forward_logged(
            inner_out,
            ctx.clone_arc(),
            caller,
            kind,
            path,
            start,
            final_write,
        )
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        self.inner.lifecycle(ctx, event).await
    }
}

/// Forward `inner` to the caller and log its outcome once it ends.
///
/// Events are forwarded as they arrive, one by one: frame boundaries are part
/// of the storage wire protocol (a GET is a header chunk, a raw-frames marker,
/// then the body chunks), and a download must not be buffered here.
///
/// A stream that ends with no terminal event is logged as an error and
/// answered with an `Error` terminal: the bytes forwarded so far may be a
/// truncated prefix, so neither the log nor the caller may take them for a
/// finished answer.
///
/// So is a `Drop`, `Continue` or `Halt` that follows forwarded body events:
/// the sink refuses each of them after a `Chunk` or `Meta` (a `Drop` or
/// `Continue` carries no body, and a `Halt` would replace a body the caller
/// has partly received), so the request was not served. The refusal is
/// decided here, before the terminal is sent, so the log row is written first
/// and the caller gets the same explicit `Error` as the no-terminal path.
///
/// A caller that drops the response before it ended — a client that left a
/// download part-way — cancels the backend: the read of `inner` is raced
/// against the response's cancellation and `inner` is dropped, which cancels
/// the backend's producer. Forwarding into a closed sink instead read the
/// rest of the object from the backend, chunk by chunk, for nobody, and
/// logged it as served.
///
/// The access is then logged as [`STATUS_CANCELLED`] — a write issued after
/// the consumer is gone. `final_write`, the guard [`ImpresspressStorageBlock`]
/// took while the request was dispatched
/// ([`crate::after_response::final_write`]), keeps the request alive for it
/// where the host would otherwise end it with the response (Cloudflare, the
/// browser's service worker); natively the producer task runs it to the end.
/// The guard is held through every outcome's row write and dropped after it
/// — or when the producer returns, on a path that writes no row.
fn forward_logged(
    inner: impl Stream<Item = StreamEvent> + Send + Unpin + 'static,
    ctx: Arc<dyn Context>,
    caller: String,
    kind: String,
    path: String,
    start: u64,
    final_write: FinalWrite,
) -> OutputStream {
    OutputStream::from_producer(move |sink, cancel| async move {
        let mut inner = inner;
        let mut final_write = Some(final_write);
        // Every outcome after the backend was reached carries how long it
        // took, in its own column: the status says what happened, the
        // duration how long, so neither has to be parsed out of the other.
        // The row is the producer's last write, so the guard goes with it.
        let mut log = |status: String| {
            let elapsed = i64::try_from(now_millis().saturating_sub(start)).unwrap_or(i64::MAX);
            let write =
                log_storage_access(ctx.as_ref(), &caller, &kind, &path, status, Some(elapsed));
            let guard = final_write.take();
            async move {
                let written = write.await;
                drop(guard);
                written
            }
        };
        let ok = || STATUS_OK.to_string();
        let mut body_forwarded = false;
        loop {
            let Some(next) = cancel.run_until_cancelled(inner.next()).await else {
                // The caller is gone. Stop the backend before the log write,
                // not after it.
                drop(inner);
                let _ = log(STATUS_CANCELLED.to_string()).await;
                return;
            };
            let Some(ev) = next else { break };
            match ev {
                StreamEvent::Chunk(bytes) => {
                    body_forwarded = true;
                    let _ = sink.send_chunk(bytes).await;
                }
                StreamEvent::Meta(entry) => {
                    body_forwarded = true;
                    let _ = sink.send_meta(entry).await;
                }
                StreamEvent::Complete { meta } => {
                    let _ = log(ok()).await;
                    let _ = sink.complete(meta).await;
                    return;
                }
                StreamEvent::Error(e) => {
                    let _ = log(format!("ERROR: {}", e.message)).await;
                    let _ = sink.error(*e).await;
                    return;
                }
                StreamEvent::Drop { .. } | StreamEvent::Continue(_) | StreamEvent::Halt { .. }
                    if body_forwarded =>
                {
                    let terminal = match ev {
                        StreamEvent::Drop { .. } => "Drop",
                        StreamEvent::Continue(_) => "Continue",
                        _ => "Halt",
                    };
                    let error = WaferError::new(
                        ErrorCode::Internal,
                        SinkSendError::BodyAlreadySent(terminal).to_string(),
                    );
                    let _ = log(format!("ERROR: {}", error.message)).await;
                    let _ = sink.error(error).await;
                    return;
                }
                StreamEvent::Drop { meta } => {
                    let _ = sink.drop_request_with_meta(meta).await;
                    return;
                }
                StreamEvent::Continue(m) => {
                    let _ = sink.continue_with(m).await;
                    return;
                }
                StreamEvent::Halt { body, meta } => {
                    let _ = log(ok()).await;
                    let _ = sink.halt(body, meta).await;
                    return;
                }
            }
        }
        let error = WaferError::from(TerminalNotResponse::Malformed);
        let _ = log(format!("ERROR: {}", error.message)).await;
        let _ = sink.error(error).await;
    })
}

/// The `status` of an access the backend served.
const STATUS_OK: &str = "OK";

/// The `status` of an access whose caller dropped the response before it
/// ended: the backend was stopped, and what it had sent may be a prefix.
///
/// "Before it ended" is literal, so it also covers reads the caller ends on
/// purpose: an existence probe that opens a read and drops it unread
/// (`dev::blobs::exists`), and a `HEAD` on a download, whose body the
/// transport never pulls. Whether such a read logs `OK` or `CANCELLED`
/// depends on whether the backend had queued its terminal before the drop,
/// i.e. on the object's size. The row is written after the consumer is gone;
/// on a host that would end the request with its response, the request's
/// final-write guard keeps it alive for that write. See [`forward_logged`].
pub(crate) const STATUS_CANCELLED: &str = "CANCELLED";

/// Log a storage access event (best-effort). `duration_ms` is how long the
/// backend took, or `None` for a request refused before it reached one.
async fn log_storage_access(
    ctx: &dyn Context,
    source_block: &str,
    operation: &str,
    path: &str,
    status: String,
    duration_ms: Option<i64>,
) -> Result<(), WaferError> {
    db::create(
        ctx,
        STORAGE_ACCESS_LOGS_TABLE,
        json_map(serde_json::json!({
            "source_block": source_block,
            "operation": operation,
            "path": path,
            "status": status,
            "duration_ms": duration_ms,
        })),
    )
    .await
    .map(|_| ())
}

/// Create a new ImpresspressStorageBlock (caller must register it with the
/// runtime as `wafer-run/storage`).
pub fn create(service: Arc<dyn StorageService>) -> Arc<ImpresspressStorageBlock> {
    Arc::new(ImpresspressStorageBlock::new(service))
}

#[cfg(test)]
mod tests {
    use std::{
        rc::Rc,
        sync::{Arc, Mutex},
    };

    use futures::StreamExt as _;
    use wafer_block::{
        wire::storage::{GetRequest, PutRequest},
        ServiceOp,
    };
    use wafer_core::interfaces::storage::service::{
        FolderInfo, ObjectInfo, ObjectList, StorageError, StorageService,
    };
    use wafer_run::{
        context::Context as _, ErrorCode, InputStream, Message, OutputStream, ResourceGrant,
        ResourceType,
    };

    use super::{create, ImpresspressStorageBlock, STATUS_OK, STORAGE_ACCESS_LOGS_TABLE};
    use crate::test_support::{InMemoryStorageService, TestContext};

    /// A context running as `caller`, over the admin schema (the audit
    /// table), in a deployment that adds `grants` to the ones its blocks
    /// declare — the admin block's among them, the one that lets any block
    /// write a storage access row.
    async fn ctx_as(caller: &str, grants: Vec<ResourceGrant>) -> TestContext {
        let mut ctx = TestContext::with_admin().await;
        ctx.add_deployment_grants(grants);
        ctx.running_as(caller)
    }

    /// `op` from `ctx`'s block to `block`, registered as the deployment's
    /// `wafer-run/storage`: through `call_block`, so the handler runs in the
    /// frame the runtime hands a service, called by `ctx`'s block.
    async fn call(
        block: &Arc<ImpresspressStorageBlock>,
        ctx: &TestContext,
        op: &str,
        input: InputStream,
    ) -> OutputStream {
        let mut ctx = ctx.clone();
        ctx.register_block("wafer-run/storage", block.clone());
        ctx.call_block("wafer-run/storage", Message::new(op), input)
            .await
    }

    fn shim() -> (Arc<ImpresspressStorageBlock>, Arc<InMemoryStorageService>) {
        let store = Arc::new(InMemoryStorageService::new());
        (create(store.clone()), store)
    }

    /// Send one encoded request through the shim; `Ok(body)` on success.
    async fn send<T: serde::Serialize>(
        block: &Arc<ImpresspressStorageBlock>,
        ctx: &TestContext,
        op: &str,
        req: &T,
    ) -> Result<Vec<u8>, wafer_run::WaferError> {
        let body = wafer_block::codec::encode(req).expect("encode request");
        call(block, ctx, op, InputStream::from_bytes(body))
            .await
            .collect_buffered()
            .await
            .map(|r| r.body)
            .map_err(wafer_block::WaferError::from)
    }

    fn put(folder: &str, key: &str) -> PutRequest {
        PutRequest {
            folder: folder.into(),
            key: key.into(),
            data: b"payload".to_vec(),
            content_type: "text/plain".into(),
        }
    }

    fn get(folder: &str, key: &str) -> GetRequest {
        GetRequest {
            folder: folder.into(),
            key: key.into(),
        }
    }

    /// The object body of a `storage.get` answer: the handler frames it as an
    /// `ObjectInfo` header chunk, then the body.
    async fn get_body(
        block: &Arc<ImpresspressStorageBlock>,
        ctx: &TestContext,
        folder: &str,
        key: &str,
    ) -> Result<Vec<u8>, wafer_run::WaferError> {
        let body = wafer_block::codec::encode(&get(folder, key)).expect("encode request");
        let out = call(
            block,
            ctx,
            ServiceOp::STORAGE_GET,
            InputStream::from_bytes(body),
        )
        .await;
        chunks(out)
            .await
            .map(|c| c.into_iter().skip(1).flatten().collect())
    }

    /// Every chunk of `out`, or its error.
    async fn chunks(out: OutputStream) -> Result<Vec<Vec<u8>>, wafer_run::WaferError> {
        let mut out = out;
        let mut chunks = Vec::new();
        while let Some(ev) = out.next().await {
            match ev {
                wafer_block::stream::StreamEvent::Chunk(c) => chunks.push(c),
                wafer_block::stream::StreamEvent::Error(e) => return Err(*e),
                _ => {}
            }
        }
        Ok(chunks)
    }

    /// `(path, status)` of every storage access row, oldest first, read by
    /// the test rather than by the block under test.
    async fn audit_rows(ctx: &TestContext) -> Vec<(String, String)> {
        crate::db_read::list_every(&ctx.fixture(), STORAGE_ACCESS_LOGS_TABLE, Vec::new())
            .await
            .expect("read storage access logs")
            .into_iter()
            .map(|r| {
                let field = |k: &str| {
                    r.data
                        .get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                (field("path"), field("status"))
            })
            .collect()
    }

    /// Objects stored before wafer-core's handler did the namespacing stay
    /// where their callers find them.
    ///
    /// Until the handler resolved folders itself, this block rewrote them and
    /// the handler passed the result to the backend verbatim: a plain `F`
    /// from block `B` was stored under `B/F`, `""` under `B`, and `@X` under
    /// `X`. The backend paths below are those rewrites, seeded straight into
    /// the store. Each must read back through the block with the folder its
    /// writer used. A sandbox guest `site/shop` writes to the folder
    /// `site/shop` (the guest docs' spelling), so its objects sit at
    /// `site/shop/site/shop/…`.
    ///
    /// If the block still prefixed the caller on top of the handler, every
    /// plain read would look under `B/B/F` and miss.
    #[tokio::test]
    async fn objects_stored_under_the_earlier_layout_stay_reachable() {
        let (block, store) = shim();
        for (folder, key) in [
            ("impresspress/files/uploads", "a.txt"),
            ("impresspress/dev", "workspace.json"),
            ("site/shop/site/shop", "index.html"),
            ("wafer-run/web/site", "index.html"),
        ] {
            store
                .put(
                    folder,
                    key,
                    format!("{folder}/{key}").as_bytes(),
                    "text/plain",
                )
                .await
                .expect("seed");
        }

        let files = ctx_as("impresspress/files", Vec::new()).await;
        assert_eq!(
            get_body(&block, &files, "uploads", "a.txt")
                .await
                .expect("own folder"),
            b"impresspress/files/uploads/a.txt",
        );
        let dev = ctx_as(
            "impresspress/dev",
            vec![
                ResourceGrant::read_write("impresspress/dev", "wafer-run/web/site/*")
                    .typed(ResourceType::Storage),
            ],
        )
        .await;
        assert_eq!(
            get_body(&block, &dev, "", "workspace.json")
                .await
                .expect("namespace root"),
            b"impresspress/dev/workspace.json",
        );
        assert_eq!(
            get_body(&block, &dev, "@wafer-run/web/site", "index.html")
                .await
                .expect("granted cross-block path"),
            b"wafer-run/web/site/index.html",
        );
        let guest = ctx_as("site/shop", Vec::new()).await;
        assert_eq!(
            get_body(&block, &guest, "site/shop", "index.html")
                .await
                .expect("sandbox guest folder"),
            b"site/shop/site/shop/index.html",
        );
    }

    /// A write lands where the earlier layout put it, and the access row
    /// names that backend path.
    #[tokio::test]
    async fn a_plain_folder_is_stored_once_under_the_caller_and_logged_as_stored() {
        let ctx = ctx_as("impresspress/files", Vec::new()).await;
        let (block, store) = shim();

        send(
            &block,
            &ctx,
            ServiceOp::STORAGE_PUT,
            &put("uploads", "a.txt"),
        )
        .await
        .expect("own-namespace put");

        assert_eq!(store.ops(), vec!["put impresspress/files/uploads/a.txt"]);
        let rows = audit_rows(&ctx).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "impresspress/files/uploads/a.txt");
        assert_eq!(rows[0].1, STATUS_OK, "{rows:?}");
        // How long it took is its own column, not part of the status.
        let stored =
            crate::db_read::list_every(&ctx.fixture(), STORAGE_ACCESS_LOGS_TABLE, Vec::new())
                .await
                .expect("read storage access logs");
        assert!(
            stored[0]
                .data
                .get("duration_ms")
                .is_some_and(serde_json::Value::is_i64),
            "{:?}",
            stored[0].data
        );
    }

    /// A store that records how `put_streaming` received its body.
    struct ChunkCounting {
        inner: InMemoryStorageService,
        streamed: Mutex<Vec<(String, String, usize)>>,
    }

    #[wafer_block::wafer_async_trait]
    impl StorageService for ChunkCounting {
        async fn put(
            &self,
            folder: &str,
            key: &str,
            data: &[u8],
            content_type: &str,
        ) -> Result<(), StorageError> {
            self.inner.put(folder, key, data, content_type).await
        }

        async fn put_streaming(
            &self,
            folder: &str,
            key: &str,
            data: InputStream,
            content_type: &str,
        ) -> Result<(), StorageError> {
            let mut data = data;
            let mut chunks = Vec::new();
            while let Some(chunk) = data.next().await {
                chunks.push(chunk.map_err(StorageError::Body)?);
            }
            self.streamed.lock().expect("streamed mutex").push((
                folder.to_string(),
                key.to_string(),
                chunks.len(),
            ));
            self.inner
                .put(folder, key, &chunks.concat(), content_type)
                .await
        }

        async fn get(
            &self,
            folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
            self.inner.get(folder, key).await
        }

        async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
            self.inner.delete(folder, key).await
        }

        async fn list(
            &self,
            folder: &str,
            opts: &wafer_core::interfaces::storage::service::ListOptions,
        ) -> Result<ObjectList, StorageError> {
            self.inner.list(folder, opts).await
        }

        async fn create_folder(&self, name: &str, public: bool) -> Result<(), StorageError> {
            self.inner.create_folder(name, public).await
        }

        async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
            self.inner.delete_folder(name).await
        }

        async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
            self.inner.list_folders().await
        }
    }

    /// `storage.put_streaming` reaches the backend's `put_streaming` with its
    /// body chunks intact, in the caller's namespace, and is logged: the
    /// block reads only the header frame, so the body is never buffered here.
    #[tokio::test]
    async fn a_streaming_upload_streams_through_to_the_backend() {
        let mut ctx = ctx_as("impresspress/files", Vec::new()).await;
        let service = Arc::new(ChunkCounting {
            inner: InMemoryStorageService::new(),
            streamed: Mutex::new(Vec::new()),
        });
        ctx.register_block("wafer-run/storage", create(service.clone()));

        let body = InputStream::from_stream(futures::stream::iter([
            Ok(b"one ".to_vec()),
            Ok(b"two ".to_vec()),
            Ok(b"three".to_vec()),
        ]));
        wafer_core::clients::storage::put_stream(&ctx, "uploads", "big.bin", "text/plain", body)
            .await
            .expect("a streaming upload is served");

        assert_eq!(
            *service.streamed.lock().expect("streamed mutex"),
            vec![(
                "impresspress/files/uploads".to_string(),
                "big.bin".to_string(),
                3
            )],
            "the backend's put_streaming saw every body chunk as its own frame",
        );
        let (bytes, _) = service
            .inner
            .get("impresspress/files/uploads", "big.bin")
            .await
            .expect("stored");
        assert_eq!(bytes, b"one two three");
        let rows = audit_rows(&ctx).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "impresspress/files/uploads/big.bin");
        assert_eq!(rows[0].1, STATUS_OK, "{rows:?}");
    }

    /// A streaming upload whose header frame fails — the connection dropped
    /// before it arrived whole — reaches no backend. It answers the
    /// transport's own error rather than a decode failure of an empty header,
    /// and the refusal is still logged.
    #[tokio::test]
    async fn a_streaming_upload_whose_header_fails_reaches_no_backend() {
        let ctx = ctx_as("impresspress/files", Vec::new()).await;
        let service = Arc::new(ChunkCounting {
            inner: InMemoryStorageService::new(),
            streamed: Mutex::new(Vec::new()),
        });
        let block = create(service.clone());

        let body = InputStream::from_stream(futures::stream::iter([Err(
            wafer_run::WaferError::new(ErrorCode::DeadlineExceeded, "request body read timed out"),
        )]));
        let out = call(&block, &ctx, ServiceOp::STORAGE_PUT_STREAMING, body).await;

        let err = chunks(out).await.expect_err("the upload must fail");
        assert_eq!(err.code, ErrorCode::DeadlineExceeded);
        assert!(
            service.streamed.lock().expect("streamed mutex").is_empty(),
            "nothing may reach the backend"
        );
        let rows = audit_rows(&ctx).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].1.starts_with("ERROR: "), "{rows:?}");
    }

    /// Every op the upstream client can emit reaches the handler's own arm:
    /// an empty body may fail to decode, but never as an unknown op.
    #[tokio::test]
    async fn every_upstream_storage_op_reaches_the_handler() {
        let ctx = ctx_as("impresspress/files", Vec::new()).await;
        let (block, _store) = shim();
        for op in ServiceOp::STORAGE_OPS {
            let out = call(&block, &ctx, op, InputStream::from_bytes(Vec::new())).await;
            if let Err(e) = chunks(out).await {
                assert!(
                    !e.message.contains("unknown storage operation"),
                    "{op}: {}",
                    e.message
                );
            }
        }
    }

    /// A `..` in the object KEY is traversal. The handler refuses it before
    /// anything is authorized, and the row names the request as sent.
    #[tokio::test]
    async fn a_traversal_key_is_refused_and_audited() {
        let ctx = ctx_as("impresspress/files", Vec::new()).await;
        let (block, store) = shim();

        let err = send(
            &block,
            &ctx,
            ServiceOp::STORAGE_PUT,
            &put("uploads", "../../impresspress/auth/x"),
        )
        .await
        .expect_err("a traversal key must be refused");

        assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
        let rows = audit_rows(&ctx).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].0,
            "impresspress/files/uploads/../../impresspress/auth/x"
        );
        assert!(rows[0].1.starts_with("ERROR: "), "{rows:?}");
        assert!(store.ops().is_empty(), "nothing may reach the store");
    }

    /// Empty and `.` segments are traversal shapes too: authorization is
    /// textual, and nothing downstream normalizes them.
    #[tokio::test]
    async fn empty_and_dot_segments_are_refused() {
        let ctx = ctx_as("impresspress/files", Vec::new()).await;
        let (block, store) = shim();

        for (folder, key) in [
            ("uploads", ""),
            ("uploads", "a//b"),
            ("uploads", "./x"),
            ("uploads/.", "x"),
            ("uploads/", "x"),
        ] {
            let err = send(&block, &ctx, ServiceOp::STORAGE_PUT, &put(folder, key))
                .await
                .expect_err("a traversal shape must be refused");
            assert_eq!(
                err.code,
                ErrorCode::InvalidArgument,
                "folder {folder:?} key {key:?}: {}",
                err.message,
            );
        }
        let rows = audit_rows(&ctx).await;
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(
            rows.iter().all(|(_, s)| s.starts_with("ERROR: ")),
            "{rows:?}"
        );
        assert!(store.ops().is_empty(), "nothing may reach the store");
    }

    /// A name that merely CONTAINS dots is a plain name, not traversal.
    #[tokio::test]
    async fn a_name_containing_dots_is_not_traversal() {
        let ctx = ctx_as("impresspress/files", Vec::new()).await;
        let (block, _store) = shim();

        send(
            &block,
            &ctx,
            ServiceOp::STORAGE_PUT,
            &put("a..b", "c..d.txt"),
        )
        .await
        .expect("`a..b` is a plain folder name");
        let got = get_body(&block, &ctx, "a..b", "c..d.txt")
            .await
            .expect("and the object reads back");
        assert_eq!(got, b"payload");
    }

    /// An explicit path into another block's namespace needs a grant, and the
    /// refusal is audited under the path that was refused.
    #[tokio::test]
    async fn an_ungranted_cross_block_read_is_refused_and_audited() {
        let ctx = ctx_as("test/ungranted", Vec::new()).await;
        let (block, store) = shim();
        store
            .put(
                "impresspress/files/uploads",
                "secret.txt",
                b"private",
                "text/plain",
            )
            .await
            .expect("seed the other block's object");

        let err = send(
            &block,
            &ctx,
            ServiceOp::STORAGE_GET,
            &get("@impresspress/files/uploads", "secret.txt"),
        )
        .await
        .expect_err("a cross-block read with no grant must be refused");

        assert_eq!(err.code, ErrorCode::PermissionDenied, "{}", err.message);
        let rows = audit_rows(&ctx).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "impresspress/files/uploads/secret.txt");
        assert!(rows[0].1.starts_with("ERROR: "), "{rows:?}");
    }

    /// A grant written against object paths (`wafer-run/web/site/*`, the dev
    /// block's shape) admits an object under it, and a read grant does not
    /// admit a write.
    #[tokio::test]
    async fn a_cross_block_grant_admits_the_objects_under_it() {
        let (block, store) = shim();
        let writer = ctx_as(
            "test/granted",
            vec![
                ResourceGrant::read_write("test/granted", "wafer-run/web/site/*")
                    .typed(ResourceType::Storage),
            ],
        )
        .await;

        send(
            &block,
            &writer,
            ServiceOp::STORAGE_PUT,
            &put("@wafer-run/web/site", "index.html"),
        )
        .await
        .expect("the grant covers wafer-run/web/site/index.html");
        let (bytes, _) = store
            .get("wafer-run/web/site", "index.html")
            .await
            .expect("the object landed in the granted namespace");
        assert_eq!(bytes, b"payload".to_vec());

        let reader = ctx_as(
            "test/granted",
            vec![ResourceGrant::read("test/granted", "wafer-run/web/site/*")
                .typed(ResourceType::Storage)],
        )
        .await;
        let err = send(
            &block,
            &reader,
            ServiceOp::STORAGE_PUT,
            &put("@wafer-run/web/site", "index.html"),
        )
        .await
        .expect_err("a read grant does not admit a write");
        assert_eq!(err.code, ErrorCode::PermissionDenied);
    }

    /// A stream that ends with no terminal event is logged as an error and
    /// answered with one. Its bytes may be a truncated prefix, so the access
    /// log must not record the request as served, and the caller must not
    /// receive the prefix as a finished answer.
    #[tokio::test]
    async fn a_stream_with_no_terminal_is_logged_and_answered_as_an_error() {
        let caller = "impresspress/files";
        let ctx = ctx_as(caller, Vec::new()).await;
        // A stream whose terminal has already been read: what a source that
        // ends with no terminal event looks like to the next reader.
        let mut spent = OutputStream::respond(b"prefix".to_vec());
        while spent.next().await.is_some() {}

        let out = super::forward_logged(
            spent,
            ctx.clone_arc(),
            caller.to_string(),
            ServiceOp::STORAGE_GET.to_string(),
            "impresspress/files/f/k".to_string(),
            crate::util::now_millis(),
            crate::after_response::final_write(),
        );

        assert!(
            chunks(out).await.is_err(),
            "a stream with no terminal must reach the caller as an error"
        );
        let rows = audit_rows(&ctx).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].1.starts_with("ERROR: "),
            "a stream with no terminal must not be logged as served: {rows:?}"
        );
    }

    /// A `Drop`, `Continue` or `Halt` after forwarded body bytes is refused
    /// by the sink, so the request was not served: the access log records an
    /// error naming the refused terminal, not `OK` and not nothing, and the
    /// caller is answered with an explicit `Error` terminal.
    #[tokio::test]
    async fn a_bodiless_terminal_after_body_bytes_is_logged_and_answered_as_an_error() {
        use wafer_block::stream::StreamEvent;

        let caller = "impresspress/files";
        for (name, terminal) in [
            ("Drop", StreamEvent::Drop { meta: Vec::new() }),
            (
                "Continue",
                StreamEvent::Continue(Message::new(ServiceOp::STORAGE_GET)),
            ),
            (
                "Halt",
                StreamEvent::Halt {
                    body: b"whole".to_vec(),
                    meta: Vec::new(),
                },
            ),
        ] {
            let ctx = ctx_as(caller, Vec::new()).await;
            let inner = futures::stream::iter([StreamEvent::Chunk(b"prefix".to_vec()), terminal]);

            let out = super::forward_logged(
                inner,
                ctx.clone_arc(),
                caller.to_string(),
                ServiceOp::STORAGE_GET.to_string(),
                "impresspress/files/f/k".to_string(),
                crate::util::now_millis(),
                crate::after_response::final_write(),
            );

            let err = chunks(out)
                .await
                .expect_err("a refused terminal must reach the caller as an error");
            assert_eq!(err.code, ErrorCode::Internal, "{name}: {err:?}");
            assert!(
                err.message
                    .contains(&format!("{name} terminal cannot follow")),
                "{name}: the caller is told why, not handed the sink's dropped-terminal error: {err:?}"
            );
            let rows = audit_rows(&ctx).await;
            assert_eq!(rows.len(), 1, "{name}: {rows:?}");
            assert!(
                rows[0].1.starts_with("ERROR: ") && rows[0].1.contains(name),
                "{name}: a refused terminal must be logged as an error: {rows:?}"
            );
        }
    }

    /// Every op holds its request's final-write guard through its access-row
    /// write and releases it after: the request's host waits for the row,
    /// and stops waiting once it is written.
    ///
    /// The row write is parked ([`TestContext::hold_next_database_create`])
    /// so the guard's state can be read while it is in flight. Fails if
    /// `handle` takes no guard for an op (the host sees no final write), or
    /// if the guard is released before the row is written.
    #[tokio::test]
    async fn every_op_holds_the_final_write_guard_through_its_access_row() {
        use std::time::Duration;

        use futures::FutureExt as _;
        use wafer_block::wire::storage::{DeleteRequest, ListRequest};

        use crate::after_response::{scope, AfterResponse};

        fn encoded<T: serde::Serialize>(req: &T) -> InputStream {
            InputStream::from_bytes(wafer_block::codec::encode(req).expect("encode request"))
        }

        let ops = [
            (ServiceOp::STORAGE_GET, encoded(&get("uploads", "a.txt"))),
            (
                ServiceOp::STORAGE_GET_STREAMING,
                encoded(&get("uploads", "a.txt")),
            ),
            (ServiceOp::STORAGE_PUT, encoded(&put("uploads", "b.txt"))),
            (
                ServiceOp::STORAGE_DELETE,
                encoded(&DeleteRequest {
                    folder: "uploads".into(),
                    key: "a.txt".into(),
                }),
            ),
            (
                ServiceOp::STORAGE_LIST,
                encoded(&ListRequest {
                    folder: "uploads".into(),
                    prefix: String::new(),
                    limit: 0,
                    offset: 0,
                    cursor: None,
                }),
            ),
        ];
        for (op, input) in ops {
            let (ctx, hold) = ctx_as("impresspress/files", Vec::new())
                .await
                .hold_next_database_create(STORAGE_ACCESS_LOGS_TABLE);
            let (block, store) = shim();
            store
                .put(
                    "impresspress/files/uploads",
                    "a.txt",
                    b"payload",
                    "text/plain",
                )
                .await
                .expect("seed");
            let after = AfterResponse::new();

            let out = scope(Rc::clone(&after), call(&block, &ctx, op, input)).await;
            let writes = after.take_final_writes();
            assert_eq!(
                writes.len(),
                1,
                "{op}: the request waits for its access row"
            );
            let mut released = Box::pin(writes.released());
            let consumer = tokio::spawn(chunks(out));

            tokio::time::timeout(Duration::from_secs(5), async {
                while !hold.was_reached() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{op}: the access row is written"));
            assert!(
                released.as_mut().now_or_never().is_none(),
                "{op}: the guard is held while the row is being written"
            );
            hold.release();
            tokio::time::timeout(Duration::from_secs(5), released)
                .await
                .unwrap_or_else(|_| panic!("{op}: the guard is released after the row"));
            assert!(
                !hold.budget_expired(),
                "{op}: the row was released, not timed out"
            );
            consumer
                .await
                .expect("the consumer ran")
                .unwrap_or_else(|e| panic!("{op}: served: {e:?}"));
            let rows = audit_rows(&ctx).await;
            assert_eq!(rows.len(), 1, "{op}: {rows:?}");
            assert_eq!(rows[0].1, STATUS_OK, "{op}: {rows:?}");
        }
    }

    /// A caller that drops a download part-way stops the backend's read, and
    /// the access is logged as cancelled rather than as served.
    ///
    /// The producer forwarded into the closed sink and kept reading `inner`
    /// to its terminal: the rest of the object was read from the backend for
    /// nobody, and the access was logged `OK`. A backend whose next chunk
    /// waits on its reader — this one, or a slow upstream — kept the
    /// producer for good.
    #[tokio::test]
    async fn a_caller_that_drops_a_download_part_way_stops_the_backend() {
        use std::time::Duration;

        let caller = "impresspress/files";
        let ctx = ctx_as(caller, Vec::new()).await;
        let (stopped, backend_stopped) = tokio::sync::oneshot::channel();
        let inner = OutputStream::from_producer(move |sink, cancel| async move {
            let _ = sink.send_chunk(b"first part".to_vec()).await;
            // The rest of the object is read only for a reader.
            cancel.cancelled().await;
            let _ = stopped.send(());
        });
        let mut out = super::forward_logged(
            inner,
            ctx.clone_arc(),
            caller.to_string(),
            ServiceOp::STORAGE_GET.to_string(),
            "impresspress/files/uploads/big.bin".to_string(),
            crate::util::now_millis(),
            crate::after_response::final_write(),
        );
        let first = tokio::time::timeout(Duration::from_secs(5), out.next())
            .await
            .expect("the first part is forwarded");
        assert!(
            matches!(first, Some(wafer_block::stream::StreamEvent::Chunk(ref c)) if c == b"first part"),
            "{first:?}"
        );

        drop(out);

        tokio::time::timeout(Duration::from_secs(5), backend_stopped)
            .await
            .expect("the backend's read is cancelled once the caller goes away")
            .expect("the backend reports it stopped");
        let rows = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let rows = audit_rows(&ctx).await;
                if !rows.is_empty() {
                    return rows;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the cancelled access is logged");
        assert_eq!(
            rows,
            [(
                "impresspress/files/uploads/big.bin".to_string(),
                super::STATUS_CANCELLED.to_string()
            )]
        );
    }
}
