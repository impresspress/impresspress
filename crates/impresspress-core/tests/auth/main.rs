//! Integration test crate for the `wafer-run/auth` block.

mod bootstrap_run;
mod common;
mod login_session_row;
mod migrations_001;
mod migrations_002;
mod migrations_008_rate_limits;
mod oauth_callback_repo;
mod repo_orgs;
mod repo_pats;
mod repo_provider_links;
mod repo_sessions;
mod repo_users;
mod service_require_role;
mod service_require_token;
mod service_require_user;
mod service_user_profile;
