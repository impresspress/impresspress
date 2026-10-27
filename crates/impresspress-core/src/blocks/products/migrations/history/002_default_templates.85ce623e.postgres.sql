-- Default group/product templates. See `002_default_templates.sqlite.sql` for
-- the full rationale (static FK-parent rows moved out of the per-request
-- runtime seed into this hash-gated migration). Idempotent via
-- `ON CONFLICT (id) DO NOTHING`.
INSERT INTO suppers_ai__products__group_templates
    (id, name, display_name, created_at, updated_at)
VALUES
    ('default', 'default', 'Default', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')
ON CONFLICT (id) DO NOTHING;

INSERT INTO suppers_ai__products__product_templates
    (id, name, display_name, created_at, updated_at)
VALUES
    ('default', 'default', 'Default', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')
ON CONFLICT (id) DO NOTHING;
