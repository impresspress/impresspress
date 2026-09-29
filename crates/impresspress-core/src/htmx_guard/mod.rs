//! The htmx guard, over every block this build registers.
//!
//! [`crate::test_support::htmx`] fires every mutating htmx control a page
//! emits and holds the answer to what htmx does with it. That is only as good
//! as the set of pages it is pointed at, so this module decides the set, and
//! refuses to let it be incomplete:
//!
//! - every block in the build has an [`Entry`] — a block this module does not
//!   know is a failure, not out of scope;
//! - every `GET` row a block declares is one of its pages, publishes a
//!   response schema (so it is JSON, not a page), or is exempt with a stated
//!   reason — a new page or tab cannot escape the guard by not being listed;
//! - every `GET` row that is not a page is dispatched once, at seeded ids,
//!   and must succeed without answering a page: a schema or an exemption is
//!   a claim about the answer, and the answer is what is checked, not the
//!   claim;
//! - every page of every entry, and every tab and view those pages link to,
//!   is rendered, and every control on it fired — and the tabs and views the
//!   crawl reaches are exactly the ones the entry names.

mod auth_ui;
#[cfg(feature = "block-dev")]
mod dev;
mod email;
#[cfg(feature = "block-files")]
mod files;
#[cfg(feature = "block-legalpages")]
mod legalpages;
#[cfg(feature = "block-llm")]
mod llm;
#[cfg(feature = "block-messages")]
mod messages;
#[cfg(feature = "block-products")]
mod products;
#[cfg(feature = "block-signal")]
mod signal;
mod system;
#[cfg(feature = "block-tickets")]
mod tickets;
#[cfg(feature = "block-userportal")]
mod userportal;
#[cfg(feature = "block-vector")]
mod vector;

use std::collections::BTreeSet;

use wafer_run::{BlockInfo, HttpMethod};

use crate::{
    endpoint_match::match_template,
    test_support::htmx::{crawl, fire_every_control, send, MakeFixture},
};

/// Why a `GET` row that is not a page publishes no response schema. Each is
/// checked against the row's answer, not taken on trust.
#[derive(Debug, Clone, Copy)]
enum Exempt {
    /// A JSON endpoint that declares no response schema. No htmx control
    /// reads it and it renders no page, so there is nothing on it for the
    /// guard to fire. It must succeed, and not with HTML.
    JsonApi,
    /// Answered with a redirect to a page the guard renders.
    Redirect,
    /// A static asset (script, stylesheet), not a page. It must succeed, and
    /// not with HTML.
    Asset,
    /// Some other answer that is not a page — a download, a plain-text probe,
    /// a hand-off to an external site — with what it is. It must succeed, not
    /// with HTML, or redirect off the site: a redirect to one of the site's
    /// own pages is an [`Exempt::Redirect`], whose target is checked.
    NotAPage(&'static str),
}

impl Exempt {
    /// What the exemption says the row answers.
    fn reason(self) -> &'static str {
        match self {
            Exempt::JsonApi => "JSON API without a declared schema",
            Exempt::Redirect => "redirect to a page the guard renders",
            Exempt::Asset => "static asset",
            Exempt::NotAPage(what) => what,
        }
    }
}

/// How the guard covers one block.
struct Entry {
    /// The block's `BlockInfo::name`.
    block: &'static str,
    /// Its pages and the context they render in, and the context its `GET`
    /// rows are dispatched in. `None` only for a block that declares no `GET`
    /// row at all, and the completeness check below says so.
    fixture: Option<MakeFixture>,
    /// `GET` rows that are neither a page nor schema'd JSON, each with why.
    exempt: &'static [(&'static str, Exempt)],
    /// `"{action} {template}"` of the controls its pages must keep reaching.
    must_fire: &'static [&'static str],
    /// The URL of every view the crawl reaches beyond the fixture's own
    /// pages — exactly: a view that stops being linked fails, and so does a
    /// new one nobody named.
    must_reach: &'static [&'static str],
    /// Non-page `GET` rows whose success no fixture can reach, each with why.
    /// They are still dispatched, and must still not answer a page; every
    /// other non-page row must succeed.
    cannot_succeed: &'static [(&'static str, &'static str)],
}

#[expect(
    clippy::vec_init_then_push,
    reason = "each push is `#[cfg]`-gated like the block manifest entry it covers, which a \
              `vec![..]` literal cannot express"
)]
fn entries() -> Vec<Entry> {
    let mut entries = Vec::new();
    entries.push(Entry {
        block: "impresspress/admin",
        fixture: Some(crate::blocks::admin::htmx_contract_tests::fixture),
        exempt: &[
            ("/b/admin/api/users/{id}", Exempt::JsonApi),
            ("/b/admin/api/database/info", Exempt::JsonApi),
            ("/b/admin/api/database/tables", Exempt::JsonApi),
            (
                "/b/admin/api/database/tables/{name}/columns",
                Exempt::JsonApi,
            ),
            ("/b/admin/api/iam/permissions", Exempt::JsonApi),
            ("/b/admin/api/iam/user-roles", Exempt::JsonApi),
            ("/b/admin/api/settings/all", Exempt::JsonApi),
            ("/b/admin/api/settings/{key}", Exempt::JsonApi),
            ("/b/admin/settings/", Exempt::Redirect),
            ("/b/admin/variables", Exempt::Redirect),
            ("/b/admin/network", Exempt::Redirect),
            ("/b/admin/email", Exempt::Redirect),
            ("/b/admin/permissions", Exempt::Redirect),
        ],
        must_reach: &[
            "/b/admin/users?tab=roles",
            "/b/admin/users?tab=api-keys",
            "/b/admin/blocks?tab=services",
            "/b/admin/blocks?tab=infrastructure",
            "/b/admin/blocks?tab=custom",
            "/b/admin/database?tab=schema",
            "/b/admin/database?tab=sql",
            "/b/admin/logs?tab=audit",
            "/b/admin/settings/variables?tab=all",
            "/b/admin/settings/permissions?subtab=database",
        ],
        cannot_succeed: &[],
        must_fire: crate::blocks::admin::htmx_contract_tests::MUST_FIRE,
    });
    entries.push(auth_ui::entry());
    entries.push(email::entry());
    entries.push(system::entry());
    #[cfg(feature = "block-dev")]
    entries.push(dev::entry());
    #[cfg(feature = "block-files")]
    entries.push(files::entry());
    #[cfg(feature = "block-legalpages")]
    entries.push(legalpages::entry());
    #[cfg(feature = "block-llm")]
    entries.push(llm::entry());
    #[cfg(feature = "block-messages")]
    entries.push(messages::entry());
    #[cfg(feature = "block-products")]
    entries.push(products::entry());
    #[cfg(feature = "block-signal")]
    entries.push(signal::entry());
    #[cfg(feature = "block-tickets")]
    entries.push(tickets::entry());
    #[cfg(feature = "block-userportal")]
    entries.push(userportal::entry());
    #[cfg(feature = "block-vector")]
    entries.push(vector::entry());
    entries
}

/// The blocks this build registers, by `info()`: the feature-block manifest
/// plus the dev block, which the builder registers on its own.
fn registered() -> Vec<BlockInfo> {
    #[cfg_attr(
        not(feature = "block-dev"),
        expect(
            unused_mut,
            reason = "only the block-dev build adds to the manifest's set"
        )
    )]
    let mut infos = crate::blocks::all_block_infos();
    #[cfg(feature = "block-dev")]
    infos.push(wafer_run::Block::info(
        &crate::blocks::dev::DevBlock::with_workspace(crate::blocks::dev::DevShared::new(
            crate::blocks::dev::test_support::FakeControl::new(),
            std::sync::Arc::new(crate::blocks::dev::test_support::FakeShell::new()),
        )),
    ));
    infos
}

#[test]
fn every_registered_block_has_an_entry() {
    let entries = entries();
    let known: BTreeSet<&str> = entries.iter().map(|e| e.block).collect();
    let registered: Vec<String> = registered().into_iter().map(|i| i.name).collect();
    let unknown: Vec<&String> = registered
        .iter()
        .filter(|name| !known.contains(name.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "blocks with no htmx guard entry: {unknown:?} — give each one an `Entry`"
    );
    let dead: Vec<&&str> = known
        .iter()
        .filter(|name| !registered.iter().any(|r| r == **name))
        .collect();
    assert!(
        dead.is_empty(),
        "entries for blocks this build does not register: {dead:?}"
    );
}

#[tokio::test]
async fn every_get_row_is_a_page_or_json_or_exempt() {
    let infos = registered();
    let mut gaps: Vec<String> = Vec::new();
    for entry in entries() {
        let Some(info) = infos.iter().find(|i| i.name == entry.block) else {
            continue;
        };
        let pages: Vec<String> = match entry.fixture {
            Some(make) => make().await.pages.into_iter().map(|p| p.path).collect(),
            None => Vec::new(),
        };
        let gets: Vec<_> = info
            .endpoints
            .iter()
            .filter(|e| e.method == HttpMethod::Get)
            .collect();
        if entry.fixture.is_none() && !gets.is_empty() {
            gaps.push(format!(
                "{}: declares GET rows but has no fixture to dispatch them in",
                entry.block
            ));
        }
        for row in &gets {
            let is_page = pages.iter().any(|path| is_row_of(&row.path, path));
            let exempt = entry.exempt.iter().any(|(t, _)| *t == row.path);
            if row.output_schema.is_some() {
                if exempt {
                    gaps.push(format!(
                        "{}: GET {} publishes a schema; its exemption is redundant",
                        entry.block, row.path
                    ));
                }
                if is_page {
                    gaps.push(format!(
                        "{}: GET {} publishes a schema and is a page the guard renders",
                        entry.block, row.path
                    ));
                }
                continue;
            }
            if is_page && exempt {
                gaps.push(format!(
                    "{}: GET {} is both a page and exempt",
                    entry.block, row.path
                ));
            }
            if !is_page && !exempt {
                gaps.push(format!(
                    "{}: GET {} is neither a page the guard renders, schema'd JSON, nor exempt",
                    entry.block, row.path
                ));
            }
        }
        for (template, _) in entry.exempt {
            if !gets.iter().any(|row| row.path == *template) {
                gaps.push(format!(
                    "{}: exempt {template} is not a GET row of the block",
                    entry.block
                ));
            }
        }
    }
    assert!(gaps.is_empty(), "{}", gaps.join("\n"));
}

/// Whether the page at `path` is served by the row `template`.
fn is_row_of(template: &str, path: &str) -> bool {
    match_template(template, path).is_some()
        || (!path.ends_with('/') && match_template(template, &format!("{path}/")).is_some())
}

/// Every `GET` row that is not a page — schema'd or exempt — is dispatched
/// through its block as the fixture's visitor, at the URL the fixture's
/// `probes` give it (seeded ids, required query), and must succeed without
/// answering a page. A route whose handler renders HTML under a response
/// schema (or under a JSON / asset / download exemption) would otherwise be
/// accepted on its label, and one that answers 401 or 500 would pass for
/// answering JSON. A redirect must land on a page the crawl renders.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_get_row_that_is_not_a_page_succeeds_without_answering_a_page() {
    let infos = registered();
    let mut wrong: Vec<String> = Vec::new();
    for entry in entries() {
        let Some(info) = infos.iter().find(|i| i.name == entry.block) else {
            continue;
        };
        let Some(make) = entry.fixture else {
            continue;
        };
        let fixture = make().await;
        let rendered: Vec<String> = crawl(&fixture)
            .await
            .iter()
            .map(|view| view.page(&fixture).url())
            .collect();
        let mut probed: BTreeSet<&str> = BTreeSet::new();
        for row in info
            .endpoints
            .iter()
            .filter(|e| e.method == HttpMethod::Get)
        {
            let exempt = entry
                .exempt
                .iter()
                .find(|(t, _)| *t == row.path)
                .map(|(_, why)| *why);
            if row.output_schema.is_none() && exempt.is_none() {
                continue;
            }
            let label = match exempt {
                Some(why) => format!("exempt as a {}", why.reason()),
                None => "publishes a response schema".to_string(),
            };
            let probe = fixture.probes.iter().find(|(t, _)| *t == row.path);
            let url = match probe {
                Some((template, url)) => {
                    probed.insert(template);
                    url.clone()
                }
                None if row.path.contains('{') => {
                    wrong.push(format!(
                        "{}: GET {} has path parameters and the fixture gives it no probe URL",
                        entry.block, row.path
                    ));
                    continue;
                }
                None => row.path.clone(),
            };
            let (_, answer) = send(&fixture, "retrieve", &url, "", false).await;
            let answered = format!(
                "answers {} {}: {}",
                answer.status,
                answer.content_type,
                answer.body.chars().take(200).collect::<String>()
            );
            if answer.content_type.starts_with("text/html") {
                wrong.push(format!(
                    "{}: GET {url} ({label}) is a page: {answered}",
                    entry.block
                ));
                continue;
            }
            if entry.cannot_succeed.iter().any(|(t, _)| *t == row.path) {
                continue;
            }
            let redirect = (300..400).contains(&answer.status);
            let to = answer.location.as_deref().unwrap_or_default();
            let problem = match exempt {
                Some(Exempt::Redirect) if !redirect || !rendered.iter().any(|page| page == to) => {
                    Some(format!(
                        "must redirect to a page the crawl renders ({rendered:?}); location {to:?}"
                    ))
                }
                Some(Exempt::Redirect) => None,
                Some(Exempt::NotAPage(_)) if redirect && !to.starts_with("https://") => {
                    Some(format!(
                        "redirects within the site to {to:?}; an internal redirect is an \
                         Exempt::Redirect, whose target is checked"
                    ))
                }
                Some(Exempt::NotAPage(_)) if redirect => None,
                _ if !(200..300).contains(&answer.status) => Some("must succeed".to_string()),
                _ => None,
            };
            if let Some(problem) = problem {
                wrong.push(format!(
                    "{}: GET {url} ({label}) {answered}: {problem}",
                    entry.block
                ));
            }
        }
        for (template, _) in &fixture.probes {
            if !probed.contains(template) {
                wrong.push(format!(
                    "{}: probe for {template}, which is not a non-page GET row",
                    entry.block
                ));
            }
        }
        for (template, _) in entry.cannot_succeed {
            if !info
                .endpoints
                .iter()
                .any(|e| e.method == HttpMethod::Get && e.path == *template)
            {
                wrong.push(format!(
                    "{}: cannot_succeed {template} is not a GET row of the block",
                    entry.block
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The crawl reaches exactly the views each entry names: the tabs and views
/// the guard exists to render cannot silently drop out of it, and a new one
/// is named the day it is linked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_crawl_reaches_exactly_the_views_each_entry_names() {
    for entry in entries() {
        let Some(make) = entry.fixture else {
            continue;
        };
        let fixture = make().await;
        let own: Vec<String> = fixture.pages.iter().map(|p| p.url()).collect();
        let views: BTreeSet<String> = crawl(&fixture)
            .await
            .iter()
            .map(|view| view.page(&fixture).url())
            .filter(|url| !own.contains(url))
            .collect();
        let named: BTreeSet<String> = entry.must_reach.iter().map(|v| v.to_string()).collect();
        assert_eq!(
            views, named,
            "{}: the views the crawl reached (left) are not the views the entry names (right)",
            entry.block
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_htmx_control_on_every_page_is_answered_the_way_it_swaps() {
    for entry in entries() {
        let Some(make) = entry.fixture else {
            continue;
        };
        let fired = fire_every_control(make).await;
        for expected in entry.must_fire {
            assert!(
                fired.contains(*expected),
                "{}: no page fired {expected}; fired: {fired:#?}",
                entry.block
            );
        }
    }
}
