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

/// Middleware валидации JWT Bearer-токена.
///
/// Fail-closed (ADR-0009, ADR-0021): если jwt_issuer не сконфигурирован,
/// возвращает 503 Service Unavailable, исключая несанкционированный доступ.
pub async fn jwt_auth_middleware(State(state): State<Arc<AppState>>, mut request: Request, next: Next) -> Response {
    let Some(issuer) = &state.jwt_issuer else {
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

    let verified = match issuer.verify(token) {
        Ok(v) => v,
        Err(e) => {
            // Детали (тип ошибки/JTI) — только в лог; наружу generic (BACKEND_RULES §6.4).
            tracing::debug!(error = %e, "jwt verification failed");
            return deny(StatusCode::UNAUTHORIZED, "unauthorized", "invalid or expired token");
        }
    };

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
    let Some(subject) = request.extensions().get::<VerifiedJwt>().map(|v| v.user_id.clone()) else {
        return deny(StatusCode::UNAUTHORIZED, "unauthorized", "missing verified token context");
    };

    let Some(role) = crate::users::Users::resolve_role(&state, &subject) else {
        return deny(StatusCode::FORBIDDEN, "forbidden", "account is unknown or disabled");
    };

    let required = v1_required_role(request.method(), request.uri().path());
    if role.level() < required.level() {
        return deny(
            StatusCode::FORBIDDEN,
            "forbidden",
            &format!(
                "role '{}' is not allowed on this endpoint (requires '{}')",
                role.as_str(),
                required.as_str()
            ),
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
}
