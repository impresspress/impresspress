//! Section Header

use maud::{html, Markup};

/// The heading of one section of a page's body: an `h2` title with an
/// optional trailing slot (a button, a count, a status) on the same row.
///
/// This is the ONLY in-body heading a page renders. The page's own title,
/// description and page-level actions belong in the shell topbar
/// (`ui::shell::Topbar`), which owns the page's single `h1`; a section under
/// it is therefore always an `h2`, whatever the section contains.
pub fn section_header(title: &str, action: Option<Markup>) -> Markup {
    html! {
        div .section-header {
            h2 .section-header__title { (title) }
            @if let Some(action) = action {
                div .section-header__action { (action) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_an_h2_title_and_the_action_slot() {
        let s = section_header("Roles", Some(html! { button { "Create role" } })).into_string();
        assert!(s.contains(r#"<h2 class="section-header__title">Roles</h2>"#));
        assert!(
            s.contains(r#"<div class="section-header__action"><button>Create role</button></div>"#)
        );
    }

    #[test]
    fn omits_the_action_slot_when_there_is_none() {
        let s = section_header("Schema", None).into_string();
        assert!(s.contains("<h2"));
        assert!(!s.contains("section-header__action"));
    }
}
