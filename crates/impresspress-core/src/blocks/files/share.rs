use wafer_core::clients::{crypto, storage as store};
use wafer_run::{context::Context, Message, OutputStream};

use super::repo;
use crate::{
    blocks::{
        crud,
        rate_limit::{check_rate_limit, ip_identity, RateLimit, RateLimitOutcome, UserRateLimiter},
    },
    http::{err_forbidden, err_internal, err_internal_no_cause, err_not_found},
    util::hex_encode,
};

/// Bytes of entropy in a share token. 256 bits of CSPRNG output is what
/// makes the token unguessable, which is the whole of its secrecy: it is a
/// bearer credential naming one row, not a signed assertion about it.
const SHARE_TOKEN_BYTES: usize = 32;

/// Mint the opaque token that addresses a share link.
///
/// The token says nothing — not the bucket, not the key, not a lifetime.
/// The share row it selects carries the expiry and the access cap, and
/// [`handle_direct_access`] enforces both from that row, so a link lives
/// exactly as long as the share its owner created and there is no second
/// clock to disagree with it.
pub async fn generate_share_token(ctx: &dyn Context) -> Result<String, OutputStream> {
    crypto::random_bytes(ctx, SHARE_TOKEN_BYTES)
        .await
        .map(|bytes| hex_encode(&bytes))
        .map_err(|e| err_internal("Token generation failed", e))
}

pub async fn handle_direct_access(
    ctx: &dyn Context,
    msg: &Message,
    limiter: &UserRateLimiter,
) -> OutputStream {
    // `{token}` in `GET /b/storage/direct/{token}`, as the block's route
    // table bound it.
    let token = match crud::path_var(msg, "token", "Missing share token") {
        Ok(value) => value,
        Err(response) => return response,
    };

    // Rate-limit per client network (`ip_identity`) before doing any work —
    // `/storage/direct/*` is public (no auth required) so without this an
    // attacker can enumerate valid tokens / amplify DOS by issuing many
    // lookups.
    let identity = ip_identity(msg);
    match check_rate_limit(limiter, ctx, &identity, "share_direct", RateLimit::API_READ).await {
        RateLimitOutcome::Limited(r) => return r,
        // Allowed headers can't be attached to a binary file response here —
        // accept this as a known limitation; the platform layer would need
        // streaming-meta middleware to inject them.
        RateLimitOutcome::Allowed(_) | RateLimitOutcome::Disabled => {}
    }

    // The token is the whole credential: 256 random bits addressing one row,
    // so a lookup miss is the only "wrong token" there is. A lookup that
    // FAILED is not a miss — an outage on the shares table is a 500, not a
    // 404 telling the recipient their link was revoked.
    let share = match repo::shares::find_by_token(ctx, token).await {
        Ok(share) => share,
        Err(e) => return crud::db_error(e, "Share not found or expired", "Share lookup failed"),
    };

    // Check expiry against the row, which is the only place a share's
    // lifetime is recorded — the token asserts nothing.
    //
    // A row with NO expiry is not a link that lives forever, it is a link
    // whose end cannot be established, and it is refused on the same terms
    // as one whose expiry cannot be read: as a fault, not as an expiry.
    // ("Your link has expired" would be as false a reason as the "not
    // found" the lookup arm above no longer gives.) Nothing in this release
    // can write such a row — `NewShare::expires_at` is not optional and the
    // legacy repair (migration 003) gave every historical row an end — but
    // the column is nullable, so an import, a restore or a hand-written row
    // can still produce one, and this is what stops it being served.
    let Some(expires) = share.expires_at.as_deref() else {
        tracing::error!(
            share_id = %share.id,
            "share row records no expiry; refusing rather than serving an endless link",
        );
        return err_internal_no_cause("Share link is unavailable");
    };
    let Ok(exp_time) = chrono::DateTime::parse_from_rfc3339(expires) else {
        tracing::error!(
            share_id = %share.id,
            expires_at = %expires,
            "share row carries an unparseable expires_at",
        );
        return err_internal_no_cause("Share link is unavailable");
    };
    if exp_time < chrono::Utc::now() {
        return err_forbidden("Share link has expired");
    }

    // Refuse a share already at its cap before paying for the object. The
    // authority is still the CAS UPDATE below — this read can be stale, and
    // a share at `access_count = max - 1` with two requests in flight is
    // decided there — but a share that is visibly finished must not cost a
    // storage read per request for the rest of its life.
    if let Some(max) = share.max_access_count {
        if share.access_count >= max {
            return err_forbidden("Share link access limit reached");
        }
    }

    let bucket = share.bucket.as_str();
    let key = share.key.as_str();

    if bucket.is_empty() || key.is_empty() {
        return err_internal_no_cause("Invalid share data");
    }

    // Resolve the object BEFORE spending an access. The share names the
    // object key; the bytes are wherever the object's row says they are (each
    // upload stores them under a key of its own), so a key with no row is a
    // missing file. `get_stream` resolves the `ObjectInfo` header eagerly, so
    // a share whose object is missing fails here — without burning one of a
    // capped share's accesses on a request that serves nothing.
    let blob_key = match repo::objects::find_blob_key(ctx, bucket, key).await {
        Ok(Some(blob_key)) => blob_key,
        Ok(None) => return err_not_found("File not found"),
        Err(e) => return crud::db_error_internal(e, "Object lookup failed"),
    };
    let stream = match store::get_stream(ctx, bucket, &blob_key).await {
        Ok(stream) => stream,
        Err(e) => return crud::db_error(e, "File not found", "Storage error"),
    };

    // Spend the access. The cap lives inside the UPDATE's WHERE clause —
    // this, not the stale read above, is what decides it:
    //   UPDATE shares SET access_count = access_count + 1
    //   WHERE id = ? AND access_count < max_access_count
    // so at most one updater wins per row and rowcount 0 ⇒ cap reached; two
    // concurrent accesses cannot both pass a `max_access_count = 1` share.
    // `None` (no cap) reaches `increment_access_count_capped` as 0, which is
    // the "unlimited" sentinel its filter is written against.
    //
    // This statement IS the cap check, so a failure refuses the download.
    // The read above cannot stand in for it: it saw one moment, and a share
    // one access short of its cap passes it every time.
    let max = share.max_access_count.unwrap_or(0);
    match repo::shares::increment_access_count_capped(ctx, &share.id, max).await {
        Ok(true) => {}
        Ok(false) => return err_forbidden("Share link access limit reached"),
        Err(e) => return crud::db_error_internal(e, "Share access accounting failed"),
    }

    // The audit trail, unlike the counter, is not load-bearing for the cap:
    // a lost log row is logged and the download proceeds.
    if let Err(e) =
        repo::shares::log_access(ctx, &share.id, msg.remote_addr(), msg.header("User-Agent")).await
    {
        tracing::warn!("Failed to log share access: {e}");
    }

    // Serve the file — stream the body straight from storage (R2
    // `get_streaming` on CF) rather than buffering the whole object in the
    // isolate. The leading meta carries the streaming opt-in marker +
    // content-type + download headers so the pipeline and platform adapter
    // take the streaming response path (see `crate::streaming`).
    //
    // This route is unauthenticated and its bytes are whatever an uploader
    // chose, so the headers come from [`super::serving`] — the same builder
    // the authenticated download uses. That is what keeps an uploaded page
    // from executing on this origin when its owner sends someone the link.
    //
    // No local fallback for a backend that reports no type: the empty string
    // is not a media type, so `serving` substitutes `application/octet-stream`
    // for it exactly as it does for a type it cannot read.
    let leading = super::serving::user_object_leading_meta(
        &stream.info().content_type.clone(),
        key,
        &[("Cache-Control", "private, max-age=3600")],
    );
    crate::streaming::stream_download(stream, leading)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use serde_json::json;

    use super::{
        super::test_support::{routed, seed_legacy_object, share_ctx, share_ctx_over, upload_msg},
        *,
    };
    use crate::{
        blocks::rate_limit::UserRateLimiter,
        test_support::{anon_msg, output_is_error, FailingDbOpContext, TestContext},
    };

    /// A token as the block mints them: opaque hex, asserting nothing about
    /// the share it addresses.
    const OPAQUE_TOKEN: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    /// Seed one share row verbatim, so a test controls the token, the expiry
    /// and the cap the handler will read.
    async fn seed_share(ctx: &TestContext, fields: &[(&str, serde_json::Value)]) -> String {
        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        data.insert("token".into(), json!(OPAQUE_TOKEN));
        data.insert("bucket".into(), json!("photos"));
        data.insert("key".into(), json!("a.png"));
        data.insert("created_by".into(), json!("alice"));
        data.insert("created_at".into(), json!(crate::util::now_rfc3339()));
        data.insert("access_count".into(), json!(0));
        // A live expiry by default: every share link records an end, so a
        // fixture without one is the exception a test states for itself.
        data.insert(
            "expires_at".into(),
            json!((chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339()),
        );
        for (k, v) in fields {
            data.insert((*k).to_string(), v.clone());
        }
        repo::shares::seed(ctx, data).await.expect("seed share").id
    }

    /// `GET /b/storage/direct/{token}` as the public link is fetched: the
    /// URL goes through the block's own route table, so `{token}` is bound
    /// the way the router binds it rather than the way a test believes it
    /// does.
    async fn direct_access(ctx: &dyn Context, token: &str) -> OutputStream {
        let msg = routed(anon_msg("retrieve", &format!("/b/storage/direct/{token}")));
        handle_direct_access(ctx, &msg, &UserRateLimiter::default()).await
    }

    /// Drain a served response into the bytes the recipient receives.
    async fn served_bytes(out: OutputStream) -> Vec<u8> {
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
        body
    }

    /// The share row is the only clock on a link.
    ///
    /// The token addresses a row and says nothing else; a row whose
    /// `expires_at` is a year out is served, whatever any credential-side
    /// lifetime might once have claimed. This is the day-31 failure of a
    /// year-long share: the row reads active and the link says "not found".
    #[tokio::test]
    async fn a_live_share_row_is_served_whatever_its_token_claims() {
        let ctx = share_ctx("photos", "alice").await;
        let stored: &[u8] = b"PNG\x89bytes-that-must-come-back";
        seed_legacy_object(&ctx, "photos", "a.png", stored, "image/png", "alice").await;
        let a_year_out = (chrono::Utc::now() + chrono::Duration::days(365)).to_rfc3339();
        seed_share(&ctx, &[("expires_at", json!(a_year_out))]).await;

        let body = served_bytes(direct_access(&ctx, OPAQUE_TOKEN).await).await;

        assert_eq!(
            body, stored,
            "a share whose row is live must serve, however old the link is"
        );
    }

    /// A share whose expiry has passed is refused — the row's clock is the
    /// one that ends a link now that the token carries none.
    #[tokio::test]
    async fn an_expired_share_is_refused() {
        let ctx = share_ctx("photos", "alice").await;
        seed_legacy_object(&ctx, "photos", "a.png", b"secret", "image/png", "alice").await;
        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let id = seed_share(&ctx, &[("expires_at", json!(yesterday))]).await;

        assert!(
            output_is_error(direct_access(&ctx, OPAQUE_TOKEN).await, "PermissionDenied").await,
            "a share past its expiry must not serve"
        );
        assert_eq!(
            repo::shares::find_by_id(&ctx, &id)
                .await
                .expect("share row")
                .access_count,
            0,
            "a refused link spends no access"
        );
    }

    /// A token that addresses no row — never minted, or revoked since — is
    /// a 404. Nothing else stands between a guess and the shares table now
    /// that there is no signature to check first.
    #[tokio::test]
    async fn a_token_that_addresses_no_row_is_not_found() {
        let ctx = share_ctx("photos", "alice").await;
        let id = seed_share(&ctx, &[]).await;
        repo::shares::delete(&ctx, &id).await.expect("revoke it");

        assert!(
            output_is_error(direct_access(&ctx, OPAQUE_TOKEN).await, "NotFound").await,
            "a revoked share must stop serving"
        );
        assert!(
            output_is_error(
                direct_access(&ctx, "not-a-token-anyone-minted").await,
                "NotFound"
            )
            .await,
            "a guessed token must be a 404"
        );
    }

    /// A share link minted under the old JWT token scheme stays dead.
    ///
    /// Its token used to carry a 30-day expiry that was checked before the
    /// row was read, and its row carries no expiry of its own (the share
    /// modal's expiry never reached the handler). Reading that token as the
    /// opaque string it now is would make every such link permanently live,
    /// so the block's `003_legacy_share_token_expiry` migration writes the
    /// expiry the JWT used to impose. This drives the shipped migration —
    /// through `apply_migrations`, as an operator upgrading with
    /// `--run-migrations` does — and then the real handler.
    #[tokio::test]
    async fn a_legacy_jwt_share_link_stays_dead_after_its_thirty_days() {
        let mut ctx = share_ctx("photos", "alice").await;
        ctx.set_config(crate::migration_helper::RUN_MIGRATIONS_KEY, "1");
        seed_legacy_object(
            &ctx,
            "photos",
            "a.png",
            b"once-shared",
            "image/png",
            "alice",
        )
        .await;

        // A row as the old code left it: a JWT token, minted well over 30
        // days ago, with no expiry on the row at all.
        const LEGACY_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJ0eXBlIjoic2hhcmUifQ.sig";
        let minted = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        let id = seed_share(
            &ctx,
            &[
                ("token", json!(LEGACY_TOKEN)),
                ("created_at", json!(minted)),
                ("expires_at", json!(null)),
            ],
        )
        .await;
        assert_eq!(
            repo::shares::find_by_id(&ctx, &id)
                .await
                .expect("share row")
                .expires_at,
            None,
            "precondition: a legacy UI-created share carries no expiry"
        );

        let repair: Vec<&str> = super::super::migrations::SQLITE_MIGRATIONS
            .iter()
            .skip_while(|(name, _)| *name != super::super::migrations::LEGACY_SHARE_TOKEN_EXPIRY)
            .map(|(_, sql)| *sql)
            .collect();
        assert!(!repair.is_empty(), "the repair must be a shipped migration");
        crate::migration_helper::apply_migrations(&ctx, "impresspress/files", &repair, &[])
            .await
            .expect("apply the legacy-expiry repair");

        assert!(
            output_is_error(direct_access(&ctx, LEGACY_TOKEN).await, "PermissionDenied").await,
            "a legacy link that is dead today must not come back to life"
        );
    }

    /// A row that records no end is refused, not served forever.
    ///
    /// Nothing in this release writes one — `NewShare::expires_at` is not
    /// optional — but the column is nullable, so an import, a restore or a
    /// hand-written row can still produce one. The serving path is where
    /// that stops, so the invariant does not rest on "no such row exists".
    #[tokio::test]
    async fn a_share_that_records_no_end_is_refused() {
        let ctx = share_ctx("photos", "alice").await;
        seed_legacy_object(&ctx, "photos", "a.png", b"secret", "image/png", "alice").await;
        let id = seed_share(&ctx, &[("expires_at", json!(null))]).await;
        assert_eq!(
            repo::shares::find_by_id(&ctx, &id)
                .await
                .expect("share row")
                .expires_at,
            None,
            "precondition: the row under test records no end"
        );

        assert!(
            output_is_error(direct_access(&ctx, OPAQUE_TOKEN).await, "Internal").await,
            "a share with no recorded end must be refused, not served forever"
        );
    }

    /// An expiry that cannot be read is not an absent expiry.
    ///
    /// The owner set one; a value the handler cannot parse cannot be shown
    /// to be in the future, so the link is refused rather than served
    /// forever.
    #[tokio::test]
    async fn an_unreadable_expiry_is_not_a_share_that_never_expires() {
        let ctx = share_ctx("photos", "alice").await;
        seed_legacy_object(&ctx, "photos", "a.png", b"secret", "image/png", "alice").await;
        seed_share(&ctx, &[("expires_at", json!("next tuesday"))]).await;

        assert!(
            output_is_error(direct_access(&ctx, OPAQUE_TOKEN).await, "Internal").await,
            "an expiry the handler cannot parse must refuse the link as the fault it is, \
             neither serving it forever nor telling the recipient it expired"
        );
    }

    /// A share lookup that FAILED is not a share that does not exist.
    ///
    /// Answering 404 during an outage tells the recipient their link was
    /// revoked, and tells the owner nothing.
    #[tokio::test]
    async fn a_share_lookup_outage_is_not_a_missing_share() {
        let ctx = share_ctx("photos", "alice").await;
        seed_legacy_object(&ctx, "photos", "a.png", b"bytes", "image/png", "alice").await;
        seed_share(&ctx, &[]).await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.list", repo::shares::TABLE)]);

        assert!(
            output_is_error(direct_access(&failing, OPAQUE_TOKEN).await, "Internal").await,
            "an outage on the shares table must be a 500, not `Share not found`"
        );
    }

    /// A request that serves nothing must not spend one of a capped share's
    /// accesses.
    ///
    /// The share below allows exactly one access and its object is missing.
    /// Once the object is there, that one access must still be available.
    #[tokio::test]
    async fn a_request_that_serves_nothing_does_not_spend_an_access() {
        let ctx = share_ctx("photos", "alice").await;
        let id = seed_share(&ctx, &[("max_access_count", json!(1))]).await;

        let out = direct_access(&ctx, OPAQUE_TOKEN).await;
        assert!(
            output_is_error(out, "NotFound").await,
            "a share whose object is gone must fail"
        );
        assert_eq!(
            repo::shares::find_by_id(&ctx, &id)
                .await
                .expect("share row")
                .access_count,
            0,
            "nothing was served, so no access was spent"
        );

        let stored: &[u8] = b"the-bytes";
        seed_legacy_object(&ctx, "photos", "a.png", stored, "image/png", "alice").await;
        let body = served_bytes(direct_access(&ctx, OPAQUE_TOKEN).await).await;
        assert_eq!(
            body, stored,
            "the one permitted access must still be available"
        );
    }

    /// A share that is already at its cap is refused without reading the
    /// object.
    ///
    /// The CAS UPDATE is still what decides a contended cap, but a finished
    /// share must not cost a storage read per request for the rest of its
    /// life. The object here does not exist: a handler that fetched first
    /// would answer `NotFound`, and the cap answer is the true one.
    #[tokio::test]
    async fn a_finished_share_is_refused_before_the_object_is_read() {
        let ctx = share_ctx("photos", "alice").await;
        seed_share(
            &ctx,
            &[("max_access_count", json!(1)), ("access_count", json!(1))],
        )
        .await;

        assert!(
            output_is_error(direct_access(&ctx, OPAQUE_TOKEN).await, "PermissionDenied").await,
            "a share at its cap must be refused as capped, without paying for a storage read"
        );
    }

    /// An access that cannot be recorded is not served.
    ///
    /// The increment IS the cap check — it is the statement that refuses the
    /// access past `max_access_count` — so serving when it fails serves past
    /// the cap, and does it silently.
    #[tokio::test]
    async fn an_access_that_cannot_be_recorded_is_not_served() {
        let ctx = share_ctx("photos", "alice").await;
        seed_legacy_object(
            &ctx,
            "photos",
            "a.png",
            b"paid-for-bytes",
            "image/png",
            "alice",
        )
        .await;
        seed_share(&ctx, &[("max_access_count", json!(1))]).await;
        let failing = FailingDbOpContext::new(
            ctx,
            vec![("database.increment_field_where", repo::shares::TABLE)],
        );

        assert!(
            output_is_error(direct_access(&failing, OPAQUE_TOKEN).await, "Internal").await,
            "an unrecordable access must refuse the download, not serve past the cap"
        );
    }

    /// A share link serves the object the key holds NOW: its bytes are
    /// resolved through the object's row, so after a replacement the link
    /// serves the replacement — never the blob the replacement superseded,
    /// which is deleted.
    #[tokio::test]
    async fn a_share_link_serves_the_bytes_the_objects_row_names() {
        let storage = Arc::new(crate::test_support::InMemoryStorageService::new());
        let ctx = share_ctx_over("photos", "alice", storage.clone()).await;
        seed_legacy_object(&ctx, "photos", "a.png", b"first", "image/png", "alice").await;
        seed_share(&ctx, &[]).await;
        assert_eq!(
            served_bytes(direct_access(&ctx, OPAQUE_TOKEN).await).await,
            b"first"
        );

        let replaced = crate::blocks::files::storage::handle_upload_object(
            &ctx,
            &upload_msg("photos", "a.png", "image/png", "alice"),
            wafer_run::InputStream::from_bytes(b"second".to_vec()),
        )
        .await;
        assert_eq!(crate::test_support::output_http_status(replaced).await, 200);

        assert_eq!(
            served_bytes(direct_access(&ctx, OPAQUE_TOKEN).await).await,
            b"second",
            "the link serves the object's current bytes"
        );
        assert!(
            wafer_core::clients::storage::get(&ctx, "photos", "a.png")
                .await
                .is_err(),
            "the superseded blob at the object key is deleted"
        );
    }
}
