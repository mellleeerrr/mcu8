//! Аутентификация и RBAC для маршрутов vks-admin.
//!
//! Основание: ADR-0009 (JWT), ADR-0021, ADR-0022 (RBAC), BACKEND_RULES §1.1.
//! Две ступени (порядок важен — RBAC читает результат первой):
//! 1. [`jwt_auth_middleware`] — `Authorization: Bearer <token>` → `VerifiedJwt`
//!    в extensions; без настроенного `jwt_issuer` — fail-closed 503.
//! 2. [`v1_rbac_middleware`] — роль из admin-хранилища (Users) по матрице
//!    BACKEND_RULES §1.7: комнаты — чтение Viewer+/мутации Moderator+,
//!    аудит — чтение/экспорт Moderator+ (запись Operator), users/settings и
//!    прочие/неизвестные роуты — Operator (fail-closed дефолт).

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use vks_auth::jwt::VerifiedJwt;
use vks_core::Role;

use crate::state::AppState;

/// Единый конверт ошибок middleware: наружу — без деталей (BACKEND_RULES §6.4),
/// подробности — в лог (tracing).
fn deny(status: StatusCode, error_code: &str, error: &str) -> Response {
    (status, Json(serde_json::json!({ "error_code": error_code, "error": error }))).into_response()
}

/// Тип авторизации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    Jwt,
    ApiKey,
}

/// Контекст авторизации (унифицированный для JWT и API-ключей).
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub subject: String,
    pub role: Role,
    pub auth_type: AuthType,
    pub scopes: Vec<String>,
}

/// Middleware валидации JWT Bearer-токена и API-ключей.
///
/// Fail-closed (ADR-0009, ADR-0021): если auth не сконфигурирован,
/// возвращает 503 Service Unavailable, исключая несанкционированный доступ.
pub async fn jwt_auth_middleware(State(state): State<Arc<AppState>>, mut request: Request, next: Next) -> Response {
    // 1. Проверяем API-ключ в X-Api-Key или Authorization: Bearer vks_...
    let api_key_candidate =
        request.headers().get("x-api-key").and_then(|h| h.to_str().ok()).map(str::trim).or_else(|| {
            let auth = request.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
            let (scheme, token) = auth.split_once(' ')?;
            if scheme.eq_ignore_ascii_case("bearer") && token.trim().starts_with(vks_auth::api_key::KEY_PREFIX) {
                Some(token.trim())
            } else {
                None
            }
        });

    if let Some(key) = api_key_candidate {
        if key.len() != vks_auth::api_key::KEY_PREFIX.len() + vks_auth::api_key::KEY_HEX_LEN {
            return deny(StatusCode::UNAUTHORIZED, "invalid_api_key", "malformed api key");
        }
        let hash = vks_auth::api_key::sha256_hex(key.as_bytes());
        let Some(storage) = &state.storage else {
            return deny(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable", "storage is unavailable");
        };

        let key_record = match storage.get_api_key(&hash).await {
            Ok(Some(k)) => k,
            Ok(None) => match storage.get_api_key(key).await {
                Ok(Some(k)) => k,
                Ok(None) => return deny(StatusCode::UNAUTHORIZED, "invalid_api_key", "api key not found"),
                Err(e) => {
                    tracing::error!(error = %e, "storage error querying api key");
                    return deny(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "storage error");
                }
            },
            Err(e) => {
                tracing::error!(error = %e, "storage error querying api key");
                return deny(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "storage error");
            }
        };

        if key_record.revoked {
            tracing::warn!(prefix = &key[..8], "rejected revoked api key");
            return deny(StatusCode::UNAUTHORIZED, "api_key_revoked", "api key has been revoked");
        }

        let owner_str = key_record.owner.to_string();
        let owner_role = match crate::users::Users::resolve_role(&state, &owner_str) {
            Some(r) => r,
            None => return deny(StatusCode::FORBIDDEN, "forbidden", "api key owner is unknown or disabled"),
        };

        let effective_role = if key_record.scopes.iter().any(|s| s == "admin" || s == "operator" || s == "*") {
            owner_role
        } else if key_record.scopes.iter().any(|s| s == "write" || s == "moderator") {
            owner_role.min(Role::Moderator)
        } else if key_record.scopes.iter().any(|s| s == "participant") {
            owner_role.min(Role::Participant)
        } else if key_record.scopes.iter().any(|s| s == "read" || s == "viewer") {
            owner_role.min(Role::Viewer)
        } else {
            return deny(StatusCode::FORBIDDEN, "forbidden", "api key has no valid scopes");
        };

        request.extensions_mut().insert(AuthContext {
            subject: owner_str,
            role: effective_role,
            auth_type: AuthType::ApiKey,
            scopes: key_record.scopes,
        });
        return next.run(request).await;
    }

    // 2. JWT Bearer
    let Some(_issuer) = &state.jwt_issuer else {
        return deny(
            StatusCode::SERVICE_UNAVAILABLE,
            "auth_unavailable",
            "JWT authentication service is not configured",
        );
    };

    let auth_header = request.headers().get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());

    // Схема Authorization не различает регистр (RFC 7235 §2.1); пустой токен — 401.
    let bearer = auth_header.and_then(|h| {
        let (scheme, token) = h.split_once(' ')?;
        (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then(|| token.trim())
    });
    let Some(token) = bearer else {
        return deny(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid Authorization header");
    };

    let verified = match state.verify_token(token) {
        Some(v) => v,
        None => {
            return deny(StatusCode::UNAUTHORIZED, "unauthorized", "invalid, expired, or revoked token");
        }
    };

    let role = crate::users::Users::resolve_role(&state, &verified.user_id).unwrap_or(Role::Viewer);

    request.extensions_mut().insert(AuthContext {
        subject: verified.user_id.clone(),
        role,
        auth_type: AuthType::Jwt,
        scopes: vec!["*".into()],
    });
    request.extensions_mut().insert(verified);
    next.run(request).await
}

/// Матрица ADR-0022/BACKEND_RULES §1.7 для v1-поверхности админки.
///
/// Комнаты — чтение Viewer+, мутации Moderator+; аудит — чтение/экспорт Moderator+,
/// запись Operator (примечание §1.7); users/settings и **любой неизвестный роут**
/// (fail-closed дефолт) — Operator.
fn v1_required_role(method: &Method, path: &str) -> Role {
    let is_read = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);

    if path == "/api/v1/audit" || path.starts_with("/api/v1/audit/") {
        return if is_read { Role::Moderator } else { Role::Operator };
    }
    // «Управление пользователями» — Operator only (ADR-0022): bulk mute/kick
    // меняет состояние UserRecord (muted/deleted), а строка §1.7 «Мутить/удалить
    // участника» — про live-модерацию сессий (в v1-поверхности не реализуемую);
    // настройки — зона Operator.
    if path == "/api/v1/users"
        || path.starts_with("/api/v1/users/")
        || path == "/api/v1/settings"
        || path.starts_with("/api/v1/settings/")
    {
        return Role::Operator;
    }
    // Единственная безопасно-читаемая зона — комнаты; всё прочее, включая
    // будущие роуты, по умолчанию требует Operator (fail-closed).
    if path == "/api/v1/rooms" || path.starts_with("/api/v1/rooms/") {
        if is_read {
            Role::Viewer
        } else {
            Role::Moderator
        }
    } else {
        Role::Operator
    }
}

/// RBAC-middleware v1: роль берётся из admin-хранилища по субъекту токена.
///
/// Fail-closed: неизвестный/удалённый субъект — 403 (даже на чтение),
/// недостаточная роль на мутацию — 403. Ставится ПОСЛЕ
/// [`jwt_auth_middleware`] (требует `VerifiedJwt` в extensions).
pub async fn v1_rbac_middleware(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let (role, scopes) = if let Some(ctx) = request.extensions().get::<AuthContext>() {
        if ctx.auth_type == AuthType::Jwt {
            let Some(role) = crate::users::Users::resolve_role(&state, &ctx.subject) else {
                return deny(StatusCode::FORBIDDEN, "forbidden", "account is unknown or disabled");
            };
            (role, ctx.scopes.clone())
        } else {
            (ctx.role, ctx.scopes.clone())
        }
    } else if let Some(subject) = request.extensions().get::<VerifiedJwt>().map(|v| v.user_id.clone()) {
        let Some(role) = crate::users::Users::resolve_role(&state, &subject) else {
            return deny(StatusCode::FORBIDDEN, "forbidden", "account is unknown or disabled");
        };
        (role, vec!["*".into()])
    } else {
        return deny(StatusCode::UNAUTHORIZED, "unauthorized", "missing verified token context");
    };

    let is_mutation = !matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if is_mutation && !scopes.iter().any(|s| s == "*" || s == "write" || s == "admin" || s == "operator") {
        return deny(StatusCode::FORBIDDEN, "forbidden", "api key does not have write scope");
    }

    let required = v1_required_role(request.method(), request.uri().path());
    if role.level() < required.level() {
        return deny(
            StatusCode::FORBIDDEN,
            "forbidden",
            &format!("role '{}' is not allowed on this endpoint (requires '{}')", role.as_str(), required.as_str()),
        );
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{middleware, routing::get, Router};
    use tower::ServiceExt;
    use vks_auth::jwt::{JwtConfig, JwtIssuer};

    #[tokio::test]
    async fn middleware_allows_valid_jwt() {
        let config =
            JwtConfig { secret: "test_secret_key_12345".to_string(), ttl_secs: 300, issuer: "vks-test".to_string() };
        let issuer = Arc::new(JwtIssuer::new(config));
        let token = issuer.issue("user_123", None).expect("issue token");

        let mut state = AppState::new();
        state.jwt_issuer = Some(issuer);
        let shared_state = Arc::new(state);

        let app = Router::new()
            .route("/protected", get(|| async { (StatusCode::OK, "protected data") }))
            .layer(middleware::from_fn_with_state(shared_state.clone(), jwt_auth_middleware))
            .with_state(shared_state);

        // Good request with valid token
        let req = Request::builder()
            .uri("/protected")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Missing Authorization header -> 401
        let req_missing = Request::builder().uri("/protected").body(axum::body::Body::empty()).unwrap();

        let resp_missing = app.clone().oneshot(req_missing).await.unwrap();
        assert_eq!(resp_missing.status(), StatusCode::UNAUTHORIZED);

        // Bad token -> 401
        let req_bad = Request::builder()
            .uri("/protected")
            .header(header::AUTHORIZATION, "Bearer invalid.jwt.token")
            .body(axum::body::Body::empty())
            .unwrap();

        let resp_bad = app.oneshot(req_bad).await.unwrap();
        assert_eq!(resp_bad.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn middleware_bearer_scheme_is_case_insensitive() {
        let config =
            JwtConfig { secret: "test_secret_key_12345".to_string(), ttl_secs: 300, issuer: "vks-test".to_string() };
        let issuer = Arc::new(JwtIssuer::new(config));
        let token = issuer.issue("user_123", None).expect("issue token");

        let mut state = AppState::new();
        state.jwt_issuer = Some(issuer);
        let shared_state = Arc::new(state);

        let app = Router::new()
            .route("/protected", get(|| async { (StatusCode::OK, "protected data") }))
            .layer(middleware::from_fn_with_state(shared_state.clone(), jwt_auth_middleware))
            .with_state(shared_state);

        // RFC 7235 §2.1: схема не различает регистр.
        let req = Request::builder()
            .uri("/protected")
            .header(header::AUTHORIZATION, format!("bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Пустой токен и чужая схема → 401.
        for h in ["Bearer ", "Basic dXNlcjpwYXNz", "Bearer"] {
            let req = Request::builder()
                .uri("/protected")
                .header(header::AUTHORIZATION, h)
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "header {h:?}");
        }
    }

    #[test]
    fn v1_required_role_matches_matrix() {
        // Строки BACKEND_RULES §1.7 для v1-поверхности + fail-closed дефолт.
        let cases: &[(Method, &str, Role)] = &[
            (Method::GET, "/api/v1/rooms", Role::Viewer),
            (Method::HEAD, "/api/v1/rooms/123", Role::Viewer),
            (Method::POST, "/api/v1/rooms", Role::Moderator),
            (Method::PATCH, "/api/v1/rooms/123", Role::Moderator),
            (Method::DELETE, "/api/v1/rooms/123", Role::Moderator),
            (Method::GET, "/api/v1/rooms/123/mode", Role::Viewer),
            (Method::POST, "/api/v1/rooms/123/mode", Role::Moderator),
            (Method::POST, "/api/v1/mode", Role::Operator),
            (Method::GET, "/api/v1/audit", Role::Moderator),
            (Method::GET, "/api/v1/audit/export", Role::Moderator),
            (Method::POST, "/api/v1/audit", Role::Operator),
            (Method::GET, "/api/v1/users", Role::Operator),
            (Method::POST, "/api/v1/users/bulk", Role::Operator),
            (Method::GET, "/api/v1/settings", Role::Operator),
            (Method::POST, "/api/v1/settings", Role::Operator),
            // Неизвестный (будущий) роут не понижается до Viewer.
            (Method::GET, "/api/v1/api-keys", Role::Operator),
            (Method::POST, "/api/v1/api-keys", Role::Operator),
            // Граница сегмента: `/rooms-foo` — не зона комнат.
            (Method::GET, "/api/v1/rooms-foo", Role::Operator),
        ];
        for (method, path, expected) in cases {
            let actual = v1_required_role(method, path);
            assert_eq!(actual.as_str(), expected.as_str(), "{method} {path}");
        }
    }

    #[tokio::test]
    async fn middleware_fails_closed_when_issuer_is_none() {
        let state = Arc::new(AppState::new());

        let app = Router::new()
            .route("/protected", get(|| async { (StatusCode::OK, "protected data") }))
            .layer(middleware::from_fn_with_state(state.clone(), jwt_auth_middleware))
            .with_state(state);

        let req = Request::builder().uri("/protected").body(axum::body::Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    fn rbac_state(username: &str, role: Option<Role>) -> (Arc<AppState>, String) {
        let config =
            JwtConfig { secret: "test_secret_key_12345".to_string(), ttl_secs: 300, issuer: "vks-test".to_string() };
        let issuer = Arc::new(JwtIssuer::new(config));
        let token = issuer.issue(username, None).expect("issue token");
        let mut state = AppState::new();
        state.jwt_issuer = Some(issuer);
        if let Some(role) = role {
            crate::users::Users::create(&state, username, role);
        }
        (Arc::new(state), token)
    }

    fn rbac_app(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/v1/rooms", get(|| async { StatusCode::OK }).post(|| async { StatusCode::OK }))
            .route("/api/v1/users", get(|| async { StatusCode::OK }).post(|| async { StatusCode::OK }))
            .route("/api/v1/users/bulk", axum::routing::post(|| async { StatusCode::OK }))
            .route("/api/v1/audit", get(|| async { StatusCode::OK }).post(|| async { StatusCode::OK }))
            .route_layer(middleware::from_fn_with_state(state.clone(), v1_rbac_middleware))
            .route_layer(middleware::from_fn_with_state(state.clone(), jwt_auth_middleware))
            .with_state(state)
    }

    fn bearer(method: Method, uri: &str, token: &str) -> Request<axum::body::Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn rbac_viewer_reads_but_cannot_mutate() {
        let (state, token) = rbac_state("viewer", Some(Role::Viewer));
        let app = rbac_app(state);
        let ok = app.clone().oneshot(bearer(Method::GET, "/api/v1/rooms", &token)).await.unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let denied = app.oneshot(bearer(Method::POST, "/api/v1/rooms", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rbac_moderator_manages_rooms_but_not_users() {
        let (state, token) = rbac_state("moderator", Some(Role::Moderator));
        let app = rbac_app(state);

        let ok = app.clone().oneshot(bearer(Method::POST, "/api/v1/rooms", &token)).await.unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        // Управление пользователями, включая bulk mute/kick (kick = soft-delete!) — Operator only.
        let denied = app.clone().oneshot(bearer(Method::POST, "/api/v1/users/bulk", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let denied = app.clone().oneshot(bearer(Method::POST, "/api/v1/users", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let denied = app.oneshot(bearer(Method::GET, "/api/v1/users", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rbac_audit_read_requires_moderator() {
        let (state, token) = rbac_state("viewer", Some(Role::Viewer));
        let app = rbac_app(state);
        let denied = app.oneshot(bearer(Method::GET, "/api/v1/audit", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let (state, token) = rbac_state("auditor", Some(Role::Moderator));
        let app = rbac_app(state);
        let ok = app.clone().oneshot(bearer(Method::GET, "/api/v1/audit", &token)).await.unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        // Запись в аудит — админская операция.
        let denied = app.oneshot(bearer(Method::POST, "/api/v1/audit", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rbac_unknown_or_deleted_subject_is_forbidden() {
        // Валидный токен без записи в Users-хранилище.
        let (state, token) = rbac_state("ghost", None);
        let app = rbac_app(state);
        let denied = app.oneshot(bearer(Method::GET, "/api/v1/rooms", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        // Мягко удалённый пользователь — доступа нет, несмотря на валидный токен.
        let (state, token) = rbac_state("gone", None);
        let record = crate::users::Users::create(&state, "gone", Role::Operator);
        assert!(crate::users::Users::delete(&state, &record.id));
        let app = rbac_app(state);
        let denied = app.oneshot(bearer(Method::GET, "/api/v1/rooms", &token)).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn middleware_rejects_expired_token() {
        // ttl_secs = -1 → `exp` в прошлом; leeway=0, поэтому 401 сразу.
        let config =
            JwtConfig { secret: "test_secret_key_12345".to_string(), ttl_secs: -1, issuer: "vks-test".to_string() };
        let issuer = Arc::new(JwtIssuer::new(config));
        let token = issuer.issue("user_123", None).expect("issue token");

        let mut state = AppState::new();
        state.jwt_issuer = Some(issuer);
        let shared_state = Arc::new(state);

        let app = Router::new()
            .route("/protected", get(|| async { (StatusCode::OK, "protected data") }))
            .layer(middleware::from_fn_with_state(shared_state.clone(), jwt_auth_middleware))
            .with_state(shared_state);

        let req = Request::builder()
            .uri("/protected")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_key_allows_read_and_enforces_write_scope() {
        use vks_core::ports::Storage;
        let storage = Arc::new(vks_storage::MemoryStorage::new());
        let (key_plain, _key_meta) = vks_auth::api_key::generate("key-1");
        let hash = vks_auth::api_key::sha256_hex(key_plain.as_bytes());

        let state = AppState::with_storage(storage.clone());
        let user = crate::users::Users::create(&state, "key_owner", Role::Operator);
        let owner = user.id;

        // Key with read-only scope
        let core_key =
            vks_core::ports::storage::ApiKey { id: hash.clone(), scopes: vec!["read".into()], owner, revoked: false };
        storage.upsert_api_key(&core_key).await.unwrap();

        let app = rbac_app(Arc::new(state));

        // GET allowed with X-Api-Key
        let req = Request::builder()
            .uri("/api/v1/rooms")
            .method(Method::GET)
            .header("x-api-key", &key_plain)
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // POST rejected because scopes only has "read"
        let req = Request::builder()
            .uri("/api/v1/rooms")
            .method(Method::POST)
            .header("x-api-key", &key_plain)
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Revoke key -> 401 UNAUTHORIZED
        let revoked_key =
            vks_core::ports::storage::ApiKey { id: hash, scopes: vec!["read".into()], owner, revoked: true };
        storage.upsert_api_key(&revoked_key).await.unwrap();
        let req = Request::builder()
            .uri("/api/v1/rooms")
            .method(Method::GET)
            .header("x-api-key", &key_plain)
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
