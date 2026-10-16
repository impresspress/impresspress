//! Card

use maud::{html, Markup, PreEscaped};

use super::action::{Action, Emphasis};
use crate::ui::icons;

/// A card: one framed surface on the page background, holding one section of
/// a page's content.
///
/// This is the only writer of `.card` and `.card__*`. A card never holds
/// another card-level surface (a card, a stat, a framed empty state, a bare
/// data table), with one exception: a [`flush`](Card::flush) card's body may
/// hold a `DataTable`, whose frame the card absorbs.
///
/// - [`Card::titled`] renders `section.card > header.card__head + div.card__body`.
///   The title is an `h2`: the shell's topbar owns the page's one `h1`
///   (`ui::shell::render_topbar`), so a card is a section under it.
/// - [`Card::plain`] renders `div.card > div.card__body`, with no heading.
#[must_use = "a Card renders nothing until `render` is called"]
pub struct Card<'a> {
    title: Option<&'a str>,
    body: Markup,
    subtitle: Option<&'a str>,
    link: Option<(&'a str, &'a str)>,
    action: Option<Action<'a>>,
    flush: bool,
    warning: bool,
    id: Option<&'a str>,
    hidden: bool,
    data: Vec<(&'static str, &'a str)>,
}

impl<'a> Card<'a> {
    /// A card with an `h2` title in its head.
    pub fn titled(title: &'a str, body: Markup) -> Self {
        Self::new(Some(title), body)
    }

    /// A card with no head: the body is the whole card.
    pub fn plain(body: Markup) -> Self {
        Self::new(None, body)
    }

    fn new(title: Option<&'a str>, body: Markup) -> Self {
        Self {
            title,
            body,
            subtitle: None,
            link: None,
            action: None,
            flush: false,
            warning: false,
            id: None,
            hidden: false,
            data: Vec::new(),
        }
    }

    /// One line under the title. Needs a [`titled`](Card::titled) card.
    pub fn subtitle(mut self, text: &'a str) -> Self {
        self.subtitle = Some(text);
        self
    }

    /// A text link at the end of the head ("View logs"), drawn with a trailing
    /// arrow. Needs a [`titled`](Card::titled) card.
    pub fn link(mut self, label: &'a str, href: &'a str) -> Self {
        self.link = Some((label, href));
        self
    }

    /// One action at the end of the head, after the [`link`](Card::link)
    /// if there is one ("Add grant", "Edit in variables"). Needs a
    /// [`titled`](Card::titled) card.
    ///
    /// Only a Secondary or Quiet [`Action`]: a card head is never where the
    /// page's one primary lives, nor where a destructive control goes.
    pub fn action(mut self, action: Action<'a>) -> Self {
        assert!(
            matches!(action.emphasis(), Emphasis::Secondary | Emphasis::Quiet),
            "a card head takes a Secondary or Quiet action, got {:?}",
            action.emphasis()
        );
        self.action = Some(action);
        self
    }

    /// The body runs edge to edge, so a `DataTable` in it loses its own frame
    /// and the card's frame is the table's. The only card that may hold a
    /// card-level surface, and only that one.
    pub fn flush(mut self) -> Self {
        self.flush = true;
        self
    }

    /// A warning-toned border, for a card that needs the reader's action (a
    /// restricted Stripe account).
    pub fn warning(mut self) -> Self {
        self.warning = true;
        self
    }

    pub fn id(mut self, id: &'a str) -> Self {
        self.id = Some(id);
        self
    }

    /// Render the card with the `hidden` attribute, for a card a script
    /// reveals.
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// A `data-{name}` attribute, for a card a script or a spec selects on.
    ///
    /// `name` is the part after `data-` (`.data("wizard-step", "2")` renders
    /// `data-wizard-step="2"`), and must be a valid custom data attribute
    /// name: lowercase ASCII letters, digits and inner hyphens, without the
    /// `data-` prefix. Anything that needs an attribute other than `id`,
    /// `hidden` or a `data-*` one is not a card.
    pub fn data(mut self, name: &'static str, value: &'a str) -> Self {
        assert!(
            is_data_name(name),
            "Card::data takes the part after `data-`, in lowercase ASCII letters, digits and \
             inner hyphens; got {name:?}"
        );
        self.data.push((name, value));
        self
    }

    pub fn render(self) -> Markup {
        assert!(
            self.title.is_some()
                || (self.subtitle.is_none() && self.link.is_none() && self.action.is_none()),
            "a subtitle, a head link or a head action needs a titled card"
        );
        let class = match (self.flush, self.warning) {
            (false, false) => "card",
            (true, false) => "card card--flush",
            (false, true) => "card card--warning",
            (true, true) => "card card--flush card--warning",
        };
        let body = html! { div .card__body { (self.body) } };
        let card = match self.title {
            Some(title) => html! {
                section class=(class) id=[self.id] hidden[self.hidden] {
                    header .card__head {
                        div {
                            h2 .card__title { (title) }
                            @if let Some(subtitle) = self.subtitle {
                                p .card__subtitle { (subtitle) }
                            }
                        }
                        @if self.link.is_some() || self.action.is_some() {
                            div .card__actions {
                                @if let Some((label, href)) = self.link {
                                    a .card__link href=(href) { (label) (icons::arrow_right()) }
                                }
                                @if let Some(action) = self.action {
                                    (action.render())
                                }
                            }
                        }
                    }
                    (body)
                }
            },
            None => html! {
                div class=(class) id=[self.id] hidden[self.hidden] { (body) }
            },
        };
        with_data_attributes(card, &self.data)
    }
}

/// `name` is the tail of a valid `data-*` attribute name: non-empty,
/// lowercase ASCII letters, digits and hyphens, not starting or ending with a
/// hyphen. (HTML forbids uppercase there; the tighter alphabet keeps every
/// name a literal a spec can select on.) A tail that itself starts `data-`
/// is the full name passed by mistake, and would render `data-data-…`.
///
/// Shared with [`super::action::Action::data`], the other builder that takes
/// caller-named `data-*` attributes.
pub(super) fn is_data_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with("data-")
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Insert the `data-*` attributes into the opening tag of `element` (a card,
/// or an [`super::action::Action`]'s `a` / `button`).
///
/// maud spells every attribute name as a literal, so a name chosen by the
/// caller cannot be written inside `html!`. The rest of the element stays in
/// `html!` (where the stylesheet guards read its classes); this adds the
/// attributes after the tag name, which is the first run of ASCII letters
/// after the `<`. Each name passed [`is_data_name`] and each value is
/// escaped, so the result is as well-formed as maud's own output.
pub(super) fn with_data_attributes(element: Markup, data: &[(&'static str, &str)]) -> Markup {
    if data.is_empty() {
        return element;
    }
    let html = element.into_string();
    let tag_end = 1 + html[1..]
        .find(|c: char| !c.is_ascii_alphabetic())
        .expect("the element's opening tag has attributes");
    let mut attrs = String::new();
    for (name, value) in data {
        attrs.push_str(" data-");
        attrs.push_str(name);
        attrs.push_str("=\"");
        attrs.push_str(&html! { (value) }.into_string());
        attrs.push('"');
    }
    PreEscaped(format!("{}{attrs}{}", &html[..tag_end], &html[tag_end..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{test_support::dom, ui::components::ActionTarget};

    fn table() -> Markup {
        html! { div .data-table { table { tr { td { "row" } } } } }
    }

    #[test]
    fn a_titled_card_is_a_section_with_an_h2_head_and_a_body() {
        let s = Card::titled("Recent users", html! { p { "rows" } })
            .render()
            .into_string();
        assert_eq!(
            s,
            r#"<section class="card"><header class="card__head"><div><h2 class="card__title">Recent users</h2></div></header><div class="card__body"><p>rows</p></div></section>"#
        );
        assert_eq!(
            dom::heading_outline(&s),
            vec![(2, "Recent users".to_string())]
        );
    }

    #[test]
    fn a_plain_card_has_no_head() {
        let s = Card::plain(html! { "body" }).render().into_string();
        assert_eq!(
            s,
            r#"<div class="card"><div class="card__body">body</div></div>"#
        );
        assert!(dom::heading_outline(&s).is_empty());
    }

    #[test]
    fn the_subtitle_sits_under_the_title_and_the_link_ends_the_head() {
        let s = Card::titled("New users", html! {})
            .subtitle("Last 30 days")
            .link("View users", "/b/admin/users")
            .render()
            .into_string();
        assert!(
            s.contains(
                r#"<div><h2 class="card__title">New users</h2><p class="card__subtitle">Last 30 days</p></div><div class="card__actions"><a class="card__link" href="/b/admin/users">View users<svg"#
            ),
            "{s}"
        );
    }

    #[test]
    fn the_head_action_ends_the_head_after_the_link() {
        let s = Card::titled("Custom grants", html! {})
            .link("Docs", "/docs")
            .action(
                Action::secondary("Add grant", ActionTarget::Modal("add-grant"))
                    .icon(icons::plus()),
            )
            .render()
            .into_string();
        assert!(
            s.contains(r#"<div class="card__actions"><a class="card__link" href="/docs">Docs<svg"#),
            "{s}"
        );
        assert!(
            s.contains(
                r#"</a><button class="btn btn--secondary" type="button" data-action="modal-open" data-modal-target="add-grant"><svg"#
            ),
            "{s}"
        );
        assert!(
            s.contains(r#"</svg>Add grant</button></div></header><div class="card__body">"#),
            "{s}"
        );

        let alone = Card::titled("Defaults", html! {})
            .action(Action::quiet("Edit in variables", ActionTarget::Link("/v")))
            .render()
            .into_string();
        assert!(
            alone.contains(
                r#"</div><div class="card__actions"><a class="btn btn--ghost" href="/v">Edit in variables</a></div></header>"#
            ),
            "{alone}"
        );
        // A card head never holds the page's primary.
        assert!(dom::visible_primaries(&s).0.is_empty());
    }

    #[test]
    fn a_card_head_refuses_a_primary_or_a_destructive_action() {
        for action in [
            Action::primary("Save", ActionTarget::Submit { form: None }),
            Action::danger("Delete", ActionTarget::Link("/")),
            Action::quiet_danger("Remove", ActionTarget::Link("/")),
        ] {
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = Card::titled("t", html! {}).action(action);
            }));
            assert!(refused.is_err());
        }
    }

    #[test]
    #[should_panic(expected = "a subtitle, a head link or a head action needs a titled card")]
    fn a_plain_card_takes_no_head_action() {
        let _ = Card::plain(html! {})
            .action(Action::secondary("x", ActionTarget::Link("/")))
            .render();
    }

    #[test]
    fn flush_and_warning_are_modifiers_on_the_card() {
        let both = Card::titled("t", html! {})
            .flush()
            .warning()
            .render()
            .into_string();
        assert!(
            both.starts_with(r#"<section class="card card--flush card--warning">"#),
            "{both}"
        );
        let flush = Card::plain(html! {}).flush().render().into_string();
        assert!(
            flush.starts_with(r#"<div class="card card--flush">"#),
            "{flush}"
        );
        let warning = Card::plain(html! {}).warning().render().into_string();
        assert!(
            warning.starts_with(r#"<div class="card card--warning">"#),
            "{warning}"
        );
    }

    #[test]
    fn id_hidden_and_data_attributes_land_on_the_card_element() {
        let s = Card::titled("Pricing", html! { "b" })
            .id("step-3")
            .hidden(true)
            .data("wizard-step", "3")
            .data("offer-url", r#"/p?a=1&b="2""#)
            .render()
            .into_string();
        assert!(
            s.starts_with(
                r#"<section data-wizard-step="3" data-offer-url="/p?a=1&amp;b=&quot;2&quot;" class="card" id="step-3" hidden>"#
            ),
            "{s}"
        );
        // Only the card element carries them.
        assert_eq!(s.matches("data-").count(), 2, "{s}");
        assert!(!Card::plain(html! {})
            .hidden(false)
            .render()
            .into_string()
            .contains("hidden"));
    }

    #[test]
    fn a_plain_card_can_be_hidden() {
        let s = Card::plain(html! { "b" })
            .hidden(true)
            .render()
            .into_string();
        assert_eq!(
            s,
            r#"<div class="card" hidden><div class="card__body">b</div></div>"#
        );
    }

    /// The flush card the phone-layout spec (`card-flush.spec.ts`) lays out
    /// in a real browser. The spec reads it from a fixture file so it runs
    /// against `Card`'s markup rather than a copy of it; this keeps the file
    /// what `Card` renders today. To refresh it after a deliberate markup
    /// change, write `flush_card_fixture()` to the path below.
    fn flush_card_fixture() -> String {
        let cols = [
            super::super::TableCol::new("Email").primary(),
            super::super::TableCol::new("Role"),
            super::super::TableCol::new("Created"),
        ];
        let rows = [
            ("admin@example.com", "Admin", "Oct 1"),
            ("bob@example.com", "User", "Oct 2"),
        ]
        .into_iter()
        .map(|(email, role, created)| {
            super::super::TableRow::new(vec![
                html! { (email) },
                html! { (role) },
                html! { (created) },
            ])
        })
        .collect();
        let table = super::super::DataTable::new(&cols).rows(rows).render();
        let card = Card::titled("Recent users", table)
            .flush()
            .link("View users", "/b/admin/users")
            .render();
        format!("{}\n", card.into_string())
    }

    #[test]
    fn the_flush_card_e2e_fixture_is_what_card_renders() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-web/tests/e2e/fixtures/card-flush.html"
        );
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk,
            flush_card_fixture(),
            "{path} is out of date; write the expected text above to it"
        );
    }

    /// The buttons and card heads `card-head-action.spec.ts` lays out in a
    /// real browser: one action of each emphasis (and a small one), a card
    /// whose head holds a subtitle and an action, and a card whose long
    /// title is followed by both a link and an action. The spec reads it
    /// from a fixture file; this keeps that file what the builders render.
    /// To refresh it after a deliberate markup change, write
    /// `card_head_fixture()` to the path below.
    fn card_head_fixture() -> String {
        let target = ActionTarget::Link("/b/admin/");
        let buttons = html! {
            div #buttons {
                (Action::primary("New bucket", ActionTarget::Modal("new-bucket")).icon(icons::plus()).render())
                (Action::secondary("Secondary", target).render())
                (Action::quiet("Quiet", target).render())
                (Action::quiet_danger("Quiet danger", target).render())
                (Action::danger("Danger", target).render())
                (Action::secondary("Enable", ActionTarget::Delegated("enable")).small().render())
            }
        };
        let grants = Card::titled("Custom grants", html! { p { "Grants an admin added." } })
            .subtitle("Added at runtime, on top of the grants declared in code")
            .action(
                Action::secondary("Add grant", ActionTarget::Modal("add-grant"))
                    .icon(icons::plus()),
            )
            .render();
        let defaults = Card::titled(
            "Defaults for every new conversation thread",
            html! { p { "Model and temperature." } },
        )
        .link("View documentation", "/b/admin/")
        .action(Action::quiet(
            "Edit in variables",
            ActionTarget::Link("/b/admin/"),
        ))
        .render();
        format!(
            "{}\n",
            html! { (buttons) (grants) (defaults) }.into_string()
        )
    }

    #[test]
    fn the_card_head_e2e_fixture_is_what_the_builders_render() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-web/tests/e2e/fixtures/card-head-action.html"
        );
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk,
            card_head_fixture(),
            "{path} is out of date; write the expected text above to it"
        );
    }

    #[test]
    fn data_attributes_on_a_plain_card_go_on_its_div() {
        let s = Card::plain(html! { "b" })
            .data("offer-card", "")
            .render()
            .into_string();
        assert_eq!(
            s,
            r#"<div data-offer-card="" class="card"><div class="card__body">b</div></div>"#
        );
    }

    #[test]
    #[should_panic(expected = "Card::data takes the part after `data-`")]
    fn data_names_are_the_tail_of_a_data_attribute() {
        let _ = Card::plain(html! {}).data("data-step", "1");
    }

    #[test]
    fn data_names_reject_everything_but_lowercase_letters_digits_and_inner_hyphens() {
        for good in ["step", "wizard-step", "offer-id", "x2"] {
            assert!(is_data_name(good), "{good}");
        }
        for bad in [
            "",
            "data-x",
            "-x",
            "x-",
            "Step",
            "a b",
            "a=b",
            "a\"",
            "on>",
            "wizard_step",
        ] {
            assert!(!is_data_name(bad), "{bad:?}");
        }
    }

    #[test]
    #[should_panic(expected = "a subtitle, a head link or a head action needs a titled card")]
    fn a_plain_card_takes_no_head_parts() {
        let _ = Card::plain(html! {}).subtitle("orphan").render();
    }

    /// Rule 2 of the layout, read off the element tree: a flush card holding
    /// a data table is the one allowed nesting, and the table must sit in the
    /// card's body, where `Card::flush` puts it.
    #[test]
    fn only_a_flush_card_may_hold_a_data_table() {
        let flush = Card::titled("Recent users", table())
            .flush()
            .render()
            .into_string();
        assert!(dom::card_level_inside_card(&flush).is_empty(), "{flush}");

        let framed = Card::titled("Recent users", table()).render().into_string();
        assert_eq!(
            dom::card_level_inside_card(&framed),
            vec![r#"div.data-table "row""#]
        );
    }

    #[test]
    fn a_card_inside_a_card_is_reported_even_when_the_outer_one_is_flush() {
        let inner = Card::plain(html! { "inner" }).render();
        let outer = Card::titled("Outer", inner).flush().render().into_string();
        assert_eq!(
            dom::card_level_inside_card(&outer),
            vec![r#"div.card "inner""#]
        );
    }
}
