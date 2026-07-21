//! Typed offer lifecycle handlers shared by administrators and seller owners.

use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};

use super::seller_policy;
use crate::{
    blocks::products::{
        contracts::{OfferDefinitionRequest, PricingPreviewRequest},
        offer_pricing,
        repo::offers,
        stripe, PRODUCTS_TABLE,
    },
    http::{err_bad_request, err_conflict, err_internal, err_not_found, err_unauthorized, ok_json},
    util::RecordExt,
};

#[derive(Clone, Copy)]
pub(super) enum OfferAccess {
    Admin,
    Owner,
}

pub(super) fn product_id(msg: &Message) -> &str {
    msg.var("product_id")
}

pub(super) fn offer_id(msg: &Message) -> &str {
    msg.var("offer_id")
}

pub(super) async fn verify_product(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> Result<Record, OutputStream> {
    let product_id = product_id(msg);
    if product_id.is_empty() {
        return Err(err_bad_request("Missing product ID"));
    }
    let product = match db::get(ctx, PRODUCTS_TABLE, product_id).await {
        Ok(product) => product,
        Err(error) if error.code == ErrorCode::NotFound => {
            return Err(err_not_found("Product not found"));
        }
        Err(error) => return Err(err_internal("Could not load product", error)),
    };
    if matches!(access, OfferAccess::Owner) {
        let user_id = msg.user_id();
        if user_id.is_empty() {
            return Err(err_unauthorized("Not authenticated"));
        }
        let owner_id = product.str_field("owner_id");
        let created_by = product.str_field("created_by");
        if owner_id != user_id && created_by != user_id {
            return Err(err_not_found("Product not found"));
        }
    }
    Ok(product)
}

pub(super) fn domain_error(error: WaferError) -> OutputStream {
    match error.code {
        ErrorCode::NotFound => err_not_found("Offer not found"),
        ErrorCode::InvalidArgument => err_bad_request(&error.message),
        ErrorCode::FailedPrecondition | ErrorCode::Aborted => err_conflict(&error.message),
        _ => err_internal("Offer operation failed", error),
    }
}

async fn definition(input: InputStream) -> Result<OfferDefinitionRequest, OutputStream> {
    let raw = input.collect_to_bytes().await;
    serde_json::from_slice(&raw).map_err(|error| err_bad_request(&format!("Invalid body: {error}")))
}

pub(super) async fn handle_list(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access).await {
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
    if let Err(response) = verify_product(ctx, msg, access).await {
        return response;
    }
    if offer_id(msg).is_empty() {
        return err_bad_request("Missing offer ID");
    }
    match offers::get_for_product(ctx, product_id(msg), offer_id(msg)).await {
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
    if let Err(response) = verify_product(ctx, msg, access).await {
        return response;
    }
    let route_offer_id = offer_id(msg);
    if route_offer_id.is_empty() {
        return err_bad_request("Missing offer ID");
    }
    let raw = input.collect_to_bytes().await;
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
    if let Err(response) = verify_product(ctx, msg, access).await {
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
    if let Err(response) = verify_product(ctx, msg, access).await {
        return response;
    }
    if offer_id(msg).is_empty() {
        return err_bad_request("Missing offer ID");
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
    match offers::update_draft(ctx, product_id(msg), offer_id(msg), &definition).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_publish(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    let product = match verify_product(ctx, msg, access).await {
        Ok(product) => product,
        Err(response) => return response,
    };
    if offer_id(msg).is_empty() {
        return err_bad_request("Missing offer ID");
    }
    if matches!(access, OfferAccess::Owner) {
        if let Err(response) = seller_policy::validate_product_record(ctx, &product).await {
            return response;
        }
        let offer = match offers::get_for_product(ctx, product_id(msg), offer_id(msg)).await {
            Ok(offer) => offer,
            Err(error) => return domain_error(error),
        };
        if let Err(response) = seller_policy::validate_currency(ctx, &offer.offer.currency).await {
            return response;
        }
    }
    match offers::publish(ctx, product_id(msg), offer_id(msg)).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_sync(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access).await {
        return response;
    }
    if offer_id(msg).is_empty() {
        return err_bad_request("Missing offer ID");
    }
    match stripe::sync_offer_catalog(ctx, product_id(msg), offer_id(msg)).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_duplicate(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access).await {
        return response;
    }
    if offer_id(msg).is_empty() {
        return err_bad_request("Missing offer ID");
    }
    match offers::duplicate(ctx, product_id(msg), offer_id(msg), msg.user_id()).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}

pub(super) async fn handle_archive(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    if let Err(response) = verify_product(ctx, msg, access).await {
        return response;
    }
    if offer_id(msg).is_empty() {
        return err_bad_request("Missing offer ID");
    }
    match stripe::archive_offer_catalog(ctx, product_id(msg), offer_id(msg)).await {
        Ok(offer) => ok_json(&offer),
        Err(error) => domain_error(error),
    }
}
