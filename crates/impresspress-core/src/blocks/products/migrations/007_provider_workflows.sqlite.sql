-- Durable Stripe Connect status fields used by seller onboarding/status UI.
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN livemode INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN country TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN default_currency TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN dashboard_type TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN requirements_disabled_reason TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN sync_error TEXT NOT NULL DEFAULT '';
