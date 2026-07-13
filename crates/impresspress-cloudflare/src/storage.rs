//! Async storage service backed by Cloudflare R2.
//!
//! Implements the shared `StorageService` trait from wafer-core so R2Block
//! can reuse the shared message handler.

use wafer_core::interfaces::storage::service::{
    FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
};
use worker::*;

/// Async storage service wrapping Cloudflare R2.
/// Each project has its own R2 bucket — no tenant prefix needed.
pub struct R2StorageService {
    bucket: Bucket,
}

// SAFETY: `R2StorageService` only holds a `worker::Bucket` handle, which is
// scoped to a single Worker isolate. wasm32-unknown-unknown has no threads,
// so the `Send`/`Sync` bounds required by `Arc<dyn StorageService>` are
// satisfied trivially — no cross-thread aliasing is possible.
unsafe impl Send for R2StorageService {}
unsafe impl Sync for R2StorageService {}

impl R2StorageService {
    pub fn new(bucket: Bucket) -> Self {
        Self { bucket }
    }

    fn prefixed_key(&self, folder: &str, key: &str) -> String {
        format!("{folder}/{key}")
    }

    fn folder_prefix(&self, folder: &str) -> String {
        format!("{folder}/")
    }
}

/// Convert an R2 `Date` (JS milliseconds since epoch) into a chrono UTC time.
/// Falls back to `Utc::now()` only if R2 returns a value outside chrono's
/// representable range, which in practice cannot happen for real objects.
fn r2_date_to_chrono(d: worker::Date) -> chrono::DateTime<chrono::Utc> {
    let millis = d.as_millis() as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis).unwrap_or_else(chrono::Utc::now)
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl StorageService for R2StorageService {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        self.bucket
            .put(&r2_key, data.to_vec())
            .http_metadata(HttpMetadata {
                content_type: Some(content_type.to_string()),
                ..Default::default()
            })
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        let obj = self
            .bucket
            .get(&r2_key)
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?
            .ok_or(StorageError::NotFound)?;

        let body = obj
            .body()
            .ok_or_else(|| StorageError::Internal("no body".into()))?;
        let bytes = body
            .bytes()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        let info = ObjectInfo {
            key: key.to_string(),
            size: bytes.len() as i64,
            content_type: obj
                .http_metadata()
                .content_type
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            last_modified: r2_date_to_chrono(obj.uploaded()),
        };

        Ok((bytes, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        let r2_key = self.prefixed_key(folder, key);
        self.bucket
            .delete(&r2_key)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let full_prefix = if opts.prefix.is_empty() {
            self.folder_prefix(folder)
        } else {
            format!("{}{}", self.folder_prefix(folder), opts.prefix)
        };

        let limit = if opts.limit > 0 {
            opts.limit as u32
        } else {
            100
        };

        let listed = self
            .bucket
            .list()
            .prefix(&full_prefix)
            .limit(limit)
            .execute()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;

        let folder_prefix_len = self.folder_prefix(folder).len();

        let objects: Vec<ObjectInfo> = listed
            .objects()
            .iter()
            .map(|obj| {
                let full_key = obj.key();
                let key = if full_key.len() > folder_prefix_len {
                    full_key[folder_prefix_len..].to_string()
                } else {
                    full_key
                };

                ObjectInfo {
                    key,
                    size: obj.size() as i64,
                    content_type: "application/octet-stream".to_string(),
                    last_modified: r2_date_to_chrono(obj.uploaded()),
                }
            })
            .collect();

        let count = objects.len() as i64;
        Ok(ObjectList {
            objects,
            total_count: count,
        })
    }

    async fn create_folder(&self, _name: &str, _public: bool) -> Result<(), StorageError> {
        // R2 doesn't need explicit folder creation — objects create the path
        Ok(())
    }

    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        // R2 has no native folder/directory concept — deleting a "folder"
        // means listing every object under its prefix and deleting each one.
        // `list()` returns at most 1000 objects per page (R2's own cap), so
        // we page through with the cursor until `truncated` is false,
        // batch-deleting each page via `delete_multiple` (also capped at
        // 1000 keys per call — same limit, so one `delete_multiple` per
        // page is always within bounds).
        let prefix = self.folder_prefix(name);
        let mut cursor: Option<String> = None;

        loop {
            let mut list_builder = self.bucket.list().prefix(&prefix).limit(1000);
            if let Some(c) = cursor.take() {
                list_builder = list_builder.cursor(c);
            }

            let listed = list_builder
                .execute()
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            let keys: Vec<String> = listed.objects().iter().map(|obj| obj.key()).collect();
            if !keys.is_empty() {
                self.bucket
                    .delete_multiple(keys)
                    .await
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
            }

            if !listed.truncated() {
                break;
            }
            cursor = listed.cursor();
            if cursor.is_none() {
                // `truncated() == true` is documented to always come with a
                // cursor. If that contract is ever violated, we cannot tell
                // whether more objects remain under this prefix — falling
                // through to `Ok(())` would silently report success on a
                // partial delete. Fail loudly instead.
                return Err(StorageError::Internal(
                    "R2 reported truncated results with no cursor".into(),
                ));
            }
        }

        Ok(())
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        // R2 doesn't have a native folder concept
        Ok(Vec::new())
    }
}
