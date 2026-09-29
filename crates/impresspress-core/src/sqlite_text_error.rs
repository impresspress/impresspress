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
//! connection, overload, queue-full, storage timeout and internal-error reset
//! messages, and SQLite's busy database (D1, and sql.js in the browser).
//!
//! Every other failure (a `NOT NULL` or `CHECK` violation, a missing table, a
//! syntax error) stays [`DatabaseError::Internal`], as it does on the native
//! backends.

use wafer_core::interfaces::database::service::DatabaseError;

/// The text SQLite puts in every primary- or unique-key violation.
const UNIQUE_VIOLATION: &str = "UNIQUE constraint failed";

/// Texts of failures that retrying can cure, matched as substrings.
const TRANSIENT: &[&str] = &[
    "Network connection lost",
    "D1 DB is overloaded",
    "Too many requests queued",
    "storage operation exceeded timeout",
    "D1_ERROR: internal error",
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
    } else if TRANSIENT.iter().any(|text| message.contains(text)) {
        DatabaseError::Unavailable(message)
    } else {
        DatabaseError::Internal(message)
    }
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

    /// Every other failure is a fault, exactly as on the native backends.
    #[test]
    fn any_other_failure_stays_internal() {
        for text in [
            "NOT NULL constraint failed: t.name",
            "CHECK constraint failed: positive",
            "FOREIGN KEY constraint failed",
            "no such table: t",
        ] {
            assert!(
                matches!(statement_error(text.into()), DatabaseError::Internal(_)),
                "{text}"
            );
        }
    }
}
