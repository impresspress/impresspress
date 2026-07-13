//! Resolves the impresspress-web wasm and JS bytes for the sealed × web flow.

use std::{borrow::Cow, path::PathBuf};

use anyhow::{anyhow, Result};

/// Resolution order:
/// 1. IMPRESSPRESS_WEB_WASM env var (must point at an existing file)
/// 2. include_bytes! baked at build time (always available)
pub fn resolve_impresspress_web_wasm() -> Result<Cow<'static, [u8]>> {
    if let Ok(p) = std::env::var("IMPRESSPRESS_WEB_WASM") {
        let path = PathBuf::from(&p);
        if !path.is_file() {
            return Err(anyhow!(
                "IMPRESSPRESS_WEB_WASM points at {p:?} but the file does not exist"
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {p:?}: {e}"))?;
        return Ok(Cow::Owned(bytes));
    }
    Ok(Cow::Borrowed(crate::IMPRESSPRESS_WEB_WASM))
}

/// Resolution order:
/// 1. IMPRESSPRESS_WEB_JS env var (must point at an existing file)
/// 2. include_bytes! baked at build time (always available)
pub fn resolve_impresspress_web_js() -> Result<Cow<'static, [u8]>> {
    if let Ok(p) = std::env::var("IMPRESSPRESS_WEB_JS") {
        let path = PathBuf::from(&p);
        if !path.is_file() {
            return Err(anyhow!(
                "IMPRESSPRESS_WEB_JS points at {p:?} but the file does not exist"
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("read {p:?}: {e}"))?;
        return Ok(Cow::Owned(bytes));
    }
    Ok(Cow::Borrowed(crate::IMPRESSPRESS_WEB_JS))
}
