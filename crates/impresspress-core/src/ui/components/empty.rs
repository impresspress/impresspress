//! Empty State

/// The "nothing here yet" block for a list or section with no rows: an icon,
/// a title, one sentence and an optional call to action.
///
/// The title is an `h2`. An empty state stands in for a page's (or a
/// section's) content directly under the topbar's `h1`, and an `h3` there
/// skipped a level (axe `heading-order`); inside a `section_header` section
/// it is a sibling `h2`, which is still a valid outline.
pub fn empty_state(
    icon: maud::Markup,
    title: &str,
    body: &str,
    action: Option<maud::Markup>,
) -> maud::Markup {
    use maud::html;
    html! {
        div .empty {
            div .empty__icon aria-hidden="true" { (icon) }
            h2 .empty__title { (title) }
            p .empty__body { (body) }
            @if let Some(a) = action { div .empty__action { (a) } }
        }
    }
}

#[cfg(test)]
mod tests {
    use maud::html;

    use super::*;

    #[test]
    fn title_is_an_h2_and_the_icon_is_decorative() {
        let s = empty_state(html! { svg {} }, "No buckets", "Create one.", None).into_string();
        assert!(s.contains(r#"<h2 class="empty__title">No buckets</h2>"#));
        assert!(s.contains(r#"<div class="empty__icon" aria-hidden="true">"#));
    }
}
