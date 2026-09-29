//! What the password-hasher Worker does with a request: the whole of it, as
//! a plain function, so the Durable Object is only I/O around it and every
//! target can test it.

use wafer_block_crypto::primitives::{self, Argon2Cost, PasswordPeppers, PasswordScheme};
use wafer_core::interfaces::crypto::service::CryptoError;

use crate::protocol::{
    Operation, Outcome, Request, Response, ACCEPTED_VERSIONS, OLDEST_ACCEPTED_VERSION,
    PROTOCOL_VERSION,
};

/// What the hasher writes: argon2id at OWASP's recommended cost (19 MiB,
/// 2 iterations, 1 lane), the cost the native runtime writes too.
/// Verification ignores it and reads the cost from the stored hash, within
/// the crypto primitives' ceilings.
pub const PASSWORD_SCHEME: PasswordScheme = PasswordScheme::Argon2(Argon2Cost::Default);

/// Answer one request body.
///
/// `peppers` is the hasher's pepper configuration, or the reason it could not
/// be read from its secrets. An unreadable configuration refuses every
/// operation as a pepper fault rather than hash without the pepper the
/// operator configured.
pub fn answer(body: &[u8], peppers: Result<&PasswordPeppers, &str>) -> Response {
    // The version first, on its own, so a request from a newer peer whose
    // operation this build cannot parse is told which versions it may use.
    #[derive(serde::Deserialize)]
    struct Version {
        version: u32,
    }
    let version = match serde_json::from_slice::<Version>(body) {
        Ok(Version { version }) => version,
        Err(e) => return bad_request(PROTOCOL_VERSION, &e),
    };
    if !ACCEPTED_VERSIONS.contains(&version) {
        return Response {
            version: PROTOCOL_VERSION,
            outcome: Outcome::UnsupportedVersion {
                oldest: OLDEST_ACCEPTED_VERSION,
                newest: PROTOCOL_VERSION,
            },
        };
    }
    let request = match serde_json::from_slice::<Request>(body) {
        Ok(request) => request,
        Err(e) => return bad_request(version, &e),
    };
    let outcome = match peppers {
        Err(reason) => Outcome::Pepper {
            message: reason.to_string(),
        },
        Ok(peppers) => run(request.operation, peppers),
    };
    Response { version, outcome }
}

fn run(operation: Operation, peppers: &PasswordPeppers) -> Outcome {
    let result = match operation {
        Operation::Hash { password } => {
            primitives::hash_password_with(&password, PASSWORD_SCHEME, peppers)
                .map(|hash| Outcome::Hashed { hash })
        }
        Operation::Verify { password, hash } => {
            primitives::verify_password_any_scheme(&password, &hash, peppers)
                .map(|()| Outcome::Verified)
        }
    };
    result.unwrap_or_else(|error| match error {
        CryptoError::PasswordMismatch => Outcome::Mismatch,
        CryptoError::MalformedHash(message) => Outcome::MalformedHash { message },
        CryptoError::Pepper(message) => Outcome::Pepper { message },
        other => Outcome::Failed {
            message: other.to_string(),
        },
    })
}

/// A request that does not parse. serde_json's message names a position and
/// what it expected, never the value it read, so no password reaches it.
fn bad_request(version: u32, error: &serde_json::Error) -> Response {
    Response {
        version,
        outcome: Outcome::BadRequest {
            message: crate::protocol::json_fault(error, "a request"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pepper::password_peppers;

    /// 32 bytes of `0x2a`, base64.
    const KEY: &str = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";

    fn ask(operation: Operation, peppers: Result<&PasswordPeppers, &str>) -> Response {
        answer(Request::new(operation).to_body().as_bytes(), peppers)
    }

    fn hash(password: &str, peppers: &PasswordPeppers) -> String {
        ask(
            Operation::Hash {
                password: password.into(),
            },
            Ok(peppers),
        )
        .into_hash()
        .expect("hash")
    }

    fn verify(password: &str, hash: &str, peppers: &PasswordPeppers) -> Result<(), CryptoError> {
        ask(
            Operation::Verify {
                password: password.into(),
                hash: hash.into(),
            },
            Ok(peppers),
        )
        .into_verify()
    }

    /// New hashes are argon2id at OWASP's recommended cost — never the 4 MiB
    /// preset the Worker used to write — and they verify.
    #[test]
    fn hashes_at_the_owasp_cost_and_verifies() {
        let peppers = PasswordPeppers::default();
        let written = hash("correct horse", &peppers);
        assert!(
            written.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{written}"
        );
        // The main Worker's check of what it is handed accepts what this
        // writes: `WRITTEN_ARGON2_PARAMS` is `PASSWORD_SCHEME`'s cost.
        crate::protocol::check_written_hash(&written).expect("the written cost");
        verify("correct horse", &written, &peppers).expect("verifies");
        assert!(matches!(
            verify("wrong", &written, &peppers),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    /// The pepper is the hasher's own: a hash it writes with a key names that
    /// key, and a hasher without the key reports a pepper fault, not a wrong
    /// password.
    #[test]
    fn peppers_with_its_own_key() {
        let peppers = password_peppers(Some(KEY), None, None).expect("key");
        let written = hash("correct horse", &peppers);
        assert!(
            written.starts_with("$argon2id-hmac-sha256$v=19$m=19456,t=2,p=1,pepper="),
            "{written}"
        );
        crate::protocol::check_written_hash(&written).expect("the written cost");
        verify("correct horse", &written, &peppers).expect("verifies with the key");
        assert!(matches!(
            verify("correct horse", &written, &PasswordPeppers::default()),
            Err(CryptoError::Pepper(_))
        ));
    }

    /// Pepper secrets that do not parse refuse every operation as a pepper
    /// fault, hashing included.
    #[test]
    fn an_unreadable_pepper_refuses_every_operation() {
        let reason = "IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED is \"yes\"";
        for operation in [
            Operation::Hash {
                password: "pw".into(),
            },
            Operation::Verify {
                password: "pw".into(),
                hash: "$argon2id$v=19$m=4096,t=2,p=1$AAAA$AAAA".into(),
            },
        ] {
            assert_eq!(
                ask(operation, Err(reason)).outcome,
                Outcome::Pepper {
                    message: reason.into()
                }
            );
        }
    }

    /// A version outside the accepted range is told which versions to use,
    /// and nothing is hashed; an accepted one is answered in its own version.
    #[test]
    fn answers_only_the_accepted_versions() {
        let peppers = PasswordPeppers::default();
        for version in [0, PROTOCOL_VERSION + 1] {
            let body =
                format!(r#"{{"version":{version},"operation":{{"op":"hash","password":"pw"}}}}"#);
            assert_eq!(
                answer(body.as_bytes(), Ok(&peppers)),
                Response {
                    version: PROTOCOL_VERSION,
                    outcome: Outcome::UnsupportedVersion {
                        oldest: OLDEST_ACCEPTED_VERSION,
                        newest: PROTOCOL_VERSION,
                    },
                }
            );
        }
        for version in ACCEPTED_VERSIONS {
            let body =
                format!(r#"{{"version":{version},"operation":{{"op":"hash","password":"pw"}}}}"#);
            let response = answer(body.as_bytes(), Ok(&peppers));
            assert_eq!(response.version, version);
            assert!(matches!(response.outcome, Outcome::Hashed { .. }));
        }
    }

    /// An unreadable request is refused without echoing what it held.
    #[test]
    fn a_bad_request_never_echoes_the_password() {
        let peppers = PasswordPeppers::default();
        for body in [
            &b"not json"[..],
            br#"{"version":1,"operation":{"op":"shred","password":"hunter2-secret"}}"#,
            br#"{"version":1,"operation":{"op":"verify","password":"hunter2-secret"}}"#,
        ] {
            let response = answer(body, Ok(&peppers));
            match &response.outcome {
                Outcome::BadRequest { message } => {
                    assert!(!message.contains("hunter2"), "{message}")
                }
                other => panic!("expected a bad request, got {other:?}"),
            }
        }
    }
}
