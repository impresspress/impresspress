mod cloud;
pub(crate) mod migrations;
pub(crate) mod models;
mod pages_admin;
pub(crate) mod pages_user;
mod quota;
pub(crate) mod repo;
mod share;
pub(crate) mod storage;

use wafer_run::{BlockEndpoint, BlockInfo, InstanceMode};

use super::rate_limit::{check_user_rate_limit_with, RateLimit, RateLimitOutcome, UserRateLimiter};
use crate::http::err_not_found;

crate::impresspress_feature_block! {
    /// File storage: buckets, objects, shares, quotas (`impresspress/files`).
    pub struct FilesBlock;
    fields: { limiter: UserRateLimiter },
    name: "impresspress/files",
    info: |_this| {
        use wafer_run::{AuthLevel, CollectionSchema};

        BlockInfo::new("impresspress/files", "0.0.1", "http-handler@v1", "File storage, sharing, quotas, and access logging")
            .instance_mode(InstanceMode::Singleton)
            .requires(vec!["wafer-run/database".into(), "wafer-run/storage".into(), "wafer-run/config".into()])
            // No explicit Storage grant needed. Wave 26 (c18) made WRAP
            // namespace-aware for Storage; this block self-admits its
            // own `impresspress/files/*` namespace via Rule 3.
            // Advisory table list — admin "Database tables" discovery + the
            // WRAP grant-UI read only `CollectionSchema::name`. The schema
            // itself (columns, indexes, FKs, quota defaults) lives solely in
            // the block's hand-authored `migrations/*.sqlite.sql` files (the
            // single source for both runtime `migrations::apply()` and the
            // Cloudflare D1 build).
            .collections(vec![
                CollectionSchema::new(repo::buckets::TABLE),
                CollectionSchema::new(repo::objects::TABLE),
                CollectionSchema::new(repo::views::TABLE),
                CollectionSchema::new(repo::shares::TABLE),
                CollectionSchema::new(repo::shares::ACCESS_LOGS_TABLE),
                CollectionSchema::new(repo::quota::TABLE),
            ])
            .category(wafer_run::BlockCategory::Feature)
            .description("File storage and management with bucket-based organization. Supports file upload, download, deletion, search, and sharing via public links with expiration and access counting. Includes per-user storage quotas.")
            .endpoints(vec![
                BlockEndpoint::get("/b/storage/").summary("Bucket list (user)").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/storage/{bucket}/").summary("Object list").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/storage/{bucket}/{prefix...}/").summary("Object list (nested)").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/storage/api/buckets").summary("List buckets").auth(AuthLevel::Authenticated),
                BlockEndpoint::post("/b/storage/api/buckets").summary("Create bucket").auth(AuthLevel::Authenticated),
                // Schemas below mirror the real shapes read from
                // `storage.rs` (`handle_list_objects`/`handle_get_object`,
                // wire types `wafer_block::wire::storage::{ObjectList,
                // ObjectInfo}`). These are the two highest-value developer
                // endpoints for browsing a bucket; full coverage of the
                // remaining storage routes (buckets, shares, quotas) is a
                // follow-up.
                BlockEndpoint::get("/b/storage/api/buckets/{name}/objects")
                    .summary("List objects")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(serde_json::json!({
                        "type": "object",
                        "required": ["name"],
                        "properties": {
                            "name": {"type": "string", "description": "Bucket name"}
                        }
                    }))
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "prefix": {"type": "string", "description": "Key prefix filter"},
                            "page": {"type": "integer", "default": 1},
                            "page_size": {"type": "integer", "default": 50, "maximum": 100}
                        }
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "objects": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "key": {"type": "string"},
                                        "size": {"type": "integer", "description": "Size in bytes"},
                                        "content_type": {"type": "string"},
                                        "last_modified": {"type": "string", "format": "date-time"}
                                    }
                                }
                            },
                            "total_count": {"type": "integer"}
                        }
                    }))
                    .tags(&["storage"]),
                BlockEndpoint::post("/b/storage/api/buckets/{name}/objects").summary("Upload file").auth(AuthLevel::Authenticated),
                // No output_schema: the success response is the raw object
                // body (`Content-Type` set from the stored object's MIME
                // type), not JSON — see `handle_get_object`'s
                // `ResponseBuilder::new().body(data, &info.content_type)`.
                // `path_params_schema` alone is enough to surface this
                // endpoint's request shape in `/openapi.json` without
                // mislabeling the response as `application/json`.
                BlockEndpoint::get("/b/storage/api/buckets/{name}/objects/{key}")
                    .summary("Download file")
                    .description("Returns the raw object bytes with the stored Content-Type — not a JSON envelope.")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(serde_json::json!({
                        "type": "object",
                        "required": ["name", "key"],
                        "properties": {
                            "name": {"type": "string", "description": "Bucket name"},
                            "key": {"type": "string", "description": "Object key (may contain '/')"}
                        }
                    }))
                    .tags(&["storage"]),
                BlockEndpoint::delete("/b/storage/api/buckets/{name}/objects/{key}").summary("Delete file").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/storage/direct/{token}").summary("Access shared file"),
                BlockEndpoint::get("/b/cloudstorage/").summary("Shares + quota page").auth(AuthLevel::Authenticated),
                // Admin SSR pages — declared `Admin` so the central router
                // enforces the tier (the block dropped its inline `is_admin`
                // check for `/b/storage/admin/*`).
                //
                // The overview is served by `handle()` for BOTH the canonical
                // slash form (`/b/storage/admin/`, the `admin_url`) and the
                // bare no-slash form (`/b/storage/admin`) via its
                // `"" | "/" => overview` dispatch arm. The central router's
                // matcher is trailing-slash-significant, so BOTH must be
                // declared `Admin` — declaring only the slash form would leave
                // the no-slash form governed solely by the Public `/b/storage/`
                // prefix tier, letting an anonymous request reach the storage
                // admin overview.
                BlockEndpoint::get("/b/storage/admin").summary("Storage admin overview").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/storage/admin/").summary("Storage admin overview").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/storage/admin/buckets").summary("All buckets (admin)").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/storage/admin/shares").summary("All shares (admin)").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/storage/admin/quotas").summary("Quotas (admin)").auth(AuthLevel::Admin),
            ])
            .admin_url("/b/storage/admin/")
            .can_disable(true)
    },
    handle: |this, ctx, msg, input| {
        let path = msg.path().to_string();

        // Admin-block delegation: when the Admin block routes a request for
        // `/admin/storage/...` or `/admin/b/cloudstorage/...` through
        // `ctx.call_block("impresspress/files", ...)`, the messages already
        // carry the normalized admin path. Authorization (admin role check)
        // is enforced by the Admin block before delegation; we accept the
        // calls here without re-checking so the admin path stays a thin
        // pass-through to the sub-module handlers that own the SQL.
        if path.starts_with("/admin/storage") {
            return storage::handle_admin(ctx, msg, input).await;
        }
        if path.starts_with("/admin/b/cloudstorage") {
            return cloud::handle(ctx, msg, input).await;
        }

        // Admin SSR pages at /b/storage/admin/... — Admin tier enforced
        // centrally from the declared `/b/storage/admin/*` endpoints (no
        // inline `is_admin` re-check).
        if path.starts_with("/b/storage/admin") && msg.action() == "retrieve" {
            let sub = path.strip_prefix("/b/storage/admin").unwrap_or("/");
            return match sub {
                "" | "/" => pages_admin::overview(ctx, &msg).await,
                "/buckets" => pages_admin::buckets(ctx, &msg).await,
                "/shares" => pages_admin::shares(ctx, &msg).await,
                "/quotas" => pages_admin::quotas(ctx, &msg).await,
                _ => err_not_found("not found"),
            };
        }

        // Direct share access (public, no auth required) — still rate-limited
        // per remote IP inside the handler to stop token enumeration / DOS.
        // Matches the REAL on-the-wire path (no `req.resource` rewrite).
        if path.starts_with("/b/storage/direct/") {
            return share::handle_direct_access(ctx, &msg, &this.limiter).await;
        }

        // Require authentication for all non-public endpoints
        let user_id = msg.user_id().to_string();
        if user_id.is_empty() {
            return crate::http::err_unauthorized("Authentication required");
        }

        // Per-user rate limiting. `create` (upload) gets its own bucket;
        // `retrieve`/everything-else fall back to the read/write split. The
        // Allowed(headers) outcome is discarded here: attaching X-RateLimit-*
        // to a streaming response would need platform-side middleware to
        // inject the headers after the handler returns its OutputStream.
        if let RateLimitOutcome::Limited(r) = check_user_rate_limit_with(
            &this.limiter,
            ctx,
            &msg,
            Some((RateLimit::UPLOAD, "upload")),
        )
        .await
        {
            return r;
        }

        // User-facing SSR pages.
        if msg.action() == "retrieve" {
            if path == "/b/storage" || path == "/b/storage/" {
                return pages_user::bucket_list_page(ctx, &msg).await;
            }
            // /b/storage/{bucket}/[{prefix...}/]  (must end with `/`).
            // Skip admin/ (handled above before normalize), api/, direct/.
            if let Some(rest) = path.strip_prefix("/b/storage/") {
                if rest.ends_with('/')
                    && !rest.starts_with("admin/")
                    && !rest.starts_with("api/")
                    && !rest.starts_with("direct/")
                {
                    let trimmed = rest.trim_end_matches('/');
                    let mut parts = trimmed.splitn(2, '/');
                    let bucket = parts.next().unwrap_or_default();
                    let prefix = parts.next().unwrap_or_default();
                    let prefix_with_slash = if prefix.is_empty() {
                        String::new()
                    } else {
                        format!("{prefix}/")
                    };
                    if !bucket.is_empty() {
                        return pages_user::object_list_page(ctx, &msg, bucket, &prefix_with_slash)
                            .await;
                    }
                }
            }
        }

        // Cloud storage SSR page — must check before JSON dispatch below.
        if msg.action() == "retrieve" && (path == "/b/cloudstorage" || path == "/b/cloudstorage/") {
            return pages_user::cloudstorage_page(ctx, &msg).await;
        }

        // Cloud storage routes (/b/cloudstorage/...) — `cloud::handle` matches
        // the real on-the-wire path directly.
        if path.starts_with("/b/cloudstorage") {
            return cloud::handle(ctx, msg, input).await;
        }

        // User storage API routes (/b/storage/api/...) — `storage::handle`
        // matches the real on-the-wire suffixes directly (the previous
        // double `req.resource` rewrite is gone).
        if path.starts_with("/b/storage/api/") || path == "/b/storage/api" {
            return storage::handle(ctx, msg, input).await;
        }

        err_not_found("not found")
    },
    lifecycle: |_this, ctx, event| {
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/files",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await
    },
}

#[cfg(test)]
mod schema_tests {
    use super::{migrations::SQLITE_MIGRATIONS, models::QuotaConfig};

    /// The quota column defaults in the migration SQL (now the single schema
    /// source) must match the `QuotaConfig` consts. If you change a const,
    /// change `migrations/001_initial_schema.*.sql` too (and remember
    /// `IMPRESSPRESS_RUN_MIGRATIONS=1`).
    #[test]
    fn quota_sql_defaults_match_quota_config_consts() {
        let sql = SQLITE_MIGRATIONS
            .iter()
            .map(|(_, s)| *s)
            .collect::<Vec<_>>()
            .join("\n");

        let asserts: &[(&str, i64)] = &[
            ("max_storage_bytes", QuotaConfig::DEFAULT_MAX_STORAGE_BYTES),
            (
                "max_file_size_bytes",
                QuotaConfig::DEFAULT_MAX_FILE_SIZE_BYTES,
            ),
            (
                "max_files_per_bucket",
                QuotaConfig::DEFAULT_MAX_FILES_PER_BUCKET,
            ),
            ("reset_period_days", QuotaConfig::DEFAULT_RESET_PERIOD_DAYS),
        ];

        for (column, expected) in asserts {
            // Match the `<column> ... DEFAULT <value>` line in the DDL.
            let line = sql
                .lines()
                .find(|l| l.trim_start().starts_with(column))
                .unwrap_or_else(|| panic!("column {column} declared in migration SQL"));
            let needle = format!("DEFAULT {expected}");
            assert!(
                line.contains(&needle),
                "column {column}: migration SQL `{line}` must carry `{needle}` to match \
                 QuotaConfig::{}",
                column.to_uppercase(),
            );
        }
    }
}

#[cfg(test)]
mod grant_tests {
    use wafer_run::{Block, ResourceType};

    use super::FilesBlock;

    #[test]
    fn files_block_does_not_declare_typed_storage() {
        // Wave 26 (c18): files block doesn't need a typed Storage grant
        // for its own namespace because Rule 3 self-admit covers it. A
        // grant here would also be redundant for *cross-block* access:
        // any block that wants to expose its storage to files declares
        // the grant from its own side.
        let files = FilesBlock::new();
        let grants = files.info().grants;

        let typed_storage = grants
            .iter()
            .find(|g| g.resource_type == Some(ResourceType::Storage));

        assert!(
            typed_storage.is_none(),
            "files block must not declare a typed Storage grant — own-namespace \
             access is covered by WRAP Rule 3 self-admit (Wave 26 / c18). \
             Cross-block grants belong on the owning block's side. (got: {typed_storage:?})",
        );
    }
}
