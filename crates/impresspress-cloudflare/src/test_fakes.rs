//! Service fakes for this crate's wasm tests that need a request's services
//! to answer: a crypto service that answers `random_bytes`, a database that
//! records the rows it is asked to create, a storage service that only
//! creates folders, and a D1 and a KV that hold `block_settings` state for the
//! deploy funnel's migration tests. Every other call is an error, so a test
//! that reaches one fails loudly instead of passing on a default. Also
//! [`bounded`], the five-second JS timer that turns a hung await into a
//! failure.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    rc::Rc,
    time::Duration,
};

use wafer_block::db::{Filter, ListOptions};
use wafer_core::interfaces::{
    crypto::service::{CryptoError, CryptoService},
    database::service::{
        AggregateSpec, CapGuard, Column, DatabaseError, DatabaseService, GuardedInsert,
        GuardedUpdate, Record, RecordList, StatementBudget, Table, UpsertSpec, WriteOp,
        WriteOutcome,
    },
    storage::service::{
        FolderInfo, ListOptions as StorageListOptions, ObjectInfo, ObjectList, StorageError,
        StorageService,
    },
};
use wafer_run::{InputStream, OutputStream};

/// Answers `random_bytes`; nothing else.
pub(crate) struct FixedCrypto;

#[wafer_block::wafer_async_trait]
impl CryptoService for FixedCrypto {
    async fn hash(&self, _password: &str) -> Result<String, CryptoError> {
        Err(CryptoError::Other("unused in this test".into()))
    }

    async fn compare_hash(&self, _password: &str, _hash: &str) -> Result<(), CryptoError> {
        Err(CryptoError::Other("unused in this test".into()))
    }

    async fn sign_for(
        &self,
        _block_id: &str,
        _claims: BTreeMap<String, serde_json::Value>,
        _expiry: Duration,
    ) -> Result<String, CryptoError> {
        Err(CryptoError::Other("unused in this test".into()))
    }

    async fn verify_for(
        &self,
        _block_id: &str,
        _token: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
        Err(CryptoError::Other("unused in this test".into()))
    }

    async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        Ok(vec![7; n])
    }
}

/// Records each row `create` is asked for as `"{collection} {operation}"`,
/// where `operation` is the row's `operation` column (empty when it has
/// none). Shared through an `Rc` so the test reads what the request wrote.
#[derive(Default)]
pub(crate) struct CreateRecordingDb {
    pub(crate) created: Rc<RefCell<Vec<String>>>,
}

fn unused<T>() -> Result<T, DatabaseError> {
    Err(DatabaseError::Internal("unused in this test".into()))
}

#[wafer_block::wafer_async_trait]
impl DatabaseService for CreateRecordingDb {
    async fn get(&self, _collection: &str, _id: &str) -> Result<Record, DatabaseError> {
        unused()
    }

    async fn list(
        &self,
        _collection: &str,
        _opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        unused()
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let operation = data
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        self.created
            .borrow_mut()
            .push(format!("{collection} {operation}"));
        Ok(Record {
            id: "created".to_string(),
            data: Default::default(),
        })
    }

    async fn update(
        &self,
        _collection: &str,
        _id: &str,
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        unused()
    }

    async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
        unused()
    }

    async fn count(&self, _collection: &str, _filters: &[Filter]) -> Result<i64, DatabaseError> {
        unused()
    }

    async fn sum(
        &self,
        _collection: &str,
        _field: &str,
        _filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        unused()
    }

    async fn increment_field_where(
        &self,
        _collection: &str,
        _col: &str,
        _delta: i64,
        _filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        unused()
    }

    async fn query_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        unused()
    }

    async fn exec_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        unused()
    }

    async fn exec_raw_transaction(
        &self,
        _statements: &[wafer_core::interfaces::database::service::RawStatement],
    ) -> Result<Vec<i64>, wafer_core::interfaces::database::service::TransactionError> {
        unused().map_err(wafer_core::interfaces::database::service::TransactionError::whole)
    }

    async fn create_many(
        &self,
        _collection: &str,
        _rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError> {
        unused()
    }

    async fn take_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        unused()
    }

    async fn update_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        unused()
    }

    async fn batch(&self, _ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
        unused()
    }

    async fn insert_guarded(
        &self,
        _collection: &str,
        _data: HashMap<String, serde_json::Value>,
        _guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError> {
        unused()
    }

    async fn update_guarded(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
        _guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError> {
        unused()
    }

    async fn upsert(
        &self,
        _collection: &str,
        _spec: UpsertSpec,
    ) -> Result<Option<Record>, DatabaseError> {
        unused()
    }

    async fn aggregate(
        &self,
        _collection: &str,
        _spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        unused()
    }

    async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
        unused()
    }

    async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
        unused()
    }

    async fn schema_columns(&self, _table: &str) -> Result<Vec<String>, DatabaseError> {
        unused()
    }

    async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
        unused()
    }

    async fn schema_add_column(&self, _table: &str, _column: &Column) -> Result<(), DatabaseError> {
        unused()
    }

    fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
        Ok(StatementBudget::Unbounded)
    }
}

/// A storage service that creates folders and does nothing else.
pub(crate) struct FoldersOnly;

fn unsupported<T>() -> Result<T, StorageError> {
    Err(StorageError::Internal("unused in this test".into()))
}

#[wafer_block::wafer_async_trait]
impl StorageService for FoldersOnly {
    async fn put(
        &self,
        _folder: &str,
        _key: &str,
        _data: &[u8],
        _content_type: &str,
    ) -> Result<(), StorageError> {
        unsupported()
    }

    async fn put_streaming(
        &self,
        _folder: &str,
        _key: &str,
        _data: InputStream,
        _content_type: &str,
    ) -> Result<(), StorageError> {
        unsupported()
    }

    async fn get(&self, _folder: &str, _key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        unsupported()
    }

    async fn get_streaming(
        &self,
        _folder: &str,
        _key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        unsupported()
    }

    async fn delete(&self, _folder: &str, _key: &str) -> Result<(), StorageError> {
        unsupported()
    }

    async fn list(
        &self,
        _folder: &str,
        _opts: &StorageListOptions,
    ) -> Result<ObjectList, StorageError> {
        unsupported()
    }

    async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
        Ok(())
    }

    async fn delete_folder(&self, _name: &str) -> Result<(), StorageError> {
        unsupported()
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        unsupported()
    }
}

/// One ordered log [`BlockSettingsD1`] and [`MemoryKv`] both append to, so a
/// test can say which came first: a statement reaching D1 or a KV write.
pub(crate) type Log = Rc<RefCell<Vec<String>>>;

/// The executable `UPDATE` of admin's migration 009, exactly as the migration
/// runner hands it to D1 (one of the file's
/// `migration_helper::executable_statements`; 009 is frozen, so the runner
/// sends them one at a time): the one raw statement [`BlockSettingsD1`]
/// gives a meaning to.
pub(crate) fn admin_009_clear_statement() -> String {
    use impresspress_core::{
        blocks::admin::migrations::{migration_files, BLOCK_SETTINGS_CLEAR_BLESSED_HASH},
        migration_helper::executable_statements,
    };
    let (_, sql) = migration_files("sqlite")
        .iter()
        .find(|(name, _)| *name == BLOCK_SETTINGS_CLEAR_BLESSED_HASH)
        .expect("009 is wired into admin's SQLite list");
    executable_statements(sql)
        .into_iter()
        .find(|statement| statement.starts_with("UPDATE impresspress__admin__block_settings"))
        .expect("009 updates block_settings")
}

/// The `current_hash` a deployment from before the per-file ledger recorded
/// for admin once it had applied 001-008: the list 009 is pending on, which
/// the runner adopts (`migration_helper`, "Databases from before the
/// ledger").
pub(crate) fn admin_list_before_009() -> String {
    use impresspress_core::{
        blocks::admin::migrations::{migration_files, BLOCK_SETTINGS_CLEAR_BLESSED_HASH},
        migration_helper::migration_set_hash,
    };
    let all = migration_files("sqlite");
    let at = all
        .iter()
        .position(|(name, _)| *name == BLOCK_SETTINGS_CLEAR_BLESSED_HASH)
        .expect("009 is wired into admin's SQLite list");
    migration_set_hash(&all[..at])
}

/// A D1 stand-in for a database an earlier deploy migrated: it holds
/// `block_settings` rows in memory, answers reads of that table with them
/// (honouring equality filters and a limit), applies typed writes to them, and
/// carries out admin 009's `UPDATE` (see [`admin_009_clear_statement`]) by
/// emptying every row's `blessed_hash`. Every other raw statement, and every
/// raw transaction (the migration ledger's rows), is accepted and changes
/// nothing. A created row takes the column's default, an empty
/// `blessed_hash`. Reads of any other table — the ledger's among them — find
/// nothing and writes to one are only logged. Every raw statement, raw
/// transaction and typed write is logged.
pub(crate) struct BlockSettingsD1 {
    pub(crate) log: Log,
    pub(crate) rows: RefCell<Vec<Record>>,
}

impl BlockSettingsD1 {
    pub(crate) fn new(log: Log, rows: Vec<Record>) -> Self {
        Self {
            log,
            rows: RefCell::new(rows),
        }
    }

    fn note(&self, entry: String) {
        self.log.borrow_mut().push(entry);
    }
}

fn block_settings_table() -> &'static str {
    impresspress_core::platform_state::block_settings::TABLE
}

#[wafer_block::wafer_async_trait]
impl DatabaseService for BlockSettingsD1 {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        if collection != block_settings_table() {
            return Err(DatabaseError::NotFound);
        }
        self.rows
            .borrow()
            .iter()
            .find(|row| row.id == id)
            .cloned()
            .ok_or(DatabaseError::NotFound)
    }

    async fn list(
        &self,
        collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        let mut records: Vec<Record> = if collection == block_settings_table() {
            self.rows
                .borrow()
                .iter()
                .filter(|row| {
                    opts.filters.iter().all(|filter| {
                        filter.operator == wafer_block::db::FilterOp::Equal
                            && row.data.get(&filter.field) == Some(&filter.value)
                    })
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if let Some(limit) = opts.limit {
            records.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }
        Ok(RecordList {
            total_count: records.len() as i64,
            records,
            page: 1,
            page_size: 500,
        })
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.note(format!("d1 create {collection}"));
        let mut data = data;
        if collection == block_settings_table() {
            // The column's default, as the table declares it.
            data.entry("blessed_hash".to_string())
                .or_insert_with(|| serde_json::json!(""));
        }
        let mut rows = self.rows.borrow_mut();
        let record = Record {
            id: data
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| format!("created_{}", rows.len()), str::to_string),
            data: data.into_iter().collect(),
        };
        if collection == block_settings_table() {
            rows.push(record.clone());
        }
        Ok(record)
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.note(format!("d1 update {collection}/{id}"));
        if collection != block_settings_table() {
            return unused();
        }
        let mut rows = self.rows.borrow_mut();
        let row = rows
            .iter_mut()
            .find(|row| row.id == id)
            .ok_or(DatabaseError::NotFound)?;
        row.data.extend(data);
        Ok(row.clone())
    }

    async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
        unused()
    }

    async fn count(&self, _collection: &str, _filters: &[Filter]) -> Result<i64, DatabaseError> {
        Ok(0)
    }

    async fn sum(
        &self,
        _collection: &str,
        _field: &str,
        _filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        unused()
    }

    async fn increment_field_where(
        &self,
        _collection: &str,
        _col: &str,
        _delta: i64,
        _filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        unused()
    }

    async fn query_raw(
        &self,
        _query: &str,
        _args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        unused()
    }

    async fn exec_raw(
        &self,
        query: &str,
        _args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        self.note(format!("d1 exec {query}"));
        if query == admin_009_clear_statement() {
            let mut cleared = 0;
            for row in self.rows.borrow_mut().iter_mut() {
                if let Some(blessed) = row.data.get_mut("blessed_hash") {
                    if blessed != "" {
                        *blessed = serde_json::json!("");
                        cleared += 1;
                    }
                }
            }
            return Ok(cleared);
        }
        Ok(0)
    }

    async fn exec_raw_transaction(
        &self,
        statements: &[wafer_core::interfaces::database::service::RawStatement],
    ) -> Result<Vec<i64>, wafer_core::interfaces::database::service::TransactionError> {
        self.note(format!("d1 tx {} statements", statements.len()));
        Ok(vec![1; statements.len()])
    }

    async fn create_many(
        &self,
        _collection: &str,
        _rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError> {
        unused()
    }

    async fn take_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        unused()
    }

    async fn update_where(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        unused()
    }

    async fn batch(&self, _ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
        unused()
    }

    async fn insert_guarded(
        &self,
        _collection: &str,
        _data: HashMap<String, serde_json::Value>,
        _guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError> {
        unused()
    }

    async fn update_guarded(
        &self,
        _collection: &str,
        _filters: &[Filter],
        _data: HashMap<String, serde_json::Value>,
        _guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError> {
        unused()
    }

    async fn upsert(
        &self,
        _collection: &str,
        _spec: UpsertSpec,
    ) -> Result<Option<Record>, DatabaseError> {
        unused()
    }

    async fn aggregate(
        &self,
        _collection: &str,
        _spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        unused()
    }

    async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
        unused()
    }

    async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
        unused()
    }

    async fn schema_columns(&self, _table: &str) -> Result<Vec<String>, DatabaseError> {
        unused()
    }

    async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
        unused()
    }

    async fn schema_add_column(&self, _table: &str, _column: &Column) -> Result<(), DatabaseError> {
        unused()
    }

    fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
        Ok(StatementBudget::Unbounded)
    }
}

/// A KV namespace in memory: reads answer what was written, and every write
/// and delete is logged to the shared [`Log`].
pub(crate) struct MemoryKv {
    pub(crate) log: Log,
    pub(crate) entries: RefCell<HashMap<String, String>>,
}

impl MemoryKv {
    pub(crate) fn new(log: Log, entries: HashMap<String, String>) -> Self {
        Self {
            log,
            entries: RefCell::new(entries),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl impresspress_core::kv::KvBackend for MemoryKv {
    async fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.entries.borrow().get(key).cloned())
    }

    async fn put_with_ttl(&self, key: &str, value: &str, _ttl_secs: u64) -> Result<(), String> {
        self.put(key, value).await
    }

    async fn put(&self, key: &str, value: &str) -> Result<(), String> {
        self.log.borrow_mut().push(format!("kv put {key}"));
        self.entries
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        self.log.borrow_mut().push(format!("kv delete {key}"));
        self.entries.borrow_mut().remove(key);
        Ok(())
    }
}

/// A sealed runtime holding the real admin block over `db`, as a deploy funnel
/// builds it: `IMPRESSPRESS_RUN_MIGRATIONS` set, and a block-settings snapshot
/// in which admin last applied `admin_current_hash`. Config and crypto are
/// registered only because admin `requires` them; the runtime does not seal
/// without them.
pub(crate) async fn deploy_admin_runtime(
    db: std::sync::Arc<dyn DatabaseService>,
    admin_current_hash: &str,
) -> wafer_run::Wafer {
    use std::sync::Arc;

    use impresspress_core::{
        blocks::admin::{AdminBlock, ADMIN_BLOCK_ID},
        features::{BlockSettings, BlockState, MigrationState, BLOCK_SETTINGS_CONFIG_KEY},
        migration_helper::RUN_MIGRATIONS_KEY,
    };

    let mut wafer = wafer_run::Wafer::new(Arc::new(wafer_run::StaticConfigSource::default()))
        .expect("Wafer::new");
    wafer.set_admin_block(ADMIN_BLOCK_ID);
    wafer_core::service_blocks::database::register_with(&mut wafer, db)
        .expect("register the database");
    wafer_core::service_blocks::config::register_with(
        &mut wafer,
        Arc::new(crate::config_service::HashMapConfigService::new(
            HashMap::new(),
        )),
    )
    .expect("register config");
    wafer_core::service_blocks::crypto::register_with(&mut wafer, Arc::new(FixedCrypto))
        .expect("register crypto");
    wafer
        .register_block(ADMIN_BLOCK_ID, Arc::new(AdminBlock::new()))
        .expect("register admin");
    let mut blocks = HashMap::new();
    blocks.insert(
        ADMIN_BLOCK_ID.to_string(),
        BlockState {
            enabled: true,
            migration: MigrationState {
                current_hash: admin_current_hash.to_string(),
            },
            seed_defaults_hash: String::new(),
        },
    );
    let mut snapshot = HashMap::new();
    snapshot.insert(RUN_MIGRATIONS_KEY.to_string(), "1".to_string());
    snapshot.insert(
        BLOCK_SETTINGS_CONFIG_KEY.to_string(),
        BlockSettings::from_blocks(blocks).to_config_json(),
    );
    wafer.set_config_snapshot(snapshot);
    wafer.seal().await.expect("seal");
    wafer
}

/// A `block_settings` row as a pre-#145 build left it: `current_hash` and
/// `blessed_hash` as given.
pub(crate) fn blessed_row(id: &str, block: &str, current: &str, blessed: &str) -> Record {
    let mut data = HashMap::new();
    data.insert("block_name".to_string(), serde_json::json!(block));
    data.insert("enabled".to_string(), serde_json::json!(1));
    data.insert("current_hash".to_string(), serde_json::json!(current));
    data.insert("blessed_hash".to_string(), serde_json::json!(blessed));
    data.insert("seed_defaults_hash".to_string(), serde_json::json!(""));
    data.insert(
        "created_at".to_string(),
        serde_json::json!("2026-09-27T00:00:00Z"),
    );
    data.insert(
        "updated_at".to_string(),
        serde_json::json!("2026-09-27T00:00:00Z"),
    );
    Record {
        id: id.to_string(),
        data: data.into_iter().collect(),
    }
}

#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &js_sys::Function, delay_ms: i32) -> wasm_bindgen::JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(id: &wasm_bindgen::JsValue);
}

/// `fut`'s output, or a failure naming `what` after five seconds. The timer
/// only turns a hang into a failure, and is cleared either way.
pub(crate) async fn bounded<F: std::future::Future + Unpin>(fut: F, what: &str) -> F::Output {
    let mut timer_id = wasm_bindgen::JsValue::UNDEFINED;
    let timer = worker::wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
        &mut |resolve, _reject| {
            timer_id = set_timeout(&resolve, 5_000);
        },
    ));
    let outcome = futures::future::select(fut, timer).await;
    clear_timeout(&timer_id);
    match outcome {
        futures::future::Either::Left((output, _)) => output,
        futures::future::Either::Right(_) => panic!("{what}: nothing within 5 s"),
    }
}

/// A `worker::Env` with no bindings and no vars: enough for the code paths
/// that capture the environment and never reach a binding.
pub(crate) fn empty_env() -> worker::Env {
    use wasm_bindgen::JsCast as _;
    wasm_bindgen::JsValue::from(js_sys::Object::new()).unchecked_into::<worker::Env>()
}
