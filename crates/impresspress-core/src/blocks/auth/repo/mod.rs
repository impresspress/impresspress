//! Row-level data access for the `wafer-run/auth` block.
//!
//! Each submodule exposes pure-function async helpers that take
//! `&dyn wafer_run::context::Context` and operate on a single table defined
//! by migration 001. Errors are the database client's own
//! [`wafer_run::WaferError`], carried through unchanged apart from a label:
//! [`db_failed`] prefixes the statement that failed, and [`internal_error`]
//! is this layer's own fault (a row whose columns are not the shape its type
//! describes).
//!
//! There used to be a `RepoError` here — `NotFound | Db(String)` — and the
//! `Db(String)` arm threw the [`wafer_run::ErrorCode`] away. That is the one
//! thing a caller needs: a WRAP grant refusal is `PermissionDenied` and must
//! reach the client as a 403, a missing row is `NotFound` and must reach it
//! as a 404, and everything else is a 500. Collapsed into a string, all
//! three arrived at `admin::users::get_user` as
//! `500 Internal server error (ref: …)`, so an operator running a
//! deployment whose admin block was missing its `wafer_run__auth__users`
//! grant could not tell the missing grant from an outage. Keeping the code
//! is what lets those call sites answer through `crud::db_error`.
//!
//! The small row-decoding utilities every submodule needs — the ISO-8601
//! timestamp writer ([`now_iso`]/[`iso`]) and reader ([`parse_iso`]), hex
//! decoding ([`decode_hex`]), and the `&HashMap<String, Value>` map accessors
//! ([`map_str`]/[`map_opt_str`]/[`map_bool`]) — live here so all auth tables
//! share one implementation. In particular [`iso`] is **the** timestamp
//! writer for auth-table rows, including for a timestamp a caller supplied:
//! keeping a single `…Z` formatter stops the documented `Z`/`+00:00`
//! intermixing (see `service::is_expired`) from growing. A stored timestamp
//! is read back with [`parse_iso`] rather than compared as text — string
//! order is time order only within one format and one offset.

use std::collections::HashMap;

use serde_json::Value;
use wafer_run::{ErrorCode, WaferError};

pub mod api_keys;
pub mod bootstrap_tokens;
pub mod jwt_blocklist;
pub mod local_credentials;
pub mod maintenance;
pub mod oauth_pkce;
pub mod orgs;
pub mod pats;
pub mod provider_links;
pub mod rate_limits;
pub mod sessions;
pub mod tokens;
pub mod users;

/// Label a failed database call with the statement that failed, keeping the
/// [`wafer_run::ErrorCode`] the client classified it with.
///
/// `what` is the same short label the deleted `RepoError::Db(format!("{what}:
/// {e}"))` carried, and it stays in the message because it is the only thing
/// naming which statement of a multi-step repo function gave up. The code is
/// what that spelling destroyed: `crud::db_error` needs it to answer 404 for
/// a missing row, **403 for a WRAP refusal** and 500 for anything else, and
/// a `String` makes all three identical.
pub(crate) fn db_failed(what: &str, error: WaferError) -> WaferError {
    WaferError {
        code: error.code,
        message: format!("{what}: {}", error.message),
        meta: error.meta,
    }
}

/// A fault this layer found in a successful read: a row whose columns are
/// not the shape the module's row type describes.
///
/// Always [`ErrorCode::Internal`], never the caller's fault — the row came
/// back, the table is simply not the one migration 001 describes, which is a
/// deployment or migration fault. Separate from [`db_failed`] because there
/// is no client error to carry a code from.
pub(crate) fn internal_error(what: impl Into<String>) -> WaferError {
    WaferError::new(ErrorCode::Internal, what)
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
    iso(chrono::Utc::now())
}

/// Format an instant in the one shape [`now_iso`] writes
/// (`%Y-%m-%dT%H:%M:%SZ`, seconds resolution, UTC).
///
/// Anything an auth table stores in a timestamp column goes through here or
/// through [`now_iso`], including a value a caller supplied: a column holding
/// two spellings of the same instant cannot be ordered by a SQL comparison,
/// and the API-key expiry column used to hold whatever string the caller
/// sent.
pub(crate) fn iso(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Read a timestamp column back as an instant, or `None` when the stored
/// text is not RFC 3339.
///
/// Accepts any RFC 3339 offset, not only the `Z` [`iso`] writes, because rows
/// written before this was the only writer carry whatever the caller sent —
/// `…T20:00:00+09:00` names 11:00 UTC and must be read as that instant, not
/// compared as the string `20:00`. Callers decide what an unreadable value
/// means; for an expiry it is "expired" (see
/// [`api_keys::ApiKeyRow::is_expired`]).
pub(crate) fn parse_iso(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
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

#[cfg(test)]
mod tests {
    use super::{iso, parse_iso};

    /// Which spellings a stored timestamp column can be read back from.
    /// Migration `015_api_key_expiry_canonical` sorts rows by exactly this
    /// rule, so its SQL and this function have to agree on the boundary;
    /// `migrations::api_key_expiry_tests` holds both to the same cases.
    #[test]
    fn parse_iso_reads_rfc_3339_and_nothing_else() {
        for (stored, utc) in [
            ("2026-06-01T12:00:00Z", "2026-06-01T12:00:00Z"),
            ("2026-06-01T12:00:00z", "2026-06-01T12:00:00Z"),
            ("2026-06-01t12:00:00Z", "2026-06-01T12:00:00Z"),
            // A space separator is RFC 3339's own permitted alternative to
            // `T`, and chrono takes it.
            ("2026-06-01 12:00:00Z", "2026-06-01T12:00:00Z"),
            ("2026-06-01T12:00:00+00:00", "2026-06-01T12:00:00Z"),
            ("2026-06-01T12:00:00.123456+00:00", "2026-06-01T12:00:00Z"),
            ("2026-06-01T20:00:00+09:00", "2026-06-01T11:00:00Z"),
        ] {
            let parsed = parse_iso(stored).unwrap_or_else(|| panic!("{stored} is RFC 3339"));
            assert_eq!(iso(parsed), utc, "{stored}");
        }

        for stored in [
            // No offset at all: RFC 3339 requires one, and guessing UTC for a
            // caller's local wall clock would move the instant.
            "2026-06-01T12:00:00",
            "2026-06-01",
            // A basic-format offset (`+0900`, no colon) is ISO 8601, not
            // RFC 3339.
            "2026-06-01T12:00:00+0900",
            // Shaped like a timestamp, names no day.
            "2026-02-31T12:00:00Z",
            "never",
            "",
        ] {
            assert!(parse_iso(stored).is_none(), "{stored:?} is not RFC 3339");
        }
    }

    #[test]
    fn now_iso_is_what_parse_iso_reads() {
        let now = super::now_iso();
        assert_eq!(iso(parse_iso(&now).expect("now_iso is RFC 3339")), now);
    }
}

/// Database faults for tests of multi-row writes.
#[cfg(test)]
pub(crate) mod test_faults {
    use crate::test_support::TestContext;

    /// Make every insert into `table` fail inside the database — after the
    /// statements before it in the same write have run — until the returned
    /// trigger is dropped. Fixture setup, the documented exception to the
    /// no-raw-SQL rule: nothing else fails the SECOND row of a write, which
    /// is the failure a non-atomic account creation leaves half done.
    pub(crate) async fn fail_inserts_into(ctx: &TestContext, table: &str) -> String {
        let trigger = format!("fail_{table}_insert");
        wafer_core::clients::database::exec_raw(
            &ctx.fixture(),
            &format!(
                "CREATE TRIGGER {trigger} BEFORE INSERT ON {table} \
                 BEGIN SELECT RAISE(ABORT, 'simulated failure on the second write'); END"
            ),
            &[],
        )
        .await
        .expect("install the failing trigger");
        trigger
    }

    pub(crate) async fn drop_trigger(ctx: &TestContext, trigger: &str) {
        wafer_core::clients::database::exec_raw(
            &ctx.fixture(),
            &format!("DROP TRIGGER {trigger}"),
            &[],
        )
        .await
        .expect("drop the failing trigger");
    }
}
