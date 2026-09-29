//! The password-hasher Worker: one stateless Durable Object class, and a
//! `fetch` entry point that answers nothing.
//!
//! The class is SQLite-backed (declared under `new_sqlite_classes`, which the
//! Workers Free plan requires) and stores nothing. It exists for the Durable
//! Object CPU allowance: see [`crate::protocol`].

use worker::{
    durable_object, event, Context, DurableObject, Env, Method, Request, Response, Result, State,
};

use crate::{
    hasher,
    pepper::{
        password_peppers, PasswordPeppers, PASSWORD_PEPPER_KEY_VAR,
        PASSWORD_PEPPER_PREVIOUS_KEYS_VAR, PASSWORD_PEPPER_REQUIRED_VAR,
    },
    protocol::DURABLE_OBJECT_CLASS,
};

/// Hashes and verifies passwords for the main Worker. See [`crate::protocol`].
#[durable_object]
pub struct ImpresspressPasswordHasher {
    /// Read once per instance from this Worker's own secrets and vars. A
    /// secret change deploys a new version, which restarts the instance.
    peppers: std::result::Result<PasswordPeppers, String>,
}

// The exported class name is the struct's name; the deploy tooling writes
// `DURABLE_OBJECT_CLASS` into both Workers' configs.
const _: () = {
    let (a, b) = (
        DURABLE_OBJECT_CLASS.as_bytes(),
        stringify!(ImpresspressPasswordHasher).as_bytes(),
    );
    assert!(a.len() == b.len());
    let mut i = 0;
    while i < a.len() {
        assert!(a[i] == b[i]);
        i += 1;
    }
};

impl DurableObject for ImpresspressPasswordHasher {
    fn new(_state: State, env: Env) -> Self {
        let secret = |name| env.secret(name).ok().map(|value| value.to_string());
        let var = env
            .var(PASSWORD_PEPPER_REQUIRED_VAR)
            .ok()
            .map(|value| value.to_string());
        Self {
            peppers: password_peppers(
                secret(PASSWORD_PEPPER_KEY_VAR).as_deref(),
                secret(PASSWORD_PEPPER_PREVIOUS_KEYS_VAR).as_deref(),
                var.as_deref(),
            ),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        if req.method() != Method::Post {
            return Response::error("method not allowed", 405);
        }
        let body = req.bytes().await?;
        let answer = hasher::answer(&body, self.peppers.as_ref().map_err(String::as_str));
        Response::from_json(&answer)
    }
}

/// The Worker's own `fetch`: the hasher is reached through its Durable Object
/// binding only, so a request to the Worker itself is not found.
#[event(fetch)]
async fn fetch(_req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    Response::error("not found", 404)
}

// Run by the `cloudflare-wasm-test` CI job. The class runs here as the
// runtime runs it — constructed from an `Env`, answering a `Request` — against
// a fake `Env` of plain string bindings, which is what secrets and vars are.
#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_test::wasm_bindgen_test;
    use worker::{DurableObject, Method, Request, RequestInit};

    use super::ImpresspressPasswordHasher;
    use crate::{
        pepper::{PASSWORD_PEPPER_KEY_VAR, PASSWORD_PEPPER_REQUIRED_VAR},
        protocol::{self, Operation, Outcome},
    };

    /// 32 bytes of `0x2a`, base64.
    const KEY: &str = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";

    fn hasher(bindings: &[(&str, &str)]) -> ImpresspressPasswordHasher {
        let env = js_sys::Object::new();
        for (name, value) in bindings {
            js_sys::Reflect::set(&env, &JsValue::from_str(name), &JsValue::from_str(value))
                .expect("set a binding");
        }
        let state =
            js_sys::Object::new().unchecked_into::<worker::worker_sys::DurableObjectState>();
        // Through the trait: `#[durable_object]` also gives the struct the
        // inherent `new`/`fetch` the JavaScript runtime calls.
        <ImpresspressPasswordHasher as DurableObject>::new(
            worker::State::from(state),
            JsValue::from(env).unchecked_into::<worker::Env>(),
        )
    }

    async fn ask(hasher: &ImpresspressPasswordHasher, body: &str) -> (u16, protocol::Response) {
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(JsValue::from_str(body)));
        let request = Request::new_with_init("https://password-hasher/", &init).expect("request");
        let mut response = DurableObject::fetch(hasher, request).await.expect("fetch");
        let bytes = response.bytes().await.expect("body");
        (
            response.status_code(),
            protocol::Response::from_body(&bytes).expect("a protocol answer"),
        )
    }

    fn hash_body(password: &str) -> String {
        protocol::Request::new(Operation::Hash {
            password: password.into(),
        })
        .to_body()
    }

    /// The pepper key comes from the hasher Worker's own secret: a hash it
    /// writes names that key, at OWASP's cost, and it verifies there.
    #[wasm_bindgen_test]
    async fn hashes_with_the_pepper_from_its_own_env() {
        let hasher = hasher(&[(PASSWORD_PEPPER_KEY_VAR, KEY)]);
        let (status, answer) = ask(&hasher, &hash_body("correct horse")).await;
        assert_eq!(status, 200);
        let hash = answer.into_hash().expect("hash");
        assert!(
            hash.starts_with("$argon2id-hmac-sha256$v=19$m=19456,t=2,p=1,pepper="),
            "{hash}"
        );
        let verify = protocol::Request::new(Operation::Verify {
            password: "correct horse".into(),
            hash,
        })
        .to_body();
        let (_, answer) = ask(&hasher, &verify).await;
        answer.into_verify().expect("verifies");
    }

    /// A pepper setting that does not parse is a pepper fault on every
    /// request, never a hash written without it.
    #[wasm_bindgen_test]
    async fn a_malformed_pepper_setting_is_a_pepper_fault() {
        let hasher = hasher(&[
            (PASSWORD_PEPPER_KEY_VAR, KEY),
            (PASSWORD_PEPPER_REQUIRED_VAR, "yes"),
        ]);
        let (status, answer) = ask(&hasher, &hash_body("pw")).await;
        assert_eq!(status, 200);
        match answer.outcome {
            Outcome::Pepper { message } => {
                assert!(message.contains(PASSWORD_PEPPER_REQUIRED_VAR), "{message}")
            }
            other => panic!("expected a pepper fault, got {other:?}"),
        }
    }

    /// Only `POST` carries a request.
    #[wasm_bindgen_test]
    async fn refuses_other_methods() {
        let hasher = hasher(&[]);
        let request = Request::new("https://password-hasher/", Method::Get).expect("request");
        let response = DurableObject::fetch(&hasher, request).await.expect("fetch");
        assert_eq!(response.status_code(), 405);
    }
}
