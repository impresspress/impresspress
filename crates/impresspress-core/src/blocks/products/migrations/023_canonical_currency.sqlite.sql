-- One spelling per currency: upper-case ISO 4217, the code
-- `money::normalize_currency` returns. Offers are stored through it, order
-- rows take theirs from a price preview computed from the offer, and refund,
-- dispute and seller-account rows copy an order's currency or upper-case
-- Stripe's on receipt. Two columns were written without it:
--
-- * products: every product write stored `currency` exactly as the caller
--   sent it, so a product created with `nzd` answered `nzd` while every offer
--   under it answered `NZD`. The product write paths and the data-snapshot
--   import now store the normalised code.
-- * purchases: before the typed commerce rewrite (005) an order's currency
--   was the checkout request's own `currency` field, stored as sent. Those
--   rows survive in the same table, and the Checkout Session and
--   PaymentIntent reconciliation (`repo::purchases`) compares the stored code
--   case-sensitively against Stripe's, upper-cased.
--
-- Re-running is harmless: after the first pass no row matches either
-- predicate, so a later replay of the whole migration set (the block
-- re-applies every file whenever the combined hash changes) updates nothing.
UPDATE impresspress__products__products
    SET currency = UPPER(TRIM(currency))
    WHERE currency <> UPPER(TRIM(currency));

UPDATE impresspress__products__purchases
    SET currency = UPPER(TRIM(currency))
    WHERE currency <> UPPER(TRIM(currency));
