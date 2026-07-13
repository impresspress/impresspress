//! Crypto platform-service factory for native targets.

use std::sync::Arc;

use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

/// Construct a CryptoService seeded with `jwt_secret`. Argon2-backed on
/// native (see `wafer_block_crypto::service::Argon2JwtCryptoService`).
///
/// Returns an error if `jwt_secret` fails the underlying minimum-length
/// check (HMAC-SHA256 requires ≥ 32 bytes per RFC 2104). Fail-fast at
/// service construction so a weak secret can't quietly produce
/// forgeable tokens at runtime.
pub fn make_jwt_crypto_service(jwt_secret: String) -> Result<Arc<dyn CryptoService>, CryptoError> {
    Ok(Arc::new(
        wafer_block_crypto::service::Argon2JwtCryptoService::new(jwt_secret)?,
    ))
}
