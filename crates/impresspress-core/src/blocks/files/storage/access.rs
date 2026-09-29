//! Bucket-ownership / access-control predicates. [`bucket_owned_by`] is the
//! single ownership predicate for the files block; [`require_bucket_access`]
//! layers the JSON-API admin-bypass policy on top of it.

use wafer_run::{context::Context, Message, OutputStream, WaferError};

use crate::{
    blocks::{crud, files::repo},
    http::err_forbidden,
};

/// Whether `user_id` owns a bucket named `bucket` (i.e.
/// [`repo::buckets::find_owned`] finds a matching row), or the read's own
/// failure. A failed read is not an answer: it is never "not owned" — that
/// would turn a WRAP denial or an outage into a 403 or 404 about the bucket —
/// and never "owned". The caller answers it through the door.
///
/// This is the single ownership predicate for the files block. Callers
/// decide the admin policy on top of it:
/// - JSON API handlers go through [`require_bucket_access`], which grants
///   admins access to every bucket.
/// - The SSR user portal (`pages_user::objects::object_list_page`) deliberately does
///   NOT bypass for admins — the portal is strictly owner-scoped so an
///   admin browsing `/b/storage/` sees only their own buckets; cross-user
///   inspection happens via the admin pages instead.
pub(in crate::blocks::files) async fn bucket_owned_by(
    ctx: &dyn Context,
    user_id: &str,
    bucket: &str,
) -> Result<bool, WaferError> {
    Ok(repo::buckets::find_owned(ctx, bucket, user_id)
        .await?
        .is_some())
}

/// `Ok` when the current user owns the given bucket (or is admin); otherwise
/// the response to send: the 403 for a bucket the caller does not own, or the
/// ownership read's own failure through `crud::db_error_internal` (a WRAP
/// denial is its 403, a quota its 429, anything else the 500). See
/// [`bucket_owned_by`] for the admin-bypass policy split between the JSON API
/// and the SSR portal.
pub(in crate::blocks::files) async fn require_bucket_access(
    ctx: &dyn Context,
    msg: &Message,
    bucket: &str,
) -> Result<(), OutputStream> {
    if crate::util::is_admin(msg) {
        return Ok(());
    }
    match bucket_owned_by(ctx, msg.user_id(), bucket).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(err_forbidden("Access denied to this bucket")),
        Err(e) => Err(crud::db_error_internal(e, "Bucket ownership check failed")),
    }
}
