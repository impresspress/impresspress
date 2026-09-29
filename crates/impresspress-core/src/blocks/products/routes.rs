//! The products block's HTTP surface, declared once.
//!
//! [`ROUTES`] is what `ProductsBlock::handle` dispatches on and what
//! `info().endpoints` is generated from (`endpoint_match::declare`).
//! Templates are the wire paths; `{id}`, `{product_id}`, `{offer_id}`,
//! `{preset_id}` and `{link_id}` are bound into `req.param.*` for the
//! handlers' `msg.var` readers. The two preconditions a route needs beyond
//! the router's auth check — the `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`
//! feature flag ([`user_products_refusal`]) and the seller-suspension check
//! ([`requires_unsuspended_seller`]) — and the rate-limit bucket a route
//! spends ([`rate_limit_for`]) are exhaustive functions of the [`Route`]
//! variant, so the block matches a path exactly once and a new row is a
//! decision on each, not an omission.
//!
//! Every row names the level the central router enforces. `/b/products` is
//! a `Public` router prefix, so a row's declared level is the only thing
//! that makes an admin page admin-only or a seller API authenticated; the
//! block has no in-handler `is_admin` check. The `public` rows are the
//! anonymous storefront surface (catalog, storefront widget and config,
//! guest pricing, checkout and receipt polling) and the Stripe webhook,
//! which authenticates itself by the `Stripe-Signature` HMAC.

use wafer_run::HttpMethod;

use super::contracts;
use crate::{
    blocks::{
        crud,
        rate_limit::{LimitKey, RateLimit},
    },
    endpoint_match::{request_schema_of, response_schema_of, EndpointRoute},
};

/// Handler for one row of [`ROUTES`]. `PortalHome` serves both root
/// spellings of the portal and `AdminOverview` both of the admin overview;
/// every other variant is one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    // ── A. Admin JSON API, `/b/products/api/admin/...` ──
    AdminListProducts,
    AdminCreateProduct,
    AdminGetProduct,
    AdminUpdateProduct,
    AdminDeleteProduct,
    AdminDuplicateProduct,
    AdminApproveProduct,
    AdminRejectProduct,
    AdminRestoreProduct,
    AdminListOffers,
    AdminCreateOffer,
    AdminGetOffer,
    AdminPreviewOffer,
    AdminUpdateOffer,
    AdminPublishOffer,
    AdminSyncOffer,
    AdminDuplicateOffer,
    AdminArchiveOffer,
    AdminListPresets,
    AdminCreatePreset,
    AdminGetPreset,
    AdminUpdatePreset,
    AdminArchivePreset,
    AdminListPaymentLinks,
    AdminCreatePaymentLink,
    AdminDeactivatePaymentLink,
    AdminListGroups,
    AdminCreateGroup,
    AdminUpdateGroup,
    AdminDeleteGroup,
    AdminListTypes,
    AdminCreateType,
    AdminDeleteType,
    AdminListPurchases,
    AdminGetPurchase,
    AdminRefundPurchase,
    AdminStats,
    AdminStripeStatus,
    AdminWebhookEvents,
    AdminReplayWebhookEvent,
    AdminProviderOperations,
    AdminReconcileProviderOperations,
    AdminListSellers,
    AdminGetSeller,
    AdminSuspendSeller,
    AdminReactivateSeller,
    // ── B. Seller JSON API, `/b/products/api/...` ──
    ListOwnProducts,
    CreateOwnProduct,
    GetOwnProduct,
    UpdateOwnProduct,
    DeleteOwnProduct,
    RestoreOwnProduct,
    DuplicateOwnProduct,
    ListOwnOffers,
    CreateOwnOffer,
    GetOwnOffer,
    PreviewOwnOffer,
    UpdateOwnOffer,
    PublishOwnOffer,
    SyncOwnOffer,
    DuplicateOwnOffer,
    ArchiveOwnOffer,
    ListOwnPresets,
    CreateOwnPreset,
    GetOwnPreset,
    UpdateOwnPreset,
    ArchiveOwnPreset,
    ListOwnPaymentLinks,
    CreateOwnPaymentLink,
    DeactivateOwnPaymentLink,
    SellerAccount,
    SellerStats,
    SellerOrders,
    SellerOrder,
    SellerRefund,
    SellerOnboarding,
    SellerDashboard,
    // ── C. User and public JSON API, `/b/products/...` ──
    ListOwnGroups,
    CreateOwnGroup,
    GetOwnGroup,
    UpdateOwnGroup,
    DeleteOwnGroup,
    OwnGroupProducts,
    ListTypes,
    ListGroupTemplates,
    Catalog,
    CatalogItem,
    StorefrontWidget,
    StorefrontConfig,
    StorefrontProduct,
    GuestOrderStatus,
    PricingPreview,
    ListPurchases,
    GetPurchase,
    Checkout,
    Subscription,
    BillingPortal,
    // ── D. Stripe webhook ──
    Webhook,
    // ── E. SSR pages ──
    PortalHome,
    MyProductsPage,
    NewProductPage,
    MyProductPage,
    MyProductClosePage,
    MyPurchasesPage,
    MyPurchasePage,
    SellingPage,
    SellingOrdersPage,
    SellingOrderPage,
    AdminOverview,
    AdminManagePage,
    AdminNewProductPage,
    AdminProductPage,
    AdminProductClosePage,
    AdminGroupsPage,
    AdminPurchasesPage,
    AdminPurchasePage,
    AdminSellersPage,
    AdminSellerPage,
    AdminStripePage,
    AdminSettingsPage,
    AdminSaveSettings,
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------
//
// Path parameter schemas stay hand-written. They restate the route template,
// and no handler deserializes them — every one reads `msg.var(..)` by name.
// A struct declared only to feed `request_schema_of::<T>` would have no
// runtime user and would generate a byte-identical parameter list. Query
// parameters are typed wherever a handler reads more than one
// (`contracts::PageQuery`, `ProductListQuery`, …: `from_message` is the
// handler's only reader); the few endpoints that read a single
// `msg.query(..)` keep the hand-written form beside that call.

/// The schema `response_schema_of::<T>` would declare for `T`, as a value,
/// for the one place a derived row must be embedded inside a hand-written
/// envelope: the product duplication response, whose sibling `offers` field
/// reaches the recursive `Condition` and so cannot be derived at all yet.
/// Same settings as wafer-block's `self_contained_schema` (inlined, no
/// `$schema`, serialize contract), which is private upstream.
fn view_schema<T: schemars::JsonSchema>() -> serde_json::Value {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
            settings.contract = schemars::generate::Contract::Serialize;
        })
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}

fn id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id"],
        "properties": {"id": {"type": "string"}}
    })
}

fn product_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["product_id"],
        "properties": {"product_id": {"type": "string"}}
    })
}

fn offer_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["product_id", "offer_id"],
        "properties": {
            "product_id": {"type": "string"},
            "offer_id": {"type": "string"}
        }
    })
}

fn preset_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["product_id", "offer_id", "preset_id"],
        "properties": {
            "product_id": {"type": "string"},
            "offer_id": {"type": "string"},
            "preset_id": {"type": "string"}
        }
    })
}

fn link_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["product_id", "offer_id", "link_id"],
        "properties": {
            "product_id": {"type": "string"},
            "offer_id": {"type": "string"},
            "link_id": {"type": "string"}
        }
    })
}

/// The public catalog's `{id}`: declared without `additionalProperties`,
/// unlike [`id_path_schema`], and kept that way because the published
/// contract has it so.
fn catalog_item_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {"type": "string"}
        }
    })
}

fn storefront_product_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["product_id"],
        "properties": {
            "product_id": {"type": "string"}
        }
    })
}

/// The guest receipt's `{id}`; same published shape as
/// [`catalog_item_path_schema`].
fn order_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["id"],
        "properties": {"id": {"type": "string"}}
    })
}

fn receipt_token_query_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["receipt_token"],
        "properties": {"receipt_token": {"type": "string"}}
    })
}

fn webhook_event_list_query_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["pending", "processing", "failed", "processed", "dead_letter"]},
            "page": {"type": "integer", "minimum": 1},
            "page_size": {"type": "integer", "minimum": 1, "maximum": 100}
        }
    })
}

fn provider_operation_list_query_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "status": {"type": "string", "enum": ["pending", "processing", "failed", "succeeded", "dead_letter"]},
            "page": {"type": "integer", "minimum": 1},
            "page_size": {"type": "integer", "minimum": 1, "maximum": 100}
        }
    })
}

fn reconcile_query_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 100}}
    })
}

/// The Stripe event envelope the webhook accepts.
fn webhook_event_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["type", "data"],
        "properties": {
            "id": {"type": "string"},
            "type": {"type": "string"},
            "account": {"type": "string"},
            "livemode": {"type": "boolean"},
            "data": {
                "type": "object",
                "required": ["object"],
                "properties": {"object": {"type": "object"}}
            }
        },
        "additionalProperties": true
    })
}

// NOT derivable: `Condition` is recursive (`All`/`Any` hold child
// `Condition`s), and it reaches these three schemas through
// `OfferComponent`/`OfferComponentDraft`. schemars cannot inline a cycle, so
// it closes it with `{"$ref": "#/$defs/Condition"}` plus a sibling `$defs`.
// Embedded in an OpenAPI document that pointer resolves against the
// *document* root, where no `$defs` exists — a dangling reference that reads
// as an ordinary `$ref` in a diff. Verified by swapping one call site and
// reading the output. These stay hand-written until `generate_openapi`
// hoists definitions into `components/schemas` and rewrites the pointers.
fn offer_definition_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "mode", "currency", "pricing_model", "usage_type", "billing_scheme", "tax_behavior", "components"],
        "properties": {
            "name": {"type": "string"},
            "mode": {"type": "string", "enum": ["payment", "subscription"]},
            "currency": {"type": "string"},
            "pricing_model": {"type": "string", "enum": ["fixed", "components"]},
            "recurring_interval": {"type": ["string", "null"], "enum": ["day", "week", "month", "year", null]},
            "interval_count": {"type": "integer", "minimum": 1, "default": 1},
            "usage_type": {"type": "string", "enum": ["licensed", "metered"]},
            "billing_scheme": {"type": "string", "enum": ["per_unit", "tiered"]},
            "tax_behavior": {"type": "string", "enum": ["unspecified", "inclusive", "exclusive"]},
            "variables": {"type": "array", "items": {"type": "object"}},
            "components": {"type": "array", "items": {"type": "object"}},
            "checkout": {"type": "object"}
        }
    })
}

fn managed_offer_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "sync_status", "sync_error", "offer"],
        "properties": {
            "status": {"type": "string", "enum": ["draft", "active", "archived"]},
            "sync_status": {"type": "string"},
            "sync_error": {"type": "string"},
            "offer": {
                "type": "object",
                "required": ["id", "product_id", "version", "name", "mode", "currency", "pricing_model", "interval_count", "usage_type", "billing_scheme", "tax_behavior", "variables", "components", "checkout", "stripe_product_id", "stripe_price_id"],
                "properties": {
                    "id": {"type": "string"},
                    "product_id": {"type": "string"},
                    "version": {"type": "integer"},
                    "name": {"type": "string"},
                    "mode": {"type": "string", "enum": ["payment", "subscription"]},
                    "currency": {"type": "string"},
                    "pricing_model": {"type": "string", "enum": ["fixed", "components"]},
                    "recurring_interval": {"type": ["string", "null"]},
                    "interval_count": {"type": "integer"},
                    "usage_type": {"type": "string"},
                    "billing_scheme": {"type": "string"},
                    "tax_behavior": {"type": "string"},
                    "variables": {"type": "array", "items": {"type": "object"}},
                    "components": {"type": "array", "items": {"type": "object"}},
                    "checkout": {"type": "object"},
                    "stripe_product_id": {"type": "string"},
                    "stripe_price_id": {"type": "string"}
                }
            }
        }
    })
}

fn offer_list_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["offers"],
        "properties": {"offers": {"type": "array", "items": managed_offer_schema()}}
    })
}

/// Half derived: `product` is `contracts::ProductView`; `offers` stays
/// hand-written because `ManagedOffer` is recursive (see above).
fn product_duplicate_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["product", "offers"],
        "properties": {
            "product": view_schema::<contracts::ProductView>(),
            "offers": {"type": "array", "items": managed_offer_schema()}
        }
    })
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// The block's HTTP surface, in the order `info()` has always listed it.
/// Two orderings matter for first-match dispatch and are kept:
/// `/b/products/my-products/new` before `/b/products/my-products/{id}`, and
/// `/b/products/storefront/config` before `/b/products/storefront/{product_id}`.
pub(super) const ROUTES: &[EndpointRoute<Route>] = &[
    // ── Authenticated commerce portal pages ── Both root forms are declared
    // because endpoint matching is trailing-slash aware.
    EndpointRoute::authenticated(HttpMethod::Get, "/b/products", Route::PortalHome)
        .summary("Commerce portal"),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/products/", Route::PortalHome)
        .summary("Commerce portal"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/my-products",
        Route::MyProductsPage,
    )
    .summary("Manage own products"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/my-products/new",
        Route::NewProductPage,
    )
    .summary("Create own product"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/my-products/{id}",
        Route::MyProductPage,
    )
    .summary("Manage own product"),
    // `{id}` matches exactly one segment and `match_template` rejects a path
    // with segments left over, so the row above does NOT cover
    // `.../{id}/close`. The page's whole job is acting on a soft-deleted
    // product, so which tier it answers at is stated here rather than
    // inherited from the router's fail-closed fallback.
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/my-products/{id}/close",
        Route::MyProductClosePage,
    )
    .summary("Close own deleted product's Stripe surface")
    .description("Archive the offers and deactivate the payment links of a product the caller owns and has deleted, without restoring it to the catalog."),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/my-purchases",
        Route::MyPurchasesPage,
    )
    .summary("View own purchases"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/my-purchases/{id}",
        Route::MyPurchasePage,
    )
    .summary("View own purchase detail"),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/products/selling", Route::SellingPage)
        .summary("Seller dashboard"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/selling/orders",
        Route::SellingOrdersPage,
    )
    .summary("Seller orders"),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/selling/orders/{id}",
        Route::SellingOrderPage,
    )
    .summary("Seller order detail"),
    // ── SSR admin pages ── `/b/products` is a PUBLIC router prefix, so
    // these rows are the only thing that makes the pages admin-only. The
    // overview is declared in both the slash form (the `admin_url`) and the
    // bare form; the matcher's slash retry would serve the bare form from
    // the slash row, but the router's `endpoint_auth` must resolve it
    // `Admin` too, and both forms have always been declared.
    EndpointRoute::admin(HttpMethod::Get, "/b/products/admin", Route::AdminOverview)
        .summary("Overview"),
    EndpointRoute::admin(HttpMethod::Get, "/b/products/admin/", Route::AdminOverview)
        .summary("Overview"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/manage",
        Route::AdminManagePage,
    )
    .summary("Manage products"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/new",
        Route::AdminNewProductPage,
    )
    .summary("Create product"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/products/{id}",
        Route::AdminProductPage,
    )
    .summary("Manage product"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/products/{id}/close",
        Route::AdminProductClosePage,
    )
    .summary("Close a deleted product's Stripe surface")
    .description("Archive the offers and deactivate the payment links of a soft-deleted product, without restoring it to the catalog."),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/groups",
        Route::AdminGroupsPage,
    )
    .summary("Manage groups"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/purchases",
        Route::AdminPurchasesPage,
    )
    .summary("Purchases"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/purchases/{id}",
        Route::AdminPurchasePage,
    )
    .summary("Purchase detail"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/sellers",
        Route::AdminSellersPage,
    )
    .summary("Seller governance and moderation"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/sellers/{id}",
        Route::AdminSellerPage,
    )
    .summary("Seller capability and product detail"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/stripe",
        Route::AdminStripePage,
    )
    .summary("Stripe setup"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/admin/settings",
        Route::AdminSettingsPage,
    )
    .summary("Product settings"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/admin/settings",
        Route::AdminSaveSettings,
    )
    .summary("Save product settings"),
    // ── JSON admin API — products ──
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/products",
        Route::AdminListProducts,
    )
    .summary("List products")
    .query_params(request_schema_of::<contracts::ProductListQuery>)
    .output(response_schema_of::<contracts::ProductListResponse>)
    .tags(&["products", "admin"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products",
        Route::AdminCreateProduct,
    )
    .summary("Create product")
    .input(request_schema_of::<contracts::CreateProductRequest>)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "admin"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/products/{id}",
        Route::AdminGetProduct,
    )
    .summary("Get product")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "admin"]),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/products/api/admin/products/{id}",
        Route::AdminUpdateProduct,
    )
    .summary("Update product")
    .path_params(id_path_schema)
    .input(request_schema_of::<contracts::UpdateProductRequest>)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "admin"]),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/products/api/admin/products/{id}",
        Route::AdminDeleteProduct,
    )
    .summary("Delete product")
    .path_params(id_path_schema)
    .output(response_schema_of::<crud::Deleted>)
    .tags(&["products", "admin"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{id}/duplicate",
        Route::AdminDuplicateProduct,
    )
    .summary("Duplicate product and editable offers")
    .path_params(id_path_schema)
    .output(product_duplicate_schema)
    .tags(&["products", "admin"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{id}/approve",
        Route::AdminApproveProduct,
    )
    .summary("Approve a seller product waiting for moderation")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "admin", "moderation"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{id}/reject",
        Route::AdminRejectProduct,
    )
    .summary("Return a seller product to draft after moderation")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "admin", "moderation"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{id}/restore",
        Route::AdminRestoreProduct,
    )
    .summary("Restore a soft-deleted product")
    .description("Clears `deleted_at`, undoing `soft_delete`. A soft-deleted product is not editable through the normal admin PATCH until it is restored.")
    .path_params(id_path_schema)
    // Typed like every other product row: it hands back the same product
    // view every other product endpoint does.
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "admin"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/products/{product_id}/offers",
        Route::AdminListOffers,
    )
    .summary("List product offers")
    .path_params(product_id_path_schema)
    .output(offer_list_schema)
    .tags(&["products", "admin", "offers"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers",
        Route::AdminCreateOffer,
    )
    .summary("Create product offer")
    .path_params(product_id_path_schema)
    .input(offer_definition_schema)
    .output(managed_offer_schema)
    .tags(&["products", "admin", "offers"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}",
        Route::AdminGetOffer,
    )
    .summary("Get product offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "admin", "offers"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/preview",
        Route::AdminPreviewOffer,
    )
    .summary("Preview draft or active product offer")
    .description("Evaluate an owner-visible immutable or draft offer with the server pricing engine. Browser totals are never trusted.")
    .path_params(offer_path_schema)
    .input(request_schema_of::<contracts::PricingPreviewRequest>)
    .output(response_schema_of::<contracts::PricingPreview>)
    .tags(&["products", "admin", "offers", "pricing"]),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}",
        Route::AdminUpdateOffer,
    )
    .summary("Update draft offer")
    .path_params(offer_path_schema)
    .input(offer_definition_schema)
    .output(managed_offer_schema)
    .tags(&["products", "admin", "offers"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/publish",
        Route::AdminPublishOffer,
    )
    .summary("Publish offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "admin", "offers"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/sync",
        Route::AdminSyncOffer,
    )
    .summary("Synchronize immutable Product and fixed Prices to Stripe")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "admin", "offers", "stripe"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/duplicate",
        Route::AdminDuplicateOffer,
    )
    .summary("Duplicate offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "admin", "offers"]),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}",
        Route::AdminArchiveOffer,
    )
    .summary("Archive offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "admin", "offers"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets",
        Route::AdminListPresets,
    )
    .summary("List checkout presets")
    .path_params(offer_path_schema)
    .output(response_schema_of::<contracts::CheckoutPresetList>)
    .tags(&["products", "admin", "offers", "payment-links"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets",
        Route::AdminCreatePreset,
    )
    .summary("Create checkout preset")
    .path_params(offer_path_schema)
    .input(request_schema_of::<contracts::CheckoutPresetRequest>)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "admin", "offers", "payment-links"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        Route::AdminGetPreset,
    )
    .summary("Get checkout preset")
    .path_params(preset_path_schema)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "admin", "offers", "payment-links"]),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        Route::AdminUpdatePreset,
    )
    .summary("Update checkout preset")
    .path_params(preset_path_schema)
    .input(request_schema_of::<contracts::CheckoutPresetRequest>)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "admin", "offers", "payment-links"]),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        Route::AdminArchivePreset,
    )
    .summary("Archive checkout preset")
    .path_params(preset_path_schema)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "admin", "offers", "payment-links"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links",
        Route::AdminListPaymentLinks,
    )
    .summary("List Payment Links")
    .path_params(offer_path_schema)
    .output(response_schema_of::<contracts::PaymentLinkList>)
    .tags(&["products", "admin", "offers", "payment-links", "stripe"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links",
        Route::AdminCreatePaymentLink,
    )
    .summary("Create or reuse Payment Link")
    .path_params(offer_path_schema)
    .input(request_schema_of::<contracts::PaymentLinkCreateRequest>)
    .output(response_schema_of::<contracts::ManagedPaymentLink>)
    .tags(&["products", "admin", "offers", "payment-links", "stripe"]),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
        Route::AdminDeactivatePaymentLink,
    )
    .summary("Deactivate Payment Link")
    .path_params(link_path_schema)
    .output(response_schema_of::<contracts::ManagedPaymentLink>)
    .tags(&["products", "admin", "offers", "payment-links", "stripe"]),
    // ── JSON admin API — groups ──
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/groups",
        Route::AdminListGroups,
    )
    .summary("List groups")
    .query_params(request_schema_of::<contracts::PageQuery>)
    .output(response_schema_of::<contracts::GroupListResponse>)
    .tags(&["products", "admin", "groups"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/groups",
        Route::AdminCreateGroup,
    )
    .summary("Create group")
    .input(request_schema_of::<contracts::CreateGroupRequest>)
    .output(response_schema_of::<contracts::GroupView>)
    .tags(&["products", "admin", "groups"]),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/products/api/admin/groups/{id}",
        Route::AdminUpdateGroup,
    )
    .summary("Update group")
    .path_params(id_path_schema)
    .input(request_schema_of::<contracts::UpdateGroupRequest>)
    .output(response_schema_of::<contracts::GroupView>)
    .tags(&["products", "admin", "groups"]),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/products/api/admin/groups/{id}",
        Route::AdminDeleteGroup,
    )
    .summary("Delete group")
    .path_params(id_path_schema)
    .output(response_schema_of::<crud::Deleted>)
    .tags(&["products", "admin", "groups"]),
    // ── JSON admin API — types ──
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/types",
        Route::AdminListTypes,
    )
    .summary("List types")
    .query_params(request_schema_of::<contracts::PageQuery>)
    .output(response_schema_of::<contracts::ProductTypeListResponse>)
    .tags(&["products", "admin", "types"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/types",
        Route::AdminCreateType,
    )
    .summary("Create type")
    .input(request_schema_of::<contracts::CreateProductTypeRequest>)
    .output(response_schema_of::<contracts::ProductTypeView>)
    .tags(&["products", "admin", "types"]),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/products/api/admin/types/{id}",
        Route::AdminDeleteType,
    )
    .summary("Delete type")
    .path_params(id_path_schema)
    .output(response_schema_of::<crud::Deleted>)
    .tags(&["products", "admin", "types"]),
    // ── JSON admin API — purchases + stats ──
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/purchases",
        Route::AdminListPurchases,
    )
    .summary("List purchases")
    .query_params(request_schema_of::<contracts::AdminPurchaseListQuery>)
    .output(response_schema_of::<contracts::PurchaseListResponse>)
    .tags(&["products", "admin", "orders"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/purchases/{id}",
        Route::AdminGetPurchase,
    )
    .summary("Get purchase")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::PurchaseDetailResponse>)
    .tags(&["products", "admin", "orders"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/purchases/{id}/refund",
        Route::AdminRefundPurchase,
    )
    .summary("Create an idempotent full or partial refund")
    .path_params(id_path_schema)
    .input(request_schema_of::<contracts::RefundRequest>)
    .output(response_schema_of::<contracts::RefundResult>)
    .tags(&["products", "admin", "refunds"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/stats",
        Route::AdminStats,
    )
    .summary("Commerce analytics separated by currency")
    .output(response_schema_of::<contracts::AdminStats>)
    .tags(&["products", "admin", "analytics"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/stripe/status",
        Route::AdminStripeStatus,
    )
    .summary("Validate Stripe connection and account mode")
    .output(response_schema_of::<contracts::StripeConnectionStatus>)
    .tags(&["products", "admin", "stripe"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/webhook-events",
        Route::AdminWebhookEvents,
    )
    .summary("List safe Stripe webhook processing state")
    .query_params(webhook_event_list_query_schema)
    .output(response_schema_of::<contracts::WebhookEventList>)
    .tags(&["products", "admin", "stripe", "webhooks"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/webhook-events/{id}/replay",
        Route::AdminReplayWebhookEvent,
    )
    .summary("Replay a failed or dead-letter Stripe webhook")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::WebhookAck>)
    .tags(&["products", "admin", "stripe", "webhooks"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/provider-operations",
        Route::AdminProviderOperations,
    )
    .summary("List safe Stripe provider reconciliation state")
    .query_params(provider_operation_list_query_schema)
    .output(response_schema_of::<contracts::ProviderOperationList>)
    .tags(&["products", "admin", "stripe", "reconciliation"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/provider-operations/reconcile",
        Route::AdminReconcileProviderOperations,
    )
    .summary("Claim and reconcile due Stripe provider operations")
    .description("Safe for an authenticated scheduler or manual administrator recovery action; leases and original Stripe idempotency keys prevent duplicate mutations.")
    .query_params(reconcile_query_schema)
    .output(response_schema_of::<contracts::ProviderReconcileResult>)
    .tags(&["products", "admin", "stripe", "reconciliation"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/sellers",
        Route::AdminListSellers,
    )
    .summary("List seller accounts and capability state")
    .output(response_schema_of::<contracts::SellerAccountList>)
    .tags(&["products", "admin", "seller", "stripe-connect"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/products/api/admin/sellers/{id}",
        Route::AdminGetSeller,
    )
    .summary("Get seller account and owned products")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::AdminSellerDetail>)
    .tags(&["products", "admin", "seller", "stripe-connect"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/sellers/{id}/suspend",
        Route::AdminSuspendSeller,
    )
    .summary("Suspend a seller after provider-safe offer archival")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::SellerAccount>)
    .tags(&["products", "admin", "seller", "stripe-connect"]),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/products/api/admin/sellers/{id}/reactivate",
        Route::AdminReactivateSeller,
    )
    .summary("Reactivate a seller for onboarding or sales")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::SellerAccount>)
    .tags(&["products", "admin", "seller", "stripe-connect"]),
    // ── Seller JSON API — own products and offers ── gated on
    // `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` (`user_products_refusal`) and,
    // for the mutations, on the seller not being suspended.
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/products",
        Route::ListOwnProducts,
    )
    .summary("List own products")
    .query_params(request_schema_of::<contracts::ProductListQuery>)
    .output(response_schema_of::<contracts::ProductListResponse>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products",
        Route::CreateOwnProduct,
    )
    .summary("Create own product")
    .input(request_schema_of::<contracts::CreateProductRequest>)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/products/{id}",
        Route::GetOwnProduct,
    )
    .summary("Get own product")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Patch,
        "/b/products/api/products/{id}",
        Route::UpdateOwnProduct,
    )
    .summary("Update own product")
    .path_params(id_path_schema)
    .input(request_schema_of::<contracts::UpdateProductRequest>)
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/products/api/products/{id}",
        Route::DeleteOwnProduct,
    )
    .summary("Delete own product")
    .path_params(id_path_schema)
    .output(response_schema_of::<crud::Deleted>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{id}/restore",
        Route::RestoreOwnProduct,
    )
    .summary("Restore own soft-deleted product")
    .description("Clears `deleted_at` on a product the caller owns, undoing their own delete. The admin route is `/b/products/api/admin/products/{id}/restore`; this one is scoped to the caller's own products and answers 404 for anyone else's.")
    .path_params(id_path_schema)
    // Same typed view as the admin restore, because both routes are the
    // same write: `handle_user_restore_product` and `handle_restore_product`
    // share one `restore_product` body.
    .output(response_schema_of::<contracts::ProductView>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{id}/duplicate",
        Route::DuplicateOwnProduct,
    )
    .summary("Duplicate own product and editable offers")
    .path_params(id_path_schema)
    .output(product_duplicate_schema)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/products/{product_id}/offers",
        Route::ListOwnOffers,
    )
    .summary("List own product offers")
    .path_params(product_id_path_schema)
    .output(offer_list_schema)
    .tags(&["products", "seller", "offers"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{product_id}/offers",
        Route::CreateOwnOffer,
    )
    .summary("Create own product offer")
    .path_params(product_id_path_schema)
    .input(offer_definition_schema)
    .output(managed_offer_schema)
    .tags(&["products", "seller", "offers"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/products/{product_id}/offers/{offer_id}",
        Route::GetOwnOffer,
    )
    .summary("Get own product offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "seller", "offers"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{product_id}/offers/{offer_id}/preview",
        Route::PreviewOwnOffer,
    )
    .summary("Preview own draft or active offer")
    .description("Evaluate an owned immutable or draft offer with the server pricing engine. Browser totals are never trusted.")
    .path_params(offer_path_schema)
    .input(request_schema_of::<contracts::PricingPreviewRequest>)
    .output(response_schema_of::<contracts::PricingPreview>)
    .tags(&["products", "seller", "offers", "pricing"]),
    EndpointRoute::authenticated(
        HttpMethod::Patch,
        "/b/products/api/products/{product_id}/offers/{offer_id}",
        Route::UpdateOwnOffer,
    )
    .summary("Update own draft offer")
    .path_params(offer_path_schema)
    .input(offer_definition_schema)
    .output(managed_offer_schema)
    .tags(&["products", "seller", "offers"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{product_id}/offers/{offer_id}/publish",
        Route::PublishOwnOffer,
    )
    .summary("Publish own offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "seller", "offers"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{product_id}/offers/{offer_id}/sync",
        Route::SyncOwnOffer,
    )
    .summary("Synchronize own immutable Product and fixed Prices to Stripe")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "seller", "offers", "stripe"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{product_id}/offers/{offer_id}/duplicate",
        Route::DuplicateOwnOffer,
    )
    .summary("Duplicate own offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "seller", "offers"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/products/api/products/{product_id}/offers/{offer_id}",
        Route::ArchiveOwnOffer,
    )
    .summary("Archive own offer")
    .path_params(offer_path_schema)
    .output(managed_offer_schema)
    .tags(&["products", "seller", "offers"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/products/{product_id}/offers/{offer_id}/presets",
        Route::ListOwnPresets,
    )
    .summary("List own checkout presets")
    .path_params(offer_path_schema)
    .output(response_schema_of::<contracts::CheckoutPresetList>)
    .tags(&["products", "seller", "offers", "payment-links"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{product_id}/offers/{offer_id}/presets",
        Route::CreateOwnPreset,
    )
    .summary("Create own checkout preset")
    .path_params(offer_path_schema)
    .input(request_schema_of::<contracts::CheckoutPresetRequest>)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "seller", "offers", "payment-links"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        Route::GetOwnPreset,
    )
    .summary("Get own checkout preset")
    .path_params(preset_path_schema)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "seller", "offers", "payment-links"]),
    EndpointRoute::authenticated(
        HttpMethod::Patch,
        "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        Route::UpdateOwnPreset,
    )
    .summary("Update own checkout preset")
    .path_params(preset_path_schema)
    .input(request_schema_of::<contracts::CheckoutPresetRequest>)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "seller", "offers", "payment-links"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        Route::ArchiveOwnPreset,
    )
    .summary("Archive own checkout preset")
    .path_params(preset_path_schema)
    .output(response_schema_of::<contracts::CheckoutPreset>)
    .tags(&["products", "seller", "offers", "payment-links"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links",
        Route::ListOwnPaymentLinks,
    )
    .summary("List own Payment Links")
    .path_params(offer_path_schema)
    .output(response_schema_of::<contracts::PaymentLinkList>)
    .tags(&["products", "seller", "offers", "payment-links", "stripe"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links",
        Route::CreateOwnPaymentLink,
    )
    .summary("Create or reuse own Payment Link")
    .path_params(offer_path_schema)
    .input(request_schema_of::<contracts::PaymentLinkCreateRequest>)
    .output(response_schema_of::<contracts::ManagedPaymentLink>)
    .tags(&["products", "seller", "offers", "payment-links", "stripe"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
        Route::DeactivateOwnPaymentLink,
    )
    .summary("Deactivate own Payment Link")
    .path_params(link_path_schema)
    .output(response_schema_of::<contracts::ManagedPaymentLink>)
    .tags(&["products", "seller", "offers", "payment-links", "stripe"]),
    // ── Authenticated user-owned groups and builder taxonomy ──
    EndpointRoute::authenticated(HttpMethod::Get, "/b/products/groups", Route::ListOwnGroups)
        .summary("List own product groups")
        .output(response_schema_of::<contracts::GroupListResponse>)
        .tags(&["products", "seller"]),
    EndpointRoute::authenticated(HttpMethod::Post, "/b/products/groups", Route::CreateOwnGroup)
        .summary("Create own product group")
        .input(request_schema_of::<contracts::CreateOwnGroupRequest>)
        .output(response_schema_of::<contracts::GroupView>)
        .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/groups/{id}",
        Route::GetOwnGroup,
    )
    .summary("Get own product group")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::GroupView>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Patch,
        "/b/products/groups/{id}",
        Route::UpdateOwnGroup,
    )
    .summary("Update own product group")
    .path_params(id_path_schema)
    .input(request_schema_of::<contracts::UpdateOwnGroupRequest>)
    .output(response_schema_of::<contracts::GroupView>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/products/groups/{id}",
        Route::DeleteOwnGroup,
    )
    .summary("Delete own product group")
    .path_params(id_path_schema)
    .output(response_schema_of::<crud::Deleted>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/groups/{id}/products",
        Route::OwnGroupProducts,
    )
    .summary("List products in own group")
    .path_params(id_path_schema)
    .query_params(request_schema_of::<contracts::PageQuery>)
    .output(response_schema_of::<contracts::ProductListResponse>)
    .tags(&["products", "seller"]),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/products/types", Route::ListTypes)
        .summary("List product types for the authenticated builder")
        .query_params(request_schema_of::<contracts::PageQuery>)
        .output(response_schema_of::<contracts::ProductTypeListResponse>)
        .tags(&["products", "seller"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/group-templates",
        Route::ListGroupTemplates,
    )
    .summary("List group templates for the authenticated builder")
    .output(response_schema_of::<contracts::GroupTemplateListResponse>)
    .tags(&["products", "seller"]),
    // ── Seller JSON API — account, stats, orders ──
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/seller/account",
        Route::SellerAccount,
    )
    .summary("Seller Stripe account status")
    .output(response_schema_of::<contracts::SellerAccount>)
    .tags(&["products", "seller", "stripe-connect"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/seller/stats",
        Route::SellerStats,
    )
    .summary("Seller analytics separated by currency")
    .output(response_schema_of::<contracts::SellerStats>)
    .tags(&["products", "seller", "analytics"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/seller/orders",
        Route::SellerOrders,
    )
    .summary("List seller-owned orders")
    .query_params(request_schema_of::<contracts::SellerOrderListQuery>)
    .output(response_schema_of::<contracts::SellerOrderListResponse>)
    .tags(&["products", "seller", "orders"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/api/seller/orders/{id}",
        Route::SellerOrder,
    )
    .summary("Get seller-owned order")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::SellerOrderDetailResponse>)
    .tags(&["products", "seller", "orders"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/seller/orders/{id}/refund",
        Route::SellerRefund,
    )
    .summary("Refund a seller-owned order")
    .path_params(id_path_schema)
    .input(request_schema_of::<contracts::RefundRequest>)
    .output(response_schema_of::<contracts::RefundResult>)
    .tags(&["products", "seller", "orders", "refunds"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/seller/onboarding",
        Route::SellerOnboarding,
    )
    .summary("Create seller account and Stripe-hosted onboarding link")
    .input(request_schema_of::<contracts::SellerOnboardingRequest>)
    .output(response_schema_of::<contracts::SellerOnboardingResponse>)
    .tags(&["products", "seller", "stripe-connect"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/api/seller/dashboard",
        Route::SellerDashboard,
    )
    .summary("Create Stripe Express dashboard login link")
    .output(response_schema_of::<contracts::ProviderRedirect>)
    .tags(&["products", "seller", "stripe-connect"]),
    // ── Public catalog ── the anonymous surface of this block. Both
    // endpoints publish `contracts::CatalogProductView`, whose field list
    // (not the row) decides what a guest may read.
    EndpointRoute::public(HttpMethod::Get, "/b/products/catalog", Route::Catalog)
        .summary("Browse catalog")
        .description("Public list of active products, sorted by name.")
        .query_params(request_schema_of::<contracts::PageQuery>)
        .output(response_schema_of::<contracts::CatalogProductListResponse>)
        .tags(&["products"])
        .agent_tool(
            "list_products",
            "List what this store sells, a page at a time, sorted \
             by name. This is the only way to discover a \
             product id: call it first, then pass an id to \
             `get_product` for that product's offers and pricing \
             inputs. There is no search — page through the \
             results to find a product by name.",
        ),
    EndpointRoute::public(HttpMethod::Get, "/b/products/catalog/{id}", Route::CatalogItem)
        .summary("Product detail")
        .path_params(catalog_item_path_schema)
        .output(response_schema_of::<contracts::CatalogProductView>)
        .tags(&["products"]),
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/products/storefront.js",
        Route::StorefrontWidget,
    )
    .summary("Framework-free product storefront widget")
    .description("Browser custom element for static sites. It loads only public product configuration and sends customer inputs to server-owned pricing and checkout endpoints.")
    .tags(&["products", "storefront"]),
    // Listed before `storefront/{product_id}`: dispatch takes the first
    // matching row.
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/products/storefront/config",
        Route::StorefrontConfig,
    )
    .summary("Browser-safe storefront configuration")
    .description("Returns only a validated Stripe publishable key and mode. Secret keys, webhook secrets, provider ids, and API URLs are never exposed.")
    .output(response_schema_of::<contracts::StorefrontConfig>)
    .tags(&["products", "storefront"])
    .agent_tool(
        "get_storefront_config",
        "Get this store's checkout configuration, including whether embedded \
         checkout is available. Call once before starting a checkout.",
    ),
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/products/storefront/{product_id}",
        Route::StorefrontProduct,
    )
    .summary("Storefront product and offers")
    .description("Safe public product detail with active offer summaries and public pricing inputs; internal ownership, provider, and pricing-rule fields are omitted.")
    .path_params(storefront_product_path_schema)
    .output(response_schema_of::<contracts::StorefrontProduct>)
    .tags(&["products", "storefront"])
    .agent_tool(
        "get_product",
        "Get one product's full details and its purchasable offers, including \
         pricing inputs. Call this before previewing a price or starting checkout.",
    ),
    // Public: `stripe::handle_webhook` verifies the `Stripe-Signature` HMAC
    // over the raw bytes before parsing or applying any side effect.
    EndpointRoute::public(HttpMethod::Post, "/b/products/webhooks", Route::Webhook)
        .summary("Receive signed Stripe webhook events")
        .description("Public transport endpoint authenticated by the Stripe-Signature HMAC header. Raw request bytes are verified before parsing or applying any side effect.")
        .input(webhook_event_schema)
        .output(response_schema_of::<contracts::WebhookAck>)
        .tags(&["products", "stripe", "webhooks"]),
    EndpointRoute::public(
        HttpMethod::Post,
        "/b/products/pricing/preview",
        Route::PricingPreview,
    )
    .summary("Preview configured offer")
    .description("Evaluate a persisted active offer from validated customer inputs. Amounts are returned in integer minor units.")
    .input(request_schema_of::<contracts::PricingPreviewRequest>)
    .output(response_schema_of::<contracts::PricingPreview>)
    .tags(&["products", "pricing"])
    .agent_tool(
        "preview_price",
        "Calculate the exact total for an offer given the customer's chosen \
         options, before any payment. Returns amounts in integer minor units. \
         Use this to answer 'how much would X cost' without starting checkout.",
    ),
    EndpointRoute::public(HttpMethod::Post, "/b/products/checkout", Route::Checkout)
        .summary("Stripe checkout")
        .description("Create a hosted or embedded Stripe Checkout Session from a public active offer. Guest checkout is supported and every amount is resolved from the immutable offer.")
        .input(request_schema_of::<contracts::CheckoutRequest>)
        .output(response_schema_of::<contracts::CheckoutResponse>)
        .tags(&["products", "checkout"])
        .agent_tool(
            "start_checkout",
            "Begin a purchase and return a Stripe checkout URL for the customer to \
             complete. Always send `presentation: \"hosted\"` — the `embedded` and \
             `payment_link` modes leave `checkout_url` null and return values only a \
             web page can use. This does NOT complete the payment: always give the \
             returned `checkout_url` to the customer so they can confirm and pay \
             themselves.",
        ),
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/products/orders/{id}/status",
        Route::GuestOrderStatus,
    )
    .summary("Guest checkout status")
    .description("Returns a minimal order projection when supplied with the short-lived receipt capability issued at checkout. Buyer and provider identifiers are omitted.")
    .path_params(order_id_path_schema)
    .query_params(receipt_token_query_schema)
    .output(response_schema_of::<contracts::GuestOrderStatus>)
    .tags(&["products", "storefront"])
    .agent_tool(
        "get_order_status",
        "Check whether an order has been paid, using the receipt token issued \
         when checkout started. Use this after the customer says they have paid.",
    ),
    // ── Authenticated buyer surface ──
    EndpointRoute::authenticated(HttpMethod::Get, "/b/products/purchases", Route::ListPurchases)
        .summary("List own purchases")
        .query_params(request_schema_of::<contracts::PageQuery>)
        .output(response_schema_of::<contracts::BuyerOrderListResponse>)
        .tags(&["products", "orders"])
        .agent_tool(
            "list_my_purchases",
            "List the signed-in customer's own past purchases. Requires a signed-in \
             session; returns nothing useful for anonymous visitors.",
        ),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/purchases/{id}",
        Route::GetPurchase,
    )
    .summary("Get own purchase")
    .path_params(id_path_schema)
    .output(response_schema_of::<contracts::BuyerOrderDetailResponse>)
    .tags(&["products", "orders"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/products/subscription",
        Route::Subscription,
    )
    .summary("Platform subscription status")
    .output(response_schema_of::<contracts::SubscriptionStatusResponse>)
    .tags(&["products", "subscriptions"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/products/billing-portal",
        Route::BillingPortal,
    )
    .summary("Create a Stripe Billing Portal session for an owned customer context")
    .input(request_schema_of::<contracts::BillingPortalRequest>)
    .output(response_schema_of::<contracts::ProviderRedirect>)
    .tags(&["products", "subscriptions", "stripe"]),
];

// ---------------------------------------------------------------------------
// Per-route preconditions and buckets
// ---------------------------------------------------------------------------

/// The refusal a route answers when `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`
/// is off, or `None` for a route the flag does not gate. Own products,
/// offers, presets, payment links and groups, and the whole seller surface
/// (API and pages) exist only while self-serve selling is on; the admin
/// API, the buyer surface, the public storefront and the taxonomy reads do
/// not depend on it. The SSR pages and the JSON API have always worded the
/// refusal differently and both texts are kept, so the gate and its wording
/// are one exhaustive match.
pub(super) const fn user_products_refusal(route: Route) -> Option<&'static str> {
    match route {
        Route::MyProductsPage
        | Route::NewProductPage
        | Route::MyProductPage
        | Route::MyProductClosePage
        | Route::SellingPage
        | Route::SellingOrdersPage
        | Route::SellingOrderPage => Some("User product selling is disabled"),
        Route::ListOwnProducts
        | Route::CreateOwnProduct
        | Route::GetOwnProduct
        | Route::UpdateOwnProduct
        | Route::DeleteOwnProduct
        | Route::RestoreOwnProduct
        | Route::DuplicateOwnProduct
        | Route::ListOwnOffers
        | Route::CreateOwnOffer
        | Route::GetOwnOffer
        | Route::PreviewOwnOffer
        | Route::UpdateOwnOffer
        | Route::PublishOwnOffer
        | Route::SyncOwnOffer
        | Route::DuplicateOwnOffer
        | Route::ArchiveOwnOffer
        | Route::ListOwnPresets
        | Route::CreateOwnPreset
        | Route::GetOwnPreset
        | Route::UpdateOwnPreset
        | Route::ArchiveOwnPreset
        | Route::ListOwnPaymentLinks
        | Route::CreateOwnPaymentLink
        | Route::DeactivateOwnPaymentLink
        | Route::SellerAccount
        | Route::SellerStats
        | Route::SellerOrders
        | Route::SellerOrder
        | Route::SellerRefund
        | Route::SellerOnboarding
        | Route::SellerDashboard
        | Route::ListOwnGroups
        | Route::CreateOwnGroup
        | Route::GetOwnGroup
        | Route::UpdateOwnGroup
        | Route::DeleteOwnGroup
        | Route::OwnGroupProducts => Some("user products are not enabled"),
        Route::AdminListProducts
        | Route::AdminCreateProduct
        | Route::AdminGetProduct
        | Route::AdminUpdateProduct
        | Route::AdminDeleteProduct
        | Route::AdminDuplicateProduct
        | Route::AdminApproveProduct
        | Route::AdminRejectProduct
        | Route::AdminRestoreProduct
        | Route::AdminListOffers
        | Route::AdminCreateOffer
        | Route::AdminGetOffer
        | Route::AdminPreviewOffer
        | Route::AdminUpdateOffer
        | Route::AdminPublishOffer
        | Route::AdminSyncOffer
        | Route::AdminDuplicateOffer
        | Route::AdminArchiveOffer
        | Route::AdminListPresets
        | Route::AdminCreatePreset
        | Route::AdminGetPreset
        | Route::AdminUpdatePreset
        | Route::AdminArchivePreset
        | Route::AdminListPaymentLinks
        | Route::AdminCreatePaymentLink
        | Route::AdminDeactivatePaymentLink
        | Route::AdminListGroups
        | Route::AdminCreateGroup
        | Route::AdminUpdateGroup
        | Route::AdminDeleteGroup
        | Route::AdminListTypes
        | Route::AdminCreateType
        | Route::AdminDeleteType
        | Route::AdminListPurchases
        | Route::AdminGetPurchase
        | Route::AdminRefundPurchase
        | Route::AdminStats
        | Route::AdminStripeStatus
        | Route::AdminWebhookEvents
        | Route::AdminReplayWebhookEvent
        | Route::AdminProviderOperations
        | Route::AdminReconcileProviderOperations
        | Route::AdminListSellers
        | Route::AdminGetSeller
        | Route::AdminSuspendSeller
        | Route::AdminReactivateSeller
        | Route::ListTypes
        | Route::ListGroupTemplates
        | Route::Catalog
        | Route::CatalogItem
        | Route::StorefrontWidget
        | Route::StorefrontConfig
        | Route::StorefrontProduct
        | Route::GuestOrderStatus
        | Route::PricingPreview
        | Route::ListPurchases
        | Route::GetPurchase
        | Route::Checkout
        | Route::Subscription
        | Route::BillingPortal
        | Route::Webhook
        | Route::PortalHome
        | Route::MyPurchasesPage
        | Route::MyPurchasePage
        | Route::AdminOverview
        | Route::AdminManagePage
        | Route::AdminNewProductPage
        | Route::AdminProductPage
        | Route::AdminProductClosePage
        | Route::AdminGroupsPage
        | Route::AdminPurchasesPage
        | Route::AdminPurchasePage
        | Route::AdminSellersPage
        | Route::AdminSellerPage
        | Route::AdminStripePage
        | Route::AdminSettingsPage
        | Route::AdminSaveSettings => None,
    }
}

/// Mutations that a platform suspension must stop while leaving the
/// seller's read-only catalog and order/refund history available. Issuing a
/// refund moves real money, so it is gated too — a buyer who needs to be
/// made whole during a suspension goes through the admin refund route.
pub(super) const fn requires_unsuspended_seller(route: Route) -> bool {
    matches!(
        route,
        Route::CreateOwnProduct
            | Route::UpdateOwnProduct
            | Route::DeleteOwnProduct
            | Route::RestoreOwnProduct
            | Route::DuplicateOwnProduct
            | Route::CreateOwnOffer
            | Route::UpdateOwnOffer
            | Route::PublishOwnOffer
            | Route::SyncOwnOffer
            | Route::DuplicateOwnOffer
            | Route::ArchiveOwnOffer
            | Route::CreateOwnPreset
            | Route::UpdateOwnPreset
            | Route::ArchiveOwnPreset
            | Route::CreateOwnPaymentLink
            | Route::DeactivateOwnPaymentLink
            | Route::CreateOwnGroup
            | Route::UpdateOwnGroup
            | Route::DeleteOwnGroup
            | Route::SellerOnboarding
            | Route::SellerRefund
    )
}

/// The rate-limit bucket a matched route spends, `(key, category, default
/// limit)`, or `None` for a route this layer does not limit. Applied after
/// `dispatch` has chosen the variant, so the block matches a path exactly
/// once.
///
/// Guest pricing, checkout and receipt polling are keyed by client IP
/// because guest storefronts deliberately have no authenticated user; each
/// category can be overridden (or disabled with `0`) through
/// `WAFER_RUN_SHARED__RATE_LIMIT_<CATEGORY>`. Every other JSON route spends
/// the per-user read (`GET`) or write bucket, which `apply_route_limit`
/// skips for a caller with no user. The SSR pages, the settings form POST
/// and the Stripe webhook spend nothing, as they never did.
pub(super) const fn rate_limit_for(route: Route) -> Option<(LimitKey, &'static str, RateLimit)> {
    match route {
        Route::PricingPreview => Some((
            LimitKey::Ip,
            "products_preview",
            RateLimit::PRODUCTS_PREVIEW,
        )),
        Route::Checkout => Some((
            LimitKey::Ip,
            "products_checkout",
            RateLimit::PRODUCTS_CHECKOUT,
        )),
        Route::GuestOrderStatus => Some((
            LimitKey::Ip,
            "products_receipt",
            RateLimit::PRODUCTS_RECEIPT,
        )),
        Route::AdminListProducts
        | Route::AdminGetProduct
        | Route::AdminListOffers
        | Route::AdminGetOffer
        | Route::AdminListPresets
        | Route::AdminGetPreset
        | Route::AdminListPaymentLinks
        | Route::AdminListGroups
        | Route::AdminListTypes
        | Route::AdminListPurchases
        | Route::AdminGetPurchase
        | Route::AdminStats
        | Route::AdminStripeStatus
        | Route::AdminWebhookEvents
        | Route::AdminProviderOperations
        | Route::AdminListSellers
        | Route::AdminGetSeller
        | Route::ListOwnProducts
        | Route::GetOwnProduct
        | Route::ListOwnOffers
        | Route::GetOwnOffer
        | Route::ListOwnPresets
        | Route::GetOwnPreset
        | Route::ListOwnPaymentLinks
        | Route::SellerAccount
        | Route::SellerStats
        | Route::SellerOrders
        | Route::SellerOrder
        | Route::ListOwnGroups
        | Route::GetOwnGroup
        | Route::OwnGroupProducts
        | Route::ListTypes
        | Route::ListGroupTemplates
        | Route::Catalog
        | Route::CatalogItem
        | Route::StorefrontWidget
        | Route::StorefrontConfig
        | Route::StorefrontProduct
        | Route::ListPurchases
        | Route::GetPurchase
        | Route::Subscription => Some((LimitKey::User, "api_read", RateLimit::API_READ)),
        Route::AdminCreateProduct
        | Route::AdminUpdateProduct
        | Route::AdminDeleteProduct
        | Route::AdminDuplicateProduct
        | Route::AdminApproveProduct
        | Route::AdminRejectProduct
        | Route::AdminRestoreProduct
        | Route::AdminCreateOffer
        | Route::AdminPreviewOffer
        | Route::AdminUpdateOffer
        | Route::AdminPublishOffer
        | Route::AdminSyncOffer
        | Route::AdminDuplicateOffer
        | Route::AdminArchiveOffer
        | Route::AdminCreatePreset
        | Route::AdminUpdatePreset
        | Route::AdminArchivePreset
        | Route::AdminCreatePaymentLink
        | Route::AdminDeactivatePaymentLink
        | Route::AdminCreateGroup
        | Route::AdminUpdateGroup
        | Route::AdminDeleteGroup
        | Route::AdminCreateType
        | Route::AdminDeleteType
        | Route::AdminRefundPurchase
        | Route::AdminReplayWebhookEvent
        | Route::AdminReconcileProviderOperations
        | Route::AdminSuspendSeller
        | Route::AdminReactivateSeller
        | Route::CreateOwnProduct
        | Route::UpdateOwnProduct
        | Route::DeleteOwnProduct
        | Route::RestoreOwnProduct
        | Route::DuplicateOwnProduct
        | Route::CreateOwnOffer
        | Route::PreviewOwnOffer
        | Route::UpdateOwnOffer
        | Route::PublishOwnOffer
        | Route::SyncOwnOffer
        | Route::DuplicateOwnOffer
        | Route::ArchiveOwnOffer
        | Route::CreateOwnPreset
        | Route::UpdateOwnPreset
        | Route::ArchiveOwnPreset
        | Route::CreateOwnPaymentLink
        | Route::DeactivateOwnPaymentLink
        | Route::SellerRefund
        | Route::SellerOnboarding
        | Route::SellerDashboard
        | Route::CreateOwnGroup
        | Route::UpdateOwnGroup
        | Route::DeleteOwnGroup
        | Route::BillingPortal => Some((LimitKey::User, "api_write", RateLimit::API_WRITE)),
        Route::Webhook
        | Route::PortalHome
        | Route::MyProductsPage
        | Route::NewProductPage
        | Route::MyProductPage
        | Route::MyProductClosePage
        | Route::MyPurchasesPage
        | Route::MyPurchasePage
        | Route::SellingPage
        | Route::SellingOrdersPage
        | Route::SellingOrderPage
        | Route::AdminOverview
        | Route::AdminManagePage
        | Route::AdminNewProductPage
        | Route::AdminProductPage
        | Route::AdminProductClosePage
        | Route::AdminGroupsPage
        | Route::AdminPurchasesPage
        | Route::AdminPurchasePage
        | Route::AdminSellersPage
        | Route::AdminSellerPage
        | Route::AdminStripePage
        | Route::AdminSettingsPage
        | Route::AdminSaveSettings => None,
    }
}

#[cfg(test)]
mod table_tests {
    use wafer_run::{Block as _, Message};

    use super::*;
    use crate::{blocks::products::ProductsBlock, test_support::anon_msg};

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = ProductsBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }

    fn resolve(action: &str, path: &str) -> (Option<Route>, Message) {
        let mut msg = anon_msg(action, path);
        let route = crate::endpoint_match::dispatch(&mut msg, ROUTES);
        (route, msg)
    }

    /// `(action, wire path, expected route, bound variables)`.
    type Case = (
        &'static str,
        &'static str,
        Route,
        &'static [(&'static str, &'static str)],
    );

    /// Every `(action, wire path)` the block answered before the table, in
    /// its declared spelling (plan Task 1): the 46 `ADMIN_ROUTES` rows mapped
    /// back from the old normalized `/admin/...` form to their wire spelling, the
    /// 51 `USER_ROUTES` rows at the spelling `info()` declared, the webhook,
    /// and the 25 SSR page arms (both root spellings of the portal and the
    /// admin overview).
    fn served_paths() -> Vec<Case> {
        vec![
            // ── A. Admin JSON API ──
            (
                "retrieve",
                "/b/products/api/admin/products",
                Route::AdminListProducts,
                &[],
            ),
            (
                "create",
                "/b/products/api/admin/products",
                Route::AdminCreateProduct,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/admin/products/prod_1",
                Route::AdminGetProduct,
                &[("id", "prod_1")],
            ),
            (
                "update",
                "/b/products/api/admin/products/prod_1",
                Route::AdminUpdateProduct,
                &[("id", "prod_1")],
            ),
            (
                "delete",
                "/b/products/api/admin/products/prod_1",
                Route::AdminDeleteProduct,
                &[("id", "prod_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/duplicate",
                Route::AdminDuplicateProduct,
                &[("id", "prod_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/approve",
                Route::AdminApproveProduct,
                &[("id", "prod_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/reject",
                Route::AdminRejectProduct,
                &[("id", "prod_1")],
            ),
            // Percent-encoded id: the binding is decoded, as the pages that
            // link here encode it (`the_restore_url_the_deleted_view_renders_
            // restores_that_product`).
            (
                "create",
                "/b/products/api/admin/products/prod%2F1%3Fx%23y/restore",
                Route::AdminRestoreProduct,
                &[("id", "prod/1?x#y")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/products/prod_1/offers",
                Route::AdminListOffers,
                &[("product_id", "prod_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/offers",
                Route::AdminCreateOffer,
                &[("product_id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/products/prod_1/offers/offer_1",
                Route::AdminGetOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/offers/offer_1/preview",
                Route::AdminPreviewOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "update",
                "/b/products/api/admin/products/prod_1/offers/offer_1",
                Route::AdminUpdateOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/offers/offer_1/publish",
                Route::AdminPublishOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/offers/offer_1/sync",
                Route::AdminSyncOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/offers/offer_1/duplicate",
                Route::AdminDuplicateOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "delete",
                "/b/products/api/admin/products/prod_1/offers/offer_1",
                Route::AdminArchiveOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/products/prod_1/offers/offer_1/presets",
                Route::AdminListPresets,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/offers/offer_1/presets",
                Route::AdminCreatePreset,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/products/prod_1/offers/offer_1/presets/preset_1",
                Route::AdminGetPreset,
                &[
                    ("product_id", "prod_1"),
                    ("offer_id", "offer_1"),
                    ("preset_id", "preset_1"),
                ],
            ),
            (
                "update",
                "/b/products/api/admin/products/prod_1/offers/offer_1/presets/preset_1",
                Route::AdminUpdatePreset,
                &[("preset_id", "preset_1")],
            ),
            (
                "delete",
                "/b/products/api/admin/products/prod_1/offers/offer_1/presets/preset_1",
                Route::AdminArchivePreset,
                &[("preset_id", "preset_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/products/prod_1/offers/offer_1/payment-links",
                Route::AdminListPaymentLinks,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/admin/products/prod_1/offers/offer_1/payment-links",
                Route::AdminCreatePaymentLink,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "delete",
                "/b/products/api/admin/products/prod_1/offers/offer_1/payment-links/link_1",
                Route::AdminDeactivatePaymentLink,
                &[
                    ("product_id", "prod_1"),
                    ("offer_id", "offer_1"),
                    ("link_id", "link_1"),
                ],
            ),
            (
                "retrieve",
                "/b/products/api/admin/groups",
                Route::AdminListGroups,
                &[],
            ),
            (
                "create",
                "/b/products/api/admin/groups",
                Route::AdminCreateGroup,
                &[],
            ),
            (
                "update",
                "/b/products/api/admin/groups/grp_1",
                Route::AdminUpdateGroup,
                &[("id", "grp_1")],
            ),
            (
                "delete",
                "/b/products/api/admin/groups/grp_1",
                Route::AdminDeleteGroup,
                &[("id", "grp_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/types",
                Route::AdminListTypes,
                &[],
            ),
            (
                "create",
                "/b/products/api/admin/types",
                Route::AdminCreateType,
                &[],
            ),
            (
                "delete",
                "/b/products/api/admin/types/type_1",
                Route::AdminDeleteType,
                &[("id", "type_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/purchases",
                Route::AdminListPurchases,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/admin/purchases/pur_1",
                Route::AdminGetPurchase,
                &[("id", "pur_1")],
            ),
            (
                "create",
                "/b/products/api/admin/purchases/pur_1/refund",
                Route::AdminRefundPurchase,
                &[("id", "pur_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/stats",
                Route::AdminStats,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/admin/stripe/status",
                Route::AdminStripeStatus,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/admin/webhook-events",
                Route::AdminWebhookEvents,
                &[],
            ),
            (
                "create",
                "/b/products/api/admin/webhook-events/evt_1/replay",
                Route::AdminReplayWebhookEvent,
                &[("id", "evt_1")],
            ),
            (
                "retrieve",
                "/b/products/api/admin/provider-operations",
                Route::AdminProviderOperations,
                &[],
            ),
            (
                "create",
                "/b/products/api/admin/provider-operations/reconcile",
                Route::AdminReconcileProviderOperations,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/admin/sellers",
                Route::AdminListSellers,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/admin/sellers/seller_1",
                Route::AdminGetSeller,
                &[("id", "seller_1")],
            ),
            (
                "create",
                "/b/products/api/admin/sellers/seller_1/suspend",
                Route::AdminSuspendSeller,
                &[("id", "seller_1")],
            ),
            (
                "create",
                "/b/products/api/admin/sellers/seller_1/reactivate",
                Route::AdminReactivateSeller,
                &[("id", "seller_1")],
            ),
            // ── B. Seller JSON API ──
            (
                "retrieve",
                "/b/products/api/products",
                Route::ListOwnProducts,
                &[],
            ),
            (
                "create",
                "/b/products/api/products",
                Route::CreateOwnProduct,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/products/prod_1",
                Route::GetOwnProduct,
                &[("id", "prod_1")],
            ),
            (
                "update",
                "/b/products/api/products/prod_1",
                Route::UpdateOwnProduct,
                &[("id", "prod_1")],
            ),
            (
                "delete",
                "/b/products/api/products/prod_1",
                Route::DeleteOwnProduct,
                &[("id", "prod_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/restore",
                Route::RestoreOwnProduct,
                &[("id", "prod_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/duplicate",
                Route::DuplicateOwnProduct,
                &[("id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/api/products/prod_1/offers",
                Route::ListOwnOffers,
                &[("product_id", "prod_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/offers",
                Route::CreateOwnOffer,
                &[("product_id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/api/products/prod_1/offers/offer_1",
                Route::GetOwnOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/offers/offer_1/preview",
                Route::PreviewOwnOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "update",
                "/b/products/api/products/prod_1/offers/offer_1",
                Route::UpdateOwnOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/offers/offer_1/publish",
                Route::PublishOwnOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/offers/offer_1/sync",
                Route::SyncOwnOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/offers/offer_1/duplicate",
                Route::DuplicateOwnOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "delete",
                "/b/products/api/products/prod_1/offers/offer_1",
                Route::ArchiveOwnOffer,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "retrieve",
                "/b/products/api/products/prod_1/offers/offer_1/presets",
                Route::ListOwnPresets,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/offers/offer_1/presets",
                Route::CreateOwnPreset,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "retrieve",
                "/b/products/api/products/prod_1/offers/offer_1/presets/preset_1",
                Route::GetOwnPreset,
                &[("preset_id", "preset_1")],
            ),
            (
                "update",
                "/b/products/api/products/prod_1/offers/offer_1/presets/preset_1",
                Route::UpdateOwnPreset,
                &[("preset_id", "preset_1")],
            ),
            (
                "delete",
                "/b/products/api/products/prod_1/offers/offer_1/presets/preset_1",
                Route::ArchiveOwnPreset,
                &[("preset_id", "preset_1")],
            ),
            (
                "retrieve",
                "/b/products/api/products/prod_1/offers/offer_1/payment-links",
                Route::ListOwnPaymentLinks,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "create",
                "/b/products/api/products/prod_1/offers/offer_1/payment-links",
                Route::CreateOwnPaymentLink,
                &[("product_id", "prod_1"), ("offer_id", "offer_1")],
            ),
            (
                "delete",
                "/b/products/api/products/prod_1/offers/offer_1/payment-links/link_1",
                Route::DeactivateOwnPaymentLink,
                &[("link_id", "link_1")],
            ),
            (
                "retrieve",
                "/b/products/api/seller/account",
                Route::SellerAccount,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/seller/stats",
                Route::SellerStats,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/seller/orders",
                Route::SellerOrders,
                &[],
            ),
            (
                "retrieve",
                "/b/products/api/seller/orders/pur_1",
                Route::SellerOrder,
                &[("id", "pur_1")],
            ),
            (
                "create",
                "/b/products/api/seller/orders/pur_1/refund",
                Route::SellerRefund,
                &[("id", "pur_1")],
            ),
            (
                "create",
                "/b/products/api/seller/onboarding",
                Route::SellerOnboarding,
                &[],
            ),
            (
                "create",
                "/b/products/api/seller/dashboard",
                Route::SellerDashboard,
                &[],
            ),
            // ── C. User and public JSON API ──
            ("retrieve", "/b/products/groups", Route::ListOwnGroups, &[]),
            ("create", "/b/products/groups", Route::CreateOwnGroup, &[]),
            (
                "retrieve",
                "/b/products/groups/grp_1",
                Route::GetOwnGroup,
                &[("id", "grp_1")],
            ),
            (
                "update",
                "/b/products/groups/grp_1",
                Route::UpdateOwnGroup,
                &[("id", "grp_1")],
            ),
            (
                "delete",
                "/b/products/groups/grp_1",
                Route::DeleteOwnGroup,
                &[("id", "grp_1")],
            ),
            (
                "retrieve",
                "/b/products/groups/grp_1/products",
                Route::OwnGroupProducts,
                &[("id", "grp_1")],
            ),
            ("retrieve", "/b/products/types", Route::ListTypes, &[]),
            (
                "retrieve",
                "/b/products/group-templates",
                Route::ListGroupTemplates,
                &[],
            ),
            ("retrieve", "/b/products/catalog", Route::Catalog, &[]),
            (
                "retrieve",
                "/b/products/catalog/prod_1",
                Route::CatalogItem,
                &[("id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/storefront.js",
                Route::StorefrontWidget,
                &[],
            ),
            (
                "retrieve",
                "/b/products/storefront/config",
                Route::StorefrontConfig,
                &[],
            ),
            (
                "retrieve",
                "/b/products/storefront/prod_1",
                Route::StorefrontProduct,
                &[("product_id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/orders/order_1/status",
                Route::GuestOrderStatus,
                &[("id", "order_1")],
            ),
            (
                "create",
                "/b/products/pricing/preview",
                Route::PricingPreview,
                &[],
            ),
            (
                "retrieve",
                "/b/products/purchases",
                Route::ListPurchases,
                &[],
            ),
            (
                "retrieve",
                "/b/products/purchases/pur_1",
                Route::GetPurchase,
                &[("id", "pur_1")],
            ),
            ("create", "/b/products/checkout", Route::Checkout, &[]),
            (
                "retrieve",
                "/b/products/subscription",
                Route::Subscription,
                &[],
            ),
            (
                "create",
                "/b/products/billing-portal",
                Route::BillingPortal,
                &[],
            ),
            // ── D. Webhook ──
            ("create", "/b/products/webhooks", Route::Webhook, &[]),
            // ── E. SSR pages ──
            ("retrieve", "/b/products", Route::PortalHome, &[]),
            ("retrieve", "/b/products/", Route::PortalHome, &[]),
            (
                "retrieve",
                "/b/products/my-products",
                Route::MyProductsPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/my-products/new",
                Route::NewProductPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/my-products/prod_1",
                Route::MyProductPage,
                &[("id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/my-products/prod_1/close",
                Route::MyProductClosePage,
                &[("id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/my-purchases",
                Route::MyPurchasesPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/my-purchases/pur_1",
                Route::MyPurchasePage,
                &[("id", "pur_1")],
            ),
            ("retrieve", "/b/products/selling", Route::SellingPage, &[]),
            (
                "retrieve",
                "/b/products/selling/orders",
                Route::SellingOrdersPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/selling/orders/pur_1",
                Route::SellingOrderPage,
                &[("id", "pur_1")],
            ),
            ("retrieve", "/b/products/admin", Route::AdminOverview, &[]),
            ("retrieve", "/b/products/admin/", Route::AdminOverview, &[]),
            (
                "retrieve",
                "/b/products/admin/manage",
                Route::AdminManagePage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/admin/new",
                Route::AdminNewProductPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/admin/products/prod_1",
                Route::AdminProductPage,
                &[("id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/admin/products/prod_1/close",
                Route::AdminProductClosePage,
                &[("id", "prod_1")],
            ),
            (
                "retrieve",
                "/b/products/admin/groups",
                Route::AdminGroupsPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/admin/purchases",
                Route::AdminPurchasesPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/admin/purchases/pur_1",
                Route::AdminPurchasePage,
                &[("id", "pur_1")],
            ),
            (
                "retrieve",
                "/b/products/admin/sellers",
                Route::AdminSellersPage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/admin/sellers/seller_1",
                Route::AdminSellerPage,
                &[("id", "seller_1")],
            ),
            (
                "retrieve",
                "/b/products/admin/stripe",
                Route::AdminStripePage,
                &[],
            ),
            (
                "retrieve",
                "/b/products/admin/settings",
                Route::AdminSettingsPage,
                &[],
            ),
            (
                "create",
                "/b/products/admin/settings",
                Route::AdminSaveSettings,
                &[],
            ),
        ]
    }

    /// Every path the two dispatch tables and the page arms served resolves
    /// to a row at its declared spelling, with the variables its handler
    /// reads bound and decoded; and every variant is reached by at least one
    /// inventory entry, so a row whose path nobody lists cannot hide in the
    /// table.
    #[test]
    fn every_path_the_block_served_resolves_to_a_row() {
        let cases = served_paths();
        assert_eq!(cases.len(), 123, "one entry per declared row");
        for (action, path, expected, vars) in &cases {
            let (route, msg) = resolve(action, path);
            assert_eq!(route, Some(*expected), "{action} {path}");
            for (name, value) in *vars {
                assert_eq!(msg.var(name), *value, "{action} {path} binds {name}");
            }
        }
        let reached: std::collections::BTreeSet<String> = cases
            .iter()
            .map(|(_, _, route, _)| format!("{route:?}"))
            .collect();
        for row in ROUTES {
            assert!(
                reached.contains(&format!("{:?}", row.handler)),
                "{} {} is a row no served path reaches",
                row.method,
                row.template
            );
        }
    }

    /// Paths that stay unmatched. The first group the old dispatch already
    /// answered 404. The second group is the deliberate narrowing this PR
    /// makes (plan, reconciliation result): every `USER_ROUTES` row used to
    /// answer at a second, undeclared spelling — `/b/products/api/X` for a
    /// row declared at `/b/products/X` and vice versa — and the webhook arm
    /// answered any method and any `/b/products/webhooks/...` suffix. Only
    /// the declared spelling is a row now.
    #[test]
    fn paths_the_block_never_declared_stay_unmatched() {
        for (action, path) in [
            ("retrieve", "/b/products/api"),
            ("retrieve", "/b/products/api/admin"),
            ("retrieve", "/b/products/api/admin/whatever"),
            ("retrieve", "/b/products/admin/whatever"),
            ("retrieve", "/b/products/admin/products/"),
            ("retrieve", "/b/products/admin/products/x/y"),
            ("create", "/b/products/admin/manage"),
            ("retrieve", "/b/other"),
            ("retrieve", "/"),
            // Narrowed: the undeclared second spelling of a user row.
            ("retrieve", "/b/products/api/catalog"),
            ("retrieve", "/b/products/api/catalog/prod_1"),
            ("create", "/b/products/api/checkout"),
            ("create", "/b/products/api/pricing/preview"),
            ("retrieve", "/b/products/api/groups"),
            ("retrieve", "/b/products/api/purchases"),
            ("retrieve", "/b/products/products"),
            ("create", "/b/products/products"),
            ("create", "/b/products/products/prod_1/restore"),
            ("retrieve", "/b/products/products/prod_1/offers"),
            ("retrieve", "/b/products/seller/account"),
            ("create", "/b/products/seller/onboarding"),
            // Narrowed: the old `/b/products/api` prefix strip admitted this
            // and then 404'd in the table; now it 404s at the matcher.
            ("retrieve", "/b/products/apifoo"),
            // Narrowed: the webhook is `POST /b/products/webhooks` exactly.
            ("retrieve", "/b/products/webhooks"),
            ("create", "/b/products/webhooks/extra"),
        ] {
            let (route, _) = resolve(action, path);
            assert_eq!(route, None, "{action} {path} must not resolve");
        }
    }
}

#[cfg(test)]
mod gate_tests {
    use wafer_run::HttpMethod::{self, Delete, Get, Patch, Post};

    use super::*;

    /// The 37 variants `UserRoute::requires_user_products` listed, by method
    /// and the wire spelling `info()` declares for each. The old dispatch
    /// answered the refusal `"user products are not enabled"` for these when
    /// `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` was off.
    const OLD_USER_PRODUCTS_API: &[(HttpMethod, &str)] = &[
        (Get, "/b/products/api/products"),
        (Post, "/b/products/api/products"),
        (Get, "/b/products/api/products/{id}"),
        (Patch, "/b/products/api/products/{id}"),
        (Delete, "/b/products/api/products/{id}"),
        (Post, "/b/products/api/products/{id}/restore"),
        (Post, "/b/products/api/products/{id}/duplicate"),
        (Get, "/b/products/api/products/{product_id}/offers"),
        (Post, "/b/products/api/products/{product_id}/offers"),
        (
            Get,
            "/b/products/api/products/{product_id}/offers/{offer_id}",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/preview",
        ),
        (
            Patch,
            "/b/products/api/products/{product_id}/offers/{offer_id}",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/publish",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/sync",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/duplicate",
        ),
        (
            Delete,
            "/b/products/api/products/{product_id}/offers/{offer_id}",
        ),
        (
            Get,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets",
        ),
        (
            Get,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        ),
        (
            Patch,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        ),
        (
            Delete,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        ),
        (
            Get,
            "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links",
        ),
        (
            Delete,
            "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
        ),
        (Get, "/b/products/groups"),
        (Post, "/b/products/groups"),
        (Get, "/b/products/groups/{id}"),
        (Patch, "/b/products/groups/{id}"),
        (Delete, "/b/products/groups/{id}"),
        (Get, "/b/products/groups/{id}/products"),
        (Get, "/b/products/api/seller/account"),
        (Get, "/b/products/api/seller/stats"),
        (Get, "/b/products/api/seller/orders"),
        (Get, "/b/products/api/seller/orders/{id}"),
        (Post, "/b/products/api/seller/orders/{id}/refund"),
        (Post, "/b/products/api/seller/onboarding"),
        (Post, "/b/products/api/seller/dashboard"),
    ];

    /// The seven page arms `mod.rs::handle` guarded with
    /// `user_products_enabled`, answering `"User product selling is
    /// disabled"`.
    const OLD_USER_PRODUCTS_PAGES: &[&str] = &[
        "/b/products/my-products",
        "/b/products/my-products/new",
        "/b/products/my-products/{id}",
        "/b/products/my-products/{id}/close",
        "/b/products/selling",
        "/b/products/selling/orders",
        "/b/products/selling/orders/{id}",
    ];

    /// The 21 variants `UserRoute::requires_unsuspended_seller` listed.
    const OLD_UNSUSPENDED_SELLER: &[(HttpMethod, &str)] = &[
        (Post, "/b/products/api/products"),
        (Patch, "/b/products/api/products/{id}"),
        (Delete, "/b/products/api/products/{id}"),
        (Post, "/b/products/api/products/{id}/restore"),
        (Post, "/b/products/api/products/{id}/duplicate"),
        (Post, "/b/products/api/products/{product_id}/offers"),
        (
            Patch,
            "/b/products/api/products/{product_id}/offers/{offer_id}",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/publish",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/sync",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/duplicate",
        ),
        (
            Delete,
            "/b/products/api/products/{product_id}/offers/{offer_id}",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets",
        ),
        (
            Patch,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        ),
        (
            Delete,
            "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        ),
        (
            Post,
            "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links",
        ),
        (
            Delete,
            "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
        ),
        (Post, "/b/products/groups"),
        (Patch, "/b/products/groups/{id}"),
        (Delete, "/b/products/groups/{id}"),
        (Post, "/b/products/api/seller/onboarding"),
        (Post, "/b/products/api/seller/orders/{id}/refund"),
    ];

    fn is_row(method: HttpMethod, path: &str) -> bool {
        ROUTES
            .iter()
            .any(|row| row.method == method && row.template == path)
    }

    /// Every row's `ALLOW_USER_PRODUCTS` gate, and the refusal it answers,
    /// is what the old dispatch gave it: the 37 API variants and the 7 page
    /// arms are gated with their own wording, nothing else is.
    #[test]
    fn user_products_gate_is_the_old_assignment() {
        for (method, path) in OLD_USER_PRODUCTS_API {
            assert!(is_row(*method, path), "{method} {path} is not a row");
        }
        for path in OLD_USER_PRODUCTS_PAGES {
            assert!(is_row(Get, path), "GET {path} is not a row");
        }
        for row in ROUTES {
            let expected = if row.method == Get && OLD_USER_PRODUCTS_PAGES.contains(&row.template) {
                Some("User product selling is disabled")
            } else if OLD_USER_PRODUCTS_API
                .iter()
                .any(|(method, path)| *method == row.method && *path == row.template)
            {
                Some("user products are not enabled")
            } else {
                None
            };
            assert_eq!(
                user_products_refusal(row.handler),
                expected,
                "{} {}",
                row.method,
                row.template
            );
        }
    }

    /// Every row's seller-suspension gate is what the old dispatch gave it.
    #[test]
    fn seller_suspension_gate_is_the_old_assignment() {
        for (method, path) in OLD_UNSUSPENDED_SELLER {
            assert!(is_row(*method, path), "{method} {path} is not a row");
        }
        for row in ROUTES {
            let expected = OLD_UNSUSPENDED_SELLER
                .iter()
                .any(|(method, path)| *method == row.method && *path == row.template);
            assert_eq!(
                requires_unsuspended_seller(row.handler),
                expected,
                "{} {}",
                row.method,
                row.template
            );
        }
    }
}

#[cfg(test)]
mod rate_limit_tests {
    use wafer_run::HttpMethod::{self, Get, Post};

    use super::*;
    use crate::blocks::rate_limit::{LimitKey, RateLimit};

    /// The three `PUBLIC_RATE_LIMIT_ROUTES` rules, by the wire path each
    /// predicate compared.
    const OLD_IP_BUCKETS: &[(HttpMethod, &str, &str)] = &[
        (Post, "/b/products/pricing/preview", "products_preview"),
        (Post, "/b/products/checkout", "products_checkout"),
        (Get, "/b/products/orders/{id}/status", "products_receipt"),
    ];

    /// The requests `handle` answered before it reached either rate-limit
    /// check: the SSR page arms, the settings POST and the webhook.
    const OLD_UNLIMITED: &[(HttpMethod, &str)] = &[
        (Post, "/b/products/webhooks"),
        (Get, "/b/products"),
        (Get, "/b/products/"),
        (Get, "/b/products/my-products"),
        (Get, "/b/products/my-products/new"),
        (Get, "/b/products/my-products/{id}"),
        (Get, "/b/products/my-products/{id}/close"),
        (Get, "/b/products/my-purchases"),
        (Get, "/b/products/my-purchases/{id}"),
        (Get, "/b/products/selling"),
        (Get, "/b/products/selling/orders"),
        (Get, "/b/products/selling/orders/{id}"),
        (Get, "/b/products/admin"),
        (Get, "/b/products/admin/"),
        (Get, "/b/products/admin/manage"),
        (Get, "/b/products/admin/new"),
        (Get, "/b/products/admin/products/{id}"),
        (Get, "/b/products/admin/products/{id}/close"),
        (Get, "/b/products/admin/groups"),
        (Get, "/b/products/admin/purchases"),
        (Get, "/b/products/admin/purchases/{id}"),
        (Get, "/b/products/admin/sellers"),
        (Get, "/b/products/admin/sellers/{id}"),
        (Get, "/b/products/admin/stripe"),
        (Get, "/b/products/admin/settings"),
        (Post, "/b/products/admin/settings"),
    ];

    /// Every row spends the bucket the old two-step check gave it: the three
    /// IP rules by wire path; nothing for the pages, the settings POST and
    /// the webhook; and for every other row `check_user_rate_limit`'s rule —
    /// `retrieve` → `api_read`/`API_READ`, anything else →
    /// `api_write`/`API_WRITE`, keyed by user.
    #[test]
    fn rate_limits_are_the_old_assignments() {
        assert_eq!(OLD_UNLIMITED.len(), 26);
        for (method, path, _) in OLD_IP_BUCKETS {
            assert!(
                ROUTES
                    .iter()
                    .any(|row| row.method == *method && row.template == *path),
                "{method} {path} is not a row"
            );
        }
        for (method, path) in OLD_UNLIMITED {
            assert!(
                ROUTES
                    .iter()
                    .any(|row| row.method == *method && row.template == *path),
                "{method} {path} is not a row"
            );
        }
        for row in ROUTES {
            let ip = OLD_IP_BUCKETS
                .iter()
                .find(|(method, path, _)| *method == row.method && *path == row.template)
                .map(|(_, _, category)| *category);
            let unlimited = OLD_UNLIMITED
                .iter()
                .any(|(method, path)| *method == row.method && *path == row.template);
            let expected: Option<(LimitKey, &str, RateLimit)> = match (ip, unlimited) {
                (Some("products_preview"), _) => Some((
                    LimitKey::Ip,
                    "products_preview",
                    RateLimit::PRODUCTS_PREVIEW,
                )),
                (Some("products_checkout"), _) => Some((
                    LimitKey::Ip,
                    "products_checkout",
                    RateLimit::PRODUCTS_CHECKOUT,
                )),
                (Some("products_receipt"), _) => Some((
                    LimitKey::Ip,
                    "products_receipt",
                    RateLimit::PRODUCTS_RECEIPT,
                )),
                (Some(other), _) => panic!("unexpected category {other}"),
                (None, true) => None,
                (None, false) if row.method == Get => {
                    Some((LimitKey::User, "api_read", RateLimit::API_READ))
                }
                (None, false) => Some((LimitKey::User, "api_write", RateLimit::API_WRITE)),
            };
            let actual = rate_limit_for(row.handler);
            let describe = |bucket: Option<(LimitKey, &str, RateLimit)>| {
                bucket.map(|(key, category, limit)| {
                    (key, category.to_string(), limit.max_requests, limit.window)
                })
            };
            assert_eq!(
                describe(actual),
                describe(expected),
                "{} {}",
                row.method,
                row.template
            );
        }
    }
}
