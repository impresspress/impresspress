//! The single-row activation journal (`impresspress__dev__runtime_state`).
//!
//! Written before and after every phase of an activation so a service worker
//! that dies mid-swap can converge on restart: a non-empty
//! `desired_generation_id` is a recovery journal, not a leftover.

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::util::RecordExt;

pub const TABLE: &str = "impresspress__dev__runtime_state";

/// Primary-key value of the one row this table holds. The column is
/// `singleton_id` (not `id`), so reads and writes go through the
/// field-scoped client helpers rather than `db::get`/`db::update`.
const SINGLETON_COLUMN: &str = "singleton_id";
const SINGLETON_ID: i64 = 1;

/// Where an in-flight activation has got to.
///
/// `Active` appears only in progress lists — once the swap is journalled the
/// activation is over and the row rests at `Idle`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPhase {
    /// Nothing in flight.
    Idle,
    /// Hashes, manifests and guests are being checked.
    Validating,
    /// A candidate runtime is being constructed from the block set.
    BuildingRuntime,
    /// The site files are being written into `wafer-run/web/site`.
    Publishing,
    /// The generation is live.
    Active,
    /// The activation was abandoned; the previous generation is still live.
    Failed,
}

impl ActivationPhase {
    /// Canonical string form (matches the serde representation and the
    /// column's `CHECK` constraint).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Validating => "validating",
            Self::BuildingRuntime => "building_runtime",
            Self::Publishing => "publishing",
            Self::Active => "active",
            Self::Failed => "failed",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for an unrecognized value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "validating" => Some(Self::Validating),
            "building_runtime" => Some(Self::BuildingRuntime),
            "publishing" => Some(Self::Publishing),
            "active" => Some(Self::Active),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// The journal's decoded contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeState {
    /// The generation currently serving, or `None` on a fresh instance.
    pub active_generation_id: Option<String>,
    /// The generation an in-flight (or interrupted) activation is converging
    /// on. Non-empty on startup means recovery is owed.
    pub desired_generation_id: Option<String>,
    /// Where that activation had got to.
    pub activation_phase: ActivationPhase,
    /// Monotonic counter, incremented once per completed activation.
    pub generation: u64,
}

impl Default for RuntimeState {
    /// The state the migration seeds: nothing active, nothing desired, idle.
    fn default() -> Self {
        Self {
            active_generation_id: None,
            desired_generation_id: None,
            activation_phase: ActivationPhase::Idle,
            generation: 0,
        }
    }
}

/// Read the journal.
///
/// An unrecognized `activation_phase` is an error, never silently `Idle`: a
/// phase this build cannot name means the row was written by a different
/// version, and converging on it blind is how a half-swapped runtime becomes
/// permanent.
pub async fn read(ctx: &dyn Context) -> Result<RuntimeState, WaferError> {
    decode(
        &db::get_by_field(
            ctx,
            TABLE,
            SINGLETON_COLUMN,
            serde_json::json!(SINGLETON_ID),
        )
        .await?,
    )
}

/// Decode the journal row.
///
/// Split out of [`read`] so the unknown-phase arm is reachable from a test:
/// the column carries a `CHECK` constraint listing exactly the six phases, so
/// a row holding anything else cannot be written through this schema at all —
/// the arm exists for a row written by a *different build* of the block, which
/// only a synthesized `Record` can stand in for.
fn decode(record: &db::Record) -> Result<RuntimeState, WaferError> {
    let phase_text = record.str_field("activation_phase");
    let activation_phase = ActivationPhase::parse(phase_text).ok_or_else(|| {
        WaferError::new(
            ErrorCode::Internal,
            format!("{TABLE}: unknown activation_phase `{phase_text}`"),
        )
    })?;

    Ok(RuntimeState {
        active_generation_id: record.opt_str_field("active_generation_id"),
        desired_generation_id: record.opt_str_field("desired_generation_id"),
        activation_phase,
        generation: record.u64_field("generation"),
    })
}

/// Overwrite the journal with `state`, stamping `updated_at`.
pub async fn write(ctx: &dyn Context, state: &RuntimeState) -> Result<(), WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "active_generation_id": state.active_generation_id,
        "desired_generation_id": state.desired_generation_id,
        "activation_phase": state.activation_phase.as_str(),
        "generation": state.generation,
        "updated_at": super::now(),
    }));

    db::update_by_filters(
        ctx,
        TABLE,
        vec![Filter {
            field: SINGLETON_COLUMN.to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(SINGLETON_ID),
        }],
        data,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{blocks::dev::test_support::FakeControl, test_support::TestContext};

    fn record(phase: &str) -> db::Record {
        let mut data = HashMap::new();
        data.insert("singleton_id".to_string(), serde_json::json!(1));
        data.insert("active_generation_id".to_string(), serde_json::Value::Null);
        data.insert("desired_generation_id".to_string(), serde_json::Value::Null);
        data.insert("activation_phase".to_string(), serde_json::json!(phase));
        data.insert("generation".to_string(), serde_json::json!(0));
        db::Record {
            id: String::new(),
            data: data.into_iter().collect(),
        }
    }

    #[tokio::test]
    async fn migration_seeds_the_journal_at_rest() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        assert_eq!(read(&ctx).await.expect("read"), RuntimeState::default());
    }

    #[tokio::test]
    async fn write_then_read_round_trips_every_field() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        let state = RuntimeState {
            active_generation_id: Some("gen-active".to_string()),
            desired_generation_id: Some("gen-desired".to_string()),
            activation_phase: ActivationPhase::BuildingRuntime,
            generation: 42,
        };
        write(&ctx, &state).await.expect("write");
        assert_eq!(read(&ctx).await.expect("read"), state);
    }

    /// Clearing the journal must actually null the columns, not leave the
    /// previous ids behind — a stale `desired_generation_id` reads as a
    /// recovery owed on the next boot (design §7.3).
    #[tokio::test]
    async fn write_clears_the_ids_it_is_given_as_none() {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        write(
            &ctx,
            &RuntimeState {
                active_generation_id: Some("gen-1".to_string()),
                desired_generation_id: Some("gen-2".to_string()),
                activation_phase: ActivationPhase::Publishing,
                generation: 1,
            },
        )
        .await
        .expect("write");

        let cleared = RuntimeState {
            active_generation_id: Some("gen-2".to_string()),
            desired_generation_id: None,
            activation_phase: ActivationPhase::Idle,
            generation: 2,
        };
        write(&ctx, &cleared).await.expect("write");
        assert_eq!(read(&ctx).await.expect("read"), cleared);
    }

    #[test]
    fn decode_accepts_every_phase_the_column_allows() {
        for phase in [
            ActivationPhase::Idle,
            ActivationPhase::Validating,
            ActivationPhase::BuildingRuntime,
            ActivationPhase::Publishing,
            ActivationPhase::Active,
            ActivationPhase::Failed,
        ] {
            let state = decode(&record(phase.as_str())).expect("decode");
            assert_eq!(state.activation_phase, phase);
        }
    }

    /// A phase this build cannot name is an error, never silently `Idle`:
    /// converging on a half-swapped runtime is how it becomes permanent.
    #[test]
    fn decode_refuses_an_unknown_phase_rather_than_defaulting() {
        let err = decode(&record("teleporting")).expect_err("unknown phase must fail");
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(
            err.message.contains("teleporting"),
            "the message must name the value it could not read: {}",
            err.message,
        );
    }
}
