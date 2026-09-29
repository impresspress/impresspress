//! Typed offer lifecycle handlers shared by administrators and seller owners.

use wafer_core::clients::database::Record;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};

use super::seller_policy;
use crate::{
    blocks::{
        crud,
        products::{
            contracts::{OfferDefinitionRequest, PricingPreviewRequest},
            offer_pricing,
            repo::{offers, products},
            stripe,
        },
    },
    http::{err_bad_request, err_conflict, err_not_found, err_unauthenticated, ok_json},
};

#[derive(Clone, Copy)]
pub(super) enum OfferAccess {
    Admin,
    Owner,
}

/// Whether an offer operation requires its product to still be live.
///
/// The default for anything reachable from a UI is [`Live`](Self::Live): a
/// soft-deleted product answers 404 everywhere, and restore is the door back
/// in. [`LiveOrDeleted`](Self::LiveOrDeleted) is the deliberate exception for
/// the operations that *close* a money surface, and the reads that enumerate
/// what there is to close.
///
/// It exists because soft delete touches nothing in Stripe. A deleted
/// product's Prices and Payment Links stay live in the connected account and
/// keep taking money, and the delete handler archives none of them — so an
/// admin or owner has to be able to shut that surface down *without* first
/// restoring the listing to the public catalog. Refusing to let them was not
/// a safe default; it was a money-taking surface with no off switch.
///
/// `pages::deleted_product_close` is the UI that reaches it — the close-only
/// manager the admin Deleted view links to. Widening these four operations
/// without shipping that page left the whole thing dead code, reachable only
/// by an operator who already knew every id, while the one affordance the
/// Deleted view did offer (Restore) is the one that puts the product back in
/// front of customers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProductState {
    /// The product must still be live. Everything that creates, edits,
    /// publishes, duplicates, syncs, or opens a new money surface.
    Live,
    /// The product may be soft-deleted. Only for operations that exclusively
    /// remove things from the live Stripe catalog, and the listings that name
    /// them.
    LiveOrDeleted,
}

/// The `{product_id}` segment, read only as the route table bound it.
/// Unguarded: [`verify_product`] is the door for the product, and every
/// caller of this runs it first.
pub(super) fn product_id(msg: &Message) -> &str {
    msg.var("product_id")
}

/// The `{offer_id}` segment, or the 400 an unbound segment turns into.
pub(super) fn offer_id(msg: &Message) -> Result<&str, OutputStream> {
    crud::path_var(msg, "offer_id", "Missing offer ID")
}

pub(super) async fn verify_product(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
    state: ProductState,
) -> Result<Record, OutputStream> {
    let product_id = crud::path_var(msg, "product_id", "Missing product ID")?;
    let loaded = match state {
        ProductState::Live => products::get(ctx, product_id).await,
        ProductState::LiveOrDeleted => products::get_including_deleted(ctx, product_id).await,
    };
    let product = match loaded {
        Ok(product) => product,
        Err(error) => {
            return Err(crud::db_error(
                error,
                "Product not found",
                "Could not load product",
            ))
        }
    };
    if matches!(access, OfferAccess::Owner) {
        let user_id = msg.user_id();
        if user_id.is_empty() {
            return Err(err_unauthenticated("Not authenticated"));
        }
        // The shared rule, so the offer routes and the product CRUD routes
        // cannot disagree about who owns the same row again.
        if !super::is_owned_by(&product, user_id) {
            return Err(err_not_found("Product not found"));
        }
    }
    Ok(product)
}

/// Map an offer-lifecycle failure onto a response.
///
/// The first three arms are the offer domain's own classifications, each
/// carrying the message that says what to change. The tail is a database
/// failure and goes through the one door, so a WRAP refusal on
/// `impresspress__products__offers` is the 403 it is rather than the 500 it
/// used to be, and a quota keeps its 429. `db_error_internal` rather than
/// `db_error` because the `NotFound` arm above already claims the caller's
/// 404.
pub(super) fn domain_error(error: WaferError) -> OutputStream {
    match error.code {
        ErrorCode::NotFound => err_not_found("Offer not found"),
        ErrorCode::InvalidArgument => err_bad_request(&error.message),
        ErrorCode::FailedPrecondition | ErrorCode::Aborted => err_conflict(&error.message),
        _ => crud::db_error_internal(error, "Offer operation failed"),
    }
}

async fn definition(input: InputStream) -> Result<OfferDefinitionRequest, OutputStream> {
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    serde_json::from_slice(&raw).map_err(|error| err_bad_request(&format!("Invalid body: {error}")))
}

pub(super) async fn handle_list(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    // Widened: this is how an admin or owner finds the offers of a product
    // they have deleted in order to archive them. A pure read, scoped to the
    // caller's own product (or an admin's), that adds nothing to the money
    // surface it exists to close.
    if let Err(response) = verify_product(ctx, msg, access, ProductState::LiveOrDeleted).await {
        return response;
    }
    match offers::list_for_product(ctx, product_id(msg)).await {
        Ok(offers) => ok_json(&serde_json::json!({"offers": offers})),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_get(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access, ProductState::Live).await {
        return response;
    }
    let offer_id = match offer_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match offers::get_for_product(ctx, product_id(msg), offer_id).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

/// Preview any offer owned by the selected product, including an unpublished
/// draft. Public previews intentionally remain restricted to active offers;
/// this owner-scoped route gives builders the same authoritative evaluator
/// without exposing draft definitions or trusting browser-calculated totals.
pub(super) async fn handle_preview(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access, ProductState::Live).await {
        return response;
    }
    let route_offer_id = match offer_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let mut request: PricingPreviewRequest = match serde_json::from_slice(&raw) {
        Ok(request) => request,
        Err(error) => return err_bad_request(&format!("Invalid body: {error}")),
    };
    if !request.offer_id.is_empty() && request.offer_id != route_offer_id {
        return err_bad_request("Preview offer ID does not match the route");
    }
    request.offer_id = route_offer_id.to_string();
    let managed = match offers::get_for_product(ctx, product_id(msg), route_offer_id).await {
        Ok(offer) => offer,
        Err(error) => return domain_error(error),
    };
    match offer_pricing::evaluate_offer(
        &managed.offer,
        &request,
        offer_pricing::InputScope::Management,
    ) {
        Ok(preview) => ok_json(&preview),
        Err(error) => err_bad_request(&format!("{}: {}", error.code, error)),
    }
}

pub(super) async fn handle_create(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access, ProductState::Live).await {
        return response;
    }
    let definition = match definition(input).await {
        Ok(definition) => definition,
        Err(response) => return response,
    };
    if matches!(access, OfferAccess::Owner) {
        if let Err(response) = seller_policy::validate_currency(ctx, &definition.currency).await {
            return response;
        }
    }
    match offers::create(ctx, product_id(msg), msg.user_id(), &definition).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_update(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access, ProductState::Live).await {
        return response;
    }
    let offer_id = match offer_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let definition = match definition(input).await {
        Ok(definition) => definition,
        Err(response) => return response,
    };
    if matches!(access, OfferAccess::Owner) {
        if let Err(response) = seller_policy::validate_currency(ctx, &definition.currency).await {
            return response;
        }
    }
    match offers::update_draft(ctx, product_id(msg), offer_id, &definition).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_publish(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    let product = match verify_product(ctx, msg, access, ProductState::Live).await {
        Ok(product) => product,
        Err(response) => return response,
    };
    let offer_id = match offer_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if matches!(access, OfferAccess::Owner) {
        if let Err(response) = seller_policy::validate_product_record(ctx, &product).await {
            return response;
        }
        let offer = match offers::get_for_product(ctx, product_id(msg), offer_id).await {
            Ok(offer) => offer,
            Err(error) => return domain_error(error),
        };
        if let Err(response) = seller_policy::validate_currency(ctx, &offer.offer.currency).await {
            return response;
        }
    }
    match offers::publish(ctx, product_id(msg), offer_id).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_sync(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access, ProductState::Live).await {
        return response;
    }
    let offer_id = match offer_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match stripe::sync_offer_catalog(ctx, product_id(msg), offer_id).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_duplicate(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access, ProductState::Live).await {
        return response;
    }
    let offer_id = match offer_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match offers::duplicate(ctx, product_id(msg), offer_id, msg.user_id()).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_archive(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    // Widened: archival is the off switch. `archive_offer_catalog`
    // deactivates every active Payment Link on the offer and takes its Prices
    // out of the live Stripe catalog — it only ever removes, so it can never
    // expose a deleted product to anyone, and it is exactly what a deleted
    // product most needs.
    if let Err(response) = verify_product(ctx, msg, access, ProductState::LiveOrDeleted).await {
        return response;
    }
    let offer_id = match offer_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match stripe::archive_offer_catalog(ctx, product_id(msg), offer_id).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}
