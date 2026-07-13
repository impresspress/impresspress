use std::fs;

use impresspress::cli::{
    cli_args::Target,
    mode::{default_target, detect_mode, Mode, ModeContext},
};
use tempfile::tempdir;

#[test]
fn no_cargo_toml_means_sealed() {
    let tmp = tempdir().unwrap();
    // Plant a `.git` so walk_up stops here rather than crawling out to the
    // real workspace's Cargo.toml.
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let ctx = ModeContext::scan(tmp.path()).unwrap();
    assert_eq!(detect_mode(&ctx), Mode::Sealed);
}

#[test]
fn cargo_toml_in_cwd_means_embed() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    let ctx = ModeContext::scan(tmp.path()).unwrap();
    assert_eq!(detect_mode(&ctx), Mode::Embed);
}

#[test]
fn cargo_toml_in_parent_means_embed() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    fs::create_dir_all(tmp.path().join("sub/dir")).unwrap();
    let ctx = ModeContext::scan(&tmp.path().join("sub/dir")).unwrap();
    assert_eq!(detect_mode(&ctx), Mode::Embed);
}

#[test]
fn sealed_default_target_is_native() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let ctx = ModeContext::scan(tmp.path()).unwrap();
    assert_eq!(default_target(&ctx, None).unwrap(), Target::Native);
}

#[test]
fn embed_cdylib_only_default_target_is_web() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cargo = "[package]\nname=\"x\"\nversion=\"0.0.1\"\n[lib]\ncrate-type=[\"cdylib\"]\n";
    fs::write(tmp.path().join("Cargo.toml"), cargo).unwrap();
    let ctx = ModeContext::scan(tmp.path()).unwrap();
    assert_eq!(default_target(&ctx, None).unwrap(), Target::Web);
}

#[test]
fn embed_bin_only_default_target_is_native() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cargo =
        "[package]\nname=\"x\"\nversion=\"0.0.1\"\n[[bin]]\nname=\"x\"\npath=\"src/main.rs\"\n";
    fs::write(tmp.path().join("Cargo.toml"), cargo).unwrap();
    let ctx = ModeContext::scan(tmp.path()).unwrap();
    assert_eq!(default_target(&ctx, None).unwrap(), Target::Native);
}

#[test]
fn explicit_target_wins_over_default() {
    let tmp = tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let ctx = ModeContext::scan(tmp.path()).unwrap();
    assert_eq!(
        default_target(&ctx, Some(Target::Web)).unwrap(),
        Target::Web
    );
}
