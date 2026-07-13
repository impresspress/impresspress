use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};
use wafer_sql_utils::{introspect, Backend};

use crate::http::{err_bad_request, err_forbidden, err_internal, err_not_found, ok_json};

/// Lightweight per-table summary: name + row count. Shared by the JSON
/// `GET /admin/database/tables` handler and the SSR database page's
/// left-pane list so both run the same introspection routine.
pub(in crate::blocks::admin) struct TableSummary {
    pub name: String,
    pub row_count: i64,
}

/// A single column's introspected metadata. Shared by the JSON
/// `GET /admin/database/tables/{name}/columns` handler and the SSR
/// schema panel.
pub(in crate::blocks::admin) struct ColumnInfo {
    pub name: String,
    pub ty: String,
    pub notnull: bool,
    pub pk: bool,
    /// The column's default expression, if any (SQLite `dflt_value`).
    pub default_value: Option<String>,
}

/// Run the backend table count for one table name, returning 0 on any
/// failure (an invalid identifier is treated like a failed count query).
///
/// The name always originates from the backend's own table listing, so a
/// build error here means an identifier the backend can't quote.
async fn table_row_count(ctx: &dyn Context, name: &str) -> i64 {
    match introspect::build_table_row_count(name, crate::db_backend(ctx).await) {
        Ok(count_sql) => db::query_raw(ctx, &count_sql, &[])
            .await
            .ok()
            .and_then(|r| {
                r.first()
                    .and_then(|r| r.data.get("cnt").and_then(|v| v.as_i64()))
            })
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// List every table with its row count, sorted by name.
///
/// Single source of truth for the table-browser introspection shared by the
/// JSON API and the SSR page. The per-table COUNT is issued sequentially —
/// concurrent counts on a single backend connection (the SQLite case) can
/// deadlock, and the row is read-once-per-page, so the dedupe (not the
/// fan-out) is the win here.
pub(in crate::blocks::admin) async fn introspect_table_summaries(
    ctx: &dyn Context,
) -> Vec<TableSummary> {
    let sql = introspect::build_list_tables(crate::db_backend(ctx).await);
    let records = db::query_raw(ctx, &sql, &[]).await.unwrap_or_default();
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
        let row_count = table_row_count(ctx, &name).await;
        out.push(TableSummary { name, row_count });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Introspect one table's columns plus its row count. `table` is untrusted
/// (URL path / selected name); an invalid identifier yields an empty column
/// list and a 0 count rather than an error, matching both surfaces' prior
/// behavior.
pub(in crate::blocks::admin) async fn introspect_columns(
    ctx: &dyn Context,
    table: &str,
) -> (Vec<ColumnInfo>, i64) {
    let columns = match introspect::build_table_info(table, crate::db_backend(ctx).await) {
        Ok((info_sql, info_args)) => db::query_raw(ctx, &info_sql, &info_args)
            .await
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
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
    let row_count = table_row_count(ctx, table).await;
    (cols, row_count)
}

/// `path` is the normalized `/admin/database/...` sub-path, passed explicitly
/// (no `req.resource` rewrite). `handle_columns` extracts the table name from it.
pub async fn handle(
    ctx: &dyn Context,
    msg: &Message,
    path: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action();

    match (action, path) {
        ("retrieve", "/admin/database/info") => handle_info(ctx).await,
        ("retrieve", "/admin/database/tables") => handle_tables(ctx).await,
        ("retrieve", _)
            if path.starts_with("/admin/database/tables/") && path.ends_with("/columns") =>
        {
            handle_columns(ctx, path).await
        }
        ("create", "/admin/database/query") => handle_query(ctx, input).await,
        _ => err_not_found("not found"),
    }
}

async fn handle_info(ctx: &dyn Context) -> OutputStream {
    let backend = crate::db_backend(ctx).await;
    let sql = introspect::build_list_tables(backend);
    let tables = match db::query_raw(ctx, &sql, &[]).await {
        Ok(t) => t,
        Err(e) => return err_internal("Database error", e),
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

async fn handle_tables(ctx: &dyn Context) -> OutputStream {
    let table_info: Vec<serde_json::Value> = introspect_table_summaries(ctx)
        .await
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

async fn handle_columns(ctx: &dyn Context, path: &str) -> OutputStream {
    // Extract table name from /admin/database/tables/{name}/columns
    let table_name = path
        .strip_prefix("/admin/database/tables/")
        .and_then(|s| s.strip_suffix("/columns"))
        .unwrap_or("");

    if table_name.is_empty() {
        return err_bad_request("Missing table name");
    }

    // The table name is user input from the URL path; an invalid identifier
    // is a bad request, not a server error.
    if introspect::build_table_info(table_name, crate::db_backend(ctx).await).is_err() {
        return err_bad_request("Invalid table name");
    }

    let (columns, _row_count) = introspect_columns(ctx, table_name).await;
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
/// and unsafe PRAGMAs.
///
/// Used by both the JSON API (`POST /admin/database/query`) and the
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

async fn handle_query(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct QueryReq {
        query: String,
        #[serde(default)]
        args: Vec<serde_json::Value>,
    }
    let raw = input.collect_to_bytes().await;
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
}
