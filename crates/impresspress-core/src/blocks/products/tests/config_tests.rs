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
    assert!(crate::blocks::products::stripe_secret_operations_allowed(
        &legacy
    ));

    let browser = ctx_with(&[(crate::blocks::products::RUNTIME_KIND_CONFIG_KEY, "browser")]).await;
    assert!(!crate::blocks::products::stripe_secret_operations_allowed(
        &browser
    ));
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

// ---------------------------------------------------------------------------
// The two config keys that had more than one reader (B22, B23)
// ---------------------------------------------------------------------------

/// [B22] A mis-set application fee is an error, not a silent zero.
///
/// Checkout and Payment Links parsed this key with `.ok().filter(..)
/// .unwrap_or(0)` — so `SELLER_APPLICATION_FEE_BPS=2.5%` meant the platform
/// took no fee at all and nothing said so, while seller onboarding refused
/// the same value outright.
#[tokio::test]
async fn seller_fee_refuses_every_value_that_is_not_basis_points() {
    use super::harness::{ctx, ctx_with};
    use crate::blocks::products::config::seller_fee_bps;

    for garbage in ["abc", "20000", "-1", "2.5", "", " ", "250 bps"] {
        let context = ctx_with(&[(
            "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
            garbage,
        )])
        .await;
        assert!(
            seller_fee_bps(&context).await.is_err(),
            "{garbage:?} must not resolve to a fee"
        );
    }
    let configured =
        ctx_with(&[("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", "250")]).await;
    assert_eq!(seller_fee_bps(&configured).await.unwrap(), 250);
    let boundary = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
        "10000",
    )])
    .await;
    assert_eq!(seller_fee_bps(&boundary).await.unwrap(), 10_000);
    // Unset falls back to the `ConfigVar` default, which is a real value.
    assert_eq!(seller_fee_bps(&ctx().await).await.unwrap(), 0);
}

/// [B23] One default for the platform country, and it is the empty one the
/// `ConfigVar` declares.
///
/// `stripe.rs` and the product wizard both defaulted it to `"US"` *and* fell
/// back to `"US"` on an invalid value, while seller onboarding defaulted it
/// to `""` and refused an invalid one. So an NZ platform that never set the
/// key onboarded with no country and shipped US-only.
#[tokio::test]
async fn platform_country_has_one_default_and_it_is_empty() {
    use super::harness::{ctx, ctx_with};
    use crate::blocks::products::config::platform_country;

    assert!(platform_country(&ctx().await).await.unwrap().is_none());
    let blank = ctx_with(&[("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", "  ")]).await;
    assert!(platform_country(&blank).await.unwrap().is_none());

    let lower = ctx_with(&[("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", " nz ")]).await;
    assert_eq!(
        platform_country(&lower).await.unwrap().unwrap().as_str(),
        "NZ"
    );

    for invalid in ["NZL", "N", "N1", "12"] {
        let context = ctx_with(&[("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", invalid)]).await;
        assert_eq!(
            platform_country(&context).await.unwrap_err().code,
            wafer_run::ErrorCode::FailedPrecondition,
            "{invalid:?} is not a country code"
        );
    }
}
