//! Справочник PNM (T2.1): пользователи, группы, роли, учётные записи и личные
//! адресные книги.
//!
//! Источники истины:
//! - идентичность и доступ — `AppState.users` (+durable `Storage::users`);
//! - профиль/группы/роли/личные адреса — `ResourceState.directory` (тот же
//!   configuration-document, что ресурсы конференций, т.е. переживают рестарт);
//! - логин/пароль — `LocalDb` (+durable `Storage::user_credentials`, M1.3).
//!
//! Контракты сняты с vendor-оболочки 2.2.1.66 (`index.bundle`, `index2.bundle`):
//! `POST/PATCH/DELETE /users`, `GET /users/{id}`, WS `ui:users:search`,
//! `GET/POST/PATCH/DELETE /groups`, `GET /roles?user_id=`, членство
//! `POST/DELETE /users/{u}/groups/{g}` и `/users/{u}/roles/{r}`,
//! `GET/POST/PATCH/DELETE /users/{u}/credentials`, `GET/PATCH /profile/credentials`,
//! `POST /ui/users/{u}/call_strings`, `POST/DELETE /users/{u}/call_strings/{a}`.

use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use vks_core::ports::storage::UserCredentials;
use vks_core::{Role, UserId};

use super::auth::ClientIp;
use super::call_strings::default_contact_payload;
use super::resources::{self, AddressRecord};
use super::{error_json, track, ListParams};
use crate::auth_middleware::AuthContext;
use crate::state::SharedState;
use crate::users::Users;

/// Роль «Administrator» (vendor default role, undeletable).
pub const ADMIN_ROLE_ID: &str = "770fada3-71d8-4176-a969-e12ec2059e64";
/// Роль «Operator» (vendor default role, undeletable).
pub const OPERATOR_ROLE_ID: &str = "425eda58-e25d-4418-b924-5a490be734ba";
const NIL_ID: &str = "00000000-0000-0000-0000-000000000000";

fn default_roles() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (ADMIN_ROLE_ID, "Administrator", "Default role with all privileges. Undeletable."),
        (OPERATOR_ROLE_ID, "Operator", "Default role with privileges to manage conferences. Undeletable."),
    ]
}

fn default_profile(display_name: &str) -> DirectoryUser {
    DirectoryUser {
        display_name: display_name.to_string(),
        locale_id: Some("ru_RU".to_string()),
        time_zone_id: Some("UTC".to_string()),
        ..DirectoryUser::default()
    }
}

/// Профиль пользователя в справочнике (то, что не входит в идентичность).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct DirectoryUser {
    pub display_name: String,
    pub description: Option<String>,
    pub locale_id: Option<String>,
    pub time_zone_id: Option<String>,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub view_own_only: bool,
    pub created_at: u64,
    pub creator_id: Option<String>,
    pub groups: Vec<String>,
    pub roles: Vec<String>,
    /// Логин учётной записи (если создана); для bootstrap-админа `None`.
    pub login: Option<String>,
}

/// Группа пользователей (`GET /groups`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct DirectoryGroup {
    pub display_name: String,
    pub created_at: u64,
    pub creator_id: Option<String>,
}

/// Справочник в `ResourceState`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DirectoryState {
    pub users: BTreeMap<String, DirectoryUser>,
    pub groups: BTreeMap<String, DirectoryGroup>,
    /// Личные адресные книги: user_id → address_id → запись.
    pub user_addresses: BTreeMap<String, BTreeMap<String, AddressRecord>>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn role_name(role: Role) -> Option<&'static str> {
    match role {
        Role::Operator => Some(ADMIN_ROLE_ID),
        Role::Moderator => Some(OPERATOR_ROLE_ID),
        _ => None,
    }
}

/// Внутренняя роль доступа по набору vendor-ролей (Administrator > Operator > остальные).
fn internal_role(roles: &[String]) -> Role {
    if roles.iter().any(|r| r == ADMIN_ROLE_ID) {
        Role::Operator
    } else if roles.iter().any(|r| r == OPERATOR_ROLE_ID) {
        Role::Moderator
    } else {
        Role::Participant
    }
}

fn can_read(auth: &AuthContext) -> bool {
    auth.role >= Role::Moderator
}

fn can_manage(auth: &AuthContext) -> bool {
    auth.role >= Role::Operator && auth.room_scope.is_none()
}

fn require_manage(auth: &AuthContext) -> Option<Response> {
    if can_manage(auth) {
        None
    } else {
        Some(error_json(403, "Forbidden: Operator role required to manage the directory"))
    }
}

fn require_read(auth: &AuthContext) -> Option<Response> {
    if can_read(auth) {
        None
    } else {
        Some(error_json(403, "Forbidden: insufficient permissions"))
    }
}

fn find_user(state: &crate::state::AppState, id: &str) -> Option<crate::state::UserRecord> {
    let users = state.users.lock().expect("users store poisoned");
    users.values().find(|u| !u.deleted && (u.id.to_string() == id || u.username == id)).cloned()
}

fn profile<'a>(directory: &'a DirectoryState, user: &crate::state::UserRecord) -> Option<&'a DirectoryUser> {
    directory.users.get(&user.id.to_string())
}

fn sorted_users(state: &crate::state::AppState, directory: &DirectoryState) -> Vec<(crate::state::UserRecord, Value)> {
    let mut rows: Vec<_> = Users::list(state)
        .into_iter()
        .map(|user| {
            let role = user.role;
            let fallback;
            let profile = match profile(directory, &user) {
                Some(profile) => profile,
                None => {
                    fallback = DirectoryUser {
                        display_name: default_display_name(&user),
                        created_at: 0,
                        roles: role_name(role).map(|r| vec![r.to_string()]).unwrap_or_default(),
                        ..DirectoryUser::default()
                    };
                    &fallback
                }
            };
            let mut value = profile_json(state, directory, profile, role, false);
            value["id"] = json!(user.id.to_string());
            (user, value)
        })
        .collect();
    rows.sort_by(|a, b| {
        a.1["display_name"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .cmp(&b.1["display_name"].as_str().unwrap_or_default().to_lowercase())
    });
    rows
}

fn default_display_name(user: &crate::state::UserRecord) -> String {
    if user.id == UserId::bootstrap_operator() {
        "Administrator".to_string()
    } else {
        user.username.clone()
    }
}

fn role_ids_for(directory: &DirectoryState, user: &crate::state::UserRecord) -> Vec<String> {
    let mut roles = profile(directory, user).map(|p| p.roles.clone()).unwrap_or_default();
    if roles.is_empty() {
        if let Some(id) = role_name(user.role) {
            roles.push(id.to_string());
        }
    }
    roles
}

fn profile_json(
    state: &crate::state::AppState,
    directory: &DirectoryState,
    profile: &DirectoryUser,
    role: Role,
    detail: bool,
) -> Value {
    let groups: Vec<Value> = profile
        .groups
        .iter()
        .filter_map(|gid| directory.groups.get(gid).map(|g| group_json(gid, g, directory)))
        .collect();
    let mut value = json!({
        "display_name": if profile.display_name.is_empty() { "User" } else { &profile.display_name },
        "description": profile.description,
        "view_own_only": profile.view_own_only,
        "locale_id": profile.locale_id,
        "time_zone_id": profile.time_zone_id,
        "creator_id": profile.creator_id,
        "created_at": profile.created_at,
        "phone": profile.phones.first(),
        "email": profile.emails.first(),
        "groups": groups,
        "address": Value::Null,
        "payload": Value::Null,
        "defaults": {"editable": true, "deletable": false, "disableable": false},
        "terminal": Value::Null,
        "busy": false,
    });
    if detail {
        let role_ids: Vec<String> = if profile.roles.is_empty() {
            role_name(role).map(|r| vec![r.to_string()]).unwrap_or_default()
        } else {
            profile.roles.clone()
        };
        let users = Users::list(state);
        let roles: Vec<Value> = role_ids
            .iter()
            .filter_map(|rid| {
                let (name, description) = match rid.as_str() {
                    ADMIN_ROLE_ID => ("Administrator", "Default role with all privileges. Undeletable."),
                    OPERATOR_ROLE_ID => {
                        ("Operator", "Default role with privileges to manage conferences. Undeletable.")
                    }
                    _ => return None,
                };
                let size = users.iter().filter(|user| role_ids_for(directory, user).iter().any(|r| r == rid)).count();
                Some(json!({
                    "id": rid, "display_name": name, "description": description,
                    "created_at": 0, "creator_id": Value::Null,
                    "defaults": {"editable": true, "deletable": false, "disableable": false},
                    "size": size,
                }))
            })
            .collect();
        value["emails"] = json!(if profile.emails.is_empty() { vec![String::new()] } else { profile.emails.clone() });
        value["phones"] = json!(if profile.phones.is_empty() { vec![String::new()] } else { profile.phones.clone() });
        value["avatars"] = json!([]);
        value["roles"] = json!(roles);
        value["permissions"] = json!(if role >= Role::Operator { vec![json!("all")] } else { Vec::<Value>::new() });
    }
    value
}

fn group_json(id: &str, group: &DirectoryGroup, directory: &DirectoryState) -> Value {
    let size = directory.users.values().filter(|u| u.groups.iter().any(|g| g == id)).count();
    json!({
        "id": id,
        "display_name": group.display_name,
        "size": size,
        "creator_id": group.creator_id,
        "created_at": group.created_at,
    })
}

/// `GET /api/v3/users` — список пользователей (HTTP-аналог WS `ui:users:search`).
pub async fn list_users(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    track("users.list");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    let params = Value::Object(params.into_iter().map(|(k, v)| (k, Value::String(v))).collect());
    let resources = state.api_v3.resources.lock().await;
    match filtered_users(&state, &resources.directory, &params) {
        Ok((count, users)) => Json(json!({"count": count, "users": users})).into_response(),
        Err(response) => response,
    }
}

// `Response` — крупный тип, но HTTP-хендлеры всё равно возвращают его; ошибка
// здесь не горячий путь.
#[allow(clippy::result_large_err)]
fn filtered_users(
    state: &crate::state::AppState,
    directory: &DirectoryState,
    params: &Value,
) -> Result<(usize, Vec<Value>), Response> {
    let list = ListParams::parse(params);
    let search = params
        .get("search")
        .or_else(|| params.get("query"))
        .and_then(Value::as_str)
        .unwrap_or(&list.search)
        .to_lowercase();
    let group_id = params.get("group_id").and_then(Value::as_str).unwrap_or_default();
    let role_id = params.get("role_id").and_then(Value::as_str).unwrap_or_default();
    let mut rows: Vec<Value> = sorted_users(state, directory)
        .into_iter()
        .filter(|(user, value)| {
            if !search.is_empty()
                && !value["display_name"].as_str().unwrap_or_default().to_lowercase().contains(&search)
            {
                return false;
            }
            if !group_id.is_empty() {
                let groups = profile(directory, user).map(|p| p.groups.clone()).unwrap_or_default();
                if group_id == NIL_ID {
                    if !groups.is_empty() {
                        return false;
                    }
                } else if !groups.iter().any(|g| g == group_id) {
                    return false;
                }
            }
            if !role_id.is_empty() {
                let roles = role_ids_for(directory, user);
                if !roles.iter().any(|r| r == role_id) {
                    return false;
                }
            }
            true
        })
        .map(|(_, value)| value)
        .collect();
    rows.sort_by(|a, b| {
        let cmp = if list.sort_field == "created_at" {
            a["created_at"].as_u64().unwrap_or(0).cmp(&b["created_at"].as_u64().unwrap_or(0))
        } else {
            a["display_name"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .cmp(&b["display_name"].as_str().unwrap_or_default().to_lowercase())
        };
        if list.sort_order == 1 {
            cmp.reverse()
        } else {
            cmp
        }
    });
    let count = rows.len();
    let start = list.page.saturating_sub(1).saturating_mul(list.page_size);
    let users = rows.into_iter().skip(start).take(list.page_size.max(1)).collect();
    Ok((count, users))
}

fn user_detail(state: &crate::state::AppState, directory: &DirectoryState, user: &crate::state::UserRecord) -> Value {
    let fallback;
    let profile = match profile(directory, user) {
        Some(profile) => profile,
        None => {
            fallback = DirectoryUser {
                display_name: default_display_name(user),
                roles: role_name(user.role).map(|r| vec![r.to_string()]).unwrap_or_default(),
                ..DirectoryUser::default()
            };
            &fallback
        }
    };
    let mut value = profile_json(state, directory, profile, user.role, true);
    value["id"] = json!(user.id.to_string());
    value["busy"] = json!(false);
    value
}

/// `POST /api/v3/users` → `{user: {...}}`.
pub async fn create_user(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Json(body): Json<Value>,
) -> Response {
    track("users.create");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let display_name = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or_default();
    if display_name.is_empty() {
        return error_json(400, "display_name is required");
    }
    if display_name.chars().count() > 255 {
        return error_json(400, "display_name is too long");
    }
    let id = UserId::new();
    let mut profile = default_profile(display_name);
    profile.created_at = now_secs();
    profile.creator_id = Some(auth.subject.clone());
    profile.description = body["description"].as_str().map(str::to_string);
    profile.view_own_only = body["view_own_only"].as_bool().unwrap_or(false);
    if let Some(locale) = body["locale_id"].as_str().filter(|s| !s.is_empty()) {
        profile.locale_id = Some(locale.to_string());
    }
    if let Some(tz) = body["time_zone_id"].as_str().filter(|s| !s.is_empty()) {
        profile.time_zone_id = Some(tz.to_string());
    }
    profile.emails = string_list(body.get("emails"));
    profile.phones = string_list(body.get("phones"));
    if let Err(error) = Users::create_with_id_async(&state, id, display_name, Role::Participant).await {
        tracing::error!(error = %error, "directory: create user persistence failed");
        return error_json(500, "Storage error");
    }
    let user = {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        resources.directory.users.insert(id.to_string(), profile);
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
        let Some(user) = find_user(&state, &id.to_string()) else {
            return error_json(500, "User not found after creation");
        };
        user_detail(&state, &resources.directory, &user)
    };
    state.record_audit_entry(
        &auth.subject,
        "user.created",
        Some(id.to_string()),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    (StatusCode::CREATED, Json(json!({"user": user}))).into_response()
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `GET /api/v3/users/{id}` → `{user: {...}}` (детальная карточка).
pub async fn get_user(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("users.get");
    if let Some(response) = require_read(&auth) {
        return response;
    }
    if id == NIL_ID {
        return Json(json!({"user": {"id": id, "display_name": "Неизвестный создатель", "emails": [], "phones": [], "avatars": []}})).into_response();
    }
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    let resources = state.api_v3.resources.lock().await;
    Json(json!({"user": user_detail(&state, &resources.directory, &user)})).into_response()
}

/// `PATCH /api/v3/users/{id}` — редактирование профиля.
pub async fn update_user(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    track("users.update");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    let new_display = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty());
    if body.get("display_name").is_some() && new_display.is_none() {
        return error_json(400, "display_name must not be empty");
    }
    let detail = {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        let profile = resources.directory.users.entry(user_id.clone()).or_insert_with(|| DirectoryUser {
            display_name: default_display_name(&user),
            created_at: now_secs(),
            ..DirectoryUser::default()
        });
        if let Some(display) = new_display {
            profile.display_name = display.to_string();
        }
        if body.get("description").is_some() {
            profile.description = body["description"].as_str().map(str::to_string);
        }
        if body.get("view_own_only").is_some() {
            profile.view_own_only = body["view_own_only"].as_bool().unwrap_or(false);
        }
        if body.get("locale_id").is_some() {
            profile.locale_id = body["locale_id"].as_str().map(str::to_string);
        }
        if body.get("time_zone_id").is_some() {
            profile.time_zone_id = body["time_zone_id"].as_str().map(str::to_string);
        }
        if body.get("emails").is_some() {
            profile.emails = string_list(body.get("emails"));
        }
        if body.get("phones").is_some() {
            profile.phones = string_list(body.get("phones"));
        }
        let display = profile.display_name.clone();
        let login = profile.login.clone();
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
        // До создания логина внутреннее имя совпадает с display_name.
        if login.is_none() && display != user.username {
            if let Err(error) = Users::update_async(&state, &user.id, Some(display), None).await {
                tracing::error!(error = %error, "directory: rename user persistence failed");
                return error_json(500, "Storage error");
            }
        }
        let Some(updated) = find_user(&state, &user_id) else {
            return error_json(404, "User not found");
        };
        user_detail(&state, &resources.directory, &updated)
    };
    state.record_audit_entry(
        &auth.subject,
        "user.updated",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"user": detail})).into_response()
}

/// `DELETE /api/v3/users/{id}` — soft-delete (durable через `Users::delete_async`).
pub async fn delete_user(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
) -> Response {
    track("users.delete");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    if user.id.to_string() == vks_core::BOOTSTRAP_ADMIN_USER_ID {
        return error_json(403, "The built-in administrator cannot be deleted");
    }
    if user.id.to_string() == auth.subject {
        return error_json(403, "You cannot delete your own account");
    }
    match Users::delete_async(&state, &user.id).await {
        Ok(true) => {
            state.record_audit_entry(
                &auth.subject,
                "user.deleted",
                Some(user.id.to_string()),
                client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
            );
            Json(json!({"status": "ok"})).into_response()
        }
        Ok(false) => error_json(404, "User not found"),
        Err(error) => {
            tracing::error!(error = %error, "directory: delete user persistence failed");
            error_json(500, "Storage error")
        }
    }
}

// --- Группы ---------------------------------------------------------------

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
    let search = params.get("search").and_then(Value::as_str).unwrap_or(&list.search).to_lowercase();
    let resources = state.api_v3.resources.lock().await;
    let mut groups: Vec<Value> = resources
        .directory
        .groups
        .iter()
        .map(|(id, group)| group_json(id, group, &resources.directory))
        .filter(|value| {
            search.is_empty() || value["display_name"].as_str().unwrap_or_default().to_lowercase().contains(&search)
        })
        .collect();
    groups.sort_by(|a, b| {
        a["display_name"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .cmp(&b["display_name"].as_str().unwrap_or_default().to_lowercase())
    });
    let count = groups.len();
    let start = list.page.saturating_sub(1).saturating_mul(list.page_size);
    let groups: Vec<Value> = groups.into_iter().skip(start).take(list.page_size.max(1)).collect();
    Json(json!({"count": count, "groups": groups})).into_response()
}

/// `POST /api/v3/groups` → `{group: {...}}`.
pub async fn create_group(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Json(body): Json<Value>,
) -> Response {
    track("groups.create");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let display_name = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or_default();
    if display_name.is_empty() {
        return error_json(400, "display_name is required");
    }
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    if resources.directory.groups.values().any(|g| g.display_name.eq_ignore_ascii_case(display_name)) {
        return error_json(409, "Group with the same name already exists");
    }
    let id = uuid::Uuid::new_v4().to_string();
    let group = DirectoryGroup {
        display_name: display_name.to_string(),
        created_at: now_secs(),
        creator_id: Some(auth.subject.clone()),
    };
    resources.directory.groups.insert(id.clone(), group);
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    let value = resources.directory.groups.get(&id).map(|group| group_json(&id, group, &resources.directory)).unwrap();
    drop(resources);
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
    Json(body): Json<Value>,
) -> Response {
    track("groups.update");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    if !resources.directory.groups.contains_key(&id) {
        return error_json(404, "Group not found");
    }
    let display_name = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty());
    if body.get("display_name").is_some() && display_name.is_none() {
        return error_json(400, "display_name must not be empty");
    }
    if let Some(display) = display_name {
        let clash = resources
            .directory
            .groups
            .iter()
            .any(|(gid, g)| gid != &id && g.display_name.eq_ignore_ascii_case(display));
        if clash {
            return error_json(409, "Group with the same name already exists");
        }
        if let Some(group) = resources.directory.groups.get_mut(&id) {
            group.display_name = display.to_string();
        }
    }
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    let value = resources.directory.groups.get(&id).map(|group| group_json(&id, group, &resources.directory)).unwrap();
    drop(resources);
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
    let mut resources = state.api_v3.resources.lock().await;
    if !resources.directory.groups.contains_key(&id) {
        return error_json(404, "Group not found");
    }
    let previous = resources.clone();
    resources.directory.groups.remove(&id);
    for user in resources.directory.users.values_mut() {
        user.groups.retain(|g| g != &id);
    }
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
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
    let mut resources = state.api_v3.resources.lock().await;
    if !resources.directory.groups.contains_key(&group_id) {
        return error_json(404, "Group not found");
    }
    let previous = resources.clone();
    let profile = resources.directory.users.entry(user_id.clone()).or_insert_with(|| DirectoryUser {
        display_name: default_display_name(&user),
        created_at: now_secs(),
        ..DirectoryUser::default()
    });
    if add {
        if !profile.groups.iter().any(|g| g == &group_id) {
            profile.groups.push(group_id.clone());
        }
    } else {
        profile.groups.retain(|g| g != &group_id);
    }
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    state.record_audit_entry(
        &auth.subject,
        if add { "user.group.added" } else { "user.group.removed" },
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

// --- Роли -----------------------------------------------------------------

/// `GET /api/v3/roles` и `GET /api/v3/roles/{id}`.
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
    let user_id = params.get("user_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let search = params.get("search").and_then(Value::as_str).unwrap_or_default().to_lowercase();
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
    let mut roles: Vec<Value> = default_roles()
        .into_iter()
        .filter(|(id, name, _)| {
            target_roles.as_ref().map(|roles| roles.iter().any(|r| r == id)).unwrap_or(true)
                && (search.is_empty() || name.to_lowercase().contains(&search))
        })
        .map(|(id, name, description)| {
            let size =
                users.iter().filter(|user| role_ids_for(&resources.directory, user).iter().any(|r| r == id)).count();
            json!({
                "id": id, "display_name": name, "description": description,
                "created_at": 0, "creator_id": Value::Null,
                "defaults": {"editable": true, "deletable": false, "disableable": false},
                "size": size,
            })
        })
        .collect();
    roles.sort_by(|a, b| {
        a["display_name"].as_str().unwrap_or_default().cmp(b["display_name"].as_str().unwrap_or_default())
    });
    Json(json!({"count": roles.len(), "roles": roles})).into_response()
}

/// `GET /api/v3/roles/{id}`.
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
    match default_roles().into_iter().find(|(rid, _, _)| rid == &id) {
        Some((id, name, description)) => {
            let size =
                users.iter().filter(|user| role_ids_for(&resources.directory, user).iter().any(|r| r == id)).count();
            Json(json!({"role": {
                "id": id, "display_name": name, "description": description,
                "created_at": 0, "creator_id": Value::Null,
                "defaults": {"editable": true, "deletable": false, "disableable": false},
                "size": size,
            }}))
            .into_response()
        }
        None => error_json(404, "Role not found"),
    }
}

/// `POST /api/v3/users/{id}/roles/{role_id}`.
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

/// `DELETE /api/v3/users/{id}/roles/{role_id}`.
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
    if !default_roles().iter().any(|(id, _, _)| id == &role_id) {
        return error_json(404, "Role not found");
    }
    let Some(user) = find_user(&state, &user_id) else {
        return error_json(404, "User not found");
    };
    let user_id = user.id.to_string();
    if !add && user_id == vks_core::BOOTSTRAP_ADMIN_USER_ID && role_id == ADMIN_ROLE_ID {
        return error_json(403, "The built-in administrator must keep the Administrator role");
    }
    let (roles, new_internal) = {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        let profile = resources.directory.users.entry(user_id.clone()).or_insert_with(|| DirectoryUser {
            display_name: default_display_name(&user),
            created_at: now_secs(),
            roles: role_name(user.role).map(|r| vec![r.to_string()]).unwrap_or_default(),
            ..DirectoryUser::default()
        });
        if add {
            if !profile.roles.iter().any(|r| r == &role_id) {
                profile.roles.push(role_id.clone());
            }
        } else {
            profile.roles.retain(|r| r != &role_id);
        }
        let roles = profile.roles.clone();
        let new_internal = internal_role(&roles);
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
        (roles, new_internal)
    };
    if new_internal != user.role {
        if let Err(error) = Users::update_async(&state, &user.id, None, Some(new_internal)).await {
            tracing::error!(error = %error, "directory: role update persistence failed");
            return error_json(500, "Storage error");
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

// --- Учётные записи -------------------------------------------------------

fn credential_json(login: &str) -> Value {
    json!({"login": login, "expire_at": 0, "block_at": 0})
}

fn local_login(state: &crate::state::AppState, user: &crate::state::UserRecord) -> Option<String> {
    state.local_db.as_ref().and_then(|db| db.user(&user.id.to_string()).ok()).map(|u| u.username)
}

/// `GET /api/v3/users/{id}/credentials`.
pub async fn get_user_credentials(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    track("users.credentials.get");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    match local_login(&state, &user) {
        Some(login) => Json(credential_json(&login)).into_response(),
        None => error_json(404, "Credentials not found"),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct CredentialsBody {
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default, alias = "newPassword", alias = "new_password")]
    pub new_password: Option<String>,
}

/// `POST /api/v3/users/{id}/credentials` — создать логин/пароль.
pub async fn create_user_credentials(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    payload: Result<Json<CredentialsBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    track("users.credentials.create");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Json(body) = match payload {
        Ok(body) => body,
        Err(rejection) => return error_json(400, &format!("Invalid credentials body: {rejection}")),
    };
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    let Some(db) = state.local_db.clone() else {
        return error_json(503, "Authentication is not configured");
    };
    if db.user(&user.id.to_string()).is_ok() {
        return error_json(409, "Credentials already exist");
    }
    let login = body.login.as_deref().map(str::trim).unwrap_or_default();
    let password = body.password.as_deref().or(body.new_password.as_deref()).unwrap_or_default();
    if login.is_empty() {
        return error_json(400, "login is required");
    }
    if login.chars().count() < 3 {
        return error_json(400, "login is too short");
    }
    if password.is_empty() {
        return error_json(400, "password is required");
    }
    if db.user_by_name(login).is_ok() {
        return error_json(409, "Login already exists");
    }
    let user_id = user.id.to_string();
    if let Err(error) = db.register(&user_id, login, password) {
        tracing::warn!(error = %error, "directory: local credential registration failed");
        return error_json(409, "Login already exists");
    }
    if let Err(error) = persist_credentials(&state, &user.id).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    if let Err(error) = Users::update_async(&state, &user.id, Some(login.to_string()), None).await {
        tracing::error!(error = %error, "directory: login rename persistence failed");
        return error_json(500, "Storage error");
    }
    {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        resources
            .directory
            .users
            .entry(user_id.clone())
            .or_insert_with(|| DirectoryUser {
                display_name: default_display_name(&user),
                created_at: now_secs(),
                ..DirectoryUser::default()
            })
            .login = Some(login.to_string());
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
    }
    state.record_audit_entry(
        &auth.subject,
        "user.credentials.created",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    (StatusCode::CREATED, Json(credential_json(login))).into_response()
}

async fn persist_credentials(state: &crate::state::AppState, user_id: &UserId) -> Result<(), String> {
    let Some(db) = state.local_db.as_ref() else { return Ok(()) };
    let user = db.user(&user_id.to_string()).map_err(|e| e.to_string())?;
    if let Some(storage) = &state.storage {
        storage
            .upsert_user_credentials(&UserCredentials {
                user_id: *user_id,
                password_hash_hex: user.password_hash_hex,
                salt_hex: user.salt_hex,
                password_changed_at: Some(db.password_changed_at(&user_id.to_string())),
                generation: db.generation(&user_id.to_string()),
            })
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `PATCH /api/v3/users/{id}/credentials` — смена пароля.
pub async fn update_user_credentials(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    Json(body): Json<CredentialsBody>,
) -> Response {
    track("users.credentials.update");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    let Some(db) = state.local_db.clone() else {
        return error_json(503, "Authentication is not configured");
    };
    let Ok(current) = db.user(&user.id.to_string()) else {
        return error_json(404, "Credentials not found");
    };
    if let Some(login) = body.login.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if login != current.username {
            return error_json(409, "Login cannot be changed");
        }
    }
    let password = body.password.as_deref().or(body.new_password.as_deref()).unwrap_or_default();
    if password.is_empty() {
        return error_json(400, "password is required");
    }
    if let Err(error) = db.set_password_async(&current.username, password).await {
        tracing::warn!(error = %error, "directory: password update failed");
        return error_json(500, "Internal error");
    }
    if let Err(error) = persist_credentials(&state, &user.id).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    let generation = db.generation(&user.id.to_string());
    state.notify_revocation(user.id.to_string(), generation);
    state.record_audit_entry(
        &auth.subject,
        "user.credentials.updated",
        Some(user.id.to_string()),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

/// `DELETE /api/v3/users/{id}/credentials` — удалить логин.
pub async fn delete_user_credentials(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
) -> Response {
    track("users.credentials.delete");
    if let Some(response) = require_manage(&auth) {
        return response;
    }
    let Some(user) = find_user(&state, &id) else {
        return error_json(404, "User not found");
    };
    if user.id.to_string() == vks_core::BOOTSTRAP_ADMIN_USER_ID {
        return error_json(403, "The built-in administrator credentials cannot be removed");
    }
    let user_id = user.id.to_string();
    let Some(db) = state.local_db.clone() else {
        return error_json(503, "Authentication is not configured");
    };
    if !db.remove_user(&user_id) {
        return error_json(404, "Credentials not found");
    }
    if let Some(storage) = &state.storage {
        if let Err(error) = storage.delete_user_credentials(user.id).await {
            tracing::error!(error = %error, "directory: delete credentials persistence failed");
            return error_json(500, "Storage error");
        }
    }
    {
        let mut resources = state.api_v3.resources.lock().await;
        if let Some(profile) = resources.directory.users.get_mut(&user_id) {
            profile.login = None;
        }
        let previous = resources.clone();
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
    }
    let display = {
        let resources = state.api_v3.resources.lock().await;
        resources
            .directory
            .users
            .get(&user_id)
            .map(|p| p.display_name.clone())
            .unwrap_or_else(|| default_display_name(&user))
    };
    let _ = Users::update_async(&state, &user.id, Some(display), None).await;
    state.notify_revocation(user_id.clone(), db.generation(&user_id));
    state.record_audit_entry(
        &auth.subject,
        "user.credentials.deleted",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

fn caller_user(state: &crate::state::AppState, auth: &AuthContext) -> Option<crate::state::UserRecord> {
    find_user(state, &auth.subject)
}

/// `GET /api/v3/profile/credentials` — учётная запись текущего пользователя.
pub async fn profile_credentials(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
) -> Response {
    track("profile.credentials.get");
    let Some(user) = caller_user(&state, &auth) else {
        return error_json(404, "User not found");
    };
    match local_login(&state, &user) {
        Some(login) => Json(credential_json(&login)).into_response(),
        None => error_json(404, "Credentials not found"),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ProfileCredentialsBody {
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default, alias = "confirm_password")]
    pub old_password: Option<String>,
    #[serde(default, alias = "newPassword")]
    pub new_password: Option<String>,
}

/// `PATCH /api/v3/profile/credentials` — смена собственного пароля.
pub async fn update_profile_credentials(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Json(body): Json<Value>,
) -> Response {
    track("profile.credentials.update");
    let Some(user) = caller_user(&state, &auth) else {
        return error_json(404, "User not found");
    };
    let Some(db) = state.local_db.clone() else {
        return error_json(503, "Authentication is not configured");
    };
    let old_password = body["old_password"].as_str().or_else(|| body["confirm_password"].as_str()).unwrap_or_default();
    let new_password = body["new_password"].as_str().or_else(|| body["password"].as_str()).unwrap_or_default();
    if body["login"].as_str().unwrap_or_default() != local_login(&state, &user).unwrap_or_default()
        && body.get("login").is_some()
    {
        return error_json(409, "Login cannot be changed");
    }
    if new_password.is_empty() {
        return error_json(400, "new password is required");
    }
    if old_password.is_empty() {
        return error_json(400, "old password is required");
    }
    match db.authenticate_async(&user.username, old_password).await {
        Ok(_) => {}
        Err(_) => return error_json(404, "Incorrect old password"),
    }
    if let Err(error) = db.set_password_async(&user.username, new_password).await {
        tracing::error!(error = %error, "directory: profile password update failed");
        return error_json(500, "Internal error");
    }
    if let Err(error) = persist_credentials(&state, &user.id).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    let generation = db.generation(&user.id.to_string());
    state.notify_revocation(user.id.to_string(), generation);
    state.record_audit_entry(
        &auth.subject,
        "profile.credentials.updated",
        Some(user.id.to_string()),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

// --- Личные адресные книги -------------------------------------------------

fn user_addresses_json(records: impl Iterator<Item = (String, AddressRecord)>) -> Vec<Value> {
    records.map(|(id, address)| super::call_strings::address_value(&id, &address)).collect()
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
    let mut resources = state.api_v3.resources.lock().await;
    let Some(address) = resources.addresses.get(&address_id).cloned() else {
        return error_json(404, "Call string not found");
    };
    let previous = resources.clone();
    resources.directory.user_addresses.entry(user_id.clone()).or_default().insert(address_id.clone(), address);
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
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
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    let removed =
        resources.directory.user_addresses.get_mut(&user_id).and_then(|map| map.remove(&address_id)).is_some();
    if !removed {
        return error_json(404, "Call string not found");
    }
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.detached",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

/// `POST /api/v3/ui/users/{id}/call_strings` — создать адрес сразу в личной книге.
pub async fn create_user_call_string(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(user_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    track("users.call_strings.create");
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
    if !super::call_strings::canonical_uri(uri).contains(':') {
        return error_json(400, "uri must contain a protocol prefix");
    }
    let display_name =
        body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(uri).to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let payload = default_contact_payload(body.get("payload"));
    let address = AddressRecord { id: id.clone(), uri: uri.to_string(), display_name, created_at: now_secs(), payload };
    let user_id = user.id.to_string();
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    resources.directory.user_addresses.entry(user_id.clone()).or_default().insert(id.clone(), address.clone());
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.created",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    (StatusCode::CREATED, Json(json!({"call_string": super::call_strings::address_value(&id, &address)})))
        .into_response()
}

// --- WebSocket ------------------------------------------------------------

/// `ui:users:search` — список пользователей для левой панели справочника.
pub(crate) async fn ws_users_search(
    state: &SharedState,
    subject: &str,
    payload: &Value,
) -> Result<Value, (u16, &'static str)> {
    let role = Users::resolve_role(state, subject).ok_or((403, "Forbidden"))?;
    if role < Role::Operator {
        return Err((403, "Forbidden: insufficient permissions"));
    }
    let resources = state.api_v3.resources.lock().await;
    let (count, users) = filtered_users(state, &resources.directory, payload).map_err(|_| (500, "Internal error"))?;
    Ok(json!({"users": users, "count": count}))
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
    let user_id = {
        let users = state.users.lock().expect("users store poisoned");
        users
            .values()
            .find(|u| !u.deleted && (u.id.to_string() == contact_id || u.username == contact_id))
            .map(|u| u.id.to_string())
            .ok_or((404, "User not found"))?
    };
    let resources = state.api_v3.resources.lock().await;
    let records = resources
        .directory
        .user_addresses
        .get(&user_id)
        .map(|map| map.iter().map(|(id, a)| (id.clone(), a.clone())).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut values = user_addresses_json(records.into_iter());
    let search = payload
        .get("search")
        .or_else(|| payload.get("query"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let uri_filter = payload.get("uri").and_then(Value::as_str).unwrap_or_default().to_lowercase();
    values.retain(|value| {
        let name = value["display_name"].as_str().unwrap_or_default().to_lowercase();
        let uri = value["uri"].as_str().unwrap_or_default().to_lowercase();
        (search.is_empty() || name.contains(&search) || uri.contains(&search))
            && (uri_filter.is_empty() || uri.contains(&uri_filter))
    });
    let count = values.len();
    Ok(json!({"call_strings": values, "count": count}))
}

#[cfg(test)]
#[path = "directory_tests.rs"]
mod directory_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_role_maps_vendor_roles() {
        assert_eq!(internal_role(&[ADMIN_ROLE_ID.to_string()]), Role::Operator);
        assert_eq!(internal_role(&[OPERATOR_ROLE_ID.to_string()]), Role::Moderator);
        assert_eq!(internal_role(&[]), Role::Participant);
        assert_eq!(internal_role(&[ADMIN_ROLE_ID.to_string(), OPERATOR_ROLE_ID.to_string()]), Role::Operator);
    }

    #[test]
    fn default_roles_are_undeletable() {
        let roles = default_roles();
        assert_eq!(roles.len(), 2);
        assert!(roles.iter().all(|(_, name, desc)| !name.is_empty() && desc.contains("Undeletable")));
    }

    #[test]
    fn group_json_counts_members() {
        let mut directory = DirectoryState::default();
        directory
            .groups
            .insert("g1".into(), DirectoryGroup { display_name: "G".into(), created_at: 1, creator_id: None });
        directory.users.insert("u1".into(), DirectoryUser { groups: vec!["g1".into()], ..DirectoryUser::default() });
        directory.users.insert("u2".into(), DirectoryUser::default());
        let json = group_json("g1", directory.groups.get("g1").unwrap(), &directory);
        assert_eq!(json["size"], 1);
        assert_eq!(json["display_name"], "G");
    }
}
