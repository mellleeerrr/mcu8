//! CSV/ZIP-выгрузка истории конференций (vendor-формат).
//!
//! Вынесено из `history.rs`: кодеки/таймзоны не должны жить в модуле с
//! HTTP-роутами. ZIP-писатель общий с журналом событий (`zip_store`).

use serde_json::Value;

use crate::state::{now_unix_ms, AppState};

use super::HistorySession;
use crate::util::csv_escape;

/// Имя файла из display_name: только безопасные символы (без `/` и `..`).
pub(super) fn sanitize_filename(name: &str) -> String {
    let mut result: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    result.truncate(64);
    let result = result.trim_matches('_').to_string();
    if result.is_empty() {
        "session".to_string()
    } else {
        result
    }
}

/// Имена контактов для CSV: профиль справочника → Users (bootstrap =
/// Administrator) → id. Vendor пишет в колонках «Запуск/Остановка» имена, а не
/// UUID; ID-колонка — DTMF (тот же ID, что в UI), fallback — id сессии.
pub(super) async fn contact_names(state: &AppState) -> std::collections::HashMap<String, String> {
    let mut names = std::collections::HashMap::new();
    if let Ok(users) = state.users.lock() {
        for user in users.values() {
            let name = if user.id == vks_core::UserId::bootstrap_operator() {
                "Administrator".to_string()
            } else {
                user.username.clone()
            };
            names.insert(user.id.to_string(), name);
        }
    }
    let resources = state.api_v3.resources.lock().await;
    for (id, profile) in &resources.directory.users {
        names.insert(id.clone(), profile.display_name.clone());
    }
    names
}

pub(super) async fn current_timezone(state: &AppState) -> String {
    let stored = state
        .api_v3
        .resources
        .lock()
        .await
        .admin_settings
        .get("timezone")
        .and_then(|value| value.get("timezone"))
        .and_then(Value::as_str)
        .map(str::to_string);
    stored.unwrap_or_else(super::super::shell::system_timezone)
}

/// Дата в TZ администратора (vendor CSV показывает локальное время).
pub(super) fn format_datetime_tz(secs: i64, timezone: &str) -> String {
    use chrono::TimeZone;
    let zone: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    zone.timestamp_opt(secs, 0)
        .single()
        .map(|datetime| datetime.format("%d.%m.%Y %H:%M:%S").to_string())
        .unwrap_or_else(|| format_datetime(secs))
}

/// CSV как у vendor: разделитель `;`, русские заголовки, даты `ДД.ММ.ГГГГ ЧЧ:ММ:СС`
/// в таймзоне администратора (как UI-список/карточка).
pub(super) fn build_history_csv(
    sessions: &[HistorySession],
    names: &std::collections::HashMap<String, String>,
    timezone: &str,
) -> Vec<u8> {
    let mut text =
        String::from("Название;ID;Маршрут;Время начала;Время окончания;Длительность;Участники;Запуск;Остановка\n");
    for session in sessions {
        let duration = (session.stopped_at - session.started_at).max(0);
        let run = names.get(&session.creator_id).cloned().unwrap_or_else(|| session.creator_id.clone());
        let stop = names.get(&session.stopped_by).cloned().unwrap_or_else(|| session.stopped_by.clone());
        let id = session.dtmf.clone().unwrap_or_else(|| session.id.clone());
        text.push_str(&format!(
            "{};{};{};{};{};{};{};{};{}\n",
            csv_escape(&session.display_name),
            csv_escape(&id),
            csv_escape(session.route.as_deref().unwrap_or("")),
            format_datetime_tz(session.started_at, timezone),
            format_datetime_tz(session.stopped_at, timezone),
            format_duration(duration),
            session.abonents_count,
            csv_escape(&run),
            csv_escape(&stop),
        ));
    }
    text.into_bytes()
}

/// ZIP (method store, как у vendor) с единственным CSV внутри.
pub(super) fn build_history_zip(
    csv_name: &str,
    sessions: &[HistorySession],
    names: &std::collections::HashMap<String, String>,
    timezone: &str,
) -> Vec<u8> {
    let data = build_history_csv(sessions, names, timezone);
    super::super::zip_store::zip_store(csv_name, &data, (now_unix_ms() / 1000) as i64)
}

pub(super) fn format_datetime(secs: i64) -> String {
    let (year, month, day, hour, minute, second) = utc_parts(secs);
    format!("{day:02}.{month:02}.{year:04} {hour:02}:{minute:02}:{second:02}")
}

pub(super) fn format_date_utc(secs: i64) -> String {
    let (year, month, day, _, _, _) = utc_parts(secs);
    format!("{year:04}-{month:02}-{day:02}")
}

pub(super) fn format_duration(secs: i64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Гражданская дата из unix-секунд (UTC) — алгоритм Hinnant, без chrono.
pub(super) fn utc_parts(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month as u32, day as u32, rem / 3600, (rem % 3600) / 60, rem % 60)
}
