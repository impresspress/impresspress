//! The one fan-out from a matched [`Route`] to its handler.
//!
//! `ProductsBlock::handle` matches the wire path against `routes::ROUTES`,
//! applies the route's rate-limit bucket and gates, and calls [`run`] with
//! the variant. Nothing here reads a path: every id a handler needs was
//! bound into `req.param.*` by the matcher, and the page handlers that take
//! an id receive the matcher's already-decoded binding.

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    catalog, commerce, group, offers, payment_links, product, provider, sellers, stats,
    subscription, types,
};
use crate::{
    blocks::products::{pages, purchase, routes::Route, stripe},
    config_vars::ALLOW_USER_PRODUCTS_KEY,
};

/// Whether `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` is on — the flag behind
/// `routes::user_products_refusal`. Visible at `crate::blocks::products`
/// (re-exported as `handlers::user_products_enabled`) so the admin Overview
/// page (`pages::overview`) can render an accurate notice instead of a silent
/// empty catalog when it's off.
pub(in crate::blocks::products) async fn user_products_enabled(
    ctx: &dyn Context,
) -> Result<bool, wafer_run::WaferError> {
    crate::config_vars::get_bool(ctx, ALLOW_USER_PRODUCTS_KEY, false).await
}

/// Run the handler `route` names.
pub(in crate::blocks::products) async fn run(
    ctx: &dyn Context,
    msg: &Message,
    route: Route,
    input: InputStream,
) -> OutputStream {
    use offers::OfferAccess::{Admin, Owner};

    match route {
        // ── Admin JSON API ──
        Route::AdminListProducts => product::handle_list_products(ctx, msg).await,
        Route::AdminCreateProduct => product::handle_create_product(ctx, msg, input).await,
        Route::AdminGetProduct => product::handle_get_product(ctx, msg).await,
        Route::AdminUpdateProduct => product::handle_update_product(ctx, msg, input).await,
        Route::AdminDeleteProduct => product::handle_delete_product(ctx, msg).await,
        Route::AdminDuplicateProduct => product::handle_duplicate_product(ctx, msg).await,
        Route::AdminApproveProduct => sellers::approve_product(ctx, msg).await,
        Route::AdminRejectProduct => sellers::reject_product(ctx, msg).await,
        Route::AdminRestoreProduct => product::handle_restore_product(ctx, msg).await,
        Route::AdminListOffers => offers::handle_list(ctx, msg, Admin).await,
        Route::AdminCreateOffer => offers::handle_create(ctx, msg, input, Admin).await,
        Route::AdminGetOffer => offers::handle_get(ctx, msg, Admin).await,
        Route::AdminPreviewOffer => offers::handle_preview(ctx, msg, input, Admin).await,
        Route::AdminUpdateOffer => offers::handle_update(ctx, msg, input, Admin).await,
        Route::AdminPublishOffer => offers::handle_publish(ctx, msg, Admin).await,
        Route::AdminSyncOffer => offers::handle_sync(ctx, msg, Admin).await,
        Route::AdminDuplicateOffer => offers::handle_duplicate(ctx, msg, Admin).await,
        Route::AdminArchiveOffer => offers::handle_archive(ctx, msg, Admin).await,
        Route::AdminListPresets => payment_links::list_presets(ctx, msg, Admin).await,
        Route::AdminCreatePreset => payment_links::create_preset(ctx, msg, input, Admin).await,
        Route::AdminGetPreset => payment_links::get_preset(ctx, msg, Admin).await,
        Route::AdminUpdatePreset => payment_links::update_preset(ctx, msg, input, Admin).await,
        Route::AdminArchivePreset => payment_links::archive_preset(ctx, msg, Admin).await,
        Route::AdminListPaymentLinks => payment_links::list_links(ctx, msg, Admin).await,
        Route::AdminCreatePaymentLink => payment_links::create_link(ctx, msg, input, Admin).await,
        Route::AdminDeactivatePaymentLink => payment_links::deactivate_link(ctx, msg, Admin).await,
        Route::AdminListGroups => group::handle_list_groups(ctx, msg).await,
        Route::AdminCreateGroup => group::handle_create_group(ctx, msg, input).await,
        Route::AdminUpdateGroup => group::handle_update_group(ctx, msg, input).await,
        Route::AdminDeleteGroup => group::handle_delete_group(ctx, msg).await,
        Route::AdminListTypes => types::handle_list_types(ctx, msg).await,
        Route::AdminCreateType => types::handle_create_type(ctx, msg, input).await,
        Route::AdminDeleteType => types::handle_delete_type(ctx, msg).await,
        Route::AdminListPurchases => purchase::handle_list_admin(ctx, msg).await,
        Route::AdminGetPurchase => purchase::handle_get_admin(ctx, msg).await,
        Route::AdminRefundPurchase => purchase::handle_refund(ctx, msg, input).await,
        Route::AdminStats => stats::handle_stats(ctx, msg).await,
        Route::AdminStripeStatus => provider::connection_status(ctx).await,
        Route::AdminWebhookEvents => provider::webhook_events(ctx, msg).await,
        Route::AdminReplayWebhookEvent => provider::replay_webhook_event(ctx, msg).await,
        Route::AdminProviderOperations => provider::provider_operations(ctx, msg).await,
        Route::AdminReconcileProviderOperations => {
            provider::reconcile_provider_operations(ctx, msg).await
        }
        Route::AdminListSellers => sellers::list(ctx).await,
        Route::AdminGetSeller => sellers::get(ctx, msg).await,
        Route::AdminSuspendSeller => sellers::suspend(ctx, msg).await,
        Route::AdminReactivateSeller => sellers::reactivate(ctx, msg).await,

        // ── Seller JSON API ──
        Route::ListOwnProducts => product::handle_user_list_products(ctx, msg).await,
        Route::CreateOwnProduct => product::handle_user_create_product(ctx, msg, input).await,
        Route::GetOwnProduct => product::handle_user_get_product(ctx, msg).await,
        Route::UpdateOwnProduct => product::handle_user_update_product(ctx, msg, input).await,
        Route::DeleteOwnProduct => product::handle_user_delete_product(ctx, msg).await,
        Route::RestoreOwnProduct => product::handle_user_restore_product(ctx, msg).await,
        Route::DuplicateOwnProduct => product::handle_user_duplicate_product(ctx, msg).await,
        Route::ListOwnOffers => offers::handle_list(ctx, msg, Owner).await,
        Route::CreateOwnOffer => offers::handle_create(ctx, msg, input, Owner).await,
        Route::GetOwnOffer => offers::handle_get(ctx, msg, Owner).await,
        Route::PreviewOwnOffer => offers::handle_preview(ctx, msg, input, Owner).await,
        Route::UpdateOwnOffer => offers::handle_update(ctx, msg, input, Owner).await,
        Route::PublishOwnOffer => offers::handle_publish(ctx, msg, Owner).await,
        Route::SyncOwnOffer => offers::handle_sync(ctx, msg, Owner).await,
        Route::DuplicateOwnOffer => offers::handle_duplicate(ctx, msg, Owner).await,
        Route::ArchiveOwnOffer => offers::handle_archive(ctx, msg, Owner).await,
        Route::ListOwnPresets => payment_links::list_presets(ctx, msg, Owner).await,
        Route::CreateOwnPreset => payment_links::create_preset(ctx, msg, input, Owner).await,
        Route::GetOwnPreset => payment_links::get_preset(ctx, msg, Owner).await,
        Route::UpdateOwnPreset => payment_links::update_preset(ctx, msg, input, Owner).await,
        Route::ArchiveOwnPreset => payment_links::archive_preset(ctx, msg, Owner).await,
        Route::ListOwnPaymentLinks => payment_links::list_links(ctx, msg, Owner).await,
        Route::CreateOwnPaymentLink => payment_links::create_link(ctx, msg, input, Owner).await,
        Route::DeactivateOwnPaymentLink => payment_links::deactivate_link(ctx, msg, Owner).await,
        Route::SellerAccount => provider::seller_status(ctx, msg).await,
        Route::SellerStats => stats::handle_seller_stats(ctx, msg).await,
        Route::SellerOrders => purchase::handle_list_seller(ctx, msg).await,
        Route::SellerOrder => purchase::handle_get_seller(ctx, msg).await,
        Route::SellerRefund => purchase::handle_seller_refund(ctx, msg, input).await,
        Route::SellerOnboarding => provider::seller_onboarding(ctx, msg, input).await,
        Route::SellerDashboard => provider::seller_dashboard(ctx, msg).await,

        // ── User and public JSON API ──
        Route::ListOwnGroups => group::handle_user_list_groups(ctx, msg).await,
        Route::CreateOwnGroup => group::handle_user_create_group(ctx, msg, input).await,
        Route::GetOwnGroup => group::handle_user_get_group(ctx, msg).await,
        Route::UpdateOwnGroup => group::handle_user_update_group(ctx, msg, input).await,
        Route::DeleteOwnGroup => group::handle_user_delete_group(ctx, msg).await,
        Route::OwnGroupProducts => group::handle_user_group_products(ctx, msg).await,
        Route::ListTypes => types::handle_list_types(ctx, msg).await,
        Route::ListGroupTemplates => group::handle_user_list_group_templates(ctx, msg).await,
        Route::Catalog => catalog::handle_catalog(ctx, msg).await,
        Route::CatalogItem => catalog::handle_get_product_public(ctx, msg).await,
        Route::StorefrontWidget => commerce::handle_storefront_widget(),
        Route::StorefrontConfig => commerce::handle_storefront_config(ctx).await,
        Route::StorefrontProduct => commerce::handle_storefront_product(ctx, msg).await,
        Route::GuestOrderStatus => commerce::handle_guest_order_status(ctx, msg).await,
        Route::PricingPreview => commerce::handle_preview(ctx, input).await,
        Route::ListPurchases => purchase::handle_list_user(ctx, msg).await,
        Route::GetPurchase => purchase::handle_get(ctx, msg).await,
        Route::Checkout => stripe::handle_checkout(ctx, msg, input).await,
        Route::Subscription => subscription::handle_subscription(ctx, msg).await,
        Route::BillingPortal => provider::billing_portal(ctx, msg, input).await,

        // ── Stripe webhook ──
        Route::Webhook => stripe::handle_webhook(ctx, msg, input).await,

        // ── SSR pages ──
        Route::PortalHome => pages::portal_home(ctx, msg).await,
        Route::MyProductsPage => pages::my_products(ctx, msg).await,
        Route::NewProductPage => pages::product_wizard(ctx, msg, false).await,
        Route::MyProductPage => pages::product_manager(ctx, msg, msg.var("id"), false).await,
        Route::MyProductClosePage => {
            pages::deleted_product_close(ctx, msg, msg.var("id"), false).await
        }
        Route::MyPurchasesPage => pages::my_purchases(ctx, msg).await,
        Route::MyPurchasePage => pages::my_purchase_detail(ctx, msg, msg.var("id")).await,
        Route::SellingPage => pages::seller_dashboard(ctx, msg).await,
        Route::SellingOrdersPage => pages::seller_orders(ctx, msg).await,
        Route::SellingOrderPage => pages::seller_order_detail(ctx, msg, msg.var("id")).await,
        Route::AdminOverview => pages::overview(ctx, msg).await,
        Route::AdminManagePage => pages::manage_products(ctx, msg).await,
        Route::AdminNewProductPage => pages::product_wizard(ctx, msg, true).await,
        Route::AdminProductPage => pages::product_manager(ctx, msg, msg.var("id"), true).await,
        Route::AdminProductClosePage => {
            pages::deleted_product_close(ctx, msg, msg.var("id"), true).await
        }
        Route::AdminGroupsPage => pages::groups(ctx, msg).await,
        Route::AdminPurchasesPage => pages::purchases(ctx, msg).await,
        Route::AdminPurchasePage => pages::admin_purchase_detail(ctx, msg, msg.var("id")).await,
        Route::AdminSellersPage => pages::admin_sellers(ctx, msg).await,
        Route::AdminSellerPage => pages::admin_seller_detail(ctx, msg, msg.var("id")).await,
        Route::AdminStripePage => pages::stripe_setup(ctx, msg).await,
        Route::AdminSettingsPage => pages::settings(ctx, msg).await,
        Route::AdminSaveSettings => pages::handle_save_settings(ctx, msg, input).await,
    }
}
