//! The `*.workers.dev` preview-host lockdown.
//!
//! A Workers *version preview* URL exposes an unpromoted candidate on a public
//! `workers.dev` host for the width of an atomic deploy. These predicates keep
//! that window closed to everything but the deploy token: see the guard in
//! `run_with_config`, which returns a plain 404 for a preview host and for any
//! `workers.dev` host a consumer has not explicitly opted into with
//! [`ALLOW_WORKERS_DEV_KEY`].

use crate::environment::CfEnvironment;

/// True when the request's host is a `*.workers.dev` host (ASCII
/// case-insensitive). Drives the preview-host lockdown in [`run`]. Two
/// distinct failure modes: a malformed request URL fails via `?` and
/// propagates as an error (the caller turns it into a 500); a well-formed
/// URL with no host (`host_str()` returns `None`) fails open to normal
/// handling — a hostless request can't be a public preview URL.
pub(crate) fn host_is_workers_dev(req: &worker::Request) -> worker::Result<bool> {
    Ok(req
        .url()?
        .host_str()
        .map(|h| h.to_ascii_lowercase().ends_with(".workers.dev"))
        .unwrap_or(false))
}

/// `true` when the request's host is a Workers *version preview* host
/// (`<8 hex>-<worker>.<subdomain>.workers.dev`) for this version: the prefix
/// is the first eight characters of the version id `CF_VERSION_METADATA`
/// reports. Without that binding (local `wrangler dev`, a deploy without the
/// binding) any eight-hex-plus-dash prefix counts — the conservative reading,
/// since a canonical worker name is not normally eight hex characters and a
/// dash.
pub(crate) fn host_is_version_preview(
    req: &worker::Request,
    environment: &CfEnvironment,
) -> worker::Result<bool> {
    let host = req.url()?.host_str().unwrap_or("").to_ascii_lowercase();
    // `CfEnvironment::capture` normalises an empty version id to `None`, and
    // that matters here: `"".starts_with(prefix)` is false for every prefix, so
    // an empty id would make the host read as NOT a version preview and the
    // lockdown would fail OPEN — the opposite of the conservative fallback
    // below, which locks on the hex-prefix pattern alone.
    Ok(is_version_preview_host(&host, environment.worker_version()))
}

/// The pure half of [`host_is_version_preview`].
fn is_version_preview_host(host: &str, version_id: Option<&str>) -> bool {
    // `split` always yields at least one item (including for ""), so this is
    // total rather than a fallible lookup.
    let first_label = host.split('.').next().unwrap_or(host);
    let Some((prefix, _worker)) = first_label.split_once('-') else {
        return false;
    };
    let looks_like_preview_prefix =
        prefix.len() == 8 && prefix.bytes().all(|b| b.is_ascii_hexdigit());
    match version_id {
        Some(id) => looks_like_preview_prefix && id.to_ascii_lowercase().starts_with(prefix),
        None => looks_like_preview_prefix,
    }
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// The workers.dev opt-in must never open a version preview: the atomic
    /// deploy proves an unpromoted candidate is unreachable before promoting.
    #[wasm_bindgen_test]
    fn version_preview_host_is_recognised_by_its_own_version_prefix() {
        let id = Some("c89ac716-7f68-437c-8484-640b5b9f2b42");
        assert!(is_version_preview_host(
            "c89ac716-impresspress-webmcp-demo.jorissuppers.workers.dev",
            id
        ));
        assert!(!is_version_preview_host(
            "impresspress-webmcp-demo.jorissuppers.workers.dev",
            id
        ));
        // Another version's preview prefix is not this version's.
        assert!(!is_version_preview_host(
            "30fbdf19-impresspress-webmcp-demo.jorissuppers.workers.dev",
            id
        ));
        // A worker whose name happens to start with eight hex characters and
        // a dash is mistaken for a preview only when no version id is known.
        assert!(!is_version_preview_host(
            "deadbeef-shop.example.workers.dev",
            id
        ));
        assert!(is_version_preview_host(
            "deadbeef-shop.example.workers.dev",
            None
        ));
    }

    /// The lockdown must fail CLOSED when the version id is unusable.
    ///
    /// `CF_VERSION_METADATA` can be present and yield an empty id (wrangler
    /// dev, a hand-written `[version_metadata]` on an unversioned deploy).
    /// That reaches this function as `Some("")`, and `"".starts_with(prefix)`
    /// is false for every prefix — so before the `.filter(|id| !id.is_empty())`
    /// on the caller, an opt-in consumer served the full app anonymously on
    /// its unpromoted preview URL.
    #[wasm_bindgen_test]
    fn an_unusable_version_id_still_locks_the_preview_host() {
        // What the caller now passes for an empty id.
        assert!(is_version_preview_host(
            "c89ac716-impresspress-webmcp-demo.jorissuppers.workers.dev",
            None
        ));
        // And the shape that used to slip through, asserted directly.
        assert!(
            !is_version_preview_host(
                "c89ac716-impresspress-webmcp-demo.jorissuppers.workers.dev",
                Some("")
            ),
            "an empty id cannot match any prefix — the caller must not pass it"
        );
        // The canonical host stays open on both.
        assert!(!is_version_preview_host(
            "impresspress-webmcp-demo.jorissuppers.workers.dev",
            None
        ));
    }

    /// The function requires a pre-lowercased host — its only caller
    /// lowercases, and that invariant is invisible from the signature.
    #[wasm_bindgen_test]
    fn version_preview_matching_is_case_insensitive_on_the_id() {
        assert!(is_version_preview_host(
            "c89ac716-worker.example.workers.dev",
            Some("C89AC716-7F68-437C-8484-640B5B9F2B42")
        ));
    }
}
