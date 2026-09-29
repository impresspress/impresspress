//! The half of a snapshot gate that looks at the directory rather than the
//! code: which committed baselines a run never compared.
//!
//! A committed baseline that no run compares is a gate passing for free — the
//! file sits there looking like a reviewed contract while nothing reads it.
//! Both snapshot gates in this directory need the check and differ only in the
//! suffix they own (`.openapi.json`, `.endpoints.json`), so the suffix is a
//! parameter and the walk is written once.
//!
//! Included with `mod baselines;` from each test root. `tests/baselines/` has
//! no `main.rs`, so cargo does not build it as a test target of its own.

/// Stems of `*{suffix}` files in `dir` that this run did not compare
/// (`compared`) and that are not excused by `absent_by_feature`, sorted.
pub fn unchecked(
    dir: &std::path::Path,
    suffix: &str,
    compared: &[String],
    absent_by_feature: &[&str],
) -> Vec<String> {
    let mut left: Vec<String> = std::fs::read_dir(dir)
        .expect("read snapshot dir")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(suffix))
                .map(str::to_string)
        })
        .filter(|stem| {
            !compared.iter().any(|c| c == stem) && !absent_by_feature.contains(&stem.as_str())
        })
        .collect();
    left.sort();
    left
}

/// A directory this process alone owns, named for what it is testing.
///
/// The process id is not enough on its own: it is reused, and a directory
/// leaked by an earlier run that died mid-test would be inherited here and
/// answer with its files instead of the ones written below.
fn scratch_dir(what: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "impresspress-{what}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&dir).expect("a directory no other run holds");
    dir
}

/// The walk itself, on a directory this test owns: a baseline the run compared
/// is quiet, one it did not is reported, one excused by a feature is quiet, and
/// a file carrying the other gate's suffix is not this gate's business.
///
/// The two calls must not agree. `suffix` is the only thing that separates the
/// two gates, so a helper that ignored it — hardcoding one suffix, or matching
/// any baseline at all — has to be visible here; if both answers were the same
/// list, it would not be. `gamma` exists under one suffix only, so it is in
/// exactly one of the two answers.
#[test]
fn unchecked_lists_committed_files_this_run_did_not_compare() {
    let dir = scratch_dir("baselines");
    for stem in ["alpha", "beta", "dev"] {
        std::fs::write(dir.join(format!("{stem}.openapi.json")), "{}\n").expect("write");
    }
    std::fs::write(dir.join("beta.endpoints.json"), "[]\n").expect("write");
    std::fs::write(dir.join("gamma.endpoints.json"), "[]\n").expect("write");

    let left = unchecked(&dir, ".openapi.json", &["alpha".to_string()], &["dev"]);
    let other = unchecked(&dir, ".endpoints.json", &[], &[]);
    std::fs::remove_dir_all(&dir).expect("remove temp dir");

    assert_eq!(
        left,
        vec!["beta".to_string()],
        "`gamma` has no OpenAPI baseline, so this gate must not see it"
    );
    assert_eq!(
        other,
        vec!["beta".to_string(), "gamma".to_string()],
        "the suffix selects the gate: `alpha` and `dev` have no endpoint baseline, \
         and `gamma` has nothing else"
    );
}
