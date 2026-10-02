use std::{fs, path::PathBuf};

use impresspress_bundle::bundle::{
    run, AppConfig, BypassRules, APP_TITLE_CLOSE, APP_TITLE_OPEN, BOOT_NOTICE_END,
    BOOT_NOTICE_START,
};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/bundle_fixtures/pkg-in")
}

/// Copy the fixture pkg into a fresh temp dir.
fn pkg_copy() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir(&fixture_path(), tmp.path());
    tmp
}

/// [`pkg_copy`] with the fixture's three-line `sw.js.tmpl` stub swapped for the
/// **shipped** template, and the shipped `loader.js.tmpl` beside it. Tests
/// that assert on real template content have to render the thing that
/// actually reaches a browser.
fn production_pkg_copy() -> tempfile::TempDir {
    let tmp = pkg_copy();
    let prod_tmpl = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/sw.js.tmpl"));
    fs::write(tmp.path().join("sw.js.tmpl"), prod_tmpl).unwrap();
    let loader_tmpl = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/loader.js.tmpl"
    ));
    fs::write(tmp.path().join("loader.js.tmpl"), loader_tmpl).unwrap();
    tmp
}

#[test]
fn exact_bypass_renders_into_sw() {
    let tmp = pkg_copy();

    let app = AppConfig {
        extra_bypass_exact: vec!["/".to_string(), "/index.html".to_string()],
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(
        sw.contains("url.pathname === '/'"),
        "sw.js missing exact '/' bypass = {sw}"
    );
    assert!(
        sw.contains("url.pathname === '/index.html'"),
        "sw.js missing exact '/index.html' bypass"
    );
    assert!(
        !sw.contains("__BYPASS_CONDITION__"),
        "placeholder not substituted"
    );
}

#[test]
fn exact_bypass_empty_leaves_sw_unchanged() {
    let tmp = pkg_copy();
    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");
    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(!sw.contains("__BYPASS_CONDITION__"));
    assert!(!sw.contains("=== '/'"));
}

#[test]
fn end_to_end_renames_rewrites_and_templates() {
    let tmp = pkg_copy();

    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");

    let manifest_body = fs::read_to_string(tmp.path().join("asset-manifest.json")).unwrap();
    assert!(manifest_body.contains("\"buildId\""));
    assert!(manifest_body.contains("\"app.js\""));
    assert!(manifest_body.contains("\"app_bg.wasm\""));

    let entries: Vec<String> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        entries
            .iter()
            .any(|n| n.starts_with("app-") && n.ends_with(".js")),
        "missing hashed JS in {entries:?}"
    );
    assert!(entries
        .iter()
        .any(|n| n.starts_with("app_bg-") && n.ends_with(".wasm")));
    assert!(!entries.iter().any(|n| n == "app.js"));
    assert!(!entries.iter().any(|n| n == "app_bg.wasm"));

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(sw.contains("from '/app-"), "sw.js = {sw}");
    assert!(!sw.contains("__WASM_JS__"));
    assert!(!sw.contains("__BUILD_ID__"));

    let glue_name = entries
        .iter()
        .find(|n| n.starts_with("app-") && n.ends_with(".js"))
        .unwrap();
    let glue = fs::read_to_string(tmp.path().join(glue_name)).unwrap();
    assert!(glue.contains("app_bg-"), "glue = {glue}");
    assert!(!glue.contains("'app_bg.wasm'"));
}

/// `files` is the whole shell, and it is what the development sandbox's
/// export copies into the bundle it hands the user. Three properties have to
/// hold, and each has a way of failing silently:
///
///  * the RENDERED templates are listed, not the `*.tmpl` inputs — a listing
///    taken before rendering would name three files that no longer exist and
///    omit the three a browser actually loads;
///  * the hashed wasm-pack pair is listed under the names it was RENAMED to,
///    since the un-hashed originals are gone by then;
///  * nested directories are walked, `/`-separated and sorted — the
///    wasm-bindgen `snippets/` tree lives one level down and a shell missing
///    it cannot load its own module.
#[test]
fn the_manifest_lists_every_file_of_the_rendered_shell() {
    let tmp = pkg_copy();
    std::fs::create_dir_all(tmp.path().join("snippets/inner")).unwrap();
    std::fs::write(tmp.path().join("snippets/inner/glue.js"), "export {};").unwrap();

    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");

    let body = fs::read_to_string(tmp.path().join("asset-manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&body).unwrap();
    let files: Vec<String> = manifest["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    assert!(files.contains(&"sw.js".to_string()), "{files:?}");
    assert!(files.contains(&"index.html".to_string()), "{files:?}");
    assert!(
        files.contains(&"snippets/inner/glue.js".to_string()),
        "the walk must recurse; {files:?}"
    );
    assert!(
        files.contains(&"asset-manifest.json".to_string()),
        "the manifest names itself, so copying every listed file copies the \
         whole shell; {files:?}"
    );
    // The inputs, not the outputs: `render_if_exists` deletes each template
    // it renders, and anything still ending in `.tmpl` is bundler input.
    assert!(!files.iter().any(|f| f.ends_with(".tmpl")), "{files:?}");
    // The hashed pair, under the names the rename produced.
    assert!(
        files
            .iter()
            .any(|f| f.starts_with("app-") && f.ends_with(".js")),
        "{files:?}"
    );
    assert!(
        files
            .iter()
            .any(|f| f.starts_with("app_bg-") && f.ends_with(".wasm")),
        "{files:?}"
    );
    let mut sorted = files.clone();
    sorted.sort();
    assert_eq!(files, sorted, "the listing must be sorted");
}

#[test]
fn deterministic_across_runs() {
    let tmp1 = pkg_copy();
    let tmp2 = pkg_copy();
    impresspress_bundle::bundle::run(tmp1.path(), tmp1.path(), AppConfig::default()).unwrap();
    impresspress_bundle::bundle::run(tmp2.path(), tmp2.path(), AppConfig::default()).unwrap();

    let m1 = fs::read_to_string(tmp1.path().join("asset-manifest.json")).unwrap();
    let m2 = fs::read_to_string(tmp2.path().join("asset-manifest.json")).unwrap();
    let v1: serde_json::Value = serde_json::from_str(&m1).unwrap();
    let v2: serde_json::Value = serde_json::from_str(&m2).unwrap();
    assert_eq!(v1.get("assets"), v2.get("assets"));
}

#[test]
fn empty_exact_leaves_production_sw_bypass_unchanged() {
    let tmp = production_pkg_copy();

    // AppConfig::default() has both extra_bypass_prefix and extra_bypass_exact empty.
    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();

    assert!(
        !sw.contains("__BYPASS_CONDITION__"),
        "placeholder not substituted in sw.js"
    );
    // The whole default condition: the base exact paths, the shell's vendor
    // files, then the prefixes — one clause per line, each after the first
    // leading with its `||`, nothing trailing and nothing dangling.
    assert!(
        sw.contains(concat!(
            "    if (url.pathname === '/sw.js' ||\n",
            "        url.pathname === '/loader.js' ||\n",
            "        url.pathname === '/manifest.json' ||\n",
            "        url.pathname === '/asset-manifest.json' ||\n",
            "        url.pathname === '/webllm-engine.js' ||\n",
            "        url.pathname === '/embed-engine.js' ||\n",
            "        url.pathname === '/t2i-engine.js' ||\n",
            "        url.pathname === '/vendor/sql-wasm-esm.js' ||\n",
            "        url.pathname === '/vendor/sql-wasm.wasm' ||\n",
            "        url.pathname.startsWith('/app') ||\n",
            "        url.pathname.startsWith('/snippets/') ||\n",
            "        url.pathname.startsWith('/cdn-cgi/')) {",
        )),
        "production bypass condition changed; sw.js = {sw}"
    );
}

#[test]
fn sw_passes_the_dev_flag_to_initialize() {
    let tmp = production_pkg_copy();

    let app = AppConfig {
        dev_enabled: true,
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    // ONE rendered constant, read by `initialize()` and by the passthrough
    // branch alike — see `sw.js.tmpl`'s header for why the flag is stated
    // once rather than substituted at each use.
    assert!(
        sw.contains("const DEV_ENABLED = true;"),
        "sw.js did not receive the dev flag; sw.js = {sw}"
    );
    assert!(
        sw.contains("initialize({ dev: DEV_ENABLED, bypass: BYPASS_RULES })"),
        "initialize() must read the one constant; sw.js = {sw}"
    );
}

#[test]
fn sw_defaults_the_dev_flag_to_false() {
    let tmp = production_pkg_copy();

    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    // The sandbox is off unless a bundle asks for it: an app that never sets
    // `[dev] enabled` must ship a service worker that boots the runtime with
    // the flag explicitly false, not merely absent.
    assert!(sw.contains("const DEV_ENABLED = false;"), "sw.js = {sw}");
    assert!(
        sw.contains("initialize({ dev: DEV_ENABLED, bypass: BYPASS_RULES })"),
        "initialize() must read the one constant; sw.js = {sw}"
    );
    assert!(
        !sw.contains("__DEV_ENABLED__"),
        "placeholder not substituted in sw.js"
    );
}

/// The sandbox's seed bundle is fetched from the static host on a cold boot,
/// so the service worker has to let `/seed/` through. It is the runtime's own
/// flag that decides, not a second thing an app has to remember to configure.
#[test]
fn a_dev_bundle_bypasses_the_seed_prefix() {
    let tmp = production_pkg_copy();

    let app = AppConfig {
        dev_enabled: true,
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(
        sw.contains("url.pathname.startsWith('/seed/')"),
        "sw.js does not bypass the seed bundle; sw.js = {sw}"
    );
    assert!(
        !sw.contains("__BYPASS_CONDITION__"),
        "placeholder not substituted"
    );
}

/// And only then: a bundle with no sandbox serves no seed, so intercepting
/// `/seed/` is the correct (and unchanged) behaviour.
#[test]
fn a_non_dev_bundle_does_not_bypass_the_seed_prefix() {
    let tmp = production_pkg_copy();

    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(!bypasses(&sw, "/seed/manifest.json"), "sw.js = {sw}");
}

/// The rendered `sw.js`'s bypass `if (…) {` expression, read back clause by
/// clause (`url.pathname === '…'` and `url.pathname.startsWith('…')`, OR'd)
/// into the rules it applies. Panics on a clause of any other shape, so a
/// template change this cannot read fails loudly instead of being read as
/// "not bypassed".
fn sw_bypass_rules(sw: &str) -> BypassRules {
    let start = sw
        .find("if (url.pathname === '/sw.js' ||")
        .expect("sw.js has its bypass expression");
    let expression = &sw[start + "if (".len()..];
    let expression = &expression[..expression.find(") {").expect("bypass expression closes")];
    let mut rules = BypassRules::default();
    for clause in expression.split("||").map(str::trim) {
        if let Some(rest) = clause.strip_prefix("url.pathname === '") {
            let path = rest.strip_suffix('\'').expect("quoted exact path");
            rules.exact.push(js_unquote(path));
        } else if let Some(rest) = clause.strip_prefix("url.pathname.startsWith('") {
            let prefix = rest.strip_suffix("')").expect("quoted prefix");
            rules.prefixes.push(js_unquote(prefix));
        } else {
            panic!("unrecognised bypass clause {clause:?} in sw.js = {sw}")
        }
    }
    rules
}

/// The value of a single-quoted JavaScript string's body: `\\` and `\'`
/// unescaped, the two escapes the bundler emits. Any other backslash sequence
/// panics, so an escape this reader does not know is not read as a path.
fn js_unquote(body: &str) -> String {
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(escaped @ ('\\' | '\'')) => out.push(escaped),
                other => panic!("unrecognised escape \\{other:?} in {body:?}"),
            }
        } else {
            assert_ne!(c, '\'', "unescaped quote in {body:?}");
            out.push(c);
        }
    }
    out
}

/// Whether the rendered `sw.js` bypasses `path`, by [`sw_bypass_rules`].
fn bypasses(sw: &str, path: &str) -> bool {
    let rules = sw_bypass_rules(sw);
    rules.exact.iter().any(|exact| exact == path)
        || rules
            .prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
}

/// The `BYPASS_RULES` constant the rendered `sw.js` hands `initialize()`.
fn sw_initialize_rules(sw: &str) -> BypassRules {
    let declaration = "const BYPASS_RULES = ";
    assert_eq!(sw.matches(declaration).count(), 1, "sw.js = {sw}");
    let start = sw.find(declaration).unwrap() + declaration.len();
    let value = &sw[start..start + sw[start..].find(";\n").expect("declaration ends")];
    serde_json::from_str(value).expect("BYPASS_RULES is JSON")
}

/// The rules `sw.js` hands the runtime in `initialize()` are what the running
/// runtime believes this worker leaves to the network — the development
/// sandbox refuses a site file at any path they list. They must be exactly
/// the rules the fetch handler's condition applies, for a plain bundle and
/// for a dev bundle with an app's own extras: a rule only the condition had
/// would be a site file that publishes and 404s, and a rule only the data had
/// would refuse a file the runtime could serve.
#[test]
fn initialize_is_handed_exactly_the_bypass_rules_the_fetch_handler_applies() {
    let configs = [
        AppConfig::default(),
        AppConfig {
            dev_enabled: true,
            // A quote and a backslash in configured paths: the condition
            // escapes them for a JavaScript string and the data for JSON, and
            // both must come back as the same path.
            extra_bypass_prefix: vec![
                "/__impresspress_dev/compiler/".to_string(),
                "/it's/".to_string(),
            ],
            extra_bypass_exact: vec![
                "/".to_string(),
                "/index.html".to_string(),
                "/back\\slash's.js".to_string(),
            ],
            ..AppConfig::default()
        },
    ];
    for app in configs {
        let dev = app.dev_enabled;
        let tmp = production_pkg_copy();
        run(tmp.path(), tmp.path(), app).expect("bundler ok");
        let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
        let handed = sw_initialize_rules(&sw);

        assert!(
            sw.contains("await initialize({ dev: DEV_ENABLED, bypass: BYPASS_RULES });"),
            "sw.js = {sw}"
        );
        // Rule for rule, in order.
        assert_eq!(handed, sw_bypass_rules(&sw), "dev = {dev}");
        // And each handed rule, put to the rendered condition, is bypassed —
        // so the comparison above is not two readings of the same mistake.
        for path in &handed.exact {
            assert!(bypasses(&sw, path), "{path} (dev = {dev}); sw.js = {sw}");
        }
        for prefix in &handed.prefixes {
            let under = format!("{prefix}x");
            assert!(bypasses(&sw, &under), "{under} (dev = {dev}); sw.js = {sw}");
        }
        assert!(handed.exact.contains(&"/manifest.json".to_string()));
        assert_eq!(
            handed.prefixes.contains(&"/seed/".to_string()),
            dev,
            "the seed prefix is a dev bundle's alone"
        );
        if dev {
            assert!(handed
                .prefixes
                .contains(&"/__impresspress_dev/compiler/".to_string()));
            assert!(handed.exact.contains(&"/index.html".to_string()));
            assert!(handed.exact.contains(&"/back\\slash's.js".to_string()));
            assert!(handed.prefixes.contains(&"/it's/".to_string()));
        }
    }
}

/// The shell owns two files under `/vendor/`, and the service worker bypasses
/// exactly those — rendered from the asset list that ships them — not the
/// whole prefix, which would shadow every site file under `/vendor/`.
#[test]
fn sw_bypasses_exactly_the_shells_vendor_files() {
    let tmp = production_pkg_copy();
    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");
    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();

    assert!(
        !sw.contains("startsWith('/vendor/')"),
        "sw.js still bypasses the whole /vendor/ prefix; sw.js = {sw}"
    );
    let vendor: Vec<&str> = impresspress_bundle::assets::vendor_files().collect();
    assert_eq!(vendor, ["vendor/sql-wasm-esm.js", "vendor/sql-wasm.wasm"]);
    for file in vendor {
        let clause = format!(" ||\n        url.pathname === '/{file}'");
        assert_eq!(sw.matches(&clause).count(), 1, "{clause} in sw.js = {sw}");
        assert!(bypasses(&sw, &format!("/{file}")), "/{file} is bypassed");
    }
}

/// A site file under `/vendor/` reaches the runtime — in a dev bundle too,
/// whose bypass list is the longest.
#[test]
fn a_site_file_under_vendor_is_not_bypassed() {
    let tmp = production_pkg_copy();
    let app = AppConfig {
        dev_enabled: true,
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");
    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();

    // The reader is load-bearing: it does see the bypasses that are there.
    assert!(bypasses(&sw, "/seed/manifest.json"));
    assert!(bypasses(&sw, "/vendor/sql-wasm.wasm"));
    for path in [
        "/vendor/bootstrap/bootstrap.min.css",
        "/vendor/bootstrap/bootstrap.bundle.min.js",
        "/vendor/sql-wasm.wasm.map",
    ] {
        assert!(!bypasses(&sw, path), "{path} is bypassed; sw.js = {sw}");
    }
}

/// Cloudflare reserves `/cdn-cgi/` on every proxied hostname — the RUM
/// beacon it injects into each page POSTs to `/cdn-cgi/rum` — so the service
/// worker leaves the whole prefix to the network, in dev and non-dev bundles
/// alike, and never routes it to the runtime (which would answer 501).
#[test]
fn cloudflare_cdn_cgi_paths_are_bypassed() {
    for dev_enabled in [false, true] {
        let tmp = production_pkg_copy();
        let app = AppConfig {
            dev_enabled,
            ..AppConfig::default()
        };
        run(tmp.path(), tmp.path(), app).expect("bundler ok");
        let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();

        for path in ["/cdn-cgi/rum", "/cdn-cgi/trace"] {
            assert!(
                bypasses(&sw, path),
                "{path} reaches the runtime (dev = {dev_enabled}); sw.js = {sw}"
            );
        }
        // The prefix, not a look-alike: a site page that merely starts with
        // the same letters is still the runtime's.
        assert!(
            !bypasses(&sw, "/cdn-cgi-notes.html"),
            "dev = {dev_enabled}; sw.js = {sw}"
        );
    }
}

/// Nothing the shell serves starts with `/sql-` (sql.js lives under
/// `/vendor/`), so a site page whose name does is the runtime's to serve.
#[test]
fn a_site_page_named_sql_something_is_not_bypassed() {
    let tmp = production_pkg_copy();
    let app = AppConfig {
        dev_enabled: true,
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");
    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();

    assert!(!sw.contains("'/sql-"), "sw.js = {sw}");
    for path in ["/sql-tips.html", "/sql-wasm.wasm"] {
        assert!(!bypasses(&sw, path), "{path} is bypassed; sw.js = {sw}");
    }
}

/// A dev bundle's service worker is the deployment's header layer.
///
/// The in-browser Rust toolchain runs in a dedicated worker started from a
/// document that is COEP `credentialless`, and a browser refuses such a worker
/// unless the worker SCRIPT's own response carries a compatible COEP. That
/// script is a bypassed static file, so no runtime response can carry it and
/// no static host is guaranteed to send it — which is why `sw.js` answers
/// every bypassed same-origin request itself in a dev bundle and adds the
/// pair. Without this the compiler cannot start on any host, and the only
/// symptom the page gets is an empty `Worker` error event.
#[test]
fn a_dev_bundle_adds_the_isolation_headers_to_bypassed_responses() {
    let tmp = production_pkg_copy();

    let app = AppConfig {
        dev_enabled: true,
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    // Gated on the one build-time constant, not decided at runtime — the
    // same constant `initialize()` is passed, so a bundle cannot be dev in
    // one half of this file and not in the other.
    assert!(
        sw.contains("const DEV_ENABLED = true;")
            && sw.contains("if (DEV_ENABLED && url.pathname !== '/sw.js') {"),
        "the dev rendering must take the passthrough branch; sw.js = {sw}"
    );
    assert!(
        sw.contains("event.respondWith(passthrough(event.request));"),
        "sw.js = {sw}"
    );
    // Both headers, with the values `blocks/dev/page.rs` and the
    // security-headers block use for the rest of the origin.
    assert!(
        sw.contains("headers.set('Cross-Origin-Embedder-Policy', 'credentialless');"),
        "sw.js = {sw}"
    );
    assert!(
        sw.contains("headers.set('Cross-Origin-Opener-Policy', 'same-origin');"),
        "sw.js = {sw}"
    );
    // Streamed, not buffered: these are multi-megabyte wasm parts.
    assert!(
        sw.contains("return new Response(response.body, {"),
        "the passthrough must pipe the body, not read it; sw.js = {sw}"
    );
    // An opaque or error response has no readable headers and no exposed
    // body, so rebuilding it would silently replace it with an empty 200.
    assert!(
        sw.contains("response.type === 'opaque'") && sw.contains("response.type === 'error'"),
        "sw.js = {sw}"
    );
}

/// And the other rendering, explicitly: a bundle with no sandbox has no
/// compiler worker to start and no site-wide isolation to be consistent with,
/// so its bypassed requests go to the network untouched — the behaviour every
/// bundle had before the sandbox existed, and one less service-worker round
/// trip per static asset.
#[test]
fn a_non_dev_bundle_leaves_bypassed_responses_to_the_network() {
    let tmp = production_pkg_copy();

    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(
        sw.contains("const DEV_ENABLED = false;")
            && sw.contains("if (DEV_ENABLED && url.pathname !== '/sw.js') {"),
        "the non-dev rendering must not take the passthrough branch; sw.js = {sw}"
    );
    // The early return is still the last statement of the bypass branch.
    assert!(
        sw.contains(
            "        }\n        return;\n    }\n    event.respondWith(handleFetch(event));"
        ),
        "the bypass branch must still end in the plain early return; sw.js = {sw}"
    );
}

/// `handle_request` resolves to `{ response, after }`, and `after` — the
/// request's audit row and deferred tasks, run once the response is out — is
/// kept alive with `event.waitUntil`. Without it the browser may stop the
/// worker as soon as the response is delivered, and that work is lost; no
/// end-to-end test can see the loss, because a worker under test is rarely
/// stopped that fast. `waitUntil` needs the fetch event, so `handleFetch`
/// takes the event rather than the request.
#[test]
fn the_fetch_handler_keeps_the_worker_alive_for_after_response_work() {
    let tmp = production_pkg_copy();

    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    assert!(
        sw.contains("event.respondWith(handleFetch(event));")
            && sw.contains("async function handleFetch(event) {"),
        "handleFetch must receive the fetch event; sw.js = {sw}"
    );
    assert!(
        sw.contains(
            "        const { response, after } = await handle_request(request);\n        \
             event.waitUntil(after);\n        return response;"
        ),
        "the after-response promise must be handed to waitUntil before the response \
         is returned; sw.js = {sw}"
    );
}

/// The seed prefix joins whatever the app already asked for rather than
/// replacing it — `examples/dev-sandbox` also bypasses the compiler worker.
#[test]
fn the_seed_prefix_joins_an_apps_own_bypass_list() {
    let tmp = production_pkg_copy();

    let app = AppConfig {
        dev_enabled: true,
        extra_bypass_prefix: vec!["/__impresspress_dev/compiler/".to_string()],
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");

    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    // The exact clause text the sandbox's export removes
    // (`impresspress-core`'s `blocks::dev::export::sw_without_compiler`):
    // a clause that led with anything else would survive into every export.
    assert!(sw.contains(" ||\n        url.pathname.startsWith('/__impresspress_dev/compiler/')"));
    assert!(sw.contains("url.pathname.startsWith('/seed/')"));
}

/// Run `node` with `args` and `env`, failing the test with its output if it
/// does not exit cleanly — or if there is no `node` to run. Not a skip: a
/// check that quietly does not run is a check that passes on a broken worker.
fn node(args: &[&std::ffi::OsStr], env: &[(&str, &std::path::Path)]) {
    let output = std::process::Command::new("node")
        .args(args)
        .envs(env.iter().copied())
        .output()
        .expect("`node` must be on PATH: the rendered scripts are checked by running them");
    assert!(
        output.status.success(),
        "node {args:?} failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The shipped templates rendered into a fresh directory, with
/// `opfs_wipe_on_recovery` as given.
fn rendered_shell(opfs_wipe_on_recovery: bool) -> tempfile::TempDir {
    let tmp = production_pkg_copy();
    let app = AppConfig {
        opfs_wipe_on_recovery,
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");
    tmp
}

/// `node --test` on one file of `tests/sw/`, against both renderings of
/// `rendered` (`sw.js` or `loader.js`): the default, in `plain_var`, and the
/// `opfs_wipe_on_recovery` one, in `wipe_var`.
fn node_test(test_file: &str, rendered: &str, plain_var: &str, wipe_var: &str) {
    let plain = rendered_shell(false);
    let wipe = rendered_shell(true);
    let tests = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/sw")
        .join(test_file);
    node(
        &["--test".as_ref(), tests.as_os_str()],
        &[
            (plain_var, &plain.path().join(rendered)),
            (wipe_var, &wipe.path().join(rendered)),
        ],
    );
}

/// The shipped templates, rendered, are scripts a JavaScript engine accepts.
/// Every other test in this file reads the worker as text, and text that
/// contains the right substrings can still be a file no browser will run.
#[test]
fn the_rendered_worker_parses() {
    for dev_enabled in [false, true] {
        let tmp = production_pkg_copy();
        let app = AppConfig {
            dev_enabled,
            ..AppConfig::default()
        };
        run(tmp.path(), tmp.path(), app).expect("bundler ok");
        // `.mjs`: the worker is registered as a module (it `import`s the wasm
        // glue), and that is how it has to be parsed.
        let module = tmp.path().join("sw-check.mjs");
        fs::copy(tmp.path().join("sw.js"), &module).unwrap();
        node(&["--check".as_ref(), module.as_os_str()], &[]);
        // `loader.js` is a classic script, and is checked as one.
        let loader = tmp.path().join("loader.js");
        node(&["--check".as_ref(), loader.as_os_str()], &[]);
    }
}

/// `sw.js` leaves the cause of a dead runtime in Cache Storage and
/// `loader.js` reads it there. Two files, two declarations of the same two
/// names — a rename in one is a cause written where nothing looks.
#[test]
fn the_worker_and_the_loader_agree_on_where_the_stop_cause_is_left() {
    let tmp = production_pkg_copy();
    run(tmp.path(), tmp.path(), AppConfig::default()).expect("bundler ok");
    let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
    let loader = fs::read_to_string(tmp.path().join("loader.js")).unwrap();

    for declaration in [
        "const STOP_CAUSE_CACHE = '__impresspress_sw_stopped';",
        "const STOP_CAUSE_KEY = '/__impresspress_sw_stopped';",
    ] {
        assert_eq!(sw.matches(declaration).count(), 1, "sw.js = {sw}");
        assert_eq!(
            loader.matches(declaration).count(),
            1,
            "loader.js = {loader}"
        );
    }
    // The worker's answer carries `cause`, which is what the loader's boot
    // probe reads back.
    assert!(sw.contains("cause: poisonReason"), "sw.js = {sw}");
    assert!(
        loader.contains("body.code === 'runtime_stopped' ? String(body.cause) : null"),
        "loader.js = {loader}"
    );
}

/// Once the wasm runtime is dead, a request only it could have answered gets
/// a 503 that names the cause and says what a reload will do, and a
/// navigation still reaches the static host. The behaviour is driven in Node
/// against the rendered file — `tests/sw/sw_runtime_stopped.test.mjs` says
/// what and why.
#[test]
fn the_rendered_worker_answers_for_a_stopped_runtime() {
    node_test(
        "sw_runtime_stopped.test.mjs",
        "sw.js",
        "SW_JS",
        "SW_JS_WIPE",
    );
}

/// The boot shell acts on a cause only when it is about this load, recovers
/// automatically once per failure, and does not mistake a probe the runtime
/// died on for a boot that worked — `tests/sw/loader_recovery.test.mjs`.
#[test]
fn the_rendered_loader_recovers_once_and_keeps_the_cause() {
    node_test(
        "loader_recovery.test.mjs",
        "loader.js",
        "LOADER_JS",
        "LOADER_JS_WIPE",
    );
}

/// `sw.js` states what a reload costs from the same build-time flag
/// `loader.js` acts on: one `AppConfig` field, rendered into both.
#[test]
fn the_worker_and_the_loader_are_rendered_with_the_same_wipe_flag() {
    for wipe in [false, true] {
        let tmp = rendered_shell(wipe);
        let declaration = format!("const OPFS_WIPE_ON_RECOVERY = {wipe};");
        for file in ["sw.js", "loader.js"] {
            let body = fs::read_to_string(tmp.path().join(file)).unwrap();
            assert_eq!(body.matches(&declaration).count(), 1, "{file} = {body}");
        }
    }
}

// ---------------------------------------------------------------------------
// The boot shell's text, and what the bundler does NOT put in a bundle
// ---------------------------------------------------------------------------

/// [`production_pkg_copy`] with the shipped `index.html.tmpl` as well.
fn production_pkg_copy_with_index() -> tempfile::TempDir {
    let tmp = production_pkg_copy();
    let index_tmpl = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/index.html.tmpl"
    ));
    fs::write(tmp.path().join("index.html.tmpl"), index_tmpl).unwrap();
    tmp
}

const NOTICE: &str =
    r#"<p>This is a build sandbox. Read <a href="/llms.txt">/llms.txt</a> first.</p>"#;

/// An app's boot notice is in the HTML the static host serves — readable by
/// a client that never runs `loader.js` — between the two markers that let it
/// be found again, and beside the status line `loader.js` writes to.
#[test]
fn the_boot_shell_carries_an_apps_boot_notice_between_its_markers() {
    let tmp = production_pkg_copy_with_index();
    let app = AppConfig {
        app_title: Some("Sandbox".to_string()),
        boot_notice_html: Some(NOTICE.to_string()),
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");
    let index = fs::read_to_string(tmp.path().join("index.html")).unwrap();
    assert!(
        index.contains(&format!("{BOOT_NOTICE_START}{NOTICE}{BOOT_NOTICE_END}")),
        "{index}"
    );
    assert_eq!(index.matches(BOOT_NOTICE_START).count(), 1, "{index}");
    assert_eq!(index.matches(BOOT_NOTICE_END).count(), 1, "{index}");
    // What #117's loader needs is still there, after the notice.
    let status = index
        .find(r#"<p id="status">Loading...</p>"#)
        .expect("status line");
    assert!(index.find(NOTICE).unwrap() < status, "{index}");
    assert!(index.contains(r#"<script src="/loader.js"></script>"#));
}

/// The template is generic: an app with no notice gets the empty region and
/// a `<noscript>` line built from its own title — and not a word about a
/// sandbox, an `llms.txt` or `/b/dev`.
#[test]
fn a_plain_boot_shell_has_an_empty_notice_region_and_no_sandbox_wording() {
    let tmp = production_pkg_copy_with_index();
    let app = AppConfig {
        app_title: Some("My Shop".to_string()),
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");
    let index = fs::read_to_string(tmp.path().join("index.html")).unwrap();
    assert!(
        index.contains(&format!("{BOOT_NOTICE_START}{BOOT_NOTICE_END}")),
        "{index}"
    );
    assert!(
        index.contains(
            "<noscript><p><span data-app-title>My Shop</span> runs in your browser and needs JavaScript to start.</p></noscript>"
        ),
        "{index}"
    );
    let lower = index.to_lowercase();
    for word in ["sandbox", "llms.txt", "/b/dev", "impresspress.org"] {
        assert!(
            !lower.contains(word),
            "the generic shell says {word:?}: {index}"
        );
    }
}

/// Every place the shell shows the app's title is one a consumer can find by
/// exact text — `<title>`, and the wrapper around each of the two in the
/// body — and the title is text there, never markup.
#[test]
fn the_boot_shell_shows_the_title_only_where_it_can_be_found_again() {
    let tmp = production_pkg_copy_with_index();
    let app = AppConfig {
        app_title: Some("Kiln & <Co>".to_string()),
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");
    let index = fs::read_to_string(tmp.path().join("index.html")).unwrap();
    let escaped = "Kiln &amp; &lt;Co&gt;";
    assert!(
        index.contains(&format!("<title>{escaped}</title>")),
        "{index}"
    );
    let wrapped = format!("{APP_TITLE_OPEN}{escaped}{APP_TITLE_CLOSE}");
    assert_eq!(index.matches(&wrapped).count(), 2, "{index}");
    // …and nowhere else: three occurrences, all accounted for.
    assert_eq!(index.matches(escaped).count(), 3, "{index}");
    assert!(!index.contains("Kiln & <Co>"), "{index}");
}

/// `loader.js` names the app to a PERSON only by what the page shows
/// (`appTitle()`); the name it was built with appears in console lines and
/// nowhere else. A baked name in visible text would survive a copy of the
/// shell that retitles `index.html` — the development sandbox's export —
/// and go on telling the copied site's visitors "Loading dev-sandbox...".
#[test]
fn the_loader_bakes_the_app_name_into_console_lines_only() {
    let tmp = production_pkg_copy();
    let app = AppConfig {
        app_name: Some("zq-build-name".to_string()),
        ..AppConfig::default()
    };
    run(tmp.path(), tmp.path(), app).expect("bundler ok");
    let loader = fs::read_to_string(tmp.path().join("loader.js")).unwrap();
    let naming: Vec<&str> = loader
        .lines()
        .filter(|line| line.contains("zq-build-name"))
        .collect();
    assert!(!naming.is_empty(), "the console prefix is still rendered");
    for line in naming {
        assert!(
            line.trim_start().starts_with("console."),
            "loader.js shows the build's name outside a console line: {line}"
        );
    }
    assert!(loader.contains("document.querySelector('[data-app-title]')"));
}

/// A notice carrying one of the markers would end its own region early (or
/// open a second), and whoever removes the notice later would cut in the
/// wrong place.
#[test]
fn a_boot_notice_containing_a_marker_is_refused() {
    for marker in [BOOT_NOTICE_START, BOOT_NOTICE_END] {
        let tmp = production_pkg_copy_with_index();
        let app = AppConfig {
            boot_notice_html: Some(format!("<p>hi</p>{marker}")),
            ..AppConfig::default()
        };
        let err = run(tmp.path(), tmp.path(), app).expect_err("refused");
        assert!(err.to_string().contains("boot notice"), "{err}");
    }
}

/// `/llms.txt` is not the bundler's: no bundle — plain or dev, with a boot
/// notice or without — carries the file, lists it in the shell, or keeps the
/// path from the runtime. A deployment that wants a static one overlays it
/// (the dev sandbox does); the runtime must still be the one asked once the
/// worker controls the page, or a site's own `llms.txt` would be shadowed.
#[test]
fn no_bundle_emits_lists_or_bypasses_llms_txt() {
    for dev_enabled in [false, true] {
        let tmp = production_pkg_copy_with_index();
        let app = AppConfig {
            dev_enabled,
            boot_notice_html: dev_enabled.then(|| NOTICE.to_string()),
            ..AppConfig::default()
        };
        let rules = BypassRules::for_bundle("/app", &app);
        run(tmp.path(), tmp.path(), app).expect("bundler ok");

        assert!(!tmp.path().join("llms.txt").exists(), "dev={dev_enabled}");
        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(tmp.path().join("asset-manifest.json")).unwrap(),
        )
        .unwrap();
        let files = manifest["files"].as_array().expect("files");
        assert!(
            !files.iter().any(|f| f.as_str() == Some("llms.txt")),
            "dev={dev_enabled}: {files:?}"
        );
        let sw = fs::read_to_string(tmp.path().join("sw.js")).unwrap();
        assert!(!bypasses(&sw, "/llms.txt"), "dev={dev_enabled}");
        assert!(!rules.exact.iter().any(|p| p == "/llms.txt"), "{rules:?}");
        assert!(
            !rules
                .prefixes
                .iter()
                .any(|p| "/llms.txt".starts_with(p.as_str())),
            "{rules:?}"
        );
    }
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    for entry in fs::read_dir(src).unwrap() {
        let e = entry.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            fs::create_dir_all(&to).unwrap();
            copy_dir(&e.path(), &to);
        } else {
            fs::copy(e.path(), to).unwrap();
        }
    }
}
