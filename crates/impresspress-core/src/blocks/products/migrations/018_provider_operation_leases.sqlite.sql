-- Atomic leases for scheduled/manual provider reconciliation workers.
ALTER TABLE impresspress__products__provider_operations
    ADD COLUMN processing_owner TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__provider_operations
    ADD COLUMN processing_started_at TEXT;
ALTER TABLE impresspress__products__provider_operations
    ADD COLUMN terminal_at TEXT;
