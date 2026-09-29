//! Content-addressed store for compiled guest artifacts.
//!
//! The `blocks/` half of the workspace is source; this is what the compiler
//! produced from it — one `wasm32-wasip1` module per stored sha
//! (`impresspress/dev/artifacts/<sha256>.wasm`, design §11.2).
//!
//! Separate from [`super::blobs`] rather than folded into it because the two
//! stores answer different questions and are collected on different rules: a
//! blob is reachable from a workspace path, an artifact is reachable from a
//! generation's block manifest. Sharing a folder would mean the blob collector
//! had to know about block manifests to avoid deleting a live guest.

use std::sync::Arc;

use wafer_block::hash::sha256_hex;
use wafer_core::{
    clients::storage,
    interfaces::storage::service::{StorageError, StorageService},
};
use wafer_run::{context::Context, ErrorCode, WaferError};

/// Storage folder the artifacts live in, relative to the block's own
/// namespace — `wafer-run/storage` resolves it to
/// `impresspress/dev/artifacts`.
pub const FOLDER: &str = "artifacts";

/// [`FOLDER`] as the object store itself sees it, for the one reader that has
/// no request context to route through `wafer-run/storage`.
///
/// Derived from the block name rather than written out, because that is what
/// `wafer_core::interfaces::storage::handler::resolve_folder` does with an
/// own-namespace folder: `{caller}/{folder}`. A literal here would be a second
/// statement of the namespacing rule, free to drift from the first.
pub fn namespaced_folder() -> String {
    format!("{}/{FOLDER}", super::BLOCK_NAME)
}

/// Content type artifacts are stored under.
const ARTIFACT_CONTENT_TYPE: &str = "application/wasm";

/// Suffix every artifact key carries (design §11.2).
const KEY_SUFFIX: &str = ".wasm";

/// The storage key an artifact with hash `sha` is filed under.
///
/// The `.wasm` suffix is part of the key design §11.2 specifies, so it is
/// derived here rather than spelled out at each call site — a key built two
/// ways is a key that can be built two *different* ways.
pub fn key_for(sha: &str) -> String {
    format!("{sha}{KEY_SUFFIX}")
}

/// The artifact hash a storage key names — the inverse of [`key_for`].
///
/// `None` for a key this module did not write. The garbage collector reads a
/// folder listing back into hashes, and it is the one caller that has to
/// decide what to do with a key it cannot explain; sharing [`KEY_SUFFIX`] with
/// `key_for` is what makes "wrote it" and "recognizes it" the same statement.
pub fn sha_of_key(key: &str) -> Option<&str> {
    key.strip_suffix(KEY_SUFFIX)
}

/// Store `bytes` and return their sha256.
///
/// Idempotent by construction — the key *is* the content.
pub async fn put(ctx: &dyn Context, bytes: &[u8]) -> Result<String, WaferError> {
    let sha = sha256_hex(bytes);
    storage::put(ctx, FOLDER, &key_for(&sha), bytes, ARTIFACT_CONTENT_TYPE).await?;
    Ok(sha)
}

/// Fetch the artifact stored under `sha`.
pub async fn get(ctx: &dyn Context, sha: &str) -> Result<Vec<u8>, WaferError> {
    let (bytes, _info) = storage::get(ctx, FOLDER, &key_for(sha)).await?;
    Ok(bytes)
}

/// [`get`] for a caller that holds the platform `StorageService` rather than
/// a request [`Context`].
///
/// The runtime rebuild is that caller and can be no other:
/// [`super::control::RuntimeControl::rebuild`] is handed a block set and asked
/// to build a `Wafer` from it, and the artifacts it must read are needed
/// *before* there is a runtime to route a `wafer-run/storage` call through —
/// on boot there is not even a request in flight. Sharing [`key_for`] and
/// [`namespaced_folder`] with the context-routed path is what keeps the two
/// readers looking at the same objects.
pub async fn get_direct(
    storage: &Arc<dyn StorageService>,
    sha: &str,
) -> Result<Vec<u8>, WaferError> {
    storage
        .get(&namespaced_folder(), &key_for(sha))
        .await
        .map(|(bytes, _info)| bytes)
        .map_err(|e| {
            let code = match e {
                StorageError::NotFound => ErrorCode::NotFound,
                _ => ErrorCode::Internal,
            };
            WaferError::new(code, format!("reading artifact {sha}: {e}"))
        })
}

/// Whether an artifact is stored under `sha`.
///
/// A keyed streaming read, dropped unread, for the reasons
/// [`super::blobs::exists`] documents: `list` of a folder nothing has written
/// yet is not portable across the storage backends this runs on, it is
/// `O(folder)` on OPFS, and a buffered `get` would transfer up to
/// [`super::validation::MAX_ARTIFACT_BYTES`] to answer a yes or no.
///
/// Nothing in the block asks this — tests use it to observe the store.
/// Activation answers the same question from the builds ledger
/// ([`artifact_index`](super::repo::builds::artifact_index)), which costs no
/// storage call at all — see `activation::missing_content`.
pub async fn exists(ctx: &dyn Context, sha: &str) -> Result<bool, WaferError> {
    match storage::get_stream(ctx, FOLDER, &key_for(sha)).await {
        Ok(_unread) => Ok(true),
        Err(e) if e.code == ErrorCode::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Remove the artifact stored under `sha`.
///
/// For [`super::gc`] only. A block leaving the active set does not delete its
/// artifact: a retained generation can still be rolled back to it.
pub async fn delete(ctx: &dyn Context, sha: &str) -> Result<(), WaferError> {
    match storage::delete(ctx, FOLDER, &key_for(sha)).await {
        // Already gone is the outcome the caller asked for.
        Err(e) if e.code == ErrorCode::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    #[tokio::test]
    async fn an_artifact_round_trips_under_the_hash_of_its_content() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let wasm = b"\0asm\x01\0\0\0";
        let sha = put(&ctx, wasm).await.expect("put");
        assert_eq!(sha, sha256_hex(wasm));
        assert_eq!(get(&ctx, &sha).await.expect("get"), wasm.to_vec());
        assert!(exists(&ctx, &sha).await.expect("exists"));
        assert_eq!(key_for(&sha), format!("{sha}.wasm"));
    }

    /// The rebuild reads artifacts without a `Context`, so the two access
    /// paths have to address the same object. This is the only place that can
    /// be shown: `put` goes through the block's namespacing wrapper and
    /// `get_direct` goes straight to the store, and they must agree on both
    /// the folder and the key.
    #[tokio::test]
    async fn a_context_free_read_finds_what_the_block_stored() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let wasm = b"\0asm\x01\0\0\0direct";
        let sha = put(&ctx, wasm).await.expect("put");

        let storage = ctx.storage_service();
        assert_eq!(
            get_direct(&storage, &sha).await.expect("get_direct"),
            wasm.to_vec()
        );
        assert_eq!(namespaced_folder(), "impresspress/dev/artifacts");
        assert_eq!(
            get_direct(&storage, &sha256_hex(b"never stored"))
                .await
                .expect_err("absent artifact")
                .code,
            ErrorCode::NotFound
        );
    }

    /// The key grammar has to round-trip, or the collector would read a
    /// folder listing back into hashes that name nothing.
    #[test]
    fn a_key_round_trips_through_the_hash_it_names() {
        let sha = sha256_hex(b"module");
        assert_eq!(sha_of_key(&key_for(&sha)), Some(sha.as_str()));
        assert_eq!(sha_of_key(&sha), None, "a key with no suffix is not one");
        assert_eq!(sha_of_key("workspace.json"), None);
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let sha = put(&ctx, b"\0asm\x01\0\0\0gone").await.expect("put");
        delete(&ctx, &sha).await.expect("delete");
        assert!(!exists(&ctx, &sha).await.expect("exists"));
        delete(&ctx, &sha).await.expect("delete again");
    }

    #[tokio::test]
    async fn an_absent_artifact_is_not_reported_present() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        assert!(!exists(&ctx, &sha256_hex(b"never stored"))
            .await
            .expect("exists"));
    }
}
