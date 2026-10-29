//! The deployment's own site: which directory it is, and where it lands.
//!
//! One answer for every server target. `wafer-run/web` serves the `/**`
//! fallback (and `/` itself when `WAFER_RUN_SHARED__HAS_LANDING_PAGE` is
//! true) from the storage folder [`SITE_STORAGE_FOLDER`], so each flow puts
//! the directory [`find_frontend_dir`] picks exactly there:
//!
//! * native (`sealed × native`, `embed × native`): copied into
//!   [`native_site_dir`], the folder inside the native server's storage root;
//! * Cloudflare (`embed × cloudflare`): staged under the logical key prefix
//!   [`SITE_STORAGE_FOLDER`] in the R2 release set
//!   (`helpers::cloudflare::assets::stage`).

use std::path::{Path, PathBuf};

pub use impresspress_core::flows::SITE_STORAGE_FOLDER;

/// The site source directories, relative to the repo root, in priority order.
pub const FRONTEND_DIRS: &[&str] = &["frontend/build", "public"];

/// The native server's storage root relative to the repo root: the default
/// of `IMPRESSPRESS_STORAGE_ROOT` (`impresspress-native`'s `env.rs`).
const NATIVE_STORAGE_ROOT: &str = "data/storage";

/// Return the first frontend directory found under `repo_root`, in priority
/// order: `frontend/build/`, then `public/`. Returns `None` if neither exists.
pub fn find_frontend_dir(repo_root: &Path) -> Option<PathBuf> {
    FRONTEND_DIRS
        .iter()
        .map(|rel| repo_root.join(rel))
        .find(|p| p.is_dir())
}

/// Where the native server reads the site from:
/// `<repo>/data/storage/` + [`SITE_STORAGE_FOLDER`].
pub fn native_site_dir(repo_root: &Path) -> PathBuf {
    repo_root
        .join(NATIVE_STORAGE_ROOT)
        .join(SITE_STORAGE_FOLDER)
}

/// Copy the site (if the repo has one) into [`native_site_dir`]. The one
/// native half of this module, shared by both native flows.
pub fn stage_native_site(repo_root: &Path) -> std::io::Result<()> {
    match find_frontend_dir(repo_root) {
        Some(src) => copy_tree(&src, &native_site_dir(repo_root)),
        None => Ok(()),
    }
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
