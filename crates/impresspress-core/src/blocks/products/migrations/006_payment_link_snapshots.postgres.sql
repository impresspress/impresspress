-- Immutable local quote used to reconcile reusable Payment Link sessions.
ALTER TABLE impresspress__products__payment_links
    ADD COLUMN IF NOT EXISTS pricing_snapshot TEXT NOT NULL DEFAULT '{}';
ALTER TABLE impresspress__products__payment_links
    ADD COLUMN IF NOT EXISTS fee_basis_points INTEGER NOT NULL DEFAULT 0;
