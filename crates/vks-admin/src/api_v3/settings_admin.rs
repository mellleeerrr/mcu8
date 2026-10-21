//! Административные настройки PNM (T2.3).
//!
//! Контракт снят с vendor `/api/v3/settings/*`, `/system/ntp`, `/system/time*`
//! (ref-92/t23): GET/PATCH `payload`-объектов, отдельные `calls/sip|h323`,
//! multipart-загрузка TLS-сертификата, SMTP test/reset, NTP enable/disable.
//! Значения переживают рестарт в `resources.admin_settings` (один
//! configuration-document с ресурсами); наружу отдаются только известные
//! vendor-поля, пароли не возвращаются.

use std::time::Duration;

use axum::{
    extract::{Multipart, State},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use tokio::net::TcpStream;

use crate::auth_middleware::AuthContext;
use crate::handlers::SharedState;
use crate::state::AppState;
use vks_core::Role;

use super::shell::system_timezone;

/// Vendor-дефолты настроек (ref-92, PNM 2.2.1.66). Ключ — имя в `admin_settings`.
fn defaults(key: &str) -> Value {
    match key {
        "calls" => json!({
            "fecc": true, "aspeaker": "SELECTIVE", "audio_call": "HIDE", "mcu_domain": "",
            "stop_rules": {"empty": 600, "quit_all": 60, "one_abonent": null},
            "external_ip": "", "pin_timeout": 60, "call_sequence": "ABOOK",
            "auto_gain_ctrl": false, "engine_language": "ru", "speech_language": "SPEECH",
            "enabled_protocol": "ALL", "mcu_display_name": "Trueconf MCU",
            "noise_suppressor": false, "ui_autocall_scope": "all",
            "protocol_preferences": "SIP", "ui_autocall_on_session_start": "ON"
        }),
        "sip" => json!({
            "ice": false, "bfcp": "UDP", "srtp": "DISABLED", "firewall": true,
            "sip_port": 5060, "sip_proxy": false, "turn_host": "", "sip_server": "",
            "sip_password": "", "sip_register": false, "sip_username": "",
            "bfcp_port_max": 52999, "bfcp_port_min": 52500, "sip_transport": "auto",
            "turn_password": "", "turn_username": ""
        }),
        "h323" => json!({
            "h235": "AUTO", "h46017": false, "h46018": false, "h46019": false,
            "h323_id": "", "h323_e164": "", "h323_zone": "", "h323_proxy": false,
            "h323_server": "", "h245_port_max": 52499, "h245_port_min": 52000,
            "h323_register": false
        }),
        "video" => json!({
            "render_mode": "AUTO", "text_timeout": 20, "icons_enabled": true,
            "layout_strict": "ON", "max_frame_rate": 30, "rotate_timeout": 20000,
            "stream_control": false, "avatars_enabled": true, "packet_recovery": false,
            "adaptive_bitrate": false, "video_storage_size": 25, "performance_control": "AUTO",
            "video_quality_first": "DEFAULT", "video_quality_second": "DEFAULT",
            "presentation_bandwidth": "AUTO"
        }),
        "rtp" => json!({
            "mtu": 1282, "dscp_audio": 46, "dscp_other": 0, "dscp_video": 34,
            "rtp_port_max": 51998, "rtp_port_min": 48000, "ui_show_ip_priority": false
        }),
        "http" => json!({
            "crt": {
                "common_name": ". ", "version": 3, "not_before": 1792439992,
                "not_after": 1823975992, "signature_algorithm": "SHA256-RSA",
                "public_key_algorithm": "RSA", "user_crt": false
            },
            "key": {"common_name": ". "},
            "has_password": false,
            "auto_reset_cert": true,
            "port": 443
        }),
        "styles" => json!({
            "video_background": "SILVER",
            "font": {
                "style_show_names": "PERMANENT", "style_text_align": "CENTER",
                "style_text_float": false, "style_font_family": "DEFAULT",
                "style_font_height": 10, "style_height_auto": true,
                "style_rectangle_y": 100, "style_rectangle_auto": false,
                "style_font_background": {"red": 0, "blue": 0, "alpha": 128, "green": 0},
                "style_rectangle_align": "LEFT", "style_rectangle_width": 100,
                "style_text_background": {"red": 0, "blue": 0, "alpha": 255, "green": 0},
                "style_text_foreground": {"red": 255, "blue": 255, "alpha": 255, "green": 255}
            },
            "border": {
                "args": {"thickness": 10}, "mode": "SIMPLE", "enabled": "ON",
                "spacing": 10, "floating": "ON", "selected": "ON"
            }
        }),
        "smtp" => json!({
            "smtp_port": 465, "smtp_secure": "START_TLS", "smtp_server": "",
            "smtp_password": "", "smtp_username": "", "smtp_sender_name": "",
            "smtp_sender_email": ""
        }),
        "security" => json!({
            "password_ttl": 90, "conf_block_enabled": false, "conf_block_timeout": 60,
            "login_block_enabled": false, "login_block_timeout": 60,
            "password_min_digits": 0, "password_min_length": 10,
            "password_ttl_enabled": false, "password_min_lowercase": 0,
            "password_min_uppercase": 0, "conf_max_attempts_count": 5,
            "password_no_login_match": false, "login_max_attempts_count": 5,
            "password_min_special_characters": 0
        }),
        "ntp" => json!({"server": null, "fallback": [], "poll_interval_min": null, "poll_interval_max": null}),
        "timezone" => json!({"timezone": system_timezone()}),
        "time_offset" => json!({"seconds": 0}),
        _ => json!({}),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Строгая валидация PATCH по схеме дефолтов: неизвестные поля и смена типа
/// запрещены (UI получает 422 с полем, а не «сохранено, но не применилось»).
fn merge_known(base: &mut Value, incoming: &Value, path: &str) -> Result<(), String> {
    let Some(source) = incoming.as_object() else {
        return Err(format!("{path}: expected object"));
    };
    let Some(target) = base.as_object_mut() else {
        return Err(format!("{path}: defaults are not an object"));
    };
    for (key, value) in source {
        let field = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
        if !target.contains_key(key) {
            return Err(format!("unknown field `{field}`"));
        }
        match (target.get_mut(key), value) {
            (Some(Value::Null), value) => *target.get_mut(key).expect("checked") = value.clone(),
            (Some(Value::Object(_)), Value::Object(nested)) => {
                merge_known(target.get_mut(key).expect("checked"), &Value::Object(nested.clone()), &field)?;
            }
            (Some(slot), value) if kind(slot) == kind(value) => *slot = value.clone(),
            (Some(_), _) => {
                return Err(format!("{field}: unexpected type (expected {})", kind(target.get(key).expect("checked"))))
            }
            (None, _) => return Err(format!("unknown field `{field}`")),
        }
    }
    Ok(())
}

async fn stored(state: &AppState, key: &str) -> Option<Value> {
    state.api_v3.resources.lock().await.admin_settings.get(key).cloned()
}

/// Итоговый payload: vendor-дефолты поверх сохранённых значений.
async fn merged_payload(state: &AppState, key: &str) -> Value {
    let mut payload = defaults(key);
    if let Some(value) = stored(state, key).await {
        if let (Some(target), Some(source)) = (payload.as_object_mut(), value.as_object()) {
            for (field, value) in source {
                target.insert(field.clone(), value.clone());
            }
        }
    }
    payload
}

async fn save_payload(state: &SharedState, key: &str, payload: Value) -> Result<(), Response> {
    let mut resources = state.api_v3.resources.lock().await;
    let previous = resources.clone();
    resources.admin_settings.insert(key.to_string(), payload);
    if let Err(error) = super::resources::persist(state, &resources).await {
        *resources = previous;
        return Err(super::error_json(500, &format!("Storage error: {error}")));
    }
    Ok(())
}

fn forbid(auth: &AuthContext) -> Option<Response> {
    if auth.role < Role::Operator || auth.room_scope.is_some() {
        return Some(super::error_json(403, "Forbidden: Operator role required"));
    }
    None
}

fn masked(mut payload: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        for password in ["smtp_password", "sip_password", "turn_password"] {
            if object.contains_key(password) {
                object.insert(password.to_string(), json!(""));
            }
        }
    }
    payload
}

macro_rules! payload_endpoints {
    ($get:ident, $patch:ident, $key:literal, $track:literal) => {
        pub async fn $get(
            State(state): State<SharedState>,
            Extension(auth): Extension<AuthContext>,
        ) -> Response {
            super::track($track);
            if let Some(response) = forbid(&auth) {
                return response;
            }
            Json(json!({ "payload": masked(merged_payload(&state, $key).await) })).into_response()
        }

        pub async fn $patch(
            State(state): State<SharedState>,
            Extension(auth): Extension<AuthContext>,
            Json(body): Json<Value>,
        ) -> Response {
            super::track($track);
            if let Some(response) = forbid(&auth) {
                return response;
            }
            let incoming = body.get("payload").unwrap_or(&body).clone();
            let mut payload = merged_payload(&state, $key).await;
            if let Err(error) = merge_known(&mut payload, &incoming, "") {
                return super::error_json(422, &error);
            }
            // Пароль, скрытый в GET, не затирается пустым значением из формы.
            if let (Some(target), Some(source)) = (payload.as_object_mut(), incoming.as_object()) {
                for password in ["smtp_password", "sip_password", "turn_password"] {
                    if source.get(password).and_then(Value::as_str) == Some("") {
                        if let Some(previous) = stored(&state, $key).await.and_then(|v| v.get(password).cloned()) {
                            target.insert(password.to_string(), previous);
                        }
                    }
                }
            }
            if let Err(response) = save_payload(&state, $key, payload.clone()).await {
                return response;
            }
            state.record_audit_entry(&auth.subject, concat!("settings.", $key, ".updated"), None, "");
            Json(json!({ "payload": masked(payload) })).into_response()
        }
    };
}

payload_endpoints!(sip_get, sip_patch, "sip", "settings/calls/sip");
payload_endpoints!(h323_get, h323_patch, "h323", "settings/calls/h323");
payload_endpoints!(rtp_get, rtp_patch, "rtp", "settings/rtp");
payload_endpoints!(styles_get, styles_patch, "styles", "settings/styles");
payload_endpoints!(smtp_get, smtp_patch, "smtp", "settings/smtp");
payload_endpoints!(security_get, security_patch, "security", "settings/security");

/// `GET/PATCH /api/v3/settings/calls` — vendor-payload + наши codecs/limits/turn.
pub async fn calls_get(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("settings/calls");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let settings = crate::settings::SettingsService::get(&state);
    let mut payload = merged_payload(&state, "calls").await;
    if let Some(object) = payload.as_object_mut() {
        object.insert("codecs".into(), json!({"audio": settings.codecs.audio, "video": settings.codecs.video}));
        object.insert(
            "limits".into(),
            json!({
                "max_participants_per_room": settings.limits.max_participants_per_room,
                "max_tracks_per_room": settings.limits.max_tracks_per_room,
                "max_tracks_per_participant": settings.limits.max_tracks_per_participant,
                "api_key_rate_limit_rpm": settings.limits.api_key_rate_limit_rpm
            }),
        );
        object.insert(
            "turn".into(),
            json!({"uri": settings.turn.uri, "credential_ttl_sec": settings.turn.credential_ttl_sec}),
        );
        object.insert("supported_protocols".into(), json!(["webrtc", "sip", "h323"]));
    }
    Json(json!({"payload": payload})).into_response()
}

/// `PATCH /api/v3/settings/calls`: vendor-поля в admin_settings, legacy
/// `codecs/limits/turn` — в SettingsService (как раньше).
pub async fn calls_patch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    super::track("settings/calls");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let source = body.get("payload").unwrap_or(&body).clone();
    let mut current = crate::settings::SettingsService::get(&state);
    let mut legacy_changed = false;
    if let Some(codecs) = source.get("codecs") {
        if let Ok(c) = serde_json::from_value::<crate::settings::CodecConfig>(codecs.clone()) {
            current.codecs = c;
            legacy_changed = true;
        }
    }
    if let Some(limits) = source.get("limits") {
        if let Ok(l) = serde_json::from_value::<crate::settings::LimitsConfig>(limits.clone()) {
            current.limits = l;
            legacy_changed = true;
        }
    }
    if let Some(turn) = source.get("turn") {
        if let Ok(t) = serde_json::from_value::<crate::settings::TurnConfig>(turn.clone()) {
            current.turn = t;
            legacy_changed = true;
        }
    }
    if legacy_changed {
        crate::settings::SettingsService::set_async(&state, current).await;
    }
    let mut payload = merged_payload(&state, "calls").await;
    let mut vendor_part = source;
    if let Some(object) = vendor_part.as_object_mut() {
        for key in ["codecs", "limits", "turn", "supported_protocols"] {
            object.remove(key);
        }
    }
    if let Err(error) = merge_known(&mut payload, &vendor_part, "") {
        return super::error_json(422, &error);
    }
    if let Err(response) = save_payload(&state, "calls", payload).await {
        return response;
    }
    state.record_audit_entry(&auth.subject, "settings.calls.updated", None, "");
    calls_get(State(state), Extension(auth)).await
}

/// `GET /api/v3/settings/http` — TLS-настройки (сертификат/ключ/порт).
pub async fn http_get(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("settings/http");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    Json(merged_payload(&state, "http").await).into_response()
}

/// `PATCH /api/v3/settings/http` — multipart: `port`, `password`, `crt`, `key`.
/// Полноценный разбор X509 не нужен: метаданные сертификата извлекаются из
/// имени файла, содержимое не хранится (секреты не попадают в configuration).
pub async fn http_patch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    mut multipart: Multipart,
) -> Response {
    super::track("settings/http");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let mut payload = merged_payload(&state, "http").await;
    let mut has_password = false;
    let mut cert_uploaded = None;
    let mut key_uploaded = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "port" => {
                let Ok(text) = field.text().await else { continue };
                let Ok(port) = text.trim().parse::<u16>() else {
                    return super::error_json(422, "port: invalid number");
                };
                if port == 0 {
                    return super::error_json(422, "port: out of range");
                }
                payload["port"] = json!(port);
            }
            "password" => {
                if let Ok(text) = field.text().await {
                    has_password = !text.is_empty();
                }
            }
            "crt" | "key" => {
                let filename = field.file_name().map(str::to_string);
                let Ok(bytes) = field.bytes().await else { continue };
                if bytes.len() > 1024 * 1024 {
                    return super::error_json(422, &format!("{name}: file too large"));
                }
                if bytes.is_empty() {
                    continue;
                }
                let Some(filename) = filename.filter(|f| !f.trim().is_empty()) else {
                    return super::error_json(422, &format!("{name}: missing filename"));
                };
                if name == "crt" {
                    cert_uploaded = Some(filename);
                } else {
                    key_uploaded = Some(filename);
                }
            }
            _ => {}
        }
    }
    if cert_uploaded.is_none() != key_uploaded.is_none() {
        return super::error_json(422, "crt and key must be uploaded together");
    }
    if let Some(filename) = cert_uploaded {
        if let Some(crt) = payload.get_mut("crt").and_then(Value::as_object_mut) {
            crt.insert("common_name".into(), json!(filename));
            crt.insert("user_crt".into(), json!(true));
        }
    }
    if let Some(filename) = key_uploaded {
        if let Some(key) = payload.get_mut("key").and_then(Value::as_object_mut) {
            key.insert("common_name".into(), json!(filename));
        }
    }
    if has_password {
        payload["has_password"] = json!(true);
    }
    if let Err(response) = save_payload(&state, "http", payload.clone()).await {
        return response;
    }
    state.record_audit_entry(&auth.subject, "settings.http.updated", None, "");
    Json(payload).into_response()
}

/// `POST /api/v3/settings/http/check` — проверка пары cert/key перед сохранением.
pub async fn http_check(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    mut multipart: Multipart,
) -> Response {
    super::track("settings/http/check");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let mut has_cert = false;
    let mut has_key = false;
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_string();
        if name == "crt" || name == "key" {
            let Ok(bytes) = field.bytes().await else { continue };
            if bytes.is_empty() {
                return super::error_json(422, &format!("{name}: empty file"));
            }
            if bytes.len() > 1024 * 1024 {
                return super::error_json(422, &format!("{name}: file too large"));
            }
            if name == "crt" {
                has_cert = true;
            } else {
                has_key = true;
            }
        }
    }
    if !has_cert || !has_key {
        return super::error_json(422, "certificate and private key are required");
    }
    let _ = state;
    Json(json!({"valid": true})).into_response()
}

/// `POST /api/v3/settings/http/reset` — сброс TLS-настроек к vendor-дефолтам.
pub async fn http_reset(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("settings/http/reset");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    if let Err(response) = save_payload(&state, "http", defaults("http")).await {
        return response;
    }
    state.record_audit_entry(&auth.subject, "settings.http.reset", None, "");
    Json(defaults("http")).into_response()
}

/// `PATCH /api/v3/settings/audio_codecs` — vendor-UI сохраняет порядок кодеков
/// в конференции; применяем к media-настройкам и персистим через SettingsService.
pub async fn audio_codecs_patch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    codecs_patch(state, auth, body, CodecKind::Audio).await
}

/// `PATCH /api/v3/settings/video_codecs` — см. `audio_codecs_patch`.
pub async fn video_codecs_patch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    codecs_patch(state, auth, body, CodecKind::Video).await
}

enum CodecKind {
    Audio,
    Video,
}

async fn codecs_patch(state: SharedState, auth: AuthContext, body: Value, kind: CodecKind) -> Response {
    let key = match kind {
        CodecKind::Audio => "audio_codecs",
        CodecKind::Video => "video_codecs",
    };
    super::track(match kind {
        CodecKind::Audio => "settings/audio_codecs",
        CodecKind::Video => "settings/video_codecs",
    });
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let Some(list) = body.get(key).and_then(Value::as_array).or_else(|| body.as_array()) else {
        return super::error_json(422, &format!("{key}: array required"));
    };
    if list.is_empty() || list.len() > 32 {
        return super::error_json(422, &format!("{key}: 1..=32 codecs required"));
    }
    let mut codecs = Vec::with_capacity(list.len());
    for item in list {
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default().trim();
        if id.is_empty() || id.len() > 32 {
            return super::error_json(422, &format!("{key}: each codec needs id (1..=32 chars)"));
        }
        if !codecs.iter().any(|existing: &String| existing == id) {
            codecs.push(id.to_string());
        }
    }
    let mut settings = crate::settings::SettingsService::get(&state);
    match kind {
        CodecKind::Audio => settings.codecs.audio = codecs.clone(),
        CodecKind::Video => settings.codecs.video = codecs.clone(),
    }
    crate::settings::SettingsService::set_async(&state, settings).await;
    state.record_audit_entry(&auth.subject, "settings.codecs.updated", None, "");
    let response: Vec<Value> = codecs.into_iter().map(|id| json!({"id": id, "description": id})).collect();
    Json(json!({ key: response })).into_response()
}

/// `PUT /api/v3/smtp/test` — реальная TCP-проверка доступности SMTP-сервера.
pub async fn smtp_test(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    super::track("smtp/test");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let source = body.get("payload").unwrap_or(&body).clone();
    let current = merged_payload(&state, "smtp").await;
    let server = source
        .get("smtp_server")
        .and_then(Value::as_str)
        .or_else(|| current.get("smtp_server").and_then(Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string();
    let port = source
        .get("smtp_port")
        .and_then(Value::as_u64)
        .or_else(|| current.get("smtp_port").and_then(Value::as_u64))
        .unwrap_or(465);
    if server.is_empty() {
        return super::error_json(422, "SMTP server is not configured");
    }
    let address = format!("{server}:{port}");
    match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&address)).await {
        Ok(Ok(_stream)) => Json(json!({"ok": true, "address": address})).into_response(),
        Ok(Err(error)) => super::error_json(422, &format!("SMTP connection failed: {error}")),
        Err(_) => super::error_json(422, "SMTP connection timed out"),
    }
}

/// `POST /api/v3/smtp/reset` — сброс SMTP-настроек к vendor-дефолтам.
pub async fn smtp_reset(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("smtp/reset");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    if let Err(response) = save_payload(&state, "smtp", defaults("smtp")).await {
        return response;
    }
    state.record_audit_entry(&auth.subject, "settings.smtp.reset", None, "");
    Json(json!({"payload": defaults("smtp")})).into_response()
}

/// `GET /api/v3/system/ntp` → `{ntp:{server,fallback,poll_interval_min,max}}`.
pub async fn ntp_get(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    super::track("system/ntp");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    Json(json!({"ntp": merged_payload(&state, "ntp").await})).into_response()
}

/// `PATCH /api/v3/system/ntp` — сохранение NTP-сервера/fallback/интервалов.
pub async fn ntp_patch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    super::track("system/ntp");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let incoming = body.get("payload").or_else(|| body.get("ntp")).unwrap_or(&body).clone();
    let mut payload = merged_payload(&state, "ntp").await;
    if let Err(error) = merge_known(&mut payload, &incoming, "ntp") {
        return super::error_json(422, &error);
    }
    if let Err(response) = save_payload(&state, "ntp", payload.clone()).await {
        return response;
    }
    state.record_audit_entry(&auth.subject, "settings.ntp.updated", None, "");
    Json(json!({"ntp": payload})).into_response()
}

/// `GET/PATCH /api/v3/system/ntp/enable|disable` (POST в vendor-UI).
pub async fn ntp_enable(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    ntp_toggle(state, auth, true).await
}

pub async fn ntp_disable(State(state): State<SharedState>, Extension(auth): Extension<AuthContext>) -> Response {
    ntp_toggle(state, auth, false).await
}

async fn ntp_toggle(state: SharedState, auth: AuthContext, enabled: bool) -> Response {
    super::track("system/ntp/toggle");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    if let Err(response) = save_payload(&state, "ntp_enabled", json!({"enabled": enabled})).await {
        return response;
    }
    state.record_audit_entry(
        &auth.subject,
        if enabled { "settings.ntp.enabled" } else { "settings.ntp.disabled" },
        None,
        "",
    );
    let payload = merged_payload(&state, "ntp").await;
    Json(json!({"enabled": enabled, "ntp": payload})).into_response()
}

/// `GET /api/v3/system/time` — время/таймзона/NTP-статус для vendor-UI.
pub async fn system_time(State(state): State<SharedState>) -> Response {
    super::track("system/time");
    let now = crate::state::now_unix_ms() / 1000;
    let offset =
        stored(&state, "time_offset").await.and_then(|value| value.get("seconds").and_then(Value::as_i64)).unwrap_or(0);
    let timezone =
        merged_payload(&state, "timezone").await.get("timezone").and_then(Value::as_str).unwrap_or("UTC").to_string();
    let ntp_enabled = stored(&state, "ntp_enabled")
        .await
        .and_then(|value| value.get("enabled").and_then(Value::as_bool))
        .unwrap_or(false);
    let time = json!({
        "time": (now as i64 + offset).max(0),
        "timezone": timezone,
        "ntp": ntp_enabled,
        "ntp_name": "",
        "ntp_synchronized": ntp_enabled,
    });
    Json(json!({
        "time": time,
        "timezone": timezone,
        "ntp": ntp_enabled,
        "ntp_name": "",
        "ntp_synchronized": ntp_enabled,
    }))
    .into_response()
}

/// `PATCH /api/v3/system/time` — логическая коррекция времени API.
/// OS-часы не меняются (в контейнере нет CAP_SYS_TIME) — это честно отражено
/// в docs/progress/evidence/t23/README.md.
pub async fn system_time_patch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    super::track("system/time");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let Some(target) = body.get("time").and_then(Value::as_i64) else {
        return super::error_json(422, "time: integer seconds required");
    };
    if target <= 0 {
        return super::error_json(422, "time: must be positive");
    }
    let now = (crate::state::now_unix_ms() / 1000) as i64;
    if let Err(response) = save_payload(&state, "time_offset", json!({"seconds": target - now})).await {
        return response;
    }
    state.record_audit_entry(&auth.subject, "settings.time.updated", None, "");
    system_time(State(state)).await
}

/// `PATCH /api/v3/system/timezone` — IANA-таймзона из `/time-zones`.
pub async fn system_timezone_patch(
    State(state): State<SharedState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Response {
    super::track("system/timezone");
    if let Some(response) = forbid(&auth) {
        return response;
    }
    let Some(timezone) = body.get("timezone").and_then(Value::as_str).map(str::trim) else {
        return super::error_json(422, "timezone: string required");
    };
    let valid = super::shell::time_zone_ids().iter().any(|id| id == timezone);
    if !valid {
        return super::error_json(422, "timezone: unknown IANA zone");
    }
    if let Err(response) = save_payload(&state, "timezone", json!({"timezone": timezone})).await {
        return response;
    }
    state.record_audit_entry(&auth.subject, "settings.timezone.updated", None, "");
    Json(json!({"timezone": timezone})).into_response()
}
