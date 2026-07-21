-- Short-lived, capability-style access for guest checkout return pages.
-- Only a SHA-256 hash is stored; the raw token is returned once when the
-- checkout session is created and kept by the storefront in sessionStorage.
ALTER TABLE impresspress__products__purchases ADD COLUMN receipt_token_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE impresspress__products__purchases ADD COLUMN receipt_token_expires_at TEXT;
