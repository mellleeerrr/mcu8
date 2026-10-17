//! Discovery инстансов: heartbeat + «dead» flag (CLUSTERING.md §1, §4).
//!
//! Instance шлёт heartbeat каждые 10 ticks (1 tick = 1s). Если нет heartbeat
//! за 10 ticks → инстанс помечен dead (detected на `check(now_tick)`).
//!
//! Что реально: registry, deadline detection, alive/dead classification.
//! Что мокнуто: NATS heartbeat transport, реальное время (tick parameterized).

use std::collections::HashMap;

/// Тик deadline для heartbeat: инстанс без heartbeat за N ticks → dead.
pub const DEADLINE_TICKS: u64 = 10;

/// Статус инстанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceStatus {
    /// Alive (heartbeat в пределах deadline).
    Alive,
    /// Dead (heartbeat просрочен).
    Dead,
}

/// Реестр инстансов кластера.
pub struct InstanceRegistry {
    /// instance_id → last heartbeat tick.
    heartbeats: HashMap<String, u64>,
    /// Настраиваемый дедлайн (по умолчанию DEADLINE_TICKS).
    deadline_ticks: u64,
}

impl InstanceRegistry {
    /// Создаёт пустой реестр с дефолтным дедлайном (10 тиков).
    pub fn new() -> Self {
        Self { heartbeats: HashMap::new(), deadline_ticks: DEADLINE_TICKS }
    }

    /// Создаёт реестр с настраиваемым дедлайном в тиках.
    pub fn with_deadline(deadline_ticks: u64) -> Self {
        Self { heartbeats: HashMap::new(), deadline_ticks }
    }

    /// Текущий дедлайн в тиках.
    pub fn deadline_ticks(&self) -> u64 {
        self.deadline_ticks
    }

    /// Регистрирует/обновляет heartbeat instance.
    pub fn heartbeat(&mut self, instance_id: &str, now_tick: u64) {
        self.heartbeats.insert(instance_id.to_string(), now_tick);
    }

    /// Убирает instance из реестра.
    pub fn remove(&mut self, instance_id: &str) {
        self.heartbeats.remove(instance_id);
    }

    /// Удаляет все мёртвые instance на now_tick и возвращает отсортированный список удалённых id.
    pub fn prune_dead(&mut self, now_tick: u64) -> Vec<String> {
        let deadline = self.deadline_ticks;
        let mut dead = Vec::new();
        self.heartbeats.retain(|id, last| {
            if now_tick.saturating_sub(*last) >= deadline {
                dead.push(id.clone());
                false
            } else {
                true
            }
        });
        dead.sort();
        dead
    }

    /// Статус instance на now_tick.
    pub fn status(&self, instance_id: &str, now_tick: u64) -> Option<InstanceStatus> {
        let last = self.heartbeats.get(instance_id)?;
        if now_tick.saturating_sub(*last) < self.deadline_ticks {
            Some(InstanceStatus::Alive)
        } else {
            Some(InstanceStatus::Dead)
        }
    }

    /// True если instance registered и alive на now_tick.
    pub fn is_alive(&self, instance_id: &str, now_tick: u64) -> bool {
        matches!(self.status(instance_id, now_tick), Some(InstanceStatus::Alive))
    }

    /// True если instance registered и dead (heartbeat просрочен).
    pub fn is_dead(&self, instance_id: &str, now_tick: u64) -> bool {
        matches!(self.status(instance_id, now_tick), Some(InstanceStatus::Dead))
    }

    /// Все registered instance_id (alive + dead), отсортированно.
    pub fn all(&self) -> Vec<String> {
        let mut v: Vec<String> = self.heartbeats.keys().cloned().collect();
        v.sort();
        v
    }

    /// Все alive instance_id на now_tick (для consistent hashing), отсортированно.
    pub fn alive(&self, now_tick: u64) -> Vec<String> {
        let mut v: Vec<String> = self
            .heartbeats
            .iter()
            .filter(|(_, last)| now_tick.saturating_sub(**last) < self.deadline_ticks)
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    }

    /// Все dead instance_id на now_tick, отсортированно.
    pub fn dead(&self, now_tick: u64) -> Vec<String> {
        let mut v: Vec<String> = self
            .heartbeats
            .iter()
            .filter(|(_, last)| now_tick.saturating_sub(**last) >= self.deadline_ticks)
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    }

    /// Количество alive instance.
    pub fn alive_count(&self, now_tick: u64) -> usize {
        self.alive(now_tick).len()
    }
}

impl Default for InstanceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_makes_alive() {
        let mut reg = InstanceRegistry::new();
        reg.heartbeat("i-1", 0);
        assert!(reg.is_alive("i-1", 5));
        assert_eq!(reg.status("i-1", 5), Some(InstanceStatus::Alive));
    }

    #[test]
    fn heartbeat_expires_after_deadline() {
        let mut reg = InstanceRegistry::new();
        reg.heartbeat("i-1", 0);
        assert!(reg.is_alive("i-1", 9)); // 9 < 10
        assert!(reg.is_dead("i-1", 10)); // 10 >= 10
        assert_eq!(reg.status("i-1", 10), Some(InstanceStatus::Dead));
    }

    #[test]
    fn update_heartbeat_keeps_alive() {
        let mut reg = InstanceRegistry::new();
        reg.heartbeat("i-1", 0);
        reg.heartbeat("i-1", 20); // refresh
        assert!(reg.is_alive("i-1", 25));
    }

    #[test]
    fn alive_list_excludes_dead() {
        let mut reg = InstanceRegistry::new();
        reg.heartbeat("i-1", 0);
        reg.heartbeat("i-2", 0);
        reg.heartbeat("i-3", 0);
        // i-2 heartbeat refresh at 50; i-1 and i-3 stale at 55.
        reg.heartbeat("i-2", 50);
        assert_eq!(reg.alive(55), vec!["i-2"]);
        assert_eq!(reg.dead(55), vec!["i-1", "i-3"]);
    }

    #[test]
    fn remove_instance() {
        let mut reg = InstanceRegistry::new();
        reg.heartbeat("i-1", 0);
        reg.remove("i-1");
        assert_eq!(reg.status("i-1", 0), None);
        assert!(!reg.is_alive("i-1", 0));
    }

    #[test]
    fn unknown_instance_none() {
        let reg = InstanceRegistry::new();
        assert_eq!(reg.status("i-x", 0), None);
    }

    #[test]
    fn alive_count() {
        let mut reg = InstanceRegistry::new();
        reg.heartbeat("i-1", 0);
        reg.heartbeat("i-2", 0);
        assert_eq!(reg.alive_count(5), 2);
        assert_eq!(reg.alive_count(15), 0);
    }

    #[test]
    fn all_includes_dead() {
        let mut reg = InstanceRegistry::new();
        reg.heartbeat("i-1", 0);
        assert_eq!(reg.all(), vec!["i-1"]);
        // Even after dead, still in all().
        assert_eq!(reg.all().as_slice(), ["i-1"]);
    }

    #[test]
    fn with_deadline_and_prune_dead() {
        let mut reg = InstanceRegistry::with_deadline(5);
        assert_eq!(reg.deadline_ticks(), 5);
        reg.heartbeat("i-1", 0);
        reg.heartbeat("i-2", 4);
        reg.heartbeat("i-3", 1);

        // At tick 6:
        // i-1 (diff 6 >= 5 -> dead)
        // i-2 (diff 2 < 5 -> alive)
        // i-3 (diff 5 >= 5 -> dead)
        let pruned = reg.prune_dead(6);
        assert_eq!(pruned, vec!["i-1", "i-3"]);
        assert_eq!(reg.all(), vec!["i-2"]);
        assert!(reg.is_alive("i-2", 6));
        assert!(!reg.is_alive("i-1", 6));
    }
}
