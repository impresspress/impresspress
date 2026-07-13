//! Auto-detection of mode (sealed vs embed) and default target
//! (native vs web) per the unified-CLI design.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::cli::cli_args::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sealed,
    Embed,
}

/// Cwd state used to drive mode + target decisions.
#[derive(Debug)]
pub struct ModeContext {
    pub cwd: PathBuf,
    pub cargo_toml_path: Option<PathBuf>,
    pub crate_types: CrateTypes,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CrateTypes {
    pub has_bin: bool,
    pub has_cdylib: bool,
}

impl ModeContext {
    pub fn scan(cwd: &Path) -> Result<Self> {
        let cargo_toml_path = walk_up_for_cargo_toml(cwd);
        let crate_types = match &cargo_toml_path {
            Some(p) => parse_crate_types(p)?,
            None => CrateTypes::default(),
        };
        Ok(Self {
            cwd: cwd.to_path_buf(),
            cargo_toml_path,
            crate_types,
        })
    }
}

pub fn detect_mode(ctx: &ModeContext) -> Mode {
    match ctx.cargo_toml_path {
        Some(_) => Mode::Embed,
        None => Mode::Sealed,
    }
}

pub fn default_target(ctx: &ModeContext, explicit: Option<Target>) -> Result<Target> {
    if let Some(t) = explicit {
        return Ok(t);
    }
    match detect_mode(ctx) {
        Mode::Sealed => Ok(Target::Native),
        Mode::Embed => match (ctx.crate_types.has_bin, ctx.crate_types.has_cdylib) {
            (true, false) => Ok(Target::Native),
            (false, true) => Ok(Target::Web),
            (true, true) => Err(anyhow!(
                "Cargo.toml has both [[bin]] and crate-type cdylib; specify --target"
            )),
            (false, false) => Err(anyhow!(
                "Cargo.toml has neither [[bin]] nor crate-type cdylib; specify --target"
            )),
        },
    }
}

fn walk_up_for_cargo_toml(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.is_file() {
            // If it's a virtual workspace Cargo.toml (defines [workspace] but not [package]),
            // skip it and continue walking up.
            if let Ok(text) = std::fs::read_to_string(&candidate) {
                if let Ok(toml) = toml::from_str::<toml::Value>(&text) {
                    if toml.get("workspace").is_some() && toml.get("package").is_none() {
                        match cur.parent() {
                            Some(p) => {
                                cur = p;
                                continue;
                            }
                            None => return None,
                        }
                    }
                }
            }
            return Some(candidate);
        }
        if cur.join(".git").exists() {
            // git root reached; stop.
            return None;
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

fn parse_crate_types(cargo_toml: &Path) -> Result<CrateTypes> {
    let text = std::fs::read_to_string(cargo_toml)?;
    let toml: toml::Value = toml::from_str(&text)?;

    let has_bin = toml
        .get("bin")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
        || cargo_toml
            .parent()
            .map(|d| d.join("src/main.rs").exists())
            .unwrap_or(false);

    let has_cdylib = toml
        .get("lib")
        .and_then(|v| v.get("crate-type"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).any(|s| s == "cdylib"))
        .unwrap_or(false);

    Ok(CrateTypes {
        has_bin,
        has_cdylib,
    })
}
