//! Request bodies for the `/b/messages/api/*` JSON surface.
//!
//! [`CreateContextRequest`] / [`AddEntryRequest`] promote the private,
//! in-function `Body` structs `rest::create_context` / `rest::add_entry`
//! already deserialized into — they were already real deserialize targets,
//! just not named or reachable from `mod.rs`, so the schema next to the
//! endpoint declaration had to be hand-written independently. Same field
//! list, same `#[serde(...)]` attributes, so `.input::<T>()` describes
//! exactly what these handlers already accepted.
//!
//! [`UpdateContextRequest`] is the one genuine "write the struct" fix:
//! `rest::update_context` used to deserialize the body into
//! `HashMap<String, serde_json::Value>` and `service::update_context` then
//! walked a `let allowed = ["status", "title", "metadata"]` array to pick
//! out the only three keys it would ever apply. The struct below replaces
//! both the ad hoc map and the string-keyed whitelist — there is no fourth
//! field left to whitelist against, the type says what used to be an
//! enforced-at-runtime convention. Any other key in the request body is
//! still silently ignored (serde's default behaviour for an unrecognized
//! field, same as the whitelist loop's `.get(*key)` was), so this is
//! behaviour-preserving.
//!
//! `list_contexts` / `list_entries` response schemas and the `GetContext`
//! path params stay hand-written in `mod.rs` — see the comments at those
//! call sites for why.

use serde::{Deserialize, Serialize};

/// What an entry *is*: the `kind` column of `impresspress__messages__entries`.
///
/// The four values the block has always documented (`contracts.rs`'s old
/// `"Entry kind: message, artifact, notification, status."`, the composer's
/// four `<option>`s and the `?kind=` filter's description), now spelled
/// once. The column is `TEXT NOT NULL DEFAULT 'message'`
/// (`migrations/001_messages_schema.sqlite.sql`), which is why
/// [`EntryKind::message`] is the serde default too: an omitted `kind`
/// stored `"message"` before this type existed and stores it still.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Message,
    Artifact,
    Notification,
    Status,
}

impl EntryKind {
    /// The `kind` an entry gets when the request body omits one — the same
    /// value the column's own `DEFAULT` clause supplies.
    pub const fn message() -> Self {
        Self::Message
    }
}

/// Who produced an entry: the `role` column of
/// `impresspress__messages__entries`.
///
/// **This type is B20.** The block documented four spellings of three roles
/// — its composer offered `agent`, `blocks::llm` wrote `assistant` — and
/// nothing reconciled them, so `llm::routes::chat::role_from_str` mapped
/// every value it did not recognise (`agent` included) onto
/// `ChatRole::User`. An agent's own turn was replayed to the model as the
/// user's next instruction. There is now one type, `blocks::llm` matches it
/// exhaustively, and a variant added here cannot silently become a user
/// turn: the match stops compiling instead.
///
/// `assistant` is the canonical spelling because it is what `blocks::llm`
/// has been storing and what `wafer_core::clients::llm::ChatRole` already
/// names. `agent` survives as a deserialisation alias, so both the rows
/// already in the table and any client still sending it keep working — they
/// are simply read, and re-published, as `assistant`. No stored row is
/// rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryRole {
    User,
    #[serde(alias = "agent")]
    Assistant,
    System,
}

impl EntryRole {
    /// The `role` an entry gets when the request body omits one.
    ///
    /// The column's `DEFAULT` is `''`, and an empty role is invisible to the
    /// model: `llm::routes::chat::history_to_messages` drops every entry
    /// whose role does not parse. So the block's own default silently
    /// excluded the entry from the conversation it was posted into. An
    /// omitted role now stores `user`, which is what a body carrying only
    /// `content` has always meant.
    pub const fn user() -> Self {
        Self::User
    }
}

/// `POST /b/messages/api/contexts` request body.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateContextRequest {
    /// Context type: conversation, task, notification, etc.
    #[serde(rename = "type")]
    pub context_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub sender_id: String,
    #[serde(default)]
    pub recipient_id: String,
    /// Parent context ID for sub-tasks/threads.
    pub parent_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// `PATCH /b/messages/api/contexts/{id}` request body. Every field is
/// optional and only the ones present are applied — see the module doc for
/// why this replaced a `HashMap<String, Value>` plus a runtime whitelist.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateContextRequest {
    pub status: Option<String>,
    pub title: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// `POST /b/messages/api/contexts/{id}/entries` request body.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddEntryRequest {
    /// What this entry is.
    #[serde(default = "EntryKind::message")]
    pub kind: EntryKind,
    /// Who produced it. `agent` is accepted as an alias of `assistant` and
    /// is stored as `assistant`.
    #[serde(default = "EntryRole::user")]
    pub role: EntryRole,
    #[serde(default)]
    pub sender_id: String,
    #[serde(default)]
    pub content: String,
    /// MIME type of `content`. Defaults to `text/plain` when omitted or
    /// explicitly `null`.
    // The explicit `#[serde(default = ..)]` (rather than relying on
    // `Option<T>`'s automatic missing-key handling, as `metadata` below
    // does) exists purely so schemars can surface a `default` in the schema
    // — matching `service::add_entry`'s real `content_type.unwrap_or(..)`
    // fallback, which runs identically whether this field ends up `None`
    // (key missing) or `Some("text/plain")` (key missing, default applied).
    #[serde(default = "default_content_type")]
    pub content_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

fn default_content_type() -> Option<String> {
    Some("text/plain".to_string())
}
