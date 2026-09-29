/// Standardized error codes for impresspress API responses.
/// Used in place of string-based error matching for reliable error handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    // Auth errors
    InvalidCredentials,
    EmailAlreadyExists,
    AccountDisabled,
    NotAuthenticated,
    InvalidToken,
    TokenExpired,
    EmailNotVerified,
    PasswordTooShort,
    PasswordTooLong,
    InvalidEmail,
    InvalidInput,

    // Authorization
    Forbidden,
    AdminRequired,

    // Resource errors
    NotFound,
    Conflict,

    // Database
    DatabaseError,

    // Payment
    PaymentNotConfigured,
    InvalidPurchaseStatus,
    RefundFailed,

    // Storage
    QuotaExceeded,
    FileTooLarge,

    // System
    InternalError,
    ConfigurationError,
    RateLimitExceeded,
}

impl ErrorCode {
    /// Stable machine-readable identifier (e.g. `"invalid_credentials"`).
    /// Surfaced in JSON error responses as the `code` field; callers should
    /// switch on this rather than parsing the human-readable message.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidCredentials => "invalid_credentials",
            Self::EmailAlreadyExists => "email_already_exists",
            Self::AccountDisabled => "account_disabled",
            Self::NotAuthenticated => "not_authenticated",
            Self::InvalidToken => "invalid_token",
            Self::TokenExpired => "token_expired",
            Self::EmailNotVerified => "email_not_verified",
            Self::PasswordTooShort => "password_too_short",
            Self::PasswordTooLong => "password_too_long",
            Self::InvalidEmail => "invalid_email",
            Self::InvalidInput => "invalid_input",
            Self::Forbidden => "forbidden",
            Self::AdminRequired => "admin_required",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::DatabaseError => "database_error",
            Self::PaymentNotConfigured => "payment_not_configured",
            Self::InvalidPurchaseStatus => "invalid_purchase_status",
            Self::RefundFailed => "refund_failed",
            Self::QuotaExceeded => "quota_exceeded",
            Self::FileTooLarge => "file_too_large",
            Self::InternalError => "internal_error",
            Self::ConfigurationError => "configuration_error",
            Self::RateLimitExceeded => "rate_limit_exceeded",
        }
    }

    /// The human half of this code, for the sites that have nothing more
    /// specific to say. [`error_response`] takes a message because most
    /// callers do have something specific; this is what
    /// `From<ErrorCode> for OutputStream` uses when the code IS the whole
    /// story.
    ///
    /// Never a repeat of [`as_str`](Self::as_str): the machine-readable code
    /// travels as `error.code` meta, and a message that restates it tells a
    /// human nothing.
    pub fn default_message(&self) -> &'static str {
        match self {
            Self::InvalidCredentials => "Invalid email or password",
            Self::EmailAlreadyExists => "That email is already registered",
            Self::AccountDisabled => "This account is disabled",
            Self::NotAuthenticated => "Not authenticated",
            Self::InvalidToken => "Invalid token",
            Self::TokenExpired => "Token expired",
            Self::EmailNotVerified => "Email address not verified",
            Self::PasswordTooShort => "Password is too short",
            Self::PasswordTooLong => "Password is too long",
            Self::InvalidEmail => "Invalid email address",
            Self::InvalidInput => "Invalid input",
            Self::Forbidden => "Access denied",
            Self::AdminRequired => "Administrator access required",
            Self::NotFound => "Not found",
            Self::Conflict => "Conflicts with the current state",
            Self::DatabaseError => "Database error",
            Self::PaymentNotConfigured => "Payments are not configured",
            Self::InvalidPurchaseStatus => "The purchase is not in a state that allows this",
            Self::RefundFailed => "The refund could not be completed",
            Self::QuotaExceeded => "Storage quota exceeded",
            Self::FileTooLarge => "File is too large",
            Self::InternalError => "Internal server error",
            Self::ConfigurationError => "Configuration error",
            Self::RateLimitExceeded => "Too many requests — try again later",
        }
    }

    /// Every variant, so a test can walk the set and a new variant added
    /// without a message here fails to compile rather than shipping one.
    pub const ALL: [Self; 24] = [
        Self::InvalidCredentials,
        Self::EmailAlreadyExists,
        Self::AccountDisabled,
        Self::NotAuthenticated,
        Self::InvalidToken,
        Self::TokenExpired,
        Self::EmailNotVerified,
        Self::PasswordTooShort,
        Self::PasswordTooLong,
        Self::InvalidEmail,
        Self::InvalidInput,
        Self::Forbidden,
        Self::AdminRequired,
        Self::NotFound,
        Self::Conflict,
        Self::DatabaseError,
        Self::PaymentNotConfigured,
        Self::InvalidPurchaseStatus,
        Self::RefundFailed,
        Self::QuotaExceeded,
        Self::FileTooLarge,
        Self::InternalError,
        Self::ConfigurationError,
        Self::RateLimitExceeded,
    ];
}

/// The refusal an [`ErrorCode`] is, when the code is the whole story.
///
/// This is the one `From` impl the orphan rules allow — `From<LocalType> for
/// OutputStream` — and it is what lets a match arm read
/// `ErrorCode::NotFound.into()` instead of spelling out
/// `error_response(ErrorCode::NotFound, "Not found")`. Callers that have
/// something more specific to say keep using [`error_response`].
impl From<ErrorCode> for wafer_run::OutputStream {
    fn from(code: ErrorCode) -> Self {
        error_response(code, code.default_message())
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Helper to create a JSON error response with a structured error code.
///
/// Maps the fine-grained impresspress [`ErrorCode`] to the coarse wafer
/// `ErrorCode` (which drives transport/status mapping) and attaches the
/// precise impresspress code as structured `error.code` meta via
/// [`wafer_run::WaferError::with_detail_code`] — the
/// `wafer_block::META_ERROR_CODE` convention. The message stays human-only;
/// the old `"[{code}] {message}"` in-band prefix is gone, so HTTP adapters
/// surface the machine-readable code as a JSON `code` field from the meta
/// rather than callers parsing it back out of the message.
pub fn error_response(code: ErrorCode, message: &str) -> wafer_run::OutputStream {
    let wafer_code = impresspress_error_code_to_wafer(code);
    wafer_run::OutputStream::error(
        wafer_run::WaferError::new(wafer_code, message.to_string()).with_detail_code(code.as_str()),
    )
}

/// Map a impresspress `ErrorCode` to a wafer `ErrorCode`.
///
/// This is the ONLY thing that decides an [`ErrorCode`]'s HTTP status, and it
/// decides it indirectly: the wafer code is what
/// `wafer_block::http_codec::error_code_to_http_status` reads, through
/// `resolve_error_status`, at the HTTP boundary. There is deliberately no
/// second status table beside this one — the `ErrorCode::status_code()` that
/// used to sit here had zero callers and disagreed with this mapping
/// (`QuotaExceeded` → 413, where `ResourceExhausted` → 429 actually ships).
pub(crate) fn impresspress_error_code_to_wafer(code: ErrorCode) -> wafer_run::ErrorCode {
    match code {
        ErrorCode::InvalidCredentials
        | ErrorCode::NotAuthenticated
        | ErrorCode::InvalidToken
        | ErrorCode::TokenExpired => wafer_run::ErrorCode::Unauthenticated,

        ErrorCode::Forbidden
        | ErrorCode::AdminRequired
        | ErrorCode::AccountDisabled
        | ErrorCode::EmailNotVerified => wafer_run::ErrorCode::PermissionDenied,

        ErrorCode::NotFound => wafer_run::ErrorCode::NotFound,

        ErrorCode::EmailAlreadyExists | ErrorCode::Conflict => wafer_run::ErrorCode::AlreadyExists,

        ErrorCode::PasswordTooShort
        | ErrorCode::PasswordTooLong
        | ErrorCode::InvalidEmail
        | ErrorCode::InvalidInput
        | ErrorCode::InvalidPurchaseStatus => wafer_run::ErrorCode::InvalidArgument,

        ErrorCode::QuotaExceeded | ErrorCode::FileTooLarge => {
            wafer_run::ErrorCode::ResourceExhausted
        }

        ErrorCode::RateLimitExceeded => wafer_run::ErrorCode::ResourceExhausted,

        ErrorCode::PaymentNotConfigured
        | ErrorCode::ConfigurationError
        | ErrorCode::DatabaseError
        | ErrorCode::InternalError
        | ErrorCode::RefundFailed => wafer_run::ErrorCode::Internal,
    }
}

/// The `info.description` of the `/openapi.json` document: how an error
/// answers, and which ones a client must not retry as they stand.
///
/// It lives in the document rather than on each operation because
/// `wafer_core::discovery::generate_openapi` describes each operation's `200`
/// only; every route shares this one error shape.
pub fn openapi_description() -> String {
    use wafer_block::wire::database::{STATEMENT_BUDGET_EXCEEDS_LIMIT, STATEMENT_BUDGET_EXHAUSTED};
    let rate_limited = ErrorCode::RateLimitExceeded.as_str();
    format!(
        "## Errors\n\n\
         A failed request is answered with a JSON body \
         `{{\"error\": \"<class>\", \"message\": \"<text>\"}}`, plus \
         `\"code\": \"<detail code>\"` when the server can say precisely what \
         went wrong. `error` is the coarse class (`NotFound`, `ResourceExhausted`, \
         ...) that sets the HTTP status; branch on `code` when it is present.\n\n\
         ## Retrying\n\n\
         - `{STATEMENT_BUDGET_EXHAUSTED}` (429) and \
         `{STATEMENT_BUDGET_EXCEEDS_LIMIT}` (400): the request needs more database \
         statements than one request may run. Sent again unchanged it fails the \
         same way, so do not retry it automatically: send less per request (fewer \
         rows in one call).\n\
         - `{rate_limited}` (429): too many requests from this client. Wait the \
         seconds the `Retry-After` header gives, then retry.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status an `ErrorCode` actually ships as, read from the response
    /// rather than from a second table beside the mapping.
    ///
    /// This replaces `test_error_code_status_codes`, which asserted a
    /// `status_code()` method with zero callers and one wrong answer: it said
    /// `QuotaExceeded | FileTooLarge => 413` while both map to
    /// `wafer_run::ErrorCode::ResourceExhausted`, which
    /// `http_codec::error_code_to_http_status` renders as **429**. Asking the
    /// response is the only assertion that cannot drift from what a client
    /// receives.
    async fn shipped_status(code: ErrorCode) -> u16 {
        match error_response(code, "message").collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(e)) => {
                wafer_block::http_codec::resolve_error_status(&e)
            }
            other => panic!("expected an error stream, got {other:?}"),
        }
    }

    /// Two 500s from the SAME failing call ship the same `error` code and the
    /// same status, but DIFFERENT `message` text.
    ///
    /// `err_internal` mints a fresh 8-byte correlation id per call and renders
    /// `Internal server error (ref: <hex>)`, so the message is unique per
    /// response by design — it is what an operator quotes into a support
    /// ticket. Every 500 in the tree goes through it.
    ///
    /// This is a contract the BROWSER depends on. `ui/assets/chrome.js`
    /// collapses a burst of identical failures — twenty `hx-trigger="load"`
    /// model badges against one unreachable backend — and keying that on the
    /// message text does not work for exactly those twenty, because the ref
    /// makes all twenty different. The key has to be the status, the code, and
    /// the message with its trailing `(ref: …)` removed, and this test is why:
    /// it reads the property off a real rendered response rather than off a
    /// fixture written to match what the JS expects.
    /// (`ui/assets/test/chrome_error_toast.test.mjs` is the other half.)
    #[tokio::test]
    async fn two_internal_errors_differ_only_by_the_correlation_ref() {
        async fn rendered(context: &str) -> serde_json::Value {
            crate::test_support::output_http_json(crate::http::err_internal(context, "boom")).await
        }

        let first = rendered("llm status failed").await;
        let second = rendered("llm status failed").await;

        assert_eq!(first["error"], serde_json::json!("Internal"));
        assert_eq!(second["error"], first["error"]);

        let (a, b) = (
            first["message"].as_str().expect("message"),
            second["message"].as_str().expect("message"),
        );
        assert!(
            a.starts_with("Internal server error (ref: ") && a.ends_with(')'),
            "the published 500 message is the sanitized one with a ref: {a:?}",
        );
        assert_ne!(a, b, "the ref is fresh per response, so the text differs");

        // …and what the browser keys on is stable once the ref is removed,
        // by the same rule `chrome.js` applies.
        assert_eq!(strip_correlation_ref(a), "Internal server error");
        assert_eq!(strip_correlation_ref(a), strip_correlation_ref(b));
    }

    /// The Rust twin of `chrome.js`'s `/\s*\(ref:[^)]*\)\s*$/` — written out
    /// rather than pulled in as a dependency, since it exists only to assert
    /// that the two halves agree on what the stable part of a 500 message is.
    fn strip_correlation_ref(message: &str) -> &str {
        match message.rfind("(ref:") {
            Some(at) if message.ends_with(')') => message[..at].trim_end(),
            _ => message,
        }
    }

    #[tokio::test]
    async fn quota_ships_as_429_not_the_413_the_deleted_table_claimed() {
        assert_eq!(shipped_status(ErrorCode::QuotaExceeded).await, 429);
        assert_eq!(shipped_status(ErrorCode::FileTooLarge).await, 429);
        assert_eq!(shipped_status(ErrorCode::RateLimitExceeded).await, 429);
    }

    #[tokio::test]
    async fn every_other_class_ships_the_status_its_wafer_code_maps_to() {
        assert_eq!(shipped_status(ErrorCode::InvalidCredentials).await, 401);
        assert_eq!(shipped_status(ErrorCode::TokenExpired).await, 401);
        assert_eq!(shipped_status(ErrorCode::Forbidden).await, 403);
        assert_eq!(shipped_status(ErrorCode::AccountDisabled).await, 403);
        assert_eq!(shipped_status(ErrorCode::NotFound).await, 404);
        assert_eq!(shipped_status(ErrorCode::Conflict).await, 409);
        assert_eq!(shipped_status(ErrorCode::EmailAlreadyExists).await, 409);
        assert_eq!(shipped_status(ErrorCode::InvalidInput).await, 400);
        assert_eq!(shipped_status(ErrorCode::PasswordTooShort).await, 400);
        assert_eq!(shipped_status(ErrorCode::DatabaseError).await, 500);
        assert_eq!(shipped_status(ErrorCode::InternalError).await, 500);
        assert_eq!(shipped_status(ErrorCode::ConfigurationError).await, 500);
    }

    #[tokio::test]
    async fn an_error_code_converts_into_a_response_carrying_its_default_message() {
        let out: wafer_run::OutputStream = ErrorCode::NotFound.into();
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(err)) => {
                assert_eq!(err.code, wafer_run::ErrorCode::NotFound);
                assert_eq!(err.detail_code(), Some("not_found"));
                assert_eq!(err.message, ErrorCode::NotFound.default_message());
                assert!(!err.message.is_empty());
            }
            other => panic!("expected an error stream, got {other:?}"),
        }
    }

    /// Every variant has a message; none of them is the machine code
    /// leaking into the human half.
    #[test]
    fn no_default_message_is_empty_or_a_repeat_of_the_code() {
        for code in ErrorCode::ALL {
            let message = code.default_message();
            assert!(!message.is_empty(), "{code} has no default message");
            assert_ne!(
                message,
                code.as_str(),
                "{code}'s default message is its machine code"
            );
        }
    }

    #[test]
    fn test_error_code_as_str() {
        assert_eq!(
            ErrorCode::InvalidCredentials.as_str(),
            "invalid_credentials"
        );
        assert_eq!(
            ErrorCode::EmailAlreadyExists.as_str(),
            "email_already_exists"
        );
        assert_eq!(ErrorCode::RateLimitExceeded.as_str(), "rate_limit_exceeded");
        assert_eq!(ErrorCode::QuotaExceeded.as_str(), "quota_exceeded");
    }

    #[test]
    fn test_error_code_display() {
        assert_eq!(format!("{}", ErrorCode::NotFound), "not_found");
        assert_eq!(format!("{}", ErrorCode::InvalidToken), "invalid_token");
    }

    #[tokio::test]
    async fn error_response_carries_code_as_structured_meta() {
        // The precise impresspress code lands in `error.code` meta (not as a
        // `"[code] "` prefix on the human message), and the coarse wafer code
        // is the transport classification.
        let out = error_response(ErrorCode::InvalidToken, "token is bad");
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(err)) => {
                assert_eq!(err.code, wafer_run::ErrorCode::Unauthenticated);
                assert_eq!(err.message, "token is bad");
                assert!(
                    !err.message.starts_with('['),
                    "message must not carry the old bracket-code prefix"
                );
                assert_eq!(err.detail_code(), Some("invalid_token"));
            }
            other => panic!("expected an error stream, got {other:?}"),
        }
    }
}
