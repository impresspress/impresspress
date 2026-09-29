import { BaseService } from "./base.service";

/**
 * One registered block, as `GET /b/admin/api/extensions` lists them.
 * `blocks/admin/mod.rs::handle_extensions` projects exactly these five keys
 * off wafer-run's `BlockInfo`; there is no description, author, config blob
 * or metadata object anywhere in that response, and `enabled` is a literal
 * `true` for every row (a registered block is by definition enabled — there
 * is no server-side enable/disable lifecycle).
 */
export interface Extension {
  /** Block name in the canonical `{org}/{block}` form. */
  name: string;
  /** Semantic version of the block implementation. */
  version: string;
  /** Interface identifier, e.g. `"middleware@v1"`. */
  interface: string;
  /** One-line human-readable summary of what the block does. */
  summary: string;
  /** Always `true` — every listed block is registered, hence enabled. */
  enabled: boolean;
}

/**
 * `call`'s options, split by whether the method carries a body.
 *
 * `BaseService.request` used to switch on the method and silently drop `data`
 * for GET and DELETE. The shared client forwards it instead, and `fetch`
 * throws "Request with GET/HEAD method cannot have body", which surfaces as an
 * opaque `network_error`. Neither dropping the caller's payload nor failing at
 * the transport is right, so the combination is unrepresentable and the
 * compiler says so at the call site.
 */
type ExtensionCallParams = Record<string, any>;

export type ExtensionCallOptions =
  | { method?: "GET" | "DELETE"; data?: never; params?: ExtensionCallParams }
  | { method: "POST" | "PUT" | "PATCH"; data?: any; params?: ExtensionCallParams };

export class ExtensionsService extends BaseService {
  /** List all available extensions (registered blocks). `GET /b/admin/api/extensions`. */
  async list(): Promise<Extension[]> {
    return this.request<Extension[]>({
      method: "GET",
      url: "/b/admin/api/extensions",
    });
  }

  /**
   * Call an arbitrary block endpoint at `/b/{extension}/{endpoint}`. This is
   * a raw passthrough — there is no generic extension lifecycle API
   * (enable/disable/configure/health) server-side, only each block's own
   * declared HTTP routes, which this method reaches directly.
   */
  async call<T = any>(
    extension: string,
    endpoint: string,
    options?: ExtensionCallOptions,
  ): Promise<T> {
    return this.request<T>({
      method: options?.method || "GET",
      url: `/b/${extension}/${endpoint}`,
      params: options?.params,
      data: options?.data,
    });
  }
}

/**
 * One row of the `impresspress__files__cloud_shares` table (see
 * `crates/impresspress-core/src/blocks/files/repo/shares.rs`), flattened
 * from the wire `Record { id, data }` shape (`id` + the row's columns).
 */
export interface ShareRecord {
  id: string;
  token: string;
  bucket: string;
  key: string;
  created_by: string;
  created_at: string;
  access_count: number;
  /**
   * Absolute expiry, or `null` for a share that never expires.
   *
   * `null`, not absent. `ShareRow.expires_at` is an `Option<String>` with no
   * `skip_serializing_if`, so the key is always present and carries JSON
   * `null` when there is no expiry — this was declared `expires_at?: string`
   * until `files.openapi.json` started describing the row and said otherwise.
   */
  expires_at: string | null;
  /** Access cap, or `null` for unlimited. Always present — see `expires_at`. */
  max_access_count: number | null;
}

export interface ListSharesResult {
  items: ShareRecord[];
  total: number;
}

/**
 * Aligned to the real `impresspress/files` cloud-storage surface in
 * `crates/impresspress-core/src/blocks/files/cloud.rs`: per-object share
 * links and the caller's own quota/usage. There is no user-facing
 * access-log or access-stats endpoint (`GET /b/cloudstorage/admin/access-logs`
 * is declared `Admin` by the files block and is not part of this surface;
 * `access-stats` does not exist at all) — both were removed rather than
 * pointed at a route that would 404 or silently expose the wrong auth
 * boundary.
 */
export class CloudStorageExtension extends ExtensionsService {
  /** Create a share link for an object. `POST /b/cloudstorage/shares`. */
  async share(
    bucket: string,
    key: string,
    options?: { expiresInHours?: number; maxAccessCount?: number },
  ): Promise<{ id: string; token: string; direct_url: string }> {
    return this.call("cloudstorage", "shares", {
      method: "POST",
      data: {
        bucket,
        key,
        expires_in_hours: options?.expiresInHours,
        max_access_count: options?.maxAccessCount,
      },
    });
  }

  /**
   * List the current user's shares. `GET /b/cloudstorage/shares`.
   *
   * The handler serializes wafer-core's `RecordList` directly
   * (`ok_json(&result)` over `repo::shares::list_for_user`) — `{ records,
   * total_count, page, page_size }`, NOT a `{ data, total }` envelope. See
   * `wafer-block/src/wire/database.rs`.
   */
  async listShares(): Promise<ListSharesResult> {
    const result = await this.call<{
      records: Array<{ id: string; data: Omit<ShareRecord, "id"> }>;
      total_count: number;
      page: number;
      page_size: number;
    }>("cloudstorage", "shares");
    return {
      items: result.records.map((r) => ({ id: r.id, ...r.data })),
      total: result.total_count,
    };
  }

  /** Delete a share. `DELETE /b/cloudstorage/shares/{id}`. */
  async deleteShare(shareId: string): Promise<void> {
    await this.call("cloudstorage", `shares/${encodeURIComponent(shareId)}`, {
      method: "DELETE",
    });
  }

  /**
   * Get the current user's storage quota and usage.
   * `GET /b/cloudstorage/quota`.
   *
   * `usage` was `Record<string, unknown>` until the endpoint declared a
   * response schema. It is two numbers, both computed over the caller's
   * object rows by `blocks::files::quota::get_user_usage`.
   */
  async getQuota(): Promise<{
    quota: {
      max_storage_bytes: number;
      max_file_size_bytes: number;
      /** Most objects the caller may hold in any one bucket, in-flight uploads included. */
      max_files_per_bucket: number;
    };
    usage: {
      /** Bytes stored, in-flight (`pending`) uploads included. */
      total_bytes: number;
      /**
       * Objects the caller owns across all buckets, in-flight uploads
       * included. Not what the per-bucket `max_files_per_bucket` cap is
       * checked against.
       */
      file_count: number;
    };
  }> {
    return this.call("cloudstorage", "quota");
  }
}

export type CommerceScope = "admin" | "seller";
export type OfferMode = "payment" | "subscription";
export type CheckoutPresentation = "hosted" | "embedded" | "payment_link";
export type PricingModel = "fixed" | "components";
export type RecurringInterval = "day" | "week" | "month" | "year";
export type VariableKind = "number" | "integer" | "boolean" | "date" | "date_time" | "select" | "multi_select" | "text";

export interface WireRecord<T = Record<string, unknown>> {
  id: string;
  data: T;
}

export interface WireRecordList<T = Record<string, unknown>> {
  records: Array<WireRecord<T>>;
  total_count: number;
  page: number;
  page_size: number;
}

export interface MoneyBreakdown {
  currency: string;
  subtotal_minor: number;
  discount_minor: number;
  tax_minor: number;
  shipping_minor: number;
  platform_fee_minor: number;
  total_minor: number;
}

export interface VariableDefinition {
  key: string;
  kind: VariableKind;
  label: string;
  help_text?: string;
  required?: boolean;
  default_value?: unknown;
  allowed_values?: string[];
  minimum?: string;
  maximum?: string;
  step?: string;
  maximum_length?: number;
  visibility?: "public" | "hidden" | "admin_only";
  sort_order?: number;
}

export type PricingCondition =
  | { op: "always" }
  | { op: "all" | "any"; conditions: PricingCondition[] }
  | { op: "not"; condition: PricingCondition }
  | { op: "present"; input: string }
  | { op: "equals" | "not_equals" | "greater_than" | "greater_than_or_equal" | "less_than" | "less_than_or_equal" | "contains"; input: string; value: unknown }
  | { op: "in"; input: string; values: unknown[] };

export type AmountRule =
  | { type: "fixed"; unit_amount_minor: number }
  | { type: "per_unit"; input: string; unit_amount_minor: number }
  | { type: "flat_plus_per_unit"; base_amount_minor: number; input: string; unit_amount_minor: number }
  | { type: "lookup"; input: string; prices: Record<string, number> }
  | { type: "graduated"; input: string; tiers: PricingTier[] }
  | { type: "volume"; input: string; tiers: PricingTier[] }
  | {
      type: "package";
      input: string;
      units_per_package: number;
      package_amount_minor: number;
      rounding?: "up" | "exact";
    };

export interface PricingTier {
  /** Inclusive upper bound. The final tier must omit this value. */
  up_to?: number;
  unit_amount_minor: number;
  flat_amount_minor?: number;
}

export type QuantityRule =
  | { type: "fixed"; value: number }
  | { type: "from_input"; input: string; minimum?: number; maximum?: number };

export interface OfferComponentDraft {
  key: string;
  label: string;
  description?: string;
  sort_order?: number;
  required?: boolean;
  amount: AmountRule;
  quantity?: QuantityRule;
  condition?: PricingCondition;
  recurrence?: { interval: RecurringInterval; interval_count?: number };
  metadata?: Record<string, unknown>;
}

export interface CheckoutPolicy {
  /** Evaluated item total before provider discounts, tax, or shipping. */
  minimum_total_minor?: number;
  /** Evaluated item total before provider discounts, tax, or shipping. */
  maximum_total_minor?: number;
  allow_promotion_codes?: boolean;
  automatic_tax?: boolean;
  collect_billing_address?: boolean;
  collect_shipping_address?: boolean;
  allowed_shipping_countries?: string[];
  shipping_options?: ShippingOption[];
  create_customer?: boolean;
  require_terms_consent?: boolean;
  trial_days?: number;
}

export type ShippingEstimateUnit =
  | "hour"
  | "day"
  | "business_day"
  | "week"
  | "month";

export interface ShippingDeliveryEstimate {
  minimum?: number;
  maximum?: number;
  unit: ShippingEstimateUnit;
}

export interface ShippingOption {
  display_name: string;
  amount_minor: number;
  tax_behavior?: "unspecified" | "inclusive" | "exclusive";
  delivery_estimate?: ShippingDeliveryEstimate;
  /** Required when this offer is used to create a reusable Payment Link. */
  stripe_shipping_rate_id?: string;
}

export interface OfferDefinition {
  name: string;
  mode: OfferMode;
  currency: string;
  pricing_model: PricingModel;
  recurring_interval?: RecurringInterval;
  interval_count?: number;
  usage_type: "licensed" | "metered";
  billing_scheme: "per_unit" | "tiered";
  tax_behavior: "unspecified" | "inclusive" | "exclusive";
  variables?: VariableDefinition[];
  components: OfferComponentDraft[];
  checkout?: CheckoutPolicy;
}

export interface ManagedOffer {
  status: "draft" | "active" | "archived";
  sync_status: string;
  sync_error?: string;
  offer: OfferDefinition & { id: string; product_id: string; version: number };
}

/**
 * A product row as the owner and administrator endpoints publish it:
 * `contracts::ProductView` — every column of the products table, flat.
 * The `{id, data}` record envelope those endpoints used to echo is gone.
 */
export interface Product {
  id: string;
  name: string;
  description: string;
  slug: string;
  currency: string;
  status: "draft" | "pending_review" | "active" | "archived";
  category: string;
  tags: string[];
  metadata: Record<string, unknown>;
  image_url: string;
  stock: number;
  group_id: string;
  type_id: string;
  group_template_id: string;
  product_template_id: string;
  /** Id of a product the buyer must already own before checkout, or empty. */
  requires: string;
  created_by: string;
  owner_kind: "platform" | "user";
  owner_id: string;
  seller_account_id: string;
  approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
  fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
  stripe_product_id: string;
  current_version: number;
  submitted_at: string | null;
  published_at: string | null;
  deleted_at: string | null;
  created_at: string;
  updated_at: string;
}

/** A product group row: `contracts::GroupView`. */
export interface Group {
  id: string;
  name: string;
  description: string;
  group_template_id: string;
  user_id: string;
  status: string;
  created_by: string;
  created_at: string;
  updated_at: string;
}

/** `{records, total_count, page, page_size}` over `Group` rows. */
export interface GroupList {
  records: Group[];
  total_count: number;
  page: number;
  page_size: number;
}

/** `contracts::CreateGroupRequest`; a key not listed here is ignored. */
export interface GroupDraft {
  name: string;
  description?: string;
  group_template_id?: string;
  /** Admin only: the owner. Defaults to the creating administrator. */
  user_id?: string;
  status?: string;
}

/**
 * A product as the public catalog publishes it: `contracts::CatalogProductView`.
 * Ownership, moderation and provider columns are not part of it.
 */
export interface CatalogProduct {
  id: string;
  name: string;
  slug: string;
  description: string;
  image_url: string;
  tags: string[];
  category: string;
  currency: string;
  status: "active";
  stock: number;
  group_id: string;
  type_id: string;
  group_template_id: string;
  product_template_id: string;
  requires: string;
  metadata: Record<string, unknown>;
  fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
  published_at: string | null;
  created_at: string;
  updated_at: string;
}

/** `{records, total_count, page, page_size}` over `CatalogProduct` rows. */
export interface CatalogProductList {
  records: CatalogProduct[];
  total_count: number;
  page: number;
  page_size: number;
}

/** `{records, total_count, page, page_size}` over `Product` rows. */
export interface ProductList {
  records: Product[];
  total_count: number;
  page: number;
  page_size: number;
}

export interface ProductDuplicateResult {
  product: Product;
  offers: ManagedOffer[];
}

export interface OfferListResult {
  offers: ManagedOffer[];
}

export interface ResolvedComponent {
  component_id: string;
  key: string;
  label: string;
  included: boolean;
  required: boolean;
  unit_amount_minor: number;
  quantity: number;
  total_amount_minor: number;
  reason: string;
}

export interface PricingPreview {
  schema_version: number;
  offer_id: string;
  offer_version: number;
  quantity: number;
  inputs: Record<string, unknown>;
  components: ResolvedComponent[];
  amounts: MoneyBreakdown;
}

export interface StorefrontPaymentLink {
  id: string;
  preset_id?: string;
  url: string;
  pricing: PricingPreview;
}

export interface StorefrontOffer {
  id: string;
  version: number;
  name: string;
  mode: OfferMode;
  currency: string;
  pricing_model: PricingModel;
  recurring_interval?: RecurringInterval;
  interval_count: number;
  variables: VariableDefinition[];
  checkout: CheckoutPolicy;
  payment_links: StorefrontPaymentLink[];
}

export interface StorefrontProduct {
  schema_version: number;
  id: string;
  name: string;
  slug: string;
  description?: string;
  image_url?: string;
  tags?: string[];
  fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
  offers: StorefrontOffer[];
}

export interface StorefrontConfig {
  schema_version: number;
  embedded_checkout_available: boolean;
  stripe_publishable_key?: string;
  stripe_mode?: "test" | "live";
}

export interface CheckoutRequest {
  offer_id: string;
  preset_id?: string;
  quantity?: number;
  inputs?: Record<string, unknown>;
  presentation?: CheckoutPresentation;
  success_url?: string;
  cancel_url?: string;
  buyer_email?: string;
}

export interface CheckoutResponse {
  order_id: string;
  receipt_token: string;
  receipt_token_expires_at: string;
  presentation: CheckoutPresentation;
  checkout_url?: string;
  client_secret?: string;
  payment_link_url?: string;
  amounts: MoneyBreakdown;
}

export interface GuestOrderStatus {
  schema_version: number;
  order_id: string;
  status: string;
  reconciliation_status: string;
  amounts: MoneyBreakdown;
  /** Absent for a one-time order rather than `""`. */
  subscription_status?: Exclude<SubscriptionStatus, "">;
  subscription_current_period_end?: string;
  subscription_cancel_at_period_end: boolean;
  paid_at?: string;
  refunded_at?: string;
}

export interface CommerceAnalytics {
  currency: string;
  gross_volume_minor: number;
  refunded_volume_minor: number;
  net_volume_minor: number;
  platform_fees_minor: number;
  order_count: number;
  paid_order_count: number;
  refunded_order_count: number;
  failed_order_count: number;
  open_dispute_count: number;
  open_disputed_volume_minor: number;
  lost_dispute_count: number;
  lost_disputed_volume_minor: number;
  active_subscription_count: number;
  trialing_subscription_count: number;
  past_due_subscription_count: number;
  canceled_subscription_count: number;
  top_products: Array<{ product_id: string; name: string; quantity: number; revenue_minor: number }>;
}

export interface SellerFailureSummary {
  order_id: string;
  status: string;
  currency: string;
  total_minor: number;
  error: string;
  created_at: string;
}

export interface RefundRequest {
  amount_minor?: number;
  provider_reason?: "duplicate" | "fraudulent" | "requested_by_customer";
  note?: string;
  idempotency_key?: string;
}

export interface RefundResult {
  purchase_id: string;
  refund_id?: string;
  provider_refund_id?: string;
  status: "pending" | "succeeded" | "failed";
  provider_status?: string;
  amount_minor: number;
  refunded_total_minor: number;
  order_total_minor: number;
  currency: string;
  livemode: boolean;
}

/**
 * `contracts::CreateProductRequest` / `UpdateProductRequest`. Ownership,
 * moderation and provider columns (`owner_id`, `approval_status`,
 * `stripe_product_id`, …) are set by the server; a key that is not listed
 * here is ignored, not written.
 */
export interface ProductDraft {
  name: string;
  slug?: string;
  description?: string;
  currency?: string;
  status?: "draft" | "pending_review" | "active" | "archived";
  category?: string;
  tags?: string[];
  metadata?: Record<string, unknown>;
  image_url?: string;
  stock?: number;
  group_id?: string;
  type_id?: string;
  group_template_id?: string;
  product_template_id?: string;
  /** Id of a product the buyer must already own before checkout. */
  requires?: string;
  fulfillment_kind?: "none" | "manual" | "download" | "entitlement" | "webhook";
}

export interface CheckoutPreset {
  id: string;
  offer_id: string;
  name: string;
  slug: string;
  inputs: Record<string, unknown>;
  active: boolean;
  configuration_hash: string;
}

export interface CheckoutPresetListResult {
  presets: CheckoutPreset[];
}

export interface ManagedPaymentLink {
  id: string;
  offer_id: string;
  preset_id?: string;
  url: string;
  active: boolean;
  configuration_hash: string;
  sync_status: string;
  sync_error?: string;
}

export interface PaymentLinkListResult {
  payment_links: ManagedPaymentLink[];
}

export interface DeleteResult {
  deleted: boolean;
}

export type WebhookEventStatus =
  | "pending"
  | "processing"
  | "failed"
  | "processed"
  | "dead_letter";

/** Safe operator projection; signed payloads and processing-owner tokens are never exposed. */
export interface WebhookEventSummary {
  id: string;
  event_type: string;
  status: WebhookEventStatus;
  stripe_account_id: string;
  livemode: boolean;
  attempts: number;
  processing_started_at?: string;
  next_retry_at?: string;
  last_error: string;
  processed_at?: string;
  terminal_at?: string;
  created_at: string;
  updated_at: string;
}

export interface WebhookEventList {
  records: WebhookEventSummary[];
  total_count: number;
  page: number;
  page_size: number;
}

export type ProviderOperationStatus =
  | "pending"
  | "processing"
  | "failed"
  | "succeeded"
  | "dead_letter";

/** Safe operator projection; request payloads, idempotency keys, and lease owners are private. */
export interface ProviderOperationSummary {
  id: string;
  operation_type: "refund.reconcile";
  aggregate_type: "refund";
  aggregate_id: string;
  stripe_account_id: string;
  status: ProviderOperationStatus;
  attempts: number;
  processing_started_at?: string;
  next_attempt_at?: string;
  last_error: string;
  completed_at?: string;
  terminal_at?: string;
  created_at: string;
  updated_at: string;
}

export interface ProviderOperationList {
  records: ProviderOperationSummary[];
  total_count: number;
  page: number;
  page_size: number;
}

export interface ProviderReconcileResult {
  claimed: number;
  succeeded: number;
  retry_scheduled: number;
  dead_letter: number;
  /** Operations whose state could not be written; a later run retries them. */
  unrecorded: number;
}

/**
 * Stripe's subscription lifecycle, as the order views publish it. `""` is the
 * state of every non-subscription order, which is why it is the first value of
 * the published list; the guest order view (`GuestOrderStatus`) omits the
 * field for such an order instead.
 */
export type SubscriptionStatus =
  | ""
  | "incomplete"
  | "incomplete_expired"
  | "trialing"
  | "active"
  | "past_due"
  | "unpaid"
  | "paused"
  | "canceled";

/** The caller's platform subscription, as `GET /b/products/subscription` returns it. */
export interface PlatformSubscription {
  id: string;
  plan: string;
  status: SubscriptionStatus;
  /** Stripe Subscription id, or empty. */
  stripe_subscription_id: string;
  /** RFC 3339 end of the grace period after a failed payment, or `null`. */
  grace_period_end: string | null;
  addon_projects: number;
  addon_requests: number;
  addon_r2_bytes: number;
  addon_d1_bytes: number;
  created_at: string;
  updated_at: string;
}

/** Response of `GET /b/products/subscription`. */
export interface PlatformSubscriptionResponse {
  /** `null` when the caller has no subscription. */
  subscription: PlatformSubscription | null;
}

/** The refund ledger's own state, distinct from the provider's `provider_status`. */
export type RefundStatus = "pending" | "provider_succeeded" | "succeeded" | "failed";

/** How far a seller account has got with Stripe Connect. */
export type SellerStatus =
  | "not_started"
  | "onboarding"
  | "restricted"
  | "active"
  | "suspended";

export type DisputeStatus =
  | "warning_needs_response"
  | "warning_under_review"
  | "warning_closed"
  | "needs_response"
  | "under_review"
  | "won"
  | "lost"
  | "prevented";

/** `contracts::DisputeView`: the durable projection of a provider dispute. */
export interface Dispute {
  id: string;
  purchase_id: string;
  seller_account_id: string;
  stripe_account_id: string;
  provider_dispute_id: string;
  provider_charge_id: string;
  payment_intent_id: string;
  status: DisputeStatus;
  amount_minor: number;
  currency: string;
  reason: string;
  evidence_due_by: string | null;
  livemode: boolean;
  event_created: number;
  closed_at: string | null;
  created_at: string;
  updated_at: string;
}

/**
 * `contracts::PurchaseView`: an order row, flat. The guest receipt digest
 * (`receipt_token_hash`, `receipt_token_expires_at`) is never published.
 */
/**
 * The whole order row — the ADMIN projection
 * (`GET /b/products/api/admin/purchases`). Buyers get `BuyerOrder` and
 * sellers get `SellerOrder`; those endpoints no longer return this shape.
 */
export interface Purchase {
  id: string;
  /**
   * The single published buyer identity. The row also carries a `user_id`
   * column holding the same value, but the server publishes one answer.
   */
  buyer_user_id: string;
  buyer_email: string;
  seller_account_id: string;
  stripe_account_id: string;
  stripe_customer_id: string;
  stripe_subscription_id: string;
  status: string;
  checkout_mode: "hosted" | "embedded" | "payment_link";
  provider: string;
  livemode: boolean;
  currency: string;
  subtotal_cents: number;
  discount_cents: number;
  tax_cents: number;
  shipping_cents: number;
  platform_fee_cents: number;
  /**
   * The single published amount, in minor units. The row also carries an
   * `amount_cents` column holding the same value; the server publishes one.
   */
  total_cents: number;
  refunded_total_cents: number;
  metadata: Record<string, unknown>;
  stripe_payment_intent_id: string;
  provider_payment_intent_id: string;
  provider_session_id: string;
  provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
  provider_payment_error_code: string;
  provider_payment_error_message: string;
  payment_intent_event_created: number;
  reconciliation_status: string;
  reconciliation_error: string;
  subscription_status: SubscriptionStatus;
  subscription_current_period_end: string | null;
  subscription_cancel_at_period_end: boolean;
  subscription_canceled_at: string | null;
  subscription_last_synced_at: string | null;
  subscription_event_created: number;
  approved_at: string | null;
  payment_at: string | null;
  refunded_at: string | null;
  refunded_by: string;
  refund_reason: string;
  created_at: string;
  updated_at: string;
}

/**
 * One order as its buyer may read it — the projection behind
 * `GET /b/products/purchases`, which is also the `list_my_purchases` WebMCP
 * tool. Narrower than `Purchase` on purpose: the platform fee, the seller's
 * identity and Stripe account, the provider handles and the reconciliation
 * diagnostics are not the buyer's to read.
 */
export interface BuyerOrder {
  id: string;
  buyer_email: string;
  status: string;
  checkout_mode: "hosted" | "embedded" | "payment_link";
  provider: string;
  currency: string;
  subtotal_cents: number;
  discount_cents: number;
  tax_cents: number;
  shipping_cents: number;
  total_cents: number;
  refunded_total_cents: number;
  metadata: Record<string, unknown>;
  provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
  reconciliation_status: string;
  subscription_status: SubscriptionStatus;
  subscription_current_period_end: string | null;
  subscription_cancel_at_period_end: boolean;
  subscription_canceled_at: string | null;
  payment_at: string | null;
  refunded_at: string | null;
  refund_reason: string;
  created_at: string;
  updated_at: string;
}

/** One refund as its buyer may read it: how much, in what currency, and whether it landed. */
export interface BuyerRefund {
  id: string;
  purchase_id: string;
  amount_minor: number;
  currency: string;
  status: string;
  provider_status: string;
  completed_at: string | null;
  created_at: string;
}

/** `{records, total_count, page, page_size}` over `BuyerOrder` rows. */
export interface BuyerOrderList {
  records: BuyerOrder[];
  total_count: number;
  page: number;
  page_size: number;
}

export interface BuyerOrderDetail {
  purchase: BuyerOrder;
  line_items: LineItem[];
  refunds: BuyerRefund[];
  disputes: Dispute[];
}

/**
 * One order as the seller fulfilling it may read it. Wider than `BuyerOrder`
 * — the fee, the connected account and the provider handles for their own
 * charge — but without the buyer's platform identity or Stripe customer id.
 */
export interface SellerOrder {
  id: string;
  buyer_email: string;
  seller_account_id: string;
  stripe_account_id: string;
  status: string;
  checkout_mode: "hosted" | "embedded" | "payment_link";
  provider: string;
  livemode: boolean;
  currency: string;
  subtotal_cents: number;
  discount_cents: number;
  tax_cents: number;
  shipping_cents: number;
  platform_fee_cents: number;
  total_cents: number;
  refunded_total_cents: number;
  metadata: Record<string, unknown>;
  stripe_payment_intent_id: string;
  provider_session_id: string;
  provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
  provider_payment_error_code: string;
  provider_payment_error_message: string;
  reconciliation_status: string;
  reconciliation_error: string;
  subscription_status: SubscriptionStatus;
  subscription_current_period_end: string | null;
  subscription_cancel_at_period_end: boolean;
  payment_at: string | null;
  refunded_at: string | null;
  refunded_by: string;
  refund_reason: string;
  created_at: string;
  updated_at: string;
}

/** `{records, total_count, page, page_size}` over `SellerOrder` rows. */
export interface SellerOrderList {
  records: SellerOrder[];
  total_count: number;
  page: number;
  page_size: number;
}

export interface SellerOrderDetail {
  purchase: SellerOrder;
  line_items: LineItem[];
  refunds: Refund[];
  disputes: Dispute[];
}

/** `{records, total_count, page, page_size}` over `Purchase` rows (admin). */
export interface PurchaseList {
  records: Purchase[];
  total_count: number;
  page: number;
  page_size: number;
}

/** `contracts::LineItemView`: one line of an order. */
export interface LineItem {
  id: string;
  purchase_id: string;
  product_id: string;
  product_name: string;
  quantity: number;
  offer_id: string;
  offer_version: number;
  component_id: string;
  seller_account_id: string;
  stripe_price_id: string;
  unit_amount_minor: number;
  subtotal_minor: number;
  discount_minor: number;
  tax_minor: number;
  total_minor: number;
  input_snapshot: Record<string, unknown>;
  condition_snapshot: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

/**
 * `contracts::RefundView`: one refund on an order. The Stripe idempotency
 * key and raw provider response are never published.
 */
export interface Refund {
  id: string;
  purchase_id: string;
  provider_refund_id: string;
  payment_intent_id: string;
  stripe_account_id: string;
  amount_minor: number;
  target_refunded_total_minor: number;
  currency: string;
  status: RefundStatus;
  provider_status: string;
  provider_reason: string;
  note: string;
  refunded_by: string;
  livemode: boolean;
  last_error: string;
  completed_at: string | null;
  stripe_event_created: number;
  created_at: string;
  updated_at: string;
}

export interface PurchaseDetail {
  purchase: Purchase;
  line_items: LineItem[];
  refunds: Refund[];
  disputes: Dispute[];
}

export interface SellerAccount {
  id: string;
  user_id: string;
  status: SellerStatus;
  /**
   * Whether an administrator has suspended the account, derived from `status`.
   * Two values, not the five of a product's `approval_status`: this field has
   * only ever carried `approved` or `suspended`, and the published schema now
   * says so.
   */
  approval_status: "approved" | "suspended";
  stripe_account_id?: string;
  capabilities: {
    details_submitted: boolean;
    charges_enabled: boolean;
    payouts_enabled: boolean;
    requirements_due?: string[];
  };
  fee_basis_points: number;
  livemode?: boolean;
  country?: string;
  default_currency?: string;
  dashboard_type?: string;
  disabled_reason?: string;
  sync_error?: string;
  last_synced_at?: string;
}

export interface AdminSellerDetail {
  seller: SellerAccount;
  products: Product[];
}

/** Typed client for public, buyer, seller, and admin products APIs. */
export class ProductsExtension extends ExtensionsService {
  /** Browse the public product catalog. `GET /b/products/catalog`. */
  async listProducts(options?: { page?: number; page_size?: number }): Promise<CatalogProductList> {
    return this.call("products", "catalog", { params: options });
  }

  async getStorefrontProduct(productId: string): Promise<StorefrontProduct> {
    return this.call("products", `storefront/${encodeURIComponent(productId)}`);
  }

  async getStorefrontConfig(): Promise<StorefrontConfig> {
    return this.call("products", "storefront/config");
  }

  async previewPrice(request: { offer_id: string; quantity?: number; inputs?: Record<string, unknown> }): Promise<PricingPreview> {
    return this.call("products", "pricing/preview", { method: "POST", data: request });
  }

  async checkout(request: CheckoutRequest): Promise<CheckoutResponse> {
    return this.call("products", "checkout", { method: "POST", data: request });
  }

  async getGuestOrderStatus(orderId: string, receiptToken: string): Promise<GuestOrderStatus> {
    return this.call("products", `orders/${encodeURIComponent(orderId)}/status`, {
      params: { receipt_token: receiptToken },
    });
  }

  /** Create a product (admin). `POST /b/products/api/admin/products`. */
  async createProduct(data: ProductDraft): Promise<Product> {
    return this.call("products", "api/admin/products", {
      method: "POST",
      data,
    });
  }

  async getProduct(productId: string): Promise<Product> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}`);
  }

  async updateProduct(productId: string, data: Partial<ProductDraft>): Promise<Product> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}`, {
      method: "PATCH",
      data,
    });
  }

  async deleteProduct(productId: string): Promise<DeleteResult> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}`, { method: "DELETE" });
  }

  async duplicateProduct(productId: string): Promise<ProductDuplicateResult> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}/duplicate`, { method: "POST" });
  }

  async listSellerProducts(options?: { page?: number; page_size?: number; group_id?: string; status?: string; search?: string }): Promise<ProductList> {
    return this.call("products", "api/products", { params: options });
  }

  async createSellerProduct(data: ProductDraft): Promise<Product> {
    return this.call("products", "api/products", { method: "POST", data });
  }

  async getSellerProduct(productId: string): Promise<Product> {
    return this.call("products", `api/products/${encodeURIComponent(productId)}`);
  }

  async updateSellerProduct(productId: string, data: Partial<ProductDraft>): Promise<Product> {
    return this.call("products", `api/products/${encodeURIComponent(productId)}`, {
      method: "PATCH",
      data,
    });
  }

  async deleteSellerProduct(productId: string): Promise<DeleteResult> {
    return this.call("products", `api/products/${encodeURIComponent(productId)}`, { method: "DELETE" });
  }

  async duplicateSellerProduct(productId: string): Promise<ProductDuplicateResult> {
    return this.call("products", `api/products/${encodeURIComponent(productId)}/duplicate`, { method: "POST" });
  }

  async listOffers(productId: string, scope: CommerceScope = "admin"): Promise<OfferListResult> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers`);
  }

  async getOffer(productId: string, offerId: string, scope: CommerceScope = "admin"): Promise<ManagedOffer> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers/${encodeURIComponent(offerId)}`);
  }

  /** Evaluate an owned draft or active offer with the authoritative server pricing engine. */
  async previewManagedOffer(
    productId: string,
    offerId: string,
    request: { quantity?: number; inputs?: Record<string, unknown> } = {},
    scope: CommerceScope = "admin",
  ): Promise<PricingPreview> {
    return this.call(
      "products",
      `${this.offerPath(productId, offerId, scope)}/preview`,
      { method: "POST", data: { offer_id: offerId, ...request } },
    );
  }

  async createOffer(productId: string, data: OfferDefinition, scope: CommerceScope = "admin"): Promise<ManagedOffer> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers`, { method: "POST", data });
  }

  async updateOffer(productId: string, offerId: string, data: OfferDefinition, scope: CommerceScope = "admin"): Promise<ManagedOffer> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers/${encodeURIComponent(offerId)}`, { method: "PATCH", data });
  }

  async publishOffer(productId: string, offerId: string, scope: CommerceScope = "admin"): Promise<ManagedOffer> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers/${encodeURIComponent(offerId)}/publish`, { method: "POST" });
  }

  async syncOffer(productId: string, offerId: string, scope: CommerceScope = "admin"): Promise<ManagedOffer> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers/${encodeURIComponent(offerId)}/sync`, { method: "POST" });
  }

  async duplicateOffer(productId: string, offerId: string, scope: CommerceScope = "admin"): Promise<ManagedOffer> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers/${encodeURIComponent(offerId)}/duplicate`, { method: "POST" });
  }

  async archiveOffer(productId: string, offerId: string, scope: CommerceScope = "admin"): Promise<ManagedOffer> {
    return this.call("products", `${this.ownerProductPath(productId, scope)}/offers/${encodeURIComponent(offerId)}`, { method: "DELETE" });
  }

  async listCheckoutPresets(productId: string, offerId: string, scope: CommerceScope = "admin"): Promise<CheckoutPresetListResult> {
    return this.call("products", `${this.offerPath(productId, offerId, scope)}/presets`);
  }

  async createCheckoutPreset(productId: string, offerId: string, data: { name: string; slug?: string; inputs?: Record<string, unknown> }, scope: CommerceScope = "admin"): Promise<CheckoutPreset> {
    return this.call("products", `${this.offerPath(productId, offerId, scope)}/presets`, { method: "POST", data });
  }

  async updateCheckoutPreset(productId: string, offerId: string, presetId: string, data: { name: string; slug?: string; inputs?: Record<string, unknown> }, scope: CommerceScope = "admin"): Promise<CheckoutPreset> {
    return this.call("products", `${this.offerPath(productId, offerId, scope)}/presets/${encodeURIComponent(presetId)}`, { method: "PATCH", data });
  }

  async archiveCheckoutPreset(productId: string, offerId: string, presetId: string, scope: CommerceScope = "admin"): Promise<CheckoutPreset> {
    return this.call("products", `${this.offerPath(productId, offerId, scope)}/presets/${encodeURIComponent(presetId)}`, { method: "DELETE" });
  }

  async listPaymentLinks(productId: string, offerId: string, scope: CommerceScope = "admin"): Promise<PaymentLinkListResult> {
    return this.call("products", `${this.offerPath(productId, offerId, scope)}/payment-links`);
  }

  async createPaymentLink(productId: string, offerId: string, data: { preset_id?: string; after_completion_url?: string } = {}, scope: CommerceScope = "admin"): Promise<ManagedPaymentLink> {
    return this.call("products", `${this.offerPath(productId, offerId, scope)}/payment-links`, { method: "POST", data });
  }

  async deactivatePaymentLink(productId: string, offerId: string, linkId: string, scope: CommerceScope = "admin"): Promise<ManagedPaymentLink> {
    return this.call("products", `${this.offerPath(productId, offerId, scope)}/payment-links/${encodeURIComponent(linkId)}`, { method: "DELETE" });
  }

  async getAdminStats(): Promise<{ total_products: number; active_products: number; total_purchases: number; total_groups: number; currency_analytics: CommerceAnalytics[] }> {
    return this.call("products", "api/admin/stats");
  }

  async listAdminSellers(): Promise<{ sellers: SellerAccount[] }> {
    return this.call("products", "api/admin/sellers");
  }

  async getAdminSeller(sellerId: string): Promise<AdminSellerDetail> {
    return this.call("products", `api/admin/sellers/${encodeURIComponent(sellerId)}`);
  }

  async suspendAdminSeller(sellerId: string): Promise<SellerAccount> {
    return this.call("products", `api/admin/sellers/${encodeURIComponent(sellerId)}/suspend`, { method: "POST" });
  }

  async reactivateAdminSeller(sellerId: string): Promise<SellerAccount> {
    return this.call("products", `api/admin/sellers/${encodeURIComponent(sellerId)}/reactivate`, { method: "POST" });
  }

  async approveSellerProduct(productId: string): Promise<Product> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}/approve`, { method: "POST" });
  }

  async rejectSellerProduct(productId: string): Promise<Product> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}/reject`, { method: "POST" });
  }

  async getStripeStatus(): Promise<Record<string, unknown>> {
    return this.call("products", "api/admin/stripe/status");
  }

  async getAdminWebhookEvents(options?: {
    page?: number;
    page_size?: number;
    status?: WebhookEventStatus;
  }): Promise<WebhookEventList> {
    return this.call("products", "api/admin/webhook-events", { params: options });
  }

  async replayAdminWebhookEvent(eventId: string): Promise<{ received: boolean; duplicate?: boolean }> {
    return this.call(
      "products",
      `api/admin/webhook-events/${encodeURIComponent(eventId)}/replay`,
      { method: "POST" },
    );
  }

  async getAdminProviderOperations(options?: {
    page?: number;
    page_size?: number;
    status?: ProviderOperationStatus;
  }): Promise<ProviderOperationList> {
    return this.call("products", "api/admin/provider-operations", { params: options });
  }

  async reconcileAdminProviderOperations(limit?: number): Promise<ProviderReconcileResult> {
    return this.call("products", "api/admin/provider-operations/reconcile", {
      method: "POST",
      params: limit === undefined ? undefined : { limit },
    });
  }

  async listAdminOrders(options?: { page?: number; page_size?: number; status?: string; user_id?: string }): Promise<PurchaseList> {
    return this.call("products", "api/admin/purchases", { params: options });
  }

  async getAdminOrder(orderId: string): Promise<PurchaseDetail> {
    return this.call("products", `api/admin/purchases/${encodeURIComponent(orderId)}`);
  }

  async refundAdminOrder(orderId: string, request: RefundRequest = {}): Promise<RefundResult> {
    return this.call("products", `api/admin/purchases/${encodeURIComponent(orderId)}/refund`, { method: "POST", data: request });
  }

  async listPurchases(options?: { page?: number; page_size?: number }): Promise<BuyerOrderList> {
    return this.call("products", "purchases", { params: options });
  }

  async getPurchase(orderId: string): Promise<BuyerOrderDetail> {
    return this.call("products", `purchases/${encodeURIComponent(orderId)}`);
  }

  async getSubscription(): Promise<PlatformSubscriptionResponse> {
    return this.call("products", "subscription");
  }

  async createBillingPortal(returnUrl: string, orderId?: string): Promise<{ url: string }> {
    return this.call("products", "billing-portal", {
      method: "POST",
      data: { return_url: returnUrl, order_id: orderId },
    });
  }

  async getSellerAccount(): Promise<SellerAccount | null> {
    return this.call("products", "api/seller/account");
  }

  async startSellerOnboarding(returnUrl: string, refreshUrl: string): Promise<{ account: SellerAccount; url: string; expires_at: number }> {
    return this.call("products", "api/seller/onboarding", {
      method: "POST",
      data: { return_url: returnUrl, refresh_url: refreshUrl },
    });
  }

  async createSellerDashboardLink(): Promise<{ url: string }> {
    return this.call("products", "api/seller/dashboard", { method: "POST" });
  }

  async getSellerStats(): Promise<{ seller_account_id: string; currency_analytics: CommerceAnalytics[]; recent_failures: SellerFailureSummary[] }> {
    return this.call("products", "api/seller/stats");
  }

  async listSellerOrders(options?: { page?: number; page_size?: number; status?: string }): Promise<SellerOrderList> {
    return this.call("products", "api/seller/orders", { params: options });
  }

  async getSellerOrder(orderId: string): Promise<SellerOrderDetail> {
    return this.call("products", `api/seller/orders/${encodeURIComponent(orderId)}`);
  }

  async refundSellerOrder(orderId: string, request: RefundRequest = {}): Promise<RefundResult> {
    return this.call("products", `api/seller/orders/${encodeURIComponent(orderId)}/refund`, { method: "POST", data: request });
  }

  /** List product groups (admin). `GET /b/products/api/admin/groups`. */
  async listGroups(options?: { page?: number; page_size?: number }): Promise<GroupList> {
    return this.call("products", "api/admin/groups", { params: options });
  }

  /** Create a product group (admin). `POST /b/products/api/admin/groups`. */
  async createGroup(data: GroupDraft): Promise<Group> {
    return this.call("products", "api/admin/groups", {
      method: "POST",
      data,
    });
  }

  private ownerProductPath(productId: string, scope: CommerceScope): string {
    const prefix = scope === "admin" ? "api/admin/products" : "api/products";
    return `${prefix}/${encodeURIComponent(productId)}`;
  }

  private offerPath(productId: string, offerId: string, scope: CommerceScope): string {
    return `${this.ownerProductPath(productId, scope)}/offers/${encodeURIComponent(offerId)}`;
  }
}
