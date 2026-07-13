//! CryptoService for Cloudflare Workers, on top of wafer-block-crypto.
//!
//! JWT policy — HS256, `exp` required on verify, per-block HKDF-derived
//! keys for `sign_for`/`verify_for`, minimum secret length — delegates to
//! [`Argon2JwtCryptoService`], the same engine the native runtime uses, so
//! tokens are interchangeable across deployment targets. (Historically this
//! service silently dropped per-block key derivation by inheriting the
//! trait's master-key fallbacks, which is exactly the kind of drift behind
//! the PR #155 → #170 production auth regression.)
//!
//! Password hashing is the one deliberate platform divergence: argon2id at
//! [`Argon2Cost::Constrained`] (4 MiB / 2 iters), because Workers'
//! CPU/memory limits rule out the default cost. `verify_password` reads the
//! cost from the PHC hash string, so hashes verify across targets either way.

use std::{collections::HashMap, time::Duration};

use wafer_block_crypto::{
    primitives::{self, Argon2Cost},
    service::Argon2JwtCryptoService,
};
use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

/// CryptoService backing the CF Worker runtime. See module docs for policy.
pub struct ImpresspressCryptoService {
    jwt_secret: String,
}

impl ImpresspressCryptoService {
    pub fn new(jwt_secret: String) -> Self {
        Self { jwt_secret }
    }

    /// Build the shared JWT engine. Fails when the secret is missing or
    /// shorter than `MIN_JWT_SECRET_LEN` — surfaced per operation rather
    /// than at worker boot, because the worker constructs this service
    /// before config is necessarily complete and a broken-auth deployment
    /// beats a boot-looping one.
    fn jwt(&self) -> Result<Argon2JwtCryptoService, CryptoError> {
        Argon2JwtCryptoService::new(self.jwt_secret.clone())
    }
}

impl CryptoService for ImpresspressCryptoService {
    fn hash(&self, password: &str) -> Result<String, CryptoError> {
        primitives::hash_password(password, Argon2Cost::Constrained)
    }

    fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError> {
        primitives::verify_password(password, hash)
    }

    fn sign(
        &self,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        self.jwt()?.sign(claims, expiry)
    }

    fn verify(&self, token: &str) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        self.jwt()?.verify(token)
    }

    fn sign_for(
        &self,
        block_id: &str,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError> {
        self.jwt()?.sign_for(block_id, claims, expiry)
    }

    fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
        self.jwt()?.verify_for(block_id, token)
    }

    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError> {
        primitives::random_bytes(n)
    }
}
