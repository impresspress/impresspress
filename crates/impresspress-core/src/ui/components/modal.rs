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
//! close button held to the touch-target floor), then a body that scrolls on its own. A form's actions
//! go in [`modal_footer`], which sticks to the bottom of that scrolling body,
//! so the submit button is on screen however tall the form is — the dialog is
//! capped at the dynamic viewport height, which is what a phone's collapsing
//! address bar changes.

use maud::{html, Markup};

use super::{Action, ActionTarget};
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
///             (Action::primary("Create", ActionTarget::Submit { form: None }).render())
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
    described_by: Option<&'a str>,
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
            described_by: None,
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

    /// Make it an `alertdialog`: a dialog that interrupts to ask about what
    /// the reader just did, described by the element with id `description`
    /// (the question it asks). Only [`confirm_dialog`] is one.
    pub fn alert(mut self, description: &'a str) -> Self {
        self.described_by = Some(description);
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
            dialog .modal .modal--lg[large] id=(self.id)
                role=[self.described_by.map(|_| "alertdialog")]
                aria-labelledby=(title_id)
                aria-describedby=[self.described_by]
            {
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
/// On a phone its buttons share the row equally; on a wide screen they sit
/// right-aligned at their own width. The row owns that layout
/// (`.modal__footer` in `modal.css`), so a hand-written button and an
/// [`Action`](super::Action) take it alike.
pub fn modal_footer(actions: Markup) -> Markup {
    html! {
        div .modal__footer { (actions) }
    }
}

/// The Cancel button every form modal has: it closes the modal it sits in.
pub fn modal_cancel() -> Markup {
    html! {
        button .btn .btn--secondary type="button" data-action="modal-close" { "Cancel" }
    }
}

/// The id of the page's one confirm dialog, [`confirm_dialog`].
pub const CONFIRM_DIALOG_ID: &str = "confirm-dialog";

/// The one dialog every page asks "are you sure?" in. `ui::layout::page`
/// renders it once per document, and the confirm half of chrome.js's modal
/// section fills it in and opens it, for an htmx control's `hx-confirm` and
/// for a block script's `window.impresspressConfirm` alike.
///
/// The markup holds no wording of its own. chrome.js writes the action's
/// label ("Revoke session") into the title and the confirm button, and the
/// question into `#confirm-dialog-question`, as text. Cancel carries
/// `autofocus`, so it is what `showModal()` focuses: a stray Enter cancels.
///
/// There are two confirm buttons, both hidden until chrome.js shows the one
/// the action's tone names. Each is an [`Action`], so the class is written
/// here and never by a script, and the dialog shows exactly one: its own
/// primary.
///
/// The tone rule, for every `data-confirm-tone` and `impresspressConfirm`
/// caller:
/// - `danger` when this app cannot undo the action: data is deleted, access
///   is taken from someone (a session, a key, a share link, a linked
///   provider), or a Stripe object is archived;
/// - `primary` when the app offers its undo: Disable has Enable, Archive
///   product has Publish, Reset to environment holds until the restart,
///   Discover / Load / Unload models can be run again.
pub fn confirm_dialog() -> Markup {
    let question = format!("{CONFIRM_DIALOG_ID}-question");
    Modal::new(CONFIRM_DIALOG_ID, "Confirm")
        .alert(&question)
        .render(html! {
            p .confirm-dialog__question id=(question) {}
            (modal_footer(html! {
                (Action::secondary("Cancel", ActionTarget::Delegated("modal-close"))
                    .autofocus()
                    .render())
                (Action::danger("Confirm", ActionTarget::Delegated("confirm-accept"))
                    .data("confirm-tone", "danger")
                    .hidden(true)
                    .render())
                (Action::primary("Confirm", ActionTarget::Delegated("confirm-accept"))
                    .data("confirm-tone", "primary")
                    .hidden(true)
                    .render())
            }))
        })
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
    fn the_footer_sits_in_the_scrolling_body_with_its_cancel() {
        let html = rendered();
        let body = html.find(r#"<div class="modal__body">"#).expect("body");
        let footer = html.find(r#"<div class="modal__footer">"#).expect("footer");
        assert!(body < footer, "the footer is inside the body: {html}");
        assert!(
            html.contains(
                r#"<button class="btn btn--secondary" type="button" data-action="modal-close">Cancel</button>"#
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

    /// The shared confirm dialog is an `alertdialog` named by its title and
    /// described by its question, with Cancel focused and both confirm
    /// buttons hidden until a tone picks one. The page's one-primary rule
    /// therefore holds whatever the dialog asks: a closed or open dialog
    /// shows at most its own primary.
    #[test]
    fn the_confirm_dialog_asks_with_cancel_focused_and_one_confirm_per_tone() {
        let html = confirm_dialog().into_string();
        assert!(
            html.starts_with(
                r#"<dialog class="modal" id="confirm-dialog" role="alertdialog" aria-labelledby="confirm-dialog-title" aria-describedby="confirm-dialog-question">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<p class="confirm-dialog__question" id="confirm-dialog-question"></p>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<button class="btn btn--secondary" autofocus type="button" data-action="modal-close">Cancel</button>"#
            ),
            "{html}"
        );
        for (class, tone) in [("btn--danger", "danger"), ("btn--primary", "primary")] {
            assert!(
                html.contains(&format!(
                    r#"<button data-confirm-tone="{tone}" class="btn {class}" hidden type="button" data-action="confirm-accept">Confirm</button>"#
                )),
                "{html}"
            );
        }
        let (page, dialogs) = crate::test_support::dom::visible_primaries(&html);
        assert!(
            page.is_empty() && dialogs == vec![Vec::<String>::new()],
            "{html}"
        );
    }

    /// The scripts this repository ships: every `.js`, `.mjs` and `.ts`
    /// under `crates/` and `packages/`, less tests, dependencies, build
    /// output and vendored code. `htmx.min.js` is vendored htmx: its own
    /// `confirm(question)` runs only for an `hx-confirm` whose `htmx:confirm`
    /// event nobody prevented, and chrome.js prevents every one that asks.
    fn shipped_scripts() -> Vec<(String, String)> {
        const SKIP_DIRS: &[&str] = &[
            "test",
            "tests",
            "node_modules",
            "target",
            "pkg",
            "vendor",
            "dist",
        ];
        const SKIP_FILES: &[&str] = &["htmx.min.js"];
        fn visit(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("read dir") {
                let path = entry.expect("dir entry").path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if path.is_dir() {
                    if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                        visit(root, &path, out);
                    }
                } else if ["js", "mjs", "ts"]
                    .iter()
                    .any(|ext| path.extension().is_some_and(|e| e == *ext))
                    && !SKIP_FILES.contains(&name.as_str())
                {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, std::fs::read_to_string(&path).expect("read script")));
                }
            }
        }
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let mut out = Vec::new();
        for top in ["crates", "packages"] {
            visit(root, &root.join(top), &mut out);
        }
        out.sort();
        out
    }

    /// No script file the repository ships reaches the browser's own
    /// `confirm`, by any spelling `dom::browser_confirm_references` knows: a
    /// global's `.confirm` or `["confirm"]`, called or not, or a bare
    /// `confirm(`. Every confirm is the shared page-built dialog, asked
    /// through `window.impresspressConfirm` or an `hx-confirm` (chrome.js).
    /// Scripts a page carries inline are held to the same rule by
    /// `htmx_guard::every_page_confirms_through_the_one_shared_dialog`.
    #[test]
    fn no_shipped_script_refers_to_the_browser_confirm() {
        let scripts = shipped_scripts();
        assert!(
            scripts.len() > 40,
            "the walk reached {} scripts; it must reach every shipped one",
            scripts.len()
        );
        assert!(
            scripts
                .iter()
                .any(|(rel, _)| rel.ends_with("ui/assets/chrome.js")),
            "the walk must reach chrome.js"
        );
        let found: Vec<String> = scripts
            .iter()
            .flat_map(|(rel, src)| {
                crate::test_support::dom::browser_confirm_references(src)
                    .into_iter()
                    .map(move |at| format!("{rel}:{at}"))
            })
            .collect();
        assert!(
            found.is_empty(),
            "ask in the shared confirm dialog (window.impresspressConfirm), not the browser's \
             confirm:\n{}",
            found.join("\n")
        );
    }

    /// No shipped script builds a danger button in any of the ordinary ways
    /// `test_support::dom::danger_class_references` reads: `.btn--danger` is the
    /// shared dialog's confirm button, written here as an `Action`, so a
    /// script that asks first leaves its own button a secondary. The crawled
    /// pages' markup and inline scripts are `htmx_guard`'s
    /// `every_page_confirms_through_the_one_shared_dialog`.
    #[test]
    fn no_shipped_script_draws_a_danger_button() {
        let scripts = shipped_scripts();
        assert!(
            scripts.len() > 40,
            "the walk reached {} scripts; it must reach every shipped one",
            scripts.len()
        );
        let found: Vec<String> = scripts
            .iter()
            .flat_map(|(rel, src)| {
                crate::test_support::dom::danger_class_references(src)
                    .into_iter()
                    .map(move |at| format!("{rel}:{at}"))
            })
            .collect();
        assert!(
            found.is_empty(),
            "a script draws no danger button: ask in the shared confirm dialog \
             (window.impresspressConfirm) from a secondary, and spell every button class \
             in full:\n{}",
            found.join("\n")
        );
    }

    /// The e2e suites that build a page by hand (the products scripts' specs)
    /// put this file and chrome.js on it, so their scripts confirm through
    /// the real dialog. Kept equal to what every page renders.
    #[test]
    fn the_confirm_dialog_fixture_is_what_pages_render() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-web/tests/e2e/fixtures/confirm-dialog.html"
        );
        let expected = format!("{}\n", confirm_dialog().into_string());
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk, expected,
            "{path} is out of date; write the expected text to it"
        );
    }

    #[test]
    fn only_the_confirm_dialog_is_an_alertdialog() {
        assert!(!rendered().contains("role="), "{}", rendered());
        assert!(!rendered().contains("aria-describedby"), "{}", rendered());
    }
}
