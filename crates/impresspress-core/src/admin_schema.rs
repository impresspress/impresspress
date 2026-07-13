//! Shared table-name constants for the `impresspress/admin` block.
//!
//! These live outside `blocks/admin/` (mirroring [`crate::messages_schema`])
//! so that consumers which read admin-owned rows by table name without
//! depending on the admin block module — today the config-snapshot cache
//! (`cache_key.rs`), the request pipeline (`pipeline.rs`), and the shared
//! migration runner (`migration_helper.rs`) — can reference them as a single
//! source of truth.
//!
//! `blocks/admin` re-exports from here (`settings.rs`, `logs.rs`), so existing
//! `blocks::admin::{BLOCK_SETTINGS_TABLE, VARIABLES_TABLE, REQUEST_LOGS_TABLE}`
//! and `settings::VARIABLES_TABLE` references continue to resolve. New
//! consumers should import directly from this module.
//!
//! Why a sibling of `blocks/`?
//! - The constants describe the on-disk schema contract, not block logic, and
//!   `migration_helper.rs` previously open-coded a *duplicate* literal here
//!   with a bogus "avoid a circular dep on `crate::blocks::admin`" comment
//!   (Rust modules in one crate cannot have circular import problems). A leaf
//!   module removes the temptation to re-hardcode the literal.
//! - WRAP grants are still declared by `AdminBlock::info()` (the schema-owning
//!   block); other modules read rows via runtime grants, not by re-declaring
//!   ownership.

/// Per-block enable/config settings (one row per block). Owned by the admin
/// block.
///
/// `pub` (not `pub(crate)`) because consumers outside `impresspress-core`
/// reference this table by name.
pub const BLOCK_SETTINGS_TABLE: &str = "impresspress__admin__block_settings";

/// Admin-managed configuration variables (key/value/scope/sensitive). Owned by
/// the admin block.
///
/// `pub` (not `pub(crate)`) because consumers outside `impresspress-core`
/// reference this table by name.
pub const VARIABLES_TABLE: &str = "impresspress__admin__variables";

/// HTTP request log entries (one row per inbound request). Owned by the admin
/// block.
pub const REQUEST_LOGS_TABLE: &str = "impresspress__admin__request_logs";
