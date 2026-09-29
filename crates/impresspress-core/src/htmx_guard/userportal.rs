//! `impresspress/userportal`: the portal pages a signed-in user sees and the
//! admin's portal-button pages.

use std::sync::Arc;

use wafer_core::clients::crypto;
use wafer_run::{Block, InputStream, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::{
        auth::repo::{local_credentials, provider_links, sessions},
        auth_ui::AuthUiBlock,
        userportal::UserPortalBlock,
    },
    test_support::{
        admin_msg, auth_msg,
        htmx::{Fixture, Page, Site},
        TestContext,
    },
};

/// The signed-in portal user every portal page is rendered for.
const USER: &str = "portal-user";
/// Their password, which the change-password form's "current" field carries.
const PASSWORD: &str = "portal-password-2026";

/// What the user types into the portal forms: their current password and a
/// new one on the security page, and a label and path for a new portal
/// button.
const OPERATOR_INPUT: &[(&str, &str)] = &[
    ("current_password", PASSWORD),
    ("new_password", "portal-password-2027"),
    ("label", "Probe link"),
    ("path", "/b/probe"),
];

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/userportal",
        fixture: Some(fixture),
        exempt: &[(
            "/b/userportal/config",
            Exempt::NotAPage("public JSON the chrome reads for the portal's branding"),
        )],
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[
            "delete /b/userportal/sessions/{family}",
            "delete /b/userportal/security/providers/{provider}",
            "create /b/auth/api/change-password",
            "create /b/userportal/admin/buttons",
            "update /b/userportal/admin/buttons/{id}",
            "delete /b/userportal/admin/buttons/{id}",
        ],
    }
}

/// Portal pages as [`USER`]; the `/admin/` pages as the site admin.
fn caller(action: &str, path: &str) -> Message {
    if path.starts_with("/b/userportal/admin/") {
        admin_msg(action, path)
    } else {
        auth_msg(action, path, USER)
    }
}

fn fixture() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture>>> {
    Box::pin(async {
        // The fixture's own frame: every request below is routed, so each
        // runs as the block it reaches.
        let mut ctx = TestContext::with_userportal().await.fixture();
        let crypto_service = Arc::new(
            wafer_block_crypto::service::Argon2JwtCryptoService::new(
                "test-jwt-secret-padded-to-min-32-bytes-aaaa".to_string(),
            )
            .expect("test secret is long enough"),
        );
        ctx.register_block(
            "wafer-run/crypto",
            Arc::new(wafer_core::service_blocks::crypto::CryptoBlock::new(
                crypto_service,
            )),
        );

        // The user, with a password (the security page's change-password
        // form), a signed-in device (the sessions page's Revoke) and a linked
        // OAuth provider (the security page's Unlink).
        ctx.seed_auth_user(USER).await;
        let hash = crypto::hash(&ctx, PASSWORD)
            .await
            .expect("hash the password");
        local_credentials::insert(&ctx, USER, &hash, false)
            .await
            .expect("seed the password");
        sessions::insert(
            &ctx,
            sessions::NewSession {
                family: "probe-family".to_string(),
                user_id: USER.to_string(),
                auth_method: "password".to_string(),
                expires_at: "2099-01-01T00:00:00Z".to_string(),
            },
        )
        .await
        .expect("seed a session");
        provider_links::upsert(
            &ctx,
            provider_links::NewLink {
                provider: "github",
                provider_ref: "probe-ref",
                user_id: USER,
                provider_login: "probe",
            },
        )
        .await
        .expect("seed a provider link");

        // One portal button, made through the admin form's own route so the
        // row is exactly what the block writes.
        let block = UserPortalBlock::new();
        let created = block
            .handle(
                &ctx,
                admin_msg("create", "/b/userportal/admin/buttons"),
                InputStream::from_bytes(b"label=Seeded&path=%2Fb%2Fseeded&icon=package".to_vec()),
            )
            .await;
        // The block answers with its buttons table, which links each row's
        // edit modal; the seeded row's id is read from there.
        let table = crate::test_support::htmx::answer(created).await;
        assert_eq!(table.status, 200, "seed a portal button: {}", table.body);
        let marker = "/b/userportal/admin/buttons/";
        let button_id = table
            .body
            .split(marker)
            .filter_map(|rest| rest.split_once("/edit").map(|(id, _)| id))
            .find(|id| !id.contains(['"', '/', '<']))
            .expect("the buttons table links the seeded row's edit modal")
            .to_string();

        Fixture {
            ctx,
            site: Site(vec![
                Arc::new(UserPortalBlock::new()) as Arc<dyn Block>,
                Arc::new(AuthUiBlock::new()),
            ]),
            caller,
            pages: vec![
                Page::at("/b/userportal/"),
                Page::at("/b/userportal/profile"),
                Page::at("/b/userportal/sessions"),
                Page::at("/b/userportal/security"),
                Page::at("/b/userportal/admin/settings"),
                Page::at("/b/userportal/admin/buttons"),
                Page::at(format!("/b/userportal/admin/buttons/{button_id}/edit")),
            ],
            probes: Vec::new(),
            operator_input: OPERATOR_INPUT,
        }
    })
}
