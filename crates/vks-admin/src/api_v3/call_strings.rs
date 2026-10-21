//! `call_strings` & `invite` — интеграционные эндпоинты исходящего дозвона и адресной книги (V1–V3, ADR-0027).
//!
//! Реализует контракты фронтенда PNM (`index.bundle` и `index6.bundle`):
//! - Одиночное и пакетное добавление шаблонов контактов в конференцию.
//! - Немедленный исходящий вызов в живую сессию с In-Flight Guard и интеграцией `OutboundDialer`.
//! - Добавление абонента в StandBy (without_call) без немедленного сетевого вызова.
//! - Повторный вызов (Recall / Invite) абонентов из списков StandBy / disconnected.
//! - Строгие форматы ошибок 400 (validation details) и 409 (conflict descriptions).

use std::collections::HashMap;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use vks_core::{Role, RoomId};

use super::auth::ClientIp;
use super::resources::{AddressRecord, ResourceState};
use super::sessions::{check_permission, find_room, sanitize_id};
use super::{error_json, track, ListParams};
use crate::auth_middleware::AuthContext;
use crate::settings::SettingsService;
use crate::state::{AppState, SharedState};

/// Запись строки вызова в памяти (in-flight или standby).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallStringRecord {
    pub id: String,
    pub uri: String,
    pub display_name: String,
    pub created_at: u64,
    #[serde(default)]
    pub payload: Value,
}

/// Native PNM payload: gains are signed tenths of a dB and directions are
/// viewed from the MCU (RX terminal -> MCU; TX MCU -> terminal).
pub fn default_contact_payload(payload: Option<&Value>) -> Value {
    let protocol = json!({"bitrate":null,"audioCodecs":[],"videoCodecs":[],"resolution":null,
        "isSelfViewEnabled":true,"cascadeMode":"none"});
    let mut defaults = json!({
        "slot_render_type":"DEFAULT","auto_call":true,
        "audio_rx_enabled":true,"audio_tx_enabled":true,
        "video_rx_enabled":true,"video_tx_enabled":true,
        "volume_rx":0,"volume_tx":0,"noise_suppression":"DEFAULT","auto_gain_control":"DEFAULT",
        "crown":"EQUAL","hide_display_name":false,
        "call_string":{"sip":protocol.clone(),"h323":protocol,
            "rtsp":{"primary":true},"vnc":{"primary":true},
            "file":{"primary":true,"zoom":"fit"},"media":{"primary":true,"loop":true}},
        "stop_details":{}
    });
    if let Some(values) = payload.and_then(Value::as_object) {
        for (key, value) in values {
            if key == "call_string" {
                if let Some(protocols) = value.as_object() {
                    for (protocol, options) in protocols {
                        if let Some(options) = options.as_object() {
                            if !defaults["call_string"][protocol].is_object() {
                                defaults["call_string"][protocol] = json!({});
                            }
                            for (option, value) in options {
                                defaults["call_string"][protocol][option] = value.clone();
                            }
                        }
                    }
                }
            } else {
                defaults[key] = value.clone();
            }
        }
    }
    for field in ["audio_rx_enabled", "audio_tx_enabled", "video_rx_enabled", "video_tx_enabled", "auto_call"] {
        if !defaults[field].is_boolean() {
            defaults[field] = json!(true);
        }
    }
    if !defaults["hide_display_name"].is_boolean() {
        defaults["hide_display_name"] = json!(false);
    }
    for field in ["volume_rx", "volume_tx"] {
        defaults[field] = json!(defaults[field].as_i64().unwrap_or(0).clamp(-400, 200));
    }
    let crown = match defaults["crown"].as_str() {
        Some("EQUAL" | "SELECTED" | "EXCLUSIVE") => defaults["crown"].clone(),
        _ => json!(if defaults["crown"].as_bool().unwrap_or(false) { "SELECTED" } else { "EQUAL" }),
    };
    defaults["crown"] = crown;
    let render = defaults["slot_render_type"].as_str().unwrap_or("DEFAULT").to_ascii_uppercase();
    defaults["slot_render_type"] = json!(if matches!(render.as_str(), "DEFAULT" | "AUTO" | "FILL" | "FULL") {
        render.as_str()
    } else {
        "DEFAULT"
    });
    for protocol in ["sip", "h323"] {
        for (snake, camel) in [
            ("audio_codecs", "audioCodecs"),
            ("video_codecs", "videoCodecs"),
            ("self_view", "isSelfViewEnabled"),
            ("cascade", "cascadeMode"),
        ] {
            if let Some(value) = payload
                .and_then(|payload| payload.get("call_string"))
                .and_then(|options| options.get(protocol))
                .and_then(|options| options.get(snake))
            {
                defaults["call_string"][protocol][camel] = value.clone();
            }
        }
    }
    defaults
}

fn merged_contact_payload(previous: Option<&Value>, patch: Option<&Value>) -> Value {
    let mut payload = default_contact_payload(previous);
    if let Some(patch) = patch.filter(|patch| patch.is_object()) {
        super::resources::merge_json(&mut payload, patch);
    }
    payload
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
    if let Some((scheme, address)) = trimmed.split_once(':') {
        let mut characters = scheme.chars();
        if characters.next().is_some_and(|character| character.is_ascii_alphabetic())
            && characters.all(|character| character.is_ascii_alphanumeric() || "+.-".contains(character))
        {
            let scheme = scheme.to_ascii_lowercase();
            return format!("{}:{address}", if scheme == "auto" { "sip" } else { scheme.as_str() });
        }
    }
    format!("sip:{trimmed}")
}

/// One call intent can have several attempts; stale completions cannot replace a retry.
#[derive(Debug, Clone)]
pub struct CallAttempt {
    pub record: CallStringRecord,
    pub generation: Uuid,
    pub participant_id: Option<String>,
    pub failure_reason: Option<String>,
    pub completed_at: Option<u64>,
    pub abort_handle: Option<tokio::task::AbortHandle>,
    /// Когда соединение реально установилось (для истории: abonent.start).
    pub connected_at: Option<u64>,
    /// Когда участник ушёл из комнаты до остановки сессии (BYE/kick/leave).
    pub stopped_at: Option<u64>,
    /// Причина завершения, сохранённая при уходе участника.
    pub stop_reason: Option<String>,
    /// Входящий вызов (шлюз → MCU), а не исходящий из адресной книги.
    pub incoming: bool,
}

struct InFlightCallGuard {
    state: SharedState,
    room_id: RoomId,
    address: String,
    generation: Uuid,
}

impl Drop for InFlightCallGuard {
    fn drop(&mut self) {
        let key = (self.room_id, self.address.clone());
        let mut attempts = self.state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
        if let Some(attempt) = attempts.get_mut(&key) {
            if attempt.generation != self.generation {
                return;
            }
            if attempt.participant_id.is_none() && attempt.failure_reason.is_none() {
                attempt.failure_reason = Some("Call cancelled before connection".into());
            }
            if attempt.failure_reason.is_some() && attempt.completed_at.is_none() {
                attempt.completed_at = Some(now_unix_sec());
            }
            attempt.abort_handle = None;
            self.state.api_v3.in_flight_calls.lock().expect("in_flight_calls poisoned").remove(&key);
        }
    }
}

/// Stop only pending futures in this room; completed calls are hung up by the
/// session lifecycle. Abort handles are invoked after releasing all state locks.
pub(super) fn cancel_pending_calls(state: &SharedState, room_id: &RoomId) {
    let mut handles = Vec::new();
    {
        let mut attempts = state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
        let mut in_flight = state.api_v3.in_flight_calls.lock().expect("in_flight_calls poisoned");
        let keys: Vec<_> = in_flight.keys().filter(|(rid, _)| rid == room_id).cloned().collect();
        for key in keys {
            if let Some(attempt) = attempts.get_mut(&key) {
                attempt.failure_reason = Some("Session ended before connection".into());
                attempt.completed_at.get_or_insert_with(now_unix_sec);
                if let Some(handle) = attempt.abort_handle.take() {
                    handles.push(handle);
                }
            }
            in_flight.remove(&key);
        }
    }
    for handle in handles {
        handle.abort();
    }
}

fn gain_percent(value: &Value) -> u32 {
    (100.0 * 10.0_f64.powf(value.as_i64().unwrap_or(0).clamp(-400, 200) as f64 / 200.0)).round() as u32
}

fn apply_call_controls(state: &SharedState, dialer: &dyn crate::state::OutboundDialer, pid: &str, payload: &Value) {
    let payload = default_contact_payload(Some(payload));
    let rx_muted = !payload["audio_rx_enabled"].as_bool().unwrap_or(true);
    let tx_muted = !payload["audio_tx_enabled"].as_bool().unwrap_or(true);
    let video_rx_muted = !payload["video_rx_enabled"].as_bool().unwrap_or(true);
    let video_tx_muted = !payload["video_tx_enabled"].as_bool().unwrap_or(true);
    state.api_v3.abonent_settings.lock().expect("abonent_settings poisoned").insert(
        pid.to_string(),
        super::AbonentMediaState {
            audio_rx_muted: rx_muted,
            audio_tx_muted: tx_muted,
            video_rx_muted,
            video_tx_muted,
            volume_rx: payload["volume_rx"].as_i64().unwrap_or(0) as i32,
            volume_tx: payload["volume_tx"].as_i64().unwrap_or(0) as i32,
        },
    );
    // Bridge names use the terminal perspective, so NativeAPI directions swap.
    if !dialer.set_media_control(
        pid,
        Some(rx_muted),
        Some(tx_muted),
        Some(video_rx_muted),
        Some(video_tx_muted),
        Some(gain_percent(&payload["volume_rx"])),
        Some(gain_percent(&payload["volume_tx"])),
    ) {
        tracing::warn!(participant_id = pid, "Outbound gateway did not apply initial media controls");
    }
}

/// Admit a real dial atomically; an unavailable gateway must never create a fake participant.
pub(super) fn queue_call(
    state: &SharedState,
    room_id: RoomId,
    mut record: CallStringRecord,
) -> Result<(), Box<Response>> {
    let Some(dialer) = state.outbound_dialer() else {
        return Err(Box::new(error_json(503, "Outbound gateway is not configured")));
    };
    let stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned");
    if stopped.contains(&room_id) {
        return Err(Box::new(conflict_error("Session is not running")));
    }
    let mut attempts = state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
    let mut in_flight = state.api_v3.in_flight_calls.lock().expect("in_flight_calls poisoned");
    let key = (room_id, record.uri.clone());
    let participants = state.room_hub.list_participants(&room_id);
    let connected = attempts
        .get(&key)
        .and_then(|a| a.participant_id.as_deref())
        .is_some_and(|id| participants.iter().any(|p| p.participant_id == id));
    if in_flight.contains_key(&key) || connected {
        return Err(Box::new(conflict_error("The specified call string is used in session")));
    }
    // Лимит комнаты (payload конференции) приоритетнее системного умолчания.
    let limit = state
        .api_v3
        .conference_settings
        .lock()
        .ok()
        .and_then(|settings| {
            settings.get(&room_id).and_then(|values| {
                values
                    .get("connections_limit")
                    .or_else(|| values.get("payload").and_then(|payload| payload.get("connections_limit")))
                    .and_then(Value::as_u64)
            })
        })
        .unwrap_or_else(|| SettingsService::get(state).limits.max_participants_per_room as u64)
        as usize;
    let current = participants.len() + in_flight.keys().filter(|(rid, _)| *rid == room_id).count();
    if current >= limit {
        return Err(Box::new(conflict_error("Too many connections")));
    }
    record.payload = default_contact_payload(Some(&record.payload));
    let generation = Uuid::new_v4();
    attempts.insert(
        key.clone(),
        CallAttempt {
            record: record.clone(),
            generation,
            participant_id: None,
            failure_reason: None,
            completed_at: None,
            abort_handle: None,
            connected_at: None,
            stopped_at: None,
            stop_reason: None,
            incoming: false,
        },
    );
    in_flight.insert(key, record.clone());
    drop(in_flight);
    drop(attempts);
    drop(stopped);
    if let Some(list) = state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned").get_mut(&room_id) {
        list.retain(|r| r.uri != record.uri && r.id != record.id);
    }
    let dial_state = state.clone();
    let attempt_key = (room_id, record.uri.clone());
    let task = tokio::spawn(async move {
        let _guard = InFlightCallGuard { state: dial_state.clone(), room_id, address: record.uri.clone(), generation };
        let result = dialer.dial(room_id, &record.uri, Some(&record.display_name)).await;
        let stopped = dial_state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned").contains(&room_id);
        let mut obsolete = stopped;
        {
            let mut attempts = dial_state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
            if let Some(attempt) =
                attempts.get_mut(&(room_id, record.uri.clone())).filter(|a| a.generation == generation)
            {
                if attempt.failure_reason.is_some() {
                    obsolete = true;
                }
                if !obsolete {
                    match &result {
                        Ok(pid) => {
                            attempt.participant_id = Some(pid.clone());
                            attempt.connected_at = Some(now_unix_sec());
                        }
                        Err(error) => {
                            attempt.failure_reason = Some(error.clone());
                            attempt.completed_at = Some(now_unix_sec());
                        }
                    }
                } else {
                    attempt.failure_reason.get_or_insert_with(|| "Session ended before connection".into());
                    attempt.completed_at.get_or_insert_with(now_unix_sec);
                }
            } else {
                obsolete = true;
            }
        }
        match result {
            Ok(pid) if obsolete => {
                dialer.hangup(&pid);
                dial_state.room_hub.unregister_participant(&room_id, &pid);
            }
            Ok(pid) => {
                apply_call_controls(&dial_state, dialer.as_ref(), &pid, &record.payload);
                tracing::info!(room=%room_id,pid=%pid,uri=%record.uri,"outbound dial connected");
            }
            Err(error) => tracing::warn!(room=%room_id,uri=%record.uri,%error,"outbound dial failed"),
        }
        dial_state.notify_admin(
            &format!("session:{room_id}:abonents"),
            json!({"action":"call_completed","contact_id":record.id}),
        );
    });
    // A stop can happen between admission and spawning. Register under the same
    // lifecycle lock order and abort immediately if cancellation already won.
    let abort_handle = task.abort_handle();
    let stopped = state.api_v3.stopped_sessions.lock().expect("stopped_sessions poisoned");
    let should_abort = {
        let mut attempts = state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
        match attempts.get_mut(&attempt_key).filter(|attempt| attempt.generation == generation) {
            Some(attempt) if !stopped.contains(&room_id) && attempt.failure_reason.is_none() => {
                attempt.abort_handle = Some(abort_handle.clone());
                false
            }
            _ => true,
        }
    };
    drop(stopped);
    if should_abort {
        abort_handle.abort();
    }
    Ok(())
}

/// Уход участника из комнаты (BYE, kick, leave) до остановки сессии.
///
/// Сохраняем `stopped_at`/`stop_reason` в [`CallAttempt`], иначе история
/// теряет abonent и vendor-UI рисует состоявшийся вызов как MISSED.
pub fn note_participant_left(state: &AppState, room_id: &vks_core::RoomId, participant_id: &str, reason: &str) {
    let now = now_unix_sec();
    if let Ok(mut attempts) = state.api_v3.call_attempts.lock() {
        for attempt in attempts
            .iter_mut()
            .filter(|((attempt_room, _), _)| attempt_room == room_id)
            .map(|(_, attempt)| attempt)
            .filter(|attempt| attempt.participant_id.as_deref() == Some(participant_id))
        {
            attempt.stopped_at.get_or_insert(now);
            attempt.stop_reason.get_or_insert_with(|| reason.to_string());
            attempt.completed_at.get_or_insert(now);
        }
    }
}

/// Регистрирует входящий шлюзовой вызов в `call_attempts` (`INCOMING` в истории).
pub fn record_incoming_call(
    state: &AppState,
    room_id: vks_core::RoomId,
    participant_id: &str,
    uri: &str,
    display_name: &str,
) {
    let now = now_unix_sec();
    let record = CallStringRecord {
        id: format!("cs-{}", Uuid::new_v4()),
        uri: uri.to_string(),
        display_name: display_name.to_string(),
        created_at: now,
        payload: default_contact_payload(None),
    };
    let mut attempts = state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
    attempts.insert(
        (room_id, format!("{}#{}", uri, record.id)),
        CallAttempt {
            record,
            generation: Uuid::new_v4(),
            participant_id: Some(participant_id.to_string()),
            failure_reason: None,
            completed_at: None,
            abort_handle: None,
            connected_at: Some(now),
            stopped_at: None,
            stop_reason: None,
            incoming: true,
        },
    );
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

    let display_name = body.display_name.clone().unwrap_or_else(|| raw_uri.to_string());
    let payload = default_contact_payload(body.payload.as_ref());
    let record = json!({"id":cs_id,"contact_id":cs_id,"call_string_id":cs_id,
        "contact_type":"call_strings","display_name":display_name,"created_at":now,
        "uri":uri,"connection_url":uri,"payload":payload,
        "call_string":{"id":cs_id,"uri":uri,"display_name":display_name}});
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    let contacts = resources.contacts.entry(room.id.to_string()).or_default();
    if contacts.iter().any(|contact| contact["uri"].as_str() == Some(uri.as_str())) {
        return conflict_error("The specified call string is used in conference");
    }
    contacts.push(record);
    if let Err(error) = super::resources::persist(&state, &resources).await {
        *resources = previous;
        return error_json(500, &format!("Storage error: {error}"));
    }
    drop(resources);

    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(&auth.subject, "conference.call_string.add", Some(room.id.to_string()), ip);

    (
        StatusCode::CREATED,
        Json(json!({
            "call_string": {
                "id": cs_id,
                "addressId": cs_id,
                "created_at": now,
                "uri": uri,
                "display_name": display_name,
                "payload":payload
            }
        })),
    )
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

    let count = body.call_strings.len();
    for mut item in body.call_strings {
        if item.uri.as_deref().is_none_or(|uri| uri.trim().is_empty()) {
            let Some(address_id) = item.id.as_deref() else {
                return validation_error("invalid.uri", "uri");
            };
            if check_permission(&auth, None).is_err()
                && state.api_v3.resources.lock().await.addresses.contains_key(address_id)
            {
                return error_json(403, "Forbidden: global address book requires broader scope");
            }
            let Some(saved) = address_book_records(&state, &auth)
                .await
                .into_iter()
                .find(|record| record["id"].as_str() == Some(address_id))
            else {
                return error_json(404, "Call string not found");
            };
            item.uri = saved["uri"].as_str().map(str::to_string);
            if item.display_name.is_none() {
                item.display_name = saved["display_name"].as_str().map(str::to_string);
            }
        }
        let response = create_conference_call_string(
            State(state.clone()),
            Extension(auth.clone()),
            client_ip.clone(),
            Path(id.clone()),
            Json(item),
        )
        .await;
        if !response.status().is_success() {
            return response;
        }
    }
    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(
        &auth.subject,
        "conference.call_string.invite_list",
        Some(format!("{}:{count} items", room.id)),
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

    let payload = default_contact_payload(body.payload.as_ref());
    let record = CallStringRecord {
        id: cs_id.clone(),
        uri: uri.clone(),
        display_name: display_name.clone(),
        created_at: now,
        payload: payload.clone(),
    };
    if let Err(response) = queue_call(&state, room.id, record) {
        return *response;
    }
    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(&auth.subject, "session.outbound_invite", Some(format!("{}:{}", room.id, uri)), ip);

    (
        StatusCode::CREATED,
        Json(json!({
            "call_string": {
                "id": cs_id,
                "addressId": cs_id,
                "created_at": now,
                "uri": uri,
                "display_name": display_name,
                "payload":payload
            }
        })),
    )
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

    let mut payload = default_contact_payload(body.payload.as_ref());
    payload["auto_call"] = json!(false);
    let record = CallStringRecord {
        id: cs_id.clone(),
        uri: uri.clone(),
        display_name: display_name.clone(),
        created_at: now,
        payload: payload.clone(),
    };

    {
        let mut scheduled = state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned");
        if scheduled.get(&room.id).is_some_and(|list| list.iter().any(|record| record.uri == uri || record.id == cs_id))
        {
            return conflict_error("The specified call string is used in session");
        }
        scheduled.entry(room.id).or_default().push(record);
    }

    let ip = client_ip.as_ref().map(|c| c.0 .0.as_str()).unwrap_or("127.0.0.1");
    state.record_audit_entry(
        &auth.subject,
        "session.call_string.standby_add",
        Some(format!("{}:{}", room.id, uri)),
        ip,
    );

    (
        StatusCode::CREATED,
        Json(json!({
            "call_string": {
                "id": cs_id,
                "addressId": cs_id,
                "created_at": now,
                "uri": uri,
                "display_name": display_name,
                "payload":payload
            }
        })),
    )
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

    for item in body.call_strings {
        // Native recall can send an id without repeating the URI.
        let previous = lookup_session_record(&state, &room.id, item.id.as_deref());
        let raw = item.uri.clone().or_else(|| previous.as_ref().map(|r| r.uri.clone())).unwrap_or_default();
        if raw.trim().is_empty() {
            return validation_error("invalid.uri", "uri");
        }
        let record = CallStringRecord {
            id: item.id.unwrap_or_else(|| format!("cs-{}", Uuid::new_v4())),
            uri: canonical_uri(&raw),
            display_name: item
                .display_name
                .or_else(|| previous.as_ref().map(|r| r.display_name.clone()))
                .unwrap_or(raw),
            created_at: now_unix_sec(),
            payload: merged_contact_payload(previous.as_ref().map(|record| &record.payload), item.payload.as_ref()),
        };
        if let Err(response) = queue_call(&state, room.id, record) {
            return *response;
        }
    }

    Json(json!({ "status": "ok" })).into_response()
}

fn lookup_session_record(state: &SharedState, room_id: &RoomId, id: Option<&str>) -> Option<CallStringRecord> {
    let id = id?;
    let previous = state
        .api_v3
        .call_attempts
        .lock()
        .expect("call_attempts poisoned")
        .iter()
        .find(|((rid, _), attempt)| rid == room_id && attempt.record.id == id)
        .map(|(_, attempt)| attempt.record.clone());
    previous.or_else(|| {
        state
            .api_v3
            .scheduled_calls
            .lock()
            .expect("scheduled_calls poisoned")
            .get(room_id)
            .and_then(|records| records.iter().find(|record| record.id == id))
            .cloned()
    })
}

/// Сериализация адреса в форме vendor (`id/uri/display_name/created_at/payload/contact`).
pub(crate) fn address_value(id: &str, address: &AddressRecord) -> Value {
    let mut record = stored_address_record(address);
    record.id = id.to_string();
    address_book_record(&record)
}

fn address_book_record(record: &CallStringRecord) -> Value {
    // Producers хранят уже нормализованный contact-payload; fallback — для записей,
    // пришедших из старых/внешних writers.
    let payload = match record.payload.get("call_string") {
        Some(call_string) if call_string.is_object() => call_string.clone(),
        _ => default_contact_payload(Some(&record.payload))["call_string"].clone(),
    };
    json!({"id":record.id,"uri":record.uri,"display_name":record.display_name,"created_at":record.created_at,
        "busy":false,"payload":payload,"contact":{"id":record.id,"type":"call_strings",
            "contact_type":"call_strings","display_name":record.display_name}})
}

async fn address_book_records(state: &SharedState, auth: &AuthContext) -> Vec<Value> {
    let mut records = std::collections::BTreeMap::new();
    {
        let resources = state.api_v3.resources.lock().await;
        for (id, address) in &resources.addresses {
            records.insert(id.clone(), address_book_record(&stored_address_record(address)));
        }
        for (room_id, contacts) in &resources.contacts {
            if check_permission(auth, Some(room_id)).is_err() {
                continue;
            }
            for contact in contacts {
                let Some(id) = contact["id"].as_str() else {
                    continue;
                };
                let record = CallStringRecord {
                    id: id.to_string(),
                    uri: contact["uri"].as_str().unwrap_or_default().to_string(),
                    display_name: contact["display_name"].as_str().unwrap_or_default().to_string(),
                    created_at: contact["created_at"].as_u64().unwrap_or(0),
                    payload: default_contact_payload(contact.get("payload")),
                };
                records.insert(id.to_string(), address_book_record(&record));
            }
        }
    }
    {
        let attempts = state.api_v3.call_attempts.lock().expect("call_attempts poisoned");
        for ((room_id, _), attempt) in attempts.iter() {
            if check_permission(auth, Some(&room_id.to_string())).is_ok() {
                records.insert(attempt.record.id.clone(), address_book_record(&attempt.record));
            }
        }
    }
    {
        let scheduled = state.api_v3.scheduled_calls.lock().expect("scheduled_calls poisoned");
        for (room_id, items) in scheduled.iter() {
            if check_permission(auth, Some(&room_id.to_string())).is_ok() {
                for record in items {
                    records.insert(record.id.clone(), address_book_record(record));
                }
            }
        }
    }
    records.into_values().collect()
}

/// Адрес из глобальной адресной книги: payload хранится как protocol-map
/// (sip/h323/…), наружу отдаётся вложенным в полный контактный payload.
fn stored_address_record(address: &AddressRecord) -> CallStringRecord {
    CallStringRecord {
        id: address.id.clone(),
        uri: address.uri.clone(),
        display_name: address.display_name.clone(),
        created_at: address.created_at,
        payload: default_contact_payload(Some(&json!({"call_string": address.payload}))),
    }
}

#[derive(Debug)]
struct AddressFilter {
    list: ListParams,
    protocols: Vec<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    uri: String,
}

fn csv(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().filter_map(Value::as_str).map(|item| item.to_ascii_lowercase()).collect(),
        Value::String(raw) => {
            raw.split(',').map(|part| part.trim().to_ascii_lowercase()).filter(|part| !part.is_empty()).collect()
        }
        _ => Vec::new(),
    }
}

/// Один парсер для HTTP-query и WS-payload: общие `page/sort` — [`ListParams`],
/// сверху фильтры адресной книги (`protocols`, include/exclude, `uri`).
impl AddressFilter {
    fn from_value(params: &Value) -> Self {
        let list = |key: &str| params.get(key).map(csv).unwrap_or_default();
        Self {
            list: ListParams::parse(params),
            protocols: list("protocols"),
            include: list("includeAddresses").into_iter().chain(list("call_strings")).collect(),
            exclude: list("excludeAddresses"),
            uri: params.get("uri").and_then(Value::as_str).unwrap_or_default().to_string(),
        }
    }

    /// Семантика vendor-поиска: подстрока в имени/адресе (`sort_and_page`),
    /// фильтр по протоколам и явные include/exclude по id.
    fn matches(&self, record: &Value) -> bool {
        let id = record["id"].as_str().unwrap_or_default();
        if !self.include.is_empty() && !self.include.iter().any(|item| item == id) {
            return false;
        }
        if self.exclude.iter().any(|item| item == id) {
            return false;
        }
        let uri = record["uri"].as_str().unwrap_or_default();
        if !self.uri.is_empty() && !uri.to_lowercase().contains(&self.uri.to_lowercase()) {
            return false;
        }
        if !self.protocols.is_empty() {
            let protocol = uri.split_once(':').map(|(scheme, _)| scheme.to_ascii_lowercase()).unwrap_or_default();
            if !self.protocols.iter().any(|item| item == &protocol) {
                return false;
            }
        }
        true
    }

    fn apply(&self, mut records: Vec<Value>) -> (usize, Vec<Value>) {
        records.retain(|record| self.matches(record));
        let list = &self.list;
        super::sort_and_page(records, &list.search, &list.sort_field, list.sort_order, list.page, list.page_size)
    }
}

/// `ui:call_strings:search` — тот же источник/фильтры, что у HTTP-списка,
/// но по WebSocket-каналу (его использует автодополнение и адресная книга).
pub(crate) async fn ws_search(
    state: &SharedState,
    user_id: &str,
    room_scope: Option<&str>,
    payload: &Value,
) -> Result<Value, (u16, &'static str)> {
    let auth = AuthContext {
        subject: user_id.to_string(),
        role: crate::users::Users::resolve_role(state, user_id).unwrap_or(Role::Viewer),
        auth_type: crate::auth_middleware::AuthType::Jwt,
        scopes: Vec::new(),
        room_scope: room_scope.map(str::to_string),
        generation: 0,
        client_ip: "127.0.0.1".to_string(),
    };
    if check_permission(&auth, None).is_err() {
        return Err((403, "Forbidden: insufficient permissions"));
    }
    if let Some(contact_id) = payload.get("contact_id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
        return super::directory::ws_user_call_strings(state, user_id, payload, contact_id).await;
    }
    let (count, records) = AddressFilter::from_value(payload).apply(address_book_records(state, &auth).await);
    Ok(json!({"call_strings": records, "count": count}))
}

/// `GET /api/v3/call_strings` и `GET /api/v3/ui/call_strings` — адресная книга:
/// сохранённые адреса, контакты комнат и отложенные вызовы с фильтрами UI.
pub async fn list_call_strings(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    track("call_strings");
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let params = Value::Object(params.into_iter().map(|(key, value)| (key, Value::String(value))).collect());
    let (count, call_strings) = AddressFilter::from_value(&params).apply(address_book_records(&state, &auth).await);
    Json(json!({"count":count,"call_strings":call_strings})).into_response()
}

fn address_payload(previous: Option<&Value>, patch: Option<&Value>) -> Value {
    let mut payload = previous.cloned().unwrap_or_else(|| json!({}));
    if !payload.is_object() {
        payload = json!({});
    }
    if let Some(protocols) = patch.and_then(Value::as_object) {
        for (protocol, options) in protocols {
            super::resources::merge_json(&mut payload[protocol], options);
        }
    }
    payload
}

fn can_write_address_book(auth: &AuthContext) -> bool {
    auth.role >= Role::Operator && auth.room_scope.is_none()
}

fn address_fields(body: &Value) -> Result<(String, String, Option<Value>), (&'static str, &'static str)> {
    for (field, position) in [("uri", "uri"), ("display_name", "display_name")] {
        if let Some(value) = body.get(field) {
            if !value.is_null() && !value.is_string() {
                return Err(("field must be a string", position));
            }
        }
    }
    let raw = body["uri"].as_str().unwrap_or_default().trim();
    if raw.is_empty() {
        return Err(("uri is required", "uri"));
    }
    let uri = canonical_uri(raw);
    let (protocol, address) = uri.split_once(':').unwrap_or_default();
    if !matches!(protocol, "sip" | "h323" | "rtsp" | "vnc" | "media" | "file") {
        return Err(("uri protocol must be sip, h323, rtsp, vnc, media or file", "uri"));
    }
    if address.is_empty() {
        return Err(("uri address is required", "uri"));
    }
    let payload = match body.get("payload") {
        None | Some(Value::Null) => None,
        Some(payload) if payload.is_object() => Some(payload.clone()),
        Some(_) => return Err(("payload must be an object", "payload")),
    };
    if let Some(protocols) = payload.as_ref().and_then(Value::as_object) {
        if protocols.values().any(|options| !options.is_object()) {
            return Err(("payload protocol options must be an object", "payload"));
        }
    }
    let display_name =
        body["display_name"].as_str().map(str::trim).filter(|value| !value.is_empty()).unwrap_or(address).to_string();
    Ok((uri, display_name, payload))
}

/// `POST /api/v3/call_strings` — сохранить адрес в адресную книгу.
pub async fn create_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    if !can_write_address_book(&auth) {
        return error_json(403, "Forbidden: insufficient permissions");
    }
    if !body.is_object() {
        return validation_error("body must be an object", "body");
    }
    let (uri, display_name, patch) = match address_fields(&body) {
        Ok(values) => values,
        Err((reason, position)) => return validation_error(reason, position),
    };
    let id = format!("cs-{}", Uuid::new_v4());
    let record = AddressRecord {
        id: id.clone(),
        uri,
        display_name,
        created_at: now_unix_sec(),
        payload: address_payload(None, patch.as_ref()),
    };
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    resources.addresses.insert(id, record.clone());
    if let Err(response) = super::resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    (StatusCode::CREATED, Json(json!({"call_string": address_book_record(&stored_address_record(&record))})))
        .into_response()
}

/// Куда указывает id из адресной книги: глобальное хранилище или контакт комнаты.
enum AddressTarget {
    Global,
    Contact { room_id: String, index: usize },
}

enum AddressLookup {
    Found(AddressTarget),
    Missing,
    /// Один id встречается в нескольких комнатах: контакты переносятся с сохранением
    /// vendor `id`, и неоднозначность нельзя решать молчаливым выбором.
    Ambiguous,
}

fn address_target(resources: &ResourceState, id: &str) -> AddressLookup {
    let mut found: Option<AddressTarget> = resources.addresses.contains_key(id).then_some(AddressTarget::Global);
    for (room_id, contacts) in &resources.contacts {
        if let Some(index) = contacts.iter().position(|contact| contact["id"].as_str() == Some(id)) {
            if found.is_some() {
                return AddressLookup::Ambiguous;
            }
            found = Some(AddressTarget::Contact { room_id: room_id.clone(), index });
        }
    }
    found.map_or(AddressLookup::Missing, AddressLookup::Found)
}

fn contact_write_allowed(auth: &AuthContext, room_id: &str) -> Option<Response> {
    if auth.role < Role::Operator {
        return Some(error_json(403, "Forbidden: Operator role required to manage call strings"));
    }
    check_permission(auth, Some(room_id)).err()
}

/// `PATCH /api/v3/call_strings/{id}` — правка адреса: глобальная книга или
/// контакт комнаты (vendor `editAddress(addressId)` для контактов).
pub async fn update_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut resources = state.api_v3.resources.lock().await;
    let target = match address_target(&resources, &id) {
        AddressLookup::Found(target) => target,
        AddressLookup::Missing => return error_json(404, "Call string not found"),
        AddressLookup::Ambiguous => return error_json(409, "Call string id is ambiguous"),
    };
    match target {
        AddressTarget::Global => {
            if !can_write_address_book(&auth) {
                return error_json(403, "Forbidden: insufficient permissions");
            }
            let Some(current) = resources.addresses.get(&id).cloned() else {
                return error_json(404, "Call string not found");
            };
            if !body.is_object() {
                return validation_error("body must be an object", "body");
            }
            let mut candidate = body.clone();
            if candidate.get("uri").is_none_or(Value::is_null) {
                candidate["uri"] = json!(current.uri);
            }
            if candidate.get("display_name").is_none_or(Value::is_null) {
                candidate["display_name"] = json!(current.display_name);
            }
            let (uri, display_name, patch) = match address_fields(&candidate) {
                Ok(values) => values,
                Err((reason, position)) => return validation_error(reason, position),
            };
            let previous = resources.clone();
            let record = AddressRecord {
                id,
                uri,
                display_name,
                created_at: current.created_at,
                payload: address_payload(Some(&current.payload), patch.as_ref()),
            };
            resources.addresses.insert(record.id.clone(), record.clone());
            if let Err(response) = super::resources::commit(&state, &mut resources, previous).await {
                return response;
            }
            Json(json!({"call_string": address_book_record(&stored_address_record(&record))})).into_response()
        }
        AddressTarget::Contact { room_id, index } => {
            if let Some(response) = contact_write_allowed(&auth, &room_id) {
                return response;
            }
            let Some(current) = resources.contacts.get(&room_id).and_then(|contacts| contacts.get(index)).cloned()
            else {
                return error_json(404, "Call string not found");
            };
            let flat_payload = current["payload"].get("call_string").cloned().unwrap_or_else(|| json!({}));
            if !body.is_object() {
                return validation_error("body must be an object", "body");
            }
            let mut candidate = body.clone();
            if candidate.get("uri").is_none_or(Value::is_null) {
                candidate["uri"] = current["uri"].clone();
            }
            if candidate.get("display_name").is_none_or(Value::is_null) {
                candidate["display_name"] = current["display_name"].clone();
            }
            let (uri, display_name, patch) = match address_fields(&candidate) {
                Ok(values) => values,
                Err((reason, position)) => return validation_error(reason, position),
            };
            // Сохраняем top-level настройки контакта (auto_call, volume_*, флаги
            // медиа, crown, slot_render_type, stop_details и пр.), меняя только
            // вложенный `call_string` — vendor `editAddress` присылает патч одного поля.
            let merged_call_string = address_payload(Some(&flat_payload), patch.as_ref());
            let mut base = match current["payload"].as_object() {
                Some(object) => Value::Object(object.clone()),
                None => json!({}),
            };
            base["call_string"] = merged_call_string;
            let payload = default_contact_payload(Some(&base));
            let previous = resources.clone();
            if let Some(contact) = resources.contacts.get_mut(&room_id).and_then(|contacts| contacts.get_mut(index)) {
                contact["uri"] = json!(uri);
                if body.get("uri").is_some_and(|value| !value.is_null()) {
                    contact["connection_url"] = json!(uri);
                }
                contact["display_name"] = json!(display_name);
                contact["payload"] = payload.clone();
                if contact["call_string"].is_object() {
                    contact["call_string"]["uri"] = json!(uri);
                    contact["call_string"]["display_name"] = json!(display_name);
                }
            }
            if let Err(response) = super::resources::commit(&state, &mut resources, previous).await {
                return response;
            }
            let record = CallStringRecord {
                id,
                uri,
                display_name,
                created_at: current["created_at"].as_u64().unwrap_or_else(now_unix_sec),
                payload,
            };
            Json(json!({"call_string": address_book_record(&record)})).into_response()
        }
    }
}

/// `DELETE /api/v3/call_strings/{id}` — удалить адрес из адресной книги.
pub async fn delete_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    let mut resources = state.api_v3.resources.lock().await;
    let target = match address_target(&resources, &id) {
        AddressLookup::Found(target) => target,
        AddressLookup::Missing => return error_json(404, "Call string not found"),
        AddressLookup::Ambiguous => return error_json(409, "Call string id is ambiguous"),
    };
    match target {
        AddressTarget::Global => {
            if !can_write_address_book(&auth) {
                return error_json(403, "Forbidden: insufficient permissions");
            }
            let previous = resources.clone();
            resources.addresses.remove(&id);
            if let Err(response) = super::resources::commit(&state, &mut resources, previous).await {
                return response;
            }
        }
        AddressTarget::Contact { room_id, index } => {
            if let Some(response) = contact_write_allowed(&auth, &room_id) {
                return response;
            }
            let previous = resources.clone();
            if let Some(contacts) = resources.contacts.get_mut(&room_id) {
                contacts.remove(index);
            }
            if let Err(response) = super::resources::commit(&state, &mut resources, previous).await {
                return response;
            }
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn get_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    match address_book_records(&state, &auth).await.into_iter().find(|record| record["id"].as_str() == Some(&id)) {
        Some(record) => Json(json!({"call_string":record})).into_response(),
        None => error_json(404, "Call string not found"),
    }
}

#[cfg(test)]
#[path = "call_lifecycle_tests.rs"]
mod call_lifecycle_tests;
