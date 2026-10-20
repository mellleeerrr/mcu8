//! Группы справочника PNM (T2.1): CRUD и членство пользователей.

use std::collections::HashMap;

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use super::super::auth::ClientIp;
use super::super::{error_json, sort_and_page, track, ListParams};
use super::{
    commit_directory, ensure_profile, find_user, id_list, json_body, now_secs, require_manage, require_read,
    DirectoryGroup, DirectoryState,
};
use crate::auth_middleware::AuthContext;
use crate::state::SharedState;

pub(super) fn group_json(id: &str, group: &DirectoryGroup, directory: &DirectoryState) -> Value {
    let size = directory.users.values().filter(|u| u.groups.iter().any(|g| g == id)).count();
    json!({
        "id": id,
        "display_name": group.display_name,
        "size": size,
        "creator_id": group.creator_id,
        "created_at": group.created_at,
    })
}

// --- Группы ---------------------------------------------------------------

/// Список id из параметров vendor-UI: массив, строка через запятую или
/// одиночное значение (`exclude_users`, `users_exclude`, `exclude_groups`…).
/// `GET /api/v3/groups` → `{count, groups}`.
pub async fn list_groups(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    track("groups.list");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let params = Value::Object(params.into_iter().map(|(k, v)| (k, Value::String(v))).collect());
    let list = ListParams::parse(&params);
    // Vendor 2.2.1.66 (`InfoMainDescription`) фильтрует группы параметром
    // `display_name`; `search` оставлен как алиас (контракт T2.1 в тестах/curl).
    let search = params
        .get("display_name")
        .or_else(|| params.get("search"))
        .or_else(|| params.get("query"))
        .and_then(Value::as_str)
        .unwrap_or(&list.search)
        .to_lowercase();
    let user_id = params.get("user_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let exclude_users = id_list(&params, &["users_exclude", "exclude_users"]);
    let resources = state.api_v3.resources.lock().await;
    let groups: Vec<Value> = resources
        .directory
        .groups
        .iter()
        .map(|(id, group)| group_json(id, group, &resources.directory))
        .filter(|value| {
            let gid = value["id"].as_str().unwrap_or_default();
            let members: Vec<&str> = resources
                .directory
                .users
                .iter()
                .filter(|(_, u)| u.groups.iter().any(|g| g == gid))
                .map(|(uid, _)| uid.as_str())
                .collect();
            if !user_id.is_empty() && !members.contains(&user_id.as_str()) {
                return false;
            }
            if exclude_users.iter().any(|excluded| members.contains(&excluded.as_str())) {
                return false;
            }
            true
        })
        .collect();
    let (count, groups) = sort_and_page(groups, &search, &list.sort_field, list.sort_order, list.page, list.page_size);
    Json(json!({"count": count, "groups": groups})).into_response()
}

/// `POST /api/v3/groups` → `{group: {...}}`.
pub async fn create_group(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("groups.create");
    let body = match json_body(payload) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let display_name = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or_default();
    if display_name.is_empty() {
        return error_json(400, "display_name is required");
    }
    if display_name.chars().count() > 128 {
        return error_json(400, "display_name is too long");
    }
    let id = uuid::Uuid::new_v4().to_string();
    let group = DirectoryGroup {
        display_name: display_name.to_string(),
        created_at: now_secs(),
        creator_id: Some(auth.subject.clone()),
    };
    let value = match commit_directory(&state, |resources| {
        if resources.directory.groups.values().any(|g| g.display_name.to_lowercase() == display_name.to_lowercase()) {
            return Err(error_json(409, "Group with the same name already exists"));
        }
        resources.directory.groups.insert(id.clone(), group);
        let value =
            group_json(&id, resources.directory.groups.get(&id).expect("group inserted above"), &resources.directory);
        Ok(value)
    })
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    state.record_audit_entry(
        &auth.subject,
        "group.created",
        Some(id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    (StatusCode::CREATED, Json(json!({"group": value}))).into_response()
}

/// `GET /api/v3/groups/{id}` → `{group: {...}}`.
pub async fn get_group(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("groups.get");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let resources = state.api_v3.resources.lock().await;
    match resources.directory.groups.get(&id) {
        Some(group) => Json(json!({"group": group_json(&id, group, &resources.directory)})).into_response(),
        None => error_json(404, "Group not found"),
    }
}

/// `PATCH /api/v3/groups/{id}`.
pub async fn update_group(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("groups.update");
    let body = match json_body(payload) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let display_name = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty());
    if body.get("display_name").is_some() && display_name.is_none() {
        return error_json(400, "display_name must not be empty");
    }
    if display_name.is_some_and(|d| d.chars().count() > 128) {
        return error_json(400, "display_name is too long");
    }
    let value = match commit_directory(&state, |resources| {
        if !resources.directory.groups.contains_key(&id) {
            return Err(error_json(404, "Group not found"));
        }
        if let Some(display) = display_name {
            let clash = resources
                .directory
                .groups
                .iter()
                .any(|(gid, g)| gid != &id && g.display_name.to_lowercase() == display.to_lowercase());
            if clash {
                return Err(error_json(409, "Group with the same name already exists"));
            }
            if let Some(group) = resources.directory.groups.get_mut(&id) {
                group.display_name = display.to_string();
            }
        }
        let value =
            group_json(&id, resources.directory.groups.get(&id).expect("group exists above"), &resources.directory);
        Ok(value)
    })
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    state.record_audit_entry(
        &auth.subject,
        "group.updated",
        Some(id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"group": value})).into_response()
}

/// `DELETE /api/v3/groups/{id}` — удаляет группу и членство во всех профилях.
pub async fn delete_group(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
) -> Response {
    track("groups.delete");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    if let Err(response) = commit_directory(&state, |resources| {
        if !resources.directory.groups.contains_key(&id) {
            return Err(error_json(404, "Group not found"));
        }
        resources.directory.groups.remove(&id);
        for user in resources.directory.users.values_mut() {
            user.groups.retain(|g| g != &id);
        }
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        "group.deleted",
        Some(id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

/// `POST /api/v3/users/{id}/groups/{group_id}` — добавить в группу.
pub async fn add_user_to_group(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, group_id)): Path<(String, String)>,
) -> Response {
    track("users.groups.add");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    change_group_membership(state, auth, client_ip, user_id, group_id, true).await
}

/// `DELETE /api/v3/users/{id}/groups/{group_id}` — убрать из группы.
pub async fn remove_user_from_group(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, group_id)): Path<(String, String)>,
) -> Response {
    track("users.groups.remove");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    change_group_membership(state, auth, client_ip, user_id, group_id, false).await
}

async fn change_group_membership(
    state: SharedState,
    auth: AuthContext,
    client_ip: Option<Extension<ClientIp>>,
    user_id: String,
    group_id: String,
    add: bool,
) -> Response {
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    if let Err(response) = commit_directory(&state, |resources| {
        if !resources.directory.groups.contains_key(&group_id) {
            return Err(error_json(404, "Group not found"));
        }
        let profile = ensure_profile(&mut resources.directory, &user);
        if add {
            if !profile.groups.iter().any(|g| g == &group_id) {
                profile.groups.push(group_id.clone());
            }
        } else {
            profile.groups.retain(|g| g != &group_id);
        }
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        if add { "user.group.added" } else { "user.group.removed" },
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}
