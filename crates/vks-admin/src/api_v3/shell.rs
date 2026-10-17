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
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

use vks_auth::jwt::VerifiedJwt;

use super::{auth, track};
use crate::settings::SettingsService;
use crate::state::SharedState;

/// `GET /api/v3/profile` → `{user:{…}}` (маппер `RW`; `permissions:["all"]`).
pub async fn profile(State(state): State<SharedState>, Extension(verified): Extension<VerifiedJwt>) -> Response {
    track("profile");
    let display_name = state
        .local_db
        .as_ref()
        .and_then(|db| db.user(&verified.user_id).ok())
        .map(|u| u.username)
        .unwrap_or_else(|| verified.user_id.clone());
    Json(json!({
        "user": {
            "id": verified.user_id,
            "display_name": display_name,
            "permissions": ["all"],
            "locale_id": "ru_RU",
            "time_zone_id": "UTC",
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
    Json(json!({ "count": count })).into_response()
}

/// `GET /api/v3/ui/system/status` → форма маппера `DW`
/// (`engine/database/network/disk/sip/h323`; навигация-гард деструктурирует `status`).
pub async fn ui_system_status(State(state): State<SharedState>) -> Response {
    track("ui/system/status");
    let settings = SettingsService::get(&state);
    Json(json!({
        "engine": {
            "status": "READY",
            "description": "",
            "version": env!("CARGO_PKG_VERSION"),
            "type": "mcu",
            "name": settings.name
        },
        "database": { "status": "READY", "size_bytes": 0 },
        "network": { "status": "READY", "ip": settings.ip },
        "disk": { "free": 0, "percent": 0, "full": "NORMAL" },
        "sip": { "status": "DISABLED" },
        "h323": { "status": "DISABLED" }
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
        "last_expired": 0
    }))
    .into_response()
}

/// `GET /api/v3/features` → `{features:{}}`.
pub async fn features() -> Response {
    track("features");
    Json(json!({ "features": {} })).into_response()
}

/// `GET /api/v3/engine/calibration` → `{}` (маппер мягкий).
pub async fn engine_calibration() -> Response {
    track("engine/calibration");
    Json(json!({})).into_response()
}

/// `GET /api/v3/engine/diagnostics` → `{}` (маппер мягкий).
pub async fn engine_diagnostics() -> Response {
    track("engine/diagnostics");
    Json(json!({})).into_response()
}

/// `GET /api/v3/system/time` → `{time,timezone,ntp,ntp_name,ntp_synchronized}`.
pub async fn system_time() -> Response {
    track("system/time");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Json(json!({
        "time": now,
        "timezone": "UTC",
        "ntp": false,
        "ntp_name": "",
        "ntp_synchronized": false
    }))
    .into_response()
}

/// `GET /api/v3/settings/calls` → кодеки/лимиты/TURN из наших `Settings` (§4.5).
pub async fn settings_calls(State(state): State<SharedState>) -> Response {
    track("settings/calls");
    let s = SettingsService::get(&state);
    Json(json!({
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
