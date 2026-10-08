-- One spelling per currency: upper-case ISO 4217. Offers and price previews
-- are stored through `money::normalize_currency`, which returns the
-- upper-case code, and the order, refund, dispute and seller-account rows
-- take theirs from a preview or upper-case Stripe's value on receipt. Product
-- writes stored `currency` exactly as the caller sent it, so a product
-- created with `nzd` answered `nzd` while every offer under it answered
-- `NZD`. The product write paths now store the normalised code, and this
-- brings the rows written before that into the same spelling.
--
-- Only this table is rewritten: it is the one currency column the code wrote
-- without upper-casing it first.
--
-- Re-running is harmless: after the first pass no row matches the predicate,
-- so a later replay of the whole migration set (the block re-applies every
-- file whenever the combined hash changes) updates nothing.
UPDATE impresspress__products__products
    SET currency = UPPER(TRIM(currency))
    WHERE currency <> UPPER(TRIM(currency));
