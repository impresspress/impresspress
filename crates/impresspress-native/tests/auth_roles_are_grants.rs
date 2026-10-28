//! Auth migration 016 makes every inline `users.role` a `user_roles` grant
//! and drops the column, on both SQL dialects, through the runner a native
//! boot applies a block's list with
//! (`migration_helper::apply_pending_via_service`, with auth's history).
//!
//! The database is the one an upgrade meets: admin's schema, and auth's list
//! as it stood on main from 2026-09-23 (001–015; `cc40a45f` on SQLite,
//! `4390791b` on PostgreSQL), built and
//! recorded by the runner from before the per-file ledger, holding accounts
//! whose role is the inline column — one of them already holding the same
//! role as a grant, one with a role no system seeds (`editor`) and one with
//! an empty role. The upgrade adopts 001–015 from that record and runs 016
//! and the files auth gained after it. After it every account holds exactly
//! its non-empty role as a grant, under its own name, and the column is
//! gone; the next change to the auth list runs only its new file.
//!
//! A database that ran 016 as it first shipped (`e73b4cc2` / `43907ff1`)
//! adopts all sixteen files through 016 and runs only the later ones; a
//! fresh one runs the whole list.
//!
//! Why each statement of 016 is shaped as it is: the 016 note on
//! `blocks::auth::migrations::ROLES_ARE_GRANTS`.
//!
//! The SQLite tests run everywhere. The PostgreSQL one needs a server, so it
//! is `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database it may create others
//! from.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    blocks::auth::{
        self,
        migrations::{MIGRATIONS, ROLES_ARE_GRANTS},
        repo::users,
    },
    migration_helper::{apply_pending_via_service, MigrationFile},
    platform_state::{applied_migrations::Origin, user_roles},
    test_support::migration_history as history,
};
use serde_json::{json, Value};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

/// The newest state of auth's list before 016 and the one 016 first shipped
/// in, in `backend`'s dialect.
fn states(backend: Backend) -> (&'static str, &'static str) {
    match backend {
        Backend::Sqlite => ("cc40a45f", "e73b4cc2"),
        Backend::Postgres => ("4390791b", "43907ff1"),
    }
}

const CREATED: &str = "2026-03-01T00:00:00Z";

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

/// Test-fixture setup: a users row in the post-016 shape.
async fn seed_account_without_role(db: &Arc<dyn DatabaseService>, id: &str) {
    let mut row: HashMap<String, Value> = HashMap::new();
    row.insert("id".into(), json!(id));
    row.insert("email".into(), json!(format!("{id}@example.com")));
    row.insert("display_name".into(), json!(id));
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

/// The columns of `users`.
async fn user_columns(db: &Arc<dyn DatabaseService>) -> Vec<String> {
    db.schema_columns(users::TABLE)
        .await
        .expect("introspect users")
}

/// Where 016 is in auth's list. The files after it are later changes, which
/// a database 016 has run on runs as any other pending file.
fn position_of_016(files: &[MigrationFile<'static>]) -> usize {
    files
        .iter()
        .position(|(name, _)| *name == ROLES_ARE_GRANTS)
        .expect("auth ships 016")
}

/// Auth's ledger as `(file, origin, adopted_from)`, in file order.
async fn auth_ledger(db: &Arc<dyn DatabaseService>) -> Vec<(String, Origin, String)> {
    history::ledger(db, auth::AUTH_BLOCK_ID)
        .await
        .expect("read the ledger")
        .into_iter()
        .map(|row| (row.file_name, row.origin, row.adopted_from))
        .collect()
}

/// The upgrade from the newest state before 016, then the next change to
/// the list.
async fn upgrades(db: Arc<dyn DatabaseService>, backend: Backend) {
    let (all, history_rows) = MIGRATIONS.for_backend(backend);
    let at_016 = position_of_016(all);
    history::fresh_admin(&db, backend)
        .await
        .expect("admin's list applies first, as every boot applies it");
    let before = history::build_state(&db, backend, &MIGRATIONS, states(backend).0)
        .await
        .expect("auth as it stood from 2026-09-23, recorded by the old runner");

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

    history::upgrade(&db, backend, &MIGRATIONS)
        .await
        .expect("auth's list, 016 included, applies to an upgraded database");

    let expected: Vec<(String, Origin, String)> = all
        .iter()
        .enumerate()
        .map(|(at, (name, _))| {
            if at < at_016 {
                (
                    name.to_string(),
                    Origin::Adopted,
                    before.list_hash.to_string(),
                )
            } else {
                (name.to_string(), Origin::Ran, String::new())
            }
        })
        .collect();
    assert_eq!(
        auth_ledger(&db).await,
        expected,
        "001-015 adopted, 016 and every later file ran"
    );

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
    let columns = user_columns(&db).await;
    assert!(
        !columns.iter().any(|c| c == "role"),
        "016 drops the inline column: {columns:?}"
    );
    assert!(columns.iter().any(|c| c == "email"), "{columns:?}");

    // The next change to the auth list runs only its new file.
    let mut next: Vec<MigrationFile<'static>> = all.to_vec();
    next.push((
        "999_next_auth_change",
        "-- A new file runs alone; 016 is recorded and never runs again.",
    ));
    apply_pending_via_service(&db, backend, auth::AUTH_BLOCK_ID, &next, history_rows)
        .await
        .expect("a later change applies");
    assert_eq!(grants(&db).await, after, "nothing is copied again");
}

/// A database that ran 016 as it first shipped — its `ADD COLUMN`, copy and
/// `DROP` — and recorded that list: every file through 016 is adopted from
/// that record, and only the files auth gained since run.
async fn ran_the_first_016(db: Arc<dyn DatabaseService>, backend: Backend) {
    history::fresh_admin(&db, backend)
        .await
        .expect("admin's list applies first");
    let state = history::build_state(&db, backend, &MIGRATIONS, states(backend).1)
        .await
        .expect("auth as 016 first shipped, recorded by the old runner");
    seed_account_without_role(&db, "u1").await;
    history::upgrade(&db, backend, &MIGRATIONS)
        .await
        .expect("adopts every file through 016");
    let all = MIGRATIONS.for_backend(backend).0;
    let at_016 = position_of_016(all);
    let ledger = auth_ledger(&db).await;
    assert_eq!(ledger.len(), all.len());
    let (through_016, later) = ledger.split_at(at_016 + 1);
    assert!(
        through_016
            .iter()
            .all(|(_, origin, from)| *origin == Origin::Adopted && from == state.list_hash),
        "every file through 016 is adopted: {ledger:?}"
    );
    assert!(
        later
            .iter()
            .all(|(_, origin, from)| *origin == Origin::Ran && from.is_empty()),
        "every file after 016 runs: {ledger:?}"
    );
    assert!(!user_columns(&db).await.iter().any(|c| c == "role"));
}

/// A database created after 016: admin's list, then auth's whole list. 001
/// creates the column, and 016 drops it again.
async fn applies_fresh(db: Arc<dyn DatabaseService>, backend: Backend) {
    history::fresh_admin(&db, backend)
        .await
        .expect("admin's list applies first");
    history::upgrade(&db, backend, &MIGRATIONS)
        .await
        .expect("auth's list applies to a fresh database");
    assert!(grants(&db).await.is_empty());
    assert!(auth_ledger(&db)
        .await
        .iter()
        .all(|(_, origin, _)| *origin == Origin::Ran));
    let columns = user_columns(&db).await;
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
    upgrades(sqlite(&dir, "upgraded.sqlite3").await, Backend::Sqlite).await;
}

#[tokio::test]
async fn migration_016_applies_to_a_fresh_sqlite_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    applies_fresh(sqlite(&dir, "fresh.sqlite3").await, Backend::Sqlite).await;
}

#[tokio::test]
async fn a_database_that_ran_the_first_016_adopts_it_on_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    ran_the_first_016(sqlite(&dir, "ran.sqlite3").await, Backend::Sqlite).await;
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
    for scenario in ["upgraded", "ran", "fresh"] {
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
            "upgraded" => upgrades(db, Backend::Postgres).await,
            "ran" => ran_the_first_016(db, Backend::Postgres).await,
            _ => applies_fresh(db, Backend::Postgres).await,
        }
    }
}
