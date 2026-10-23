//! Тесты адаптера `/api/v3` (§4.5 плана): полный роутер через `tower::oneshot`
//! (exempt/токены/fail-closed/lockout/catch-all/sessions/conferences/инвалидация
//! парольной меткой) + WS e2e через `tokio-tungstenite` (реальный сервер на
//! эфемерном порту — проверка конвертов, лимитов и живости соединения).
//!
//! WS-тесты используют `into_make_service_with_connect_info`, как `main.rs`
//! (иначе `ClientIp` падает в fallback `"unknown"` — тоже допустимо, §4.3).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as WsMsg;
use tower::ServiceExt;

use vks_auth::jwt::{JwtConfig, JwtIssuer};
use vks_auth::local_db::LocalDb;
use vks_core::RoomId;

use super::router;
use crate::state::{AppState, SharedState};

struct RejectingDialer;

#[tokio::test]
async fn pnm_lifecycle_pushes_include_native_conference_and_session_envelopes() {
    let (state, _) = test_state();
    let mut events = state.admin_events.subscribe();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let created = send(
        &server,
        request("POST", "/api/v3/conferences", Some(&token), json!({"display_name":"WS envelope regression"})),
    )
    .await;
    let created = json_body(created).await;
    let id = created["conference"]["id"].as_str().unwrap();
    let started = send(&server, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id":id}))).await;
    assert!(started.status().is_success());
    let mut pushed = None;
    while let Ok(event) = events.try_recv() {
        if event.topic == "sessions:started" {
            pushed = Some(event.payload);
        }
    }
    let pushed = pushed.expect("Native start event");
    assert_eq!(pushed["conference"]["id"], id, "onStartConference destructures conference.id");
    assert_eq!(pushed["session"]["id"], id, "onStartConference destructures session.id");
    let stopped = send(&server, request("DELETE", &format!("/api/v3/sessions/{id}"), Some(&token), json!({}))).await;
    assert!(stopped.status().is_success());
    let mut pushed = None;
    while let Ok(event) = events.try_recv() {
        if event.topic == "sessions:stopped" {
            pushed = Some(event.payload);
        }
    }
    assert_eq!(pushed.unwrap()["session"]["id"], id, "Native stop callback destructures session.id");
}
#[async_trait::async_trait]
impl crate::state::OutboundDialer for RejectingDialer {
    async fn dial(&self, _: RoomId, _: &str, _: Option<&str>) -> Result<String, String> {
        Err("SIP 486 Busy Here".into())
    }
}

#[tokio::test]
async fn pnm_native_gain_and_direction_reach_media_bridge() {
    let (state, _) = test_state();
    let dialer = Arc::new(MockStep4Dialer::default());
    state.set_dialer(dialer.clone());
    let room = crate::rooms::Rooms::create_async(&state, "Controls regression").await.unwrap();
    let pid = "controls@sip-gateway";
    state.room_hub.register_participant(
        &room.id,
        crate::hub::ParticipantSnapshot {
            participant_id: pid.into(),
            display_name: "Terminal".into(),
            is_recorder: false,
            is_speaking: false,
            track_ids: vec!["audio".into()],
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: 1,
        },
    );
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(
        &server,
        request("POST", &format!("/api/v3/abonents/{pid}/volume"), Some(&token), json!({"volume":-60,"outgoing":true})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "The native slider sends signed tenths of a dB");
    let record = dialer.last_media_control.lock().unwrap().clone().unwrap();
    assert_eq!(record.3, None);
    assert_eq!(record.4, Some(50), "-6 dB MCU→terminal must set internal RX to 50%");
    let response = send(
        &server,
        request(
            "POST",
            &format!("/api/v3/abonents/{pid}/audio"),
            Some(&token),
            json!({"enable":false,"outgoing":false}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let record = dialer.last_media_control.lock().unwrap().clone().unwrap();
    assert_eq!(record.1, Some(true));
    assert_eq!(record.2, None, "Microphone mute must affect terminal→MCU only");
    let response =
        send(&server, request("GET", &format!("/api/v3/sessions/{}/abonents", room.id), Some(&token), json!({}))).await;
    let roster = json_body(response).await;
    assert_eq!(roster["connected"][0]["payload"]["volume_tx"], -60);
    assert_eq!(roster["connected"][0]["payload"]["audio_rx_enabled"], false);
    assert_eq!(roster["connected"][0]["payload"]["audio_tx_enabled"], true);
}

#[tokio::test]
async fn pnm_engine_uses_dialed_address_and_native_contact_payload() {
    let (state, _) = test_state();
    let room = crate::rooms::Rooms::create_async(&state, "Engine contact regression").await.unwrap();
    let pid = "engine-address@sip-gateway";
    state.room_hub.register_participant(
        &room.id,
        crate::hub::ParticipantSnapshot {
            participant_id: pid.into(),
            display_name: "Physical terminal label".into(),
            is_recorder: false,
            is_speaking: false,
            track_ids: vec!["audio".into()],
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: 1,
        },
    );
    state.api_v3.call_attempts.lock().unwrap().insert(
        (room.id, "sip:192.168.1.132".into()),
        super::call_strings::CallAttempt {
            record: super::call_strings::CallStringRecord {
                id: "saved-physical-contact".into(),
                uri: "sip:192.168.1.132".into(),
                display_name: "Physical terminal label".into(),
                created_at: 1,
                payload: super::call_strings::default_contact_payload(Some(&json!({"auto_call":true,"volume_rx":-60}))),
            },
            generation: uuid::Uuid::new_v4(),
            participant_id: Some(pid.into()),
            failure_reason: None,
            completed_at: None,
            abort_handle: None,
            connected_at: Some(1),
            stopped_at: None,
            stop_reason: None,
            incoming: false,
        },
    );
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let engine =
        json_body(send(&server, request("GET", "/api/v3/engine/abonents", Some(&token), json!({}))).await).await;
    let roster = json_body(
        send(&server, request("GET", &format!("/api/v3/sessions/{}/abonents", room.id), Some(&token), json!({}))).await,
    )
    .await;
    let abonent = &engine["abonents"][0];
    assert_eq!(abonent["uri"], "sip:192.168.1.132", "Native info must show the dialed address, not a display name");
    for field in ["uri", "connection_url", "contact_id", "call_string_id", "payload"] {
        assert_eq!(abonent[field], roster["connected"][0][field], "Engine and room DTO disagree on {field}");
    }
    assert_eq!(abonent["contact_id"], "saved-physical-contact");
    assert_eq!(abonent["details"]["audio"]["rx"]["volume"], -60);
}

#[tokio::test]
async fn pnm_failed_call_keeps_contact_and_reason() {
    let (state, _) = test_state();
    state.set_dialer(Arc::new(RejectingDialer));
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(
        &server,
        request("POST", "/api/v3/conferences", Some(&token), json!({"display_name":"Failure regression"})),
    )
    .await;
    let room = json_body(response).await;
    let id = room["conference"]["id"].as_str().unwrap();
    let response = send(&server, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id":id}))).await;
    assert!(response.status().is_success());
    let response = send(
        &server,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{id}/call_strings"),
            Some(&token),
            json!({"uri":"sip:192.168.1.132","display_name":"Real terminal"}),
        ),
    )
    .await;
    assert!(response.status().is_success());
    let contact = json_body(response).await["call_string"]["id"].clone();
    tokio::task::yield_now().await;
    let response =
        send(&server, request("GET", &format!("/api/v3/sessions/{id}/abonents"), Some(&token), json!({}))).await;
    let roster = json_body(response).await;
    assert_eq!(roster["connected"].as_array().unwrap().len(), 0);
    assert_eq!(roster["disconnected"][0]["contact_id"], contact, "Failed dial must not erase the participant row");
    assert_eq!(roster["disconnected"][0]["failure_reason"], "SIP 486 Busy Here");
}

#[tokio::test]
async fn pnm_template_contact_and_settings_survive_reload() {
    let storage = Arc::new(vks_storage::memory::MemoryStorage::new());
    let (mut state, _) = test_state();
    Arc::get_mut(&mut state).unwrap().storage = Some(storage.clone());
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/conferences",
            Some(&token),
            json!({
                "display_name":"Persistent template", "description":"Keep this description", "pin":"4321",
                "payload":{"mode":"CUSTOM","ui_autocall_on_session_start":"ON"}
            }),
        ),
    )
    .await;
    let room = json_body(response).await;
    let id = room["conference"]["id"].as_str().unwrap();
    let response = send(
        &server,
        request(
            "POST",
            &format!("/api/v3/ui/conferences/{id}/call_strings"),
            Some(&token),
            json!({
                "uri":"sip:192.168.1.132:5060", "display_name":"Physical terminal", "payload":{"auto_call":true}
            }),
        ),
    )
    .await;
    assert!(response.status().is_success());
    let expected = json_body(response).await["call_string"]["id"].clone();
    let response =
        send(&server, request("GET", &format!("/api/v3/conferences/{id}/contacts"), Some(&token), json!({}))).await;
    let contacts = json_body(response).await;
    assert_eq!(contacts["contacts"][0]["contact_id"], expected, "A successful Add address must retain the contact");
    let (mut restored, _) = test_state();
    Arc::get_mut(&mut restored).unwrap().storage = Some(storage);
    restored.sync_from_storage().await.unwrap();
    let server = app(restored);
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("GET", &format!("/api/v3/conferences/{id}/contacts"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["contacts"][0]["contact_id"], expected);
    let response = send(&server, request("GET", &format!("/api/v3/conferences/{id}"), Some(&token), json!({}))).await;
    let conference = json_body(response).await;
    assert_eq!(conference["conference"]["pin"], "4321");
    assert_eq!(conference["conference"]["description"], "Keep this description");
}

#[tokio::test]
async fn pnm_card_resources_roundtrip_regression() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;
    let response = send(
        &app,
        request(
            "POST",
            "/api/v3/conferences",
            Some(&token),
            json!({
                "display_name": "Card regression", "route": "card-regression", "dtmf": "98261020"
            }),
        ),
    )
    .await;
    assert!(response.status().is_success());
    let body = json_body(response).await;
    let id = body["conference"]["id"].as_str().unwrap();
    for (path, field) in [
        (format!("/api/v3/ui/conferences/{id}/video_layouts"), "video_layouts"),
        (format!("/api/v3/conferences/{id}/audiochannels"), "audiochannels"),
        (format!("/api/v3/schedules?object_id={id}&object_type=conferences"), "schedules"),
        ("/api/v3/video_layout_grids".to_string(), "grids"),
    ] {
        let response = send(&app, request("GET", &path, Some(&token), json!({}))).await;
        assert_eq!(response.status(), StatusCode::OK, "The original conference card requires {path}");
        assert!(json_body(response).await[field].is_array(), "{path} must return its collection");
    }
    let response = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts",
            Some(&token),
            json!({
                "display_name": null, "payload": {"mode":"auto", "preset":"preset_default_13", "type":"default"}
            }),
        ),
    )
    .await;
    assert!(response.status().is_success(), "Create-conference must be able to create its default layout");
    let layout = json_body(response).await["video_layout"].clone();
    let lid = layout["id"].as_str().unwrap();
    let response =
        send(&app, request("POST", &format!("/api/v3/conferences/{id}/video_layouts/{lid}"), Some(&token), json!({})))
            .await;
    assert!(response.status().is_success());
    let response =
        send(&app, request("GET", &format!("/api/v3/ui/conferences/{id}/video_layouts"), Some(&token), json!({})))
            .await;
    let layouts = json_body(response).await;
    // Конференция раскладок не сеет (UI создаёт их сам); созданная и привязанная — в списке.
    assert!(layouts["video_layouts"].as_array().unwrap().iter().any(|l| l["id"] == lid));
    let response = send(
        &app,
        request(
            "POST",
            "/api/v3/audiochannels",
            Some(&token),
            json!({
                "display_name":"For all participants", "payload":{"type":"default","vad":true,"attachmode":"BLACKLIST"}
            }),
        ),
    )
    .await;
    assert!(response.status().is_success());
    let audio = json_body(response).await["audiochannel"].clone();
    let aid = audio["id"].as_str().unwrap();
    let response =
        send(&app, request("POST", &format!("/api/v3/conferences/{id}/audiochannels/{aid}"), Some(&token), json!({})))
            .await;
    assert!(response.status().is_success());
    let response =
        send(&app, request("GET", &format!("/api/v3/conferences/{id}/audiochannels"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["audiochannels"][0]["id"], aid);
}

#[tokio::test]
async fn codec_settings_etag_304_and_concurrent_patch() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;

    let first = send(&server, request("GET", "/api/v3/settings/audio_codecs", Some(&token), json!({}))).await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers().get(header::ETAG).expect("ETag").to_str().unwrap().to_string();
    assert_eq!(first.headers().get(header::CACHE_CONTROL).unwrap(), "private, no-cache");

    let req = Request::builder()
        .method("GET")
        .uri("/api/v3/settings/audio_codecs")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    let second = send(&server, req).await;
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    assert!(second.headers().get(header::ETAG).is_some());
    for validator in [format!("W/{etag}"), format!("\"other\", {etag}"), "*".to_string()] {
        let req = Request::builder()
            .method("GET")
            .uri("/api/v3/settings/audio_codecs")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::IF_NONE_MATCH, &validator)
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&server, req).await.status(), StatusCode::NOT_MODIFIED, "{validator}");
    }

    let (audio, video) = tokio::join!(
        send(
            &server,
            request("PATCH", "/api/v3/settings/audio_codecs", Some(&token), json!({"audio_codecs":["OPUS"]}),),
        ),
        send(
            &server,
            request("PATCH", "/api/v3/settings/video_codecs", Some(&token), json!({"video_codecs":["H265"]}),),
        ),
    );
    assert!(audio.status().is_success(), "audio PATCH: {}", audio.status());
    assert!(video.status().is_success(), "video PATCH: {}", video.status());

    let current = send(&server, request("GET", "/api/v3/settings/audio_codecs", Some(&token), json!({}))).await;
    let current = json_body(current).await;
    assert!(current["audio_codecs"].as_array().unwrap().iter().any(|codec| codec["name"] == "OPUS"));
    let current_video = send(&server, request("GET", "/api/v3/settings/video_codecs", Some(&token), json!({}))).await;
    let current_video = json_body(current_video).await;
    assert!(current_video["video_codecs"].as_array().unwrap().iter().any(|codec| codec == "H265"));
}

#[tokio::test]
async fn codec_patch_returns_503_and_rolls_back_on_persist_failure() {
    let (state, _) = test_state();
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let before =
        json_body(send(&server, request("GET", "/api/v3/settings/audio_codecs", Some(&token), json!({}))).await).await;
    state.settings_persist_fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let patch = send(
        &server,
        request("PATCH", "/api/v3/settings/audio_codecs", Some(&token), json!({"audio_codecs":["G729"]})),
    )
    .await;
    assert_eq!(patch.status(), 503);
    state.settings_persist_fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let after =
        json_body(send(&server, request("GET", "/api/v3/settings/audio_codecs", Some(&token), json!({}))).await).await;
    assert_eq!(before, after, "persist failure must roll settings back");
}

#[tokio::test]
async fn codec_patch_persists_and_keeps_independent_global_mode() {
    let (state, _) = test_state();
    let mut inner = std::sync::Arc::try_unwrap(state).expect("single owner");
    inner.storage = Some(std::sync::Arc::new(vks_storage::MemoryStorage::new()));
    inner.mode.lock().expect("mode").global_mode = vks_core::RoomMode::Mcu;
    let state = std::sync::Arc::new(inner);
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let patch = send(
        &server,
        request("PATCH", "/api/v3/settings/audio_codecs", Some(&token), json!({"audio_codecs":["OPUS"]})),
    )
    .await;
    assert_eq!(patch.status(), 200);
    let stored =
        state.storage.as_ref().unwrap().get_settings().await.unwrap().expect("settings persisted into storage");
    assert!(stored.contains("OPUS"), "{stored}");
    assert_eq!(state.mode.lock().expect("mode").global_mode, vks_core::RoomMode::Mcu);
}

#[tokio::test]
async fn calls_patch_and_codec_patch_do_not_lose_each_other() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let (calls, codecs) = tokio::join!(
        send(
            &server,
            request(
                "PATCH",
                "/api/v3/settings/calls",
                Some(&token),
                json!({"limits":{"max_participants_per_room":77}}),
            ),
        ),
        send(
            &server,
            request("PATCH", "/api/v3/settings/audio_codecs", Some(&token), json!({"audio_codecs":["OPUS"]}),),
        ),
    );
    assert!(calls.status().is_success(), "calls: {}", calls.status());
    assert!(codecs.status().is_success(), "codecs: {}", codecs.status());
    let calls = json_body(send(&server, request("GET", "/api/v3/settings/calls", Some(&token), json!({}))).await).await;
    assert_eq!(calls["payload"]["limits"]["max_participants_per_room"], 77);
    assert!(calls["payload"]["codecs"]["audio"].as_array().unwrap().iter().any(|codec| codec == "OPUS"));
    let codecs =
        json_body(send(&server, request("GET", "/api/v3/settings/audio_codecs", Some(&token), json!({}))).await).await;
    assert!(codecs["audio_codecs"].as_array().unwrap().iter().any(|codec| codec["name"] == "OPUS"));
}

/// Состояние с issuer + LocalDb(`admin`/`s3cret`), как бутстрап `main.rs`.
pub(crate) fn test_state() -> (SharedState, Arc<JwtIssuer>) {
    let issuer = Arc::new(JwtIssuer::new(JwtConfig {
        secret: "test_secret_api_v3_phase2".to_string(),
        ttl_secs: 86_400,
        issuer: "vks-test".to_string(),
    }));
    let db = Arc::new(LocalDb::new());
    db.register("u-admin", "admin", "s3cret").expect("register admin");
    let mut state = AppState::new();
    state.jwt_issuer = Some(issuer.clone());
    state.local_db = Some(db);
    // Как main.rs после bind UDP-лупа: conference-адреса строятся из candidate IP.
    state.server_addr.set("192.168.1.92".to_string()).expect("server_addr set once");
    // UI-поиск пользователей (`ui:users:search`) читает admin-хранилище (§4.4);
    // bootstrap `main.rs` наполняет его тем же администратором.
    crate::users::Users::create(&state, "admin", vks_core::Role::Operator);
    (Arc::new(state), issuer)
}

pub(crate) fn app(state: SharedState) -> Router {
    router(state)
}

pub(crate) fn request(method: &str, uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_string())).expect("build request")
}

pub(crate) async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.expect("oneshot")
}

pub(crate) async fn json_body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

async fn login(app: &Router, user: &str, password: &str) -> Response {
    send(app, request("POST", "/api/v3/login", None, json!({ "login": user, "password": password }))).await
}

pub(crate) async fn login_token(app: &Router, user: &str, password: &str) -> String {
    let response = login(app, user, password).await;
    assert_eq!(response.status(), StatusCode::OK, "login must succeed");
    let body = json_body(response).await;
    body["token"].as_str().expect("token field").to_string()
}

#[tokio::test]
async fn exempt_endpoints_work_without_token() {
    let (state, _) = test_state();
    let app = app(state);

    for uri in ["/api/v3/login/status", "/api/v3/system/about", "/api/v3/about", "/api/v3/references"] {
        let response = send(&app, request("GET", uri, None, json!({}))).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri} must be exempt from tokens");
    }
}

#[tokio::test]
async fn protected_endpoints_require_valid_token() {
    let (state, issuer) = test_state();
    let app = app(state);

    let response = send(&app, request("GET", "/api/v3/profile", None, json!({}))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], 401, "UI читает error.description: {body}");

    let response = send(&app, request("GET", "/api/v3/profile", Some("garbage"), json!({}))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let token = issuer.issue("u-admin", None).expect("issue");
    let response = send(&app, request("GET", "/api/v3/profile", Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn fail_closed_returns_503_when_auth_not_configured() {
    // Состояние без issuer/localDb — как сервер, стартовавший с пустым секретом.
    let state: SharedState = Arc::new(AppState::new());
    let app = app(state);

    // Включая exempt-пути (login) — весь /api/v3 закрыт.
    let response = login(&app, "admin", "s3cret").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], 503);

    let response = send(&app, request("GET", "/api/v3/system/about", None, json!({}))).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn login_success_failure_and_lockout() {
    let (state, _) = test_state();
    let app = app(state);

    // Успех.
    let token = login_token(&app, "admin", "s3cret").await;
    assert!(!token.is_empty());

    // Неверный пароль → 401 Login rejected.
    let response = login(&app, "admin", "nope").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(response).await;
    assert_eq!(body["error"]["description"], "Login rejected");

    // 4 добивающих неудачи → блок на 5-й; следующий запрос — 401 + Retry-After.
    for _ in 0..3 {
        let _ = login(&app, "admin", "nope").await;
    }
    let response = login(&app, "admin", "nope").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers().get(header::RETRY_AFTER).is_some(),
        "5-я неудача ставит блок и отвечает Retry-After (§1.4)"
    );

    // Даже правильный пароль теперь заблокирован.
    let response = login(&app, "admin", "s3cret").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(header::RETRY_AFTER).is_some());
}

#[tokio::test]
async fn password_change_mark_invalidates_old_tokens() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let iat = issuer.verify(&token).expect("verify").iat;

    // Метка смены пароля «в будущем» — токен инвалидирован (iat < mark).
    state.local_db.as_ref().expect("local_db").record_password_change_at("u-admin", iat + 2);
    let response = send(&app, request("GET", "/api/v3/profile", Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "токен до смены пароля отозван");

    // Граница: iat == mark — токен ещё валиден (инвалидация строго iat < mark).
    state.local_db.as_ref().expect("local_db").record_password_change_at("u-admin", iat);
    let response = send(&app, request("GET", "/api/v3/profile", Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn catch_all_returns_501_envelope() {
    let (state, _) = test_state();
    let app = app(state);

    for (method, uri) in
        [("GET", "/api/v3/unknown/deep"), ("POST", "/api/v3/unknown"), ("GET", "/api/v3/"), ("GET", "/api/v3")]
    {
        let response = send(&app, request(method, uri, None, json!({}))).await;
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED, "{method} {uri}");
        let body = json_body(response).await;
        assert_eq!(body["error"]["code"], 501, "{method} {uri}: {body}");
        assert_eq!(body["error"]["description"], "Not implemented");
    }
}

#[tokio::test]
async fn conferences_and_sessions_flow() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;

    // Пустой список — дашборд после логина не должен видеть ошибку.
    let body = json_body(send(&app, request("GET", "/api/v3/ui/sessions", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 0);
    assert_eq!(body["sessions"].as_array().map(Vec::len), Some(0));

    // Создание.
    let response =
        send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({ "display_name": "Зал А" }))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let id = body["conference"]["id"].as_str().expect("conference id").to_string();
    assert_eq!(body["conference"]["display_name"], "Зал А");

    // Карточка списка читает счётчики, маппер раскладок — `abonents.abonents`;
    // одна форма обязана содержать оба набора полей.
    for key in ["sip", "h323", "rtsp", "vnc", "media", "file"] {
        assert!(body["conference"]["abonents"][key].is_number(), "abonents.{key}: {}", body["conference"]["abonents"]);
    }
    assert!(body["conference"]["abonents"]["abonents"].is_array());
    for key in ["users", "sip", "h323", "rtsp", "vnc", "media", "file"] {
        assert!(body["conference"]["contacts"][key].is_number(), "contacts.{key}: {}", body["conference"]);
    }
    assert_eq!(body["conference"]["payload"]["connections_limit"], 50, "лимит из настроек");
    assert!(
        body["conference"]["started_at"].as_u64().is_some_and(|v| v > 0)
            && body["conference"]["started_at"].as_u64().is_some_and(|v| v < 10_000_000_000),
        "started_at — unix-секунды (карточки умножают на 1e3): {}",
        body["conference"]["started_at"]
    );

    // Список и чтение.
    let body = json_body(send(&app, request("GET", "/api/v3/ui/sessions", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["sessions"][0]["id"], id);
    assert!(body["sessions"][0]["abonents"]["sip"].is_number());
    assert!(body["sessions"][0]["contacts"]["users"].is_number());

    let response = send(&app, request("GET", &format!("/api/v3/ui/sessions/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);

    let missing = uuid::Uuid::new_v4();
    let response = send(&app, request("GET", &format!("/api/v3/ui/sessions/{missing}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json_body(response).await;
    assert_eq!(body["error"]["code"], 404);

    // Поиск по имени фильтрует.
    let body =
        json_body(send(&app, request("GET", "/api/v3/ui/sessions?search=зал", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 1);
    let body =
        json_body(send(&app, request("GET", "/api/v3/ui/sessions?search=nope", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 0);

    // Удаление.
    let response = send(&app, request("DELETE", &format!("/api/v3/conferences/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let body = json_body(send(&app, request("GET", "/api/v3/conferences", Some(&token), json!({}))).await).await;
    assert_eq!(body["count"], 0);
}

#[tokio::test]
async fn sessions_sort_by_time_and_state_filters() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;

    // Две комнаты с разным created_ms (порядок создания известен).
    for name in ["Bravo", "Alpha"] {
        let response =
            send(&app, request("POST", "/api/v3/conferences", Some(&token), json!({ "display_name": name }))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let created = json_body(response).await;
        let response = send(
            &app,
            request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id":created["conference"]["id"]})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Default (display_name asc): Alpha раньше Bravo.
    let body = json_body(send(&app, request("GET", "/api/v3/ui/sessions", Some(&token), json!({}))).await).await;
    assert_eq!(body["sessions"][0]["display_name"], "Alpha");
    assert_eq!(body["sessions"][1]["display_name"], "Bravo");

    // `sort_field=started_at` (шлёт UI: «By start time») — по времени создания.
    let body = json_body(
        send(&app, request("GET", "/api/v3/ui/sessions?sort_field=started_at&sort_order=0", Some(&token), json!({})))
            .await,
    )
    .await;
    assert_eq!(body["sessions"][0]["display_name"], "Bravo", "started_at asc: {body}");
    let body = json_body(
        send(&app, request("GET", "/api/v3/ui/sessions?sort_field=started_at&sort_order=1", Some(&token), json!({})))
            .await,
    )
    .await;
    assert_eq!(body["sessions"][0]["display_name"], "Alpha", "started_at desc: {body}");

    // Ненулевой sort_order разворачивает и остальные сортировки (Z-A).
    let body =
        json_body(send(&app, request("GET", "/api/v3/ui/sessions?sort_order=1", Some(&token), json!({}))).await).await;
    assert_eq!(body["sessions"][0]["display_name"], "Bravo", "display_name desc: {body}");

    // Экран конференций по умолчанию шлёт `state=inactive` — у нас inactive-комнат нет.
    let body = json_body(
        send(
            &app,
            request(
                "GET",
                "/api/v3/ui/conferences?page=1&page_size=100&sort_field=display_name&sort_order=0&state=inactive&exclude_tags=Immediate",
                Some(&token),
                json!({}),
            ),
        )
        .await,
    )
    .await;
    assert_eq!(body["count"], 0, "inactive → пусто: {body}");
    let body =
        json_body(send(&app, request("GET", "/api/v3/ui/conferences?state=active", Some(&token), json!({}))).await)
            .await;
    assert_eq!(body["count"], 2, "active → все комнаты: {body}");

    // `has_stopped=true` (stopped-сессии) — тоже пусто; false — все.
    let body =
        json_body(send(&app, request("GET", "/api/v3/ui/sessions?has_stopped=true", Some(&token), json!({}))).await)
            .await;
    assert_eq!(body["count"], 0, "stopped → пусто: {body}");
    let body =
        json_body(send(&app, request("GET", "/api/v3/ui/sessions?has_stopped=false", Some(&token), json!({}))).await)
            .await;
    assert_eq!(body["count"], 2);
}

#[tokio::test]
async fn shell_security_settings_and_credentials_expire_match_ui_contract() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;

    // `GET /api/v3/settings/security`: маппер `Ane` читает `data.payload`
    // (без него `settings.password` → TypeError на boot-цепочке).
    let response = send(&app, request("GET", "/api/v3/settings/security", Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let payload = &body["payload"];
    assert!(payload.is_object(), "payload обязателен: {body}");
    // Vendor-дефолты ref-92/t23 (настройки редактируются через PATCH).
    assert_eq!(payload["password_ttl"], 90);
    assert_eq!(payload["password_ttl_enabled"], false);
    assert_eq!(payload["login_max_attempts_count"], 5);
    assert_eq!(payload["login_block_timeout"], 60);
    assert_eq!(payload["login_block_enabled"], false);
    for key in [
        "password_min_length",
        "password_min_lowercase",
        "password_min_uppercase",
        "password_min_digits",
        "password_min_special_characters",
    ] {
        assert!(payload[key].is_number(), "payload.{key}: {body}");
    }

    // `GET /api/v3/users/credentials/expire`: форма запроса UI дашборда;
    // маппер `MW` деструктурирует `users` → без него store re-throw.
    let response = send(&app, request("GET", "/api/v3/users/credentials/expire", None, json!({}))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "endpoint за token-middleware");
    let response = send(
        &app,
        request(
            "GET",
            "/api/v3/users/credentials/expire?expire_in=3&sort_order=0&sort_field=expire_at",
            Some(&token),
            json!({}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["users"].as_array().map(Vec::len), Some(0), "users обязателен как массив: {body}");
}

// --- WS e2e (tokio-tungstenite) -------------------------------------------------

async fn spawn_ws_server(state: SharedState) -> SocketAddr {
    let app = app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.expect("serve");
    });
    addr
}

async fn ws_next_json<S>(read: &mut S) -> Value
where
    S: futures_util::Stream<Item = Result<WsMsg, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), read.next())
            .await
            .expect("ws read timeout")
            .expect("stream ended")
            .expect("ws error");
        match message {
            WsMsg::Text(text) => return serde_json::from_str(text.as_str()).expect("json frame"),
            WsMsg::Ping(_) | WsMsg::Pong(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn ws_send(write: &mut (impl SinkExt<WsMsg> + Unpin), value: Value) -> Result<(), ()> {
    write.send(WsMsg::Text(value.to_string().into())).await.map_err(|_| ())
}

#[tokio::test]
async fn ws_subscribe_immediate_event_and_limits() {
    let (state, issuer) = test_state();
    let token = issuer.issue("u-admin", None).expect("issue");
    let addr = spawn_ws_server(state).await;
    let (stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/v3/ws")).await.expect("connect ws");
    let (mut write, mut read) = stream.split();

    // subscribe: ответ {id, payload:{}} + немедленное событие диска.
    ws_send(
        &mut write,
        json!({
            "method": "subscribe",
            "payload": { "events": { "system:status:disk": {} } },
            "id": "sub-1",
            "access_token": token
        }),
    )
    .await
    .expect("send subscribe");
    let reply = ws_next_json(&mut read).await;
    assert_eq!(reply["id"], "sub-1");
    assert!(reply.get("error").is_none(), "subscribe must not error: {reply}");
    let event = ws_next_json(&mut read).await;
    assert_eq!(event["event"], "system:status:disk");
    assert_eq!(event["payload"]["full"], "NORMAL");

    // ui:users:search → пользователь из LocalDb-хранилища.
    ws_send(
        &mut write,
        json!({
            "method": "ui:users:search",
            "payload": { "search": "adm" },
            "id": "u-1",
            "access_token": token
        }),
    )
    .await
    .expect("send users search");
    let reply = ws_next_json(&mut read).await;
    assert_eq!(reply["id"], "u-1");
    assert_eq!(reply["payload"]["count"], 1);
    assert_eq!(reply["payload"]["users"][0]["display_name"], "admin");

    // Неизвестный метод → 404, соединение живо.
    ws_send(&mut write, json!({ "method": "nope", "payload": {}, "id": "x-1", "access_token": token }))
        .await
        .expect("send unknown");
    let reply = ws_next_json(&mut read).await;
    assert_eq!(reply["id"], "x-1");
    assert_eq!(reply["error"]["code"], 404);

    // Битый токен → 401, соединение по-прежнему живо.
    ws_send(&mut write, json!({ "method": "nope", "payload": {}, "id": "x-2", "access_token": "bad" }))
        .await
        .expect("send bad token");
    let reply = ws_next_json(&mut read).await;
    assert_eq!(reply["error"]["code"], 401);

    // Кадр > 16 KB → 413 без разрыва.
    let huge = "x".repeat(super::ws::MAX_FRAME_BYTES + 1);
    ws_send(
        &mut write,
        json!({ "method": "ui:files:search", "payload": { "q": huge }, "id": "big-1", "access_token": token }),
    )
    .await
    .expect("send oversized");
    let reply = ws_next_json(&mut read).await;
    assert_eq!(reply["error"]["code"], 413);

    // После всех ошибок рабочий запрос всё ещё обслуживается.
    ws_send(
        &mut write,
        json!({ "method": "ui:call_strings:search", "payload": {}, "id": "ok-1", "access_token": token }),
    )
    .await
    .expect("send call strings");
    let reply = ws_next_json(&mut read).await;
    assert_eq!(reply["id"], "ok-1");
    assert_eq!(reply["payload"]["count"], 0);

    // Чистое закрытие: сервер отвечает Close-frame'ом без ошибок.
    write.send(WsMsg::Close(None)).await.expect("send close");
    if let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(5), read.next()).await {
        assert!(matches!(frame, WsMsg::Close(_)), "expected close frame, got {frame:?}");
    }
}

#[tokio::test]
async fn ws_connection_without_subscription_still_answers() {
    let (state, issuer) = test_state();
    let token = issuer.issue("u-admin", None).expect("issue");
    let addr = spawn_ws_server(state).await;
    let (stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/v3/ws")).await.expect("connect ws");
    let (mut write, mut read) = stream.split();

    ws_send(&mut write, json!({ "method": "unsubscribe", "payload": {}, "id": "us-1", "access_token": token }))
        .await
        .expect("send unsubscribe");
    let reply = ws_next_json(&mut read).await;
    assert_eq!(reply["id"], "us-1");
    assert!(reply.get("error").is_none());
}

#[tokio::test]
async fn ws_rate_limit_returns_http_429() {
    let (state, _) = test_state();
    let addr = spawn_ws_server(state.clone()).await;
    let url = format!("ws://{addr}/api/v3/ws");
    // Исчерпываем лимит соединений для IP 127.0.0.1 и пробуем подключиться.
    // Окно — секунда: если между исчерпанием и коннектом оно перещёлкнуло,
    // повторяем (иначе тест был бы флейки на границе секунды).
    for _attempt in 0..50 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        for _ in 0..super::ws::WS_MAX_CONNECTIONS_PER_SEC {
            assert!(state.api_v3.ws_ratelimit.allow("127.0.0.1", now_ms));
        }
        match tokio_tungstenite::connect_async(&url).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                return;
            }
            Err(other) => panic!("expected HTTP 429 refusal, got {other:?}"),
            Ok(_) => continue, // Окно перещёлкнуло — повторяем.
        }
    }
    panic!("не удалось наблюдать 429 за 50 попыток (границы секунд)");
}

struct RegisteredDialer;

#[async_trait::async_trait]
impl crate::state::OutboundDialer for RegisteredDialer {
    async fn dial(&self, _: RoomId, _: &str, _: Option<&str>) -> Result<String, String> {
        Ok("registered@gw".into())
    }

    fn gateway_registration(&self, _protocol: &str) -> Option<bool> {
        Some(true)
    }
}

#[tokio::test]
async fn system_status_uses_real_ip_db_size_and_last_gateway_dial() {
    let (state, _) = test_state();
    state.set_dialer(Arc::new(RegisteredDialer));
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let expected_ip = state.server_addr.get().expect("server_addr").clone();
    let db = std::env::temp_dir().join(format!("vks-status-{}.db", std::process::id()));
    std::fs::write(&db, vec![0u8; 4096]).expect("write fake db");
    let _ = state.database_file.set(db.to_string_lossy().into_owned());

    // H.323 direct mode без факта H.225 setup — DISABLED, не ложный READY.
    let resp = send(&app, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["network"]["ip"], expected_ip);
    assert_eq!(body["sip"]["local_address"], expected_ip);
    assert_eq!(body["database"]["size_bytes"], 4096);
    assert_eq!(body["sip"]["status"], "READY");
    assert_eq!(body["sip"]["registration"], true);
    assert_eq!(body["h323"]["status"], "DISABLED");
    assert_eq!(body["h323"]["registration"], false);
    assert_eq!(body["h323"]["enable"], true);

    // Реальный успешный setup — единственный источник READY для H.323.
    state.record_gateway_health("h323", true, false, None);
    let resp = send(&app, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["h323"]["status"], "READY");
    assert_eq!(body["h323"]["registration"], true);

    // Терминальный отказ не гасит живой шлюз и не стирает last_success_at.
    state.record_gateway_health("h323", false, true, Some("remote_release:cause=17".into()));
    let resp = send(&app, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["h323"]["status"], "READY");
    assert_eq!(body["h323"]["connected"], true);
    assert!(body["h323"]["last_error"].as_str().unwrap_or_default().contains("remote_release"));

    // Транспортный отказ — FAILED до следующего фактического успеха.
    state.record_gateway_health("h323", false, false, Some("connect refused".into()));
    let resp = send(&app, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["h323"]["status"], "FAILED");
    assert_eq!(body["h323"]["connected"], false);

    state.record_gateway_health("h323", true, false, None);
    let resp = send(&app, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["h323"]["status"], "READY");
    assert!(body["h323"]["last_error"].is_null());
    let _ = std::fs::remove_file(db);
}

#[tokio::test]
async fn shell_monitoring_shapes_and_lifecycle_m4() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;

    // 1. C1: ui_system_status must have connected=true and realistic disk
    let resp = send(&app, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["engine"]["connected"], true, "engine.connected must be true (C1)");
    assert_eq!(body["database"]["connected"], true);
    assert_eq!(body["network"]["connected"], true);
    assert_eq!(body["network"]["editable"], true);
    // Эталон отдаёт `disk.free` в KiB; 1 GiB = 1_048_576 KiB.
    assert!(body["disk"]["free"].as_u64().unwrap_or(0) > 1_000_000, "disk free must be > 1 GiB (KiB)");

    // 2. C2: settings_calls must return payload wrapper with fecc/aspeaker and support PATCH
    let resp = send(&app, request("GET", "/api/v3/settings/calls", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["payload"].is_object(), "payload wrapper is required for Cne (C2)");
    assert_eq!(body["payload"]["fecc"], true);
    assert_eq!(body["payload"]["aspeaker"], "SELECTIVE");

    let patch_resp = send(
        &app,
        request(
            "PATCH",
            "/api/v3/settings/calls",
            Some(&token),
            json!({ "payload": { "limits": { "max_participants_per_room": 50 } } }),
        ),
    )
    .await;
    assert_eq!(patch_resp.status(), StatusCode::OK);
    let patch_body = json_body(patch_resp).await;
    assert_eq!(patch_body["payload"]["limits"]["max_participants_per_room"], 50);

    // 3. C3: abonents_count must return connections object
    let resp = send(&app, request("GET", "/api/v3/abonents/count", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["connections"].is_object(), "connections object is required for hI/BQ (C3)");
    assert!(body["connections"]["total"].is_number());
    assert!(body["connections"]["session"].is_number());

    // 4. C4: system_time must return nested time object
    let resp = send(&app, request("GET", "/api/v3/system/time", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["time"].is_object(), "time must be an object for NW (C4)");
    assert!(body["time"]["time"].is_number());

    // 5. C11: license/diagnostics — вложенные структуры; калибровка удалена из UX (T1.1)
    // и отдаётся как у TrueConf-эталона: `{"calibration":null}`.
    let resp = send(&app, request("GET", "/api/v3/license", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert!(body["params"]["protocol"]["max_calls"].is_number(), "license params required (C11)");

    let resp = send(&app, request("GET", "/api/v3/engine/diagnostics", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["diagnostics"]["vsm"]["limits"]["performance"], 100, "diagnostics required (C11)");

    let resp = send(&app, request("GET", "/api/v3/engine/calibration", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["calibration"].is_null(), "calibration must be null (T1.1)");
    // Раздел удалён: start/stop не зарегистрированы (405), состояние не меняется.
    for method in ["POST", "DELETE"] {
        let resp = send(&app, request(method, "/api/v3/engine/calibration", Some(&token), json!({}))).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{method} calibration must be 405");
    }

    // 6. M4.3 Lifecycle: Create -> Patch -> Start session -> Query -> Stop session -> Verify conference remains!
    let create_resp = send(
        &app,
        request("POST", "/api/v3/conferences", Some(&token), json!({ "display_name": "Test Lifecycle Conf" })),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_body = json_body(create_resp).await;
    let conf_id = create_body["conference"]["id"].as_str().unwrap().to_string();

    // PATCH conference
    let patch_conf_resp = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/conferences/{conf_id}"),
            Some(&token),
            json!({ "display_name": "Renamed Conf" }),
        ),
    )
    .await;
    assert_eq!(patch_conf_resp.status(), StatusCode::OK);
    let patch_conf_body = json_body(patch_conf_resp).await;
    assert_eq!(patch_conf_body["conference"]["display_name"], "Renamed Conf");

    // Contacts & abonents endpoints
    let contacts_resp =
        send(&app, request("GET", &format!("/api/v3/ui/conferences/{conf_id}/contacts"), Some(&token), json!({})))
            .await;
    assert_eq!(contacts_resp.status(), StatusCode::OK);
    let contacts_body = json_body(contacts_resp).await;
    assert_eq!(contacts_body["contacts"].as_array().map(Vec::len), Some(0));

    let abonents_resp =
        send(&app, request("GET", &format!("/api/v3/ui/sessions/{conf_id}/abonents"), Some(&token), json!({}))).await;
    assert_eq!(abonents_resp.status(), StatusCode::OK);
    let abonents_body = json_body(abonents_resp).await;
    assert_eq!(abonents_body["abonents"].as_array().map(Vec::len), Some(0));

    // Start session
    let start_resp =
        send(&app, request("POST", "/api/v3/sessions", Some(&token), json!({ "conference_id": conf_id }))).await;
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body = json_body(start_resp).await;
    assert_eq!(start_body["session"]["state"], "ACTIVE");
    assert_eq!(start_body["session"]["has_stopped"], false);

    // Stop session
    let stop_resp =
        send(&app, request("DELETE", &format!("/api/v3/sessions/{conf_id}"), Some(&token), json!({}))).await;
    assert_eq!(stop_resp.status(), StatusCode::NO_CONTENT);

    // CRITICAL: Conference MUST STILL EXIST in /api/v3/conferences/{id}!
    let check_conf_resp =
        send(&app, request("GET", &format!("/api/v3/conferences/{conf_id}"), Some(&token), json!({}))).await;
    assert_eq!(check_conf_resp.status(), StatusCode::OK, "stopping session MUST NOT delete the conference!");

    // Stopped session must appear in has_stopped=true
    let stopped_query = send(&app, request("GET", "/api/v3/sessions?has_stopped=true", Some(&token), json!({}))).await;
    let stopped_body = json_body(stopped_query).await;
    assert!(stopped_body["count"].as_u64().unwrap_or(0) >= 1, "stopped session must be found in has_stopped=true");

    // DELETE conference permanently
    let del_conf =
        send(&app, request("DELETE", &format!("/api/v3/conferences/{conf_id}"), Some(&token), json!({}))).await;
    assert_eq!(del_conf.status(), StatusCode::NO_CONTENT);

    let final_check =
        send(&app, request("GET", &format!("/api/v3/conferences/{conf_id}"), Some(&token), json!({}))).await;
    assert_eq!(final_check.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn m4_abonents_lifecycle_and_commands() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    // 1. Quick session
    let quick_resp = send(
        &app,
        request("POST", "/api/v3/sessions/quick", Some(&token), json!({ "display_name": "M4 Quick Session" })),
    )
    .await;
    assert_eq!(quick_resp.status(), StatusCode::OK);
    let quick_body = json_body(quick_resp).await;
    let room_id_str = quick_body["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(quick_body["session"]["display_name"], "M4 Quick Session");
    assert_eq!(quick_body["session"]["state"], "ACTIVE");
    // T1.2: addresses нужны карточке конференции (`Proxy.informationList`).
    assert_eq!(
        quick_body["session"]["addresses"],
        json!({
            "sip": "sip:M4_Quick_Session@192.168.1.92",
            "h323": "h323:M4_Quick_Session@192.168.1.92",
            "webrtc": "sip:M4_Quick_Session@127.0.0.1",
        })
    );

    let room_id = vks_core::RoomId::from_uuid(uuid::Uuid::parse_str(&room_id_str).unwrap());

    // 2. Register participant in RoomHub
    let pid = "p-m4-test-1".to_string();
    let p_snap = crate::hub::ParticipantSnapshot {
        participant_id: pid.clone(),
        display_name: "Bob".to_string(),
        is_recorder: false,
        is_speaking: false,
        track_ids: vec!["t1".into()],
        is_audio_muted: false,
        is_video_muted: false,
        connected_at: 1700000000,
    };
    let (_rx, _) = state.room_hub.register_participant(&room_id, p_snap);

    // 3. GET abonents
    let abonents_resp =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/abonents"), Some(&token), json!({}))).await;
    assert_eq!(abonents_resp.status(), StatusCode::OK);
    let abonents_body = json_body(abonents_resp).await;
    assert_eq!(abonents_body["count"], 1);
    assert_eq!(abonents_body["connected"][0]["display_name"], "Bob");
    assert_eq!(abonents_body["connected"][0]["payload"]["audio_tx_enabled"], true);
    assert_eq!(abonents_body["connected"][0]["payload"]["video_tx_enabled"], true);
    // T1.2: engine-маппер UI читает `details.user_agent` (`isWebrtcAbonent().includes`)
    // и `details.*.enabled`; без них — TypeError в карточке участника.
    assert_eq!(
        abonents_body["connected"][0]["details"]["user_agent"], "Trueconf WebRTC Gateway",
        "isWebrtcAbonent() checks this exact UA substring"
    );
    assert_eq!(abonents_body["connected"][0]["details"]["audio"]["tx"]["enabled"], true);
    assert_eq!(abonents_body["connected"][0]["details"]["audio"]["rx"]["volume"], 0);

    // T1.3: у сессии всегда есть «Общая» раскладка, и участник привязан к её id —
    // без этого `sessionLayoutListByType.default.some(...)` падает в session view.
    let layouts_resp =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/video_layouts"), Some(&token), json!({})))
            .await;
    assert_eq!(layouts_resp.status(), StatusCode::OK);
    let layouts_body = json_body(layouts_resp).await;
    assert_eq!(layouts_body["count"], 1, "session must get a default layout");
    assert_eq!(layouts_body["video_layouts"][0]["payload"]["type"], "default");
    let default_layout_id = layouts_body["video_layouts"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        abonents_body["connected"][0]["video_layout_id"], default_layout_id,
        "abonent must reference the session layout (hasIndividualLayout compares layoutId)"
    );

    // engine/abonents фильтруется по session_id (иначе карточка видит чужих).
    let engine_own = send(
        &app,
        request("GET", &format!("/api/v3/engine/abonents?session_id={room_id_str}"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(engine_own.status(), StatusCode::OK);
    let engine_own_body = json_body(engine_own).await;
    assert_eq!(engine_own_body["count"], 1);
    // display_name — регистронезависимый поиск (передаёт store `engine-abonents`).
    let engine_search =
        send(&app, request("GET", "/api/v3/engine/abonents?display_name=bo", Some(&token), json!({}))).await;
    assert_eq!(json_body(engine_search).await["count"], 1);
    let engine_other = send(
        &app,
        request(
            "GET",
            "/api/v3/engine/abonents?session_id=00000000-0000-0000-0000-000000000000",
            Some(&token),
            json!({}),
        ),
    )
    .await;
    let engine_other_body = json_body(engine_other).await;
    assert_eq!(engine_other_body["count"], 0, "session_id must filter abonents");
    // `/ui/conferences/{id}` использует тот же session_json (карточка конференции).
    let ui_conf =
        send(&app, request("GET", &format!("/api/v3/ui/conferences/{room_id_str}"), Some(&token), json!({}))).await;
    assert_eq!(ui_conf.status(), StatusCode::OK);
    assert_eq!(json_body(ui_conf).await["conference"]["addresses"]["sip"], "sip:M4_Quick_Session@192.168.1.92");

    // 4. Mute Audio
    let mute_audio =
        send(&app, request("POST", &format!("/api/v3/abonents/{pid}/audio"), Some(&token), json!({ "enable": false })))
            .await;
    assert_eq!(mute_audio.status(), StatusCode::OK);

    let check_mute_a =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/abonents"), Some(&token), json!({}))).await;
    let body_mute_a = json_body(check_mute_a).await;
    assert_eq!(body_mute_a["connected"][0]["payload"]["audio_tx_enabled"], false);

    // 5. Mute Video
    let mute_video =
        send(&app, request("POST", &format!("/api/v3/abonents/{pid}/video"), Some(&token), json!({ "enable": false })))
            .await;
    assert_eq!(mute_video.status(), StatusCode::OK);

    let check_mute_v =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/abonents"), Some(&token), json!({}))).await;
    let body_mute_v = json_body(check_mute_v).await;
    assert_eq!(body_mute_v["connected"][0]["payload"]["video_tx_enabled"], false);

    // 6. Rename participant
    let rename = send(
        &app,
        request("PATCH", &format!("/api/v3/abonents/{pid}"), Some(&token), json!({ "display_name": "Bob Renamed" })),
    )
    .await;
    assert_eq!(rename.status(), StatusCode::OK);

    let check_rename =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/abonents"), Some(&token), json!({}))).await;
    let body_rename = json_body(check_rename).await;
    assert_eq!(body_rename["connected"][0]["display_name"], "Bob Renamed");

    // 7. Video layout mode switch (layout обязан быть привязан к сессии)
    let layouts = send(
        &app,
        request("GET", &format!("/api/v3/ui/sessions/{room_id_str}/video_layouts"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(layouts.status(), StatusCode::OK);
    let layouts_body = json_body(layouts).await;
    assert_eq!(layouts_body["count"], 1, "сессия получает ровно одну «Общую» раскладку");
    let layout_id = layouts_body["video_layouts"][0]["id"].as_str().unwrap().to_string();
    assert!(layouts_body["video_layouts"][0]["abonents"]["abonents"]["abonents"].is_array());

    let mode_switch = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts/mode",
            Some(&token),
            json!({
                "session_id": room_id_str,
                "new_mode": "mcu",
                "video_layout_id": layout_id,
                "payload": {"mode":"manual","preset":"preset_equal_4","type":"individual","size":4}
            }),
        ),
    )
    .await;
    assert_eq!(mode_switch.status(), StatusCode::OK);
    let mode_body = json_body(mode_switch).await;
    assert_eq!(mode_body["payload"]["mode"], "manual");
    assert_eq!(mode_body["payload"]["preset"], "preset_equal_4");
    assert_eq!(mode_body["payload"]["type"], "individual");
    assert!(mode_body["abonents"]["abonents"]["abonents"].is_array());

    let unknown_layout = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts/mode",
            Some(&token),
            json!({"session_id": room_id_str, "new_mode": "auto", "video_layout_id": "grid"}),
        ),
    )
    .await;
    assert_eq!(unknown_layout.status(), StatusCode::NOT_FOUND, "чужой layout_id не даёт фейковый 200");

    // 7b. Codian-раскладка проходит тем же путём «Применить»: id не подменяется на
    // `preset_equal_N`, а size считается по таблице макета (`slot_count`), не по хвосту id.
    let codian_switch = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts/mode",
            Some(&token),
            json!({
                "session_id": room_id_str,
                "new_mode": "mcu",
                "video_layout_id": layout_id,
                "payload": {"mode":"manual","preset":"codian_43"}
            }),
        ),
    )
    .await;
    assert_eq!(codian_switch.status(), StatusCode::OK);
    let codian_body = json_body(codian_switch).await;
    assert_eq!(codian_body["payload"]["preset"], "codian_43", "codian-пресет не должен подменяться");
    assert_eq!(
        codian_body["payload"]["size"],
        vks_layout::codian::slot_count("codian_43").unwrap() as u64,
        "size — число окон макета, а не хвост id"
    );
    assert_eq!(codian_body["payload"]["mode"], "manual");

    // 7c. Конструктор участников: параметрический `vks_*` не подменяется и не
    // теряется — иначе PIN молча превращался в equal-сетку из N окон.
    let constructor_switch = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts/mode",
            Some(&token),
            json!({
                "session_id": room_id_str,
                "new_mode": "mcu",
                "video_layout_id": layout_id,
                "payload": {"mode":"manual","preset":"vks_pin_r_9"}
            }),
        ),
    )
    .await;
    assert_eq!(constructor_switch.status(), StatusCode::OK);
    let constructor_body = json_body(constructor_switch).await;
    assert_eq!(constructor_body["payload"]["preset"], "vks_pin_r_9", "preset конструктора не подменяется");
    assert_eq!(constructor_body["payload"]["size"], 9, "size — число окон из параметрического id");

    // 7d. T13: счётчик конструктора доходит до 100 — size больше старого
    // лимита 64 не должен молча отбрасываться ни в mode, ни в upsert.
    let hundred_switch = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts/mode",
            Some(&token),
            json!({
                "session_id": room_id_str,
                "new_mode": "mcu",
                "video_layout_id": layout_id,
                "payload": {"mode":"manual","preset":"vks_pin_b_100","type":"individual","size":100,"slots":[]}
            }),
        ),
    )
    .await;
    assert_eq!(hundred_switch.status(), StatusCode::OK);
    let hundred_body = json_body(hundred_switch).await;
    assert_eq!(hundred_body["payload"]["preset"], "vks_pin_b_100", "100 окон не обрезаются до 64");
    assert_eq!(hundred_body["payload"]["size"], 100, "size=100 принимается (MAX_WINDOWS)");

    let bad_mode = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts/mode",
            Some(&token),
            json!({"session_id": room_id_str, "video_layout_id": layout_id, "payload": {"mode": "mcu"}}),
        ),
    )
    .await;
    assert_eq!(bad_mode.status(), StatusCode::UNPROCESSABLE_ENTITY, "payload.mode — только auto/dynamic/manual");

    // 8. Engine command: калибровка удалена (T1.1) — честный 501, не фейковый 200.
    let eng_resp =
        send(&app, request("POST", "/api/v3/engine/command", Some(&token), json!({ "command": "calibration" }))).await;
    assert_eq!(eng_resp.status(), StatusCode::NOT_IMPLEMENTED);
    let benign =
        send(&app, request("POST", "/api/v3/engine/command", Some(&token), json!({ "command": "reload" }))).await;
    assert_eq!(benign.status(), StatusCode::OK);

    // 9. Kick participant
    let kick_resp = send(&app, request("DELETE", &format!("/api/v3/abonents/{pid}"), Some(&token), json!({}))).await;
    assert_eq!(kick_resp.status(), StatusCode::NO_CONTENT);

    let after_kick =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/abonents"), Some(&token), json!({}))).await;
    let body_after_kick = json_body(after_kick).await;
    assert_eq!(body_after_kick["count"], 0);
    assert!(body_after_kick["connected"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_m0_ui_system_status_faults_and_password_persistence() {
    let storage = std::sync::Arc::new(vks_storage::MemoryStorage::new());
    let local_db = std::sync::Arc::new(vks_auth::local_db::LocalDb::new());
    local_db.register(vks_core::BOOTSTRAP_ADMIN_USER_ID, "admin", "admin_pass").unwrap();

    let mut state = AppState::with_storage(storage.clone());
    state.local_db = Some(local_db.clone());
    state.jwt_issuer = Some(std::sync::Arc::new(vks_auth::jwt::JwtIssuer::new(vks_auth::jwt::JwtConfig {
        secret: "test_secret".to_string(),
        ttl_secs: 3600,
        issuer: "test".to_string(),
    })));
    crate::users::Users::bootstrap_operator_async(&state, "admin").await.unwrap();

    let state = std::sync::Arc::new(state);
    let app_router = app(state.clone());
    let token = login_token(&app_router, "admin", "admin_pass").await;

    // 1. Initial healthy status
    let resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["engine"]["status"], "READY");
    assert_eq!(body["engine"]["connected"], true);
    assert_eq!(body["database"]["status"], "READY");
    assert_eq!(body["database"]["connected"], true);

    // 2. Inject media fault
    state.inject_fault("media");
    let resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["engine"]["status"], "FAILED");
    assert_eq!(body["engine"]["connected"], false);
    assert_eq!(body["database"]["status"], "READY");
    state.clear_fault("media");

    // 3. Inject db fault
    state.inject_fault("db");
    let resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["database"]["status"], "FAILED");
    assert_eq!(body["database"]["connected"], false);
    state.clear_fault("db");

    // 4. Change password and verify persistence
    let chg_resp = send(
        &app_router,
        request(
            "PATCH",
            "/api/v3/password",
            Some(&token),
            json!({
                "username": "admin",
                "old_password": "admin_pass",
                "new_password": "new_secret_pwd_42"
            }),
        ),
    )
    .await;
    assert_eq!(chg_resp.status(), StatusCode::OK);

    // Check old login fails
    let fail_resp = send(
        &app_router,
        request("POST", "/api/v3/login", None, json!({ "login": "admin", "password": "admin_pass" })),
    )
    .await;
    assert_eq!(fail_resp.status(), StatusCode::UNAUTHORIZED);

    // Check new login succeeds
    let new_token = login_token(&app_router, "admin", "new_secret_pwd_42").await;
    assert!(!new_token.is_empty());

    // 5. Verify credentials were saved in storage and hydrate into a fresh AppState
    let fresh_local_db = std::sync::Arc::new(vks_auth::local_db::LocalDb::new());
    let mut fresh_state = AppState::with_storage(storage.clone());
    fresh_state.local_db = Some(fresh_local_db.clone());
    fresh_state.jwt_issuer = state.jwt_issuer.clone();
    fresh_state.sync_from_storage().await.unwrap();

    let fresh_app = app(std::sync::Arc::new(fresh_state));
    let restored_token = login_token(&fresh_app, "admin", "new_secret_pwd_42").await;
    assert!(!restored_token.is_empty());

    // 6. Verify audit entries were durably persisted to storage and reloaded
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let audit_records = vks_core::ports::storage::Storage::list_audit(storage.as_ref()).await.unwrap();
    assert!(audit_records.iter().any(|a| a.action == "password.changed" && a.user == "admin"));
}

#[tokio::test]
async fn test_m1_password_change_failure_rolls_back_and_preserves_old_credentials() {
    let storage = std::sync::Arc::new(vks_storage::MemoryStorage::new());
    let local_db = std::sync::Arc::new(vks_auth::local_db::LocalDb::new());
    local_db.register(vks_core::BOOTSTRAP_ADMIN_USER_ID, "admin", "original_secret").unwrap();

    let mut state = AppState::with_storage(storage.clone());
    state.local_db = Some(local_db.clone());
    state.jwt_issuer = Some(std::sync::Arc::new(vks_auth::jwt::JwtIssuer::new(vks_auth::jwt::JwtConfig {
        secret: "test_secret".to_string(),
        ttl_secs: 3600,
        issuer: "test".to_string(),
    })));
    crate::users::Users::bootstrap_operator_async(&state, "admin").await.unwrap();

    let state = std::sync::Arc::new(state);
    let app_router = app(state.clone());
    let token = login_token(&app_router, "admin", "original_secret").await;

    // Inject database storage failure
    state.inject_fault("db");

    // Attempt to change password while DB is failed
    let chg_resp = send(
        &app_router,
        request(
            "PATCH",
            "/api/v3/password",
            Some(&token),
            json!({
                "username": "admin",
                "old_password": "original_secret",
                "new_password": "unpersisted_new_pass"
            }),
        ),
    )
    .await;

    // Must fail-closed with 503 error
    assert_eq!(chg_resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Old password must STILL work!
    let login_old = send(
        &app_router,
        request("POST", "/api/v3/login", None, json!({ "login": "admin", "password": "original_secret" })),
    )
    .await;
    assert_eq!(login_old.status(), StatusCode::OK);

    // Unpersisted new password must NOT work!
    let login_new = send(
        &app_router,
        request("POST", "/api/v3/login", None, json!({ "login": "admin", "password": "unpersisted_new_pass" })),
    )
    .await;
    assert_eq!(login_new.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_m0_custom_layouts_api_keys_and_gateway_cluster_status() {
    let storage = std::sync::Arc::new(vks_storage::MemoryStorage::new());

    // 1. Seed custom layout in storage
    let custom_layout = vks_core::ports::storage::Layout {
        id: "custom_3x3".to_string(),
        name: "3x3 Grid Custom".to_string(),
        version: 1,
        assigned_rooms: std::collections::BTreeMap::new(),
        is_default: false,
    };
    vks_core::ports::storage::Storage::upsert_layout(storage.as_ref(), &custom_layout).await.unwrap();

    // 2. Seed API key in storage
    let (plain_key, _key_meta) = vks_auth::api_key::generate("key-op-1");
    let key_hash = vks_auth::api_key::sha256_hex(plain_key.as_bytes());
    let admin_id = vks_core::UserId::parse(vks_core::BOOTSTRAP_ADMIN_USER_ID).unwrap();
    let stored_key = vks_core::ports::storage::ApiKey {
        id: key_hash.clone(),
        scopes: vec!["admin".to_string()],
        owner: admin_id,
        revoked: false,
    };
    vks_core::ports::storage::Storage::upsert_api_key(storage.as_ref(), &stored_key).await.unwrap();

    // 3. Create state and sync from storage
    let mut state = AppState::with_storage(storage.clone());
    let local_db = std::sync::Arc::new(vks_auth::local_db::LocalDb::new());
    local_db.register(vks_core::BOOTSTRAP_ADMIN_USER_ID, "admin", "admin_secret").unwrap();
    state.local_db = Some(local_db);
    state.jwt_issuer = Some(std::sync::Arc::new(vks_auth::jwt::JwtIssuer::new(vks_auth::jwt::JwtConfig {
        secret: "test_secret".to_string(),
        ttl_secs: 3600,
        issuer: "test".to_string(),
    })));
    crate::users::Users::bootstrap_operator_async(&state, "admin").await.unwrap();

    state.sync_from_storage().await.expect("sync_from_storage succeeds");

    // Verify custom layout is hydrated in state.layouts
    {
        let layouts = state.layouts.lock().unwrap();
        assert!(layouts.contains_key("custom_3x3"));
        assert_eq!(layouts.get("custom_3x3").unwrap().name, "3x3 Grid Custom");
    }

    // Verify API key is hydrated in state.api_keys
    {
        let keys = state.api_keys.lock().unwrap();
        assert!(keys.contains_key(&key_hash));
    }

    let state = std::sync::Arc::new(state);
    let app_router = app(state.clone());
    let token = login_token(&app_router, "admin", "admin_secret").await;

    // 4. Verify GET /api/v3/video_layouts returns Grid, Focus, Speaker, AND custom_3x3
    let vl_resp = send(&app_router, request("GET", "/api/v3/video_layouts", Some(&token), json!({}))).await;
    assert_eq!(vl_resp.status(), StatusCode::OK);
    let vl_body = json_body(vl_resp).await;
    let list = vl_body["video_layouts"].as_array().expect("array of layouts");
    assert!(list.iter().any(|l| l["id"] == "grid"));
    assert!(list.iter().any(|l| l["id"] == "focus"));
    assert!(list.iter().any(|l| l["id"] == "speaker"));
    assert!(list.iter().any(|l| l["id"] == "custom_3x3" && l["title"] == "3x3 Grid Custom"));

    // 5. Test truthful gateway and cluster status in ui/system/status
    let status_resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    assert_eq!(status_resp.status(), StatusCode::OK);
    let status_body = json_body(status_resp).await;
    // Без dialer нет ни регистрации, ни факта успеха: честный DISABLED,
    // а не статический READY.
    assert_eq!(status_body["sip"]["status"], "DISABLED");
    assert_eq!(status_body["sip"]["connected"], false);
    assert_eq!(status_body["h323"]["status"], "DISABLED");
    assert_eq!(status_body["h323"]["connected"], false);
    assert_eq!(status_body["cluster"]["status"], "READY");
    assert_eq!(status_body["cluster"]["connected"], true);
    assert_eq!(status_body["cluster"]["lease_valid"], true);

    // 6. Inject SIP fault
    state.inject_fault("sip");
    let resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["sip"]["status"], "FAILED");
    assert_eq!(body["sip"]["connected"], false);
    assert_eq!(body["h323"]["status"], "DISABLED");
    state.clear_fault("sip");

    // 7. Inject Cluster fault
    state.inject_fault("cluster");
    let resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["cluster"]["status"], "FAILED");
    assert_eq!(body["cluster"]["connected"], false);
    state.clear_fault("cluster");

    // 8. Inject Lease fault
    state.inject_fault("lease");
    let resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["cluster"]["lease_valid"], false);
    state.clear_fault("lease");
}

struct PendingTestDialer;
#[async_trait::async_trait]
impl crate::state::OutboundDialer for PendingTestDialer {
    async fn dial(&self, _: RoomId, _: &str, _: Option<&str>) -> Result<String, String> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn step2_call_strings_and_invite_vertical() {
    let (state, _) = test_state();
    state.set_dialer(Arc::new(PendingTestDialer));
    let app_router = app(state.clone());
    let token = login_token(&app_router, "admin", "s3cret").await;

    // 1. Create conference
    let create_resp = send(
        &app_router,
        request("POST", "/api/v3/conferences", Some(&token), json!({ "display_name": "Vertical 192 Conf" })),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_body = json_body(create_resp).await;
    let conf_id = create_body["conference"]["id"].as_str().unwrap().to_string();

    // 2. Start session
    let start_resp =
        send(&app_router, request("POST", "/api/v3/sessions", Some(&token), json!({ "conference_id": conf_id }))).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    // 3. Test POST /api/v3/ui/conferences/{id}/call_strings
    let cs_conf_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/conferences/{conf_id}/call_strings"),
            Some(&token),
            json!({ "uri": "192.168.1.132", "display_name": "Terminal 132" }),
        ),
    )
    .await;
    assert_eq!(cs_conf_resp.status(), StatusCode::CREATED);
    let cs_conf_body = json_body(cs_conf_resp).await;
    assert_eq!(cs_conf_body["call_string"]["uri"], "sip:192.168.1.132");
    assert_eq!(cs_conf_body["call_string"]["display_name"], "Terminal 132");
    assert!(cs_conf_body["call_string"]["id"].is_string());

    // 4. Test validation error (empty uri) returns 400 with details array
    let val_err_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/conferences/{conf_id}/call_strings"),
            Some(&token),
            json!({ "uri": "   " }),
        ),
    )
    .await;
    assert_eq!(val_err_resp.status(), StatusCode::BAD_REQUEST);
    let val_err_body = json_body(val_err_resp).await;
    assert_eq!(val_err_body["error"]["code"], 400);
    assert_eq!(val_err_body["error"]["details"][0]["reason"], "invalid.uri");
    assert_eq!(val_err_body["error"]["details"][0]["position"], "uri");

    // 5. Test stray brace URL rewrite: GET /api/v3/conferences/{id}}/contacts
    let stray_resp = send(
        &app_router,
        request("GET", &format!("/api/v3/conferences/{conf_id}}}/contacts"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(stray_resp.status(), StatusCode::OK, "Stray brace route must not return 404/501");
    let stray_body = json_body(stray_resp).await;
    assert_eq!(stray_body["count"], 1, "Added template contact must remain visible");

    // 6. Test standby abonent: POST /api/v3/ui/sessions/{id}/call_strings/without_call
    let standby_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings/without_call"),
            Some(&token),
            json!({ "uri": "sip:192.168.1.150", "display_name": "Standby Box" }),
        ),
    )
    .await;
    assert_eq!(standby_resp.status(), StatusCode::CREATED);

    // Verify standby appears in session abonents
    let abonents_resp =
        send(&app_router, request("GET", &format!("/api/v3/sessions/{conf_id}/abonents"), Some(&token), json!({})))
            .await;
    assert_eq!(abonents_resp.status(), StatusCode::OK);
    let abonents_body = json_body(abonents_resp).await;
    assert_eq!(abonents_body["count"], 1);
    assert_eq!(abonents_body["scheduled"][0]["display_name"], "Standby Box");
    assert_eq!(abonents_body["scheduled"][0]["uri"], "sip:192.168.1.150");
    assert_eq!(abonents_body["scheduled"][0]["payload"]["audio_tx_enabled"], true);
    assert_eq!(abonents_body["scheduled"][0]["payload"]["audio_tx_enabled"], true);

    // 7. Test active outbound dial: POST /api/v3/ui/sessions/{id}/call_strings
    let dial_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings"),
            Some(&token),
            json!({ "uri": "192.168.1.132", "display_name": "Live 132" }),
        ),
    )
    .await;
    assert_eq!(dial_resp.status(), StatusCode::CREATED);
    let dial_body = json_body(dial_resp).await;
    assert_eq!(dial_body["call_string"]["uri"], "sip:192.168.1.132");

    // 8. Test In-Flight Guard: Immediate duplicate dial returns 409
    let dup_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings"),
            Some(&token),
            json!({ "uri": "sip:192.168.1.132" }),
        ),
    )
    .await;
    assert_eq!(dup_resp.status(), StatusCode::CONFLICT);
    let dup_body = json_body(dup_resp).await;
    assert_eq!(dup_body["error"]["code"], 409);
    assert_eq!(dup_body["error"]["description"], "The specified call string is used in session");

    // 9. Test Recall/Invite batch: POST /api/v3/sessions/{id}/invite/call_strings/list
    let recall_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/sessions/{conf_id}/invite/call_strings/list"),
            Some(&token),
            json!({ "call_strings": [{ "uri": "sip:192.168.1.150", "display_name": "Standby Box" }] }),
        ),
    )
    .await;
    assert_eq!(recall_resp.status(), StatusCode::OK);

    // 10. Verify GET /api/v3/call_strings and /ui/call_strings
    let cs_get = send(&app_router, request("GET", "/api/v3/call_strings", Some(&token), json!({}))).await;
    assert_eq!(cs_get.status(), StatusCode::OK);
    let cs_get_ui = send(&app_router, request("GET", "/api/v3/ui/call_strings", Some(&token), json!({}))).await;
    assert_eq!(cs_get_ui.status(), StatusCode::OK);

    // 11. Test POST /api/v3/conferences/{id}/invite/call_string/list (batch template invite)
    let batch_conf_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/conferences/{conf_id}/invite/call_string/list"),
            Some(&token),
            json!({
                "call_strings": [
                    { "uri": "192.168.1.160", "display_name": "Box 160" },
                    { "uri": "192.168.1.161", "display_name": "Box 161" }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(batch_conf_resp.status(), StatusCode::OK);

    // 12. Test Role-Based Access Control (< Operator -> 403 Forbidden)
    crate::users::Users::create(&state, "restricted_user", vks_core::Role::Moderator);
    let restricted_token = state.jwt_issuer.as_ref().unwrap().issue("restricted_user", None).expect("issue token");
    let forbidden_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/conferences/{conf_id}/call_strings"),
            Some(&restricted_token),
            json!({ "uri": "192.168.1.170", "display_name": "Forbidden Terminal" }),
        ),
    )
    .await;
    assert_eq!(forbidden_resp.status(), StatusCode::FORBIDDEN);

    // 13. Test Room Capacity Limit (Too many connections -> 409 Conflict)
    let mut curr_settings = crate::settings::SettingsService::get(&state);
    curr_settings.limits.max_participants_per_room = 1;
    crate::settings::SettingsService::set(&state, curr_settings);
    let limit_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings"),
            Some(&token),
            json!({ "uri": "192.168.1.199", "display_name": "Over-limit Terminal" }),
        ),
    )
    .await;
    assert_eq!(limit_resp.status(), StatusCode::CONFLICT);
    let limit_body = json_body(limit_resp).await;
    assert_eq!(limit_body["error"]["code"], 409);
    assert_eq!(limit_body["error"]["description"], "Too many connections");

    // 14. Лимит комнаты (`conference_settings.connections_limit`) приоритетнее
    // системного умолчания: системный лимит поднимаем, комнатный ставим 1.
    let mut curr_settings = crate::settings::SettingsService::get(&state);
    curr_settings.limits.max_participants_per_room = 50;
    crate::settings::SettingsService::set(&state, curr_settings);
    let room = conf_id.parse::<RoomId>().unwrap();
    state.api_v3.conference_settings.lock().unwrap().insert(room, json!({"connections_limit": 1}));
    let room_limit_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings"),
            Some(&token),
            json!({ "uri": "192.168.1.200", "display_name": "Room-limit Terminal" }),
        ),
    )
    .await;
    assert_eq!(room_limit_resp.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(room_limit_resp).await["error"]["description"], "Too many connections");
}

type MediaControlRecord = (String, Option<bool>, Option<bool>, Option<u32>, Option<u32>);

#[derive(Default)]
struct MockStep4Dialer {
    last_dtmf: std::sync::Mutex<Option<(String, String)>>,
    last_fecc: std::sync::Mutex<Option<(String, String)>>,
    last_media_control: std::sync::Mutex<Option<MediaControlRecord>>,
}

#[async_trait::async_trait]
impl crate::state::OutboundDialer for MockStep4Dialer {
    async fn dial(&self, _room_id: RoomId, _uri: &str, _display_name: Option<&str>) -> Result<String, String> {
        Ok("sip-mock-pid@sip-gateway".to_string())
    }

    fn set_media_control(
        &self,
        participant_id: &str,
        audio_tx_muted: Option<bool>,
        audio_rx_muted: Option<bool>,
        _video_tx_muted: Option<bool>,
        _video_rx_muted: Option<bool>,
        volume_tx: Option<u32>,
        volume_rx: Option<u32>,
    ) -> bool {
        *self.last_media_control.lock().unwrap() =
            Some((participant_id.to_string(), audio_tx_muted, audio_rx_muted, volume_tx, volume_rx));
        true
    }

    fn send_dtmf(&self, participant_id: &str, sequence: &str) -> Result<(), String> {
        *self.last_dtmf.lock().unwrap() = Some((participant_id.to_string(), sequence.to_string()));
        Ok(())
    }

    fn send_fecc(&self, participant_id: &str, command: &str) -> Result<(), String> {
        *self.last_fecc.lock().unwrap() = Some((participant_id.to_string(), command.to_string()));
        Ok(())
    }
}

#[tokio::test]
async fn step4_abonent_media_controls_dtmf_fecc_and_standby() {
    let (state, _) = test_state();
    let mock_dialer = std::sync::Arc::new(MockStep4Dialer::default());
    state.set_dialer(mock_dialer.clone());

    let app_router = app(state.clone());
    let token = login_token(&app_router, "admin", "s3cret").await;

    // 1. Создаем комнату и начинаем сессию
    let create_resp = send(
        &app_router,
        request("POST", "/api/v3/conferences", Some(&token), json!({ "display_name": "Step 4 Room" })),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_body = json_body(create_resp).await;
    let conf_id = create_body["conference"]["id"].as_str().unwrap().to_string();

    let start_resp =
        send(&app_router, request("POST", "/api/v3/sessions", Some(&token), json!({ "conference_id": conf_id }))).await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let room_id = RoomId::from_uuid(uuid::Uuid::parse_str(&conf_id).expect("valid uuid"));

    // 2. Регистрируем участника в RoomHub
    let pid = "sip-123@sip-gateway".to_string();
    state.room_hub.register_participant(
        &room_id,
        crate::hub::ParticipantSnapshot {
            participant_id: pid.clone(),
            display_name: "Hardware Terminal 132".to_string(),
            is_recorder: false,
            is_speaking: false,
            track_ids: vec!["audio".into(), "video".into()],
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: 1000,
        },
    );

    // 3. Проверяем GET /api/v3/abonents и GET /api/v3/abonents/previews
    let ab_resp = send(&app_router, request("GET", "/api/v3/abonents", Some(&token), json!({}))).await;
    assert_eq!(ab_resp.status(), StatusCode::OK);
    let ab_body = json_body(ab_resp).await;
    assert!(ab_body["count"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(ab_body["connections"], ab_body["count"]);
    let found = ab_body["abonents"].as_array().unwrap().iter().any(|a| a["id"] == pid);
    assert!(found, "registered participant must appear in /api/v3/abonents");

    let prev_resp = send(&app_router, request("GET", "/api/v3/abonents/previews", Some(&token), json!({}))).await;
    assert_eq!(prev_resp.status(), StatusCode::OK);
    let prev_body = json_body(prev_resp).await;
    assert!(prev_body["previews"].is_array());

    // 4. Проверяем POST /api/v3/abonents/{id}/audio (mute TX mic)
    let mute_tx_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/abonents/{pid}/audio"),
            Some(&token),
            json!({ "enable": false, "outgoing": true }),
        ),
    )
    .await;
    assert_eq!(mute_tx_resp.status(), StatusCode::OK);
    let last_ctrl = mock_dialer.last_media_control.lock().unwrap().take().expect("media control called");
    assert_eq!(last_ctrl.0, pid);
    assert_eq!(last_ctrl.2, Some(true), "Native TX controls MCU→terminal (internal RX)");

    // 5. Проверяем POST /api/v3/abonents/{id}/microphone (mute RX speaker)
    let mute_rx_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/abonents/{pid}/microphone"),
            Some(&token),
            json!({ "enable": false, "outgoing": false }),
        ),
    )
    .await;
    assert_eq!(mute_rx_resp.status(), StatusCode::OK);
    let last_ctrl = mock_dialer.last_media_control.lock().unwrap().take().expect("media control called");
    assert_eq!(last_ctrl.1, Some(true), "Native RX controls terminal→MCU (internal TX)");

    // 6. Проверяем POST /api/v3/abonents/{id}/volume (volume RX)
    let vol_resp = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/abonents/{pid}/volume"),
            Some(&token),
            json!({ "volume": 80, "outgoing": false }),
        ),
    )
    .await;
    assert_eq!(vol_resp.status(), StatusCode::OK);
    let last_ctrl = mock_dialer.last_media_control.lock().unwrap().take().expect("media control called");
    assert_eq!(last_ctrl.3, Some(251), "Native +8dB RX must reach internal TX as linear 251%");

    // Проверяем, что GET /api/v3/abonents и GET /api/v3/engine/abonents возвращают сохраненную громкость 80
    let list_resp = send(&app_router, request("GET", "/api/v3/abonents", Some(&token), json!({}))).await;
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = json_body(list_resp).await;
    let abonent_item =
        list_body["abonents"].as_array().unwrap().iter().find(|a| a["id"] == pid).expect("abonent found in roster");
    assert_eq!(abonent_item["payload"]["volume_rx"], 80);

    // 7. Проверяем POST /api/v3/abonents/{id}/dtmf
    let dtmf_resp = send(
        &app_router,
        request("POST", &format!("/api/v3/abonents/{pid}/dtmf"), Some(&token), json!({ "sequence": "123#*" })),
    )
    .await;
    assert_eq!(dtmf_resp.status(), StatusCode::OK);
    let last_dtmf = mock_dialer.last_dtmf.lock().unwrap().take().expect("dtmf called");
    assert_eq!(last_dtmf.0, pid);
    assert_eq!(last_dtmf.1, "123#*");

    // Валидация DTMF: пустой или слишком длинный (>32)
    let dtmf_empty = send(
        &app_router,
        request("POST", &format!("/api/v3/abonents/{pid}/dtmf"), Some(&token), json!({ "sequence": "   " })),
    )
    .await;
    assert_eq!(dtmf_empty.status(), StatusCode::BAD_REQUEST);

    let dtmf_toolong = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/abonents/{pid}/dtmf"),
            Some(&token),
            json!({ "sequence": "12345678901234567890123456789012345" }),
        ),
    )
    .await;
    assert_eq!(dtmf_toolong.status(), StatusCode::BAD_REQUEST);

    // 8. Проверяем POST /api/v3/abonents/{id}/fecc
    let fecc_resp = send(
        &app_router,
        request("POST", &format!("/api/v3/abonents/{pid}/fecc"), Some(&token), json!({ "action": "zoom_in" })),
    )
    .await;
    assert_eq!(fecc_resp.status(), StatusCode::OK);
    let last_fecc = mock_dialer.last_fecc.lock().unwrap().take().expect("fecc called");
    assert_eq!(last_fecc.0, pid);
    assert_eq!(last_fecc.1, "zoom_in");

    // Валидация FECC: пустая команда
    let fecc_empty = send(
        &app_router,
        request("POST", &format!("/api/v3/abonents/{pid}/fecc"), Some(&token), json!({ "command": "" })),
    )
    .await;
    assert_eq!(fecc_empty.status(), StatusCode::BAD_REQUEST);

    // 9. Проверяем StandBy: добавление и удаление отдельного контакта
    let standby_add = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings/without_call"),
            Some(&token),
            json!({ "uri": "sip:192.168.1.188", "display_name": "Standby Box" }),
        ),
    )
    .await;
    assert_eq!(standby_add.status(), StatusCode::CREATED);
    let sb_body = json_body(standby_add).await;
    let contact_id = sb_body["call_string"]["id"].as_str().unwrap().to_string();

    // Проверяем, что контакт есть в StandBy
    {
        let map = state.api_v3.scheduled_calls.lock().unwrap();
        assert_eq!(map.get(&room_id).unwrap().len(), 1);
    }

    // Удаляем единичный контакт: DELETE /api/v3/sessions/{id}/contacts/{contact_id}
    let del_resp = send(
        &app_router,
        request("DELETE", &format!("/api/v3/sessions/{conf_id}/contacts/{contact_id}"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);

    {
        let map = state.api_v3.scheduled_calls.lock().unwrap();
        assert_eq!(map.get(&room_id).unwrap().len(), 0);
    }

    // 10. Проверяем пакетное удаление: DELETE /api/v3/sessions/{id}/contacts
    let _ = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings/without_call"),
            Some(&token),
            json!({ "uri": "sip:192.168.1.189", "display_name": "Standby Box 2" }),
        ),
    )
    .await;
    let _ = send(
        &app_router,
        request(
            "POST",
            &format!("/api/v3/ui/sessions/{conf_id}/call_strings/without_call"),
            Some(&token),
            json!({ "uri": "sip:192.168.1.190", "display_name": "Standby Box 3" }),
        ),
    )
    .await;

    // Пустой batch delete не должен удалять контакты (защита от потери данных)
    let empty_batch_del = send(
        &app_router,
        request("DELETE", &format!("/api/v3/sessions/{conf_id}/contacts"), Some(&token), json!({ "contacts": [] })),
    )
    .await;
    assert_eq!(empty_batch_del.status(), StatusCode::NO_CONTENT);
    {
        let map = state.api_v3.scheduled_calls.lock().unwrap();
        assert_eq!(map.get(&room_id).unwrap().len(), 2, "empty batch delete must NOT clear StandBy");
    }

    let batch_del_resp = send(
        &app_router,
        request(
            "DELETE",
            &format!("/api/v3/sessions/{conf_id}/contacts"),
            Some(&token),
            json!({ "contacts": ["sip:192.168.1.189", "sip:192.168.1.190"] }),
        ),
    )
    .await;
    assert_eq!(batch_del_resp.status(), StatusCode::NO_CONTENT);

    {
        let map = state.api_v3.scheduled_calls.lock().unwrap();
        assert_eq!(map.get(&room_id).unwrap().len(), 0);
    }

    // 11. Проверяем Role-Based Access Control (< Operator -> 403 Forbidden)
    crate::users::Users::create(&state, "step4_restricted_user", vks_core::Role::Moderator);
    let restricted_token =
        state.jwt_issuer.as_ref().unwrap().issue("step4_restricted_user", None).expect("issue token");

    let rbac_dtmf = send(
        &app_router,
        request("POST", &format!("/api/v3/abonents/{pid}/dtmf"), Some(&restricted_token), json!({ "sequence": "9" })),
    )
    .await;
    assert_eq!(rbac_dtmf.status(), StatusCode::FORBIDDEN);

    let rbac_settings = send(
        &app_router,
        request("POST", &format!("/api/v3/abonents/{pid}/audio"), Some(&restricted_token), json!({ "enable": false })),
    )
    .await;
    assert_eq!(rbac_settings.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn step6_conference_8_tabs_settings_persistence() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    // 1. Создаем конференцию с начальными настройками нескольких вкладок
    let create_resp = send(
        &app,
        request(
            "POST",
            "/api/v3/conferences",
            Some(&token),
            json!({
                "display_name": "Multi-Tab Conference",
                "pin": "4321",
                "description": "Initial Conference Description",
                "owner_id": "user-admin",
                "auto_call": true,
                "layout": "grid",
                "payload": {
                    "skin": "modern",
                    "codec": "OPUS",
                    "auto_record": true
                }
            }),
        ),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_body = json_body(create_resp).await;
    let conf = &create_body["conference"];
    let conf_id = conf["id"].as_str().unwrap().to_string();

    assert_eq!(conf["display_name"], "Multi-Tab Conference");
    assert_eq!(conf["pin"], "4321");
    assert_eq!(conf["has_pin"], true);
    assert_eq!(conf["description"], "Initial Conference Description");
    assert_eq!(conf["owner_id"], "u-admin", "Ownership comes from authenticated identity, not a client-supplied id");
    assert_eq!(conf["auto_call"], true);
    assert_eq!(conf["payload"]["skin"], "modern");
    assert_eq!(conf["payload"]["codec"], "OPUS");
    assert_eq!(conf["payload"]["auto_record"], true);

    // 2. Обновляем настройки 8 вкладок через PATCH /api/v3/conferences/{id}
    let patch_resp = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/conferences/{conf_id}"),
            Some(&token),
            json!({
                "display_name": "Updated 8-Tabs Conf",
                "pin": "9999",
                "description": "Updated Description for Tab 1",
                "layout": "mosaic-4",
                "skin": "dark-blue",
                "show_names": true,
                "frame_border": false,
                "codec": "G711U",
                "agc": true,
                "noise_suppression": true,
                "auto_record": false,
                "record_layout": "speaker-only",
                "storage_path": "/var/recordings/vks",
                "rtmp_url": "rtmp://live.broadcast.local/mcu/stream1",
                "hls_enabled": true,
                "stun_turn_server": "stun:192.168.1.93:3478",
                "sip_proxy": "192.168.1.132:5060",
                "payload": {
                    "custom_ui_accordion": "advanced",
                    "bitrate_preset": "hd"
                }
            }),
        ),
    )
    .await;
    assert_eq!(patch_resp.status(), StatusCode::OK);
    let patch_body = json_body(patch_resp).await;
    let patched_conf = &patch_body["conference"];

    assert_eq!(patched_conf["display_name"], "Updated 8-Tabs Conf");
    assert_eq!(patched_conf["pin"], "9999");
    assert_eq!(patched_conf["has_pin"], true);
    assert_eq!(patched_conf["description"], "Updated Description for Tab 1");
    assert_eq!(patched_conf["layout"], "mosaic-4");
    assert_eq!(patched_conf["skin"], "dark-blue");
    assert_eq!(patched_conf["show_names"], true);
    assert_eq!(patched_conf["frame_border"], false);
    assert_eq!(patched_conf["codec"], "G711U");
    assert_eq!(patched_conf["rtmp_url"], "rtmp://live.broadcast.local/mcu/stream1");
    assert_eq!(patched_conf["hls_enabled"], true);
    assert_eq!(patched_conf["stun_turn_server"], "stun:192.168.1.93:3478");
    assert_eq!(patched_conf["sip_proxy"], "192.168.1.132:5060");
    assert_eq!(patched_conf["payload"]["custom_ui_accordion"], "advanced");
    assert_eq!(patched_conf["payload"]["bitrate_preset"], "hd");

    // 3. Запрашиваем GET /api/v3/conferences/{id} и проверяем сохранение всех полей
    let get_resp = send(&app, request("GET", &format!("/api/v3/conferences/{conf_id}"), Some(&token), json!({}))).await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = json_body(get_resp).await;
    let fetched_conf = &get_body["conference"];

    assert_eq!(fetched_conf["display_name"], "Updated 8-Tabs Conf");
    assert_eq!(fetched_conf["pin"], "9999");
    assert_eq!(fetched_conf["layout"], "mosaic-4");
    assert_eq!(fetched_conf["rtmp_url"], "rtmp://live.broadcast.local/mcu/stream1");
    assert_eq!(fetched_conf["payload"]["custom_ui_accordion"], "advanced");

    // 4. Удаляем конференцию
    let del_resp =
        send(&app, request("DELETE", &format!("/api/v3/conferences/{conf_id}"), Some(&token), json!({}))).await;
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);

    let get_after_del =
        send(&app, request("GET", &format!("/api/v3/conferences/{conf_id}"), Some(&token), json!({}))).await;
    assert_eq!(get_after_del.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pnm_settings_video_is_durable_and_validated() {
    let storage = std::sync::Arc::new(vks_storage::memory::MemoryStorage::new());
    let (mut state, _) = test_state();
    std::sync::Arc::get_mut(&mut state).unwrap().storage = Some(storage.clone());
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;

    let response = send(&server, request("GET", "/api/v3/settings/video", Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["payload"]["max_frame_rate"], 30);

    let response = send(
        &server,
        request(
            "PATCH",
            "/api/v3/settings/video",
            Some(&token),
            json!({"payload":{"max_frame_rate":60,"layout_strict":"OFF"}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["payload"]["max_frame_rate"], 60);
    assert_eq!(body["payload"]["layout_strict"], "OFF");
    assert_eq!(body["payload"]["render_mode"], "AUTO", "частичный PATCH сохраняет соседей");

    for invalid in [json!({"payload":{"nested":{"x":1}}}), json!({"payload":"nope"})] {
        let response = send(&server, request("PATCH", "/api/v3/settings/video", Some(&token), invalid)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let (mut restored, _) = test_state();
    std::sync::Arc::get_mut(&mut restored).unwrap().storage = Some(storage);
    restored.sync_from_storage().await.unwrap();
    let server = app(restored);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(&server, request("GET", "/api/v3/settings/video", Some(&token), json!({}))).await;
    let body = json_body(response).await;
    assert_eq!(body["payload"]["max_frame_rate"], 60, "PATCH durable после рестарта");
    assert_eq!(body["payload"]["layout_strict"], "OFF");
}

#[tokio::test]
async fn pnm_conference_connections_limit_prefers_stored_settings() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/conferences",
            Some(&token),
            json!({"display_name":"Limit priority","payload":{"connections_limit":7}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["conference"]["payload"]["connections_limit"], 7, "лимит конференции приоритетнее системного");
}

#[tokio::test]
async fn pnm_session_inherits_conference_layouts_without_orphan() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name":"Inherit layouts"})))
            .await;
    let conference_id = json_body(response).await["conference"]["id"].as_str().unwrap().to_string();
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/ui/video_layouts",
            Some(&token),
            json!({"display_name":"Shared","payload":{"mode":"auto","type":"default"}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let layout_id = json_body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    let response = send(
        &server,
        request(
            "POST",
            &format!("/api/v3/conferences/{conference_id}/video_layouts/{layout_id}"),
            Some(&token),
            json!({}),
        ),
    )
    .await;
    assert!(response.status().is_success());

    let response =
        send(&server, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id":conference_id}))).await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = send(
        &server,
        request("GET", &format!("/api/v3/ui/sessions/{conference_id}/video_layouts"), Some(&token), json!({})),
    )
    .await;
    let layouts = json_body(response).await;
    assert_eq!(layouts["count"], 1, "сессия наследует раскладку конференции");
    assert_eq!(layouts["video_layouts"][0]["id"], layout_id);
    assert!(layouts["video_layouts"][0]["abonents"]["abonents"]["abonents"].is_array());

    let response = send(&server, request("GET", "/api/v3/ui/video_layouts", Some(&token), json!({}))).await;
    let global = json_body(response).await;
    assert_eq!(global["count"], 1, "старт сессии не оставляет orphan-раскладку");
}

#[tokio::test]
async fn pnm_video_layouts_calculate_parses_preset_suffix() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/ui/video_layouts/calculate",
            Some(&token),
            json!({"preset":"preset_selected_b_4_(2)","width":640,"height":360}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let slots = body["slots"].as_array().expect("slots array");
    assert_eq!(slots.len(), 4, "размер берётся из хвоста пресета, а не из «(2)»");
    // Эталонный `/ui/video_layouts/calculate` отдаёт позиции 1..N (проверено
    // live на TrueConf MCU 2.2.1.66), не 0..N-1.
    assert_eq!(slots[0]["position"], 1);
    assert_eq!(slots[3]["position"], 4);
    // `selected_b_4_(2)` — эталонная схема 5/9 + полоса 4/9, а не равномерная 2×2.
    let first_width = slots[0]["width"].as_f64().expect("ширина первого слота");
    let first_height = slots[0]["height"].as_f64().expect("высота первого слота");
    assert!((first_width - 640.0).abs() < 1e-6, "крупная зона занимает всю ширину: {first_width}");
    assert!((first_height - 200.0).abs() < 1e-6, "вариант _(2): floor(360*5/9)=200: {first_height}");
    assert!((slots[1]["height"].as_f64().unwrap() - 160.0).abs() < 1e-6, "полоса 4/9: 160");

    let response =
        send(&server, request("POST", "/api/v3/ui/video_layouts/calculate", None, json!({"preset":"preset_equal_25"})))
            .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Превью раскладок Codian: тот же контракт, что у вендорских manual-пресетов
/// (position 1-based, z_order для PiP/overlap), но из словаря `codian`.
#[tokio::test]
async fn codian_calculate_returns_slot_geometry_for_preview() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let id = "codian_custom_14";
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/ui/video_layouts/calculate",
            Some(&token),
            json!({"preset": id, "width": 1920, "height": 1080}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let slots = body["slots"].as_array().expect("slots array");
    let expected = vks_layout::codian::geometry(id, 1920.0, 1080.0).expect("геометрия Codian");
    assert_eq!(slots.len(), expected.len(), "окон столько же, сколько в таблице");
    assert_eq!(slots.len(), vks_layout::codian::slot_count(id).expect("число окон"));
    // Позиции 1-based, порядок совпадает с таблицей геометрии.
    for (index, (slot, cell)) in slots.iter().zip(expected.iter()).enumerate() {
        assert_eq!(slot["position"], index as u64 + 1, "позиции 1-based");
        assert!((slot["x"].as_f64().unwrap() - cell.rect.x).abs() < 1e-6, "x слота {index}");
        assert!((slot["y"].as_f64().unwrap() - cell.rect.y).abs() < 1e-6, "y слота {index}");
        assert!((slot["width"].as_f64().unwrap() - cell.rect.width).abs() < 1e-6, "ширина слота {index}");
        assert!((slot["height"].as_f64().unwrap() - cell.rect.height).abs() < 1e-6, "высота слота {index}");
        assert_eq!(slot["z_order"], u64::from(cell.z_order), "z_order слота {index}");
    }
    // Неизвестный id остаётся 400 (вендорская ошибка не изменилась) и
    // перечисляет параметрические id конструктора, а не только ручные пресеты.
    let response = send(
        &server,
        request("POST", "/api/v3/ui/video_layouts/calculate", Some(&token), json!({"preset": "codian_99"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = json_body(response).await;
    let description = error["error"]["details"][0]["description"].as_str().unwrap_or_default();
    assert!(description.contains("vks_grid_<n>"), "ошибка перечисляет vks_grid: {description}");
    assert!(description.contains("vks_pin_"), "ошибка перечисляет vks_pin: {description}");
}

/// Конструктор участников: `vks_grid_*` / `vks_pin_*` дают превью той же
/// геометрией, что MCU применит к холсту (`grids::geometry`), позиции 1-based.
#[tokio::test]
async fn constructor_calculate_matches_grid_geometry() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    for id in [
        "vks_grid_9",
        "vks_grid_20",
        "vks_pin_r_8",
        "vks_pin_b_13",
        "vks_grid_g12_m24_9",
        "vks_pin_l_g8_7",
        "vks_grid_100",
        "vks_pin_b_100",
    ] {
        let response = send(
            &server,
            request(
                "POST",
                "/api/v3/ui/video_layouts/calculate",
                Some(&token),
                json!({"preset": id, "width": 1280, "height": 720}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "превью для {id}");
        let body = json_body(response).await;
        let slots = body["slots"].as_array().expect("slots array");
        let expected = vks_layout::grids::geometry(id, 1280.0, 720.0).expect("геометрия конструктора");
        assert_eq!(slots.len(), expected.len(), "окон столько же у {id}");
        let windows = vks_layout::grids::parse(id).expect("id разобран").count;
        assert_eq!(slots.len(), windows, "число окон из id у {id}");
        for (index, (slot, cell)) in slots.iter().zip(expected.iter()).enumerate() {
            assert_eq!(slot["position"], index as u64 + 1, "позиции 1-based у {id}");
            assert!((slot["x"].as_f64().unwrap() - cell.rect.x).abs() < 1e-6, "x слота {index} у {id}");
            assert!((slot["height"].as_f64().unwrap() - cell.rect.height).abs() < 1e-6, "h слота {index} у {id}");
        }
    }
    // Опции библиотеки (зазор/поля) приходят из id и учитываются в превью.
    let response = send(
        &server,
        request(
            "POST",
            "/api/v3/ui/video_layouts/calculate",
            Some(&token),
            json!({"preset": "vks_grid_g12_m24_9", "width": 1280, "height": 720}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let slots = body["slots"].as_array().expect("slots array");
    let left = slots.iter().map(|slot| slot["x"].as_f64().unwrap()).fold(f64::MAX, f64::min);
    let top = slots.iter().map(|slot| slot["y"].as_f64().unwrap()).fold(f64::MAX, f64::min);
    assert!(left >= 23.0, "левое поле из id g12_m24: {left}");
    assert!(top >= 23.0, "верхнее поле из id g12_m24: {top}");
    for unknown in ["vks_grid_0", "vks_grid_9x9", "vks_pin_x_9", "vks_grid_9_g12"] {
        let response = send(
            &server,
            request("POST", "/api/v3/ui/video_layouts/calculate", Some(&token), json!({"preset": unknown})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "неизвестный id {unknown:?}");
    }
}

/// Все 54 раскладки Codian дают превью в канвасе; неизвестные id — по-прежнему 400.
#[tokio::test]
async fn every_codian_layout_has_preview_geometry() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    for id in vks_layout::codian::codian_preset_ids() {
        let response = send(
            &server,
            request(
                "POST",
                "/api/v3/ui/video_layouts/calculate",
                Some(&token),
                json!({"preset": id, "width": 1280, "height": 720}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "превью для {id}");
        let body = json_body(response).await;
        let slots = body["slots"].as_array().expect("slots array");
        let expected = vks_layout::codian::slot_count(id).expect("slot_count") as usize;
        assert_eq!(slots.len(), expected, "число окон {id}");
        let mut area = 0.0_f64;
        let (mut max_x, mut max_y) = (0.0_f64, 0.0_f64);
        for (index, slot) in slots.iter().enumerate() {
            assert_eq!(slot["position"], index as u64 + 1, "позиции 1-based у {id}");
            assert!(matches!(slot["z_order"].as_u64(), Some(0 | 1)), "z_order 0/1 у {id}");
            let (x, y) = (slot["x"].as_f64().unwrap(), slot["y"].as_f64().unwrap());
            let (width, height) = (slot["width"].as_f64().unwrap(), slot["height"].as_f64().unwrap());
            assert!(width > 0.0 && height > 0.0, "пустой слот у {id}");
            if slot["z_order"] == 0 {
                area += width * height;
                max_x = max_x.max(x + width);
                max_y = max_y.max(y + height);
            }
        }
        // Основные окна покрывают канвас 16:9 без щелей (PIP — наложенные, не в счёт).
        assert!((area - 1280.0 * 720.0).abs() < 2.0, "площадь основных окон у {id}: {area}");
        assert!((max_x - 1280.0).abs() < 0.5 && (max_y - 720.0).abs() < 0.5, "край канваса у {id}");
    }
    for unknown in ["codian_99", "codian_53", "codian_custom_15", ""] {
        let response = send(
            &server,
            request(
                "POST",
                "/api/v3/ui/video_layouts/calculate",
                Some(&token),
                json!({"preset": unknown, "width": 1280, "height": 720}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "неизвестный id {unknown:?} отвергнут");
    }
}

/// Каталог `/presets`: 65 вендорных + 54 Codian, id уникальны, `windows` = числу
/// окон макета, title непустой — это контракт для проектного слоя UI.
#[tokio::test]
async fn video_layout_presets_catalog_covers_vendor_and_codian() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(&server, request("GET", "/api/v3/ui/video_layouts/presets", Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let presets = body["presets"].as_array().expect("presets array");
    assert_eq!(presets.len(), 65 + 54, "65 вендорных manual + 54 Codian");
    let mut ids = std::collections::HashSet::new();
    for preset in presets {
        let id = preset["id"].as_str().expect("id");
        assert!(ids.insert(id.to_string()), "id {id} не должен повторяться");
        match preset["source"].as_str() {
            Some("vendor") => {}
            Some("codian") => {
                assert_eq!(preset["windows"].as_u64(), vks_layout::codian::slot_count(id).map(|n| n as u64));
                assert!(!preset["title"].as_str().unwrap_or_default().is_empty(), "пустой title у {id}");
                let slots = preset["slots"].as_array().expect("слоты каталога");
                assert_eq!(slots.len(), vks_layout::codian::slot_count(id).unwrap(), "слоты каталога = числу окон");
                for slot in slots {
                    for key in ["position", "x", "y", "width", "height", "z_order"] {
                        assert!(slot[key].is_number(), "поле {key} слота каталога у {id}");
                    }
                    assert!(slot["width"].as_f64().unwrap() > 0.0 && slot["height"].as_f64().unwrap() > 0.0);
                }
            }
            other => panic!("неизвестный source {other:?} у {id}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Счётчики контактов комнаты (`contacts` в карточке/списке конференций).
// ---------------------------------------------------------------------------

/// «Участники (SIP/H.323)» — ровно как считает вендорный UI (PNM 2.2.1.66):
/// `contacts.users + contacts.h323 + contacts.sip`.
fn ui_participants_count(contacts: &Value) -> u64 {
    ["users", "h323", "sip"].iter().map(|key| contacts[*key].as_u64().unwrap_or(0)).sum()
}

/// «Устройства (VNC/RTSP) и медиафайлы» — `rtsp + vnc + media + file`.
fn ui_devices_count(contacts: &Value) -> u64 {
    ["rtsp", "vnc", "media", "file"].iter().map(|key| contacts[*key].as_u64().unwrap_or(0)).sum()
}

/// Контакт комнаты в виде, в котором его кладёт `resources.contacts`.
fn contact_record(id: &str, contact_type: &str, uri: &str) -> Value {
    json!({
        "id": id, "contact_id": id, "call_string_id": id, "contact_type": contact_type,
        "display_name": id, "uri": uri, "connection_url": uri,
        "call_string": {"id": id, "uri": uri, "display_name": id},
        "payload": {"auto_call": false},
    })
}

#[test]
fn contact_counts_classify_contact_type_and_uri_scheme() {
    use super::resources::{contact_counts, contact_protocol, ContactProtocol};
    let contacts = vec![
        contact_record("cs-1", "call_strings", "sip:alice@example.invalid"),
        contact_record("cs-2", "call_strings", "SIP:Bob@Example.COM:5060"),
        contact_record("cs-3", "call_strings", "h323:192.168.1.132:1720"),
        contact_record("cs-4", "call_strings", "192.168.1.133:5060"),
        contact_record("cs-5", "users", "sip:carol@example.invalid"),
        contact_record("cs-6", "call_strings", "rtsp://192.168.1.134/stream"),
        contact_record("cs-7", "call_strings", "vnc:192.168.1.135:5900"),
        contact_record("cs-8", "call_strings", "media:192.168.1.136"),
        contact_record("cs-9", "call_strings", "file:/var/lib/record.mp4"),
        contact_record("cs-10", "alias", "alias:room@example.invalid"),
        contact_record("cs-11", "call_strings", "webrtc:room-1"),
    ];
    let counts = contact_counts(&contacts);
    assert_eq!(counts.sip, 3, "sip + SIP: + бесхостовый host:port");
    assert_eq!(counts.h323, 1);
    assert_eq!(counts.users, 1, "contact_type=users важнее схемы uri");
    assert_eq!(counts.rtsp, 1);
    assert_eq!(counts.vnc, 1);
    assert_eq!(counts.media, 1);
    assert_eq!(counts.file, 1);
    assert_eq!(counts.alias, 1);
    assert_eq!(contact_protocol(&contact_record("cs-12", "call_strings", "sips:a@b.invalid")), ContactProtocol::Sip);
    assert_eq!(contact_protocol(&contact_record("cs-13", "call_strings", "auto:host")), ContactProtocol::Sip);
    assert_eq!(contact_protocol(&contact_record("cs-14", "call_strings", "webrtc:x")), ContactProtocol::Other);
    // Пустая/неизвестная схема не попадает ни в один счётчик вендора.
    assert_eq!(contact_protocol(&contact_record("cs-15", "call_strings", "")), ContactProtocol::Other);

    // `payload.call_string` перечисляет ВСЕ опции протоколов (`sip/h323/rtsp/…`
    // с `primary: true`) — протокол контакта определяется только схемой `uri`.
    let mut live_shape = contact_record("cs-16", "call_strings", "sip:t12b-label-probe@example.invalid");
    live_shape["payload"]["call_string"] = json!({
        "sip": {"primary": true}, "h323": {"primary": true}, "rtsp": {"primary": true},
        "vnc": {"primary": true}, "media": {"primary": true}, "file": {"primary": true},
    });
    let live_counts = contact_counts(std::slice::from_ref(&live_shape));
    assert_eq!(live_counts.sip, 1, "боевой контакт комнаты — SIP");
    assert_eq!(live_counts.h323, 0);
    assert_eq!(live_counts.rtsp, 0, "опции протоколов не превращают контакт в устройство");
    assert_eq!(live_counts.vnc, 0);
    assert_eq!(live_counts.media, 0);
    assert_eq!(live_counts.file, 0);
    assert_eq!(live_counts.users, 0);
}

#[tokio::test]
async fn conference_contacts_counters_count_room_contacts() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(
        &server,
        request("POST", "/api/v3/conferences", Some(&token), json!({"display_name":"Contacts counters"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = json_body(response).await["conference"]["id"].as_str().expect("conference id").to_string();

    // Комната без контактов: все счётчики нулевые.
    let body = json_body(
        send(&server, request("GET", &format!("/api/v3/ui/conferences/{id}"), Some(&token), json!({}))).await,
    )
    .await;
    assert_eq!(body["conference"]["contacts"]["sip"], 0);
    assert_eq!(body["conference"]["contacts"]["users"], 0);

    // SIP- и H.323-контакты комнаты.
    for (uri, label) in [("sip:t12b-label-probe@example.invalid", "SIP probe"), ("h323:192.168.1.132", "H323 probe")] {
        let response = send(
            &server,
            request(
                "POST",
                &format!("/api/v3/ui/conferences/{id}/call_strings"),
                Some(&token),
                json!({"uri": uri, "display_name": label}),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED, "contact {uri} must be created");
    }

    // (а) sip=1, h323=1, users=0 — и одинаково в списке и в карточке.
    let card =
        json_body(send(&server, request("GET", &format!("/api/v3/conferences/{id}"), Some(&token), json!({}))).await)
            .await;
    let contacts = &card["conference"]["contacts"];
    assert_eq!(contacts["sip"], 1, "card contacts: {contacts}");
    assert_eq!(contacts["h323"], 1, "card contacts: {contacts}");
    assert_eq!(contacts["users"], 0, "card contacts: {contacts}");
    assert_eq!(ui_participants_count(contacts), 2, "UI «Участники (SIP/H.323)»: {contacts}");
    assert_eq!(ui_devices_count(contacts), 0);

    for (path, field) in [
        ("/api/v3/ui/conferences".to_string(), "conferences"),
        ("/api/v3/conferences".to_string(), "conferences"),
        ("/api/v3/ui/sessions".to_string(), "sessions"),
        ("/api/v3/sessions".to_string(), "sessions"),
    ] {
        let body = json_body(send(&server, request("GET", &path, Some(&token), json!({}))).await).await;
        let item = body[field]
            .as_array()
            .expect("list array")
            .iter()
            .find(|item| item["id"] == id.as_str())
            .unwrap_or_else(|| panic!("{path} must contain {id}: {body}"));
        assert_eq!(item["contacts"]["sip"], 1, "{path} contacts.sip: {}", item["contacts"]);
        assert_eq!(item["contacts"]["h323"], 1, "{path} contacts.h323: {}", item["contacts"]);
        assert_eq!(item["contacts"]["users"], 0, "{path} contacts.users: {}", item["contacts"]);
    }
}

#[tokio::test]
async fn conference_contacts_users_type_is_counted_as_user() {
    let (state, _) = test_state();
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name":"Users counter"})))
            .await;
    let id = json_body(response).await["conference"]["id"].as_str().expect("conference id").to_string();

    // (б) Контакт из справочника пользователей (`contact_type == "users"`).
    state
        .api_v3
        .resources
        .lock()
        .await
        .contacts
        .insert(id.clone(), vec![contact_record("u-ivan", "users", "sip:ivan@example.invalid")]);

    let body = json_body(
        send(&server, request("GET", &format!("/api/v3/ui/conferences/{id}"), Some(&token), json!({}))).await,
    )
    .await;
    let contacts = &body["conference"]["contacts"];
    assert_eq!(contacts["users"], 1, "contacts.users: {contacts}");
    assert_eq!(contacts["sip"], 0, "контакт users не считается sip: {contacts}");
    assert_eq!(ui_participants_count(contacts), 1, "UI «Участники (SIP/H.323)»: {contacts}");
}

#[tokio::test]
async fn conference_contacts_devices_are_counted_separately_from_participants() {
    let (state, _) = test_state();
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", "/api/v3/conferences", Some(&token), json!({"display_name":"Devices counter"})))
            .await;
    let id = json_body(response).await["conference"]["id"].as_str().expect("conference id").to_string();

    // (в) Устройства и медиафайлы — отдельная вкладка UI.
    let devices = vec![
        contact_record("cs-vnc", "call_strings", "vnc:192.168.1.135:5900"),
        contact_record("cs-rtsp", "call_strings", "rtsp://192.168.1.134/stream"),
        contact_record("cs-media", "call_strings", "media:192.168.1.136"),
        contact_record("cs-file", "call_strings", "file:/var/lib/record.mp4"),
    ];
    state.api_v3.resources.lock().await.contacts.insert(id.clone(), devices);

    let body = json_body(
        send(&server, request("GET", &format!("/api/v3/ui/conferences/{id}"), Some(&token), json!({}))).await,
    )
    .await;
    let conference = &body["conference"];
    let contacts = &conference["contacts"];
    assert_eq!(contacts["vnc"], 1, "contacts.vnc: {contacts}");
    assert_eq!(contacts["rtsp"], 1, "contacts.rtsp: {contacts}");
    assert_eq!(contacts["media"], 1, "contacts.media: {contacts}");
    assert_eq!(contacts["file"], 1, "contacts.file: {contacts}");
    assert_eq!(ui_devices_count(contacts), 4, "UI «Устройства (VNC/RTSP) и медиафайлы»: {contacts}");
    assert_eq!(ui_participants_count(contacts), 0, "устройства не попадают в «Участники»: {contacts}");
    // Участники комнаты (живые) и абоненты не меняются от контактов-устройств.
    assert_eq!(conference["participants"], 0, "устройства не подключены: {conference}");
    assert_eq!(conference["abonents"]["sip"], 0, "живых SIP-абонентов нет: {}", conference["abonents"]);
}

#[tokio::test]
async fn conference_contacts_counters_do_not_depend_on_session_state() {
    let (state, _) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let response = send(
        &server,
        request("POST", "/api/v3/conferences", Some(&token), json!({"display_name":"State independent"})),
    )
    .await;
    let id = json_body(response).await["conference"]["id"].as_str().expect("conference id").to_string();
    let response = send(
        &server,
        request(
            "POST",
            &format!("/api/v3/ui/conferences/{id}/call_strings"),
            Some(&token),
            json!({"uri":"sip:state-probe@example.invalid","display_name":"Probe"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // (г) Неактивированная комната.
    let body = json_body(
        send(&server, request("GET", &format!("/api/v3/ui/conferences/{id}"), Some(&token), json!({}))).await,
    )
    .await;
    let inactive = body["conference"]["contacts"].clone();
    assert_eq!(inactive["sip"], 1, "неактивированная комната: {inactive}");
    assert_eq!(body["conference"]["has_stopped"], true, "комната ещё не запущена");

    // Активированная комната: те же счётчики (живые — отдельно в abonents).
    let response = send(&server, request("POST", "/api/v3/sessions", Some(&token), json!({"conference_id": id}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(
        send(&server, request("GET", &format!("/api/v3/ui/conferences/{id}"), Some(&token), json!({}))).await,
    )
    .await;
    let active = body["conference"]["contacts"].clone();
    assert_eq!(active, inactive, "счётчики контактов не зависят от активности комнаты");
    assert_eq!(active["sip"], 1, "активированная комната: {active}");
    assert_eq!(body["conference"]["has_stopped"], false, "комната запущена");

    // Остановленная комната: счётчики те же.
    let response = send(&server, request("DELETE", &format!("/api/v3/sessions/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let body = json_body(
        send(&server, request("GET", &format!("/api/v3/ui/conferences/{id}"), Some(&token), json!({}))).await,
    )
    .await;
    assert_eq!(body["conference"]["contacts"], inactive, "после остановки счётчики те же");
    assert_eq!(body["conference"]["has_stopped"], true, "комната остановлена");
}

/// T7: RBAC карточек/списков (Viewer не видит PIN), PATCH /profile и
/// PATCH /sessions/{id} (exact и slash) в envelope, 409 на дубли имён.
#[tokio::test]
async fn t7_rbac_patches_and_duplicate_names() {
    let (state, issuer) = test_state();
    let viewer = crate::users::Users::create(&state, "t7_viewer", vks_core::Role::Viewer);
    let viewer_token = issuer.issue(&viewer.id.to_string(), None).expect("issue viewer");
    let app = app(state.clone());
    let admin = login_token(&app, "admin", "s3cret").await;

    let created = send(
        &app,
        request("POST", "/api/v3/conferences", Some(&admin), json!({"display_name":"T7 room","pin":"246813"})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created_body = json_body(created).await;
    let room_id = created_body["conference"]["id"].as_str().expect("conference id").to_string();

    // Anon: 401 envelope.
    let anon = send(&app, request("GET", &format!("/api/v3/conferences/{room_id}"), None, json!({}))).await;
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // Viewer: карточка конференции и все 4 списка — 403 (и PIN недоступен).
    for uri in [
        format!("/api/v3/conferences/{room_id}"),
        format!("/api/v3/ui/conferences/{room_id}"),
        "/api/v3/conferences".to_string(),
        "/api/v3/ui/conferences".to_string(),
        "/api/v3/sessions".to_string(),
        "/api/v3/ui/sessions".to_string(),
    ] {
        let response = send(&app, request("GET", &uri, Some(&viewer_token), json!({}))).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "Viewer {uri} должен быть 403");
    }

    // Admin: карточка 200 и PIN на месте (гейт, а не удаление поля).
    let card = send(&app, request("GET", &format!("/api/v3/conferences/{room_id}"), Some(&admin), json!({}))).await;
    assert_eq!(card.status(), StatusCode::OK);
    let card_body = json_body(card).await;
    assert_eq!(card_body["conference"]["pin"], "246813");
    assert_eq!(card_body["conference"]["has_pin"], true);

    // PATCH /profile: 200, записанные поля читаются из GET; role игнорируется.
    let patch = send(
        &app,
        request(
            "PATCH",
            "/api/v3/profile",
            Some(&admin),
            json!({"display_name":"T7 admin","emails":["ops@example.test"],"locale_id":"en_US","role":"viewer"}),
        ),
    )
    .await;
    assert_eq!(patch.status(), StatusCode::OK);
    let patch_body = json_body(patch).await;
    assert_eq!(patch_body["user"]["display_name"], "T7 admin");
    assert_ne!(patch_body["user"]["roles"][0]["display_name"], "Viewer", "self-PATCH не меняет роль");
    let profile = send(&app, request("GET", "/api/v3/profile", Some(&admin), json!({}))).await;
    let profile_body = json_body(profile).await;
    assert_eq!(profile_body["user"]["emails"][0], "ops@example.test");
    assert_eq!(profile_body["user"]["locale_id"], "en_US");
    // Slash-форма тоже реализована (раньше 501 из catch-all).
    let slash =
        send(&app, request("PATCH", "/api/v3/profile/", Some(&admin), json!({"display_name":"T7 admin 2"}))).await;
    assert_eq!(slash.status(), StatusCode::OK);
    let slash_body = json_body(slash).await;
    assert_eq!(slash_body["user"]["display_name"], "T7 admin 2");

    // Viewer может править свой профиль (это не админская операция).
    let viewer_patch =
        send(&app, request("PATCH", "/api/v3/profile", Some(&viewer_token), json!({"display_name":"V"}))).await;
    assert_eq!(viewer_patch.status(), StatusCode::OK);

    // PATCH /sessions/{id}: 200 (exact и slash), имя меняется; Viewer — 403.
    for uri in [format!("/api/v3/sessions/{room_id}"), format!("/api/v3/ui/sessions/{room_id}/")] {
        let response =
            send(&app, request("PATCH", &uri, Some(&admin), json!({"display_name":"T7 renamed", "duration": 600})))
                .await;
        assert_eq!(response.status(), StatusCode::OK, "{uri} должен быть 200");
    }
    let after = send(&app, request("GET", &format!("/api/v3/conferences/{room_id}"), Some(&admin), json!({}))).await;
    let after_body = json_body(after).await;
    assert_eq!(after_body["conference"]["display_name"], "T7 renamed");
    assert_eq!(after_body["conference"]["duration"], 600);
    let viewer_patch_session = send(
        &app,
        request("PATCH", &format!("/api/v3/sessions/{room_id}"), Some(&viewer_token), json!({"display_name":"x"})),
    )
    .await;
    assert_eq!(viewer_patch_session.status(), StatusCode::FORBIDDEN);

    // 409: дубль имени при создании и при переименовании, тело — envelope.
    let dup =
        send(&app, request("POST", "/api/v3/conferences", Some(&admin), json!({"display_name":"T7 renamed"}))).await;
    assert_eq!(dup.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(dup).await["error"]["code"], 409);
    let second =
        send(&app, request("POST", "/api/v3/conferences", Some(&admin), json!({"display_name":"T7 other"}))).await;
    assert_eq!(second.status(), StatusCode::OK);
    let second_body = json_body(second).await;
    let other_id = second_body["conference"]["id"].as_str().expect("id").to_string();
    let rename_dup = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/conferences/{other_id}"),
            Some(&admin),
            json!({"display_name":"T7 renamed"}),
        ),
    )
    .await;
    assert_eq!(rename_dup.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(rename_dup).await["error"]["code"], 409);
}
