pub mod contracts;
mod handlers;
pub(crate) mod migrations;
pub mod money;
pub mod offer_pricing;
mod pages;
mod purchase;
mod repo;
mod stripe;
mod stripe_client;
mod stripe_provider;

#[cfg(test)]
mod tests;

pub(crate) use handlers::{
    GROUPS_TABLE, GROUP_TEMPLATES_TABLE, PRODUCTS_TABLE, PRODUCT_TEMPLATES_TABLE, TYPES_TABLE,
};
pub(crate) use repo::purchases::{LINE_ITEMS_TABLE, PURCHASES_TABLE};
pub(crate) use repo::variables::TABLE as VARIABLES_TABLE;
use wafer_core::clients::config;
use wafer_run::{BlockEndpoint, BlockInfo, ConfigVar, InputType, InstanceMode};

use super::rate_limit::{
    check_route_limits, check_user_rate_limit, LimitKey, RateLimit, RateLimitOutcome, RouteLimit,
    UserRateLimiter,
};
use crate::http::{err_forbidden, err_not_found};

/// Public commerce operations are keyed by client IP because guest storefronts
/// deliberately have no authenticated user. Each category can be overridden
/// (or disabled with `0`) through `WAFER_RUN_SHARED__RATE_LIMIT_<CATEGORY>`.
const PUBLIC_RATE_LIMIT_ROUTES: &[RouteLimit] = &[
    RouteLimit {
        matches: |action, path| action == "create" && path == "/b/products/pricing/preview",
        key: LimitKey::Ip,
        category: "products_preview",
        limit: RateLimit::PRODUCTS_PREVIEW,
    },
    RouteLimit {
        matches: |action, path| action == "create" && path == "/b/products/checkout",
        key: LimitKey::Ip,
        category: "products_checkout",
        limit: RateLimit::PRODUCTS_CHECKOUT,
    },
    RouteLimit {
        matches: |action, path| {
            action == "retrieve"
                && path.starts_with("/b/products/orders/")
                && path.ends_with("/status")
        },
        key: LimitKey::Ip,
        category: "products_receipt",
        limit: RateLimit::PRODUCTS_RECEIPT,
    },
];

/// Adapter-injected runtime identity. The browser service-worker adapter sets
/// this directly on its in-memory ConfigService after loading persisted
/// variables, so an admin database value cannot accidentally turn a public
/// browser runtime into a trusted secret holder. Native and Cloudflare leave
/// it unset and retain the server default. Double-underscore brackets mark
/// the key as internal (same convention as `BLOCK_SETTINGS_CONFIG_KEY`) — it
/// is never set via env var or the variables table, so it must not claim the
/// admin-writable `WAFER_RUN_SHARED__` prefix.
pub const RUNTIME_KIND_CONFIG_KEY: &str = "__IMPRESSPRESS_RUNTIME_KIND__";

pub(crate) async fn stripe_secret_operations_allowed(
    ctx: &dyn wafer_run::context::Context,
) -> bool {
    config::get_default(ctx, RUNTIME_KIND_CONFIG_KEY, "server").await != "browser"
}

/// The products block's own declared config vars. Single source of truth for
/// both `BlockInfo::config_keys` and the admin settings page (which renders
/// these via `ui::settings_form` rather than a parallel tuple table).
/// Stripe presentment currencies offered for the platform default. Values
/// are ISO 4217; the storefront still accepts any valid currency configured
/// per product — this list only drives the admin default select.
const CURRENCY_OPTIONS: &[(&str, &str)] = &[
    ("USD", "USD — US Dollar"),
    ("EUR", "EUR — Euro"),
    ("GBP", "GBP — British Pound"),
    ("AUD", "AUD — Australian Dollar"),
    ("CAD", "CAD — Canadian Dollar"),
    ("NZD", "NZD — New Zealand Dollar"),
    ("JPY", "JPY — Japanese Yen"),
    ("CHF", "CHF — Swiss Franc"),
    ("SEK", "SEK — Swedish Krona"),
    ("NOK", "NOK — Norwegian Krone"),
    ("DKK", "DKK — Danish Krone"),
    ("SGD", "SGD — Singapore Dollar"),
    ("HKD", "HKD — Hong Kong Dollar"),
    ("INR", "INR — Indian Rupee"),
    ("BRL", "BRL — Brazilian Real"),
    ("MXN", "MXN — Mexican Peso"),
    ("PLN", "PLN — Polish Zloty"),
    ("CZK", "CZK — Czech Koruna"),
    ("AED", "AED — UAE Dirham"),
    ("ZAR", "ZAR — South African Rand"),
];

/// Countries where Stripe supports platform accounts (ISO 3166-1 alpha-2).
/// The leading empty value renders as "Not set" so the optional var can be
/// cleared from the select widget.
const COUNTRY_OPTIONS: &[(&str, &str)] = &[
    ("", "Not set"),
    ("AU", "Australia"),
    ("AT", "Austria"),
    ("BE", "Belgium"),
    ("BG", "Bulgaria"),
    ("BR", "Brazil"),
    ("CA", "Canada"),
    ("HR", "Croatia"),
    ("CY", "Cyprus"),
    ("CZ", "Czechia"),
    ("DK", "Denmark"),
    ("EE", "Estonia"),
    ("FI", "Finland"),
    ("FR", "France"),
    ("DE", "Germany"),
    ("GI", "Gibraltar"),
    ("GR", "Greece"),
    ("HK", "Hong Kong"),
    ("HU", "Hungary"),
    ("IN", "India"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IT", "Italy"),
    ("JP", "Japan"),
    ("LV", "Latvia"),
    ("LI", "Liechtenstein"),
    ("LT", "Lithuania"),
    ("LU", "Luxembourg"),
    ("MY", "Malaysia"),
    ("MT", "Malta"),
    ("MX", "Mexico"),
    ("NL", "Netherlands"),
    ("NZ", "New Zealand"),
    ("NG", "Nigeria"),
    ("NO", "Norway"),
    ("PL", "Poland"),
    ("PT", "Portugal"),
    ("RO", "Romania"),
    ("SG", "Singapore"),
    ("SK", "Slovakia"),
    ("SI", "Slovenia"),
    ("ZA", "South Africa"),
    ("ES", "Spain"),
    ("SE", "Sweden"),
    ("CH", "Switzerland"),
    ("TH", "Thailand"),
    ("AE", "United Arab Emirates"),
    ("GB", "United Kingdom"),
    ("US", "United States"),
];

pub(crate) fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "Stripe API secret key",
            "",
        )
        .name("Stripe Secret Key")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
            "Stripe publishable key used by embedded Checkout and static storefronts. This key is safe to send to browsers, but is masked in admin storage and pages to prevent accidental configuration disclosure.",
            "",
        )
        .name("Stripe Publishable Key")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            "Stripe webhook signing secret",
            "",
        )
        .name("Stripe Webhook Secret")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
            "Stripe API base URL",
            "https://api.stripe.com",
        )
        .name("Stripe API URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
            "Stripe API version sent with every provider request and expected by the webhook destination",
            "2026-02-25.clover",
        )
        .name("Stripe API Version")
        .input_type(InputType::Text),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__DEFAULT_CURRENCY",
            "Currency preselected for new products and offers",
            "USD",
        )
        .name("Default Currency")
        .input_type(InputType::Select)
        .options(CURRENCY_OPTIONS),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY",
            "Country of the platform Stripe account; also the seller onboarding default",
            "",
        )
        .name("Platform Country")
        .input_type(InputType::Select)
        .options(COUNTRY_OPTIONS)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX",
            "Enable Stripe automatic tax by default for new offers",
            "false",
        )
        .name("Automatic Tax")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS",
            "Comma-separated HTTPS origins allowed for Checkout return and cancel URLs; localhost HTTP origins are accepted in development",
            "",
        )
        .name("Checkout Allowed Origins")
        .input_type(InputType::Text)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
            "Default platform application fee for connected-account sales, in basis points (0-10000)",
            "0",
        )
        .name("Seller Application Fee (bps)")
        .input_type(InputType::Number),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED",
            "Require admin approval before a user-owned product can be published",
            "true",
        )
        .name("Moderate Seller Products")
        .input_type(InputType::Toggle),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_TEMPLATES",
            "Optional comma-separated product template IDs sellers may use; blank allows every template",
            "",
        )
        .name("Seller Allowed Templates")
        .input_type(InputType::Text)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES",
            "Optional comma-separated ISO currency codes sellers may use; blank allows every valid currency",
            "",
        )
        .name("Seller Allowed Currencies")
        .input_type(InputType::Text)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CATEGORIES",
            "Optional comma-separated product categories sellers may use; blank allows every category",
            "",
        )
        .name("Seller Allowed Categories")
        .input_type(InputType::Text)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS",
            "Maximum non-deleted products per seller; 0 means unlimited",
            "0",
        )
        .name("Seller Product Limit")
        .input_type(InputType::Number),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__WEBHOOK_URL",
            "Webhook URL for billing events",
            "",
        )
        .name("Billing Webhook URL")
        .input_type(InputType::Url)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET",
            "Webhook signing secret",
            "",
        )
        .name("Billing Webhook Secret")
        .input_type(InputType::Password)
        .auto_generate(),
    ]
}

crate::impresspress_feature_block! {
    /// Products, groups, pricing, purchases, subscriptions (`impresspress/products`).
    pub struct ProductsBlock;
    fields: { limiter: UserRateLimiter },
    name: "impresspress/products",
    info: |_this| {
        use wafer_run::{AuthLevel, CollectionSchema};

        // Product row shape (see `migrations/001_products_schema.sqlite.sql`),
        // reused below by the public catalog list/detail response schemas —
        // `db::get`/`db::paginated_list` return a `Record { id, data }` where
        // `data` is the full column map (`id` included).
        let product_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "slug": {"type": "string"},
                "currency": {"type": "string"},
                "status": {"type": "string", "description": "draft | active"},
                "category": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "metadata": {"type": "object"},
                "image_url": {"type": "string"},
                "stock": {"type": "integer"},
                "group_id": {"type": "string"},
                "type_id": {"type": "string"},
                "group_template_id": {"type": "string"},
                "product_template_id": {"type": "string"},
                "requires": {"type": "string"},
                "created_by": {"type": "string"},
                "owner_kind": {"type": "string", "enum": ["platform", "user"]},
                "owner_id": {"type": "string"},
                "seller_account_id": {"type": "string"},
                "approval_status": {"type": "string", "enum": ["draft", "pending_review", "approved", "rejected", "suspended"]},
                "fulfillment_kind": {"type": "string", "enum": ["none", "manual", "download", "entitlement", "webhook"]},
                "stripe_product_id": {"type": "string"},
                "current_version": {"type": "integer", "minimum": 1},
                "submitted_at": {"type": ["string", "null"], "format": "date-time"},
                "published_at": {"type": ["string", "null"], "format": "date-time"},
                "deleted_at": {"type": ["string", "null"], "format": "date-time", "description": "Null unless the product has been soft-deleted."},
                "created_at": {"type": "string", "format": "date-time"},
                "updated_at": {"type": "string", "format": "date-time"}
            }
        });
        let group_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "group_template_id": {"type": "string"},
                "user_id": {"type": "string", "readOnly": true},
                "status": {"type": "string"},
                "created_by": {"type": "string", "readOnly": true},
                "created_at": {"type": "string", "format": "date-time", "readOnly": true},
                "updated_at": {"type": "string", "format": "date-time", "readOnly": true}
            }
        });
        let product_type_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "is_system": {"type": "integer"},
                "created_at": {"type": "string", "format": "date-time"},
                "updated_at": {"type": "string", "format": "date-time"}
            }
        });
        let group_template_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "display_name": {"type": "string"},
                "created_at": {"type": "string", "format": "date-time"},
                "updated_at": {"type": "string", "format": "date-time"}
            }
        });
        let record_schema = |data: serde_json::Value| serde_json::json!({
            "type": "object",
            "required": ["id", "data"],
            "properties": {
                "id": {"type": "string"},
                "data": data
            }
        });
        let record_list_schema = |data: serde_json::Value| serde_json::json!({
            "type": "object",
            "required": ["records", "total_count"],
            "properties": {
                "records": {
                    "type": "array",
                    "items": record_schema(data)
                },
                "total_count": {"type": "integer"},
                "page": {"type": "integer"},
                "page_size": {"type": "integer"}
            }
        });
        let id_path_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id"],
            "properties": {"id": {"type": "string"}}
        });
        let money_breakdown_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["currency", "subtotal_minor", "discount_minor", "tax_minor", "shipping_minor", "platform_fee_minor", "total_minor"],
            "properties": {
                "currency": {"type": "string"},
                "subtotal_minor": {"type": "integer"},
                "discount_minor": {"type": "integer"},
                "tax_minor": {"type": "integer"},
                "shipping_minor": {"type": "integer"},
                "platform_fee_minor": {"type": "integer"},
                "total_minor": {"type": "integer"}
            }
        });
        let pricing_preview_input_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["offer_id"],
            "properties": {
                "offer_id": {"type": "string"},
                "quantity": {"type": "integer", "minimum": 1, "default": 1},
                "inputs": {"type": "object"}
            }
        });
        let pricing_preview_output_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["schema_version", "offer_id", "offer_version", "quantity", "inputs", "components", "amounts"],
            "properties": {
                "schema_version": {"type": "integer"},
                "offer_id": {"type": "string"},
                "offer_version": {"type": "integer"},
                "quantity": {"type": "integer"},
                "inputs": {"type": "object"},
                "components": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["component_id", "key", "label", "included", "required", "unit_amount_minor", "quantity", "total_amount_minor", "reason"],
                        "properties": {
                            "component_id": {"type": "string"},
                            "key": {"type": "string"},
                            "label": {"type": "string"},
                            "included": {"type": "boolean"},
                            "required": {"type": "boolean"},
                            "unit_amount_minor": {"type": "integer"},
                            "quantity": {"type": "integer"},
                            "total_amount_minor": {"type": "integer"},
                            "reason": {"type": "string"}
                        }
                    }
                },
                "amounts": money_breakdown_schema
            }
        });
        let checkout_input_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["offer_id"],
            "properties": {
                "offer_id": {"type": "string"},
                "preset_id": {"type": ["string", "null"]},
                "quantity": {"type": "integer", "minimum": 1, "default": 1},
                "inputs": {"type": "object"},
                "presentation": {"type": "string", "enum": ["hosted", "embedded", "payment_link"], "default": "hosted"},
                "success_url": {"type": ["string", "null"], "format": "uri"},
                "cancel_url": {"type": ["string", "null"], "format": "uri"},
                "buyer_email": {"type": ["string", "null"], "format": "email"}
            }
        });
        let checkout_output_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["order_id", "receipt_token", "receipt_token_expires_at", "presentation", "amounts"],
            "properties": {
                "order_id": {"type": "string"},
                "receipt_token": {"type": "string", "writeOnly": true},
                "receipt_token_expires_at": {"type": "string", "format": "date-time"},
                "presentation": {"type": "string", "enum": ["hosted", "embedded", "payment_link"]},
                "checkout_url": {"type": ["string", "null"], "format": "uri"},
                "client_secret": {"type": ["string", "null"], "writeOnly": true},
                "payment_link_url": {"type": ["string", "null"], "format": "uri"},
                "amounts": money_breakdown_schema
            }
        });
        let purchase_list_schema = record_list_schema(serde_json::json!({"type": "object"}));
        let purchase_data_schema = serde_json::json!({
            "type": "object",
            "required": ["provider_payment_status", "provider_payment_error_code", "provider_payment_error_message", "payment_intent_event_created"],
            "properties": {
                "provider_payment_status": {"type": "string", "enum": ["", "succeeded", "payment_failed", "processing", "requires_action", "canceled"]},
                "provider_payment_error_code": {"type": "string"},
                "provider_payment_error_message": {"type": "string"},
                "payment_intent_event_created": {"type": "integer"}
            }
        });
        let purchase_detail_schema = serde_json::json!({
            "type": "object",
            "required": ["purchase", "line_items", "refunds", "disputes"],
            "properties": {
                "purchase": record_schema(purchase_data_schema),
                "line_items": {"type": "array", "items": record_schema(serde_json::json!({"type": "object"}))},
                "refunds": {"type": "array", "items": record_schema(serde_json::json!({"type": "object"}))},
                "disputes": {
                    "type": "array",
                    "items": record_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["purchase_id", "seller_account_id", "stripe_account_id", "provider_dispute_id", "provider_charge_id", "payment_intent_id", "status", "amount_minor", "currency", "reason", "livemode", "event_created", "created_at", "updated_at"],
                        "properties": {
                            "purchase_id": {"type": "string"},
                            "seller_account_id": {"type": "string"},
                            "stripe_account_id": {"type": "string"},
                            "provider_dispute_id": {"type": "string"},
                            "provider_charge_id": {"type": "string"},
                            "payment_intent_id": {"type": "string"},
                            "status": {"type": "string", "enum": ["warning_needs_response", "warning_under_review", "warning_closed", "needs_response", "under_review", "won", "lost", "prevented"]},
                            "amount_minor": {"type": "integer"},
                            "currency": {"type": "string"},
                            "reason": {"type": "string"},
                            "evidence_due_by": {"type": ["string", "null"], "format": "date-time"},
                            "livemode": {"type": "boolean"},
                            "event_created": {"type": "integer"},
                            "closed_at": {"type": ["string", "null"], "format": "date-time"},
                            "created_at": {"type": "string", "format": "date-time"},
                            "updated_at": {"type": "string", "format": "date-time"}
                        }
                    }))
                }
            }
        });
        let subscription_status_schema = serde_json::json!({
            "type": "object",
            "required": ["subscription"],
            "properties": {
                "subscription": {
                    "type": ["object", "null"],
                    "properties": {
                        "id": {"type": "string"},
                        "plan": {"type": "string"},
                        "status": {"type": "string"},
                        "stripe_subscription_id": {"type": "string"},
                        "grace_period_end": {"type": ["string", "null"], "format": "date-time"},
                        "addon_projects": {"type": "integer"},
                        "addon_requests": {"type": "integer"},
                        "addon_r2_bytes": {"type": "integer"},
                        "addon_d1_bytes": {"type": "integer"},
                        "created_at": {"type": "string", "format": "date-time"},
                        "updated_at": {"type": "string", "format": "date-time"}
                    }
                }
            }
        });
        let provider_redirect_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["url"],
            "properties": {"url": {"type": "string", "format": "uri"}}
        });
        let seller_account_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id", "user_id", "status", "approval_status", "stripe_account_id", "capabilities", "fee_basis_points", "livemode", "country", "default_currency", "dashboard_type", "disabled_reason", "sync_error", "last_synced_at"],
            "properties": {
                "id": {"type": "string"},
                "user_id": {"type": "string"},
                "status": {"type": "string"},
                "approval_status": {"type": "string", "enum": ["draft", "pending", "approved", "rejected", "suspended"]},
                "stripe_account_id": {"type": "string"},
                "capabilities": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["details_submitted", "charges_enabled", "payouts_enabled", "requirements_due"],
                    "properties": {
                        "details_submitted": {"type": "boolean"},
                        "charges_enabled": {"type": "boolean"},
                        "payouts_enabled": {"type": "boolean"},
                        "requirements_due": {"type": "array", "items": {"type": "string"}}
                    }
                },
                "fee_basis_points": {"type": "integer", "minimum": 0, "maximum": 10000},
                "livemode": {"type": "boolean"},
                "country": {"type": "string"},
                "default_currency": {"type": "string"},
                "dashboard_type": {"type": "string"},
                "disabled_reason": {"type": "string"},
                "sync_error": {"type": "string"},
                "last_synced_at": {"type": "string"}
            }
        });
        let commerce_analytics_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["currency", "gross_volume_minor", "refunded_volume_minor", "net_volume_minor", "platform_fees_minor", "order_count", "paid_order_count", "refunded_order_count", "failed_order_count", "open_dispute_count", "open_disputed_volume_minor", "lost_dispute_count", "lost_disputed_volume_minor", "active_subscription_count", "trialing_subscription_count", "past_due_subscription_count", "canceled_subscription_count", "top_products"],
            "properties": {
                "currency": {"type": "string"},
                "gross_volume_minor": {"type": "integer"},
                "refunded_volume_minor": {"type": "integer"},
                "net_volume_minor": {"type": "integer"},
                "platform_fees_minor": {"type": "integer"},
                "order_count": {"type": "integer"},
                "paid_order_count": {"type": "integer"},
                "refunded_order_count": {"type": "integer"},
                "failed_order_count": {"type": "integer"},
                "open_dispute_count": {"type": "integer"},
                "open_disputed_volume_minor": {"type": "integer"},
                "lost_dispute_count": {"type": "integer"},
                "lost_disputed_volume_minor": {"type": "integer"},
                "active_subscription_count": {"type": "integer"},
                "trialing_subscription_count": {"type": "integer"},
                "past_due_subscription_count": {"type": "integer"},
                "canceled_subscription_count": {"type": "integer"},
                "top_products": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["product_id", "name", "quantity", "revenue_minor"],
                        "properties": {
                            "product_id": {"type": "string"},
                            "name": {"type": "string"},
                            "quantity": {"type": "integer"},
                            "revenue_minor": {"type": "integer"}
                        }
                    }
                }
            }
        });
        let seller_stats_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["seller_account_id", "currency_analytics", "recent_failures"],
            "properties": {
                "seller_account_id": {"type": "string"},
                "currency_analytics": {"type": "array", "items": commerce_analytics_schema},
                "recent_failures": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["order_id", "status", "currency", "total_minor", "error", "created_at"],
                        "properties": {
                            "order_id": {"type": "string"},
                            "status": {"type": "string"},
                            "currency": {"type": "string"},
                            "total_minor": {"type": "integer"},
                            "error": {"type": "string"},
                            "created_at": {"type": "string", "format": "date-time"}
                        }
                    }
                }
            }
        });
        let refund_input_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "amount_minor": {"type": ["integer", "null"], "minimum": 1},
                "provider_reason": {"type": ["string", "null"], "enum": ["duplicate", "fraudulent", "requested_by_customer", null]},
                "note": {"type": ["string", "null"], "maxLength": 500},
                "idempotency_key": {"type": ["string", "null"], "maxLength": 80}
            }
        });
        let refund_output_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["purchase_id", "refund_id", "provider_refund_id", "status", "provider_status", "amount_minor", "refunded_total_minor", "order_total_minor", "currency", "livemode"],
            "properties": {
                "purchase_id": {"type": "string"},
                "refund_id": {"type": "string"},
                "provider_refund_id": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "succeeded", "failed"]},
                "provider_status": {"type": "string"},
                "amount_minor": {"type": "integer"},
                "refunded_total_minor": {"type": "integer"},
                "order_total_minor": {"type": "integer"},
                "currency": {"type": "string"},
                "livemode": {"type": "boolean"}
            }
        });
        let admin_stats_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["total_products", "active_products", "total_purchases", "currency_analytics", "total_groups"],
            "properties": {
                "total_products": {"type": "integer"},
                "active_products": {"type": "integer"},
                "total_purchases": {"type": "integer"},
                "currency_analytics": {"type": "array", "items": commerce_analytics_schema},
                "total_groups": {"type": "integer"}
            }
        });
        let stripe_connection_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["state", "configured", "livemode", "account_id", "country", "default_currency", "business_name", "charges_enabled", "payouts_enabled", "details_submitted", "capabilities", "publishable_key_configured", "webhook_secret_configured", "api_version", "error"],
            "properties": {
                "state": {"type": "string", "enum": ["not_configured", "connected_test", "connected_live", "misconfigured"]},
                "configured": {"type": "boolean"},
                "livemode": {"type": "boolean"},
                "account_id": {"type": "string"},
                "country": {"type": "string"},
                "default_currency": {"type": "string"},
                "business_name": {"type": "string"},
                "charges_enabled": {"type": "boolean"},
                "payouts_enabled": {"type": "boolean"},
                "details_submitted": {"type": "boolean"},
                "capabilities": {"type": "object", "additionalProperties": {"type": "string"}},
                "publishable_key_configured": {"type": "boolean"},
                "webhook_secret_configured": {"type": "boolean"},
                "api_version": {"type": "string"},
                "error": {"type": "string"}
            }
        });
        let webhook_event_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id", "event_type", "status", "stripe_account_id", "livemode", "attempts", "last_error", "created_at", "updated_at"],
            "properties": {
                "id": {"type": "string"},
                "event_type": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "processing", "failed", "processed", "dead_letter"]},
                "stripe_account_id": {"type": "string"},
                "livemode": {"type": "boolean"},
                "attempts": {"type": "integer"},
                "processing_started_at": {"type": "string", "format": "date-time"},
                "next_retry_at": {"type": "string", "format": "date-time"},
                "last_error": {"type": "string"},
                "processed_at": {"type": "string", "format": "date-time"},
                "terminal_at": {"type": "string", "format": "date-time"},
                "created_at": {"type": "string", "format": "date-time"},
                "updated_at": {"type": "string", "format": "date-time"}
            }
        });
        let webhook_event_list_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["records", "total_count", "page", "page_size"],
            "properties": {
                "records": {"type": "array", "items": webhook_event_schema},
                "total_count": {"type": "integer"},
                "page": {"type": "integer"},
                "page_size": {"type": "integer"}
            }
        });
        let webhook_ack_schema = serde_json::json!({
            "type": "object",
            "required": ["received"],
            "properties": {
                "received": {"type": "boolean"},
                "duplicate": {"type": "boolean"},
                "dead_letter": {"type": "boolean"}
            }
        });
        let provider_operation_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id", "operation_type", "aggregate_type", "aggregate_id", "stripe_account_id", "status", "attempts", "last_error", "created_at", "updated_at"],
            "properties": {
                "id": {"type": "string"},
                "operation_type": {"type": "string", "enum": ["refund.reconcile"]},
                "aggregate_type": {"type": "string", "enum": ["refund"]},
                "aggregate_id": {"type": "string"},
                "stripe_account_id": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "processing", "failed", "succeeded", "dead_letter"]},
                "attempts": {"type": "integer"},
                "processing_started_at": {"type": "string", "format": "date-time"},
                "next_attempt_at": {"type": "string", "format": "date-time"},
                "last_error": {"type": "string"},
                "completed_at": {"type": "string", "format": "date-time"},
                "terminal_at": {"type": "string", "format": "date-time"},
                "created_at": {"type": "string", "format": "date-time"},
                "updated_at": {"type": "string", "format": "date-time"}
            }
        });
        let provider_operation_list_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["records", "total_count", "page", "page_size"],
            "properties": {
                "records": {"type": "array", "items": provider_operation_schema},
                "total_count": {"type": "integer"},
                "page": {"type": "integer"},
                "page_size": {"type": "integer"}
            }
        });
        let provider_reconcile_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["claimed", "succeeded", "retry_scheduled", "dead_letter"],
            "properties": {
                "claimed": {"type": "integer"},
                "succeeded": {"type": "integer"},
                "retry_scheduled": {"type": "integer"},
                "dead_letter": {"type": "integer"}
            }
        });
        let deleted_schema = serde_json::json!({
            "type": "object",
            "required": ["deleted"],
            "properties": {"deleted": {"type": "boolean"}}
        });
        let product_write_schema = serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "slug": {"type": "string"},
                "currency": {"type": "string"},
                "status": {"type": "string", "enum": ["draft", "pending_review", "active", "archived"]},
                "category": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "metadata": {"type": "object"},
                "image_url": {"type": "string"},
                "stock": {"type": "integer"},
                "group_id": {"type": "string"},
                "type_id": {"type": "string"},
                "group_template_id": {"type": "string"},
                "product_template_id": {"type": "string"},
                "fulfillment_kind": {"type": "string", "enum": ["none", "manual", "download", "entitlement", "webhook"]}
            }
        });
        let mut product_update_schema = product_write_schema.clone();
        product_update_schema
            .as_object_mut()
            .expect("product schema is an object")
            .remove("required");
        let product_id_path_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["product_id"],
            "properties": {"product_id": {"type": "string"}}
        });
        let offer_path_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["product_id", "offer_id"],
            "properties": {
                "product_id": {"type": "string"},
                "offer_id": {"type": "string"}
            }
        });
        let preset_path_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["product_id", "offer_id", "preset_id"],
            "properties": {
                "product_id": {"type": "string"},
                "offer_id": {"type": "string"},
                "preset_id": {"type": "string"}
            }
        });
        let link_path_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["product_id", "offer_id", "link_id"],
            "properties": {
                "product_id": {"type": "string"},
                "offer_id": {"type": "string"},
                "link_id": {"type": "string"}
            }
        });
        let offer_definition_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["name", "mode", "currency", "pricing_model", "usage_type", "billing_scheme", "tax_behavior", "components"],
            "properties": {
                "name": {"type": "string"},
                "mode": {"type": "string", "enum": ["payment", "subscription"]},
                "currency": {"type": "string"},
                "pricing_model": {"type": "string", "enum": ["fixed", "components"]},
                "recurring_interval": {"type": ["string", "null"], "enum": ["day", "week", "month", "year", null]},
                "interval_count": {"type": "integer", "minimum": 1, "default": 1},
                "usage_type": {"type": "string", "enum": ["licensed", "metered"]},
                "billing_scheme": {"type": "string", "enum": ["per_unit", "tiered"]},
                "tax_behavior": {"type": "string", "enum": ["unspecified", "inclusive", "exclusive"]},
                "variables": {"type": "array", "items": {"type": "object"}},
                "components": {"type": "array", "items": {"type": "object"}},
                "checkout": {"type": "object"}
            }
        });
        let managed_offer_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["status", "sync_status", "sync_error", "offer"],
            "properties": {
                "status": {"type": "string", "enum": ["draft", "active", "archived"]},
                "sync_status": {"type": "string"},
                "sync_error": {"type": "string"},
                "offer": {
                    "type": "object",
                    "required": ["id", "product_id", "version", "name", "mode", "currency", "pricing_model", "interval_count", "usage_type", "billing_scheme", "tax_behavior", "variables", "components", "checkout", "stripe_product_id", "stripe_price_id"],
                    "properties": {
                        "id": {"type": "string"},
                        "product_id": {"type": "string"},
                        "version": {"type": "integer"},
                        "name": {"type": "string"},
                        "mode": {"type": "string", "enum": ["payment", "subscription"]},
                        "currency": {"type": "string"},
                        "pricing_model": {"type": "string", "enum": ["fixed", "components"]},
                        "recurring_interval": {"type": ["string", "null"]},
                        "interval_count": {"type": "integer"},
                        "usage_type": {"type": "string"},
                        "billing_scheme": {"type": "string"},
                        "tax_behavior": {"type": "string"},
                        "variables": {"type": "array", "items": {"type": "object"}},
                        "components": {"type": "array", "items": {"type": "object"}},
                        "checkout": {"type": "object"},
                        "stripe_product_id": {"type": "string"},
                        "stripe_price_id": {"type": "string"}
                    }
                }
            }
        });
        let offer_list_schema = serde_json::json!({
            "type": "object",
            "required": ["offers"],
            "properties": {"offers": {"type": "array", "items": managed_offer_schema}}
        });
        let product_duplicate_schema = serde_json::json!({
            "type": "object",
            "required": ["product", "offers"],
            "properties": {
                "product": record_schema(product_schema.clone()),
                "offers": {"type": "array", "items": managed_offer_schema}
            }
        });
        let checkout_preset_input_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["name"],
            "properties": {
                "name": {"type": "string"},
                "slug": {"type": "string"},
                "inputs": {"type": "object"}
            }
        });
        let checkout_preset_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id", "offer_id", "name", "slug", "inputs", "active", "configuration_hash"],
            "properties": {
                "id": {"type": "string"},
                "offer_id": {"type": "string"},
                "name": {"type": "string"},
                "slug": {"type": "string"},
                "inputs": {"type": "object"},
                "active": {"type": "boolean"},
                "configuration_hash": {"type": "string"}
            }
        });
        let checkout_preset_list_schema = serde_json::json!({
            "type": "object",
            "required": ["presets"],
            "properties": {"presets": {"type": "array", "items": checkout_preset_schema}}
        });
        let payment_link_input_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "preset_id": {"type": ["string", "null"]},
                "after_completion_url": {"type": ["string", "null"], "format": "uri"}
            }
        });
        let managed_payment_link_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["id", "offer_id", "preset_id", "url", "active", "configuration_hash", "sync_status", "sync_error"],
            "properties": {
                "id": {"type": "string"},
                "offer_id": {"type": "string"},
                "preset_id": {"type": "string"},
                "url": {"type": "string", "format": "uri"},
                "active": {"type": "boolean"},
                "configuration_hash": {"type": "string"},
                "sync_status": {"type": "string"},
                "sync_error": {"type": "string"}
            }
        });
        let payment_link_list_schema = serde_json::json!({
            "type": "object",
            "required": ["payment_links"],
            "properties": {"payment_links": {"type": "array", "items": managed_payment_link_schema}}
        });
        let group_write_schema = serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "group_template_id": {"type": "string"},
                "user_id": {"type": "string"},
                "status": {"type": "string"}
            }
        });
        let mut group_update_schema = group_write_schema.clone();
        group_update_schema
            .as_object_mut()
            .expect("group schema is an object")
            .remove("required");
        let product_type_write_schema = serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "is_system": {"type": "integer", "enum": [0, 1]}
            }
        });

        BlockInfo::new("impresspress/products", "0.0.1", "http-handler@v1", "Products, pricing, purchases, and payment integration")
            .instance_mode(InstanceMode::Singleton)
            .requires(vec!["wafer-run/database".into(), "wafer-run/config".into(), "wafer-run/network".into()])
            // Advisory table list — admin "Database tables" discovery + the
            // WRAP grant-UI read only `CollectionSchema::name`. The schema
            // itself (columns, indexes, FKs) lives solely in the block's
            // hand-authored `migrations/*.sqlite.sql` files (the single
            // source for both runtime `migrations::apply()` and the
            // Cloudflare D1 build).
            .collections(vec![
                CollectionSchema::new(PRODUCTS_TABLE),
                CollectionSchema::new(GROUPS_TABLE),
                CollectionSchema::new(TYPES_TABLE),
                CollectionSchema::new(PURCHASES_TABLE),
                CollectionSchema::new(LINE_ITEMS_TABLE),
                CollectionSchema::new(GROUP_TEMPLATES_TABLE),
                CollectionSchema::new(PRODUCT_TEMPLATES_TABLE),
                CollectionSchema::new(VARIABLES_TABLE),
                CollectionSchema::new(repo::subscriptions::SUBSCRIPTIONS_TABLE),
                CollectionSchema::new(repo::product_versions::TABLE),
                CollectionSchema::new(repo::offers::TABLE),
                CollectionSchema::new(repo::offer_components::TABLE),
                CollectionSchema::new(repo::checkout_presets::TABLE),
                CollectionSchema::new(repo::payment_links::TABLE),
                CollectionSchema::new(repo::seller_accounts::TABLE),
                CollectionSchema::new(repo::subscription_items::TABLE),
                CollectionSchema::new(repo::entitlements::TABLE),
                CollectionSchema::new(repo::provider_operations::TABLE),
                CollectionSchema::new(repo::refunds::TABLE),
                CollectionSchema::new(repo::disputes::TABLE),
            ])
            .category(wafer_run::BlockCategory::Feature)
            .description("Product catalog and offer-based commerce. Manages typed customer inputs, itemized pricing, orders, sellers, and Stripe checkout for one-time and recurring products.")
            // Declared in full so the central router enforces each tier from
            // the declared `AuthLevel` — the block dropped its in-handler
            // `is_admin` preambles, so any admin path NOT declared here would
            // silently fall back to the Public prefix tier (a regression). All
            // `/b/products/admin/*` SSR pages and `/b/products/api/admin/*`
            // JSON routes are `Admin`; the public catalog stays Public; the
            // user-facing purchase/checkout/subscription routes are
            // `Authenticated`.
            .endpoints(vec![
                // Authenticated commerce portal pages. Declare both root
                // forms because endpoint matching is trailing-slash aware.
                BlockEndpoint::get("/b/products")
                    .summary("Commerce portal")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/")
                    .summary("Commerce portal")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/my-products")
                    .summary("Manage own products")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/my-products/new")
                    .summary("Create own product")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/my-products/{id}")
                    .summary("Manage own product")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/my-purchases")
                    .summary("View own purchases")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/my-purchases/{id}")
                    .summary("View own purchase detail")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/selling")
                    .summary("Seller dashboard")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/selling/orders")
                    .summary("Seller orders")
                    .auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/products/selling/orders/{id}")
                    .summary("Seller order detail")
                    .auth(AuthLevel::Authenticated),
                // SSR admin pages.
                //
                // The overview is served by `handle()` for BOTH the canonical
                // slash form (`/b/products/admin/`, the `admin_url`) and the
                // bare no-slash form (`/b/products/admin`) via its
                // `"" | "/" => overview` dispatch arm. The central router's
                // matcher is trailing-slash-significant, so BOTH forms must be
                // declared `Admin` — declaring only the slash form would leave
                // the no-slash form governed solely by the Public `/b/products`
                // prefix tier, letting an anonymous request reach the admin
                // overview (the dispatch table and the declared surface must
                // agree on every path the block actually answers).
                BlockEndpoint::get("/b/products/admin").summary("Overview").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/").summary("Overview").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/manage").summary("Manage products").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/new").summary("Create product").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/products/{id}").summary("Manage product").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/groups").summary("Manage groups").auth(AuthLevel::Admin),

                BlockEndpoint::get("/b/products/admin/purchases").summary("Purchases").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/purchases/{id}").summary("Purchase detail").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/sellers").summary("Seller governance and moderation").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/sellers/{id}").summary("Seller capability and product detail").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/stripe").summary("Stripe setup").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/products/admin/settings").summary("Product settings").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/products/admin/settings").summary("Save product settings").auth(AuthLevel::Admin),
                // JSON admin API — products
                BlockEndpoint::get("/b/products/api/admin/products")
                    .summary("List products")
                    .auth(AuthLevel::Admin)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "page": {"type": "integer", "minimum": 1},
                            "page_size": {"type": "integer", "minimum": 1},
                            "group_id": {"type": "string"},
                            "status": {"type": "string"},
                            "search": {"type": "string"}
                        }
                    }))
                    .output_schema(record_list_schema(product_schema.clone()))
                    .tags(&["products", "admin"]),
                BlockEndpoint::post("/b/products/api/admin/products")
                    .summary("Create product")
                    .auth(AuthLevel::Admin)
                    .input_schema(product_write_schema.clone())
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "admin"]),
                BlockEndpoint::get("/b/products/api/admin/products/{id}")
                    .summary("Get product")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "admin"]),
                BlockEndpoint::patch("/b/products/api/admin/products/{id}")
                    .summary("Update product")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .input_schema(product_update_schema.clone())
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "admin"]),
                BlockEndpoint::delete("/b/products/api/admin/products/{id}")
                    .summary("Delete product")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(deleted_schema.clone())
                    .tags(&["products", "admin"]),
                BlockEndpoint::post("/b/products/api/admin/products/{id}/duplicate")
                    .summary("Duplicate product and editable offers")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(product_duplicate_schema.clone())
                    .tags(&["products", "admin"]),
                BlockEndpoint::post("/b/products/api/admin/products/{id}/approve")
                    .summary("Approve a seller product waiting for moderation")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "admin", "moderation"]),
                BlockEndpoint::post("/b/products/api/admin/products/{id}/reject")
                    .summary("Return a seller product to draft after moderation")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "admin", "moderation"]),
                BlockEndpoint::get("/b/products/api/admin/products/{product_id}/offers")
                    .summary("List product offers")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(product_id_path_schema.clone())
                    .output_schema(offer_list_schema.clone())
                    .tags(&["products", "admin", "offers"]),
                BlockEndpoint::post("/b/products/api/admin/products/{product_id}/offers")
                    .summary("Create product offer")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(product_id_path_schema.clone())
                    .input_schema(offer_definition_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "admin", "offers"]),
                BlockEndpoint::get("/b/products/api/admin/products/{product_id}/offers/{offer_id}")
                    .summary("Get product offer")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "admin", "offers"]),
                BlockEndpoint::post("/b/products/api/admin/products/{product_id}/offers/{offer_id}/preview")
                    .summary("Preview draft or active product offer")
                    .description("Evaluate an owner-visible immutable or draft offer with the server pricing engine. Browser totals are never trusted.")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .input_schema(pricing_preview_input_schema.clone())
                    .output_schema(pricing_preview_output_schema.clone())
                    .tags(&["products", "admin", "offers", "pricing"]),
                BlockEndpoint::patch("/b/products/api/admin/products/{product_id}/offers/{offer_id}")
                    .summary("Update draft offer")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .input_schema(offer_definition_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "admin", "offers"]),
                BlockEndpoint::post("/b/products/api/admin/products/{product_id}/offers/{offer_id}/publish")
                    .summary("Publish offer")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "admin", "offers"]),
                BlockEndpoint::post("/b/products/api/admin/products/{product_id}/offers/{offer_id}/sync")
                    .summary("Synchronize immutable Product and fixed Prices to Stripe")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "admin", "offers", "stripe"]),
                BlockEndpoint::post("/b/products/api/admin/products/{product_id}/offers/{offer_id}/duplicate")
                    .summary("Duplicate offer")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "admin", "offers"]),
                BlockEndpoint::delete("/b/products/api/admin/products/{product_id}/offers/{offer_id}")
                    .summary("Archive offer")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "admin", "offers"]),
                BlockEndpoint::get("/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets")
                    .summary("List checkout presets")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(checkout_preset_list_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links"]),
                BlockEndpoint::post("/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets")
                    .summary("Create checkout preset")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .input_schema(checkout_preset_input_schema.clone())
                    .output_schema(checkout_preset_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links"]),
                BlockEndpoint::get("/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}")
                    .summary("Get checkout preset")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(preset_path_schema.clone())
                    .output_schema(checkout_preset_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links"]),
                BlockEndpoint::patch("/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}")
                    .summary("Update checkout preset")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(preset_path_schema.clone())
                    .input_schema(checkout_preset_input_schema.clone())
                    .output_schema(checkout_preset_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links"]),
                BlockEndpoint::delete("/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}")
                    .summary("Archive checkout preset")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(preset_path_schema.clone())
                    .output_schema(checkout_preset_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links"]),
                BlockEndpoint::get("/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links")
                    .summary("List Payment Links")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(payment_link_list_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links", "stripe"]),
                BlockEndpoint::post("/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links")
                    .summary("Create or reuse Payment Link")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(offer_path_schema.clone())
                    .input_schema(payment_link_input_schema.clone())
                    .output_schema(managed_payment_link_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links", "stripe"]),
                BlockEndpoint::delete("/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links/{link_id}")
                    .summary("Deactivate Payment Link")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(link_path_schema.clone())
                    .output_schema(managed_payment_link_schema.clone())
                    .tags(&["products", "admin", "offers", "payment-links", "stripe"]),
                // JSON admin API — groups
                BlockEndpoint::get("/b/products/api/admin/groups")
                    .summary("List groups")
                    .auth(AuthLevel::Admin)
                    .output_schema(record_list_schema(group_schema.clone()))
                    .tags(&["products", "admin", "groups"]),
                BlockEndpoint::post("/b/products/api/admin/groups")
                    .summary("Create group")
                    .auth(AuthLevel::Admin)
                    .input_schema(group_write_schema)
                    .output_schema(record_schema(group_schema.clone()))
                    .tags(&["products", "admin", "groups"]),
                BlockEndpoint::patch("/b/products/api/admin/groups/{id}")
                    .summary("Update group")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .input_schema(group_update_schema.clone())
                    .output_schema(record_schema(group_schema.clone()))
                    .tags(&["products", "admin", "groups"]),
                BlockEndpoint::delete("/b/products/api/admin/groups/{id}")
                    .summary("Delete group")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(deleted_schema.clone())
                    .tags(&["products", "admin", "groups"]),
                // JSON admin API — types
                BlockEndpoint::get("/b/products/api/admin/types")
                    .summary("List types")
                    .auth(AuthLevel::Admin)
                    .output_schema(record_list_schema(product_type_schema.clone()))
                    .tags(&["products", "admin", "types"]),
                BlockEndpoint::post("/b/products/api/admin/types")
                    .summary("Create type")
                    .auth(AuthLevel::Admin)
                    .input_schema(product_type_write_schema)
                    .output_schema(record_schema(product_type_schema.clone()))
                    .tags(&["products", "admin", "types"]),
                BlockEndpoint::delete("/b/products/api/admin/types/{id}")
                    .summary("Delete type")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(deleted_schema.clone())
                    .tags(&["products", "admin", "types"]),
                // JSON admin API — purchases + stats
                BlockEndpoint::get("/b/products/api/admin/purchases")
                    .summary("List purchases")
                    .auth(AuthLevel::Admin)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "page": {"type": "integer", "minimum": 1},
                            "page_size": {"type": "integer", "minimum": 1},
                            "status": {"type": "string"},
                            "user_id": {"type": "string"}
                        }
                    }))
                    .output_schema(purchase_list_schema.clone())
                    .tags(&["products", "admin", "orders"]),
                BlockEndpoint::get("/b/products/api/admin/purchases/{id}")
                    .summary("Get purchase")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(purchase_detail_schema.clone())
                    .tags(&["products", "admin", "orders"]),
                BlockEndpoint::post("/b/products/api/admin/purchases/{id}/refund")
                    .summary("Create an idempotent full or partial refund")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .input_schema(refund_input_schema.clone())
                    .output_schema(refund_output_schema.clone())
                    .tags(&["products", "admin", "refunds"]),
                BlockEndpoint::get("/b/products/api/admin/stats")
                    .summary("Commerce analytics separated by currency")
                    .auth(AuthLevel::Admin)
                    .output_schema(admin_stats_schema)
                    .tags(&["products", "admin", "analytics"]),
                BlockEndpoint::get("/b/products/api/admin/stripe/status")
                    .summary("Validate Stripe connection and account mode")
                    .auth(AuthLevel::Admin)
                    .output_schema(stripe_connection_schema)
                    .tags(&["products", "admin", "stripe"]),
                BlockEndpoint::get("/b/products/api/admin/webhook-events")
                    .summary("List safe Stripe webhook processing state")
                    .auth(AuthLevel::Admin)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "status": {"type": "string", "enum": ["pending", "processing", "failed", "processed", "dead_letter"]},
                            "page": {"type": "integer", "minimum": 1},
                            "page_size": {"type": "integer", "minimum": 1, "maximum": 100}
                        }
                    }))
                    .output_schema(webhook_event_list_schema)
                    .tags(&["products", "admin", "stripe", "webhooks"]),
                BlockEndpoint::post("/b/products/api/admin/webhook-events/{id}/replay")
                    .summary("Replay a failed or dead-letter Stripe webhook")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(webhook_ack_schema)
                    .tags(&["products", "admin", "stripe", "webhooks"]),
                BlockEndpoint::get("/b/products/api/admin/provider-operations")
                    .summary("List safe Stripe provider reconciliation state")
                    .auth(AuthLevel::Admin)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "status": {"type": "string", "enum": ["pending", "processing", "failed", "succeeded", "dead_letter"]},
                            "page": {"type": "integer", "minimum": 1},
                            "page_size": {"type": "integer", "minimum": 1, "maximum": 100}
                        }
                    }))
                    .output_schema(provider_operation_list_schema)
                    .tags(&["products", "admin", "stripe", "reconciliation"]),
                BlockEndpoint::post("/b/products/api/admin/provider-operations/reconcile")
                    .summary("Claim and reconcile due Stripe provider operations")
                    .description("Safe for an authenticated scheduler or manual administrator recovery action; leases and original Stripe idempotency keys prevent duplicate mutations.")
                    .auth(AuthLevel::Admin)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {"limit": {"type": "integer", "minimum": 1, "maximum": 100}}
                    }))
                    .output_schema(provider_reconcile_schema)
                    .tags(&["products", "admin", "stripe", "reconciliation"]),
                BlockEndpoint::get("/b/products/api/admin/sellers")
                    .summary("List seller accounts and capability state")
                    .auth(AuthLevel::Admin)
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "required": ["sellers"],
                        "properties": {"sellers": {"type": "array", "items": seller_account_schema}}
                    }))
                    .tags(&["products", "admin", "seller", "stripe-connect"]),
                BlockEndpoint::get("/b/products/api/admin/sellers/{id}")
                    .summary("Get seller account and owned products")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "required": ["seller", "products"],
                        "properties": {
                            "seller": seller_account_schema,
                            "products": {"type": "array", "items": record_schema(product_schema.clone())}
                        }
                    }))
                    .tags(&["products", "admin", "seller", "stripe-connect"]),
                BlockEndpoint::post("/b/products/api/admin/sellers/{id}/suspend")
                    .summary("Suspend a seller after provider-safe offer archival")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(seller_account_schema.clone())
                    .tags(&["products", "admin", "seller", "stripe-connect"]),
                BlockEndpoint::post("/b/products/api/admin/sellers/{id}/reactivate")
                    .summary("Reactivate a seller for onboarding or sales")
                    .auth(AuthLevel::Admin)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(seller_account_schema.clone())
                    .tags(&["products", "admin", "seller", "stripe-connect"]),
                BlockEndpoint::get("/b/products/api/products")
                    .summary("List own products")
                    .auth(AuthLevel::Authenticated)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "page": {"type": "integer", "minimum": 1},
                            "page_size": {"type": "integer", "minimum": 1},
                            "group_id": {"type": "string"},
                            "status": {"type": "string"},
                            "search": {"type": "string"}
                        }
                    }))
                    .output_schema(record_list_schema(product_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::post("/b/products/api/products")
                    .summary("Create own product")
                    .auth(AuthLevel::Authenticated)
                    .input_schema(product_write_schema)
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::get("/b/products/api/products/{id}")
                    .summary("Get own product")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::patch("/b/products/api/products/{id}")
                    .summary("Update own product")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .input_schema(product_update_schema.clone())
                    .output_schema(record_schema(product_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::delete("/b/products/api/products/{id}")
                    .summary("Delete own product")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(deleted_schema)
                    .tags(&["products", "seller"]),
                BlockEndpoint::post("/b/products/api/products/{id}/duplicate")
                    .summary("Duplicate own product and editable offers")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(product_duplicate_schema)
                    .tags(&["products", "seller"]),
                BlockEndpoint::get("/b/products/api/products/{product_id}/offers")
                    .summary("List own product offers")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(product_id_path_schema.clone())
                    .output_schema(offer_list_schema)
                    .tags(&["products", "seller", "offers"]),
                BlockEndpoint::post("/b/products/api/products/{product_id}/offers")
                    .summary("Create own product offer")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(product_id_path_schema)
                    .input_schema(offer_definition_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "seller", "offers"]),
                BlockEndpoint::get("/b/products/api/products/{product_id}/offers/{offer_id}")
                    .summary("Get own product offer")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "seller", "offers"]),
                BlockEndpoint::post("/b/products/api/products/{product_id}/offers/{offer_id}/preview")
                    .summary("Preview own draft or active offer")
                    .description("Evaluate an owned immutable or draft offer with the server pricing engine. Browser totals are never trusted.")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .input_schema(pricing_preview_input_schema.clone())
                    .output_schema(pricing_preview_output_schema.clone())
                    .tags(&["products", "seller", "offers", "pricing"]),
                BlockEndpoint::patch("/b/products/api/products/{product_id}/offers/{offer_id}")
                    .summary("Update own draft offer")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .input_schema(offer_definition_schema)
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "seller", "offers"]),
                BlockEndpoint::post("/b/products/api/products/{product_id}/offers/{offer_id}/publish")
                    .summary("Publish own offer")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "seller", "offers"]),
                BlockEndpoint::post("/b/products/api/products/{product_id}/offers/{offer_id}/sync")
                    .summary("Synchronize own immutable Product and fixed Prices to Stripe")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "seller", "offers", "stripe"]),
                BlockEndpoint::post("/b/products/api/products/{product_id}/offers/{offer_id}/duplicate")
                    .summary("Duplicate own offer")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema.clone())
                    .tags(&["products", "seller", "offers"]),
                BlockEndpoint::delete("/b/products/api/products/{product_id}/offers/{offer_id}")
                    .summary("Archive own offer")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(managed_offer_schema)
                    .tags(&["products", "seller", "offers"]),
                BlockEndpoint::get("/b/products/api/products/{product_id}/offers/{offer_id}/presets")
                    .summary("List own checkout presets")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(checkout_preset_list_schema)
                    .tags(&["products", "seller", "offers", "payment-links"]),
                BlockEndpoint::post("/b/products/api/products/{product_id}/offers/{offer_id}/presets")
                    .summary("Create own checkout preset")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .input_schema(checkout_preset_input_schema.clone())
                    .output_schema(checkout_preset_schema.clone())
                    .tags(&["products", "seller", "offers", "payment-links"]),
                BlockEndpoint::get("/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}")
                    .summary("Get own checkout preset")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(preset_path_schema.clone())
                    .output_schema(checkout_preset_schema.clone())
                    .tags(&["products", "seller", "offers", "payment-links"]),
                BlockEndpoint::patch("/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}")
                    .summary("Update own checkout preset")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(preset_path_schema.clone())
                    .input_schema(checkout_preset_input_schema)
                    .output_schema(checkout_preset_schema.clone())
                    .tags(&["products", "seller", "offers", "payment-links"]),
                BlockEndpoint::delete("/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}")
                    .summary("Archive own checkout preset")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(preset_path_schema)
                    .output_schema(checkout_preset_schema)
                    .tags(&["products", "seller", "offers", "payment-links"]),
                BlockEndpoint::get("/b/products/api/products/{product_id}/offers/{offer_id}/payment-links")
                    .summary("List own Payment Links")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema.clone())
                    .output_schema(payment_link_list_schema)
                    .tags(&["products", "seller", "offers", "payment-links", "stripe"]),
                BlockEndpoint::post("/b/products/api/products/{product_id}/offers/{offer_id}/payment-links")
                    .summary("Create or reuse own Payment Link")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(offer_path_schema)
                    .input_schema(payment_link_input_schema)
                    .output_schema(managed_payment_link_schema.clone())
                    .tags(&["products", "seller", "offers", "payment-links", "stripe"]),
                BlockEndpoint::delete("/b/products/api/products/{product_id}/offers/{offer_id}/payment-links/{link_id}")
                    .summary("Deactivate own Payment Link")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(link_path_schema)
                    .output_schema(managed_payment_link_schema)
                    .tags(&["products", "seller", "offers", "payment-links", "stripe"]),
                // Authenticated user-owned groups and builder taxonomy. These
                // routes used to rely on the products prefix's fail-closed
                // fallback, which protected them but omitted them from
                // discovery and made dispatch/declaration drift invisible.
                BlockEndpoint::get("/b/products/groups")
                    .summary("List own product groups")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(record_list_schema(group_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::post("/b/products/groups")
                    .summary("Create own product group")
                    .auth(AuthLevel::Authenticated)
                    .input_schema(serde_json::json!({
                        "type": "object",
                        "required": ["name"],
                        "properties": {
                            "name": {"type": "string"},
                            "description": {"type": "string"},
                                        "group_template_id": {"type": "string"},
                            "status": {"type": "string"}
                        }
                    }))
                    .output_schema(record_schema(group_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::get("/b/products/groups/{id}")
                    .summary("Get own product group")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(record_schema(group_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::patch("/b/products/groups/{id}")
                    .summary("Update own product group")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .input_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "description": {"type": "string"},
                                        "group_template_id": {"type": "string"},
                            "status": {"type": "string"}
                        }
                    }))
                    .output_schema(record_schema(group_schema))
                    .tags(&["products", "seller"]),
                BlockEndpoint::delete("/b/products/groups/{id}")
                    .summary("Delete own product group")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "required": ["deleted"],
                        "properties": {"deleted": {"type": "boolean"}}
                    }))
                    .tags(&["products", "seller"]),
                BlockEndpoint::get("/b/products/groups/{id}/products")
                    .summary("List products in own group")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(record_list_schema(product_schema.clone()))
                    .tags(&["products", "seller"]),
                BlockEndpoint::get("/b/products/types")
                    .summary("List product types for the authenticated builder")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(record_list_schema(product_type_schema))
                    .tags(&["products", "seller"]),
                BlockEndpoint::get("/b/products/group-templates")
                    .summary("List group templates for the authenticated builder")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(record_list_schema(group_template_schema))
                    .tags(&["products", "seller"]),
                BlockEndpoint::get("/b/products/api/seller/account")
                    .summary("Seller Stripe account status")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(seller_account_schema.clone())
                    .tags(&["products", "seller", "stripe-connect"]),
                BlockEndpoint::get("/b/products/api/seller/stats")
                    .summary("Seller analytics separated by currency")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(seller_stats_schema)
                    .tags(&["products", "seller", "analytics"]),
                BlockEndpoint::get("/b/products/api/seller/orders")
                    .summary("List seller-owned orders")
                    .auth(AuthLevel::Authenticated)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "page": {"type": "integer", "minimum": 1},
                            "page_size": {"type": "integer", "minimum": 1},
                            "status": {"type": "string"}
                        }
                    }))
                    .output_schema(purchase_list_schema.clone())
                    .tags(&["products", "seller", "orders"]),
                BlockEndpoint::get("/b/products/api/seller/orders/{id}")
                    .summary("Get seller-owned order")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .output_schema(purchase_detail_schema.clone())
                    .tags(&["products", "seller", "orders"]),
                BlockEndpoint::post("/b/products/api/seller/orders/{id}/refund")
                    .summary("Refund a seller-owned order")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema.clone())
                    .input_schema(refund_input_schema)
                    .output_schema(refund_output_schema)
                    .tags(&["products", "seller", "orders", "refunds"]),
                BlockEndpoint::post("/b/products/api/seller/onboarding")
                    .summary("Create seller account and Stripe-hosted onboarding link")
                    .auth(AuthLevel::Authenticated)
                    .input_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["return_url", "refresh_url"],
                        "properties": {
                            "return_url": {"type": "string", "format": "uri"},
                            "refresh_url": {"type": "string", "format": "uri"}
                        }
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["account", "url", "expires_at"],
                        "properties": {
                            "account": seller_account_schema,
                            "url": {"type": "string", "format": "uri"},
                            "expires_at": {"type": "integer"}
                        }
                    }))
                    .tags(&["products", "seller", "stripe-connect"]),
                BlockEndpoint::post("/b/products/api/seller/dashboard")
                    .summary("Create Stripe Express dashboard login link")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(provider_redirect_schema.clone())
                    .tags(&["products", "seller", "stripe-connect"]),
                // Public + authenticated user surface
                // Public catalog — highest-value developer-facing surface of
                // this block; accurate shapes read from `handlers.rs`
                // (`handle_catalog` → `crud::crud_list` → `RecordList`,
                // `handle_get_product_public` → `db::get` → `Record`). Full
                // schema coverage of the admin/purchase/checkout API is a
                // follow-up.
                BlockEndpoint::get("/b/products/catalog")
                    .summary("Browse catalog")
                    .description("Public list of active products, sorted by name.")
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "page": {"type": "integer", "default": 1},
                            "page_size": {"type": "integer", "default": 20, "maximum": 100}
                        }
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "records": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": {"type": "string"},
                                        "data": product_schema
                                    }
                                }
                            },
                            "total_count": {"type": "integer"},
                            "page": {"type": "integer"},
                            "page_size": {"type": "integer"}
                        }
                    }))
                    .tags(&["products"]),
                BlockEndpoint::get("/b/products/catalog/{id}")
                    .summary("Product detail")
                    .path_params_schema(serde_json::json!({
                        "type": "object",
                        "required": ["id"],
                        "properties": {
                            "id": {"type": "string"}
                        }
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "data": product_schema
                        }
                    }))
                    .tags(&["products"]),
                BlockEndpoint::get("/b/products/storefront.js")
                    .summary("Framework-free product storefront widget")
                    .description("Browser custom element for static sites. It loads only public product configuration and sends customer inputs to server-owned pricing and checkout endpoints.")
                    .auth(AuthLevel::Public)
                    .tags(&["products", "storefront"]),
                BlockEndpoint::get("/b/products/storefront/config")
                    .summary("Browser-safe storefront configuration")
                    .description("Returns only a validated Stripe publishable key and mode. Secret keys, webhook secrets, provider ids, and API URLs are never exposed.")
                    .auth(AuthLevel::Public)
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["schema_version", "embedded_checkout_available"],
                        "properties": {
                            "schema_version": {"type": "integer"},
                            "embedded_checkout_available": {"type": "boolean"},
                            "stripe_publishable_key": {"type": "string"},
                            "stripe_mode": {"type": "string", "enum": ["test", "live"]}
                        }
                    }))
                    .tags(&["products", "storefront"]),
                BlockEndpoint::get("/b/products/storefront/{product_id}")
                    .summary("Storefront product and offers")
                    .description("Safe public product detail with active offer summaries and public pricing inputs; internal ownership, provider, and pricing-rule fields are omitted.")
                    .path_params_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["product_id"],
                        "properties": {
                            "product_id": {"type": "string"}
                        }
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["schema_version", "id", "name", "offers"],
                        "properties": {
                            "schema_version": {"type": "integer"},
                            "id": {"type": "string"},
                            "name": {"type": "string"},
                            "slug": {"type": "string"},
                            "description": {"type": "string"},
                            "image_url": {"type": "string"},
                            "tags": {"type": "array", "items": {"type": "string"}},
                            "fulfillment_kind": {"type": "string"},
                            "offers": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["id", "version", "name", "mode", "currency", "variables"],
                                    "properties": {
                                        "id": {"type": "string"},
                                        "version": {"type": "integer"},
                                        "name": {"type": "string"},
                                        "mode": {"type": "string"},
                                        "currency": {"type": "string"},
                                        "pricing_model": {"type": "string"},
                                        "recurring_interval": {"type": ["string", "null"]},
                                        "interval_count": {"type": "integer"},
                                        "variables": {"type": "array"},
                                        "checkout": {"type": "object"},
                                        "payment_links": {"type": "array"}
                                    }
                                }
                            }
                        }
                    }))
                    .auth(AuthLevel::Public)
                    .tags(&["products", "storefront"]),
                BlockEndpoint::post("/b/products/webhooks")
                    .summary("Receive signed Stripe webhook events")
                    .description("Public transport endpoint authenticated by the Stripe-Signature HMAC header. Raw request bytes are verified before parsing or applying any side effect.")
                    .auth(AuthLevel::Public)
                    .input_schema(serde_json::json!({
                        "type": "object",
                        "required": ["type", "data"],
                        "properties": {
                            "id": {"type": "string"},
                            "type": {"type": "string"},
                            "account": {"type": "string"},
                            "livemode": {"type": "boolean"},
                            "data": {
                                "type": "object",
                                "required": ["object"],
                                "properties": {"object": {"type": "object"}}
                            }
                        },
                        "additionalProperties": true
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "required": ["received"],
                        "properties": {
                            "received": {"type": "boolean"},
                            "duplicate": {"type": "boolean"},
                            "dead_letter": {"type": "boolean"}
                        }
                    }))
                    .tags(&["products", "stripe", "webhooks"]),
                BlockEndpoint::post("/b/products/pricing/preview")
                    .summary("Preview configured offer")
                    .description("Evaluate a persisted active offer from validated customer inputs. Amounts are returned in integer minor units.")
                    .input_schema(pricing_preview_input_schema)
                    .output_schema(pricing_preview_output_schema)
                    .auth(AuthLevel::Public)
                    .tags(&["products", "pricing"]),
                BlockEndpoint::post("/b/products/checkout")
                    .summary("Stripe checkout")
                    .description("Create a hosted or embedded Stripe Checkout Session from a public active offer. Guest checkout is supported and every amount is resolved from the immutable offer.")
                    .input_schema(checkout_input_schema)
                    .output_schema(checkout_output_schema)
                    .auth(AuthLevel::Public)
                    .tags(&["products", "checkout"]),
                BlockEndpoint::get("/b/products/orders/{id}/status")
                    .summary("Guest checkout status")
                    .description("Returns a minimal order projection when supplied with the short-lived receipt capability issued at checkout. Buyer and provider identifiers are omitted.")
                    .auth(AuthLevel::Public)
                    .path_params_schema(serde_json::json!({
                        "type": "object",
                        "required": ["id"],
                        "properties": {"id": {"type": "string"}}
                    }))
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["receipt_token"],
                        "properties": {"receipt_token": {"type": "string"}}
                    }))
                    .output_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["schema_version", "order_id", "status", "reconciliation_status", "amounts", "subscription_cancel_at_period_end"],
                        "properties": {
                            "schema_version": {"type": "integer"},
                            "order_id": {"type": "string"},
                            "status": {"type": "string"},
                            "reconciliation_status": {"type": "string"},
                            "amounts": money_breakdown_schema,
                            "subscription_status": {"type": "string"},
                            "subscription_current_period_end": {"type": "string", "format": "date-time"},
                            "subscription_cancel_at_period_end": {"type": "boolean"},
                            "paid_at": {"type": "string", "format": "date-time"},
                            "refunded_at": {"type": "string", "format": "date-time"}
                        }
                    }))
                    .tags(&["products", "storefront"]),
                BlockEndpoint::get("/b/products/purchases")
                    .summary("List own purchases")
                    .auth(AuthLevel::Authenticated)
                    .query_params_schema(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "page": {"type": "integer", "minimum": 1},
                            "page_size": {"type": "integer", "minimum": 1}
                        }
                    }))
                    .output_schema(purchase_list_schema)
                    .tags(&["products", "orders"]),
                BlockEndpoint::get("/b/products/purchases/{id}")
                    .summary("Get own purchase")
                    .auth(AuthLevel::Authenticated)
                    .path_params_schema(id_path_schema)
                    .output_schema(purchase_detail_schema)
                    .tags(&["products", "orders"]),
                BlockEndpoint::get("/b/products/subscription")
                    .summary("Platform subscription status")
                    .auth(AuthLevel::Authenticated)
                    .output_schema(subscription_status_schema)
                    .tags(&["products", "subscriptions"]),
                BlockEndpoint::post("/b/products/billing-portal")
                    .summary("Create a Stripe Billing Portal session for an owned customer context")
                    .auth(AuthLevel::Authenticated)
                    .input_schema(serde_json::json!({
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["return_url"],
                        "properties": {
                            "return_url": {"type": "string", "format": "uri"},
                            "order_id": {"type": ["string", "null"]}
                        }
                    }))
                    .output_schema(provider_redirect_schema)
                    .tags(&["products", "subscriptions", "stripe"]),
            ])
            .config_keys(config_vars())
            .admin_url("/b/products/admin/")
            .can_disable(true)
    },
    handle: |this, ctx, msg, input| {
        let path = msg.path().to_string();
        let action = msg.action().to_string();

        // Settings save (POST to admin settings page). Admin tier enforced
        // centrally from the declared `POST /b/products/admin/settings`
        // endpoint — no in-handler `is_admin` re-check.
        if action == "create" && path == "/b/products/admin/settings" {
            return pages::handle_save_settings(ctx, input).await;
        }

        // SSR pages (GET requests to specific page paths)
        if action == "retrieve" && (path == "/b/products" || path.starts_with("/b/products/")) {
            let sub = path.strip_prefix("/b/products").unwrap_or("/");
            // Admin pages under /b/products/admin/... — Admin tier enforced
            // centrally from the declared `/b/products/admin/*` endpoints.
            if sub.starts_with("/admin") {
                let admin_sub = sub.strip_prefix("/admin").unwrap_or("/");
                return match admin_sub {
                    "" | "/" => pages::overview(ctx, &msg).await,
                    "/manage" => pages::manage_products(ctx, &msg).await,
                    "/new" => pages::product_wizard(ctx, &msg, true).await,
                    "/groups" => pages::groups(ctx, &msg).await,
                    "/purchases" => pages::purchases(ctx, &msg).await,
                    "/sellers" => pages::admin_sellers(ctx, &msg).await,
                    "/stripe" => pages::stripe_setup(ctx, &msg).await,
                    "/settings" => pages::settings(ctx, &msg).await,
                    _ => {
                        if let Some(purchase_id) = admin_sub.strip_prefix("/purchases/") {
                            if !purchase_id.is_empty() && !purchase_id.contains('/') {
                                return pages::admin_purchase_detail(ctx, &msg, purchase_id).await;
                            }
                        }
                        if let Some(seller_id) = admin_sub.strip_prefix("/sellers/") {
                            if !seller_id.is_empty() && !seller_id.contains('/') {
                                return pages::admin_seller_detail(ctx, &msg, seller_id).await;
                            }
                        }
                        if let Some(product_id) = admin_sub.strip_prefix("/products/") {
                            if !product_id.is_empty() && !product_id.contains('/') {
                                return pages::product_manager(ctx, &msg, product_id, true).await;
                            }
                        }
                        err_not_found("not found")
                    }
                };
            }
            // User-facing pages (require auth but not admin)
            match sub {
                "" | "/" => return pages::portal_home(ctx, &msg).await,
                "/my-products" => {
                    if !handlers::user_products_enabled(ctx).await {
                        return err_forbidden("User product selling is disabled");
                    }
                    return pages::my_products(ctx, &msg).await;
                }
                "/my-products/new" => {
                    if !handlers::user_products_enabled(ctx).await {
                        return err_forbidden("User product selling is disabled");
                    }
                    return pages::product_wizard(ctx, &msg, false).await;
                }
                "/selling" => {
                    if !handlers::user_products_enabled(ctx).await {
                        return err_forbidden("User product selling is disabled");
                    }
                    return pages::seller_dashboard(ctx, &msg).await;
                }
                "/selling/orders" => {
                    if !handlers::user_products_enabled(ctx).await {
                        return err_forbidden("User product selling is disabled");
                    }
                    return pages::seller_orders(ctx, &msg).await;
                }
                _ if sub.starts_with("/selling/orders/") => {
                    if !handlers::user_products_enabled(ctx).await {
                        return err_forbidden("User product selling is disabled");
                    }
                    let purchase_id = sub.strip_prefix("/selling/orders/").unwrap_or_default();
                    if !purchase_id.is_empty() && !purchase_id.contains('/') {
                        return pages::seller_order_detail(ctx, &msg, purchase_id).await;
                    }
                }
                _ if sub.starts_with("/my-products/") => {
                    if !handlers::user_products_enabled(ctx).await {
                        return err_forbidden("User product selling is disabled");
                    }
                    let product_id = sub.strip_prefix("/my-products/").unwrap_or_default();
                    if !product_id.is_empty() && !product_id.contains('/') {
                        return pages::product_manager(ctx, &msg, product_id, false).await;
                    }
                }
                "/my-purchases" => return pages::my_purchases(ctx, &msg).await,
                _ if sub.starts_with("/my-purchases/") => {
                    let purchase_id = sub.strip_prefix("/my-purchases/").unwrap_or_default();
                    if !purchase_id.is_empty() && !purchase_id.contains('/') {
                        return pages::my_purchase_detail(ctx, &msg, purchase_id).await;
                    }
                }
                _ => {} // fall through to API handlers
            }
        }

        // Webhook (no auth, no user rate limit)
        if path == "/b/products/webhooks" || path.starts_with("/b/products/webhooks/") {
            return stripe::handle_webhook(ctx, &msg, input).await;
        }

        // Guest pricing, checkout, and receipt polling use route-specific IP
        // buckets. Other endpoints retain the read/write per-user buckets.
        // Allowed(headers) is currently discarded because injecting headers
        // into a streaming response requires platform middleware.
        let matched_public_limit = match check_route_limits(
            &this.limiter,
            ctx,
            &msg,
            &action,
            &path,
            PUBLIC_RATE_LIMIT_ROUTES,
        )
        .await
        {
            Some(RateLimitOutcome::Limited(out)) => return out,
            Some(_) => true,
            None => false,
        };
        if !matched_public_limit {
            if let RateLimitOutcome::Limited(out) =
                check_user_rate_limit(&this.limiter, ctx, &msg).await
            {
                return out;
            }
        }

        // Admin API at /b/products/api/admin/... — dispatched against the
        // normalized `/admin/b/products/...` sub-path passed EXPLICITLY (no
        // `req.resource` rewrite). Admin tier enforced centrally from the
        // declared `/b/products/api/admin/*` endpoints; the in-block
        // `is_admin` preamble is gone.
        if let Some(rest) = path.strip_prefix("/b/products/api/admin") {
            let norm = format!("/admin/b/products{rest}");
            return handlers::handle_admin(ctx, &mut msg, &norm, input).await;
        }

        // User API at /b/products/api/... — normalized to /b/products/... and
        // passed explicitly.
        if let Some(rest) = path.strip_prefix("/b/products/api") {
            let norm = format!("/b/products{rest}");
            return handlers::handle_user(ctx, &mut msg, &norm, input).await;
        }

        // User endpoints at /b/products/... (catalog, checkout, subscription,
        // etc.) — the on-the-wire path is already normalized.
        if path.starts_with("/b/products/") || path == "/b/products" {
            return handlers::handle_user(ctx, &mut msg, &path, input).await;
        }

        err_not_found("not found")
    },
    lifecycle: |_this, ctx, event| {
        // Apply block-owned schema migrations. Migration 002 seeds the default
        // group/product templates (the static FK-parent rows the
        // groups/products tables require) via idempotent INSERTs, so there is
        // no per-request runtime existence-check + seed — the hash-gate
        // short-circuits in memory once applied.
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/products",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await
    },
}
