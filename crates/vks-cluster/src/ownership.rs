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

/// Описание лиза владения комнатой.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// Идентификатор инстанса-владельца.
    pub owner: String,
    /// Монотонный тик истечения лиза.
    pub expiry_tick: u64,
    /// Монотонная эпоха владения (fencing token).
    pub epoch: u64,
}

/// Ошибки проверки fencing (ограждения).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FencingError {
    #[error("комната {0} не имеет активного лиза")]
    NoLease(String),
    #[error("инстанс {actual} не является владельцем комнаты {room_id} (текущий владелец {expected})")]
    NotOwner { room_id: String, expected: String, actual: String },
    #[error("устаревшая эпоха {provided} для комнаты {room_id} (текущая {current})")]
    StaleEpoch { room_id: String, current: u64, provided: u64 },
    #[error("лиз комнаты {room_id} истек на тике {expiry_tick} (текущий {now_tick})")]
    LeaseExpired { room_id: String, expiry_tick: u64, now_tick: u64 },
}

/// Реестр ownership rooms.
pub struct RoomOwner {
    /// instance_id → tick его последней registration.
    instances: HashMap<String, u64>,
    /// room_id → Lease.
    leases: HashMap<String, Lease>,
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
            self.leases.iter().filter(|(_, lease)| lease.owner == instance_id).map(|(room, _)| room.clone()).collect();
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

    /// Выдаёт lease для room с монотонным сроком `now_tick + ttl_ticks`.
    ///
    /// - Если room не имеет лиза или предыдущий лиз ИСТЁК (`now_tick >= expiry`):
    ///   выдаётся новый лиз с инкрементом epoch.
    /// - Если активный лиз принадлежит caller: продление (renew) без смены epoch.
    /// - Если активный лиз принадлежит другому: split-brain (lowest instance_id wins).
    pub fn acquire_lease(&mut self, room_id: &str, instance_id: &str, now_tick: u64, ttl_ticks: u64) -> LeaseResult {
        self.register(instance_id, now_tick);
        let cur = self.leases.get(room_id).cloned();

        match cur {
            None => {
                let expiry = now_tick.saturating_add(ttl_ticks);
                self.leases.insert(
                    room_id.to_string(),
                    Lease { owner: instance_id.to_string(), expiry_tick: expiry, epoch: 1 },
                );
                LeaseResult::Acquired
            }
            Some(lease) => {
                if now_tick >= lease.expiry_tick {
                    // Предыдущий лиз истёк: старый владелец теряет права независимо от ID.
                    let new_epoch = lease.epoch.saturating_add(1);
                    let expiry = now_tick.saturating_add(ttl_ticks);
                    self.leases.insert(
                        room_id.to_string(),
                        Lease { owner: instance_id.to_string(), expiry_tick: expiry, epoch: new_epoch },
                    );
                    LeaseResult::Acquired
                } else if lease.owner == instance_id {
                    // Renew активного лиза тем же владельцем: epoch сохраняется.
                    let new_expiry = now_tick.saturating_add(ttl_ticks);
                    self.leases.insert(
                        room_id.to_string(),
                        Lease { owner: instance_id.to_string(), expiry_tick: new_expiry, epoch: lease.epoch },
                    );
                    LeaseResult::Acquired
                } else {
                    // Split-brain среди активных претендентов: lowest instance_id wins.
                    let winner = instance_id.min(&lease.owner);
                    if winner == instance_id {
                        let new_epoch = lease.epoch.saturating_add(1);
                        let new_expiry = now_tick.saturating_add(ttl_ticks);
                        self.leases.insert(
                            room_id.to_string(),
                            Lease { owner: instance_id.to_string(), expiry_tick: new_expiry, epoch: new_epoch },
                        );
                        LeaseResult::Acquired
                    } else {
                        LeaseResult::OwnedBy(lease.owner)
                    }
                }
            }
        }
    }

    /// Обновляет (продлевает) lease до `now_tick + ttl_ticks`.
    pub fn renew_lease(&mut self, room_id: &str, instance_id: &str, now_tick: u64, ttl_ticks: u64) -> LeaseResult {
        self.acquire_lease(room_id, instance_id, now_tick, ttl_ticks)
    }

    /// Освобождает lease (caller должен быть owner).
    pub fn release(&mut self, room_id: &str, instance_id: &str) -> bool {
        let cur = self.leases.get(room_id).cloned();
        match cur {
            Some(lease) if lease.owner == instance_id => {
                self.leases.remove(room_id);
                true
            }
            _ => false,
        }
    }

    /// Возвращает список комнат, принадлежащих данному instance.
    pub fn rooms_for(&self, instance_id: &str) -> Vec<String> {
        let mut rooms: Vec<String> =
            self.leases.iter().filter(|(_, lease)| lease.owner == instance_id).map(|(room, _)| room.clone()).collect();
        rooms.sort();
        rooms
    }

    /// Возвращает текущего owner room (даже если истёк).
    pub fn owner(&self, room_id: &str) -> Option<&str> {
        self.leases.get(room_id).map(|l| l.owner.as_str())
    }

    /// Возвращает активного владельца на now_tick (None, если лиз истёк).
    pub fn active_owner(&self, room_id: &str, now_tick: u64) -> Option<&str> {
        self.leases.get(room_id).and_then(|l| if now_tick < l.expiry_tick { Some(l.owner.as_str()) } else { None })
    }

    /// Текущая эпоха лиза комнаты.
    pub fn epoch(&self, room_id: &str) -> Option<u64> {
        self.leases.get(room_id).map(|l| l.epoch)
    }

    /// Тик истечения lease (None если room не имеет lease).
    pub fn lease_expiry(&self, room_id: &str) -> Option<u64> {
        self.leases.get(room_id).map(|l| l.expiry_tick)
    }

    /// Проверяет, истёк ли lease room на now_tick.
    pub fn lease_expired(&self, room_id: &str, now_tick: u64) -> bool {
        match self.leases.get(room_id) {
            Some(lease) => now_tick >= lease.expiry_tick,
            None => false,
        }
    }

    /// Проверяет fencing-токен перед выполнением мутации состояния комнаты.
    pub fn check_fencing(
        &self,
        room_id: &str,
        instance_id: &str,
        epoch: u64,
        now_tick: u64,
    ) -> Result<(), FencingError> {
        let lease = self.leases.get(room_id).ok_or_else(|| FencingError::NoLease(room_id.to_string()))?;

        if lease.owner != instance_id {
            return Err(FencingError::NotOwner {
                room_id: room_id.to_string(),
                expected: lease.owner.clone(),
                actual: instance_id.to_string(),
            });
        }

        if now_tick >= lease.expiry_tick {
            return Err(FencingError::LeaseExpired {
                room_id: room_id.to_string(),
                expiry_tick: lease.expiry_tick,
                now_tick,
            });
        }

        if lease.epoch != epoch {
            return Err(FencingError::StaleEpoch {
                room_id: room_id.to_string(),
                current: lease.epoch,
                provided: epoch,
            });
        }

        Ok(())
    }

    /// Resolves split-brain для room: winner = lowest instance_id из
    /// `claimants`; loser'ы освобождают room (CLUSTERING.md §2).
    /// Winner получает новый lease со сдвигом эпохи.
    pub fn resolve_split_brain(
        &mut self,
        room_id: &str,
        claimants: &[String],
        now_tick: u64,
        ttl_ticks: u64,
    ) -> Option<SplitBrainResult> {
        if claimants.len() < 2 {
            return None;
        }
        let winner = claimants.iter().min().cloned()?;
        let prev_epoch = self.leases.get(room_id).map(|l| l.epoch).unwrap_or(0);
        let new_epoch = prev_epoch.saturating_add(1);
        let expiry = now_tick.saturating_add(ttl_ticks);

        self.leases.insert(room_id.to_string(), Lease { owner: winner.clone(), expiry_tick: expiry, epoch: new_epoch });
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
        let r = ro.acquire_lease("room-1", "i-1", 0, 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
        assert_eq!(ro.epoch("room-1"), Some(1));
        assert_eq!(ro.lease_expiry("room-1"), Some(30));
    }

    #[test]
    fn acquire_lease_owned_by_other() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        // i-2 claim'ит room i-1 пока лиз активен → split-brain: lowest wins → i-1 выигрывает.
        let r = ro.acquire_lease("room-1", "i-2", 10, 30);
        assert_eq!(r, LeaseResult::OwnedBy("i-1".to_string()));
    }

    #[test]
    fn split_brain_lowest_wins() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-2", 0, 30);
        // i-1 (lower) claim'ит активный лиз → выигрывает со сдвигом эпохи.
        let r = ro.acquire_lease("room-1", "i-1", 5, 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
        assert_eq!(ro.epoch("room-1"), Some(2));
        assert_eq!(ro.lease_expiry("room-1"), Some(35));
    }

    #[test]
    fn renew_lease_same_owner() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        let r = ro.renew_lease("room-1", "i-1", 10, 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
        assert_eq!(ro.epoch("room-1"), Some(1)); // Эпоха не меняется при штатном продлении
        assert_eq!(ro.lease_expiry("room-1"), Some(40)); // 10 + 30
    }

    #[test]
    fn renew_lease_other_lower_wins() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-2", 0, 30);
        // i-1 (lower) renew'ит → забирает со сдвигом эпохи.
        let r = ro.renew_lease("room-1", "i-1", 5, 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
        assert_eq!(ro.epoch("room-1"), Some(2));
    }

    #[test]
    fn renew_lease_other_higher_loses() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        // i-2 (higher) renew'ит пока активен i-1 → loses.
        let r = ro.renew_lease("room-1", "i-2", 10, 30);
        assert_eq!(r, LeaseResult::OwnedBy("i-1".to_string()));
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn release_own_lease() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        assert!(ro.release("room-1", "i-1"));
        assert_eq!(ro.owner("room-1"), None);
    }

    #[test]
    fn release_not_owner_fails() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        assert!(!ro.release("room-1", "i-2"));
        assert_eq!(ro.owner("room-1"), Some("i-1"));
    }

    #[test]
    fn lease_expiry_tracked() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        assert_eq!(ro.lease_expiry("room-1"), Some(30));
        assert!(!ro.lease_expired("room-1", 29));
        assert!(ro.lease_expired("room-1", 30));
        assert_eq!(ro.active_owner("room-1", 29), Some("i-1"));
        assert_eq!(ro.active_owner("room-1", 30), None);
    }

    #[test]
    fn now100_ttl30_expiry_at_130() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 100, 30);
        assert_eq!(ro.lease_expiry("room-1"), Some(130));
        assert!(!ro.lease_expired("room-1", 129));
        assert!(ro.lease_expired("room-1", 130));
    }

    #[test]
    fn expired_lower_id_owner_can_be_claimed_by_higher_id() {
        let mut ro = RoomOwner::new();
        // i-1 (меньший ID) берёт лиз на тике 0 до 30.
        ro.acquire_lease("room-1", "i-1", 0, 30);
        assert_eq!(ro.owner("room-1"), Some("i-1"));
        assert_eq!(ro.epoch("room-1"), Some(1));

        // На тике 100 лиз i-1 давно истёк! i-2 (больший ID) пытается забрать комнату.
        // Он ДОЛЖЕН успешно её получить со сдвигом эпохи (dead owner не блокирует).
        let r = ro.acquire_lease("room-1", "i-2", 100, 30);
        assert_eq!(r, LeaseResult::Acquired);
        assert_eq!(ro.owner("room-1"), Some("i-2"));
        assert_eq!(ro.epoch("room-1"), Some(2));
        assert_eq!(ro.lease_expiry("room-1"), Some(130));
    }

    #[test]
    fn fencing_epoch_and_validation() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);

        // Штатная проверка владельца с валидной эпохой на тике 10.
        assert!(ro.check_fencing("room-1", "i-1", 1, 10).is_ok());

        // Неверная эпоха.
        assert_eq!(
            ro.check_fencing("room-1", "i-1", 2, 10),
            Err(FencingError::StaleEpoch { room_id: "room-1".into(), current: 1, provided: 2 })
        );

        // Чужой инстанс.
        assert_eq!(
            ro.check_fencing("room-1", "i-2", 1, 10),
            Err(FencingError::NotOwner { room_id: "room-1".into(), expected: "i-1".into(), actual: "i-2".into() })
        );

        // Истёкший лиз (тик 35 >= 30).
        assert_eq!(
            ro.check_fencing("room-1", "i-1", 1, 35),
            Err(FencingError::LeaseExpired { room_id: "room-1".into(), expiry_tick: 30, now_tick: 35 })
        );
    }

    #[test]
    fn resolve_split_brain_lowest() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-2", 0, 30);
        let r = ro.resolve_split_brain("room-1", &["i-2".into(), "i-1".into()], 5, 30);
        assert_eq!(r, Some(SplitBrainResult::Winner("i-1".to_string())));
        assert_eq!(ro.owner("room-1"), Some("i-1"));
        assert_eq!(ro.epoch("room-1"), Some(2));
        assert_eq!(ro.lease_expiry("room-1"), Some(35));
    }

    #[test]
    fn resolve_split_brain_single_claimant() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        assert_eq!(ro.resolve_split_brain("room-1", &["i-1".into()], 0, 30), None);
    }

    #[test]
    fn unregister_releases_leases() {
        let mut ro = RoomOwner::new();
        ro.acquire_lease("room-1", "i-1", 0, 30);
        ro.unregister("i-1");
        assert_eq!(ro.owner("room-1"), None);
    }
}
