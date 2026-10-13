//! The boot shell's own URL path: `[app] shell_path` in `impresspress.toml`,
//! [`super::AppConfig::shell_path`] here.
//!
//! The bundler renders it into `sw.js.tmpl` and `loader.js.tmpl` as
//! `SHELL_URL`, from this one value, so the two scripts cannot disagree about
//! where the shell is:
//!
//! * the worker fetches the shell document from it to answer a navigation
//!   once its runtime is dead (`bootShell` in `sw.js.tmpl`);
//! * the loader tells a shell standing there (the app being opened, which
//!   goes on to the boot URL) from one standing at a page of the app, which
//!   goes on to that page (`destination` in `loader.js.tmpl`).
//!
//! # The static host's contract
//!
//! The static host must answer a request for this path with the boot shell
//! document — the bundle's `index.html`, or an overlay of it that loads
//! `/loader.js` — with a `200` and without a redirect to another path. A
//! visit that boots the app with no worker yet starts there (an app with a
//! landing page at `/` may send its first visitors through that page
//! first), and a dead worker fetches the shell from there. The default, `/`, is what every static host does with
//! `index.html`; an app that keeps another document at `/` (a landing page
//! the worker never intercepts, say) names the path its host serves the
//! shell at instead, e.g. `shell_path = "/chat"` beside a `chat.html` that a
//! host such as Cloudflare Pages serves at `/chat`.
//!
//! # What a valid path is
//!
//! An absolute URL path and nothing else, spelled exactly as a browser's
//! `URL.pathname` spells it, because the loader compares the two as strings:
//!
//! * it starts with `/`, and has no scheme, host, query (`?`) or fragment
//!   (`#`);
//! * no segment is empty (`//`), `.` or `..` — a browser resolves those
//!   away, so the path the page stands at would never be the one written;
//! * every character is an ASCII letter or digit, or one of
//!   `-._~!$&'()*+,;=:@` — the characters a URL path carries as they are.
//!   Percent-encoding is refused rather than normalized: a browser leaves
//!   some escapes as written and decodes none, so an escaped spelling would
//!   be a second way to write the same path that compares unequal.
//!
//! A trailing slash is significant and kept: `/chat` and `/chat/` are two
//! different paths, as they are to the host. Write the one the host answers
//! with the shell — `/chat` for a `chat.html` served at `/chat`, `/chat/`
//! for a `chat/index.html` served at `/chat/`. A host that answers the path
//! written here only by redirecting to the other spelling breaks the
//! contract above: the shell would stand at a path the loader does not know
//! as its own.

use serde::{Deserialize, Serialize};

/// A valid boot shell path — see the module docs. Constructed only by
/// [`ShellPath::parse`] (and [`Default`], which is `/`), so a value of this
/// type is always one the bundler can render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ShellPath(String);

/// The characters a shell path may carry besides ASCII letters, digits and
/// the `/` that separates its segments: RFC 3986's `unreserved` and
/// `sub-delims`, `:` and `@` — the `pchar` set, without the `%` of a
/// percent-encoding.
const ALLOWED_PUNCTUATION: &str = "-._~!$&'()*+,;=:@";

impl ShellPath {
    /// The default: the root, where every static host serves `index.html`.
    pub const ROOT: &'static str = "/";

    /// `value` as a shell path, or why it is not one.
    pub fn parse(value: &str) -> Result<Self, InvalidShellPath> {
        let refuse = |reason: String| {
            Err(InvalidShellPath {
                value: value.to_string(),
                reason,
            })
        };
        if !value.starts_with('/') {
            return refuse(
                "is not an absolute path: it must start with `/` and name no scheme or host"
                    .to_string(),
            );
        }
        if value.contains('?') {
            return refuse("has a query (`?`); give the path alone".to_string());
        }
        if value.contains('#') {
            return refuse("has a fragment (`#`); give the path alone".to_string());
        }
        if value.contains('%') {
            return refuse(
                "is percent-encoded (`%`); write the path with the characters a URL path \
                 carries as they are"
                    .to_string(),
            );
        }
        if let Some(c) = value
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '/' || ALLOWED_PUNCTUATION.contains(*c)))
        {
            return refuse(format!(
                "contains {c:?}, which a URL path does not carry as it is; use ASCII letters, \
                 digits, `/` and `{ALLOWED_PUNCTUATION}`"
            ));
        }
        // The segments after the leading `/`. The last is empty for a path
        // that ends in `/` (and for `/` itself), which is the one empty
        // segment allowed.
        let segments: Vec<&str> = value[1..].split('/').collect();
        let last = segments.len() - 1;
        for (index, segment) in segments.iter().enumerate() {
            match *segment {
                "" if index != last => {
                    return refuse(
                        "has an empty segment (`//`), which a browser would read as a host"
                            .to_string(),
                    );
                }
                "." | ".." => {
                    return refuse(format!(
                        "has a `{segment}` segment, which a browser resolves away, so no page \
                         would ever stand at the path as written"
                    ));
                }
                _ => {}
            }
        }
        Ok(Self(value.to_string()))
    }

    /// The path, as `sw.js` and `loader.js` are rendered with it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ShellPath {
    fn default() -> Self {
        Self(Self::ROOT.to_string())
    }
}

impl std::fmt::Display for ShellPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ShellPath {
    type Error = InvalidShellPath;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ShellPath> for String {
    fn from(path: ShellPath) -> Self {
        path.0
    }
}

/// A value refused by [`ShellPath::parse`]: the value, and why. Displays as
/// `shell_path "<value>" <reason>`, naming the field, so a reader of a
/// configuration error knows which setting to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidShellPath {
    value: String,
    reason: String,
}

impl std::fmt::Display for InvalidShellPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "shell_path {:?} {}", self.value, self.reason)
    }
}

impl std::error::Error for InvalidShellPath {}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(value: &str) -> String {
        ShellPath::parse(value)
            .expect_err(&format!("{value:?} must be refused"))
            .to_string()
    }

    #[test]
    fn the_default_is_the_root() {
        assert_eq!(ShellPath::default().as_str(), "/");
        assert_eq!(ShellPath::parse("/").unwrap(), ShellPath::default());
    }

    #[test]
    fn an_absolute_path_is_kept_as_written() {
        for value in [
            "/chat",
            "/chat/",
            "/app/shell",
            "/app/shell/",
            "/a-b_c.d~e",
            "/v1.2/start",
            "/x:y@z",
            "/!$&'()*+,;=",
            "/...",
            "/.well",
        ] {
            assert_eq!(ShellPath::parse(value).unwrap().as_str(), value);
        }
    }

    // `/chat` and `/chat/` are different paths to the host, and the loader
    // compares the page's path with this one as a string.
    #[test]
    fn a_trailing_slash_is_significant() {
        assert_ne!(
            ShellPath::parse("/chat").unwrap(),
            ShellPath::parse("/chat/").unwrap()
        );
    }

    #[test]
    fn a_relative_path_or_a_url_is_refused() {
        for value in [
            "",
            "chat",
            "./chat",
            "https://example.com/chat",
            "example.com/chat",
        ] {
            assert!(
                refused(value).contains("is not an absolute path"),
                "{value:?}: {}",
                refused(value)
            );
        }
    }

    #[test]
    fn a_host_is_refused() {
        // Protocol-relative: a browser reads `//host/...` as a host.
        for value in ["//example.com/chat", "/chat//x", "/chat//"] {
            assert!(refused(value).contains("empty segment"), "{value:?}");
        }
    }

    #[test]
    fn a_query_or_a_fragment_is_refused() {
        assert!(refused("/chat?x=1").contains("query"));
        assert!(refused("/chat?").contains("query"));
        assert!(refused("/chat#top").contains("fragment"));
    }

    #[test]
    fn a_dot_segment_is_refused() {
        for value in [
            "/..", "/../chat", "/chat/..", "/a/../b", "/.", "/a/./b", "/a/.",
        ] {
            assert!(refused(value).contains("segment"), "{value:?}");
        }
    }

    #[test]
    fn percent_encoding_is_refused() {
        assert!(refused("/ch%61t").contains("percent-encoded"));
        assert!(refused("/%2e%2e/chat").contains("percent-encoded"));
    }

    #[test]
    fn a_character_a_path_does_not_carry_is_refused() {
        for value in [
            "/ch at",
            "/chat\n",
            "/a\\b",
            "/caf\u{e9}",
            "/a\"b",
            "/a<b>",
            "/a`b",
            "/a{b}",
        ] {
            assert!(refused(value).contains("does not carry"), "{value:?}");
        }
    }

    #[test]
    fn the_refusal_names_the_field_and_the_value() {
        assert_eq!(
            refused("chat"),
            "shell_path \"chat\" is not an absolute path: it must start with `/` and name no \
             scheme or host"
        );
    }

    #[test]
    fn it_reads_and_writes_as_a_plain_string() {
        let path: ShellPath = serde_json::from_str("\"/chat\"").unwrap();
        assert_eq!(path.as_str(), "/chat");
        assert_eq!(serde_json::to_string(&path).unwrap(), "\"/chat\"");
        let error = serde_json::from_str::<ShellPath>("\"/a/../b\"").unwrap_err();
        assert!(
            error.to_string().contains("shell_path \"/a/../b\""),
            "{error}"
        );
    }
}
