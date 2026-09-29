//! Privacy-preserving submission identities, form tokens and fail-closed limits.

use std::time::Duration;

use wafer_block_crypto::primitives;
use wafer_run::context::Context;

use crate::blocks::rate_limit::{ip_bucket, RateLimit, UserRateLimiter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbuseDecision {
    Allowed { remaining: u32 },
    Limited { retry_after: u64 },
    Unavailable,
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn check_durable(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    key: &str,
    limit: RateLimit,
) -> AbuseDecision {
    match limiter.check(ctx, key, limit).await {
        Ok(remaining) => AbuseDecision::Allowed { remaining },
        Err(retry_after) => AbuseDecision::Limited { retry_after },
    }
}

#[cfg(target_arch = "wasm32")]
pub async fn check_durable(
    _limiter: &UserRateLimiter,
    ctx: &dyn Context,
    key: &str,
    limit: RateLimit,
) -> AbuseDecision {
    let now = (js_sys::Date::now() / 1000.0) as i64;
    let window_secs = limit.window.as_secs() as i64;
    let window_cutoff = now - window_secs;
    let id = crate::util::sha256_hex(format!("tickets-rl:{key}:{now}").as_bytes());
    let count = crate::blocks::auth::repo::rate_limits::windowed_increment(
        ctx,
        &id,
        key,
        now,
        window_cutoff,
    )
    .await;
    match crate::blocks::rate_limit::decide_rate_limit(
        count,
        "<redacted-ticket-identity>",
        limit.max_requests,
        window_secs as u64,
    ) {
        crate::blocks::rate_limit::BackendCheckOutcome::Allowed(remaining) => {
            AbuseDecision::Allowed { remaining }
        }
        crate::blocks::rate_limit::BackendCheckOutcome::Limited(retry_after) => {
            AbuseDecision::Limited { retry_after }
        }
        crate::blocks::rate_limit::BackendCheckOutcome::FailedOpen { .. } => {
            AbuseDecision::Unavailable
        }
    }
}

pub fn limit(max_requests: u32, window_secs: u64) -> Option<RateLimit> {
    (max_requests > 0 && window_secs > 0).then_some(RateLimit {
        max_requests,
        window: Duration::from_secs(window_secs),
    })
}

pub fn rotating_identity(secret: &str, now_secs: u64, remote_addr: &str) -> String {
    let day = now_secs / 86_400;
    let client = ip_bucket(remote_addr);
    hmac_hex(secret.as_bytes(), format!("{day}:{client}").as_bytes())
}

pub fn dedupe_hash(
    secret: &str,
    rotating_identity: &str,
    type_id: &str,
    subject: &str,
    description: &str,
    source_path: &str,
) -> String {
    let normalized = format!(
        "{}\n{}\n{}\n{}\n{}",
        rotating_identity,
        type_id.trim(),
        normalize_text(subject),
        normalize_text(description),
        source_path.trim(),
    );
    hmac_hex(secret.as_bytes(), normalized.as_bytes())
}

pub fn issue_form_token(secret: &str, now_secs: u64) -> String {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let payload = format!("v1.{now_secs}.{nonce}");
    let signature = hmac_hex(secret.as_bytes(), payload.as_bytes());
    format!("{payload}.{signature}")
}

pub fn verify_form_token(
    secret: &str,
    token: &str,
    now_secs: u64,
    ttl_secs: u64,
) -> Result<(), &'static str> {
    if token.len() > 512 {
        return Err("invalid form token");
    }
    let mut parts = token.split('.');
    let version = parts.next().unwrap_or("");
    let issued = parts.next().unwrap_or("");
    let nonce = parts.next().unwrap_or("");
    let signature = parts.next().unwrap_or("");
    if version != "v1"
        || issued.is_empty()
        || nonce.len() != 32
        || signature.len() != 64
        || parts.next().is_some()
    {
        return Err("invalid form token");
    }
    let issued = issued.parse::<u64>().map_err(|_| "invalid form token")?;
    if issued > now_secs.saturating_add(300) || now_secs.saturating_sub(issued) > ttl_secs {
        return Err("expired form token");
    }
    let payload = format!("v1.{issued}.{nonce}");
    let expected = hmac_hex(secret.as_bytes(), payload.as_bytes());
    if !primitives::constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err("invalid form token");
    }
    Ok(())
}

pub fn now_secs() -> u64 {
    crate::util::now_millis() / 1_000
}

pub(super) fn canonical_ip(value: &str) -> String {
    let value = value.trim();
    value
        .parse::<std::net::IpAddr>()
        .or_else(|_| value.parse::<std::net::SocketAddr>().map(|addr| addr.ip()))
        .map_or_else(|_| "unknown".to_string(), |ip| ip.to_string())
}

fn normalize_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Lowercase-hex HMAC-SHA256 of `message` under `secret`.
fn hmac_hex(secret: &[u8], message: &[u8]) -> String {
    crate::util::hex_encode(&primitives::hmac_sha256(secret, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotating_identity_is_stable_per_day_and_never_contains_ip() {
        let first = rotating_identity("secret", 86_400, "203.0.113.9");
        let same = rotating_identity("secret", 172_799, "203.0.113.9");
        let next = rotating_identity("secret", 172_800, "203.0.113.9");
        assert_eq!(first, same);
        assert_ne!(first, next);
        assert!(!first.contains("203.0.113.9"));
        assert_eq!(canonical_ip("203.0.113.9:443"), "203.0.113.9");
        assert_eq!(canonical_ip("[2001:db8::1]:443"), "2001:db8::1");
    }

    /// The per-identity submission limit is keyed by this identity, so it
    /// must name the client network: one IPv6 client rotating interface ids
    /// within its /64 is one identity, the next /64 is another.
    #[test]
    fn rotating_identity_names_the_ipv6_64() {
        let one = rotating_identity("secret", 86_400, "2001:db8:1:2::1");
        let rotated = rotating_identity("secret", 86_400, "[2001:db8:1:2:dead:beef:0:3]:443");
        let neighbour = rotating_identity("secret", 86_400, "2001:db8:1:3::1");
        assert_eq!(one, rotated);
        assert_ne!(one, neighbour);
        assert_eq!(
            rotating_identity("secret", 86_400, "::ffff:203.0.113.9"),
            rotating_identity("secret", 86_400, "203.0.113.9")
        );
    }

    #[test]
    fn form_token_round_trip_expiry_and_tamper() {
        let token = issue_form_token("secret", 1_000);
        assert!(verify_form_token("secret", &token, 1_100, 300).is_ok());
        assert!(verify_form_token("secret", &token, 1_301, 300).is_err());
        assert!(verify_form_token("other", &token, 1_100, 300).is_err());
    }

    #[test]
    fn dedupe_normalizes_whitespace_and_case() {
        let a = dedupe_hash(
            "secret",
            "identity",
            "type",
            " Hello ",
            "A  REPORT",
            "/page",
        );
        let b = dedupe_hash("secret", "identity", "type", "hello", "a report", "/page");
        assert_eq!(a, b);
    }

    /// RFC 2104 HMAC-SHA256 written out over `sha2`. Stored dedupe hashes and
    /// outstanding form tokens were computed with this construction, so
    /// `hmac_hex` must reproduce it byte for byte.
    fn reference_hmac_hex(secret: &[u8], message: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        const BLOCK: usize = 64;
        let mut key = [0_u8; BLOCK];
        if secret.len() > BLOCK {
            key[..32].copy_from_slice(&Sha256::digest(secret));
        } else {
            key[..secret.len()].copy_from_slice(secret);
        }
        let mut inner_pad = [0x36_u8; BLOCK];
        let mut outer_pad = [0x5c_u8; BLOCK];
        for index in 0..BLOCK {
            inner_pad[index] ^= key[index];
            outer_pad[index] ^= key[index];
        }
        let mut inner = Sha256::new();
        inner.update(inner_pad);
        inner.update(message);
        let inner = inner.finalize();
        let mut outer = Sha256::new();
        outer.update(outer_pad);
        outer.update(inner);
        crate::util::hex_encode(&outer.finalize())
    }

    /// Byte-identical to the reference over random keys and messages, with
    /// key lengths straddling the 64-byte block (padded, exact, pre-hashed).
    #[test]
    fn hmac_matches_the_reference_construction_on_random_inputs() {
        let mut bytes = [0_u8; 512];
        for round in 0..2_000_usize {
            getrandom::getrandom(&mut bytes).expect("os rng");
            let key_len = match round % 4 {
                0 => usize::from(bytes[0]) % 64,
                1 => 64,
                2 => 65 + usize::from(bytes[0]),
                _ => usize::from(bytes[0]) + usize::from(bytes[1]),
            };
            let message_len = usize::from(bytes[2]) + usize::from(bytes[3]);
            let key = &bytes[..key_len];
            let message = &bytes[512 - message_len..];
            assert_eq!(
                hmac_hex(key, message),
                reference_hmac_hex(key, message),
                "key {key:02x?} message {message:02x?}"
            );
        }
    }

    #[test]
    fn hmac_matches_standard_vector() {
        assert_eq!(
            hmac_hex(b"key", b"The quick brown fox jumps over the lazy dog"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }
}
