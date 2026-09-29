//! `/b/dev/api/generations*` — reading the publication ledger and rolling
//! back to an entry in it.
//!
//! # Rolling back is publishing forwards
//!
//! History is append-only (design §7.2): a rollback does not move the ledger's
//! head backwards, it stages a *new* generation carrying an old one's
//! manifests. That is what makes "roll back the rollback" an ordinary
//! operation, and what keeps every id in the ledger meaning exactly one set of
//! bytes.
//!
//! The workspace follows, because it has to. The workspace — not the ledger —
//! is what the next site write builds its manifest from, so a rollback that
//! republished the old site and left the workspace holding the new one would
//! be silently undone by the very next keystroke. That adoption happens inside
//! the activation queue, under the same lease as the publish
//! (`activation::activate`), so no site write can dequeue between the two.

use wafer_run::{context::Context, ErrorCode, Message, OutputStream, WaferError};

use super::{
    activation::{self, ActivationIntent},
    contracts::{
        ActivationResponse, GenerationDetail, GenerationListQuery, GenerationListResponse,
    },
    generation, no_store, no_store_db_error, no_store_db_error_internal, no_store_error,
    repo::{self, generations::GenerationCause},
    retention, DevShared,
};

/// Default page size: the retention window, so the default listing is exactly
/// the set of generations that can still be rolled back to.
const DEFAULT_LIMIT: u32 = retention::RETAINED_GENERATIONS as u32;

/// `GET /b/dev/api/generations` — the ledger, newest first.
pub async fn handle_list(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let limit = GenerationListQuery::from_message(msg)
        .limit
        .unwrap_or(DEFAULT_LIMIT);
    match list(ctx, limit).await {
        Ok(response) => no_store().json(&response),
        // The listing names no row, so a `NotFound` from it is a missing
        // ledger table — a 500, not a 404 telling the caller they have no
        // generations. A refusal is still a 403.
        Err(e) => no_store_db_error_internal(e, "dev generation list"),
    }
}

async fn list(ctx: &dyn Context, limit: u32) -> Result<GenerationListResponse, WaferError> {
    let rows = repo::generations::list_recent(ctx, limit).await?;
    let mut generations = Vec::with_capacity(rows.len());
    for row in &rows {
        let manifest = generation::from_row(row)?;
        generations.push(generation::summarize(row, &manifest));
    }
    Ok(GenerationListResponse { generations })
}

/// `GET /b/dev/api/generations/{id}` — one generation, its manifest and what
/// it changed.
pub async fn handle_detail(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let Some(id) = generation_id(msg) else {
        return no_store_error(ErrorCode::InvalidArgument, "the path names no generation");
    };
    match detail(ctx, &id).await {
        Ok(response) => no_store().json(&response),
        // A generation id that is not in the ledger is a 404 the caller can
        // act on, not an internal failure — and a WRAP refusal on the
        // ledger is a 403, which is what the shared classification adds.
        Err(e) => no_store_db_error(e, &format!("no generation {id:?}"), "dev generation detail"),
    }
}

async fn detail(ctx: &dyn Context, id: &str) -> Result<GenerationDetail, WaferError> {
    let (row, manifest) = generation::load(ctx, id).await?;
    // A parent that is no longer in the ledger is a MISSING BASELINE, not a
    // missing generation. Propagating its `NotFound` through the same `?` as
    // the row above would surface at `handle_detail` as `"no generation {id}"`
    // naming the CHILD — a 404 for a generation the list response is still
    // showing, sending the caller after the wrong id. Retention is bounded, so
    // a parent outliving its child is expected rather than exceptional: the
    // diff simply has nothing to be a diff from, exactly as for a generation
    // that never had a parent.
    let parent = match row.parent_id.as_deref() {
        Some(parent_id) => match generation::load(ctx, parent_id).await {
            Ok((_row, manifest)) => Some(manifest),
            Err(e) if e.code == ErrorCode::NotFound => None,
            Err(e) => return Err(e),
        },
        None => None,
    };
    Ok(GenerationDetail {
        summary: generation::summarize(&row, &manifest),
        diff_from_parent: generation::diff(parent.as_ref(), &manifest),
        manifest,
    })
}

/// `POST /b/dev/api/generations/{id}/rollback` — republish an earlier
/// generation as a new one.
pub async fn handle_rollback(ctx: &dyn Context, shared: &DevShared, msg: &Message) -> OutputStream {
    let Some(id) = generation_id(msg) else {
        return no_store_error(ErrorCode::InvalidArgument, "the path names no generation");
    };

    let target = match generation::load(ctx, &id).await {
        Ok((_row, manifest)) => manifest,
        Err(e) => {
            return no_store_db_error(
                e,
                &format!("no generation {id:?}"),
                "dev generation rollback",
            )
        }
    };

    // A new generation carrying the target's contents — not the target row
    // re-activated. The intent carries the target manifest rather than its id
    // because a ledger row is immutable: nothing can have changed it between
    // the lookup above (which is what answers the `404`) and the dequeue. The
    // new id and parent are assigned inside the queue, which is what keeps the
    // history a straight line.
    let intent = ActivationIntent::Rollback {
        target: target.clone(),
    };
    // A rollback replaces the whole block set, exactly as a compile or a
    // removal does — so it takes the same lock, for the same reason. Without
    // it, a compile that read the active block set just before this rollback
    // ran would compose its own set from a snapshot the rollback has already
    // replaced, and would put the rolled-back block straight back.
    //
    // The workspace adoption is *not* here: it happens inside the queue, as
    // part of applying the `Rollback` intent, so that it and the publish land
    // under one lease. See `activation::activate`.
    let _compiling = shared.compile.lock().await;
    let outcome = match activation::request(ctx, shared, GenerationCause::Rollback, intent).await {
        Ok(outcome) => outcome,
        Err(e) => return e.into_response(),
    };

    no_store().json(&ActivationResponse {
        generation: outcome.generation,
        progress: outcome.progress,
    })
}

/// The `{id}` the route bound, or `None` when it is empty.
fn generation_id(msg: &Message) -> Option<String> {
    let id = msg.var("id");
    (!id.is_empty()).then(|| id.to_string())
}
