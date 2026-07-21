//! Ordered conditional line-item definitions belonging to an offer.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::{AmountRule, OfferComponentDraft},
    util::{stamp_created, stamp_updated, RecordExt},
};

pub(crate) const TABLE: &str = "impresspress__products__offer_components";

fn offer_filter(offer_id: &str) -> Filter {
    Filter {
        field: "offer_id".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(offer_id.to_string()),
    }
}

fn encode<T: Serialize>(value: &T, field: &str) -> Result<Value, WaferError> {
    serde_json::to_string(value)
        .map(Value::String)
        .map_err(|error| {
            WaferError::new(
                ErrorCode::Internal,
                format!("could not encode offer component {field}: {error}"),
            )
        })
}

fn component_type(amount: &AmountRule) -> &'static str {
    match amount {
        AmountRule::Fixed { .. } => "fixed",
        AmountRule::PerUnit { .. } => "per_unit",
        AmountRule::FlatPlusPerUnit { .. } => "flat_plus_per_unit",
        AmountRule::Lookup { .. } => "lookup",
        AmountRule::Graduated { .. } => "graduated",
        AmountRule::Volume { .. } => "volume",
        AmountRule::Package { .. } => "package",
    }
}

fn data_for(
    offer_id: &str,
    component: &OfferComponentDraft,
) -> Result<HashMap<String, Value>, WaferError> {
    Ok(HashMap::from([
        ("offer_id".to_string(), Value::String(offer_id.to_string())),
        (
            "component_key".to_string(),
            Value::String(component.key.clone()),
        ),
        ("label".to_string(), Value::String(component.label.clone())),
        (
            "description".to_string(),
            Value::String(component.description.clone()),
        ),
        ("sort_order".to_string(), Value::from(component.sort_order)),
        ("required".to_string(), Value::Bool(component.required)),
        (
            "component_type".to_string(),
            Value::String(component_type(&component.amount).to_string()),
        ),
        (
            "amount_rule_json".to_string(),
            encode(&component.amount, "amount")?,
        ),
        (
            "quantity_rule_json".to_string(),
            encode(&component.quantity, "quantity")?,
        ),
        (
            "condition_json".to_string(),
            encode(&component.condition, "condition")?,
        ),
        (
            "recurring_json".to_string(),
            match &component.recurrence {
                Some(recurrence) => encode(recurrence, "recurrence")?,
                None => Value::String("{}".to_string()),
            },
        ),
        ("stripe_price_id".to_string(), Value::String(String::new())),
        (
            "metadata".to_string(),
            encode(&component.metadata, "metadata")?,
        ),
    ]))
}

pub(crate) async fn list_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
) -> Result<Vec<Record>, WaferError> {
    db::list_all(ctx, TABLE, vec![offer_filter(offer_id)]).await
}

pub(crate) async fn set_stripe_price_id(
    ctx: &dyn Context,
    component_id: &str,
    stripe_price_id: &str,
) -> Result<(), WaferError> {
    let mut data = HashMap::from([(
        "stripe_price_id".to_string(),
        Value::String(stripe_price_id.to_string()),
    )]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, component_id, data).await.map(|_| ())
}

/// Replace one draft offer's complete component definition while preserving
/// row IDs for component keys that still exist.
pub(crate) async fn replace_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
    components: &[OfferComponentDraft],
) -> Result<(), WaferError> {
    let mut existing: HashMap<String, Record> = list_for_offer(ctx, offer_id)
        .await?
        .into_iter()
        .map(|record| (record.str_field("component_key").to_string(), record))
        .collect();
    let mut seen = HashSet::new();

    for component in components {
        if !seen.insert(component.key.as_str()) {
            return Err(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("duplicate component key {}", component.key),
            ));
        }
        let mut data = data_for(offer_id, component)?;
        if let Some(record) = existing.remove(&component.key) {
            stamp_updated(&mut data);
            db::update(ctx, TABLE, &record.id, data).await?;
        } else {
            data.insert(
                "id".to_string(),
                Value::String(uuid::Uuid::now_v7().to_string()),
            );
            stamp_created(&mut data);
            db::create(ctx, TABLE, data).await?;
        }
    }

    for record in existing.into_values() {
        db::delete(ctx, TABLE, &record.id).await?;
    }
    Ok(())
}

pub(crate) async fn delete_for_offer(ctx: &dyn Context, offer_id: &str) -> Result<(), WaferError> {
    for record in list_for_offer(ctx, offer_id).await? {
        db::delete(ctx, TABLE, &record.id).await?;
    }
    Ok(())
}
