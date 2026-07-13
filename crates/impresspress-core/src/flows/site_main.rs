/// The top-level flow that wafer-run/http-listener dispatches all requests to.
///
/// API requests are routed to impresspress/router, which delegates to the
/// shared impresspress-core pipeline (JWT validation, feature gates, admin checks,
/// block dispatch). Non-API requests fall through to wafer-run/web for SPA serving.
///
/// Infrastructure blocks (security headers, CORS, readonly guard)
/// are applied inline before routing.
pub const JSON: &str = r#"{
    "id": "site-main",
    "name": "Site Main",
    "version": "0.1.0",
    "description": "Top-level HTTP dispatch — API router + frontend SPA",
    "steps": [
        { "id": "security-headers", "block": "wafer-run/security-headers" },
        { "id": "cors", "block": "wafer-run/cors" },
        { "id": "readonly-guard", "block": "wafer-run/readonly-guard" },
        { "id": "router", "block": "wafer-run/router" }
    ],
    "config": { "on_error": "stop" },
    "config_map": {
        "routes": { "target": "wafer-run/router", "key": "routes" }
    }
}"#;

/// Default routes for the site-main flow.
///
/// Block SSR pages + API go through `impresspress/router`.
/// Static assets are embedded and served by the system block via `/static/`.
/// User's own site content is served by `wafer-run/web` as fallback.
///
/// `/` is routed explicitly to `impresspress/router` (not the `/**` fallback)
/// so the root redirect handler in `routing::route_to_block` fires:
/// anonymous → `/b/auth/login`, authenticated → `/b/userportal/`.
pub fn default_routes() -> serde_json::Value {
    serde_json::json!([
        { "path": "/",                        "block": "impresspress/router" },
        { "path": "/b/**",                    "block": "impresspress/router" },
        { "path": "/health",                  "block": "impresspress/router" },
        { "path": "/openapi.json",            "block": "impresspress/router" },
        { "path": "/.well-known/agent.json",  "block": "impresspress/router" },
        { "path": "/**",            "block": "wafer-run/web", "config": { "web_root": "site", "web_spa": "true", "web_index": "index.html" } }
    ])
}
