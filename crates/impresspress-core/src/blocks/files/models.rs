use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaConfig {
    pub max_storage_bytes: i64,
    pub max_file_size_bytes: i64,
    pub max_files_per_bucket: i64,
    pub reset_period_days: i64,
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
    pub const DEFAULT_MAX_FILE_SIZE_BYTES: i64 = 104_857_600;
    /// Default per-bucket file-count cap.
    pub const DEFAULT_MAX_FILES_PER_BUCKET: i64 = 10_000;
    /// Default reset period (0 = never).
    pub const DEFAULT_RESET_PERIOD_DAYS: i64 = 0;
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            max_storage_bytes: Self::DEFAULT_MAX_STORAGE_BYTES,
            max_file_size_bytes: Self::DEFAULT_MAX_FILE_SIZE_BYTES,
            max_files_per_bucket: Self::DEFAULT_MAX_FILES_PER_BUCKET,
            reset_period_days: Self::DEFAULT_RESET_PERIOD_DAYS,
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
        assert_eq!(quota.reset_period_days, 0);
    }

    #[test]
    fn test_quota_serialization() {
        let quota = QuotaConfig {
            max_storage_bytes: 500_000,
            max_file_size_bytes: 10_000,
            max_files_per_bucket: 100,
            reset_period_days: 30,
        };
        let json = serde_json::to_string(&quota).unwrap();
        let deserialized: QuotaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_storage_bytes, 500_000);
        assert_eq!(deserialized.max_file_size_bytes, 10_000);
        assert_eq!(deserialized.max_files_per_bucket, 100);
        assert_eq!(deserialized.reset_period_days, 30);
    }
}
