//! Tab navigation and the canonical button (Phase 1).

use maud::{html, Markup};

use super::avatar::CtrlSize;

// ---------------------------------------------------------------------------
// Tab Navigation
// ---------------------------------------------------------------------------

/// One tab in a [`tab_navigation`] bar.
///
/// `icon` is pre-rendered [`Markup`] (e.g. `icons::users()`) so each call site
/// references the icon function directly — no name-string lookup, no silent
/// fallback. `href` is borrowed; the same URL feeds both the `href` and the
/// `hx-get` so the htmx swap and a no-JS click navigate identically.
pub struct Tab<'a> {
    /// Whether this tab is the active one (renders the `active` class).
    pub active: bool,
    /// Destination URL — used for both `href` and `hx-get`.
    pub href: &'a str,
    /// Visible label.
    pub label: &'a str,
    /// Optional leading icon markup.
    pub icon: Option<Markup>,
}

/// Render an htmx tab bar: each tab swaps `#content` and pushes its URL.
///
/// This is the single place the admin pages' tab strips are defined, so the
/// `hx-target` / `hx-push-url` behavior lives in one spot.
pub fn tab_navigation(tabs: Vec<Tab<'_>>) -> Markup {
    html! {
        div .tabs {
            @for tab in tabs {
                a .tab
                    .(if tab.active { "active" } else { "" })
                    href=(tab.href)
                    hx-get=(tab.href)
                    hx-target="#content"
                    hx-push-url="true"
                {
                    @if let Some(icon) = tab.icon {
                        (icon) " "
                    }
                    (tab.label)
                }
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
/// the shell above the content card through
/// [`crate::ui::PageBody::with_subnav`]; `label` names the landmark ("Tickets
/// sections").
pub fn subnav(label: &str, tabs: Vec<Tab<'_>>) -> Markup {
    html! {
        nav .subnav aria-label=(label) {
            div .tabs {
                @for tab in tabs {
                    a .tab .(if tab.active { "active" } else { "" })
                        href=(tab.href)
                        aria-current=[tab.active.then_some("page")]
                    {
                        @if let Some(icon) = tab.icon {
                            (icon) " "
                        }
                        (tab.label)
                    }
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
                        @if let Some(icon) = link.icon {
                            (icon)
                        }
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

// ---------------------------------------------------------------------------
// Canonical button (Phase 1)
// ---------------------------------------------------------------------------

/// Visual variant for buttons.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum BtnVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

impl BtnVariant {
    fn class(self) -> &'static str {
        match self {
            BtnVariant::Primary => "btn btn--primary",
            BtnVariant::Secondary => "btn btn--secondary",
            BtnVariant::Ghost => "btn btn--ghost",
            BtnVariant::Danger => "btn btn--danger",
        }
    }
}

impl CtrlSize {
    fn class(self) -> &'static str {
        match self {
            CtrlSize::Sm => "btn--sm",
            CtrlSize::Md => "btn--md",
            CtrlSize::Lg => "btn--lg",
        }
    }
}

/// Canonical button. Use for every button on every new page.
///
/// `extra_attrs` is a maud `PreEscaped` block of additional attributes
/// (e.g. `hx-post=...`, `type="submit"`, `disabled`). Pass
/// `maud::PreEscaped(String::new())` if none.
pub fn button(
    variant: BtnVariant,
    size: CtrlSize,
    label: &str,
    extra_attrs: maud::PreEscaped<String>,
) -> maud::Markup {
    use maud::PreEscaped;
    let class = format!("{} {}", variant.class(), size.class());
    let extra = extra_attrs.0;
    let label_escaped = maud::html! { (label) }.into_string();
    PreEscaped(format!(
        r#"<button class="{class}" {extra}>{label_escaped}</button>"#,
    ))
}

#[cfg(test)]
mod tests {
    use maud::PreEscaped;

    use super::*;

    #[test]
    fn button_primary_md() {
        let m = button(
            BtnVariant::Primary,
            CtrlSize::Md,
            "Save",
            PreEscaped(String::new()),
        );
        let s = m.into_string();
        assert!(s.contains("btn--primary"), "missing variant class: {s}");
        assert!(s.contains("btn--md"), "missing size class: {s}");
        assert!(s.contains(">Save</button>"), "missing label: {s}");
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
                    icon: None,
                },
                Tab {
                    active: false,
                    href: "/b/products/admin/manage?view=deleted",
                    label: "Deleted",
                    icon: None,
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
                icon: None,
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

    #[test]
    fn button_extra_attrs_pass_through() {
        let m = button(
            BtnVariant::Danger,
            CtrlSize::Sm,
            "Delete",
            PreEscaped(r#"hx-delete="/users/1" type="button""#.to_string()),
        );
        let s = m.into_string();
        assert!(
            s.contains(r#"hx-delete="/users/1""#),
            "extra attrs missing: {s}"
        );
        assert!(s.contains("btn--danger"), "variant missing: {s}");
    }
}
