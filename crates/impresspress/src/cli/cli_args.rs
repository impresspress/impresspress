//! Clap definitions for the unified impresspress CLI.

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "impresspress",
    version,
    about = "Impresspress — build and run Impresspress apps for native or browser targets",
    arg_required_else_help = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Build the app for `--target` (defaults per directory contents).
    Build {
        #[arg(long)]
        target: Option<Target>,

        /// Use the release profile. Default is debug for fast iteration.
        #[arg(long)]
        release: bool,
    },
    /// Build the app and run a server.
    Serve {
        #[arg(long)]
        target: Option<Target>,

        #[arg(long)]
        release: bool,

        /// Override the listen port. Native: from .env. Web: defaults to 8080.
        ///
        /// Every target applies pending block migrations before it serves:
        /// a native server at every boot (refusing to start if one fails),
        /// `--target cloudflare` through the `/_deploy/init` funnel once
        /// `wrangler dev` is reachable, `--target web` when the bundle loads.
        #[arg(long)]
        port: Option<u16>,
    },
    /// Build the app and deploy it to the target's hosting environment.
    /// (v1: only `--target cloudflare` is supported.)
    ///
    /// Cloudflare deploys are atomic: a dynamic candidate runs the one
    /// authenticated `/_deploy/prepare` mutation funnel, then its strict plan
    /// is packaged with the exact same Wasm into a second candidate. Only the
    /// second version is promoted after mutation-free plan/asset/health
    /// verification. Every deploy runs the prepare funnel, which applies
    /// pending block migrations. A site's first deploy also creates its
    /// Worker and sets its deploy secrets.
    Deploy {
        #[arg(long)]
        target: Option<Target>,

        #[arg(long)]
        release: bool,

        /// Optional subaction. Bare `impresspress deploy` runs the full deploy;
        /// `impresspress deploy secret` provisions worker secrets instead.
        #[command(subcommand)]
        action: Option<DeployAction>,
    },
}

/// Subactions of `impresspress deploy`.
#[derive(Subcommand, Debug)]
pub enum DeployAction {
    /// Set the worker secrets the deploy funnel needs
    /// (`IMPRESSPRESS_DEPLOY_TOKEN` + the auth JWT secret) via
    /// `wrangler secret put`. Each value is taken from the same-named env var
    /// if set, otherwise a fresh 32-byte hex token is generated. A site's
    /// first `impresspress deploy` sets both itself; this sets them again.
    /// Requires a generated `wrangler.toml` (run `impresspress build --target
    /// cloudflare` first).
    Secret,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Native,
    Web,
    Cloudflare,
}

// No `Default for Cli` impl on purpose: hand-constructing a default
// `Cli` bypasses clap's verb-level flag handling, so any new flag added
// to a verb silently defaults to whatever value happens to be in the
// hand-rolled default. The bare-`impresspress` fallback in `main` instead
// reparses the synthetic argv `["impresspress", "serve"]`, which keeps clap
// as the single source of truth.
