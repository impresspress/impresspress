//! The migration runner from before the per-file ledger, reproduced as a test
//! fixture: it rebuilds what a real pre-ledger database holds, so that the
//! history tests can upgrade it through today's runner.
//!
//! From `79a6ec58` (2026-05-18) to `fa454025` (2026-10-07) every runner
//! applied a list the same way: every file, every statement, one at a time,
//! with a refused `ALTER TABLE … ADD COLUMN` taken as already applied when
//! the error said the column exists, and any other failure stopping the
//! list. That is all this module does. It records nothing — a caller that
//! wants the old runner's stamp writes `block_settings.current_hash` itself.
//!
//! It is the one place that tolerance survives. It is compiled only for
//! tests and the `test-support` feature, never into a deployment, and
//! `migration_helper`'s `no_deployed_module_names_the_legacy_runner` fails if
//! a non-test module names it.

use std::sync::Arc;

use wafer_core::interfaces::database::service::DatabaseService;

use super::{executable_statements, is_duplicate_column_error, MigrationFile};

/// Apply every statement of every file of `files`, in order, the way the
/// runner before the ledger did. Returns the first statement that failed and
/// was not a duplicate `ADD COLUMN`, with the database's error.
pub async fn apply(
    db: &Arc<dyn DatabaseService>,
    files: &[MigrationFile<'_>],
) -> Result<(), String> {
    for (name, sql) in files {
        for statement in executable_statements(sql) {
            if let Err(e) = db.exec_raw(&statement, &[]).await {
                let error = e.to_string();
                if is_alter_add_column(&statement) && is_duplicate_column_error(&error) {
                    continue;
                }
                return Err(format!("{name}: `{statement}`: {error}"));
            }
        }
    }
    Ok(())
}

/// `true` when `statement` is `ALTER TABLE … ADD COLUMN …`, case-insensitive:
/// the one shape whose duplicate the old runner tolerated.
fn is_alter_add_column(statement: &str) -> bool {
    let shape = statement
        .to_ascii_uppercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    shape.starts_with("ALTER TABLE ") && shape.contains(" ADD COLUMN ")
}

#[cfg(test)]
mod tests {
    use super::is_alter_add_column;

    #[test]
    fn detects_alter_table_add_column_in_all_shapes() {
        assert!(is_alter_add_column("ALTER TABLE foo ADD COLUMN bar TEXT"));
        assert!(is_alter_add_column(
            "alter table foo\n  add column bar text"
        ));
        assert!(!is_alter_add_column("ALTER TABLE foo DROP COLUMN bar"));
        assert!(!is_alter_add_column("CREATE TABLE foo (id TEXT)"));
    }
}
