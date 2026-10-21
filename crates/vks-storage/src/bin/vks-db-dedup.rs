//! `vks-db-dedup` — недеструктивная дедупликация справочника в SQLite.
//!
//! Использование (сервер должен быть остановлен, чтобы память не перезаписала
//! очищенный документ):
//!
//! ```text
//! vks-db-dedup vks_data.db            # dry-run: показать план
//! vks-db-dedup vks_data.db --apply    # backup + one transaction + запись
//! ```
//!
//! Перед записью создаётся копия `<db>.before-dedup-<unix>`, плюс вызывающий
//! обязан сохранить внешний backup. При ошибке транзакция откатывается.

use std::path::Path;
use std::str::FromStr;

use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use vks_storage::dedup::{canonical_default_layout, dedup_document, CONFIGURATION_KEY};

fn backup_path(db: &Path) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = db.file_name().and_then(|n| n.to_str()).unwrap_or("vks_data.db");
    let mut candidate = db.with_file_name(format!("{name}.before-dedup-{ts}"));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = db.with_file_name(format!("{name}.before-dedup-{ts}-{suffix}"));
        suffix += 1;
    }
    candidate
}

/// Проверка ссылочной целостности после очистки (links → layouts/audiochannels).
fn validate(doc: &Value) -> Result<(), String> {
    let resources = doc.get("resources").and_then(Value::as_object).ok_or("resources missing")?;
    let layouts: std::collections::BTreeSet<&str> = resources
        .get("layouts")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let audio: std::collections::BTreeSet<&str> = resources
        .get("audiochannels")
        .and_then(Value::as_object)
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if let Some(links) = resources.get("links").and_then(Value::as_object) {
        for (key, ids) in links {
            let Some(ids) = ids.as_array() else { return Err(format!("link {key} is not an array")) };
            for id in ids.iter().filter_map(Value::as_str) {
                if !layouts.contains(id) && !audio.contains(id) {
                    return Err(format!("dangling link {key} -> {id}"));
                }
            }
        }
    }
    if let Some(addresses) = resources.get("addresses").and_then(Value::as_object) {
        for (id, record) in addresses {
            if record.get("uri").and_then(Value::as_str).unwrap_or_default().trim().is_empty() {
                return Err(format!("address {id} has empty uri"));
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let db = args.next().ok_or("usage: vks-db-dedup <db-path> [--apply]")?;
    let apply = args.any(|arg| arg == "--apply");
    let db_path = Path::new(&db);
    if !db_path.is_file() {
        return Err(format!("db not found: {db}").into());
    }

    // Lock берём до чтения/backup: живой сервер держит тот же lock.
    let _lock = if apply {
        Some(
            vks_storage::db_lock::DbLock::acquire(db_path)
                .map_err(|e| format!("refusing --apply: {e}; stop the server or run against a copy"))?,
        )
    } else {
        None
    };

    let url = format!("sqlite://{}", db_path.display());
    let options = SqliteConnectOptions::from_str(&url)?.create_if_missing(false);
    let pool = SqlitePoolOptions::new().max_connections(1).connect_with(options).await?;

    if apply {
        let backup = backup_path(db_path);
        let escaped = backup.display().to_string().replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{escaped}'")).execute(&pool).await?;
        let backup_options = SqliteConnectOptions::from_str(&format!("sqlite://{}", backup.display()))?
            .create_if_missing(false)
            .read_only(true);
        let backup_pool = SqlitePoolOptions::new().max_connections(1).connect_with(backup_options).await?;
        let check: String = sqlx::query_scalar("PRAGMA quick_check").fetch_one(&backup_pool).await?;
        if check != "ok" {
            return Err(format!("backup quick_check failed: {check}").into());
        }
        println!("backup: {} (quick_check ok)", backup.display());
    }

    let mut tx = pool.begin().await?;
    let stored: Option<String> =
        sqlx::query_scalar("SELECT document_json FROM configuration_documents WHERE document_key = ?1")
            .bind(CONFIGURATION_KEY)
            .fetch_optional(&mut *tx)
            .await?;
    let raw = stored.ok_or("configuration document not found")?;
    let mut doc: Value = serde_json::from_str(&raw)?;
    let report = dedup_document(&mut doc);
    validate(&doc)?;

    let server_settings: Option<String> =
        sqlx::query_scalar("SELECT data_json FROM server_settings WHERE id = 'current'")
            .fetch_optional(&mut *tx)
            .await?;
    let canonical = canonical_default_layout(&doc);
    let mut settings_changed = false;
    if let (Some(settings_raw), Some(canonical)) = (server_settings.as_ref(), canonical.as_ref()) {
        let mut settings: Value = serde_json::from_str(settings_raw)?;
        let current = settings.get("default_layout").and_then(Value::as_str).unwrap_or_default().to_string();
        let present = doc
            .get("resources")
            .and_then(|r| r.get("layouts"))
            .and_then(Value::as_object)
            .is_some_and(|layouts| layouts.contains_key(&current));
        if !current.is_empty() && !present {
            settings["default_layout"] = Value::String(canonical.clone());
            settings_changed = true;
            let encoded = serde_json::to_string(&settings)?;
            if apply {
                sqlx::query(
                    "UPDATE server_settings SET data_json = ?1, updated_at = datetime('now') WHERE id = 'current'",
                )
                .bind(encoded)
                .execute(&mut *tx)
                .await?;
            }
            println!("default_layout: {current} -> {canonical} (dangling repaired)");
        }
    }

    if apply {
        let encoded = serde_json::to_string(&doc)?;
        sqlx::query("UPDATE configuration_documents SET document_json = ?1 WHERE document_key = ?2")
            .bind(encoded)
            .bind(CONFIGURATION_KEY)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        println!("written: {}", db_path.display());
    } else {
        tx.rollback().await?;
        println!("dry-run: no changes written (use --apply)");
    }
    if settings_changed && !apply {
        println!("default_layout would be repaired");
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
