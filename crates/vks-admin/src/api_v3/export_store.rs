//! Bounded TTL-хранилище сгенерированных архивов (история/журнал событий).
//!
//! Имена содержат случайный токен, но карта всё равно ограничена: без cap/TTL
//! каждая выгрузка навсегда занимала бы память процесса. Архивы хранятся как
//! `Arc<[u8]>`, поэтому отдача статики не копирует многомегабайтный ZIP.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Entry {
    bytes: Arc<[u8]>,
    created: Instant,
}

#[derive(Debug)]
pub struct ExportStore {
    entries: HashMap<String, Entry>,
    cap: usize,
    ttl: Duration,
}

impl Default for ExportStore {
    fn default() -> Self {
        Self::new(32, Duration::from_secs(3600))
    }
}

impl ExportStore {
    pub fn new(cap: usize, ttl: Duration) -> Self {
        Self { entries: HashMap::new(), cap: cap.max(1), ttl }
    }

    /// Кладёт архив, вытесняя истёкшие и (при переполнении) самый старый файл.
    pub fn insert(&mut self, filename: String, bytes: Vec<u8>) {
        let now = Instant::now();
        self.entries.retain(|_, entry| now.duration_since(entry.created) < self.ttl);
        if !self.entries.contains_key(&filename) && self.entries.len() >= self.cap {
            if let Some(oldest) =
                self.entries.iter().min_by_key(|(_, entry)| entry.created).map(|(name, _)| name.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(filename, Entry { bytes: Arc::from(bytes), created: now });
    }

    /// Возвращает архив по имени; истёкшие (по TTL) записи не отдаются.
    pub fn get(&mut self, filename: &str) -> Option<Arc<[u8]>> {
        let now = Instant::now();
        self.entries.retain(|_, entry| now.duration_since(entry.created) < self.ttl);
        self.entries.get(filename).map(|entry| Arc::clone(&entry.bytes))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_only_oldest_when_cap_exceeded() {
        let mut store = ExportStore::new(2, Duration::from_secs(60));
        store.insert("a".into(), b"a".to_vec());
        store.insert("b".into(), b"b".to_vec());
        store.insert("c".into(), b"c".to_vec());
        assert!(store.get("a").is_none(), "oldest file evicted");
        assert!(store.get("b").is_some() && store.get("c").is_some());
    }

    #[test]
    fn expired_files_are_not_served() {
        let mut store = ExportStore::new(8, Duration::from_secs(0));
        store.insert("a".into(), b"a".to_vec());
        assert!(store.get("a").is_none());
    }

    #[test]
    fn replace_same_name_does_not_evict_other() {
        let mut store = ExportStore::new(2, Duration::from_secs(60));
        store.insert("a".into(), b"1".to_vec());
        store.insert("b".into(), b"2".to_vec());
        store.insert("a".into(), b"3".to_vec());
        assert_eq!(store.get("a").unwrap().as_ref(), b"3");
        assert!(store.get("b").is_some());
    }
}
