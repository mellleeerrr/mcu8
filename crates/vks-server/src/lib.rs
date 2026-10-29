//! `vks-server` — библиотека составных частей сервера.
//!
//! Здесь — единая точка сборки HTTP-приложения [`build_app`]: admin REST
//! (`/api/v1/*`), метрики (`/metrics`), сетевой сигналинг (`/ws`, protobuf;
//! auth-at-handshake + лимиты) и статика фронтенда PNM (`/pnm/`, `/mcu/*`).
//! Используется и `main.rs` (композиционный корень), и интеграционными тестами.

pub mod cluster;
pub mod codec_policy;
pub mod farm_metrics;
pub mod frontend;
pub mod gateways;
pub mod layout_signal;
pub mod operator_policy;
pub mod recorder;
pub mod room_events;

pub use cluster::ClusterManager;
pub use gateways::GatewayManager;
pub use recorder::RecorderManager;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn};

use vks_admin::state::AppState;
use vks_auth::jwt::VerifiedJwt;
use vks_common::metrics::{
    encode as encode_metrics, HTTP_REQUESTS, HTTP_REQUEST_DURATION, HTTP_RESPONSE_BYTES, SIGNALING_MESSAGES,
};
use vks_core::ports::media_engine::MediaEngine;
use vks_core::Role;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ParticipantJoined, ParticipantLeft, SignalingMessage};
use vks_signaling::envelope::Envelope;
use vks_signaling::handler::{HandlerConfig, SignalingHandler};

use crate::frontend::{statics_rel_path, Frontend};

pub mod transcoder_runtime;
pub mod webrtc_runtime;

/// Контекст для WebSocket-сигналинга (состояние приложения + конфигурация сессии + медиа-движки SFU/MCU + UDP-луп + сервисы M5).
#[derive(Clone)]
pub struct WsContext {
    pub state: Arc<AppState>,
    pub handler_config: HandlerConfig,
    pub sfu_engine: Arc<vks_media_sfu::SfuEngine>,
    pub mcu_engine: Arc<vks_media_mcu::McuEngine>,
    pub udp_loop: Option<Arc<vks_media_sfu::udp_loop::UdpMediaLoop>>,
    pub gateways: Arc<GatewayManager>,
    pub recorder: Arc<RecorderManager>,
    pub cluster: Arc<ClusterManager>,
}

/// Собирает полное приложение сервера с тестовой конфигурацией по умолчанию.
pub fn build_app(state: Arc<AppState>, frontend: &Frontend) -> Router {
    build_app_with_config(state, frontend, HandlerConfig::default_test())
}

/// Собирает полное приложение сервера с заданной конфигурацией обработчика сигналинга.
pub fn build_app_with_config(state: Arc<AppState>, frontend: &Frontend, handler_config: HandlerConfig) -> Router {
    build_app_with_engine(state, frontend, handler_config, Arc::new(vks_media_sfu::SfuEngine::new()))
}

/// Собирает полное приложение сервера с заданной конфигурацией сигналинга и инстансом SFU-движка.
pub fn build_app_with_engine(
    state: Arc<AppState>,
    frontend: &Frontend,
    handler_config: HandlerConfig,
    sfu_engine: Arc<vks_media_sfu::SfuEngine>,
) -> Router {
    build_app_full(state, frontend, handler_config, sfu_engine, None)
}

/// Собирает полное приложение сервера с полной поддержкой WebRTC (SFU + UDP медиа-луп).
pub fn build_app_full(
    state: Arc<AppState>,
    frontend: &Frontend,
    handler_config: HandlerConfig,
    sfu_engine: Arc<vks_media_sfu::SfuEngine>,
    udp_loop: Option<Arc<vks_media_sfu::udp_loop::UdpMediaLoop>>,
) -> Router {
    build_app_with_mcu(state, frontend, handler_config, sfu_engine, Arc::new(vks_media_mcu::McuEngine::new()), udp_loop)
}

/// Собирает полное приложение сервера с полной поддержкой WebRTC (SFU + MCU + UDP медиа-луп).
pub fn build_app_with_mcu(
    state: Arc<AppState>,
    frontend: &Frontend,
    handler_config: HandlerConfig,
    sfu_engine: Arc<vks_media_sfu::SfuEngine>,
    mcu_engine: Arc<vks_media_mcu::McuEngine>,
    udp_loop: Option<Arc<vks_media_sfu::udp_loop::UdpMediaLoop>>,
) -> Router {
    build_app_with_services(
        state,
        frontend,
        handler_config,
        sfu_engine,
        mcu_engine.clone(),
        udp_loop,
        Arc::new(GatewayManager::new().with_mcu(mcu_engine)),
        Arc::new(RecorderManager::new()),
        Arc::new(ClusterManager::new("vks-node-1")),
    )
}

/// Собирает полное приложение сервера с полной поддержкой всех сервисов M5 (шлюзы, рекордер, кластер) и медиа-движков SFU/MCU (Package D).
#[allow(clippy::too_many_arguments)]
pub fn build_app_with_services(
    state: Arc<AppState>,
    frontend: &Frontend,
    handler_config: HandlerConfig,
    sfu_engine: Arc<vks_media_sfu::SfuEngine>,
    mcu_engine: Arc<vks_media_mcu::McuEngine>,
    udp_loop: Option<Arc<vks_media_sfu::udp_loop::UdpMediaLoop>>,
    gateways: Arc<GatewayManager>,
    recorder: Arc<RecorderManager>,
    cluster: Arc<ClusterManager>,
) -> Router {
    gateways.bind_state(Arc::downgrade(&state));
    state.set_dialer(gateways.clone());
    // S12: события движка (речь, пересадка главного окна) доходят до клиентов.
    room_events::install(&state, &mcu_engine);
    // `P4a`: метрики фермы энкодеров (`encoder_group_*`/`viewer_rtp_*`) и
    // JSON-снимок `/api/v3/mcu/farm` для этого движка.
    farm_metrics::install(&mcu_engine);
    let farm_engine = mcu_engine.clone();

    let csp = frontend.csp.clone();
    let ws_ctx = WsContext {
        state: state.clone(),
        handler_config,
        sfu_engine,
        mcu_engine,
        udp_loop,
        gateways: gateways.clone(),
        recorder: recorder.clone(),
        cluster: cluster.clone(),
    };
    // Сигналинг: auth-at-handshake и лимиты (T1.2) — внутри ws_handler.
    let signaling = Router::new().route("/ws", get(ws_handler)).with_state(ws_ctx);

    // Дополнительные API M5 эндпоинты для мониторинга шлюзов и статуса кластера
    let m5_api = Router::new()
        .route(
            "/api/v3/gateways/status",
            get(move || {
                let gw = gateways.clone();
                async move {
                    Json(serde_json::json!({
                        "sip": gw.sip_status(),
                        "h323": gw.h323_status()
                    }))
                }
            }),
        )
        .route(
            "/api/v3/mcu/farm",
            get(move || {
                let engine = farm_engine.clone();
                async move { Json(farm_metrics::json_from_stats(&engine.video_farm_stats())) }
            }),
        )
        .route(
            "/api/v3/cluster/status",
            get(move || {
                let cl = cluster.clone();
                async move {
                    let now_sec = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    Json(cl.status(now_sec))
                }
            }),
        )
        // P1-7: служебные endpoint'ы отдают топологию/статус → Moderator+.
        // Анонимный запрос из LAN получает 401, а не 200 (было в аудите).
        .route_layer(axum::middleware::from_fn(require_moderator))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), vks_admin::api_v3::auth::token_middleware));

    // `/metrics`: Prometheus на этом же узле скрейпит с loopback без токена;
    // удалённый скрейп — через `VKS_METRICS_TOKEN` (Bearer или X-Metrics-Token).
    let metrics_router =
        Router::new().route("/metrics", get(metrics_handler)).route_layer(axum::middleware::from_fn(metrics_guard));

    vks_admin::handlers::router(state.clone())
        // Адаптер legacy UI `/api/v3/*` (ADR-0027, фазы 2–3 плана).
        .merge(vks_admin::api_v3::router(state))
        .merge(signaling)
        .merge(m5_api)
        .merge(metrics_router)
        .merge(frontend.router())
        .layer(axum::middleware::from_fn(move |request: Request, next: Next| {
            let csp = csp.clone();
            async move { crate::frontend::with_security_headers(csp, request, next).await }
        }))
        .layer(axum::middleware::from_fn(http_metrics_middleware))
}

/// Конверт ошибки API v3 (совпадает по форме с `vks_admin::api_v3`).
fn api_error(status: StatusCode, code: u16, description: &str) -> Response {
    (status, Json(serde_json::json!({"error": {"code": code, "description": description}}))).into_response()
}

/// `m5_api` (`/api/v3/gateways/status`, `/api/v3/mcu/farm`, `/api/v3/cluster/status`):
/// после `token_middleware` требуем Moderator+ — это операционные данные
/// (топология комнат, статус шлюзов), а не публичный API.
async fn require_moderator(request: Request, next: Next) -> Response {
    let role = request.extensions().get::<vks_admin::auth_middleware::AuthContext>().map(|ctx| ctx.role);
    match role {
        Some(role) if role >= Role::Moderator => next.run(request).await,
        Some(_) => api_error(StatusCode::FORBIDDEN, 403, "Forbidden: Moderator role required"),
        // token_middleware всегда кладёт AuthContext при успехе; отсутствие —
        // значит слой собран неверно, но fail-closed важнее диагностики.
        None => api_error(StatusCode::UNAUTHORIZED, 401, "Unauthorized"),
    }
}

/// Решение по доступу к `/metrics` (чистая функция — тестируется без env):
/// - `expected` задан (`VKS_METRICS_TOKEN`) → токен обязателен для всех, включая
///   loopback (иначе same-host reverse-proxy, отдающий запросы с 127.0.0.1,
///   снова открыл бы метрики наружу); неверный/пустой — 401;
/// - токен не задан → только loopback, иначе 403.
fn metrics_access(
    expected: Option<&str>,
    provided: &[String],
    loopback: bool,
) -> Result<(), (StatusCode, &'static str)> {
    match expected {
        Some(expected) => {
            use sha2::{Digest, Sha256};
            let matches = provided.iter().any(|provided| {
                !provided.is_empty() && Sha256::digest(expected.as_bytes()) == Sha256::digest(provided.as_bytes())
            });
            if matches {
                Ok(())
            } else {
                Err((StatusCode::UNAUTHORIZED, "metrics token required"))
            }
        }
        None if loopback => Ok(()),
        None => Err((
            StatusCode::FORBIDDEN,
            "metrics are available from loopback only; set VKS_METRICS_TOKEN for remote scrape",
        )),
    }
}

/// `/metrics`: см. [`metrics_access`]. Токен принимается как
/// `Authorization: Bearer <t>` или `X-Metrics-Token: <t>` (Prometheus умеет
/// первое, ручной curl — второе).
async fn metrics_guard(request: Request, next: Next) -> Response {
    let expected =
        std::env::var("VKS_METRICS_TOKEN").ok().map(|token| token.trim().to_string()).filter(|token| !token.is_empty());
    let provided = metrics_tokens(request.headers());
    let loopback = request.extensions().get::<ConnectInfo<SocketAddr>>().is_some_and(|info| info.0.ip().is_loopback());
    match metrics_access(expected.as_deref(), &provided, loopback) {
        Ok(()) => next.run(request).await,
        Err((status, description)) => api_error(status, status.as_u16(), description),
    }
}

/// Токены из запроса: `Authorization: Bearer …` и/или `X-Metrics-Token`.
/// Принимаем любой из заголовков, поэтому неверный Bearer не глушит корректный
/// `X-Metrics-Token` (и наоборот); пустые/пробельные значения отбрасываются.
fn metrics_tokens(headers: &HeaderMap) -> Vec<String> {
    [
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer ")),
        headers.get("x-metrics-token").and_then(|value| value.to_str().ok()),
    ]
    .into_iter()
    .flatten()
    .map(|token| token.trim().to_string())
    .filter(|token| !token.is_empty())
    .collect()
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
    // `P4a`: снимок фермы перед выгрузкой (гейджи текущих групп; серии
    // ретированных групп удаляются внутри `update`).
    farm_metrics::update();
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
#[derive(Debug, serde::Deserialize)]
struct WsQuery {
    /// Short-lived JWT supplied by the browser bootstrap; browsers cannot set
    /// the Authorization header on WebSocket. Header auth remains supported.
    token: Option<String>,
}

async fn ws_handler(
    State(ctx): State<WsContext>,
    headers: HeaderMap,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(_issuer) = &ctx.state.jwt_issuer else {
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
    let query_token = query.token.as_deref().map(str::trim).filter(|token| !token.is_empty());
    let bearer = bearer.or(query_token);
    let Some(token) = bearer else {
        return ws_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid Authorization header or ?token= query parameter",
        );
    };
    if query_token.is_some() {
        // A query token is the browser-compatible path; browsers send Origin.
        // Reject cross-site WebSocket hijacking unless explicitly allowlisted.
        if let Some(origin) = headers.get(header::ORIGIN).and_then(|value| value.to_str().ok()) {
            let authority = origin
                .strip_prefix("http://")
                .or_else(|| origin.strip_prefix("https://"))
                .unwrap_or(origin)
                .trim_end_matches('/');
            let host = headers.get(header::HOST).and_then(|value| value.to_str().ok()).unwrap_or_default();
            let allowlisted = std::env::var("VKS_WS_ORIGIN_ALLOW")
                .ok()
                .is_some_and(|list| list.split(',').map(str::trim).any(|entry| entry == origin || entry == authority));
            if !allowlisted && !authority.eq_ignore_ascii_case(host) {
                return ws_error(StatusCode::FORBIDDEN, "forbidden", "WebSocket Origin is not allowed");
            }
        }
    }
    let Some(verified) = ctx.state.verify_token(token) else {
        tracing::debug!("ws jwt verification failed or token revoked");
        return ws_error(StatusCode::UNAUTHORIZED, "unauthorized", "invalid, expired, or revoked token");
    };
    if let Ok(users) = ctx.state.users.lock() {
        if let Some(u) = users.values().find(|u| u.id.to_string() == verified.user_id || u.username == verified.user_id)
        {
            if u.deleted {
                return ws_error(StatusCode::FORBIDDEN, "forbidden", "user account is deactivated");
            }
        }
    }
    let role = vks_admin::users::Users::resolve_role(&ctx.state, &verified.user_id).unwrap_or(Role::Participant);
    // Транспортный потолок памяти на сообщение (см. MAX_WS_TRANSPORT_BYTES):
    // 64–256 КиБ — graceful close 4008 в handle_ws_socket, свыше — транспортный отсев.
    ws.max_message_size(MAX_WS_TRANSPORT_BYTES).on_upgrade(move |socket| handle_ws_socket(socket, ctx, verified, role))
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

/// Парсит IP и порт из строки SDP ICE-кандидата.
fn parse_ice_candidate_addr(candidate: &str) -> Option<std::net::SocketAddr> {
    let s = candidate.strip_prefix("candidate:").unwrap_or(candidate).trim();
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() >= 6 {
        let ip = parts[4];
        let port = parts[5].parse::<u16>().ok()?;
        format!("{ip}:{port}").parse().ok()
    } else {
        None
    }
}

/// Ищет UUID в произвольном идентификаторе: целиком, в токене или скользящим
/// окном 36 байт (UTF-8-safe). `None` — UUID нет, вызывающий решает сам
/// (хешировать нельзя: слот раскладки никогда не совпадёт с реальным участником).
///
/// Одного суффикса недостаточно: inbound-абонент — `sip-<uuid>-in@sip-gateway`,
/// у него UUID не в конце строки.
pub(crate) fn embedded_uuid(value: &str) -> Option<uuid::Uuid> {
    if let Ok(uuid) = uuid::Uuid::parse_str(value) {
        return Some(uuid);
    }
    // Токены `sip-<uuid>-in`, `part-room-<uuid>`, `urn:uuid:<uuid>`:
    // разделитель — всё, что не hex-цифра и не дефис.
    for token in value.split(|c: char| !(c.is_ascii_hexdigit() || c == '-')) {
        if let Ok(uuid) = uuid::Uuid::parse_str(token) {
            return Some(uuid);
        }
    }
    // UUID «вклеен» в строку без разделителей — скользящее окно только по
    // границам UTF-8 (byte-слайс на не-ASCII строке паниковал бы).
    for start in 0..=value.len().saturating_sub(36) {
        let Some(window) = value.get(start..start + 36) else { continue };
        if let Ok(uuid) = uuid::Uuid::parse_str(window) {
            return Some(uuid);
        }
    }
    None
}

/// Извлекает стабильный ParticipantId из строки сессии (детерминированный UUIDv5 для псевдонимов).
pub fn parse_participant_id(pid_str: &str) -> vks_core::ParticipantId {
    let u = embedded_uuid(pid_str).unwrap_or_else(|| {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(pid_str.as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x50; // Version 5
        bytes[8] = (bytes[8] & 0x3f) | 0x80; // Variant RFC 4122
        uuid::Uuid::from_bytes(bytes)
    });
    vks_core::ParticipantId::from_uuid(u)
}

#[derive(Debug, Clone)]
struct ActiveMembership {
    room_id: vks_core::RoomId,
    participant_id: String,
}

async fn cleanup_active_membership(
    membership: &mut Option<ActiveMembership>,
    sfu_engine: &vks_media_sfu::SfuEngine,
    mcu_engine: &vks_media_mcu::McuEngine,
    state: &AppState,
    reason: u64,
) {
    if let Some(m) = membership.take() {
        let pid_val = parse_participant_id(&m.participant_id);
        crate::webrtc_runtime::remove(m.room_id, pid_val);
        let _ = sfu_engine.remove_subscriber(m.room_id, pid_val).await;
        let _ = sfu_engine.remove_publisher(m.room_id, pid_val).await;
        let _ = mcu_engine.remove_subscriber(m.room_id, pid_val).await;
        let _ = mcu_engine.remove_publisher(m.room_id, pid_val).await;
        vks_admin::api_v3::call_strings::note_participant_left(state, &m.room_id, &m.participant_id, "Ended By Remote");
        let rem_opt = state.room_hub.unregister_participant(&m.room_id, &m.participant_id);
        state.room_hub.unsubscribe(&m.room_id);
        let count = rem_opt.unwrap_or(0);
        vks_admin::rooms::Rooms::update(state, &m.room_id, None, None, Some(count), None);

        let left_msg = SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::ParticipantLeft(ParticipantLeft {
                participant_id: m.participant_id.clone(),
                reason,
            })),
        };
        let payload_bytes = Envelope::new(left_msg).encode();
        state.room_hub.broadcast(&m.room_id, Some(&m.participant_id), payload_bytes.into());

        state.notify_admin(
            "conferences:update",
            serde_json::json!({
                "id": m.room_id.to_string(),
                "participants_count": count
            }),
        );
    }
}

/// `b=AS` для SFU-ответа из живого профиля комнаты (конференция имеет
/// приоритет над глобальным env); SFU не перекодирует, но обязан честно
/// объявить полосу, которую реально отдаёт браузеру.
/// Какие PT из `SdpAnswer.rtp_map` нужно переписать на egress SFU.
/// Функция ровно та, что вызывает WS-обработчик после успешного offer.
/// Offers without ICE/DTLS material are legacy clients (and integration
/// tests): they keep the old hand-built answer. A real offer that str0m
/// rejects is failed closed instead.
fn offer_supports_real_webrtc(sdp: &str) -> bool {
    sdp.contains("a=ice-ufrag:") && sdp.contains("a=ice-pwd:") && sdp.contains("a=fingerprint:")
}

fn answer_payload_types(maps: &[vks_proto::signaling::RtpMap]) -> Vec<(vks_core::MediaKind, u8)> {
    maps.iter()
        .filter_map(|map| {
            let kind = match map.codec.to_ascii_lowercase().as_str() {
                "opus" | "pcmu" | "pcma" | "g722" => vks_core::MediaKind::Audio,
                "vp8" | "h264" => vks_core::MediaKind::Video,
                _ => return None,
            };
            u8::try_from(map.payload).ok().map(|payload_type| (kind, payload_type))
        })
        .collect()
}

fn conference_video_bitrate(value: Option<&serde_json::Value>) -> Option<u32> {
    let value = value?;
    let payload = value.get("payload").unwrap_or(value);
    payload
        .get("bitrate")
        .and_then(serde_json::Value::as_u64)
        .map(|bitrate| u32::try_from(bitrate.clamp(64, 8192)).unwrap_or(2048))
}

fn room_video_bitrate(state: &Arc<AppState>, room: &vks_core::RoomId) -> Option<u32> {
    let settings = state.api_v3.conference_settings.lock().ok()?;
    conference_video_bitrate(settings.get(room))
}

/// Обработка сессии WebSocket-сигналинга.
///
/// Сайд-эффекты комнаты (создание записи, счётчик участников) применяются только
/// по **успешному** результату FSM (`JoinRoomResult`/`Ack` на `LeaveRoom`):
/// отклонённый JoinRoom (например, до ClientHello) ничего не создаёт и не считает.
async fn handle_ws_socket(socket: WebSocket, ctx: WsContext, verified: VerifiedJwt, role: Role) {
    let (mut sender, mut receiver) = socket.split();
    let handler_config = ctx.handler_config.clone();
    let mut handler = SignalingHandler::new(handler_config.clone());
    let state = ctx.state;
    let mut current_membership: Option<ActiveMembership> = None;
    // Set when the current SDP offer was answered by the real str0m peer; the
    // legacy plaintext PT/SSRC/endpoint side effects are skipped for it.
    let mut real_webrtc_peer;
    let mut real_webrtc_attempted = false;
    let mut room_rx: Option<tokio::sync::broadcast::Receiver<vks_admin::hub::RoomBroadcastEvent>> = None;
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(15));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_interval.tick().await;

    // Real-time token revocation subscription (M1.2)
    let mut revocation_rx = state.revocation_tx.subscribe();

    // Token expiration deadline
    let now_unix =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let time_left = if verified.exp > now_unix {
        std::time::Duration::from_secs((verified.exp - now_unix) as u64)
    } else {
        std::time::Duration::from_millis(1)
    };
    let token_deadline = tokio::time::sleep(time_left);
    tokio::pin!(token_deadline);

    // Скользящее окно частоты сообщений (T1.2).
    let mut window_start = std::time::Instant::now();
    let mut window_count: usize = 0;

    'session: loop {
        tokio::select! {
            () = &mut token_deadline => {
                warn!(user_id = %verified.user_id, "WS token expired during session; closing connection");
                let _ = sender.send(Message::Close(Some(CloseFrame { code: 4001, reason: "token expired".into() }))).await;
                break 'session;
            }
            rev_res = revocation_rx.recv() => {
                if let Ok(notice) = rev_res {
                    if (notice.user_id == verified.user_id || notice.user_id == "*")
                        && notice.generation >= verified.generation
                    {
                        warn!(user_id = %verified.user_id, "User session revoked via broadcast; closing WebSocket");
                        let _ = sender.send(Message::Close(Some(CloseFrame { code: 4003, reason: "session revoked".into() }))).await;
                        break 'session;
                    }
                }
            }
            _ = ping_interval.tick() => {
                if let Err(e) = sender.send(Message::Ping(Vec::new().into())).await {
                    warn!(error = %e, "failed to send server keepalive ping; closing");
                    break 'session;
                }
            }
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
                            if let Ok(env) = Envelope::decode(&event.payload) {
                                if let Some(Payload::ParticipantLeft(ref left)) = env.payload() {
                                    if left.participant_id == handler.participant_id().unwrap_or_default() && left.reason == 2 {
                                        warn!(participant_id = %left.participant_id, "Participant was kicked by operator; closing connection");
                                        let _ = sender.send(Message::Binary(event.payload.clone())).await;
                                        let close_frame = CloseFrame { code: 4003, reason: "kicked by operator".into() };
                                        let _ = sender.send(Message::Close(Some(close_frame))).await;
                                        break 'session;
                                    }
                                }
                            }

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
                        // Check if user account was deleted/deactivated during session
                        let is_deactivated = if let Ok(users) = state.users.lock() {
                            users.values().any(|u| (u.id.to_string() == verified.user_id || u.username == verified.user_id) && u.deleted)
                        } else {
                            false
                        };
                        if is_deactivated {
                            warn!(user_id = %verified.user_id, "User account deactivated during session; closing WS");
                            let _ = sender.send(Message::Close(Some(CloseFrame { code: 4003, reason: "account deactivated".into() }))).await;
                            break 'session;
                        }

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
                                let (join_name, join_display_name) = match env.payload() {
                                    Some(Payload::JoinRoom(join)) => (
                                        Some(join.room_id.clone()),
                                        if join.display_name.is_empty() {
                                            None
                                        } else {
                                            Some(join.display_name.clone())
                                        },
                                    ),
                                    _ => (None, None),
                                };
                                let was_leave = matches!(env.payload(), Some(Payload::LeaveRoom(_)));
                                let payload_opt = env.payload().clone();

                                // Check room scope for JoinRoom (M1.1)
                                if let Some(ref jname) = join_name {
                                    if let Some(ref target) = verified.room_id {
                                        if !jname.eq_ignore_ascii_case(target) {
                                            warn!(room_id = %jname, allowed = %target, "JoinRoom rejected: room_id does not match token scope");
                                            let err_reply = Envelope::error_reply(
                                                env.msg_id(),
                                                "forbidden",
                                                "Token is not authorized for this room",
                                                false,
                                            );
                                            let _ = sender.send(Message::Binary(err_reply.encode().into())).await;
                                            continue;
                                        }
                                    }
                                }

                                // SFU media negotiation: SDP Offer pre-validation (M1.1: Viewer cannot publish)
                                if let Some(Payload::SdpOffer(ref _o)) = payload_opt {
                                    if role == Role::Viewer {
                                        warn!(user_id = %verified.user_id, "Viewer role cannot publish media (SDP Offer rejected)");
                                        let err_reply = Envelope::error_reply(
                                            env.msg_id(),
                                            "forbidden",
                                            "Viewer role is not allowed to publish media",
                                            false,
                                        );
                                        let _ = sender.send(Message::Binary(err_reply.encode().into())).await;
                                        continue;
                                    }
                                }

                                // Живой профиль комнаты попадает в SFU-ответ до
                                // FSM: PATCH комнаты применяется к следующему offer.
                                if matches!(payload_opt, Some(Payload::SdpOffer(_))) {
                                    if let Some(room) = handler
                                        .room_id()
                                        .and_then(|value| uuid::Uuid::parse_str(value).ok())
                                        .map(vks_core::RoomId::from_uuid)
                                    {
                                        if let Some(bitrate) = room_video_bitrate(&state, &room) {
                                            handler.set_video_bitrate(bitrate);
                                        }
                                    }
                                }
                                // Запрос исполняет FSM; сайд-эффекты — только по успешному результату.
                                let request_msg_id = env.msg_id();
                                let mut webrtc_failure: Option<(&'static str, String)> = None;
                                let mut replies = handler.handle(env);

                                // Real WebRTC transport: replace the legacy hand-built
                                // answer before it is sent. If the transport is enabled
                                // but rejects the offer, fail closed with Payload::Error;
                                // only an explicit VKS_WEBRTC_TRANSPORT=0 may use legacy.
                                real_webrtc_peer = false;
                                if let (Some(Payload::SdpOffer(ref offer)), Some(ref membership)) =
                                    (&payload_opt, &current_membership)
                                {
                                    let _admission = crate::webrtc_runtime::admission();
                                    // Re-check the room shape under the admission
                                    // lock, not only before handler.handle.
                                    let gateway_room = state.room_hub.list_participants(&membership.room_id).iter().any(
                                        |participant| {
                                            participant.participant_id.contains("@sip-gateway")
                                                || participant.participant_id.contains("@h323-gateway")
                                        },
                                    );
                                    // Mode may have switched while we waited for the
                                    // admission lock; the decision must use the lock-time value.
                                    let effective_room_mode = state
                                        .mode
                                        .lock()
                                        .ok()
                                        .map(|mode| mode.effective_mode(&membership.room_id))
                                        .unwrap_or(vks_core::RoomMode::Sfu);
                                    if replies.iter().any(|r| matches!(r.payload(), Some(Payload::SdpAnswer(_)))) {
                                        let pid = parse_participant_id(&membership.participant_id);
                                        if !offer_supports_real_webrtc(&offer.sdp) {
                                            vks_common::metrics::WEBRTC_FALLBACK
                                                .with_label_values(&["legacy_offer"])
                                                .inc();
                                            if crate::webrtc_runtime::enabled() {
                                                // Enabled transport is authoritative: a
                                                // non-ICE offer would receive fake DTLS
                                                // credentials, so reject it instead.
                                                real_webrtc_attempted = true;
                                                webrtc_failure = Some((
                                                    "webrtc_offer_unsupported",
                                                    "WebRTC transport is enabled; SDP offer has no ICE/DTLS material"
                                                        .to_string(),
                                                ));
                                            }
                                        } else if effective_room_mode == vks_core::RoomMode::Mcu {
                                            real_webrtc_attempted = true;
                                            webrtc_failure = Some((
                                                "webrtc_mcu_room_unsupported",
                                                "Real WebRTC is not bridged into MCU-mode rooms".to_string(),
                                            ));
                                        } else if gateway_room {
                                            real_webrtc_attempted = true;
                                            warn!(
                                                room = %membership.room_id,
                                                participant = %pid,
                                                "WebRTC: real browser transport is not bridged to SIP/H.323 in this room"
                                            );
                                            webrtc_failure = Some((
                                                "webrtc_gateway_room_unsupported",
                                                "Real WebRTC in a room with SIP/H.323 participants is not supported yet".to_string(),
                                            ));
                                        } else {
                                            // Release the admission lock before the
                                            // async re-negotiation: holding a std
                                            // MutexGuard across `.await` is not Send.
                                            drop(_admission);
                                            real_webrtc_attempted = true;
                                            match crate::webrtc_runtime::accept_offer(
                                                &handler_config,
                                                membership.room_id,
                                                pid,
                                                &offer.sdp,
                                            )
                                            .await
                                            {
                                                Some(Ok(accepted)) => {
                                                    let rtp_maps = accepted.rtp_maps;
                                                    if let Some(answer) = replies.iter_mut().find_map(|reply| {
                                                        match reply.payload_mut() {
                                                            Some(Payload::SdpAnswer(answer)) => Some(answer),
                                                            _ => None,
                                                        }
                                                    }) {
                                                        answer.sdp = accepted.answer_sdp;
                                                        answer.rtp_map = rtp_maps
                                                            .into_iter()
                                                            .map(|(payload, codec, clock, channels)| vks_proto::signaling::RtpMap {
                                                                payload: u32::from(payload),
                                                                codec,
                                                                clock,
                                                                channels,
                                                            })
                                                            .collect();
                                                    }
                                                    real_webrtc_peer = true;
                                                    ctx.sfu_engine.remove_endpoint(membership.room_id, pid);
                                                    info!(
                                                        room = %membership.room_id,
                                                        participant = %pid,
                                                        "WebRTC: real str0m transport accepted"
                                                    );
                                                }
                                                Some(Err(error)) => {
                                                    warn!(
                                                        room = %membership.room_id,
                                                        participant = %pid,
                                                        %error,
                                                        "WebRTC: real transport rejected offer"
                                                    );
                                                    let code = match error {
                                                        vks_media_sfu::WebRtcError::RoomFull => "room_full",
                                                        vks_media_sfu::WebRtcError::CapacityExceeded => {
                                                            "webrtc_capacity"
                                                        }
                                                        _ => "webrtc_offer_rejected",
                                                    };
                                                    webrtc_failure = Some((
                                                        code,
                                                        format!("Real WebRTC transport rejected the offer: {error}"),
                                                    ));
                                                }
                                                None => {
                                                    if crate::webrtc_runtime::enabled() {
                                                        webrtc_failure = Some((
                                                            "webrtc_unavailable",
                                                            "WebRTC transport is enabled but failed to initialize".to_string(),
                                                        ));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                // The real transport is authoritative whenever it is
                                // enabled: never answer with fake ICE/DTLS credentials if
                                // str0m rejected the offer.
                                if let Some((code, message)) = webrtc_failure {
                                    // handler.handle already moved InRoom->Negotiating;
                                    // without rollback a retry on the same socket is
                                    // rejected as bad_state.
                                    handler.rollback_offer();
                                    replies.retain(|reply| !matches!(reply.payload(), Some(Payload::SdpAnswer(_))));
                                    let retryable = matches!(code, "room_full" | "webrtc_capacity");
                                    replies.push(Envelope::error_reply(request_msg_id, code, &message, retryable));
                                    vks_common::metrics::WEBRTC_FALLBACK
                                        .with_label_values(&["offer_rejected"])
                                        .inc();
                                }

                                let joined_ok = replies.iter().any(|r| matches!(r.payload(), Some(Payload::JoinRoomResult(_))));
                                let left_ok = was_leave && replies.iter().any(|r| matches!(r.payload(), Some(Payload::Ack(_))));

                                // Mutate SFU engine only AFTER handler.handle successfully accepts the offer
                                if let Some(Payload::SdpOffer(ref o)) = payload_opt {
                                    if replies.iter().any(|r| matches!(r.payload(), Some(Payload::SdpAnswer(_)))) {
                                        if let Ok(sdp) = vks_signaling::sdp::Sdp::parse(&o.sdp) {
                                            if let Some(ref m) = current_membership {
                                                let pid = parse_participant_id(&m.participant_id);
                                                if let Some(answer) = replies.iter().find_map(|reply| match reply.payload() {
                                                    Some(Payload::SdpAnswer(answer)) => Some(answer),
                                                    _ => None,
                                                }) {
                                                    if !real_webrtc_peer {
                                                        for (kind, payload_type) in answer_payload_types(&answer.rtp_map) {
                                                            ctx.sfu_engine.set_subscriber_payload_type(
                                                                m.room_id, pid, kind, payload_type,
                                                            );
                                                        }
                                                    }
                                                }
                                                let mut tracks = Vec::new();
                                                for media in &sdp.media {
                                                    let track_kind = match media.kind {
                                                        vks_signaling::sdp::MediaKind::Audio => vks_core::track::TrackKind::Audio,
                                                        _ => vks_core::track::TrackKind::Video,
                                                    };
                                                    let track = vks_core::ports::media_engine::MediaTrack::new(pid, track_kind);
                                                    if !real_webrtc_peer {
                                                        for ssrc_str in &media.ssrcs {
                                                            if let Ok(ssrc) = ssrc_str.split_whitespace().next().unwrap_or("").parse::<u32>() {
                                                                ctx.sfu_engine.register_ssrc(m.room_id, pid, track.id, ssrc);
                                                            }
                                                        }
                                                    }
                                                    tracks.push(track);
                                                }
                                                if !tracks.is_empty() {
                                                    // Публишер регистрируется в обоих движках при
                                                    // любом транспорте: реестр паблишеров — это
                                                    // состав участников и их треков, а не путь
                                                    // доставки RTP (реальным WebRTC занимается
                                                    // str0m). Раньше реальный пир в реестр не
                                                    // попадал вовсе — участника не было видно
                                                    // ни в списке треков, ни в раскладке.
                                                    let _ = ctx.sfu_engine.add_publisher(m.room_id, pid, tracks.clone()).await;
                                                    let _ = ctx.mcu_engine.add_publisher(m.room_id, pid, tracks.clone()).await;
                                                    if real_webrtc_peer {
                                                        // Реальный пир не подписан на композит
                                                        // (своё медиа он получает через fanout
                                                        // str0m), но обязан занять слот
                                                        // раскладки: иначе `set_participant_frame`
                                                        // выбросит его кадры.
                                                        ctx.mcu_engine.seat_publisher(m.room_id, pid, tracks.clone());
                                                    }
                                                    for track in &tracks {
                                                        let proto_kind = match track.media_kind {
                                                            vks_core::MediaKind::Audio => vks_proto::media::TrackKind::Audio,
                                                            vks_core::MediaKind::Video => vks_proto::media::TrackKind::Video,
                                                            vks_core::MediaKind::Screen => vks_proto::media::TrackKind::Screen,
                                                        };
                                                        let proto_codec = match track.media_kind {
                                                            vks_core::MediaKind::Audio => vks_proto::media::CodecId::Opus,
                                                            _ => vks_proto::media::CodecId::Vp8,
                                                        };
                                                        let added_msg = SignalingMessage {
                                                            msg_id: 0,
                                                            reply_to: 0,
                                                            payload: Some(Payload::TrackAdded(vks_proto::signaling::TrackAdded {
                                                                participant_id: m.participant_id.clone(),
                                                                track: Some(vks_proto::media::TrackInfo {
                                                                    track_id: track.id.to_string(),
                                                                    participant_id: m.participant_id.clone(),
                                                                    kind: proto_kind as i32,
                                                                    codec: proto_codec as i32,
                                                                    ssrc: 0,
                                                                    clock_rate: 90000,
                                                                    channels: 2,
                                                                }),
                                                            })),
                                                        };
                                                        let payload_bytes = Envelope::new(added_msg).encode();
                                                        state.room_hub.broadcast(&m.room_id, Some(&m.participant_id), payload_bytes.into());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // SFU ICE candidate exchange -> register participant UDP endpoint only if accepted
                                if let Some(Payload::IceCandidate(ref c)) = payload_opt {
                                    if !replies.iter().any(|r| matches!(r.payload(), Some(Payload::Error(_)))) {
                                        if let Some(ref m) = current_membership {
                                            let pid = parse_participant_id(&m.participant_id);
                                            if !real_webrtc_attempted && !crate::webrtc_runtime::has_peer(m.room_id, pid) {
                                                if let Some(addr) = parse_ice_candidate_addr(&c.candidate) {
                                                    ctx.sfu_engine.register_endpoint(m.room_id, pid, addr);
                                                }
                                            }
                                        }
                                    }
                                }

                                // SFU / MCU Subscribe / Unsubscribe handling (M2.5, M3.4)
                                if let Some(Payload::SubscribeRequest(ref s)) = payload_opt {
                                    if let Some(ref m) = current_membership {
                                        let pid = parse_participant_id(&m.participant_id);
                                        let track_ids: Vec<vks_core::TrackId> = s
                                            .source_track_ids
                                            .iter()
                                            .filter_map(|id_str| uuid::Uuid::parse_str(id_str).ok().map(vks_core::TrackId::from_uuid))
                                            .collect();
                                        let _ = ctx.sfu_engine.add_subscriber(m.room_id, pid, vks_core::ports::media_engine::SubscribeRequest {
                                            track_ids: track_ids.clone(),
                                            audio_only: false,
                                        }).await;
                                        if let Some(registry) = crate::webrtc_runtime::registry(&handler_config) {
                                            registry.set_subscription(m.room_id, pid, true, !s.audio_only);
                                            registry.set_max_bitrate(m.room_id, pid, u64::from(s.max_bitrate_bps));
                                        }
                                        let _ = ctx.mcu_engine.add_subscriber(m.room_id, pid, vks_core::ports::media_engine::SubscribeRequest {
                                            track_ids,
                                            audio_only: false,
                                        }).await;
                                    }
                                }

                                if let Some(Payload::Unsubscribe(ref _u)) = payload_opt {
                                    if let Some(ref m) = current_membership {
                                        let pid = parse_participant_id(&m.participant_id);
                                        let _ = ctx.sfu_engine.remove_subscriber(m.room_id, pid).await;
                                        let _ = ctx.mcu_engine.remove_subscriber(m.room_id, pid).await;
                                        if let Some(registry) = crate::webrtc_runtime::registry(&handler_config) {
                                            registry.set_subscription(m.room_id, pid, false, false);
                                        }
                                    }
                                }

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
                                        // Если клиент уже находился в другой комнате на этом сокете, вычищаем её
                                        if current_membership.is_some() {
                                            cleanup_active_membership(&mut current_membership, &ctx.sfu_engine, &ctx.mcu_engine, &state, 0).await;
                                        }

                                        let pid_str = handler.participant_id().map(str::to_string).unwrap_or_default();
                                        current_membership = Some(ActiveMembership {
                                            room_id: rid,
                                            participant_id: pid_str.clone(),
                                        });

                                        let pid = parse_participant_id(&pid_str);
                                        let _ = ctx.sfu_engine.create_room(vks_core::ports::media_engine::RoomMediaCfg {
                                            room_id: rid,
                                            mode: vks_core::RoomMode::Sfu,
                                            caps: vks_core::ports::media_engine::RoomCaps::default(),
                                        }).await;
                                        let _ = ctx.mcu_engine.create_room(vks_core::ports::media_engine::RoomMediaCfg {
                                            room_id: rid,
                                            mode: vks_core::RoomMode::Mcu,
                                            caps: vks_core::ports::media_engine::RoomCaps::default(),
                                        }).await;
                                        let _ = ctx.sfu_engine.add_subscriber(rid, pid, vks_core::ports::media_engine::SubscribeRequest {
                                            track_ids: Vec::new(),
                                            audio_only: false,
                                        }).await;
                                        let _ = ctx.mcu_engine.add_subscriber(rid, pid, vks_core::ports::media_engine::SubscribeRequest {
                                            track_ids: Vec::new(),
                                            audio_only: false,
                                        }).await;

                                        // RTCP PLI: запрашиваем ключевые кадры у видео-паблишеров для устранения чёрного экрана
                                        if let Some(udp) = &ctx.udp_loop {
                                            let plis = ctx.sfu_engine.generate_pli_for_room_publishers(rid);
                                            for (dest, pli_bytes) in plis {
                                                let _ = udp.send_raw(dest, &pli_bytes).await;
                                            }
                                        }

                                        let now_secs = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs();
                                        let display_name = join_display_name.unwrap_or_else(|| pid_str.clone());
                                        let p_snapshot = vks_admin::hub::ParticipantSnapshot {
                                            participant_id: pid_str.clone(),
                                            display_name: display_name.clone(),
                                            is_recorder: false,
                                            is_speaking: false,
                                            track_ids: Vec::new(),
                                            is_audio_muted: false,
                                            is_video_muted: false,
                                            connected_at: now_secs,
                                        };
                                        let (rx, snapshot) = state.room_hub.register_participant(&rid, p_snapshot);
                                        room_rx = Some(rx);

                                        // Обогащаем JoinRoomResult полным снимком участников
                                        for reply in &mut replies {
                                            if let Some(Payload::JoinRoomResult(ref mut res)) = reply.payload_mut() {
                                                res.participants = snapshot
                                                    .iter()
                                                    .map(|p| vks_proto::signaling::ParticipantInfo {
                                                        participant_id: p.participant_id.clone(),
                                                        display_name: p.display_name.clone(),
                                                        is_recorder: p.is_recorder,
                                                        is_speaking: p.is_speaking,
                                                        track_ids: p.track_ids.clone(),
                                                    })
                                                    .collect();
                                                // S9: снимок раскладки — в самом ответе на
                                                // JoinRoom. Отдельный push после него создавал
                                                // гонку (push мог прийти позже ответа), и
                                                // клиент успевал нарисовать авто-сетку.
                                                res.layout = ctx
                                                    .mcu_engine
                                                    .layout_snapshot(rid)
                                                    .as_ref()
                                                    .map(|snapshot| {
                                                        crate::layout_signal::proto_layout(
                                                            &snapshot.layout,
                                                            &snapshot.preset,
                                                            &snapshot.mode,
                                                        )
                                                    });
                                            }
                                        }

                                        let count = snapshot.len();
                                        vks_admin::rooms::Rooms::update(&state, &rid, None, None, Some(count), None);

                                        // Late joiners: немедленно отправляем актуальный режим комнаты, если он отличается от SFU или комната переключалась
                                        let (effective_mode, switch_epoch) = if let Ok(m) = state.mode.lock() {
                                            (m.effective_mode(&rid), m.switch_epoch)
                                        } else {
                                            (vks_core::RoomMode::Sfu, 0)
                                        };
                                        if effective_mode != vks_core::RoomMode::Sfu || switch_epoch > 0 {
                                            let proto_mode = match effective_mode {
                                                vks_core::RoomMode::Sfu => vks_proto::media::RoomMode::Sfu,
                                                vks_core::RoomMode::Mcu => vks_proto::media::RoomMode::Mcu,
                                            };
                                            let mode_msg = SignalingMessage {
                                                msg_id: 0,
                                                reply_to: 0,
                                                payload: Some(Payload::RoomModeChanged(vks_proto::signaling::RoomModeChanged {
                                                    new_mode: proto_mode as i32,
                                                    caps: None,
                                                    switch_epoch,
                                                    graceful: true,
                                                })),
                                            };
                                            replies.push(Envelope::new(mode_msg));
                                        }

                                        // Рассылаем ParticipantJoined другим участникам комнаты
                                        if !pid_str.is_empty() {
                                            let joined_msg = SignalingMessage {
                                                msg_id: 0,
                                                reply_to: 0,
                                                payload: Some(Payload::ParticipantJoined(ParticipantJoined {
                                                    participant_id: pid_str.clone(),
                                                    display_name: display_name.clone(),
                                                    is_recorder: false,
                                                })),
                                            };
                                            let payload_bytes = Envelope::new(joined_msg).encode();
                                            state.room_hub.broadcast(&rid, Some(&pid_str), payload_bytes.into());
                                        }

                                        // Уведомляем админский WebSocket (/api/v3/ws)
                                        state.notify_admin("conferences:update", serde_json::json!({
                                            "id": rid.to_string(),
                                            "participants_count": count
                                        }));
                                    }
                                } else if left_ok {
                                    room_rx = None;
                                    cleanup_active_membership(&mut current_membership, &ctx.sfu_engine, &ctx.mcu_engine, &state, 0).await;
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

    drop(room_rx);
    cleanup_active_membership(&mut current_membership, &ctx.sfu_engine, &ctx.mcu_engine, &state, 1).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    #[test]
    fn room_bitrate_reaches_sfu_answer_without_overriding_env_when_absent() {
        use vks_signaling::handler::{HandlerConfig, SignalingHandler};
        let state = Arc::new(AppState::new());
        let room = vks_core::RoomId::new();
        let mut handler = SignalingHandler::new(HandlerConfig::default_test().with_video_bitrate(4096));
        assert!(room_video_bitrate(&state, &room).is_none());
        // Нет настройки комнаты — env/конструктор не затирается.
        if let Some(bitrate) = room_video_bitrate(&state, &room) {
            handler.set_video_bitrate(bitrate);
        }
        assert_eq!(handler.config().answer_profile().b_as, Some(4096));

        state
            .api_v3
            .conference_settings
            .lock()
            .unwrap()
            .insert(room, serde_json::json!({"payload": {"bitrate": 1024}}));
        if let Some(bitrate) = room_video_bitrate(&state, &room) {
            handler.set_video_bitrate(bitrate);
        }
        assert_eq!(handler.config().answer_profile().b_as, Some(1024));
    }

    #[test]
    fn answer_payload_types_maps_offer_pt_to_sfu_kinds() {
        use vks_proto::signaling::RtpMap;
        let maps = vec![
            RtpMap { payload: 120, codec: "opus".into(), clock: 48000, channels: 2 },
            RtpMap { payload: 100, codec: "VP8".into(), clock: 90000, channels: 0 },
            RtpMap { payload: 102, codec: "h264".into(), clock: 90000, channels: 0 },
            RtpMap { payload: 300, codec: "rtx".into(), clock: 90000, channels: 0 },
        ];
        assert_eq!(
            answer_payload_types(&maps),
            vec![
                (vks_core::MediaKind::Audio, 120),
                (vks_core::MediaKind::Video, 100),
                (vks_core::MediaKind::Video, 102),
            ]
        );
    }

    #[test]
    fn conference_bitrate_reads_payload_and_clamps() {
        use serde_json::json;
        assert_eq!(conference_video_bitrate(None), None);
        assert_eq!(conference_video_bitrate(Some(&json!({}))), None);
        assert_eq!(conference_video_bitrate(Some(&json!({"bitrate": 4096}))), Some(4096));
        assert_eq!(conference_video_bitrate(Some(&json!({"payload": {"bitrate": 512}}))), Some(512));
        assert_eq!(conference_video_bitrate(Some(&json!({"payload": {"bitrate": 0}}))), Some(64));
        assert_eq!(conference_video_bitrate(Some(&json!({"payload": {"bitrate": 99999}}))), Some(8192));
    }

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

    #[test]
    fn test_parse_participant_id_deterministic() {
        let alias = "alice-web-client";
        let pid1 = parse_participant_id(alias);
        let pid2 = parse_participant_id(alias);
        assert_eq!(pid1, pid2);

        let random_uuid = uuid::Uuid::new_v4();
        let pid_uuid = parse_participant_id(&random_uuid.to_string());
        assert_eq!(pid_uuid.to_string(), random_uuid.to_string());

        let suffixed = format!("prefix-{}", random_uuid);
        let pid_suffixed = parse_participant_id(&suffixed);
        assert_eq!(pid_suffixed.to_string(), random_uuid.to_string());

        let infixed = format!("prefix-{}-audio-track", random_uuid);
        let pid_infixed = parse_participant_id(&infixed);
        assert_eq!(pid_infixed.to_string(), random_uuid.to_string());
    }

    #[test]
    fn metrics_tokens_extracts_either_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().expect("header value"));
        assert_eq!(metrics_tokens(&headers), vec!["secret".to_string()]);
        headers.insert("x-metrics-token", " second ".parse().expect("header value"));
        assert_eq!(metrics_tokens(&headers), vec!["secret".to_string(), "second".to_string()]);
        headers.insert(header::AUTHORIZATION, "Bearer   ".parse().expect("header value"));
        assert_eq!(metrics_tokens(&headers), vec!["second".to_string()], "empty/blank token dropped");
        // Basic-схема не принимается как Bearer-токен.
        headers.insert(header::AUTHORIZATION, "Basic dXNlcjpwYXNz".parse().expect("header value"));
        assert_eq!(metrics_tokens(&headers), vec!["second".to_string()]);
    }

    #[test]
    fn metrics_access_enforces_token_and_loopback_rules() {
        // Токен не настроен: loopback можно, LAN нельзя.
        assert!(metrics_access(None, &[], true).is_ok());
        assert_eq!(metrics_access(None, &[], false).unwrap_err().0, StatusCode::FORBIDDEN);
        // Токен настроен: он обязателен и для loopback; неверный — 401.
        assert!(metrics_access(Some("secret"), &["secret".into()], true).is_ok());
        assert!(metrics_access(Some("secret"), &["secret".into()], false).is_ok());
        // Любой из двух заголовков достаточен: неверный Bearer не глушит X-Metrics-Token.
        assert!(metrics_access(Some("secret"), &["wrong".into(), "secret".into()], true).is_ok());
        assert_eq!(metrics_access(Some("secret"), &[], true).unwrap_err().0, StatusCode::UNAUTHORIZED);
        assert_eq!(metrics_access(Some("secret"), &["nope".into()], true).unwrap_err().0, StatusCode::UNAUTHORIZED);
    }
}
