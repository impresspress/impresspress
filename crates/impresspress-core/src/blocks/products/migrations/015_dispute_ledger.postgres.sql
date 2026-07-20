CREATE TABLE IF NOT EXISTS impresspress__products__disputes (
    id                    TEXT PRIMARY KEY,
    purchase_id           TEXT NOT NULL,
    seller_account_id     TEXT NOT NULL DEFAULT '',
    stripe_account_id     TEXT NOT NULL DEFAULT '',
    provider_dispute_id   TEXT NOT NULL,
    provider_charge_id    TEXT NOT NULL DEFAULT '',
    payment_intent_id     TEXT NOT NULL,
    status                TEXT NOT NULL,
    amount_minor          BIGINT NOT NULL,
    currency              TEXT NOT NULL,
    reason                TEXT NOT NULL DEFAULT '',
    evidence_due_by       TEXT,
    livemode              INTEGER NOT NULL DEFAULT 0,
    event_created         BIGINT NOT NULL DEFAULT 0,
    closed_at             TEXT,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL,
    FOREIGN KEY (purchase_id) REFERENCES impresspress__products__purchases(id)
);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__disputes_provider_uniq
    ON impresspress__products__disputes (stripe_account_id, provider_dispute_id);
CREATE INDEX IF NOT EXISTS impresspress__products__disputes_purchase_idx
    ON impresspress__products__disputes (purchase_id, created_at);
CREATE INDEX IF NOT EXISTS impresspress__products__disputes_status_idx
    ON impresspress__products__disputes (status, evidence_due_by);
