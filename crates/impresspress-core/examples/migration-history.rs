//! Generate (or check) `src/migration_history/`: every list state each block's
//! migrations were ever in on main, with what a database from before the
//! per-file ledger in that state can do under this build.
//!
//! ```text
//! cargo run -p impresspress-core --example migration-history -- \
//!     --repo <path> --until <main commit> --write
//! cargo run -p impresspress-core --example migration-history -- \
//!     --repo <path> [--base origin/main] --check
//! ```
//!
//! For each block directory under `src/blocks/*/migrations` and each dialect,
//! it walks `git log --reverse <until>` over that directory (and its
//! `crates/solobase-core` predecessor), reads every `*.<dialect>.sql` at each
//! commit, and collapses the commits into distinct list states keyed by
//! `migration_helper::migration_set_hash` — the value a pre-ledger database
//! stores as its `block_settings.current_hash`.
//!
//! Each state gets a verdict by one mechanical rule: today's file F is
//! applied in a state S when S has a file of F's basename whose
//! `content_hash` equals F's, or whose exact text `migration-history.toml`
//! lists under `converges`; the applied files must be a non-empty prefix of
//! today's list. The generator **refuses to guess**: a state before its
//! block's horizon is unsupported by the horizon's reason; any other state
//! that breaks the rule (applied files that are not a prefix, a pending file
//! the state ran under another text that is not `rerun_safe`, a basename
//! today's list lacks that is not `superseded_basename`, nothing applied) is
//! an error unless the config lists it `unsupported`. Every config entry must
//! be used, so no judgment goes stale.
//!
//! Every historical file text that differs from the live file is written,
//! byte-exact, to `src/blocks/<block>/migrations/history/<basename>.<sha8>.<dialect>.sql`,
//! so `migration_history_tests` can rebuild each state.
//!
//! `--check` regenerates in memory and fails on any difference. It also
//! fails unless no commit between the recorded `--until` and the merge-base
//! of `--base` and `HEAD` touches a block's migrations: a state main reached
//! after the table was derived would have no entry, and a database in it
//! would be refused as unknown. Regenerate on rebase.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use impresspress_core::{
    migration_helper::{content_hash, migration_set_hash},
    util::sha256_hex,
};

const CRATE_DIR: &str = "crates/impresspress-core";
const OLD_CRATE_DIR: &str = "crates/solobase-core";
const DIALECTS: [&str; 2] = ["sqlite", "postgres"];

fn main() {
    if let Err(e) = run() {
        eprintln!("migration-history: {e}");
        std::process::exit(1);
    }
}

struct Args {
    repo: PathBuf,
    until: Option<String>,
    base: String,
    write: bool,
}

fn args() -> Result<Args, String> {
    let mut repo = PathBuf::from(".");
    let mut until = None;
    let mut base = "origin/main".to_string();
    let mut mode = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--repo" => repo = PathBuf::from(it.next().ok_or("--repo needs a path")?),
            "--until" => until = Some(it.next().ok_or("--until needs a commit")?),
            "--base" => base = it.next().ok_or("--base needs a ref")?,
            "--write" => mode = Some(true),
            "--check" => mode = Some(false),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    let write = mode.ok_or("pass --write or --check")?;
    if write && until.is_none() {
        return Err("--write needs --until <main commit>".into());
    }
    Ok(Args {
        repo,
        until,
        base,
        write,
    })
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn git_text(repo: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git(repo, args)?).map_err(|e| format!("git {args:?}: {e}"))
}

fn sha8(text: &str) -> String {
    sha256_hex(text.as_bytes())[..8].to_string()
}

// ---------------------------------------------------------------------------
// The reviewed config
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    horizon: Vec<StateJudgment>,
    #[serde(default)]
    unsupported: Vec<StateJudgment>,
    #[serde(default)]
    converges: Vec<FileJudgment>,
    #[serde(default)]
    rerun_safe: Vec<FileJudgment>,
    #[serde(default)]
    superseded_basename: Vec<FileJudgment>,
}

/// A judgment about one list state of one block in one dialect.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StateJudgment {
    block: String,
    dialect: String,
    /// A unique prefix of the state's list hash.
    state: String,
    reason: String,
}

/// A judgment about one file: `block/basename`, or `block/basename@sha8`
/// naming one exact historical text.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileJudgment {
    file: String,
    reason: String,
}

// ---------------------------------------------------------------------------
// Walking main
// ---------------------------------------------------------------------------

struct State {
    list_hash: String,
    first_seen: String,
    /// `(basename, whole text)`, in list order.
    files: Vec<(String, String)>,
}

fn states(repo: &Path, until: &str, block: &str, dialect: &str) -> Result<Vec<State>, String> {
    let dirs = [
        format!("{CRATE_DIR}/src/blocks/{block}/migrations"),
        format!("{OLD_CRATE_DIR}/src/blocks/{block}/migrations"),
    ];
    let suffix = format!(".{dialect}.sql");
    let log = git_text(
        repo,
        &[
            "log",
            "--reverse",
            "--format=%H %as",
            until,
            "--",
            &dirs[0],
            &dirs[1],
        ],
    )?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for line in log.lines() {
        let (commit, date) = line.split_once(' ').ok_or("unexpected git log line")?;
        let mut files = Vec::new();
        for dir in &dirs {
            let listing = git_text(
                repo,
                &["ls-tree", "--name-only", commit, &format!("{dir}/")],
            )?;
            let mut names: Vec<&str> = listing.lines().filter(|n| n.ends_with(&suffix)).collect();
            names.sort();
            if names.is_empty() {
                continue;
            }
            for path in names {
                let basename = path
                    .rsplit('/')
                    .next()
                    .and_then(|n| n.strip_suffix(&suffix))
                    .ok_or("unexpected path")?
                    .to_string();
                let text = git_text(repo, &["show", &format!("{commit}:{path}")])?;
                files.push((basename, text));
            }
            break;
        }
        if files.is_empty() {
            continue;
        }
        let pairs: Vec<(&str, &str)> = files
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        let list_hash = migration_set_hash(&pairs);
        if !seen.insert(list_hash.clone()) {
            continue;
        }
        out.push(State {
            list_hash,
            first_seen: format!("{date} {}", &commit[..8]),
            files,
        });
    }
    Ok(out)
}

/// The live files of `block` in `dialect`, in list order, from the work tree.
fn live_files(
    crate_dir: &Path,
    block: &str,
    dialect: &str,
) -> Result<Vec<(String, String)>, String> {
    let dir = crate_dir.join("src/blocks").join(block).join("migrations");
    let suffix = format!(".{dialect}.sql");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(basename) = name.strip_suffix(&suffix) {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            out.push((basename.to_string(), text));
        }
    }
    out.sort();
    Ok(out)
}

fn blocks(crate_dir: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(crate_dir.join("src/blocks")).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.join("migrations").is_dir() {
            out.push(
                path.file_name()
                    .and_then(|n| n.to_str())
                    .ok_or("block name")?
                    .to_string(),
            );
        }
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

enum Verdict {
    Applied(Vec<String>),
    Unsupported(String),
}

struct Used {
    horizon: BTreeSet<usize>,
    unsupported: BTreeSet<usize>,
    converges: BTreeSet<usize>,
    rerun_safe: BTreeSet<usize>,
    superseded: BTreeSet<usize>,
}

fn find_state<'c>(
    judgments: &'c [StateJudgment],
    block: &str,
    dialect: &str,
    state: &State,
) -> Option<(usize, &'c StateJudgment)> {
    judgments.iter().enumerate().find(|(_, j)| {
        j.block == block && j.dialect == dialect && state.list_hash.starts_with(&j.state)
    })
}

fn verdicts(
    config: &Config,
    used: &mut Used,
    block: &str,
    dialect: &str,
    states: &[State],
    live: &[(String, String)],
) -> Result<Vec<Verdict>, String> {
    for j in config.horizon.iter().chain(&config.unsupported) {
        if j.block == block && j.dialect == dialect {
            let matching = states
                .iter()
                .filter(|s| s.list_hash.starts_with(&j.state))
                .count();
            if matching != 1 {
                return Err(format!(
                    "{block} ({dialect}): config state `{}` matches {matching} states",
                    j.state
                ));
            }
        }
    }
    let horizon = states.iter().enumerate().find_map(|(i, s)| {
        find_state(&config.horizon, block, dialect, s).map(|(index, j)| (i, index, j))
    });
    let mut out = Vec::new();
    for (i, state) in states.iter().enumerate() {
        if let Some((at, index, j)) = horizon {
            if i < at {
                used.horizon.insert(index);
                out.push(Verdict::Unsupported(j.reason.clone()));
                continue;
            }
        }
        if let Some((index, j)) = find_state(&config.unsupported, block, dialect, state) {
            used.unsupported.insert(index);
            out.push(Verdict::Unsupported(j.reason.clone()));
            continue;
        }
        out.push(Verdict::Applied(applied_prefix(
            config, used, block, dialect, state, live,
        )?));
    }
    Ok(out)
}

fn applied_prefix(
    config: &Config,
    used: &mut Used,
    block: &str,
    dialect: &str,
    state: &State,
    live: &[(String, String)],
) -> Result<Vec<String>, String> {
    let at = format!(
        "{block} ({dialect}) state {} ({})",
        &state.list_hash[..8],
        state.first_seen
    );
    let applied: Vec<bool> = live
        .iter()
        .map(|(name, text)| {
            state
                .files
                .iter()
                .find(|(n, _)| n == name)
                .is_some_and(|(_, old)| {
                    if content_hash(old) == content_hash(text) {
                        return true;
                    }
                    let key = format!("{block}/{name}@{}", sha8(old));
                    match config.converges.iter().position(|j| j.file == key) {
                        Some(index) => {
                            used.converges.insert(index);
                            true
                        }
                        None => false,
                    }
                })
        })
        .collect();
    let k = applied.iter().take_while(|a| **a).count();
    // The older texts the state ran of today's files that are not applied:
    // what a `converges` or `rerun_safe` entry would name.
    let other_texts: Vec<String> = live
        .iter()
        .zip(&applied)
        .filter(|(_, applied)| !**applied)
        .filter_map(|((name, _), _)| {
            state
                .files
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, old)| format!("{block}/{name}@{}", sha8(old)))
        })
        .collect();
    if k == 0 {
        return Err(format!(
            "{at}: none of today's files is applied (older texts: {other_texts:?}); list it \
             `unsupported`, move the horizon, or name a `converges` text"
        ));
    }
    if let Some(later) = applied[k..].iter().position(|a| *a) {
        return Err(format!(
            "{at}: `{}` is applied after the pending `{}`, so the state is not a prefix (older \
             texts: {other_texts:?}); list it `unsupported` or name a `converges` text",
            live[k + later].0,
            live[k].0
        ));
    }
    for (name, _) in &live[k..] {
        if state.files.iter().any(|(n, _)| n == name) {
            let key = format!("{block}/{name}");
            match config.rerun_safe.iter().position(|j| j.file == key) {
                Some(index) => {
                    used.rerun_safe.insert(index);
                }
                None => {
                    return Err(format!(
                        "{at}: `{name}` ran under another text and is pending (older texts: \
                         {other_texts:?}); name that text `converges`, list the file \
                         `rerun_safe` (its current statements are idempotent), or list the state \
                         `unsupported`"
                    ))
                }
            }
        }
    }
    for (name, _) in &state.files {
        if !live.iter().any(|(n, _)| n == name) {
            let key = format!("{block}/{name}");
            match config
                .superseded_basename
                .iter()
                .position(|j| j.file == key)
            {
                Some(index) => {
                    used.superseded.insert(index);
                }
                None => {
                    return Err(format!(
                        "{at}: `{name}` is not in today's list; list it `superseded_basename` or \
                         the state `unsupported`"
                    ))
                }
            }
        }
    }
    Ok(live[..k].iter().map(|(n, _)| n.clone()).collect())
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn rust_str(s: &str) -> String {
    format!("{s:?}")
}

fn render_block(
    block: &str,
    until: &str,
    per_dialect: &[(&str, Vec<State>, Vec<Verdict>)],
) -> String {
    let mut out = format!(
        "//! The `{block}` block's migration list states on main, oldest first.\n\
         //!\n\
         //! Generated by `examples/migration-history.rs` from main through\n\
         //! `{until}` and `migration-history.toml`. Do not edit: regenerate, and\n\
         //! review the diff.\n\n\
         use crate::migration_helper::{{HistoricalState, Verdict}};\n"
    );
    for (dialect, states, verdicts) in per_dialect {
        let name = dialect.to_ascii_uppercase();
        out.push_str(&format!(
            "\n/// `{block}`'s {dialect} list states.\npub const {name}: &[HistoricalState] = &[\n"
        ));
        for (state, verdict) in states.iter().zip(verdicts) {
            let composition: Vec<String> = state
                .files
                .iter()
                .map(|(n, t)| rust_str(&format!("{n}@{}", sha8(t))))
                .collect();
            let verdict = match verdict {
                Verdict::Applied(names) => format!(
                    "Verdict::Applied(&[{}])",
                    names
                        .iter()
                        .map(|n| rust_str(n))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Verdict::Unsupported(reason) => {
                    format!("Verdict::Unsupported({})", rust_str(reason))
                }
            };
            out.push_str(&format!(
                "    HistoricalState {{\n        list_hash: {},\n        first_seen: {},\n        \
                 composition: &[{}],\n        verdict: {verdict},\n    }},\n",
                rust_str(&state.list_hash),
                rust_str(&state.first_seen),
                composition.join(", "),
            ));
        }
        out.push_str("];\n");
    }
    out
}

fn render_mod(blocks: &[String], until: &str) -> String {
    let mut out = format!(
        "//! Every list state each block's migrations were in on main before the\n\
         //! per-file migration ledger, with what a database in that state can do\n\
         //! under this build (`migration_helper`, \"Databases from before the\n\
         //! ledger\").\n\
         //!\n\
         //! Generated by `examples/migration-history.rs` from main through\n\
         //! [`DERIVED_THROUGH`] and `migration-history.toml`; CI runs it with\n\
         //! `--check`. Do not edit: regenerate, and review the diff.\n\n\
         /// The main commit the tables were derived through: the ledger's\n\
         /// merge-base. A state main reached after it is a ledger-era state and\n\
         /// never needs an entry.\n\
         pub const DERIVED_THROUGH: &str = {};\n\n",
        rust_str(until)
    );
    for block in blocks {
        out.push_str(&format!("#[rustfmt::skip]\npub mod {block};\n"));
    }
    out
}

/// Every file `migration_history` should hold: the generated modules and the
/// fixtures, by path relative to the crate.
fn generate(
    repo: &Path,
    crate_dir: &Path,
    until: &str,
) -> Result<BTreeMap<PathBuf, String>, String> {
    let config_path = crate_dir.join("migration-history.toml");
    let config: Config = toml::from_str(
        &std::fs::read_to_string(&config_path)
            .map_err(|e| format!("{}: {e}", config_path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", config_path.display()))?;
    let reasons = config
        .horizon
        .iter()
        .chain(&config.unsupported)
        .map(|j| &j.reason)
        .chain(
            config
                .converges
                .iter()
                .chain(&config.rerun_safe)
                .chain(&config.superseded_basename)
                .map(|j| &j.reason),
        );
    if reasons.into_iter().any(|r| r.trim().is_empty()) {
        return Err("migration-history.toml: every entry needs a reason".into());
    }
    let mut used = Used {
        horizon: BTreeSet::new(),
        unsupported: BTreeSet::new(),
        converges: BTreeSet::new(),
        rerun_safe: BTreeSet::new(),
        superseded: BTreeSet::new(),
    };
    let blocks = blocks(crate_dir)?;
    let mut files = BTreeMap::new();
    for block in &blocks {
        let mut per_dialect = Vec::new();
        for dialect in DIALECTS {
            let states = states(repo, until, block, dialect)?;
            let live = live_files(crate_dir, block, dialect)?;
            let verdicts = verdicts(&config, &mut used, block, dialect, &states, &live)?;
            for state in &states {
                for (name, text) in &state.files {
                    let is_live = live.iter().any(|(n, t)| n == name && t == text);
                    if !is_live {
                        files.insert(
                            PathBuf::from(format!(
                                "src/blocks/{block}/migrations/history/{name}.{}.{dialect}.sql",
                                sha8(text)
                            )),
                            text.clone(),
                        );
                    }
                }
            }
            per_dialect.push((dialect, states, verdicts));
        }
        files.insert(
            PathBuf::from(format!("src/migration_history/{block}.rs")),
            render_block(block, until, &per_dialect),
        );
    }
    files.insert(
        PathBuf::from("src/migration_history/mod.rs"),
        render_mod(&blocks, until),
    );

    let unused = |kind: &str, count: usize, used: &BTreeSet<usize>| -> Result<(), String> {
        match (0..count).find(|i| !used.contains(i)) {
            Some(i) => Err(format!(
                "migration-history.toml: {kind} entry #{i} is never used"
            )),
            None => Ok(()),
        }
    };
    unused("horizon", config.horizon.len(), &used.horizon)?;
    unused("unsupported", config.unsupported.len(), &used.unsupported)?;
    unused("converges", config.converges.len(), &used.converges)?;
    unused("rerun_safe", config.rerun_safe.len(), &used.rerun_safe)?;
    unused(
        "superseded_basename",
        config.superseded_basename.len(),
        &used.superseded,
    )?;
    Ok(files)
}

/// Every file under the generated locations now, for `--check` and for
/// removing stale fixtures on `--write`.
fn existing(crate_dir: &Path, blocks: &[String]) -> Result<BTreeSet<PathBuf>, String> {
    let mut out = BTreeSet::new();
    let mut dirs = vec![PathBuf::from("src/migration_history")];
    dirs.extend(
        blocks
            .iter()
            .map(|b| PathBuf::from(format!("src/blocks/{b}/migrations/history"))),
    );
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(crate_dir.join(&dir)) else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            out.insert(dir.join(entry.file_name()));
        }
    }
    Ok(out)
}

fn recorded_until(crate_dir: &Path) -> Result<String, String> {
    let path = crate_dir.join("src/migration_history/mod.rs");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .find_map(|l| l.strip_prefix("pub const DERIVED_THROUGH: &str = \""))
        .and_then(|l| l.strip_suffix("\";"))
        .map(str::to_string)
        .ok_or_else(|| format!("{}: no DERIVED_THROUGH", path.display()))
}

fn run() -> Result<(), String> {
    let args = args()?;
    let repo = args.repo;
    let crate_dir = repo.join(CRATE_DIR);
    let blocks = blocks(&crate_dir)?;
    if args.write {
        let until = git_text(
            &repo,
            &["rev-parse", args.until.as_deref().unwrap_or_default()],
        )?
        .trim()
        .to_string();
        let files = generate(&repo, &crate_dir, &until)?;
        for stale in existing(&crate_dir, &blocks)? {
            if !files.contains_key(&stale) {
                std::fs::remove_file(crate_dir.join(&stale)).map_err(|e| e.to_string())?;
            }
        }
        for (path, text) in &files {
            let full = crate_dir.join(path);
            std::fs::create_dir_all(full.parent().ok_or("path")?).map_err(|e| e.to_string())?;
            std::fs::write(&full, text).map_err(|e| format!("{}: {e}", full.display()))?;
        }
        println!("wrote {} files, derived through {until}", files.len());
        return Ok(());
    }

    let until = recorded_until(&crate_dir)?;
    let merge_base = git_text(&repo, &["merge-base", &args.base, "HEAD"])?
        .trim()
        .to_string();
    git(&repo, &["merge-base", "--is-ancestor", &until, &merge_base]).map_err(|_| {
        format!("the table was derived through {until}, which is not an ancestor of {merge_base}")
    })?;
    let range = format!("{until}..{merge_base}");
    let moved = git_text(
        &repo,
        &[
            "log",
            "--format=%h %s",
            &range,
            "--",
            &format!(":(glob){CRATE_DIR}/src/blocks/*/migrations/*.sql"),
        ],
    )?;
    if !moved.trim().is_empty() {
        return Err(format!(
            "main changed migrations after the table was derived ({until}); regenerate with \
             --until {merge_base}:\n{moved}"
        ));
    }
    let files = generate(&repo, &crate_dir, &until)?;
    let mut problems = Vec::new();
    for (path, text) in &files {
        match std::fs::read_to_string(crate_dir.join(path)) {
            Ok(current) if &current == text => {}
            Ok(_) => problems.push(format!("differs: {}", path.display())),
            Err(_) => problems.push(format!("missing: {}", path.display())),
        }
    }
    for path in existing(&crate_dir, &blocks)? {
        if !files.contains_key(&path) {
            problems.push(format!("not generated: {}", path.display()));
        }
    }
    if problems.is_empty() {
        println!("migration history is current, derived through {until}");
        Ok(())
    } else {
        Err(format!(
            "the checked-in migration history is not what the generator produces; run it with \
             --write and review the diff:\n  {}",
            problems.join("\n  ")
        ))
    }
}
