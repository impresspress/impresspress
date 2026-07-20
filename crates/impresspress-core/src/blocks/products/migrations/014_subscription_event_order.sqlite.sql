-- Stripe does not guarantee webhook delivery order. Keep the provider event
-- creation timestamp beside each subscription projection so an older invoice
-- or subscription delivery cannot overwrite newer lifecycle state.
ALTER TABLE impresspress__products__purchases ADD COLUMN subscription_event_created INTEGER NOT NULL DEFAULT 0;
ALTER TABLE impresspress__products__subscriptions ADD COLUMN stripe_event_created INTEGER NOT NULL DEFAULT 0;
