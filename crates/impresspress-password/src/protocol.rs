//! The call between the main Cloudflare Worker and the password-hasher
//! Worker, and the names both sides and the deploy tooling share.
//!
//! On Cloudflare, password hashing and verification do not run in the Worker
//! that serves requests. They run in a Durable Object
//! ([`DURABLE_OBJECT_CLASS`]) exported by a second, small Worker — the
//! password-hasher Worker — which the main Worker reaches through a Durable
//! Object binding ([`BINDING`]) naming that Worker's script. A Worker request
//! on the Free plan is documented at 10 ms of CPU, and argon2id at OWASP's
//! recommended cost (19 MiB, 2 iterations) takes about 50-130 ms of it in
//! wasm32; a Durable Object request has a far larger allowance. The class
//! lives in a Worker of its own because Cloudflare generates no version
//! preview URLs for a Worker that implements a Durable Object, and the main
//! Worker's deploy funnel prepares and verifies each version through one.
//!
//! # One request, one answer
//!
//! The main Worker sends one JSON [`Request`] per operation as the body of a
//! `POST` to a Durable Object stub, and the object answers one JSON
//! [`Response`] with status 200 — including for a wrong password, a stored
//! hash it cannot check and a pepper fault, which are answers, not transport
//! failures. Any other status, an unreadable body, or an answer that does not
//! fit the operation means the hasher could not be asked, and the main
//! Worker reports the operation as failed. It never hashes or verifies a
//! password itself instead: a fallback would write hashes at whatever cost the
//! Worker can afford, silently.
//!
//! # Compatibility across a release
//!
//! `impresspress deploy` deploys the password-hasher Worker FIRST, then takes
//! the main Worker through its prepare/verify/promote funnel, and every
//! version of the main Worker — a preview being verified, the live one, a
//! rollback target — talks to whichever hasher is deployed at that moment.
//! So a hasher must answer the requests of the main Worker release before it,
//! not just its own. The rule:
//!
//! - A change to [`Request`] or [`Response`] that an older peer cannot read
//!   bumps [`PROTOCOL_VERSION`].
//! - The hasher answers every version from [`OLDEST_ACCEPTED_VERSION`] to
//!   [`PROTOCOL_VERSION`], in the version it was asked in; the release that
//!   bumps the version keeps [`OLDEST_ACCEPTED_VERSION`] at the previous one
//!   (a compile-time assertion below holds it there), and a later release may
//!   raise it once no deployed main Worker sends the old one.
//! - A version outside that range is answered [`Outcome::UnsupportedVersion`],
//!   which the main Worker reports as the hasher being unavailable.
//!
//! Skipping a release breaks the rule's premise: a hasher two versions ahead
//! of the live main Worker refuses its requests, and every password operation
//! on the live site answers 503 from the hasher deploy until the new main
//! Worker is promoted. Deploy each release in turn when one bumps the
//! version.
//!
//! # Who can call it
//!
//! Durable Object bindings are not authenticated beyond the account: any
//! Worker in the same Cloudflare account can bind [`DURABLE_OBJECT_CLASS`] by
//! the hasher's script name and ask it to hash or verify. The pepper keeps a
//! stolen credential table uncrackable offline, but it does not stop a
//! compromised Worker in the account — the main one included — from checking
//! password guesses against stored hashes through the hasher, one argon2id
//! verification per guess, without ever reading the key. Keep the account's
//! Workers, and who may deploy them, as trusted as the credential table.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};
use wafer_core::interfaces::crypto::service::CryptoError;

/// The Durable Object binding the main Worker reaches the hasher through.
pub const BINDING: &str = "IMPRESSPRESS_PASSWORD_HASHER";

/// The Durable Object class the password-hasher Worker exports. The
/// `durable_object` module's struct carries this name; a compile-time check
/// there keeps the two equal.
pub const DURABLE_OBJECT_CLASS: &str = "ImpresspressPasswordHasher";

/// What the deploy tooling appends to the main Worker's name to name the
/// password-hasher Worker when `impresspress.toml` does not name it.
pub const WORKER_NAME_SUFFIX: &str = "-password-hasher";

/// The main Worker var holding how many Durable Object instances hashing is
/// spread across. Written into the generated config from `impresspress.toml`'s
/// `[cloudflare.password_hasher].shards`.
pub const SHARDS_VAR: &str = "IMPRESSPRESS_PASSWORD_HASHER_SHARDS";

/// Instances hashing is spread across when [`SHARDS_VAR`] is unset.
///
/// A Durable Object instance runs one request at a time, and a hash costs
/// about 75-130 ms of CPU there, so one instance caps the whole deployment at
/// roughly ten logins a second and queues a burst behind itself. Instances
/// cost nothing while idle.
pub const DEFAULT_SHARDS: u32 = 8;

/// The accepted range of [`SHARDS_VAR`].
pub const SHARDS_RANGE: RangeInclusive<u32> = 1..=64;

/// Read [`SHARDS_VAR`]: unset is [`DEFAULT_SHARDS`]; anything else must be a
/// whole number in [`SHARDS_RANGE`].
pub fn parse_shards(raw: Option<&str>) -> Result<u32, String> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_SHARDS);
    };
    let shards = raw
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("{SHARDS_VAR} is {raw:?}; it must be a whole number"))?;
    validate_shards(shards)
}

/// Check a shard count against [`SHARDS_RANGE`].
pub fn validate_shards(shards: u32) -> Result<u32, String> {
    if SHARDS_RANGE.contains(&shards) {
        Ok(shards)
    } else {
        Err(format!(
            "{SHARDS_VAR} is {shards}; it must be between {} and {}",
            SHARDS_RANGE.start(),
            SHARDS_RANGE.end()
        ))
    }
}

/// The name of the Durable Object instance a request goes to, picked by
/// `random` (any value, e.g. four random bytes) among `shards` instances.
/// Names are stable (`shard-0` … `shard-{shards - 1}`), so the instances are
/// reused rather than created per request.
pub fn shard_name(shards: u32, random: u32) -> String {
    format!("shard-{}", random % shards.max(1))
}

/// The protocol version this build sends and answers in by default.
pub const PROTOCOL_VERSION: u32 = 1;

/// The oldest version the hasher answers. See the module docs.
pub const OLDEST_ACCEPTED_VERSION: u32 = 1;

// The hasher keeps answering the previous release's main Worker.
const _: () = assert!(
    OLDEST_ACCEPTED_VERSION <= PROTOCOL_VERSION
        && OLDEST_ACCEPTED_VERSION + 1 >= PROTOCOL_VERSION
        && OLDEST_ACCEPTED_VERSION >= 1
);

/// The versions the hasher answers.
pub const ACCEPTED_VERSIONS: RangeInclusive<u32> = OLDEST_ACCEPTED_VERSION..=PROTOCOL_VERSION;

/// What the main Worker asks the hasher. No `Debug`: it carries a password.
#[derive(Serialize, Deserialize)]
pub struct Request {
    /// The protocol version the request is written in.
    pub version: u32,
    /// The operation.
    pub operation: Operation,
}

/// One hashing operation. No `Debug`: it carries a password.
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Hash a new password.
    Hash {
        /// The password.
        password: String,
    },
    /// Check a password against a stored hash of any scheme the crypto
    /// primitives recognise.
    Verify {
        /// The password.
        password: String,
        /// The stored hash.
        hash: String,
    },
}

impl Request {
    /// A request at [`PROTOCOL_VERSION`].
    pub fn new(operation: Operation) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            operation,
        }
    }

    /// The JSON body sent to the Durable Object.
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).expect("a request of strings always serializes")
    }
}

/// What the hasher answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// The version the answer is written in: the request's, when the hasher
    /// accepts it, and the hasher's own [`PROTOCOL_VERSION`] otherwise.
    pub version: u32,
    /// The outcome.
    pub outcome: Outcome,
}

/// How an operation went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// `hash` succeeded.
    Hashed {
        /// The new hash.
        hash: String,
    },
    /// `verify`: the password matches.
    Verified,
    /// `verify`: the password is wrong.
    Mismatch,
    /// `verify`: the stored hash is one the hasher cannot check
    /// (`CryptoError::MalformedHash`).
    MalformedHash {
        /// The crypto primitives' message.
        message: String,
    },
    /// The hasher's pepper configuration stands in the way
    /// (`CryptoError::Pepper`): a key it does not hold, a pepper it requires,
    /// or pepper secrets that do not parse.
    Pepper {
        /// What is wrong, naming the variable; never key material.
        message: String,
    },
    /// The primitives failed for another reason.
    Failed {
        /// The primitives' message.
        message: String,
    },
    /// The request's version is outside the range this hasher answers.
    UnsupportedVersion {
        /// The oldest version this hasher answers.
        oldest: u32,
        /// The newest version this hasher answers.
        newest: u32,
    },
    /// The request could not be read.
    BadRequest {
        /// What was wrong with it; never the password.
        message: String,
    },
}

/// The prefix of every error that means the hasher could not be asked or
/// gave no answer this Worker can read; see [`unavailable`].
pub const UNAVAILABLE_PREFIX: &str = "password hasher unavailable: ";

/// The error an operation fails with when the hasher could not answer it:
/// the binding is not usable, the Durable Object call failed, it answered a
/// status other than 200 or a body that does not parse, or it speaks another
/// protocol version.
///
/// `CryptoError::Unavailable`, which the crypto block answers with
/// `ErrorCode::Unavailable`: the request was sound and may succeed when
/// retried, so a sign-in, sign-up, password change, password reset or
/// bootstrap-token redemption answers 503, and nothing about the stored
/// credential is inferred from it.
pub fn unavailable(reason: impl std::fmt::Display) -> CryptoError {
    CryptoError::Unavailable(format!("{UNAVAILABLE_PREFIX}{reason}"))
}

/// The prefix of every error that means the hasher answered in this Worker's
/// protocol, but with something it must not act on; see [`misanswered`].
pub const MISANSWERED_PREFIX: &str = "password hasher misanswered: ";

/// The error an operation fails with when the hasher answered, readably, with
/// something other than an answer to the question: an outcome that belongs
/// to another operation, a refusal to read the request, or a hash other than
/// the one it is meant to write.
///
/// `CryptoError::Other`, which the crypto block answers with
/// `ErrorCode::Internal`: retrying the same request gets the same answer, so
/// it is a fault, not an outage. A sign-in still answers it 503
/// (`impresspress_core::blocks::auth::check_password` does not tell the two
/// apart), and nothing about the stored credential is inferred from it.
pub fn misanswered(reason: impl std::fmt::Display) -> CryptoError {
    CryptoError::Other(format!("{MISANSWERED_PREFIX}{reason}"))
}

impl Response {
    /// Read a response body the hasher sent with status 200.
    pub fn from_body(body: &[u8]) -> Result<Self, CryptoError> {
        serde_json::from_slice(body).map_err(|e| {
            unavailable(format!(
                "its answer could not be read ({})",
                json_fault(&e, "an answer")
            ))
        })
    }

    /// The answer to a [`Operation::Hash`], as the crypto service returns it.
    ///
    /// A hash that is not argon2id at [`WRITTEN_ARGON2_PARAMS`] (peppered or
    /// not) is refused rather than stored: whatever answered, the Worker
    /// never keeps a credential weaker than the hasher is meant to write.
    pub fn into_hash(self) -> Result<String, CryptoError> {
        match self.into_answer()? {
            Outcome::Hashed { hash } => {
                check_written_hash(&hash)?;
                Ok(hash)
            }
            other => Err(unexpected("hash", &other)),
        }
    }

    /// The answer to a [`Operation::Verify`], as the crypto service returns
    /// it.
    pub fn into_verify(self) -> Result<(), CryptoError> {
        match self.into_answer()? {
            Outcome::Verified => Ok(()),
            Outcome::Mismatch => Err(CryptoError::PasswordMismatch),
            Outcome::MalformedHash { message } => Err(CryptoError::MalformedHash(message)),
            other => Err(unexpected("verify", &other)),
        }
    }

    /// The outcome, with the ones every operation shares already turned into
    /// errors. An answer in a version this build does not speak is refused
    /// before its outcome is trusted.
    fn into_answer(self) -> Result<Outcome, CryptoError> {
        if !ACCEPTED_VERSIONS.contains(&self.version) {
            return Err(unavailable(format!(
                "it answered in protocol version {}, and this Worker reads {}..={}",
                self.version, OLDEST_ACCEPTED_VERSION, PROTOCOL_VERSION
            )));
        }
        match self.outcome {
            Outcome::Pepper { message } => Err(CryptoError::Pepper(message)),
            Outcome::Failed { message } => Err(CryptoError::HashError(message)),
            Outcome::UnsupportedVersion { oldest, newest } => Err(unavailable(format!(
                "it answers protocol versions {oldest}..={newest}, and this Worker sent \
                 {PROTOCOL_VERSION}"
            ))),
            Outcome::BadRequest { message } => {
                Err(misanswered(format!("it refused the request: {message}")))
            }
            other => Ok(other),
        }
    }
}

/// The argon2id parameters every hash the hasher writes carries: OWASP's
/// recommended cost, `wafer_block_crypto::primitives::Argon2Cost::Default`.
/// `hasher`'s tests hold the two equal.
pub const WRITTEN_ARGON2_PARAMS: &str = "m=19456,t=2,p=1";

/// Check a hash the hasher returned is one it is meant to write: argon2id
/// (`$argon2id$`) or peppered argon2id (`$argon2id-hmac-sha256$`, whose
/// parameters end in `,pepper=<id>`), version 19, at exactly
/// [`WRITTEN_ARGON2_PARAMS`]. The error never contains the hash.
pub fn check_written_hash(hash: &str) -> Result<(), CryptoError> {
    let mut fields = hash.split('$');
    let well_formed = match (fields.next(), fields.next(), fields.next(), fields.next()) {
        (Some(""), Some("argon2id"), Some("v=19"), Some(params)) => params == WRITTEN_ARGON2_PARAMS,
        (Some(""), Some(crate::PEPPERED_SCHEME_ID), Some("v=19"), Some(params)) => params
            .strip_prefix(WRITTEN_ARGON2_PARAMS)
            .and_then(|rest| rest.strip_prefix(",pepper="))
            .is_some_and(|id| !id.is_empty() && !id.contains(',')),
        _ => false,
    };
    // A salt and an output follow the parameters, and nothing after them.
    let rest: Vec<&str> = fields.collect();
    if well_formed && rest.len() == 2 && rest.iter().all(|f| !f.is_empty()) {
        Ok(())
    } else {
        Err(misanswered(format!(
            "it returned a hash that is not argon2id at {WRITTEN_ARGON2_PARAMS}"
        )))
    }
}

/// What was wrong with a JSON body, without any of its contents: serde_json's
/// own message can quote the value it choked on, which may be a password or a
/// hash.
pub(crate) fn json_fault(error: &serde_json::Error, expected: &str) -> String {
    use serde_json::error::Category;
    let what = match error.classify() {
        Category::Io => "unreadable body".to_string(),
        Category::Syntax => "not JSON".to_string(),
        Category::Data => format!("not {expected}"),
        Category::Eof => "truncated body".to_string(),
    };
    format!("{what} at column {}", error.column())
}

fn unexpected(operation: &str, outcome: &Outcome) -> CryptoError {
    // Only the kind: a `Hashed` outcome carries a hash, which does not belong
    // in a log line.
    let kind = match outcome {
        Outcome::Hashed { .. } => "hashed",
        Outcome::Verified => "verified",
        Outcome::Mismatch => "mismatch",
        Outcome::MalformedHash { .. } => "malformed_hash",
        Outcome::Pepper { .. } => "pepper",
        Outcome::Failed { .. } => "failed",
        Outcome::UnsupportedVersion { .. } => "unsupported_version",
        Outcome::BadRequest { .. } => "bad_request",
    };
    misanswered(format!(
        "it answered a {operation} request with a {kind} outcome"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_counts_parse_and_default() {
        assert_eq!(parse_shards(None), Ok(DEFAULT_SHARDS));
        assert_eq!(parse_shards(Some(" 3 ")), Ok(3));
        assert_eq!(parse_shards(Some("64")), Ok(64));
        for bad in ["0", "65", "-1", "two", ""] {
            let err = parse_shards(Some(bad)).expect_err(bad);
            assert!(err.contains(SHARDS_VAR), "{err}");
        }
    }

    #[test]
    fn shard_names_stay_within_the_configured_count() {
        let names: std::collections::BTreeSet<String> =
            (0..100u32).map(|random| shard_name(4, random)).collect();
        let expected: std::collections::BTreeSet<String> =
            (0..4).map(|i| format!("shard-{i}")).collect();
        assert_eq!(names, expected);
        assert_eq!(shard_name(1, u32::MAX), "shard-0");
    }

    /// The wire format is a contract with the hasher of the previous and the
    /// next release: pin it.
    #[test]
    fn the_wire_format_is_pinned() {
        assert_eq!(
            Request::new(Operation::Verify {
                password: "pw".into(),
                hash: "$argon2id$x".into(),
            })
            .to_body(),
            r#"{"version":1,"operation":{"op":"verify","password":"pw","hash":"$argon2id$x"}}"#
        );
        assert_eq!(
            Request::new(Operation::Hash {
                password: "pw".into()
            })
            .to_body(),
            r#"{"version":1,"operation":{"op":"hash","password":"pw"}}"#
        );
        let answer = Response {
            version: 1,
            outcome: Outcome::MalformedHash {
                message: "m".into(),
            },
        };
        assert_eq!(
            serde_json::to_string(&answer).unwrap(),
            r#"{"version":1,"outcome":{"kind":"malformed_hash","message":"m"}}"#
        );
    }

    #[test]
    fn outcomes_become_the_crypto_errors_the_auth_block_classifies() {
        let at = |outcome| Response {
            version: PROTOCOL_VERSION,
            outcome,
        };
        assert_eq!(
            at(Outcome::Hashed { hash: PLAIN.into() })
                .into_hash()
                .unwrap(),
            PLAIN
        );
        at(Outcome::Verified).into_verify().unwrap();
        assert!(matches!(
            at(Outcome::Mismatch).into_verify(),
            Err(CryptoError::PasswordMismatch)
        ));
        assert!(matches!(
            at(Outcome::MalformedHash { message: "m".into() }).into_verify(),
            Err(CryptoError::MalformedHash(m)) if m == "m"
        ));
        for pepper in [
            at(Outcome::Pepper {
                message: "p".into(),
            })
            .into_verify(),
            at(Outcome::Pepper {
                message: "p".into(),
            })
            .into_hash()
            .map(drop),
        ] {
            assert!(matches!(pepper, Err(CryptoError::Pepper(m)) if m == "p"));
        }
    }

    fn assert_unavailable(result: Result<(), CryptoError>) {
        match result {
            Err(CryptoError::Unavailable(message)) => {
                assert!(message.starts_with(UNAVAILABLE_PREFIX), "{message}")
            }
            other => panic!("expected the hasher to be unavailable, got {other:?}"),
        }
    }

    fn assert_misanswered(result: Result<(), CryptoError>) {
        match result {
            Err(CryptoError::Other(message)) => {
                assert!(message.starts_with(MISANSWERED_PREFIX), "{message}")
            }
            other => panic!("expected the hasher to have misanswered, got {other:?}"),
        }
    }

    /// An answer this Worker cannot read — not JSON, or in a protocol version
    /// it does not speak — is the hasher being unavailable, which the crypto
    /// block answers 503: never a match, a mismatch or a hash.
    #[test]
    fn an_unreadable_answer_is_unavailable() {
        let at = |version, outcome| Response { version, outcome };
        assert_unavailable(
            at(
                1,
                Outcome::UnsupportedVersion {
                    oldest: 2,
                    newest: 3,
                },
            )
            .into_verify(),
        );
        // A verdict written in a version this build does not speak is not
        // trusted, even one that says "verified".
        assert_unavailable(at(PROTOCOL_VERSION + 1, Outcome::Verified).into_verify());
        assert_unavailable(at(0, Outcome::Verified).into_verify());
        assert_unavailable(Response::from_body(b"not json").map(drop));
    }

    /// A readable answer that does not answer the question asked is a fault,
    /// which the crypto block answers 500 — also never a match, a mismatch
    /// or a hash.
    #[test]
    fn an_answer_to_another_question_is_a_fault() {
        let at = |version, outcome| Response { version, outcome };
        assert_misanswered(at(1, Outcome::Hashed { hash: "h".into() }).into_verify());
        assert_misanswered(at(1, Outcome::Verified).into_hash().map(drop));
        assert_misanswered(at(1, Outcome::Mismatch).into_hash().map(drop));
        assert_misanswered(
            at(
                1,
                Outcome::BadRequest {
                    message: "x".into(),
                },
            )
            .into_verify(),
        );
    }

    const PLAIN: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0c2FsdA$aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaGhhc2g";
    const PEPPERED: &str = "$argon2id-hmac-sha256$v=19$m=19456,t=2,p=1,pepper=0011223344556677$c2FsdHNhbHRzYWx0c2FsdA$aGFzaGhhc2hoYXNoaGFzaGhhc2hoYXNoaGFzaGhhc2g";

    #[test]
    fn a_hash_at_the_written_cost_is_kept() {
        for hash in [PLAIN, PEPPERED] {
            let answer = Response {
                version: PROTOCOL_VERSION,
                outcome: Outcome::Hashed { hash: hash.into() },
            };
            assert_eq!(answer.into_hash().unwrap(), hash);
        }
    }

    /// Defence in depth: whatever answered, a hash weaker than — or other
    /// than — what the hasher writes is never handed back to be stored.
    #[test]
    fn a_weaker_or_foreign_hash_is_refused() {
        for hash in [
            // The 4 MiB preset the Worker used to write.
            "$argon2id$v=19$m=4096,t=2,p=1$c2FsdA$aGFzaA",
            "$argon2id$v=19$m=19456,t=1,p=1$c2FsdA$aGFzaA",
            "$argon2id$v=19$m=19456,t=2,p=1,x=1$c2FsdA$aGFzaA",
            "$argon2i$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA",
            "$argon2id$v=16$m=19456,t=2,p=1$c2FsdA$aGFzaA",
            "$argon2id-hmac-sha256$v=19$m=4096,t=2,p=1,pepper=00$c2FsdA$aGFzaA",
            "$argon2id-hmac-sha256$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA",
            "$argon2id-hmac-sha256$v=19$m=19456,t=2,p=1,pepper=$c2FsdA$aGFzaA",
            "$pbkdf2-sha256$i=600000$c2FsdA$aGFzaA",
            "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA",
            "$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA$extra",
            "plaintext-password",
            "",
        ] {
            let answer = Response {
                version: PROTOCOL_VERSION,
                outcome: Outcome::Hashed { hash: hash.into() },
            };
            match answer.into_hash() {
                Err(CryptoError::Other(message)) => {
                    assert!(message.starts_with(MISANSWERED_PREFIX), "{message}");
                    if !hash.is_empty() {
                        assert!(!message.contains(hash), "{message}");
                    }
                }
                other => panic!("{hash:?} must be refused, got {other:?}"),
            }
        }
    }

    /// serde_json's messages can quote the value they choked on; an
    /// unreadable answer is described without its contents.
    #[test]
    fn an_unreadable_answer_never_quotes_its_contents() {
        for body in [
            &br#"{"version":1,"outcome":{"kind":"hashed-hunter2-secret"}}"#[..],
            br#"{"version":"hunter2-secret","outcome":{"kind":"verified"}}"#,
        ] {
            match Response::from_body(body) {
                Err(CryptoError::Unavailable(message)) => {
                    assert!(!message.contains("hunter2"), "{message}")
                }
                other => panic!("expected unavailable, got {other:?}"),
            }
        }
    }
}
