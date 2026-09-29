//! `impresspress/messages`: the context list and one context's detail page,
//! over one conversation the admin owns and one task context, so both entry
//! composers render.

use std::{future::Future, pin::Pin, sync::Arc};

use wafer_run::{Block, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::messages::{test_support::ctx_with_messages, MessagesBlock},
    test_support::{
        admin_msg,
        htmx::{Fixture, Page, Site},
        output_json,
    },
};

/// What an operator types into the new-context form and the composers.
const OPERATOR_INPUT: &[(&str, &str)] = &[("title", "Probe typed"), ("content", "probe typed")];

fn fixture() -> Pin<Box<dyn Future<Output = Fixture>>> {
    Box::pin(async {
        let ctx = ctx_with_messages().await;
        let messages: Arc<dyn Block> = Arc::new(MessagesBlock::new());
        let mut ids = Vec::new();
        for kind in ["conversation", "task"] {
            let created = output_json(
                messages
                    .handle(
                        &ctx,
                        admin_msg("create", "/b/messages/api/contexts"),
                        wafer_run::InputStream::from_bytes(
                            serde_json::to_vec(&serde_json::json!({
                                "type": kind,
                                "title": format!("Probe {kind}"),
                            }))
                            .expect("encode"),
                        ),
                    )
                    .await,
            )
            .await;
            ids.push(created["id"].as_str().expect("context id").to_string());
        }

        // One entry in the first context, for the entry row to read.
        let entry = output_json(
            messages
                .handle(
                    &ctx,
                    admin_msg(
                        "create",
                        &format!("/b/messages/api/contexts/{}/entries", ids[0]),
                    ),
                    wafer_run::InputStream::from_bytes(
                        serde_json::to_vec(&serde_json::json!({"content": "Probe entry"}))
                            .expect("encode"),
                    ),
                )
                .await,
        )
        .await;
        let entry_id = entry["id"]
            .as_str()
            .unwrap_or_else(|| panic!("the entry was created: {entry}"))
            .to_string();

        let mut pages = vec![Page::at("/b/messages/")];
        pages.extend(
            ids.iter()
                .map(|id| Page::at(format!("/b/messages/contexts/{id}"))),
        );
        Fixture {
            ctx,
            site: Site(vec![messages]),
            caller: admin_caller,
            pages,
            probes: vec![
                (
                    "/b/messages/api/contexts/{id}",
                    format!("/b/messages/api/contexts/{}", ids[0]),
                ),
                (
                    "/b/messages/api/contexts/{id}/entries",
                    format!("/b/messages/api/contexts/{}/entries", ids[0]),
                ),
                (
                    "/b/messages/api/entries/{id}",
                    format!("/b/messages/api/entries/{entry_id}"),
                ),
            ],
            operator_input: OPERATOR_INPUT,
        }
    })
}

fn admin_caller(action: &str, path: &str) -> Message {
    admin_msg(action, path)
}

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/messages",
        fixture: Some(fixture),
        exempt: &[
            ("/b/messages/api/contexts/{id}", Exempt::JsonApi),
            ("/b/messages/api/contexts/{id}/entries", Exempt::JsonApi),
            ("/b/messages/api/entries/{id}", Exempt::JsonApi),
        ],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[
            "create /b/messages/api/contexts",
            "create /b/messages/api/contexts/{id}/entries",
        ],
    }
}
