//! Build the password-hasher Worker, and deploy or keep it (step 0 of a
//! Cloudflare deploy, [`release`]).
//!
//! The hasher is `impresspress-password`'s Durable Object class
//! (`impresspress_password::durable_object`) in a Worker of its own (see
//! [`super::wrangler::generate_password_hasher`] for why it cannot live in the
//! consumer's Worker). A consumer crate does not declare it; it already
//! depends on `impresspress-password`, through `impresspress-cloudflare`. So
//! the build stages a two-file crate under
//! `target/impresspress-password-hasher/` that depends on that same
//! `impresspress-password` — the same source and, through a copy of the
//! consumer's `Cargo.lock`, the same versions of everything under it,
//! wafer-run's crypto included — and runs `worker-build` on it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use super::first_create::WorkerCommands;

/// The crate that carries the hasher's Durable Object class.
const PASSWORD_CRATE: &str = "impresspress-password";

/// Where the hasher crate is staged and built, under the consumer's
/// `target/`. Outside `target/impresspress-cloudflare`, which every build
/// deletes, so its own `target/` keeps the compile cache between builds.
pub fn crate_dir(repo_root: &Path) -> PathBuf {
    repo_root.join("target/impresspress-password-hasher")
}

/// Where the consumer resolved `impresspress-password` from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordCrateSource {
    /// A path dependency (this repository's own examples, or a consumer that
    /// vendors impresspress): the crate's directory.
    Path(PathBuf),
    /// A git dependency, at the exact commit the consumer's lockfile holds.
    Git { url: String, rev: String },
    /// A registry release.
    Registry { version: String },
}

impl PasswordCrateSource {
    /// Read the source from one `cargo metadata` package entry: `source` is
    /// `null` for a path dependency, `git+<url>?<ref>#<commit>` for git, and
    /// `registry+…` (or `sparse+…`) for a registry.
    fn from_metadata(package: &MetadataPackage) -> Result<Self> {
        let Some(source) = package.source.as_deref() else {
            let dir = Path::new(&package.manifest_path)
                .parent()
                .context("impresspress-password's manifest path has no parent")?;
            return Ok(Self::Path(dir.to_path_buf()));
        };
        if let Some(git) = source.strip_prefix("git+") {
            let (location, commit) = git
                .split_once('#')
                .with_context(|| format!("git source {source:?} names no commit"))?;
            let url = location.split('?').next().unwrap_or(location).to_string();
            return Ok(Self::Git {
                url,
                rev: commit.to_string(),
            });
        }
        if source.starts_with("registry+") || source.starts_with("sparse+") {
            return Ok(Self::Registry {
                version: package.version.clone(),
            });
        }
        bail!("unsupported source {source:?} for {PASSWORD_CRATE}")
    }

    /// The dependency entry for the staged crate's manifest.
    fn dependency(&self) -> toml::Value {
        let mut dep = toml::map::Map::new();
        match self {
            Self::Path(dir) => {
                dep.insert(
                    "path".into(),
                    toml::Value::String(dir.to_string_lossy().into_owned()),
                );
            }
            Self::Git { url, rev } => {
                dep.insert("git".into(), toml::Value::String(url.clone()));
                dep.insert("rev".into(), toml::Value::String(rev.clone()));
            }
            Self::Registry { version } => {
                dep.insert("version".into(), toml::Value::String(format!("={version}")));
            }
        }
        dep.insert(
            "features".into(),
            toml::Value::Array(vec![toml::Value::String("durable-object".into())]),
        );
        toml::Value::Table(dep)
    }
}

#[derive(Debug, Deserialize)]
pub struct Metadata {
    packages: Vec<MetadataPackage>,
    workspace_root: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct MetadataPackage {
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: String,
}

impl Metadata {
    /// Where `impresspress-password` comes from in this dependency graph.
    pub fn password_crate_source(&self) -> Result<PasswordCrateSource> {
        let mut found = self
            .packages
            .iter()
            .filter(|package| package.name == PASSWORD_CRATE);
        let package = found.next().ok_or_else(|| {
            anyhow!(
                "{PASSWORD_CRATE} is not in the consumer's dependency graph; a Cloudflare \
                 consumer depends on it through impresspress-cloudflare"
            )
        })?;
        if found.next().is_some() {
            bail!(
                "{PASSWORD_CRATE} appears more than once in the consumer's dependency graph; \
                 the password-hasher Worker must be built from the one the main Worker links"
            );
        }
        PasswordCrateSource::from_metadata(package)
    }
}

/// `cargo metadata` for the consumer crate, with its lockfile left as is.
async fn consumer_metadata(repo_root: &Path) -> Result<Metadata> {
    let output = Command::new("cargo")
        .current_dir(repo_root)
        .args(["metadata", "--format-version", "1"])
        .output()
        .await
        .context("run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parse cargo metadata")
}

/// The staged crate's `Cargo.toml`.
///
/// Its own `[workspace]`, so the consumer's workspace does not claim it; a
/// release profile tuned for the argon2 loop rather than for size (the hasher
/// is a few hundred KB either way, and its CPU time is the Durable Object's
/// whole cost); and a `target-cloudflare` feature because `worker-build` is
/// driven with the same flags as the main Worker's build.
pub fn manifest(source: &PasswordCrateSource) -> Result<String> {
    use toml::Value;
    let mut package = toml::map::Map::new();
    package.insert(
        "name".into(),
        Value::String("impresspress-password-hasher-worker".into()),
    );
    package.insert("version".into(), Value::String("0.0.0".into()));
    package.insert("edition".into(), Value::String("2021".into()));
    package.insert("publish".into(), Value::Boolean(false));

    let mut lib = toml::map::Map::new();
    lib.insert(
        "crate-type".into(),
        Value::Array(vec![Value::String("cdylib".into())]),
    );

    let mut features = toml::map::Map::new();
    features.insert("default".into(), Value::Array(Vec::new()));
    features.insert("target-cloudflare".into(), Value::Array(Vec::new()));

    let mut dependencies = toml::map::Map::new();
    dependencies.insert(PASSWORD_CRATE.into(), source.dependency());

    let mut release = toml::map::Map::new();
    release.insert("opt-level".into(), Value::Integer(3));
    release.insert("lto".into(), Value::Boolean(true));
    release.insert("codegen-units".into(), Value::Integer(1));
    release.insert("strip".into(), Value::Boolean(true));
    release.insert("panic".into(), Value::String("abort".into()));
    let mut profile = toml::map::Map::new();
    profile.insert("release".into(), Value::Table(release));

    let mut root = toml::map::Map::new();
    root.insert("package".into(), Value::Table(package));
    root.insert("lib".into(), Value::Table(lib));
    root.insert("features".into(), Value::Table(features));
    root.insert("dependencies".into(), Value::Table(dependencies));
    root.insert("profile".into(), Value::Table(profile));
    root.insert("workspace".into(), Value::Table(toml::map::Map::new()));
    let body = toml::to_string_pretty(&Value::Table(root)).context("serialize Cargo.toml")?;
    Ok(format!(
        "# Generated by `impresspress build --target cloudflare`: the password-hasher \
         Worker. Do not edit.\n\n{body}"
    ))
}

/// The staged crate's `src/lib.rs`. The re-export is what links the class —
/// and the Worker's `fetch` entry point beside it — into the `cdylib`.
pub const LIB_RS: &str = "// Generated by `impresspress build --target cloudflare`. Do not edit.\n\
pub use impresspress_password::durable_object::ImpresspressPasswordHasher;\n";

/// Write the hasher crate into [`crate_dir`]. Rewritten on every build, so it
/// always follows the consumer's current `impresspress-password` and lockfile.
pub fn stage(repo_root: &Path, metadata: &Metadata) -> Result<PathBuf> {
    let dir = crate_dir(repo_root);
    std::fs::create_dir_all(dir.join("src"))
        .with_context(|| format!("create {}", dir.display()))?;
    let source = metadata.password_crate_source()?;
    std::fs::write(dir.join("Cargo.toml"), manifest(&source)?)
        .with_context(|| format!("write {}/Cargo.toml", dir.display()))?;
    std::fs::write(dir.join("src/lib.rs"), LIB_RS)
        .with_context(|| format!("write {}/src/lib.rs", dir.display()))?;
    let lock = metadata.workspace_root.join("Cargo.lock");
    if lock.is_file() {
        std::fs::copy(&lock, dir.join("Cargo.lock"))
            .with_context(|| format!("copy {} into {}", lock.display(), dir.display()))?;
    }
    Ok(dir)
}

/// Stage and cross-compile the hasher Worker to
/// `target/impresspress-password-hasher/build/worker/shim.mjs`, the `main` of
/// the config [`super::wrangler::generate_password_hasher`] writes.
pub async fn build(repo_root: &Path) -> Result<PathBuf> {
    let metadata = consumer_metadata(repo_root).await?;
    let dir = stage(repo_root, &metadata)?;
    super::build::ensure_worker_build_installed().await?;
    let status = Command::new("worker-build")
        .current_dir(&dir)
        .args(["--no-default-features", "--features", "target-cloudflare"])
        .status()
        .await
        .context("run worker-build for the password-hasher Worker")?;
    if !status.success() {
        bail!(
            "worker-build failed for the password-hasher Worker (exit {:?})",
            status.code()
        );
    }
    Ok(dir.join("build/index_bg.wasm"))
}

/// Refuse to put a main Worker version in front of a hasher that lacks the
/// pepper the main Worker still holds.
///
/// A site that set its pepper before hashing moved to the hasher has the keys
/// as the MAIN Worker's secrets, which nothing reads any more. Deploying on
/// would leave every peppered account unable to sign in (a pepper fault, 503)
/// and — worse, and silently — have every sign-up and password change write
/// an unpeppered hash. So: for each pepper key secret the main Worker holds,
/// the hasher must hold one of the same name. Values are never read; the
/// names come from `wrangler secret list`.
///
/// `Ok` carries the names the main Worker still holds that the hasher has
/// too, which the operator should delete from the main Worker.
pub fn check_pepper_placement(
    main_secrets: &[String],
    hasher_secrets: &[String],
    main_worker: &str,
    hasher_worker: &str,
) -> Result<Vec<&'static str>> {
    use impresspress_password::pepper::{
        PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_PREVIOUS_KEYS_VAR,
    };
    let holds = |secrets: &[String], name: &str| secrets.iter().any(|s| s == name);
    let mut leftovers = Vec::new();
    let mut missing = Vec::new();
    for name in [PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_PREVIOUS_KEYS_VAR] {
        if holds(main_secrets, name) {
            if holds(hasher_secrets, name) {
                leftovers.push(name);
            } else {
                missing.push(name);
            }
        }
    }
    if missing.is_empty() {
        return Ok(leftovers);
    }
    let steps: String = missing
        .iter()
        .map(|name| {
            format!(
                "\n  npx wrangler secret put {name} --name {hasher_worker}   \
                 # the same value the main Worker holds\n  npx wrangler secret delete {name} \
                 --name {main_worker}"
            )
        })
        .collect();
    bail!(
        "the main Worker {main_worker} holds {} but the password-hasher Worker {hasher_worker} \
         does not. Passwords are hashed by the hasher now, with its own secrets: deploying on \
         would lock out every peppered account and write unpeppered hashes for every sign-up \
         and password change. Nothing of the main Worker has been uploaded. Move the secret(s), \
         then run the deploy again:{steps}\n(and set [cloudflare.password_hasher].\
         pepper_required = true in impresspress.toml if the pepper was required)",
        missing.join(" and ")
    )
}

/// The secret names `wrangler secret list --format json` printed: a JSON
/// array of `{ "name": …, "type": … }`. Wrangler may print a banner first,
/// so the array is read from its first `[`.
pub fn parse_secret_names(stdout: &str) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Secret {
        name: String,
    }
    let start = stdout
        .find('[')
        .context("no JSON array in `wrangler secret list` output")?;
    let secrets: Vec<Secret> =
        serde_json::from_str(&stdout[start..]).context("parse `wrangler secret list` output")?;
    Ok(secrets.into_iter().map(|secret| secret.name).collect())
}

/// What `impresspress deploy --target cloudflare` does with the
/// password-hasher Worker (`--password-hasher`).
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PasswordHasherMode {
    /// Build the hasher from this release and deploy it before any main
    /// Worker version is uploaded.
    #[default]
    Deploy,
    /// Leave the deployed hasher as it is: neither build nor deploy it. The
    /// kept version must already hold the vars this release would deploy
    /// (`[cloudflare.password_hasher]`, e.g. `pepper_required`), or the
    /// deploy refuses; the deploy still reads its secret names for the
    /// pepper check and records the version it keeps, and promotion still
    /// needs it to answer this release's protocol version (the
    /// `/_deploy/verify` probe).
    Keep,
}

/// The password-hasher Worker this deploy's main Worker versions will call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HasherRelease {
    /// This deploy deployed it.
    Deployed,
    /// This deploy left it at this version.
    Kept { version_id: String },
}

/// Step 0 of a Cloudflare deploy: put the hasher every main Worker version of
/// this deploy will call in place, before any of them is uploaded.
///
/// [`PasswordHasherMode::Deploy`] runs plain `wrangler deploy` on `config`:
/// it uploads and activates in one step, and applies the config's Durable
/// Object migration, which `wrangler versions upload` cannot carry. The hasher
/// has no preview to verify first — Cloudflare generates none for a Worker
/// that implements a Durable Object — so what keeps that safe is the
/// protocol's compatibility rule (`impresspress_password::protocol`): the
/// hasher answers the live main Worker's requests as well as the new one's.
///
/// [`PasswordHasherMode::Keep`] runs no deploy. It reads the hasher's current
/// deployment (`wrangler deployments status --json`), which must serve one
/// version to all traffic, then that version (`wrangler versions view
/// --json`), whose vars must be exactly `vars`, the ones a deploy of this
/// release would apply (`wrangler::password_hasher_vars`). It returns that
/// version. A hasher that does not exist, splits its traffic, or holds other
/// vars refuses the deploy: the version the main Worker would call must be
/// one known version, configured as this release says. Without the vars
/// check, a `pepper_required = true` the operator just set would look
/// applied while the hasher kept running without it.
pub fn release(
    commands: &impl WorkerCommands,
    config: &Path,
    mode: PasswordHasherMode,
    vars: &BTreeMap<String, String>,
) -> Result<HasherRelease> {
    match mode {
        PasswordHasherMode::Deploy => {
            commands.deploy(config)?;
            Ok(HasherRelease::Deployed)
        }
        PasswordHasherMode::Keep => {
            let status = commands.deployment_status(config).context(
                "--password-hasher keep needs the password-hasher Worker to be deployed already",
            )?;
            let version_id = parse_deployment_status(&status)?.sole_version()?;
            let view = commands.version_view(config, &version_id)?;
            parse_version_view(&view)?.check_vars(&version_id, vars)?;
            Ok(HasherRelease::Kept { version_id })
        }
    }
}

/// `wrangler deployments status --json`: Cloudflare's latest deployment of a
/// Worker, as its API returns it. Only the fields the deploy reads; the
/// object carries more (`source`, `strategy`, `author_email`,
/// `annotations`, …).
///
/// `id` is only named in an error; a missing one reads as empty rather than
/// refusing the deploy.
#[derive(Debug, Deserialize)]
pub struct DeploymentStatus {
    #[serde(default)]
    pub id: String,
    pub versions: Vec<VersionTraffic>,
}

/// One version of a deployment and the share of traffic it serves.
#[derive(Debug, Deserialize)]
pub struct VersionTraffic {
    pub version_id: String,
    pub percentage: f64,
}

impl DeploymentStatus {
    /// The version serving all traffic, when exactly one does.
    pub fn sole_version(&self) -> Result<String> {
        match self.versions.as_slice() {
            [only] if only.percentage == 100.0 && !only.version_id.is_empty() => {
                Ok(only.version_id.clone())
            }
            versions => bail!(
                "deployment {} serves {} version(s) ({}); --password-hasher keep needs one \
                 version at 100%",
                self.id,
                versions.len(),
                versions
                    .iter()
                    .map(|v| format!("{} at {}%", v.version_id, v.percentage))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// `wrangler versions view <id> --json`: one version of a Worker, as
/// Cloudflare's API returns it. Only its bindings are read.
#[derive(Debug, Deserialize)]
pub struct VersionView {
    pub resources: VersionResources,
}

/// The resources of a version; only its bindings are read.
#[derive(Debug, Deserialize)]
pub struct VersionResources {
    pub bindings: Vec<VersionBinding>,
}

/// One binding of a version. A var is `plain_text` (its value in `text`) or
/// `json` (in `json`); every other type (`secret_text`, `durable_object_namespace`,
/// …) is not a var.
#[derive(Debug, Deserialize)]
pub struct VersionBinding {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub json: Option<serde_json::Value>,
}

impl VersionView {
    /// The version's vars, by name: a `plain_text` var's text, a `json`
    /// var's JSON text.
    pub fn vars(&self) -> BTreeMap<String, String> {
        self.resources
            .bindings
            .iter()
            .filter_map(|binding| match binding.kind.as_str() {
                "plain_text" => Some((
                    binding.name.clone(),
                    binding.text.clone().unwrap_or_default(),
                )),
                "json" => Some((
                    binding.name.clone(),
                    binding
                        .json
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                )),
                _ => None,
            })
            .collect()
    }

    /// Refuse unless the version's vars are exactly `expected`, naming each
    /// one that differs.
    pub fn check_vars(&self, version_id: &str, expected: &BTreeMap<String, String>) -> Result<()> {
        let held = self.vars();
        let differences: Vec<String> = expected
            .keys()
            .chain(held.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|name| expected.get(*name) != held.get(*name))
            .map(|name| {
                let show = |value: Option<&String>| {
                    value.map_or_else(|| "unset".to_string(), |v| format!("{v:?}"))
                };
                format!(
                    "{name}: kept version has {}, this release deploys {}",
                    show(held.get(name)),
                    show(expected.get(name))
                )
            })
            .collect();
        if differences.is_empty() {
            return Ok(());
        }
        bail!(
            "the kept password-hasher version {version_id} is not configured as this release \
             says ([cloudflare.password_hasher] in impresspress.toml):\n  {}\nDeploy the hasher \
             to apply it: impresspress deploy --target cloudflare --password-hasher deploy",
            differences.join("\n  ")
        )
    }
}

/// Read `wrangler versions view --json` output, from its first `{`.
pub fn parse_version_view(stdout: &str) -> Result<VersionView> {
    let start = stdout
        .find('{')
        .context("no JSON object in `wrangler versions view --json` output")?;
    serde_json::from_str(&stdout[start..]).context("parse `wrangler versions view --json`")
}

/// Read `wrangler deployments status --json` output. Wrangler prints no
/// banner with `--json`; the object is still read from its first `{` so a
/// stray line before it cannot break the parse.
pub fn parse_deployment_status(stdout: &str) -> Result<DeploymentStatus> {
    let start = stdout
        .find('{')
        .context("no JSON object in `wrangler deployments status --json` output")?;
    serde_json::from_str(&stdout[start..]).context("parse `wrangler deployments status --json`")
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "IMPRESSPRESS_PASSWORD_PEPPER_KEY";
    const PREVIOUS: &str = "IMPRESSPRESS_PASSWORD_PEPPER_PREVIOUS_KEYS";

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// The upgrade gap: the pepper is still the main Worker's and the hasher
    /// has none. Refused, naming the secret and the exact steps.
    #[test]
    fn a_pepper_left_on_the_main_worker_refuses_the_deploy() {
        for main in [vec![KEY], vec![KEY, PREVIOUS], vec![PREVIOUS]] {
            let err = check_pepper_placement(
                &names(&main),
                &names(&["OTHER"]),
                "site",
                "site-password-hasher",
            )
            .expect_err("refused");
            let err = format!("{err:#}");
            for name in &main {
                assert!(
                    err.contains(&format!(
                        "npx wrangler secret put {name} --name site-password-hasher"
                    )) && err.contains(&format!("npx wrangler secret delete {name} --name site")),
                    "{err}"
                );
            }
        }
        // The key moved but the previous keys did not: still refused.
        let err = check_pepper_placement(
            &names(&[KEY, PREVIOUS]),
            &names(&[KEY]),
            "site",
            "site-password-hasher",
        )
        .expect_err("previous keys missing");
        assert!(format!("{err:#}").contains(PREVIOUS));
    }

    #[test]
    fn a_pepper_on_the_hasher_or_nowhere_is_fine() {
        assert!(check_pepper_placement(&[], &[], "m", "h")
            .unwrap()
            .is_empty());
        assert!(check_pepper_placement(
            &names(&["IMPRESSPRESS_DEPLOY_TOKEN"]),
            &names(&[KEY]),
            "m",
            "h"
        )
        .unwrap()
        .is_empty());
        // Moved, but not yet deleted from the main Worker: allowed, and named.
        assert_eq!(
            check_pepper_placement(&names(&[KEY]), &names(&[KEY]), "m", "h").unwrap(),
            vec![KEY]
        );
    }

    #[test]
    fn reads_secret_names_from_wrangler_json() {
        let stdout = "\n ⛅️ wrangler 4.72.0\n[\n  {\n    \"name\": \"IMPRESSPRESS_PASSWORD_PEPPER_KEY\",\n    \"type\": \"secret_text\"\n  }\n]\n";
        assert_eq!(parse_secret_names(stdout).unwrap(), vec![KEY.to_string()]);
        assert!(parse_secret_names("[]").unwrap().is_empty());
        assert!(parse_secret_names("error").is_err());
    }

    fn package(source: Option<&str>) -> MetadataPackage {
        MetadataPackage {
            name: PASSWORD_CRATE.into(),
            version: "0.1.0".into(),
            source: source.map(str::to_string),
            manifest_path: "/src/impresspress/crates/impresspress-password/Cargo.toml".into(),
        }
    }

    #[test]
    fn reads_every_kind_of_source() {
        assert_eq!(
            PasswordCrateSource::from_metadata(&package(None)).unwrap(),
            PasswordCrateSource::Path("/src/impresspress/crates/impresspress-password".into())
        );
        assert_eq!(
            PasswordCrateSource::from_metadata(&package(Some(
                "git+https://github.com/Jsuppers/impresspress?rev=abc123#abc1234567890"
            )))
            .unwrap(),
            PasswordCrateSource::Git {
                url: "https://github.com/Jsuppers/impresspress".into(),
                rev: "abc1234567890".into(),
            }
        );
        assert_eq!(
            PasswordCrateSource::from_metadata(&package(Some(
                "registry+https://github.com/rust-lang/crates.io-index"
            )))
            .unwrap(),
            PasswordCrateSource::Registry {
                version: "0.1.0".into()
            }
        );
        assert!(PasswordCrateSource::from_metadata(&package(Some("local+x"))).is_err());
    }

    #[test]
    fn refuses_a_graph_without_or_with_two_password_crates() {
        let none = Metadata {
            packages: vec![],
            workspace_root: "/src".into(),
        };
        assert!(none.password_crate_source().is_err());
        let two = Metadata {
            packages: vec![package(None), package(Some("registry+x"))],
            workspace_root: "/src".into(),
        };
        assert!(two.password_crate_source().is_err());
    }

    /// The staged manifest, pinned: its own workspace, the DO feature on, a
    /// speed-first release profile.
    #[test]
    fn the_staged_manifest_is_pinned() {
        let manifest = manifest(&PasswordCrateSource::Git {
            url: "https://github.com/Jsuppers/impresspress".into(),
            rev: "abc".into(),
        })
        .unwrap();
        let parsed: toml::Value = toml::from_str(&manifest).unwrap();
        assert_eq!(
            parsed["dependencies"][PASSWORD_CRATE],
            toml::from_str::<toml::Value>(
                r#"git = "https://github.com/Jsuppers/impresspress"
rev = "abc"
features = ["durable-object"]"#
            )
            .unwrap()
        );
        assert_eq!(parsed["lib"]["crate-type"][0].as_str(), Some("cdylib"));
        assert!(parsed["workspace"].as_table().unwrap().is_empty());
        assert_eq!(
            parsed["profile"]["release"]["opt-level"].as_integer(),
            Some(3)
        );
        assert!(parsed["features"].get("target-cloudflare").is_some());
        assert!(LIB_RS.contains("durable_object::ImpresspressPasswordHasher"));
    }

    #[test]
    fn stage_writes_the_crate_and_copies_the_lockfile() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::write(repo.join("Cargo.lock"), "# lock\n").unwrap();
        let metadata = Metadata {
            packages: vec![package(None)],
            workspace_root: repo.to_path_buf(),
        };
        let dir = stage(repo, &metadata).unwrap();
        assert_eq!(dir, repo.join("target/impresspress-password-hasher"));
        assert_eq!(
            std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
            LIB_RS
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("Cargo.lock")).unwrap(),
            "# lock\n"
        );
        assert!(std::fs::read_to_string(dir.join("Cargo.toml"))
            .unwrap()
            .contains("/src/impresspress/crates/impresspress-password"));
    }

    /// `wrangler deployments status --json` for a Worker deployed with plain
    /// `wrangler deploy`: Cloudflare's deployment object, which wrangler
    /// 4.72 prints unchanged (`JSON.stringify(latestDeployment, null, 2)`).
    const DEPLOYMENT_STATUS: &str = include_str!("fixtures/wrangler-deployments-status.json");

    #[test]
    fn reads_the_kept_version_from_wrangler_deployments_status() {
        let status = parse_deployment_status(DEPLOYMENT_STATUS).unwrap();
        assert_eq!(status.id, "6f4c3b1e-8a2d-4d0f-9b7e-2c5a1d9e0f34");
        assert_eq!(
            status.sole_version().unwrap(),
            "20577eb6-6e78-4b5e-9dc4-403951f2ed5e"
        );
    }

    /// A gradual deployment serves two versions; which one a main Worker
    /// request reaches is not known, so keep refuses it. So does a deployment
    /// that lists no version.
    #[test]
    fn a_split_or_empty_deployment_cannot_be_kept() {
        let mut split: serde_json::Value = serde_json::from_str(DEPLOYMENT_STATUS).unwrap();
        split["versions"] = serde_json::json!([
            { "percentage": 90, "version_id": "a" },
            { "percentage": 10, "version_id": "b" },
        ]);
        let err = parse_deployment_status(&split.to_string())
            .unwrap()
            .sole_version()
            .unwrap_err()
            .to_string();
        assert!(err.contains("a at 90%, b at 10%"), "{err}");

        let mut empty = split;
        empty["versions"] = serde_json::json!([]);
        assert!(parse_deployment_status(&empty.to_string())
            .unwrap()
            .sole_version()
            .is_err());
        assert!(parse_deployment_status("The Worker has no deployments.").is_err());
    }

    /// `wrangler versions view <id> --json` for the kept hasher version: its
    /// pepper key secret (name only) and `pepper_required = false`.
    const VERSION_VIEW: &str = include_str!("fixtures/wrangler-versions-view.json");

    /// The vars a release with `pepper_required = required` deploys.
    fn vars(required: bool) -> BTreeMap<String, String> {
        BTreeMap::from([(
            "IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED".to_string(),
            required.to_string(),
        )])
    }

    #[test]
    fn reads_the_kept_versions_vars_from_wrangler_versions_view() {
        let view = parse_version_view(VERSION_VIEW).unwrap();
        assert_eq!(view.vars(), vars(false), "the secret is not a var");
        view.check_vars("v", &vars(false)).unwrap();
    }

    /// A var this release would deploy that the kept version does not hold —
    /// a different value, a missing var, or one the release no longer
    /// writes — refuses, naming it.
    #[test]
    fn a_kept_version_with_other_vars_is_refused() {
        let view = parse_version_view(VERSION_VIEW).unwrap();
        let err = view.check_vars("v", &vars(true)).unwrap_err().to_string();
        assert!(
            err.contains(
                "IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED: kept version has \"false\", this release \
                 deploys \"true\""
            ),
            "{err}"
        );
        assert!(err.contains("--password-hasher deploy"), "{err}");

        let mut extra = vars(false);
        extra.insert("NEW_HASHER_VAR".into(), "1".into());
        let err = view.check_vars("v", &extra).unwrap_err().to_string();
        assert!(
            err.contains("NEW_HASHER_VAR: kept version has unset"),
            "{err}"
        );

        let err = view
            .check_vars("v", &BTreeMap::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(
                "IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED: kept version has \"false\", this \
                          release deploys unset"
            ),
            "{err}"
        );
    }

    /// A [`WorkerCommands`] that records every call, answers
    /// `deployment_status` with `status` and `version_view` with
    /// [`VERSION_VIEW`].
    struct Recorder {
        status: Result<String, &'static str>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl WorkerCommands for Recorder {
        fn worker_exists(&self, config: &Path) -> Result<bool> {
            self.calls
                .borrow_mut()
                .push(format!("exists {}", config.display()));
            Ok(true)
        }

        fn deploy(&self, config: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("deploy {}", config.display()));
            Ok(())
        }

        fn deployment_status(&self, config: &Path) -> Result<String> {
            self.calls
                .borrow_mut()
                .push(format!("status {}", config.display()));
            self.status.clone().map_err(|e| anyhow!(e))
        }

        fn version_view(&self, config: &Path, version_id: &str) -> Result<String> {
            self.calls
                .borrow_mut()
                .push(format!("view {version_id} {}", config.display()));
            Ok(VERSION_VIEW.to_string())
        }

        fn secret_put(&self, config: &Path, name: &str, _value: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("secret {name} {}", config.display()));
            Ok(())
        }

        fn secret_names(&self, config: &Path) -> Result<Vec<String>> {
            self.calls
                .borrow_mut()
                .push(format!("list {}", config.display()));
            Ok(Vec::new())
        }
    }

    /// `keep` deploys nothing: its wrangler calls against the hasher are the
    /// read-only status and version view, and the version it reports is the
    /// deployed one.
    #[test]
    fn keep_reads_the_deployed_version_and_deploys_nothing() {
        let commands = Recorder {
            status: Ok(DEPLOYMENT_STATUS.to_string()),
            calls: Default::default(),
        };
        let release = release(
            &commands,
            Path::new("hasher.toml"),
            PasswordHasherMode::Keep,
            &vars(false),
        )
        .unwrap();
        assert_eq!(
            release,
            HasherRelease::Kept {
                version_id: "20577eb6-6e78-4b5e-9dc4-403951f2ed5e".into()
            }
        );
        assert_eq!(
            *commands.calls.borrow(),
            [
                "status hasher.toml",
                "view 20577eb6-6e78-4b5e-9dc4-403951f2ed5e hasher.toml"
            ]
        );
    }

    /// `keep` with a `[cloudflare.password_hasher]` change the kept version
    /// does not hold — here `pepper_required = true` — refuses before
    /// anything is uploaded, and still deploys nothing.
    #[test]
    fn keep_refuses_a_kept_version_without_this_releases_config() {
        let commands = Recorder {
            status: Ok(DEPLOYMENT_STATUS.to_string()),
            calls: Default::default(),
        };
        let err = release(
            &commands,
            Path::new("hasher.toml"),
            PasswordHasherMode::Keep,
            &vars(true),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED"),
            "{err}"
        );
        assert!(
            !commands
                .calls
                .borrow()
                .iter()
                .any(|c| c.starts_with("deploy")),
            "nothing is deployed"
        );
    }

    #[test]
    fn deploy_deploys_the_hasher_config() {
        let commands = Recorder {
            status: Err("never asked"),
            calls: Default::default(),
        };
        let release = release(
            &commands,
            Path::new("hasher.toml"),
            PasswordHasherMode::Deploy,
            &vars(true),
        )
        .unwrap();
        assert_eq!(release, HasherRelease::Deployed);
        assert_eq!(*commands.calls.borrow(), ["deploy hasher.toml"]);
    }

    /// No hasher to keep (a first deploy, or a renamed one): refused, and
    /// nothing is deployed in its place.
    #[test]
    fn keep_without_a_deployed_hasher_refuses() {
        let commands = Recorder {
            status: Err("[code: 10007] This Worker does not exist on your account."),
            calls: Default::default(),
        };
        let err = release(
            &commands,
            Path::new("hasher.toml"),
            PasswordHasherMode::Keep,
            &vars(false),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("needs the password-hasher Worker to be deployed"),
            "{err:#}"
        );
        assert_eq!(*commands.calls.borrow(), ["status hasher.toml"]);
    }
}
