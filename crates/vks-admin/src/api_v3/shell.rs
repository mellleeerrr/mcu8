//! Boot/shell-эндпоинты (§4.5 плана) за token-middleware: profile, справочники,
//! статус системы, лицензия, настройки, кодеки.
//!
//! Мапперы UI мягкие (`snake_case → camelCase`, отсутствующие поля отбрасываются),
//! поэтому конверты минимальны, но ключевые для навигации поля обязательны:
//! `ui/system/status.engine.description` (guard: `INVALID_LICENSE`/`FAILED` → экраны-заглушки).

use axum::{
    extract::{Extension, State},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use vks_auth::jwt::VerifiedJwt;

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

    // Роль — только из admin-хранилища (Users); неизвестному субъекту — Viewer
    // (least privilege). Единая точка резолвинга id/username — `Users::resolve_role`.
    let role = crate::users::Users::resolve_role(&state, &verified.user_id).unwrap_or(vks_core::Role::Viewer);

    let permissions = role_to_permissions(role);

    Json(json!({
        "user": {
            "id": verified.user_id,
            "display_name": display_name,
            "role": role.as_str(),
            "permissions": permissions,
            "locale_id": "ru_RU",
            "time_zone_id": system_timezone(),
            "groups": [],
            "emails": [],
            "phones": [],
            "avatars": [],
            "description": "",
            "creator_id": "",
            "created_at": 0,
            "busy": false,
            "defaults": { "deletable": false, "editable": true }
        }
    }))
    .into_response()
}

/// `GET /api/v3/time-zones` → `{time_zones:[{id}]}` (маппер `LW`).
pub async fn time_zones() -> Response {
    track("time-zones");
    Json(json!({ "time_zones": [{ "id": "UTC" }] })).into_response()
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

    let sip_healthy = !state.has_fault("sip") && !state.has_fault("gateway") && readiness.gateway;
    let h323_healthy = !state.has_fault("h323") && !state.has_fault("gateway") && readiness.gateway;
    let cluster_healthy = readiness.cluster;

    let sip_status = if sip_healthy { "READY" } else { "FAILED" };
    let h323_status = if h323_healthy { "READY" } else { "FAILED" };
    let cluster_status = if cluster_healthy { "READY" } else { "FAILED" };

    Json(json!({
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
            "size_bytes": 0
        },
        "network": { "status": "READY", "connected": true, "editable": true, "ip": settings.ip },
        "disk": { "free": 107374182400u64, "percent": 15, "full": "NORMAL" },
        "sip": {
            "status": sip_status,
            "connected": sip_healthy,
            "registration": sip_healthy,
            "account": if sip_healthy { "sip:vks@local" } else { "" }
        },
        "h323": {
            "status": h323_status,
            "connected": h323_healthy,
            "registration": h323_healthy,
            "account": if h323_healthy { "h323:vks@local" } else { "" }
        },
        "cluster": {
            "status": cluster_status,
            "connected": cluster_healthy,
            "lease_valid": cluster_healthy
        }
    }))
    .into_response()
}

/// `GET /api/v3/license` → `{state:"VALID",type:"PERMANENT",…}` (маппер `kk`).
pub async fn license() -> Response {
    track("license");
    Json(json!({
        "state": "VALID",
        "type": "PERMANENT",
        "count": 100,
        "serial_id": "",
        "demo": false,
        "first_expired": 0,
        "last_expired": 0,
        "params": {
            "protocol": { "max_calls": 100 },
            "video": { "max_active": 100 },
            "server": { "max_conferences": 50 }
        }
    }))
    .into_response()
}

/// `GET /api/v3/features` → `{features:{}}`.
pub async fn features() -> Response {
    track("features");
    Json(json!({ "features": {} })).into_response()
}

/// `GET /api/v3/engine/calibration` → `{"calibration":null}`.
///
/// Калибровка в нашей MCU не поддерживается (и убрана из UX, T1.1). Форма ответа —
/// байт-в-байт как у TrueConf-эталона: UI-стор корректно превращает `null` в
/// `calibration=null`, виджет калибровки не монтируется, статистика не уводит
/// на страницу калибровки.
pub async fn engine_calibration() -> Response {
    track("engine/calibration");
    Json(json!({ "calibration": null })).into_response()
}

/// `GET /api/v3/engine/diagnostics` → `{diagnostics:{vsm:{limits:{performance:100},resources:{required:{encoders:0,decoders:0},available:{encoders:100,decoders:100}}},scheduler:{workers:4}}}`.
pub async fn engine_diagnostics() -> Response {
    track("engine/diagnostics");
    Json(json!({
        "diagnostics": {
            "vsm": {
                "limits": { "performance": 100 },
                "resources": {
                    "required": {
                        "encoders": 0,
                        "decoders": 0
                    },
                    "available": {
                        "encoders": 100,
                        "decoders": 100
                    }
                }
            },
            "scheduler": { "workers": 4 }
        }
    }))
    .into_response()
}

/// `GET /api/v3/system/time` → `{time,timezone,ntp,ntp_name,ntp_synchronized}`.
pub async fn system_time() -> Response {
    track("system/time");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Vendor-UI форматирует время списка по `time.timezone`; «UTC» при
    // локальном браузере давал расхождение со страницей детали.
    let timezone = system_timezone();
    let time_obj = json!({
        "time": now,
        "timezone": timezone,
        "ntp": false,
        "ntp_name": "",
        "ntp_synchronized": false
    });
    Json(json!({
        "time": time_obj,
        "timezone": timezone,
        "ntp": false,
        "ntp_name": "",
        "ntp_synchronized": false
    }))
    .into_response()
}

/// IANA-таймзона хоста: `TZ` → `/etc/timezone` → UTC.
fn system_timezone() -> String {
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

/// `GET /api/v3/settings/calls` → кодеки/лимиты/TURN из наших `Settings` (§4.5).
pub async fn settings_calls(State(state): State<SharedState>) -> Response {
    track("settings/calls");
    let s = SettingsService::get(&state);
    let payload = json!({
        "codecs": { "audio": s.codecs.audio, "video": s.codecs.video },
        "limits": {
            "max_participants_per_room": s.limits.max_participants_per_room,
            "max_tracks_per_room": s.limits.max_tracks_per_room,
            "max_tracks_per_participant": s.limits.max_tracks_per_participant,
            "api_key_rate_limit_rpm": s.limits.api_key_rate_limit_rpm
        },
        "turn": { "uri": s.turn.uri, "credential_ttl_sec": s.turn.credential_ttl_sec },
        "fecc": false,
        "aspeaker": true,
        "mcu_domain": "",
        "mcu_display_name": s.name,
        "protocol_preferences": "SIP",
        "supported_protocols": ["webrtc", "sip", "h323"],
        "enabled_protocol":"ALL",
        "ui_autocall_on_session_start":"ON",
        "ui_autocall_scope":"all",
        "stop_rules": {}
    });
    Json(json!({
        "payload": payload,
        "codecs": { "audio": s.codecs.audio, "video": s.codecs.video },
        "limits": {
            "max_participants_per_room": s.limits.max_participants_per_room,
            "max_tracks_per_room": s.limits.max_tracks_per_room,
            "max_tracks_per_participant": s.limits.max_tracks_per_participant,
            "api_key_rate_limit_rpm": s.limits.api_key_rate_limit_rpm
        },
        "turn": { "uri": s.turn.uri, "credential_ttl_sec": s.turn.credential_ttl_sec }
    }))
    .into_response()
}

/// `PATCH /api/v3/settings/calls` — обновление параметров вызовов.
pub async fn update_settings_calls(
    State(state): State<SharedState>,
    Extension(auth::ClientIp(ip)): Extension<auth::ClientIp>,
    Extension(auth): Extension<crate::auth_middleware::AuthContext>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    track("settings/calls");
    if auth.role < vks_core::Role::Operator {
        return super::error_json(403, "Forbidden: Operator role required");
    }
    let source = body.get("payload").unwrap_or(&body);
    let mut current = SettingsService::get(&state);
    if let Some(codecs) = source.get("codecs") {
        if let Ok(c) = serde_json::from_value::<crate::settings::CodecConfig>(codecs.clone()) {
            current.codecs = c;
        }
    }
    if let Some(limits) = source.get("limits") {
        if let Ok(l) = serde_json::from_value::<crate::settings::LimitsConfig>(limits.clone()) {
            current.limits = l;
        }
    }
    if let Some(turn) = source.get("turn") {
        if let Ok(t) = serde_json::from_value::<crate::settings::TurnConfig>(turn.clone()) {
            current.turn = t;
        }
    }
    let updated = SettingsService::set_async(&state, current).await;
    state.record_audit_entry(&auth.subject, "settings.calls.updated", None, &ip);

    let payload = json!({
        "codecs": { "audio": updated.codecs.audio, "video": updated.codecs.video },
        "limits": {
            "max_participants_per_room": updated.limits.max_participants_per_room,
            "max_tracks_per_room": updated.limits.max_tracks_per_room,
            "max_tracks_per_participant": updated.limits.max_tracks_per_participant,
            "api_key_rate_limit_rpm": updated.limits.api_key_rate_limit_rpm
        },
        "turn": { "uri": updated.turn.uri, "credential_ttl_sec": updated.turn.credential_ttl_sec },
        "fecc": false,
        "aspeaker": true,
        "mcu_domain": "",
        "mcu_display_name": updated.name,
        "protocol_preferences": "SIP",
        "supported_protocols": ["webrtc", "sip", "h323"],
        "enabled_protocol":"ALL",
        "ui_autocall_on_session_start":"ON",
        "ui_autocall_scope":"all",
        "stop_rules": {}
    });
    Json(json!({
        "payload": payload,
        "codecs": { "audio": updated.codecs.audio, "video": updated.codecs.video },
        "limits": {
            "max_participants_per_room": updated.limits.max_participants_per_room,
            "max_tracks_per_room": updated.limits.max_tracks_per_room,
            "max_tracks_per_participant": updated.limits.max_tracks_per_participant,
            "api_key_rate_limit_rpm": updated.limits.api_key_rate_limit_rpm
        },
        "turn": { "uri": updated.turn.uri, "credential_ttl_sec": updated.turn.credential_ttl_sec }
    }))
    .into_response()
}

/// `GET /api/v3/settings/security` → `{payload:{…}}` — форма экрана
/// «Безопасность» (маппер `Ane` читает `data.payload`; без вложенного
/// `payload` boot-цепочка падает на `settings.password` → TypeError в консоли).
///
/// Значения честные: парольная политика не enforced (`change_password`
/// отклоняет только пустой пароль → минимум 1 символ, остальные требования 0);
/// TTL паролей выключен; lockout логина — реальные константы `auth`
/// (5 неудач / 900 с блок, §1.4); блокировок конференций нет (`conf_*`).
/// `PATCH /api/v3/settings/security` — долг (keepalive настроек, §4.5).
pub async fn settings_security() -> Response {
    track("settings/security");
    Json(json!({
        "payload": {
            "password_min_length": 1,
            "password_min_lowercase": 0,
            "password_min_uppercase": 0,
            "password_min_digits": 0,
            "password_min_special_characters": 0,
            "password_no_login_match": false,
            "password_ttl": 0,
            "password_ttl_enabled": false,
            "login_max_attempts_count": auth::USER_IP_MAX_FAILS,
            "login_block_timeout": auth::BLOCK_SECS,
            "login_block_enabled": true,
            "conf_max_attempts_count": 0,
            "conf_block_timeout": 0,
            "conf_block_enabled": false
        }
    }))
    .into_response()
}

/// `GET /api/v3/users/credentials/expire` → `{users:[]}` — маппер `MW`
/// деструктурирует `users` (без него store-ре-throw кладёт консоль).
/// Пусто: парольных TTL у нас нет; при включении — фильтр по `block_at`.
pub async fn users_credentials_expire() -> Response {
    track("users/credentials/expire");
    Json(json!({ "users": [] })).into_response()
}

/// `GET /api/v3/settings/audio_codecs` → `{audio_codecs:[{id, description}]}`.
pub async fn settings_audio_codecs(State(state): State<SharedState>) -> Response {
    track("settings/audio_codecs");
    let s = SettingsService::get(&state);
    let codecs: Vec<serde_json::Value> = s.codecs.audio.iter().map(|c| json!({ "id": c, "description": c })).collect();
    Json(json!({ "audio_codecs": codecs })).into_response()
}

/// `GET /api/v3/settings/video_codecs` → `{video_codecs:[{id, description}]}`.
pub async fn settings_video_codecs(State(state): State<SharedState>) -> Response {
    track("settings/video_codecs");
    let s = SettingsService::get(&state);
    let codecs: Vec<serde_json::Value> = s.codecs.video.iter().map(|c| json!({ "id": c, "description": c })).collect();
    Json(json!({ "video_codecs": codecs })).into_response()
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
    if auth.role < vks_core::Role::Operator {
        return super::error_json(403, "Forbidden: Operator role required");
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

/// `POST /api/v3/ui/video_layouts/calculate` — геометрия слотов для превью
/// (`{preset,width,height}` → `{slots:[{position,x,y,width,height,z_order}]}`).
pub async fn video_layouts_calculate(Json(body): Json<serde_json::Value>) -> Response {
    track("ui/video_layouts/calculate");
    let preset = body.get("preset").and_then(serde_json::Value::as_str).unwrap_or("preset_default_13");
    let width = body.get("width").and_then(serde_json::Value::as_f64).filter(|v| *v > 0.0).unwrap_or(1280.0);
    let height = body.get("height").and_then(serde_json::Value::as_f64).filter(|v| *v > 0.0).unwrap_or(720.0);
    let count = crate::api_v3::resources::preset_size(preset)
        .map(|n| n as usize)
        .filter(|n| (1..=64).contains(n))
        .unwrap_or(13);
    let cols = (count as f64).sqrt().ceil() as usize;
    let rows = count.div_ceil(cols);
    let slots: Vec<serde_json::Value> = (0..count)
        .map(|index| {
            json!({
                "position": index,
                "x": (index % cols) as f64 * (width / cols as f64),
                "y": (index / cols) as f64 * (height / rows as f64),
                "width": width / cols as f64,
                "height": height / rows as f64,
                "z_order": index,
            })
        })
        .collect();
    Json(json!({ "slots": slots })).into_response()
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
    let size = field("size").and_then(|value| value.as_u64()).filter(|size| (1..=64).contains(size));
    let slots = match field("slots") {
        Some(Value::Array(slots)) if slots.iter().all(Value::is_object) => {
            let len = slots.len() as u64;
            Some((Value::Array(slots), len))
        }
        Some(_) => return super::error_json(422, "payload.slots items must be objects"),
        None => None,
    };
    let inferred_size = size.or_else(|| slots.as_ref().map(|(_, len)| *len)).unwrap_or(13);
    let type_default = match mode_lc.as_str() {
        "dynamic" | "manual" => "individual",
        _ => "default",
    };
    let preset_default = match mode_lc.as_str() {
        "dynamic" | "manual" => format!("preset_equal_{inferred_size}"),
        _ => "preset_default_13".to_string(),
    };
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
    patch.insert(
        "preset".to_string(),
        field("preset")
            .filter(|preset| preset.as_str().is_some_and(|p| super::resources::preset_size(p).is_some()))
            .unwrap_or_else(|| json!(preset_default)),
    );
    patch.insert("type".to_string(), field("type").unwrap_or_else(|| json!(type_default)));
    patch.insert("size".to_string(), json!(inferred_size));
    if let Some((slots, _)) = slots {
        patch.insert("slots".to_string(), slots);
    }
    if let Some(autoscroll) = field("autoscroll") {
        patch.insert("autoscroll".to_string(), autoscroll);
    }

    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    let Some(layout_id) = body.video_layout_id.clone() else {
        // UI всегда передаёт id; молчаливый fallback маскировал бы ошибку клиента.
        return super::error_json(422, "video_layout_id is required");
    };
    let attached = ["sessions", "conferences"].iter().any(|scope| {
        resources.links.get(&format!("{scope}:{}:video_layouts", room.id)).is_some_and(|ids| ids.contains(&layout_id))
    });
    if !attached {
        return super::error_json(404, "Layout is not attached to this session");
    }
    let Some(record) = resources.layouts.get_mut(&layout_id) else {
        return super::error_json(404, "Layout not found");
    };
    if !record["payload"].is_object() {
        return super::error_json(422, "Layout payload is malformed");
    }
    if let Some(payload) = record["payload"].as_object_mut() {
        for (key, value) in &patch {
            payload.insert(key.clone(), value.clone());
        }
    }
    let mut result = record.clone();
    if let Err(response) = super::resources::commit(&state, &mut resources, previous).await {
        return response;
    }
    drop(resources);
    if let Some(new_mode) = topology_mode {
        let _ = crate::rooms::Rooms::update_async(&state, &room.id, None, Some(new_mode), None, None).await;
    }
    super::resources::with_layout_abonents(&state, &mut result, &room.id);
    // `Boe` делает `.then(o => my(o.data))`: ответ — запись раскладки, не конверт.
    Json(result).into_response()
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

/// `GET /api/v3/ui/records` & `GET /api/v3/records` (M5.5).
pub async fn ui_records(State(state): State<SharedState>) -> Response {
    track("ui/records");
    let records: Vec<_> = if let Some(ref storage) = state.storage {
        if let Ok(recs) = storage.list_records(None).await {
            recs
        } else {
            state.records.lock().unwrap().values().cloned().collect()
        }
    } else {
        state.records.lock().unwrap().values().cloned().collect()
    };
    Json(json!({
        "count": records.len(),
        "records": records,
    }))
    .into_response()
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
