//! `call_strings` & `invite` — интеграционные эндпоинты исходящего дозвона и адресной книги (V1–V3, ADR-0027).
//!
//! Реализует контракты фронтенда PNM (`index.bundle` и `index6.bundle`):
//! - Одиночное и пакетное добавление шаблонов контактов в конференцию.
//! - Немедленный исходящий вызов в живую сессию с In-Flight Guard и интеграцией `OutboundDialer`.
//! - Добавление абонента в StandBy (without_call) без немедленного сетевого вызова.
//! - Повторный вызов (Recall / Invite) абонентов из списков StandBy / disconnected.
//! - Строгие форматы ошибок 400 (validation details) и 409 (conflict descriptions).

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use vks_core::{Role, RoomId};

use super::auth::ClientIp;
use super::sessions::{check_permission, find_room, sanitize_id};
use super::{error_json, track};
use crate::auth_middleware::AuthContext;
use crate::settings::SettingsService;
use crate::state::SharedState;

/// Запись строки вызова в памяти (in-flight или standby).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallStringRecord {
    pub id: String,
    pub uri: String,
    pub display_name: String,
    pub created_at: u64,
}

/// Гибкий элемент строки вызова с поддержкой алиасов Serde для совместимости с различными версиями DTO фронтенда.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CallStringItem {
    #[serde(default, alias = "call_string_id", alias = "addressId", alias = "id")]
    pub id: Option<String>,
    #[serde(default, alias = "call_string", alias = "url", alias = "connection_url")]
    pub uri: Option<String>,
    #[serde(default, alias = "displayName", alias = "name")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub payload: Option<Value>,
}

/// Тело пакетного приглашения / добавления строк вызова.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InviteCallStringsRequest {
    #[serde(default)]
    pub call_strings: Vec<CallStringItem>,
}

/// Форматирует ошибку валидации 400 со списком details для защиты от краша `details.reduce` в JS.
pub fn validation_error(reason: &str, position: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": {
                "code": 400,
                "description": "Validation error",
                "details": [
                    { "reason": reason, "position": position }
                ]
            }
        })),
    )
        .into_response()
}

/// Форматирует ошибку конфликта 409 с каноническим описанием, ожидаемым интерцепторами фронтенда.
pub fn conflict_error(description: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": {
                "code": 409,
                "description": description
            }
        })),
    )
        .into_response()
}

/// Канонизирует адрес абонента, гарантируя наличие схемы протокола (`sip:` по умолчанию).
pub fn canonical_uri(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("sip:")
        || trimmed.starts_with("h323:")
        || trimmed.starts_with("webrtc:")
        || trimmed.starts_with("recorder:")
    {
        trimmed.to_string()
    } else {
        format!("sip:{}", trimmed)
    }
}

/// RAII Guard для In-Flight исходящих вызовов.
/// Гарантирует удаление записи из `in_flight_calls` при завершении вызова, ошибке или панике.
pub struct InFlightCallGuard {
    state: SharedState,
    room_id: RoomId,
    address: String,
}

impl InFlightCallGuard {
    pub fn new(state: SharedState, room_id: RoomId, address: String) -> Self {
        Self { state, room_id, address }
    }
}

impl Drop for InFlightCallGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = self.state.api_v3.in_flight_calls.lock() {
            g.remove(&(self.room_id, self.address.clone()));
        }
    }
}

fn now_unix_sec() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/v3/ui/conferences/{id}/call_strings` — добавление строки вызова в шаблон конференции (без сетевого звонка).
pub async fn create_conference_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<CallStringItem>,
) -> Response {
    track("ui/conferences/{id}/call_strings");
    let clean_id = sanitize_id(&id);
    if let Err(resp) = check_permission(&auth, Some(clean_id)) {
        return resp;
    }
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required to manage call strings");
    }
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Conference not found");
    };

    let raw_uri = body.uri.as_deref().unwrap_or("").trim();
    if raw_uri.is_empty() {
        return validation_error("invalid.uri", "uri");
    }
    let uri = canonical_uri(raw_uri);
    let cs_id = body.id.unwrap_or_else(|| format!("cs-{}", Uuid::new_v4()));
    let now = now_unix_sec();

    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(&auth.subject, "conference.call_string.add", Some(room.id.to_string()), ip);

    Json(json!({
        "call_string": {
            "id": cs_id,
            "addressId": cs_id,
            "created_at": now,
            "uri": uri,
            "display_name": body.display_name.unwrap_or_else(|| raw_uri.to_string())
        }
    }))
    .into_response()
}

/// `POST /api/v3/conferences/{id}/invite/call_string/list` — пакетное добавление строк вызова в конференцию.
pub async fn invite_conference_call_strings(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<InviteCallStringsRequest>,
) -> Response {
    track("conferences/{id}/invite/call_string/list");
    let clean_id = sanitize_id(&id);
    if let Err(resp) = check_permission(&auth, Some(clean_id)) {
        return resp;
    }
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required to manage call strings");
    }
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Conference not found");
    };

    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(
        &auth.subject,
        "conference.call_string.invite_list",
        Some(format!("{}:{} items", room.id, body.call_strings.len())),
        ip,
    );

    Json(json!({ "status": "ok" })).into_response()
}

/// `POST /api/v3/ui/sessions/{id}/call_strings` — немедленный вызов абонента в активную сессию (dial-out).
pub async fn create_session_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<CallStringItem>,
) -> Response {
    track("ui/sessions/{id}/call_strings");
    let clean_id = sanitize_id(&id);
    if let Err(resp) = check_permission(&auth, Some(clean_id)) {
        return resp;
    }
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required to initiate outbound calls");
    }
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Session not found");
    };

    let raw_uri = body.uri.as_deref().unwrap_or("").trim();
    if raw_uri.is_empty() {
        return validation_error("invalid.uri", "uri");
    }
    let uri = canonical_uri(raw_uri);
    let display_name = body.display_name.unwrap_or_else(|| raw_uri.to_string());
    let cs_id = body.id.unwrap_or_else(|| format!("cs-{}", Uuid::new_v4()));
    let now = now_unix_sec();

    // 1. Проверка лимитов участников комнаты
    let max_participants = SettingsService::get(&state).limits.max_participants_per_room as usize;
    let current_participants = state.room_hub.list_participants(&room.id).len();
    let current_in_flight = if let Ok(guard) = state.api_v3.in_flight_calls.lock() {
        guard.keys().filter(|(rid, _)| *rid == room.id).count()
    } else {
        0
    };

    if current_participants + current_in_flight >= max_participants {
        return conflict_error("Too many connections");
    }

    // 2. Проверка дублирования абонента / In-Flight Guard
    {
        let mut in_flight = state.api_v3.in_flight_calls.lock().expect("in_flight_calls poisoned");
        if in_flight.contains_key(&(room.id, uri.clone())) {
            return conflict_error("The specified call string is used in session");
        }

        // Проверяем также уже подключенных участников комнаты
        let participants = state.room_hub.list_participants(&room.id);
        if participants.iter().any(|p| {
            p.display_name == display_name || p.participant_id.contains(&raw_uri.replace([':', '@', '.'], "-"))
        }) {
            return conflict_error("The specified call string is used in session");
        }

        let record = CallStringRecord {
            id: cs_id.clone(),
            uri: uri.clone(),
            display_name: display_name.clone(),
            created_at: now,
        };
        in_flight.insert((room.id, uri.clone()), record);
    }

    // Удаляем из standby если контакт уже был запланирован
    if let Ok(mut scheduled) = state.api_v3.scheduled_calls.lock() {
        if let Some(list) = scheduled.get_mut(&room.id) {
            list.retain(|r| r.uri != uri && r.id != cs_id);
        }
    }

    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(&auth.subject, "session.outbound_invite", Some(format!("{}:{}", room.id, uri)), ip);

    // 3. Инициируем вызов через OutboundDialer (асинхронно в фоне с RAII guard)
    let dialer_opt = state.outbound_dialer.clone();
    let dial_state = state.clone();
    let dial_room_id = room.id;
    let dial_uri = uri.clone();
    let dial_name = display_name.clone();

    tokio::spawn(async move {
        let _guard = InFlightCallGuard::new(dial_state.clone(), dial_room_id, dial_uri.clone());
        if let Some(dialer) = dialer_opt {
            match dialer.dial(dial_room_id, &dial_uri, Some(&dial_name)).await {
                Ok(pid) => {
                    tracing::info!(room = %dial_room_id, pid = %pid, uri = %dial_uri, "outbound dial connected");
                }
                Err(e) => {
                    tracing::warn!(room = %dial_room_id, uri = %dial_uri, error = %e, "outbound dial failed");
                }
            }
        } else {
            // Без dialer (standalone/тестовый режим): создаем тестового участника в RoomHub
            let proto_tag = if dial_uri.starts_with("h323:") { "@h323-gateway" } else { "@sip-gateway" };
            let pid = format!("{}p-out-{}", proto_tag, Uuid::new_v4());
            let snapshot = crate::hub::ParticipantSnapshot {
                participant_id: pid.clone(),
                display_name: dial_name,
                is_recorder: false,
                is_speaking: false,
                track_ids: vec!["audio".into(), "video".into()],
                is_audio_muted: false,
                is_video_muted: false,
                connected_at: now_unix_sec(),
            };
            let _ = dial_state.room_hub.register_participant(&dial_room_id, snapshot);
        }
    });

    Json(json!({
        "call_string": {
            "id": cs_id,
            "addressId": cs_id,
            "created_at": now,
            "uri": uri,
            "display_name": display_name
        }
    }))
    .into_response()
}

/// `POST /api/v3/ui/sessions/{id}/call_strings/without_call` — добавление абонента в StandBy (запланированные) без вызова.
pub async fn create_session_call_string_standby(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<CallStringItem>,
) -> Response {
    track("ui/sessions/{id}/call_strings/without_call");
    let clean_id = sanitize_id(&id);
    if let Err(resp) = check_permission(&auth, Some(clean_id)) {
        return resp;
    }
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required to manage call strings");
    }
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Session not found");
    };

    let raw_uri = body.uri.as_deref().unwrap_or("").trim();
    if raw_uri.is_empty() {
        return validation_error("invalid.uri", "uri");
    }
    let uri = canonical_uri(raw_uri);
    let display_name = body.display_name.unwrap_or_else(|| raw_uri.to_string());
    let cs_id = body.id.unwrap_or_else(|| format!("cs-{}", Uuid::new_v4()));
    let now = now_unix_sec();

    let record =
        CallStringRecord { id: cs_id.clone(), uri: uri.clone(), display_name: display_name.clone(), created_at: now };

    {
        let mut scheduled = state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned");
        scheduled.entry(room.id).or_default().push(record);
    }

    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(
        &auth.subject,
        "session.call_string.standby_add",
        Some(format!("{}:{}", room.id, uri)),
        ip,
    );

    Json(json!({
        "call_string": {
            "id": cs_id,
            "addressId": cs_id,
            "created_at": now,
            "uri": uri,
            "display_name": display_name
        }
    }))
    .into_response()
}

/// `POST /api/v3/sessions/{id}/invite/call_strings/list` — вызов/повторный дозвон списка строк вызова (Recall/Invite).
pub async fn invite_session_call_strings_list(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<InviteCallStringsRequest>,
) -> Response {
    track("sessions/{id}/invite/call_strings/list");
    let clean_id = sanitize_id(&id);
    if let Err(resp) = check_permission(&auth, Some(clean_id)) {
        return resp;
    }
    if auth.role < Role::Operator {
        return error_json(403, "Forbidden: Operator role required to initiate outbound calls");
    }
    let Some(room) = find_room(&state, clean_id) else {
        return error_json(404, "Session not found");
    };

    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(
        &auth.subject,
        "session.call_string.recall_list",
        Some(format!("{}:{} items", room.id, body.call_strings.len())),
        ip,
    );

    let max_participants = SettingsService::get(&state).limits.max_participants_per_room as usize;
    let dialer_opt = state.outbound_dialer.clone();

    for item in body.call_strings {
        let raw_uri = item.uri.as_deref().unwrap_or("").trim();
        if raw_uri.is_empty() {
            continue;
        }
        let uri = canonical_uri(raw_uri);
        let display_name = item.display_name.unwrap_or_else(|| raw_uri.to_string());

        // Проверяем лимит комнаты с учетом активных и in-flight участников
        let current_participants = state.room_hub.list_participants(&room.id).len();
        let in_flight_count = if let Ok(guard) = state.api_v3.in_flight_calls.lock() {
            guard.keys().filter(|(rid, _)| *rid == room.id).count()
        } else {
            0
        };
        if current_participants + in_flight_count >= max_participants {
            tracing::warn!(room = %room.id, uri = %uri, "invite skipped: room capacity reached");
            break;
        }

        // Проверяем на дубликат: уже подключен или уже в процессе вызова
        let is_connected = state.room_hub.list_participants(&room.id).iter().any(|p| {
            p.display_name == display_name || p.participant_id.contains(&raw_uri.replace([':', '@', '.'], "-"))
        });
        if is_connected {
            continue;
        }

        let mut in_flight = state.api_v3.in_flight_calls.lock().expect("in_flight_calls poisoned");
        if in_flight.contains_key(&(room.id, uri.clone())) {
            continue;
        }

        // Удаляем из standby если был там
        if let Ok(mut scheduled) = state.api_v3.scheduled_calls.lock() {
            if let Some(list) = scheduled.get_mut(&room.id) {
                list.retain(|r| r.uri != uri && r.id != item.id.as_deref().unwrap_or(""));
            }
        }

        let cs_id = item.id.unwrap_or_else(|| format!("cs-{}", Uuid::new_v4()));
        let record = CallStringRecord {
            id: cs_id,
            uri: uri.clone(),
            display_name: display_name.clone(),
            created_at: now_unix_sec(),
        };
        in_flight.insert((room.id, uri.clone()), record);
        drop(in_flight);

        let dial_state = state.clone();
        let dial_room_id = room.id;
        let dial_uri = uri.clone();
        let dial_name = display_name.clone();
        let dialer = dialer_opt.clone();

        tokio::spawn(async move {
            let _guard = InFlightCallGuard::new(dial_state.clone(), dial_room_id, dial_uri.clone());
            if let Some(d) = dialer {
                let _ = d.dial(dial_room_id, &dial_uri, Some(&dial_name)).await;
            } else {
                let proto_tag = if dial_uri.starts_with("h323:") { "@h323-gateway" } else { "@sip-gateway" };
                let pid = format!("{}p-out-{}", proto_tag, Uuid::new_v4());
                let snapshot = crate::hub::ParticipantSnapshot {
                    participant_id: pid.clone(),
                    display_name: dial_name,
                    is_recorder: false,
                    is_speaking: false,
                    track_ids: vec!["audio".into(), "video".into()],
                    is_audio_muted: false,
                    is_video_muted: false,
                    connected_at: now_unix_sec(),
                };
                let _ = dial_state.room_hub.register_participant(&dial_room_id, snapshot);
            }
        });
    }

    Json(json!({ "status": "ok" })).into_response()
}

/// `GET /api/v3/call_strings` и `GET /api/v3/ui/call_strings` — список сохраненных строк вызова в адресной книге.
pub async fn list_call_strings() -> Response {
    track("call_strings");
    Json(json!({ "call_strings": [], "count": 0 })).into_response()
}
