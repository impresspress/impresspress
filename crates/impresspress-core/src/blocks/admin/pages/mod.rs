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
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    platform_state::request_logs,
    ui::{
        self,
        components::BadgeVariant,
        shell::{Crumb, Topbar},
        NavKind, Shell,
    },
};

/// Wrap content in the admin shell: the shared [`ui::shell_page`] with the
/// admin sidebar. The caller passes a `Topbar` describing the page's
/// breadcrumbs, subtitle and optional primary action; site config, the
/// signed-in user and the current path come from `ctx` / `msg` like every
/// other shelled page, and nav entries for blocks that aren't registered on
/// this target are hidden rather than linking into a 404.
pub(crate) async fn admin_page(
    ctx: &dyn Context,
    msg: &Message,
    title: &str,
    topbar: Topbar<'_>,
    content: Markup,
) -> OutputStream {
    ui::shell_page(
        ctx,
        msg,
        Shell {
            title,
            nav: NavKind::Admin,
            crumbs: topbar.crumbs,
            subtitle: topbar.subtitle,
            primary_action: topbar.primary_action,
        },
        content,
    )
    .await
}

/// The badge a request-log row's status code renders in, on every page that
/// lists rows: a 5xx is `Danger`, any other error row
/// ([`request_logs::is_error_status`]) is `Warning`, the rest `Success`.
pub(crate) fn status_code_badge_variant(status_code: i64) -> BadgeVariant {
    if status_code >= 500 {
        BadgeVariant::Danger
    } else if request_logs::is_error_status(status_code) {
        BadgeVariant::Warning
    } else {
        BadgeVariant::Success
    }
}

/// Convenience: a single top-level breadcrumb with no link.
pub(crate) fn crumb(label: &'static str) -> Vec<Crumb<'static>> {
    vec![Crumb { label, href: None }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{admin_msg, output_body, TestContext};

    /// The badge boundaries: the error floor turns a row amber, a 5xx red.
    #[test]
    fn status_code_badge_variant_splits_at_400_and_500() {
        for (code, variant) in [
            (399, BadgeVariant::Success),
            (400, BadgeVariant::Warning),
            (499, BadgeVariant::Warning),
            (500, BadgeVariant::Danger),
        ] {
            assert_eq!(status_code_badge_variant(code), variant, "{code}");
        }
    }

    /// The admin shell must hide nav entries whose block isn't registered on
    /// this target, exactly as every other shelled page does — otherwise the
    /// Cloudflare and browser builds link to blocks that answer 404.
    #[tokio::test]
    async fn admin_shell_hides_nav_entries_for_unregistered_blocks() {
        // `with_admin` registers no feature blocks, so every block-bound entry
        // (LLM, Vector, Messages, Products, Tickets) must be absent while the
        // plain admin entries stay.
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);

        let out = logs_page(&ctx, &admin_msg("retrieve", "/b/admin/logs")).await;
        let html = String::from_utf8(output_body(out).await).expect("utf-8 page");

        assert!(
            html.contains("href=\"/b/admin/logs\""),
            "the plain admin entries must still render"
        );
        assert!(
            !html.contains("href=\"/b/llm/\""),
            "an entry for an unregistered block must not render"
        );
    }
}
