//! Embedded static assets — CSS and JS.
//!
//! Asset URLs include a content hash for cache busting:
//! `/b/static/app-{hash}.css` and `/b/static/htmx-{hash}.min.js`

use std::sync::OnceLock;

use crate::routing::STATIC_PREFIX;

const TOKENS_CSS: &str = include_str!("assets/tokens.css");
const BASE_CSS: &str = include_str!("assets/base.css");
const COMPONENTS_CSS: &str = include_str!("assets/components.css");
const LAYOUT_CSS: &str = include_str!("assets/layout.css");
const CHARTS_CSS: &str = include_str!("assets/charts.css");

/// Itim font binaries, sourced from `impresspress/site-kit`'s `/fonts/` mirror
/// and committed here so every impresspress deployment ships its own glyphs
/// (no cross-origin runtime dependency, no `https://impresspress.org/fonts/` 404).
const ITIM_LATIN_WOFF2: &[u8] = include_bytes!("assets/fonts/itim-latin.woff2");
const ITIM_LATIN_EXT_WOFF2: &[u8] = include_bytes!("assets/fonts/itim-latin-ext.woff2");

/// Square Impresspress mark used as the sidebar/login icon. Bundled locally so
/// the admin renders correctly without internet (the previous default pointed
/// at `https://impresspress.org/images/logo.png` which 404s offline).
const LOGO_ICON_PNG: &[u8] = include_bytes!("assets/impresspress-logo.png");
/// Impresspress wordmark/long logo — used in the sidebar brand and login splash.
const LOGO_LONG_PNG: &[u8] = include_bytes!("assets/impresspress-logo-long.png");
/// Impresspress favicon — bundled so every deployment ships its own `<link rel="icon">`
/// target without depending on a per-deployment external URL or the implicit
/// browser fallback to `/favicon.ico` (which 404s by default).
const FAVICON_ICO: &[u8] = include_bytes!("assets/favicon.ico");

/// The built-in brand accent, as a hex literal for surfaces that can't use
/// CSS variables (email inline styles). Must match `--primary-color` in
/// `assets/tokens.css` — the `brand_accent_matches_tokens_css` test pins the
/// two together so they can't drift.
pub const BRAND_ACCENT_HEX: &str = "#f0480f";

/// Impresspress square logo bytes.
pub fn logo_icon_png() -> &'static [u8] {
    LOGO_ICON_PNG
}

/// Impresspress long/wordmark logo bytes.
pub fn logo_long_png() -> &'static [u8] {
    LOGO_LONG_PNG
}

/// Square logo URL with content hash, e.g. `/b/static/impresspress-logo-a1b2c3d4.png`.
pub fn logo_icon_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}impresspress-logo-{}.png",
            short_hash(LOGO_ICON_PNG)
        )
    })
}

/// Long/wordmark logo URL with content hash, e.g. `/b/static/impresspress-logo-long-a1b2c3d4.png`.
pub fn logo_long_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}impresspress-logo-long-{}.png",
            short_hash(LOGO_LONG_PNG)
        )
    })
}

/// Impresspress favicon bytes.
pub fn favicon_ico() -> &'static [u8] {
    FAVICON_ICO
}

/// Favicon URL with content hash, e.g. `/b/static/favicon-a1b2c3d4.ico`.
pub fn favicon_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| format!("{STATIC_PREFIX}favicon-{}.ico", short_hash(FAVICON_ICO)))
}

/// Itim latin (basic) woff2 bytes.
pub fn itim_latin_woff2() -> &'static [u8] {
    ITIM_LATIN_WOFF2
}

/// Itim latin-ext woff2 bytes.
pub fn itim_latin_ext_woff2() -> &'static [u8] {
    ITIM_LATIN_EXT_WOFF2
}

/// Itim latin woff2 URL with content hash, e.g. `/b/static/itim-latin-a1b2c3d4.woff2`.
pub fn itim_latin_woff2_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}itim-latin-{}.woff2",
            short_hash(ITIM_LATIN_WOFF2)
        )
    })
}

/// Itim latin-ext woff2 URL with content hash, e.g. `/b/static/itim-latin-ext-a1b2c3d4.woff2`.
pub fn itim_latin_ext_woff2_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}itim-latin-ext-{}.woff2",
            short_hash(ITIM_LATIN_EXT_WOFF2)
        )
    })
}

/// Embedded CSS bundle — concatenation of tokens / base / components / layout.
/// Served as one file at the URL returned by `css_url()` so a single
/// `<link rel="stylesheet">` covers everything. The two font URL placeholders
/// in tokens.css are substituted with the content-hashed worker-bundled
/// URLs at bundle generation time.
///
/// Cached in a `OnceLock`: every CSS-bundle consumer (`css()`, `css_url()`,
/// and the static-asset handler) used to rebuild this on each call.
pub fn css_bundle() -> &'static str {
    static BUNDLE: OnceLock<String> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let tokens = TOKENS_CSS
            .replace("__ITIM_LATIN_URL__", itim_latin_woff2_url())
            .replace("__ITIM_LATIN_EXT_URL__", itim_latin_ext_woff2_url());
        format!("{tokens}\n{BASE_CSS}\n{COMPONENTS_CSS}\n{LAYOUT_CSS}\n{CHARTS_CSS}\n")
    })
}

/// The main CSS stylesheet (all design system styles combined).
pub fn css() -> &'static str {
    css_bundle()
}

/// htmx 2.x minified JS.
pub fn htmx_js() -> &'static str {
    include_str!("assets/htmx.min.js")
}

/// Short content hash (first 8 chars of hex SHA-256) for cache busting.
fn short_hash(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(content);
    hash.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// CSS URL with content hash, e.g. `/b/static/app-a1b2c3d4.css`
pub fn css_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| format!("{STATIC_PREFIX}app-{}.css", short_hash(css().as_bytes())))
}

/// htmx JS URL with content hash, e.g. `/b/static/htmx-a1b2c3d4.min.js`
pub fn htmx_js_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}htmx-{}.min.js",
            short_hash(htmx_js().as_bytes())
        )
    })
}

/// marked.js (markdown parser), vendored from marked@14 — self-hosted instead of
/// a jsdelivr CDN `<script>` so there's no external runtime fetch (CSP-friendly,
/// no third-party availability/supply-chain dependency at page load).
///
/// Only consumed by the LLM chat page (`blocks::llm::pages`), which is itself
/// gated behind `block-llm` — feature-gated here too so a build without the
/// LLM block (e.g. Cloudflare, which can't enable `block-llm`: the provider
/// service isn't wasm32-compatible) doesn't embed this JS.
#[cfg(feature = "block-llm")]
pub fn marked_js() -> &'static str {
    include_str!("assets/marked.min.js")
}

/// marked.js URL with content hash, e.g. `/b/static/marked-a1b2c3d4.min.js`
#[cfg(feature = "block-llm")]
pub fn marked_js_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}marked-{}.min.js",
            short_hash(marked_js().as_bytes())
        )
    })
}

/// DOMPurify (HTML sanitizer), vendored from DOMPurify 3.2.4 — self-hosted
/// instead of a CDN `<script>` so there's no external runtime fetch
/// (CSP-friendly, no third-party availability/supply-chain dependency at
/// page load). Loaded before `marked.js`/`llm-chat.js` so `renderMarkdown`
/// can sanitize the parsed markdown before it reaches `innerHTML`
/// (P0 stored-XSS fix).
///
/// Like `marked_js`, only the LLM chat page loads this — gated behind
/// `block-llm` for the same reason.
#[cfg(feature = "block-llm")]
const PURIFY_JS: &str = include_str!("assets/purify.min.js");

#[cfg(feature = "block-llm")]
pub fn purify_js() -> &'static str {
    PURIFY_JS
}

/// DOMPurify JS URL with content hash, e.g. `/b/static/purify-a1b2c3d4.js`
#[cfg(feature = "block-llm")]
pub fn purify_js_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}purify-{}.js",
            short_hash(purify_js().as_bytes())
        )
    })
}

/// Embedded vanilla-JS bundle for the LLM chat surface — markdown, message
/// rendering, model management, chat submission, thread creation/selection.
/// Consumed by the unified LLM page handler and (for the conversation lens)
/// by the Messages context_detail handler.
///
/// Gated behind `block-llm`: Cloudflare currently cannot enable `block-llm`
/// (the provider service isn't wasm32-compatible), so this JS has no
/// consumer there — embedding it unconditionally was pure bloat.
#[cfg(feature = "block-llm")]
const LLM_CHAT_JS: &str = include_str!("assets/llm-chat.js");

#[cfg(feature = "block-llm")]
pub fn llm_chat_js() -> &'static str {
    LLM_CHAT_JS
}

/// LLM chat JS URL with content hash, e.g. `/b/static/llm-chat-a1b2c3d4.js`.
/// Not minified — readability matters for a script that's debugged in
/// Chrome devtools.
#[cfg(feature = "block-llm")]
pub fn llm_chat_js_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}llm-chat-{}.js",
            short_hash(llm_chat_js().as_bytes())
        )
    })
}

/// Embedded vanilla-JS bundle for the file-browser surfaces — drag-drop
/// upload, bulk select, kebab menus, share modal, upload modal,
/// confirm-delete. Consumed by `pages_user::object_list_page` and
/// `cloudstorage_page`, both in the `block-files`-gated `blocks::files`
/// module — gated here to match, so a build without the Files block drops
/// this JS too.
#[cfg(feature = "block-files")]
const FILES_BROWSER_JS: &str = include_str!("assets/files-browser.js");

#[cfg(feature = "block-files")]
pub fn files_browser_js() -> &'static str {
    FILES_BROWSER_JS
}

/// Files-browser JS URL with content hash, e.g. `/b/static/files-browser-a1b2c3d4.js`.
#[cfg(feature = "block-files")]
pub fn files_browser_js_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        format!(
            "{STATIC_PREFIX}files-browser-{}.js",
            short_hash(files_browser_js().as_bytes())
        )
    })
}

/// Small inline JS for toast notifications (triggered by htmx HX-Trigger).
pub fn toast_js() -> &'static str {
    r#"
document.body.addEventListener("showToast", function(e) {
    var d = e.detail || {};
    var c = document.getElementById("toast-container");
    if (!c) return;
    var t = document.createElement("div");
    t.className = "toast toast-" + (d.type || "info");
    t.innerHTML = '<span>' + (d.message || '') + '</span><button class="toast-dismiss" onclick="this.parentElement.remove()">&times;</button>';
    c.appendChild(t);
    setTimeout(function() { t.remove(); }, 4000);
});
"#
}

/// Vanilla JS for the command palette — open/close, fuzzy filter,
/// keyboard navigation. Embedded as a string the same way `toast_js()`
/// and `modal_js()` are.
pub fn palette_js() -> &'static str {
    r#"
(function () {
  if (window.__cmdkInit) return;
  window.__cmdkInit = true;
  const el = document.getElementById('cmdk');
  if (!el) return;
  const input = document.getElementById('cmdk-input');
  const list = document.getElementById('cmdk-list');

  const items = () => Array.from(list.querySelectorAll('.palette__item'));
  let selected = 0;

  function open() {
    el.dataset.open = 'true';
    el.setAttribute('aria-hidden', 'false');
    input.value = '';
    apply('');
    requestAnimationFrame(() => input.focus());
  }
  function close() {
    el.dataset.open = 'false';
    el.setAttribute('aria-hidden', 'true');
  }
  function visibleItems() { return items().filter(i => !i.classList.contains('is-hidden')); }

  function apply(query) {
    const q = query.trim().toLowerCase();
    items().forEach(i => {
      const k = (i.dataset.keywords || '').toLowerCase();
      const match = !q || k.includes(q);
      i.classList.toggle('is-hidden', !match);
      i.setAttribute('aria-selected', 'false');
    });
    const vis = visibleItems();
    selected = 0;
    if (vis[0]) vis[0].setAttribute('aria-selected', 'true');
  }

  function move(delta) {
    const vis = visibleItems();
    if (!vis.length) return;
    vis[selected]?.setAttribute('aria-selected', 'false');
    selected = (selected + delta + vis.length) % vis.length;
    vis[selected].setAttribute('aria-selected', 'true');
    vis[selected].scrollIntoView({ block: 'nearest' });
  }

  function activate() {
    const vis = visibleItems();
    const sel = vis[selected];
    if (!sel?.dataset.href) return;
    if (sel.dataset.external === 'true') {
      window.open(sel.dataset.href, '_blank', 'noopener,noreferrer');
    } else {
      window.location.assign(sel.dataset.href);
    }
  }

  // Hotkeys
  document.addEventListener('keydown', (e) => {
    const isMod = e.metaKey || e.ctrlKey;
    if (isMod && e.key.toLowerCase() === 'k') { e.preventDefault(); open(); return; }
    if (el.dataset.open !== 'true') return;
    if (e.key === 'Escape') { e.preventDefault(); close(); }
    else if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
    else if (e.key === 'Enter') { e.preventDefault(); activate(); }
  });

  // Click triggers
  document.addEventListener('click', (e) => {
    const t = e.target.closest('[data-action]');
    if (!t) return;
    if (t.dataset.action === 'palette-open') { e.preventDefault(); open(); }
    if (t.dataset.action === 'palette-close') { e.preventDefault(); close(); }
  });

  // Item click → navigate
  list.addEventListener('click', (e) => {
    const item = e.target.closest('.palette__item');
    if (!item?.dataset.href) return;
    if (item.dataset.external === 'true') {
      window.open(item.dataset.href, '_blank', 'noopener,noreferrer');
    } else {
      window.location.assign(item.dataset.href);
    }
  });

  input.addEventListener('input', (e) => apply(e.target.value));
})();
"#
}

/// Small inline JS for modal close (Escape key + overlay click).
pub fn modal_js() -> &'static str {
    r#"
document.addEventListener("keydown", function(e) {
    if (e.key === "Escape") {
        var m = document.querySelector('.modal-overlay:not([hidden])');
        if (m) m.setAttribute("hidden", "");
    }
});
function openModal(id) {
    var m = document.getElementById(id);
    if (m) m.removeAttribute("hidden");
}
function closeModal(id) {
    var m = document.getElementById(id);
    if (m) m.setAttribute("hidden", "");
}
document.body.addEventListener("closeModal", function(e) {
    var d = e.detail || {};
    if (d.id) closeModal(d.id);
});
"#
}

/// Vanilla JS for the mobile sidebar drawer. Toggles `body[data-drawer-open]`
/// from clicks on `[data-action="drawer-open"]` (the hamburger), the overlay
/// (`[data-action="drawer-close"]`), Escape, or any sidebar nav-link click
/// (so navigation auto-collapses the drawer).
pub fn drawer_js() -> &'static str {
    r#"
(function () {
  if (window.__drawerInit) return;
  window.__drawerInit = true;
  var body = document.body;
  function open() { body.setAttribute('data-drawer-open', 'true'); }
  function close() { body.removeAttribute('data-drawer-open'); }
  document.addEventListener('click', function (e) {
    var t = e.target;
    if (!(t instanceof Element)) return;
    var actEl = t.closest('[data-action]');
    var action = actEl ? actEl.getAttribute('data-action') : null;
    if (action === 'drawer-open') { open(); e.preventDefault(); return; }
    if (action === 'drawer-close') { close(); e.preventDefault(); return; }
    if (body.hasAttribute('data-drawer-open') && t.closest('.sidebar a')) {
      close();
    }
  });
  document.addEventListener('keydown', function (e) {
    if (e.key === 'Escape' && body.hasAttribute('data-drawer-open')) {
      close();
    }
  });
})();
"#
}

#[cfg(test)]
mod tests {
    #[test]
    fn css_bundle_includes_all_layers() {
        let s = super::css_bundle();
        assert!(s.contains("--primary-color"), "tokens layer missing");
        assert!(s.contains("box-sizing"), "base layer missing");
        assert!(
            s.contains(".btn") || s.contains(".button"),
            "components layer missing"
        );
        assert!(s.contains(".shell"), "layout layer missing");
    }

    #[test]
    fn brand_accent_matches_tokens_css() {
        // BRAND_ACCENT_HEX exists for CSS-var-less surfaces (emails). It must
        // stay byte-identical to the stylesheet's --primary-color default.
        assert!(
            super::TOKENS_CSS.contains(&format!("--primary-color: {}", super::BRAND_ACCENT_HEX)),
            "BRAND_ACCENT_HEX ({}) does not match --primary-color in tokens.css",
            super::BRAND_ACCENT_HEX
        );
    }

    #[test]
    fn css_bundle_substitutes_itim_font_urls() {
        let s = super::css_bundle();
        // No raw placeholders left in the served bundle.
        assert!(
            !s.contains("__ITIM_LATIN_URL__"),
            "tokens.css placeholder __ITIM_LATIN_URL__ not substituted"
        );
        assert!(
            !s.contains("__ITIM_LATIN_EXT_URL__"),
            "tokens.css placeholder __ITIM_LATIN_EXT_URL__ not substituted"
        );
        // No reference to the old hardcoded external host.
        assert!(
            !s.contains("impresspress.org/fonts/"),
            "stale impresspress.org font URL still in bundle"
        );
        // The hashed worker-bundled URLs are present.
        assert!(
            s.contains(super::itim_latin_woff2_url()),
            "itim-latin URL missing from bundle"
        );
        assert!(
            s.contains(super::itim_latin_ext_woff2_url()),
            "itim-latin-ext URL missing from bundle"
        );
    }

    #[test]
    fn itim_font_urls_have_content_hash() {
        for url in [
            super::itim_latin_woff2_url(),
            super::itim_latin_ext_woff2_url(),
        ] {
            assert!(url.starts_with("/b/static/itim-latin"));
            assert!(url.ends_with(".woff2"));
        }
    }

    #[test]
    fn favicon_url_has_content_hash() {
        let url = super::favicon_url();
        assert!(url.starts_with("/b/static/favicon-"));
        assert!(url.ends_with(".ico"));
        let hash = url
            .trim_start_matches("/b/static/favicon-")
            .trim_end_matches(".ico");
        assert_eq!(hash.len(), 8, "expected 8-char short hash, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn tokens_include_new_scale() {
        let s = super::css_bundle();
        for tok in [
            "--text-base",
            "--text-2xl",
            "--space-2xl",
            "--surface-1",
            "--primary-button",
            "--focus-ring",
        ] {
            assert!(s.contains(tok), "missing token: {tok}");
        }
    }

    #[test]
    fn palette_js_present_and_self_invoking() {
        let js = super::palette_js();
        assert!(js.contains("cmdk"));
        assert!(js.contains("Meta+K") || js.contains("metaKey"));
        assert!(js.starts_with("\n(function") || js.contains("(function "));
    }

    #[test]
    fn drawer_js_handles_open_close_esc_and_navlink() {
        let js = super::drawer_js();
        assert!(js.contains("'drawer-open'"));
        assert!(js.contains("'drawer-close'"));
        assert!(js.contains("'Escape'"));
        assert!(js.contains(".sidebar a"));
        assert!(js.contains("data-drawer-open"));
        // Self-invoking + idempotent guard.
        assert!(js.contains("__drawerInit"));
    }

    #[test]
    #[cfg(feature = "block-llm")]
    fn llm_chat_js_is_self_invoking_and_exposes_init() {
        let js = super::llm_chat_js();
        assert!(js.contains("(function ()") || js.contains("(function()"));
        assert!(js.contains("__impresspressLlmChatLoaded"));
        assert!(js.contains("window.impresspressLlmChat = { init: init }"));
        for sym in [
            "handleChatSubmit",
            "createNewThread",
            "selectThread",
            "onModelChange",
            "unloadLocalModel",
        ] {
            assert!(
                js.contains(&format!("window.{sym} = {sym}")),
                "missing global re-export for {sym}"
            );
        }
    }

    #[test]
    #[cfg(feature = "block-llm")]
    fn purify_js_url_has_content_hash() {
        let url = super::purify_js_url();
        assert!(url.starts_with("/b/static/purify-"));
        assert!(url.ends_with(".js"));
        let hash = url
            .trim_start_matches("/b/static/purify-")
            .trim_end_matches(".js");
        assert_eq!(hash.len(), 8, "expected 8-char short hash, got: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    #[cfg(feature = "block-llm")]
    fn purify_js_is_dompurify_umd_build() {
        let js = super::purify_js();
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
    #[cfg(feature = "block-files")]
    fn files_browser_js_exposes_init_and_handles_drag_drop() {
        let js = super::files_browser_js();
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
    #[cfg(feature = "block-files")]
    fn files_browser_js_url_has_content_hash() {
        let url = super::files_browser_js_url();
        assert!(url.starts_with("/b/static/files-browser-"));
        assert!(url.ends_with(".js"));
        let hash = url
            .trim_start_matches("/b/static/files-browser-")
            .trim_end_matches(".js");
        assert_eq!(hash.len(), 8);
    }

    #[test]
    #[cfg(feature = "block-llm")]
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
}
