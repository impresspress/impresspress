//! CryptoService for Cloudflare Workers.
//!
//! JWT policy — HS256, `exp` required on verify, per-block HKDF-derived
//! keys for `sign_for`/`verify_for`, minimum secret length — delegates to
//! [`Argon2JwtCryptoService`], the same engine the native runtime uses, so
//! tokens are interchangeable across deployment targets. (Historically this
//! service silently dropped per-block key derivation by inheriting the
//! trait's master-key fallbacks, which is exactly the kind of drift behind
//! the PR #155 → #170 production auth regression.)
//!
//! Passwords are not hashed or verified in this Worker. [`PasswordHasher`]
//! sends each `hash` and `compare_hash` to the password-hasher Worker's
//! Durable Object over the [`protocol::BINDING`] binding, where argon2id runs
//! at OWASP's recommended cost (19 MiB, 2 iterations) with the pepper that
//! Worker reads from its own secrets. That cost takes about 50-130 ms of CPU
//! in wasm32, and a Worker request on the Free plan is documented at 10 ms; a
//! Durable Object request is allowed far more. See
//! [`impresspress_password::protocol`] for the call and its compatibility
//! rule. Each call adds a round trip to the Durable Object, about 60-100 ms of
//! wall time.
//!
//! The hasher verifies against whichever scheme and cost the **stored hash**
//! names — argon2id at any cost up to the crypto primitives' ceilings (the
//! 4 MiB hashes this Worker used to write included), PBKDF2 from the browser
//! target, peppered or not — so no stored credential is stranded.
//!
//! When the hasher cannot answer — the binding is missing, the Durable Object
//! errors or is unreachable, or it answers something this Worker cannot read
//! — the operation fails as `CryptoError::Unavailable`
//! ([`protocol::unavailable`]), and a sign-in, sign-up, password change,
//! password reset or bootstrap-token redemption answers 503. When it answers
//! readably but wrongly — an outcome for another operation, or a hash weaker
//! than it is meant to write — the operation fails as a fault
//! ([`protocol::misanswered`]): a sign-in still answers 503, the others 500. This Worker never hashes or verifies a
//! password itself instead: a fallback would write hashes at a cost its CPU
//! limit allows, silently.

use std::{collections::BTreeMap, time::Duration};

use impresspress_password::protocol::{self, Operation};
use wafer_block_crypto::{primitives, service::Argon2JwtCryptoService};
use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

/// CryptoService backing the CF Worker runtime. See module docs for policy.
pub struct ImpresspressCryptoService {
    jwt_secret: String,
    /// Lazily constructed once, on first use, instead of once per
    /// sign/verify/sign_for/verify_for call — `Argon2JwtCryptoService::new`
    /// re-clones `jwt_secret` and re-validates its length every time it's
    /// called, which is pure overhead after the first (successful or
    /// failed) construction. `OnceLock` degrades to a plain guarded cell on
    /// wasm32 (single-threaded), so this is safe despite every
    /// `CryptoService` method taking `&self`.
    ///
    /// The error side is stored pre-stringified (`String`, not
    /// `CryptoError`) because `CryptoError` isn't `Clone`; a fresh
    /// `CryptoError::Other` is rebuilt from the cached message on every
    /// call after the first, so a missing/short secret still fails
    /// consistently rather than only on the first call.
    jwt_engine: std::sync::OnceLock<Result<Argon2JwtCryptoService, String>>,
    /// Where passwords are hashed and verified.
    hasher: PasswordHasher,
}

impl ImpresspressCryptoService {
    pub fn new(jwt_secret: String, hasher: PasswordHasher) -> Self {
        Self {
            jwt_secret,
            jwt_engine: std::sync::OnceLock::new(),
            hasher,
        }
    }

    /// Borrow the shared JWT engine, constructing it on first use. Fails
    /// when the secret is missing or shorter than `MIN_JWT_SECRET_LEN` —
    /// surfaced per operation rather than at worker boot, because the
    /// worker constructs this service before config is necessarily
    /// complete and a broken-auth deployment beats a boot-looping one.
    fn jwt(&self) -> Result<&Argon2JwtCryptoService, CryptoError> {
        self.jwt_engine
            .get_or_init(|| {
                Argon2JwtCryptoService::new(self.jwt_secret.clone()).map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|msg| CryptoError::Other(msg.clone()))
    }
}

#[wafer_block::wafer_async_trait]
impl CryptoService for ImpresspressCryptoService {
    /// The hash the hasher returns is checked to be argon2id at the cost it is
    /// meant to write ([`protocol::check_written_hash`]) before it is handed
    /// back to be stored.
    async fn hash(&self, password: &str) -> Result<String, CryptoError> {
        self.hasher
            .ask(Operation::Hash {
                password: password.to_string(),
            })
            .await?
            .into_hash()
    }

    /// Verified by the hasher against whichever scheme the **stored hash**
    /// names, not the one it writes: a PBKDF2 credential from the browser
    /// target, or an argon2id one at any cost within the ceilings, verifies,
    /// and a string naming no known scheme is `MalformedHash`, never a
    /// mismatch.
    async fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        self.hasher
            .ask(Operation::Verify {
                password: password.to_string(),
                hash: hash.to_string(),
            })
            .await?
            .into_verify()
    }

    async fn sign_for(
        &self,
        block_id: &str,
        claims: BTreeMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        self.jwt()?.sign_for(block_id, claims, expiry).await
    }

    async fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
        self.jwt()?.verify_for(block_id, token).await
    }

    async fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        primitives::random_bytes(n)
    }
}

/// The main Worker's side of the password-hasher call: which Durable Object
/// namespace, spread across how many instances.
pub struct PasswordHasher {
    /// The way to the Durable Object, or why there is none. A missing binding
    /// fails each password operation rather than the Worker's boot, so a
    /// deployment with a broken hasher still serves everything that does not
    /// take a password.
    transport: Result<Box<dyn HasherTransport>, String>,
    /// How many instances requests are spread across
    /// ([`protocol::SHARDS_VAR`]).
    shards: u32,
}

impl PasswordHasher {
    /// The hasher reached through this Worker's [`protocol::BINDING`]
    /// binding, spread across `shards` instances.
    pub fn from_env(env: &worker::Env, shards: u32) -> Self {
        let transport = env
            .durable_object(protocol::BINDING)
            .map(|namespace| Box::new(DurableObjectTransport { namespace }) as Box<_>)
            .map_err(|e| {
                format!(
                    "the {} Durable Object binding is not usable ({e}); the generated \
                     wrangler config binds it to the password-hasher Worker",
                    protocol::BINDING
                )
            });
        Self { transport, shards }
    }

    #[cfg(test)]
    fn with_transport(transport: impl HasherTransport + 'static, shards: u32) -> Self {
        Self {
            transport: Ok(Box::new(transport)),
            shards,
        }
    }

    /// Send one operation to one instance and read its answer. Every way the
    /// call can fail is [`protocol::unavailable`].
    async fn ask(&self, operation: Operation) -> Result<protocol::Response, CryptoError> {
        let transport = self.transport.as_ref().map_err(protocol::unavailable)?;
        let random = primitives::random_bytes(4)?;
        let random = u32::from_le_bytes([random[0], random[1], random[2], random[3]]);
        let shard = protocol::shard_name(self.shards, random);
        let (status, body) = transport
            .post(&shard, protocol::Request::new(operation).to_body())
            .await
            .map_err(|e| protocol::unavailable(format!("{shard}: {e}")))?;
        if status != 200 {
            return Err(protocol::unavailable(format!(
                "{shard} answered HTTP {status}"
            )));
        }
        protocol::Response::from_body(&body)
    }
}

/// How a request reaches a hasher instance: the Durable Object binding in
/// production, a fake in tests.
#[async_trait::async_trait(?Send)]
trait HasherTransport {
    /// `POST` `body` to the instance named `shard`; the answer's status and
    /// body, or why there is none.
    async fn post(&self, shard: &str, body: String) -> Result<(u16, Vec<u8>), String>;
}

struct DurableObjectTransport {
    namespace: worker::ObjectNamespace,
}

#[async_trait::async_trait(?Send)]
impl HasherTransport for DurableObjectTransport {
    async fn post(&self, shard: &str, body: String) -> Result<(u16, Vec<u8>), String> {
        let stub = self
            .namespace
            .get_by_name(shard)
            .map_err(|e| e.to_string())?;
        let mut init = worker::RequestInit::new();
        init.with_method(worker::Method::Post)
            .with_body(Some(wasm_bindgen::JsValue::from_str(&body)));
        // The URL is not routed anywhere: a Durable Object stub delivers the
        // request to the instance whatever its host.
        let request = worker::Request::new_with_init("https://password-hasher/", &init)
            .map_err(|e| e.to_string())?;
        let mut response = stub
            .fetch_with_request(request)
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status_code();
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        Ok((status, bytes))
    }
}

// Run by the `cloudflare-wasm-test` CI job.
//
// The hasher here is a fake Durable Object namespace — a JavaScript object
// the real `worker::Env::durable_object` accepts — whose stubs answer with
// `impresspress_password::hasher::answer`, the function the real Durable
// Object runs, under the fake's OWN pepper configuration, as the
// password-hasher Worker's secrets would be. So these tests drive the
// production transport (`PasswordHasher::from_env` → `get_by_name` →
// `fetch_with_request`) end to end; only the Durable Object runtime is faked.
#[cfg(all(test, target_arch = "wasm32"))]
pub(crate) mod test_support {
    use std::{cell::RefCell, rc::Rc};

    use impresspress_password::{hasher, pepper::PasswordPeppers, protocol};
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};

    use super::{ImpresspressCryptoService, PasswordHasher};

    /// How the fake Durable Object answers a request body.
    pub(crate) type Answer = Rc<dyn Fn(&[u8]) -> (u16, String)>;

    /// A fake `DurableObjectNamespace` binding and what it saw.
    pub(crate) struct FakeHasher {
        pub(crate) env: worker::Env,
        /// The instance name of every request, in order.
        pub(crate) shards: Rc<RefCell<Vec<String>>>,
        _handler: Closure<dyn FnMut(String, web_sys_request::Request) -> js_sys::Promise>,
    }

    /// `worker` re-exports the `web_sys` types the fake receives.
    mod web_sys_request {
        pub(super) use worker::worker_sys::web_sys::Request;
    }

    impl FakeHasher {
        /// A binding whose instances answer with `answer`.
        pub(crate) fn new(answer: Answer) -> Self {
            let shards = Rc::new(RefCell::new(Vec::new()));
            let seen = shards.clone();
            let handler = Closure::new(
                move |shard: String, request: web_sys_request::Request| -> js_sys::Promise {
                    seen.borrow_mut().push(shard);
                    let answer = answer.clone();
                    worker::wasm_bindgen_futures::future_to_promise(async move {
                        let body = request.text()?;
                        let body = worker::wasm_bindgen_futures::JsFuture::from(body)
                            .await?
                            .as_string()
                            .unwrap_or_default();
                        let (status, text) = answer(body.as_bytes());
                        let init = worker::worker_sys::web_sys::ResponseInit::new();
                        init.set_status(status);
                        worker::worker_sys::web_sys::Response::new_with_opt_str_and_init(
                            Some(&text),
                            &init,
                        )
                        .map(JsValue::from)
                    })
                },
            );
            // The class name matters: `Env::durable_object` checks the
            // binding's constructor is a `DurableObjectNamespace`.
            let make = js_sys::Function::new_with_args(
                "handler",
                "class DurableObjectNamespace { \
                     getByName(name) { return { fetch: (request) => handler(name, request) }; } \
                 } \
                 return new DurableObjectNamespace();",
            );
            let namespace = make
                .call1(&JsValue::NULL, handler.as_ref())
                .expect("build the fake namespace");
            let env = js_sys::Object::new();
            js_sys::Reflect::set(&env, &JsValue::from_str(protocol::BINDING), &namespace)
                .expect("bind the fake namespace");
            Self {
                env: JsValue::from(env).unchecked_into::<worker::Env>(),
                shards,
                _handler: handler,
            }
        }

        /// A binding whose instances run the real hasher with `peppers`.
        pub(crate) fn running(peppers: PasswordPeppers) -> Self {
            Self::new(Rc::new(move |body| {
                let answer = hasher::answer(body, Ok(&peppers));
                (200, serde_json::to_string(&answer).expect("encode"))
            }))
        }

        pub(crate) fn service(&self, shards: u32) -> ImpresspressCryptoService {
            ImpresspressCryptoService::new(
                "test-secret-padded-to-32-bytes-or-more".to_string(),
                PasswordHasher::from_env(&self.env, shards),
            )
        }
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use std::rc::Rc;

    use impresspress_password::{
        pepper::{password_peppers, PasswordPeppers},
        protocol::{self, Outcome},
    };
    use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{
        test_support::{Answer, FakeHasher},
        HasherTransport, ImpresspressCryptoService, PasswordHasher,
    };

    /// A pepper key as `openssl rand -base64 32` prints one: 32 bytes of
    /// `0x2a`.
    const PEPPER_KEY: &str = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";

    fn unavailable(result: Result<impl std::fmt::Debug, CryptoError>) {
        match result {
            Err(CryptoError::Unavailable(message)) => assert!(
                message.starts_with(protocol::UNAVAILABLE_PREFIX),
                "{message}"
            ),
            other => panic!("expected the hasher to be unavailable, got {other:?}"),
        }
    }

    fn misanswered(result: Result<impl std::fmt::Debug, CryptoError>) {
        match result {
            Err(CryptoError::Other(message)) => assert!(
                message.starts_with(protocol::MISANSWERED_PREFIX),
                "{message}"
            ),
            other => panic!("expected the hasher to have misanswered, got {other:?}"),
        }
    }

    /// A password hashed through the binding verifies through it, and is
    /// argon2id at OWASP's cost: the Worker no longer writes the 4 MiB preset.
    #[wasm_bindgen_test]
    async fn a_hash_round_trips_through_the_durable_object() {
        let fake = FakeHasher::running(PasswordPeppers::default());
        let svc = fake.service(4);
        let hash = svc
            .hash("correct horse battery staple")
            .await
            .expect("hash");
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "the hasher writes OWASP-strength argon2id: {hash}"
        );
        svc.compare_hash("correct horse battery staple", &hash)
            .await
            .expect("a freshly written credential verifies");
        match svc.compare_hash("wrong", &hash).await {
            Err(CryptoError::PasswordMismatch) => {}
            other => panic!("expected PasswordMismatch, got {other:?}"),
        }
        assert_eq!(
            fake.shards.borrow().len(),
            3,
            "every operation went to the hasher"
        );
    }

    /// libargon2 known-answer hashes (argon2-cffi `low_level.hash_secret`,
    /// salt `00..0f`), password `correcthorsebatterystaple`: the 4 MiB preset
    /// this Worker wrote before hashing moved to the Durable Object, the
    /// default preset, and the 46 MiB ceiling.
    const KAT_PASSWORD: &str = "correcthorsebatterystaple";
    const KAT_BY_CLASS: [&str; 3] = [
        "$argon2id$v=19$m=4096,t=2,p=1$AAECAwQFBgcICQoLDA0ODw$DOJe9Dre1CKOGYGj/SicLaOiPXVXJ1Jame2jnwMAoGU",
        "$argon2id$v=19$m=19456,t=2,p=1$AAECAwQFBgcICQoLDA0ODw$7RmKhudBqFsX3LFSkYHsVYCSSV/j3hL/zZ9AjkvygIo",
        "$argon2id$v=19$m=47104,t=1,p=1$AAECAwQFBgcICQoLDA0ODw$xnXTdHV0WrguRO6PuHmv73XvW60GvsB6rAVhzZddIts",
    ];

    /// Every stored hash keeps verifying: a 4 MiB one this Worker wrote, a
    /// native one at the default cost, one at the memory ceiling — the cost
    /// comes out of the stored string.
    #[wasm_bindgen_test]
    async fn stored_hashes_of_every_cost_verify_including_the_old_constrained_ones() {
        let fake = FakeHasher::running(PasswordPeppers::default());
        let svc = fake.service(2);
        for hash in KAT_BY_CLASS {
            svc.compare_hash(KAT_PASSWORD, hash)
                .await
                .unwrap_or_else(|e| panic!("{hash}: {e:?}"));
            match svc.compare_hash("wrong", hash).await {
                Err(CryptoError::PasswordMismatch) => {}
                other => panic!("{hash}: expected PasswordMismatch, got {other:?}"),
            }
        }
    }

    /// A stored hash above the 46 MiB ceiling is refused as a hash the hasher
    /// will not run, never as a wrong password.
    #[wasm_bindgen_test]
    async fn a_hash_above_the_memory_ceiling_is_malformed_not_a_mismatch() {
        const OVER_CEILING: &str = "$argon2id$v=19$m=65536,t=3,p=4$AAECAwQFBgcICQoLDA0ODw$ig/1Ydv8lGLja+cEry2Q+/MeqvCw1xexf4oGjq9DiAQ";
        let fake = FakeHasher::running(PasswordPeppers::default());
        let svc = fake.service(1);
        match svc.compare_hash(KAT_PASSWORD, OVER_CEILING).await {
            Err(CryptoError::MalformedHash(msg)) => assert!(
                msg.contains("m=65536 exceeds the ceiling of 47104"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected MalformedHash, got {other:?}"),
        }
    }

    /// A PBKDF2 credential from the browser target verifies, and a scheme
    /// nobody knows is malformed, not a mismatch. Fixture: password `correct
    /// horse battery staple`, salt `00..0f`, `i=10000`.
    #[wasm_bindgen_test]
    async fn other_schemes_are_dispatched_on_the_stored_hash() {
        const BROWSER_HASH: &str =
            "$pbkdf2-sha256$i=10000$AAECAwQFBgcICQoLDA0ODw==$2flfZcLfnShdJogjAMpb4p4+1QBVZmODXExi4nBRUCI=";
        let fake = FakeHasher::running(PasswordPeppers::default());
        let svc = fake.service(1);
        svc.compare_hash("correct horse battery staple", BROWSER_HASH)
            .await
            .expect("a PBKDF2 credential from the browser target verifies");
        match svc
            .compare_hash("pw", "$scrypt$ln=16,r=8,p=1$c2FsdA$aGFzaA")
            .await
        {
            Err(CryptoError::MalformedHash(msg)) => assert!(
                msg.contains("unrecognised password hash scheme"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected a MalformedHash for an unknown scheme, got {other:?}"),
        }
    }

    /// The pepper is the hasher's, from its own environment: this Worker holds
    /// none, yet a new hash is peppered, and a stored hash naming a key the
    /// hasher does not hold is a pepper fault — which the auth block logs with
    /// the user id and answers 503 — not a wrong password.
    #[wasm_bindgen_test]
    async fn the_pepper_comes_from_the_hasher() {
        let peppered = FakeHasher::running(
            password_peppers(Some(PEPPER_KEY), None, None).expect("a valid key"),
        );
        let hash = peppered
            .service(1)
            .hash("correct horse")
            .await
            .expect("hash");
        assert!(
            hash.starts_with("$argon2id-hmac-sha256$v=19$m=19456,t=2,p=1,pepper="),
            "{hash}"
        );
        peppered
            .service(1)
            .compare_hash("correct horse", &hash)
            .await
            .expect("verifies where the key is");

        let keyless = FakeHasher::running(PasswordPeppers::default());
        match keyless
            .service(1)
            .compare_hash("correct horse", &hash)
            .await
        {
            Err(CryptoError::Pepper(message)) => {
                assert_eq!(
                    CryptoError::Pepper(message).to_string().split(": ").next(),
                    Some("password pepper"),
                    "the auth block classifies a pepper fault by this prefix"
                );
            }
            other => panic!("expected a pepper fault, got {other:?}"),
        }
    }

    /// **Never a weaker hash.** When the hasher cannot answer, `hash` and
    /// `compare_hash` fail as unavailable; when it answers another question,
    /// as a fault. A fallback to hashing in this Worker would return a hash
    /// (and a verdict) here, and fail this test.
    #[wasm_bindgen_test]
    async fn a_failing_hasher_fails_the_operation_and_nothing_is_hashed_locally() {
        // Stored with the 4 MiB preset, which a local fallback would accept.
        const STORED: &str = KAT_BY_CLASS[0];
        let failing: [(&str, AnswerFactory); 4] = [
            ("an HTTP error", || {
                Rc::new(|_| (500, "internal error".into()))
            }),
            ("an unreadable answer", || {
                Rc::new(|_| (200, "Service temporarily unavailable (maintenance)".into()))
            }),
            ("a newer protocol only", || {
                Rc::new(|_| {
                    let answer = protocol::Response {
                        version: protocol::PROTOCOL_VERSION,
                        outcome: Outcome::UnsupportedVersion {
                            oldest: protocol::PROTOCOL_VERSION + 1,
                            newest: protocol::PROTOCOL_VERSION + 1,
                        },
                    };
                    (200, serde_json::to_string(&answer).expect("encode"))
                })
            }),
            ("an answer to another question", || {
                Rc::new(|_| {
                    let answer = protocol::Response {
                        version: protocol::PROTOCOL_VERSION,
                        outcome: Outcome::Verified,
                    };
                    (200, serde_json::to_string(&answer).expect("encode"))
                })
            }),
        ];
        for (case, answer) in failing {
            // The fake must outlive the service: its stubs call back into it.
            let fake = FakeHasher::new(answer());
            let svc = fake.service(1);
            if case == "an answer to another question" {
                misanswered(svc.hash("pw").await);
            } else {
                match svc.hash("pw").await {
                    Err(CryptoError::Unavailable(message)) => assert!(
                        message.starts_with(protocol::UNAVAILABLE_PREFIX),
                        "{case}: {message}"
                    ),
                    other => panic!("{case}: expected unavailable, got {other:?}"),
                }
                unavailable(svc.compare_hash(KAT_PASSWORD, STORED).await);
            }
            assert!(
                !fake.shards.borrow().is_empty(),
                "{case}: the fake hasher was never reached, so this proved nothing"
            );
        }

        // No binding at all: the same, and the Worker still builds the service.
        let unbound = ImpresspressCryptoService::new(
            "test-secret-padded-to-32-bytes-or-more".to_string(),
            PasswordHasher::from_env(
                &JsValue::from(js_sys::Object::new()).unchecked_into::<worker::Env>(),
                4,
            ),
        );
        unavailable(unbound.hash("pw").await);
        unavailable(unbound.compare_hash(KAT_PASSWORD, STORED).await);
        assert!(
            unbound
                .sign_for(
                    "impresspress/auth",
                    std::collections::BTreeMap::new(),
                    std::time::Duration::from_secs(60)
                )
                .await
                .is_ok(),
            "tokens do not depend on the hasher"
        );
    }

    /// Defence in depth: a hasher that hands back a hash weaker than the one
    /// it is meant to write — the 4 MiB preset, say — is not believed, and
    /// nothing is stored. It is a fault, not an outage: asking again gets the
    /// same hash.
    #[wasm_bindgen_test]
    async fn a_hash_below_the_written_cost_is_refused() {
        let weak = wafer_block_crypto::primitives::hash_password(
            "pw",
            wafer_block_crypto::primitives::Argon2Cost::Constrained,
        )
        .expect("a 4 MiB hash");
        let fake = FakeHasher::new(Rc::new(move |_| {
            let answer = protocol::Response {
                version: protocol::PROTOCOL_VERSION,
                outcome: Outcome::Hashed { hash: weak.clone() },
            };
            (200, serde_json::to_string(&answer).expect("encode"))
        }));
        misanswered(fake.service(1).hash("pw").await);
        assert_eq!(fake.shards.borrow().len(), 1, "the fake was asked");
    }

    /// A stub whose `fetch` rejects — the Durable Object threw, or could not
    /// be reached — is unavailable too.
    #[wasm_bindgen_test]
    async fn a_rejected_durable_object_call_is_unavailable() {
        struct Rejecting;
        #[async_trait::async_trait(?Send)]
        impl HasherTransport for Rejecting {
            async fn post(&self, _: &str, _: String) -> Result<(u16, Vec<u8>), String> {
                Err("Durable Object reset because its code was updated".into())
            }
        }
        let svc = ImpresspressCryptoService::new(
            String::new(),
            PasswordHasher::with_transport(Rejecting, 1),
        );
        unavailable(svc.hash("pw").await);
        unavailable(svc.compare_hash("pw", KAT_BY_CLASS[0]).await);
    }

    /// Requests are spread over exactly the configured number of instances,
    /// by stable names.
    #[wasm_bindgen_test]
    async fn requests_are_spread_across_the_configured_shards() {
        let fake = FakeHasher::new(Rc::new(|_| {
            let answer = protocol::Response {
                version: protocol::PROTOCOL_VERSION,
                outcome: Outcome::Mismatch,
            };
            (200, serde_json::to_string(&answer).expect("encode"))
        }));
        let svc = fake.service(3);
        // 3 instances, 120 draws: the chance any one is never drawn is
        // 3 * (2/3)^120, about 2e-21.
        for _ in 0..120 {
            let _ = svc.compare_hash("pw", "$argon2id$x").await;
        }
        let seen: std::collections::BTreeSet<String> =
            fake.shards.borrow().iter().cloned().collect();
        assert_eq!(
            seen,
            ["shard-0", "shard-1", "shard-2"]
                .into_iter()
                .map(String::from)
                .collect()
        );
    }

    /// A hasher one protocol version ahead still answers the version this
    /// Worker sends, in that version, and the answer is used.
    #[wasm_bindgen_test]
    async fn an_answer_in_the_version_asked_is_accepted() {
        let fake = FakeHasher::new(Rc::new(|body| {
            let request: serde_json::Value = serde_json::from_slice(body).expect("json");
            assert_eq!(request["version"], protocol::PROTOCOL_VERSION);
            let answer = protocol::Response {
                version: protocol::PROTOCOL_VERSION,
                outcome: Outcome::Verified,
            };
            (200, serde_json::to_string(&answer).expect("encode"))
        }));
        fake.service(1)
            .compare_hash("pw", "$argon2id$x")
            .await
            .expect("verified");
    }

    /// Password hashing does not depend on the JWT secret, and signing still
    /// refuses an empty one.
    #[wasm_bindgen_test]
    async fn hashing_works_without_a_usable_jwt_secret() {
        let fake = FakeHasher::running(PasswordPeppers::default());
        let svc =
            ImpresspressCryptoService::new(String::new(), PasswordHasher::from_env(&fake.env, 1));
        let hash = svc.hash("pw").await.expect("hash without a JWT secret");
        svc.compare_hash("pw", &hash)
            .await
            .expect("verify without a JWT secret");
        assert!(
            svc.sign_for(
                "impresspress/auth",
                std::collections::BTreeMap::new(),
                std::time::Duration::from_secs(60)
            )
            .await
            .is_err(),
            "signing, unlike hashing, must still refuse an empty secret"
        );
    }

    /// A constructor of a fake answer; see
    /// `a_failing_hasher_fails_the_operation_and_nothing_is_hashed_locally`.
    type AnswerFactory = fn() -> Answer;
}
