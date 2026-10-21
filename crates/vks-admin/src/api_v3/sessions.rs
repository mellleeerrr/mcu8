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
    /// Дополнительные поля 8 вкладок конференции
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, Value>,
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
    /// Дополнительные поля 8 вкладок конференции
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, Value>,
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

/// Эталонный payload конференции (ref-92) — обязателен для валидатора старта в UI.
static CONFERENCE_PAYLOAD_DEFAULTS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("conference_payload_defaults.json"))
        .expect("conference_payload_defaults.json must be valid JSON")
});

pub(crate) fn conference_payload_defaults() -> serde_json::Map<String, Value> {
    CONFERENCE_PAYLOAD_DEFAULTS.as_object().cloned().unwrap_or_default()
}

/// JSON сессии/конференции (мапперы `Fv`/`dy`/`zZ` мягкие).
fn session_json(state: &AppState, r: &RoomRecord, connections_limit: u32, is_stopped: bool) -> Value {
    let (state_str, has_stopped) = if is_stopped { ("STOPPED", true) } else { ("ACTIVE", false) };
    let participants = state.room_hub.list_participants(&r.id);
    let sip_count = participants.iter().filter(|p| p.participant_id.contains("@sip-gateway")).count();
    let h323_count = participants.iter().filter(|p| p.participant_id.contains("@h323-gateway")).count();
    let is_recording = participants.iter().any(|p| p.is_recorder);

    // Извлекаем сохраненные настройки 8 вкладок для комнаты
    let stored_settings =
        state.api_v3.conference_settings.lock().ok().and_then(|m| m.get(&r.id).cloned()).unwrap_or_else(|| json!({}));

    // Полный payload эталона (ref-92): UI-валидатор открывает кнопки старта
    // только когда есть `audio_codecs`/`video_codecs`, `broadcast`, `styles` и т.д.
    let mut payload_map = conference_payload_defaults();
    if let Some(stored) = stored_settings.as_object() {
        for (key, value) in stored {
            payload_map.insert(key.clone(), value.clone());
        }
    }
    // Лимит конференции приоритетнее системного умолчания (UI правит его в форме).
    if stored_settings.get("connections_limit").is_none() {
        payload_map.insert("connections_limit".to_string(), json!(connections_limit));
    }
    // Ключи эталона 2.2.1.66, которые UI читает с дефолтами (T1.2).
    payload_map.entry("selfview".to_string()).or_insert_with(|| json!("SELECTIVE"));
    payload_map.entry("cascade".to_string()).or_insert_with(|| json!("none"));

    // Эталон отдаёт null, а не ""; `has_pin` остаётся булевым для формы.
    let text_or_null = |k: &str| {
        stored_settings.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map_or(Value::Null, Value::from)
    };
    let (pin_val, description_val) = (text_or_null("pin"), text_or_null("description"));
    let owner_id_val = stored_settings
        .get("creator_id")
        .or_else(|| stored_settings.get("owner_id"))
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .unwrap_or("00000000-0000-0000-0000-000000000000");
    let layout_val = stored_settings.get("layout").and_then(|v| v.as_str()).unwrap_or("grid");
    let auto_call_val = stored_settings.get("auto_call").and_then(|v| v.as_bool()).unwrap_or(false);

    let mut base_obj = json!({
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
        "owner_id": owner_id_val,
        "creator_id":owner_id_val,
        "conference_id":r.id.to_string(),
        "created_at":r.created_ms/1000,
        "duration":stored_settings.get("duration").cloned().unwrap_or(Value::Null),
        "stopped_at":if is_stopped {stored_settings.get("stopped_at").cloned().unwrap_or(Value::Null)} else {Value::Null},
        "session":if is_stopped {Value::Null} else {json!({"id":r.id.to_string(),"display_name":r.name,"started_at":r.created_ms/1000})},
        "start_at": Value::Null,
        "tags": Value::Null,
        "addresses": conference_addresses(
            state,
            stored_settings.get("route").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&r.name),
        ),
        "primary_route":{"id":format!("route-{}",r.id),"name":stored_settings.get("route").and_then(Value::as_str).unwrap_or(&r.name)},
        "primary_dtmf":stored_settings.get("dtmf").filter(|v|!v.is_null()).map(|value|json!({"id":format!("dtmf-{}",r.id),"name":value})),
        "primary_alias":stored_settings.get("alias").and_then(Value::as_str).filter(|s|!s.is_empty()).map(|value|json!({"id":format!("alias-{}",r.id),"name":value})),
        "cascade":"none",
        "pin": pin_val,
        "has_pin": pin_val != Value::Null,
        "description": description_val,
        "layout": layout_val,
        "auto_call": auto_call_val,
        // Карточка списка деструктурирует счётчики (`entity.abonents.rtsp`),
        // а маппер `Mee` — `abonents.abonents.map`; отдаём обе формы сразу.
        "abonents": {
            "count": 0, "abonents": [], "records": [], "rtmp": [], "youtube": [],
            "sip": sip_count, "h323": h323_count, "rtsp": 0, "vnc": 0, "media": 0, "file": 0
        },
        "contacts": { "users": 0, "sip": sip_count, "h323": h323_count, "alias": 0, "rtsp": 0, "vnc": 0, "media": 0, "file": 0 },
        "payload": payload_map
    });

    if let Some(obj) = base_obj.as_object_mut() {
        if let Some(stored_obj) = stored_settings.as_object() {
            for (k, v) in stored_obj {
                if !obj.contains_key(k) {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
    }

    base_obj
}

/// Лимит подключений комнаты (`payload.connections_limit`, из наших настроек).
fn connections_limit(state: &AppState) -> u32 {
    SettingsService::get(state).limits.max_participants_per_room
}

/// Адреса конференции для входа внешних клиентов (эталон `.92`: `sip:/h323:` — LAN-IP
/// сервера, `webrtc:` — локальный адрес; UI показывает их в карточке конференции).
fn conference_addresses(state: &AppState, name: &str) -> Value {
    let ip = state.server_addr.get().map(String::as_str).unwrap_or("127.0.0.1");
    // IPv6 в SIP/H.323 URI — в квадратных скобках.
    let host = if ip.contains(':') { format!("[{ip}]") } else { ip.to_string() };
    // Имя может быть любым (пробелы/кириллица): в URI — только безопасный набор.
    let name: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    json!({
        "sip": format!("sip:{name}@{host}"),
        "h323": format!("h323:{name}@{host}"),
        "webrtc": format!("sip:{name}@127.0.0.1"),
    })
}

/// Очищает ID от лишних пробелов и фигурных скобок (вендорный баг фронтенда: `{id}}`).
pub fn sanitize_id(id: &str) -> &str {
    id.trim().trim_start_matches('{').trim_end_matches('}')
}

pub fn find_room(state: &AppState, id: &str) -> Option<RoomRecord> {
    let clean = sanitize_id(id);
    if let Ok(uuid) = Uuid::parse_str(clean) {
        if let Some(r) = Rooms::get(state, &RoomId::from_uuid(uuid)) {
            return Some(r);
        }
    }
    let rooms = state.rooms.lock().expect("rooms store poisoned");
    rooms.values().find(|r| r.name == clean || r.name.eq_ignore_ascii_case(clean)).cloned()
}

#[allow(clippy::result_large_err)]
pub fn check_permission(auth: &AuthContext, target_room_id: Option<&str>) -> Result<(), Response> {
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

/// Room-scoped bearer tokens must not enumerate other rooms.
fn filter_sessions_scope(mut payload: Value, auth: &AuthContext) -> Value {
    let Some(scope) = auth.room_scope.as_deref() else {
        return payload;
    };
    for field in ["sessions", "conferences"] {
        if let Some(items) = payload.get_mut(field).and_then(Value::as_array_mut) {
            items
                .retain(|item| item.get("id").and_then(Value::as_str).is_some_and(|id| id.eq_ignore_ascii_case(scope)));
            payload["count"] = json!(items.len());
        }
    }
    payload
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
pub async fn ui_sessions(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<SessionsQuery>,
) -> Response {
    track("ui/sessions");
    Json(filter_sessions_scope(sessions_payload(&state, &query), &auth)).into_response()
}

/// `GET /api/v3/ui/sessions/{id}` или `GET /api/v3/sessions/{id}` → `{session:{…}}` (404 JSON если нет).
pub async fn session(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("sessions/{id}");
    match find_room(&state, &id) {
        Some(room) => {
            if let Err(resp) = check_permission(&auth, Some(&room.id.to_string())) {
                return resp;
            }
            let is_stopped =
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&room.id);
            Json(json!({ "session": session_json(&state, &room, connections_limit(&state), is_stopped) }))
                .into_response()
        }
        None => error_json(404, "Session not found"),
    }
}

/// `GET /api/v3/sessions` — тот же список (не-ui форма).
pub async fn sessions(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<SessionsQuery>,
) -> Response {
    track("sessions");
    Json(filter_sessions_scope(sessions_payload(&state, &query), &auth)).into_response()
}

/// `POST /api/v3/sessions` — старт сессии конференции (или создание быстрой сессии).
/// Сессия наследует ресурсы конференции и получает «Общую» раскладку (идемпотентно).
/// При ошибке persist откатывает in-memory состояние и возвращает 500.
async fn prepare_session_resources(state: &SharedState, room_id: &str, creator: &str) -> Result<(), Response> {
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    let source = format!("conferences:{room_id}:audiochannels");
    let target = format!("sessions:{room_id}:audiochannels");
    if let Some(links) = resources.links.get(&source).cloned() {
        // Уже существующие session-ссылки (раскладки прошлого запуска) сохраняются.
        resources.links.entry(target).or_insert(links);
    }
    super::resources::inherit_and_ensure_default_layout(&mut resources, room_id, creator);
    super::resources::commit(state, &mut resources, previous).await
}

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
                let was_stopped =
                    state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").remove(&r.id);
                if was_stopped {
                    // Повторный запуск — новая сессия в истории (новый started_at).
                    match crate::rooms::Rooms::reset_created_ms(&state, &r.id).await {
                        Ok(Some(record)) => record,
                        Ok(None) => return error_json(404, "Conference not found"),
                        Err(error) => return error_json(500, &format!("Storage error: {error}")),
                    }
                } else {
                    r
                }
            }
            None => return error_json(404, "Conference not found"),
        }
    } else {
        match Rooms::create_async(&state, "Quick Session".to_string()).await {
            Ok(r) => r,
            Err(e) => return error_json(500, &format!("Storage error: {e}")),
        }
    };
    if let Ok(mut settings) = state.api_v3.conference_settings.lock() {
        ensure_conference_identity(&mut settings, &room, &auth.subject, true);
    }
    // A live room inherits its definition resources; template contacts remain stable.
    if let Err(response) = prepare_session_resources(&state, &room.id.to_string(), &auth.subject).await {
        return response;
    }
    let contacts = state.api_v3.resources.lock().await.contacts.get(&room.id.to_string()).cloned().unwrap_or_default();
    let autocall = state
        .api_v3
        .conference_settings
        .lock()
        .expect("conference_settings poisoned")
        .get(&room.id)
        .and_then(|v| v.get("ui_autocall_on_session_start"))
        .and_then(Value::as_str)
        .is_none_or(|v| v != "OFF");
    for contact in contacts {
        let record = super::call_strings::CallStringRecord {
            id: contact["id"].as_str().unwrap_or_default().to_string(),
            uri: contact["uri"].as_str().unwrap_or_default().to_string(),
            display_name: contact["display_name"].as_str().unwrap_or_default().to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            payload: super::call_strings::default_contact_payload(contact.get("payload")),
        };
        if autocall && contact["payload"]["auto_call"].as_bool().unwrap_or(true) {
            if let Err(response) = super::call_strings::queue_call(&state, room.id, record.clone()) {
                tracing::warn!(room=%room.id,uri=%record.uri,status=%response.status(),"Autocall could not be admitted");
                state
                    .api_v3
                    .scheduled_calls
                    .lock()
                    .expect("scheduled_calls poisoned")
                    .entry(room.id)
                    .or_default()
                    .push(record);
            }
        } else {
            state
                .api_v3
                .scheduled_calls
                .lock()
                .expect("scheduled_calls poisoned")
                .entry(room.id)
                .or_default()
                .push(record);
        }
    }
    state.notify_admin("sessions:started", json!({"id":room.id.to_string(),"conference":{"id":room.id.to_string()},"session":session_json(&state,&room,connections_limit(&state),false)}));
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
    // Быстрая сессия — полноценная сессия: creator/route/DTMF и маркер старта,
    // иначе карточка и история показывают «ID: -», а delete не знает о старте.
    if let Ok(mut settings) = state.api_v3.conference_settings.lock() {
        ensure_conference_identity(&mut settings, &room, &auth.subject, true);
    }
    // Быстрая сессия тоже имеет «Общую» раскладку — иначе UI-сессия падает на `.some`.
    if let Err(response) = prepare_session_resources(&state, &room.id.to_string(), &auth.subject).await {
        return response;
    }
    state.notify_admin("sessions:started", json!({"id":room.id.to_string(),"conference":{"id":room.id.to_string()},"session":session_json(&state,&room,connections_limit(&state),false)}));
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
            // Останавливать можно только живую сессию: create_conference и
            // boot-reconcile заранее помечают комнату stopped, поэтому DELETE
            // /sessions на никогда не запускавшейся конференции — идемпотентный
            // 204 без фантомной записи в истории (симметрично delete-пути).
            let was_stopped =
                state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&room.id);
            if was_stopped {
                return StatusCode::NO_CONTENT.into_response();
            }
            state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").insert(room.id);
            super::call_strings::cancel_pending_calls(&state, &room.id);
            let stopped_participants: Vec<String> = state
                .room_hub
                .list_participants(&room.id)
                .into_iter()
                .map(|participant| participant.participant_id)
                .collect();
            // T2.2: снимок истории ДО очистки attempts/участников.
            super::history::record_stopped(&state, &room, &auth.subject, "Manual").await;
            for participant in &stopped_participants {
                Rooms::kick_participant(&state, &room.id, participant);
            }
            state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned").remove(&room.id);
            state
                .api_v3
                .in_flight_calls
                .lock()
                .expect("in_flight_calls poisoned")
                .retain(|(room_id, _), _| *room_id != room.id);
            state
                .api_v3
                .call_attempts
                .lock()
                .expect("call_attempts poisoned")
                .retain(|(room_id, _), _| *room_id != room.id);
            if let Ok(mut settings) = state.api_v3.abonent_settings.lock() {
                for participant in &stopped_participants {
                    settings.remove(participant);
                }
            }
            if let Ok(mut processing) = state.api_v3.abonent_processing.lock() {
                for participant in &stopped_participants {
                    processing.remove(participant);
                }
            }
            let _ = Rooms::update_async(&state, &room.id, None, None, Some(0), Some(0)).await;
            state.notify_admin(
                &format!("session:{}:abonents", room.id),
                json!({"action":"stopped","room_id":room.id.to_string(),"count":0}),
            );
            state.notify_admin(
                "sessions:stopped",
                json!({"id":room.id.to_string(),"session":{"id":room.id.to_string()}}),
            );
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
pub async fn conferences(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<SessionsQuery>,
) -> Response {
    track("conferences");
    Json(filter_sessions_scope(conferences_payload(&state, &query), &auth)).into_response()
}

/// `GET /api/v3/ui/conferences` — тот же список для мапперов `dy/zZ`.
pub async fn ui_conferences(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<SessionsQuery>,
) -> Response {
    track("ui/conferences");
    Json(filter_sessions_scope(conferences_payload(&state, &query), &auth)).into_response()
}

/// `POST /api/v3/conferences` — создание комнаты тем же внутренним путём,
/// что `POST /api/v1/rooms` (без дублирования логики).
/// Свободный 9-значный DTMF-код конференции (vendor-контракт: `primary_dtmf`).
fn free_dtmf(settings: &std::collections::HashMap<vks_core::RoomId, Value>) -> String {
    let taken: std::collections::HashSet<&str> =
        settings.values().filter_map(|value| value.get("dtmf").and_then(Value::as_str)).collect();
    for _ in 0..32 {
        let candidate = format!("{:09}", uuid::Uuid::new_v4().as_u128() % 1_000_000_000);
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
    }
    format!("{:09}", uuid::Uuid::new_v4().as_u128() % 1_000_000_000)
}

/// Дозаполняет route/dtmf у сессии, созданной до T2.2 или через quick-start:
/// иначе history/карточка показывают «ID: -», а vendor-UI ждёт реальные значения.
fn ensure_conference_identity(
    settings: &mut std::collections::HashMap<vks_core::RoomId, Value>,
    room: &RoomRecord,
    creator: &str,
    mark_started: bool,
) {
    let missing_dtmf = settings.get(&room.id).and_then(|value| value.get("dtmf")).is_none_or(|value| value.is_null());
    let code = missing_dtmf.then(|| free_dtmf(settings));
    let entry = settings.entry(room.id).or_insert_with(|| json!({}));
    let Some(object) = entry.as_object_mut() else { return };
    object.entry("route").or_insert_with(|| json!(room.name));
    object.entry("creator_id").or_insert_with(|| json!(creator));
    object.entry("owner_id").or_insert_with(|| json!(creator));
    if mark_started {
        object.insert("started_at".to_string(), json!(crate::state::now_unix_ms() / 1000));
        // Новый запуск — новая сессия: durable-маркер прошлой сессии больше не
        // должен блокировать запись истории при её удалении.
        object.remove("history_recorded_at");
    }
    if let Some(code) = code {
        object.insert("dtmf".to_string(), json!(code));
    }
}

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
    let record = match Rooms::create_async(&state, name.clone()).await {
        Ok(r) => r,
        Err(e) => return error_json(500, &format!("Storage error: {e}")),
    };
    // Раскладку/аудиоканал конференции создаёт сам UI после POST (он шлёт
    // `/ui/video_layouts` + link) — серверный сид дал бы дубль «Общей».
    let resources_guard = state.api_v3.resources.lock().await;
    {
        let mut initial_settings = json!({"creator_id":auth.subject,"owner_id":auth.subject,"route":name});
        if let Some(cur_obj) = initial_settings.as_object_mut() {
            if let Some(ref pin) = body.pin {
                cur_obj.insert("pin".to_string(), json!(pin));
            }
            if let Some(ref p) = body.payload {
                if let Some(obj) = p.as_object() {
                    for (k, v) in obj {
                        cur_obj.insert(k.clone(), v.clone());
                    }
                }
            }
            for (k, v) in body.extra {
                cur_obj.insert(k, v);
            }
        }
        if let Ok(mut settings_map) = state.api_v3.conference_settings.lock() {
            if let Some(object) = initial_settings.as_object_mut() {
                object.entry("dtmf").or_insert_with(|| json!(free_dtmf(&settings_map)));
            }
            settings_map.insert(record.id, initial_settings);
        }
    }
    if let Err(error) = super::resources::persist(&state, &resources_guard).await {
        state.api_v3.conference_settings.lock().unwrap().remove(&record.id);
        drop(resources_guard);
        let _ = Rooms::delete_async(&state, &record.id).await;
        return error_json(500, &format!("Storage error: {error}"));
    }
    drop(resources_guard);
    state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").insert(record.id);
    state.notify_admin("conferences:created", json!({ "id": record.id.to_string(), "display_name": record.name }));
    state.notify_admin("conferences", json!({ "action": "created", "id": record.id.to_string() }));
    audit_room(
        &state,
        &auth.subject,
        "conference.created",
        &record.id.to_string(),
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );
    Json(json!({ "conference": session_json(&state, &record, connections_limit(&state), true) })).into_response()
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

    let resources_guard = state.api_v3.resources.lock().await;
    let previous_settings = state.api_v3.conference_settings.lock().unwrap().get(&room.id).cloned();
    {
        if let Ok(mut settings_map) = state.api_v3.conference_settings.lock() {
            let current = settings_map.entry(room.id).or_insert_with(|| json!({}));
            if !current.is_object() {
                *current = json!({});
            }
            if let Some(cur_obj) = current.as_object_mut() {
                if let Some(ref pin) = body.pin {
                    cur_obj.insert("pin".to_string(), json!(pin));
                }
                if let Some(ref p) = body.payload {
                    if let Some(obj) = p.as_object() {
                        for (k, v) in obj {
                            cur_obj.insert(k.clone(), v.clone());
                        }
                    }
                }
                for (k, v) in body.extra {
                    cur_obj.insert(k, v);
                }
            }
        }
    }
    if let Err(error) = super::resources::persist(&state, &resources_guard).await {
        let mut settings = state.api_v3.conference_settings.lock().unwrap();
        if let Some(previous) = previous_settings {
            settings.insert(room.id, previous);
        } else {
            settings.remove(&room.id);
        }
        return error_json(500, &format!("Storage error: {error}"));
    }
    drop(resources_guard);

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
        Some(room) => {
            let settings_snapshot =
                state.api_v3.conference_settings.lock().ok().and_then(|settings| settings.get(&room.id).cloned());
            // В историю попадает только реально запущенная сессия (маркер
            // `started_at` ставится стартом/quick-start), и только если удаление
            // действительно состоялось.
            let started = settings_snapshot.as_ref().is_some_and(|settings| settings.get("started_at").is_some());
            // `clean` стирает историю, но не комнаты: запись при удалении уже
            // остановленной/удалённой сессии воскресила бы её. Маркер durable
            // (пишется вместе с историей), stopped_sessions — для текущего процесса.
            let already_recorded =
                settings_snapshot.as_ref().is_some_and(|settings| settings.get("history_recorded_at").is_some())
                    || state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&room.id);
            let participant_ids: Vec<String> = state
                .room_hub
                .list_participants(&room.id)
                .into_iter()
                .map(|participant| participant.participant_id)
                .collect();
            match Rooms::delete_async(&state, &room.id).await {
                Ok(true) => {
                    if started && !already_recorded {
                        super::history::record_stopped_with_settings(
                            &state,
                            &room,
                            &auth.subject,
                            "Deleted",
                            settings_snapshot,
                        )
                        .await;
                    }
                    super::call_strings::cancel_pending_calls(&state, &room.id);
                    for participant in &participant_ids {
                        Rooms::kick_participant(&state, &room.id, participant);
                    }
                    state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned").remove(&room.id);
                    state
                        .api_v3
                        .in_flight_calls
                        .lock()
                        .expect("in_flight_calls poisoned")
                        .retain(|(room_id, _), _| *room_id != room.id);
                    state
                        .api_v3
                        .call_attempts
                        .lock()
                        .expect("call_attempts poisoned")
                        .retain(|(room_id, _), _| *room_id != room.id);
                    state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").remove(&room.id);
                    state.api_v3.conference_settings.lock().ok().map(|mut m| m.remove(&room.id));
                    super::resources::purge_room_resources(&state, &room.id.to_string()).await;
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
            }
        }
        None => error_json(404, "Conference not found"),
    }
}

/// `GET /api/v3/conferences/{id}/contacts` → `{contacts:[], count:0}`.
pub async fn conference_contacts(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("conferences/{id}/contacts");
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let contacts = state.api_v3.resources.lock().await.contacts.get(&room.id.to_string()).cloned().unwrap_or_default();
    Json(json!({ "contacts": contacts, "count": contacts.len() })).into_response()
}

/// `details.*` — источник для engine-мапперов UI (`dY`/`QQ`): `statuses.userAgent`
/// (`isWebrtcAbonent()` ищет «Trueconf WebRTC Gateway»), `settings.*.isEnabled` и
/// медиа-метрики. `present` = `enabled`, иначе карточка считает поток недоступным.
fn abonent_details(
    proto: &str,
    audio_rx: bool,
    audio_tx: bool,
    video_rx: bool,
    video_tx: bool,
    volume_rx: i32,
    volume_tx: i32,
) -> Value {
    let user_agent = match proto {
        "sip" => "Trueconf SIP Gateway",
        "h323" => "Trueconf H.323 Gateway",
        "recorder" => "Trueconf Recorder",
        _ => "Trueconf WebRTC Gateway",
    };
    let media = |enabled: bool, volume: i32| {
        json!({
            "enabled": enabled,
            "volume": volume,
            "bitrate": 0, "codec": "", "loss": 0.0, "jitter": 0, "present": enabled, "active": enabled,
            "packets": 0, "bytes": 0, "lost_packets": 0, "lossrate": 0, "requested_bitrate": 0,
            "resolution": "0x0", "fps": 0, "width": 0, "height": 0, "frame_rate": 0
        })
    };
    json!({
        "user_agent": user_agent,
        "bitrate": 0,
        "video_supported": true,
        "content_supported": true,
        "fecc": proto == "h323",
        "fec": false,
        "srtp": false,
        "self_view": false,
        "cascade": "none",
        "selection": false,
        "audio": {
            "auto_gain_control_switch": "DEFAULT",
            "noise_suppressor_switch": "DEFAULT",
            "auto_gain_control": false,
            "noise_suppressor": false,
            "power": 0,
            "rx": media(audio_rx, volume_rx),
            "tx": media(audio_tx, volume_tx)
        },
        "video": { "rx": media(video_rx, 0), "tx": media(video_tx, 0) },
        "content": { "rx": media(false, 0), "tx": media(false, 0) }
    })
}

/// Сериализует снапшот участника в DTO абонента, полностью совместимый с мапперами фронтенда (`dY`/`Lh`).
pub fn participant_to_abonent_json_with_settings(
    p: &crate::hub::ParticipantSnapshot,
    media_opt: Option<&super::AbonentMediaState>,
) -> Value {
    let (proto, connection_url, uri) = if p.is_recorder {
        ("recorder", format!("recorder://{}", p.participant_id), format!("recorder:{}", p.participant_id))
    } else if p.participant_id.contains("@sip-gateway") {
        let uri =
            if p.display_name.starts_with("sip:") { p.display_name.clone() } else { format!("sip:{}", p.display_name) };
        ("sip", format!("sip://{}", p.display_name), uri)
    } else if p.participant_id.contains("@h323-gateway") {
        let uri = if p.display_name.starts_with("h323:") {
            p.display_name.clone()
        } else {
            format!("h323:{}", p.display_name)
        };
        ("h323", format!("h323://{}", p.display_name), uri)
    } else {
        ("webrtc", format!("webrtc://{}", p.participant_id), format!("webrtc:{}", p.participant_id))
    };

    let audio_rx_enabled = media_opt.map(|m| !m.audio_rx_muted).unwrap_or(!p.is_audio_muted);
    let audio_tx_enabled = media_opt.map(|m| !m.audio_tx_muted).unwrap_or(true);
    let video_rx_enabled = media_opt.map(|m| !m.video_rx_muted).unwrap_or(!p.is_video_muted);
    let video_tx_enabled = media_opt.map(|m| !m.video_tx_muted).unwrap_or(true);
    let volume_rx = media_opt.map(|m| m.volume_rx).unwrap_or(0);
    let volume_tx = media_opt.map(|m| m.volume_tx).unwrap_or(0);

    let details = abonent_details(
        proto,
        audio_rx_enabled,
        audio_tx_enabled,
        video_rx_enabled,
        video_tx_enabled,
        volume_rx,
        volume_tx,
    );

    json!({
        "id": p.participant_id,
        "abonent_id": p.participant_id,
        "connection_id": p.participant_id,
        "contact_id": p.participant_id,
        "call_string_id": p.participant_id,
        "video_layout_id": "grid",
        "display_name": p.display_name,
        "connection_url": connection_url,
        "uri": uri,
        "type_id": "OUTGOING",
        "protocol": proto,
        "contact_type": "call_strings",
        "crown": "EQUAL",
        "connected_at": p.connected_at,
        "details": details,
        "payload": {
            "slot_render_type": "DEFAULT",
            "auto_call": false,
            "audio_rx_enabled": audio_rx_enabled,
            "audio_tx_enabled": audio_tx_enabled,
            "volume_rx": volume_rx,
            "volume_tx": volume_tx,
            "video_rx_enabled": video_rx_enabled,
            "video_tx_enabled": video_tx_enabled,
            "crown": "EQUAL",
            "hide_display_name": false
        }
    })
}

pub(crate) fn native_stop_reason(reason: &str) -> &'static str {
    let reason = reason.to_ascii_lowercase();
    if reason.contains("busy") || reason.contains("486") {
        "Busy Here"
    } else if reason.contains("timeout") || reason.contains("timed out") {
        "Request Timeout"
    } else if reason.contains("codec") || reason.contains("media") || reason.contains("sdp") {
        "Unsupported Media Type"
    } else if reason.contains("cancel") || reason.contains("session ended") {
        "Ended By Local"
    } else {
        "Ended By Remote"
    }
}

/// Сериализует снимок участника в формат абонента PNM UI.
pub fn participant_to_abonent_json(p: &crate::hub::ParticipantSnapshot) -> Value {
    participant_to_abonent_json_with_settings(p, None)
}

/// Сериализует запись строки вызова (in-flight или standby) в формат абонента.
pub fn call_record_to_abonent_json(r: &super::call_strings::CallStringRecord, state_str: &str) -> Value {
    let uri = if r.uri.contains(':') { r.uri.clone() } else { format!("sip:{}", r.uri) };
    let proto = uri.split(':').next().unwrap_or("sip");
    let details = abonent_details(proto, true, true, true, true, 0, 0);

    let mut value = json!({
        "id": r.id,
        "abonent_id": r.id,
        "connection_id": r.id,
        "contact_id": r.id,
        "call_string_id": r.id,
        "video_layout_id": "grid",
        "display_name": r.display_name,
        "connection_url": uri,
        "uri": uri,
        "type_id": "OUTGOING",
        "protocol": proto,
        "contact_type": "call_strings",
        "state": state_str,
        "crown": "EQUAL",
        "connected_at": r.created_at,
        "details": details
    });
    // Payload — из записи call string (плоские `audio_*_enabled` — их читает маппер `Lh`).
    let payload = super::call_strings::default_contact_payload(Some(&r.payload));
    value["payload"] = payload;
    value["crown"] = value["payload"]["crown"].clone();
    fill_native_stream_settings(&mut value);
    value
}

/// The stock monitor reads stream settings from the engine DTO as well as payload.
fn fill_native_stream_settings(value: &mut Value) {
    for media in ["audio", "video", "content"] {
        for direction in ["rx", "tx"] {
            let field = format!("{media}_{direction}_enabled");
            let enabled = value["payload"][&field].as_bool().unwrap_or(false);
            value["details"][media][direction]["enabled"] = json!(enabled);
            if media == "audio" {
                value["details"][media][direction]["volume"] = value["payload"][format!("volume_{direction}")].clone();
            }
        }
    }
}

/// Forward measured counters; negotiated codecs are not invented bitrate/fps values.
fn attach_gateway_statistics(state: &SharedState, pid: &str, value: &mut Value) {
    fill_native_stream_settings(value);
    let processing = state
        .api_v3
        .abonent_processing
        .lock()
        .expect("abonent_processing poisoned")
        .get(pid)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let agc = processing["auto_gain_control"].as_str().unwrap_or("DEFAULT");
    let noise = processing["noise_suppression"].as_str().unwrap_or("DEFAULT");
    value["details"]["audio"]["auto_gain_control_switch"] = json!(agc);
    value["details"]["audio"]["noise_suppressor_switch"] = json!(noise);
    value["details"]["audio"]["auto_gain_control"] = json!(false);
    value["details"]["audio"]["noise_suppressor"] = json!(false);
    value["payload"]["auto_gain_control"] = json!(agc);
    value["payload"]["noise_suppression"] = json!(noise);
    let Some(stats) = state.outbound_dialer().and_then(|dialer| dialer.media_stats(pid)) else {
        return;
    };
    for media in ["audio", "video"] {
        for direction in ["rx", "tx"] {
            if let Some(codec) = stats.get(format!("{media}_codec")) {
                value["details"][media][direction]["codec"] = codec.clone();
            }
            if let Some(bitrate) = stats.get(format!("{media}_{direction}_bitrate_bps")) {
                value["details"][media][direction]["bitrate"] = bitrate.clone();
            }
            for metric in ["packets", "bytes"] {
                if let Some(counter) = stats.get(format!("{media}_{direction}_{metric}")) {
                    value["details"][media][direction][metric] = counter.clone();
                }
            }
        }
    }
    if let Some(running) = stats.get("audio_codec_running").and_then(Value::as_bool) {
        for direction in ["rx", "tx"] {
            value["details"]["audio"][direction]["present"] = json!(running);
        }
    }
    if let Some(running) = stats.get("video_codec_running").and_then(Value::as_bool) {
        for direction in ["rx", "tx"] {
            value["details"]["video"][direction]["present"] = json!(running);
        }
    }
    for (direction, field) in [("rx", "decoded_video_fps"), ("tx", "encoded_video_fps")] {
        if let Some(fps) = stats.get(field) {
            value["details"]["video"][direction]["frame_rate"] = fps.clone();
        }
    }
    if stats["video_tx_packets"].as_u64().unwrap_or(0) > 0 {
        // The gateway encoder always emits 640x360 baseline H.264.
        value["details"]["video"]["tx"]["width"] = json!(640);
        value["details"]["video"]["tx"]["height"] = json!(360);
        value["details"]["video"]["tx"]["resolution"] = json!("640x360");
    }
    if stats["decoded_video_frames"].as_u64().unwrap_or(0) > 0 {
        // The gateway decoder normalises every incoming stream to the same
        // composite tile, so this is the real decoded geometry.
        value["details"]["video"]["rx"]["width"] = json!(640);
        value["details"]["video"]["rx"]["height"] = json!(360);
        value["details"]["video"]["rx"]["resolution"] = json!("640x360");
    }
    value["media"] = stats;
}

/// Both the native info modal and the room roster must retain the dial intent.
/// A participant's editable display label is not a callable SIP/H.323 address.
fn attach_call_contact(value: &mut Value, record: &super::call_strings::CallStringRecord, has_media_settings: bool) {
    value["contact_id"] = json!(record.id);
    value["call_string_id"] = json!(record.id);
    value["uri"] = json!(record.uri);
    value["connection_url"] = json!(record.uri);
    value["protocol"] = json!(record.uri.split(':').next().unwrap_or("sip"));
    let mut payload = super::call_strings::default_contact_payload(Some(&record.payload));
    if has_media_settings {
        for field in
            ["audio_rx_enabled", "audio_tx_enabled", "video_rx_enabled", "video_tx_enabled", "volume_rx", "volume_tx"]
        {
            payload[field] = value["payload"][field].clone();
        }
    }
    value["payload"] = payload;
    value["crown"] = value["payload"]["crown"].clone();
}

/// Раскладка участника: его individual-раскладка, иначе «Общая» комнаты.
/// UI считает `hasIndividualLayout = !defaults.some(l => l.layoutId === entity.layoutId)`.
pub async fn session_abonents(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("sessions/{id}/abonents");
    let clean_id = sanitize_id(&id);
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Session not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room.id.to_string())) {
        return resp;
    }

    let participants = state.room_hub.list_participants(&room.id);
    let layout_ids = {
        let resources = state.api_v3.resources.lock().await;
        participants
            .iter()
            .filter_map(|p| {
                super::resources::participant_layout_id(&resources, &room.id, &p.participant_id)
                    .map(|layout_id| (p.participant_id.clone(), layout_id))
            })
            .collect::<std::collections::HashMap<String, String>>()
    };
    let settings_map = state.api_v3.abonent_settings.lock().ok();
    let mut attempts = state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
    let connected: Vec<Value> = participants
        .iter()
        .map(|p| {
            let media = settings_map.as_ref().and_then(|m| m.get(&p.participant_id));
            let mut value = participant_to_abonent_json_with_settings(p, media);
            if let Some(layout_id) = layout_ids.get(&p.participant_id) {
                value["video_layout_id"] = json!(layout_id);
            }
            if let Some(attempt) = attempts
                .iter()
                .find(|((rid, _), a)| *rid == room.id && a.participant_id.as_deref() == Some(p.participant_id.as_str()))
                .map(|(_, a)| a)
            {
                attach_call_contact(&mut value, &attempt.record, media.is_some());
            }
            attach_gateway_statistics(&state, &p.participant_id, &mut value);
            value
        })
        .collect();

    let in_flight_map = state.api_v3.in_flight_calls.lock().expect("in_flight_calls poisoned");
    let connecting: Vec<Value> = in_flight_map
        .iter()
        .filter(|((rid, _), _)| *rid == room.id)
        .map(|(_, record)| call_record_to_abonent_json(record, "connecting"))
        .collect();
    drop(in_flight_map);

    let scheduled_map = state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned");
    let scheduled: Vec<Value> = scheduled_map
        .get(&room.id)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| call_record_to_abonent_json(r, "scheduled"))
        .collect();
    drop(scheduled_map);

    let mut disconnected = Vec::new();
    let in_flight = state.api_v3.in_flight_calls.lock().expect("in_flight_calls poisoned");
    for ((rid, uri), attempt) in attempts.iter_mut() {
        if *rid != room.id
            || in_flight.contains_key(&(*rid, uri.clone()))
            || attempt.participant_id.as_ref().is_some_and(|pid| participants.iter().any(|p| &p.participant_id == pid))
        {
            continue;
        }
        let end = *attempt.completed_at.get_or_insert_with(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
        });
        let reason = attempt.failure_reason.as_deref().unwrap_or("Call ended");
        let mut value = call_record_to_abonent_json(&attempt.record, "disconnected");
        value["failure_reason"] = json!(reason);
        value["stop_reason"] = json!(native_stop_reason(reason));
        value["disconnected_at"] = json!(end);
        value["stopped_at"] = json!(end);
        disconnected.push(value);
    }
    drop(in_flight);
    drop(attempts);

    let mut all_abonents = Vec::new();
    all_abonents.extend(connected.clone());
    all_abonents.extend(connecting.clone());
    all_abonents.extend(scheduled.clone());
    all_abonents.extend(disconnected.clone());
    let count = all_abonents.len();

    Json(json!({
        "abonents": all_abonents,
        "connected": connected,
        "connecting": connecting,
        "disconnected": disconnected,
        "scheduled": scheduled,
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
    pub volume: Option<i32>,
    #[serde(default)]
    pub noise_suppression: Option<String>,
    #[serde(default)]
    pub auto_gain_control: Option<String>,
}

/// PNM's slider uses signed tenths of a dB; the bridge accepts linear percent.
fn db_tenths_to_percent(value: i32) -> u32 {
    (100.0 * 10.0_f64.powf(f64::from(value) / 200.0)).round().clamp(0.0, 1000.0) as u32
}

/// `POST /api/v3/abonents/{connection_id}/{settings_type}` — переключение mute микрофона/камеры и громкости.
pub async fn switch_abonent_settings(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((connection_id, setting_type)): Path<(String, String)>,
    Json(body): Json<SwitchAbonentSettingsRequest>,
) -> Response {
    track("abonents/{id}/{setting}");
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required");
    }
    let Some((room_id, _snapshot)) = state.room_hub.find_participant_room(&connection_id) else {
        return error_json(404, "Abonent not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room_id.to_string())) {
        return resp;
    }

    if matches!(setting_type.as_str(), "noise_suppression" | "auto_gain_control") {
        let requested = if setting_type == "noise_suppression" {
            body.noise_suppression.as_deref()
        } else {
            body.auto_gain_control.as_deref()
        }
        .unwrap_or("DEFAULT");
        if !matches!(requested, "DEFAULT" | "OFF" | "ON") {
            return super::call_strings::validation_error("invalid.audio_processing", &setting_type);
        }
        // Resetting inheritance or explicitly disabling DSP is supported. The
        // mixer currently has no active denoiser/automatic input-level filter.
        if requested == "ON" {
            return error_json(501, "This audio processing filter is not implemented in the active media runtime");
        }
        let mut processing = state.api_v3.abonent_processing.lock().expect("abonent_processing poisoned");
        let saved = processing.entry(connection_id.clone()).or_insert_with(|| json!({}));
        saved[&setting_type] = json!(requested);
        return Json(json!({"status":"ok","mode":requested,"effective":false})).into_response();
    }
    if body.volume.is_some_and(|volume| !(-400..=200).contains(&volume)) {
        return super::call_strings::validation_error("invalid.volume", "volume");
    }
    let is_outgoing = body.outgoing.unwrap_or(true);
    let mut audio_tx_muted = None;
    let mut audio_rx_muted = None;
    let mut video_tx_muted = None;
    let mut video_rx_muted = None;
    let mut vol_tx = None;
    let mut vol_rx = None;

    match setting_type.as_str() {
        "audio" | "microphone" => {
            let muted = !body.enable.unwrap_or(true);
            if is_outgoing {
                audio_tx_muted = Some(muted);
            } else {
                audio_rx_muted = Some(muted);
            }
        }
        "video" | "camera" => {
            let muted = !body.enable.unwrap_or(true);
            if is_outgoing {
                video_tx_muted = Some(muted);
            } else {
                video_rx_muted = Some(muted);
            }
        }
        "volume" => {
            if let Some(vol) = body.volume {
                if is_outgoing {
                    vol_tx = Some(vol);
                } else {
                    vol_rx = Some(vol);
                }
            }
        }
        _ => return error_json(400, "Unsupported media setting"),
    }

    // Сохраняем актуальные настройки абонента для ростера
    if let Ok(mut settings_map) = state.api_v3.abonent_settings.lock() {
        let entry = settings_map.entry(connection_id.clone()).or_default();
        if let Some(m) = audio_tx_muted {
            entry.audio_tx_muted = m;
        }
        if let Some(m) = audio_rx_muted {
            entry.audio_rx_muted = m;
        }
        if let Some(m) = video_tx_muted {
            entry.video_tx_muted = m;
        }
        if let Some(m) = video_rx_muted {
            entry.video_rx_muted = m;
        }
        if let Some(v) = vol_tx {
            entry.volume_tx = v;
        }
        if let Some(v) = vol_rx {
            entry.volume_rx = v;
        }
    }

    let handled_by_dialer = if let Some(dialer) = state.outbound_dialer() {
        // API `outgoing` is relative to the conference: outgoing=true means
        // MCU→terminal, which is the bridge's RX direction. Keep this explicit
        // translation; the two vocabularies are inverted.
        dialer.set_media_control(
            &connection_id,
            audio_rx_muted,
            audio_tx_muted,
            video_rx_muted,
            video_tx_muted,
            vol_rx.map(db_tenths_to_percent),
            vol_tx.map(db_tenths_to_percent),
        )
    } else {
        false
    };

    if !handled_by_dialer {
        state.room_hub.update_participant_media(&room_id, &connection_id, audio_rx_muted, video_rx_muted, None);
    }

    if let Ok(mut attempts) = state.api_v3.call_attempts.lock() {
        for attempt in
            attempts.values_mut().filter(|attempt| attempt.participant_id.as_deref() == Some(connection_id.as_str()))
        {
            let direction = if is_outgoing { "tx" } else { "rx" };
            match setting_type.as_str() {
                "audio" | "microphone" => {
                    attempt.record.payload[format!("audio_{direction}_enabled")] = json!(body.enable.unwrap_or(true))
                }
                "video" | "camera" => {
                    attempt.record.payload[format!("video_{direction}_enabled")] = json!(body.enable.unwrap_or(true))
                }
                "volume" => {
                    if let Some(volume) = body.volume {
                        attempt.record.payload[format!("volume_{direction}")] = json!(volume);
                    }
                }
                _ => {}
            }
        }
    }

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
            "audio_muted": audio_tx_muted,
            "video_muted": video_tx_muted,
            "volume": body.volume,
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
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: insufficient permissions");
    }
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
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: insufficient permissions");
    }
    if let Err(resp) = check_permission(&auth, Some(&session_id)) {
        return resp;
    }
    let Some(room) = find_room(&state, &session_id) else {
        return error_json(404, "Session not found");
    };

    let belongs_to_room =
        state.room_hub.find_participant_room(&abonent_id).map(|(rid, _)| rid == room.id).unwrap_or(false);
    if !belongs_to_room {
        return error_json(404, "Abonent not found in this session");
    }

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

async fn collect_all_abonents_json(state: &SharedState, scope: Option<&str>) -> Vec<Value> {
    let mut room_ids: Vec<RoomId> =
        if let Ok(rooms) = state.rooms.lock() { rooms.keys().copied().collect() } else { Vec::new() };
    if let Some(scope) = scope {
        room_ids.retain(|rid| rid.to_string().eq_ignore_ascii_case(scope));
    }
    let resources = state.api_v3.resources.lock().await;
    let settings_map = state.api_v3.abonent_settings.lock().ok();
    let attempts = state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
    let mut all_abonents = Vec::new();
    for rid in room_ids {
        for p in state.room_hub.list_participants(&rid) {
            let media = settings_map.as_ref().and_then(|m| m.get(&p.participant_id));
            let mut value = participant_to_abonent_json_with_settings(&p, media);
            if let Some(layout_id) = super::resources::participant_layout_id(&resources, &rid, &p.participant_id) {
                value["video_layout_id"] = json!(layout_id);
            }
            if let Some(attempt) = attempts
                .iter()
                .find(|((room, _), attempt)| {
                    *room == rid && attempt.participant_id.as_deref() == Some(p.participant_id.as_str())
                })
                .map(|(_, attempt)| attempt)
            {
                attach_call_contact(&mut value, &attempt.record, media.is_some());
            }
            attach_gateway_statistics(state, &p.participant_id, &mut value);
            value["room_id"] = json!(rid.to_string());
            all_abonents.push(value);
        }
    }
    all_abonents
}

/// Параметры `/engine/abonents` (UI передаёт `session_id` и `display_name`).
#[derive(Deserialize)]
pub struct EngineAbonentsQuery {
    pub session_id: Option<String>,
    pub display_name: Option<String>,
}

/// `GET /api/v3/engine/abonents?session_id=&display_name=` → `{abonents:[...], count:N}`.
pub async fn engine_abonents(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<EngineAbonentsQuery>,
) -> Response {
    track("engine/abonents");
    let mut abonents = collect_all_abonents_json(&state, auth.room_scope.as_deref()).await;
    // Без фильтра карточка одной сессии видела участников другой (T1.2).
    if let Some(session_id) = query.session_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        abonents.retain(|a| a["room_id"].as_str().is_some_and(|r| r.eq_ignore_ascii_case(session_id)));
    }
    if let Some(search) = query.display_name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let needle = search.to_lowercase();
        abonents.retain(|a| a["display_name"].as_str().is_some_and(|name| name.to_lowercase().contains(&needle)));
    }
    let count = abonents.len();
    Json(json!({
        "abonents": abonents,
        "count": count
    }))
    .into_response()
}

/// `GET /api/v3/abonents` → список всех активных абонентов (для PNM UI).
pub async fn list_abonents(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    track("abonents");
    let abonents = collect_all_abonents_json(&state, auth.room_scope.as_deref()).await;
    let count = abonents.len();
    Json(json!({
        "abonents": abonents,
        "count": count,
        "connections": count
    }))
    .into_response()
}

/// `GET /api/v3/abonents/previews` → список превью потоков (PNM UI).
pub async fn abonents_previews() -> Response {
    track("abonents/previews");
    Json(json!({
        "previews": []
    }))
    .into_response()
}

/// `DELETE /api/v3/sessions/{id}/contacts/{contact_id}` — удаление одного контакта из StandBy.
pub async fn remove_session_contact(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((id, contact_id)): Path<(String, String)>,
) -> Response {
    track("sessions/{id}/contacts/{contact_id}");
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required");
    }
    let clean_id = sanitize_id(&id);
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Session not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room.id.to_string())) {
        return resp;
    }

    let removed = {
        let mut map = state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned");
        if let Some(list) = map.get_mut(&room.id) {
            let before = list.len();
            let contact_clean = contact_id.trim_start_matches("sip:");
            list.retain(|c| {
                c.id != contact_id && c.uri != contact_id && c.uri.trim_start_matches("sip:") != contact_clean
            });
            list.len() < before
        } else {
            false
        }
    };

    if removed {
        audit_room(
            &state,
            &auth.subject,
            "contact.removed_from_standby",
            &contact_id,
            client_ip.as_ref().map(|c| c.0 .0.as_str()),
        );
        state.notify_admin(
            &format!("session:{}:abonents", room.id),
            json!({
                "action": "standby_removed",
                "room_id": room.id.to_string(),
                "contact_id": contact_id
            }),
        );
    }

    StatusCode::NO_CONTENT.into_response()
}

/// Тело запроса пакетного удаления контактов из StandBy.
#[derive(Debug, Default, Deserialize)]
pub struct DeleteContactsBatchPayload {
    #[serde(default)]
    pub contacts: Vec<String>,
    #[serde(default)]
    pub data: Option<DeleteContactsBatchData>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DeleteContactsBatchData {
    #[serde(default)]
    pub contacts: Vec<String>,
}

/// `DELETE /api/v3/sessions/{id}/contacts` и `/contacts/list` — пакетное удаление из StandBy.
pub async fn remove_session_contacts_batch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    body: Option<Json<DeleteContactsBatchPayload>>,
) -> Response {
    track("sessions/{id}/contacts:batch");
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required");
    }
    let clean_id = sanitize_id(&id);
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Session not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room.id.to_string())) {
        return resp;
    }

    let to_remove: std::collections::HashSet<String> = if let Some(Json(b)) = body {
        if !b.contacts.is_empty() {
            b.contacts.into_iter().collect()
        } else if let Some(d) = b.data {
            d.contacts.into_iter().collect()
        } else {
            std::collections::HashSet::new()
        }
    } else {
        std::collections::HashSet::new()
    };

    let count_removed = {
        let mut map = state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned");
        if let Some(list) = map.get_mut(&room.id) {
            let before = list.len();
            if to_remove.is_empty() {
                0
            } else {
                list.retain(|c| {
                    let clean = c.uri.trim_start_matches("sip:");
                    !to_remove.contains(&c.id)
                        && !to_remove.contains(&c.uri)
                        && !to_remove.contains(&format!("sip:{}", c.uri))
                        && !to_remove.contains(clean)
                });
                before.saturating_sub(list.len())
            }
        } else {
            0
        }
    };

    if count_removed > 0 {
        audit_room(
            &state,
            &auth.subject,
            "contacts.batch_removed_from_standby",
            &format!("count:{}", count_removed),
            client_ip.as_ref().map(|c| c.0 .0.as_str()),
        );
        state.notify_admin(
            &format!("session:{}:abonents", room.id),
            json!({
                "action": "standby_batch_removed",
                "room_id": room.id.to_string(),
                "removed_count": count_removed
            }),
        );
    }

    StatusCode::NO_CONTENT.into_response()
}

/// Тело запроса отправки DTMF.
#[derive(Debug, Default, Deserialize)]
pub struct DtmfPayload {
    #[serde(default, alias = "dtmf", alias = "key")]
    pub sequence: Option<String>,
}

/// `POST /api/v3/abonents/{id}/dtmf` — передача DTMF последовательности терминалу.
pub async fn send_abonent_dtmf(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<DtmfPayload>,
) -> Response {
    track("abonents/{id}/dtmf");
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required");
    }
    let seq = body.sequence.unwrap_or_default().trim().to_string();
    if seq.is_empty() {
        return error_json(400, "Missing DTMF sequence");
    }
    if seq.len() > 32 {
        return error_json(400, "DTMF sequence exceeds maximum length of 32");
    }

    let Some((room_id, _snapshot)) = state.room_hub.find_participant_room(&id) else {
        return error_json(404, "Abonent not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room_id.to_string())) {
        return resp;
    }

    if let Some(dialer) = state.outbound_dialer() {
        if let Err(e) = dialer.send_dtmf(&id, &seq) {
            // H.323 does not negotiate RFC 4733 telephone-event; report an
            // explicit capability gap instead of an opaque 500.
            let code = if e.contains("did not negotiate RTP telephone-event") { 501 } else { 500 };
            return error_json(code, &format!("Failed to send DTMF: {e}"));
        }
    }

    audit_room(
        &state,
        &auth.subject,
        "abonent.dtmf",
        &format!("{}:{}", id, seq),
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );

    Json(json!({ "status": "ok", "sequence": seq })).into_response()
}

/// Тело запроса FECC (Far End Camera Control).
#[derive(Debug, Default, Deserialize)]
pub struct FeccPayload {
    #[serde(default, alias = "action")]
    pub command: Option<String>,
}

/// `POST /api/v3/abonents/{id}/fecc` — передача команд управления камерой.
pub async fn send_abonent_fecc(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<FeccPayload>,
) -> Response {
    track("abonents/{id}/fecc");
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required");
    }
    let cmd = body.command.unwrap_or_default().trim().to_string();
    if cmd.is_empty() {
        return error_json(400, "Missing FECC command");
    }

    let Some((room_id, _snapshot)) = state.room_hub.find_participant_room(&id) else {
        return error_json(404, "Abonent not found");
    };
    if let Err(resp) = check_permission(&auth, Some(&room_id.to_string())) {
        return resp;
    }

    if let Some(dialer) = state.outbound_dialer() {
        if let Err(e) = dialer.send_fecc(&id, &cmd) {
            return error_json(500, &format!("Failed to send FECC: {e}"));
        }
    }

    audit_room(
        &state,
        &auth.subject,
        "abonent.fecc",
        &format!("{}:{}", id, cmd),
        client_ip.as_ref().map(|c| c.0 .0.as_str()),
    );

    Json(json!({ "status": "ok" })).into_response()
}

#[cfg(test)]
mod auth_scope_tests {
    use super::*;

    fn scoped() -> AuthContext {
        AuthContext {
            subject: "operator".into(),
            role: Role::Operator,
            auth_type: crate::auth_middleware::AuthType::Jwt,
            scopes: vec!["*".into()],
            room_scope: Some("11111111-1111-1111-1111-111111111111".into()),
            generation: 0,
            client_ip: "127.0.0.1".into(),
        }
    }

    #[test]
    fn scoped_payload_hides_foreign_rooms_and_recounts() {
        let payload = json!({
            "sessions": [
                {"id": "11111111-1111-1111-1111-111111111111"},
                {"id": "22222222-2222-2222-2222-222222222222"}
            ],
            "count": 2
        });
        let filtered = filter_sessions_scope(payload, &scoped());
        assert_eq!(filtered["count"], 1);
        assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["sessions"][0]["id"], "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn unscoped_payload_is_untouched() {
        let mut auth = scoped();
        auth.room_scope = None;
        let payload = json!({"sessions": [{"id": "a"}, {"id": "b"}], "count": 2});
        assert_eq!(filter_sessions_scope(payload.clone(), &auth), payload);
    }
}

#[cfg(test)]
mod boot_reconcile_tests {
    use super::*;
    use std::sync::Arc;
    use vks_core::ports::Storage;
    use vks_core::room::RoomMode;

    fn operator_auth(room_scope: Option<&str>) -> AuthContext {
        AuthContext {
            subject: "operator".into(),
            role: Role::Operator,
            auth_type: crate::auth_middleware::AuthType::Jwt,
            scopes: vec!["*".into()],
            room_scope: room_scope.map(str::to_string),
            generation: 0,
            client_ip: "127.0.0.1".into(),
        }
    }

    /// T1.7: при старте все persisted-комнаты считаются остановленными
    /// (активные сессии не переживают рестарт), а `start_session` снова делает
    /// выбранную комнату активной; следующий «рестарт» опять гасит обе.
    #[tokio::test]
    async fn boot_reconcile_stops_sessions_until_started() {
        let storage: Arc<dyn Storage> = Arc::new(vks_storage::MemoryStorage::new());
        let room_a = vks_core::Room::new("boot-a", RoomMode::Sfu);
        let id_a = room_a.id;
        storage.upsert_room(&room_a).await.unwrap();
        let room_b = vks_core::Room::new("boot-b", RoomMode::Sfu);
        let id_b = room_b.id;
        storage.upsert_room(&room_b).await.unwrap();

        let state = Arc::new(AppState::with_storage(storage.clone()));
        state.sync_from_storage().await.unwrap();

        {
            let stopped = state.api_v3.stopped_sessions.lock().unwrap();
            assert!(stopped.contains(&id_a) && stopped.contains(&id_b), "обе комнаты после boot — STOPPED");
        }
        let payload = sessions_payload(&state, &SessionsQuery::default());
        assert_eq!(payload["count"], 2);
        for session in payload["sessions"].as_array().unwrap() {
            assert_eq!(session["state"], "STOPPED", "persisted-сессии после рестарта остановлены: {session}");
            assert_eq!(session["has_stopped"], true);
            assert!(session["session"].is_null(), "у остановленной сессии нет живого session");
        }

        let response = start_session(
            State(state.clone()),
            Extension(operator_auth(None)),
            None,
            Json(StartSessionRequest { conference_id: Some(id_a.to_string()) }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let after = sessions_payload(&state, &SessionsQuery::default());
        assert_eq!(after["count"], 2, "после старта обе комнаты остаются в выдаче");
        let sessions = after["sessions"].as_array().unwrap();
        let active: Vec<_> = sessions.iter().filter(|s| s["state"] == "ACTIVE").collect();
        assert_eq!(active.len(), 1, "активна ровно одна сессия: {after}");
        assert_eq!(active[0]["id"], id_a.to_string());
        assert_eq!(active[0]["session"]["id"], id_a.to_string(), "у активной сессии session именно room_a");
        assert_eq!(active[0]["has_stopped"], false, "активная сессия не в stopped_sessions");
        let stopped_b = sessions.iter().find(|s| s["id"] == id_b.to_string()).expect("room_b в выдаче");
        assert_eq!(stopped_b["state"], "STOPPED");
        assert_eq!(stopped_b["has_stopped"], true);

        // Имитация второго рестарта: тот же storage, новое состояние.
        let restarted = Arc::new(AppState::with_storage(storage));
        restarted.sync_from_storage().await.unwrap();
        let payload = sessions_payload(&restarted, &SessionsQuery::default());
        assert_eq!(payload["count"], 2);
        {
            let stopped = restarted.api_v3.stopped_sessions.lock().unwrap();
            assert!(stopped.contains(&id_a) && stopped.contains(&id_b), "после рестарта обе комнаты снова STOPPED");
        }
        for session in payload["sessions"].as_array().unwrap() {
            assert_eq!(session["state"], "STOPPED", "после рестарта активная сессия снова STOPPED: {session}");
            assert!(session["session"].is_null(), "после рестарта живого session нет: {session}");
        }
    }
}
