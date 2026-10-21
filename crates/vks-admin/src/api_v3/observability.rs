//! T2.4: наблюдаемость и сервисные данные PNM.
//!
//! Read-only эндпоинты, которые рисуют страницы «Статистика», «Журнал событий»,
//! «Сетевые интерфейсы», «Терминалы», «Медиафайлы» vendor-UI 2.2.1.66:
//! `log_events` (+export), `system/network/interfaces`, `system/utils/ping|
//! traceroute|tcpdumps`, `terminals`, `videorecords`.
//!
//! Всё, что меняет состояние хоста/сети (`PATCH` интерфейса, запуск/остановка
//! tcpdump, установка прошивок терминалов, удаление записей), — явный отказ
//! `501 Not implemented` с описанием, а не молчаливый успех: UI показывает
//! ошибку, пользователь не думает, что настройка применилась.
//! Реальные данные берутся из аудита, `ip -j`, `/proc`-записей и store записей.

use std::collections::BTreeMap;
use std::process::Stdio;

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{timeout, Duration};

use crate::auth_middleware::AuthContext;
use crate::state::{now_unix_ms, SharedState};
use vks_core::ports::storage::RecordEntry;

use super::error_json;

const PAGE_DEFAULT: usize = 20;
const PAGE_MAX: usize = 200;

fn forbid(auth: &AuthContext) -> Option<Response> {
    if auth.role < vks_core::Role::Operator {
        return Some(error_json(403, "Forbidden: Operator role required"));
    }
    None
}

fn unsupported(what: &str) -> Response {
    error_json(501, &format!("{what} is not supported by this MCU build (read-only mode): the change was not applied"))
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    page: Option<usize>,
    #[serde(default, alias = "pageSize")]
    page_size: Option<usize>,
    #[serde(default, alias = "sortField")]
    sort_field: Option<String>,
    #[serde(default, alias = "sortOrder")]
    sort_order: Option<i64>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default, alias = "eventTypes")]
    event_types: Option<String>,
    #[serde(default, alias = "dateFrom")]
    date_from: Option<i64>,
    #[serde(default, alias = "dateTo")]
    date_to: Option<i64>,
    #[serde(default, alias = "localeId")]
    locale_id: Option<String>,
}

fn page_bounds(query: &PageQuery) -> (usize, usize) {
    let page = query.page.unwrap_or(1).max(1);
    let size = query.page_size.unwrap_or(PAGE_DEFAULT).clamp(1, PAGE_MAX);
    (page, size)
}

fn paginate<T>(items: Vec<T>, page: usize, size: usize) -> (usize, Vec<T>) {
    let count = items.len();
    let offset = page.saturating_sub(1).saturating_mul(size);
    (count, items.into_iter().skip(offset).take(size).collect())
}

/// Vendor `log_events`: событие плоское, UI-маппер `nu` переименовывает поля.
#[derive(Debug, Clone)]
pub(crate) struct LogEvent {
    id: u64,
    pub(crate) event_type: String,
    object_type: String,
    pub(crate) created_at: i64,
    payload: Value,
    invoker: Value,
    event_object: Value,
}

/// Только event_type, для которых vendor-UI 2.2.1.66 имеет шаблон
/// (`index3.bundle`: 53 typeId). Неизвестный тип UI рисует консольной ошибкой
/// «Unknown event template», поэтому внутренние room/session-действия, которых
/// нет в vendor-журнале, в `log_events` не попадают (они остаются в audit API).
fn event_type_for(action: &str) -> Option<(&'static str, &'static str)> {
    let mapped = match action {
        "login_ok" => ("login_success", "users"),
        "login_fail" => ("login_failed", "users"),
        "login_locked" => ("login_rejected", "users"),
        "password.changed" => ("updating_password_success", "users"),
        // Legacy v1-действия и реальные v3-действия UI: журнал должен
        // показывать и старые записи аудита, и то, что шлёт текущий фронтенд.
        "create_user" | "user.created" | "user.credentials.created" => ("creating_credentials_success", "users"),
        "update_user" | "user.credentials.updated" | "profile.credentials.updated" => {
            ("updating_password_success", "users")
        }
        "delete_user" | "user.credentials.deleted" => ("deleting_credentials_success", "users"),
        "set_settings" | "set_global_mode" | "set_room_mode" => ("updating_configuration_security_success", "nodes"),
        action if action.starts_with("settings.") => ("updating_configuration_security_success", "nodes"),
        _ => return None,
    };
    Some(mapped)
}

pub(crate) fn collect_events(state: &SharedState) -> Vec<LogEvent> {
    let entries = state.audit.lock().map(|audit| audit.all().to_vec()).unwrap_or_default();
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let (event_type, object_type) = event_type_for(&entry.action)?;
            let created_at = (entry.ts_ms / 1000) as i64;
            Some(LogEvent {
                id: index as u64 + 1,
                event_type: event_type.to_string(),
                object_type: object_type.to_string(),
                created_at,
                payload: json!({
                    "ip": entry.ip,
                    "error": Value::Null,
                    "login": entry.user,
                }),
                invoker: json!({
                    "ip": entry.ip,
                    "name": entry.user,
                    "avatar": Value::Null,
                }),
                // Шаблон `updating_configuration_security_*` читает
                // eventObject.config_before/config_after; без них UI падает.
                event_object: if object_type == "nodes" {
                    json!({"config_before": {}, "config_after": {}})
                } else {
                    entry.room.clone().map(|room| json!({"id": room})).unwrap_or_else(|| json!({}))
                },
            })
        })
        .collect()
}

fn event_json(event: &LogEvent) -> Value {
    json!({
        "id": event.id,
        "event_type": event.event_type,
        "object_type": event.object_type,
        "created_at": event.created_at,
        "payload": event.payload,
        "invoker": event.invoker,
        "event_object": event.event_object,
        "additional_info": Value::Null,
    })
}

fn filter_events(events: Vec<LogEvent>, query: &PageQuery) -> Vec<LogEvent> {
    let wanted: Vec<String> = query
        .event_types
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    let search = query.search.as_deref().map(str::to_lowercase).filter(|value| !value.is_empty());
    events
        .into_iter()
        .filter(|event| {
            if !wanted.is_empty() && !wanted.iter().any(|value| value == &event.event_type) {
                return false;
            }
            if let Some(from) = query.date_from {
                if event.created_at < from {
                    return false;
                }
            }
            if let Some(to) = query.date_to {
                if event.created_at > to {
                    return false;
                }
            }
            if let Some(search) = search.as_deref() {
                let haystack =
                    format!("{} {} {} {}", event.event_type, event.object_type, event.payload, event.invoker)
                        .to_lowercase();
                if !haystack.contains(search) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// `GET /api/v3/log_events` — журнал событий (аудит) в vendor-форме.
pub async fn log_events(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<PageQuery>,
) -> Response {
    super::track("log_events");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let mut events = filter_events(collect_events(&state), &query);
    let descending = query.sort_order.unwrap_or(1) != 0;
    let sort_by_id = query.sort_field.as_deref() == Some("id");
    events.sort_by_key(|event| if sort_by_id { event.id as i64 } else { event.created_at });
    if descending {
        events.reverse();
    }
    let (page, size) = page_bounds(&query);
    let (count, page_items) = paginate(events, page, size);
    Json(json!({
        "count": count,
        "log_events": page_items.iter().map(event_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// Локальная IANA-таймзона сервера (та же, что в истории): CSV/ZIP-даты
/// обязаны совпадать с тем, что UI показывает в таблице.
async fn export_timezone(state: &SharedState) -> String {
    super::history::public_timezone(state).await
}

/// `GET /api/v3/log_events/export` — ZIP с CSV журнала; файл на `/static/log_events/`.
pub async fn log_events_export(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<PageQuery>,
) -> Response {
    super::track("log_events/export");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let mut events = filter_events(collect_events(&state), &query);
    events.sort_by_key(|event| event.created_at);
    if query.sort_order.unwrap_or(1) != 0 {
        events.reverse();
    }
    let timezone = export_timezone(&state).await;
    let csv = log_events_csv(&events, query.locale_id.as_deref().unwrap_or("ru_RU"), &timezone);
    // 128-битный случайный токен: /static отдаётся без авторизации (скачивание
    // через <a>), 32 бита были бы перебираемы. Имя — в локальном времени UI.
    let token = uuid::Uuid::new_v4().simple().to_string();
    let local_now = local_now_parts(&timezone);
    let name = format!("log_events_{local_now}_{token}.zip");
    let zip = zip_store(&name.replace(".zip", ".csv"), &csv, local_now_epoch(&timezone));
    if let Ok(mut exports) = state.api_v3.log_exports.lock() {
        exports.insert(name.clone(), zip);
    }
    Json(json!({ "filename": name })).into_response()
}

fn log_events_csv(events: &[LogEvent], locale: &str, timezone: &str) -> Vec<u8> {
    let header = if locale.starts_with("ru") {
        "ID;Дата;Событие;Объект;Пользователь;IP;Дополнительно\n"
    } else {
        "ID;Date;Event;Object;User;IP;Additional\n"
    };
    let mut out = String::from(header);
    for event in events {
        out.push_str(&format!(
            "{};{};{};{};{};{};{}\n",
            event.id,
            format_local_datetime(event.created_at, timezone),
            csv_escape(&event.event_type),
            csv_escape(&event.object_type),
            csv_escape(
                event
                    .invoker
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && *value != "00000000-0000-0000-0000-000000000000")
                    .or_else(|| event.payload.get("login").and_then(Value::as_str))
                    .unwrap_or(""),
            ),
            csv_escape(event.payload.get("ip").and_then(Value::as_str).unwrap_or("")),
            csv_escape(&event.event_object.to_string()),
        ));
    }
    out.into_bytes()
}

/// `GET /static/log_events/{filename}` — скачивание ранее выданного ZIP.
pub async fn static_log_export(State(state): State<SharedState>, Path(filename): Path<String>) -> Response {
    if filename.contains(['/', '\\']) || filename.contains("..") {
        return error_json(400, "invalid export filename").into_response();
    }
    let bytes = state.api_v3.log_exports.lock().ok().and_then(|mut exports| exports.get(&filename));
    match bytes {
        Some(bytes) => ([(header::CONTENT_TYPE, "application/zip")], bytes).into_response(),
        None => error_json(404, "Log export not found").into_response(),
    }
}

fn local_now_parts(timezone: &str) -> String {
    let now = chrono::Utc::now();
    timezone
        .parse::<chrono_tz::Tz>()
        .map(|tz| now.with_timezone(&tz).format("%Y_%m_%d_%H%M%S").to_string())
        .unwrap_or_else(|_| now.format("%Y_%m_%d_%H%M%S").to_string())
}

fn local_now_epoch(timezone: &str) -> i64 {
    let now = chrono::Utc::now();
    timezone.parse::<chrono_tz::Tz>().map(|tz| now.with_timezone(&tz).timestamp()).unwrap_or_else(|_| now.timestamp())
}

fn format_local_datetime(epoch: i64, timezone: &str) -> String {
    let Some(utc) = chrono::DateTime::from_timestamp(epoch, 0) else { return String::new() };
    timezone
        .parse::<chrono_tz::Tz>()
        .map(|tz| utc.with_timezone(&tz).format("%d.%m.%Y %H:%M:%S").to_string())
        .unwrap_or_else(|_| utc.format("%d.%m.%Y %H:%M:%S").to_string())
}

fn csv_escape(value: &str) -> String {
    let value =
        if value.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{value}") } else { value.to_string() };
    if value.contains([';', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

// ---------- network interfaces ----------

fn ipv4_netmask(prefix: u32) -> String {
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32)) };
    format!("{}.{}.{}.{}", mask >> 24, (mask >> 16) & 0xff, (mask >> 8) & 0xff, mask & 0xff)
}

fn resolvers() -> Vec<String> {
    std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver ").map(|value| value.trim().to_string()))
        .filter(|value| !value.is_empty())
        .collect()
}

fn default_route() -> (String, bool) {
    let output = std::process::Command::new("ip").args(["-j", "route", "show", "default"]).output();
    let Ok(output) = output else { return (String::new(), false) };
    let routes: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let route = routes.first();
    let gateway = route.and_then(|route| route.get("gateway")).and_then(Value::as_str).unwrap_or_default().to_string();
    let dhcp = route.and_then(|route| route.get("protocol")).and_then(Value::as_str) == Some("dhcp");
    (gateway, dhcp)
}

fn network_interfaces() -> Vec<Value> {
    let output = std::process::Command::new("ip").args(["-j", "addr", "show"]).output();
    let Ok(output) = output else { return Vec::new() };
    let interfaces: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let (gateway, use_dhcp) = default_route();
    let nameservers = {
        let values = resolvers();
        if values.is_empty() && !gateway.is_empty() {
            vec![gateway.clone()]
        } else {
            values
        }
    };
    interfaces
        .into_iter()
        .filter_map(|interface| {
            let name = interface.get("ifname").and_then(Value::as_str).unwrap_or_default().to_string();
            if name.is_empty() || name == "lo" {
                return None;
            }
            let addresses: Vec<Value> = interface
                .get("addr_info")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter(|item| item.get("family").and_then(Value::as_str) == Some("inet"))
                        .filter_map(|item| {
                            let ipaddress = item.get("local").and_then(Value::as_str)?;
                            let prefix = item.get("prefixlen").and_then(Value::as_u64).unwrap_or(24) as u32;
                            Some(json!({"ipaddress": ipaddress, "netmask": ipv4_netmask(prefix)}))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if addresses.is_empty() {
                return None;
            }
            let ready = matches!(interface.get("operstate").and_then(Value::as_str), Some("UP") | Some("UNKNOWN"));
            Some(json!({
                "interface": name,
                "mac": interface.get("address").and_then(Value::as_str).unwrap_or_default().to_uppercase(),
                "use_dhcp": use_dhcp,
                "gateway": gateway,
                "nameservers": nameservers,
                "ready": ready,
                "addresses": addresses,
            }))
        })
        .collect()
}

/// `GET /api/v3/system/network/interfaces` — реальные интерфейсы хоста.
pub async fn network_interfaces_list(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
) -> Response {
    super::track("system/network/interfaces");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let interfaces = network_interfaces();
    Json(json!({"count": interfaces.len(), "interfaces": interfaces})).into_response()
}

/// `GET /api/v3/system/network/interfaces/{interface}` — детали одного интерфейса.
pub async fn network_interface_get(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(interface): Path<String>,
) -> Response {
    super::track("system/network/interfaces/{interface}");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    match network_interfaces().into_iter().find(|value| value["interface"].as_str() == Some(&interface)) {
        Some(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.remove("interface");
            }
            Json(json!({"interface": value})).into_response()
        }
        None => error_json(404, "Network interface not found").into_response(),
    }
}

/// `PATCH /api/v3/system/network/interfaces/{interface}` — изменение сети не
/// применяется (нет прав CAP_NET_ADMIN и риск потерять удалённый доступ).
pub async fn network_interface_patch(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(_interface): Path<String>,
) -> Response {
    super::track("system/network/interfaces/{interface}");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    unsupported("Network interface configuration")
}

// ---------- system utils ----------

fn validated_address(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 || value.starts_with('-') {
        return None;
    }
    if !value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_')) {
        return None;
    }
    Some(value.to_string())
}

async fn run_util(program: &str, args: &[&str]) -> Response {
    if !binary_available(program) {
        return error_json(503, &format!("{program} is not installed on this host")).into_response();
    }
    let execution = tokio::process::Command::new(program).args(args).stdin(Stdio::null()).kill_on_drop(true).output();
    match timeout(Duration::from_secs(10), execution).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Json(json!({
                "stdout": if stdout.is_empty() { Value::Null } else { Value::String(stdout) },
                "stderr": if stderr.is_empty() { Value::Null } else { Value::String(stderr) },
            }))
            .into_response()
        }
        Ok(Err(error)) => Json(json!({"stdout": Value::Null, "stderr": format!("{program}: {error}")})).into_response(),
        Err(_) => Json(json!({"stdout": Value::Null, "stderr": format!("{program}: timeout")})).into_response(),
    }
}

/// Unix-время старта текущего процесса из `/proc` (`btime + starttime/HZ`).
///
/// Fallback — текущий момент, если `/proc` недоступен (не Linux): тогда
/// timestamp стабилен, но не является временем старта.
fn process_start_unix() -> i64 {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let stat = read("/proc/self/stat");
    let proc_stat = read("/proc/stat");
    let start_ticks = stat
        .as_deref()
        .and_then(|text| text.rsplit_once(')'))
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|value| value.parse::<u64>().ok());
    let btime = proc_stat.as_deref().and_then(|text| {
        text.lines().find_map(|line| line.strip_prefix("btime ")).and_then(|value| value.trim().parse::<u64>().ok())
    });
    let hz = 100u64; // USER_HZ на Linux
    match (btime, start_ticks) {
        (Some(btime), Some(ticks)) => (btime + ticks / hz) as i64,
        _ => now_unix_ms() as i64 / 1000,
    }
}

fn binary_available(program: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {program} >/dev/null 2>&1")])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// `GET /api/v3/system/utils/ping?address=` — реальный ping (ограниченный ввод).
pub async fn utils_ping(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    super::track("system/utils/ping");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let Some(address) = query.get("address").and_then(|value| validated_address(value)) else {
        return error_json(422, "address: invalid host").into_response();
    };
    run_util("ping", &["-c", "3", "-W", "2", "-n", &address]).await
}

/// `GET /api/v3/system/utils/traceroute?address=` — реальный traceroute, если установлен.
pub async fn utils_traceroute(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    super::track("system/utils/traceroute");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let Some(address) = query.get("address").and_then(|value| validated_address(value)) else {
        return error_json(422, "address: invalid host").into_response();
    };
    run_util("traceroute", &["-n", "-m", "5", "-w", "2", &address]).await
}

/// `GET /api/v3/system/utils/tcpdumps/status` — tcpdump не запускается этим
/// бинарником (нет CAP_NET_RAW/фонового процесса), статус честно `false`.
pub async fn tcpdumps_status(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/utils/tcpdumps/status");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    Json(json!({"running": false})).into_response()
}

/// `GET /api/v3/system/utils/tcpdumps` — список файлов дампов (пусто).
pub async fn tcpdumps_list(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/utils/tcpdumps");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    Json(json!({"files": []})).into_response()
}

/// `POST /api/v3/system/utils/tcpdumps` — запуск дампа не поддерживается.
pub async fn tcpdumps_start(Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/utils/tcpdumps");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    unsupported("Starting a packet capture")
}

/// `DELETE /api/v3/system/utils/tcpdumps` — остановка дампа не поддерживается.
pub async fn tcpdumps_stop(Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/utils/tcpdumps");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    unsupported("Stopping a packet capture")
}

/// `DELETE /api/v3/system/utils/tcpdumps/{file}` — удаление дампа не поддерживается.
pub async fn tcpdumps_delete(Extension(auth): Extension<AuthContext>, Path(_file): Path<String>) -> Response {
    super::track("system/utils/tcpdumps/{file}");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    unsupported("Deleting a packet capture")
}

// ---------- terminals ----------

/// `GET /api/v3/terminals` — список терминалов; в этой сборке терминалов нет
/// (vendor-совместимый пустой список, а не 501).
pub async fn terminals_list(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<PageQuery>,
) -> Response {
    super::track("terminals");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let _ = page_bounds(&query);
    Json(json!({"count": 0, "terminals": []})).into_response()
}

/// `GET /api/v3/terminals/credentials` — учётные данные терминалов не настроены.
pub async fn terminals_credentials(Extension(auth): Extension<AuthContext>) -> Response {
    super::track("terminals/credentials");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    error_json(404, "User credentials not found").into_response()
}

/// `POST/PATCH/DELETE /api/v3/terminals*` — управление терминалами не поддерживается.
pub async fn terminals_unsupported(Extension(auth): Extension<AuthContext>) -> Response {
    super::track("terminals");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    unsupported("Terminal management")
}

/// `GET /api/v3/terminals/firmwares|configurations` — пустые списки.
pub async fn terminals_empty_list(Extension(auth): Extension<AuthContext>) -> Response {
    if let Some(response) = forbid(&auth) {
        return response;
    }
    Json(json!({"count": 0, "firmwares": [], "configurations": []})).into_response()
}

// ---------- videorecords ----------

fn record_json(record: &RecordEntry) -> Value {
    let started_at = (record.created_ms / 1000) as i64;
    json!({
        "id": record.id,
        "display_name": record.id,
        "started_at": started_at,
        "stopped_at": started_at + record.duration_sec as i64,
        "session": {"id": record.room_id.to_string(), "display_name": record.room_id.to_string()},
        "video_layout": {"id": Value::Null, "display_name": Value::Null},
        "payload": {
            "bitrate": 0,
            "title": Value::Null,
            "profile": Value::Null,
            "src": record.id,
            "preview": Value::Null,
            "resolution": [1280, 720],
            "meta": {"size": record.size_bytes},
        },
    })
}

async fn collect_records(state: &SharedState) -> Vec<RecordEntry> {
    if let Some(storage) = &state.storage {
        if let Ok(records) = storage.list_records(None).await {
            return records;
        }
    }
    state.records.lock().map(|records| records.values().cloned().collect()).unwrap_or_default()
}

/// `GET /api/v3/videorecords` — записи из общего store в vendor-форме.
pub async fn videorecords_list(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<PageQuery>,
) -> Response {
    super::track("videorecords");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let records = collect_records(&state).await;
    videorecords_response(records, &query)
}

/// Алиасы `GET /sessions/{id}/videorecords` и `/conferences/{id}/videorecords`:
/// запись принадлежит сессии, поэтому id из пути обязан фильтровать список.
pub async fn session_videorecords_list(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Path(session_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    super::track("sessions/{id}/videorecords");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let records =
        collect_records(&state).await.into_iter().filter(|record| record.room_id.to_string() == session_id).collect();
    videorecords_response(records, &query)
}

fn videorecords_response(mut records: Vec<RecordEntry>, query: &PageQuery) -> Response {
    if let Some(search) = query.search.as_deref().filter(|value| !value.is_empty()) {
        let search = search.to_lowercase();
        records.retain(|record| {
            record.id.to_lowercase().contains(&search) || record.room_id.to_string().to_lowercase().contains(&search)
        });
    }
    let descending = query.sort_order.unwrap_or(1) != 0;
    // vendor UI сортирует по display_name/started_at; id — стабильный tie-breaker.
    let by_started = query.sort_field.as_deref() != Some("display_name");
    records.sort_by_key(|record| (if by_started { record.created_ms } else { 0 }, record.id.clone()));
    if descending {
        records.reverse();
    }
    let (page, size) = page_bounds(query);
    let (count, page_items) = paginate(records, page, size);
    Json(json!({
        "count": count,
        "videorecords": page_items.iter().map(record_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `DELETE /api/v3/videorecords/{id}` — удаление файлов не поддерживается.
pub async fn videorecords_delete(Extension(auth): Extension<AuthContext>, Path(_id): Path<String>) -> Response {
    super::track("videorecords/{id}");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    unsupported("Deleting a video record")
}

/// `GET /api/v3/conferences/{id}/videorecords/estimated_time` — оценка записи.
pub async fn videorecords_estimated_time(Extension(auth): Extension<AuthContext>) -> Response {
    super::track("videorecords/estimated_time");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let recording_time = if state_disk_free_gb() < 1 { 0 } else { 3600 };
    Json(json!({"recording_time": recording_time})).into_response()
}

fn state_disk_free_gb() -> u64 {
    let Ok(output) = std::process::Command::new("df").args(["-Pk", "/"]).output() else { return 0 };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|value| value.parse::<u64>().ok())
        .map(|kib| kib / 1_048_576)
        .unwrap_or(0)
}

// ---------- system statistics (vendor-UI «Статистика») ----------

fn meminfo_kb(key: &str) -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo").ok()?.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name != key {
            return None;
        }
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// `GET /api/v3/system/stats/memory` — реальная память хоста (`/proc/meminfo`), MB.
pub async fn stats_memory(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/stats/memory");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let total = meminfo_kb("MemTotal").unwrap_or(0) / 1024;
    let available = meminfo_kb("MemAvailable").unwrap_or(0) / 1024;
    Json(json!({"total": total, "used": total.saturating_sub(available), "free": available})).into_response()
}

fn cpu_times() -> Vec<(u64, u64, u64)> {
    let Ok(text) = std::fs::read_to_string("/proc/stat") else { return Vec::new() };
    text.lines()
        .filter(|line| line.starts_with("cpu") && !line.starts_with("cpu "))
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            parts.next()?;
            let values: Vec<u64> = parts.take(8).filter_map(|value| value.parse().ok()).collect();
            if values.len() < 4 {
                return None;
            }
            let user = values[0] + values[1];
            let kernel = values[2];
            let idle = values[3] + values.get(4).copied().unwrap_or(0);
            Some((user, kernel, idle))
        })
        .collect()
}

/// `GET /api/v3/system/stats/cpu` — загрузка по ядрам (дельта `/proc/stat`).
pub async fn stats_cpu(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/stats/cpu");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let first = cpu_times();
    tokio::time::sleep(Duration::from_millis(120)).await;
    let second = cpu_times();
    let load: Vec<Value> = second
        .iter()
        .enumerate()
        .map(|(index, current)| {
            let previous = first.get(index).unwrap_or(&(0, 0, 0));
            let user = current.0.saturating_sub(previous.0) as f64;
            let kernel = current.1.saturating_sub(previous.1) as f64;
            let idle = current.2.saturating_sub(previous.2) as f64;
            let total = (user + kernel + idle).max(1.0);
            let percent = |value: f64| (value * 1000.0 / total).round() / 10.0;
            json!({"user_mode": percent(user), "kernel_mode": percent(kernel), "idle": percent(idle)})
        })
        .collect();
    Json(json!({"count": load.len(), "load": load})).into_response()
}

/// `GET /api/v3/system/stats/disk` — реальные ФС (`df -Pk`), **MB**.
///
/// Vendor-UI `StatisticsDisk` делит значения на 1024 и подписывает «Gb»,
/// поэтому отдаём мебибайты (как vendor-эталон и статистика памяти).
pub async fn stats_disk(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/stats/disk");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let output = std::process::Command::new("df").args(["-Pk"]).output();
    let mut spaces = Vec::new();
    if let Ok(output) = output {
        for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 6 {
                continue;
            }
            let Ok(total) = fields[1].parse::<u64>() else { continue };
            let Ok(used) = fields[2].parse::<u64>() else { continue };
            let Ok(free) = fields[3].parse::<u64>() else { continue };
            // `df -Pk` — KiB; UI ожидает MB.
            let (total, used, free) = (total / 1024, used / 1024, free / 1024);
            if total == 0 || fields[0].starts_with("/dev/loop") {
                continue;
            }
            spaces.push(json!({"name": fields[5], "total": total, "used": used, "free": free}));
        }
    }
    Json(json!({"count": spaces.len(), "spaces": spaces})).into_response()
}

/// Счётчики `/proc/net/dev`: `(rx_bytes, tx_bytes)` по интерфейсу.
fn netdev_counters() -> std::collections::HashMap<String, (u64, u64)> {
    let mut counters = std::collections::HashMap::new();
    if let Ok(text) = std::fs::read_to_string("/proc/net/dev") {
        for line in text.lines().skip(2) {
            let Some((name, rest)) = line.split_once(':') else { continue };
            let name = name.trim();
            if name == "lo" || name.is_empty() {
                continue;
            }
            let fields: Vec<u64> = rest.split_whitespace().filter_map(|value| value.parse().ok()).collect();
            if fields.len() >= 9 {
                counters.insert(name.to_string(), (fields[0], fields[8]));
            }
        }
    }
    counters
}

/// `GET /api/v3/system/stats/network` — скорость RX/TX, **kbps** (дельта за 200 мс).
///
/// `/proc/net/dev` отдаёт накопительные байты; vendor-UI подписывает значения
/// «кбит/сек» и не конвертирует, поэтому считаем дельту и переводим в kbit/s.
pub async fn stats_network(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/stats/network");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let first = netdev_counters();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = netdev_counters();
    let interfaces: Vec<Value> = second
        .iter()
        .map(|(name, (rx, tx))| {
            let (previous_rx, previous_tx) = first.get(name).copied().unwrap_or((*rx, *tx));
            // Байты за 0.2 с → килобиты в секунду: delta * 8 / 1000 / 0.2.
            let kbps = |delta: u64| ((delta as f64) * 8.0 / 1000.0 / 0.2).round() as u64;
            json!({"name": name, "rx": kbps(rx.saturating_sub(previous_rx)), "tx": kbps(tx.saturating_sub(previous_tx))})
        })
        .collect();
    Json(json!({"count": interfaces.len(), "interfaces": interfaces})).into_response()
}

/// `GET /api/v3/system/packages` — установленные пакеты; в этой сборке их нет.
pub async fn system_packages(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/packages");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    Json(json!({"packages": []})).into_response()
}

/// `GET /api/v3/engine/log` — журнал медиа-движка.
///
/// Бинарник не поднимает отдельный CrystalVu-процесс, поэтому отдаём реальную
/// стартовую запись сервиса в vendor-форме (`bounds.id`/`bounds.timestamp`,
/// `log[{id,timestamp,src,class,msg}]`), достаточную для инициализации UI.
pub async fn engine_log(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("engine/log");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    // Реальное время старта процесса (btime + starttime/HZ), фиксируем один
    // раз: иначе «журнал» каждый раз выглядит как новая запись.
    static STARTED: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let started = *STARTED.get_or_init(process_start_unix);
    Json(json!({
        "language": "ru",
        "bounds": {"id": {"min": 1, "max": 1}, "timestamp": {"min": started, "max": started}},
        "log": [{
            "id": 1,
            "timestamp": started,
            "src": "MCU",
            "class": "INFO",
            "msg": "media runtime ready (built-in engine)",
        }],
    }))
    .into_response()
}

// ---------- ZIP writer (shared with history) ----------

fn zip_store(name: &str, data: &[u8], modified_secs: i64) -> Vec<u8> {
    super::zip_store::zip_store(name, data, modified_secs)
}
