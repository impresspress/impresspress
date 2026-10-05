//! Pagination

/// Previous/next links and "page / pages" for a list of `total` rows shown
/// `per_page` at a time. `per_page` is the page size the caller asked the
/// database for, which is never zero.
///
/// One row: the total on the left, the controls on the right, at every
/// width. Renders nothing when everything fits on one page — a "1 / 1" bar
/// with two dead links is noise. At an end of the range the dead control is
/// a `span`, not a link: there is nowhere for it to go. Each control is a
/// 44px target.
pub fn pagination(
    page: u32,
    per_page: std::num::NonZeroU32,
    total: u32,
    base_href: &str,
) -> maud::Markup {
    use maud::html;
    let total_pages = total.div_ceil(per_page.get()).max(1);
    if total_pages <= 1 {
        return html! {};
    }
    let page = page.clamp(1, total_pages);
    let join = if base_href.contains('?') { '&' } else { '?' };
    let href = |p: u32| format!("{base_href}{join}page={p}");
    let control = |label: &str, aria: &str, class: &str, target: Option<u32>| {
        html! {
            @match target {
                Some(p) => a .pagination__link .(class) href=(href(p)) aria-label=(aria) { (label) },
                None => span .pagination__link .(class) .is-disabled aria-disabled="true" { (label) },
            }
        }
    };
    html! {
        nav .pagination aria-label="Pagination" {
            span .pagination__count { (total) " total" }
            div .pagination__controls {
                (control("‹ Prev", "Previous page", "pagination__prev", (page > 1).then(|| page - 1)))
                span .pagination__page aria-current="page" { (page) " / " (total_pages) }
                (control("Next ›", "Next page", "pagination__next", (page < total_pages).then(|| page + 1)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn per_page(n: u32) -> std::num::NonZeroU32 {
        std::num::NonZeroU32::new(n).expect("a page size")
    }

    #[test]
    fn first_page_has_no_previous_link() {
        let s = pagination(1, per_page(25), 100, "/users").into_string();
        assert!(s.contains(
            r#"<span class="pagination__link pagination__prev is-disabled" aria-disabled="true">"#
        ));
        assert!(s.contains(r#"href="/users?page=2""#));
        assert!(s.contains("100 total"));
        assert!(s.contains("1 / 4"));
    }

    #[test]
    fn last_page_has_no_next_link() {
        let s = pagination(4, per_page(25), 100, "/users").into_string();
        assert!(s.contains(r#"href="/users?page=3""#));
        assert!(s.contains(
            r#"<span class="pagination__link pagination__next is-disabled" aria-disabled="true">"#
        ));
    }

    #[test]
    fn pagination_appends_query_correctly() {
        let s = pagination(2, per_page(10), 30, "/users?role=admin").into_string();
        assert!(s.contains("/users?role=admin&amp;page=1"));
        assert!(s.contains("/users?role=admin&amp;page=3"));
    }

    #[test]
    fn a_single_page_renders_nothing() {
        assert_eq!(pagination(1, per_page(20), 0, "/users").into_string(), "");
        assert_eq!(pagination(1, per_page(20), 20, "/users").into_string(), "");
        assert!(!pagination(1, per_page(20), 21, "/users")
            .into_string()
            .is_empty());
    }
}
