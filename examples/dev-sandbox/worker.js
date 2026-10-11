// dev.impresspress.org's Cloudflare Worker: hands every request to the static
// asset store, unchanged.
//
// No COOP/COEP rule here, and that is deliberate. The in-browser Rust
// toolchain needs `/b/dev` to be cross-origin isolated AND the compiler
// worker's own script response to carry a matching COEP — but the compiler
// assets are on the service worker's bypass list (`impresspress.toml`), so
// nothing the runtime serves can carry that header for them.
//
// A header rule at THIS layer would fix it only here. The same bundle is
// served by `python3 -m http.server` in CI and by whatever a contributor runs
// locally, and "every host must be configured" is a rule with no enforcement
// point. So the rule lives in the one thing that ships inside the bundle and
// sits in front of every same-origin request: a workspace `sw.js` answers
// each bypassed request itself and adds `Cross-Origin-Embedder-Policy:
// credentialless` + `Cross-Origin-Opener-Policy: same-origin`
// (`crates/impresspress-bundle/assets/sw.js.tmpl`). Nothing is lost by the
// service worker being late to the first load of an origin: `/b/dev` is a
// runtime route, so until `sw.js` is installed the page that would start the
// compiler does not exist.
//
// This file stays a pass-through so that stays the single place the rule
// lives. The 25 MiB static-asset per-file cap is respected by the compiler
// packaging (`compiler/scripts/verify-compiler-assets.mjs`).
export default { fetch: (req, env) => env.ASSETS.fetch(req) };
