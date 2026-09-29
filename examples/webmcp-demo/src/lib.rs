//! The WebMCP demo Worker: impresspress with the products block, deployed
//! to Cloudflare so a WebMCP-capable browser can register the storefront
//! tools from a public URL.
//!
//! There is deliberately nothing here but the three Worker entry points and
//! the bootstrap-admin secrets they pass on.
//! Every page of this site carries `ui/assets/webmcp.js`, which fetches
//! `/b/webmcp/manifest.json` (filtered to the visitor's auth level) and
//! registers each tool with `document.modelContext`. The tools themselves
//! are the products block's storefront endpoints, annotated with
//! `.agent_tool(...)` in `impresspress-core/src/blocks/products/mod.rs`.

/// The first-admin credentials, from the Worker secrets of the same names
/// when they are set, for [`impresspress_cloudflare::run_with_config`].
///
/// On Cloudflare a `WAFER_RUN_SHARED__*` key is read from D1's variables
/// table, which an admin edits and which has no rows before the first deploy
/// creates it. Request config is how a consumer hands its blocks a Worker
/// secret instead: with both set, the auth block's `Init` in the first
/// `/_deploy/prepare` creates this account as the admin (the same bootstrap
/// `impresspress serve` runs from the process environment), and does
/// nothing once any account exists.
#[cfg(feature = "target-cloudflare")]
fn bootstrap_admin_config(env: &worker::Env) -> std::collections::HashMap<String, String> {
    use impresspress_core::blocks::auth::config::{
        BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY,
    };
    [BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY]
        .into_iter()
        .filter_map(|key| {
            let value = env.secret(key).ok()?.to_string();
            (!value.is_empty()).then(|| (key.to_string(), value))
        })
        .collect()
}

/// Cloudflare Worker `fetch` entrypoint. Defers everything to
/// [`impresspress_cloudflare::run_with_config`]: D1-backed config, R2
/// storage, the `/_deploy/*` funnel, then WAFER dispatch, with the
/// bootstrap-admin secrets as request config ([`bootstrap_admin_config`]).
/// No consumer blocks are registered and no post-build wiring is needed, so
/// both hooks are no-ops.
#[cfg(feature = "target-cloudflare")]
#[worker::event(fetch)]
async fn fetch_main(
    req: worker::Request,
    env: worker::Env,
    ctx: worker::Context,
) -> worker::Result<worker::Response> {
    let config = bootstrap_admin_config(&env);
    impresspress_cloudflare::run_with_config(req, env, ctx, config, Ok, |_wafer, _storage| Ok(()))
        .await
}

/// Cloudflare Worker `scheduled` entrypoint. Defers to
/// [`impresspress_cloudflare::run_scheduled_with_config`], which runs the
/// auth retention sweep and nothing else, with the same request config as
/// `fetch_main`: that config is part of the runtime's identity, so a
/// different map here would rebuild the isolate's runtime on every cron and
/// every fetch after it.
///
/// This export is step 1 of 2; step 2 is `[cloudflare].crons` in
/// `impresspress.toml` (this demo sets `17 3 * * *`). The sweep is opt-in
/// precisely because this half cannot be supplied by the adapter — it needs
/// the consumer's own registration hooks — so a default schedule would give a
/// consumer without this export a daily failed invocation.
///
/// The two registration hooks are the same ones `fetch_main` passes. They are
/// not part of runtime identity, so both entry points share one per-isolate
/// runtime cache and a `scheduled` handler that registered a different block
/// set would publish the wrong runtime for the next request to serve.
#[cfg(feature = "target-cloudflare")]
#[worker::event(scheduled)]
async fn scheduled_main(
    event: worker::ScheduledEvent,
    env: worker::Env,
    ctx: worker::ScheduleContext,
) {
    let config = bootstrap_admin_config(&env);
    impresspress_cloudflare::run_scheduled_with_config(
        event,
        env,
        ctx,
        config,
        Ok,
        |_wafer, _storage| Ok(()),
    )
    .await
}

/// Cloudflare Worker `start` entrypoint: one-time isolate initialization
/// before the first fetch event.
#[cfg(feature = "target-cloudflare")]
#[worker::event(start)]
fn start() {
    impresspress_cloudflare::init_isolate();
}
