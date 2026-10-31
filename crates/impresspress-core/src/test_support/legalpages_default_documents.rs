//! The proof behind legalpages' `003_default_documents`, on both dialects.
//!
//! Driven by `tests/migration_legalpages_default_documents.rs` (SQLite) and
//! by `impresspress-native`'s `tests/postgres_migration_history.rs` (the
//! `postgres` lane), over databases the caller provides, empty. Every
//! database goes through the real runner (`apply_pending_via_service`, the
//! entry point the native server and the browser use pre-build), admin's
//! list first.
//!
//! - **a fresh install** holds the two documents `Init` used to publish:
//!   published, version 1, by `system`, with `DocumentType::title()` and the
//!   same text; their three timestamps are one stamp in the executor's
//!   `+00:00` form, which sorts against the executor's own stamps as the
//!   instants do; 003 is recorded `ran` with its content hash.
//! - **a database that already has documents** (the defaults the `Init` seed
//!   published, and an admin's draft beside them) keeps exactly the rows it
//!   had: nothing is inserted, so nothing is duplicated.
//! - **a table with no published document** (only an archived version, or
//!   only a draft) gets nothing either: the guard is any row, in any
//!   status. A guard on published rows alone would insert the defaults, and
//!   over an archived `terms` v1 fail on 002's unique version index.
//! - **a type an admin deleted** stays deleted: the guard is the whole
//!   table, not one per type.
//! - **an empty table** is seeded by the upgrade that runs 003, as the next
//!   boot's `Init` seeded it before. Once 003 is recorded, emptying the table
//!   is final: re-running the upgrade, and re-planning against the ledger
//!   after the block's `current_hash` is lost, inserts nothing.

use std::sync::Arc;

use serde_json::{json, Value};
use wafer_block::db::{ListOptions, SortField};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

use super::migration_history::{self as history, NewDatabase};
use crate::{
    blocks::legalpages::migrations::{DEFAULT_DOCUMENTS, MIGRATIONS},
    migration_helper::{self, content_hash},
    platform_state::applied_migrations::Origin,
};

const TABLE: &str = "impresspress__legalpages__documents";

/// The rows 003 writes, as `(id, doc_type, title, content)`; every one is
/// `published`, version 1, by `system`. The titles are
/// `DocumentType::title()`'s and the texts are the ones `Init` published.
const DEFAULTS: [(&str, &str, &str, &str); 2] = [
    (
        "seed-privacy",
        "privacy",
        "Privacy policy",
        "This is the default privacy policy. Please update it in the admin panel.\n",
    ),
    (
        "seed-terms",
        "terms",
        "Terms of service",
        "These are the default terms of service. Please update them in the admin panel.\n",
    ),
];

/// The columns a document row has, in the order rows are compared.
const COLUMNS: [&str; 10] = [
    "id",
    "doc_type",
    "title",
    "content",
    "status",
    "version",
    "created_by",
    "published_at",
    "created_at",
    "updated_at",
];

/// Every document row, ordered by id, each as its column values.
async fn rows(db: &Arc<dyn DatabaseService>) -> Result<Vec<Vec<Value>>, String> {
    let list = db
        .list(
            TABLE,
            &ListOptions {
                sort: vec![SortField {
                    field: "id".to_string(),
                    desc: false,
                }],
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("list documents: {e}"))?;
    Ok(list
        .records
        .iter()
        .map(|record| {
            COLUMNS
                .iter()
                .map(|column| record.data.get(*column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect())
}

/// Admin's list, then legalpages' up to (not including) 003, through the
/// real runner: the database a release before this one left.
async fn before_003(db: &Arc<dyn DatabaseService>, backend: Backend) -> Result<(), String> {
    history::fresh_admin(db, backend).await?;
    let (files, history_rows) = MIGRATIONS.for_backend(backend);
    let at = files
        .iter()
        .position(|(name, _)| *name == DEFAULT_DOCUMENTS)
        .ok_or_else(|| format!("legalpages does not ship {DEFAULT_DOCUMENTS}"))?;
    migration_helper::apply_pending_via_service(
        db,
        backend,
        MIGRATIONS.block,
        &files[..at],
        history_rows,
    )
    .await
    .map_err(|e| format!("legalpages before {DEFAULT_DOCUMENTS}: {e}"))
}

async fn upgrade(db: &Arc<dyn DatabaseService>, backend: Backend) -> Result<(), String> {
    history::upgrade(db, backend, &MIGRATIONS)
        .await
        .map_err(|e| format!("the upgrade failed: {e}"))
}

/// A document written through the executor, as the block writes one: the
/// executor mints the id and stamps `created_at` and `updated_at`.
async fn write_document(
    db: &Arc<dyn DatabaseService>,
    doc_type: &str,
    title: &str,
    content: &str,
    status: &str,
    version: i64,
) -> Result<Vec<Value>, String> {
    let mut data = crate::util::json_map(json!({
        "doc_type": doc_type,
        "title": title,
        "content": content,
        "status": status,
        "version": version,
        "created_by": "system",
    }));
    if status == "published" {
        data.insert(
            "published_at".to_string(),
            json!(crate::util::now_rfc3339()),
        );
    }
    let record = db
        .create(TABLE, data)
        .await
        .map_err(|e| format!("write a {status} {doc_type} document: {e}"))?;
    Ok(COLUMNS
        .iter()
        .map(|column| record.data.get(*column).cloned().unwrap_or(Value::Null))
        .collect())
}

/// What is wrong with the documents 003 wrote, if anything: exactly the two
/// defaults, each with one `+00:00` stamp in all three timestamp columns.
fn check_defaults(rows: &[Vec<Value>], what: &str, problems: &mut Vec<String>) {
    let expected: Vec<Vec<Value>> = DEFAULTS
        .iter()
        .map(|(id, doc_type, title, content)| {
            vec![
                json!(id),
                json!(doc_type),
                json!(title),
                json!(content),
                json!("published"),
                json!(1),
                json!("system"),
            ]
        })
        .collect();
    let written: Vec<Vec<Value>> = rows.iter().map(|row| row[..7].to_vec()).collect();
    if written != expected {
        problems.push(format!(
            "{what}: the documents are {written:?}, not the two defaults {expected:?}"
        ));
        return;
    }
    for row in rows {
        let stamps = &row[7..];
        let first = stamps[0].as_str().unwrap_or_default();
        if stamps.iter().any(|stamp| stamp.as_str() != Some(first)) {
            problems.push(format!(
                "{what}: {} has three different timestamps {stamps:?}",
                row[0]
            ));
        }
        if !is_whole_second_utc(first) {
            problems.push(format!(
                "{what}: {} is stamped {first:?}, not `YYYY-MM-DDTHH:MM:SS+00:00`",
                row[0]
            ));
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS+00:00`: the executor's offset, whole seconds.
fn is_whole_second_utc(stamp: &str) -> bool {
    let b = stamp.as_bytes();
    let digits = |range: std::ops::Range<usize>| range.into_iter().all(|i| b[i].is_ascii_digit());
    b.len() == 25
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19)
        && &stamp[19..] == "+00:00"
        && chrono::DateTime::parse_from_rfc3339(stamp).is_ok()
}

/// The cases of the module docs, each on a database `new_db` hands back
/// empty: what is wrong, if anything.
pub async fn check(backend: Backend, new_db: NewDatabase<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(e) = a_fresh_install_has_the_defaults(backend, new_db, &mut problems).await {
        problems.push(format!("fresh install: {e}"));
    }
    if let Err(e) = existing_documents_are_kept_as_they_are(backend, new_db, &mut problems).await {
        problems.push(format!("existing documents: {e}"));
    }
    for (status, version) in [("archived", 1), ("draft", 0)] {
        if let Err(e) = a_table_with_no_published_document_is_kept(
            backend,
            new_db,
            status,
            version,
            &mut problems,
        )
        .await
        {
            problems.push(format!("only {status}: {e}"));
        }
    }
    if let Err(e) = a_deleted_type_stays_deleted(backend, new_db, &mut problems).await {
        problems.push(format!("deleted type: {e}"));
    }
    if let Err(e) = an_empty_table_is_seeded_once(backend, new_db, &mut problems).await {
        problems.push(format!("empty table: {e}"));
    }
    problems
}

async fn a_fresh_install_has_the_defaults(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    history::fresh_install(&db, backend, &MIGRATIONS).await?;
    let seeded = rows(&db).await?;
    check_defaults(&seeded, "fresh install", problems);

    let (files, _) = MIGRATIONS.for_backend(backend);
    let (_, sql) = files
        .iter()
        .find(|(name, _)| *name == DEFAULT_DOCUMENTS)
        .ok_or("003 is not wired")?;
    let ledger = history::ledger(&db, MIGRATIONS.block).await?;
    match ledger.iter().find(|r| r.file_name == DEFAULT_DOCUMENTS) {
        Some(row) if row.origin == Origin::Ran && row.content_sha256 == content_hash(sql) => {}
        Some(row) => problems.push(format!(
            "fresh install: 003 is recorded {:?} with {}, not ran with {}",
            row.origin,
            row.content_sha256,
            content_hash(sql)
        )),
        None => problems.push("fresh install: 003 is not recorded".to_string()),
    }

    // The stamp sorts as text against the executor's stamps as the instants
    // do. Its own formatter, one millisecond either side of the seed …
    let Some(stamp) = seeded.first().and_then(|row| row[9].as_str()) else {
        return Ok(());
    };
    let instant = chrono::DateTime::parse_from_rfc3339(stamp)
        .map_err(|e| format!("parse {stamp:?}: {e}"))?
        .with_timezone(&chrono::Utc);
    let millisecond = chrono::Duration::milliseconds(1);
    let before = (instant - millisecond).to_rfc3339();
    let after = (instant + millisecond).to_rfc3339();
    if !(before.as_str() < stamp && stamp < after.as_str()) {
        problems.push(format!(
            "fresh install: {stamp:?} does not sort between the executor's {before:?} and \
             {after:?}"
        ));
    }
    // … and a row the executor writes after the seed, most likely within the
    // same second, which is where a `…Z` stamp would sort after it.
    let draft = write_document(&db, "terms", "Next", "body", "draft", 0).await?;
    let written = draft[9].as_str().unwrap_or_default().to_string();
    if written.as_str() <= stamp {
        problems.push(format!(
            "fresh install: a row the executor wrote after the seed, stamped {written:?}, \
             sorts before the seed's {stamp:?}"
        ));
    }

    // Re-running the upgrade is a no-op.
    upgrade(&db, backend).await?;
    if rows(&db).await?.len() != 3 {
        problems.push("fresh install: re-running the upgrade changed the documents".to_string());
    }
    Ok(())
}

async fn existing_documents_are_kept_as_they_are(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    before_003(&db, backend).await?;
    // The defaults as the `Init` seed published them (executor ids and
    // stamps), and an admin's draft beside them.
    for (_, doc_type, title, content) in DEFAULTS {
        write_document(&db, doc_type, title, content, "published", 1).await?;
    }
    write_document(&db, "terms", "Terms of service", "edited", "draft", 0).await?;
    let before = rows(&db).await?;

    upgrade(&db, backend).await?;

    let after = rows(&db).await?;
    if after != before {
        problems.push(format!(
            "existing documents: the upgrade changed them from {before:?} to {after:?}"
        ));
    }
    if !history::ledger(&db, MIGRATIONS.block)
        .await?
        .iter()
        .any(|r| r.file_name == DEFAULT_DOCUMENTS && r.origin == Origin::Ran)
    {
        problems.push("existing documents: 003 is not recorded as ran".to_string());
    }
    Ok(())
}

async fn a_table_with_no_published_document_is_kept(
    backend: Backend,
    new_db: NewDatabase<'_>,
    status: &str,
    version: i64,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    before_003(&db, backend).await?;
    // The live terms deleted, leaving only one of its other rows.
    let (_, doc_type, title, content) = DEFAULTS[1];
    write_document(&db, doc_type, title, content, status, version).await?;
    let before = rows(&db).await?;

    upgrade(&db, backend).await?;

    let after = rows(&db).await?;
    if after != before {
        problems.push(format!(
            "only {status}: the upgrade changed the documents from {before:?} to {after:?}"
        ));
    }
    if !history::ledger(&db, MIGRATIONS.block)
        .await?
        .iter()
        .any(|r| r.file_name == DEFAULT_DOCUMENTS && r.origin == Origin::Ran)
    {
        problems.push(format!("only {status}: 003 is not recorded as ran"));
    }
    Ok(())
}

async fn a_deleted_type_stays_deleted(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    before_003(&db, backend).await?;
    // The privacy policy was deleted; only the terms remain.
    let (_, doc_type, title, content) = DEFAULTS[1];
    write_document(&db, doc_type, title, content, "published", 1).await?;
    let before = rows(&db).await?;

    upgrade(&db, backend).await?;

    let after = rows(&db).await?;
    if after != before {
        problems.push(format!(
            "deleted type: the upgrade changed the documents from {before:?} to {after:?}"
        ));
    }
    Ok(())
}

async fn an_empty_table_is_seeded_once(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    before_003(&db, backend).await?;

    // Every document deleted before this release: the upgrade seeds them, as
    // the next boot's `Init` did.
    upgrade(&db, backend).await?;
    check_defaults(&rows(&db).await?, "empty table", problems);

    // Every document deleted after 003 ran: nothing seeds them again.
    for (id, ..) in DEFAULTS {
        db.delete(TABLE, id)
            .await
            .map_err(|e| format!("delete {id}: {e}"))?;
    }
    upgrade(&db, backend).await?;
    let after_rerun = rows(&db).await?;
    if !after_rerun.is_empty() {
        problems.push(format!(
            "empty table: re-running the upgrade re-seeded {after_rerun:?}"
        ));
    }
    // The block's record lost: the runner plans against the ledger, which
    // records 003, so nothing is pending and only the record is restored.
    history::stamp(&db, MIGRATIONS.block, "").await?;
    upgrade(&db, backend).await?;
    let after_replan = rows(&db).await?;
    if !after_replan.is_empty() {
        problems.push(format!(
            "empty table: re-planning against the ledger re-seeded {after_replan:?}"
        ));
    }
    if history::current_hash(&db, MIGRATIONS.block).await?
        != history::todays_hash(&MIGRATIONS, backend)
    {
        problems.push("empty table: legalpages' record is not today's list".to_string());
    }
    Ok(())
}
