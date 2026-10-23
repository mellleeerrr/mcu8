//! Личные адресные книги пользователей PNM (T2.1).

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use super::super::auth::ClientIp;
use super::super::call_strings::default_contact_payload;
use super::super::resources::AddressRecord;
use super::super::{error_json, sort_and_page, track, ListParams};
use super::{commit_directory, ensure_profile, find_user, json_body, now_secs, require_manage, require_read};
use crate::auth_middleware::AuthContext;
use crate::state::SharedState;
use crate::users::Users;
use vks_core::Role;

// --- Личные адресные книги -------------------------------------------------

fn user_addresses_json(records: impl Iterator<Item = (String, AddressRecord)>) -> Vec<Value> {
    records.map(|(id, address)| super::super::call_strings::address_value(&id, &address)).collect()
}

/// `GET /api/v3/users/{id}/call_strings`.
pub async fn list_user_call_strings(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("users.call_strings.list");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    let resources = state.api_v3.resources.lock().await;
    let records = resources
        .directory
        .user_addresses
        .get(&user_id)
        .map(|map| map.iter().map(|(id, a)| (id.clone(), a.clone())).collect::<Vec<_>>())
        .unwrap_or_default();
    let count = records.len();
    Json(json!({"count": count, "call_strings": user_addresses_json(records.into_iter())})).into_response()
}

/// `POST /api/v3/users/{id}/call_strings/{address_id}` — привязать существующий
/// глобальный адрес к личной адресной книге пользователя.
pub async fn attach_user_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, address_id)): Path<(String, String)>,
) -> Response {
    track("users.call_strings.attach");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    if let Err(response) = commit_directory(&state, |resources| {
        let Some(address) = resources.addresses.get(&address_id).cloned() else {
            return Err(error_json(404, "Call string not found"));
        };
        resources.directory.user_addresses.entry(user_id.clone()).or_default().insert(address_id.clone(), address);
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.attached",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

/// `DELETE /api/v3/users/{id}/call_strings/{address_id}` — отвязать адрес
/// (глобальная книга не затрагивается).
pub async fn detach_user_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, address_id)): Path<(String, String)>,
) -> Response {
    track("users.call_strings.detach");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    if let Err(response) = commit_directory(&state, |resources| {
        let removed =
            resources.directory.user_addresses.get_mut(&user_id).and_then(|map| map.remove(&address_id)).is_some();
        if !removed {
            return Err(error_json(404, "Call string not found"));
        }
        // Удалённый адрес не может остаться основным.
        if let Some(profile) = resources.directory.users.get_mut(&user_id) {
            if profile.primary_address.as_deref() == Some(address_id.as_str()) {
                profile.primary_address = None;
            }
        }
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.detached",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

/// `POST /api/v3/users/{id}/call_strings/{address_id}/favorite` — назначить
/// основной (primary) личный адрес; vendor показывает «звёздочку» при >1 адресе
/// и ожидает этот вызов. `DELETE` — снять primary, если он совпадает.
pub async fn favorite_user_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, address_id)): Path<(String, String)>,
) -> Response {
    track("users.call_strings.favorite");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    if let Err(response) = commit_directory(&state, |resources| {
        let present = resources.directory.user_addresses.get(&user_id).is_some_and(|map| map.contains_key(&address_id));
        if !present {
            return Err(error_json(404, "Call string not found"));
        }
        ensure_profile(&mut resources.directory, &user).primary_address = Some(address_id.clone());
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.favorited",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok", "address_id": address_id})).into_response()
}

/// `DELETE …/favorite` — снять основной адрес (идемпотентно).
pub async fn unfavorite_user_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, address_id)): Path<(String, String)>,
) -> Response {
    track("users.call_strings.unfavorite");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    if let Err(response) = commit_directory(&state, |resources| {
        if let Some(profile) = resources.directory.users.get_mut(&user_id) {
            if profile.primary_address.as_deref() == Some(address_id.as_str()) {
                profile.primary_address = None;
            }
        }
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.unfavorited",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

/// Проверка адреса: протокол (или host) обязателен, пробелы/управляющие символы
/// запрещены. `canonical_uri` сам добавляет `sip:`, поэтому проверяем остаток.
fn validate_call_string_uri(uri: &str) -> Result<(), String> {
    if uri.is_empty() || uri.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("uri is invalid".to_string());
    }
    let canonical = vks_common::uri::canonical_uri(uri);
    let rest = canonical.split_once(':').map(|(_, rest)| rest).unwrap_or_default();
    if rest.trim().is_empty() || rest == ":" {
        return Err("uri is invalid".to_string());
    }
    Ok(())
}

/// `PATCH /api/v3/ui/users/{id}/call_strings/{address_id}` — редактировать
/// личный адрес (display_name/uri/payload). Глобальная книга не затрагивается.
pub async fn update_user_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, address_id)): Path<(String, String)>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("users.call_strings.update");
    let body = match json_body(payload) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    let updated = match commit_directory(&state, |resources| {
        let Some(existing) =
            resources.directory.user_addresses.get(&user_id).and_then(|map| map.get(&address_id)).cloned()
        else {
            return Err(error_json(404, "Call string not found"));
        };
        let mut updated = existing;
        if let Some(uri) = body["uri"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            if let Err(error) = validate_call_string_uri(uri) {
                return Err(error_json(400, &error));
            }
            updated.uri = uri.to_string();
        }
        if let Some(display_name) = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            updated.display_name = display_name.to_string();
        }
        if body.get("payload").is_some() {
            updated.payload = default_contact_payload(body.get("payload"));
        }
        if let Some(map) = resources.directory.user_addresses.get_mut(&user_id) {
            map.insert(address_id.clone(), updated.clone());
        }
        Ok(updated)
    })
    .await
    {
        Ok(updated) => updated,
        Err(response) => return response,
    };
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.updated",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"call_string": super::super::call_strings::address_value(&address_id, &updated)})).into_response()
}

/// `POST /api/v3/ui/users/{id}/call_strings` — создать адрес сразу в личной книге.
pub async fn create_user_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(user_id): Path<String>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("users.call_strings.create");
    let body = match json_body(payload) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let uri = body["uri"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or_default();
    if uri.is_empty() {
        return error_json(400, "uri is required");
    }
    if let Err(error) = validate_call_string_uri(uri) {
        return error_json(400, &error);
    }
    let display_name =
        body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(uri).to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let payload = default_contact_payload(body.get("payload"));
    let address = AddressRecord { id: id.clone(), uri: uri.to_string(), display_name, created_at: now_secs(), payload };
    let user_id = user.id.to_string();
    let address_clone = address.clone();
    if let Err(response) = commit_directory(&state, |resources| {
        resources.directory.user_addresses.entry(user_id.clone()).or_default().insert(id.clone(), address_clone);
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.created",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    (StatusCode::CREATED, Json(json!({"call_string": super::super::call_strings::address_value(&id, &address)})))
        .into_response()
}

/// `ui:call_strings:search` с `contact_id` — личная адресная книга пользователя.
pub(crate) async fn ws_user_call_strings(
    state: &SharedState,
    subject: &str,
    payload: &Value,
    contact_id: &str,
) -> Result<Value, (u16, &'static str)> {
    let role = Users::resolve_role(state, subject).ok_or((403, "Forbidden"))?;
    if role < Role::Moderator {
        return Err((403, "Forbidden: insufficient permissions"));
    }
    let user = find_user(state, contact_id).ok_or((404, "User not found"))?;
    let user_id = user.id.to_string();
    let resources = state.api_v3.resources.lock().await;
    let records = resources
        .directory
        .user_addresses
        .get(&user_id)
        .map(|map| map.iter().map(|(id, a)| (id.clone(), a.clone())).collect::<Vec<_>>())
        .unwrap_or_default();
    let values = user_addresses_json(records.into_iter());
    let list = ListParams::parse(payload);
    let search = payload
        .get("search")
        .or_else(|| payload.get("query"))
        .and_then(Value::as_str)
        .unwrap_or(&list.search)
        .to_lowercase();
    let uri_filter = payload.get("uri").and_then(Value::as_str).unwrap_or_default().to_lowercase();
    let values: Vec<Value> = values
        .into_iter()
        .filter(|value| {
            uri_filter.is_empty() || value["uri"].as_str().unwrap_or_default().to_lowercase().contains(&uri_filter)
        })
        .collect();
    let (count, value) = sort_and_page(values, &search, &list.sort_field, list.sort_order, list.page, list.page_size);
    Ok(json!({"call_strings": value, "count": count}))
}
