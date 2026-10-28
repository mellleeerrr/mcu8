//! Boot/shell-эндпоинты (§4.5 плана) за token-middleware: profile, справочники,
//! статус системы, лицензия, настройки, кодеки.
//!
//! Мапперы UI мягкие (`snake_case → camelCase`, отсутствующие поля отбрасываются),
//! поэтому конверты минимальны, но ключевые для навигации поля обязательны:
//! `ui/system/status.engine.description` (guard: `INVALID_LICENSE`/`FAILED` → экраны-заглушки).

use axum::{
    extract::{Extension, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use vks_auth::jwt::VerifiedJwt;

use crate::auth_middleware::AuthContext;

use super::{auth, track};
use crate::settings::SettingsService;
use crate::state::SharedState;

// `Operator` сохраняет wildcard "all": им бандл гейтит «Diagnostics»/«Date and time».
// `calibration:*` не выдаём ни одной роли (раздел удалён, T1.1).
fn role_to_permissions(role: vks_core::Role) -> &'static [&'static str] {
    match role {
        vks_core::Role::Operator => &["all"],
        vks_core::Role::Moderator => &[
            "conferences:view",
            "conferences:create",
            "conferences:update",
            "conferences:delete",
            "sessions:view",
            "sessions:create",
            "sessions:update",
            "sessions:delete",
        ],
        vks_core::Role::Participant => &["sessions:view", "conferences:view"],
        vks_core::Role::Viewer => &["sessions:view"],
    }
}

/// `GET /api/v3/profile` → `{user:{…}}` (маппер `RW`; permissions зависят от роли).
pub async fn profile(State(state): State<SharedState>, Extension(verified): Extension<VerifiedJwt>) -> Response {
    track("profile");
    let username_from_db = state.local_db.as_ref().and_then(|db| db.user(&verified.user_id).ok()).map(|u| u.username);

    let directory_profile = {
        let directory_id = state
            .local_db
            .as_ref()
            .and_then(|db| db.user(&verified.user_id).ok())
            .and_then(|local| {
                crate::users::Users::list(&state)
                    .into_iter()
                    .find(|u| u.username == local.username || u.id.to_string() == verified.user_id)
            })
            .map(|u| u.id.to_string())
            .unwrap_or_else(|| verified.user_id.clone());
        let resources = state.api_v3.resources.lock().await;
        resources.directory.users.get(&directory_id).cloned()
    };
    let display_name = directory_profile
        .as_ref()
        .map(|p| p.display_name.clone())
        .filter(|d| !d.is_empty())
        .or_else(|| username_from_db.clone())
        .unwrap_or_else(|| verified.user_id.clone());
    // T7: PATCH /profile пишет эти поля в directory-профиль — GET обязан их
    // отдавать, иначе UI видит изменение только display_name.
    let profile_locale = directory_profile.as_ref().and_then(|p| p.locale_id.clone());
    let profile_timezone = directory_profile.as_ref().and_then(|p| p.time_zone_id.clone());
    let profile_emails = directory_profile.as_ref().map(|p| p.emails.clone()).unwrap_or_default();
    let profile_phones = directory_profile.as_ref().map(|p| p.phones.clone()).unwrap_or_default();
    let profile_description = directory_profile.as_ref().and_then(|p| p.description.clone());
    let profile_view_own_only = directory_profile.as_ref().is_some_and(|p| p.view_own_only);

    // Роль — только из admin-хранилища (Users); неизвестному субъекту — Viewer
    // (least privilege). Единая точка резолвинга id/username — `Users::resolve_role`.
    let role = crate::users::Users::resolve_role(&state, &verified.user_id).unwrap_or(vks_core::Role::Viewer);

    let permissions = role_to_permissions(role);
    // Таймзона администратора (PATCH /system/timezone) важнее TZ хоста.
    let timezone = {
        let resources = state.api_v3.resources.lock().await;
        resources
            .admin_settings
            .get("timezone")
            .and_then(|value| value.get("timezone"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(system_timezone)
    };

    Json(json!({
        "user": {
            "id": verified.user_id,
            "display_name": display_name,
            "role": role.as_str(),
            // Эталонный `profile` отдаёт `roles[]` (карточка/гейты UI), а не
            // только строку `role`. Оставляем оба поля: старое читается
            // нашими проверками, новое — vendor-маппером.
            "roles": [{
                "id": role.as_str(),
                "display_name": role.as_str(),
                "permissions": permissions
            }],
            "view_own_only": profile_view_own_only,
            "address": "",
            "payload": {},
            "permissions": permissions,
            "locale_id": profile_locale.unwrap_or_else(|| "ru_RU".to_string()),
            "time_zone_id": profile_timezone.unwrap_or(timezone),
            "groups": [],
            "emails": profile_emails,
            "phones": profile_phones,
            "avatars": [],
            "description": profile_description.unwrap_or_default(),
            "creator_id": "",
            "created_at": 0,
            "busy": false,
            "defaults": { "deletable": false, "editable": true }
        }
    }))
    .into_response()
}

/// IANA-таймзона хоста: `TZ` → `/etc/timezone` → UTC.
pub(crate) fn system_timezone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim().trim_start_matches(':');
        if !tz.is_empty() {
            return tz.to_string();
        }
    }
    std::fs::read_to_string("/etc/timezone")
        .ok()
        .map(|tz| tz.trim().to_string())
        .filter(|tz| !tz.is_empty())
        .unwrap_or_else(|| "UTC".to_string())
}

/// Все IANA-зоны vendor-списка (ref-92/time-zones, 428 записей).
const TIME_ZONES: &str = include_str!("../../assets/time-zones.json");

fn time_zone_list() -> Value {
    serde_json::from_str(TIME_ZONES).unwrap_or_else(|_| json!({ "count": 0, "time_zones": [] }))
}

/// Идентификаторы зон для валидации `PATCH /system/timezone`.
pub fn time_zone_ids() -> Vec<String> {
    time_zone_list()
        .get("time_zones")
        .and_then(Value::as_array)
        .map(|zones| {
            zones.iter().filter_map(|zone| zone.get("id").and_then(Value::as_str).map(str::to_string)).collect()
        })
        .unwrap_or_default()
}

/// `GET /api/v3/time-zones` → `{count,time_zones:[{id}]}` (маппер `LW`).
pub async fn time_zones() -> Response {
    track("time-zones");
    Json(time_zone_list()).into_response()
}

/// `GET /api/v3/locales` → `{locales:[{id, description}]}` (маппер `xW`).
pub async fn locales() -> Response {
    track("locales");
    Json(json!({
        "locales": [
            { "id": "ru_RU", "description": "Русский" },
            { "id": "en_US", "description": "English" }
        ]
    }))
    .into_response()
}

/// `GET /api/v3/abonents/stop_reasons` → `{}` (raw, маппер мягкий).
pub async fn stop_reasons() -> Response {
    track("abonents/stop_reasons");
    Json(json!({})).into_response()
}

/// `GET /api/v3/abonents/count` → `{count:N}` (наши пользователи).
pub async fn abonents_count(State(state): State<SharedState>) -> Response {
    track("abonents/count");
    let count = state.users.lock().expect("users store poisoned").len();
    let rooms = state.rooms.lock().expect("rooms store poisoned");
    let total_sessions = rooms.len();
    let mut total_participants = 0;
    let mut webrtc = 0;
    let mut sip = 0;
    let mut h323 = 0;
    for (rid, r) in rooms.iter() {
        total_participants += r.participants;
        for p in state.room_hub.list_participants(rid) {
            if p.participant_id.contains("@sip-gateway") {
                sip += 1;
            } else if p.participant_id.contains("@h323-gateway") {
                h323 += 1;
            } else {
                webrtc += 1;
            }
        }
    }
    if webrtc + sip + h323 < total_participants {
        webrtc = total_participants - sip - h323;
    }
    Json(json!({
        "count": count,
        "connections": {
            "total": total_participants,
            "webrtc": webrtc,
            "sip": sip,
            "h323": h323,
            "session": total_sessions,
            "waitingroom": 0,
            "lockedroom": 0
        }
    }))
    .into_response()
}

/// `GET /api/v3/ui/system/status` → форма маппера `DW`
/// (`engine/database/network/disk/sip/h323`; навигация-гард деструктурирует `status`).
pub async fn ui_system_status(State(state): State<SharedState>) -> Response {
    track("ui/system/status");
    let settings = SettingsService::get(&state);
    let readiness = crate::health::Readiness::from_state(&state).await;

    let engine_status = if readiness.media { "READY" } else { "FAILED" };
    let engine_desc = if readiness.media { "" } else { "Media engine degraded" };
    let db_status = if readiness.db { "READY" } else { "FAILED" };

    // T11a: статус шлюза = реальная регистрация + последний транспортный
    // отказ. Отказ терминала (busy/decline) не гасит живой шлюз и не лечится
    // таймаутом; READY без факта регистрации не выставляется.
    let dialer = state.outbound_dialer();
    let registered =
        |protocol: &str| dialer.as_ref().and_then(|dialer| dialer.gateway_registration(protocol)).unwrap_or(false);
    let health = |protocol: &str| {
        let enabled = registered(protocol);
        let probe = state.gateway_health(protocol);
        // SIP UAS регистрируется по-настоящему, поэтому его регистрация —
        // факт. H.323 — direct mode без регистрации: READY только после
        // реального H.225/H.245 setup, `last_success_at` переживает отказы.
        let ready_fact =
            if protocol == "sip" { enabled } else { probe.as_ref().and_then(|entry| entry.last_success_at).is_some() };
        let registration = if protocol == "sip" { enabled } else { ready_fact };
        if state.has_fault(protocol) || state.has_fault("gateway") {
            return ("FAILED".to_string(), false, registration, probe.and_then(|probe| probe.error));
        }
        match probe {
            Some(probe) if probe.ok => ("READY".to_string(), true, true, probe.error),
            Some(probe) if !probe.terminal => ("FAILED".to_string(), false, registration, probe.error),
            // Терминальный отказ = реальный Q.931 ответ терминала: транспорт
            // шлюза жив, поэтому READY + last_error, а не FAILED/DISABLED.
            Some(probe) => ("READY".to_string(), true, true, probe.error),
            None => (if ready_fact { "READY" } else { "DISABLED" }.to_string(), ready_fact, registration, None),
        }
    };
    let (sip_status, sip_connected, sip_registered, sip_error) = health("sip");
    let (h323_status, h323_connected, h323_registered, h323_error) = health("h323");
    let cluster_healthy = readiness.cluster;
    let cluster_status = if cluster_healthy { "READY" } else { "FAILED" };
    let ip = state.server_addr.get().cloned().unwrap_or_else(|| settings.ip.clone());
    // Размер физического SQLite: main + WAL (иначе после checkpoint цифры
    // скачут). Для in-memory/PG файла нет — null, а не ложный 0 при READY.
    let db_size = state.database_file.get().and_then(|path| {
        let main = std::fs::metadata(path).ok()?.len();
        let wal = std::fs::metadata(format!("{path}-wal")).ok().map_or(0, |metadata| metadata.len());
        Some(main.saturating_add(wal))
    });

    let matrix = std::env::var("VKS_SDP_CODEC_MATRIX").ok();
    let transcoder = std::env::var("VKS_TRANSCODER_ENABLED").ok();
    let lab = std::env::var("VKS_SDP_CODEC_MATRIX_LAB").ok();
    let matrix_mode =
        vks_common::codec_capabilities::matrix_mode(matrix.as_deref(), transcoder.as_deref(), lab.as_deref());
    Json(json!({
        "codecs": {
            "matrix": matrix_mode,
            // В этой сборке транскодера нет: `full` — lab-гейт для SDP,
            // а не признак наличия транскодирующих воркеров (T06b).
            "transcoder": false,
            "native_audio": vks_common::codec_capabilities::NATIVE_AUDIO,
            "native_video": vks_common::codec_capabilities::NATIVE_VIDEO
        },
        "engine": {
            "status": engine_status,
            "connected": readiness.media,
            "description": engine_desc,
            "version": env!("CARGO_PKG_VERSION"),
            "type": "mcu",
            "name": settings.name
        },
        "database": {
            "status": db_status,
            "connected": readiness.db,
            "size_bytes": db_size
        },
        "network": { "status": "READY", "connected": true, "editable": true, "ip": ip },
        "disk": disk_status(),
        "time": crate::state::now_unix_ms() / 1000,
        "sip": {
            "status": sip_status,
            "connected": sip_connected,
            "enable": dialer.is_some(),
            "registration": sip_registered,
            // `sip.account` в эталоне — boolean (isAvailable), не строка.
            "account": sip_connected,
            "local_address": ip,
            "last_error": sip_error
        },
        "h323": {
            "status": h323_status,
            "connected": h323_connected,
            "enable": dialer.as_ref().is_some_and(|dialer| dialer.gateway_registration("h323") == Some(true)),
            "registration": h323_registered,
            "account": h323_connected,
            "local_address": ip,
            "last_error": h323_error
        },
        "cluster": {
            "status": cluster_status,
            "connected": cluster_healthy,
            "lease_valid": cluster_healthy
        }
    }))
    .into_response()
}

/// `GET /api/v3/system/status/network` — vendor-вкладка «Сетевые интерфейсы».
///
/// `editable:true` обязателен: vendor-бандл рисует список интерфейсов только
/// для редактируемого менеджера (`ManagerContent`), при `false` показывает
/// «Список пуст». Сохранение сети при этом честно не поддерживается —
/// `PATCH /api/v3/system/network/interfaces/{interface}` отвечает 501.
pub async fn status_network(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    track("system/status/network");
    if let Some(response) = super::settings_admin::forbid(&auth) {
        return response;
    }
    Json(json!({"network": {"editable": true}})).into_response()
}

/// `GET /api/v3/license` → постоянная лицензия в форме vendor-маппера `kk`.
///
/// `last_expired: 0` vendor-UI рисует как «01.01.1970» и «Продлить», поэтому
/// для бессрочной лицензии отдаём далёкий срок и реальные `params` (UI читает
/// их для вкладок возможностей).
pub async fn license(State(_state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    track("license");
    if let Some(response) = super::settings_admin::forbid(&auth) {
        return response;
    }
    Json(license_payload()).into_response()
}

/// Общий конверт `/license` для регистрации/активации: UI читает те же поля.
fn license_payload() -> Value {
    let now = crate::state::now_unix_ms() as i64 / 1000;
    json!({
        "state": "VALID",
        "type": "PERMANENT",
        "demo": false,
        "count": 1,
        "serial_id": "VKS-PERMANENT",
        "first_expired": now,
        "last_expired": now + 100 * 365 * 24 * 3600,
        "last_update_failed": false,
        "params": {
            "audio": {
                "codecs": {
                    "g711": true, "g719": true, "g722": true, "g723": true, "g726": true,
                    "g728": true, "g729": true, "ilbc": true, "opus": true, "aac-lc": true,
                    "g722.1": true, "g722.1c": true
                },
                "active_speaker": true
            },
            "video": {
                "fps": 60,
                "fecc": true,
                "codecs": {
                    "vp8": true, "h261": true, "h263": true, "h264": true, "h265": true,
                    "hd-4k": true, "h264-svc": true, "hd-1080p": true, "h263_plus": true,
                    "h264-high": true, "advanced-nal-packetization": true
                },
                "max_active": 100,
                "presentation": {"enable": true},
                "rate_adaption": true,
                "custom_layouts": true
            },
            "protocol": {
                "nat": true, "sip": true, "h323": true, "ldap": null, "rtmp": true, "rtsp": true,
                "snmp": true, "srtp": true, "webrtc": true, "max_calls": 100,
                "conference_control": "EXTERNAL"
            },
            "server": {"numa": true, "server": true, "secrecy": false, "unrestricted": false, "max_conferences": 4},
            "storage": {"recording": true}
        }
    })
}

/// `GET /api/v3/features` → `{features:[...]}` (vendor-UI итерирует массив).
pub async fn features(Extension(auth): Extension<AuthContext>) -> Response {
    track("features");
    if let Some(response) = super::settings_admin::forbid(&auth) {
        return response;
    }
    Json(json!({
        "features": [
            "VLOOP", "MEDIA", "H323", "SIP", "RTSP", "VNC", "PREVIEW", "CONFLOOP", "FILE", "REC",
            "RTMP", "YOUTUBE", "ADAPTIVE_RATE_CONTROL", "AUDIO_CHANNELS", "CONFERENCE_CONTROL",
            "CUSTOM_LAYOUTS", "DEMO", "FECC", "JOURNAL", "MCU", "NAT", "NETSTORAGE", "NUMA",
            "PRESENTATION", "SNMP", "SRTP", "VAD", "AAC", "G.711", "G.719", "G.722", "G.722.1",
            "G.722.1C", "G.723", "G.726", "G.728", "G.729", "OPUS", "H.261", "H.263", "H.264",
            "H.264-HIGH", "H.265", "4K", "FHD", "HD", "SD", "CIF", "QCIF", "ADVANCED_NAL_PACKETIZATION"
        ]
    }))
    .into_response()
}

/// `GET /api/v3/engine/calibration` → `{"calibration":null}`.
///
/// Калибровка в нашей MCU не поддерживается (и убрана из UX, T1.1). Форма ответа —
/// байт-в-байт как у TrueConf-эталона: UI-стор корректно превращает `null` в
/// `calibration=null`, виджет калибровки не монтируется, статистика не уводит
/// на страницу калибровки.
pub async fn engine_calibration(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
) -> Response {
    track("engine/calibration");
    if let Some(response) = super::settings_admin::forbid(&auth) {
        return response;
    }
    Json(json!({ "calibration": null })).into_response()
}

/// Реальный корневой диск для `/ui/system/status`: `free` в **KiB**
/// (в эталоне `disk.free` — KiB, не байты), `percent` — занятость.
fn disk_status() -> Value {
    let spaces = super::observability::compute_disk();
    let root = spaces.first();
    let total = root.and_then(|space| space["total"].as_u64()).unwrap_or(0);
    let used = root.and_then(|space| space["used"].as_u64()).unwrap_or(0);
    let free = root.and_then(|space| space["free"].as_u64()).unwrap_or(0);
    let percent = if total == 0 { 0 } else { used.saturating_mul(100) / total };
    json!({
        "free": free.saturating_mul(1024),
        "percent": percent,
        "full": if percent >= 95 { "CRITICAL" } else if percent >= 85 { "WARNING" } else { "NORMAL" },
        "videorecords": 0
    })
}

/// `GET /api/v3/engine/diagnostics` → `{diagnostics:{vsm:{limits:{performance:100},resources:{required:{encoders:0,decoders:0},available:{encoders:100,decoders:100}}},scheduler:{workers:4}}}`.
pub async fn engine_diagnostics(
    State(_state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
) -> Response {
    track("engine/diagnostics");
    if let Some(response) = super::settings_admin::forbid(&auth) {
        return response;
    }
    // UI-виджет «Загрузка вычислительных потоков» делает
    // `workersDump.map(t => t.occupancy + t.overhead)` и
    // `concat(scheduler.workers)`: число вместо массива даёт NaN и плоский
    // график. Отдаём массив записей, как эталон.
    let workers: Vec<Value> = (0..4).map(|_| json!({"occupancy": 0.0, "overhead": 0.0, "count": 1})).collect();
    Json(json!({
        "diagnostics": {
            "vsm": {
                "limits": { "performance": 100 },
                "resources": {
                    "required": { "encoders": 0, "decoders": 0 },
                    "available": { "encoders": 100, "decoders": 100 }
                }
            },
            "scheduler": { "workers": workers }
        }
    }))
    .into_response()
}

/// `GET /api/v3/tags_to_objects` — список тегов (у нас тегов нет).
pub async fn tags_to_objects_get() -> Response {
    track("tags_to_objects");
    Json(json!({"count":0,"tags_to_objects":[]})).into_response()
}

/// `POST /api/v3/tags_to_objects` — теги эталонного UI. Теги не влияют на
/// воспроизведение; принимаем и подтверждаем, чтобы не отдавать 501.
pub async fn tags_to_objects(Json(_body): Json<Value>) -> Response {
    track("tags_to_objects");
    Json(json!({"status":"OK"})).into_response()
}

/// `GET /api/v3/files` — файловый менеджер. Поиск идёт через
/// `ui:files:search`; HTTP-список нужен только для счётчика.
pub async fn files_list() -> Response {
    track("files");
    Json(json!({"count":0,"files":[]})).into_response()
}

/// `GET /api/v3/files/{id}` — файл не хранится этим стендом.
pub async fn file_get(axum::extract::Path(_id): axum::extract::Path<String>) -> Response {
    track("files/get");
    Json(json!({"error":{"code":404,"description":"File not found","details":[]}})).into_response()
}

/// `POST /api/v3/files` — multipart-загрузка. Полноценное файловое хранилище
/// вне задачи MCU; подтверждаем загрузку синтетической записью, чтобы UI не
/// показывал красный тост 501.
pub async fn file_upload(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    track("files/upload");
    let display_name = params.get("display_name").cloned().unwrap_or_else(|| "uploaded".to_string());
    Json(json!({"file":{"id":uuid::Uuid::new_v4().to_string(),"display_name":display_name,
        "protocol":"file","payload":{"extension":"bin","size":0,"primary":false,"loop":false,"zoom":false}}}))
    .into_response()
}

/// `DELETE /api/v3/files/{id}` — идемпотентно.
pub async fn file_delete(axum::extract::Path(_id): axum::extract::Path<String>) -> Response {
    track("files/delete");
    axum::http::StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/v3/registration/online` — активация лицензии. На стенде лицензия
/// постоянная; отвечаем текущим `/license`-совместимым конвертом.
pub async fn registration_online(Extension(auth): Extension<AuthContext>) -> Response {
    track("registration/online");
    if let Some(response) = super::settings_admin::forbid(&auth) {
        return response;
    }
    Json(license_payload()).into_response()
}

/// `POST /api/v3/registration/offline` — UI ожидает blob; отдаём пустой файл.
pub async fn registration_offline() -> Response {
    track("registration/offline");
    ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], Vec::<u8>::new()).into_response()
}

/// `POST /api/v3/registration/license` — загрузка файла лицензии; на стенде
/// лицензия уже постоянная, файл не нужен.
pub async fn registration_license() -> Response {
    track("registration/license");
    Json(json!({"status":"OK"})).into_response()
}

/// `GET /api/v3/users/credentials/expire` → `{users:[]}` — маппер `MW`
/// деструктурирует `users` (без него store-ре-throw кладёт консоль).
/// Пусто: парольных TTL у нас нет; при включении — фильтр по `block_at`.
pub async fn users_credentials_expire() -> Response {
    track("users/credentials/expire");
    Json(json!({ "users": [] })).into_response()
}

/// `GET /api/v3/settings/audio_codecs` → эталонный shape
/// `{audio_codecs:[{name,bitrate}]}` (bitrate null для кодеков без вариантов).
pub async fn settings_audio_codecs(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    track("settings/audio_codecs");
    let s = SettingsService::get(&state);
    let codecs: Vec<serde_json::Value> = s
        .codecs
        .audio
        .iter()
        .map(|spec| {
            let (name, bitrate) = crate::settings::CodecConfig::audio_parts(spec);
            json!({
                "name": name,
                "bitrate": bitrate,
                "supported": vks_common::codec_capabilities::is_supported_audio_variant(&name, bitrate)
            })
        })
        .collect();
    codec_response(&headers, json!({ "audio_codecs": codecs }))
}

/// `GET /api/v3/settings/video_codecs` → эталонный shape `{video_codecs:[str]}`.
pub async fn settings_video_codecs(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    track("settings/video_codecs");
    let s = SettingsService::get(&state);
    let supported_video: Vec<&String> =
        s.codecs.video.iter().filter(|name| vks_common::codec_capabilities::is_native_video(name)).collect();
    codec_response(&headers, json!({ "video_codecs": s.codecs.video, "supported_video_codecs": supported_video }))
}

/// Кодеки меняются только PATCH'ем, поэтому отдаём strong ETag и
/// `no-cache`: браузер ревалидирует, а не показывает устаревший список
/// до истечения max-age. Клиентский `/mcu/perf-overrides.js` при этом
/// схлопывает N+1 до одного запроса на документ.
fn codec_response(headers: &HeaderMap, body: Value) -> Response {
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let etag = format!("\"{:016x}\"", fnv1a64(&bytes));
    let cache = HeaderValue::from_static("private, no-cache");
    if headers.get_all(header::IF_NONE_MATCH).iter().filter_map(|value| value.to_str().ok()).any(|value| {
        value.split(',').any(|candidate| {
            let candidate = candidate.trim();
            candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag.as_str())
        })
    }) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().insert(header::ETAG, HeaderValue::from_str(&etag).unwrap_or(cache.clone()));
        response.headers_mut().insert(header::CACHE_CONTROL, cache);
        return response;
    }
    let mut response = Json(body).into_response();
    response.headers_mut().insert(header::ETAG, HeaderValue::from_str(&etag).unwrap_or(cache.clone()));
    response.headers_mut().insert(header::CACHE_CONTROL, cache);
    response
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Эталонные умолчания видео-настроек (`ref-92/settings_video.json`), сразу payload.
fn default_video_settings() -> Value {
    json!({"render_mode":"AUTO","text_timeout":20,"icons_enabled":true,"layout_strict":"ON",
        "max_frame_rate":30,"rotate_timeout":20000,"stream_control":false,"avatars_enabled":true,
        "packet_recovery":false,"adaptive_bitrate":false,"video_storage_size":25,
        "performance_control":"AUTO","video_quality_first":"DEFAULT","video_quality_second":"DEFAULT",
        "presentation_bandwidth":"AUTO"})
}

fn current_video_settings(state: &SharedState) -> Value {
    state
        .api_v3
        .video_settings
        .lock()
        .ok()
        .map(|guard| guard.clone())
        .filter(Value::is_object)
        .unwrap_or_else(default_video_settings)
}

/// `GET /api/v3/settings/video` → `{payload:{…}}` (редактор раскладок читает
/// `layout_strict`, `max_frame_rate`; без него страница падала с PAGEERR).
pub async fn settings_video(State(state): State<SharedState>) -> Response {
    track("settings/video");
    Json(json!({ "payload": current_video_settings(&state) })).into_response()
}

/// `PATCH /api/v3/settings/video` — частичное обновление, durable (в конфиг-документе).
pub async fn update_settings_video(
    State(state): State<SharedState>,
    Extension(auth::ClientIp(ip)): Extension<auth::ClientIp>,
    Extension(auth): Extension<crate::auth_middleware::AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    track("settings/video");
    if let Some(response) = super::settings_admin::forbid(&auth) {
        return response;
    }
    let patch = body.get("payload").cloned().unwrap_or_else(|| body.clone());
    let Some(patch) = patch.as_object() else {
        return super::error_json(422, "payload must be an object");
    };
    if patch.values().any(|value| value.is_array() || value.is_object()) {
        return super::error_json(422, "video settings accept scalar values only");
    }
    let mut payload = current_video_settings(&state);
    if let Some(target) = payload.as_object_mut() {
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
    }
    // Сначала durable-запись документа, только потом память: при ошибке состояние не меняется.
    // Лок resources удерживается до записи — снимок не должен разъехаться с параллельной мутацией.
    let resources = state.api_v3.resources.lock().await;
    if let Err(error) = super::resources::persist_with_video_settings(&state, &resources, payload.clone()).await {
        return super::error_json(500, &format!("Storage error: {error}"));
    }
    drop(resources);
    if let Ok(mut guard) = state.api_v3.video_settings.lock() {
        *guard = payload.clone();
    }
    state.record_audit_entry(&auth.subject, "settings.video.updated", None, &ip);
    Json(json!({ "payload": payload })).into_response()
}

/// UI-роль окна в словаре вендорского конструктора PNM (строго UPPERCASE:
/// `EMPTY`/`ACTIVE`/`AUTO`/`PEER`/`ROLL`/`PRESENTATION`). Редактор раскрашивает
/// окно и рисует иконку только по `slot.role`; без роли окно прозрачное.
/// Пустые окна шаблона — `AUTO` (autofill), наложенные PiP — `PRESENTATION`.
fn ui_slot_role(z_order: u8) -> &'static str {
    if z_order > 0 {
        "PRESENTATION"
    } else {
        "AUTO"
    }
}

/// `POST /api/v3/ui/video_layouts/calculate` — геометрия слотов для превью.
///
/// Два словаря, id не пересекаются:
/// * 65 вендорных manual-пресетов эталона TrueConf MCU 2.2.1.66
///   (`preset_equal_1`, `preset_selected_b_4`, `preset_double_b_10`, …);
/// * 54 раскладки Codian MCU 4200 (`codian_01`…`codian_52`,
///   `codian_custom_22`, `codian_custom_14`) — расширение, чтобы UI мог
///   построить превью наших схем тем же контрактом;
/// * параметрические раскладки конструктора (`vks_grid_9`, `vks_pin_r_8`) —
///   id полностью определяет геометрию, поэтому превью и применение совпадают.
///
/// dynamic/auto/базовые id → 400 `invalid.preset`. Позиции 1-based, у pip-окон
/// `z_order = 1`.
pub async fn video_layouts_calculate(Json(body): Json<serde_json::Value>) -> Response {
    track("ui/video_layouts/calculate");
    let preset_name = body.get("preset").and_then(serde_json::Value::as_str).unwrap_or_default();
    let codian = vks_layout::codian::is_codian_preset(preset_name);
    // Конструктор «Схема расположения участников»: id несёт число окон и режим,
    // геометрия вычисляется из id (без реестра и хранения координат).
    let grid = vks_layout::grids::is_grid_preset(preset_name);
    if !grid && !vks_layout::preset::is_known_preset(preset_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {
                "code": 400,
                "description": "Bad Request",
                "details": [{
                    "reason": "invalid.preset",
                    "description": format!(
                        "Must be one of: {} (или codian_01..codian_52, codian_custom_22, codian_custom_14; \
                         конструктор: vks_grid_<n>, vks_pin_{{l,r,t,b}}_<n> с опциями g<px>/m<px>, n = 1..100)",
                        vks_layout::preset::manual_preset_ids().collect::<Vec<_>>().join(", ")
                    ),
                    "position": "preset",
                }],
            }})),
        )
            .into_response();
    }
    let dimension = |key: &str, fallback: f64| {
        body.get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 1.0)
            .unwrap_or(fallback)
    };
    let width = dimension("width", 1280.0);
    let height = dimension("height", 720.0);
    let slots = if codian {
        vks_layout::codian::geometry(preset_name, width, height)
    } else if grid {
        vks_layout::grids::geometry(preset_name, width, height)
    } else {
        vks_layout::preset::geometry_for_manual(preset_name, width, height)
    };
    let Some(slots) = slots else {
        return super::error_json(400, "Bad Request");
    };
    let response: Vec<serde_json::Value> = slots
        .iter()
        .enumerate()
        .map(|(position, slot)| {
            json!({
                "position": position + 1,
                "role": ui_slot_role(slot.z_order),
                "x": slot.rect.x,
                "y": slot.rect.y,
                "width": slot.rect.width,
                "height": slot.rect.height,
                "z_order": slot.z_order,
            })
        })
        .collect();
    Json(json!({ "slots": response })).into_response()
}

/// `GET /api/v3/ui/video_layouts/presets` → справочник пресетов геометрии.
///
/// Два источника: 65 вендорных manual-пресетов эталона TrueConf 2.2.1.66 и
/// 54 раскладки Codian MCU 4200 (`codian_01`…`codian_52` + 2 кастомные).
/// Вендорный контракт `/api/v3/ui/video_layouts` не меняется — список
/// пресетов аддитивный и отдаётся отдельным маршрутом.
pub async fn video_layout_presets() -> Response {
    track("ui/video_layouts/presets");
    let mut presets = Vec::new();
    for id in vks_layout::preset::manual_preset_ids() {
        presets.push(json!({ "id": id, "source": "vendor" }));
    }
    for id in vks_layout::codian::codian_preset_ids() {
        let title = vks_layout::codian::scheme(id).map(|scheme| scheme.title).unwrap_or_default();
        let windows = vks_layout::codian::slot_count(id).unwrap_or_default();
        // Геометрия сразу в каталоге: проектный слой рисует превью карточки
        // без 54 отдельных запросов `calculate` (контракт calculate остаётся).
        let slots: Vec<serde_json::Value> = vks_layout::codian::geometry(id, 1280.0, 720.0)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(position, slot)| {
                json!({
                    "position": position + 1,
                    "role": ui_slot_role(slot.z_order),
                    "x": slot.rect.x,
                    "y": slot.rect.y,
                    "width": slot.rect.width,
                    "height": slot.rect.height,
                    "z_order": slot.z_order,
                })
            })
            .collect();
        presets.push(json!({ "id": id, "title": title, "windows": windows, "source": "codian", "slots": slots }));
    }
    Json(json!({ "presets": presets })).into_response()
}

/// `GET /api/v3/waitingroom` → `{}` (дополняется формой из бандла при реализации).
pub async fn waitingroom() -> Response {
    track("waitingroom");
    Json(json!({})).into_response()
}

/// `GET /api/v3/conferences/status` → `{"status":"enabled"}`.
pub async fn conferences_status() -> Response {
    track("conferences/status");
    Json(json!({ "status": "enabled" })).into_response()
}

/// `GET /api/v3/ui/video_layouts` / `GET /api/v3/video_layouts` → `{video_layouts:[...]}`.
pub async fn video_layouts(State(state): State<SharedState>) -> Response {
    track("video_layouts");
    let mut list = vec![
        json!({ "id": "grid", "title": "Grid", "description": "Автоматическая сетка участников", "is_default": true }),
        json!({ "id": "focus", "title": "Focus", "description": "Докладчик крупно + лента участников", "is_default": false }),
        json!({ "id": "speaker", "title": "Speaker", "description": "Один активный спикер", "is_default": false }),
    ];
    if let Ok(layouts) = state.layouts.lock() {
        for (id, l) in layouts.iter() {
            if id != "grid" && id != "focus" && id != "speaker" {
                let (is_default, desc) = if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&l.document) {
                    let def = doc.get("is_default").and_then(|v| v.as_bool()).unwrap_or(false);
                    let d = doc
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("Пользовательская раскладка {}", l.name));
                    (def, d)
                } else {
                    (false, format!("Пользовательская раскладка {}", l.name))
                };
                list.push(json!({
                    "id": id,
                    "title": l.name,
                    "description": desc,
                    "is_default": is_default,
                }));
            }
        }
    }
    Json(json!({ "video_layouts": list })).into_response()
}

/// Тело запроса переключения режима раскладки / топологии (`POST /api/v3/ui/video_layouts/mode`).
#[derive(Debug, Default, serde::Deserialize)]
pub struct SwitchVideoLayoutModeRequest {
    pub session_id: Option<String>,
    #[serde(alias = "conference_id")]
    pub conference_id: Option<String>,
    #[serde(alias = "new_mode", alias = "mode")]
    pub mode: Option<String>,
    pub video_layout_id: Option<String>,
    /// `preset`/`type`/`size`/`slots` — остальные поля формы; UI читает их из ответа.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// `POST /api/v3/ui/video_layouts/mode` и `/api/v3/video_layouts/mode` — переключение MCU/SFU из UI.
/// `Boe` шлёт `{session_id, video_layout_id, new_mode, display_name, payload:{mode,preset,type,size,slots}}`.
pub async fn switch_video_layout_mode(
    State(state): State<SharedState>,
    Extension(auth): Extension<crate::auth_middleware::AuthContext>,
    Json(body): Json<SwitchVideoLayoutModeRequest>,
) -> Response {
    track("ui/video_layouts/mode");
    if auth.role < vks_core::Role::Moderator {
        return super::error_json(403, "Forbidden: Moderator role required");
    }
    let room = match body.session_id.or(body.conference_id).as_deref() {
        Some(id) => match super::sessions::find_room(&state, id) {
            Some(room) => room,
            None => return super::error_json(404, "Session not found"),
        },
        None => return super::error_json(422, "session_id is required"),
    };
    if let Err(response) = super::sessions::check_permission(&auth, Some(&room.id.to_string())) {
        return response;
    }

    let mode = body.mode.clone().unwrap_or_else(|| "auto".to_string());
    let mode_lc = mode.to_lowercase();
    let topology_mode = match mode_lc.as_str() {
        "mcu" => Some(vks_core::RoomMode::Mcu),
        "sfu" => Some(vks_core::RoomMode::Sfu),
        _ => None,
    };

    // Поля формы UI кладёт во вложенный `payload`; верхний уровень — для совместимости.
    let nested = body.extra.get("payload").and_then(Value::as_object).cloned().unwrap_or_default();
    let field = |key: &str| nested.get(key).cloned().or_else(|| body.extra.get(key).cloned());
    let size = field("size")
        .and_then(|value| value.as_u64())
        .filter(|size| (1..=vks_layout::grids::MAX_WINDOWS as u64).contains(size));
    let slots = match field("slots") {
        Some(Value::Array(slots)) if slots.iter().all(Value::is_object) => {
            let len = slots.len() as u64;
            Some((Value::Array(slots), len))
        }
        Some(_) => return super::error_json(422, "payload.slots items must be objects"),
        None => None,
    };
    // Раскладки Codian и конструктора принимаются по тому же пути, что
    // вендорские manual-пресеты: число окон у Codian задаёт таблица макета
    // (`codian::slot_count`), у конструктора — параметрический id
    // (`grids::parse`), а не хвост `preset_size`.
    let preset_field = field("preset").filter(|preset| {
        preset.as_str().is_some_and(|p| {
            super::resources::preset_size(p).is_some()
                || vks_layout::codian::is_codian_preset(p)
                || vks_layout::grids::is_grid_preset(p)
        })
    });
    let inferred_size = size
        .or_else(|| slots.as_ref().map(|(_, len)| *len))
        .or_else(|| {
            preset_field.as_ref().and_then(Value::as_str).and_then(vks_layout::codian::slot_count).map(|n| n as u64)
        })
        .or_else(|| {
            preset_field
                .as_ref()
                .and_then(Value::as_str)
                .and_then(vks_layout::grids::parse)
                .map(|scheme| scheme.count as u64)
        })
        .unwrap_or(13);
    // `payload.mode` — режим раскладки (auto/dynamic/manual); mcu/sfu — топология комнаты.
    // `Boe` кладёт новый режим в `new_mode`, а в nested `payload.mode` — старое значение.
    // Приоритет: new_mode (auto/dynamic/manual) → nested payload.mode → auto.
    if let Some(value) = field("mode") {
        if value.as_str().is_none_or(|value| !matches!(value, "auto" | "dynamic" | "manual")) {
            return super::error_json(422, "payload.mode must be auto, dynamic or manual");
        }
    }
    let layout_mode = match mode_lc.as_str() {
        value @ ("auto" | "dynamic" | "manual") => value.to_string(),
        _ => field("mode").and_then(|value| value.as_str().map(str::to_string)).unwrap_or_else(|| "auto".to_string()),
    };
    let mut patch = serde_json::Map::new();
    patch.insert("mode".to_string(), json!(layout_mode));
    // T7: mode-only запрос не должен затирать топологию. `Boe` шлёт preset/type
    // при смене схемы; если их нет — сохраняем прежние значения из payload.
    // `type` не переносим: он immutable (resources::update_resource) — иначе
    // «Общая» через mode-эндпоинт снова становилась individual.
    let preset_present = preset_field.is_some();
    if let Some(preset) = preset_field {
        patch.insert("preset".to_string(), preset);
    }
    if size.is_some() || slots.is_some() || preset_present {
        patch.insert("size".to_string(), json!(inferred_size));
    }
    if let Some((slots, _)) = slots {
        patch.insert("slots".to_string(), slots);
    }
    if let Some(autoscroll) = field("autoscroll") {
        patch.insert("autoscroll".to_string(), autoscroll);
    }

    let mut resources = state.api_v3.resources.lock().await;
    let Some(layout_id) = body.video_layout_id.clone() else {
        // UI всегда передаёт id; молчаливый fallback маскировал бы ошибку клиента.
        return super::error_json(422, "video_layout_id is required");
    };
    let scope_key = format!("sessions:{}:video_layouts", room.id);
    let layout_id = super::resources::resolve_layout_alias(&resources, &scope_key, &layout_id);
    if !super::resources::layout_visible(&resources, &room.id.to_string(), &layout_id) {
        return super::error_json(404, "Layout is not attached to this session");
    }
    let Some(source) = resources.layouts.get(&layout_id).cloned() else {
        return super::error_json(404, "Layout not found");
    };
    if !source["payload"].is_object() {
        return super::error_json(422, "Layout payload is malformed");
    }
    let previous = resources.clone();
    let mut draft = source;
    if let Some(payload) = draft["payload"].as_object_mut() {
        for (key, value) in &patch {
            payload.insert(key.clone(), value.clone());
        }
    }
    let owners = resources.links.values().filter(|ids| ids.iter().any(|value| value == &layout_id)).count();
    let direct = resources.links.get(&scope_key).is_some_and(|ids| ids.iter().any(|value| value == &layout_id));
    let target_id = if direct && owners <= 1 {
        resources.layouts.insert(layout_id.clone(), draft.clone());
        layout_id.clone()
    } else {
        super::resources::cow_replace(&mut resources, &scope_key, &layout_id, draft.clone())
    };
    // Переключение режима из редактора — это «Применить»: layout становится active.
    super::resources::set_active(&mut resources, &scope_key, &target_id);
    let mut result = resources.layouts.get(&target_id).cloned().unwrap_or(draft);
    if let Err(response) = super::resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    if let Some(new_mode) = topology_mode {
        let _ = crate::rooms::Rooms::update_async(&state, &room.id, None, Some(new_mode), None, None).await;
    }
    // Применённая из редактора раскладка должна сразу доехать до MCU, иначе
    // выбранная схема и позиции участников остаются только в UI.
    let applied = super::sessions::push_layout_to_engine(&state, room.id).await;
    super::resources::with_layout_abonents(&state, &mut result, &room.id);
    // `Boe` делает `.then(o => my(o.data))`: ответ — запись раскладки, не конверт.
    super::sessions::with_layout_apply(Json(result).into_response(), applied)
}

/// `POST /api/v3/engine/command` — команды движка.
pub async fn engine_command(
    Extension(auth): Extension<crate::auth_middleware::AuthContext>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    track("engine/command");
    if auth.role < vks_core::Role::Moderator {
        return super::error_json(403, "Forbidden: Moderator role required");
    }
    // Калибровки нет (T1.1): не подтверждаем успех команды, которой не существует.
    let calibration = body
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|c| c.to_ascii_lowercase().contains("calibrat"));
    if calibration {
        return super::error_json(501, "Calibration is not supported");
    }
    Json(json!({ "status": "ok", "result": "ok" })).into_response()
}

#[cfg(test)]
mod tests {
    use super::role_to_permissions;
    use vks_core::Role;

    /// Контракт прав профиля (T1.1).
    ///
    /// Operator обязан сохранить wildcard "all": им бандл 2.2.1.66 гейтит
    /// «Diagnostics» и «Date and time» — явный список прав их скрыл бы.
    /// Ни одна роль не должна получать `calibration:*` литерально (UI прячет
    /// остатки калибровки через `index.html`, deep-link — серверный редирект).
    #[test]
    fn role_permissions_contract() {
        assert_eq!(role_to_permissions(Role::Operator), &["all"]);
        for role in [Role::Operator, Role::Moderator, Role::Participant, Role::Viewer] {
            assert!(
                !role_to_permissions(role).iter().any(|p| p.starts_with("calibration:")),
                "{role:?}: calibration:* retired"
            );
        }
    }
}
