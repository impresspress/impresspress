/// Returns the first 8 hex chars of the SHA-256 of `bytes`.
pub fn short_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex::encode(&digest[..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_hash_is_eight_chars() {
        let h = short_hash(b"hello");
        assert_eq!(h.len(), 8);
    }

    #[test]
    fn short_hash_is_deterministic() {
        assert_eq!(short_hash(b"abc"), short_hash(b"abc"));
    }

    #[test]
    fn short_hash_differs_per_input() {
        assert_ne!(short_hash(b"abc"), short_hash(b"abd"));
    }

    #[test]
    fn short_hash_matches_known_value() {
        // SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(short_hash(b"hello"), "2cf24dba");
    }
}
