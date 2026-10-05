//! Callout: a boxed note inside a page's content.

use maud::{html, Markup};

/// A [`callout`]'s tone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalloutTone {
    /// Neutral information: a hint, a pointer to where something is set up,
    /// a state the page is in that is not a problem (a feature turned off).
    Info,
    /// Something is missing or will not work here (a backend not built in).
    Warning,
}

impl CalloutTone {
    fn class(self) -> &'static str {
        match self {
            CalloutTone::Info => "callout--info",
            CalloutTone::Warning => "callout--warning",
        }
    }
}

/// A boxed note in a page's content: a bold one-line `title`, a `body`
/// (usually one short paragraph) and optional `actions` (links or buttons)
/// beside it, which drop under the copy below 760px.
///
/// The tone colours the box, never the brand colour: brand red is kept for
/// primary actions, and a note that informs is not one. Not a live region —
/// it is part of the page as rendered, not an announcement; a form's
/// feedback is `components::alert`.
pub fn callout(tone: CalloutTone, title: &str, body: Markup, actions: Option<Markup>) -> Markup {
    html! {
        section class={ "callout " (tone.class()) } {
            div .callout__copy {
                strong .callout__title { (title) }
                div .callout__body { (body) }
            }
            @if let Some(actions) = actions {
                div .callout__actions { (actions) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use maud::html;

    use super::*;

    #[test]
    fn callout_renders_tone_title_body_and_actions() {
        let s = callout(
            CalloutTone::Info,
            "Seller products are turned off",
            html! { p { "New listings cannot be created." } },
            Some(html! { a .btn href="/x" { "Open settings" } }),
        )
        .into_string();
        assert!(
            s.starts_with(r#"<section class="callout callout--info"><div class="callout__copy"><strong class="callout__title">Seller products are turned off</strong>"#),
            "{s}"
        );
        assert!(
            s.contains(
                r#"<div class="callout__body"><p>New listings cannot be created.</p></div>"#
            ),
            "{s}"
        );
        assert!(
            s.contains(r#"<div class="callout__actions"><a class="btn" href="/x">"#),
            "{s}"
        );
    }

    #[test]
    fn callout_without_actions_has_no_actions_box() {
        let s = callout(CalloutTone::Warning, "T", html! { "b" }, None).into_string();
        assert!(s.contains("callout--warning"), "{s}");
        assert!(!s.contains("callout__actions"), "{s}");
    }
}
