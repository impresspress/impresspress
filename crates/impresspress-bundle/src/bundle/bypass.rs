//! The service worker's bypass rules: which request paths `sw.js` hands to the
//! network (the static host) instead of the wasm runtime.
//!
//! [`BypassRules::for_bundle`] is the ONE place the list is assembled. Both of
//! its consumers in `sw.js.tmpl` read the value it returns, never a list of
//! their own:
//!
//! * `__BYPASS_CONDITION__`, rendered by [`BypassRules::render_condition`]
//!   into the fetch handler's `if (…)`;
//! * `__BYPASS_RULES__`, rendered by [`BypassRules::render_data`] into the
//!   `BYPASS_RULES` constant the worker passes to `initialize({ bypass })`.
//!   That is how the running runtime learns the rules of the worker in front
//!   of it: the development sandbox refuses a site file at a path the worker
//!   would never route to the runtime — a file there would publish and then
//!   404, because the request for it never reaches the code that serves the
//!   site.
//!
//! A rule the condition applied but the data did not state would be exactly
//! that silent shadowing again, which is why there is no second list.

use serde::{Deserialize, Serialize};

use super::AppConfig;

/// URL prefix the development sandbox's seed bundle is served under, added to
/// the service worker's bypass list whenever [`AppConfig::dev_enabled`].
///
/// The same value as `impresspress_core::blocks::dev::seed::ROOT`. It is
/// restated rather than imported because this crate deliberately depends on no
/// impresspress crate — it is native bundling tooling that the wasm32 runtime
/// never compiles — and pulling `impresspress-core` in for one string would
/// invert that. (`crates/impresspress/tests/seed_bypass_prefix.rs` compares
/// the two spellings.)
pub const SEED_BYPASS_PREFIX: &str = "/seed/";

/// The exact paths every bundle's service worker bypasses, whatever the app
/// configures: the worker's own script, the boot loader, the PWA manifest
/// (which the browser fetches as metadata) and the asset manifest. The model
/// engines' page-side scripts and the shell's vendored files are added after
/// these, from the asset lists that ship them — see
/// [`BypassRules::for_bundle`].
const BASE_EXACT: &[&str] = &[
    "/sw.js",
    "/loader.js",
    "/manifest.json",
    "/asset-manifest.json",
];

/// The prefixes every bundle's service worker bypasses, after the wasm-pack
/// glue's own prefix:
///
/// * `/snippets/` — the wasm-bindgen `snippets/` tree the glue imports.
/// * `/cdn-cgi/` — a path namespace Cloudflare reserves on every proxied
///   hostname (Web Analytics' RUM beacon, challenge pages, image resizing,
///   email obfuscation, `/cdn-cgi/trace`): never site or runtime content, and
///   only Cloudflare's edge can answer it. Without this the RUM beacon
///   Cloudflare injects into every page POSTs to `/cdn-cgi/rum` once the
///   worker controls the page, and the runtime answers 501.
const BASE_PREFIXES: &[&str] = &["/snippets/", "/cdn-cgi/"];

/// The request paths a bundle's service worker leaves to the network.
///
/// `exact` paths match `url.pathname === path`; `prefixes` match
/// `url.pathname.startsWith(prefix)`. Everything else same-origin goes to the
/// wasm runtime. `/` and `/index.html` are deliberately NOT here by default:
/// they are intercepted so the consumer's router can render a UI block at
/// root (an app may still list them in [`AppConfig::extra_bypass_exact`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BypassRules {
    pub exact: Vec<String>,
    pub prefixes: Vec<String>,
}

impl BypassRules {
    /// The rules for one bundle.
    ///
    /// `wasm_js_prefix` is the wasm-pack glue's base path (`/impresspress_web`),
    /// which prefixes both halves of the content-hashed pair.
    ///
    /// The page engines ([`crate::assets::PAGE_ENGINES`]) are bypassed by
    /// exact path, from the list that ships them.
    ///
    /// The shell's vendored files (sql.js) are bypassed by EXACT path, from
    /// the asset list that ships them ([`crate::assets::vendor_files`]), never
    /// as a `/vendor/` prefix: `/vendor/` is an ordinary directory for a
    /// site's own files (a seed's CSS framework, an agent's library), and a
    /// prefix would hand every one of them to the static host.
    ///
    /// A dev bundle also bypasses [`SEED_BYPASS_PREFIX`]: the sandbox's seed
    /// bundle is served by the static host, not by the runtime — on a cold
    /// boot the service worker fetches `/seed/manifest.json` and imports
    /// generation 0 from it, and a page asking for the same files must reach
    /// the host too. A runtime that intercepted the prefix would answer from
    /// the published site, which on the boot that needs the seed is empty.
    /// Added here rather than by every consumer, so "the sandbox is on" is the
    /// only thing an app has to say.
    ///
    /// A path an app lists that is already a rule is not listed twice.
    pub fn for_bundle(wasm_js_prefix: &str, app: &AppConfig) -> Self {
        let mut rules = Self::default();
        let exact = BASE_EXACT
            .iter()
            .map(|path| (*path).to_string())
            .chain(crate::assets::page_engine_scripts())
            .chain(crate::assets::vendor_files().map(|path| format!("/{path}")))
            .chain(app.extra_bypass_exact.iter().cloned());
        for path in exact {
            push_unique(&mut rules.exact, path);
        }
        let prefixes = std::iter::once(wasm_js_prefix.to_string())
            .chain(BASE_PREFIXES.iter().map(|prefix| (*prefix).to_string()))
            .chain(app.extra_bypass_prefix.iter().cloned())
            .chain(app.dev_enabled.then(|| SEED_BYPASS_PREFIX.to_string()));
        for prefix in prefixes {
            push_unique(&mut rules.prefixes, prefix);
        }
        rules
    }

    /// The JavaScript condition `sw.js.tmpl`'s `__BYPASS_CONDITION__` renders
    /// to: every
    /// exact rule, then every prefix rule, OR'd one clause per line.
    ///
    /// Every clause after the first LEADS with its `||`, so a clause can be
    /// removed by its exact text without leaving the expression dangling —
    /// the sandbox's export (`impresspress-core`'s `blocks::dev::export`)
    /// removes the compiler prefix's clause that way. Each value is quoted
    /// and escaped against a quote or backslash in the path.
    pub fn render_condition(&self) -> String {
        let clauses = self
            .exact
            .iter()
            .map(|path| format!("url.pathname === '{}'", js_quote(path)))
            .chain(
                self.prefixes
                    .iter()
                    .map(|prefix| format!("url.pathname.startsWith('{}')", js_quote(prefix))),
            );
        clauses.collect::<Vec<_>>().join(CLAUSE_SEPARATOR)
    }

    /// The rules as the JavaScript object literal `sw.js.tmpl`'s
    /// `__BYPASS_RULES__` renders to — `{"exact":[…],"prefixes":[…]}`, on one
    /// line. JSON is a JavaScript expression, and it is also exactly what the
    /// runtime parses back out of the `initialize()` options.
    pub fn render_data(&self) -> String {
        serde_json::to_string(self).expect("a list of strings always serializes")
    }
}

/// What joins two clauses of [`BypassRules::render_condition`]: the `||` and
/// the line break and indent that put each clause on a line of its own inside
/// the fetch handler's `if (`.
pub const CLAUSE_SEPARATOR: &str = " ||\n        ";

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.contains(&value) {
        list.push(value);
    }
}

fn js_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_bundle_has_the_base_rules_and_the_shell_vendor_files() {
        let rules = BypassRules::for_bundle("/app", &AppConfig::default());
        let expected: Vec<String> = BASE_EXACT
            .iter()
            .map(|path| (*path).to_string())
            .chain(crate::assets::page_engine_scripts())
            .chain(["/vendor/sql-wasm-esm.js", "/vendor/sql-wasm.wasm"].map(String::from))
            .collect();
        assert_eq!(rules.exact, expected);
        assert_eq!(rules.prefixes, ["/app", "/snippets/", "/cdn-cgi/"]);
    }

    #[test]
    fn a_dev_bundle_adds_the_seed_prefix_after_the_apps_own() {
        let app = AppConfig {
            dev_enabled: true,
            extra_bypass_prefix: vec!["/__impresspress_dev/compiler/".to_string()],
            extra_bypass_exact: vec!["/".to_string()],
            ..AppConfig::default()
        };
        let rules = BypassRules::for_bundle("/app", &app);
        assert_eq!(rules.exact.last().map(String::as_str), Some("/"));
        assert_eq!(
            rules.prefixes,
            [
                "/app",
                "/snippets/",
                "/cdn-cgi/",
                "/__impresspress_dev/compiler/",
                "/seed/",
            ]
        );
    }

    #[test]
    fn a_rule_an_app_repeats_is_listed_once() {
        let app = AppConfig {
            dev_enabled: true,
            extra_bypass_prefix: vec!["/seed/".to_string(), "/snippets/".to_string()],
            extra_bypass_exact: vec!["/sw.js".to_string()],
            ..AppConfig::default()
        };
        let rules = BypassRules::for_bundle("/app", &app);
        assert_eq!(rules.exact.iter().filter(|p| *p == "/sw.js").count(), 1);
        assert_eq!(
            rules.prefixes,
            ["/app", "/snippets/", "/cdn-cgi/", "/seed/"]
        );
    }

    #[test]
    fn the_data_is_the_rules_as_one_line_of_json() {
        let rules = BypassRules {
            exact: vec!["/a".to_string()],
            prefixes: vec!["/p/".to_string()],
        };
        assert_eq!(
            rules.render_data(),
            r#"{"exact":["/a"],"prefixes":["/p/"]}"#
        );
    }

    #[test]
    fn the_condition_puts_one_clause_per_line_each_leading_with_its_or() {
        let rules = BypassRules {
            exact: vec!["/a".to_string(), "/it's".to_string()],
            prefixes: vec!["/p/".to_string()],
        };
        assert_eq!(
            rules.render_condition(),
            concat!(
                "url.pathname === '/a' ||\n",
                "        url.pathname === '/it\\'s' ||\n",
                "        url.pathname.startsWith('/p/')",
            )
        );
    }
}
