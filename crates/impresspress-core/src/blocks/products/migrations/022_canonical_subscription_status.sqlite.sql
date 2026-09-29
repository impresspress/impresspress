-- One spelling per subscription status: Stripe's. The platform-billing
-- projection stored its cancelled state as the British `cancelled`, while
-- every other subscription status — here and in the order table's
-- `subscription_status` — is spelled the way Stripe sends it, `canceled`
-- included. `SubscriptionStatus` now has no second spelling to accept, so a
-- row left holding `cancelled` is refused as a data fault on every read.
--
-- Only this table is rewritten. `impresspress__products__purchases`'
-- `subscription_status` is written by `repo::purchases::sync_commerce_subscription`
-- from the status Stripe delivered, which is never the British spelling.
--
-- Re-running is harmless: after the first pass no row matches the predicate,
-- so a later replay of the whole migration set (the block re-applies every
-- file whenever the combined hash changes) updates nothing.
UPDATE impresspress__products__subscriptions
    SET status = 'canceled'
    WHERE status = 'cancelled';
