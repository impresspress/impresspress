//! Build the password-hasher Worker.
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

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

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
}
