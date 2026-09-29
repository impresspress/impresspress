//! Public ticket form and protected submission flow.

use std::collections::HashMap;

use maud::{html, Markup};
use wafer_core::clients::config as config_client;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    abuse::{self, AbuseDecision},
    config::{self, SecurityReadiness},
    contracts::{PublicSubmissionRequest, SubmissionAck},
    models::{ActorType, CreateTicketInput, TicketSource},
    repo, service, turnstile,
};
use crate::{
    blocks::{crud, rate_limit::UserRateLimiter},
    http::ResponseBuilder,
    ui::{templates, SiteConfig},
};

const MAX_BODY_BYTES: usize = 16 * 1_024;
const TURNSTILE_SCRIPT: &str = "https://challenges.cloudflare.com/turnstile/v0/api.js";

struct Submission {
    ticket: CreateTicketInput,
    form_token: String,
    turnstile_token: String,
    website: String,
}

/// What every public page reads before it renders: the site chrome, the
/// validated back link and the support address.
struct PublicChrome {
    site: SiteConfig,
    back_url: String,
    support_email: String,
}

impl PublicChrome {
    async fn load(ctx: &dyn Context) -> Result<Self, wafer_run::WaferError> {
        Ok(Self {
            site: SiteConfig::load(ctx).await?,
            back_url: safe_back_url(&config_client::get_default(ctx, config::BACK_URL, "/").await?),
            support_email: support_email(ctx).await?,
        })
    }
}

/// The answer to a public submission whose settings could not be read: the
/// same 503 an unreadable ticket table gets, with the cause logged.
fn settings_unavailable(error: &wafer_run::WaferError) -> OutputStream {
    tracing::warn!(error = %error, "public ticket settings read failed");
    public_error(503, "Public reporting is temporarily unavailable")
}

pub async fn form(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (readiness, chrome) = match async {
        Ok::<_, wafer_run::WaferError>((
            SecurityReadiness::load(ctx).await?,
            PublicChrome::load(ctx).await?,
        ))
    }
    .await
    {
        Ok(loaded) => loaded,
        Err(e) => return crud::db_error_page(msg, e, "public ticket form: settings read failed"),
    };
    let PublicChrome {
        site,
        back_url,
        support_email,
    } = chrome;
    let body = if readiness.ready {
        let types = match repo::list_types(ctx, true, 100, 0).await {
            Ok(rows) => rows.records,
            Err(error) => {
                tracing::warn!(error = %error, "public ticket type list failed");
                return unavailable_page(
                    &site,
                    &back_url,
                    &support_email,
                    &["Ticket types are unavailable"],
                );
            }
        };
        let keys = async {
            Ok::<_, wafer_run::WaferError>((
                config_client::get_default(ctx, config::IDENTITY_SECRET, "").await?,
                config_client::get_default(ctx, config::TURNSTILE_SITE_KEY, "").await?,
            ))
        };
        let (secret, site_key) = match keys.await {
            Ok(keys) => keys,
            Err(e) => {
                return crud::db_error_page(msg, e, "public ticket form: key read failed");
            }
        };
        let form_token = abuse::issue_form_token(&secret, abuse::now_secs());
        let source_path = safe_prefill(msg.query("page"), super::models::validate_source_path);
        let subject_type = safe_prefill(msg.query("subject_type"), |value| {
            if value.len() <= 64
                && value.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')
                })
            {
                Ok(())
            } else {
                Err(String::new())
            }
        });
        let subject_id = safe_prefill(msg.query("subject_id"), |value| {
            if value.len() <= 160 && !value.chars().any(char::is_control) {
                Ok(())
            } else {
                Err(String::new())
            }
        });
        form_markup(
            &types,
            &form_token,
            &site_key,
            &source_path,
            &subject_type,
            &subject_id,
            &support_email,
        )
    } else {
        unavailable_markup(&readiness.reasons, &support_email)
    };
    public_page_response(&site, "Report a problem", &back_url, body)
}

pub async fn submitted(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let PublicChrome {
        site,
        back_url,
        support_email,
    } = match PublicChrome::load(ctx).await {
        Ok(chrome) => chrome,
        Err(e) => {
            return crud::db_error_page(msg, e, "public ticket receipt: settings read failed")
        }
    };
    let reference = sanitize_reference(msg.query("reference"));
    let body = html! {
        div .public-page__head {
            h1 { "Thanks for letting us know" }
            p { "Your report has been received and will be reviewed." }
        }
        div .public-page__content {
            @if !reference.is_empty() {
                p { "Reference: " strong { (reference) } }
            }
            @if !support_email.is_empty() {
                p { "For urgent privacy, safety, copyright, or trademark concerns, you can also email " (support_email_link(&support_email)) "." }
            }
        }
    };
    public_page_response(&site, "Report received", &back_url, body)
}

/// The settings a submission is checked against, read together so one
/// failed read answers once.
struct SubmitSettings {
    identity_secret: String,
    form_ttl: u64,
    identity_max: u32,
    identity_window: u64,
    global_max: u32,
    global_window: u64,
}

pub async fn submit(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let readiness = match SecurityReadiness::load(ctx).await {
        Ok(readiness) => readiness,
        Err(e) => return settings_unavailable(&e),
    };
    if !readiness.ready {
        return public_error(503, "Public reporting is temporarily unavailable");
    }
    if msg
        .header("content-length")
        .parse::<usize>()
        .is_ok_and(|size| size > MAX_BODY_BYTES)
    {
        return public_error(413, "Submission is too large");
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    if raw.len() > MAX_BODY_BYTES {
        return public_error(413, "Submission is too large");
    }
    let submission = match parse_submission(msg.header("content-type"), &raw) {
        Ok(value) => value,
        Err(error) => return public_error(400, &error),
    };
    if !submission.website.trim().is_empty() {
        return success_response(msg, "");
    }
    if !same_origin(msg) {
        return public_error(403, "Submission origin could not be verified");
    }

    let settings = async {
        Ok::<_, wafer_run::WaferError>(SubmitSettings {
            identity_secret: config_client::get_default(ctx, config::IDENTITY_SECRET, "").await?,
            form_ttl: config::u64_value(ctx, config::FORM_TTL, 7_200).await?,
            identity_max: config::u32_value(ctx, config::IDENTITY_MAX, 3).await?,
            identity_window: config::u64_value(ctx, config::IDENTITY_WINDOW, 3_600).await?,
            global_max: config::u32_value(ctx, config::GLOBAL_MAX, 100).await?,
            global_window: config::u64_value(ctx, config::GLOBAL_WINDOW, 3_600).await?,
        })
    };
    let settings = match settings.await {
        Ok(settings) => settings,
        Err(e) => return settings_unavailable(&e),
    };
    let identity_secret = settings.identity_secret;
    let form_ttl = settings.form_ttl.clamp(300, 86_400);
    if let Err(error) = abuse::verify_form_token(
        &identity_secret,
        &submission.form_token,
        abuse::now_secs(),
        form_ttl,
    ) {
        return public_error(400, error);
    }
    if let Err(error) = submission.ticket.clone().validate() {
        return public_error(400, &error);
    }

    let now = abuse::now_secs();
    let identity = abuse::rotating_identity(&identity_secret, now, msg.remote_addr());
    let identity_limit = abuse::limit(settings.identity_max, settings.identity_window);
    let global_limit = abuse::limit(settings.global_max, settings.global_window);
    let (Some(identity_limit), Some(global_limit)) = (identity_limit, global_limit) else {
        return public_error(503, "Submission limits are unavailable");
    };
    // The per-identity limit runs BEFORE the outbound Turnstile Siteverify
    // call so one client can trigger at most IDENTITY_MAX subrequests per
    // window; otherwise a flood of schema-valid submissions with garbage
    // tokens makes this handler an unbounded Siteverify amplifier (and, on
    // Workers, burns the per-request subrequest budget). The global limit
    // stays AFTER verification so unverified spam cannot consume the shared
    // bucket and lock out legitimate reporters.
    if let Some(response) = enforce_limit(
        limiter,
        ctx,
        &format!("tickets:identity:{identity}"),
        identity_limit,
    )
    .await
    {
        return response;
    }

    if let Err(error) = turnstile::verify(
        ctx,
        &submission.turnstile_token,
        msg.remote_addr(),
        msg.header("host"),
    )
    .await
    {
        return match error {
            turnstile::VerifyError::Unavailable => {
                public_error(503, "Challenge verification is temporarily unavailable")
            }
            _ => public_error(400, "Challenge verification failed"),
        };
    }

    if let Some(response) = enforce_limit(limiter, ctx, "tickets:global", global_limit).await {
        return response;
    }

    let dedupe = abuse::dedupe_hash(
        &identity_secret,
        &identity,
        &submission.ticket.type_id,
        &submission.ticket.subject,
        &submission.ticket.description,
        &submission.ticket.source_path,
    );
    match service::create_ticket(
        ctx,
        submission.ticket,
        TicketSource::PublicForm,
        ActorType::Public,
        "",
        Some(&dedupe),
    )
    .await
    {
        Ok(ticket) => {
            let reference = service::str_field(&ticket, "reference");
            success_response(msg, reference)
        }
        Err(service::ServiceError::Validation(message)) => public_error(400, &message),
        Err(service::ServiceError::Conflict(_)) => {
            public_error(409, "The report could not be accepted")
        }
        Err(service::ServiceError::Db(error)) => {
            tracing::warn!(error = %error, "public ticket write failed");
            public_error(503, "Public reporting is temporarily unavailable")
        }
    }
}

fn form_markup(
    types: &[wafer_core::clients::database::Record],
    form_token: &str,
    site_key: &str,
    source_path: &str,
    subject_type: &str,
    subject_id: &str,
    support_email: &str,
) -> Markup {
    html! {
        div .public-page__head {
            h1 { "Report a problem" }
            p { "Tell us about incorrect information, broken links, legal concerns, or another problem." }
        }
        div .public-page__content {
            form method="post" action="/b/tickets/api/submissions" {
                input type="hidden" name="form_token" value=(form_token);
                input type="hidden" name="source_path" value=(source_path);
                input type="hidden" name="subject_type" value=(subject_type);
                input type="hidden" name="subject_id" value=(subject_id);
                div .honeypot aria-hidden="true" {
                    label for="website" { "Website" }
                    input #website type="text" name="website" tabindex="-1" autocomplete="off";
                }
                div .form-group {
                    label .form-label for="ticket-type" { "What kind of report is this?" }
                    select #ticket-type .form-input name="type_id" required {
                        option value="" { "Choose a report type" }
                        @for ticket_type in types {
                            option value=(ticket_type.id)
                                data-requires-contact=(if service::bool_field(ticket_type, "requires_contact") { "true" } else { "false" })
                                data-requests-evidence=(if service::bool_field(ticket_type, "requests_evidence") { "true" } else { "false" }) {
                                (service::str_field(ticket_type, "title"))
                            }
                        }
                    }
                    div #ticket-type-help aria-live="polite" {
                        p #ticket-type-prompt .text-muted {
                            "Choose a report type to see what information will help us review it."
                        }
                        @for ticket_type in types {
                            @let description = service::str_field(ticket_type, "description");
                            @let guidance = service::str_field(ticket_type, "guidance");
                            @if !description.is_empty() || !guidance.is_empty() {
                                p .text-muted data-ticket-type-guidance=(ticket_type.id) hidden {
                                    strong { (service::str_field(ticket_type, "title")) ": " }
                                    (description)
                                    @if !guidance.is_empty() { " " (guidance) }
                                }
                            }
                        }
                    }
                }
                div .form-group {
                    label .form-label for="ticket-subject" { "Short summary" }
                    input #ticket-subject .form-input type="text" name="subject" minlength="5" maxlength="160" required;
                }
                div .form-group {
                    label .form-label for="ticket-description" { "What happened?" }
                    textarea #ticket-description .form-input name="description" minlength="20" maxlength="4000" rows="8" required {}
                }
                div .form-group {
                    label #ticket-evidence-label .form-label for="ticket-evidence" {
                        "Evidence URL (optional)"
                    }
                    input #ticket-evidence .form-input type="url" name="evidence_url" maxlength="1024";
                }
                div .form-group {
                    label #ticket-email-label .form-label for="ticket-email" {
                        "Email for a reply (optional)"
                    }
                    input #ticket-email .form-input type="email" name="reporter_email"
                        maxlength="254" aria-describedby="ticket-contact-help";
                    p #ticket-contact-help .text-muted hidden {
                        "This report type needs an email address and permission to reply so we can verify and follow up on the concern."
                    }
                    label {
                        input #ticket-reply-consent type="checkbox"
                            name="reporter_wants_reply" value="true";
                        " I consent to being contacted about this report"
                    }
                }
                div .cf-turnstile data-sitekey=(site_key) data-action="ticket_submit" {}
                noscript { p { "JavaScript is required only for the anti-spam challenge." } }
                button .btn .btn--primary type="submit" { "Submit report" }
            }
            @if !support_email.is_empty() {
                p .text-muted { "You can also email " (support_email_link(support_email)) "." }
            }
        }
        script {
            (maud::PreEscaped(r#"(() => {
                const select = document.getElementById('ticket-type');
                const prompt = document.getElementById('ticket-type-prompt');
                const guidance = document.querySelectorAll('[data-ticket-type-guidance]');
                const email = document.getElementById('ticket-email');
                const emailLabel = document.getElementById('ticket-email-label');
                const consent = document.getElementById('ticket-reply-consent');
                const contactHelp = document.getElementById('ticket-contact-help');
                const evidenceLabel = document.getElementById('ticket-evidence-label');
                const update = () => {
                    let hasSelection = false;
                    guidance.forEach((item) => {
                        const matches = item.dataset.ticketTypeGuidance === select.value;
                        item.hidden = !matches;
                        hasSelection ||= matches;
                    });
                    prompt.hidden = hasSelection;
                    const option = select.selectedOptions[0];
                    const requiresContact = option?.dataset.requiresContact === 'true';
                    const requestsEvidence = option?.dataset.requestsEvidence === 'true';
                    email.required = requiresContact;
                    consent.required = requiresContact;
                    emailLabel.textContent = requiresContact
                        ? 'Contact email (required)'
                        : 'Email for a reply (optional)';
                    contactHelp.hidden = !requiresContact;
                    evidenceLabel.textContent = requestsEvidence
                        ? 'Evidence URL (recommended)'
                        : 'Evidence URL (optional)';
                };
                select.addEventListener('change', update);
                update();
            })();"#))
        }
        script src=(TURNSTILE_SCRIPT) async defer {}
    }
}

fn unavailable_markup(reasons: &[String], support_email: &str) -> Markup {
    html! {
        div .public-page__head {
            h1 { "Reporting is temporarily unavailable" }
            @if support_email.is_empty() {
                p { "Please try again later." }
            } @else {
                p { "Please email " (support_email_link(support_email)) " instead." }
            }
        }
        @if cfg!(debug_assertions) {
            ul {
                @for reason in reasons { li { (reason) } }
            }
        }
    }
}

fn unavailable_page(
    site: &SiteConfig,
    back_url: &str,
    support_email: &str,
    reasons: &[&str],
) -> OutputStream {
    let reasons = reasons
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    public_page_response(
        site,
        "Reporting unavailable",
        back_url,
        unavailable_markup(&reasons, support_email),
    )
}

/// The operator-configured support address, or empty when none is set. Only
/// a plausible single address is accepted so a misconfigured value can never
/// inject markup or a foreign `mailto:` into the public page. A failed read
/// is returned.
async fn support_email(ctx: &dyn Context) -> Result<String, wafer_run::WaferError> {
    let value = config_client::get_default(ctx, config::SUPPORT_EMAIL, "").await?;
    let value = value.trim();
    let plausible = value.len() <= 254
        && value.matches('@').count() == 1
        && !value.starts_with('@')
        && !value.ends_with('@')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'-' | b'_' | b'+'));
    Ok(if plausible {
        value.to_string()
    } else {
        String::new()
    })
}

fn support_email_link(address: &str) -> Markup {
    html! { a href=(format!("mailto:{address}")) { (address) } }
}

fn public_page_response(
    site: &SiteConfig,
    title: &str,
    back_url: &str,
    body: Markup,
) -> OutputStream {
    let page = templates::public_page(
        templates::PublicPage {
            title,
            config: site,
            meta_description: None,
            back_url: Some(back_url),
            bg_color: None,
            accent_color: None,
            footer: None,
        },
        body,
    );
    public_headers(ResponseBuilder::new())
        .body(page.into_string().into_bytes(), "text/html; charset=utf-8")
}

fn parse_submission(content_type: &str, raw: &[u8]) -> Result<Submission, String> {
    if content_type
        .to_ascii_lowercase()
        .starts_with("application/json")
    {
        let raw_value: serde_json::Value =
            serde_json::from_slice(raw).map_err(|_| "Invalid JSON submission".to_string())?;
        // The honeypot is read from the raw body, not from the contract —
        // see `PublicSubmissionRequest`'s derive comment for why it must not
        // be declared there.
        //
        // The trap fires on PRESENCE, not on the value being a string. Reading
        // it with `as_str` meant `{"website": 12345}` yielded `None` and read
        // as "not filled"; because the field is deliberately undeclared, the
        // typed deserialize below tolerates it too, so such a body passed
        // straight through. A real browser submits either nothing or a string,
        // so any other JSON type is already a bot.
        let website = match raw_value.get("website") {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(serde_json::Value::String(filled)) => filled.clone(),
            Some(other) => other.to_string(),
        };
        let value: PublicSubmissionRequest =
            serde_json::from_value(raw_value).map_err(|_| "Invalid JSON submission".to_string())?;
        return Ok(Submission {
            ticket: CreateTicketInput {
                type_id: value.type_id,
                subject: value.subject,
                description: value.description,
                source_path: value.source_path,
                subject_type: value.subject_type,
                subject_id: value.subject_id,
                evidence_url: value.evidence_url,
                reporter_email: value.reporter_email,
                reporter_wants_reply: value.reporter_wants_reply,
                priority: None,
            },
            form_token: value.form_token,
            turnstile_token: value.turnstile_token,
            website,
        });
    }
    let form = crate::util::parse_form_body(raw);
    let required = |name: &str| {
        form.get(name)
            .cloned()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("Missing {name}"))
    };
    Ok(Submission {
        ticket: CreateTicketInput {
            type_id: required("type_id")?,
            subject: required("subject")?,
            description: required("description")?,
            source_path: form_value(&form, "source_path"),
            subject_type: form_value(&form, "subject_type"),
            subject_id: form_value(&form, "subject_id"),
            evidence_url: form_value(&form, "evidence_url"),
            reporter_email: form_value(&form, "reporter_email"),
            reporter_wants_reply: crate::config_vars::form_bool(&form, "reporter_wants_reply"),
            priority: None,
        },
        form_token: required("form_token")?,
        turnstile_token: form
            .get("cf-turnstile-response")
            .or_else(|| form.get("turnstile_token"))
            .cloned()
            .unwrap_or_default(),
        website: form_value(&form, "website"),
    })
}

fn same_origin(msg: &Message) -> bool {
    let fetch_site = msg.header("sec-fetch-site").to_ascii_lowercase();
    if !fetch_site.is_empty() {
        return matches!(fetch_site.as_str(), "same-origin" | "none");
    }
    let host = canonical_authority(msg.header("host"));
    ["origin", "referer"]
        .into_iter()
        .find_map(|header| {
            let value = msg.header(header);
            (!value.is_empty()).then(|| {
                url::Url::parse(value)
                    .ok()
                    .and_then(|url| url.host_str().map(|h| authority(h, url.port())))
                    .is_some_and(|candidate| candidate == host)
            })
        })
        .unwrap_or(false)
}

fn canonical_authority(value: &str) -> String {
    url::Url::parse(&format!("https://{}", value.trim()))
        .ok()
        .and_then(|url| url.host_str().map(|host| authority(host, url.port())))
        .unwrap_or_default()
}

fn authority(host: &str, port: Option<u16>) -> String {
    match port {
        Some(port) => format!("{}:{port}", host.to_ascii_lowercase()),
        None => host.to_ascii_lowercase(),
    }
}

async fn enforce_limit(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    key: &str,
    limit: crate::blocks::rate_limit::RateLimit,
) -> Option<OutputStream> {
    match abuse::check_durable(limiter, ctx, key, limit).await {
        AbuseDecision::Allowed { .. } => None,
        AbuseDecision::Limited { retry_after } => Some(
            public_headers(ResponseBuilder::new())
                .status(429)
                .set_header("Retry-After", &retry_after.to_string())
                .body(
                    b"Too many reports; please try again later".to_vec(),
                    "text/plain; charset=utf-8",
                ),
        ),
        AbuseDecision::Unavailable => Some(public_error(
            503,
            "Submission limits are temporarily unavailable",
        )),
    }
}

fn success_response(msg: &Message, reference: &str) -> OutputStream {
    if msg
        .header("accept")
        .to_ascii_lowercase()
        .contains("application/json")
    {
        return public_headers(ResponseBuilder::new())
            .status(201)
            .json(&SubmissionAck::received(reference));
    }
    let location = if reference.is_empty() {
        "/b/tickets/submitted".to_string()
    } else {
        format!(
            "/b/tickets/submitted?reference={}",
            crate::util::urlencode(reference)
        )
    };
    let _ = msg;
    public_headers(ResponseBuilder::new())
        .status(303)
        .set_header("Location", &location)
        .body(Vec::new(), "text/plain")
}

fn public_error(status: u16, message: &str) -> OutputStream {
    public_headers(ResponseBuilder::new())
        .status(status)
        .body(message.as_bytes().to_vec(), "text/plain; charset=utf-8")
}

fn public_headers(builder: ResponseBuilder) -> ResponseBuilder {
    builder
        .set_header("Cache-Control", "no-store")
        .set_header("X-Robots-Tag", "noindex, nofollow")
        .set_header("Referrer-Policy", "no-referrer")
}

fn form_value(form: &HashMap<String, String>, key: &str) -> String {
    form.get(key).cloned().unwrap_or_default()
}

fn safe_back_url(value: &str) -> String {
    if super::models::validate_source_path(value).is_ok() {
        value.to_string()
    } else {
        "/".into()
    }
}

fn safe_prefill(value: &str, validate: impl FnOnce(&str) -> Result<(), String>) -> String {
    if validate(value).is_ok() {
        value.to_string()
    } else {
        String::new()
    }
}

fn sanitize_reference(value: &str) -> String {
    let valid = value.len() == 20
        && value.starts_with("TKT-")
        && value[4..].bytes().all(|b| b.is_ascii_hexdigit());
    if valid {
        value.to_ascii_uppercase()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The honeypot works because a bot that fills every field it can see
    /// also fills `website`. That needs two things of the JSON path: unknown
    /// keys are tolerated (a `deny_unknown_fields` contract would reject the
    /// bot's post before the trap could fire), and the trap is read from the
    /// raw body rather than declared on the contract — declaring it published
    /// "website — Must be left empty" to every schema-reading caller.
    #[test]
    fn json_parser_reads_the_honeypot_without_declaring_it() {
        let raw = br#"{
            "type_id":"type","subject":"valid subject","description":"a sufficiently long description",
            "form_token":"token","turnstile_token":"challenge",
            "website":"http://spam.example","unexpected":"value"
        }"#;
        let submission =
            parse_submission("application/json", raw).expect("unknown keys are tolerated");
        assert_eq!(submission.website, "http://spam.example");
    }

    /// Reading the trap with `as_str` meant any non-string JSON type fell
    /// through as "not filled". The contract no longer declares `website`, so
    /// a typed deserializer no longer rejects those bodies either — the bot
    /// simply passed. The trap fires on presence, not on being a string.
    #[test]
    fn json_parser_catches_a_honeypot_filled_with_a_non_string() {
        for filled in ["12345", "true", r#"["x"]"#, r#"{"a":1}"#] {
            let raw = format!(
                r#"{{
                "type_id":"type","subject":"valid subject","description":"a sufficiently long description",
                "form_token":"token","turnstile_token":"challenge",
                "website":{filled}
            }}"#
            );
            let submission = parse_submission("application/json", raw.as_bytes())
                .expect("unknown/oddly-typed keys are tolerated");
            assert!(
                !submission.website.trim().is_empty(),
                "a honeypot filled with {filled} must trip the trap"
            );
        }
    }

    /// The other half: an explicitly empty or null `website` is what a real
    /// browser submits, and must not trip the trap.
    #[test]
    fn json_parser_treats_an_empty_or_null_honeypot_as_unfilled() {
        for unfilled in [r#""""#, "null"] {
            let raw = format!(
                r#"{{
                "type_id":"type","subject":"valid subject","description":"a sufficiently long description",
                "form_token":"token","turnstile_token":"challenge",
                "website":{unfilled}
            }}"#
            );
            let submission =
                parse_submission("application/json", raw.as_bytes()).expect("valid submission");
            assert!(
                submission.website.trim().is_empty(),
                "a honeypot left as {unfilled} must not trip the trap"
            );
        }
    }

    #[test]
    fn json_parser_treats_an_absent_honeypot_as_empty() {
        let raw = br#"{
            "type_id":"type","subject":"valid subject","description":"a sufficiently long description",
            "form_token":"token","turnstile_token":"challenge"
        }"#;
        assert_eq!(
            parse_submission("application/json", raw)
                .expect("a body without the honeypot is a normal submission")
                .website,
            ""
        );
    }

    #[test]
    fn references_are_shape_checked_without_database_lookup() {
        assert_eq!(
            sanitize_reference("TKT-0123456789ABCDEF"),
            "TKT-0123456789ABCDEF"
        );
        assert!(sanitize_reference("TKT-guess").is_empty());
    }

    #[test]
    fn unsafe_prefill_is_dropped() {
        assert_eq!(safe_back_url("https://evil.test"), "/");
        assert_eq!(safe_back_url("/activities/42"), "/activities/42");
    }
}
