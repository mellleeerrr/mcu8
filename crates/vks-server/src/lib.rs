//! `vks-server` — библиотека составных частей сервера.
//!
//! Здесь — единая точка сборки HTTP-приложения [`build_app`]: admin REST
//! (`/api/v1/*`), метрики (`/metrics`), сетевой сигналинг (`/ws`, protobuf) и
//! статика фронтенда PNM (`/pnm/`, `/mcu/*`) на [`frontend::Frontend`].
//! Используется и `main.rs` (композиционный корень), и интеграционными тестами.

pub mod frontend;

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn};

use vks_admin::state::AppState;
use vks_common::metrics::{
    encode as encode_metrics, HTTP_REQUESTS, HTTP_REQUEST_DURATION, HTTP_RESPONSE_BYTES, SIGNALING_MESSAGES,
};
use vks_signaling::envelope::Envelope;
use vks_signaling::handler::{HandlerConfig, SignalingHandler};

use crate::frontend::{statics_rel_path, Frontend};

/// Собирает полное приложение сервера.
///
/// Слои (снаружи внутрь): HTTP-метрики → security-заголовки → `Cache-Control`/`Vary`
/// (внутри [`Frontend::router`]) → роуты. Security-заголовки — **app-wide**, включая
/// `/api/v1/*`, `/metrics` и `/ws` (§4.2 плана). Статика монтируется из `frontend`,
/// поэтому тесты гоняют ровно ту же сборку, что и `main.rs`.
pub fn build_app(state: Arc<AppState>, frontend: &Frontend) -> Router {
    let csp = frontend.csp.clone();
    let ws_state = state.clone();
    vks_admin::handlers::router(state.clone())
        // Адаптер legacy UI `/api/v3/*` (ADR-0027, фазы 2–3 плана).
        .merge(vks_admin::api_v3::router(state))
        .route("/metrics", get(metrics_handler))
        .route("/ws", get(move |ws| ws_handler(ws, ws_state)))
        .merge(frontend.router())
        .layer(axum::middleware::from_fn(move |request: Request, next: Next| {
            let csp = csp.clone();
            async move { crate::frontend::with_security_headers(csp, request, next).await }
        }))
        .layer(axum::middleware::from_fn(http_metrics_middleware))
}

/// Класс запроса для метрик (`vks_http_requests_total` и гистограмм).
///
/// Классы ограничивают кардинальность меток: сырые пути в метки не попадают.
/// `static` — файловые запросы статики (последний сегмент похож на имя файла),
/// `spa` — навигация SPA/редиректы UI, `health` — `/healthz`/`/readyz`.
pub fn classify_path(path: &str) -> &'static str {
    if path == "/metrics" {
        return "metrics";
    }
    if path == "/ws" {
        return "ws";
    }
    if path == "/healthz" || path == "/readyz" {
        return "health";
    }
    if path.starts_with("/api/v1") {
        return "api_v1";
    }
    if path.starts_with("/api/v3") {
        return "api_v3";
    }
    if path == "/" || path.starts_with("/admin") {
        return "spa";
    }
    if let Some(rel) = statics_rel_path(path) {
        let last = rel.rsplit('/').next().unwrap_or("");
        if !rel.is_empty() && !rel.ends_with('/') && last.contains('.') {
            return "static";
        }
        return "spa";
    }
    "other"
}

/// Разрешённые значения метки `method` (кардинальность, как у `class`).
const METRIC_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Слой HTTP-метрик: счётчик по `(method, class, status)`, длительность и байты
/// тела ответа по классу. Байты учитываются по `Content-Length` (потоковые
/// ответы без него — не учитываются; для статики/API этого достаточно).
///
/// `method` whitelist-ится (как и `class` по путям): произвольные методы
/// (расширения, мусор) не раздувают кардинальность — попадают в `other`.
async fn http_metrics_middleware(request: Request, next: Next) -> Response {
    let class = classify_path(request.uri().path());
    let raw_method = request.method().clone();
    let method = METRIC_METHODS.iter().find(|m| **m == raw_method.as_str()).copied().unwrap_or("other");
    let started = std::time::Instant::now();

    let response = next.run(request).await;

    let status = response.status().as_u16().to_string();
    HTTP_REQUESTS.with_label_values(&[method, class, status.as_str()]).inc();
    HTTP_REQUEST_DURATION.with_label_values(&[class]).observe(started.elapsed().as_secs_f64());

    let bytes = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if bytes > 0 {
        HTTP_RESPONSE_BYTES.with_label_values(&[class]).inc_by(bytes);
    }
    response
}

/// Эндпоинт сбора Prometheus-метрик.
async fn metrics_handler() -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")], encode_metrics())
        .into_response()
}

/// Эндпоинт WebSocket-сигналинга.
async fn ws_handler(ws: WebSocketUpgrade, state: Arc<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_ws_socket(socket, state))
}

/// Обработка сессии WebSocket-сигналинга.
async fn handle_ws_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut handler = SignalingHandler::new(HandlerConfig::default_test());
    let mut current_room: Option<vks_core::RoomId> = None;

    while let Some(msg_res) = receiver.next().await {
        let msg = match msg_res {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "websocket receive error");
                break;
            }
        };

        match msg {
            Message::Binary(bytes) => {
                SIGNALING_MESSAGES.with_label_values(&["uplink"]).inc();
                match Envelope::decode(&bytes) {
                    Ok(env) => {
                        if let Some(vks_proto::signaling::signaling_message::Payload::JoinRoom(join)) = env.payload() {
                            let rid = if let Ok(u) = uuid::Uuid::parse_str(&join.room_id) {
                                vks_core::RoomId::from_uuid(u)
                            } else {
                                let mut found = None;
                                if let Ok(rooms) = state.rooms.lock() {
                                    for (id, r) in rooms.iter() {
                                        if r.name == join.room_id {
                                            found = Some(*id);
                                            break;
                                        }
                                    }
                                }
                                found.unwrap_or_else(|| {
                                    let r = vks_admin::rooms::Rooms::create(&state, &join.room_id);
                                    r.id
                                })
                            };
                            current_room = Some(rid);
                            let count =
                                vks_admin::rooms::Rooms::get(&state, &rid).map(|r| r.participants + 1).unwrap_or(1);
                            vks_admin::rooms::Rooms::update(&state, &rid, None, None, Some(count), None);
                        } else if let Some(vks_proto::signaling::signaling_message::Payload::LeaveRoom(_)) =
                            env.payload()
                        {
                            if let Some(rid) = current_room.take() {
                                if let Some(room) = vks_admin::rooms::Rooms::get(&state, &rid) {
                                    let new_count = room.participants.saturating_sub(1);
                                    vks_admin::rooms::Rooms::update(&state, &rid, None, None, Some(new_count), None);
                                }
                            }
                        }

                        let replies = handler.handle(env);
                        for reply in replies {
                            SIGNALING_MESSAGES.with_label_values(&["downlink"]).inc();
                            let reply_bytes = reply.encode();
                            if let Err(e) = sender.send(Message::Binary(reply_bytes.into())).await {
                                warn!(error = %e, "failed to send signaling reply");
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to decode protobuf signaling envelope");
                    }
                }
            }
            Message::Ping(payload) => {
                if let Err(e) = sender.send(Message::Pong(payload)).await {
                    warn!(error = %e, "failed to send pong reply");
                    break;
                }
            }
            Message::Close(_) => {
                info!("websocket client initiated close");
                break;
            }
            _ => {}
        }
    }

    if let Some(rid) = current_room.take() {
        if let Some(room) = vks_admin::rooms::Rooms::get(&state, &rid) {
            let new_count = room.participants.saturating_sub(1);
            vks_admin::rooms::Rooms::update(&state, &rid, None, None, Some(new_count), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    #[test]
    fn classify_path_matrix() {
        assert_eq!(classify_path("/"), "spa");
        assert_eq!(classify_path("/admin"), "spa");
        assert_eq!(classify_path("/admin/"), "spa");
        assert_eq!(classify_path("/pnm"), "spa");
        assert_eq!(classify_path("/pnm/"), "spa");
        assert_eq!(classify_path("/pnm/authorization"), "spa");
        assert_eq!(classify_path("/pnm/chunks/index.bundle-2.2.1.66.js"), "static");
        assert_eq!(classify_path("/mcu/assets/fonts/DxNvdLoR.ttf"), "static");
        assert_eq!(classify_path("/metrics"), "metrics");
        assert_eq!(classify_path("/ws"), "ws");
        assert_eq!(classify_path("/healthz"), "health");
        assert_eq!(classify_path("/readyz"), "health");
        assert_eq!(classify_path("/api/v1/rooms"), "api_v1");
        assert_eq!(classify_path("/api/v3/login"), "api_v3");
        assert_eq!(classify_path("/openapi.json"), "other");
        assert_eq!(classify_path("/favicon.ico"), "other");
        // Граница сегмента: `/pnmx` — не статика (не монтировано).
        assert_eq!(classify_path("/pnmx"), "other");
    }

    #[tokio::test]
    async fn http_metrics_middleware_records_requests() {
        let router = Router::new()
            .route("/probe-metrics", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(http_metrics_middleware));

        let series = HTTP_REQUESTS.with_label_values(&["GET", "other", "200"]);
        let before = series.get();

        let request = HttpRequest::builder().uri("/probe-metrics").body(Body::empty()).expect("build request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);

        assert!(
            HTTP_REQUESTS.with_label_values(&["GET", "other", "200"]).get() > before,
            "counter must increase for GET /probe-metrics (class=other)"
        );
    }

    #[tokio::test]
    async fn http_metrics_middleware_whitelists_method() {
        let router = Router::new()
            .route("/probe-method", axum::routing::any(|| async { "ok" }))
            .layer(axum::middleware::from_fn(http_metrics_middleware));

        // Нестандартный метод (валидный HTTP-токен) не должен попадать в метку как есть.
        let request =
            HttpRequest::builder().method("PROPFIND").uri("/probe-method").body(Body::empty()).expect("build request");
        let response = router.oneshot(request).await.expect("oneshot");
        assert!(response.status().is_success() || response.status() == StatusCode::METHOD_NOT_ALLOWED);

        let other = HTTP_REQUESTS.with_label_values(&["other", "other", "200"]).get();
        let propfind = HTTP_REQUESTS.with_label_values(&["PROPFIND", "other", "200"]).get();
        assert!(other > 0, "PROPFIND must be aggregated under method=other");
        assert_eq!(propfind, 0, "raw non-whitelisted method must never appear as a label");
    }
}
