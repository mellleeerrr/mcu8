//! Интеграция подсистемы кластеризации с сервером VKS (Milestone M5).
//!
//! Обеспечивает:
//! - Регистрацию инстанса в реестре кластера и периодический heartbeat.
//! - Распределённое владение комнатами (consistent hash + монотонный lease TTL + CAS fencing).
//! - Шину событий кластера (Bus) с двухпороговым backpressure (soft max / hard max) и DLQ.

use std::sync::{Arc, Mutex};
use tracing::warn;

use vks_cluster::{Bus, FencingError, InstanceRegistry, LeaseResult, Msg, PushResult, RoomOwner};

/// Менеджер кластерного взаимодействия инстанса.
#[derive(Clone)]
pub struct ClusterManager {
    node_id: String,
    owner: Arc<Mutex<RoomOwner>>,
    registry: Arc<Mutex<InstanceRegistry>>,
    bus: Arc<Bus>,
}

impl ClusterManager {
    /// Создаёт новый менеджер кластера для данного узла.
    pub fn new(node_id: impl Into<String>) -> Self {
        let node_id = node_id.into();
        let mut owner = RoomOwner::new();
        let mut registry = InstanceRegistry::new();

        let now_tick =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

        // Регистрируем локальный узел на текущем тике
        owner.register(&node_id, now_tick);
        registry.heartbeat(&node_id, now_tick);

        Self {
            node_id,
            owner: Arc::new(Mutex::new(owner)),
            registry: Arc::new(Mutex::new(registry)),
            bus: Arc::new(Bus::new()),
        }
    }

    /// Идентификатор текущего узла кластера.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Обновляет heartbeat текущего узла и отсекает упавшие узлы.
    pub fn send_heartbeat(&self, now_tick: u64) {
        let mut reg = self.registry.lock().unwrap();
        reg.heartbeat(&self.node_id, now_tick);
        let dead = reg.prune_dead(now_tick);
        if !dead.is_empty() {
            warn!(dead_nodes = ?dead, "Pruned dead cluster nodes");
            let mut owner = self.owner.lock().unwrap();
            for d in dead {
                owner.unregister(&d);
            }
        }
    }

    /// Запрашивает лиз владения комнатой с монотонным сроком `now_tick + ttl_ticks`.
    pub fn acquire_room(&self, room_id: &str, now_tick: u64, ttl_ticks: u64) -> LeaseResult {
        let mut owner = self.owner.lock().unwrap();
        owner.acquire_lease(room_id, &self.node_id, now_tick, ttl_ticks)
    }

    /// Продлевает лиз владения комнатой.
    pub fn renew_room(&self, room_id: &str, now_tick: u64, ttl_ticks: u64) -> LeaseResult {
        let mut owner = self.owner.lock().unwrap();
        owner.renew_lease(room_id, &self.node_id, now_tick, ttl_ticks)
    }

    /// Освобождает владение комнатой.
    pub fn release_room(&self, room_id: &str) -> bool {
        let mut owner = self.owner.lock().unwrap();
        owner.release(room_id, &self.node_id)
    }

    /// Проверяет fencing токен комнаты перед операциями.
    pub fn verify_fencing(&self, room_id: &str, provided_epoch: u64, now_tick: u64) -> Result<(), FencingError> {
        let owner = self.owner.lock().unwrap();
        owner.check_fencing(room_id, &self.node_id, provided_epoch, now_tick)
    }

    /// Публикует событие в шину кластера с защитой от переполнения памяти (backpressure).
    pub fn publish_event(&self, topic: &str, payload: Vec<u8>, critical: bool) -> PushResult {
        let msg = Msg::new(topic, payload, critical);
        self.bus.push_backpressure_with_limits(topic, msg, 10_000, 20_000)
    }

    /// Статус кластера для мониторинга.
    pub fn status(&self, now_tick: u64) -> serde_json::Value {
        let reg = self.registry.lock().unwrap();
        let alive = reg.alive(now_tick);
        let dead = reg.dead(now_tick);
        let owner = self.owner.lock().unwrap();
        let owned_rooms = owner.rooms_for(&self.node_id);

        serde_json::json!({
            "node_id": self.node_id,
            "status": "ACTIVE",
            "alive_nodes": alive,
            "dead_nodes": dead,
            "owned_rooms_count": owned_rooms.len(),
            "dlq_count": self.bus.dlq_count()
        })
    }
}
