//! Row-level data access for the `wafer-run/auth` block.
//!
//! Each submodule exposes pure-function async helpers that take
//! `&dyn wafer_run::context::Context` and operate on a single table defined
//! by migration 001. Errors collapse into the [`RepoError`] enum so higher
//! layers don't have to reason about the underlying `db` client's error type.
//!
//! The small row-decoding utilities every submodule needs — the ISO-8601
//! timestamp writer ([`now_iso`]), hex decoding ([`decode_hex`]), and the
//! `&HashMap<String, Value>` map accessors ([`map_str`]/[`map_opt_str`]/
//! [`map_bool`]) — live here so all auth tables share one implementation. In
//! particular [`now_iso`] is **the** timestamp writer for auth-table rows:
//! keeping a single `…Z` formatter stops the documented `Z`/`+00:00`
//! intermixing (see `service::is_expired`) from growing.

use std::collections::HashMap;

use serde_json::Value;

pub mod api_keys;
pub mod bootstrap_tokens;
pub mod jwt_blocklist;
pub mod local_credentials;
pub mod oauth_pkce;
pub mod orgs;
pub mod pats;
pub mod provider_links;
pub mod rate_limits;
pub mod sessions;
pub mod tokens;
pub mod users;

/// Errors surfaced by the auth repo layer.
#[derive(thiserror::Error, Debug)]
pub enum RepoError {
    /// The requested row was not visible after an insert/update — treat as a
    /// programmer/db-consistency error.
    #[error("not found")]
    NotFound,
    /// Low-level database error (client error, bad row shape, JSON decode, …).
    #[error("db: {0}")]
    Db(String),
}

/// Current UTC time as an ISO-8601 string with a literal `Z` suffix
/// (`%Y-%m-%dT%H:%M:%SZ`).
///
/// This is the single timestamp writer for every auth table. Using one
/// formatter everywhere keeps stored timestamps in one format so the
/// string-comparison cleanup queries (e.g. `sessions::delete_expired`'s
/// `expires_at < cutoff`) stay correct, and stops the historical
/// `Z`-vs-`+00:00` intermixing documented in `service::is_expired`.
pub(crate) fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Decode a lowercase hex string into raw bytes. Returns `None` for an
/// odd-length or non-hex input. Used by the token-hash columns
/// (`sessions`, `pats`) which persist `hex_encode(sha256(raw))`.
pub(crate) fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Map accessor: owned `String` for a TEXT column, or `None` when the key is
/// absent / not a JSON string. Mirrors `RecordExt::str_field`'s "absent → empty"
/// intent but preserves the `Option` so callers can distinguish missing.
pub(crate) fn map_opt_str(m: &HashMap<String, Value>, key: &str) -> Option<String> {
    m.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Map accessor: owned `String` for a TEXT column, defaulting to empty.
pub(crate) fn map_str(m: &HashMap<String, Value>, key: &str) -> String {
    map_opt_str(m, key).unwrap_or_default()
}

/// Map accessor: bool for a column, tolerant of the shapes the different
/// backends return (JSON bool, SQLite TEXT-int `0`/`1`, Postgres BOOLEAN,
/// string `'true'`/`'false'`). Mirrors `RecordExt::bool_field`.
pub(crate) fn map_bool(m: &HashMap<String, Value>, key: &str) -> bool {
    match m.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}
