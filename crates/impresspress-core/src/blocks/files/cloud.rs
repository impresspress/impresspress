//! The cloud-storage JSON API: a user's share links and quota, and the admin
//! views over every user's shares, access logs and quotas. Dispatch lives in
//! the block's one route table (`blocks/files/mod.rs`); the admin handlers
//! are declared `Admin` there and gated by the router from that declaration
//! (until this PR they were reached through the admin block's `call_block`
//! delegation on synthetic paths that never existed on the wire).

use std::collections::HashMap;

use wafer_run::{context::Context, InputStream, Message, OutputStream, WaferError};

use super::{
    contracts::{
        DeletedResponse, ObjectStatus, QuotaResponse, RecordListView, RecordView,
        ShareCreatedResponse,
    },
    repo,
};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_conflict, err_forbidden, err_not_found, ok_json},
};

pub(super) async fn handle_list_shares(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match repo::shares::list_for_user(ctx, msg.user_id(), 100).await {
        Ok(page) => ok_json(&RecordListView::from_page(page)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

/// How long a share link may live, in hours — the ceiling on
/// `expires_in_hours` AND the lifetime a request that names none receives.
///
/// A share link is an unauthenticated bearer credential: it is pasted into a
/// chat or a document and never looked at again. A cap that is too short is
/// a visible annoyance with an obvious remedy (re-share), where "no expiry"
/// fails silently and without bound. So every link has an end, and an
/// operator who genuinely needs longer-lived public links raises this key
/// rather than reaching for a sentinel that means forever.
///
/// Read per request rather than compiled in, so that change takes effect
/// without a redeploy.
pub const MAX_SHARE_EXPIRY_HOURS_KEY: &str = "IMPRESSPRESS__FILES__MAX_SHARE_EXPIRY_HOURS";

/// Default for [`MAX_SHARE_EXPIRY_HOURS_KEY`]: one year.
pub const DEFAULT_MAX_SHARE_EXPIRY_HOURS: i64 = 24 * 365;

/// The configured ceiling, or the default when the key is unset or unusable,
/// or the lookup's own failure.
///
/// A non-positive or unparseable value would mint an already-expired share
/// (or, for a huge one, overflow the chrono arithmetic below — both
/// `Duration::hours` and `DateTime + Duration` panic on overflow in chrono
/// 0.4.44), so a value this handler cannot honour falls back to the default
/// the `ConfigVar` declares rather than being obeyed.
async fn max_share_expiry_hours(ctx: &dyn Context) -> Result<i64, WaferError> {
    // The two ways of not having a value are not the same event. An unset
    // key is the declared default, by design. A lookup that FAILED is not an
    // answer at all: a deployment that lowered this ceiling would be handing
    // out the longer default while the config store is unreachable, so the
    // share is refused instead, the way the upload refuses when its quota
    // cannot be read.
    let Some(raw) =
        wafer_core::clients::config::get_optional(ctx, MAX_SHARE_EXPIRY_HOURS_KEY).await?
    else {
        return Ok(DEFAULT_MAX_SHARE_EXPIRY_HOURS);
    };
    match raw.trim().parse::<i64>() {
        Ok(hours) if hours > 0 && chrono::Duration::try_hours(hours).is_some() => Ok(hours),
        _ => {
            tracing::warn!(
                key = MAX_SHARE_EXPIRY_HOURS_KEY,
                value = %raw,
                default = DEFAULT_MAX_SHARE_EXPIRY_HOURS,
                "unusable share-expiry ceiling; granting the declared default"
            );
            Ok(DEFAULT_MAX_SHARE_EXPIRY_HOURS)
        }
    }
}

pub(super) async fn handle_create_share(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // `deny_unknown_fields`: a field this struct does not know is a caller
    // asking for something the handler will not do. Ignoring it answers 200
    // to a request that was not honoured — a misspelled expiry field would
    // mint a never-expiring link and report success.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Req {
        bucket: String,
        key: String,
        expires_in_hours: Option<i64>,
        max_access_count: Option<i64>,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Validate bucket/key through the shared storage validators so the share
    // path enforces exactly the same rules as upload/download (SEC-064: the
    // old inline copy here omitted the backslash check, letting a share be
    // created for a key the storage path would reject).
    if body.bucket.is_empty() || body.key.is_empty() {
        return err_bad_request("Bucket and key are required");
    }
    if !super::storage::is_valid_bucket_name(&body.bucket) {
        return err_bad_request("Invalid bucket name");
    }
    if !super::storage::is_valid_storage_key(&body.key) {
        return err_bad_request("Invalid object key");
    }

    // Verify the user owns this bucket (or is admin) — shared helper from
    // storage.rs so the two modules stay in lockstep on what "access
    // denied" means.
    if let Err(refusal) = super::storage::require_bucket_access(ctx, msg, &body.bucket).await {
        return refusal;
    }

    // Only a stored object is shareable, and the object's row is what says
    // so: `(bucket, key)` is UNIQUE, so this is one indexed read, where
    // asking the object store would fetch the whole file to learn that it
    // exists. A `Pending` row is an upload in flight — a first upload or a
    // replacement — so which bytes a link would serve is not settled yet: a
    // 409 to retry once it finishes.
    match repo::objects::find_by_bucket_key(ctx, &body.bucket, &body.key).await {
        Ok(Some(object)) => match object.status {
            ObjectStatus::Complete => {}
            ObjectStatus::Pending => {
                return err_conflict("File upload still in progress; retry once it finishes")
            }
        },
        Ok(None) => return err_not_found("File not found"),
        Err(e) => return crud::db_error_internal(e, "Object lookup failed"),
    }

    // Mint the token that addresses the share row. It carries no expiry of
    // its own — the row below is the only clock on this link.
    let token = match super::share::generate_share_token(ctx).await {
        Ok(t) => t,
        Err(r) => return r,
    };

    let now = chrono::Utc::now();
    // Every share link has an end. A request that names no expiry gets the
    // configured ceiling — the longest life this deployment grants — rather
    // than an unexpiring link.
    let max_hours = match max_share_expiry_hours(ctx).await {
        Ok(hours) => hours,
        Err(e) => return crud::db_error_internal(e, "Share expiry ceiling lookup failed"),
    };
    let hours = match body.expires_in_hours {
        None => max_hours,
        Some(h) if !(1..=max_hours).contains(&h) => {
            return err_bad_request(&format!(
                "expires_in_hours must be between 1 and {max_hours}"
            ));
        }
        Some(h) => h,
    };
    // `try_hours` + `checked_add_signed` instead of `Duration::hours` + `+` —
    // both of the latter panic on overflow in chrono 0.4.44. The ceiling
    // already excludes anything that would overflow (`max_share_expiry_hours`
    // refuses a value chrono cannot represent); these keep the arithmetic
    // itself panic-free regardless.
    let Some(duration) = chrono::Duration::try_hours(hours) else {
        return err_bad_request("expires_in_hours out of range");
    };
    let Some(expiry) = now.checked_add_signed(duration) else {
        return err_bad_request("expires_in_hours out of range");
    };
    let expires_at = expiry.to_rfc3339();

    let created_at = now.to_rfc3339();
    let new_share = repo::shares::NewShare {
        token: &token,
        bucket: &body.bucket,
        key: &body.key,
        created_by: msg.user_id(),
        created_at: &created_at,
        expires_at: &expires_at,
        max_access_count: body.max_access_count,
    };
    match repo::shares::insert(ctx, new_share).await {
        Ok(row) => ok_json(&ShareCreatedResponse {
            id: row.id,
            direct_url: format!("/b/storage/direct/{token}"),
            token,
        }),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

pub(super) async fn handle_delete_share(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Share") {
        Ok(value) => value,
        Err(response) => return response,
    };

    // Verify ownership. This lookup is the only authorization on the path,
    // so a failed read stops the request instead of skipping the check.
    match repo::shares::find_by_id(ctx, id).await {
        Ok(share) => {
            if share.created_by != msg.user_id() && !crate::util::is_admin(msg) {
                return err_forbidden("Cannot delete another user's share");
            }
        }
        Err(e) => return crud::db_error(e, "Share not found", "Database error"),
    }

    match repo::shares::delete(ctx, id).await {
        Ok(()) => ok_json(&DeletedResponse { deleted: true }),
        Err(e) => crud::db_error(e, "Share not found", "Database error"),
    }
}

pub(super) async fn handle_get_quota(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let quota = match super::quota::get_user_quota(ctx, msg.user_id()).await {
        Ok(quota) => quota,
        Err(e) => return crud::db_error_internal(e, "Quota lookup failed"),
    };
    let usage = match super::quota::get_user_usage(ctx, msg.user_id()).await {
        Ok(usage) => usage,
        Err(e) => return crud::db_error_internal(e, "Quota usage lookup failed"),
    };
    ok_json(&QuotaResponse { quota, usage })
}

pub(super) async fn handle_admin_list_shares(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(20);
    let offset = ((page - 1) * page_size) as i64;
    match repo::shares::list_recent(ctx, page_size as u32, offset).await {
        Ok(page) => ok_json(&RecordListView::from_page(page)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

pub(super) async fn handle_access_logs(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (page, page_size, _) = msg.pagination_params(50);
    let share_id = msg.query("share_id").to_string();
    let share_id = (!share_id.is_empty()).then_some(share_id.as_str());
    let offset = ((page - 1) * page_size) as i64;

    match repo::shares::list_access_logs(ctx, share_id, page_size as u32, offset).await {
        Ok(page) => ok_json(&RecordListView::from_page(page)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

pub(super) async fn handle_admin_quotas(ctx: &dyn Context, _msg: &Message) -> OutputStream {
    match repo::quota::list(ctx, 1000).await {
        Ok(page) => ok_json(&RecordListView::from_page(page)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

pub(super) async fn handle_update_quota(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // `{id}` in `PATCH /b/cloudstorage/admin/quotas/{id}` is the user whose
    // quota is set.
    let user_id = match crud::path_id(msg, "User") {
        Ok(value) => value,
        Err(response) => return response,
    };

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // SEC-059: whitelist accepted quota fields — never forward arbitrary
    // caller-controlled keys to the upsert. Reject anything outside the
    // known quota schema. (`user_id` + `updated_at` are stamped by
    // `repo::quota::upsert_for_user`.)
    const ALLOWED_QUOTA_FIELDS: &[&str] = &[
        "max_storage_bytes",
        "max_file_size_bytes",
        "max_files_per_bucket",
    ];
    for key in body.keys() {
        if !ALLOWED_QUOTA_FIELDS.contains(&key.as_str()) {
            return err_bad_request(&format!("Unknown quota field: {key}"));
        }
    }

    match repo::quota::upsert_for_user(ctx, user_id, body).await {
        Ok(row) => ok_json(&RecordView::from_row(row)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_core::interfaces::storage::service as storage_service;
    use wafer_run::InputStream;

    use super::{
        super::test_support::{routed, share_modal_expiry, share_modal_expiry_options, upload},
        *,
    };
    use crate::test_support::{
        auth_msg, output_is_error, output_json, FailingDbOpContext, TestContext,
    };

    /// Seed one share row owned by `owner` and return its id.
    async fn seed_share(ctx: &TestContext, owner: &str) -> String {
        repo::shares::insert(
            ctx,
            repo::shares::NewShare {
                token: "share-token-1",
                bucket: "photos",
                key: "a.png",
                created_by: owner,
                created_at: "2026-09-05T00:00:00Z",
                expires_at: "2027-09-05T00:00:00Z",
                max_access_count: None,
            },
        )
        .await
        .expect("seed share")
        .id
    }

    fn share_body(bucket: &str, key: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "bucket": bucket, "key": key })).unwrap()
    }

    /// An object store that counts every call made to it, over a store that
    /// really holds bytes — so a test can assert which requests reach storage
    /// at all.
    #[derive(Default)]
    struct CountingStorage {
        inner: crate::test_support::InMemoryStorageService,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl CountingStorage {
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn count(&self) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[wafer_block::wafer_async_trait]
    impl storage_service::StorageService for CountingStorage {
        async fn put(
            &self,
            folder: &str,
            key: &str,
            data: &[u8],
            content_type: &str,
        ) -> Result<(), storage_service::StorageError> {
            self.count();
            self.inner.put(folder, key, data, content_type).await
        }

        async fn put_streaming(
            &self,
            folder: &str,
            key: &str,
            data: InputStream,
            content_type: &str,
        ) -> Result<(), storage_service::StorageError> {
            self.count();
            self.inner
                .put_streaming(folder, key, data, content_type)
                .await
        }

        async fn get(
            &self,
            folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, storage_service::ObjectInfo), storage_service::StorageError> {
            self.count();
            self.inner.get(folder, key).await
        }

        async fn get_streaming(
            &self,
            folder: &str,
            key: &str,
        ) -> Result<(OutputStream, storage_service::ObjectInfo), storage_service::StorageError>
        {
            self.count();
            self.inner.get_streaming(folder, key).await
        }

        async fn delete(
            &self,
            folder: &str,
            key: &str,
        ) -> Result<(), storage_service::StorageError> {
            self.count();
            self.inner.delete(folder, key).await
        }

        async fn list(
            &self,
            folder: &str,
            opts: &storage_service::ListOptions,
        ) -> Result<storage_service::ObjectList, storage_service::StorageError> {
            self.count();
            self.inner.list(folder, opts).await
        }

        async fn create_folder(
            &self,
            name: &str,
            public: bool,
        ) -> Result<(), storage_service::StorageError> {
            self.count();
            self.inner.create_folder(name, public).await
        }

        async fn delete_folder(&self, name: &str) -> Result<(), storage_service::StorageError> {
            self.count();
            self.inner.delete_folder(name).await
        }

        async fn list_folders(
            &self,
        ) -> Result<Vec<storage_service::FolderInfo>, storage_service::StorageError> {
            self.count();
            self.inner.list_folders().await
        }
    }

    /// The block's one share fixture (crypto, the production storage shim
    /// over a store that really holds bytes, one bucket owned by `owner`),
    /// shared with `share.rs`'s tests so both ends of the round trip run on
    /// the same wiring.
    async fn ctx_for_share_round_trip(bucket: &str, owner: &str) -> TestContext {
        super::super::test_support::share_ctx(bucket, owner).await
    }

    /// [`ctx_for_share_round_trip`] with `key` already uploaded to `photos`
    /// by `alice` through the real upload handler — the minimum a request
    /// needs to get past the existence check into the expiry handling.
    async fn ctx_sharing(key: &str) -> TestContext {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(&ctx, "photos", key, b"bytes", "text/plain", "alice").await;
        ctx
    }

    /// CRUX regression (found by driving the live app): creating a share link
    /// must succeed.
    ///
    /// `POST /b/cloudstorage/shares` 500'd on the live server because
    /// `share::generate_share_token` calls the crypto block while its
    /// `info().requires` named only database, storage and config — so the
    /// runtime refused the call with `PermissionDenied: block
    /// 'wafer-run/crypto' not in requires list` above every grant check. No
    /// test in the suite created a share against a fixture that enforced
    /// `requires` (an empty list, which production reads as unrestricted), so
    /// the whole feature shipped dead.
    #[tokio::test]
    async fn create_share_mints_a_token_for_an_existing_object() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(&ctx, "photos", "a.png", b"PNGBYTES", "image/png", "alice").await;

        let msg = auth_msg("create", "/b/cloudstorage/shares", "alice");
        let out = handle_create_share(
            &ctx,
            &msg,
            InputStream::from_bytes(share_body("photos", "a.png")),
        )
        .await;

        let resp = output_json(out).await;
        let token = resp["token"].as_str().unwrap_or_default().to_string();
        assert!(
            !token.is_empty(),
            "share creation must mint a token, got: {resp}"
        );
        assert_eq!(
            resp["direct_url"],
            serde_json::json!(format!("/b/storage/direct/{token}")),
            "the response must point at the public link for that token"
        );
    }

    /// `POST /b/cloudstorage/shares` for `photos/a.png`, sent by `alice`.
    async fn share_a_png(ctx: &dyn Context) -> OutputStream {
        handle_create_share(
            ctx,
            &auth_msg("create", "/b/cloudstorage/shares", "alice"),
            InputStream::from_bytes(share_body("photos", "a.png")),
        )
        .await
    }

    /// A key that holds nothing is not shareable. A guard: the storage read
    /// this check replaced refused it too.
    #[tokio::test]
    async fn create_share_refuses_a_key_that_holds_nothing() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;

        assert!(output_is_error(share_a_png(&ctx).await, "NotFound").await);
        assert!(
            repo::shares::list_for_user(&ctx, "alice", 10)
                .await
                .expect("shares")
                .rows
                .is_empty(),
            "a refused share must leave no row"
        );
    }

    /// An upload still in flight is not shareable yet: its row is `Pending`,
    /// so the request is a 409 to retry once it settles. The blob is in place
    /// (the upload's `put` has landed, its `mark_complete` has not), which is
    /// exactly the state a storage-existence check reads as shareable.
    #[tokio::test]
    async fn create_share_refuses_an_upload_still_in_flight() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        repo::objects::reserve_upload(
            &ctx,
            "photos",
            "a.png",
            8,
            "image/png",
            "alice",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await
        .expect("reserve the upload");
        wafer_core::clients::storage::put(&ctx, "photos", "a.png", b"PNGBYTES", "image/png")
            .await
            .expect("the upload's bytes land before its row settles");

        assert!(output_is_error(share_a_png(&ctx).await, "AlreadyExists").await);
        assert!(
            repo::shares::list_for_user(&ctx, "alice", 10)
                .await
                .expect("shares")
                .rows
                .is_empty(),
            "a refused share must leave no row"
        );
    }

    /// A blob with no object row — placed in the bucket out of band — is not
    /// shareable. Nothing else in the block sees it either: the object
    /// browser, search and quota all read the row.
    #[tokio::test]
    async fn create_share_refuses_a_blob_the_block_has_no_row_for() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        wafer_core::clients::storage::put(&ctx, "photos", "a.png", b"PNGBYTES", "image/png")
            .await
            .expect("place a blob without its row");

        assert!(output_is_error(share_a_png(&ctx).await, "NotFound").await);
    }

    /// Deciding that the object exists reads its row and nothing else: the
    /// object store is not called at all, where a `storage.get` would fetch
    /// the whole file into memory to learn that it is there.
    #[tokio::test]
    async fn create_share_makes_no_storage_call() {
        let storage = Arc::new(CountingStorage::default());
        let ctx =
            super::super::test_support::share_ctx_over("photos", "alice", storage.clone()).await;
        upload(&ctx, "photos", "a.png", b"PNGBYTES", "image/png", "alice").await;
        let before = storage.calls();

        let resp = output_json(share_a_png(&ctx).await).await;

        assert!(
            resp["token"].as_str().is_some_and(|t| !t.is_empty()),
            "the share must still be created: {resp}"
        );
        assert_eq!(
            storage.calls() - before,
            0,
            "creating a share must not call the object store"
        );
    }

    /// The row lookup's own failure is a fault, not "not found": a share
    /// request that could not tell must not be told the file is gone.
    #[tokio::test]
    async fn create_share_reports_a_failed_object_lookup() {
        let ctx = ctx_sharing("a.png").await;
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", repo::objects::TABLE)]);

        assert!(output_is_error(share_a_png(&failing).await, "Internal").await);
    }

    /// The other half of the same outage: the public share link must serve
    /// the shared object's BYTES.
    ///
    /// This crosses both bugs — the share is minted through the crypto
    /// block (bug 2) and served through `store::get_stream`, i.e.
    /// `storage.get_streaming` (bug 1) — so it is the end-to-end proof that a
    /// user can share a file and the recipient can download it.
    #[tokio::test]
    async fn shared_link_serves_the_stored_bytes() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        let stored: &[u8] = b"PNG\x89bytes-that-must-come-back";
        upload(&ctx, "photos", "a.png", stored, "image/png", "alice").await;

        let create = handle_create_share(
            &ctx,
            &auth_msg("create", "/b/cloudstorage/shares", "alice"),
            InputStream::from_bytes(share_body("photos", "a.png")),
        )
        .await;
        let token = output_json(create).await["token"]
            .as_str()
            .expect("share creation must mint a token")
            .to_string();

        // The public link takes no auth — the token is the credential.
        let mut msg =
            crate::test_support::anon_msg("retrieve", &format!("/b/storage/direct/{token}"));
        msg.set_meta("req.param.token", &token);
        let out = super::super::share::handle_direct_access(
            &ctx,
            &msg,
            &crate::blocks::rate_limit::UserRateLimiter::default(),
        )
        .await;

        let mut body = Vec::new();
        let mut events = out;
        while let Some(evt) = futures::StreamExt::next(&mut events).await {
            match evt {
                wafer_block::stream::StreamEvent::Chunk(bytes) => body.extend_from_slice(&bytes),
                wafer_block::stream::StreamEvent::Error(e) => {
                    panic!("the share link errored instead of serving: {}", e.message)
                }
                _ => {}
            }
        }
        assert_eq!(
            body, stored,
            "the share link must serve the stored bytes verbatim"
        );
    }

    /// Mint a share for an object stored at `key` with `content_type`, and
    /// return the response headers `GET /b/storage/direct/{token}` serves it
    /// with — both halves through their real handlers.
    async fn share_link_headers(key: &str, content_type: &str) -> Vec<wafer_run::MetaEntry> {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(
            &ctx,
            "photos",
            key,
            b"<h1>uploader-chosen bytes</h1>",
            content_type,
            "alice",
        )
        .await;

        let create = handle_create_share(
            &ctx,
            &auth_msg("create", "/b/cloudstorage/shares", "alice"),
            InputStream::from_bytes(share_body("photos", key)),
        )
        .await;
        let token = output_json(create).await["token"]
            .as_str()
            .expect("share creation must mint a token")
            .to_string();

        let mut msg =
            crate::test_support::anon_msg("retrieve", &format!("/b/storage/direct/{token}"));
        msg.set_meta("req.param.token", &token);
        let out = super::super::share::handle_direct_access(
            &ctx,
            &msg,
            &crate::blocks::rate_limit::UserRateLimiter::default(),
        )
        .await;

        // The response headers are the LEADING meta — the frame that precedes
        // the first body chunk, which is what makes this a streaming response.
        let events: Vec<wafer_block::stream::StreamEvent> = futures::StreamExt::collect(out).await;
        let first_chunk = events
            .iter()
            .position(|e| matches!(e, wafer_block::stream::StreamEvent::Chunk(_)))
            .expect("a body chunk must be streamed");
        events[..first_chunk]
            .iter()
            .filter_map(|e| match e {
                wafer_block::stream::StreamEvent::Meta(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    fn served_header<'m>(meta: &'m [wafer_run::MetaEntry], name: &str) -> Option<&'m str> {
        wafer_run::MetaGet::get(meta, &format!("resp.header.{name}"))
    }

    /// Stored XSS on the one route that needs no account: `/b/storage/direct/`
    /// is public, served from the app's own origin, and both its bytes and its
    /// content type are an uploader's. It used to send every object `inline`
    /// with no `nosniff`, so sharing an uploaded HTML page ran its script on
    /// this origin for whoever opened the link.
    #[tokio::test]
    async fn a_shared_html_object_is_served_as_an_inert_attachment() {
        let meta = share_link_headers("payload.html", "text/html").await;

        assert_eq!(
            served_header(&meta, "Content-Disposition"),
            Some("attachment; filename=\"payload.html\""),
            "a shared HTML object must be downloaded, never rendered on this origin",
        );
        assert_eq!(
            served_header(&meta, "X-Content-Type-Options"),
            Some("nosniff")
        );
        assert!(
            served_header(&meta, "Content-Security-Policy")
                .is_some_and(|csp| csp.contains("sandbox")),
            "an attachment a browser renders anyway must render sandboxed: {meta:?}",
        );
    }

    /// An SVG is an image by content type and a document by behaviour — it can
    /// hold `<script>` — so it is on the attachment side of the allowlist.
    #[tokio::test]
    async fn a_shared_svg_is_not_rendered_inline() {
        let meta = share_link_headers("logo.svg", "image/svg+xml").await;

        assert_eq!(
            served_header(&meta, "Content-Disposition"),
            Some("attachment; filename=\"logo.svg\"")
        );
    }

    /// Previews still work over a share link: a raster image keeps `inline`
    /// and its caching header, and gains `nosniff`.
    #[tokio::test]
    async fn a_shared_image_still_previews_inline() {
        let meta = share_link_headers("pic.png", "image/png").await;

        assert_eq!(
            served_header(&meta, "Content-Disposition"),
            Some("inline; filename=\"pic.png\"")
        );
        assert_eq!(
            served_header(&meta, "X-Content-Type-Options"),
            Some("nosniff")
        );
        assert_eq!(
            served_header(&meta, "Cache-Control"),
            Some("private, max-age=3600"),
            "the share path's own header must survive alongside the security ones",
        );
    }

    /// Regression (SEC-064): the share path used to inline its own bucket/key
    /// validation that OMITTED the backslash rejection, so a share could be
    /// created for a key the upload/download path (`is_valid_storage_key`)
    /// rejects. Now it routes through the shared validator and rejects the
    /// key before any ownership/existence lookup.
    ///
    /// The key here is a *backslash-only* key with NO `..` segment. This
    /// pins the actual SEC-064 drift: the old inline check rejected `..` but
    /// accepted a bare backslash, so a `..`-containing key (e.g. `a\..\secret`)
    /// would have been rejected by the old code too and would not prove the
    /// backslash branch. `a\secret` was *accepted* by the old inline check and
    /// is *rejected* only by the shared validator's `!key.contains('\\')` arm.
    #[tokio::test]
    async fn create_share_rejects_backslash_key() {
        let ctx = TestContext::with_files().await;
        let msg = auth_msg("create", "/b/cloudstorage/shares", "u1");
        let out = handle_create_share(
            &ctx,
            &msg,
            InputStream::from_bytes(share_body("photos", "a\\secret")),
        )
        .await;
        assert!(
            output_is_error(out, "InvalidArgument").await,
            "backslash key must be rejected (SEC-064)"
        );
    }

    /// The share path now enforces the same S3-compatible bucket-name rule as
    /// the rest of the block (and the client modal), so an uppercase /
    /// invalid bucket name is rejected up front.
    #[tokio::test]
    async fn create_share_rejects_invalid_bucket_name() {
        let ctx = TestContext::with_files().await;
        let msg = auth_msg("create", "/b/cloudstorage/shares", "u1");
        let out = handle_create_share(
            &ctx,
            &msg,
            InputStream::from_bytes(share_body("Bad/Bucket", "file.txt")),
        )
        .await;
        assert!(
            output_is_error(out, "InvalidArgument").await,
            "invalid bucket name must be rejected"
        );
    }

    /// The ownership check is the only authorization on share deletion. An
    /// outage on that lookup must stop the request, not skip the check.
    #[tokio::test]
    async fn delete_share_lookup_outage_does_not_delete() {
        let ctx = TestContext::with_files().await;
        let id = seed_share(&ctx, "u1").await;
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.get", repo::shares::TABLE)]);

        let msg = routed(auth_msg(
            "delete",
            &format!("/b/cloudstorage/shares/{id}"),
            "u2",
        ));
        let out = handle_delete_share(&failing, &msg).await;

        assert!(
            output_is_error(out, "Internal").await,
            "an ownership lookup outage must not fall through to the delete"
        );
        assert!(
            repo::shares::find_by_id(&ctx, &id).await.is_ok(),
            "the share must survive a failed ownership check"
        );
    }

    #[tokio::test]
    async fn delete_share_by_non_owner_is_forbidden() {
        let ctx = TestContext::with_files().await;
        let id = seed_share(&ctx, "u1").await;

        let msg = routed(auth_msg(
            "delete",
            &format!("/b/cloudstorage/shares/{id}"),
            "u2",
        ));
        let out = handle_delete_share(&ctx, &msg).await;

        assert!(output_is_error(out, "PermissionDenied").await);
        assert!(repo::shares::find_by_id(&ctx, &id).await.is_ok());
    }

    #[tokio::test]
    async fn delete_missing_share_is_not_found() {
        let ctx = TestContext::with_files().await;
        let msg = routed(auth_msg(
            "delete",
            "/b/cloudstorage/shares/no-such-share",
            "u1",
        ));
        assert!(output_is_error(handle_delete_share(&ctx, &msg).await, "NotFound").await);
    }

    /// `/b/cloudstorage/quota` must not report zero usage during an outage.
    #[tokio::test]
    async fn quota_endpoint_surfaces_usage_outage() {
        let ctx = TestContext::with_files().await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.sum", repo::objects::TABLE)]);

        let msg = auth_msg("retrieve", "/b/cloudstorage/quota", "u1");
        let out = handle_get_quota(&failing, &msg).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a usage outage must surface as an error, not as zero usage"
        );
    }

    /// A valid key/bucket gets past validation and is denied only by the
    /// ownership check (the user owns no such bucket) — confirming the
    /// validator change didn't accidentally reject legitimate input.
    #[tokio::test]
    async fn create_share_valid_input_reaches_ownership_check() {
        let ctx = TestContext::with_files().await;
        let msg = auth_msg("create", "/b/cloudstorage/shares", "u1");
        let out = handle_create_share(
            &ctx,
            &msg,
            InputStream::from_bytes(share_body("my-bucket", "dir/file.txt")),
        )
        .await;
        // No bucket owned by u1 → PermissionDenied, NOT InvalidArgument.
        assert!(
            output_is_error(out, "PermissionDenied").await,
            "valid input should pass validation and hit the ownership check"
        );
    }

    /// The expiry the share modal offers must be the expiry the endpoint
    /// applies.
    ///
    /// The request body is built from `files-browser.js` itself — the field
    /// the modal names and the value it sends for its pre-selected option —
    /// and the assertion is against what that option's LABEL promised the
    /// user. A modal naming a field the handler does not read, or sending
    /// days where the handler counts hours, fails here; both are invisible
    /// to a test that hand-writes the Rust struct's field names.
    #[tokio::test]
    async fn create_share_applies_the_expiry_the_share_modal_offers() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(&ctx, "photos", "a.png", b"PNGBYTES", "image/png", "alice").await;

        let expiry = share_modal_expiry();
        let mut body = serde_json::Map::new();
        body.insert("bucket".to_string(), serde_json::json!("photos"));
        body.insert("key".to_string(), serde_json::json!("a.png"));
        body.insert(expiry.field.clone(), serde_json::json!(expiry.value));
        let raw = serde_json::to_vec(&serde_json::Value::Object(body)).unwrap();

        let before = chrono::Utc::now();
        let resp = output_json(
            handle_create_share(
                &ctx,
                &auth_msg("create", "/b/cloudstorage/shares", "alice"),
                InputStream::from_bytes(raw),
            )
            .await,
        )
        .await;
        let id = resp["id"]
            .as_str()
            .unwrap_or_else(|| panic!("share creation must succeed, got: {resp}"))
            .to_string();

        let row = repo::shares::find_by_id(&ctx, &id)
            .await
            .expect("share row");
        let expires_at = row.expires_at.as_deref().unwrap_or_else(|| {
            panic!(
                "the modal's `{}` expiry was dropped: the share never expires",
                expiry.field
            )
        });
        let parsed = chrono::DateTime::parse_from_rfc3339(expires_at)
            .expect("valid rfc3339")
            .with_timezone(&chrono::Utc);
        let promised = before + chrono::Duration::hours(expiry.label_hours);
        assert!(
            (parsed - promised).num_minutes().abs() < 60,
            "the modal promised the user {} hours; the share expires at {expires_at}",
            expiry.label_hours,
        );
    }

    /// A field the handler will not honour is refused, not accepted and
    /// dropped: `deny_unknown_fields` is what stops a 200 from meaning "your
    /// expiry was applied" when it was not.
    #[tokio::test]
    async fn create_share_refuses_a_field_it_does_not_honour() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(&ctx, "photos", "a.png", b"PNGBYTES", "image/png", "alice").await;

        let body = serde_json::to_vec(&serde_json::json!({
            "bucket": "photos",
            "key": "a.png",
            "expires_days": 7,
        }))
        .unwrap();
        let out = handle_create_share(
            &ctx,
            &auth_msg("create", "/b/cloudstorage/shares", "alice"),
            InputStream::from_bytes(body),
        )
        .await;

        assert!(
            output_is_error(out, "InvalidArgument").await,
            "an expiry field the handler does not read must be a 400, not a silently unexpiring share"
        );
    }

    /// The minted token is entropy, not a dated assertion.
    ///
    /// A token that carries its own lifetime is a second clock on the share,
    /// and the row is the authoritative one: a link the owner asked to keep
    /// for a year must not stop working because the credential aged out
    /// while its row still reads active.
    #[tokio::test]
    async fn the_share_token_carries_no_lifetime_of_its_own() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(&ctx, "photos", "a.png", b"PNGBYTES", "image/png", "alice").await;

        let resp = output_json(
            handle_create_share(
                &ctx,
                &auth_msg("create", "/b/cloudstorage/shares", "alice"),
                InputStream::from_bytes(share_body("photos", "a.png")),
            )
            .await,
        )
        .await;
        let token = resp["token"].as_str().expect("a token").to_string();

        assert!(
            !token.contains('.'),
            "a share token must not be a JWT — its `exp` would expire links the row still counts as live: {token}"
        );
        assert_eq!(
            token.len(),
            64,
            "a share token is 32 random bytes, hex-encoded: {token}"
        );
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "a share token is hex-encoded entropy: {token}"
        );
    }

    /// How far out `expires_at` landed on the share `resp` created.
    async fn persisted_lifetime_hours(
        ctx: &TestContext,
        resp: &serde_json::Value,
        from: chrono::DateTime<chrono::Utc>,
    ) -> i64 {
        let id = resp["id"]
            .as_str()
            .unwrap_or_else(|| panic!("share creation must succeed, got: {resp}"));
        let row = repo::shares::find_by_id(ctx, id).await.expect("share row");
        let expires_at = row
            .expires_at
            .as_deref()
            .expect("every share link has an end");
        let parsed = chrono::DateTime::parse_from_rfc3339(expires_at)
            .expect("valid rfc3339")
            .with_timezone(&chrono::Utc);
        (parsed - from).num_minutes().div_euclid(60)
    }

    /// A share created without an expiry gets the configured ceiling, not
    /// forever.
    ///
    /// A public share link is an unauthenticated bearer credential — it is
    /// pasted into a chat and never revisited — so an unexpiring one fails
    /// silently and without bound. The longest life this deployment grants
    /// is what a request that names no expiry receives.
    #[tokio::test]
    async fn a_share_created_without_an_expiry_gets_the_configured_maximum() {
        let ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(&ctx, "photos", "a.png", b"PNGBYTES", "image/png", "alice").await;

        let before = chrono::Utc::now();
        let resp = output_json(
            handle_create_share(
                &ctx,
                &auth_msg("create", "/b/cloudstorage/shares", "alice"),
                InputStream::from_bytes(share_body("photos", "a.png")),
            )
            .await,
        )
        .await;

        let hours = persisted_lifetime_hours(&ctx, &resp, before).await;
        assert_eq!(
            hours, DEFAULT_MAX_SHARE_EXPIRY_HOURS,
            "an omitted expiry must mean the ceiling, not an unexpiring link"
        );
    }

    /// The ceiling is an operator's decision, read per request: raising or
    /// lowering the key changes both the life an expiry-less share gets and
    /// the value an explicit one is refused above.
    #[tokio::test]
    async fn the_configured_ceiling_is_what_bounds_a_share() {
        let mut ctx = ctx_for_share_round_trip("photos", "alice").await;
        upload(&ctx, "photos", "a.png", b"PNGBYTES", "image/png", "alice").await;
        ctx.set_config(MAX_SHARE_EXPIRY_HOURS_KEY, "48");

        let before = chrono::Utc::now();
        let resp = output_json(
            handle_create_share(
                &ctx,
                &auth_msg("create", "/b/cloudstorage/shares", "alice"),
                InputStream::from_bytes(share_body("photos", "a.png")),
            )
            .await,
        )
        .await;
        assert_eq!(
            persisted_lifetime_hours(&ctx, &resp, before).await,
            48,
            "the configured ceiling is the life an expiry-less share gets"
        );

        let body = serde_json::to_vec(&serde_json::json!({
            "bucket": "photos",
            "key": "a.png",
            "expires_in_hours": 49,
        }))
        .unwrap();
        let out = handle_create_share(
            &ctx,
            &auth_msg("create", "/b/cloudstorage/shares", "alice"),
            InputStream::from_bytes(body),
        )
        .await;
        assert!(
            output_is_error(out, "InvalidArgument").await,
            "an expiry past the configured ceiling must be refused"
        );

        // And the year that was legal a moment ago is not legal now — the
        // bound is read per request, not compiled in.
        let body = serde_json::to_vec(&serde_json::json!({
            "bucket": "photos",
            "key": "a.png",
            "expires_in_hours": DEFAULT_MAX_SHARE_EXPIRY_HOURS,
        }))
        .unwrap();
        assert!(
            output_is_error(
                handle_create_share(
                    &ctx,
                    &auth_msg("create", "/b/cloudstorage/shares", "alice"),
                    InputStream::from_bytes(body),
                )
                .await,
                "InvalidArgument"
            )
            .await,
            "lowering the ceiling must bind immediately"
        );
    }

    /// The share modal cannot offer a link that outlives the cap, and cannot
    /// offer "never" at all.
    ///
    /// Read off the shipped bundle: a dropdown entry with no value, or one
    /// longer than the default ceiling, would be a promise the endpoint
    /// refuses.
    #[tokio::test]
    async fn the_share_modal_offers_no_expiry_that_outlives_the_cap() {
        for option in share_modal_expiry_options() {
            assert!(
                option.value > 0,
                "every expiry option is a real duration — there is no `never`"
            );
            assert_eq!(
                option.value, option.label_hours,
                "an option must send what its label promises"
            );
            assert!(
                option.value <= DEFAULT_MAX_SHARE_EXPIRY_HOURS,
                "the modal offers {} hours, past the {DEFAULT_MAX_SHARE_EXPIRY_HOURS}-hour ceiling",
                option.value,
            );
        }
    }

    /// SB-4: `expires_in_hours` used to be fed straight into
    /// `chrono::Duration::hours` and `now + duration`, both of which PANIC
    /// on overflow in chrono 0.4.44. A huge value on this authenticated,
    /// reachable request path must produce a 400, not a handler panic.
    #[tokio::test]
    async fn create_share_rejects_huge_expiry_without_panicking() {
        let ctx = ctx_sharing("f").await;
        let msg = auth_msg("create", "/b/cloudstorage/shares", "alice");
        let body = serde_json::to_vec(&serde_json::json!({
            "bucket": "photos",
            "key": "f",
            "expires_in_hours": i64::MAX,
        }))
        .unwrap();
        let out = handle_create_share(&ctx, &msg, InputStream::from_bytes(body)).await;
        assert!(
            output_is_error(out, "InvalidArgument").await,
            "huge expiry must be a 400, not a handler panic"
        );
    }

    /// Zero/negative hours would mint an already-expired share (or, for
    /// very negative values, also overflow the same arithmetic) — rejected
    /// the same as an out-of-range positive value.
    #[tokio::test]
    async fn create_share_rejects_non_positive_expiry() {
        for hours in [0_i64, -1, i64::MIN] {
            let ctx = ctx_sharing("f").await;
            let msg = auth_msg("create", "/b/cloudstorage/shares", "alice");
            let body = serde_json::to_vec(&serde_json::json!({
                "bucket": "photos",
                "key": "f",
                "expires_in_hours": hours,
            }))
            .unwrap();
            let out = handle_create_share(&ctx, &msg, InputStream::from_bytes(body)).await;
            assert!(
                output_is_error(out, "InvalidArgument").await,
                "non-positive expires_in_hours ({hours}) must be rejected"
            );
        }
    }

    /// The range/overflow guard must not reject legitimate input: a normal
    /// in-range value still produces a share whose persisted `expires_at`
    /// is a correct ~24h-out timestamp.
    #[tokio::test]
    async fn create_share_valid_expiry_produces_future_timestamp() {
        let ctx = ctx_sharing("f").await;
        let msg = auth_msg("create", "/b/cloudstorage/shares", "alice");
        let body = serde_json::to_vec(&serde_json::json!({
            "bucket": "photos",
            "key": "f",
            "expires_in_hours": 24,
        }))
        .unwrap();
        let before = chrono::Utc::now();
        let out = handle_create_share(&ctx, &msg, InputStream::from_bytes(body)).await;
        let resp = output_json(out).await;
        let id = resp
            .get("id")
            .and_then(|v| v.as_str())
            .expect("successful create_share returns an id")
            .to_string();

        let row = repo::shares::find_by_id(&ctx, &id)
            .await
            .expect("share row");
        let expires_at = row
            .expires_at
            .as_deref()
            .expect("expires_at set for a 24h share");
        let parsed = chrono::DateTime::parse_from_rfc3339(expires_at)
            .expect("valid rfc3339")
            .with_timezone(&chrono::Utc);
        let expected_min = before + chrono::Duration::hours(23);
        let expected_max = before + chrono::Duration::hours(25);
        assert!(
            parsed > expected_min && parsed < expected_max,
            "expires_at should be ~24h in the future, got {expires_at}"
        );
    }
}
