//! Page Header

use maud::{html, Markup};

/// Render a page header with title, optional subtitle, and optional action slot.
///
/// The title is an `h2`: the shell's topbar owns the page's single `h1`
/// (see `ui::shell::render_topbar`), so body headers are section headings.
pub fn page_header(title: &str, subtitle: Option<&str>, action: Option<Markup>) -> Markup {
    html! {
        // Wraps: the action drops under the title rather than pushing the
        // row past a narrow viewport (buttons keep their label width).
        div .flex .flex-wrap .items-center .justify-between .gap-3 .mb-4 {
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
