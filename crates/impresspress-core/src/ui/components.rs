//! Shared UI components rendered with maud.

use maud::{html, Markup};

use super::icons;

// ---------------------------------------------------------------------------
// Data Table
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Stat Card
// ---------------------------------------------------------------------------

/// Render a stat card.
pub fn stat_card(label: &str, value: &str, icon: Markup) -> Markup {
    html! {
        div .stat-card {
            div .stat-header {
                div .stat-content {
                    div .stat-label { (label) }
                    div .stat-value { (value) }
                }
                div .stat-icon { (icon) }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Search Input
// ---------------------------------------------------------------------------

/// Render a search input with htmx-powered search.
/// If `current_value` is non-empty, shows a "Results for X" banner with a clear button.
pub fn search_input(name: &str, placeholder: &str, hx_get: &str, hx_target: &str) -> Markup {
    search_input_with_value(name, placeholder, hx_get, hx_target, "")
}

/// Search input with a pre-filled value and results banner.
pub fn search_input_with_value(
    name: &str,
    placeholder: &str,
    hx_get: &str,
    hx_target: &str,
    current_value: &str,
) -> Markup {
    html! {
        @if !current_value.is_empty() {
            div .flex .items-center .gap-2 .mb-2 style="font-size:0.875rem" {
                span .text-muted { "Results for " }
                span .font-semibold { "\"" (current_value) "\"" }
                a .btn .btn-ghost .btn-sm
                    href=(hx_get)
                    hx-get=(hx_get)
                    hx-target=(hx_target)
                { (icons::x()) " Clear" }
            }
        }
        div .search-input {
            span .search-input-icon { (icons::search()) }
            input .form-input
                type="search"
                name=(name)
                placeholder=(placeholder)
                value=(current_value)
                hx-get=(hx_get)
                hx-trigger="input changed delay:300ms, search"
                hx-target=(hx_target)
                autocomplete="off";
        }
    }
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Badge — single source of truth for the small status pill
// ---------------------------------------------------------------------------

/// Color variant for [`badge`]. Typed so call sites pick a variant by name
/// rather than passing a class string; [`status_badge`] is the convenience
/// that derives the variant from a status string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeVariant {
    Success,
    Danger,
    Warning,
    Info,
}

impl BadgeVariant {
    /// Map a free-form status string to a variant. Centralizes the
    /// status→color policy in one place (the only implicit mapping, and it's
    /// presentation, not data translation).
    fn from_status(status: &str) -> Self {
        match status.to_lowercase().as_str() {
            "active" | "enabled" | "completed" | "running" => BadgeVariant::Success,
            "inactive" | "disabled" | "stopped" => BadgeVariant::Danger,
            "pending" | "draft" => BadgeVariant::Warning,
            _ => BadgeVariant::Info,
        }
    }

    fn class(self) -> &'static str {
        match self {
            BadgeVariant::Success => "badge-success",
            BadgeVariant::Danger => "badge-danger",
            BadgeVariant::Warning => "badge-warning",
            BadgeVariant::Info => "badge-info",
        }
    }
}

/// Render a colored badge pill for an explicit variant. The single badge
/// renderer — [`status_badge`] delegates here.
pub fn badge(variant: BadgeVariant, label: &str) -> Markup {
    html! {
        span .badge .(variant.class()) { (label) }
    }
}

/// Render a colored status badge, deriving the color from the status string.
pub fn status_badge(status: &str) -> Markup {
    badge(BadgeVariant::from_status(status), status)
}

// ---------------------------------------------------------------------------
// Modal
// ---------------------------------------------------------------------------

/// Render a modal container (hidden by default).
pub fn modal(id: &str, title: &str, body: Markup) -> Markup {
    html! {
        div .modal-overlay id=(id) hidden
            onclick="if(event.target===this)closeModal(this.id)"
        {
            div .modal {
                div .modal-header {
                    h3 .modal-title { (title) }
                    button .modal-close onclick={"closeModal('" (id) "')"} {
                        (icons::x())
                    }
                }
                div .modal-body {
                    (body)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Empty State
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Page Header
// ---------------------------------------------------------------------------

/// Render a page header with title, optional subtitle, and optional action slot.
///
/// The title is an `h2`: the shell's topbar owns the page's single `h1`
/// (see `ui::shell::render_topbar`), so body headers are section headings.
pub fn page_header(title: &str, subtitle: Option<&str>, action: Option<Markup>) -> Markup {
    html! {
        div .flex .items-center .justify-between .mb-4 {
            div {
                h2 .page-title { (title) }
                @if let Some(sub) = subtitle {
                    p .page-subtitle { (sub) }
                }
            }
            @if let Some(action) = action {
                div { (action) }
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

/// Size for buttons (and other form controls when adopted).
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum CtrlSize {
    Sm,
    Md,
    Lg,
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

// ---------------------------------------------------------------------------
// data_table, empty_state, pagination (Phase 1)
// ---------------------------------------------------------------------------

/// One column declaration for `data_table`.
pub struct TableCol<'a> {
    pub label: &'a str,
    pub width: Option<&'a str>, // CSS width, e.g. "160px" or "30%"
}

/// `data_table` — caller passes pre-rendered cell markup per row.
/// Sticky header. Optional row-link via `row_href` closure.
///
/// `rows` is a Vec because we need to know if it's empty. If empty, an
/// `empty_state` is rendered in place of the table body.
///
/// Each `<td>` carries `data-label="{column label}"` so the mobile
/// card-collapse CSS (`.data-table td::before { content: attr(data-label) }`,
/// the PR #75 responsive fix) labels every stacked cell automatically. Cells
/// are matched to columns positionally; a cell beyond the declared columns
/// (shouldn't happen) gets an empty label.
pub fn data_table<'a, F>(
    columns: &[TableCol<'a>],
    rows: Vec<Vec<maud::Markup>>,
    row_href: Option<F>,
    empty: maud::Markup,
) -> maud::Markup
where
    F: Fn(usize) -> Option<String>,
{
    use maud::html;
    if rows.is_empty() {
        return html! { div .data-table__empty { (empty) } };
    }
    html! {
        div .data-table {
            table {
                thead { tr {
                    @for col in columns {
                        @match col.width {
                            Some(w) => th style=(format!("width:{w}")) { (col.label) },
                            None => th { (col.label) },
                        }
                    }
                } }
                tbody {
                    @for (i, cells) in rows.into_iter().enumerate() {
                        @let href = row_href.as_ref().and_then(|f| f(i));
                        tr .(if href.is_some() { "data-table__row data-table__row--linked" } else { "data-table__row" }) {
                            @for (j, cell) in cells.into_iter().enumerate() {
                                td data-label=(columns.get(j).map(|c| c.label).unwrap_or("")) { (cell) }
                            }
                            @if let Some(h) = href {
                                td .data-table__row-href { a href=(h) aria-label="Open" { "›" } }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn empty_state(
    icon: maud::Markup,
    title: &str,
    body: &str,
    action: Option<maud::Markup>,
) -> maud::Markup {
    use maud::html;
    html! {
        div .empty {
            div .empty__icon { (icon) }
            h3 .empty__title { (title) }
            p .empty__body { (body) }
            @if let Some(a) = action { div .empty__action { (a) } }
        }
    }
}

pub fn pagination(page: u32, per_page: u32, total: u32, base_href: &str) -> maud::Markup {
    use maud::{html, PreEscaped};
    // Guard against zero `per_page` — divides by zero panics in debug and
    // produces wrong output in release. Callers can legitimately read 0
    // from query params before validation.
    let per_page = per_page.max(1);
    let total_pages = if total == 0 {
        1
    } else {
        total.div_ceil(per_page)
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

// ---------------------------------------------------------------------------
// Badge, Avatar (Phase 1)
// ---------------------------------------------------------------------------

/// Avatar — flat brand-orange circle with the seed's first character. The
/// initial varies per user, but the background does not: a single brand
/// color across every avatar is calmer to scan than a wall of per-hash
/// hues, and the colored variants previously made the admin lists feel
/// like distinct personas instead of rows of the same shape.
pub fn avatar(seed: &str, size: CtrlSize) -> maud::Markup {
    use maud::html;
    let initial = seed.chars().next().unwrap_or('?').to_ascii_uppercase();
    let size_class = match size {
        CtrlSize::Sm => "avatar--sm",
        CtrlSize::Md => "avatar--md",
        CtrlSize::Lg => "avatar--lg",
    };
    html! {
        span class={ "avatar " (size_class) } { (initial) }
    }
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

    #[test]
    fn badge_renders_variant_class_and_label() {
        let s = badge(BadgeVariant::Success, "Online").into_string();
        assert!(s.contains("badge-success"), "variant class missing: {s}");
        assert!(s.contains(">Online</span>"), "label missing: {s}");
    }

    #[test]
    fn status_badge_delegates_to_badge_with_mapped_variant() {
        // status_badge is the single status-string entry point; it derives a
        // BadgeVariant and renders through the one `badge` function.
        assert!(status_badge("active")
            .into_string()
            .contains("badge-success"));
        assert!(status_badge("disabled")
            .into_string()
            .contains("badge-danger"));
        assert!(status_badge("pending")
            .into_string()
            .contains("badge-warning"));
        // Unknown status falls to the Info variant and keeps the label text.
        let unknown = status_badge("public").into_string();
        assert!(unknown.contains("badge-info"), "default variant: {unknown}");
        assert!(unknown.contains(">public</span>"), "label text: {unknown}");
    }

    #[test]
    fn avatar_uses_brand_color_for_every_seed() {
        // Background lives in CSS (`.avatar { background: var(--primary-color) }`),
        // so the rendered markup carries no inline gradient/hue style — every
        // avatar is the same flat brand-orange circle, only the initial varies.
        let a = avatar("alice@example.com", CtrlSize::Md).into_string();
        let b = avatar("bob@example.com", CtrlSize::Md).into_string();
        for s in [&a, &b] {
            assert!(!s.contains("linear-gradient"), "no gradient in markup: {s}");
            assert!(!s.contains("hsl("), "no inline hue in markup: {s}");
            assert!(s.contains("avatar--md"), "size class present: {s}");
        }
        assert!(a.contains(">A</span>"), "alice initial A: {a}");
        assert!(b.contains(">B</span>"), "bob initial B: {b}");
    }

    #[test]
    fn avatar_is_deterministic_per_seed() {
        // Two calls with the same seed render identical markup.
        let a = avatar("alice@example.com", CtrlSize::Md).into_string();
        let b = avatar("alice@example.com", CtrlSize::Md).into_string();
        assert_eq!(a, b);
    }

    #[test]
    fn data_table_empty_renders_empty_slot() {
        let cols = [TableCol {
            label: "Name",
            width: None,
        }];
        let empty = empty_state(
            maud::html! { "📭" },
            "No users",
            "Invite someone to get started.",
            None,
        );
        let s =
            data_table::<fn(usize) -> Option<String>>(&cols, Vec::new(), None, empty).into_string();
        assert!(s.contains("data-table__empty"));
        assert!(s.contains("No users"));
        assert!(!s.contains("<tbody>"));
    }

    #[test]
    fn data_table_with_rows_renders_thead_and_tbody() {
        let cols = [
            TableCol {
                label: "Name",
                width: Some("200px"),
            },
            TableCol {
                label: "Role",
                width: None,
            },
        ];
        let rows = vec![
            vec![maud::html! { "alice" }, maud::html! { "admin" }],
            vec![maud::html! { "bob" }, maud::html! { "user" }],
        ];
        let s = data_table::<fn(usize) -> Option<String>>(
            &cols,
            rows,
            None,
            empty_state(maud::html! {}, "", "", None),
        )
        .into_string();
        assert!(s.contains("<thead>"));
        assert!(s.contains("<tbody>"));
        assert!(s.contains("alice"));
        assert!(s.contains(r#"style="width:200px""#));
    }

    #[test]
    fn data_table_row_href_renders_link_cell() {
        let cols = [TableCol {
            label: "Name",
            width: None,
        }];
        let rows = vec![vec![maud::html! { "alice" }]];
        let s = data_table(
            &cols,
            rows,
            Some(|i: usize| Some(format!("/users/{i}"))),
            empty_state(maud::html! {}, "", "", None),
        )
        .into_string();
        assert!(s.contains(r#"href="/users/0""#));
        assert!(s.contains("data-table__row--linked"));
    }

    #[test]
    fn pagination_clamps_prev_at_page_1() {
        let s = pagination(1, 25, 100, "/users").into_string();
        assert!(s.contains(r#"aria-disabled="true""#)); // prev disabled at page 1
        assert!(s.contains("100 total"));
        assert!(s.contains("1 / 4"));
    }

    #[test]
    fn pagination_appends_query_correctly() {
        let s = pagination(2, 10, 30, "/users?role=admin").into_string();
        assert!(s.contains("/users?role=admin&page=1"));
        assert!(s.contains("/users?role=admin&page=3"));
    }
}
