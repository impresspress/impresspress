//! Dropping the content type a sandbox used to store beside each file.
//!
//! A file's content type is a function of its path
//! ([`FileEntry::content_type`](super::workspace::FileEntry::content_type)),
//! taken from the one table every server path uses. Builds before that
//! stored a copy on every entry, in `workspace.json` and in each generation's
//! site manifest, and published each file with that copy. Once the table
//! changed (`.md` became `text/markdown`, `.json` and `.svg` gained a
//! charset, `.xml` and `.csv` were typed), the copies were stale. Such a
//! sandbox would also no longer load at all: a file entry refuses a key it
//! does not know.
//!
//! [`upgrade`] runs on every boot, before anything reads either store, and
//! leaves no copy behind. It runs in three steps, ordered so that an upgrade
//! interrupted at any point is finished by the next boot:
//!
//! 1. `workspace.json` is rewritten without the key. This step depends on
//!    nothing and gates nothing, so it runs first: whatever happens to the
//!    steps below, the workspace loads, and the files API works.
//! 2. If the active generation's row still stores types, its site is
//!    published in full. A publish writes only changed files, and these are
//!    unchanged, so the folder would otherwise keep serving the stored types.
//! 3. Every row that still stores types is rewritten without them: the same
//!    canonical site manifest minus one key per entry, and the hash
//!    recomputed over the manifest the row now denotes. The active row is
//!    rewritten only once step 2 has succeeded. Its stored types are what
//!    tells the next boot that the folder still has to be published, so a
//!    publish that fails (a blob gone missing, say) leaves that one row as it
//!    was, and the next boot tries again. Every other row is rewritten
//!    regardless.
//!
//! A failed publish is still reported as an error, after step 3 has run.
//!
//! # What it costs
//!
//! On a boot with nothing to upgrade: one query that matches no row
//! ([`generations::list_with_stored_content_types`]), and one read of
//! `workspace.json`, checked as bytes before anything is parsed. The
//! journal and the active row are read only when some row matched.

use serde_json::Value;
use wafer_core::clients::storage;
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    contracts::SiteManifest,
    generation, publisher,
    repo::{self, generations},
    workspace::{self, Workspace},
};

/// The key every stored entry used to carry.
const KEY: &str = "content_type";

/// What [`upgrade`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Upgrade {
    /// The active generation's site was published again, every file of it.
    pub republished: bool,
    /// `workspace.json` was rewritten.
    pub workspace: bool,
    /// How many generation rows were rewritten.
    pub generations: usize,
}

impl Upgrade {
    /// Whether anything was stored the old way.
    pub fn changed_anything(&self) -> bool {
        self.republished || self.workspace || self.generations > 0
    }
}

/// Drop every stored content type and publish the active site with the
/// derived ones. See the module docs for the order and the cost.
///
/// An `Err` from the publish comes back after the workspace and every other
/// row have been upgraded.
pub async fn upgrade(ctx: &dyn Context) -> Result<Upgrade, WaferError> {
    // 1. The workspace.
    let mut done = Upgrade {
        workspace: upgrade_workspace(ctx).await?,
        ..Upgrade::default()
    };

    if generations::list_with_stored_content_types(ctx)
        .await?
        .is_empty()
    {
        return Ok(done);
    }

    // 2. The published folder, while the active row still says it is stale.
    let mut republish = Ok(());
    let mut held = None;
    let state = repo::runtime_state::read(ctx).await?;
    if let Some(id) = state.active_generation_id.as_deref() {
        let active = generations::get(ctx, id).await?;
        if stores_types(active.site_manifest_json.as_bytes()) {
            match republish_without_types(ctx, &active).await {
                Ok(()) => done.republished = true,
                Err(e) => {
                    held = Some(active.id);
                    republish = Err(e);
                }
            }
        }
    }

    // 3. The rows, a page at a time: each rewritten row stops matching, and
    // only the held one can be left.
    loop {
        let page = generations::list_with_stored_content_types(ctx).await?;
        let todo: Vec<_> = page
            .iter()
            .filter(|row| Some(&row.id) != held.as_ref())
            .collect();
        if todo.is_empty() {
            break;
        }
        for row in todo {
            let site = site_without_types(row)?;
            let site_json = generation::canonical_text(&site)?;
            let mut rewritten = row.clone();
            rewritten.site_manifest_json = site_json;
            let manifest = generation::from_row(&rewritten)?;
            let sha = generation::manifest_sha256(&manifest)?;
            generations::replace_site_manifest(ctx, &row.id, &rewritten.site_manifest_json, &sha)
                .await?;
            done.generations += 1;
        }
    }
    republish.map(|()| done)
}

/// Publish `active`'s site in full, with the types its paths derive.
async fn republish_without_types(
    ctx: &dyn Context,
    active: &generations::GenerationRow,
) -> Result<(), WaferError> {
    let site = site_without_types(active)?;
    publisher::publish_site(ctx, None, &site).await?;
    Ok(())
}

/// One row's site manifest with the stored types dropped.
fn site_without_types(row: &generations::GenerationRow) -> Result<SiteManifest, WaferError> {
    let mut value: Value = serde_json::from_str(&row.site_manifest_json)
        .map_err(|e| unreadable(&format!("generation {}", row.id), &e))?;
    if let Some(Value::Array(files)) = value.get_mut("files") {
        drop_types(files.iter_mut());
    }
    serde_json::from_value(value).map_err(|e| unreadable(&format!("generation {}", row.id), &e))
}

/// Rewrite `workspace.json` without stored types, when it has any.
async fn upgrade_workspace(ctx: &dyn Context) -> Result<bool, WaferError> {
    let bytes = match storage::get(ctx, workspace::FOLDER, workspace::KEY).await {
        Ok((bytes, _info)) => bytes,
        Err(e) if e.code == ErrorCode::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if !stores_types(&bytes) {
        return Ok(false);
    }
    let mut value: Value =
        serde_json::from_slice(&bytes).map_err(|e| unreadable(workspace::KEY, &e))?;
    if let Some(Value::Object(files)) = value.get_mut("files") {
        drop_types(files.values_mut());
    }
    let ws: Workspace =
        serde_json::from_value(value).map_err(|e| unreadable(workspace::KEY, &e))?;
    workspace::save(ctx, &ws).await?;
    Ok(true)
}

/// Whether serialized entries carry the key. The same test the row query
/// makes, for the same reason: only a key is followed by `:`.
fn stores_types(bytes: &[u8]) -> bool {
    let needle = format!("\"{KEY}\":");
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// Remove the key from every entry object.
fn drop_types<'a>(entries: impl Iterator<Item = &'a mut Value>) {
    for entry in entries {
        if let Value::Object(fields) = entry {
            fields.remove(KEY);
        }
    }
}

fn unreadable(what: &str, e: &serde_json::Error) -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        format!("dropping the stored content types: {what} did not parse: {e}"),
    )
}
