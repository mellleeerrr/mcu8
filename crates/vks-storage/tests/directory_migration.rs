//! T10b storage-level tests: normalized directory tables + `schema_state`
//! marker, atomic/idempotent migration, no resurrection, reset semantics and
//! SQLite↔PostgreSQL parity (`VKS_STORAGE_PG_URL`).

use vks_core::ports::storage::{
    DirectoryMigrationOutcome, DirectorySnapshot, Storage, StoredDirectoryAddress, StoredDirectoryGroup,
    StoredDirectoryProfile, StoredDirectoryRole,
};
use vks_storage::{MemoryStorage, PostgresStorage, SqliteStorage};

const KEY: &str = "pnm-definitions-v1";

fn temp_sqlite_url() -> (String, std::path::PathBuf) {
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        std::path::PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let path = dir.join(format!("vks-t10-store-{}.db", uuid::Uuid::new_v4().simple()));
    (format!("sqlite://{}?mode=rwc", path.to_string_lossy()), path)
}

fn address(owner_id: &str, id: &str, uri: &str, created_at: u64) -> StoredDirectoryAddress {
    StoredDirectoryAddress {
        owner_id: owner_id.to_string(),
        id: id.to_string(),
        uri: uri.to_string(),
        uri_canonical: uri.to_lowercase(),
        display_name: format!("addr {id}"),
        payload_json: format!(r#"{{"sip":{{"bitrate":{}}}}}"#, created_at * 64),
        address_json: format!(
            r#"{{"id":"{id}","uri":"{uri}","display_name":"addr {id}","created_at":{created_at},"future_field":"keep"}}"#
        ),
        created_at,
    }
}

fn fixture() -> DirectorySnapshot {
    DirectorySnapshot {
        profiles: vec![StoredDirectoryProfile {
            user_id: "u-1".to_string(),
            profile_json:
                r#"{"display_name":"Иван","roles":["770fada3-71d8-4176-a969-e12ec2059e64"],"groups":["g-1"],"created_at":1,"future_profile":true}"#
                    .to_string(),
            updated_at: 1,
        }],
        groups: vec![StoredDirectoryGroup {
            id: "g-1".to_string(),
            group_json: r#"{"display_name":"Группа","created_at":2}"#.to_string(),
            updated_at: 2,
        }],
        roles: vec![StoredDirectoryRole {
            id: "r-1".to_string(),
            role_json: r#"{"display_name":"Кастом","permissions":["users:read"],"created_at":3}"#.to_string(),
            updated_at: 3,
        }],
        group_members: vec![("g-1".to_string(), "u-1".to_string())],
        user_roles: vec![("u-1".to_string(), "770fada3-71d8-4176-a969-e12ec2059e64".to_string())],
        addresses: vec![
            address("", "global-1", "sip:one@example.com", 10),
            address("", "global-2", "sip:two@example.com", 11),
            address("u-1", "global-1", "sip:one@example.com", 10),
        ],
    }
}

fn legacy_document() -> String {
    r#"{"future_top":{"keep":true},"resources":{"layouts":{},"future_res":"keep","addresses":{"global-1":{"id":"global-1","uri":"sip:one@example.com","created_at":10}},"directory":{"users":{"u-1":{"display_name":"Иван","future_profile":true,"created_at":7}}}}}"#.to_string()
}

fn stripped_document() -> String {
    r#"{"future_top":{"keep":true},"resources":{"layouts":{},"future_res":"keep"}}"#.to_string()
}

#[tokio::test]
async fn memory_snapshot_roundtrip_marker_and_reset() {
    let storage = MemoryStorage::new();
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), DirectorySnapshot::default());
    storage.replace_directory_snapshot(&fixture()).await.unwrap();
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), fixture());
    let outcome = storage.migrate_directory_document(KEY, None, None, &fixture()).await.expect("migrate memory");
    assert_eq!(outcome, DirectoryMigrationOutcome::Applied);
    assert_eq!(storage.directory_schema_version().await.unwrap(), Some(1));
    let second = storage.migrate_directory_document(KEY, None, None, &DirectorySnapshot::default()).await.unwrap();
    assert_eq!(second, DirectoryMigrationOutcome::AlreadyMigrated);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), fixture());
    storage.reset().await.unwrap();
    assert_eq!(storage.directory_schema_version().await.unwrap(), None);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), DirectorySnapshot::default());
}

#[tokio::test]
async fn sqlite_migration_is_idempotent_and_writes_marker() {
    let (url, path) = temp_sqlite_url();
    let storage = SqliteStorage::init(&url).await.expect("sqlite init");
    let migrations_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(storage.pool())
        .await
        .expect("count migrations");
    storage.put_configuration_document(KEY, &legacy_document()).await.unwrap();
    let outcome = storage
        .migrate_directory_document(KEY, Some(&legacy_document()), Some(&stripped_document()), &fixture())
        .await
        .expect("migrate");
    assert_eq!(outcome, DirectoryMigrationOutcome::Applied);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), fixture());
    assert_eq!(storage.directory_schema_version().await.unwrap(), Some(1));
    assert_eq!(storage.get_configuration_document(KEY).await.unwrap().as_deref(), Some(stripped_document().as_str()));
    let migrations_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(storage.pool())
        .await
        .expect("count migrations");
    assert_eq!(migrations_before, migrations_after, "data migration must not add/_sqlx rows");

    let second = storage
        .migrate_directory_document(KEY, Some(&stripped_document()), None, &DirectorySnapshot::default())
        .await
        .unwrap();
    assert_eq!(second, DirectoryMigrationOutcome::AlreadyMigrated);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), fixture());
    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// A lost marker with non-empty SQL rows must NOT re-import the stale document:
/// existing rows win, deleted contacts stay deleted.
#[tokio::test]
async fn sqlite_lost_marker_does_not_resurrect_deleted_address() {
    let (url, path) = temp_sqlite_url();
    let storage = SqliteStorage::init(&url).await.expect("sqlite init");
    storage.put_configuration_document(KEY, &legacy_document()).await.unwrap();
    storage
        .migrate_directory_document(KEY, Some(&legacy_document()), Some(&stripped_document()), &fixture())
        .await
        .unwrap();
    // Delete one global address from SQL, then lose the marker.
    sqlx::query("DELETE FROM directory_addresses WHERE owner_id='' AND id='global-1'")
        .execute(storage.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM schema_state WHERE state_key='directory'").execute(storage.pool()).await.unwrap();
    // Restore the stale document (operator restored an old backup document).
    storage.put_configuration_document(KEY, &legacy_document()).await.unwrap();
    let outcome = storage
        .migrate_directory_document(KEY, Some(&legacy_document()), Some(&stripped_document()), &fixture())
        .await
        .expect("second migration");
    assert_eq!(outcome, DirectoryMigrationOutcome::Applied, "marker itself is rewritten");
    let after = storage.load_directory_snapshot().await.unwrap();
    assert!(
        !after.addresses.iter().any(|address| address.owner_id.is_empty() && address.id == "global-1"),
        "deleted contact must not be resurrected: {after:?}"
    );
    assert!(after.addresses.iter().any(|address| address.owner_id.is_empty() && address.id == "global-2"));
    assert_eq!(after.profiles.len(), 1, "adopt keeps existing rows");
    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// The document CAS guard detects a concurrent writer and rolls the whole
/// transaction back (no marker, no rows).
#[tokio::test]
async fn sqlite_document_changed_rolls_back() {
    let (url, path) = temp_sqlite_url();
    let storage = SqliteStorage::init(&url).await.expect("sqlite init");
    storage.put_configuration_document(KEY, r#"{"concurrent":true}"#).await.unwrap();
    let outcome = storage
        .migrate_directory_document(KEY, Some(r#"{"expected":true}"#), Some(&stripped_document()), &fixture())
        .await
        .expect("cas");
    assert_eq!(outcome, DirectoryMigrationOutcome::DocumentChanged);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), DirectorySnapshot::default());
    assert_eq!(storage.directory_schema_version().await.unwrap(), None);
    assert_eq!(storage.get_configuration_document(KEY).await.unwrap().as_deref(), Some(r#"{"concurrent":true}"#));
    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// Duplicate global canonical URIs are rejected by validation before the
/// transaction: marker/rows/document stay untouched.
#[tokio::test]
async fn sqlite_invalid_snapshot_rolls_back() {
    let (url, path) = temp_sqlite_url();
    let storage = SqliteStorage::init(&url).await.expect("sqlite init");
    storage.put_configuration_document(KEY, &legacy_document()).await.unwrap();
    let mut bad = fixture();
    bad.addresses.push(address("", "dup", "SIP:one@example.com", 99));
    let error = storage
        .migrate_directory_document(KEY, Some(&legacy_document()), Some(&stripped_document()), &bad)
        .await
        .expect_err("duplicate global canonical");
    assert!(error.to_string().contains("duplicate global canonical"), "{error}");
    assert_eq!(storage.directory_schema_version().await.unwrap(), None);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), DirectorySnapshot::default());
    assert_eq!(storage.get_configuration_document(KEY).await.unwrap().as_deref(), Some(legacy_document().as_str()));
    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn sqlite_reset_clears_directory_rows_and_marker() {
    let (url, path) = temp_sqlite_url();
    let storage = SqliteStorage::init(&url).await.expect("sqlite init");
    storage.put_configuration_document(KEY, &legacy_document()).await.unwrap();
    storage
        .migrate_directory_document(KEY, Some(&legacy_document()), Some(&stripped_document()), &fixture())
        .await
        .unwrap();
    storage.reset().await.expect("reset");
    assert_eq!(storage.directory_schema_version().await.unwrap(), None);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), DirectorySnapshot::default());
    assert!(storage.get_configuration_document(KEY).await.unwrap().is_none());
    // Migration can apply again after reset.
    storage.put_configuration_document(KEY, &legacy_document()).await.unwrap();
    let outcome = storage
        .migrate_directory_document(KEY, Some(&legacy_document()), Some(&stripped_document()), &fixture())
        .await
        .unwrap();
    assert_eq!(outcome, DirectoryMigrationOutcome::Applied);
    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// SQLite == PostgreSQL: same fixture/document → same snapshot JSON strings and
/// same migration outcomes. Gated by `VKS_STORAGE_PG_URL` (the existing CI
/// `test-postgres` job runs `cargo test -p vks-storage`, so it is exercised
/// there; locally it is skipped and reported as such).
#[tokio::test]
async fn sqlite_and_pg_load_same_directory_snapshot() {
    let Some(pg_url) = std::env::var("VKS_STORAGE_PG_URL").ok().filter(|url| !url.trim().is_empty()) else {
        eprintln!("SKIP sqlite_and_pg_load_same_directory_snapshot: VKS_STORAGE_PG_URL is not set");
        return;
    };
    let (url, path) = temp_sqlite_url();
    let sqlite = SqliteStorage::init(&url).await.expect("sqlite init");
    let pg = PostgresStorage::init(&pg_url).await.expect("postgres init");
    let key = format!("t10-parity-{}", uuid::Uuid::new_v4().simple());
    let document = legacy_document();
    let stripped = stripped_document();

    // Isolate the shared test DB from previous runs of this test.
    sqlx::query("DELETE FROM schema_state").execute(sqlite.pool()).await.expect("clear sqlite marker");
    sqlx::query("DELETE FROM schema_state").execute(pg.pool()).await.expect("clear pg marker");
    for storage in [&sqlite as &dyn Storage, &pg as &dyn Storage] {
        storage.replace_directory_snapshot(&fixture()).await.expect("replace");
        storage.put_configuration_document(&key, &document).await.expect("put doc");
    }
    let sqlite_snapshot = sqlite.load_directory_snapshot().await.expect("sqlite load");
    let pg_snapshot = pg.load_directory_snapshot().await.expect("pg load");
    assert_eq!(sqlite_snapshot, pg_snapshot, "same rows/order from SQLite and PostgreSQL");
    assert_eq!(sqlite_snapshot, fixture());

    // Same migration result on both backends (rows already exist -> adopt).
    let sqlite_outcome = sqlite
        .migrate_directory_document(&key, Some(&document), Some(&stripped), &fixture())
        .await
        .expect("sqlite migrate");
    let pg_outcome =
        pg.migrate_directory_document(&key, Some(&document), Some(&stripped), &fixture()).await.expect("pg migrate");
    assert_eq!(sqlite_outcome, DirectoryMigrationOutcome::Applied);
    assert_eq!(pg_outcome, DirectoryMigrationOutcome::Applied);
    assert_eq!(
        sqlite.get_configuration_document(&key).await.unwrap().as_deref(),
        pg.get_configuration_document(&key).await.unwrap().as_deref()
    );
    let second_sqlite = sqlite
        .migrate_directory_document(&key, Some(&stripped), None, &DirectorySnapshot::default())
        .await
        .expect("sqlite second");
    let second_pg = pg
        .migrate_directory_document(&key, Some(&stripped), None, &DirectorySnapshot::default())
        .await
        .expect("pg second");
    assert_eq!(second_sqlite, DirectoryMigrationOutcome::AlreadyMigrated);
    assert_eq!(second_pg, DirectoryMigrationOutcome::AlreadyMigrated);
    assert_eq!(sqlite.load_directory_snapshot().await.unwrap(), pg.load_directory_snapshot().await.unwrap());
    assert_eq!(sqlite.directory_schema_version().await.unwrap(), Some(1));
    assert_eq!(pg.directory_schema_version().await.unwrap(), Some(1));

    // ---- Import branch parity (review B P2-2): empty tables + legacy document.
    // The existing part above only exercised adopt; clear both backends so
    // `existing == 0` and the imported snapshot is the one under test.
    for table in [
        "directory_profiles",
        "directory_groups",
        "directory_roles",
        "directory_group_members",
        "directory_user_roles",
        "directory_addresses",
        "schema_state",
    ] {
        sqlx::query(&format!("DELETE FROM {table}")).execute(sqlite.pool()).await.expect("clear sqlite table");
        sqlx::query(&format!("DELETE FROM {table}")).execute(pg.pool()).await.expect("clear pg table");
    }
    let import_key = format!("t10-parity-import-{}", uuid::Uuid::new_v4().simple());
    let import_document = r#"{"future_top":{"keep":true},"resources":{"layouts":{},"future_res":"keep","addresses":{"global-1":{"id":"global-1","uri":"sip:one@example.com","created_at":10,"future_address":true}},"directory":{"users":{"u-1":{"display_name":"Иван","created_at":7,"future_profile":true}}}}}"#;
    let import_stripped = r#"{"future_top":{"keep":true},"resources":{"layouts":{},"future_res":"keep"}}"#;
    let import_snapshot = DirectorySnapshot {
        profiles: vec![StoredDirectoryProfile {
            user_id: "u-1".to_string(),
            profile_json: r#"{"display_name":"Иван","created_at":7,"future_profile":true}"#.to_string(),
            updated_at: 7,
        }],
        addresses: vec![address("", "global-1", "sip:one@example.com", 10)],
        ..DirectorySnapshot::default()
    };
    for storage in [&sqlite as &dyn Storage, &pg as &dyn Storage] {
        storage.put_configuration_document(&import_key, import_document).await.expect("put import doc");
    }
    let import_sqlite = sqlite
        .migrate_directory_document(&import_key, Some(import_document), Some(import_stripped), &import_snapshot)
        .await
        .expect("sqlite import");
    let import_pg = pg
        .migrate_directory_document(&import_key, Some(import_document), Some(import_stripped), &import_snapshot)
        .await
        .expect("pg import");
    assert_eq!(import_sqlite, DirectoryMigrationOutcome::Applied);
    assert_eq!(import_pg, DirectoryMigrationOutcome::Applied);
    assert_eq!(
        sqlite.load_directory_snapshot().await.unwrap(),
        import_snapshot,
        "SQLite import branch stores the legacy snapshot"
    );
    assert_eq!(
        pg.load_directory_snapshot().await.unwrap(),
        import_snapshot,
        "PostgreSQL import branch stores the legacy snapshot"
    );
    assert_eq!(
        sqlite.load_directory_snapshot().await.unwrap(),
        pg.load_directory_snapshot().await.unwrap(),
        "import branch: SQLite and PostgreSQL snapshots match"
    );
    assert_eq!(sqlite.get_configuration_document(&import_key).await.unwrap().as_deref(), Some(import_stripped));
    assert_eq!(pg.get_configuration_document(&import_key).await.unwrap().as_deref(), Some(import_stripped));
    assert_eq!(sqlite.directory_schema_version().await.unwrap(), Some(1));
    assert_eq!(pg.directory_schema_version().await.unwrap(), Some(1));

    sqlite.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// MemoryStorage `DocumentChanged` must be a no-op: "Nothing was written"
/// (T10 P3-1). The old order assigned the snapshot before the CAS check.
#[tokio::test]
async fn memory_document_changed_writes_nothing() {
    let storage = MemoryStorage::new();
    storage.put_configuration_document(KEY, r#"{"old":true}"#).await.unwrap();
    let outcome = storage
        .migrate_directory_document(KEY, Some(r#"{"stale":true}"#), Some(r#"{"new":true}"#), &fixture())
        .await
        .expect("cas");
    assert_eq!(outcome, DirectoryMigrationOutcome::DocumentChanged);
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), DirectorySnapshot::default());
    assert_eq!(storage.directory_schema_version().await.unwrap(), None);
    assert_eq!(storage.get_configuration_document(KEY).await.unwrap().as_deref(), Some(r#"{"old":true}"#));
}
