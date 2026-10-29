//! `GET /b/dev` — the workspace document, and the two assets it pulls.
//!
//! The page is the human half of the sandbox. The agent works through
//! `/b/dev/api/*` and the tools projected from it; this document is what lets
//! the human see what the agent did, edit a file themselves, watch an
//! activation land, and read the live site — plus the "how this works"
//! section and, when the seed suggested one, the prompt that gets a
//! first-time visitor started.
//!
//! # Why it builds its own response
//!
//! Every other page in the crate is a [`crate::ui::shell_page`] call. This one
//! needs three headers that page cannot carry:
//!
//! * `Cross-Origin-Opener-Policy: same-origin` and
//!   `Cross-Origin-Embedder-Policy: credentialless` — together they make the
//!   document cross-origin isolated, which is what `SharedArrayBuffer` (and
//!   so the in-browser Rust compiler) requires. This document needs them
//!   whatever the deployment's policy is, so it sets them itself. Note that a
//!   COEP document can only embed nested documents that ALSO carry a
//!   compatible COEP (the HTML spec's navigation-response adherence check is
//!   origin-independent), so the preview iframe of `/` only renders when the
//!   deployment serves the site with COEP too: the browser runtime sets the
//!   security-headers block's `cross_origin_isolation` to `credentialless`
//!   when the sandbox is active (design §20, amendment 14). `credentialless`
//!   rather than `require-corp` so an agent-built site can still show a
//!   cross-origin image without CORP on the third party.
//! * `Cache-Control: no-store` — the block-wide rule (design §12).
//!
//! An `OutputStream`'s response meta is fixed when the stream is built, so
//! the headers have to be on the response as it is constructed. Hence
//! [`crate::ui::shell_document`], which renders the shelled markup and leaves
//! the response to the caller.

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use super::{
    assets, no_store,
    repo::seed_info::{self, SeedInfo},
};
use crate::{
    http::ResponseBuilder,
    ui,
    ui::components::{Action, ActionTarget, Card},
};

/// Serve the workspace document.
pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let seed = match seed_info::read(ctx).await {
        Ok(seed) => seed,
        Err(e) => {
            return super::no_store_db_error_internal(e, "workspace page: seed info read failed")
        }
    };
    // The same read the entry page signs in with (`enter.rs`), so the guide
    // can never print credentials other than the ones this instance has.
    let credentials = match super::enter::bootstrap_credentials(ctx).await {
        Ok(credentials) => credentials,
        Err(e) => {
            return super::no_store_db_error_internal(
                e,
                "workspace page: admin credentials read failed",
            )
        }
    };
    let credentials = credentials
        .as_ref()
        .map(|(email, password)| (email.as_str(), password.as_str()));
    let shell = ui::Shell::admin("Workspace", "Workspace");
    let markup = match ui::shell_document(ctx, msg, shell, body(seed.as_ref(), credentials)).await {
        Ok(markup) => markup,
        Err(e) => {
            return super::no_store_db_error_internal(e, "workspace page: site config read failed")
        }
    };
    no_store()
        .set_header("Cross-Origin-Opener-Policy", "same-origin")
        .set_header("Cross-Origin-Embedder-Policy", "credentialless")
        .body(
            markup.into_string().into_bytes(),
            "text/html; charset=utf-8",
        )
}

/// The page body: seven panes with stable ids, each a [`Card`] on the page
/// background, then the assets that drive them.
///
/// The ids are the contract between this markup, `dev.js` and the end-to-end
/// test — `dev.js` looks every element it reads or writes up by id and does
/// nothing else with the document's shape, so the layout can change without
/// touching it. Its buttons are [`Action`]s with a `dev-*` verb
/// ([`ActionTarget::Delegated`]), which one delegated listener in `dev.js`
/// runs. Save is the page's one primary.
///
/// `seed` is what the seed bundle said about this sandbox — the template
/// and the prompt to suggest. `None` renders no prompt disclosure at all.
///
/// `credentials` is the bootstrap admin's email and password as this instance
/// has them configured (`enter::bootstrap_credentials`). `None` — the password
/// row was cleared — prints none: there is nothing true to print.
fn body(seed: Option<&SeedInfo>, credentials: Option<(&str, &str)>) -> Markup {
    html! {
        div .dev-workspace {
            (Card::titled("How this workspace works", html! {
                p {
                    "This page is a WebMCP workspace. An agent in your browser uses the tools \
                     published here to edit the site under " code { "site/" } ", write Rust \
                     backend blocks under " code { "blocks/<name>/" } ", stock the shop with the "
                    code { "shop_*" } " tools, and export the result."
                }
                // How an agent reaches the tools in THIS browser: through
                // WebMCP, or — where the browser has none — through the Tool
                // console below. Only `dev.js` can tell which, so it writes
                // the sentence; the markup ships it empty rather than
                // promising either.
                p #dev-webmcp-status {}
                ul .dev-guide__facts {
                    li { "Start with " code { "dev_status" } "." }
                    @if let Some(seed) = seed {
                        li {
                            "This sandbox was seeded from the " strong { (seed.template) }
                            " template. " code { "dev_read_reference" } " returns two guides: "
                            code { "markdown" } " for backend blocks and " code { "site_markdown" }
                            " for the site — read the second before writing under "
                            code { "site/" } "."
                        }
                    }
                    li {
                        "Every successful change is live at " a href="/" target="_blank" { "/" }
                        " immediately; " code { "dev_rollback" } " undoes a generation."
                    }
                    li {
                        "Include " code { "<script src=\"/b/webmcp/webmcp.js\" defer></script>" }
                        " in every page you write under " code { "site/" } ", so a visitor's \
                         agent gets the site's public tools too."
                    }
                    @if let Some((email, password)) = credentials {
                        li {
                            "Credentials for this browser-local instance: "
                            code #dev-credentials-email { (email) } " / "
                            code #dev-credentials-password { (password) } "."
                        }
                    }
                }
                @if let Some(seed) = seed {
                    details .dev-disclosure {
                        summary { "Suggested prompt" }
                        pre #dev-suggested-prompt { (seed.suggested_prompt) }
                    }
                }
            }).id("dev-guide").render())
            (Card::titled("Tool console", html! {
                // The same tools, for an agent whose browser has no WebMCP
                // and can only drive the page: pick one, edit its arguments,
                // press Run, read the result. Every control is filled in by
                // `dev.js` from the list it publishes to WebMCP, and Run
                // calls the same function an agent's tool call would — so
                // this pane cannot offer a tool the page does not have, or
                // run one differently.
                p {
                    "Every tool this page publishes, runnable by hand. Choose a tool, edit its \
                     arguments as JSON, and press Run; the result appears below. A tool that \
                     changes the site refreshes this page exactly as an agent's call would."
                }
                // Two columns from 900px: what the tool is (select,
                // description, schema) beside what to call it with (the
                // arguments, Run, the result).
                div .dev-console__grid {
                    div {
                        div .form-group {
                            label .form-label for="dev-console-tool" { "Tool" }
                            select #dev-console-tool .form-select {}
                        }
                        p #dev-console-description {}
                        details .dev-disclosure {
                            summary { "Input schema" }
                            pre #dev-console-schema {}
                        }
                    }
                    div {
                        div .form-group {
                            label .form-label for="dev-console-args" { "Arguments (JSON)" }
                            textarea #dev-console-args spellcheck="false" {}
                        }
                        div .dev-editor-actions {
                            // Ships `disabled`: there is nothing to run until
                            // `dev.js` has fetched the tools. A secondary:
                            // the page's one primary is the editor's Save.
                            (Action::secondary("Run", ActionTarget::Delegated("dev-console-run"))
                                .id("dev-console-run")
                                .disabled(true)
                                .render())
                        }
                        // What a screen reader hears when a run ends: one
                        // short sentence ("Run finished: ok" / "…: error"),
                        // written by `dev.js`. Always rendered — a live
                        // region that only appears with its text is not
                        // reliably announced.
                        div #dev-console-status .sr-only role="status" {}
                        // `output` rather than `pre` because it is labelable,
                        // so the "Result" label names it. `aria-live="off"`
                        // overrides the implicit status role an `output`
                        // carries: the result can be pages of JSON, which is
                        // for reading, not for announcing — the run's outcome
                        // is announced by `#dev-console-status` above. The
                        // block around it stays hidden while it is empty
                        // (dev.css), so no "Result" label stands over nothing.
                        div .dev-console__result {
                            label .form-label for="dev-console-result" { "Result" }
                            output #dev-console-result for="dev-console-args" aria-live="off" {}
                        }
                    }
                }
            }).id("dev-console").render())
            (Card::titled("Files", html! {
                // Each entry is a `button` (`dev.js`'s `loadFiles`): opening
                // a file is an action on this page, not a navigation.
                ul #dev-file-list aria-label="Workspace files" {}
            })
            .action(Action::secondary("New file", ActionTarget::Delegated("dev-new-file")))
            .id("dev-files")
            .render())
            (Card::titled("Editor", html! {
                // The open file's path, which `dev.js` writes here, is the
                // textarea's label: "site/index.html" is what the box holds.
                label #dev-editor-title .dev-editor__path for="dev-editor-text" { "No file open" }
                textarea #dev-editor-text spellcheck="false" {}
                div .dev-editor-actions {
                    (Action::primary("Save", ActionTarget::Delegated("dev-save")).id("dev-save").render())
                    (Action::quiet_danger("Delete", ActionTarget::Delegated("dev-delete")).render())
                }
            }).id("dev-editor").render())
            (Card::titled("Live site", html! {
                // `sandbox` is NOT a prompt-injection boundary, and cannot
                // be one while this frame carries `allow-same-origin` on
                // same-origin content: the framed page can read and write
                // the parent's WebMCP registries (`parent.navigator.modelContext`,
                // `parent.document.modelContext`, including `registerTool`)
                // and `parent.__impresspressWebmcp` (set by `webmcp.js`,
                // which every SSR page — this one included — loads), and
                // per the HTML spec a same-origin frame with both
                // `allow-scripts` and `allow-same-origin` can drop its own
                // `sandbox` attribute and re-navigate itself out of the
                // sandbox altogether. `allow-same-origin` is unavoidable
                // without a distinct preview origin: without it the framed
                // site's own `/b/products/*` calls are cross-origin and
                // CORS-blocked, and the storefront widget dies. What this
                // attribute actually buys is narrower — no top-level
                // navigation of the parent, no modals, no downloads, no
                // pointer-lock/presentation — and needs none of the above
                // reasoning to justify: any script on `/` already runs
                // same-origin with the admin's session cookie regardless of
                // this iframe. What the sandbox actually relies on: the
                // framed content IS the admin's own site (design §2 — the
                // `shop_*`/`dev_*` tools exist only on this trusted,
                // browser-local instance) and every tool call is authorized
                // server-side, not the browser tab it happens to render in.
                // A distinct preview origin (or a credentialless frame,
                // which would lose the service worker `webmcp.js` needs) is
                // the real fix and is a tracked follow-up — this attribute
                // does not provide isolation from the parent today.
                iframe #dev-preview-frame
                    src="/"
                    sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
                    title="Live site" {}
            }).id("dev-preview").render())
            (Card::titled("Progress", html! {
                ol #dev-progress-steps {}
                // Focusable so its scroll can be driven from the keyboard;
                // `role="log"` because new lines are appended at the end and
                // only those are news.
                pre #dev-log role="log" aria-label="Activity log" tabindex="0" {}
            }).id("dev-progress").render())
            (Card::titled("Build and export", html! {
                // Both ship `disabled`, and that is the honest default: this
                // markup is the same in every build, and whether a build
                // carries the browser toolchain is a property of the BUNDLE
                // (`examples/dev-sandbox/impresspress.toml` overlays
                // `compiler/dist/` onto `/__impresspress_dev/compiler/`),
                // which no Rust code here can see. So `dev.js` fetches the
                // compiler's manifest on load and enables Compile only when
                // this deployment actually has one — a button that starts
                // enabled would be a promise the page cannot keep on a build
                // without the overlay. Export ships disabled for a
                // DIFFERENT reason with the same shape: an export is a
                // snapshot of the live generation, and a fresh instance has
                // none — `dev.js`'s `renderStatus` enables it the moment one
                // exists, and `GET /b/dev/api/export` answers 400 until then.

                // Which block Compile acts on. It ships EMPTY, and stays
                // empty on a workspace with no blocks: the options are the
                // `blocks/<name>/` prefixes in the file listing (`dev.js`'s
                // `renderBlockChoices`, refreshed with the file pane), and
                // a block only exists as files under such a prefix — there
                // is no block record until one compiles, so there is
                // nothing this markup could have known to pre-render. The
                // label is on the control rather than beside it because the
                // pane is a row of buttons with no space for one, and a
                // select whose only clue is its first option is
                // unreachable by anything that is not looking at it.
                div .dev-actions {
                    select #dev-compile-block .form-select aria-label="Block to compile" {}
                    (Action::secondary("Compile block", ActionTarget::Delegated("dev-compile"))
                        .id("dev-compile")
                        .disabled(true)
                        .render())
                    (Action::secondary("Export", ActionTarget::Delegated("dev-export"))
                        .id("dev-export")
                        .disabled(true)
                        .render())
                    (Action::secondary("Refresh tools", ActionTarget::Delegated("dev-refresh-tools")).render())
                    // Filled in by `dev.js` from the compiler manifest, and
                    // left empty when there is none — the version is the
                    // pinned rubrc sha every compiler URL carries, so it is
                    // what a bug report about a build needs to quote.
                    span #dev-compiler-version {}
                }
            }).id("dev-actions").render())
        }
        link rel="stylesheet" href="/b/dev/static/dev.css";
        // `type="module"`, not `defer`: the script imports
        // `BrowserRustCompiler` from `/b/dev/static/compiler-adapter.js`, and
        // an `import` only resolves in a module. A module script defers by
        // itself — it runs after parsing, in document order with the deferred
        // classic scripts — so `dev.js` still runs before the `webmcp.js`
        // that `ui/layout.rs` puts at the end of the body, which is the
        // ordering the tail's `refreshSiteTools` guard is written for.
        script type="module" src="/b/dev/static/dev.js" {}
    }
}

/// Serve `/b/dev/static/dev.js`.
pub fn handle_script(msg: &Message) -> OutputStream {
    asset(
        msg,
        assets::dev_js().as_bytes(),
        "application/javascript; charset=utf-8",
        assets::dev_js_hash(),
    )
}

/// Serve `/b/dev/static/dev.css`.
pub fn handle_stylesheet(msg: &Message) -> OutputStream {
    asset(
        msg,
        assets::dev_css().as_bytes(),
        "text/css; charset=utf-8",
        assets::dev_css_hash(),
    )
}

/// Serve `/b/dev/static/compiler-adapter.js`.
///
/// Its own URL rather than bytes folded into `dev.js` because it is a
/// separate ES module: `dev.js` imports it by this path, and the browser
/// fetches it as a module of its own. Same tier, same headers, same
/// `no-cache` reasoning as the other two — it is one more file whose content
/// is fixed for a build and carries no hash to prove it.
pub fn handle_compiler_adapter(msg: &Message) -> OutputStream {
    asset(
        msg,
        assets::compiler_adapter_js().as_bytes(),
        "application/javascript; charset=utf-8",
        assets::compiler_adapter_js_hash(),
    )
}

/// The three page assets are the block's only responses that are not
/// `no-store`.
///
/// `no-cache` still means "revalidate before every use", so a rebuilt bundle
/// is never served from a stale cache — these URLs carry no content hash, so
/// the immutable, year-long `Cache-Control` the hashed `/b/static/*` bundle
/// uses would be wrong. What it buys over `no-store` is a 304 — via
/// `http::conditional::not_modified`, the same comparison the
/// `/b/webmcp/webmcp.js` stable route uses — instead of a re-download of the
/// whole script on every navigation to the page, which is the difference
/// §12's rule was never about: the rule exists so a *stale answer about the
/// workspace* cannot outlive the generation that changed it, and these bytes
/// describe no generation at all.
fn asset(msg: &Message, bytes: &'static [u8], content_type: &str, hash: &str) -> OutputStream {
    let etag = format!("\"{hash}\"");
    if let Some(not_modified) = crate::http::conditional::not_modified(msg, &etag, "no-cache") {
        return not_modified;
    }
    ResponseBuilder::new()
        .set_header("Cache-Control", "no-cache")
        .set_header("ETag", &etag)
        .set_header("X-Content-Type-Options", "nosniff")
        .body(bytes.to_vec(), content_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids `dev.js` looks up must exist in the markup it is served with.
    /// The two halves ship in the same crate and are only ever deployed
    /// together, so a rename that hits one and not the other is a defect this
    /// test can see without a browser.
    #[test]
    fn every_id_dev_js_looks_up_is_in_the_document() {
        let html = body(None, None).into_string();
        for id in [
            "dev-log",
            "dev-progress-steps",
            "dev-preview-frame",
            "dev-file-list",
            "dev-editor-text",
            "dev-editor-title",
            "dev-save",
            "dev-compile",
            "dev-compile-block",
            "dev-compiler-version",
            "dev-webmcp-status",
            "dev-console-tool",
            "dev-console-description",
            "dev-console-schema",
            "dev-console-args",
            "dev-console-run",
            "dev-console-result",
            "dev-console-status",
        ] {
            assert!(
                assets::dev_js().contains(&format!("'{id}'")),
                "{id} is not looked up by dev.js — drop it or wire it"
            );
            assert!(
                html.contains(&format!("id=\"{id}\"")),
                "{id} missing from the document"
            );
        }
    }

    /// Every form control in the document has an accessible name, and every
    /// `<label for>` names an element a label can actually label.
    ///
    /// A label whose `for` names a non-labelable element (a `<pre>`, say)
    /// labels nothing and is announced as stray text, and an unlabelled
    /// textarea is announced as "edit text" with no hint of what it holds.
    #[test]
    fn every_control_is_labelled_and_every_label_names_a_control() {
        let seed = SeedInfo {
            template: "blank".into(),
            suggested_prompt: "Build a shop.".into(),
            guide_markdown: String::new(),
            llms_text: None,
        };
        let html = body(Some(&seed), Some(("a@example.com", "pw"))).into_string();
        let element = regex::Regex::new(r#"<(\w+)\b([^>]*)>"#).unwrap();
        let attr = |attrs: &str, name: &str| {
            regex::Regex::new(&format!(r#"\b{name}="([^"]*)""#))
                .unwrap()
                .captures(attrs)
                .map(|c| c[1].to_string())
        };
        let mut tags_by_id = std::collections::HashMap::new();
        let mut label_targets = Vec::new();
        let mut controls = Vec::new();
        for c in element.captures_iter(&html) {
            let (tag, attrs) = (c[1].to_string(), c[2].to_string());
            if let Some(id) = attr(&attrs, "id") {
                tags_by_id.insert(id, tag.clone());
            }
            if tag == "label" {
                label_targets.push(attr(&attrs, "for").expect("a label in this page uses `for`"));
            }
            if matches!(tag.as_str(), "input" | "select" | "textarea" | "output") {
                controls.push((tag, attrs));
            }
        }
        for target in &label_targets {
            let tag = tags_by_id
                .get(target)
                .unwrap_or_else(|| panic!("label for=\"{target}\" names no element"));
            assert!(
                matches!(
                    tag.as_str(),
                    "input" | "select" | "textarea" | "output" | "button" | "meter" | "progress"
                ),
                "label for=\"{target}\" names a <{tag}>, which a label cannot label"
            );
        }
        assert!(
            controls.len() >= 5,
            "only {} controls found",
            controls.len()
        );
        for (tag, attrs) in &controls {
            let id = attr(attrs, "id").unwrap_or_default();
            assert!(
                label_targets.contains(&id)
                    || attr(attrs, "aria-label").is_some()
                    || attr(attrs, "aria-labelledby").is_some(),
                "<{tag} id=\"{id}\"> has no label"
            );
        }
        // The editor's label is the open file's path, which `dev.js` writes
        // into `#dev-editor-title`.
        assert!(
            html.contains(
                r#"<label class="dev-editor__path" id="dev-editor-title" for="dev-editor-text">"#
            ),
            "{html}"
        );
        assert!(assets::dev_js().contains("title.textContent = file.path;"));
    }

    /// Each pane is a titled `components::Card` with the id `dev.js` and the
    /// specs find it by, so every in-body heading is the card's h2; no pane
    /// holds another card-level surface; Save is the page's one primary
    /// (Run, the console's, is a secondary); and the selects are the shared
    /// `.form-select`, which carries the target floor.
    #[test]
    fn panes_are_cards_under_one_primary_and_use_the_shared_form_controls() {
        let html = body(None, None).into_string();
        for (id, title) in [
            ("dev-guide", "How this workspace works"),
            ("dev-console", "Tool console"),
            ("dev-files", "Files"),
            ("dev-editor", "Editor"),
            ("dev-preview", "Live site"),
            ("dev-progress", "Progress"),
            ("dev-actions", "Build and export"),
        ] {
            assert!(
                html.contains(&format!(
                    r#"<section class="card" id="{id}"><header class="card__head"><div><h2 class="card__title">{title}</h2>"#
                )),
                "{title} is not a titled card with id {id}; {html}"
            );
        }
        assert!(!html.contains("dev-pane"), "{html}");
        assert_eq!(
            crate::test_support::dom::visible_primaries(&html).0,
            vec![r#"button.btn.btn--primary "Save""#.to_string()],
            "{html}"
        );
        assert!(
            crate::test_support::dom::card_level_inside_card(&html).is_empty(),
            "{html}"
        );
        assert!(!html.contains("<h2>"), "a bare h2 is left; {html}");
        assert!(
            html.contains(r#"<select class="form-select" id="dev-console-tool">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<select class="form-select" id="dev-compile-block""#),
            "{html}"
        );
    }

    /// Files open from buttons, not `href="#"` links: opening one swaps the
    /// editor's contents and navigates nowhere.
    #[test]
    fn files_open_from_buttons() {
        let js = assets::dev_js();
        assert!(js.contains("var button = document.createElement('button');"));
        assert!(js.contains("button.setAttribute('aria-current', 'true');"));
        assert!(!js.contains("link.href = '#';"));
    }

    /// The guide prints the credentials it is GIVEN — the configured ones —
    /// and none when the instance has none configured.
    #[test]
    fn the_guide_prints_the_configured_credentials_and_no_others() {
        let html = body(None, Some(("owner@example.com", "a-seeded-password"))).into_string();
        assert!(
            html.contains(r#"<code id="dev-credentials-email">owner@example.com</code>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<code id="dev-credentials-password">a-seeded-password</code>"#),
            "{html}"
        );
        let bare = body(None, None).into_string();
        assert!(!bare.contains("dev-credentials"), "{bare}");
        assert!(!bare.contains("Credentials for"), "{bare}");
        // No literal credential is left in the markup's own source.
        for html in [&html, &bare] {
            assert!(!html.contains("admin123"), "{html}");
            assert!(!html.contains("admin@example.com"), "{html}");
        }
    }

    /// The Tool console is a pane of its own with the id an agent is told to
    /// look for, and it runs the page's tools through the one list WebMCP is
    /// handed — not through a request builder of its own.
    #[test]
    fn the_tool_console_runs_the_tools_webmcp_is_handed() {
        let html = body(None, None).into_string();
        assert!(
            html.contains(r#"<section class="card" id="dev-console">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<h2 class="card__title">Tool console</h2>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<button class="btn btn--secondary" id="dev-console-run" type="button" data-action="dev-console-run" disabled>"#
            ),
            "Run must ship disabled until the tools are loaded; {html}"
        );
        let js = assets::dev_js();
        // One publication point feeds both consumers…
        assert_eq!(
            js.matches("pageTools.push(options);").count(),
            1,
            "the console's list must be filled in exactly one place"
        );
        assert_eq!(
            js.matches("webmcp.register(options)").count(),
            1,
            "and WebMCP must be handed tools from that same place"
        );
        // Every registry the browser provides is reached through the one
        // helper in webmcp-core.js: no call site asks a registry directly.
        assert_eq!(
            js.matches(".registerTool(").count(),
            1,
            "only `webmcpRegistrar` calls a registry's registerTool"
        );
        assert!(js.contains("[navigator.modelContext, document.modelContext]"));
        // …and the console calls the published `execute`, building no
        // request itself: `buildRequest` is called once, in `toolOptions`
        // (webmcp-core.js).
        assert!(js.contains("await tool.execute(args)"));
        assert_eq!(
            js.matches("buildRequest(").count(),
            2,
            "definition + one call"
        );
        // What the guide says without WebMCP is the sentence an agent reads.
        assert!(js.contains(
            "'This browser has no WebMCP: use the Tool console below, or the file editor.'"
        ));
    }

    /// Saving a binary file's placeholder over the file itself is the one
    /// data-loss path this pane has: the box shows
    /// `(binary file, N bytes)`, the stored `expected_sha256` still matches,
    /// so the write succeeds, destroys the file and publishes a generation
    /// for it. Both halves of the guard are pinned here — the early return
    /// and the button that moves with the box — because the behaviour itself
    /// can only be driven by the Task 6 end-to-end test, and a source
    /// assertion that fails loudly on a rewrite is worth more than nothing
    /// until then.
    #[test]
    fn a_binary_file_cannot_be_saved_over() {
        let js = assets::dev_js();
        assert!(
            js.contains("if (!current || text.disabled) {"),
            "save() must refuse a disabled editor, not just a missing file"
        );
        assert!(
            js.contains("saveButton.disabled = !enabled;"),
            "the Save button must be disabled with the editor"
        );
        assert!(
            js.contains("setEditorEnabled(file.encoding === 'utf8');"),
            "opening a file is what decides whether the editor is usable"
        );
    }

    /// The script tag is a module, and is not `defer`.
    ///
    /// `assets::dev_js()` opens with an `import` of the compiler adapter,
    /// which a classic script cannot parse: a `defer` here — the shape this
    /// tag had before the adapter existed — makes the browser drop the whole
    /// script with nothing but a console message, and every pane on the page
    /// stays empty. The two attributes are also mutually exclusive on a
    /// module (a module defers by itself, and `defer` on it is ignored), so
    /// finding both would mean somebody added one back without reading why
    /// the other is there.
    #[test]
    fn the_script_tag_is_a_module() {
        let html = body(None, None).into_string();
        assert!(
            html.contains(r#"<script type="module" src="/b/dev/static/dev.js">"#),
            "{html}"
        );
        assert!(
            !html.contains("dev.js\" defer"),
            "a module script must not also be marked defer"
        );
        assert!(
            assets::dev_js().starts_with("import "),
            "the composed script must be the module this tag promises"
        );
    }

    /// The Compile button ships disabled, and two facts have to arrive
    /// before it turns on.
    ///
    /// The markup is identical in every build; whether the browser toolchain
    /// is present is a property of the bundle's asset overlay, which nothing
    /// in this crate can see. So an enabled-by-default button would be a
    /// promise a build without the overlay cannot keep — and the 404 path has
    /// to say so on the button rather than fail on click. The second fact is
    /// the workspace's: a block only exists as files under `blocks/<name>/`,
    /// so a fresh instance has nothing to compile and the button says which
    /// half is missing rather than offering a click it can only answer with
    /// an alert. All of it is pinned here because the behaviour itself needs
    /// a browser (`dev-workspace.spec.ts`, `dev-compile-tool.spec.ts`) and a
    /// source assertion that fails loudly on a rewrite is worth more than
    /// nothing in between.
    #[test]
    fn the_compile_button_is_enabled_only_by_a_toolchain_and_a_block() {
        let html = body(None, None).into_string();
        assert!(
            html.contains(
                r#"<button class="btn btn--secondary" id="dev-compile" type="button" data-action="dev-compile" disabled>"#
            ),
            "the Compile button must ship disabled; {html}"
        );
        let js = assets::dev_js();
        assert!(
            js.contains("'/__impresspress_dev/compiler/manifest.json'"),
            "dev.js must discover the compiler at the path the bundle overlays it to"
        );
        assert!(
            js.contains("{ cache: 'no-store' }"),
            "the manifest is the one compiler file whose URL carries no version, so it \
             must not be served from a cache"
        );
        assert!(
            js.contains("compileButton.disabled = false;"),
            "something must enable the button once a manifest is found"
        );
        assert!(
            js.contains("compileButton.title = 'No compiler in this build';"),
            "a build with no compiler must say so on the button"
        );
        assert!(
            js.contains("if (!blockNames.length) {"),
            "a workspace with no blocks must leave the button disabled too"
        );
        // One owner for the button's state. The two facts arrive from
        // separate fetches in no fixed order — and a third, whether a compile
        // is running, arrives from the compile itself — so a second place
        // setting `disabled` would race and whichever landed later would win
        // regardless of what it knew.
        assert!(
            js.contains("if (compileInFlight) {"),
            "a compile in flight must leave the button disabled too"
        );
        assert_eq!(
            js.matches("compileButton.disabled =").count(),
            4,
            "`disabled` must be set only by updateCompileButton's four arms"
        );
    }

    /// Every button on the page declares a `dev-*` verb, and the verbs the
    /// markup writes are exactly the ones `dev.js`'s one delegated listener
    /// runs (`DEV_ACTIONS`): a verb with no runner is a dead button, and a
    /// runner with no button is a control the page lost.
    #[test]
    fn every_button_verb_is_one_dev_js_runs() {
        let html = body(None, None).into_string();
        let verb = regex::Regex::new(r#"data-action="(dev-[a-z-]+)""#).unwrap();
        let mut in_markup: Vec<String> = verb
            .captures_iter(&html)
            .map(|c| c[1].to_string())
            .collect();
        in_markup.sort();
        let js = assets::dev_js();
        let table = &js[js
            .find("var DEV_ACTIONS = {")
            .expect("dev.js has DEV_ACTIONS")..];
        let table = &table[..table.find("\n};").expect("DEV_ACTIONS is closed")];
        let key = regex::Regex::new(r#"(?m)^  '(dev-[a-z-]+)': function"#).unwrap();
        let mut in_js: Vec<String> = key.captures_iter(table).map(|c| c[1].to_string()).collect();
        in_js.sort();
        assert_eq!(in_markup, in_js);
        assert_eq!(in_markup.len(), 7, "{in_markup:?}");
        // Every button the markup ships carries one.
        assert_eq!(
            html.matches("<button").count() - html.matches(r#"data-action="dev-"#).count(),
            0,
            "a button with no verb; {html}"
        );
    }

    /// The page body as a sandbox seeded with long values renders it, less
    /// the stylesheet link and the script: what `admin-core-widths.spec.ts`
    /// lays out in a real content column with `dev.css` beside it (the
    /// e2e:visual server is not a sandbox, so it serves neither).
    fn dev_fixture() -> String {
        let seed = SeedInfo {
            template: "northwind-traders-production-storefront".into(),
            suggested_prompt: "Build a storefront for Northwind Traders' Wellington CBD store at \
                               https://northwind-traders-production.example.com/activities/\
                               wellington-cbd-store-lambton-quay/opening-hours?listing=\
                               listing_01HZX9Q2KZp7a9XyTqAbCdEfGh, with the opening hours, a \
                               product list and a contact form."
                .into(),
            guide_markdown: String::new(),
            llms_text: None,
        };
        let html = body(
            Some(&seed),
            Some((
                "accountspayable.northwindtraders.wellington@example.com",
                "correct-horse-battery-staple-northwind-2026",
            )),
        )
        .into_string();
        let html = &html[..html
            .find(r#"<link rel="stylesheet""#)
            .expect("the stylesheet link")];
        format!("{html}\n")
    }

    /// `admin-core-widths.spec.ts` lays the workspace out from a fixture
    /// file; this keeps that file what the page renders. To refresh it after
    /// a deliberate markup change, write `dev_fixture()` to the path below.
    #[test]
    fn the_dev_fixture_is_what_the_page_renders() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-web/tests/e2e/fixtures/dev-page.html"
        );
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk,
            dev_fixture(),
            "{path} is out of date; write the expected text to it"
        );
    }

    /// Every `/b/dev/api/…` URL the page calls is a route this block serves.
    ///
    /// `dev.js` reaches four endpoints that no Rust caller does — the file
    /// list, the file read, the staging endpoint and the status poll — and
    /// nothing but a running browser would notice a typo in one of them: a
    /// misspelled path is a 404 the page logs and swallows, so the Compile
    /// button would simply stop working with a line in a panel to say why.
    /// The URLs are scraped out of the script rather than listed here, so a
    /// new endpoint the page starts calling is checked without this test
    /// being edited.
    #[test]
    fn the_page_only_calls_endpoints_this_block_routes() {
        let js = assets::dev_js();
        let mut found = 0;
        for (index, _) in js.match_indices("'/b/dev/api") {
            let rest = &js[index + 1..];
            let end = rest.find('\'').expect("an unterminated string literal");
            // The query string is not part of a route template: the files
            // list is `/b/dev/api/files?prefix=…` at the call site and
            // `/b/dev/api/files` in the table.
            let url = &rest[..end];
            let path = url.split('?').next().unwrap();
            assert!(
                super::super::ROUTES
                    .iter()
                    .any(|route| route.template == path),
                "dev.js calls {url}, which this block does not route"
            );
            found += 1;
        }
        // A scrape that matched nothing would pass the loop above silently.
        assert!(found >= 4, "only {found} api URLs found in dev.js");
    }

    /// The page can read the guest ABI version out of a legacy block's
    /// vendored module, in the spelling that module actually uses.
    ///
    /// A legacy block carries its own copy of the SDK as `src/wafer_guest.rs`
    /// and compiles against it rather than against the session's guest crate,
    /// so `dev.js` parses `WAFER_GUEST_VERSION` out of that copy and reports
    /// it with the staged build, and `blocks_api.rs` refuses anything that is
    /// not the sandbox's own. Those copies were byte-copies of the crate's
    /// `src/lib.rs`, so the spelling the regex must match is the one
    /// `GUEST_LIB_RS` uses. Were the constant reformatted — a type annotation
    /// dropped, the spacing changed — the regex would no longer describe the
    /// files it is aimed at, and every legacy block would be staged as `null`
    /// ("unknown") with the check silently off. This is the only place the
    /// regex and the file it is aimed at are both in scope.
    #[test]
    fn the_page_can_read_the_version_out_of_a_vendored_guest_module() {
        assert!(
            assets::dev_js().contains(r"/WAFER_GUEST_VERSION: u32 = (\d+)/"),
            "dev.js must read a legacy block's own guest version, not assume one"
        );
        let expected = format!(
            "WAFER_GUEST_VERSION: u32 = {}",
            super::super::WAFER_GUEST_VERSION
        );
        assert!(
            super::super::scaffold::GUEST_LIB_RS.contains(&expected),
            "the guest crate must state `{expected}` for dev.js's regex to find it"
        );
    }
}
