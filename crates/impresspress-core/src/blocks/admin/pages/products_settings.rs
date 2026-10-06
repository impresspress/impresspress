//! The save behind the products block's settings page
//! (`/b/products/admin/settings`, which stays in the Products section).
//!
//! That page shows two shared keys — `WAFER_RUN_SHARED__FRONTEND_URL` and
//! `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` — beside the products block's own
//! `IMPRESSPRESS__PRODUCTS__*` keys. WRAP lets only the admin block write a
//! shared key, so the form posts here, and this handler writes the whole
//! form in one request: the allowlist is exactly the vars the page renders
//! (`products::pages::settings_allowlist`, the products block's declarations),
//! each key under its own prefix. The block-scoped keys stay the products
//! block's — it declares and reads them; the admin block may write them as it
//! may any block's (the Variables page does the same). One request keeps the
//! save atomic: `save_settings` refuses a bad field before its first write.

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::ui::settings_form;

/// `POST /b/admin/settings/products`.
pub async fn handle_save_products_settings(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let allowed = match crate::blocks::products::pages::settings_allowlist(ctx).await {
        Ok(allowed) => allowed,
        Err(e) => {
            return crate::blocks::crud::db_error_internal(e, "Could not read the products runtime")
        }
    };
    settings_form::save_settings(ctx, msg, input, &allowed, "products").await
}
