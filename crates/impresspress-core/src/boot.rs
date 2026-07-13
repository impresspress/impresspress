//! Target-agnostic boot orchestration + variable seeding.
//!
//! Before this module existed, each platform (native CLI, Cloudflare Worker,
//! browser WASM) carried its own copy of:
//! - the auto-generated-secret seeder,
//! - the env-var → variables-table seeder,
//! - and the seal → admin-init → seed → init_all_blocks → post_start
//!   lifecycle dance,
//!
//! with documented drift between the copies (the audit's Top-10 #9). The
//! seeding now lives here once, written against [`DatabaseService`] so all
//! three targets share it — native constructs its `SQLiteDatabaseService`
//! pre-wafer, the browser hands in its `BrowserDatabaseService`, and the
//! Cloudflare runner its D1 service.
//!
//! The block-settings hash-gated seed lives next to its pure planner in
//! [`crate::features::load_and_seed_block_settings`].

use std::{collections::HashMap, sync::Arc};

use wafer_block::db::ListOptions;
use wafer_core::interfaces::database::service::DatabaseService;

use crate::blocks::admin::VARIABLES_TABLE;

/// Build one admin-variables row: a synthesized `id`, the caller-supplied
/// `block` column (derived via [`crate::config_vars::screaming_block`] or
/// [`crate::config_vars::key_block_prefix`]; omitted when empty so the column
/// stays NULL), and `created_at` / `updated_at`. The `value`/`name`/
/// `description`/`warning`/`sensitive` columns vary by call site.
fn build_variable_row(
    key: &str,
    value: &str,
    name: &str,
    description: &str,
    warning: &str,
    sensitive: bool,
    block: &str,
) -> HashMap<String, serde_json::Value> {
    let now = chrono::Utc::now().to_rfc3339();
    let id = format!("var_{}", uuid::Uuid::new_v4());
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert("id".into(), serde_json::Value::String(id));
    data.insert("key".into(), serde_json::Value::String(key.to_string()));
    data.insert("value".into(), serde_json::Value::String(value.to_string()));
    data.insert("name".into(), serde_json::Value::String(name.to_string()));
    data.insert(
        "description".into(),
        serde_json::Value::String(description.to_string()),
    );
    data.insert(
        "warning".into(),
        serde_json::Value::String(warning.to_string()),
    );
    data.insert(
        "sensitive".into(),
        serde_json::Value::Number(serde_json::Number::from(i64::from(sensitive))),
    );
    if !block.is_empty() {
        data.insert("block".into(), serde_json::Value::String(block.to_string()));
    }
    data.insert("created_at".into(), serde_json::Value::String(now.clone()));
    data.insert("updated_at".into(), serde_json::Value::String(now));
    data
}

/// `INSERT OR IGNORE` semantics over [`DatabaseService`]: insert a variable
/// row for `key` only when no row with that `key` already exists.
///
/// Public so platform code can seed its own non-declared defaults (e.g. the
/// browser's bootstrap-admin credentials and WebLLM script var) through the
/// same `DatabaseService` path — no bridge raw SQL, no hardcoded table
/// literal. The `block` column is derived from the key via
/// [`crate::config_vars::key_block_prefix`], matching migration 002.
///
/// Returns `Ok(true)` when a row was inserted, `Ok(false)` when one already
/// existed. A pre-existing row always wins — seeding never clobbers a stored
/// value.
pub async fn seed_variable_if_absent(
    db: &Arc<dyn DatabaseService>,
    key: &str,
    value: &str,
    name: &str,
    description: &str,
    sensitive: bool,
) -> Result<bool, String> {
    let block = crate::config_vars::key_block_prefix(key);
    let data = build_variable_row(key, value, name, description, "", sensitive, &block);
    insert_if_absent(db, key, data).await
}

/// `INSERT OR IGNORE` semantics over [`DatabaseService`]: insert `data` into
/// `VARIABLES_TABLE` only when no row with `data["key"]` already exists.
///
/// `db.create` has no native `OR IGNORE`, so we check existence first via a
/// `key`-filtered list. A pre-existing row (env override, prior boot, admin-UI
/// edit) always wins — seeding never clobbers a stored value.
///
/// Returns `Ok(true)` when a row was inserted, `Ok(false)` when one already
/// existed. Errors bubble up so the caller can decide whether a failed seed is
/// fatal (a missing JWT secret) or merely logged (best-effort secrets).
async fn insert_if_absent(
    db: &Arc<dyn DatabaseService>,
    key: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<bool, String> {
    let exists_opts = ListOptions {
        filters: vec![wafer_block::db::Filter {
            field: "key".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::Value::String(key.to_string()),
        }],
        limit: 1,
        offset: 0,
        skip_count: true,
        ..Default::default()
    };
    // `list` tolerates a missing table (returns empty), so on a fresh DB this
    // is a clean "does not exist" rather than an error.
    let listed = db
        .list(VARIABLES_TABLE, &exists_opts)
        .await
        .map_err(|e| format!("list {VARIABLES_TABLE} for key `{key}`: {e}"))?;
    if !listed.records.is_empty() {
        return Ok(false);
    }
    db.create(VARIABLES_TABLE, data)
        .await
        .map_err(|e| format!("insert variable `{key}`: {e}"))?;
    Ok(true)
}

/// Auto-generate random 32-byte hex secrets for every [`wafer_block::ConfigVar`]
/// declared with `.auto_generate()` that lacks a row in the admin variables
/// table. Shared by all three targets.
///
/// Idempotent: a key that already has a row is left untouched. Per-key failures
/// are logged and tolerated — operators retain the manual seed fallback.
///
/// Ordering contract: this MUST run after the admin block's `lifecycle(Init)`
/// (so migration 002's `block` column exists) and BEFORE
/// [`wafer_run::Wafer::init_all_blocks`] on the targets that seed post-admin
/// (Cloudflare, browser). Native seeds pre-wafer, so it ensures the tables
/// itself first via [`crate::migration_helper::apply_ddl_via_service`].
pub async fn seed_auto_generated(db: &Arc<dyn DatabaseService>) {
    let block_infos = crate::blocks::all_block_infos();
    for info in &block_infos {
        let block_col = crate::config_vars::screaming_block(&info.name);
        for var in &info.config_keys {
            if !var.auto_generate {
                continue;
            }
            match seed_one_secret(db, &block_col, var).await {
                Ok(true) => tracing::warn!(
                    key = %var.key,
                    block = %info.name,
                    "auto-generated secret seeded (no row existed)"
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    key = %var.key,
                    block = %info.name,
                    error = %e,
                    "seed_auto_generated failed"
                ),
            }
        }
    }
}

/// Generate one 32-byte hex secret and insert it for `var` when absent.
async fn seed_one_secret(
    db: &Arc<dyn DatabaseService>,
    block_col: &str,
    var: &wafer_block::ConfigVar,
) -> Result<bool, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("getrandom: {e}"))?;
    let secret: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let data = build_variable_row(
        &var.key,
        &secret,
        &var.name,
        &var.description,
        &var.warning,
        true,
        block_col,
    );
    insert_if_absent(db, &var.key, data).await
}

/// Seed `env_vars` into the admin variables table (`INSERT OR IGNORE`),
/// auto-generate any `auto_generate` secrets, and return the full key→value
/// map currently stored.
///
/// `env_vars` is empty for the browser and Cloudflare targets (their config
/// lives in the platform store, not process env) and carries the
/// declared-key-filtered process environment on native.
///
/// PRECONDITION: the `VARIABLES_TABLE` must already exist — either because the
/// admin block's `lifecycle(Init)` has run (browser, Cloudflare), or because
/// the caller ensured it pre-wafer (native). `db.create` does not lazily
/// create tables.
pub async fn seed_and_load_variables(
    db: &Arc<dyn DatabaseService>,
    env_vars: &[(String, String)],
) -> Result<HashMap<String, String>, String> {
    // 1. Seed env-provided values (existing rows win).
    for (key, value) in env_vars {
        let sensitive = key.ends_with("_SECRET") || key.ends_with("_KEY");
        let block = crate::config_vars::key_block_prefix(key);
        let data = build_variable_row(key, value, "", "", "", sensitive, &block);
        if let Err(e) = insert_if_absent(db, key, data).await {
            tracing::warn!(key = %key, error = %e, "failed to seed env variable");
        }
    }

    // 2. Auto-generate declared secrets (incl. the auth JWT secret).
    seed_auto_generated(db).await;
    seed_jwt_secret(db).await;

    // 3. Load the full set back.
    load_all_variables(db).await
}

/// JWT_SECRET is not declared as an `auto_generate: true` `ConfigVar` by the
/// auth block (a wafer-run config-keys gap noted in the auth block module), so
/// the auto-gen loop above never seeds it. Seed it here so the strict
/// empty-secret boot check (native `server.rs`) can't trip on a fresh DB and
/// the browser/CF crypto can pick up a real key. Idempotent.
async fn seed_jwt_secret(db: &Arc<dyn DatabaseService>) {
    let key = crate::blocks::auth::JWT_SECRET_KEY;
    let block = crate::config_vars::key_block_prefix(key);
    let mut bytes = [0u8; 32];
    if let Err(e) = getrandom::getrandom(&mut bytes) {
        tracing::warn!(error = %e, "getrandom failed for JWT secret");
        return;
    }
    let secret: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let data = build_variable_row(
        key,
        &secret,
        "JWT signing secret",
        "256-bit secret used to sign access + refresh JWTs.",
        "Rotating this secret invalidates every issued session.",
        true,
        &block,
    );
    match insert_if_absent(db, key, data).await {
        Ok(true) => {
            tracing::warn!(key = %key, "auto-generated JWT secret (not found in variables table)")
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(key = %key, error = %e, "failed to seed JWT secret"),
    }
}

/// Read every row of the admin variables table into a key→value map. Rows with
/// an empty `key` are skipped (and warned) as corruption rather than silently
/// dropped.
pub async fn load_all_variables(
    db: &Arc<dyn DatabaseService>,
) -> Result<HashMap<String, String>, String> {
    let opts = ListOptions {
        offset: 0,
        limit: 100_000,
        skip_count: true,
        ..Default::default()
    };
    let listed = db
        .list(VARIABLES_TABLE, &opts)
        .await
        .map_err(|e| format!("load variables from {VARIABLES_TABLE}: {e}"))?;
    let mut vars = HashMap::new();
    for record in listed.records {
        let key = record
            .data
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if key.is_empty() {
            tracing::warn!("variables table contains a row with an empty key");
            continue;
        }
        let value = record
            .data
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        vars.insert(key.to_string(), value.to_string());
    }
    Ok(vars)
}

/// Load admin-created WRAP grants from `impresspress__admin__wrap_grants` via
/// the `DatabaseService`. DB-service twin of the native sqlite-file reader
/// (`cli/server_config.rs::load_wrap_grants`) so stateless targets
/// (Cloudflare) can inject dynamic grants at runtime build. Missing table /
/// read errors degrade to an empty vec — dynamic grants are additive.
pub async fn load_wrap_grants_from_db(
    db: &Arc<dyn DatabaseService>,
) -> Vec<wafer_run::ResourceGrant> {
    // Structured fresh-boot signal: on a first-ever deploy the table does
    // not exist yet (deploy-init builds the runtime BEFORE migrations run)
    // — that's expected and quiet. Any error after the existence check is a
    // real read failure and yields a grant-less, WRAP-denying runtime on
    // the Cloudflare path, so it warns.
    match db
        .schema_table_exists(crate::blocks::admin::WRAP_GRANTS_TABLE)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(
                "wrap_grants table absent (fresh boot before migrations); no dynamic grants"
            );
            return Vec::new();
        }
        Err(e) => {
            tracing::warn!(error = %e, "wrap_grants existence check failed; dynamic grants skipped");
            return Vec::new();
        }
    }
    let opts = ListOptions {
        limit: 10_000,
        skip_count: true,
        ..Default::default()
    };
    let rows = match db
        .list(crate::blocks::admin::WRAP_GRANTS_TABLE, &opts)
        .await
    {
        Ok(list) => list.records,
        Err(e) => {
            tracing::warn!(error = %e, "wrap_grants read failed; dynamic grants skipped");
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter_map(|r| {
            let grantee = match r.data.get("grantee").and_then(|v| v.as_str()) {
                Some(g) => g.to_string(),
                None => {
                    tracing::warn!(id = %r.id, "wrap_grants row missing/non-string `grantee`; dropped");
                    return None;
                }
            };
            let resource = match r.data.get("resource").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    tracing::warn!(id = %r.id, grantee = %grantee, "wrap_grants row missing/non-string `resource`; dropped");
                    return None;
                }
            };
            // sqlite stores booleans as 0/1 integers.
            let write = match r.data.get("write") {
                Some(v) => match v.as_i64().map(|n| n != 0).or_else(|| v.as_bool()) {
                    Some(w) => w,
                    None => {
                        tracing::warn!(id = %r.id, grantee = %grantee, resource = %resource, "wrap_grants row has non-bool/non-int `write`; dropped");
                        return None;
                    }
                },
                None => {
                    tracing::warn!(id = %r.id, grantee = %grantee, resource = %resource, "wrap_grants row missing `write`; dropped");
                    return None;
                }
            };
            // Stored wire value is the lowercase `ResourceType` Display
            // string ("db", "config", …). Absent/empty = intentional
            // all-types wildcard; a non-empty unrecognized value is a
            // typo'd grant and is dropped (fail-closed) rather than
            // widened to the wildcard.
            let resource_type = match wafer_run::ResourceType::parse_stored(
                r.data.get("resource_type").and_then(|v| v.as_str()),
            ) {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!(id = %r.id, grantee = %grantee, resource = %resource, error = %e, "wrap_grants row dropped");
                    return None;
                }
            };
            Some(wafer_run::ResourceGrant {
                grantee,
                resource,
                write,
                resource_type,
            })
        })
        .collect()
}

#[cfg(test)]
mod wrap_grants_tests {
    use super::*;

    /// Open a fresh in-memory SQLite [`DatabaseService`] with no migrations
    /// applied — the same construction `features.rs`'s
    /// `db_with_block_settings_table` and `test_support::TestContext::new`
    /// use for a real, host-runnable `DatabaseService`.
    async fn bare_db() -> Arc<dyn DatabaseService> {
        Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        )
    }

    #[tokio::test]
    async fn load_wrap_grants_maps_rows_and_tolerates_missing_table() {
        let db = bare_db().await;

        // Missing table → empty, no error.
        assert!(load_wrap_grants_from_db(&db).await.is_empty());

        // Apply admin migrations (creates `impresspress__admin__wrap_grants`
        // among the other admin tables) through the same pre-wafer DDL
        // runner native's `server.rs::run` uses
        // (`migration_helper::apply_ddl_via_service` +
        // `blocks::admin::migrations::ddl_files`) — the migration-file-
        // runner exception to the no-raw-SQL rule (CLAUDE.md), reusing the
        // real embedded schema rather than a hand-rolled CREATE TABLE.
        crate::migration_helper::apply_ddl_via_service(
            &db,
            crate::blocks::admin::migrations::ddl_files("sqlite"),
        )
        .await
        .expect("apply admin migrations");

        // Create + seed two rows via the service (no raw SQL).
        let mut r1 = HashMap::new();
        r1.insert("grantee".into(), serde_json::json!("impresspress/files"));
        r1.insert(
            "resource".into(),
            serde_json::json!("impresspress__files__objects"),
        );
        r1.insert("write".into(), serde_json::json!(1));
        r1.insert("resource_type".into(), serde_json::json!("db"));
        let mut r2 = HashMap::new();
        r2.insert("grantee".into(), serde_json::json!("wafer-run/auth"));
        r2.insert("resource".into(), serde_json::json!("bucket/x"));
        r2.insert("write".into(), serde_json::json!(0));
        db.create(crate::blocks::admin::WRAP_GRANTS_TABLE, r1)
            .await
            .unwrap();
        db.create(crate::blocks::admin::WRAP_GRANTS_TABLE, r2)
            .await
            .unwrap();

        let grants = load_wrap_grants_from_db(&db).await;
        assert_eq!(grants.len(), 2);
        let g1 = grants
            .iter()
            .find(|g| g.grantee == "impresspress/files")
            .unwrap();
        assert!(g1.write);
        assert_eq!(g1.resource_type, Some(wafer_run::ResourceType::Db));
        let g2 = grants
            .iter()
            .find(|g| g.grantee == "wafer-run/auth")
            .unwrap();
        assert!(!g2.write);
        assert_eq!(g2.resource_type, None);

        // Unrecognized resource_type → the ROW is dropped (fail-closed),
        // never widened to the all-types wildcard.
        let mut r3 = HashMap::new();
        r3.insert("grantee".into(), serde_json::json!("impresspress/products"));
        r3.insert(
            "resource".into(),
            serde_json::json!("impresspress__products__items"),
        );
        r3.insert("write".into(), serde_json::json!(1));
        r3.insert("resource_type".into(), serde_json::json!("databsae"));
        db.create(crate::blocks::admin::WRAP_GRANTS_TABLE, r3)
            .await
            .unwrap();
        // Empty-string resource_type → kept as an intentional wildcard.
        let mut r4 = HashMap::new();
        r4.insert("grantee".into(), serde_json::json!("impresspress/files"));
        r4.insert("resource".into(), serde_json::json!("bucket/y"));
        r4.insert("write".into(), serde_json::json!(0));
        r4.insert("resource_type".into(), serde_json::json!(""));
        db.create(crate::blocks::admin::WRAP_GRANTS_TABLE, r4)
            .await
            .unwrap();

        let grants = load_wrap_grants_from_db(&db).await;
        assert_eq!(grants.len(), 3, "typo'd resource_type row must be dropped");
        assert!(grants.iter().all(|g| g.grantee != "impresspress/products"));
        let g4 = grants
            .iter()
            .find(|g| g.resource == "bucket/y")
            .expect("empty resource_type row kept");
        assert_eq!(g4.resource_type, None);
    }

    use wafer_block::db::Filter;
    use wafer_core::interfaces::database::service::{
        Column, DatabaseError, Record, RecordList, Table,
    };

    /// A [`DatabaseService`] whose existence check fails hard and whose every
    /// other method is [`unreachable!`]. Isolates the fail-closed `Err` arm of
    /// [`load_wrap_grants_from_db`]: the only method it should reach is
    /// `schema_table_exists`, so a real read error there must short-circuit to
    /// an empty grant set without ever touching `list`.
    struct ErroringDb;

    #[async_trait::async_trait]
    impl DatabaseService for ErroringDb {
        async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
            Err(DatabaseError::Internal(
                "simulated wrap_grants existence-check failure".into(),
            ))
        }

        async fn get(&self, _collection: &str, _id: &str) -> Result<Record, DatabaseError> {
            unreachable!(
                "load_wrap_grants_from_db must not read rows after an existence-check error"
            )
        }

        async fn list(
            &self,
            _collection: &str,
            _opts: &ListOptions,
        ) -> Result<RecordList, DatabaseError> {
            unreachable!(
                "load_wrap_grants_from_db must not list rows after an existence-check error"
            )
        }

        async fn create(
            &self,
            _collection: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!()
        }

        async fn update(
            &self,
            _collection: &str,
            _id: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!()
        }

        async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn sum(
            &self,
            _collection: &str,
            _field: &str,
            _filters: &[Filter],
        ) -> Result<f64, DatabaseError> {
            unreachable!()
        }

        async fn query_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }

        async fn exec_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn schema_add_column(
            &self,
            _table: &str,
            _column: &Column,
        ) -> Result<(), DatabaseError> {
            unreachable!()
        }
    }

    /// A hard read error from the existence check is fail-closed: dynamic
    /// grants are additive, so a runtime that cannot confirm the table exists
    /// must build WRAP-denying (empty grants) rather than risk widening access
    /// on a bad read.
    #[tokio::test]
    async fn load_wrap_grants_existence_check_error_fails_closed() {
        let db: Arc<dyn DatabaseService> = Arc::new(ErroringDb);
        assert!(
            load_wrap_grants_from_db(&db).await.is_empty(),
            "a schema_table_exists error must degrade to an empty grant set"
        );
    }
}
