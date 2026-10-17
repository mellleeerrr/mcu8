//! `vks-server` — библиотека составных частей сервера.
//!
//! Здесь — единая точка сборки HTTP-приложения [`build_app`]: admin REST
//! (`/api/v1/*`), метрики (`/metrics`), сетевой сигналинг (`/ws`, protobuf;
//! auth-at-handshake + лимиты) и статика фронтенда PNM (`/pnm/`, `/mcu/*`).
//! Используется и `main.rs` (композиционный корень), и интеграционными тестами.

pub mod frontend;

use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn};

use vks_admin::state::AppState;
use vks_common::metrics::{
    encode as encode_metrics, HTTP_REQUESTS, HTTP_REQUEST_DURATION, HTTP_RESPONSE_BYTES, SIGNALING_MESSAGES,
};
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ParticipantJoined, ParticipantLeft, SignalingMessage};
use vks_signaling::envelope::Envelope;
use vks_signaling::handler::{HandlerConfig, SignalingHandler};

use crate::frontend::{statics_rel_path, Frontend};

/// Контекст для WebSocket-сигналинга (состояние приложения + конфигурация сессии).
#[derive(Clone)]
pub struct WsContext {
    pub state: Arc<AppState>,
    pub handler_config: HandlerConfig,
}

/// Собирает полное приложение сервера с тестовой конфигурацией по умолчанию.
pub fn build_app(state: Arc<AppState>, frontend: &Frontend) -> Router {
    build_app_with_config(state, frontend, HandlerConfig::default_test())
}

/// Собирает полное приложение сервера с заданной конфигурацией обработчика сигналинга.
///
/// Слои (снаружи внутрь): HTTP-метрики → security-заголовки → `Cache-Control`/`Vary`
/// (внутри [`Frontend::router`]) → роуты. Security-заголовки — **app-wide**, включая
/// `/api/v1/*`, `/metrics` и `/ws` (§4.2 плана). Статика монтируется из `frontend`,
/// поэтому тесты гоняют ровно ту же сборку, что и `main.rs`.
pub fn build_app_with_config(
    state: Arc<AppState>,
    frontend: &Frontend,
    handler_config: HandlerConfig,
) -> Router {
    let csp = frontend.csp.clone();
    let ws_ctx = WsContext {
        state: state.clone(),
        handler_config,
    };
    // Сигналинг: auth-at-handshake и лимиты (T1.2) — внутри ws_handler.
    let signaling = Router::new().route("/ws", get(ws_handler)).with_state(ws_ctx);
    vks_admin::handlers::router(state.clone())
        // Адаптер legacy UI `/api/v3/*` (ADR-0027, фазы 2–3 плана).
        .merge(vks_admin::api_v3::router(state))
        .merge(signaling)
        .route("/metrics", get(metrics_handler))
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
///
/// Auth-at-handshake (ADR-0009, fail-closed): обязателен валидный
/// `Authorization: Bearer`; без настроенного `jwt_issuer` — 503, без/с битым
/// токеном — 401. Браузерный JS (не умеет ставить заголовки) обязан идти через
/// адаптер `/api/v3/ws`. Роль на этом шаге не проверяется: авторизация
/// конкретных сигналинг-действий — зона FSM и следующих волн.
async fn ws_handler(State(ctx): State<WsContext>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let Some(issuer) = &ctx.state.jwt_issuer else {
        return ws_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "auth_unavailable",
            "JWT authentication service is not configured",
        );
    };
    let bearer = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|h| {
        let (scheme, token) = h.split_once(' ')?;
        (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then(|| token.trim())
    });
    let Some(token) = bearer else {
        return ws_error(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid Authorization header");
    };
    if let Err(e) = issuer.verify(token) {
        tracing::debug!(error = %e, "ws jwt verification failed");
        return ws_error(StatusCode::UNAUTHORIZED, "unauthorized", "invalid or expired token");
    }
    // Транспортный потолок памяти на сообщение (см. MAX_WS_TRANSPORT_BYTES):
    // 64–256 КиБ — graceful close 4008 в handle_ws_socket, свыше — транспортный отсев.
    ws.max_message_size(MAX_WS_TRANSPORT_BYTES).on_upgrade(move |socket| handle_ws_socket(socket, ctx))
}

/// JSON-конверт ошибки handshake (до upgrade).
fn ws_error(status: StatusCode, error_code: &str, error: &str) -> Response {
    (status, Json(serde_json::json!({ "error_code": error_code, "error": error }))).into_response()
}

/// Лимит сигналинг-сообщения (protobuf-конверт), BACKEND_RULES §2.4: 64 КиБ;
/// превышение — `MessageTooLarge` (close 4008) до декодирования протокола.
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024;

/// Транспортный потолок памяти на сообщение (4× явного лимита): сообщения
/// 64–256 КиБ доходят до прикладной проверки (graceful close 4008), свыше —
/// отсекаются tungstenite'ом до сборки всего сообщения.
const MAX_WS_TRANSPORT_BYTES: usize = MAX_WS_MESSAGE_BYTES * 4;

/// Лимит частоты: не более 60 сообщений/сек на соединение.
const MAX_WS_MESSAGES_PER_SEC: usize = 60;

/// Обработка сессии WebSocket-сигналинга.
///
/// Сайд-эффекты комнаты (создание записи, счётчик участников) применяются только
/// по **успешному** результату FSM (`JoinRoomResult`/`Ack` на `LeaveRoom`):
/// отклонённый JoinRoom (например, до ClientHello) ничего не создаёт и не считает.
async fn handle_ws_socket(socket: WebSocket, ctx: WsContext) {
    let (mut sender, mut receiver) = socket.split();
    let mut handler = SignalingHandler::new(ctx.handler_config);
    let state = ctx.state;
    let mut current_room: Option<vks_core::RoomId> = None;
    let mut room_rx: Option<tokio::sync::broadcast::Receiver<vks_admin::hub::RoomBroadcastEvent>> = None;
    // Скользящее окно частоты сообщений (T1.2).
    let mut window_start = std::time::Instant::now();
    let mut window_count: usize = 0;

    'session: loop {
        tokio::select! {
            // Fan-out push событий от других участников комнаты (Downlink)
            broadcast_res = async {
                match &mut room_rx {
                    Some(rx) => Some(rx.recv().await),
                    None => std::future::pending().await,
                }
            } => {
                if let Some(res) = broadcast_res {
                    match res {
                        Ok(event) => {
                            if event.sender_participant_id.as_deref() != handler.participant_id() {
                                SIGNALING_MESSAGES.with_label_values(&["downlink"]).inc();
                                if let Err(e) = sender.send(Message::Binary(event.payload.clone())).await {
                                    warn!(error = %e, "failed to send broadcast to client");
                                    break 'session;
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "slow signaling subscriber dropped events");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            room_rx = None;
                        }
                    }
                }
            }

            msg_res = receiver.next() => {
                let Some(msg_res) = msg_res else { break 'session };
                let msg = match msg_res {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, "websocket receive error");
                        break 'session;
                    }
                };

                // Лимит частоты (T1.2 / SEC-03): окно в секунду распространяется на все входящие кадры.
                if window_start.elapsed() >= std::time::Duration::from_secs(1) {
                    window_start = std::time::Instant::now();
                    window_count = 0;
                }
                window_count += 1;
                if window_count > MAX_WS_MESSAGES_PER_SEC {
                    warn!(limit = MAX_WS_MESSAGES_PER_SEC, "signaling rate limit exceeded; closing");
                    let frame = CloseFrame { code: 1008, reason: "message rate exceeded".into() }; // 1008 Policy Violation
                    let _ = sender.send(Message::Close(Some(frame))).await;
                    break 'session;
                }

                match msg {
                    Message::Binary(bytes) => {
                        // Лимит сообщения (§2.4: protobuf ≤64 КиБ) — до декодирования протокола.
                        if bytes.len() > MAX_WS_MESSAGE_BYTES {
                            warn!(
                                size = bytes.len(),
                                limit = MAX_WS_MESSAGE_BYTES,
                                "signaling message too large; closing 4008"
                            );
                            let frame = CloseFrame { code: 4008, reason: "message too large".into() }; // 4008 MessageTooLarge
                            let _ = sender.send(Message::Close(Some(frame))).await;
                            break 'session;
                        }

                        SIGNALING_MESSAGES.with_label_values(&["uplink"]).inc();
                        match Envelope::decode(&bytes) {
                            Ok(env) => {
                                let join_name = match env.payload() {
                                    Some(Payload::JoinRoom(join)) => Some(join.room_id.clone()),
                                    _ => None,
                                };
                                let was_leave = matches!(env.payload(), Some(Payload::LeaveRoom(_)));

                                // Запрос исполняет FSM; сайд-эффекты — только по успешному результату.
                                let replies = handler.handle(env);
                                let joined_ok = replies.iter().any(|r| matches!(r.payload(), Some(Payload::JoinRoomResult(_))));
                                let left_ok = was_leave && replies.iter().any(|r| matches!(r.payload(), Some(Payload::Ack(_))));

                                if joined_ok {
                                    if let Some(name) = join_name {
                                        let rid = if let Ok(u) = uuid::Uuid::parse_str(&name) {
                                            vks_core::RoomId::from_uuid(u)
                                        } else {
                                            let mut found = None;
                                            if let Ok(rooms) = state.rooms.lock() {
                                                for (id, r) in rooms.iter() {
                                                    if r.name == name {
                                                        found = Some(*id);
                                                        break;
                                                    }
                                                }
                                            }
                                            found.unwrap_or_else(|| vks_admin::rooms::Rooms::create(&state, &name).id)
                                        };
                                        current_room = Some(rid);
                                        room_rx = Some(state.room_hub.subscribe(&rid));
                                        let count =
                                            vks_admin::rooms::Rooms::get(&state, &rid).map(|r| r.participants + 1).unwrap_or(1);
                                        vks_admin::rooms::Rooms::update(&state, &rid, None, None, Some(count), None);

                                        // Рассылаем ParticipantJoined другим участникам комнаты
                                        if let Some(pid) = handler.participant_id() {
                                            let joined_msg = SignalingMessage {
                                                msg_id: 0,
                                                reply_to: 0,
                                                payload: Some(Payload::ParticipantJoined(ParticipantJoined {
                                                    participant_id: pid.to_string(),
                                                    display_name: pid.to_string(),
                                                    is_recorder: false,
                                                })),
                                            };
                                            let payload_bytes = Envelope::new(joined_msg).encode();
                                            state.room_hub.broadcast(&rid, Some(pid), payload_bytes.into());
                                        }

                                        // Уведомляем админский WebSocket (/api/v3/ws)
                                        state.notify_admin("conferences:update", serde_json::json!({
                                            "id": rid.to_string(),
                                            "participants_count": count
                                        }));
                                    }
                                } else if left_ok {
                                    if let Some(rid) = current_room.take() {
                                        room_rx = None;
                                        state.room_hub.unsubscribe(&rid);
                                        if let Some(room) = vks_admin::rooms::Rooms::get(&state, &rid) {
                                            let new_count = room.participants.saturating_sub(1);
                                            vks_admin::rooms::Rooms::update(&state, &rid, None, None, Some(new_count), None);

                                            if let Some(pid) = handler.participant_id() {
                                                let left_msg = SignalingMessage {
                                                    msg_id: 0,
                                                    reply_to: 0,
                                                    payload: Some(Payload::ParticipantLeft(ParticipantLeft {
                                                        participant_id: pid.to_string(),
                                                        reason: 0,
                                                    })),
                                                };
                                                let payload_bytes = Envelope::new(left_msg).encode();
                                                state.room_hub.broadcast(&rid, Some(pid), payload_bytes.into());
                                            }

                                            state.notify_admin("conferences:update", serde_json::json!({
                                                "id": rid.to_string(),
                                                "participants_count": new_count
                                            }));
                                        }
                                    }
                                }

                                for reply in replies {
                                    SIGNALING_MESSAGES.with_label_values(&["downlink"]).inc();
                                    let reply_bytes = reply.encode();
                                    if let Err(e) = sender.send(Message::Binary(reply_bytes.into())).await {
                                        warn!(error = %e, "failed to send signaling reply");
                                        break 'session; // не `return`: ниже — очистка счётчика комнаты
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to decode protobuf signaling envelope");
                            }
                        }
                    }
                    Message::Text(text) => {
                        // Сигналинг — только бинарный протокол; текстовые кадры не
                        // обрабатываем, но ограничиваем их размер тем же лимитом.
                        if text.len() > MAX_WS_MESSAGE_BYTES {
                            warn!(
                                size = text.len(),
                                limit = MAX_WS_MESSAGE_BYTES,
                                "signaling text message too large; closing 4008"
                            );
                            let frame = CloseFrame { code: 4008, reason: "message too large".into() }; // 4008 MessageTooLarge
                            let _ = sender.send(Message::Close(Some(frame))).await;
                            break 'session;
                        }
                    }
                    Message::Ping(payload) => {
                        if let Err(e) = sender.send(Message::Pong(payload)).await {
                            warn!(error = %e, "failed to send pong reply");
                            break 'session;
                        }
                    }
                    Message::Close(_) => {
                        info!("websocket client initiated close");
                        break 'session;
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(rid) = current_room.take() {
        state.room_hub.unsubscribe(&rid);
        if let Some(room) = vks_admin::rooms::Rooms::get(&state, &rid) {
            let new_count = room.participants.saturating_sub(1);
            vks_admin::rooms::Rooms::update(&state, &rid, None, None, Some(new_count), None);

            if let Some(pid) = handler.participant_id() {
                let left_msg = SignalingMessage {
                    msg_id: 0,
                    reply_to: 0,
                    payload: Some(Payload::ParticipantLeft(ParticipantLeft {
                        participant_id: pid.to_string(),
                        reason: 0,
                    })),
                };
                let payload_bytes = Envelope::new(left_msg).encode();
                state.room_hub.broadcast(&rid, Some(pid), payload_bytes.into());
            }

            state.notify_admin("conferences:update", serde_json::json!({
                "id": rid.to_string(),
                "participants_count": new_count
            }));
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
