//! Modal dialogs: one native `<dialog>`, opened with `showModal()`.
//!
//! Every modal in the tree is rendered here and driven by the modal section
//! of `ui/assets/chrome.js`. The native element is what supplies the parts a
//! hand-rolled overlay has to fake and usually gets wrong: the page behind an
//! open modal is inert (no click, no Tab, no screen-reader cursor reaches
//! it), the dialog sits in the top layer above every stacking context, and
//! Esc closes it. chrome.js adds what the element does not do on its own:
//! Tab wraps inside the dialog instead of escaping to the browser, focus goes
//! back to the control that opened it (or that control's replacement, when
//! the request the modal made re-rendered the page behind it), and a click on
//! the backdrop closes it.
//!
//! The markup is declared, never scripted: a control opens a modal with
//! `data-action="modal-open" data-modal-target="<id>"`, closes the one it sits
//! in with `data-action="modal-close"`, and a handler that answers with a
//! modal opens or closes it through the `openModal`/`closeModal` `HX-Trigger`
//! events ([`crate::ui::html_response_opening_modal`],
//! [`crate::ui::html_response_closing_modal`]).
//!
//! Layout: a header (the `h2` title the dialog is labelled by, and a labelled
//! 44px close button), then a body that scrolls on its own. A form's actions
//! go in [`modal_footer`], which sticks to the bottom of that scrolling body,
//! so the submit button is on screen however tall the form is — the dialog is
//! capped at the dynamic viewport height, which is what a phone's collapsing
//! address bar changes.

use maud::{html, Markup};

use crate::ui::icons;

/// How wide a modal may grow. Every modal is the full viewport width less a
/// gutter on a phone; this caps it on a wide screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModalSize {
    /// A form: 500px.
    #[default]
    Default,
    /// Reference content with tables in it (the block detail modal): 720px.
    Large,
}

/// A modal dialog, built up and then rendered with [`Modal::render`].
///
/// ```ignore
/// Modal::new("create-role", "Create role").render(html! {
///     form hx-post="/b/admin/iam/roles" hx-target="#iam-content" {
///         /* fields */
///         (modal_footer(html! {
///             (modal_cancel())
///             button .btn .btn--primary .btn--block type="submit" { "Create" }
///         }))
///     }
/// })
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Modal<'a> {
    id: &'a str,
    title: &'a str,
    size: ModalSize,
    focusable_body: bool,
}

impl<'a> Modal<'a> {
    /// A modal with the element id `id` (what `data-modal-target` and the
    /// `openModal`/`closeModal` triggers name) and the visible title `title`,
    /// which is also its accessible name.
    pub fn new(id: &'a str, title: &'a str) -> Self {
        Self {
            id,
            title,
            size: ModalSize::Default,
            focusable_body: false,
        }
    }

    /// Set the width cap.
    pub fn size(mut self, size: ModalSize) -> Self {
        self.size = size;
        self
    }

    /// Make the scrolling body a tab stop, for a modal of reference content
    /// that may hold no control of its own (the block detail modal for a core
    /// block). A form needs none: focusing a field scrolls it into view, but a
    /// body with nothing focusable in it could not be scrolled from the
    /// keyboard at all.
    pub fn focusable_body(mut self) -> Self {
        self.focusable_body = true;
        self
    }

    /// Render the closed dialog with `body` in its scrolling body.
    pub fn render(self, body: Markup) -> Markup {
        self.render_with_meta(html! {}, body)
    }

    /// [`Modal::render`] with `meta` beside the title — a version or category
    /// badge that belongs to the heading rather than the body.
    pub fn render_with_meta(self, meta: Markup, body: Markup) -> Markup {
        let title_id = format!("{}-title", self.id);
        let large = self.size == ModalSize::Large;
        html! {
            dialog .modal .modal--lg[large] id=(self.id) aria-labelledby=(title_id) {
                div .modal__header {
                    div .modal__heading {
                        h2 .modal__title id=(title_id) { (self.title) }
                        (meta)
                    }
                    button .modal__close type="button" data-action="modal-close" aria-label="Close" {
                        (icons::x())
                    }
                }
                div .modal__body
                    tabindex=[self.focusable_body.then_some("0")]
                    aria-labelledby=[self.focusable_body.then_some(title_id.as_str())]
                {
                    (body)
                }
            }
        }
    }
}

/// A modal with the default width: `Modal::new(id, title).render(body)`.
pub fn modal(id: &str, title: &str, body: Markup) -> Markup {
    Modal::new(id, title).render(body)
}

/// The action row of a modal's form, kept on screen at the bottom of the
/// modal's scrolling body. It belongs INSIDE the form, as its last child, so
/// the submit button submits it and an htmx re-render of the form (a field
/// error) re-renders the actions with it.
///
/// The buttons in it carry `.btn--block`, so on a phone they share the row in
/// full-width halves; on a wide screen they sit right-aligned at their own
/// width.
pub fn modal_footer(actions: Markup) -> Markup {
    html! {
        div .modal__footer { (actions) }
    }
}

/// The Cancel button every form modal has: it closes the modal it sits in.
pub fn modal_cancel() -> Markup {
    html! {
        button .btn .btn--secondary .btn--block type="button" data-action="modal-close" { "Cancel" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered() -> String {
        modal(
            "create-role",
            "Create role",
            html! {
                form {
                    input #role-name name="name";
                    (modal_footer(html! { (modal_cancel()) }))
                }
            },
        )
        .into_string()
    }

    #[test]
    fn a_modal_is_a_closed_native_dialog() {
        let html = rendered();
        assert!(
            html.starts_with(r#"<dialog class="modal" id="create-role""#),
            "{html}"
        );
        // Closed until chrome.js calls `showModal()`: no `open`, and no
        // `hidden` either — a `hidden` dialog would stay `display: none`
        // after it opened.
        assert!(!html.contains(" open"), "{html}");
        assert!(!html.contains("hidden"), "{html}");
    }

    #[test]
    fn the_dialog_is_named_by_its_h2_title() {
        let html = rendered();
        assert!(
            html.contains(r#"aria-labelledby="create-role-title""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<h2 class="modal__title" id="create-role-title">Create role</h2>"#),
            "{html}"
        );
    }

    #[test]
    fn the_close_button_is_labelled_and_closes_its_own_modal() {
        let html = rendered();
        assert!(
            html.contains(
                r#"<button class="modal__close" type="button" data-action="modal-close" aria-label="Close">"#
            ),
            "{html}"
        );
    }

    #[test]
    fn the_footer_sits_in_the_scrolling_body_with_a_block_cancel() {
        let html = rendered();
        let body = html.find(r#"<div class="modal__body">"#).expect("body");
        let footer = html.find(r#"<div class="modal__footer">"#).expect("footer");
        assert!(body < footer, "the footer is inside the body: {html}");
        assert!(
            html.contains(
                r#"<button class="btn btn--secondary btn--block" type="button" data-action="modal-close">Cancel</button>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn only_a_reference_modal_makes_its_body_a_tab_stop() {
        assert!(!rendered().contains("tabindex"));
        let html = Modal::new("block-detail", "auth")
            .focusable_body()
            .render(html! { p { "x" } })
            .into_string();
        assert!(
            html.contains(
                r#"<div class="modal__body" tabindex="0" aria-labelledby="block-detail-title">"#
            ),
            "{html}"
        );
    }

    #[test]
    fn the_large_size_adds_its_class_and_meta_sits_beside_the_title() {
        let html = Modal::new("block-detail", "auth")
            .size(ModalSize::Large)
            .render_with_meta(html! { small { "v1" } }, html! { p { "x" } })
            .into_string();
        assert!(
            html.starts_with(r#"<dialog class="modal modal--lg" id="block-detail""#),
            "{html}"
        );
        assert!(html.contains(r#"</h2><small>v1</small></div>"#), "{html}");
    }
}
