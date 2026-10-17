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

/// Максимальный размер кадра/сообщения (16 KB, §4.4).
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
/// Период keepalive-ping (30 s, §4.4).
pub const PING_INTERVAL_SECS: u64 = 30;
/// Лимит новых WS-соединений в секунду на IP (§4.4).
pub const WS_MAX_CONNECTIONS_PER_SEC: u32 = 100;

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
    ws.on_upgrade(move |socket| handle_socket(state, ip, socket))
}

/// Одна WS-сессия: чтение транзакций, keepalive-ping, ответы/события.
async fn handle_socket(state: SharedState, client_ip: String, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut subscriptions: HashSet<String> = HashSet::new();
    let mut ping = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Первый tick interval срабатывает сразу — пропускаем, чтобы не пинговать на старте.
    ping.tick().await;

    loop {
        tokio::select! {
            _ = ping.tick() => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            incoming = receiver.next() => {
                let Some(Ok(message)) = incoming else { break };
                let outcomes = match message {
                    Message::Text(text) => process_frame(&state, &client_ip, text.as_str(), &mut subscriptions),
                    Message::Binary(bytes) => match std::str::from_utf8(&bytes) {
                        Ok(text) => process_frame(&state, &client_ip, text, &mut subscriptions),
                        Err(_) => vec![WsOutcome::json(json!({
                            "error": { "code": 400, "description": "Malformed message" }
                        }))],
                    },
                    Message::Ping(payload) => {
                        if sender.send(Message::Pong(payload)).await.is_err() { break }
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => break,
                };
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
/// (ответ + возможные немедленные события подписки).
///
/// Тех.ошибки (нечитаемый JSON, неизвестный метод, слишком большой кадр) —
/// коды ≠ 401, соединение сохраняется (§4.4).
fn process_frame(
    state: &SharedState,
    client_ip: &str,
    raw: &str,
    subscriptions: &mut HashSet<String>,
) -> Vec<WsOutcome> {
    if raw.len() > MAX_FRAME_BYTES {
        tracing::warn!(client_ip, frame_bytes = raw.len(), "api_v3 ws: frame too large");
        return vec![WsOutcome::json(json!({
            "error": { "code": 413, "description": "Frame too large" }
        }))];
    }

    let Ok(message) = serde_json::from_str::<Value>(raw) else {
        return vec![WsOutcome::json(json!({
            "error": { "code": 400, "description": "Malformed message" }
        }))];
    };

    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return vec![WsOutcome::json(json!({
            "id": id, "error": { "code": 400, "description": "Missing method" }
        }))];
    };
    let payload = message.get("payload").cloned().unwrap_or_else(|| json!({}));

    let Some(token) = message.get("access_token").and_then(Value::as_str) else {
        return vec![WsOutcome::json(json!({
            "id": id, "error": { "code": 401, "description": "Unauthorized" }
        }))];
    };
    if verify_access_token(state, token).is_none() {
        // 401 → UI эмитит logout; соединение не рвём (§4.4).
        tracing::warn!(client_ip, method, "api_v3 ws: rejected message (invalid token)");
        return vec![WsOutcome::json(json!({
            "id": id, "error": { "code": 401, "description": "Unauthorized" }
        }))];
    }

    match method {
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
        "ui:users:search" => vec![ok_reply(&id, &users_search(state, &payload))],
        "ui:call_strings:search" => vec![ok_reply(&id, &json!({ "call_strings": [], "count": 0 }))],
        "ui:files:search" => vec![ok_reply(&id, &json!({ "files": [], "count": 0 }))],
        "terminals:configurations:install" | "terminals:firmwares:install" => {
            vec![WsOutcome::json(json!({
                "id": id, "error": { "code": 501, "description": "Not implemented" }
            }))]
        }
        _ => vec![WsOutcome::json(json!({
            "id": id, "error": { "code": 404, "description": "Unknown method" }
        }))],
    }
}

/// Обёртка ответа: `{"id": …, "payload": {…}}`.
fn ok_reply(id: &Value, payload: &Value) -> WsOutcome {
    WsOutcome::json(json!({ "id": id, "payload": payload }))
}

/// `ui:users:search` → `{users:[…], count}` (маппер `QT`: `id`, `display_name`, …).
///
/// Источник — admin-хранилище пользователей; `search`/`query` фильтруют по имени
/// (подстрока, case-insensitive). Незнакомые фильтры мягко игнорируются.
fn users_search(state: &SharedState, payload: &Value) -> Value {
    let needle =
        payload.get("search").or_else(|| payload.get("query")).and_then(Value::as_str).unwrap_or("").to_lowercase();
    let users = state.users.lock().expect("users store poisoned");
    let matched: Vec<Value> = users
        .values()
        .filter(|u| needle.is_empty() || u.username.to_lowercase().contains(&needle))
        .map(|u| {
            json!({
                "id": u.id,
                "display_name": u.username,
                "busy": false,
                "deleted": u.deleted
            })
        })
        .collect();
    json!({ "users": matched, "count": matched.len() })
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

    #[test]
    fn process_frame_rejects_oversized_and_malformed() {
        let state = crate::state::AppState::new();
        let state: SharedState = std::sync::Arc::new(state);
        let mut subs = HashSet::new();

        let huge = "x".repeat(MAX_FRAME_BYTES + 1);
        let out = process_frame(&state, "10.0.0.1", &huge, &mut subs);
        assert_eq!(out.len(), 1);
        assert!(out[0].0.contains("\"code\":413"), "{}", out[0].0);

        let out = process_frame(&state, "10.0.0.1", "not-json", &mut subs);
        assert!(out[0].0.contains("\"code\":400"), "{}", out[0].0);
    }

    #[test]
    fn process_frame_requires_token_even_for_unknown_methods() {
        let state: SharedState = std::sync::Arc::new(crate::state::AppState::new());
        let mut subs = HashSet::new();
        let frame = r#"{"method":"nope","payload":{},"id":"1"}"#;
        let out = process_frame(&state, "10.0.0.1", frame, &mut subs);
        assert!(out[0].0.contains("\"code\":401"), "{}", out[0].0);
    }
}
