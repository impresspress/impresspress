import { BaseService } from "./base.service";

export interface Extension {
  name: string;
  version: string;
  description: string;
  author: string;
  enabled: boolean;
  config?: Record<string, any>;
  metadata?: {
    tags?: string[];
    homepage?: string;
    license?: string;
  };
}

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
    options?: {
      method?: "GET" | "POST" | "PUT" | "DELETE" | "PATCH";
      data?: any;
      params?: Record<string, any>;
    },
  ): Promise<T> {
    const queryString = options?.params ? this.buildQueryString(options.params) : "";
    return this.request<T>({
      method: options?.method || "GET",
      url: `/b/${extension}/${endpoint}${queryString ? `?${queryString}` : ""}`,
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
  expires_at?: string;
  max_access_count?: number;
}

export interface ListSharesResult {
  items: ShareRecord[];
  total: number;
}

/**
 * Aligned to the real `impresspress/files` cloud-storage surface in
 * `crates/impresspress-core/src/blocks/files/cloud.rs`: per-object share
 * links and the caller's own quota/usage. There is no user-facing
 * access-log or access-stats endpoint (`/admin/b/cloudstorage/access-logs`
 * is admin-only and reached through the admin block's delegated HTTP
 * surface, not this one; `access-stats` does not exist at all) — both were
 * removed rather than pointed at a route that would 404 or silently expose
 * the wrong auth boundary.
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

  /** Get the current user's storage quota and usage. `GET /b/cloudstorage/quota`. */
  async getQuota(): Promise<{
    quota: {
      max_storage_bytes: number;
      max_file_size_bytes: number;
      max_files_per_bucket: number;
      reset_period_days: number;
    };
    usage: Record<string, unknown>;
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

export interface ProductDuplicateResult {
  product: WireRecord;
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
  subscription_status?: string;
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

export interface ProductDraft {
  name: string;
  slug?: string;
  description?: string;
  image_url?: string;
  tags?: string[];
  group_id?: string;
  product_template_id?: string;
  fulfillment_kind?: "none" | "manual" | "download" | "entitlement" | "webhook";
  status?: "draft" | "pending_review" | "active" | "archived";
  metadata?: Record<string, unknown>;
  [key: string]: unknown;
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
}

export type DisputeStatus =
  | "warning_needs_response"
  | "warning_under_review"
  | "warning_closed"
  | "needs_response"
  | "under_review"
  | "won"
  | "lost"
  | "prevented";

export interface DisputeSummary {
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
  evidence_due_by?: string;
  livemode: boolean;
  event_created: number;
  closed_at?: string;
  created_at: string;
  updated_at: string;
}

export type PurchaseRecordData = Record<string, unknown> & {
  provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
  provider_payment_error_code: string;
  provider_payment_error_message: string;
  payment_intent_event_created: number;
};

export interface PurchaseDetail {
  purchase: WireRecord<PurchaseRecordData>;
  line_items: WireRecord[];
  refunds: WireRecord[];
  disputes: Array<WireRecord<DisputeSummary>>;
}

export interface SellerAccount {
  id: string;
  user_id: string;
  status: string;
  approval_status: string;
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
  products: WireRecord[];
}

/** Typed client for public, buyer, seller, and admin products APIs. */
export class ProductsExtension extends ExtensionsService {
  /** Browse the public product catalog. `GET /b/products/catalog`. */
  async listProducts(options?: { page?: number; page_size?: number }): Promise<WireRecordList> {
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
  async createProduct(data: ProductDraft): Promise<WireRecord> {
    return this.call("products", "api/admin/products", {
      method: "POST",
      data,
    });
  }

  async getProduct(productId: string): Promise<WireRecord> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}`);
  }

  async updateProduct(productId: string, data: Partial<ProductDraft>): Promise<WireRecord> {
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

  async listSellerProducts(options?: { page?: number; page_size?: number; status?: string; search?: string }): Promise<WireRecordList> {
    return this.call("products", "api/products", { params: options });
  }

  async createSellerProduct(data: ProductDraft): Promise<WireRecord> {
    return this.call("products", "api/products", { method: "POST", data });
  }

  async getSellerProduct(productId: string): Promise<WireRecord> {
    return this.call("products", `api/products/${encodeURIComponent(productId)}`);
  }

  async updateSellerProduct(productId: string, data: Partial<ProductDraft>): Promise<WireRecord> {
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

  async approveSellerProduct(productId: string): Promise<WireRecord> {
    return this.call("products", `api/admin/products/${encodeURIComponent(productId)}/approve`, { method: "POST" });
  }

  async rejectSellerProduct(productId: string): Promise<WireRecord> {
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

  async listAdminOrders(options?: { page?: number; page_size?: number; status?: string }): Promise<WireRecordList> {
    return this.call("products", "api/admin/purchases", { params: options });
  }

  async getAdminOrder(orderId: string): Promise<PurchaseDetail> {
    return this.call("products", `api/admin/purchases/${encodeURIComponent(orderId)}`);
  }

  async refundAdminOrder(orderId: string, request: RefundRequest = {}): Promise<RefundResult> {
    return this.call("products", `api/admin/purchases/${encodeURIComponent(orderId)}/refund`, { method: "POST", data: request });
  }

  async listPurchases(options?: { page?: number; page_size?: number }): Promise<WireRecordList> {
    return this.call("products", "purchases", { params: options });
  }

  async getPurchase(orderId: string): Promise<PurchaseDetail> {
    return this.call("products", `purchases/${encodeURIComponent(orderId)}`);
  }

  async getSubscription(): Promise<unknown> {
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

  async listSellerOrders(options?: { page?: number; page_size?: number; status?: string }): Promise<WireRecordList> {
    return this.call("products", "api/seller/orders", { params: options });
  }

  async getSellerOrder(orderId: string): Promise<PurchaseDetail> {
    return this.call("products", `api/seller/orders/${encodeURIComponent(orderId)}`);
  }

  async refundSellerOrder(orderId: string, request: RefundRequest = {}): Promise<RefundResult> {
    return this.call("products", `api/seller/orders/${encodeURIComponent(orderId)}/refund`, { method: "POST", data: request });
  }

  /** List product groups (admin). `GET /b/products/api/admin/groups`. */
  async listGroups(): Promise<WireRecordList> {
    return this.call("products", "api/admin/groups");
  }

  /** Create a product group (admin). `POST /b/products/api/admin/groups`. */
  async createGroup(data: {
    name: string;
    group_template_id?: string;
    [key: string]: unknown;
  }): Promise<WireRecord> {
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
