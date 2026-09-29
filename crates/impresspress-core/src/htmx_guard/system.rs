//! `impresspress/system`: the health probe and the shared static assets. It
//! serves no page; the fixture is the context its `GET` rows are dispatched
//! in.

use std::sync::Arc;

use wafer_run::{Block, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::system::SystemBlock,
    test_support::{
        anon_msg,
        htmx::{Fixture, Site},
        TestContext,
    },
};

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/system",
        fixture: Some(fixture),
        exempt: &[
            (
                "/health",
                Exempt::NotAPage("liveness probe answered in plain text"),
            ),
            ("/b/static/{filename}", Exempt::Asset),
        ],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[],
    }
}

/// Every row is public: a visitor with no session.
fn caller(action: &str, path: &str) -> Message {
    anon_msg(action, path)
}

fn fixture() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture>>> {
    Box::pin(async {
        Fixture {
            ctx: TestContext::new().await,
            site: Site(vec![Arc::new(SystemBlock::new()) as Arc<dyn Block>]),
            caller,
            pages: Vec::new(),
            // The favicon, by the content-hashed name the pages link it by.
            probes: vec![(
                "/b/static/{filename}",
                format!(
                    "/b/static/{}",
                    crate::ui::assets::entry("favicon.ico").filename
                ),
            )],
            operator_input: &[],
        }
    })
}
