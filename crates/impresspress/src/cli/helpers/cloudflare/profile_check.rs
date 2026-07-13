//! Pre-build inspection of the consumer's `[profile.release]` and
//! post-build inspection of the produced WASM size.
//!
//! Cloudflare Workers has a hard 400ms startup-CPU cap. V8's WASM
//! startup compiler (Liftoff) runs at roughly 10–15 MB/sec, so a
//! Rust WASM worker built with cargo defaults (opt-level=3, no LTO,
//! codegen-units=16, no strip) is typically too large to start within
//! the cap and fails every cold-start request with `error code: 1102`.
//!
//! These checks emit warnings to stderr; they never error or block the
//! build. Users can ignore the warnings if they have a reason to.

use std::path::Path;

use anyhow::{Context, Result};

/// Warn if the WASM exceeds this size. V8 Liftoff at the slow end
/// (~10 MB/sec) compiles 6 MB in ~600ms — already over the 400ms cap.
/// Below this we have meaningful headroom.
const WASM_SIZE_WARN_BYTES: u64 = 6 * 1024 * 1024;

/// Inspect the consumer's `Cargo.toml` for `[profile.release]` settings
/// and emit a warning if size optimizations are missing.
pub fn check_release_profile(repo_root: &Path) -> Result<()> {
    let cargo_toml = repo_root.join("Cargo.toml");
    let raw = std::fs::read_to_string(&cargo_toml)
        .with_context(|| format!("read {}", cargo_toml.display()))?;
    let parsed: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parse {}", cargo_toml.display()))?;

    let issues = collect_profile_issues(&parsed);
    if issues.is_empty() {
        return Ok(());
    }

    eprintln!();
    eprintln!("⚠️  Cloudflare Workers has a 400ms startup-CPU cap.");
    eprintln!("    Without size optimization, your WASM may exceed it");
    eprintln!("    and fail every cold-start with `error code: 1102`.");
    eprintln!();
    eprintln!("    Cargo.toml issues found:");
    for issue in &issues {
        eprintln!("      • {issue}");
    }
    eprintln!();
    eprintln!("    Suggested [profile.release] in your Cargo.toml:");
    eprintln!();
    eprintln!("      [profile.release]");
    eprintln!("      opt-level = \"z\"");
    eprintln!("      lto = true");
    eprintln!("      codegen-units = 1");
    eprintln!("      strip = true");
    eprintln!("      panic = \"abort\"");
    eprintln!();
    Ok(())
}

/// Measure the produced WASM and warn if it's likely to exceed the
/// startup-CPU cap. Called after `worker-build` finishes.
pub fn check_wasm_size(wasm_path: &Path) -> Result<()> {
    // No file = nothing to check; the build step itself will surface
    // the actual error. Don't double-report.
    let Ok(meta) = std::fs::metadata(wasm_path) else {
        return Ok(());
    };
    let size = meta.len();
    let mb = size as f64 / (1024.0 * 1024.0);

    if size <= WASM_SIZE_WARN_BYTES {
        eprintln!("-> WASM: {mb:.2} MB ({size} bytes)");
        return Ok(());
    }

    eprintln!();
    eprintln!("⚠️  WASM is {mb:.2} MB — Workers cold-start may exceed the 400ms");
    eprintln!("    startup-CPU cap. Symptoms: `error code: 1102` on every");
    eprintln!("    cold request, `outcome: exceededCpu` in Workers Logs.");
    eprintln!();
    eprintln!("    Levers, in priority order:");
    eprintln!("      1. Verify [profile.release] in Cargo.toml is set for size:");
    eprintln!("         opt-level=\"z\", lto=true, codegen-units=1, strip=true,");
    eprintln!("         panic=\"abort\".");
    eprintln!("      2. Feature-gate impresspress-core blocks you don't use");
    eprintln!("         (products, vector, files, llm, userportal, messages).");
    eprintln!("      3. Audit large dependencies via `twiggy top` on the WASM.");
    eprintln!();
    Ok(())
}

/// Pure inspection helper, easy to unit-test. Returns a list of
/// human-readable problems found in `[profile.release]`. Empty list
/// means all good.
fn collect_profile_issues(parsed: &toml::Value) -> Vec<String> {
    let mut issues = Vec::new();
    let release = parsed.get("profile").and_then(|p| p.get("release"));

    let Some(release) = release else {
        issues.push("[profile.release] is missing — using cargo defaults".into());
        return issues;
    };

    // opt-level: must be "z" or "s" for size. Numeric levels (1/2/3) are
    // size-suboptimal. Default is 3.
    match release.get("opt-level") {
        None => issues.push("opt-level is unset (defaults to 3, optimizes for speed)".into()),
        Some(v) => match v.as_str() {
            Some("z") | Some("s") => {}
            Some(other) => issues.push(format!(
                "opt-level = \"{other}\" — use \"z\" (or \"s\") for size"
            )),
            None => issues.push(format!("opt-level = {v} — use \"z\" (or \"s\") for size")),
        },
    }

    // lto: must be true / "fat" / "thin".
    match release.get("lto") {
        None => issues.push("lto is unset — set lto = true".into()),
        Some(v) => match (v.as_bool(), v.as_str()) {
            (Some(true), _) | (_, Some("fat")) | (_, Some("thin")) => {}
            _ => issues.push(format!("lto = {v} — set lto = true")),
        },
    }

    // codegen-units: should be 1 for best LTO results.
    match release.get("codegen-units") {
        None => issues.push("codegen-units is unset (defaults to 16) — set to 1".into()),
        Some(v) => match v.as_integer() {
            Some(1) => {}
            _ => issues.push(format!("codegen-units = {v} — set to 1")),
        },
    }

    // strip: should be true to drop the function-names section (~half
    // a MB on a typical Rust WASM).
    match release.get("strip") {
        None => issues.push("strip is unset — set strip = true".into()),
        Some(v) => match (v.as_bool(), v.as_str()) {
            (Some(true), _) | (_, Some("symbols")) | (_, Some("debuginfo")) => {}
            _ => issues.push(format!("strip = {v} — set strip = true")),
        },
    }

    // panic = "abort" is recommended but not required; mention only if
    // explicitly set to something else. Absence is common and not loud.
    if let Some(v) = release.get("panic") {
        if v.as_str() != Some("abort") {
            issues.push(format!(
                "panic = {v} — \"abort\" removes unwinding tables (smaller binary)"
            ));
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> toml::Value {
        toml::from_str(s).expect("test toml")
    }

    #[test]
    fn missing_profile_release_is_flagged() {
        let issues = collect_profile_issues(&parse("[package]\nname = \"x\"\n"));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("[profile.release] is missing"));
    }

    #[test]
    fn default_settings_are_all_flagged() {
        let issues = collect_profile_issues(&parse("[profile.release]\n"));
        assert_eq!(issues.len(), 4); // opt-level, lto, codegen-units, strip
    }

    #[test]
    fn ideal_profile_passes() {
        let toml = r#"
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
panic = "abort"
"#;
        assert!(collect_profile_issues(&parse(toml)).is_empty());
    }

    #[test]
    fn opt_level_s_passes() {
        let toml = r#"
[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
strip = true
"#;
        assert!(collect_profile_issues(&parse(toml)).is_empty());
    }

    #[test]
    fn opt_level_3_is_flagged() {
        let toml = r#"
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
"#;
        let issues = collect_profile_issues(&parse(toml));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("opt-level"));
    }

    #[test]
    fn lto_fat_and_thin_pass() {
        for lto in &["\"fat\"", "\"thin\"", "true"] {
            let toml = format!(
                r#"
[profile.release]
opt-level = "z"
lto = {lto}
codegen-units = 1
strip = true
"#
            );
            assert!(
                collect_profile_issues(&parse(&toml)).is_empty(),
                "lto = {lto} should pass"
            );
        }
    }

    #[test]
    fn strip_symbols_passes() {
        let toml = r#"
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = "symbols"
"#;
        assert!(collect_profile_issues(&parse(toml)).is_empty());
    }

    #[test]
    fn explicit_panic_unwind_is_flagged_as_optional() {
        let toml = r#"
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
panic = "unwind"
"#;
        let issues = collect_profile_issues(&parse(toml));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("panic"));
    }

    #[test]
    fn panic_unset_is_not_flagged() {
        let toml = r#"
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
"#;
        assert!(collect_profile_issues(&parse(toml)).is_empty());
    }
}
