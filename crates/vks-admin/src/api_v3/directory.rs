//! Справочник PNM (T2.1): пользователи, группы, роли, учётные записи и личные
//! адресные книги.

// Хендлеры обязаны возвращать `Response` (крупный тип); `commit_directory`
// проносит его через `Result` замыканий — это осознанный контракт axum, не ошибка.
#![allow(clippy::result_large_err)]
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

mod accounts;
mod addresses;
mod groups;
mod roles;

use accounts::local_identity;
pub use accounts::{
    create_user_credentials, delete_user_credentials, get_user_credentials, profile_credentials,
    update_profile_credentials, update_user_credentials,
};
pub(crate) use addresses::ws_user_call_strings;
pub use addresses::{
    attach_user_call_string, create_user_call_string, detach_user_call_string, favorite_user_call_string,
    list_user_call_strings, unfavorite_user_call_string, update_user_call_string,
};
use groups::group_json;
pub use groups::{
    add_user_to_group, create_group, delete_group, get_group, list_groups, remove_user_from_group, update_group,
};
pub use roles::{add_user_role, create_role, delete_role, get_role, list_roles, remove_user_role, update_role};

use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::{rejection::JsonRejection, Extension, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use vks_core::{Role, UserId};

use super::auth::ClientIp;
use super::resources::{self, AddressRecord};
use super::{error_json, sort_and_page, track, ListParams};
use crate::auth_middleware::AuthContext;
use crate::state::SharedState;
use crate::users::Users;

/// Роль «Administrator» (vendor default role, undeletable).
pub const ADMIN_ROLE_ID: &str = "770fada3-71d8-4176-a969-e12ec2059e64";
/// Роль «Operator» (vendor default role, undeletable).
pub const OPERATOR_ROLE_ID: &str = "425eda58-e25d-4418-b924-5a490be734ba";
const NIL_ID: &str = "00000000-0000-0000-0000-000000000000";

fn all_role_ids(directory: &DirectoryState) -> Vec<String> {
    let builtin: Vec<&str> = default_roles().iter().map(|(id, _, _)| *id).collect();
    let mut ids: Vec<String> = builtin.iter().map(|id| (*id).to_string()).collect();
    // Built-in override (description/permissions) хранится под тем же id в
    // directory.roles — его нельзя отдавать вторым элементом списка.
    ids.extend(directory.roles.keys().filter(|id| !builtin.contains(&id.as_str())).cloned());
    ids
}

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
    /// Основной личный адрес (vendor «звёздочка»/primary address).
    pub primary_address: Option<String>,
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

/// Единый конверт ошибки разбора JSON: vendor ждёт
/// `{error:{code,description}}`, а не plain-text rejection axum. `Response` —
/// крупный тип, но это не горячий путь.
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

/// Резолв пользователя по id/логину без неоднозначности: id имеет приоритет,
/// совпадение по `username` принимается только если оно единственное. Дубли
/// логинов запрещены при создании/переименовании; исторические дубли
/// трактуются как «не найдено» (fail-closed), а не «первый из HashMap».
fn find_user(state: &crate::state::AppState, id: &str) -> Option<crate::state::UserRecord> {
    let users = state.users.lock().expect("users store poisoned");
    let active = || users.values().filter(|user| !user.deleted);
    if let Some(user) = active().find(|user| user.id.to_string() == id) {
        return Some(user.clone());
    }
    let mut matches = active().filter(|user| user.username == id);
    match (matches.next(), matches.next()) {
        (Some(user), None) => Some(user.clone()),
        _ => None,
    }
}

/// Уникальное внутреннее имя пользователя (`Users.username` не равно
/// отображаемому имени): vendor допускает одинаковые display_name, а RBAC-резолв
/// должен оставаться однозначным. `display_name` не меняется.
fn unique_username(state: &crate::state::AppState, display_name: &str) -> String {
    // Пространства LocalDb-логинов и внутренних username не должны пересекаться:
    // иначе legacy-fallback резолва учётки может указать на чужого пользователя.
    let available = |candidate: &str| {
        !Users::username_taken(state, candidate, None)
            && state.local_db.as_ref().is_none_or(|db| db.user_by_name(candidate).is_err())
    };
    if available(display_name) {
        return display_name.to_string();
    }
    for suffix in 2..1000 {
        let candidate = format!("{display_name} ({suffix})");
        if available(&candidate) {
            return candidate;
        }
    }
    format!("{display_name} ({})", now_secs())
}

fn profile<'a>(directory: &'a DirectoryState, user: &crate::state::UserRecord) -> Option<&'a DirectoryUser> {
    directory.users.get(&user.id.to_string())
}

/// Идентификатор вызывающего в admin-хранилище: JWT `sub` может быть id
/// LocalDb-пользователя (`u-admin`), а карточка/гарды работают по id `Users`.
fn caller_id(state: &crate::state::AppState, subject: &str) -> Option<String> {
    Users::resolve(state, subject).map(|user| user.id.to_string())
}

fn is_deletable(viewer: Option<&str>, id: &str) -> bool {
    id != vks_core::BOOTSTRAP_ADMIN_USER_ID && viewer != Some(id)
}

/// Строки пользователей без сортировки/пагинации: порядок задаёт общий
/// `sort_and_page` в вызывающем списке (не дублируем компараторы).
fn user_rows(
    state: &crate::state::AppState,
    directory: &DirectoryState,
    viewer: Option<&str>,
) -> Vec<(crate::state::UserRecord, Value)> {
    Users::list(state)
        .into_iter()
        .map(|user| {
            let role = user.role;
            let fallback;
            let user_id = user.id.to_string();
            let profile = match profile(directory, &user) {
                Some(profile) => profile,
                None => {
                    fallback = fallback_profile(&user, 0);
                    &fallback
                }
            };
            let mut value = profile_json(state, directory, &user_id, profile, role, false, viewer);
            value["id"] = json!(user.id.to_string());
            (user, value)
        })
        .collect()
}

fn default_display_name(user: &crate::state::UserRecord) -> String {
    if user.id == UserId::bootstrap_operator() {
        "Administrator".to_string()
    } else {
        user.username.clone()
    }
}

/// Профиль по умолчанию для пользователя без сохранённого `DirectoryUser`:
/// один источник для списка, карточки и мутаций (неявная built-in роль уже
/// материализована — как её отдал бы `profile_json`).
fn fallback_profile(user: &crate::state::UserRecord, created_at: u64) -> DirectoryUser {
    DirectoryUser {
        display_name: default_display_name(user),
        created_at,
        roles: role_name(user.role).map(|role| vec![role.to_string()]).unwrap_or_default(),
        ..DirectoryUser::default()
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
            // Built-in роль редактируема (vendor `defaults.editable=true`): права и
            // описание могут быть переопределены и хранятся в directory.roles,
            // но отображаемое имя фиксировано.
            let stored = directory.roles.get(role_id);
            (
                name.to_string(),
                stored
                    .map(|role| role.description.clone())
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| description.to_string()),
                stored
                    .map(|role| role.permissions.clone())
                    .filter(|permissions| !permissions.is_empty())
                    .unwrap_or_else(|| default_role_permissions(role_id)),
                0,
                Value::Null,
                false,
            )
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
        "address": profile
            .primary_address
            .as_deref()
            .and_then(|address_id| directory.user_addresses.get(user_id).and_then(|map| map.get(address_id)))
            .map(|address| super::call_strings::address_value(profile.primary_address.as_deref().unwrap_or_default(), address))
            .unwrap_or(Value::Null),
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
    let exclude_users = id_list(params, &["exclude_users", "users_exclude"]);
    // Vendor `includeUsers → users`: это include/intersection, НЕ exclude.
    let include_users = id_list(params, &["users"]);
    let rows: Vec<Value> = user_rows(state, directory, viewer)
        .into_iter()
        .filter(|(user, value)| {
            if !search.is_empty() {
                let display = value["display_name"].as_str().unwrap_or_default().to_lowercase();
                let profile = profile(directory, user);
                let emails = profile.map(|p| p.emails.iter().map(|e| e.to_lowercase()).collect::<Vec<_>>());
                let phones = profile.map(|p| p.phones.iter().map(|e| e.to_lowercase()).collect::<Vec<_>>());
                let login = profile.and_then(|p| p.login.clone()).unwrap_or_default().to_lowercase();
                let matches = display.contains(&search)
                    || user.username.to_lowercase().contains(&search)
                    || login.contains(&search)
                    || emails.is_some_and(|items| items.iter().any(|item| item.contains(&search)))
                    || phones.is_some_and(|items| items.iter().any(|item| item.contains(&search)));
                if !matches {
                    return false;
                }
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
            if !include_users.is_empty() && !include_users.iter().any(|id| id == &uid) {
                return false;
            }
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
    // Поиск уже отфильтрован выше (login/email/phone), поэтому общий хелпер
    // отвечает только за сортировку/страницу.
    sort_and_page(rows, "", &list.sort_field, list.sort_order, list.page, list.page_size)
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
            fallback = fallback_profile(user, 0);
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
    let username = unique_username(&state, display_name);
    if let Err(error) = Users::create_with_id_async(&state, id, username, Role::Participant).await {
        tracing::error!(error = %error, "directory: create user persistence failed");
        return error_json(500, "Storage error");
    }
    if let Err(response) = commit_directory(&state, |resources| {
        resources.directory.users.insert(id.to_string(), profile);
        Ok(())
    })
    .await
    {
        return response;
    }
    let user = {
        let Some(user) = find_user(&state, &id.to_string()) else {
            return error_json(500, "User not found after creation");
        };
        let resources = state.api_v3.resources.lock().await;
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

/// Единая точка мутации справочника: лок → снимок ДО изменений → мутация →
/// commit с автоматическим rollback. Убирает 19 копий `previous = clone()` и
/// структурно исключает rollback «после мутации» (как было в delete credentials).
async fn commit_directory<T>(
    state: &SharedState,
    mutate: impl FnOnce(&mut resources::ResourceState) -> Result<T, Response>,
) -> Result<T, Response> {
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    let output = match mutate(&mut resources) {
        Ok(output) => output,
        Err(response) => {
            *resources = previous;
            return Err(response);
        }
    };
    resources::commit(state, &mut resources, previous).await?;
    Ok(output)
}

/// Гарантирует профиль пользователя в справочнике (единый дефолт для всех
/// мутаций: display_name/created_at), устраняя 6 копий `or_insert_with`.
fn ensure_profile<'a>(directory: &'a mut DirectoryState, user: &crate::state::UserRecord) -> &'a mut DirectoryUser {
    directory.users.entry(user.id.to_string()).or_insert_with(|| fallback_profile(user, now_secs()))
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
    if let Err(response) = commit_directory(&state, |resources| {
        let profile = ensure_profile(&mut resources.directory, &user);
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
        Ok(())
    })
    .await
    {
        return response;
    }
    let detail = {
        let Some(updated) = find_user(&state, &user_id) else {
            return error_json(404, "User not found");
        };
        let resources = state.api_v3.resources.lock().await;
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
        // legacy bootstrap-расклад: LocalDb-строка может жить под `u-admin`.
        let local_id = local_identity(&state, &user).map(|(id, _)| id).unwrap_or_else(|| user_id.clone());
        db.remove_user(&local_id);
        generation = db.generation(&local_id);
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
    if let Err(response) = commit_directory(&state, |resources| {
        resources.directory.users.remove(&user_id);
        resources.directory.user_addresses.remove(&user_id);
        Ok(())
    })
    .await
    {
        tracing::error!(user=%user_id, "directory: profile cleanup persistence failed");
        // Учётные данные/идентичность уже удалены; успешный ответ скрыл бы
        // оставшийся профиль (ретенция/GDPR) — отдаём 500.
        return response;
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
    let viewer = caller_id(state, subject);
    let (count, users) = filtered_users(state, &resources.directory, payload, viewer.as_deref());
    Ok(json!({"users": users, "count": count}))
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
