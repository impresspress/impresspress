//! A failed SQLite statement, classified from the only thing two of our
//! adapters get back: the error's text.
//!
//! `wafer_core`'s `DatabaseService` contract names a write that duplicates a
//! primary or unique key [`DatabaseError::AlreadyExists`], so a caller can tell
//! "that key is taken" from a fault; the database handler turns it into
//! `ErrorCode::AlreadyExists` (a 409). The native backends read the driver's
//! structured code (`SQLITE_CONSTRAINT_UNIQUE`/`_PRIMARYKEY`, SQLSTATE
//! `23505`). Cloudflare D1 and the browser's sql.js have no structured code to
//! read — a `worker::Error` and a JS exception carry a message and nothing
//! else — so both adapters classify that message here, with one predicate,
//! rather than each keeping its own copy.
//!
//! SQLite spells both kinds of key violation the same way:
//! `UNIQUE constraint failed: <table>.<column>` — for a `UNIQUE` index, and
//! for a primary key too, whether it is a rowid alias or not.
//!
//! A failure that retrying can cure is [`DatabaseError::Unavailable`], as the
//! native backends report a busy database or a lost connection: the runtime
//! retries a block `Init` that failed that way instead of caching the failure
//! until the isolate is replaced. [`TRANSIENT`] lists the texts: D1's lost
//! connection, overload, queue-full, storage timeout, reset and
//! unreachable-replica messages, the Free plan's daily row read and row write
//! limits (they lift at 00:00 UTC), and SQLite's busy database (D1, and
//! sql.js in the browser). The match ignores ASCII case: D1's texts are not
//! cased consistently (`internal error; reference = …` beside `Internal error
//! in D1 DB storage…`).
//!
//! Every other failure (a `NOT NULL` or `CHECK` violation, a missing table, a
//! syntax error) stays [`DatabaseError::Internal`], as it does on the native
//! backends. So do a full database (`Exceeded maximum DB size.`) and a full
//! account (`…exceeded D1's maximum account storage limit…`): retrying cannot
//! cure them, only an operator can.

use wafer_core::interfaces::database::service::DatabaseError;

/// The text SQLite puts in every primary- or unique-key violation.
const UNIQUE_VIOLATION: &str = "UNIQUE constraint failed";

/// Texts of failures that retrying can cure, matched as substrings, ignoring
/// ASCII case. The D1 texts are the ones Cloudflare's D1 error list gives
/// (<https://developers.cloudflare.com/d1/observability/debug-d1/>).
/// `D1 DB is overloaded` covers both overload texts (`Requests queued for too
/// long.` and `Too many requests queued.`).
const TRANSIENT: &[&str] = &[
    "Network connection lost",
    "D1 DB is overloaded",
    "Too many requests queued",
    "storage operation exceeded timeout",
    "D1_ERROR: internal error",
    "exceeded D1's free tier daily row read limit",
    "exceeded D1's free tier daily row write limit",
    "D1 DB reset because its code was updated",
    "Internal error while starting up D1 DB storage caused object to be reset",
    "Internal error in D1 DB storage caused object to be reset",
    "Replica disconnected from primary",
    "Cannot resolve D1 DB due to transient issue on remote node",
    "D1 DB's isolate exceeded its memory limit and was reset",
    "D1 DB exceeded its CPU time limit and was reset",
    "database is locked",
    "SQLITE_BUSY",
];

/// `message` — the text of a failed statement — as a [`DatabaseError`]:
/// [`DatabaseError::AlreadyExists`] for a primary- or unique-key violation,
/// [`DatabaseError::Unavailable`] for a transient failure ([`TRANSIENT`]),
/// [`DatabaseError::Internal`] for anything else.
pub fn statement_error(message: String) -> DatabaseError {
    if message.contains(UNIQUE_VIOLATION) {
        DatabaseError::AlreadyExists(message)
    } else if is_transient(&message) {
        DatabaseError::Unavailable(message)
    } else {
        DatabaseError::Internal(message)
    }
}

/// Whether `message` contains one of the [`TRANSIENT`] texts, in any ASCII
/// case.
fn is_transient(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    TRANSIENT
        .iter()
        .any(|text| message.contains(&text.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The texts D1 and sql.js hand back for a taken key: D1 prefixes its own
    /// tag and appends the result code, sql.js passes SQLite's message
    /// through as is. Both are a taken key.
    #[test]
    fn a_unique_or_primary_key_violation_is_already_exists() {
        for text in [
            "D1_ERROR: UNIQUE constraint failed: impresspress__admin__roles.name: \
             SQLITE_CONSTRAINT",
            "UNIQUE constraint failed: t.id",
            "sql exec: JsValue(Error: UNIQUE constraint failed: t.a, t.b)",
        ] {
            assert!(
                matches!(
                    statement_error(text.into()),
                    DatabaseError::AlreadyExists(_)
                ),
                "{text}"
            );
        }
    }

    /// D1's and SQLite's transient failures — a lost connection, an
    /// overloaded or queue-full database, a storage timeout, an internal-error
    /// reset, a busy database — are `Unavailable`, so the runtime retries a
    /// block `Init` that hit one. The texts are the ones D1 and sql.js return.
    #[test]
    fn a_transient_failure_is_unavailable() {
        for text in [
            "D1_ERROR: Network connection lost.: undefined",
            "D1_ERROR: D1 DB is overloaded. Too many requests queued.: undefined",
            "D1_ERROR: Too many requests queued.",
            "D1_ERROR: storage operation exceeded timeout which caused object to be reset.",
            "D1_ERROR: internal error; reference = abc123",
            "D1_ERROR: database is locked: SQLITE_BUSY",
            "sql exec: JsValue(Error: database is locked)",
        ] {
            assert!(
                matches!(statement_error(text.into()), DatabaseError::Unavailable(_)),
                "{text}"
            );
        }
    }

    /// `text` is `Unavailable`.
    fn assert_unavailable(text: &str) {
        assert!(
            matches!(statement_error(text.into()), DatabaseError::Unavailable(_)),
            "{text} must be Unavailable, got {:?}",
            statement_error(text.into())
        );
    }

    /// D1's error text `text` is `Unavailable` in both shapes the Cloudflare
    /// adapter classifies: a rejected statement, which carries D1's
    /// `D1_ERROR:` tag, and the batch guard's `D1 batch statement failed:`
    /// wrapping, which does not depend on that tag being there.
    fn assert_d1_text_unavailable(text: &str) {
        assert_unavailable(&format!("D1_ERROR: {text}"));
        assert_unavailable(&format!("D1 batch statement failed: {text}"));
    }

    /// The Free plan's daily row read limit. Its `D1_ERROR:` shape is the
    /// exact text D1 returned in the 2026-10-30 outage. It lifts at 00:00 UTC,
    /// so a block `Init` that hit it must be retried, not cached as failed for
    /// the isolate's life.
    #[test]
    fn the_daily_row_read_limit_is_unavailable() {
        assert_d1_text_unavailable(
            "Your account has exceeded D1's free tier daily row read limit. \
             Upgrade to a paid plan or wait until tomorrow (midnight UTC) to continue. \
             See https://developers.cloudflare.com/d1/platform/limits/ for more details.",
        );
    }

    /// The row write twin of the read limit, in Cloudflare's text.
    #[test]
    fn the_daily_row_write_limit_is_unavailable() {
        assert_d1_text_unavailable(
            "Your account has exceeded D1's free tier daily row write limit. \
             Upgrade to a paid plan or wait until tomorrow (midnight UTC) to continue. \
             See https://developers.cloudflare.com/d1/platform/limits/ for more details.",
        );
    }

    #[test]
    fn a_reset_for_a_code_update_is_unavailable() {
        assert_d1_text_unavailable("D1 DB reset because its code was updated.");
    }

    #[test]
    fn a_reset_while_starting_storage_is_unavailable() {
        assert_d1_text_unavailable(
            "Internal error while starting up D1 DB storage caused object to be reset.",
        );
    }

    #[test]
    fn a_reset_after_a_storage_error_is_unavailable() {
        assert_d1_text_unavailable("Internal error in D1 DB storage caused object to be reset.");
    }

    #[test]
    fn a_replica_disconnected_from_its_primary_is_unavailable() {
        assert_d1_text_unavailable("Replica disconnected from primary.");
    }

    #[test]
    fn an_unresolvable_database_is_unavailable() {
        assert_d1_text_unavailable("Cannot resolve D1 DB due to transient issue on remote node.");
    }

    /// Already covered by the `D1 DB is overloaded` entry; pinned so that
    /// entry is not narrowed to the other overload text.
    #[test]
    fn requests_queued_for_too_long_is_unavailable() {
        assert_d1_text_unavailable("D1 DB is overloaded. Requests queued for too long.");
    }

    #[test]
    fn a_reset_at_the_memory_limit_is_unavailable() {
        assert_d1_text_unavailable("D1 DB's isolate exceeded its memory limit and was reset.");
    }

    #[test]
    fn a_reset_at_the_cpu_limit_is_unavailable() {
        assert_d1_text_unavailable("D1 DB exceeded its CPU time limit and was reset.");
    }

    /// The match ignores case: D1 does not case its texts consistently, and a
    /// listed text in another case is the same condition.
    #[test]
    fn a_transient_text_matches_in_any_case() {
        for text in [
            "D1_ERROR: Internal error; reference = abc123",
            "D1_ERROR: NETWORK CONNECTION LOST.",
            "sql exec: JsValue(Error: Database is locked)",
        ] {
            assert_unavailable(text);
        }
    }

    /// Every other failure is a fault, exactly as on the native backends —
    /// a full database or account included: only an operator can cure those.
    #[test]
    fn any_other_failure_stays_internal() {
        for text in [
            "NOT NULL constraint failed: t.name",
            "CHECK constraint failed: positive",
            "FOREIGN KEY constraint failed",
            "no such table: t",
            "D1_ERROR: Exceeded maximum DB size.",
            "D1_ERROR: Your account has exceeded D1's maximum account storage limit, \
             please contact Cloudflare to raise your limit",
        ] {
            assert!(
                matches!(statement_error(text.into()), DatabaseError::Internal(_)),
                "{text}"
            );
        }
    }
}
