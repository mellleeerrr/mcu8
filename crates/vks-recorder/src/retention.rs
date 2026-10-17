//! Политика хранения записей: [`RetentionPolicy`].
//!
//! Источник: `DEVELOPMENT_PLAN.md` Фаза 7 (retention 90 дней, auto-delete),
//! `docs/rules/BACKEND_RULES.md` §3.3 (retention 90 дней, настраиваемо)
//! и §5.3 (GDPR/152-ФЗ: recordings 90 дней).
//!
//! Ядро определяет «что удалить» (возвращает [`Vec<RecordingMeta>`]);
//! само удаление выполняет
//! [`RecordingStore::purge_expired`](crate::storage::RecordingStore::purge_expired).
//!
//! Два критерия (независимые, OR):
//! - по возрасту: `started_at_ms < now_ms - max_age_ms`;
//! - по количеству: в комнате больше `max_rooms` записей — самые старые
//!   (по `started_at_ms`) подлежат удалению.

use crate::recording::RecordingMeta;
use crate::storage::RecordingStore;
use uuid::Uuid;

/// Milliseconds in one day (for days → ms conversion).
pub const MS_PER_DAY: u64 = 86_400_000;

/// Retention policy for recordings.
///
/// `max_age_days` — max recording age in days (0 = no age limit).
/// `max_rooms` — max recordings per room (0 = no count limit).
/// Defaults match BACKEND_RULES §3.3 (90 days) and a sensible per-room
/// bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Max age in days (0 = no limit).
    pub max_age_days: u32,
    /// Max recordings per room (0 = no limit).
    pub max_rooms: usize,
}

impl RetentionPolicy {
    /// Creates a policy.
    #[must_use]
    pub const fn new(max_age_days: u32, max_rooms: usize) -> Self {
        Self { max_age_days, max_rooms }
    }

    /// Default policy (BACKEND_RULES §3.3: 90 days; 10 recordings per room).
    #[must_use]
    pub const fn default_90d() -> Self {
        Self::new(90, 10)
    }

    /// Converts `max_age_days` to milliseconds.
    #[must_use]
    pub const fn max_age_ms(self) -> u64 {
        self.max_age_days as u64 * MS_PER_DAY
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::default_90d()
    }
}

/// Returns `true` if the recording is expired by age.
///
/// Expired when `started_at_ms < now_ms - max_age_ms`. A policy with
/// `max_age_days == 0` never expires by age.
#[must_use]
pub fn is_expired_by_age(meta: &RecordingMeta, now_ms: u64, policy: &RetentionPolicy) -> bool {
    if policy.max_age_days == 0 {
        return false;
    }
    let threshold = now_ms.saturating_sub(policy.max_age_ms());
    meta.started_at_ms < threshold
}

/// Returns `true` if the recording is expired by count: `max_rooms > 0`
/// and `meta` is among the oldest `(len - max_rooms)` recordings in its
/// room.
///
/// `all_in_room` — all recordings of the room (full, unfiltered list).
#[must_use]
pub fn is_expired_by_count(meta: &RecordingMeta, all_in_room: &[RecordingMeta], policy: &RetentionPolicy) -> bool {
    if policy.max_rooms == 0 || all_in_room.is_empty() {
        return false;
    }
    let mut sorted: Vec<&RecordingMeta> = all_in_room.iter().collect();
    sorted.sort_by_key(|m| m.started_at_ms);
    let excess = sorted.len().saturating_sub(policy.max_rooms);
    if excess == 0 {
        return false;
    }
    for candidate in sorted.iter().take(excess) {
        if candidate.id == meta.id {
            return true;
        }
    }
    false
}

/// Returns `true` if the recording is expired by either criterion.
#[must_use]
pub fn is_expired(meta: &RecordingMeta, now_ms: u64, all_in_room: &[RecordingMeta], policy: &RetentionPolicy) -> bool {
    is_expired_by_age(meta, now_ms, policy) || is_expired_by_count(meta, all_in_room, policy)
}

/// Selects recordings to purge from the store, applying the policy.
///
/// Groups the store's recordings by room and applies [`is_expired`] to
/// each. Returns the selected [`RecordingMeta`]s (the caller performs
/// the actual removal, e.g. via
/// [`RecordingStore::purge_expired`](crate::storage::RecordingStore::purge_expired)
/// or per-id deletion).
pub fn select_for_purge(store: &dyn RecordingStore, now_ms: u64, policy: &RetentionPolicy) -> Vec<RecordingMeta> {
    let all = store.list_all();
    let mut rooms: std::collections::HashMap<Uuid, Vec<RecordingMeta>> = std::collections::HashMap::new();
    for meta in all {
        rooms.entry(meta.room_id).or_default().push(meta);
    }
    let mut to_purge = Vec::new();
    for metas in rooms.values() {
        for meta in metas {
            if is_expired(meta, now_ms, metas, policy) {
                to_purge.push(meta.clone());
            }
        }
    }
    to_purge
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::RecordingMode;
    use crate::storage::MemoryStore;

    fn meta(id: Uuid, room: Uuid, started: u64) -> RecordingMeta {
        RecordingMeta::new(id, room, RecordingMode::Mcu, started, started + 1_000, 1, 100)
    }

    const DAY: u64 = MS_PER_DAY;

    #[test]
    fn expired_by_age_when_started_before_threshold() {
        let policy = RetentionPolicy::new(90, 0);
        let now = 100 * DAY;
        let old = meta(Uuid::new_v4(), Uuid::new_v4(), now - 91 * DAY);
        let new = meta(Uuid::new_v4(), Uuid::new_v4(), now - 89 * DAY);
        assert!(is_expired_by_age(&old, now, &policy));
        assert!(!is_expired_by_age(&new, now, &policy));
    }

    #[test]
    fn not_expired_when_max_age_zero() {
        let policy = RetentionPolicy::new(0, 0);
        let now = 1_000 * DAY;
        let old = meta(Uuid::new_v4(), Uuid::new_v4(), 0);
        assert!(!is_expired_by_age(&old, now, &policy));
    }

    #[test]
    fn boundary_exactly_max_age_not_expired() {
        let policy = RetentionPolicy::new(90, 0);
        let now = 100 * DAY;
        let at_boundary = meta(Uuid::new_v4(), Uuid::new_v4(), now - 90 * DAY);
        assert!(!is_expired_by_age(&at_boundary, now, &policy));
    }

    #[test]
    fn expired_by_count_removes_oldest_beyond_max_rooms() {
        let policy = RetentionPolicy::new(0, 2);
        let room = Uuid::new_v4();
        let oldest = meta(Uuid::new_v4(), room, 100);
        let middle = meta(Uuid::new_v4(), room, 200);
        let newest = meta(Uuid::new_v4(), room, 300);
        let all = vec![oldest.clone(), middle.clone(), newest.clone()];
        assert!(is_expired_by_count(&oldest, &all, &policy));
        assert!(!is_expired_by_count(&middle, &all, &policy));
        assert!(!is_expired_by_count(&newest, &all, &policy));
    }

    #[test]
    fn not_expired_by_count_when_within_limit() {
        let policy = RetentionPolicy::new(0, 3);
        let room = Uuid::new_v4();
        let a = meta(Uuid::new_v4(), room, 100);
        let b = meta(Uuid::new_v4(), room, 200);
        let all = vec![a.clone(), b.clone()];
        assert!(!is_expired_by_count(&a, &all, &policy));
        assert!(!is_expired_by_count(&b, &all, &policy));
    }

    #[test]
    fn not_expired_by_count_when_max_zero() {
        let policy = RetentionPolicy::new(0, 0);
        let room = Uuid::new_v4();
        let a = meta(Uuid::new_v4(), room, 100);
        let all = vec![a.clone()];
        assert!(!is_expired_by_count(&a, &all, &policy));
    }

    #[test]
    fn is_expired_uses_either_criterion() {
        let policy = RetentionPolicy::new(90, 0);
        let now = 100 * DAY;
        let old = meta(Uuid::new_v4(), Uuid::new_v4(), now - 91 * DAY);
        assert!(is_expired(&old, now, &[], &policy));
        let fresh = meta(Uuid::new_v4(), Uuid::new_v4(), now);
        assert!(!is_expired(&fresh, now, &[], &policy));
    }

    #[test]
    fn select_for_purge_removes_oldest_in_room_beyond_max_rooms() {
        let room = Uuid::new_v4();
        let mut store = MemoryStore::new();
        let m1 = meta(Uuid::new_v4(), room, 100);
        let m2 = meta(Uuid::new_v4(), room, 200);
        let m3 = meta(Uuid::new_v4(), room, 300);
        store.save(m1.clone());
        store.save(m2.clone());
        store.save(m3.clone());
        let policy = RetentionPolicy::new(0, 2);
        let purge = select_for_purge(&store, 0, &policy);
        assert_eq!(purge.len(), 1);
        assert_eq!(purge[0].id, m1.id);
    }

    #[test]
    fn select_for_purge_removes_old_by_age() {
        let room = Uuid::new_v4();
        let mut store = MemoryStore::new();
        let now = 100 * DAY;
        let old = meta(Uuid::new_v4(), room, now - 91 * DAY);
        let new = meta(Uuid::new_v4(), room, now);
        store.save(old.clone());
        store.save(new.clone());
        let policy = RetentionPolicy::new(90, 0);
        let purge = select_for_purge(&store, now, &policy);
        assert_eq!(purge.len(), 1);
        assert_eq!(purge[0].id, old.id);
    }

    #[test]
    fn select_for_purge_keeps_recent_under_policy() {
        let room = Uuid::new_v4();
        let mut store = MemoryStore::new();
        let now = 100 * DAY;
        let m1 = meta(Uuid::new_v4(), room, now - 10 * DAY);
        let m2 = meta(Uuid::new_v4(), room, now - 5 * DAY);
        store.save(m1);
        store.save(m2);
        let policy = RetentionPolicy::new(90, 10);
        let purge = select_for_purge(&store, now, &policy);
        assert!(purge.is_empty());
    }
}
