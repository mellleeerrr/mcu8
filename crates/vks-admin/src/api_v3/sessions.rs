//! Фаза 3 (§4.5 плана): `ui/sessions`, `sessions`, `conferences`, `ui/conferences` —
//! данные из наших комнат (`RoomRecord`).
//!
//! Маппинг (мапперы `Fv`/`dy`/`zZ` мягкие): `id` → `sessionId`/`conferenceId`,
//! `display_name` → `displayName`. `has_stopped=true` и `state=inactive` фильтруют
//! по остановленным сессиям (`state.api_v3.stopped_sessions`), `search` фильтрует по
//! имени, `sort_field=started_at/start_at/created_at` — по времени создания.

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use vks_auth::jwt::VerifiedJwt;
use vks_core::RoomId;

use super::{error_json, track};
use crate::rooms::Rooms;
use crate::settings::SettingsService;
use crate::state::{AppState, RoomRecord, SharedState};

/// Параметры списка сессий (дашборд шлёт
/// `page/page_size/sort_field/sort_order/search/has_stopped`, конференции —
/// ещё `state/exclude_tags`).
#[derive(Debug, Default, Deserialize)]
pub struct SessionsQuery {
    /// Страница (с 1; default 1).
    pub page: Option<u32>,
    /// Размер страницы (default 20; clamp 1..=200).
    pub page_size: Option<u32>,
    /// Поле сортировки (`display_name` default; `id`/`session_id`;
    /// `start_at`/`started_at`/`created_at` — по времени создания).
    pub sort_field: Option<String>,
    /// Порядок: `0`/`None` — возрастание, любой ненулевой — убывание.
    pub sort_order: Option<i64>,
    /// Поиск по имени (подстрока, case-insensitive).
    pub search: Option<String>,
    /// `state` из экрана конференций (`active`/`inactive`).
    pub state: Option<String>,
    /// `true` → остановленные сессии, `false` → активные, `None` — все комнаты (§4.5).
    pub has_stopped: Option<bool>,
}

/// Тело `POST /api/v3/conferences` (legacy `VT`: snake_case, `pin`/`payload` — про запас).
#[derive(Debug, Deserialize)]
pub struct CreateConferenceRequest {
    /// Имя конференции (`display_name` в UI).
    #[serde(default)]
    pub display_name: Option<String>,
    /// Альтернативное имя (`name`).
    #[serde(default, alias = "name")]
    pub name: Option<String>,
    /// PIN (в доменной модели пока не отражается — долг §4.5).
    #[serde(default)]
    pub pin: Option<String>,
    /// Доп. payload UI (игнорируется).
    #[serde(default)]
    pub payload: Option<Value>,
}

/// Тело `PATCH /api/v3/conferences/{id}`.
#[derive(Debug, Deserialize)]
pub struct PatchConferenceRequest {
    /// Имя конференции (`display_name` или `name`).
    #[serde(default, alias = "name")]
    pub display_name: Option<String>,
    /// PIN.
    #[serde(default)]
    pub pin: Option<String>,
    /// Доп. payload UI.
    #[serde(default)]
    pub payload: Option<Value>,
}

/// Тело `POST /api/v3/sessions`.
#[derive(Debug, Default, Deserialize)]
pub struct StartSessionRequest {
    #[serde(default, alias = "id", alias = "conferenceId")]
    pub conference_id: Option<String>,
}

/// Отбор + сортировка + пагинация комнат. Возвращает (страница, всего совпало).
fn select_rooms(state: &AppState, query: &SessionsQuery) -> (Vec<RoomRecord>, usize) {
    let rooms = state.rooms.lock().expect("rooms store poisoned");
    let stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned");
    let mut items: Vec<RoomRecord> = rooms.values().cloned().collect();
    if let Some(search) = query.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let needle = search.to_lowercase();
        items.retain(|r| r.name.to_lowercase().contains(&needle));
    }
    if query.has_stopped == Some(true) {
        items.retain(|r| stopped.contains(&r.id));
    } else if query.has_stopped == Some(false) {
        items.retain(|r| !stopped.contains(&r.id));
    }
    if let Some(ref st) = query.state {
        if st.eq_ignore_ascii_case("inactive") {
            items.retain(|r| stopped.contains(&r.id));
        } else if st.eq_ignore_ascii_case("active") {
            items.retain(|r| !stopped.contains(&r.id));
        }
    }
    match query.sort_field.as_deref().unwrap_or("display_name") {
        "id" | "session_id" => items.sort_by(|a, b| a.id.to_string().cmp(&b.id.to_string())),
        "start_at" | "started_at" | "created_at" => items.sort_by_key(|r| r.created_ms),
        _ => items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
    }
    if query.sort_order.unwrap_or(0) != 0 {
        items.reverse();
    }
    let total = items.len();
    let page = query.page.unwrap_or(1).max(1) as usize;
    let page_size = query.page_size.unwrap_or(20).clamp(1, 200) as usize;
    let start = (page - 1) * page_size;
    let page_items = if start >= total { Vec::new() } else { items.into_iter().skip(start).take(page_size).collect() };
    (page_items, total)
}

/// JSON сессии/конференции (мапперы `Fv`/`dy`/`zZ` мягкие).
fn session_json(r: &RoomRecord, connections_limit: u32, is_stopped: bool) -> Value {
    let (state_str, has_stopped) = if is_stopped { ("STOPPED", true) } else { ("ACTIVE", false) };
    json!({
        "id": r.id.to_string(),
        "display_name": r.name,
        "participants": r.participants,
        "bitrate_kbps": r.bitrate_kbps,
        "state": state_str,
        "has_stopped": has_stopped,
        // Секунды: карточки умножают на 1e3 (`startedAt*1e3`) для Date/duration.
        "started_at": r.created_ms / 1000,
        "recording": false,
        "videorecord": false,
        "broadcast": false,
        "owner_id": "",
        "abonents": { "sip": 0, "h323": 0, "rtsp": 0, "vnc": 0, "media": 0, "file": 0 },
        "contacts": { "users": 0, "sip": 0, "h323": 0, "rtsp": 0, "vnc": 0, "media": 0, "file": 0 },
        "payload": { "connections_limit": connections_limit }
    })
}

/// Лимит подключений комнаты (`payload.connections_limit`, из наших настроек).
fn connections_limit(state: &AppState) -> u32 {
    SettingsService::get(state).limits.max_participants_per_room
}

fn find_room(state: &AppState, id: &str) -> Option<RoomRecord> {
    let uuid = Uuid::parse_str(id).ok()?;
    Rooms::get(state, &RoomId::from_uuid(uuid))
}

fn audit_room(state: &AppState, user: &str, action: &str, room_id: &str) {
    state.record_audit_entry(user, action, Some(room_id.to_string()), "127.0.0.1");
}

fn sessions_payload(state: &AppState, query: &SessionsQuery) -> Value {
    let (items, count) = select_rooms(state, query);
    let limit = connections_limit(state);
    let stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned");
    json!({
        "sessions": items.iter().map(|r| session_json(r, limit, stopped.contains(&r.id))).collect::<Vec<_>>(),
        "count": count
    })
}

fn conferences_payload(state: &AppState, query: &SessionsQuery) -> Value {
    let (items, count) = select_rooms(state, query);
    let limit = connections_limit(state);
    let stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned");
    json!({
        "conferences": items.iter().map(|r| session_json(r, limit, stopped.contains(&r.id))).collect::<Vec<_>>(),
        "count": count
    })
}

/// `GET /api/v3/ui/sessions` (Ф3: наши комнаты; дашборд дёргает сразу после логина).
pub async fn ui_sessions(State(state): State<SharedState>, Query(query): Query<SessionsQuery>) -> Response {
    track("ui/sessions");
    Json(sessions_payload(&state, &query)).into_response()
}

/// `GET /api/v3/ui/sessions/{id}` или `GET /api/v3/sessions/{id}` → `{session:{…}}` (404 JSON если нет).
pub async fn session(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    track("sessions/{id}");
    match find_room(&state, &id) {
        Some(room) => {
            let is_stopped =
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&room.id);
            Json(json!({ "session": session_json(&room, connections_limit(&state), is_stopped) })).into_response()
        }
        None => error_json(404, "Session not found"),
    }
}

/// `GET /api/v3/sessions` — тот же список (не-ui форма).
pub async fn sessions(State(state): State<SharedState>, Query(query): Query<SessionsQuery>) -> Response {
    track("sessions");
    Json(sessions_payload(&state, &query)).into_response()
}

/// `POST /api/v3/sessions` — старт сессии конференции (или создание быстрой сессии).
pub async fn start_session(
    State(state): State<SharedState>,
    Extension(verified): Extension<VerifiedJwt>,
    Json(body): Json<StartSessionRequest>,
) -> Response {
    track("sessions");
    let room = if let Some(ref cid) = body.conference_id {
        match find_room(&state, cid) {
            Some(r) => {
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").remove(&r.id);
                r
            }
            None => return error_json(404, "Conference not found"),
        }
    } else {
        match Rooms::create_async(&state, "Quick Session".to_string()).await {
            Ok(r) => r,
            Err(e) => return error_json(500, &format!("Storage error: {e}")),
        }
    };
    audit_room(&state, &verified.user_id, "session.started", &room.id.to_string());
    Json(json!({ "session": session_json(&room, connections_limit(&state), false) })).into_response()
}

/// `DELETE /api/v3/sessions/{id}` (или `/ui/sessions/{id}`) — завершение сессии БЕЗ удаления конференции.
pub async fn stop_session(
    State(state): State<SharedState>,
    Extension(verified): Extension<VerifiedJwt>,
    Path(id): Path<String>,
) -> Response {
    track("sessions/{id}");
    match find_room(&state, &id) {
        Some(room) => {
            let _ = Rooms::update_async(&state, &room.id, None, None, Some(0), Some(0)).await;
            state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").insert(room.id);
            audit_room(&state, &verified.user_id, "session.stopped", &room.id.to_string());
            StatusCode::NO_CONTENT.into_response()
        }
        None => error_json(404, "Session not found"),
    }
}

/// `GET /api/v3/conferences` — список конференций.
pub async fn conferences(State(state): State<SharedState>, Query(query): Query<SessionsQuery>) -> Response {
    track("conferences");
    Json(conferences_payload(&state, &query)).into_response()
}

/// `GET /api/v3/ui/conferences` — тот же список для мапперов `dy/zZ`.
pub async fn ui_conferences(State(state): State<SharedState>, Query(query): Query<SessionsQuery>) -> Response {
    track("ui/conferences");
    Json(conferences_payload(&state, &query)).into_response()
}

/// `POST /api/v3/conferences` — создание комнаты тем же внутренним путём,
/// что `POST /api/v1/rooms` (без дублирования логики).
pub async fn create_conference(
    State(state): State<SharedState>,
    Extension(verified): Extension<VerifiedJwt>,
    Json(body): Json<CreateConferenceRequest>,
) -> Response {
    track("conferences");
    let name = body
        .display_name
        .or(body.name)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Conference".to_string());
    let record = match Rooms::create_async(&state, name).await {
        Ok(r) => r,
        Err(e) => return error_json(500, &format!("Storage error: {e}")),
    };
    audit_room(&state, &verified.user_id, "conference.created", &record.id.to_string());
    Json(json!({ "conference": session_json(&record, connections_limit(&state), false) })).into_response()
}

/// `GET /api/v3/conferences/{id}` → `{conference:{…}}` (404 JSON если нет).
pub async fn conference(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    track("conferences/{id}");
    match find_room(&state, &id) {
        Some(room) => {
            let is_stopped =
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&room.id);
            Json(json!({ "conference": session_json(&room, connections_limit(&state), is_stopped) })).into_response()
        }
        None => error_json(404, "Conference not found"),
    }
}

/// `PATCH /api/v3/conferences/{id}` — обновление настроек конференции.
pub async fn update_conference(
    State(state): State<SharedState>,
    Extension(verified): Extension<VerifiedJwt>,
    Path(id): Path<String>,
    Json(body): Json<PatchConferenceRequest>,
) -> Response {
    track("conferences/{id}");
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference not found");
    };
    if let Some(new_name) = body.display_name.filter(|s| !s.trim().is_empty()) {
        if let Err(e) = Rooms::update_async(&state, &room.id, Some(new_name), None, None, None).await {
            return error_json(500, &format!("Storage error: {e}"));
        }
    }
    audit_room(&state, &verified.user_id, "conference.updated", &room.id.to_string());
    let updated = find_room(&state, &id).unwrap_or(room);
    let is_stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&updated.id);
    Json(json!({ "conference": session_json(&updated, connections_limit(&state), is_stopped) })).into_response()
}

/// `DELETE /api/v3/conferences/{id}` → 204 | 404 (аудит §4.5).
pub async fn delete_conference(
    State(state): State<SharedState>,
    Extension(verified): Extension<VerifiedJwt>,
    Path(id): Path<String>,
) -> Response {
    track("conferences/{id}");
    match find_room(&state, &id) {
        Some(room) => match Rooms::delete_async(&state, &room.id).await {
            Ok(true) => {
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").remove(&room.id);
                audit_room(&state, &verified.user_id, "conference.deleted", &room.id.to_string());
                StatusCode::NO_CONTENT.into_response()
            }
            Ok(false) => error_json(404, "Conference not found"),
            Err(e) => error_json(500, &format!("Storage error: {e}")),
        },
        None => error_json(404, "Conference not found"),
    }
}

/// `GET /api/v3/conferences/{id}/contacts` → `{contacts:[], count:0}`.
pub async fn conference_contacts(Path(_id): Path<String>) -> Response {
    track("conferences/{id}/contacts");
    Json(json!({ "contacts": [], "count": 0 })).into_response()
}

/// `GET /api/v3/sessions/{id}/abonents` → `{abonents:[], count:0}`.
pub async fn session_abonents(Path(_id): Path<String>) -> Response {
    track("sessions/{id}/abonents");
    Json(json!({ "abonents": [], "count": 0 })).into_response()
}
