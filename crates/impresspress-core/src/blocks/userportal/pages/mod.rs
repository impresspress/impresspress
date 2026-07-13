pub mod admin_buttons;
pub mod dashboard;
pub mod profile;
pub mod security;
pub mod sessions;

use maud::Markup;
use wafer_run::OutputStream;

use crate::ui::{self, SiteConfig};

/// Render a page in the shared single-card account layout —
/// `ui::layout::page` + `ui::templates::account_card_page` +
/// `ui::html_response`. `title` doubles as the document title and the card
/// heading (every account page uses the same string for both).
fn account_page(
    config: &SiteConfig,
    title: &str,
    back_href: Option<&str>,
    body: Markup,
) -> OutputStream {
    let markup = ui::layout::page(
        title,
        config,
        ui::templates::account_card_page(
            ui::templates::AccountCard {
                logo_url: &config.logo_url,
                title,
                back_href,
            },
            body,
        ),
    );
    ui::html_response(markup)
}
