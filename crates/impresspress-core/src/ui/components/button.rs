//! Tab navigation and list filters. Buttons are [`super::Action`].

use maud::{html, Markup};

// ---------------------------------------------------------------------------
// Tab Navigation
// ---------------------------------------------------------------------------

/// One tab in a [`tab_navigation`] bar, a [`subnav`] or a [`FilterLinks`]
/// set. A tab is its text label alone, in sentence case: tabs and filter
/// links carry no icon.
pub struct Tab<'a> {
    /// Whether this tab is the current one: `.active` on a tab, plus
    /// `aria-current` in a [`subnav`]; `aria-current` on a filter link.
    pub active: bool,
    /// Destination URL. [`tab_navigation`] also swaps it in through `hx-get`,
    /// so the htmx swap and a no-JS click navigate identically.
    pub href: &'a str,
    /// Visible label, in sentence case ("API keys", "Audit logs").
    pub label: &'a str,
}

/// Render an htmx tab bar: each tab swaps `#content` and pushes its URL.
///
/// This is the single place the admin pages' tab strips are defined, so the
/// `hx-target` / `hx-push-url` behavior lives in one spot.
pub fn tab_navigation(tabs: Vec<Tab<'_>>) -> Markup {
    html! {
        div .tabs {
            @for tab in tabs {
                a .tab .active[tab.active]
                    href=(tab.href)
                    hx-get=(tab.href)
                    hx-target="#content"
                    hx-push-url="true"
                { (tab.label) }
            }
        }
    }
}

/// A list filter that is either on or off ("Server errors", "Errors only"):
/// a real `<button>` whose `aria-pressed` carries the state, and which swaps
/// `#content` for `href` — the same list with this filter flipped — and
/// pushes that URL, so the filter survives a reload and the back button.
///
/// `href` is the URL the page is at after the press, so the caller builds it
/// with this filter's value inverted and every other filter kept.
pub fn filter_toggle(label: &str, pressed: bool, href: &str) -> Markup {
    html! {
        button .btn .btn--secondary .filter-toggle
            type="button"
            aria-pressed=(if pressed { "true" } else { "false" })
            hx-get=(href)
            hx-target="#content"
            hx-push-url="true"
        {
            span .filter-toggle__check aria-hidden="true" { (crate::ui::icons::check()) }
            (label)
        }
    }
}

/// A block's own sections — separate pages of one block (Tickets' Inbox /
/// Types / Settings / Endpoints) — as the same `.tabs` strip
/// [`tab_navigation`] draws, but as plain links in a labelled `nav`, the
/// current one `aria-current="page"`.
///
/// Not [`tab_navigation`]'s htmx swap: that replaces `#content` only, which
/// is right for views of ONE page (they share its header) and wrong between
/// pages, whose title, actions and scripts differ — the previous page's
/// header and `<title>` would stay on screen over the next page's body. Rendered by
/// the shell between the page header and the body through
/// [`crate::ui::PageBody::with_subnav`]; `label` names the landmark ("Tickets
/// sections").
pub fn subnav(label: &str, tabs: Vec<Tab<'_>>) -> Markup {
    html! {
        nav .subnav aria-label=(label) {
            div .tabs {
                @for tab in tabs {
                    a .tab .active[tab.active]
                        href=(tab.href)
                        aria-current=[tab.active.then_some("page")]
                    { (tab.label) }
                }
            }
        }
    }
}

/// One list's views of which exactly one is current — Active / Deleted
/// products, an order status, a sort order — as a set of links in the
/// [`filter_toggle`] look (`.filter-toggle`, the current one filled and
/// checked), in a labelled `nav`, the current link `aria-current="true"`.
/// The one way a page draws such a set; an on/off filter is
/// [`filter_toggle`].
///
/// Links, not [`filter_toggle`]'s on/off buttons: only one can be on. Not
/// [`subnav`]: those are a block's separate pages, drawn above the content
/// card; these narrow the list the page already shows, so they sit in its
/// filter row.
///
/// Plain navigations by default, for the reason [`subnav`] gives: a view
/// that changes the page's subtitle or actions (they live in the topbar)
/// must load the whole page. A view that changes only the list
/// ([`FilterLinks::swap`]) swaps the target and pushes the URL instead.
pub struct FilterLinks<'a> {
    label: &'a str,
    label_id: Option<&'a str>,
    swap_target: Option<&'a str>,
    links: Vec<Tab<'a>>,
}

impl<'a> FilterLinks<'a> {
    /// `links` named `label` (read by screen readers only, unless
    /// [`visible_label`](Self::visible_label) shows it).
    pub fn new(label: &'a str, links: Vec<Tab<'a>>) -> Self {
        Self {
            label,
            label_id: None,
            swap_target: None,
            links,
        }
    }

    /// Show the label in front of the links ("Sort by"), as the element
    /// `id` the `nav` is labelled by.
    pub fn visible_label(self, id: &'a str) -> Self {
        Self {
            label_id: Some(id),
            ..self
        }
    }

    /// Swap `target` (`"#content"`) with each link's page and push its URL,
    /// rather than navigating — for views that change only the list.
    pub fn swap(self, target: &'a str) -> Self {
        Self {
            swap_target: Some(target),
            ..self
        }
    }

    pub fn render(self) -> Markup {
        let label = self.label_id.is_none().then_some(self.label);
        html! {
            nav .filter-toggles aria-label=[label] aria-labelledby=[self.label_id] {
                @if let Some(id) = self.label_id {
                    span #(id) .text-sm .text-muted { (self.label) }
                }
                @for link in self.links {
                    a .btn .btn--secondary .filter-toggle
                        href=(link.href)
                        hx-get=[self.swap_target.map(|_| link.href)]
                        hx-target=[self.swap_target]
                        hx-push-url=[self.swap_target.map(|_| "true")]
                        aria-current=[link.active.then_some("true")]
                    {
                        span .filter-toggle__check aria-hidden="true" { (crate::ui::icons::check()) }
                        (link.label)
                    }
                }
            }
        }
    }
}

/// [`FilterLinks`] with neither a visible label nor a swap: plain links.
pub fn filter_links(label: &str, links: Vec<Tab<'_>>) -> Markup {
    FilterLinks::new(label, links).render()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_tabs() -> Vec<Tab<'static>> {
        vec![
            Tab {
                active: true,
                href: "/b/admin/users",
                label: "Users",
            },
            Tab {
                active: false,
                href: "/b/admin/users?tab=api-keys",
                label: "API keys",
            },
        ]
    }

    /// An htmx tab bar: text-only tabs, the current one `.active`, each
    /// swapping `#content` for its URL and pushing it.
    #[test]
    fn tab_navigation_draws_text_tabs_that_swap_the_content() {
        assert_eq!(
            tab_navigation(two_tabs()).into_string(),
            concat!(
                r##"<div class="tabs">"##,
                r##"<a class="tab active" href="/b/admin/users" hx-get="/b/admin/users" hx-target="#content" hx-push-url="true">Users</a>"##,
                r##"<a class="tab" href="/b/admin/users?tab=api-keys" hx-get="/b/admin/users?tab=api-keys" hx-target="#content" hx-push-url="true">API keys</a>"##,
                r##"</div>"##,
            )
        );
    }

    /// Section links: the same text-only tabs as plain links in a labelled
    /// `nav`, the current one `aria-current="page"`.
    #[test]
    fn subnav_draws_text_tabs_as_plain_links() {
        assert_eq!(
            subnav("Admin sections", two_tabs()).into_string(),
            concat!(
                r#"<nav class="subnav" aria-label="Admin sections"><div class="tabs">"#,
                r#"<a class="tab active" href="/b/admin/users" aria-current="page">Users</a>"#,
                r#"<a class="tab" href="/b/admin/users?tab=api-keys">API keys</a>"#,
                r#"</div></nav>"#,
            )
        );
    }

    #[test]
    fn filter_links_are_labelled_links_with_the_current_one_marked() {
        let s = filter_links(
            "Product views",
            vec![
                Tab {
                    active: true,
                    href: "/b/products/admin/manage",
                    label: "Active",
                },
                Tab {
                    active: false,
                    href: "/b/products/admin/manage?view=deleted",
                    label: "Deleted",
                },
            ],
        )
        .into_string();
        assert!(
            s.starts_with(r#"<nav class="filter-toggles" aria-label="Product views"><a class="btn btn--secondary filter-toggle" href="/b/products/admin/manage" aria-current="true"><span class="filter-toggle__check" aria-hidden="true">"#),
            "{s}"
        );
        assert!(
            s.contains(r#"<a class="btn btn--secondary filter-toggle" href="/b/products/admin/manage?view=deleted"><span"#),
            "{s}"
        );
        // Plain links: a view changes the topbar, so it is never a swap.
        assert!(!s.contains("hx-"), "{s}");
    }

    #[test]
    fn swapping_filter_links_carry_a_visible_label_and_the_htmx_swap() {
        let s = FilterLinks::new(
            "Sort by",
            vec![Tab {
                active: true,
                href: "/b/admin/settings/network?sort=errors",
                label: "Errors",
            }],
        )
        .visible_label("network-sort-label")
        .swap("#content")
        .render()
        .into_string();
        assert!(
            s.starts_with(r##"<nav class="filter-toggles" aria-labelledby="network-sort-label"><span class="text-sm text-muted" id="network-sort-label">Sort by</span><a class="btn btn--secondary filter-toggle" href="/b/admin/settings/network?sort=errors" hx-get="/b/admin/settings/network?sort=errors" hx-target="#content" hx-push-url="true" aria-current="true">"##),
            "{s}"
        );
    }
}
