//! GET /b/auth/api/oauth/providers — the OAuth providers this deployment has
//! configured, by the same test the sign-in page uses to draw its buttons
//! (`pages::oauth_provider_configured`), so the two never disagree.

use wafer_run::{context::Context, OutputStream};

use crate::{blocks::crud, http::ok_json};

pub async fn handle(ctx: &dyn Context) -> OutputStream {
    let mut providers = Vec::new();

    for spec in super::spec::OAUTH_PROVIDERS {
        match super::super::pages::oauth_provider_configured(ctx, spec.name).await {
            Ok(true) => providers.push(serde_json::json!({
                "name": spec.name,
                "enabled": true
            })),
            Ok(false) => {}
            Err(e) => {
                return crud::db_error_internal(e, "Failed to read the OAuth provider config")
            }
        }
    }

    ok_json(&serde_json::json!({"providers": providers}))
}

#[cfg(test)]
mod tests {
    use wafer_run::{ErrorCode, WaferError};

    use super::handle;
    use crate::{
        blocks::auth_ui::{
            OAUTH_GITHUB_CLIENT_ID_KEY, OAUTH_GITHUB_CLIENT_SECRET_KEY, OAUTH_REDIRECT_URI_KEY,
        },
        test_support::{output_http_json, output_json, TestContext},
    };

    /// A provider is listed only when the sign-in page would draw its button:
    /// a client ID that is set but empty is not a configured provider.
    #[tokio::test]
    async fn a_provider_is_listed_only_when_fully_configured() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(OAUTH_GITHUB_CLIENT_ID_KEY, "");
        ctx.set_config(OAUTH_GITHUB_CLIENT_SECRET_KEY, "secret");
        ctx.set_config(OAUTH_REDIRECT_URI_KEY, "https://example.com/cb");
        assert_eq!(
            output_json(handle(&ctx).await).await,
            serde_json::json!({ "providers": [] }),
        );

        ctx.set_config(OAUTH_GITHUB_CLIENT_ID_KEY, "id");
        assert_eq!(
            output_json(handle(&ctx).await).await,
            serde_json::json!({ "providers": [{ "name": "github", "enabled": true }] }),
        );
    }

    /// A refused read is the classified denial, not a list that silently
    /// drops the provider.
    #[tokio::test]
    async fn a_refused_config_read_is_the_classified_denial() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.refuse_config_reads(WaferError::new(
            ErrorCode::PermissionDenied,
            "WRAP: impresspress/auth-ui holds no grant on wafer-run/config",
        ));
        assert_eq!(
            output_http_json(handle(&ctx).await).await,
            serde_json::json!({ "error": "PermissionDenied", "message": "Access denied" }),
        );
    }
}
