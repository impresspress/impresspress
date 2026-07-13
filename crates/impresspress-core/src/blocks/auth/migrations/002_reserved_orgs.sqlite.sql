-- Reserved org names (spec §3). Only site admins can publish under these.
-- Idempotent: re-running is a no-op via INSERT OR IGNORE on the unique `name`.
INSERT OR IGNORE INTO wafer_run__auth__orgs (id, name, is_reserved, created_at)
VALUES
    ('reserved-wafer-run',  'wafer-run',  1, '1970-01-01T00:00:00Z'),
    ('reserved-wafer',      'wafer',      1, '1970-01-01T00:00:00Z'),
    ('reserved-impresspress', 'impresspress', 1, '1970-01-01T00:00:00Z');
