-- Products block schema (PostgreSQL).
--
-- Mirror of 001_products_schema.sqlite.sql. INTEGER (not BOOLEAN) is used
-- for boolean-like columns to match the JSON-value round-trips used by
-- block code. DOUBLE PRECISION is used for the float price columns.
-- CREATE TABLE IF NOT EXISTS makes this idempotent across repeated `Init`
-- lifecycle events.
--
-- Solobase deploys SQLite/D1 today; this file is included for parity with
-- the auth/files migrations pattern. Validate before enabling Postgres
-- for the products block.

-- Products ----------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__products (
    id                    TEXT PRIMARY KEY,
    name                  TEXT NOT NULL,
    description           TEXT NOT NULL DEFAULT '',
    slug                  TEXT NOT NULL DEFAULT '',
    price                 DOUBLE PRECISION NOT NULL DEFAULT 0,
    base_price            DOUBLE PRECISION NOT NULL DEFAULT 0,
    currency              TEXT NOT NULL DEFAULT 'USD',
    status                TEXT NOT NULL DEFAULT 'draft',
    category              TEXT NOT NULL DEFAULT '',
    tags                  TEXT NOT NULL DEFAULT '[]',
    metadata              TEXT NOT NULL DEFAULT '{}',
    image_url             TEXT NOT NULL DEFAULT '',
    stock                 INTEGER NOT NULL DEFAULT 0,
    group_id              TEXT NOT NULL DEFAULT '',
    type_id               TEXT NOT NULL DEFAULT '',
    group_template_id     TEXT NOT NULL DEFAULT '',
    product_template_id   TEXT NOT NULL DEFAULT '',
    pricing_template_id   TEXT NOT NULL DEFAULT '',
    requires              TEXT NOT NULL DEFAULT '',
    created_by            TEXT NOT NULL DEFAULT '',
    deleted_at            TEXT,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__products__products_status_idx
    ON suppers_ai__products__products (status);
CREATE INDEX IF NOT EXISTS suppers_ai__products__products_group_id_idx
    ON suppers_ai__products__products (group_id);
CREATE INDEX IF NOT EXISTS suppers_ai__products__products_created_by_idx
    ON suppers_ai__products__products (created_by);

-- Groups ------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__groups (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    template_id         TEXT NOT NULL DEFAULT '',
    group_template_id   TEXT NOT NULL DEFAULT '',
    user_id             TEXT NOT NULL DEFAULT '',
    status              TEXT NOT NULL DEFAULT 'active',
    created_by          TEXT NOT NULL DEFAULT '',
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

-- Types -------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__types (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    is_system   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- Pricing templates -------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__pricing_templates (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    price_formula   TEXT NOT NULL DEFAULT '',
    template_data   TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

-- Purchases ---------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__purchases (
    id                          TEXT PRIMARY KEY,
    user_id                     TEXT NOT NULL,
    status                      TEXT NOT NULL DEFAULT 'pending',
    total_cents                 BIGINT NOT NULL DEFAULT 0,
    amount_cents                BIGINT NOT NULL DEFAULT 0,
    currency                    TEXT NOT NULL DEFAULT 'USD',
    provider                    TEXT NOT NULL DEFAULT 'manual',
    metadata                    TEXT NOT NULL DEFAULT '{}',
    stripe_payment_intent_id    TEXT NOT NULL DEFAULT '',
    provider_payment_intent_id  TEXT NOT NULL DEFAULT '',
    approved_at                 TEXT,
    refunded_at                 TEXT,
    refunded_by                 TEXT NOT NULL DEFAULT '',
    refund_reason               TEXT NOT NULL DEFAULT '',
    payment_at                  TEXT,
    created_at                  TEXT NOT NULL,
    updated_at                  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__products__purchases_user_id_idx
    ON suppers_ai__products__purchases (user_id);
CREATE INDEX IF NOT EXISTS suppers_ai__products__purchases_status_idx
    ON suppers_ai__products__purchases (status);
CREATE INDEX IF NOT EXISTS suppers_ai__products__purchases_provider_payment_intent_idx
    ON suppers_ai__products__purchases (provider_payment_intent_id);

-- Line items --------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__line_items (
    id            TEXT PRIMARY KEY,
    purchase_id   TEXT NOT NULL,
    product_id    TEXT NOT NULL,
    product_name  TEXT NOT NULL DEFAULT '',
    quantity      INTEGER NOT NULL DEFAULT 1,
    unit_price    DOUBLE PRECISION NOT NULL DEFAULT 0,
    total_price   DOUBLE PRECISION NOT NULL DEFAULT 0,
    variables     TEXT NOT NULL DEFAULT '{}',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS suppers_ai__products__line_items_purchase_id_idx
    ON suppers_ai__products__line_items (purchase_id);

-- Group templates ---------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__group_templates (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    display_name  TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

-- Product templates -------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__product_templates (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    display_name  TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

-- Variables (pricing-formula inputs) --------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__variables (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    var_type       TEXT NOT NULL DEFAULT 'number',
    default_value  TEXT,
    scope          TEXT NOT NULL DEFAULT 'system',
    product_id     TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

-- Subscriptions -----------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__subscriptions (
    id                       TEXT PRIMARY KEY,
    user_id                  TEXT NOT NULL,
    stripe_customer_id       TEXT NOT NULL DEFAULT '',
    stripe_subscription_id   TEXT NOT NULL DEFAULT '',
    plan                     TEXT NOT NULL DEFAULT '',
    status                   TEXT NOT NULL DEFAULT '',
    grace_period_end         TEXT,
    addon_projects           BIGINT NOT NULL DEFAULT 0,
    addon_requests           BIGINT NOT NULL DEFAULT 0,
    addon_r2_bytes           BIGINT NOT NULL DEFAULT 0,
    addon_d1_bytes           BIGINT NOT NULL DEFAULT 0,
    created_at               TEXT NOT NULL,
    updated_at               TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__products__subscriptions_user_id_uniq
    ON suppers_ai__products__subscriptions (user_id);
CREATE INDEX IF NOT EXISTS suppers_ai__products__subscriptions_stripe_sub_id_idx
    ON suppers_ai__products__subscriptions (stripe_subscription_id);
