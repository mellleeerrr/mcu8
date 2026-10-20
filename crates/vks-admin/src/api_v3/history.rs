//! История конференций PNM (T2.2).
//!
//! Контракт снят с vendor `/api/v3/call_history*` (ref-92):
//! * `GET /call_history` — постраничный список прошедших сессий;
//! * `GET /call_history/{id}` — сессия + `call_strings` (вызовы) + `invitations`;
//! * `GET /ui/session_history/{id}` — шапка детальной страницы;
//! * `GET /ui/call_history/{id}` — список вызовов сессии;
//! * `GET /call_history/export` → `{filename}`, файл на `/static/call_history/{filename}`
//!   (ZIP с CSV `;` и русскими заголовками, метод store — как у vendor);
//! * `POST /call_history/clean` — backup-ZIP и очистка истории.
//!
//! История пишется в configuration-document (`resources.history`), переживает
//! рестарт; запись создаётся в момент остановки/удаления сессии.

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use vks_core::{Role, RoomId};

use crate::auth_middleware::AuthContext;
use crate::handlers::SharedState;
use crate::state::{now_unix_ms, AppState, RoomRecord};

use super::sessions::{conference_payload_defaults, native_stop_reason};

/// Сессия истории (персистентная). `room_id` — внутренняя связка с комнатой
/// (для дедупликации повторной остановки), в JSON не отдаётся.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistorySession {
    pub id: String,
    #[serde(default)]
    pub room_id: String,
    pub display_name: String,
    pub started_at: i64,
    pub stopped_at: i64,
    pub stop_reason: String,
    #[serde(default)]
    pub abonents_count: u32,
    #[serde(default)]
    pub creator_id: String,
    #[serde(default)]
    pub stopped_by: String,
    #[serde(default)]
    pub calls: Vec<HistoryCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dtmf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

/// Вызов (call string) внутри сессии истории.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryCall {
    pub id: String,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
    pub display_name: String,
    pub connection_url: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<Value>,
    #[serde(default)]
    pub invitations: Vec<Value>,
    #[serde(default)]
    pub abonents: Vec<Value>,
}

/// Корень персистентной истории.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistoryState {
    #[serde(default)]
    pub sessions: Vec<HistorySession>,
}

/// Максимум хранимых сессий (защита configuration-document от роста).
const MAX_SESSIONS: usize = 500;

impl HistoryState {
    fn record(&mut self, session: HistorySession) {
        // Повторная остановка уже записанной сессии не создаёт дубль.
        if self.sessions.iter().any(|s| s.room_id == session.room_id && s.started_at == session.started_at) {
            return;
        }
        self.sessions.insert(0, session);
        self.sessions.truncate(MAX_SESSIONS);
    }

    fn find(&self, id: &str) -> Option<&HistorySession> {
        self.sessions.iter().find(|s| s.id == id)
    }
}

/// Параметры списка/экспорта. Vendor-UI шлёт `page/page_size/sort_field/
/// sort_order`; фильтры даты/названия — наши расширения для формы истории.
#[derive(Debug, Default, Deserialize)]
pub struct HistoryQuery {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
    pub sort_field: Option<String>,
    pub sort_order: Option<String>,
    pub display_name: Option<String>,
    pub search: Option<String>,
    pub started_at: Option<i64>,
    pub stopped_at: Option<i64>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub creator_id: Option<String>,
}

impl HistoryQuery {
    fn filtered<'a>(&self, sessions: &'a [HistorySession]) -> Vec<&'a HistorySession> {
        let needle = self.display_name.as_deref().or(self.search.as_deref()).map(str::to_lowercase);
        let from = self.started_at.or(self.from);
        let to = self.stopped_at.or(self.to);
        let mut rows: Vec<&HistorySession> = sessions
            .iter()
            .filter(|s| {
                needle.as_deref().is_none_or(|needle| {
                    s.display_name.to_lowercase().contains(needle) || s.id.to_lowercase().contains(needle)
                })
            })
            .filter(|s| from.is_none_or(|from| s.started_at >= from))
            .filter(|s| to.is_none_or(|to| s.stopped_at <= to))
            .filter(|s| self.creator_id.as_deref().is_none_or(|id| s.creator_id == id))
            .collect();
        let descending = matches!(self.sort_order.as_deref(), Some("1") | Some("desc"));
        rows.sort_by(|left, right| {
            let order = match self.sort_field.as_deref().unwrap_or("stopped_at") {
                "started_at" => (left.started_at, left.stopped_at).cmp(&(right.started_at, right.stopped_at)),
                "display_name" => left.display_name.to_lowercase().cmp(&right.display_name.to_lowercase()),
                _ => (left.stopped_at, left.started_at).cmp(&(right.stopped_at, right.started_at)),
            };
            if descending {
                order.reverse()
            } else {
                order
            }
        });
        rows
    }

    fn pagination(&self) -> (usize, usize) {
        let page = self.page.unwrap_or(1).max(1);
        let size = self.page_size.unwrap_or(25).clamp(1, 200);
        ((page - 1) * size, size)
    }
}

/// Пользователь-контакт для `creator`/`stopper` (vendor отдаёт объект, даже
/// если профиль удалён — тогда display_name = id).
async fn contact_json(state: &AppState, id: &str) -> Value {
    if id.is_empty() {
        return Value::Null;
    }
    let display_name = {
        let resources = state.api_v3.resources.lock().await;
        resources
            .directory
            .users
            .get(id)
            .map(|u| u.display_name.clone())
            .or_else(|| {
                state.users.lock().ok().and_then(|users| {
                    users.values().find(|user| user.id.to_string() == id).map(|user| user.username.clone())
                })
            })
            .unwrap_or_else(|| id.to_string())
    };
    json!({
        "id": id,
        "contact_type": "users",
        "display_name": display_name,
        "deleted_at": Value::Null,
        "avatar": Value::Null,
        "address": Value::Null,
    })
}

fn route_label(state: &AppState, room_id: &RoomId) -> Option<String> {
    state
        .api_v3
        .conference_settings
        .lock()
        .ok()
        .and_then(|settings| settings.get(room_id).cloned())
        .and_then(|settings| settings.get("route").and_then(Value::as_str).map(str::to_string))
        .filter(|route| !route.is_empty())
}

fn dtmf_label(state: &AppState, room_id: &RoomId) -> Option<String> {
    state
        .api_v3
        .conference_settings
        .lock()
        .ok()
        .and_then(|settings| settings.get(room_id).cloned())
        .and_then(|settings| settings.get("dtmf").and_then(Value::as_str).map(str::to_string))
        .filter(|dtmf| !dtmf.is_empty())
}

fn creator_id(state: &AppState, room_id: &RoomId) -> String {
    state
        .api_v3
        .conference_settings
        .lock()
        .ok()
        .and_then(|settings| settings.get(room_id).cloned())
        .and_then(|settings| {
            settings.get("creator_id").or_else(|| settings.get("owner_id")).and_then(Value::as_str).map(str::to_string)
        })
        .unwrap_or_default()
}

fn session_calls(state: &AppState, room: &RoomRecord, stopped_at: i64) -> Vec<HistoryCall> {
    let attempts = state.api_v3.call_attempts.lock().map(|attempts| attempts.clone()).unwrap_or_default();
    let mut calls = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for ((room_id, uri), attempt) in attempts {
        if room_id != room.id || !seen.insert(attempt.record.id.clone()) {
            continue;
        }
        let finished = attempt.completed_at.map(|ts| ts as i64).unwrap_or(stopped_at);
        let finish_reason = native_stop_reason(attempt.failure_reason.as_deref().unwrap_or("Ended By Remote"));
        let invitation = json!({
            "display_name": attempt.record.display_name,
            "created_at": room.created_ms as i64 / 1000,
            "finished_at": finished,
            "connection_url": uri,
            "abonent_id": attempt.participant_id,
            "finish_reason": finish_reason,
            "contact_type": "call_strings",
            "contact_id": attempt.record.id,
            "payload": attempt.record.payload,
        });
        calls.push(HistoryCall {
            id: attempt.record.id,
            started_at: room.created_ms as i64 / 1000,
            deleted_at: Some(stopped_at),
            display_name: attempt.record.display_name,
            connection_url: uri,
            payload: attempt.record.payload,
            contact: None,
            invitations: vec![invitation],
            abonents: Vec::new(),
        });
    }
    calls.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()).then(a.id.cmp(&b.id)));
    calls
}

/// Записать остановленную сессию в историю и сохранить configuration-document.
/// Вызывается ДО очистки `call_attempts`/участников, чтобы снимок был полным.
pub async fn record_stopped(state: &SharedState, room: &RoomRecord, stopped_by: &str, reason: &str) {
    let started_at = room.created_ms as i64 / 1000;
    let stopped_at = (now_unix_ms() / 1000) as i64;
    let participants = state.room_hub.list_participants(&room.id);
    let abonents_count = participants.len() as u32;
    let calls = session_calls(state, room, stopped_at);
    let payload = {
        let stored = state
            .api_v3
            .conference_settings
            .lock()
            .ok()
            .and_then(|settings| settings.get(&room.id).cloned())
            .unwrap_or_else(|| json!({}));
        let mut payload = Value::Object(conference_payload_defaults());
        if let (Some(target), Some(stored)) = (payload.as_object_mut(), stored.as_object()) {
            for (key, value) in stored {
                target.insert(key.clone(), value.clone());
            }
        }
        payload
    };
    let session = HistorySession {
        id: uuid::Uuid::new_v4().to_string(),
        room_id: room.id.to_string(),
        display_name: room.name.clone(),
        started_at,
        stopped_at,
        stop_reason: reason.to_string(),
        abonents_count,
        creator_id: creator_id(state, &room.id),
        stopped_by: stopped_by.to_string(),
        calls,
        dtmf: dtmf_label(state, &room.id),
        route: route_label(state, &room.id),
        payload,
    };
    let (previous, updated) = {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        resources.history.record(session);
        (previous, resources.clone())
    };
    if let Err(error) = super::resources::persist(state, &updated).await {
        *state.api_v3.resources.lock().await = previous;
        tracing::warn!(error = %error, "history: persist failed");
    }
}

fn session_list_json(session: &HistorySession, creator: &Value, stopper: &Value) -> Value {
    json!({
        "id": session.id,
        "display_name": session.display_name,
        "started_at": session.started_at,
        "stopped_at": session.stopped_at,
        "stop_reason": session.stop_reason,
        "abonents": session.abonents_count,
        "creator_id": session.creator_id,
        "stopped_by": session.stopped_by,
        "creator": creator,
        "stopper": stopper,
        "dtmf": session.dtmf,
        "route": session.route,
        "payload": session.payload,
    })
}

/// `GET /api/v3/call_history` → `{count, sessions:[…]}`.
pub async fn list(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    super::track("call_history");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let (count, page_rows) = {
        let resources = state.api_v3.resources.lock().await;
        let rows = query.filtered(&resources.history.sessions);
        let (offset, size) = query.pagination();
        let count = rows.len();
        let page_rows: Vec<HistorySession> = rows.into_iter().skip(offset).take(size).cloned().collect();
        (count, page_rows)
    };
    let mut sessions = Vec::with_capacity(page_rows.len());
    for row in &page_rows {
        let creator = contact_json(&state, &row.creator_id).await;
        let stopper = contact_json(&state, &row.stopped_by).await;
        sessions.push(session_list_json(row, &creator, &stopper));
    }
    Json(json!({ "count": count, "sessions": sessions })).into_response()
}

/// `GET /api/v3/call_history/{id}` → `{session:{…,call_strings}}`.
pub async fn detail(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    super::track("call_history/{id}");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let session = { state.api_v3.resources.lock().await.history.find(&id).cloned() };
    let Some(session) = session else {
        return super::error_json(404, "Session not found");
    };
    let creator = contact_json(&state, &session.creator_id).await;
    let stopper = contact_json(&state, &session.stopped_by).await;
    Json(json!({
        "session": {
            "id": session.id,
            "display_name": session.display_name,
            "started_at": session.started_at,
            "stopped_at": session.stopped_at,
            "stop_reason": session.stop_reason,
            "abonents_count": session.abonents_count,
            "creator_id": session.creator_id,
            "stopped_by": session.stopped_by,
            "creator": creator,
            "stopper": stopper,
            "dtmf": session.dtmf,
            "route": session.route,
            "call_strings": session.calls,
            "payload": session.payload,
        }
    }))
    .into_response()
}

/// `GET /api/v3/ui/session_history/{id}` — шапка страницы истории сессии.
pub async fn ui_session_history(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    super::track("ui/session_history/{id}");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let session = { state.api_v3.resources.lock().await.history.find(&id).cloned() };
    let Some(session) = session else {
        return super::error_json(404, "Session not found");
    };
    let started_by = contact_json(&state, &session.creator_id).await;
    let stopped_by = contact_json(&state, &session.stopped_by).await;
    Json(json!({
        "session": {
            "display_name": session.display_name,
            "started_at": session.started_at,
            "stopped_at": session.stopped_at,
            "stop_reason": session.stop_reason,
            "abonents_count": session.abonents_count,
            "started_by": started_by,
            "stopped_by": stopped_by,
            "dtmf": session.dtmf,
            "route": session.route,
            "payload": session.payload,
        }
    }))
    .into_response()
}

/// `GET /api/v3/ui/call_history/{id}` → `{count, call_strings:[…]}`.
pub async fn ui_call_history(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    super::track("ui/call_history/{id}");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let session = { state.api_v3.resources.lock().await.history.find(&id).cloned() };
    let Some(session) = session else {
        return super::error_json(404, "Session not found");
    };
    let mut calls = session.calls;
    let descending = matches!(query.sort_order.as_deref(), Some("1") | Some("desc"));
    match query.sort_field.as_deref().unwrap_or("started_at") {
        "display_name" => calls.sort_by_key(|c| c.display_name.to_lowercase()),
        _ => calls.sort_by_key(|c| (c.started_at, c.id.clone())),
    }
    if descending {
        calls.reverse();
    }
    Json(json!({ "count": calls.len(), "call_strings": calls })).into_response()
}

/// `GET /api/v3/call_history/export` → `{filename}`; файл отдаётся по
/// `/static/call_history/{filename}` (см. [`static_export`]).
pub async fn export(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    super::track("call_history/export");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let sessions = {
        let resources = state.api_v3.resources.lock().await;
        query.filtered(&resources.history.sessions).into_iter().cloned().collect::<Vec<_>>()
    };
    let filename = format!("Conference_history_{}.zip", format_date_utc((now_unix_ms() / 1000) as i64));
    let archive = build_history_zip(&filename.replace(".zip", ".csv"), &sessions);
    state.api_v3.history_exports.lock().expect("history_exports poisoned").insert(filename.clone(), archive);
    Json(json!({ "filename": filename })).into_response()
}

/// `GET /api/v3/call_history/{id}/export` → `{filename}`: ZIP с одной сессией
/// (vendor-UI скачивает историю из строки списка/карточки именно так).
pub async fn export_session(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    super::track("call_history/{id}/export");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let session = { state.api_v3.resources.lock().await.history.find(&id).cloned() };
    let Some(session) = session else {
        return super::error_json(404, "Session not found");
    };
    let filename = format!("Conference_history_{}.zip", sanitize_filename(&session.display_name));
    let archive = build_history_zip(&filename.replace(".zip", ".csv"), std::slice::from_ref(&session));
    state.api_v3.history_exports.lock().expect("history_exports poisoned").insert(filename.clone(), archive);
    Json(json!({ "filename": filename })).into_response()
}

/// `POST /api/v3/call_history/clean` → `{filename}` backup-архива; история очищена.
pub async fn clean(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("call_history/clean");
    if auth.role < Role::Operator || auth.room_scope.is_some() {
        return super::error_json(403, "Forbidden: Operator role required");
    }
    let (previous, updated) = {
        let mut resources = state.api_v3.resources.lock().await;
        if resources.history.sessions.is_empty() {
            return Json(json!({ "filename": Value::Null })).into_response();
        }
        let previous = resources.clone();
        resources.history.sessions.clear();
        (previous, resources.clone())
    };
    let stamp = format_date_utc((now_unix_ms() / 1000) as i64).replace('-', "");
    let filename = format!("Conference_history_backup_{stamp}_{}.zip", now_unix_ms() % 100_000);
    let archive = build_history_zip(&filename.replace(".zip", ".csv"), &previous.history.sessions);
    state.api_v3.history_exports.lock().expect("history_exports poisoned").insert(filename.clone(), archive);
    if let Err(error) = super::resources::persist(&state, &updated).await {
        *state.api_v3.resources.lock().await = previous;
        return super::error_json(500, &format!("Storage error: {error}"));
    }
    Json(json!({ "filename": filename })).into_response()
}

/// `GET /static/call_history/{filename}` — скачивание ZIP (vendor отдаёт без
/// авторизации; имя санитизируется, чужие файлы недоступны).
pub async fn static_export(State(state): State<SharedState>, Path(filename): Path<String>) -> Response {
    if filename.contains('/') || filename.contains("..") {
        return super::error_json(400, "Invalid filename");
    }
    let archive = state.api_v3.history_exports.lock().ok().and_then(|files| files.get(&filename).cloned());
    match archive {
        Some(bytes) => {
            let mut response =
                (StatusCode::OK, [(header::CONTENT_TYPE, "application/zip")], Body::from(bytes)).into_response();
            if let Ok(value) = header::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
                response.headers_mut().insert(header::CONTENT_DISPOSITION, value);
            }
            response
        }
        None => super::error_json(404, "File not found"),
    }
}

fn require_read(auth: &AuthContext) -> Option<Response> {
    if auth.role < Role::Moderator {
        return Some(super::error_json(403, "Forbidden: insufficient permissions"));
    }
    if auth.room_scope.is_some() {
        return Some(super::error_json(403, "Forbidden: token room scope mismatch"));
    }
    None
}

// ── CSV/ZIP ────────────────────────────────────────────────────────────────

/// Имя файла из display_name: только безопасные символы (без `/` и `..`).
fn sanitize_filename(name: &str) -> String {
    let mut result: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    result.truncate(64);
    let result = result.trim_matches('_').to_string();
    if result.is_empty() {
        "session".to_string()
    } else {
        result
    }
}

/// CSV как у vendor: разделитель `;`, русские заголовки, даты
/// `ДД.ММ.ГГГГ ЧЧ:ММ:СС` (UTC — системная TZ сервера не фиксирована).
fn build_history_csv(sessions: &[HistorySession]) -> Vec<u8> {
    let mut text =
        String::from("Название;ID;Маршрут;Время начала;Время окончания;Длительность;Участники;Запуск;Остановка\n");
    for session in sessions {
        let duration = (session.stopped_at - session.started_at).max(0);
        let run = contact_label(session, &session.creator_id);
        let stop = contact_label(session, &session.stopped_by);
        text.push_str(&format!(
            "{};{};{};{};{};{};{};{};{}\n",
            csv_escape(&session.display_name),
            csv_escape(&session.id),
            csv_escape(session.route.as_deref().unwrap_or("")),
            format_datetime(session.started_at),
            format_datetime(session.stopped_at),
            format_duration(duration),
            session.abonents_count,
            csv_escape(&run),
            csv_escape(&stop),
        ));
    }
    text.into_bytes()
}

/// Имя контакта для CSV: профиль справочника по id, иначе id (vendor отдаёт
/// `Administrator`; после удаления пользователя останется id — честнее пустоты).
fn contact_label(_session: &HistorySession, id: &str) -> String {
    if id.is_empty() {
        String::new()
    } else {
        id.to_string()
    }
}

fn csv_escape(value: &str) -> String {
    if value.contains([';', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// ZIP (method store, как у vendor) с единственным CSV внутри.
fn build_history_zip(csv_name: &str, sessions: &[HistorySession]) -> Vec<u8> {
    let data = build_history_csv(sessions);
    let name = csv_name.as_bytes();
    let crc = crc32(&data);
    let (dos_time, dos_date) = dos_timestamp((now_unix_ms() / 1000) as i64);
    let mut out = Vec::with_capacity(data.len() + name.len() + 128);
    out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // store
    out.extend_from_slice(&dos_time.to_le_bytes());
    out.extend_from_slice(&dos_date.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(&data);
    let central_offset = out.len() as u32;
    out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&dos_time.to_le_bytes());
    out.extend_from_slice(&dos_date.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(name);
    let central_size = out.len() as u32 - central_offset;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn dos_timestamp(secs: i64) -> (u16, u16) {
    let (year, month, day, hour, minute, second) = utc_parts(secs);
    let year = year.clamp(1980, 2107) as u16;
    let date = ((year - 1980) << 9) | ((month as u16) << 5) | day as u16;
    let time = ((hour as u16) << 11) | ((minute as u16) << 5) | (second as u16 / 2);
    (time, date)
}

fn format_datetime(secs: i64) -> String {
    let (year, month, day, hour, minute, second) = utc_parts(secs);
    format!("{day:02}.{month:02}.{year:04} {hour:02}:{minute:02}:{second:02}")
}

fn format_date_utc(secs: i64) -> String {
    let (year, month, day, _, _, _) = utc_parts(secs);
    format!("{year:04}-{month:02}-{day:02}")
}

fn format_duration(secs: i64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Гражданская дата из unix-секунд (UTC) — алгоритм Hinnant, без зависимостей.
fn utc_parts(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month as u32, day as u32, rem / 3600, (rem % 3600) / 60, rem % 60)
}

// ── Тесты ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, started: i64, stopped: i64) -> HistorySession {
        HistorySession {
            id: format!("id-{name}"),
            room_id: format!("room-{name}"),
            display_name: name.to_string(),
            started_at: started,
            stopped_at: stopped,
            stop_reason: "Manual".into(),
            abonents_count: 2,
            creator_id: "creator".into(),
            stopped_by: "stopper".into(),
            calls: vec![HistoryCall {
                id: "call-1".into(),
                started_at: started,
                deleted_at: Some(stopped),
                display_name: "sip:192.168.1.132".into(),
                connection_url: "sip:192.168.1.132".into(),
                payload: json!({}),
                contact: None,
                invitations: vec![json!({"finish_reason": "Ended By Remote"})],
                abonents: Vec::new(),
            }],
            dtmf: None,
            route: None,
            payload: json!({"bitrate": 2048}),
        }
    }

    #[test]
    fn filters_sorts_and_paginates() {
        let sessions = vec![session("Alpha", 1, 10), session("beta", 2, 20), session("Gamma", 3, 30)];
        let query =
            HistoryQuery { sort_field: Some("started_at".into()), sort_order: Some("1".into()), ..Default::default() };
        let names: Vec<&str> = query.filtered(&sessions).iter().map(|s| s.display_name.as_str()).collect();
        assert_eq!(names, ["Gamma", "beta", "Alpha"]);
        let query = HistoryQuery { display_name: Some("BET".into()), ..Default::default() };
        assert_eq!(query.filtered(&sessions).len(), 1);
        let query = HistoryQuery { page: Some(2), page_size: Some(1), ..Default::default() };
        assert_eq!(query.pagination(), (1, 1));
        let query = HistoryQuery { from: Some(2), to: Some(30), ..Default::default() };
        assert_eq!(query.filtered(&sessions).len(), 2);
    }

    #[test]
    fn csv_has_vendor_headers_and_escapes_values() {
        let mut evil = session("Name;with\"quotes", 0, 61);
        evil.route = Some("route;1".into());
        let csv = String::from_utf8(build_history_csv(&[evil])).unwrap();
        assert!(csv
            .starts_with("Название;ID;Маршрут;Время начала;Время окончания;Длительность;Участники;Запуск;Остановка\n"));
        assert!(csv.contains("\"Name;with\"\"quotes\""));
        assert!(csv.contains("00:01:01"));
        assert!(csv.contains("01.01.1970 00:00:00"));
    }

    #[test]
    fn zip_is_valid_store_archive_with_single_csv() {
        let archive = build_history_zip("Conference_history_2026-01-01.csv", &[session("Alpha", 0, 5)]);
        assert_eq!(&archive[..4], b"PK\x03\x04");
        assert_eq!(&archive[archive.len() - 22..archive.len() - 18], b"PK\x05\x06");
        let name = b"Conference_history_2026-01-01.csv";
        let found = archive.windows(name.len()).any(|w| w == name);
        assert!(found);
        // CRC/размеры локального заголовка согласованы с фактическим CSV.
        let data = build_history_csv(&[session("Alpha", 0, 5)]);
        assert_eq!(u32::from_le_bytes(archive[18..22].try_into().unwrap()) as usize, data.len());
        assert_eq!(u32::from_le_bytes(archive[22..26].try_into().unwrap()) as usize, data.len());
    }

    #[test]
    fn record_deduplicates_same_room_and_start() {
        let mut history = HistoryState::default();
        history.record(session("Alpha", 1, 2));
        history.record(session("Alpha", 1, 3));
        assert_eq!(history.sessions.len(), 1);
        history.record(session("Alpha", 2, 3));
        assert_eq!(history.sessions.len(), 2);
    }

    #[test]
    fn utc_formatting_is_stable() {
        assert_eq!(format_datetime(0), "01.01.1970 00:00:00");
        assert_eq!(format_datetime(1_000_000_000), "09.09.2001 01:46:40");
        assert_eq!(format_duration(3_661), "01:01:01");
        assert_eq!(format_date_utc(1_700_000_000), "2023-11-14");
    }
}
