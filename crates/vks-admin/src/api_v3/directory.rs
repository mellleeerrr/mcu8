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
    extract::{rejection::JsonRejection, Extension, Path, Query, State},
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

/// Права built-in ролей (vendor `GET /roles/{id}.permissions`). `Administrator`
/// получает wildcard `all`; `Operator` — набор для управления конференциями и
/// справочником. Кастомные роли создаются с минимальным read-набором.
fn default_role_permissions(id: &str) -> Vec<String> {
    match id {
        ADMIN_ROLE_ID => vec!["all".to_string()],
        OPERATOR_ROLE_ID => [
            "conferences:create",
            "conferences:delete",
            "conferences:read",
            "conferences:update",
            "sessions:start",
            "sessions:stop",
            "sessions:read",
            "sessions:update",
            "groups:create",
            "groups:delete",
            "groups:read",
            "groups:update",
            "users:create",
            "users:delete",
            "users:read",
            "users:read_credentials",
            "users:update",
            "roles:read",
            "profile:read",
            "profile:update",
            "profile:read_credentials",
            "call_strings:create",
            "call_strings:delete",
            "call_strings:read",
            "call_strings:update",
            "call_history:read",
            "call_history:export",
            "credentials:create",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        _ => ["users:read", "groups:read", "roles:read", "profile:read", "sessions:read"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
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

/// Роль справочника (`GET /roles`): built-in Administrator/Operator и
/// созданные администратором кастомные роли.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct DirectoryRole {
    pub display_name: String,
    pub description: String,
    pub permissions: Vec<String>,
    pub created_at: u64,
    pub creator_id: Option<String>,
}

/// Справочник в `ResourceState`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DirectoryState {
    pub users: BTreeMap<String, DirectoryUser>,
    pub groups: BTreeMap<String, DirectoryGroup>,
    /// Кастомные роли (built-in приходят из `default_roles()`).
    pub roles: BTreeMap<String, DirectoryRole>,
    /// Личные адресные книги: user_id → address_id → запись.
    pub user_addresses: BTreeMap<String, BTreeMap<String, AddressRecord>>,
}

/// Единый конверт ошибки разбора JSON: vendor ждёт `{error:{code,message}}`,
/// а не plain-text rejection axum. `Response` — крупный тип, но это не горячий путь.
#[allow(clippy::result_large_err)]
fn json_body(payload: Result<Json<Value>, JsonRejection>) -> Result<Value, Response> {
    match payload {
        Ok(Json(body)) => Ok(body),
        Err(rejection) => Err(error_json(400, &format!("Invalid JSON body: {rejection}"))),
    }
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

/// Чтение справочника — Moderator+ в глобальном scope. Room-scoped токен
/// (участник конкретной конференции) глобальный справочник не читает.
fn can_read(auth: &AuthContext) -> bool {
    auth.role >= Role::Moderator && auth.room_scope.is_none()
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

/// Идентификатор вызывающего в admin-хранилище: JWT `sub` может быть id
/// LocalDb-пользователя (`u-admin`), а карточка/гарды работают по id `Users`.
fn caller_id(state: &crate::state::AppState, subject: &str) -> Option<String> {
    if let Some(user) = find_user(state, subject) {
        return Some(user.id.to_string());
    }
    let username = state.local_db.as_ref().and_then(|db| db.user(subject).ok()).map(|u| u.username);
    username.as_deref().and_then(|name| find_user(state, name)).map(|user| user.id.to_string())
}

fn is_deletable(viewer: Option<&str>, id: &str) -> bool {
    id != vks_core::BOOTSTRAP_ADMIN_USER_ID && viewer != Some(id)
}

fn sorted_users(
    state: &crate::state::AppState,
    directory: &DirectoryState,
    viewer: Option<&str>,
) -> Vec<(crate::state::UserRecord, Value)> {
    let mut rows: Vec<_> = Users::list(state)
        .into_iter()
        .map(|user| {
            let role = user.role;
            let fallback;
            let user_id = user.id.to_string();
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
            let mut value = profile_json(state, directory, &user_id, profile, role, false, viewer);
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

/// Описание роли для `GET /roles`, `GET /roles/{id}` и карточки пользователя.
fn role_json(directory: &DirectoryState, users: &[crate::state::UserRecord], role_id: &str) -> Option<Value> {
    let (display_name, description, permissions, created_at, creator_id, deletable) =
        if let Some((_, name, description)) = default_roles().into_iter().find(|(id, _, _)| *id == role_id) {
            (name.to_string(), description.to_string(), default_role_permissions(role_id), 0, Value::Null, false)
        } else {
            let role = directory.roles.get(role_id)?;
            (
                role.display_name.clone(),
                role.description.clone(),
                role.permissions.clone(),
                role.created_at,
                json!(role.creator_id),
                true,
            )
        };
    let size = users.iter().filter(|user| role_ids_for(directory, user).iter().any(|r| r == role_id)).count();
    Some(json!({
        "id": role_id,
        "display_name": display_name,
        "description": description,
        "permissions": permissions,
        "created_at": created_at,
        "creator_id": creator_id,
        "defaults": {"editable": true, "deletable": deletable, "disableable": false},
        "size": size,
    }))
}

fn profile_json(
    state: &crate::state::AppState,
    directory: &DirectoryState,
    user_id: &str,
    profile: &DirectoryUser,
    role: Role,
    detail: bool,
    viewer: Option<&str>,
) -> Value {
    let groups: Vec<Value> = profile
        .groups
        .iter()
        .filter_map(|gid| directory.groups.get(gid).map(|g| group_json(gid, g, directory)))
        .collect();
    let role_ids: Vec<String> = if profile.roles.is_empty() {
        role_name(role).map(|r| vec![r.to_string()]).unwrap_or_default()
    } else {
        profile.roles.clone()
    };
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
        "defaults": {"editable": true, "deletable": is_deletable(viewer, user_id), "disableable": false},
        "terminal": Value::Null,
        "busy": false,
    });
    if detail {
        let users = Users::list(state);
        let roles: Vec<Value> = role_ids.iter().filter_map(|rid| role_json(directory, &users, rid)).collect();
        let permissions: Vec<String> = if role_ids.is_empty() {
            Vec::new()
        } else {
            let mut all = Vec::new();
            for rid in &role_ids {
                if let Some(Value::Array(items)) =
                    roles.iter().find(|r| r["id"] == rid.as_str()).map(|r| &r["permissions"])
                {
                    all.extend(items.iter().filter_map(Value::as_str).map(str::to_string));
                }
            }
            all.sort();
            all.dedup();
            all
        };
        value["emails"] = json!(if profile.emails.is_empty() { vec![String::new()] } else { profile.emails.clone() });
        value["phones"] = json!(if profile.phones.is_empty() { vec![String::new()] } else { profile.phones.clone() });
        value["avatars"] = json!([]);
        value["roles"] = json!(roles);
        value["permissions"] = json!(permissions);
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
    let viewer = caller_id(&state, &auth.subject);
    let (count, users) = filtered_users(&state, &resources.directory, &params, viewer.as_deref());
    Json(json!({"count": count, "users": users})).into_response()
}

// `Response` — крупный тип, но HTTP-хендлеры всё равно возвращают его; ошибка
// здесь не горячий путь.
#[allow(clippy::result_large_err)]
fn filtered_users(
    state: &crate::state::AppState,
    directory: &DirectoryState,
    params: &Value,
    viewer: Option<&str>,
) -> (usize, Vec<Value>) {
    let list = ListParams::parse(params);
    let search = params
        .get("search")
        .or_else(|| params.get("query"))
        .and_then(Value::as_str)
        .unwrap_or(&list.search)
        .to_lowercase();
    let group_id = params.get("group_id").and_then(Value::as_str).unwrap_or_default();
    let role_id = params.get("role_id").and_then(Value::as_str).unwrap_or_default();
    let exclude_groups = id_list(params, &["exclude_groups"]);
    let exclude_roles = id_list(params, &["exclude_roles"]);
    let exclude_users = id_list(params, &["exclude_users", "users"]);
    let mut rows: Vec<Value> = sorted_users(state, directory, viewer)
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
            let uid = user.id.to_string();
            if exclude_users.iter().any(|id| id == &uid) {
                return false;
            }
            if !exclude_groups.is_empty() {
                let groups = profile(directory, user).map(|p| p.groups.clone()).unwrap_or_default();
                if exclude_groups.iter().any(|g| groups.iter().any(|member| member == g)) {
                    return false;
                }
            }
            if !exclude_roles.is_empty() {
                let roles = role_ids_for(directory, user);
                if exclude_roles.iter().any(|r| roles.iter().any(|assigned| assigned == r)) {
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
    (count, users)
}

fn user_detail(
    state: &crate::state::AppState,
    directory: &DirectoryState,
    user: &crate::state::UserRecord,
    viewer: Option<&str>,
) -> Value {
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
    let mut value = profile_json(state, directory, &user.id.to_string(), profile, user.role, true, viewer);
    value["id"] = json!(user.id.to_string());
    value["busy"] = json!(false);
    value
}

/// `POST /api/v3/users` → `{user: {...}}`.
pub async fn create_user(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("users.create");
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
        user_detail(&state, &resources.directory, &user, caller_id(&state, &auth.subject).as_deref())
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
    Json(json!({"user": user_detail(&state, &resources.directory, &user, caller_id(&state, &auth.subject).as_deref())}))
        .into_response()
}

/// `PATCH /api/v3/users/{id}` — редактирование профиля.
pub async fn update_user(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    Path(id): Path<String>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("users.update");
    let body = match json_body(payload) {
        Ok(body) => body,
        Err(response) => return response,
    };
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
    if new_display.is_some_and(|d| d.chars().count() > 255) {
        return error_json(400, "display_name is too long");
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
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
        // Внутреннее имя = display_name, пока у пользователя нет учётной записи
        // в LocalDb (источник истины для логина — LocalDb, не кэш профиля).
        if local_login(&state, &user).is_none() && display != user.username {
            if let Err(error) = Users::update_async(&state, &user.id, Some(display), None).await {
                tracing::error!(error = %error, "directory: rename user persistence failed");
                return error_json(500, "Storage error");
            }
        }
        let Some(updated) = find_user(&state, &user_id) else {
            return error_json(404, "User not found");
        };
        user_detail(&state, &resources.directory, &updated, caller_id(&state, &auth.subject).as_deref())
    };
    state.record_audit_entry(
        &auth.subject,
        "user.updated",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"user": detail})).into_response()
}

/// `DELETE /api/v3/users/{id}` — удаление пользователя.
///
/// Помимо soft-delete идентичности (`Users::delete_async`) удаляются учётные
/// данные (`LocalDb` + durable `Storage::user_credentials`), профиль, членства и
/// личная адресная книга. Токены пользователя отзываются; повторный login
/// невозможен (учётная запись удалена из LocalDb).
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
    if caller_id(&state, &auth.subject).as_deref() == Some(user.id.to_string().as_str()) {
        return error_json(403, "You cannot delete your own account");
    }
    let user_id = user.id.to_string();

    // 1. Учётные данные — в первую очередь, чтобы удалённый пользователь не мог
    //    войти даже при частичном сбое следующих шагов.
    let mut generation = 0;
    if let Some(db) = state.local_db.as_ref() {
        db.remove_user(&user_id);
        generation = db.generation(&user_id);
    }
    if let Some(storage) = state.storage.as_ref() {
        if let Err(error) = storage.delete_user_credentials(user.id).await {
            tracing::error!(error = %error, user=%user_id, "directory: delete credentials persistence failed");
        }
    }

    // 2. Идентичность (soft-delete; durable `users`).
    match Users::delete_async(&state, &user.id).await {
        Ok(true) => {}
        Ok(false) => return error_json(404, "User not found"),
        Err(error) => {
            tracing::error!(error = %error, "directory: delete user persistence failed");
            return error_json(500, "Storage error");
        }
    }

    // 3. Профиль/членства/личные адреса — из configuration-document.
    {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        resources.directory.users.remove(&user_id);
        resources.directory.user_addresses.remove(&user_id);
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            tracing::error!(user=%user_id, "directory: profile cleanup persistence failed");
            let _ = response;
        }
    }
    state.notify_revocation(user_id.clone(), generation);
    state.record_audit_entry(
        &auth.subject,
        "user.deleted",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

// --- Группы ---------------------------------------------------------------

/// Список id из параметров vendor-UI: массив, строка через запятую или
/// одиночное значение (`exclude_users`, `users_exclude`, `exclude_groups`…).
fn id_list(params: &Value, keys: &[&str]) -> Vec<String> {
    let mut ids = Vec::new();
    for key in keys {
        match params.get(*key) {
            Some(Value::Array(items)) => {
                ids.extend(
                    items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
                );
            }
            Some(Value::String(raw)) => {
                ids.extend(raw.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));
            }
            _ => {}
        }
    }
    ids
}

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
    let mut groups: Vec<Value> = resources
        .directory
        .groups
        .iter()
        .map(|(id, group)| group_json(id, group, &resources.directory))
        .filter(|value| {
            if !(search.is_empty()
                || value["display_name"].as_str().unwrap_or_default().to_lowercase().contains(&search))
            {
                return false;
            }
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
    groups.sort_by(|a, b| {
        let cmp = match list.sort_field.as_str() {
            "created_at" => a["created_at"].as_u64().unwrap_or(0).cmp(&b["created_at"].as_u64().unwrap_or(0)),
            "size" => a["size"].as_u64().unwrap_or(0).cmp(&b["size"].as_u64().unwrap_or(0)),
            _ => a["display_name"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .cmp(&b["display_name"].as_str().unwrap_or_default().to_lowercase()),
        };
        if list.sort_order == 1 {
            cmp.reverse()
        } else {
            cmp
        }
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
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    if resources.directory.groups.values().any(|g| g.display_name.to_lowercase() == display_name.to_lowercase()) {
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
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    if !resources.directory.groups.contains_key(&id) {
        return error_json(404, "Group not found");
    }
    let display_name = body["display_name"].as_str().map(str::trim).filter(|s| !s.is_empty());
    if body.get("display_name").is_some() && display_name.is_none() {
        return error_json(400, "display_name must not be empty");
    }
    if display_name.is_some_and(|d| d.chars().count() > 128) {
        return error_json(400, "display_name is too long");
    }
    if let Some(display) = display_name {
        let clash = resources
            .directory
            .groups
            .iter()
            .any(|(gid, g)| gid != &id && g.display_name.to_lowercase() == display.to_lowercase());
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

/// Идентификаторы всех ролей: built-in + кастомные.
fn all_role_ids(directory: &DirectoryState) -> Vec<String> {
    let mut ids: Vec<String> = default_roles().iter().map(|(id, _, _)| id.to_string()).collect();
    ids.extend(directory.roles.keys().cloned());
    ids
}

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
    let mut roles: Vec<Value> = all_role_ids(&resources.directory)
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
    roles.sort_by(|a, b| {
        let cmp = match list.sort_field.as_str() {
            "size" => a["size"].as_u64().unwrap_or(0).cmp(&b["size"].as_u64().unwrap_or(0)),
            "created_at" => a["created_at"].as_u64().unwrap_or(0).cmp(&b["created_at"].as_u64().unwrap_or(0)),
            _ => a["display_name"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .cmp(&b["display_name"].as_str().unwrap_or_default().to_lowercase()),
        };
        if list.sort_order == 1 {
            cmp.reverse()
        } else {
            cmp
        }
    });
    let count = roles.len();
    let start = list.page.saturating_sub(1).saturating_mul(list.page_size);
    let roles: Vec<Value> = roles.into_iter().skip(start).take(list.page_size.max(1)).collect();
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
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    if resources.directory.roles.values().any(|r| r.display_name.to_lowercase() == display_name.to_lowercase())
        || default_roles().iter().any(|(_, name, _)| name.to_lowercase() == display_name.to_lowercase())
    {
        return error_json(409, "Role with the same name already exists");
    }
    let id = uuid::Uuid::new_v4().to_string();
    resources.directory.roles.insert(
        id.clone(),
        DirectoryRole {
            display_name: display_name.to_string(),
            description: body["description"].as_str().map(str::to_string).unwrap_or_default(),
            permissions,
            created_at: now_secs(),
            creator_id: Some(auth.subject.clone()),
        },
    );
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    let users = Users::list(&state);
    let role = role_json(&resources.directory, &users, &id);
    drop(resources);
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
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    if builtin {
        let Some((_, name, _)) = default_roles().into_iter().find(|(rid, _, _)| rid == &id) else {
            return error_json(404, "Role not found");
        };
        if display_name.is_some_and(|d| d != name) {
            return error_json(403, "Built-in role cannot be renamed");
        }
    } else if !resources.directory.roles.contains_key(&id) {
        return error_json(404, "Role not found");
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
                return error_json(409, "Role with the same name already exists");
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
            let permissions: Vec<String> = permissions.iter().filter_map(Value::as_str).map(str::to_string).collect();
            if let Some(role) = resources.directory.roles.get_mut(&id) {
                role.permissions = permissions;
            }
        }
    } else if body.get("description").is_some() {
        // Описание built-in роли не хранится отдельно — отдаём 200 без изменений,
        // как vendor-заглушка (editable=true, но текст фиксирован).
    }
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    let users = Users::list(&state);
    let role = role_json(&resources.directory, &users, &id);
    drop(resources);
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
    let members: Vec<()> = {
        let resources = state.api_v3.resources.lock().await;
        if !resources.directory.roles.contains_key(&id) {
            return error_json(404, "Role not found");
        }
        Vec::new()
    };
    drop(members);
    let mut updates: Vec<(UserId, Role)> = Vec::new();
    {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        resources.directory.roles.remove(&id);
        for user in Users::list(&state) {
            if let Some(profile) = resources.directory.users.get_mut(&user.id.to_string()) {
                if profile.roles.iter().any(|r| r == &id) {
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
    // Built-in Administrator назначен всегда; снять его нельзя (`Undeletable`).
    if !add && role_id == ADMIN_ROLE_ID {
        return error_json(403, "The Administrator role cannot be removed");
    }
    let roles;
    let new_internal;
    {
        let mut resources = state.api_v3.resources.lock().await;
        if !all_role_ids(&resources.directory).iter().any(|id| id == &role_id) {
            return error_json(404, "Role not found");
        }
        let previous = resources.clone();
        let mut profile = resources.directory.users.get(&user_id).cloned().unwrap_or_else(|| DirectoryUser {
            display_name: default_display_name(&user),
            created_at: now_secs(),
            ..DirectoryUser::default()
        });
        // Материализуем неявную built-in роль перед изменением: профиль с пустым
        // `roles` представляет роль из `Users` (Administrator/Operator), и без
        // материализации добавление Operator понижало бы Administrator.
        if profile.roles.is_empty() {
            if let Some(current) = role_name(user.role) {
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

// --- Учётные записи -------------------------------------------------------

fn credential_json(login: &str) -> Value {
    json!({"login": login, "expire_at": 0, "block_at": 0})
}

fn local_login(state: &crate::state::AppState, user: &crate::state::UserRecord) -> Option<String> {
    let db = state.local_db.as_ref()?;
    db.user(&user.id.to_string())
        .ok()
        .map(|u| u.username)
        // bootstrap: LocalDb-запись может быть заведена под id `u-admin`, а
        // `Users`-запись создана с другим id; резолвим по логину.
        .or_else(|| db.user_by_name(&user.username).ok().map(|u| u.username))
}

/// Синхронизирует in-memory поколение пользователя с LocalDb: старые токены
/// (generation < новое) перестают проходить проверку немедленно, а не только
/// после рестарта/чтения из durable credentials.
fn sync_generation(state: &crate::state::AppState, user_id: &str, generation: u64) {
    if let Ok(mut users) = state.users.lock() {
        if let Some(user) = users.values_mut().find(|u| u.id.to_string() == user_id) {
            user.generation = generation;
        }
    }
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
    // bootstrap: LocalDb-запись может быть заведена под логином (`admin`), а не
    // под core-id пользователя.
    let username = state
        .users
        .lock()
        .map_err(|e| e.to_string())?
        .values()
        .find(|u| u.id == *user_id)
        .map(|u| u.username.clone())
        .unwrap_or_else(|| user_id.to_string());
    let user = db.user(&user_id.to_string()).or_else(|_| db.user_by_name(&username)).map_err(|e| e.to_string())?;
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
    payload: Result<Json<CredentialsBody>, JsonRejection>,
) -> Response {
    track("users.credentials.update");
    let Json(body) = match payload {
        Ok(body) => body,
        Err(rejection) => return error_json(400, &format!("Invalid credentials body: {rejection}")),
    };
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
    let new_login = body.login.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let password = body.password.as_deref().or(body.new_password.as_deref()).unwrap_or_default();
    if new_login.is_none() && password.is_empty() {
        return error_json(400, "login or password is required");
    }
    let mut login = current.username.clone();
    if let Some(requested) = new_login {
        if requested != current.username {
            if db.user_by_name(requested).is_ok() {
                return error_json(409, "Login already exists");
            }
            if let Err(error) = db.rename_user(&current.id, requested) {
                tracing::warn!(error = %error, "directory: login rename failed");
                return error_json(409, "Login cannot be changed");
            }
            login = requested.to_string();
            if let Err(error) = Users::update_async(&state, &user.id, Some(login.clone()), None).await {
                tracing::error!(error = %error, "directory: login rename persistence failed");
                return error_json(500, "Storage error");
            }
        }
    }
    if !password.is_empty() {
        if let Err(error) = db.set_password_async(&login, password).await {
            tracing::warn!(error = %error, "directory: password update failed");
            return error_json(500, "Internal error");
        }
    }
    {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        resources
            .directory
            .users
            .entry(user.id.to_string())
            .or_insert_with(|| DirectoryUser { display_name: default_display_name(&user), ..DirectoryUser::default() })
            .login = Some(login.clone());
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
    }
    if let Err(error) = persist_credentials(&state, &user.id).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    let generation = db.generation(&user.id.to_string());
    sync_generation(&state, &user.id.to_string(), generation);
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
    // `auth.subject` — id из LocalDb/JWT; в admin-хранилище он резолвится через
    // логин (bootstrap: LocalDb `u-admin` ↔ `Users.username = admin`).
    let username = state.local_db.as_ref().and_then(|db| db.user(&auth.subject).ok()).map(|u| u.username);
    username.as_deref().and_then(|name| find_user(state, name)).or_else(|| find_user(state, &auth.subject))
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

/// `PATCH /api/v3/profile/credentials` — смена собственного пароля.
///
/// Vendor-UI при неизменённом логине отправляет `"login": null`; `null`
/// означает «не менять», а не смену логина на пустой.
pub async fn update_profile_credentials(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    client_ip: Option<Extension<ClientIp>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Response {
    track("profile.credentials.update");
    let body = match json_body(payload) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Some(user) = caller_user(&state, &auth) else {
        return error_json(404, "User not found");
    };
    let Some(db) = state.local_db.clone() else {
        return error_json(503, "Authentication is not configured");
    };
    let Some(current_login) = local_login(&state, &user) else {
        return error_json(404, "Credentials not found");
    };
    let requested_login = body.get("login").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let old_password = body["old_password"].as_str().or_else(|| body["confirm_password"].as_str()).unwrap_or_default();
    let new_password = body["new_password"].as_str().or_else(|| body["password"].as_str()).unwrap_or_default();
    if new_password.is_empty() && requested_login.is_none() {
        return error_json(400, "new password is required");
    }
    if old_password.is_empty() {
        return error_json(400, "old password is required");
    }
    match db.authenticate_async(&current_login, old_password).await {
        Ok(_) => {}
        Err(_) => return error_json(404, "Incorrect old password"),
    }
    let mut login = current_login.clone();
    if let Some(requested) = requested_login {
        if requested != current_login {
            if db.user_by_name(requested).is_ok() {
                return error_json(409, "Login already exists");
            }
            let local_id = db.user_by_name(&current_login).map(|u| u.id).unwrap_or_else(|_| user.id.to_string());
            if let Err(error) = db.rename_user(&local_id, requested) {
                tracing::warn!(error = %error, "directory: login rename failed");
                return error_json(409, "Login cannot be changed");
            }
            login = requested.to_string();
            if let Err(error) = Users::update_async(&state, &user.id, Some(login.clone()), None).await {
                tracing::error!(error = %error, "directory: login rename persistence failed");
                return error_json(500, "Storage error");
            }
        }
    }
    if !new_password.is_empty() {
        if let Err(error) = db.set_password_async(&login, new_password).await {
            tracing::error!(error = %error, "directory: profile password update failed");
            return error_json(500, "Internal error");
        }
    }
    {
        let mut resources = state.api_v3.resources.lock().await;
        let previous = resources.clone();
        resources
            .directory
            .users
            .entry(user.id.to_string())
            .or_insert_with(|| DirectoryUser { display_name: default_display_name(&user), ..DirectoryUser::default() })
            .login = Some(login.clone());
        if let Err(response) = resources::commit(&state, &mut resources, previous).await {
            return response;
        }
    }
    if let Err(error) = persist_credentials(&state, &user.id).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    let generation = db.generation(&user.id.to_string());
    sync_generation(&state, &user.id.to_string(), generation);
    state.notify_revocation(user.id.to_string(), generation);
    if let Ok(mut users) = state.users.lock() {
        if let Some(record) = users.values_mut().find(|u| u.id == user.id) {
            record.generation = generation;
        }
    }
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

/// Проверка адреса: протокол (или host) обязателен, пробелы/управляющие символы
/// запрещены. `canonical_uri` сам добавляет `sip:`, поэтому проверяем остаток.
fn validate_call_string_uri(uri: &str) -> Result<(), String> {
    if uri.is_empty() || uri.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("uri is invalid".to_string());
    }
    let canonical = super::call_strings::canonical_uri(uri);
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
    let mut resources = state.api_v3.resources.lock().await;
    let Some(existing) = resources.directory.user_addresses.get(&user_id).and_then(|map| map.get(&address_id)).cloned()
    else {
        return error_json(404, "Call string not found");
    };
    let previous = resources.clone();
    let mut updated = existing;
    if let Some(uri) = body["uri"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
        if let Err(error) = validate_call_string_uri(uri) {
            return error_json(400, &error);
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
    if let Err(response) = resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    state.record_audit_entry(
        &auth.subject,
        "user.call_string.updated",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"call_string": super::call_strings::address_value(&address_id, &updated)})).into_response()
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
    room_scope: Option<&str>,
    payload: &Value,
) -> Result<Value, (u16, &'static str)> {
    let role = Users::resolve_role(state, subject).ok_or((403, "Forbidden"))?;
    if role < Role::Moderator || room_scope.is_some() {
        return Err((403, "Forbidden: insufficient permissions"));
    }
    let resources = state.api_v3.resources.lock().await;
    let (count, users) = filtered_users(state, &resources.directory, payload, Some(subject));
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
