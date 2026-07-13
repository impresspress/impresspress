//! Row-level data access for the `impresspress/files` block.
//!
//! Mirrors `blocks/auth/repo/`: each submodule owns its table — the
//! `TABLE` const and every `db::*` statement that touches it live here, so
//! the handler/page modules above never issue database calls directly.
//! Functions are thin typed wrappers around the pre-existing queries (same
//! filters, same values) and surface the db client's `WaferError`
//! unchanged, so call-site error handling (NotFound matching, warn-and-
//! default, `err_internal`) keeps its exact previous behavior.
//!
//! Submodule → table map:
//! - [`buckets`] — `impresspress__files__buckets`
//! - [`objects`] — `impresspress__files__objects`
//! - [`views`] — `impresspress__files__views`
//! - [`shares`] — `impresspress__files__cloud_shares` +
//!   `impresspress__files__cloud_access_logs` (the access log is a child
//!   audit table of shares; one submodule owns both)
//! - [`quota`] — `impresspress__files__cloud_quotas`

pub mod buckets;
pub mod objects;
pub mod quota;
pub mod shares;
pub mod views;
