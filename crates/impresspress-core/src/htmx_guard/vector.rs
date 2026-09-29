//! `impresspress/vector`: the index list and one index's detail page, over a
//! deployment with one index, served by a scripted `wafer-run/vector`.

use std::{future::Future, pin::Pin, sync::Arc};

use wafer_run::{Block, Message};

use super::Entry;
use crate::{
    blocks::vector::{service, test_support::StubVectorBlock, VectorBlock},
    test_support::{
        admin_msg,
        htmx::{answer, Fixture, Page, Site},
        TestContext,
    },
};

/// What an operator types into the create-index form.
const OPERATOR_INPUT: &[(&str, &str)] = &[("name", "probe_typed")];

fn fixture() -> Pin<Box<dyn Future<Output = Fixture>>> {
    Box::pin(async {
        let mut ctx = TestContext::with_vector().await;
        ctx.register_block(
            "wafer-run/vector",
            Arc::new(StubVectorBlock {
                indexes: vec![service::prefixed_index_name("docs")],
                ..Default::default()
            }),
        );
        let vector: Arc<dyn Block> = Arc::new(VectorBlock::new());

        // The index's registry row, through the route that writes it.
        let mut create = admin_msg("create", "/b/vector/api/indexes");
        create.set_meta("http.header.content-type", "application/json");
        let created = answer(
            vector
                .handle(
                    &ctx,
                    create,
                    wafer_run::InputStream::from_bytes(
                        serde_json::to_vec(&serde_json::json!({"name": "docs"})).expect("encode"),
                    ),
                )
                .await,
        )
        .await;
        assert_eq!(created.status, 200, "seed index: {}", created.body);

        Fixture {
            ctx,
            site: Site(vec![vector]),
            caller: admin_caller,
            pages: vec![Page::at("/b/vector/"), Page::at("/b/vector/docs/")],
            probes: Vec::new(),
            operator_input: OPERATOR_INPUT,
        }
    })
}

fn admin_caller(action: &str, path: &str) -> Message {
    admin_msg(action, path)
}

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/vector",
        fixture: Some(fixture),
        exempt: &[],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &["create /b/vector/api/indexes"],
    }
}
