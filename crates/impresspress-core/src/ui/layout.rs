//! Page layout components — the full HTML page wrapper.
//!
//! `block_shell()` was removed in Phase 2 of the UI cleanup; pages now build
//! chrome via `ui::Page::response()` which delegates to `ui::shell::shell()`
//! + `ui::sidebar::sidebar_grouped()`.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use super::{assets, SiteConfig};

/// The `htmx-config` every page carries. See the comment where it is emitted.
const HTMX_CONFIG: &str = r#"{"allowEval":false}"#;

/// Render a full HTML page with head (CSS + htmx) and body.
pub fn page(title: &str, config: &SiteConfig, body: Markup) -> Markup {
    // Brand accent override. Sanitized to a safe CSS-color charset so a
    // stored value can't break out of the <style> tag. `--primary-hover`
    // derives from it so a single config var re-themes the whole chrome.
    let primary_override = if config.primary_color.trim().is_empty() {
        String::new()
    } else {
        let c: String = config
            .primary_color
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || "#(),%. -".contains(*ch))
            .collect();
        format!(":root{{--primary-color:{c};--primary-hover:color-mix(in srgb,{c} 82%,#000)}}")
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { (title) " — " (config.app_name) }
                link rel="stylesheet" href=(assets::css_url());
                @if !primary_override.is_empty() {
                    style { (PreEscaped(&primary_override)) }
                }
                @if !config.favicon_url.is_empty() {
                    link rel="icon" href=(config.favicon_url);
                }
                // htmx's own switch for every feature that compiles
                // attribute text with `new Function`: `hx-on`, a `js:` value,
                // a trigger filter. The served content-security policy has no
                // `'unsafe-eval'`, so each of those would be refused by the
                // browser anyway; with this off htmx refuses them first, with
                // an `htmx:evalDisallowedError`, and never reaches the eval.
                // Read at htmx's init, which waits for the document to be
                // ready, so this tag only has to be in the head.
                meta name="htmx-config" content=(HTMX_CONFIG);
                script src=(assets::htmx_js_url()) defer {}
                // The chrome's own behaviour — palette, drawer, toasts,
                // modals — as one hashed asset instead of four raw strings
                // inlined at the bottom of every page. `defer` is what keeps
                // the bindings sound: a deferred script runs after parsing
                // and in document order, so every element these sections bind
                // to exists by the time they run — their end-of-body
                // placement gave them the same guarantee.
                //
                // What did change is the order relative to htmx. The four
                // inline tags were synchronous, so they ran during parse,
                // ahead of every deferred script including htmx; the file
                // below runs after it. That is inert: htmx defers its own
                // document processing to the ready event, which fires after
                // all deferred scripts, so the body listeners here are still
                // installed before anything can dispatch to them.
                script src=(assets::chrome_js_url()) defer {}
            }
            body {
                (body)
                div #toast-container .toast-container {}
                script src=(assets::webmcp_js_url()) defer {}
                @for src in &config.embedded_scripts {
                    script type="module" src=(src) {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_includes_the_webmcp_registration_script() {
        let config = SiteConfig {
            app_name: "Test".into(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: String::new(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
            auth_headline: String::new(),
            auth_tagline: String::new(),
        };
        let rendered = page("Title", &config, maud::html! { p { "body" } }).into_string();
        assert!(
            rendered.contains(&assets::webmcp_js_url()),
            "the WebMCP script must be on every page: {rendered}"
        );
    }

    /// Every page turns htmx's eval off, and does so before htmx loads.
    #[test]
    fn every_page_disables_htmx_eval() {
        let config = SiteConfig {
            app_name: "Test".into(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: String::new(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
            auth_headline: String::new(),
            auth_tagline: String::new(),
        };
        let rendered = page("Title", &config, maud::html! { p { "body" } }).into_string();
        let meta = r#"<meta name="htmx-config" content="{&quot;allowEval&quot;:false}">"#;
        let at = rendered
            .find(meta)
            .unwrap_or_else(|| panic!("no allowEval=false htmx-config: {rendered}"));
        let htmx = rendered
            .find(&assets::htmx_js_url())
            .expect("htmx is loaded");
        assert!(at < htmx, "the config must precede htmx: {rendered}");
    }

    /// The chrome's behaviour ships as one hashed `<script src>`, not as
    /// inlined raw strings. Markers from all four former inline scripts must
    /// be absent from the document, and the asset URL present.
    #[test]
    fn chrome_behaviour_is_one_hashed_script_not_inline_source() {
        let config = SiteConfig {
            app_name: "Test".into(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: String::new(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
            auth_headline: String::new(),
            auth_tagline: String::new(),
        };
        let rendered = page("Title", &config, maud::html! { p { "body" } }).into_string();
        assert!(
            rendered.contains(&format!(
                r#"<script src="{}" defer></script>"#,
                assets::chrome_js_url()
            )),
            "the chrome script must be linked, hashed and deferred: {rendered}"
        );
        for marker in ["__cmdkInit", "__drawerInit", "showToast", "__modalInit"] {
            assert!(
                !rendered.contains(marker),
                "{marker} is still inlined into the page: {rendered}"
            );
        }
    }
}
