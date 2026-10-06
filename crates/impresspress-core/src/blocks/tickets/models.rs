//! Typed ticket-domain inputs and validation.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $value)] $variant),+
        }

        impl $name {
            /// Every value, in declaration order — what a form offers.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(format!("invalid {}: {value}", stringify!($name))),
                }
            }
        }
    };
}

string_enum!(TicketSource {
    PublicForm => "public_form",
    Admin => "admin",
    Api => "api",
    Ai => "ai",
});

string_enum!(TicketStatus {
    New => "new",
    Triaged => "triaged",
    Investigating => "investigating",
    Resolved => "resolved",
    Rejected => "rejected",
    Spam => "spam",
    Duplicate => "duplicate",
});

impl TicketStatus {
    pub const fn is_open(self) -> bool {
        matches!(self, Self::New | Self::Triaged | Self::Investigating)
    }

    pub const fn requires_reason(self) -> bool {
        !self.is_open()
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        if self as u8 == next as u8 {
            return true;
        }
        match (self.is_open(), next.is_open()) {
            (true, _) => true,
            (false, true) => matches!(next, Self::Triaged),
            (false, false) => false,
        }
    }
}

string_enum!(Priority {
    Low => "low",
    Normal => "normal",
    High => "high",
    Urgent => "urgent",
});

string_enum!(EscalationKind {
    None => "none",
    Legal => "legal",
    Privacy => "privacy",
    Safety => "safety",
});

string_enum!(ActorType {
    Public => "public",
    Admin => "admin",
    Api => "api",
    Ai => "ai",
    System => "system",
});

/// Request body of `POST /b/tickets/api/admin/types`.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TicketTypeInput {
    /// Immutable lowercase slug, 2–48 characters, identifying the type.
    pub key: String,
    /// Title shown to reporters and reviewers, 2–80 characters.
    pub title: String,
    /// Short explanation of what belongs under this type, ≤500 characters.
    #[serde(default)]
    pub description: String,
    /// Longer guidance shown on the public form, ≤1000 characters.
    #[serde(default)]
    pub guidance: String,
    /// Priority applied to tickets filed under this type: `"low"`,
    /// `"normal"`, `"high"` or `"urgent"`.
    #[serde(default = "default_priority")]
    pub default_priority: String,
    /// Review track: `"none"`, `"legal"`, `"privacy"` or `"safety"`.
    #[serde(default = "default_escalation")]
    pub escalation_kind: String,
    /// Whether the public form offers this type.
    #[serde(default)]
    pub public_visible: bool,
    /// Whether a reporter must supply an email address and consent to a reply.
    #[serde(default)]
    pub requires_contact: bool,
    /// Whether the form asks for an evidence URL.
    #[serde(default)]
    pub requests_evidence: bool,
    /// Whether the type accepts new tickets.
    #[serde(default = "default_true")]
    pub active: bool,
    /// Ordering weight, between -1000000 and 1000000.
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone)]
pub struct ValidTicketType {
    pub key: String,
    pub title: String,
    pub description: String,
    pub guidance: String,
    pub default_priority: Priority,
    pub escalation_kind: EscalationKind,
    pub public_visible: bool,
    pub requires_contact: bool,
    pub requests_evidence: bool,
    pub active: bool,
    pub sort_order: i64,
}

impl TicketTypeInput {
    pub fn validate(self) -> Result<ValidTicketType, String> {
        validate_slug(&self.key, 2, 48, "type key")?;
        validate_chars(&self.title, 2, 80, "type title")?;
        validate_chars(&self.description, 0, 500, "type description")?;
        validate_chars(&self.guidance, 0, 1_000, "type guidance")?;
        if self.sort_order.abs() > 1_000_000 {
            return Err("sort_order must be between -1000000 and 1000000".into());
        }
        Ok(ValidTicketType {
            key: self.key,
            title: self.title.trim().to_string(),
            description: self.description.trim().to_string(),
            guidance: self.guidance.trim().to_string(),
            default_priority: self.default_priority.parse()?,
            escalation_kind: self.escalation_kind.parse()?,
            public_visible: self.public_visible,
            requires_contact: self.requires_contact,
            requests_evidence: self.requests_evidence,
            active: self.active,
            sort_order: self.sort_order,
        })
    }
}

/// Request body of `PATCH /b/tickets/api/admin/types/{id}`.
///
/// Every field is optional; an omitted field is left as stored.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TicketTypeUpdate {
    /// Accepted only when it equals the stored key — the key is immutable.
    #[serde(default)]
    pub key: Option<String>,
    /// New title, 2–80 characters.
    pub title: Option<String>,
    /// New description, ≤500 characters.
    pub description: Option<String>,
    /// New guidance, ≤1000 characters.
    pub guidance: Option<String>,
    /// `"low"`, `"normal"`, `"high"` or `"urgent"`.
    pub default_priority: Option<String>,
    /// `"none"`, `"legal"`, `"privacy"` or `"safety"`.
    pub escalation_kind: Option<String>,
    /// Whether the public form offers this type. Removing the last public type
    /// while public submissions are on is rejected with 409.
    pub public_visible: Option<bool>,
    /// Whether a reporter must supply an email address and consent to a reply.
    pub requires_contact: Option<bool>,
    /// Whether the form asks for an evidence URL.
    pub requests_evidence: Option<bool>,
    /// Whether the type accepts new tickets. Deactivating the last public type
    /// while public submissions are on is rejected with 409.
    pub active: Option<bool>,
    /// Ordering weight, between -1000000 and 1000000.
    pub sort_order: Option<i64>,
}

impl TicketTypeUpdate {
    pub fn validate(&self, stored_key: &str) -> Result<(), String> {
        if self.key.as_deref().is_some_and(|key| key != stored_key) {
            return Err("ticket type key is immutable".into());
        }
        if let Some(title) = &self.title {
            validate_chars(title, 2, 80, "type title")?;
        }
        if let Some(value) = &self.description {
            validate_chars(value, 0, 500, "type description")?;
        }
        if let Some(value) = &self.guidance {
            validate_chars(value, 0, 1_000, "type guidance")?;
        }
        if let Some(value) = &self.default_priority {
            value.parse::<Priority>()?;
        }
        if let Some(value) = &self.escalation_kind {
            value.parse::<EscalationKind>()?;
        }
        if self.sort_order.is_some_and(|v| v.abs() > 1_000_000) {
            return Err("sort_order must be between -1000000 and 1000000".into());
        }
        Ok(())
    }
}

// Not a wire type, and deliberately not serde-derived. Both intake paths build
// it in Rust from their own request struct - `AdminCreateTicketRequest` in
// `rest`, `PublicSubmissionRequest` (or a form body) in `public` - because the
// two surfaces accept different fields: only the public one may set reporter
// contact details. It previously carried `Deserialize`/`Serialize` that no
// caller used, which made it read like a third request contract.
/// The intake fields common to every ticket source, before validation.
#[derive(Debug, Clone)]
pub struct CreateTicketInput {
    pub type_id: String,
    pub subject: String,
    pub description: String,
    pub source_path: String,
    pub subject_type: String,
    pub subject_id: String,
    pub evidence_url: String,
    pub reporter_email: String,
    pub reporter_wants_reply: bool,
    pub priority: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidCreateTicket {
    pub type_id: String,
    pub subject: String,
    pub description: String,
    pub source_path: String,
    pub subject_type: String,
    pub subject_id: String,
    pub evidence_url: String,
    pub reporter_email: String,
    pub reporter_wants_reply: bool,
    pub priority: Option<Priority>,
}

impl CreateTicketInput {
    pub fn validate(self) -> Result<ValidCreateTicket, String> {
        validate_identifier(&self.type_id, 1, 100, "type id")?;
        validate_chars(&self.subject, 5, 160, "subject")?;
        validate_chars(&self.description, 20, 4_000, "description")?;
        validate_source_path(&self.source_path)?;
        if !self.subject_type.is_empty() {
            validate_slug(&self.subject_type, 1, 64, "subject type")?;
        }
        if !self.subject_id.is_empty() {
            validate_identifier(&self.subject_id, 1, 160, "subject id")?;
        }
        validate_evidence_url(&self.evidence_url)?;
        validate_email(&self.reporter_email)?;
        if self.reporter_wants_reply && self.reporter_email.trim().is_empty() {
            return Err("reply consent requires an email address".into());
        }
        Ok(ValidCreateTicket {
            type_id: self.type_id,
            subject: self.subject.trim().to_string(),
            description: self.description.trim().to_string(),
            source_path: self.source_path.trim().to_string(),
            subject_type: self.subject_type.trim().to_string(),
            subject_id: self.subject_id.trim().to_string(),
            evidence_url: self.evidence_url.trim().to_string(),
            reporter_email: self.reporter_email.trim().to_string(),
            reporter_wants_reply: self.reporter_wants_reply,
            priority: self.priority.as_deref().map(str::parse).transpose()?,
        })
    }
}

/// Request body of `PATCH /b/tickets/api/admin/tickets/{id}`.
///
/// Only workflow fields are mutable. The original report is immutable: no
/// field here can edit the subject, description, evidence URL or reporter
/// contact details.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowUpdate {
    /// New workflow state: `"new"`, `"triaged"`, `"investigating"`,
    /// `"resolved"`, `"rejected"`, `"spam"` or `"duplicate"`. A closed ticket
    /// can only reopen to `"triaged"`.
    pub status: Option<String>,
    /// `"low"`, `"normal"`, `"high"` or `"urgent"`.
    pub priority: Option<String>,
    /// Id of the reviewer to assign, or `""` to unassign.
    pub assignee_id: Option<String>,
    /// Id of the ticket this one duplicates. Required when moving to
    /// `"duplicate"`, and must name an existing, different ticket.
    pub duplicate_of: Option<String>,
    /// Suspend retention for this ticket. A ticket under hold never expires.
    pub legal_hold: Option<bool>,
    /// Why the change was made, ≤4000 characters. Required when closing a
    /// ticket (`"resolved"`, `"rejected"`, `"spam"`, `"duplicate"`) and
    /// recorded on the audit timeline with the change it explains. A reason
    /// sent with no change is refused with 400: a comment is a note.
    #[serde(default)]
    pub reason: String,
}

impl WorkflowUpdate {
    pub fn validate(
        &self,
        ticket_id: &str,
        current: TicketStatus,
    ) -> Result<Option<TicketStatus>, String> {
        let next = self.status.as_deref().map(str::parse).transpose()?;
        if let Some(next) = next {
            if !current.can_transition_to(next) {
                return Err(format!("cannot move {current} to {next}"));
            }
            if next != current && next.requires_reason() && self.reason.trim().is_empty() {
                return Err(format!("a reason is required when moving to {next}"));
            }
            if next == TicketStatus::Duplicate && current != TicketStatus::Duplicate {
                let duplicate = self
                    .duplicate_of
                    .as_deref()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| "duplicate_of is required".to_string())?;
                if duplicate == ticket_id {
                    return Err("a ticket cannot duplicate itself".into());
                }
            }
        }
        if let Some(duplicate) = self
            .duplicate_of
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            validate_identifier(duplicate, 1, 160, "duplicate target")?;
            if duplicate == ticket_id {
                return Err("a ticket cannot duplicate itself".into());
            }
        }
        if let Some(priority) = &self.priority {
            priority.parse::<Priority>()?;
        }
        if let Some(assignee) = &self.assignee_id {
            if !assignee.is_empty() {
                validate_identifier(assignee, 1, 160, "assignee id")?;
            }
        }
        validate_chars(&self.reason, 0, 4_000, "reason")?;
        Ok(next)
    }
}

/// Request body of `POST /b/tickets/api/admin/tickets/{id}/analyses`.
///
/// An analysis is advisory and append-only. Posting one records a suggestion;
/// it never changes the ticket's type, priority or status.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalysisInput {
    /// Name of the system producing the analysis, 1–80 characters.
    pub source: String,
    /// Model identifier, ≤160 characters.
    pub model: Option<String>,
    /// Prompt version, ≤80 characters.
    #[serde(default)]
    pub prompt_version: String,
    /// The analysis itself, 1–4000 characters.
    pub summary: String,
    /// Ticket type to suggest. Must name an active type.
    pub suggested_type_id: Option<String>,
    /// `"low"`, `"normal"`, `"high"` or `"urgent"`.
    pub suggested_priority: Option<String>,
    /// Reported confidence, between 0 and 1 inclusive.
    pub confidence: f64,
    /// Proposed follow-up actions, ≤8192 bytes once encoded. Stored verbatim;
    /// the block neither interprets nor executes them.
    #[serde(default = "empty_array")]
    #[schemars(with = "Vec<serde_json::Value>")]
    pub suggested_actions: serde_json::Value,
}

impl AnalysisInput {
    pub fn validate(&self) -> Result<(), String> {
        validate_identifier(&self.source, 1, 80, "analysis source")?;
        if let Some(model) = &self.model {
            validate_chars(model, 1, 160, "model")?;
        }
        validate_chars(&self.prompt_version, 0, 80, "prompt version")?;
        validate_chars(&self.summary, 1, 4_000, "analysis summary")?;
        if let Some(id) = &self.suggested_type_id {
            validate_identifier(id, 1, 100, "suggested type id")?;
        }
        if let Some(priority) = &self.suggested_priority {
            priority.parse::<Priority>()?;
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err("confidence must be between 0 and 1".into());
        }
        validate_json_array(&self.suggested_actions, "suggested actions")
    }
}

pub fn validate_note(note: &str) -> Result<(), String> {
    validate_chars(note, 1, 4_000, "note")
}

pub fn validate_metadata(value: &serde_json::Value) -> Result<(), String> {
    if !value.is_object() {
        return Err("metadata must be a JSON object".into());
    }
    validate_json_size(value, "metadata")
}

fn validate_json_array(value: &serde_json::Value, label: &str) -> Result<(), String> {
    if !value.is_array() {
        return Err(format!("{label} must be a JSON array"));
    }
    validate_json_size(value, label)
}

fn validate_json_size(value: &serde_json::Value, label: &str) -> Result<(), String> {
    let size = serde_json::to_vec(value)
        .map_err(|_| format!("{label} could not be serialized"))?
        .len();
    if size > 8 * 1_024 {
        return Err(format!("{label} exceeds 8192 bytes"));
    }
    Ok(())
}

pub fn validate_source_path(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    if value.len() > 512
        || !value.starts_with('/')
        || value.starts_with("//")
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err("source path must be a safe same-site relative path".into());
    }
    Ok(())
}

pub fn validate_evidence_url(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Ok(());
    }
    if value.len() > 1_024 {
        return Err("evidence URL exceeds 1024 characters".into());
    }
    let url = url::Url::parse(value).map_err(|_| "evidence URL is invalid".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err("evidence URL must be an http(s) URL without credentials".into());
    }
    Ok(())
}

pub fn validate_email(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    if value.len() > 254
        || value.chars().any(char::is_whitespace)
        || value.matches('@').count() != 1
    {
        return Err("email address is invalid".into());
    }
    let (local, domain) = value.split_once('@').unwrap_or_default();
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err("email address is invalid".into());
    }
    Ok(())
}

fn validate_slug(value: &str, min: usize, max: usize, label: &str) -> Result<(), String> {
    let len = value.len();
    let valid = (min..=max).contains(&len)
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if !valid {
        return Err(format!(
            "{label} must be a {min}-{max} character lowercase slug"
        ));
    }
    Ok(())
}

fn validate_identifier(value: &str, min: usize, max: usize, label: &str) -> Result<(), String> {
    if !(min..=max).contains(&value.len())
        || value
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')))
    {
        return Err(format!("{label} contains unsupported characters"));
    }
    Ok(())
}

fn validate_chars(value: &str, min: usize, max: usize, label: &str) -> Result<(), String> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if !(min..=max).contains(&len) || trimmed.chars().any(|c| c == '\0') {
        return Err(format!(
            "{label} must contain between {min} and {max} characters"
        ));
    }
    Ok(())
}

fn default_priority() -> String {
    "normal".into()
}

fn default_escalation() -> String {
    "none".into()
}

const fn default_true() -> bool {
    true
}

fn empty_array() -> serde_json::Value {
    serde_json::json!([])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_enforce_closed_reopen_policy() {
        assert!(TicketStatus::New.can_transition_to(TicketStatus::Resolved));
        assert!(TicketStatus::Resolved.can_transition_to(TicketStatus::Triaged));
        assert!(!TicketStatus::Resolved.can_transition_to(TicketStatus::New));
        assert!(!TicketStatus::Resolved.can_transition_to(TicketStatus::Rejected));
    }

    #[test]
    fn source_path_rejects_absolute_and_protocol_relative_values() {
        assert!(validate_source_path("/activities/42?x=1").is_ok());
        assert!(validate_source_path("https://evil.test").is_err());
        assert!(validate_source_path("//evil.test/path").is_err());
        assert!(validate_source_path("/ok\\bad").is_err());
    }

    #[test]
    fn evidence_url_is_http_only_and_has_no_userinfo() {
        assert!(validate_evidence_url("https://example.test/evidence").is_ok());
        assert!(validate_evidence_url("javascript:alert(1)").is_err());
        assert!(validate_evidence_url("https://user:pass@example.test").is_err());
    }

    #[test]
    fn workflow_closure_and_duplicate_require_context() {
        let update = WorkflowUpdate {
            status: Some("duplicate".into()),
            priority: None,
            assignee_id: None,
            duplicate_of: Some("other".into()),
            legal_hold: None,
            reason: "Same report".into(),
        };
        assert_eq!(
            update.validate("self", TicketStatus::New).unwrap(),
            Some(TicketStatus::Duplicate)
        );

        let mut invalid = update;
        invalid.reason.clear();
        assert!(invalid.validate("self", TicketStatus::New).is_err());
    }

    #[test]
    fn analysis_requires_bounded_array_and_confidence() {
        let mut input = AnalysisInput {
            source: "agent".into(),
            model: Some("model".into()),
            prompt_version: "v1".into(),
            summary: "Looks outdated".into(),
            suggested_type_id: None,
            suggested_priority: Some("high".into()),
            confidence: 0.8,
            suggested_actions: serde_json::json!([{"kind": "verify"}]),
        };
        assert!(input.validate().is_ok());
        input.confidence = 1.1;
        assert!(input.validate().is_err());
    }
}
