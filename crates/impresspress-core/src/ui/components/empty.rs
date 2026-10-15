//! Empty states: what a page or a section shows when it has nothing yet.
//!
//! There are two, and the choice is the caller's, made by where the empty
//! state sits (layout rule 9):
//! - [`empty_state`] stands in for a page's (or a section's) content on the
//!   page background. It is a framed, card-level surface: a small icon, a
//!   title, one sentence and the action, if there is one.
//! - [`empty_note`] is the one inside a `Card`, where a second frame would be
//!   a card inside a card (rule 2): one muted sentence, unframed.
//!
//! These two are the only writers of `.empty` and `.empty-note`.

use maud::{html, Markup};

/// The framed "nothing here yet" surface on the page background: an icon, a
/// title, one sentence and an optional call to action. Never inside a `Card`;
/// use [`empty_note`] there.
///
/// The title is an `h2`. An empty state stands in for a page's (or a
/// section's) content directly under the topbar's `h1`, and an `h3` there
/// skipped a level (axe `heading-order`); inside a `section_header` section
/// it is a sibling `h2`, which is still a valid outline.
///
/// The action is the page's primary only when the header carries none; when
/// the header already offers it as the primary, pass it here as a secondary.
pub fn empty_state(icon: Markup, title: &str, body: &str, action: Option<Markup>) -> Markup {
    html! {
        div .empty {
            div .empty__icon aria-hidden="true" { (icon) }
            h2 .empty__title { (title) }
            p .empty__body { (body) }
            @if let Some(a) = action { div .empty__action { (a) } }
        }
    }
}

/// The one empty state allowed inside a `Card`: a single muted sentence,
/// with no frame, icon or heading, and optionally a link to where the thing
/// is made (`(label, href)`), after the sentence.
///
/// Inside a `Card::flush()` body, which has no padding of its own, the note
/// takes the card's inset (`card.css`), so it lines up with the card's head.
pub fn empty_note(text: &str, link: Option<(&str, &str)>) -> Markup {
    html! {
        p .empty-note {
            (text)
            @if let Some((label, href)) = link { " " a href=(href) { (label) } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::dom;

    #[test]
    fn title_is_an_h2_and_the_icon_is_decorative() {
        let s = empty_state(html! { svg {} }, "No buckets", "Create one.", None).into_string();
        assert_eq!(
            s,
            concat!(
                r#"<div class="empty"><div class="empty__icon" aria-hidden="true"><svg></svg></div>"#,
                r#"<h2 class="empty__title">No buckets</h2><p class="empty__body">Create one.</p></div>"#,
            )
        );
    }

    #[test]
    fn the_action_follows_the_body() {
        let s = empty_state(
            html! { svg {} },
            "No buckets",
            "Create one.",
            Some(html! { a .btn .btn--secondary href="/new" { "New bucket" } }),
        )
        .into_string();
        assert!(
            s.ends_with(concat!(
                r#"<p class="empty__body">Create one.</p>"#,
                r#"<div class="empty__action"><a class="btn btn--secondary" href="/new">New bucket</a></div></div>"#,
            )),
            "{s}"
        );
    }

    #[test]
    fn a_note_is_one_sentence_with_no_frame_icon_or_heading() {
        let s = empty_note("No server errors in the last 30 days.", None).into_string();
        assert_eq!(
            s,
            r#"<p class="empty-note">No server errors in the last 30 days.</p>"#
        );
        assert!(dom::heading_outline(&s).is_empty());
    }

    #[test]
    fn a_note_may_end_with_a_link() {
        let s = empty_note(
            "User products are turned off.",
            Some(("Go to settings", "/b/admin/settings/products")),
        )
        .into_string();
        assert_eq!(
            s,
            concat!(
                r#"<p class="empty-note">User products are turned off. "#,
                r#"<a href="/b/admin/settings/products">Go to settings</a></p>"#,
            )
        );
    }

    #[test]
    fn a_note_escapes_its_text_and_link() {
        let s = empty_note("<b>&</b>", Some(("a\"b", "/x?a=1&b=\"2\""))).into_string();
        assert_eq!(
            s,
            concat!(
                r#"<p class="empty-note">&lt;b&gt;&amp;&lt;/b&gt; "#,
                r#"<a href="/x?a=1&amp;b=&quot;2&quot;">a&quot;b</a></p>"#,
            )
        );
    }

    /// Rule 2: a note is not a card-level surface, so a card may hold one;
    /// the framed empty state is one, so a card may not.
    #[test]
    fn a_card_may_hold_a_note_but_not_an_empty_state() {
        let note = format!(
            r#"<section class="card"><div class="card__body">{}</div></section>"#,
            empty_note("Nothing yet.", None).into_string()
        );
        assert!(dom::card_level_inside_card(&note).is_empty(), "{note}");
        let framed = format!(
            r#"<section class="card"><div class="card__body">{}</div></section>"#,
            empty_state(html! {}, "Nothing yet", "Add one.", None).into_string()
        );
        assert_eq!(dom::card_level_inside_card(&framed).len(), 1, "{framed}");
    }

    /// The empty states the layout spec (`empty-states.spec.ts`) lays out in
    /// a real browser: a framed state with an action, one whose body is long
    /// enough to wrap at every width, and a note in place of a table in a
    /// titled and in a plain flush card. The spec reads them from a fixture
    /// file so it runs against this module's markup rather than a copy of
    /// it; this test keeps the file what the builders render today. To
    /// refresh it after a deliberate markup change, write
    /// `empty_states_fixture()` to the path below.
    fn empty_states_fixture() -> String {
        use super::super::{Card, DataTable, TableCol};
        use crate::ui::icons;
        const COLS: [TableCol<'static>; 2] =
            [TableCol::new("Email").primary(), TableCol::new("Created")];
        let framed = empty_state(
            icons::folder(),
            "No buckets yet",
            "Buckets hold uploaded files. Create one to start uploading.",
            Some(html! { a .btn .btn--secondary href="/b/storage/admin/buckets" { "New bucket" } }),
        );
        let long = DataTable::new(&COLS)
            .empty_state(
                "No seller templates are available",
                "Ask an administrator to allow at least one built-in product template. \
                 Seller templates decide which kinds of product a seller can list.",
                None,
            )
            .render();
        let titled = Card::titled(
            "Recent users",
            DataTable::new(&COLS)
                .empty_note("No users have signed up yet.")
                .render(),
        )
        .flush()
        .link("View users", "/b/admin/users")
        .render();
        let plain = Card::plain(
            DataTable::new(&COLS)
                .empty_note("No grants are declared in code.")
                .render(),
        )
        .flush()
        .render();
        [framed, long, titled, plain]
            .into_iter()
            .map(|m| format!("{}\n", m.into_string()))
            .collect()
    }

    #[test]
    fn the_empty_states_e2e_fixture_is_what_the_builders_render() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-web/tests/e2e/fixtures/empty-states.html"
        );
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk,
            empty_states_fixture(),
            "{path} is out of date; write `empty_states_fixture()` to it"
        );
    }
}
