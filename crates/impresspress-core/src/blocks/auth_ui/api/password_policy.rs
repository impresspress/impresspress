//! Single source of truth for new-password validation, shared by signup,
//! password-reset, password-change, and bootstrap-redeem. Consolidates the
//! per-path checks that had drifted (three paths hardcoded `len() < 8` and
//! skipped the common-password blocklist — audit F21).

use wafer_run::context::Context;

use crate::blocks::{auth::helpers::password_min_length, errors::ErrorCode};

/// Validate a caller-supplied new password against the account policy:
/// configurable minimum length, a 1024-char maximum, no control characters,
/// and a small common-password blocklist. Returns the error code + message a
/// handler should surface, or `Ok(())` when the password is acceptable.
pub(crate) async fn validate_new_password(
    ctx: &dyn Context,
    pw: &str,
) -> Result<(), (ErrorCode, String)> {
    let min_len = password_min_length(ctx).await;
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

    #[tokio::test]
    async fn rejects_short_common_and_control_but_accepts_strong() {
        let ctx = TestContext::with_auth().await; // password_min_length defaults to 8

        // Too short.
        let e = validate_new_password(&ctx, "short").await.unwrap_err();
        assert_eq!(e.0, ErrorCode::PasswordTooShort);

        // Common password (in the blocklist) even though length is fine.
        let e = validate_new_password(&ctx, "password").await.unwrap_err();
        assert_eq!(e.0, ErrorCode::InvalidInput);

        // Control character.
        let e = validate_new_password(&ctx, "abcdefg\u{0007}h")
            .await
            .unwrap_err();
        assert_eq!(e.0, ErrorCode::InvalidInput);

        // Strong, uncommon passphrase → Ok.
        assert!(
            validate_new_password(&ctx, "correct-horse-battery-staple-9")
                .await
                .is_ok()
        );
    }
}
