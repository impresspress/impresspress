use std::collections::{BTreeMap, HashMap};

use serde_json::json;
use wafer_core::clients::database as db;
use wafer_run::{AuthLevel, Block, ErrorCode};

use super::{
    super::{
        contracts::{
            AmountRule, BillingScheme, CheckoutPolicy, ComponentRecurrence, Condition, Offer,
            OfferComponent, OfferMode, PackageRounding, PricingModel, PricingPreviewRequest,
            PricingTier, QuantityRule, RecurringInterval, ShippingDeliveryEstimate,
            ShippingEstimateUnit, ShippingOption, TaxBehavior, UsageType, VariableDefinition,
            VariableKind, VariableVisibility,
        },
        offer_pricing::{
            evaluate_condition, evaluate_offer, validate_inputs, validate_offer, InputScope,
        },
        repo::{offer_components, offers, variables},
        ProductsBlock, PRODUCTS_TABLE,
    },
    harness::{create_msg, ctx, dispatch_user, output_is_error, output_to_json, seed},
};

fn variable(key: &str, kind: VariableKind) -> VariableDefinition {
    VariableDefinition {
        key: key.to_string(),
        kind,
        label: key.to_string(),
        help_text: String::new(),
        required: true,
        default_value: None,
        allowed_values: Vec::new(),
        minimum: None,
        maximum: None,
        step: None,
        maximum_length: None,
        visibility: VariableVisibility::Public,
        sort_order: 0,
    }
}

fn component(key: &str, amount: AmountRule, condition: Condition) -> OfferComponent {
    OfferComponent {
        id: format!("component_{key}"),
        key: key.to_string(),
        label: key.to_string(),
        description: String::new(),
        sort_order: 0,
        required: true,
        amount,
        quantity: QuantityRule::Fixed { value: 1 },
        condition,
        recurrence: None,
        stripe_price_id: String::new(),
        metadata: BTreeMap::new(),
    }
}

fn configurable_offer() -> Offer {
    let mut size = variable("size", VariableKind::Select);
    size.allowed_values = vec!["small".to_string(), "large".to_string()];
    let mut pages = variable("pages", VariableKind::Integer);
    pages.minimum = Some("1".to_string());
    pages.maximum = Some("100".to_string());
    pages.step = Some("1".to_string());
    let rush = variable("rush", VariableKind::Boolean);
    Offer {
        id: "offer_configurable".to_string(),
        product_id: "product_print".to_string(),
        version: 3,
        name: "Custom print".to_string(),
        mode: OfferMode::Payment,
        currency: "nzd".to_string(),
        pricing_model: PricingModel::Components,
        recurring_interval: None,
        interval_count: 1,
        usage_type: UsageType::Licensed,
        billing_scheme: BillingScheme::PerUnit,
        tax_behavior: TaxBehavior::Exclusive,
        variables: vec![size, pages, rush],
        components: vec![
            component(
                "base",
                AmountRule::Fixed {
                    unit_amount_minor: 1000,
                },
                Condition::Always,
            ),
            component(
                "pages",
                AmountRule::PerUnit {
                    input: "pages".to_string(),
                    unit_amount_minor: 25,
                },
                Condition::Always,
            ),
            component(
                "large",
                AmountRule::Fixed {
                    unit_amount_minor: 500,
                },
                Condition::Equals {
                    input: "size".to_string(),
                    value: json!("large"),
                },
            ),
            component(
                "rush",
                AmountRule::Fixed {
                    unit_amount_minor: 700,
                },
                Condition::Equals {
                    input: "rush".to_string(),
                    value: json!(true),
                },
            ),
        ],
        checkout: CheckoutPolicy::default(),
        stripe_product_id: String::new(),
        stripe_price_id: String::new(),
    }
}

#[test]
fn configurable_rows_resolve_in_minor_units_with_explanations() {
    let offer = configurable_offer();
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 2,
        inputs: BTreeMap::from([
            ("size".to_string(), json!("large")),
            ("pages".to_string(), json!(4)),
            ("rush".to_string(), json!(false)),
        ]),
    };
    let preview = evaluate_offer(&offer, &request, InputScope::Public).unwrap();
    assert_eq!(preview.amounts.total_minor, 3200);
    assert_eq!(preview.amounts.currency, "NZD");
    let rush = preview
        .components
        .iter()
        .find(|component| component.key == "rush")
        .unwrap();
    assert!(!rush.included);
    assert_eq!(rush.reason, "condition_not_met");
    assert_eq!(preview.inputs["pages"], json!(4));
}

#[test]
fn offer_total_policy_is_exact_inclusive_and_strictly_validated() {
    let mut offer = configurable_offer();
    offer.checkout.minimum_total_minor = Some(3200);
    offer.checkout.maximum_total_minor = Some(3200);
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 2,
        inputs: BTreeMap::from([
            ("size".to_string(), json!("large")),
            ("pages".to_string(), json!(4)),
            ("rush".to_string(), json!(false)),
        ]),
    };
    assert_eq!(
        evaluate_offer(&offer, &request, InputScope::Public)
            .unwrap()
            .amounts
            .total_minor,
        3200
    );

    offer.checkout.minimum_total_minor = Some(3201);
    offer.checkout.maximum_total_minor = None;
    assert_eq!(
        evaluate_offer(&offer, &request, InputScope::Public)
            .unwrap_err()
            .code,
        "total_below_minimum"
    );

    offer.checkout.minimum_total_minor = None;
    offer.checkout.maximum_total_minor = Some(3199);
    assert_eq!(
        evaluate_offer(&offer, &request, InputScope::Public)
            .unwrap_err()
            .code,
        "total_above_maximum"
    );

    offer.checkout.minimum_total_minor = Some(-1);
    offer.checkout.maximum_total_minor = None;
    assert_eq!(validate_offer(&offer).unwrap_err().code, "invalid_offer");

    offer.checkout.minimum_total_minor = Some(5000);
    offer.checkout.maximum_total_minor = Some(4999);
    assert!(validate_offer(&offer)
        .unwrap_err()
        .message
        .contains("greater than or equal"));
}

#[test]
fn typed_input_validation_covers_all_supported_kinds() {
    let number = variable("number", VariableKind::Number);
    let integer = variable("integer", VariableKind::Integer);
    let boolean = variable("boolean", VariableKind::Boolean);
    let mut date = variable("date", VariableKind::Date);
    date.minimum = Some("2026-07-01".to_string());
    date.maximum = Some("2026-07-31".to_string());
    let mut date_time = variable("date_time", VariableKind::DateTime);
    date_time.minimum = Some("2026-07-20T09:00".to_string());
    date_time.maximum = Some("2026-07-20T17:00".to_string());
    let mut select = variable("select", VariableKind::Select);
    select.allowed_values = vec!["a".to_string(), "b".to_string()];
    let mut multi = variable("multi", VariableKind::MultiSelect);
    multi.allowed_values = vec!["x".to_string(), "y".to_string()];
    let mut text = variable("text", VariableKind::Text);
    text.maximum_length = Some(10);
    let definitions = vec![
        number, integer, boolean, date, date_time, select, multi, text,
    ];
    let inputs = BTreeMap::from([
        ("number".to_string(), json!("1.25")),
        ("integer".to_string(), json!(2)),
        ("boolean".to_string(), json!(true)),
        ("date".to_string(), json!("2026-07-20")),
        ("date_time".to_string(), json!("2026-07-20T13:30")),
        ("select".to_string(), json!("a")),
        ("multi".to_string(), json!(["x", "y"])),
        ("text".to_string(), json!("hello")),
    ]);
    let validated = validate_inputs(&definitions, &inputs, InputScope::Public).unwrap();
    assert_eq!(validated.normalized()["number"], json!("1.25"));
    assert_eq!(validated.normalized()["multi"], json!(["x", "y"]));
    assert_eq!(validated.normalized()["date"], json!("2026-07-20"));
    assert_eq!(
        validated.normalized()["date_time"],
        json!("2026-07-20T13:30")
    );
}

#[test]
fn booking_dates_are_validated_bounded_and_orderable() {
    let mut arrival = variable("arrival", VariableKind::Date);
    arrival.minimum = Some("2026-07-01".to_string());
    arrival.maximum = Some("2026-07-31".to_string());
    let definitions = vec![arrival];

    let valid = BTreeMap::from([("arrival".to_string(), json!("2026-07-20"))]);
    assert!(validate_inputs(&definitions, &valid, InputScope::Public).is_ok());

    let invalid = BTreeMap::from([("arrival".to_string(), json!("20/07/2026"))]);
    assert_eq!(
        validate_inputs(&definitions, &invalid, InputScope::Public)
            .unwrap_err()
            .code,
        "invalid_input"
    );

    let out_of_range = BTreeMap::from([("arrival".to_string(), json!("2026-08-01"))]);
    assert_eq!(
        validate_inputs(&definitions, &out_of_range, InputScope::Public)
            .unwrap_err()
            .code,
        "invalid_input"
    );

    let inputs = validate_inputs(&definitions, &valid, InputScope::Public).unwrap();
    assert!(evaluate_condition(
        &Condition::GreaterThanOrEqual {
            input: "arrival".to_string(),
            value: json!("2026-07-15"),
        },
        &inputs,
    )
    .unwrap());
}

#[test]
fn inputs_reject_unknown_missing_bounds_and_duplicates() {
    let offer = configurable_offer();
    let mut inputs = BTreeMap::from([
        ("size".to_string(), json!("small")),
        ("pages".to_string(), json!(101)),
        ("rush".to_string(), json!(false)),
    ]);
    assert_eq!(
        validate_inputs(&offer.variables, &inputs, InputScope::Public)
            .unwrap_err()
            .code,
        "invalid_input"
    );
    inputs.insert("pages".to_string(), json!(2));
    inputs.insert("unknown".to_string(), json!(true));
    assert_eq!(
        validate_inputs(&offer.variables, &inputs, InputScope::Public)
            .unwrap_err()
            .code,
        "unknown_input"
    );
    inputs.remove("unknown");
    inputs.remove("rush");
    assert_eq!(
        validate_inputs(&offer.variables, &inputs, InputScope::Public)
            .unwrap_err()
            .code,
        "missing_input"
    );
}

fn comp_toggle_offer() -> Offer {
    let mut offer = configurable_offer();
    let mut comp = variable("comp", VariableKind::Boolean);
    comp.required = false;
    comp.default_value = Some(json!(false));
    comp.visibility = VariableVisibility::Hidden;
    let mut discount_tier = variable("discount_tier", VariableKind::Select);
    discount_tier.required = false;
    discount_tier.default_value = Some(json!("none"));
    discount_tier.allowed_values = vec!["none".to_string(), "half".to_string()];
    discount_tier.visibility = VariableVisibility::AdminOnly;
    offer.variables = vec![comp, discount_tier];
    offer.components = vec![
        component(
            "base",
            AmountRule::Fixed {
                unit_amount_minor: 10_000,
            },
            Condition::Equals {
                input: "comp".to_string(),
                value: json!(false),
            },
        ),
        component(
            "comped_base",
            AmountRule::Fixed {
                unit_amount_minor: 100,
            },
            Condition::Equals {
                input: "comp".to_string(),
                value: json!(true),
            },
        ),
    ];
    offer
}

#[test]
fn public_scope_rejects_hidden_and_admin_only_inputs_and_prices_their_defaults() {
    let offer = comp_toggle_offer();
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 1,
        inputs: BTreeMap::new(),
    };
    assert_eq!(
        evaluate_offer(&offer, &request, InputScope::Public)
            .unwrap()
            .amounts
            .total_minor,
        10_000
    );

    let comped = PricingPreviewRequest {
        inputs: BTreeMap::from([("comp".to_string(), json!(true))]),
        ..request.clone()
    };
    let error = evaluate_offer(&offer, &comped, InputScope::Public).unwrap_err();
    assert_eq!(error.code, "restricted_input");
    assert_eq!(error.field.as_deref(), Some("comp"));

    let discounted = PricingPreviewRequest {
        inputs: BTreeMap::from([("discount_tier".to_string(), json!("half"))]),
        ..request
    };
    let error = evaluate_offer(&offer, &discounted, InputScope::Public).unwrap_err();
    assert_eq!(error.code, "restricted_input");
    assert_eq!(error.field.as_deref(), Some("discount_tier"));
}

#[test]
fn management_scope_may_supply_hidden_and_admin_only_inputs() {
    let offer = comp_toggle_offer();
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 1,
        inputs: BTreeMap::from([
            ("comp".to_string(), json!(true)),
            ("discount_tier".to_string(), json!("half")),
        ]),
    };
    let preview = evaluate_offer(&offer, &request, InputScope::Management).unwrap();
    assert_eq!(preview.amounts.total_minor, 100);
    assert_eq!(preview.inputs["comp"], json!(true));
    assert_eq!(preview.inputs["discount_tier"], json!("half"));
}

#[test]
fn decimal_variable_pricing_is_exact_and_enforces_steps() {
    let mut offer = configurable_offer();
    let mut weight = variable("weight", VariableKind::Number);
    weight.minimum = Some("0.1".to_string());
    weight.maximum = Some("10".to_string());
    weight.step = Some("0.1".to_string());
    offer.variables = vec![weight];
    offer.components = vec![component(
        "weight",
        AmountRule::PerUnit {
            input: "weight".to_string(),
            unit_amount_minor: 10,
        },
        Condition::Always,
    )];
    let valid = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 1,
        inputs: BTreeMap::from([("weight".to_string(), json!("0.3"))]),
    };
    assert_eq!(
        evaluate_offer(&offer, &valid, InputScope::Public)
            .unwrap()
            .amounts
            .total_minor,
        3
    );
    let invalid = PricingPreviewRequest {
        inputs: BTreeMap::from([("weight".to_string(), json!("0.33"))]),
        ..valid
    };
    assert_eq!(
        evaluate_offer(&offer, &invalid, InputScope::Public)
            .unwrap_err()
            .code,
        "invalid_input"
    );
}

fn flat_plus_per_unit_offer() -> Offer {
    let mut offer = configurable_offer();
    offer.variables = vec![variable("hours", VariableKind::Number)];
    offer.components = vec![component(
        "labour",
        AmountRule::FlatPlusPerUnit {
            base_amount_minor: 10_000,
            input: "hours".to_string(),
            unit_amount_minor: 2000,
        },
        Condition::Always,
    )];
    offer
}

#[test]
fn flat_plus_per_unit_charges_the_base_plus_the_per_unit_contribution() {
    let offer = flat_plus_per_unit_offer();
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 1,
        inputs: BTreeMap::from([("hours".to_string(), json!(3))]),
    };
    let preview = evaluate_offer(&offer, &request, InputScope::Public).unwrap();
    assert_eq!(preview.amounts.total_minor, 16_000);
    let labour = preview
        .components
        .iter()
        .find(|component| component.key == "labour")
        .unwrap();
    assert!(labour.included);
    assert_eq!(labour.unit_amount_minor, 16_000);
    assert_eq!(labour.total_amount_minor, 16_000);
}

#[test]
fn negative_per_unit_inputs_never_reduce_the_price() {
    // FlatPlusPerUnit: -3 × 2000 must not offset the 10000 base down to 4000.
    let mut offer = flat_plus_per_unit_offer();
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 1,
        inputs: BTreeMap::from([("hours".to_string(), json!(-3))]),
    };
    let error = evaluate_offer(&offer, &request, InputScope::Public).unwrap_err();
    assert_eq!(error.code, "invalid_input");
    assert_eq!(error.field.as_deref(), Some("hours"));
    assert!(error.message.contains("must not be negative"));

    // PerUnit rejects the negative contribution the same way.
    offer.components[0].amount = AmountRule::PerUnit {
        input: "hours".to_string(),
        unit_amount_minor: 2000,
    };
    let error = evaluate_offer(&offer, &request, InputScope::Public).unwrap_err();
    assert_eq!(error.code, "invalid_input");
    assert_eq!(error.field.as_deref(), Some("hours"));
    assert!(error.message.contains("must not be negative"));
}

#[test]
fn bounds_comparison_overflow_is_rejected_not_treated_as_in_range() {
    let mut weight = variable("weight", VariableKind::Number);
    weight.minimum = Some("0.5".to_string());
    weight.maximum = Some("10.5".to_string());
    let definitions = vec![weight];
    // 38 digits parse as a valid decimal but overflow the i128 comparison
    // against a fractional bound; that must fail closed instead of being
    // treated as within bounds.
    let huge = "9".repeat(38);
    let inputs = BTreeMap::from([("weight".to_string(), json!(huge))]);
    let error = validate_inputs(&definitions, &inputs, InputScope::Public).unwrap_err();
    assert_eq!(error.code, "invalid_input");
    assert_eq!(error.field.as_deref(), Some("weight"));
    assert!(error.message.contains("too large"));
}

#[test]
fn graduated_volume_and_package_pricing_are_exact_at_boundaries() {
    let tiers = vec![
        PricingTier {
            up_to: Some(5),
            unit_amount_minor: 100,
            flat_amount_minor: 200,
        },
        PricingTier {
            up_to: None,
            unit_amount_minor: 80,
            flat_amount_minor: 50,
        },
    ];
    let mut offer = configurable_offer();
    let mut units = variable("units", VariableKind::Integer);
    units.minimum = Some("0".to_string());
    units.maximum = Some("1000".to_string());
    offer.variables = vec![units];
    offer.billing_scheme = BillingScheme::Tiered;
    offer.components = vec![component(
        "usage",
        AmountRule::Graduated {
            input: "units".to_string(),
            tiers: tiers.clone(),
        },
        Condition::Always,
    )];
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 1,
        inputs: BTreeMap::from([("units".to_string(), json!(7))]),
    };
    let graduated = evaluate_offer(&offer, &request, InputScope::Public).unwrap();
    assert_eq!(graduated.amounts.total_minor, 910);

    offer.components[0].amount = AmountRule::Volume {
        input: "units".to_string(),
        tiers,
    };
    let volume = evaluate_offer(&offer, &request, InputScope::Public).unwrap();
    assert_eq!(volume.amounts.total_minor, 610);

    offer.billing_scheme = BillingScheme::PerUnit;
    offer.components[0].amount = AmountRule::Package {
        input: "units".to_string(),
        units_per_package: 5,
        package_amount_minor: 250,
        rounding: PackageRounding::Up,
    };
    let package_request = PricingPreviewRequest {
        inputs: BTreeMap::from([("units".to_string(), json!(11))]),
        ..request
    };
    let packages = evaluate_offer(&offer, &package_request, InputScope::Public).unwrap();
    assert_eq!(packages.amounts.total_minor, 750);

    offer.components[0].amount = AmountRule::Package {
        input: "units".to_string(),
        units_per_package: 5,
        package_amount_minor: 250,
        rounding: PackageRounding::Exact,
    };
    let error = evaluate_offer(&offer, &package_request, InputScope::Public).unwrap_err();
    assert_eq!(error.code, "invalid_input");
    assert!(error.message.contains("exact multiple"));
}

#[test]
fn advanced_amount_rules_reject_bad_tiers_types_and_overflow() {
    let mut offer = configurable_offer();
    offer.billing_scheme = BillingScheme::Tiered;
    offer.components[0].amount = AmountRule::Graduated {
        input: "pages".to_string(),
        tiers: vec![
            PricingTier {
                up_to: Some(5),
                unit_amount_minor: 100,
                flat_amount_minor: 0,
            },
            PricingTier {
                up_to: Some(5),
                unit_amount_minor: 90,
                flat_amount_minor: 0,
            },
            PricingTier {
                up_to: None,
                unit_amount_minor: 80,
                flat_amount_minor: 0,
            },
        ],
    };
    assert!(validate_offer(&offer)
        .unwrap_err()
        .message
        .contains("strictly increasing"));

    offer.billing_scheme = BillingScheme::PerUnit;
    offer.components[0].amount = AmountRule::PerUnit {
        input: "size".to_string(),
        unit_amount_minor: 10,
    };
    assert!(validate_offer(&offer)
        .unwrap_err()
        .message
        .contains("incompatible input type"));

    offer.variables = vec![variable("units", VariableKind::Integer)];
    offer.components = vec![component(
        "package",
        AmountRule::Package {
            input: "units".to_string(),
            units_per_package: 1,
            package_amount_minor: i64::MAX,
            rounding: PackageRounding::Up,
        },
        Condition::Always,
    )];
    let error = evaluate_offer(
        &offer,
        &PricingPreviewRequest {
            offer_id: offer.id.clone(),
            quantity: 1,
            inputs: BTreeMap::from([("units".to_string(), json!(2))]),
        },
        InputScope::Public,
    )
    .unwrap_err();
    assert_eq!(error.code, "amount_overflow");
}

#[test]
fn recurring_components_must_match_the_subscription_offer() {
    let mut offer = configurable_offer();
    offer.mode = OfferMode::Subscription;
    offer.recurring_interval = Some(RecurringInterval::Month);
    offer.components[0].recurrence = Some(ComponentRecurrence {
        interval: RecurringInterval::Year,
        interval_count: 1,
    });
    assert_eq!(validate_offer(&offer).unwrap_err().code, "invalid_offer");
}

#[test]
fn checkout_shipping_policy_is_strict_and_bounded() {
    let mut offer = configurable_offer();
    offer.checkout = CheckoutPolicy {
        collect_shipping_address: true,
        allowed_shipping_countries: vec!["nz".to_string(), "AU".to_string()],
        shipping_options: vec![ShippingOption {
            display_name: "Standard".to_string(),
            amount_minor: 500,
            tax_behavior: TaxBehavior::Exclusive,
            delivery_estimate: Some(ShippingDeliveryEstimate {
                minimum: Some(3),
                maximum: Some(5),
                unit: ShippingEstimateUnit::BusinessDay,
            }),
            stripe_shipping_rate_id: String::new(),
        }],
        create_customer: true,
        ..CheckoutPolicy::default()
    };
    validate_offer(&offer).unwrap();

    let mut invalid = offer.clone();
    invalid.checkout.collect_shipping_address = false;
    assert!(validate_offer(&invalid)
        .unwrap_err()
        .message
        .contains("require shipping-address collection"));

    let mut invalid = offer.clone();
    invalid.checkout.allowed_shipping_countries = vec!["NZ".to_string(), "nz".to_string()];
    assert!(validate_offer(&invalid)
        .unwrap_err()
        .message
        .contains("must be unique"));

    let mut invalid = offer.clone();
    invalid.checkout.shipping_options[0].delivery_estimate = Some(ShippingDeliveryEstimate {
        minimum: Some(6),
        maximum: Some(5),
        unit: ShippingEstimateUnit::Day,
    });
    assert!(validate_offer(&invalid)
        .unwrap_err()
        .message
        .contains("positive and ordered"));

    let mut invalid = offer.clone();
    invalid.checkout.shipping_options[0].stripe_shipping_rate_id = "rate_wrong".to_string();
    assert!(validate_offer(&invalid)
        .unwrap_err()
        .message
        .contains("must start with shr_"));

    let mut invalid = offer;
    invalid.checkout.shipping_options = vec![invalid.checkout.shipping_options[0].clone(); 6];
    assert!(validate_offer(&invalid)
        .unwrap_err()
        .message
        .contains("at most five"));
}

async fn seed_persisted_preview_offer(
    ctx: &crate::test_support::TestContext,
    product_status: &str,
) {
    seed(
        ctx,
        PRODUCTS_TABLE,
        "product_preview",
        HashMap::from([
            ("name".to_string(), json!("Custom print")),
            ("status".to_string(), json!(product_status)),
            ("approval_status".to_string(), json!("approved")),
        ]),
    )
    .await;
    seed(
        ctx,
        offers::TABLE,
        "offer_preview",
        HashMap::from([
            ("product_id".to_string(), json!("product_preview")),
            ("name".to_string(), json!("Print offer")),
            ("status".to_string(), json!("active")),
            ("mode".to_string(), json!("payment")),
            ("currency".to_string(), json!("NZD")),
            ("pricing_model".to_string(), json!("components")),
            ("config_json".to_string(), json!("{}")),
        ]),
    )
    .await;
    seed(
        ctx,
        variables::TABLE,
        "variable_pages",
        HashMap::from([
            ("name".to_string(), json!("pages")),
            ("var_type".to_string(), json!("integer")),
            ("offer_id".to_string(), json!("offer_preview")),
            ("label".to_string(), json!("Pages")),
            ("required".to_string(), json!(1)),
            ("minimum_value".to_string(), json!("1")),
            ("maximum_value".to_string(), json!("20")),
            ("step_value".to_string(), json!("1")),
        ]),
    )
    .await;
    seed(
        ctx,
        offer_components::TABLE,
        "component_pages",
        HashMap::from([
            ("offer_id".to_string(), json!("offer_preview")),
            ("component_key".to_string(), json!("pages")),
            ("label".to_string(), json!("Printed pages")),
            (
                "amount_rule_json".to_string(),
                json!(r#"{"type":"per_unit","input":"pages","unit_amount_minor":25}"#),
            ),
            (
                "quantity_rule_json".to_string(),
                json!(r#"{"type":"fixed","value":1}"#),
            ),
            ("condition_json".to_string(), json!("{}")),
        ]),
    )
    .await;
}

#[tokio::test]
async fn public_preview_loads_server_owned_offer_rows() {
    let ctx = ctx().await;
    seed_persisted_preview_offer(&ctx, "active").await;
    let persisted = offers::get_public(&ctx, "offer_preview").await.unwrap();
    assert_eq!(persisted.id, "offer_preview");
    assert_eq!(persisted.components.len(), 1);
    let (msg, input) = create_msg(
        "/b/products/pricing/preview",
        "",
        json!({
            "offer_id": "offer_preview",
            "quantity": 2,
            "inputs": {"pages": 4}
        }),
    );
    let body = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(body["offer_id"], "offer_preview");
    assert_eq!(body["amounts"]["currency"], "NZD");
    assert_eq!(body["amounts"]["total_minor"], 200);
    assert_eq!(body["components"][0]["label"], "Printed pages");

    db::update(
        &ctx,
        offers::TABLE,
        "offer_preview",
        HashMap::from([(
            "config_json".to_string(),
            json!(r#"{"minimum_total_minor":201}"#),
        )]),
    )
    .await
    .unwrap();
    let (msg, input) = create_msg(
        "/b/products/pricing/preview",
        "",
        json!({
            "offer_id": "offer_preview",
            "quantity": 2,
            "inputs": {"pages": 4}
        }),
    );
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
}

#[tokio::test]
async fn public_preview_rejects_hidden_and_admin_only_inputs() {
    let ctx = ctx().await;
    seed_persisted_preview_offer(&ctx, "active").await;
    seed(
        &ctx,
        variables::TABLE,
        "variable_comp",
        HashMap::from([
            ("name".to_string(), json!("comp")),
            ("var_type".to_string(), json!("boolean")),
            ("offer_id".to_string(), json!("offer_preview")),
            ("label".to_string(), json!("Comp this order")),
            ("required".to_string(), json!(0)),
            ("default_value".to_string(), json!("false")),
            ("visibility".to_string(), json!("hidden")),
        ]),
    )
    .await;
    seed(
        &ctx,
        variables::TABLE,
        "variable_discount_tier",
        HashMap::from([
            ("name".to_string(), json!("discount_tier")),
            ("var_type".to_string(), json!("select")),
            ("offer_id".to_string(), json!("offer_preview")),
            ("label".to_string(), json!("Discount tier")),
            ("required".to_string(), json!(0)),
            ("default_value".to_string(), json!("\"none\"")),
            ("allowed_values".to_string(), json!(r#"["none","half"]"#)),
            ("visibility".to_string(), json!("admin_only")),
        ]),
    )
    .await;

    for restricted in [json!({"comp": true}), json!({"discount_tier": "half"})] {
        let mut inputs = json!({"pages": 4});
        inputs
            .as_object_mut()
            .unwrap()
            .extend(restricted.as_object().unwrap().clone());
        let (msg, input) = create_msg(
            "/b/products/pricing/preview",
            "",
            json!({"offer_id": "offer_preview", "quantity": 1, "inputs": inputs}),
        );
        assert!(
            output_is_error(
                dispatch_user(&ctx, msg, input).await,
                ErrorCode::InvalidArgument
            )
            .await
        );
    }

    // Without the restricted inputs the defaults price normally.
    let (msg, input) = create_msg(
        "/b/products/pricing/preview",
        "",
        json!({"offer_id": "offer_preview", "quantity": 1, "inputs": {"pages": 4}}),
    );
    let body = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(body["amounts"]["total_minor"], 100);
    assert_eq!(body["inputs"]["comp"], json!(false));
    assert_eq!(body["inputs"]["discount_tier"], json!("none"));
}

#[tokio::test]
async fn public_preview_hides_offers_for_non_active_products() {
    let ctx = ctx().await;
    seed_persisted_preview_offer(&ctx, "draft").await;
    let (msg, input) = create_msg(
        "/b/products/pricing/preview",
        "",
        json!({"offer_id": "offer_preview", "inputs": {"pages": 4}}),
    );
    assert!(output_is_error(dispatch_user(&ctx, msg, input).await, ErrorCode::NotFound).await);
}

#[test]
fn preview_endpoint_is_explicitly_public_for_static_storefronts() {
    let info = ProductsBlock::new().info();
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "create",
            "/b/products/pricing/preview"
        ),
        Some(AuthLevel::Public)
    );
}

#[test]
fn nested_conditions_are_deterministic() {
    let mut offer = configurable_offer();
    offer.components[0].condition = Condition::All {
        conditions: vec![
            Condition::GreaterThanOrEqual {
                input: "pages".to_string(),
                value: json!(4),
            },
            Condition::Not {
                condition: Box::new(Condition::Equals {
                    input: "rush".to_string(),
                    value: json!(true),
                }),
            },
        ],
    };
    let request = PricingPreviewRequest {
        offer_id: offer.id.clone(),
        quantity: 1,
        inputs: BTreeMap::from([
            ("size".to_string(), json!("small")),
            ("pages".to_string(), json!(4)),
            ("rush".to_string(), json!(false)),
        ]),
    };
    let preview = evaluate_offer(&offer, &request, InputScope::Public).unwrap();
    assert!(
        preview
            .components
            .iter()
            .find(|component| component.key == "base")
            .unwrap()
            .included
    );
}
