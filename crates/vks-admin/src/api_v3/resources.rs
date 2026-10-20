//! Persistable PNM conference resources. These are real definitions and links,
//! not canned success responses: creation, attachment, edits and removal share one store.
use std::collections::BTreeMap;

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
}

pub const CONFIGURATION_KEY: &str = "pnm-definitions-v1";

/// Caller holds the async resources lock, serializing mutations and durable commits.
pub async fn persist(state: &SharedState, resources: &ResourceState) -> Result<(), String> {
    if let Some(storage) = &state.storage {
        let configurations = state.api_v3.conference_settings.lock().map_err(|e| e.to_string())?.clone();
        let document = json!({"version":1,"resources":resources,"conference_settings":configurations});
        storage
            .put_configuration_document(CONFIGURATION_KEY, &document.to_string())
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn commit(state: &SharedState, resources: &mut ResourceState, previous: ResourceState) -> Result<(), Response> {
    if let Err(error) = persist(state, resources).await {
        *resources = previous;
        return Err(error_json(500, &format!("Storage error: {error}")));
    }
    Ok(())
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

fn normalize_resource(kind: &str, body: &Value, creator: &str) -> Value {
    let mut payload = if kind == "video_layouts" {
        json!({"mode":"auto", "preset":"preset_default_13", "type":"default", "slots":[],
            "size":13, "autoscroll":0})
    } else {
        json!({"type":"default", "vad":true, "autofill_speakers":true, "autofill_listeners":true,
            "speakers":[], "listeners":[], "blacklist":[], "whitelist":[], "attachmode":"BLACKLIST"})
    };
    if let Some(values) = body.get("payload").and_then(Value::as_object) {
        merge_json(&mut payload, &Value::Object(values.clone()));
    }
    let mut result = json!({"id":Uuid::new_v4().to_string(), "created_at":now(), "creator_id":creator,
        "display_name":body.get("display_name").cloned().unwrap_or(Value::Null), "payload":payload,
        "defaults":{"deletable":true,"editable":true}, "speakers":0,"listeners":0});
    if kind == "video_layouts" {
        result["abonents"] = json!({"abonents":{"count":0,"abonents":[]},"records":[],"rtmp":[],"youtube":[]});
        result["contacts"] = json!({"count":0,"contacts":[]});
    }
    result
}

/// PNM editors send partial nested payloads; changing one option must keep siblings.
fn merge_json(target: &mut Value, patch: &Value) {
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

fn update_resource(record: &mut Value, body: &Value) {
    if let Some(name) = body.get("display_name") {
        record["display_name"] = name.clone();
    }
    if let Some(payload) = body.get("payload").filter(|payload| payload.is_object()) {
        merge_json(&mut record["payload"], payload);
    }
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
    let record = normalize_resource(kind, &body, &auth.subject);
    let id = record["id"].as_str().unwrap().to_string();
    let mut resources = state.api_v3.resources.lock().await;
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
) -> Response {
    if let Err(response) = check_permission(&auth, None) {
        return response;
    }
    let kind = kind(uri.path());
    let resources = state.api_v3.resources.lock().await;
    let items: Vec<_> = collection(&resources, kind).values().cloned().collect();
    Json(json!({kind:items,"count":items.len()})).into_response()
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
    let previous = resources.clone();
    let Some(record) = collection_mut(&mut resources, kind).get_mut(&id) else {
        return error_json(404, "Resource not found");
    };
    update_resource(record, &body);
    let result = record.clone();
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    Json(json!({singular(kind):result})).into_response()
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
    let previous = resources.clone();
    if collection_mut(&mut resources, kind).remove(&id).is_none() {
        return error_json(404, "Resource not found");
    }
    for ids in resources.links.values_mut() {
        ids.retain(|value| value != &id);
    }
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn list_linked(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
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
    let items: Vec<Value> = resources
        .links
        .get(&key)
        .into_iter()
        .flatten()
        .filter_map(|id| collection(&resources, kind).get(id).cloned())
        .collect();
    Json(json!({kind:items,"count":items.len()})).into_response()
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
    let key = scoped_key(uri.path(), &room.id.to_string(), kind);
    if !resources.links.get(&key).is_some_and(|ids| ids.contains(&resource_id)) {
        return error_json(404, "Resource is not attached to this conference or session");
    }
    match collection(&resources, kind).get(&resource_id) {
        Some(record) => Json(json!({singular(kind):record})).into_response(),
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
    let key = scoped_key(uri.path(), &room.id.to_string(), kind);
    if !resources.links.get(&key).is_some_and(|ids| ids.contains(&resource_id)) {
        return error_json(404, "Resource is not attached to this conference or session");
    }
    let previous = resources.clone();
    let Some(record) = collection_mut(&mut resources, kind).get_mut(&resource_id) else {
        return error_json(404, "Resource not found");
    };
    update_resource(record, &body);
    let result = record.clone();
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
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
    let key = format!("conferences:{}:video_layouts", room.id);
    if !resources.links.get(&key).is_some_and(|ids| ids.contains(&layout_id)) {
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
    let previous = resources.clone();
    let Some(layout) = resources.layouts.get_mut(&layout_id) else {
        return error_json(404, "Layout not found");
    };
    let mut contacts = layout["contacts"]["contacts"].as_array().cloned().unwrap_or_default();
    if !contacts.iter().any(|contact| contact["contact_id"].as_str() == Some(&contact_id)) {
        contacts.push(json!({"id":contact_id,"contact_id":contact_id,"display_name":contact["display_name"],
            "contact_type":contact["contact_type"],"type":contact["contact_type"]}));
    }
    layout["contacts"] = json!({"count":contacts.len(),"contacts":contacts});
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
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
    let key = format!("conferences:{}:video_layouts", room.id);
    if !resources.links.get(&key).is_some_and(|ids| ids.contains(&layout_id)) {
        return error_json(404, "Layout is not attached to this conference");
    }
    let previous = resources.clone();
    let Some(layout) = resources.layouts.get_mut(&layout_id) else {
        return error_json(404, "Layout not found");
    };
    let mut contacts = layout["contacts"]["contacts"].as_array().cloned().unwrap_or_default();
    contacts.retain(|contact| contact["contact_id"].as_str() != Some(&contact_id));
    layout["contacts"] = json!({"count":contacts.len(),"contacts":contacts});
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
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
    if !collection(&resources, kind).contains_key(&resource_id) {
        return error_json(404, "Resource not found");
    }
    let list = resources.links.entry(scoped_key(uri.path(), &room.id.to_string(), kind)).or_default();
    if !list.contains(&resource_id) {
        list.push(resource_id);
    }
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
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
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    resources
        .links
        .entry(scoped_key(uri.path(), &room.id.to_string(), kind(uri.path())))
        .or_default()
        .retain(|value| value != &resource_id);
    if let Err(response) = commit(&state, &mut resources, previous).await {
        return response;
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

/// The original client needs a catalog of geometries even before it creates a layout.
fn grid_catalog() -> Vec<Value> {
    [1u32, 2, 3, 4, 6, 8, 9, 12, 13, 16]
        .iter()
        .map(|&count| {
            let cols = (count as f64).sqrt().ceil() as u32;
            let rows = count.div_ceil(cols);
            let slots: Vec<_> = (0..count)
                .map(|i| {
                    json!({"position":i,"x":(i%cols)*10000/cols,
            "y":(i/cols)*10000/rows,"width":10000/cols,"height":10000/rows,"z_order":0})
                })
                .collect();
            json!({"id":format!("preset_default_{count}"),"display_name":format!("{count}"),
            "payload":{"width":10000,"height":10000,"centered_row":false,"slots":slots},
            "defaults":{"deletable":false,"editable":false,"disableable":false}})
        })
        .collect()
}

pub async fn grids() -> Response {
    let grids = grid_catalog();
    Json(json!({"grids":grids,"count":grids.len()})).into_response()
}

pub async fn get_grid(Path(id): Path<String>) -> Response {
    match grid_catalog().into_iter().find(|grid| grid["id"].as_str() == Some(&id)) {
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

pub async fn templates() -> Response {
    Json(json!({"video_layout_templates":[],"count":0})).into_response()
}

#[cfg(test)]
#[path = "resource_tests.rs"]
mod resource_tests;
