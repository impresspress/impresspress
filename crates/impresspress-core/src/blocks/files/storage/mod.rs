//! Storage HANDLERS for the `impresspress/files` block: the user-facing
//! `/b/storage/api/...` JSON API and the admin stats endpoint. (This is the
//! impresspress-core files storage-handlers module — NOT the Cloudflare R2
//! adapter.) Dispatch lives in the block's one route table
//! (`blocks/files/mod.rs`); this module owns only the handlers.
//!
//! Split by domain responsibility:
//! - [`params`] — the bound `{name}` / `{key...}` path variables.
//! - [`validation`] — bucket-name / storage-key validation rules, shared
//!   with the share-creation path (`cloud.rs`).
//! - [`access`] — bucket-ownership / access-control predicates.
//! - [`buckets`] — bucket lifecycle: list, create, delete.
//! - [`objects`] — object lifecycle: list, download (streamed), upload,
//!   delete.
//! - [`search`] — object search + recently-viewed listing.
//! - [`admin`] — the aggregate stats endpoint.
//!
//! Every handler is re-exported here at `pub(in crate::blocks::files)` so the
//! block's `handle` names them as `storage::handle_*`.

mod access;
mod admin;
mod buckets;
mod objects;
mod params;
mod search;
mod validation;

pub(in crate::blocks::files) use access::{bucket_owned_by, require_bucket_access};
pub(in crate::blocks::files) use admin::handle_stats;
pub(in crate::blocks::files) use buckets::{
    handle_create_bucket, handle_delete_bucket, handle_list_buckets,
};
pub(in crate::blocks::files) use objects::{
    delete_blobs, handle_delete_object, handle_get_object, handle_list_objects,
    handle_upload_object,
};
pub(in crate::blocks::files) use search::{handle_recent, handle_search};
pub(in crate::blocks::files) use validation::{
    is_valid_bucket_name, is_valid_storage_key, BUCKET_NAME_MAX_LEN, BUCKET_NAME_MIN_LEN,
    BUCKET_NAME_PATTERN,
};

/// Test-only fixture shared by more than one domain submodule's integration
/// tests (buckets, objects, and admin/stats all seed buckets the same way).
#[cfg(test)]
mod test_helpers {
    use std::{
        collections::{HashMap, HashSet},
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use serde_json::json;
    use wafer_core::interfaces::storage::service::{
        FolderInfo, ListOptions as StoreListOptions, ObjectInfo, ObjectList, StorageError,
        StorageService,
    };

    use crate::{blocks::files::repo, test_support::TestContext};

    pub(super) async fn seed_bucket(ctx: &TestContext, name: &str, owner: &str) {
        let data = crate::util::json_map(json!({
            "name": name,
            "public": false,
            "created_by": owner,
            "created_at": crate::util::now_rfc3339(),
        }));
        repo::buckets::seed(ctx, data).await.expect("seed bucket");
    }

    /// Seed a completed object-metadata row, as a finished upload leaves it.
    pub(super) async fn seed_object_row(
        ctx: &TestContext,
        bucket: &str,
        key: &str,
        owner: &str,
        size: i64,
    ) {
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("bucket".into(), json!(bucket));
        row.insert("key".into(), json!(key));
        row.insert("size".into(), json!(size));
        row.insert("uploaded_by".into(), json!(owner));
        row.insert("status".into(), json!("complete"));
        repo::objects::seed(ctx, row)
            .await
            .expect("seed object row");
    }

    /// `(folder, key)` → `(bytes, content_type)`.
    type MemObjects = HashMap<(String, String), (Vec<u8>, String)>;

    /// In-memory [`StorageService`] so handler tests exercise the production
    /// `wafer-run/storage` [`StorageBlock`] wire protocol end-to-end (the
    /// typed `store::*` clients round-trip through the real handler) without
    /// touching the filesystem.
    ///
    /// Folders are tracked, and `delete` / `delete_folder` answer `NotFound`
    /// for what was never stored, the way the real backends do — the delete
    /// handlers' retry behaviour depends on that distinction.
    #[derive(Default)]
    pub(super) struct MemStorage {
        objects: Mutex<MemObjects>,
        folders: Mutex<HashSet<String>>,
        /// [`StorageService`] method names this double refuses, so a test can
        /// drive the compensation a handler runs when storage fails after the
        /// metadata row is already written — the paths that decide whether a
        /// failure leaves an orphan row or an unrecorded blob behind.
        ///
        /// Switchable at runtime rather than fixed at construction, so one
        /// context (one database) can serve the upload that succeeds and the
        /// upload that fails: the state the compensation has to restore is
        /// what the first one left.
        refused: Mutex<HashSet<&'static str>>,
    }

    impl MemStorage {
        /// Make every later call to `op` — `"put"`, `"create_folder"` — fail
        /// with a backend-internal error.
        pub(super) fn refuse(&self, op: &'static str) {
            self.refused.lock().unwrap().insert(op);
        }

        /// Every blob key stored in the files block's `bucket`, sorted — what
        /// a test asserts to show that an upload left nothing behind, or
        /// exactly one blob. The storage shim namespaces the folder under the
        /// calling block, so that is the folder this double sees.
        pub(super) fn blob_keys(&self, bucket: &str) -> Vec<String> {
            let folder = format!("{}/{bucket}", crate::blocks::files::FilesBlock::BLOCK_NAME);
            let mut keys: Vec<String> = self
                .objects
                .lock()
                .unwrap()
                .keys()
                .filter(|(f, _)| *f == folder)
                .map(|(_, key)| key.clone())
                .collect();
            keys.sort();
            keys
        }

        fn refusal(&self, op: &str) -> Option<StorageError> {
            self.refused
                .lock()
                .unwrap()
                .contains(op)
                .then(|| StorageError::Internal("simulated storage outage".to_string()))
        }
    }

    #[async_trait]
    impl StorageService for MemStorage {
        async fn put(
            &self,
            folder: &str,
            key: &str,
            data: &[u8],
            content_type: &str,
        ) -> Result<(), StorageError> {
            if let Some(refusal) = self.refusal("put") {
                return Err(refusal);
            }
            self.objects.lock().unwrap().insert(
                (folder.to_string(), key.to_string()),
                (data.to_vec(), content_type.to_string()),
            );
            Ok(())
        }

        async fn get(
            &self,
            folder: &str,
            key: &str,
        ) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
            let guard = self.objects.lock().unwrap();
            let (data, content_type) = guard
                .get(&(folder.to_string(), key.to_string()))
                .ok_or(StorageError::NotFound)?;
            Ok((
                data.clone(),
                ObjectInfo {
                    key: key.to_string(),
                    size: data.len() as i64,
                    content_type: content_type.clone(),
                    last_modified: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
                        .expect("epoch"),
                },
            ))
        }

        async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
            self.objects
                .lock()
                .unwrap()
                .remove(&(folder.to_string(), key.to_string()))
                .map(|_| ())
                .ok_or(StorageError::NotFound)
        }

        async fn list(
            &self,
            _folder: &str,
            _opts: &StoreListOptions,
        ) -> Result<ObjectList, StorageError> {
            Ok(ObjectList {
                objects: vec![],
                total_count: 0,
                next_cursor: None,
            })
        }

        async fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
            if let Some(refusal) = self.refusal("create_folder") {
                return Err(refusal);
            }
            self.folders.lock().unwrap().insert(name.to_string());
            Ok(())
        }

        async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
            if !self.folders.lock().unwrap().remove(name) {
                return Err(StorageError::NotFound);
            }
            self.objects
                .lock()
                .unwrap()
                .retain(|(folder, _), _| folder != name);
            Ok(())
        }

        async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
            Ok(vec![])
        }
    }

    /// [`TestContext::with_files`] plus the **production** `wafer-run/storage`
    /// wiring over [`MemStorage`], so handlers can complete their `store::*`
    /// calls against the same stack the server runs.
    ///
    /// "Production wiring" is the block the runtime registers under that
    /// name (`builder::registration`):
    /// [`crate::blocks::storage::ImpresspressStorageBlock`], whose wafer-core
    /// handler resolves every folder into the calling block's namespace and
    /// authorizes the resolved path. [`TestContext::with_files`] installs the
    /// files block's OWN declared `requires` plus the deployment's grants
    /// (sourced from the admin block's declaration, which is where they live
    /// in production), so a `call_block` or a storage path production refuses
    /// is refused here too.
    pub(super) async fn ctx_with_storage() -> TestContext {
        ctx_with_storage_handle().await.0
    }

    /// [`ctx_with_storage`], plus the backend behind it — so a test can make
    /// storage start failing partway through
    /// ([`MemStorage::refuse`]) and drive a handler's compensation path
    /// against the database state the successful calls left.
    pub(super) async fn ctx_with_storage_handle() -> (TestContext, Arc<MemStorage>) {
        with_storage(TestContext::with_files().await)
    }

    /// [`ctx_with_storage_handle`] on a database that has migration 001 but
    /// NOT 002 — a deployment that took this code without `--run-migrations`,
    /// which `RELEASE.md` explicitly anticipates.
    ///
    /// `TestContext::with_files` applies every migration the block declares,
    /// so no other fixture can reach this state, and the handler behaviour
    /// that must not depend on the unique index would go untested.
    pub(super) async fn ctx_with_storage_without_the_unique_index() -> (TestContext, Arc<MemStorage>)
    {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::files::FilesBlock::BLOCK_NAME);
        // Selected by basename, not by position: `SQLITE_MIGRATIONS[0]` means
        // "001" only for as long as 001 stays first, and a fixture that
        // silently started applying 002 as well would be the INDEXED case
        // while still claiming to be the un-migrated one.
        let sql = crate::blocks::files::migrations::SQLITE_MIGRATIONS
            .iter()
            .find(|(basename, _)| *basename == "001_initial_schema")
            .map(|(_, sql)| *sql)
            .expect("the files block still has its initial-schema migration");
        crate::migration_helper::apply_migrations(&ctx, "impresspress/files", &[sql], &[])
            .await
            .expect("001 applies");
        with_storage(ctx.running_as(crate::blocks::files::FilesBlock::BLOCK_NAME))
    }

    /// [`ctx_with_storage_handle`] on a database with every files migration
    /// but `004_object_claim_id` — a deployment that took this code without
    /// `--run-migrations`, so its objects table has no `claim_id` column.
    pub(super) async fn ctx_with_storage_before_004() -> (TestContext, Arc<MemStorage>) {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::files::FilesBlock::BLOCK_NAME);
        let sql: Vec<&str> = crate::blocks::files::migrations::SQLITE_MIGRATIONS
            .iter()
            .filter(|(basename, _)| *basename != "004_object_claim_id")
            .map(|(_, sql)| *sql)
            .collect();
        assert_eq!(
            sql.len() + 1,
            crate::blocks::files::migrations::SQLITE_MIGRATIONS.len(),
            "004 is still shipped, under this name"
        );
        crate::migration_helper::apply_migrations(&ctx, "impresspress/files", &sql, &[])
            .await
            .expect("001-003 apply");
        with_storage(ctx.running_as(crate::blocks::files::FilesBlock::BLOCK_NAME))
    }

    /// [`ctx_with_storage_handle`] on a database with every files migration
    /// but `005_object_blob_key` — a deployment that took this code without
    /// `--run-migrations`, so its objects table has no `blob_key` column.
    pub(super) async fn ctx_with_storage_before_005() -> (TestContext, Arc<MemStorage>) {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::files::FilesBlock::BLOCK_NAME);
        let sql: Vec<&str> = crate::blocks::files::migrations::SQLITE_MIGRATIONS
            .iter()
            .filter(|(basename, _)| *basename != "005_object_blob_key")
            .map(|(_, sql)| *sql)
            .collect();
        assert_eq!(
            sql.len() + 1,
            crate::blocks::files::migrations::SQLITE_MIGRATIONS.len(),
            "005 is still shipped, under this name"
        );
        crate::migration_helper::apply_migrations(&ctx, "impresspress/files", &sql, &[])
            .await
            .expect("001-004 apply");
        with_storage(ctx.running_as(crate::blocks::files::FilesBlock::BLOCK_NAME))
    }

    fn with_storage(mut ctx: TestContext) -> (TestContext, Arc<MemStorage>) {
        let service = Arc::new(MemStorage::default());
        ctx.register_block(
            "wafer-run/storage",
            crate::blocks::storage::create(service.clone()),
        );
        (ctx, service)
    }
}
