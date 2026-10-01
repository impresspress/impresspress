//! `POST /b/dev/api/blocks`, `GET /b/dev/api/reference` and
//! `GET /b/dev/api/guest` — starting a block, the guide for writing one, and
//! the SDK crate it is compiled against.
//!
//! # Why a scaffolder and not a documented file list
//!
//! A block is two files, `Cargo.toml` and `src/lib.rs`, and both are
//! instantiated from a template: the block's name is
//! its directory, its crate name, its block id (`site/<name>`), its route
//! prefix (`/b/<name>/`) and its collection prefix (`site__<name>__`, a
//! hyphen spelled `_`) all at once, and a template that got any one of those
//! wrong would be refused by validation with a diagnostic the author did not
//! cause. The manifest also carries the one dependency a block may have —
//! the guest SDK crate, by path — and an agent that wrote the manifest by
//! hand would have to get that path exactly right too.
//!
//! The SDK itself is not part of a block: it is the `crates/wafer-guest`
//! crate, served whole by [`guest_files`] and built once beside the blocks.
//!
//! # Why the reference is served rather than shipped as a file
//!
//! Its two long code samples ARE the templates, spliced in by
//! [`reference_markdown`] at render time. A reference whose samples were
//! copies would drift from the templates the same endpoint writes, and the
//! drift would be invisible — both halves would still look right.

use serde::{Deserialize, Serialize};
use wafer_run::{context::Context, ErrorCode, InputStream, OutputStream};

use super::{
    contracts::{
        CreateBlockRequest, CreateBlockResponse, FileConflict, GuestResponse, ReferenceResponse,
        WarmupCrate,
    },
    files, no_store, no_store_db_error_internal, no_store_error,
    paths::{self, WorkspaceArea, BLOCK_NAME_RULE},
    repo::seed_info,
    validation, workspace, DevShared, WAFER_GUEST_VERSION,
};
use crate::blocks::crud;

/// The guest SDK crate, byte for byte from `crates/wafer-guest`.
///
/// One source, three readers: `GET /b/dev/api/guest` hands it to the page
/// (which hands it to the compiler), the export archive carries it beside
/// the blocks, and the golden test builds the templates against it. A
/// scaffolded block never contains it — the block depends on it by path.
pub const GUEST_CARGO_TOML: &str = include_str!("../../../../wafer-guest/Cargo.toml");
pub const GUEST_LIB_RS: &str = include_str!("../../../../wafer-guest/src/lib.rs");

/// The crate as the compiler and the archive want it: crate-relative paths.
pub fn guest_files() -> std::collections::BTreeMap<String, String> {
    [
        ("Cargo.toml".to_string(), GUEST_CARGO_TOML.to_string()),
        ("src/lib.rs".to_string(), GUEST_LIB_RS.to_string()),
    ]
    .into_iter()
    .collect()
}

/// The two starting points `dev_create_block` offers.
///
/// A closed enum rather than a free-form string: the template is what decides
/// which bytes are written, and an unrecognized name must be a `400` from
/// serde rather than an empty block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Template {
    /// One public `GET` and nothing else — the smallest block that serves.
    Hello,
    /// A newsletter block: a claimed collection, a table created in `init`,
    /// a public write endpoint with an agent tool, and two admin reads.
    Table,
}

impl Template {
    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Template> {
        match value {
            "hello" => Some(Template::Hello),
            "table" => Some(Template::Table),
            _ => None,
        }
    }

    /// The wire spelling (matches the serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Template::Hello => "hello",
            Template::Table => "table",
        }
    }

    /// The block name the template is written under.
    ///
    /// Not the same as [`Self::as_str`] for `table`: the template is a
    /// *newsletter*, and calling its collection `site__table__rows` would
    /// teach the namespace rule with a name that means nothing. This is the
    /// string [`instantiate`] rewrites.
    pub fn identifier(self) -> &'static str {
        match self {
            Template::Hello => "hello",
            Template::Table => "newsletter",
        }
    }

    /// The template's `Cargo.toml`, before instantiation.
    pub fn cargo_toml(self) -> &'static str {
        match self {
            Template::Hello => include_str!("templates/hello/Cargo.toml"),
            Template::Table => include_str!("templates/table/Cargo.toml"),
        }
    }

    /// The template's `src/lib.rs`, before instantiation.
    pub fn lib_rs(self) -> &'static str {
        match self {
            Template::Hello => include_str!("templates/hello/src/lib.rs"),
            Template::Table => include_str!("templates/table/src/lib.rs"),
        }
    }

    /// The two files a block starts as, workspace-relative, in path order,
    /// each instantiated under `name`.
    pub fn files(self, name: &str) -> Vec<(String, String)> {
        vec![
            (
                format!("{}{name}/Cargo.toml", workspace::BLOCKS_PREFIX),
                instantiate(self.cargo_toml(), self.identifier(), name),
            ),
            (
                format!("{}{name}/src/lib.rs", workspace::BLOCKS_PREFIX),
                instantiate(self.lib_rs(), self.identifier(), name),
            ),
        ]
    }
}

/// Rewrite a template's own name to `to`.
///
/// Deliberately five *anchored* substitutions rather than a blanket replace
/// of `from`: the `hello` template has a handler function called `hello`, and
/// a blanket replace would rename it to something with a hyphen in it — a
/// block scaffolded as `my-shop` would not compile, for a reason nothing in
/// its source would explain.
///
/// Each anchor is one of the five places the block's name is load-bearing —
/// the crate name, the block id, the route prefix, the collection prefix and
/// the config prefix — which is exactly the set validation checks. The two
/// prefixes are [`validation::collection_prefix`] and its uppercase, the
/// spelling validation requires, so a hyphen in the name becomes `_` there
/// (`site__my_shop__rows`, `SITE__MY_SHOP__KEY`) and stays a hyphen in the
/// other three.
fn instantiate(source: &str, from: &str, to: &str) -> String {
    let (from_prefix, to_prefix) = (
        validation::collection_prefix(from),
        validation::collection_prefix(to),
    );
    source
        .replace(&format!("name = \"{from}\""), &format!("name = \"{to}\""))
        .replace(&format!("site/{from}"), &format!("site/{to}"))
        .replace(&format!("/b/{from}/"), &format!("/b/{to}/"))
        .replace(&from_prefix, &to_prefix)
        .replace(&from_prefix.to_uppercase(), &to_prefix.to_uppercase())
}

/// Marker the `hello` template's source is spliced into.
const TEMPLATE_HELLO_MARKER: &str = "{{TEMPLATE_HELLO}}";

/// Marker the `table` template's source is spliced into.
const TEMPLATE_TABLE_MARKER: &str = "{{TEMPLATE_TABLE}}";

/// The authoring reference, with both templates spliced in.
///
/// `pub` because the reference is the one artifact an agent must read before
/// writing Rust, and the page renders it too.
pub fn reference_markdown() -> String {
    include_str!("templates/reference.md")
        .replace(TEMPLATE_HELLO_MARKER, Template::Hello.lib_rs().trim_end())
        .replace(TEMPLATE_TABLE_MARKER, Template::Table.lib_rs().trim_end())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /b/dev/api/blocks` — write a new block's two files.
///
/// Writes source and nothing else: a block does not serve until it is
/// compiled and staged, exactly as a hand-written `blocks/` edit does not.
/// So there is no activation here and no generation to report.
pub async fn handle_create(
    ctx: &dyn Context,
    shared: &DevShared,
    input: InputStream,
) -> OutputStream {
    let request: CreateBlockRequest = match crud::read_json_body_or(input, |detail| {
        no_store_error(
            ErrorCode::InvalidArgument,
            &format!("invalid request body: {detail}"),
        )
    })
    .await
    {
        Ok(request) => request,
        Err(refusal) => return refusal,
    };

    // The name is refused here rather than by `validate_path` on the first
    // file, so the message is about the name the caller sent rather than
    // about a path it never wrote.
    if !paths::block_name_is_valid(&request.name) {
        return no_store_error(
            ErrorCode::InvalidArgument,
            &format!(
                "block name {:?} is not allowed: {BLOCK_NAME_RULE}",
                request.name
            ),
        );
    }
    let files = request.template.files(&request.name);

    // The whole read-modify-write runs under `DevShared::workspace`, for the
    // reason `files::handle_write` documents: the manifest is loaded, changed
    // and saved as a whole, so two writers interleaving would each save a
    // snapshot that predates the other.
    let _serialized = shared.workspace.lock().await;
    let mut ws = match workspace::load(ctx).await {
        Ok(ws) => ws,
        Err(e) => return no_store_db_error_internal(e, "dev workspace load"),
    };

    // Refuse if ANY path under `blocks/<name>/` is taken, not just the two
    // this would write: a directory that holds a stray file is a block the
    // author started, and overwriting its manifest and root would leave a
    // crate that is neither what they wrote nor what the template is.
    let prefix = format!("{}{}/", workspace::BLOCKS_PREFIX, request.name);
    if let Some(existing) = ws.files.keys().find(|path| path.starts_with(&prefix)) {
        return no_store()
            .status(409)
            .json(&FileConflict::new(existing, ws.get(existing)));
    }

    // Every collision and quota is checked before any blob is stored, and
    // the manifest is saved once — `files::store_files`, which says why.
    let contents: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_bytes()))
        .collect();
    let written = match files::store_files(
        ctx,
        &mut ws,
        &WorkspaceArea::Block(request.name.clone()),
        &contents,
    )
    .await
    {
        Ok(written) => written,
        Err(refusal) => return refusal,
    };

    no_store().json(&CreateBlockResponse {
        name: request.name,
        files: written,
    })
}

/// `GET /b/dev/api/reference` — the authoring guides: the Rust one this
/// crate ships, and the site one the seed carried.
pub async fn handle_reference(ctx: &dyn Context) -> OutputStream {
    let seed = match seed_info::read(ctx).await {
        Ok(seed) => seed,
        Err(e) => return no_store_db_error_internal(e, "dev reference: seed info read"),
    };
    no_store().json(&ReferenceResponse {
        wafer_guest_version: WAFER_GUEST_VERSION,
        markdown: reference_markdown(),
        template: seed.as_ref().map(|seed| seed.template.clone()),
        site_markdown: seed.map(|seed| seed.guide_markdown),
    })
}

/// `GET /b/dev/api/guest` — the guest crate and a warm-up block.
pub async fn handle_guest(_ctx: &dyn Context) -> OutputStream {
    let prefix = format!("{}hello/", workspace::BLOCKS_PREFIX);
    let files = Template::Hello
        .files("hello")
        .into_iter()
        .map(|(path, content)| {
            let relative = path
                .strip_prefix(&prefix)
                .expect("Template::files writes under blocks/<name>/");
            (relative.to_string(), content)
        })
        .collect();
    no_store().json(&GuestResponse {
        version: WAFER_GUEST_VERSION,
        files: guest_files(),
        warmup: WarmupCrate {
            crate_name: "hello".to_string(),
            files,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every template's `Cargo.toml` is the same file but for its package
    /// name — the profile above all.
    ///
    /// cargo builds a dependency under the ROOT package's profile, so the
    /// `wafer_guest` build in `/target` is only reused by a block whose
    /// `[profile.release]` matches the one it was built with. The session's
    /// warm-up builds `hello`; a table-based block whose profile had drifted
    /// from it would silently pay the ~30 s guest rebuild on every session,
    /// and nothing but its compile time would say so.
    #[test]
    fn every_template_manifest_differs_from_hello_only_in_its_package_name() {
        let hello: Vec<&str> = Template::Hello.cargo_toml().lines().collect();
        let table: Vec<&str> = Template::Table.cargo_toml().lines().collect();
        assert_eq!(
            hello.len(),
            table.len(),
            "the two manifests differ in length"
        );
        let differing: Vec<(&str, &str)> = hello
            .iter()
            .zip(&table)
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (*a, *b))
            .collect();
        assert_eq!(
            differing,
            vec![(r#"name = "hello""#, r#"name = "newsletter""#)],
            "the templates may differ only in `[package] name`"
        );
    }

    /// The two files, in the order the manifest will list them.
    #[test]
    fn a_scaffolded_block_is_two_files_under_its_own_directory() {
        let files = Template::Table.files("newsletter");
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "blocks/newsletter/Cargo.toml",
                "blocks/newsletter/src/lib.rs"
            ]
        );
    }

    /// Every place the name is load-bearing is rewritten, and nothing else
    /// is — a hyphenated name must still produce compilable Rust.
    #[test]
    fn instantiating_rewrites_the_five_anchors_and_no_identifiers() {
        let files = Template::Table.files("my-shop");
        let cargo = &files[0].1;
        let lib = &files[1].1;
        assert!(cargo.contains(r#"name = "my-shop""#), "{cargo}");
        assert!(lib.contains(r#"Block::new("site/my-shop""#), "{lib}");
        assert!(lib.contains("/b/my-shop/subscribe"), "{lib}");
        assert!(lib.contains("site__my_shop__subscribers"), "{lib}");
        assert!(lib.contains("SITE__MY_SHOP__"), "{lib}");
        assert!(!lib.contains("site__my-shop__"), "{lib}");
        assert!(!lib.contains("SITE__MY-SHOP__"), "{lib}");
        // The handler function names survive: a blanket replace would have
        // produced `fn subscribe_my-shop`, which is not an identifier.
        assert!(lib.contains("fn subscribe("), "{lib}");
        assert!(!lib.contains("site/newsletter"), "{lib}");
        assert!(!lib.contains("site__newsletter__"), "{lib}");

        let hello = Template::Hello.files("my-shop");
        assert!(hello[1].1.contains(r#"Block::new("site/my-shop""#));
        assert!(hello[1].1.contains(r#""/b/my-shop/", hello"#));
        assert!(
            hello[1].1.contains("fn hello("),
            "the handler keeps its name"
        );
    }

    /// The template spellings and the enum agree in both directions.
    #[test]
    fn template_spellings_agree() {
        for template in [Template::Hello, Template::Table] {
            assert_eq!(Template::parse(template.as_str()), Some(template));
            assert_eq!(
                serde_json::to_value(template).expect("serialize"),
                serde_json::json!(template.as_str()),
            );
        }
        assert_eq!(Template::parse("newsletter"), None);
    }

    /// The reference's two long samples ARE the templates, so they cannot
    /// drift from what the scaffolder writes.
    #[test]
    fn the_reference_splices_both_templates_in() {
        let markdown = reference_markdown();
        assert!(!markdown.contains(TEMPLATE_HELLO_MARKER));
        assert!(!markdown.contains(TEMPLATE_TABLE_MARKER));
        assert!(markdown.contains(Template::Hello.lib_rs().trim_end()));
        assert!(markdown.contains(Template::Table.lib_rs().trim_end()));
    }
}
