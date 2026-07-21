//! Named, validated offer-input presets used by reusable static checkout links.

use std::collections::HashMap;

use serde_json::Value;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::offers;
use crate::{
    blocks::products::{
        contracts::{CheckoutPreset, CheckoutPresetRequest, PricingPreviewRequest},
        offer_pricing,
    },
    util::RecordExt,
};

pub(crate) const TABLE: &str = "impresspress__products__checkout_presets";

fn invalid(message: impl Into<String>) -> WaferError {
    WaferError::new(ErrorCode::InvalidArgument, message)
}

fn decode_inputs(record: &Record) -> Result<std::collections::BTreeMap<String, Value>, WaferError> {
    match record.data.get("inputs_json") {
        None | Some(Value::Null) => Ok(Default::default()),
        Some(Value::String(raw)) => serde_json::from_str(raw),
        Some(value) => serde_json::from_value(value.clone()),
    }
    .map_err(|error| {
        WaferError::new(
            ErrorCode::Internal,
            format!("invalid persisted checkout preset {}: {error}", record.id),
        )
    })
}

fn hydrate(record: Record) -> Result<CheckoutPreset, WaferError> {
    Ok(CheckoutPreset {
        id: record.id.clone(),
        offer_id: record.str_field("offer_id").to_string(),
        name: record.str_field("name").to_string(),
        slug: record.str_field("slug").to_string(),
        inputs: decode_inputs(&record)?,
        active: record.bool_field("active"),
        configuration_hash: record.str_field("configuration_hash").to_string(),
    })
}

fn validate_slug(slug: &str) -> Result<(), WaferError> {
    if !slug.is_empty()
        && (slug.len() > 100
            || slug.starts_with('-')
            || slug.ends_with('-')
            || !slug
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
    {
        return Err(invalid(
            "preset slug must contain lowercase letters, numbers, and internal hyphens only",
        ));
    }
    Ok(())
}

async fn validated_data(
    ctx: &dyn Context,
    offer_id: &str,
    request: &CheckoutPresetRequest,
) -> Result<(HashMap<String, Value>, String), WaferError> {
    if request.name.trim().is_empty() || request.name.len() > 120 {
        return Err(invalid(
            "preset name is required and must be at most 120 characters",
        ));
    }
    validate_slug(&request.slug)?;
    let managed = offers::get_managed(ctx, offer_id).await?;
    let preview = offer_pricing::evaluate_offer(
        &managed.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.to_string(),
            quantity: 1,
            inputs: request.inputs.clone(),
        },
        // Presets are authored on owner/admin routes and may deliberately pin
        // hidden or admin-only variables for a curated checkout link.
        offer_pricing::InputScope::Management,
    )
    .map_err(|error| invalid(format!("invalid preset inputs: {error}")))?;
    let canonical = serde_json::to_string(&serde_json::json!({
        "offer_id": offer_id,
        "offer_version": managed.offer.version,
        "inputs": preview.inputs,
    }))
    .map_err(|error| {
        WaferError::new(
            ErrorCode::Internal,
            format!("could not encode preset configuration: {error}"),
        )
    })?;
    let configuration_hash = wafer_block::hash::sha256_hex(canonical.as_bytes());
    let inputs_json = serde_json::to_string(&preview.inputs).map_err(|error| {
        WaferError::new(
            ErrorCode::Internal,
            format!("could not encode preset inputs: {error}"),
        )
    })?;
    Ok((
        HashMap::from([
            (
                "name".to_string(),
                Value::String(request.name.trim().to_string()),
            ),
            ("slug".to_string(), Value::String(request.slug.clone())),
            ("inputs_json".to_string(), Value::String(inputs_json)),
            (
                "configuration_hash".to_string(),
                Value::String(configuration_hash.clone()),
            ),
        ]),
        configuration_hash,
    ))
}

pub(crate) async fn create(
    ctx: &dyn Context,
    offer_id: &str,
    created_by: &str,
    request: &CheckoutPresetRequest,
) -> Result<CheckoutPreset, WaferError> {
    let (mut data, _) = validated_data(ctx, offer_id, request).await?;
    data.insert("offer_id".to_string(), Value::String(offer_id.to_string()));
    data.insert("active".to_string(), Value::Bool(true));
    data.insert(
        "created_by".to_string(),
        Value::String(created_by.to_string()),
    );
    let now = chrono::Utc::now().to_rfc3339();
    data.insert("created_at".to_string(), Value::String(now.clone()));
    data.insert("updated_at".to_string(), Value::String(now));
    hydrate(db::create(ctx, TABLE, data).await?)
}

pub(crate) async fn update(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
    request: &CheckoutPresetRequest,
) -> Result<CheckoutPreset, WaferError> {
    let existing = get_for_offer(ctx, offer_id, preset_id).await?;
    if !existing.active {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "archived presets are immutable",
        ));
    }
    let (mut data, _) = validated_data(ctx, offer_id, request).await?;
    data.insert(
        "updated_at".to_string(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    hydrate(db::update(ctx, TABLE, preset_id, data).await?)
}

pub(crate) async fn get_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
) -> Result<CheckoutPreset, WaferError> {
    let record = db::get(ctx, TABLE, preset_id).await?;
    if record.str_field("offer_id") != offer_id {
        return Err(WaferError::new(ErrorCode::NotFound, "preset not found"));
    }
    hydrate(record)
}

pub(crate) async fn get_active(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
) -> Result<CheckoutPreset, WaferError> {
    let preset = get_for_offer(ctx, offer_id, preset_id).await?;
    if !preset.active {
        return Err(WaferError::new(ErrorCode::NotFound, "preset not found"));
    }
    Ok(preset)
}

pub(crate) async fn list_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
) -> Result<Vec<CheckoutPreset>, WaferError> {
    let mut rows = db::list_all(
        ctx,
        TABLE,
        vec![Filter {
            field: "offer_id".to_string(),
            operator: FilterOp::Equal,
            value: Value::String(offer_id.to_string()),
        }],
    )
    .await?;
    rows.sort_by(|left, right| {
        left.str_field("name")
            .cmp(right.str_field("name"))
            .then_with(|| left.id.cmp(&right.id))
    });
    rows.into_iter().map(hydrate).collect()
}

pub(crate) async fn archive(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
) -> Result<CheckoutPreset, WaferError> {
    get_for_offer(ctx, offer_id, preset_id).await?;
    hydrate(
        db::update(
            ctx,
            TABLE,
            preset_id,
            HashMap::from([
                ("active".to_string(), Value::Bool(false)),
                (
                    "updated_at".to_string(),
                    Value::String(chrono::Utc::now().to_rfc3339()),
                ),
            ]),
        )
        .await?,
    )
}
