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

// TODO(T3): ключи не-Operator ролей — провизорные (источник истины — гейт UI
// `getIsProfileHasPermissions`); синхронизировать при появлении неоператорских
// логинов. Operator отдаёт wildcard "all".
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
            "calibration:start",
            "calibration:stop",
        ],
        vks_core::Role::Participant => &["sessions:view", "conferences:view"],
        vks_core::Role::Viewer => &["sessions:view"],
    }
}

/// `GET /api/v3/profile` → `{user:{…}}` (маппер `RW`; permissions зависят от роли).
pub async fn profile(State(state): State<SharedState>, Extension(verified): Extension<VerifiedJwt>) -> Response {
    track("profile");
    let username_from_db = state.local_db.as_ref().and_then(|db| db.user(&verified.user_id).ok()).map(|u| u.username);

    let display_name = username_from_db.clone().unwrap_or_else(|| verified.user_id.clone());

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
    let rooms = state.rooms.lock().expect("rooms store poisoned");
    let total_participants = rooms.values().map(|r| r.participants).sum::<usize>();
    let total_sessions = rooms.len();
    Json(json!({
        "count": count,
        "connections": {
            "total": total_participants,
            "webrtc": total_participants,
            "sip": 0,
            "h323": 0,
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

/// `GET /api/v3/engine/calibration` → `{calibration:{state:"IDLE",progress:0}}`.
pub async fn engine_calibration() -> Response {
    track("engine/calibration");
    Json(json!({
        "calibration": {
            "state": "IDLE",
            "progress": 0
        }
    }))
    .into_response()
}

/// `GET /api/v3/engine/diagnostics` → `{diagnostics:{vsm:{limits:{performance:100}},scheduler:{workers:4}}}`.
pub async fn engine_diagnostics() -> Response {
    track("engine/diagnostics");
    Json(json!({
        "diagnostics": {
            "vsm": { "limits": { "performance": 100 } },
            "scheduler": { "workers": 4 }
        }
    }))
    .into_response()
}

/// `GET /api/v3/system/time` → `{time,timezone,ntp,ntp_name,ntp_synchronized}`.
pub async fn system_time() -> Response {
    track("system/time");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let time_obj = json!({
        "time": now,
        "timezone": "UTC",
        "ntp": false,
        "ntp_name": "",
        "ntp_synchronized": false
    });
    Json(json!({
        "time": time_obj,
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
        "protocol_preferences": ["webrtc", "sip", "h323"],
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
        "protocol_preferences": ["webrtc", "sip", "h323"],
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
}

/// `POST /api/v3/ui/video_layouts/mode` и `/api/v3/video_layouts/mode` — переключение MCU/SFU из UI.
pub async fn switch_video_layout_mode(
    State(state): State<SharedState>,
    Extension(auth): Extension<crate::auth_middleware::AuthContext>,
    Json(body): Json<SwitchVideoLayoutModeRequest>,
) -> Response {
    track("ui/video_layouts/mode");
    if auth.role < vks_core::Role::Moderator {
        return super::error_json(403, "Forbidden: Moderator role required");
    }
    let room_id_str = body.session_id.or(body.conference_id);
    if let (Some(ref user_room), Some(ref req_room)) = (&auth.room_scope, &room_id_str) {
        if !req_room.eq_ignore_ascii_case(user_room) {
            return super::error_json(403, "Forbidden: room_scope mismatch");
        }
    }
    let mode_str = body.mode.unwrap_or_else(|| "sfu".to_string());
    let new_mode = match mode_str.to_lowercase().as_str() {
        "mcu" => vks_core::RoomMode::Mcu,
        _ => vks_core::RoomMode::Sfu,
    };

    if let Some(ref rid_str) = room_id_str {
        if let Ok(u) = uuid::Uuid::parse_str(rid_str) {
            let rid = vks_core::RoomId::from_uuid(u);
            let _ = crate::rooms::Rooms::update_async(&state, &rid, None, Some(new_mode), None, None).await;
        }
    }

    let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    Json(json!({
        "id": body.video_layout_id.as_deref().unwrap_or("grid"),
        "display_name": "Default",
        "creator_id": "admin",
        "created_at": now_secs,
        "payload": {
            "mode": mode_str,
            "preset": "auto",
            "type": "grid"
        }
    }))
    .into_response()
}

/// `POST /api/v3/engine/command` — команды движка.
pub async fn engine_command(
    Extension(auth): Extension<crate::auth_middleware::AuthContext>,
    Json(_body): Json<serde_json::Value>,
) -> Response {
    track("engine/command");
    if auth.role < vks_core::Role::Moderator {
        return super::error_json(403, "Forbidden: Moderator role required");
    }
    Json(json!({ "status": "ok", "result": "ok" })).into_response()
}
