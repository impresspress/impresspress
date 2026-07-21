//! Administrator-configured constraints for user-owned product catalogs.

use std::collections::{HashMap, HashSet};

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::{config, database as db};
use wafer_run::{context::Context, OutputStream};

use crate::{
    blocks::products::{money, PRODUCTS_TABLE},
    http::{err_bad_request, err_internal},
    util::RecordExt,
};

const TEMPLATES_KEY: &str = "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_TEMPLATES";
const CURRENCIES_KEY: &str = "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES";
const CATEGORIES_KEY: &str = "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CATEGORIES";
const MAX_PRODUCTS_KEY: &str = "IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS";

fn csv_values(raw: &str, uppercase: bool) -> HashSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if uppercase {
                value.to_ascii_uppercase()
            } else {
                value.to_ascii_lowercase()
            }
        })
        .collect()
}

async fn configured_values(ctx: &dyn Context, key: &str, uppercase: bool) -> HashSet<String> {
    csv_values(&config::get_default(ctx, key, "").await, uppercase)
}

pub(crate) async fn allowed_templates(ctx: &dyn Context) -> HashSet<String> {
    configured_values(ctx, TEMPLATES_KEY, false).await
}

pub(crate) async fn allowed_currencies(ctx: &dyn Context) -> HashSet<String> {
    configured_values(ctx, CURRENCIES_KEY, true).await
}

pub(crate) async fn validate_product_fields(
    ctx: &dyn Context,
    data: &HashMap<String, serde_json::Value>,
) -> Result<(), OutputStream> {
    if let Some(template) = data.get("product_template_id") {
        let Some(template) = template.as_str() else {
            return Err(err_bad_request("product_template_id must be a string"));
        };
        let allowed = allowed_templates(ctx).await;
        if !allowed.is_empty() && !allowed.contains(&template.trim().to_ascii_lowercase()) {
            return Err(err_bad_request(
                "This product template is not allowed for sellers",
            ));
        }
    }
    if let Some(currency) = data.get("currency") {
        let Some(currency) = currency.as_str() else {
            return Err(err_bad_request("currency must be a string"));
        };
        validate_currency(ctx, currency).await?;
    }
    if let Some(category) = data.get("category") {
        let Some(category) = category.as_str() else {
            return Err(err_bad_request("category must be a string"));
        };
        let allowed = configured_values(ctx, CATEGORIES_KEY, false).await;
        if !allowed.is_empty()
            && !category.trim().is_empty()
            && !allowed.contains(&category.trim().to_ascii_lowercase())
        {
            return Err(err_bad_request(
                "This product category is not allowed for sellers",
            ));
        }
    }
    Ok(())
}

pub(crate) async fn validate_product_record(
    ctx: &dyn Context,
    product: &db::Record,
) -> Result<(), OutputStream> {
    validate_product_record_with_patch(ctx, product, &HashMap::new()).await
}

/// Validate the restricted seller fields as they will exist after `patch` is
/// applied to `product`. Activation-in-the-same-request must check this
/// merged view — checking the stale pre-update record would wrongly reject a
/// request that fixes a non-compliant field and activates in one call.
pub(crate) async fn validate_product_record_with_patch(
    ctx: &dyn Context,
    product: &db::Record,
    patch: &HashMap<String, serde_json::Value>,
) -> Result<(), OutputStream> {
    let mut merged = HashMap::from([
        (
            "product_template_id".to_string(),
            serde_json::json!(product.str_field("product_template_id")),
        ),
        (
            "currency".to_string(),
            serde_json::json!(product.str_field("currency")),
        ),
        (
            "category".to_string(),
            serde_json::json!(product.str_field("category")),
        ),
    ]);
    for key in ["product_template_id", "currency", "category"] {
        if let Some(value) = patch.get(key) {
            merged.insert(key.to_string(), value.clone());
        }
    }
    validate_product_fields(ctx, &merged).await
}

pub(crate) async fn validate_currency(
    ctx: &dyn Context,
    currency: &str,
) -> Result<(), OutputStream> {
    let currency = money::normalize_currency(currency).map_err(err_bad_request)?;
    let allowed = allowed_currencies(ctx).await;
    if !allowed.is_empty() && !allowed.contains(&currency) {
        return Err(err_bad_request("This currency is not allowed for sellers"));
    }
    Ok(())
}

pub(crate) async fn ensure_product_capacity(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<(), OutputStream> {
    let configured = config::get_default(ctx, MAX_PRODUCTS_KEY, "0").await;
    let limit = match configured.trim().parse::<i64>() {
        Ok(limit) if limit >= 0 => limit,
        Ok(_) | Err(_) => {
            return Err(crate::http::err_internal_no_cause(
                "Seller product limit is misconfigured",
            ))
        }
    };
    if limit == 0 {
        return Ok(());
    }
    let count = db::count(
        ctx,
        PRODUCTS_TABLE,
        &[
            Filter {
                field: "created_by".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(user_id),
            },
            Filter {
                field: "deleted_at".to_string(),
                operator: FilterOp::IsNull,
                value: serde_json::Value::Null,
            },
        ],
    )
    .await
    .map_err(|error| err_internal("Could not enforce seller product limit", error))?;
    if count >= limit {
        return Err(err_bad_request(&format!(
            "Seller product limit reached ({limit}); delete a product or ask an administrator to raise the limit"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::csv_values;

    #[test]
    fn policy_lists_are_trimmed_normalized_and_deduplicated() {
        assert_eq!(csv_values(" usd, NZD,usd ", true).len(), 2);
        assert!(csv_values(" simple_product,Custom ", false).contains("custom"));
    }
}
