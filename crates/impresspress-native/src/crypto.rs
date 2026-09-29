//! Crypto platform-service factory for native targets.

use std::sync::Arc;

use wafer_block_crypto::{primitives::PasswordPeppers, service::Argon2JwtCryptoService};
use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};

/// Construct a CryptoService seeded with `jwt_secret`, peppering passwords
/// with `peppers`. Argon2-backed on native (see
/// `wafer_block_crypto::service::Argon2JwtCryptoService`).
///
/// Returns an error if `jwt_secret` fails the underlying minimum-length
/// check (HMAC-SHA256 requires ≥ 32 bytes per RFC 2104). Fail-fast at
/// service construction so a weak secret can't quietly produce
/// forgeable tokens at runtime.
///
/// `peppers` come from outside the database the hashes live in (the
/// process environment; see `impresspress_password::pepper`).
/// `PasswordPeppers::default()` is no pepper: new hashes are written
/// unpeppered and a stored peppered hash fails as a missing key.
pub fn make_jwt_crypto_service(
    jwt_secret: String,
    peppers: PasswordPeppers,
) -> Result<Arc<dyn CryptoService>, CryptoError> {
    Ok(Arc::new(
        Argon2JwtCryptoService::new(jwt_secret)?.with_password_peppers(peppers)?,
    ))
}
