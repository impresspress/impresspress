//! Bucket-name and storage-key validation rules. `pub(in crate::blocks::files)`
//! so the share-creation path (`cloud.rs`) can enforce exactly the same
//! rules rather than re-inlining copies that drift.

/// Validate a storage key for path traversal attacks.
/// Rejects keys containing `..`, absolute paths, null bytes, or backslashes.
///
/// SEC-064: backslash is rejected because backends running on Windows-style
/// paths (or any backend that ever normalises `\` to `/`) would otherwise
/// allow `..\..\etc\passwd`-style traversal that the `..` check alone would
/// not catch when the segment separator is `\` instead of `/`.
///
/// `pub(in crate::blocks::files)` so the share-creation path
/// (`cloud.rs::handle_create_share`) enforces exactly the same rule rather
/// than re-inlining a copy that drifts (the SEC-064 backslash check was the
/// missing piece there).
pub(in crate::blocks::files) fn is_valid_storage_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains("..")
        && !key.starts_with('/')
        && !key.contains('\0')
        && !key.contains('\\')
}

/// Minimum / maximum bucket-name length (S3-compatible).
pub(in crate::blocks::files) const BUCKET_NAME_MIN_LEN: usize = 3;
pub(in crate::blocks::files) const BUCKET_NAME_MAX_LEN: usize = 63;

/// HTML5 `pattern=` attribute source for the bucket-name input — the single
/// source of truth shared with the server-side [`is_valid_bucket_name`] check
/// so the client modal and the API enforce identically. S3-compatible:
/// lowercase letters, digits, and hyphens; must start and end with a letter
/// or digit. (Length is enforced separately via `minlength`/`maxlength` on the
/// input and the length check in [`is_valid_bucket_name`].)
pub(in crate::blocks::files) const BUCKET_NAME_PATTERN: &str = "[a-z0-9]([a-z0-9-]*[a-z0-9])?";

/// Validate a bucket name against the S3-compatible rule the client modal
/// advertises ([`BUCKET_NAME_PATTERN`] + length bounds): 3–63 chars,
/// lowercase letters / digits / hyphens, must start and end with a letter or
/// digit. This rejects path traversal (`..`, `/`, `\`), NUL, uppercase, and
/// leading/trailing hyphens by construction.
///
/// `pub(in crate::blocks::files)` so the share path uses the identical rule.
pub(in crate::blocks::files) fn is_valid_bucket_name(name: &str) -> bool {
    let len = name.len();
    if !(BUCKET_NAME_MIN_LEN..=BUCKET_NAME_MAX_LEN).contains(&len) {
        return false;
    }
    let bytes = name.as_bytes();
    let is_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    // First and last char must be a lowercase letter or digit.
    if !is_alnum(bytes[0]) || !is_alnum(bytes[len - 1]) {
        return false;
    }
    // Interior chars: lowercase letter, digit, or hyphen.
    bytes.iter().all(|&b| is_alnum(b) || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_storage_key() {
        // Valid keys
        assert!(is_valid_storage_key("file.txt"));
        assert!(is_valid_storage_key("dir/file.txt"));
        assert!(is_valid_storage_key("a/b/c/file.txt"));
        assert!(is_valid_storage_key("file-name_123.txt"));

        // Invalid keys
        assert!(!is_valid_storage_key(""));
        assert!(!is_valid_storage_key("../etc/passwd"));
        assert!(!is_valid_storage_key("dir/../secret"));
        assert!(!is_valid_storage_key("/absolute/path"));
        assert!(!is_valid_storage_key("file\0name"));
        assert!(!is_valid_storage_key(".."));
    }

    #[test]
    fn test_is_valid_bucket_name() {
        // Valid bucket names (S3-compatible: 3-63 chars, lowercase/digits/
        // hyphens, start+end alnum).
        assert!(is_valid_bucket_name("my-bucket"));
        assert!(is_valid_bucket_name("bucket123"));
        assert!(is_valid_bucket_name("uploads"));
        assert!(is_valid_bucket_name("a1b"));

        // Invalid bucket names
        assert!(!is_valid_bucket_name(""));
        assert!(!is_valid_bucket_name("../other"));
        assert!(!is_valid_bucket_name("bucket/subdir"));
        assert!(!is_valid_bucket_name("bucket\0name"));
        assert!(!is_valid_bucket_name(".."));
        // Too short / too long.
        assert!(!is_valid_bucket_name("ab"));
        assert!(!is_valid_bucket_name(&"a".repeat(64)));
        // Uppercase rejected (S3 rule + matches the modal pattern).
        assert!(!is_valid_bucket_name("MyBucket"));
        // Leading / trailing hyphen rejected (start+end must be alnum).
        assert!(!is_valid_bucket_name("-bucket"));
        assert!(!is_valid_bucket_name("bucket-"));
        // Backslash rejected (SEC-064; not in the allowed alphabet).
        assert!(!is_valid_bucket_name("bucket\\name"));
    }

    /// The server-side validator enforces the same alphabet the HTML
    /// `pattern=` attribute ([`BUCKET_NAME_PATTERN`]) advertises, so the client
    /// modal and the API agree on what a valid bucket name is (modulo length,
    /// which the input enforces separately via minlength/maxlength). This pins
    /// the cases the pattern accepts/rejects against the validator.
    #[test]
    fn bucket_name_validator_matches_advertised_pattern() {
        // Sanity-check the constant is the S3 alphabet we documented.
        assert_eq!(BUCKET_NAME_PATTERN, "[a-z0-9]([a-z0-9-]*[a-z0-9])?");
        // Pattern-accepted names (length-valid) the validator must accept.
        for name in ["my-bucket", "bucket123", "a1b", "abc"] {
            assert!(is_valid_bucket_name(name), "validator should accept {name}");
        }
        // Pattern-rejected names the validator must reject.
        for name in ["MyBucket", "-bucket", "bucket-", "bucket/sub", "bucket\\x"] {
            assert!(
                !is_valid_bucket_name(name),
                "validator should reject {name}"
            );
        }
    }
}
