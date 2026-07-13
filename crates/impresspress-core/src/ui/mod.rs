//! Server-side rendered UI components for impresspress blocks.
//!
//! Uses maud for compile-time HTML generation and htmx for interactivity.
//! CSS, htmx JS, logos, and the favicon are embedded in the binary; their
//! URLs can be overridden via environment variables (`WAFER_RUN_SHARED__LOGO_URL`,
//! `WAFER_RUN_SHARED__LOGO_ICON_URL`, `WAFER_RUN_SHARED__FAVICON_URL`).

pub mod assets;
pub mod components;
pub mod icons;
pub mod layout;
pub mod nav_groups;
pub mod palette;
pub mod settings_form;
pub mod shell;
pub mod sidebar;
pub mod templates;

/// Branding/site config loaded from environment variables.
/// Passed through to layout and sidebar so every page renders consistently.
pub struct SiteConfig {
    pub app_name: String,
    pub logo_url: String,
    pub logo_icon_url: String,
    pub favicon_url: String,
    /// Extra module-type script URLs appended to every rendered page.
    /// Browser targets populate this (e.g. `/webllm-engine.js` for the
    /// page-side LLM engine); native targets leave it empty.
    pub embedded_scripts: Vec<String>,
}

impl SiteConfig {
    /// Load site config from the WAFER config system (env vars / variables table).
    pub async fn load(ctx: &dyn wafer_run::context::Context) -> Self {
        use wafer_core::clients::config;
        let scripts_raw = config::get_default(ctx, "WAFER_RUN_SHARED__EMBEDDED_SCRIPTS", "").await;
        Self {
            app_name: config::get_default(ctx, "WAFER_RUN_SHARED__APP_NAME", "Impresspress").await,
            logo_url: config::get_default(
                ctx,
                "WAFER_RUN_SHARED__LOGO_URL",
                assets::logo_long_url(),
            )
            .await,
            logo_icon_url: config::get_default(
                ctx,
                "WAFER_RUN_SHARED__LOGO_ICON_URL",
                assets::logo_icon_url(),
            )
            .await,
            favicon_url: config::get_default(
                ctx,
                "WAFER_RUN_SHARED__FAVICON_URL",
                assets::favicon_url(),
            )
            .await,
            embedded_scripts: scripts_raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }
}

/// User info available during rendering (extracted from auth metadata).
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub roles: Vec<String>,
}

impl UserInfo {
    /// Create from message auth metadata.
    pub fn from_message(msg: &wafer_run::Message) -> Option<Self> {
        let id = msg.get_meta("auth.user_id");
        if id.is_empty() {
            return None;
        }
        let email = msg.get_meta("auth.user_email").to_string();
        let roles: Vec<String> = msg
            .get_meta("auth.user_roles")
            .split(',')
            .filter(|r| !r.trim().is_empty())
            .map(|r| r.trim().to_string())
            .collect();
        Some(Self {
            id: id.to_string(),
            email,
            roles,
        })
    }

    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == "admin")
    }

    /// First letter of email, uppercased, for avatar.
    pub fn avatar_initial(&self) -> char {
        self.email
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
    }
}

/// A navigation item for the sidebar.
pub struct NavItem {
    pub label: String,
    pub href: String,
    /// The icon renderer, referenced directly (e.g. `icons::users`). Typed as
    /// a function pointer rather than a name string so the compiler rejects a
    /// missing or misspelled icon instead of silently falling back to a
    /// default glyph (the bug the old `nav_icon` string match hid).
    pub icon: fn() -> maud::Markup,
    /// When true, render as `target="_blank"` and open in a new tab from
    /// both the sidebar and the ⌘K palette. Used for cross-block links
    /// that have their own chrome (e.g. Inspector).
    pub external: bool,
}

pub use sidebar::NavGroup;

/// Check if the current request is an htmx partial request.
pub fn is_htmx(msg: &wafer_run::Message) -> bool {
    !msg.get_meta("http.header.hx-request").is_empty()
}

/// Respond with full HTML page or htmx fragment depending on request type.
pub fn html_response(markup: maud::Markup) -> wafer_run::OutputStream {
    crate::http::ResponseBuilder::new().body(
        markup.into_string().into_bytes(),
        "text/html; charset=utf-8",
    )
}

/// Declarative description of a shelled SSR page.
///
/// Built with named fields (rather than the former 8-positional-arg
/// `shelled_response`) so the two `&str` fields `title` and `current_path`
/// can't be transposed, and so the compiler enforces every field is supplied.
/// Render it with [`Page::render`] (full `Markup`) or [`Page::response`]
/// (htmx-aware `OutputStream`).
pub struct Page<'a> {
    pub config: &'a SiteConfig,
    pub title: &'a str,
    /// The audience's sidebar groups (admin or portal).
    pub nav: &'a [NavGroup],
    pub user: Option<&'a UserInfo>,
    pub current_path: &'a str,
    pub topbar: shell::Topbar<'a>,
    pub body: maud::Markup,
}

impl<'a> Page<'a> {
    /// Render the full page: `page()` wrapping `shell()` + the ⌘K palette
    /// modal (mounted only when `topbar.show_palette` is true).
    pub fn render(self) -> maud::Markup {
        use maud::{html, PreEscaped};
        let palette_markup = if self.topbar.show_palette {
            palette::palette(nav_groups::palette_entries_from_groups(self.nav))
        } else {
            html! {}
        };
        layout::page(
            self.title,
            self.config,
            html! {
                (shell::shell(
                    self.nav,
                    self.user,
                    self.current_path,
                    &self.config.logo_url,
                    &self.config.logo_icon_url,
                    self.topbar,
                    self.body,
                ))
                (palette_markup)
                script { (PreEscaped(assets::palette_js())) }
                script { (PreEscaped(assets::drawer_js())) }
            },
        )
    }

    /// htmx-aware response: the raw `body` (no chrome) for an htmx partial,
    /// else the full [`render`](Self::render) document.
    pub fn response(self, msg: &wafer_run::Message) -> wafer_run::OutputStream {
        if is_htmx(msg) {
            return html_response(self.body);
        }
        html_response(self.render())
    }
}

/// Which audience's sidebar a shelled page should render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKind {
    /// End-user portal sidebar (Account / Apps).
    Portal,
    /// Admin sidebar (Workspace / Data / System).
    Admin,
}

impl NavKind {
    fn groups(self) -> Vec<NavGroup> {
        match self {
            NavKind::Portal => nav_groups::portal(),
            NavKind::Admin => nav_groups::admin(),
        }
    }
}

/// Declarative inputs for [`shell_page`] — everything a block page needs to
/// render the standard chrome, minus the body and the data ([`SiteConfig`] /
/// [`UserInfo`] are loaded internally).
pub struct Shell<'a> {
    /// `<title>` text.
    pub title: &'a str,
    /// Which sidebar to render.
    pub nav: NavKind,
    /// Breadcrumb trail. A single `Crumb { label, href: None }` is the common case.
    pub crumbs: Vec<shell::Crumb<'a>>,
    /// Optional subtitle shown after the crumbs.
    pub subtitle: Option<&'a str>,
    /// Optional primary action button in the topbar.
    pub primary_action: Option<maud::Markup>,
}

impl<'a> Shell<'a> {
    /// The single-crumb, no-subtitle, no-action shell that almost every page uses.
    pub fn simple(title: &'a str, nav: NavKind, crumb_label: &'a str) -> Self {
        Self {
            title,
            nav,
            crumbs: vec![shell::Crumb {
                label: crumb_label,
                href: None,
            }],
            subtitle: None,
            primary_action: None,
        }
    }
}

/// Render `body` inside the standard page chrome (sidebar + topbar + ⌘K
/// palette), loading [`SiteConfig`] and [`UserInfo`] internally and returning
/// an htmx-aware [`OutputStream`]. This is the single shell constructor that
/// replaced the six per-block `*_page` wrapper functions (`render_page`,
/// `legalpages_page`, `messages_page`, `products_page`, `llm_page`,
/// `files_page*`) and the inline `Page { .. }.response(msg)` reconstructions.
///
/// `current_path` is taken from the request path so the active sidebar item
/// highlights correctly.
pub async fn shell_page(
    ctx: &dyn wafer_run::context::Context,
    msg: &wafer_run::Message,
    shell: Shell<'_>,
    body: maud::Markup,
) -> wafer_run::OutputStream {
    let config = SiteConfig::load(ctx).await;
    let user = UserInfo::from_message(msg);
    let groups = shell.nav.groups();
    let path = msg.path().to_string();
    Page {
        config: &config,
        title: shell.title,
        nav: &groups,
        user: user.as_ref(),
        current_path: &path,
        topbar: shell::Topbar {
            crumbs: shell.crumbs,
            subtitle: shell.subtitle,
            primary_action: shell.primary_action,
            show_palette: true,
        },
        body,
    }
    .response(msg)
}

/// Minimal `SiteConfig` used by the status-page helpers. They render
/// before context is available, so they can't load real config; fixed
/// branding + no embedded scripts is the right shape.
fn minimal_config() -> SiteConfig {
    SiteConfig {
        app_name: "Impresspress".to_string(),
        logo_url: String::new(),
        logo_icon_url: String::new(),
        favicon_url: assets::favicon_url().to_string(),
        embedded_scripts: Vec::new(),
    }
}

/// Render the styled `status_page` body wrapped in `layout::page` and
/// return it with the requested HTTP status. Used by 403/404/500 helpers.
fn status_response(
    status: u16,
    page_title: &str,
    code: &str,
    title: &str,
    body_text: &str,
    primary_action: (&str, &str),
) -> wafer_run::OutputStream {
    let config = minimal_config();
    let body = templates::status_page(
        code,
        title,
        body_text,
        Some((primary_action.0.to_string(), primary_action.1.to_string())),
    );
    let markup = layout::page(page_title, &config, body);
    crate::http::ResponseBuilder::new().status(status).body(
        markup.into_string().into_bytes(),
        "text/html; charset=utf-8",
    )
}

/// Return styled 403 for browser requests, JSON for API requests.
pub fn forbidden_response(msg: &wafer_run::Message) -> wafer_run::OutputStream {
    let accept = msg.get_meta("http.header.accept");
    if accept.contains("text/html") && !accept.contains("application/json") {
        status_response(
            403,
            "Forbidden",
            "403",
            "Forbidden",
            "You don't have access to this page.",
            ("Sign in", "/b/auth/login"),
        )
    } else {
        crate::http::err_forbidden("admin access required")
    }
}

/// Anonymous (or stale-session — identical by the time enforcement runs)
/// browser request on a protected route: send the user to login with a return
/// path so they land back where they started after signing in. API callers
/// (non-HTML `Accept`) keep the JSON 403 contract instead of a redirect.
///
/// The return path is form-encoded via [`crate::util::urlencode`] into a
/// `?redirect=` query param — the exact param name and encoding the login page
/// consumes (`is_safe_local_redirect` on the consumer side); the producer only
/// needs to encode.
pub fn unauthenticated_response(msg: &wafer_run::Message) -> wafer_run::OutputStream {
    let accept = msg.get_meta("http.header.accept");
    if accept.contains("text/html") && !accept.contains("application/json") {
        let target = format!(
            "/b/auth/login?redirect={}",
            crate::util::urlencode(msg.path())
        );
        crate::http::redirect(302, &target)
    } else {
        crate::http::err_forbidden("authentication required")
    }
}

/// Return styled 404 for browser requests, JSON for API requests.
pub fn not_found_response(msg: &wafer_run::Message) -> wafer_run::OutputStream {
    let accept = msg.get_meta("http.header.accept");
    if accept.contains("text/html") && !accept.contains("application/json") {
        status_response(
            404,
            "Not found",
            "404",
            "Not found",
            "We couldn't find that page.",
            ("Go home", "/"),
        )
    } else {
        crate::http::err_not_found("endpoint not found")
    }
}

/// Return styled 500 for browser requests, JSON for API requests.
pub fn server_error_response(msg: &wafer_run::Message) -> wafer_run::OutputStream {
    let accept = msg.get_meta("http.header.accept");
    if accept.contains("text/html") && !accept.contains("application/json") {
        status_response(
            500,
            "Server error",
            "500",
            "Something went wrong",
            "An unexpected error occurred. Please try again.",
            ("Go home", "/"),
        )
    } else {
        crate::http::err_internal_no_cause("internal server error")
    }
}

/// Respond with HTML + an HX-Trigger header for toast notifications.
///
/// The trigger payload lands in an HTTP response header and is parsed by
/// htmx as JSON. Building it with `format!` would let a toast message
/// containing `"` or `\` produce malformed JSON (and a possible header-
/// injection vector via embedded `\r\n`). Route through `serde_json` so
/// the message text is properly escaped.
pub fn html_response_with_toast(
    markup: maud::Markup,
    toast_message: &str,
    toast_type: &str,
) -> wafer_run::OutputStream {
    let trigger = serde_json::json!({
        "showToast": {
            "message": toast_message,
            "type": toast_type,
        }
    })
    .to_string();
    crate::http::ResponseBuilder::new()
        .set_header("HX-Trigger", &trigger)
        .body(
            markup.into_string().into_bytes(),
            "text/html; charset=utf-8",
        )
}

#[cfg(test)]
mod tests {
    use maud::{html, Markup};
    use wafer_run::Message;

    use super::*;
    use crate::ui::shell::{Crumb, Topbar};

    fn site_config() -> SiteConfig {
        SiteConfig {
            app_name: "TestApp".to_string(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: String::new(),
            embedded_scripts: Vec::new(),
        }
    }

    fn dashboard_page<'a>(
        config: &'a SiteConfig,
        groups: &'a [NavGroup],
        body: Markup,
    ) -> Page<'a> {
        Page {
            config,
            title: "Dashboard",
            nav: groups,
            user: None,
            current_path: "/b/admin/",
            topbar: Topbar {
                crumbs: vec![Crumb {
                    label: "Dashboard",
                    href: None,
                }],
                primary_action: None,
                subtitle: None,
                show_palette: true,
            },
            body,
        }
    }

    #[test]
    fn page_full_render_includes_html_doctype_shell_and_body() {
        let config = site_config();
        let groups = nav_groups::admin();
        let s = dashboard_page(&config, &groups, html! { p { "hello" } })
            .render()
            .into_string();
        assert!(s.contains("<!DOCTYPE html>"));
        assert!(s.contains(r#"class="shell""#));
        assert!(s.contains(r#"id="cmdk""#)); // palette mounted
        assert!(s.contains("hello"));
    }

    #[tokio::test]
    async fn page_response_returns_raw_body_for_htmx_and_full_doc_otherwise() {
        let config = site_config();
        let groups = nav_groups::admin();

        // Non-htmx → full document with chrome.
        let full = dashboard_page(&config, &groups, html! { p { "hello" } })
            .response(&Message::new("http.request"))
            .collect_buffered()
            .await
            .unwrap();
        let full_body = String::from_utf8(full.body).unwrap_or_default();
        assert!(full_body.contains("<!DOCTYPE html>"));
        assert!(full_body.contains(r#"class="shell""#));

        // htmx partial → raw body, no chrome.
        let mut htmx = Message::new("http.request");
        htmx.set_meta("http.header.hx-request", "true");
        let partial = dashboard_page(&config, &groups, html! { p { "hello" } })
            .response(&htmx)
            .collect_buffered()
            .await
            .unwrap();
        let partial_body = String::from_utf8(partial.body).unwrap_or_default();
        assert!(partial_body.contains("hello"));
        assert!(!partial_body.contains("<!DOCTYPE html>"));
        assert!(!partial_body.contains(r#"class="shell""#));
    }

    #[tokio::test]
    async fn not_found_response_uses_status_template() {
        let mut msg = Message::new("http.request");
        msg.set_meta("http.header.accept", "text/html");
        let out = not_found_response(&msg);
        let buf = out.collect_buffered().await.unwrap();
        let body = String::from_utf8(buf.body).unwrap_or_default();
        assert!(
            body.contains("status-page"),
            "body should contain status-page class"
        );
        assert!(body.contains(">404<"), "body should contain 404 code");
        assert!(
            body.contains("Go home"),
            "body should contain Go home action"
        );
    }

    #[tokio::test]
    async fn forbidden_response_uses_status_template() {
        let mut msg = Message::new("http.request");
        msg.set_meta("http.header.accept", "text/html");
        let out = forbidden_response(&msg);
        let buf = out.collect_buffered().await.unwrap();
        let body = String::from_utf8(buf.body).unwrap_or_default();
        assert!(
            body.contains("status-page"),
            "body should contain status-page class"
        );
        assert!(body.contains(">403<"), "body should contain 403 code");
        assert!(
            body.contains("Sign in"),
            "body should contain Sign in action"
        );
    }

    #[tokio::test]
    async fn unauthenticated_response_redirects_html_to_login() {
        // Browser (HTML) request with an empty identity → 302 to the login page
        // carrying a form-encoded `?redirect=` return path the login page
        // consumes via `is_safe_local_redirect`.
        let mut msg = Message::new("http.request");
        msg.set_meta("req.resource", "/b/chat/hello");
        msg.set_meta("http.header.accept", "text/html,application/xhtml+xml");
        let buf = unauthenticated_response(&msg)
            .collect_buffered()
            .await
            .expect("redirect is a Response terminal");
        let status = buf
            .meta
            .iter()
            .find(|e| e.key == "resp.status")
            .map(|e| e.value.as_str());
        let location = buf
            .meta
            .iter()
            .find(|e| e.key == "resp.header.Location")
            .map(|e| e.value.as_str());
        assert_eq!(status, Some("302"));
        assert_eq!(location, Some("/b/auth/login?redirect=%2Fb%2Fchat%2Fhello"));
    }

    #[tokio::test]
    async fn unauthenticated_response_json_accept_stays_403() {
        // API caller (non-HTML Accept) keeps the JSON 403 contract — status
        // stays 403 (not 401) so existing API clients/tests don't break.
        use wafer_block::http_codec;
        use wafer_run::streams::output::TerminalNotResponse;
        let mut msg = Message::new("http.request");
        msg.set_meta("req.resource", "/b/chat/hello");
        msg.set_meta("http.header.accept", "application/json");
        let status = match unauthenticated_response(&msg).collect_buffered().await {
            Ok(buf) => i64::from(http_codec::resolve_status(&buf.meta, 200)),
            Err(TerminalNotResponse::Error(err)) => {
                i64::from(http_codec::resolve_error_status(&err))
            }
            Err(other) => panic!("unexpected terminal: {other:?}"),
        };
        assert_eq!(status, 403);
    }

    #[tokio::test]
    async fn server_error_response_uses_status_template() {
        let mut msg = Message::new("http.request");
        msg.set_meta("http.header.accept", "text/html");
        let out = server_error_response(&msg);
        let buf = out.collect_buffered().await.unwrap();
        let body = String::from_utf8(buf.body).unwrap_or_default();
        assert!(body.contains(">500<"), "body should contain 500 code");
        assert!(
            body.contains("Something went wrong"),
            "body should contain 'Something went wrong' title"
        );
    }
}
