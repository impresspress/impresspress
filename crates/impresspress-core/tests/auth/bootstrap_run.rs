//! `bootstrap::run` — covers email+password, token, email-only,
//! empty-config, and already-seeded paths.

use impresspress_core::{
    blocks::auth::{
        bootstrap,
        config::AuthConfig,
        migrations,
        repo::{bootstrap_tokens, local_credentials, users},
        service::hash_token,
    },
    test_support::seed_user,
    util::RecordExt as _,
};

use crate::common::auth_fixture;

/// The roles `user_id`'s grants name: the whole of an account's role.
async fn roles_of(
    ctx: &impresspress_core::test_support::TestContext,
    user_id: &str,
) -> Vec<String> {
    impresspress_core::platform_state::user_roles::list_for_user(ctx, user_id)
        .await
        .expect("list grants")
        .into_iter()
        .map(|grant| grant.role)
        .collect()
}

fn cfg_email_pw(email: &str, pw: &str) -> AuthConfig {
    AuthConfig::from_env_for_test(&[
        ("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL", email),
        ("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD", pw),
    ])
}

fn cfg_token(token: &str) -> AuthConfig {
    AuthConfig::from_env_for_test(&[("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN", token)])
}

fn cfg_email_only(email: &str) -> AuthConfig {
    AuthConfig::from_env_for_test(&[("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL", email)])
}

fn cfg_empty() -> AuthConfig {
    AuthConfig::from_env_for_test(&[])
}

#[tokio::test]
async fn email_password_path_creates_admin_with_local_credentials() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");

    bootstrap::run(&ctx, &cfg_email_pw("root@x.io", "pw"))
        .await
        .expect("bootstrap run");

    let u = users::find_by_email(&ctx, "root@x.io")
        .await
        .expect("find admin")
        .expect("admin created");
    assert_eq!(roles_of(&ctx, &u.id).await, ["admin"]);
    let creds = local_credentials::find_by_user_id(&ctx, &u.id)
        .await
        .expect("find creds")
        .expect("creds row");
    assert!(
        !creds.password_hash.is_empty(),
        "password_hash must be populated"
    );
    assert!(
        creds.password_hash.starts_with("$argon2"),
        "expected argon2 hash, got {}",
        creds.password_hash
    );
    assert!(!creds.must_reset);
    assert_eq!(
        u.email_verified_by.as_deref(),
        Some(users::proof::BOOTSTRAP),
        "the operator chose this address, so it is recorded as proven by the bootstrap"
    );
    assert!(u.email_is_proven());
}

/// An email with neither a password nor a token creates nobody, and no
/// sign-in grants a role from the email: booting would leave the site
/// without the admin its operator configured. So `Init` fails, saying what
/// to set.
#[tokio::test]
async fn email_only_on_an_empty_table_is_refused() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");

    let refusal = match bootstrap::run(&ctx, &cfg_email_only("root@x.io")).await {
        Err(bootstrap::RunError::Misconfigured(e)) => e,
        other => panic!("an email-only first run must be refused, got {other:?}"),
    };
    assert_eq!(refusal.code, wafer_run::ErrorCode::InvalidArgument);
    for key in [
        "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL",
        "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD",
        "WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN",
    ] {
        assert!(
            refusal.message.contains(key),
            "the refusal names {key}: {}",
            refusal.message
        );
    }
    assert_eq!(
        users::count(&ctx).await.expect("count"),
        0,
        "nothing is written"
    );
}

/// The same email-only configuration on a table that already has an
/// account is a no-op, like every other configuration there: the email is
/// spent, and a later settings edit cannot fail a running site's boot.
#[tokio::test]
async fn email_only_after_provisioning_is_a_noop() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");
    seed_user("existing@x.io")
        .display_name("E")
        .insert(&ctx)
        .await;

    bootstrap::run(&ctx, &cfg_email_only("root@x.io"))
        .await
        .expect("an email left behind after provisioning is ignored");

    assert!(
        users::find_by_email(&ctx, "root@x.io")
            .await
            .expect("lookup")
            .is_none(),
        "no account is created for the email"
    );
    assert_eq!(users::count(&ctx).await.expect("count"), 1);
}

#[tokio::test]
async fn token_path_inserts_bootstrap_token_row() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");

    bootstrap::run(&ctx, &cfg_token("secret-token"))
        .await
        .expect("bootstrap run");

    // The bootstrap_tokens row uses sha256(raw) as PK; is_valid checks
    // existence + unexpired.
    let valid = bootstrap_tokens::is_valid(&ctx, &hash_token("secret-token"))
        .await
        .expect("is_valid");
    assert!(valid, "bootstrap token row must be installed and unexpired");

    // No admin user should have been created on the token path.
    assert_eq!(
        users::count(&ctx).await.expect("count"),
        0,
        "token path must not create a user"
    );
}

/// Every `bootstrap_tokens` row, as `(token_hash, expires_at)`.
async fn token_rows(ctx: &impresspress_core::test_support::TestContext) -> Vec<(String, String)> {
    wafer_core::clients::database::list_all(ctx, bootstrap_tokens::TABLE, Vec::new())
        .await
        .expect("list bootstrap tokens")
        .into_iter()
        .map(|row| {
            (
                row.str_field("token_hash").to_string(),
                row.str_field("expires_at").to_string(),
            )
        })
        .collect()
}

/// `iso` as `bootstrap_tokens` stores an expiry.
fn iso(at: chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// **The token step runs on every `Init` while no account exists, and only
/// the first one installs anything.** Every runtime build runs `Init` (each
/// native start, each Cloudflare isolate and prepared plan), so the second
/// one meeting the first one's row is the ordinary case. It used to insert
/// the same `token_hash` again, fail the UNIQUE constraint and fail `Init`.
#[tokio::test]
async fn the_token_step_installs_once_however_often_init_runs() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");
    let cfg = cfg_token("secret-token");

    bootstrap::run(&ctx, &cfg).await.expect("the first Init");
    let first = token_rows(&ctx).await;
    assert_eq!(first.len(), 1, "the first Init installs the token");
    bootstrap::run(&ctx, &cfg).await.expect("the second Init");
    bootstrap::run(&ctx, &cfg).await.expect("the third Init");

    assert_eq!(
        token_rows(&ctx).await,
        first,
        "later Inits leave the first one's row as it is"
    );
    assert!(
        bootstrap_tokens::is_valid(&ctx, &hash_token("secret-token"))
            .await
            .expect("is_valid")
    );
}

/// **The 24 hours run from the first install, not from the latest `Init`.**
/// The row here was installed an hour before it expires, as a first `Init`
/// 23 hours ago left it; a later `Init` computes "24 hours from now" and must
/// not write it over that, or a site rebuilt now and then would keep an
/// unredeemed token alive for ever.
#[tokio::test]
async fn a_later_init_keeps_the_first_install_s_expiry() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");
    let expires = iso(chrono::Utc::now() + chrono::Duration::hours(1));
    assert!(
        bootstrap_tokens::insert_if_absent(&ctx, &hash_token("secret-token"), &expires)
            .await
            .expect("the first install")
    );

    bootstrap::run(&ctx, &cfg_token("secret-token"))
        .await
        .expect("a later Init");

    let rows = token_rows(&ctx).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, expires, "the first install's expiry stands");
}

/// **A token that expired unredeemed stays expired.** `Init` succeeds and
/// does not bring it back: reviving it would be the silent extension the
/// expiry exists to prevent. The operator configures a different token.
#[tokio::test]
async fn an_expired_token_is_not_revived_by_a_later_init() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");
    let expired = iso(chrono::Utc::now() - chrono::Duration::hours(1));
    assert!(
        bootstrap_tokens::insert_if_absent(&ctx, &hash_token("secret-token"), &expired)
            .await
            .expect("the first install")
    );

    bootstrap::run(&ctx, &cfg_token("secret-token"))
        .await
        .expect("Init with an expired token installed");

    assert_eq!(token_rows(&ctx).await[0].1, expired);
    assert!(
        !bootstrap_tokens::is_valid(&ctx, &hash_token("secret-token"))
            .await
            .expect("is_valid")
    );

    bootstrap::run(&ctx, &cfg_token("the-next-token"))
        .await
        .expect("Init with a different token");
    assert_eq!(
        token_rows(&ctx).await.len(),
        2,
        "a different token gets its own row"
    );
    assert!(
        bootstrap_tokens::is_valid(&ctx, &hash_token("the-next-token"))
            .await
            .expect("is_valid")
    );
}

/// **A redeemed token is not installed again.** Redemption spends the row
/// and creates the admin; from then on `Init` finds an account and does
/// nothing, so the token still in the config cannot come back.
#[tokio::test]
async fn a_redeemed_token_is_not_installed_again() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");
    let cfg = cfg_token("secret-token");
    bootstrap::run(&ctx, &cfg).await.expect("the first Init");

    // What `POST /b/auth/api/bootstrap` does: spend the token, then create
    // the admin.
    assert!(
        bootstrap_tokens::take_valid_by_hash(&ctx, &hash_token("secret-token"))
            .await
            .expect("take")
    );
    seed_user("root@x.io")
        .display_name("Admin")
        .insert(&ctx)
        .await;

    bootstrap::run(&ctx, &cfg)
        .await
        .expect("Init after redemption");
    assert!(
        token_rows(&ctx).await.is_empty(),
        "nothing is installed again"
    );
}

/// **Two `Init`s installing the same token at once leave one row, and both
/// succeed** — two Cloudflare isolates building together, or two native
/// processes over one database. The install is one statement, so there is no
/// gap between a "not there yet" read and the insert for the other to land in.
/// Over a file-backed database, so the two really are in flight together on
/// separate connections.
#[tokio::test]
async fn two_inits_installing_the_same_token_at_once_leave_one_row() {
    let ctx = impresspress_core::test_support::TestContext::with_auth_on_disk()
        .await
        .running_as(impresspress_core::blocks::auth::AUTH_BLOCK_ID);
    let cfg = cfg_token("secret-token");

    let (a, b) = futures::join!(bootstrap::run(&ctx, &cfg), bootstrap::run(&ctx, &cfg));
    a.expect("the first Init");
    b.expect("the second Init");

    assert_eq!(token_rows(&ctx).await.len(), 1);
}

#[tokio::test]
async fn no_config_is_noop() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");

    bootstrap::run(&ctx, &cfg_empty())
        .await
        .expect("bootstrap run");

    assert_eq!(users::count(&ctx).await.expect("count"), 0);
}

#[tokio::test]
async fn skipped_when_users_already_exist() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");

    seed_user("existing@x.io")
        .display_name("E")
        .insert(&ctx)
        .await;

    bootstrap::run(&ctx, &cfg_email_pw("new@x.io", "pw"))
        .await
        .expect("bootstrap run");

    assert!(
        users::find_by_email(&ctx, "new@x.io")
            .await
            .expect("lookup")
            .is_none(),
        "must not create new admin when table non-empty"
    );
    assert_eq!(users::count(&ctx).await.expect("count"), 1);
}

/// Spec 2.2.1: the eleven-column map `bootstrap_with_email_password` used to
/// build justified itself with "the legacy columns the rest of impresspress
/// still reads (`name`, `disabled`, `deleted_at`)". Migration 006 creates all
/// three with the defaults the map wrote, so the row `users::insert` produces
/// is the same row. This test writes the OLD map by hand and compares the two
/// column-for-column; if a future migration changes a default, it fails here
/// rather than in production. The old map's `role` entry is left out: auth
/// migration 016 dropped that column, and a role is a grant row now
/// (`bootstrap_writes_the_admin_grant` below).
#[tokio::test]
async fn bootstrap_row_matches_the_hand_built_map_it_replaced() {
    use std::collections::HashMap;

    use serde_json::Value;
    use wafer_core::clients::database as db;

    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");

    bootstrap::run(&ctx, &cfg_email_pw("root@x.io", "pw"))
        .await
        .expect("bootstrap run");
    let through_repo = users::find_by_email(&ctx, "root@x.io")
        .await
        .expect("lookup")
        .expect("bootstrapped admin");

    // The map as `bootstrap.rs` built it before it used the repo, less the
    // dropped `role` column.
    let now = "2026-01-01T00:00:00Z";
    let mut legacy: HashMap<String, Value> = HashMap::new();
    legacy.insert("id".into(), Value::String("legacy-admin".into()));
    legacy.insert("email".into(), Value::String("legacy@x.io".into()));
    legacy.insert("display_name".into(), Value::String("Admin".into()));
    legacy.insert("avatar_url".into(), Value::String(String::new()));
    legacy.insert("email_verified".into(), Value::Bool(true));
    legacy.insert("created_at".into(), Value::String(now.into()));
    legacy.insert("updated_at".into(), Value::String(now.into()));
    legacy.insert("name".into(), Value::String("Admin".into()));
    legacy.insert("disabled".into(), Value::Bool(false));
    legacy.insert("deleted_at".into(), Value::Null);
    db::create(&ctx, users::TABLE, legacy)
        .await
        .expect("write the legacy map");
    let through_map = users::find_by_email(&ctx, "legacy@x.io")
        .await
        .expect("lookup")
        .expect("legacy admin");

    assert_eq!(through_repo.display_name, through_map.display_name);
    assert_eq!(through_repo.name, through_map.name, "the `name` alias");
    assert_eq!(through_repo.email_verified, through_map.email_verified);
    assert_eq!(
        through_repo.disabled, through_map.disabled,
        "migration 006's `DEFAULT 0` gives the same value the map wrote"
    );
    assert_eq!(through_repo.deleted_at, through_map.deleted_at);
    assert_eq!(through_repo.last_login_at, through_map.last_login_at);
    assert!(through_repo.is_active() && through_map.is_active());
    // The one difference, stated rather than glossed: the map wrote an empty
    // string into the nullable `avatar_url`; the repo omits the column, so it
    // stays SQL NULL. Both render as "no avatar" everywhere it is read.
    assert_eq!(through_repo.avatar_url, None);
    assert_eq!(through_map.avatar_url.as_deref(), Some(""));
}

/// A bootstrapped admin is created holding exactly the `admin` grant, in the
/// account's own batch, with no admin behind it. The grant is the whole of
/// its role (auth migration 016 dropped the inline column), so it is an
/// ordinary row another admin can remove through IAM.
#[tokio::test]
async fn bootstrap_writes_the_admin_grant() {
    use impresspress_core::platform_state::user_roles;

    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migrations");
    bootstrap::run(&ctx, &cfg_email_pw("root@x.io", "pw"))
        .await
        .expect("bootstrap run");

    let u = users::find_by_email(&ctx, "root@x.io")
        .await
        .expect("lookup")
        .expect("admin");
    let grants = user_roles::list_for_user(&ctx, &u.id)
        .await
        .expect("list grants");
    assert_eq!(grants.len(), 1, "exactly one grant: {grants:?}");
    assert_eq!(grants[0].role, "admin");
    assert_eq!(grants[0].assigned_by, "", "no admin is behind it");
}
