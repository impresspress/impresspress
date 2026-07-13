use std::path::{Path, PathBuf};

/// Return the first frontend directory found under `repo_root`, in priority
/// order: `frontend/build/`, then `public/`. Returns `None` if neither exists.
pub fn find_frontend_dir(repo_root: &Path) -> Option<PathBuf> {
    let candidates = ["frontend/build", "public"];
    for rel in &candidates {
        let p = repo_root.join(rel);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// Recursively copy `src` into `dst`. Creates `dst` if missing.
pub fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
