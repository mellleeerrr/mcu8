//! T10b tests: directory/address book moved from the configuration document to
//! normalized SQL, one-shot idempotent migration, no resurrection.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};

use vks_core::ports::storage::{DirectorySnapshot, Storage};

use super::super::resources::CONFIGURATION_KEY;
use super::LEGACY_DIRECTORY_KEYS;
use crate::api_v3::tests::{app, json_body, login_token, request, send, test_state};

fn temp_sqlite_url() -> (String, std::path::PathBuf) {
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        std::path::PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let path = dir.join(format!("vks-t10-{}.db", uuid::Uuid::new_v4().simple()));
    let url = format!("sqlite://{}?mode=rwc", path.to_string_lossy());
    (url, path)
}

fn legacy_document() -> Value {
    json!({
        "future_top": {"keep": [1, 2, 3]},
        "resources": {
            "layouts": {},
            "audiochannels": {},
            "links": {},
            "schedules": {},
            "contacts": {},
            "active_layouts": {},
            "future_res": "keep",
            "addresses": {
                "global-1": {"id": "global-1", "uri": "h323:192.168.1.10", "display_name": "Зал", "created_at": 3,
                              "payload": {"h323": {"bitrate": 4096}}, "future_address": true}
            },
            "directory": {
                "users": {
                    "u-1": {"display_name": "Гидратация", "roles": ["770fada3-71d8-4176-a969-e12ec2059e64"],
                            "groups": ["g-1"], "created_at": 1, "future_profile": "keep"}
                },
                "groups": {"g-1": {"display_name": "Группа рестарта", "created_at": 2}},
                "roles": {"r-1": {"display_name": "Кастом", "permissions": ["users:read"], "created_at": 3}},
                "user_addresses": {"u-1": {"a-1": {"id": "a-1", "uri": "sip:a@b", "display_name": "Адрес", "created_at": 1,
                                               "future_personal": "keep"}}}
            }
        }
    })
}

async fn sqlite_storage(url: &str) -> Arc<vks_storage::SqliteStorage> {
    Arc::new(vks_storage::SqliteStorage::init(url).await.expect("sqlite init"))
}

/// T10 acceptance: "поле убрано из JSON"; unknown fields and other resources
/// survive the raw-Value rewrite.
#[tokio::test]
async fn migration_strips_legacy_keys_preserves_unknown_fields_and_is_idempotent() {
    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let raw = legacy_document().to_string();
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    let report = super::run_on_storage(&*storage, Some(&raw)).await.expect("migrate");
    assert_eq!(report.outcome, "applied");
    assert_eq!(report.profiles, 1);
    assert_eq!(report.groups, 1);
    assert_eq!(report.roles, 1);
    assert_eq!(report.global_addresses, 1);
    assert_eq!(report.personal_addresses, 1);

    let document: Value =
        serde_json::from_str(&storage.get_configuration_document(CONFIGURATION_KEY).await.unwrap().unwrap()).unwrap();
    assert_eq!(document["future_top"], json!({"keep": [1, 2, 3]}));
    assert_eq!(document["resources"]["future_res"], "keep");
    assert_eq!(document["resources"]["active_layouts"], json!({}));
    for key in LEGACY_DIRECTORY_KEYS {
        assert!(document["resources"].get(key).is_none(), "legacy key {key} must be stripped");
    }

    let snapshot = storage.load_directory_snapshot().await.unwrap();
    assert_eq!(snapshot.profiles.len(), 1);
    assert_eq!(snapshot.groups.len(), 1);
    assert_eq!(snapshot.roles.len(), 1);
    assert_eq!(snapshot.group_members, vec![("g-1".to_string(), "u-1".to_string())]);
    let profile_json: Value = serde_json::from_str(&snapshot.profiles[0].profile_json).unwrap();
    assert_eq!(profile_json["future_profile"], "keep", "unknown directory fields preserved in profile_json");
    let global = snapshot.addresses.iter().find(|address| address.owner_id.is_empty()).unwrap();
    assert_eq!(global.id, "global-1");
    assert_eq!(global.uri, "h323:192.168.1.10");
    let address_json: Value = serde_json::from_str(&global.address_json).unwrap();
    assert_eq!(address_json["future_address"], true, "unknown address fields preserved in address_json");
    assert!(snapshot.addresses.iter().any(|address| address.owner_id == "u-1" && address.id == "a-1"));
    assert_eq!(storage.directory_schema_version().await.unwrap(), Some(1));

    let second = super::run_on_storage(&*storage, Some(&raw)).await.expect("second");
    assert_eq!(second.outcome, "already_migrated");
    assert_eq!(storage.load_directory_snapshot().await.unwrap(), snapshot);
    let expected_document = serde_json::to_string(&document).unwrap();
    assert_eq!(
        storage.get_configuration_document(CONFIGURATION_KEY).await.unwrap().as_deref(),
        Some(expected_document.as_str())
    );
}

/// A stale legacy document must not override existing SQL rows (rerun/adopt).
#[tokio::test]
async fn stale_document_does_not_override_sql_rows() {
    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let raw = legacy_document().to_string();
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    super::run_on_storage(&*storage, Some(&raw)).await.expect("migrate");

    // Simulate a later delete of the global contact + personal copy.
    let mut snapshot = storage.load_directory_snapshot().await.unwrap();
    snapshot.addresses.retain(|address| !address.id.starts_with("global-") && address.id != "a-1");
    storage.replace_directory_snapshot(&snapshot).await.unwrap();

    // Put the stale legacy document back (e.g. operator restored a backup file)
    // and re-run: marker is present -> nothing is imported.
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    let report = super::run_on_storage(&*storage, Some(&raw)).await.expect("rerun");
    assert_eq!(report.outcome, "already_migrated");
    let after = storage.load_directory_snapshot().await.unwrap();
    assert!(!after.addresses.iter().any(|address| address.id.starts_with("global-") || address.id == "a-1"));
}

/// T10: broken legacy entries are skipped, the rest is imported; the document
/// is still stripped and the marker written.
#[tokio::test]
async fn broken_legacy_entries_do_not_abort_migration() {
    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let raw = json!({
        "resources": {
            "addresses": {
                "good": {"uri": "sip:good@example.com", "created_at": 1},
                "broken": {"uri": 42}
            },
            "directory": {
                "users": {
                    "good": {"display_name": "Живой"},
                    "broken": {"display_name": 7}
                },
                "groups": {"g": {"display_name": "Группа"}},
                "roles": {"r": {"display_name": "Роль", "permissions": ["users:read"]}}
            }
        }
    })
    .to_string();
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    let report = super::run_on_storage(&*storage, Some(&raw)).await.expect("tolerant migration");
    assert_eq!(report.outcome, "applied");
    assert_eq!(report.profiles, 1);
    assert_eq!(report.global_addresses, 1);
    let snapshot = storage.load_directory_snapshot().await.unwrap();
    assert!(snapshot.profiles.iter().any(|profile| profile.user_id == "good"));
    assert!(!snapshot.profiles.iter().any(|profile| profile.user_id == "broken"));
    assert!(snapshot.addresses.iter().any(|address| address.id == "good"));
    assert!(!snapshot.addresses.iter().any(|address| address.id == "broken"));
}

/// Invalid document JSON must abort the migration before anything is written.
#[tokio::test]
async fn invalid_document_aborts_without_marker_or_rows() {
    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let raw = "{not json";
    storage.put_configuration_document(CONFIGURATION_KEY, raw).await.unwrap();
    let error = super::run_on_storage(&*storage, Some(raw)).await.expect_err("must fail");
    assert!(error.contains("not valid JSON"), "{error}");
    assert!(storage.load_directory_snapshot().await.unwrap().is_empty());
    assert_eq!(storage.directory_schema_version().await.unwrap(), None);
    assert_eq!(storage.get_configuration_document(CONFIGURATION_KEY).await.unwrap().as_deref(), Some(raw));
}

/// Duplicate global canonical addresses collapse; personal copies and
/// `primary_address` aliases point at the surviving id.
#[test]
fn duplicate_canonical_global_addresses_merge_and_rewrite_refs() {
    let raw = json!({
        "resources": {
            "addresses": {
                "addr-old": {"id": "addr-old", "uri": "sip:user@example.com:5060", "created_at": 10,
                             "payload": {"sip": {"bitrate": 1024}}},
                "addr-new": {"id": "addr-new", "uri": "sip:user@example.com", "display_name": "Зал",
                             "created_at": 20, "payload": {"sip": {"bitrate": 4096}}}
            },
            "directory": {
                "users": {"u-1": {"display_name": "U", "primary_address": "addr-new", "created_at": 1}},
                "user_addresses": {"u-1": {
                    "addr-new": {"id": "addr-new", "uri": "sip:user@example.com", "created_at": 20},
                    "personal-only": {"id": "personal-only", "uri": "h323:192.168.1.5", "created_at": 1}
                }}
            }
        }
    })
    .to_string();
    let parsed = super::parse_legacy_document(Some(&raw)).expect("parse");
    let globals: Vec<_> = parsed.snapshot.addresses.iter().filter(|address| address.owner_id.is_empty()).collect();
    assert_eq!(globals.len(), 1, "duplicate canonical globals must merge");
    assert_eq!(globals[0].id, "addr-old", "earliest created_at wins");
    assert_eq!(globals[0].display_name, "Зал", "non-empty name fills the kept record");
    let payload: Value = serde_json::from_str(&globals[0].payload_json).unwrap();
    assert_eq!(payload["sip"]["bitrate"], 4096, "payloads merge");
    let personal: Vec<_> = parsed.snapshot.addresses.iter().filter(|address| address.owner_id == "u-1").collect();
    assert_eq!(personal.len(), 2);
    assert!(personal.iter().any(|address| address.id == "addr-old"));
    assert!(personal.iter().any(|address| address.id == "personal-only"));
    let profile = parsed.snapshot.profiles.iter().find(|profile| profile.user_id == "u-1").unwrap();
    let profile_json: Value = serde_json::from_str(&profile.profile_json).unwrap();
    assert_eq!(profile_json["primary_address"], "addr-old");
}

/// Directory CRUD via `/api/v3` plus global/personal addresses survive a real
/// SQLite restart (new state, sync from SQL).
#[tokio::test]
async fn directory_crud_survives_sqlite_restart() {
    let (url, path) = temp_sqlite_url();
    let storage = sqlite_storage(&url).await;
    let (mut base, _) = test_state();
    Arc::get_mut(&mut base).unwrap().storage = Some(storage.clone());
    let server = app(base);
    let token = login_token(&server, "admin", "s3cret").await;

    let response =
        send(&server, request("POST", "/api/v3/users", Some(&token), json!({"display_name": "Рестарт"}))).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let user_id = json_body(response).await["user"]["id"].as_str().unwrap().to_string();
    let response =
        send(&server, request("POST", "/api/v3/groups", Some(&token), json!({"display_name": "Группа SQL"}))).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let group_id = json_body(response).await["group"]["id"].as_str().unwrap().to_string();
    let response =
        send(&server, request("POST", "/api/v3/roles", Some(&token), json!({"display_name": "Роль SQL"}))).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let role_id = json_body(response).await["role"]["id"].as_str().unwrap().to_string();
    for uri in
        [format!("/api/v3/users/{user_id}/groups/{group_id}"), format!("/api/v3/users/{user_id}/roles/{role_id}")]
    {
        let response = send(&server, request("POST", &uri, Some(&token), json!({}))).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/call_strings",
            Some(&token),
            json!({"uri": "sip:192.168.1.60", "display_name": "Зал 60"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let address_id = json_body(response).await["call_string"]["id"].as_str().unwrap().to_string();
    let response = send(
        &server,
        request("POST", &format!("/api/v3/users/{user_id}/call_strings/{address_id}"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    drop(server);

    // Restart: brand-new AppState over the same SQLite file.
    let (mut restarted, _) = test_state();
    Arc::get_mut(&mut restarted).unwrap().storage = Some(storage.clone());
    restarted.sync_from_storage().await.expect("sync after restart");
    let server = app(restarted);
    let token = login_token(&server, "admin", "s3cret").await;

    let response = send(&server, request("GET", &format!("/api/v3/users/{user_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["user"]["display_name"], "Рестарт");
    let response = send(&server, request("GET", &format!("/api/v3/groups/{group_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["group"]["display_name"], "Группа SQL");
    let response = send(&server, request("GET", &format!("/api/v3/roles/{role_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response =
        send(&server, request("GET", &format!("/api/v3/users/{user_id}/call_strings"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["count"], 1, "personal address copy survives restart: {body}");
    assert_eq!(body["call_strings"][0]["uri"], "sip:192.168.1.60");

    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// Personal address copies are independent rows: deleting the global record
/// must not delete them (and must not hit a PK conflict on attach).
#[tokio::test]
async fn personal_address_copy_survives_global_delete() {
    let (state, _) = test_state();
    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let state = {
        let mut inner = Arc::try_unwrap(state).expect("single owner");
        inner.storage = Some(storage.clone());
        Arc::new(inner)
    };
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", "/api/v3/users", Some(&token), json!({"display_name": "Личный"}))).await;
    let user_id = json_body(response).await["user"]["id"].as_str().unwrap().to_string();
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/call_strings",
            Some(&token),
            json!({"uri": "sip:192.168.1.77", "display_name": "Глобальный"}),
        ),
    )
    .await;
    let address_id = json_body(response).await["call_string"]["id"].as_str().unwrap().to_string();
    let attach = send(
        &server,
        request("POST", &format!("/api/v3/users/{user_id}/call_strings/{address_id}"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(attach.status(), StatusCode::OK, "attach must not steal/conflict with the global row");
    let delete =
        send(&server, request("DELETE", &format!("/api/v3/call_strings/{address_id}"), Some(&token), json!({}))).await;
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
    let list =
        send(&server, request("GET", &format!("/api/v3/users/{user_id}/call_strings"), Some(&token), json!({}))).await;
    let body = json_body(list).await;
    assert_eq!(body["count"], 1, "personal copy must survive the global delete: {body}");
    let snapshot = storage.load_directory_snapshot().await.unwrap();
    assert!(!snapshot.addresses.iter().any(|address| address.owner_id.is_empty()));
    assert!(snapshot.addresses.iter().any(|address| address.owner_id == user_id && address.id == address_id));
}

/// Hot reload replaces the RAM directory cache from SQL, not from JSON.
#[tokio::test]
async fn hot_reload_preserves_directory_and_addresses() {
    let (state, _) = test_state();
    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let state = {
        let mut inner = Arc::try_unwrap(state).expect("single owner");
        inner.storage = Some(storage.clone());
        Arc::new(inner)
    };
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(&server, request("POST", "/api/v3/users", Some(&token), json!({"display_name": "Hot"}))).await;
    let user_id = json_body(response).await["user"]["id"].as_str().unwrap().to_string();
    let response =
        send(&server, request("POST", "/api/v3/groups", Some(&token), json!({"display_name": "Hot group"}))).await;
    let group_id = json_body(response).await["group"]["id"].as_str().unwrap().to_string();
    send(&server, request("POST", &format!("/api/v3/users/{user_id}/groups/{group_id}"), Some(&token), json!({})))
        .await;
    let response =
        send(&server, request("POST", "/api/v3/call_strings", Some(&token), json!({"uri": "h323:192.168.1.90"}))).await;
    let address_id = json_body(response).await["call_string"]["id"].as_str().unwrap().to_string();
    drop(server);

    state.hot_reload_from_storage().await.expect("hot reload");

    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.directory.users[&user_id].display_name, "Hot");
    assert!(resources.directory.users[&user_id].groups.contains(&group_id));
    assert_eq!(resources.directory.groups[&group_id].display_name, "Hot group");
    assert_eq!(resources.addresses[&address_id].uri, "h323:192.168.1.90");
    drop(resources);

    // Directory cache is not backed by the JSON document.
    let raw = storage.get_configuration_document(CONFIGURATION_KEY).await.unwrap().unwrap();
    assert!(!raw.contains("\"directory\""));
    assert!(!raw.contains("\"addresses\""));
}

/// `Storage::reset()` clears the marker together with the rows, so a migration
/// can run again (admin-visible equivalent for MemoryStorage).
#[tokio::test]
async fn reset_allows_migration_again() {
    let storage = Arc::new(vks_storage::MemoryStorage::new());
    let raw = legacy_document().to_string();
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    super::run_on_storage(&*storage, Some(&raw)).await.expect("migrate");
    assert_eq!(storage.directory_schema_version().await.unwrap(), Some(1));
    storage.reset().await.expect("reset");
    assert_eq!(storage.directory_schema_version().await.unwrap(), None);
    assert_eq!(storage.load_directory_snapshot().await, Ok(DirectorySnapshot::default()));
    // Fresh legacy document -> migration applies again.
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    let report = super::run_on_storage(&*storage, Some(&raw)).await.expect("migrate after reset");
    assert_eq!(report.outcome, "applied");
    assert_eq!(report.profiles, 1);
}

/// T10 P2-1: unknown/future address fields must round-trip
/// legacy JSON → migration → RAM → mutation → SQL → restart → API.
#[tokio::test]
async fn future_address_fields_survive_mutation_restart_and_api() {
    let (url, path) = temp_sqlite_url();
    let storage = sqlite_storage(&url).await;
    let raw = legacy_document().to_string();
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();

    let (mut base, _) = test_state();
    Arc::get_mut(&mut base).unwrap().storage = Some(storage.clone());
    let state = base;
    let report = super::run(&state).await.expect("migrate");
    assert_eq!(report.outcome, "applied");
    state.sync_from_storage().await.expect("hydrate after migration");

    // Any directory mutation rewrites the whole snapshot: the unknown fields
    // must survive that rewrite in SQL.
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", "/api/v3/groups", Some(&token), json!({"display_name": "После миграции"}))).await;
    assert_eq!(response.status(), StatusCode::CREATED, "unrelated group mutation");
    drop(server);

    let snapshot = storage.load_directory_snapshot().await.unwrap();
    let global = snapshot.addresses.iter().find(|address| address.owner_id.is_empty()).expect("global row");
    let global_json: Value = serde_json::from_str(&global.address_json).unwrap();
    assert_eq!(global_json["future_address"], true, "global unknown field lost on mutation: {global_json}");
    let personal = snapshot.addresses.iter().find(|address| address.owner_id == "u-1").expect("personal row");
    let personal_json: Value = serde_json::from_str(&personal.address_json).unwrap();
    assert_eq!(personal_json["future_personal"], "keep", "personal unknown field lost: {personal_json}");

    // Restart: new state hydrates from SQL, API returns the same fields.
    let (mut restarted, _) = test_state();
    Arc::get_mut(&mut restarted).unwrap().storage = Some(storage.clone());
    restarted.sync_from_storage().await.expect("restart hydrate");
    {
        let resources = restarted.api_v3.resources.lock().await;
        assert!(
            resources.addresses.contains_key("global-1"),
            "restart hydrate lost the address: {:?}",
            resources.addresses.keys().collect::<Vec<_>>()
        );
    }
    let server = app(restarted);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(&server, request("GET", "/api/v3/call_strings/global-1", Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["call_string"]["future_address"], true, "API must expose the forwarded field: {body}");
    drop(server);

    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// T10 P2-3: a document that is valid JSON but cannot hydrate `ResourceState`
/// makes `sync_from_storage` fail. Even with `storage_allow_fallback=true` the
/// process must fail closed and directory mutations must not replace SQL rows
/// with the empty RAM snapshot.
#[tokio::test]
async fn failed_directory_hydration_refuses_mutations_and_keeps_sql() {
    let (url, path) = temp_sqlite_url();
    let storage = sqlite_storage(&url).await;
    let raw = legacy_document().to_string();
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    {
        let (mut seeded, _) = test_state();
        Arc::get_mut(&mut seeded).unwrap().storage = Some(storage.clone());
        let seeded = Arc::new(seeded);
        super::run(&seeded).await.expect("migrate");
        seeded.sync_from_storage().await.expect("initial hydrate");
    }

    // Valid JSON, stripped of legacy keys, but `resources.layouts` has the
    // wrong type: `directory_migrate::run` is happy, hydration is not.
    let corrupt = r#"{"version":1,"resources":{"layouts":"not-an-object"}}"#;
    storage.put_configuration_document(CONFIGURATION_KEY, corrupt).await.unwrap();

    let (mut restarted, _) = test_state();
    Arc::get_mut(&mut restarted).unwrap().storage = Some(storage.clone());
    let error = restarted.sync_from_storage().await.expect_err("hydration must fail");
    assert!(!error.is_empty(), "explicit hydration error expected");
    assert!(restarted.directory_hydration_failed(), "state must be marked read-only for directory writes");

    let before = storage.load_directory_snapshot().await.unwrap();
    assert!(!before.is_empty(), "fixture rows present");
    let server = app(restarted);
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", "/api/v3/groups", Some(&token), json!({"display_name": "Не должно записаться"})))
            .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = json_body(response).await;
    assert!(
        body.to_string().contains("refusing to overwrite the SQL directory snapshot"),
        "explicit fail-closed error expected: {body}"
    );
    let after = storage.load_directory_snapshot().await.unwrap();
    assert_eq!(before, after, "directory SQL must not be wiped after a failed hydration");
    // The mutation was not persisted either.
    assert_eq!(
        storage.get_configuration_document(CONFIGURATION_KEY).await.unwrap().as_deref(),
        Some(corrupt),
        "the corrupt document must not be replaced by an empty-state persist"
    );

    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}

/// T10 P3-2: `MigrationReport.imported` must distinguish import from adopt and
/// `already_migrated`; the latter also reports zero imported counts.
#[tokio::test]
async fn migration_report_distinguishes_import_adopt_and_already_migrated() {
    let raw = legacy_document().to_string();
    let storage = vks_storage::MemoryStorage::new();
    storage.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    let first = super::run_on_storage(&storage, Some(&raw)).await.expect("import");
    assert_eq!(first.outcome, "applied");
    assert!(first.imported, "fresh SQL rows are imported");
    assert_eq!(first.profiles, 1);

    let second = super::run_on_storage(&storage, Some(&raw)).await.expect("already migrated");
    assert_eq!(second.outcome, "already_migrated");
    assert!(!second.imported, "marker present means nothing was imported");
    assert_eq!(second.profiles, 0);
    assert_eq!(second.global_addresses, 0);

    // Marker lost but non-empty SQL rows: adopt, not import.
    let adopt = vks_storage::MemoryStorage::new();
    adopt.put_configuration_document(CONFIGURATION_KEY, &raw).await.unwrap();
    let fixture = super::parse_legacy_document(Some(&raw)).expect("parse").snapshot;
    adopt.replace_directory_snapshot(&fixture).await.unwrap();
    let report = super::run_on_storage(&adopt, Some(&raw)).await.expect("adopt");
    assert_eq!(report.outcome, "applied");
    assert!(!report.imported, "non-empty SQL rows win, the document is not imported");
}

/// T10 P3-1: personal canonical dedup must rewrite `users[].primary_address`
/// to the surviving id (otherwise the profile points at a removed row).
#[test]
fn personal_dedup_rewrites_primary_address() {
    let raw = json!({
        "resources": {
            "directory": {
                "users": {"u-1": {"display_name": "U", "primary_address": "p-b", "created_at": 1}},
                "user_addresses": {"u-1": {
                    "p-a": {"id": "p-a", "uri": "sip:user@example.com", "created_at": 10},
                    "p-b": {"id": "p-b", "uri": "sip:user@example.com:5060", "created_at": 20}
                }}
            }
        }
    })
    .to_string();
    let parsed = super::parse_legacy_document(Some(&raw)).expect("parse");
    let profile = parsed.snapshot.profiles.iter().find(|profile| profile.user_id == "u-1").unwrap();
    let profile_json: Value = serde_json::from_str(&profile.profile_json).unwrap();
    assert_eq!(profile_json["primary_address"], "p-a", "primary must follow the dedup keep-id");
}

/// T10 P2-4 (documented limitation, not a supported deployment): every commit
/// replaces the WHOLE directory snapshot from that process's RAM cache. Two
/// writers on one database are last-writer-wins — PG clustering / a second
/// instance is unsupported and must not be advertised. This test pins the
/// current single-node contract; a CAS/per-row redesign must update it.
#[tokio::test]
async fn two_writers_on_one_sqlite_are_last_writer_wins_single_node_only() {
    let (url, path) = temp_sqlite_url();
    let storage = sqlite_storage(&url).await;
    storage.put_configuration_document(CONFIGURATION_KEY, r#"{"version":1,"resources":{}}"#).await.unwrap();

    let (mut first, _) = test_state();
    Arc::get_mut(&mut first).unwrap().storage = Some(storage.clone());
    first.sync_from_storage().await.expect("first writer hydrate");
    let (mut second, _) = test_state();
    Arc::get_mut(&mut second).unwrap().storage = Some(storage.clone());
    second.sync_from_storage().await.expect("second writer hydrate");

    async fn write_address(state: &crate::state::SharedState, id: &str, uri: &str) {
        let mut resources = state.api_v3.resources.lock().await;
        resources.addresses.insert(
            id.to_string(),
            crate::api_v3::resources::AddressRecord {
                id: id.to_string(),
                uri: uri.to_string(),
                display_name: id.to_string(),
                created_at: 1,
                payload: json!({}),
                extra: Default::default(),
            },
        );
        crate::api_v3::resources::persist_with_directory(state, &resources).await.expect("persist");
    }

    write_address(&first, "addr-a", "sip:a@example.com").await;
    // The second node's cache is stale: its whole-snapshot write removes A's
    // row. This is exactly why T10 is single-writer only.
    write_address(&second, "addr-b", "sip:b@example.com").await;
    let snapshot = storage.load_directory_snapshot().await.unwrap();
    assert!(snapshot.addresses.iter().any(|address| address.id == "addr-b"), "last writer keeps its row");
    assert!(
        !snapshot.addresses.iter().any(|address| address.id == "addr-a"),
        "documented limitation: a second writer on one DB erases the first writer's rows"
    );

    storage.pool().close().await;
    let _ = std::fs::remove_file(path);
}
