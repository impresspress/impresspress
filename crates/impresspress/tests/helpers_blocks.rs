use std::fs;

use impresspress::cli::helpers::blocks::discover_blocks;
use tempfile::tempdir;

#[test]
fn no_blocks_dir_returns_empty() {
    let tmp = tempdir().unwrap();
    let blocks = discover_blocks(tmp.path()).unwrap();
    assert!(blocks.is_empty());
}

#[test]
fn returns_each_dir_with_cargo_toml() {
    let tmp = tempdir().unwrap();
    let blocks_dir = tmp.path().join("blocks");
    fs::create_dir_all(blocks_dir.join("foo")).unwrap();
    fs::write(blocks_dir.join("foo/Cargo.toml"), "").unwrap();
    fs::create_dir_all(blocks_dir.join("bar")).unwrap();
    fs::write(blocks_dir.join("bar/Cargo.toml"), "").unwrap();
    fs::create_dir_all(blocks_dir.join("notablock")).unwrap(); // no Cargo.toml

    let mut out = discover_blocks(tmp.path()).unwrap();
    out.sort();
    assert_eq!(out.len(), 2);
    assert!(out[0].ends_with("blocks/bar"));
    assert!(out[1].ends_with("blocks/foo"));
}
