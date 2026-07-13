use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// Rename `path` to include `-<hash>` before its extension.
/// Example: `foo.wasm` + `a1b2c3d4` → `foo-a1b2c3d4.wasm`.
/// Returns the new path (absolute, same directory).
pub fn rename_with_hash(path: &Path, hash: &str) -> Result<PathBuf> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    let stem = path
        .file_stem()
        .ok_or_else(|| anyhow!("no file stem: {}", path.display()))?
        .to_string_lossy()
        .into_owned();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let new_name = format!("{stem}-{hash}{ext}");
    let new_path = dir.join(&new_name);
    std::fs::rename(path, &new_path)
        .with_context(|| format!("rename {} -> {}", path.display(), new_path.display()))?;
    Ok(new_path)
}

/// Replace one exact substring in a UTF-8 file. Fails if `from` is not
/// present, or if it appears more than once.
pub fn rewrite_literal(path: &Path, from: &str, to: &str) -> Result<()> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let count = body.matches(from).count();
    if count == 0 {
        bail!("literal {:?} not found in {}", from, path.display());
    }
    if count > 1 {
        bail!(
            "literal {:?} appears {} times in {} — expected exactly one",
            from,
            count,
            path.display()
        );
    }
    let replaced = body.replace(from, to);
    std::fs::write(path, replaced).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Replace all occurrences of `from` with `to` in a UTF-8 file.
/// Fails if `from` is not present at all.
pub fn rewrite_all(path: &Path, from: &str, to: &str) -> Result<()> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let count = body.matches(from).count();
    if count == 0 {
        bail!("literal {:?} not found in {}", from, path.display());
    }
    let replaced = body.replace(from, to);
    std::fs::write(path, replaced).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn rename_adds_hash_before_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("foo.wasm");
        fs::write(&p, b"x").unwrap();
        let out = rename_with_hash(&p, "abcd1234").unwrap();
        assert_eq!(out.file_name().unwrap(), "foo-abcd1234.wasm");
        assert!(out.exists());
        assert!(!p.exists());
    }

    #[test]
    fn rename_handles_compound_stems() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("sql-wasm-esm.js");
        fs::write(&p, b"x").unwrap();
        let out = rename_with_hash(&p, "ffff0000").unwrap();
        assert_eq!(out.file_name().unwrap(), "sql-wasm-esm-ffff0000.js");
    }

    #[test]
    fn rewrite_replaces_single_literal() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.js");
        fs::write(&p, "x = 'impresspress_web_bg.wasm';").unwrap();
        rewrite_literal(
            &p,
            "'impresspress_web_bg.wasm'",
            "'impresspress_web_bg-abcd1234.wasm'",
        )
        .unwrap();
        let body = fs::read_to_string(&p).unwrap();
        assert_eq!(body, "x = 'impresspress_web_bg-abcd1234.wasm';");
    }

    #[test]
    fn rewrite_fails_when_literal_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.js");
        fs::write(&p, "unrelated content").unwrap();
        let err = rewrite_literal(&p, "MISSING", "X").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn rewrite_fails_on_multiple_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.js");
        fs::write(&p, "foo foo").unwrap();
        let err = rewrite_literal(&p, "foo", "bar").unwrap_err();
        assert!(err.to_string().contains("expected exactly one"));
    }

    #[test]
    fn rewrite_all_replaces_every_occurrence() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.js");
        // Simulate sql.js UMD bundle with multiple references to the wasm path.
        fs::write(
            &p,
            r#""sql-wasm.wasm" || "sql-wasm.wasm" ? "sql-wasm.wasm" : B"#,
        )
        .unwrap();
        rewrite_all(&p, "\"sql-wasm.wasm\"", "\"sql-wasm-abc123.wasm\"").unwrap();
        let body = fs::read_to_string(&p).unwrap();
        assert_eq!(
            body,
            r#""sql-wasm-abc123.wasm" || "sql-wasm-abc123.wasm" ? "sql-wasm-abc123.wasm" : B"#
        );
    }

    #[test]
    fn rewrite_all_fails_when_literal_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.js");
        fs::write(&p, "unrelated content").unwrap();
        let err = rewrite_all(&p, "MISSING", "X").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
