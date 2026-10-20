//! Тесты истории конференций (T2.2): запись при остановке/удалении, чтение
//! vendor-эндпоинтов, CSV/ZIP-экспорт, clean и персистентность между рестартами.

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;
use vks_core::{Role, RoomId};

use crate::api_v3::call_strings::{CallAttempt, CallStringRecord};
use crate::api_v3::history::record_stopped;
use crate::api_v3::tests::{app, json_body, login_token, request, send, test_state};
use crate::rooms::Rooms;
use crate::users::Users;

fn seed_attempt(state: &crate::state::AppState, room: &RoomId) {
    let record = CallStringRecord {
        id: "call-1".into(),
        uri: "sip:192.168.1.132".into(),
        display_name: "Polycom".into(),
        created_at: 1,
        payload: json!({"sip": {"audio_codecs": [{"name": "G722"}]}}),
    };
    state.api_v3.call_attempts.lock().unwrap().insert(
        (*room, "sip:192.168.1.132".to_string()),
        CallAttempt {
            record,
            generation: uuid::Uuid::new_v4(),
            participant_id: Some("sip-1@192.168.1.132".into()),
            failure_reason: Some("Ended By Remote".into()),
            completed_at: Some(1_800_000_000),
            abort_handle: None,
        },
    );
}

#[tokio::test]
async fn history_endpoints_list_detail_export_and_clean() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let room = Rooms::create_async(&state, "Историческая").await.unwrap();
    state.api_v3.conference_settings.lock().unwrap().insert(room.id, json!({"creator_id": "caller"}));
    seed_attempt(&state, &room.id);
    record_stopped(&state, &room, "caller", "Manual").await;

    // список
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1, "{body}");
    let session = &body["sessions"][0];
    assert_eq!(session["display_name"], "Историческая");
    assert_eq!(session["stop_reason"], "Manual");
    assert_eq!(session["abonents"], 0);
    assert_eq!(session["creator"]["contact_type"], "users");
    assert_eq!(session["stopper"]["id"], "caller");
    assert!(session["payload"]["bitrate"].is_number());
    let history_id = session["id"].as_str().unwrap().to_string();

    // поиск/сортировка/пагинация не теряют запись
    let body = json_body(
        send(
            &app,
            request("GET", "/api/v3/call_history?display_name=историч&page=1&page_size=1", Some(&token), json!({})),
        )
        .await,
    )
    .await;
    assert_eq!(body["count"], 1);

    // детали + ui-шапка + ui-вызовы
    let detail = json_body(
        send(&app, request("GET", &format!("/api/v3/call_history/{history_id}"), Some(&token), json!({}))).await,
    )
    .await;
    assert_eq!(detail["session"]["call_strings"][0]["connection_url"], "sip:192.168.1.132");
    assert_eq!(detail["session"]["call_strings"][0]["invitations"][0]["finish_reason"], "Ended By Remote");
    let ui = json_body(
        send(&app, request("GET", &format!("/api/v3/ui/session_history/{history_id}"), Some(&token), json!({}))).await,
    )
    .await;
    assert_eq!(ui["session"]["display_name"], "Историческая");
    assert_eq!(ui["session"]["started_by"]["id"], "caller");
    let calls = json_body(
        send(&app, request("GET", &format!("/api/v3/ui/call_history/{history_id}"), Some(&token), json!({}))).await,
    )
    .await;
    assert_eq!(calls["count"], 1);
    assert_eq!(calls["call_strings"][0]["contact_type"], Value::Null);

    // экспорт: {filename}, ZIP по /static (без токена) валиден для python/unzip
    let export = json_body(
        send(&app, request("GET", "/api/v3/call_history/export?page_size=10", Some(&token), json!({}))).await,
    )
    .await;
    let filename = export["filename"].as_str().unwrap().to_string();
    assert!(filename.starts_with("Conference_history_") && filename.ends_with(".zip"));
    let download = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/static/call_history/{filename}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(download.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    let header = "Название".as_bytes();
    assert!(bytes.windows(header.len()).any(|w| w == header), "CSV внутри ZIP должен содержать заголовок «Название»");

    // clean: backup-ZIP + очистка
    let clean = send(&app, request("POST", "/api/v3/call_history/clean", Some(&token), json!({}))).await;
    assert_eq!(clean.status(), StatusCode::OK);
    let backup = json_body(clean).await;
    assert!(backup["filename"].as_str().unwrap().contains("backup"));
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 0);

    // права: Participant не читает историю
    let participant = Users::create(&state, "hist-viewer", Role::Participant);
    let participant_token = issuer.issue(&participant.id.to_string(), None).unwrap();
    let denied = send(&app, request("GET", "/api/v3/call_history", Some(&participant_token), json!({}))).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn stopping_and_deleting_sessions_writes_history() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    let created = json_body(
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "Стоп"}))).await,
    )
    .await;
    let room_id = created["conference"]["id"].as_str().unwrap().to_string();
    let response = send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["sessions"][0]["display_name"], "Стоп");
    assert_eq!(body["sessions"][0]["stop_reason"], "Manual");

    // повторная остановка не создаёт дубль
    let _ = send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await;
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1, "dedupe по room_id+started_at");

    // удаление активной сессии пишет reason=Deleted
    let created = json_body(
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "Удал"}))).await,
    )
    .await;
    let active_id = created["conference"]["id"].as_str().unwrap().to_string();

    let response =
        send(&app, request("DELETE", &format!("/api/v3/conferences/{active_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let body = json_body(
        send(&app, request("GET", "/api/v3/call_history?sort_field=started_at&sort_order=1", Some(&token), json!({})))
            .await,
    )
    .await;
    assert_eq!(body["count"], 2);
    assert_eq!(body["sessions"][0]["display_name"], "Удал");
    assert_eq!(body["sessions"][0]["stop_reason"], "Deleted");
}

#[tokio::test]
async fn history_survives_restart_from_configuration_document() {
    let storage = std::sync::Arc::new(vks_storage::MemoryStorage::new());
    let mut state = crate::state::AppState::new();
    state.storage = Some(storage.clone());
    let state = std::sync::Arc::new(state);
    let room = Rooms::create_async(state.as_ref(), "До рестарта").await.unwrap();
    record_stopped(&state, &room, "caller", "Manual").await;
    assert_eq!(state.api_v3.resources.lock().await.history.sessions.len(), 1);

    let mut restarted = crate::state::AppState::new();
    restarted.storage = Some(storage);
    restarted.sync_from_storage().await.expect("hydrate");
    let resources = restarted.api_v3.resources.lock().await;
    assert_eq!(resources.history.sessions.len(), 1);
    assert_eq!(resources.history.sessions[0].display_name, "До рестарта");
}

/// T2.2: конференция получает vendor-идентификаторы (`primary_dtmf` 9 цифр и
/// `primary_route`), сессия записывает их в историю, а снимок вызовов попадает
/// в detail до очистки `call_attempts`.
#[tokio::test]
async fn stopped_session_records_dtmf_route_and_call_snapshot() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    let created =
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "T22 Identity"}))).await;
    assert_eq!(created.status(), StatusCode::OK, "{:?}", json_body(created).await);
    let created = json_body(created).await;
    let conference = &created["conference"];
    let room_id = conference["id"].as_str().unwrap().to_string();
    let dtmf = conference["primary_dtmf"]["name"].as_str().expect("primary_dtmf.name");
    assert_eq!(dtmf.len(), 9, "vendor-контракт: DTMF ровно 9 цифр");
    assert!(dtmf.chars().all(|c| c.is_ascii_digit()));
    assert_eq!(conference["primary_route"]["name"], "T22 Identity");

    let started =
        send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id": room_id}))).await;
    assert_eq!(started.status(), StatusCode::OK, "{:?}", json_body(started).await);

    seed_attempt(&state, &RoomId::from_uuid(uuid::Uuid::parse_str(&room_id).unwrap()));
    let stopped = send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await;
    assert_eq!(stopped.status(), StatusCode::NO_CONTENT, "{:?}", json_body(stopped).await);

    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1, "{body}");
    let session = &body["sessions"][0];
    assert_eq!(session["dtmf"], dtmf);
    assert_eq!(session["route"], "T22 Identity");
    let history_id = session["id"].as_str().unwrap().to_string();

    let detail = json_body(
        send(&app, request("GET", &format!("/api/v3/call_history/{history_id}"), Some(&token), json!({}))).await,
    )
    .await;
    assert_eq!(detail["session"]["call_strings"][0]["display_name"], "Polycom");

    // Quick-start-сессия тоже получает route/dtmf (иначе UI показывал «ID: -»).
    let quick = send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({}))).await;
    assert_eq!(quick.status(), StatusCode::OK, "{:?}", json_body(quick).await);
    let quick = json_body(quick).await;
    let quick_id = quick["session"]["id"].as_str().unwrap().to_string();
    let stopped = send(&app, request("DELETE", &format!("/api/v3/sessions/{quick_id}"), Some(&token), json!({}))).await;
    assert_eq!(stopped.status(), StatusCode::NO_CONTENT);
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    let quick_session = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["display_name"] == "Quick Session")
        .cloned()
        .expect("quick session записана в историю");
    assert_eq!(quick_session["dtmf"].as_str().map(str::len), Some(9));
    assert_eq!(quick_session["route"], "Quick Session");
}

/// Vendor-UI скачивает историю из строки списка через
/// `GET /api/v3/call_history/{id}/export` (не общий `/export`).
#[tokio::test]
async fn session_export_returns_downloadable_zip() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let room = Rooms::create_async(&state, "Сессия экспорта").await.unwrap();
    record_stopped(&state, &room, "caller", "Manual").await;
    let list = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    let history_id = list["sessions"][0]["id"].as_str().unwrap().to_string();

    let export =
        send(&app, request("GET", &format!("/api/v3/call_history/{history_id}/export"), Some(&token), json!({}))).await;
    assert_eq!(export.status(), StatusCode::OK, "{:?}", json_body(export).await);
    let export = json_body(export).await;
    let filename = export["filename"].as_str().unwrap();
    assert!(filename.starts_with("Conference_history_") && filename.ends_with(".zip"), "{filename}");

    let download = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/static/call_history/{filename}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let disposition = download
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(disposition, format!("attachment; filename=\"{filename}\""));
    let bytes = axum::body::to_bytes(download.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    assert!(bytes.windows("Сессия экспорта".len()).any(|w| w == "Сессия экспорта".as_bytes()));

    let missing = send(&app, request("GET", "/api/v3/call_history/unknown/export", Some(&token), json!({}))).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
