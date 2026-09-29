//! `impresspress/signal` — WebRTC signalling rooms for blockfarming's online
//! play (spec `2026-09-11-visual-redesign-design.md` §18.3-§18.4).
//!
//! A room code, an offer, and an answer: the smallest store that can hold a
//! WebRTC handshake between two browsers that have never met. Every endpoint
//! is public — blockfarming has no account and no login, so an endpoint that
//! required one would be an endpoint the game cannot call. Rate limiting per
//! client network (`ip_identity`, category `signal`) is the abuse guard
//! in place of auth.
//!
//! Not a relay, not a lobby, not a matchmaker, not a message bus: it holds at
//! most two strings per room for at most ten minutes and hands each of them
//! over once. See [`service`] for the store and [`rest`] for the handlers.

pub(crate) mod migrations;
pub mod rest;
pub mod service;

use std::time::Duration;

use wafer_run::{BlockInfo, HttpMethod, InstanceMode};

use crate::{
    blocks::rate_limit::{
        check_rate_limit, ip_identity, RateLimit, RateLimitOutcome, UserRateLimiter,
    },
    endpoint_match::{self, EndpointRoute},
    http::err_not_found,
};

/// In-block dispatch targets, one per declared HTTP endpoint.
#[derive(Clone, Copy)]
enum Route {
    Config,
    PostOffer,
    GetOffer,
    PostAnswer,
    GetAnswer,
}

fn code_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["code"],
        "properties": {
            "code": {
                "type": "string",
                "description": "Six-character room code (see GET /b/signal/config for its length)"
            }
        }
    })
}

/// Request body of both `POST .../offer` and `POST .../answer`.
fn sdp_request_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["sdp"],
        "properties": {
            "sdp": {"type": "string", "description": "Session description (offer or answer)"}
        }
    })
}

/// Response body of both `GET .../offer` and `GET .../answer`.
fn sdp_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "sdp": {
                "type": ["string", "null"],
                "description": "The stored SDP, or null while the answer is still pending"
            }
        }
    })
}

/// Response body of both `POST .../offer` and `POST .../answer` on success.
fn ok_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}}
    })
}

/// Response body of `GET /b/signal/config`.
fn config_response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "ice_servers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {"urls": {"type": "string"}}
                }
            },
            "room_seconds": {"type": "integer"},
            "code_length": {"type": "integer"}
        }
    })
}

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. `/b/signal/config` first (no path
/// params, so ordering against the `{code}` rows doesn't matter, but it
/// reads as the entry point).
const ROUTES: &[EndpointRoute<Route>] = &[
    EndpointRoute::public(HttpMethod::Get, "/b/signal/config", Route::Config)
        .summary("ICE servers, room lifetime and code length")
        .output(config_response_schema)
        .tags(&["signal"]),
    EndpointRoute::public(
        HttpMethod::Post,
        "/b/signal/rooms/{code}/offer",
        Route::PostOffer,
    )
    .summary("Host puts its offer up — also the room's create")
    .description("409 if the code is already live, 400 on a malformed code or an over-long SDP")
    .path_params(code_path_schema)
    .input(sdp_request_schema)
    .output(ok_response_schema)
    .tags(&["signal"]),
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/signal/rooms/{code}/offer",
        Route::GetOffer,
    )
    .summary("Guest reads the host's offer")
    .description("404 when the code is unknown or expired")
    .path_params(code_path_schema)
    .output(sdp_response_schema)
    .tags(&["signal"]),
    EndpointRoute::public(
        HttpMethod::Post,
        "/b/signal/rooms/{code}/answer",
        Route::PostAnswer,
    )
    .summary("Guest puts its answer up")
    .description("404 if the room is gone, 409 if an answer already stands")
    .path_params(code_path_schema)
    .input(sdp_request_schema)
    .output(ok_response_schema)
    .tags(&["signal"]),
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/signal/rooms/{code}/answer",
        Route::GetAnswer,
    )
    .summary("Host polls for the guest's answer")
    .description(
        "null sdp while waiting; once answered, the answer is returned and the room is \
         deleted — single-use. 404 once the room is gone",
    )
    .path_params(code_path_schema)
    .output(sdp_response_schema)
    .tags(&["signal"]),
];

/// Per-IP budget for the whole block (category `signal`; checked before
/// dispatch, so a flood never reaches the store at all). Operators tune it
/// with `WAFER_RUN_SHARED__RATE_LIMIT_SIGNAL` like every other category
/// (`RateLimit::resolve` formats `WAFER_RUN_SHARED__RATE_LIMIT_{NAME}` from
/// the category name passed to `check_rate_limit`).
///
/// 180 a minute, not 60: `UserRateLimiter::check` is a fixed window (a
/// bucket resets `window` seconds after its *own* first request, not a
/// rolling count), so the budget has to cover everything one identity can
/// throw at it inside a single 60-second window, not "a handshake" averaged
/// over time. The host's own `GET .../answer` poll alone already spends 60
/// of those — once a second for the full 60-second wait — and the client
/// retries that same poll for another 60 seconds after "Nobody joined"
/// before giving up, so one host can burn 120 in a single window before the
/// guest (who often shares the host's IP: two players on one home network,
/// or one machine testing with two tabs) sends a single byte. Add the
/// guest's own offer read, its answer post, and both sides' `GET
/// /b/signal/config` read — a handful, well under 20 — and ~140 is the
/// realistic ceiling per window; 180 leaves real headroom above that without
/// opening the door to actual abuse. (This is what 429'd two tabs on one
/// machine around the sixtieth second, before this budget existed.)
const SIGNAL_LIMIT: RateLimit = RateLimit {
    max_requests: 180,
    window: Duration::from_secs(60),
};

crate::impresspress_feature_block! {
    /// WebRTC signalling rooms for blockfarming's online play
    /// (`impresspress/signal`).
    pub struct SignalBlock;
    fields: { limiter: UserRateLimiter },
    name: "impresspress/signal",
    info: |_this| {
        use wafer_run::CollectionSchema;

        BlockInfo::new(
            "impresspress/signal",
            "0.0.1",
            "http-handler@v1",
            "WebRTC signalling rooms",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec![
            "wafer-run/database".into(),
            // `migration_helper::db_backend` reads the database backend through
            // the config client when the block's migrations run at Init.
            "wafer-run/config".into(),
        ])
        // No `grants(..)`: the block reads and writes only its own table
        // (`impresspress__signal__rooms`), so it needs no `ResourceGrant`.
        .collections(vec![CollectionSchema::new(service::TABLE)])
        .category(wafer_run::BlockCategory::Feature)
        .description(
            "Signalling for peer-to-peer WebRTC handshakes: a six-character room code \
             holds one host offer and then one guest answer, for ten minutes. Not a \
             relay, not a lobby, not a matchmaker.",
        )
        .endpoints(endpoint_match::declare(ROUTES))
        .config_keys(service::config_vars())
        .can_disable(true)
        .default_enabled(true)
    },
    handle: |this, ctx, mut msg, input| {
        // Public and unauthenticated by necessity — the game has no account
        // — so the bucket is the client network rather than a user, and it
        // is checked before dispatch so a flood cannot reach the store at
        // all. See `SIGNAL_LIMIT` for the budget's arithmetic.
        if let RateLimitOutcome::Limited(out) =
            check_rate_limit(&this.limiter, ctx, &ip_identity(&msg), "signal", SIGNAL_LIMIT).await
        {
            return out;
        }
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };
        match route {
            Route::Config => rest::get_config(ctx).await,
            Route::PostOffer => rest::post_offer(ctx, &msg, input).await,
            Route::GetOffer => rest::get_offer(ctx, &msg).await,
            Route::PostAnswer => rest::post_answer(ctx, &msg, input).await,
            Route::GetAnswer => rest::get_answer(ctx, &msg).await,
        }
    },
    lifecycle: |_this, ctx, event| {
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/signal",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await
    },
}

#[cfg(test)]
mod tests {
    use wafer_run::Block as _;

    use super::*;

    /// The game has no account, so every endpoint here must be reachable
    /// without one. A tightened tier would not fail a unit test — it would
    /// 401 the players — so the tier itself is what is asserted.
    #[test]
    fn every_signal_endpoint_is_public() {
        let info = crate::blocks::all_block_infos()
            .into_iter()
            .find(|i| i.name == "impresspress/signal")
            .expect("signal block must be in all_block_infos()");
        assert!(!info.endpoints.is_empty());
        for e in &info.endpoints {
            assert_eq!(
                e.auth,
                wafer_run::AuthLevel::Public,
                "{} must stay public",
                e.path
            );
        }
    }

    /// The three declared vars are the three the handlers read. A fourth
    /// read with no declaration is a setting nobody can see in the admin UI.
    #[test]
    fn the_block_declares_the_config_it_reads() {
        let info = SignalBlock::new().info();
        let keys: Vec<&str> = info.config_keys.iter().map(|v| v.key.as_str()).collect();
        assert!(keys.contains(&service::TTL_KEY));
        assert!(keys.contains(&service::MAX_SDP_KEY));
        assert!(keys.contains(&service::STUN_KEY));
    }

    /// Send one request through the real block, from `ip`, the way the HTTP
    /// boundary would build it (`anon_msg` plus the client-IP meta
    /// `ip_identity` reads) — sharing `block`'s single `UserRateLimiter`
    /// across every call, the same way one running server instance does.
    async fn call_from(
        block: &SignalBlock,
        ctx: &crate::test_support::TestContext,
        ip: &str,
        action: &str,
        path: &str,
        body: &str,
    ) -> wafer_run::OutputStream {
        let mut msg = crate::test_support::anon_msg(action, path);
        msg.set_meta("req.client.ip", ip);
        block
            .handle(
                ctx,
                msg,
                wafer_run::InputStream::from_bytes(body.as_bytes().to_vec()),
            )
            .await
    }

    /// A room store the block was never granted is the database door's 403 on
    /// the room write and on the room read: `RoomError::Db` carries the
    /// failure itself, so its `PermissionDenied` reaches the route.
    #[tokio::test]
    async fn a_refused_room_store_is_403() {
        use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

        let ctx = crate::test_support::TestContext::with_signal()
            .await
            .running_as("test/ungranted");
        let block = SignalBlock::new();
        let offer_path = "/b/signal/rooms/AB2CD3/offer";
        let mut misses = Vec::new();
        for (action, body) in [("create", r#"{"sdp":"v=0"}"#), ("retrieve", "")] {
            let out = call_from(&block, &ctx, "203.0.113.9", action, offer_path, body).await;
            match out.collect_buffered().await {
                Err(TerminalNotResponse::Error(error))
                    if (error.code, error.message.as_str())
                        == (ErrorCode::PermissionDenied, "Access denied") => {}
                Err(TerminalNotResponse::Error(error)) => {
                    misses.push(format!("{action}: {:?} {:?}", error.code, error.message))
                }
                Ok(_) => misses.push(format!("{action}: a response")),
                Err(_) => misses.push(format!("{action}: another terminal")),
            }
        }
        assert!(misses.is_empty(), "{misses:?}");
    }

    /// Regression for the live incident: the host's own 60-second poll loop
    /// is capacity enough by itself to starve a guest sharing its IP (two
    /// players on one home network, or one machine testing with two tabs).
    /// A host's sixty `GET .../answer` polls plus a guest's offer read and
    /// answer post — all from the one identity, all in the one window — must
    /// every one of them be served; and the budget must still run out
    /// somewhere, so a real flood is still refused.
    #[tokio::test]
    async fn a_hosts_own_polling_leaves_room_for_the_guest_on_the_same_ip() {
        let ctx = crate::test_support::TestContext::with_signal().await;
        let block = SignalBlock::new();
        let ip = "203.0.113.7";
        let code = "AB2CD3";
        let offer_path = format!("/b/signal/rooms/{code}/offer");
        let answer_path = format!("/b/signal/rooms/{code}/answer");

        // The host opens the room.
        assert_eq!(
            crate::test_support::output_http_status(
                call_from(&block, &ctx, ip, "create", &offer_path, r#"{"sdp":"v=0"}"#).await
            )
            .await,
            200,
            "the host's own create must not be rate-limited"
        );

        // The host polls for the answer once a second for a full 60-second
        // wait. Nobody has joined yet, so every one of these sixty is a 200
        // with a null sdp — exactly the load that exhausted the old 60/min
        // budget by itself.
        for i in 0..60 {
            assert_eq!(
                crate::test_support::output_http_status(
                    call_from(&block, &ctx, ip, "retrieve", &answer_path, "").await
                )
                .await,
                200,
                "the host's own poll #{i} must not 429 the host"
            );
        }

        // The guest, behind the same IP, reads the offer and posts its
        // answer — the exact pair that 429'd in production once the host's
        // own polling had spent the old budget.
        assert_eq!(
            crate::test_support::output_http_status(
                call_from(&block, &ctx, ip, "retrieve", &offer_path, "").await
            )
            .await,
            200,
            "the guest's offer read must not be starved by the host's own polling"
        );
        assert_eq!(
            crate::test_support::output_http_status(
                call_from(&block, &ctx, ip, "create", &answer_path, r#"{"sdp":"v=1"}"#).await
            )
            .await,
            200,
            "the guest's answer post must not be starved by the host's own polling"
        );

        // The limit still exists: enough further requests from the same
        // identity in the same window eventually hit 429. `/config` is
        // stateless (no room to keep alive), so it spends the rest of the
        // bucket cleanly.
        let mut saw_429 = false;
        for _ in 0..(SIGNAL_LIMIT.max_requests as usize) {
            let status = crate::test_support::output_http_status(
                call_from(&block, &ctx, ip, "retrieve", "/b/signal/config", "").await,
            )
            .await;
            if status == 429 {
                saw_429 = true;
                break;
            }
            assert_eq!(status, 200);
        }
        assert!(
            saw_429,
            "the budget must still run out once it is spent — the limit must still exist"
        );
    }
}
