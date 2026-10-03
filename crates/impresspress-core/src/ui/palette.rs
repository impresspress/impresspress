//! Command palette — ⌘K / Ctrl+K modal that searches routes + actions.
//!
//! In Phase 1 the markup is mounted on every shelled page, but the
//! action list is sourced from `routing::routes_config()` only.
//! Phase 2 adds named verb actions ("Invite user", etc.).

use maud::{html, Markup};

/// One palette entry: a page reachable from the sidebar.
pub struct PaletteEntry {
    pub label: String,
    pub href: String,
    pub keywords: String, // space-separated, for fuzzy match
    pub external: bool,   // open in a new tab (e.g. Inspector)
}

/// The `id` of entry `i`'s option, which the input names in
/// `aria-activedescendant` while that entry is selected.
fn option_id(i: usize) -> String {
    format!("cmdk-opt-{i}")
}

/// Render the palette markup. Hidden by default; CSS class controls
/// visibility, JS controls focus + filter + selection.
///
/// ARIA combobox pattern: focus stays in the input (`role="combobox"`), which
/// owns the listbox through `aria-controls` and points at the selected option
/// with `aria-activedescendant` — chrome.js keeps that attribute in step with
/// the arrow keys and the filter, so a screen reader announces each option as
/// it is selected without focus ever leaving the text field.
pub fn palette(entries: Vec<PaletteEntry>) -> Markup {
    let first = (!entries.is_empty()).then(|| option_id(0));
    html! {
        div #cmdk .palette aria-hidden="true" role="dialog" aria-modal="true" aria-label="Command palette" {
            div .palette__backdrop data-action="palette-close" {}
            div .palette__panel {
                input #cmdk-input .palette__input type="text"
                    role="combobox"
                    placeholder="Type to search…"
                    // Placeholder-only leaves the field with no accessible
                    // name once it has a value; this is the palette's only
                    // control, so it needs one of its own.
                    aria-label="Search pages"
                    aria-autocomplete="list"
                    aria-expanded="true"
                    aria-controls="cmdk-list"
                    aria-activedescendant=[first]
                    autocomplete="off"
                    spellcheck="false" {}
                ul #cmdk-list .palette__list role="listbox" aria-label="Pages" {
                    @for (i, e) in entries.iter().enumerate() {
                        li .palette__item role="option" id=(option_id(i))
                           data-href=(e.href)
                           data-external=[e.external.then_some("true")]
                           data-keywords=(e.keywords)
                           aria-selected=(if i == 0 { "true" } else { "false" }) {
                            (e.label)
                        }
                    }
                }
                div .palette__hint aria-hidden="true" { "↑↓ navigate · ↵ open · Esc close" }
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
                                                        // Combobox wiring: the input names the listbox and the selected option.
        assert!(s.contains(r#"role="combobox""#));
        assert!(s.contains(r#"aria-activedescendant="cmdk-opt-0""#));
        assert!(s.contains(r#"<li class="palette__item" role="option" id="cmdk-opt-1""#));
        assert!(s.contains(r#"role="listbox" aria-label="Pages""#));
        // No per-entry kind tag: every entry is a page.
        assert!(!s.contains("palette__item-kind"));
        assert!(s.contains(">Users<"));
        assert!(s.contains(">Logs<"));
    }

    #[test]
    fn palette_with_no_entries_still_renders_dialog() {
        let s = palette(Vec::new()).into_string();
        assert!(s.contains(r#"role="dialog""#));
        assert!(s.contains("cmdk-list"));
        // Nothing to point at: no dangling activedescendant.
        assert!(!s.contains("aria-activedescendant"));
    }
}
