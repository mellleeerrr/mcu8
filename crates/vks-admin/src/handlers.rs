// Handlers — axum Router + HTTP handlers (фаза 5).
//
// API versioning (BACKEND_RULES §7): `/api/v1/...`.
// Deprecation: `Deprecation: true` + `Sunset: <date>` headers на
// deprecated endpoints (эта волна — все v1 активны).
//
// Тесты: service-функции тестируются в модулях каждого сервиса;
// здесь — route-mapping тесты через `tower::ServiceExt::oneshot`.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use uuid::Uuid;

use vks_core::{Role, RoomId, RoomMode, UserId};

use crate::{
    audit::{AuditEntry, AuditFilter},
    health::{Liveness, Readiness},
    mode::ModeService,
    rooms::Rooms,
    settings::SettingsService,
    state::AppState,
    users::{MockLdapProvider, Users},
};

/// Тип-алиас для state на уровне Router.
pub type SharedState = Arc<AppState>;

// ---------------------------------------------------------------------------
// Request types (JSON, serde).
// ---------------------------------------------------------------------------

/// Body: `POST /api/v1/rooms`.
#[derive(Debug, Deserialize)]
pub struct CreateRoomRequest {
    /// Название комнаты.
    pub name: String,
}

/// Body: `PATCH /api/v1/rooms/{id}` (all fields optional).
#[derive(Debug, Default, Deserialize)]
pub struct UpdateRoomRequest {
    pub name: Option<String>,
    pub mode: Option<RoomMode>,
    pub participants: Option<usize>,
    pub bitrate_kbps: Option<u32>,
}

/// Body: `POST /api/v1/rooms/{id}/mode` (per-room mode, ADR-0026).
#[derive(Debug, Deserialize)]
pub struct SetRoomModeRequest {
    pub mode: RoomMode,
}

/// Body: `POST /api/v1/mode` (global mode, ADR-0026).
#[derive(Debug, Deserialize)]
pub struct SetGlobalModeRequest {
    pub mode: RoomMode,
}

/// Body: `POST /api/v1/users`.
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub role: Role,
}

/// Body: `PATCH /api/v1/users/{id}`.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateUserRequest {
    pub username: Option<String>,
    pub role: Option<Role>,
}

/// Bulk action (mute/unmute/kick).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkAction {
    Mute,
    Unmute,
    Kick,
}

/// Body: `POST /api/v1/users/bulk` (mute/kick).
#[derive(Debug, Deserialize)]
pub struct BulkUsersRequest {
    pub action: BulkAction,
    /// Целевые userId (string UUID).
    pub user_ids: Vec<String>,
}

/// Body: `POST /api/v1/settings`.
#[derive(Debug, Deserialize)]
pub struct SetSettingsRequest {
    pub settings: crate::settings::Settings,
}

/// Body: `POST /api/v1/audit`.
#[derive(Debug, Deserialize)]
pub struct RecordAuditRequest {
    pub entry: AuditEntry,
}

/// Query: `GET /api/v1/audit?user=&action=&ip=&from_ms=&to_ms=`.
#[derive(Debug, Default, Deserialize)]
pub struct AuditQuery {
    pub user: Option<String>,
    pub action: Option<String>,
    pub ip: Option<String>,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
}

impl AuditQuery {
    /// Преобразует в [`AuditFilter`].
    pub fn into_filter(self) -> AuditFilter {
        AuditFilter { user: self.user, action: self.action, ip: self.ip, from_ms: self.from_ms, to_ms: self.to_ms }
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn parse_room_id(s: &str) -> Result<RoomId, (StatusCode, String)> {
    let u = Uuid::parse_str(s).map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid room_id: {e}")))?;
    Ok(RoomId::from_uuid(u))
}

fn parse_user_id(s: &str) -> Result<UserId, (StatusCode, String)> {
    let u = Uuid::parse_str(s).map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid user_id: {e}")))?;
    Ok(UserId::from_uuid(u))
}

/// Оборачивает `T: Serialize` в Json response.
fn json_ok<T: serde::Serialize>(value: T) -> Response {
    (StatusCode::OK, Json(serde_json::to_value(value).unwrap_or(serde_json::Value::Null))).into_response()
}

/// 404 с `error_code` (BACKEND_RULES §6.4).
fn not_found(entity: &str, id: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error_code": "not_found", "entity": entity, "id": id })))
        .into_response()
}

/// Код envelope для router-отказов (прочие статусы — не наши: не трогаем).
fn envelope_code(status: StatusCode) -> Option<&'static str> {
    match status {
        StatusCode::METHOD_NOT_ALLOWED => Some("method_not_allowed"),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => Some("unsupported_media_type"),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => Some("invalid_body"),
        _ => None,
    }
}

/// Текст `error` по умолчанию, когда исходное тело пустое.
fn default_error_text(status: StatusCode) -> &'static str {
    match status {
        StatusCode::METHOD_NOT_ALLOWED => "method not allowed for this endpoint",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "expected application/json body",
        _ => "invalid request body",
    }
}

/// Единый error-envelope для router-отказов (BACKEND_RULES §6.4, T1.4):
/// axum-отказы 405/415 и ошибки парсинга тела 400/422 отдаются JSON-конвертом
/// `{error_code, error}` вместо text/plain. Заголовки исходного ответа
/// сохраняются (в т.ч. `Allow` у 405 — обязателен по RFC 9110 §15.5.6), текст
/// исходного тела переносится в `error`; JSON-ответы не перетираются.
async fn error_envelope_middleware(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let Some(error_code) = envelope_code(response.status()) else {
        return response;
    };
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if is_json {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let raw = axum::body::to_bytes(body, 4096).await.unwrap_or_default();
    let detail = String::from_utf8_lossy(&raw).trim().to_string();
    let error = if detail.is_empty() { default_error_text(parts.status).to_string() } else { detail };
    let payload = serde_json::json!({ "error_code": error_code, "error": error }).to_string();
    parts.headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    parts.headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&payload.len().to_string()).unwrap_or(HeaderValue::from_static("0")),
    );
    Response::from_parts(parts, Body::from(payload))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// `GET /healthz` — liveness (always 200).
pub async fn healthz() -> Response {
    let mode = Liveness::alive();
    (StatusCode::from_u16(mode.status_code()).unwrap(), Json(serde_json::to_value(mode).unwrap())).into_response()
}

/// `GET /readyz` — readiness (BACKEND_RULES §4.3): реальный ping DB + media.
pub async fn readyz(State(state): State<SharedState>) -> Response {
    let r = Readiness::from_state(&state).await;
    (StatusCode::from_u16(r.status_code()).unwrap(), Json(serde_json::to_value(r).unwrap())).into_response()
}

/// `GET /openapi.json` — stub OpenAPI (utoipa behind `openapi` feature).
pub async fn openapi() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": { "title": "VKS Admin API", "version": "1.0.0" },
        "paths": {
            "/api/v1/rooms": { "get": {}, "post": {} },
            "/api/v1/rooms/{id}": { "get": {}, "patch": {}, "delete": {} },
            "/api/v1/rooms/{id}/mode": { "post": {} },
            "/api/v1/mode": { "post": {} },
            "/api/v1/users": { "get": {}, "post": {} },
            "/api/v1/users/{id}": { "get": {}, "patch": {}, "delete": {} },
            "/api/v1/users/bulk": { "post": {} },
            "/api/v1/settings": { "get": {}, "post": {} },
            "/api/v1/audit": { "get": {}, "post": {} },
            "/api/v1/audit/export": { "get": {} }
        }
    }))
}

/// `POST /api/v1/rooms` — create room.
pub async fn create_room(State(state): State<SharedState>, Json(body): Json<CreateRoomRequest>) -> Response {
    json_ok(Rooms::create(&state, body.name))
}

/// `GET /api/v1/rooms` — list rooms.
pub async fn list_rooms(State(state): State<SharedState>) -> Response {
    json_ok(Rooms::list(&state))
}

/// `GET /api/v1/rooms/{id}` — get room.
pub async fn get_room(State(state): State<SharedState>, Path(id_str): Path<String>) -> Response {
    let id = match parse_room_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match Rooms::get(&state, &id) {
        Some(record) => json_ok(record),
        None => not_found("room", &id_str),
    }
}

/// `PATCH /api/v1/rooms/{id}` — update room.
pub async fn update_room(
    State(state): State<SharedState>,
    Path(id_str): Path<String>,
    Json(body): Json<UpdateRoomRequest>,
) -> Response {
    let id = match parse_room_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match Rooms::update(&state, &id, body.name, body.mode, body.participants, body.bitrate_kbps) {
        Some(record) => json_ok(record),
        None => not_found("room", &id_str),
    }
}

/// `DELETE /api/v1/rooms/{id}` — delete room.
pub async fn delete_room(State(state): State<SharedState>, Path(id_str): Path<String>) -> Response {
    let id = match parse_room_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    if Rooms::delete(&state, &id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found("room", &id_str)
    }
}

/// `GET /api/v1/rooms/{id}/live` — live view (Observer, read-only).
pub async fn room_live_view(State(state): State<SharedState>, Path(id_str): Path<String>) -> Response {
    let id = match parse_room_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match Rooms::live_view(&state, &id) {
        Some(view) => json_ok(view),
        None => not_found("room", &id_str),
    }
}

/// `POST /api/v1/mode` — set global mode (Operator only, ADR-0022/0026).
pub async fn set_global_mode(State(state): State<SharedState>, Json(body): Json<SetGlobalModeRequest>) -> Response {
    let (old, new) = ModeService::set_global_mode(&state, body.mode);
    json_ok(serde_json::json!({ "old_mode": old, "new_mode": new }))
}

/// `POST /api/v1/rooms/{id}/mode` — set per-room mode (ADR-0026, Q7).
pub async fn set_room_mode(
    State(state): State<SharedState>,
    Path(id_str): Path<String>,
    Json(body): Json<SetRoomModeRequest>,
) -> Response {
    let id = match parse_room_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match ModeService::set_room_mode(&state, &id, body.mode) {
        Ok((old, new)) => json_ok(serde_json::json!({ "old_mode": old, "new_mode": new })),
        Err(e) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error_code": "not_found", "error": e.to_string() })))
                .into_response()
        }
    }
}

/// `GET /api/v1/rooms/{id}/mode` — effective mode (override else global).
pub async fn get_room_mode(State(state): State<SharedState>, Path(id_str): Path<String>) -> Response {
    let id = match parse_room_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match ModeService::effective_mode(&state, &id) {
        Ok(mode) => json_ok(serde_json::json!({ "mode": mode })),
        Err(e) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error_code": "not_found", "error": e.to_string() })))
                .into_response()
        }
    }
}

/// `POST /api/v1/users` — create user (Operator only).
pub async fn create_user(State(state): State<SharedState>, Json(body): Json<CreateUserRequest>) -> Response {
    json_ok(Users::create(&state, body.username, body.role))
}

/// `GET /api/v1/users` — list users (active).
pub async fn list_users(State(state): State<SharedState>) -> Response {
    json_ok(Users::list(&state))
}

/// `GET /api/v1/users/{id}` — get user.
pub async fn get_user(State(state): State<SharedState>, Path(id_str): Path<String>) -> Response {
    let id = match parse_user_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match Users::get(&state, &id) {
        Some(record) => json_ok(record),
        None => not_found("user", &id_str),
    }
}

/// `PATCH /api/v1/users/{id}` — update user.
pub async fn update_user(
    State(state): State<SharedState>,
    Path(id_str): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> Response {
    let id = match parse_user_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match Users::update(&state, &id, body.username, body.role) {
        Some(record) => json_ok(record),
        None => not_found("user", &id_str),
    }
}

/// `DELETE /api/v1/users/{id}` — soft-delete (GDPR 152-ФЗ).
pub async fn delete_user(State(state): State<SharedState>, Path(id_str): Path<String>) -> Response {
    let id = match parse_user_id(&id_str) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    if Users::delete(&state, &id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found("user", &id_str)
    }
}

/// `POST /api/v1/users/bulk` — bulk mute/unmute/kick.
pub async fn bulk_users(State(state): State<SharedState>, Json(body): Json<BulkUsersRequest>) -> Response {
    let ids: Vec<UserId> =
        body.user_ids.iter().filter_map(|s| Uuid::parse_str(s).ok().map(UserId::from_uuid)).collect();
    let touched = match body.action {
        BulkAction::Mute => Users::bulk_mute(&state, &ids, true),
        BulkAction::Unmute => Users::bulk_mute(&state, &ids, false),
        BulkAction::Kick => Users::bulk_kick(&state, &ids),
    };
    json_ok(serde_json::json!({ "touched": touched }))
}

/// `POST /api/v1/users/sync-ldap` — LDAP sync (mock; real in vks-auth).
pub async fn sync_ldap(State(state): State<SharedState>) -> Response {
    let ldap = MockLdapProvider::fetch();
    let total = Users::sync_ldap(&state, ldap);
    json_ok(serde_json::json!({ "total": total }))
}

/// `GET /api/v1/settings` — get settings (secrets redacted).
pub async fn get_settings(State(state): State<SharedState>) -> Response {
    json_ok(SettingsService::get(&state).redacted())
}

/// `POST /api/v1/settings` — set settings (hot-reload).
pub async fn set_settings(State(state): State<SharedState>, Json(body): Json<SetSettingsRequest>) -> Response {
    json_ok(SettingsService::set(&state, body.settings.clone()))
}

/// `POST /api/v1/audit` — record an audit entry (append-only).
pub async fn record_audit(
    State(state): State<SharedState>,
    auth: Option<axum::extract::Extension<vks_auth::jwt::VerifiedJwt>>,
    Json(body): Json<RecordAuditRequest>,
) -> Response {
    let mut entry = body.entry;
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    // Strict server anti-spoofing: server timestamp is unconditionally authoritative
    entry.ts_ms = now;
    if let Some(axum::extract::Extension(jwt)) = auth {
        entry.user = jwt.user_id;
    }
    state.audit.lock().expect("audit store poisoned").record(entry.clone());
    json_ok(serde_json::json!({ "recorded": true, "ts_ms": entry.ts_ms }))
}

/// `GET /api/v1/audit?user=&action=&ip=&from_ms=&to_ms=` — filter.
pub async fn list_audit(State(state): State<SharedState>, Query(query): Query<AuditQuery>) -> Response {
    let filter = query.into_filter();
    let entries: Vec<AuditEntry> =
        state.audit.lock().expect("audit store poisoned").filter(&filter).into_iter().map(|e| (*e).clone()).collect();
    json_ok(entries)
}

/// `GET /api/v1/audit/export?user=&action=&ip=&from_ms=&to_ms=` — CSV export.
pub async fn export_audit(State(state): State<SharedState>, Query(query): Query<AuditQuery>) -> Response {
    let filter = query.into_filter();
    let csv = state.audit.lock().expect("audit store poisoned").export_csv(&filter);
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/csv; charset=utf-8")], csv).into_response()
}

// ---------------------------------------------------------------------------
// Router.
// ---------------------------------------------------------------------------

/// Строит `Router` для admin API (`/api/v1/...` + `/healthz`, `/readyz`,
/// `/openapi.json`).
///
/// API versioning (BACKEND_RULES §7): major version в path.
/// Deprecation: `Deprecation`/`Sunset` headers на deprecated endpoints
/// (эта волна — все v1 активны).
pub fn router(state: SharedState) -> Router {
    let public_routes = Router::new()
        // Health (no version).
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/openapi.json", get(openapi));

    let protected_v1 = Router::new()
        // Rooms (v1).
        .route("/api/v1/rooms", get(list_rooms).post(create_room))
        .route("/api/v1/rooms/{id}", get(get_room).patch(update_room).delete(delete_room))
        .route("/api/v1/rooms/{id}/live", get(room_live_view))
        .route("/api/v1/rooms/{id}/mode", get(get_room_mode).post(set_room_mode))
        // Mode (v1, global).
        .route("/api/v1/mode", post(set_global_mode))
        // Users (v1).
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/users/{id}", get(get_user).patch(update_user).delete(delete_user))
        .route("/api/v1/users/bulk", post(bulk_users))
        .route("/api/v1/users/sync-ldap", post(sync_ldap))
        // Settings (v1).
        .route("/api/v1/settings", get(get_settings).post(set_settings))
        // Audit (v1).
        .route("/api/v1/audit", get(list_audit).post(record_audit))
        .route("/api/v1/audit/export", get(export_audit))
        // Порядок route_layer: последний добавленный — внешний, поэтому JWT
        // выполняется первым и кладёт VerifiedJwt, RBAC читает его следом.
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::auth_middleware::v1_rbac_middleware))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::auth_middleware::jwt_auth_middleware));

    public_routes.merge(protected_v1).layer(axum::middleware::from_fn(error_envelope_middleware)).with_state(state)
}

// ---------------------------------------------------------------------------
// Route-mapping tests (tower::ServiceExt::via).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;
    use tower::ServiceExt;
    use vks_auth::jwt::{JwtConfig, JwtIssuer};

    fn test_router() -> Router {
        router(Arc::new(AppState::new()))
    }

    /// Приложение с валидным токеном; запись в Users-хранилище — по желанию
    /// (`None` = «токен есть, аккаунта нет» → RBAC должен отклонять).
    fn authed_app(username: &str, role: Option<vks_core::Role>) -> (Router, Arc<AppState>, String) {
        let mut state = AppState::new();
        let config = JwtConfig {
            secret: "test_secret_for_handlers_12345".to_string(),
            ttl_secs: 300,
            issuer: "vks-test".to_string(),
        };
        let issuer = Arc::new(JwtIssuer::new(config));
        let token = issuer.issue(username, None).expect("issue token");
        state.jwt_issuer = Some(issuer);
        if let Some(role) = role {
            crate::users::Users::create(&state, username, role);
        }
        let shared = Arc::new(state);
        (router(shared.clone()), shared, token)
    }

    fn authed_test_app() -> (Router, Arc<AppState>, String) {
        authed_app("admin", Some(vks_core::Role::Operator))
    }

    fn req(method: Method, uri: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder().method(method).uri(uri).body(axum::body::Body::empty()).unwrap()
    }

    fn authed_req(method: Method, uri: &str, token: &str) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn healthz_returns_200() {
        let app = test_router();
        let resp = app.oneshot(req(Method::GET, "/healthz")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["alive"], true);
    }

    #[tokio::test]
    async fn readyz_returns_200_with_real_state() {
        let app = test_router();
        let resp = app.oneshot(req(Method::GET, "/readyz")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["db"], true);
        assert_eq!(json["media"], true);
        assert!(json.get("nats").is_none(), "NATS-фикция удалена (T1.3)");
    }

    #[tokio::test]
    async fn router_rejections_use_error_envelope() {
        // 405: метод не подходит — JSON-конверт + `Allow` (RFC 9110 §15.5.6).
        let app = test_router();
        let resp = app.oneshot(req(Method::PUT, "/healthz")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(resp.headers().contains_key(header::ALLOW), "405 обязан нести Allow");
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error_code"], "method_not_allowed");

        // 415: тело без application/json.
        let (app, _state, token) = authed_test_app();
        let request = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/v1/rooms")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "text/plain")
            .body(axum::body::Body::from("not json"))
            .unwrap();
        let resp = app.oneshot(request).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error_code"], "unsupported_media_type");

        // 400: битый JSON с корректным content-type.
        let (app, _state, token) = authed_test_app();
        let request = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/v1/rooms")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("{not-json"))
            .unwrap();
        let resp = app.oneshot(request).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error_code"], "invalid_body");

        // 400 из хендлера (невалидный UUID): статус сохраняется, текст ошибки — в `error`.
        let (app, _state, token) = authed_test_app();
        let resp = app.oneshot(authed_req(Method::GET, "/api/v1/rooms/not-a-uuid", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error_code"], "invalid_body");
        assert!(json["error"].as_str().is_some_and(|s| !s.is_empty()), "текст ошибки сохранён");
    }

    #[tokio::test]
    async fn openapi_returns_json() {
        let app = test_router();
        let resp = app.oneshot(req(Method::GET, "/openapi.json")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["openapi"], "3.1.0");
    }

    #[tokio::test]
    async fn unauthenticated_v1_request_fails_with_401() {
        let (app, _state, _token) = authed_test_app();
        let resp = app.oneshot(req(Method::GET, "/api/v1/rooms")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unconfigured_v1_request_fails_closed_with_503() {
        let app = test_router();
        let resp = app.oneshot(req(Method::GET, "/api/v1/rooms")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn rbac_viewer_cannot_mutate_v1() {
        let (app, _state, token) = authed_app("viewer", Some(vks_core::Role::Viewer));
        let resp = app.oneshot(authed_req(Method::POST, "/api/v1/rooms", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rbac_unknown_token_subject_is_forbidden() {
        let (app, _state, token) = authed_app("ghost", None);
        let resp = app.oneshot(authed_req(Method::GET, "/api/v1/rooms", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rbac_audit_gate_on_real_router() {
        // Audit log читается только Moderator+ (BACKEND_RULES §1.7).
        let (app, _state, token) = authed_app("viewer", Some(vks_core::Role::Viewer));
        let resp = app.oneshot(authed_req(Method::GET, "/api/v1/audit", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        let (app, _state, token) = authed_app("auditor", Some(vks_core::Role::Moderator));
        let resp = app.oneshot(authed_req(Method::GET, "/api/v1/audit", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rbac_moderator_cannot_kick_via_bulk_on_real_router() {
        // bulk включает Kick = soft-delete аккаунта → Operator only.
        let (app, _state, token) = authed_app("moderator", Some(vks_core::Role::Moderator));
        let resp = app.oneshot(authed_req(Method::POST, "/api/v1/users/bulk", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rbac_matrix_negatives_on_real_router() {
        // §1.7: глобальный MCU/SFU — Operator only.
        let (app, _state, token) = authed_app("moderator", Some(vks_core::Role::Moderator));
        let resp = app.oneshot(authed_req(Method::POST, "/api/v1/mode", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // §1.7: экспорт audit-log — Moderator+ (Participant/Viewer ❌).
        let (app, _state, token) = authed_app("viewer", Some(vks_core::Role::Viewer));
        let resp = app.oneshot(authed_req(Method::GET, "/api/v1/audit/export", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Operator — разрешённый путь (CSV-экспорт).
        let (app, _state, token) = authed_test_app();
        let resp = app.oneshot(authed_req(Method::GET, "/api/v1/audit/export", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_room_after_create() {
        let (app, state, token) = authed_test_app();
        let created = Rooms::create(&state, "conf");
        let resp =
            app.oneshot(authed_req(Method::GET, &format!("/api/v1/rooms/{}", created.id), &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "conf");
    }

    #[tokio::test]
    async fn get_unknown_room_404() {
        let (app, _state, token) = authed_test_app();
        let resp = app
            .oneshot(authed_req(Method::GET, "/api/v1/rooms/00000000-0000-0000-0000-000000000000", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_global_mode_endpoint() {
        let (app, _state, token) = authed_test_app();
        let body = serde_json::json!({ "mode": "mcu" });
        let req = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/v1/mode")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["new_mode"], "mcu");
        assert_eq!(json["old_mode"], "sfu");
    }

    #[tokio::test]
    async fn invalid_room_id_returns_400() {
        let (app, _state, token) = authed_test_app();
        let resp = app.oneshot(authed_req(Method::GET, "/api/v1/rooms/not-a-uuid", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
