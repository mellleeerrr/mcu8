//! Black-box: `vks-db-dedup --apply` отказывается писать, пока сервер
//! (или другой процесс) держит advisory lock на БД.

use std::path::PathBuf;
use std::process::Command;

use vks_storage::db_lock::DbLock;
use vks_storage::dedup::CONFIGURATION_KEY;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vks-dedup-lock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn apply_refuses_when_server_lock_is_held_and_leaves_db_untouched() {
    let dir = temp_dir();
    let db_path = dir.join("vks_data.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let storage = vks_storage::SqliteStorage::init(&url).await.unwrap();
    let document = serde_json::json!({"version":1,"resources":{"layouts":{},"links":{},"audiochannels":{}}});
    sqlx::query("INSERT INTO configuration_documents(document_key,document_json) VALUES(?1,?2)")
        .bind(CONFIGURATION_KEY)
        .bind(document.to_string())
        .execute(storage.pool())
        .await
        .unwrap();

    let lock = DbLock::acquire(&db_path).expect("simulate running server");
    let output = Command::new(env!("CARGO_BIN_EXE_vks-db-dedup"))
        .arg(&db_path)
        .arg("--apply")
        .output()
        .expect("run vks-db-dedup");
    assert!(!output.status.success(), "apply must fail while lock is held");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("locked by another process"), "stderr: {stderr}");
    drop(lock);

    let raw: String = sqlx::query_scalar("SELECT document_json FROM configuration_documents WHERE document_key=?1")
        .bind(CONFIGURATION_KEY)
        .fetch_one(storage.pool())
        .await
        .unwrap();
    assert_eq!(raw, document.to_string(), "DB must be untouched on refusal");
    std::fs::remove_dir_all(dir).unwrap();
}
