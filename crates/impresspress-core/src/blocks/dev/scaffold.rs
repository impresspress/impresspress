//! `POST /b/dev/api/blocks` and `GET /b/dev/api/reference` — starting a block,
//! and the guide for writing one.
//!
//! # Why a scaffolder and not a documented file list
//!
//! A block is three files, and one of them —
//! [`templates/wafer_guest.rs`](Template::WAFER_GUEST) — is ~1 500 lines of
//! vendored ABI the author must not write, must not edit, and must have an
//! exact copy of. An agent told to "create `blocks/x/src/wafer_guest.rs` with
//! the contents of the reference" would reproduce it approximately, and the
//! failure would surface as a trap inside wasmi. So the sandbox writes it,
//! byte for byte, from [`include_str!`].
//!
//! The other two files are instantiated from a template: the block's name is
//! its directory, its crate name, its block id (`site/<name>`), its route
//! prefix (`/b/<name>/`) and its collection prefix (`site__<name>__`, a
//! hyphen spelled `_`) all at once, and a template that got any one of those
//! wrong would be refused by validation with a diagnostic the author did not
//! cause.
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
    blobs,
    contracts::{CreateBlockRequest, CreateBlockResponse, FileConflict, ReferenceResponse},
    files, no_store, no_store_db_error_internal, no_store_error,
    paths::{self, WorkspaceArea, BLOCK_NAME_RULE},
    validation, workspace, DevShared, WAFER_GUEST_VERSION,
};
use crate::blocks::crud;

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
    /// The vendored support module, byte for byte.
    ///
    /// `include_str!` of the canonical file. The templates' own
    /// `src/wafer_guest.rs` are symlinks to that same file, so the copy this
    /// endpoint writes, the copy the golden test compiles, and the copy the
    /// reference documents cannot be three different things.
    pub const WAFER_GUEST: &'static str = include_str!("templates/wafer_guest.rs");

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

    /// The three files a block starts as, workspace-relative, in path order.
    ///
    /// `wafer_guest.rs` is written verbatim: nothing in it names the block,
    /// and rewriting it would break the byte-for-byte identity the version
    /// check depends on.
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
            (
                format!("{}{name}/src/wafer_guest.rs", workspace::BLOCKS_PREFIX),
                Template::WAFER_GUEST.to_string(),
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

/// `POST /b/dev/api/blocks` — write a new block's three files.
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

    // Refuse if ANY path under `blocks/<name>/` is taken, not just the three
    // this would write: a directory that holds a stray file is a block the
    // author started, and overwriting two of its three files would leave a
    // crate that is neither what they wrote nor what the template is.
    let prefix = format!("{}{}/", workspace::BLOCKS_PREFIX, request.name);
    if let Some(existing) = ws.files.keys().find(|path| path.starts_with(&prefix)) {
        return no_store()
            .status(409)
            .json(&FileConflict::new(existing, ws.get(existing)));
    }

    // EVERY file's quota is checked before ANY of them is stored. Checking
    // one and storing it before checking the next means a refusal on the
    // second returns without ever reaching `workspace::save` below — leaving
    // the first one's blob in the store while the `record_blob_stored` that
    // charges for it is discarded with `ws`. `check_quotas` bounds on
    // `blob_bytes` precisely because a blob no entry names still occupies the
    // author's storage, so each such refusal would open a hole in the
    // accounting that stays open until the next collection resets the
    // counters from the store (`super::gc`) — and a `blocks/` write triggers
    // no collection, so a caller sitting on the limit could widen it past
    // `MAX_WORKSPACE_BYTES` by retrying. `files::handle_write` cannot reach
    // this shape — it writes one file — and nothing forces the interleave
    // here: every size and hash is known before the first store.
    //
    // `planned` is checked against a projection rather than against `ws`, so
    // each file is counted on top of the ones ahead of it in this same
    // request — including their content, which is what makes two identical
    // template files cost what the store will actually charge for them.
    let mut planned = Vec::with_capacity(files.len());
    let mut projected = ws.clone();
    for (path, content) in &files {
        let bytes = content.as_bytes();
        let sha = blobs::sha256_hex(bytes);
        // What the blob store would grow by. Content some entry already
        // names is certainly stored, so writing the same `wafer_guest.rs`
        // into a second block costs nothing — which is the common case.
        let new_blob_bytes = if projected.references(&sha) {
            0
        } else {
            bytes.len() as u64
        };
        if let Err(e) = files::check_quotas(
            &projected,
            path,
            &WorkspaceArea::Block(request.name.clone()),
            new_blob_bytes,
        ) {
            return e.into_response();
        }
        projected.insert(path, sha.clone(), bytes.len() as u64);
        if new_blob_bytes > 0 {
            projected.record_blob_stored(new_blob_bytes);
        }
        planned.push((path, sha, bytes));
    }

    // Store, then record — the order `files::handle_write` uses, and for the
    // same reason: a manifest naming a blob that was never written would 500
    // on every later read of that path. Which is also why no entry is
    // inserted until every blob is down: a store that fails on the second
    // file must not leave the first one named by a half-written block.
    for (_, sha, bytes) in &planned {
        match blobs::put_hashed(ctx, sha, bytes).await {
            Ok(blobs::Stored::New) => ws.record_blob_stored(bytes.len() as u64),
            Ok(blobs::Stored::Deduplicated) => {}
            Err(e) => {
                // The blobs already written are charged for even though this
                // request is over and no entry will ever name them. They are
                // in the store, `blob_bytes` is what the store holds, and the
                // next collection frees them and resets the counters from
                // what is left — a workspace saved without them would
                // under-report storage until then.
                if let Err(save) = workspace::save(ctx, &ws).await {
                    tracing::error!(
                        error = %save,
                        "dev workspace: a blob write failed and the bytes already stored \
                         could not be recorded — blob_bytes under-reports the store until the \
                         next collection"
                    );
                }
                return no_store_db_error_internal(e, "dev workspace blob write");
            }
        }
    }
    let written: Vec<_> = planned
        .into_iter()
        .map(|(path, sha, bytes)| ws.insert(path, sha, bytes.len() as u64))
        .collect();
    if let Err(e) = workspace::save(ctx, &ws).await {
        return no_store_db_error_internal(e, "dev workspace save");
    }

    no_store().json(&CreateBlockResponse {
        name: request.name,
        files: written,
    })
}

/// `GET /b/dev/api/reference` — the authoring guide.
pub async fn handle_reference(_ctx: &dyn Context) -> OutputStream {
    no_store().json(&ReferenceResponse {
        wafer_guest_version: WAFER_GUEST_VERSION,
        markdown: reference_markdown(),
        wafer_guest_module: Template::WAFER_GUEST.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three files, in the order the manifest will list them.
    #[test]
    fn a_scaffolded_block_is_three_files_under_its_own_directory() {
        let files = Template::Table.files("newsletter");
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "blocks/newsletter/Cargo.toml",
                "blocks/newsletter/src/lib.rs",
                "blocks/newsletter/src/wafer_guest.rs",
            ]
        );
        // The support module is written verbatim — a rewritten copy would no
        // longer be the version its `WAFER_GUEST_VERSION` line claims.
        assert_eq!(files[2].1, Template::WAFER_GUEST);
        assert!(files[2]
            .1
            .contains(&format!("WAFER_GUEST_VERSION: u32 = {WAFER_GUEST_VERSION}")));
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
