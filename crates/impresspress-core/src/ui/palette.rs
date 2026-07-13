//! Command palette — ⌘K / Ctrl+K modal that searches routes + actions.
//!
//! In Phase 1 the markup is mounted on every shelled page, but the
//! action list is sourced from `routing::routes_config()` only.
//! Phase 2 adds named verb actions ("Invite user", etc.).

use maud::{html, Markup};

/// One palette entry. Either a route (path + label) or an action verb.
pub struct PaletteEntry {
    pub label: String,
    pub kind_label: String, // "Page", "Action", etc.
    pub href: String,       // for route entries
    pub keywords: String,   // space-separated, for fuzzy match
    pub external: bool,     // open in a new tab (e.g. Inspector)
}

/// Render the palette markup. Hidden by default; CSS class controls
/// visibility, JS controls focus + filter + selection.
pub fn palette(entries: Vec<PaletteEntry>) -> Markup {
    html! {
        div #cmdk .palette aria-hidden="true" role="dialog" aria-modal="true" aria-label="Command palette" {
            div .palette__backdrop data-action="palette-close" {}
            div .palette__panel {
                input #cmdk-input .palette__input type="text"
                    placeholder="Type to search…"
                    autocomplete="off"
                    aria-controls="cmdk-list" {}
                ul #cmdk-list .palette__list role="listbox" {
                    @for (i, e) in entries.iter().enumerate() {
                        li .palette__item role="option"
                           data-href=(e.href)
                           data-external=[e.external.then_some("true")]
                           data-keywords=(e.keywords)
                           aria-selected=[(i == 0).then_some("true")] {
                            span .palette__item-label { (e.label) }
                            span .palette__item-kind { (e.kind_label) }
                        }
                    }
                }
                div .palette__hint { "↑↓ navigate · ↵ open · Esc close" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, href: &str) -> PaletteEntry {
        PaletteEntry {
            label: label.to_string(),
            kind_label: "Page".to_string(),
            href: href.to_string(),
            keywords: format!("{} {}", label.to_lowercase(), href),
            external: false,
        }
    }

    #[test]
    fn palette_renders_entries_with_keywords() {
        let entries = vec![
            entry("Users", "/b/admin/users"),
            entry("Logs", "/b/admin/logs"),
        ];
        let s = palette(entries).into_string();
        assert!(s.contains(r#"id="cmdk""#));
        assert!(s.contains(r#"data-href="/b/admin/users""#));
        assert!(s.contains(r#"data-keywords="users /b/admin/users""#));
        assert!(s.contains(r#"aria-selected="true""#)); // first entry
        assert!(s.contains(">Users<"));
        assert!(s.contains(">Logs<"));
    }

    #[test]
    fn palette_with_no_entries_still_renders_dialog() {
        let s = palette(Vec::new()).into_string();
        assert!(s.contains(r#"role="dialog""#));
        assert!(s.contains("cmdk-list"));
    }
}
