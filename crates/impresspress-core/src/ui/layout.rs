//! Page layout components — the full HTML page wrapper.
//!
//! `block_shell()` was removed in Phase 2 of the UI cleanup; pages now build
//! chrome via `ui::Page::response()` which delegates to `ui::shell::shell()`
//! + `ui::sidebar::sidebar_grouped()`.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use super::{assets, SiteConfig};

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
                script src=(assets::htmx_js_url()) defer {}
            }
            body {
                (body)
                div #toast-container .toast-container {}
                script { (PreEscaped(assets::toast_js())) }
                script { (PreEscaped(assets::modal_js())) }
                @for src in &config.embedded_scripts {
                    script type="module" src=(src) {}
                }
            }
        }
    }
}
