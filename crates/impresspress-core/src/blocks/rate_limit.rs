#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

#[cfg(target_arch = "wasm32")]
use wafer_core::clients::database as db;
use wafer_core::clients::{config, database::Record};
use wafer_run::{context::Context, OutputStream, WaferError};

/// Per-user rate limiter using fixed-window counters.
///
/// Keyed by a composite string (typically `user_id:category`).
/// Separate from wafer-core's per-IP rate limiter which runs as middleware.
/// On native, uses in-memory counters (Mutex<HashMap>).
/// On wasm32 (Cloudflare Workers), uses D1-backed counters via the
/// `wafer_run__auth__rate_limits` collection.
pub struct UserRateLimiter {
    #[cfg(not(target_arch = "wasm32"))]
    buckets: Mutex<HashMap<String, RateBucket>>,
}

#[cfg(not(target_arch = "wasm32"))]
struct RateBucket {
    count: u32,
    window_start: Instant,
}

/// Rate limit configuration: max requests allowed within a time window.
///
/// Configurable via env vars using the format `RATE_LIMIT_{NAME}=requests/seconds`.
/// For example: `RATE_LIMIT_AUTH=20/60` means 20 requests per 60 seconds.
/// Set `RATE_LIMIT_{NAME}=0` to disable rate limiting for that category.
#[derive(Debug, Clone, Copy)]
pub struct RateLimit {
    pub max_requests: u32,
    pub window: Duration,
}

impl RateLimit {
    /// Login and signup: 30 requests per 60 seconds per IP.
    pub const AUTH: Self = Self {
        max_requests: 30,
        window: Duration::from_secs(60),
    };
    /// Token refresh: 30 requests per 60 seconds per IP.
    pub const REFRESH: Self = Self {
        max_requests: 30,
        window: Duration::from_secs(60),
    };
    /// API reads: 300 requests per 60 seconds per user.
    pub const API_READ: Self = Self {
        max_requests: 300,
        window: Duration::from_secs(60),
    };
    /// API writes (create/update/delete): 120 requests per 60 seconds per user.
    pub const API_WRITE: Self = Self {
        max_requests: 120,
        window: Duration::from_secs(60),
    };
    /// File uploads: 60 requests per 60 seconds per user.
    pub const UPLOAD: Self = Self {
        max_requests: 60,
        window: Duration::from_secs(60),
    };
    /// Anonymous configured-price previews: 120 requests per minute per IP.
    pub const PRODUCTS_PREVIEW: Self = Self {
        max_requests: 120,
        window: Duration::from_secs(60),
    };
    /// Anonymous Stripe Checkout creation: 30 requests per minute per IP.
    pub const PRODUCTS_CHECKOUT: Self = Self {
        max_requests: 30,
        window: Duration::from_secs(60),
    };
    /// Guest receipt status polling: 120 requests per minute per IP.
    pub const PRODUCTS_RECEIPT: Self = Self {
        max_requests: 120,
        window: Duration::from_secs(60),
    };

    /// Read config override for this rate limit category.
    ///
    /// Looks up `RATE_LIMIT_{name}` in config. Format: `requests/seconds` (e.g. `50/60`).
    /// Set to `0` to disable rate limiting for this category.
    /// Returns `None` if disabled, otherwise the resolved limit.
    pub async fn resolve(self, ctx: &dyn Context, name: &str) -> Option<Self> {
        let key = format!("WAFER_RUN_SHARED__RATE_LIMIT_{}", name.to_uppercase());
        let default = format!("{}/{}", self.max_requests, self.window.as_secs());
        let value = config::get_default(ctx, &key, &default).await;

        // "0" disables this category
        if value.trim() == "0" {
            return None;
        }

        if let Some((req_str, sec_str)) = value.split_once('/') {
            let max = req_str.trim().parse::<u32>().unwrap_or(self.max_requests);
            if max == 0 {
                return None;
            }
            let secs = sec_str
                .trim()
                .parse::<u64>()
                .unwrap_or(self.window.as_secs());
            Some(Self {
                max_requests: max,
                window: Duration::from_secs(secs),
            })
        } else {
            // Just a number = override max requests, keep default window
            let max = value.trim().parse::<u32>().unwrap_or(self.max_requests);
            if max == 0 {
                return None;
            }
            Some(Self {
                max_requests: max,
                window: self.window,
            })
        }
    }
}

impl Default for UserRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl UserRateLimiter {
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Check rate limit for a given key. Returns `Ok(remaining)` if allowed,
    /// or `Err(retry_after_secs)` if the limit is exceeded.
    ///
    /// On native, uses in-memory counters (Mutex<HashMap>).
    /// On wasm32 (Cloudflare Workers), uses D1-backed counters.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn check(&self, _ctx: &dyn Context, key: &str, limit: RateLimit) -> Result<u32, u64> {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();

        // Evict expired entries when map gets large
        if buckets.len() > 5_000 {
            buckets.retain(|_, b| now.duration_since(b.window_start) <= limit.window);
        }
        // Hard cap
        if buckets.len() > 50_000 {
            buckets.clear();
        }

        let bucket = buckets.entry(key.to_string()).or_insert(RateBucket {
            count: 0,
            window_start: now,
        });

        // Reset window if expired
        if now.duration_since(bucket.window_start) > limit.window {
            bucket.count = 0;
            bucket.window_start = now;
        }

        bucket.count += 1;

        if bucket.count > limit.max_requests {
            let remaining = limit
                .window
                .checked_sub(now.duration_since(bucket.window_start))
                .unwrap_or(Duration::ZERO);
            Err(remaining.as_secs().max(1))
        } else {
            Ok(limit.max_requests - bucket.count)
        }
    }

    /// On wasm32 (Cloudflare Workers), uses D1-backed fixed-window counters.
    ///
    /// Uses an atomic INSERT ... ON CONFLICT DO UPDATE to increment the counter
    /// within the current window, or reset if the window has expired.
    #[cfg(target_arch = "wasm32")]
    pub async fn check(&self, ctx: &dyn Context, key: &str, limit: RateLimit) -> Result<u32, u64> {
        use wafer_block::{
            db::{Filter, FilterOp},
            wire::database::OnConflict,
        };

        // std::time::SystemTime::now() panics on wasm32-unknown-unknown
        // (no system clock). Use js_sys::Date::now() which returns ms since epoch.
        let now = (js_sys::Date::now() / 1000.0) as i64;
        let window_secs = limit.window.as_secs() as i64;
        let window_cutoff = now - window_secs;

        // Atomic fixed-window upsert: increment count if window is current,
        // reset count + window_start if expired. The server renders the
        // dialect-portable SQL (CASE WHEN + CURRENT_TIMESTAMP) from the
        // structured `OnConflict::WindowedCounter` request.
        use crate::blocks::auth::RATE_LIMITS_TABLE as RATE_LIMITS;
        let id = crate::util::sha256_hex(format!("rl:{key}:{now}").as_bytes());
        let upsert_result = db::upsert(
            ctx,
            RATE_LIMITS,
            vec![
                ("id".to_string(), serde_json::json!(id)),
                ("key".to_string(), serde_json::json!(key)),
            ],
            vec!["key".to_string()],
            OnConflict::WindowedCounter {
                count_field: "count".to_string(),
                window_field: "window_start".to_string(),
                now,
                window_cutoff,
                created_fields: vec!["created_at".to_string()],
                updated_fields: vec!["updated_at".to_string()],
            },
        )
        .await;

        // Read back the current count for this window via the typed client
        // (replaces a hand-rolled `db::query_raw` of `build_select_columns`).
        let rows_result = db::list_all(
            ctx,
            RATE_LIMITS,
            vec![
                Filter {
                    field: "key".into(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(key),
                },
                Filter {
                    field: "window_start".into(),
                    operator: FilterOp::GreaterEqual,
                    value: serde_json::json!(window_cutoff),
                },
            ],
        )
        .await;

        match decide_rate_limit(
            &upsert_result,
            rows_result,
            key,
            limit.max_requests,
            window_secs as u64,
        ) {
            BackendCheckOutcome::Allowed(remaining) => Ok(remaining),
            BackendCheckOutcome::Limited(retry_after) => Err(retry_after),
            // Availability is preserved (the request is still allowed), but
            // `decide_rate_limit` has already emitted a `tracing::warn!` so
            // this is a loud, distinguishable fail-open — never the silent
            // `count = 0` allow that left CF rate limiting inert for weeks
            // (2026-07-10 incident: the `rate_limits` table didn't exist).
            BackendCheckOutcome::FailedOpen { .. } => Ok(limit.max_requests),
        }
    }

    /// Build a composite key from user identity and category.
    /// For unauthenticated endpoints (login/signup), use IP as the identity.
    pub fn key(identity: &str, category: &str) -> String {
        format!("{identity}:{category}")
    }
}

/// Outcome of the wasm32 D1-backed fixed-window decision, factored out of
/// `UserRateLimiter::check` so the fail-open path is testable on the host —
/// the `check` arm that calls this only compiles under
/// `cfg(target_arch = "wasm32")`, so a test placed inside it would never run
/// in CI. This type and [`decide_rate_limit`] carry no `target_arch` gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCheckOutcome {
    /// Under the limit. Caller should return `Ok(remaining)`.
    Allowed(u32),
    /// Over the limit. Caller should return `Err(retry_after_secs)`.
    Limited(u64),
    /// The upsert or the read-back against the D1 backend failed.
    /// Availability is preserved — the request is still allowed — but this
    /// is a distinct, logged decision, never an unlabeled `count = 0` allow.
    /// Regression target for the 2026-07-10 incident where a missing
    /// `rate_limits` table left CF rate limiting silently inert for weeks
    /// with zero log evidence.
    FailedOpen { reason: String },
}

/// Decide the outcome of a D1-backed fixed-window rate-limit check from the
/// raw upsert/read-back results, without touching the backend itself.
///
/// A failure on either the write (`upsert_result`) or the read
/// (`rows_result`) fails open for availability, but loudly: it emits a
/// `tracing::warn!` and returns [`BackendCheckOutcome::FailedOpen`] instead
/// of silently deriving `count = 0` from an empty/absent row set.
pub fn decide_rate_limit(
    upsert_result: &Result<i64, WaferError>,
    rows_result: Result<Vec<Record>, WaferError>,
    key: &str,
    max_requests: u32,
    retry_after_secs: u64,
) -> BackendCheckOutcome {
    if let Err(e) = upsert_result {
        tracing::warn!(
            error = %e,
            key = %key,
            "rate-limit backend upsert failed — failing open (allowing request, count unknown)"
        );
        return BackendCheckOutcome::FailedOpen {
            reason: e.to_string(),
        };
    }

    let rows = match rows_result {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                error = %e,
                key = %key,
                "rate-limit backend read-back failed — failing open (allowing request, count unknown)"
            );
            return BackendCheckOutcome::FailedOpen {
                reason: e.to_string(),
            };
        }
    };

    let count = rows
        .first()
        .and_then(|r| r.data.get("count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as u32;

    if count > max_requests {
        BackendCheckOutcome::Limited(retry_after_secs)
    } else {
        BackendCheckOutcome::Allowed(max_requests - count)
    }
}

/// Rate limit headers to attach to a successful response.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitHeaders {
    pub limit: u32,
    pub remaining: u32,
}

impl RateLimitHeaders {
    /// Apply these headers to a `ResponseBuilder`.
    pub fn apply(self, builder: crate::http::ResponseBuilder) -> crate::http::ResponseBuilder {
        builder
            .set_header("X-RateLimit-Limit", &self.limit.to_string())
            .set_header("X-RateLimit-Remaining", &self.remaining.to_string())
    }
}

/// Return a 429 Too Many Requests response with a `Retry-After` header.
pub fn rate_limited_response(retry_after: u64) -> OutputStream {
    use super::errors::ErrorCode;
    let wafer_code = super::errors::impresspress_error_code_to_wafer(ErrorCode::RateLimitExceeded);
    let full_message = format!(
        "[{}] Too many requests — try again later",
        ErrorCode::RateLimitExceeded.as_str()
    );
    OutputStream::error(wafer_run::WaferError {
        code: wafer_code,
        message: full_message,
        meta: vec![wafer_run::MetaEntry {
            key: "resp.header.Retry-After".to_string(),
            value: retry_after.to_string(),
        }],
    })
}

/// Outcome of a rate-limit check.
pub enum RateLimitOutcome {
    /// Allowed — caller should attach these headers to the success response.
    Allowed(RateLimitHeaders),
    /// Disabled — no rate limiting applied for this category.
    Disabled,
    /// Rate-limited — caller should return this `OutputStream` immediately.
    Limited(OutputStream),
}

/// Check a per-user/identity rate limit and return an `OutputStream` if blocked,
/// or rate-limit headers to attach to the success response.
pub async fn check_rate_limit(
    limiter: &UserRateLimiter,
    ctx: &dyn wafer_run::context::Context,
    identity: &str,
    category: &str,
    default: RateLimit,
) -> RateLimitOutcome {
    let Some(limit) = default.resolve(ctx, category).await else {
        return RateLimitOutcome::Disabled;
    };
    let key = UserRateLimiter::key(identity, category);
    match limiter.check(ctx, &key, limit).await {
        Ok(remaining) => RateLimitOutcome::Allowed(RateLimitHeaders {
            limit: limit.max_requests,
            remaining,
        }),
        Err(retry_after) => RateLimitOutcome::Limited(rate_limited_response(retry_after)),
    }
}

/// Convenience wrapper: check per-user rate limit using the request's user_id.
///
/// Automatically determines read vs write category from the message action.
/// Returns `RateLimitOutcome::Disabled` for unauthenticated requests (empty user_id).
///
/// `upload_action` lets callers (e.g. the files block) map their "create"
/// action onto the `upload` category instead of `api_write` — pass `None` for
/// the default read/write split.
pub async fn check_user_rate_limit(
    limiter: &UserRateLimiter,
    ctx: &dyn wafer_run::context::Context,
    msg: &wafer_run::Message,
) -> RateLimitOutcome {
    check_user_rate_limit_with(limiter, ctx, msg, None).await
}

/// As [`check_user_rate_limit`], but with an optional category override for the
/// `create` action. `upload_action = Some((RateLimit::UPLOAD, "upload"))` makes
/// `create` requests count against the upload bucket; `None` uses the default
/// read (`retrieve`) vs write (everything else) split.
pub async fn check_user_rate_limit_with(
    limiter: &UserRateLimiter,
    ctx: &dyn wafer_run::context::Context,
    msg: &wafer_run::Message,
    create_override: Option<(RateLimit, &str)>,
) -> RateLimitOutcome {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return RateLimitOutcome::Disabled;
    }
    let action = msg.action();
    let (default, category) = match action {
        "retrieve" => (RateLimit::API_READ, "api_read"),
        "create" => create_override.unwrap_or((RateLimit::API_WRITE, "api_write")),
        _ => (RateLimit::API_WRITE, "api_write"),
    };
    check_rate_limit(limiter, ctx, &user_id, category, default).await
}

/// The identity an IP-keyed rate-limit bucket uses for a request: the remote
/// address, or `"unknown"` when the platform didn't populate one (so anonymous
/// callers behind a missing `remote_addr` still share one bucket rather than
/// bypassing the limit entirely).
pub fn ip_identity(msg: &wafer_run::Message) -> String {
    let ip = msg.remote_addr();
    if ip.is_empty() {
        "unknown".to_string()
    } else {
        ip.to_string()
    }
}

/// Whether a route-limit rule keys its bucket by client IP or by user id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKey {
    /// Key the bucket by [`ip_identity`] — for unauthenticated endpoints.
    Ip,
    /// Key the bucket by `msg.user_id()` — for authenticated endpoints. A
    /// request with an empty user_id is skipped (no limit applied).
    User,
}

/// One declarative rate-limit rule: a `(action, path)` predicate plus the
/// bucket key, category name, and default limit to apply when it matches.
pub struct RouteLimit {
    /// Predicate over `(action, normalized_path)`. The first rule whose
    /// predicate returns `true` wins.
    pub matches: fn(&str, &str) -> bool,
    /// Whether to key the bucket by IP or user id.
    pub key: LimitKey,
    /// Rate-limit category name (drives the `RATE_LIMIT_{CATEGORY}` override).
    pub category: &'static str,
    /// Default limit when no config override is present.
    pub limit: RateLimit,
}

/// Walk a declarative table of [`RouteLimit`] rules and apply the first one
/// that matches `(action, path)`. Returns `Some(outcome)` when a rule matched
/// (the caller returns the `Limited` stream or attaches the `Allowed` headers);
/// `None` when no rule matched (the request is not rate-limited at this layer).
///
/// `User`-keyed rules are skipped for requests with an empty user_id.
pub async fn check_route_limits(
    limiter: &UserRateLimiter,
    ctx: &dyn wafer_run::context::Context,
    msg: &wafer_run::Message,
    action: &str,
    path: &str,
    rules: &[RouteLimit],
) -> Option<RateLimitOutcome> {
    let rule = rules.iter().find(|r| (r.matches)(action, path))?;
    let identity = match rule.key {
        LimitKey::Ip => ip_identity(msg),
        LimitKey::User => {
            let uid = msg.user_id();
            if uid.is_empty() {
                return None;
            }
            uid.to_string()
        }
    };
    Some(check_rate_limit(limiter, ctx, &identity, rule.category, rule.limit).await)
}

#[cfg(test)]
mod tests {
    use wafer_run::{context::Context, InputStream, Message, OutputStream};

    use super::*;

    #[derive(Clone)]
    struct TestCtx;

    #[async_trait::async_trait]
    impl Context for TestCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(vec![])
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    #[tokio::test]
    async fn test_rate_limit_allows_within_window() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        let limit = RateLimit {
            max_requests: 5,
            window: Duration::from_secs(60),
        };

        // First 5 requests should succeed
        for i in (0..5).rev() {
            let result = limiter.check(&ctx, "user1:test", limit).await;
            assert_eq!(result, Ok(i));
        }
    }

    #[tokio::test]
    async fn test_rate_limit_blocks_excess() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        let limit = RateLimit {
            max_requests: 3,
            window: Duration::from_secs(60),
        };

        // Use up the limit
        assert!(limiter.check(&ctx, "user1:test", limit).await.is_ok());
        assert!(limiter.check(&ctx, "user1:test", limit).await.is_ok());
        assert!(limiter.check(&ctx, "user1:test", limit).await.is_ok());

        // 4th request should be denied
        let result = limiter.check(&ctx, "user1:test", limit).await;
        assert!(result.is_err());
        let retry_after = result.unwrap_err();
        assert!(retry_after >= 1);
    }

    #[tokio::test]
    async fn test_rate_limit_separate_keys() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        let limit = RateLimit {
            max_requests: 2,
            window: Duration::from_secs(60),
        };

        // Different keys have independent limits
        assert!(limiter.check(&ctx, "user1:auth", limit).await.is_ok());
        assert!(limiter.check(&ctx, "user1:auth", limit).await.is_ok());
        assert!(limiter.check(&ctx, "user1:auth", limit).await.is_err());

        // user2 should still be allowed
        assert!(limiter.check(&ctx, "user2:auth", limit).await.is_ok());
    }

    #[test]
    fn test_rate_limit_key_format() {
        assert_eq!(UserRateLimiter::key("user123", "auth"), "user123:auth");
        assert_eq!(
            UserRateLimiter::key("192.168.1.1", "login"),
            "192.168.1.1:login"
        );
    }

    #[tokio::test]
    async fn test_rate_limit_window_reset() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        let limit = RateLimit {
            max_requests: 2,
            window: Duration::from_millis(1),
        };

        // Use up the limit
        assert!(limiter.check(&ctx, "user:test", limit).await.is_ok());
        assert!(limiter.check(&ctx, "user:test", limit).await.is_ok());
        assert!(limiter.check(&ctx, "user:test", limit).await.is_err());

        // Wait for window to expire
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Should be allowed again
        assert!(limiter.check(&ctx, "user:test", limit).await.is_ok());
    }

    #[test]
    fn test_rate_limit_constants() {
        assert_eq!(RateLimit::AUTH.max_requests, 30);
        assert_eq!(RateLimit::AUTH.window, Duration::from_secs(60));
        assert_eq!(RateLimit::REFRESH.max_requests, 30);
        assert_eq!(RateLimit::API_READ.max_requests, 300);
        assert_eq!(RateLimit::API_WRITE.max_requests, 120);
        assert_eq!(RateLimit::UPLOAD.max_requests, 60);
        assert_eq!(RateLimit::PRODUCTS_PREVIEW.max_requests, 120);
        assert_eq!(RateLimit::PRODUCTS_CHECKOUT.max_requests, 30);
        assert_eq!(RateLimit::PRODUCTS_RECEIPT.max_requests, 120);
    }

    #[tokio::test]
    async fn test_default_impl() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::default();
        let limit = RateLimit {
            max_requests: 1,
            window: Duration::from_secs(60),
        };
        assert!(limiter.check(&ctx, "key", limit).await.is_ok());
    }

    fn msg_with(action: &str, user_id: &str, remote: &str) -> Message {
        let mut m = Message::new("test");
        m.set_meta("req.action", action);
        if !user_id.is_empty() {
            m.set_meta("auth.user_id", user_id);
        }
        if !remote.is_empty() {
            m.set_meta("req.client.ip", remote);
        }
        m
    }

    #[test]
    fn ip_identity_falls_back_to_unknown() {
        assert_eq!(ip_identity(&msg_with("create", "", "1.2.3.4")), "1.2.3.4");
        assert_eq!(ip_identity(&msg_with("create", "", "")), "unknown");
    }

    const TEST_ROUTES: &[RouteLimit] = &[
        RouteLimit {
            matches: |a, p| a == "create" && p == "/auth/api/login",
            key: LimitKey::Ip,
            category: "auth",
            limit: RateLimit {
                max_requests: 2,
                window: Duration::from_secs(60),
            },
        },
        RouteLimit {
            matches: |a, _| a == "update",
            key: LimitKey::User,
            category: "auth_write",
            limit: RateLimit {
                max_requests: 2,
                window: Duration::from_secs(60),
            },
        },
    ];

    #[tokio::test]
    async fn check_route_limits_matches_ip_rule_and_limits() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        let msg = msg_with("create", "", "9.9.9.9");
        // First two allowed, third limited.
        for _ in 0..2 {
            assert!(matches!(
                check_route_limits(
                    &limiter,
                    &ctx,
                    &msg,
                    "create",
                    "/auth/api/login",
                    TEST_ROUTES
                )
                .await,
                Some(RateLimitOutcome::Allowed(_))
            ));
        }
        assert!(matches!(
            check_route_limits(
                &limiter,
                &ctx,
                &msg,
                "create",
                "/auth/api/login",
                TEST_ROUTES
            )
            .await,
            Some(RateLimitOutcome::Limited(_))
        ));
    }

    #[tokio::test]
    async fn check_route_limits_skips_user_rule_when_anonymous_and_no_match() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        // User-keyed rule but empty user_id → None (skipped).
        let anon = msg_with("update", "", "");
        assert!(
            check_route_limits(&limiter, &ctx, &anon, "update", "/auth/api/me", TEST_ROUTES)
                .await
                .is_none()
        );
        // No rule matches this (action, path) → None.
        let other = msg_with("retrieve", "u1", "");
        assert!(check_route_limits(
            &limiter,
            &ctx,
            &other,
            "retrieve",
            "/auth/whatever",
            TEST_ROUTES
        )
        .await
        .is_none());
    }

    // -- decide_rate_limit (wasm32 D1-backend decision logic) --------------
    //
    // `UserRateLimiter::check`'s wasm32 arm only compiles under
    // `cfg(target_arch = "wasm32")`, so `cargo test` on the host never
    // exercises it directly. `decide_rate_limit` carries no `target_arch`
    // gate specifically so these regression tests run on every `cargo test`.

    fn wafer_error(message: &str) -> WaferError {
        WaferError {
            code: wafer_run::ErrorCode::Unavailable,
            message: message.to_string(),
            meta: vec![],
        }
    }

    fn count_row(count: i64) -> Record {
        let mut data = std::collections::HashMap::new();
        data.insert("count".to_string(), serde_json::json!(count));
        Record {
            id: "row1".to_string(),
            data,
        }
    }

    #[test]
    fn rate_limit_decision_is_explicit_when_upsert_fails() {
        // Regression for the CF incident where a missing `rate_limits` table
        // left limiting silently inert for weeks. A backend write failure
        // must be a logged, explicit fail-open — not an unlabeled count=0
        // allow.
        let outcome = decide_rate_limit(&Err(wafer_error("D1 down")), Ok(vec![]), "k", 5, 60);
        assert!(matches!(outcome, BackendCheckOutcome::FailedOpen { .. }));
    }

    #[test]
    fn rate_limit_decision_is_explicit_when_read_back_fails() {
        // Same regression, but for the read-back half of the check: the
        // upsert can succeed while the follow-up `list_all` still fails.
        let outcome = decide_rate_limit(&Ok(1), Err(wafer_error("D1 down")), "k", 5, 60);
        assert!(matches!(outcome, BackendCheckOutcome::FailedOpen { .. }));
    }

    #[test]
    fn rate_limit_decision_allows_under_limit() {
        let outcome = decide_rate_limit(&Ok(1), Ok(vec![count_row(2)]), "k", 5, 60);
        assert_eq!(outcome, BackendCheckOutcome::Allowed(3));
    }

    #[test]
    fn rate_limit_decision_limits_over_limit() {
        let outcome = decide_rate_limit(&Ok(1), Ok(vec![count_row(6)]), "k", 5, 60);
        assert_eq!(outcome, BackendCheckOutcome::Limited(60));
    }

    #[test]
    fn rate_limit_decision_treats_empty_rows_as_zero_count_not_failure() {
        // No row yet for this window (first request) is a legitimate empty
        // result, not a backend failure — must stay a normal `Allowed`, not
        // `FailedOpen`.
        let outcome = decide_rate_limit(&Ok(1), Ok(vec![]), "k", 5, 60);
        assert_eq!(outcome, BackendCheckOutcome::Allowed(5));
    }

    #[test]
    fn rate_limit_counts_when_upsert_succeeds() {
        // Regression for the adapter fail-open bug this PR fixes: before the
        // D1 / KV-cached-D1 / browser `DatabaseService::upsert` forwarders
        // existed (added alongside this test), every wasm rate-limit check's
        // `upsert_result` was `Err("... not implemented by this database
        // backend")`, so `decide_rate_limit` always took the `FailedOpen`
        // branch below — the limiter silently allowed every request at full
        // quota, on every backend, forever. Method *presence* is now
        // compile-enforced (`upsert`/`aggregate` are required `DatabaseService`
        // trait methods; the adapters would not build without the forwarders),
        // so a real deployment's `upsert_result` is `Ok(_)`. This test locks in
        // that once `upsert` actually succeeds, the decision is a real
        // count-based `Allowed`/`Limited` — never the fail-open branch.

        // Under the limit: counts, allows-by-remaining (not fail-open).
        let under = decide_rate_limit(&Ok(1), Ok(vec![count_row(2)]), "k", 5, 60);
        assert!(!matches!(under, BackendCheckOutcome::FailedOpen { .. }));
        assert_eq!(under, BackendCheckOutcome::Allowed(3));

        // Over the limit: counts, denies (not fail-open).
        let over = decide_rate_limit(&Ok(1), Ok(vec![count_row(6)]), "k", 5, 60);
        assert!(!matches!(over, BackendCheckOutcome::FailedOpen { .. }));
        assert_eq!(over, BackendCheckOutcome::Limited(60));
    }
}

// ---------------------------------------------------------------------------
// Cross-adapter backend conformance (scope note)
// ---------------------------------------------------------------------------
//
// The originally proposed Task 3 also called for a cross-adapter conformance
// suite exercising `D1DatabaseService` / `KvCachedD1DatabaseService` (in
// `impresspress-cloudflare`) and `BrowserDatabaseService` (in
// `impresspress-browser`) directly, e.g. over a mock KV / sqlite `inner`.
// That harness is not buildable: `impresspress-cloudflare` depends
// unconditionally on the `worker` and `wasm-bindgen` crates (not
// `target_arch`-gated in `Cargo.toml`), and `cargo check -p
// impresspress-cloudflare` fails to even *compile* on a native host target
// (`D1Database`/`R2`/`JsFuture` types are `!Send`, tripping the `async_trait`
// `Send` bound the non-wasm32 cfg arm requires). `kv_cached_db.rs` documents
// this directly: "this crate is wasm32-only and excluded from `cargo test
// --workspace`". `impresspress-browser` is the same shape (wasm-bindgen web
// bindings). Standing up a wasm-bindgen-test runner or a mock Worker KV was
// explicitly out of scope for this task.
//
// Method *presence* conformance for all three adapters is instead enforced by
// the compiler: PR 2a made `upsert`/`aggregate` required (non-defaulted)
// `DatabaseService` trait methods, so `D1DatabaseService`,
// `KvCachedD1DatabaseService`, and `BrowserDatabaseService` would not compile
// at all without the forwarders Task 2 added — the workspace build is the
// presence conformance check, re-run on every `cargo check`/`cargo test`.
// The SQL *correctness* of the shared `DbExec::upsert`/`aggregate`/
// `update_where_count` defaults those forwarders call into is covered by
// wafer-run's own test suite. What remained testable on a native host was the
// decision logic in `decide_rate_limit` above, which `rate_limit_counts_when_
// upsert_succeeds` now covers for the success path (mirroring the existing
// `rate_limit_decision_is_explicit_when_upsert_fails` for the failure path).
