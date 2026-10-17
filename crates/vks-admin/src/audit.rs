// AuditLog — аудит-лог (ADR-0019): append-only, фильтры
// (user/action/date/IP), export CSV, retention 2 года (730d).
//
// Immutability: только `record` (append); `prune` — retention (2y).
// BACKEND_RULES §5.3: retention audit 2y.

use serde::{Deserialize, Serialize};

/// Запись аудита (ADR-0019).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Время (ms epoch).
    pub ts_ms: u64,
    /// Пользователь (username, не JWT).
    pub user: String,
    /// Действие (например "room.mode_changed").
    pub action: String,
    /// Комната (Option: None = глобальное).
    pub room: Option<String>,
    /// IP клиента.
    pub ip: String,
}

/// Фильтр аудита (ADR-0019: user/action/date/IP).
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    /// Username (substring match).
    pub user: Option<String>,
    /// Action (exact match).
    pub action: Option<String>,
    /// IP (exact match).
    pub ip: Option<String>,
    /// Диапазон времени [from, to), ms epoch.
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
}

/// Аудит-лог (append-only).
#[derive(Debug, Clone, Default)]
pub struct AuditLog {
    entries: Vec<AuditEntry>,
}

impl AuditLog {
    /// Создаёт пустой лог.
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавляет запись (append-only, immutability).
    pub fn record(&mut self, entry: AuditEntry) {
        self.entries.push(entry);
    }

    /// Число записей.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Лог пуст.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Возвращает все записи (immutable snapshot).
    pub fn all(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Фильтрует записи (ADR-0019: user/action/date/IP).
    pub fn filter(&self, filter: &AuditFilter) -> Vec<&AuditEntry> {
        self.entries.iter().filter(|e| matches_filter(e, filter)).collect()
    }

    /// Удаляет записи старше retention_days (2y, ADR-0019).
    ///
    /// Возвращает число удалённых записей.
    pub fn prune(&mut self, now_ms: u64, retention_days: u32) -> usize {
        let cutoff_ms = retention_days as u64 * 24 * 60 * 60 * 1000;
        let keep_from = now_ms.saturating_sub(cutoff_ms);
        let before = self.entries.len();
        self.entries.retain(|e| e.ts_ms >= keep_from);
        before - self.entries.len()
    }

    /// Экспортирует записи (по фильтру) в CSV.
    ///
    /// Колонки: `ts_ms,user,action,room,ip`. CSV escaping: кавычки,
    /// запятые, новые строки.
    pub fn export_csv(&self, filter: &AuditFilter) -> String {
        let rows = self.filter(filter);
        let mut out = String::from("ts_ms,user,action,room,ip\n");
        for e in rows {
            out.push_str(&format!(
                "{},{},{},{},{}\n",
                e.ts_ms,
                csv_escape(&e.user),
                csv_escape(&e.action),
                csv_escape(e.room.as_deref().unwrap_or("")),
                csv_escape(&e.ip)
            ));
        }
        out
    }
}

/// Проверяет, проходит ли запись фильтр.
fn matches_filter(e: &AuditEntry, f: &AuditFilter) -> bool {
    if let Some(user) = f.user.as_deref() {
        if !e.user.contains(user) {
            return false;
        }
    }
    if let Some(action) = f.action.as_deref() {
        if e.action != action {
            return false;
        }
    }
    if let Some(ip) = f.ip.as_deref() {
        if e.ip != ip {
            return false;
        }
    }
    if let Some(from) = f.from_ms {
        if e.ts_ms < from {
            return false;
        }
    }
    if let Some(to) = f.to_ms {
        if e.ts_ms >= to {
            return false;
        }
    }
    true
}

/// CSV-escape (RFC 4180): кавычки → `""`, запятые/`\n` → в кавычках.
fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: u64, user: &str, action: &str, room: Option<&str>, ip: &str) -> AuditEntry {
        AuditEntry {
            ts_ms: ts,
            user: user.into(),
            action: action.into(),
            room: room.map(str::to_string),
            ip: ip.into(),
        }
    }

    #[test]
    fn record_is_append_only() {
        let mut log = AuditLog::new();
        log.record(entry(1, "u", "a", None, "1.1.1.1"));
        log.record(entry(2, "u", "b", Some("r"), "1.1.1.1"));
        assert_eq!(log.len(), 2);
        assert_eq!(log.all()[0].ts_ms, 1);
        assert_eq!(log.all()[1].ts_ms, 2);
    }

    #[test]
    fn filter_by_user_action_ip() {
        let mut log = AuditLog::new();
        log.record(entry(1, "alice", "room.create", None, "10.0.0.1"));
        log.record(entry(2, "bob", "room.create", None, "10.0.0.2"));
        log.record(entry(3, "alice", "room.delete", Some("r1"), "10.0.0.1"));

        let by_user = log.filter(&AuditFilter { user: Some("alice".into()), ..Default::default() });
        assert_eq!(by_user.len(), 2);

        let by_action = log.filter(&AuditFilter { action: Some("room.delete".into()), ..Default::default() });
        assert_eq!(by_action.len(), 1);
        assert_eq!(by_action[0].user, "alice");

        let by_ip = log.filter(&AuditFilter { ip: Some("10.0.0.2".into()), ..Default::default() });
        assert_eq!(by_ip.len(), 1);
        assert_eq!(by_ip[0].user, "bob");
    }

    #[test]
    fn filter_by_date_range() {
        let mut log = AuditLog::new();
        log.record(entry(100, "u", "a", None, "1.1.1.1"));
        log.record(entry(200, "u", "a", None, "1.1.1.1"));
        log.record(entry(300, "u", "a", None, "1.1.1.1"));

        let in_range = log.filter(&AuditFilter { from_ms: Some(150), to_ms: Some(250), ..Default::default() });
        assert_eq!(in_range.len(), 1);
        assert_eq!(in_range[0].ts_ms, 200);

        let none = log.filter(&AuditFilter { from_ms: Some(400), to_ms: Some(500), ..Default::default() });
        assert!(none.is_empty());
    }

    #[test]
    fn prune_removes_older_than_retention() {
        let mut log = AuditLog::new();
        // now = 10000ms; retention 1d = 86400000ms → cutoff ≈ -86390000 (все проходят).
        // Возьмём now = 100000000ms, retention 1d.
        let now = 100_000_000u64;
        log.record(entry(1, "u", "old", None, "1.1.1.1"));
        log.record(entry(now - 1000, "u", "new", None, "1.1.1.1"));
        let removed = log.prune(now, 1);
        assert_eq!(removed, 1);
        assert_eq!(log.len(), 1);
        assert_eq!(log.all()[0].ts_ms, now - 1000);
    }

    #[test]
    fn prune_2_year_retention() {
        let mut log = AuditLog::new();
        let now = 1_000_000_000_000u64; // ~31.7 years
        let two_years_ms = 730 * 24 * 60 * 60 * 1000;
        log.record(entry(now - two_years_ms - 1, "u", "too_old", None, "1.1.1.1"));
        log.record(entry(now - two_years_ms + 1, "u", "recent", None, "1.1.1.1"));
        let removed = log.prune(now, 730);
        assert_eq!(removed, 1);
        assert_eq!(log.len(), 1);
        assert_eq!(log.all()[0].action, "recent");
    }

    #[test]
    fn export_csv_header_and_rows() {
        let mut log = AuditLog::new();
        log.record(entry(100, "alice", "room.create", None, "10.0.0.1"));
        log.record(entry(200, "bob", "room.delete", Some("r,1"), "10.0.0.2"));
        let csv = log.export_csv(&AuditFilter::default());
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "ts_ms,user,action,room,ip");
        assert_eq!(lines[1], "100,alice,room.create,,10.0.0.1");
        // Комната с запятой → в кавычках.
        assert_eq!(lines[2], "200,bob,room.delete,\"r,1\",10.0.0.2");
    }

    #[test]
    fn export_csv_escapes_quotes() {
        let mut log = AuditLog::new();
        log.record(entry(1, "a\"b", "act", None, "1.1.1.1"));
        let csv = log.export_csv(&AuditFilter::default());
        assert!(csv.contains("\"a\"\"b\""));
    }

    #[test]
    fn export_csv_respects_filter() {
        let mut log = AuditLog::new();
        log.record(entry(1, "alice", "a", None, "1.1.1.1"));
        log.record(entry(2, "bob", "a", None, "1.1.1.1"));
        let csv = log.export_csv(&AuditFilter { user: Some("bob".into()), ..Default::default() });
        assert!(!csv.contains("alice"));
        assert!(csv.contains("bob"));
    }
}
