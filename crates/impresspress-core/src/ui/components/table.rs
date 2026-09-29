//! Data Table (Phase 1)

use std::borrow::Cow;

use maud::{html, Markup};

/// One column declaration for `data_table`.
pub struct TableCol<'a> {
    pub label: &'a str,
    pub width: Option<&'a str>, // CSS width, e.g. "160px" or "30%"
}

/// One row of a [`DataTable`].
///
/// The cells are the row's *inner* markup, one `Markup` per column; the
/// component owns the `<td>` and carries what it is given through verbatim.
/// That is deliberate and is what lets a caller keep something inside a cell
/// that must survive the migration — the visual-baseline suite's mask hooks
/// (`<time>`, `<span data-volatile-metric>`) are the worked example — without
/// the component growing a general per-cell attribute affordance.
///
/// The three optional pieces exist because administration's tables needed
/// them and nothing in the cell markup could express them:
///
/// - [`id`](TableRow::id) — an htmx swap target. The users table swaps one
///   row's `outerHTML` after an enable/disable.
/// - [`classes`](TableRow::classes) — extra classes on the `<tr>` itself,
///   for a row that carries a page-local behaviour or style
///   (`.expand-row` on the network page).
/// - [`after`](TableRow::after) — markup emitted immediately after the row's
///   `</tr>`, still inside the `<tbody>`. A row whose detail is loaded
///   lazily into a second, full-width `<tr>` needs one; the component does
///   not know what that second row contains, so the caller writes it.
pub struct TableRow {
    cells: Vec<Markup>,
    id: Option<String>,
    classes: String,
    after: Option<Markup>,
}

impl TableRow {
    /// A plain row: one cell of inner markup per column, no id, no extra
    /// classes, nothing after it.
    pub fn new(cells: Vec<Markup>) -> Self {
        TableRow {
            cells,
            id: None,
            classes: String::new(),
            after: None,
        }
    }

    /// The `<tr>`'s `id`, emitted before `class`. An htmx `hx-target` needs
    /// one; nothing else should.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Extra classes appended after the row's own `data-table__row` classes,
    /// space-separated in the order given.
    pub fn classes(mut self, classes: impl Into<String>) -> Self {
        self.classes = classes.into();
        self
    }

    /// Markup emitted immediately after this row's `</tr>`, inside the same
    /// `<tbody>`. The caller writes a complete `<tr>`; the component only
    /// places it.
    pub fn after(mut self, markup: Markup) -> Self {
        self.after = Some(markup);
        self
    }

    /// Render this row against `columns`, with or without a surrounding table
    /// — the `<tr>` plus whatever [`after`](TableRow::after) puts behind it.
    /// `href`, when set, appends the row-link chevron cell and puts the row in
    /// its `--linked` state, exactly as a [`DataTable`] whose
    /// [`row_href`](DataTable::row_href) returned that destination for this
    /// row would have emitted it.
    ///
    /// The link target is a parameter rather than a default because the
    /// standalone case exists for the htmx `outerHTML` swap of a single row:
    /// the administration users table replaces one row after an enable or a
    /// disable, and the replacement has to carry the same classes and the same
    /// `data-label` cells as the row it replaces. Rendering it through the
    /// component is what stops the two from drifting — and a row swapped into
    /// a *linked* table that defaulted to `None` would silently come back one
    /// cell short and unclickable. Every caller states which it is.
    pub fn render(self, columns: &[TableCol<'_>], href: Option<String>) -> Markup {
        let TableRow {
            cells,
            id,
            classes,
            after,
        } = self;
        let row_class = row_class(&classes, href.is_some());
        html! {
            tr id=[id] class=(row_class) {
                @for (j, cell) in cells.into_iter().enumerate() {
                    td data-label=(columns.get(j).map(|c| c.label).unwrap_or("")) { (cell) }
                }
                @if let Some(h) = href {
                    td .data-table__row-href { a href=(h) aria-label="Open" { "›" } }
                }
            }
            @if let Some(after) = after { (after) }
        }
    }
}

/// The shared data table.
///
/// Sticky header, mobile card-collapse, optional row link. Each `<td>`
/// carries `data-label="{column label}"` so the mobile card-collapse CSS
/// (`.data-table td::before { content: attr(data-label) }`, the PR #75
/// responsive fix) labels every stacked cell automatically. Cells are matched
/// to columns positionally; a cell beyond the declared columns (shouldn't
/// happen) gets an empty label.
///
/// `rows` is a `Vec` because the component needs to know whether it is empty:
/// an empty table renders the `empty` slot in place of the whole table, header
/// included.
///
/// [`data_table`] is the four-argument shorthand and delegates here.
pub struct DataTable<'a> {
    columns: &'a [TableCol<'a>],
    rows: Vec<TableRow>,
    row_href: Option<Box<dyn Fn(usize) -> Option<String> + 'a>>,
    empty: Markup,
    head: bool,
}

impl<'a> DataTable<'a> {
    /// An empty table over `columns`: no rows, no row link, an empty
    /// empty-slot, header shown.
    pub fn new(columns: &'a [TableCol<'a>]) -> Self {
        DataTable {
            columns,
            rows: Vec::new(),
            row_href: None,
            empty: html! {},
            head: true,
        }
    }

    /// The rows, in render order.
    pub fn rows(mut self, rows: Vec<TableRow>) -> Self {
        self.rows = rows;
        self
    }

    /// Make each row link to a destination, by row index. Adds the trailing
    /// chevron cell and the `--linked` hover affordance.
    pub fn row_href(mut self, href: impl Fn(usize) -> Option<String> + 'a) -> Self {
        self.row_href = Some(Box::new(href));
        self
    }

    /// What to render when there are no rows.
    pub fn empty(mut self, empty: Markup) -> Self {
        self.empty = empty;
        self
    }

    /// Suppress the `<thead>`. The column labels are still declared and still
    /// reach every `<td>`'s `data-label`, so the mobile card-collapse keeps
    /// naming its cells — this only drops the header band. For a table that
    /// reads as a two-column list rather than as a grid (the administration
    /// dashboard's "Recent Users" card).
    pub fn headless(mut self) -> Self {
        self.head = false;
        self
    }

    /// Render the table.
    pub fn render(self) -> Markup {
        if self.rows.is_empty() {
            return html! { div .data-table__empty { (self.empty) } };
        }
        let columns = self.columns;
        let head = self.head;
        let row_href = self.row_href;
        html! {
            div .data-table {
                table {
                    @if head {
                        thead { tr {
                            @for col in columns {
                                @match col.width {
                                    // Caller-declared, per-instance column width -- a
                                    // genuine runtime value, so it's handed to CSS as
                                    // a custom property rather than a literal inline
                                    // width declaration.
                                    Some(w) => th .data-table__col-w style=(format!("--col-width:{w}")) { (col.label) },
                                    None => th { (col.label) },
                                }
                            }
                        } }
                    }
                    tbody {
                        @for (i, row) in self.rows.into_iter().enumerate() {
                            (row.render(columns, row_href.as_ref().and_then(|f| f(i))))
                        }
                    }
                }
            }
        }
    }
}

/// The `<tr>`'s class list: the base row class, `--linked` when the row is a
/// link, then whatever the caller added. Borrowed for the two unadorned
/// shapes, which is every row products renders.
fn row_class(extra: &str, linked: bool) -> Cow<'_, str> {
    let base = if linked {
        "data-table__row data-table__row--linked"
    } else {
        "data-table__row"
    };
    if extra.is_empty() {
        Cow::Borrowed(base)
    } else {
        Cow::Owned(format!("{base} {extra}"))
    }
}

/// An identifier for a table cell — a `{org}/{block}` name, a
/// `{org}__{block}__{name}` table or variable key, a request path — with a
/// line-break opportunity (`<wbr>`) after each `/` and after each run of `_`,
/// the points where it reads as separate words. Such an identifier has no
/// spaces, so without these a long one sets its column's minimum width and
/// pushes the table past its card; with them it wraps between its words,
/// never inside one, and only where the column is too narrow for it. The text
/// content is unchanged: `<wbr>` adds no characters.
pub fn breakable_id(id: &str) -> Markup {
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0;
    let bytes = id.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let end = if bytes[i] == b'/' && i > 0 {
            Some(i + 1)
        } else if bytes[i] == b'_' {
            // A run of underscores is one separator; break after all of it.
            let mut j = i;
            while j < bytes.len() && bytes[j] == b'_' {
                j += 1;
            }
            Some(j)
        } else {
            None
        };
        match end {
            Some(end) if end < bytes.len() => {
                parts.push(&id[start..end]);
                start = end;
                i = end;
            }
            Some(end) => i = end,
            None => i += 1,
        }
    }
    parts.push(&id[start..]);
    html! {
        @for (n, part) in parts.iter().enumerate() {
            @if n > 0 { wbr; }
            (part)
        }
    }
}

/// `data_table` — caller passes pre-rendered cell markup per row.
/// Sticky header. Optional row-link via `row_href` closure.
///
/// The four-argument shorthand for [`DataTable`], for the majority of call
/// sites whose rows carry no id, no extra classes and no follow-up row.
pub fn data_table<'a, F>(
    columns: &[TableCol<'a>],
    rows: Vec<Vec<maud::Markup>>,
    row_href: Option<F>,
    empty: maud::Markup,
) -> maud::Markup
where
    F: Fn(usize) -> Option<String>,
{
    let mut table = DataTable::new(columns)
        .rows(rows.into_iter().map(TableRow::new).collect())
        .empty(empty);
    if let Some(f) = row_href {
        table = table.row_href(f);
    }
    table.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{components::empty_state, icons};

    #[test]
    fn data_table_empty_renders_empty_slot() {
        let cols = [TableCol {
            label: "Name",
            width: None,
        }];
        let empty = empty_state(
            icons::inbox(),
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
        assert!(s.contains(r#"style="--col-width:200px""#));
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

    /// The shorthand is the builder with nothing set, so its bytes must not
    /// have moved — every products call site goes through it.
    #[test]
    fn shorthand_and_builder_render_the_same_bytes() {
        let cols = [
            TableCol {
                label: "Name",
                width: None,
            },
            TableCol {
                label: "Role",
                width: None,
            },
        ];
        let cells = || {
            vec![
                vec![maud::html! { "alice" }, maud::html! { "admin" }],
                vec![maud::html! { "bob" }, maud::html! { "user" }],
            ]
        };
        let shorthand =
            data_table::<fn(usize) -> Option<String>>(&cols, cells(), None, html! {}).into_string();
        let builder = DataTable::new(&cols)
            .rows(cells().into_iter().map(TableRow::new).collect())
            .render()
            .into_string();
        assert_eq!(shorthand, builder);
        assert!(shorthand.contains(r#"<div class="data-table">"#));
        assert!(shorthand.contains(r#"<tr class="data-table__row">"#));
    }

    #[test]
    fn row_id_and_classes_reach_the_tr() {
        let cols = [TableCol {
            label: "Key",
            width: None,
        }];
        let s = DataTable::new(&cols)
            .rows(vec![TableRow::new(vec![html! { "k" }])
                .id("var-row-K")
                .classes("expand-row")])
            .render()
            .into_string();
        assert!(
            s.contains(r#"<tr id="var-row-K" class="data-table__row expand-row">"#),
            "{s}"
        );
    }

    #[test]
    fn row_after_markup_follows_the_row_inside_the_tbody() {
        let cols = [TableCol {
            label: "Key",
            width: None,
        }];
        let s = DataTable::new(&cols)
            .rows(vec![TableRow::new(vec![html! { "k" }]).after(
                html! { tr .detail-rows hidden { td colspan="1" { "detail" } } },
            )])
            .render()
            .into_string();
        assert!(
            s.contains(
                r#"</tr><tr class="detail-rows" hidden><td colspan="1">detail</td></tr></tbody>"#
            ),
            "{s}"
        );
    }

    /// The single-row htmx swap has to be byte-identical to the row the table
    /// itself emits, or the swapped-in row loses its classes and its
    /// `data-label` cells the moment it is replaced.
    #[test]
    fn standalone_row_matches_the_row_the_table_emits() {
        let cols = [
            TableCol {
                label: "Email",
                width: None,
            },
            TableCol {
                label: "Created",
                width: None,
            },
        ];
        let row = || {
            TableRow::new(vec![html! { "a@example.com" }, html! { "2026-01-01" }]).id("user-row-1")
        };
        let in_table = DataTable::new(&cols)
            .rows(vec![row()])
            .render()
            .into_string();
        let standalone = row().render(&cols, None).into_string();
        assert!(in_table.contains(&standalone), "{in_table} !⊇ {standalone}");
        assert!(
            standalone.starts_with(r#"<tr id="user-row-1" class="data-table__row">"#),
            "{standalone}"
        );
        assert!(standalone.ends_with("</tr>"), "{standalone}");
    }

    /// The same guarantee for a *linked* table. A row swapped into one has to
    /// carry the trailing chevron cell and the `--linked` modifier, or the
    /// replacement comes back one cell short and unclickable.
    #[test]
    fn standalone_row_matches_the_linked_row_the_table_emits() {
        let cols = [TableCol {
            label: "Name",
            width: None,
        }];
        let row = || TableRow::new(vec![html! { "widget" }]);
        let in_table = DataTable::new(&cols)
            .rows(vec![row()])
            .row_href(|_| Some("/b/products/admin/products/widget".to_string()))
            .render()
            .into_string();
        let standalone = row()
            .render(&cols, Some("/b/products/admin/products/widget".to_string()))
            .into_string();
        assert!(in_table.contains(&standalone), "{in_table} !⊇ {standalone}");
        assert!(
            standalone.contains("data-table__row--linked"),
            "{standalone}"
        );
        assert!(standalone.contains("data-table__row-href"), "{standalone}");
    }

    #[test]
    fn headless_drops_the_thead_but_keeps_the_cell_labels() {
        let cols = [TableCol {
            label: "Email",
            width: None,
        }];
        let s = DataTable::new(&cols)
            .rows(vec![TableRow::new(vec![html! { "a@example.com" }])])
            .headless()
            .render()
            .into_string();
        assert!(!s.contains("<thead>"), "{s}");
        assert!(s.contains(r#"data-label="Email""#), "{s}");
    }

    /// Every file that still writes a first-generation `table .table` by hand
    /// rather than through this module, with the number it writes. Same
    /// ratchet as `only_the_declared_files_still_hand_write_badge_markup`: an
    /// unlisted file must render none, and a listed file's count must be
    /// exact, so a migration cannot half-land and a new raw table cannot
    /// appear unrecorded.
    ///
    /// **This list is what blocks the deletion of the first-generation table
    /// stylesheet.** `ui/styles/components/table.css` still carries
    /// `.table-container`, `.table`, `.table th`, `.table td`,
    /// `.table tbody tr:hover`, and the `max-width: 720px`
    /// `white-space: nowrap` rule, for these ten tables and nothing else —
    /// administration's 19 moved to `.data-table` in the pull request before
    /// this one. (`.table th.sortable` went in the same change as this test:
    /// the sortable header was a `components.rs` affordance that the phase-3a
    /// administration port deleted, and its two rules outlived it.) `pages_use_only_classes_defined_in_the_stylesheet`
    /// (`ui/mod.rs`) fails the build if those rules are deleted while any
    /// entry below survives, which is why the deletion could not ship here.
    ///
    /// When the last entry goes, delete those rules with it and delete this
    /// test. Migrating one is not free: `.data-table` is a different chrome
    /// (rounded, bordered, sticky `thead`, dashed row rules, a `data-label`
    /// per cell that collapses to cards below 720px), so each entry is a
    /// rendered change. Six of the ten sit on pages with no visual baseline
    /// at all — `blocks/legalpages` and `blocks/tickets` have none — so the
    /// gate there is a Rust render test, not a screenshot.
    ///
    /// The scope is maud's bare `.table` class shorthand, the form all ten are
    /// written in; the counter is `test_support::count_bare_class_shorthand`,
    /// shared with the badge ratchet rather than copied, and it reads every
    /// boundary maud accepts, unspaced ones included.
    const HAND_WRITTEN_TABLES: &[(&str, usize)] = &[
        ("blocks/legalpages/pages.rs", 3),
        ("blocks/llm/pages.rs", 1),
        ("blocks/llm/ui.rs", 2),
        ("blocks/tickets/pages.rs", 3),
        ("blocks/userportal/pages/admin_buttons.rs", 1),
    ];

    #[test]
    fn only_the_declared_files_still_hand_write_a_first_generation_table() {
        let expected: std::collections::BTreeMap<&str, usize> =
            HAND_WRITTEN_TABLES.iter().copied().collect();
        // Skipped for the reason the badge ratchet skips its own pair: string
        // literals are not masked, this file names `.table` in the assertion
        // below and in the doc comment's rule list, and `ui/test_support.rs`
        // holds the counter's own fixtures. Neither renders a page.
        let found = crate::ui::test_support::hand_written_class_shorthand(
            "table",
            &["ui/components/table.rs", "ui/test_support.rs"],
        );
        let found_refs: std::collections::BTreeMap<&str, usize> =
            found.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            found_refs, expected,
            "first-generation table markup moved; update HAND_WRITTEN_TABLES only to \
             remove entries or lower counts, and when it empties delete the \
             .table / .table-container rules from ui/styles/components/table.css"
        );
    }
}

#[cfg(test)]
mod breakable_id_tests {
    use super::breakable_id;

    #[test]
    fn breaks_after_each_slash_and_underscore_run_only() {
        let cases = [
            ("impresspress/auth-ui", "impresspress/<wbr>auth-ui"),
            (
                "ACME_CO__WIDGETS_MAX",
                "ACME_<wbr>CO__<wbr>WIDGETS_<wbr>MAX",
            ),
            ("acme__widgets__orders", "acme__<wbr>widgets__<wbr>orders"),
            ("/b/admin/network", "/b/<wbr>admin/<wbr>network"),
            ("acme__shop__*", "acme__<wbr>shop__<wbr>*"),
            ("acme___x", "acme___<wbr>x"),
            ("trailing/", "trailing/"),
            ("plain", "plain"),
            ("", ""),
        ];
        for (id, want) in cases {
            assert_eq!(breakable_id(id).into_string(), want, "{id}");
        }
    }
}
