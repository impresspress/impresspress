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
//! bypass condition and `/asset-manifest.json`'s `bypass` field from ONE value
//! (its `BypassRules`), and the sandbox reads that field back through its
//! static-shell seam ([`ShellSource::bypass_rules`](super::ShellSource::bypass_rules)).
//! This module is the reading side: the same JSON shape, and the one check
//! every site-writing path applies — a single write, a batch, the scaffolder
//! and the seed importer.

use serde::{Deserialize, Serialize};

use super::workspace;

/// The service worker's bypass rules, as `/asset-manifest.json`'s `bypass`
/// states them: a request path equal to an `exact` entry, or starting with a
/// `prefixes` entry, goes to the static host instead of the runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BypassRules {
    #[serde(default)]
    pub exact: Vec<String>,
    #[serde(default)]
    pub prefixes: Vec<String>,
}

/// The one field of `/asset-manifest.json` [`BypassRules::from_asset_manifest`]
/// reads.
#[derive(Deserialize)]
struct AssetManifestBypass {
    /// Absent from a manifest written by an `impresspress-bundle` that
    /// predates the field — see [`BypassRules::from_asset_manifest`].
    #[serde(default)]
    bypass: BypassRules,
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
    /// The rules `/asset-manifest.json`'s bytes state.
    ///
    /// A manifest with no `bypass` field yields NO rules, not an error: it was
    /// written by a bundler that predates the field, and its service worker
    /// still bypasses what it always did — the sandbox simply cannot see
    /// which paths those are, so it refuses none, exactly as it did before
    /// the field existed. Refusing every site write on such a deployment
    /// instead would turn a missing safety check into a broken sandbox.
    ///
    /// `Err` only for bytes that are not a JSON object at all.
    pub fn from_asset_manifest(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice::<AssetManifestBypass>(bytes)
            .map(|manifest| manifest.bypass)
            .map_err(|e| format!("/asset-manifest.json did not parse: {e}"))
    }

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
    fn a_manifest_without_the_field_states_no_rules() {
        let parsed =
            BypassRules::from_asset_manifest(br#"{"buildId":"x","assets":{},"files":[]}"#).unwrap();
        assert_eq!(parsed, BypassRules::default());
        assert_eq!(parsed.refuse_shadowed("site/manifest.json"), Ok(()));
    }

    #[test]
    fn the_field_is_read_as_the_bundler_writes_it() {
        let parsed = BypassRules::from_asset_manifest(
            br#"{"buildId":"x","assets":{},"files":[],
                 "bypass":{"exact":["/sw.js","/manifest.json"],"prefixes":["/snippets/","/seed/"]}}"#,
        )
        .unwrap();
        assert_eq!(parsed, rules());
    }

    #[test]
    fn bytes_that_are_not_a_manifest_are_an_error() {
        assert!(BypassRules::from_asset_manifest(b"<!doctype html>").is_err());
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
