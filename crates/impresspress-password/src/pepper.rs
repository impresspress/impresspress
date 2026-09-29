//! The password pepper's three infrastructure keys, and how a target turns
//! their values into [`PasswordPeppers`].
//!
//! A pepper is a secret key every stored password hash is keyed with (see
//! `wafer_block_crypto::primitives::PasswordPeppers`): with it, a stolen copy
//! of the credential table cannot be cracked offline unless the key is stolen
//! too. That only holds while the key lives somewhere the table does not, so
//! these keys are `IMPRESSPRESS_*` infrastructure
//! (`impresspress_core::config_vars::is_infrastructure_key`) and follow a
//! stricter path than any other key of that class:
//!
//! - The process that hashes reads them straight from where the operator put
//!   them and builds its [`PasswordPeppers`] from them: the native server from
//!   its process environment, and on Cloudflare the password-hasher Worker
//!   ([`crate::protocol`]) from its own secrets and vars. The main Cloudflare
//!   Worker never holds them: it does not hash.
//! - They are never put on either config surface (the boot map and the
//!   `wafer-run/config` service). `impresspress_core::blocks::config` answers
//!   an infrastructure key from the boot map only, so a key missing from it is
//!   unreadable through `CONFIG_GET` whatever the `variables` table holds, and
//!   `CONFIG_SET` and the admin API refuse to store one.
//! - The browser target holds no pepper: nothing in a visitor's browser can
//!   be kept from the visitor.
//!
//! This module lives in its own crate rather than in `impresspress-core`
//! because the password-hasher Worker parses the same three keys by the same
//! rules and must not link the rest of the runtime.
//!
//! # Operating it
//!
//! - Generate a key with `openssl rand -base64 32` and set it as
//!   [`PASSWORD_PEPPER_KEY_VAR`]. From then on new password hashes are
//!   peppered, and stored unpeppered ones keep verifying.
//! - Back the key up somewhere other than the deployment. Losing it makes
//!   every hash peppered with it unverifiable: those users can no longer
//!   sign in.
//! - Rotate by moving the current key into [`PASSWORD_PEPPER_PREVIOUS_KEYS_VAR`]
//!   (comma-separated, oldest first) and setting a new current key. Hashes
//!   move to the new key only when they are rewritten, so keep an old key
//!   until no stored hash names it.
//! - Set [`PASSWORD_PEPPER_REQUIRED_VAR`] to `true` only once no stored hash
//!   is unpeppered: from then on an unpeppered one is refused, which is what
//!   stops someone who can write the credential table from planting a hash
//!   of a password they know. Check first that no row of
//!   `wafer_run__auth__local_credentials` has a `password_hash` starting
//!   with `$argon2id$` or `$pbkdf2-sha256$`; any such user is locked out.

/// Re-exported so a target building its crypto service needs no second
/// import path for the type [`password_peppers`] returns.
pub use wafer_block_crypto::primitives::PasswordPeppers;
use wafer_block_crypto::primitives::PepperKey;

/// The current pepper key: standard padded base64 of at least 32 random
/// bytes. New password hashes are peppered with it. A secret.
pub const PASSWORD_PEPPER_KEY_VAR: &str = "IMPRESSPRESS_PASSWORD_PEPPER_KEY";

/// Earlier pepper keys, comma-separated, in the same form as
/// [`PASSWORD_PEPPER_KEY_VAR`]. Stored hashes naming one still verify; none
/// is used to hash. A secret.
pub const PASSWORD_PEPPER_PREVIOUS_KEYS_VAR: &str = "IMPRESSPRESS_PASSWORD_PEPPER_PREVIOUS_KEYS";

/// `true` or `false` (unset is `false`): whether a stored hash without a
/// pepper is refused.
pub const PASSWORD_PEPPER_REQUIRED_VAR: &str = "IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED";

/// Build the pepper set from the three variables' raw values, `None` for an
/// unset one.
///
/// A blank key or key list is unset. [`PASSWORD_PEPPER_REQUIRED_VAR`] must be
/// exactly `true` or `false`: a value that is neither — `1`, `yes`, a typo —
/// is an error rather than a guess, since reading it as `false` would leave
/// the deployment accepting unpeppered hashes the operator meant to refuse.
///
/// Every error names the variable it is about, and none contains key
/// material: `PepperKey::from_base64` never echoes its input, and a
/// duplicated key is named by its id, a fingerprint of the key.
pub fn password_peppers(
    key: Option<&str>,
    previous_keys: Option<&str>,
    required: Option<&str>,
) -> Result<PasswordPeppers, String> {
    let required = match required {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => {
            return Err(format!(
                "{PASSWORD_PEPPER_REQUIRED_VAR} is {other:?}; it must be `true` or `false`"
            ))
        }
    };
    PasswordPeppers::from_config(key, previous_keys, required).map_err(name_the_variable)
}

/// `PasswordPeppers::from_config`'s error, with the setting it names spelled
/// as the variable an operator sets. `from_config` labels a fault in one key
/// `current key: …` or `previous key <n>: …`; a fault in the combination
/// (required without a key, previous keys without a current one, a key given
/// twice) carries no label and names all three.
fn name_the_variable(error: wafer_core::interfaces::crypto::service::CryptoError) -> String {
    use wafer_core::interfaces::crypto::service::CryptoError;
    let message = match error {
        CryptoError::Pepper(message) => message,
        other => other.to_string(),
    };
    if let Some(rest) = message.strip_prefix("current key: ") {
        return format!("{PASSWORD_PEPPER_KEY_VAR}: {rest}");
    }
    if let Some(rest) = message.strip_prefix("previous key ") {
        return format!("{PASSWORD_PEPPER_PREVIOUS_KEYS_VAR} entry {rest}");
    }
    format!(
        "password pepper ({PASSWORD_PEPPER_KEY_VAR}, {PASSWORD_PEPPER_PREVIOUS_KEYS_VAR}, \
         {PASSWORD_PEPPER_REQUIRED_VAR}): {message}"
    )
}

/// One line for a boot log: whether a pepper is configured, the ids of its
/// keys (fingerprints, never key material) and whether it is required.
pub fn describe(peppers: &PasswordPeppers) -> String {
    match peppers.current() {
        None => "no password pepper".to_string(),
        Some(current) => {
            let previous: Vec<&str> = peppers.previous().iter().map(PepperKey::id).collect();
            format!(
                "password pepper {} (previous: [{}]), required: {}",
                current.id(),
                previous.join(", "),
                peppers.is_required()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 32 bytes of `0x01`, base64.
    const KEY_A: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
    /// 32 bytes of `0x02`, base64.
    const KEY_B: &str = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";

    #[test]
    fn unset_is_no_pepper() {
        let peppers = password_peppers(None, None, None).expect("unset is valid");
        assert!(peppers.current().is_none());
        assert!(!peppers.is_required());
        let blank = password_peppers(Some("  "), Some(""), None).expect("blank is unset");
        assert!(blank.current().is_none());
    }

    #[test]
    fn required_is_exactly_true_or_false() {
        assert!(password_peppers(Some(KEY_A), None, Some("true"))
            .expect("true")
            .is_required());
        assert!(!password_peppers(Some(KEY_A), None, Some("false"))
            .expect("false")
            .is_required());
        for bad in [
            "", " ", "1", "0", "yes", "on", "TRUE", "True", " true", "treu",
        ] {
            let err = password_peppers(Some(KEY_A), None, Some(bad)).expect_err(bad);
            assert!(err.contains(PASSWORD_PEPPER_REQUIRED_VAR), "{bad:?}: {err}");
        }
    }

    #[test]
    fn required_without_a_key_is_refused() {
        let err = password_peppers(None, None, Some("true")).expect_err("no key");
        assert!(err.contains(PASSWORD_PEPPER_REQUIRED_VAR), "{err}");
    }

    /// An invalid key is an error naming its variable, and the error never
    /// carries the value an operator typed.
    #[test]
    fn a_bad_key_names_its_variable_and_never_echoes_it() {
        let short = "c2hvcnQta2V5LW1hdGVyaWFs"; // "short-key-material"
        let not_base64 = "not base64 at all: s3cr3t-p3pp3r!";
        for bad in [short, not_base64] {
            let err = password_peppers(Some(bad), None, None).expect_err(bad);
            assert!(err.contains(PASSWORD_PEPPER_KEY_VAR), "{err}");
            assert!(!err.contains(bad), "the error echoed the key: {err}");
            assert!(!err.contains("s3cr3t"), "the error echoed the key: {err}");

            let err = password_peppers(Some(KEY_A), Some(&format!("{KEY_B},{bad}")), None)
                .expect_err(bad);
            assert!(
                err.contains(PASSWORD_PEPPER_PREVIOUS_KEYS_VAR) && err.contains("entry 2"),
                "{err}"
            );
            assert!(!err.contains(bad), "the error echoed the key: {err}");
        }
    }

    #[test]
    fn an_empty_previous_key_entry_is_refused() {
        let err =
            password_peppers(Some(KEY_A), Some(&format!("{KEY_B} ,")), None).expect_err("empty");
        assert!(
            err.contains(PASSWORD_PEPPER_PREVIOUS_KEYS_VAR) && err.contains("entry 2"),
            "{err}"
        );
    }

    #[test]
    fn previous_keys_are_kept_apart_from_the_current_one() {
        let peppers = password_peppers(Some(KEY_A), Some(KEY_B), None).expect("rotation");
        assert_eq!(peppers.previous().len(), 1);
        assert_ne!(
            peppers.current().expect("current").id(),
            peppers.previous()[0].id()
        );
    }

    /// Neither the boot log line nor `Debug` carries key material.
    #[test]
    fn describe_and_debug_show_ids_only() {
        let peppers = password_peppers(Some(KEY_A), Some(KEY_B), Some("true")).expect("valid");
        let raw_a = [1u8; 32];
        for shown in [describe(&peppers), format!("{peppers:?}")] {
            assert!(!shown.contains(KEY_A) && !shown.contains(KEY_B), "{shown}");
            assert!(!shown.contains(&format!("{raw_a:?}")), "{shown}");
            assert!(
                shown.contains(peppers.current().expect("current").id()),
                "{shown}"
            );
        }
        assert_eq!(describe(&PasswordPeppers::default()), "no password pepper");
    }
}
