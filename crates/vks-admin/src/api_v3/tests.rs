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
