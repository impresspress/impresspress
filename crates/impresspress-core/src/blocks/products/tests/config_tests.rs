use wafer_run::InputType;

use crate::blocks::products::config_vars;

fn var(key: &str) -> wafer_run::ConfigVar {
    config_vars()
        .into_iter()
        .find(|var| var.key == key)
        .unwrap_or_else(|| panic!("missing products config var {key}"))
}

#[test]
fn stripe_api_version_is_explicit_and_stable() {
    let version = var("IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION");
    assert_eq!(version.default, "2026-02-25.clover");
    assert_eq!(version.input_type, InputType::Text);
}

#[test]
fn publishable_key_is_declared_but_masked_in_admin_surfaces() {
    let publishable = var("IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY");
    assert!(publishable.optional);
    assert_eq!(publishable.input_type, InputType::Password);
}

#[test]
fn commerce_policy_defaults_are_safe() {
    let automatic_tax = var("IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX");
    assert_eq!(automatic_tax.default, "false");
    assert_eq!(automatic_tax.input_type, InputType::Toggle);
    let country = var("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY");
    assert!(country.optional);
    assert!(country.default.is_empty());

    let fee = var("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS");
    assert_eq!(fee.default, "0");

    let moderation = var("IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED");
    assert_eq!(moderation.default, "true");
    assert_eq!(moderation.input_type, InputType::Toggle);

    let origins = var("IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS");
    assert!(origins.optional);
    assert!(origins.default.is_empty());
}

#[tokio::test]
async fn runtime_kind_is_adapter_injected_not_shared_config() {
    use super::harness::ctx_with;

    // The runtime marker is an internal synthetic key (adapter-injected,
    // never env/DB): a value persisted under the legacy shared name must not
    // affect Stripe secret-operation gating, and the key itself must follow
    // the double-underscore internal convention rather than claiming the
    // admin-writable WAFER_RUN_SHARED__ prefix.
    assert!(
        !crate::blocks::products::RUNTIME_KIND_CONFIG_KEY.starts_with("WAFER_RUN_SHARED__"),
        "runtime kind must not use the admin-writable shared prefix"
    );

    let legacy = ctx_with(&[("WAFER_RUN_SHARED__RUNTIME__KIND", "browser")]).await;
    assert!(crate::blocks::products::stripe_secret_operations_allowed(&legacy).await);

    let browser = ctx_with(&[(crate::blocks::products::RUNTIME_KIND_CONFIG_KEY, "browser")]).await;
    assert!(!crate::blocks::products::stripe_secret_operations_allowed(&browser).await);
}

#[test]
fn enumerable_and_numeric_vars_use_typed_widgets() {
    // Currency and country are enumerable — free-text invites typos that
    // only surface at checkout; they render as selects with declared
    // options. Fee and product limits are numeric.
    let currency = var("IMPRESSPRESS__PRODUCTS__DEFAULT_CURRENCY");
    assert_eq!(currency.input_type, InputType::Select);
    assert!(currency.options.iter().any(|o| o.value == "USD"));
    assert!(currency.options.iter().any(|o| o.value == "NZD"));

    let country = var("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY");
    assert_eq!(country.input_type, InputType::Select);
    assert!(country.options.iter().any(|o| o.value == "US"));
    assert!(country.options.iter().any(|o| o.value == "NZ"));
    // Optional var: an explicit "not set" choice must exist so admins can
    // clear it from the select widget.
    assert!(country.options.iter().any(|o| o.value.is_empty()));

    assert_eq!(
        var("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS").input_type,
        InputType::Number
    );
    assert_eq!(
        var("IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS").input_type,
        InputType::Number
    );
}
