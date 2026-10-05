//! The save behind the legal pages block's settings page
//! (`/b/legalpages/admin/settings`, which stays in the Legal section).
//!
//! Every settings form saves through this block — the one WRAP lets write
//! any key, shared or block-scoped — so a save runs in a single frame that
//! can read the rows it writes and validate the whole form before the first
//! write. The keys are the legal pages block's own `IMPRESSPRESS__LEGALPAGES__*`
//! declarations (`legalpages::config_vars`), under their own prefix.

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::ui::settings_form;

/// `POST /b/admin/settings/legal`.
pub async fn handle_save_legal_settings(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    settings_form::save_settings(
        ctx,
        msg,
        input,
        &crate::blocks::legalpages::config_vars(),
        "legalpages",
    )
    .await
}
