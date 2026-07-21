ALTER TABLE impresspress__products__purchases ADD COLUMN provider_payment_status TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN provider_payment_error_code TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN provider_payment_error_message TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN payment_intent_event_created INTEGER NOT NULL DEFAULT 0;
