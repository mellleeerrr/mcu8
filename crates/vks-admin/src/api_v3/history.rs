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
//!
//! Формы ответов намеренно различаются (vendor-контракт):
//! | endpoint | abonents | stopped_by |
//! |---|---|---|
//! | `GET /call_history` | счётчик `abonents` | id или null |
//! | `GET /call_history/{id}` | счётчик `abonents_count` | id или null |
//! | `GET /ui/session_history/{id}` | `abonents_count` | contact-объект |
//!
//! `HistoryFilter` (фильтры) и `PageSort` (страницы/сортировка) разделены:
//! экспорт использует только фильтры, а списки вызовов — только сортировку.

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

mod export;
use export::{build_history_zip, contact_names, current_timezone, format_date_utc, sanitize_filename};

/// Сессия истории (персистентная). `room_id` — внутренняя связка с комнатой
/// (для дедупликации повторной остановки), в JSON не отдаётся.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HistorySession {
    pub id: String,
    #[serde(default)]
    pub room_id: String,
    pub display_name: String,
    pub started_at: i64,
    /// Точное время старта в мс: дедуп повторной остановки не должен склеивать
    /// два старта в одну секунду.
    #[serde(default)]
    pub started_ms: u64,
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
        // Повторная остановка уже записанной сессии не создаёт дубль; сравнение
        // по мс (а не секундам) не склеивает соседние старты.
        let started_ms =
            if session.started_ms != 0 { session.started_ms } else { (session.started_at.max(0) as u64) * 1000 };
        if self.sessions.iter().any(|s| {
            let existing_ms = if s.started_ms != 0 { s.started_ms } else { (s.started_at.max(0) as u64) * 1000 };
            s.room_id == session.room_id && existing_ms == started_ms
        }) {
            return;
        }
        self.sessions.insert(0, session);
        self.sessions.truncate(MAX_SESSIONS);
    }

    fn find(&self, id: &str) -> Option<&HistorySession> {
        self.sessions.iter().find(|s| s.id == id)
    }
}

/// Фильтры списка/экспорта истории (без пагинации и сортировки).
///
/// Три исторических написания диапазона дат пришли из vendor-UI и старых
/// клиентов. Приоритет явно зафиксирован в [`HistoryFilter::date_range`]:
/// `started_at` > `from` > `date_from` (и аналогично для конца), чтобы
/// поведение не зависело от порядка `.or()`.
#[derive(Debug, Default, Deserialize)]
pub struct HistoryFilter {
    pub display_name: Option<String>,
    pub search: Option<String>,
    pub started_at: Option<i64>,
    pub stopped_at: Option<i64>,
    #[serde(default, alias = "dateFrom")]
    pub date_from: Option<i64>,
    #[serde(default, alias = "dateTo")]
    pub date_to: Option<i64>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub creator_id: Option<String>,
}

impl HistoryFilter {
    fn date_range(&self) -> (Option<i64>, Option<i64>) {
        (self.started_at.or(self.from).or(self.date_from), self.stopped_at.or(self.to).or(self.date_to))
    }

    fn filter<'a>(&self, sessions: &'a [HistorySession]) -> Vec<&'a HistorySession> {
        let needle = self.display_name.as_deref().or(self.search.as_deref()).map(str::to_lowercase);
        let (from, to) = self.date_range();
        sessions
            .iter()
            .filter(|s| {
                needle.as_deref().is_none_or(|needle| {
                    s.display_name.to_lowercase().contains(needle) || s.id.to_lowercase().contains(needle)
                })
            })
            .filter(|s| from.is_none_or(|from| s.started_at >= from))
            .filter(|s| to.is_none_or(|to| s.stopped_at <= to))
            .filter(|s| self.creator_id.as_deref().is_none_or(|id| s.creator_id == id))
            .collect()
    }
}

/// Пагинация и сортировка vendor-списков (`page/page_size/sort_field/sort_order`).
#[derive(Debug, Default, Deserialize)]
pub struct PageSort {
    pub page: Option<usize>,
    pub page_size: Option<usize>,
    pub sort_field: Option<String>,
    pub sort_order: Option<String>,
}

impl PageSort {
    fn pagination(&self) -> (usize, usize) {
        let page = self.page.unwrap_or(1).max(1);
        let size = self.page_size.unwrap_or(25).clamp(1, 200);
        ((page - 1) * size, size)
    }

    fn descending(&self) -> bool {
        matches!(self.sort_order.as_deref(), Some("1") | Some("desc"))
    }

    fn sort_sessions(&self, rows: &mut [&HistorySession], default_field: &str) {
        rows.sort_by(|left, right| {
            let order = match self.sort_field.as_deref().unwrap_or(default_field) {
                "started_at" => (left.started_at, left.stopped_at).cmp(&(right.started_at, right.stopped_at)),
                "display_name" => left.display_name.to_lowercase().cmp(&right.display_name.to_lowercase()),
                _ => (left.stopped_at, left.started_at).cmp(&(right.stopped_at, right.started_at)),
            };
            if self.descending() {
                order.reverse()
            } else {
                order
            }
        });
    }

    fn sort_calls(&self, calls: &mut [HistoryCall]) {
        match self.sort_field.as_deref().unwrap_or("started_at") {
            "display_name" => calls.sort_by_key(|call| call.display_name.to_lowercase()),
            _ => calls.sort_by_key(|call| (call.started_at, call.id.clone())),
        }
        if self.descending() {
            calls.reverse();
        }
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

fn conference_settings(state: &AppState, room_id: &RoomId) -> Option<Value> {
    state.api_v3.conference_settings.lock().ok().and_then(|settings| settings.get(room_id).cloned())
}

fn setting_str(settings: &Option<Value>, key: &str) -> Option<String> {
    settings
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn creator_id(settings: &Option<Value>) -> String {
    setting_str(settings, "creator_id").or_else(|| setting_str(settings, "owner_id")).unwrap_or_default()
}

fn session_calls(
    state: &AppState,
    room: &RoomRecord,
    stopped_at: i64,
    participants: &[crate::hub::ParticipantSnapshot],
) -> Vec<HistoryCall> {
    let attempts = state.api_v3.call_attempts.lock().map(|attempts| attempts.clone()).unwrap_or_default();
    let mut calls = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for ((room_id, uri), attempt) in attempts {
        if room_id != room.id || !seen.insert(attempt.record.id.clone()) {
            continue;
        }
        // Участник мог уйти (BYE/kick) до остановки сессии — это нормальный
        // сценарий; состояние соединения берём из CallAttempt, а live-снимок
        // используется только как более точный источник.
        let live = attempt
            .participant_id
            .as_deref()
            .and_then(|participant_id| participants.iter().find(|p| p.participant_id == participant_id));
        let finished = live
            .map(|_| attempt.completed_at.map(|ts| ts as i64).unwrap_or(stopped_at))
            .or_else(|| attempt.stopped_at.map(|ts| ts as i64))
            .or_else(|| attempt.completed_at.map(|ts| ts as i64))
            .unwrap_or(stopped_at);
        let finish_reason = native_stop_reason(
            attempt.stop_reason.as_deref().or(attempt.failure_reason.as_deref()).unwrap_or("Ended By Remote"),
        );
        // Время вызова — когда адрес создан/начат, а не старт всей сессии.
        let started_at = (attempt.record.created_at as i64).min(finished).max(0);
        let connected_at = live
            .map(|participant| participant.connected_at as i64)
            .or(attempt.connected_at.map(|ts| ts as i64))
            .or(attempt.participant_id.as_ref().map(|_| started_at));
        let connection_type = if attempt.incoming { "INCOMING" } else { "OUTGOING" };
        // Vendor-UI (маппер H) читает abonents[].abonent_id/contact/connection/
        // started_at/stopped_at/stop_reason; без этой формы реальные
        // подключения классифицировались бы как MISSED.
        let display_name = live
            .map(|participant| participant.display_name.clone())
            .unwrap_or_else(|| attempt.record.display_name.clone());
        let abonents: Vec<Value> = attempt
            .participant_id
            .as_deref()
            .filter(|_| connected_at.is_some())
            .map(|participant_id| {
                vec![json!({
                    "abonent_id": participant_id,
                    "contact_id": attempt.record.id,
                    "display_name": display_name,
                    "contact": {"contact_type": "call_strings", "id": attempt.record.id},
                    "connection": {"type_id": connection_type, "connection_url": uri},
                    "started_at": connected_at.unwrap_or(started_at),
                    "stopped_at": finished,
                    "stop_reason": finish_reason,
                    "moved_to": Value::Null,
                })]
            })
            .unwrap_or_default();
        let invitation = json!({
            "display_name": attempt.record.display_name,
            "created_at": started_at,
            "finished_at": finished,
            "connection_url": uri,
            "abonent_id": attempt.participant_id,
            "finish_reason": finish_reason,
            "contact_type": "call_strings",
            "contact_id": attempt.record.id,
            "call_string_id": attempt.record.id,
            "payload": attempt.record.payload,
        });
        calls.push(HistoryCall {
            id: attempt.record.id.clone(),
            started_at,
            deleted_at: Some(stopped_at),
            display_name: attempt.record.display_name.clone(),
            connection_url: uri.clone(),
            payload: attempt.record.payload.clone(),
            contact: Some(json!({
                "id": attempt.record.id,
                "contact_type": "call_strings",
                "display_name": attempt.record.display_name,
                "deleted_at": Value::Null,
                "avatar": Value::Null,
                "address": uri,
            })),
            invitations: vec![invitation],
            abonents,
        });
    }
    calls.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()).then(a.id.cmp(&b.id)));
    calls
}

/// Вызывается ДО очистки `call_attempts`/участников, чтобы снимок был полным.
pub async fn record_stopped(state: &SharedState, room: &RoomRecord, stopped_by: &str, reason: &str) {
    record_stopped_with_settings(state, room, stopped_by, reason, None).await;
}

/// Как [`record_stopped`], но с уже снятым снимком настроек: delete-путь
/// удаляет настройки до записи, поэтому передаёт их явно.
pub async fn record_stopped_with_settings(
    state: &SharedState,
    room: &RoomRecord,
    stopped_by: &str,
    reason: &str,
    settings_override: Option<Value>,
) {
    let started_at = room.created_ms as i64 / 1000;
    let stopped_at = (now_unix_ms() / 1000) as i64;
    let participants = state.room_hub.list_participants(&room.id);
    let calls = session_calls(state, room, stopped_at, &participants);
    // Считаем и тех, кто уже отключился до остановки: иначе сессия с
    // завершившимися звонками показывает 0 участников. Участники, для которых
    // нет CallAttempt (веб/локальные), учитываются отдельно — без двойного счёта.
    let call_participants: std::collections::HashSet<&str> = calls
        .iter()
        .flat_map(|call| call.abonents.iter())
        .filter_map(|abonent| abonent.get("abonent_id").and_then(Value::as_str))
        .collect();
    let local_participants = participants
        .iter()
        .filter(|participant| !call_participants.contains(participant.participant_id.as_str()))
        .count();
    let abonents_count = (call_participants.len() + local_participants).min(u32::MAX as usize) as u32;
    let settings = settings_override.or_else(|| conference_settings(state, &room.id));
    let payload = {
        let stored = settings.clone().unwrap_or_else(|| json!({}));
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
        started_ms: room.created_ms,
        stopped_at,
        stop_reason: reason.to_string(),
        abonents_count,
        creator_id: creator_id(&settings),
        stopped_by: stopped_by.to_string(),
        calls,
        dtmf: setting_str(&settings, "dtmf"),
        route: setting_str(&settings, "route"),
        payload,
    };
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    resources.history.record(session);
    // Durable-маркер на комнате: `clean` стирает историю, но не комнату, поэтому
    // без него удаление старой остановленной конференции воскрешало бы запись.
    let previous_marker = {
        let mut settings = state.api_v3.conference_settings.lock().ok();
        settings.as_mut().and_then(|settings| {
            let entry = settings.entry(room.id).or_insert_with(|| json!({}));
            let marker = entry.get("history_recorded_at").cloned();
            if let Some(object) = entry.as_object_mut() {
                object.insert("history_recorded_at".into(), json!(stopped_at));
            }
            marker
        })
    };
    if let Err(error) = super::resources::persist(state, &resources).await {
        *resources = previous;
        if let Ok(mut settings) = state.api_v3.conference_settings.lock() {
            if let Some(entry) = settings.get_mut(&room.id).and_then(Value::as_object_mut) {
                match previous_marker {
                    Some(marker) => {
                        entry.insert("history_recorded_at".into(), marker);
                    }
                    None => {
                        entry.remove("history_recorded_at");
                    }
                }
            }
        }
        tracing::warn!(error = %error, "history: persist failed");
    }
}

fn nullable_id(id: &str) -> Value {
    if id.is_empty() {
        Value::Null
    } else {
        json!(id)
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
        "stopped_by": nullable_id(&session.stopped_by),
        "creator": creator,
        "stopper": stopper,
        "dtmf": session.dtmf,
        "route": session.route,
        "payload": session.payload,
    })
}

/// Клонирует сессию под коротким lock (404-ветка повторялась в 4 хендлерах).
async fn load_session(state: &AppState, id: &str) -> Option<HistorySession> {
    let resources = state.api_v3.resources.lock().await;
    resources.history.find(id).cloned()
}

/// `GET /api/v3/call_history` → `{count, sessions:[…]}`.
pub async fn list(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(filter): Query<HistoryFilter>,
    Query(page): Query<PageSort>,
) -> Response {
    super::track("call_history");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let (count, page_rows) = {
        let resources = state.api_v3.resources.lock().await;
        let mut rows = filter.filter(&resources.history.sessions);
        page.sort_sessions(&mut rows, "stopped_at");
        let (offset, size) = page.pagination();
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
    let Some(session) = load_session(&state, &id).await else {
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
            "stopped_by": nullable_id(&session.stopped_by),
            "creator": creator,
            "stopper": stopper,
            "started_by": creator,
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
    let Some(session) = load_session(&state, &id).await else {
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
    Query(query): Query<PageSort>,
) -> Response {
    super::track("ui/call_history/{id}");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let Some(session) = load_session(&state, &id).await else {
        return super::error_json(404, "Session not found");
    };
    let mut calls = session.calls;
    query.sort_calls(&mut calls);
    let count = calls.len();
    let (offset, size) = query.pagination();
    let page: Vec<HistoryCall> = calls.into_iter().skip(offset).take(size).collect();
    Json(json!({ "count": count, "call_strings": page })).into_response()
}

/// `GET /api/v3/call_history/export` → `{filename}`; файл отдаётся по
/// `/static/call_history/{filename}` (см. [`static_export`]).
pub async fn export(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(filter): Query<HistoryFilter>,
) -> Response {
    super::track("call_history/export");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let sessions = {
        let resources = state.api_v3.resources.lock().await;
        filter.filter(&resources.history.sessions).into_iter().cloned().collect::<Vec<_>>()
    };
    let mut sessions = sessions;
    sessions.sort_by_key(|session| (session.stopped_at, session.started_at));
    let names = contact_names(&state).await;
    let timezone = current_timezone(&state).await;
    // Случайный токен: имя файла нельзя угадать, даже если роут статики
    // vendor-совместимо отдаёт архивы без заголовка (UI скачивает через <a>).
    let filename = format!(
        "Conference_history_{}_{}.zip",
        format_date_utc((now_unix_ms() / 1000) as i64),
        uuid::Uuid::new_v4().simple()
    );
    let archive = build_history_zip(&filename.replace(".zip", ".csv"), &sessions, &names, &timezone);
    store_export(&state, &filename, archive);
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
    let Some(session) = load_session(&state, &id).await else {
        return super::error_json(404, "Session not found");
    };
    let names = contact_names(&state).await;
    let timezone = current_timezone(&state).await;
    let filename = format!(
        "Conference_history_{}_{}.zip",
        sanitize_filename(&session.display_name),
        uuid::Uuid::new_v4().simple()
    );
    let archive =
        build_history_zip(&filename.replace(".zip", ".csv"), std::slice::from_ref(&session), &names, &timezone);
    store_export(&state, &filename, archive);
    Json(json!({ "filename": filename })).into_response()
}

/// `POST /api/v3/call_history/clean` → `{filename}` backup-архива; история очищена.
pub async fn clean(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("call_history/clean");
    if auth.role < Role::Operator || auth.room_scope.is_some() {
        return super::error_json(403, "Forbidden: Operator role required");
    }
    let names = contact_names(&state).await;
    let timezone = current_timezone(&state).await;
    let mut resources = state.api_v3.resources.lock().await;
    if resources.history.sessions.is_empty() {
        // Очистка пустой истории всё равно инвалидирует ранее выданные файлы.
        if let Ok(mut exports) = state.api_v3.history_exports.lock() {
            exports.clear();
        }
        return Json(json!({ "filename": Value::Null })).into_response();
    }
    let previous = resources.clone();
    let stamp = format_date_utc((now_unix_ms() / 1000) as i64).replace('-', "");
    let filename = format!("Conference_history_backup_{stamp}_{}.zip", uuid::Uuid::new_v4().simple());
    // Архив строится ДО мутации карты выгрузок: неудачный persist не должен
    // инвалидировать ранее выданные ссылки и оставлять «висячий» backup.
    let archive = build_history_zip(&filename.replace(".zip", ".csv"), &previous.history.sessions, &names, &timezone);
    resources.history.sessions.clear();
    if let Err(error) = super::resources::persist(&state, &resources).await {
        *resources = previous;
        drop(resources);
        return super::error_json(500, &format!("Storage error: {error}"));
    }
    drop(resources);
    // Старые выгрузки больше не должны оставаться скачиваемыми после clean.
    if let Ok(mut exports) = state.api_v3.history_exports.lock() {
        exports.clear();
        exports.insert(filename.clone(), archive);
    }
    Json(json!({ "filename": filename })).into_response()
}

/// Кладёт архив в bounded TTL-хранилище (cap 32, 1 час), чтобы память не
/// росла с каждой выгрузкой; вытесняется только самый старый файл.
fn store_export(state: &AppState, filename: &str, archive: Vec<u8>) {
    if let Ok(mut exports) = state.api_v3.history_exports.lock() {
        exports.insert(filename.to_string(), archive);
    }
}

/// `GET /static/call_history/{filename}` — скачивание ZIP (vendor отдаёт без
/// авторизации; имя санитизируется, чужие файлы недоступны).
pub async fn static_export(State(state): State<SharedState>, Path(filename): Path<String>) -> Response {
    if filename.contains('/') || filename.contains("..") {
        return super::error_json(400, "Invalid filename");
    }
    let archive = state.api_v3.history_exports.lock().ok().and_then(|mut files| files.get(&filename));
    match archive {
        Some(bytes) => {
            let mut response =
                (StatusCode::OK, [(header::CONTENT_TYPE, "application/zip")], Body::from(bytes.as_ref().to_vec()))
                    .into_response();
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

// ── Тесты ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "history_tests.rs"]
mod history_tests;

#[cfg(test)]
mod tests {
    use super::export::{build_history_csv, build_history_zip, format_date_utc, format_datetime, format_duration};
    use super::*;

    fn session(name: &str, started: i64, stopped: i64) -> HistorySession {
        HistorySession {
            id: format!("id-{name}"),
            room_id: format!("room-{name}"),
            display_name: name.to_string(),
            started_at: started,
            started_ms: (started.max(0) as u64) * 1000,
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
        let page =
            PageSort { sort_field: Some("started_at".into()), sort_order: Some("1".into()), ..Default::default() };
        let mut rows = HistoryFilter::default().filter(&sessions);
        page.sort_sessions(&mut rows, "stopped_at");
        let names: Vec<&str> = rows.iter().map(|s| s.display_name.as_str()).collect();
        assert_eq!(names, ["Gamma", "beta", "Alpha"]);
        let filter = HistoryFilter { display_name: Some("BET".into()), ..Default::default() };
        assert_eq!(filter.filter(&sessions).len(), 1);
        let page = PageSort { page: Some(2), page_size: Some(1), ..Default::default() };
        assert_eq!(page.pagination(), (1, 1));
        // Приоритет диапазонов зафиксирован: from/to перекрывают date_from/date_to.
        let filter =
            HistoryFilter { from: Some(2), to: Some(30), date_from: Some(1), date_to: Some(1), ..Default::default() };
        assert_eq!(filter.filter(&sessions).len(), 2);
    }

    #[test]
    fn csv_has_vendor_headers_and_escapes_values() {
        let mut evil = session("Name;with\"quotes", 0, 61);
        evil.route = Some("route;1".into());
        evil.dtmf = Some("123456789".into());
        let mut names = std::collections::HashMap::new();
        names.insert("creator".to_string(), "Administrator".to_string());
        names.insert("stopper".to_string(), "Operator".to_string());
        let csv = String::from_utf8(build_history_csv(&[evil], &names, "UTC")).unwrap();
        assert!(csv
            .starts_with("Название;ID;Маршрут;Время начала;Время окончания;Длительность;Участники;Запуск;Остановка\n"));
        assert!(csv.contains("\"Name;with\"\"quotes\""));
        assert!(csv.contains("123456789"), "ID-колонка = DTMF, как в UI: {csv}");
        assert!(csv.contains("Administrator;Operator"), "имена вместо UUID: {csv}");
        assert!(csv.contains("00:01:01"));
        assert!(csv.contains("01.01.1970 00:00:00"));
        // CSV-инъекция нейтрализуется апострофом.
        let mut formula = session("=1+1", 0, 1);
        formula.dtmf = None;
        let csv = String::from_utf8(build_history_csv(&[formula], &names, "UTC")).unwrap();
        assert!(csv.contains("'=1+1"), "{csv}");
    }

    #[test]
    fn zip_is_valid_store_archive_with_single_csv() {
        let names = std::collections::HashMap::new();
        let archive = build_history_zip("Conference_history_2026-01-01.csv", &[session("Alpha", 0, 5)], &names, "UTC");
        assert_eq!(&archive[..4], b"PK\x03\x04");
        let eocd = archive.len() - 22;
        assert_eq!(&archive[eocd..eocd + 4], b"PK\x05\x06");
        let cd_offset = u32::from_le_bytes(archive[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        assert_eq!(&archive[cd_offset..cd_offset + 4], b"PK\x01\x02");
        // Central entry: local-header offset = 0 (иначе unzip: overlapped components).
        let local_offset = u32::from_le_bytes(archive[cd_offset + 42..cd_offset + 46].try_into().unwrap());
        assert_eq!(local_offset, 0);
        let name_len = u16::from_le_bytes(archive[26..28].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(archive[28..30].try_into().unwrap()) as usize;
        let data_start = 30 + name_len + extra_len;
        let data_len = u32::from_le_bytes(archive[18..22].try_into().unwrap()) as usize;
        let local_crc = u32::from_le_bytes(archive[14..18].try_into().unwrap());
        let expected = build_history_csv(&[session("Alpha", 0, 5)], &names, "UTC");
        // Реальное извлечение: CRC и байты CSV совпадают с записанными.
        assert_eq!(&archive[data_start..data_start + data_len], expected.as_slice());
        assert_eq!(local_crc, crate::api_v3::zip_store::crc32(&expected));
        assert_eq!(u32::from_le_bytes(archive[22..26].try_into().unwrap()) as usize, expected.len());
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
    fn history_is_truncated_at_max_sessions() {
        let mut history = HistoryState::default();
        for index in 0..(MAX_SESSIONS + 5) {
            history.record(session(&format!("S{index}"), index as i64 + 1, index as i64 + 2));
        }
        assert_eq!(history.sessions.len(), MAX_SESSIONS);
        // Новые записи в начале, старые вытеснены.
        assert!(history.sessions[0].display_name.starts_with("S5"));
    }

    #[test]
    fn utc_formatting_is_stable() {
        assert_eq!(format_datetime(0), "01.01.1970 00:00:00");
        assert_eq!(format_datetime(1_000_000_000), "09.09.2001 01:46:40");
        assert_eq!(format_duration(3_661), "01:01:01");
        assert_eq!(format_date_utc(1_700_000_000), "2023-11-14");
    }
}
