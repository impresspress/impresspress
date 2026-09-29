//! Moves each index whose name has uppercase letters to its lowercase name,
//! once, when the vector block starts.
//!
//! Index names used to admit `[A-Za-z0-9_]`. The database layer now accepts
//! only plain lowercase identifiers as table names, and every vector backend
//! addresses an index's tables by that rule, so an index created as `Docs`
//! (`impresspress__vector__Docs`) can no longer be opened, queried or
//! deleted. `vector.rename_index` is the one operation that accepts such a
//! legacy name: it moves the index's tables, entries and keyword search to the
//! lowercase name atomically.
//!
//! Every legacy name is moved, whether the registry
//! (`impresspress__vector__registry`) names it or only the backend's catalog
//! does (`vector.list_indexes`, which also lists indexes older than the
//! registry). A registry row follows its index: it is renamed to the
//! lowercase name.
//!
//! **Two rows that differ only by case** name ONE index: the SQLite family
//! (native and the browser's sql.js) compares table names case-insensitively,
//! so `Docs_meta` and `docs_meta` cannot both exist. Each row carries the
//! index's `model`, `dimensions` and `keyword_search`, and only one of them
//! can describe the tables that exist. The row kept is the one that does:
//! - rows that agree are duplicates, and the legacy one is removed;
//! - rows that differ only in `keyword_search` are told apart by the index
//!   itself (`vector.describe_index` reports whether it has keyword search):
//!   the matching row's metadata is kept under the lowercase name;
//! - rows that differ in `model` or `dimensions` cannot be told apart — no
//!   backend reports either — so both are left, and an error log names them
//!   for the operator.
//!
//! Idempotent, so every start runs it: a name already lowercase is skipped,
//! and `NotFound` from the rename with the lowercase index present means an
//! earlier start moved the index and stopped before its registry row, which is
//! then fixed.
//!
//! Failures: a transient failure reading the registry, the catalog or the
//! moved index (`Unavailable`) fails the block's `Init`, so the runtime
//! retries it. A refused move is logged and left for the next start, so one
//! unmovable index does not take the vector block down; that includes a
//! native busy database during the rename, which the vector service reports as
//! `Internal` because its errors have no transient kind.

use wafer_block::db::{Filter, FilterOp, SortField};
use wafer_core::clients::{database as db, vector as vclient};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::service::{vector_backend_available, REGISTRY_TABLE, TABLE_PREFIX};
use crate::{
    db_read::{self, Bound},
    util::RecordExt,
};

/// What [`move_legacy_index`] found for one legacy name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Moved {
    /// The index was moved to the lowercase name now.
    Now,
    /// The index was already under the lowercase name.
    Already,
    /// The backend refused the move; nothing changed.
    Refused(String),
}

/// One registry row's description of its index.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RegistryRow {
    name: String,
    model: String,
    dimensions: u64,
    keyword_search: bool,
}

impl RegistryRow {
    fn from_record(record: &db::Record) -> Self {
        Self {
            name: record.str_field("prefixed_name").to_string(),
            model: record.str_field("model").to_string(),
            dimensions: record.u64_field("dimensions"),
            keyword_search: record.bool_field("keyword_search"),
        }
    }

    /// Whether `self` and `other` say the same about their index.
    fn same_index_as(&self, other: &Self) -> bool {
        (&self.model, self.dimensions, self.keyword_search)
            == (&other.model, other.dimensions, other.keyword_search)
    }
}

/// Move every legacy-named index, and its registry row, to the lowercase
/// name.
pub(crate) async fn rename_legacy_indexes(ctx: &dyn Context) -> Result<(), WaferError> {
    if !vector_backend_available(ctx) {
        return Ok(());
    }
    let registered: Vec<RegistryRow> = db_read::list_bounded_sorted(
        ctx,
        REGISTRY_TABLE,
        Vec::new(),
        vec![SortField {
            field: "prefixed_name".to_string(),
            desc: false,
        }],
        Bound::Curated("vector indexes are registered from the vector admin surface"),
    )
    .await?
    .iter()
    .map(RegistryRow::from_record)
    .collect();
    let catalog = match vclient::list_indexes(ctx, TABLE_PREFIX).await {
        Ok(stems) => stems,
        Err(e) if e.code == ErrorCode::Unavailable => return Err(e),
        Err(e) => {
            tracing::error!(
                error = %e.message,
                "vector backend could not list its indexes; only registered indexes are \
                 checked for legacy names"
            );
            Vec::new()
        }
    };
    let row = |name: &str| registered.iter().find(|r| r.name == name).cloned();

    let mut legacy: Vec<&String> = registered
        .iter()
        .map(|r| &r.name)
        .chain(&catalog)
        .filter(|name| !wafer_block::db::is_plain_ident(name))
        .collect();
    legacy.sort();
    legacy.dedup();
    for name in legacy {
        let lowercase = name.to_ascii_lowercase();
        let (legacy_row, lowercase_row) = (row(name), row(&lowercase));
        // Twin rows that disagree on what no backend reports: decide nothing,
        // move nothing.
        if let (Some(upper), Some(lower)) = (&legacy_row, &lowercase_row) {
            if (&upper.model, upper.dimensions) != (&lower.model, lower.dimensions) {
                tracing::error!(
                    index = %lowercase,
                    rows = ?[upper, lower],
                    "two vector registry rows name one index but disagree on its model or \
                     dimensions, which the index cannot confirm; both are left and the index \
                     is not moved — delete the wrong row"
                );
                continue;
            }
        }
        match move_legacy_index(ctx, name, &lowercase).await? {
            Moved::Now => {
                tracing::info!(from = %name, to = %lowercase, "vector index moved to its lowercase name");
            }
            Moved::Already => {}
            Moved::Refused(reason) => {
                tracing::error!(
                    index = %name,
                    to = %lowercase,
                    reason = %reason,
                    "vector index could not be moved to its lowercase name; the next start tries again"
                );
                continue;
            }
        }
        match (legacy_row, lowercase_row) {
            (Some(_), None) => rename_row(ctx, name, &lowercase).await?,
            (Some(upper), Some(lower)) => resolve_twin_rows(ctx, &upper, &lower).await?,
            (None, _) => {}
        }
    }
    Ok(())
}

/// Move the index stored as `legacy` to `lowercase`.
pub(crate) async fn move_legacy_index(
    ctx: &dyn Context,
    legacy: &str,
    lowercase: &str,
) -> Result<Moved, WaferError> {
    match vclient::rename_index(ctx, legacy, lowercase).await {
        Ok(()) => Ok(Moved::Now),
        // Nothing is stored under the legacy spelling: an earlier start moved
        // it, or it was always stored lowercase under a legacy registry row.
        Err(e) if e.code == ErrorCode::NotFound => match vclient::count(ctx, lowercase).await {
            Ok(_) => Ok(Moved::Already),
            Err(e) if e.code == ErrorCode::NotFound => Ok(Moved::Refused(format!(
                "no index is stored under {legacy:?} or {lowercase:?}"
            ))),
            Err(e) => Err(e),
        },
        Err(e) if e.code == ErrorCode::Unavailable => Err(e),
        Err(e) => Ok(Moved::Refused(e.message)),
    }
}

/// Keep the one of two rows naming the moved index (`lower` holds its name)
/// that describes it; `upper` and `lower` agree on model and dimensions.
async fn resolve_twin_rows(
    ctx: &dyn Context,
    upper: &RegistryRow,
    lower: &RegistryRow,
) -> Result<(), WaferError> {
    let keep_upper = if upper.same_index_as(lower) {
        false
    } else {
        match vclient::describe_index(ctx, &lower.name).await {
            Ok(described) if described.exists => described.keyword_search == upper.keyword_search,
            Ok(_) => {
                tracing::error!(index = %lower.name, "the moved vector index is not there to describe; both registry rows are left");
                return Ok(());
            }
            Err(e) if e.code == ErrorCode::Unavailable => return Err(e),
            Err(e) => {
                tracing::error!(
                    index = %lower.name,
                    error = %e.message,
                    rows = ?[upper, lower],
                    "two vector registry rows name one index and disagree on keyword search, \
                     and the backend cannot describe the index; both are left — delete the \
                     wrong row"
                );
                return Ok(());
            }
        }
    };
    if keep_upper {
        delete_row(ctx, &lower.name).await?;
        rename_row(ctx, &upper.name, &lower.name).await?;
    } else {
        delete_row(ctx, &upper.name).await?;
    }
    tracing::info!(
        index = %lower.name,
        kept = ?if keep_upper { upper } else { lower },
        removed = ?if keep_upper { lower } else { upper },
        "two vector registry rows named one index; the row that describes it is kept under \
         the lowercase name"
    );
    Ok(())
}

fn row_named(name: &str) -> Vec<Filter> {
    vec![Filter {
        field: "prefixed_name".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(name),
    }]
}

async fn rename_row(ctx: &dyn Context, from: &str, to: &str) -> Result<(), WaferError> {
    db::update_by_filters(
        ctx,
        REGISTRY_TABLE,
        row_named(from),
        crate::util::json_map(serde_json::json!({ "prefixed_name": to })),
    )
    .await
}

async fn delete_row(ctx: &dyn Context, name: &str) -> Result<(), WaferError> {
    db::delete_by_filters(ctx, REGISTRY_TABLE, row_named(name)).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_block::{db::SortField, wire::database::OnConflict};
    use wafer_block_sqlite::vector::SqliteVecService;
    use wafer_core::{
        clients::{database as db, vector as vclient},
        interfaces::vector::service::{SearchMode, VectorEntry},
        service_blocks::vector::VectorBlock,
    };

    use super::{move_legacy_index, rename_legacy_indexes, Moved, REGISTRY_TABLE};
    use crate::{
        db_read::{self, Bound},
        test_support::TestContext,
        util::RecordExt,
    };

    const LEGACY: &str = "impresspress__vector__Docs";
    const LOWERCASE: &str = "impresspress__vector__docs";

    fn blob(v: [f32; 3]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    /// A SQLite file holding one index as the code before lowercase names
    /// wrote it: the statements `VectorIndexSchema` rendered then, with the
    /// mixed-case name spliced in unquoted, and two entries written the way
    /// its upsert wrote them.
    fn legacy_index_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
        // Registers sqlite-vec as an auto-extension for every connection
        // opened after it.
        drop(SqliteVecService::open_in_memory().expect("register sqlite-vec"));
        let path = dir.path().join("vectors.sqlite3");
        let conn = rusqlite::Connection::open(&path).expect("open the vector file");
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE {LEGACY}_vec USING vec0(embedding float[3]);\n\
             CREATE TABLE {LEGACY}_meta(\n\
                id TEXT PRIMARY KEY,\n\
                rowid INTEGER NOT NULL,\n\
                metadata TEXT,\n\
                text TEXT\n\
             );"
        ))
        .expect("create the legacy index");
        for (rowid, id, vector) in [(1_i64, "a", [1.0, 0.0, 0.0]), (2, "b", [0.0, 1.0, 0.0])] {
            conn.execute(
                &format!(
                    "INSERT INTO {LEGACY}_meta(id, rowid, metadata, text) VALUES (?1, ?2, ?3, '')"
                ),
                rusqlite::params![id, rowid, format!("{{\"doc\":\"{id}\"}}")],
            )
            .expect("legacy meta row");
            conn.execute(
                &format!("INSERT INTO {LEGACY}_vec(rowid, embedding) VALUES (?1, ?2)"),
                rusqlite::params![rowid, blob(vector)],
            )
            .expect("legacy vector row");
        }
        path
    }

    /// A context with the vector block's schema and a real
    /// `wafer-run/vector` over `SqliteVecService` on `path`.
    async fn ctx_over(path: &std::path::Path) -> TestContext {
        let service =
            SqliteVecService::new(rusqlite::Connection::open(path).expect("open the vector file"))
                .expect("vector service");
        let mut ctx = TestContext::with_vector().await;
        ctx.register_block(
            "wafer-run/vector",
            Arc::new(VectorBlock::new(Arc::new(service))),
        );
        ctx
    }

    async fn register(ctx: &TestContext, name: &str, model: &str) {
        register_row(ctx, name, model, 3, false).await;
    }

    async fn register_row(
        ctx: &TestContext,
        name: &str,
        model: &str,
        dimensions: u32,
        keyword_search: bool,
    ) {
        db::upsert(
            ctx,
            REGISTRY_TABLE,
            vec![
                ("prefixed_name".to_string(), serde_json::json!(name)),
                ("model".to_string(), serde_json::json!(model)),
                ("dimensions".to_string(), serde_json::json!(dimensions)),
                (
                    "keyword_search".to_string(),
                    serde_json::json!(i64::from(keyword_search)),
                ),
            ],
            vec!["prefixed_name".to_string()],
            OnConflict::SetColumns(vec!["model".to_string()]),
        )
        .await
        .expect("registry row");
    }

    async fn registry(ctx: &TestContext) -> Vec<(String, String)> {
        db_read::list_bounded_sorted(
            ctx,
            REGISTRY_TABLE,
            Vec::new(),
            vec![SortField {
                field: "prefixed_name".to_string(),
                desc: false,
            }],
            Bound::Curated("a handful of rows seeded by the test"),
        )
        .await
        .expect("registry")
        .iter()
        .map(|r| {
            (
                r.str_field("prefixed_name").to_string(),
                r.str_field("model").to_string(),
            )
        })
        .collect()
    }

    async fn nearest(ctx: &TestContext, index: &str, v: [f32; 3]) -> Vec<String> {
        vclient::query(ctx, index, v.to_vec(), 2, None, SearchMode::Vector, None)
            .await
            .expect("query")
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    /// An index stored under a mixed-case name is moved to its lowercase
    /// name with the entries it held, its registry row follows, and the
    /// index is then usable under that name: its old entries answer a
    /// query, a new entry can be written. A second start changes nothing.
    #[tokio::test]
    async fn a_mixed_case_index_answers_under_its_lowercase_name_with_its_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = legacy_index_file(&dir);
        let ctx = ctx_over(&path).await;
        register(&ctx, LEGACY, "bge-m3").await;

        rename_legacy_indexes(&ctx).await.expect("startup step");

        assert_eq!(
            registry(&ctx).await,
            vec![(LOWERCASE.to_string(), "bge-m3".to_string())]
        );
        assert_eq!(nearest(&ctx, LOWERCASE, [0.0, 1.0, 0.0]).await[0], "b");
        vclient::upsert(
            &ctx,
            LOWERCASE,
            vec![VectorEntry {
                id: "c".to_string(),
                vector: vec![0.0, 0.0, 1.0],
                metadata: None,
                text: None,
            }],
        )
        .await
        .expect("a new entry");
        assert_eq!(vclient::count(&ctx, LOWERCASE).await.expect("count"), 3);
        assert_eq!(nearest(&ctx, LOWERCASE, [1.0, 0.0, 0.0]).await[0], "a");

        rename_legacy_indexes(&ctx).await.expect("a second start");
        assert_eq!(
            registry(&ctx).await,
            vec![(LOWERCASE.to_string(), "bge-m3".to_string())]
        );
        assert_eq!(vclient::count(&ctx, LOWERCASE).await.expect("count"), 3);
    }

    /// A start that moved the index and stopped before renaming its registry
    /// row finishes the job: the backend reports no index under the legacy
    /// name, and the lowercase index exists.
    #[tokio::test]
    async fn a_move_interrupted_before_the_registry_row_is_finished() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = legacy_index_file(&dir);
        let ctx = ctx_over(&path).await;
        vclient::rename_index(&ctx, LEGACY, LOWERCASE)
            .await
            .expect("the backend half of the move");
        register(&ctx, LEGACY, "bge-m3").await;

        rename_legacy_indexes(&ctx).await.expect("startup step");

        assert_eq!(
            registry(&ctx).await,
            vec![(LOWERCASE.to_string(), "bge-m3".to_string())]
        );
        assert_eq!(vclient::count(&ctx, LOWERCASE).await.expect("count"), 2);
    }

    /// Whether `name`'s registry row says the index has keyword search.
    async fn keyword_search_of(ctx: &TestContext, name: &str) -> bool {
        db::get_by_field(
            ctx,
            REGISTRY_TABLE,
            "prefixed_name",
            serde_json::json!(name),
        )
        .await
        .expect("registry row")
        .bool_field("keyword_search")
    }

    /// Two rows that agree are one index registered twice: the index is
    /// moved, one row remains under the lowercase name, and the entries
    /// answer there. The tables exist once — SQLite compares their names
    /// without case — so nothing else was ever there to keep.
    #[tokio::test]
    async fn agreeing_case_twin_rows_are_one_row_after_the_move() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = legacy_index_file(&dir);
        let ctx = ctx_over(&path).await;
        register(&ctx, LEGACY, "bge-m3").await;
        register(&ctx, LOWERCASE, "bge-m3").await;

        rename_legacy_indexes(&ctx).await.expect("startup step");

        assert_eq!(
            registry(&ctx).await,
            vec![(LOWERCASE.to_string(), "bge-m3".to_string())]
        );
        assert_eq!(vclient::count(&ctx, LOWERCASE).await.expect("count"), 2);
        assert_eq!(nearest(&ctx, LOWERCASE, [1.0, 0.0, 0.0]).await[0], "a");
    }

    /// Twin rows that disagree on keyword search are told apart by the index:
    /// it has no keyword search, which is what the uppercase row says, so
    /// that row's description is kept under the lowercase name and the other
    /// is removed.
    #[tokio::test]
    async fn the_twin_row_that_matches_the_index_is_the_one_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = legacy_index_file(&dir);
        let ctx = ctx_over(&path).await;
        register_row(&ctx, LEGACY, "bge-m3", 3, false).await;
        register_row(&ctx, LOWERCASE, "bge-m3", 3, true).await;

        rename_legacy_indexes(&ctx).await.expect("startup step");

        assert_eq!(
            registry(&ctx).await,
            vec![(LOWERCASE.to_string(), "bge-m3".to_string())]
        );
        assert!(
            !keyword_search_of(&ctx, LOWERCASE).await,
            "the kept row is the one that matches the index"
        );
        assert_eq!(vclient::count(&ctx, LOWERCASE).await.expect("count"), 2);
    }

    /// Twin rows that disagree on what no backend reports (dimensions here)
    /// cannot be told apart: both are left and the index is not moved.
    #[tokio::test]
    async fn twin_rows_the_index_cannot_decide_between_are_left() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = legacy_index_file(&dir);
        let ctx = ctx_over(&path).await;
        register_row(&ctx, LEGACY, "bge-m3", 3, false).await;
        register_row(&ctx, LOWERCASE, "bge-m3", 8, false).await;

        rename_legacy_indexes(&ctx).await.expect("startup step");

        assert_eq!(
            registry(&ctx).await,
            vec![
                (LEGACY.to_string(), "bge-m3".to_string()),
                (LOWERCASE.to_string(), "bge-m3".to_string()),
            ]
        );
        // Not moved: the index is still stored under the legacy name.
        vclient::rename_index(&ctx, LEGACY, LOWERCASE)
            .await
            .expect("the index is still under its legacy name");
    }

    /// An index the registry does not name — one older than the registry,
    /// which the index list finds in the backend's catalog — is moved too,
    /// and no registry row is invented for it.
    #[tokio::test]
    async fn an_unregistered_mixed_case_index_is_moved_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = legacy_index_file(&dir);
        let ctx = ctx_over(&path).await;

        rename_legacy_indexes(&ctx).await.expect("startup step");

        assert_eq!(vclient::count(&ctx, LOWERCASE).await.expect("count"), 2);
        assert_eq!(nearest(&ctx, LOWERCASE, [0.0, 1.0, 0.0]).await[0], "b");
        assert_eq!(registry(&ctx).await, Vec::new());
    }

    /// A name no backend stores under either spelling is refused and left.
    #[tokio::test]
    async fn a_name_with_no_index_behind_it_is_left_as_it_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = legacy_index_file(&dir);
        let ctx = ctx_over(&path).await;
        let ghost = "impresspress__vector__Ghost";

        assert!(matches!(
            move_legacy_index(&ctx, ghost, "impresspress__vector__ghost")
                .await
                .expect("no fault"),
            Moved::Refused(_)
        ));
    }
}
