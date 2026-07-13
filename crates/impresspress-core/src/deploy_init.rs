//! Deploy-time init funnel: `boot()`'s ordering invariants with per-step
//! outcome capture, for the `/_deploy/init` endpoint. Runs once per deploy
//! (invoked by the CLI against the freshly-uploaded worker version), so the
//! request path never migrates or seeds.

use serde::Serialize;
use wafer_run::{RuntimeError, Wafer};

use crate::{
    blocks::storage::ImpresspressStorageBlock,
    builder::{post_start, BootHooks},
};

#[derive(Debug, Serialize)]
pub struct StepOutcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BlockInitOutcome {
    pub block: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeployInitReport {
    pub sealed: bool,
    pub seed: StepOutcome,
    pub blocks: Vec<BlockInitOutcome>,
    /// True iff every step and every block init succeeded.
    pub ok: bool,
}

/// `seal → init_block(admin) → seed hook → init every block (captured) →
/// post_start`, returning a per-step report. Ordering matches
/// [`crate::builder::boot`]: admin first (its migrations create the
/// variables / block_settings tables everything else stamps into).
///
/// `Err` only for pre-init failures (seal); block-level failures are
/// captured in the report with `ok: false` so the caller can render the
/// full picture and fail the deploy.
pub async fn deploy_init(
    wafer: &mut Wafer,
    storage_block: &ImpresspressStorageBlock,
    hooks: &dyn BootHooks,
) -> Result<DeployInitReport, RuntimeError> {
    wafer.seal().await?;

    let admin_id = crate::blocks::admin::ADMIN_BLOCK_ID;
    let mut blocks = Vec::new();
    let admin_outcome = match wafer.init_block(admin_id).await {
        Ok(_) => BlockInitOutcome {
            block: admin_id.to_string(),
            ok: true,
            error: None,
        },
        Err(e) => BlockInitOutcome {
            block: admin_id.to_string(),
            ok: false,
            error: Some(e.to_string()),
        },
    };
    blocks.push(admin_outcome);

    let seed = match hooks.seed_after_admin_init(wafer).await {
        Ok(()) => StepOutcome {
            ok: true,
            error: None,
        },
        Err(e) => StepOutcome {
            ok: false,
            error: Some(e),
        },
    };

    // Init every registered block individually so each outcome is captured.
    // Admin is a slot-cached no-op on its second pass. Iterates the
    // registration keys (`block_names`) — the exact set `init_block`
    // resolves — rather than each block's self-reported `info().name`.
    for name in wafer.block_names() {
        if name == admin_id {
            continue;
        }
        match wafer.init_block(&name).await {
            Ok(_) => blocks.push(BlockInitOutcome {
                block: name,
                ok: true,
                error: None,
            }),
            Err(e) => blocks.push(BlockInitOutcome {
                block: name,
                ok: false,
                error: Some(e.to_string()),
            }),
        }
    }

    post_start(wafer, storage_block);

    let ok = seed.ok && blocks.iter().all(|b| b.ok);
    Ok(DeployInitReport {
        sealed: true,
        seed,
        blocks,
        ok,
    })
}
