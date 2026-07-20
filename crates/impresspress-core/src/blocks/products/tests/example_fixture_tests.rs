use serde_json::Value;

use crate::blocks::products::{
    contracts::{
        Offer, OfferComponent, OfferDefinitionRequest, PricingPreview, PricingPreviewRequest,
        StorefrontProduct,
    },
    offer_pricing::{evaluate_offer, validate_offer},
};

const EXAMPLES: [(&str, &str); 10] = [
    (
        "digital-download",
        include_str!("../../../../../../examples/products/digital-download/commerce.fixture.json"),
    ),
    (
        "boutique-store",
        include_str!("../../../../../../examples/products/boutique-store/commerce.fixture.json"),
    ),
    (
        "saas-plans",
        include_str!("../../../../../../examples/products/saas-plans/commerce.fixture.json"),
    ),
    (
        "usage-saas",
        include_str!("../../../../../../examples/products/usage-saas/commerce.fixture.json"),
    ),
    (
        "membership",
        include_str!("../../../../../../examples/products/membership/commerce.fixture.json"),
    ),
    (
        "event-tickets",
        include_str!("../../../../../../examples/products/event-tickets/commerce.fixture.json"),
    ),
    (
        "course-configurator",
        include_str!(
            "../../../../../../examples/products/course-configurator/commerce.fixture.json"
        ),
    ),
    (
        "professional-services",
        include_str!(
            "../../../../../../examples/products/professional-services/commerce.fixture.json"
        ),
    ),
    (
        "marketplace",
        include_str!("../../../../../../examples/products/marketplace/commerce.fixture.json"),
    ),
    (
        "donation-campaign",
        include_str!("../../../../../../examples/products/donation-campaign/commerce.fixture.json"),
    ),
];

fn materialize_offer(definition: &OfferDefinitionRequest, offer_id: &str, version: u32) -> Offer {
    Offer {
        id: offer_id.to_string(),
        product_id: format!("product-{offer_id}"),
        version,
        name: definition.name.clone(),
        mode: definition.mode,
        currency: definition.currency.clone(),
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
            .enumerate()
            .map(|(index, draft)| OfferComponent {
                id: draft.key.clone(),
                key: draft.key.clone(),
                label: draft.label.clone(),
                description: draft.description.clone(),
                sort_order: draft.sort_order.max(index as i32),
                required: draft.required,
                amount: draft.amount.clone(),
                quantity: draft.quantity.clone(),
                condition: draft.condition.clone(),
                recurrence: draft.recurrence.clone(),
                stripe_price_id: String::new(),
                metadata: draft.metadata.clone(),
            })
            .collect(),
        checkout: definition.checkout.clone(),
        stripe_product_id: String::new(),
        stripe_price_id: String::new(),
    }
}

#[test]
fn all_example_fixtures_match_strict_public_contracts_and_domain_pricing() {
    for (slug, source) in EXAMPLES {
        let fixture: Value = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{slug}: fixture JSON is invalid: {error}"));
        assert_eq!(fixture["slug"], slug);

        let product: StorefrontProduct = serde_json::from_value(fixture["product"].clone())
            .unwrap_or_else(|error| panic!("{slug}: storefront contract mismatch: {error}"));
        let definition: OfferDefinitionRequest =
            serde_json::from_value(fixture["seed"]["offer_definition"].clone())
                .unwrap_or_else(|error| panic!("{slug}: offer seed contract mismatch: {error}"));
        let request: PricingPreviewRequest = serde_json::from_value(serde_json::json!({
            "offer_id": fixture["scenario"]["offer_id"],
            "quantity": fixture["scenario"]["quantity"],
            "inputs": fixture["scenario"]["inputs"]
        }))
        .unwrap_or_else(|error| panic!("{slug}: pricing scenario contract mismatch: {error}"));
        let expected: PricingPreview = serde_json::from_value(fixture["scenario"]["quote"].clone())
            .unwrap_or_else(|error| panic!("{slug}: quote contract mismatch: {error}"));

        assert!(
            product
                .offers
                .iter()
                .any(|offer| offer.id == request.offer_id),
            "{slug}: scenario offer is not in the public storefront"
        );
        let offer = materialize_offer(&definition, &request.offer_id, expected.offer_version);
        validate_offer(&offer)
            .unwrap_or_else(|error| panic!("{slug}: invalid offer seed: {error:?}"));
        let evaluated = evaluate_offer(&offer, &request)
            .unwrap_or_else(|error| panic!("{slug}: scenario evaluation failed: {error:?}"));

        assert_eq!(
            evaluated.amounts, expected.amounts,
            "{slug}: checked-in expected amounts drifted from the pricing engine"
        );
        assert_eq!(
            evaluated.inputs, expected.inputs,
            "{slug}: normalized scenario inputs drifted"
        );
        assert_eq!(
            evaluated
                .components
                .iter()
                .filter(|component| component.included)
                .map(|component| (
                    component.key.as_str(),
                    component.included,
                    component.total_amount_minor
                ))
                .collect::<Vec<_>>(),
            expected
                .components
                .iter()
                .filter(|component| component.included)
                .map(|component| (
                    component.key.as_str(),
                    component.included,
                    component.total_amount_minor
                ))
                .collect::<Vec<_>>(),
            "{slug}: component inclusion or totals drifted"
        );
    }
}
