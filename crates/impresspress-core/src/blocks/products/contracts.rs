//! Stable, provider-neutral JSON contracts for the commerce APIs.
//!
//! # `#[schemars(required)]` on `Option<T>` is not decoration
//!
//! `.output::<T>()` generates under schemars' **serialize** contract, so a
//! response schema states what the server emits. That contract already gets
//! `required` right for `#[serde(default)]` and for `skip_serializing_if`.
//! What it does **not** fix is nullability: `Option<T>`'s `JsonSchema` impl
//! calls `allow_null` unconditionally, without consulting the contract, so
//! every `Option<String>` renders as `["string", "null"]` even when the
//! server omits the key rather than writing `null`.
//!
//! `#[schemars(required)]` is the one lever that drops that `null` branch,
//! under either contract. On a non-`Option` field it is genuinely a no-op —
//! which is why it is easy to mistake for inert everywhere — but on an
//! `Option<T>` it narrows `["T", "null"]` back to `"T"`. Paired with
//! `skip_serializing_if = "Option::is_none"` under the serialize contract it
//! does *not* also force the property into `required`, which is exactly the
//! shape these fields have: optional, and never `null` when present.
//!
//! Removing one of these widens the published response contract. Do not strip
//! them from an `Option<T>` field without changing what the handler emits.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wafer_core::clients::database::{Record, RecordList};
use wafer_run::{Message, WaferError};

use crate::util::{enum_column, json_map, RecordExt};

pub const COMMERCE_SCHEMA_VERSION: u32 = 1;

/// Moderation state of a **product**: the `approval_status` column of
/// `impresspress__products__products`.
///
/// The five variants describe the product moderation ladder a seller
/// listing walks — `draft` before submission, `pending` while an
/// administrator has it, then `approved`, `rejected` or `suspended`. It was
/// also the declared type of `SellerAccount.approval_status`, where only
/// two of the five could ever be produced (`repo::seller_accounts` derives
/// that field from one boolean); a seller account has never had a `draft`
/// or a `rejected` state, and publishing three unreachable values as part
/// of that contract said it did. The field is [`SellerApproval`] now.
///
/// Not to be confused with [`ProductStatus`], the *publication* state on
/// the same table's `status` column. They are two columns and two
/// vocabularies: a submitted listing is `status = pending_review` and
/// `approval_status = pending` at the same time, which review bug B11 read
/// as one value spelled two ways. `products::tests::status_enum_tests`
/// pins that they are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Draft,
    Pending,
    Approved,
    Rejected,
    Suspended,
}

/// Whether a seller account may sell: the two-valued projection
/// `SellerAccount.approval_status` publishes.
///
/// `repo::seller_accounts::to_contract` computes this from one fact —
/// whether the row's `status` is `suspended` — so `approved` and
/// `suspended` are the only values that have ever reached a client. This
/// type is that sentence; the five-variant [`ApprovalStatus`] it replaces
/// on this field describes a different column entirely.
///
/// - `approved` — not suspended. Whether the account can actually take
///   money is [`SellerStatus`] and [`SellerCapabilities`], not this.
/// - `suspended` — an administrator has suspended the account.
///
/// The variants carry no doc comments of their own, here or in any of this
/// module's other published status enums, and that is deliberate: schemars
/// renders a unit-variant enum as one flat `{"enum": [...]}` only while
/// every variant is undocumented. Describing one splits the published field
/// into a `oneOf` of `const` subschemas — a change to the schema every SDK
/// generator reads, for prose that belongs on the type anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SellerApproval {
    Approved,
    Suspended,
}

/// How far a seller account has got with Stripe Connect: the `status`
/// column of `impresspress__products__seller_accounts`.
///
/// A ladder, and it was computed by hand in three places
/// (`set_admin_suspended`, `sync_account`, `sync_account_event`) from the
/// same three booleans, plus a fourth literal in `ensure_for_user` and a
/// fifth in `stripe_provider`. `repo::seller_accounts::ladder` is the one
/// computation now; this is the one vocabulary.
///
/// - `not_started` — the row exists because the user asked to sell; no
///   Connect account has been created yet.
/// - `onboarding` — a Connect account exists and Stripe still wants
///   information.
/// - `restricted` — Stripe has the seller's details but has not enabled
///   charges.
/// - `active` — charges are enabled: the seller can be paid.
/// - `suspended` — an administrator has suspended the account. Outranks
///   every capability state: a suspended account stays suspended however
///   its Stripe capabilities move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SellerStatus {
    NotStarted,
    Onboarding,
    Restricted,
    Active,
    Suspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FulfillmentKind {
    None,
    Manual,
    Download,
    Entitlement,
    Webhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfferMode {
    Payment,
    Subscription,
}

/// Lifecycle state of an offer: the `status` column of
/// `impresspress__products__offers`.
///
/// The type existed before this PR but nothing wrote it: every transition
/// wrote a string literal and `repo::offers` re-spelled the three variants
/// back out for its compare-and-swap expectations, so the CAS guard and
/// the column could drift apart silently. Both are this type now.
///
/// - `draft` — editable; the only state whose definition may still change.
/// - `active` — published and purchasable; the definition is immutable.
/// - `archived` — withdrawn. Existing orders keep referring to it by
///   version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    Draft,
    Active,
    Archived,
}

/// Where an offer stands against its Stripe Product/Price: the
/// `sync_status` column of `impresspress__products__offers`.
///
/// Distinct from [`OfferStatus`]: an offer is `active` locally the moment
/// it is published, and only becomes `synced` once Stripe has the matching
/// Price. The two were both plain strings and both called "status" on the
/// same row.
///
/// `impresspress__products__payment_links` has a `sync_status` column with
/// a *different* value set (`not_synced`, `syncing`, `synced`, `error` —
/// note `error`, not `failed`). It is deliberately not typed with this
/// enum; giving it one means either changing a stored literal or carrying a
/// fourth spelling, and that is its own decision.
///
/// - `not_synced` — never sent to Stripe. The column's default.
/// - `syncing` — a synchronization is in flight.
/// - `synced` — Stripe holds a Product and Price matching this offer
///   version.
/// - `failed` — the last synchronization failed; `sync_error` says why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OfferSyncStatus {
    NotSynced,
    Syncing,
    Synced,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PricingModel {
    Fixed,
    Components,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecurringInterval {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsageType {
    Licensed,
    Metered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BillingScheme {
    PerUnit,
    Tiered,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TaxBehavior {
    #[default]
    Unspecified,
    Inclusive,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShippingEstimateUnit {
    Hour,
    Day,
    BusinessDay,
    Week,
    Month,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutPresentation {
    #[default]
    Hosted,
    Embedded,
    PaymentLink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VariableVisibility {
    #[default]
    Public,
    Hidden,
    AdminOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PricingTier {
    /// Inclusive upper bound for this tier. Only the final tier may omit it.
    #[serde(default)]
    pub up_to: Option<u64>,
    pub unit_amount_minor: i64,
    #[serde(default)]
    pub flat_amount_minor: i64,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PackageRounding {
    /// Charge one package for any partially used package.
    #[default]
    Up,
    /// Require the input to be an exact multiple of the package size.
    Exact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentRecurrence {
    pub interval: RecurringInterval,
    #[serde(default = "one_u32")]
    pub interval_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShippingDeliveryEstimate {
    #[serde(default)]
    pub minimum: Option<u32>,
    #[serde(default)]
    pub maximum: Option<u32>,
    pub unit: ShippingEstimateUnit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManagedOffer {
    pub status: OfferStatus,
    pub sync_status: OfferSyncStatus,
    #[serde(default)]
    pub sync_error: String,
    pub offer: Offer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

/// Which Stripe environment a validated publishable key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StripeMode {
    Test,
    Live,
}

/// Browser-safe deployment configuration. The Stripe secret key, webhook
/// secret, account ids, and provider API URL are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorefrontConfig {
    pub schema_version: u32,
    pub embedded_checkout_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(required)]
    pub stripe_publishable_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(required)]
    pub stripe_mode: Option<StripeMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PricingPreviewRequest {
    pub offer_id: String,
    #[serde(default = "one_u64")]
    #[schemars(range(min = 1))]
    pub quantity: u64,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckoutRequest {
    pub offer_id: String,
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default = "one_u64")]
    #[schemars(range(min = 1))]
    pub quantity: u64,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
    #[serde(default)]
    pub presentation: CheckoutPresentation,
    #[serde(default)]
    #[schemars(url)]
    pub success_url: Option<String>,
    #[serde(default)]
    #[schemars(url)]
    pub cancel_url: Option<String>,
    #[serde(default)]
    #[schemars(email)]
    pub buyer_email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckoutResponse {
    pub order_id: String,
    /// Returned once and never persisted in plaintext. Static storefronts use
    /// it to poll the minimal guest order-status endpoint after Stripe returns.
    ///
    /// A bearer capability: whoever holds it can read the order's status.
    /// Treat it like a session token — never log it, never put it in a URL
    /// that gets shared. This response is its only delivery, which is why it
    /// is not `writeOnly`: that keyword claims a field is never present in a
    /// response, and this one is always present in this one.
    pub receipt_token: String,
    #[schemars(extend("format" = "date-time"))]
    pub receipt_token_expires_at: String,
    pub presentation: CheckoutPresentation,
    #[serde(default)]
    #[schemars(url)]
    pub checkout_url: Option<String>,
    /// Stripe Embedded Checkout client secret. Present only for the embedded
    /// presentation; it is how the browser opens the session, so it is
    /// returned here and nowhere else. Same handling as `receipt_token`:
    /// never log it.
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    #[schemars(url)]
    pub payment_link_url: Option<String>,
    pub amounts: MoneyBreakdown,
}

/// Lifecycle state of an order: the `status` column of
/// `impresspress__products__purchases`.
///
/// This is the one definition of the column's value set. `repo::purchases`
/// stores these variants and filters on them, and the order views parse the
/// column back through [`Self::from_record`], so a stored value outside the
/// set is reported as a data-integrity error rather than published or
/// defaulted.
///
/// `pending`: created, checkout not yet claimed. `checkout_started`: a
/// provider Checkout Session was claimed for the order. `completed`: paid.
/// `partially_refunded` / `refunded`: paid, then refunded in part or in
/// full. `failed`: checkout or reconciliation failed; `reconciliation_error`
/// says why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    CheckoutStarted,
    Completed,
    PartiallyRefunded,
    Refunded,
    Failed,
}

impl OrderStatus {
    /// Every status this contract defines, in declaration order.
    ///
    /// The commerce analytics needs the value set as data, not as a `match`:
    /// it filters and groups the orders table by the stored `status`
    /// spelling, so it has to name the whole set in a query. Keeping the list
    /// here — beside the enum — is what stops a second, drifting copy of it
    /// growing inside `repo::purchases`.
    ///
    /// `status_enum_tests::order_status_all_lists_every_variant` keeps the
    /// list honest: its `slot` match is exhaustive, so a seventh variant
    /// stops that test compiling, and its assertion then checks the variant
    /// reached this array too.
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::CheckoutStarted,
        Self::Completed,
        Self::PartiallyRefunded,
        Self::Refunded,
        Self::Failed,
    ];

    /// Parse the `status` column of an order row.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        enum_column(record, "status")
    }

    /// Whether the order was paid: `completed`, or paid and then refunded.
    pub const fn is_paid(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::PartiallyRefunded | Self::Refunded
        )
    }

    /// Whether a further refund may be issued against the order.
    pub const fn is_refundable(self) -> bool {
        matches!(self, Self::Completed | Self::PartiallyRefunded)
    }

    /// Whether checkout has not completed yet: `pending` or
    /// `checkout_started`.
    pub const fn awaits_completion(self) -> bool {
        matches!(self, Self::Pending | Self::CheckoutStarted)
    }
}

/// Where an order stands against the payment provider's view of it: the
/// `reconciliation_status` column of `impresspress__products__purchases`.
///
/// This is the one definition of the column's value set. `repo::purchases`
/// and `stripe` store these variants, and the order views parse the column
/// back through [`Self::from_record`], so a stored value outside the set is
/// reported as a data-integrity error rather than published or defaulted.
///
/// `pending`: row created, no provider session yet. `awaiting_payment`: a
/// Checkout Session exists and the customer has not paid. `reconciled`: the
/// completed session matched the local snapshot. `provider_error`: the
/// provider's answer was unusable or contradicted the snapshot;
/// `reconciliation_error` says why. The `payment_*` values mirror the last
/// PaymentIntent event received before Checkout completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationStatus {
    Pending,
    AwaitingPayment,
    Reconciled,
    ProviderError,
    PaymentSucceededAwaitingCheckout,
    PaymentFailed,
    PaymentProcessing,
    PaymentRequiresAction,
    PaymentCanceled,
}

impl ReconciliationStatus {
    /// Parse the `reconciliation_status` column of an order row.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        enum_column(record, "reconciliation_status")
    }
}

/// The provider's own view of the order's payment: the
/// `provider_payment_status` column of
/// `impresspress__products__purchases`.
///
/// Stripe's PaymentIntent vocabulary, owned here because it is our column.
/// It is not [`ReconciliationStatus`], which is *our* reading of the same
/// events; both live on the same row and the two were both bare strings.
///
/// [`Self::Unset`] is a real state, not a null stand-in: the column is
/// `TEXT NOT NULL DEFAULT ''` (`016_payment_intent_state.sqlite.sql`) and
/// every order carries it until a PaymentIntent event arrives, which is
/// why `""` has always been the first entry in this field's published
/// value list. Modelling it as a variant is what keeps that list — and
/// every stored row — exactly as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPaymentStatus {
    #[serde(rename = "")]
    Unset,
    Succeeded,
    PaymentFailed,
    Processing,
    RequiresAction,
    Canceled,
}

/// A subscription's lifecycle state, as Stripe reports it: the
/// `subscription_status` column of `impresspress__products__purchases`.
///
/// The complete Stripe set, not the subset the repo happens to write
/// today. `sync_commerce_subscription` copies `data.object.status` from a
/// `customer.subscription.*` delivery straight into the column, so any
/// status Stripe can emit can be stored — including `incomplete`, which
/// the phase spec's variant list omits. Typing the column with a narrower
/// set than its writer accepts would turn a legitimate Stripe state into a
/// decode failure on the order view, so the type carries all of them.
///
/// [`Self::Unset`] is the state of every non-subscription order: the
/// column is `TEXT NOT NULL DEFAULT ''`
/// (`009_commerce_subscription_state.sqlite.sql`).
///
/// The same vocabulary is the `status` column of the platform-billing
/// projection (`impresspress__products__subscriptions`).
//
// Each variant has exactly one spelling, Stripe's, and it is the only one
// either column stores: every write serialises a variant, and migration
// `022_canonical_subscription_status` rewrote the British `cancelled` the
// projection used to store. A stored value outside the set — that spelling
// included — is refused by `enum_column` as a data fault, so the lifecycle
// comparisons and the compare-and-swap filters, which compare the stored
// text against a serialised variant, always compare like with like.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    #[default]
    #[serde(rename = "")]
    Unset,
    Incomplete,
    IncompleteExpired,
    Trialing,
    Active,
    PastDue,
    Unpaid,
    Paused,
    Canceled,
}

impl SubscriptionStatus {
    /// Every variant, in declaration order. SQL cannot ask the type which
    /// statuses are terminal, so a filter over the column names them from
    /// this list.
    pub const ALL: [Self; 9] = [
        Self::Unset,
        Self::Incomplete,
        Self::IncompleteExpired,
        Self::Trialing,
        Self::Active,
        Self::PastDue,
        Self::Unpaid,
        Self::Paused,
        Self::Canceled,
    ];

    /// Parse the `subscription_status` column of an order row.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        enum_column(record, "subscription_status")
    }

    /// Whether this is the column default: the order is not a subscription.
    pub const fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }

    /// Schema of a field that is omitted rather than sent as
    /// [`Self::Unset`]: this type's own schema without the value `Unset`
    /// serialises to, since that value never reaches the wire.
    pub fn set_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = <Self as schemars::JsonSchema>::json_schema(generator);
        if let Some(values) = schema
            .get_mut("enum")
            .and_then(serde_json::Value::as_array_mut)
        {
            values.retain(|value| {
                serde_json::from_value::<Self>(value.clone())
                    .map_or(true, |status| !status.is_unset())
            });
        }
        schema
    }

    /// Whether the subscription can never become live again.
    ///
    /// Stripe issues a new subscription id for a resubscription, so a
    /// canceled or expired projection is final for its id. This is what
    /// `repo::subscription_transition_allowed` refuses to move away from.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Canceled | Self::IncompleteExpired)
    }

    /// How far along the lifecycle this status is, for ordering
    /// same-second webhook deliveries: terminal outranks delinquency,
    /// which outranks every live status. Immediate cancellation makes
    /// Stripe emit `customer.subscription.updated` and
    /// `customer.subscription.deleted` with the same `created` second;
    /// ranking keeps the deletion authoritative regardless of delivery
    /// order.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Canceled | Self::IncompleteExpired => 2,
            Self::PastDue | Self::Unpaid => 1,
            Self::Unset | Self::Incomplete | Self::Trialing | Self::Active | Self::Paused => 0,
        }
    }
}

/// Minimal order state exposed to a guest who presents the checkout receipt
/// capability. Buyer details and all Stripe resource ids remain private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuestOrderStatus {
    pub schema_version: u32,
    pub order_id: String,
    /// Lifecycle state of the order.
    pub status: OrderStatus,
    /// Where the order stands against the provider's view of it.
    pub reconciliation_status: ReconciliationStatus,
    pub amounts: MoneyBreakdown,
    /// Stripe subscription lifecycle state; absent for a one-time order.
    #[serde(default, skip_serializing_if = "SubscriptionStatus::is_unset")]
    #[schemars(schema_with = "SubscriptionStatus::set_schema")]
    pub subscription_status: SubscriptionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub subscription_current_period_end: Option<String>,
    pub subscription_cancel_at_period_end: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub paid_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub refunded_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckoutPresetRequest {
    pub name: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

/// Every checkout preset defined for one offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckoutPresetList {
    pub presets: Vec<CheckoutPreset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaymentLinkCreateRequest {
    #[serde(default)]
    pub preset_id: Option<String>,
    /// Where Stripe sends the buyer once the Payment Link is paid.
    #[serde(default)]
    #[schemars(url)]
    pub after_completion_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManagedPaymentLink {
    pub id: String,
    pub offer_id: String,
    #[serde(default)]
    pub preset_id: String,
    /// Stripe-hosted Payment Link the buyer opens.
    #[schemars(url)]
    pub url: String,
    pub active: bool,
    pub configuration_hash: String,
    pub sync_status: String,
    #[serde(default)]
    pub sync_error: String,
}

/// Every reusable Stripe Payment Link synchronized for one offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaymentLinkList {
    pub payment_links: Vec<ManagedPaymentLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SellerCapabilities {
    pub details_submitted: bool,
    pub charges_enabled: bool,
    pub payouts_enabled: bool,
    #[serde(default)]
    pub requirements_due: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SellerAccount {
    pub id: String,
    pub user_id: String,
    /// How far the account has got with Stripe Connect.
    pub status: SellerStatus,
    /// Whether an administrator has suspended the account. Derived from
    /// `status`; the two move together.
    pub approval_status: SellerApproval,
    #[serde(default)]
    pub stripe_account_id: String,
    pub capabilities: SellerCapabilities,
    /// The platform application fee new Checkout Sessions and Payment Links carry, in basis points.
    #[schemars(range(max = 10000))]
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

/// Every seller account known to the platform, with its capability state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SellerAccountList {
    pub sellers: Vec<SellerAccount>,
    /// How many seller accounts exist, which is not `sellers.len()` when the
    /// listing is showing a prefix.
    pub total_count: i64,
    /// Whether more seller accounts exist than `sellers` lists.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StripeConnectionState {
    NotConfigured,
    ConnectedTest,
    ConnectedLive,
    Misconfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SellerOnboardingRequest {
    /// Where Stripe returns the seller once onboarding is submitted.
    #[schemars(url)]
    pub return_url: String,
    /// Where Stripe returns the seller if the onboarding link expired.
    #[schemars(url)]
    pub refresh_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SellerOnboardingResponse {
    pub account: SellerAccount,
    /// Single-use Stripe-hosted onboarding link.
    #[schemars(url)]
    pub url: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderRedirect {
    /// Absolute provider-hosted URL the browser must be sent to.
    #[schemars(url)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BillingPortalRequest {
    /// Where Stripe returns the customer when they leave the portal.
    #[schemars(url)]
    pub return_url: String,
    #[serde(default)]
    pub order_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefundRequest {
    /// Exact amount in the order currency's minor unit. Omit to refund the
    /// complete remaining refundable amount.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub amount_minor: Option<i64>,
    /// Stripe's constrained provider reason. Human context belongs in `note`.
    #[serde(default)]
    pub provider_reason: Option<RefundReason>,
    /// Private operator note retained in ImpressPress, never sent to Stripe.
    #[serde(default, alias = "reason")]
    #[schemars(length(max = 500))]
    pub note: Option<String>,
    /// Stable client operation key. Supplying a fresh key allows a deliberate
    /// second partial refund of the same amount; retries must reuse the key.
    #[serde(default)]
    #[schemars(length(max = 80))]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefundResultStatus {
    Pending,
    Succeeded,
    Failed,
}

/// Ledger state of a refund row: the `status` column of
/// `impresspress__products__refunds`.
///
/// Not the same set as [`RefundResultStatus`], which is the answer one
/// refund *request* gets, and not the same as the row's `provider_status`,
/// which is Stripe's own state for the refund. The distinguishing variant
/// is [`Self::ProviderSucceeded`]: the provider has paid the money back but
/// the order's refunded total has not been settled yet, a local-only state
/// with no counterpart at Stripe. It is why the column needs its own type
/// rather than reusing either neighbour.
///
/// - `pending` — recorded; the provider has not answered.
/// - `provider_succeeded` — the provider refunded; the order total has not
///   been settled yet.
/// - `succeeded` — settled against the order's refunded total.
/// - `failed` — the provider refused or canceled the refund.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefundStatus {
    Pending,
    ProviderSucceeded,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsProduct {
    pub product_id: String,
    pub name: String,
    pub quantity: u64,
    pub revenue_minor: i64,
}

/// Ownership-safe failed-order projection for seller operational dashboards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SellerFailureSummary {
    pub order_id: String,
    /// `failed` for a terminal failure; otherwise the state of an order whose
    /// last PaymentIntent event needs the seller's attention.
    pub status: OrderStatus,
    pub currency: String,
    pub total_minor: i64,
    #[serde(default)]
    pub error: String,
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
}

/// Platform-wide commerce counters for the admin dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminStats {
    pub total_products: i64,
    pub active_products: i64,
    pub total_purchases: i64,
    /// One entry per currency the platform has transacted in.
    pub currency_analytics: Vec<CommerceAnalytics>,
    pub total_groups: i64,
}

/// One seller's own commerce counters and recent operational failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SellerStats {
    /// Empty when the user has not started Stripe onboarding.
    pub seller_account_id: String,
    /// One entry per currency this seller has transacted in.
    pub currency_analytics: Vec<CommerceAnalytics>,
    pub recent_failures: Vec<SellerFailureSummary>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Acknowledgement returned to Stripe for a delivered webhook event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WebhookAck {
    pub received: bool,
    /// Present only when the event id had already been recorded, in which
    /// case no side effect ran.
    #[serde(default, skip_serializing_if = "is_false")]
    pub duplicate: bool,
    /// Present only when the event exhausted its retry budget.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dead_letter: bool,
}

impl WebhookAck {
    /// The event was accepted and processed.
    pub fn received() -> Self {
        Self {
            received: true,
            duplicate: false,
            dead_letter: false,
        }
    }

    /// The event id was already recorded, so no side effect ran.
    pub fn duplicate() -> Self {
        Self {
            received: true,
            duplicate: true,
            dead_letter: false,
        }
    }

    /// The event exhausted its retry budget.
    pub fn dead_letter() -> Self {
        Self {
            received: true,
            duplicate: false,
            dead_letter: true,
        }
    }
}

/// Declare [`StripeEventType`] and the list of every variant in one place.
///
/// `ALL` has to be complete or the setup page under-reports what the
/// destination must subscribe to, and a hand-written `ALL` beside a
/// hand-written enum is exactly the second list this type exists to remove.
/// Generating both from one `Variant => "wire.name"` table is what makes
/// completeness a property of the declaration rather than of a test.
macro_rules! stripe_event_types {
    ($(#[$doc:meta])* $($variant:ident => $wire:literal,)+) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum StripeEventType {
            $(#[serde(rename = $wire)] $variant,)+
        }

        impl StripeEventType {
            /// Every handled type, in the order the setup page lists them.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
        }
    };
}

stripe_event_types! {
    /// A Stripe webhook `type` this block does something with.
    ///
    /// Not a column: `stripe_events.event_type` records whatever Stripe
    /// sent, including the types this block ignores, so
    /// [`WebhookEventSummary`] keeps publishing that field as a string.
    /// This type is the *handled* set — the one thing the webhook
    /// dispatcher and the Stripe setup page both need to agree on, and did
    /// not, because each spelled the 21 names out on its own.
    ///
    /// [`Self::from_wire`] returning `None` is not an error. A destination
    /// can be subscribed to more event types than this block handles
    /// (Stripe's dashboard offers "all events"), so an unhandled type is
    /// ordinary traffic: the dispatcher ignores it and acknowledges the
    /// delivery, which is what its former `_ =>` arm did.
    ///
    /// The variants carry no doc comments deliberately. Nothing publishes
    /// this type today, but the moment something does, per-variant docs
    /// turn a flat `{"enum": [...]}` into a `oneOf` of `const` subschemas
    /// and move the snapshot for no reason — the trap PR #30 hit on eight
    /// fields.
    AccountUpdated => "account.updated",
    CheckoutSessionCompleted => "checkout.session.completed",
    CheckoutSessionAsyncPaymentSucceeded => "checkout.session.async_payment_succeeded",
    CheckoutSessionAsyncPaymentFailed => "checkout.session.async_payment_failed",
    PaymentIntentSucceeded => "payment_intent.succeeded",
    PaymentIntentPaymentFailed => "payment_intent.payment_failed",
    PaymentIntentProcessing => "payment_intent.processing",
    PaymentIntentRequiresAction => "payment_intent.requires_action",
    PaymentIntentCanceled => "payment_intent.canceled",
    CustomerSubscriptionUpdated => "customer.subscription.updated",
    CustomerSubscriptionDeleted => "customer.subscription.deleted",
    InvoicePaid => "invoice.paid",
    InvoicePaymentSucceeded => "invoice.payment_succeeded",
    InvoicePaymentFailed => "invoice.payment_failed",
    ChargeDisputeCreated => "charge.dispute.created",
    ChargeDisputeUpdated => "charge.dispute.updated",
    ChargeDisputeClosed => "charge.dispute.closed",
    RefundCreated => "refund.created",
    RefundUpdated => "refund.updated",
    RefundFailed => "refund.failed",
    ChargeRefunded => "charge.refunded",
}

impl StripeEventType {
    /// The event `type` of a delivery, or `None` for a type this block does
    /// not handle.
    ///
    /// The counterpart of [`crate::util::wire_str`], through the same serde
    /// definition, so the name decoded here is the name the setup page
    /// rendered.
    pub fn from_wire(value: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(value.to_string())).ok()
    }
}

/// Where a received Stripe event is in the block's own processing queue:
/// the `status` column of `impresspress__products__stripe_events`.
///
/// The five values were five `const &str` in `stripe.rs`, used both as
/// written values and as match patterns against `record.str_field`, and
/// re-spelled a second time in the admin filter and a third in this
/// module's schema. `stripe.rs` matches this type now, so a variant added
/// here stops the lease state machine compiling rather than falling
/// through one of its arms.
///
/// Deliberately NOT merged with [`OperationStatus`], which differs in
/// exactly one variant (`processed` here, `succeeded` there): reconciling
/// them changes stored values, and this phase does not.
///
/// - `pending` — recorded, not yet leased.
/// - `processing` — a delivery holds the processing lease.
/// - `failed` — the last attempt failed; `next_retry_at` schedules another.
/// - `processed` — handled; further deliveries of the same event id are
///   ignored.
/// - `dead_letter` — out of attempts. Only a replay moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Pending,
    Processing,
    Failed,
    Processed,
    DeadLetter,
}

/// Safe operational projection of a Stripe event. The signed payload and
/// processing owner are never serialized to the admin API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookEventSummary {
    pub id: String,
    pub event_type: String,
    /// One of `pending`, `processing`, `failed`, `processed`, `dead_letter`.
    pub status: EventStatus,
    #[serde(default)]
    pub stripe_account_id: String,
    pub livemode: bool,
    pub attempts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub processing_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub next_retry_at: Option<String>,
    #[serde(default)]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub processed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub terminal_at: Option<String>,
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WebhookEventList {
    pub records: Vec<WebhookEventSummary>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

/// Where a durable provider operation is in its retry schedule: the
/// `status` column of `impresspress__products__provider_operations`.
///
/// One variant apart from [`EventStatus`] — a completed operation is
/// `succeeded`, a handled event is `processed` — and the two were both
/// written as bare strings against columns named `status` on tables read
/// by the same admin page. Merging the vocabularies would change stored
/// values, so they stay two types and the difference is stated here.
///
/// - `pending` — enqueued, not yet leased.
/// - `processing` — a worker holds the lease.
/// - `failed` — the last attempt failed; `next_attempt_at` schedules
///   another.
/// - `succeeded` — completed.
/// - `dead_letter` — out of attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Pending,
    Processing,
    Failed,
    Succeeded,
    DeadLetter,
}

/// Safe administrator projection of a durable Stripe provider operation.
/// Request/response payloads, idempotency keys, and lease owners stay private.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderOperationSummary {
    pub id: String,
    /// Currently only `refund.reconcile`.
    #[schemars(extend("enum" = ["refund.reconcile"]))]
    pub operation_type: String,
    /// Currently only `refund`.
    #[schemars(extend("enum" = ["refund"]))]
    pub aggregate_type: String,
    pub aggregate_id: String,
    #[serde(default)]
    pub stripe_account_id: String,
    /// One of `pending`, `processing`, `failed`, `succeeded`, `dead_letter`.
    pub status: OperationStatus,
    pub attempts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub processing_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub next_attempt_at: Option<String>,
    #[serde(default)]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    #[schemars(required)]
    pub terminal_at: Option<String>,
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderOperationList {
    pub records: Vec<ProviderOperationSummary>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderReconcileResult {
    pub claimed: u64,
    pub succeeded: u64,
    pub retry_scheduled: u64,
    pub dead_letter: u64,
    /// Operations whose state could not be written; a later run retries them.
    // A claim, dead-letter or outcome write that failed. The operation keeps
    // its previous state (a lost outcome leaves it leased until the lease
    // lapses) and each failure is logged with its operation id.
    pub unrecorded: u64,
}

// ---------------------------------------------------------------------------
// Catalog rows — typed views of the block's own tables
// ---------------------------------------------------------------------------
//
// Until these existed the product, group, type and purchase endpoints echoed
// the database layer's `Record` / `RecordList` (`{id, data: {column → value}}`)
// straight to the wire, so a response was whatever the row held and its JSON
// types depended on the backend (`tags` / `metadata` are JSON-encoded `TEXT`
// that only SQLite decodes; `is_system` / `livemode` are `INTEGER`). Each view
// below is a closed field list built column by column, normalized through
// `RecordExt`, so a column added by a migration is never published by
// accident and one schema is true on SQLite, D1 and Postgres.
//
// The `{records, total_count, page, page_size}` list envelope is unchanged;
// only the row moves from `{id, data: {…}}` to the flat view.

/// A nullable timestamp column as `Option<String>`.
///
/// The moderation path writes `published_at = ""` when it returns a product to
/// draft, so the column holds `NULL`, the empty string, or an RFC 3339
/// timestamp. The empty string is not a `date-time`; it reads as `None` so the
/// declared format is true.
fn timestamp_field(record: &Record, key: &str) -> Option<String> {
    record.opt_str_field(key).filter(|value| !value.is_empty())
}

/// An absent query parameter and an empty one mean the same thing to every
/// filter here (`msg.query` returns `""` for both), so collapse them onto
/// `None` rather than letting `Some("")` reach a `LIKE '%%'`.
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// Default page size for every paginated list in this block.
const DEFAULT_PAGE_SIZE: u32 = 20;

/// `?page` default.
fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    DEFAULT_PAGE_SIZE
}

/// Turn a request struct into the column map the database client writes.
///
/// Every optional field carries `skip_serializing_if = "Option::is_none"`, so
/// a key the client did not send is not written — an update touches only what
/// arrived, and an explicit `null` is treated the same as an absent key rather
/// than becoming a `NULL` write against a `NOT NULL` column.
fn columns<T: Serialize>(request: &T) -> HashMap<String, Value> {
    json_map(serde_json::to_value(request).expect("request structs serialize"))
}

/// Publication state of a product row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProductStatus {
    Draft,
    PendingReview,
    Active,
    Archived,
}

/// A product row as published to its owner and to administrators: every
/// column of the products table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProductView {
    /// Stable product identifier.
    pub id: String,
    pub name: String,
    pub description: String,
    /// URL slug, unique per owner among non-deleted products. Empty when the
    /// product has none.
    pub slug: String,
    /// ISO 4217 presentment currency.
    pub currency: String,
    /// Publication state: `draft`, `pending_review` (seller product awaiting
    /// moderation), `active` (in the public catalog) or `archived`.
    pub status: ProductStatus,
    pub category: String,
    pub tags: Vec<String>,
    /// Free-form key/value metadata attached by the product builder.
    pub metadata: serde_json::Map<String, Value>,
    pub image_url: String,
    /// Units in stock; `0` when inventory is not tracked.
    pub stock: i64,
    /// Group the product is listed under, or empty.
    pub group_id: String,
    /// Product type (taxonomy) id, or empty.
    pub type_id: String,
    pub group_template_id: String,
    /// Product builder template this product was created from.
    pub product_template_id: String,
    /// Id of a product the buyer must already own before checkout, or empty.
    pub requires: String,
    /// Id of the user who created the row.
    pub created_by: String,
    /// `platform` for an administrator-owned product, `user` for a seller's.
    #[schemars(extend("enum" = ["platform", "user"]))]
    pub owner_kind: String,
    /// Owning seller's user id; empty for platform products.
    pub owner_id: String,
    /// Seller account the product sells through; empty for platform products.
    pub seller_account_id: String,
    /// Moderation state: `draft`, `pending` (submitted for review), `approved`,
    /// `rejected` or `suspended`. A different column and a different
    /// vocabulary from `status` — a listing awaiting review is
    /// `status = pending_review` and `approval_status = pending` at once.
    pub approval_status: ApprovalStatus,
    /// How a purchase is fulfilled.
    #[schemars(extend("enum" = ["none", "manual", "download", "entitlement", "webhook"]))]
    pub fulfillment_kind: String,
    /// Stripe Product id once the catalog has been synchronized, or empty.
    pub stripe_product_id: String,
    /// Version counter of the product's immutable offer definitions.
    #[schemars(range(min = 1))]
    pub current_version: i64,
    /// RFC 3339 timestamp the seller last submitted the product for
    /// moderation, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub submitted_at: Option<String>,
    /// RFC 3339 timestamp the product last became active, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub published_at: Option<String>,
    /// RFC 3339 soft-delete timestamp, or `null` unless the product has been
    /// soft-deleted.
    #[schemars(extend("format" = "date-time"))]
    pub deleted_at: Option<String>,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl ProductView {
    /// Project a products-table row.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            name: record.str_field("name").to_string(),
            description: record.str_field("description").to_string(),
            slug: record.str_field("slug").to_string(),
            currency: record.str_field("currency").to_string(),
            status: enum_column(record, "status")?,
            category: record.str_field("category").to_string(),
            tags: record.string_list_field("tags"),
            metadata: record.json_object_field("metadata"),
            image_url: record.str_field("image_url").to_string(),
            stock: record.i64_field("stock"),
            group_id: record.str_field("group_id").to_string(),
            type_id: record.str_field("type_id").to_string(),
            group_template_id: record.str_field("group_template_id").to_string(),
            product_template_id: record.str_field("product_template_id").to_string(),
            requires: record.str_field("requires").to_string(),
            created_by: record.str_field("created_by").to_string(),
            owner_kind: record.str_field("owner_kind").to_string(),
            owner_id: record.str_field("owner_id").to_string(),
            seller_account_id: record.str_field("seller_account_id").to_string(),
            approval_status: enum_column(record, "approval_status")?,
            fulfillment_kind: record.str_field("fulfillment_kind").to_string(),
            stripe_product_id: record.str_field("stripe_product_id").to_string(),
            current_version: record.i64_field("current_version"),
            submitted_at: timestamp_field(record, "submitted_at"),
            published_at: timestamp_field(record, "published_at"),
            deleted_at: timestamp_field(record, "deleted_at"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

// The public catalog row. Nine columns of the products table
// that `ProductView` publishes to owners and administrators are deliberately
// NOT published here, on the `AuthLevel::Public` catalog. The reasons live
// in this plain comment rather than the doc comment: a `///` line is
// published as the schema's `description`, and the public contract has no
// reason to name the columns it withholds. The untyped handler only ever
// emitted them because it echoed the whole row; `StorefrontProduct`, the
// block's other public product projection, has never carried any of them
// ("internal ownership, provider, and pricing-rule fields are omitted").
//
// * `owner_id`, `created_by` — user ids. A guest has no business learning
//   which account owns or created a listing.
// * `seller_account_id` — the seller's account row id; an internal handle
//   that pairs with the two above.
// * `owner_kind` — `platform` / `user`; the ownership flag the storefront
//   projection also omits.
// * `stripe_product_id` — a provider resource id. Never on a public surface.
// * `approval_status`, `submitted_at` — moderation state and timestamp.
//   Every catalog row is `active`, which is the only moderation fact a
//   buyer needs.
// * `current_version` — the counter behind the immutable offer definitions;
//   an internal handle the storefront projection also omits.
// * `deleted_at` — soft-delete bookkeeping.
//
// Kept, as the row has always published them and nothing about them is
// internal: the catalog content (`name`, `slug`, `description`, `image_url`,
// `tags`, `category`, `currency`, `stock`, `fulfillment_kind`), the taxonomy
// ids a storefront filters by (`group_id`, `type_id`), the builder template
// ids, `requires` (a product id, and the checkout error names it anyway),
// `metadata`, `status` (always `active` here) and the public timestamps.
/// A product as the public catalog publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CatalogProductView {
    /// Stable product identifier.
    pub id: String,
    pub name: String,
    /// URL slug, or empty.
    pub slug: String,
    pub description: String,
    pub image_url: String,
    pub tags: Vec<String>,
    pub category: String,
    /// ISO 4217 presentment currency.
    pub currency: String,
    // The *type* is the whole column's, because a projection that narrowed
    // it to the one value the query happens to select would have to be
    // widened again the moment the query did. The *published value list*
    // stays the one value, because that is a true statement about this
    // endpoint's responses and re-narrowing it later would break every
    // consumer that had widened to match — so the `extend` stays too.
    /// Always `active`: the catalog lists active products only.
    #[schemars(extend("enum" = ["active"]))]
    pub status: ProductStatus,
    /// Units in stock; `0` when inventory is not tracked.
    pub stock: i64,
    /// Group the product is listed under, or empty.
    pub group_id: String,
    /// Product type (taxonomy) id, or empty.
    pub type_id: String,
    pub group_template_id: String,
    /// Product builder template this product was created from.
    pub product_template_id: String,
    /// Id of a product the buyer must already own before checkout, or empty.
    pub requires: String,
    /// Free-form key/value metadata attached by the product builder.
    pub metadata: serde_json::Map<String, Value>,
    /// How a purchase is fulfilled.
    #[schemars(extend("enum" = ["none", "manual", "download", "entitlement", "webhook"]))]
    pub fulfillment_kind: String,
    /// RFC 3339 timestamp the product last became active, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub published_at: Option<String>,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl CatalogProductView {
    /// Project a products-table row for the public catalog.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            name: record.str_field("name").to_string(),
            slug: record.str_field("slug").to_string(),
            description: record.str_field("description").to_string(),
            image_url: record.str_field("image_url").to_string(),
            tags: record.string_list_field("tags"),
            category: record.str_field("category").to_string(),
            currency: record.str_field("currency").to_string(),
            status: enum_column(record, "status")?,
            stock: record.i64_field("stock"),
            group_id: record.str_field("group_id").to_string(),
            type_id: record.str_field("type_id").to_string(),
            group_template_id: record.str_field("group_template_id").to_string(),
            product_template_id: record.str_field("product_template_id").to_string(),
            requires: record.str_field("requires").to_string(),
            metadata: record.json_object_field("metadata"),
            fulfillment_kind: record.str_field("fulfillment_kind").to_string(),
            published_at: timestamp_field(record, "published_at"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

/// One page of the public catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CatalogProductListResponse {
    /// Active products on this page, sorted by name.
    pub records: Vec<CatalogProductView>,
    /// Total active products, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl CatalogProductListResponse {
    /// Project a `RecordList` of active product rows. Same row-not-page
    /// degradation as [`PurchaseListResponse::from_record_list`]: a row
    /// whose `status` is outside the contract costs that row, not the
    /// page, and is logged at ERROR with its id.
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .filter_map(|record| match CatalogProductView::from_record(record) {
                    Ok(view) => Some(view),
                    Err(e) => {
                        tracing::error!(
                            product_id = %record.id,
                            error = %e,
                            "product row is outside the published contract and was omitted from the page"
                        );
                        None
                    }
                })
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// One page of product rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProductListResponse {
    /// Products on this page, newest first.
    pub records: Vec<ProductView>,
    /// Total products matching the filters, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl ProductListResponse {
    /// Project a `RecordList` of product rows. Same row-not-page
    /// degradation as [`PurchaseListResponse::from_record_list`].
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .filter_map(|record| match ProductView::from_record(record) {
                    Ok(view) => Some(view),
                    Err(e) => {
                        tracing::error!(
                            product_id = %record.id,
                            error = %e,
                            "product row is outside the published contract and was omitted from the page"
                        );
                        None
                    }
                })
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// Query parameters accepted by the list endpoints that paginate and do
/// nothing else.
///
/// Built by [`Self::from_message`], which is the handler's only source for
/// these values — the type is the parser, not a parallel description of one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
pub struct PageQuery {
    /// 1-based page number. Values below 1 clamp to 1.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Rows per page, capped at 100.
    #[serde(default = "default_page_size")]
    #[schemars(range(max = 100))]
    pub page_size: u32,
}

impl PageQuery {
    /// Resolve the query string on `msg`, applying the same defaults and
    /// clamps the handler applied inline before this type existed.
    pub fn from_message(msg: &Message) -> Self {
        let (page, page_size, _) = msg.pagination_params(DEFAULT_PAGE_SIZE as usize);
        Self {
            page: page as u32,
            page_size: page_size as u32,
        }
    }
}

/// Query parameters accepted by the product list endpoints.
///
/// Built by [`Self::from_message`], which is the handler's only source for
/// these values — the type is the parser, not a parallel description of one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
pub struct ProductListQuery {
    /// 1-based page number. Values below 1 clamp to 1.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Rows per page, capped at 100.
    #[serde(default = "default_page_size")]
    #[schemars(range(max = 100))]
    pub page_size: u32,
    /// Exact-match filter on the product's group.
    pub group_id: Option<String>,
    /// Exact-match filter on the publication state.
    pub status: Option<String>,
    /// `LIKE '%…%'` filter on the product name.
    pub search: Option<String>,
}

impl ProductListQuery {
    /// Resolve the query string on `msg`, applying the same defaults and
    /// clamps the handler applied inline before this type existed.
    pub fn from_message(msg: &Message) -> Self {
        let (page, page_size, _) = msg.pagination_params(DEFAULT_PAGE_SIZE as usize);
        Self {
            page: page as u32,
            page_size: page_size as u32,
            group_id: non_empty(msg.query("group_id")),
            status: non_empty(msg.query("status")),
            search: non_empty(msg.query("search")),
        }
    }
}

/// Longest product slug, in bytes. The wizard derives a slug from the name
/// and cuts it to this length, and both edit forms cap their input at it.
pub const PRODUCT_SLUG_MAX_LEN: usize = 160;

/// The product slug grammar, as the published schema and the edit forms'
/// `pattern` state it. The empty alternative is the column default: a
/// product with no web address.
pub const PRODUCT_SLUG_PATTERN: &str = "^(?:[a-z0-9]+(?:-[a-z0-9]+)*)?$";

/// Refuse a product slug outside [`PRODUCT_SLUG_PATTERN`] or longer than
/// [`PRODUCT_SLUG_MAX_LEN`], with the message the caller is shown.
pub fn check_product_slug(slug: &str) -> Result<(), String> {
    if slug.len() > PRODUCT_SLUG_MAX_LEN {
        return Err(format!(
            "slug must be at most {PRODUCT_SLUG_MAX_LEN} characters"
        ));
    }
    let well_formed = slug.is_empty()
        || slug.split('-').all(|run| {
            !run.is_empty()
                && run
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    if !well_formed {
        return Err(
            "slug may contain only lowercase letters, numbers, and single hyphens between them"
                .to_string(),
        );
    }
    Ok(())
}

/// `value` brought into the product slug grammar, at most `max_len` bytes:
/// ASCII letters lowercased, every run of anything else one hyphen, no
/// hyphen at either end. Empty when `value` has no ASCII letter or digit.
pub fn slug_from(value: &str, max_len: usize) -> String {
    let mut slug = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(max_len);
    slug.trim_end_matches('-').to_string()
}

// The write requests are closed field lists too, and that is the point: the
// column-map path they replace wrote every key it was sent. On the seller
// create path that included `seller_account_id`, `stripe_product_id`,
// `current_version`, `published_at`, `submitted_at` and `deleted_at` — the
// update path stripped them, the create path did not. A field that is not
// declared here cannot reach the row from any tier. Unknown keys are ignored
// rather than refused so that a client sending one of those protected columns
// gets the same answer it always did on update: the row, unchanged in that
// column.
/// `POST /b/products/api/admin/products` and `POST /b/products/api/products`
/// request body. Ownership, moderation and provider columns are set by the
/// server and cannot be supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateProductRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Web address of the product: lowercase letters and digits in runs
    /// joined by single hyphens, at most 160 characters. Empty means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = PRODUCT_SLUG_MAX_LEN), regex(pattern = PRODUCT_SLUG_PATTERN))]
    pub slug: Option<String>,
    /// ISO 4217 currency. A seller product defaults to the platform default
    /// currency when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Initial publication state. Defaults to `draft`; a seller product is
    /// always created as `draft` regardless of this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProductStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Free-form key/value metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock: Option<i64>,
    /// Group to list the product under. A seller may only use a group they
    /// own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_template_id: Option<String>,
    /// Product builder template. A seller product defaults to the seeded
    /// `default` template when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_template_id: Option<String>,
    /// Id of a product the buyer must already own before checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfillment_kind: Option<FulfillmentKind>,
}

impl CreateProductRequest {
    /// The columns this request writes: only the fields that were sent.
    pub fn into_columns(self) -> HashMap<String, Value> {
        columns(&self)
    }
}

/// `PATCH /b/products/api/admin/products/{id}` and
/// `PATCH /b/products/api/products/{id}` request body. Every field is
/// optional and only the ones present are applied. Ownership, moderation and
/// provider columns are set by the server and cannot be supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateProductRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Web address of the product: lowercase letters and digits in runs
    /// joined by single hyphens, at most 160 characters. Empty means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = PRODUCT_SLUG_MAX_LEN), regex(pattern = PRODUCT_SLUG_PATTERN))]
    pub slug: Option<String>,
    /// ISO 4217 currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Publication state. A seller may set `draft`, `active` or `archived`;
    /// `active` on a moderated product submits it for review instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProductStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Free-form key/value metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_template_id: Option<String>,
    /// Product builder template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_template_id: Option<String>,
    /// Id of a product the buyer must already own before checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfillment_kind: Option<FulfillmentKind>,
}

impl UpdateProductRequest {
    /// The columns this request writes: only the fields that were sent.
    pub fn into_columns(self) -> HashMap<String, Value> {
        columns(&self)
    }
}

// Not declared through `.output::<T>()`: `ManagedOffer` reaches the recursive
// `Condition`, so the endpoint schema for this response stays hand-written in
// `mod.rs` (`product_duplicate_schema`), with the `product` half derived from
// `ProductView`. The handler still builds this type, so the wire shape has one
// source.
/// Response body of the product duplication endpoints: the new draft product
/// and its copied, editable offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductDuplicateResponse {
    pub product: ProductView,
    pub offers: Vec<ManagedOffer>,
}

/// A product group row: every column of `impresspress__products__groups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupView {
    /// Stable group identifier.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Group template the group was created from.
    pub group_template_id: String,
    /// Id of the user who owns the group. The owner tier lists and edits
    /// only groups whose `user_id` is the caller.
    pub user_id: String,
    /// `active` unless the group has been retired.
    pub status: String,
    /// Id of the user who created the row: the administrator on the admin
    /// tier (the owner is `user_id`), the owner on the owner tier.
    pub created_by: String,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl GroupView {
    /// Project an `impresspress__products__groups` row.
    pub fn from_record(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            name: record.str_field("name").to_string(),
            description: record.str_field("description").to_string(),
            group_template_id: record.str_field("group_template_id").to_string(),
            user_id: record.str_field("user_id").to_string(),
            status: record.str_field("status").to_string(),
            created_by: record.str_field("created_by").to_string(),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        }
    }
}

/// One page of group rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupListResponse {
    /// Groups on this page.
    pub records: Vec<GroupView>,
    /// Total groups matching the filters, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl GroupListResponse {
    /// Project a `RecordList` of group rows.
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list.records.iter().map(GroupView::from_record).collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// `POST /b/products/api/admin/groups` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateGroupRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_template_id: Option<String>,
    /// Owner of the group. Defaults to the administrator creating it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl CreateGroupRequest {
    /// The columns this request writes: only the fields that were sent.
    pub fn into_columns(self) -> HashMap<String, Value> {
        columns(&self)
    }
}

/// `PATCH /b/products/api/admin/groups/{id}` request body. Every field is
/// optional and only the ones present are applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateGroupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_template_id: Option<String>,
    /// Owner of the group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl UpdateGroupRequest {
    /// The columns this request writes: only the fields that were sent.
    pub fn into_columns(self) -> HashMap<String, Value> {
        columns(&self)
    }
}

/// `POST /b/products/groups` request body. The owner is always the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateOwnGroupRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Group template. Defaults to the seeded `default` template when
    /// omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl CreateOwnGroupRequest {
    /// The columns this request writes: only the fields that were sent.
    pub fn into_columns(self) -> HashMap<String, Value> {
        columns(&self)
    }
}

/// `PATCH /b/products/groups/{id}` request body. Every field is optional
/// and only the ones present are applied; the owner cannot be changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateOwnGroupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl UpdateOwnGroupRequest {
    /// The columns this request writes: only the fields that were sent.
    pub fn into_columns(self) -> HashMap<String, Value> {
        columns(&self)
    }
}

/// A product type (taxonomy) row: every column of
/// `impresspress__products__types`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProductTypeView {
    /// Stable type identifier.
    pub id: String,
    pub name: String,
    pub description: String,
    /// Whether the type is built in. System types are seeded by the block
    /// rather than created through the API.
    pub is_system: bool,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl ProductTypeView {
    /// Project an `impresspress__products__types` row.
    pub fn from_record(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            name: record.str_field("name").to_string(),
            description: record.str_field("description").to_string(),
            is_system: record.bool_field("is_system"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        }
    }
}

/// One page of product type rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProductTypeListResponse {
    /// Types on this page, newest first.
    pub records: Vec<ProductTypeView>,
    /// Total types, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl ProductTypeListResponse {
    /// Project a `RecordList` of type rows.
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .map(ProductTypeView::from_record)
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// `POST /b/products/api/admin/types` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateProductTypeRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the type is built in. Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_system: Option<bool>,
}

impl CreateProductTypeRequest {
    /// The columns this request writes: only the fields that were sent.
    ///
    /// `is_system` is an `INTEGER` column on every backend (the Postgres
    /// migration mirrors SQLite's `INTEGER NOT NULL DEFAULT 0` deliberately),
    /// so the boolean is written as `0` / `1` rather than bound as a boolean
    /// Postgres would refuse.
    pub fn into_columns(self) -> HashMap<String, Value> {
        let is_system = self.is_system;
        let mut data = columns(&self);
        if let Some(is_system) = is_system {
            data.insert(
                "is_system".to_string(),
                Value::from(if is_system { 1 } else { 0 }),
            );
        }
        data
    }
}

/// A group template row: every column of
/// `impresspress__products__group_templates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupTemplateView {
    /// Stable template identifier.
    pub id: String,
    /// Machine name (`default`, …).
    pub name: String,
    /// Human-readable name shown in the product builder.
    pub display_name: String,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl GroupTemplateView {
    /// Project an `impresspress__products__group_templates` row.
    pub fn from_record(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            name: record.str_field("name").to_string(),
            display_name: record.str_field("display_name").to_string(),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        }
    }
}

/// Every group template, as one page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GroupTemplateListResponse {
    /// Templates, sorted by name.
    pub records: Vec<GroupTemplateView>,
    /// Total templates. Always equal to the number of records: the endpoint
    /// does not paginate.
    pub total_count: i64,
    /// Always `1`.
    pub page: i64,
    /// The fixed ceiling on rows returned.
    pub page_size: i64,
}

impl GroupTemplateListResponse {
    /// Project a `RecordList` of template rows.
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .map(GroupTemplateView::from_record)
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

// Two columns of `impresspress__products__purchases` are deliberately NOT
// published, on any tier (buyer, seller, admin). The reasons live in this
// plain comment rather than the doc comment: a `///` line is published as the
// schema's `description`.
//
// * `receipt_token_hash` — the sha256 of the guest receipt capability issued
//   at checkout (`CheckoutResponse::receipt_token`). Whoever holds the raw
//   token can read the order's status without a session; the digest is what
//   the server compares it against. Credential material of the same class as
//   the admin block's withheld `verification_token`; it has no use on any
//   order page, and the untyped handler only ever emitted it because it
//   echoed the whole row.
// * `receipt_token_expires_at` — the capability's expiry, meaningful only
//   beside the digest.
/// An order row: `impresspress__products__purchases`, as published to the
/// buyer, the seller and administrators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PurchaseView {
    /// Stable order identifier.
    pub id: String,
    // The row also carries a `user_id` column holding the same value — one
    // writer sets both — and this view used to publish it too, so a drifted
    // row put two answers to one question on the wire. `buyer_user_id` is
    // the one per-row ownership is decided on, so it is the one published.
    // The column stays; it is simply not a field.
    /// Signed-in buyer's user id, or empty for a guest order. The order's
    /// single buyer identity.
    pub buyer_user_id: String,
    /// Buyer's email address as captured at checkout, or empty.
    pub buyer_email: String,
    /// Seller account the order was placed against; empty for platform
    /// products.
    pub seller_account_id: String,
    /// Stripe connected account the order was charged through; empty for
    /// platform products.
    pub stripe_account_id: String,
    /// Stripe Customer id, or empty.
    pub stripe_customer_id: String,
    /// Stripe Subscription id for subscription orders, or empty.
    pub stripe_subscription_id: String,
    /// Lifecycle state of the order. `pending`: created, checkout not yet
    /// claimed; `checkout_started`: a provider Checkout Session was claimed;
    /// `completed`: paid; `partially_refunded` / `refunded`: paid, then
    /// refunded in part or in full; `failed`: checkout or reconciliation
    /// failed (`reconciliation_error` says why).
    pub status: OrderStatus,
    /// Checkout presentation the order was started with.
    #[schemars(extend("enum" = ["hosted", "embedded", "payment_link"]))]
    pub checkout_mode: String,
    /// Payment provider: `stripe`, or `manual` for orders recorded outside a
    /// provider.
    pub provider: String,
    /// Whether the order was placed against the live Stripe environment.
    pub livemode: bool,
    /// ISO 4217 currency of every amount on the order.
    pub currency: String,
    pub subtotal_cents: i64,
    pub discount_cents: i64,
    pub tax_cents: i64,
    pub shipping_cents: i64,
    pub platform_fee_cents: i64,
    // The row also carries an `amount_cents` column holding the same value —
    // one writer sets both — and this view used to publish it too, under a
    // field whose own description said to prefer this one. The column stays;
    // it is simply not a field.
    /// Final charged amount in minor units. The order's single amount.
    pub total_cents: i64,
    /// Sum of succeeded refunds in minor units.
    pub refunded_total_cents: i64,
    /// Immutable checkout snapshot: the offer id and version the order was
    /// priced against and the shipping amounts allowed at checkout.
    pub metadata: serde_json::Map<String, Value>,
    /// Stripe PaymentIntent id, or empty.
    pub stripe_payment_intent_id: String,
    /// PaymentIntent id as recorded from the provider event stream, or empty.
    pub provider_payment_intent_id: String,
    /// Stripe Checkout Session id, or empty.
    pub provider_session_id: String,
    /// Latest PaymentIntent state received from the provider.
    pub provider_payment_status: ProviderPaymentStatus,
    pub provider_payment_error_code: String,
    pub provider_payment_error_message: String,
    /// Provider timestamp (Unix seconds) of the PaymentIntent event that
    /// last updated the payment state; `0` until one arrives.
    pub payment_intent_event_created: i64,
    /// Where the order stands against the provider's view of it. `pending`:
    /// no provider session yet; `awaiting_payment`: a Checkout Session exists;
    /// `reconciled`: the completed session matched the local snapshot;
    /// `provider_error`: the provider's answer was unusable or contradicted
    /// it (`reconciliation_error` says why); the `payment_*` values mirror
    /// the last PaymentIntent event received before Checkout completion.
    pub reconciliation_status: ReconciliationStatus,
    pub reconciliation_error: String,
    /// Stripe subscription lifecycle state for subscription orders, or
    /// empty.
    pub subscription_status: SubscriptionStatus,
    /// RFC 3339 end of the current billing period, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub subscription_current_period_end: Option<String>,
    pub subscription_cancel_at_period_end: bool,
    /// RFC 3339 timestamp the subscription was canceled, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub subscription_canceled_at: Option<String>,
    /// RFC 3339 timestamp the subscription state was last synchronized from
    /// the provider, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub subscription_last_synced_at: Option<String>,
    /// Provider timestamp (Unix seconds) of the subscription event that last
    /// updated the subscription state; `0` until one arrives.
    pub subscription_event_created: i64,
    /// RFC 3339 timestamp the order was approved, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub approved_at: Option<String>,
    /// RFC 3339 timestamp payment was recorded, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub payment_at: Option<String>,
    /// RFC 3339 timestamp of the last refund applied, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub refunded_at: Option<String>,
    /// User id of the operator who issued the last refund, or empty.
    pub refunded_by: String,
    /// Operator note attached to the last refund, or empty.
    pub refund_reason: String,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

/// One order as its **buyer** may read it.
///
/// The narrowest of the three order projections, and the one that matters
/// most: `GET /b/products/purchases` is opted in as the `list_my_purchases`
/// WebMCP tool, so every field here is handed to whatever agent runs in the
/// buyer's page.
///
/// Withheld, deliberately: the platform's economics (`platform_fee_cents`),
/// the seller's identity and Stripe account, the buyer's own provider handles
/// (`stripe_customer_id`, the PaymentIntent and Checkout Session ids — a
/// buyer never needs to quote one, and they are the provider's namespace, not
/// ours), and the reconciliation and payment-error diagnostics, which describe
/// our integration rather than their purchase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuyerOrderView {
    /// Stable order identifier.
    pub id: String,
    /// Email captured at checkout, or empty.
    pub buyer_email: String,
    /// Lifecycle state of the order.
    pub status: OrderStatus,
    /// Checkout presentation the order was started with.
    pub checkout_mode: String,
    /// Payment provider: `stripe`, or `manual` for orders recorded outside a
    /// provider.
    pub provider: String,
    /// ISO 4217 currency of every amount on the order.
    pub currency: String,
    pub subtotal_cents: i64,
    pub discount_cents: i64,
    pub tax_cents: i64,
    pub shipping_cents: i64,
    /// Final charged amount in minor units.
    pub total_cents: i64,
    /// Sum of succeeded refunds in minor units.
    pub refunded_total_cents: i64,
    /// Immutable checkout snapshot: the offer id and version the order was
    /// priced against and the shipping amounts allowed at checkout.
    pub metadata: serde_json::Map<String, Value>,
    /// Latest payment state received from the provider.
    pub provider_payment_status: ProviderPaymentStatus,
    /// Where the order stands against the provider's view of it.
    pub reconciliation_status: ReconciliationStatus,
    /// Stripe subscription lifecycle state for subscription orders, or empty.
    pub subscription_status: SubscriptionStatus,
    /// RFC 3339 end of the current billing period, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub subscription_current_period_end: Option<String>,
    pub subscription_cancel_at_period_end: bool,
    /// RFC 3339 timestamp the subscription was canceled, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub subscription_canceled_at: Option<String>,
    /// RFC 3339 timestamp payment was recorded, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub payment_at: Option<String>,
    /// RFC 3339 timestamp of the last refund applied, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub refunded_at: Option<String>,
    /// Reason given for the last refund, or empty.
    pub refund_reason: String,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl BuyerOrderView {
    /// Project an `impresspress__products__purchases` row for its buyer.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            buyer_email: record.str_field("buyer_email").to_string(),
            status: OrderStatus::from_record(record)?,
            checkout_mode: record.str_field("checkout_mode").to_string(),
            provider: record.str_field("provider").to_string(),
            currency: record.str_field("currency").to_string(),
            subtotal_cents: record.i64_field("subtotal_cents"),
            discount_cents: record.i64_field("discount_cents"),
            tax_cents: record.i64_field("tax_cents"),
            shipping_cents: record.i64_field("shipping_cents"),
            total_cents: record.i64_field("total_cents"),
            refunded_total_cents: record.i64_field("refunded_total_cents"),
            metadata: record.json_object_field("metadata"),
            provider_payment_status: enum_column(record, "provider_payment_status")?,
            reconciliation_status: ReconciliationStatus::from_record(record)?,
            subscription_status: SubscriptionStatus::from_record(record)?,
            subscription_current_period_end: timestamp_field(
                record,
                "subscription_current_period_end",
            ),
            subscription_cancel_at_period_end: record
                .bool_field("subscription_cancel_at_period_end"),
            subscription_canceled_at: timestamp_field(record, "subscription_canceled_at"),
            payment_at: timestamp_field(record, "payment_at"),
            refunded_at: timestamp_field(record, "refunded_at"),
            refund_reason: record.str_field("refund_reason").to_string(),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

/// One refund as its **buyer** may read it: how much came back, in what
/// currency, and whether it has landed.
///
/// The provider handles (`provider_refund_id`, `payment_intent_id`,
/// `stripe_account_id`), the operator fields (`refunded_by`, `note`,
/// `provider_reason`) and the failure diagnostics (`last_error`) belong to
/// whoever issued the refund, not to whoever received it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuyerRefundView {
    /// Stable refund identifier.
    pub id: String,
    pub purchase_id: String,
    pub amount_minor: i64,
    pub currency: String,
    /// Ledger state.
    pub status: RefundStatus,
    /// The provider's own state for the refund. Empty until the provider
    /// answers; `succeeded` for a refund recorded without a provider. Kept
    /// for the buyer because "has my money actually gone back" is the
    /// question this endpoint exists to answer — it is a state, not a handle.
    pub provider_status: String,
    /// RFC 3339 timestamp the refund reached a terminal state, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub completed_at: Option<String>,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
}

impl BuyerRefundView {
    /// Project an `impresspress__products__refunds` row for the buyer.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            purchase_id: record.str_field("purchase_id").to_string(),
            amount_minor: record.i64_field("amount_minor"),
            currency: record.str_field("currency").to_string(),
            status: enum_column(record, "status")?,
            provider_status: record.str_field("provider_status").to_string(),
            completed_at: timestamp_field(record, "completed_at"),
            created_at: record.str_field("created_at").to_string(),
        })
    }
}

/// One page of the caller's own orders, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuyerOrderListResponse {
    pub records: Vec<BuyerOrderView>,
    /// Total orders matching the query, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl BuyerOrderListResponse {
    /// Project a `RecordList` of the caller's order rows. A row outside the
    /// contract costs that row, not the page — see
    /// [`PurchaseListResponse::from_record_list`].
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .filter_map(|record| match BuyerOrderView::from_record(record) {
                    Ok(view) => Some(view),
                    Err(e) => {
                        tracing::error!(
                            order_id = %record.id,
                            error = %e,
                            "order row is outside the published contract and was omitted from the page"
                        );
                        None
                    }
                })
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// One of the caller's own orders with the lines it was made of and the
/// refunds against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BuyerOrderDetailResponse {
    pub purchase: BuyerOrderView,
    pub line_items: Vec<LineItemView>,
    pub refunds: Vec<BuyerRefundView>,
    /// Disputes raised against this order. Kept because the SSR order page
    /// already shows a buyer their own disputes, and this endpoint is not
    /// opted into the WebMCP manifest — only the list above is, and it
    /// carries no nested rows.
    pub disputes: Vec<DisputeView>,
}

/// One order as the **seller** who fulfils it may read it.
///
/// Wider than the buyer's: a seller needs the fee that was taken, their own
/// connected account, the provider handles for their own charge, and the
/// buyer's email in order to fulfil. It still withholds the buyer's platform
/// identity (`user_id` / `buyer_user_id`) and their Stripe customer id, none
/// of which a seller needs to ship an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SellerOrderView {
    /// Stable order identifier.
    pub id: String,
    /// Buyer's email address as captured at checkout, or empty.
    pub buyer_email: String,
    /// Seller account the order was placed against.
    pub seller_account_id: String,
    /// Stripe connected account the order was charged through, or empty.
    pub stripe_account_id: String,
    /// Lifecycle state of the order.
    pub status: OrderStatus,
    /// Checkout presentation the order was started with.
    pub checkout_mode: String,
    /// Payment provider: `stripe`, or `manual`.
    pub provider: String,
    /// Whether the order was placed against the live Stripe environment.
    pub livemode: bool,
    /// ISO 4217 currency of every amount on the order.
    pub currency: String,
    pub subtotal_cents: i64,
    pub discount_cents: i64,
    pub tax_cents: i64,
    pub shipping_cents: i64,
    /// Platform fee taken from this order, in minor units.
    pub platform_fee_cents: i64,
    /// Final charged amount in minor units.
    pub total_cents: i64,
    /// Sum of succeeded refunds in minor units.
    pub refunded_total_cents: i64,
    /// Immutable checkout snapshot.
    pub metadata: serde_json::Map<String, Value>,
    /// Stripe PaymentIntent id, or empty.
    pub stripe_payment_intent_id: String,
    /// Stripe Checkout Session id, or empty.
    pub provider_session_id: String,
    /// Latest PaymentIntent state received from the provider.
    pub provider_payment_status: ProviderPaymentStatus,
    pub provider_payment_error_code: String,
    pub provider_payment_error_message: String,
    /// Where the order stands against the provider's view of it.
    pub reconciliation_status: ReconciliationStatus,
    pub reconciliation_error: String,
    /// Stripe subscription lifecycle state, or empty.
    pub subscription_status: SubscriptionStatus,
    /// RFC 3339 end of the current billing period, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub subscription_current_period_end: Option<String>,
    pub subscription_cancel_at_period_end: bool,
    /// RFC 3339 timestamp payment was recorded, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub payment_at: Option<String>,
    /// RFC 3339 timestamp of the last refund applied, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub refunded_at: Option<String>,
    /// User id of the operator who issued the last refund, or empty.
    pub refunded_by: String,
    /// Operator note attached to the last refund, or empty.
    pub refund_reason: String,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl SellerOrderView {
    /// Project an `impresspress__products__purchases` row for its seller.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            buyer_email: record.str_field("buyer_email").to_string(),
            seller_account_id: record.str_field("seller_account_id").to_string(),
            stripe_account_id: record.str_field("stripe_account_id").to_string(),
            status: OrderStatus::from_record(record)?,
            checkout_mode: record.str_field("checkout_mode").to_string(),
            provider: record.str_field("provider").to_string(),
            livemode: record.bool_field("livemode"),
            currency: record.str_field("currency").to_string(),
            subtotal_cents: record.i64_field("subtotal_cents"),
            discount_cents: record.i64_field("discount_cents"),
            tax_cents: record.i64_field("tax_cents"),
            shipping_cents: record.i64_field("shipping_cents"),
            platform_fee_cents: record.i64_field("platform_fee_cents"),
            total_cents: record.i64_field("total_cents"),
            refunded_total_cents: record.i64_field("refunded_total_cents"),
            metadata: record.json_object_field("metadata"),
            stripe_payment_intent_id: record.str_field("stripe_payment_intent_id").to_string(),
            provider_session_id: record.str_field("provider_session_id").to_string(),
            provider_payment_status: enum_column(record, "provider_payment_status")?,
            provider_payment_error_code: record
                .str_field("provider_payment_error_code")
                .to_string(),
            provider_payment_error_message: record
                .str_field("provider_payment_error_message")
                .to_string(),
            reconciliation_status: ReconciliationStatus::from_record(record)?,
            reconciliation_error: record.str_field("reconciliation_error").to_string(),
            subscription_status: SubscriptionStatus::from_record(record)?,
            subscription_current_period_end: timestamp_field(
                record,
                "subscription_current_period_end",
            ),
            subscription_cancel_at_period_end: record
                .bool_field("subscription_cancel_at_period_end"),
            payment_at: timestamp_field(record, "payment_at"),
            refunded_at: timestamp_field(record, "refunded_at"),
            refunded_by: record.str_field("refunded_by").to_string(),
            refund_reason: record.str_field("refund_reason").to_string(),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

/// One page of the seller's own orders, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SellerOrderListResponse {
    pub records: Vec<SellerOrderView>,
    /// Total orders matching the query, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl SellerOrderListResponse {
    /// Project a `RecordList` of the seller's order rows. Same row-not-page
    /// degradation as [`PurchaseListResponse::from_record_list`].
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .filter_map(|record| match SellerOrderView::from_record(record) {
                    Ok(view) => Some(view),
                    Err(e) => {
                        tracing::error!(
                            order_id = %record.id,
                            error = %e,
                            "order row is outside the published contract and was omitted from the page"
                        );
                        None
                    }
                })
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// One seller-owned order with its lines, refunds and disputes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SellerOrderDetailResponse {
    pub purchase: SellerOrderView,
    pub line_items: Vec<LineItemView>,
    pub refunds: Vec<RefundView>,
    pub disputes: Vec<DisputeView>,
}

impl PurchaseView {
    /// Project an `impresspress__products__purchases` row. Fails when a state
    /// column holds a value outside its contract.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            buyer_user_id: record.str_field("buyer_user_id").to_string(),
            buyer_email: record.str_field("buyer_email").to_string(),
            seller_account_id: record.str_field("seller_account_id").to_string(),
            stripe_account_id: record.str_field("stripe_account_id").to_string(),
            stripe_customer_id: record.str_field("stripe_customer_id").to_string(),
            stripe_subscription_id: record.str_field("stripe_subscription_id").to_string(),
            status: OrderStatus::from_record(record)?,
            checkout_mode: record.str_field("checkout_mode").to_string(),
            provider: record.str_field("provider").to_string(),
            livemode: record.bool_field("livemode"),
            currency: record.str_field("currency").to_string(),
            subtotal_cents: record.i64_field("subtotal_cents"),
            discount_cents: record.i64_field("discount_cents"),
            tax_cents: record.i64_field("tax_cents"),
            shipping_cents: record.i64_field("shipping_cents"),
            platform_fee_cents: record.i64_field("platform_fee_cents"),
            total_cents: record.i64_field("total_cents"),
            refunded_total_cents: record.i64_field("refunded_total_cents"),
            metadata: record.json_object_field("metadata"),
            stripe_payment_intent_id: record.str_field("stripe_payment_intent_id").to_string(),
            provider_payment_intent_id: record.str_field("provider_payment_intent_id").to_string(),
            provider_session_id: record.str_field("provider_session_id").to_string(),
            provider_payment_status: enum_column(record, "provider_payment_status")?,
            provider_payment_error_code: record
                .str_field("provider_payment_error_code")
                .to_string(),
            provider_payment_error_message: record
                .str_field("provider_payment_error_message")
                .to_string(),
            payment_intent_event_created: record.i64_field("payment_intent_event_created"),
            reconciliation_status: ReconciliationStatus::from_record(record)?,
            reconciliation_error: record.str_field("reconciliation_error").to_string(),
            subscription_status: SubscriptionStatus::from_record(record)?,
            subscription_current_period_end: timestamp_field(
                record,
                "subscription_current_period_end",
            ),
            subscription_cancel_at_period_end: record
                .bool_field("subscription_cancel_at_period_end"),
            subscription_canceled_at: timestamp_field(record, "subscription_canceled_at"),
            subscription_last_synced_at: timestamp_field(record, "subscription_last_synced_at"),
            subscription_event_created: record.i64_field("subscription_event_created"),
            approved_at: timestamp_field(record, "approved_at"),
            payment_at: timestamp_field(record, "payment_at"),
            refunded_at: timestamp_field(record, "refunded_at"),
            refunded_by: record.str_field("refunded_by").to_string(),
            refund_reason: record.str_field("refund_reason").to_string(),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

/// One page of order rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PurchaseListResponse {
    /// Orders on this page, newest first.
    pub records: Vec<PurchaseView>,
    /// Total orders matching the filters, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl PurchaseListResponse {
    /// Project a `RecordList` of order rows.
    ///
    /// A row holding a state column outside its contract is a data-integrity
    /// problem, and it costs that row rather than the page: the alternative —
    /// collecting into a `Result` — meant one legacy, imported or hand-edited
    /// order denied the caller every order they had. It is logged at ERROR
    /// with the row id so the operator sees what the caller cannot. The
    /// single-order paths keep failing loudly, because there the row *is* the
    /// response.
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .filter_map(|record| match PurchaseView::from_record(record) {
                    Ok(view) => Some(view),
                    Err(e) => {
                        tracing::error!(
                            order_id = %record.id,
                            error = %e,
                            "order row is outside the published contract and was omitted from the page"
                        );
                        None
                    }
                })
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// Query parameters accepted by `GET /b/products/api/admin/purchases`.
///
/// Built by [`Self::from_message`], which is the handler's only source for
/// these values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
pub struct AdminPurchaseListQuery {
    /// 1-based page number. Values below 1 clamp to 1.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Rows per page, capped at 100.
    #[serde(default = "default_page_size")]
    #[schemars(range(max = 100))]
    pub page_size: u32,
    /// Exact-match filter on the order state.
    pub status: Option<String>,
    /// Exact-match filter on the legacy owner column `user_id`.
    pub user_id: Option<String>,
}

impl AdminPurchaseListQuery {
    /// Resolve the query string on `msg`.
    pub fn from_message(msg: &Message) -> Self {
        let (page, page_size, _) = msg.pagination_params(DEFAULT_PAGE_SIZE as usize);
        Self {
            page: page as u32,
            page_size: page_size as u32,
            status: non_empty(msg.query("status")),
            user_id: non_empty(msg.query("user_id")),
        }
    }
}

/// Query parameters accepted by `GET /b/products/api/seller/orders`.
///
/// Built by [`Self::from_message`], which is the handler's only source for
/// these values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
pub struct SellerOrderListQuery {
    /// 1-based page number. Values below 1 clamp to 1.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Rows per page, capped at 100.
    #[serde(default = "default_page_size")]
    #[schemars(range(max = 100))]
    pub page_size: u32,
    /// Exact-match filter on the order state. `all` (or omitting the
    /// parameter) returns every state.
    pub status: Option<String>,
}

impl SellerOrderListQuery {
    /// Resolve the query string on `msg`. `status=all` reads as no filter.
    pub fn from_message(msg: &Message) -> Self {
        let (page, page_size, _) = msg.pagination_params(DEFAULT_PAGE_SIZE as usize);
        Self {
            page: page as u32,
            page_size: page_size as u32,
            status: non_empty(msg.query("status")).filter(|status| status != "all"),
        }
    }
}

/// One line of an order: `impresspress__products__line_items`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LineItemView {
    /// Stable line identifier.
    pub id: String,
    pub purchase_id: String,
    pub product_id: String,
    /// Product name as it was at checkout.
    pub product_name: String,
    pub quantity: i64,
    /// Offer the line was priced from, or empty for legacy lines.
    pub offer_id: String,
    /// Version of that offer at checkout.
    pub offer_version: i64,
    /// Offer component the line resolved, or empty for a whole-offer line.
    pub component_id: String,
    pub seller_account_id: String,
    /// Stripe Price id the line was charged through, or empty.
    pub stripe_price_id: String,
    pub unit_amount_minor: i64,
    pub subtotal_minor: i64,
    pub discount_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
    /// Customer inputs the line was priced with, as submitted at checkout.
    pub input_snapshot: serde_json::Map<String, Value>,
    /// The component condition as it was evaluated at checkout.
    pub condition_snapshot: serde_json::Map<String, Value>,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl LineItemView {
    /// Project an `impresspress__products__line_items` row.
    pub fn from_record(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            purchase_id: record.str_field("purchase_id").to_string(),
            product_id: record.str_field("product_id").to_string(),
            product_name: record.str_field("product_name").to_string(),
            quantity: record.i64_field("quantity"),
            offer_id: record.str_field("offer_id").to_string(),
            offer_version: record.i64_field("offer_version"),
            component_id: record.str_field("component_id").to_string(),
            seller_account_id: record.str_field("seller_account_id").to_string(),
            stripe_price_id: record.str_field("stripe_price_id").to_string(),
            unit_amount_minor: record.i64_field("unit_amount_minor"),
            subtotal_minor: record.i64_field("subtotal_minor"),
            discount_minor: record.i64_field("discount_minor"),
            tax_minor: record.i64_field("tax_minor"),
            total_minor: record.i64_field("total_minor"),
            input_snapshot: record.json_object_field("input_snapshot"),
            condition_snapshot: record.json_object_field("condition_snapshot"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        }
    }
}

// Two columns of `impresspress__products__refunds` are NOT published, under
// the rule `ProviderOperationSummary` already states for the block's
// provider operations ("request/response payloads, idempotency keys ... stay
// private"): `idempotency_key`, the Stripe idempotency key the refund was
// claimed under, and `response_json`. The latter is NOT the raw provider
// response, as this comment used to say: every writer passes a summary this
// block builds itself (`stripe.rs` assembles `{id, status, amount_minor,
// livemode, source}`; the reconciliation path in `purchase.rs` forwards that
// same stored text). Still withheld — a provider's own view of a refund is
// not part of the order contract — but withheld as bookkeeping, not as a
// payload that might carry anything of Stripe's.
/// One refund on an order: `impresspress__products__refunds`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefundView {
    /// Stable refund identifier.
    pub id: String,
    pub purchase_id: String,
    /// Stripe Refund id once the provider accepted it, or empty.
    pub provider_refund_id: String,
    /// PaymentIntent the refund was issued against; empty for manual
    /// refunds.
    pub payment_intent_id: String,
    /// Stripe connected account the refund was issued through, or empty.
    pub stripe_account_id: String,
    pub amount_minor: i64,
    /// The order's `refunded_total_cents` once this refund succeeds.
    pub target_refunded_total_minor: i64,
    pub currency: String,
    /// Ledger state.
    pub status: RefundStatus,
    /// The provider's own state for the refund (`pending`, `requires_action`,
    /// `succeeded`, `failed` or `canceled`). Empty until the provider answers;
    /// `succeeded` for a refund recorded without a provider, which the ledger
    /// completes itself.
    pub provider_status: String,
    /// Reason sent to the provider, or empty.
    pub provider_reason: String,
    /// Operator note kept on the platform, never sent to the provider.
    pub note: String,
    /// User id of the operator who issued the refund.
    pub refunded_by: String,
    pub livemode: bool,
    pub last_error: String,
    /// RFC 3339 timestamp the refund reached a terminal state, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub completed_at: Option<String>,
    /// Provider timestamp (Unix seconds) of the event that last updated the
    /// refund; `0` until one arrives.
    pub stripe_event_created: i64,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl RefundView {
    /// Project an `impresspress__products__refunds` row.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            purchase_id: record.str_field("purchase_id").to_string(),
            provider_refund_id: record.str_field("provider_refund_id").to_string(),
            payment_intent_id: record.str_field("payment_intent_id").to_string(),
            stripe_account_id: record.str_field("stripe_account_id").to_string(),
            amount_minor: record.i64_field("amount_minor"),
            target_refunded_total_minor: record.i64_field("target_refunded_total_minor"),
            currency: record.str_field("currency").to_string(),
            status: enum_column(record, "status")?,
            provider_status: record.str_field("provider_status").to_string(),
            provider_reason: record.str_field("provider_reason").to_string(),
            note: record.str_field("note").to_string(),
            refunded_by: record.str_field("refunded_by").to_string(),
            livemode: record.bool_field("livemode"),
            last_error: record.str_field("last_error").to_string(),
            completed_at: timestamp_field(record, "completed_at"),
            stripe_event_created: record.i64_field("stripe_event_created"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

/// Where a dispute stands with the card network: the `status` column of
/// `impresspress__products__disputes`.
///
/// Stripe's eight dispute states. They were a `matches!` list in
/// `repo::disputes::supported_status`, a second `matches!` in `is_closed`
/// and a third spelling in this module's schema; the three could drift and
/// nothing would notice, because the validator and the schema were read by
/// different people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DisputeStatus {
    WarningNeedsResponse,
    WarningUnderReview,
    WarningClosed,
    NeedsResponse,
    UnderReview,
    Won,
    Lost,
    Prevented,
}

impl DisputeStatus {
    /// Whether the dispute has reached a state the network will not move
    /// from. `repo::disputes` stamps `closed_at` on exactly these.
    pub const fn is_closed(self) -> bool {
        matches!(
            self,
            Self::WarningClosed | Self::Won | Self::Lost | Self::Prevented
        )
    }
}

/// One payment dispute on an order: `impresspress__products__disputes`, the
/// durable projection of the provider's dispute events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisputeView {
    /// Stable dispute identifier.
    pub id: String,
    pub purchase_id: String,
    pub seller_account_id: String,
    pub stripe_account_id: String,
    /// Stripe Dispute id.
    pub provider_dispute_id: String,
    /// Stripe Charge id the dispute was raised against, or empty.
    pub provider_charge_id: String,
    pub payment_intent_id: String,
    // A doc comment where there was none, deliberately: schemars inlines the
    // *type's* doc as the published `description` when the field has none,
    // and this module's enum docs carry the reasoning that produced them,
    // which is not what belongs in a published schema.
    /// Where the dispute stands with the card network.
    pub status: DisputeStatus,
    pub amount_minor: i64,
    pub currency: String,
    /// Provider's dispute reason, or empty.
    pub reason: String,
    /// RFC 3339 evidence deadline, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub evidence_due_by: Option<String>,
    pub livemode: bool,
    /// Provider timestamp (Unix seconds) of the event that last updated the
    /// dispute.
    pub event_created: i64,
    /// RFC 3339 timestamp the dispute closed, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub closed_at: Option<String>,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl DisputeView {
    /// Project an `impresspress__products__disputes` row.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            purchase_id: record.str_field("purchase_id").to_string(),
            seller_account_id: record.str_field("seller_account_id").to_string(),
            stripe_account_id: record.str_field("stripe_account_id").to_string(),
            provider_dispute_id: record.str_field("provider_dispute_id").to_string(),
            provider_charge_id: record.str_field("provider_charge_id").to_string(),
            payment_intent_id: record.str_field("payment_intent_id").to_string(),
            status: enum_column(record, "status")?,
            amount_minor: record.i64_field("amount_minor"),
            currency: record.str_field("currency").to_string(),
            reason: record.str_field("reason").to_string(),
            evidence_due_by: timestamp_field(record, "evidence_due_by"),
            livemode: record.bool_field("livemode"),
            event_created: record.i64_field("event_created"),
            closed_at: timestamp_field(record, "closed_at"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

/// Response body of the order detail endpoints: the order with its lines,
/// refunds and disputes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PurchaseDetailResponse {
    pub purchase: PurchaseView,
    pub line_items: Vec<LineItemView>,
    /// Refunds, newest first.
    pub refunds: Vec<RefundView>,
    /// Disputes, newest first.
    pub disputes: Vec<DisputeView>,
}

// Two columns of `impresspress__products__subscriptions` are NOT published:
// `user_id` (the caller already is that user) and `stripe_customer_id`,
// the provider Customer id, which the hand-curated projection this type
// replaces had always kept out of the response.
/// The caller's platform subscription: `impresspress__products__subscriptions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubscriptionView {
    /// Stable subscription identifier.
    pub id: String,
    /// Plan name.
    pub plan: String,
    /// Stripe subscription lifecycle state.
    pub status: SubscriptionStatus,
    /// Stripe Subscription id, or empty.
    pub stripe_subscription_id: String,
    /// RFC 3339 end of the grace period after a failed payment, or `null`.
    #[schemars(extend("format" = "date-time"))]
    pub grace_period_end: Option<String>,
    /// Purchased add-on quantities; `0` when none.
    pub addon_projects: i64,
    pub addon_requests: i64,
    pub addon_r2_bytes: i64,
    pub addon_d1_bytes: i64,
    /// RFC 3339 creation timestamp.
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    #[schemars(extend("format" = "date-time"))]
    pub updated_at: String,
}

impl SubscriptionView {
    /// Project an `impresspress__products__subscriptions` row. A `status`
    /// outside [`SubscriptionStatus`] is an `Internal` fault naming the row.
    pub fn from_record(record: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: record.id.clone(),
            plan: record.str_field("plan").to_string(),
            status: enum_column(record, "status")?,
            stripe_subscription_id: record.str_field("stripe_subscription_id").to_string(),
            grace_period_end: timestamp_field(record, "grace_period_end"),
            addon_projects: record.i64_field("addon_projects"),
            addon_requests: record.i64_field("addon_requests"),
            addon_r2_bytes: record.i64_field("addon_r2_bytes"),
            addon_d1_bytes: record.i64_field("addon_d1_bytes"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        })
    }
}

/// Response body of `GET /b/products/subscription`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubscriptionStatusResponse {
    /// The caller's subscription, or `null` when they have none.
    pub subscription: Option<SubscriptionView>,
}

/// Response body of `GET /b/products/api/admin/sellers/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdminSellerDetail {
    pub seller: SellerAccount,
    /// The seller's live catalog — products the seller's user owns that have
    /// not been soft-deleted, in any publication state.
    pub products: Vec<ProductView>,
    /// Whether the seller owns more live products than `products` lists.
    ///
    /// Nothing caps a seller's catalog, so the listing is read up to a
    /// ceiling; this is how a client tells a complete catalog from a prefix
    /// of one.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wafer_core::clients::database::Record;
    use wafer_run::ErrorCode;

    use super::{Condition, OfferMode, OrderStatus, PricingPreviewRequest, ReconciliationStatus};

    /// One row whose state column is outside the contract must cost that
    /// row, not the page.
    ///
    /// `PurchaseView::from_record` is fallible, so collecting the page into a
    /// `Result` meant a single legacy, imported or hand-edited row took down
    /// the caller's whole order list — a buyer could see none of their orders
    /// because of one they could not see anyway.
    #[test]
    fn a_row_outside_the_contract_is_dropped_not_fatal_for_the_page() {
        fn order(id: &str, status: &str) -> Record {
            Record {
                id: id.to_string(),
                data: HashMap::from([
                    ("status".to_string(), serde_json::json!(status)),
                    (
                        "reconciliation_status".to_string(),
                        serde_json::json!("pending"),
                    ),
                ]),
            }
        }

        let list = wafer_core::clients::database::RecordList {
            records: vec![
                order("pur_ok", "completed"),
                order("pur_bad", "shipped"),
                order("pur_ok2", "pending"),
            ],
            total_count: 3,
            page: 1,
            page_size: 20,
        };

        let projected = super::PurchaseListResponse::from_record_list(&list);
        let ids: Vec<&str> = projected.records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["pur_ok", "pur_ok2"],
            "the conforming rows must still reach the caller"
        );
        assert_eq!(
            projected.total_count, 3,
            "total_count is the database's count and is not rewritten by the projection"
        );
    }

    /// Same contract as for `ReconciliationStatus`: the wire form is the
    /// stored column value, and the predicates the repo filters on are the
    /// documented groupings.
    #[test]
    fn order_status_wire_form_is_the_stored_column_value() {
        use OrderStatus::*;
        for (variant, stored, paid, refundable, awaiting) in [
            (Pending, "pending", false, false, true),
            (CheckoutStarted, "checkout_started", false, false, true),
            (Completed, "completed", true, true, false),
            (PartiallyRefunded, "partially_refunded", true, true, false),
            (Refunded, "refunded", true, false, false),
            (Failed, "failed", false, false, false),
        ] {
            assert_eq!(serde_json::to_value(variant).unwrap(), stored);
            let record = Record {
                id: "pur_1".to_string(),
                data: HashMap::from([("status".to_string(), serde_json::json!(stored))]),
            };
            assert_eq!(OrderStatus::from_record(&record).unwrap(), variant);
            assert_eq!(variant.is_paid(), paid, "{stored}");
            assert_eq!(variant.is_refundable(), refundable, "{stored}");
            assert_eq!(variant.awaits_completion(), awaiting, "{stored}");
        }
        let record = Record {
            id: "pur_1".to_string(),
            data: HashMap::from([("status".to_string(), serde_json::json!("shipped"))]),
        };
        let error = OrderStatus::from_record(&record).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(error.message.contains("shipped"), "{}", error.message);
    }

    #[test]
    fn enums_use_stable_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_string(&OfferMode::Subscription).unwrap(),
            "\"subscription\""
        );
    }

    /// The enum's wire form is the stored column value: the writers store
    /// `json!(variant)` and the views parse the column back, so a rename here
    /// would be a migration, not a refactor.
    #[test]
    fn reconciliation_status_wire_form_is_the_stored_column_value() {
        use ReconciliationStatus::*;
        for (variant, stored) in [
            (Pending, "pending"),
            (AwaitingPayment, "awaiting_payment"),
            (Reconciled, "reconciled"),
            (ProviderError, "provider_error"),
            (
                PaymentSucceededAwaitingCheckout,
                "payment_succeeded_awaiting_checkout",
            ),
            (PaymentFailed, "payment_failed"),
            (PaymentProcessing, "payment_processing"),
            (PaymentRequiresAction, "payment_requires_action"),
            (PaymentCanceled, "payment_canceled"),
        ] {
            assert_eq!(serde_json::to_value(variant).unwrap(), stored);
            let record = Record {
                id: "pur_1".to_string(),
                data: HashMap::from([(
                    "reconciliation_status".to_string(),
                    serde_json::json!(stored),
                )]),
            };
            assert_eq!(ReconciliationStatus::from_record(&record).unwrap(), variant);
        }
    }

    #[test]
    fn a_state_column_outside_the_contract_is_an_internal_error_naming_the_row() {
        let record = Record {
            id: "pur_1".to_string(),
            data: HashMap::from([(
                "reconciliation_status".to_string(),
                serde_json::json!("half_done"),
            )]),
        };
        let error = ReconciliationStatus::from_record(&record).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(
            error.message.contains("pur_1")
                && error.message.contains("reconciliation_status")
                && error.message.contains("half_done"),
            "{}",
            error.message
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

    /// The guest receipt omits `subscription_status` for a one-time order
    /// rather than sending `""`, so its published enum lists every status
    /// but that one: a client generated from the schema must not handle a
    /// value the server never sends.
    #[test]
    fn guest_subscription_status_schema_omits_the_unset_value() {
        use super::{GuestOrderStatus, SubscriptionStatus};

        // The schema the route publishes, subschemas inlined.
        let schema = crate::endpoint_match::response_schema_of::<GuestOrderStatus>();
        let published = schema
            .get("properties")
            .and_then(|properties| properties.get("subscription_status"))
            .and_then(|field| field.get("enum"))
            .expect("subscription_status publishes an enum");
        let sent: Vec<serde_json::Value> = SubscriptionStatus::ALL
            .iter()
            .filter(|status| !status.is_unset())
            .map(|status| serde_json::to_value(status).unwrap())
            .collect();
        assert_eq!(published, &serde_json::Value::Array(sent));
    }

    #[test]
    fn product_slug_grammar() {
        use super::{check_product_slug, PRODUCT_SLUG_MAX_LEN};

        for good in ["", "a", "a1-b2", "0", &"z".repeat(PRODUCT_SLUG_MAX_LEN)] {
            assert_eq!(check_product_slug(good), Ok(()), "{good:?}");
        }
        for bad in [
            "-",
            "a-",
            "-a",
            "a--b",
            "A",
            "a b",
            "a_b",
            "caf\u{e9}",
            &"z".repeat(PRODUCT_SLUG_MAX_LEN + 1),
        ] {
            assert!(check_product_slug(bad).is_err(), "{bad:?}");
        }
    }
}
