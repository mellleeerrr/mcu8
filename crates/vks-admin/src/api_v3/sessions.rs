//! Фаза 3 (§4.5 плана): `ui/sessions`, `sessions`, `conferences`, `ui/conferences` —
//! данные из наших комнат (`RoomRecord`).
//!
//! Маппинг (мапперы `Fv`/`dy`/`zZ` мягкие): `id` → `sessionId`/`conferenceId`,
//! `display_name` → `displayName`. `has_stopped=true` и `state=inactive` дают пустой
//! список (stopped/inactive комнат у нас нет — все «live»), `search` фильтрует по
//! имени, `sort_field=started_at/start_at/created_at` — по времени создания.

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use vks_auth::jwt::VerifiedJwt;
use vks_core::RoomId;

use super::{error_json, track};
use crate::audit::AuditEntry;
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
    /// `state` из экрана конференций (`active`/`inactive`): наши комнаты всегда
    /// «активные» → `inactive` даёт пустой список, прочее — без фильтра.
    pub state: Option<String>,
    /// `true` → пусто (stopped-сессий у нас нет), `false`/`None` — все комнаты (§4.5).
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

/// Отбор + сортировка + пагинация комнат. Возвращает (страница, всего совпало).
fn select_rooms(state: &AppState, query: &SessionsQuery) -> (Vec<RoomRecord>, usize) {
    let rooms = state.rooms.lock().expect("rooms store poisoned");
    let mut items: Vec<RoomRecord> = rooms.values().cloned().collect();
    if let Some(search) = query.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let needle = search.to_lowercase();
        items.retain(|r| r.name.to_lowercase().contains(&needle));
    }
    // Все наши комнаты «активные»: stopped-сессий нет, inactive-конференций нет.
    if query.has_stopped == Some(true) {
        items.clear();
    }
    if query.state.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("inactive")) {
        items.clear();
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
///
/// Контракт карточек из бандла (иначе `TypeError` деструктуризации в Vue):
/// карточка сессии читает `abonents.{sip,h323}` и `abonents.{rtsp,vnc,media,file}`;
/// карточка конференции — `contacts.{users,h323,sip}` и те же устройства;
/// обе проверяют переполнение по `payload.connections_limit` (мапперы:
/// `connectionsLimit ← payload.connections_limit`). Счётчики — числа.
fn session_json(r: &RoomRecord, connections_limit: u32) -> Value {
    json!({
        "id": r.id.to_string(),
        "display_name": r.name,
        "participants": r.participants,
        "bitrate_kbps": r.bitrate_kbps,
        "state": "ACTIVE",
        "has_stopped": false,
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
    let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    state.audit.lock().expect("audit store poisoned").record(AuditEntry {
        ts_ms,
        user: user.to_string(),
        action: action.to_string(),
        room: Some(room_id.to_string()),
        ip: String::new(),
    });
}

fn sessions_payload(state: &AppState, query: &SessionsQuery) -> Value {
    let (items, count) = select_rooms(state, query);
    let limit = connections_limit(state);
    json!({
        "sessions": items.iter().map(|r| session_json(r, limit)).collect::<Vec<_>>(),
        "count": count
    })
}

fn conferences_payload(state: &AppState, query: &SessionsQuery) -> Value {
    let (items, count) = select_rooms(state, query);
    let limit = connections_limit(state);
    json!({
        "conferences": items.iter().map(|r| session_json(r, limit)).collect::<Vec<_>>(),
        "count": count
    })
}

/// `GET /api/v3/ui/sessions` (Ф3: наши комнаты; дашборд дёргает сразу после логина).
pub async fn ui_sessions(State(state): State<SharedState>, Query(query): Query<SessionsQuery>) -> Response {
    track("ui/sessions");
    Json(sessions_payload(&state, &query)).into_response()
}

/// `GET /api/v3/ui/sessions/{id}` → `{session:{…}}` (404 JSON если нет).
pub async fn ui_session(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    track("ui/sessions/{id}");
    match find_room(&state, &id) {
        Some(room) => Json(json!({ "session": session_json(&room, connections_limit(&state)) })).into_response(),
        None => error_json(404, "Session not found"),
    }
}

/// `GET /api/v3/sessions` — тот же список (не-ui форма).
pub async fn sessions(State(state): State<SharedState>, Query(query): Query<SessionsQuery>) -> Response {
    track("sessions");
    Json(sessions_payload(&state, &query)).into_response()
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
    let record = Rooms::create(&state, name);
    audit_room(&state, &verified.user_id, "conference.created", &record.id.to_string());
    Json(json!({ "conference": session_json(&record, connections_limit(&state)) })).into_response()
}

/// `GET /api/v3/conferences/{id}` → `{conference:{…}}` (404 JSON если нет).
pub async fn conference(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    track("conferences/{id}");
    match find_room(&state, &id) {
        Some(room) => Json(json!({ "conference": session_json(&room, connections_limit(&state)) })).into_response(),
        None => error_json(404, "Conference not found"),
    }
}

/// `DELETE /api/v3/conferences/{id}` → 204 | 404 (аудит §4.5).
pub async fn delete_conference(
    State(state): State<SharedState>,
    Extension(verified): Extension<VerifiedJwt>,
    Path(id): Path<String>,
) -> Response {
    track("conferences/{id}");
    match find_room(&state, &id) {
        Some(room) => {
            if Rooms::delete(&state, &room.id) {
                audit_room(&state, &verified.user_id, "conference.deleted", &room.id.to_string());
                StatusCode::NO_CONTENT.into_response()
            } else {
                error_json(404, "Conference not found")
            }
        }
        None => error_json(404, "Conference not found"),
    }
}
