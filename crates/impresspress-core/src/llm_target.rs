//! The wire shape of the llm block's inter-block discovery route,
//! `GET /b/llm/api/internal/default-target`.
//!
//! One type, used by the block that publishes the body and by every block
//! that reads it. It lives at the crate root for the same reason
//! [`crate::llm_wire`] does: the two sides do not share a cargo feature. The
//! llm block is `block-llm`, the vector block is `block-vector`, and a wasm32
//! build enables the second without the first — so neither may reach the
//! other through a Rust path, and the only thing they can share is a shape
//! that depends on neither.
//!
//! It exists because the shape had been hand-built and hand-parsed as
//! untyped JSON in three places (the route, the caller, and the caller's test
//! double), which is three copies free to drift. A field added to one of them
//! and missing from another does not fail: it silently becomes "no default
//! LLM model configured", a cause that is not true, logged where an operator
//! will believe it.

use serde::{Deserialize, Serialize};

/// The llm block's config key for the output-token budget it publishes as
/// [`DefaultTarget::max_tokens`].
///
/// Declared here rather than in `blocks::llm` for the reason this module
/// exists: the vector block names it when the budget it receives is
/// unusable, and it cannot reach `blocks::llm` through a Rust path.
pub const DEFAULT_MAX_TOKENS_VAR: &str = "IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS";

/// The default LLM target as it travels between blocks.
///
/// Every field is optional on the wire because "nothing is configured" is a
/// `200` with nulls, not an error — a caller with no LLM takes a degraded
/// path rather than failing. [`resolve`](Self::resolve) is what turns the
/// permissive wire shape into the values a completion needs, and names what
/// is missing when it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DefaultTarget {
    /// Backend id to route the completion to.
    pub provider: Option<String>,
    /// Model id within that backend.
    pub model: Option<String>,
    /// Output-token budget to send with the completion. Carried here rather
    /// than read by the caller because it is the llm block's own
    /// configuration variable, and WRAP scopes an `IMPRESSPRESS__LLM__*` key
    /// to the block that owns it.
    pub max_tokens: Option<u32>,
}

/// A target with everything a completion needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// Backend id to route the completion to.
    pub provider: String,
    /// Model id within that backend.
    pub model: String,
    /// Output-token budget to send with the completion.
    pub max_tokens: u32,
}

/// Why a published target cannot be used. The two arms are different events
/// and read as different causes in a log: one is a deployment that has no LLM
/// configured, the other is a deployment that has one and answered with a
/// body this caller cannot use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetGap {
    /// No provider or model is configured — the expected state on a
    /// deployment that does not use an LLM.
    NotConfigured,
    /// A provider and model are configured, but the budget is absent or zero.
    /// Not an operator's doing: the route always publishes one, so this is
    /// the two sides of this contract disagreeing.
    MissingBudget,
}

impl DefaultTarget {
    /// Path of the route this body belongs to. Shared so the publisher's
    /// guard and the caller's request cannot name different strings.
    pub const RESOURCE: &'static str = "/b/llm/api/internal/default-target";

    /// The body for a deployment with no LLM configured: all nulls.
    pub fn unconfigured() -> Self {
        Self::default()
    }

    /// The body for a configured deployment.
    pub fn configured(provider: &str, model: &str, max_tokens: u32) -> Self {
        Self {
            provider: Some(provider.to_string()),
            model: Some(model.to_string()),
            max_tokens: Some(max_tokens),
        }
    }

    /// Turn the wire shape into a usable target, or say what is missing.
    ///
    /// An empty string counts as absent: the route published `""` for an
    /// unset provider or model before it published `null`, and a deployment
    /// upgrading across that change must not read an empty backend id as a
    /// real one.
    pub fn resolve(self) -> Result<ResolvedTarget, TargetGap> {
        let provider = self.provider.filter(|v| !v.is_empty());
        let model = self.model.filter(|v| !v.is_empty());
        let (Some(provider), Some(model)) = (provider, model) else {
            return Err(TargetGap::NotConfigured);
        };
        match self.max_tokens {
            Some(max_tokens) if max_tokens > 0 => Ok(ResolvedTarget {
                provider,
                model,
                max_tokens,
            }),
            _ => Err(TargetGap::MissingBudget),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_target_resolves_to_its_three_values() {
        let target = DefaultTarget::configured("openai-main", "o3", 4096);
        assert_eq!(
            target.resolve(),
            Ok(ResolvedTarget {
                provider: "openai-main".into(),
                model: "o3".into(),
                max_tokens: 4096,
            })
        );
    }

    #[test]
    fn nulls_and_empty_strings_are_both_not_configured() {
        assert_eq!(
            DefaultTarget::unconfigured().resolve(),
            Err(TargetGap::NotConfigured)
        );
        assert_eq!(
            DefaultTarget {
                provider: Some(String::new()),
                model: Some(String::new()),
                max_tokens: Some(4096),
            }
            .resolve(),
            Err(TargetGap::NotConfigured)
        );
    }

    /// A configured target whose budget is missing or zero is NOT "no LLM
    /// configured" — reporting it that way is what sent an operator looking
    /// at a variable that was set correctly all along.
    #[test]
    fn a_configured_target_without_a_budget_is_its_own_gap() {
        for max_tokens in [None, Some(0)] {
            assert_eq!(
                DefaultTarget {
                    provider: Some("openai-main".into()),
                    model: Some("o3".into()),
                    max_tokens,
                }
                .resolve(),
                Err(TargetGap::MissingBudget),
                "max_tokens={max_tokens:?}"
            );
        }
    }

    /// The body is plain JSON on the wire: the publisher serializes this type
    /// and the caller deserializes it, so a field renamed on one side has to
    /// fail here rather than degrade in production.
    #[test]
    fn the_wire_body_is_the_three_named_fields() {
        let json = serde_json::to_value(DefaultTarget::configured("p", "m", 7)).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({ "provider": "p", "model": "m", "max_tokens": 7 })
        );
        assert_eq!(
            serde_json::from_value::<DefaultTarget>(json).expect("round-trip"),
            DefaultTarget::configured("p", "m", 7)
        );
    }
}
