//! Page layout components — the full HTML page wrapper.
//!
//! `block_shell()` was removed in Phase 2 of the UI cleanup; pages now build
//! chrome via `ui::Page::response()` which delegates to `ui::shell::shell()`
//! + `ui::sidebar::sidebar_grouped()`.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use super::{assets, SiteConfig};

/// Render a full HTML page with head (CSS + htmx) and body.
pub fn page(title: &str, config: &SiteConfig, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { (title) " — " (config.app_name) }
                link rel="stylesheet" href=(assets::css_url());
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
