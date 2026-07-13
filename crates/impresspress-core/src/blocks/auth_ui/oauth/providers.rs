//! GET /b/auth/api/oauth/providers — relocated from auth/oauth.rs::handle_oauth_providers
//! in Task 5.

use wafer_core::clients::config;
use wafer_run::{context::Context, OutputStream};

use crate::http::ok_json;

pub async fn handle(ctx: &dyn Context) -> OutputStream {
    let mut providers = Vec::new();

    for spec in super::spec::OAUTH_PROVIDERS {
        let client_id_key = format!(
            "IMPRESSPRESS__AUTH_UI__OAUTH_{}_CLIENT_ID",
            spec.name.to_uppercase()
        );
        if config::get(ctx, &client_id_key).await.is_ok() {
            providers.push(serde_json::json!({
                "name": spec.name,
                "enabled": true
            }));
        }
    }

    ok_json(&serde_json::json!({"providers": providers}))
}
