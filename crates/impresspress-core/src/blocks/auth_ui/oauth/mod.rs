//! OAuth handlers for the auth-ui block.
//!
//! Each leaf module hosts one handler relocated from the legacy
//! `auth/oauth.rs` module in Task 5 of Plan A2 PR 5.

pub mod callback;
pub mod providers;
pub(crate) mod spec;
pub mod start;
