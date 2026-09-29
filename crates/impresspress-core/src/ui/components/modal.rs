//! Modal

use maud::{html, Markup};

use crate::ui::icons;

/// Render a modal container (hidden by default).
///
/// Backdrop dismissal and the close button are declared, not scripted: the
/// delegated listener in `ui/assets/chrome.js` reads `data-modal-dismiss` and
/// `data-action="modal-close"`. See that file for why no page emits `on*=`.
pub fn modal(id: &str, title: &str, body: Markup) -> Markup {
    html! {
        div .modal-overlay id=(id) hidden data-modal-dismiss {
            div .modal {
                div .modal-header {
                    h3 .modal-title { (title) }
                    button .modal-close data-action="modal-close" data-modal-target=(id) {
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
