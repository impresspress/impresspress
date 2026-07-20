//! Admin/user route dispatch tables for the products block.
//!
//! `handle_admin`/`handle_user` are the two entry points called by
//! `ProductsBlock::handle` (via `handlers::handle_admin`/`handlers::handle_user`,
//! re-exported at the `handlers` module root) with the normalized sub-path
//! already resolved. Each matches against a declarative [`EndpointRoute`]
//! table and fans out to the domain handler modules.

use wafer_core::clients::config;
use wafer_run::{context::Context, HttpMethod, InputStream, Message, OutputStream};

use super::{
    catalog, commerce, group, offers, payment_links, product, provider, sellers, stats,
    subscription, types,
};
use crate::{
    blocks::products::{purchase, repo, stripe},
    endpoint_match::{self, EndpointRoute},
    http::{err_forbidden, err_not_found},
};

/// Whether `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` is on — gates the
/// user-facing (non-admin) product/group CRUD routes below. Visible at
/// `crate::blocks::products` (re-exported as `handlers::user_products_enabled`)
/// so the admin Overview page (`pages::overview`) can render an accurate
/// notice instead of a silent empty catalog when it's off.
pub(in crate::blocks::products) async fn user_products_enabled(ctx: &dyn Context) -> bool {
    config::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false").await == "true"
}

/// Admin JSON-API dispatch targets (normalized `/admin/b/products/...`).
#[derive(Clone, Copy)]
pub(crate) enum AdminRoute {
    ListProducts,
    GetProduct,
    CreateProduct,
    UpdateProduct,
    DeleteProduct,
    DuplicateProduct,
    ApproveProduct,
    RejectProduct,
    ListOffers,
    GetOffer,
    PreviewManagedOffer,
    CreateOffer,
    UpdateOffer,
    PublishOffer,
    SyncOffer,
    DuplicateOffer,
    ArchiveOffer,
    ListPresets,
    GetPreset,
    CreatePreset,
    UpdatePreset,
    ArchivePreset,
    ListPaymentLinks,
    CreatePaymentLink,
    DeactivatePaymentLink,
    ListGroups,
    CreateGroup,
    UpdateGroup,
    DeleteGroup,
    ListTypes,
    CreateType,
    DeleteType,
    ListPurchases,
    RefundPurchase,
    GetPurchase,
    Stats,
    StripeStatus,
    WebhookEvents,
    ReplayWebhookEvent,
    ProviderOperations,
    ReconcileProviderOperations,
    ListSellers,
    GetSeller,
    SuspendSeller,
    ReactivateSeller,
}

/// Admin dispatch table over the normalized `/admin/b/products/...` paths.
/// The `purchases/{id}/refund` template precedes the generic
/// `purchases/{id}` so the refund route wins (the old `ends_with("/refund")`
/// guard).
const ADMIN_ROUTES: &[EndpointRoute<AdminRoute>] = &[
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products",
        AdminRoute::ListProducts,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products",
        AdminRoute::CreateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products/{id}",
        AdminRoute::GetProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/products/{id}",
        AdminRoute::UpdateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/products/{id}",
        AdminRoute::DeleteProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{id}/duplicate",
        AdminRoute::DuplicateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{id}/approve",
        AdminRoute::ApproveProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{id}/reject",
        AdminRoute::RejectProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products/{product_id}/offers",
        AdminRoute::ListOffers,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{product_id}/offers",
        AdminRoute::CreateOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/publish",
        AdminRoute::PublishOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/sync",
        AdminRoute::SyncOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/duplicate",
        AdminRoute::DuplicateOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/preview",
        AdminRoute::PreviewManagedOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/presets",
        AdminRoute::ListPresets,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/presets",
        AdminRoute::CreatePreset,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        AdminRoute::GetPreset,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        AdminRoute::UpdatePreset,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        AdminRoute::ArchivePreset,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/payment-links",
        AdminRoute::ListPaymentLinks,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/payment-links",
        AdminRoute::CreatePaymentLink,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
        AdminRoute::DeactivatePaymentLink,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/products/{product_id}/offers/{offer_id}",
        AdminRoute::GetOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/products/{product_id}/offers/{offer_id}",
        AdminRoute::UpdateOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/products/{product_id}/offers/{offer_id}",
        AdminRoute::ArchiveOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/groups",
        AdminRoute::ListGroups,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/groups",
        AdminRoute::CreateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/groups/{id}",
        AdminRoute::UpdateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/groups/{id}",
        AdminRoute::DeleteGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/types",
        AdminRoute::ListTypes,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/types",
        AdminRoute::CreateType,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/admin/b/products/types/{id}",
        AdminRoute::DeleteType,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/purchases",
        AdminRoute::ListPurchases,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/admin/b/products/purchases/{id}/refund",
        AdminRoute::RefundPurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/purchases/{id}/refund",
        AdminRoute::RefundPurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/purchases/{id}",
        AdminRoute::GetPurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/stats",
        AdminRoute::Stats,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/stripe/status",
        AdminRoute::StripeStatus,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/webhook-events/{id}/replay",
        AdminRoute::ReplayWebhookEvent,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/webhook-events",
        AdminRoute::WebhookEvents,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/provider-operations",
        AdminRoute::ProviderOperations,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/provider-operations/reconcile",
        AdminRoute::ReconcileProviderOperations,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/sellers",
        AdminRoute::ListSellers,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/sellers/{id}/suspend",
        AdminRoute::SuspendSeller,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/admin/b/products/sellers/{id}/reactivate",
        AdminRoute::ReactivateSeller,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/admin/b/products/sellers/{id}",
        AdminRoute::GetSeller,
    ),
];

/// User-facing dispatch targets (normalized `/b/products/...`).
#[derive(Clone, Copy)]
pub(crate) enum UserRoute {
    ListProducts,
    GetProduct,
    CreateProduct,
    UpdateProduct,
    DeleteProduct,
    DuplicateProduct,
    ListOffers,
    GetOffer,
    PreviewManagedOffer,
    CreateOffer,
    UpdateOffer,
    PublishOffer,
    SyncOffer,
    DuplicateOffer,
    ArchiveOffer,
    ListPresets,
    GetPreset,
    CreatePreset,
    UpdatePreset,
    ArchivePreset,
    ListPaymentLinks,
    CreatePaymentLink,
    DeactivatePaymentLink,
    ListGroups,
    GetGroup,
    CreateGroup,
    UpdateGroup,
    DeleteGroup,
    GroupProducts,
    ListTypes,
    GroupTemplates,
    Catalog,
    CatalogItem,
    StorefrontWidget,
    StorefrontConfig,
    StorefrontProduct,
    GuestOrderStatus,
    PreviewOffer,
    ListPurchases,
    GetPurchase,
    Checkout,
    Subscription,
    SellerAccount,
    SellerStats,
    SellerOrders,
    SellerOrder,
    SellerRefund,
    SellerOnboarding,
    SellerDashboard,
    BillingPortal,
}

impl UserRoute {
    /// Routes that operate on a user's OWN product/group rows and are gated on
    /// `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` (matching the old
    /// `starts_with("/b/products/products"|"groups")` 403 fallback).
    fn requires_user_products(self) -> bool {
        matches!(
            self,
            UserRoute::ListProducts
                | UserRoute::GetProduct
                | UserRoute::CreateProduct
                | UserRoute::UpdateProduct
                | UserRoute::DeleteProduct
                | UserRoute::DuplicateProduct
                | UserRoute::ListOffers
                | UserRoute::GetOffer
                | UserRoute::PreviewManagedOffer
                | UserRoute::CreateOffer
                | UserRoute::UpdateOffer
                | UserRoute::PublishOffer
                | UserRoute::SyncOffer
                | UserRoute::DuplicateOffer
                | UserRoute::ArchiveOffer
                | UserRoute::ListPresets
                | UserRoute::GetPreset
                | UserRoute::CreatePreset
                | UserRoute::UpdatePreset
                | UserRoute::ArchivePreset
                | UserRoute::ListPaymentLinks
                | UserRoute::CreatePaymentLink
                | UserRoute::DeactivatePaymentLink
                | UserRoute::ListGroups
                | UserRoute::GetGroup
                | UserRoute::CreateGroup
                | UserRoute::UpdateGroup
                | UserRoute::DeleteGroup
                | UserRoute::GroupProducts
                | UserRoute::SellerAccount
                | UserRoute::SellerStats
                | UserRoute::SellerOrders
                | UserRoute::SellerOrder
                | UserRoute::SellerRefund
                | UserRoute::SellerOnboarding
                | UserRoute::SellerDashboard
        )
    }

    /// Mutations that a platform suspension must stop while leaving the
    /// seller's read-only catalog and order/refund history available.
    /// Issuing a refund moves real money, so it is gated too — a buyer who
    /// needs to be made whole during a suspension goes through the admin
    /// refund route.
    fn requires_unsuspended_seller(self) -> bool {
        matches!(
            self,
            UserRoute::CreateProduct
                | UserRoute::UpdateProduct
                | UserRoute::DeleteProduct
                | UserRoute::DuplicateProduct
                | UserRoute::CreateOffer
                | UserRoute::UpdateOffer
                | UserRoute::PublishOffer
                | UserRoute::SyncOffer
                | UserRoute::DuplicateOffer
                | UserRoute::ArchiveOffer
                | UserRoute::CreatePreset
                | UserRoute::UpdatePreset
                | UserRoute::ArchivePreset
                | UserRoute::CreatePaymentLink
                | UserRoute::DeactivatePaymentLink
                | UserRoute::CreateGroup
                | UserRoute::UpdateGroup
                | UserRoute::DeleteGroup
                | UserRoute::SellerOnboarding
                | UserRoute::SellerRefund
        )
    }
}

/// User dispatch table over the normalized `/b/products/...` paths. The
/// `groups/{id}/products` template precedes the generic `groups/{id}` so the
/// "products in a group" route wins (the old `ends_with("/products")` guard);
/// `catalog` precedes `catalog/{id}`, and `purchases` precedes
/// `purchases/{id}`.
const USER_ROUTES: &[EndpointRoute<UserRoute>] = &[
    // Own products
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products",
        UserRoute::ListProducts,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products",
        UserRoute::CreateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products/{id}",
        UserRoute::GetProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/products/products/{id}",
        UserRoute::UpdateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/products/products/{id}",
        UserRoute::DeleteProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{id}/duplicate",
        UserRoute::DuplicateProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products/{product_id}/offers",
        UserRoute::ListOffers,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{product_id}/offers",
        UserRoute::CreateOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{product_id}/offers/{offer_id}/publish",
        UserRoute::PublishOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{product_id}/offers/{offer_id}/sync",
        UserRoute::SyncOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{product_id}/offers/{offer_id}/duplicate",
        UserRoute::DuplicateOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{product_id}/offers/{offer_id}/preview",
        UserRoute::PreviewManagedOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products/{product_id}/offers/{offer_id}/presets",
        UserRoute::ListPresets,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{product_id}/offers/{offer_id}/presets",
        UserRoute::CreatePreset,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        UserRoute::GetPreset,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/products/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        UserRoute::UpdatePreset,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/products/products/{product_id}/offers/{offer_id}/presets/{preset_id}",
        UserRoute::ArchivePreset,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products/{product_id}/offers/{offer_id}/payment-links",
        UserRoute::ListPaymentLinks,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/products/{product_id}/offers/{offer_id}/payment-links",
        UserRoute::CreatePaymentLink,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/products/products/{product_id}/offers/{offer_id}/payment-links/{link_id}",
        UserRoute::DeactivatePaymentLink,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/products/{product_id}/offers/{offer_id}",
        UserRoute::GetOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/products/products/{product_id}/offers/{offer_id}",
        UserRoute::UpdateOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/products/products/{product_id}/offers/{offer_id}",
        UserRoute::ArchiveOffer,
    ),
    // Own groups (group-products before the generic {id})
    EndpointRoute::new(HttpMethod::Get, "/b/products/groups", UserRoute::ListGroups),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/groups",
        UserRoute::CreateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/groups/{id}/products",
        UserRoute::GroupProducts,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/groups/{id}",
        UserRoute::GetGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/products/groups/{id}",
        UserRoute::UpdateGroup,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/products/groups/{id}",
        UserRoute::DeleteGroup,
    ),
    // Read-only taxonomy
    EndpointRoute::new(HttpMethod::Get, "/b/products/types", UserRoute::ListTypes),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/pricing/preview",
        UserRoute::PreviewOffer,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/group-templates",
        UserRoute::GroupTemplates,
    ),
    // Catalog (public)
    EndpointRoute::new(HttpMethod::Get, "/b/products/catalog", UserRoute::Catalog),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/catalog/{id}",
        UserRoute::CatalogItem,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/storefront.js",
        UserRoute::StorefrontWidget,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/storefront/config",
        UserRoute::StorefrontConfig,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/storefront/{product_id}",
        UserRoute::StorefrontProduct,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/orders/{id}/status",
        UserRoute::GuestOrderStatus,
    ),
    // Offer pricing, order history, and checkout
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/purchases",
        UserRoute::ListPurchases,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/purchases/{id}",
        UserRoute::GetPurchase,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/checkout",
        UserRoute::Checkout,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/subscription",
        UserRoute::Subscription,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/seller/account",
        UserRoute::SellerAccount,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/seller/stats",
        UserRoute::SellerStats,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/seller/orders/{id}/refund",
        UserRoute::SellerRefund,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/seller/orders/{id}",
        UserRoute::SellerOrder,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/products/seller/orders",
        UserRoute::SellerOrders,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/seller/onboarding",
        UserRoute::SellerOnboarding,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/seller/dashboard",
        UserRoute::SellerDashboard,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/products/billing-portal",
        UserRoute::BillingPortal,
    ),
];

/// Admin JSON-API dispatch.
///
/// `norm` is the normalized admin sub-path (`/admin/b/products/...`), passed
/// as an explicit argument by `ProductsBlock::handle` rather than written
/// back onto `req.resource` (the old in-band routing mutation). The matcher
/// binds `{id}` into `req.param.id` so the `crud::*` helpers read it.
pub async fn handle_admin(
    ctx: &dyn Context,
    msg: &mut Message,
    norm: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action().to_string();
    let Some(route) = endpoint_match::dispatch_path(msg, &action, norm, ADMIN_ROUTES) else {
        return err_not_found("not found");
    };
    match route {
        AdminRoute::ListProducts => product::handle_list_products(ctx, msg).await,
        AdminRoute::GetProduct => product::handle_get_product(ctx, msg).await,
        AdminRoute::CreateProduct => product::handle_create_product(ctx, msg, input).await,
        AdminRoute::UpdateProduct => product::handle_update_product(ctx, msg, input).await,
        AdminRoute::DeleteProduct => product::handle_delete_product(ctx, msg).await,
        AdminRoute::DuplicateProduct => product::handle_duplicate_product(ctx, msg).await,
        AdminRoute::ApproveProduct => sellers::approve_product(ctx, msg).await,
        AdminRoute::RejectProduct => sellers::reject_product(ctx, msg).await,
        AdminRoute::ListOffers => offers::handle_list(ctx, msg, offers::OfferAccess::Admin).await,
        AdminRoute::GetOffer => offers::handle_get(ctx, msg, offers::OfferAccess::Admin).await,
        AdminRoute::PreviewManagedOffer => {
            offers::handle_preview(ctx, msg, input, offers::OfferAccess::Admin).await
        }
        AdminRoute::CreateOffer => {
            offers::handle_create(ctx, msg, input, offers::OfferAccess::Admin).await
        }
        AdminRoute::UpdateOffer => {
            offers::handle_update(ctx, msg, input, offers::OfferAccess::Admin).await
        }
        AdminRoute::PublishOffer => {
            offers::handle_publish(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::SyncOffer => offers::handle_sync(ctx, msg, offers::OfferAccess::Admin).await,
        AdminRoute::DuplicateOffer => {
            offers::handle_duplicate(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::ArchiveOffer => {
            offers::handle_archive(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::ListPresets => {
            payment_links::list_presets(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::GetPreset => {
            payment_links::get_preset(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::CreatePreset => {
            payment_links::create_preset(ctx, msg, input, offers::OfferAccess::Admin).await
        }
        AdminRoute::UpdatePreset => {
            payment_links::update_preset(ctx, msg, input, offers::OfferAccess::Admin).await
        }
        AdminRoute::ArchivePreset => {
            payment_links::archive_preset(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::ListPaymentLinks => {
            payment_links::list_links(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::CreatePaymentLink => {
            payment_links::create_link(ctx, msg, input, offers::OfferAccess::Admin).await
        }
        AdminRoute::DeactivatePaymentLink => {
            payment_links::deactivate_link(ctx, msg, offers::OfferAccess::Admin).await
        }
        AdminRoute::ListGroups => group::handle_list_groups(ctx, msg).await,
        AdminRoute::CreateGroup => group::handle_create_group(ctx, msg, input).await,
        AdminRoute::UpdateGroup => group::handle_update_group(ctx, msg, input).await,
        AdminRoute::DeleteGroup => group::handle_delete_group(ctx, msg).await,
        AdminRoute::ListTypes => types::handle_list_types(ctx, msg).await,
        AdminRoute::CreateType => types::handle_create_type(ctx, msg, input).await,
        AdminRoute::DeleteType => types::handle_delete_type(ctx, msg).await,
        AdminRoute::ListPurchases => purchase::handle_list_admin(ctx, msg).await,
        AdminRoute::RefundPurchase => purchase::handle_refund(ctx, msg, input).await,
        AdminRoute::GetPurchase => purchase::handle_get(ctx, msg).await,
        AdminRoute::Stats => stats::handle_stats(ctx, msg).await,
        AdminRoute::StripeStatus => provider::connection_status(ctx).await,
        AdminRoute::WebhookEvents => provider::webhook_events(ctx, msg).await,
        AdminRoute::ReplayWebhookEvent => provider::replay_webhook_event(ctx, msg).await,
        AdminRoute::ProviderOperations => provider::provider_operations(ctx, msg).await,
        AdminRoute::ReconcileProviderOperations => {
            provider::reconcile_provider_operations(ctx, msg).await
        }
        AdminRoute::ListSellers => sellers::list(ctx).await,
        AdminRoute::GetSeller => sellers::get(ctx, msg).await,
        AdminRoute::SuspendSeller => sellers::suspend(ctx, msg).await,
        AdminRoute::ReactivateSeller => sellers::reactivate(ctx, msg).await,
    }
}

/// User-facing dispatch (own products/groups under `ALLOW_USER_PRODUCTS`, plus
/// the public catalog, purchases, checkout, subscription).
///
/// `norm` is the normalized user sub-path passed explicitly by
/// `ProductsBlock::handle`. The own-products/groups routes are gated on
/// `ALLOW_USER_PRODUCTS` *after* matching, preserving the prior "feature
/// disabled → 403" behaviour for those paths while leaving catalog/purchase
/// routes always available.
pub async fn handle_user(
    ctx: &dyn Context,
    msg: &mut Message,
    norm: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action().to_string();
    let Some(route) = endpoint_match::dispatch_path(msg, &action, norm, USER_ROUTES) else {
        return err_not_found("not found");
    };

    // Own products/groups require ALLOW_USER_PRODUCTS; reject with the same
    // 403 the old `(_, _) if starts_with("/b/products/products"|"groups")` arm
    // produced when the feature is off.
    if route.requires_user_products() && !user_products_enabled(ctx).await {
        return err_forbidden("user products are not enabled");
    }
    if route.requires_unsuspended_seller() {
        match repo::seller_accounts::is_suspended(ctx, msg.user_id()).await {
            Ok(true) => return err_forbidden("Seller account is suspended"),
            Ok(false) => {}
            Err(error) => {
                return crate::http::err_internal("Could not verify seller status", error)
            }
        }
    }

    match route {
        UserRoute::ListProducts => product::handle_user_list_products(ctx, msg).await,
        UserRoute::GetProduct => product::handle_user_get_product(ctx, msg).await,
        UserRoute::CreateProduct => product::handle_user_create_product(ctx, msg, input).await,
        UserRoute::UpdateProduct => product::handle_user_update_product(ctx, msg, input).await,
        UserRoute::DeleteProduct => product::handle_user_delete_product(ctx, msg).await,
        UserRoute::DuplicateProduct => product::handle_user_duplicate_product(ctx, msg).await,
        UserRoute::PreviewOffer => commerce::handle_preview(ctx, input).await,
        UserRoute::ListOffers => offers::handle_list(ctx, msg, offers::OfferAccess::Owner).await,
        UserRoute::GetOffer => offers::handle_get(ctx, msg, offers::OfferAccess::Owner).await,
        UserRoute::PreviewManagedOffer => {
            offers::handle_preview(ctx, msg, input, offers::OfferAccess::Owner).await
        }
        UserRoute::CreateOffer => {
            offers::handle_create(ctx, msg, input, offers::OfferAccess::Owner).await
        }
        UserRoute::UpdateOffer => {
            offers::handle_update(ctx, msg, input, offers::OfferAccess::Owner).await
        }
        UserRoute::PublishOffer => {
            offers::handle_publish(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::SyncOffer => offers::handle_sync(ctx, msg, offers::OfferAccess::Owner).await,
        UserRoute::DuplicateOffer => {
            offers::handle_duplicate(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::ArchiveOffer => {
            offers::handle_archive(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::ListPresets => {
            payment_links::list_presets(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::GetPreset => {
            payment_links::get_preset(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::CreatePreset => {
            payment_links::create_preset(ctx, msg, input, offers::OfferAccess::Owner).await
        }
        UserRoute::UpdatePreset => {
            payment_links::update_preset(ctx, msg, input, offers::OfferAccess::Owner).await
        }
        UserRoute::ArchivePreset => {
            payment_links::archive_preset(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::ListPaymentLinks => {
            payment_links::list_links(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::CreatePaymentLink => {
            payment_links::create_link(ctx, msg, input, offers::OfferAccess::Owner).await
        }
        UserRoute::DeactivatePaymentLink => {
            payment_links::deactivate_link(ctx, msg, offers::OfferAccess::Owner).await
        }
        UserRoute::ListGroups => group::handle_user_list_groups(ctx, msg).await,
        UserRoute::GetGroup => group::handle_user_get_group(ctx, msg).await,
        UserRoute::CreateGroup => group::handle_user_create_group(ctx, msg, input).await,
        UserRoute::UpdateGroup => group::handle_user_update_group(ctx, msg, input).await,
        UserRoute::DeleteGroup => group::handle_user_delete_group(ctx, msg).await,
        UserRoute::GroupProducts => group::handle_user_group_products(ctx, msg).await,
        UserRoute::ListTypes => types::handle_list_types(ctx, msg).await,
        UserRoute::GroupTemplates => group::handle_user_list_group_templates(ctx, msg).await,
        UserRoute::Catalog => catalog::handle_catalog(ctx, msg).await,
        UserRoute::CatalogItem => catalog::handle_get_product_public(ctx, msg).await,
        UserRoute::StorefrontWidget => commerce::handle_storefront_widget(),
        UserRoute::StorefrontConfig => commerce::handle_storefront_config(ctx).await,
        UserRoute::StorefrontProduct => commerce::handle_storefront_product(ctx, msg).await,
        UserRoute::GuestOrderStatus => commerce::handle_guest_order_status(ctx, msg).await,
        UserRoute::ListPurchases => purchase::handle_list_user(ctx, msg).await,
        UserRoute::GetPurchase => purchase::handle_get(ctx, msg).await,
        UserRoute::Checkout => stripe::handle_checkout(ctx, msg, input).await,
        UserRoute::Subscription => subscription::handle_subscription(ctx, msg).await,
        UserRoute::SellerAccount => provider::seller_status(ctx, msg).await,
        UserRoute::SellerStats => stats::handle_seller_stats(ctx, msg).await,
        UserRoute::SellerOrders => purchase::handle_list_seller(ctx, msg).await,
        UserRoute::SellerOrder => purchase::handle_get_seller(ctx, msg).await,
        UserRoute::SellerRefund => purchase::handle_seller_refund(ctx, msg, input).await,
        UserRoute::SellerOnboarding => provider::seller_onboarding(ctx, msg, input).await,
        UserRoute::SellerDashboard => provider::seller_dashboard(ctx, msg).await,
        UserRoute::BillingPortal => provider::billing_portal(ctx, msg, input).await,
    }
}
