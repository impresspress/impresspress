//! Integration tests for the Plan A2 OAuth callback repo-layer logic.
//!
//! These tests exercise the three key paths in `handle_oauth_callback` at the
//! repo level (provider_links + users), without going through the HTTP handler
//! (which would require a live OAuth provider):
//!
//! 1. First OAuth login creates a new user row + provider_link.
//! 2. Re-login with same (provider, provider_ref) finds the existing link and
//!    does NOT create a duplicate user.
//! 3. Login with same email but different provider creates a second
//!    provider_link bound to the SAME user_id (account-merging).

use impresspress_core::blocks::auth::{
    migrations,
    repo::{provider_links, users},
};

use crate::common::MigrationTestCtx;

/// Simulate the "new user via OAuth" path:
/// no prior link, no prior email → insert user, upsert link.
#[tokio::test]
async fn first_oauth_login_creates_user_and_link() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    // No prior link.
    let existing_link = provider_links::find_by_provider_ref(&ctx, "github", "gh-42")
        .await
        .expect("find_by_provider_ref")
        .is_some();
    assert!(!existing_link);

    // No prior user for this email.
    let existing_user = users::find_by_email(&ctx, "alice@example.com")
        .await
        .expect("find_by_email");
    assert!(existing_user.is_none());

    // Create the user (new signup path).
    let user = users::insert(
        &ctx,
        users::NewUser {
            email: "alice@example.com".into(),
            display_name: "Alice".into(),
            avatar_url: Some("https://github.com/alice.png".into()),
            role: "user".into(),
        },
    )
    .await
    .expect("insert user");

    // Upsert the provider_link.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "gh-42",
            user_id: &user.id,
            provider_login: "alice",
            access_token: "tok-first",
        },
    )
    .await
    .expect("upsert link");

    // Verify both rows exist.
    let link = provider_links::find_by_provider_ref(&ctx, "github", "gh-42")
        .await
        .expect("find")
        .expect("link row present");
    assert_eq!(link.user_id, user.id);
    assert_eq!(link.provider_login, "alice");
    assert_eq!(link.access_token, "tok-first");

    let found = users::find_by_email(&ctx, "alice@example.com")
        .await
        .expect("find_by_email")
        .expect("user present");
    assert_eq!(found.id, user.id);
    assert_eq!(found.display_name, "Alice");
}

/// Simulate re-login with same (provider, provider_ref):
/// the existing link is found → user_id reused → no new user row.
#[tokio::test]
async fn re_login_same_provider_ref_no_duplicate_user() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    // Seed the user + initial link (first login).
    let user = users::insert(
        &ctx,
        users::NewUser {
            email: "bob@example.com".into(),
            display_name: "Bob".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("insert user");

    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "google",
            provider_ref: "gg-99",
            user_id: &user.id,
            provider_login: "bob@gmail.com",
            access_token: "tok-v1",
        },
    )
    .await
    .expect("initial upsert");

    // Second login: link already exists → reuse user_id, update token.
    let link = provider_links::find_by_provider_ref(&ctx, "google", "gg-99")
        .await
        .expect("find")
        .expect("link present");
    assert_eq!(link.user_id, user.id); // same user

    // Upsert again with a new access token (token refresh on re-login).
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "google",
            provider_ref: "gg-99",
            user_id: &link.user_id,
            provider_login: "bob@gmail.com",
            access_token: "tok-v2",
        },
    )
    .await
    .expect("re-login upsert");

    // Token updated; still only one link row.
    let updated = provider_links::find_by_provider_ref(&ctx, "google", "gg-99")
        .await
        .expect("find")
        .expect("link present");
    assert_eq!(updated.user_id, user.id);
    assert_eq!(updated.access_token, "tok-v2");

    // Confirm only one user with this email.
    let found = users::find_by_email(&ctx, "bob@example.com")
        .await
        .expect("find_by_email")
        .expect("user present");
    assert_eq!(found.id, user.id);
}

/// Account-merging: same email, different provider → second provider_link
/// bound to the SAME user_id.
#[tokio::test]
async fn same_email_different_provider_merges_to_same_user() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");

    // Carol first logged in via GitHub.
    let user = users::insert(
        &ctx,
        users::NewUser {
            email: "carol@example.com".into(),
            display_name: "Carol".into(),
            avatar_url: None,
            role: "user".into(),
        },
    )
    .await
    .expect("insert user");

    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "github",
            provider_ref: "gh-carol",
            user_id: &user.id,
            provider_login: "carol-gh",
            access_token: "gh-tok",
        },
    )
    .await
    .expect("github link");

    // Now Carol logs in via Google with the same email.
    // No provider_link for (google, gg-carol) yet → email-merge path.
    let link_opt = provider_links::find_by_provider_ref(&ctx, "google", "gg-carol")
        .await
        .expect("find google link");
    assert!(link_opt.is_none()); // no google link yet

    // Email lookup: finds carol's existing account.
    let existing = users::find_by_email(&ctx, "carol@example.com")
        .await
        .expect("find_by_email")
        .expect("user found");
    assert_eq!(existing.id, user.id); // same user

    // Create the Google link pointing at the same user.
    provider_links::upsert(
        &ctx,
        provider_links::NewLink {
            provider: "google",
            provider_ref: "gg-carol",
            user_id: &existing.id,
            provider_login: "carol@gmail.com",
            access_token: "gg-tok",
        },
    )
    .await
    .expect("google link");

    // Both links now exist, both pointing to the same user_id.
    let gh_link = provider_links::find_by_provider_ref(&ctx, "github", "gh-carol")
        .await
        .expect("find")
        .expect("gh link");
    let gg_link = provider_links::find_by_provider_ref(&ctx, "google", "gg-carol")
        .await
        .expect("find")
        .expect("gg link");

    assert_eq!(gh_link.user_id, user.id);
    assert_eq!(gg_link.user_id, user.id); // merged — not a second user
    assert_ne!(gh_link.provider, gg_link.provider);
    assert_eq!(gh_link.user_id, gg_link.user_id);

    // Still only one user row for this email.
    let all_carols: Vec<_> = {
        // count via find_by_email — only one can exist (email is UNIQUE)
        let found = users::find_by_email(&ctx, "carol@example.com")
            .await
            .expect("find_by_email");
        found.into_iter().collect()
    };
    assert_eq!(all_carols.len(), 1);
}
