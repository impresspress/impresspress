-- Products block schema (SQLite / D1).
--
-- Replaces the implicit `ensure_table` materialization (TEXT-only columns,
-- no indexes) for tables owned by `suppers-ai/products`. CREATE TABLE
-- IF NOT EXISTS is a no-op against existing prod tables, but the CREATE
-- INDEX statements below add indexes the auto-create path was silently
-- skipping (CollectionSchema indexes are advisory under ensure_table).
--
-- Also creates the `subscriptions` table which was previously missing
-- from CollectionSchema entirely — stripe.rs writes to it on
-- `checkout.session.completed` and the addon_* columns were materialized
-- as TEXT then queried via COALESCE(.., 0).
--
-- Mirrored to 001_products_schema.postgres.sql.

-- Products ----------------------------------------------------------------
CREATE TABLE IF NOT EXISTS suppers_ai__products__products (
    id                    TEXT PRIMARY KEY,
    name                  TEXT NOT NULL,
    description           TEXT NOT NULL DEFAULT '',
    slug                  TEXT NOT NULL DEFAULT '',
    base_price            REAL NOT NULL DEFAULT 0,
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
-- Includes columns referenced by stripe.rs that were missing from the
-- CollectionSchema declaration: `provider_payment_intent_id`, `approved_at`.
CREATE TABLE IF NOT EXISTS suppers_ai__products__purchases (
    id                          TEXT PRIMARY KEY,
    user_id                     TEXT NOT NULL,
    status                      TEXT NOT NULL DEFAULT 'pending',
    total_cents                 INTEGER NOT NULL DEFAULT 0,
    amount_cents                INTEGER NOT NULL DEFAULT 0,
    currency                    TEXT NOT NULL DEFAULT 'USD',
    provider                    TEXT NOT NULL DEFAULT 'manual',
    metadata                    TEXT NOT NULL DEFAULT '{}',
    stripe_payment_intent_id    TEXT NOT NULL DEFAULT '',
    provider_payment_intent_id  TEXT NOT NULL DEFAULT '',
    provider_session_id         TEXT NOT NULL DEFAULT '',
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
    unit_price    REAL NOT NULL DEFAULT 0,
    total_price   REAL NOT NULL DEFAULT 0,
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
-- Written by stripe.rs webhook handlers, queried by handlers.rs for
-- subscription/addon lookup. Was previously absent from CollectionSchema
-- and materialized via ensure_table on first insert (TEXT-only columns,
-- including the addon_* counts that are written + read as INTEGER).
CREATE TABLE IF NOT EXISTS suppers_ai__products__subscriptions (
    id                       TEXT PRIMARY KEY,
    user_id                  TEXT NOT NULL,
    stripe_customer_id       TEXT NOT NULL DEFAULT '',
    stripe_subscription_id   TEXT NOT NULL DEFAULT '',
    plan                     TEXT NOT NULL DEFAULT '',
    status                   TEXT NOT NULL DEFAULT '',
    grace_period_end         TEXT,
    addon_projects           INTEGER NOT NULL DEFAULT 0,
    addon_requests           INTEGER NOT NULL DEFAULT 0,
    addon_r2_bytes           INTEGER NOT NULL DEFAULT 0,
    addon_d1_bytes           INTEGER NOT NULL DEFAULT 0,
    created_at               TEXT NOT NULL,
    updated_at               TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS suppers_ai__products__subscriptions_user_id_uniq
    ON suppers_ai__products__subscriptions (user_id);
CREATE INDEX IF NOT EXISTS suppers_ai__products__subscriptions_stripe_sub_id_idx
    ON suppers_ai__products__subscriptions (stripe_subscription_id);
