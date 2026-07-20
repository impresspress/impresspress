-- Durable Stripe Connect status fields used by seller onboarding/status UI.
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN IF NOT EXISTS livemode INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN IF NOT EXISTS country TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN IF NOT EXISTS default_currency TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN IF NOT EXISTS dashboard_type TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN IF NOT EXISTS requirements_disabled_reason TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__seller_accounts
    ADD COLUMN IF NOT EXISTS sync_error TEXT NOT NULL DEFAULT '';
