//! Opaque continuation-token codec for the browser (OPFS) storage backend.
//!
//! This backend gives callers the SAME opaque-token contract as every other
//! WAFER storage backend (wafer-run #318): [`ListOptions::cursor`] resumes a
//! walk, [`ObjectList::next_cursor`] advances it, and the token is a URL-safe
//! unpadded base64 string the caller must treat as opaque and feed back only to
//! this backend.
//!
//! **Envelope — identical to `wafer-block-local-storage`.** Both backends wrap
//! their token with `base64ct::Base64UrlUnpadded` (the same crate + alphabet),
//! so the token *shape* is consistent everywhere and there is a single writer
//! per backend — no second writer in a divergent format.
//!
//! **Payload — backend-specific, as the contract requires.** A cursor is never
//! valid across backends ([`ObjectList::next_cursor`] docs), so the bytes inside
//! the envelope are the backend's own business. The filesystem backend holds
//! the whole sorted key slice in memory and encodes the *last returned key*
//! (resuming via `partition_point`). This backend paginates through the
//! offset-based `storageList` bridge — it never holds the full slice in Rust —
//! so it encodes the *next page's start offset*. Both are the base64 of a short
//! UTF-8 string; only the string's meaning differs, and the two never mix.
//!
//! Split out of the wasm32-only `storage` module (which uses `crate::bridge`,
//! and so only compiles on `wasm32-unknown-unknown`) so this pure codec
//! unit-tests on the host — same arrangement as `db_codec`. It's compiled on
//! wasm32 (real use) or under `test` (host unit tests), never on plain native
//! builds where it would be dead — see `lib.rs`.
//!
//! [`ListOptions::cursor`]: wafer_core::interfaces::storage::service::ListOptions::cursor
//! [`ObjectList::next_cursor`]: wafer_core::interfaces::storage::service::ObjectList::next_cursor

use base64ct::{Base64UrlUnpadded, Encoding};
use wafer_core::interfaces::storage::service::StorageError;

/// Encode a resume offset as this backend's opaque list cursor.
///
/// Mirrors `wafer-block-local-storage::encode_cursor`'s envelope exactly:
/// URL-safe unpadded base64 of a short UTF-8 string (there, the last key; here,
/// the decimal start offset), keeping the token opaque and safe to round-trip
/// through the wire / URLs.
pub(crate) fn encode(offset: u64) -> String {
    Base64UrlUnpadded::encode_string(offset.to_string().as_bytes())
}

/// Decode an opaque list cursor back to the resume offset it was minted from.
///
/// An empty cursor means "before the first object" → offset 0 (this is how a
/// caller *begins* a cursor walk, `Some(String::new())`), matching
/// `wafer-block-local-storage::cursor_start_index`. A cursor that isn't valid
/// base64 / UTF-8 / an offset is a client error, surfaced as
/// [`StorageError::Internal`] with the same message shape as the local-storage
/// backend.
pub(crate) fn decode(cursor: &str) -> Result<u64, StorageError> {
    if cursor.is_empty() {
        return Ok(0);
    }
    let bytes = Base64UrlUnpadded::decode_vec(cursor)
        .map_err(|e| StorageError::Internal(format!("invalid storage list cursor: {e}")))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| {
        StorageError::Internal(format!("invalid storage list cursor (not utf-8): {e}"))
    })?;
    text.parse::<u64>().map_err(|e| {
        StorageError::Internal(format!("invalid storage list cursor (not an offset): {e}"))
    })
}

/// Clamp a resume offset to the `u32` domain of the `storageList` bridge
/// parameter. OPFS object counts never approach `u32::MAX`; the clamp only
/// guards against a hand-forged out-of-range cursor truncating silently.
pub(crate) fn clamp_offset(offset: u64) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}

/// Decide the continuation token for the *next* page.
///
/// Returns `Some(token)` only in cursor mode AND only when more objects follow
/// this page (`start + page_len < total`); returns `None` on the final page and
/// for every offset-mode call. This mirrors `wafer-block-local-storage::list`'s
/// rule exactly — offset callers get `None` and use `total_count` for has-more,
/// cursor callers use `next_cursor.is_some()`.
pub(crate) fn next_page_cursor(
    cursor_mode: bool,
    start: u64,
    page_len: u64,
    total: u64,
) -> Option<String> {
    let end = start.saturating_add(page_len);
    if cursor_mode && end < total {
        Some(encode(end))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trips() {
        for offset in [0u64, 1, 2, 25, 1000, u32::MAX as u64] {
            let token = encode(offset);
            assert_eq!(
                decode(&token).unwrap(),
                offset,
                "round-trip offset {offset}"
            );
        }
    }

    #[test]
    fn empty_cursor_starts_before_first_object() {
        // `Some(String::new())` begins a cursor walk at offset 0.
        assert_eq!(decode("").unwrap(), 0);
    }

    #[test]
    fn encoded_token_is_opaque_url_safe_base64() {
        let token = encode(25);
        // No padding, no URL-unsafe chars — same envelope as local-storage.
        assert!(!token.contains('='));
        assert!(!token.contains('+'));
        assert!(!token.contains('/'));
        // And it is NOT the bare decimal (callers must not parse it).
        assert_ne!(token, "25");
    }

    #[test]
    fn malformed_base64_is_client_error() {
        match decode("not valid base64!!!") {
            Err(StorageError::Internal(msg)) => {
                assert!(msg.contains("invalid storage list cursor"), "got: {msg}");
            }
            other => panic!("expected StorageError::Internal, got {other:?}"),
        }
    }

    #[test]
    fn non_offset_payload_is_client_error() {
        // Valid base64 of a non-numeric string — decodes to UTF-8 but not to an
        // offset.
        let token = Base64UrlUnpadded::encode_string(b"some/object/key.txt");
        match decode(&token) {
            Err(StorageError::Internal(msg)) => {
                assert!(msg.contains("not an offset"), "got: {msg}");
            }
            other => panic!("expected StorageError::Internal, got {other:?}"),
        }
    }

    #[test]
    fn clamp_offset_passes_realistic_values_and_saturates_out_of_range() {
        assert_eq!(clamp_offset(0), 0);
        assert_eq!(clamp_offset(1000), 1000);
        assert_eq!(clamp_offset(u32::MAX as u64), u32::MAX);
        // A hand-forged cursor beyond the bridge's u32 domain saturates rather
        // than truncating to a bogus small offset.
        assert_eq!(clamp_offset(u32::MAX as u64 + 1), u32::MAX);
        assert_eq!(clamp_offset(u64::MAX), u32::MAX);
    }

    #[test]
    fn offset_mode_never_emits_next_cursor() {
        // Even with more objects remaining, offset callers (cursor_mode = false)
        // get `None` — behavior is unchanged from before #318.
        assert_eq!(next_page_cursor(false, 0, 10, 100), None);
        assert_eq!(next_page_cursor(false, 90, 10, 100), None);
    }

    #[test]
    fn cursor_mode_emits_next_cursor_while_more_remain() {
        // Page of 10 out of 100, starting at 0 → resume at offset 10.
        let next = next_page_cursor(true, 0, 10, 100).expect("more remain");
        assert_eq!(decode(&next).unwrap(), 10);
    }

    #[test]
    fn cursor_mode_final_page_has_no_next_cursor() {
        // Last page exactly fills the keyspace → no continuation.
        assert_eq!(next_page_cursor(true, 90, 10, 100), None);
        // Short final page (fewer than a full limit) → also no continuation.
        assert_eq!(next_page_cursor(true, 95, 5, 100), None);
    }

    #[test]
    fn cursor_mode_empty_result_has_no_next_cursor() {
        assert_eq!(next_page_cursor(true, 0, 0, 0), None);
    }

    #[test]
    fn full_cursor_walk_visits_every_page_then_stops() {
        // Simulate the bridge over a 25-object store, limit 10: the walk must
        // yield pages [0,10), [10,20), [20,25) and then stop with `None`, never
        // skipping or repeating an offset.
        const TOTAL: u64 = 25;
        const LIMIT: u64 = 10;

        let mut visited_starts = Vec::new();
        // Begin the walk with an empty cursor (offset 0).
        let mut cursor_token = Some(String::new());

        while let Some(token) = cursor_token {
            let start = decode(&token).unwrap();
            visited_starts.push(start);
            // The bridge returns keys[start .. min(start+LIMIT, TOTAL)].
            let end = (start + LIMIT).min(TOTAL);
            let page_len = end - start;
            cursor_token = next_page_cursor(true, start, page_len, TOTAL);
        }

        assert_eq!(
            visited_starts,
            vec![0, 10, 20],
            "walk must page contiguously and terminate on the final page"
        );
    }
}
