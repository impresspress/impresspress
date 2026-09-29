//! `impresspress/legalpages`: the public terms/privacy pages and the admin
//! editor, settings and endpoints pages.
//!
//! The editor and settings pages save and preview through `fetch`, not htmx,
//! so no page carries a mutating htmx control today and `must_fire` is empty.
//! The pages are still rendered, so a control added to one of them is fired
//! from then on.

use std::sync::Arc;

use wafer_run::{Block, LifecycleEvent, LifecycleType, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::legalpages::LegalPagesBlock,
    test_support::{
        admin_msg, anon_msg,
        htmx::{Fixture, Page, Site},
        TestContext,
    },
};

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/legalpages",
        fixture: Some(|| Box::pin(fixture())),
        exempt: &[
            ("/b/legalpages/api/documents", Exempt::JsonApi),
            ("/b/legalpages/api/documents/{id}", Exempt::JsonApi),
        ],
        // No mutating htmx control on any legalpages page; see the module doc.
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[],
    }
}

/// The public pages are read by anyone, so they are fetched signed out; the
/// editor is the admin's.
fn caller(action: &str, path: &str) -> Message {
    if path.starts_with("/b/legalpages/admin") {
        admin_msg(action, path)
    } else {
        anon_msg(action, path)
    }
}

/// The block's own `Init`: its migrations, and the two default documents it
/// publishes — so the public pages render a published document and the
/// editor opens on one.
async fn fixture() -> Fixture {
    let ctx = TestContext::with_admin().await;
    // Init runs in the block's own frame.
    LegalPagesBlock::new()
        .lifecycle(
            &ctx.clone().running_as(LegalPagesBlock::BLOCK_NAME),
            LifecycleEvent {
                event_type: LifecycleType::Init,
                data: Vec::new(),
            },
        )
        .await
        .expect("legalpages Init");

    // A document Init published, for the by-id row to read.
    let listed = crate::test_support::htmx::answer(
        LegalPagesBlock::new()
            .handle(
                &ctx.clone().running_as(LegalPagesBlock::BLOCK_NAME),
                admin_msg("retrieve", "/b/legalpages/api/documents"),
                wafer_run::InputStream::empty(),
            )
            .await,
    )
    .await;
    let listed: serde_json::Value = serde_json::from_str(&listed.body).expect("the list is JSON");
    let document = listed["records"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("Init published a document: {listed}"))
        .to_string();

    Fixture {
        ctx,
        site: Site(vec![Arc::new(LegalPagesBlock::new()) as Arc<dyn Block>]),
        caller,
        pages: vec![
            Page::at("/b/legalpages/terms"),
            Page::at("/b/legalpages/privacy"),
            Page::at("/b/legalpages/admin"),
            Page::at("/b/legalpages/admin/privacy"),
            Page::at("/b/legalpages/admin/terms"),
            Page::at("/b/legalpages/admin/settings"),
            Page::at("/b/legalpages/admin/endpoints"),
        ],
        probes: vec![(
            "/b/legalpages/api/documents/{id}",
            format!("/b/legalpages/api/documents/{document}"),
        )],
        operator_input: &[],
    }
}
