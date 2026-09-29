//! Administrator seller governance and product moderation.

use std::collections::HashMap;

use wafer_run::{context::Context, ErrorCode, Message, OutputStream, WaferError};

use crate::{
    blocks::{
        crud,
        products::{
            config::{seller_fee_bps, SELLER_APPLICATION_FEE_BPS},
            contracts::{
                AdminSellerDetail, ApprovalStatus, OfferStatus, ProductStatus, SellerAccountList,
                SellerStatus,
            },
            repo, stripe,
        },
    },
    http::{err_bad_request, err_conflict, err_internal, err_not_found, ok_json},
    util::{enum_column, stamp_updated, wire_str, RecordExt},
};

/// The seller-governance classifications, then [`crud::db_error`] for
/// everything a database raises — which is what makes a WRAP refusal here a
/// 403 rather than the 500 the old `_` arm produced.
fn admin_error(error: WaferError, not_found: &str) -> OutputStream {
    match error.code {
        ErrorCode::InvalidArgument => err_bad_request(&error.message),
        ErrorCode::FailedPrecondition | ErrorCode::Aborted => err_conflict(&error.message),
        _ => crud::db_error(error, not_found, "Seller governance operation failed"),
    }
}

/// The platform fee a published seller account carries, or the 500 an
/// unreadable fee setting is.
///
/// A server fault, not [`admin_error`]'s 409 for a `FailedPrecondition`:
/// nothing about the request is wrong, and retrying it changes nothing.
/// `outcome` says what already happened — on the suspension paths the
/// seller's new state is saved before this is read — and goes into the
/// logged label beside the setting's name; the caller gets the usual
/// sanitized 500 with its correlation id.
async fn platform_fee(ctx: &dyn Context, outcome: &str) -> Result<u16, OutputStream> {
    seller_fee_bps(ctx).await.map_err(|error| {
        err_internal(
            &format!("{outcome}{SELLER_APPLICATION_FEE_BPS} cannot be read"),
            error,
        )
    })
}

/// A stored seller row as the published account, the answer both
/// suspension exits build. The platform fee is read here, after any write,
/// for the reason the raw read in [`set_suspended`] gives: nothing but
/// storage may stop the fraud control itself.
async fn seller_json(
    ctx: &dyn Context,
    account: &wafer_core::clients::database::Record,
) -> OutputStream {
    let fee = match platform_fee(ctx, "Seller state saved, but ").await {
        Ok(fee) => fee,
        Err(response) => return response,
    };
    match repo::seller_accounts::to_contract(account, fee) {
        Ok(seller) => ok_json(&seller),
        Err(error) => admin_error(error, "Seller not found"),
    }
}

pub(super) async fn list(ctx: &dyn Context) -> OutputStream {
    let fee = match platform_fee(ctx, "").await {
        Ok(fee) => fee,
        Err(response) => return response,
    };
    let sellers = match repo::seller_accounts::list_rows(ctx).await {
        Ok(sellers) => sellers.map(|seller| seller.into_contract(fee)),
        Err(error) => return admin_error(error, "Seller not found"),
    };
    // One row per selling user, so the read is capped. `total_count` comes
    // from the database when it is: a count taken off the returned page would
    // report the ceiling as the platform's seller population.
    let total_count = if sellers.truncated {
        match repo::seller_accounts::count_all(ctx).await {
            Ok(total) => total,
            Err(error) => return admin_error(error, "Seller not found"),
        }
    } else {
        sellers.rows.len() as i64
    };
    ok_json(&SellerAccountList {
        sellers: sellers.rows,
        total_count,
        truncated: sellers.truncated,
    })
}

pub(super) async fn get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Seller") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let fee = match platform_fee(ctx, "").await {
        Ok(fee) => fee,
        Err(response) => return response,
    };
    let seller = match repo::seller_accounts::get_row(ctx, id).await {
        Ok(Some(seller)) => seller.into_contract(fee),
        Ok(None) => return err_not_found("Seller not found"),
        Err(error) => return admin_error(error, "Seller not found"),
    };
    let products = match repo::products::list_owned_by(ctx, &seller.user_id).await {
        Ok(products) => products,
        Err(error) => return crud::db_error_internal(error, "Could not list seller products"),
    };
    let truncated = products.truncated;
    let products = match products
        .rows
        .iter()
        .map(crate::blocks::products::contracts::ProductView::from_record)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(products) => products,
        Err(error) => return err_internal("Product row is outside the contract", error),
    };
    ok_json(&AdminSellerDetail {
        seller,
        products,
        truncated,
    })
}

async fn moderate_product(ctx: &dyn Context, msg: &Message, approve: bool) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let product = match repo::products::get(ctx, id).await {
        Ok(product) => product,
        Err(error) => return admin_error(error, "Product not found"),
    };
    if product.str_field("owner_kind") != "user" {
        return err_conflict("Only seller-owned products use moderation");
    }
    // The two columns moderation reads, each through its own enum. They are
    // not one value spelled twice (review bug B11): a listing waiting for a
    // moderator is `status = pending_review` and `approval_status = pending`
    // at the same time, and neither vocabulary accepts the other's spelling.
    let approval = match enum_column::<ApprovalStatus>(&product, "approval_status") {
        Ok(approval) => approval,
        Err(error) => return err_internal("Product row is outside the contract", error),
    };
    let status = match enum_column::<ProductStatus>(&product, "status") {
        Ok(status) => status,
        Err(error) => return err_internal("Product row is outside the contract", error),
    };
    if approve && approval == ApprovalStatus::Approved && status == ProductStatus::Active {
        return super::product_json(&product);
    }
    if !approve && approval == ApprovalStatus::Rejected && status == ProductStatus::Draft {
        return super::product_json(&product);
    }
    if approval != ApprovalStatus::Pending || status != ProductStatus::PendingReview {
        return err_conflict("Product is not waiting for moderation");
    }
    if approve {
        if let Err(error) =
            repo::seller_accounts::ready_for_user(ctx, product.str_field("owner_id")).await
        {
            return admin_error(error, "Seller not found");
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut data = if approve {
        HashMap::from([
            (
                "approval_status".to_string(),
                serde_json::json!(ApprovalStatus::Approved),
            ),
            (
                "status".to_string(),
                serde_json::json!(ProductStatus::Active),
            ),
            ("published_at".to_string(), serde_json::json!(&now)),
        ])
    } else {
        HashMap::from([
            (
                "approval_status".to_string(),
                serde_json::json!(ApprovalStatus::Rejected),
            ),
            (
                "status".to_string(),
                serde_json::json!(ProductStatus::Draft),
            ),
            ("published_at".to_string(), serde_json::json!("")),
        ])
    };
    stamp_updated(&mut data);
    // `update_live`, not the unfiltered write: moderation acts on the row the
    // `get` above found live, and the checks between the two are a window a
    // concurrent delete fits through. Approving a product that has since been
    // deleted would publish a listing (`status = active`) that no read can
    // reach, so the write itself has to test liveness — and `NotFound` is the
    // same answer the `get` would have given a moment earlier.
    match repo::products::update_live(ctx, id, data).await {
        Ok(product) => super::product_json(&product),
        // The shared mapper, so this site cannot drift back to answering 500
        // for a repository refusal that names what the caller must change.
        Err(error) => super::write_error(error, "Could not moderate product"),
    }
}

pub(super) async fn approve_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    moderate_product(ctx, msg, true).await
}

pub(super) async fn reject_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    moderate_product(ctx, msg, false).await
}

async fn set_suspended(ctx: &dyn Context, msg: &Message, suspended: bool) -> OutputStream {
    let id = match crud::path_id(msg, "Seller") {
        Ok(value) => value,
        Err(response) => return response,
    };
    // The stored row, not `get_row`'s decoded one: suspension is a
    // fraud control, and a row whose `status` no longer decodes is exactly
    // the account an operator most needs to be able to suspend. The decode,
    // and the platform-fee read the answer carries, happen where the answer
    // is built, at the end.
    let account = match repo::seller_accounts::get(ctx, id).await {
        Ok(Some(account)) => account,
        Ok(None) => return err_not_found("Seller not found"),
        Err(error) => return admin_error(error, "Seller not found"),
    };
    // The stored spelling against the variant's own, not a decode: the read
    // above deliberately holds the raw row so a seller whose data no longer
    // decodes can still be suspended, and decoding here would put that
    // failure mode back one line later.
    if (account.str_field("status") == wire_str(&SellerStatus::Suspended)) == suspended {
        return seller_json(ctx, &account).await;
    }
    // EVERY product the seller owns, soft-deleted ones included. Deliberately
    // a different read from the catalog listing in `get` above: suspension is
    // a lifecycle and fraud control, so its set is "everything this seller
    // owns" — soft delete changes nothing in Stripe, so a deleted product's
    // Prices and Payment Links keep taking money in the connected account
    // until suspension archives them.
    let user_id = account.str_field("user_id").to_string();
    let products = match repo::products::list_owned_by_including_deleted(ctx, &user_id).await {
        Ok(products) => products,
        Err(error) => return crud::db_error_internal(error, "Could not load seller products"),
    };
    if suspended {
        for product in &products {
            let offers = match repo::offers::list_for_product(ctx, &product.id).await {
                Ok(offers) => offers,
                Err(error) => return admin_error(error, "Seller product not found"),
            };
            for offer in offers {
                if offer.status != OfferStatus::Archived {
                    if let Err(error) =
                        stripe::archive_offer_catalog(ctx, &product.id, &offer.offer.id).await
                    {
                        return admin_error(error, "Seller product not found");
                    }
                }
            }
        }
    }
    for product in products {
        let mut data = if suspended {
            HashMap::from([
                (
                    "approval_status".to_string(),
                    serde_json::json!(ApprovalStatus::Suspended),
                ),
                (
                    "status".to_string(),
                    serde_json::json!(ProductStatus::Archived),
                ),
            ])
            // Again the wire spelling rather than a decode: this loop is the
            // fraud control's compensating write over every row the seller
            // owns, deleted ones included, and one undecodable row must not
            // abort it half-applied.
        } else if product.str_field("approval_status") == wire_str(&ApprovalStatus::Suspended) {
            HashMap::from([
                (
                    "approval_status".to_string(),
                    serde_json::json!(ApprovalStatus::Draft),
                ),
                (
                    "status".to_string(),
                    serde_json::json!(ProductStatus::Draft),
                ),
            ])
        } else {
            continue;
        };
        stamp_updated(&mut data);
        // Deliberately the unfiltered write. The read above spans the
        // deleted rows on purpose (suspension is a fraud control and has to
        // cover everything the seller owns), so filtering the write on
        // liveness here would silently exempt exactly those rows.
        if let Err(error) = repo::products::update_including_deleted(ctx, &product.id, data).await {
            return crud::db_error_internal(error, "Could not update seller product state");
        }
    }
    match repo::seller_accounts::set_admin_suspended(ctx, id, suspended).await {
        Ok(account) => seller_json(ctx, &account).await,
        Err(error) => admin_error(error, "Seller not found"),
    }
}

pub(super) async fn suspend(ctx: &dyn Context, msg: &Message) -> OutputStream {
    set_suspended(ctx, msg, true).await
}

pub(super) async fn reactivate(ctx: &dyn Context, msg: &Message) -> OutputStream {
    set_suspended(ctx, msg, false).await
}
