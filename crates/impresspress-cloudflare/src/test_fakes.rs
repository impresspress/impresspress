//! Service fakes for this crate's wasm tests that need a request's services
//! to answer: a crypto service that answers `random_bytes`, a database that
//! records the rows it is asked to create, and a storage service that only
//! creates folders. Every other call is an error, so a test that reaches one fails
//! loudly instead of passing on a default.

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
