//! Platform state: the six `impresspress__admin__*` tables the runtime
//! itself reads and writes — configuration variables, per-block settings,
//! admin-created WRAP grants, the request log, role grants and the per-file
//! migration ledger.
//!
//! Each submodule owns one table: its `TABLE` name, its row struct, one
//! `from_record` and one `to_data`, so a column name is spelled in exactly
//! one Rust file. Every other module — the boot orchestrators, the request
//! pipeline, the admin block's pages, the framework auth block, the dev
//! sandbox, the platform adapters and the CLI — reaches the table through
//! the functions here (spec `2026-09-06-ownership-and-repo-boundaries`,
//! section 2.1).
//!
//! Two callers, one codec: boot runs before WRAP over
//! `Arc<dyn DatabaseService>` (native seeds pre-wafer, Cloudflare and the
//! browser seed after admin's `Init`), blocks and the pipeline run under
//! WRAP over `&dyn Context`. Where the spec names both flavours a module
//! offers both, and both go through the same row codec.
//!
//! Five of the tables are still created by the admin block's migrations —
//! `blocks/admin/migrations/001_admin_schema.{sqlite,postgres}.sql` (all
//! five), `002_variables_block_column` (`variables.block`),
//! `003_block_settings_seed_hash` (`block_settings.seed_defaults_hash`),
//! `004_user_roles_unique` (one `user_roles` row per `(user_id, role)`) and
//! `005_wrap_grants_append_column` (`wrap_grants.append`) — applied by
//! admin's `Init` like every block's migrations. Moving the DDL would change
//! the shipped migration files every deployment has applied, so the schema
//! keeps living there (spec decision 5.4). The sixth, the migration ledger
//! ([`applied_migrations`]), is created by the migration runner itself,
//! because it has to exist before admin's first file can be recorded in it.
//! `blocks/admin` also keeps `ADMIN_BLOCK_ID` and the
//! `collections(..)`/`grants(..)` declarations, because the
//! `impresspress__admin__` prefix makes it the WRAP schema owner.

pub mod applied_migrations;
pub mod block_settings;
pub mod request_logs;
pub mod user_roles;
pub mod variables;
pub mod wrap_grants;

/// One page of typed rows plus the envelope `db::paginated_list` returns,
/// for the admin pages' pagination controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}
