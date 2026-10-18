//! `/api/v3/*` — адаптер к legacy PNM UI (ADR-0027; `docs/progress/FRONTEND_PORT_PLAN.md` §4.3–4.5).
//!
//! Фаза 2: auth-цепочка (login + двухступенчатый lockout + инвалидация токенов при смене
//! пароля), весь boot/shell (§4.5), WS-протокол (§4.4), catch-all 501 (вкл. `/api/v3/`).
//! Фаза 3: `ui/sessions` / `conferences` из наших комнат (§4.5).
//!
//! Конверт ошибок (UI читает `error.response.data.error.description`):
//! `{"error":{"code":<int>,"description":"<string>"}}`. HTTP-статус совпадает с `code`
//! (401 → интерцептор UI эмитит logout).
//!
//! Слои: fail-closed 503 (наружу всего `/api/v3/*`, включая login и ws) →
//! exempt-list (§4.3) + WS (токен в каждом сообщении) → token-middleware на остальном.

pub mod auth;
pub mod boot;
pub mod sessions;
pub mod shell;
pub mod ws;

#[cfg(test)]
mod tests;

use axum::{
    http::{Method, StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, delete, get, patch, post},
    Json, Router,
};

use vks_common::metrics::API_V3_REQUESTS;

use crate::handlers::SharedState;

pub use auth::Lockout;

/// Состояние адаптера `/api/v3` (в `AppState`).
#[derive(Debug, Default)]
pub struct ApiV3State {
    /// Двухступенчатый lockout логина (BACKEND_RULES §1.4).
    pub lockout: Lockout,
    /// Rate-limit новых WS-соединений (≤ 100/s per-IP, §4.4).
    pub ws_ratelimit: ws::WsRateLimit,
    /// Идентификаторы остановленных сессий (M4.3).
    pub stopped_sessions: std::sync::Mutex<std::collections::HashSet<vks_core::RoomId>>,
}

/// Собирает `Router` адаптера `/api/v3/*`.
///
/// Монтируется в композиционном корне (`vks-server`) через `.merge(...)`.
pub fn router(state: SharedState) -> Router {
    let protected = Router::new()
        // Auth (protected: смена пароля).
        .route("/api/v3/password", patch(auth::change_password))
        // Shell (§4.5).
        .route("/api/v3/profile", get(shell::profile))
        .route("/api/v3/time-zones", get(shell::time_zones))
        .route("/api/v3/locales", get(shell::locales))
        .route("/api/v3/abonents/stop_reasons", get(shell::stop_reasons))
        .route("/api/v3/abonents/count", get(shell::abonents_count))
        .route("/api/v3/users/credentials/expire", get(shell::users_credentials_expire))
        .route("/api/v3/ui/system/status", get(shell::ui_system_status))
        .route("/api/v3/license", get(shell::license))
        .route("/api/v3/features", get(shell::features))
        .route("/api/v3/engine/calibration", get(shell::engine_calibration))
        .route("/api/v3/engine/diagnostics", get(shell::engine_diagnostics))
        .route("/api/v3/system/time", get(shell::system_time))
        .route("/api/v3/settings/calls", get(shell::settings_calls).patch(shell::update_settings_calls))
        .route("/api/v3/settings/security", get(shell::settings_security))
        .route("/api/v3/settings/audio_codecs", get(shell::settings_audio_codecs))
        .route("/api/v3/settings/video_codecs", get(shell::settings_video_codecs))
        .route("/api/v3/waitingroom", get(shell::waitingroom))
        .route("/api/v3/conferences/status", get(shell::conferences_status))
        // Фаза 3 — данные (наши комнаты).
        .route("/api/v3/ui/sessions", get(sessions::ui_sessions).post(sessions::start_session))
        .route("/api/v3/ui/sessions/{id}", get(sessions::session).delete(sessions::stop_session))
        .route("/api/v3/ui/sessions/{id}/abonents", get(sessions::session_abonents))
        .route("/api/v3/sessions", get(sessions::sessions).post(sessions::start_session))
        .route("/api/v3/sessions/{id}", get(sessions::session).delete(sessions::stop_session))
        .route("/api/v3/sessions/{id}/abonents", get(sessions::session_abonents))
        .route("/api/v3/conferences", get(sessions::conferences).post(sessions::create_conference))
        .route(
            "/api/v3/conferences/{id}",
            get(sessions::conference).patch(sessions::update_conference).delete(sessions::delete_conference),
        )
        .route("/api/v3/conferences/{id}/contacts", get(sessions::conference_contacts))
        .route("/api/v3/ui/conferences", get(sessions::ui_conferences).post(sessions::create_conference))
        .route(
            "/api/v3/ui/conferences/{id}",
            get(sessions::conference).patch(sessions::update_conference).delete(sessions::delete_conference),
        )
        .route("/api/v3/ui/conferences/{id}/contacts", get(sessions::conference_contacts))
        .route("/api/v3/ui/video_layouts", get(shell::video_layouts))
        .route("/api/v3/video_layouts", get(shell::video_layouts))
        .route("/api/v3/ui/video_layouts/mode", post(shell::switch_video_layout_mode))
        .route("/api/v3/ui/sessions/quick", post(sessions::quick_session))
        .route("/api/v3/sessions/quick", post(sessions::quick_session))
        .route("/api/v3/ui/records", get(shell::ui_records))
        .route("/api/v3/records", get(shell::ui_records))
        .route("/api/v3/abonents/{id}", delete(sessions::remove_abonent).patch(sessions::edit_abonent))
        .route("/api/v3/abonents/{id}/{setting}", post(sessions::switch_abonent_settings))
        .route("/api/v3/sessions/{id}/abonents/{abonent_id}", delete(sessions::remove_session_abonent))
        .route("/api/v3/ui/sessions/{id}/abonents/{abonent_id}", delete(sessions::remove_session_abonent))
        .route("/api/v3/engine/command", post(shell::engine_command))
        .route("/api/v3/engine/abonents", get(sessions::engine_abonents))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::token_middleware));

    Router::new()
        // Exempt-list (§4.3) — без токена.
        .route("/api/v3/login", post(auth::login))
        .route("/api/v3/login/status", get(boot::login_status))
        .route("/api/v3/system/about", get(boot::system_about))
        .route("/api/v3/about", get(boot::about))
        .route("/api/v3/references", get(boot::references))
        // WS: сокет открывается без токена, токен — в каждом сообщении (§4.4).
        .route("/api/v3/ws", get(ws::ws_handler))
        .merge(protected)
        // Catch-all 501 на любой метод; `{*rest}` не матчит пустой rest (B3 ревью),
        // поэтому `/api/v3/` и `/api/v3` регистрируются явно (единый конверт).
        .route("/api/v3/{*rest}", any(catch_all))
        .route("/api/v3/", any(catch_all))
        .route("/api/v3", any(catch_all))
        // Fail-closed: jwt_issuer=None → 503 на **весь** /api/v3 (включая login и ws).
        .layer(middleware::from_fn_with_state(state.clone(), auth::fail_closed_middleware))
        .with_state(state)
}

/// Учитывает вызов endpoint'а: `vks_api_v3_requests_total{endpoint}` (§4.7).
pub(crate) fn track(endpoint: &'static str) {
    API_V3_REQUESTS.with_label_values(&[endpoint]).inc();
}

/// Конверт ошибки API v3: `{"error":{"code":<int>,"description":"…"}}`.
pub(crate) fn error_json(code: u16, description: &str) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(serde_json::json!({ "error": { "code": code, "description": description } }))).into_response()
}

/// 401 (UI эмитит `logout`).
pub(crate) fn unauthorized(description: &str) -> Response {
    error_json(401, description)
}

/// Catch-all: 501 `Not implemented` + warn + метрика `endpoint="unmatched"` (§4.5).
///
/// Падение в 404 запрещено: UI считает 404 «метод не существует».
async fn catch_all(method: Method, uri: Uri) -> Response {
    track("unmatched");
    tracing::warn!(%method, %uri, "api_v3: unmatched route → 501");
    error_json(501, "Not implemented")
}
