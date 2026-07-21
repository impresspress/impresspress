CREATE TABLE IF NOT EXISTS impresspress__products__refunds (
    id                            TEXT PRIMARY KEY,
    purchase_id                   TEXT NOT NULL,
    provider_refund_id            TEXT NOT NULL DEFAULT '',
    payment_intent_id             TEXT NOT NULL,
    stripe_account_id             TEXT NOT NULL DEFAULT '',
    idempotency_key               TEXT NOT NULL,
    amount_minor                  BIGINT NOT NULL CHECK (amount_minor > 0),
    target_refunded_total_minor   BIGINT NOT NULL CHECK (target_refunded_total_minor > 0),
    currency                      TEXT NOT NULL,
    status                        TEXT NOT NULL DEFAULT 'pending',
    provider_status               TEXT NOT NULL DEFAULT '',
    provider_reason               TEXT NOT NULL DEFAULT '',
    note                          TEXT NOT NULL DEFAULT '',
    refunded_by                   TEXT NOT NULL DEFAULT '',
    livemode                      INTEGER NOT NULL DEFAULT 0,
    response_json                 TEXT NOT NULL DEFAULT '{}',
    last_error                    TEXT NOT NULL DEFAULT '',
    completed_at                  TEXT,
    created_at                    TEXT NOT NULL,
    updated_at                    TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__refunds_idempotency_uniq
    ON impresspress__products__refunds (idempotency_key);
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__refunds_provider_uniq
    ON impresspress__products__refunds (provider_refund_id)
    WHERE provider_refund_id <> '';
CREATE UNIQUE INDEX IF NOT EXISTS impresspress__products__refunds_active_purchase_uniq
    ON impresspress__products__refunds (purchase_id)
    WHERE status IN ('pending', 'provider_succeeded');
CREATE INDEX IF NOT EXISTS impresspress__products__refunds_purchase_idx
    ON impresspress__products__refunds (purchase_id, created_at);
