mod handlers;
pub(crate) mod migrations;
mod pages;
mod pricing;
mod purchase;
mod repo;
mod stripe;
mod variables;

#[cfg(test)]
mod tests;

pub(crate) use handlers::{
    GROUPS_TABLE, GROUP_TEMPLATES_TABLE, PRODUCTS_TABLE, PRODUCT_TEMPLATES_TABLE, TYPES_TABLE,
};
pub(crate) use pricing::TABLE as PRICING_TABLE;
pub(crate) use repo::purchases::{LINE_ITEMS_TABLE, PURCHASES_TABLE};
pub(crate) use variables::TABLE as VARIABLES_TABLE;
use wafer_run::{BlockEndpoint, BlockInfo, ConfigVar, InputType, InstanceMode};

use super::rate_limit::{check_user_rate_limit, RateLimitOutcome, UserRateLimiter};
use crate::http::err_not_found;

/// The products block's own declared config vars. Single source of truth for
/// both `BlockInfo::config_keys` and the admin settings page (which renders
/// these via `ui::settings_form` rather than a parallel tuple table).
pub(crate) fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "Stripe API secret key",
            "",
        )
        .name("Stripe Secret Key")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            "Stripe webhook signing secret",
            "",
        )
        .name("Stripe Webhook Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
            "Stripe API base URL",
            "https://api.stripe.com",
        )
        .name("Stripe API URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__WEBHOOK_URL",
            "Webhook URL for billing events",
            "",
        )
        .name("Billing Webhook URL")
        .input_type(InputType::Url)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET",
            "Webhook signing secret",
            "",
        )
        .name("Billing Webhook Secret")
        .input_type(InputType::Password)
        .auto_generate(),
    ]
}

crate::impresspress_feature_block! {
    /// Products, groups, pricing, purchases, subscriptions (`impresspress/products`).
    pub struct ProductsBlock;
    fields: { limiter: UserRateLimiter },
    name: "impresspress/products",
    info: |_this| {
        use wafer_run::{AuthLevel, CollectionSchema};

        // Product row shape (see `migrations/001_products_schema.sqlite.sql`),
        // reused below by the public catalog list/detail response schemas —
        // `db::get`/`db::paginated_list` return a `Record { id, data }` where
        // `data` is the full column map (`id` included).
        let product_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "slug": {"type": "string"},
                "base_price": {"type": "number"},
                "currency": {"type": "string"},
                "status": {"type": "string", "description": "draft | active"},
                "category": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "metadata": {"type": "object"},
                "image_url": {"type": "string"},
                "stock": {"type": "integer"},
                "group_id": {"type": "string"},
                "type_id": {"type": "string"},
                "group_template_id": {"type": "string"},
                "product_template_id": {"type": "string"},
                "pricing_template_id": {"type": "string"},
                "requires": {"type": "string"},
                "created_by": {"type": "string"},
                "deleted_at": {"type": ["string", "null"], "format": "date-time", "description": "Null unless the product has been soft-deleted."},
                "created_at": {"type": "string", "format": "date-time"},
                "updated_at": {"type": "string", "format": "date-time"}
            }
        });

        BlockInfo::new("impresspress/products", "0.0.1", "http-handler@v1", "Products, pricing, purchases, and payment integration")
            .instance_mode(InstanceMode::Singleton)
            .requires(vec!["wafer-run/database".into(), "wafer-run/config".into(), "wafer-run/network".into()])
            // Advisory table list — admin "Database tables" discovery + the
            // WRAP grant-UI read only `CollectionSchema::name`. The schema
            // itself (columns, indexes, FKs) lives solely in the block's
            // hand-authored `migrations/*.sqlite.sql` files (the single
            // source for both runtime `migrations::apply()` and the
            // Cloudflare D1 build).
            .collections(vec![
                CollectionSchema::new(PRODUCTS_TABLE),
                CollectionSchema::new(GROUPS_TABLE),
                CollectionSchema::new(TYPES_TABLE),
                CollectionSchema::new(PRICING_TABLE),
                CollectionSchema::new(PURCHASES_TABLE),
                CollectionSchema::new(LINE_ITEMS_TABLE),
                CollectionSchema::new(GROUP_TEMPLATES_TABLE),
                CollectionSchema::new(PRODUCT_TEMPLATES_TABLE),
                CollectionSchema::new(VARIABLES_TABLE),
            ])
            .category(wafer_run::BlockCategory::Feature)
            .description("Product catalog, pricing engine, and payment processing. Manages products, groups, pricing templates with formula evaluation, purchases, and Stripe integration for checkout and recurring subscriptions.")
            // Declared in full so the central router enforces each tier from
            // the declared `AuthLevel` — the block dropped its in-handler
            // `is_admin` preambles, so any admin path NOT declared here would
            // silently fall back to the Public prefix tier (a regression). All
            // `/b/products/admin/*` SSR pages and `/b/products/api/admin/*`
            // JSON routes are `Admin`; the public catalog stays Public; the
            // user-facing purchase/checkout/subscription routes are
            // `Authenticated`.
            .endpoints(vec![
                // SSR admin pages.
                //
                // The overview is served by `handle()` for BOTH the canonical
                // slash form (`/b/products/admin/`, the `admin_url`) and the
                // bare no-slash form (`/b/products/admin`) via its
                // `"" | "/" => overview` dispatch arm. The central router's
                // matcher is trailing-slash-significant, so BOTH forms must be
                // declared `Admin` — declaring only the slash form would leave
                // the no-slash form governed solely by the Public `/b/products`
                // prefix tier, letting an anonymous request reach the admin
                // overview (the dispatch table and the declared surface must
                // agree on every path the block actually answers).
                BlockEndpoint::get("/b/products/admin").summary("Overview").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/").summary("Overview").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/manage").summary("Manage products").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/groups").summary("Manage groups").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/pricing").summary("Pricing templates").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/purchases").summary("Purchases").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/settings").summary("Product settings").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/products/admin/settings").summary("Save product settings").auth(AuthLevel::Admin),
                // JSON admin API — products
                BlockEndpoint::get("/b/products/api/admin/products").summary("List products").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/products/api/admin/products").summary("Create product").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/api/admin/products/{id}").summary("Get product").auth(AuthLevel::Admin),
                BlockEndpoint::patch("/b/products/api/admin/products/{id}").summary("Update product").auth(AuthLevel::Admin),
                BlockEndpoint::delete("/b/products/api/admin/products/{id}").summary("Delete product").auth(AuthLevel::Admin),
                // JSON admin API — groups
                BlockEndpoint::get("/b/products/api/admin/groups").summary("List groups").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/products/api/admin/groups").summary("Create group").auth(AuthLevel::Admin),
                BlockEndpoint::patch("/b/products/api/admin/groups/{id}").summary("Update group").auth(AuthLevel::Admin),
                BlockEndpoint::delete("/b/products/api/admin/groups/{id}").summary("Delete group").auth(AuthLevel::Admin),
                // JSON admin API — types
                BlockEndpoint::get("/b/products/api/admin/types").summary("List types").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/products/api/admin/types").summary("Create type").auth(AuthLevel::Admin),
                BlockEndpoint::delete("/b/products/api/admin/types/{id}").summary("Delete type").auth(AuthLevel::Admin),
                // JSON admin API — pricing
                BlockEndpoint::get("/b/products/api/admin/pricing").summary("List pricing").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/products/api/admin/pricing").summary("Create pricing").auth(AuthLevel::Admin),
                BlockEndpoint::patch("/b/products/api/admin/pricing/{id}").summary("Update pricing").auth(AuthLevel::Admin),
                BlockEndpoint::delete("/b/products/api/admin/pricing/{id}").summary("Delete pricing").auth(AuthLevel::Admin),
                // JSON admin API — variables
                BlockEndpoint::get("/b/products/api/admin/variables").summary("List variables").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/products/api/admin/variables").summary("Create variable").auth(AuthLevel::Admin),
                BlockEndpoint::patch("/b/products/api/admin/variables/{id}").summary("Update variable").auth(AuthLevel::Admin),
                BlockEndpoint::delete("/b/products/api/admin/variables/{id}").summary("Delete variable").auth(AuthLevel::Admin),
                // JSON admin API — purchases + stats
                BlockEndpoint::get("/b/products/api/admin/purchases").summary("List purchases").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/api/admin/purchases/{id}").summary("Get purchase").auth(AuthLevel::Admin),
                BlockEndpoint::patch("/b/products/api/admin/purchases/{id}/refund").summary("Refund purchase").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/api/admin/stats").summary("Stats").auth(AuthLevel::Admin),
                // Public + authenticated user surface
                // Public catalog — highest-value developer-facing surface of
                // this block; accurate shapes read from `handlers.rs`
                // (`handle_catalog` → `crud::crud_list` → `RecordList`,
                // `handle_get_product_public` → `db::get` → `Record`). Full
                // schema coverage of the admin/purchase/checkout API is a
                // follow-up.
                BlockEndpoint::get("/b/products/catalog")
                    .summary("Browse catalog")
                    .description("Public list of active products, sorted by name.")
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "page": {"type": "integer", "default": 1},
                            "page_size": {"type": "integer", "default": 20, "maximum": 100}
                        }
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "records": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": {"type": "string"},
                                        "data": product_schema
                                    }
                                }
                            },
                            "total_count": {"type": "integer"},
                            "page": {"type": "integer"},
                            "page_size": {"type": "integer"}
                        }
                    }))
                    .tags(&["products"]),
                BlockEndpoint::get("/b/products/catalog/{id}")
                    .summary("Product detail")
                    .path_params_schema(serde_json::json!({
                        "type": "object",
                        "required": ["id"],
                        "properties": {
                            "id": {"type": "string"}
                        }
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "data": product_schema
                        }
                    }))
                    .tags(&["products"]),
                BlockEndpoint::post("/b/products/checkout").summary("Stripe checkout").auth(AuthLevel::Authenticated),
                BlockEndpoint::post("/b/products/purchases").summary("Create purchase").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/purchases").summary("List purchases").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/purchases/{id}").summary("Get purchase").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/subscription").summary("Subscription status").auth(AuthLevel::Authenticated),
            ])
            .config_keys(config_vars())
            .admin_url("/b/products/admin/")
            .can_disable(true)
    },
    handle: |this, ctx, msg, input| {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        // Settings save (POST to admin settings page). Admin tier enforced
        // centrally from the declared `POST /b/products/admin/settings`
        // endpoint — no in-handler `is_admin` re-check.
        if action == "create" && path == "/b/products/admin/settings" {
            return pages::handle_save_settings(ctx, input).await;
        }

        // SSR pages (GET requests to specific page paths)
        if action == "retrieve" && path.starts_with("/b/products/") {
            let sub = path.strip_prefix("/b/products").unwrap_or("/");
            // Admin pages under /b/products/admin/... — Admin tier enforced
            // centrally from the declared `/b/products/admin/*` endpoints.
            if sub.starts_with("/admin") {
                let admin_sub = sub.strip_prefix("/admin").unwrap_or("/");
                return match admin_sub {
                    "" | "/" => pages::overview(ctx, &msg).await,
                    "/manage" => pages::manage_products(ctx, &msg).await,
                    "/groups" => pages::groups(ctx, &msg).await,
                    "/pricing" => pages::pricing(ctx, &msg).await,
                    "/purchases" => pages::purchases(ctx, &msg).await,
                    "/settings" => pages::settings(ctx, &msg).await,
                    _ => err_not_found("not found"),
                };
            }
            // User-facing pages (require auth but not admin)
            match sub {
                "/my-products" => return pages::my_products(ctx, &msg).await,
                "/my-purchases" => return pages::my_purchases(ctx, &msg).await,
                _ => {} // fall through to API handlers
            }
        }

        // Webhook (no auth, no user rate limit)
        if path == "/b/products/webhooks" || path.starts_with("/b/products/webhooks/") {
            return stripe::handle_webhook(ctx, &msg, input).await;
        }

        // Per-user rate limiting for authenticated endpoints. Allowed(headers)
        // is discarded: attaching X-RateLimit-* to a streaming OutputStream
        // would need platform-side middleware to inject headers after the
        // handler returns. Limits are still enforced, just not surfaced.
        if let RateLimitOutcome::Limited(out) =
            check_user_rate_limit(&this.limiter, ctx, &msg).await
        {
            return out;
        }

        // Admin API at /b/products/api/admin/... — dispatched against the
        // normalized `/admin/b/products/...` sub-path passed EXPLICITLY (no
        // `req.resource` rewrite). Admin tier enforced centrally from the
        // declared `/b/products/api/admin/*` endpoints; the in-block
        // `is_admin` preamble is gone.
        if let Some(rest) = path.strip_prefix("/b/products/api/admin") {
            let norm = format!("/admin/b/products{rest}");
            return handlers::handle_admin(ctx, &mut msg, &norm, input).await;
        }

        // User API at /b/products/api/... — normalized to /b/products/... and
        // passed explicitly.
        if let Some(rest) = path.strip_prefix("/b/products/api") {
            let norm = format!("/b/products{rest}");
            return handlers::handle_user(ctx, &mut msg, &norm, input).await;
        }

        // User endpoints at /b/products/... (catalog, checkout, subscription,
        // etc.) — the on-the-wire path is already normalized.
        if path.starts_with("/b/products/") || path == "/b/products" {
            return handlers::handle_user(ctx, &mut msg, &path, input).await;
        }

        err_not_found("not found")
    },
    lifecycle: |_this, ctx, event| {
        // Apply block-owned schema migrations. Migration 002 seeds the default
        // group/product templates (the static FK-parent rows the
        // groups/products tables require) via idempotent INSERTs, so there is
        // no per-request runtime existence-check + seed — the hash-gate
        // short-circuits in memory once applied.
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/products",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await
    },
}
