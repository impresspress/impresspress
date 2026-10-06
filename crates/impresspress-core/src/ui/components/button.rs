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

/// One page's own views of the same list — Active / Deleted products, an
/// order status — as a row of chips: plain links in a labelled `nav`, the
/// current one `aria-current="page"`.
///
/// Not [`subnav`]: those are a block's separate pages, drawn above the
/// content card; these narrow the list the page already shows, so they sit
/// in the body beside its search box and must not read as a second section
/// strip. Not an htmx swap either, for the reason [`subnav`] gives: a view
/// changes the page's subtitle and actions, which live in the topbar. The
/// current chip is marked by weight and border as well as fill, never by
/// colour alone.
pub fn filter_chips(label: &str, chips: Vec<Tab<'_>>) -> Markup {
    html! {
        nav .filter-chips aria-label=(label) {
            @for chip in chips {
                a .filter-chip href=(chip.href) aria-current=[chip.active.then_some("page")] {
                    @if let Some(icon) = chip.icon {
                        (icon)
                    }
                    (chip.label)
                }
            }
        }
    }
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
    fn filter_chips_are_labelled_links_with_the_current_one_marked() {
        let s = filter_chips(
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
        assert_eq!(
            s,
            r#"<nav class="filter-chips" aria-label="Product views"><a class="filter-chip" href="/b/products/admin/manage" aria-current="page">Active</a><a class="filter-chip" href="/b/products/admin/manage?view=deleted">Deleted</a></nav>"#
        );
        // Plain links: a view changes the topbar, so it is never a swap.
        assert!(!s.contains("hx-"), "{s}");
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
