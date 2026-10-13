//! The sealed × web flow builds a bundle only around a runtime compiled as
//! the bundle's `[sandbox] mode`.
//!
//! The flow bundles `impresspress-web`: the plain build baked into the CLI,
//! or the wasm-pack output `IMPRESSPRESS_WEB_PKG_DIR` names. A site or a
//! workspace needs the latter — a runtime built with `browser-site-runtime`
//! or `browser-devtools` — and the bundler refuses any other pairing, so the
//! tests here hand each build a fake pkg directory of the right mode, or of
//! the wrong one on purpose.
//!
//! `IMPRESSPRESS_WEB_PKG_DIR` is process-global, so this is its own test
//! binary with ONE `#[tokio::test]` that runs the scenarios in sequence — the
//! same reason `flow_sealed_web_snippets.rs` is one.

mod support;

use std::{fs, path::Path};

use impresspress::cli::{flows::sealed_web, helpers::wasm::PKG_DIR_ENV};
use impresspress_bundle::bundle::SandboxShell;
use support::fake_web_pkg;
use tempfile::tempdir;

/// Every scenario, in sequence. See the module docs for why they are one
/// test.
#[tokio::test]
async fn sealed_web_builds_each_sandbox_mode_only_around_its_own_runtime() {
    let pkgs: Vec<tempfile::TempDir> = [
        SandboxShell::None,
        SandboxShell::Site,
        SandboxShell::Workspace,
    ]
    .into_iter()
    .map(|mode| {
        let dir = tempdir().unwrap();
        fake_web_pkg(dir.path(), mode);
        dir
    })
    .collect();
    let [none_pkg, site_pkg, workspace_pkg] = &pkgs[..] else {
        unreachable!()
    };
    the_dev_sandbox_configuration_emits_a_static_llms_txt_and_a_readable_boot_page(
        site_pkg.path(),
        workspace_pkg.path(),
    )
    .await;
    a_site_is_not_built_around_a_runtime_with_no_sandbox(none_pkg.path()).await;
    a_site_is_not_built_around_the_baked_runtime().await;
}

/// The export shell's own configuration (`[sandbox] mode = "site"`) around
/// a runtime compiled with no sandbox: refused, naming both modes and how
/// this flow gets a site runtime, rather than shipping a worker that stops
/// before `initialize()`.
async fn a_site_is_not_built_around_a_runtime_with_no_sandbox(none_pkg: &Path) {
    let app = site_app();
    std::env::set_var(PKG_DIR_ENV, none_pkg);
    let built = sealed_web::build(app.path(), false).await;
    std::env::remove_var(PKG_DIR_ENV);
    let message = format!(
        "{:#}",
        built.expect_err("a site around a runtime with no sandbox must be refused")
    );
    assert!(
        message
            .contains("was compiled as sandbox mode 'none', but this bundle is built for 'site'"),
        "{message}"
    );
    assert!(
        message.contains(PKG_DIR_ENV) && message.contains("--features browser-site-runtime"),
        "the sealed flow says how to get a site runtime: {message}"
    );
    assert!(!app.path().join("dist/sw.js").exists());
}

/// The same configuration with no override: the runtime baked into the CLI
/// is the plain build (`wasm_bundling.rs` holds it to `none`), so a site is
/// refused around it too.
async fn a_site_is_not_built_around_the_baked_runtime() {
    let app = site_app();
    std::env::remove_var(PKG_DIR_ENV);
    let message = format!(
        "{:#}",
        sealed_web::build(app.path(), false)
            .await
            .expect_err("a site around the baked runtime must be refused")
    );
    assert!(
        message
            .contains("was compiled as sandbox mode 'none', but this bundle is built for 'site'"),
        "{message}"
    );
}

/// A directory holding the dev sandbox's export-shell configuration: a site.
fn site_app() -> tempfile::TempDir {
    let app = tempdir().unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/dev-sandbox/export-shell/impresspress.toml"),
        app.path().join("impresspress.toml"),
    )
    .unwrap();
    app
}

/// The dev sandbox's own configuration, built the way `examples/dev-sandbox/
/// build.sh` builds it: its `impresspress.toml` and boot notice, over a staged
/// `seed/`, the staged title of the seed being built, a compiler tree, and
/// the export shell built from its own configuration first.
///
/// What a reader with no service worker gets from that deployment is two
/// static files, and both are this configuration's doing rather than the
/// bundler's: `/llms.txt` (an overlay of the staged seed's) and a boot page
/// that says what the sandbox is (`[app] boot_notice`) under the seed's own
/// title (`[app] title_file`). A plain build has neither
/// (`flow_sealed_web.rs`, `a_plain_build_has_no_llms_txt_and_no_boot_notice`).
///
/// Built once per committed seed, each with the title its `sandbox.json`
/// gives: the configuration is shared, so a title written in it would head
/// every seed's boot page alike. Each half around a runtime of its own mode,
/// as `build.sh` builds them: the export shell around a site runtime, the
/// sandbox around a workspace one.
async fn the_dev_sandbox_configuration_emits_a_static_llms_txt_and_a_readable_boot_page(
    site_pkg: &Path,
    workspace_pkg: &Path,
) {
    let sandbox =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/dev-sandbox");
    let config = fs::read_to_string(sandbox.join("impresspress.toml")).unwrap();
    // The title has one source. `title_file` is the only way this
    // configuration gets one, and no seed's title is spelled in it.
    let parsed = impresspress::cli::config::parse(&config).unwrap();
    // The development sandbox itself: the workspace half on top of the
    // runtime one.
    assert_eq!(
        parsed.sandbox.mode,
        impresspress_bundle::bundle::SandboxShell::Workspace
    );
    // …which ships the shell its export is for, built beside it.
    assert_eq!(
        parsed.sandbox.export_shell.as_deref(),
        Some("export-shell/dist")
    );
    let impresspress::cli::config::AppTitle::File(title_file) = parsed.app.title else {
        panic!("[app] title is written inline: {:?}", parsed.app.title);
    };

    let mut titles = Vec::new();
    for seed in ["blank", "bootstrap"] {
        let seed_json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(sandbox.join("seeds").join(seed).join("sandbox.json")).unwrap(),
        )
        .unwrap();
        let title = seed_json["title"]
            .as_str()
            .unwrap_or_else(|| panic!("seeds/{seed}/sandbox.json: no title"))
            .to_string();
        assert!(
            !config.contains(&title),
            "impresspress.toml restates the {seed} seed's title"
        );

        let tmp = tempdir().unwrap();
        for file in ["impresspress.toml", "boot-notice.html"] {
            fs::copy(sandbox.join(file), tmp.path().join(file)).unwrap();
        }
        // What build.sh stages and builds before the CLI runs.
        let llms = "# ImpressPress build sandbox\n\nBuild a website here.\n";
        fs::create_dir_all(tmp.path().join("seed/site")).unwrap();
        fs::write(tmp.path().join("seed/manifest.json"), "{}").unwrap();
        fs::write(tmp.path().join("seed/llms.txt"), llms).unwrap();
        fs::write(tmp.path().join(&title_file), format!("{title}\n")).unwrap();
        fs::create_dir_all(tmp.path().join("compiler/dist")).unwrap();
        fs::write(tmp.path().join("compiler/dist/manifest.json"), "{}").unwrap();
        // The export shell, built first as build.sh builds it: its own
        // configuration, through the same flow, into `export-shell/dist/`.
        let shell = tmp.path().join("export-shell");
        fs::create_dir_all(&shell).unwrap();
        fs::copy(
            sandbox.join("export-shell/impresspress.toml"),
            shell.join("impresspress.toml"),
        )
        .unwrap();
        std::env::set_var(PKG_DIR_ENV, site_pkg);
        let built = sealed_web::build(&shell, false).await;
        std::env::remove_var(PKG_DIR_ENV);
        built.unwrap();
        let shell_dist = shell.join("dist");

        std::env::set_var(PKG_DIR_ENV, workspace_pkg);
        let built = sealed_web::build(tmp.path(), false).await;
        std::env::remove_var(PKG_DIR_ENV);
        built.unwrap();
        let dist = tmp.path().join("dist");

        // The static copy, and the one the seed importer reads.
        assert_eq!(fs::read_to_string(dist.join("llms.txt")).unwrap(), llms);
        assert_eq!(
            fs::read_to_string(dist.join("seed/llms.txt")).unwrap(),
            llms
        );

        // The boot page carries the sandbox's notice, verbatim, in its HTML.
        let index = fs::read_to_string(dist.join("index.html")).unwrap();
        let notice = fs::read_to_string(sandbox.join("boot-notice.html")).unwrap();
        assert!(index.contains(&notice), "{index}");
        for needle in [
            r#"href="/llms.txt""#,
            r#"href="/b/dev/enter""#,
            "<noscript>",
        ] {
            assert!(
                index.contains(needle),
                "the boot page lacks {needle}: {index}"
            );
        }

        // …under the seed's own title: the document's, and the heading's.
        // (Neither committed title has a character the bundler escapes.)
        assert!(!title.contains(['&', '<', '>']), "{title}");
        assert!(
            index.contains(&format!("<title>{title}</title>")),
            "the {seed} boot page is not titled {title:?}: {index}"
        );
        assert!(
            index.contains(&format!("<h1><span data-app-title>{title}</span></h1>")),
            "the {seed} boot page is not headed {title:?}: {index}"
        );
        // The title file is the build's input, not something it ships.
        assert!(!dist.join(&title_file).exists());

        // `/llms.txt` stays the runtime's once the worker controls the page:
        // it is not a bypass rule, so a site's own `site/llms.txt` is never
        // shadowed by the static file above. And it is not part of the shell
        // an export copies.
        let sw = fs::read_to_string(dist.join("sw.js")).unwrap();
        assert!(!sw.contains("llms.txt"), "sw.js names llms.txt");
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dist.join("asset-manifest.json")).unwrap())
                .unwrap();
        // `[sandbox] mode = "workspace"` reaches the shipped worker as its
        // one constant, and the seed and compiler trees it serves from the
        // static host are rules the worker and the manifest both state.
        assert_eq!(
            sw.matches("const SANDBOX = 'workspace';").count(),
            1,
            "{sw}"
        );
        let bypass = &manifest["bypass"]["prefixes"];
        for prefix in ["/seed/", "/__impresspress_dev/compiler/"] {
            assert!(
                bypass.as_array().unwrap().iter().any(|p| p == prefix),
                "{prefix} is not in the manifest's bypass: {manifest}"
            );
        }
        assert!(
            !manifest["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f.as_str() == Some("llms.txt")),
            "{manifest}"
        );

        // The export shell is a site's: its worker boots the site runtime,
        // and nothing in it names the sandbox's own trees.
        let shell_sw = fs::read_to_string(shell_dist.join("sw.js")).unwrap();
        assert_eq!(
            shell_sw.matches("const SANDBOX = 'site';").count(),
            1,
            "{shell_sw}"
        );
        assert!(!shell_sw.contains("/__impresspress_dev/"), "{shell_sw}");
        assert!(
            shell_sw.contains("const EXPORT_SHELL = null;"),
            "{shell_sw}"
        );
        // It is laid down whole at the bundler's root, byte for byte…
        let root = impresspress_bundle::bundle::EXPORT_SHELL_ROOT;
        let laid = dist.join(root.trim_matches('/'));
        let shell_files =
            impresspress_bundle::bundle::manifest::list_dist_files(&shell_dist).unwrap();
        assert!(shell_files.iter().any(|f| f == "sw.js"), "{shell_files:?}");
        assert_eq!(
            impresspress_bundle::bundle::manifest::list_dist_files(&laid).unwrap(),
            shell_files
        );
        for file in &shell_files {
            assert_eq!(
                fs::read(laid.join(file)).unwrap(),
                fs::read(shell_dist.join(file)).unwrap(),
                "{file}"
            );
        }
        // …the sandbox's worker hands its runtime that root…
        assert_eq!(
            sw.matches(&format!(
                "const EXPORT_SHELL = {};",
                serde_json::to_string(root).unwrap()
            ))
            .count(),
            1,
            "{sw}"
        );
        // …and none of its files is listed as the sandbox's own, nor is the
        // root a path the sandbox's worker leaves to the static host.
        let root_relative = root.trim_start_matches('/');
        assert!(
            !manifest["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f.as_str().unwrap().starts_with(root_relative)),
            "{manifest}"
        );
        assert!(
            !bypass
                .as_array()
                .unwrap()
                .iter()
                .any(|p| root.starts_with(p.as_str().unwrap())),
            "{manifest}"
        );
        titles.push(title);
    }
    // Two seeds, two boot pages a reader can tell apart — and the one built
    // from a template says which.
    assert_ne!(titles[0], titles[1]);
    assert!(titles[1].contains("Bootstrap"), "{}", titles[1]);
}
