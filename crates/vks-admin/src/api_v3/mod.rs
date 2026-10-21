//! `/api/v3/*` — адаптер к legacy PNM UI (ADR-0027; `docs/progress/FRONTEND_PORT_PLAN.md` §4.3–4.5).
//!
//! Фаза 2: auth-цепочка (login + двухступенчатый lockout + инвалидация токенов при смене
//! пароля), весь boot/shell (§4.5), WS-протокол (§4.4), catch-all 501 (вкл. `/api/v3/`).
//! Фаза 3: `ui/sessions` / `conferences` из наших комнат (§4.5).
//!
//! Конверт ошибок (UI читает `error.response.data.error.description`):
//! `{"error":{"code":<int>,"description":"<string>"}}`. HTTP-статус совпадает с `code`
//! (401 → интерцептор UI эмитит logout).
//!
//! Слои: fail-closed 503 (наружу всего `/api/v3/*`, включая login и ws) →
//! exempt-list (§4.3) + WS (токен в каждом сообщении) → token-middleware на остальном.

pub mod auth;
pub mod boot;
pub mod call_strings;
pub mod directory;
pub mod export_store;
pub mod history;
pub mod observability;
pub mod resources;
pub mod sessions;
pub mod settings_admin;
pub mod shell;
pub mod ws;
pub mod zip_store;

#[cfg(test)]
#[path = "observability_tests.rs"]
mod observability_tests;
#[cfg(test)]
#[path = "settings_tests.rs"]
mod settings_tests;
#[cfg(test)]
mod tests;

use axum::{
    http::{Method, StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, delete, get, patch, post},
    Json, Router,
};

use vks_common::metrics::API_V3_REQUESTS;

use crate::handlers::SharedState;

pub use auth::Lockout;

/// Состояние адаптера `/api/v3` (в `AppState`).
#[derive(Debug, Default)]
pub struct ApiV3State {
    /// Двухступенчатый lockout логина (BACKEND_RULES §1.4).
    pub lockout: Lockout,
    /// Rate-limit новых WS-соединений (≤ 100/s per-IP, §4.4).
    pub ws_ratelimit: ws::WsRateLimit,
    /// Идентификаторы остановленных сессий (M4.3).
    pub stopped_sessions: std::sync::Mutex<std::collections::HashSet<vks_core::RoomId>>,
    /// Активные исходящие вызовы в процессе дозвона (In-Flight Guard).
    pub in_flight_calls:
        std::sync::Mutex<std::collections::HashMap<(vks_core::RoomId, String), call_strings::CallStringRecord>>,
    /// Outcomes remain visible after a failed dial or a remote disconnect.
    pub call_attempts:
        std::sync::Mutex<std::collections::HashMap<(vks_core::RoomId, String), call_strings::CallAttempt>>,
    /// Запланированные контакты (StandBy / without_call).
    pub scheduled_calls:
        std::sync::Mutex<std::collections::HashMap<vks_core::RoomId, Vec<call_strings::CallStringRecord>>>,
    /// Персистентные настройки медиа абонентов (volume, mute rx/tx).
    pub abonent_settings: std::sync::Mutex<std::collections::HashMap<String, AbonentMediaState>>,
    /// Requested inherit/disabled DSP modes (active filters are reported separately).
    pub abonent_processing: std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>,
    /// Настройки всех 8 вкладок конференции (M1.5 / F1 / F2 / F6).
    pub conference_settings: std::sync::Mutex<std::collections::HashMap<vks_core::RoomId, serde_json::Value>>,
    pub resources: tokio::sync::Mutex<resources::ResourceState>,
    /// `settings/video` (правится администратором; эталонные умолчания — ref-92).
    pub video_settings: std::sync::Mutex<serde_json::Value>,
    /// Сгенерированные ZIP-архивы истории (export/clean) для
    /// `/static/call_history/{filename}`; cap 32, TTL 1 час.
    pub history_exports: std::sync::Mutex<export_store::ExportStore>,
    /// Сгенерированные ZIP-экспорты журнала событий (`/static/log_events/{filename}`).
    pub log_exports: std::sync::Mutex<export_store::ExportStore>,
}

/// Состояние медиа-настроек абонента (громкость, mute).
#[derive(Debug, Clone, Default)]
pub struct AbonentMediaState {
    pub audio_rx_muted: bool,
    pub audio_tx_muted: bool,
    pub video_rx_muted: bool,
    pub video_tx_muted: bool,
    pub volume_rx: i32,
    pub volume_tx: i32,
}

/// Нормализует опечатку вендорного фронтенда PNM (`}/contacts` -> `/contacts`) до роутинга.
async fn normalize_stray_brace_middleware(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let uri = req.uri().clone();
    let path = uri.path();
    if path.contains("}/contacts") {
        let new_path = path.replace("}/contacts", "/contacts");
        let new_uri_str = if let Some(query) = uri.query() { format!("{}?{}", new_path, query) } else { new_path };
        if let Ok(parsed) = new_uri_str.parse::<axum::http::Uri>() {
            *req.uri_mut() = parsed;
        }
    }
    next.run(req).await
}

/// Собирает `Router` адаптера `/api/v3/*`.
///
/// Монтируется в композиционном корне (`vks-server`) через `.merge(...)`.
pub fn router(state: SharedState) -> Router {
    let protected = Router::new()
        // Auth (protected: смена пароля).
        .route("/api/v3/password", patch(auth::change_password))
        // Shell (§4.5).
        .route("/api/v3/profile", get(shell::profile))
        .route("/api/v3/time-zones", get(shell::time_zones))
        .route("/api/v3/locales", get(shell::locales))
        .route("/api/v3/abonents/stop_reasons", get(shell::stop_reasons))
        .route("/api/v3/abonents/count", get(shell::abonents_count))
        .route("/api/v3/users/credentials/expire", get(shell::users_credentials_expire))
        .route("/api/v3/ui/system/status", get(shell::ui_system_status))
        // T2.4: наблюдаемость/сервис (read-only) + явные отказы на мутации.
        .route("/api/v3/log_events", get(observability::log_events))
        .route("/api/v3/log_events/export", get(observability::log_events_export))
        .route(
            "/api/v3/system/restart_services",
            post(settings_admin::restart_services).get(settings_admin::restart_services),
        )
        .route(
            "/api/v3/system/restart_engine",
            post(settings_admin::restart_services).get(settings_admin::restart_services),
        )
        .route(
            "/api/v3/system/restart_web_api",
            post(settings_admin::restart_services).get(settings_admin::restart_services),
        )
        .route(
            "/api/v3/system/restart_webproxy",
            post(settings_admin::restart_services).get(settings_admin::restart_services),
        )
        .route("/api/v3/system/network/interfaces", get(observability::network_interfaces_list))
        .route(
            "/api/v3/system/network/interfaces/{interface}",
            get(observability::network_interface_get).patch(observability::network_interface_patch),
        )
        .route("/api/v3/system/stats/memory", get(observability::stats_memory))
        .route("/api/v3/system/stats/cpu", get(observability::stats_cpu))
        .route("/api/v3/system/stats/disk", get(observability::stats_disk))
        .route("/api/v3/system/stats/network", get(observability::stats_network))
        .route("/api/v3/system/packages", get(observability::system_packages))
        .route("/api/v3/engine/log", get(observability::engine_log))
        .route("/api/v3/ui/terminals", get(observability::terminals_list))
        .route("/api/v3/system/utils/ping", get(observability::utils_ping))
        .route("/api/v3/system/utils/traceroute", get(observability::utils_traceroute))
        .route("/api/v3/system/utils/tcpdumps/status", get(observability::tcpdumps_status))
        .route(
            "/api/v3/system/utils/tcpdumps",
            get(observability::tcpdumps_list).post(observability::tcpdumps_start).delete(observability::tcpdumps_stop),
        )
        .route("/api/v3/system/utils/tcpdumps/{file}", delete(observability::tcpdumps_delete))
        .route("/api/v3/terminals", get(observability::terminals_list).post(observability::terminals_unsupported))
        .route("/api/v3/terminals/credentials", get(observability::terminals_credentials))
        .route(
            "/api/v3/terminals/credentials",
            post(observability::terminals_unsupported)
                .patch(observability::terminals_unsupported)
                .delete(observability::terminals_unsupported),
        )
        .route("/api/v3/terminals/firmwares", get(observability::terminals_empty_list))
        .route("/api/v3/terminals/configurations", get(observability::terminals_empty_list))
        .route("/api/v3/videorecords", get(observability::videorecords_list))
        .route("/api/v3/videorecords/{id}", delete(observability::videorecords_delete))
        .route("/api/v3/conferences/{id}/videorecords/estimated_time", get(observability::videorecords_estimated_time))
        .route("/api/v3/license", get(shell::license))
        .route("/api/v3/features", get(shell::features))
        .route("/api/v3/engine/calibration", get(shell::engine_calibration))
        .route("/api/v3/engine/diagnostics", get(shell::engine_diagnostics))
        .route("/api/v3/system/time", get(settings_admin::system_time).patch(settings_admin::system_time_patch))
        .route("/api/v3/system/timezone", axum::routing::patch(settings_admin::system_timezone_patch))
        .route("/api/v3/system/ntp", get(settings_admin::ntp_get).patch(settings_admin::ntp_patch))
        .route("/api/v3/system/ntp/enable", axum::routing::post(settings_admin::ntp_enable))
        .route("/api/v3/system/ntp/disable", axum::routing::post(settings_admin::ntp_disable))
        .route("/api/v3/settings/calls", get(settings_admin::calls_get).patch(settings_admin::calls_patch))
        .route("/api/v3/settings/calls/sip", get(settings_admin::sip_get).patch(settings_admin::sip_patch))
        .route("/api/v3/settings/calls/h323", get(settings_admin::h323_get).patch(settings_admin::h323_patch))
        .route("/api/v3/settings/rtp", get(settings_admin::rtp_get).patch(settings_admin::rtp_patch))
        .route("/api/v3/settings/http", get(settings_admin::http_get).patch(settings_admin::http_patch))
        .route("/api/v3/settings/http/check", axum::routing::post(settings_admin::http_check))
        .route("/api/v3/settings/http/reset", axum::routing::post(settings_admin::http_reset))
        .route("/api/v3/settings/styles", get(settings_admin::styles_get).patch(settings_admin::styles_patch))
        .route("/api/v3/settings/smtp", get(settings_admin::smtp_get).patch(settings_admin::smtp_patch))
        .route("/api/v3/smtp/test", axum::routing::put(settings_admin::smtp_test))
        .route("/api/v3/smtp/reset", axum::routing::post(settings_admin::smtp_reset))
        .route("/api/v3/settings/security", get(settings_admin::security_get).patch(settings_admin::security_patch))
        .route("/api/v3/users", get(directory::list_users).post(directory::create_user))
        .route(
            "/api/v3/users/{id}",
            get(directory::get_user).patch(directory::update_user).delete(directory::delete_user),
        )
        .route(
            "/api/v3/users/{id}/credentials",
            get(directory::get_user_credentials)
                .post(directory::create_user_credentials)
                .patch(directory::update_user_credentials)
                .delete(directory::delete_user_credentials),
        )
        .route(
            "/api/v3/profile/credentials",
            get(directory::profile_credentials).patch(directory::update_profile_credentials),
        )
        .route("/api/v3/groups", get(directory::list_groups).post(directory::create_group))
        .route(
            "/api/v3/groups/{id}",
            get(directory::get_group).patch(directory::update_group).delete(directory::delete_group),
        )
        .route(
            "/api/v3/users/{id}/groups/{group_id}",
            post(directory::add_user_to_group).delete(directory::remove_user_from_group),
        )
        .route("/api/v3/users/{id}/roles/{role_id}", post(directory::add_user_role).delete(directory::remove_user_role))
        .route("/api/v3/roles", get(directory::list_roles).post(directory::create_role))
        .route(
            "/api/v3/roles/{id}",
            get(directory::get_role).patch(directory::update_role).delete(directory::delete_role),
        )
        .route("/api/v3/users/{id}/call_strings", get(directory::list_user_call_strings))
        .route(
            "/api/v3/users/{id}/call_strings/{address_id}",
            post(directory::attach_user_call_string).delete(directory::detach_user_call_string),
        )
        .route(
            "/api/v3/users/{id}/call_strings/{address_id}/favorite",
            post(directory::favorite_user_call_string).delete(directory::unfavorite_user_call_string),
        )
        .route(
            "/api/v3/ui/users/{id}/call_strings/{address_id}/favorite",
            post(directory::favorite_user_call_string).delete(directory::unfavorite_user_call_string),
        )
        .route("/api/v3/ui/users/{id}/call_strings", post(directory::create_user_call_string))
        .route(
            "/api/v3/ui/users/{id}/call_strings/{address_id}",
            patch(directory::update_user_call_string).delete(directory::detach_user_call_string),
        )
        .route("/api/v3/schedules", get(resources::schedules).post(resources::create_schedule))
        .route(
            "/api/v3/schedules/{id}",
            get(resources::get_schedule).patch(resources::edit_schedule).delete(resources::remove_schedule),
        )
        .route("/api/v3/video_layout_grids", get(resources::grids))
        .route("/api/v3/video_layout_grids/{id}", get(resources::get_grid))
        .route("/api/v3/conferences/{id}/video_layouts/{resource_id}/contacts", get(resources::layout_contacts))
        .route(
            "/api/v3/conferences/{id}/video_layouts/{resource_id}/contacts/{contact_id}",
            post(resources::attach_layout_contact).delete(resources::detach_layout_contact),
        )
        .route("/api/v3/audiochannels", get(resources::list_global).post(resources::create))
        .route("/api/v3/ui/audiochannels", get(resources::list_global))
        .route(
            "/api/v3/audiochannels/{id}",
            get(resources::get_global).patch(resources::edit_global).delete(resources::remove_global),
        )
        .route(
            "/api/v3/video_layouts/{id}",
            get(resources::get_global).patch(resources::edit_global).delete(resources::remove_global),
        )
        .route("/api/v3/ui/conferences/{id}/video_layouts", get(resources::list_linked))
        .route(
            "/api/v3/ui/conferences/{id}/video_layouts/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked),
        )
        .route("/api/v3/ui/sessions/{id}/video_layouts", get(resources::list_linked))
        .route(
            "/api/v3/ui/sessions/{id}/video_layouts/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked),
        )
        .route("/api/v3/conferences/{id}/video_layouts", get(resources::list_linked))
        .route("/api/v3/sessions/{id}/video_layouts", get(resources::list_linked))
        .route(
            "/api/v3/conferences/{id}/video_layouts/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked).post(resources::attach).delete(resources::detach),
        )
        .route(
            "/api/v3/sessions/{id}/video_layouts/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked).post(resources::attach).delete(resources::detach),
        )
        .route("/api/v3/conferences/{id}/audiochannels", get(resources::list_linked))
        .route("/api/v3/sessions/{id}/audiochannels", get(resources::list_linked))
        .route("/api/v3/ui/conferences/{id}/audiochannels", get(resources::list_linked))
        .route("/api/v3/ui/sessions/{id}/audiochannels", get(resources::list_linked))
        .route(
            "/api/v3/conferences/{id}/audiochannels/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked).post(resources::attach).delete(resources::detach),
        )
        .route(
            "/api/v3/sessions/{id}/audiochannels/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked).post(resources::attach).delete(resources::detach),
        )
        .route(
            "/api/v3/ui/conferences/{id}/audiochannels/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked).post(resources::attach).delete(resources::detach),
        )
        .route(
            "/api/v3/ui/sessions/{id}/audiochannels/{resource_id}",
            get(resources::get_linked).patch(resources::edit_linked).post(resources::attach).delete(resources::detach),
        )
        .route("/api/v3/sessions/{id}/video_layouts/{resource_id}/abonents", get(resources::layout_abonents))
        .route("/api/v3/ui/sessions/{id}/video_layouts/{resource_id}/abonents", get(resources::layout_abonents))
        .route(
            "/api/v3/sessions/{id}/video_layouts/{resource_id}/abonents/{abonent_id}",
            post(resources::attach_layout_abonent).delete(resources::detach_layout_abonent),
        )
        .route(
            "/api/v3/ui/sessions/{id}/video_layouts/{resource_id}/abonents/{abonent_id}",
            post(resources::attach_layout_abonent).delete(resources::detach_layout_abonent),
        )
        .route("/api/v3/conferences/{id}/video_layouts/templates", get(resources::templates))
        .route("/api/v3/sessions/{id}/video_layouts/templates", get(resources::templates))
        .route("/api/v3/conferences/{id}/video_layouts/{resource_id}/templates", patch(resources::edit_linked))
        .route("/api/v3/sessions/{id}/video_layouts/{resource_id}/templates", patch(resources::edit_linked))
        .route("/api/v3/settings/video", get(shell::settings_video).patch(shell::update_settings_video))
        .route("/api/v3/ui/video_layouts/calculate", post(shell::video_layouts_calculate))
        .route(
            "/api/v3/settings/audio_codecs",
            get(shell::settings_audio_codecs).patch(settings_admin::audio_codecs_patch),
        )
        .route(
            "/api/v3/settings/video_codecs",
            get(shell::settings_video_codecs).patch(settings_admin::video_codecs_patch),
        )
        .route("/api/v3/waitingroom", get(shell::waitingroom))
        .route("/api/v3/conferences/status", get(shell::conferences_status))
        // Фаза 3 — данные (наши комнаты).
        .route("/api/v3/ui/sessions", get(sessions::ui_sessions).post(sessions::start_session))
        .route("/api/v3/ui/sessions/{id}", get(sessions::session).delete(sessions::stop_session))
        .route("/api/v3/ui/sessions/{id}/abonents", get(sessions::session_abonents))
        .route("/api/v3/sessions", get(sessions::sessions).post(sessions::start_session))
        .route("/api/v3/sessions/{id}", get(sessions::session).delete(sessions::stop_session))
        .route("/api/v3/sessions/{id}/abonents", get(sessions::session_abonents))
        .route("/api/v3/conferences", get(sessions::conferences).post(sessions::create_conference))
        .route(
            "/api/v3/conferences/{id}",
            get(sessions::conference).patch(sessions::update_conference).delete(sessions::delete_conference),
        )
        .route("/api/v3/conferences/{id}/contacts", get(sessions::conference_contacts))
        .route("/api/v3/ui/conferences", get(sessions::ui_conferences).post(sessions::create_conference))
        .route(
            "/api/v3/ui/conferences/{id}",
            get(sessions::conference).patch(sessions::update_conference).delete(sessions::delete_conference),
        )
        .route("/api/v3/ui/conferences/{id}/contacts", get(sessions::conference_contacts))
        // Call strings & Contacts (V1-V3, ADR-0027).
        .route("/api/v3/ui/conferences/{id}/call_strings", post(call_strings::create_conference_call_string))
        .route("/api/v3/conferences/{id}/invite/call_string/list", post(call_strings::invite_conference_call_strings))
        .route("/api/v3/ui/sessions/{id}/call_strings", post(call_strings::create_session_call_string))
        .route(
            "/api/v3/ui/sessions/{id}/call_strings/without_call",
            post(call_strings::create_session_call_string_standby),
        )
        .route("/api/v3/sessions/{id}/invite/call_strings/list", post(call_strings::invite_session_call_strings_list))
        .route("/api/v3/call_strings", get(call_strings::list_call_strings).post(call_strings::create_call_string))
        .route("/api/v3/ui/call_strings", get(call_strings::list_call_strings).post(call_strings::create_call_string))
        .route(
            "/api/v3/call_strings/{id}",
            get(call_strings::get_call_string)
                .patch(call_strings::update_call_string)
                .delete(call_strings::delete_call_string),
        )
        .route(
            "/api/v3/ui/call_strings/{id}",
            get(call_strings::get_call_string)
                .patch(call_strings::update_call_string)
                .delete(call_strings::delete_call_string),
        )
        .route(
            "/api/v3/sessions/{id}/contacts",
            get(sessions::conference_contacts).delete(sessions::remove_session_contacts_batch),
        )
        .route(
            "/api/v3/ui/sessions/{id}/contacts",
            get(sessions::conference_contacts).delete(sessions::remove_session_contacts_batch),
        )
        .route("/api/v3/sessions/{id}/contacts/list", delete(sessions::remove_session_contacts_batch))
        .route("/api/v3/ui/sessions/{id}/contacts/list", delete(sessions::remove_session_contacts_batch))
        .route("/api/v3/sessions/{id}/contacts/{contact_id}", delete(sessions::remove_session_contact))
        .route("/api/v3/ui/sessions/{id}/contacts/{contact_id}", delete(sessions::remove_session_contact))
        .route("/api/v3/ui/video_layouts", get(resources::list_global).post(resources::create))
        .route("/api/v3/video_layouts", get(shell::video_layouts))
        .route("/api/v3/ui/video_layouts/mode", post(shell::switch_video_layout_mode))
        .route("/api/v3/ui/sessions/quick", post(sessions::quick_session))
        .route("/api/v3/sessions/quick", post(sessions::quick_session))
        .route("/api/v3/ui/records", get(shell::ui_records))
        .route("/api/v3/records", get(shell::ui_records))
        .route("/api/v3/abonents", get(sessions::list_abonents))
        .route("/api/v3/abonents/previews", get(sessions::abonents_previews))
        .route("/api/v3/abonents/{id}", delete(sessions::remove_abonent).patch(sessions::edit_abonent))
        .route("/api/v3/abonents/{id}/{setting}", post(sessions::switch_abonent_settings))
        .route("/api/v3/abonents/{id}/dtmf", post(sessions::send_abonent_dtmf))
        .route("/api/v3/abonents/{id}/fecc", post(sessions::send_abonent_fecc))
        .route("/api/v3/sessions/{id}/abonents/{abonent_id}", delete(sessions::remove_session_abonent))
        .route("/api/v3/ui/sessions/{id}/abonents/{abonent_id}", delete(sessions::remove_session_abonent))
        // T2.2: история конференций (vendor /call_history*).
        .route("/api/v3/call_history", get(history::list))
        .route("/api/v3/call_history/export", get(history::export))
        .route("/api/v3/call_history/{id}/export", get(history::export_session))
        .route("/api/v3/call_history/clean", post(history::clean))
        .route("/api/v3/call_history/{id}", get(history::detail))
        .route("/api/v3/ui/session_history/{id}", get(history::ui_session_history))
        .route("/api/v3/ui/call_history/{id}", get(history::ui_call_history))
        .route("/api/v3/engine/command", post(shell::engine_command))
        .route("/api/v3/engine/abonents", get(sessions::engine_abonents))
        .layer(middleware::from_fn(normalize_stray_brace_middleware))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::token_middleware));

    Router::new()
        // Exempt-list (§4.3) — без токена.
        .route("/api/v3/login", post(auth::login))
        .route("/api/v3/login/status", get(boot::login_status))
        .route("/api/v3/system/about", get(boot::system_about))
        .route("/api/v3/about", get(boot::about))
        .route("/api/v3/references", get(boot::references))
        // WS: сокет открывается без токена, токен — в каждом сообщении (§4.4).
        .route("/api/v3/ws", get(ws::ws_handler))
        // Vendor отдаёт готовые выгрузки истории как статику (без токена).
        .route("/static/call_history/{filename}", get(history::static_export))
        .route("/static/log_events/{filename}", get(observability::static_log_export))
        .merge(protected)
        // Catch-all 501 на любой метод; `{*rest}` не матчит пустой rest (B3 ревью),
        // поэтому `/api/v3/` и `/api/v3` регистрируются явно (единый конверт).
        .route("/api/v3/{*rest}", any(catch_all))
        .route("/api/v3/", any(catch_all))
        .route("/api/v3", any(catch_all))
        // Fail-closed: jwt_issuer=None → 503 на **весь** /api/v3 (включая login и ws).
        .layer(middleware::from_fn_with_state(state.clone(), auth::fail_closed_middleware))
        .with_state(state)
}

/// Общий контракт списков PNM: подстрочный поиск по имени/uri, сортировка
/// (`created_at`/`display_name`, `sort_order=1` — по убыванию) и пагинация.
/// Используется адресной книгой и списками ресурсов, чтобы формы не расходились.
pub(crate) fn sort_and_page(
    mut items: Vec<serde_json::Value>,
    search: &str,
    sort_field: &str,
    sort_order: i64,
    page: usize,
    page_size: usize,
) -> (usize, Vec<serde_json::Value>) {
    if !search.is_empty() {
        let needle = search.to_lowercase();
        items.retain(|item| {
            let name = item["display_name"].as_str().unwrap_or_default().to_lowercase();
            let uri = item["uri"].as_str().unwrap_or_default().to_lowercase();
            name.contains(&needle) || uri.contains(&needle)
        });
    }
    items.sort_by(|left, right| {
        // Любое числовое поле (`created_at`, `size`, ...) сравнивается как число,
        // остальные — как регистронезависимая строка. Один компаратор на все
        // списки PNM вместо копий в каждом хендлере.
        let order = match (left[sort_field].as_u64(), right[sort_field].as_u64()) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => left[sort_field]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .cmp(&right[sort_field].as_str().unwrap_or_default().to_lowercase()),
        };
        if sort_order == 1 {
            order.reverse()
        } else {
            order
        }
    });
    let count = items.len();
    let size = page_size.max(1);
    let start = page.saturating_sub(1).saturating_mul(size);
    (count, items.into_iter().skip(start).take(size).collect())
}

/// Общие query-параметры списков PNM: `search`, `sort_field`, `sort_order`
/// (`1` — по убыванию), `page`, `page_size` (дефолт 100). Один парсер для
/// HTTP-query и WS-payload, чтобы формы списков не расходились.
/// Дефолтный размер страницы списков PNM (UI всегда шлёт `page_size`).
pub(crate) const DEFAULT_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone)]
pub(crate) struct ListParams {
    pub search: String,
    pub sort_field: String,
    pub sort_order: i64,
    pub page: usize,
    pub page_size: usize,
}

impl ListParams {
    /// `params` — JSON-объект (WS) или сконвертированная query-карта (HTTP).
    pub(crate) fn parse(params: &serde_json::Value) -> Self {
        let text = |key: &str| {
            params
                .get(key)
                .map(|value| match value {
                    serde_json::Value::String(raw) => raw.clone(),
                    serde_json::Value::Null => String::new(),
                    value => value.to_string(),
                })
                .filter(|value| !value.is_empty())
                .unwrap_or_default()
        };
        let number = |key: &str| {
            params.get(key).and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
                    .or_else(|| value.as_i64().and_then(|raw| u64::try_from(raw).ok()))
            })
        };
        let sort_order = params
            .get("sort_order")
            .and_then(|value| value.as_i64().or_else(|| value.as_str().and_then(|raw| raw.parse().ok())))
            .unwrap_or(0);
        let page = number("page").unwrap_or(1).max(1) as usize;
        let page_size = number("page_size")
            .and_then(|size| usize::try_from(size).ok())
            .filter(|size| *size > 0)
            .unwrap_or(DEFAULT_PAGE_SIZE);
        Self { search: text("search"), sort_field: text("sort_field"), sort_order, page, page_size }
    }

    /// Query-карта Axum → те же правила, что у WS-payload.
    pub(crate) fn from_query(params: &std::collections::HashMap<String, String>) -> Self {
        let object = params.iter().map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())));
        Self::parse(&serde_json::Value::Object(object.collect()))
    }

    /// `sort_field` по умолчанию: `created_at` для адресов/аудио, иначе имя.
    pub(crate) fn sort_field_or(&self, fallback: &str) -> String {
        if self.sort_field.is_empty() {
            fallback.to_string()
        } else {
            self.sort_field.clone()
        }
    }
}

/// Учитывает вызов endpoint'а: `vks_api_v3_requests_total{endpoint}` (§4.7).
pub(crate) fn track(endpoint: &'static str) {
    API_V3_REQUESTS.with_label_values(&[endpoint]).inc();
}

/// Конверт ошибки API v3: `{"error":{"code":<int>,"description":"…"}}`.
pub(crate) fn error_json(code: u16, description: &str) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(serde_json::json!({ "error": { "code": code, "description": description } }))).into_response()
}

/// 401 (UI эмитит `logout`).
pub(crate) fn unauthorized(description: &str) -> Response {
    error_json(401, description)
}

/// Catch-all: 501 `Not implemented` + warn + метрика `endpoint="unmatched"` (§4.5).
///
/// Падение в 404 запрещено: UI считает 404 «метод не существует».
async fn catch_all(method: Method, uri: Uri) -> Response {
    track("unmatched");
    tracing::warn!(%method, %uri, "api_v3: unmatched route → 501");
    error_json(501, "Not implemented")
}
