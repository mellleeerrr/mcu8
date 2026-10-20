//! WebSocket `/api/v3/ws` (§4.4 плана): JSON-протокол legacy UI.
//!
//! Сокет открывается без токена; **токен проверяется на каждом сообщении**
//! (`access_token` в конверте транзакции). Невалидный токен → `error.code=401`
//! (UI эмитит `logout`), при этом соединение **не** рвётся: клиент реконнектится
//! с задержкой 1s × 1.5ⁿ, и разрыв на тех.ошибке превратился бы в шторм реконнектов.
//!
//! Форматы (таблица §4.4):
//! - C→S: `{"method": "<m>", "payload": {…}, "id": "<uuid>", "access_token": "<jwt>"}`;
//! - S→C ответ: `{"id": "<uuid>", "payload": {…}}` | `{"id": …, "error": {"code", "description"}}`;
//! - S→C событие: `{"event": "<name>", "payload": {…}}` (без `id`).
//!
//! Лимиты (BACKEND_RULES §2.4/§2.5): кадр ≤ 16 KB (больше — `error` без разрыва);
//! keepalive ping 30 s; ≤ 100 новых соединений/с per-IP (отказ — HTTP 429 в конверте).
//! Токены и payload'ы в логи не пишутся (§6.3) — только `method` и метрики.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};

use super::auth::{verify_access_token, ClientIp};
use super::{error_json, track};
use crate::state::SharedState;

/// Максимальный размер кадра/сообщения (64 KB, M1.8).
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// Период keepalive-ping (30 s, §4.4).
pub const PING_INTERVAL_SECS: u64 = 30;
/// Лимит новых WS-соединений в секунду на IP (§4.4).
pub const WS_MAX_CONNECTIONS_PER_SEC: u32 = 100;
/// Лимит входящих сообщений в секунду на соединение (M1.8).
pub const WS_MAX_MESSAGES_PER_SEC: u32 = 100;

/// Rate-limit новых WS-соединений: ≤ [`WS_MAX_CONNECTIONS_PER_SEC`]/s per-IP,
/// фиксированное окно в секунду (in-memory, Mutex — single-node).
#[derive(Debug, Default)]
pub struct WsRateLimit {
    inner: Mutex<RateWindow>,
}

#[derive(Debug, Default)]
struct RateWindow {
    /// Начало текущего окна (unix-секунды).
    second: i64,
    /// ip → число соединений в окне.
    counts: HashMap<String, u32>,
}

impl WsRateLimit {
    /// Создаёт пустой лимитер.
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрирует попытку соединения с `ip`; `false` — лимит превышен.
    pub fn allow(&self, ip: &str, now_ms: i64) -> bool {
        let second = now_ms.div_euclid(1000);
        let mut window = self.inner.lock().expect("ws ratelimit poisoned");
        if window.second != second {
            window.second = second;
            window.counts.clear();
        }
        let count = window.counts.entry(ip.to_string()).or_insert(0);
        *count += 1;
        *count <= WS_MAX_CONNECTIONS_PER_SEC
    }
}

/// `GET /api/v3/ws` → upgrade + цикл сессии.
///
/// Rate-limit проверяется **до** upgrade: отказ — HTTP 429 с обычным конвертом
/// ошибки (`{"error":{"code":429,…}}`); UI воспринимает это как неудачный коннект
/// и повторит с backoff, не считая его протокольным сообщением.
pub async fn ws_handler(
    State(state): State<SharedState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    ws: WebSocketUpgrade,
) -> Response {
    track("ws");
    if !state.api_v3.ws_ratelimit.allow(&ip, now_unix_ms()) {
        tracing::warn!(client_ip = %ip, "api_v3 ws: connection rate limit exceeded");
        return error_json(429, "Too many connections");
    }
    ws.max_message_size(MAX_FRAME_BYTES * 4).on_upgrade(move |socket| handle_socket(state, ip, socket))
}

/// Сопоставляет подписку клиента с темой события (точное совпадение, wildcard `*` или разделитель `:`).
#[inline]
fn topic_matches(sub: &str, topic: &str) -> bool {
    sub == topic
        || (sub.ends_with('*') && topic.starts_with(sub.trim_end_matches('*')))
        || topic.strip_prefix(sub).is_some_and(|rest| rest.starts_with(':'))
}

/// Одна WS-сессия: чтение транзакций, keepalive-ping, ответы/события.
async fn handle_socket(state: SharedState, client_ip: String, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut subscriptions: HashSet<String> = HashSet::new();
    let mut admin_rx = state.admin_events.subscribe();
    let mut revocation_rx = state.revocation_tx.subscribe();
    let mut active_user: Option<String> = None;
    let mut active_gen: u64 = 0;
    let mut active_exp: i64 = i64::MAX;
    let mut ping = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Первый tick interval срабатывает сразу — пропускаем, чтобы не пинговать на старте.
    ping.tick().await;

    let exp_timer = tokio::time::sleep(Duration::from_secs(86400 * 365));
    tokio::pin!(exp_timer);

    let mut msg_window_sec = 0i64;
    let mut msg_count = 0u32;

    loop {
        tokio::select! {
            () = &mut exp_timer, if active_user.is_some() => {
                tracing::warn!(client_ip, "api_v3 ws: active token expired; closing connection");
                let _ = sender.send(Message::Text(
                    json!({ "error": { "code": 401, "description": "Token expired" } }).to_string().into()
                )).await;
                break;
            }
            _ = ping.tick() => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            rev_notice = revocation_rx.recv() => {
                if let Ok(notice) = rev_notice {
                    if let Some(user_id) = &active_user {
                        if (notice.user_id == *user_id || notice.user_id == "*") && notice.generation >= active_gen {
                            tracing::warn!(user_id = %user_id, "api_v3 ws: active session revoked in real-time");
                            let _ = sender.send(Message::Text(
                                json!({ "error": { "code": 401, "description": "Session revoked" } }).to_string().into()
                            )).await;
                            break;
                        }
                    }
                }
            }
            admin_msg = admin_rx.recv() => {
                match admin_msg {
                    Ok(evt) => {
                        let matched = subscriptions.iter().any(|sub| topic_matches(sub, &evt.topic));
                        if matched {
                            let frame = json!({ "event": evt.topic, "payload": evt.payload });
                            if sender.send(Message::Text(frame.to_string().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "slow admin ws subscriber dropped events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            incoming = receiver.next() => {
                let Some(Ok(message)) = incoming else { break };
                let now_sec = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if active_user.is_some() && now_sec >= active_exp {
                    tracing::warn!(client_ip, "api_v3 ws: active token expired");
                    let _ = sender.send(Message::Text(
                        json!({ "error": { "code": 401, "description": "Token expired" } }).to_string().into()
                    )).await;
                    break;
                }
                if msg_window_sec != now_sec {
                    msg_window_sec = now_sec;
                    msg_count = 0;
                }
                msg_count += 1;
                if msg_count > WS_MAX_MESSAGES_PER_SEC {
                    tracing::warn!(client_ip, msg_count, "api_v3 ws: message rate limit exceeded");
                    let _ = sender
                        .send(Message::Text(
                            json!({
                                "error": { "code": 429, "description": "Message rate limit exceeded" }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await;
                    break;
                }

                let (outcomes, verified_info) = match message {
                    Message::Text(text) => process_frame(&state, &client_ip, text.as_str(), &mut subscriptions).await,
                    Message::Binary(bytes) => match std::str::from_utf8(&bytes) {
                        Ok(text) => process_frame(&state, &client_ip, text, &mut subscriptions).await,
                        Err(_) => (vec![WsOutcome::json(json!({
                            "error": { "code": 400, "description": "Malformed message" }
                        }))], None),
                    },
                    Message::Ping(payload) => {
                        if sender.send(Message::Pong(payload)).await.is_err() { break }
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => break,
                };
                if let Some((user_id, gen, exp)) = verified_info {
                    active_user = Some(user_id.clone());
                    active_gen = gen;
                    active_exp = exp;
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
                    let left = if exp > now { (exp - now) as u64 } else { 0 };
                    exp_timer.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(left));

                    let is_deactivated = if let Ok(users) = state.users.lock() {
                        users.values().any(|u| (u.id.to_string() == user_id || u.username == user_id) && u.deleted)
                    } else {
                        false
                    };
                    if is_deactivated {
                        tracing::warn!(user_id = %user_id, "api_v3 ws: account deactivated; closing WS");
                        let _ = sender.send(Message::Text(
                            json!({ "error": { "code": 403, "description": "User account is deactivated" } }).to_string().into()
                        )).await;
                        break;
                    }
                }
                let mut send_failed = false;
                for outcome in outcomes {
                    if sender.send(Message::Text(outcome.0.into())).await.is_err() {
                        send_failed = true;
                        break;
                    }
                }
                if send_failed { break }
            }
        }
    }
    // Чистое закрытие (тест рестарт-совместимости §4.4: клиент видит нормальный close).
    let _ = sender.close().await;
}

/// Результат обработки одного кадра: сериализованное исходящее сообщение.
struct WsOutcome(String);

impl WsOutcome {
    fn json(value: Value) -> Self {
        Self(value.to_string())
    }
}

/// Обрабатывает один текстовый кадр; возвращает 0..n исходящих сообщений
/// (ответ + возможные немедленные события подписки) и информацию об аутентифицированном токене.
async fn process_frame(
    state: &SharedState,
    client_ip: &str,
    raw: &str,
    subscriptions: &mut HashSet<String>,
) -> (Vec<WsOutcome>, Option<(String, u64, i64)>) {
    if raw.len() > MAX_FRAME_BYTES {
        tracing::warn!(client_ip, frame_bytes = raw.len(), "api_v3 ws: frame too large");
        return (
            vec![WsOutcome::json(json!({
                "error": { "code": 413, "description": "Frame too large" }
            }))],
            None,
        );
    }

    let Ok(message) = serde_json::from_str::<Value>(raw) else {
        return (
            vec![WsOutcome::json(json!({
                "error": { "code": 400, "description": "Malformed message" }
            }))],
            None,
        );
    };

    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return (
            vec![WsOutcome::json(json!({
                "id": id, "error": { "code": 400, "description": "Missing method" }
            }))],
            None,
        );
    };
    let payload = message.get("payload").cloned().unwrap_or_else(|| json!({}));

    let Some(token) = message.get("access_token").and_then(Value::as_str) else {
        return (
            vec![WsOutcome::json(json!({
                "id": id, "error": { "code": 401, "description": "Unauthorized" }
            }))],
            None,
        );
    };
    let verified = match verify_access_token(state, token) {
        Some(v) => v,
        None => {
            tracing::warn!(client_ip, method, "api_v3 ws: rejected message (invalid token)");
            return (
                vec![WsOutcome::json(json!({
                    "id": id, "error": { "code": 401, "description": "Unauthorized" }
                }))],
                None,
            );
        }
    };

    // Check account status in state.users
    if let Ok(users) = state.users.lock() {
        if let Some(u) = users.values().find(|u| u.id.to_string() == verified.user_id || u.username == verified.user_id)
        {
            if u.deleted {
                tracing::warn!(client_ip, user_id = %verified.user_id, "api_v3 ws: user account deactivated");
                return (
                    vec![WsOutcome::json(json!({
                        "id": id, "error": { "code": 403, "description": "Account is deactivated" }
                    }))],
                    None,
                );
            }
        }
    }

    let outcomes = match method {
        "subscribe" => {
            if let Some(events) = payload.get("events").and_then(Value::as_object) {
                for event in events.keys() {
                    subscriptions.insert(event.clone());
                }
            }
            let mut outcomes = vec![ok_reply(&id, &json!({}))];
            // Немедленное событие подписки (§4.4): состояние диска известно сразу.
            if subscriptions.contains("system:status:disk") {
                outcomes.push(WsOutcome::json(json!({
                    "event": "system:status:disk",
                    "payload": { "full": "NORMAL" }
                })));
            }
            outcomes
        }
        "unsubscribe" => {
            if let Some(events) = payload.get("events").and_then(Value::as_object) {
                for event in events.keys() {
                    subscriptions.remove(event);
                }
            }
            vec![ok_reply(&id, &json!({}))]
        }
        "ui:users:search" => {
            match super::directory::ws_users_search(state, &verified.user_id, verified.room_id.as_deref(), &payload)
                .await
            {
                Ok(result) => vec![ok_reply(&id, &result)],
                Err((code, description)) => {
                    vec![WsOutcome::json(json!({"id": id, "error": {"code": code, "description": description}}))]
                }
            }
        }
        "ui:call_strings:search" => {
            match super::call_strings::ws_search(state, &verified.user_id, verified.room_id.as_deref(), &payload).await
            {
                Ok(result) => vec![ok_reply(&id, &result)],
                Err((code, description)) => {
                    vec![WsOutcome::json(json!({"id": id, "error": {"code": code, "description": description}}))]
                }
            }
        }
        "ui:files:search" => vec![ok_reply(&id, &json!({ "files": [], "count": 0 }))],
        "terminals:configurations:install" | "terminals:firmwares:install" => {
            vec![WsOutcome::json(json!({
                "id": id, "error": { "code": 501, "description": "Not implemented" }
            }))]
        }
        _ => vec![WsOutcome::json(json!({
            "id": id, "error": { "code": 404, "description": "Unknown method" }
        }))],
    };
    (outcomes, Some((verified.user_id, verified.generation, verified.exp)))
}

/// Обёртка ответа: `{"id": …, "payload": {…}}`.
fn ok_reply(id: &Value, payload: &Value) -> WsOutcome {
    WsOutcome::json(json!({ "id": id, "payload": payload }))
}

fn now_unix_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_allows_100_per_second_then_denies() {
        let rl = WsRateLimit::new();
        let now = 1_700_000_000_000;
        for i in 0..WS_MAX_CONNECTIONS_PER_SEC {
            assert!(rl.allow("10.0.0.1", now + i as i64), "connection {i} must be allowed");
        }
        assert!(!rl.allow("10.0.0.1", now + 100), "101st connection must be denied");
        // Другой IP — свой счётчик.
        assert!(rl.allow("10.0.0.2", now + 101));
    }

    #[test]
    fn rate_limit_resets_each_second() {
        let rl = WsRateLimit::new();
        let now = 1_700_000_000_000;
        for i in 0..WS_MAX_CONNECTIONS_PER_SEC {
            assert!(rl.allow("10.0.0.1", now + i as i64));
        }
        assert!(!rl.allow("10.0.0.1", now + 500));
        // Новое окно — снова можно.
        assert!(rl.allow("10.0.0.1", now + 1000));
    }

    #[tokio::test]
    async fn process_frame_rejects_oversized_and_malformed() {
        let state = crate::state::AppState::new();
        let state: SharedState = std::sync::Arc::new(state);
        let mut subs = HashSet::new();

        let huge = "x".repeat(MAX_FRAME_BYTES + 1);
        let (out, _) = process_frame(&state, "10.0.0.1", &huge, &mut subs).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].0.contains("\"code\":413"), "{}", out[0].0);

        let (out, _) = process_frame(&state, "10.0.0.1", "not-json", &mut subs).await;
        assert!(out[0].0.contains("\"code\":400"), "{}", out[0].0);
    }

    #[tokio::test]
    async fn process_frame_requires_token_even_for_unknown_methods() {
        let state: SharedState = std::sync::Arc::new(crate::state::AppState::new());
        let mut subs = HashSet::new();
        let frame = r#"{"method":"nope","payload":{},"id":"1"}"#;
        let (out, _) = process_frame(&state, "10.0.0.1", frame, &mut subs).await;
        assert!(out[0].0.contains("\"code\":401"), "{}", out[0].0);
    }

    #[test]
    fn topic_matching_rules() {
        assert!(topic_matches("conferences", "conferences:update"));
        assert!(topic_matches("conferences", "conferences"));
        assert!(topic_matches("conferences:*", "conferences:update"));
        assert!(!topic_matches("conferences", "conference:update"));
        assert!(!topic_matches("conference", "conferences:update"));
    }
}
