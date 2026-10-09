/// Versioned integer-minor-unit product offers/prices.
pub(crate) const TABLE: &str = "impresspress__products__offers";

use std::collections::{BTreeMap, HashMap};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{offer_components, products, variables};
use crate::{
    blocks::products::{
        contracts::{
            AmountRule, ApprovalStatus, BillingScheme, CheckoutPolicy, Condition, ManagedOffer,
            Offer, OfferComponent, OfferComponentDraft, OfferDefinitionRequest, OfferMode,
            OfferStatus, OfferSyncStatus, PricingModel, ProductStatus, QuantityRule,
            RecurringInterval, TaxBehavior, UsageType, VariableDefinition, VariableKind,
            VariableVisibility,
        },
        money::normalize_currency,
        offer_pricing,
    },
    db_read::{self, Bound},
    util::{enum_column_or, stamp_created, stamp_updated, wire_str, RecordExt},
};

fn decode_error(entity: &str, id: &str, message: impl std::fmt::Display) -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        format!("invalid persisted {entity} {id}: {message}"),
    )
}

/// The `status` column of an offer row, as the enum that defines it.
///
/// Empty reads as [`OfferStatus::Draft`], which is the column's own
/// `DEFAULT` (`005_commerce_v2.sqlite.sql:80`) and what the fallback string
/// this replaces named.
fn status_of(record: &Record) -> Result<OfferStatus, WaferError> {
    enum_column_or(record, "status", OfferStatus::Draft)
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
        kind: enum_column_or(record, "var_type", VariableKind::Number)?,
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
        visibility: enum_column_or(record, "visibility", VariableVisibility::Public)?,
        sort_order: i32::try_from(record.i64_field("sort_order"))
            .map_err(|error| decode_error("variable", &record.id, error))?,
    })
}

/// Refuse offer variable rows a data-snapshot import carries that no offer
/// writer would store: each offer's rows, decoded exactly as an offer read
/// decodes them, must pass
/// [`offer_pricing::validate_variable_definitions`] — the key grammar,
/// unique keys and non-empty select choices that `validate_offer` applies to
/// every offer create and update.
///
/// The bundle's rows are the offer's whole variable set: the import replaces
/// the destination's rows for every offer it carries rather than merging
/// into them, so these rows are exactly what the offer will hold.
///
/// Import is the one write to the variables table that does not go through
/// [`build_offer`], so without this an offer whose key is `kilo-grams` would
/// land, and every preview and checkout of it would then be refused.
/// `InvalidArgument`, naming the offer, for the first violation.
#[cfg(feature = "block-dev")]
pub(crate) fn validate_imported_variables<'a>(
    rows: impl IntoIterator<Item = &'a serde_json::Map<String, Value>>,
) -> Result<(), WaferError> {
    let refuse = |offer_id: &str, message: &dyn std::fmt::Display| {
        WaferError::new(
            ErrorCode::InvalidArgument,
            format!(
                "the data snapshot carries a variable of offer {offer_id:?} that this build \
                 refuses, so nothing was imported: {message}"
            ),
        )
    };
    let mut by_offer: BTreeMap<String, Vec<VariableDefinition>> = BTreeMap::new();
    for row in rows {
        let record = Record {
            id: row
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            data: row.clone().into_iter().collect(),
        };
        let offer_id = record.str_field("offer_id").to_string();
        let definition =
            variable_from_record(&record).map_err(|error| refuse(&offer_id, &error.message))?;
        by_offer.entry(offer_id).or_default().push(definition);
    }
    for (offer_id, definitions) in &by_offer {
        offer_pricing::validate_variable_definitions(definitions)
            .map_err(|error| refuse(offer_id, &error))?;
    }
    Ok(())
}

/// The ids of every offer of the products in `product_ids` that is not
/// archived — what a data-snapshot import reads to find the offers a product
/// it carries no longer has. The caller bounds `product_ids` to what one
/// statement may bind.
#[cfg(feature = "block-dev")]
pub(crate) async fn unarchived_offer_ids(
    ctx: &dyn Context,
    product_ids: &[String],
) -> Result<Vec<String>, WaferError> {
    let records = db_read::list_every(
        ctx,
        TABLE,
        vec![
            Filter {
                field: "product_id".to_string(),
                operator: FilterOp::In,
                value: Value::from(product_ids.to_vec()),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::NotEqual,
                value: serde_json::json!(OfferStatus::Archived),
            },
        ],
    )
    .await?;
    Ok(records.into_iter().map(|record| record.id).collect())
}

/// The batch write that archives the offers in `offer_ids` inside a
/// data-snapshot import's one batch: the columns [`archive`] writes, on every
/// listed offer that is not archived already (so a row archived since it was
/// read keeps its own stamp). The caller bounds `offer_ids` to what one
/// statement may bind.
///
/// Only the local half of the products block's archive. Its Stripe half
/// (`stripe::archive_offer_catalog`: retire the offer's active Payment Links,
/// deactivate its synced Prices) is one the import could not run: the import
/// exists only in builds with the dev block, which only the browser runtime
/// ships, and there `stripe_secret_operations_allowed` is false, so every
/// Stripe call `archive_offer_catalog` would make is refused. An offer at the
/// destination may still name a Stripe Price — a hand-written bundle can carry
/// `price_…` ids, since the export resets provider linkage and the import does
/// not — but the browser runtime could not archive that Price either way, and
/// no offer there gained a Payment Link, whose creation it also refuses. What
/// the browser can do is what this does: take the offer out of its own
/// catalog.
#[cfg(feature = "block-dev")]
pub(crate) fn archive_offers_write(
    offer_ids: &[String],
) -> wafer_block::wire::database::BatchWrite {
    wafer_block::wire::database::BatchWrite::UpdateWhere {
        collection: TABLE.to_string(),
        filters: crate::util::to_wire_filters(&[
            Filter {
                field: "id".to_string(),
                operator: FilterOp::In,
                value: Value::from(offer_ids.to_vec()),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::NotEqual,
                value: serde_json::json!(OfferStatus::Archived),
            },
        ]),
        data: archived_columns(),
    }
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
        mode: enum_column_or(&record, "mode", OfferMode::Payment)?,
        currency: record.str_field("currency").to_string(),
        pricing_model: enum_column_or(&record, "pricing_model", PricingModel::Fixed)?,
        recurring_interval,
        interval_count,
        usage_type: enum_column_or(&record, "usage_type", UsageType::Licensed)?,
        billing_scheme: enum_column_or(&record, "billing_scheme", BillingScheme::PerUnit)?,
        tax_behavior: enum_column_or(&record, "tax_behavior", TaxBehavior::Unspecified)?,
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
            serde_json::json!(OfferSyncStatus::NotSynced),
        ),
        ("sync_error".to_string(), Value::String(String::new())),
    ]))
}

async fn hydrate_managed(ctx: &dyn Context, record: Record) -> Result<ManagedOffer, WaferError> {
    let status = enum_column_or(&record, "status", OfferStatus::Draft)?;
    let sync_status = enum_column_or(&record, "sync_status", OfferSyncStatus::NotSynced)?;
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
            serde_json::json!(OfferSyncStatus::Syncing),
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
            serde_json::json!(OfferSyncStatus::Synced),
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
            serde_json::json!(OfferSyncStatus::Failed),
        ),
        (
            "sync_error".to_string(),
            Value::String(message.chars().take(500).collect()),
        ),
    ]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, offer_id, data).await.map(|_| ())
}

/// Count the offers matching `filters` (`&[]` for the whole catalog). The
/// admin overview page's "Offers" stat; a failure surfaces as a 500 rather
/// than a fabricated `0`.
pub(crate) async fn count(ctx: &dyn Context, filters: &[Filter]) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, filters).await
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
    let mut records = db_read::list_bounded(
        ctx,
        TABLE,
        vec![product_filter(product_id)],
        Bound::OnePer("offer on one product, authored by hand in the offer editor"),
    )
    .await?;
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
    products::get(ctx, product_id).await?;
    let offer_id = uuid::Uuid::now_v7().to_string();
    let offer = build_offer(&offer_id, product_id, 1, definition)?;
    let mut data = definition_data(&offer)?;
    data.insert("id".to_string(), Value::String(offer_id.clone()));
    data.insert(
        "product_id".to_string(),
        Value::String(product_id.to_string()),
    );
    data.insert("status".to_string(), serde_json::json!(OfferStatus::Draft));
    data.insert(
        "created_by".to_string(),
        Value::String(created_by.to_string()),
    );
    data.insert(
        "stripe_product_id".to_string(),
        Value::String(String::new()),
    );
    // Born fenced: the offer already lists for its product, but its child
    // rows are only written below. `publish` refuses drafts with
    // `draft_updating` raised, so a crash between here and the settle write
    // can never leave a publishable half-created offer.
    data.insert("draft_updating".to_string(), Value::Bool(true));
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
    let mut settle = HashMap::from([("draft_updating".to_string(), Value::Bool(false))]);
    stamp_updated(&mut settle);
    if !update_if_current(ctx, &offer_id, OfferStatus::Draft, Some(0), settle).await? {
        return Err(concurrent_transition());
    }
    get_managed(ctx, &offer_id).await
}

/// Compare-and-swap write: apply `data` only if the offer row still holds
/// `expected_status`. Returns whether the write landed. Every offer state
/// transition writes through this guard — a plain `db::update` would let a
/// write that raced a concurrent transition land on the wrong state (e.g. a
/// stale draft edit wiping `stripe_price_id` on a just-published offer).
pub(crate) async fn update_if_status(
    ctx: &dyn Context,
    offer_id: &str,
    expected_status: OfferStatus,
    data: HashMap<String, Value>,
) -> Result<bool, WaferError> {
    update_if_current(ctx, offer_id, expected_status, None, data).await
}

/// [`update_if_status`] additionally fenced on `draft_revision` when
/// `expected_draft_revision` is given. The revision fence is what extends the
/// single-row CAS to the offer's child tables: `update_draft` advances the
/// revision before replacing variables/components, so a `publish` that CASes
/// on the revision it read and validated can never land over a child set it
/// did not see.
pub(crate) async fn update_if_current(
    ctx: &dyn Context,
    offer_id: &str,
    expected_status: OfferStatus,
    expected_draft_revision: Option<i64>,
    data: HashMap<String, Value>,
) -> Result<bool, WaferError> {
    let mut filters = vec![
        Filter {
            field: "id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(offer_id),
        },
        Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(expected_status),
        },
    ];
    if let Some(draft_revision) = expected_draft_revision {
        filters.push(Filter {
            field: "draft_revision".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(draft_revision),
        });
    }
    let rows = db::update_by_filters_count(ctx, TABLE, filters, data).await?;
    Ok(rows == 1)
}

fn concurrent_transition() -> WaferError {
    WaferError::new(
        ErrorCode::FailedPrecondition,
        "the offer was modified concurrently; reload it and retry",
    )
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
    if status_of(&record)? != OfferStatus::Draft {
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
    let draft_revision = record.i64_field("draft_revision");
    let next_revision = draft_revision
        .checked_add(1)
        .ok_or_else(|| invalid("offer draft revision is too large"))?;

    // Write the offer row first, guarded on the status still being draft and
    // on the draft revision read above: if a concurrent publish or draft
    // update landed since that read, nothing (including the
    // variable/component replacement below) may touch the offer. The same
    // write advances the revision and raises `draft_updating`, opening the
    // fence that keeps `publish` off the offer until the child rows are
    // consistent again.
    let mut data = definition_data(&offer)?;
    data.insert("draft_revision".to_string(), Value::from(next_revision));
    data.insert("draft_updating".to_string(), Value::Bool(true));
    stamp_updated(&mut data);
    if !update_if_current(
        ctx,
        offer_id,
        OfferStatus::Draft,
        Some(draft_revision),
        data,
    )
    .await?
    {
        let current = db::get(ctx, TABLE, offer_id).await?;
        if status_of(&current)? != OfferStatus::Draft {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "active or archived offers are immutable; duplicate the offer to edit it",
            ));
        }
        return Err(concurrent_transition());
    }
    variables::replace_for_offer(ctx, offer_id, &definition.variables).await?;
    offer_components::replace_for_offer(ctx, offer_id, &definition.components).await?;

    // Settle the fence only after every child row is consistent. If this
    // update crashed above, the offer stays a draft with `draft_updating`
    // raised: publish keeps failing cleanly until a retried update completes.
    let mut settle = HashMap::from([("draft_updating".to_string(), Value::Bool(false))]);
    stamp_updated(&mut settle);
    if !update_if_current(
        ctx,
        offer_id,
        OfferStatus::Draft,
        Some(next_revision),
        settle,
    )
    .await?
    {
        return Err(concurrent_transition());
    }
    get_managed(ctx, offer_id).await
}

pub(crate) async fn publish(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
) -> Result<ManagedOffer, WaferError> {
    let record = db::get(ctx, TABLE, offer_id).await?;
    if record.str_field("product_id") != product_id {
        return Err(WaferError::new(ErrorCode::NotFound, "offer not found"));
    }
    let draft_revision = record.i64_field("draft_revision");
    let draft_updating = record.bool_field("draft_updating");
    let managed = hydrate_managed(ctx, record).await?;
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
    if draft_updating {
        // A draft update is replacing this offer's variables/components (or
        // crashed while doing so): the child set read above may be
        // half-replaced and must never become the published version.
        return Err(concurrent_transition());
    }
    offer_pricing::validate_offer(&managed.offer).map_err(|error| invalid(error.to_string()))?;
    let mut data = HashMap::from([("status".to_string(), serde_json::json!(OfferStatus::Active))]);
    stamp_updated(&mut data);
    // CAS draft->active fenced on the exact draft revision that was read and
    // validated: any draft update that started since (which always advances
    // the revision before touching child rows) makes this write miss, so a
    // publish can only ever pin the consistent child set it validated.
    if !update_if_current(
        ctx,
        offer_id,
        OfferStatus::Draft,
        Some(draft_revision),
        data,
    )
    .await?
    {
        // A concurrent transition won the race. Re-read: a concurrent
        // publish converges to the same outcome; anything else is an error.
        let managed = get_managed(ctx, offer_id).await?;
        if managed.status == OfferStatus::Active {
            return Ok(managed);
        }
        return Err(concurrent_transition());
    }
    get_managed(ctx, offer_id).await
}

/// What archiving writes to an offer row: the status, and the update stamp.
/// The offer's variables, components, presets and every order that names it
/// are left as they are, which is what lets an archived offer still be read.
/// [`archive`] and [`archive_offers_write`] both write exactly this.
fn archived_columns() -> HashMap<String, Value> {
    let mut data = HashMap::from([(
        "status".to_string(),
        serde_json::json!(OfferStatus::Archived),
    )]);
    stamp_updated(&mut data);
    data
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
    let data = archived_columns();
    // The offer is archived from whichever state it is in; `managed.status`
    // is the CAS expectation directly, which is what the re-spelled
    // `match` this replaces was reconstructing.
    if !update_if_status(ctx, offer_id, managed.status, data).await? {
        // A concurrent transition won the race. A concurrent archive
        // converges to the same outcome; anything else is an error.
        let managed = get_managed(ctx, offer_id).await?;
        if managed.status == OfferStatus::Archived {
            return Ok(managed);
        }
        return Err(concurrent_transition());
    }
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
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        vec![product_filter(product_id)],
        Bound::OnePer("offer on one product, authored by hand in the offer editor"),
    )
    .await?;
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
    let mut records = db_read::list_bounded(
        ctx,
        TABLE,
        vec![
            product_filter(product_id),
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(OfferStatus::Active),
            },
        ],
        Bound::OnePer("offer on one product, authored by hand in the offer editor"),
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
    if status_of(&record)? != OfferStatus::Active {
        return Err(WaferError::new(ErrorCode::NotFound, "offer not found"));
    }
    let product_id = record.str_field("product_id");
    let product = products::get(ctx, product_id).await?;
    // `products::get` already answers `NotFound` for a soft-deleted row; only
    // `status`/`approval_status` are this function's own rules to enforce.
    //
    // The stored spelling against the variants' own, not a decode: this is the
    // public visibility gate, and a row outside either contract has to stay
    // invisible (404) rather than announce itself with a 500.
    if product.str_field("status") != wire_str(&ProductStatus::Active)
        || product.str_field("approval_status") != wire_str(&ApprovalStatus::Approved)
    {
        return Err(WaferError::new(ErrorCode::NotFound, "offer not found"));
    }
    hydrate(ctx, record).await
}
