//! Аутентификация `/api/v3` (§4.3 плана): middleware, login + двухступенчатый lockout,
//! смена пароля с инвалидацией ранее выданных токенов.
//!
//! BACKEND_RULES §1.4 (двухступенчатый lockout):
//! - 5 неудач / 15 мин per user+IP → блок 15 мин;
//! - 10 неудач / 5 мин per IP → блок 15 мин;
//! - ответ 401 «Too many attempts…» + `Retry-After`.
//!
//! Аудит: `login_ok` / `login_fail` / `login_locked` / `password.changed` в `AuditLog`.
//! Токены и пароли **никогда не логируются** (BACKEND_RULES §6.3): в лог попадают только
//! `jti` (в ошибках vks-auth) и метрики-счётчики.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Extension, Request, State},
    http::{header, Extensions, HeaderMap},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use vks_auth::jwt::VerifiedJwt;
use vks_common::metrics::LOGIN_ATTEMPTS;

use super::{error_json, track, unauthorized};
use crate::state::{AppState, SharedState};

/// Порог user+IP: 5 неудач / 15 минут (BACKEND_RULES §1.4).
pub const USER_IP_MAX_FAILS: usize = 5;
/// Окно user+IP: 15 минут.
pub const USER_IP_WINDOW_SECS: i64 = 15 * 60;
/// Порог IP: 10 неудач / 5 минут.
pub const IP_MAX_FAILS: usize = 10;
/// Окно IP: 5 минут.
pub const IP_WINDOW_SECS: i64 = 5 * 60;
/// Длительность блокировки: 15 минут.
pub const BLOCK_SECS: i64 = 15 * 60;

/// IP клиента, вычисленный outer-middleware (`ConnectInfo` → `X-Forwarded-For` →
/// `X-Real-IP` → `"unknown"`). Кладётся в extensions запроса.
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

#[derive(Debug, Default)]
struct LockoutInner {
    /// user+IP → таймстампы неудач (сек), FIFO-обрезка по окну.
    user_ip_fails: HashMap<String, Vec<i64>>,
    /// IP → таймстампы неудач (сек).
    ip_fails: HashMap<String, Vec<i64>>,
    /// ключ (`u:…` / `ip:…`) → блокировка до (сек).
    blocked_until: HashMap<String, i64>,
}

/// Двухступенчатый in-memory lockout (BACKEND_RULES §1.4).
#[derive(Debug, Default)]
pub struct Lockout {
    inner: Mutex<LockoutInner>,
}

impl Lockout {
    /// Создаёт пустой lockout.
    pub fn new() -> Self {
        Self::default()
    }

    fn user_key(user: &str, ip: &str) -> String {
        format!("u:{user}@{ip}")
    }

    fn ip_key(ip: &str) -> String {
        format!("ip:{ip}")
    }

    /// `Some(retry_after_secs)`, если user+IP или IP сейчас заблокированы.
    pub fn blocked_for(&self, user: &str, ip: &str, now: i64) -> Option<i64> {
        let inner = self.inner.lock().expect("lockout poisoned");
        for key in [Self::user_key(user, ip), Self::ip_key(ip)] {
            if let Some(until) = inner.blocked_until.get(&key) {
                if *until > now {
                    return Some(*until - now);
                }
            }
        }
        None
    }

    /// Регистрирует неудачу. `true`, если именно этим вызовом поставлен блок
    /// (порог достигнут).
    pub fn record_failure(&self, user: &str, ip: &str, now: i64) -> bool {
        let mut inner = self.inner.lock().expect("lockout poisoned");
        let mut blocked_now = false;

        let uk = Self::user_key(user, ip);
        let fails = inner.user_ip_fails.entry(uk.clone()).or_default();
        fails.push(now);
        fails.retain(|t| now - *t < USER_IP_WINDOW_SECS);
        if fails.len() >= USER_IP_MAX_FAILS {
            inner.blocked_until.insert(uk, now + BLOCK_SECS);
            blocked_now = true;
        }

        let ik = Self::ip_key(ip);
        let ip_fails = inner.ip_fails.entry(ik.clone()).or_default();
        ip_fails.push(now);
        ip_fails.retain(|t| now - *t < IP_WINDOW_SECS);
        if ip_fails.len() >= IP_MAX_FAILS {
            inner.blocked_until.insert(ik, now + BLOCK_SECS);
            blocked_now = true;
        }

        blocked_now
    }

    /// Успешный вход: сбрасывает счётчик user+IP. IP-счётчик сохраняется —
    /// защита от перебора разных логинов с одного IP.
    pub fn record_success(&self, user: &str, ip: &str) {
        let mut inner = self.inner.lock().expect("lockout poisoned");
        inner.user_ip_fails.remove(&Self::user_key(user, ip));
    }
}

/// IP клиента: `ConnectInfo` (реальный сервер) → `X-Forwarded-For` → `X-Real-IP` → `"unknown"`.
///
/// **Развёртывание за reverse-proxy (долг §4.3):** в проде `ConnectInfo` всегда
/// присутствует (`into_make_service_with_connect_info` в `main.rs`), поэтому XFF/X-Real-IP
/// достижимы только при вызове роутера без connect-info (unit/библиотечное встраивание).
/// За nginx lockout/WS-лимиты считаются по IP прокси — общие для всех клиентов;
/// доверие к XFF осознанно не включено (подделываемый заголовок = обход лимитов).
pub fn client_ip(headers: &HeaderMap, extensions: &Extensions) -> String {
    if let Some(axum::extract::ConnectInfo(addr)) = extensions.get::<axum::extract::ConnectInfo<SocketAddr>>() {
        return addr.ip().to_string();
    }
    if let Some(fwd) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = fwd.split(',').next() {
            let ip = first.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
    }
    if let Some(real) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let ip = real.trim();
        if !ip.is_empty() {
            return ip.to_string();
        }
    }
    "unknown".to_string()
}

/// Fail-closed (§4.3): без сконфигурированного `JwtIssuer` весь `/api/v3/*`
/// (включая login и ws) отвечает 503. Попутно кладёт [`ClientIp`] в extensions
/// (нужен login/ws-хендлерам на exempt-маршрутах).
pub async fn fail_closed_middleware(State(state): State<SharedState>, mut request: Request, next: Next) -> Response {
    if state.jwt_issuer.is_none() {
        return error_json(503, "Authentication is not configured");
    }
    let ip = client_ip(request.headers(), request.extensions());
    request.extensions_mut().insert(ClientIp(ip));
    next.run(request).await
}

/// Проверка access-токена: подпись/`exp` (vks-auth) + инвалидация токенов,
/// выданных до последней смены пароля (`iat < password_changed_at`).
///
/// Общий путь для HTTP-middleware и WS-сообщений (§4.4: токен в каждом
/// ws-сообщении). `None` — токен недействителен по любой причине.
pub(crate) fn verify_access_token(state: &AppState, token: &str) -> Option<VerifiedJwt> {
    state.verify_token(token)
}

/// Проверка `Authorization: Bearer <token>` (§4.3) + инвалидация токенов,
/// выданных до последней смены пароля (`iat < password_changed_at`).
///
/// Успешная проверка кладёт [`VerifiedJwt`] в extensions (использует `profile`).
pub async fn token_middleware(State(state): State<SharedState>, mut request: Request, next: Next) -> Response {
    if state.jwt_issuer.is_none() {
        return error_json(503, "Authentication is not configured");
    }
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or("");
    match verify_access_token(&state, token) {
        Some(verified) => {
            let role = crate::users::Users::resolve_role(&state, &verified.user_id)
                .unwrap_or(vks_core::Role::Viewer);
            let auth_ctx = crate::auth_middleware::AuthContext {
                subject: verified.user_id.clone(),
                role,
                auth_type: crate::auth_middleware::AuthType::Jwt,
                scopes: vec!["*".into()],
                room_scope: verified.room_id.clone(),
                generation: verified.generation,
            };
            request.extensions_mut().insert(auth_ctx);
            request.extensions_mut().insert(verified);
            next.run(request).await
        }
        None => unauthorized("Unauthorized"),
    }
}

/// Тело `POST /api/v3/login` (UI отправляет `{login, password}`; `username` — alias).
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Логин (в UI — `login`; `username` принимается как alias).
    #[serde(alias = "username")]
    pub login: String,
    /// Пароль (никогда не логируется).
    pub password: String,
}

/// Тело `PATCH /api/v3/password` (§4.3: `{username, old_password, new_password}` → `{token}`).
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    /// Логин пользователя (`login` принимается как alias).
    #[serde(alias = "login")]
    pub username: String,
    /// Текущий пароль (verify old).
    pub old_password: String,
    /// Новый пароль.
    pub new_password: String,
}

/// `POST /api/v3/login` → `{token}` | 401 `{"error":{…}}` (`Login rejected` /
/// `Too many attempts…` + `Retry-After` при блокировке).
pub async fn login(
    State(state): State<SharedState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(body): Json<LoginRequest>,
) -> Response {
    track("login");
    let now = now_unix();

    if let Some(retry_after) = state.api_v3.lockout.blocked_for(&body.login, &ip, now) {
        LOGIN_ATTEMPTS.with_label_values(&["locked"]).inc();
        audit(&state, &body.login, &ip, "login_locked");
        return locked_response(retry_after);
    }

    let auth_result = if let Some(provider) = &state.auth_provider {
        let creds = vks_core::ports::auth::Credentials {
            username: body.login.clone(),
            password: body.password.clone(),
        };
        provider.verify(&creds).await.map(|u| u.id.to_string())
    } else if let Some(db) = state.local_db.clone() {
        db.authenticate(&body.login, &body.password).map(|u| u.id).map_err(|_| vks_core::ports::auth::AuthError::InvalidCredentials)
    } else {
        return error_json(503, "Authentication is not configured");
    };

    match auth_result {
        Ok(user_id) => {
            state.api_v3.lockout.record_success(&body.login, &ip);
            LOGIN_ATTEMPTS.with_label_values(&["ok"]).inc();
            audit(&state, &body.login, &ip, "login_ok");
            issue_token(&state, &user_id)
        }
        Err(_) => {
            let blocked_now = state.api_v3.lockout.record_failure(&body.login, &ip, now);
            if blocked_now {
                LOGIN_ATTEMPTS.with_label_values(&["locked"]).inc();
                audit(&state, &body.login, &ip, "login_locked");
                locked_response(BLOCK_SECS)
            } else {
                LOGIN_ATTEMPTS.with_label_values(&["fail"]).inc();
                audit(&state, &body.login, &ip, "login_fail");
                unauthorized("Login rejected")
            }
        }
    }
}

/// `PATCH /api/v3/password` → verify old → `set_password` → **новый** `{token}`
/// (старые токены инвалидированы меткой смены пароля).
pub async fn change_password(
    State(state): State<SharedState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(body): Json<ChangePasswordRequest>,
) -> Response {
    track("password");
    let Some(db) = state.local_db.clone() else {
        return error_json(503, "Authentication is not configured");
    };
    let user = match db.authenticate(&body.username, &body.old_password) {
        Ok(u) => u,
        Err(_) => {
            LOGIN_ATTEMPTS.with_label_values(&["fail"]).inc();
            audit(&state, &body.username, &ip, "login_fail");
            return unauthorized("Login rejected");
        }
    };
    if body.new_password.is_empty() {
        return error_json(422, "New password must not be empty");
    }
    if let Err(e) = db.set_password(&body.username, &body.new_password) {
        tracing::error!(error = %e, "api_v3: password change failed");
        return error_json(500, "Internal error");
    }
    let new_gen = db.generation(&user.id);
    if let Ok(mut users) = state.users.lock() {
        if let Some(u) = users.values_mut().find(|u| u.id.to_string() == user.id || u.username == user.username) {
            u.generation = new_gen;
        }
    }
    state.notify_revocation(&user.id, new_gen);
    audit(&state, &body.username, &ip, "password.changed");
    issue_token(&state, &user.id)
}

/// Выдаёт токен; ошибки подписи → 500 (без утечки деталей).
fn issue_token(state: &AppState, user_id: &str) -> Response {
    let Some(issuer) = &state.jwt_issuer else {
        return error_json(503, "Authentication is not configured");
    };
    let gen = if let Ok(users) = state.users.lock() {
        users.values().find(|u| u.id.to_string() == user_id || u.username == user_id).map(|u| u.generation)
    } else {
        None
    };
    let generation = gen.unwrap_or_else(|| {
        state.local_db.as_ref().map(|db| db.generation(user_id)).unwrap_or(1)
    });
    let min_iat = state.local_db.as_ref().map(|db| db.password_changed_at(user_id)).unwrap_or(0);
    let iat = now_unix().max(min_iat);
    match issuer.issue_with_gen_and_iat(user_id, None, generation, iat) {
        Ok(token) => Json(serde_json::json!({ "token": token })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "api_v3: token issue failed");
            error_json(500, "Internal error")
        }
    }
}

/// 401 + `Retry-After` (сек, минимум 1).
fn locked_response(retry_after_secs: i64) -> Response {
    let mut resp = unauthorized("Too many attempts. Try again later");
    if let Ok(value) = retry_after_secs.max(1).to_string().parse() {
        resp.headers_mut().insert(header::RETRY_AFTER, value);
    }
    resp
}

/// Запись в аудит (ADR-0019).
fn audit(state: &AppState, user: &str, ip: &str, action: &str) {
    state.audit.lock().expect("audit store poisoned").record(crate::audit::AuditEntry {
        ts_ms: now_unix_ms(),
        user: user.to_string(),
        action: action.to_string(),
        room: None,
        ip: ip.to_string(),
    });
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_five_user_ip_failures() {
        let l = Lockout::new();
        let now = 1_000_000;
        for i in 0..USER_IP_MAX_FAILS - 1 {
            assert!(!l.record_failure("alice", "10.0.0.1", now + i as i64));
        }
        assert!(l.record_failure("alice", "10.0.0.1", now + 10));
        let retry = l.blocked_for("alice", "10.0.0.1", now + 11).expect("blocked");
        assert!(retry > 0 && retry <= BLOCK_SECS);
        // Другой IP — не заблокирован.
        assert!(l.blocked_for("alice", "10.0.0.2", now + 11).is_none());
    }

    #[test]
    fn blocks_after_ten_ip_failures_across_users() {
        let l = Lockout::new();
        let now = 2_000_000;
        for i in 0..IP_MAX_FAILS - 1 {
            assert!(!l.record_failure(&format!("user{i}"), "10.0.0.9", now + i as i64));
        }
        assert!(l.record_failure("userX", "10.0.0.9", now + 100));
        assert!(l.blocked_for("any-user", "10.0.0.9", now + 101).is_some());
    }

    #[test]
    fn success_clears_user_ip_counter() {
        let l = Lockout::new();
        let now = 3_000_000;
        for i in 0..USER_IP_MAX_FAILS - 1 {
            l.record_failure("bob", "10.0.0.5", now + i as i64);
        }
        l.record_success("bob", "10.0.0.5");
        // После сброса нужно снова 5 неудач до блока.
        for i in 0..USER_IP_MAX_FAILS - 1 {
            assert!(!l.record_failure("bob", "10.0.0.5", now + 10 + i as i64));
        }
        assert!(l.blocked_for("bob", "10.0.0.5", now + 20).is_none());
    }

    #[test]
    fn window_expiry_releases_user_ip_block() {
        let l = Lockout::new();
        let now = 4_000_000;
        for i in 0..USER_IP_MAX_FAILS {
            l.record_failure("carol", "10.0.0.7", now + i as i64);
        }
        assert!(l.blocked_for("carol", "10.0.0.7", now + 10).is_some());
        // Блок истёк (15 мин).
        assert!(l.blocked_for("carol", "10.0.0.7", now + BLOCK_SECS + 11).is_none());
        // Окно неудач тоже истекло → счётчик обнуляется прозрачно.
        let fresh = now + BLOCK_SECS + 1000;
        assert!(!l.record_failure("carol", "10.0.0.7", fresh));
        assert!(l.blocked_for("carol", "10.0.0.7", fresh + 1).is_none());
    }

    #[test]
    fn client_ip_prefers_forwarded_for_then_real_ip() {
        let mut headers = HeaderMap::new();
        let extensions = Extensions::new();
        assert_eq!(client_ip(&headers, &extensions), "unknown");

        headers.insert("x-real-ip", "192.168.1.5".parse().unwrap());
        assert_eq!(client_ip(&headers, &extensions), "192.168.1.5");

        headers.insert("x-forwarded-for", "203.0.113.9, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&headers, &extensions), "203.0.113.9");
    }
}
