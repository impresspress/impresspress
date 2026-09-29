//! `impresspress/signal`: the WebRTC signalling relay. Every `GET` row
//! publishes a response schema, so none is a page; the fixture is the context
//! those rows are dispatched in, and has no page to render.

use std::sync::Arc;

use wafer_run::{Block, InputStream, Message};

use super::Entry;
use crate::{
    blocks::signal::SignalBlock,
    test_support::{
        anon_msg,
        htmx::{answer, Fixture, Site},
        TestContext,
    },
};

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/signal",
        fixture: Some(fixture),
        exempt: &[],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[],
    }
}

/// The room code the fixture opens.
const ROOM: &str = "AB2CD3";

/// Every row is public: a visitor with no session.
fn caller(action: &str, path: &str) -> Message {
    anon_msg(action, path)
}

fn fixture() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture>>> {
    Box::pin(async {
        let ctx = TestContext::with_signal().await;
        let block: Arc<dyn Block> = Arc::new(SignalBlock::new());
        // A room with the host's offer up and no answer yet: the guest's read
        // finds the offer, the host's poll answers its waiting state.
        let opened = answer(
            block
                .handle(
                    &ctx,
                    anon_msg("create", &format!("/b/signal/rooms/{ROOM}/offer")),
                    InputStream::from_bytes(br#"{"sdp":"v=0"}"#.to_vec()),
                )
                .await,
        )
        .await;
        assert_eq!(opened.status, 200, "open the room: {}", opened.body);
        Fixture {
            ctx,
            site: Site(vec![block]),
            caller,
            pages: Vec::new(),
            probes: vec![
                (
                    "/b/signal/rooms/{code}/offer",
                    format!("/b/signal/rooms/{ROOM}/offer"),
                ),
                (
                    "/b/signal/rooms/{code}/answer",
                    format!("/b/signal/rooms/{ROOM}/answer"),
                ),
            ],
            operator_input: &[],
        }
    })
}
