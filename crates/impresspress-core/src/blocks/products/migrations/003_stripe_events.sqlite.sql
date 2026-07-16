-- Stripe webhook event idempotency (code review 2026-07-16: "Stripe
-- webhooks lack event idempotency"; I1 follow-up 2026-07-17: "recording
-- event before side effects drops the event on transient failure").
--
-- `stripe::handle_webhook` records the top-level Stripe `event.id` here via
-- `INSERT ... ON CONFLICT (id) DO NOTHING` BEFORE running any side effect
-- (purchase completion, subscription sync, webhook fan-out), with
-- `status = 'pending'`. Only once the side effects SUCCEED is the row
-- flipped to `status = 'processed'`. Stripe retries undelivered/non-2xx
-- webhooks, and the signature-timestamp window itself accepts up to 5
-- minutes of replay — both redeliver the exact same `id`:
--
-- - A redelivery that finds `status = 'processed'` is a true duplicate —
--   skipped, same as before.
-- - A redelivery that finds `status = 'pending'` means a PRIOR attempt died
--   mid-way (the process crashed, the DB call errored, etc. after the row
--   was recorded but before side effects completed) — it is RE-processed
--   rather than silently dropped, so a transient failure can't permanently
--   swallow a purchase/subscription update.
--
-- `id` is the Stripe event id itself (e.g. `evt_1N...`), not a synthetic
-- key — the PRIMARY KEY is the UNIQUE constraint the insert-or-ignore
-- conflicts against.
--
-- Mirrored to 003_stripe_events.postgres.sql.
CREATE TABLE IF NOT EXISTS impresspress__products__stripe_events (
    id            TEXT PRIMARY KEY,
    event_type    TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'pending',
    created_at    TEXT NOT NULL
);
