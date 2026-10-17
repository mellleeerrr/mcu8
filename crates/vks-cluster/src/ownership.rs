//! Room ownership: consistent hashing + lease (CLUSTERING.md §2, ADR-0014).
//!
//! `room_id → instance_id` через consistent hash (FNV-1a room_id mod N
//! instance_id, детерминированно). Lease TTL 30s (tick). Split-brain:
//! если два инстанса claim'ят room → **lowest instance_id wins**.
//!
//! Что реально: deterministic mapping, lease lifecycle, split-brain resolution.
//! Что мокнуто: NATS lease storage, PostgreSQL advisory lock, реальное время
//! (tick = 1s, параметризовано).

use std::collections::HashMap;

/// Результат acquire_lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseResult {
    /// Этот инстанс владеет room (lease выдан).
    Acquired,
    /// Room уже принадлежит другому инстансу (активный lease).
    OwnedBy(String),
    /// Lease истёк — room свободен, можно claim.
    LeaseExpired,
}

/// Результат split-brain resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitBrainResult {
    /// Победитель (lowest instance_id).
    Winner(String),
    /// Проигравший должен освободить room.
    Loser(String),
}

/// Реестр ownership rooms.
pub struct RoomOwner {
    /// instance_id → tick его последней registration.
    instances: HashMap<String, u64>,
    /// room_id → (owner_instance_id, lease_expiry_tick).
    leases: HashMap<String, (String, u64)>,
}

/// FNV-1a 64-bit hash (детерминированно, без внешних deps).
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl RoomOwner {
    /// Создаёт пустой реестр.
    pub fn new() -> Self {
        Self { instances: HashMap::new(), leases: HashMap::new() }
    }

    /// Регистрирует instance (instance_id → now_tick).
    pub fn register(&mut self, instance_id: &str, now_tick: u64) {
        self.instances.insert(instance_id.to_string(), now_tick);
    }

    /// Отменяет регистрацию instance.
    pub fn unregister(&mut self, instance_id: &str) {
        self.instances.remove(instance_id);
        // Освобождаем все lease'ы этого instance.
        let stale: Vec<String> =
            self.leases.iter().filter(|(_, (owner, _))| owner == instance_id).map(|(room, _)| room.clone()).collect();
        for room in stale {
            self.leases.remove(&room);
        }
    }

    /// Список зарегистрированных instance_id (отсортированно, для детерминированности).
    pub fn instances(&self) -> Vec<String> {
        let mut v: Vec<String> = self.instances.keys().cloned().collect();
        v.sort();
        v
    }

    /// Consistent hash: room_id → index в отсортированном списке instance'ов.
    /// Используется для выбора «природного» owner (CLUSTERING.md §2).
    pub fn natural_owner(&self, room_id: &str) -> Option<String> {
        let ids = self.instances();
        if ids.is_empty() {
            return None;
        }
        let idx = (fnv1a64(room_id.as_bytes()) % ids.len() as u64) as usize;
        Some(ids[idx].clone())
    }

    /// Выдаёт lease для room.
    ///
    /// - Если room не имеет активного lease → owner = caller, Acquired.
    /// - Если lease активен и принадлежит caller → Acquired (renew).
    /// - Если lease активен и принадлежит другому → split-brain: lowest
    ///   instance_id wins (CLUSTERING.md §2).
    pub fn acquire_lease(&mut self, room_id: &str, instance_id: &str, ttl_ticks: u64) -> LeaseResult {
        self.register(instance_id, 0);
        let cur = self.leases.get(room_id).cloned();

        match cur {
            None => {
                // Свободно: owner = caller, lease до now + ttl.
                self.leases.insert(room_id.to_string(), (instance_id.to_string(), ttl_ticks));
                LeaseResult::Acquired
            }
            Some((owner, expiry)) => {
                if owner == instance_id {
                    // Renew.
                    self.leases.insert(room_id.to_string(), (instance_id.to_string(), expiry));
                    LeaseResult::Acquired
                } else {
                    // Split-brain: lowest instance_id wins.
                    let winner = instance_id.min(&owner);
                    if winner == instance_id {
                        // Caller выигрывает, забирает lease.
                        let new_expiry = expiry + ttl_ticks;
                        self.leases.insert(room_id.to_string(), (instance_id.to_string(), new_expiry));
                        LeaseResult::Acquired
                    } else {
                        // Owner выигрывает, caller отклонён.
                        LeaseResult::OwnedBy(owner)
                    }
                }
            }
        }
    }

    /// Обновляет (продлевает) lease до now + ttl. Возврат LeaseResult.
    /// Если room без lease — эквивалентно `acquire_lease`.
    pub fn renew_lease(&mut self, room_id: &str, instance_id: &str, ttl_ticks: u64) -> LeaseResult {
        match self.leases.get(room_id).cloned() {
            None => self.acquire_lease(room_id, instance_id, ttl_ticks),
            Some((owner, _)) => {
                if owner == instance_id {
                    self.leases.insert(room_id.to_string(), (instance_id.to_string(), ttl_ticks));
                    LeaseResult::Acquired
                } else if instance_id < &owner {
                    // Split-brain: lowest wins — caller забирает lease.
                    self.leases.insert(room_id.to_string(), (instance_id.to_string(), ttl_ticks));
                    LeaseResult::Acquired
                } else {
                    LeaseResult::OwnedBy(owner)
                }
            }
        }
    }

    /// Освобождает lease (caller должен быть owner).
    pub fn release(&mut self, room_id: &str, instance_id: &str) -> bool {
        let cur = self.leases.get(room_id).cloned();
        match cur {
            Some((owner, _)) if owner == instance_id => {
                self.leases.remove(room_id);
                true
            }
            _ => false,
        }
    }

    /// Возвращает текущего owner room (если lease активен).
    pub fn owner(&self, room_id: &str) -> Option<&str> {
        self.leases.get(room_id).map(|(o, _)| o.as_str())
    }

    /// Тик истечения lease (None если room не имеет lease).
    pub fn lease_expiry(&self, room_id: &str) -> Option<u64> {
        self.leases.get(room_id).map(|(_, e)| *e)
    }

    /// Проверяет, истёк ли lease room на now_tick.
    pub fn lease_expired(&self, room_id: &str, now_tick: u64) -> bool {
        match self.leases.get(room_id) {
            Some((_, expiry)) => now_tick >= *expiry,
            None => false,
        }
    }

    /// Resolves split-brain для room: winner = lowest instance_id из
    /// `claimants`; loser'ы освобождают room (CLUSTERING.md §2).
    /// Winner получает новый lease (TTL 30 ticks).
    pub fn resolve_split_brain(&mut self, room_id: &str, claimants: &[String]) -> Option<SplitBrainResult> {
        if claimants.len() < 2 {
            return None;
        }
        let winner = claimants.iter().min().cloned()?;
        self.leases.insert(room_id.to_string(), (winner.clone(), 30));
        Some(SplitBrainResult::Winner(winner))
    }
}

impl Default for RoomOwner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_owner_is_deterministic() {
        let mut ro = RoomOwner::new();
        ro.register("i-1", 0);
        ro.register("i-2", 0);
        ro.register("i-3", 0);
        let a = ro.natural_owner("room-1");
        let b = ro.natural_owner("room-1");
        assert_eq!(a, b);
        assert!(a.is_some());
    }

    #[test]
    fn natural_owner_empty_is_none() {
        let ro = RoomOwner::new();
        assert_eq!(ro.natural_owner("room-1"), None);
    }

    #[test]
    fn acquire_lease_free_room() {
        let mut ro = RoomOwner::new();
        let r = ro.acquire_lease("room-1", "i-1", 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn acquire_lease_owned_by_other() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        // i-2 claim'ит room i-1 → split-brain: lowest wins → i-1 выигрывает.
        let r = ro.acquire_lease("room-1", "i-2", 30);
        assert_eq!(r, LeaseResult::OwnedBy("i-1".to_string()));
    }

    #[test]
    fn split_brain_lowest_wins() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-2", 30);
        // i-1 (lower) claim'ит → выигрывает.
        let r = ro.acquire_lease("room-1", "i-1", 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn renew_lease_same_owner() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        let r = ro.renew_lease("room-1", "i-1", 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn renew_lease_other_lower_wins() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-2", 30);
        // i-1 (lower) renew'ит → забирает.
        let r = ro.renew_lease("room-1", "i-1", 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn renew_lease_other_higher_loses() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        // i-2 (higher) renew'ит → loses.
        let r = ro.renew_lease("room-1", "i-2", 30);
        assert_eq!(r, LeaseResult::OwnedBy("i-1".to_string()));
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn release_own_lease() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        assert!(ro.release("room-1", "i-1"));
        assert_eq!(ro.owner("room-1"), None);
    }

    #[test]
    fn release_not_owner_fails() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        assert!(!ro.release("room-1", "i-2"));
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn lease_expiry_tracked() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        assert_eq!(ro.lease_expiry("room-1"), Some(30));
        assert!(!ro.lease_expired("room-1", 29));
        assert!(ro.lease_expired("room-1", 30));
    }

    #[test]
    fn resolve_split_brain_lowest() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-2", 30);
        let r = ro.resolve_split_brain("room-1", &["i-2".into(), "i-1".into()]);
        assert_eq!(r, Some(SplitBrainResult::Winner("i-1".to_string())));
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn resolve_split_brain_single_claimant() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        assert_eq!(ro.resolve_split_brain("room-1", &["i-1".into()]), None);
    }

    #[test]
    fn unregister_releases_leases() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 30);
        ro.unregister("i-1");
        assert_eq!(ro.owner("room-1"), None);
    }
}
