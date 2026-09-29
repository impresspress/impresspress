#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use wafer_core::clients::config;
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
    /// Most buckets the native map holds; see [`evict_for_new_key`].
    #[cfg(not(target_arch = "wasm32"))]
    capacity: usize,
}

/// Default [`UserRateLimiter`] capacity on native.
#[cfg(not(target_arch = "wasm32"))]
const MAX_BUCKETS: usize = 50_000;

#[cfg(not(target_arch = "wasm32"))]
struct RateBucket {
    count: u32,
    window_start: Instant,
    /// The limit this bucket was last charged under. Categories differ in
    /// window and budget, so expiry and "throttled" are per bucket, never
    /// judged by whichever category's request happens to trigger eviction.
    limit: RateLimit,
}

#[cfg(not(target_arch = "wasm32"))]
impl RateBucket {
    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.window_start) > self.limit.window
    }

    fn throttled(&self) -> bool {
        self.count >= self.limit.max_requests
    }
}

/// Make room for one new key in a map at `capacity`.
///
/// Expired buckets go first: they hold nothing. If that frees nothing, the
/// map drops live buckets down to 90% of `capacity`, cheapest first: buckets
/// still under their budget before throttled ones, lower counts before
/// higher, older windows before newer. So a flood of fresh keys (one per /64
/// of a /48 costs one request each) evicts its own one-request buckets, and a
/// client that is being throttled keeps its counter; resetting every bucket
/// at once would hand each throttled client a new budget.
#[cfg(not(target_arch = "wasm32"))]
fn evict_for_new_key(buckets: &mut HashMap<String, RateBucket>, capacity: usize, now: Instant) {
    buckets.retain(|_, b| !b.expired(now));
    if buckets.len() < capacity {
        return;
    }
    let target = capacity - capacity / 10;
    let excess = buckets.len() + 1 - target;
    let mut victims: Vec<(bool, u32, Instant, String)> = buckets
        .iter()
        .map(|(key, b)| (b.throttled(), b.count, b.window_start, key.clone()))
        .collect();
    victims.sort_unstable();
    for (_, _, _, key) in victims.into_iter().take(excess) {
        buckets.remove(&key);
    }
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
    /// Transactional email one requester can cause to be SENT: 10 per hour
    /// per IP. Distinct from [`Self::AUTH`], which bounds requests to the
    /// auth routes; this bounds the outbound mail those requests spend.
    ///
    /// Charged at the send site (`auth_ui::api::send_template_email`), not
    /// per route, so a caller who mistypes an address or asks about an
    /// unregistered one — neither of which sends anything — keeps their
    /// budget. Without it, one requester could still empty the email block's
    /// deployment-wide ceiling simply by naming a new address each time: the
    /// per-recipient bucket caps one address's share of that ceiling, not one
    /// requester's.
    pub const AUTH_EMAIL: Self = Self {
        max_requests: 10,
        window: Duration::from_secs(3600),
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

    /// The config key that overrides the limit for category `name`:
    /// `WAFER_RUN_SHARED__RATE_LIMIT_{NAME}`.
    pub fn override_key(name: &str) -> String {
        format!("WAFER_RUN_SHARED__RATE_LIMIT_{}", name.to_uppercase())
    }

    /// Read config override for this rate limit category.
    ///
    /// Looks up [`Self::override_key`] in config. Format: `requests/seconds` (e.g. `50/60`).
    /// Set to `0` to disable rate limiting for this category.
    /// Returns `None` if disabled, otherwise the resolved limit. A failed
    /// read is returned: a limit nobody could read is neither the default nor
    /// off.
    pub async fn resolve(self, ctx: &dyn Context, name: &str) -> Result<Option<Self>, WaferError> {
        let key = Self::override_key(name);
        let default = format!("{}/{}", self.max_requests, self.window.as_secs());
        let value = config::get_default(ctx, &key, &default).await?;
        Ok(self.parse_override(&value))
    }

    /// [`Self::resolve`] once the override is read.
    fn parse_override(self, value: &str) -> Option<Self> {
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
            #[cfg(not(target_arch = "wasm32"))]
            capacity: MAX_BUCKETS,
        }
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn with_capacity(capacity: usize) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity,
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

        if !buckets.contains_key(key) && buckets.len() >= self.capacity {
            evict_for_new_key(&mut buckets, self.capacity, now);
        }

        let bucket = buckets.entry(key.to_string()).or_insert(RateBucket {
            count: 0,
            window_start: now,
            limit,
        });
        bucket.limit = limit;

        // Reset window if expired
        if bucket.expired(now) {
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
        // std::time::SystemTime::now() panics on wasm32-unknown-unknown
        // (no system clock). Use js_sys::Date::now() which returns ms since epoch.
        let now = (js_sys::Date::now() / 1000.0) as i64;
        let window_secs = limit.window.as_secs() as i64;
        let window_cutoff = now - window_secs;

        let id = crate::util::sha256_hex(format!("rl:{key}:{now}").as_bytes());
        let count = crate::blocks::auth::repo::rate_limits::windowed_increment(
            ctx,
            &id,
            key,
            now,
            window_cutoff,
        )
        .await;

        match decide_rate_limit(count, key, limit.max_requests, window_secs as u64) {
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
    /// The counter's upsert against the D1 backend failed.
    /// Availability is preserved — the request is still allowed — but this
    /// is a distinct, logged decision, never an unlabeled `count = 0` allow.
    /// Regression target for the 2026-07-10 incident where a missing
    /// `rate_limits` table left CF rate limiting silently inert for weeks
    /// with zero log evidence.
    FailedOpen { reason: String },
}

/// Decide the outcome of a D1-backed fixed-window rate-limit check from the
/// counter `auth::repo::rate_limits::windowed_increment` reported, without
/// touching the backend itself.
///
/// A failure of that call — the upsert, or an answer without the counter
/// row — fails open for availability, but loudly: it emits a
/// `tracing::warn!` and returns [`BackendCheckOutcome::FailedOpen`] instead
/// of silently deriving `count = 0` from an absent row.
pub fn decide_rate_limit(
    count: Result<i64, WaferError>,
    key: &str,
    max_requests: u32,
    retry_after_secs: u64,
) -> BackendCheckOutcome {
    let count = match count {
        Ok(count) => count as u32,
        Err(e) => {
            tracing::warn!(
                error = %e,
                key = %key,
                "rate-limit backend failed — failing open (allowing request, count unknown)"
            );
            return BackendCheckOutcome::FailedOpen {
                reason: e.to_string(),
            };
        }
    };

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
///
/// The one refusal in this repo that hand-builds its `WaferError`, because it
/// has to attach `Retry-After` as response meta. That is why it carried the
/// `"[rate_limit_exceeded] "` message prefix long after `errors.rs`'s doc
/// comment declared the prefix gone: without a detail code, the prefix was
/// this response's only machine-readable identity, and the SDK reads
/// `body.code` (`packages/impresspress-js/src/http-client.ts`). It now sets
/// the detail code the same way [`super::errors::error_response`] does, so
/// the message is human-only and the code travels as `error.code` meta.
pub fn rate_limited_response(retry_after: u64) -> OutputStream {
    use super::errors::ErrorCode;
    let code = ErrorCode::RateLimitExceeded;
    let mut error = wafer_run::WaferError::new(
        super::errors::impresspress_error_code_to_wafer(code),
        code.default_message(),
    )
    .with_detail_code(code.as_str());
    error.meta.push(wafer_run::MetaEntry {
        key: "resp.header.Retry-After".to_string(),
        value: retry_after.to_string(),
    });
    OutputStream::error(error)
}

/// Outcome of a rate-limit check.
pub enum RateLimitOutcome {
    /// Allowed — caller should attach these headers to the success response.
    Allowed(RateLimitHeaders),
    /// Disabled — no rate limiting applied for this category.
    Disabled,
    /// Rate-limited, or the limit could not be read — caller should return
    /// this `OutputStream` immediately.
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
    let limit = match default.resolve(ctx, category).await {
        Ok(Some(limit)) => limit,
        Ok(None) => return RateLimitOutcome::Disabled,
        Err(e) => {
            return RateLimitOutcome::Limited(super::crud::db_error_internal(
                e,
                "Could not read the rate limit",
            ))
        }
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

/// Check the per-user read/write rate limit using the request's user_id.
///
/// Determines the category from the message action: `retrieve` spends
/// `api_read`, everything else `api_write`, unless `create_override` names
/// another bucket for the `create` action (`Some((RateLimit::UPLOAD,
/// "upload"))` makes uploads count against their own bucket). Returns
/// `RateLimitOutcome::Disabled` for unauthenticated requests (empty user_id).
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

/// The bucket identity a request with no client IP falls back to.
///
/// Every such request shares this one bucket — fail-closed, so a platform
/// that stops populating `remote_addr` cannot turn an IP-keyed limit off.
/// The cost is that the limit then applies to the whole deployment at once,
/// which is why a caller whose refusal was charged against this identity
/// should say so rather than report an ordinary per-IP refusal (see
/// `auth_ui::api::send_template_email`).
pub const UNKNOWN_IP: &str = "unknown";

/// The identity an IP-keyed rate-limit bucket uses for a request: the client
/// network [`ip_bucket`] derives from the remote address.
pub fn ip_identity(msg: &wafer_run::Message) -> String {
    ip_bucket(msg.remote_addr())
}

/// The prefix length one IPv6 client is charged under.
///
/// A /64 is the smallest network an ISP assigns one subscriber, and a host
/// picks any of its 2^64 interface ids itself (SLAAC privacy addresses rotate
/// them routinely), so a bucket per /128 is a bucket per request to anyone
/// who chooses so.
const IPV6_CLIENT_PREFIX: u32 = 64;

/// The client network a remote address is rate-limited as, in the one
/// spelling every IP-keyed bucket uses: an IPv4 address as itself, an IPv6
/// address as its /64 (`2001:db8:1:2::/64`), and an IPv4-mapped IPv6 address
/// (`::ffff:a.b.c.d`, what a dual-stack socket reports for an IPv4 peer) as
/// the IPv4 address it carries. A `host:port` form is accepted and the port
/// dropped. An empty or unparseable address is [`UNKNOWN_IP`].
pub fn ip_bucket(remote_addr: &str) -> String {
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};

    let value = remote_addr.trim();
    let Some(ip) = value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| value.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
    else {
        return UNKNOWN_IP.to_string();
    };
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let mask = u128::MAX << (128 - IPV6_CLIENT_PREFIX);
                let network = Ipv6Addr::from(u128::from(v6) & mask);
                format!("{network}/{IPV6_CLIENT_PREFIX}")
            }
        },
    }
}

/// Whether a route's rate-limit bucket is keyed by client IP or by user id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKey {
    /// Key the bucket by [`ip_identity`] — for unauthenticated endpoints.
    Ip,
    /// Key the bucket by `msg.user_id()` — for authenticated endpoints. A
    /// request with an empty user_id is skipped (no limit applied).
    User,
}

/// Spend the `(key, category, limit)` bucket a block's route table assigned
/// to this request. `Some(response)` is the 429 to return; `None` means
/// proceed: the bucket is disabled by config, a user-keyed bucket has no user
/// to charge, or the request is under the limit.
///
/// `RateLimitOutcome::Allowed(headers)` is discarded for every caller:
/// injecting `X-RateLimit-*` response headers needs a streaming-middleware
/// shape we don't have yet. Tracked as a single follow-up, not a per-route
/// TODO.
pub async fn apply_route_limit(
    limiter: &UserRateLimiter,
    ctx: &dyn wafer_run::context::Context,
    msg: &wafer_run::Message,
    key: LimitKey,
    category: &str,
    limit: RateLimit,
) -> Option<OutputStream> {
    let identity = match key {
        LimitKey::Ip => ip_identity(msg),
        LimitKey::User => {
            let user_id = msg.user_id();
            if user_id.is_empty() {
                return None;
            }
            user_id.to_string()
        }
    };
    match check_rate_limit(limiter, ctx, &identity, category, limit).await {
        RateLimitOutcome::Limited(response) => Some(response),
        RateLimitOutcome::Allowed(_) | RateLimitOutcome::Disabled => None,
    }
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
            block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            // No override is configured: the config block answers an unset
            // key with `NotFound`, which the limit reads as its default.
            if block_name == "wafer-run/config" {
                return OutputStream::error(WaferError::new(
                    wafer_run::ErrorCode::NotFound,
                    "config key not set",
                ));
            }
            OutputStream::respond(vec![])
        }
        /// Admits nothing, as the fail-closed `check_resource_access` default
        /// this context keeps does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_run::ResourceType,
            _access: wafer_block::ResourceAccess,
        ) -> bool {
            false
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

    /// Filling the map with fresh keys (a /48 holder has 65,536 /64s) must
    /// not reset a client that is being throttled, and the map stays bounded.
    #[tokio::test]
    async fn filling_the_map_keeps_a_throttled_bucket() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::with_capacity(10);
        let limit = RateLimit {
            max_requests: 2,
            window: Duration::from_secs(60),
        };
        for _ in 0..2 {
            assert!(limiter.check(&ctx, "victim:auth", limit).await.is_ok());
        }
        assert!(limiter.check(&ctx, "victim:auth", limit).await.is_err());

        for i in 0..100 {
            let key = format!("2001:db8:0:{i:x}::/64:auth");
            assert!(limiter.check(&ctx, &key, limit).await.is_ok());
        }
        assert!(
            limiter.check(&ctx, "victim:auth", limit).await.is_err(),
            "a flood of new keys reset a throttled client's counter"
        );
        let len = limiter
            .buckets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        assert!(len <= 10, "the map grew past its capacity: {len}");
    }

    /// An expired bucket is evicted before any live one, whatever category
    /// triggered the eviction: here a short-window request makes room while
    /// a long-window throttled bucket keeps its count.
    #[tokio::test]
    async fn eviction_judges_expiry_by_each_buckets_own_window() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::with_capacity(2);
        let hourly = RateLimit {
            max_requests: 1,
            window: Duration::from_secs(3600),
        };
        let brief = RateLimit {
            max_requests: 5,
            window: Duration::from_millis(50),
        };
        assert!(limiter
            .check(&ctx, "mailer:auth_email", hourly)
            .await
            .is_ok());
        assert!(limiter
            .check(&ctx, "mailer:auth_email", hourly)
            .await
            .is_err());
        assert!(limiter.check(&ctx, "old:signal", brief).await.is_ok());
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(limiter.check(&ctx, "new:signal", brief).await.is_ok());
        {
            let buckets = limiter.buckets.lock().unwrap_or_else(|e| e.into_inner());
            assert!(!buckets.contains_key("old:signal"));
        }
        assert!(limiter
            .check(&ctx, "mailer:auth_email", hourly)
            .await
            .is_err());
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
        assert_eq!(ip_identity(&msg_with("create", "", "not-an-ip")), "unknown");
    }

    #[test]
    fn ip_bucket_spells_each_client_network_once() {
        // One /64, any interface id, any spelling: one bucket.
        assert_eq!(ip_bucket("2001:db8:1:2::1"), "2001:db8:1:2::/64");
        assert_eq!(
            ip_bucket("2001:0db8:0001:0002:ffff:ffff:ffff:ffff"),
            "2001:db8:1:2::/64"
        );
        assert_eq!(ip_bucket("[2001:db8:1:2::9]:443"), "2001:db8:1:2::/64");
        // The neighbouring /64 is another subscriber.
        assert_eq!(ip_bucket("2001:db8:1:3::1"), "2001:db8:1:3::/64");
        // IPv4, bare or with a port, and IPv4-mapped IPv6 are the IPv4 /32.
        assert_eq!(ip_bucket("203.0.113.9"), "203.0.113.9");
        assert_eq!(ip_bucket("203.0.113.9:8080"), "203.0.113.9");
        assert_eq!(ip_bucket("::ffff:203.0.113.9"), "203.0.113.9");
        assert_eq!(ip_bucket("[::ffff:203.0.113.9]:8080"), "203.0.113.9");
        assert_eq!(ip_bucket(" "), UNKNOWN_IP);
    }

    /// Drives the real route limiter: a client rotating interface ids
    /// within its /64 spends one budget, and a client in the next /64 has
    /// its own.
    #[tokio::test]
    async fn apply_route_limit_charges_an_ipv6_client_per_64() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        for suffix in ["1", "2"] {
            let msg = msg_with("create", "", &format!("2001:db8:1:2::{suffix}"));
            assert!(
                apply_route_limit(&limiter, &ctx, &msg, LimitKey::Ip, "auth", TWO_PER_MINUTE)
                    .await
                    .is_none()
            );
        }
        let rotated = msg_with("create", "", "2001:db8:1:2:dead:beef:0:3");
        assert!(
            apply_route_limit(
                &limiter,
                &ctx,
                &rotated,
                LimitKey::Ip,
                "auth",
                TWO_PER_MINUTE
            )
            .await
            .is_some(),
            "a third address in the same /64 must hit the /64's limit"
        );
        let neighbour = msg_with("create", "", "2001:db8:1:3::1");
        assert!(
            apply_route_limit(
                &limiter,
                &ctx,
                &neighbour,
                LimitKey::Ip,
                "auth",
                TWO_PER_MINUTE
            )
            .await
            .is_none(),
            "a different /64 has its own bucket"
        );
    }

    /// A dual-stack listener reports an IPv4 peer as `::ffff:a.b.c.d`; that
    /// peer must spend the same budget as when it arrives as plain IPv4.
    #[tokio::test]
    async fn apply_route_limit_charges_ipv4_mapped_as_ipv4() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        for remote in ["198.51.100.7", "::ffff:198.51.100.7"] {
            let msg = msg_with("create", "", remote);
            assert!(
                apply_route_limit(&limiter, &ctx, &msg, LimitKey::Ip, "auth", TWO_PER_MINUTE)
                    .await
                    .is_none()
            );
        }
        let again = msg_with("create", "", "198.51.100.7");
        assert!(
            apply_route_limit(&limiter, &ctx, &again, LimitKey::Ip, "auth", TWO_PER_MINUTE)
                .await
                .is_some()
        );
    }

    const TWO_PER_MINUTE: RateLimit = RateLimit {
        max_requests: 2,
        window: Duration::from_secs(60),
    };

    #[tokio::test]
    async fn apply_route_limit_limits_an_ip_bucket() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        let msg = msg_with("create", "", "9.9.9.9");
        // First two proceed, the third is the 429.
        for _ in 0..2 {
            assert!(
                apply_route_limit(&limiter, &ctx, &msg, LimitKey::Ip, "auth", TWO_PER_MINUTE)
                    .await
                    .is_none()
            );
        }
        assert!(
            apply_route_limit(&limiter, &ctx, &msg, LimitKey::Ip, "auth", TWO_PER_MINUTE)
                .await
                .is_some()
        );
    }

    /// A user-keyed bucket has nothing to charge for an anonymous caller, so
    /// the request proceeds and spends nothing; a caller with a user is
    /// charged.
    #[tokio::test]
    async fn apply_route_limit_skips_a_user_bucket_for_an_anonymous_caller() {
        let ctx = TestCtx;
        let limiter = UserRateLimiter::new();
        let anon = msg_with("update", "", "");
        for _ in 0..3 {
            assert!(apply_route_limit(
                &limiter,
                &ctx,
                &anon,
                LimitKey::User,
                "auth_write",
                TWO_PER_MINUTE
            )
            .await
            .is_none());
        }
        let user = msg_with("update", "u1", "");
        for _ in 0..2 {
            assert!(apply_route_limit(
                &limiter,
                &ctx,
                &user,
                LimitKey::User,
                "auth_write",
                TWO_PER_MINUTE
            )
            .await
            .is_none());
        }
        assert!(apply_route_limit(
            &limiter,
            &ctx,
            &user,
            LimitKey::User,
            "auth_write",
            TWO_PER_MINUTE
        )
        .await
        .is_some());
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

    #[test]
    fn rate_limit_decision_is_explicit_when_the_backend_fails() {
        // Regression for the CF incident where a missing `rate_limits` table
        // left limiting silently inert for weeks. A backend failure — the
        // upsert, or an answer without the counter row — must be a logged,
        // explicit fail-open, not an unlabeled count=0 allow.
        let upsert = decide_rate_limit(
            Err(wafer_error("rate_limits windowed upsert: D1 down")),
            "k",
            5,
            60,
        );
        assert!(matches!(upsert, BackendCheckOutcome::FailedOpen { .. }));
        let no_row = decide_rate_limit(
            Err(wafer_error(
                "rate_limits windowed upsert answered no counter row: None",
            )),
            "k",
            5,
            60,
        );
        assert!(matches!(no_row, BackendCheckOutcome::FailedOpen { .. }));
    }

    #[test]
    fn rate_limit_decision_allows_under_limit() {
        let outcome = decide_rate_limit(Ok(2), "k", 5, 60);
        assert_eq!(outcome, BackendCheckOutcome::Allowed(3));
    }

    #[test]
    fn rate_limit_decision_limits_over_limit() {
        let outcome = decide_rate_limit(Ok(6), "k", 5, 60);
        assert_eq!(outcome, BackendCheckOutcome::Limited(60));
    }

    #[test]
    fn rate_limit_counts_when_upsert_succeeds() {
        // Regression for the adapter fail-open bug: before the D1 /
        // KV-cached-D1 / browser `DatabaseService::upsert` forwarders existed
        // (added alongside this test), every wasm rate-limit check's upsert
        // was `Err("... not implemented by this database backend")`, so
        // `decide_rate_limit` always took the `FailedOpen` branch below — the
        // limiter silently allowed every request at full quota, on every
        // backend, forever. Method *presence* is now
        // compile-enforced (`upsert`/`aggregate` are required `DatabaseService`
        // trait methods; the adapters would not build without the forwarders),
        // so a real deployment's counter read is `Ok(_)`. This test locks in
        // that once the counter actually lands, the decision is a real
        // count-based `Allowed`/`Limited` — never the fail-open branch.

        // Under the limit: counts, allows-by-remaining (not fail-open).
        let under = decide_rate_limit(Ok(2), "k", 5, 60);
        assert!(!matches!(under, BackendCheckOutcome::FailedOpen { .. }));
        assert_eq!(under, BackendCheckOutcome::Allowed(3));

        // Over the limit: counts, denies (not fail-open).
        let over = decide_rate_limit(Ok(6), "k", 5, 60);
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

#[cfg(test)]
mod rate_limited_response_tests {
    use super::*;

    /// The `"[code] message"` prefix `errors.rs`'s doc comment calls gone
    /// survived here, because this response hand-builds its `WaferError` to
    /// attach `Retry-After` and so carried no `error.code` detail meta — the
    /// prefix was its only machine-readable code. It now carries the detail
    /// code every other refusal in this repo carries, and the message is
    /// human-only.
    #[tokio::test]
    async fn the_429_carries_a_detail_code_and_no_bracket_prefix() {
        let out = rate_limited_response(42);
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(e)) => {
                assert_eq!(
                    e.detail_code(),
                    Some(super::super::errors::ErrorCode::RateLimitExceeded.as_str())
                );
                assert!(
                    !e.message.starts_with('['),
                    "message must not carry the old bracket-code prefix, got {:?}",
                    e.message
                );
                assert_eq!(wafer_block::http_codec::resolve_error_status(&e), 429);
            }
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_429_still_carries_retry_after() {
        let headers = wafer_block::http_codec::collect_http_response(rate_limited_response(42))
            .await
            .headers;
        assert!(
            headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("Retry-After") && v == "42"),
            "Retry-After must survive, got {headers:?}"
        );
    }
}
