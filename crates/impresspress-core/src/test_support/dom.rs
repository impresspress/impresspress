//! Questions about a rendered page that only its element tree can answer.
//!
//! The admin layout rules (one visible primary action per page, one level of
//! cards, a heading outline that starts at the page title and never skips a
//! level) are statements about ancestry and visibility: a `.btn--primary`
//! *outside* every `<dialog>`, a `.stat` *under* a `.card`, an `<h2>` *not
//! inside* a `[hidden]` region. A scan over the page text cannot see any of
//! that. It finds the tag, but not which element encloses it, so a guard
//! built on one is either wrong (it counts the primary in a closed dialog) or
//! quietly loose (it gives up on nesting and checks nothing).
//!
//! So this module parses the page with html5ever, through `scraper`, into the
//! same tree a browser builds, and answers each question by walking it. The
//! parse is the browser's, including the parts a hand-written walker gets
//! wrong: implied end tags, `<template>` contents (html5ever keeps them under
//! a fragment node that is the template's child, so they are still the
//! template's descendants here), and attribute values with escapes in them.
//!
//! "Visible" means the static visibility the markup states, and nothing that
//! needs a stylesheet or a script: an element is concealed when it, or an
//! ancestor, is a `<template>` or carries the `hidden` attribute. Whatever a
//! page reveals at runtime is the e2e suite's to check.
//!
//! Every function takes the page as text and parses it itself, so a guard
//! hands over what the renderer produced and nothing else. Elements are
//! reported by [`describe`]: the tag, its classes and its text, which is what
//! a failing guard needs to point at the offending element.

use scraper::{ElementRef, Html, Node, Selector};

/// The class that makes a button the page's primary action.
const PRIMARY: &str = "btn--primary";

/// The classes that draw a card-level surface on the page background. Rule 2
/// of the layout: none of them sits inside a `.card`.
const CARD_LEVEL: [&str; 5] = ["card", "stat", "empty", "chat-panes", "data-table"];

/// The `.btn--primary` elements a reader can see, in document order.
///
/// The first list holds the ones on the page itself, outside every
/// `<dialog>`. The second holds one list per visible `<dialog>`, in document
/// order, with the primaries whose closest `<dialog>` is that one; a dialog
/// with none has an empty list, so the outer list says how many dialogs were
/// looked at. Anything that is, or is inside, a `<template>` or a `[hidden]`
/// element is skipped, a dialog included.
pub fn visible_primaries(html: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let doc = Html::parse_document(html);
    let dialogs: Vec<ElementRef<'_>> = doc
        .select(&selector("dialog"))
        .filter(|dialog| !concealed(*dialog))
        .collect();
    let mut page = Vec::new();
    let mut per_dialog = vec![Vec::new(); dialogs.len()];
    for primary in doc.select(&selector(&format!(".{PRIMARY}"))) {
        if concealed(primary) {
            continue;
        }
        let closest_dialog = primary
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|ancestor| ancestor.value().name() == "dialog");
        match closest_dialog {
            None => page.push(describe(primary)),
            Some(dialog) => {
                let index = dialogs
                    .iter()
                    .position(|seen| seen.id() == dialog.id())
                    .expect("a primary that is not concealed sits in a dialog that is not");
                per_dialog[index].push(describe(primary));
            }
        }
    }
    (page, per_dialog)
}

/// Every card-level surface (`.card`, `.stat`, `.empty`, `.chat-panes`,
/// `.data-table`) that sits inside a `.card`, in document order.
///
/// The one exception is a `.data-table` whose closest `.card` is
/// `.card--flush`: a flush card's body is the table, and the card absorbs the
/// table's frame. A `.data-table` deeper inside some other card under the
/// flush one is still reported, because its closest card is that other one.
///
/// Concealed elements are checked too. Nesting is a property of the markup,
/// and a hidden region that is revealed later shows the nesting it has.
pub fn card_level_inside_card(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    doc.root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter(|el| CARD_LEVEL.iter().any(|class| has_class(*el, class)))
        .filter(|el| {
            let Some(closest_card) = el
                .ancestors()
                .filter_map(ElementRef::wrap)
                .find(|ancestor| has_class(*ancestor, "card"))
            else {
                return false;
            };
            !(has_class(*el, "data-table") && has_class(closest_card, "card--flush"))
        })
        .map(describe)
        .collect()
}

/// The page's headings in document order, as `(level, text)`, with the text's
/// whitespace collapsed.
///
/// Headings that are concealed, or inside a `<dialog>`, are left out: a
/// dialog is its own context with its own heading, and it is not part of the
/// page's outline.
pub fn heading_outline(html: &str) -> Vec<(u8, String)> {
    let doc = Html::parse_document(html);
    doc.select(&selector("h1, h2, h3, h4, h5, h6"))
        .filter(|heading| !concealed(*heading))
        .filter(|heading| !self_or_ancestor(*heading, |el| el.value().name() == "dialog"))
        .map(|heading| {
            let level = heading.value().name()[1..]
                .parse()
                .expect("the selector matches h1 to h6 only");
            (level, text_of(heading))
        })
        .collect()
}

/// The page header's status and actions (`ui::shell::Topbar`), in order, as
/// one line each: `status "SQLite · 9 tables"`, `secondary "Refresh"`,
/// `primary "New bucket"`, with ` hidden` after one the markup conceals.
/// An item that is neither a badge nor a secondary or primary `.btn` is
/// reported by [`describe`], so a guard sees it rather than skips it.
pub fn header_items(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    doc.select(&selector("header.topbar > .topbar__actions > *"))
        .map(|item| {
            let kind = if has_class(item, "badge") {
                "status"
            } else if has_class(item, "btn") && has_class(item, PRIMARY) {
                "primary"
            } else if has_class(item, "btn") && has_class(item, "btn--secondary") {
                "secondary"
            } else {
                return describe(item);
            };
            let hidden = if concealed(item) { " hidden" } else { "" };
            format!("{kind} {:?}{hidden}", text_of(item))
        })
        .collect()
}

/// Every tab's label (`.tabs > .tab`: a `tab_navigation` bar, a `subnav`,
/// the legal editor's Edit / Preview), in document order: `Ok(text)` for a
/// tab that is its text alone, `Err` naming the tab by [`describe`] when it
/// holds an element (an icon, a wrapper), which a tab never does.
pub fn tab_labels(html: &str) -> Vec<Result<String, String>> {
    let doc = Html::parse_document(html);
    doc.select(&selector(".tabs > .tab"))
        .map(|tab| {
            if tab.children().any(|child| child.value().is_element()) {
                Err(describe(tab))
            } else {
                Ok(text_of(tab))
            }
        })
        .collect()
}

/// The labels a page writes for a reader, in document order, as
/// `(kind, phrase)`: headings (`h1`…`h6`), table headers (`th`), form labels
/// (`label`, `legend`), buttons (`button`, `.btn`), disclosure summaries
/// (`summary`), stat labels (`.stat__label`), badges (`.badge`) and tabs
/// (`.tab`).
///
/// Each text node is its own phrase, whitespace collapsed: a checkbox card's
/// `label` holds its name and the sentence under it (`strong` "Automatic
/// tax", `small` "Ask Stripe to calculate tax…"), and a wizard step badge
/// its number and its name, and each reads on its own. Concealed labels are
/// included: a region revealed later shows the labels it has. An element
/// with no text (an icon button) has no phrase. The sidebar (`.sidebar`) is
/// left out: it is outside the redesign. So are select options, which are
/// the values a reader picks (countries, currencies, model ids), not labels
/// the page writes about them.
pub fn ui_labels(html: &str) -> Vec<(&'static str, String)> {
    const KINDS: [(&str, &str); 8] = [
        ("h1, h2, h3, h4, h5, h6", "heading"),
        ("th", "table header"),
        ("label, legend", "label"),
        ("button, .btn", "button"),
        ("summary", "summary"),
        (".stat__label", "stat label"),
        (".badge", "badge"),
        (".tab", "tab"),
    ];
    let doc = Html::parse_document(html);
    let kinds: Vec<(Selector, &'static str)> = KINDS
        .iter()
        .map(|(css, kind)| (selector(css), *kind))
        .collect();
    let mut out = Vec::new();
    for el in doc
        .root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
    {
        let Some((_, kind)) = kinds.iter().find(|(sel, _)| sel.matches(&el)) else {
            continue;
        };
        if self_or_ancestor(el, |el| has_class(el, "sidebar")) {
            continue;
        }
        for node in el.descendants() {
            if let Node::Text(text) = node.value() {
                let phrase = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !phrase.is_empty() {
                    out.push((*kind, phrase));
                }
            }
        }
    }
    out
}

/// Every way the page departs from the one confirm mechanism
/// (`ui::components::confirm_dialog`, asked through by chrome.js), as one line
/// each:
///
/// - a whole document that loads chrome.js (which asks every confirm), or
///   that holds an `hx-confirm`, and does not carry the shared dialog exactly
///   once; and the shared dialog as anything but an `alertdialog`. A document
///   with neither (`templates::public_page`) asks nothing, so needs none;
/// - any other `<dialog>` holding a `.btn--danger`, the class a destructive
///   confirm's button is drawn in: that is a block's own confirm dialog, a
///   second mechanism beside the shared one;
/// - an inline `<script>` that reaches the browser's own `confirm`
///   ([`browser_confirm_references`]): a script embedded in a page is held
///   to the rule the shipped script files are;
/// - an element with `hx-confirm` that does not name its action
///   (`data-confirm-label`, not empty) and its tone (`data-confirm-tone`,
///   `danger` or `primary`), without which chrome.js refuses to ask.
///
/// A fragment (an htmx answer) is held to the last two only: the document it
/// lands in carries the dialog.
pub fn confirm_problems(html: &str) -> Vec<String> {
    use crate::ui::components::CONFIRM_DIALOG_ID;

    let doc = Html::parse_document(html);
    let mut problems = Vec::new();
    let shared: Vec<ElementRef<'_>> = doc
        .select(&selector("dialog"))
        .filter(|dialog| dialog.value().id() == Some(CONFIRM_DIALOG_ID))
        .collect();
    let asks = html.contains(&crate::ui::assets::chrome_js_url())
        || doc.select(&selector("[hx-confirm]")).next().is_some();
    if crate::ui::layout::is_document(html) && asks && shared.len() != 1 {
        problems.push(format!(
            "the document carries {} dialog#{CONFIRM_DIALOG_ID}, not one",
            shared.len()
        ));
    }
    for dialog in &shared {
        if dialog.value().attr("role") != Some("alertdialog") {
            problems.push(format!(
                "dialog#{CONFIRM_DIALOG_ID} is not an alertdialog: {}",
                describe(*dialog)
            ));
        }
    }
    for dialog in doc.select(&selector("dialog")) {
        if dialog.value().id() == Some(CONFIRM_DIALOG_ID) {
            continue;
        }
        for danger in dialog.select(&selector(".btn--danger")) {
            problems.push(format!(
                "dialog#{} holds its own destructive confirm, {}; confirm through the \
                 shared dialog instead",
                dialog.value().id().unwrap_or_default(),
                describe(danger)
            ));
        }
    }
    for script in doc.select(&selector("script:not([src])")) {
        let text: String = script.text().collect();
        for at in browser_confirm_references(&text) {
            problems.push(format!(
                "an inline <script> refers to the browser's confirm, at its line {at} \
                 (ask in the shared dialog: window.impresspressConfirm)"
            ));
        }
    }
    for asker in doc.select(&selector("[hx-confirm]")) {
        let label = asker.value().attr("data-confirm-label").unwrap_or_default();
        let tone = asker.value().attr("data-confirm-tone").unwrap_or_default();
        if label.trim().is_empty() || !matches!(tone, "danger" | "primary") {
            problems.push(format!(
                "{} asks {:?} without a data-confirm-label and a data-confirm-tone of \
                 danger or primary (label {label:?}, tone {tone:?})",
                describe(asker),
                asker.value().attr("hx-confirm").unwrap_or_default()
            ));
        }
    }
    problems
}

/// Every place script `src` reaches the browser's own `confirm`, as
/// `line: text`. Not only a call: any reference to a global's `confirm` —
/// `window`, `self`, `globalThis`, `top` or `parent`, then `.confirm` (or
/// `?.confirm`) or `["confirm"]` with any quote — whether it is called there,
/// aliased (`var ask = window.confirm`) or called through `.call`. And a bare
/// `confirm(`, which is the same global. Words that only contain the name —
/// `impresspressConfirm(`, `htmx:confirm`, `data-confirm-tone`,
/// `confirm-accept` — are not references to it.
///
/// Prose in a script comment is held to it too: a comment that names the
/// browser's confirm writes it as "the browser's confirm dialog".
pub fn browser_confirm_references(src: &str) -> Vec<String> {
    static GLOBAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?x)
            (?:^|[^\w$.])(?:window|self|globalThis|top|parent)\s*
            (?: (?:\?\.|\.)\s*confirm(?:[^\w$]|$)
              | (?:\?\.)?\s*\[\s*["'`]confirm["'`]\s*\] )
            | (?:^|[^\w$])confirm\s*\(
            "#,
        )
        .expect("a valid pattern")
    });
    src.lines()
        .enumerate()
        .filter(|(_, line)| GLOBAL.is_match(line))
        .map(|(n, line)| format!("{}: {}", n + 1, line.trim()))
        .collect()
}

/// How a guard names an element: `button.btn.btn--primary "Save"`.
pub fn describe(el: ElementRef<'_>) -> String {
    let mut out = el.value().name().to_string();
    // The attribute's own order, not `classes()`, which sorts.
    for class in el.value().attr("class").unwrap_or("").split_whitespace() {
        out.push('.');
        out.push_str(class);
    }
    let text = text_of(el);
    if !text.is_empty() {
        out.push_str(&format!(" {text:?}"));
    }
    out
}

/// Whether the element, or an ancestor, is a `<template>` or `[hidden]`.
fn concealed(el: ElementRef<'_>) -> bool {
    self_or_ancestor(el, |el| {
        el.value().name() == "template" || el.value().attr("hidden").is_some()
    })
}

/// Whether `test` holds for the element or any element above it.
fn self_or_ancestor(el: ElementRef<'_>, test: impl Fn(ElementRef<'_>) -> bool) -> bool {
    test(el) || el.ancestors().filter_map(ElementRef::wrap).any(test)
}

/// Whether the element's `class` attribute lists `class` as one of its
/// tokens (`card` does not match `card__body`).
fn has_class(el: ElementRef<'_>, class: &str) -> bool {
    el.value().classes().any(|c| c == class)
}

/// The element's text, with every run of whitespace collapsed to one space.
fn text_of(el: ElementRef<'_>) -> String {
    el.descendants()
        .filter_map(|node| match node.value() {
            Node::Text(text) => Some(&**text),
            _ => None,
        })
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("the module's selectors are valid CSS")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btn(label: &str) -> String {
        format!(r#"<button class="btn btn--primary">{label}</button>"#)
    }

    fn primary(label: &str) -> String {
        format!("button.btn.btn--primary {label:?}")
    }

    // ── tab_labels ───────────────────────────────────────────────────────

    #[test]
    fn tab_labels_report_a_tab_that_holds_an_element() {
        let html = concat!(
            r#"<div class="tabs"><a class="tab active" href="/a">API  keys</a>"#,
            r#"<button class="tab" role="tab">Preview</button>"#,
            r#"<a class="tab" href="/c"><svg></svg> Roles</a></div>"#,
            r#"<a class="tab" href="/d">Outside a strip</a>"#,
        );
        assert_eq!(
            tab_labels(html),
            [
                Ok("API keys".to_string()),
                Ok("Preview".to_string()),
                Err(r#"a.tab "Roles""#.to_string()),
            ]
        );
    }

    // ── header_items ─────────────────────────────────────────────────────

    #[test]
    fn header_items_name_each_slot_in_order() {
        let html = concat!(
            r#"<header class="topbar"><div class="topbar__heading"><h1>T</h1></div>"#,
            r#"<div class="topbar__actions">"#,
            r#"<span class="badge badge-info">SQLite · 3 tables</span>"#,
            r#"<a class="btn btn--secondary" href="/x">Open <span class="sr-only">(new tab)</span></a>"#,
            r#"<button class="btn btn--secondary" hidden>Save</button>"#,
            r#"<button class="btn btn--quiet">Quiet</button>"#,
            r#"<button class="btn btn--primary">Publish</button>"#,
            r#"</div></header><main><button class="btn btn--primary">Body</button></main>"#,
        );
        assert_eq!(
            header_items(html),
            [
                r#"status "SQLite · 3 tables""#,
                r#"secondary "Open (new tab)""#,
                r#"secondary "Save" hidden"#,
                r#"button.btn.btn--quiet "Quiet""#,
                r#"primary "Publish""#,
            ]
        );
        assert!(header_items("<main><p>no header</p></main>").is_empty());
    }

    // ── visible_primaries ────────────────────────────────────────────────

    #[test]
    fn primaries_on_the_page_are_listed_in_document_order() {
        let html = format!(
            r#"<header class="topbar">{}</header><main>{}<a class="btn btn--secondary">Cancel</a></main>"#,
            btn("Create"),
            btn("Save"),
        );
        let (page, per_dialog) = visible_primaries(&html);
        assert_eq!(page, vec![primary("Create"), primary("Save")]);
        assert!(per_dialog.is_empty());
    }

    #[test]
    fn a_link_styled_as_a_primary_counts() {
        let (page, _) = visible_primaries(r#"<a class="btn btn--primary" href="/x">New</a>"#);
        assert_eq!(page, vec![r#"a.btn.btn--primary "New""#.to_string()]);
    }

    #[test]
    fn a_class_that_only_starts_with_the_primary_class_is_not_a_primary() {
        let (page, _) = visible_primaries(r#"<span class="btn--primary-ish">x</span>"#);
        assert!(page.is_empty());
    }

    #[test]
    fn a_hidden_element_and_everything_inside_it_is_skipped() {
        let html = format!(
            r#"{}<div hidden><section>{}</section></div><button class="btn btn--primary" hidden>Own</button><div hidden="until-found">{}</div>"#,
            btn("Shown"),
            btn("Nested"),
            btn("Found"),
        );
        let (page, _) = visible_primaries(&html);
        assert_eq!(page, vec![primary("Shown")]);
    }

    #[test]
    fn template_contents_are_skipped() {
        let html = format!(
            "{}<template><tr><td>{}</td></tr></template><table><template>{}</template></table>",
            btn("Shown"),
            btn("Row"),
            btn("In a table"),
        );
        let (page, _) = visible_primaries(&html);
        assert_eq!(page, vec![primary("Shown")]);
    }

    #[test]
    fn each_dialog_is_its_own_group() {
        let html = format!(
            r#"{}<dialog id="a">{}{}</dialog><dialog id="b"><form>{}</form></dialog><dialog id="c"></dialog>"#,
            btn("Page"),
            btn("A1"),
            btn("A2"),
            btn("B1"),
        );
        let (page, per_dialog) = visible_primaries(&html);
        assert_eq!(page, vec![primary("Page")]);
        assert_eq!(
            per_dialog,
            vec![
                vec![primary("A1"), primary("A2")],
                vec![primary("B1")],
                vec![],
            ]
        );
    }

    #[test]
    fn a_hidden_dialog_or_one_in_a_template_is_not_a_group() {
        let html = format!(
            "<dialog hidden>{}</dialog><template><dialog>{}</dialog></template><dialog>{}</dialog>",
            btn("Hidden"),
            btn("Templated"),
            btn("Open"),
        );
        let (page, per_dialog) = visible_primaries(&html);
        assert!(page.is_empty());
        assert_eq!(per_dialog, vec![vec![primary("Open")]]);
    }

    #[test]
    fn a_primary_belongs_to_its_closest_dialog() {
        let html = format!(
            "<dialog>{}<dialog>{}</dialog></dialog>",
            btn("Outer"),
            btn("Inner")
        );
        let (page, per_dialog) = visible_primaries(&html);
        assert!(page.is_empty());
        assert_eq!(
            per_dialog,
            vec![vec![primary("Outer")], vec![primary("Inner")]]
        );
    }

    // ── card_level_inside_card ───────────────────────────────────────────

    #[test]
    fn surfaces_directly_on_the_page_are_fine() {
        let html = r#"<div class="card"><div class="card__body">x</div></div>
            <div class="stat">1</div><div class="empty">none</div>
            <div class="chat-panes"></div><table class="data-table"></table>"#;
        assert!(card_level_inside_card(html).is_empty());
    }

    #[test]
    fn every_card_level_surface_inside_a_card_is_reported() {
        let html = r#"<section class="card"><div class="card__body">
            <div class="card">inner</div>
            <div class="stat">1</div>
            <div class="empty">none</div>
            <div class="chat-panes">c</div>
            <table class="data-table"><tr><td>t</td></tr></table>
        </div></section>"#;
        assert_eq!(
            card_level_inside_card(html),
            vec![
                r#"div.card "inner""#,
                r#"div.stat "1""#,
                r#"div.empty "none""#,
                r#"div.chat-panes "c""#,
                r#"table.data-table "t""#,
            ]
        );
    }

    #[test]
    fn card_parts_and_lookalike_classes_are_not_surfaces() {
        let html = r#"<div class="card"><div class="card__body"><div class="card__head">h</div>
            <div class="stat-row"></div><p class="empty-note">none</p>
            <div class="data-table__row"></div></div></div>"#;
        assert!(card_level_inside_card(html).is_empty());
    }

    #[test]
    fn a_lookalike_ancestor_is_not_a_card() {
        let html = r#"<div class="card__body"><div class="stat">1</div></div>
            <div class="cards"><div class="empty">e</div></div>"#;
        assert!(card_level_inside_card(html).is_empty());
    }

    #[test]
    fn a_table_whose_closest_card_is_flush_is_allowed() {
        // The shape `Card::flush()` renders: the table inside `.card__body`,
        // so the flush card is the table's closest card, not its parent.
        let html = r#"<div class="card card--flush"><div class="card__body">
            <table class="data-table"></table></div></div>"#;
        assert!(card_level_inside_card(html).is_empty());
    }

    #[test]
    fn the_flush_exception_covers_only_a_table() {
        let html = r#"<div class="card card--flush"><div class="card__body">
            <div class="empty">e</div></div></div>"#;
        assert_eq!(card_level_inside_card(html), vec![r#"div.empty "e""#]);
    }

    #[test]
    fn the_flush_exception_is_judged_by_the_closest_card() {
        // A table in a plain card inside a flush card: the plain card is the
        // closest, so the table is reported (and so is the plain card).
        let html = r#"<div class="card card--flush"><div class="card__body"><div class="card">
            <table class="data-table"><tr><td>t</td></tr></table></div></div></div>"#;
        assert_eq!(
            card_level_inside_card(html),
            vec![r#"div.card "t""#, r#"table.data-table "t""#]
        );
        // A table in a flush card inside a plain card: the table is allowed,
        // the flush card itself is not.
        let html = r#"<div class="card"><div class="card__body"><div class="card card--flush">
            <div class="card__body"><table class="data-table"></table></div></div></div></div>"#;
        assert_eq!(card_level_inside_card(html), vec!["div.card.card--flush"]);
    }

    #[test]
    fn nesting_inside_a_hidden_region_is_still_reported() {
        let html = r#"<div class="card"><div hidden><div class="stat">1</div></div></div>"#;
        assert_eq!(card_level_inside_card(html), vec![r#"div.stat "1""#]);
    }

    // ── heading_outline ──────────────────────────────────────────────────

    #[test]
    fn headings_come_back_in_document_order_with_their_levels() {
        let html = r#"<header><h1 class="topbar__title">Users</h1></header>
            <main><h2>All   users</h2><section><h3>Filters <small>(2)</small></h3></section>
            <h6>Six</h6></main>"#;
        assert_eq!(
            heading_outline(html),
            vec![
                (1, "Users".to_string()),
                (2, "All users".to_string()),
                (3, "Filters (2)".to_string()),
                (6, "Six".to_string()),
            ]
        );
    }

    #[test]
    fn hidden_template_and_dialog_headings_are_left_out() {
        let html = r#"<h1>Page</h1>
            <div hidden><h2>Hidden</h2></div>
            <h2 hidden>Own hidden</h2>
            <template><h2>Templated</h2></template>
            <dialog><h2>Dialog</h2><div><h3>Deep</h3></div></dialog>
            <h2>Shown</h2>"#;
        assert_eq!(
            heading_outline(html),
            vec![(1, "Page".to_string()), (2, "Shown".to_string())]
        );
    }

    /// A guard hands over the whole document the renderer produced, head,
    /// inline scripts and all, so the fixture is one: `layout::page` around a
    /// body, not a hand-written imitation of its doctype and head.
    #[test]
    fn a_whole_rendered_document_is_walked() {
        let config = crate::ui::SiteConfig {
            app_name: "TestApp".to_string(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: String::new(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
            auth_headline: String::new(),
            auth_tagline: String::new(),
        };
        let body = maud::html! {
            h1 { "Title" }
            dialog { button class="btn btn--primary" { "Ok" } }
        };
        let html = crate::ui::layout::page("Title", &config, body).into_string();
        assert_eq!(heading_outline(&html), vec![(1, "Title".to_string())]);
        let (page, per_dialog) = visible_primaries(&html);
        assert!(page.is_empty());
        // The page's own dialog, then the shared confirm dialog every
        // document carries, whose confirm buttons are hidden until it asks.
        assert_eq!(per_dialog, vec![vec![primary("Ok")], vec![]]);
        assert_eq!(confirm_problems(&html), Vec::<String>::new());
    }

    // ── confirm_problems ─────────────────────────────────────────────────

    #[test]
    fn a_confirm_must_name_its_action_and_tone() {
        let html = concat!(
            r#"<button hx-delete="/a" hx-confirm="Delete a?" data-confirm-label="Delete a" data-confirm-tone="danger">A</button>"#,
            r#"<button hx-delete="/b" hx-confirm="Delete b?">B</button>"#,
            r#"<button hx-post="/c" hx-confirm="Load c?" data-confirm-label=" " data-confirm-tone="primary">C</button>"#,
            r#"<button hx-post="/d" hx-confirm="Load d?" data-confirm-label="Load d" data-confirm-tone="ok">D</button>"#,
        );
        let problems = confirm_problems(html);
        assert_eq!(problems.len(), 3, "{problems:#?}");
        for (at, name) in [(0, "\"B\""), (1, "\"C\""), (2, "\"D\"")] {
            assert!(
                problems[at].starts_with(&format!("button {name} asks")),
                "{problems:#?}"
            );
        }
    }

    #[test]
    fn a_dialog_of_its_own_with_a_danger_button_is_a_second_mechanism() {
        let html = concat!(
            r#"<dialog id="delete-confirm"><p></p>"#,
            r#"<button class="btn btn--secondary">Cancel</button>"#,
            r#"<button class="btn btn--danger" data-confirm>Delete</button></dialog>"#,
            r#"<dialog id="edit"><button class="btn btn--primary">Save</button></dialog>"#,
        );
        assert_eq!(
            confirm_problems(html),
            vec![
                r#"dialog#delete-confirm holds its own destructive confirm, button.btn.btn--danger "Delete"; confirm through the shared dialog instead"#
            ]
        );
    }

    #[test]
    fn a_document_carries_the_shared_dialog_once_as_an_alertdialog() {
        let shared = crate::ui::components::confirm_dialog().into_string();
        // A whole document, spelled with maud's own doctype so this file
        // writes no page chrome of its own (`scripts/grep-guard-html.sh`).
        let document = |head: &str, body: &str| {
            format!("{}<head>{head}</head><body>{body}</body>", maud::DOCTYPE.0)
        };
        let chrome = format!(
            r#"<script src="{}" defer></script>"#,
            crate::ui::assets::chrome_js_url()
        );
        let with_chrome = |body: &str| document(&chrome, body);
        assert!(confirm_problems(&with_chrome(&shared)).is_empty());
        assert_eq!(
            confirm_problems(&with_chrome("<p>x</p>")),
            vec!["the document carries 0 dialog#confirm-dialog, not one"]
        );
        assert_eq!(
            confirm_problems(&with_chrome(&format!("{shared}{shared}"))),
            vec!["the document carries 2 dialog#confirm-dialog, not one"]
        );
        assert_eq!(
            confirm_problems(&with_chrome(r#"<dialog id="confirm-dialog"></dialog>"#)),
            vec!["dialog#confirm-dialog is not an alertdialog: dialog"]
        );
        // A fragment lands in a document that carries the dialog, and a
        // document that loads no chrome.js and holds no `hx-confirm` (a
        // public page) asks nothing.
        assert!(confirm_problems("<tr><td>x</td></tr>").is_empty());
        assert!(confirm_problems(&document("", "<p>x</p>")).is_empty());
        assert_eq!(
            confirm_problems(&document(
                "",
                r#"<button hx-delete="/x" hx-confirm="X?" data-confirm-label="Delete x" data-confirm-tone="danger">X</button>"#
            )),
            vec!["the document carries 0 dialog#confirm-dialog, not one"]
        );
    }

    // ── browser_confirm_references ────────────────────────────────────

    #[test]
    fn every_spelling_of_the_browser_confirm_is_a_reference() {
        for spelling in [
            "if (!window.confirm('x')) return;",
            "window.confirm ('x')",
            "window . confirm('x')",
            r#"if (!window["confirm"]("x")) return;"#,
            "window['confirm']('x')",
            "window[`confirm`]('x')",
            "window[ 'confirm' ]",
            "var ask = window.confirm; ask('y');",
            "const ask = window.confirm",
            "window.confirm.call(window, 'x')",
            "window.confirm.bind(window)",
            "window?.confirm?.('x')",
            "self.confirm('x')",
            "globalThis.confirm('x')",
            "top.confirm('x')",
            "parent.confirm('x')",
            "confirm('x')",
            "if(!confirm('x'))return;",
            "confirm ('y');",
        ] {
            assert_eq!(
                browser_confirm_references(spelling),
                vec![format!("1: {spelling}")],
                "{spelling}"
            );
        }
    }

    #[test]
    fn a_name_that_only_contains_confirm_is_not_a_reference() {
        for text in [
            "await window.impresspressConfirm({question: 'x'})",
            "askToConfirm(a)",
            "confirmed(b)",
            r#"document.body.addEventListener("htmx:confirm", f)"#,
            r#"b.getAttribute("data-confirm-tone")"#,
            r#"'[data-action="confirm-accept"]'"#,
            "window.confirmDialog = d",
            "myWindow.confirm",
            "// asked in the shared dialog, never the browser's confirm dialog",
        ] {
            assert!(browser_confirm_references(text).is_empty(), "{text}");
        }
        assert_eq!(
            browser_confirm_references("a();\nb();\n  window.confirm('x');"),
            vec!["3: window.confirm('x');"]
        );
    }

    #[test]
    fn an_inline_script_that_reaches_the_browser_confirm_is_a_problem() {
        let html = concat!(
            r#"<script src="/b/static/x.js"></script>"#,
            r#"<script>var ok = 1;</script>"#,
            r#"<script>function f(){ if (!window["confirm"]("Really?")) return; }</script>"#,
        );
        assert_eq!(
            confirm_problems(html),
            vec![
                r#"an inline <script> refers to the browser's confirm, at its line 1: function f(){ if (!window["confirm"]("Really?")) return; } (ask in the shared dialog: window.impresspressConfirm)"#
            ]
        );
    }

    // ── describe ─────────────────────────────────────────────────────────

    #[test]
    fn an_element_is_described_by_its_tag_classes_and_text() {
        let doc = Html::parse_fragment(
            r#"<a class="btn  btn--primary" href="/x"> New
            bucket </a><div class="card"></div>"#,
        );
        let described: Vec<String> = doc
            .root_element()
            .children()
            .filter_map(ElementRef::wrap)
            .map(describe)
            .collect();
        assert_eq!(
            described,
            vec![r#"a.btn.btn--primary "New bucket""#, "div.card"]
        );
    }
}
