//! Browser `CryptoService`: PBKDF2 password hashing, HS256 JWTs.
//!
//! Both halves are the shared `wafer-block-crypto` implementations; no
//! cryptography is written here.
//!
//! **Passwords.** The native runtime hashes with argon2id. On wasm32
//! `wafer-block-crypto` runs every argon2 derivation in a working buffer it
//! allocates once and keeps (19 MiB at the default cost, 4 MiB at
//! `Argon2Cost::Constrained`), because linear memory never shrinks; in a
//! Service Worker that memory is shared with sql.js for the worker's life.
//! This target selects the other scheme `wafer-block-crypto` supports, which
//! holds no working memory:
//! [`PasswordScheme::Pbkdf2Sha256`] at
//! [`PBKDF2_SHA256_RECOMMENDED_ITERATIONS`] (OWASP's 2023 recommendation; NIST
//! SP 800-132's floor is 10,000). The trade is CPU: about 180 ms per hash in
//! wasm32, where argon2id takes about 17-35 ms at the default cost.
//! Verification does NOT consult that choice —
//! [`primitives::verify_password_any_scheme`] dispatches on the scheme the
//! stored hash itself names, so a credential written by any target verifies
//! here and selecting a scheme is not a password reset. That is what lets a
//! dev-sandbox export, or one workspace opened under two runtimes, keep
//! working.
//!
//! One consequence, because it is a real cost and not only a capability: this
//! target will now RUN argon2id if a stored hash names it, at whatever cost
//! that hash declares — the smallest of `wafer-block-crypto`'s kept buffers
//! that holds it (19 MiB for the default `m=19456`), permanently, since wasm
//! memory never shrinks. That is the right
//! answer for verifying a credential someone actually owns, and the wrong
//! answer for anything synthetic. It is why the login timing-equalization hash
//! (`impresspress_core::blocks::auth::timing_equalization_hash`) is derived
//! from `crypto::hash` rather than hardcoded: a constant argon2id string would
//! have made every mistyped email pay that price.
//!
//! **No pepper.** The native and Cloudflare targets can pepper password hashes
//! with a key held outside the database (`IMPRESSPRESS_PASSWORD_PEPPER_KEY`).
//! This target runs in a visitor's browser, where no value can be kept from
//! the visitor, so a pepper would protect nothing and it holds none: it
//! writes unpeppered PBKDF2 (which `wafer-block-crypto` never peppers anyway)
//! and verifies with an empty [`PasswordPeppers`]. A peppered credential
//! carried in from a server deployment (a dev-sandbox import of its data)
//! therefore fails here with `CryptoError::Pepper` — the key it names is
//! missing — never as a wrong password.
//!
//! **JWTs.** Signing, verification and the per-block HKDF key derivation for
//! `sign_for`/`verify_for` delegate to [`Argon2JwtCryptoService`], the same
//! HS256 engine the native runtime and the Cloudflare Worker use, so tokens
//! are interchangeable across targets.
//!
//! ## Why the password half does not go through `Argon2JwtCryptoService`
//!
//! [`Argon2JwtCryptoService::with_password_scheme`] would carry the choice
//! above on the engine itself, which is what spec 2.7 asked for. It is not
//! used, because [`Argon2JwtCryptoService::new`] refuses a JWT secret shorter
//! than 32 bytes and this service is constructed with an **empty** one:
//! `impresspress-web` cannot read `WAFER_RUN__AUTH__JWT_SECRET` until admin's
//! `Init` has created the variables table, so it builds the service first and
//! rotates the secret in `seed_after_admin_init` (see
//! [`BrowserCryptoService::set_jwt_secret`]).
//! Routing `hash` through the engine would make password hashing fail whenever
//! that secret is absent or short — and the auth block's own `Init` hashes the
//! bootstrap admin's password, so a missing secret would stop meaning "nobody
//! can sign in" and start meaning "the first-run admin is never created".
//! `hash`/`compare_hash` below call the same `primitives` entry points the
//! engine calls, with the same scheme, so nothing about the hashes differs;
//! only the coupling does. Pinned by
//! `password_parity::hashing_works_before_the_jwt_secret_is_installed`.

use std::{collections::BTreeMap, time::Duration};

use wafer_block_crypto::{
    primitives::{self, PasswordPeppers, PasswordScheme, PBKDF2_SHA256_RECOMMENDED_ITERATIONS},
    service::Argon2JwtCryptoService,
};
use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

/// What this target writes when it hashes a new password. See the module doc
/// for why it is not argon2id, and why verification ignores it.
const PASSWORD_SCHEME: PasswordScheme = PasswordScheme::Pbkdf2Sha256 {
    iterations: PBKDF2_SHA256_RECOMMENDED_ITERATIONS,
};

pub struct BrowserCryptoService {
    // Stored behind a lock so the secret can be rotated post-construction.
    // `impresspress-web` builds the crypto service before it has loaded the
    // persisted `WAFER_RUN__AUTH__JWT_SECRET` value (the variables table
    // doesn't exist until admin's Init has run), so it constructs with an
    // empty secret and calls [`Self::set_jwt_secret`] in the post-admin-Init
    // phase. Native consumers that have the secret up-front can pass it to
    // [`Self::new`] and never touch the setter.
    jwt_secret: std::sync::RwLock<String>,
}

// SAFETY: `BrowserCryptoService` only holds owned data behind a `RwLock`.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn CryptoService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserCryptoService {}
unsafe impl Sync for BrowserCryptoService {}

impl BrowserCryptoService {
    pub fn new(jwt_secret: String) -> Self {
        Self {
            jwt_secret: std::sync::RwLock::new(jwt_secret),
        }
    }

    /// Rotate the JWT secret. Used by `impresspress-web`'s init flow to install
    /// the persisted/auto-generated secret AFTER admin's Init has created
    /// the variables table; see the doc on [`Self::jwt_secret`].
    pub fn set_jwt_secret(&self, new_secret: String) {
        *self
            .jwt_secret
            .write()
            .expect("BrowserCryptoService jwt_secret RwLock poisoned") = new_secret;
    }

    fn jwt_secret(&self) -> String {
        self.jwt_secret
            .read()
            .expect("BrowserCryptoService jwt_secret RwLock poisoned")
            .clone()
    }

    /// Build the shared JWT engine from the current secret. Constructed per
    /// call because the secret is rotatable (see [`Self::set_jwt_secret`]);
    /// fails while the secret is still empty/short — a deployment that
    /// signs tokens before installing a real secret must error, not mint
    /// weakly-signed credentials.
    fn jwt(&self) -> Result<Argon2JwtCryptoService, CryptoError> {
        Argon2JwtCryptoService::new(self.jwt_secret())
    }
}

#[wafer_block::wafer_async_trait]
impl CryptoService for BrowserCryptoService {
    /// Write a new credential under [`PASSWORD_SCHEME`]. The hand-rolled
    /// PBKDF2 body this replaced produced a byte-identical string (same
    /// `$pbkdf2-sha256$i=N$salt$dk` layout, same 16-byte salt, same 32-byte
    /// derived key, same `base64ct` standard alphabet), so no stored
    /// credential moves — see `password_parity`.
    async fn hash(&self, password: &str) -> Result<String, CryptoError> {
        primitives::hash_password_with(password, PASSWORD_SCHEME, &PasswordPeppers::default())
    }

    /// Verify against whichever scheme the **stored hash** names, not the one
    /// this target writes.
    ///
    /// Two defects of the hand-rolled verifier this replaced are fixed by
    /// using the shared one, and both are pinned by `password_parity`:
    ///
    /// - it derived `stored.len()` bytes rather than a fixed 32, and PBKDF2 at
    ///   a shorter `dkLen` returns a PREFIX of the longer output — so a stored
    ///   hash truncated to eight bytes verified at 64 bits;
    /// - it recognised `pbkdf2-sha256` and nothing else, so an argon2id
    ///   credential written by the native or Cloudflare runtime against the
    ///   same workspace could never be verified here.
    async fn compare_hash(&self, password: &str, hash_str: &str) -> Result<(), CryptoError> {
        primitives::verify_password_any_scheme(password, hash_str, &PasswordPeppers::default())
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

/// Factory: returns an `Arc<dyn CryptoService>` seeded with `jwt_secret`.
/// The secret is used for HMAC-based JWT signing. It is the caller's
/// responsibility to source this secret (typically from an
/// `WAFER_RUN__AUTH__JWT_SECRET` config var).
pub fn make_crypto_service(
    jwt_secret: String,
) -> std::sync::Arc<dyn wafer_core::interfaces::crypto::service::CryptoService> {
    std::sync::Arc::new(BrowserCryptoService::new(jwt_secret))
}

// ─── Password-hash parity ────────────────────────────────────────────────────
//
// These run under `wasm-pack test --node`; they touch no bridge.
#[cfg(all(test, target_arch = "wasm32"))]
mod password_parity {
    use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::BrowserCryptoService;

    /// 32 bytes, so `Argon2JwtCryptoService::new` accepts it.
    const SECRET: &str = "test-secret-padded-to-32-bytes-or-more";

    fn svc() -> BrowserCryptoService {
        BrowserCryptoService::new(SECRET.to_string())
    }

    /// Written by the hand-rolled PBKDF2 body this module used to carry, at
    /// the production iteration count: password `correct horse battery
    /// staple`, salt `00..0f`, `i=600000`, 32-byte derived key, `base64ct`
    /// standard alphabet. A stored credential from a browser workspace that
    /// predates this change looks exactly like this, so it is the fixture
    /// that says whether the change invalidates one.
    const LEGACY_HASH_600K: &str =
        "$pbkdf2-sha256$i=600000$AAECAwQFBgcICQoLDA0ODw==$7xdxRO7JQgy8EJPSqLNEqSvFBtDU7JwCjdGfgyTYweY=";
    /// The same credential at the NIST floor, to prove the iteration count is
    /// read from the stored string rather than from the service's setting.
    const LEGACY_HASH_10K: &str =
        "$pbkdf2-sha256$i=10000$AAECAwQFBgcICQoLDA0ODw==$2flfZcLfnShdJogjAMpb4p4+1QBVZmODXExi4nBRUCI=";
    const LEGACY_PASSWORD: &str = "correct horse battery staple";

    /// Fixture parity, direction 1: a credential hashed by the OLD browser
    /// code still verifies.
    #[wasm_bindgen_test]
    async fn a_legacy_browser_hash_still_verifies() {
        svc()
            .compare_hash(LEGACY_PASSWORD, LEGACY_HASH_600K)
            .await
            .expect("a credential stored by the pre-change browser must still verify");
        svc()
            .compare_hash(LEGACY_PASSWORD, LEGACY_HASH_10K)
            .await
            .expect("the iteration count comes from the stored string, not from the service");
    }

    /// …and still rejects the wrong password, as a mismatch rather than as a
    /// malformed-hash error.
    #[wasm_bindgen_test]
    async fn a_legacy_browser_hash_rejects_the_wrong_password() {
        match svc()
            .compare_hash("not the password", LEGACY_HASH_10K)
            .await
        {
            Err(CryptoError::PasswordMismatch) => {}
            other => panic!("expected PasswordMismatch, got {other:?}"),
        }
    }

    /// Fixture parity, direction 2: a credential hashed by the NEW path
    /// verifies, and it is still a `pbkdf2-sha256` string — the browser must
    /// not start writing argon2id, which takes minutes in single-threaded
    /// wasm.
    #[wasm_bindgen_test]
    async fn a_new_hash_is_pbkdf2_and_verifies() {
        let svc = svc();
        let hash = svc.hash(LEGACY_PASSWORD).await.expect("hash");
        assert!(
            hash.starts_with("$pbkdf2-sha256$i=600000$"),
            "the browser must keep writing PBKDF2 at 600k iterations: {hash}"
        );
        svc.compare_hash(LEGACY_PASSWORD, &hash)
            .await
            .expect("a freshly written credential must verify");
    }

    /// **Fails on the pre-change tree.** The hand-rolled verifier derived
    /// `expected.len()` bytes from the stored string, and PBKDF2 at a shorter
    /// `dkLen` returns a PREFIX of the longer output — so a hash truncated to
    /// eight bytes verified at 64 bits. Upstream fixed this by deriving a
    /// fixed 32 bytes and refusing anything else as malformed.
    #[wasm_bindgen_test]
    async fn a_truncated_hash_is_refused_instead_of_verifying_at_64_bits() {
        let truncated = "$pbkdf2-sha256$i=10000$AAECAwQFBgcICQoLDA0ODw==$2flfZcLfnSg=";
        match svc().compare_hash(LEGACY_PASSWORD, truncated).await {
            Err(CryptoError::MalformedHash(msg)) => assert!(
                msg.contains("32 bytes"),
                "the refusal must name the required derived-key length: {msg}"
            ),
            other => panic!("a truncated hash must not verify: {other:?}"),
        }
    }

    /// **Fails on the pre-change tree**, which recognised `pbkdf2-sha256` and
    /// nothing else. One database can be carried between targets — a dev
    /// sandbox export, a workspace opened on both — and argon2id is what the
    /// native and Cloudflare runtimes write. Refusing to verify it locked
    /// such a user out and reported it as a wrong password.
    #[wasm_bindgen_test]
    async fn an_argon2_hash_written_by_another_target_verifies_here() {
        let argon2 = wafer_block_crypto::primitives::hash_password(
            LEGACY_PASSWORD,
            wafer_block_crypto::primitives::Argon2Cost::Constrained,
        )
        .expect("argon2 hash");
        svc()
            .compare_hash(LEGACY_PASSWORD, &argon2)
            .await
            .expect("an argon2id credential from another target must verify");
    }

    /// An unrecognised scheme is a distinct error, never a password
    /// mismatch: reporting it as a mismatch tells the logs that a user who
    /// cannot possibly sign in keeps mistyping.
    #[wasm_bindgen_test]
    async fn an_unknown_scheme_is_a_malformed_hash_not_a_mismatch() {
        match svc()
            .compare_hash(LEGACY_PASSWORD, "$scrypt$ln=16,r=8,p=1$c2FsdA$aGFzaA")
            .await
        {
            Err(CryptoError::MalformedHash(msg)) => assert!(
                msg.contains("unrecognised password hash scheme"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected a MalformedHash for an unknown scheme, got {other:?}"),
        }
    }

    /// Password hashing must not depend on the JWT secret: the browser
    /// constructs this service with an EMPTY secret and rotates it in
    /// `seed_after_admin_init`, while the auth block's own `Init` hashes the
    /// bootstrap admin's password. Coupling the two would make first-run
    /// admin creation fail whenever the secret is absent or short.
    #[wasm_bindgen_test]
    async fn hashing_works_before_the_jwt_secret_is_installed() {
        let svc = BrowserCryptoService::new(String::new());
        let hash = svc
            .hash(LEGACY_PASSWORD)
            .await
            .expect("hash without a JWT secret");
        svc.compare_hash(LEGACY_PASSWORD, &hash)
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
}
