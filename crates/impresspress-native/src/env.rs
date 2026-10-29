//! Environment-variable bootstrap helpers for native WAFER apps.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};

/// Names an env file to load in place of `<dir>/.env`.
const ENV_FILE_VAR: &str = "IMPRESSPRESS_ENV_FILE";

/// Load the env file into the process environment. Honors
/// `IMPRESSPRESS_ENV_FILE` for an explicit path (absolute or relative to the
/// current process cwd; set but empty counts as unset); otherwise loads
/// `<dir>/.env`. A variable already
/// set in the process environment keeps its value.
///
/// `<dir>/.env` is optional: when there is none, nothing is loaded and this
/// returns `Ok`. Every other failure is an error — a file that cannot be
/// read, a line that cannot be parsed, or an `IMPRESSPRESS_ENV_FILE` that
/// names no file — because each one leaves the process running without
/// settings the operator wrote down: a parse error stops the load at that
/// line, so every variable on or after it (a bootstrap admin, a database
/// path) would silently be missing. The caller must not boot on an error.
/// The message names the file and, for a parse error, the line number; it
/// never quotes the line, which can hold a secret.
///
/// Taking the dir explicitly (instead of relying on `dotenvy::dotenv`'s
/// implicit cwd walk) lets the CLI pin the env-file lookup to the
/// detected repo root without mutating global process state via
/// `std::env::set_current_dir`.
pub fn load_dotenv(dir: &Path) -> Result<()> {
    // Empty is unset, as for every other variable here (`FOO=` in a shell or
    // `.env` is "unset", not "set to blank").
    match std::env::var_os(ENV_FILE_VAR).filter(|path| !path.is_empty()) {
        Some(path) => load_env_file(&PathBuf::from(path), Presence::Required)
            .with_context(|| format!("{ENV_FILE_VAR} names an env file that cannot be loaded")),
        None => load_env_file(&dir.join(".env"), Presence::Optional),
    }
}

/// Whether a missing env file is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    /// The default `<dir>/.env`: absent means "no env file".
    Optional,
    /// A file the operator named: absent is an error.
    Required,
}

/// Read `path` and load its variables; see [`load_dotenv`].
fn load_env_file(path: &Path, presence: Presence) -> Result<()> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound && presence == Presence::Optional => {
            return Ok(())
        }
        Err(e) => return Err(anyhow!(e).context(format!("read env file {}", path.display()))),
    };
    match dotenvy::from_read(content.as_bytes()) {
        Ok(()) => Ok(()),
        Err(dotenvy::Error::LineParse(text, index)) => {
            let at = match failing_line(&content, &text, index) {
                Some(n) => format!("{}:{n}", path.display()),
                None => path.display().to_string(),
            };
            Err(anyhow!(
                "{at}: env file line cannot be parsed, so it and every line after it \
                 were not loaded (each line is KEY=value; quote a value that contains \
                 spaces, e.g. KEY=\"two words\", and close every quote; a last line with no \
                 final newline must not end in a space or a backslash)"
            ))
        }
        Err(e) => Err(anyhow!(e).context(format!("load env file {}", path.display()))),
    }
}

/// The first error dotenvy reports for `content`, parsed the way
/// `dotenvy::from_read` parses it (a leading byte-order mark is skipped)
/// but without touching the environment.
fn first_parse_error(content: &str) -> Option<dotenvy::Error> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    dotenvy::from_read_iter(content.as_bytes()).find_map(Result::err)
}

/// The 1-based physical line of `content` that dotenvy failed on with
/// `LineParse(text, index)`.
///
/// dotenvy reports no position, and its `text` is not reliably the line: a
/// bad value reports only the value, and a trailing comment is cut off. So
/// the line is found by parsing again (without touching the environment):
/// it is the first physical line `L` such that lines `1..=L` alone fail with
/// the same error the whole file did. Every logical line before the failing
/// one parsed, and a prefix cut inside a multi-line value fails differently
/// (with that value's quote open).
///
/// That search alone would name the LAST line for a value still open when
/// the file ends — an unclosed quote, or a last line without a final newline
/// that ends in a backslash or in whitespace — because dotenvy then hands
/// back the rest of the file from the open logical line's start, with the
/// index at its end, and only the whole file reproduces that. Its start is
/// taken directly instead, but only when the text starts a line and every
/// line before it parses: a line with no `=` (`FOO`, `export FOO`) reports
/// the same `(line, line.len())` shape, and the same text can also end the
/// file elsewhere (`FOO` then `X=FOO`), so a match that fails either check is
/// left to the search.
fn failing_line(content: &str, text: &str, index: usize) -> Option<usize> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let line_of = |offset: usize| content[..offset].matches('\n').count() + 1;
    if index == text.len() && !text.is_empty() && content.ends_with(text) {
        let start = content.len() - text.len();
        let starts_a_line = start == 0 || content[..start].ends_with('\n');
        if starts_a_line && first_parse_error(&content[..start]).is_none() {
            return Some(line_of(start));
        }
    }
    let mut end = 0;
    while end < content.len() {
        end = content[end..]
            .find('\n')
            .map_or(content.len(), |i| end + i + 1);
        if let Some(dotenvy::Error::LineParse(t, i)) = first_parse_error(&content[..end]) {
            if t == text && i == index {
                return Some(line_of(end - 1));
            }
        }
    }
    None
}

/// Collect env vars that look like app config — i.e. any key containing
/// `__`. The workspace convention (per CLAUDE.md) is:
///
/// - `WAFER_RUN_SHARED__*` — shared app config (any block reads it)
/// - `{ORG}__{BLOCK}__*` — block-scoped (only the owner block + admin)
/// - `IMPRESSPRESS_*` (no `__`) — infrastructure, never seeded into the DB
///
/// The presence of `__` is the discriminator: every app/block config key
/// contains it, infra keys never do.
///
/// Consumers who want additional filtering (e.g., only env vars that
/// match declared config var keys) should apply their own filter on top
/// of this result.
pub fn collect_app_env_vars() -> HashMap<String, String> {
    filter_app_env_vars(std::env::vars())
}

/// Pure filter: keeps any pair whose key contains `__`.
///
/// Split out so tests can exercise the filter without mutating the
/// process environment (which is `unsafe` in Rust 2024 and races with
/// parallel test runs).
pub(crate) fn filter_app_env_vars<I>(iter: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    iter.into_iter().filter(|(k, _)| k.contains("__")).collect()
}

#[cfg(test)]
mod tests {
    use impresspress_core::blocks::auth::{config::BOOTSTRAP_ADMIN_EMAIL_KEY, JWT_SECRET_KEY};

    use super::*;

    #[test]
    fn filter_keeps_shared_and_block_scoped_drops_infra_and_plain() {
        let input = vec![
            // Shared app config — keep.
            (
                BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
                "admin@example.com".to_string(),
            ),
            // Block-scoped — keep.
            (JWT_SECRET_KEY.to_string(), "abc".to_string()),
            // Infra — drop.
            (LISTEN_VAR.to_string(), "0.0.0.0:8090".to_string()),
            (DB_PATH_VAR.to_string(), "data/impresspress.db".to_string()),
            // Plain env vars without `__` — drop.
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/joris".to_string()),
        ];
        let out = filter_app_env_vars(input);
        assert!(out.contains_key(BOOTSTRAP_ADMIN_EMAIL_KEY));
        assert!(out.contains_key(JWT_SECRET_KEY));
        assert!(!out.contains_key(LISTEN_VAR));
        assert!(!out.contains_key(DB_PATH_VAR));
        assert!(!out.contains_key("PATH"));
        assert!(!out.contains_key("HOME"));
    }

    #[test]
    fn filter_app_env_vars_empty_iterator_returns_empty_map() {
        let out = filter_app_env_vars(std::iter::empty());
        assert!(out.is_empty());
    }
}

#[cfg(test)]
mod env_file_tests {
    //! Each test loads keys no other test uses: loading writes the process
    //! environment, which every test in this binary shares.

    use super::*;

    fn write(dir: &tempfile::TempDir, content: &str) -> PathBuf {
        let path = dir.path().join(".env");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn a_missing_default_env_file_loads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        load_env_file(&dir.path().join(".env"), Presence::Optional).unwrap();
    }

    #[test]
    fn a_missing_named_env_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prod.env");
        let err = load_env_file(&path, Presence::Required).unwrap_err();
        let shown = format!("{err:#}");
        assert!(shown.contains(&path.display().to_string()), "{shown}");
    }

    #[test]
    fn a_well_formed_env_file_loads_without_overriding_the_process_env() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("ENV_FILE_TEST_WELL_FORMED_KEPT", "from-process");
        let path = write(
            &dir,
            "# comment\n\nENV_FILE_TEST_WELL_FORMED=\"two words\"\n\
             ENV_FILE_TEST_WELL_FORMED_KEPT=from-file\n",
        );
        load_env_file(&path, Presence::Optional).unwrap();
        assert_eq!(
            std::env::var("ENV_FILE_TEST_WELL_FORMED").unwrap(),
            "two words"
        );
        assert_eq!(
            std::env::var("ENV_FILE_TEST_WELL_FORMED_KEPT").unwrap(),
            "from-process"
        );
    }

    /// The reported bug: an unquoted value with a space stopped the load at
    /// that line, so the bootstrap admin on a later line was never created,
    /// and nothing said so.
    #[test]
    fn an_unparseable_line_is_an_error_naming_file_and_line_but_not_its_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "ENV_FILE_TEST_BAD_BEFORE=ok\n\
             # a comment counts as a line\n\
             ENV_FILE_TEST_BAD_SECRET=hunter2 hunter3\n\
             ENV_FILE_TEST_BAD_AFTER=never-loaded\n",
        );
        let err = load_env_file(&path, Presence::Optional).unwrap_err();
        let shown = format!("{err:#}");
        assert!(shown.contains(&format!("{}:3:", path.display())), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(std::env::var("ENV_FILE_TEST_BAD_AFTER").is_err());
    }

    #[test]
    fn an_unreadable_env_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: present, but not readable as
        // one.
        let path = dir.path().join(".env");
        std::fs::create_dir(&path).unwrap();
        let err = load_env_file(&path, Presence::Optional).unwrap_err();
        let shown = format!("{err:#}");
        assert!(shown.contains(&path.display().to_string()), "{shown}");
    }

    /// Removes a file when dropped, so a failed assertion still cleans up.
    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Every `IMPRESSPRESS_ENV_FILE` case through `load_dotenv` itself, in one
    /// test: the variable is process-wide, so parallel tests would race on it.
    #[test]
    fn load_dotenv_honors_the_env_file_variable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "ENV_FILE_TEST_VAR_DEFAULT=from-default\n",
        )
        .unwrap();

        // A named file that exists is loaded instead of `<dir>/.env`.
        let named = dir.path().join("named.env");
        std::fs::write(&named, "ENV_FILE_TEST_VAR_NAMED=from-named\n").unwrap();
        std::env::set_var(ENV_FILE_VAR, &named);
        load_dotenv(dir.path()).unwrap();
        assert_eq!(
            std::env::var("ENV_FILE_TEST_VAR_NAMED").unwrap(),
            "from-named"
        );
        assert!(std::env::var("ENV_FILE_TEST_VAR_DEFAULT").is_err());

        // A named file that does not exist stops the load, naming the
        // variable and the path.
        let missing = dir.path().join("missing.env");
        std::env::set_var(ENV_FILE_VAR, &missing);
        let shown = format!("{:#}", load_dotenv(dir.path()).unwrap_err());
        assert!(shown.contains(ENV_FILE_VAR), "{shown}");
        assert!(shown.contains(&missing.display().to_string()), "{shown}");

        // A relative name is read against the cwd only (cargo runs tests
        // from the package root), not searched for in its parents.
        let name = format!("env-file-test-parent-{}.env", std::process::id());
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(&name);
        std::fs::write(&parent, "ENV_FILE_TEST_VAR_PARENT=from-parent\n").unwrap();
        let _cleanup = RemoveOnDrop(parent);
        std::env::set_var(ENV_FILE_VAR, &name);
        let shown = format!("{:#}", load_dotenv(dir.path()).unwrap_err());
        assert!(shown.contains(ENV_FILE_VAR), "{shown}");
        assert!(std::env::var("ENV_FILE_TEST_VAR_PARENT").is_err());

        // Set but empty is unset: `<dir>/.env` loads.
        std::env::set_var(ENV_FILE_VAR, "");
        let loaded = load_dotenv(dir.path());
        std::env::remove_var(ENV_FILE_VAR);
        loaded.unwrap();
        assert_eq!(
            std::env::var("ENV_FILE_TEST_VAR_DEFAULT").unwrap(),
            "from-default"
        );
    }

    /// The line dotenvy failed on, for `content`.
    fn line_of_error(content: &str) -> Option<usize> {
        match first_parse_error(content) {
            Some(dotenvy::Error::LineParse(text, index)) => failing_line(content, &text, index),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn failing_line_skips_a_byte_order_mark() {
        assert_eq!(line_of_error("\u{feff}A=1\nB=x y\n"), Some(2));
    }

    /// A line with no `=` reports `(line, line.len())`, the shape of a value
    /// left open at the end of the file; text that also ends the file must
    /// not be mistaken for one.
    #[test]
    fn failing_line_of_a_line_without_equals_is_that_line() {
        assert_eq!(line_of_error("FOO\nX=FOO"), Some(1));
        assert_eq!(line_of_error("FOO\nY=1\nFOO"), Some(1));
        assert_eq!(line_of_error("export FOO\nA=export FOO"), Some(1));
        assert_eq!(line_of_error("A=1\nFOO"), Some(2));
    }

    #[test]
    fn failing_line_of_trailing_whitespace_or_backslash_at_the_end_is_that_line() {
        assert_eq!(line_of_error("A=1\nB=2 "), Some(2));
        assert_eq!(line_of_error("A=1\nB=2\\"), Some(2));
    }

    #[test]
    fn failing_line_counts_physical_lines_past_crlf_and_blank_lines() {
        assert_eq!(line_of_error("A=1\r\n\r\nB=x y\r\nC=2\r\n"), Some(3));
    }

    #[test]
    fn failing_line_is_found_when_dotenvy_reports_only_the_value() {
        // dotenvy's error text is `x y`, which also appears on line 1.
        assert_eq!(line_of_error("A=\"x y\"\nB=x y\n"), Some(2));
    }

    #[test]
    fn failing_line_is_found_past_a_trailing_comment_and_a_multi_line_value() {
        assert_eq!(
            line_of_error("A=\"one\ntwo\"\nB=x y # note\nC=1\n"),
            Some(3)
        );
    }

    #[test]
    fn failing_line_is_found_without_a_final_newline() {
        assert_eq!(line_of_error("A=1\nB=x y"), Some(2));
    }

    #[test]
    fn failing_line_of_an_unclosed_quote_is_where_it_opens() {
        assert_eq!(line_of_error("A=1\nB=\"open\nstill open\n"), Some(2));
    }
}

/// The `IMPRESSPRESS_*` process environment variables [`InfraConfig`] reads.
const LISTEN_VAR: &str = "IMPRESSPRESS_LISTEN";
const DB_TYPE_VAR: &str = "IMPRESSPRESS_DB_TYPE";
const DB_PATH_VAR: &str = "IMPRESSPRESS_DB_PATH";
const DB_URL_VAR: &str = "IMPRESSPRESS_DB_URL";
const STORAGE_TYPE_VAR: &str = "IMPRESSPRESS_STORAGE_TYPE";
const STORAGE_ROOT_VAR: &str = "IMPRESSPRESS_STORAGE_ROOT";
const MODEL_CACHE_DIR_VAR: &str = "IMPRESSPRESS_MODEL_CACHE_DIR";
const TRUSTED_PROXIES_VAR: &str = "IMPRESSPRESS_TRUSTED_PROXIES";
const HEADER_READ_TIMEOUT_VAR: &str = "IMPRESSPRESS_HEADER_READ_TIMEOUT_SECS";
const BODY_READ_TIMEOUT_VAR: &str = "IMPRESSPRESS_BODY_READ_TIMEOUT_SECS";
const MAX_CONNECTIONS_VAR: &str = "IMPRESSPRESS_MAX_CONNECTIONS";
const WRITE_TIMEOUT_VAR: &str = "IMPRESSPRESS_WRITE_TIMEOUT_SECS";
const SHUTDOWN_GRACE_VAR: &str = "IMPRESSPRESS_SHUTDOWN_GRACE_SECS";

/// The `wafer-run/http-listener` settings an operator sets through
/// `IMPRESSPRESS_*` variables, one field per listener config key.
///
/// A field is `None` when its variable is unset, and the listener's own
/// default applies: `register_http_listener` passes only the set ones, so
/// the listener stays the single source of its defaults and of their
/// validation. A set value is passed as written; the listener checks it at
/// `Init`, and a bad value fails boot naming the key.
///
/// - `IMPRESSPRESS_TRUSTED_PROXIES` → `trusted_proxies`: comma-separated IPs
///   or CIDR ranges whose `X-Forwarded-For` is honored. Unset trusts none,
///   so behind a reverse proxy every client shares the proxy's address.
/// - `IMPRESSPRESS_HEADER_READ_TIMEOUT_SECS` → `header_read_timeout_secs`
/// - `IMPRESSPRESS_BODY_READ_TIMEOUT_SECS` → `body_read_timeout_secs`
/// - `IMPRESSPRESS_MAX_CONNECTIONS` → `max_connections`
/// - `IMPRESSPRESS_WRITE_TIMEOUT_SECS` → `write_timeout_secs`
/// - `IMPRESSPRESS_SHUTDOWN_GRACE_SECS` → `shutdown_grace_secs`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListenerEnv {
    pub trusted_proxies: Option<String>,
    pub header_read_timeout_secs: Option<String>,
    pub body_read_timeout_secs: Option<String>,
    pub max_connections: Option<String>,
    pub write_timeout_secs: Option<String>,
    pub shutdown_grace_secs: Option<String>,
}

impl ListenerEnv {
    /// Read the variables through `var`, which answers `None` for an unset
    /// one. [`InfraConfig::from_env`] passes the process environment.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            trusted_proxies: var(TRUSTED_PROXIES_VAR),
            header_read_timeout_secs: var(HEADER_READ_TIMEOUT_VAR),
            body_read_timeout_secs: var(BODY_READ_TIMEOUT_VAR),
            max_connections: var(MAX_CONNECTIONS_VAR),
            write_timeout_secs: var(WRITE_TIMEOUT_VAR),
            shutdown_grace_secs: var(SHUTDOWN_GRACE_VAR),
        }
    }

    /// The set settings as `(listener config key, value)` pairs.
    pub fn set(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("trusted_proxies", &self.trusted_proxies),
            ("header_read_timeout_secs", &self.header_read_timeout_secs),
            ("body_read_timeout_secs", &self.body_read_timeout_secs),
            ("max_connections", &self.max_connections),
            ("write_timeout_secs", &self.write_timeout_secs),
            ("shutdown_grace_secs", &self.shutdown_grace_secs),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_deref().map(|value| (key, value)))
    }
}

/// Infrastructure config read from `IMPRESSPRESS_*` env vars.
///
/// `Debug` is hand-written: `db_url` carries the database password, so it is
/// shown only as [`crate::database::postgres_target`] describes it.
pub struct InfraConfig {
    pub listen: String,
    pub db_type: String,
    pub db_path: String,
    pub db_url: Option<String>,
    pub storage_type: String,
    pub storage_root: String,
    /// Where the native ONNX embedding model's weights are cached
    /// (`IMPRESSPRESS_MODEL_CACHE_DIR`), handed to
    /// `ImpresspressBuilder::model_cache_dir`. Read only by a build with
    /// the `block-fastembed` feature (which `native-embedding` implies).
    pub model_cache_dir: String,
    pub listener: ListenerEnv,
}

impl InfraConfig {
    pub fn from_env() -> Self {
        Self {
            listen: env_or(LISTEN_VAR, "0.0.0.0:8090"),
            db_type: env_or(DB_TYPE_VAR, "sqlite"),
            db_path: env_or(DB_PATH_VAR, "data/impresspress.db"),
            db_url: std::env::var(DB_URL_VAR).ok(),
            storage_type: env_or(STORAGE_TYPE_VAR, "local"),
            storage_root: env_or(STORAGE_ROOT_VAR, "data/storage"),
            model_cache_dir: env_or(MODEL_CACHE_DIR_VAR, "data/models"),
            listener: ListenerEnv::from_vars(|key| std::env::var(key).ok()),
        }
    }

    /// Serve on `port` instead of the port `IMPRESSPRESS_LISTEN` (or its
    /// default) gives, keeping that address's host. Fails when `listen` has
    /// no `host:port` shape to replace the port of.
    pub fn override_port(&mut self, port: u16) -> Result<()> {
        self.listen = listen_with_port(&self.listen, port)?;
        Ok(())
    }
}

/// `listen` with its port replaced by `port`: the text after the last `:` is
/// the port, so a bracketed IPv6 host (`[::1]:8090`) keeps its brackets.
fn listen_with_port(listen: &str, port: u16) -> Result<String> {
    match listen.rsplit_once(':') {
        Some((host, _)) if !host.is_empty() => Ok(format!("{host}:{port}")),
        _ => Err(anyhow!(
            "{LISTEN_VAR}={listen} is not a host:port address, so its port cannot be replaced"
        )),
    }
}

impl std::fmt::Debug for InfraConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InfraConfig")
            .field("listen", &self.listen)
            .field("db_type", &self.db_type)
            .field("db_path", &self.db_path)
            .field(
                "db_url",
                &self.db_url.as_deref().map(crate::database::postgres_target),
            )
            .field("storage_type", &self.storage_type)
            .field("storage_root", &self.storage_root)
            .field("model_cache_dir", &self.model_cache_dir)
            .field("listener", &self.listener)
            .finish()
    }
}

#[cfg(test)]
mod infra_config_tests {
    use super::InfraConfig;

    #[test]
    fn debug_never_shows_the_db_password() {
        let infra = InfraConfig {
            listen: "0.0.0.0:8090".into(),
            db_type: "postgres".into(),
            db_path: "data/impresspress.db".into(),
            db_url: Some("postgres://app:hunter2@db.internal:5432/prod".into()),
            storage_type: "local".into(),
            storage_root: "data/storage".into(),
            model_cache_dir: "data/models".into(),
            listener: super::ListenerEnv::default(),
        };
        let shown = format!("{infra:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("db.internal:5432/prod"), "{shown}");
    }

    #[test]
    fn override_port_keeps_the_host() {
        for (listen, expected) in [
            ("127.0.0.1:8090", "127.0.0.1:9123"),
            ("0.0.0.0:8090", "0.0.0.0:9123"),
            ("localhost:8090", "localhost:9123"),
            ("[::1]:8090", "[::1]:9123"),
        ] {
            assert_eq!(super::listen_with_port(listen, 9123).unwrap(), expected);
        }
    }

    #[test]
    fn override_port_refuses_an_address_without_a_port() {
        for listen in ["8090", ":8090", ""] {
            let err = super::listen_with_port(listen, 9123).unwrap_err();
            assert!(err.to_string().contains("IMPRESSPRESS_LISTEN"), "{err}");
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
