//! HTTP handlers for `impresspress/signal`. Thin: parse the path/body, read
//! config, call [`service`], map the result to a response. No auth check
//! here — every route this block declares is `AuthLevel::Public` (the game
//! has no account), enforced centrally from the declaration in
//! [`super::ROUTES`].

use serde::Deserialize;
use serde_json::json;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::service::{self, RoomError};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_conflict, err_not_found, ok_json},
};

/// Request body of both `POST .../offer` and `POST .../answer`.
#[derive(Debug, Deserialize)]
struct Body {
    sdp: String,
}

/// Map a [`RoomError`] onto the shared HTTP helpers. The two 400s (`BadCode`,
/// `TooBig`) and the two 409s (`Taken`, `Answered`) get their own message so
/// the card can say the right sentence, even though they share a status.
fn error_response(e: RoomError) -> OutputStream {
    match e {
        RoomError::Taken => err_conflict("Room code already in use"),
        RoomError::Answered => err_conflict("Room already has an answer"),
        RoomError::Gone => err_not_found("Room not found or expired"),
        RoomError::BadCode => err_bad_request("Invalid room code"),
        RoomError::TooBig => err_bad_request("SDP too large"),
        RoomError::Db(e) => crud::db_error_internal(e, "signal store error"),
    }
}

/// The configured room lifetime, held to `1..=MAX_TTL_SECONDS` so the expiry
/// stamped from it (`service::open_room`) is always a representable date.
fn ttl_secs(ctx: &dyn Context) -> i64 {
    ctx.config_get(service::TTL_KEY)
        .and_then(|v| v.parse().ok())
        .unwrap_or(service::DEFAULT_TTL_SECONDS)
        .clamp(1, service::MAX_TTL_SECONDS)
}

fn max_sdp_bytes(ctx: &dyn Context) -> usize {
    ctx.config_get(service::MAX_SDP_KEY)
        .and_then(|v| v.parse().ok())
        .unwrap_or(service::DEFAULT_MAX_SDP_BYTES)
}

/// Parse the request body and check it against the configured size cap in
/// one place, so both `POST` handlers refuse an over-long SDP the same way
/// — before the store is ever touched, the same discipline `valid_code`
/// applies to the code itself.
async fn sdp_body(ctx: &dyn Context, input: InputStream) -> Result<String, OutputStream> {
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    let body: Body =
        serde_json::from_slice(&raw).map_err(|e| err_bad_request(&format!("Invalid body: {e}")))?;
    if body.sdp.len() > max_sdp_bytes(ctx) {
        return Err(error_response(RoomError::TooBig));
    }
    Ok(body.sdp)
}

/// `GET /b/signal/config` — `{ ice_servers, room_seconds, code_length }`.
/// The client carries no STUN hostname and no copy of the expiry; both come
/// from here so an operator can change either without a client release.
pub async fn get_config(ctx: &dyn Context) -> OutputStream {
    let stun = ctx
        .config_get(service::STUN_KEY)
        .unwrap_or(service::DEFAULT_STUN_URLS)
        .to_string();
    let ice_servers: Vec<_> = stun
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| json!({"urls": url}))
        .collect();
    ok_json(&json!({
        "ice_servers": ice_servers,
        "room_seconds": ttl_secs(ctx),
        "code_length": service::CODE_LEN,
    }))
}

/// `POST /b/signal/rooms/{code}/offer` — the create. `409` if the code is
/// live, `400` on a malformed code or an over-long SDP.
pub async fn post_offer(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let code = match crud::path_var(msg, "code", "Missing room code") {
        Ok(c) => c.to_string(),
        Err(resp) => return resp,
    };
    let sdp = match sdp_body(ctx, input).await {
        Ok(sdp) => sdp,
        Err(resp) => return resp,
    };
    match service::open_room(ctx, &code, &sdp, ttl_secs(ctx)).await {
        Ok(()) => ok_json(&json!({"ok": true})),
        Err(e) => error_response(e),
    }
}

/// `GET /b/signal/rooms/{code}/offer` — the guest's read. `404` when the
/// code is unknown or expired.
pub async fn get_offer(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let code = match crud::path_var(msg, "code", "Missing room code") {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match service::offer_for(ctx, code).await {
        Ok(sdp) => ok_json(&json!({"sdp": sdp})),
        Err(e) => error_response(e),
    }
}

/// `POST /b/signal/rooms/{code}/answer` — `404` if the room is gone, `409`
/// if an answer already stands.
pub async fn post_answer(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let code = match crud::path_var(msg, "code", "Missing room code") {
        Ok(c) => c.to_string(),
        Err(resp) => return resp,
    };
    let sdp = match sdp_body(ctx, input).await {
        Ok(sdp) => sdp,
        Err(resp) => return resp,
    };
    match service::answer_room(ctx, &code, &sdp).await {
        Ok(()) => ok_json(&json!({"ok": true})),
        Err(e) => error_response(e),
    }
}

/// `GET /b/signal/rooms/{code}/answer` — the host's poll. `{"sdp": null}`
/// while nobody has answered; once answered, the answer and the row is
/// deleted (single-use). `404` once the room is gone.
pub async fn get_answer(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let code = match crud::path_var(msg, "code", "Missing room code") {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match service::take_answer(ctx, code).await {
        Ok(Some(sdp)) => ok_json(&json!({"sdp": sdp})),
        Ok(None) => ok_json(&json!({"sdp": serde_json::Value::Null})),
        Err(e) => error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use wafer_run::Block as _;

    use super::*;
    use crate::{
        blocks::signal::SignalBlock,
        test_support::{anon_msg, output_http_status, output_json, TestContext},
    };

    async fn call(ctx: &TestContext, action: &str, path: &str, body: &str) -> OutputStream {
        SignalBlock::new()
            .handle(
                ctx,
                anon_msg(action, path),
                wafer_run::InputStream::from_bytes(body.as_bytes().to_vec()),
            )
            .await
    }

    #[tokio::test]
    async fn the_handshake_goes_through_unauthenticated() {
        let ctx = TestContext::with_signal().await;
        let out = call(
            &ctx,
            "create",
            "/b/signal/rooms/AB2CD3/offer",
            r#"{"sdp":"v=0"}"#,
        )
        .await;
        assert_eq!(output_http_status(out).await, 200);
        let got =
            output_json(call(&ctx, "retrieve", "/b/signal/rooms/AB2CD3/offer", "").await).await;
        assert_eq!(got["sdp"], "v=0");
        // The host polls before the guest answers: waiting reads as a null
        // sdp, not as a 404 — a 404 is what a wrong code looks like, and the
        // card says different things about the two.
        let waiting =
            output_json(call(&ctx, "retrieve", "/b/signal/rooms/AB2CD3/answer", "").await).await;
        assert_eq!(waiting["sdp"], serde_json::Value::Null);
        call(
            &ctx,
            "create",
            "/b/signal/rooms/AB2CD3/answer",
            r#"{"sdp":"v=1"}"#,
        )
        .await;
        let done =
            output_json(call(&ctx, "retrieve", "/b/signal/rooms/AB2CD3/answer", "").await).await;
        assert_eq!(done["sdp"], "v=1");
    }

    #[tokio::test]
    async fn an_unknown_code_is_not_found_rather_than_empty() {
        let ctx = TestContext::with_signal().await;
        assert_eq!(
            output_http_status(call(&ctx, "retrieve", "/b/signal/rooms/ZZ9ZZ9/offer", "").await)
                .await,
            404
        );
        assert_eq!(
            output_http_status(call(&ctx, "retrieve", "/b/signal/rooms/ZZ9ZZ9/answer", "").await)
                .await,
            404
        );
    }

    #[tokio::test]
    async fn a_taken_code_conflicts_so_the_host_can_roll_another() {
        let ctx = TestContext::with_signal().await;
        call(
            &ctx,
            "create",
            "/b/signal/rooms/AB2CD3/offer",
            r#"{"sdp":"v=0"}"#,
        )
        .await;
        assert_eq!(
            output_http_status(
                call(
                    &ctx,
                    "create",
                    "/b/signal/rooms/AB2CD3/offer",
                    r#"{"sdp":"v=0"}"#,
                )
                .await
            )
            .await,
            409
        );
    }

    #[tokio::test]
    async fn an_oversized_sdp_is_refused_rather_than_stored() {
        let mut ctx = TestContext::with_signal().await;
        ctx.set_config(service::MAX_SDP_KEY, "16");
        let body = format!(r#"{{"sdp":"{}"}}"#, "x".repeat(64));
        assert_eq!(
            output_http_status(call(&ctx, "create", "/b/signal/rooms/AB2CD3/offer", &body).await)
                .await,
            400
        );
    }

    #[tokio::test]
    async fn a_malformed_code_never_reaches_the_store() {
        let ctx = TestContext::with_signal().await;
        assert_eq!(
            output_http_status(
                call(
                    &ctx,
                    "create",
                    "/b/signal/rooms/aa/offer",
                    r#"{"sdp":"v=0"}"#
                )
                .await
            )
            .await,
            400
        );
    }

    #[tokio::test]
    async fn config_serves_the_stun_list_from_configuration_not_from_a_constant_in_the_client() {
        let mut ctx = TestContext::with_signal().await;
        ctx.set_config(service::STUN_KEY, "stun:one.test:3478,stun:two.test:3478");
        ctx.set_config(service::TTL_KEY, "900");
        let got = output_json(call(&ctx, "retrieve", "/b/signal/config", "").await).await;
        assert_eq!(got["ice_servers"][0]["urls"], "stun:one.test:3478");
        assert_eq!(got["ice_servers"][1]["urls"], "stun:two.test:3478");
        assert_eq!(got["room_seconds"], 900);
        assert_eq!(got["code_length"], 6);
    }

    /// A room TTL past the cap opens a room that lives for the cap, instead of
    /// panicking while it stamps the expiry: `10000000000000` seconds is past
    /// the last date chrono can represent.
    #[tokio::test]
    async fn an_out_of_range_room_ttl_is_held_to_the_cap() {
        let mut ctx = TestContext::with_signal().await;
        ctx.set_config(service::TTL_KEY, "10000000000000");
        assert_eq!(
            output_http_status(
                call(
                    &ctx,
                    "create",
                    "/b/signal/rooms/AB2CD3/offer",
                    r#"{"sdp":"v=0"}"#
                )
                .await
            )
            .await,
            200
        );
        let got = output_json(call(&ctx, "retrieve", "/b/signal/config", "").await).await;
        assert_eq!(got["room_seconds"], service::MAX_TTL_SECONDS);
    }
}
