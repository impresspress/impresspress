//! Shared table-name constants for the `impresspress/messages` block.
//!
//! These live outside `blocks/messages/` so that consumers which read
//! messages-owned rows by table name (today: the LLM chat UI in
//! `blocks/llm/pages.rs`) can reference them without forcing the messages
//! block module — and its dependencies — to compile.
//!
//! The `MessagesBlock` impl re-exports from here, so existing
//! `blocks::messages::service::{CONTEXTS_TABLE, ENTRIES_TABLE}` references
//! continue to resolve. New consumers should import directly from this
//! module.
//!
//! Why a sibling of `blocks/`?
//! - The constants describe the on-disk schema contract, not block logic;
//!   keeping them in a leaf module with zero deps is the only way to make
//!   them feature-independent (a `blocks/messages/tables.rs` would still
//!   live inside the gated `messages` module tree).
//! - WRAP grants are still declared by `MessagesBlock::info()` (the
//!   schema-owning block); other blocks read rows via runtime grants, not by
//!   re-declaring ownership.

/// Table backing `impresspress/messages` contexts (conversations, tasks,
/// notifications, …). Owned by the messages block.
pub const CONTEXTS_TABLE: &str = "impresspress__messages__contexts";

/// Table backing `impresspress/messages` entries (messages, artifacts,
/// notifications, status changes). Owned by the messages block.
pub const ENTRIES_TABLE: &str = "impresspress__messages__entries";
