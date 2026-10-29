//! Black-box T10 P2-3: with `storage_allow_fallback=true`, a document that is
//! valid JSON but cannot be hydrated into `ResourceState` used to make
//! `sync_from_storage` non-fatal; the next mutation then replaced the SQL
//! directory with the empty RAM snapshot. The server must now exit 78 and leave
//! the directory rows untouched.

use std::path::PathBuf;
use std::process::Command;

use vks_core::ports::storage::{DirectorySnapshot, Storage, StoredDirectoryAddress, StoredDirectoryProfile};
use vks_storage::SqliteStorage;

const KEY: &str = "pnm-definitions-v1";

fn temp_db() -> (PathBuf, String) {
    let dir = if std::path::Path::new("/dev/shm").is_dir() { PathBuf::from("/dev/shm") } else { std::env::temp_dir() };
    let path = dir.join(format!("vks-t10-failclosed-{}.db", uuid::Uuid::new_v4().simple()));
    (path.clone(), format!("sqlite://{}?mode=rwc", path.to_string_lossy()))
}

fn fixture() -> DirectorySnapshot {
    DirectorySnapshot {
        profiles: vec![StoredDirectoryProfile {
            user_id: "u-1".to_string(),
            profile_json: r#"{"display_name":"Иван","created_at":1}"#.to_string(),
            updated_at: 1,
        }],
        addresses: vec![StoredDirectoryAddress {
            owner_id: String::new(),
            id: "global-1".to_string(),
            uri: "sip:one@example.com".to_string(),
            uri_canonical: "sip:one@example.com".to_string(),
            display_name: "one".to_string(),
            payload_json: r#"{"sip":{}}"#.to_string(),
            address_json: r#"{"id":"global-1","uri":"sip:one@example.com","future_address":true}"#.to_string(),
            created_at: 10,
        }],
        ..DirectorySnapshot::default()
    }
}

#[tokio::test]
async fn corrupt_document_with_fallback_exits_78_and_keeps_directory_rows() {
    let (path, url) = temp_db();
    {
        let storage = SqliteStorage::init(&url).await.expect("sqlite init");
        storage.replace_directory_snapshot(&fixture()).await.expect("seed rows");
        // `run()` would strip legacy keys from this document; it is accepted by
        // the migration parser but rejected later by `ResourceState` hydration.
        let document = r#"{"version":1,"resources":{"layouts":"not-an-object"}}"#;
        storage
            .migrate_directory_document(KEY, Some(document), None, &DirectorySnapshot::default())
            .await
            .expect("marker");
        storage.put_configuration_document(KEY, document).await.expect("corrupt document ready");
        storage.pool().close().await;
    }

    let bin = env!("CARGO_BIN_EXE_vks-server");
    let output = Command::new(bin)
        .env_clear()
        .env("VKS_DATABASE_URL", &url)
        .env("VKS_STORAGE_ALLOW_FALLBACK", "true")
        .env("VKS_ALLOW_INSECURE_DEFAULTS", "true")
        .env("VKS_LISTEN_ADDR", "127.0.0.1:0")
        .env("VKS_MEDIA_LISTEN_ADDR", "127.0.0.1:0")
        .output()
        .expect("spawn vks-server");
    let logs = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.status.code(), Some(78), "fallback must not mask a directory hydration failure; logs: {logs}");
    assert!(logs.contains("failed to hydrate directory data from storage"), "logs: {logs}");
    assert!(logs.contains("storage_allow_fallback does not apply"), "logs: {logs}");

    // No mutation happened, but prove the rows are still there and hydration of
    // the SQL snapshot still works after fixing the document.
    let storage = SqliteStorage::init(&url).await.expect("reopen sqlite");
    assert_eq!(storage.load_directory_snapshot().await.expect("snapshot"), fixture());
    assert_eq!(storage.directory_schema_version().await.expect("marker"), Some(1));
    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}
