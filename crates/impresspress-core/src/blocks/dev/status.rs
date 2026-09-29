//! `GET /b/dev/api/status` — what is live, what is in flight.
//!
//! The `/b/dev` page polls this every ~300 ms while a mutating tool call is
//! outstanding (design §7.5), which is why the whole block answers
//! `Cache-Control: no-store`: a cached status is a progress panel that never
//! moves.

use wafer_run::{context::Context, OutputStream, WaferError};

use super::{
    contracts::{ActivationView, ActiveBlockView, StatusResponse},
    gc, generation, no_store, no_store_db_error_internal, repo, seed, DevShared,
    WAFER_GUEST_VERSION,
};

/// Answer the status endpoint.
pub async fn handle(ctx: &dyn Context, shared: &DevShared) -> OutputStream {
    match build(ctx, shared).await {
        Ok(response) => no_store().json(&response),
        Err(e) => no_store_db_error_internal(e, "dev sandbox status"),
    }
}

async fn build(ctx: &dyn Context, shared: &DevShared) -> Result<StatusResponse, WaferError> {
    let state = repo::runtime_state::read(ctx).await?;

    // The block manifest is stored as the same `DynamicBlockSpec` list the
    // runtime is rebuilt from, so the active block set needs no separate
    // record — it is a projection of the generation that is live.
    let active = generation::active_from(ctx, &state).await?;

    let activation = state
        .desired_generation_id
        .map(|generation_id| ActivationView {
            generation_id,
            phase: state.activation_phase,
            detail: String::new(),
        });

    Ok(StatusResponse {
        active_generation: active
            .as_ref()
            .map(|(row, manifest)| generation::summarize(row, manifest)),
        runtime_generation: shared.control.runtime_generation(),
        blocks: active
            .as_ref()
            .map(|(_row, manifest)| {
                manifest
                    .blocks
                    .iter()
                    .map(ActiveBlockView::from_spec)
                    .collect()
            })
            .unwrap_or_default(),
        activation,
        // From the workspace counters and the builds table on every poll, not
        // from a walk of the stores (`gc::storage_usage` says why). The page
        // polls this while a tool call is outstanding, so the figures move as
        // the collector works rather than only after the panel is reopened.
        storage: gc::storage_usage(ctx, shared).await?,
        wafer_guest_version: WAFER_GUEST_VERSION,
        // One indexed read of a `UNIQUE` column, on the same poll — cheap in
        // the way a store listing is not, and the difference between an empty
        // sandbox that says why and one that does not.
        seed_error: seed::last_failure(ctx).await?,
    })
}
