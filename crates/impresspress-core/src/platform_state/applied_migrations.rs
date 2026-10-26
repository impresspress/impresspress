//! `impresspress__admin__applied_migrations`: the per-file migration ledger.
//! One row per migration file a block has applied, written in the same
//! transaction as the file's own statements, so a row exists exactly when its
//! file's statements committed (`migration_helper`'s module docs say how the
//! runner plans from it).
//!
//! # Created by the runner, not by a migration file
//!
//! Admin's `001` cannot create it: the ledger has to exist before admin's
//! first file can be recorded in it, and a shipped file is never edited. So
//! the runner sends [`create_table_sql`] — `CREATE TABLE IF NOT EXISTS`, built
//! with `wafer_sql_utils::ddl` — before it reads the ledger, once for each
//! block whose migrations it plans. `ensure_schema_table` is not used: D1
//! refuses it, because there the schema is migration-owned.
//!
//! # Who reads and writes it
//!
//! Every block reads its own rows, under WRAP, through [`list_for_block`]
//! (admin grants `read("*", TABLE)`); the native server's pre-build apply
//! reads admin's through [`list_for_block_via_service`]. Rows are written
//! only inside a migration file's transaction, as the statement
//! [`insert_statement`] renders, which goes over the `__ddl__` channel with
//! the file's own statements. That channel already runs any statement, so the
//! ledger needs no write grant.
//!
//! The KV row cache never holds it: `cache_key::classify_table` names only
//! `variables` and `block_settings`.

use std::{collections::HashMap, sync::Arc};

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, ListOptions};
use wafer_core::interfaces::database::service::{
    col_string, col_text, pk, DatabaseError, DatabaseService, RawStatement, RecordData, Table,
};
use wafer_run::{context::Context, ErrorCode, WaferError};
use wafer_sql_utils::Backend;

use crate::{
    db_read::{self, Bound},
    util::RecordExt,
};

pub const TABLE: &str = "impresspress__admin__applied_migrations";

/// How a row came to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The runner ran the file's statements, in the transaction that wrote
    /// this row.
    Ran,
    /// Recorded without running: the block's `block_settings.current_hash`,
    /// stamped by a runner from before the ledger only after a whole list had
    /// run, proved this file ran with exactly today's text.
    Adopted,
    /// Run once more by the legacy replay, for a block whose record from
    /// before the ledger proves nothing about which text ran.
    Replayed,
}

impl Origin {
    /// The column's spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ran => "ran",
            Self::Adopted => "adopted",
            Self::Replayed => "replayed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ran" => Some(Self::Ran),
            "adopted" => Some(Self::Adopted),
            "replayed" => Some(Self::Replayed),
            _ => None,
        }
    }
}

/// One row of the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub id: String,
    /// Spelled as in `block_settings.block_name`, e.g. `wafer-run/auth`.
    pub block_name: String,
    /// The file's basename without the dialect suffix, e.g.
    /// `016_roles_are_grants`.
    pub file_name: String,
    /// `migration_helper::content_hash` of the file as it was applied, in the
    /// dialect this database runs.
    pub content_sha256: String,
    /// RFC 3339.
    pub applied_at: String,
    pub origin: Origin,
}

impl AppliedMigration {
    /// A row for `file_name` of `block_name`, applied now, with a fresh id.
    pub fn new(block_name: &str, file_name: &str, content_sha256: &str, origin: Origin) -> Self {
        Self {
            id: format!("am_{}", uuid::Uuid::new_v4()),
            block_name: block_name.to_string(),
            file_name: file_name.to_string(),
            content_sha256: content_sha256.to_string(),
            applied_at: crate::util::now_rfc3339(),
            origin,
        }
    }

    /// Decode one row. Every column is required: a row the runner cannot read
    /// is refused rather than skipped, because a skipped row would read as a
    /// file that never ran, and the runner would run it again.
    pub fn from_record(id: &str, data: &RecordData) -> Result<Self, String> {
        let field = |name: &str| {
            let value = data.str_field(name);
            if value.is_empty() {
                Err(format!("{TABLE} row `{id}` has no {name}"))
            } else {
                Ok(value.to_string())
            }
        };
        let origin = field("origin")?;
        Ok(Self {
            id: id.to_string(),
            block_name: field("block_name")?,
            file_name: field("file_name")?,
            content_sha256: field("content_sha256")?,
            applied_at: field("applied_at")?,
            origin: Origin::parse(&origin)
                .ok_or_else(|| format!("{TABLE} row `{id}` has an unknown origin `{origin}`"))?,
        })
    }

    /// The column map this row inserts as.
    pub fn to_data(&self) -> HashMap<String, Value> {
        HashMap::from([
            ("id".to_string(), json!(self.id)),
            ("block_name".to_string(), json!(self.block_name)),
            ("file_name".to_string(), json!(self.file_name)),
            ("content_sha256".to_string(), json!(self.content_sha256)),
            ("applied_at".to_string(), json!(self.applied_at)),
            ("origin".to_string(), json!(self.origin.as_str())),
        ])
    }
}

/// The ledger's schema. `UNIQUE(block_name, file_name)` is what makes a row
/// the claim on its file: a second migrator's insert of the same file is
/// refused, and its transaction with it.
fn table() -> Table {
    let mut table = Table::new(TABLE);
    table.columns = vec![
        pk("id"),
        col_string("block_name"),
        col_string("file_name"),
        col_string("content_sha256"),
        col_text("applied_at"),
        col_string("origin"),
    ];
    table.unique_keys = vec![vec!["block_name".to_string(), "file_name".to_string()]];
    table
}

/// `CREATE TABLE IF NOT EXISTS` for the ledger, in `backend`'s dialect.
pub fn create_table_sql(backend: Backend) -> Result<String, String> {
    wafer_sql_utils::ddl::build_create_table(&table(), backend)
        .map(|statement| statement.sql)
        .map_err(|e| format!("render the {TABLE} schema: {e}"))
}

/// The `INSERT` that records `row`, in `backend`'s dialect, as one statement
/// of a migration file's transaction.
pub fn insert_statement(row: &AppliedMigration, backend: Backend) -> RawStatement {
    let mut data: Vec<(String, Value)> = row.to_data().into_iter().collect();
    // A stable column order, so the same row always renders the same text.
    data.sort_by(|(a, _), (b, _)| a.cmp(b));
    let statement = wafer_sql_utils::query::build_insert(TABLE, &data, backend);
    RawStatement {
        query: statement.sql,
        args: wafer_sql_utils::value::sea_values_to_json(statement.values),
    }
}

fn block_filter(block_name: &str) -> Vec<Filter> {
    vec![Filter {
        field: "block_name".into(),
        operator: FilterOp::Equal,
        value: Value::String(block_name.to_string()),
    }]
}

fn decode<'a>(
    records: impl IntoIterator<Item = (&'a str, &'a RecordData)>,
) -> Result<Vec<AppliedMigration>, String> {
    records
        .into_iter()
        .map(|(id, data)| AppliedMigration::from_record(id, data))
        .collect()
}

/// Every row `block_name` has, in no particular order, read as the block
/// itself under WRAP. A row that does not decode fails the read.
pub async fn list_for_block(
    ctx: &dyn Context,
    block_name: &str,
) -> Result<Vec<AppliedMigration>, WaferError> {
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        block_filter(block_name),
        Bound::OnePer("migration file the block ships"),
    )
    .await?;
    decode(records.iter().map(|r| (r.id.as_str(), &r.data)))
        .map_err(|e| WaferError::new(ErrorCode::Internal, e))
}

/// [`list_for_block`] over the platform [`DatabaseService`], for the native
/// server's pre-build apply of admin's list, which runs before any `Context`
/// exists. Same bound: more rows than [`db_read::UNPAGED_LIMIT`] is a fault,
/// not a page.
pub async fn list_for_block_via_service(
    db: &Arc<dyn DatabaseService>,
    block_name: &str,
) -> Result<Vec<AppliedMigration>, DatabaseError> {
    let listed = db
        .list(
            TABLE,
            &ListOptions {
                filters: block_filter(block_name),
                limit: Some(db_read::UNPAGED_LIMIT + 1),
                skip_count: true,
                ..Default::default()
            },
        )
        .await?;
    if listed.records.len() > db_read::UNPAGED_LIMIT as usize {
        return Err(DatabaseError::Internal(format!(
            "{TABLE} holds more than {} rows for {block_name}, one per migration file it ships",
            db_read::UNPAGED_LIMIT
        )));
    }
    decode(listed.records.iter().map(|r| (r.id.as_str(), &r.data))).map_err(DatabaseError::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The codec: a row reads back as it was written, and a row with an
    /// unknown origin or a missing column is refused, not skipped.
    #[test]
    fn a_row_round_trips_and_a_bad_one_is_refused() {
        let row = AppliedMigration::new("impresspress/probe", "001_a", "abc", Origin::Adopted);
        let data: RecordData = row.to_data().into_iter().collect();
        assert_eq!(
            AppliedMigration::from_record(&row.id, &data),
            Ok(row.clone())
        );

        let mut unknown = data.clone();
        unknown.insert("origin".into(), json!("guessed"));
        assert!(AppliedMigration::from_record(&row.id, &unknown).is_err());

        let mut missing = data;
        missing.shift_remove("content_sha256");
        assert!(AppliedMigration::from_record(&row.id, &missing).is_err());
    }

    /// The schema the runner creates: the unique pair that makes the insert
    /// the claim on a file, in both dialects.
    #[test]
    fn the_schema_makes_block_and_file_unique() {
        for backend in [Backend::Sqlite, Backend::Postgres] {
            let sql = create_table_sql(backend).expect("render");
            assert!(
                sql.starts_with(&format!("CREATE TABLE IF NOT EXISTS \"{TABLE}\"")),
                "{sql}"
            );
            assert!(
                sql.contains("UNIQUE(\"block_name\", \"file_name\")"),
                "{sql}"
            );
        }
    }
}
