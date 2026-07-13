//! `bootstrap::run` — covers email+password, token, empty-config, and
//! already-seeded paths.

use impresspress_core::blocks::auth::{
    bootstrap,
    config::AuthConfig,
    migrations,
    repo::{bootstrap_tokens, local_credentials, users},
    service::hash_token,
};

use crate::common::MigrationTestCtx;

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
    let ctx = MigrationTestCtx::new().await;
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
    let ctx = MigrationTestCtx::new().await;
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
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migrations");

    bootstrap::run(&ctx, &cfg_empty())
        .await
        .expect("bootstrap run");

    assert_eq!(users::count(&ctx).await.expect("count"), 0);
}

#[tokio::test]
async fn skipped_when_users_already_exist() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migrations");

    users::insert(
        &ctx,
        users::NewUser {
            email: "existing@x.io".into(),
            display_name: "E".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("seed existing user");

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
