//! Verifies the clap CLI accepts the verbs and flags from the unified
//! design (build/serve, --target native|web, --release, --port).

use clap::Parser;
use impresspress::cli::{
    cli_args::{Cli, Command, DeployAction, Target},
    helpers::cloudflare::password_hasher::PasswordHasherMode,
};

#[test]
fn parses_build_with_target_native() {
    let cli = Cli::parse_from(["impresspress", "build", "--target", "native"]);
    if let Command::Build { target, release } = cli.command {
        assert_eq!(target, Some(Target::Native));
        assert!(!release);
    } else {
        panic!("expected Build");
    }
}

#[test]
fn parses_build_with_release_flag() {
    let cli = Cli::parse_from(["impresspress", "build", "--release"]);
    if let Command::Build { release, .. } = cli.command {
        assert!(release);
    } else {
        panic!("expected Build");
    }
}

#[test]
fn parses_serve_with_port() {
    let cli = Cli::parse_from(["impresspress", "serve", "--port", "9090"]);
    if let Command::Serve { port, .. } = cli.command {
        assert_eq!(port, Some(9090));
    } else {
        panic!("expected Serve");
    }
}

#[test]
fn parses_deploy_without_action() {
    // Bare `impresspress deploy` (optionally with flags) leaves `action` None so
    // the full deploy flow runs — the `secret` subaction is opt-in.
    let cli = Cli::parse_from(["impresspress", "deploy", "--target", "cloudflare"]);
    if let Command::Deploy {
        target,
        release,
        password_hasher,
        action,
    } = cli.command
    {
        assert_eq!(target, Some(Target::Cloudflare));
        assert!(!release);
        assert_eq!(password_hasher, PasswordHasherMode::Deploy);
        assert!(action.is_none());
    } else {
        panic!("expected Deploy");
    }
}

/// `--password-hasher keep` leaves the deployed hasher in place; `deploy` is
/// the default, and no other value is accepted.
#[test]
fn parses_deploy_password_hasher_mode() {
    for (value, mode) in [
        ("keep", PasswordHasherMode::Keep),
        ("deploy", PasswordHasherMode::Deploy),
    ] {
        let cli = Cli::parse_from([
            "impresspress",
            "deploy",
            "--target",
            "cloudflare",
            "--password-hasher",
            value,
        ]);
        let Command::Deploy {
            password_hasher, ..
        } = cli.command
        else {
            panic!("expected Deploy");
        };
        assert_eq!(password_hasher, mode, "{value}");
    }
    assert!(Cli::try_parse_from(["impresspress", "deploy", "--password-hasher", "skip"]).is_err());
}

#[test]
fn parses_deploy_secret_action() {
    let cli = Cli::parse_from(["impresspress", "deploy", "secret"]);
    if let Command::Deploy { action, .. } = cli.command {
        assert!(matches!(action, Some(DeployAction::Secret)));
    } else {
        panic!("expected Deploy");
    }
}

#[test]
fn bare_impresspress_uses_serve_native_default() {
    // Bare `impresspress` invokes the same code path as the synthetic argv
    // `["impresspress", "serve"]` (see `main::run`). Asserting the parsed
    // shape — instead of a `Cli::default()` impl — keeps clap as the
    // single source of truth for verb-level defaults.
    let cli = Cli::parse_from(["impresspress", "serve"]);
    if let Command::Serve {
        target,
        release,
        port,
    } = cli.command
    {
        assert_eq!(target, None);
        assert!(!release);
        assert_eq!(port, None);
    } else {
        panic!("expected Serve");
    }
}
