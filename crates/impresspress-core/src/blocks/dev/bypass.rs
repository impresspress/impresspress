//! The paths the runtime's service worker never routes to the runtime, and the
//! refusal of a site file at one.
//!
//! `sw.js` hands some same-origin requests straight to the static host — its
//! own script, `/manifest.json`, the wasm-bindgen `snippets/`, the sandbox's
//! `/seed/`, Cloudflare's `/cdn-cgi/`, an app's own extras. A site file at one
//! of those paths is shadowed: the write succeeds, a generation publishes, and
//! the page 404s, because the request for it never reaches the code that
//! serves the site. The likeliest collision is an agent building a PWA and
//! writing `site/manifest.json`.
//!
//! The rules are not restated here. `impresspress-bundle` renders `sw.js`'s
//! bypass condition and a `BYPASS_RULES` constant from ONE value (its
//! `BypassRules`), and the running worker hands that constant to the runtime
//! as `initialize({ bypass })`. The browser host keeps it on
//! [`DevShared::bypass`](super::DevShared::bypass), so every check reads the
//! rules of the worker actually in front of this runtime — no fetch, nothing
//! to fail, and not the rules a newer deployment may already be serving.
//! This module is the reading side: the same JSON shape, and the one check
//! every site-writing path applies — a single write, a batch, and the seed
//! importer.
//!
//! # What it covers
//!
//! A site file's OWN URL: `site/<path>` is checked at `/<path>`. The web
//! block's clean-URL aliases for the same file (`/about` for
//! `site/about.html`, `/blog/` for `site/blog/index.html`) are not: a
//! bypassed alias still leaves the file reachable at its own URL, so it is
//! not shadowed, and no default rule names such an alias anyway.

use serde::{Deserialize, Serialize};

use super::workspace;

/// The service worker's bypass rules, as `sw.js` hands them to
/// `initialize({ bypass })`: a request path equal to an `exact` entry, or
/// starting with a `prefixes` entry, goes to the static host instead of the
/// runtime.
///
/// [`Default`] is NO rules — what a runtime gets from an older `sw.js` that
/// passes none. Its worker still bypasses what it always did; the sandbox
/// simply cannot see which paths those are, so it refuses none, exactly as
/// it did before the rules were handed over.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassRules {
    #[serde(default)]
    pub exact: Vec<String>,
    #[serde(default)]
    pub prefixes: Vec<String>,
}

/// The rule a path matched — what a refusal names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassRule<'a> {
    /// `url.pathname === path`.
    Exact(&'a str),
    /// `url.pathname.startsWith(prefix)`.
    Prefix(&'a str),
}

impl std::fmt::Display for BypassRule<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exact(path) => write!(f, "the exact path {path:?}"),
            Self::Prefix(prefix) => write!(f, "everything under {prefix:?}"),
        }
    }
}

impl BypassRules {
    /// The rule that sends a request for `request_path` (`/manifest.json`)
    /// to the static host, if any — exact rules first, then prefixes, in the
    /// order the manifest lists them.
    pub fn shadowing(&self, request_path: &str) -> Option<BypassRule<'_>> {
        self.exact
            .iter()
            .find(|path| path.as_str() == request_path)
            .map(|path| BypassRule::Exact(path))
            .or_else(|| {
                self.prefixes
                    .iter()
                    .find(|prefix| request_path.starts_with(prefix.as_str()))
                    .map(|prefix| BypassRule::Prefix(prefix))
            })
    }

    /// Refuse a workspace file the service worker would shadow.
    ///
    /// Only the site area is served, at `/` + the path after `site/`
    /// (`site/manifest.json` at `/manifest.json`); a `blocks/…` path, or any
    /// other, is never requested by URL and is always `Ok`. The `Err` names
    /// the path, the URL it would be served at and the rule, and says why —
    /// it is what an agent reads back from the write it tried.
    pub fn refuse_shadowed(&self, workspace_path: &str) -> Result<(), String> {
        let Some(served) = workspace_path.strip_prefix(workspace::SITE_PREFIX) else {
            return Ok(());
        };
        let request_path = format!("/{served}");
        match self.shadowing(&request_path) {
            None => Ok(()),
            Some(rule) => Err(format!(
                "{workspace_path:?} would be served at {request_path:?}, which the runtime \
                 reserves ({rule}): the runtime's service worker serves this path from the \
                 static host, so a site file here would never be shown. Use a different path."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> BypassRules {
        BypassRules {
            exact: vec!["/sw.js".into(), "/manifest.json".into()],
            prefixes: vec!["/snippets/".into(), "/seed/".into()],
        }
    }

    #[test]
    fn no_rules_refuse_nothing() {
        assert_eq!(
            BypassRules::default().refuse_shadowed("site/manifest.json"),
            Ok(())
        );
    }

    #[test]
    fn the_rules_are_read_in_the_shape_the_bundler_renders() {
        let parsed: BypassRules = serde_json::from_str(
            r#"{"exact":["/sw.js","/manifest.json"],"prefixes":["/snippets/","/seed/"]}"#,
        )
        .unwrap();
        assert_eq!(parsed, rules());
    }

    #[test]
    fn an_exact_rule_shadows_that_path_only() {
        let rules = rules();
        let refusal = rules.refuse_shadowed("site/manifest.json").unwrap_err();
        assert!(refusal.contains("\"/manifest.json\""), "{refusal}");
        assert!(refusal.contains("the exact path"), "{refusal}");
        assert!(refusal.contains("never be shown"), "{refusal}");
        assert_eq!(rules.refuse_shadowed("site/app/manifest.json"), Ok(()));
        assert_eq!(rules.refuse_shadowed("site/manifest.json.bak"), Ok(()));
    }

    #[test]
    fn a_prefix_rule_shadows_everything_under_it() {
        let rules = rules();
        let refusal = rules.refuse_shadowed("site/snippets/x.js").unwrap_err();
        assert!(refusal.contains("\"/snippets/x.js\""), "{refusal}");
        assert!(refusal.contains("\"/snippets/\""), "{refusal}");
        assert!(rules.refuse_shadowed("site/seed/a/b.css").is_err());
        assert_eq!(rules.refuse_shadowed("site/snippet.js"), Ok(()));
        assert_eq!(rules.refuse_shadowed("site/vendor/bootstrap/x.css"), Ok(()));
    }

    #[test]
    fn only_the_site_area_is_served() {
        let rules = rules();
        assert_eq!(rules.refuse_shadowed("blocks/hello/sw.js"), Ok(()));
        assert_eq!(rules.refuse_shadowed("blocks/snippets/src/lib.rs"), Ok(()));
    }
}
