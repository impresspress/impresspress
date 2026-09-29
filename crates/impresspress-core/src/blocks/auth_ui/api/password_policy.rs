//! Single source of truth for new-password validation, shared by signup,
//! password-reset, password-change, and bootstrap-redeem. Consolidates the
//! per-path checks that had drifted (three paths hardcoded `len() < 8` and
//! skipped the common-password blocklist — audit F21).

use wafer_run::{context::Context, OutputStream};

use crate::blocks::{auth::helpers::password_min_length, crud, errors::ErrorCode};

/// A new password the account policy refuses: the error code and message a
/// handler surfaces.
pub(crate) type PasswordRefusal = (ErrorCode, String);

/// Validate a caller-supplied new password against the account policy:
/// configurable minimum length, a 1024-char maximum, no control characters,
/// and a small common-password blocklist.
///
/// The outer `Err` is the response for a policy that could not be read (the
/// configured minimum length); the inner one is the policy's refusal, which
/// each handler answers in its own shape.
pub(crate) async fn validate_new_password(
    ctx: &dyn Context,
    pw: &str,
) -> Result<Result<(), PasswordRefusal>, OutputStream> {
    let min_len = password_min_length(ctx)
        .await
        .map_err(|e| crud::db_error_internal(e, "Could not read the password policy"))?;
    Ok(check_new_password(pw, min_len))
}

/// [`validate_new_password`] once the minimum length is known.
fn check_new_password(pw: &str, min_len: usize) -> Result<(), PasswordRefusal> {
    if pw.len() < min_len {
        return Err((
            ErrorCode::PasswordTooShort,
            format!("Password must be at least {min_len} characters"),
        ));
    }
    if pw.len() > 1024 {
        return Err((
            ErrorCode::PasswordTooLong,
            "Password must not exceed 1024 characters".to_string(),
        ));
    }
    if pw.chars().any(|c| c.is_control()) {
        return Err((
            ErrorCode::InvalidInput,
            "Password must not contain control characters".to_string(),
        ));
    }
    if is_common_password(pw) {
        return Err((
            ErrorCode::InvalidInput,
            "Password is too common. Please choose a less predictable password.".to_string(),
        ));
    }
    Ok(())
}

/// [SEC-041] Top-25 most common passwords from the NordPass 2023 list.
/// Comparison is case-insensitive — `Password1` and `password1` are both
/// rejected. Embedded rather than pulled from a crate to keep dependencies
/// minimal; the list rarely drifts year-over-year and a refresh is cheap.
const COMMON_PASSWORDS: &[&str] = &[
    "123456",
    "admin",
    "12345678",
    "123456789",
    "1234",
    "12345",
    "password",
    "123",
    "aa123456",
    "1234567890",
    "user",
    "unknown",
    "1234567",
    "tmp",
    "test",
    "111111",
    "qwerty123",
    "abc123",
    "1q2w3e4r5t",
    "qwertyuiop",
    "654321",
    "iloveyou",
    "dragon",
    "monkey",
    "qwerty",
    // Common Impresspress-flavored additions that always show up in password lists
    // for new self-hosted apps. Cheap to include here.
    "password1",
    "admin123",
    "impresspress",
];

pub(crate) fn is_common_password(pw: &str) -> bool {
    COMMON_PASSWORDS.iter().any(|p| p.eq_ignore_ascii_case(pw))
}

#[cfg(test)]
mod tests {
    use super::validate_new_password;
    use crate::{blocks::errors::ErrorCode, test_support::TestContext};

    async fn check(ctx: &TestContext, pw: &str) -> Result<(), super::PasswordRefusal> {
        match validate_new_password(ctx, pw).await {
            Ok(verdict) => verdict,
            Err(_) => panic!("the password policy must be readable"),
        }
    }

    #[tokio::test]
    async fn rejects_short_common_and_control_but_accepts_strong() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID); // password_min_length defaults to 8

        // Too short.
        let e = check(&ctx, "short").await.unwrap_err();
        assert_eq!(e.0, ErrorCode::PasswordTooShort);

        // Common password (in the blocklist) even though length is fine.
        let e = check(&ctx, "password").await.unwrap_err();
        assert_eq!(e.0, ErrorCode::InvalidInput);

        // Control character.
        let e = check(&ctx, "abcdefg\u{0007}h").await.unwrap_err();
        assert_eq!(e.0, ErrorCode::InvalidInput);

        // Strong, uncommon passphrase → Ok.
        assert!(check(&ctx, "correct-horse-battery-staple-9").await.is_ok());
    }
}
