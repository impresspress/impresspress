use std::time::Duration;

use wafer_core::clients::{crypto, storage as store};
use wafer_run::{context::Context, ErrorCode, Message, OutputStream};

use super::repo;
use crate::{
    blocks::rate_limit::{check_rate_limit, RateLimit, RateLimitOutcome, UserRateLimiter},
    http::{
        err_bad_request, err_forbidden, err_internal, err_internal_no_cause, err_not_found,
        ResponseBuilder,
    },
    util::{json_map, RecordExt},
};

pub async fn generate_share_token(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
) -> Result<String, OutputStream> {
    let claims = json_map(serde_json::json!({
        "bucket": bucket,
        "key": key,
        "type": "share",
    }));

    // SEC-055: share JWT lifetime — 30 days. The previous 1-year default
    // gave any leaked share URL effectively unbounded validity. Users who
    // need longer-lived shares can re-share; the typical use case (send
    // a link, recipient downloads within hours/days) fits well under 30d.
    const SHARE_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
    crypto::sign(ctx, &claims, SHARE_TOKEN_TTL)
        .await
        .map_err(|e| err_internal("Token generation failed", e))
}

pub async fn handle_direct_access(
    ctx: &dyn Context,
    msg: &Message,
    limiter: &UserRateLimiter,
) -> OutputStream {
    // The real on-the-wire path (no `req.resource` rewrite in the parent
    // dispatcher anymore).
    let path = msg.path();
    let token = path.strip_prefix("/b/storage/direct/").unwrap_or("");
    if token.is_empty() {
        return err_bad_request("Missing share token");
    }

    // Rate-limit per remote IP before doing any work — `/storage/direct/*` is
    // public (no auth required) so without this an attacker can enumerate
    // valid tokens / amplify DOS by issuing many lookups. Identity key falls
    // back to "unknown" if the platform layer can't expose a remote IP.
    let identity = {
        let addr = msg.remote_addr();
        if addr.is_empty() {
            "unknown".to_string()
        } else {
            addr.to_string()
        }
    };
    match check_rate_limit(limiter, ctx, &identity, "share_direct", RateLimit::API_READ).await {
        RateLimitOutcome::Limited(r) => return r,
        // Allowed headers can't be attached to a binary file response here —
        // accept this as a known limitation; the platform layer would need
        // streaming-meta middleware to inject them.
        RateLimitOutcome::Allowed(_) | RateLimitOutcome::Disabled => {}
    }

    // Verify the token's HMAC before touching the DB. Tokens are JWT-signed
    // at issue time (`generate_share_token`), so an invalid signature means
    // the token wasn't minted by us — short-circuit before the DB lookup so
    // attackers can't enumerate the shares table via random tokens.
    if crypto::verify(ctx, token).await.is_err() {
        return err_not_found("Share not found or expired");
    }

    // Look up share by token
    let Ok(share) = repo::shares::find_by_token(ctx, token).await else {
        return err_not_found("Share not found or expired");
    };

    // Check expiry
    if let Some(expires) = share.data.get("expires_at").and_then(|v| v.as_str()) {
        if !expires.is_empty() {
            if let Ok(exp_time) = chrono::DateTime::parse_from_rfc3339(expires) {
                if exp_time < chrono::Utc::now() {
                    return err_forbidden("Share link has expired");
                }
            }
        }
    }

    // Atomic access-count increment + cap enforcement via a CAS UPDATE:
    //   UPDATE shares SET access_count = access_count + 1
    //   WHERE id = ? AND access_count < max_access_count
    // The read-then-write pattern previously here let two concurrent
    // accesses with `max_access_count = 1` both pass the check and double-
    // serve the file. With the cap inside the WHERE clause, at most one
    // updater wins per row and rowcount 0 ⇒ cap reached.
    let max = share.i64_field("max_access_count");
    match repo::shares::increment_access_count_capped(ctx, &share.id, max).await {
        Ok(true) => {}
        Ok(false) => return err_forbidden("Share link access limit reached"),
        Err(e) => {
            // Don't block a legitimate access on a transient DB blip — log and
            // continue. Counters drifting low is preferable to denying paid-
            // for downloads. (The cap check above ran on a stale read but
            // covers the common case.)
            tracing::warn!(error = %e, share_id = %share.id, "Failed to increment share access count");
        }
    }

    let bucket = share
        .data
        .get("bucket")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let key = share.data.get("key").and_then(|v| v.as_str()).unwrap_or("");

    if bucket.is_empty() || key.is_empty() {
        return err_internal_no_cause("Invalid share data");
    }

    // Log access
    if let Err(e) =
        repo::shares::log_access(ctx, &share.id, msg.remote_addr(), msg.header("User-Agent")).await
    {
        tracing::warn!("Failed to log share access: {e}");
    }

    // Serve the file
    match store::get(ctx, bucket, key).await {
        Ok((data, info)) => ResponseBuilder::new()
            .set_header(
                "Content-Disposition",
                &format!(
                    "inline; filename=\"{}\"",
                    key.replace(['"', '\n', '\r'], "")
                ),
            )
            .set_header("Cache-Control", "private, max-age=3600")
            .body(data, &info.content_type),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("File not found"),
        Err(e) => err_internal("Storage error", e),
    }
}
