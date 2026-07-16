//! Pre-build inspection of the consumer's `[profile.release]` and
//! post-build inspection of the produced WASM size.
//!
//! Cloudflare Workers gives a Worker's global (top-level) scope a hard
//! **1-second (1000ms)** CPU budget to parse and instantiate — this is the
//! *startup* limit, and it is checked once, at deploy validation time. A
//! Worker that exceeds it is rejected outright (`wrangler deploy`/`versions
//! upload` fails with "Script startup exceeded CPU time limit", error code
//! **10021**) and never goes live to serve a single request. That is a
//! *different, much smaller* budget than the *per-request* CPU limit
//! enforced on every live invocation (10ms on the Free plan; configurable up
//! to 300s on Paid), whose runtime failure is the unrelated error **1102**
//! ("Worker exceeded resource limits", `outcome: exceededCpu` in Workers
//! Logs). See <https://developers.cloudflare.com/workers/platform/limits/>
//! and <https://developers.cloudflare.com/workers/observability/errors/>.
//! (A prior version of this module cited a 400ms startup cap and claimed
//! oversized startup produced request-time 1102s — both wrong: the real
//! limit is 1000ms and it is a deploy-time rejection, not a request-time
//! failure.)
//!
//! V8's baseline WASM compiler (Liftoff) is commonly cited at roughly
//! 10-15 MB/sec, so a Rust WASM worker built with cargo defaults
//! (opt-level=3, no LTO, codegen-units=16, no strip) can plausibly miss the
//! 1000ms budget once its `.wasm` reaches several MB. The size check below
//! is a **cheap local heuristic** for that risk, not a measurement — prefer
//! `wrangler check startup` (profiles actual startup CPU, albeit on your
//! local machine rather than Cloudflare's edge hardware) or `wrangler deploy
//! --dry-run` for an authoritative answer before a real deploy.
//!
//! These checks emit warnings to stderr; they never error or block the
//! build. Users can ignore the warnings if they have a reason to.

use std::path::Path;

use anyhow::{Context, Result};

/// Warn if the WASM exceeds this size. At the slow end of Liftoff's cited
/// range (~10 MB/sec), the real 1000ms startup budget buys about 10 MB
/// before a cold start risks deploy-time rejection (error 10021). This
/// constant warns at 8 MB — meaningful headroom below that ceiling, not
/// "already over budget" (unlike the previous 6 MB/400ms pairing, which put
/// the warning past a cap that doesn't exist).
const WASM_SIZE_WARN_BYTES: u64 = 8 * 1024 * 1024;

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
    eprintln!("⚠️  Cloudflare Workers gives a Worker's startup (global scope) a");
    eprintln!("    1-second CPU budget, checked at deploy time. Without size");
    eprintln!("    optimization, your WASM may miss it — `wrangler deploy` then");
    eprintln!("    REJECTS the deployment with `error code: 10021` (\"Script");
    eprintln!("    startup exceeded CPU time limit\"); nothing goes live.");
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
    eprintln!("⚠️  WASM is {mb:.2} MB — a cold-start compile risks missing the");
    eprintln!("    real 1-second startup-CPU budget. That's checked at DEPLOY");
    eprintln!("    time: `wrangler deploy`/`versions upload` would then REJECT");
    eprintln!("    the deployment with `error code: 10021` (\"Script startup");
    eprintln!("    exceeded CPU time limit\") — not the unrelated per-request");
    eprintln!("    `1102`/`exceededCpu`, which is a separate, much larger budget.");
    eprintln!();
    eprintln!("    This byte count is a heuristic. For an authoritative answer:");
    eprintln!("      - `wrangler check startup` profiles real startup CPU time");
    eprintln!("        (locally — your machine, not Cloudflare's edge hardware).");
    eprintln!("      - `wrangler deploy --dry-run` / `versions upload` re-validates");
    eprintln!("        against the real 1000ms cap before anything goes live.");
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

/// Cloudflare-reported upload size for a version, parsed from `wrangler`'s
/// own `versions upload` output. Unlike [`WASM_SIZE_WARN_BYTES`], this is a
/// real number Cloudflare computed for the exact bundle it received — the
/// authoritative "compressed bundle size" the raw-`.wasm`-byte heuristic can
/// only estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UploadSize {
    pub raw_kib: f64,
    pub gzip_kib: f64,
}

/// Parse wrangler's `"Total Upload: <raw> KiB / gzip: <gzip> KiB"` line out
/// of `versions upload`/`deploy` stdout. Returns `None` if the line isn't
/// present (older/newer wrangler output format, or the command failed
/// before printing it) — callers treat that as "nothing to report", not an
/// error, since this is a best-effort supplement to the real upload result.
pub fn parse_upload_size(stdout: &str) -> Option<UploadSize> {
    let line = stdout.lines().find(|l| l.contains("Total Upload:"))?;
    let after_label = line.split("Total Upload:").nth(1)?;
    let (raw_part, gzip_part) = after_label.split_once('/')?;
    let raw_kib = raw_part
        .trim()
        .trim_end_matches("KiB")
        .trim()
        .parse()
        .ok()?;
    let gzip_kib = gzip_part
        .split("gzip:")
        .nth(1)?
        .trim()
        .trim_end_matches("KiB")
        .trim()
        .parse()
        .ok()?;
    Some(UploadSize { raw_kib, gzip_kib })
}

/// Print the Cloudflare-reported upload size, if `wrangler` printed one.
/// Called after a successful `versions upload`/`deploy` — the number here
/// reflects the exact artifact Cloudflare received, so prefer reading it
/// over the pre-upload `.wasm` byte heuristic when both are available.
pub fn report_upload_size(size: Option<UploadSize>) {
    let Some(size) = size else {
        return;
    };
    println!(
        "-> Cloudflare-reported upload size: {:.1} KiB raw / {:.1} KiB gzip",
        size.raw_kib, size.gzip_kib
    );
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

    #[test]
    fn parses_total_upload_line() {
        let out = "Total Upload: 4210.12 KiB / gzip: 1234.56 KiB\nWorker Version ID: abc-123\n";
        let size = parse_upload_size(out).expect("should parse Total Upload line");
        assert_eq!(size.raw_kib, 4210.12);
        assert_eq!(size.gzip_kib, 1234.56);
    }

    #[test]
    fn missing_total_upload_line_yields_none() {
        assert_eq!(parse_upload_size("Worker Version ID: abc-123\n"), None);
    }

    #[test]
    fn integer_kib_values_parse() {
        let out = "Total Upload: 4210 KiB / gzip: 1234 KiB\n";
        let size = parse_upload_size(out).expect("should parse");
        assert_eq!(size.raw_kib, 4210.0);
        assert_eq!(size.gzip_kib, 1234.0);
    }
}
