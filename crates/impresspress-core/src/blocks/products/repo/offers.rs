/// Versioned integer-minor-unit product offers/prices.
pub(crate) const TABLE: &str = "impresspress__products__offers";

use std::collections::{BTreeMap, HashMap};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{offer_components, variables};
use crate::{
    blocks::products::{
        contracts::{
            AmountRule, BillingScheme, CheckoutPolicy, Condition, ManagedOffer, Offer,
            OfferComponent, OfferComponentDraft, OfferDefinitionRequest, OfferMode, OfferStatus,
            PricingModel, QuantityRule, RecurringInterval, TaxBehavior, UsageType,
            VariableDefinition, VariableKind, VariableVisibility,
        },
        money::normalize_currency,
        offer_pricing, PRODUCTS_TABLE,
    },
    util::{stamp_created, stamp_updated, RecordExt},
};

fn decode_error(entity: &str, id: &str, message: impl std::fmt::Display) -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        format!("invalid persisted {entity} {id}: {message}"),
    )
}

fn wire_enum<T: DeserializeOwned>(
    record: &Record,
    field: &str,
    fallback: &str,
) -> Result<T, WaferError> {
    let value = record.str_field(field);
    let value = if value.is_empty() { fallback } else { value };
    serde_json::from_value(Value::String(value.to_string()))
        .map_err(|error| decode_error("offer", &record.id, error))
}

fn json_text<T: DeserializeOwned + Default>(
    record: &Record,
    field: &str,
    entity: &str,
) -> Result<T, WaferError> {
    match record.data.get(field) {
        None | Some(Value::Null) => Ok(T::default()),
        Some(Value::String(raw)) if raw.is_empty() => Ok(T::default()),
        Some(Value::String(raw)) => {
            serde_json::from_str(raw).map_err(|error| decode_error(entity, &record.id, error))
        }
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| decode_error(entity, &record.id, error)),
    }
}

fn required_json<T: DeserializeOwned>(
    record: &Record,
    field: &str,
    entity: &str,
) -> Result<T, WaferError> {
    match record.data.get(field) {
        Some(Value::String(raw)) => {
            serde_json::from_str(raw).map_err(|error| decode_error(entity, &record.id, error))
        }
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| decode_error(entity, &record.id, error)),
        None => Err(decode_error(entity, &record.id, format!("missing {field}"))),
    }
}

fn empty_json_field(record: &Record, field: &str) -> bool {
    match record.data.get(field) {
        None | Some(Value::Null) => true,
        Some(Value::String(raw)) => raw.is_empty() || raw == "{}",
        Some(Value::Object(map)) => map.is_empty(),
        _ => false,
    }
}

fn optional_text(record: &Record, field: &str) -> Option<String> {
    record
        .data
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn default_value(record: &Record) -> Option<Value> {
    match record.data.get("default_value") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) => {
            Some(serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone())))
        }
        Some(value) => Some(value.clone()),
    }
}

fn variable_from_record(record: &Record) -> Result<VariableDefinition, WaferError> {
    let label =
        optional_text(record, "label").unwrap_or_else(|| record.str_field("name").to_string());
    Ok(VariableDefinition {
        key: record.str_field("name").to_string(),
        kind: wire_enum::<VariableKind>(record, "var_type", "number")?,
        label,
        help_text: record.str_field("help_text").to_string(),
        required: record.bool_field("required"),
        default_value: default_value(record),
        allowed_values: json_text(record, "allowed_values", "variable")?,
        minimum: optional_text(record, "minimum_value"),
        maximum: optional_text(record, "maximum_value"),
        step: optional_text(record, "step_value"),
        maximum_length: record
            .data
            .get("maximum_length")
            .and_then(Value::as_i64)
            .map(usize::try_from)
            .transpose()
            .map_err(|error| decode_error("variable", &record.id, error))?,
        visibility: wire_enum::<VariableVisibility>(record, "visibility", "public")?,
        sort_order: i32::try_from(record.i64_field("sort_order"))
            .map_err(|error| decode_error("variable", &record.id, error))?,
    })
}

fn component_from_record(record: &Record) -> Result<OfferComponent, WaferError> {
    let condition = if empty_json_field(record, "condition_json") {
        Condition::Always
    } else {
        required_json(record, "condition_json", "offer component")?
    };
    let recurrence = if empty_json_field(record, "recurring_json") {
        None
    } else {
        Some(required_json(record, "recurring_json", "offer component")?)
    };
    Ok(OfferComponent {
        id: record.id.clone(),
        key: record.str_field("component_key").to_string(),
        label: record.str_field("label").to_string(),
        description: record.str_field("description").to_string(),
        sort_order: i32::try_from(record.i64_field("sort_order"))
            .map_err(|error| decode_error("offer component", &record.id, error))?,
        required: record.bool_field("required"),
        amount: required_json::<AmountRule>(record, "amount_rule_json", "offer component")?,
        quantity: json_text::<QuantityRule>(record, "quantity_rule_json", "offer component")?,
        condition,
        recurrence,
        stripe_price_id: record.str_field("stripe_price_id").to_string(),
        metadata: json_text::<BTreeMap<String, Value>>(record, "metadata", "offer component")?,
    })
}

async fn hydrate(ctx: &dyn Context, record: Record) -> Result<Offer, WaferError> {
    let mut variable_records = variables::list_for_offer(ctx, &record.id).await?;
    variable_records.sort_by_key(|record| record.i64_field("sort_order"));
    let variables = variable_records
        .iter()
        .map(variable_from_record)
        .collect::<Result<Vec<_>, _>>()?;

    let mut component_records = offer_components::list_for_offer(ctx, &record.id).await?;
    component_records.sort_by(|left, right| {
        left.i64_field("sort_order")
            .cmp(&right.i64_field("sort_order"))
            .then_with(|| {
                left.str_field("component_key")
                    .cmp(right.str_field("component_key"))
            })
    });
    let components = component_records
        .iter()
        .map(component_from_record)
        .collect::<Result<Vec<_>, _>>()?;

    let recurring_interval = optional_text(&record, "recurring_interval")
        .map(|value| {
            serde_json::from_value::<RecurringInterval>(Value::String(value))
                .map_err(|error| decode_error("offer", &record.id, error))
        })
        .transpose()?;
    let version = u32::try_from(record.i64_field("version"))
        .map_err(|error| decode_error("offer", &record.id, error))?;
    let interval_count = u32::try_from(record.i64_field("interval_count"))
        .map_err(|error| decode_error("offer", &record.id, error))?;

    Ok(Offer {
        id: record.id.clone(),
        product_id: record.str_field("product_id").to_string(),
        version,
        name: record.str_field("name").to_string(),
        mode: wire_enum::<OfferMode>(&record, "mode", "payment")?,
        currency: record.str_field("currency").to_string(),
        pricing_model: wire_enum::<PricingModel>(&record, "pricing_model", "fixed")?,
        recurring_interval,
        interval_count,
        usage_type: wire_enum::<UsageType>(&record, "usage_type", "licensed")?,
        billing_scheme: wire_enum::<BillingScheme>(&record, "billing_scheme", "per_unit")?,
        tax_behavior: wire_enum::<TaxBehavior>(&record, "tax_behavior", "unspecified")?,
        variables,
        components,
        checkout: json_text::<CheckoutPolicy>(&record, "config_json", "offer")?,
        stripe_product_id: record.str_field("stripe_product_id").to_string(),
        stripe_price_id: record.str_field("stripe_price_id").to_string(),
    })
}

fn invalid(message: impl Into<String>) -> WaferError {
    WaferError::new(ErrorCode::InvalidArgument, message)
}

fn encode<T: Serialize>(value: &T, field: &str) -> Result<Value, WaferError> {
    serde_json::to_string(value)
        .map(Value::String)
        .map_err(|error| {
            WaferError::new(
                ErrorCode::Internal,
                format!("could not encode offer {field}: {error}"),
            )
        })
}

fn wire<T: Serialize>(value: &T, field: &str) -> Result<Value, WaferError> {
    match serde_json::to_value(value) {
        Ok(Value::String(value)) => Ok(Value::String(value)),
        Ok(_) => Err(WaferError::new(
            ErrorCode::Internal,
            format!("offer {field} did not serialize as a wire string"),
        )),
        Err(error) => Err(WaferError::new(
            ErrorCode::Internal,
            format!("could not encode offer {field}: {error}"),
        )),
    }
}

fn product_filter(product_id: &str) -> Filter {
    Filter {
        field: "product_id".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(product_id.to_string()),
    }
}

fn build_offer(
    id: &str,
    product_id: &str,
    version: u32,
    definition: &OfferDefinitionRequest,
) -> Result<Offer, WaferError> {
    if definition.name.trim().is_empty() {
        return Err(invalid("offer name is required"));
    }
    if definition
        .variables
        .iter()
        .any(|variable| variable.key.trim().is_empty() || variable.label.trim().is_empty())
    {
        return Err(invalid("variable keys and labels are required"));
    }
    if definition
        .components
        .iter()
        .any(|component| component.key.trim().is_empty() || component.label.trim().is_empty())
    {
        return Err(invalid("component keys and labels are required"));
    }
    if matches!(definition.pricing_model, PricingModel::Fixed)
        && (definition.components.len() != 1
            || !matches!(definition.components[0].amount, AmountRule::Fixed { .. }))
    {
        return Err(invalid(
            "fixed pricing requires exactly one fixed-amount component",
        ));
    }

    let offer = Offer {
        id: id.to_string(),
        product_id: product_id.to_string(),
        version,
        name: definition.name.trim().to_string(),
        mode: definition.mode,
        currency: normalize_currency(&definition.currency).map_err(invalid)?,
        pricing_model: definition.pricing_model,
        recurring_interval: definition.recurring_interval,
        interval_count: definition.interval_count,
        usage_type: definition.usage_type,
        billing_scheme: definition.billing_scheme,
        tax_behavior: definition.tax_behavior,
        variables: definition.variables.clone(),
        components: definition
            .components
            .iter()
            .map(|component| OfferComponent {
                id: format!("{id}:{}", component.key),
                key: component.key.clone(),
                label: component.label.clone(),
                description: component.description.clone(),
                sort_order: component.sort_order,
                required: component.required,
                amount: component.amount.clone(),
                quantity: component.quantity.clone(),
                condition: component.condition.clone(),
                recurrence: component.recurrence.clone(),
                stripe_price_id: String::new(),
                metadata: component.metadata.clone(),
            })
            .collect(),
        checkout: definition.checkout.clone(),
        stripe_product_id: String::new(),
        stripe_price_id: String::new(),
    };
    offer_pricing::validate_offer(&offer).map_err(|error| invalid(error.to_string()))?;
    Ok(offer)
}

fn definition_data(offer: &Offer) -> Result<HashMap<String, Value>, WaferError> {
    let unit_amount_minor = if matches!(offer.pricing_model, PricingModel::Fixed) {
        match offer.components.first().map(|component| &component.amount) {
            Some(AmountRule::Fixed { unit_amount_minor }) => *unit_amount_minor,
            _ => 0,
        }
    } else {
        0
    };
    Ok(HashMap::from([
        ("version".to_string(), Value::from(offer.version)),
        ("name".to_string(), Value::String(offer.name.clone())),
        ("mode".to_string(), wire(&offer.mode, "mode")?),
        (
            "currency".to_string(),
            Value::String(offer.currency.clone()),
        ),
        (
            "pricing_model".to_string(),
            wire(&offer.pricing_model, "pricing_model")?,
        ),
        (
            "unit_amount_minor".to_string(),
            Value::from(unit_amount_minor),
        ),
        (
            "recurring_interval".to_string(),
            match offer.recurring_interval {
                Some(interval) => wire(&interval, "recurring_interval")?,
                None => Value::String(String::new()),
            },
        ),
        (
            "interval_count".to_string(),
            Value::from(offer.interval_count),
        ),
        (
            "usage_type".to_string(),
            wire(&offer.usage_type, "usage_type")?,
        ),
        (
            "billing_scheme".to_string(),
            wire(&offer.billing_scheme, "billing_scheme")?,
        ),
        (
            "tax_behavior".to_string(),
            wire(&offer.tax_behavior, "tax_behavior")?,
        ),
        (
            "trial_days".to_string(),
            Value::from(offer.checkout.trial_days),
        ),
        (
            "config_json".to_string(),
            encode(&offer.checkout, "checkout policy")?,
        ),
        ("stripe_price_id".to_string(), Value::String(String::new())),
        (
            "sync_status".to_string(),
            Value::String("not_synced".to_string()),
        ),
        ("sync_error".to_string(), Value::String(String::new())),
    ]))
}

async fn hydrate_managed(ctx: &dyn Context, record: Record) -> Result<ManagedOffer, WaferError> {
    let status = wire_enum::<OfferStatus>(&record, "status", "draft")?;
    let sync_status = record.str_field("sync_status").to_string();
    let sync_error = record.str_field("sync_error").to_string();
    Ok(ManagedOffer {
        status,
        sync_status,
        sync_error,
        offer: hydrate(ctx, record).await?,
    })
}

pub(crate) async fn get_managed(
    ctx: &dyn Context,
    offer_id: &str,
) -> Result<ManagedOffer, WaferError> {
    hydrate_managed(ctx, db::get(ctx, TABLE, offer_id).await?).await
}

pub(crate) async fn mark_syncing(ctx: &dyn Context, offer_id: &str) -> Result<(), WaferError> {
    let mut data = HashMap::from([
        (
            "sync_status".to_string(),
            Value::String("syncing".to_string()),
        ),
        ("sync_error".to_string(), Value::String(String::new())),
    ]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, offer_id, data).await.map(|_| ())
}

pub(crate) async fn mark_synced(
    ctx: &dyn Context,
    offer_id: &str,
    stripe_product_id: &str,
    stripe_price_id: &str,
) -> Result<ManagedOffer, WaferError> {
    let mut data = HashMap::from([
        (
            "sync_status".to_string(),
            Value::String("synced".to_string()),
        ),
        ("sync_error".to_string(), Value::String(String::new())),
        (
            "stripe_product_id".to_string(),
            Value::String(stripe_product_id.to_string()),
        ),
        (
            "stripe_price_id".to_string(),
            Value::String(stripe_price_id.to_string()),
        ),
    ]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, offer_id, data).await?;
    get_managed(ctx, offer_id).await
}

pub(crate) async fn mark_sync_error(
    ctx: &dyn Context,
    offer_id: &str,
    message: &str,
) -> Result<(), WaferError> {
    let mut data = HashMap::from([
        (
            "sync_status".to_string(),
            Value::String("failed".to_string()),
        ),
        (
            "sync_error".to_string(),
            Value::String(message.chars().take(500).collect()),
        ),
    ]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, offer_id, data).await.map(|_| ())
}

pub(crate) async fn get_for_product(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
) -> Result<ManagedOffer, WaferError> {
    let record = db::get(ctx, TABLE, offer_id).await?;
    if record.str_field("product_id") != product_id {
        return Err(WaferError::new(ErrorCode::NotFound, "offer not found"));
    }
    hydrate_managed(ctx, record).await
}

pub(crate) async fn list_for_product(
    ctx: &dyn Context,
    product_id: &str,
) -> Result<Vec<ManagedOffer>, WaferError> {
    let mut records = db::list_all(ctx, TABLE, vec![product_filter(product_id)]).await?;
    records.sort_by(|left, right| {
        left.str_field("name")
            .cmp(right.str_field("name"))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut offers = Vec::with_capacity(records.len());
    for record in records {
        offers.push(hydrate_managed(ctx, record).await?);
    }
    Ok(offers)
}

async fn cleanup_new(ctx: &dyn Context, offer_id: &str) {
    if let Err(error) = offer_components::delete_for_offer(ctx, offer_id).await {
        tracing::error!(offer_id, error = %error, "could not compensate offer components");
    }
    if let Err(error) = variables::delete_for_offer(ctx, offer_id).await {
        tracing::error!(offer_id, error = %error, "could not compensate offer variables");
    }
    if let Err(error) = db::delete(ctx, TABLE, offer_id).await {
        tracing::error!(offer_id, error = %error, "could not compensate offer row");
    }
}

pub(crate) async fn create(
    ctx: &dyn Context,
    product_id: &str,
    created_by: &str,
    definition: &OfferDefinitionRequest,
) -> Result<ManagedOffer, WaferError> {
    db::get(ctx, PRODUCTS_TABLE, product_id).await?;
    let offer_id = uuid::Uuid::now_v7().to_string();
    let offer = build_offer(&offer_id, product_id, 1, definition)?;
    let mut data = definition_data(&offer)?;
    data.insert("id".to_string(), Value::String(offer_id.clone()));
    data.insert(
        "product_id".to_string(),
        Value::String(product_id.to_string()),
    );
    data.insert("status".to_string(), Value::String("draft".to_string()));
    data.insert(
        "created_by".to_string(),
        Value::String(created_by.to_string()),
    );
    data.insert(
        "stripe_product_id".to_string(),
        Value::String(String::new()),
    );
    stamp_created(&mut data);
    db::create(ctx, TABLE, data).await?;

    if let Err(error) = variables::replace_for_offer(ctx, &offer_id, &definition.variables).await {
        cleanup_new(ctx, &offer_id).await;
        return Err(error);
    }
    if let Err(error) =
        offer_components::replace_for_offer(ctx, &offer_id, &definition.components).await
    {
        cleanup_new(ctx, &offer_id).await;
        return Err(error);
    }
    get_managed(ctx, &offer_id).await
}

pub(crate) async fn update_draft(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
    definition: &OfferDefinitionRequest,
) -> Result<ManagedOffer, WaferError> {
    let record = db::get(ctx, TABLE, offer_id).await?;
    if record.str_field("product_id") != product_id {
        return Err(WaferError::new(ErrorCode::NotFound, "offer not found"));
    }
    if record.str_field("status") != "draft" {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "active or archived offers are immutable; duplicate the offer to edit it",
        ));
    }
    let version = u32::try_from(record.i64_field("version"))
        .map_err(|error| decode_error("offer", offer_id, error))?
        .checked_add(1)
        .ok_or_else(|| invalid("offer version is too large"))?;
    let offer = build_offer(offer_id, product_id, version, definition)?;

    variables::replace_for_offer(ctx, offer_id, &definition.variables).await?;
    offer_components::replace_for_offer(ctx, offer_id, &definition.components).await?;
    let mut data = definition_data(&offer)?;
    stamp_updated(&mut data);
    db::update(ctx, TABLE, offer_id, data).await?;
    get_managed(ctx, offer_id).await
}

pub(crate) async fn publish(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
) -> Result<ManagedOffer, WaferError> {
    let managed = get_for_product(ctx, product_id, offer_id).await?;
    match managed.status {
        OfferStatus::Active => return Ok(managed),
        OfferStatus::Archived => {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "archived offers cannot be published",
            ));
        }
        OfferStatus::Draft => {}
    }
    offer_pricing::validate_offer(&managed.offer).map_err(|error| invalid(error.to_string()))?;
    let mut data = HashMap::from([("status".to_string(), Value::String("active".to_string()))]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, offer_id, data).await?;
    get_managed(ctx, offer_id).await
}

pub(crate) async fn archive(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
) -> Result<ManagedOffer, WaferError> {
    let managed = get_for_product(ctx, product_id, offer_id).await?;
    if managed.status == OfferStatus::Archived {
        return Ok(managed);
    }
    let mut data = HashMap::from([("status".to_string(), Value::String("archived".to_string()))]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, offer_id, data).await?;
    get_managed(ctx, offer_id).await
}

pub(crate) async fn duplicate(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
    created_by: &str,
) -> Result<ManagedOffer, WaferError> {
    let source = get_for_product(ctx, product_id, offer_id).await?.offer;
    create(ctx, product_id, created_by, &definition_from_offer(source)).await
}

fn definition_from_offer(source: Offer) -> OfferDefinitionRequest {
    OfferDefinitionRequest {
        name: format!("{} copy", source.name),
        mode: source.mode,
        currency: source.currency,
        pricing_model: source.pricing_model,
        recurring_interval: source.recurring_interval,
        interval_count: source.interval_count,
        usage_type: source.usage_type,
        billing_scheme: source.billing_scheme,
        tax_behavior: source.tax_behavior,
        variables: source.variables,
        components: source
            .components
            .into_iter()
            .map(|component| OfferComponentDraft {
                key: component.key,
                label: component.label,
                description: component.description,
                sort_order: component.sort_order,
                required: component.required,
                amount: component.amount,
                quantity: component.quantity,
                condition: component.condition,
                recurrence: component.recurrence,
                metadata: component.metadata,
            })
            .collect(),
        checkout: source.checkout,
    }
}

/// Copy every non-archived offer to another product as a mutable draft.
/// Provider IDs, checkout presets, and Payment Links deliberately stay with
/// the immutable source product/version.
pub(crate) async fn duplicate_for_product(
    ctx: &dyn Context,
    source_product_id: &str,
    target_product_id: &str,
    created_by: &str,
) -> Result<Vec<ManagedOffer>, WaferError> {
    let source_offers = list_for_product(ctx, source_product_id).await?;
    let mut duplicated = Vec::new();
    for managed in source_offers {
        if managed.status == OfferStatus::Archived {
            continue;
        }
        let mut definition = definition_from_offer(managed.offer);
        definition.name = definition
            .name
            .strip_suffix(" copy")
            .unwrap_or(&definition.name)
            .to_string();
        duplicated.push(create(ctx, target_product_id, created_by, &definition).await?);
    }
    Ok(duplicated)
}

/// Compensate a failed whole-product duplication before the target becomes
/// observable. This is intentionally scoped to a freshly-created target.
pub(crate) async fn delete_for_product(
    ctx: &dyn Context,
    product_id: &str,
) -> Result<(), WaferError> {
    let records = db::list_all(ctx, TABLE, vec![product_filter(product_id)]).await?;
    for record in records {
        offer_components::delete_for_offer(ctx, &record.id).await?;
        variables::delete_for_offer(ctx, &record.id).await?;
        db::delete(ctx, TABLE, &record.id).await?;
    }
    Ok(())
}

pub(crate) async fn list_public_for_product(
    ctx: &dyn Context,
    product_id: &str,
) -> Result<Vec<Offer>, WaferError> {
    let mut records = db::list_all(
        ctx,
        TABLE,
        vec![
            product_filter(product_id),
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: Value::String("active".to_string()),
            },
        ],
    )
    .await?;
    records.sort_by(|left, right| {
        left.str_field("name")
            .cmp(right.str_field("name"))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut offers = Vec::with_capacity(records.len());
    for record in records {
        offers.push(hydrate(ctx, record).await?);
    }
    Ok(offers)
}
/// Load only an offer whose own state and parent product are publicly
/// purchasable. This prevents preview responses from leaking draft, rejected,
/// suspended, archived, or soft-deleted seller configurations.
pub(crate) async fn get_public(ctx: &dyn Context, offer_id: &str) -> Result<Offer, WaferError> {
    let record = db::get(ctx, TABLE, offer_id).await?;
    if record.str_field("status") != "active" {
        return Err(WaferError::new(ErrorCode::NotFound, "offer not found"));
    }
    let product_id = record.str_field("product_id");
    let product = db::get(ctx, PRODUCTS_TABLE, product_id).await?;
    let deleted = product
        .data
        .get("deleted_at")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
    if product.str_field("status") != "active"
        || product.str_field("approval_status") != "approved"
        || deleted
    {
        return Err(WaferError::new(ErrorCode::NotFound, "offer not found"));
    }
    hydrate(ctx, record).await
}
