//! Stable, provider-neutral JSON contracts for the commerce APIs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const COMMERCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    SimpleProduct,
    SimpleSubscription,
    ConfigurableProduct,
    ConfigurableSubscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    Platform,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductStatus {
    Draft,
    PendingReview,
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Draft,
    Pending,
    Approved,
    Rejected,
    Suspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FulfillmentKind {
    None,
    Manual,
    Download,
    Entitlement,
    Webhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferMode {
    Payment,
    Subscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    Draft,
    Active,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingModel {
    Fixed,
    Components,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecurringInterval {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageType {
    Licensed,
    Metered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingScheme {
    PerUnit,
    Tiered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaxBehavior {
    #[default]
    Unspecified,
    Inclusive,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShippingEstimateUnit {
    Hour,
    Day,
    BusinessDay,
    Week,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutPresentation {
    #[default]
    Hosted,
    Embedded,
    PaymentLink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableKind {
    Number,
    Integer,
    Boolean,
    Date,
    DateTime,
    Select,
    MultiSelect,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VariableVisibility {
    #[default]
    Public,
    Hidden,
    AdminOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariableDefinition {
    pub key: String,
    pub kind: VariableKind,
    pub label: String,
    #[serde(default)]
    pub help_text: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default_value: Option<Value>,
    #[serde(default)]
    pub allowed_values: Vec<String>,
    #[serde(default)]
    pub minimum: Option<String>,
    #[serde(default)]
    pub maximum: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub maximum_length: Option<usize>,
    #[serde(default)]
    pub visibility: VariableVisibility,
    #[serde(default)]
    pub sort_order: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    #[default]
    Always,
    All {
        conditions: Vec<Condition>,
    },
    Any {
        conditions: Vec<Condition>,
    },
    Not {
        condition: Box<Condition>,
    },
    Present {
        input: String,
    },
    Equals {
        input: String,
        value: Value,
    },
    NotEquals {
        input: String,
        value: Value,
    },
    GreaterThan {
        input: String,
        value: Value,
    },
    GreaterThanOrEqual {
        input: String,
        value: Value,
    },
    LessThan {
        input: String,
        value: Value,
    },
    LessThanOrEqual {
        input: String,
        value: Value,
    },
    In {
        input: String,
        values: Vec<Value>,
    },
    Contains {
        input: String,
        value: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingTier {
    /// Inclusive upper bound for this tier. Only the final tier may omit it.
    #[serde(default)]
    pub up_to: Option<u64>,
    pub unit_amount_minor: i64,
    #[serde(default)]
    pub flat_amount_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PackageRounding {
    /// Charge one package for any partially used package.
    #[default]
    Up,
    /// Require the input to be an exact multiple of the package size.
    Exact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AmountRule {
    Fixed {
        unit_amount_minor: i64,
    },
    PerUnit {
        input: String,
        unit_amount_minor: i64,
    },
    FlatPlusPerUnit {
        base_amount_minor: i64,
        input: String,
        unit_amount_minor: i64,
    },
    Lookup {
        input: String,
        prices: BTreeMap<String, i64>,
    },
    Graduated {
        input: String,
        tiers: Vec<PricingTier>,
    },
    Volume {
        input: String,
        tiers: Vec<PricingTier>,
    },
    Package {
        input: String,
        units_per_package: u64,
        package_amount_minor: i64,
        #[serde(default)]
        rounding: PackageRounding,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuantityRule {
    Fixed {
        value: u64,
    },
    FromInput {
        input: String,
        #[serde(default = "one_u64")]
        minimum: u64,
        #[serde(default)]
        maximum: Option<u64>,
    },
}

impl Default for QuantityRule {
    fn default() -> Self {
        Self::Fixed { value: 1 }
    }
}

fn one_u64() -> u64 {
    1
}
fn one_u32() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRecurrence {
    pub interval: RecurringInterval,
    #[serde(default = "one_u32")]
    pub interval_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferComponent {
    pub id: String,
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub required: bool,
    pub amount: AmountRule,
    #[serde(default)]
    pub quantity: QuantityRule,
    #[serde(default)]
    pub condition: Condition,
    #[serde(default)]
    pub recurrence: Option<ComponentRecurrence>,
    #[serde(default)]
    pub stripe_price_id: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferComponentDraft {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub required: bool,
    pub amount: AmountRule,
    #[serde(default)]
    pub quantity: QuantityRule,
    #[serde(default)]
    pub condition: Condition,
    #[serde(default)]
    pub recurrence: Option<ComponentRecurrence>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CheckoutPolicy {
    /// Minimum evaluated item total before provider discounts, tax, or shipping.
    #[serde(default)]
    pub minimum_total_minor: Option<i64>,
    /// Maximum evaluated item total before provider discounts, tax, or shipping.
    #[serde(default)]
    pub maximum_total_minor: Option<i64>,
    #[serde(default)]
    pub allow_promotion_codes: bool,
    #[serde(default)]
    pub automatic_tax: bool,
    #[serde(default)]
    pub collect_billing_address: bool,
    #[serde(default)]
    pub collect_shipping_address: bool,
    #[serde(default)]
    pub allowed_shipping_countries: Vec<String>,
    #[serde(default)]
    pub shipping_options: Vec<ShippingOption>,
    #[serde(default)]
    pub create_customer: bool,
    #[serde(default)]
    pub require_terms_consent: bool,
    #[serde(default)]
    pub trial_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShippingDeliveryEstimate {
    #[serde(default)]
    pub minimum: Option<u32>,
    #[serde(default)]
    pub maximum: Option<u32>,
    pub unit: ShippingEstimateUnit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShippingOption {
    pub display_name: String,
    pub amount_minor: i64,
    #[serde(default)]
    pub tax_behavior: TaxBehavior,
    #[serde(default)]
    pub delivery_estimate: Option<ShippingDeliveryEstimate>,
    #[serde(default)]
    pub stripe_shipping_rate_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub id: String,
    pub product_id: String,
    pub version: u32,
    pub name: String,
    pub mode: OfferMode,
    pub currency: String,
    pub pricing_model: PricingModel,
    #[serde(default)]
    pub recurring_interval: Option<RecurringInterval>,
    #[serde(default = "one_u32")]
    pub interval_count: u32,
    pub usage_type: UsageType,
    pub billing_scheme: BillingScheme,
    pub tax_behavior: TaxBehavior,
    #[serde(default)]
    pub variables: Vec<VariableDefinition>,
    pub components: Vec<OfferComponent>,
    #[serde(default)]
    pub checkout: CheckoutPolicy,
    #[serde(default)]
    pub stripe_product_id: String,
    #[serde(default)]
    pub stripe_price_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferDefinitionRequest {
    pub name: String,
    pub mode: OfferMode,
    pub currency: String,
    pub pricing_model: PricingModel,
    #[serde(default)]
    pub recurring_interval: Option<RecurringInterval>,
    #[serde(default = "one_u32")]
    pub interval_count: u32,
    pub usage_type: UsageType,
    pub billing_scheme: BillingScheme,
    pub tax_behavior: TaxBehavior,
    #[serde(default)]
    pub variables: Vec<VariableDefinition>,
    pub components: Vec<OfferComponentDraft>,
    #[serde(default)]
    pub checkout: CheckoutPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedOffer {
    pub status: OfferStatus,
    pub sync_status: String,
    #[serde(default)]
    pub sync_error: String,
    pub offer: Offer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductTemplate {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub kind: TemplateKind,
    pub schema_version: u32,
    #[serde(default)]
    pub variables: Vec<VariableDefinition>,
    #[serde(default)]
    pub offer_defaults: Value,
    pub is_system: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    pub owner_kind: OwnerKind,
    #[serde(default)]
    pub owner_id: String,
    #[serde(default)]
    pub seller_account_id: String,
    pub status: ProductStatus,
    pub approval_status: ApprovalStatus,
    pub fulfillment_kind: FulfillmentKind,
    pub template_id: String,
    pub current_version: u32,
    #[serde(default)]
    pub image_url: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default)]
    pub offers: Vec<Offer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorefrontOffer {
    pub id: String,
    pub version: u32,
    pub name: String,
    pub mode: OfferMode,
    pub currency: String,
    pub pricing_model: PricingModel,
    #[serde(default)]
    pub recurring_interval: Option<RecurringInterval>,
    pub interval_count: u32,
    pub variables: Vec<VariableDefinition>,
    pub checkout: CheckoutPolicy,
    #[serde(default)]
    pub payment_links: Vec<StorefrontPaymentLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorefrontProduct {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub image_url: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub fulfillment_kind: FulfillmentKind,
    pub offers: Vec<StorefrontOffer>,
}

/// Browser-safe deployment configuration. The Stripe secret key, webhook
/// secret, account ids, and provider API URL are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorefrontConfig {
    pub schema_version: u32,
    pub embedded_checkout_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stripe_publishable_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stripe_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingPreviewRequest {
    pub offer_id: String,
    #[serde(default = "one_u64")]
    pub quantity: u64,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedComponent {
    pub component_id: String,
    pub key: String,
    pub label: String,
    pub included: bool,
    pub required: bool,
    pub unit_amount_minor: i64,
    pub quantity: u64,
    pub total_amount_minor: i64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoneyBreakdown {
    pub currency: String,
    pub subtotal_minor: i64,
    pub discount_minor: i64,
    pub tax_minor: i64,
    #[serde(default)]
    pub shipping_minor: i64,
    pub platform_fee_minor: i64,
    pub total_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingPreview {
    pub schema_version: u32,
    pub offer_id: String,
    pub offer_version: u32,
    pub quantity: u64,
    pub inputs: BTreeMap<String, Value>,
    pub components: Vec<ResolvedComponent>,
    pub amounts: MoneyBreakdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutRequest {
    pub offer_id: String,
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default = "one_u64")]
    pub quantity: u64,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
    #[serde(default)]
    pub presentation: CheckoutPresentation,
    #[serde(default)]
    pub success_url: Option<String>,
    #[serde(default)]
    pub cancel_url: Option<String>,
    #[serde(default)]
    pub buyer_email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutResponse {
    pub order_id: String,
    /// Returned once and never persisted in plaintext. Static storefronts use
    /// it to poll the minimal guest order-status endpoint after Stripe returns.
    pub receipt_token: String,
    pub receipt_token_expires_at: String,
    pub presentation: CheckoutPresentation,
    #[serde(default)]
    pub checkout_url: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub payment_link_url: Option<String>,
    pub amounts: MoneyBreakdown,
}

/// Minimal order state exposed to a guest who presents the checkout receipt
/// capability. Buyer details and all Stripe resource ids remain private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestOrderStatus {
    pub schema_version: u32,
    pub order_id: String,
    pub status: String,
    pub reconciliation_status: String,
    pub amounts: MoneyBreakdown,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_current_period_end: Option<String>,
    pub subscription_cancel_at_period_end: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refunded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutPresetRequest {
    pub name: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutPreset {
    pub id: String,
    pub offer_id: String,
    pub name: String,
    pub slug: String,
    pub inputs: BTreeMap<String, Value>,
    pub active: bool,
    pub configuration_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentLinkCreateRequest {
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default)]
    pub after_completion_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedPaymentLink {
    pub id: String,
    pub offer_id: String,
    #[serde(default)]
    pub preset_id: String,
    pub url: String,
    pub active: bool,
    pub configuration_hash: String,
    pub sync_status: String,
    #[serde(default)]
    pub sync_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorefrontPaymentLink {
    pub id: String,
    #[serde(default)]
    pub preset_id: String,
    pub url: String,
    /// Immutable server-resolved pricing captured when the reusable link was
    /// synchronized. This lets static pages display the link's actual price
    /// without issuing a runtime checkout or evaluating unrelated inputs.
    pub pricing: PricingPreview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderLineItem {
    pub product_id: String,
    pub offer_id: String,
    #[serde(default)]
    pub component_id: String,
    pub description: String,
    pub unit_amount_minor: i64,
    pub quantity: u64,
    pub total_amount_minor: i64,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub id: String,
    pub status: String,
    pub mode: OfferMode,
    #[serde(default)]
    pub buyer_user_id: String,
    #[serde(default)]
    pub buyer_email: String,
    #[serde(default)]
    pub seller_account_id: String,
    pub amounts: MoneyBreakdown,
    pub items: Vec<OrderLineItem>,
    #[serde(default)]
    pub stripe_session_id: String,
    #[serde(default)]
    pub stripe_payment_intent_id: String,
    #[serde(default)]
    pub stripe_subscription_id: String,
    pub livemode: bool,
    pub reconciliation_status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscription {
    pub id: String,
    pub status: String,
    pub product_id: String,
    pub offer_id: String,
    pub buyer_user_id: String,
    pub buyer_email: String,
    pub current_period_end: Option<String>,
    pub cancel_at_period_end: bool,
    pub items: Vec<OrderLineItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SellerCapabilities {
    pub details_submitted: bool,
    pub charges_enabled: bool,
    pub payouts_enabled: bool,
    #[serde(default)]
    pub requirements_due: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SellerAccount {
    pub id: String,
    pub user_id: String,
    pub status: String,
    pub approval_status: ApprovalStatus,
    #[serde(default)]
    pub stripe_account_id: String,
    pub capabilities: SellerCapabilities,
    pub fee_basis_points: u32,
    #[serde(default)]
    pub livemode: bool,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub default_currency: String,
    #[serde(default)]
    pub dashboard_type: String,
    #[serde(default)]
    pub disabled_reason: String,
    #[serde(default)]
    pub sync_error: String,
    #[serde(default)]
    pub last_synced_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StripeConnectionState {
    NotConfigured,
    ConnectedTest,
    ConnectedLive,
    Misconfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StripeConnectionStatus {
    pub state: StripeConnectionState,
    pub configured: bool,
    pub livemode: bool,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub default_currency: String,
    #[serde(default)]
    pub business_name: String,
    pub charges_enabled: bool,
    pub payouts_enabled: bool,
    pub details_submitted: bool,
    #[serde(default)]
    pub capabilities: BTreeMap<String, String>,
    pub publishable_key_configured: bool,
    pub webhook_secret_configured: bool,
    pub api_version: String,
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SellerOnboardingRequest {
    pub return_url: String,
    pub refresh_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SellerOnboardingResponse {
    pub account: SellerAccount,
    pub url: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRedirect {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingPortalRequest {
    pub return_url: String,
    #[serde(default)]
    pub order_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefundReason {
    Duplicate,
    Fraudulent,
    RequestedByCustomer,
}

impl RefundReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::Fraudulent => "fraudulent",
            Self::RequestedByCustomer => "requested_by_customer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RefundRequest {
    /// Exact amount in the order currency's minor unit. Omit to refund the
    /// complete remaining refundable amount.
    #[serde(default)]
    pub amount_minor: Option<i64>,
    /// Stripe's constrained provider reason. Human context belongs in `note`.
    #[serde(default)]
    pub provider_reason: Option<RefundReason>,
    /// Private operator note retained in ImpressPress, never sent to Stripe.
    #[serde(default, alias = "reason")]
    pub note: Option<String>,
    /// Stable client operation key. Supplying a fresh key allows a deliberate
    /// second partial refund of the same amount; retries must reuse the key.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefundResultStatus {
    Pending,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefundResult {
    pub purchase_id: String,
    #[serde(default)]
    pub refund_id: String,
    #[serde(default)]
    pub provider_refund_id: String,
    pub status: RefundResultStatus,
    #[serde(default)]
    pub provider_status: String,
    pub amount_minor: i64,
    pub refunded_total_minor: i64,
    pub order_total_minor: i64,
    pub currency: String,
    pub livemode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommerceAnalytics {
    pub currency: String,
    pub gross_volume_minor: i64,
    pub refunded_volume_minor: i64,
    pub net_volume_minor: i64,
    pub platform_fees_minor: i64,
    pub order_count: u64,
    pub paid_order_count: u64,
    pub refunded_order_count: u64,
    pub failed_order_count: u64,
    pub open_dispute_count: u64,
    pub open_disputed_volume_minor: i64,
    pub lost_dispute_count: u64,
    pub lost_disputed_volume_minor: i64,
    pub active_subscription_count: u64,
    pub trialing_subscription_count: u64,
    pub past_due_subscription_count: u64,
    pub canceled_subscription_count: u64,
    pub top_products: Vec<AnalyticsProduct>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsProduct {
    pub product_id: String,
    pub name: String,
    pub quantity: u64,
    pub revenue_minor: i64,
}

/// Ownership-safe failed-order projection for seller operational dashboards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SellerFailureSummary {
    pub order_id: String,
    pub status: String,
    pub currency: String,
    pub total_minor: i64,
    #[serde(default)]
    pub error: String,
    pub created_at: String,
}

/// Safe operational projection of a Stripe event. The signed payload and
/// processing owner are never serialized to the admin API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookEventSummary {
    pub id: String,
    pub event_type: String,
    pub status: String,
    #[serde(default)]
    pub stripe_account_id: String,
    pub livemode: bool,
    pub attempts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<String>,
    #[serde(default)]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookEventList {
    pub records: Vec<WebhookEventSummary>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

/// Safe administrator projection of a durable Stripe provider operation.
/// Request/response payloads, idempotency keys, and lease owners stay private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOperationSummary {
    pub id: String,
    pub operation_type: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    #[serde(default)]
    pub stripe_account_id: String,
    pub status: String,
    pub attempts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    #[serde(default)]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOperationList {
    pub records: Vec<ProviderOperationSummary>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderReconcileResult {
    pub claimed: u64,
    pub succeeded: u64,
    pub retry_scheduled: u64,
    pub dead_letter: u64,
}

#[cfg(test)]
mod tests {
    use super::{Condition, PricingPreviewRequest, TemplateKind};

    #[test]
    fn enums_use_stable_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_string(&TemplateKind::ConfigurableSubscription).unwrap(),
            "\"configurable_subscription\""
        );
    }

    #[test]
    fn requests_reject_unknown_fields() {
        let error = serde_json::from_value::<PricingPreviewRequest>(serde_json::json!({
            "offer_id": "offer_1",
            "surprise": true
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn recursive_conditions_have_explicit_operations() {
        let condition: Condition = serde_json::from_value(serde_json::json!({
            "op": "all",
            "conditions": [
                {"op": "present", "input": "size"},
                {"op": "not", "condition": {"op": "equals", "input": "rush", "value": false}}
            ]
        }))
        .unwrap();
        assert!(matches!(condition, Condition::All { .. }));
    }
}
