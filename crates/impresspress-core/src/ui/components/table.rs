//! The shared data table.

use std::borrow::Cow;

use maud::{html, Markup};

use crate::ui::icons;

/// What a column holds, which decides how its cells render — above all in
/// card mode, the stacked layout every table collapses to below 720px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColKind {
    /// An ordinary value: in card mode, its label stacked above it.
    Data,
    /// The row's name. In card mode it is the card's title — full width, no
    /// label — and the header row it sits in also carries the row's controls.
    Primary,
    /// The row's controls (buttons, an expand toggle). Its header text is
    /// for screen readers only; in card mode the cell moves into the card's
    /// header row, beside the title, and is never labelled.
    Actions,
    /// A row-selection checkbox. Same treatment as [`ColKind::Actions`], at
    /// the start of the card's header row instead of the end.
    Select,
}

/// One column declaration for a [`DataTable`].
///
/// Built with `const` methods so a table's columns can still be declared once
/// as a `const` array:
///
/// ```ignore
/// const COLUMNS: [TableCol<'static>; 3] = [
///     TableCol::new("Email").primary(),
///     TableCol::new("Roles").optional(),
///     TableCol::new("Actions").actions(),
/// ];
/// ```
#[derive(Clone, Copy, Debug)]
pub struct TableCol<'a> {
    label: &'a str,
    /// Caller-declared CSS width, e.g. `"160px"` or `"30%"`.
    width: Option<&'a str>,
    kind: ColKind,
    optional: bool,
}

impl<'a> TableCol<'a> {
    /// An ordinary data column headed `label`. The label is also every cell's
    /// `data-label`, which names the value in card mode.
    pub const fn new(label: &'a str) -> Self {
        TableCol {
            label,
            width: None,
            kind: ColKind::Data,
            optional: false,
        }
    }

    /// A fixed CSS width for the column (`"160px"`, `"30%"`).
    pub const fn width(mut self, width: &'a str) -> Self {
        self.width = Some(width);
        self
    }

    /// The row's name column: the card title in card mode. At most one per
    /// table — a second is rendered as a second title.
    pub const fn primary(mut self) -> Self {
        self.kind = ColKind::Primary;
        self
    }

    /// The row's controls. `label` ("Actions", "Details") becomes a
    /// screen-reader-only header, so the column is named without printing a
    /// word over a column of buttons.
    pub const fn actions(mut self) -> Self {
        self.kind = ColKind::Actions;
        self
    }

    /// A row-selection checkbox column; `label` ("Select") is the
    /// screen-reader-only header.
    pub const fn select(mut self) -> Self {
        self.kind = ColKind::Select;
        self
    }

    /// Drop the column — header and cells — when every row's cell is empty,
    /// so a column nobody filled in does not print a header over nothing.
    /// Not for a table whose rows are also swapped in one at a time
    /// ([`TableRow::render`]): a standalone row cannot know the column was
    /// dropped and would come back one cell too long.
    pub const fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// The column's label.
    pub const fn label(&self) -> &'a str {
        self.label
    }

    /// The cell class for this column, or `None` for an ordinary data cell
    /// (which keeps the bare `<td data-label>` every existing table emits).
    fn cell_class(&self) -> Option<&'static str> {
        match self.kind {
            ColKind::Data => None,
            ColKind::Primary => Some("data-table__cell--primary"),
            ColKind::Actions => Some("data-table__cell--actions"),
            ColKind::Select => Some("data-table__cell--select"),
        }
    }

    /// Whether the header text is for screen readers only.
    fn header_hidden(&self) -> bool {
        matches!(self.kind, ColKind::Actions | ColKind::Select)
    }
}

/// Whether a cell's markup renders nothing at all. Such a cell is hidden in
/// card mode (an empty value under its label is noise) and is what an
/// [`optional`](TableCol::optional) column is dropped for.
fn is_blank(cell: &Markup) -> bool {
    cell.0.trim().is_empty()
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
        self.render_visible(columns, &vec![true; columns.len()], href)
    }

    /// [`render`](TableRow::render) with the columns a [`DataTable`] dropped
    /// (`visible[j] == false`) left out.
    fn render_visible(
        self,
        columns: &[TableCol<'_>],
        visible: &[bool],
        href: Option<String>,
    ) -> Markup {
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
                    @if visible.get(j).copied().unwrap_or(true) {
                        @let col = columns.get(j);
                        @let class = cell_class(col.and_then(TableCol::cell_class), is_blank(&cell));
                        td class=[class] data-label=(col.map(|c| c.label).unwrap_or("")) { (cell) }
                    }
                }
                @if let Some(h) = href {
                    // The chevron is the row's link for keyboard and
                    // screen-reader users; pointer users can click anywhere
                    // on the row (`chrome.js` forwards the click here).
                    td .data-table__row-href { a href=(h) aria-label="Open" { (icons::chevron_right()) } }
                }
            }
            @if let Some(after) = after { (after) }
        }
    }
}

/// A `<td>`'s class list: the column kind's class, then `--empty` for a cell
/// with no content. `None` for an ordinary, non-empty data cell.
fn cell_class(kind: Option<&'static str>, blank: bool) -> Option<Cow<'static, str>> {
    match (kind, blank) {
        (None, false) => None,
        (Some(k), false) => Some(Cow::Borrowed(k)),
        (None, true) => Some(Cow::Borrowed("data-table__cell--empty")),
        (Some(k), true) => Some(Cow::Owned(format!("{k} data-table__cell--empty"))),
    }
}

/// How a [`DataTable`] lays out on a narrow viewport.
enum Layout {
    /// Below 720px, each row becomes a card (the default).
    Cards,
    /// A grid at every width, inside a labelled, focusable horizontal
    /// scroller — for tables whose columns are data, not a record (the SQL
    /// explorer's result grid), where stacking would lose the grid.
    Scroll { label: String },
}

/// The shared data table.
///
/// Sticky header; optional row link; and below 720px a *card mode*, in which
/// each row becomes a card: the [`primary`](TableCol::primary) cell is the
/// card's title, the [`actions`](TableCol::actions) and
/// [`select`](TableCol::select) cells sit in the card's header row beside it,
/// every other cell stacks its label (`data-label`, from the column) above
/// its value, and empty cells are hidden. Each `<td>` carries
/// `data-label="{column label}"` for that. Cells are matched to columns
/// positionally; a cell beyond the declared columns (shouldn't happen) gets
/// an empty label.
///
/// `rows` is a `Vec` because the component needs to know whether it is empty:
/// an empty table renders the empty slot in place of the whole table, header
/// included — use [`empty_state`](DataTable::empty_state) for it.
///
/// [`data_table`] is the four-argument shorthand and delegates here.
pub struct DataTable<'a> {
    columns: &'a [TableCol<'a>],
    rows: Vec<TableRow>,
    row_href: Option<Box<dyn Fn(usize) -> Option<String> + 'a>>,
    empty: Markup,
    head: bool,
    layout: Layout,
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
            layout: Layout::Cards,
        }
    }

    /// The rows, in render order.
    pub fn rows(mut self, rows: Vec<TableRow>) -> Self {
        self.rows = rows;
        self
    }

    /// Make each row link to a destination, by row index. Adds the trailing
    /// chevron cell (a labelled 44px link) and makes the whole row clickable.
    pub fn row_href(mut self, href: impl Fn(usize) -> Option<String> + 'a) -> Self {
        self.row_href = Some(Box::new(href));
        self
    }

    /// What to render when there are no rows, as raw markup. Prefer
    /// [`empty_state`](DataTable::empty_state).
    pub fn empty(mut self, empty: Markup) -> Self {
        self.empty = empty;
        self
    }

    /// The shared empty state ([`super::empty_state`]) in place of the whole
    /// table when there are no rows: a title, one sentence and an optional
    /// call to action. No header is rendered over it.
    pub fn empty_state(mut self, title: &str, body: &str, action: Option<Markup>) -> Self {
        self.empty = super::empty_state(icons::inbox(), title, body, action);
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

    /// Keep the grid at every width inside a horizontal scroller named
    /// `label`. The scroller is a focusable region (`tabindex="0"`), so a
    /// keyboard user can scroll it, and its scrollbar is always visible.
    /// For wide grids of data (the SQL explorer's results) rather than lists
    /// of records.
    pub fn scroll(mut self, label: impl Into<String>) -> Self {
        self.layout = Layout::Scroll {
            label: label.into(),
        };
        self
    }

    /// Render the table.
    pub fn render(self) -> Markup {
        if self.rows.is_empty() {
            return html! { div .data-table__empty { (self.empty) } };
        }
        let columns = self.columns;
        // An optional column is dropped when no row fills it in.
        let visible: Vec<bool> = columns
            .iter()
            .enumerate()
            .map(|(j, col)| {
                !col.optional
                    || self
                        .rows
                        .iter()
                        .any(|r| r.cells.get(j).is_some_and(|c| !is_blank(c)))
            })
            .collect();
        let head = self.head;
        let row_href = self.row_href;
        let linked = row_href.is_some();
        let table = html! {
            table {
                @if head {
                    thead { tr {
                        @for (col, _) in columns.iter().zip(&visible).filter(|(_, v)| **v) {
                            @let label = html! {
                                @if col.header_hidden() { span .sr-only { (col.label) } } @else { (col.label) }
                            };
                            @match col.width {
                                // Caller-declared, per-instance column width -- a
                                // genuine runtime value, so it's handed to CSS as
                                // a custom property rather than a literal inline
                                // width declaration.
                                Some(w) => th .data-table__col-w style=(format!("--col-width:{w}")) { (label) },
                                None => th { (label) },
                            }
                        }
                        @if linked { th .data-table__row-href { span .sr-only { "Open" } } }
                    } }
                }
                tbody {
                    @for (i, row) in self.rows.into_iter().enumerate() {
                        (row.render_visible(columns, &visible, row_href.as_ref().and_then(|f| f(i))))
                    }
                }
            }
        };
        match self.layout {
            Layout::Cards => html! { div .data-table { (table) } },
            Layout::Scroll { label } => html! {
                div .data-table .data-table--scroll role="region" tabindex="0" aria-label=(label) { (table) }
            },
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
/// `{org}__{block}__{name}` table or variable key, a request path, an email —
/// with a line-break opportunity (`<wbr>`) at the points where it reads as
/// separate words: after each `/`, after each run of `_`, after the `?`, `&`
/// and `=` of a query string and after a `:`; and *before* an `@` or a `.`,
/// so a domain or an extension starts the next line rather than ending the
/// last one. Such an identifier has no spaces, so without these a long one
/// sets its column's minimum width and pushes the table past its card; with
/// them it wraps between its words, never inside one, and only where the
/// column is too narrow for it. No break is offered at either end, and the
/// text content is unchanged: `<wbr>` adds no characters.
pub fn breakable_id(id: &str) -> Markup {
    // Byte offsets at which a new part starts. Every separator is ASCII, so
    // each offset is a char boundary.
    let bytes = id.as_bytes();
    let mut cuts: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'_' => {
                // A run of underscores is one separator; break after all of it.
                let mut j = i;
                while j < bytes.len() && bytes[j] == b'_' {
                    j += 1;
                }
                cuts.push(j);
                i = j;
                continue;
            }
            // Not inside a `//` (a scheme's `://`, a doubled slash).
            b'/' | b':' if i > 0 && bytes.get(i + 1) != Some(&b'/') && bytes[i - 1] != b'/' => {
                cuts.push(i + 1)
            }
            b'?' | b'&' | b'=' => cuts.push(i + 1),
            b'@' | b'.' if i > 0 && bytes[i - 1] != b'.' && bytes[i - 1] != b'/' => cuts.push(i),
            _ => {}
        }
        i += 1;
    }
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0;
    for cut in cuts {
        if cut > start && cut < bytes.len() {
            parts.push(&id[start..cut]);
            start = cut;
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
        let cols = [TableCol::new("Name")];
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
        let cols = [TableCol::new("Name").width("200px"), TableCol::new("Role")];
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
        let cols = [TableCol::new("Name")];
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
        let cols = [TableCol::new("Name"), TableCol::new("Role")];
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
        let cols = [TableCol::new("Key")];
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
        let cols = [TableCol::new("Key")];
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
        let cols = [TableCol::new("Email"), TableCol::new("Created")];
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
        let cols = [TableCol::new("Name")];
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
        let cols = [TableCol::new("Email")];
        let s = DataTable::new(&cols)
            .rows(vec![TableRow::new(vec![html! { "a@example.com" }])])
            .headless()
            .render()
            .into_string();
        assert!(!s.contains("<thead>"), "{s}");
        assert!(s.contains(r#"data-label="Email""#), "{s}");
    }

    // ── Card mode, optional columns, the empty state ──────────────────

    const CARD_COLS: [TableCol<'static>; 4] = [
        TableCol::new("Select").select(),
        TableCol::new("Email").primary(),
        TableCol::new("Roles").optional(),
        TableCol::new("Actions").actions(),
    ];

    fn card_row(email: &str, roles: &str) -> TableRow {
        TableRow::new(vec![
            html! { input type="checkbox"; },
            html! { (email) },
            html! { (roles) },
            html! { button .btn { "Delete" } },
        ])
    }

    /// The primary, actions and select cells carry the classes the card
    /// layout places in the card's header row; an ordinary cell keeps the
    /// bare `<td data-label>`.
    #[test]
    fn card_mode_cells_carry_their_column_kind() {
        let s = DataTable::new(&CARD_COLS)
            .rows(vec![card_row("a@example.com", "admin")])
            .render()
            .into_string();
        assert!(
            s.contains(r#"<td class="data-table__cell--select" data-label="Select"><input type="checkbox"></td>"#),
            "{s}"
        );
        assert!(
            s.contains(
                r#"<td class="data-table__cell--primary" data-label="Email">a@example.com</td>"#
            ),
            "{s}"
        );
        assert!(s.contains(r#"<td data-label="Roles">admin</td>"#), "{s}");
        assert!(
            s.contains(r#"<td class="data-table__cell--actions" data-label="Actions"><button class="btn">Delete</button></td>"#),
            "{s}"
        );
    }

    /// Control columns are named for assistive technology only — no visible
    /// word over a column of buttons, and no empty `<th>` either.
    #[test]
    fn control_column_headers_are_screen_reader_only() {
        let s = DataTable::new(&CARD_COLS)
            .rows(vec![card_row("a@example.com", "admin")])
            .row_href(|_| Some("/x".into()))
            .render()
            .into_string();
        assert!(
            s.contains(r#"<th><span class="sr-only">Select</span></th>"#),
            "{s}"
        );
        assert!(s.contains(r#"<th>Email</th>"#), "{s}");
        assert!(
            s.contains(r#"<th><span class="sr-only">Actions</span></th>"#),
            "{s}"
        );
        // The row-link column has a header too, so header and row have the
        // same number of cells.
        assert!(
            s.contains(
                r#"<th class="data-table__row-href"><span class="sr-only">Open</span></th>"#
            ),
            "{s}"
        );
        assert!(!s.contains("<th></th>"), "{s}");
    }

    /// An empty cell is marked so card mode can hide it, whatever its kind.
    #[test]
    fn empty_cells_are_marked_for_card_mode() {
        let s = DataTable::new(&CARD_COLS)
            .rows(vec![
                card_row("a@example.com", "admin"),
                card_row("b@example.com", ""),
            ])
            .render()
            .into_string();
        assert!(
            s.contains(r#"<td class="data-table__cell--empty" data-label="Roles"></td>"#),
            "{s}"
        );
        let blank_action =
            TableRow::new(vec![html! {}, html! { "x" }, html! { "r" }, html! { " " }])
                .render(&CARD_COLS, None)
                .into_string();
        assert!(
            blank_action.contains(r#"class="data-table__cell--actions data-table__cell--empty""#),
            "{blank_action}"
        );
    }

    /// An optional column nobody filled in is dropped — header and every
    /// cell — and kept as soon as one row fills it in.
    #[test]
    fn an_all_empty_optional_column_is_dropped_with_its_header() {
        let dropped = DataTable::new(&CARD_COLS)
            .rows(vec![
                card_row("a@example.com", ""),
                card_row("b@example.com", "  "),
            ])
            .render()
            .into_string();
        assert!(!dropped.contains("Roles"), "{dropped}");
        assert_eq!(
            dropped.matches("<td").count(),
            6,
            "three cells per row: {dropped}"
        );

        let kept = DataTable::new(&CARD_COLS)
            .rows(vec![
                card_row("a@example.com", ""),
                card_row("b@example.com", "admin"),
            ])
            .render()
            .into_string();
        assert!(kept.contains("<th>Roles</th>"), "{kept}");
        assert_eq!(kept.matches(r#"data-label="Roles""#).count(), 2, "{kept}");
    }

    /// A non-optional column stays even when it is empty everywhere.
    #[test]
    fn a_required_column_is_never_dropped() {
        let cols = [TableCol::new("Name").primary(), TableCol::new("Note")];
        let s = DataTable::new(&cols)
            .rows(vec![TableRow::new(vec![html! { "a" }, html! {}])])
            .render()
            .into_string();
        assert!(s.contains("<th>Note</th>"), "{s}");
    }

    /// `.empty_state(..)` renders the shared empty state in place of the
    /// whole table — no header over nothing.
    #[test]
    fn empty_state_replaces_the_whole_table() {
        let s = DataTable::new(&CARD_COLS)
            .empty_state(
                "No users",
                "Invite someone to get started.",
                Some(html! { a .btn href="/invite" { "Invite" } }),
            )
            .render()
            .into_string();
        assert!(
            s.starts_with(r#"<div class="data-table__empty"><div class="empty">"#),
            "{s}"
        );
        assert!(
            s.contains(r#"<h2 class="empty__title">No users</h2>"#),
            "{s}"
        );
        assert!(
            s.contains(r#"<p class="empty__body">Invite someone to get started.</p>"#),
            "{s}"
        );
        assert!(
            s.contains(
                r#"<div class="empty__action"><a class="btn" href="/invite">Invite</a></div>"#
            ),
            "{s}"
        );
        assert!(!s.contains("<table") && !s.contains("<th"), "{s}");
    }

    /// A scrolling grid is a labelled, focusable region, so a keyboard user
    /// can scroll it (axe `scrollable-region-focusable`).
    #[test]
    fn scroll_layout_is_a_labelled_focusable_region() {
        let cols = [TableCol::new("id"), TableCol::new("created_at")];
        let s = DataTable::new(&cols)
            .rows(vec![TableRow::new(vec![html! { "1" }, html! { "x" }])])
            .scroll("Query results")
            .render()
            .into_string();
        assert!(
            s.starts_with(r#"<div class="data-table data-table--scroll" role="region" tabindex="0" aria-label="Query results"><table>"#),
            "{s}"
        );
    }

    /// The row link is a labelled anchor around the chevron icon.
    #[test]
    fn row_link_is_a_labelled_chevron() {
        let s = TableRow::new(vec![html! { "a" }])
            .render(&[TableCol::new("Name").primary()], Some("/a".into()))
            .into_string();
        assert!(
            s.contains(r#"<td class="data-table__row-href"><a href="/a" aria-label="Open"><svg"#),
            "{s}"
        );
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
    fn breaks_only_between_the_words_of_an_identifier() {
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
            // An email breaks before the `@` and the domain's dots.
            ("reviewer41@example.com", "reviewer41<wbr>@example<wbr>.com"),
            // A query string breaks after `?`, `&` and `=`; a scheme's `://`
            // stays whole.
            (
                "https://x.io/p?a=1&b=2",
                "https://x<wbr>.io/<wbr>p?<wbr>a=<wbr>1&amp;<wbr>b=<wbr>2",
            ),
            // A leading dot (a dotfile) is not a break; nor is `..`.
            ("/.env", "/.env"),
            ("a/../b", "a/<wbr>../<wbr>b"),
            ("", ""),
        ];
        for (id, want) in cases {
            assert_eq!(breakable_id(id).into_string(), want, "{id}");
        }
    }
}
