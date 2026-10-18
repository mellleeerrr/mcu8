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

use vks_core::{Role, RoomId};

use super::auth::ClientIp;
use super::{error_json, track};
use crate::auth_middleware::AuthContext;
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
fn session_json(state: &AppState, r: &RoomRecord, connections_limit: u32, is_stopped: bool) -> Value {
    let (state_str, has_stopped) = if is_stopped { ("STOPPED", true) } else { ("ACTIVE", false) };
    let participants = state.room_hub.list_participants(&r.id);
    let sip_count = participants.iter().filter(|p| p.participant_id.contains("@sip-gateway")).count();
    let h323_count = participants.iter().filter(|p| p.participant_id.contains("@h323-gateway")).count();
    let is_recording = participants.iter().any(|p| p.is_recorder);
    json!({
        "id": r.id.to_string(),
        "display_name": r.name,
        "participants": r.participants,
        "bitrate_kbps": r.bitrate_kbps,
        "state": state_str,
        "has_stopped": has_stopped,
        // Секунды: карточки умножают на 1e3 (`startedAt*1e3`) для Date/duration.
        "started_at": r.created_ms / 1000,
        "recording": is_recording,
        "videorecord": is_recording,
        "broadcast": false,
        "owner_id": "",
        "abonents": { "sip": sip_count, "h323": h323_count, "rtsp": 0, "vnc": 0, "media": 0, "file": 0 },
        "contacts": { "users": 0, "sip": sip_count, "h323": h323_count, "rtsp": 0, "vnc": 0, "media": 0, "file": 0 },
        "payload": { "connections_limit": connections_limit }
    })
}

/// Лимит подключений комнаты (`payload.connections_limit`, из наших настроек).
fn connections_limit(state: &AppState) -> u32 {
    SettingsService::get(state).limits.max_participants_per_room
}

fn find_room(state: &AppState, id: &str) -> Option<RoomRecord> {
    if let Ok(uuid) = Uuid::parse_str(id) {
        if let Some(r) = Rooms::get(state, &RoomId::from_uuid(uuid)) {
            return Some(r);
        }
    }
    let rooms = state.rooms.lock().expect("rooms store poisoned");
    rooms.values().find(|r| r.name == id || r.name.eq_ignore_ascii_case(id)).cloned()
}

#[allow(clippy::result_large_err)]
fn check_permission(auth: &AuthContext, target_room_id: Option<&str>) -> Result<(), Response> {
    if auth.role < Role::Moderator {
        return Err(error_json(403, "Forbidden: insufficient permissions"));
    }
    if let Some(ref scope) = auth.room_scope {
        match target_room_id {
            Some(rid) if rid.eq_ignore_ascii_case(scope) => Ok(()),
            _ => Err(error_json(403, "Forbidden: token room scope mismatch")),
        }
    } else {
        Ok(())
    }
}

fn audit_room(state: &AppState, user: &str, action: &str, room_id: &str, client_ip: Option<&str>) {
    let ip = client_ip.unwrap_or("127.0.0.1");
    state.record_audit_entry(user, action, Some(room_id.to_string()), ip);
}

fn sessions_payload(state: &AppState, query: &SessionsQuery) -> Value {
    let (items, count) = select_rooms(state, query);
    let limit = connections_limit(state);
    let stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned");
    json!({
        "sessions": items.iter().map(|r| session_json(state, r, limit, stopped.contains(&r.id))).collect::<Vec<_>>(),
        "count": count
    })
}

fn conferences_payload(state: &AppState, query: &SessionsQuery) -> Value {
    let (items, count) = select_rooms(state, query);
    let limit = connections_limit(state);
    let stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned");
    json!({
        "conferences": items.iter().map(|r| session_json(state, r, limit, stopped.contains(&r.id))).collect::<Vec<_>>(),
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
            Json(json!({ "session": session_json(&state, &room, connections_limit(&state), is_stopped) }))
                .into_response()
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
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Json(body): Json<StartSessionRequest>,
) -> Response {
    track("sessions");
    if let Err(resp) = check_permission(&auth, body.conference_id.as_deref()) {
        return resp;
    }
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
    state.notify_admin("sessions:started", json!({ "id": room.id.to_string() }));
    state.notify_admin("sessions", json!({ "action": "started", "id": room.id.to_string() }));
    audit_room(
        &state,
        &auth.subject,
        "session.started",
        &room.id.to_string(),
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );
    Json(json!({ "session": session_json(&state, &room, connections_limit(&state), false) })).into_response()
}

/// `POST /api/v3/sessions/quick` (или `/ui/sessions/quick`) — быстрый старт сессии.
pub async fn quick_session(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    body: Option<Json<Value>>,
) -> Response {
    track("sessions/quick");
    if let Err(resp) = check_permission(&auth, None) {
        return resp;
    }
    let name = body
        .as_ref()
        .and_then(|b| b.get("display_name").or_else(|| b.get("name")))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Quick Session");
    let room = match Rooms::create_async(&state, name.to_string()).await {
        Ok(r) => r,
        Err(e) => return error_json(500, &format!("Storage error: {e}")),
    };
    state.notify_admin("sessions:started", json!({ "id": room.id.to_string() }));
    state.notify_admin("sessions", json!({ "action": "started", "id": room.id.to_string() }));
    state.notify_admin("conferences:created", json!({ "id": room.id.to_string() }));
    state.notify_admin("conferences", json!({ "action": "created", "id": room.id.to_string() }));
    audit_room(
        &state,
        &auth.subject,
        "session.quick_started",
        &room.id.to_string(),
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );
    Json(json!({ "session": session_json(&state, &room, connections_limit(&state), false) })).into_response()
}

/// `DELETE /api/v3/sessions/{id}` (или `/ui/sessions/{id}`) — завершение сессии БЕЗ удаления конференции.
pub async fn stop_session(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
) -> Response {
    track("sessions/{id}");
    if let Err(resp) = check_permission(&auth, Some(&id)) {
        return resp;
    }
    match find_room(&state, &id) {
        Some(room) => {
            let _ = Rooms::update_async(&state, &room.id, None, None, Some(0), Some(0)).await;
            state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").insert(room.id);
            state.notify_admin("sessions:stopped", json!({ "id": room.id.to_string() }));
            state.notify_admin("sessions", json!({ "action": "stopped", "id": room.id.to_string() }));
            audit_room(
                &state,
                &auth.subject,
                "session.stopped",
                &room.id.to_string(),
                client_ip.as_ref().map(|c| c.0 .0.as_str()),
            );
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
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Json(body): Json<CreateConferenceRequest>,
) -> Response {
    track("conferences");
    if let Err(resp) = check_permission(&auth, None) {
        return resp;
    }
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
    state.notify_admin("conferences:created", json!({ "id": record.id.to_string(), "display_name": record.name }));
    state.notify_admin("conferences", json!({ "action": "created", "id": record.id.to_string() }));
    audit_room(
        &state,
        &auth.subject,
        "conference.created",
        &record.id.to_string(),
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );
    Json(json!({ "conference": session_json(&state, &record, connections_limit(&state), false) })).into_response()
}

/// `GET /api/v3/conferences/{id}` → `{conference:{…}}` (404 JSON если нет).
pub async fn conference(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    track("conferences/{id}");
    match find_room(&state, &id) {
        Some(room) => {
            let is_stopped =
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&room.id);
            Json(json!({ "conference": session_json(&state, &room, connections_limit(&state), is_stopped) }))
                .into_response()
        }
        None => error_json(404, "Conference not found"),
    }
}

/// `PATCH /api/v3/conferences/{id}` — обновление настроек конференции.
pub async fn update_conference(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<PatchConferenceRequest>,
) -> Response {
    track("conferences/{id}");
    if let Err(resp) = check_permission(&auth, Some(&id)) {
        return resp;
    }
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference not found");
    };
    if let Some(new_name) = body.display_name.filter(|s| !s.trim().is_empty()) {
        if let Err(e) = Rooms::update_async(&state, &room.id, Some(new_name), None, None, None).await {
            return error_json(500, &format!("Storage error: {e}"));
        }
    }
    audit_room(
        &state,
        &auth.subject,
        "conference.updated",
        &room.id.to_string(),
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );
    let updated = find_room(&state, &id).unwrap_or(room);
    state.notify_admin("conferences:updated", json!({ "id": updated.id.to_string(), "display_name": updated.name }));
    state.notify_admin("conferences", json!({ "action": "updated", "id": updated.id.to_string() }));
    let is_stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&updated.id);
    Json(json!({ "conference": session_json(&state, &updated, connections_limit(&state), is_stopped) })).into_response()
}

/// `DELETE /api/v3/conferences/{id}` → 204 | 404 (аудит §4.5).
pub async fn delete_conference(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
) -> Response {
    track("conferences/{id}");
    if let Err(resp) = check_permission(&auth, Some(&id)) {
        return resp;
    }
    match find_room(&state, &id) {
        Some(room) => match Rooms::delete_async(&state, &room.id).await {
            Ok(true) => {
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").remove(&room.id);
                state.notify_admin("conferences:deleted", json!({ "id": room.id.to_string() }));
                state.notify_admin("conferences", json!({ "action": "deleted", "id": room.id.to_string() }));
                audit_room(
                    &state,
                    &auth.subject,
                    "conference.deleted",
                    &room.id.to_string(),
                    client_ip.as_ref().map(|c| c.0 .0.as_str()),
                );
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

/// `GET /api/v3/sessions/{id}/abonents` (и `/ui/sessions/{id}/abonents`) → динамический ростер абонентов.
pub async fn session_abonents(State(state): State<SharedState>, Path(id): Path<String>) -> Response {
    track("sessions/{id}/abonents");
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Session not found");
    };
    let participants = state.room_hub.list_participants(&room.id);
    let connected: Vec<Value> = participants
        .iter()
        .map(|p| {
            let (type_id, connection_url) = if p.is_recorder {
                ("recorder", format!("recorder://{}", p.participant_id))
            } else if p.participant_id.contains("@sip-gateway") {
                ("sip", format!("sip://{}", p.display_name))
            } else if p.participant_id.contains("@h323-gateway") {
                ("h323", format!("h323://{}", p.display_name))
            } else {
                ("webrtc", format!("webrtc://{}", p.participant_id))
            };
            json!({
                "abonent_id": p.participant_id,
                "connection_id": p.participant_id,
                "contact_id": p.participant_id,
                "call_string_id": p.participant_id,
                "video_layout_id": "grid",
                "display_name": p.display_name,
                "connection_url": connection_url,
                "type_id": type_id,
                "connected_at": p.connected_at,
                "payload": {
                    "settings": {
                        "slot_render_type": "auto",
                        "auto_call": false,
                        "audio_rx_enabled": true,
                        "audio_tx_enabled": !p.is_audio_muted,
                        "volume_rx": 100,
                        "volume_tx": 100,
                        "video_rx_enabled": true,
                        "video_tx_enabled": !p.is_video_muted,
                        "crown": false,
                        "hide_display_name": false
                    }
                }
            })
        })
        .collect();
    let count = connected.len();
    Json(json!({
        "abonents": connected.clone(),
        "connected": connected,
        "connecting": [],
        "disconnected": [],
        "scheduled": [],
        "count": count
    }))
    .into_response()
}

/// Тело запроса переключения настроек абонента (`POST /api/v3/abonents/{id}/{setting}`).
#[derive(Debug, Default, Deserialize)]
pub struct SwitchAbonentSettingsRequest {
    #[serde(default)]
    pub enable: Option<bool>,
    #[serde(default)]
    pub outgoing: Option<bool>,
    #[serde(default)]
    pub volume: Option<u32>,
}

/// `POST /api/v3/abonents/{connection_id}/{settings_type}` — переключение mute микрофона/камеры.
pub async fn switch_abonent_settings(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((connection_id, setting_type)): Path<(String, String)>,
    Json(body): Json<SwitchAbonentSettingsRequest>,
) -> Response {
    track("abonents/{id}/{setting}");
    let Some((room_id, _snapshot)) = state.room_hub.find_participant_room(&connection_id) else {
        return error_json(404, "Abonent not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room_id.to_string())) {
        return resp;
    }

    let audio_muted = match setting_type.as_str() {
        "audio" | "microphone" => Some(!body.enable.unwrap_or(true)),
        _ => None,
    };
    let video_muted = match setting_type.as_str() {
        "video" | "camera" => Some(!body.enable.unwrap_or(true)),
        _ => None,
    };

    state.room_hub.update_participant_media(&room_id, &connection_id, audio_muted, video_muted, None);
    audit_room(
        &state,
        &auth.subject,
        &format!("abonent.{setting_type}"),
        &connection_id,
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );
    state.notify_admin(
        &format!("session:{}:abonents", room_id),
        json!({
            "connection_id": connection_id,
            "setting": setting_type,
            "audio_muted": audio_muted,
            "video_muted": video_muted,
        }),
    );
    Json(json!({ "status": "ok" })).into_response()
}

/// `DELETE /api/v3/abonents/{connection_id}` — исключение участника (Kick).
pub async fn remove_abonent(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(connection_id): Path<String>,
) -> Response {
    track("abonents/{id}");
    let Some((room_id, _snapshot)) = state.room_hub.find_participant_room(&connection_id) else {
        return error_json(404, "Abonent not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room_id.to_string())) {
        return resp;
    }

    Rooms::kick_participant(&state, &room_id, &connection_id);
    audit_room(&state, &auth.subject, "abonent.removed", &connection_id, client_ip.as_ref().map(|c| c.0 .0.as_str()));
    StatusCode::NO_CONTENT.into_response()
}

/// `DELETE /api/v3/sessions/{id}/abonents/{abonent_id}` (и `/ui/sessions/...`) — исключение абонента из сессии.
pub async fn remove_session_abonent(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((session_id, abonent_id)): Path<(String, String)>,
) -> Response {
    track("sessions/{id}/abonents/{abonent_id}");
    if let Err(resp) = check_permission(&auth, Some(&session_id)) {
        return resp;
    }
    let Some(room) = find_room(&state, &session_id) else {
        return error_json(404, "Session not found");
    };

    Rooms::kick_participant(&state, &room.id, &abonent_id);
    audit_room(&state, &auth.subject, "abonent.removed", &abonent_id, client_ip.as_ref().map(|c| c.0 .0.as_str()));
    StatusCode::NO_CONTENT.into_response()
}

/// Тело запроса редактирования абонента.
#[derive(Debug, Default, Deserialize)]
pub struct EditAbonentRequest {
    #[serde(default)]
    pub display_name: Option<String>,
}

/// `PATCH /api/v3/abonents/{connection_id}` — переименование абонента.
pub async fn edit_abonent(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(connection_id): Path<String>,
    Json(body): Json<EditAbonentRequest>,
) -> Response {
    track("abonents/{id}");
    let Some((room_id, _snapshot)) = state.room_hub.find_participant_room(&connection_id) else {
        return error_json(404, "Abonent not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room_id.to_string())) {
        return resp;
    }

    if let Some(ref name) = body.display_name {
        state.room_hub.update_participant_media(&room_id, &connection_id, None, None, Some(name.clone()));
        audit_room(
            &state,
            &auth.subject,
            "abonent.renamed",
            &connection_id,
            client_ip.as_ref().map(|c| c.0 .0.as_str()),
        );
        state.notify_admin(
            &format!("session:{}:abonents", room_id),
            json!({
                "connection_id": connection_id,
                "display_name": name,
            }),
        );
    }
    Json(json!({ "status": "ok" })).into_response()
}

/// `GET /api/v3/engine/abonents` → `{abonents:[], count:0}`.
pub async fn engine_abonents() -> Response {
    track("engine/abonents");
    Json(json!({ "abonents": [], "count": 0 })).into_response()
}
