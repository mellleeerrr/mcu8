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

use super::router;
use crate::state::{AppState, SharedState};

/// Состояние с issuer + LocalDb(`admin`/`s3cret`), как бутстрап `main.rs`.
fn test_state() -> (SharedState, Arc<JwtIssuer>) {
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
    // UI-поиск пользователей (`ui:users:search`) читает admin-хранилище (§4.4);
    // bootstrap `main.rs` наполняет его тем же администратором.
    crate::users::Users::create(&state, "admin", vks_core::Role::Operator);
    (Arc::new(state), issuer)
}

fn app(state: SharedState) -> Router {
    router(state)
}

fn request(method: &str, uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_string())).expect("build request")
}

async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.expect("oneshot")
}

async fn json_body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

async fn login(app: &Router, user: &str, password: &str) -> Response {
    send(app, request("POST", "/api/v3/login", None, json!({ "login": user, "password": password }))).await
}

async fn login_token(app: &Router, user: &str, password: &str) -> String {
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

    // Контракт карточек из бандла: счётчики-числа обязаны быть объектами,
    // иначе Vue падает на деструктуризации `entity.abonents`/`entity.contacts`.
    for key in ["sip", "h323", "rtsp", "vnc", "media", "file"] {
        assert!(body["conference"]["abonents"][key].is_number(), "abonents.{key}: {}", body["conference"]);
    }
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
    assert_eq!(payload["password_ttl"], 0);
    assert_eq!(payload["password_ttl_enabled"], false);
    assert_eq!(payload["login_max_attempts_count"], super::auth::USER_IP_MAX_FAILS);
    assert_eq!(payload["login_block_timeout"], super::auth::BLOCK_SECS);
    assert_eq!(payload["login_block_enabled"], true);
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
    assert!(body["disk"]["free"].as_u64().unwrap_or(0) > 1_000_000_000, "disk free must be > 1GB");

    // 2. C2: settings_calls must return payload wrapper with fecc/aspeaker and support PATCH
    let resp = send(&app, request("GET", "/api/v3/settings/calls", Some(&token), json!({}))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["payload"].is_object(), "payload wrapper is required for Cne (C2)");
    assert_eq!(body["payload"]["fecc"], false);
    assert_eq!(body["payload"]["aspeaker"], true);

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

    // 5. C11: license, diagnostics, calibration must have expected nested structures
    let resp = send(&app, request("GET", "/api/v3/license", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert!(body["params"]["protocol"]["max_calls"].is_number(), "license params required (C11)");

    let resp = send(&app, request("GET", "/api/v3/engine/diagnostics", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["diagnostics"]["vsm"]["limits"]["performance"], 100, "diagnostics required (C11)");

    let resp = send(&app, request("GET", "/api/v3/engine/calibration", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["calibration"]["state"], "IDLE", "calibration required (C11)");

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
    assert_eq!(abonents_body["connected"][0]["payload"]["settings"]["audio_tx_enabled"], true);
    assert_eq!(abonents_body["connected"][0]["payload"]["settings"]["video_tx_enabled"], true);

    // 4. Mute Audio
    let mute_audio =
        send(&app, request("POST", &format!("/api/v3/abonents/{pid}/audio"), Some(&token), json!({ "enable": false })))
            .await;
    assert_eq!(mute_audio.status(), StatusCode::OK);

    let check_mute_a =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/abonents"), Some(&token), json!({}))).await;
    let body_mute_a = json_body(check_mute_a).await;
    assert_eq!(body_mute_a["connected"][0]["payload"]["settings"]["audio_tx_enabled"], false);

    // 5. Mute Video
    let mute_video =
        send(&app, request("POST", &format!("/api/v3/abonents/{pid}/video"), Some(&token), json!({ "enable": false })))
            .await;
    assert_eq!(mute_video.status(), StatusCode::OK);

    let check_mute_v =
        send(&app, request("GET", &format!("/api/v3/sessions/{room_id_str}/abonents"), Some(&token), json!({}))).await;
    let body_mute_v = json_body(check_mute_v).await;
    assert_eq!(body_mute_v["connected"][0]["payload"]["settings"]["video_tx_enabled"], false);

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

    // 7. Video layout mode switch
    let mode_switch = send(
        &app,
        request(
            "POST",
            "/api/v3/ui/video_layouts/mode",
            Some(&token),
            json!({
                "session_id": room_id_str,
                "new_mode": "mcu",
                "video_layout_id": "grid"
            }),
        ),
    )
    .await;
    assert_eq!(mode_switch.status(), StatusCode::OK);
    let mode_body = json_body(mode_switch).await;
    assert_eq!(mode_body["payload"]["mode"], "mcu");

    // 8. Engine command
    let eng_resp =
        send(&app, request("POST", "/api/v3/engine/command", Some(&token), json!({ "command": "calibration" }))).await;
    assert_eq!(eng_resp.status(), StatusCode::OK);

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
    assert_eq!(status_body["sip"]["status"], "READY");
    assert_eq!(status_body["sip"]["connected"], true);
    assert_eq!(status_body["h323"]["status"], "READY");
    assert_eq!(status_body["h323"]["connected"], true);
    assert_eq!(status_body["cluster"]["status"], "READY");
    assert_eq!(status_body["cluster"]["connected"], true);
    assert_eq!(status_body["cluster"]["lease_valid"], true);

    // 6. Inject SIP fault
    state.inject_fault("sip");
    let resp = send(&app_router, request("GET", "/api/v3/ui/system/status", Some(&token), json!({}))).await;
    let body = json_body(resp).await;
    assert_eq!(body["sip"]["status"], "FAILED");
    assert_eq!(body["sip"]["connected"], false);
    assert_eq!(body["h323"]["status"], "READY");
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

#[tokio::test]
async fn step2_call_strings_and_invite_vertical() {
    let (state, _) = test_state();
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
    assert_eq!(cs_conf_resp.status(), StatusCode::OK);
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
    assert_eq!(stray_body["count"], 0);

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
    assert_eq!(standby_resp.status(), StatusCode::OK);

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
    assert_eq!(abonents_body["scheduled"][0]["payload"]["settings"]["audio_tx_enabled"], true);

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
    assert_eq!(dial_resp.status(), StatusCode::OK);
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
}
