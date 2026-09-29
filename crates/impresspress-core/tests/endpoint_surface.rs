//! Per-block endpoint-surface snapshots.
//!
//! One line per `info().endpoints` entry, `METHOD path auth [tool=name]`,
//! sorted. The OpenAPI snapshot beside this one lists only endpoints that
//! carry a schema (`BlockEndpoint::has_schema`), so a page or a schema-less
//! API can be added, dropped or moved to another auth level without it
//! noticing. This file is the contract for the part of the surface the
//! router enforces: which (method, path) pairs a block declares and the
//! level each requires.
//!
//! Regenerate with
//! `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface`
//! (and once more with `--features block-dev` for the dev block) and review
//! every changed line: a new line is a path the router now admits at that
//! level, and a changed level is a security decision.

use std::path::PathBuf;

use wafer_run::BlockInfo;

mod baselines;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// `impresspress/auth-ui` -> `auth_ui`, the same file stem the OpenAPI
/// snapshots use, so the two files for one block sit next to each other.
fn slug(block_name: &str) -> String {
    block_name
        .rsplit('/')
        .next()
        .unwrap_or(block_name)
        .replace('-', "_")
}

fn surface_lines(info: &BlockInfo) -> Vec<String> {
    let mut lines: Vec<String> = info
        .endpoints
        .iter()
        .map(|ep| {
            let mut line = format!("{} {} {}", ep.method, ep.path, ep.auth);
            if let Some(tool) = &ep.agent_tool {
                line.push_str(&format!(" tool={}", tool.name));
            }
            line
        })
        .collect();
    lines.sort();
    lines
}

/// Every block whose `info()` the runtime registers. `all_block_infos` covers
/// the manifest blocks plus `llm`; `dev` has its own constructor and joins
/// only when compiled in.
fn surface_block_infos() -> Vec<BlockInfo> {
    #[cfg_attr(
        not(feature = "block-dev"),
        expect(unused_mut, reason = "only the `block-dev` extend below needs it")
    )]
    let mut infos = impresspress_core::blocks::all_block_infos();
    #[cfg(feature = "block-dev")]
    infos.extend(
        impresspress_core::test_support::real_block_infos()
            .into_iter()
            .filter(|info| info.name == "impresspress/dev"),
    );
    infos
}

/// Baselines whose block is compiled in only under a non-default feature,
/// each with the gate that decides it. They are legitimately absent from
/// `surface_block_infos()` in a run without that feature; every other
/// committed baseline must be compared, or the gate is passing for free and
/// the test fails to say so.
///
/// The gate is per row: the row's own `cfg!` decides that row and no other, so
/// an entry gated on some different feature cannot be excused by this one's
/// being off — which is what one bool for the whole list would do, silently.
///
/// What a row does not do is compile the block in; `surface_block_infos` above
/// carries its own `#[cfg]` for that, because a `const` cannot produce a
/// `BlockInfo`. So the same decision is written twice in this file and the two
/// spellings can disagree. Both disagreements fail loudly, because a baseline
/// that is neither compared nor excused is exactly what the check below
/// reports: a row without a block is caught when its feature is ON, a block
/// without a row when its feature is OFF.
const FEATURE_GATED_BASELINES: &[(&str, bool)] = &[("dev", cfg!(feature = "block-dev"))];

#[test]
fn endpoint_surface_matches_committed_snapshots() {
    let updating = std::env::var("UPDATE_OPENAPI_SNAPSHOTS").is_ok();
    std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");

    let mut failures = Vec::new();
    let mut checked = Vec::new();
    for info in surface_block_infos() {
        let stem = slug(&info.name);
        let mut actual =
            serde_json::to_string_pretty(&surface_lines(&info)).expect("serialize surface");
        actual.push('\n');
        let path = snapshot_dir().join(format!("{stem}.endpoints.json"));
        checked.push(stem);

        if updating {
            std::fs::write(&path, &actual).expect("write snapshot");
            continue;
        }
        if !path.exists() {
            failures.push(format!(
                "\n=== {} ===\nNo baseline at {}. A new block's declared surface is a \
                 decision: review the lines below, then create the file with \
                 UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test \
                 endpoint_surface\n{actual}",
                info.name,
                path.display()
            ));
            continue;
        }

        let expected = std::fs::read_to_string(&path).expect("read snapshot");
        if expected != actual {
            failures.push(format!(
                "\n=== {} ===\nEndpoint surface differs from {}.\n\
                 Review EVERY changed line: a new line is a path the router now admits at \
                 that level; a changed level is a security decision.\n\
                 Accept with: UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core \
                 --test endpoint_surface",
                info.name,
                path.display()
            ));
        }
    }

    let absent_by_feature: Vec<&str> = FEATURE_GATED_BASELINES
        .iter()
        .filter(|(_, compiled)| !*compiled)
        .map(|(stem, _)| *stem)
        .collect();
    for stem in baselines::unchecked(
        &snapshot_dir(),
        ".endpoints.json",
        &checked,
        &absent_by_feature,
    ) {
        failures.push(format!(
            "\n=== {stem} ===\n{stem}.endpoints.json was not compared by this run, so the gate \
             is vacuous for it. Either the block no longer declares a surface and the baseline \
             should be `git rm`-ed, or it is compiled in only under a feature and belongs in \
             FEATURE_GATED_BASELINES with the `cfg!` that gates it."
        ));
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn slug_matches_the_openapi_snapshot_stems() {
    assert_eq!(slug("impresspress/auth-ui"), "auth_ui");
    assert_eq!(slug("impresspress/llm"), "llm");
}

#[test]
fn surface_lines_are_sorted_and_carry_the_tool_name() {
    use wafer_run::{AuthLevel, BlockEndpoint};
    let info =
        BlockInfo::new("impresspress/probe", "0.0.1", "http-handler@v1", "t").endpoints(vec![
            BlockEndpoint::post("/b/probe/api/things")
                .auth(AuthLevel::Admin)
                .agent_tool("make_thing", "Makes a thing"),
            BlockEndpoint::get("/b/probe/").auth(AuthLevel::Authenticated),
        ]);
    assert_eq!(
        surface_lines(&info),
        vec![
            "GET /b/probe/ authenticated".to_string(),
            "POST /b/probe/api/things admin tool=make_thing".to_string(),
        ]
    );
}
