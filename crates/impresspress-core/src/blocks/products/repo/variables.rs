//! Typed offer input definitions.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::VariableDefinition,
    util::{stamp_created, stamp_updated, RecordExt},
};

pub(crate) const TABLE: &str = "impresspress__products__variables";

fn offer_filter(offer_id: &str) -> Filter {
    Filter {
        field: "offer_id".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(offer_id.to_string()),
    }
}

fn encode<T: Serialize>(value: &T, field: &str) -> Result<String, WaferError> {
    serde_json::to_string(value).map_err(|error| {
        WaferError::new(
            ErrorCode::Internal,
            format!("could not encode offer variable {field}: {error}"),
        )
    })
}

fn wire<T: Serialize>(value: &T, field: &str) -> Result<Value, WaferError> {
    match serde_json::to_value(value) {
        Ok(Value::String(value)) => Ok(Value::String(value)),
        Ok(_) => Err(WaferError::new(
            ErrorCode::Internal,
            format!("offer variable {field} did not serialize as a wire string"),
        )),
        Err(error) => Err(WaferError::new(
            ErrorCode::Internal,
            format!("could not encode offer variable {field}: {error}"),
        )),
    }
}

fn data_for(
    offer_id: &str,
    definition: &VariableDefinition,
) -> Result<HashMap<String, Value>, WaferError> {
    let maximum_length = definition
        .maximum_length
        .map(i64::try_from)
        .transpose()
        .map_err(|_| {
            WaferError::new(
                ErrorCode::InvalidArgument,
                "variable maximum_length is too large",
            )
        })?;
    Ok(HashMap::from([
        ("offer_id".to_string(), Value::String(offer_id.to_string())),
        ("name".to_string(), Value::String(definition.key.clone())),
        ("var_type".to_string(), wire(&definition.kind, "kind")?),
        ("label".to_string(), Value::String(definition.label.clone())),
        (
            "help_text".to_string(),
            Value::String(definition.help_text.clone()),
        ),
        ("required".to_string(), Value::Bool(definition.required)),
        (
            "default_value".to_string(),
            match &definition.default_value {
                Some(value) => Value::String(encode(value, "default_value")?),
                None => Value::Null,
            },
        ),
        (
            "allowed_values".to_string(),
            Value::String(encode(&definition.allowed_values, "allowed_values")?),
        ),
        (
            "minimum_value".to_string(),
            definition
                .minimum
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        ),
        (
            "maximum_value".to_string(),
            definition
                .maximum
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        ),
        (
            "step_value".to_string(),
            definition
                .step
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        ),
        (
            "maximum_length".to_string(),
            maximum_length.map(Value::from).unwrap_or(Value::Null),
        ),
        (
            "visibility".to_string(),
            wire(&definition.visibility, "visibility")?,
        ),
        ("sort_order".to_string(), Value::from(definition.sort_order)),
    ]))
}

pub(crate) async fn list_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
) -> Result<Vec<Record>, WaferError> {
    db::list_all(ctx, TABLE, vec![offer_filter(offer_id)]).await
}

/// Replace one draft offer's complete variable definition while preserving
/// row IDs for keys that still exist.
pub(crate) async fn replace_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
    definitions: &[VariableDefinition],
) -> Result<(), WaferError> {
    let mut existing: HashMap<String, Record> = list_for_offer(ctx, offer_id)
        .await?
        .into_iter()
        .map(|record| (record.str_field("name").to_string(), record))
        .collect();
    let mut seen = HashSet::new();

    for definition in definitions {
        if !seen.insert(definition.key.as_str()) {
            return Err(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("duplicate variable key {}", definition.key),
            ));
        }
        let mut data = data_for(offer_id, definition)?;
        if let Some(record) = existing.remove(&definition.key) {
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
