//! Observability hooks wired into a `Wafer` instance: logs every flow
//! start / block end / flow end via the `tracing` crate. Call once after
//! `Wafer` construction, before `wafer.start()`.

use wafer_run::Wafer;

pub fn register_observability_hooks(wafer: &mut Wafer) {
    wafer.hooks.on_flow_start(|flow_id, _msg| {
        tracing::info_span!("flow", flow = %flow_id).in_scope(|| {});
    });

    wafer.hooks.on_block_end(|obs_ctx, duration| {
        tracing::debug!(
            flow   = %obs_ctx.flow_id,
            block  = %obs_ctx.block_name,
            trace  = %obs_ctx.trace_id,
            ms     = duration.as_millis() as u64,
            "block executed"
        );
    });

    wafer.hooks.on_flow_end(|flow_id, duration| {
        tracing::info!(
            flow   = %flow_id,
            ms     = duration.as_millis() as u64,
            "flow completed"
        );
    });
}
