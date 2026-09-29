//! The admin SQL explorer must not read a table that stores credential
//! material.
//!
//! Both explorer surfaces — `POST /b/admin/api/database/query` (JSON) and
//! `POST /b/admin/database/query` (the SSR SQL editor) — are driven here
//! through `TestContext::dispatch_resolved*` — the production request path
//! from `routing::route_to_block` on, access gate included, with the admin
//! caller taken as already resolved: this file is about what the explorer
//! answers an admin, not about who is one. Rows are staged in the
//! real tables through the real migrations, so a passing assertion is about
//! the endpoint an operator reaches, not about a helper it happens to call.
//!
//! # Why the boundary sits at the validator and not at the result set
//!
//! [`column_name_masking_has_nothing_to_key_on`] is the load-bearing test of
//! this file. `db::query_raw` hands back records keyed by the **returned**
//! column name, which the query author chooses. A mask keyed on
//! `(table, column)` therefore dies to `SELECT value AS v`, and to every
//! expression, subquery, join and CTE shape below it — it would advertise a
//! guarantee it cannot keep. Refusing the query before it runs is the only
//! rule the query text cannot be reshaped around.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use impresspress_core::{
    blocks::admin::{migrations, AdminBlock},
    platform_state::variables::{self, NewVariable},
    secret_tables::SECRET_TABLES,
    test_support::{admin_msg, output_http_json, output_http_status, TestContext},
};
use serde_json::json;
use wafer_core::clients::database as db;

/// The value staged in `impresspress__admin__variables`. Distinctive enough
/// that a substring check over a whole response body is meaningful.
const SECRET: &str = "s3cr3t-jwt-signing-value-0192837465";

/// Stage one sensitive variables row through the module that owns the table,
/// then prove by direct SQL that the plaintext really is sitting in `value` —
/// otherwise a refusal below would be protecting nothing.
async fn stage_secret_variable(ctx: &TestContext) {
    let ctx = &ctx.fixture();
    migrations::apply(ctx)
        .await
        .expect("apply admin migrations");
    variables::insert(
        ctx,
        NewVariable {
            key: "WAFER_RUN__AUTH__JWT_SECRET".to_string(),
            value: SECRET.to_string(),
            name: String::new(),
            description: String::new(),
            warning: String::new(),
            sensitive: true,
            updated_by: "test".to_string(),
            block: None,
        },
    )
    .await
    .expect("insert variables row");

    let rows = db::query_raw(
        ctx,
        "SELECT value FROM impresspress__admin__variables WHERE key = 'WAFER_RUN__AUTH__JWT_SECRET'",
        &[],
    )
    .await
    .expect("read back the staged row");
    assert_eq!(
        rows.first()
            .and_then(|r| r.data.get("value"))
            .and_then(|v| v.as_str()),
        Some(SECRET),
        "the fixture did not put the plaintext in the column under test"
    );
}

/// A `TestContext` with the admin block registered, so `dispatch*` routes to
/// the real handlers rather than 404ing.
async fn explorer_ctx() -> TestContext {
    let mut ctx = TestContext::new().await;
    ctx.register_block("impresspress/admin", std::sync::Arc::new(AdminBlock::new()));
    stage_secret_variable(&ctx).await;
    ctx
}

/// Run `query` through the JSON explorer endpoint and return
/// `(http status, response body as text)`.
async fn run_json(ctx: &TestContext, query: &str) -> (u16, String) {
    let out = ctx
        .dispatch_resolved_json(
            admin_msg("create", "/b/admin/api/database/query"),
            &json!({ "query": query }),
        )
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    (
        parts.status,
        String::from_utf8_lossy(&parts.body).into_owned(),
    )
}

/// Every query shape that reaches `impresspress__admin__variables.value`
/// without spelling `value` as the returned column name — the reason a
/// result-set mask cannot work.
fn evasion_shapes() -> Vec<(&'static str, String)> {
    let t = "impresspress__admin__variables";
    vec![
        ("plain", format!("SELECT key, value FROM {t}")),
        ("alias", format!("SELECT value AS v FROM {t}")),
        (
            "expression",
            format!("SELECT substr(value, 1, 20) FROM {t}"),
        ),
        ("hex", format!("SELECT hex(value) FROM {t}")),
        ("concat", format!("SELECT value || '' FROM {t}")),
        ("subquery", format!("SELECT * FROM (SELECT value FROM {t})")),
        (
            "join",
            format!("SELECT v.value FROM {t} v JOIN {t} w ON v.id = w.id"),
        ),
        (
            "cte",
            format!("WITH x AS (SELECT value FROM {t}) SELECT * FROM x"),
        ),
        (
            "group_concat",
            format!("SELECT group_concat(value) FROM {t}"),
        ),
        ("quoted", format!("SELECT value FROM \"{t}\"")),
        ("bracketed", format!("SELECT value FROM [{t}]")),
        ("backticked", format!("SELECT value FROM `{t}`")),
        ("schema_qualified", format!("SELECT value FROM main.{t}")),
        (
            "schema_qualified_quoted",
            format!("SELECT value FROM main.\"{t}\""),
        ),
        (
            "uppercase",
            format!("SELECT VALUE FROM {}", t.to_uppercase()),
        ),
        ("mixed_case", format!("SELECT value FROM {}", mixed_case(t))),
        ("explain", format!("EXPLAIN SELECT value FROM {t}")),
        ("pragma", format!("PRAGMA table_info({t})")),
    ]
}

/// `impresspress__admin__variables` with alternating capitals — SQLite folds
/// identifier case, so this names the same table.
fn mixed_case(s: &str) -> String {
    s.chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Why the mask has to be at the validator
// ---------------------------------------------------------------------------

/// The evidence for rejecting result-set masking: the records the explorer
/// serialises are keyed by the name the QUERY chose, and the plaintext rides
/// under it.
///
/// This drives `db::query_raw` — the exact call `handle_query` makes — rather
/// than the endpoint, because the claim under test is about what that call
/// returns, which is what a masking layer would have had to work from.
#[tokio::test]
async fn column_name_masking_has_nothing_to_key_on() {
    let ctx = TestContext::new().await;
    stage_secret_variable(&ctx).await;

    // In the admin block's frame, where `handle_query` makes it.
    let rows = db::query_raw(
        &ctx.clone()
            .running_as(impresspress_core::blocks::admin::ADMIN_BLOCK_ID),
        "SELECT value AS v FROM impresspress__admin__variables",
        &[],
    )
    .await
    .expect("aliased select");
    let row = rows.first().expect("one row");
    assert!(
        !row.data.contains_key("value"),
        "a (table, column) mask would look for `value` and find nothing: {:?}",
        row.data.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        row.data.get("v").and_then(|v| v.as_str()),
        Some(SECRET),
        "the secret arrives under the alias the query picked"
    );

    let rows = db::query_raw(
        &ctx.clone()
            .running_as(impresspress_core::blocks::admin::ADMIN_BLOCK_ID),
        "SELECT substr(value, 1, 12) FROM impresspress__admin__variables",
        &[],
    )
    .await
    .expect("expression select");
    let leaked: Vec<String> = rows
        .first()
        .expect("one row")
        .data
        .values()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    assert!(
        leaked.iter().any(|v| SECRET.starts_with(v.as_str())),
        "an expression leaks a prefix of the secret under a computed column \
         name, so even an exact-value comparison would not catch it: {leaked:?}"
    );
}

// ---------------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_evasion_shape_is_refused_and_leaks_nothing() {
    let ctx = explorer_ctx().await;
    for (name, query) in evasion_shapes() {
        let (status, body) = run_json(&ctx, &query).await;
        assert_eq!(status, 403, "{name}: {query}\nbody: {body}");
        assert!(
            !body.contains(SECRET),
            "{name}: the response carried the secret: {body}"
        );
    }
}

#[tokio::test]
async fn the_refusal_names_the_table_and_where_to_go_instead() {
    let ctx = explorer_ctx().await;
    let (_status, body) = run_json(
        &ctx,
        "SELECT key, value FROM impresspress__admin__variables",
    )
    .await;
    assert!(
        body.contains("impresspress__admin__variables"),
        "the refusal must say which table it refused: {body}"
    );
    assert!(
        body.contains("/b/admin/variables"),
        "the refusal must point at the page that serves this need: {body}"
    );
}

#[tokio::test]
async fn the_ssr_sql_editor_refuses_the_same_queries() {
    let ctx = explorer_ctx().await;
    let body = "query=SELECT+value+FROM+impresspress__admin__variables";
    let mut msg = admin_msg("create", "/b/admin/database/query");
    msg.set_meta("req.content_type", "application/x-www-form-urlencoded");
    let out = ctx
        .dispatch_resolved_with_input(
            msg,
            wafer_run::InputStream::from_bytes(body.as_bytes().to_vec()),
        )
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let rendered = String::from_utf8_lossy(&parts.body).into_owned();
    assert!(
        !rendered.contains(SECRET),
        "the SSR editor rendered the secret: {rendered}"
    );
    assert!(
        rendered.contains("/b/admin/variables"),
        "the SSR editor must render the same refusal the API returns: {rendered}"
    );
}

#[tokio::test]
async fn every_registered_secret_table_is_refused() {
    let ctx = explorer_ctx().await;
    for entry in SECRET_TABLES {
        let (status, body) = run_json(&ctx, &format!("SELECT * FROM {}", entry.table)).await;
        assert_eq!(status, 403, "{} was queryable\nbody: {body}", entry.table);
        assert!(
            body.contains(entry.table),
            "{}: the refusal did not name it: {body}",
            entry.table
        );
    }
}

/// The `client_secret` inside [`STRIPE_EVENT_BODY`]. Distinctive enough that
/// a substring check over a whole response body is meaningful, and not
/// invented: it is the value Stripe's own Event reference prints.
#[cfg(feature = "block-products")]
const STRIPE_CLIENT_SECRET: &str =
    "seti_1NG8Du2eZvKYlo2C9XMqbR0x_secret_O2CdhLwGFh2Aej7bCY7qp8jlIuyR8DJ";

/// A Stripe webhook body, trimmed to the fields that matter here.
///
/// The `client_secret` is not a fixture invention: `data.object` is copied
/// from the `setup_intent.created` example Event that Stripe's own reference
/// prints at <https://docs.stripe.com/api/events/object>, which carries a
/// populated secret. See the module docs on `secret_tables` for why that
/// example is the evidence this table is refused.
#[cfg(feature = "block-products")]
const STRIPE_EVENT_BODY: &str = concat!(
    r#"{"id":"evt_1NG8Du2eZvKYlo2CUI79vXWy","object":"event","#,
    r#""type":"setup_intent.created","livemode":false,"data":{"object":{"#,
    r#""id":"seti_1NG8Du2eZvKYlo2C9XMqbR0x","object":"setup_intent","#,
    r#""client_secret":"seti_1NG8Du2eZvKYlo2C9XMqbR0x_secret_O2CdhLwGFh2Aej7bCY7qp8jlIuyR8DJ""#,
    r#"}}}"#,
);

/// A stored Stripe webhook body can carry a credential of Stripe's own, so
/// the explorer must refuse the table that holds it.
///
/// The row is staged the way `stripe::record_event` stages one — base64 of
/// the verbatim bytes Stripe posted — and the fixture asserts the secret
/// really decodes out of the stored column before testing the refusal, so a
/// pass cannot come from having staged nothing.
#[cfg(feature = "block-products")]
#[tokio::test]
async fn a_stored_stripe_webhook_body_is_not_served_by_the_explorer() {
    use base64ct::{Base64, Encoding};

    // The fixture's own frame: it stages the row, and the explorer request
    // below is routed, so it runs as the admin block.
    let mut ctx = TestContext::with_products().await.fixture();
    ctx.register_block("impresspress/admin", std::sync::Arc::new(AdminBlock::new()));

    let mut row: HashMap<String, serde_json::Value> = HashMap::new();
    row.insert("id".into(), json!("evt_1NG8Du2eZvKYlo2CUI79vXWy"));
    row.insert("event_type".into(), json!("setup_intent.created"));
    row.insert("status".into(), json!("processed"));
    row.insert(
        "payload_base64".into(),
        json!(Base64::encode_string(STRIPE_EVENT_BODY.as_bytes())),
    );
    row.insert("created_at".into(), json!("2026-01-01T00:00:00Z"));
    db::create(&ctx, "impresspress__products__stripe_events", row)
        .await
        .expect("stage a stripe_events row");

    let rows = db::query_raw(
        &ctx,
        "SELECT payload_base64 FROM impresspress__products__stripe_events",
        &[],
    )
    .await
    .expect("read back the staged row");
    let stored = rows
        .first()
        .and_then(|r| r.data.get("payload_base64"))
        .and_then(|v| v.as_str())
        .expect("the staged row has a payload_base64");
    let decoded = String::from_utf8(Base64::decode_vec(stored).expect("stored value is base64"))
        .expect("the decoded body is utf-8");
    assert!(
        decoded.contains(STRIPE_CLIENT_SECRET),
        "the fixture did not put a Stripe credential in the column under test: {decoded}"
    );

    // The same evasion shapes the config store is tested against: a result-set
    // mask has nothing to key on, so the refusal has to come before the query
    // runs.
    for query in [
        "SELECT * FROM impresspress__products__stripe_events",
        "SELECT payload_base64 AS v FROM impresspress__products__stripe_events",
        "SELECT substr(payload_base64, 1, 40) FROM impresspress__products__stripe_events",
        "WITH e AS (SELECT * FROM impresspress__products__stripe_events) SELECT * FROM e",
    ] {
        let (status, body) = run_json(&ctx, query).await;
        assert_eq!(status, 403, "{query} was answered\nbody: {body}");
        assert!(
            !body.contains(STRIPE_CLIENT_SECRET) && !body.contains(stored),
            "{query}: the response carried the stored webhook body: {body}"
        );
    }
}

/// Postgres can spell an identifier without its own characters appearing in
/// the statement, which is the one hole in a substring test. The explorer
/// refuses the syntax outright rather than leaving it open.
#[tokio::test]
async fn postgres_unicode_escaped_identifiers_are_refused() {
    let ctx = explorer_ctx().await;
    // `impresspress__admin__variable` + `\0073` (`s`) — the config store,
    // spelled so that `impresspress__admin__variables` appears nowhere.
    let query = "SELECT value FROM U&\"impresspress__admin__variable\\0073\"";
    assert!(
        !query.contains("impresspress__admin__variables"),
        "the fixture must not contain the literal name, or it proves nothing"
    );
    let (status, body) = run_json(&ctx, query).await;
    assert_eq!(status, 403, "body: {body}");
    assert!(!body.contains(SECRET), "body: {body}");
}

// ---------------------------------------------------------------------------
// What the refusal must NOT cost
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ordinary_tables_are_still_queryable() {
    let ctx = explorer_ctx().await;
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("id".into(), json!("role-1"));
    data.insert("name".into(), json!("admin"));
    data.insert("description".into(), json!(""));
    data.insert("permissions".into(), json!("[]"));
    data.insert("is_system".into(), json!(1));
    data.insert("created_at".into(), json!("2026-01-01T00:00:00Z"));
    data.insert("updated_at".into(), json!("2026-01-01T00:00:00Z"));
    db::create(&ctx.fixture(), "impresspress__admin__roles", data)
        .await
        .expect("stage a roles row");

    let (status, body) = run_json(&ctx, "SELECT name FROM impresspress__admin__roles").await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("admin"), "body: {body}");

    let value = output_http_json(
        ctx.dispatch_resolved_json(
            admin_msg("create", "/b/admin/api/database/query"),
            &json!({ "query": "SELECT 1 AS one" }),
        )
        .await,
    )
    .await;
    assert_eq!(value["row_count"], json!(1));
    assert_eq!(
        output_http_status(
            ctx.dispatch_resolved_json(
                admin_msg("create", "/b/admin/api/database/query"),
                &json!({ "query": "SELECT 1 AS one" }),
            )
            .await,
        )
        .await,
        200
    );
}

// ---------------------------------------------------------------------------
// The boundary is derived, not hand-kept
// ---------------------------------------------------------------------------

/// Columns whose NAME says "credential" but whose content is not one, with the
/// reason each is cleared.
///
/// The scan below is deliberately over-broad: it matches on the naming
/// convention alone, so every digest column in the schema lands in front of it.
/// That is the point — a new `*_hash`, `*token*`, `*secret*`, `*password*` or
/// `*verifier*` column, a new `*payload*`, `*body*` or `*raw*` column, or a
/// new table carrying a `sensitive` flag, has to be classified by a person.
/// Adding a row here is how you record "looked at it, it is not a credential";
/// the alternative is `impresspress_core::secret_tables::SECRET_TABLES`.
const CLEARED_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "impresspress__admin__block_settings",
        "current_hash",
        "digest of a block's migration set, for change detection",
    ),
    (
        "impresspress__admin__block_settings",
        "blessed_hash",
        "the migration digest an operator accepted",
    ),
    (
        "impresspress__admin__block_settings",
        "seed_defaults_hash",
        "digest of the seed defaults already applied",
    ),
    (
        "impresspress__products__checkout_presets",
        "configuration_hash",
        "digest of a saved checkout configuration",
    ),
    (
        "impresspress__products__payment_links",
        "configuration_hash",
        "digest of the configuration a payment link was minted for",
    ),
    (
        "impresspress__tickets__tickets",
        "dedupe_hash",
        "digest of an inbound message, for idempotency",
    ),
    (
        "impresspress__llm__providers",
        "max_tokens_field",
        "the name of the JSON field a provider's chat body spells its \
         output-token budget in — `max_tokens` or `max_completion_tokens`. It \
         matches the `token` needle by the arithmetic of the word; the value \
         is one of two literals the admin picks from a select",
    ),
    (
        "impresspress__tickets__events",
        "body",
        "one ticket message's prose. A submitter does compose it, but it is \
         text this block stores and serves back itself, not a structured \
         document from another system with fields of its own",
    ),
];

/// The schema the migrations leave behind: `table → columns`, as of the last
/// migration, per block.
///
/// **Migration ORDER and `DROP TABLE` are modelled, not ignored.** A union
/// over every `CREATE TABLE` in a block's history is not the schema — it is
/// the schema plus every column any earlier version ever had.
/// `012_sessions_family` drops `wafer_run__auth__sessions` and recreates it
/// without the `token_hash` that `001_auth_schema` declared, and a scan that
/// unions the two reports a column no deployment has. That is not a corner:
/// the guard tests below exist to catch a registry entry naming a column that
/// is not there, and a history-union scan cannot see exactly that mistake.
///
/// Each dialect is folded separately, in filename order (which is migration
/// order — every file is `NNN_name.{sqlite,postgres}.sql`), and the two
/// results are unioned. A column only one dialect declares is kept: a
/// divergence between the two is its own bug, and over-reporting is the safe
/// direction for everything built on this.
fn migration_schema() -> BTreeMap<String, BTreeSet<String>> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let blocks_dir = manifest_dir.join("src/blocks");
    let mut merged: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    let blocks = std::fs::read_dir(&blocks_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", blocks_dir.display()));
    for block in blocks {
        let dir = block.expect("dir entry").path().join("migrations");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // `(dialect, filename) → path`, so a BTreeMap key sorts each dialect's
        // files into migration order.
        let mut files: BTreeMap<(String, String), std::path::PathBuf> = BTreeMap::new();
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("sql") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("migration filename")
                .to_string();
            // A file that names no dialect is a naming scheme this scan does
            // not understand; failing loudly beats folding it into one dialect
            // arbitrarily or skipping it silently.
            let dialect = if name.ends_with(".sqlite.sql") {
                "sqlite"
            } else if name.ends_with(".postgres.sql") {
                "postgres"
            } else {
                panic!("{}: migration filename names no dialect", path.display());
            };
            files.insert((dialect.to_string(), name), path);
        }

        let mut per_dialect: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        for ((dialect, _name), path) in files {
            let sql = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            apply_sql(&sql, &path, per_dialect.entry(dialect).or_default());
        }
        for schema in per_dialect.into_values() {
            for (table, columns) in schema {
                merged.entry(table).or_default().extend(columns);
            }
        }
    }

    assert!(
        merged.values().map(BTreeSet::len).sum::<usize>() > 200,
        "the migration scan found {} columns — it lost its way",
        merged.values().map(BTreeSet::len).sum::<usize>()
    );
    merged
}

/// Apply one migration file's DDL to `schema`.
///
/// Handles the four shapes the migrations use: `DROP TABLE [IF EXISTS] <t>`
/// (which forgets every column, so a following `CREATE TABLE` starts clean),
/// `CREATE TABLE [IF NOT EXISTS] <t> ( … )` whose body lines are
/// `<name> <TYPE> …`, and `ALTER TABLE <t> ADD COLUMN [IF NOT EXISTS] <c> …`,
/// whose clause may sit on the `ALTER TABLE` line or on the line after it
/// (admin migration 003 wraps; the products and auth ones do not).
fn apply_sql(sql: &str, path: &std::path::Path, schema: &mut BTreeMap<String, BTreeSet<String>>) {
    let mut current: Option<String> = None;
    let mut altering: Option<String> = None;
    for (lineno, raw) in sql.lines().enumerate() {
        let line = raw.trim();
        if let Some(rest) = strip_keyword(line, "DROP TABLE") {
            let rest = strip_keyword(rest, "IF EXISTS").unwrap_or(rest);
            schema.remove(&identifier(rest));
            current = None;
            continue;
        }
        if let Some(rest) = strip_keyword(line, "ALTER TABLE") {
            let table = identifier(rest);
            match add_column_name(line) {
                Some(col) => {
                    schema.entry(table).or_default().insert(col);
                }
                // The clause wrapped: remember the table for the next line.
                None => altering = Some(table),
            }
            current = None;
            continue;
        }
        if let Some(table) = altering.take() {
            if let Some(col) = add_column_name(line) {
                schema.entry(table).or_default().insert(col);
            }
            continue;
        }
        if let Some(rest) = strip_keyword(line, "CREATE TABLE") {
            let rest = strip_keyword(rest, "IF NOT EXISTS").unwrap_or(rest);
            let table = identifier(rest);
            schema.entry(table.clone()).or_default();
            current = Some(table);
            continue;
        }
        let Some(table) = current.clone() else {
            continue;
        };
        if line.starts_with(')') {
            current = None;
            continue;
        }
        let mut words = line.split_whitespace();
        let (Some(name), Some(ty)) = (words.next(), words.next()) else {
            continue;
        };
        // Not a column declaration: a comment, or a table-level constraint
        // (`UNIQUE (a, b)`, `PRIMARY KEY (…)`), or a continuation line of a
        // multi-line CHECK. Decided by the leading word, so that what counts
        // as a column is never decided by the column's TYPE — a whitelist of
        // types silently drops every column declared with one nobody thought
        // of, which is the opposite of the derivation this file claims.
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || NON_COLUMN_LEADING_WORDS
                .iter()
                .any(|kw| name.eq_ignore_ascii_case(kw))
        {
            continue;
        }
        // Past that filter the line IS a column declaration, so its type must
        // be one this scan knows. An unrecognised token is a loud failure
        // rather than a dropped column: `TIMESTAMPTZ` and `DOUBLE PRECISION`
        // were both already in the tree and both silently invisible here.
        let ty = ty
            .split('(')
            .next()
            .unwrap_or("")
            .trim_end_matches(',')
            .to_ascii_uppercase();
        assert!(
            KNOWN_COLUMN_TYPES.contains(&ty.as_str()),
            "{}:{}: unrecognised column type {ty:?} in {line:?} — add it to \
             KNOWN_COLUMN_TYPES (or to NON_COLUMN_LEADING_WORDS if this line is \
             not a column declaration). Left out, the column would be scanned \
             as if it did not exist.",
            path.display(),
            lineno + 1,
        );
        schema.entry(table).or_default().insert(name.to_string());
    }
}

/// Leading words that mean "this line inside a `CREATE TABLE` body is not a
/// column declaration". Table constraints and the continuation lines of a
/// multi-line `CHECK`.
const NON_COLUMN_LEADING_WORDS: &[&str] = &[
    "UNIQUE",
    "PRIMARY",
    "FOREIGN",
    "CHECK",
    "CONSTRAINT",
    "AND",
    "OR",
    "NOT",
    "REFERENCES",
    "ON",
];

/// Every column type the migrations use. Not a filter — a checklist: a type
/// missing from here fails the scan rather than dropping its column.
const KNOWN_COLUMN_TYPES: &[&str] = &[
    "TEXT",
    "INTEGER",
    "BIGINT",
    "BOOLEAN",
    "REAL",
    "DOUBLE",
    "BLOB",
    "BYTEA",
    "TIMESTAMPTZ",
];

/// Strip a leading SQL keyword (case-insensitively) plus the whitespace after
/// it, or `None` when `line` does not start with it.
fn strip_keyword<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    let head = line.get(..keyword.len())?;
    head.eq_ignore_ascii_case(keyword)
        .then(|| line[keyword.len()..].trim_start())
}

/// `(table, column)` for every column a person has to classify: one whose
/// name matches the credential convention, one whose name says it stores a
/// document composed elsewhere, plus every column of a table that carries a
/// `sensitive` flag.
///
/// The `sensitive` clause is what catches
/// `impresspress__admin__variables.value`, whose name says nothing: a
/// `sensitive` column is the schema's own statement that rows of this table
/// may hold a secret. That is the same column `cache_key::row_is_sensitive`
/// reads (paired with `key` by `sensitive_check_columns`) to keep secrets out
/// of the KV cache.
fn columns_needing_classification() -> BTreeSet<(String, String)> {
    let schema = migration_schema();
    let mut out = BTreeSet::new();
    for (table, columns) in &schema {
        let flagged = columns.iter().any(|c| c.eq_ignore_ascii_case("sensitive"));
        for column in columns {
            if flagged || looks_like_a_credential(column) || looks_like_a_foreign_document(column) {
                out.insert((table.clone(), column.clone()));
            }
        }
    }
    out
}

/// The leading identifier of `rest`, stopping at whitespace, `(` or `;`.
fn identifier(rest: &str) -> String {
    rest.chars()
        .take_while(|c| !c.is_whitespace() && *c != '(' && *c != ';')
        .collect()
}

/// The column name of an `ADD COLUMN [IF NOT EXISTS] <name> …` clause.
fn add_column_name(line: &str) -> Option<String> {
    let at = line.to_ascii_uppercase().find("ADD COLUMN")?;
    let rest = line[at + "ADD COLUMN".len()..].trim_start();
    let rest = strip_keyword(rest, "IF NOT EXISTS").unwrap_or(rest);
    Some(identifier(rest))
}

/// The naming convention: a column whose name says it carries a secret, a
/// bearer token, or a digest of one.
fn looks_like_a_credential(column: &str) -> bool {
    let c = column.to_ascii_lowercase();
    ["secret", "password", "credential", "token", "verifier"]
        .iter()
        .any(|needle| c.contains(needle))
        || c.ends_with("_hash")
}

/// The second convention: a column named `*payload*`, `*body*` or `*raw*`.
///
/// `payload` and `body` are the words this schema uses today when a column
/// holds a document rather than a field; `raw` matches nothing yet and is
/// here because it is the third word someone would reach for.
///
/// `impresspress__products__stripe_events.payload_base64` is why this exists.
/// It matches nothing in [`looks_like_a_credential`] — no name convention
/// could, because what makes it refusable is not the column's name but the
/// fact that a third party chooses its contents, and Stripe puts a
/// `client_secret` in them. A name scan cannot judge that; what it can do is
/// refuse to let such a column pass unlooked-at, which is the same job the
/// credential scan does one row above.
///
/// These three needles, and not a general "foreign document" test: a column
/// spelled `*_manifest_json` or `*_info_json` would hold a document and would
/// not be caught. The four that exist today are each a serialisation of a
/// local typed struct, so nothing is uncovered now —
/// `impresspress__dev__generations.site_manifest_json` and
/// `block_manifest_json` are `generation::canonical_text` of the staged
/// manifest, and `impresspress__dev__builds.block_info_json` and
/// `diagnostics_json` are `serde_json::to_string` of a `BlockInfo` and of the
/// compiler diagnostics. Widen the needles rather than trusting this note if
/// a column ever holds a document someone else composed.
///
/// Over-broad on purpose, and cheaply so: across every migration in the tree
/// it selects three columns in two tables — `payload_base64` and
/// `payload_sha256`, both on the refused `stripe_events`, and
/// `impresspress__tickets__events.body`, cleared above.
fn looks_like_a_foreign_document(column: &str) -> bool {
    let c = column.to_ascii_lowercase();
    ["payload", "body", "raw"]
        .iter()
        .any(|needle| c.contains(needle))
}

#[test]
fn every_column_needing_classification_is_refused_or_cleared() {
    let refused: BTreeSet<&str> = SECRET_TABLES.iter().map(|e| e.table).collect();
    let cleared: BTreeSet<(&str, &str)> = CLEARED_COLUMNS
        .iter()
        .map(|(table, column, _why)| (*table, *column))
        .collect();

    let unclassified: Vec<(String, String)> = columns_needing_classification()
        .into_iter()
        .filter(|(table, column)| {
            !refused.contains(table.as_str())
                && !cleared.contains(&(table.as_str(), column.as_str()))
        })
        .collect();

    assert!(
        unclassified.is_empty(),
        "columns nobody has classified: {unclassified:?} — either add the \
         table to `secret_tables::SECRET_TABLES` or record in CLEARED_COLUMNS why its contents \
         are not a credential"
    );
}

/// The other half of the closed set: a registered table must actually have the
/// columns its entry claims, and a cleared column must still exist. Either one
/// going stale is how a boundary quietly stops covering what it names.
#[test]
fn the_registry_and_the_clearances_still_describe_the_schema() {
    let shaped = columns_needing_classification();
    for entry in SECRET_TABLES {
        for column in entry.columns {
            assert!(
                shaped.contains(&(entry.table.to_string(), (*column).to_string())),
                "{}.{column} is registered in SECRET_TABLES but the scan does not select it. \
                 Either the migrations no longer declare the column, or its name matches \
                 neither `looks_like_a_credential` nor `looks_like_a_foreign_document` — check \
                 which before assuming the schema changed",
                entry.table
            );
        }
    }
    for (table, column, _why) in CLEARED_COLUMNS {
        assert!(
            shaped.contains(&((*table).to_string(), (*column).to_string())),
            "{table}.{column} is cleared but the migrations no longer declare it"
        );
    }
}

/// The tables a reviewer would expect to be refused and deliberately are not,
/// stated so the boundary is not read as "everything auth touches".
#[test]
fn deliberately_readable_tables_stay_readable() {
    let refused: BTreeSet<&str> = SECRET_TABLES.iter().map(|e| e.table).collect();
    for (table, why) in [
        (
            "wafer_run__auth__sessions",
            "a device list since migration 012 dropped and recreated it: \
             family/user_id/auth_method/timestamps, no token_hash, and its repo \
             module says nothing authenticates against it",
        ),
        (
            "wafer_run__auth__jwt_blocklist",
            "revoked `jti`s: identifiers of tokens, not tokens",
        ),
        ("wafer_run__auth__orgs", "org names and verification refs"),
        ("wafer_run__auth__rate_limits", "counters keyed by bucket"),
        (
            "impresspress__products__provider_operations",
            "request_json is the literal {\"version\":1} and response_json a \
             summary this repo builds; no provider body reaches either",
        ),
        (
            "impresspress__admin__block_settings",
            "per-block enable flag plus migration-state digests",
        ),
    ] {
        assert!(
            !refused.contains(table),
            "{table} became refused; if that is right, drop this row and say why ({why})"
        );
    }
}

/// Selecting a refused table in the SQL editor must not prefill a query whose
/// only outcome is a 403 — the panel says why and where to go instead.
#[tokio::test]
async fn the_sql_editor_does_not_prefill_a_query_it_will_refuse() {
    let ctx = explorer_ctx().await;
    let table = variables::TABLE;

    let mut msg = admin_msg("retrieve", "/b/admin/database");
    msg.set_meta("req.query.table", table);
    msg.set_meta("req.query.tab", "sql");
    let parts =
        wafer_block::http_codec::collect_http_response(ctx.dispatch_resolved(msg).await).await;
    let page = String::from_utf8_lossy(&parts.body).into_owned();

    // The panel really rendered — without this the two assertions below pass
    // on a 404 body, which is how a page test says nothing at all.
    assert!(
        page.contains("db-sql__input"),
        "the SQL panel did not render"
    );
    assert!(
        !page.contains(&format!("SELECT * FROM {table} LIMIT 100;")),
        "the editor prefilled a query the validator refuses"
    );
    assert!(
        page.contains("/b/admin/variables"),
        "the editor must name the surface that serves this table: {page}"
    );

    // An ordinary table still gets its prefill: the note is scoped to the
    // refused set, not to the panel.
    let mut msg = admin_msg("retrieve", "/b/admin/database");
    msg.set_meta("req.query.table", "impresspress__admin__roles");
    msg.set_meta("req.query.tab", "sql");
    let parts =
        wafer_block::http_codec::collect_http_response(ctx.dispatch_resolved(msg).await).await;
    let page = String::from_utf8_lossy(&parts.body).into_owned();
    assert!(
        page.contains("SELECT * FROM impresspress__admin__roles LIMIT 100;"),
        "an ordinary table lost its prefill"
    );
}

// ---------------------------------------------------------------------------
// The scanner's own properties
// ---------------------------------------------------------------------------
//
// These drive `apply_sql` over synthetic DDL rather than over the tree,
// deliberately. Asserting against the real migrations cannot pin either
// property: `rate_limits.created_at` is `TIMESTAMPTZ` in the postgres file and
// `TEXT` in the sqlite one, so the union hides a postgres-only type from any
// tree-level assertion — a tree-level test of the type handling passes even
// with the type filter restored, which is exactly the kind of test that proves
// nothing. What the scanner must do is a property of the scanner.

/// Fold one synthetic migration file and return the resulting schema.
fn schema_of(sql: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut schema = BTreeMap::new();
    apply_sql(sql, std::path::Path::new("<test>.sqlite.sql"), &mut schema);
    schema
}

/// A column is read whatever its declared type.
///
/// The first draft decided "is this line a column?" with a whitelist of eight
/// type names, so every column declared with anything else was dropped in
/// silence. `TIMESTAMPTZ` and `DOUBLE PRECISION` were both already in the tree
/// and both invisible; a credential column declared `VARCHAR(64)` would have
/// left the whole suite green, which the "derived, not hand-kept" claim this
/// file rests on cannot survive.
#[test]
fn the_scanner_reads_a_column_of_any_known_type() {
    let schema = schema_of(
        "CREATE TABLE IF NOT EXISTS t (
            id          TEXT PRIMARY KEY,
            seen_at     TIMESTAMPTZ NOT NULL,
            score       DOUBLE PRECISION NOT NULL CHECK (score >= 0.0 AND score <= 1.0),
            digest      BYTEA,
            hits        BIGINT NOT NULL DEFAULT 0,
            UNIQUE (id, seen_at)
        );",
    );
    let cols = schema.get("t").expect("table scanned");
    assert_eq!(
        cols.iter().map(String::as_str).collect::<Vec<_>>(),
        ["digest", "hits", "id", "score", "seen_at"],
        "a column was dropped, or a constraint line was read as one"
    );
}

/// A type the scanner does not know is a loud failure, not a dropped column —
/// so the checklist cannot quietly fall behind the schema the way the old
/// whitelist did.
#[test]
#[should_panic(expected = "unrecognised column type \"VARCHAR\"")]
fn the_scanner_panics_on_an_unknown_column_type() {
    schema_of("CREATE TABLE t (\n    name VARCHAR(64) NOT NULL\n);");
}

/// `DROP TABLE` forgets the dropped table's columns, so a recreated table is
/// described by its CURRENT declaration rather than by the union of every
/// declaration it has ever had.
///
/// This is the property the `sessions` entry needed and did not have: the
/// registry claimed `sessions.token_hash`, the guard test passed, and the
/// column had not existed since `012_sessions_family` dropped and recreated
/// the table without it.
#[test]
fn the_scanner_honours_drop_table() {
    let schema = schema_of(
        "CREATE TABLE IF NOT EXISTS t (
            token_hash TEXT PRIMARY KEY,
            user_id    TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS kept (
            id TEXT PRIMARY KEY
        );
        DROP TABLE IF EXISTS t;
        CREATE TABLE IF NOT EXISTS t (
            family  TEXT PRIMARY KEY,
            user_id TEXT NOT NULL
        );",
    );
    let t = schema.get("t").expect("table scanned");
    assert!(
        !t.contains("token_hash"),
        "the scan is unioning history rather than applying the drop: {t:?}"
    );
    assert!(t.contains("family"), "{t:?}");
    // The drop must forget one table, not the file.
    assert!(schema.get("kept").is_some_and(|c| c.contains("id")));
}

/// And the same property, as the tree actually stands: `sessions` is the table
/// it caught, so a regression that reintroduced history-unioning would show up
/// here without anyone reading a synthetic fixture.
#[test]
fn the_sessions_table_is_scanned_as_migration_012_left_it() {
    let schema = migration_schema();
    let sessions = schema
        .get("wafer_run__auth__sessions")
        .expect("the sessions table is declared");
    assert!(
        !sessions.contains("token_hash"),
        "`012_sessions_family` dropped this column: {sessions:?}"
    );
    assert!(sessions.contains("family"), "{sessions:?}");
}
