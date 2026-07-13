//! Auth block migrations. Applied from the framework auth service's
//! `AuthService::init` (the framework `wafer-run/auth` block's
//! `lifecycle(Init)` delegates to it) via
//! [`crate::migration_helper::apply_migrations`].
//!
//! Hash-gated apply — runs only when the SQL hash differs from the recorded
//! `current_hash` in `impresspress__admin__block_settings`. Concatenated SQL of
//! all migration scripts is hashed and tracked.

const SQL_001_SQLITE: &str = include_str!("001_auth_schema.sqlite.sql");
const SQL_001_POSTGRES: &str = include_str!("001_auth_schema.postgres.sql");
const SQL_002_SQLITE: &str = include_str!("002_reserved_orgs.sqlite.sql");
const SQL_002_POSTGRES: &str = include_str!("002_reserved_orgs.postgres.sql");
const SQL_003_SQLITE: &str = include_str!("003_oauth_pkce_states.sqlite.sql");
const SQL_003_POSTGRES: &str = include_str!("003_oauth_pkce_states.postgres.sql");
const SQL_004_SQLITE: &str = include_str!("004_refresh_tokens.sqlite.sql");
const SQL_004_POSTGRES: &str = include_str!("004_refresh_tokens.postgres.sql");
const SQL_005_SQLITE: &str = include_str!("005_jwt_blocklist.sqlite.sql");
const SQL_005_POSTGRES: &str = include_str!("005_jwt_blocklist.postgres.sql");
const SQL_006_SQLITE: &str = include_str!("006_user_extended_fields.sqlite.sql");
const SQL_006_POSTGRES: &str = include_str!("006_user_extended_fields.postgres.sql");
const SQL_007_SQLITE: &str = include_str!("007_api_keys.sqlite.sql");
const SQL_007_POSTGRES: &str = include_str!("007_api_keys.postgres.sql");
const SQL_008_SQLITE: &str = include_str!("008_rate_limits.sqlite.sql");
const SQL_008_POSTGRES: &str = include_str!("008_rate_limits.postgres.sql");

/// Ordered SQLite migration scripts for this block, as `(basename, content)`
/// pairs. Feeds the runtime `lifecycle(Init)` apply path (auth's `init`).
/// Order here is the apply order.
pub(crate) const SQLITE_MIGRATIONS: &[(&str, &str)] = &[
    ("001_auth_schema", SQL_001_SQLITE),
    ("002_reserved_orgs", SQL_002_SQLITE),
    ("003_oauth_pkce_states", SQL_003_SQLITE),
    ("004_refresh_tokens", SQL_004_SQLITE),
    ("005_jwt_blocklist", SQL_005_SQLITE),
    ("006_user_extended_fields", SQL_006_SQLITE),
    ("007_api_keys", SQL_007_SQLITE),
    ("008_rate_limits", SQL_008_SQLITE),
];

/// Ordered PostgreSQL migration scripts, matching [`SQLITE_MIGRATIONS`] one
/// for one. Selected at runtime by `apply_migrations`.
pub(crate) const POSTGRES_MIGRATIONS: &[&str] = &[
    SQL_001_POSTGRES,
    SQL_002_POSTGRES,
    SQL_003_POSTGRES,
    SQL_004_POSTGRES,
    SQL_005_POSTGRES,
    SQL_006_POSTGRES,
    SQL_007_POSTGRES,
    SQL_008_POSTGRES,
];

/// Apply the auth schema through the shared migration-state gate.
///
/// Production no longer calls this: the framework auth service applies these
/// migrations inside [`AuthService::init`](super::service) (via
/// `apply_migrations` directly, because it needs an `AuthError` return).
/// This thin forwarder exists for the `tests/auth/*` integration suite, which
/// applies the auth schema against an in-memory fixture before exercising the
/// repo layer — test-fixture setup is an explicit exception to the
/// no-raw-migration-runner rule (CLAUDE.md).
pub async fn apply(ctx: &dyn wafer_run::context::Context) -> Result<(), String> {
    let sqlite: Vec<&str> = SQLITE_MIGRATIONS.iter().map(|(_, sql)| *sql).collect();
    crate::migration_helper::apply_migrations(ctx, "wafer-run/auth", &sqlite, POSTGRES_MIGRATIONS)
        .await
}
