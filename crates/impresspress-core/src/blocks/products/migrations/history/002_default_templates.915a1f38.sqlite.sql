-- Default group/product templates — static FK-parent rows the groups and
-- products tables require. Previously seeded at runtime in
-- `ProductsBlock::lifecycle(Init)` via a per-request existence check + insert
-- (two `db::list_all` reads on every request, uncached). Moved here so the
-- seed is hash-gated like every other migration and costs zero D1 reads per
-- request in steady state. Idempotent via `INSERT OR IGNORE` on the PK.
--
-- Lookups resolve the default by `name = 'default'` (see
-- `handlers::default_template_id`), so the `id` value is arbitrary as long as
-- it is stable; we use the literal `default`.
INSERT OR IGNORE INTO suppers_ai__products__group_templates
    (id, name, display_name, created_at, updated_at)
VALUES
    ('default', 'default', 'Default', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z');

INSERT OR IGNORE INTO suppers_ai__products__product_templates
    (id, name, display_name, created_at, updated_at)
VALUES
    ('default', 'default', 'Default', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z');
