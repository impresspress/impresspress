//! Bucket-ownership / access-control predicates. [`bucket_owned_by`] is the
//! single ownership predicate for the files block; [`is_bucket_access_denied`]
//! layers the JSON-API admin-bypass policy on top of it.

use wafer_run::{context::Context, Message};

use crate::blocks::files::repo;

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
pub(in crate::blocks::files) async fn bucket_owned_by(
    ctx: &dyn Context,
    user_id: &str,
    bucket: &str,
) -> bool {
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
pub(in crate::blocks::files) async fn is_bucket_access_denied(
    ctx: &dyn Context,
    msg: &Message,
    bucket: &str,
) -> bool {
    if crate::util::is_admin(msg) {
        return false;
    }
    !bucket_owned_by(ctx, msg.user_id(), bucket).await
}
