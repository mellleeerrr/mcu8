//! Учётные записи и профиль пользователя PNM (T2.1).

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::auth::ClientIp;
use super::super::{error_json, track};
use super::{commit_directory, ensure_profile, find_user, json_body, require_manage, Users};
use crate::auth_middleware::AuthContext;
use crate::state::SharedState;
use vks_core::ports::storage::UserCredentials;
use vks_core::UserId;

// --- Учётные записи -------------------------------------------------------

fn credential_json(login: &str) -> Value {
    json!({"login": login, "expire_at": 0, "block_at": 0})
}

/// LocalDb-идентичность пользователя: id + логин. id имеет приоритет; fallback
/// по внутреннему username разрешён ТОЛЬКО для bootstrap-расклада (`u-admin`),
/// иначе пользователь без учётки «нашёл» бы чужой логин, совпавший с его
/// display_name (cross-user credentials alias).
pub(super) fn local_identity(
    state: &crate::state::AppState,
    user: &crate::state::UserRecord,
) -> Option<(String, String)> {
    let db = state.local_db.as_ref()?;
    if let Ok(local) = db.user(&user.id.to_string()) {
        return Some((local.id, local.username));
    }
    let local = db.user_by_name(&user.username).ok()?;
    let bootstrap = user.id == UserId::bootstrap_operator();
    (bootstrap || local.id == "u-admin").then_some((local.id, local.username))
}

fn local_login(state: &crate::state::AppState, user: &crate::state::UserRecord) -> Option<String> {
    local_identity(state, user).map(|(_, login)| login)
}

/// Синхронизирует in-memory поколение пользователя с LocalDb: старые токены
/// (generation < новое) перестают проходить проверку немедленно, а не только
/// после рестарта/чтения из durable credentials.
fn sync_generation(state: &crate::state::AppState, user_id: &str, generation: u64) {
    if let Ok(mut users) = state.users.lock() {
        if let Some(user) = users.values_mut().find(|u| u.id.to_string() == user_id) {
            // Только вперёд: иначе LDAP-sync с меньшим generation «воскресил» бы
            // отозванные токены.
            user.generation = user.generation.max(generation);
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
    if local_identity(&state, &user).is_some() {
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
    // Логин не должен совпадать и с внутренним username другого пользователя:
    // иначе legacy-fallback и Users-идентичность начнут пересекаться.
    if db.user_by_name(login).is_ok() || Users::username_taken(&state, login, Some(user.id)) {
        return error_json(409, "Login already exists");
    }
    let user_id = user.id.to_string();
    if let Err(error) = db.register(&user_id, login, password) {
        tracing::warn!(error = %error, "directory: local credential registration failed");
        return error_json(409, "Login already exists");
    }
    if let Err(error) = persist_credentials(&state, &user.id, None).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    if let Err(response) = commit_directory(&state, |resources| {
        ensure_profile(&mut resources.directory, &user).login = Some(login.to_string());
        Ok(())
    })
    .await
    {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        "user.credentials.created",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    (StatusCode::CREATED, Json(credential_json(login))).into_response()
}

async fn persist_credentials(
    state: &crate::state::AppState,
    user_id: &UserId,
    local_id: Option<&str>,
) -> Result<(), String> {
    let Some(db) = state.local_db.as_ref() else { return Ok(()) };
    // bootstrap: LocalDb-запись может быть заведена под id/логином, отличным от
    // core-id; после переименования логина ищем по сохранённому LocalDb id.
    let username = state
        .users
        .lock()
        .map_err(|e| e.to_string())?
        .values()
        .find(|u| u.id == *user_id)
        .map(|u| u.username.clone())
        .unwrap_or_else(|| user_id.to_string());
    let local = match local_id {
        Some(local_id) => db.user(local_id),
        None => match db.user(&user_id.to_string()) {
            Ok(local) => Ok(local),
            Err(_) => {
                let local = db.user_by_name(&username).map_err(|e| e.to_string())?;
                let bootstrap = *user_id == UserId::bootstrap_operator() || local.id == "u-admin";
                if !bootstrap {
                    return Err("local credentials not found".to_string());
                }
                Ok(local)
            }
        },
    }
    .map_err(|e| e.to_string())?;
    if let Some(storage) = &state.storage {
        storage
            .upsert_user_credentials(&UserCredentials {
                user_id: *user_id,
                password_hash_hex: local.password_hash_hex,
                salt_hex: local.salt_hex,
                password_changed_at: Some(db.password_changed_at(&local.id)),
                generation: db.generation(&local.id),
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
    // bootstrap: LocalDb-запись может жить под id `u-admin`, а не под core-id.
    let Some((current_id, current_login)) = local_identity(&state, &user) else {
        return error_json(404, "Credentials not found");
    };
    let new_login = body.login.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let password = body.password.as_deref().or(body.new_password.as_deref()).unwrap_or_default();
    if new_login.is_none() && password.is_empty() {
        return error_json(400, "login or password is required");
    }
    let mut login = current_login.clone();
    if let Some(requested) = new_login {
        if requested != current_login {
            if db.user_by_name(requested).is_ok() || Users::username_taken(&state, requested, Some(user.id)) {
                return error_json(409, "Login already exists");
            }
            if let Err(error) = db.rename_user(&current_id, requested) {
                tracing::warn!(error = %error, "directory: login rename failed");
                return error_json(409, "Login cannot be changed");
            }
            login = requested.to_string();
            // `Users.username` — внутренний уникальный ключ RBAC, не логин
            // LocalDb; переименование логина его не меняет.
        }
    }
    if !password.is_empty() {
        if let Err(error) = db.set_password_async(&login, password).await {
            tracing::warn!(error = %error, "directory: password update failed");
            return error_json(500, "Internal error");
        }
    }
    if let Err(response) = commit_directory(&state, |resources| {
        ensure_profile(&mut resources.directory, &user).login = Some(login.clone());
        Ok(())
    })
    .await
    {
        return response;
    }
    if let Err(error) = persist_credentials(&state, &user.id, Some(&current_id)).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    let generation = db.generation(&current_id);
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
    // bootstrap: id в LocalDb может быть `u-admin` (не core-id).
    let Some((local_id, _)) = local_identity(&state, &user) else {
        return error_json(404, "Credentials not found");
    };
    let local = db.user(&local_id).ok();
    let Some(local) = local else {
        return error_json(404, "Credentials not found");
    };
    if !db.remove_user(&local.id) {
        return error_json(404, "Credentials not found");
    }
    if let Some(storage) = &state.storage {
        if let Err(error) = storage.delete_user_credentials(user.id).await {
            tracing::error!(error = %error, "directory: delete credentials persistence failed");
            return error_json(500, "Storage error");
        }
    }
    if let Err(response) = commit_directory(&state, |resources| {
        if let Some(profile) = resources.directory.users.get_mut(&user_id) {
            profile.login = None;
        }
        Ok(())
    })
    .await
    {
        return response;
    }
    let generation = db.generation(&local.id);
    sync_generation(&state, &user_id, generation);
    state.notify_revocation(user_id.clone(), generation);
    state.record_audit_entry(
        &auth.subject,
        "user.credentials.deleted",
        Some(user_id),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}

fn caller_user(state: &crate::state::AppState, auth: &AuthContext) -> Option<crate::state::UserRecord> {
    Users::resolve(state, &auth.subject)
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
            if db.user_by_name(requested).is_ok() || Users::username_taken(&state, requested, Some(user.id)) {
                return error_json(409, "Login already exists");
            }
            let local_id = db.user_by_name(&current_login).map(|u| u.id).unwrap_or_else(|_| user.id.to_string());
            if let Err(error) = db.rename_user(&local_id, requested) {
                tracing::warn!(error = %error, "directory: login rename failed");
                return error_json(409, "Login cannot be changed");
            }
            login = requested.to_string();
            // см. update_user_credentials: внутренний username не переименовываем.
        }
    }
    if !new_password.is_empty() {
        if let Err(error) = db.set_password_async(&login, new_password).await {
            tracing::error!(error = %error, "directory: profile password update failed");
            return error_json(500, "Internal error");
        }
    }
    if let Err(response) = commit_directory(&state, |resources| {
        ensure_profile(&mut resources.directory, &user).login = Some(login.clone());
        Ok(())
    })
    .await
    {
        return response;
    }
    let local_id = db.user_by_name(&login).map(|local| local.id).ok();
    if let Err(error) = persist_credentials(&state, &user.id, local_id.as_deref()).await {
        tracing::error!(error = %error, "directory: credential persistence failed");
        return error_json(500, "Storage error");
    }
    let generation =
        local_id.as_deref().map(|id| db.generation(id)).unwrap_or_else(|| db.generation(&user.id.to_string()));
    sync_generation(&state, &user.id.to_string(), generation);
    state.notify_revocation(user.id.to_string(), generation);
    state.record_audit_entry(
        &auth.subject,
        "profile.credentials.updated",
        Some(user.id.to_string()),
        client_ip.map(|Extension(ip)| ip.0).unwrap_or_default(),
    );
    Json(json!({"status": "ok"})).into_response()
}
