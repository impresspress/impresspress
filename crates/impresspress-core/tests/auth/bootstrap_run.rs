//! `bootstrap::run` — covers email+password, token, empty-config, and
//! already-seeded paths.

use impresspress_core::{
    blocks::auth::{
        bootstrap,
        config::AuthConfig,
        migrations,
        repo::{bootstrap_tokens, local_credentials, users},
        service::hash_token,
    },
    test_support::seed_user,
};

use crate::common::auth_fixture;

fn cfg_email_pw(email: &str, pw: &str) -> AuthConfig {
    AuthConfig::from_env_for_test(&[
        ("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL", email),
        ("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD", pw),
    ])
}

fn cfg_token(token: &str) -> AuthConfig {
    AuthConfig::from_env_for_test(&[("WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN", token)])
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
    assert_eq!(u.role, "admin");
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
/// rather than in production.
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

    // The map exactly as `bootstrap.rs` built it before this PR.
    let now = "2026-01-01T00:00:00Z";
    let mut legacy: HashMap<String, Value> = HashMap::new();
    legacy.insert("id".into(), Value::String("legacy-admin".into()));
    legacy.insert("email".into(), Value::String("legacy@x.io".into()));
    legacy.insert("display_name".into(), Value::String("Admin".into()));
    legacy.insert("avatar_url".into(), Value::String(String::new()));
    legacy.insert("role".into(), Value::String("admin".into()));
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
    assert_eq!(through_repo.role, through_map.role);
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

/// Spec 2.2.3: the initial role is the inline `users.role` column, so a
/// bootstrapped admin holds no `user_roles` row and still resolves as admin.
/// This is the same shape both signup paths produce.
#[tokio::test]
async fn bootstrap_writes_no_user_roles_row() {
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
    assert_eq!(u.role, "admin", "the inline column carries the role");
    assert!(
        user_roles::list_for_user(&ctx, &u.id)
            .await
            .expect("list grants")
            .is_empty(),
        "a user_roles row means a grant BEYOND the initial role"
    );
}
