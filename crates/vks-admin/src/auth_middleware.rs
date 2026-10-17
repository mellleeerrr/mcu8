//! JWT-middleware аутентификации для маршрутов vks-admin.
//!
//! Основание: ADR-0009, ADR-0021, BACKEND_RULES §1.1.
//! Проверяет заголовок `Authorization: Bearer <token>`.
//! Если в AppState настроен `jwt_issuer`, невалидные или отсутствующие токены
//! отклоняются со статусом 401 Unauthorized.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use crate::state::AppState;

/// Middleware валидации JWT Bearer-токена.
pub async fn jwt_auth_middleware(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    if let Some(issuer) = &state.jwt_issuer {
        let auth_header = request.headers().get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());

        let token = match auth_header.and_then(|h| h.strip_prefix("Bearer ")) {
            Some(t) => t.trim(),
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error_code": "unauthorized",
                        "error": "missing or invalid Authorization header"
                    })),
                )
                    .into_response();
            }
        };

        if let Err(e) = issuer.verify(token) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error_code": "unauthorized",
                    "error": format!("token verification failed: {e}")
                })),
            )
                .into_response();
        }
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
}
