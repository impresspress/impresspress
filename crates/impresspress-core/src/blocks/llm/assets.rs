//! The LLM block's own browser assets: the chat bundle and the two vendored
//! libraries it loads before itself.
//!
//! They live with the block, not in [`crate::ui::assets`], for the reason
//! `blocks::dev::assets` states for the sandbox's: they are this block's, not
//! the shared chrome's. A build without `block-llm` must not carry them, and
//! no page but the LLM block's own has any use for them. Keeping them here
//! is what lets `ui::assets` name no block at all — `ui::assets::bytes`
//! delegates every block-owned key to its block through
//! [`crate::blocks::static_asset_bytes`].
//!
//! Where this deliberately differs from the sandbox: these stay on the
//! **shared, content-hashed `/b/static/` manifest** rather than moving to a
//! block-local `/b/llm/static/` tier. The sandbox serves its assets itself
//! because they are `Admin`-tier and must never be published; these are
//! public page assets, and the manifest is what makes them detachable — a
//! `--no-default-features` Worker build carries no bytes and streams them
//! from R2, and the CLI publishes them from `ASSETS`. Moving them off the
//! manifest would take that away to buy nothing. Ownership is about which
//! module declares the file, not about which route serves it.

/// marked.js (markdown parser), vendored from marked@14 — self-hosted
/// instead of a jsdelivr CDN `<script>` so there's no external runtime fetch
/// (CSP-friendly, no third-party availability/supply-chain dependency at page
/// load). Only consumed by the chat page ([`super::pages`]).
#[cfg(feature = "embed-assets")]
const MARKED_JS: &str = include_str!("assets/marked.min.js");

/// DOMPurify (HTML sanitizer), vendored from DOMPurify 3.2.4 — self-hosted
/// for the same reasons as marked.js. Loaded before `marked.js`/`llm-chat.js`
/// so `renderMarkdown` can sanitize the parsed markdown before it reaches
/// `innerHTML` (P0 stored-XSS fix).
#[cfg(feature = "embed-assets")]
const PURIFY_JS: &str = include_str!("assets/purify.min.js");

/// The chat surface's vanilla-JS bundle — markdown, message rendering, model
/// management, chat submission, thread creation/selection. Not minified:
/// readability matters for a script that is debugged in devtools.
#[cfg(feature = "embed-assets")]
const LLM_CHAT_JS: &str = include_str!("assets/llm-chat.js");

/// Bytes for this block's manifest assets, or `None` for a key it does not
/// own. Called only through [`crate::blocks::static_asset_bytes`].
#[cfg(feature = "embed-assets")]
pub fn bytes(logical: &str) -> Option<&'static [u8]> {
    Some(match logical {
        "marked.min.js" => MARKED_JS.as_bytes(),
        "purify.min.js" => PURIFY_JS.as_bytes(),
        "llm-chat.js" => LLM_CHAT_JS.as_bytes(),
        _ => return None,
    })
}

/// marked.js URL with content hash, e.g. `/b/static/marked-a1b2c3d4.min.js`.
pub fn marked_js_url() -> String {
    crate::ui::assets::url("marked.min.js")
}

/// DOMPurify URL with content hash, e.g. `/b/static/purify-a1b2c3d4.min.js`.
pub fn purify_js_url() -> String {
    crate::ui::assets::url("purify.min.js")
}

/// Chat bundle URL with content hash, e.g. `/b/static/llm-chat-a1b2c3d4.js`.
pub fn llm_chat_js_url() -> String {
    crate::ui::assets::url("llm-chat.js")
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(feature = "embed-assets")]
    fn llm_chat_js_is_self_invoking_and_exposes_init() {
        let js = super::LLM_CHAT_JS;
        assert!(js.contains("(function ()") || js.contains("(function()"));
        assert!(js.contains("__impresspressLlmChatLoaded"));
        assert!(js.contains("window.impresspressLlmChat = { init: init }"));
        // `init` is the ONLY global. The five handlers used to be re-exported
        // on `window` so the page's `onclick=""`/`onsubmit=""` attributes
        // could reach them; those attributes are `data-action` verbs now and
        // the file binds them itself, so a re-export would be a needless
        // global.
        for sym in [
            "handleChatSubmit",
            "createNewThread",
            "selectThread",
            "onModelChange",
            "unloadLocalModel",
        ] {
            assert!(
                !js.contains(&format!("window.{sym} = {sym}")),
                "{sym} must not be re-exported as a global"
            );
        }
        for verb in ["llm-new-thread", "llm-unload-model", "llm-model-change"] {
            assert!(
                js.contains(&format!("'{verb}'")),
                "the delegated listener must handle {verb}"
            );
        }
    }

    #[test]
    #[cfg(feature = "embed-assets")]
    fn purify_js_is_dompurify_umd_build() {
        let js = super::PURIFY_JS;
        assert!(
            js.contains("DOMPurify"),
            "vendored asset should be DOMPurify"
        );
        // UMD build: `(e=...globalThis...||self).DOMPurify=t()` — assigns
        // onto the global object (`window` in a browser) when there's no
        // CommonJS/AMD module system, which is the load path llm-chat.js
        // relies on for the bare `DOMPurify` global.
        assert!(
            js.contains(").DOMPurify=t()"),
            "expected UMD build to assign a global .DOMPurify"
        );
    }

    #[test]
    fn purify_js_url_has_content_hash() {
        let url = super::purify_js_url();
        assert!(url.starts_with("/b/static/purify-"));
        // Source file is `purify.min.js`; the manifest's `hashed_name` splits
        // on the *first* dot, so the hashed filename keeps the full
        // `.min.js` extension (same shape as `marked-{hash}.min.js`) rather
        // than the pre-manifest ad-hoc `purify-{hash}.js`.
        assert!(url.ends_with(".min.js"));
        let hash = url
            .trim_start_matches("/b/static/purify-")
            .trim_end_matches(".min.js");
        assert_eq!(hash.len(), 8, "expected 8-char short hash, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn llm_chat_js_url_has_content_hash() {
        let url = super::llm_chat_js_url();
        assert!(url.starts_with("/b/static/llm-chat-"));
        assert!(url.ends_with(".js"));
        assert!(
            !url.ends_with(".min.js"),
            "we deliberately ship un-minified"
        );
        let mid = url
            .trim_start_matches("/b/static/llm-chat-")
            .trim_end_matches(".js");
        assert_eq!(mid.len(), 8, "expected 8-char short hash, got: {mid}");
        assert!(mid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The block owns these bytes, and `ui::assets` reaches them only through
    /// the block registry's delegation — never through an arm of its own.
    #[test]
    #[cfg(feature = "embed-assets")]
    fn the_shared_manifest_serves_this_blocks_bytes() {
        for logical in ["marked.min.js", "purify.min.js", "llm-chat.js"] {
            assert_eq!(
                crate::ui::assets::bytes(logical),
                super::bytes(logical),
                "{logical} must resolve to this block's bytes"
            );
            assert!(super::bytes(logical).is_some(), "{logical} not embedded");
        }
    }
}
