//! Роли справочника PNM (T2.1): built-in/custom роли и назначение пользователям.

use std::collections::HashMap;

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use super::super::auth::ClientIp;
use super::super::resources;
use super::super::{error_json, sort_and_page, track, ListParams};
use super::{
    all_role_ids, caller_id, commit_directory, default_role_permissions, default_roles, find_user, id_list,
    internal_role, json_body, now_secs, require_manage, require_read, role_ids_for, role_json, role_name,
    DirectoryRole, ADMIN_ROLE_ID,
};
use crate::auth_middleware::AuthContext;
use crate::state::SharedState;
use crate::users::Users;
use vks_core::{Role, UserId};

// --- Роли -----------------------------------------------------------------

/// `GET /api/v3/roles` → `{count, roles}`.
pub async fn list_roles(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    track("roles.list");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let params = Value::Object(params.into_iter().map(|(k, v)| (k, Value::String(v))).collect());
    let list = ListParams::parse(&params);
    let user_id = params.get("user_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let search = params
        .get("search")
        .or_else(|| params.get("display_name"))
        .and_then(Value::as_str)
        .unwrap_or(&list.search)
        .to_lowercase();
    // Роли, которые уже назначены указанным пользователям, не предлагаются в
    // попапе назначения (`users_exclude`).
    let exclude_users = id_list(&params, &["users_exclude", "exclude_users"]);
    let resources = state.api_v3.resources.lock().await;
    let users = Users::list(&state);
    let target_roles = if user_id.is_empty() {
        None
    } else {
        match find_user(&state, &user_id) {
            Some(user) => Some(role_ids_for(&resources.directory, &user)),
            None => return error_json(404, "User not found"),
        }
    };
    let excluded_roles: Vec<String> = exclude_users
        .iter()
        .filter_map(|uid| find_user(&state, uid))
        .flat_map(|user| role_ids_for(&resources.directory, &user))
        .collect();
    let roles: Vec<Value> = all_role_ids(&resources.directory)
        .into_iter()
        .filter(|id| {
            target_roles.as_ref().map(|roles| roles.iter().any(|r| r == id)).unwrap_or(true)
                && !excluded_roles.iter().any(|r| r == id)
        })
        .filter_map(|id| role_json(&resources.directory, &users, &id))
        .filter(|value| {
            search.is_empty()
                || value["display_name"].as_str().unwrap_or_default().to_lowercase().contains(&search)
                || value["description"].as_str().unwrap_or_default().to_lowercase().contains(&search)
        })
        .collect();
    let (count, roles) = sort_and_page(roles, &search, &list.sort_field, list.sort_order, list.page, list.page_size);
    Json(json!({"count": count, "roles": roles})).into_response()
}

/// `GET /api/v3/roles/{id}` → `{role: {...}}` (включая `permissions`).
pub async fn get_role(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("roles.get");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let resources = state.api_v3.resources.lock().await;
    let users = Users::list(&state);
    match role_json(&resources.directory, &users, &id) {
        Some(role) => Json(json!({"role": role})).into_response(),
        None => error_json(404, "Role not found"),
    }
}

/// `POST /api/v3/roles` — создать кастомную роль.
pub async fn create_role(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("roles.create");
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
    let mut permissions: Vec<String> = body["permissions"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    if permissions.is_empty() {
        permissions = default_role_permissions("custom");
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Err(response) = commit_directory(&state, |resources| {
        if resources.directory.roles.values().any(|r| r.display_name.to_lowercase() == display_name.to_lowercase())
            || default_roles().iter().any(|(_, name, _)| name.to_lowercase() == display_name.to_lowercase())
        {
            return Err(error_json(409, "Role with the same name already exists"));
        }
        resources.directory.roles.insert(
            id.clone(),
            DirectoryRole {
                display_name: display_name.to_string(),
                description: body["description"].as_str().map(str::to_string).unwrap_or_default(),
                permissions,
                created_at: now_secs(),
                creator_id: Some(auth.subject.clone()),
                ..DirectoryRole::default()
            },
        );
        Ok(())
    })
    .await
    {
        return response;
    }
    let role = {
        let resources = state.api_v3.resources.lock().await;
        let users = Users::list(&state);
        role_json(&resources.directory, &users, &id)
    };
    state.record_audit_entry(
        &auth.subject,
        "role.created",
        Some(id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    (StatusCode::CREATED, Json(json!({"role": role}))).into_response()
}

/// `PATCH /api/v3/roles/{id}` — переименовать/описать/изменить права кастомной
/// роли. Built-in роли неизменяемы по имени/правам (vendor `Undeletable`), но
/// описание отредактировать можно.
pub async fn update_role(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("roles.update");
    let body = match json_body(payload) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let builtin = default_roles().iter().any(|(rid, _, _)| rid == &id);
    let display_name = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty());
    if body.get("display_name").is_some() && display_name.is_none() {
        return error_json(400, "display_name must not be empty");
    }
    if display_name.is_some_and(|d| d.chars().count() > 128) {
        return error_json(400, "display_name is too long");
    }
    if let Err(response) = commit_directory(&state, |resources| {
        if builtin {
            let Some((_, name, _)) = default_roles().into_iter().find(|(rid, _, _)| rid == &id) else {
                return Err(error_json(404, "Role not found"));
            };
            if display_name.is_some_and(|d| d != name) {
                return Err(error_json(403, "Built-in role cannot be renamed"));
            }
        } else if !resources.directory.roles.contains_key(&id) {
            return Err(error_json(404, "Role not found"));
        }
        if !builtin {
            if let Some(display) = display_name {
                let clash = all_role_ids(&resources.directory).into_iter().any(|rid| {
                    rid != id
                        && role_json(&resources.directory, &Users::list(&state), &rid)
                            .and_then(|role| role["display_name"].as_str().map(|n| n.to_lowercase()))
                            .is_some_and(|n| n == display.to_lowercase())
                });
                if clash {
                    return Err(error_json(409, "Role with the same name already exists"));
                }
                if let Some(role) = resources.directory.roles.get_mut(&id) {
                    role.display_name = display.to_string();
                }
            }
            if body.get("description").is_some() {
                if let Some(role) = resources.directory.roles.get_mut(&id) {
                    role.description = body["description"].as_str().map(str::to_string).unwrap_or_default();
                }
            }
            if let Some(permissions) = body["permissions"].as_array() {
                let permissions: Vec<String> =
                    permissions.iter().filter_map(Value::as_str).map(str::to_string).collect();
                if let Some(role) = resources.directory.roles.get_mut(&id) {
                    role.permissions = permissions;
                }
            }
        } else if body.get("description").is_some() || body.get("permissions").is_some() {
            // Built-in роль editable: сохраняем переопределение описания/прав, имя
            // остаётся каноническим. Пустое описание возвращает vendor-текст.
            let Some((_, name, default_description)) = default_roles().into_iter().find(|(rid, _, _)| rid == &id)
            else {
                return Err(error_json(404, "Role not found"));
            };
            let entry = resources.directory.roles.entry(id.clone()).or_insert_with(|| DirectoryRole {
                display_name: name.to_string(),
                description: default_description.to_string(),
                permissions: default_role_permissions(&id),
                created_at: now_secs(),
                creator_id: Some(auth.subject.clone()),
                ..DirectoryRole::default()
            });
            entry.display_name = name.to_string();
            if body.get("description").is_some() {
                entry.description = body["description"].as_str().unwrap_or_default().to_string();
                if entry.description.is_empty() {
                    entry.description = default_description.to_string();
                }
            }
            if let Some(permissions) = body["permissions"].as_array() {
                entry.permissions = permissions.iter().filter_map(Value::as_str).map(str::to_string).collect();
                if entry.permissions.is_empty() {
                    entry.permissions = default_role_permissions(&id);
                }
            }
        }
        Ok(())
    })
    .await
    {
        return response;
    }
    let role = {
        let resources = state.api_v3.resources.lock().await;
        let users = Users::list(&state);
        role_json(&resources.directory, &users, &id)
    };
    state.record_audit_entry(
        &auth.subject,
        "role.updated",
        Some(id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"role": role})).into_response()
}

/// `DELETE /api/v3/roles/{id}` — удалить кастомную роль и снять её со всех
/// пользователей. Built-in роли неудаляемы.
pub async fn delete_role(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
) -> Response {
    track("roles.delete");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    if default_roles().iter().any(|(rid, _, _)| rid == &id) {
        return error_json(403, "Built-in role cannot be deleted");
    }
    let mut updates: Vec<(UserId, Role)> = Vec::new();
    {
        let mut resources = state.api_v3.resources.lock().await;
        if !resources.directory.roles.contains_key(&id) {
            return error_json(404, "Role not found");
        }
        let previous = resources.clone();
        resources.directory.roles.remove(&id);
        for user in Users::list(&state) {
            if let Some(profile) = resources.directory.users.get_mut(&user.id.to_string()) {
                if profile.roles.iter().any(|r| r == &id) {
                    // Материализуем неявную built-in роль только у тех, у кого
                    // удаляемая роль реально была: остальным профилям не меняем
                    // состав ролей (никакого churn'а).
                    if let Some(builtin) = role_name(user.role) {
                        if !profile.roles.iter().any(|role| role == builtin) {
                            profile.roles.push(builtin.to_string());
                        }
                    }
                    profile.roles.retain(|r| r != &id);
                    let new_internal = internal_role(&profile.roles);
                    if new_internal != user.role {
                        updates.push((user.id, new_internal));
                    }
                }
            }
        }
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
    }
    for (user_id, role) in updates {
        if let Err(error) = Users::update_async(&state, &user_id, None, Some(role)).await {
            tracing::error!(error = %error, "directory: role cleanup persistence failed");
            // Роль уже удалена из профилей; без обновления internal-роли часть
            // пользователей сохранит повышенные права до рестарта — не 200.
            return error_json(500, "Storage error");
        }
    }
    state.record_audit_entry(
        &auth.subject,
        "role.deleted",
        Some(id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

/// `POST/DELETE /api/v3/users/{id}/roles/{role_id}`.
pub async fn add_user_role(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, role_id)): Path<(String, String)>,
) -> Response {
    track("users.roles.add");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    change_role(state, auth, client_ip, user_id, role_id, true).await
}

pub async fn remove_user_role(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path((user_id, role_id)): Path<(String, String)>,
) -> Response {
    track("users.roles.remove");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    change_role(state, auth, client_ip, user_id, role_id, false).await
}

async fn change_role(
    state: SharedState,
    auth: AuthContext,
    client_ip: Option<Extension<ClientIp>>,
    user_id: String,
    role_id: String,
    add: bool,
) -> Response {
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    // Снять Administrator нельзя только с встроенного администратора
    // (`BOOTSTRAP_ADMIN_USER_ID`); у обычных пользователей vendor-UI предлагает
    // удаление чипа роли (`isDeletable=true`), поэтому 403 для всех был регрессией.
    let self_id = caller_id(&state, &auth.subject);
    if !add
        && role_id == ADMIN_ROLE_ID
        && (user.id == vks_core::UserId::bootstrap_operator() || self_id.as_deref() == Some(user_id.as_str()))
    {
        return error_json(403, "The Administrator role cannot be removed from the built-in administrator or yourself");
    }
    let roles;
    let new_internal;
    {
        let mut resources = state.api_v3.resources.lock().await;
        if !all_role_ids(&resources.directory).iter().any(|id| id == &role_id) {
            return error_json(404, "Role not found");
        }
        let previous = resources.clone();
        let mut profile = resources
            .directory
            .users
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| super::fallback_profile(&user, now_secs()));
        // Материализуем неявную built-in роль перед изменением: профиль без неё
        // представляет роль из `Users` (Administrator/Operator), и без
        // материализации добавление Operator понижало бы Administrator.
        if let Some(current) = role_name(user.role) {
            if !profile.roles.iter().any(|role| role == current) {
                profile.roles.push(current.to_string());
            }
        }
        if add {
            if !profile.roles.iter().any(|r| r == &role_id) {
                profile.roles.push(role_id.clone());
            }
        } else {
            profile.roles.retain(|r| r != &role_id);
        }
        roles = profile.roles.clone();
        new_internal = internal_role(&roles);
        // Сначала идентичность, затем профиль: при сбое второго шага роль в
        // `Users` откатывается, чтобы они не разошлись.
        if new_internal != user.role {
            if let Err(error) = Users::update_async(&state, &user.id, None, Some(new_internal)).await {
                tracing::error!(error = %error, "directory: role update persistence failed");
                return error_json(500, "Storage error");
            }
        }
        resources.directory.users.insert(user_id.clone(), profile);
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            if new_internal != user.role {
                let _ = Users::update_async(&state, &user.id, None, Some(user.role)).await;
            }
            return response;
        }
    }
    state.record_audit_entry(
        &auth.subject,
        if add { "user.role.added" } else { "user.role.removed" },
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok", "roles": roles})).into_response()
}
