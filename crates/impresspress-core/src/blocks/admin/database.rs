use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};
use wafer_sql_utils::{introspect, Backend};

use crate::{
    blocks::crud,
    http::{err_bad_request, err_forbidden, err_not_found, ok_json},
};

/// Lightweight per-table summary: name + row count. Shared by the JSON
/// `GET /b/admin/api/database/tables` handler and the SSR database page's
/// left-pane list so both run the same introspection routine.
pub(in crate::blocks::admin) struct TableSummary {
    pub name: String,
    pub row_count: i64,
}

/// A single column's introspected metadata. Shared by the JSON
/// `GET /b/admin/api/database/tables/{name}/columns` handler and the SSR
/// schema panel.
pub(in crate::blocks::admin) struct ColumnInfo {
    pub name: String,
    pub ty: String,
    pub notnull: bool,
    pub pk: bool,
    /// The column's default expression, if any (SQLite `dflt_value`).
    pub default_value: Option<String>,
}

/// Why [`introspect_columns`] has no schema to show.
pub(in crate::blocks::admin) enum IntrospectError {
    /// The name is not an identifier the backend can quote. It is user input
    /// (URL path / `?table=`), so this is the caller's mistake.
    InvalidName,
    /// The backend reports no columns for the name: there is no such table.
    NoSuchTable,
    /// A read failed. Never shown as an empty schema or a zero count.
    Read(WaferError),
}

/// Run the backend table count for one table name.
///
/// The name has already been through the backend's quoting (it comes from the
/// backend's own table listing, or from a column read that found the table),
/// so a build error here is a fault, not user input.
async fn table_row_count(ctx: &dyn Context, name: &str) -> Result<i64, WaferError> {
    let count_sql = introspect::build_table_row_count(name, crate::db_backend(ctx).await?)
        .map_err(|e| WaferError::new(ErrorCode::Internal, format!("count {name}: {e}")))?;
    let rows = db::query_raw(ctx, &count_sql, &[]).await?;
    rows.first()
        .and_then(|r| r.data.get("cnt").and_then(|v| v.as_i64()))
        .ok_or_else(|| {
            WaferError::new(
                ErrorCode::Internal,
                format!("count {name}: the backend returned no count"),
            )
        })
}

/// List every table with its row count, sorted by name.
///
/// Single source of truth for the table-browser introspection shared by the
/// JSON API and the SSR page. The per-table COUNT is issued sequentially —
/// concurrent counts on a single backend connection (the SQLite case) can
/// deadlock, and the row is read-once-per-page, so the dedupe (not the
/// fan-out) is the win here.
///
/// A failed listing or count is an error: an empty list or a `0` count would
/// read as an empty database.
pub(in crate::blocks::admin) async fn introspect_table_summaries(
    ctx: &dyn Context,
) -> Result<Vec<TableSummary>, WaferError> {
    let sql = introspect::build_list_tables(crate::db_backend(ctx).await?);
    let records = db::query_raw(ctx, &sql, &[]).await?;
    let mut out = Vec::with_capacity(records.len());
    for r in &records {
        let name = r
            .data
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let row_count = table_row_count(ctx, &name).await?;
        out.push(TableSummary { name, row_count });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Introspect one table's columns plus its row count. `table` is untrusted
/// (URL path / selected name).
pub(in crate::blocks::admin) async fn introspect_columns(
    ctx: &dyn Context,
    table: &str,
) -> Result<(Vec<ColumnInfo>, i64), IntrospectError> {
    let backend = crate::db_backend(ctx)
        .await
        .map_err(IntrospectError::Read)?;
    let (info_sql, info_args) =
        introspect::build_table_info(table, backend).map_err(|_| IntrospectError::InvalidName)?;
    let columns = db::query_raw(ctx, &info_sql, &info_args)
        .await
        .map_err(IntrospectError::Read)?;
    // Both backends answer the column read for an unknown table with no rows
    // rather than an error; counting it would fail, so stop here.
    if columns.is_empty() {
        return Err(IntrospectError::NoSuchTable);
    }
    let cols = columns
        .iter()
        .map(|c| ColumnInfo {
            name: c
                .data
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            ty: c
                .data
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            notnull: c.data.get("notnull").and_then(|v| v.as_i64()).unwrap_or(0) == 1,
            pk: c.data.get("pk").and_then(|v| v.as_i64()).unwrap_or(0) == 1,
            default_value: c
                .data
                .get("dflt_value")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
        .collect();
    let row_count = table_row_count(ctx, table)
        .await
        .map_err(IntrospectError::Read)?;
    Ok((cols, row_count))
}

/// `GET /b/admin/api/database/info`.
pub(super) async fn handle_info(ctx: &dyn Context) -> OutputStream {
    let backend = match crate::db_backend(ctx).await {
        Ok(backend) => backend,
        Err(e) => return crud::db_error_internal(e, "Could not read the database backend"),
    };
    let sql = introspect::build_list_tables(backend);
    let tables = match db::query_raw(ctx, &sql, &[]).await {
        Ok(t) => t,
        Err(e) => return crud::db_error_internal(e, "Database error"),
    };

    let table_names: Vec<&str> = tables
        .iter()
        .filter_map(|r| r.data.get("name").and_then(|v| v.as_str()))
        .collect();

    ok_json(&serde_json::json!({
        "type": backend_name(backend),
        "tables": table_names,
        "table_count": table_names.len()
    }))
}

/// Lowercase dialect name for the JSON `type` field, matching the
/// `WAFER_RUN_SHARED__DATABASE__BACKEND` config var values.
fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Sqlite => "sqlite",
        Backend::Postgres => "postgres",
    }
}

/// `GET /b/admin/api/database/tables`.
pub(super) async fn handle_tables(ctx: &dyn Context) -> OutputStream {
    let summaries = match introspect_table_summaries(ctx).await {
        Ok(summaries) => summaries,
        Err(e) => return crud::db_error_internal(e, "Could not list the tables"),
    };
    let table_info: Vec<serde_json::Value> = summaries
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "row_count": t.row_count,
            })
        })
        .collect();
    ok_json(&serde_json::json!(table_info))
}

/// `GET /b/admin/api/database/tables/{name}/columns`. `{name}` is read only
/// as the route table bound it.
pub(super) async fn handle_columns(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let table_name = match crud::path_var(msg, "name", "Missing table name") {
        Ok(value) => value,
        Err(response) => return response,
    };

    let columns = match introspect_columns(ctx, table_name).await {
        Ok((columns, _row_count)) => columns,
        // The table name is user input from the URL path; an invalid
        // identifier is a bad request, not a server error.
        Err(IntrospectError::InvalidName) => return err_bad_request("Invalid table name"),
        Err(IntrospectError::NoSuchTable) => return err_not_found("Table not found"),
        Err(IntrospectError::Read(e)) => {
            return crud::db_error_internal(e, "Could not read the table's columns")
        }
    };
    let col_info: Vec<serde_json::Value> = columns
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "type": c.ty,
                "notnull": c.notnull,
                "pk": c.pk,
                "default_value": c.default_value,
            })
        })
        .collect();

    ok_json(&serde_json::json!({"table": table_name, "columns": col_info}))
}

/// Why a SQL query was rejected, with the right HTTP status mapping
/// for `handle_query` (JSON API) and the SSR fragment handler in
/// `pages::database` to use without reading message text.
#[derive(Debug)]
pub(in crate::blocks::admin) enum QueryValidationError {
    /// Multi-statement queries or write/control keywords — caller should
    /// return HTTP 403.
    Forbidden(String),
    /// Wrong shape (unknown first word, unsafe PRAGMA name) — caller
    /// should return HTTP 400.
    BadRequest(String),
}

impl QueryValidationError {
    pub(in crate::blocks::admin) fn message(&self) -> &str {
        match self {
            Self::Forbidden(m) | Self::BadRequest(m) => m,
        }
    }
}

/// Validate that `query` is a read-only SQL statement we will execute.
///
/// Accepts: SELECT / PRAGMA (whitelisted) / EXPLAIN / WITH.
/// Rejects: multi-statement (`;`), any write keyword (whole-word match),
/// unsafe PRAGMAs, and any statement naming a
/// [`crate::secret_tables::SECRET_TABLES`] table.
///
/// The secret-table refusal is here, in the validator, rather than over the
/// result set: `db::query_raw` returns records keyed by the column name the
/// query chose, so a mask keyed on `(table, column)` is defeated by
/// `SELECT value AS v`. Refusing before execution is the only rule the query
/// text cannot be reshaped around — see the `secret_tables` module docs and
/// `tests/admin/sql_explorer_secrets.rs`.
///
/// Used by both the JSON API (`POST /b/admin/api/database/query`) and the
/// admin SSR page handler (`POST /b/admin/database/query`). Single
/// source of truth — do not duplicate this logic.
pub(in crate::blocks::admin) fn validate_readonly_query(
    query: &str,
) -> Result<(), QueryValidationError> {
    let trimmed = query.trim();

    // Strip one trailing `;` (and any whitespace after it) before the
    // multi-statement check. Editors frequently auto-append a terminator,
    // and the no-semicolon rule exists to block *piggy-backed* writes
    // like `SELECT 1; DROP TABLE x` — a lone terminator carries none of
    // that risk and produces a footgun otherwise. After stripping, a
    // remaining `;` means there's more than one statement and we reject.
    let trimmed = trimmed
        .strip_suffix(';')
        .map(|s| s.trim_end())
        .unwrap_or(trimmed);

    // Reject multi-statement queries (prevent piggy-backed writes).
    if trimmed.contains(';') {
        return Err(QueryValidationError::Forbidden(
            "Multi-statement queries are not allowed".to_string(),
        ));
    }

    // A query that names a table holding authentication material is refused
    // whatever verb it uses and whatever it would have done with the rows.
    // Placed ahead of the keyword scan, the PRAGMA whitelist and the
    // first-word check so the refusal never depends on any of them reading the
    // statement the way the backend will: an `EXPLAIN`, a `PRAGMA`, or a shape
    // none of them recognise is refused here just the same.
    if crate::secret_tables::rejects_unicode_escape(trimmed) {
        return Err(QueryValidationError::Forbidden(
            "Unicode-escaped identifiers and string constants (U&\"…\" / U&'…') are not \
             allowed: they can spell a table name this validator would not see"
                .to_string(),
        ));
    }
    if let Some(entry) = crate::secret_tables::secret_table_named_in(trimmed) {
        return Err(QueryValidationError::Forbidden(entry.refusal()));
    }

    let query_upper = trimmed.to_uppercase();

    const FORBIDDEN_KEYWORDS: &[&str] = &[
        "INSERT",
        "UPDATE",
        "DELETE",
        "DROP",
        "ALTER",
        "CREATE",
        "REPLACE",
        "ATTACH",
        "DETACH",
        "REINDEX",
        "VACUUM",
        "SAVEPOINT",
        "RELEASE",
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "RETURNING",
        // SEC-052: reject WITH RECURSIVE — unbounded recursive CTEs are a
        // cheap DoS vector against the admin SQL explorer. A plain
        // (non-recursive) WITH is still allowed via the first-word check.
        "RECURSIVE",
    ];
    for keyword in FORBIDDEN_KEYWORDS {
        let upper = query_upper.as_str();
        let kw = *keyword;
        let mut start = 0;
        while let Some(pos) = upper[start..].find(kw) {
            let abs_pos = start + pos;
            let before_ok = abs_pos == 0 || !upper.as_bytes()[abs_pos - 1].is_ascii_alphanumeric();
            let after_pos = abs_pos + kw.len();
            let after_ok =
                after_pos >= upper.len() || !upper.as_bytes()[after_pos].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return Err(QueryValidationError::Forbidden(format!(
                    "{keyword} is not allowed in read-only queries"
                )));
            }
            start = abs_pos + kw.len();
        }
    }

    let first_word = query_upper.split_whitespace().next().unwrap_or("");

    if first_word == "PRAGMA" {
        const SAFE_PRAGMAS: &[&str] = &[
            "TABLE_INFO",
            "TABLE_LIST",
            "TABLE_XINFO",
            "INDEX_LIST",
            "INDEX_INFO",
            "FOREIGN_KEY_LIST",
            "DATABASE_LIST",
            "COMPILE_OPTIONS",
            "INTEGRITY_CHECK",
            "QUICK_CHECK",
            "PAGE_COUNT",
            "PAGE_SIZE",
            "FREELIST_COUNT",
        ];
        let pragma_name = query_upper
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .trim_start_matches('"')
            .split('(')
            .next()
            .unwrap_or("");
        if !SAFE_PRAGMAS.iter().any(|p| pragma_name.starts_with(p)) {
            return Err(QueryValidationError::BadRequest(
                "Only read-only PRAGMA queries are allowed (table_info, index_list, etc.)"
                    .to_string(),
            ));
        }
    }

    match first_word {
        "SELECT" | "PRAGMA" | "EXPLAIN" | "WITH" => Ok(()),
        _ => Err(QueryValidationError::BadRequest(
            "Only SELECT, PRAGMA, EXPLAIN, and WITH queries are allowed".to_string(),
        )),
    }
}

/// `POST /b/admin/api/database/query`.
pub(super) async fn handle_query(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct QueryReq {
        query: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: QueryReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    if let Err(e) = validate_readonly_query(&body.query) {
        return match e {
            QueryValidationError::Forbidden(m) => err_forbidden(&m),
            QueryValidationError::BadRequest(m) => err_bad_request(&m),
        };
    }

    match db::query_raw(ctx, &body.query, &body.args).await {
        Ok(records) => {
            let row_count = records.len();
            ok_json(&serde_json::json!({
                "rows": records,
                "row_count": row_count
            }))
        }
        Err(e) => err_bad_request(&format!("Query error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_readonly_query, QueryValidationError};
    use crate::{
        blocks::admin::test_support::routed,
        test_support::{admin_msg, output_http_status, output_json, TestContext},
    };

    async fn api(ctx: &TestContext, path: &str) -> wafer_run::OutputStream {
        wafer_run::Block::handle(
            &crate::blocks::admin::AdminBlock::new(),
            ctx,
            routed(admin_msg("retrieve", path)),
            wafer_run::InputStream::empty(),
        )
        .await
    }

    /// A failed listing is a 500, not `[]` — an empty database.
    #[tokio::test]
    async fn a_failed_table_listing_is_a_500_not_an_empty_list() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let out = api(&ctx, "/b/admin/api/database/tables").await;
        assert_eq!(output_http_status(out).await, 500);
    }

    /// A failed column read is a 500, not a table with no columns.
    #[tokio::test]
    async fn a_failed_column_read_is_a_500_not_no_columns() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let path = format!(
            "/b/admin/api/database/tables/{}/columns",
            crate::blocks::admin::ROLES_TABLE
        );
        let out = api(&ctx, &path).await;
        assert_eq!(output_http_status(out).await, 500);
    }

    /// A name the backend has no table for is a 404, not an empty column list.
    #[tokio::test]
    async fn an_unknown_table_is_a_404() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let out = api(&ctx, "/b/admin/api/database/tables/no_such_table/columns").await;
        assert_eq!(output_http_status(out).await, 404);
    }

    /// Control: healthy reads answer the tables with counts and the columns.
    #[tokio::test]
    async fn healthy_reads_answer_tables_and_columns() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let table = crate::blocks::admin::ROLES_TABLE;

        let tables = output_json(api(&ctx, "/b/admin/api/database/tables").await).await;
        let row = tables
            .as_array()
            .expect("an array")
            .iter()
            .find(|t| t["name"] == table)
            .expect("the roles table is listed");
        assert!(row["row_count"].is_i64(), "{row}");

        let path = format!("/b/admin/api/database/tables/{table}/columns");
        let columns = output_json(api(&ctx, &path).await).await;
        assert!(
            columns["columns"]
                .as_array()
                .expect("columns")
                .iter()
                .any(|c| c["name"] == "name"),
            "{columns}"
        );
    }

    #[test]
    fn validate_accepts_select_pragma_explain_with() {
        assert!(validate_readonly_query("SELECT * FROM users").is_ok());
        assert!(validate_readonly_query("PRAGMA table_info(users)").is_ok());
        assert!(validate_readonly_query("EXPLAIN SELECT 1").is_ok());
        assert!(validate_readonly_query("WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    }

    #[test]
    fn validate_rejects_writes_and_multistatement() {
        assert!(validate_readonly_query("INSERT INTO users VALUES (1)").is_err());
        assert!(validate_readonly_query("UPDATE users SET x = 1").is_err());
        assert!(validate_readonly_query("DELETE FROM users").is_err());
        assert!(validate_readonly_query("SELECT 1; DROP TABLE users").is_err());
    }

    #[test]
    fn validate_rejects_recursive_cte() {
        // SEC-052: unbounded recursive CTEs are a DoS vector.
        assert!(validate_readonly_query(
            "WITH RECURSIVE x(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM x) SELECT * FROM x"
        )
        .is_err());
        // Plain (non-recursive) WITH still works.
        assert!(validate_readonly_query("WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    }

    #[test]
    fn validate_rejects_unsafe_pragma() {
        assert!(validate_readonly_query("PRAGMA writable_schema = 1").is_err());
        assert!(validate_readonly_query("PRAGMA journal_mode = WAL").is_err());
    }

    #[test]
    fn validate_accepts_safe_pragmas() {
        assert!(validate_readonly_query("PRAGMA table_info(users)").is_ok());
        assert!(validate_readonly_query("PRAGMA index_list(users)").is_ok());
        assert!(validate_readonly_query("PRAGMA database_list").is_ok());
    }

    #[test]
    fn validate_accepts_single_trailing_semicolon() {
        // Editors frequently auto-append `;`. A lone trailing terminator
        // is harmless; the no-semicolon rule exists to block piggy-backed
        // writes, not statement terminators.
        assert!(validate_readonly_query("SELECT * FROM users;").is_ok());
        assert!(validate_readonly_query("SELECT * FROM users ;").is_ok());
        assert!(validate_readonly_query("SELECT * FROM users;\n").is_ok());
        assert!(validate_readonly_query("  SELECT 1 ;  ").is_ok());
    }

    #[test]
    fn validate_still_rejects_multistatement_with_trailing_semicolon() {
        // Two real statements, the second terminated — must still be
        // rejected. Stripping one trailing `;` leaves the inner `;`
        // visible to the multi-statement check.
        let e = validate_readonly_query("SELECT 1; DROP TABLE users;").unwrap_err();
        assert!(matches!(e, QueryValidationError::Forbidden(_)));
        let e = validate_readonly_query("SELECT 1; SELECT 2;").unwrap_err();
        assert!(matches!(e, QueryValidationError::Forbidden(_)));
    }

    #[test]
    fn validate_marks_writes_as_forbidden() {
        let err = validate_readonly_query("INSERT INTO users VALUES (1)").unwrap_err();
        assert!(matches!(err, QueryValidationError::Forbidden(_)));
    }

    #[test]
    fn validate_marks_unknown_first_word_as_bad_request() {
        let err = validate_readonly_query("EXEC users").unwrap_err();
        assert!(matches!(err, QueryValidationError::BadRequest(_)));
    }

    /// A secret table is `Forbidden` (403), not `BadRequest` — the query is
    /// well-formed and the answer is "not here". The end-to-end coverage,
    /// including the shapes a result-set mask would have missed, lives in
    /// `tests/admin/sql_explorer_secrets.rs`.
    #[test]
    fn validate_refuses_every_secret_table_whatever_shape_names_it() {
        for entry in crate::secret_tables::SECRET_TABLES {
            let table = entry.table;
            for query in [
                format!("SELECT * FROM {table}"),
                format!("SELECT x AS v FROM {table}"),
                format!("WITH q AS (SELECT x FROM {table}) SELECT * FROM q"),
                format!("PRAGMA table_info({table})"),
            ] {
                let err = validate_readonly_query(&query).unwrap_err();
                assert!(
                    matches!(err, QueryValidationError::Forbidden(_)),
                    "{query}: {err:?}"
                );
                assert!(err.message().contains(table), "{query}: {err:?}");
            }
        }
    }
}
