use wafer_core::clients::storage as store;
use wafer_run::{context::Context, ErrorCode, HttpMethod, InputStream, Message, OutputStream};

use super::repo;
use crate::{
    endpoint_match::{self, EndpointRoute},
    http::{err_bad_request, err_forbidden, err_internal, err_not_found, ok_json},
};

/// In-block dispatch targets for the user storage API.
#[derive(Clone, Copy)]
enum Route {
    ListBuckets,
    CreateBucket,
    ListObjects,
    GetObject,
    UploadObject,
    DeleteObject,
    DeleteBucket,
    Search,
    Recent,
}

/// Dispatch table over the REAL on-the-wire `/b/storage/api/...` suffixes —
/// no path rewrite. The object-key routes use a trailing `{key...}` rest
/// param (keys may contain `/`); the more-specific `.../objects/{key...}`
/// templates precede the bare `.../objects` and `.../{name}` ones so ordering
/// resolves them like the old `contains("/objects/")` guards. `{name}` and
/// `{key}` bind into `req.param.*`.
const ROUTES: &[EndpointRoute<Route>] = &[
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/storage/api/buckets",
        Route::ListBuckets,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/storage/api/buckets",
        Route::CreateBucket,
    ),
    EndpointRoute::new(HttpMethod::Get, "/b/storage/api/search", Route::Search),
    EndpointRoute::new(HttpMethod::Get, "/b/storage/api/recent", Route::Recent),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/storage/api/buckets/{name}/objects/{key...}",
        Route::GetObject,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/storage/api/buckets/{name}/objects/{key...}",
        Route::DeleteObject,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/storage/api/buckets/{name}/objects",
        Route::ListObjects,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/storage/api/buckets/{name}/objects",
        Route::UploadObject,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/storage/api/buckets/{name}",
        Route::DeleteBucket,
    ),
];

pub async fn handle(ctx: &dyn Context, mut msg: Message, input: InputStream) -> OutputStream {
    let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
        return err_not_found("not found");
    };
    match route {
        Route::ListBuckets => handle_list_buckets(ctx, &msg).await,
        Route::CreateBucket => handle_create_bucket(ctx, &msg, input).await,
        Route::ListObjects => handle_list_objects(ctx, &msg).await,
        Route::GetObject => handle_get_object(ctx, &msg).await,
        Route::UploadObject => handle_upload_object(ctx, &msg, input).await,
        Route::DeleteObject => handle_delete_object(ctx, &msg).await,
        Route::DeleteBucket => handle_delete_bucket(ctx, &msg).await,
        Route::Search => handle_search(ctx, &msg).await,
        Route::Recent => handle_recent(ctx, &msg).await,
    }
}

/// Admin storage API, delegated from the admin block via `call_block` on the
/// real `/admin/storage/...` paths. Authorization is enforced by the admin
/// block's central tier before delegation.
pub async fn handle_admin(ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
    let action = msg.action();
    let path = msg.path();
    match (action, path) {
        ("retrieve", "/admin/storage/buckets") => handle_list_buckets(ctx, &msg).await,
        ("retrieve", "/admin/storage/stats") => handle_stats(ctx, &msg).await,
        _ => err_not_found("not found"),
    }
}

/// Extract the bucket name. Prefers the matcher-bound `{name}` path var, with a
/// prefix-strip fallback for the admin delegation path and hand-built tests.
fn extract_bucket_name(msg: &Message) -> String {
    let var = msg.var("name");
    if !var.is_empty() {
        return var.to_string();
    }
    let path = msg.path();
    let rest = path
        .strip_prefix("/b/storage/api/buckets/")
        .or_else(|| path.strip_prefix("/admin/storage/buckets/"))
        .unwrap_or("");
    match rest.find('/') {
        Some(idx) => rest[..idx].to_string(),
        None => rest.to_string(),
    }
}

/// Extract the object key (may contain `/`). Prefers the matcher-bound
/// `{key...}` rest param, falling back to the substring after `/objects/`.
fn extract_object_key(msg: &Message) -> String {
    let var = msg.var("key");
    if !var.is_empty() {
        return var.to_string();
    }
    let path = msg.path();
    match path.find("/objects/") {
        Some(idx) => path[idx + 9..].to_string(),
        None => String::new(),
    }
}

/// True when `user_id` owns a bucket named `bucket` (i.e.
/// [`repo::buckets::find_owned`] finds a matching row). DB errors are
/// logged and treated as "not owned" (fail closed).
///
/// This is the single ownership predicate for the files block. Callers
/// decide the admin policy on top of it:
/// - JSON API handlers go through [`is_bucket_access_denied`], which grants
///   admins access to every bucket.
/// - The SSR user portal (`pages_user::object_list_page`) deliberately does
///   NOT bypass for admins — the portal is strictly owner-scoped so an
///   admin browsing `/b/storage/` sees only their own buckets; cross-user
///   inspection happens via the admin pages instead.
pub(super) async fn bucket_owned_by(ctx: &dyn Context, user_id: &str, bucket: &str) -> bool {
    match repo::buckets::find_owned(ctx, bucket, user_id).await {
        Ok(record) => record.is_some(),
        Err(e) => {
            tracing::warn!(error = %e, bucket = %bucket, "bucket-ownership check failed");
            false
        }
    }
}

/// Check if the current user owns the given bucket (or is admin).
/// Returns true if access is denied. See [`bucket_owned_by`] for the
/// admin-bypass policy split between the JSON API and the SSR portal.
pub(super) async fn is_bucket_access_denied(
    ctx: &dyn Context,
    msg: &Message,
    bucket: &str,
) -> bool {
    if crate::util::is_admin(msg) {
        return false;
    }
    !bucket_owned_by(ctx, msg.user_id(), bucket).await
}

/// Validate a storage key for path traversal attacks.
/// Rejects keys containing `..`, absolute paths, null bytes, or backslashes.
///
/// SEC-064: backslash is rejected because backends running on Windows-style
/// paths (or any backend that ever normalises `\` to `/`) would otherwise
/// allow `..\..\etc\passwd`-style traversal that the `..` check alone would
/// not catch when the segment separator is `\` instead of `/`.
///
/// `pub(super)` so the share-creation path (`cloud.rs::handle_create_share`)
/// enforces exactly the same rule rather than re-inlining a copy that drifts
/// (the SEC-064 backslash check was the missing piece there).
pub(super) fn is_valid_storage_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains("..")
        && !key.starts_with('/')
        && !key.contains('\0')
        && !key.contains('\\')
}

/// Minimum / maximum bucket-name length (S3-compatible).
pub(super) const BUCKET_NAME_MIN_LEN: usize = 3;
pub(super) const BUCKET_NAME_MAX_LEN: usize = 63;

/// HTML5 `pattern=` attribute source for the bucket-name input — the single
/// source of truth shared with the server-side [`is_valid_bucket_name`] check
/// so the client modal and the API enforce identically. S3-compatible:
/// lowercase letters, digits, and hyphens; must start and end with a letter
/// or digit. (Length is enforced separately via `minlength`/`maxlength` on the
/// input and the length check in [`is_valid_bucket_name`].)
pub(super) const BUCKET_NAME_PATTERN: &str = "[a-z0-9]([a-z0-9-]*[a-z0-9])?";

/// Validate a bucket name against the S3-compatible rule the client modal
/// advertises ([`BUCKET_NAME_PATTERN`] + length bounds): 3–63 chars,
/// lowercase letters / digits / hyphens, must start and end with a letter or
/// digit. This rejects path traversal (`..`, `/`, `\`), NUL, uppercase, and
/// leading/trailing hyphens by construction.
///
/// `pub(super)` so the share path uses the identical rule.
pub(super) fn is_valid_bucket_name(name: &str) -> bool {
    let len = name.len();
    if !(BUCKET_NAME_MIN_LEN..=BUCKET_NAME_MAX_LEN).contains(&len) {
        return false;
    }
    let bytes = name.as_bytes();
    let is_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    // First and last char must be a lowercase letter or digit.
    if !is_alnum(bytes[0]) || !is_alnum(bytes[len - 1]) {
        return false;
    }
    // Interior chars: lowercase letter, digit, or hyphen.
    bytes.iter().all(|&b| is_alnum(b) || b == b'-')
}

/// Collect an `InputStream` into `Vec<u8>` with a hard size cap. Errors out
/// as soon as the running total exceeds `cap_bytes`, so a multi-GB body
/// can't OOM the process before we check quota. Returns `Err(())` when
/// the cap is exceeded.
async fn collect_with_cap(
    mut input: wafer_run::InputStream,
    cap_bytes: i64,
) -> Result<Vec<u8>, ()> {
    use futures::StreamExt;
    let cap = if cap_bytes <= 0 {
        usize::MAX
    } else {
        cap_bytes as usize
    };
    let mut out = Vec::new();
    while let Some(chunk) = input.next().await {
        if out.len().saturating_add(chunk.len()) > cap {
            return Err(());
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

async fn handle_list_buckets(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // [`repo::buckets::TABLE`] is the single source of truth for bucket
    // existence / ownership / visibility. Both the admin and user branches
    // read it (the admin sees every bucket, the user only their own) —
    // storage folders are a blob namespace, not a directory we enumerate
    // here, so the admin list no longer diverges from `store::list_folders`.
    let owner = if crate::util::is_admin(msg) {
        None
    } else {
        Some(msg.user_id())
    };
    match repo::buckets::list_visible(ctx, owner).await {
        Ok(records) => {
            let names: Vec<&str> = records
                .iter()
                .filter_map(|r| r.data.get("name").and_then(|v| v.as_str()))
                .collect();
            ok_json(&serde_json::json!({"buckets": names}))
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_create_bucket(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        #[serde(default)]
        public: bool,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    if body.name.is_empty() {
        return err_bad_request("Bucket name is required");
    }
    if !is_valid_bucket_name(&body.name) {
        return err_bad_request("Invalid bucket name");
    }

    // Create the blob-namespace folder first, then record the metadata row.
    if let Err(e) = store::create_folder(ctx, &body.name, body.public).await {
        return err_internal("Failed to create bucket", e);
    }

    // [`repo::buckets::TABLE`] is the source of truth for bucket existence,
    // so the metadata insert must succeed for the bucket to count as created.
    // If it fails, compensate by deleting the just-created folder rather than
    // warn-and-continue (which would leave an orphan folder invisible to every
    // listing path, which now all read the table).
    if let Err(e) = repo::buckets::insert(ctx, &body.name, body.public, msg.user_id()).await {
        if let Err(cleanup) = store::delete_folder(ctx, &body.name).await {
            tracing::error!(
                bucket = %body.name,
                error = %cleanup,
                "failed to roll back orphan storage folder after bucket metadata insert failed",
            );
        }
        return err_internal("Failed to create bucket", e);
    }
    ok_json(&serde_json::json!({"name": body.name, "created": true}))
}

async fn handle_delete_bucket(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let bucket = extract_bucket_name(msg);
    let bucket = bucket.as_str();
    if bucket.is_empty() {
        return err_bad_request("Missing bucket name");
    }
    if !is_valid_bucket_name(bucket) {
        return err_bad_request("Invalid bucket name");
    }
    if is_bucket_access_denied(ctx, msg, bucket).await {
        return err_forbidden("Access denied to this bucket");
    }

    match store::delete_folder(ctx, bucket).await {
        Ok(()) => {
            // Clean up DB metadata for the bucket and its objects
            repo::buckets::delete_by_name(ctx, bucket).await.ok();
            repo::objects::delete_for_bucket(ctx, bucket).await.ok();
            ok_json(&serde_json::json!({"deleted": true}))
        }
        Err(e) => err_internal("Failed to delete bucket", e),
    }
}

async fn handle_list_objects(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let bucket = extract_bucket_name(msg);
    let bucket = bucket.as_str();
    if bucket.is_empty() {
        return err_bad_request("Missing bucket name");
    }
    if !is_valid_bucket_name(bucket) {
        return err_bad_request("Invalid bucket name");
    }
    if is_bucket_access_denied(ctx, msg, bucket).await {
        return err_forbidden("Access denied to this bucket");
    }

    let prefix = msg.query("prefix").to_string();
    let (_, page_size, offset) = msg.pagination_params(50);

    let opts = store::ListOptions {
        prefix,
        limit: page_size as i64,
        offset: offset as i64,
        // Offset-only paging; `None` preserves the existing behavior.
        cursor: None,
    };

    match store::list(ctx, bucket, &opts).await {
        Ok(list) => ok_json(&list),
        Err(e) => err_internal("Storage error", e),
    }
}

async fn handle_get_object(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let bucket = extract_bucket_name(msg);
    let bucket = bucket.as_str();
    let key = extract_object_key(msg);
    let key = key.as_str();
    if bucket.is_empty() || key.is_empty() {
        return err_bad_request("Missing bucket name or object key");
    }
    if !is_valid_storage_key(key) {
        return err_bad_request("Invalid object key");
    }
    if is_bucket_access_denied(ctx, msg, bucket).await {
        return err_forbidden("Access denied to this bucket");
    }

    // Track view in DB
    if let Err(e) = repo::views::insert(ctx, bucket, key, msg.user_id()).await {
        tracing::warn!("Failed to track storage object view: {e}");
    }

    // Stream the object body straight from storage (R2 `get_streaming` on CF)
    // rather than buffering the whole object into the isolate: `get_stream`
    // returns the `ObjectInfo` header eagerly, then the body flows chunk by
    // chunk. The leading meta carries the streaming opt-in marker + the real
    // content-type so the pipeline and platform adapter take the streaming
    // response path (see `crate::streaming`).
    match store::get_stream(ctx, bucket, key).await {
        Ok(stream) => {
            let content_type = resolved_content_type(stream.info());
            let leading = crate::streaming::download_leading_meta(&content_type, &[]);
            crate::streaming::stream_download(stream, leading)
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Object not found"),
        Err(e) => err_internal("Storage error", e),
    }
}

/// The object's stored content-type, falling back to `application/octet-stream`
/// when the backend reports none (parity with the buffered `get` path, which
/// R2/S3 default the same way).
fn resolved_content_type(info: &store::ObjectInfo) -> String {
    if info.content_type.is_empty() {
        "application/octet-stream".to_string()
    } else {
        info.content_type.clone()
    }
}

async fn handle_upload_object(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let bucket = extract_bucket_name(msg);
    let bucket = bucket.as_str();
    if bucket.is_empty() {
        return err_bad_request("Missing bucket name");
    }

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
    if is_bucket_access_denied(ctx, msg, bucket).await {
        return err_forbidden("Access denied to this bucket");
    }

    // Best-effort sweep before quota check: orphan `pending` rows (from
    // previous uploads where the storage put failed AND the compensating
    // delete also failed) would otherwise inflate this user's quota usage
    // and lock them out. 1h cutoff.
    super::quota::sweep_stale_pending(ctx, msg.user_id(), 3600).await;

    // Stream the upload body chunk-by-chunk so an attacker who streams a
    // multi-GB body can't OOM us before quota check fires. Two bounds:
    //   - per-file `max_file_size_bytes` (cheap to check on the running
    //     total; abort as soon as the chunked total exceeds it)
    //   - total `max_storage_bytes` (depends on current usage; checked once
    //     after we know the body's full size)
    // The chunked check uses the user's *file-size* cap as a hard ceiling
    // since that's the smaller of the two. For multipart bodies the cap
    // applies to the envelope — a slight over-estimate (the extracted file
    // is always smaller than its envelope), never an under-estimate.
    let quota = super::quota::get_user_quota(ctx, msg.user_id()).await;
    let Ok(body_bytes) = collect_with_cap(input, quota.max_file_size_bytes).await else {
        return err_bad_request(&format!(
            "File exceeds maximum size of {} bytes",
            quota.max_file_size_bytes
        ));
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

    if let Err(r) = super::quota::check_quota(ctx, msg.user_id(), content.len() as i64).await {
        return r;
    }

    // Insert a pending record BEFORE uploading so concurrent quota checks see it.
    // This closes the TOCTOU race between check_quota and the actual upload.
    let pending_record = match repo::objects::insert_pending(
        ctx,
        bucket,
        &key,
        content.len(),
        &content_type,
        msg.user_id(),
    )
    .await
    {
        Ok(record) => record,
        Err(e) => return err_internal("Failed to reserve upload slot", e),
    };

    match store::put(ctx, bucket, &key, &content, &content_type).await {
        Ok(()) => {
            // Upload succeeded — mark the pending record as complete.
            if let Err(e) = repo::objects::mark_complete(ctx, &pending_record.id).await {
                tracing::warn!("Failed to mark upload as complete: {e}");
            }
            ok_json(&serde_json::json!({"bucket": bucket, "key": key, "uploaded": true}))
        }
        Err(e) => {
            // Upload failed — delete the pending record so it doesn't block quota.
            if let Err(del_err) = repo::objects::delete(ctx, &pending_record.id).await {
                tracing::warn!("Failed to clean up pending record: {del_err}");
            }
            err_internal("Upload failed", e)
        }
    }
}

async fn handle_delete_object(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let bucket = extract_bucket_name(msg);
    let bucket = bucket.as_str();
    let key = extract_object_key(msg);
    let key = key.as_str();
    if bucket.is_empty() || key.is_empty() {
        return err_bad_request("Missing bucket name or object key");
    }
    if !is_valid_storage_key(key) {
        return err_bad_request("Invalid object key");
    }
    if is_bucket_access_denied(ctx, msg, bucket).await {
        return err_forbidden("Access denied to this bucket");
    }

    match store::delete(ctx, bucket, key).await {
        Ok(()) => {
            // Clean up metadata
            repo::objects::delete_by_bucket_key(ctx, bucket, key)
                .await
                .ok();
            ok_json(&serde_json::json!({"deleted": true}))
        }
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Object not found"),
        Err(e) => err_internal("Delete failed", e),
    }
}

async fn handle_search(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = msg.query("q").to_string();
    if query.is_empty() {
        return err_bad_request("Missing search query");
    }

    let (_, page_size, offset) = msg.pagination_params(20);
    match repo::objects::search_completed(
        ctx,
        msg.user_id(),
        &query,
        page_size as i64,
        offset as i64,
    )
    .await
    {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Search failed", e),
    }
}

async fn handle_recent(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match repo::views::list_recent_for_user(ctx, msg.user_id(), 20).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a message carrying `path` on `req.resource` and, optionally, the
    /// matcher-bound `{name}`/`{key}` path vars in `req.param.*`.
    fn msg_with(path: &str, params: &[(&str, &str)]) -> Message {
        let mut m = Message::new("test");
        m.set_meta("req.resource", path);
        for (k, v) in params {
            m.set_meta(format!("req.param.{k}"), *v);
        }
        m
    }

    #[test]
    fn test_extract_bucket_name_from_param() {
        // Router-populated path var wins (the normal dispatch path).
        let m = msg_with(
            "/b/storage/api/buckets/my-bucket/objects",
            &[("name", "my-bucket")],
        );
        assert_eq!(extract_bucket_name(&m), "my-bucket");
    }

    #[test]
    fn test_extract_bucket_name_prefix_fallback() {
        // Fallback for the admin delegation path / hand-built messages.
        assert_eq!(
            extract_bucket_name(&msg_with("/b/storage/api/buckets/my-bucket", &[])),
            "my-bucket"
        );
        assert_eq!(
            extract_bucket_name(&msg_with("/b/storage/api/buckets/my-bucket/objects", &[])),
            "my-bucket"
        );
        assert_eq!(
            extract_bucket_name(&msg_with("/admin/storage/buckets/admin-bucket", &[])),
            "admin-bucket"
        );
        assert_eq!(extract_bucket_name(&msg_with("/other/path", &[])), "");
    }

    #[test]
    fn test_extract_object_key_from_param() {
        // Rest param preserves embedded slashes.
        let m = msg_with(
            "/b/storage/api/buckets/b/objects/dir/file.txt",
            &[("key", "dir/file.txt")],
        );
        assert_eq!(extract_object_key(&m), "dir/file.txt");
    }

    #[test]
    fn test_extract_object_key_prefix_fallback() {
        assert_eq!(
            extract_object_key(&msg_with("/b/storage/api/buckets/b/objects/file.txt", &[])),
            "file.txt"
        );
        assert_eq!(
            extract_object_key(&msg_with(
                "/b/storage/api/buckets/b/objects/dir/file.txt",
                &[]
            )),
            "dir/file.txt"
        );
        assert_eq!(
            extract_object_key(&msg_with("/b/storage/api/buckets/b", &[])),
            ""
        );
        assert_eq!(extract_object_key(&msg_with("/other/path", &[])), "");
    }

    #[test]
    fn test_is_valid_storage_key() {
        // Valid keys
        assert!(is_valid_storage_key("file.txt"));
        assert!(is_valid_storage_key("dir/file.txt"));
        assert!(is_valid_storage_key("a/b/c/file.txt"));
        assert!(is_valid_storage_key("file-name_123.txt"));

        // Invalid keys
        assert!(!is_valid_storage_key(""));
        assert!(!is_valid_storage_key("../etc/passwd"));
        assert!(!is_valid_storage_key("dir/../secret"));
        assert!(!is_valid_storage_key("/absolute/path"));
        assert!(!is_valid_storage_key("file\0name"));
        assert!(!is_valid_storage_key(".."));
    }

    #[test]
    fn test_is_valid_bucket_name() {
        // Valid bucket names (S3-compatible: 3-63 chars, lowercase/digits/
        // hyphens, start+end alnum).
        assert!(is_valid_bucket_name("my-bucket"));
        assert!(is_valid_bucket_name("bucket123"));
        assert!(is_valid_bucket_name("uploads"));
        assert!(is_valid_bucket_name("a1b"));

        // Invalid bucket names
        assert!(!is_valid_bucket_name(""));
        assert!(!is_valid_bucket_name("../other"));
        assert!(!is_valid_bucket_name("bucket/subdir"));
        assert!(!is_valid_bucket_name("bucket\0name"));
        assert!(!is_valid_bucket_name(".."));
        // Too short / too long.
        assert!(!is_valid_bucket_name("ab"));
        assert!(!is_valid_bucket_name(&"a".repeat(64)));
        // Uppercase rejected (S3 rule + matches the modal pattern).
        assert!(!is_valid_bucket_name("MyBucket"));
        // Leading / trailing hyphen rejected (start+end must be alnum).
        assert!(!is_valid_bucket_name("-bucket"));
        assert!(!is_valid_bucket_name("bucket-"));
        // Backslash rejected (SEC-064; not in the allowed alphabet).
        assert!(!is_valid_bucket_name("bucket\\name"));
    }

    /// The server-side validator enforces the same alphabet the HTML
    /// `pattern=` attribute ([`BUCKET_NAME_PATTERN`]) advertises, so the client
    /// modal and the API agree on what a valid bucket name is (modulo length,
    /// which the input enforces separately via minlength/maxlength). This pins
    /// the cases the pattern accepts/rejects against the validator.
    #[test]
    fn bucket_name_validator_matches_advertised_pattern() {
        // Sanity-check the constant is the S3 alphabet we documented.
        assert_eq!(BUCKET_NAME_PATTERN, "[a-z0-9]([a-z0-9-]*[a-z0-9])?");
        // Pattern-accepted names (length-valid) the validator must accept.
        for name in ["my-bucket", "bucket123", "a1b", "abc"] {
            assert!(is_valid_bucket_name(name), "validator should accept {name}");
        }
        // Pattern-rejected names the validator must reject.
        for name in ["MyBucket", "-bucket", "bucket-", "bucket/sub", "bucket\\x"] {
            assert!(
                !is_valid_bucket_name(name),
                "validator should reject {name}"
            );
        }
    }
}

#[cfg(test)]
mod integration_tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use serde_json::json;
    use wafer_core::{
        interfaces::storage::service::{
            FolderInfo, ListOptions as StoreListOptions, ObjectInfo, ObjectList, StorageError,
            StorageService,
        },
        service_blocks::storage::StorageBlock,
    };

    use super::*;
    use crate::test_support::{admin_msg, auth_msg, output_json, TestContext};

    /// `(folder, key)` → `(bytes, content_type)`.
    type MemObjects = HashMap<(String, String), (Vec<u8>, String)>;

    /// In-memory [`StorageService`] so upload tests exercise the production
    /// `wafer-run/storage` [`StorageBlock`] wire protocol end-to-end (the
    /// typed `store::put`/`store::get` clients round-trip through the real
    /// handler) without touching the filesystem.
    #[derive(Default)]
    struct MemStorage {
        objects: Mutex<MemObjects>,
    }

    #[async_trait]
    impl StorageService for MemStorage {
        async fn put(
            &self,
            folder: &str,
            key: &str,
            data: &[u8],
            content_type: &str,
        ) -> Result<(), StorageError> {
            self.objects.lock().unwrap().insert(
                (folder.to_string(), key.to_string()),
                (data.to_vec(), content_type.to_string()),
            );
            Ok(())
        }

        async fn get(
            &self,
            folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
            let guard = self.objects.lock().unwrap();
            let (data, content_type) = guard
                .get(&(folder.to_string(), key.to_string()))
                .ok_or(StorageError::NotFound)?;
            Ok((
                data.clone(),
                ObjectInfo {
                    key: key.to_string(),
                    size: data.len() as i64,
                    content_type: content_type.clone(),
                    last_modified: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                        .expect("epoch"),
                },
            ))
        }

        async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
            self.objects
                .lock()
                .unwrap()
                .remove(&(folder.to_string(), key.to_string()));
            Ok(())
        }

        async fn list(
            &self,
            _folder: &str,
            _opts: &StoreListOptions,
        ) -> Result<ObjectList, StorageError> {
            Ok(ObjectList {
                objects: vec![],
                total_count: 0,
                next_cursor: None,
            })
        }

        async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
            Ok(())
        }

        async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
            Ok(())
        }

        async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
            Ok(vec![])
        }
    }

    /// `TestContext::with_files()` plus a real `wafer-run/storage` block over
    /// [`MemStorage`], so `handle_upload_object` can complete its `store::put`.
    async fn ctx_with_storage() -> TestContext {
        let mut ctx = TestContext::with_files().await;
        ctx.register_block(
            "wafer-run/storage",
            Arc::new(StorageBlock::new(Arc::new(MemStorage::default()))),
        );
        ctx
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
        store::put(&ctx, "assets", "pic.png", b"PNGDATA", "image/png")
            .await
            .expect("seed object");

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

    /// Fetch the single object-metadata row (asserting there is exactly
    /// one) and return its `(size, content_type, status)`.
    async fn sole_object_row(ctx: &TestContext) -> (i64, String, String) {
        let rows = repo::objects::list_all(ctx)
            .await
            .expect("list object rows");
        assert_eq!(rows.len(), 1, "expected exactly one object metadata row");
        let data = &rows[0].data;
        (
            data.get("size")
                .and_then(crate::util::json_as_i64)
                .expect("size field"),
            data.get("content_type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            data.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        )
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

        let (stored, info) = store::get(&ctx, "site-assets", "index.html")
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
        assert_eq!(status, "complete");
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

        let (stored, info) = store::get(&ctx, "raw-bucket", "notes.txt")
            .await
            .expect("stored object");
        assert_eq!(stored, body, "raw body must be stored unchanged");
        assert_eq!(info.content_type, "text/plain");

        let (size, content_type, status) = sole_object_row(&ctx).await;
        assert_eq!(size, body.len() as i64);
        assert_eq!(content_type, "text/plain");
        assert_eq!(status, "complete");
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

        let (stored, _) = store::get(&ctx, "site-assets", "from-part.html")
            .await
            .expect("stored object");
        assert_eq!(stored, file_bytes);
    }

    async fn seed_bucket(ctx: &TestContext, name: &str, owner: &str) {
        let data = crate::util::json_map(json!({
            "name": name,
            "public": false,
            "created_by": owner,
            "created_at": crate::util::now_rfc3339(),
        }));
        repo::buckets::seed(ctx, data).await.expect("seed bucket");
    }

    fn bucket_names(v: &serde_json::Value) -> Vec<String> {
        v.get("buckets")
            .and_then(|b| b.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Seed an object-metadata row directly (bypassing `handle_upload_object`
    /// / the real storage backend, same as [`seed_bucket`] does for buckets) —
    /// enough to exercise `handle_search`'s DB query.
    async fn seed_object(ctx: &TestContext, bucket: &str, key: &str, owner: &str) {
        let data = crate::util::json_map(json!({
            "bucket": bucket,
            "key": key,
            "size": 0,
            "content_type": "application/octet-stream",
            "status": "complete",
            "uploaded_by": owner,
            "uploaded_at": crate::util::now_rfc3339(),
        }));
        repo::objects::seed(ctx, data).await.expect("seed object");
    }

    fn search_result_keys(v: &serde_json::Value) -> Vec<String> {
        v.get("records")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|rec| {
                        rec.get("data")
                            .and_then(|d| d.get("key"))
                            .and_then(|k| k.as_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Single source of truth: the admin bucket listing now reads
    /// [`repo::buckets::TABLE`] (every bucket) instead of `store::list_folders`,
    /// can no longer diverge from the per-user listing that already read the
    /// table. An admin sees all buckets regardless of owner.
    #[tokio::test]
    async fn admin_list_buckets_reads_metadata_table_for_all_owners() {
        let ctx = TestContext::with_files().await;
        seed_bucket(&ctx, "alice-bucket", "alice").await;
        seed_bucket(&ctx, "bob-bucket", "bob").await;

        let out = handle_list_buckets(&ctx, &admin_msg("retrieve", "/storage/buckets")).await;
        let mut names = bucket_names(&output_json(out).await);
        names.sort();
        assert_eq!(names, vec!["alice-bucket", "bob-bucket"]);
    }

    /// A non-admin user sees only the buckets they own (same table, filtered).
    #[tokio::test]
    async fn user_list_buckets_is_owner_scoped() {
        let ctx = TestContext::with_files().await;
        seed_bucket(&ctx, "alice-bucket", "alice").await;
        seed_bucket(&ctx, "bob-bucket", "bob").await;

        let out =
            handle_list_buckets(&ctx, &auth_msg("retrieve", "/storage/buckets", "alice")).await;
        let names = bucket_names(&output_json(out).await);
        assert_eq!(names, vec!["alice-bucket"]);
    }

    /// `handle_stats` counts buckets from [`repo::buckets::TABLE`] (the same source
    /// admin SSR overview uses), not by enumerating storage folders.
    #[tokio::test]
    async fn stats_counts_buckets_from_metadata_table() {
        let ctx = TestContext::with_files().await;
        seed_bucket(&ctx, "one", "alice").await;
        seed_bucket(&ctx, "two", "bob").await;

        let out = handle_stats(&ctx, &admin_msg("retrieve", "/admin/storage/stats")).await;
        let body = output_json(out).await;
        assert_eq!(body.get("bucket_count").and_then(|v| v.as_i64()), Some(2));
    }

    /// Regression test for SB-5. [`escape_like`] backslash-escapes `_`/`%`/`\`
    /// in the search term, but that escaping is only *effective* because
    /// `handle_search`'s `FilterOp::Like` filter now renders an explicit
    /// `ESCAPE '\'` clause (`wafer-sql-utils`, SB-5A) — SQLite's `LIKE` has NO
    /// default escape character, so a bare `\` in the pattern is just an
    /// ordinary literal byte without that clause.
    ///
    /// Seeds a file whose name contains `_` (`my_report.pdf`) alongside a
    /// decoy that an *unescaped* `_` wildcard would also match
    /// (`myXreport.pdf`); asserting the result is exactly the underscore file
    /// (not zero, not both) rules out either pre-SB-5A failure mode. Verified
    /// (2026-07-11) against wafer-run main (543e788, pre-ESCAPE): the pattern
    /// becomes `%my\_report%` with a literal backslash that appears in no
    /// real filename, so the query actually matched **zero** rows — worse
    /// than "underscore still wildcards", `escape_like`'s output broke search
    /// entirely on SQLite/D1. Against wafer-run `fix/sb5a-sql-like-escape`
    /// (b1e6c68, ESCAPE `'\'` present) this passes.
    #[tokio::test]
    async fn search_escapes_underscore_as_literal_not_wildcard() {
        let ctx = TestContext::with_files().await;
        seed_object(&ctx, "bucket", "my_report.pdf", "alice").await;
        // Decoy: only matches `%my_report%` if `_` is treated as a
        // single-char wildcard instead of the literal `_` it should be.
        seed_object(&ctx, "bucket", "myXreport.pdf", "alice").await;

        let mut msg = auth_msg("retrieve", "/b/storage/api/search", "alice");
        msg.set_meta("req.query.q", "my_report");

        let out = handle_search(&ctx, &msg).await;
        let keys = search_result_keys(&output_json(out).await);
        assert_eq!(
            keys,
            vec!["my_report.pdf"],
            "underscore in query must be escaped as a literal, not treated as a wildcard (got: {keys:?})"
        );
    }
}

async fn handle_stats(ctx: &dyn Context, _msg: &Message) -> OutputStream {
    let total_objects = repo::objects::count_completed(ctx).await.unwrap_or(0);
    let total_size = repo::objects::sum_size_completed(ctx).await.unwrap_or(0.0);
    // Count buckets from the metadata table (single source of truth), the same
    // way the admin SSR overview does, rather than enumerating storage folders.
    let bucket_count = repo::buckets::count_all(ctx).await.unwrap_or(0);

    ok_json(&serde_json::json!({
        "total_objects": total_objects,
        "total_size_bytes": total_size as i64,
        "bucket_count": bucket_count
    }))
}
