//! Тесты истории конференций (T2.2): запись при остановке/удалении, чтение
//! vendor-эндпоинтов, CSV/ZIP-экспорт, clean и персистентность между рестартами.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
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
            connected_at: Some(1_800_000_000),
            stopped_at: None,
            stop_reason: None,
            incoming: false,
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
    assert_eq!(session["abonents"], 1);
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
    assert_eq!(calls["call_strings"][0]["contact"]["contact_type"], "call_strings");
    assert_eq!(detail["session"]["started_by"]["id"], "caller");

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

    // Остановка реально запущенной сессии → Manual + vendor-идентификаторы.
    let created = json_body(
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "Стоп"}))).await,
    )
    .await;
    let room_id = created["conference"]["id"].as_str().unwrap().to_string();
    let started =
        send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id": room_id}))).await;
    assert_eq!(started.status(), StatusCode::OK);
    let response = send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["sessions"][0]["display_name"], "Стоп");
    assert_eq!(body["sessions"][0]["stop_reason"], "Manual");
    assert!(body["sessions"][0]["dtmf"]
        .as_str()
        .is_some_and(|dtmf| dtmf.len() == 9 && dtmf.chars().all(|c| c.is_ascii_digit())));

    // Повторная остановка не создаёт дубль (room_id+started_ms).
    let _ = send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await;
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1, "dedupe по room_id+started_ms");

    // Конференция, которую никогда не запускали, в историю не попадает.
    let never = json_body(
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "НеЗапуск"}))).await,
    )
    .await;
    let never_id = never["conference"]["id"].as_str().unwrap().to_string();
    let response =
        send(&app, request("DELETE", &format!("/api/v3/conferences/{never_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1, "never-started delete не пишет историю");

    // Удаление активной (запущенной) сессии пишет reason=Deleted.
    let created = json_body(
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "Удал"}))).await,
    )
    .await;
    let active_id = created["conference"]["id"].as_str().unwrap().to_string();
    let started =
        send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id": active_id}))).await;
    assert_eq!(started.status(), StatusCode::OK);

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
async fn quick_session_gets_identity_and_history_entry() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let quick = json_body(
        send(&app, request("POST", "/api/v3/sessions/quick", Some(&token), json!({"display_name": "Quick T22"}))).await,
    )
    .await;
    let room_id = quick["session"]["id"].as_str().unwrap().to_string();
    let dtmf = quick["session"]["primary_dtmf"]["name"].as_str().unwrap_or_default().to_string();
    assert_eq!(dtmf.len(), 9, "{quick}");
    let route = quick["session"]["primary_route"]["name"].as_str().unwrap_or_default();
    assert_eq!(route, "Quick T22");

    let stopped = send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await;
    assert_eq!(stopped.status(), StatusCode::NO_CONTENT);
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["sessions"][0]["dtmf"], dtmf);
    assert_eq!(body["sessions"][0]["route"], "Quick T22");
    assert_eq!(body["sessions"][0]["creator"]["contact_type"], "users");
    assert_ne!(body["sessions"][0]["creator"]["id"], "");
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

/// Никогда не запускавшаяся конференция: DELETE /sessions — идемпотентный
/// 204 без фантомной записи; повторная остановка живой сессии не дублирует.
#[tokio::test]
async fn stopping_never_started_and_repeated_stop_writes_no_phantom_history() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let created = json_body(
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "НеЗапускСтоп"}))).await,
    )
    .await;
    let room_id = created["conference"]["id"].as_str().unwrap().to_string();
    let response = send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 0, "stop без старта не пишет историю");

    let started =
        send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id": room_id}))).await;
    assert_eq!(started.status(), StatusCode::OK);
    assert_eq!(
        send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await.status(),
        StatusCode::NO_CONTENT
    );
    let body = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1, "повторная остановка не дублирует: {body}");
}

/// Реальный подключённый участник: vendor-форма abonents (H) и contact вызова,
/// иначе UI классифицирует вызов как MISSED.
#[tokio::test]
async fn history_call_snapshot_has_vendor_abonent_shape() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let room = Rooms::create_async(&state, "С участником").await.unwrap();
    seed_attempt(&state, &room.id);
    state.room_hub.register_participant(
        &room.id,
        crate::hub::ParticipantSnapshot {
            participant_id: "sip-1@192.168.1.132".into(),
            display_name: "Polycom".into(),
            is_recorder: false,
            is_speaking: false,
            track_ids: vec![],
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: 1_800_000_100,
        },
    );
    record_stopped(&state, &room, "caller", "Manual").await;
    let body = json_body(
        send(&app, request("GET", "/api/v3/call_history?sort_field=started_at", Some(&token), json!({}))).await,
    )
    .await;
    let history_id = body["sessions"][0]["id"].as_str().unwrap();
    let detail = json_body(
        send(&app, request("GET", &format!("/api/v3/call_history/{history_id}"), Some(&token), json!({}))).await,
    )
    .await;
    let call = &detail["session"]["call_strings"][0];
    assert_eq!(call["contact"]["contact_type"], "call_strings");
    assert_eq!(call["invitations"][0]["call_string_id"], "call-1");
    let abonent = &call["abonents"][0];
    assert_eq!(abonent["abonent_id"], "sip-1@192.168.1.132");
    assert_eq!(abonent["display_name"], "Polycom");
    assert_eq!(abonent["contact"]["contact_type"], "call_strings");
    assert_eq!(abonent["connection"]["type_id"], "OUTGOING");
    assert_eq!(abonent["connection"]["connection_url"], "sip:192.168.1.132");
    assert_eq!(abonent["started_at"], 1_800_000_100);
    assert_eq!(abonent["stop_reason"], "Ended By Remote");
}

/// CSV даты — в таймзоне администратора (как UI), не в UTC.
#[tokio::test]
async fn export_csv_uses_configured_timezone() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    {
        let mut resources = state.api_v3.resources.lock().await;
        resources.admin_settings.insert("timezone".into(), json!({"timezone": "Europe/Moscow"}));
    }
    let room = Rooms::create_async(&state, "Таймзона").await.unwrap();
    record_stopped(&state, &room, "caller", "Manual").await;
    let export =
        json_body(send(&app, request("GET", "/api/v3/call_history/export", Some(&token), json!({}))).await).await;
    let filename = export["filename"].as_str().unwrap().to_string();
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
    let bytes = axum::body::to_bytes(download.into_body(), usize::MAX).await.unwrap();
    // ZIP store: CSV лежит после local header; декодируем данные без внешних крейтов.
    let name_len = u16::from_le_bytes(bytes[26..28].try_into().unwrap()) as usize;
    let extra_len = u16::from_le_bytes(bytes[28..30].try_into().unwrap()) as usize;
    let data_len = u32::from_le_bytes(bytes[18..22].try_into().unwrap()) as usize;
    let start = 30 + name_len + extra_len;
    let csv = String::from_utf8(bytes[start..start + data_len].to_vec()).unwrap();
    let expected = {
        use chrono::TimeZone;
        chrono_tz::Europe::Moscow
            .timestamp_opt(room.created_ms as i64 / 1000, 0)
            .single()
            .unwrap()
            .format("%d.%m.%Y %H:%M:%S")
            .to_string()
    };
    assert!(csv.contains(&expected), "CSV должен быть в Europe/Moscow (+3): {csv}");
}

/// Дата-фильтр vendor-UI (`date_from`/`date_to`, секунды) реально сужает
/// список и экспорт; clean инвалидирует ранее выданные архивы.
#[tokio::test]
async fn date_filter_and_clean_invalidate_exports() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let room = Rooms::create_async(&state, "Дата-фильтр").await.unwrap();
    record_stopped(&state, &room, "caller", "Manual").await;

    let future = (crate::state::now_unix_ms() / 1000 + 86_400) as i64;
    let body = json_body(
        send(&app, request("GET", &format!("/api/v3/call_history?date_from={future}"), Some(&token), json!({}))).await,
    )
    .await;
    assert_eq!(body["count"], 0, "{body}");
    let body = json_body(
        send(&app, request("GET", "/api/v3/call_history?date_from=1&date_to=4102444800", Some(&token), json!({})))
            .await,
    )
    .await;
    assert_eq!(body["count"], 1);
    let export = json_body(
        send(&app, request("GET", "/api/v3/call_history/export?date_from=4102444800", Some(&token), json!({}))).await,
    )
    .await;
    let filename = export["filename"].as_str().unwrap().to_string();
    assert!(filename.ends_with(".zip"));
    assert!(filename.contains("Conference_history_"));
    // Имя содержит случайный токен (Uuid без дефисов) — не угадывается.
    let token_part = filename.trim_end_matches(".zip").rsplit('_').next().unwrap();
    assert_eq!(token_part.len(), 32, "{filename}");

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

    let clean = send(&app, request("POST", "/api/v3/call_history/clean", Some(&token), json!({}))).await;
    assert_eq!(clean.status(), StatusCode::OK);
    let missing = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/static/call_history/{filename}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND, "clean должен инвалидировать старые выгрузки");
}

#[tokio::test]
async fn connected_call_that_hung_up_before_stop_keeps_vendor_abonent() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let room = Rooms::create_async(&state, "Позвонили и положили").await.unwrap();
    state.api_v3.conference_settings.lock().unwrap().insert(room.id, json!({"creator_id": "caller"}));
    let record = CallStringRecord {
        id: "cs-live".into(),
        uri: "sip:192.168.1.132".into(),
        display_name: "Polycom".into(),
        created_at: 1_700_000_000,
        payload: json!({"volume_rx": -60}),
    };
    state.api_v3.call_attempts.lock().unwrap().insert(
        (room.id, "sip:192.168.1.132".to_string()),
        CallAttempt {
            record,
            generation: uuid::Uuid::new_v4(),
            participant_id: Some("sip-1@192.168.1.132".into()),
            failure_reason: None,
            completed_at: None,
            abort_handle: None,
            connected_at: Some(1_700_000_100),
            stopped_at: None,
            stop_reason: None,
            incoming: false,
        },
    );
    // SIP BYE: участник ушёл до остановки сессии, CallAttempt остаётся.
    crate::api_v3::call_strings::note_participant_left(&state, &room.id, "sip-1@192.168.1.132", "Ended By Remote");
    record_stopped(&state, &room, "caller", "Manual").await;

    let list = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(list["count"], 1, "{list}");
    assert_eq!(list["sessions"][0]["abonents"], 1);

    let id = list["sessions"][0]["id"].as_str().unwrap().to_string();
    let detail =
        json_body(send(&app, request("GET", &format!("/api/v3/call_history/{id}"), Some(&token), json!({}))).await)
            .await;
    let abonent = &detail["session"]["call_strings"][0]["abonents"][0];
    assert_eq!(abonent["abonent_id"], "sip-1@192.168.1.132");
    assert_eq!(abonent["connection"]["type_id"], "OUTGOING");
    assert!(abonent["started_at"].as_i64().unwrap() >= 1_700_000_000);
    assert!(abonent["stopped_at"].as_i64().unwrap() >= 1_700_000_100);
    assert_eq!(abonent["stop_reason"], "Ended By Remote");
    assert!(detail["session"]["call_strings"][0]["invitations"][0]["call_string_id"].is_string());
    let _ = issuer;
}

#[tokio::test]
async fn clean_is_durable_and_delete_does_not_resurrect_history() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let room = Rooms::create_async(&state, "После очистки").await.unwrap();
    state
        .api_v3
        .conference_settings
        .lock()
        .unwrap()
        .insert(room.id, json!({"creator_id": "caller", "started_at": 1_800_000_000_i64}));
    record_stopped(&state, &room, "caller", "Manual").await;
    let before = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(before["count"], 1);

    let cleaned = send(&app, request("POST", "/api/v3/call_history/clean", Some(&token), json!({}))).await;
    assert_eq!(cleaned.status(), StatusCode::OK);
    let after_clean =
        json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(after_clean["count"], 0, "{after_clean}");

    let deleted =
        send(&app, request("DELETE", &format!("/api/v3/conferences/{}", room.id), Some(&token), json!({}))).await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let after_delete =
        json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(after_delete["count"], 0, "clean must be durable: {after_delete}");
}

#[tokio::test]
async fn incoming_call_is_recorded_as_incoming_connection() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let room = Rooms::create_async(&state, "Входящий").await.unwrap();
    crate::api_v3::call_strings::record_incoming_call(&state, room.id, "sip-in-1", "sip:192.168.1.50", "Входящий SIP");
    record_stopped(&state, &room, "caller", "Manual").await;
    let list = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    let id = list["sessions"][0]["id"].as_str().unwrap().to_string();
    let detail =
        json_body(send(&app, request("GET", &format!("/api/v3/call_history/{id}"), Some(&token), json!({}))).await)
            .await;
    let call = &detail["session"]["call_strings"][0];
    assert_eq!(call["abonents"][0]["connection"]["type_id"], "INCOMING");
    assert_eq!(call["abonents"][0]["abonent_id"], "sip-in-1");
    // Ключ входящего вызова содержит `#{call_string_id}` только внутри карты
    // попыток; наружу (connection_url/contact/invitation) обязан идти чистый URI.
    assert_eq!(call["connection_url"], "sip:192.168.1.50", "{call}");
    assert_eq!(call["contact"]["address"], "sip:192.168.1.50", "{call}");
    assert_eq!(call["abonents"][0]["connection"]["connection_url"], "sip:192.168.1.50");
    assert_eq!(call["invitations"][0]["connection_url"], "sip:192.168.1.50");
}

#[tokio::test]
async fn repeated_start_after_stop_records_the_second_session() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let created = json_body(
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name": "Перезапуск"}))).await,
    )
    .await;
    let room_id = created["conference"]["id"].as_str().unwrap().to_string();

    // Сессия №1: старт → стоп (Manual) → durable-маркер истории остаётся.
    assert_eq!(
        send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id": room_id}))).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&app, request("DELETE", &format!("/api/v3/sessions/{room_id}"), Some(&token), json!({}))).await.status(),
        StatusCode::NO_CONTENT
    );

    // Сессия №2: повторный старт и удаление активной конференции должны дать
    // отдельную запись, а не потеряться из-за маркера прошлой сессии.
    assert_eq!(
        send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id": room_id}))).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&app, request("DELETE", &format!("/api/v3/conferences/{room_id}"), Some(&token), json!({})))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );

    let body = json_body(
        send(&app, request("GET", "/api/v3/call_history?sort_field=started_at&sort_order=1", Some(&token), json!({})))
            .await,
    )
    .await;
    assert_eq!(body["count"], 2, "повторный запуск — новая сессия истории: {body}");
    assert_eq!(body["sessions"][0]["stop_reason"], "Deleted");
    assert_eq!(body["sessions"][1]["stop_reason"], "Manual");
}

#[tokio::test]
async fn history_default_order_is_newest_first_and_export_honours_sort_order() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    {
        let mut resources = state.api_v3.resources.lock().await;
        resources.history.sessions.push(crate::api_v3::history::HistorySession {
            id: "old-session".into(),
            room_id: "old-room".into(),
            display_name: "OldSession".into(),
            started_at: 1_000,
            started_ms: 1_000_000,
            stopped_at: 1_100,
            stop_reason: "Manual".into(),
            abonents_count: 0,
            creator_id: String::new(),
            stopped_by: String::new(),
            calls: Vec::new(),
            dtmf: None,
            route: None,
            payload: json!({}),
        });
        resources.history.sessions.push(crate::api_v3::history::HistorySession {
            id: "new-session".into(),
            room_id: "new-room".into(),
            display_name: "NewSession".into(),
            started_at: 2_000,
            started_ms: 2_000_000,
            stopped_at: 2_100,
            stop_reason: "Manual".into(),
            abonents_count: 0,
            creator_id: String::new(),
            stopped_by: String::new(),
            calls: Vec::new(),
            dtmf: None,
            route: None,
            payload: json!({}),
        });
    }

    // Bare-список: vendor-эталон newest-first даже без sort_order.
    let list = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(list["sessions"][0]["id"], "new-session", "{list}");

    // Экспорт по умолчанию наследует порядок списка (newest-first) и уважает
    // явный sort_order=0 (oldest-first) — CSV внутри ZIP не сжат.
    for (query, first, second) in [("", "NewSession", "OldSession"), ("?sort_order=0", "OldSession", "NewSession")] {
        let export = json_body(
            send(&app, request("GET", &format!("/api/v3/call_history/export{query}"), Some(&token), json!({}))).await,
        )
        .await;
        let filename = export["filename"].as_str().unwrap().to_string();
        let download = app
            .clone()
            .oneshot(Request::builder().uri(format!("/static/call_history/{filename}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(download.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(download.into_body(), usize::MAX).await.unwrap();
        let csv = String::from_utf8_lossy(&bytes);
        let first_pos = csv.find(first).unwrap_or_else(|| panic!("{first} отсутствует в CSV"));
        let second_pos = csv.find(second).unwrap_or_else(|| panic!("{second} отсутствует в CSV"));
        assert!(first_pos < second_pos, "порядок экспорта неверен для {query}: {csv}");
    }
}

#[tokio::test]
async fn static_export_rejects_unknown_and_traversal_and_clean_requires_operator() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let _token = login_token(&app, "admin", "s3cret").await;
    let missing = app
        .clone()
        .oneshot(Request::builder().uri("/static/call_history/nope.zip").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let traversal = app
        .clone()
        .oneshot(Request::builder().uri("/static/call_history/..%2Fsecret.zip").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);

    let viewer = Users::create(&state, "t22-viewer", Role::Viewer);
    let viewer_token = issuer.issue(&viewer.id.to_string(), None).unwrap();
    let clean = send(&app, request("POST", "/api/v3/call_history/clean", Some(&viewer_token), json!({}))).await;
    assert_eq!(clean.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn export_honours_date_filter_and_null_stopper() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let early = Rooms::create_async(&state, "EarlyExport").await.unwrap();
    let late = Rooms::create_async(&state, "LateExport").await.unwrap();
    record_stopped(&state, &early, "", "Manual").await;
    // Сдвигаем раннюю сессию в прошлое, чтобы фильтр её отсекал (история в памяти).
    {
        let mut resources = state.api_v3.resources.lock().await;
        if let Some(session) = resources.history.sessions.iter_mut().find(|s| s.room_id == early.id.to_string()) {
            session.started_at = 1_000_000;
            session.stopped_at = 1_000_100;
        }
    }
    record_stopped(&state, &late, "", "Manual").await;
    let detail = json_body(send(&app, request("GET", "/api/v3/call_history", Some(&token), json!({}))).await).await;
    assert_eq!(detail["count"], 2, "{detail}");
    let null_stopper =
        detail["sessions"].as_array().unwrap().iter().find(|session| session["display_name"] == "LateExport").unwrap();
    assert!(null_stopper["stopped_by"].is_null(), "{null_stopper}");

    let export = json_body(
        send(&app, request("GET", "/api/v3/call_history/export?date_from=1500000000", Some(&token), json!({}))).await,
    )
    .await;
    let filename = export["filename"].as_str().unwrap().to_string();
    let download = app
        .clone()
        .oneshot(Request::builder().uri(format!("/static/call_history/{filename}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(download.into_body(), usize::MAX).await.unwrap();
    assert!(
        bytes.windows(b"LateExport".len()).any(|window| window == b"LateExport"),
        "filtered export keeps late session"
    );
    assert!(
        !bytes.windows(b"EarlyExport".len()).any(|window| window == b"EarlyExport"),
        "filtered export must not include early session"
    );
}
