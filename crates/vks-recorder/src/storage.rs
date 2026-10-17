//! Хранилище метаданных записей: абстракция [`RecordingStore`] + [`MemoryStore`].
//!
//! Источник: `DEVELOPMENT_PLAN.md` Фаза 7 (`storage.rs` — S3/локальный),
//! `docs/rules/BACKEND_RULES.md` §3.3 (retention 90 дней, auto-delete).
//!
//! Ядро работает только с метаданными ([`RecordingMeta`]); бинарный
//! контент (MP4/chunks) хранится отдельно и привязывается к
//! [`RecordingMeta::id`]. Реальные реализации (S3 + postgres, локальный
//! диск) добавляются за фичами позже; в этом крейте — in-memory
//! реализация для тестов.

use std::collections::HashMap;

use crate::recording::RecordingMeta;

/// Операции хранения метаданных записей.
///
/// Реализуется любым бэкендом (in-memory, S3 + postgres, локальный диск).
/// Все методы принимают/возвращают только [`RecordingMeta`] — бинарные
/// данные (MP4/chunks) хранятся отдельно и привязываются по `id`.
pub trait RecordingStore {
    /// Сохраняет метаданные записи (создаёт или перезаписывает по `id`).
    fn save(&mut self, meta: RecordingMeta);

    /// Возвращает запись по `id` (или `None`).
    fn get(&self, id: &uuid::Uuid) -> Option<RecordingMeta>;

    /// Возвращает все записи для `room_id` (порядок не гарантируется).
    fn list(&self, room_id: &uuid::Uuid) -> Vec<RecordingMeta>;

    /// Удаляет записи, у которых `started_at_ms < before_ms`; возвращает
    /// количество удалённых (retention auto-delete job).
    fn purge_expired(&mut self, before_ms: u64) -> usize;

    /// Возвращает все записи хранилища. По умолчанию — пустой список
    /// (консервативный default; бэкенды с полным перечислением
    /// переопределяют). Используется retention-ядром
    /// ([`select_for_purge`](crate::retention::select_for_purge)).
    fn list_all(&self) -> Vec<RecordingMeta> {
        Vec::new()
    }
}

/// In-memory реализация [`RecordingStore`] для тестов.
#[derive(Debug, Default)]
pub struct MemoryStore {
    by_id: HashMap<uuid::Uuid, RecordingMeta>,
}

impl MemoryStore {
    /// Создаёт пустое хранилище.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Количество хранимых записей.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Пустое ли хранилище.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

impl RecordingStore for MemoryStore {
    fn save(&mut self, meta: RecordingMeta) {
        self.by_id.insert(meta.id, meta);
    }

    fn get(&self, id: &uuid::Uuid) -> Option<RecordingMeta> {
        self.by_id.get(id).cloned()
    }

    fn list(&self, room_id: &uuid::Uuid) -> Vec<RecordingMeta> {
        self.by_id.values().filter(|m| &m.room_id == room_id).cloned().collect()
    }

    fn purge_expired(&mut self, before_ms: u64) -> usize {
        let to_remove: Vec<uuid::Uuid> =
            self.by_id.values().filter(|m| m.started_at_ms < before_ms).map(|m| m.id).collect();
        let count = to_remove.len();
        for id in to_remove {
            self.by_id.remove(&id);
        }
        count
    }

    fn list_all(&self) -> Vec<RecordingMeta> {
        self.by_id.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::RecordingMode;
    use uuid::Uuid;

    fn meta(id: Uuid, room: Uuid, started: u64) -> RecordingMeta {
        RecordingMeta::new(id, room, RecordingMode::Mcu, started, started + 1_000, 1, 100)
    }

    #[test]
    fn save_and_get_roundtrip() {
        let mut store = MemoryStore::new();
        let id = Uuid::new_v4();
        let room = Uuid::new_v4();
        store.save(meta(id, room, 100));
        let got = store.get(&id).expect("present");
        assert_eq!(got.id, id);
        assert_eq!(got.room_id, room);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let store = MemoryStore::new();
        assert!(store.get(&Uuid::new_v4()).is_none());
    }

    #[test]
    fn list_filters_by_room() {
        let mut store = MemoryStore::new();
        let room_a = Uuid::new_v4();
        let room_b = Uuid::new_v4();
        store.save(meta(Uuid::new_v4(), room_a, 100));
        store.save(meta(Uuid::new_v4(), room_a, 200));
        store.save(meta(Uuid::new_v4(), room_b, 300));
        assert_eq!(store.list(&room_a).len(), 2);
        assert_eq!(store.list(&room_b).len(), 1);
        assert!(store.list(&Uuid::new_v4()).is_empty());
    }

    #[test]
    fn purge_expired_removes_only_old() {
        let mut store = MemoryStore::new();
        let room = Uuid::new_v4();
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();
        store.save(meta(old_id, room, 100));
        store.save(meta(new_id, room, 500));
        let removed = store.purge_expired(300);
        assert_eq!(removed, 1);
        assert!(store.get(&old_id).is_none());
        assert!(store.get(&new_id).is_some());
    }

    #[test]
    fn save_is_upsert() {
        let mut store = MemoryStore::new();
        let id = Uuid::new_v4();
        let room = Uuid::new_v4();
        store.save(meta(id, room, 100));
        store.save(meta(id, room, 200));
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&id).unwrap().started_at_ms, 200);
    }

    #[test]
    fn list_all_returns_everything() {
        let mut store = MemoryStore::new();
        store.save(meta(Uuid::new_v4(), Uuid::new_v4(), 100));
        store.save(meta(Uuid::new_v4(), Uuid::new_v4(), 200));
        assert_eq!(store.list_all().len(), 2);
    }

    #[test]
    fn empty_store_is_empty() {
        let store = MemoryStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }
}
