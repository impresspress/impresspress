//! The Files block's own browser asset: the file-browser bundle.
//!
//! It lives with the block, not in [`crate::ui::assets`], for the reason
//! `blocks::dev::assets` states for the sandbox's: it is this block's, not
//! the shared chrome's. A build without `block-files` must not carry it, and
//! no page but this block's own storage surfaces has any use for it. Keeping
//! it here is what lets `ui::assets` name no block at all — `ui::assets::bytes`
//! delegates every block-owned key to its block through
//! [`crate::blocks::static_asset_bytes`].
//!
//! It stays on the shared, content-hashed `/b/static/` manifest rather than
//! moving to a block-local tier; see `blocks::llm::assets`'s header for why
//! that half of the sandbox's precedent does not carry over to a public page
//! asset.

/// Vanilla-JS bundle for the file-browser surfaces — drag-drop upload, bulk
/// select, kebab menus, share modal, upload modal, confirm-delete. Consumed
/// by [`super::pages_user::objects::object_list_page`],
/// [`super::pages_user::cloudstorage::cloudstorage_page`] and the admin
/// storage pages.
#[cfg(feature = "embed-assets")]
const FILES_BROWSER_JS: &str = include_str!("assets/files-browser.js");

/// The bundle's source, for the tests that hold the browser and the Rust
/// handlers to one contract — the request fields the share modal sends, the
/// `data-*` attribute the kebab reads, the URL it revokes through.
///
/// Those tests read the shipped file rather than a transcription of it: a
/// transcription is what lets the two sides drift (a share modal posting
/// `expires_days` at a handler reading `expires_in_hours`) while every test
/// stays green. Not gated on `embed-assets`, because the contract holds
/// whether or not this build serves the asset.
#[cfg(test)]
pub(super) const SOURCE: &str = include_str!("assets/files-browser.js");

/// Bytes for this block's manifest assets, or `None` for a key it does not
/// own. Called only through [`crate::blocks::static_asset_bytes`].
#[cfg(feature = "embed-assets")]
pub fn bytes(logical: &str) -> Option<&'static [u8]> {
    Some(match logical {
        "files-browser.js" => FILES_BROWSER_JS.as_bytes(),
        _ => return None,
    })
}

/// File-browser JS URL with content hash, e.g.
/// `/b/static/files-browser-a1b2c3d4.js`.
pub fn files_browser_js_url() -> String {
    crate::ui::assets::url("files-browser.js")
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(feature = "embed-assets")]
    fn files_browser_js_exposes_init_and_handles_drag_drop() {
        let js = super::FILES_BROWSER_JS;
        assert!(
            js.contains("impresspressFilesBrowser"),
            "module namespace missing"
        );
        assert!(js.contains("dragenter"), "drag handler missing");
        assert!(js.contains("dragover"), "drag handler missing");
        assert!(
            js.contains("'drop'") || js.contains("\"drop\""),
            "drop handler missing"
        );
        assert!(js.contains("data-bulk-toggle"), "bulk-select missing");
        assert!(js.contains("data-action-menu"), "kebab handler missing");
        assert!(js.contains("dialog"), "modal uses <dialog>");
    }

    #[test]
    fn files_browser_js_url_has_content_hash() {
        let url = super::files_browser_js_url();
        assert!(url.starts_with("/b/static/files-browser-"));
        assert!(url.ends_with(".js"));
        let hash = url
            .trim_start_matches("/b/static/files-browser-")
            .trim_end_matches(".js");
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The block owns these bytes, and `ui::assets` reaches them only through
    /// the block registry's delegation — never through an arm of its own.
    #[test]
    #[cfg(feature = "embed-assets")]
    fn the_shared_manifest_serves_this_blocks_bytes() {
        assert_eq!(
            crate::ui::assets::bytes("files-browser.js"),
            super::bytes("files-browser.js")
        );
        assert!(super::bytes("files-browser.js").is_some());
    }
}
