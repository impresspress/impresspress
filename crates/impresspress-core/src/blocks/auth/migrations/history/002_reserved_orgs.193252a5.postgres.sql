-- Reserved org names (spec §3). Only site admins can publish under these.
-- Idempotent: re-running is a no-op via ON CONFLICT on the unique `name`.
INSERT INTO suppers_ai__auth__orgs (id, name, is_reserved, created_at)
VALUES
    ('reserved-wafer-run',  'wafer-run',  TRUE, '1970-01-01T00:00:00Z'),
    ('reserved-wafer',      'wafer',      TRUE, '1970-01-01T00:00:00Z'),
    ('reserved-suppers-ai', 'suppers-ai', TRUE, '1970-01-01T00:00:00Z'),
    ('reserved-solobase',   'solobase',   TRUE, '1970-01-01T00:00:00Z')
ON CONFLICT (name) DO NOTHING;
