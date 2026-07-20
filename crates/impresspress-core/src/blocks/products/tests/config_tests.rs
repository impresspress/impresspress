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
