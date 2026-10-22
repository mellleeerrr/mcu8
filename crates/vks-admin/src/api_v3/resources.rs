//! Persistable PNM conference resources. These are real definitions and links,
//! not canned success responses: creation, attachment, edits and removal share one store.
use std::collections::{BTreeMap, HashMap};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{OriginalUri, Path, Query, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error_json,
    sessions::{check_permission, find_room},
};
use crate::{auth_middleware::AuthContext, state::SharedState};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceState {
    pub layouts: BTreeMap<String, Value>,
    pub audiochannels: BTreeMap<String, Value>,
    pub links: BTreeMap<String, Vec<String>>,
    pub schedules: BTreeMap<String, Value>,
    pub contacts: BTreeMap<String, Vec<Value>>,
    /// Явно выбранная активная раскладка по scope-key
    /// (`sessions:{room}:video_layouts` / `conferences:{room}:video_layouts`).
    /// Порядок `links` остаётся UI-контрактом и не кодирует active.
    #[serde(default)]
    pub active_layouts: BTreeMap<String, String>,
    /// Алиасы COW: `scope-key|old-id -> new-id`, чтобы UI, продолжающий
    /// присылать старый id после клонирования, попадал в клон текущей комнаты.
    #[serde(default)]
    pub layout_aliases: BTreeMap<String, String>,
    /// Глобальная адресная книга (`/api/v3/call_strings`): сохранённые адреса
    /// переживают рестарт и участвуют в автодополнении «Добавить по адресу».
    #[serde(default, deserialize_with = "deserialize_addresses")]
    pub addresses: BTreeMap<String, AddressRecord>,
    /// Справочник (T2.1): профили пользователей, группы, роли и личные
    /// адресные книги. Хранится в том же configuration-document, что и ресурсы.
    #[serde(default)]
    pub directory: super::directory::DirectoryState,
    /// История конференций (T2.2): переживает рестарт в том же документе.
    #[serde(default)]
    pub history: super::history::HistoryState,
    /// Административные настройки (T2.3): vendor-совместимые payload'ы
    /// `settings/*`, `system/ntp`, `system/time-zone`. Хранятся рядом с
    /// ресурсами, чтобы переживать рестарт и попадать в один configuration-document.
    #[serde(default)]
    pub admin_settings: BTreeMap<String, Value>,
}

/// Толерантная загрузка адресной книги: битые/старые записи не роняют весь
/// `ResourceState` (а с ним и layout'ы) — чинятся недостающие поля, отбрасываются
/// записи без `uri` и не-объектные значения протоколов.
fn deserialize_addresses<'de, D>(deserializer: D) -> Result<BTreeMap<String, AddressRecord>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = BTreeMap::<String, Value>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .filter_map(|(id, value)| address_record_from_value(&id, &value).map(|record| (record.id.clone(), record)))
        .collect())
}

fn address_record_from_value(stored_id: &str, value: &Value) -> Option<AddressRecord> {
    let uri = value["uri"].as_str().map(str::trim).filter(|uri| !uri.is_empty())?.to_string();
    let display_name = value["display_name"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uri.clone());
    let mut payload = serde_json::Map::new();
    if let Some(protocols) = value["payload"].as_object() {
        for (protocol, options) in protocols {
            if options.is_object() {
                payload.insert(protocol.clone(), options.clone());
            }
        }
    }
    Some(AddressRecord {
        id: value["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| stored_id.to_string()),
        uri,
        display_name,
        created_at: value["created_at"].as_u64().unwrap_or(0),
        payload: Value::Object(payload),
    })
}

/// Сохранённый адрес: `payload` — плоская карта протоколов (sip/h323/…),
/// как её понимает UI-маппер `lh`; наружу отдаётся вложенной в `call_string`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AddressRecord {
    pub id: String,
    pub uri: String,
    pub display_name: String,
    pub created_at: u64,
    pub payload: Value,
}

pub const CONFIGURATION_KEY: &str = "pnm-definitions-v1";

/// Caller holds the async resources lock, serializing mutations and durable commits.
pub async fn persist(state: &SharedState, resources: &ResourceState) -> Result<(), String> {
    let video_settings = state.api_v3.video_settings.lock().map(|guard| guard.clone()).unwrap_or(Value::Null);
    persist_with_video_settings(state, resources, video_settings).await
}

/// Как [`persist`], но снимок `settings/video` уже снят вызывающим (без повторного lock).
pub async fn persist_with_video_settings(
    state: &SharedState,
    resources: &ResourceState,
    video_settings: Value,
) -> Result<(), String> {
    if let Some(storage) = &state.storage {
        let configurations = state.api_v3.conference_settings.lock().map_err(|e| e.to_string())?.clone();
        let document = json!({
            "version":1,
            "resources":resources,
            "conference_settings":configurations,
            "video_settings":video_settings,
        });
        storage
            .put_configuration_document(CONFIGURATION_KEY, &document.to_string())
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) async fn commit(
    state: &SharedState,
    resources: &mut ResourceState,
    previous: ResourceState,
) -> Result<(), Response> {
    if let Err(error) = persist(state, resources).await {
        *resources = previous;
        return Err(error_json(500, &format!("Storage error: {error}")));
    }
    Ok(())
}

/// Ресурс доступен только через привязанную к комнате ссылку (иначе — 404).
fn attached(
    resources: &ResourceState,
    path: &str,
    room_id: &vks_core::RoomId,
    kind: &str,
    resource_id: &str,
) -> Option<Response> {
    let key = scoped_key(path, &room_id.to_string(), kind);
    if kind == "video_layouts" {
        // После COW UI может прислать старый id: alias ведёт в клон текущего scope.
        let resolved = resolve_layout_alias(resources, &key, resource_id);
        if layout_visible(resources, &room_id.to_string(), &resolved) {
            return None;
        }
        return Some(error_json(404, "Resource is not attached to this conference or session"));
    }
    let attached = resources.links.get(&key).is_some_and(|ids| ids.iter().any(|id| id == resource_id));
    if attached {
        None
    } else {
        Some(error_json(404, "Resource is not attached to this conference or session"))
    }
}

/// Разрешает старый id после COW для read-path (GET/PATCH/abonents/templates).
pub(crate) fn resolve_read_layout_id(
    resources: &ResourceState,
    path: &str,
    room_id: &vks_core::RoomId,
    kind: &str,
    resource_id: &str,
) -> String {
    if kind != "video_layouts" {
        return resource_id.to_string();
    }
    let key = scoped_key(path, &room_id.to_string(), kind);
    let resolved = resolve_layout_alias(resources, &key, resource_id);
    if resolved != resource_id
        && (!resources.layouts.contains_key(&resolved) || !layout_visible(resources, &room_id.to_string(), &resolved))
    {
        // Alias мог остаться после detach клона — не отдаём 404 на старый id,
        // который всё ещё виден через conference-scope.
        return resource_id.to_string();
    }
    resolved
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn scoped_key(path: &str, id: &str, kind: &str) -> String {
    let scope = if path.contains("/conferences/") { "conferences" } else { "sessions" };
    format!("{scope}:{id}:{kind}")
}

fn collection<'a>(resources: &'a ResourceState, kind: &str) -> &'a BTreeMap<String, Value> {
    if kind == "video_layouts" {
        &resources.layouts
    } else {
        &resources.audiochannels
    }
}

fn collection_mut<'a>(resources: &'a mut ResourceState, kind: &str) -> &'a mut BTreeMap<String, Value> {
    if kind == "video_layouts" {
        &mut resources.layouts
    } else {
        &mut resources.audiochannels
    }
}

fn kind(path: &str) -> &'static str {
    if path.contains("/audiochannels") {
        "audiochannels"
    } else {
        "video_layouts"
    }
}

fn singular(kind: &str) -> &'static str {
    if kind == "video_layouts" {
        "video_layout"
    } else {
        "audiochannel"
    }
}

/// `preset_equal_25` → `Some(25)`, `preset_selected_b_4_(2)` → `Some(4)`.
/// Как UI-маппер `Iee`: сначала отрезается суффикс `_(N)`, затем берётся хвост.
pub(crate) fn preset_size(preset: &str) -> Option<u32> {
    let base = preset.rsplit_once("_(").map_or(preset, |(base, _)| base);
    base.rsplit('_').next().and_then(|tail| tail.parse().ok())
}

/// `speakers`/`listeners` в записи — счётчики для таблицы UI; источник истины —
/// массивы в payload (их правят редакторы канала).
fn recalc_audio_counts(record: &mut Value) {
    let payload = &record["payload"];
    if !payload.get("speakers").is_some_and(Value::is_array) && !payload.get("listeners").is_some_and(Value::is_array) {
        return;
    }
    let (speakers, listeners) =
        (payload["speakers"].as_array().map_or(0, Vec::len), payload["listeners"].as_array().map_or(0, Vec::len));
    record["speakers"] = json!(speakers);
    record["listeners"] = json!(listeners);
}

fn default_audio_payload() -> Value {
    json!({"type":"default", "vad":true, "autofill_speakers":true, "autofill_listeners":true,
        "speakers":[], "listeners":[], "blacklist":[], "whitelist":[], "attachmode":"BLACKLIST"})
}

/// Гигиена названия раскладки: без emoji/пиктограмм/управляющих символов,
/// одиночные пробелы, максимум 64 символа. UI рисует это имя по центру ячейки.
fn sanitize_layout_label(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|ch| ch.is_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '.' | '(' | ')' | '/' | '+' | '#'))
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(64).collect()
}

fn sanitize_layout_record(record: &mut Value) {
    if let Some(name) = record["display_name"].as_str() {
        record["display_name"] = json!(sanitize_layout_label(name));
    }
}

fn normalize_resource(kind: &str, body: &Value, creator: &str) -> Value {
    let mut payload = if kind == "video_layouts" { canonical_default_payload() } else { default_audio_payload() };
    if let Some(values) = body.get("payload").and_then(Value::as_object) {
        merge_json(&mut payload, &Value::Object(values.clone()));
    }
    let display_name = if kind == "video_layouts" {
        json!(sanitize_layout_label(body.get("display_name").and_then(Value::as_str).unwrap_or_default()))
    } else {
        body.get("display_name").cloned().unwrap_or(Value::Null)
    };
    let mut result = json!({"id":Uuid::new_v4().to_string(), "created_at":now(), "creator_id":creator,
        "display_name":display_name, "payload":payload,
        "defaults":{"deletable":true,"editable":true}, "speakers":0,"listeners":0});
    if kind == "video_layouts" {
        // `my` зовёт `Mee(record.abonents)`, а `Mee` читает `abonents.abonents.map`:
        // объект обязателен (null → `entity.abonents.common` падает у individual-строк).
        result["abonents"] = layout_record_abonents(Vec::new());
        result["contacts"] = json!({"count":0,"contacts":[]});
        recalc_layout_size(&mut result);
    } else {
        recalc_audio_counts(&mut result);
    }
    result
}

/// PNM editors send partial nested payloads; changing one option must keep siblings.
pub(crate) fn merge_json(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            match target.get_mut(key) {
                Some(existing) if existing.is_object() && value.is_object() => merge_json(existing, value),
                _ => {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *target = patch.clone();
    }
}

fn gateway_protocol(participant_id: &str) -> &'static str {
    if participant_id.contains("@sip-gateway") {
        "sip"
    } else if participant_id.contains("@h323-gateway") {
        "h323"
    } else if participant_id.contains("@rtsp-gateway") {
        "rtsp"
    } else {
        "webrtc"
    }
}

fn gateway_url(participant_id: &str, display_name: &str) -> String {
    match gateway_protocol(participant_id) {
        "sip" => format!("sip:{display_name}"),
        "h323" => format!("h323:{display_name}"),
        _ => format!("webrtc:{participant_id}"),
    }
}

/// `abonents` записи раскладки в форме, которую ждёт `my → Mee`:
/// `{abonents:{count,abonents:[...]}, records:[], rtmp:[]}`; плоский `{count, abonents}`
/// роняет `.map` в Mee (`{abonents:{abonents:t}}`).
fn layout_record_abonents(abonents: Vec<Value>) -> Value {
    json!({
        "abonents": {"count": abonents.len(), "abonents": abonents},
        "records": [],
        "rtmp": [],
    })
}

fn participant_abonent_json(participant: &crate::hub::ParticipantSnapshot) -> Value {
    json!({
        "abonent_id": participant.participant_id,
        "connection_id": participant.participant_id,
        "connection_url": gateway_url(&participant.participant_id, &participant.display_name),
        "contact_id": participant.participant_id,
        "contact_type": gateway_protocol(&participant.participant_id),
        "display_name": participant.display_name,
    })
}

/// Кто реально стоит в раскладке: manual — занятые слоты, auto — все участники комнаты.
fn resolved_layout_abonents(state: &SharedState, room_id: &vks_core::RoomId, layout: &Value) -> Vec<Value> {
    let participants = state.room_hub.list_participants(room_id);
    let individual = layout["payload"]["type"].as_str() == Some("individual");
    if !individual && layout["payload"]["mode"].as_str() != Some("manual") {
        return participants.iter().map(participant_abonent_json).collect();
    }
    let live: HashMap<&str, &crate::hub::ParticipantSnapshot> =
        participants.iter().map(|p| (p.participant_id.as_str(), p)).collect();
    let mut abonents = Vec::new();
    for slot in layout["payload"]["slots"].as_array().into_iter().flatten() {
        let Some(slot) = slot.as_object() else { continue };
        let abonent_id = slot.get("abonent_id").and_then(Value::as_str).unwrap_or_default();
        let stored_name = slot.get("display_name").and_then(Value::as_str).unwrap_or_default();
        let participant = live.get(abonent_id).copied();
        if participant.is_none() && stored_name.is_empty() {
            // Пустая ячейка сетки (`cs-<uuid>` плейсхолдер) — не «стоящий в раскладке».
            continue;
        }
        let mut entry = participant.map_or_else(
            || {
                json!({
                    "abonent_id": slot.get("abonent_id").cloned().unwrap_or(Value::Null),
                    "connection_id": slot.get("connection_id").cloned().unwrap_or(Value::Null),
                    "connection_url": Value::Null,
                    "display_name": stored_name,
                })
            },
            participant_abonent_json,
        );
        entry["contact_id"] = slot.get("contact_id").cloned().unwrap_or(Value::Null);
        entry["contact_type"] = slot.get("contact_type").cloned().unwrap_or(json!("call_strings"));
        entry["position"] = slot.get("position").cloned().unwrap_or(Value::Null);
        entry["role"] = slot.get("role").cloned().unwrap_or(json!("AUTO"));
        abonents.push(entry);
    }
    abonents
}

pub(crate) fn with_layout_abonents(state: &SharedState, record: &mut Value, room_id: &vks_core::RoomId) {
    let abonents = resolved_layout_abonents(state, room_id, record);
    record["abonents"] = layout_record_abonents(abonents);
}

fn layout_contains_abonent(layout: &Value, abonent_id: &str) -> bool {
    layout["payload"]["slots"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|slot| slot["abonent_id"].as_str() == Some(abonent_id))
}

/// Канонический payload «Общей» раскладки: ровно то, что создаёт
/// `normalize_resource` для `type=default`. Legacy `type=default` с ручными
/// слотами/пресетом каноническим не считается и не переиспользуется.
pub(crate) fn canonical_default_payload() -> Value {
    json!({"mode":"auto", "preset":"preset_default_13", "type":"default", "slots":[],
        "size":13, "autoscroll":0})
}

fn is_canonical_default(layout: &Value) -> bool {
    layout["payload"] == canonical_default_payload()
}

pub(crate) fn resolve_layout_alias(resources: &ResourceState, scope_key: &str, id: &str) -> String {
    let mut current = id.to_string();
    for _ in 0..8 {
        let alias_key = format!("{scope_key}|{current}");
        let Some(next) = resources.layout_aliases.get(&alias_key) else { break };
        if *next == current {
            break;
        }
        current = next.clone();
    }
    current
}

fn scope_links(resources: &ResourceState, scope_key: &str, room_id: &str) -> Vec<String> {
    let mut ids = resources.links.get(scope_key).cloned().unwrap_or_default();
    if scope_key.starts_with("sessions:") {
        let inherited = format!("conferences:{room_id}:video_layouts");
        for id in resources.links.get(&inherited).cloned().unwrap_or_default() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// Layout виден комнате: привязан к её scope напрямую или унаследован из
/// конференции. Учитывает COW-алиасы текущего scope.
pub(crate) fn layout_visible(resources: &ResourceState, room_id: &str, id: &str) -> bool {
    for scope in ["sessions", "conferences"] {
        let key = format!("{scope}:{room_id}:video_layouts");
        let resolved = resolve_layout_alias(resources, &key, id);
        if resources.links.get(&key).is_some_and(|ids| ids.iter().any(|value| value == id || value == &resolved)) {
            return true;
        }
    }
    false
}

pub(crate) fn active_id_for(resources: &ResourceState, room_id: &str) -> Option<String> {
    for scope in ["sessions", "conferences"] {
        let key = format!("{scope}:{room_id}:video_layouts");
        if let Some(id) = resources.active_layouts.get(&key) {
            if resources.layouts.contains_key(id) && layout_visible(resources, room_id, id) {
                return Some(id.clone());
            }
        }
    }
    None
}

fn fallback_layout_for_scope(resources: &ResourceState, scope_key: &str, room_id: &str) -> Option<String> {
    let ids = scope_links(resources, scope_key, room_id);
    if let Some(id) = ids.iter().find(|id| resources.layouts.get(*id).is_some_and(is_canonical_default)) {
        return Some(id.clone());
    }
    if let Some(id) = ids.iter().rev().find(|id| {
        resources.layouts.get(*id).is_some_and(|layout| layout["payload"]["type"].as_str() == Some("default"))
    }) {
        return Some(id.clone());
    }
    ids.last().cloned()
}

pub(crate) fn set_active(resources: &mut ResourceState, scope_key: &str, id: &str) {
    resources.active_layouts.insert(scope_key.to_string(), id.to_string());
}

/// Вставляет draft как COW-клон и переключает ссылку текущего scope.
/// Возвращает id клона.
pub(crate) fn cow_replace(resources: &mut ResourceState, scope_key: &str, source_id: &str, mut draft: Value) -> String {
    let new_id = Uuid::new_v4().to_string();
    draft["id"] = json!(new_id);
    draft["created_at"] = json!(now());
    resources.layouts.insert(new_id.clone(), draft);
    if let Some(ids) = resources.links.get_mut(scope_key) {
        let mut replaced = false;
        for id in ids.iter_mut() {
            if *id == source_id {
                *id = new_id.clone();
                replaced = true;
            }
        }
        if !replaced {
            ids.push(new_id.clone());
        }
    } else {
        resources.links.insert(scope_key.to_string(), vec![new_id.clone()]);
    }
    resources.layout_aliases.insert(format!("{scope_key}|{source_id}"), new_id.clone());
    // COW на конференции при живой сессии: mirror-ссылка сессии должна
    // перейти на клон, иначе сессия держит stale layout и в списке дубль.
    let parts: Vec<&str> = scope_key.split(':').collect();
    if parts.len() == 3 && parts[0] == "conferences" && parts[2] == "video_layouts" {
        let mirror = format!("sessions:{}:video_layouts", parts[1]);
        if let Some(mirror_ids) = resources.links.get_mut(&mirror) {
            if mirror_ids.iter().any(|id| id == source_id) {
                for id in mirror_ids.iter_mut() {
                    if id == source_id {
                        *id = new_id.clone();
                    }
                }
                resources.layout_aliases.insert(format!("{mirror}|{source_id}"), new_id.clone());
                if resources.active_layouts.get(&mirror).is_some_and(|id| id == source_id) {
                    resources.active_layouts.insert(mirror, new_id.clone());
                }
            }
        }
    }
    let active_was_source = resources.active_layouts.get(scope_key).is_some_and(|id| id == source_id);
    if active_was_source || !resources.active_layouts.contains_key(scope_key) {
        resources.active_layouts.insert(scope_key.to_string(), new_id.clone());
    }
    new_id
}

/// Единственная каноническая «Общая» в документе; создаёт, если её нет.
fn find_canonical_layout_id(resources: &ResourceState) -> Option<String> {
    resources
        .layouts
        .values()
        .find(|layout| is_canonical_default(layout))
        .and_then(|layout| layout["id"].as_str().map(str::to_string))
}

fn canonical_layout_id(resources: &mut ResourceState) -> String {
    if let Some(id) = find_canonical_layout_id(resources) {
        return id;
    }
    let record = normalize_resource("video_layouts", &json!({"payload":canonical_default_payload()}), "system");
    let id = record["id"].as_str().unwrap_or_default().to_string();
    resources.layouts.insert(id.clone(), record);
    id
}

/// Гарантирует, что scope имеет каноническую «Общую» раскладку. Возвращает id.
fn ensure_canonical_default(resources: &mut ResourceState, scope_key: &str, room_id: &str) -> String {
    if let Some(id) = scope_links(resources, scope_key, room_id)
        .into_iter()
        .find(|id| resources.layouts.get(id).is_some_and(is_canonical_default))
    {
        if !resources.links.get(scope_key).is_some_and(|ids| ids.contains(&id)) {
            resources.links.entry(scope_key.to_string()).or_default().push(id.clone());
        }
        return id;
    }
    let id = canonical_layout_id(resources);
    resources.links.entry(scope_key.to_string()).or_default().push(id.clone());
    id
}

/// Чинит active только если он отсутствует/невалиден. Единая точка вместо
/// повторяющихся `if valid { ... }` в detach/remove_global.
fn repair_active_layout(resources: &mut ResourceState, scope_key: &str, room_id: &str) {
    let valid = resources.active_layouts.get(scope_key).is_some_and(|active| {
        resources.layouts.contains_key(active) && resources.links.get(scope_key).is_some_and(|ids| ids.contains(active))
    });
    if valid {
        return;
    }
    let active = fallback_layout_for_scope(resources, scope_key, room_id)
        .unwrap_or_else(|| ensure_canonical_default(resources, scope_key, room_id));
    set_active(resources, scope_key, &active);
}

/// «Общая» раскладка комнаты (canonical → legacy default → первый link).
fn default_layout_id(resources: &ResourceState, room_id: &vks_core::RoomId) -> Option<String> {
    for scope in ["sessions", "conferences"] {
        let key = format!("{scope}:{room_id}:video_layouts");
        if let Some(id) = resources
            .links
            .get(&key)
            .into_iter()
            .flatten()
            .find(|id| resources.layouts.get(*id).is_some_and(is_canonical_default))
        {
            return Some(id.clone());
        }
    }
    for scope in ["sessions", "conferences"] {
        let key = format!("{scope}:{room_id}:video_layouts");
        if let Some(id) = resources.links.get(&key).into_iter().flatten().find(|id| {
            resources.layouts.get(*id).is_some_and(|layout| layout["payload"]["type"].as_str() == Some("default"))
        }) {
            return Some(id.clone());
        }
    }
    for scope in ["sessions", "conferences"] {
        let key = format!("{scope}:{room_id}:video_layouts");
        if let Some(id) = resources.links.get(&key).and_then(|ids| ids.first()) {
            return Some(id.clone());
        }
    }
    None
}

/// Individual-раскладка участника (иначе `hasIndividualLayout` в UI всегда false).
pub(crate) fn participant_layout_id(
    resources: &ResourceState,
    room_id: &vks_core::RoomId,
    abonent_id: &str,
) -> Option<String> {
    for scope in ["sessions", "conferences"] {
        let Some(ids) = resources.links.get(&format!("{scope}:{room_id}:video_layouts")) else { continue };
        for id in ids {
            let Some(layout) = resources.layouts.get(id) else { continue };
            if layout["payload"]["type"].as_str() == Some("individual") && layout_contains_abonent(layout, abonent_id) {
                return Some(id.clone());
            }
        }
    }
    default_layout_id(resources, room_id)
}

/// `GET …/video_layouts/{resource_id}/abonents` — кто стоит в раскладке.
pub async fn layout_abonents(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path((id, resource_id)): Path<(String, String)>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let layout = {
        let resources = state.api_v3.resources.lock().await;
        let resource_id = resolve_read_layout_id(&resources, uri.path(), &room.id, "video_layouts", &resource_id);
        if let Some(response) = attached(&resources, uri.path(), &room.id, "video_layouts", &resource_id) {
            return response;
        }
        match resources.layouts.get(&resource_id) {
            Some(layout) => layout.clone(),
            None => return error_json(404, "Resource not found"),
        }
    };
    let abonents = resolved_layout_abonents(&state, &room.id, &layout);
    Json(json!({ "abonents": abonents, "count": abonents.len() })).into_response()
}

/// `POST …/video_layouts/{resource_id}/abonents/{abonent_id}` — посадить абонента в слот.
pub async fn attach_layout_abonent(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path((id, resource_id, abonent_id)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let Some(participant) = state
        .room_hub
        .list_participants(&room.id)
        .into_iter()
        .find(|participant| participant.participant_id == abonent_id)
    else {
        return error_json(404, "Abonent is not connected to this session");
    };
    let mut resources = state.api_v3.resources.lock().await;
    let scope_key = scoped_key(uri.path(), &room.id.to_string(), "video_layouts");
    let resource_id = resolve_layout_alias(&resources, &scope_key, &resource_id);
    if !layout_visible(&resources, &room.id.to_string(), &resource_id) {
        return error_json(404, "Resource is not attached to this conference or session");
    }
    let Some(source) = resources.layouts.get(&resource_id).cloned() else {
        return error_json(404, "Resource not found");
    };
    if !source["payload"].is_object() {
        return error_json(422, "Layout payload is malformed");
    }
    let previous = resources.clone();
    let mut draft = source;
    let limit = draft["payload"]["size"].as_u64().filter(|size| *size > 0).unwrap_or(64);
    let Some(slots) = draft["payload"]["slots"].as_array_mut() else {
        return error_json(409, "Layout has no video windows");
    };
    if slots.iter().any(|slot| !slot.is_object()) {
        return error_json(422, "Layout slots are malformed");
    }
    let connection_id = body.get("connection_id").cloned().unwrap_or_else(|| json!(abonent_id));
    let contact_id = body.get("contact_id").cloned().unwrap_or(Value::Null);
    let contact_type = body.get("contact_type").cloned().unwrap_or_else(|| json!("call_strings"));
    if let Some(slot) = slots.iter_mut().find(|slot| slot["abonent_id"].as_str() == Some(abonent_id.as_str())) {
        slot["connection_id"] = connection_id;
        slot["contact_id"] = contact_id;
        slot["contact_type"] = contact_type;
        slot["display_name"] = json!(participant.display_name);
    } else {
        let occupied =
            slots.iter().filter(|slot| slot["abonent_id"].as_str().is_some_and(|id| !id.is_empty())).count() as u64;
        // Сетка хранит и свободные окна (role=AUTO): занимаем первое из них, позицию не меняем.
        let free = slots.iter().position(|slot| slot["abonent_id"].as_str().is_none_or(str::is_empty));
        if let Some(index) = free {
            let position = slots[index]["position"].clone();
            slots[index] = json!({
                "position": position,
                "role": "PEER",
                "abonent_id": abonent_id,
                "connection_id": connection_id,
                "contact_id": contact_id,
                "contact_type": contact_type,
                "call_string_id": contact_id.clone(),
                "display_name": participant.display_name,
            });
        } else {
            if occupied >= limit {
                return error_json(409, "Layout is full");
            }
            let position = slots.iter().filter_map(|slot| slot["position"].as_u64()).max().unwrap_or(0) + 1;
            slots.push(json!({
                "position": position,
                "role": "PEER",
                "abonent_id": abonent_id,
                "connection_id": connection_id,
                "contact_id": contact_id,
                "contact_type": contact_type,
                "call_string_id": contact_id.clone(),
                "display_name": participant.display_name,
            }));
        }
    }
    let owners = resources.links.values().filter(|ids| ids.iter().any(|value| value == &resource_id)).count();
    let direct = resources.links.get(&scope_key).is_some_and(|ids| ids.iter().any(|value| value == &resource_id));
    let target_id = if direct && owners <= 1 {
        resources.layouts.insert(resource_id.clone(), draft);
        resource_id.clone()
    } else {
        cow_replace(&mut resources, &scope_key, &resource_id, draft)
    };
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    // Живое изменение раскладки должно доехать до MCU без перезапуска сессии.
    drop(resources);
    super::sessions::push_layout_to_engine(&state, room.id).await;
    Json(json!({ "status": "ok", "layout_id": target_id })).into_response()
}

/// `DELETE …/video_layouts/{resource_id}/abonents/{abonent_id}` — убрать из раскладки.
pub async fn detach_layout_abonent(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path((id, resource_id, abonent_id)): Path<(String, String, String)>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let mut resources = state.api_v3.resources.lock().await;
    let scope_key = scoped_key(uri.path(), &room.id.to_string(), "video_layouts");
    let resource_id = resolve_layout_alias(&resources, &scope_key, &resource_id);
    if !layout_visible(&resources, &room.id.to_string(), &resource_id) {
        return error_json(404, "Resource is not attached to this conference or session");
    }
    let Some(source) = resources.layouts.get(&resource_id).cloned() else {
        return error_json(404, "Resource not found");
    };
    if !source["payload"].is_object() {
        return error_json(422, "Layout payload is malformed");
    }
    let previous = resources.clone();
    let mut draft = source;
    let Some(slots) = draft["payload"]["slots"].as_array_mut() else {
        return error_json(409, "Layout has no video windows");
    };
    // Окно не удаляем: размер/геометрия сетки стабильны, освободившееся окно — AUTO.
    if let Some(slot) =
        slots.iter_mut().find(|slot| slot.is_object() && slot["abonent_id"].as_str() == Some(abonent_id.as_str()))
    {
        let position = slot["position"].clone();
        *slot = json!({"position": position, "role": "AUTO"});
    }
    let owners = resources.links.values().filter(|ids| ids.iter().any(|value| value == &resource_id)).count();
    let direct = resources.links.get(&scope_key).is_some_and(|ids| ids.iter().any(|value| value == &resource_id));
    let target_id = if direct && owners <= 1 {
        resources.layouts.insert(resource_id.clone(), draft);
        resource_id.clone()
    } else {
        cow_replace(&mut resources, &scope_key, &resource_id, draft)
    };
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    // Живое изменение раскладки должно доехать до MCU без перезапуска сессии.
    drop(resources);
    super::sessions::push_layout_to_engine(&state, room.id).await;
    {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Ok(value) = axum::http::HeaderValue::from_str(&target_id) {
            response.headers_mut().insert("x-vks-layout-id", value);
        }
        response
    }
}

/// Миграция сохранённых записей: ранние версии держали в раскладке объект
/// `abonents` (в т.ч. с лишним уровнем вложенности), битые `payload`/`slots`
/// и висячие ссылки. Иначе `my → Mee` падает на `.map`, а мутации — на IndexMut.
/// Непривязанные раскладки НЕ удаляются: UI создаёт их до attach, рестарт между
/// этими шагами не должен терять данные.
/// T01-семантика для документа без active: последняя non-default раскладка,
/// иначе canonical/default. Используется при миграции старой БД.
fn legacy_effective_layout(resources: &ResourceState, key: &str) -> Option<String> {
    let ids = resources.links.get(key)?;
    ids.iter()
        .rev()
        .find(|id| {
            resources.layouts.get(*id).is_some_and(|layout| layout["payload"]["type"].as_str() != Some("default"))
        })
        .or_else(|| ids.iter().find(|id| resources.layouts.get(*id).is_some_and(is_canonical_default)))
        .or_else(|| {
            ids.iter().rev().find(|id| {
                resources.layouts.get(*id).is_some_and(|layout| layout["payload"]["type"].as_str() == Some("default"))
            })
        })
        .or_else(|| ids.first())
        .cloned()
}

pub fn normalize_loaded(resources: &mut ResourceState) {
    resources.layouts.retain(|_, layout| {
        if !layout.is_object() {
            return false;
        }
        if !layout["payload"].is_object() {
            layout["payload"] = json!({
                "mode":"auto","preset":"preset_default_13","type":"default","slots":[],
                "size":13,"autoscroll":0
            });
        }
        let slots = layout["payload"]["slots"].as_array().cloned().unwrap_or_default();
        layout["payload"]["slots"] = json!(slots.into_iter().filter(Value::is_object).collect::<Vec<_>>());
        layout["abonents"] = layout_record_abonents(Vec::new());
        if !layout.get("contacts").is_some_and(Value::is_object) {
            layout["contacts"] = json!({"count": 0, "contacts": []});
        }
        true
    });
    resources.audiochannels.retain(|_, channel| {
        if !channel.is_object() {
            return false;
        }
        if !channel["payload"].is_object() {
            channel["payload"] = default_audio_payload();
        }
        recalc_audio_counts(channel);
        true
    });
    resources.links.retain(|_, ids| {
        // Привязки живут и для раскладок, и для аудиоканалов; висячие id чистим.
        ids.retain(|id| resources.layouts.contains_key(id) || resources.audiochannels.contains_key(id));
        !ids.is_empty()
    });
    let layout_ids: std::collections::HashSet<String> = resources.layouts.keys().cloned().collect();
    resources.active_layouts.retain(|key, id| layout_ids.contains(id) && resources.links.contains_key(key));
    resources.layout_aliases.retain(|key, target| {
        layout_ids.contains(target) && key.split('|').next().is_some_and(|scope| resources.links.contains_key(scope))
    });
    // Для существующего документа выводим active из links (не пишем до первого commit).
    let keys: Vec<String> = resources.links.keys().filter(|key| key.ends_with(":video_layouts")).cloned().collect();
    for key in keys {
        let valid = resources.active_layouts.get(&key).is_some_and(|active| {
            resources.links.get(&key).is_some_and(|ids| ids.contains(active)) && layout_ids.contains(active)
        });
        if valid {
            continue;
        }
        if let Some(fallback) = legacy_effective_layout(resources, &key) {
            resources.active_layouts.insert(key, fallback);
        }
    }
    dedup_addresses(resources);
}

/// Схлопывает глобальные адреса с одинаковым canonical URI, сохраняя самый
/// ранний id: документ мог накопить дубли до появления canonical-idempotent
/// create. Личные ссылки переписываются на оставшийся id, payload'ы
/// объединяются, пустое имя заменяется непустым. Это дешёвая durable-защита
/// на старте; API-путь дополнительно сериализован мьютексом и canonical-create.
fn dedup_addresses(resources: &mut ResourceState) {
    let mut keep_by_uri: HashMap<String, String> = HashMap::new();
    let mut aliases: Vec<(String, String)> = Vec::new();
    let mut ids: Vec<String> = resources.addresses.keys().cloned().collect();
    // Ранний created_at важнее порядка id: именно его id остаётся стабильным
    // для ссылок из личных книг.
    ids.sort_by_key(|id| {
        resources
            .addresses
            .get(id)
            .map(|record| (record.created_at, record.id.clone()))
            .unwrap_or((u64::MAX, id.clone()))
    });
    for id in ids {
        let Some(key) = resources.addresses.get(&id).map(|record| vks_common::uri::canonical_uri(&record.uri)) else {
            continue;
        };
        match keep_by_uri.get(&key).cloned() {
            Some(keep) => aliases.push((id, keep)),
            None => {
                keep_by_uri.insert(key, id);
            }
        }
    }
    for (duplicate_id, keep_id) in aliases {
        let Some(duplicate) = resources.addresses.remove(&duplicate_id) else {
            continue;
        };
        let Some(keep) = resources.addresses.get_mut(&keep_id) else {
            continue;
        };
        keep.created_at = keep.created_at.min(duplicate.created_at);
        if keep.display_name.trim().is_empty() {
            keep.display_name = duplicate.display_name;
        }
        merge_json(&mut keep.payload, &duplicate.payload);
        for addresses in resources.directory.user_addresses.values_mut() {
            if addresses.remove(&duplicate_id).is_some() {
                addresses.insert(keep_id.clone(), keep.clone());
            }
        }
    }
}

/// Удаление комнаты: убираются только её scoped-ссылки и ставшие недостижимыми
/// ресурсы. Глобальные (ещё не привязанные) раскладки не трогаем — их бережёт
/// `normalize_loaded`, UI создаёт их до attach.
pub async fn purge_room_resources(state: &SharedState, room_id: &str) {
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    let marker = format!(":{room_id}:");
    let removed: Vec<String> =
        resources.links.iter().filter(|(key, _)| key.contains(&marker)).flat_map(|(_, ids)| ids.clone()).collect();
    resources.links.retain(|key, _| !key.contains(&marker));
    resources.active_layouts.retain(|key, _| !key.contains(&marker));
    resources.layout_aliases.retain(|key, _| !key.contains(&marker));
    resources.contacts.remove(room_id);
    if let Ok(room) = room_id.parse::<vks_core::RoomId>() {
        if let Ok(mut pushes) = state.api_v3.layout_pushes.lock() {
            pushes.remove(&room);
        }
    }
    let referenced: std::collections::HashSet<String> = resources.links.values().flatten().cloned().collect();
    for id in removed {
        if !referenced.contains(&id) {
            resources.layouts.remove(&id);
            resources.audiochannels.remove(&id);
        }
    }
    if commit(state, &mut resources, previous).await.is_err() {
        tracing::warn!(%room_id, "failed to persist resource cleanup after room deletion");
    }
}

/// Сессия наследует ресурсы конференции, затем получает «Общую» (default),
/// если её нет в итоговом списке: без неё UI падает на `…default.some(...)`.
/// Idempotent, вызывать под локом.
pub fn inherit_and_ensure_default_layout(resources: &mut ResourceState, room_id: &str, _creator: &str) {
    let inherit_key = format!("conferences:{room_id}:video_layouts");
    let key = format!("sessions:{room_id}:video_layouts");
    let conference_ids = resources.links.get(&inherit_key).cloned().unwrap_or_default();
    // T01-гарантия: выбранная на конференции раскладка должна доехать до
    // первого моста сессии. Session-active имеет приоритет, но при его
    // отсутствии наследуем active конференции, а не canonical default.
    let conference_active = resources
        .active_layouts
        .get(&inherit_key)
        .cloned()
        .filter(|id| conference_ids.contains(id) && resources.layouts.contains_key(id));
    let session_ids = resources.links.entry(key.clone()).or_default();
    for id in conference_ids {
        // Рестарт конференции не должен терять индивидуальные раскладки прошлого запуска.
        if !session_ids.contains(&id) && resources.layouts.contains_key(&id) {
            session_ids.push(id);
        }
    }
    let has_default = resources
        .links
        .get(&key)
        .is_some_and(|ids| ids.iter().any(|id| resources.layouts.get(id).is_some_and(is_canonical_default)));
    if has_default {
        let canonical = resources.links[&key]
            .iter()
            .find(|id| resources.layouts.get(*id).is_some_and(is_canonical_default))
            .cloned();
        if let Some(id) = canonical {
            let valid = resources.active_layouts.get(&key).is_some_and(|active| {
                resources.layouts.contains_key(active)
                    && resources.links.get(&key).is_some_and(|ids| ids.contains(active))
            });
            if !valid {
                let fallback = conference_active.clone().unwrap_or(id);
                set_active(resources, &key, &fallback);
            }
        }
        return;
    }
    // Переиспользуем единственную каноническую «Общую» вместо создания копии
    // на каждую комнату/сессию.
    let id = canonical_layout_id(resources);
    resources.links.entry(key.clone()).or_default().push(id.clone());
    let valid = resources
        .active_layouts
        .get(&key)
        .is_some_and(|active| resources.links.get(&key).is_some_and(|ids| ids.contains(active)));
    if !valid {
        let fallback = conference_active.unwrap_or(id);
        set_active(resources, &key, &fallback);
    }
}

fn validate_layout_payload(payload: &Value) -> Result<(), String> {
    if let Some(slots) = payload.get("slots") {
        let Some(slots) = slots.as_array() else {
            return Err("payload.slots must be an array".to_string());
        };
        if slots.iter().any(|slot| !slot.is_object()) {
            return Err("payload.slots items must be objects".to_string());
        }
    }
    Ok(())
}

fn recalc_layout_size(record: &mut Value) {
    let Some(payload) = record["payload"].as_object() else { return };
    let size = payload
        .get("slots")
        .and_then(Value::as_array)
        .map(Vec::len)
        .filter(|count| *count > 0)
        .or_else(|| payload.get("preset").and_then(Value::as_str).and_then(preset_size).map(|n| n as usize));
    if let Some(size) = size {
        record["payload"]["size"] = json!(size);
    }
}

/// UI присылает слоты без `display_name`/`connection_url`: переносим метаданные
/// по `abonent_id`/`call_string_id`, иначе PATCH стирает имена получателей.
fn merge_layout_slots(previous: &[Value], incoming: &[Value]) -> Vec<Value> {
    incoming
        .iter()
        .map(|slot| {
            if !slot.is_object() {
                return slot.clone();
            }
            let key = slot
                .get("abonent_id")
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty())
                .or_else(|| slot.get("call_string_id").and_then(Value::as_str).filter(|key| !key.is_empty()));
            let mut entry = key
                .and_then(|key| {
                    previous.iter().find(|old| {
                        old.get("abonent_id").and_then(Value::as_str) == Some(key)
                            || old.get("call_string_id").and_then(Value::as_str) == Some(key)
                    })
                })
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let (Some(target), Some(patch)) = (entry.as_object_mut(), slot.as_object()) {
                for (name, value) in patch {
                    target.insert(name.clone(), value.clone());
                }
            }
            entry
        })
        .collect()
}

fn update_resource(record: &mut Value, body: &Value) -> Result<(), String> {
    if let Some(name) = body.get("display_name") {
        record["display_name"] = name.clone();
    }
    if let Some(payload) = body.get("payload") {
        if !payload.is_object() {
            return Err("payload must be an object".to_string());
        }
        validate_layout_payload(payload)?;
        let previous_slots = record["payload"]["slots"].as_array().cloned().unwrap_or_default();
        merge_json(&mut record["payload"], payload);
        if let Some(incoming) = payload["slots"].as_array() {
            record["payload"]["slots"] = json!(merge_layout_slots(&previous_slots, incoming));
        }
    }
    if record["id"].is_string() && record["payload"].is_object() {
        recalc_layout_size(record);
        recalc_audio_counts(record);
    }
    Ok(())
}

pub async fn create(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Json(body): Json<Value>,
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let kind = kind(uri.path());
    if kind == "video_layouts" {
        let payload = body.get("payload").cloned().unwrap_or(json!({}));
        if !payload.is_object() {
            return error_json(422, "payload must be an object");
        }
        if let Err(message) = validate_layout_payload(&payload) {
            return error_json(422, &message);
        }
    }
    let record = normalize_resource(kind, &body, &auth.subject);
    let id = record["id"].as_str().unwrap().to_string();
    let mut resources = state.api_v3.resources.lock().await;
    // Каноническая «Общая» — одна запись на документ. UI создаёт её перед
    // attach и после рестарта; без reuse БД плодит default-раскладки.
    if kind == "video_layouts"
        && is_canonical_default(&record)
        && record["display_name"].as_str().unwrap_or_default().is_empty()
    {
        // Только поиск: find не мутирует документ. Раньше canonical_layout_id
        // вставлял запись и возвращал 201 до commit — на свежей БД «Общая»
        // терялась после рестарта (F1).
        if let Some(id) = find_canonical_layout_id(&resources) {
            if let Some(existing) = resources.layouts.get(&id).cloned() {
                return (StatusCode::CREATED, Json(json!({singular(kind):existing}))).into_response();
            }
        }
    }
    let previous = resources.clone();
    collection_mut(&mut resources, kind).insert(id, record.clone());
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    (StatusCode::CREATED, Json(json!({singular(kind):record}))).into_response()
}

pub async fn list_global(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let kind = kind(uri.path());
    let resources = state.api_v3.resources.lock().await;
    let items: Vec<_> = collection(&resources, kind).values().cloned().collect();
    let (count, items) = page_records(items, &params);
    Json(json!({kind:items,"count":count})).into_response()
}

/// `page`/`page_size`/`search`/`sort_field`/`sort_order` — общий контракт списков PNM.
fn page_records(items: Vec<Value>, params: &HashMap<String, String>) -> (usize, Vec<Value>) {
    let list = super::ListParams::from_query(params);
    super::sort_and_page(
        items,
        &list.search,
        &list.sort_field_or("display_name"),
        list.sort_order,
        list.page,
        list.page_size,
    )
}

pub async fn get_global(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let kind = kind(uri.path());
    match collection(&*state.api_v3.resources.lock().await, kind).get(&id).cloned() {
        Some(record) => Json(json!({singular(kind):record})).into_response(),
        None => error_json(404, "Resource not found"),
    }
}

pub async fn edit_global(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let kind = kind(uri.path());
    let mut resources = state.api_v3.resources.lock().await;
    let refs: Vec<String> = resources
        .links
        .iter()
        .filter(|(_, ids)| ids.iter().any(|value| value == &id))
        .map(|(key, _)| key.clone())
        .collect();
    if kind == "video_layouts" && refs.len() > 1 {
        return error_json(409, "Layout is shared; edit it through a conference/session to copy-on-write");
    }
    let previous = resources.clone();
    let Some(source) = collection(&resources, kind).get(&id).cloned() else {
        return error_json(404, "Resource not found");
    };
    let mut draft = source;
    if let Err(message) = update_resource(&mut draft, &body) {
        return error_json(422, &message);
    }
    if kind == "video_layouts" {
        sanitize_layout_record(&mut draft);
    }
    collection_mut(&mut resources, kind).insert(id.clone(), draft.clone());
    let mut rooms = Vec::new();
    for key in &refs {
        if !key.starts_with("conferences:") && !key.starts_with("sessions:") {
            continue;
        }
        let room = key.split(':').nth(1).unwrap_or_default().to_string();
        if kind == "video_layouts" {
            let active = resources.active_layouts.get(key).cloned();
            if active.is_none() {
                set_active(&mut resources, key, &id);
            }
        }
        rooms.push(room);
    }
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    if kind == "video_layouts" {
        for room in rooms {
            if let Ok(room) = room.parse::<vks_core::RoomId>() {
                super::sessions::push_layout_to_engine(&state, room).await;
            }
        }
    }
    Json(json!({singular(kind):draft})).into_response()
}

pub async fn remove_global(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let kind = kind(uri.path());
    let mut resources = state.api_v3.resources.lock().await;
    let refs: Vec<String> = resources
        .links
        .iter()
        .filter(|(_, ids)| ids.iter().any(|value| value == &id))
        .map(|(key, _)| key.clone())
        .collect();
    if kind == "video_layouts" && refs.len() > 1 {
        return error_json(409, "Layout is attached to multiple rooms; detach it from each room first");
    }
    let is_default =
        resources.layouts.get(&id).is_some_and(|layout| layout["payload"]["type"].as_str() == Some("default"));
    if kind == "video_layouts" && is_default && !refs.is_empty() {
        return error_json(409, "The default layout cannot be deleted while it is attached");
    }
    let previous = resources.clone();
    if collection_mut(&mut resources, kind).remove(&id).is_none() {
        return error_json(404, "Resource not found");
    }
    for key in &refs {
        if let Some(ids) = resources.links.get_mut(key) {
            ids.retain(|value| value != &id);
        }
    }
    for key in &refs {
        if !key.starts_with("conferences:") && !key.starts_with("sessions:") {
            continue;
        }
        if kind != "video_layouts" {
            continue;
        }
        let room = key.split(':').nth(1).unwrap_or_default().to_string();
        repair_active_layout(&mut resources, key, &room);
    }
    resources.layout_aliases.retain(|_, target| target != &id);
    let rooms: Vec<String> = refs.iter().filter_map(|key| key.split(':').nth(1).map(str::to_string)).collect();
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    if kind == "video_layouts" {
        for room in rooms {
            if let Ok(room) = room.parse::<vks_core::RoomId>() {
                super::sessions::push_layout_to_engine(&state, room).await;
            }
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn list_linked(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let kind = kind(uri.path());
    let resources = state.api_v3.resources.lock().await;
    let key = scoped_key(uri.path(), &room.id.to_string(), kind);
    // Сессия = живой экземпляр конференции: показываем union собственных
    // ссылок и ссылок конференции, иначе раскладки, созданные после старта,
    // не появляются в UI сессии (и не применяются к MCU).
    let mut ids: Vec<String> = resources.links.get(&key).cloned().unwrap_or_default();
    if uri.path().contains("/sessions/") {
        let inherited = format!("conferences:{}:{kind}", room.id);
        for id in resources.links.get(&inherited).cloned().unwrap_or_default() {
            // Если в session-scope уже есть COW-клон, оригинал из конференции
            // не показываем: UI не должен видеть две строки одной раскладки.
            let resolved =
                if kind == "video_layouts" { resolve_layout_alias(&resources, &key, &id) } else { id.clone() };
            let aliased = kind == "video_layouts" && resolved != id && ids.contains(&resolved);
            if !aliased && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    let mut items: Vec<Value> = ids.iter().filter_map(|id| collection(&resources, kind).get(id).cloned()).collect();
    if kind == "video_layouts" {
        for record in &mut items {
            with_layout_abonents(&state, record, &room.id);
        }
        Json(json!({kind:items,"count":items.len()})).into_response()
    } else {
        let (count, items) = page_records(items, &params);
        Json(json!({kind:items,"count":count})).into_response()
    }
}

/// A resource UUID alone does not grant access through an unrelated conference.
pub async fn get_linked(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path((id, resource_id)): Path<(String, String)>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let kind = kind(uri.path());
    let resources = state.api_v3.resources.lock().await;
    let resource_id = resolve_read_layout_id(&resources, uri.path(), &room.id, kind, &resource_id);
    if let Some(response) = attached(&resources, uri.path(), &room.id, kind, &resource_id) {
        return response;
    }
    match collection(&resources, kind).get(&resource_id) {
        Some(record) => {
            let mut record = record.clone();
            drop(resources);
            if kind == "video_layouts" {
                with_layout_abonents(&state, &mut record, &room.id);
            }
            Json(json!({singular(kind):record})).into_response()
        }
        None => error_json(404, "Resource not found"),
    }
}

pub async fn edit_linked(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path((id, resource_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let kind = kind(uri.path());
    let mut resources = state.api_v3.resources.lock().await;
    let scope_key = scoped_key(uri.path(), &room.id.to_string(), kind);
    let resource_id = resolve_layout_alias(&resources, &scope_key, &resource_id);
    if kind == "video_layouts" {
        if !layout_visible(&resources, &room.id.to_string(), &resource_id) {
            return error_json(404, "Resource is not attached to this conference or session");
        }
    } else if let Some(response) = attached(&resources, uri.path(), &room.id, kind, &resource_id) {
        return response;
    }
    let previous = resources.clone();
    let Some(source) = collection(&resources, kind).get(&resource_id).cloned() else {
        return error_json(404, "Resource not found");
    };
    // Валидация — на draft-копии, до вставки клона/переключения ссылок.
    let mut draft = source.clone();
    if let Err(message) = update_resource(&mut draft, &body) {
        return error_json(422, &message);
    }
    if kind == "video_layouts" {
        sanitize_layout_record(&mut draft);
    }
    let target_id = if kind == "video_layouts" {
        let owners = resources.links.values().filter(|ids| ids.iter().any(|value| value == &resource_id)).count();
        let direct = resources.links.get(&scope_key).is_some_and(|ids| ids.iter().any(|value| value == &resource_id));
        if direct && owners <= 1 {
            resources.layouts.insert(resource_id.clone(), draft.clone());
            resource_id.clone()
        } else {
            cow_replace(&mut resources, &scope_key, &resource_id, draft.clone())
        }
    } else {
        collection_mut(&mut resources, kind).insert(resource_id.clone(), draft.clone());
        resource_id.clone()
    };
    let result =
        resources.layouts.get(&target_id).or_else(|| resources.audiochannels.get(&target_id)).cloned().unwrap_or(draft);
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    if kind == "video_layouts" {
        super::sessions::push_layout_to_engine(&state, room.id).await;
    }
    Json(json!({singular(kind):result})).into_response()
}

pub async fn layout_contacts(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path((id, layout_id)): Path<(String, String)>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let resources = state.api_v3.resources.lock().await;
    let key = format!("conferences:{}:video_layouts", room.id);
    if !resources.links.get(&key).is_some_and(|ids| ids.contains(&layout_id)) {
        return error_json(404, "Layout is not attached to this conference");
    }
    let Some(layout) = resources.layouts.get(&layout_id) else {
        return error_json(404, "Layout not found");
    };
    let contacts = layout["contacts"]["contacts"].as_array().cloned().unwrap_or_default();
    Json(json!({"count":contacts.len(),"contacts":contacts})).into_response()
}

pub async fn attach_layout_contact(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path((id, layout_id, contact_id)): Path<(String, String, String)>,
    Json(_body): Json<Value>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let mut resources = state.api_v3.resources.lock().await;
    let scope_key = format!("conferences:{}:video_layouts", room.id);
    let layout_id = resolve_layout_alias(&resources, &scope_key, &layout_id);
    if !layout_visible(&resources, &room.id.to_string(), &layout_id) {
        return error_json(404, "Layout is not attached to this conference");
    }
    let Some(contact) = resources
        .contacts
        .get(&room.id.to_string())
        .into_iter()
        .flatten()
        .find(|contact| contact["contact_id"].as_str() == Some(&contact_id))
        .cloned()
    else {
        return error_json(404, "Contact does not belong to this conference");
    };
    let Some(source) = resources.layouts.get(&layout_id).cloned() else {
        return error_json(404, "Layout not found");
    };
    let previous = resources.clone();
    let mut draft = source;
    let mut contacts = draft["contacts"]["contacts"].as_array().cloned().unwrap_or_default();
    if !contacts.iter().any(|contact| contact["contact_id"].as_str() == Some(&contact_id)) {
        contacts.push(json!({"id":contact_id,"contact_id":contact_id,"display_name":contact["display_name"],
            "contact_type":contact["contact_type"],"type":contact["contact_type"]}));
    }
    draft["contacts"] = json!({"count":contacts.len(),"contacts":contacts});
    let owners = resources.links.values().filter(|ids| ids.iter().any(|value| value == &layout_id)).count();
    let direct = resources.links.get(&scope_key).is_some_and(|ids| ids.iter().any(|value| value == &layout_id));
    let target_id = if direct && owners <= 1 {
        resources.layouts.insert(layout_id.clone(), draft);
        layout_id.clone()
    } else {
        cow_replace(&mut resources, &scope_key, &layout_id, draft)
    };
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    // Живое изменение раскладки должно доехать до MCU без перезапуска сессии.
    drop(resources);
    super::sessions::push_layout_to_engine(&state, room.id).await;
    {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Ok(value) = axum::http::HeaderValue::from_str(&target_id) {
            response.headers_mut().insert("x-vks-layout-id", value);
        }
        response
    }
}

pub async fn detach_layout_contact(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path((id, layout_id, contact_id)): Path<(String, String, String)>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let mut resources = state.api_v3.resources.lock().await;
    let scope_key = format!("conferences:{}:video_layouts", room.id);
    let layout_id = resolve_layout_alias(&resources, &scope_key, &layout_id);
    if !layout_visible(&resources, &room.id.to_string(), &layout_id) {
        return error_json(404, "Layout is not attached to this conference");
    }
    let Some(source) = resources.layouts.get(&layout_id).cloned() else {
        return error_json(404, "Layout not found");
    };
    let previous = resources.clone();
    let mut draft = source;
    let mut contacts = draft["contacts"]["contacts"].as_array().cloned().unwrap_or_default();
    contacts.retain(|contact| contact["contact_id"].as_str() != Some(&contact_id));
    draft["contacts"] = json!({"count":contacts.len(),"contacts":contacts});
    let owners = resources.links.values().filter(|ids| ids.iter().any(|value| value == &layout_id)).count();
    let direct = resources.links.get(&scope_key).is_some_and(|ids| ids.iter().any(|value| value == &layout_id));
    let target_id = if direct && owners <= 1 {
        resources.layouts.insert(layout_id.clone(), draft);
        layout_id.clone()
    } else {
        cow_replace(&mut resources, &scope_key, &layout_id, draft)
    };
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    // Живое изменение раскладки должно доехать до MCU без перезапуска сессии.
    drop(resources);
    super::sessions::push_layout_to_engine(&state, room.id).await;
    {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Ok(value) = axum::http::HeaderValue::from_str(&target_id) {
            response.headers_mut().insert("x-vks-layout-id", value);
        }
        response
    }
}

pub async fn attach(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path((id, resource_id)): Path<(String, String)>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let kind = kind(uri.path());
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    let scope_key = scoped_key(uri.path(), &room.id.to_string(), kind);
    let resource_id = resolve_layout_alias(&resources, &scope_key, &resource_id);
    if !collection(&resources, kind).contains_key(&resource_id) {
        return error_json(404, "Resource not found");
    }
    if kind == "video_layouts" {
        let list = resources.links.entry(scope_key.clone()).or_default();
        if !list.contains(&resource_id) {
            list.push(resource_id.clone());
        }
        // «Применить» = сделать активной; порядок links не меняем.
        set_active(&mut resources, &scope_key, &resource_id);
    } else {
        let list = resources.links.entry(scope_key).or_default();
        if !list.contains(&resource_id) {
            list.push(resource_id);
        }
    }
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    if kind == "video_layouts" {
        drop(resources);
        super::sessions::push_layout_to_engine(&state, room.id).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn detach(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path((id, resource_id)): Path<(String, String)>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let kind = kind(uri.path());
    let scope_key = scoped_key(uri.path(), &room.id.to_string(), kind);
    let mut resources = state.api_v3.resources.lock().await;
    let resource_id = resolve_layout_alias(&resources, &scope_key, &resource_id);
    let direct = resources.links.get(&scope_key).is_some_and(|ids| ids.iter().any(|value| value == &resource_id));
    if !direct {
        return error_json(404, "Resource is not directly attached to this scope");
    }
    if kind == "video_layouts" {
        let Some(layout) = resources.layouts.get(&resource_id) else {
            return error_json(404, "Resource not found");
        };
        if layout["payload"]["type"].as_str() == Some("default") {
            return error_json(409, "The default layout cannot be detached");
        }
    }
    let previous = resources.clone();
    if let Some(ids) = resources.links.get_mut(&scope_key) {
        ids.retain(|value| value != &resource_id);
    }
    // Alias на удалённый клон больше не должен вести в пустоту — только
    // в текущем scope: alias конференции может вести на тот же клон.
    let alias_prefix = format!("{scope_key}|");
    resources.layout_aliases.retain(|key, target| !(key.starts_with(&alias_prefix) && target == &resource_id));
    if kind == "video_layouts" {
        repair_active_layout(&mut resources, &scope_key, &room.id.to_string());
    }
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    if kind == "video_layouts" {
        drop(resources);
        super::sessions::push_layout_to_engine(&state, room.id).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct ScheduleQuery {
    pub object_id: Option<String>,
    pub object_type: Option<String>,
}

pub async fn schedules(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<ScheduleQuery>,
) -> Response {
    if let Some(ref id) = query.object_id {
        let Some(room) = find_room(&state, id) else {
            return error_json(404, "Conference not found");
        };
        if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
            return response;
        }
    } else if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let resources = state.api_v3.resources.lock().await;
    let items: Vec<_> = resources
        .schedules
        .values()
        .filter(|item| {
            query.object_id.as_ref().is_none_or(|id| item["object_id"].as_str() == Some(id))
                && query.object_type.as_ref().is_none_or(|kind| item["object_type"].as_str() == Some(kind))
        })
        .cloned()
        .collect();
    Json(json!({"schedules":items,"count":items.len()})).into_response()
}

/// The original client uses five cron fields and timestamps in seconds, not a
/// frontend-only "weekly"/"once" enum. Saving definitions does not execute them.
fn validate_schedule(body: &Value) -> Result<(), &'static str> {
    let cron = body.get("cron_expression").and_then(Value::as_str).ok_or("cron_expression is required")?;
    let fields: Vec<_> = cron.split_whitespace().collect();
    if fields.len() != 5
        || fields.iter().any(|field| {
            field.is_empty()
                || !field.chars().all(|character| character.is_ascii_alphanumeric() || "*/,-".contains(character))
        })
    {
        return Err("cron_expression must contain five cron fields");
    }
    for (field_index, (field, (minimum, maximum))) in
        fields.iter().zip([(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)]).enumerate()
    {
        for part in field.split(',') {
            let (range, step) = part.split_once('/').map_or((part, None), |(range, step)| (range, Some(step)));
            if step.is_some_and(|step| step.parse::<u32>().ok().is_none_or(|step| step == 0)) {
                return Err("cron step must be positive");
            }
            if range == "*" {
                continue;
            }
            let mut previous = None;
            for boundary in range.split('-') {
                // Named months/weekdays are supported by the original cron editor.
                let name = boundary.to_ascii_uppercase();
                if (field_index == 3
                    && matches!(
                        name.as_str(),
                        "JAN" | "FEB" | "MAR" | "APR" | "MAY" | "JUN" | "JUL" | "AUG" | "SEP" | "OCT" | "NOV" | "DEC"
                    ))
                    || (field_index == 4
                        && matches!(name.as_str(), "SUN" | "MON" | "TUE" | "WED" | "THU" | "FRI" | "SAT"))
                {
                    continue;
                }
                let value = boundary.parse::<u32>().map_err(|_| "Invalid cron field")?;
                if value < minimum || value > maximum || previous.is_some_and(|start| start > value) {
                    return Err("Cron field is outside its valid range");
                }
                previous = Some(value);
            }
            if range.split('-').count() > 2 {
                return Err("Invalid cron range");
            }
        }
    }
    let zone = body.get("time_zone").and_then(Value::as_str).ok_or("time_zone is required")?;
    if zone.is_empty() || zone.len() > 128 || zone.chars().any(char::is_control) {
        return Err("Invalid time_zone");
    }
    for field in ["valid_from", "valid_until"] {
        if body.get(field).is_some_and(|value| !value.is_null() && value.as_u64().is_none()) {
            return Err("Validity timestamps must be nonnegative integer seconds");
        }
    }
    let start = body["valid_from"].as_u64().unwrap_or(0);
    let end = body["valid_until"].as_u64().unwrap_or(0);
    if end > 0 && end < start {
        return Err("valid_until precedes valid_from");
    }
    if body.get("payload").is_some_and(|value| !value.is_object()) {
        return Err("Schedule payload must be an object");
    }
    if body["payload"].get("duration").is_some_and(|value| !value.is_null() && value.as_u64().is_none()) {
        return Err("Duration must be nonnegative integer seconds");
    }
    Ok(())
}

pub async fn create_schedule(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    let Some(object_id) = body["object_id"].as_str() else {
        return error_json(400, "object_id is required");
    };
    if body["object_type"].as_str() != Some("conferences") {
        return error_json(400, "Only conference schedule definitions are supported");
    }
    let Some(room) = find_room(&state, object_id) else {
        return error_json(404, "Conference not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    if let Err(error) = validate_schedule(&body) {
        return error_json(400, error);
    }
    let id = Uuid::new_v4().to_string();
    let mut schedule = json!({"id":id,"object_id":room.id.to_string(),"object_type":"conferences",
        "created_at":now(),"creator_id":auth.subject,"valid_from":null,"valid_until":null,
        "payload":{"duration":null}});
    for field in ["cron_expression", "time_zone", "valid_from", "valid_until", "payload"] {
        if let Some(value) = body.get(field) {
            schedule[field] = value.clone();
        }
    }
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    resources.schedules.insert(id, schedule.clone());
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    (StatusCode::CREATED, Json(json!({"schedule":schedule}))).into_response()
}

pub async fn get_schedule(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    let resources = state.api_v3.resources.lock().await;
    let Some(schedule) = resources.schedules.get(&id) else {
        return error_json(404, "Schedule not found");
    };
    let Some(room_id) = schedule["object_id"].as_str() else {
        return error_json(500, "Invalid stored schedule owner");
    };
    if let Err(response) = check_permission(&auth, Some(room_id)) {
        return response;
    }
    Json(json!({"schedule":schedule})).into_response()
}

pub async fn edit_schedule(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut resources = state.api_v3.resources.lock().await;
    let Some(mut schedule) = resources.schedules.get(&id).cloned() else {
        return error_json(404, "Schedule not found");
    };
    let Some(room_id) = schedule["object_id"].as_str() else {
        return error_json(500, "Invalid stored schedule owner");
    };
    if let Err(response) = check_permission(&auth, Some(room_id)) {
        return response;
    }
    for field in ["cron_expression", "time_zone", "valid_from", "valid_until", "payload"] {
        if let Some(value) = body.get(field) {
            schedule[field] = value.clone();
        }
    }
    if let Err(error) = validate_schedule(&schedule) {
        return error_json(400, error);
    }
    let previous = resources.clone();
    resources.schedules.insert(id, schedule.clone());
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    Json(json!({"schedule":schedule})).into_response()
}

pub async fn remove_schedule(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    let mut resources = state.api_v3.resources.lock().await;
    let Some(schedule) = resources.schedules.get(&id) else {
        return error_json(404, "Schedule not found");
    };
    let Some(room_id) = schedule["object_id"].as_str() else {
        return error_json(500, "Invalid stored schedule owner");
    };
    if let Err(response) = check_permission(&auth, Some(room_id)) {
        return response;
    }
    let previous = resources.clone();
    resources.schedules.remove(&id);
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Каталог `/video_layout_grids`: ровно 6 встроенных custom-схем эталона.
/// Пресеты `preset_*` — статическая карта vendor-UI и в этот каталог не входят.
fn all_grids() -> Vec<Value> {
    // Эталон TrueConf 2.2.1.66 отдаёт в `/video_layout_grids` ровно 6
    // встроенных custom-схем; `preset_*` — статическая карта UI, в этот
    // каталог не входят. Держим только builtin, иначе UI рисует 179
    // псевдо-схем и ломает страницу «Раскладки».
    vks_layout::preset::builtin_grids().into_iter().filter_map(|grid| serde_json::to_value(&grid).ok()).collect()
}

/// Контракт списков PNM для grid-каталога: поиск по `display_name`/`id`,
/// сортировка (`sort_order=1` — по убыванию) и пагинация. UI-маппер присылает
/// поисковый запрос как `display_name` (см. `gridListConfig`), поэтому он
/// принимается наравне с `search`.
fn page_grids(mut items: Vec<Value>, params: &HashMap<String, String>) -> (usize, Vec<Value>) {
    let mut params = params.clone();
    if !params.contains_key("search") {
        if let Some(value) = params.get("display_name").cloned() {
            params.insert("search".to_string(), value);
        }
    }
    let list = super::ListParams::from_query(&params);
    if !list.search.is_empty() {
        let needle = list.search.to_lowercase();
        items.retain(|grid| {
            grid["display_name"].as_str().unwrap_or_default().to_lowercase().contains(&needle)
                || grid["id"].as_str().unwrap_or_default().to_lowercase().contains(&needle)
        });
    }
    let field = if list.sort_field.is_empty() { "display_name" } else { list.sort_field.as_str() };
    items.sort_by(|left, right| {
        let order = match (left[field].as_u64(), right[field].as_u64()) {
            (Some(left), Some(right)) => left.cmp(&right),
            _ => left[field]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .cmp(&right[field].as_str().unwrap_or_default().to_lowercase()),
        };
        if list.sort_order == 1 {
            order.reverse()
        } else {
            order
        }
    });
    let count = items.len();
    let start = list.page.saturating_sub(1).saturating_mul(list.page_size);
    (count, items.into_iter().skip(start).take(list.page_size).collect())
}

pub async fn grids(Query(params): Query<HashMap<String, String>>) -> Response {
    let (count, grids) = page_grids(all_grids(), &params);
    Json(json!({"grids":grids,"count":count})).into_response()
}

pub async fn get_grid(Path(id): Path<String>) -> Response {
    match all_grids().into_iter().find(|grid| grid["id"].as_str() == Some(&id)) {
        Some(grid) => Json(json!({"grid":grid})).into_response(),
        None => error_json(404, "Grid not found"),
    }
}

pub async fn smtp() -> Response {
    Json(json!({"smtp":{"enabled":false,"enable":false,"host":"","port":25,"username":"","password":"","sender":""}}))
        .into_response()
}

pub async fn user(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let users = state.users.lock().unwrap();
    if id == "00000000-0000-0000-0000-000000000000" {
        return Json(
            json!({"user":{"id":id,"display_name":"Неизвестный создатель","emails":[],"phones":[],"avatars":[]}}),
        )
        .into_response();
    }
    match users.values().find(|user|user.id.to_string()==id || user.username==id) {
        Some(user)=>Json(json!({"user":{"id":user.id.to_string(),"display_name":user.username,
            "creator_id":user.id.to_string(),"locale_id":"ru_RU","time_zone_id":"UTC", "emails":[],"phones":[],"avatars":[]}})).into_response(),
        None=>error_json(404,"User not found"),
    }
}

/// `GET …/video_layouts/templates?video_layout_id=…` — шаблоны превью.
///
/// manual — ровно один шаблон с реальными слотами (как раньше); auto/dynamic —
/// по шаблону на каждый применимый preset. Без `video_layout_id` берётся
/// «Общая» раскладка комнаты (`type=default`), иначе первая привязанная.
pub async fn templates(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(room) = find_room(&state, &id) else {
        return error_json(404, "Conference or session not found");
    };
    if let Err(response) = check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }
    let resources = state.api_v3.resources.lock().await;
    let key = scoped_key(uri.path(), &room.id.to_string(), "video_layouts");
    let layout = match params.get("video_layout_id") {
        Some(requested) => {
            let requested = resolve_layout_alias(&resources, &key, requested);
            if !layout_visible(&resources, &room.id.to_string(), &requested) {
                None
            } else {
                resources.layouts.get(&requested).cloned()
            }
        }
        None => {
            let attached = resources.links.get(&key).cloned().unwrap_or_default();
            // Если конкретная раскладка не запрошена, показываем последнюю
            // ручную/индивидуальную: именно её оператор только что сохранил.
            // Fallback — «Общая» (default), затем первая привязанная.
            let selected = attached
                .iter()
                .rev()
                .find(|id| {
                    resources
                        .layouts
                        .get(*id)
                        .is_some_and(|layout| layout["payload"]["type"].as_str() != Some("default"))
                })
                .or_else(|| {
                    attached.iter().find(|id| {
                        resources
                            .layouts
                            .get(*id)
                            .is_some_and(|layout| layout["payload"]["type"].as_str() == Some("default"))
                    })
                })
                .or_else(|| attached.first())
                .cloned();
            selected.and_then(|id| resources.layouts.get(&id).cloned())
        }
    };
    // Эталон TrueConf 2.2.1.66 отдаёт шаблоны только для manual-раскладок
    // (dynamic/auto → count 0): подвыбор схем в редакторе строит сам UI по
    // `rangeMap`, а не сервер. Не выдумываем лишние шаблоны.
    let templates = match layout {
        Some(layout) if layout["payload"]["mode"].as_str() == Some("manual") => {
            // UI ждёт полноценную запись раскладки (id/display_name/
            // creator_id/created_at/payload), а не только preview-payload:
            // по id он открывает редактор, по created_at сортирует.
            vec![layout]
        }
        _ => Vec::new(),
    };
    Json(json!({"count":templates.len(),"video_layout_templates":templates})).into_response()
}

#[cfg(test)]
#[path = "resource_tests.rs"]
mod resource_tests;
