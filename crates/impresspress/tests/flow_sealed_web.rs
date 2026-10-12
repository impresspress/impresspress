use std::fs;

use impresspress::cli::flows::sealed_web;
use tempfile::tempdir;

#[tokio::test]
async fn build_emits_dist_with_wasm_and_index() {
    let tmp = tempdir().unwrap();
    sealed_web::build(tmp.path(), false).await.unwrap();

    let dist = tmp.path().join("dist");
    assert!(dist.is_dir(), "expected dist/ to be created");

    let wasm_files: Vec<_> = fs::read_dir(&dist)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("wasm"))
        .collect();
    assert!(
        !wasm_files.is_empty(),
        "expected at least one .wasm in dist/"
    );

    assert!(
        dist.join("index.html").is_file(),
        "expected dist/index.html"
    );
}

/// A present-but-malformed `impresspress.toml` is refused, not treated as
/// "no config" (which would bundle with default app name/title/redirect and
/// skip the operator's overlays without a word).
#[tokio::test]
async fn build_refuses_malformed_impresspress_toml() {
    let tmp = tempdir().unwrap();
    fs::write(tmp.path().join("impresspress.toml"), "[app\n").unwrap();

    let err = sealed_web::build(tmp.path(), false)
        .await
        .expect_err("a malformed impresspress.toml must fail the build");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parse") && msg.contains("impresspress.toml"),
        "{msg}"
    );
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
/// title (`[app] title_file`). A plain build — the test below — has neither.
///
/// Built once per committed seed, each with the title its `sandbox.json`
/// gives: the configuration is shared, so a title written in it would head
/// every seed's boot page alike.
#[tokio::test]
async fn the_dev_sandbox_configuration_emits_a_static_llms_txt_and_a_readable_boot_page() {
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
        sealed_web::build(&shell, false).await.unwrap();
        let shell_dist = shell.join("dist");

        sealed_web::build(tmp.path(), false).await.unwrap();
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

/// The sandbox's configuration with no staged title is not built with a
/// title from somewhere else: the build fails and names the file.
#[tokio::test]
async fn the_dev_sandbox_configuration_does_not_build_without_its_seeds_title() {
    let sandbox =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/dev-sandbox");
    let tmp = tempdir().unwrap();
    for file in ["impresspress.toml", "boot-notice.html"] {
        fs::copy(sandbox.join(file), tmp.path().join(file)).unwrap();
    }
    let err = sealed_web::build(tmp.path(), false)
        .await
        .expect_err("a sandbox build with no staged title must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("title_file") && msg.contains("boot-title.txt"),
        "{msg}"
    );
}

/// A build with no configuration is an ordinary app: no `llms.txt`, and a
/// boot page with nothing between its heading and its status line.
#[tokio::test]
async fn a_plain_build_has_no_llms_txt_and_no_boot_notice() {
    let tmp = tempdir().unwrap();
    sealed_web::build(tmp.path(), false).await.unwrap();
    let dist = tmp.path().join("dist");
    assert!(!dist.join("llms.txt").exists());
    let index = fs::read_to_string(dist.join("index.html")).unwrap();
    let after_heading = &index[index.find("</h1>").expect("the heading") + "</h1>".len()..];
    let before_status = &after_heading[..after_heading
        .find(r#"<p id="status">"#)
        .expect("status line")];
    assert_eq!(before_status.trim(), "", "{index}");
    assert!(!index.contains("llms.txt"), "{index}");
}
