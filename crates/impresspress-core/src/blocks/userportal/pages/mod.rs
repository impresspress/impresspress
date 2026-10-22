pub mod admin_buttons;
pub mod dashboard;
pub mod profile;
pub mod security;
pub mod sessions;

/// The account pages under one primary and one level of cards, with the long
/// values a real account holds, and the fixture `account-pages.spec.ts` lays
/// out at every width.
#[cfg(test)]
mod page_chrome_tests {
    use crate::{
        blocks::auth::repo::{
            orgs::fixtures::seed_claimed_org,
            provider_links::{upsert, NewLink},
        },
        crypto::META_AUTH_FAMILY,
        test_support::{auth_msg, dom, output_html, TestContext},
    };

    const USER: &str = "user-long";
    /// One unbroken word as wide as a real mailbox: a phone's card is
    /// narrower.
    const EMAIL: &str = "accountspayable.northwindtraders.wellington@example.com";

    fn msg(path: &str) -> wafer_run::Message {
        let mut msg = auth_msg("retrieve", path, USER);
        msg.set_meta("auth.user_email", EMAIL);
        msg.set_meta(META_AUTH_FAMILY, "fam-current");
        msg
    }

    /// An account with what a real one collects: linked accounts whose
    /// logins are long handles and mailboxes (one from a provider this build
    /// no longer supports) and sessions from every way in.
    async fn seeded() -> TestContext {
        let ctx = TestContext::with_userportal().await;
        ctx.seed_auth_user(USER).await;
        for (provider, reference, login) in [
            ("github", "gh-1", "northwind-traders-production-engineering"),
            ("google", "gg-1", EMAIL),
            (
                "microsoft",
                "ms-1",
                "margaret.vandenbergoyelaran@northwindtradersproduction.onmicrosoft.com",
            ),
            (
                "gitlab",
                "gl-1",
                "northwind_traders_wellington_cbd_lambton_quay",
            ),
        ] {
            upsert(
                &ctx,
                NewLink {
                    provider,
                    provider_ref: reference,
                    user_id: USER,
                    provider_login: login,
                },
            )
            .await
            .unwrap();
        }
        // Fixed, distinct dates: the list's order, and the fixture taken
        // from it, cannot depend on when the test ran.
        for (family, method, at) in [
            ("fam-current", "password", "2026-10-01T10:04:00Z"),
            ("fam-ms", "oauth.microsoft", "2026-10-01T10:03:00Z"),
            ("fam-gh", "oauth.github", "2026-10-01T10:02:00Z"),
            ("fam-setup", "bootstrap", "2026-10-01T10:01:00Z"),
        ] {
            ctx.seed_auth_session(family, USER, method, at).await;
        }
        ctx
    }

    /// Claimed orgs named by a long GitHub login and long domains.
    async fn seed_orgs(ctx: &TestContext) {
        for (name, via, reference) in [
            (
                "northwind-traders-production-engineering",
                "github",
                "gh-org-1",
            ),
            (
                "northwindtradersproduction.example.com",
                "google",
                "gg-org-1",
            ),
            (
                "northwindtradersproduction.onmicrosoft.com",
                "microsoft",
                "ms-org-1",
            ),
        ] {
            seed_claimed_org(ctx, name, USER, via, reference, "2026-10-01T10:00:00Z").await;
        }
    }

    /// Each account page, rendered as the seeded user sees it: `(name, the
    /// path, the page)`.
    async fn pages(ctx: TestContext) -> Vec<(&'static str, &'static str, String)> {
        let ctx = &ctx;
        let mut pages = vec![
            (
                "overview",
                "/b/userportal/",
                output_html(super::dashboard::dashboard_page(ctx, &msg("/b/userportal/")).await)
                    .await,
            ),
            (
                "profile",
                "/b/userportal/profile",
                output_html(super::profile::profile_page(ctx, &msg("/b/userportal/profile")).await)
                    .await,
            ),
            (
                "security",
                "/b/userportal/security",
                output_html(
                    super::security::security_page(ctx, &msg("/b/userportal/security")).await,
                )
                .await,
            ),
            (
                "sessions",
                "/b/userportal/sessions",
                output_html(
                    super::sessions::sessions_page(ctx, &msg("/b/userportal/sessions")).await,
                )
                .await,
            ),
        ];
        // Organizations is the auth UI block's page, read under its grants.
        let orgs_ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        orgs_ctx.seed_auth_user(USER).await;
        seed_orgs(&orgs_ctx).await;
        pages.push((
            "orgs",
            "/b/auth/orgs",
            output_html(
                crate::blocks::auth_ui::pages::orgs::handle(&orgs_ctx, &msg("/b/auth/orgs")).await,
            )
            .await,
        ));
        pages
    }

    /// Profile's one primary is Save and Security's is Change password
    /// (Resend is a secondary); Sessions and Organizations hold none, and
    /// Revoke is quiet. Profile is one card and Security three, none holding
    /// another card-level surface; every page has one h1 and no heading
    /// skips a level.
    #[tokio::test]
    async fn each_account_page_has_one_primary_and_one_level_of_cards() {
        let ctx = seeded().await;
        for (name, _, html) in pages(ctx).await {
            let (primaries, cards) = match name {
                "profile" => (vec![r#"button.btn.btn--primary "Save""#], 1),
                "security" => (vec![r#"button.btn.btn--primary "Change password""#], 3),
                _ => (vec![], 0),
            };
            assert_eq!(dom::visible_primaries(&html).0, primaries, "{name}: {html}");
            assert_eq!(
                html.matches(r#"class="card""#).count(),
                cards,
                "{name}: {html}"
            );
            assert_eq!(
                dom::card_level_inside_card(&html),
                Vec::<String>::new(),
                "{name}"
            );
            let outline = dom::heading_outline(&html);
            assert_eq!(
                outline.iter().filter(|(level, _)| *level == 1).count(),
                1,
                "{name}: {outline:?}"
            );
            assert!(
                outline.windows(2).all(|pair| pair[1].0 <= pair[0].0 + 1),
                "{name}: a heading skips a level: {outline:?}"
            );
        }
    }

    /// Security's three sections are titled cards; the name and the address
    /// break anywhere (`.profile-header__name`, `.account-email`); a linked
    /// account and an org name a provider as people read it; and the
    /// unverified page's Resend stays a secondary.
    #[tokio::test]
    async fn the_cards_carry_their_titles_and_the_long_values_their_wrap() {
        let ctx = seeded().await;
        let pages = pages(ctx).await;
        let page = |name: &str| {
            pages
                .iter()
                .find(|(n, _, _)| *n == name)
                .map(|(_, _, html)| html.as_str())
                .unwrap()
        };
        assert_eq!(
            dom::heading_outline(page("security")),
            vec![
                (1, "Security".to_string()),
                (2, "Password".to_string()),
                (2, "Email verification".to_string()),
                (2, "Linked accounts".to_string()),
            ]
        );
        assert!(
            page("security").contains(&format!(
                r#"Email not verified — <span class="account-email">{EMAIL}</span>"#
            )),
            "{}",
            page("security")
        );
        assert!(
            page("security").contains(
                r#"<button class="btn btn--secondary" type="button" data-action="resend-verification""#
            ),
            "{}",
            page("security")
        );
        assert!(
            page("profile").contains(r#"<div class="profile-header__name">user-long</div>"#),
            "{}",
            page("profile")
        );
        assert!(
            page("profile").contains(&format!(
                r#"<div class="profile-header__email">{EMAIL}</div>"#
            )),
            "{}",
            page("profile")
        );
        for provider in ["GitHub", "Google", "Microsoft"] {
            assert!(
                page("security").contains(&format!(
                    r#"<span class="linked-provider__name">{provider}</span>"#
                )),
                "{provider}"
            );
            assert!(
                page("orgs").contains(&format!(r#"<td data-label="Verified via">{provider}</td>"#)),
                "{}",
                page("orgs")
            );
        }
    }

    /// The pages' bodies, by name (`<!-- page: name -->` lines), each the
    /// inside of `main#content` less its scripts, with every timestamp
    /// pinned to one date: a linked account is stamped with the time the
    /// test ran. (The sessions' order comes from their fixed dates, so
    /// pinning their text changes nothing else.)
    async fn account_fixture() -> String {
        let ctx = seeded().await;
        let time = regex::Regex::new(
            r#"<time class="datetime" datetime="[^"]*" title="[^"]*">[^<]*</time>"#,
        )
        .unwrap();
        let mut out = String::new();
        for (name, _, html) in pages(ctx).await {
            if !matches!(name, "security" | "sessions" | "orgs") {
                continue;
            }
            let main = html
                .split_once(r#"<main class="shell__body" id="content" tabindex="0">"#)
                .and_then(|(_, rest)| rest.split_once("</main>"))
                .map(|(main, _)| main)
                .unwrap_or_else(|| panic!("{name}: no main#content\n{html}"));
            let mut body = String::new();
            let mut rest = main;
            while let Some(start) = rest.find("<script") {
                body.push_str(&rest[..start]);
                let end = rest[start..].find("</script>").expect("closed script")
                    + start
                    + "</script>".len();
                rest = &rest[end..];
            }
            body.push_str(rest);
            let body = time.replace_all(
                &body,
                r#"<time class="datetime" datetime="2026-10-01T10:00:00.000Z" title="2026-10-01T10:00:00.000Z">2026-10-01 10:00 UTC</time>"#,
            );
            out.push_str(&format!("<!-- page: {name} -->\n{body}\n"));
        }
        out
    }

    /// `account-pages.spec.ts` lays out the seeded Security, Sessions and
    /// Organizations pages from a fixture file (the e2e server cannot link a
    /// provider, start an OAuth session or claim an org); this keeps that
    /// file what the pages render. To refresh it after a deliberate markup
    /// change, write `account_fixture()` to the path below.
    #[tokio::test]
    async fn the_account_fixture_is_what_the_pages_render() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../impresspress-web/tests/e2e/fixtures/account-pages.html"
        );
        let expected = account_fixture().await;
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert_eq!(
            on_disk, expected,
            "{path} is out of date; write the expected text to it"
        );
    }
}
