//! `impresspress/llm`: the chat page (with and without a thread), and the
//! providers, models and settings pages, over a deployment that has one
//! provider row, one model, one thread and one per-thread override — so every
//! per-record control renders.

use std::{future::Future, pin::Pin, sync::Arc};

use wafer_core::clients::llm::ModelInfo;
use wafer_run::{Block, Message};

use super::Entry;
use crate::{
    blocks::{
        llm::{
            repo,
            routes::test_support::{RecordingProviderAdmin, StubLlmServiceBlock},
            LlmBlock,
        },
        messages::{test_support::ctx_with_messages, MessagesBlock},
    },
    test_support::{
        admin_msg,
        htmx::{answer, Fixture, Page, Site},
        output_json,
    },
};

/// What an operator types into the add-provider form.
const OPERATOR_INPUT: &[(&str, &str)] = &[
    ("name", "probe-typed-provider"),
    ("endpoint", "https://probe.example/v1"),
];

fn fixture() -> Pin<Box<dyn Future<Output = Fixture>>> {
    Box::pin(async {
        // The fixture's own frame; each step below runs as the block whose
        // code it is.
        let mut ctx = ctx_with_messages().await.fixture();
        let as_llm =
            |ctx: &crate::test_support::TestContext| ctx.clone().running_as(LlmBlock::BLOCK_NAME);
        let sqlite: Vec<&str> = crate::blocks::llm::migrations::SQLITE_MIGRATIONS
            .iter()
            .map(|(_, sql)| *sql)
            .collect();
        crate::migration_helper::apply_migrations(
            &as_llm(&ctx),
            LlmBlock::BLOCK_NAME,
            &sqlite,
            crate::blocks::llm::migrations::POSTGRES_MIGRATIONS,
        )
        .await
        .expect("apply llm migrations");
        ctx.register_block(
            "wafer-run/llm",
            Arc::new(StubLlmServiceBlock {
                models: vec![ModelInfo::new("probe-provider", "probe-model", "Probe")],
                ..Default::default()
            }),
        );
        let llm: Arc<dyn Block> =
            Arc::new(LlmBlock::new(Arc::new(RecordingProviderAdmin::default())));
        let messages: Arc<dyn Block> = Arc::new(MessagesBlock::new());

        // One provider row, through the route an SDK caller uses.
        let mut create = admin_msg("create", "/b/llm/api/providers");
        create.set_meta("http.header.content-type", "application/json");
        let created = answer(
            llm.handle(
                &as_llm(&ctx),
                create,
                wafer_run::InputStream::from_bytes(
                    serde_json::to_vec(&serde_json::json!({
                        "name": "probe-provider",
                        "protocol": "open_ai",
                        "endpoint": "https://probe.example/v1",
                        "models": ["probe-model"],
                    }))
                    .expect("encode"),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(created.status, 200, "seed provider: {}", created.body);

        // One thread the admin owns, and an override for it.
        let thread = output_json(
            messages
                .handle(
                    &ctx.clone().running_as(MessagesBlock::BLOCK_NAME),
                    admin_msg("create", "/b/messages/api/contexts"),
                    wafer_run::InputStream::from_bytes(
                        serde_json::to_vec(&serde_json::json!({
                            "type": "conversation",
                            "title": "Probe thread",
                        }))
                        .expect("encode"),
                    ),
                )
                .await,
        )
        .await;
        let thread_id = thread["id"].as_str().expect("thread id").to_string();
        repo::settings::insert(&as_llm(&ctx), &thread_id, "probe-provider", "probe-model")
            .await
            .expect("seed override");

        Fixture {
            ctx,
            site: Site(vec![llm, messages]),
            caller: admin_caller,
            pages: vec![
                Page::at("/b/llm/"),
                Page::at(format!("/b/llm/threads/{thread_id}")),
                Page::at("/b/llm/providers"),
                Page::at("/b/llm/models"),
                Page::at("/b/llm/settings"),
            ],
            probes: vec![(
                "/b/llm/api/models/{backend_id}/{model_id}/status",
                "/b/llm/api/models/probe-provider/probe-model/status".to_string(),
            )],
            operator_input: OPERATOR_INPUT,
        }
    })
}

fn admin_caller(action: &str, path: &str) -> Message {
    admin_msg(action, path)
}

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/llm",
        fixture: Some(fixture),
        exempt: &[],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[
            "create /b/llm/api/providers",
            "create /b/llm/api/providers/{id}/discover-models",
            "delete /b/llm/api/providers/{id}",
            "create /b/llm/api/models/{backend_id}/{model_id}/load",
            "create /b/llm/api/models/{backend_id}/{model_id}/unload",
            "delete /b/llm/api/config/{id}",
        ],
    }
}
