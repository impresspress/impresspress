//! The deploy's check that the live password-hasher Worker answers this
//! build's protocol.
//!
//! `impresspress deploy` asks the final main Worker version's
//! `/_deploy/verify` to send the hasher one request before that version is
//! promoted: a `verify` in [`PROTOCOL_VERSION`] against [`PROBE_HASH`], a
//! stored hash that names no scheme. By the protocol's contract a stored hash
//! the hasher cannot check is an answer — status 200,
//! [`Outcome::MalformedHash`] — and it is reached before any argon2
//! derivation or pepper lookup, so the probe costs the hasher no hashing and
//! reads no credential. [`judge`] accepts exactly that answer, in the version
//! it was asked in; every other answer means the hasher this version would
//! call cannot serve it, and the deploy is refused.
//!
//! [`Report`] is the probe's field in the verify response, shared by the
//! Worker that writes it and the CLI that reads it.

use serde::{Deserialize, Serialize};
use wafer_core::interfaces::crypto::service::CryptoError;

use crate::protocol::{
    misanswered, unavailable, unexpected, Operation, Outcome, Response, PROTOCOL_VERSION,
};

/// The stored hash the probe verifies against. Its scheme id is no scheme
/// the crypto primitives know, so the hasher answers
/// [`Outcome::MalformedHash`] without deriving anything, peppered or not.
pub const PROBE_HASH: &str = "$impresspress-deploy-probe$";

/// The one operation the probe sends: verify an empty password against
/// [`PROBE_HASH`].
pub fn operation() -> Operation {
    Operation::Verify {
        password: String::new(),
        hash: PROBE_HASH.to_string(),
    }
}

/// The name of the `/_deploy/verify` response field [`verify_reply`] writes
/// the [`Report`] under, and the CLI's `VerifyResponse` reads it from.
pub const VERIFY_FIELD: &str = "password_hasher";

/// The `/_deploy/verify` reply, from the rest of the verified report and the
/// probe's outcome: on a pass, status 200 and `verified` with the [`Report`]
/// under [`VERIFY_FIELD`]; on a failure, status 503 and a body naming why.
/// The 503 is what keeps the version from being promoted: the CLI retries
/// server errors, then refuses.
pub fn verify_reply(
    mut verified: serde_json::Value,
    probe: Result<Report, String>,
) -> (u16, String) {
    match probe {
        Ok(report) => {
            verified[VERIFY_FIELD] =
                serde_json::to_value(report).expect("a report of a number and a name serializes");
            (
                200,
                serde_json::to_string_pretty(&verified).expect("a JSON value serializes"),
            )
        }
        Err(reason) => (
            503,
            format!("the password-hasher Worker failed the deploy probe: {reason}"),
        ),
    }
}

/// The `password_hasher` field of the `/_deploy/verify` response:
/// `{"protocol": 1, "answer": "malformed_hash"}`. Only a passing probe
/// writes one; a failing probe fails the request instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// The version the probe was asked in, and answered in.
    pub protocol: u32,
    /// What the hasher answered.
    pub answer: Answer,
}

/// The answers a passing probe can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// [`Outcome::MalformedHash`]: the only answer the protocol allows for
    /// [`PROBE_HASH`].
    MalformedHash,
}

/// Judge the hasher's answer to [`operation`], read from a 200 response.
///
/// It passes only as [`Outcome::MalformedHash`] written in
/// [`PROTOCOL_VERSION`], the version it was asked in. An answer in any other
/// version — even one this build reads — or
/// [`Outcome::UnsupportedVersion`] is [`unavailable`]; a pepper fault is
/// [`CryptoError::Pepper`], as it would be for every sign-in; any other
/// outcome is [`misanswered`].
pub fn judge(response: Response) -> Result<Report, CryptoError> {
    if response.version != PROTOCOL_VERSION {
        return Err(unavailable(format!(
            "it answered the probe in protocol version {}, and was asked in {PROTOCOL_VERSION}",
            response.version
        )));
    }
    match response.outcome {
        Outcome::MalformedHash { .. } => Ok(Report {
            protocol: PROTOCOL_VERSION,
            answer: Answer::MalformedHash,
        }),
        Outcome::UnsupportedVersion { oldest, newest } => Err(unavailable(format!(
            "it answers protocol versions {oldest}..={newest}, and the probe was asked in \
             {PROTOCOL_VERSION}"
        ))),
        Outcome::Pepper { message } => Err(CryptoError::Pepper(message)),
        Outcome::BadRequest { message } => Err(misanswered(format!(
            "it refused the probe request: {message}"
        ))),
        other => Err(unexpected("probe", &other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        hasher,
        pepper::{password_peppers, PasswordPeppers},
        protocol::{Request, UNAVAILABLE_PREFIX},
    };

    /// The real hasher's answer to the probe body, as the Durable Object
    /// would send it.
    fn ask_hasher(peppers: Result<&PasswordPeppers, &str>) -> Response {
        let body = Request::new(operation()).to_body();
        let answer = hasher::answer(body.as_bytes(), peppers);
        Response::from_body(serde_json::to_string(&answer).unwrap().as_bytes()).unwrap()
    }

    #[test]
    fn the_real_hasher_answers_the_probe_in_the_version_asked() {
        let peppers = PasswordPeppers::default();
        let response = ask_hasher(Ok(&peppers));
        assert_eq!(response.version, PROTOCOL_VERSION);
        assert_eq!(
            judge(response).unwrap(),
            Report {
                protocol: PROTOCOL_VERSION,
                answer: Answer::MalformedHash,
            }
        );
    }

    /// A hasher that requires a pepper, or holds one, still answers the
    /// probe without a pepper fault: the hash names no scheme, so no pepper
    /// rule applies to it.
    #[test]
    fn a_peppered_hasher_answers_the_probe_too() {
        const KEY: &str = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";
        let peppers = password_peppers(Some(KEY), None, Some("true")).unwrap();
        judge(ask_hasher(Ok(&peppers))).unwrap();
    }

    #[test]
    fn the_report_is_the_named_field_the_cli_reads() {
        let report = Report {
            protocol: 1,
            answer: Answer::MalformedHash,
        };
        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            r#"{"protocol":1,"answer":"malformed_hash"}"#
        );
        for other in [
            r#"{"protocol":1,"answer":"verified"}"#,
            r#"{"protocol":1}"#,
            r#"{"protocol":1,"answer":"malformed_hash","extra":true}"#,
        ] {
            assert!(serde_json::from_str::<Report>(other).is_err(), "{other}");
        }
    }

    fn assert_unavailable(result: Result<Report, CryptoError>) {
        match result {
            Err(CryptoError::Unavailable(message)) => {
                assert!(message.starts_with(UNAVAILABLE_PREFIX), "{message}")
            }
            other => panic!("expected the hasher to be unavailable, got {other:?}"),
        }
    }

    #[test]
    fn only_an_in_version_malformed_hash_answer_passes() {
        let at = |version, outcome| Response { version, outcome };
        let malformed = || Outcome::MalformedHash {
            message: "m".into(),
        };
        // The right answer in another version, even one this build reads.
        assert_unavailable(judge(at(PROTOCOL_VERSION + 1, malformed())));
        assert_unavailable(judge(at(PROTOCOL_VERSION - 1, malformed())));
        assert_unavailable(judge(at(
            PROTOCOL_VERSION,
            Outcome::UnsupportedVersion {
                oldest: PROTOCOL_VERSION + 1,
                newest: PROTOCOL_VERSION + 1,
            },
        )));
        assert!(matches!(
            judge(at(
                PROTOCOL_VERSION,
                Outcome::Pepper {
                    message: "p".into()
                }
            )),
            Err(CryptoError::Pepper(m)) if m == "p"
        ));
        for outcome in [
            Outcome::Verified,
            Outcome::Mismatch,
            Outcome::Hashed { hash: "h".into() },
            Outcome::Failed {
                message: "f".into(),
            },
            Outcome::BadRequest {
                message: "b".into(),
            },
        ] {
            assert!(
                matches!(
                    judge(at(PROTOCOL_VERSION, outcome)),
                    Err(CryptoError::Other(_))
                ),
                "a wrong outcome must refuse"
            );
        }
    }

    /// A pass puts the report beside the verified fields, under
    /// [`VERIFY_FIELD`], with status 200; a failure is a 503 that names the
    /// reason and carries no report.
    #[test]
    fn the_verify_reply_carries_the_report_or_refuses() {
        let verified = serde_json::json!({ "ok": true, "release_asset_verified": true });
        let (status, body) = verify_reply(
            verified.clone(),
            Ok(Report {
                protocol: PROTOCOL_VERSION,
                answer: Answer::MalformedHash,
            }),
        );
        assert_eq!(status, 200);
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "ok": true,
                "release_asset_verified": true,
                "password_hasher": { "protocol": PROTOCOL_VERSION, "answer": "malformed_hash" },
            })
        );

        let (status, body) = verify_reply(verified, Err("shard-3 answered HTTP 500".into()));
        assert_eq!(status, 503);
        assert!(body.contains("shard-3 answered HTTP 500"), "{body}");
        assert!(!body.contains(VERIFY_FIELD), "{body}");
    }

    /// An unreadable body never reaches [`judge`]: it is unavailable where
    /// it is read.
    #[test]
    fn an_unreadable_answer_is_unavailable() {
        assert!(matches!(
            Response::from_body(b"Service temporarily unavailable (maintenance)"),
            Err(CryptoError::Unavailable(_))
        ));
    }
}
