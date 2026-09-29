//! Checkout preset and reusable Stripe Payment Link management.

use wafer_core::clients::database::Record;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::offers::{self, OfferAccess, ProductState};
use crate::{
    blocks::{
        crud,
        products::{
            contracts::{
                CheckoutPresetList, CheckoutPresetRequest, PaymentLinkCreateRequest,
                PaymentLinkList,
            },
            repo::{checkout_presets, offers as offer_repo, payment_links},
            stripe,
        },
    },
    http::{err_bad_request, ok_json},
};

/// The `{preset_id}` segment, or the 400 an unbound segment turns into.
fn preset_id(msg: &Message) -> Result<&str, OutputStream> {
    crud::path_var(msg, "preset_id", "Missing preset ID")
}

/// What a failed preset create or update answers.
///
/// Migration 005's `impresspress__products__checkout_presets_slug_uniq` —
/// `(offer_id, slug)`, partial on `slug <> ''` — is the presets table's one
/// caller-chosen unique key, and both writes set the request's slug on the
/// offer the route names. So a refused duplicate is that slug being taken on
/// this offer; a request with an empty slug cannot collide at all.
fn preset_write_error(
    error: wafer_run::WaferError,
    request: &CheckoutPresetRequest,
) -> OutputStream {
    if request.slug.is_empty() {
        return offers::domain_error(error);
    }
    crud::taken_key_or(
        error,
        crud::TakenKey::new("checkout preset", "slug", &request.slug),
        offers::domain_error,
    )
}

/// The `{link_id}` segment, or the 400 an unbound segment turns into.
fn link_id(msg: &Message) -> Result<&str, OutputStream> {
    crud::path_var(msg, "link_id", "Missing payment link ID")
}

/// The product and the `{offer_id}` the route named, after the ownership
/// check and after proving the offer belongs to that product.
///
/// Returning the id is what keeps every caller from re-reading it: the
/// binding is guarded exactly once, here.
async fn authorized_offer<'m>(
    ctx: &dyn Context,
    msg: &'m Message,
    access: OfferAccess,
    state: ProductState,
) -> Result<(Record, &'m str), OutputStream> {
    let product = offers::verify_product(ctx, msg, access, state).await?;
    let offer_id = offers::offer_id(msg)?;
    offer_repo::get_for_product(ctx, &product.id, offer_id)
        .await
        .map_err(offers::domain_error)?;
    Ok((product, offer_id))
}

async fn body<T: serde::de::DeserializeOwned>(input: InputStream) -> Result<T, OutputStream> {
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    serde_json::from_slice(&raw).map_err(|error| err_bad_request(&format!("Invalid body: {error}")))
}

pub(super) async fn list_presets(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    let (_product, offer_id) = match authorized_offer(ctx, msg, access, ProductState::Live).await {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    match checkout_presets::list_for_offer(ctx, offer_id).await {
        Ok(presets) => ok_json(&CheckoutPresetList { presets }),
        Err(error) => offers::domain_error(error),
    }
}

pub(super) async fn get_preset(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    let (_product, offer_id) = match authorized_offer(ctx, msg, access, ProductState::Live).await {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let preset_id = match preset_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match checkout_presets::get_for_offer(ctx, offer_id, preset_id).await {
        Ok(preset) => ok_json(&preset),
        Err(error) => offers::domain_error(error),
    }
}

pub(super) async fn create_preset(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    access: OfferAccess,
) -> OutputStream {
    let (_product, offer_id) = match authorized_offer(ctx, msg, access, ProductState::Live).await {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let request: CheckoutPresetRequest = match body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match checkout_presets::create(ctx, offer_id, msg.user_id(), &request).await {
        Ok(preset) => ok_json(&preset),
        Err(error) => preset_write_error(error, &request),
    }
}

pub(super) async fn update_preset(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    access: OfferAccess,
) -> OutputStream {
    let (_product, offer_id) = match authorized_offer(ctx, msg, access, ProductState::Live).await {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let preset_id = match preset_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let request: CheckoutPresetRequest = match body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match checkout_presets::update(ctx, offer_id, preset_id, &request).await {
        Ok(preset) => ok_json(&preset),
        Err(error) => preset_write_error(error, &request),
    }
}

pub(super) async fn archive_preset(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    let (_product, offer_id) = match authorized_offer(ctx, msg, access, ProductState::Live).await {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let preset_id = match preset_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match checkout_presets::archive(ctx, offer_id, preset_id).await {
        Ok(preset) => ok_json(&preset),
        Err(error) => offers::domain_error(error),
    }
}

pub(super) async fn list_links(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    // Widened for the same reason as the offer listing: naming the links of a
    // deleted product is how the caller finds the ones still taking money.
    let (_product, offer_id) =
        match authorized_offer(ctx, msg, access, ProductState::LiveOrDeleted).await {
            Ok(authorized) => authorized,
            Err(response) => return response,
        };
    match payment_links::list_for_offer(ctx, offer_id).await {
        Ok(links) => ok_json(&PaymentLinkList {
            payment_links: links,
        }),
        Err(error) => offers::domain_error(error),
    }
}

pub(super) async fn create_link(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    access: OfferAccess,
) -> OutputStream {
    let (product, offer_id) = match authorized_offer(ctx, msg, access, ProductState::Live).await {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let request: PaymentLinkCreateRequest = match body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match stripe::create_payment_link(ctx, &product, offer_id, &request).await {
        Ok(link) => ok_json(&link),
        Err(error) => offers::domain_error(error),
    }
}

pub(super) async fn deactivate_link(
    ctx: &dyn Context,
    msg: &Message,
    access: OfferAccess,
) -> OutputStream {
    // Widened: deactivation only ever closes. `create_link` above deliberately
    // stays live-only — opening a new way to charge for a deleted product is
    // the opposite operation.
    let (_product, offer_id) =
        match authorized_offer(ctx, msg, access, ProductState::LiveOrDeleted).await {
            Ok(authorized) => authorized,
            Err(response) => return response,
        };
    let link_id = match link_id(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    match stripe::deactivate_payment_link(ctx, offer_id, link_id).await {
        Ok(link) => ok_json(&link),
        Err(error) => offers::domain_error(error),
    }
}
