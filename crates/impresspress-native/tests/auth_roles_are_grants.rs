//! Auth migration 016 makes every inline `users.role` a `user_roles` grant
//! and drops the column, on both SQL dialects, through the tracked runner a
//! native boot applies a block's list with
//! (`migration_helper::apply_pending_via_service`).
//!
//! The database is the one an upgrade meets: admin's schema, auth's 001–015,
//! and accounts whose role is the inline column — one of them already holding
//! the same role as a grant, one with a role no system seeds (`editor`) and
//! one with an empty role. After the upgrade every account holds exactly its
//! non-empty role as a grant, under its own name, and the column is gone;
//! the next change to the auth list re-runs the whole of it, 016 included,
//! and changes nothing.
//!
//! Why each statement of 016 is shaped as it is: the 016 note on
//! `blocks::auth::migrations::ROLES_ARE_GRANTS`.
//!
//! The SQLite test runs everywhere. The PostgreSQL one needs a server, so it
//! is `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database it may create others
//! from.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    blocks::{
        admin,
        auth::{
            self,
            migrations::{POSTGRES_MIGRATIONS, ROLES_ARE_GRANTS, SQLITE_MIGRATIONS},
            repo::users,
        },
    },
    migration_helper::{apply_ddl_via_service, apply_pending_via_service, MigrationFile},
    platform_state::user_roles,
};
use serde_json::{json, Value};
use wafer_core::interfaces::database::service::DatabaseService;

const CREATED: &str = "2026-03-01T00:00:00Z";

/// Auth's list in `dialect`, and the files of it before 016.
fn auth_lists(dialect: &str) -> (Vec<MigrationFile<'static>>, Vec<MigrationFile<'static>>) {
    let all: Vec<MigrationFile<'static>> = match dialect {
        "postgres" => POSTGRES_MIGRATIONS.to_vec(),
        _ => SQLITE_MIGRATIONS.to_vec(),
    };
    let at = all
        .iter()
        .position(|(name, _)| *name == ROLES_ARE_GRANTS)
        .unwrap_or_else(|| panic!("016 is wired into auth's {dialect} list"));
    assert_eq!(at, all.len() - 1, "016 is the last auth migration");
    (all[..at].to_vec(), all)
}

fn sql_of(files: &[MigrationFile<'static>]) -> Vec<&'static str> {
    files.iter().map(|(_, sql)| *sql).collect()
}

/// Test-fixture setup: a users row in the pre-016 shape, inline role and all.
async fn seed_account(db: &Arc<dyn DatabaseService>, id: &str, role: &str) {
    let mut row: HashMap<String, Value> = HashMap::new();
    row.insert("id".into(), json!(id));
    row.insert("email".into(), json!(format!("{id}@example.com")));
    row.insert("display_name".into(), json!(id));
    row.insert("role".into(), json!(role));
    row.insert("created_at".into(), json!(CREATED));
    row.insert("updated_at".into(), json!(CREATED));
    db.create(users::TABLE, row)
        .await
        .unwrap_or_else(|e| panic!("seed {id}: {e}"));
}

/// Every grant, as `(user_id, role) -> (id, assigned_by, assigned_at)`,
/// failing on a repeated pair.
async fn grants(
    db: &Arc<dyn DatabaseService>,
) -> HashMap<(String, String), (String, String, String)> {
    let opts = wafer_block::db::ListOptions {
        limit: Some(10_000),
        skip_count: true,
        ..Default::default()
    };
    let mut out = HashMap::new();
    for record in db
        .list(user_roles::TABLE, &opts)
        .await
        .expect("list grants")
        .records
    {
        let text = |column: &str| {
            record
                .data
                .get(column)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let pair = (text("user_id"), text("role"));
        let seen = out.insert(
            pair.clone(),
            (record.id.clone(), text("assigned_by"), text("assigned_at")),
        );
        assert!(seen.is_none(), "a repeated grant {pair:?}");
    }
    out
}

/// The upgrade, then a re-run of the whole list.
async fn upgrades(db: Arc<dyn DatabaseService>, dialect: &str) {
    let (before_016, all) = auth_lists(dialect);
    apply_ddl_via_service(&db, &admin::migrations::ddl_files(dialect))
        .await
        .expect("admin's list applies first, as every boot applies it");
    apply_ddl_via_service(&db, &sql_of(&before_016))
        .await
        .expect("auth's 001-015 apply");

    seed_account(&db, "u1", "admin").await;
    seed_account(&db, "u2", "user").await;
    seed_account(&db, "u3", "admin").await;
    // A role no system seeds is copied as the name it is; an empty inline
    // role names nothing and becomes no grant.
    seed_account(&db, "u4", "editor").await;
    seed_account(&db, "u5", "").await;
    // u3 already holds its inline role as a grant, made by an admin.
    let mut held: HashMap<String, Value> = HashMap::new();
    held.insert("id".into(), json!("ur_held"));
    held.insert("user_id".into(), json!("u3"));
    held.insert("role".into(), json!("admin"));
    held.insert("assigned_by".into(), json!("admin_x"));
    held.insert("created_at".into(), json!(CREATED));
    held.insert("updated_at".into(), json!(CREATED));
    db.create(user_roles::TABLE, held)
        .await
        .expect("seed u3's grant");

    apply_pending_via_service(&db, auth::AUTH_BLOCK_ID, &all)
        .await
        .expect("auth's list, 016 included, applies to an upgraded database");

    let after = grants(&db).await;
    let pairs = |grants: &HashMap<(String, String), (String, String, String)>| {
        let mut pairs: Vec<(String, String)> = grants.keys().cloned().collect();
        pairs.sort();
        pairs
    };
    let want: Vec<(String, String)> = [
        ("u1", "admin"),
        ("u2", "user"),
        ("u3", "admin"),
        ("u4", "editor"),
    ]
    .iter()
    .map(|(u, r)| ((*u).to_string(), (*r).to_string()))
    .collect();
    assert_eq!(
        pairs(&after),
        want,
        "every non-empty inline role is a grant, once, under its own name; u5's empty role is none"
    );
    for user in ["u1", "u2", "u4"] {
        let (id, by, at) = after
            .iter()
            .find(|((u, _), _)| u == user)
            .map(|(_, v)| v.clone())
            .expect("the copied grant");
        assert!(
            id.starts_with("ur_") && id.len() == 35,
            "{user}'s grant id: {id}"
        );
        assert_eq!(by, "", "no admin is behind {user}'s copied grant");
        assert_eq!(at, CREATED, "{user}'s grant dates from the account");
    }
    assert_eq!(
        after[&("u3".to_string(), "admin".to_string())],
        ("ur_held".to_string(), "admin_x".to_string(), String::new()),
        "the grant u3 already held is the one it keeps"
    );
    let columns = db
        .schema_columns(users::TABLE)
        .await
        .expect("introspect users");
    assert!(
        !columns.iter().any(|c| c == "role"),
        "016 drops the inline column: {columns:?}"
    );
    assert!(columns.iter().any(|c| c == "email"), "{columns:?}");

    // The next change to the auth list re-runs it whole, 016 included: the
    // ADD puts the column back empty, the copy finds nothing, the DROP
    // removes it again.
    let mut next = all.clone();
    next.push((
        "999_next_auth_change",
        "-- Any new file moves the hash, so the whole list re-runs.",
    ));
    apply_pending_via_service(&db, auth::AUTH_BLOCK_ID, &next)
        .await
        .expect("a later change re-runs the auth list, 016 included");
    assert_eq!(grants(&db).await, after, "the re-run copies nothing");
    let columns = db
        .schema_columns(users::TABLE)
        .await
        .expect("introspect users");
    assert!(!columns.iter().any(|c| c == "role"), "{columns:?}");
}

/// A database created after 016: admin's list, then auth's whole list, then
/// a re-run of it. 001 creates the column and 016 drops it again.
async fn applies_fresh(db: Arc<dyn DatabaseService>, dialect: &str) {
    let (_, all) = auth_lists(dialect);
    apply_ddl_via_service(&db, &admin::migrations::ddl_files(dialect))
        .await
        .expect("admin's list applies first");
    apply_pending_via_service(&db, auth::AUTH_BLOCK_ID, &all)
        .await
        .expect("auth's list applies to a fresh database");
    apply_ddl_via_service(&db, &sql_of(&all))
        .await
        .expect("auth's list replays over a fresh database");
    assert!(grants(&db).await.is_empty());
    let columns = db
        .schema_columns(users::TABLE)
        .await
        .expect("introspect users");
    assert!(!columns.iter().any(|c| c == "role"), "{columns:?}");
}

async fn sqlite(dir: &tempfile::TempDir, name: &str) -> Arc<dyn DatabaseService> {
    let path = dir.path().join(name);
    impresspress_native::make_database_service("sqlite", path.to_str().expect("utf-8 path"), None)
        .await
        .expect("open SQLite")
}

#[tokio::test]
async fn migration_016_makes_every_inline_role_a_grant_on_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    upgrades(sqlite(&dir, "upgraded.sqlite3").await, "sqlite").await;
}

#[tokio::test]
async fn migration_016_applies_to_a_fresh_sqlite_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    applies_fresh(sqlite(&dir, "fresh.sqlite3").await, "sqlite").await;
}

/// Both scenarios against PostgreSQL, each on a database of its own.
#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn the_same_holds_on_postgres() {
    let url = std::env::var("IMPRESSPRESS_TEST_POSTGRES_URL")
        .expect("IMPRESSPRESS_TEST_POSTGRES_URL must name a PostgreSQL database");
    let server = impresspress_native::make_postgres_database_service(&url)
        .await
        .expect("connect to PostgreSQL");
    for scenario in ["upgraded", "fresh"] {
        let name = format!("impresspress_auth_016_{scenario}");
        // Test-fixture setup: a database per scenario, created fresh.
        server
            .exec_raw(&format!("DROP DATABASE IF EXISTS {name}"), &[])
            .await
            .expect("drop the scenario's database");
        server
            .exec_raw(&format!("CREATE DATABASE {name}"), &[])
            .await
            .expect("create the scenario's database");
        let (base, _) = url.rsplit_once('/').expect("a database URL");
        let db = impresspress_native::make_postgres_database_service(&format!("{base}/{name}"))
            .await
            .expect("connect to the scenario's database");
        match scenario {
            "upgraded" => upgrades(db, "postgres").await,
            _ => applies_fresh(db, "postgres").await,
        }
    }
}
