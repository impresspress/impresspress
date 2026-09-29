//! The admin block's fixture for the repo-wide htmx guard
//! (`crate::htmx_guard`, over `test_support::htmx`), and the two admin
//! controls worth a named test of their own.

use std::sync::Arc;

use wafer_run::{Block, Message};

use super::{
    page_link_tests::{seeded_ctx, PAGES, PROBE_VARIABLE},
    AdminBlock,
};
use crate::{
    blocks::auth_ui::AuthUiBlock,
    test_support::{
        admin_msg,
        htmx::{
            self, encoded_fields, mutating_controls, send, Answer, Control, Fixture, Page, Site,
        },
    },
};

/// What an operator types or picks for a field the admin pages render empty.
///
/// `resource` is the one field no operator types: it is a hidden input the
/// grant modal's script fills from the three selects above it, and this is
/// the value that script produces for "the probe block's `things` table".
const OPERATOR_INPUT: &[(&str, &str)] = &[
    ("name", "probe-typed-name"),
    ("key", "PROBE_TYPED_SETTING"),
    ("grantee", "impresspress/probe"),
    ("resource", "impresspress__probe__things"),
];

/// Every admin page, over `page_link_tests`' seeded fixture, as the site
/// admin. The pages post to this block and, for API keys, to auth-ui.
pub(crate) fn fixture() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture>>> {
    Box::pin(async {
        let (ctx, seeds) = seeded_ctx().await;
        Fixture {
            ctx,
            site: Site(vec![
                Arc::new(AdminBlock::new()) as Arc<dyn Block>,
                Arc::new(AuthUiBlock::new()),
            ]),
            caller: admin_caller,
            pages: PAGES
                .iter()
                .map(|(_, path, query)| {
                    query.iter().fold(Page::at(*path), |page, (name, value)| {
                        page.with(*name, *value)
                    })
                })
                .collect(),
            probes: vec![
                (
                    "/b/admin/api/users/{id}",
                    format!("/b/admin/api/users/{}", seeds.user_id),
                ),
                (
                    "/b/admin/api/database/tables/{name}/columns",
                    format!(
                        "/b/admin/api/database/tables/{}/columns",
                        super::ROLES_TABLE
                    ),
                ),
                (
                    "/b/admin/api/settings/{key}",
                    format!("/b/admin/api/settings/{PROBE_VARIABLE}"),
                ),
            ],
            operator_input: OPERATOR_INPUT,
        }
    })
}

fn admin_caller(action: &str, path: &str) -> Message {
    admin_msg(action, path)
}

/// The rows the admin pages must keep reaching: each carries a request body
/// or swaps its answer, so a page that stops rendering one must not let the
/// guard pass by firing less.
pub(crate) const MUST_FIRE: &[&str] = &[
    "create /b/admin/users/{id}/disable",
    "create /b/admin/users/{id}/enable",
    "delete /b/admin/users/{id}",
    "create /b/admin/iam/roles",
    "delete /b/admin/iam/roles/{id}",
    "create /b/admin/api-keys/{id}/revoke",
    "create /b/auth/api/api-keys",
    "create /b/admin/blocks/{name}/toggle",
    "create /b/admin/database/query",
    "create /b/admin/variables",
    "update /b/admin/variables/{key}",
    "delete /b/admin/variables/{key}",
    "create /b/admin/variables/{key}/reset-to-environment",
    "create /b/admin/variables/reset-pinned-at-upgrade",
    "create /b/admin/grants/rules",
    "delete /b/admin/grants/rules/{id}",
];

/// The controls on `page`, rendered over `fixture`.
async fn controls_on(fixture: &Fixture, page: Page) -> Vec<Control> {
    mutating_controls(&htmx::render(fixture, &page).await)
}

/// Fire `control` over `fixture` the way htmx does.
async fn fire(fixture: &Fixture, control: &Control) -> Answer {
    let fields = encoded_fields(control, fixture.operator_input);
    send(fixture, control.action, &control.url, &fields, true)
        .await
        .1
}

/// The API-keys tab's Revoke button swaps its answer into
/// `#users-tab-content`. It posted to auth-ui's
/// `PATCH /b/auth/api/api-keys/{id}`, which answers
/// `{"message": "API key revoked"}` — so revoking a key replaced the key table
/// with that JSON as text. Its answer is now the tab, with the key revoked.
#[tokio::test]
async fn revoking_an_api_key_from_the_tab_answers_with_the_tab() {
    use crate::blocks::auth::repo::api_keys;

    let (ctx, seeds) = seeded_ctx().await;
    let fixture = Fixture {
        ctx: ctx.clone(),
        ..fixture().await
    };
    let controls = controls_on(&fixture, Page::at("/b/admin/users").with("tab", "api-keys")).await;
    let revoke = controls
        .iter()
        .find(|c| c.tag.contains("Revoke this API key?"))
        .expect("the tab renders a Revoke button for the seeded key");

    let answer = fire(&fixture, revoke).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(
        answer.content_type.starts_with("text/html"),
        "the answer is swapped into the tab, so it must be HTML; got {}: {}",
        answer.content_type,
        answer.body
    );
    assert!(
        answer.body.contains("ipk_abc"),
        "the answer must be the key table, with the seeded key in it: {}",
        answer.body
    );
    assert!(
        !answer.body.contains("Revoke this API key?"),
        "the re-rendered tab must show the key revoked, with no Revoke button: {}",
        answer.body
    );
    let key = api_keys::find_by_id(&ctx, &seeds.key_id)
        .await
        .expect("read the key")
        .expect("the key row");
    assert!(key.revoked_at.is_some(), "the key must really be revoked");
}

/// The two forms a review reported as posting a form body to a JSON-only
/// handler: the roles tab's Create Role modal and the SQL tab's editor. Both
/// target the block's form routes (`POST /b/admin/iam/roles`,
/// `POST /b/admin/database/query`), not the JSON API rows the report paired
/// them with, and both routes parse form bodies. This pins that with the
/// page's own bytes, and pins what each answer carries. A guard: nothing here
/// was broken when it was written.
#[tokio::test]
async fn the_create_role_and_sql_forms_post_to_routes_that_read_their_bytes() {
    let fixture = fixture().await;

    let create_role = controls_on(&fixture, Page::at("/b/admin/users").with("tab", "roles"))
        .await
        .into_iter()
        .find(|c| c.url == "/b/admin/iam/roles" && c.form.is_some())
        .expect("the roles tab renders the Create Role form");
    assert_eq!(
        encoded_fields(&create_role, OPERATOR_INPUT),
        "name=probe-typed-name&description=",
        "the form posts the fields the page names"
    );
    let answer = fire(&fixture, &create_role).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(answer.content_type.starts_with("text/html"), "{answer:?}");
    assert!(
        answer.body.contains("probe-typed-name"),
        "the answer is the roles tab with the new role in it: {}",
        answer.body
    );

    let query = controls_on(&fixture, Page::at("/b/admin/database").with("tab", "sql"))
        .await
        .into_iter()
        .find(|c| c.url == "/b/admin/database/query")
        .expect("the SQL tab renders the editor form");
    assert_eq!(
        encoded_fields(&query, OPERATOR_INPUT),
        "query=SELECT+1%3B",
        "the editor posts its textarea, form-encoded"
    );
    let answer = fire(&fixture, &query).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert!(answer.content_type.starts_with("text/html"), "{answer:?}");
    assert!(
        answer.body.contains("1 row") || answer.body.contains("<table"),
        "the answer is the result grid for the query the form carried: {}",
        answer.body
    );
}
