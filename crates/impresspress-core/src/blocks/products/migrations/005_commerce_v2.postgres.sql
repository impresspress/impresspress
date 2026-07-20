-- Commerce v2 foundation (PostgreSQL).
-- Mirror of 005_commerce_v2.sqlite.sql. BIGINT is used for minor-unit money;
-- INTEGER remains the project convention for JSON-backed boolean flags.

ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS owner_kind TEXT NOT NULL DEFAULT 'platform';
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS owner_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS seller_account_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS approval_status TEXT NOT NULL DEFAULT 'approved';
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS fulfillment_kind TEXT NOT NULL DEFAULT 'none';
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS stripe_product_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS current_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS submitted_at TEXT;
ALTER TABLE impresspress__products__products ADD COLUMN IF NOT EXISTS published_at TEXT;
CREATE INDEX IF NOT EXISTS impresspress__products__products_owner_idx ON impresspress__products__products (owner_kind, owner_id);
CREATE INDEX IF NOT EXISTS impresspress__products__products_seller_idx ON impresspress__products__products (seller_account_id);
CREATE INDEX IF NOT EXISTS impresspress__products__products_stripe_product_idx ON impresspress__products__products (seller_account_id, stripe_product_id);
CREATE INDEX IF NOT EXISTS impresspress__products__products_approval_idx ON impresspress__products__products (approval_status);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__products_owner_slug_uniq
    ON impresspress__products__products (owner_kind, owner_id, slug)
    WHERE slug <> '' AND deleted_at IS NULL;

ALTER TABLE impresspress__products__product_templates ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'one_time';
ALTER TABLE impresspress__products__product_templates ADD COLUMN IF NOT EXISTS schema_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE impresspress__products__product_templates ADD COLUMN IF NOT EXISTS schema_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE impresspress__products__product_templates ADD COLUMN IF NOT EXISTS config_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE impresspress__products__product_templates ADD COLUMN IF NOT EXISTS is_system INTEGER NOT NULL DEFAULT 0;
INSERT INTO impresspress__products__product_templates
    (id, name, display_name, kind, schema_version, schema_json, config_json, is_system, created_at, updated_at)
VALUES
    ('simple_product', 'simple_product', 'Simple product', 'one_time', 1, '{}', '{"pricing_model":"fixed"}', 1, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z'),
    ('simple_subscription', 'simple_subscription', 'Simple subscription', 'subscription', 1, '{}', '{"pricing_model":"fixed","interval":"month"}', 1, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z'),
    ('configurable_product', 'configurable_product', 'Configurable product', 'one_time', 1, '{}', '{"pricing_model":"components"}', 1, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z'),
    ('configurable_subscription', 'configurable_subscription', 'Configurable subscription', 'subscription', 1, '{}', '{"pricing_model":"components","interval":"month"}', 1, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')
ON CONFLICT (id) DO NOTHING;

ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS offer_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS label TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS help_text TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS required INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS minimum_value TEXT;
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS maximum_value TEXT;
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS step_value TEXT;
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS maximum_length INTEGER;
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS allowed_values TEXT NOT NULL DEFAULT '[]';
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'public';
ALTER TABLE impresspress__products__variables ADD COLUMN IF NOT EXISTS sort_order INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS impresspress__products__variables_offer_idx ON impresspress__products__variables (offer_id, sort_order);

CREATE TABLE IF NOT EXISTS impresspress__products__product_versions (
    id TEXT PRIMARY KEY,
    product_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    definition_json TEXT NOT NULL DEFAULT '{}',
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__product_versions_uniq ON impresspress__products__product_versions (product_id, version);

CREATE TABLE IF NOT EXISTS impresspress__products__offers (
    id TEXT PRIMARY KEY,
    product_id TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'draft',
    mode TEXT NOT NULL DEFAULT 'payment',
    currency TEXT NOT NULL DEFAULT 'USD',
    pricing_model TEXT NOT NULL DEFAULT 'fixed',
    unit_amount_minor BIGINT NOT NULL DEFAULT 0,
    recurring_interval TEXT NOT NULL DEFAULT '',
    interval_count INTEGER NOT NULL DEFAULT 1,
    usage_type TEXT NOT NULL DEFAULT 'licensed',
    billing_scheme TEXT NOT NULL DEFAULT 'per_unit',
    tax_behavior TEXT NOT NULL DEFAULT 'unspecified',
    trial_days INTEGER NOT NULL DEFAULT 0,
    config_json TEXT NOT NULL DEFAULT '{}',
    stripe_product_id TEXT NOT NULL DEFAULT '',
    stripe_price_id TEXT NOT NULL DEFAULT '',
    sync_status TEXT NOT NULL DEFAULT 'not_synced',
    sync_error TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS impresspress__products__offers_product_idx ON impresspress__products__offers (product_id, status);
CREATE INDEX IF NOT EXISTS impresspress__products__offers_stripe_price_idx ON impresspress__products__offers (stripe_price_id);

CREATE TABLE IF NOT EXISTS impresspress__products__offer_components (
    id TEXT PRIMARY KEY,
    offer_id TEXT NOT NULL,
    component_key TEXT NOT NULL,
    label TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0,
    required INTEGER NOT NULL DEFAULT 1,
    component_type TEXT NOT NULL DEFAULT 'fixed',
    amount_rule_json TEXT NOT NULL DEFAULT '{}',
    quantity_rule_json TEXT NOT NULL DEFAULT '{"type":"fixed","value":1}',
    condition_json TEXT NOT NULL DEFAULT '{}',
    recurring_json TEXT NOT NULL DEFAULT '{}',
    stripe_price_id TEXT NOT NULL DEFAULT '',
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__offer_components_key_uniq ON impresspress__products__offer_components (offer_id, component_key);
CREATE INDEX IF NOT EXISTS impresspress__products__offer_components_order_idx ON impresspress__products__offer_components (offer_id, sort_order);

CREATE TABLE IF NOT EXISTS impresspress__products__checkout_presets (
    id TEXT PRIMARY KEY,
    offer_id TEXT NOT NULL,
    name TEXT NOT NULL,
    slug TEXT NOT NULL DEFAULT '',
    inputs_json TEXT NOT NULL DEFAULT '{}',
    active INTEGER NOT NULL DEFAULT 1,
    configuration_hash TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__checkout_presets_slug_uniq
    ON impresspress__products__checkout_presets (offer_id, slug) WHERE slug <> '';

CREATE TABLE IF NOT EXISTS impresspress__products__payment_links (
    id TEXT PRIMARY KEY,
    offer_id TEXT NOT NULL,
    preset_id TEXT NOT NULL DEFAULT '',
    seller_account_id TEXT NOT NULL DEFAULT '',
    stripe_account_id TEXT NOT NULL DEFAULT '',
    stripe_payment_link_id TEXT NOT NULL DEFAULT '',
    stripe_buy_button_id TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL DEFAULT '',
    active INTEGER NOT NULL DEFAULT 1,
    configuration_hash TEXT NOT NULL DEFAULT '',
    sync_status TEXT NOT NULL DEFAULT 'not_synced',
    sync_error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS impresspress__products__payment_links_offer_idx ON impresspress__products__payment_links (offer_id, active);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__payment_links_stripe_uniq
    ON impresspress__products__payment_links (stripe_payment_link_id) WHERE stripe_payment_link_id <> '';

CREATE TABLE IF NOT EXISTS impresspress__products__seller_accounts (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'not_started',
    stripe_account_id TEXT NOT NULL DEFAULT '',
    details_submitted INTEGER NOT NULL DEFAULT 0,
    charges_enabled INTEGER NOT NULL DEFAULT 0,
    payouts_enabled INTEGER NOT NULL DEFAULT 0,
    requirements_json TEXT NOT NULL DEFAULT '{}',
    fee_basis_points INTEGER NOT NULL DEFAULT 0,
    suspended_at TEXT,
    last_synced_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__seller_accounts_user_uniq ON impresspress__products__seller_accounts (user_id);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__seller_accounts_stripe_uniq
    ON impresspress__products__seller_accounts (stripe_account_id) WHERE stripe_account_id <> '';

ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS buyer_user_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS buyer_email TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS seller_account_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS stripe_account_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS stripe_customer_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS stripe_subscription_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS checkout_mode TEXT NOT NULL DEFAULT 'hosted';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS livemode INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS subtotal_cents BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS discount_cents BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS tax_cents BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS platform_fee_cents BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS refunded_total_cents BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS reconciliation_status TEXT NOT NULL DEFAULT 'pending';
ALTER TABLE impresspress__products__purchases ADD COLUMN IF NOT EXISTS reconciliation_error TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS impresspress__products__purchases_seller_idx ON impresspress__products__purchases (seller_account_id, created_at);
CREATE INDEX IF NOT EXISTS impresspress__products__purchases_session_idx ON impresspress__products__purchases (provider_session_id);
CREATE INDEX IF NOT EXISTS impresspress__products__purchases_subscription_idx ON impresspress__products__purchases (stripe_subscription_id);

ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS offer_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS component_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS seller_account_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS stripe_price_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS unit_amount_minor BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS subtotal_minor BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS discount_minor BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS tax_minor BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS total_minor BIGINT NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS input_snapshot TEXT NOT NULL DEFAULT '{}';
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS condition_snapshot TEXT NOT NULL DEFAULT '{}';
ALTER TABLE impresspress__products__line_items ADD COLUMN IF NOT EXISTS offer_version INTEGER NOT NULL DEFAULT 1;

CREATE TABLE IF NOT EXISTS impresspress__products__subscription_items (
    id TEXT PRIMARY KEY,
    subscription_id TEXT NOT NULL,
    purchase_id TEXT NOT NULL DEFAULT '',
    product_id TEXT NOT NULL DEFAULT '',
    offer_id TEXT NOT NULL DEFAULT '',
    component_id TEXT NOT NULL DEFAULT '',
    stripe_subscription_item_id TEXT NOT NULL DEFAULT '',
    stripe_price_id TEXT NOT NULL DEFAULT '',
    quantity INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'active',
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS impresspress__products__subscription_items_subscription_idx ON impresspress__products__subscription_items (subscription_id);

CREATE TABLE IF NOT EXISTS impresspress__products__entitlements (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL DEFAULT '',
    buyer_email TEXT NOT NULL DEFAULT '',
    product_id TEXT NOT NULL,
    offer_id TEXT NOT NULL DEFAULT '',
    purchase_id TEXT NOT NULL DEFAULT '',
    subscription_id TEXT NOT NULL DEFAULT '',
    entitlement_key TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    starts_at TEXT,
    ends_at TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS impresspress__products__entitlements_subject_idx ON impresspress__products__entitlements (user_id, buyer_email, status);
CREATE INDEX IF NOT EXISTS impresspress__products__entitlements_purchase_idx ON impresspress__products__entitlements (purchase_id);

CREATE TABLE IF NOT EXISTS impresspress__products__provider_operations (
    id TEXT PRIMARY KEY,
    operation_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL DEFAULT '',
    aggregate_id TEXT NOT NULL DEFAULT '',
    stripe_account_id TEXT NOT NULL DEFAULT '',
    idempotency_key TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    request_json TEXT NOT NULL DEFAULT '{}',
    response_json TEXT NOT NULL DEFAULT '{}',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_error TEXT NOT NULL DEFAULT '',
    completed_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__provider_operations_idempotency_uniq ON impresspress__products__provider_operations (idempotency_key);
CREATE INDEX IF NOT EXISTS impresspress__products__provider_operations_due_idx ON impresspress__products__provider_operations (status, next_attempt_at);

ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS stripe_account_id TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS livemode INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS processing_owner TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS processing_started_at TEXT;
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS next_retry_at TEXT;
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS last_error TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__stripe_events ADD COLUMN IF NOT EXISTS processed_at TEXT;
CREATE INDEX IF NOT EXISTS impresspress__products__stripe_events_retry_idx ON impresspress__products__stripe_events (status, next_retry_at);
