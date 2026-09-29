//! Content-addressed blob store for the sandbox workspace.
//!
//! Every byte the workspace holds lives here exactly once, keyed by the
//! SHA-256 of its content. Nothing is ever edited in place: a write stores a
//! new blob and repoints the manifest entry, so a generation that names a
//! sha keeps naming the same bytes for as long as the blob exists, and a
//! rollback needs no copy.
//!
//! Blobs are never deleted by a file delete — an older generation may still
//! reference them (design §7.2). Reclaiming unreachable blobs is Plan 4's
//! garbage collector, which is why [`delete`] exists but no handler calls it.

use wafer_core::clients::storage;
use wafer_run::{context::Context, ErrorCode, WaferError};

/// Storage folder the blobs live in, relative to the block's own namespace —
/// `wafer-run/storage` resolves it to `impresspress/dev/blobs` (see
/// [`crate::blocks::storage`]).
pub const FOLDER: &str = "blobs";

/// Content type blobs are stored under. The *file's* content type is a
/// property of the path that names the blob, not of the blob: the same bytes
/// can be reachable as `site/a.txt` and `blocks/x/README`, so it is recorded
/// on the manifest entry ([`super::workspace::FileEntry::content_type`]) and
/// never here.
const BLOB_CONTENT_TYPE: &str = "application/octet-stream";

/// SHA-256 of `bytes`, lowercase hex — the key a blob is stored under.
///
/// Re-exported rather than reimplemented: `wafer_block::hash` is already the
/// single source of this hash for the whole runtime, and a second
/// implementation is a second thing that can disagree with a stored key.
pub use wafer_block::hash::sha256_hex;

/// What a [`put`] did with the bytes it was given.
///
/// The caller needs to know: the workspace's blob-byte accounting
/// ([`super::workspace::Workspace::blob_bytes`]) may only grow when the store
/// actually grew, and a `Deduplicated` write costs the store nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    /// The bytes were not in the store and were written.
    New,
    /// The bytes were already stored under this hash; nothing was written.
    Deduplicated,
}

/// Store `bytes` and return their sha256 plus whether the store grew.
///
/// Idempotent by construction — the key *is* the content — and skips the
/// write when the blob is already present, which is the common case: a page
/// saved twice, or two paths holding the same asset.
pub async fn put(ctx: &dyn Context, bytes: &[u8]) -> Result<(String, Stored), WaferError> {
    let sha = sha256_hex(bytes);
    let stored = put_hashed(ctx, &sha, bytes).await?;
    Ok((sha, stored))
}

/// [`put`] for a caller that has already hashed `bytes`.
///
/// The write handler needs the hash *before* it stores, to decide how much
/// quota headroom the write needs; hashing half a megabyte twice to get the
/// same answer is the duplication this exists to avoid. `sha` must be
/// [`sha256_hex`] of `bytes` — passing anything else would file the content
/// under a key that does not describe it, and every later read would be a
/// silent mismatch.
pub async fn put_hashed(ctx: &dyn Context, sha: &str, bytes: &[u8]) -> Result<Stored, WaferError> {
    debug_assert_eq!(sha, sha256_hex(bytes), "blob key must be the content hash");
    if exists(ctx, sha).await? {
        return Ok(Stored::Deduplicated);
    }
    storage::put(ctx, FOLDER, sha, bytes, BLOB_CONTENT_TYPE).await?;
    Ok(Stored::New)
}

/// Fetch the blob stored under `sha`.
pub async fn get(ctx: &dyn Context, sha: &str) -> Result<Vec<u8>, WaferError> {
    let (bytes, _info) = storage::get(ctx, FOLDER, sha).await?;
    Ok(bytes)
}

/// Whether a blob is stored under `sha`.
///
/// A keyed read rather than a prefix `list`, for two reasons that both matter
/// on the sandbox's own target:
///
/// * a read of an absent key is `NotFound` on every backend; what `list` does
///   with a folder nothing has written yet is not part of the
///   `StorageService` contract, and the backends genuinely differ —
///   `wafer-block-local-storage` answers an empty listing, the OPFS backend
///   in `impresspress-browser` rejects with `NotFoundError`. A blob store
///   asks this question before its first write has created the folder.
/// * `list` is `O(folder)` on OPFS (the bridge walks the whole directory tree
///   and filters in JS), so a prefix probe would make every write walk every
///   blob.
///
/// And a *streaming* read, dropped unread: the object's metadata arrives
/// ahead of its body, so the answer costs an open and at most the few chunks
/// the backend's reader pulls before it sees the consumer has gone — not the
/// whole blob, which the buffered `storage::get` would transfer to answer a yes
/// or no (up to [`super::paths::MAX_FILE_BYTES`] per call, on the write path).
/// Dropping the stream is how the body is declined: the OPFS backend the
/// sandbox runs on releases its reader when the consumer goes away.
pub async fn exists(ctx: &dyn Context, sha: &str) -> Result<bool, WaferError> {
    match storage::get_stream(ctx, FOLDER, sha).await {
        Ok(_unread) => Ok(true),
        Err(e) if e.code == ErrorCode::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Remove the blob stored under `sha`.
///
/// For the garbage collector only. A file delete leaves the blob alone: it may
/// still be named by a generation that can be rolled back to.
pub async fn delete(ctx: &dyn Context, sha: &str) -> Result<(), WaferError> {
    match storage::delete(ctx, FOLDER, sha).await {
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
    async fn a_blob_round_trips_under_the_hash_of_its_content() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let (sha, stored) = put(&ctx, b"hello").await.expect("put");
        assert_eq!(sha, sha256_hex(b"hello"));
        assert_eq!(stored, Stored::New);
        assert_eq!(get(&ctx, &sha).await.expect("get"), b"hello".to_vec());
        assert!(exists(&ctx, &sha).await.expect("exists"));
    }

    /// The second put must report `Deduplicated`, not just return the same
    /// hash: that flag is what stops the workspace's blob-byte accounting
    /// from charging twice for one stored copy.
    #[tokio::test]
    async fn putting_the_same_bytes_twice_is_the_same_blob() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let (first, first_stored) = put(&ctx, b"hello").await.expect("put");
        let (second, second_stored) = put(&ctx, b"hello").await.expect("put again");
        assert_eq!(first, second);
        assert_eq!(first_stored, Stored::New);
        assert_eq!(second_stored, Stored::Deduplicated);

        let listing = storage::list(
            &ctx,
            FOLDER,
            &wafer_core::clients::storage::ListOptions::default(),
        )
        .await
        .expect("list");
        assert_eq!(listing.objects.len(), 1);
    }

    #[tokio::test]
    async fn put_hashed_is_put_without_the_second_hash() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let sha = sha256_hex(b"hello");
        assert_eq!(
            put_hashed(&ctx, &sha, b"hello").await.expect("store"),
            Stored::New
        );
        assert_eq!(
            put_hashed(&ctx, &sha, b"hello").await.expect("store again"),
            Stored::Deduplicated
        );
        assert_eq!(get(&ctx, &sha).await.expect("get"), b"hello".to_vec());
    }

    #[tokio::test]
    async fn an_absent_blob_is_not_reported_present_and_reads_as_not_found() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let sha = sha256_hex(b"never stored");
        assert!(!exists(&ctx, &sha).await.expect("exists"));
        assert_eq!(
            get(&ctx, &sha).await.expect_err("get must fail").code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let (sha, _) = put(&ctx, b"hello").await.expect("put");
        delete(&ctx, &sha).await.expect("delete");
        assert!(!exists(&ctx, &sha).await.expect("exists"));
        delete(&ctx, &sha).await.expect("delete again");
    }

    /// The blob namespace belongs to the dev block, not to whoever asked.
    ///
    /// The negative half has to name an object that REALLY EXISTS in the
    /// foreign namespace, otherwise it proves nothing: a `get` of a key
    /// nothing ever stored is `NotFound` whether or not WRAP is enforcing.
    /// So another block's identity puts the object first — through the same
    /// registered storage block, which namespaces it under
    /// `impresspress/files/uploads` — and only then does the dev block reach
    /// for it and get `PermissionDenied`.
    #[tokio::test]
    async fn a_foreign_namespace_is_refused_even_when_the_object_is_there() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;

        // Act as `impresspress/files`: a shallow clone of the fixture running
        // as another block, sharing the same storage block and the same
        // backing store.
        let files_block = ctx.clone().running_as("impresspress/files");
        storage::put(
            &files_block,
            "uploads",
            "secret.txt",
            b"private",
            "text/plain",
        )
        .await
        .expect("the owning block may write its own namespace");

        // It is genuinely there — for its owner.
        let (bytes, _info) = storage::get(&files_block, "uploads", "secret.txt")
            .await
            .expect("the owning block may read it back");
        assert_eq!(bytes, b"private".to_vec());

        // The dev block holds no grant on it, so the same object is refused —
        // and refused as a permission failure, not as a miss.
        let denied = storage::get(&ctx, "@impresspress/files/uploads", "secret.txt")
            .await
            .expect_err("cross-namespace read must be refused");
        assert_eq!(denied.code, ErrorCode::PermissionDenied);

        // The block's own namespace still resolves, so the refusal above is
        // about the namespace and not about storage being broken.
        let (sha, _) = put(&ctx, b"hello").await.expect("put");
        assert_eq!(get(&ctx, &sha).await.expect("own namespace"), b"hello");

        // A global folder enumeration is admin-only, so the sandbox cannot
        // use it to discover what other blocks have stored.
        assert_eq!(
            storage::list_folders(&ctx)
                .await
                .expect_err("list_folders is admin-only")
                .code,
            ErrorCode::PermissionDenied,
        );
    }

    /// The counterpart of the refusal above: the ONE namespace the sandbox is
    /// granted — the published site — is reachable. Without this, the test
    /// above would pass just as well if every cross-block reach were broken.
    #[tokio::test]
    async fn the_granted_site_namespace_is_reachable() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        storage::put(
            &ctx,
            "@wafer-run/web/site",
            "index.html",
            b"<h1>hi</h1>",
            "text/html; charset=utf-8",
        )
        .await
        .expect("the dev block is granted wafer-run/web/site/*");
        let (bytes, _info) = storage::get(&ctx, "@wafer-run/web/site", "index.html")
            .await
            .expect("and may read it back");
        assert_eq!(bytes, b"<h1>hi</h1>".to_vec());
    }
}
