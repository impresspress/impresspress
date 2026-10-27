//! The proof behind `crate::migration_history`: every list state a block's
//! migrations were in on main, rebuilt the way the runner from before the
//! ledger built it, upgraded through today's runner, and compared with a
//! fresh install.
//!
//! Driven by `tests/migration_history.rs` (SQLite) and by
//! `impresspress-native`'s `tests/postgres_migration_history.rs` (the
//! `postgres` lane), over a database the caller provides, empty, for each
//! case.
//!
//! - [`resolve`] turns a state's composition back into file texts: the live
//!   file when its whole-text hash matches, else the byte-exact fixture the
//!   generator wrote under `migrations/history/`.
//! - [`upgrade`] stamps a database the way the old runner left it, runs the
//!   real runner (`apply_pending_via_service`, the pre-build entry point the
//!   native server and the browser use) and reports what happened.
//! - [`Schema`] is what the comparison reads: tables, and per table the set
//!   of (column, declared type, not-null, default, primary key) and of
//!   indexes, order-insensitive. Seeding writes one row per table with raw
//!   SQL that names only the columns the table has — never through a typed
//!   write, whose lazy column-add would change the very schema under test.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde_json::Value;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

use crate::{
    migration_helper::{
        self, legacy_runner, migration_set_hash, BlockMigrations, HistoricalState, MigrationError,
        MigrationFile,
    },
    platform_state::{
        applied_migrations::{self, AppliedMigration},
        block_settings::{self, BlockSettingsPatch},
    },
};

fn dialect(backend: Backend) -> &'static str {
    match backend {
        Backend::Sqlite => "sqlite",
        Backend::Postgres => "postgres",
    }
}

fn sha8(text: &str) -> String {
    crate::util::sha256_hex(text.as_bytes())[..8].to_string()
}

/// The directory a block's migrations live in: its name's last segment.
pub fn block_dir(migrations: &BlockMigrations) -> &'static str {
    migrations
        .block
        .rsplit_once('/')
        .map_or(migrations.block, |(_, dir)| dir)
}

/// The file texts of `state`'s composition, in order, as
/// `(basename, whole text)`.
pub fn resolve(
    migrations: &BlockMigrations,
    backend: Backend,
    state: &HistoricalState,
) -> Result<Vec<(String, String)>, String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/blocks")
        .join(block_dir(migrations))
        .join("migrations");
    let dialect = dialect(backend);
    state
        .composition
        .iter()
        .map(|entry| {
            let (name, want) = entry
                .split_once('@')
                .ok_or_else(|| format!("composition entry `{entry}` has no @"))?;
            let live = root.join(format!("{name}.{dialect}.sql"));
            if let Ok(text) = std::fs::read_to_string(&live) {
                if sha8(&text) == want {
                    return Ok((name.to_string(), text));
                }
            }
            let fixture = root.join(format!("history/{name}.{want}.{dialect}.sql"));
            let text = std::fs::read_to_string(&fixture).map_err(|_| {
                format!(
                    "state {} needs fixture {}: run examples/migration-history.rs --write",
                    &state.list_hash[..8],
                    fixture.display()
                )
            })?;
            if sha8(&text) != want {
                return Err(format!(
                    "fixture {} does not hash to {want}",
                    fixture.display()
                ));
            }
            Ok((name.to_string(), text))
        })
        .collect()
}

/// Run `files` with the runner from before the ledger.
pub async fn legacy_apply(
    db: &Arc<dyn DatabaseService>,
    files: &[(String, String)],
) -> Result<(), String> {
    let pairs: Vec<MigrationFile<'_>> = files
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    legacy_runner::apply(db, &pairs).await
}

/// Apply admin's live list with the real runner, as every target does
/// before any other block: admin's tables hold every block's migration
/// record, and auth's 016 writes admin's `user_roles`.
pub async fn fresh_admin(db: &Arc<dyn DatabaseService>, backend: Backend) -> Result<(), String> {
    let admin = &crate::blocks::admin::migrations::MIGRATIONS;
    let (files, history) = admin.for_backend(backend);
    migration_helper::apply_pending_via_service(db, backend, admin.block, files, history)
        .await
        .map_err(|e| e.to_string())
}

/// Today's list of `migrations` applied to an empty database with the real
/// runner (admin's first): the schema every upgrade is compared with.
pub async fn fresh_install(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
    migrations: &BlockMigrations,
) -> Result<Schema, String> {
    fresh_admin(db, backend).await?;
    if migrations.block != crate::blocks::admin::ADMIN_BLOCK_ID {
        let (files, history) = migrations.for_backend(backend);
        migration_helper::apply_pending_via_service(db, backend, migrations.block, files, history)
            .await
            .map_err(|e| e.to_string())?;
    }
    Schema::read(db, backend).await
}

/// Record `list_hash` as `block`'s applied list, as the runner from before
/// the ledger stamped it after a whole list ran.
pub async fn stamp(
    db: &Arc<dyn DatabaseService>,
    block: &str,
    list_hash: &str,
) -> Result<(), String> {
    block_settings::upsert_fields_via_service(
        db,
        block,
        BlockSettingsPatch {
            current_hash: Some(list_hash.to_string()),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| format!("stamp {block}: {e}"))
}

/// The entry of `migrations`' `backend` history whose list hash starts with
/// `short` (its first eight characters, as RELEASE.md and the plan name
/// states).
pub fn state(
    migrations: &BlockMigrations,
    backend: Backend,
    short: &str,
) -> Result<&'static HistoricalState, String> {
    let mut found = migrations
        .for_backend(backend)
        .1
        .iter()
        .filter(|s| s.list_hash.starts_with(short));
    match (found.next(), found.next()) {
        (Some(state), None) => Ok(state),
        (None, _) => Err(format!(
            "{} has no {} state {short}",
            migrations.block,
            dialect(backend)
        )),
        (Some(_), Some(_)) => Err(format!(
            "{short} names more than one {} state",
            migrations.block
        )),
    }
}

/// The list hash of the entry of `migrations`' `backend` history first seen
/// at main commit `commit` (its short SHA, as `first_seen` ends): the way to
/// name one state in both dialects, whose hashes differ.
pub fn hash_at(
    migrations: &BlockMigrations,
    backend: Backend,
    commit: &str,
) -> Result<&'static str, String> {
    migrations
        .for_backend(backend)
        .1
        .iter()
        .find(|s| s.first_seen.ends_with(commit))
        .map(|s| s.list_hash)
        .ok_or_else(|| {
            format!(
                "{} has no {} state first seen at {commit}",
                migrations.block,
                dialect(backend)
            )
        })
}

/// What the runner from before the ledger left after applying `short`'s
/// list: its files run with that runner, and its hash stamped as `block`'s
/// record. Admin's tables (which hold the record) must already exist,
/// unless `migrations` is admin's.
pub async fn build_state(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
    migrations: &BlockMigrations,
    short: &str,
) -> Result<&'static HistoricalState, String> {
    let state = state(migrations, backend, short)?;
    legacy_apply(db, &resolve(migrations, backend, state)?).await?;
    stamp(db, migrations.block, state.list_hash).await?;
    Ok(state)
}

/// Upgrade `block` through today's runner.
pub async fn upgrade(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
    migrations: &BlockMigrations,
) -> Result<(), MigrationError> {
    let (files, history) = migrations.for_backend(backend);
    migration_helper::apply_pending_via_service(db, backend, migrations.block, files, history).await
}

/// `block`'s ledger rows, sorted by file.
pub async fn ledger(
    db: &Arc<dyn DatabaseService>,
    block: &str,
) -> Result<Vec<AppliedMigration>, String> {
    let mut rows = applied_migrations::list_for_block_via_service(db, block)
        .await
        .map_err(|e| e.to_string())?;
    rows.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(rows)
}

/// `block`'s stored `current_hash`.
pub async fn current_hash(db: &Arc<dyn DatabaseService>, block: &str) -> Result<String, String> {
    Ok(block_settings::load(db)
        .await
        .map_err(|e| e.to_string())?
        .state(block)
        .migration
        .current_hash)
}

/// Today's list hash of `migrations` in `backend`'s dialect.
pub fn todays_hash(migrations: &BlockMigrations, backend: Backend) -> String {
    migration_set_hash(migrations.for_backend(backend).0)
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// One column, as the comparison sees it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Column {
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default: Option<String>,
    pub primary_key: bool,
}

/// One index: its name (`(auto)` for one SQLite makes for a `UNIQUE` or
/// `PRIMARY KEY` constraint, whose name follows the table's history rather
/// than its schema), whether it is unique, and its columns in order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: BTreeSet<Column>,
    pub indexes: BTreeSet<Index>,
    /// Foreign keys: `(column, referenced table, referenced column)`.
    pub foreign_keys: BTreeSet<(String, String, String)>,
    /// The first literal each column's `CHECK (column IN (...))` allows.
    pub check_literals: BTreeMap<String, String>,
}

/// A database's schema, every table, order-insensitive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    pub tables: BTreeMap<String, Table>,
}

async fn rows(
    db: &Arc<dyn DatabaseService>,
    sql: &str,
    args: &[Value],
) -> Result<Vec<BTreeMap<String, Value>>, String> {
    Ok(db
        .query_raw(sql, args)
        .await
        .map_err(|e| format!("`{sql}`: {e}"))?
        .into_iter()
        .map(|r| r.data.into_iter().collect())
        .collect())
}

fn text(row: &BTreeMap<String, Value>, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn truthy(row: &BTreeMap<String, Value>, key: &str) -> bool {
    match row.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("yes") || s == "t",
        _ => false,
    }
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `CHECK (column IN ('a', …))` in SQLite's text, or `column = ANY
/// (ARRAY['a'::text, …])` in PostgreSQL's: the first literal per column.
fn check_literals(sql: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let lower = sql.to_ascii_lowercase();
    for (marker, open) in [(" in (", "'"), (" = any (array[", "'")] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(marker) {
            let at = from + at;
            let before = &sql[..at];
            let column: String = before
                .trim_end_matches(|c: char| c == ')' || c == '(' || c.is_whitespace())
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let rest = &sql[at + marker.len()..];
            if let Some(start) = rest.find(open) {
                if let Some(end) = rest[start + 1..].find('\'') {
                    if !column.is_empty() {
                        out.entry(column)
                            .or_insert_with(|| rest[start + 1..start + 1 + end].to_string());
                    }
                }
            }
            from = at + marker.len();
        }
    }
    out
}

impl Schema {
    /// Read every table of `db`.
    pub async fn read(db: &Arc<dyn DatabaseService>, backend: Backend) -> Result<Self, String> {
        match backend {
            Backend::Sqlite => Self::read_sqlite(db).await,
            Backend::Postgres => Self::read_postgres(db).await,
        }
    }

    async fn read_sqlite(db: &Arc<dyn DatabaseService>) -> Result<Self, String> {
        let mut tables = BTreeMap::new();
        for t in rows(
            db,
            "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            &[],
        )
        .await?
        {
            let name = text(&t, "name");
            let mut table = Table {
                check_literals: check_literals(&text(&t, "sql")),
                ..Table::default()
            };
            for c in rows(db, &format!("PRAGMA table_info({})", quote(&name)), &[]).await? {
                table.columns.insert(Column {
                    name: text(&c, "name"),
                    declared_type: text(&c, "type").to_ascii_uppercase(),
                    not_null: truthy(&c, "notnull"),
                    default: c
                        .get("dflt_value")
                        .filter(|v| !v.is_null())
                        .map(|_| text(&c, "dflt_value")),
                    primary_key: truthy(&c, "pk"),
                });
            }
            for i in rows(db, &format!("PRAGMA index_list({})", quote(&name)), &[]).await? {
                let index = text(&i, "name");
                let columns = rows(db, &format!("PRAGMA index_info({})", quote(&index)), &[])
                    .await?
                    .iter()
                    .map(|c| text(c, "name"))
                    .collect();
                table.indexes.insert(Index {
                    name: if text(&i, "origin") == "c" {
                        index
                    } else {
                        "(auto)".to_string()
                    },
                    unique: truthy(&i, "unique"),
                    columns,
                });
            }
            for f in rows(
                db,
                &format!("PRAGMA foreign_key_list({})", quote(&name)),
                &[],
            )
            .await?
            {
                table
                    .foreign_keys
                    .insert((text(&f, "from"), text(&f, "table"), text(&f, "to")));
            }
            tables.insert(name, table);
        }
        Ok(Self { tables })
    }

    async fn read_postgres(db: &Arc<dyn DatabaseService>) -> Result<Self, String> {
        let mut tables = BTreeMap::new();
        for t in rows(
            db,
            "SELECT table_name::text AS name FROM information_schema.tables \
             WHERE table_schema = current_schema() AND table_type = 'BASE TABLE'",
            &[],
        )
        .await?
        {
            let name = text(&t, "name");
            let mut table = Table::default();
            let pk: BTreeSet<String> = rows(
                db,
                "SELECT a.attname::text AS name FROM pg_index ix \
                 JOIN pg_class c ON c.oid = ix.indrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace AND n.nspname = current_schema() \
                 JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY(ix.indkey) \
                 WHERE c.relname = $1 AND ix.indisprimary",
                &[Value::from(name.clone())],
            )
            .await?
            .iter()
            .map(|r| text(r, "name"))
            .collect();
            for c in rows(
                db,
                "SELECT column_name::text AS name, data_type::text AS type, \
                 is_nullable::text AS nullable, column_default::text AS dflt \
                 FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND table_name = $1",
                &[Value::from(name.clone())],
            )
            .await?
            {
                let column = text(&c, "name");
                table.columns.insert(Column {
                    primary_key: pk.contains(&column),
                    name: column,
                    declared_type: text(&c, "type").to_ascii_uppercase(),
                    not_null: text(&c, "nullable") == "NO",
                    default: c
                        .get("dflt")
                        .filter(|v| !v.is_null())
                        .map(|_| text(&c, "dflt")),
                });
            }
            for i in rows(
                db,
                "SELECT i.relname::text AS name, ix.indisunique AS uniq, \
                 array_to_string(ARRAY(SELECT a.attname FROM unnest(ix.indkey) WITH ORDINALITY \
                 AS k(attnum, ord) JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = \
                 k.attnum ORDER BY k.ord), ',') AS cols \
                 FROM pg_class t JOIN pg_namespace n ON n.oid = t.relnamespace \
                 AND n.nspname = current_schema() \
                 JOIN pg_index ix ON ix.indrelid = t.oid JOIN pg_class i ON i.oid = ix.indexrelid \
                 WHERE t.relname = $1",
                &[Value::from(name.clone())],
            )
            .await?
            {
                table.indexes.insert(Index {
                    name: text(&i, "name"),
                    unique: truthy(&i, "uniq"),
                    columns: text(&i, "cols").split(',').map(str::to_string).collect(),
                });
            }
            for f in rows(
                db,
                "SELECT kcu.column_name::text AS col, ccu.table_name::text AS ref_table, \
                 ccu.column_name::text AS ref_col \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                 JOIN information_schema.constraint_column_usage ccu \
                   ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema \
                 WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = current_schema() \
                   AND tc.table_name = $1",
                &[Value::from(name.clone())],
            )
            .await?
            {
                table.foreign_keys.insert((
                    text(&f, "col"),
                    text(&f, "ref_table"),
                    text(&f, "ref_col"),
                ));
            }
            for c in rows(
                db,
                "SELECT pg_get_constraintdef(con.oid)::text AS def FROM pg_constraint con \
                 JOIN pg_class t ON t.oid = con.conrelid \
                 JOIN pg_namespace n ON n.oid = t.relnamespace AND n.nspname = current_schema() \
                 WHERE con.contype = 'c' AND t.relname = $1",
                &[Value::from(name.clone())],
            )
            .await?
            {
                for (column, literal) in check_literals(&text(&c, "def")) {
                    table.check_literals.entry(column).or_insert(literal);
                }
            }
            tables.insert(name, table);
        }
        Ok(Self { tables })
    }

    /// What `self` (an upgraded database) has that `fresh` lacks, and what
    /// `fresh` has that `self` lacks or has differently. Tables are compared
    /// by columns and indexes; the migration ledger's and the seed's rows do
    /// not enter into it.
    pub fn compare(&self, fresh: &Schema) -> Comparison {
        let mut extra = Vec::new();
        let mut missing = Vec::new();
        for (name, table) in &self.tables {
            match fresh.tables.get(name) {
                None => extra.push(format!("table {name}")),
                Some(want) => {
                    for c in table.columns.difference(&want.columns) {
                        if want.columns.iter().any(|w| w.name == c.name) {
                            missing.push(format!("{name}.{} differs: {c:?}", c.name));
                        } else {
                            extra.push(format!("column {name}.{}", c.name));
                        }
                    }
                    for c in want.columns.difference(&table.columns) {
                        if !table.columns.iter().any(|w| w.name == c.name) {
                            missing.push(format!("column {name}.{}", c.name));
                        }
                    }
                    for i in table.indexes.difference(&want.indexes) {
                        extra.push(format!("index {name}.{}{:?}", i.name, i.columns));
                    }
                    for i in want.indexes.difference(&table.indexes) {
                        missing.push(format!("index {name}.{}{:?}", i.name, i.columns));
                    }
                }
            }
        }
        for name in fresh.tables.keys() {
            if !self.tables.contains_key(name) {
                missing.push(format!("table {name}"));
            }
        }
        Comparison { extra, missing }
    }
}

/// [`Schema::compare`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// Present in the upgraded database only: harmless residue.
    pub extra: Vec<String>,
    /// Absent from it, or declared differently: it did not converge.
    pub missing: Vec<String>,
}

// ---------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------

/// One row seeded into one table: how to find it again.
#[derive(Debug, Clone)]
pub struct Seeded {
    pub table: String,
    /// `(column, value)` of the row's primary key, when it has one the seed
    /// chose; otherwise the row is found by counting.
    pub key: Option<(String, Value)>,
}

fn literal(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        other => other.to_string(),
    }
}

/// A value of `column`'s declared type that its table's checks accept.
fn value_for(table: &Table, table_name: &str, column: &Column, backend: Backend) -> Value {
    if let Some(literal) = table.check_literals.get(&column.name) {
        return Value::from(literal.clone());
    }
    let t = column.declared_type.as_str();
    if t.contains("BOOL") {
        return match backend {
            Backend::Postgres => Value::Bool(true),
            Backend::Sqlite => Value::from(1),
        };
    }
    if t.contains("INT") || t.contains("SERIAL") {
        return Value::from(1);
    }
    if t.contains("REAL")
        || t.contains("FLOA")
        || t.contains("DOUB")
        || t.contains("NUMERIC")
        || t.contains("DECIMAL")
    {
        return Value::from(1.0);
    }
    if t.contains("JSON") {
        return Value::from("{}");
    }
    if t.contains("TIMESTAMP") || t.contains("DATE") {
        return Value::from("2026-01-01T00:00:00Z");
    }
    if t.contains("BYTEA") || t.contains("BLOB") {
        return Value::from("");
    }
    Value::from(format!("seed-{table_name}-{}", column.name))
}

/// Insert one row into every table of `schema` except the ledger and
/// `skip`, parents before children, naming only the columns each table has
/// and leaving every column with a default (or nullable) to it.
pub async fn seed(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
    schema: &Schema,
    skip: &[&str],
) -> Result<Vec<Seeded>, String> {
    let mut order: Vec<&String> = Vec::new();
    let mut pending: Vec<&String> = schema
        .tables
        .keys()
        .filter(|t| t.as_str() != applied_migrations::TABLE && !skip.contains(&t.as_str()))
        .collect();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|t| {
            let ready = schema.tables[*t].foreign_keys.iter().all(|(_, parent, _)| {
                parent == *t
                    || order.iter().any(|o| o == &parent)
                    || !schema.tables.contains_key(parent)
                    || skip.contains(&parent.as_str())
            });
            if ready {
                order.push(t);
            }
            !ready
        });
        if pending.len() == before {
            return Err(format!("foreign keys form a cycle among {pending:?}"));
        }
    }

    let mut values: BTreeMap<(String, String), Value> = BTreeMap::new();
    let mut seeded = Vec::new();
    for name in order {
        let table = &schema.tables[name];
        let mut columns = Vec::new();
        let mut literals = Vec::new();
        let mut key = None;
        let rowid_alias = backend == Backend::Sqlite
            && table.columns.iter().filter(|c| c.primary_key).count() == 1
            && table
                .columns
                .iter()
                .any(|c| c.primary_key && c.declared_type == "INTEGER");
        for column in &table.columns {
            let fk = table
                .foreign_keys
                .iter()
                .find(|(c, _, _)| *c == column.name);
            let value = if let Some((_, parent, parent_column)) = fk {
                match values.get(&(parent.clone(), parent_column.clone())) {
                    Some(v) => v.clone(),
                    None if !column.not_null => continue,
                    None => value_for(table, name, column, backend),
                }
            } else if column.primary_key && rowid_alias {
                continue;
            } else if column.primary_key && column.default.is_some() {
                continue;
            } else if !column.primary_key && (column.default.is_some() || !column.not_null) {
                continue;
            } else {
                value_for(table, name, column, backend)
            };
            if column.primary_key && table.columns.iter().filter(|c| c.primary_key).count() == 1 {
                key = Some((column.name.clone(), value.clone()));
            }
            values.insert((name.clone(), column.name.clone()), value.clone());
            columns.push(quote(&column.name));
            literals.push(literal(&value));
        }
        let sql = if columns.is_empty() {
            match backend {
                Backend::Sqlite => format!("INSERT INTO {} DEFAULT VALUES", quote(name)),
                Backend::Postgres => format!("INSERT INTO {} DEFAULT VALUES", quote(name)),
            }
        } else {
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                quote(name),
                columns.join(", "),
                literals.join(", ")
            )
        };
        // Test-fixture setup: a row in exactly the shape this state's
        // schema has.
        db.exec_raw(&sql, &[])
            .await
            .map_err(|e| format!("seed {name}: `{sql}`: {e}"))?;
        seeded.push(Seeded {
            table: name.clone(),
            key,
        });
    }
    Ok(seeded)
}

/// The seeded rows that are gone.
pub async fn lost(db: &Arc<dyn DatabaseService>, seeded: &[Seeded]) -> Result<Vec<String>, String> {
    let mut gone = Vec::new();
    for row in seeded {
        let (sql, args) = match &row.key {
            Some((column, value)) => (
                format!(
                    "SELECT count(*) AS n FROM {} WHERE {} = {}",
                    quote(&row.table),
                    quote(column),
                    literal(value)
                ),
                Vec::new(),
            ),
            None => (
                format!("SELECT count(*) AS n FROM {}", quote(&row.table)),
                Vec::new(),
            ),
        };
        let n = match rows(db, &sql, &args).await {
            Ok(found) => found
                .first()
                .and_then(|r| r.get("n"))
                .and_then(|n| {
                    n.as_i64()
                        .or_else(|| n.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(0),
            Err(_) => 0,
        };
        if n == 0 {
            gone.push(row.table.clone());
        }
    }
    Ok(gone)
}

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// What one upgrade of one state ended in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The schema equals a fresh install's.
    Converges,
    /// It has everything a fresh install has, and more.
    Residue(Vec<String>),
    /// It lacks something, or the upgrade or the old runner failed.
    Diverges(String),
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Converges => write!(f, "converges"),
            Self::Residue(extra) => write!(f, "residue {}", extra.join("; ")),
            Self::Diverges(why) => write!(f, "diverges {why}"),
        }
    }
}

/// Build a database by running `lists` in order with the old runner, stamp
/// the last one, seed it, upgrade it, and compare it with `fresh`.
///
/// For admin itself the old runner builds admin; for any other block admin
/// is built fresh first, as every target's boot applies admin before the
/// rest.
pub async fn case(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
    migrations: &BlockMigrations,
    lists: &[Vec<(String, String)>],
    state: &HistoricalState,
    fresh: &Schema,
) -> Result<(Outcome, CaseReport), String> {
    let is_admin = migrations.block == crate::blocks::admin::ADMIN_BLOCK_ID;
    if !is_admin {
        fresh_admin(db, backend).await?;
    }
    for list in lists {
        if let Err(e) = legacy_apply(db, list).await {
            return Ok((
                Outcome::Diverges(format!("the old runner failed: {e}")),
                CaseReport::default(),
            ));
        }
    }
    // The record the old runner stamped once the last list had run.
    stamp(db, migrations.block, state.list_hash).await?;
    let before = Schema::read(db, backend).await?;
    let admin_tables: Vec<&str> = if is_admin {
        Vec::new()
    } else {
        fresh
            .tables
            .keys()
            .filter(|t| t.starts_with("impresspress__admin__"))
            .map(String::as_str)
            .collect()
    };
    let seeded = seed(db, backend, &before, &admin_tables).await?;
    if let Err(e) = upgrade(db, backend, migrations).await {
        return Ok((
            Outcome::Diverges(format!("the upgrade failed: {e}")),
            CaseReport::default(),
        ));
    }
    let after = Schema::read(db, backend).await?;
    let comparison = after.compare(fresh);
    let report = CaseReport {
        ledger: ledger(db, migrations.block).await?,
        current_hash: current_hash(db, migrations.block).await?,
        lost: lost(db, &seeded).await?,
    };
    let outcome = if !comparison.missing.is_empty() {
        Outcome::Diverges(comparison.missing.join("; "))
    } else if !comparison.extra.is_empty() {
        Outcome::Residue(comparison.extra)
    } else {
        Outcome::Converges
    };
    Ok((outcome, report))
}

/// What a case left behind, beside its schema.
#[derive(Debug, Clone, Default)]
pub struct CaseReport {
    pub ledger: Vec<AppliedMigration>,
    pub current_hash: String,
    /// Tables whose seeded row is gone.
    pub lost: Vec<String>,
}

// ---------------------------------------------------------------------------
// Drivers, shared by the SQLite and the PostgreSQL lanes
// ---------------------------------------------------------------------------

/// A fresh, empty database for one case.
pub type NewDatabase<'a> = &'a (dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Arc<dyn DatabaseService>> + 'a>>
         + 'a);

/// Every block compiled into this build, with its migrations.
pub fn blocks() -> Vec<&'static BlockMigrations> {
    crate::blocks::all_block_migrations()
}

/// Every state of every block resolves to texts whose list hash is its own.
pub fn check_compositions(backend: Backend) -> Vec<String> {
    let mut problems = Vec::new();
    for migrations in blocks() {
        for state in migrations.for_backend(backend).1 {
            match resolve(migrations, backend, state) {
                Ok(files) => {
                    let pairs: Vec<MigrationFile<'_>> = files
                        .iter()
                        .map(|(n, t)| (n.as_str(), t.as_str()))
                        .collect();
                    if migration_set_hash(&pairs) != state.list_hash {
                        problems.push(format!(
                            "{} {}: its composition hashes to {}",
                            migrations.block,
                            &state.list_hash[..8],
                            &migration_set_hash(&pairs)[..8]
                        ));
                    }
                }
                Err(e) => problems.push(format!("{}: {e}", migrations.block)),
            }
        }
    }
    problems
}

/// The table's invariants: unique list hashes, and every `Applied` set a
/// non-empty prefix of today's list.
pub fn check_invariants(backend: Backend) -> Vec<String> {
    let mut problems = Vec::new();
    for migrations in blocks() {
        let (files, history) = migrations.for_backend(backend);
        if files.is_empty() {
            // This dialect is not compiled in (PostgreSQL without its
            // feature); its lane checks it.
            continue;
        }
        let mut seen = BTreeSet::new();
        for state in history {
            if !seen.insert(state.list_hash) {
                problems.push(format!("{} {} twice", migrations.block, state.list_hash));
            }
            if let crate::migration_helper::Verdict::Applied(applied) = state.verdict {
                let prefix = !applied.is_empty()
                    && applied.len() <= files.len()
                    && applied.iter().zip(files).all(|(a, (n, _))| a == n);
                if !prefix {
                    problems.push(format!(
                        "{} {}: {applied:?} is not a non-empty prefix of today's list",
                        migrations.block,
                        &state.list_hash[..8]
                    ));
                }
            }
        }
    }
    problems
}

/// What [`check_states`] found: the problems with supported entries'
/// direct upgrades and unsupported entries' refusals (which must be none),
/// and one crossing line per supported entry and other path, for the
/// checked-in record.
#[derive(Debug, Default)]
pub struct StatesReport {
    pub problems: Vec<String>,
    pub crossing: Vec<String>,
}

/// Every entry of every block, in `backend`'s dialect:
///
/// - **supported, direct** (path i): the state's own list run by the old
///   runner, stamped, seeded, upgraded. It must converge, every seeded row
///   must survive, the ledger must hold the `Applied` files as `adopted`
///   from the state's hash and the rest as `ran` (nothing, for today's own
///   list, which takes the fast path), and `current_hash` must be today's;
/// - **supported, from an older state** — every state from the block's
///   first in order up to this one (path ii), and jumps straight to this one
///   from the block's first state and from the newest state before its
///   horizon (path iii): the outcome is recorded as a crossing line,
///   converging or not;
/// - **unsupported**: stamped and run, it must be refused as
///   `UnsupportedState`, record nothing and leave the record unchanged.
pub async fn check_states(backend: Backend, new_db: NewDatabase<'_>) -> StatesReport {
    let mut report = StatesReport::default();
    for migrations in blocks() {
        if let Err(e) = check_block(backend, new_db, migrations, &mut report).await {
            report.problems.push(format!("{}: {e}", migrations.block));
        }
    }
    report
}

async fn check_block(
    backend: Backend,
    new_db: NewDatabase<'_>,
    migrations: &BlockMigrations,
    report: &mut StatesReport,
) -> Result<(), String> {
    let (files, history) = migrations.for_backend(backend);
    if history.is_empty() {
        return Ok(());
    }
    let fresh = fresh_install(&new_db().await, backend, migrations).await?;
    let resolved: Vec<Vec<(String, String)>> = history
        .iter()
        .map(|state| resolve(migrations, backend, state))
        .collect::<Result<_, _>>()?;
    let todays = todays_hash(migrations, backend);
    let block = migrations.block;
    let first_supported = history
        .iter()
        .position(|s| matches!(s.verdict, crate::migration_helper::Verdict::Applied(_)));

    for (at, state) in history.iter().enumerate() {
        let short = &state.list_hash[..8];
        match state.verdict {
            crate::migration_helper::Verdict::Applied(applied) => {
                // Path i.
                let db = new_db().await;
                let (outcome, case_report) =
                    case(&db, backend, migrations, &resolved[at..=at], state, &fresh).await?;
                if outcome != Outcome::Converges {
                    report.problems.push(format!("{block} {short}: {outcome}"));
                    continue;
                }
                if !case_report.lost.is_empty() {
                    report.problems.push(format!(
                        "{block} {short}: seeded rows lost in {:?}",
                        case_report.lost
                    ));
                }
                if case_report.current_hash != todays {
                    report
                        .problems
                        .push(format!("{block} {short}: current_hash is not today's"));
                }
                // Today's own list takes the fast path: nothing is read,
                // adopted or run.
                let is_todays = state.list_hash == todays;
                let expected: Vec<(String, applied_migrations::Origin, String)> = files
                    .iter()
                    .filter(|_| !is_todays)
                    .enumerate()
                    .map(|(i, (name, _))| {
                        if i < applied.len() {
                            (
                                name.to_string(),
                                applied_migrations::Origin::Adopted,
                                state.list_hash.to_string(),
                            )
                        } else {
                            (
                                name.to_string(),
                                applied_migrations::Origin::Ran,
                                String::new(),
                            )
                        }
                    })
                    .collect();
                let mut got: Vec<(String, applied_migrations::Origin, String)> = case_report
                    .ledger
                    .iter()
                    .map(|r| (r.file_name.clone(), r.origin, r.adopted_from.clone()))
                    .collect();
                got.sort();
                let mut expected = expected;
                expected.sort();
                if got != expected {
                    report.problems.push(format!(
                        "{block} {short}: ledger {got:?}, expected {expected:?}"
                    ));
                }

                // Paths ii and iii, from the block's first state.
                let db = new_db().await;
                let (cumulative, _) =
                    case(&db, backend, migrations, &resolved[..=at], state, &fresh).await?;
                report.crossing.push(format!(
                    "{block} {} {short} every-state {cumulative}",
                    dialect(backend)
                ));
                // Jumps: a database last migrated at an older state, then
                // at this one, with nothing between — from the block's first
                // state, and from the newest state before its horizon (a
                // database created just before the cut-off).
                let mut origins = vec![("from-first", 0)];
                if let Some(edge) = first_supported.and_then(|f| f.checked_sub(1)) {
                    if edge > 0 {
                        origins.push(("from-last-unsupported", edge));
                    }
                }
                for (label, origin) in origins {
                    if origin >= at {
                        continue;
                    }
                    let db = new_db().await;
                    let lists = [resolved[origin].clone(), resolved[at].clone()];
                    let (jump, _) = case(&db, backend, migrations, &lists, state, &fresh).await?;
                    report.crossing.push(format!(
                        "{block} {} {short} {label} {jump}",
                        dialect(backend)
                    ));
                }
            }
            crate::migration_helper::Verdict::Unsupported(reason) => {
                // The refusal depends on the record alone, so the database is
                // one whose record this build reads: the block's first
                // supported state for admin (whose record table is admin's
                // own), this state for any other block.
                let db = new_db().await;
                let is_admin = block == crate::blocks::admin::ADMIN_BLOCK_ID;
                if is_admin {
                    let base = first_supported.ok_or("admin has no supported state")?;
                    legacy_apply(&db, &resolved[base]).await?;
                } else {
                    fresh_admin(&db, backend).await?;
                    if let Err(e) = legacy_apply(&db, &resolved[at]).await {
                        report
                            .problems
                            .push(format!("{block} {short}: the old runner failed: {e}"));
                        continue;
                    }
                }
                stamp(&db, block, state.list_hash).await?;
                let mut before = Schema::read(&db, backend).await?;
                before.tables.remove(applied_migrations::TABLE);
                let refusal = upgrade(&db, backend, migrations)
                    .await
                    .err()
                    .and_then(|e| e.refusal().cloned());
                let want = crate::migration_helper::Refusal::UnsupportedState {
                    list_hash: state.list_hash.to_string(),
                    first_seen: state.first_seen.to_string(),
                    reason: reason.to_string(),
                };
                if refusal.as_ref() != Some(&want) {
                    report.problems.push(format!(
                        "{block} {short}: expected {want:?}, got {refusal:?}"
                    ));
                }
                let mut after = Schema::read(&db, backend).await?;
                after.tables.remove(applied_migrations::TABLE);
                if after != before {
                    report
                        .problems
                        .push(format!("{block} {short}: the refusal changed the schema"));
                }
                if !ledger(&db, block).await?.is_empty() {
                    report
                        .problems
                        .push(format!("{block} {short}: the refusal recorded rows"));
                }
                if current_hash(&db, block).await? != state.list_hash {
                    report
                        .problems
                        .push(format!("{block} {short}: the refusal moved the record"));
                }
            }
        }
    }
    Ok(())
}

/// Every block's whole list from an empty database, admin first, each file
/// one transaction, through the real runner: the failures (which must be
/// none) and, per block, the files and statements sent.
pub async fn apply_fresh(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
) -> Vec<Result<(String, usize, usize), String>> {
    let mut out = Vec::new();
    for migrations in blocks() {
        let (files, history) = migrations.for_backend(backend);
        let statements: usize = files
            .iter()
            .map(|(_, sql)| migration_helper::executable_statements(sql).len())
            .sum();
        out.push(
            migration_helper::apply_pending_via_service(
                db,
                backend,
                migrations.block,
                files,
                history,
            )
            .await
            .map(|()| (migrations.block.to_string(), files.len(), statements))
            .map_err(|e| e.to_string()),
        );
    }
    out
}
