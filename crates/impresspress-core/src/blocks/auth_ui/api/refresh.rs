//! POST /b/auth/api/refresh — relocated from auth/login.rs in Task 5.
//!
//! Token-rotation flow for refresh JWTs. Implements the family-rotation
//! reuse-detection pattern (SEC-039):
//!
//! 1. Hash the incoming refresh token, look up the row by `token_hash` (SEC-032).
//! 2. If the row is already revoked, that token was rotated away — a
//!    legitimate client would only have the *current* token. Revoke the
//!    entire family.
//! 3. If the row is live, claim it with a compare-and-set revoke. The claim
//!    is what keeps two concurrent refreshes of one token from both minting a
//!    successor: exactly one wins. The loser is refused with the same answer
//!    step 2 gives, but its family is left alone — see [`refuse_not_live`].
//! 4. The winner inserts a new row under the same family ID with
//!    `generation + 1` and returns the new access + refresh pair.
//!
//! Before the claim, the token's `auth_version` claim is checked against the
//! account's: a family issued before the account's latest security-relevant
//! change is refused and revoked, whatever its row says. See the check in
//! [`handle`].

use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, OutputStream};

use crate::{
    blocks::{
        auth::{
            credential_check_failed,
            helpers::{issue_tokens_and_cookie, RoleSource, Rotation, SessionLifetime, TokenGrant},
            repo::{tokens, users},
        },
        auth_ui::contracts::{RefreshRequest, RefreshResponse, TokenType},
        crud,
        errors::{error_response, ErrorCode},
    },
    http::{err_bad_request, ResponseBuilder},
};

pub async fn handle(ctx: &dyn Context, input: InputStream) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: RefreshRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Verify the JWT signature/expiry. A valid signature alone is not enough
    // — the row lookup below is the source of truth for "this token has not
    // been used or revoked yet".
    //
    // Only `Unauthenticated` is the crypto service judging the token. Any
    // other error means the check did not run (WRAP refusal, service down),
    // and "invalid refresh token" would tell the client its credential is
    // finished when nothing about it was judged, so it is the classified
    // 403/429/503 instead — the same rule the token-row read below follows.
    let claims = match crypto::verify(ctx, &body.refresh_token).await {
        Ok(claims) => claims,
        Err(e) if e.code == wafer_run::ErrorCode::Unauthenticated => {
            return error_response(ErrorCode::InvalidToken, "Invalid or expired refresh token");
        }
        Err(e) => {
            return OutputStream::error(credential_check_failed(
                e,
                "auth: refresh token verification",
            ));
        }
    };

    let Some(user_id) = claims
        .get("user_id")
        .or_else(|| claims.get("sub"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return error_response(ErrorCode::InvalidToken, "Invalid refresh token");
    };

    let token_type = claims.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if token_type != "refresh" {
        return error_response(ErrorCode::InvalidToken, "Not a refresh token");
    }

    // [SEC-038] Require the iss claim to match this deployment. A refresh
    // token minted against a different WAFER_RUN_SHARED__FRONTEND_URL value
    // (e.g. a leaked staging secret) must not refresh into a production
    // access token.
    let expected_iss = match crate::crypto::expected_issuer(ctx).await {
        Ok(expected_iss) => expected_iss,
        Err(e) => return crud::db_error_internal(e, "Could not read the token issuer"),
    };
    let iss = claims.get("iss").and_then(|v| v.as_str()).unwrap_or("");
    if iss != expected_iss {
        return error_response(ErrorCode::InvalidToken, "Invalid or expired refresh token");
    }

    // SEC-032: look up the row by SHA-256 hash of the JWT — the raw token
    // is never stored, only its hash.
    let row = match tokens::find_by_token(ctx, &body.refresh_token).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            // Signature was valid but no row exists — token was rotated and
            // its tombstone has since been wiped, or this is a forged
            // refresh token whose family we never minted. Either way, no
            // family to revoke; just refuse.
            return error_response(ErrorCode::InvalidToken, "Refresh token has been revoked");
        }
        // A read that could not run says nothing about the token. "Revoked"
        // is a statement about the credential, and making it for this
        // deployment's own outage ends a session a working database would
        // have kept — the same rule the account-state read below follows.
        Err(e) => return crud::db_error_internal(e, "Refresh could not look the token up"),
    };

    if row.revoked {
        return refuse_not_live(ctx, &row, NotLive::RevokedAtRead).await;
    }

    // Get user and verify account is still active. Use the typed repo so
    // `disabled` / `email_verified` come off the row instead of a second
    // raw `db::get`.
    let user = match users::find_by_id(ctx, &user_id).await {
        Ok(Some(u)) => u,
        // The row is genuinely gone: the account was deleted while a refresh
        // token was still live. That is a revoked session.
        Ok(None) => return error_response(ErrorCode::NotAuthenticated, "User not found"),
        // A read that could not run is not a revoked session. Reporting it as
        // one tells the caller its credential is finished when nothing about
        // the credential changed — `AuthService.refreshSession` in
        // `packages/impresspress-js` raises the 401 as an `ImpresspressError`
        // the app has to handle — while the real cause is visible only in
        // this deployment's own logs.
        Err(e) => return crud::db_error_internal(e, "Refresh could not load the account"),
    };

    if !user.is_active() {
        return error_response(ErrorCode::AccountDisabled, "Account is disabled");
    }

    // The account version this family was issued against, set by
    // `generate_tokens` from the row the issuing sign-in read before it
    // checked anything. Every password change, reset, disable, enable,
    // soft-delete and role change bumps the account's version after it lands
    // (`bump_auth_version`), so a family behind it predates one of those and
    // ends here.
    //
    // The row lookup above cannot decide this. A password change revokes the
    // rows that exist when it runs, and a sign-in that verified the OLD
    // password can write its row a moment later; a rotation that claimed its
    // predecessor before the revocation writes its successor after it. Both
    // rows are live, and both carry the version from before the change.
    //
    // A missing claim is `0`, the column's default — the access-token rule
    // (`crate::crypto::verify_access_token`). A token minted before the claim
    // existed is accepted only for an account whose version has never moved,
    // for which `0` is exactly the version it was minted at.
    let issued_at_version = claims
        .get(users::AUTH_VERSION_FIELD)
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if issued_at_version < user.auth_version {
        return refuse_stale_family(ctx, &row).await;
    }

    let require_verification = match crate::config_vars::get_bool(
        ctx,
        crate::blocks::auth::config::REQUIRE_VERIFICATION_KEY,
        false,
    )
    .await
    {
        Ok(required) => required,
        Err(e) => return crud::db_error_internal(e, "Could not read the verification policy"),
    };
    if require_verification && !user.email_verified {
        return error_response(ErrorCode::EmailNotVerified, "Email not verified");
    }

    let email = user.email.clone();
    // The new access token's roles and the `auth_version` of the row read
    // above (see `TokenGrant`). A WRAP denial or DB error here must not
    // silently resolve to "no roles" — that would 403 an admin or
    // double-grant on the next login (SB-3).
    let grant = match TokenGrant::resolve(
        ctx,
        &user,
        RoleSource::CheckBootstrapAdmin { email: &email },
    )
    .await
    {
        Ok(grant) => grant,
        Err(e) => return crud::db_error_internal(e, "Failed to resolve user roles"),
    };

    // Preserve the original auth method across refresh — a token issued
    // via OAuth must remain "oauth.<provider>" forever, not silently
    // upgrade/downgrade. Default "password" handles refresh tokens
    // minted before this claim was added.
    let prior_auth_method = claims
        .get("auth_method")
        .and_then(|v| v.as_str())
        .unwrap_or("password")
        .to_string();

    // Claim the row before minting the replacement, so a family never holds
    // two live generations. The claim is a compare-and-set
    // ([`tokens::revoke_if_live`]): losing it means another request rotated
    // this very token first, and this one is refused with the same answer a
    // replayed token gets. If issuance then fails the user is logged out —
    // recoverable, and the alternative is a live token nobody can account for.
    // The lifetime is resolved before the claim: a misconfigured one refuses
    // the refresh and leaves the presented token live, rather than revoking it
    // for an issuance that cannot succeed (see `SessionLifetime`).
    let lifetime = match SessionLifetime::resolve_or_error(ctx).await {
        Ok(lifetime) => lifetime,
        Err(r) => return r,
    };

    match tokens::revoke_if_live(ctx, &row.id).await {
        Ok(true) => {}
        Ok(false) => return refuse_not_live(ctx, &row, NotLive::CasLoss).await,
        // The claim is the gate on minting a successor. A write that could
        // not run leaves the presented token live, so issuing anyway would
        // put a second live generation in the family — refuse, and say it was
        // this deployment that failed.
        Err(e) => return crud::db_error_internal(e, "Refresh could not rotate the token"),
    }

    // Re-issue within the *preserved* family (SEC-039): `Rotation::Within`
    // makes `generate_tokens` carry the existing family on the new refresh JWT
    // so its `family` claim agrees with the DB row that anchors reuse
    // detection, and `row.generation + 1` advances the rotation counter. This is the same shared issuance tail every other
    // login flow uses, so the userportal session row is written here too.
    let issued = match issue_tokens_and_cookie(
        ctx,
        &lifetime,
        &user_id,
        &email,
        &grant,
        &prior_auth_method,
        Rotation::Within {
            family: &row.family,
            generation: row.generation + 1,
        },
    )
    .await
    {
        Ok(i) => i,
        Err(r) => return r,
    };

    ResponseBuilder::new()
        .set_cookie(&issued.cookie)
        .json(&RefreshResponse {
            access_token: issued.access_token,
            refresh_token: issued.refresh_token,
            token_type: TokenType::Bearer,
            expires_in: issued.access_lifetime,
        })
}

/// Refuse a refresh token whose family was issued before the account's
/// current `auth_version`, and revoke the family.
///
/// The revoke is housekeeping, not the refusal: every token the family could
/// still mint from carries the same stale version and meets the same check,
/// so a revoke that fails is logged and the answer stays the same. Revoking
/// makes the rows say what the check decided, so a later presentation takes
/// the row-level refusal without reading the account.
async fn refuse_stale_family(ctx: &dyn Context, row: &tokens::TokenRow) -> OutputStream {
    tracing::info!(
        user_id = %row.user_id,
        family = %row.family,
        "refresh: family issued before the account's current auth_version; revoking it"
    );
    if let Err(e) = tokens::revoke_family(ctx, &row.family).await {
        tracing::warn!(
            user_id = %row.user_id,
            family = %row.family,
            error = %e,
            "refresh: could not revoke a stale family; it stays refused by auth_version"
        );
    }
    error_response(ErrorCode::InvalidToken, "Refresh token has been revoked")
}

/// How a refresh token turned out not to be its family's live generation.
///
/// The client is told the same thing either way — it presented a token that is
/// no longer current, and the two are indistinguishable from outside — but
/// what the deployment does about it differs.
enum NotLive {
    /// The row was already `revoked` when it was read: this token was rotated
    /// away at some earlier point and is being presented again.
    RevokedAtRead,
    /// The row was live when it was read and the compare-and-set claim still
    /// lost, so another request rotated this same token in the window between.
    CasLoss,
}

/// Refuse a refresh token that is not its family's live generation.
///
/// [`NotLive::RevokedAtRead`] is the SEC-039 replay case and burns the family
/// when one is still live: the legitimate client holds the successor, so
/// whoever presents the predecessor after the fact has a copy. Both steps fail
/// closed — an outage on the live-row check or on the family revoke would
/// leave the replayed family usable, so it surfaces as an error rather than as
/// the ordinary "revoked" rejection.
///
/// [`NotLive::CasLoss`] refuses and stops there. The family is deliberately
/// NOT burned: the row was live when THIS request read it, so the two requests
/// raced inside one rotation window, and the overwhelmingly common source of
/// that is one legitimate client refreshing twice at once — two tabs, a retry
/// — for which burning the family would sign the user out of a session that
/// was never compromised.
///
/// What it gives up is bounded. Whoever loses the race is left holding a token
/// the winner has permanently revoked, and cannot reach the successor, which
/// only the winner was handed. Presenting that token again reads a revoked row
/// and lands in `RevokedAtRead`, which burns the family as it always has — so
/// a thief gains only the silent refusals of the burst itself, whichever side
/// of it they were on.
async fn refuse_not_live(
    ctx: &dyn Context,
    row: &tokens::TokenRow,
    cause: NotLive,
) -> OutputStream {
    match cause {
        NotLive::CasLoss => {
            // Deliberately not `warn`: "reuse detected" is the line operators
            // alert on, and a double-clicked refresh must not raise it.
            tracing::info!(
                user_id = %row.user_id,
                family = %row.family,
                cause = "cas_loss",
                "refresh: lost the rotation claim to a concurrent refresh of the same token"
            );
        }
        NotLive::RevokedAtRead => {
            let family_live = match tokens::family_has_live_row(ctx, &row.family).await {
                Ok(live) => live,
                Err(e) => {
                    return crud::db_error_internal(e, "Refresh could not check the token family")
                }
            };
            if family_live {
                tracing::warn!(
                    user_id = %row.user_id,
                    family = %row.family,
                    cause = "revoked_at_read",
                    "refresh: token reuse detected; revoking entire family"
                );
                if let Err(e) = tokens::revoke_family(ctx, &row.family).await {
                    return crud::db_error_internal(e, "Refresh could not revoke the token family");
                }
            }
        }
    }
    error_response(ErrorCode::InvalidToken, "Refresh token has been revoked")
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    use super::*;
    use crate::{
        blocks::auth_ui::api::{login, signup},
        db_read,
        test_support::{
            collect_or_panic, output_http_json, output_is_error, output_json, FailingDbOpContext,
            RendezvousDbOpContext, TestContext,
        },
    };

    fn json_input(value: serde_json::Value) -> InputStream {
        InputStream::from_bytes(value.to_string().into_bytes())
    }

    fn refresh_with(token: &str) -> InputStream {
        json_input(serde_json::json!({ "refresh_token": token }))
    }

    /// Sign up and log in through the real handlers, returning the login's
    /// refresh token — a live generation-0 row in a fresh family.
    async fn fresh_refresh_token(ctx: &TestContext) -> String {
        let creds = serde_json::json!({
            "email": "reuse@example.com",
            "password": "correct-horse-battery",
        });
        let (limiter, msg) = crate::blocks::auth_ui::api::test_mail_request();
        collect_or_panic(signup::handle(&limiter, ctx, &msg, json_input(creds.clone())).await)
            .await;
        let resp = output_json(login::handle(ctx, json_input(creds)).await).await;
        resp["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("login must return a refresh token: {resp}"))
            .to_string()
    }

    /// Rotate once, so `first` is a revoked row whose family still has a
    /// live successor — the SEC-039 replay setup.
    async fn rotated_pair(ctx: &TestContext) -> (String, String) {
        let first = fresh_refresh_token(ctx).await;
        let resp = output_json(handle(ctx, refresh_with(&first)).await).await;
        let second = resp["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("rotation must return a new refresh token: {resp}"))
            .to_string();
        (first, second)
    }

    #[tokio::test]
    async fn replaying_a_rotated_token_burns_the_whole_family() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let (first, second) = rotated_pair(&ctx).await;
        let family = tokens::find_by_token(&ctx, &first)
            .await
            .expect("token lookup")
            .expect("the rotated-away row is kept as a tombstone")
            .family;

        assert!(
            output_is_error(handle(&ctx, refresh_with(&first)).await, "Unauthenticated").await,
            "a rotated-away token must be refused"
        );

        assert!(
            !tokens::family_has_live_row(&ctx, &family)
                .await
                .expect("live-row check"),
            "reuse must revoke every row in the family"
        );
        assert!(
            output_is_error(handle(&ctx, refresh_with(&second)).await, "Unauthenticated").await,
            "the live successor must be unusable once reuse was detected"
        );
    }

    #[tokio::test]
    async fn reuse_check_outage_is_not_reported_as_a_plain_rejection() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let (first, _second) = rotated_pair(&ctx).await;
        // The token lookup and the live-row check are both `database.list`
        // on the tokens table: let the lookup through, fail the check.
        let failing =
            FailingDbOpContext::new(ctx, vec![("database.list", tokens::TABLE)]).after_passing(1);

        let out = handle(&failing, refresh_with(&first)).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed reuse check leaves the replayed family live; \
             it must surface as an error, not as an ordinary 401"
        );
    }

    #[tokio::test]
    async fn family_revoke_outage_is_not_reported_as_a_plain_rejection() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let (first, _second) = rotated_pair(&ctx).await;
        let failing = FailingDbOpContext::new(ctx, vec![("database.update_where", tokens::TABLE)]);

        let out = handle(&failing, refresh_with(&first)).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed family revoke leaves the attacker's token live; \
             it must surface as an error, not as an ordinary 401"
        );
    }

    /// A rotation that happens inside the second its predecessor was minted
    /// in still has to hand back a *different* token.
    ///
    /// Everything a refresh JWT carries is the same on both sides of one
    /// rotation — same user, same family, same auth method, same issuer, and
    /// `iat`/`exp` are whole seconds — and the signer encodes claims
    /// canonically, so unless something in the claims distinguishes them the
    /// successor is the predecessor, byte for byte. It hashes to the
    /// `token_hash` the row the rotation has just revoked already holds: the
    /// unique index refuses the insert,
    /// the refresh 500s, and because the presented token was revoked first the
    /// family is left with no live generation at all. The user is signed out
    /// by a refresh that should have been routine.
    ///
    /// [`PinnedMintCrypto`](crate::test_support::PinnedMintCrypto) pins the
    /// clock so both mints land in one second; the rest of the path is the
    /// real one.
    #[tokio::test]
    async fn a_rotation_inside_one_second_mints_a_distinct_token() {
        let ctx = TestContext::with_auth_and_pinned_mint_crypto().await;
        let first = fresh_refresh_token(&ctx).await;

        let rotated = output_http_json(handle(&ctx, refresh_with(&first)).await).await;
        let second = rotated["refresh_token"].as_str().unwrap_or_else(|| {
            panic!("rotating inside the mint's own second must issue a successor, got {rotated}")
        });
        assert_ne!(
            second, first,
            "the successor must not be the predecessor, whose row the rotation just revoked"
        );

        // And the successor is usable: a token that merely looked new would
        // read back the revoked predecessor's row and be refused as a replay.
        let again = output_http_json(handle(&ctx, refresh_with(second)).await).await;
        assert!(
            again["refresh_token"].is_string(),
            "the successor must refresh in its turn, got {again}"
        );
    }

    /// A live token whose user row could not be read is an error, not a
    /// revocation: `Ok(None)` and `Err` are different answers, and collapsing
    /// them told every client its session was over whenever the database
    /// blinked. The SDK does not silently recover — `AuthService.refreshSession`
    /// raises the 401 as an `ImpresspressError` and keeps the token pair it
    /// cached (only `signOut` clears it) — so the app sees a hard refresh
    /// failure and the deployment's own logs are the only place the real cause
    /// appears.
    #[tokio::test]
    async fn an_unreadable_user_row_does_not_sign_a_live_token_out() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let token = fresh_refresh_token(&ctx).await;
        // The token row is a `database.list` on the tokens table and is left
        // alone; only the account-state read of the users table fails.
        let failing = FailingDbOpContext::new(ctx, vec![("database.get", users::TABLE)]);

        let out = handle(&failing, refresh_with(&token)).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a failed account-state read must not be answered as a revoked session"
        );
    }

    /// A lookup that could not run says nothing about the token, so it cannot
    /// be answered as "this token is finished".
    #[tokio::test]
    async fn a_token_lookup_outage_is_not_reported_as_a_revoked_session() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let token = fresh_refresh_token(&ctx).await;
        // The first `database.list` on the tokens table a refresh makes is
        // `find_by_token`, so no `after_passing` is needed here.
        let failing = FailingDbOpContext::new(ctx, vec![("database.list", tokens::TABLE)]);

        let out = handle(&failing, refresh_with(&token)).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a token lookup that could not run must not be answered as a revoked session"
        );
    }

    /// The rotation claim is the gate on minting a successor: if it could not
    /// run, the presented token is still live and issuing anyway would leave
    /// two live generations in the family.
    #[tokio::test]
    async fn a_rotation_claim_that_could_not_run_is_not_reported_as_a_plain_rejection() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let token = fresh_refresh_token(&ctx).await;
        let failing =
            FailingDbOpContext::new(ctx, vec![("database.update_where_count", tokens::TABLE)]);

        let out = handle(&failing, refresh_with(&token)).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a rotation claim that could not run must surface as an error, not as an ordinary 401"
        );
    }

    /// The refresh row IS the token: `handle` refuses any refresh JWT whose
    /// hash has no row. A rotation whose new row cannot be written must
    /// therefore hand out nothing — the alternative is a 200 carrying a pair
    /// the very next refresh rejects, on a family whose previous generation
    /// this request already revoked.
    #[tokio::test]
    async fn a_rotation_that_cannot_store_its_row_hands_out_no_tokens() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let token = fresh_refresh_token(&ctx).await;
        // Signup and login are already done, so the only `database.create`
        // left on the tokens table is the rotation's own insert.
        let failing = FailingDbOpContext::new(ctx, vec![("database.create", tokens::TABLE)]);

        let body = output_http_json(handle(&failing, refresh_with(&token)).await).await;

        assert_eq!(
            body["error"], "Internal",
            "a refresh row that could not be written is this deployment failing: {body}"
        );
        assert!(
            body.get("access_token").is_none() && body.get("refresh_token").is_none(),
            "no row, no token — the response must not carry a credential: {body}"
        );
    }

    /// Two requests presenting the SAME live refresh token, each on its own
    /// worker thread, both past the row read before either claims it.
    ///
    /// Only one may rotate. The claim is a compare-and-set, so the loser is
    /// told its token is gone instead of being handed a second live generation
    /// in the family — which is what an unconditional revoke gave it, leaving a
    /// stolen token that refreshes forever and never trips reuse detection.
    ///
    /// The winner's pair must still work afterwards. Losing the claim is not
    /// evidence of theft — one client refreshing twice at once produces it —
    /// so the loser's refusal must not take the session with it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_concurrent_refreshes_of_one_token_mint_one_pair() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let token = fresh_refresh_token(&ctx).await;
        let family = tokens::find_by_token(&ctx, &token)
            .await
            .expect("token lookup")
            .expect("the login's row")
            .family;
        // Signup and login are already done, so the only `database.list` calls
        // on the tokens table left to hold are the two racers' own lookups.
        let gated = RendezvousDbOpContext::new(ctx.clone(), "database.list", tokens::TABLE, 2);

        let racers: Vec<_> = (0..2)
            .map(|_| {
                let gated = gated.clone();
                let token = token.clone();
                tokio::spawn(async move {
                    output_http_json(handle(&gated, refresh_with(&token)).await).await
                })
            })
            .collect();

        // Joined, not awaited one after the other: a rendezvous that never
        // releases then costs the suite one timeout rather than two.
        let bodies: Vec<serde_json::Value> = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            futures::future::try_join_all(racers),
        )
        .await
        .expect("both requests must reach the rendezvous and finish")
        .expect("refresh task panicked");

        let minted = bodies
            .iter()
            .filter(|b| b["refresh_token"].is_string())
            .count();
        assert_eq!(
            minted, 1,
            "exactly one of two concurrent refreshes of one token may mint a pair: {bodies:?}"
        );
        let loser = bodies
            .iter()
            .find(|b| !b["refresh_token"].is_string())
            .expect("one request must lose the claim");
        assert_eq!(
            loser["message"], "Refresh token has been revoked",
            "the request that lost the claim is refused: {loser}"
        );

        let rows = db_read::list_every(
            &ctx,
            tokens::TABLE,
            vec![wafer_block::db::Filter {
                field: "family".into(),
                operator: wafer_block::db::FilterOp::Equal,
                value: serde_json::json!(family),
            }],
        )
        .await
        .expect("read the family back");
        assert_eq!(
            rows.len(),
            2,
            "the family holds the rotated-away row and ONE successor, not two: {rows:?}"
        );

        // The winner's session survives. On this code that holds however the
        // two requests interleave, so it does not pin the decision not to burn
        // the family on a lost claim: nothing here forces the loser to reach
        // its family check after the winner's insert, which is the ordering
        // that would tell the policies apart.
        // `losing_the_claim_behind_a_completed_rotation_leaves_the_session_alone`
        // forces it.
        let winner = bodies
            .iter()
            .find(|b| b["refresh_token"].is_string())
            .expect("one request must win the claim");
        let winner_token = winner["refresh_token"].as_str().expect("a string token");
        let again = output_http_json(handle(&ctx, refresh_with(winner_token)).await).await;
        assert!(
            again["refresh_token"].is_string(),
            "the winner's session must survive the loser's refusal: {again}"
        );
    }

    /// Runs one COMPLETE competing rotation of the same token — through the
    /// real handler, on the undecorated context — at the moment the request
    /// under test reaches its own rotation claim.
    ///
    /// That is the interleaving the multi-thread race cannot pin: it forces
    /// the loser to arrive with the winner's successor already inserted, so
    /// the family has a live row when the loser looks. Without it the loser
    /// checks too early, finds nothing live, and code that burns the family
    /// on a lost claim looks identical to code that does not.
    ///
    /// It carries its own `Context` delegation rather than reusing a
    /// `test_support` decorator because what it injects is a call to THIS
    /// module's handler.
    #[derive(Clone)]
    struct RotateBeforeTheClaim {
        inner: TestContext,
        token: String,
        /// The competing rotation's refresh token, for the caller to check
        /// afterwards. `None` until it has run.
        winner: Arc<Mutex<Option<String>>>,
        already_ran: Arc<AtomicBool>,
    }

    impl RotateBeforeTheClaim {
        fn new(inner: TestContext, token: String) -> Self {
            Self {
                inner,
                token,
                winner: Arc::new(Mutex::new(None)),
                already_ran: Arc::new(AtomicBool::new(false)),
            }
        }

        fn winner_token(&self) -> String {
            self.winner
                .lock()
                .expect("winner mutex poisoned")
                .clone()
                .expect("the competing rotation must have run")
        }
    }

    #[async_trait::async_trait]
    impl Context for RotateBeforeTheClaim {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), wafer_run::WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(
            &self,
            name: &str,
            msg: wafer_run::Message,
            input: InputStream,
        ) -> OutputStream {
            if !(name == "wafer-run/database" && msg.action() == "database.update_where_count") {
                return self.inner.call_block(name, msg, input).await;
            }
            let bytes = match input.collect_to_bytes().await {
                Ok(bytes) => bytes,
                Err(e) => return OutputStream::error(e),
            };
            let on_tokens =
                wafer_block::codec::decode::<crate::test_support::CollectionPeek>(&bytes)
                    .map(|p| p.collection == tokens::TABLE)
                    .unwrap_or(false);
            if on_tokens && !self.already_ran.swap(true, Ordering::SeqCst) {
                // The competing request runs on the INNER context, so its own
                // claim does not re-enter this branch.
                let resp = output_json(handle(&self.inner, refresh_with(&self.token)).await).await;
                let minted = resp["refresh_token"]
                    .as_str()
                    .unwrap_or_else(|| panic!("the competing rotation must succeed: {resp}"))
                    .to_string();
                *self.winner.lock().expect("winner mutex poisoned") = Some(minted);
            }
            self.inner
                .call_block(name, msg, InputStream::from_bytes(bytes))
                .await
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
    }

    /// Losing the claim to a rotation that has ALREADY landed is refused, and
    /// nothing else.
    ///
    /// Burning the family here would be indefensible: the row was live when
    /// this request read it, so the two overlapped inside one rotation window
    /// — which is what one client refreshing twice at once (two tabs, a retry)
    /// produces — and the answer to that cannot be signing the client out of
    /// the pair it just received.
    ///
    /// A genuine replay is still caught: a thief who races the victim gets
    /// nothing durable, because the next attempt reads a revoked row and takes
    /// the branch `replaying_a_rotated_token_burns_the_whole_family` covers.
    #[tokio::test]
    async fn losing_the_claim_behind_a_completed_rotation_leaves_the_session_alone() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let token = fresh_refresh_token(&ctx).await;
        let racer = RotateBeforeTheClaim::new(ctx.clone(), token.clone());

        let loser = output_http_json(handle(&racer, refresh_with(&token)).await).await;

        assert!(
            !loser["refresh_token"].is_string(),
            "the request that lost the claim must not be handed a second live \
             generation: {loser}"
        );
        assert_eq!(
            loser["message"], "Refresh token has been revoked",
            "it is refused with the answer a token that is no longer current gets: {loser}"
        );

        let winner_token = racer.winner_token();
        let again = output_http_json(handle(&ctx, refresh_with(&winner_token)).await).await;
        assert!(
            again["refresh_token"].is_string(),
            "the rotation that WON must still refresh; burning the family on a \
             lost claim kills the pair it had just minted: {again}"
        );
    }

    /// Only the crypto service judging the token makes it invalid. A verify
    /// call that could not run — the service down, or refused by WRAP — is
    /// the classified 503 or 403: "Invalid or expired refresh token" would
    /// tell the client its credential is finished when nothing judged it.
    #[tokio::test]
    async fn an_unrun_token_verification_is_classified_not_invalid() {
        use crate::test_support::{output_http_status, FailingServiceOpContext};

        for (code, status) in [
            (wafer_run::ErrorCode::Unavailable, 503),
            (wafer_run::ErrorCode::PermissionDenied, 403),
        ] {
            let ctx = TestContext::with_auth_and_crypto().await;
            let token = fresh_refresh_token(&ctx).await;
            let failing = FailingServiceOpContext::failing_with(
                ctx,
                "wafer-run/crypto",
                vec!["crypto.verify"],
                wafer_run::WaferError::new(code, "simulated crypto fault"),
            );

            let out = handle(&failing, refresh_with(&token)).await;
            assert_eq!(output_http_status(out).await, status, "{code:?}");
        }
    }

    /// The refresh rotation mints a new access token the way a sign-in does,
    /// so it has the same race to lose: a role removal committing after the
    /// rotation's grants read has seen the role must leave an access token
    /// whose first use is refused (see login's
    /// `a_role_removed_during_sign_in_leaves_a_token_that_is_refused`).
    #[tokio::test]
    async fn a_role_removed_during_refresh_leaves_a_token_that_is_refused() {
        use crate::{
            blocks::auth::bump_auth_version,
            platform_state::user_roles,
            test_support::{anon_msg, output_http_status, AfterDbOpContext},
        };

        const EMAIL: &str = "demoted@example.com";
        const PASSWORD: &str = "correct-horse-battery";
        let ctx = TestContext::with_auth()
            .await
            .with_admin_added()
            .await
            .with_sign_in_added();
        let uid = ctx.seed_account(EMAIL, PASSWORD, "user").await;
        user_roles::assign(&ctx.fixture(), &uid, "editor", "")
            .await
            .expect("grant editor");
        let auth_ui = ctx
            .clone()
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let creds = serde_json::json!({"email": EMAIL, "password": PASSWORD});
        let signed_in = output_json(login::handle(&auth_ui, json_input(creds)).await).await;
        let refresh_token = signed_in["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in succeeded: {signed_in}"))
            .to_string();

        // The admin's removal, landing the moment the rotation's grants read
        // has answered: the grant goes, then the version is bumped.
        let (fixture, holder) = (ctx.fixture(), uid.clone());
        let racing = AfterDbOpContext::new(
            auth_ui.clone(),
            "database.list",
            user_roles::TABLE,
            async move {
                let grants = user_roles::list_for_user(&fixture, &holder)
                    .await
                    .expect("read the grants");
                let editor = grants
                    .iter()
                    .find(|g| g.role == "editor")
                    .expect("the editor grant is still there");
                user_roles::remove(&fixture, &editor.id)
                    .await
                    .expect("remove the grant");
                bump_auth_version(&fixture, &holder)
                    .await
                    .expect("bump auth_version");
            },
        );

        let rotated = output_json(handle(&racing, refresh_with(&refresh_token)).await).await;
        assert!(
            racing.fired(),
            "the removal must land inside the rotation: {rotated}"
        );
        let access = rotated["access_token"]
            .as_str()
            .unwrap_or_else(|| panic!("the rotation succeeded: {rotated}"));
        let claims = wafer_core::clients::crypto::verify(&auth_ui, access)
            .await
            .expect("the minted token's signature verifies");
        assert!(
            claims
                .get("roles")
                .and_then(|r| r.as_array())
                .is_some_and(|roles| roles.iter().any(|r| r == "editor")),
            "the rotation read the grants before the removal, so its token \
             carries the removed role: {claims:?}"
        );

        // First use, through the router's verification like any request.
        let mut me = anon_msg("retrieve", "/b/auth/api/me");
        me.set_meta("http.header.authorization", format!("Bearer {access}"));
        assert_eq!(
            output_http_status(ctx.request(me).await).await,
            401,
            "an access token carrying a role removed during its mint must be refused"
        );

        // The successor it handed out was minted at the same version, so the
        // bump ends its family as well: the next rotation is refused.
        let next_refresh = rotated["refresh_token"]
            .as_str()
            .expect("the rotation handed out a successor");
        assert!(
            output_is_error(
                handle(&auth_ui, refresh_with(next_refresh)).await,
                "Unauthenticated"
            )
            .await,
            "a refresh token minted before the removal's bump must be refused"
        );

        // The control: a sign-in after the removal authenticates on the same
        // route, so the refusal above is the stale version and nothing else.
        let fresh = ctx.sign_in(EMAIL, PASSWORD).await;
        let me = fresh.bearer(anon_msg("retrieve", "/b/auth/api/me"));
        assert_eq!(output_http_status(ctx.request(me).await).await, 200);
    }

    /// A refresh token minted before tokens carried an `auth_version` claim:
    /// `source`'s claims with that one removed, re-signed, and stored under a
    /// family of its own so it is a live generation-0 row like the original.
    async fn claimless_copy_of(ctx: &TestContext, source: &str) -> String {
        let mut claims = crypto::verify(ctx, source)
            .await
            .expect("the source token verifies");
        claims.remove(users::AUTH_VERSION_FIELD);
        // The signer sets these itself.
        for set_by_signer in ["exp", "iat", "nbf"] {
            claims.remove(set_by_signer);
        }
        let family = uuid::Uuid::now_v7().to_string();
        claims.insert("family".to_string(), serde_json::json!(family));
        claims.insert(
            "jti".to_string(),
            serde_json::json!(uuid::Uuid::now_v7().to_string()),
        );
        let token = crypto::sign(ctx, &claims, std::time::Duration::from_secs(3600))
            .await
            .expect("sign the claim-less token");
        let user_id = tokens::find_by_token(ctx, source)
            .await
            .expect("token lookup")
            .expect("the source row")
            .user_id;
        let expires_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        tokens::insert(ctx, &user_id, &token, &family, 0, &expires_at)
            .await
            .expect("store the claim-less token's row");
        token
    }

    /// A refresh token with no `auth_version` claim was issued at version 0,
    /// the column's default: it refreshes while the account's version has
    /// never moved, and is refused once it has — a password change, disable
    /// or role change must end a family minted before the claim existed as
    /// surely as one minted after.
    #[tokio::test]
    async fn a_claimless_refresh_token_is_accepted_at_version_0_and_refused_after_a_bump() {
        use crate::blocks::auth::bump_auth_version;

        let ctx = TestContext::with_auth_and_crypto().await;
        let source = fresh_refresh_token(&ctx).await;
        let (before, after) = (
            claimless_copy_of(&ctx, &source).await,
            claimless_copy_of(&ctx, &source).await,
        );
        let uid = race_user_id(&ctx).await;
        assert_eq!(users::auth_version(&ctx, &uid).await.unwrap(), 0);
        assert!(
            !crypto::verify(&ctx, &before)
                .await
                .expect("verifies")
                .contains_key(users::AUTH_VERSION_FIELD),
            "precondition: the token carries no auth_version claim"
        );

        let rotated = output_http_json(handle(&ctx, refresh_with(&before)).await).await;
        assert!(
            rotated["refresh_token"].is_string(),
            "a claim-less token refreshes against an account still at version 0: {rotated}"
        );

        bump_auth_version(&ctx, &uid)
            .await
            .expect("bump auth_version");
        assert!(
            output_is_error(handle(&ctx, refresh_with(&after)).await, "Unauthenticated").await,
            "a claim-less token must not outlive the account's first auth_version bump"
        );
    }

    /// The account-changing half of a sign-in race: `change` runs the moment
    /// `inner`'s next `(op, collection)` call has answered.
    fn change_after(
        inner: &TestContext,
        op: &'static str,
        collection: &'static str,
        change: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> crate::test_support::AfterDbOpContext {
        crate::test_support::AfterDbOpContext::new(inner.clone(), op, collection, change)
    }

    /// A password change through the real handler, as the signed-in
    /// `user_id`, from `old` to `new`. Panics unless it succeeds.
    async fn change_password_now(ctx: TestContext, user_id: String, old: &str, new: &str) {
        use crate::{
            blocks::auth_ui::api::change_password,
            test_support::{auth_msg, output_status},
        };
        let msg = auth_msg("update", "/b/auth/api/change-password", &user_id);
        let body = serde_json::json!({"current_password": old, "new_password": new});
        let out = change_password::handle(&ctx, &msg, json_input(body)).await;
        assert_eq!(
            output_status(out).await,
            200,
            "the password change succeeds"
        );
    }

    const RACE_EMAIL: &str = "reuse@example.com";
    const OLD_PASSWORD: &str = "correct-horse-battery";
    const NEW_PASSWORD: &str = "new-horse-battery-2026";

    fn credentials(password: &str) -> InputStream {
        json_input(serde_json::json!({"email": RACE_EMAIL, "password": password}))
    }

    async fn race_user_id(ctx: &TestContext) -> String {
        users::find_by_email(ctx, RACE_EMAIL)
            .await
            .expect("read the account")
            .expect("the account exists")
            .id
    }

    /// Sign in with the OLD password while a password change lands between
    /// the sign-in's credential read and its refresh-row insert.
    ///
    /// The change revokes every refresh row that exists when it runs; this
    /// sign-in's row does not exist yet, so it is written live afterwards.
    /// A row-only check lets that token refresh — and keep refreshing — past
    /// a change whose whole point was ending every session the old password
    /// opened. The token carries the version read before the old password was
    /// checked, which the change's bump has passed, so it is refused.
    #[tokio::test]
    async fn a_sign_in_racing_a_password_change_leaves_a_refresh_token_that_is_refused() {
        use crate::blocks::auth::repo::local_credentials;

        let ctx = TestContext::with_auth_and_crypto().await;
        fresh_refresh_token(&ctx).await;
        let uid = race_user_id(&ctx).await;

        // The credential read is the sign-in's last read before it mints: the
        // old hash is in hand, the password it checks is the old one.
        let racing = change_after(
            &ctx,
            "database.list",
            local_credentials::TABLE,
            change_password_now(ctx.clone(), uid, OLD_PASSWORD, NEW_PASSWORD),
        );
        let signed_in = output_json(login::handle(&racing, credentials(OLD_PASSWORD)).await).await;
        assert!(
            racing.fired(),
            "the change must land inside the sign-in: {signed_in}"
        );
        let token = signed_in["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in checked the old password first: {signed_in}"));
        let family = tokens::find_by_token(&ctx, token)
            .await
            .expect("token lookup")
            .expect("the sign-in wrote its row after the revocation")
            .family;
        assert!(
            tokens::family_has_live_row(&ctx, &family)
                .await
                .expect("live-row check"),
            "the revocation ran before this row existed, so the row alone is live"
        );

        assert!(
            output_is_error(handle(&ctx, refresh_with(token)).await, "Unauthenticated").await,
            "a refresh token from a sign-in that checked the old password must not \
             outlive the password change"
        );
        assert!(
            !tokens::family_has_live_row(&ctx, &family)
                .await
                .expect("live-row check"),
            "the refused family is revoked"
        );

        // The control: a sign-in with the new password refreshes, so the
        // refusal above is the stale version and nothing else.
        let fresh = output_json(login::handle(&ctx, credentials(NEW_PASSWORD)).await).await;
        let fresh = fresh["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("the new password signs in: {fresh}"));
        let again = output_http_json(handle(&ctx, refresh_with(fresh)).await).await;
        assert!(
            again["refresh_token"].is_string(),
            "a family issued after the change refreshes: {again}"
        );
    }

    /// The same race against an admin disabling the account and enabling it
    /// again. While disabled the account refuses every refresh; once enabled,
    /// a family the sign-in wrote after the disable must not be what signs
    /// the user straight back in.
    ///
    /// The hook makes the two writes `admin::ops::set_user_disabled` makes
    /// for each — the flag, then the bump — because that function is private
    /// to the admin block.
    #[tokio::test]
    async fn a_sign_in_racing_a_disable_leaves_a_refresh_token_that_is_refused() {
        use crate::blocks::auth::{bump_auth_version, repo::local_credentials};

        let ctx = TestContext::with_auth_and_crypto().await;
        fresh_refresh_token(&ctx).await;
        let uid = race_user_id(&ctx).await;

        let (admin, holder) = (ctx.clone(), uid.clone());
        let racing = change_after(
            &ctx,
            "database.list",
            local_credentials::TABLE,
            async move {
                for disabled in [true, false] {
                    users::set_disabled(&admin, &holder, disabled)
                        .await
                        .expect("set the disabled flag");
                    bump_auth_version(&admin, &holder)
                        .await
                        .expect("bump auth_version");
                }
            },
        );
        let signed_in = output_json(login::handle(&racing, credentials(OLD_PASSWORD)).await).await;
        assert!(
            racing.fired(),
            "the disable must land inside the sign-in: {signed_in}"
        );
        let token = signed_in["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("the sign-in read the account while active: {signed_in}"));

        assert!(
            output_is_error(handle(&ctx, refresh_with(token)).await, "Unauthenticated").await,
            "a refresh token from a sign-in that straddled a disable must not \
             refresh once the account is enabled again"
        );
    }

    /// A rotation that has claimed its predecessor when a password change
    /// lands writes its successor after the change's revocation, so the
    /// successor row is live. It was minted from the account read before the
    /// change, and the next rotation refuses it.
    #[tokio::test]
    async fn a_rotation_racing_a_password_change_leaves_a_successor_that_is_refused() {
        let ctx = TestContext::with_auth_and_crypto().await;
        let token = fresh_refresh_token(&ctx).await;
        let uid = race_user_id(&ctx).await;

        // The claim is the rotation's last write before its insert.
        let racing = change_after(
            &ctx,
            "database.update_where_count",
            tokens::TABLE,
            change_password_now(ctx.clone(), uid, OLD_PASSWORD, NEW_PASSWORD),
        );
        let rotated = output_json(handle(&racing, refresh_with(&token)).await).await;
        assert!(
            racing.fired(),
            "the change must land inside the rotation: {rotated}"
        );
        let successor = rotated["refresh_token"]
            .as_str()
            .unwrap_or_else(|| panic!("the rotation claimed its predecessor first: {rotated}"));

        assert!(
            output_is_error(
                handle(&ctx, refresh_with(successor)).await,
                "Unauthenticated"
            )
            .await,
            "a successor minted from the account as it was before the password \
             change must not outlive it"
        );
    }
}
