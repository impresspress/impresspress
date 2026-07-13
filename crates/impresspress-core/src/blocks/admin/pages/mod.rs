//! SSR pages for the admin block.
//!
//! Each page queries the database directly (same patterns as the JSON handlers)
//! and renders HTML via maud.

mod blocks;
mod dashboard;
mod database;
pub(super) mod email;
mod logs;
pub(super) mod network;
pub(super) mod permissions;
pub(super) mod settings;
mod storage;
mod users;
pub(super) mod variables;

// Re-export all public functions so callers can use `pages::dashboard(...)` etc.
pub use blocks::*;
pub use dashboard::*;
pub use database::*;
pub use email::*;
pub use logs::*;
use maud::Markup;
pub use network::*;
pub use permissions::*;
pub use settings::*;
pub use storage::*;
pub use users::*;
pub use variables::*;
use wafer_run::{Message, OutputStream};

use crate::ui::{
    self, nav_groups,
    shell::{Crumb, Topbar},
    SiteConfig, UserInfo,
};

/// Wrap content in the admin shell. The caller passes a `Topbar` describing
/// the page's breadcrumbs + optional primary action.
pub(crate) fn admin_page(
    title: &str,
    config: &SiteConfig,
    path: &str,
    user: Option<&UserInfo>,
    topbar: Topbar<'_>,
    content: Markup,
    msg: &Message,
) -> OutputStream {
    let groups = nav_groups::admin();
    ui::Page {
        config,
        title,
        nav: &groups,
        user,
        current_path: path,
        topbar,
        body: content,
    }
    .response(msg)
}

/// Convenience: a single top-level breadcrumb with no link.
pub(crate) fn crumb(label: &'static str) -> Vec<Crumb<'static>> {
    vec![Crumb { label, href: None }]
}
