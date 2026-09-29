use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaConfig {
    pub max_storage_bytes: i64,
    pub max_file_size_bytes: i64,
    /// Most objects one user may hold in any one bucket, in-flight uploads included.
    pub max_files_per_bucket: i64,
}

impl QuotaConfig {
    /// Default per-user storage cap: 1 GiB.
    ///
    /// These consts are the single in-code source of the quota defaults —
    /// `Default::default()`, the per-field fallbacks in `quota.rs`, and the
    /// `CollectionSchema` defaults in `mod.rs` all derive from them. The
    /// migration SQL files carry the same values as DB-side column defaults
    /// (`migrations/001_initial_schema.*.sql`); a schema test in `mod.rs`
    /// guards against drift on the in-code side.
    pub const DEFAULT_MAX_STORAGE_BYTES: i64 = 1_073_741_824;
    /// Default single-file size cap: 100 MiB.
    ///
    /// This is the stored policy, not what an upload can reach: the transport
    /// refuses a request body over
    /// [`crate::streaming::MAX_REQUEST_BODY_BYTES`] before this block sees it,
    /// so every read of the quota clamps to that ceiling
    /// ([`super::quota::clamp_to_transport`]). Raise the ceiling and this
    /// default becomes reachable without changing here or the migration.
    pub const DEFAULT_MAX_FILE_SIZE_BYTES: i64 = 104_857_600;
    /// Default cap on the objects one user holds in one bucket: 10,000.
    ///
    /// Enforced per `(uploader, bucket)` by the upload's reservation
    /// ([`super::repo::objects::reserve_upload`]), counting `pending` rows as
    /// well as completed ones; the user's total across buckets is not capped.
    pub const DEFAULT_MAX_FILES_PER_BUCKET: i64 = 10_000;
}

impl QuotaConfig {
    /// The block defaults as they are actually enforced: [`Default::default`]
    /// with the per-file cap clamped to the transport's request-body ceiling
    /// ([`super::quota::clamp_to_transport`]).
    ///
    /// Every path that produces defaults rather than decoding a stored row
    /// goes through this — the quota lookup for a user with no override row,
    /// and the admin table's empty state — so a fourth one cannot quietly
    /// advertise the unreachable 100 MiB. A decoded row is clamped where it is
    /// decoded ([`super::repo::quota::QuotaRow::from_record`]).
    pub fn effective_default() -> Self {
        super::quota::clamp_to_transport(Self::default())
    }
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            max_storage_bytes: Self::DEFAULT_MAX_STORAGE_BYTES,
            max_file_size_bytes: Self::DEFAULT_MAX_FILE_SIZE_BYTES,
            max_files_per_bucket: Self::DEFAULT_MAX_FILES_PER_BUCKET,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_quota() {
        let quota = QuotaConfig::default();
        assert_eq!(quota.max_storage_bytes, 1_073_741_824); // 1GB
        assert_eq!(quota.max_file_size_bytes, 104_857_600); // 100MB
        assert_eq!(quota.max_files_per_bucket, 10_000);
    }

    #[test]
    fn test_quota_serialization() {
        let quota = QuotaConfig {
            max_storage_bytes: 500_000,
            max_file_size_bytes: 10_000,
            max_files_per_bucket: 100,
        };
        let json = serde_json::to_string(&quota).unwrap();
        let deserialized: QuotaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_storage_bytes, 500_000);
        assert_eq!(deserialized.max_file_size_bytes, 10_000);
        assert_eq!(deserialized.max_files_per_bucket, 100);
    }
}
