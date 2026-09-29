//! The two templates, compiled for real and run for real.
//!
//! `wafer_guest_parity.rs` proves the JSON the guest module *renders* is the
//! JSON the host's types parse. That is a check of shapes, and it can be true
//! of a module that never compiles to wasm, never negotiates the JSON host
//! codec, and never reaches a database. This file closes the rest of the
//! loop:
//!
//! 1. copy a template out of the tree (dereferencing the `wafer_guest.rs`
//!    symlink) and build it with **plain `cargo`** for `wasm32-wasip1`,
//!    `--offline`, which is what proves the crate has no dependencies — the
//!    browser toolchain the sandbox actually uses has no registry at all;
//! 2. load it the way the sandbox loads a staged block — read its
//!    `BlockInfo` under deny-all, run the static rules over that, then
//!    `WasmiBlock::load_with_capabilities_and_limits` with the capabilities
//!    those rules ACCEPTED (see [`load_as_the_sandbox_does`]);
//! 3. register it in a real `Wafer` beside the real `wafer-run/database`
//!    block over in-memory SQLite and start the runtime, which runs the
//!    guest's `Init` — and therefore its `db::ensure_table`;
//! 4. drive HTTP requests through it and assert on the bytes that come back.
//!
//! Everything between the template's source and the response body is the
//! production path: the same three-step load `impresspress-web`'s
//! `dev_runtime::load_guest` performs, the ABI exports, the JSON host codec,
//! WRAP's own-namespace rule, the `schema` capability, and the real database
//! handler.
//!
//! One thing this cannot claim: `WasmiBlock`'s linker defines every host
//! import regardless of the capability set — enforcement is per call, off the
//! store's host state — so there is no load-time import filtering here to
//! exercise. The capabilities passed at load are the guest's bound:
//! `Wafer::start()` narrows them to what the guest declares (∩ its
//! `capabilities` block config) and never widens them, so a guest runs under
//! at most the accepted spec, as it does in the sandbox.
//!
//! # When it does not run
//!
//! A machine without `cargo` or without the `wasm32-wasip1` target cannot do
//! step 1. The test then prints why and returns — but with
//! `IMPRESSPRESS_GUEST_GOLDEN=1` set (as CI does) the same condition is a
//! **failure**, so a CI job that lost the target reports it instead of
//! quietly testing nothing.
#![cfg(all(feature = "block-dev", feature = "wasm"))]

use std::{path::Path, process::Command, sync::Arc};

use impresspress_core::blocks::dev::{control::DynamicBlockSpec, scaffold::Template, validation};
use wafer_block::{
    abi::{GuestAction, GuestResult},
    http_codec,
    streams::input::InputStream,
    BlockCapabilities, ErrorCode, Message, MetaEntry, WaferError,
};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_run::{wasm::WasmiBlock, ResourceLimits, Wafer};

/// The canonical guest support module, compiled for the host, so a test can
/// render a `BlockInfo` exactly as a sandbox block does.
///
/// It carries its own `#![expect(dead_code)]` — this file uses only part of
/// the API — so this declaration must not add a second one.
#[path = "../src/blocks/dev/templates/wafer_guest.rs"]
mod wafer_guest;

// ---------------------------------------------------------------------------
// Building a template
// ---------------------------------------------------------------------------

/// Whether this machine can build a template, and why not when it cannot.
fn toolchain_ready() -> Result<(), String> {
    let probe = Command::new("cargo").arg("--version").output();
    match probe {
        Ok(out) if out.status.success() => {}
        Ok(out) => return Err(format!("`cargo --version` failed: {}", out.status)),
        Err(e) => return Err(format!("`cargo` is not runnable: {e}")),
    }
    // `--print target-libdir` succeeds only when the target's std is actually
    // installed, which is the thing the build needs — `rustc --print
    // target-list` would answer yes for every target rustc knows about.
    let std_probe = Command::new("rustc")
        .args(["--print", "target-libdir", "--target", "wasm32-wasip1"])
        .output();
    match std_probe {
        Ok(out) if out.status.success() => {
            let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if Path::new(&dir).is_dir() {
                Ok(())
            } else {
                Err(format!(
                    "the wasm32-wasip1 std is not installed ({dir} is missing) — \
                     `rustup target add wasm32-wasip1`"
                ))
            }
        }
        Ok(_) => Err(
            "rustc does not know the wasm32-wasip1 target — `rustup target add wasm32-wasip1`"
                .to_string(),
        ),
        Err(e) => Err(format!("`rustc` is not runnable: {e}")),
    }
}

/// Gate every test in this file on [`toolchain_ready`].
///
/// Returns `false` to skip. `IMPRESSPRESS_GUEST_GOLDEN=1` turns the skip into
/// a panic, so the environment that is supposed to run this (CI) cannot pass
/// by not running it.
fn buildable() -> bool {
    match toolchain_ready() {
        Ok(()) => true,
        Err(why) if std::env::var_os("IMPRESSPRESS_GUEST_GOLDEN").is_some() => {
            panic!("IMPRESSPRESS_GUEST_GOLDEN=1 but the template cannot be built: {why}")
        }
        Err(why) => {
            eprintln!(
                "SKIPPED wafer_guest_golden: {why}. Set IMPRESSPRESS_GUEST_GOLDEN=1 to make this \
                 a failure instead."
            );
            false
        }
    }
}

/// Copy `from` to `to`, following symlinks.
///
/// Following them is the point: `src/wafer_guest.rs` in each template is a
/// symlink to the canonical module, and the copy has to be a real file so the
/// build is of the same bytes `dev_create_block` writes.
fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        // `metadata` follows symlinks, so a symlinked file reports as a file.
        if entry.metadata()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The `name = "…"` line of a template's `Cargo.toml`.
///
/// The crate name is not the directory name — `templates/table` is the
/// `newsletter` crate — and the artifact is named after the crate, with
/// hyphens turned into underscores by the linker.
fn package_name(manifest: &str) -> String {
    manifest
        .lines()
        .find_map(|line| {
            line.strip_prefix("name = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .expect("the template's Cargo.toml declares a package name")
        .to_string()
}

/// Build `templates/{name}` for `wasm32-wasip1` and return the module.
fn build_template(name: &str) -> Vec<u8> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/blocks/dev/templates")
        .join(name);
    let out = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&source, out.path()).expect("copy the template");
    build_crate(name, out.path())
}

/// Scaffold `template` as block `name` — the three files `dev_create_block`
/// writes, from [`Template::files`] itself — and build it.
///
/// The one edit is the one an author makes before a second block from the
/// same template can run beside the first: agent tool names are unique
/// across a runtime, and the template's is not derived from the block name.
fn build_scaffolded(template: Template, name: &str) -> Vec<u8> {
    build_scaffolded_with(template, name, |content| content)
}

/// [`build_scaffolded`], with `edit` applied to every file's content — the
/// change an author makes to the template before compiling it.
fn build_scaffolded_with(
    template: Template,
    name: &str,
    edit: impl Fn(String) -> String,
) -> Vec<u8> {
    let out = tempfile::tempdir().expect("tempdir");
    let block_dir = format!("blocks/{name}/");
    let tool = format!("\"subscribe_{}\"", name.replace('-', "_"));
    for (path, content) in template.files(name) {
        let content = edit(content.replace("\"subscribe_newsletter\"", &tool));
        let relative = path
            .strip_prefix(&block_dir)
            .unwrap_or_else(|| panic!("{path} is outside {block_dir}"));
        let target = out.path().join(relative);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("create the directory");
        std::fs::write(&target, content).expect("write the scaffolded file");
    }
    build_crate(name, out.path())
}

/// Build the crate at `dir` for `wasm32-wasip1` and return the module.
fn build_crate(name: &str, dir: &Path) -> Vec<u8> {
    let package =
        package_name(&std::fs::read_to_string(dir.join("Cargo.toml")).expect("read Cargo.toml"));
    // `--offline` is the assertion, not an optimization: a template with a
    // single dependency would fail here rather than quietly working on a
    // machine with a warm registry cache. `--target-dir` is explicit so an
    // ambient `CARGO_TARGET_DIR` cannot move the artifact out from under the
    // read below.
    let target_dir = dir.join("target");
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip1",
            "--offline",
        ])
        .arg("--target-dir")
        .arg(&target_dir)
        .current_dir(dir)
        .status()
        .expect("run cargo");
    assert!(
        status.success(),
        "the {name} template must build with plain cargo and no dependencies"
    );

    let artifact = target_dir
        .join("wasm32-wasip1/release")
        .join(format!("{}.wasm", package.replace('-', "_")));
    let bytes =
        std::fs::read(&artifact).unwrap_or_else(|e| panic!("read {}: {e}", artifact.display()));
    assert!(
        bytes.len() <= impresspress_core::blocks::dev::validation::MAX_ARTIFACT_BYTES,
        "the {name} template must fit the sandbox's {} byte artifact limit; it is {} bytes",
        impresspress_core::blocks::dev::validation::MAX_ARTIFACT_BYTES,
        bytes.len(),
    );
    bytes
}

// ---------------------------------------------------------------------------
// The runtime the block runs in
// ---------------------------------------------------------------------------

/// An unstarted `Wafer` carrying the real `wafer-run/database` block over an
/// in-memory SQLite.
///
/// The shape `wafer-run`'s own `json_host_codec_e2e` uses: no admin block and
/// no WRAP grants, so the guest is an ordinary unprivileged caller that
/// reaches its own namespace and nothing else.
fn golden_wafer() -> Wafer {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("build a Wafer");
    let sqlite = Arc::new(SQLiteDatabaseService::open_in_memory().expect("in-memory sqlite"));
    wafer_core::service_blocks::database::register_with_tables(&mut wafer, sqlite, vec![])
        .expect("register wafer-run/database");
    wafer
}

/// Load `wasm` exactly as the sandbox loads a staged block, and report the
/// spec it was admitted under.
///
/// Three steps, in production's order (`blocks/dev/blocks_api.rs::stage` and
/// `impresspress-web/src/dev_runtime.rs::load_guest`):
///
/// 1. **inspect** — instantiate under [`BlockCapabilities::none`] and read the
///    guest's own `BlockInfo`. No lifecycle event runs, because nothing has
///    approved the capability set that is *inside* the value being read.
/// 2. **the rules** — [`validation::validate_static`] turns that declaration
///    into an accepted [`DynamicBlockSpec`], refusing anything outside the
///    block's namespace.
/// 3. **load** — [`WasmiBlock::load_with_capabilities_and_limits`] with
///    `spec.capabilities` (the accepted set, never the raw declaration) and
///    [`ResourceLimits::default`], which is the fuel/memory pair
///    `dev_runtime::guest_limits` also builds.
///
/// A `load_from_bytes` here would carry no bound at all, leaving the guest's
/// capabilities to whatever an operator's `capabilities` block config states
/// (`none()` without one) — a set that has nothing to do with step 2's
/// refusals.
fn load_as_the_sandbox_does(name: &str, wasm: &[u8]) -> (WasmiBlock, DynamicBlockSpec) {
    let inspected = WasmiBlock::load_with_capabilities(wasm, BlockCapabilities::none())
        .unwrap_or_else(|e| panic!("inspect-load {name}: {e}"));
    let info = wafer_run::Block::info(&inspected);

    let spec = validation::validate_static(
        name,
        &info,
        "sha",
        &validation::builtin_route_prefixes(),
        &[],
        &std::collections::BTreeSet::new(),
    )
    .unwrap_or_else(|found| panic!("the compiled {name} template was refused: {found:?}"));

    let block = WasmiBlock::load_with_capabilities_and_limits(
        wasm,
        spec.capabilities.clone(),
        ResourceLimits::default(),
    )
    .unwrap_or_else(|e| panic!("load {name} under its accepted capabilities: {e}"));
    (block, spec)
}

/// The `Message` the HTTP boundary builds for a request, plus the auth meta
/// the auth layer would have added.
fn http_msg(method: &str, path: &str, auth: &[(&str, &str)]) -> Message {
    let mut msg = http_codec::build_http_message(
        method,
        path,
        "",
        "127.0.0.1",
        [("content-type", "application/json")],
    );
    for (key, value) in auth {
        msg.set_meta(*key, *value);
    }
    msg
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// The `table` template creates its table on `Init` and serves all three of
/// its endpoints over the JSON host codec.
#[tokio::test]
async fn table_template_creates_its_table_and_serves_its_endpoints() {
    if !buildable() {
        return;
    }
    let wasm = build_template("table");
    let mut wafer = golden_wafer();
    // Loaded under the capabilities the sandbox's rules accepted — including
    // `schema: true`, which is what lets the `Init` below create the table.
    let (block, spec) = load_as_the_sandbox_does("newsletter", &wasm);
    assert!(
        spec.capabilities.schema,
        "the accepted spec grants schema ops"
    );
    wafer
        .register_block("site/newsletter", Arc::new(block))
        .expect("register site/newsletter");
    // `start` runs the guest's `Init`, which is where `db::ensure_table` is —
    // so a failure here is the schema capability, WRAP, or the wire shape of
    // `database.ensure_table`, and nothing else.
    let wafer = wafer.start().await.expect("start the runtime");

    // A public POST from an anonymous caller.
    let out = wafer
        .run_block(
            "site/newsletter",
            http_msg("POST", "/b/newsletter/subscribe", &[("auth.user_id", "")]),
            InputStream::from_bytes(br#"{"email":"a@b.c"}"#.to_vec()),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    assert_eq!(http_codec::resolve_status(&out.meta, 200), 200);
    let body: serde_json::Value = serde_json::from_slice(&out.body).unwrap_or_else(|e| {
        panic!(
            "subscribe body ({e}): {:?}",
            String::from_utf8_lossy(&out.body)
        )
    });
    assert_eq!(body["ok"], true, "{body}");

    // The row is really there: an admin read comes back through the database.
    let out = wafer
        .run_block(
            "site/newsletter",
            http_msg(
                "GET",
                "/b/newsletter/subscribers",
                &[("auth.user_id", "admin_1"), ("auth.user_roles", "admin")],
            ),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    let listing: serde_json::Value = serde_json::from_slice(&out.body).unwrap_or_else(|e| {
        panic!(
            "listing body ({e}): {:?}",
            String::from_utf8_lossy(&out.body)
        )
    });
    assert_eq!(listing["subscribers"][0]["email"], "a@b.c", "{listing}");
    let id = listing["subscribers"][0]["id"]
        .as_str()
        .expect("the subscriber's id")
        .to_string();
    assert!(
        !listing["subscribers"][0]["created_at"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "the host stamps created_at: {listing}"
    );

    // The `{id}` route: the guest's own router binds the parameter.
    let out = wafer
        .run_block(
            "site/newsletter",
            http_msg(
                "GET",
                &format!("/b/newsletter/subscribers/{id}"),
                &[("auth.user_id", "admin_1"), ("auth.user_roles", "admin")],
            ),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    let one: serde_json::Value = serde_json::from_slice(&out.body).expect("by-id body");
    assert_eq!(one["email"], "a@b.c", "{one}");

    // A second signup for the address: refused by the UNIQUE constraint,
    // which the host reports to the guest as `AlreadyExists`.
    let out = wafer
        .run_block(
            "site/newsletter",
            http_msg("POST", "/b/newsletter/subscribe", &[("auth.user_id", "")]),
            InputStream::from_bytes(br#"{"email":"a@b.c"}"#.to_vec()),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    assert_eq!(http_codec::resolve_status(&out.meta, 200), 409);

    // A malformed body is the template's own 400, not a trap.
    let out = wafer
        .run_block(
            "site/newsletter",
            http_msg("POST", "/b/newsletter/subscribe", &[("auth.user_id", "")]),
            InputStream::from_bytes(br#"{"email":"nope"}"#.to_vec()),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    assert_eq!(http_codec::resolve_status(&out.meta, 200), 400);

    // A path the block declares no endpoint for is the guest's own 404.
    let out = wafer
        .run_block(
            "site/newsletter",
            http_msg("GET", "/b/newsletter/nothing", &[]),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    assert_eq!(http_codec::resolve_status(&out.meta, 200), 404);
}

/// The `hello` template answers, and reports itself as `site/hello`.
#[tokio::test]
async fn hello_template_answers() {
    if !buildable() {
        return;
    }
    let wasm = build_template("hello");
    let mut wafer = golden_wafer();
    // A block that claims nothing runs under a capability set that permits
    // nothing — the production constructor, with `none()` as the accepted set.
    let (block, spec) = load_as_the_sandbox_does("hello", &wasm);
    assert!(!spec.capabilities.collections.is_enabled());
    assert!(!spec.capabilities.schema);
    wafer
        .register_block("site/hello", Arc::new(block))
        .expect("register site/hello");
    let wafer = wafer.start().await.expect("start the runtime");

    let out = wafer
        .run_block(
            "site/hello",
            http_msg("GET", "/b/hello/", &[]),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    assert_eq!(http_codec::resolve_status(&out.meta, 200), 200);
    assert!(
        String::from_utf8_lossy(&out.body).contains("Hello from site/hello"),
        "{:?}",
        String::from_utf8_lossy(&out.body)
    );
}

/// The `BlockInfo` a real compiled template reports is the one the sandbox's
/// static rules accept — the same check `wafer_guest_parity` makes against
/// the natively-rendered string, but against the bytes wasmi read out of the
/// module.
#[tokio::test]
async fn a_compiled_template_reports_the_block_info_the_sandbox_accepts() {
    if !buildable() {
        return;
    }
    let wasm = build_template("table");
    // `load_as_the_sandbox_does` IS the inspect → rules → load sequence, so
    // reaching its second return value means the rules accepted the module.
    let (_block, spec) = load_as_the_sandbox_does("newsletter", &wasm);
    assert_eq!(spec.name, "site/newsletter");
    assert_eq!(spec.routes[0].prefix, "/b/newsletter/");
    assert!(spec.capabilities.schema);
    assert!(!spec.capabilities.ddl);
    assert!(spec
        .capabilities
        .allows_collection("site__newsletter__subscribers"));
}

/// POST one signup to `name`'s `subscribe` endpoint and return the status.
async fn subscribe(wafer: &Wafer, name: &str, email: &str) -> u16 {
    let out = wafer
        .run_block(
            &format!("site/{name}"),
            http_msg(
                "POST",
                &format!("/b/{name}/subscribe"),
                &[("auth.user_id", "")],
            ),
            InputStream::from_bytes(format!(r#"{{"email":"{email}"}}"#).into_bytes()),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    http_codec::resolve_status(&out.meta, 200)
}

/// The emails `name`'s admin listing returns, in listing order.
async fn subscriber_emails(wafer: &Wafer, name: &str) -> Vec<String> {
    let out = wafer
        .run_block(
            &format!("site/{name}"),
            http_msg(
                "GET",
                &format!("/b/{name}/subscribers"),
                &[("auth.user_id", "admin_1"), ("auth.user_roles", "admin")],
            ),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    let listing: serde_json::Value = serde_json::from_slice(&out.body).unwrap_or_else(|e| {
        panic!(
            "{name} listing body ({e}): {:?}",
            String::from_utf8_lossy(&out.body)
        )
    });
    listing["subscribers"]
        .as_array()
        .unwrap_or_else(|| panic!("{name} listing has no subscribers array: {listing}"))
        .iter()
        .map(|row| row["email"].as_str().expect("an email").to_string())
        .collect()
}

/// Register `name`, scaffolded from the `table` template and admitted as the
/// sandbox admits it.
fn register_scaffolded(wafer: &mut Wafer, name: &str) {
    let wasm = build_scaffolded(Template::Table, name);
    let (block, spec) = load_as_the_sandbox_does(name, &wasm);
    assert_eq!(spec.name, format!("site/{name}"));
    wafer
        .register_block(format!("site/{name}"), Arc::new(block))
        .unwrap_or_else(|e| panic!("register site/{name}: {e}"));
}

/// A block with a hyphen in its name, scaffolded as `dev_create_block`
/// scaffolds it, can write a row and read it back.
///
/// The hyphen is what makes this a different test from the `newsletter` one:
/// the collection the block claims is the table the database writes, only if
/// the claimed spelling is one the database uses as written.
#[tokio::test]
async fn a_hyphenated_block_round_trips_its_own_rows() {
    if !buildable() {
        return;
    }
    let mut wafer = golden_wafer();
    register_scaffolded(&mut wafer, "my-shop");
    let wafer = wafer.start().await.expect("start the runtime");

    assert_eq!(subscribe(&wafer, "my-shop", "mine@example.com").await, 200);
    assert_eq!(
        subscriber_emails(&wafer, "my-shop").await,
        vec!["mine@example.com".to_string()],
    );
}

/// `site/my-shop` and `site/myshop` are two blocks with two sets of tables:
/// neither sees a row the other wrote.
///
/// A name with its hyphen removed is always another legal block name, so
/// this is the pair a stripped identifier would merge.
#[tokio::test]
async fn a_hyphenated_block_cannot_reach_its_unhyphenated_twin() {
    if !buildable() {
        return;
    }
    let mut wafer = golden_wafer();
    register_scaffolded(&mut wafer, "myshop");
    register_scaffolded(&mut wafer, "my-shop");
    let wafer = wafer.start().await.expect("start the runtime");

    assert_eq!(subscribe(&wafer, "myshop", "twin@example.com").await, 200);
    assert_eq!(subscribe(&wafer, "my-shop", "mine@example.com").await, 200);

    assert_eq!(
        subscriber_emails(&wafer, "my-shop").await,
        vec!["mine@example.com".to_string()],
        "site/my-shop reads only its own rows",
    );
    assert_eq!(
        subscriber_emails(&wafer, "myshop").await,
        vec!["twin@example.com".to_string()],
        "site/myshop's table holds only its own rows",
    );
}

/// A list with no `limit` returns every row. The guest SDK leaves `limit`
/// out of the request when the author set none; the host refuses a `0`
/// page size, which is what the SDK used to send for "no limit".
#[tokio::test]
async fn a_list_without_a_limit_returns_every_row() {
    if !buildable() {
        return;
    }
    // The edit below must remove something, or this is the paged listing.
    assert!(
        Template::Table
            .files("unpaged")
            .iter()
            .any(|(_, content)| content.contains(".limit(200)")),
        "the table template's listing is paged at 200"
    );
    let wasm = build_scaffolded_with(Template::Table, "unpaged", |content| {
        content.replace(".limit(200)", "")
    });
    let (block, _spec) = load_as_the_sandbox_does("unpaged", &wasm);
    let mut wafer = golden_wafer();
    wafer
        .register_block("site/unpaged", Arc::new(block))
        .expect("register site/unpaged");
    let wafer = wafer.start().await.expect("start the runtime");

    assert_eq!(subscribe(&wafer, "unpaged", "one@example.com").await, 200);
    assert_eq!(subscribe(&wafer, "unpaged", "two@example.com").await, 200);
    let mut emails = subscriber_emails(&wafer, "unpaged").await;
    emails.sort();
    assert_eq!(
        emails,
        vec!["one@example.com".to_string(), "two@example.com".to_string()]
    );
}

// ---------------------------------------------------------------------------
// The header contract
// ---------------------------------------------------------------------------

/// A guest written against the raw ABI rather than the template SDK, so it
/// can return what a hostile author could: the SDK only ever renders a
/// `Respond`.
///
/// Its `BlockInfo` is the one the SDK renders for two public `GET` endpoints
/// and no capabilities, so the sandbox's rules admit it as they would any
/// block. `GET /b/hostile/echo` answers with the request frame the host
/// handed it, byte for byte, so the test reads exactly what the guest saw.
/// Every other request answers an `Error` whose meta tries to set a session
/// cookie, a redirect and a CORS grant beside one ordinary header.
fn build_hostile_guest() -> Vec<u8> {
    fn unused(_: &wafer_guest::Request, _: &wafer_guest::Ctx) -> wafer_guest::Response {
        wafer_guest::Response::text(500, "never dispatched: the raw guest routes itself")
    }
    let info = wafer_guest::render_block_info(
        &wafer_guest::Block::new("site/hostile", "Tries every egress")
            .endpoint(
                wafer_guest::Endpoint::new(wafer_guest::Method::Get, "/b/hostile/echo", unused)
                    .auth(wafer_guest::Auth::Public),
            )
            .endpoint(
                wafer_guest::Endpoint::new(wafer_guest::Method::Get, "/b/hostile/fail", unused)
                    .auth(wafer_guest::Auth::Public),
            ),
    );
    let entry = |key: &str, value: &str| MetaEntry {
        key: key.to_string(),
        value: value.to_string(),
    };
    let error = serde_json::to_string(&GuestResult {
        action: GuestAction::Error,
        response: None,
        error: Some(WaferError {
            code: ErrorCode::PermissionDenied,
            message: "refused".to_string(),
            meta: vec![
                entry("resp.set_cookie.0", "session=attacker; Path=/"),
                entry("resp.header.location", "https://evil.example/"),
                entry("resp.header.access-control-allow-origin", "*"),
                entry("resp.header.x-guest", "kept"),
            ],
        }),
        message: None,
    })
    .expect("render the error result");

    let source = format!(
        r####"
const INFO: &str = r###"{info}"###;
const ERROR_RESULT: &str = r###"{error}"###;

fn pack(bytes: &'static [u8]) -> i64 {{
    ((bytes.as_ptr() as u32 as i64) << 32) | bytes.len() as i64
}}

#[no_mangle]
pub extern "C" fn __wafer_alloc(size: i32) -> i32 {{
    Box::leak(vec![0u8; size.max(0) as usize].into_boxed_slice()).as_mut_ptr() as i32
}}

#[no_mangle]
pub extern "C" fn __wafer_host_codec() -> i32 {{
    1
}}

#[no_mangle]
pub extern "C" fn __wafer_info() -> i64 {{
    pack(INFO.as_bytes())
}}

#[no_mangle]
pub extern "C" fn __wafer_handle(ptr: i32, len: i32) -> i64 {{
    let frame = unsafe {{ std::slice::from_raw_parts(ptr as *const u8, len as usize) }};
    let echo = b"/b/hostile/echo";
    if !frame.windows(echo.len()).any(|w| w == echo) {{
        return pack(ERROR_RESULT.as_bytes());
    }}
    let mut out = String::from(r#"{{"action":"Respond","response":{{"data":["#);
    for (i, byte) in frame.iter().enumerate() {{
        if i > 0 {{
            out.push(',');
        }}
        out.push_str(&byte.to_string());
    }}
    out.push_str(r#"],"meta":[]}},"error":null,"message":null}}"#);
    pack(Box::leak(out.into_bytes().into_boxed_slice()))
}}

#[no_mangle]
pub extern "C" fn __wafer_lifecycle(_ptr: i32, _len: i32) -> i64 {{
    pack(br#"{{"Ok":null}}"#)
}}
"####
    );
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("create src");
    std::fs::write(dir.path().join("src/lib.rs"), source).expect("write the guest");
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/blocks/dev/templates/hello/Cargo.toml"),
    )
    .expect("read the hello manifest")
    .replace("name = \"hello\"", "name = \"hostile\"");
    std::fs::write(dir.path().join("Cargo.toml"), manifest).expect("write the manifest");
    build_crate("hostile", dir.path())
}

/// The hostile guest, admitted as the sandbox admits it, in a started runtime.
async fn hostile_runtime() -> Arc<Wafer> {
    let wasm = build_hostile_guest();
    let (block, spec) = load_as_the_sandbox_does("hostile", &wasm);
    assert_eq!(spec.name, "site/hostile");
    let mut wafer = golden_wafer();
    wafer
        .register_block("site/hostile", Arc::new(block))
        .expect("register site/hostile");
    wafer.start().await.expect("start the runtime")
}

/// A sandbox guest never sees the request's credentials. The service worker
/// puts the admin's session cookie on every same-origin request it forwards,
/// `/b/{name}/` included, so this is the header a guest would otherwise read.
/// An ordinary header does arrive, so the guest is shown the request.
#[tokio::test]
async fn a_guest_never_sees_the_session_cookie_or_authorization() {
    if !buildable() {
        return;
    }
    let wafer = hostile_runtime().await;

    let msg = http_codec::build_http_message(
        "GET",
        "/b/hostile/echo",
        "",
        "127.0.0.1",
        [
            ("cookie", "impresspress_session=admin-session"),
            ("authorization", "Bearer admin-token"),
            ("x-probe", "visible"),
        ],
    );
    let out = wafer
        .run_block("site/hostile", msg, InputStream::empty())
        .await
        .collect_buffered()
        .await
        .expect("a buffered response");
    let seen = String::from_utf8(out.body).expect("the echoed frame is JSON text");

    assert!(
        seen.contains("visible"),
        "the guest was shown the request: {seen}"
    );
    assert!(
        !seen.contains("admin-session"),
        "the session cookie reached the guest: {seen}"
    );
    assert!(
        !seen.contains("admin-token"),
        "the credential reached the guest: {seen}"
    );
}

/// An `Error` a guest returns cannot set a cookie, a redirect or a CORS
/// grant: the HTTP response the codec renders from it carries none of them,
/// only the ordinary header the guest set.
#[tokio::test]
async fn a_guest_error_cannot_set_a_cookie_or_a_sensitive_header() {
    if !buildable() {
        return;
    }
    let wafer = hostile_runtime().await;

    let out = wafer
        .run_block(
            "site/hostile",
            http_msg("GET", "/b/hostile/fail", &[("auth.user_id", "")]),
            InputStream::empty(),
        )
        .await;
    let parts = http_codec::collect_http_response(out).await;

    assert_eq!(parts.status, 403);
    let header = |name: &str| {
        parts
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(header("x-guest"), Some("kept"), "{:?}", parts.headers);
    for name in ["set-cookie", "location", "access-control-allow-origin"] {
        assert_eq!(header(name), None, "{name} crossed: {:?}", parts.headers);
    }
}

// ---------------------------------------------------------------------------
// Native discovery
// ---------------------------------------------------------------------------

/// A block the deployment built and placed under `blocks/` runs with the
/// capabilities it declares: native discovery approves the declaration of the
/// deployment's own blocks. A block loaded with no stated bound runs with
/// none, and the `table` template's `Init` then cannot create its table.
#[tokio::test]
async fn a_discovered_block_runs_with_the_capabilities_it_declares() {
    if !buildable() {
        return;
    }
    let wasm = build_template("table");
    let root = tempfile::tempdir().expect("tempdir");
    let target = root.path().join("blocks/newsletter/target");
    std::fs::create_dir_all(&target).expect("create the block's target dir");
    std::fs::write(target.join("block.wasm"), &wasm).expect("place the block");

    let mut wafer = golden_wafer();
    impresspress_core::builder::register_discovered_blocks(&mut wafer, root.path())
        .expect("discover the block");
    let wafer = wafer.start().await.expect("start the runtime");

    let effective = wafer
        .effective_capabilities("site/newsletter")
        .expect("the discovered block has effective capabilities");
    assert!(
        effective.schema,
        "the declared schema capability: {effective:?}"
    );
    assert!(
        effective.allows_collection("site__newsletter__subscribers"),
        "the declared collection: {effective:?}"
    );
    assert_eq!(
        subscribe(&wafer, "newsletter", "found@example.com").await,
        200
    );
}
