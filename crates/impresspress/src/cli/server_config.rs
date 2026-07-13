//! App-specific config loading (schema-aware, depends on impresspress-core).
//!
//! `filter_to_declared_keys` sits between the library's raw-env-var
//! collection and the SQLite-backed variable seeding, preserving the
//! prior behavior of only persisting env vars that match a declared
//! block/shared config var key.

use std::collections::HashMap;

pub fn filter_to_declared_keys(env_vars: HashMap<String, String>) -> Vec<(String, String)> {
    let block_infos = impresspress_core::blocks::all_block_infos();
    let all_vars = impresspress_core::config_vars::collect_all_config_vars(&block_infos);
    // Borrow the declared keys directly — the HashSet only lives for the
    // duration of the filter, so there's no need to allocate owned Strings.
    let known: std::collections::HashSet<&str> = all_vars.iter().map(|v| v.key.as_str()).collect();
    env_vars
        .into_iter()
        .filter(|(k, _)| known.contains(k.as_str()))
        .collect()
}

// Block-settings loading + the #222 hash-gate seed are now handled by the
// shared `impresspress_core::features::load_and_seed_block_settings` over the
// platform `DatabaseService` (see `server.rs::run`). The previous native-only
// `load_block_settings` carried a `IMPRESSPRESS_BLOCK_ENABLED` / `_DISABLED`
// env-var override that no other target honoured — removing it converges the
// three boot paths on the admin-UI-managed enablement model (the documented
// path). See the S3-R PR body for the drop rationale.

/// Load custom WRAP grants from the SQLite database.
///
/// Reads the `impresspress__admin__wrap_grants` table and returns a list of
/// `ResourceGrant` values that should be injected into the WAFER runtime.
pub fn load_wrap_grants(db_path: &str) -> Vec<wafer_run::ResourceGrant> {
    let Ok(conn) = rusqlite::Connection::open(db_path) else {
        return Vec::new();
    };

    let mut grants = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT grantee, resource, write, resource_type FROM impresspress__admin__wrap_grants",
    ) else {
        // Fall back to query without resource_type (column may not exist yet)
        let Ok(mut stmt) =
            conn.prepare("SELECT grantee, resource, write FROM impresspress__admin__wrap_grants")
        else {
            return Vec::new();
        };
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
            ))
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (grantee, resource, write) = row;
                grants.push(wafer_run::ResourceGrant {
                    grantee,
                    resource,
                    write: write != 0,
                    resource_type: None,
                });
            }
        }
        return grants;
    };

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i32>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    });

    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let (grantee, resource, write, rt) = row;
            let resource_type = match wafer_run::ResourceType::parse_stored(rt.as_deref()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(grantee = %grantee, resource = %resource, error = %e, "wrap_grants row dropped");
                    continue;
                }
            };
            grants.push(wafer_run::ResourceGrant {
                grantee,
                resource,
                write: write != 0,
                resource_type,
            });
        }
    }

    grants
}
