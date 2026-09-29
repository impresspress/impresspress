//! `impresspress/dev`: the dev sandbox page and its JSON workspace API.

use std::sync::Arc;

use wafer_run::{Block, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::dev::{
        test_support::{dev_get, dev_post, FakeControl},
        DevBlock,
    },
    test_support::{
        admin_msg,
        htmx::{Fixture, Page, Site},
        output_json, TestContext,
    },
};

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/dev",
        fixture: Some(fixture),
        exempt: &[
            ("/b/dev/static/dev.js", Exempt::Asset),
            ("/b/dev/static/dev.css", Exempt::Asset),
            ("/b/dev/static/compiler-adapter.js", Exempt::Asset),
            ("/b/dev/api/tools.json", Exempt::JsonApi),
            (
                "/b/dev/api/export",
                Exempt::NotAPage("a site-export archive download"),
            ),
        ],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[],
    }
}

fn caller(action: &str, path: &str) -> Message {
    admin_msg(action, path)
}

fn fixture() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture>>> {
    Box::pin(async {
        let ctx = TestContext::with_dev(FakeControl::new()).await;
        // One site file, so a generation is active: what the export rows
        // export, and the generation the by-id row reads.
        let written = output_json(
            dev_post(
                &ctx,
                "/b/dev/api/files/write",
                serde_json::json!({
                    "path": "site/index.html",
                    "content": "<h1>hi</h1>",
                    "expected_sha256": null,
                }),
            )
            .await,
        )
        .await;
        assert!(written["sha256"].is_string(), "seed a site file: {written}");
        let generations = output_json(dev_get(&ctx, "/b/dev/api/generations").await).await;
        let generation = generations["generations"][0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("the write made a generation: {generations}"))
            .to_string();
        let block = Arc::new(DevBlock::with_workspace(ctx.dev_shared()));
        Fixture {
            ctx,
            site: Site(vec![block as Arc<dyn Block>]),
            caller,
            pages: vec![Page::at("/b/dev")],
            probes: vec![(
                "/b/dev/api/generations/{id}",
                format!("/b/dev/api/generations/{generation}"),
            )],
            operator_input: &[],
        }
    })
}
