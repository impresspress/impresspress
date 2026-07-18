//! Storage HANDLERS for the `impresspress/files` block: the user-facing
//! `/b/storage/api/...` JSON API and its admin-delegated
//! `/admin/storage/...` counterpart. (This is the impresspress-core files
//! storage-handlers module — NOT the Cloudflare R2 adapter.)
//!
//! Split by domain responsibility:
//! - [`params`] — path-parameter extraction (bucket name / object key).
//! - [`validation`] — bucket-name / storage-key validation rules, shared
//!   with the share-creation path (`cloud.rs`).
//! - [`access`] — bucket-ownership / access-control predicates.
//! - [`buckets`] — bucket lifecycle: list, create, delete.
//! - [`objects`] — object lifecycle: list, download (streamed), upload,
//!   delete.
//! - [`search`] — object search + recently-viewed listing.
//! - [`admin`] — the admin-delegated JSON API (`/admin/storage/...`),
//!   including the aggregate stats endpoint.
//!
//! Every item that was previously `pub`/`pub(super)` at `storage::*` is
//! re-exported here so external callers (`cloud.rs`, `pages_user::*`,
//! `blocks/files/mod.rs`) keep using the same paths unchanged.

use wafer_run::{context::Context, HttpMethod, InputStream, Message, OutputStream};

use crate::{
    endpoint_match::{self, EndpointRoute},
    http::err_not_found,
};

mod access;
mod admin;
mod buckets;
mod objects;
mod params;
mod search;
mod validation;

pub(in crate::blocks::files) use access::{bucket_owned_by, is_bucket_access_denied};
pub use admin::handle_admin;
pub(in crate::blocks::files) use validation::{
    is_valid_bucket_name, is_valid_storage_key, BUCKET_NAME_MAX_LEN, BUCKET_NAME_MIN_LEN,
    BUCKET_NAME_PATTERN,
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
        Route::ListBuckets => buckets::handle_list_buckets(ctx, &msg).await,
        Route::CreateBucket => buckets::handle_create_bucket(ctx, &msg, input).await,
        Route::ListObjects => objects::handle_list_objects(ctx, &msg).await,
        Route::GetObject => objects::handle_get_object(ctx, &msg).await,
        Route::UploadObject => objects::handle_upload_object(ctx, &msg, input).await,
        Route::DeleteObject => objects::handle_delete_object(ctx, &msg).await,
        Route::DeleteBucket => buckets::handle_delete_bucket(ctx, &msg).await,
        Route::Search => search::handle_search(ctx, &msg).await,
        Route::Recent => search::handle_recent(ctx, &msg).await,
    }
}

/// Test-only fixture shared by more than one domain submodule's integration
/// tests (buckets, objects, and admin/stats all seed buckets the same way).
#[cfg(test)]
mod test_helpers {
    use serde_json::json;

    use crate::{blocks::files::repo, test_support::TestContext};

    pub(super) async fn seed_bucket(ctx: &TestContext, name: &str, owner: &str) {
        let data = crate::util::json_map(json!({
            "name": name,
            "public": false,
            "created_by": owner,
            "created_at": crate::util::now_rfc3339(),
        }));
        repo::buckets::seed(ctx, data).await.expect("seed bucket");
    }
}
