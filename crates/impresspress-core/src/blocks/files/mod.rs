//! File storage: buckets, objects, shares, quotas (`impresspress/files`).
//!
//! [`ROUTES`] is the block's one description of its HTTP surface: `handle()`
//! dispatches on it and `info().endpoints` is generated from it. Handlers
//! read path variables only as the matcher bound them (`msg.var(..)`).

pub mod assets;
mod cloud;
mod contracts;
pub(crate) mod migrations;
pub(crate) mod models;
mod pages_admin;
pub(crate) mod pages_user;
mod quota;
pub(crate) mod repo;
mod serving;
mod share;
pub(crate) mod storage;

use wafer_run::{BlockInfo, ConfigVar, HttpMethod, InputType, InstanceMode};

/// The config vars this block declares, for `BlockInfo::config_keys` — the
/// admin Variables screen renders them from this list, and the validator
/// reads the same declaration.
fn config_vars() -> Vec<ConfigVar> {
    vec![ConfigVar::new(
        cloud::MAX_SHARE_EXPIRY_HOURS_KEY,
        "Longest a public share link may live, in hours. A share created \
         without an expiry gets this long; a longer one is refused.",
        &cloud::DEFAULT_MAX_SHARE_EXPIRY_HOURS.to_string(),
    )
    .name("Max share link lifetime (hours)")
    .input_type(InputType::Number)]
}

use super::rate_limit::{check_user_rate_limit_with, RateLimit, RateLimitOutcome, UserRateLimiter};
use crate::{
    endpoint_match::{self, response_schema_of, EndpointRoute},
    http::{err_not_found, err_unauthenticated},
};

/// Handler for one row of [`ROUTES`]. `AdminOverview` serves both the
/// `/b/storage/admin` and `/b/storage/admin/` rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    // Admin SSR pages
    AdminOverview,
    AdminBucketsPage,
    AdminSharesPage,
    AdminQuotasPage,
    // Admin JSON API
    AdminListBuckets,
    AdminStats,
    AdminListShares,
    AdminAccessLogs,
    AdminListQuotas,
    AdminUpdateQuota,
    // Public share link
    DirectAccess,
    // User SSR pages
    BucketListPage,
    ObjectListPage,
    FolderListPage,
    CloudStoragePage,
    // User storage JSON API
    ListBuckets,
    CreateBucket,
    DeleteBucket,
    ListObjects,
    UploadObject,
    GetObject,
    DeleteObject,
    Search,
    Recent,
    // User cloud-storage JSON API
    ListShares,
    CreateShare,
    DeleteShare,
    GetQuota,
}

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. Wire paths; `{name}`, `{key...}`,
/// `{id}`, `{token}`, `{bucket}` and `{prefix...}` are bound into
/// `req.param.*` for the handlers' `msg.var` readers.
///
/// Order is dispatch order. The admin rows and the share link come first
/// because `/b/storage/{bucket}/` and `/b/storage/{bucket}/{prefix...}/` at
/// the end would otherwise claim `/b/storage/admin/` and every other
/// slash-terminated path; the router's `endpoint_auth` is order-independent
/// and takes the strictest matching row, so a generic row can never lower the
/// level a specific one declares. The two `{key...}` object rows precede the
/// bare `.../objects` and `.../{name}` rows, as the old sub-table had them.
///
/// Every row names the level the central router enforces. The `/b/storage/`
/// and `/b/cloudstorage/` prefix routes are `Public`, so a row's level is the
/// whole gate: the admin rows moved here from the admin block's delegation
/// (which sat behind the `Admin` `/b/admin/` prefix) are `admin`, and the
/// user rows the block served but never declared are `authenticated`, the
/// level the handler already required through the session preamble
/// ([`user_preamble`]) and its owner checks, and the level the router's
/// fail-closed default already applied to an undeclared path.
const ROUTES: &[EndpointRoute<Route>] = &[
    // ── Admin SSR pages ── declared `Admin` so the central router enforces
    // the tier; the block has no inline `is_admin` check for them.
    //
    // The overview is declared for BOTH the canonical slash form
    // (`/b/storage/admin/`, the `admin_url`) and the bare form
    // (`/b/storage/admin`). The matcher's trailing-slash retry would serve
    // the bare form from the slash row on its own, but the declared surface
    // keeps both lines so the router's gate for the bare form is stated, not
    // inferred.
    EndpointRoute::admin(HttpMethod::Get, "/b/storage/admin", Route::AdminOverview)
        .summary("Storage admin overview"),
    EndpointRoute::admin(HttpMethod::Get, "/b/storage/admin/", Route::AdminOverview)
        .summary("Storage admin overview"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/storage/admin/buckets",
        Route::AdminBucketsPage,
    )
    .summary("All buckets (admin)"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/storage/admin/shares",
        Route::AdminSharesPage,
    )
    .summary("All shares (admin)"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/storage/admin/quotas",
        Route::AdminQuotasPage,
    )
    .summary("Quotas (admin)"),
    // ── Admin JSON API ── reached until this PR through the admin block,
    // which rewrote `/b/admin/api/storage/...` and
    // `/b/admin/api/cloudstorage/...` to synthetic paths and forwarded them
    // via `call_block`. They live under the files prefixes now, gated `Admin`
    // by the router from these rows. Never rate-limited (the delegation
    // returned before the per-user preamble), and still not.
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/storage/admin/api/buckets",
        Route::AdminListBuckets,
    )
    .summary("List every bucket (admin)")
    .output(response_schema_of::<contracts::BucketListResponse>)
    .tags(&["storage"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/storage/admin/api/stats",
        Route::AdminStats,
    )
    .summary("Storage totals (admin)")
    .output(response_schema_of::<contracts::StorageStatsResponse>)
    .tags(&["storage"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/cloudstorage/admin/shares",
        Route::AdminListShares,
    )
    .summary("Recent shares, all users (admin)")
    .output(response_schema_of::<contracts::RecordListView<repo::shares::ShareRow>>)
    .tags(&["cloudstorage"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/cloudstorage/admin/access-logs",
        Route::AdminAccessLogs,
    )
    .summary("Share access logs (admin)")
    .output(response_schema_of::<contracts::RecordListView<repo::shares::AccessLogRow>>)
    .tags(&["cloudstorage"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/cloudstorage/admin/quotas",
        Route::AdminListQuotas,
    )
    .summary("Per-user quotas (admin)")
    .output(response_schema_of::<contracts::RecordListView<repo::quota::QuotaRow>>)
    .tags(&["cloudstorage"]),
    // PATCH is what clients send; `update` is the action both PUT and PATCH
    // map to, which is what the old delegated arm matched.
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/cloudstorage/admin/quotas/{id}",
        Route::AdminUpdateQuota,
    )
    .summary("Set a user's quota (admin)")
    .path_params(quota_user_id_path_schema)
    .output(response_schema_of::<contracts::RecordView<repo::quota::QuotaRow>>)
    .tags(&["cloudstorage"]),
    // ── Public share link ── `share::handle_direct_access` rate-limits per
    // remote IP, resolves the token to its share row, and enforces that
    // row's expiry and access cap itself.
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/storage/direct/{token}",
        Route::DirectAccess,
    )
    .summary("Access shared file"),
    // ── User storage JSON API ──
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/storage/api/buckets",
        Route::ListBuckets,
    )
    .summary("List buckets")
    .output(response_schema_of::<contracts::BucketListResponse>)
    .tags(&["storage"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/storage/api/buckets",
        Route::CreateBucket,
    )
    .summary("Create bucket")
    .output(response_schema_of::<contracts::BucketCreatedResponse>)
    .tags(&["storage"]),
    // Never declared before this PR; `storage/search.rs` scopes both by
    // `msg.user_id()`.
    EndpointRoute::authenticated(HttpMethod::Get, "/b/storage/api/search", Route::Search)
        .summary("Search objects")
        .output(response_schema_of::<contracts::RecordListView<repo::objects::ObjectRow>>)
        .tags(&["storage"]),
    // The row type here is `views::ViewRow`, NOT `objects::ObjectRow`:
    // `storage::handle_recent` reads `repo::views::list_recent_for_user`, so
    // what goes on the wire is the object-view audit row (`user_id`,
    // `viewed_at`), not object metadata (`size`, `content_type`, `status`).
    // Both in-repo consumers — the summary below and the SDK's
    // `getRecentFiles` — read as though it were metadata; which side is
    // wrong is a product question, and the schema describes what the
    // handler emits today rather than pre-judging it.
    EndpointRoute::authenticated(HttpMethod::Get, "/b/storage/api/recent", Route::Recent)
        .summary("Recently viewed objects")
        .description(
            "Object-view audit rows, newest first — one row per tracked download, naming the \
             object viewed and when. Not object metadata.",
        )
        .output(response_schema_of::<contracts::RecordListView<repo::views::ViewRow>>)
        .tags(&["storage"]),
    // The object rows bind `{key...}`: keys contain `/`, and dispatch has
    // always matched the rest of the path. The declaration used to say
    // `{key}`, a template no nested key could match.
    //
    // These are the two highest-value developer endpoints for browsing a
    // bucket; full schema coverage of the remaining storage routes (buckets,
    // shares, quotas) is a follow-up.
    //
    // No output schema on the download: the success response is the raw
    // object body, not JSON. `handle_get_object` streams it with
    // `streaming::stream_download`, under the headers
    // `serving::user_object_leading_meta` builds — the stored MIME type once
    // `normalized_content_type` has vetted it, `X-Content-Type-Options:
    // nosniff`, and a `Content-Disposition` that is `inline` only for the
    // types that cannot carry script. The path schema alone surfaces the
    // request shape in `/openapi.json` without mislabeling the response as
    // `application/json`.
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/storage/api/buckets/{name}/objects/{key...}",
        Route::GetObject,
    )
    .summary("Download file")
    .description("Returns the raw object bytes with the stored Content-Type — not a JSON envelope.")
    .path_params(object_path_schema)
    .tags(&["storage"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/storage/api/buckets/{name}/objects/{key...}",
        Route::DeleteObject,
    )
    .summary("Delete file")
    .path_params(object_path_schema)
    .output(response_schema_of::<contracts::DeletedResponse>)
    .tags(&["storage"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/storage/api/buckets/{name}/objects",
        Route::ListObjects,
    )
    .summary("List objects")
    .path_params(list_objects_path_schema)
    .query_params(list_objects_query_schema)
    .output(response_schema_of::<contracts::ObjectListResponse>)
    .tags(&["storage"]),
    // No input schema: `handle_upload_object` accepts either a raw body
    // (programmatic clients) or a `multipart/form-data` envelope (browser
    // `FormData` uploads) — see `crate::multipart` and the `is_multipart`
    // branch in `storage/objects.rs`. Neither shape is a JSON body, so there
    // is no `T` to derive a request schema from; a JSON Schema here would
    // misdescribe what the endpoint accepts.
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/storage/api/buckets/{name}/objects",
        Route::UploadObject,
    )
    .summary("Upload file")
    .path_params(list_objects_path_schema)
    .output(response_schema_of::<contracts::ObjectUploadedResponse>)
    .tags(&["storage"]),
    // Never declared before this PR; `storage/buckets.rs` refuses a bucket
    // the caller does not own (`require_bucket_access`).
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/storage/api/buckets/{name}",
        Route::DeleteBucket,
    )
    .summary("Delete bucket")
    .path_params(list_objects_path_schema)
    .output(response_schema_of::<contracts::DeletedResponse>)
    .tags(&["storage"]),
    // ── User cloud-storage JSON API ── never declared before this PR;
    // `cloud.rs` lists, creates and reads quota for `msg.user_id()` and
    // refuses to delete another user's share.
    EndpointRoute::authenticated(HttpMethod::Get, "/b/cloudstorage/shares", Route::ListShares)
        .summary("List my share links")
        .output(response_schema_of::<contracts::RecordListView<repo::shares::ShareRow>>)
        .tags(&["cloudstorage"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/cloudstorage/shares",
        Route::CreateShare,
    )
    .summary("Create a share link")
    .output(response_schema_of::<contracts::ShareCreatedResponse>)
    .tags(&["cloudstorage"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/cloudstorage/shares/{id}",
        Route::DeleteShare,
    )
    .summary("Delete a share link")
    .path_params(share_id_path_schema)
    .output(response_schema_of::<contracts::DeletedResponse>)
    .tags(&["cloudstorage"]),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/cloudstorage/quota", Route::GetQuota)
        .summary("My quota and usage")
        .output(response_schema_of::<contracts::QuotaResponse>)
        .tags(&["cloudstorage"]),
    // ── User SSR pages ── the two generic bucket rows last (see above).
    EndpointRoute::authenticated(HttpMethod::Get, "/b/storage/", Route::BucketListPage)
        .summary("Bucket list (user)"),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/cloudstorage/", Route::CloudStoragePage)
        .summary("Shares + quota page"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/storage/{bucket}/",
        Route::ObjectListPage,
    )
    .summary("Object list"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/storage/{bucket}/{prefix...}/",
        Route::FolderListPage,
    )
    .summary("Object list (nested)"),
];

/// Whether a matched route runs the per-user preamble in `handle`: a session
/// is required (belt and braces under the router's `Authenticated` gate) and
/// the per-user rate limit is spent. `false` for the public share link, which
/// limits itself per remote IP inside `share::handle_direct_access`, and for
/// the admin rows, which the router gates `Admin` from the declaration and
/// which were never rate-limited. Exhaustive so a new row is a decision, not
/// an omission; `table_tests` pins it to the declared level.
const fn user_preamble(route: Route) -> bool {
    match route {
        Route::AdminOverview
        | Route::AdminBucketsPage
        | Route::AdminSharesPage
        | Route::AdminQuotasPage
        | Route::AdminListBuckets
        | Route::AdminStats
        | Route::AdminListShares
        | Route::AdminAccessLogs
        | Route::AdminListQuotas
        | Route::AdminUpdateQuota
        | Route::DirectAccess => false,
        Route::BucketListPage
        | Route::ObjectListPage
        | Route::FolderListPage
        | Route::CloudStoragePage
        | Route::ListBuckets
        | Route::CreateBucket
        | Route::DeleteBucket
        | Route::ListObjects
        | Route::UploadObject
        | Route::GetObject
        | Route::DeleteObject
        | Route::Search
        | Route::Recent
        | Route::ListShares
        | Route::CreateShare
        | Route::DeleteShare
        | Route::GetQuota => true,
    }
}

/// Path-parameter schema for `GET /b/storage/api/buckets/{name}/objects`.
///
/// Hand-written (the same call shape as `products::mod::info`'s
/// `id_path_schema`): `storage::params::extract_bucket_name` reads
/// `msg.var("name")` by name and `handle_list_objects` reads `msg.query(..)`
/// / `msg.pagination_params(..)` by name, so nothing here deserializes into a
/// struct. A struct declared only to feed `request_schema_of::<T>` would have
/// no runtime user and would generate a byte-identical parameter list.
fn list_objects_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": {"type": "string", "description": "Bucket name"}
        }
    })
}

/// Query-parameter schema for `GET /b/storage/api/buckets/{name}/objects`;
/// hand-written for the reason given on [`list_objects_path_schema`].
fn list_objects_query_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "prefix": {"type": "string", "description": "Key prefix filter"},
            "page": {"type": "integer", "default": 1},
            "page_size": {"type": "integer", "default": 50, "maximum": 100}
        }
    })
}

/// Path-parameter schema for the `{name}/objects/{key...}` rows. The
/// parameter is `key`; the template's `...` marks that it binds the rest of
/// the path, since keys contain `/`.
fn object_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "key"],
        "properties": {
            "name": {"type": "string", "description": "Bucket name"},
            "key": {"type": "string", "description": "Object key (may contain '/')"}
        }
    })
}

/// Path-parameter schema for `DELETE /b/cloudstorage/shares/{id}`.
fn share_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {"type": "string", "description": "Share row id, as `POST /b/cloudstorage/shares` returned it"}
        }
    })
}

/// Path-parameter schema for `PATCH /b/cloudstorage/admin/quotas/{id}`. The
/// segment is the *user* whose quota is being set, not the quota row's id —
/// `handle_update_quota` upserts by user.
fn quota_user_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {"type": "string", "description": "User id whose quota is being set"}
        }
    })
}

crate::impresspress_feature_block! {
    /// File storage: buckets, objects, shares, quotas (`impresspress/files`).
    pub struct FilesBlock;
    fields: { limiter: UserRateLimiter },
    name: "impresspress/files",
    info: |_this| {
        use wafer_run::CollectionSchema;

        BlockInfo::new("impresspress/files", "0.0.1", "http-handler@v1", "File storage, sharing, quotas, and access logging")
            .instance_mode(InstanceMode::Singleton)
            // `wafer-run/crypto`: a share link's token is 256 bits of CSPRNG
            // output, drawn by `share::generate_share_token` through
            // `crypto::random_bytes`. Without the entry,
            // `POST /b/cloudstorage/shares` is refused at the `call_block`
            // boundary — above every grant check — and sharing is dead.
            .requires(vec!["wafer-run/database".into(), "wafer-run/storage".into(), "wafer-run/config".into(), "wafer-run/crypto".into()])
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
            .config_keys(config_vars())
            .category(wafer_run::BlockCategory::Feature)
            .description("File storage and management with bucket-based organization. Supports file upload, download, deletion, search, and sharing via public links with expiration and access counting. Includes per-user storage quotas.")
            .endpoints(endpoint_match::declare(ROUTES))
            .admin_url("/b/storage/admin/")
            .can_disable(true)
    },
    handle: |this, ctx, mut msg, input| {
        // Auth is enforced centrally by `route_to_block` from each row's
        // declared `AuthLevel`; the matcher binds the path variables the
        // handlers read through `msg.var(..)`.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };

        if user_preamble(route) {
            // Belt and braces under the router's `Authenticated` gate.
            if msg.user_id().is_empty() {
                return err_unauthenticated("Authentication required");
            }
            // Per-user rate limiting. `create` (upload) gets its own bucket;
            // `retrieve`/everything-else fall back to the read/write split.
            // The Allowed(headers) outcome is discarded here: attaching
            // X-RateLimit-* to a streaming response would need platform-side
            // middleware to inject the headers after the handler returns its
            // OutputStream.
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
        }

        match route {
            Route::AdminOverview => pages_admin::overview(ctx, &msg).await,
            Route::AdminBucketsPage => pages_admin::buckets(ctx, &msg).await,
            Route::AdminSharesPage => pages_admin::shares(ctx, &msg).await,
            Route::AdminQuotasPage => pages_admin::quotas(ctx, &msg).await,
            Route::AdminListBuckets => storage::handle_list_buckets(ctx, &msg).await,
            Route::AdminStats => storage::handle_stats(ctx, &msg).await,
            Route::AdminListShares => cloud::handle_admin_list_shares(ctx, &msg).await,
            Route::AdminAccessLogs => cloud::handle_access_logs(ctx, &msg).await,
            Route::AdminListQuotas => cloud::handle_admin_quotas(ctx, &msg).await,
            Route::AdminUpdateQuota => cloud::handle_update_quota(ctx, &msg, input).await,
            Route::DirectAccess => share::handle_direct_access(ctx, &msg, &this.limiter).await,
            Route::BucketListPage => pages_user::buckets::bucket_list_page(ctx, &msg).await,
            Route::ObjectListPage => {
                pages_user::objects::object_list_page(ctx, &msg, msg.var("bucket"), "").await
            }
            Route::FolderListPage => {
                // The bound prefix carries no trailing slash; the page's
                // prefix convention is `dir/`.
                let prefix = format!("{}/", msg.var("prefix"));
                pages_user::objects::object_list_page(ctx, &msg, msg.var("bucket"), &prefix).await
            }
            Route::CloudStoragePage => pages_user::cloudstorage::cloudstorage_page(ctx, &msg).await,
            Route::ListBuckets => storage::handle_list_buckets(ctx, &msg).await,
            Route::CreateBucket => storage::handle_create_bucket(ctx, &msg, input).await,
            Route::DeleteBucket => storage::handle_delete_bucket(ctx, &msg).await,
            Route::ListObjects => storage::handle_list_objects(ctx, &msg).await,
            Route::UploadObject => storage::handle_upload_object(ctx, &msg, input).await,
            Route::GetObject => storage::handle_get_object(ctx, &msg).await,
            Route::DeleteObject => storage::handle_delete_object(ctx, &msg).await,
            Route::Search => storage::handle_search(ctx, &msg).await,
            Route::Recent => storage::handle_recent(ctx, &msg).await,
            Route::ListShares => cloud::handle_list_shares(ctx, &msg).await,
            Route::CreateShare => cloud::handle_create_share(ctx, &msg, input).await,
            Route::DeleteShare => cloud::handle_delete_share(ctx, &msg).await,
            Route::GetQuota => cloud::handle_get_quota(ctx, &msg).await,
        }
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
    ///
    /// The checked columns are exactly `QuotaConfig`'s fields, so a cap added
    /// to the struct without a matching column default fails here rather
    /// than going unchecked. `reset_period_days` is in the SQL but not the
    /// struct: see `migrations/mod.rs` for why the column stays.
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
        ];

        let fields: std::collections::BTreeSet<String> =
            match serde_json::to_value(QuotaConfig::default()).expect("serializes") {
                serde_json::Value::Object(map) => map.keys().cloned().collect(),
                other => panic!("QuotaConfig serializes as an object, got {other}"),
            };
        let checked: std::collections::BTreeSet<String> = asserts
            .iter()
            .map(|(column, _)| column.to_string())
            .collect();
        assert_eq!(
            checked, fields,
            "every QuotaConfig field needs its column default checked here, and nothing else"
        );

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

#[cfg(test)]
mod error_mapping_tests;

#[cfg(test)]
mod test_support {
    use std::sync::Arc;

    use wafer_run::Message;

    use crate::test_support::{InMemoryStorageService, TestContext};

    /// Run `msg` through the block's own route table so `{name}`, `{key}`,
    /// `{id}`, `{token}`, `{bucket}` and `{prefix}` are bound the way they
    /// are on the wire, then hand the message to a handler directly. Panics
    /// when no row matches: a test that sends an unroutable path would
    /// otherwise exercise the handler's "missing id" branch by accident.
    pub(super) fn routed(mut msg: Message) -> Message {
        let route = crate::endpoint_match::dispatch(&mut msg, super::ROUTES);
        assert!(
            route.is_some(),
            "no files route matches {} {}",
            msg.action(),
            msg.path()
        );
        msg
    }

    /// A files-block fixture carrying everything a share link touches: the
    /// real `wafer-run/crypto` block (a share token is CSPRNG output drawn
    /// through it), the production storage shim over an object store that
    /// really holds bytes, and one bucket owned by `owner`.
    ///
    /// Both halves of the round trip — `cloud::handle_create_share` and
    /// `share::handle_direct_access` — run on this one fixture, so neither
    /// side can be tested against wiring the other never sees.
    pub(super) async fn share_ctx(bucket: &str, owner: &str) -> TestContext {
        share_ctx_over(bucket, owner, Arc::new(InMemoryStorageService::new())).await
    }

    /// [`share_ctx`] over a caller-supplied object store, so a test can watch
    /// which storage calls a handler makes.
    pub(super) async fn share_ctx_over(
        bucket: &str,
        owner: &str,
        storage: Arc<dyn wafer_core::interfaces::storage::service::StorageService>,
    ) -> TestContext {
        let mut ctx = TestContext::with_files().await;

        let crypto_svc = Arc::new(
            wafer_block_crypto::service::Argon2JwtCryptoService::new(
                // ≥ 32 bytes for the HMAC-SHA256 minimum-length check.
                "test-jwt-secret-padded-to-min-32-bytes-aaaa".to_string(),
            )
            .expect("test secret is long enough"),
        );
        ctx.register_block(
            "wafer-run/crypto",
            Arc::new(wafer_core::service_blocks::crypto::CryptoBlock::new(
                crypto_svc,
            )),
        );
        ctx.register_block("wafer-run/storage", crate::blocks::storage::create(storage));

        let data = crate::util::json_map(serde_json::json!({
            "name": bucket,
            "public": false,
            "created_by": owner,
            "created_at": crate::util::now_rfc3339(),
        }));
        super::repo::buckets::seed(&ctx, data)
            .await
            .expect("seed bucket");

        ctx
    }

    /// Hold `bytes` for `bucket/key` the way a deployment from before
    /// migration 005 holds an object: the blob at the object key itself, and
    /// a `Complete` row owned by `owner` whose `blob_key` is NULL. Every
    /// reader resolves such a row to the object key.
    pub(in crate::blocks::files) async fn seed_legacy_object(
        ctx: &TestContext,
        bucket: &str,
        key: &str,
        bytes: &[u8],
        content_type: &str,
        owner: &str,
    ) {
        wafer_core::clients::storage::put(ctx, bucket, key, bytes, content_type)
            .await
            .expect("store the legacy blob at the object key");
        super::repo::objects::seed(
            ctx,
            crate::util::json_map(serde_json::json!({
                "bucket": bucket,
                "key": key,
                "size": bytes.len(),
                "content_type": content_type,
                "status": super::contracts::ObjectStatus::Complete,
                "uploaded_by": owner,
                "uploaded_at": "2026-01-01T00:00:00Z",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
            })),
        )
        .await
        .expect("seed the legacy object row");
    }

    /// The bytes and content type the object `bucket/key` serves, read the
    /// way every reader reads them: through its row. `Err(NotFound)` when
    /// the key has no row.
    pub(in crate::blocks::files) async fn stored_object(
        ctx: &dyn wafer_run::context::Context,
        bucket: &str,
        key: &str,
    ) -> Result<(Vec<u8>, wafer_core::clients::storage::ObjectInfo), wafer_run::WaferError> {
        let blob_key = super::repo::objects::find_blob_key(ctx, bucket, key)
            .await?
            .ok_or_else(|| {
                wafer_run::WaferError::new(wafer_run::ErrorCode::NotFound, "no object row")
            })?;
        wafer_core::clients::storage::get(ctx, bucket, &blob_key).await
    }

    /// The message the router hands the upload handler for
    /// `POST /b/storage/api/buckets/{bucket}/objects?key={key}` with a raw
    /// `content_type` body, sent by `uploader`.
    pub(super) fn upload_msg(
        bucket: &str,
        key: &str,
        content_type: &str,
        uploader: &str,
    ) -> Message {
        let mut msg = routed(crate::test_support::auth_msg(
            "create",
            &format!("/b/storage/api/buckets/{bucket}/objects"),
            uploader,
        ));
        msg.set_meta("req.query.key", key);
        msg.set_meta("req.content_type", content_type);
        msg
    }

    /// Store `bytes` at `bucket/key` the way a user does: through the real
    /// upload handler, so the object has both its blob and the `complete`
    /// row every other files handler reads.
    pub(super) async fn upload(
        ctx: &TestContext,
        bucket: &str,
        key: &str,
        bytes: &[u8],
        content_type: &str,
        uploader: &str,
    ) {
        let out = super::storage::handle_upload_object(
            ctx,
            &upload_msg(bucket, key, content_type, uploader),
            wafer_run::InputStream::from_bytes(bytes.to_vec()),
        )
        .await;
        let resp = crate::test_support::output_json(out).await;
        assert_eq!(
            resp["uploaded"],
            serde_json::json!(true),
            "seeding {bucket}/{key} through the upload handler failed: {resp}"
        );
    }

    // -----------------------------------------------------------------
    // The browser half of the contract
    //
    // These read `files-browser.js` itself, so a test drives the handler
    // with the field names, attributes and URLs the shipped bundle really
    // uses. A test that re-types them in Rust certifies the Rust side
    // against itself and passes while the browser talks to nothing.
    // -----------------------------------------------------------------

    /// The slice of `js` between `start` and the next `end` after it.
    fn between<'a>(js: &'a str, start: &str, end: &str, what: &str) -> &'a str {
        let from = js
            .find(start)
            .unwrap_or_else(|| panic!("{what}: no `{start}`"))
            + start.len();
        let len = js[from..]
            .find(end)
            .unwrap_or_else(|| panic!("{what}: no `{end}` after `{start}`"));
        &js[from..from + len]
    }

    /// One entry of the share modal's expiry dropdown.
    pub(super) struct ShareModalOption {
        /// The number the modal sends for this option.
        pub value: i64,
        /// The duration this option's LABEL promises the user, in hours — so
        /// a test can catch an option offering the right field in the wrong
        /// unit as well as one that outlives the cap.
        pub label_hours: i64,
        /// Whether the modal pre-selects it.
        pub selected: bool,
    }

    /// What the share modal sends when the user accepts its default expiry.
    pub(super) struct ShareModalExpiry {
        /// The JSON field the modal puts the expiry in.
        pub field: String,
        /// The number it sends for the pre-selected option.
        pub value: i64,
        /// That option's promised duration in hours.
        pub label_hours: i64,
    }

    /// The name of the variable the modal reads its expiry select into.
    fn expiry_select_var(js: &str) -> &str {
        let read = " = dlg.querySelector('select[name=\"expires\"]').value;";
        let end = js
            .find(read)
            .expect("the share modal must read its expiry select");
        let var_at = js[..end].rfind("const ").expect("read into a const") + "const ".len();
        &js[var_at..end]
    }

    /// Every option the share modal's expiry dropdown offers, in order.
    pub(super) fn share_modal_expiry_options() -> Vec<ShareModalOption> {
        let js = super::assets::SOURCE;
        let select = between(
            js,
            "<select name=\"expires\">",
            "</select>",
            "the share modal's expiry select",
        );

        let mut options = Vec::new();
        let mut rest = select;
        while let Some(at) = rest.find("<option value=\"") {
            rest = &rest[at + "<option value=\"".len()..];
            let value_end = rest.find('"').expect("an option value ends");
            let raw_value = &rest[..value_end];
            let value: i64 = raw_value.parse().unwrap_or_else(|_| {
                panic!(
                    "expiry option value `{raw_value}` is not a number of hours — \
                        a share link always has an end"
                )
            });
            let selected = between(rest, "\"", ">", "an option's attributes").contains("selected");
            let label = between(rest, ">", "</option>", "an option label");
            options.push(ShareModalOption {
                value,
                label_hours: label_in_hours(label),
                selected,
            });
        }
        assert!(
            !options.is_empty(),
            "the share modal must offer an expiry to pick"
        );
        options
    }

    /// `"7 days"` ⇒ 168. The label is what the user was promised.
    fn label_in_hours(label: &str) -> i64 {
        let (count, unit) = label
            .split_once(' ')
            .unwrap_or_else(|| panic!("expiry label `{label}` is not `<n> <unit>`"));
        let count: i64 = count
            .parse()
            .unwrap_or_else(|_| panic!("expiry label `{label}` does not start with a number"));
        match unit.trim_end_matches('s') {
            "day" => count * 24,
            "hour" => count,
            other => panic!("expiry label `{label}` uses an unhandled unit `{other}`"),
        }
    }

    /// Read [`ShareModalExpiry`] out of the shipped bundle.
    pub(super) fn share_modal_expiry() -> ShareModalExpiry {
        let js = super::assets::SOURCE;
        let var = expiry_select_var(js);

        // The field the modal sends it under — `<field>: Number(<var>)` or
        // `body.<field> = Number(<var>)`, whichever spelling the bundle uses.
        let call = format!("Number({var})");
        let at = js
            .find(&call)
            .expect("the share modal must send the expiry it read");
        let head = js[..at].trim_end().trim_end_matches([':', '=']).trim_end();
        let field_at = head
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |i| i + 1);
        let field = head[field_at..].to_string();
        assert!(
            !field.is_empty(),
            "the expiry must be sent under a named field"
        );

        let selected = share_modal_expiry_options()
            .into_iter()
            .find(|option| option.selected)
            .expect("the share modal must preselect an expiry");

        ShareModalExpiry {
            field,
            value: selected.value,
            label_hours: selected.label_hours,
        }
    }

    /// The HTML attribute the kebab's revoke button reads, derived from the
    /// `dataset` key the bundle uses (`dataset.shareId` ⇒ `data-share-id`).
    pub(super) fn revoke_id_attribute() -> String {
        let key = between(
            super::assets::SOURCE,
            "revokeShare(trigger.dataset.",
            ")",
            "revoke button wiring",
        );
        let mut attr = String::from("data-");
        for ch in key.chars() {
            if ch.is_ascii_uppercase() {
                attr.push('-');
                attr.push(ch.to_ascii_lowercase());
            } else {
                attr.push(ch);
            }
        }
        attr
    }

    /// The URL the revoke button DELETEs for the value it read out of that
    /// attribute.
    pub(super) fn revoke_url(id: &str) -> String {
        let js = super::assets::SOURCE;
        let handler = &js[js
            .find("async function revokeShare(")
            .expect("the bundle must define revokeShare")..];
        format!(
            "{}{id}",
            between(handler, "fetch('", "'", "revoke fetch URL")
        )
    }
}

#[cfg(test)]
mod table_tests {
    use wafer_run::{AuthLevel, Block as _, Message};

    use super::*;
    use crate::{endpoint_match::endpoint_auth, test_support::anon_msg};

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = FilesBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }

    fn resolve(action: &str, path: &str) -> (Option<Route>, Message) {
        let mut msg = anon_msg(action, path);
        let route = endpoint_match::dispatch(&mut msg, ROUTES);
        (route, msg)
    }

    /// `(action, path, expected route, bound variables)`.
    type Case<'a> = (&'a str, &'a str, Route, &'a [(&'a str, &'a str)]);

    fn assert_resolves(cases: &[Case<'_>]) {
        for (action, path, expected, vars) in cases {
            let (route, msg) = resolve(action, path);
            assert_eq!(route, Some(*expected), "{action} {path}");
            for (name, value) in *vars {
                assert_eq!(msg.var(name), *value, "{action} {path} binds {name}");
            }
        }
    }

    /// The thirteen rows this PR declares for paths the block already served
    /// (four cloud-storage and three storage-API rows the block never
    /// declared, six admin rows moved from the admin block's delegation),
    /// each resolving to its handler with its path variable bound.
    #[test]
    fn every_new_row_dispatches_to_its_handler() {
        assert_resolves(&[
            ("retrieve", "/b/cloudstorage/shares", Route::ListShares, &[]),
            ("create", "/b/cloudstorage/shares", Route::CreateShare, &[]),
            (
                "delete",
                "/b/cloudstorage/shares/s-1",
                Route::DeleteShare,
                &[("id", "s-1")],
            ),
            ("retrieve", "/b/cloudstorage/quota", Route::GetQuota, &[]),
            ("retrieve", "/b/storage/api/search", Route::Search, &[]),
            ("retrieve", "/b/storage/api/recent", Route::Recent, &[]),
            (
                "delete",
                "/b/storage/api/buckets/photos",
                Route::DeleteBucket,
                &[("name", "photos")],
            ),
            (
                "retrieve",
                "/b/cloudstorage/admin/shares",
                Route::AdminListShares,
                &[],
            ),
            (
                "retrieve",
                "/b/cloudstorage/admin/access-logs",
                Route::AdminAccessLogs,
                &[],
            ),
            (
                "retrieve",
                "/b/cloudstorage/admin/quotas",
                Route::AdminListQuotas,
                &[],
            ),
            (
                "update",
                "/b/cloudstorage/admin/quotas/u-1",
                Route::AdminUpdateQuota,
                &[("id", "u-1")],
            ),
            (
                "retrieve",
                "/b/storage/admin/api/buckets",
                Route::AdminListBuckets,
                &[],
            ),
            (
                "retrieve",
                "/b/storage/admin/api/stats",
                Route::AdminStats,
                &[],
            ),
        ]);
    }

    /// Every path the old `handle` chain, the `/b/storage/api` sub-table and
    /// `cloud::handle` served resolves to a row, with the variables the
    /// handlers read bound. Nested keys and nested folder prefixes keep their
    /// slashes; the bare index paths reach their rows through the matcher's
    /// trailing-slash retry.
    #[test]
    fn every_path_the_block_served_resolves_to_a_row() {
        assert_resolves(&[
            ("retrieve", "/b/storage/admin", Route::AdminOverview, &[]),
            ("retrieve", "/b/storage/admin/", Route::AdminOverview, &[]),
            (
                "retrieve",
                "/b/storage/admin/buckets",
                Route::AdminBucketsPage,
                &[],
            ),
            (
                "retrieve",
                "/b/storage/admin/shares",
                Route::AdminSharesPage,
                &[],
            ),
            (
                "retrieve",
                "/b/storage/admin/quotas",
                Route::AdminQuotasPage,
                &[],
            ),
            (
                "retrieve",
                "/b/storage/direct/tok-1",
                Route::DirectAccess,
                &[("token", "tok-1")],
            ),
            ("retrieve", "/b/storage", Route::BucketListPage, &[]),
            ("retrieve", "/b/storage/", Route::BucketListPage, &[]),
            (
                "retrieve",
                "/b/storage/photos/",
                Route::ObjectListPage,
                &[("bucket", "photos")],
            ),
            (
                "retrieve",
                "/b/storage/photos/nested/",
                Route::FolderListPage,
                &[("bucket", "photos"), ("prefix", "nested")],
            ),
            (
                "retrieve",
                "/b/storage/photos/nested/deep/",
                Route::FolderListPage,
                &[("bucket", "photos"), ("prefix", "nested/deep")],
            ),
            (
                "retrieve",
                "/b/storage/photos/my%20files/",
                Route::FolderListPage,
                &[("bucket", "photos"), ("prefix", "my files")],
            ),
            ("retrieve", "/b/cloudstorage", Route::CloudStoragePage, &[]),
            ("retrieve", "/b/cloudstorage/", Route::CloudStoragePage, &[]),
            (
                "retrieve",
                "/b/storage/api/buckets",
                Route::ListBuckets,
                &[],
            ),
            ("create", "/b/storage/api/buckets", Route::CreateBucket, &[]),
            (
                "retrieve",
                "/b/storage/api/buckets/photos/objects",
                Route::ListObjects,
                &[("name", "photos")],
            ),
            (
                "create",
                "/b/storage/api/buckets/photos/objects",
                Route::UploadObject,
                &[("name", "photos")],
            ),
            (
                "retrieve",
                "/b/storage/api/buckets/photos/objects/nested/b.png",
                Route::GetObject,
                &[("name", "photos"), ("key", "nested/b.png")],
            ),
            (
                "delete",
                "/b/storage/api/buckets/photos/objects/nested/b.png",
                Route::DeleteObject,
                &[("name", "photos"), ("key", "nested/b.png")],
            ),
        ]);
    }

    /// The per-user preamble (a session is required, the per-user rate limit
    /// is spent) runs for exactly the rows declared `Authenticated`. The
    /// public share link limits itself by IP inside its handler; the admin
    /// rows are gated by the router from the declaration and were never
    /// rate-limited.
    #[test]
    fn user_preamble_follows_the_declared_level() {
        for row in ROUTES {
            assert_eq!(
                user_preamble(row.handler),
                row.auth == AuthLevel::Authenticated,
                "{} {}",
                row.method,
                row.template
            );
        }
    }

    /// What the router enforces for each new row, resolved from the
    /// declaration alone through the same `endpoint_auth` it calls. The
    /// `/b/storage/` and `/b/cloudstorage/` prefix rows are `Public`, so the
    /// declared level is the whole gate for the admin JSON API that used to
    /// sit behind the admin block's `Admin` prefix.
    #[test]
    fn declared_levels_gate_the_router() {
        let eps = FilesBlock::new().info().endpoints;
        for (action, path) in [
            ("retrieve", "/b/cloudstorage/admin/shares"),
            ("retrieve", "/b/cloudstorage/admin/access-logs"),
            ("retrieve", "/b/cloudstorage/admin/quotas"),
            ("update", "/b/cloudstorage/admin/quotas/u-1"),
            ("retrieve", "/b/storage/admin/api/buckets"),
            ("retrieve", "/b/storage/admin/api/stats"),
            ("retrieve", "/b/storage/admin"),
            ("retrieve", "/b/storage/admin/"),
        ] {
            assert_eq!(
                endpoint_auth(&eps, action, path),
                Some(AuthLevel::Admin),
                "{action} {path}"
            );
        }
        assert_eq!(
            endpoint_auth(&eps, "retrieve", "/b/storage/direct/tok-1"),
            Some(AuthLevel::Public)
        );
        for (action, path) in [
            ("retrieve", "/b/cloudstorage/shares"),
            ("create", "/b/cloudstorage/shares"),
            ("delete", "/b/cloudstorage/shares/s-1"),
            ("retrieve", "/b/cloudstorage/quota"),
            ("retrieve", "/b/storage/api/search"),
            ("retrieve", "/b/storage/api/recent"),
            ("delete", "/b/storage/api/buckets/photos"),
            ("retrieve", "/b/storage/photos/nested/"),
            (
                "retrieve",
                "/b/storage/api/buckets/photos/objects/nested/b.png",
            ),
        ] {
            assert_eq!(
                endpoint_auth(&eps, action, path),
                Some(AuthLevel::Authenticated),
                "{action} {path}"
            );
        }
        // The slash variant of an admin page matches only the folder row; it
        // was gated `Authenticated` by the fail-closed default before and must
        // not become more permissive now that a row covers it.
        assert!(
            matches!(
                endpoint_auth(&eps, "retrieve", "/b/storage/admin/buckets/"),
                Some(AuthLevel::Authenticated | AuthLevel::Admin)
            ),
            "/b/storage/admin/buckets/ must stay at least Authenticated"
        );
    }
}

#[cfg(test)]
mod handle_tests {
    use wafer_run::{Block as _, InputStream};

    use super::*;
    use crate::test_support::{admin_msg, anon_msg, output_is_error, output_json, TestContext};

    /// The stats endpoint the admin block used to reach by rewriting
    /// `/b/admin/api/storage/stats` to a synthetic path and forwarding it
    /// is served under the files prefix.
    #[tokio::test]
    async fn admin_stats_are_served_under_the_files_prefix() {
        let ctx = TestContext::with_files().await;
        let out = FilesBlock::new()
            .handle(
                &ctx,
                admin_msg("retrieve", "/b/storage/admin/api/stats"),
                InputStream::empty(),
            )
            .await;
        let body = output_json(out).await;
        assert_eq!(
            body.get("bucket_count").and_then(|v| v.as_i64()),
            Some(0),
            "{body}"
        );
    }

    /// The quota update reads `{id}` as the table bound it, end to end.
    #[tokio::test]
    async fn admin_quota_update_reads_the_bound_user_id() {
        let ctx = TestContext::with_files().await;
        let body = serde_json::to_vec(&serde_json::json!({ "max_storage_bytes": 5 })).unwrap();
        let out = FilesBlock::new()
            .handle(
                &ctx,
                admin_msg("update", "/b/cloudstorage/admin/quotas/u-9"),
                InputStream::from_bytes(body),
            )
            .await;
        let record = output_json(out).await;
        assert_eq!(
            record["data"]["user_id"],
            serde_json::json!("u-9"),
            "{record}"
        );
        let quota = quota::get_user_quota(&ctx, "u-9").await.expect("quota row");
        assert_eq!(quota.max_storage_bytes, 5);
    }

    /// `reset_period_days` is not a quota field: nothing enforces a reset
    /// period, so an update naming it is refused rather than stored as a
    /// setting that does nothing. The refusal is the whole request — the
    /// other field in it is not written either.
    #[tokio::test]
    async fn admin_quota_update_naming_reset_period_days_is_refused() {
        let ctx = TestContext::with_files().await;
        let body = serde_json::to_vec(&serde_json::json!({
            "max_storage_bytes": 5,
            "reset_period_days": 30,
        }))
        .unwrap();
        let out = FilesBlock::new()
            .handle(
                &ctx,
                admin_msg("update", "/b/cloudstorage/admin/quotas/u-9"),
                InputStream::from_bytes(body),
            )
            .await;
        assert!(output_is_error(out, "InvalidArgument").await);
        assert_eq!(
            quota::get_user_quota(&ctx, "u-9")
                .await
                .expect("quota lookup"),
            models::QuotaConfig::effective_default(),
            "a refused update writes no override row",
        );
    }

    /// The share link is the block's one public row, and its handler reads
    /// the `{token}` the row binds. A bogus token is read, fails signature
    /// verification and answers `NotFound`; a handler reading any other
    /// variable name would see an empty token and answer `InvalidArgument`
    /// ("Missing share token") instead, which is what this distinguishes.
    #[tokio::test]
    async fn share_link_handler_reads_the_bound_token() {
        let ctx = TestContext::with_files().await;
        let out = FilesBlock::new()
            .handle(
                &ctx,
                anon_msg("retrieve", "/b/storage/direct/bogus"),
                InputStream::empty(),
            )
            .await;
        assert!(output_is_error(out, "NotFound").await);
    }

    /// The router gates the user rows `Authenticated` from the declaration;
    /// the block keeps refusing an anonymous caller itself as belt and braces.
    #[tokio::test]
    async fn anonymous_user_rows_are_refused_by_the_block_itself() {
        let ctx = TestContext::with_files().await;
        let out = FilesBlock::new()
            .handle(
                &ctx,
                anon_msg("retrieve", "/b/cloudstorage/quota"),
                InputStream::empty(),
            )
            .await;
        assert!(output_is_error(out, "Unauthenticated").await);
    }
}
