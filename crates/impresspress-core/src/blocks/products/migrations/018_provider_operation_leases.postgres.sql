-- Atomic leases for scheduled/manual provider reconciliation workers.
ALTER TABLE impresspress__products__provider_operations
    ADD COLUMN IF NOT EXISTS processing_owner TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__provider_operations
    ADD COLUMN IF NOT EXISTS processing_started_at TEXT;
ALTER TABLE impresspress__products__provider_operations
    ADD COLUMN IF NOT EXISTS terminal_at TEXT;
