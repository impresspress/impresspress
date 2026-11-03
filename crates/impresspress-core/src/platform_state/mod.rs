//! Platform state: the six `impresspress__admin__*` tables the runtime
//! itself reads and writes — configuration variables, per-block settings,
//! admin-created WRAP grants, the request log, role grants and the per-file
//! migration ledger.
//!
//! Each submodule owns one table: its `TABLE` name, its row struct, one
//! `from_record` and one `to_data`, so a column name is spelled in exactly
//! one Rust file. Every other module — the boot orchestrators, the request
//! pipeline, the admin block's pages, the framework auth block, the dev
//! sandbox, the platform adapters and the CLI — reaches the table through
//! the functions here (spec `2026-09-06-ownership-and-repo-boundaries`,
//! section 2.1).
//!
//! Two callers, one codec: boot runs before WRAP over
//! `Arc<dyn DatabaseService>` (native seeds pre-wafer, Cloudflare and the
//! browser seed after admin's `Init`), blocks and the pipeline run under
//! WRAP over `&dyn Context`. Where the spec names both flavours a module
//! offers both, and both go through the same row codec.
//!
//! Five of the tables are still created by the admin block's migrations —
//! `blocks/admin/migrations/001_admin_schema.{sqlite,postgres}.sql` (all
//! five), `002_variables_block_column` (`variables.block`),
//! `003_block_settings_seed_hash` (`block_settings.seed_defaults_hash`),
//! `004_user_roles_unique` (one `user_roles` row per `(user_id, role)`) and
//! `005_wrap_grants_append_column` (`wrap_grants.append`) — applied by
//! admin's `Init` like every block's migrations. Moving the DDL would change
//! the shipped migration files every deployment has applied, so the schema
//! keeps living there (spec decision 5.4). The sixth, the migration ledger
//! ([`applied_migrations`]), is created by the migration runner itself,
//! because it has to exist before admin's first file can be recorded in it.
//! `blocks/admin` also keeps `ADMIN_BLOCK_ID` and the
//! `collections(..)`/`grants(..)` declarations, because the
//! `impresspress__admin__` prefix makes it the WRAP schema owner.

pub mod applied_migrations;
pub mod block_settings;
pub mod request_logs;
pub mod user_roles;
pub mod variables;
pub mod wrap_grants;

/// The error of a cold-start-tolerant read whose `list` failed and whose
/// table-existence probe failed too, so the read could not tell "the table
/// is not there yet" from "the read failed".
///
/// [`block_settings::load`], [`wrap_grants::load`] and
/// [`variables::read_unfiltered`] each list first and probe only after a
/// failed list. When the probe cannot answer, which error the read returns
/// decides its class, and with it whether a caller retries
/// ([`wafer_run::init_failure_is_transient`]):
///
/// - **The probe's, when it is transient and the list's is not.** On a
///   fresh database the list's `no such table` is the cold start, not a
///   fault — and the probe that would have said so was refused by something
///   a retry clears: an outage, or the invocation's spent call budget (a
///   cold `list` of a missing table is several calls, and can spend the
///   last of it). Returning the list's error made a deploy under a small
///   budget a permanent 500 naming a missing table.
/// - **Otherwise the list's**, the read that failed: both transient (an
///   outage), both permanent, or a transient list whose probe failed
///   permanently (a malformed answer) — a retry may still resolve the list,
///   so it is not made permanent by the probe.
///
/// The other error is logged beside it either way.
pub(crate) fn unresolved_read_error(
    table: &str,
    list: wafer_core::interfaces::database::service::DatabaseError,
    probe: wafer_core::interfaces::database::service::DatabaseError,
) -> wafer_core::interfaces::database::service::DatabaseError {
    let transient = |e: &wafer_core::interfaces::database::service::DatabaseError| {
        wafer_run::init_failure_is_transient(e.code(), e.detail_code())
    };
    tracing::error!(
        table = %table,
        list_error = %list,
        probe_error = %probe,
        "a list failed and the table-existence probe could not answer whether \
         the table exists",
    );
    if transient(&probe) && !transient(&list) {
        probe
    } else {
        list
    }
}

/// One page of typed rows plus the envelope `db::paginated_list` returns,
/// for the admin pages' pagination controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

#[cfg(test)]
mod unresolved_read_tests {
    use wafer_core::interfaces::database::service::DatabaseError;

    use super::unresolved_read_error;

    fn no_such_table() -> DatabaseError {
        DatabaseError::Internal("no such table: t".into())
    }
    fn budget() -> DatabaseError {
        DatabaseError::CallBudgetExhausted("no call left".into())
    }
    fn outage() -> DatabaseError {
        DatabaseError::Unavailable("D1 down".into())
    }
    fn malformed() -> DatabaseError {
        DatabaseError::Internal("malformed probe answer".into())
    }

    /// The read keeps the class a retry needs: the probe's error only when
    /// it is transient and the list's is not; the list's otherwise.
    #[test]
    fn an_unresolved_read_is_transient_when_either_failure_may_clear() {
        let pick = |list, probe| unresolved_read_error("t", list, probe).to_string();
        // The cold start whose probe the budget refused: the budget.
        assert_eq!(pick(no_such_table(), budget()), budget().to_string());
        assert_eq!(pick(no_such_table(), outage()), outage().to_string());
        // A transient list keeps its own error whatever the probe says.
        assert_eq!(pick(outage(), malformed()), outage().to_string());
        assert_eq!(pick(outage(), budget()), outage().to_string());
        // Both permanent: the read that failed.
        assert_eq!(
            pick(no_such_table(), malformed()),
            no_such_table().to_string()
        );
    }
}
