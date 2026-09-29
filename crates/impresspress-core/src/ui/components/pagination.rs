//! Pagination

/// Previous/next links and "page / pages" for a list of `total` rows shown
/// `per_page` at a time. `per_page` is the page size the caller asked the
/// database for, which is never zero.
pub fn pagination(
    page: u32,
    per_page: std::num::NonZeroU32,
    total: u32,
    base_href: &str,
) -> maud::Markup {
    use maud::{html, PreEscaped};
    let total_pages = if total == 0 {
        1
    } else {
        total.div_ceil(per_page.get())
    };
    let prev_disabled = page <= 1;
    let next_disabled = page >= total_pages;
    let join = if base_href.contains('?') { '&' } else { '?' };
    let prev_href = format!(
        "{}{}page={}",
        base_href,
        join,
        page.saturating_sub(1).max(1)
    );
    let next_href = format!("{}{}page={}", base_href, join, (page + 1).min(total_pages));
    html! {
        nav .pagination aria-label="Pagination" {
            span .pagination__count { (format!("{} total", total)) }
            a .pagination__prev .(if prev_disabled { "is-disabled" } else { "" })
                href=(PreEscaped(&prev_href)) aria-disabled=(prev_disabled.to_string()) { "‹ Prev" }
            span .pagination__page { (format!("{} / {}", page, total_pages)) }
            a .pagination__next .(if next_disabled { "is-disabled" } else { "" })
                href=(PreEscaped(&next_href)) aria-disabled=(next_disabled.to_string()) { "Next ›" }
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
    fn pagination_clamps_prev_at_page_1() {
        let s = pagination(1, per_page(25), 100, "/users").into_string();
        assert!(s.contains(r#"aria-disabled="true""#)); // prev disabled at page 1
        assert!(s.contains("100 total"));
        assert!(s.contains("1 / 4"));
    }

    #[test]
    fn pagination_appends_query_correctly() {
        let s = pagination(2, per_page(10), 30, "/users?role=admin").into_string();
        assert!(s.contains("/users?role=admin&page=1"));
        assert!(s.contains("/users?role=admin&page=3"));
    }
}
