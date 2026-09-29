//! Table-name constants and helpers for the impresspress/vector block.
//!
//! Vector indexes are stored as tables in the underlying database with a
//! fixed prefix. User-facing index names (e.g. `"docs"`) are mapped to the
//! prefixed storage name (e.g. `"impresspress__vector__docs"`) at the block
//! boundary — no magic mapping elsewhere in the stack.

use wafer_block::db::SortField;
use wafer_core::clients::{database as db, vector as vclient};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    db_read::{self, Bound},
    util::RecordExt,
};

/// Per-row data fed to the vector index list table renderer.
///
/// Lives in the service layer alongside the loader (`list_index_rows`) so
/// the data shape and its query stay co-located. The UI layer imports it
/// for rendering only.
#[derive(Clone, Debug)]
pub struct IndexRow {
    pub name: String,
    pub model: String,
    pub dimensions: u32,
    /// `None` when there is no count to report: this runtime has no vector
    /// backend, or the backend holds no index by this name.
    pub vector_count: Option<u64>,
    pub keyword_search: bool,
}

/// All tables created for a vector index are named with this prefix.
pub const TABLE_PREFIX: &str = "impresspress__vector__";

/// Per-index metadata registry table.
///
/// One row per index, keyed by the prefixed (storage) name. Keeping this
/// separate from `sqlite_master` lets us remember per-index knobs — the
/// model to re-embed text with, and whether keyword search was enabled —
/// without spelunking through DDL on every query.
///
/// Schema lives in `migrations/001_vector_schema.{sqlite,postgres}.sql` and
/// is applied at block Init via `apply_if_blessed`. The column layout is:
/// `(prefixed_name TEXT PK, model TEXT, dimensions INTEGER,
/// keyword_search INTEGER)`.
pub(crate) const REGISTRY_TABLE: &str = "impresspress__vector__registry";

/// Convert a user-facing index name (e.g. `"docs"`) into the fully prefixed
/// name that is actually stored in the database (e.g. `"impresspress__vector__docs"`).
pub fn prefixed_index_name(user_name: &str) -> String {
    format!("{TABLE_PREFIX}{user_name}")
}

/// Strip the prefix for display to users. Returns the input unchanged if it
/// does not carry the prefix.
pub fn display_index_name(stored: &str) -> &str {
    stored.strip_prefix(TABLE_PREFIX).unwrap_or(stored)
}

/// True when the `wafer-run/vector` backend block is registered on this
/// runtime.
///
/// Native impresspress ships no native vector engine (see the module docs on
/// `pages_ui::index_list_page`) — the `wafer-run/vector` service block is
/// only present on the browser-WASM build or when a runtime config wires
/// one in. Without it, every `vclient::*` call in `pages.rs` fails with
/// `ErrorCode::NotFound: block 'wafer-run/vector' not found`, which is
/// indistinguishable (same error code) from a genuine "index not found".
/// Checking `registered_blocks()` up front — instead of pattern-matching on
/// that ambiguous `NotFound`, which the API's per-index-op error branches
/// already use for a different meaning — lets both the UI page and every
/// JSON API handler detect the missing backend unambiguously and degrade
/// gracefully. Single source of truth so the two call sites can't drift.
pub fn vector_backend_available(ctx: &dyn Context) -> bool {
    ctx.registered_blocks()
        .iter()
        .any(|b| b.name == "wafer-run/vector")
}

/// The longest suffix any backend appends to an index's prefixed name to
/// form one of its tables: `_vectors` (the browser backend's
/// `{name}_vectors`; the native sqlite-vec builders use `_vec`, `_meta` and
/// `_fts`).
const LONGEST_TABLE_SUFFIX: &str = "_vectors";

/// The longest index name whose every table is still a plain identifier.
pub const MAX_INDEX_NAME_LEN: usize =
    wafer_block::db::MAX_IDENT_LEN - TABLE_PREFIX.len() - LONGEST_TABLE_SUFFIX.len();

/// Validate a user-facing index name: 1 to [`MAX_INDEX_NAME_LEN`] bytes of
/// `[a-z0-9_]`.
///
/// Every table an index owns is named `{TABLE_PREFIX}{name}{suffix}`, and the
/// database layer admits only plain identifiers — lowercase `[a-z0-9_]`, at
/// most `wafer_block::db::MAX_IDENT_LEN` bytes
/// (`wafer_block::db::is_plain_ident`) — refusing anything else rather than
/// rewriting it. So a name is valid exactly when its longest table name is a
/// plain identifier, which is the check below: an uppercase or hyphenated
/// name, or one long enough that PostgreSQL would truncate a table name, is
/// refused here at the route boundary instead of failing inside the backend
/// halfway through creating the index.
///
/// Returns the name on success so callers can chain it at the use site.
pub fn validate_index_name(name: &str) -> Result<&str, WaferError> {
    if name.is_empty() {
        return Err(WaferError {
            code: ErrorCode::InvalidArgument,
            message: "index name must not be empty".to_string(),
            meta: vec![],
        });
    }
    let longest_table = format!("{TABLE_PREFIX}{name}{LONGEST_TABLE_SUFFIX}");
    if !wafer_block::db::is_plain_ident(&longest_table) {
        return Err(WaferError {
            code: ErrorCode::InvalidArgument,
            message: format!(
                "invalid index name '{name}': only [a-z0-9_] allowed, at most \
                 {MAX_INDEX_NAME_LEN} characters"
            ),
            meta: vec![],
        });
    }
    Ok(name)
}

/// Map one registry record into an `IndexRow`, asking the vector service
/// for the live vector count. `Ok(None)` if the row has no `prefixed_name`.
/// Shared between the list and detail loaders so the column-extraction
/// quirks (TEXT-as-string round-trip from the SQLite service) live in
/// exactly one place.
///
/// A count the service refused is an error, carried back with its code: a
/// refusal shown as "0 vectors" is a claim about the index nobody checked.
async fn map_index_row(
    ctx: &dyn Context,
    rec: &db::Record,
) -> Result<Option<IndexRow>, WaferError> {
    let storage_name = rec
        .data
        .get("prefixed_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if storage_name.is_empty() {
        return Ok(None);
    }
    let model = rec
        .data
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // The SQLite service stores all auto-created columns as TEXT, so the
    // numeric registry values arrive as JSON strings (e.g. `"384"`)
    // rather than numbers. The RecordExt accessors handle both shapes.
    let dimensions = rec.u64_field("dimensions") as u32;
    let keyword_search = rec.bool_field("keyword_search");

    // Count through the vector service boundary — the same `vclient::count`
    // the API `stats()` route uses — instead of reaching around it with a
    // `db::count` on the backend-private `{storage}_meta` table. No backend,
    // or a backend holding no such index, has no count to report; any other
    // failure is the caller's to answer.
    let count = if vector_backend_available(ctx) {
        match vclient::count(ctx, &storage_name).await {
            Ok(n) => Some(n),
            Err(e) if e.code == ErrorCode::NotFound => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    Ok(Some(IndexRow {
        name: storage_name,
        model,
        dimensions,
        vector_count: count,
        keyword_search,
    }))
}

/// Read every registered vector index plus its current vector count
/// (via the vector service). Caller decides what to do with errors —
/// returning `Result` (rather than swallowing) keeps the helper testable
/// in isolation, while the page handler maps any failure to the empty
/// state.
///
/// On a fresh database the registry table doesn't exist yet; the
/// SQLite service returns an empty `RecordList` for unknown
/// collections, so `db::list` gives us `Ok(empty)` rather than an
/// error. Per-index counts come from `vclient::count`, which degrades
/// to 0 in `map_index_row` when the backend is absent.
pub async fn list_index_rows(ctx: &dyn Context) -> Result<Vec<IndexRow>, WaferError> {
    let records = db_read::list_bounded_sorted(
        ctx,
        REGISTRY_TABLE,
        vec![],
        vec![SortField {
            field: "prefixed_name".to_string(),
            desc: false,
        }],
        Bound::Curated("vector indexes are registered from the vector admin surface"),
    )
    .await?;

    let mut rows = Vec::with_capacity(records.len());
    for rec in records {
        if let Some(row) = map_index_row(ctx, &rec).await? {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Look up a single registry row by its prefixed (storage) name. Returns
/// `None` if no such row exists. Used by the detail page handler.
pub async fn get_index_row(
    ctx: &dyn Context,
    storage_name: &str,
) -> Result<Option<IndexRow>, WaferError> {
    let rec = match db::get_by_field(
        ctx,
        REGISTRY_TABLE,
        "prefixed_name",
        serde_json::Value::String(storage_name.to_string()),
    )
    .await
    {
        Ok(r) => r,
        Err(e) if e.code == ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    map_index_row(ctx, &rec).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_roundtrip() {
        let p = prefixed_index_name("docs");
        assert_eq!(p, "impresspress__vector__docs");
        assert_eq!(display_index_name(&p), "docs");
    }

    #[test]
    fn display_passes_through_unprefixed() {
        assert_eq!(display_index_name("other"), "other");
    }
}

#[cfg(test)]
mod tests_validate {
    use super::*;

    #[test]
    fn accepts_valid_names() {
        assert!(validate_index_name("docs").is_ok());
        assert!(validate_index_name("my_index").is_ok());
        assert!(validate_index_name("index_42").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_index_name("").is_err());
    }

    #[test]
    fn rejects_special_chars() {
        assert!(validate_index_name("docs; DROP TABLE users").is_err());
        assert!(validate_index_name("doc's").is_err());
        assert!(validate_index_name("my.index").is_err());
        assert!(
            validate_index_name("index-42").is_err(),
            "the database layer refuses a hyphenated table name"
        );
    }

    /// The database layer refuses uppercase table names, so an index whose
    /// name has any would be accepted here and refused by the backend.
    #[test]
    fn rejects_uppercase() {
        assert!(validate_index_name("Docs").is_err());
        assert!(validate_index_name("DOCS").is_err());
    }

    /// The longest admitted name still gives plain identifiers for every
    /// table any backend creates; one byte more does not.
    #[test]
    fn the_length_cap_keeps_every_table_name_a_plain_identifier() {
        let longest = "a".repeat(MAX_INDEX_NAME_LEN);
        assert!(validate_index_name(&longest).is_ok());
        for suffix in ["_vec", "_meta", "_fts", "_vectors"] {
            let table = format!("{}{suffix}", prefixed_index_name(&longest));
            assert!(wafer_block::db::is_plain_ident(&table), "{table}");
        }
        assert!(validate_index_name(&"a".repeat(MAX_INDEX_NAME_LEN + 1)).is_err());
    }
}
