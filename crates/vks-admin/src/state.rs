// AppState — общее состояние админ-сервера (фаза 5).
//
// В этой волне все хранилища in-memory (детерминированные, без I/O).
// Production-замены: PostgreSQL (users/rooms), NATS (events), storage
// (layouts/records) — по мере наполнения фаз 5–7 (DEVELOPMENT_PLAN.md).
//
// MCU/SFU-прозрачность (ARCHITECTURE §2, ADR-0026): глобальный режим +
// per-room override. `ModeState::effective_mode` — единственный
// источник истины для `RoomServerEvent` и `ServerHello.room_mode`.
//
// Мутирующие хранилища (rooms/users/layouts/records/audit/settings)
// обёрнуты в `Mutex` — `SharedState` = `Arc<AppState>` (axum).
// `global_mode`/`switch_epoch`/`room_mode_overrides` в `Mutex<ModeState>`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use vks_core::{RoomId, RoomMode, User, UserId};

pub use crate::handlers::SharedState;

/// Запись комнаты в админ-хранилище.
///
/// `mode` — per-room override (ADR-0026, Q7): `None` = режим берётся
/// из глобального [`ModeState::global_mode`]. `created_ms` — момент
/// создания (ms epoch), отдаётся UI как `started_at` (карточка считает
/// аптайм/длительность от него).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomRecord {
    /// Идентикатор комнаты.
    pub id: RoomId,
    /// Название комнаты.
    pub name: String,
    /// Per-room override режима MCU/SFU (None = глобальный режим).
    pub mode: Option<RoomMode>,
    /// Текущее число участников (live status).
    pub participants: usize,
    /// Агрегированная битрейт комнаты, kbps (live status, monitor).
    pub bitrate_kbps: u32,
    /// Момент создания, ms epoch (`#[serde(default)]` для старых снимков).
    #[serde(default)]
    pub created_ms: u64,
}

impl RoomRecord {
    /// Создаёт запись комнаты с заданным per-room режимом.
    pub fn new(id: RoomId, name: impl Into<String>, mode: Option<RoomMode>) -> Self {
        Self { id, name: name.into(), mode, participants: 0, bitrate_kbps: 0, created_ms: now_unix_ms() }
    }
}

/// Текущее время, ms epoch (0 при сбое системных часов — не паникуем).
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Раскладка (layout) — структура из vks-layout (phase 4); здесь
/// храним как JSON-строка, твк. vks-layout ещё скелет.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutRecord {
    /// Идентикатор раскладки (ULID/UUID-строка).
    pub id: String,
    /// Название раскладки.
    pub name: String,
    /// JSON-документ раскладки (string; `serde_json::Value` в production).
    pub document: String,
}

/// Запись пользователя (управление учётными записями, фаза 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    /// Идентикатор пользователя.
    pub id: UserId,
    /// Username.
    pub username: String,
    /// Глобальная роль (ADR-0022).
    pub role: vks_core::Role,
    /// Muted (bulk mute, ADR-0022: Operator/Mediator).
    pub muted: bool,
    /// Soft-delete (GDPR 152-7: 1000d через 30).
    pub deleted: bool,
}

/// Запись видео-записи (records, фаза 5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordEntry {
    /// Идентикатор записи.
    pub id: String,
    /// Комната, к которой относится запись.
    pub room_id: RoomId,
    /// Время создания, ms epoch.
    pub created_ms: u64,
    /// Длительность, сек.
    pub duration_sec: u32,
    /// Размер, байты.
    pub size_bytes: u64,
}

/// Режим MCU/SFU (global + per-room override + epoch).
///
/// ADR-0026: in-place переключение (комната остаётся).
#[derive(Debug, Clone)]
pub struct ModeState {
    /// Глобальный режим MCU/SFU (ADR-0026).
    pub global_mode: RoomMode,
    /// Per-room override (room_id → mode); пустой = глобальный.
    pub room_mode_overrides: BTreeMap<RoomId, RoomMode>,
    /// Эпоха переключений (idempotence-кэш клиента, ADR-0026).
    pub switch_epoch: u64,
}

impl Default for ModeState {
    fn default() -> Self {
        Self { global_mode: RoomMode::Sfu, room_mode_overrides: BTreeMap::new(), switch_epoch: 0 }
    }
}

impl ModeState {
    /// Создаёт дефолтный режим (SFU, без override).
    pub fn new() -> Self {
        Self::default()
    }

    /// Эффективный режим комнаты: override иначе глобальный.
    ///
    /// Единственный источник истины для `RoomServerEvent` и
    /// `ServerHello.room_mode` (ARCHITECTURE §2, ADR-0026).
    pub fn effective_mode(&self, room_id: &RoomId) -> RoomMode {
        self.room_mode_overrides.get(room_id).copied().unwrap_or(self.global_mode)
    }
}

/// Состояние админ-сервера (in-memory stores + mode).
///
/// Обёрнуто в `Arc` на уровне `Router` (axum: `State(Arc<AppState>)`).
/// Мутирующие хранилища — `Mutex` (production: PostgreSQL/NATS).
///
/// `Clone` — ручная: `Mutex` не клонируется, но данные внутри `ш
/// копируются (для `Arc<AppState>` — ` handlers).
pub struct AppState {
    /// Персистентное хранилище (Postgres / SQLite / Memory).
    pub storage: Option<Arc<dyn vks_core::ports::Storage>>,
    /// Полиморфный AuthProvider (local / ldap / oidc).
    pub auth_provider: Option<Arc<dyn vks_core::ports::AuthProvider>>,
    /// JWT Issuer для проверки токенов доступа.
    pub jwt_issuer: Option<Arc<vks_auth::jwt::JwtIssuer>>,
    /// Встроенная база пользователей (local provider) для login `/api/v3` (§4.3).
    pub local_db: Option<Arc<vks_auth::local_db::LocalDb>>,
    /// Состояние адаптера `/api/v3` (lockout логина, rate-limit WS).
    pub api_v3: crate::api_v3::ApiV3State,
    /// Комнаты (id → запись).
    pub rooms: Mutex<BTreeMap<RoomId, RoomRecord>>,
    /// Пользователи (id → запись).
    pub users: Mutex<BTreeMap<UserId, UserRecord>>,
    /// Раскладки (id → запись).
    pub layouts: Mutex<BTreeMap<String, LayoutRecord>>,
    /// Записи (id → запись).
    pub records: Mutex<BTreeMap<String, RecordEntry>>,
    /// Аудит-лог (append-only, ADR-0019).
    pub audit: Mutex<crate::audit::AuditLog>,
    /// Настройки (hot-reload shape: set возвращает новое значение).
    pub settings: Mutex<crate::settings::Settings>,
    /// Режим (global + per-room override + epoch).
    pub mode: Mutex<ModeState>,
    /// Очередь событий (production → NATS, ADR-0014).
    pub event_queue: crate::mode::ModeEventQueue,
    /// Хаб fan-out рассылки сигнальных событий в комнатах (WebRTC WS).
    pub room_hub: Arc<crate::hub::RoomHub>,
    /// Канал административных событий для legacy UI WS (/api/v3/ws).
    pub admin_events: tokio::sync::broadcast::Sender<crate::hub::AdminBroadcastEvent>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("has_storage", &self.storage.is_some())
            .field("has_jwt_issuer", &self.jwt_issuer.is_some())
            .field("has_local_db", &self.local_db.is_some())
            .field("api_v3", &self.api_v3)
            .field("rooms", &self.rooms)
            .field("users", &self.users)
            .field("layouts", &self.layouts)
            .field("records", &self.records)
            .field("audit", &self.audit)
            .field("settings", &self.settings)
            .field("mode", &self.mode)
            .field("event_queue", &self.event_queue)
            .finish()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Создаёт новое состояние с дефолтными настройками без персистентного хранилища.
    pub fn new() -> Self {
        let (admin_tx, _admin_rx) = tokio::sync::broadcast::channel(crate::hub::ADMIN_CHANNEL_CAPACITY);
        Self {
            storage: None,
            auth_provider: None,
            jwt_issuer: None,
            local_db: None,
            api_v3: crate::api_v3::ApiV3State::default(),
            rooms: Mutex::new(BTreeMap::new()),
            users: Mutex::new(BTreeMap::new()),
            layouts: Mutex::new(BTreeMap::new()),
            records: Mutex::new(BTreeMap::new()),
            audit: Mutex::new(crate::audit::AuditLog::new()),
            settings: Mutex::new(crate::settings::Settings::default()),
            mode: Mutex::new(ModeState::new()),
            event_queue: crate::mode::ModeEventQueue::new(),
            room_hub: Arc::new(crate::hub::RoomHub::new()),
            admin_events: admin_tx,
        }
    }

    /// Отправляет push-уведомление администраторам (в `/api/v3/ws`).
    pub fn notify_admin(&self, topic: &str, payload: serde_json::Value) {
        let _ = self.admin_events.send(crate::hub::AdminBroadcastEvent { topic: topic.to_string(), payload });
    }

    /// Рассылает сигнальное сообщение всем участникам комнаты.
    pub fn broadcast_room(&self, room: &RoomId, sender_pid: Option<&str>, payload: bytes::Bytes) {
        self.room_hub.broadcast(room, sender_pid, payload);
    }

    /// Добавляет доверенную запись в аудит-лог с серверной временной меткой.
    pub fn record_audit_entry(
        &self,
        user: impl Into<String>,
        action: impl Into<String>,
        room: Option<String>,
        ip: impl Into<String>,
    ) {
        let entry = crate::audit::AuditEntry {
            ts_ms: now_unix_ms(),
            user: user.into(),
            action: action.into(),
            room,
            ip: ip.into(),
        };
        if let Ok(mut audit) = self.audit.lock() {
            audit.record(entry);
        }
    }

    /// Создаёт состояние с подключенным хранилищем ядра.
    pub fn with_storage(storage: Arc<dyn vks_core::ports::Storage>) -> Self {
        let mut state = Self::new();
        state.storage = Some(storage);
        state
    }

    /// Синхронизирует начальное состояние комнат и оверрайдов из хранилища.
    pub async fn sync_from_storage(&self) -> Result<(), String> {
        if let Some(storage) = &self.storage {
            let rooms = storage.list_rooms().await.map_err(|e| e.to_string())?;
            let overrides = storage.list_room_mode_overrides().await.unwrap_or_default();

            let mut map = self.rooms.lock().map_err(|e| e.to_string())?;
            let mut mode_state = self.mode.lock().map_err(|e| e.to_string())?;

            for r in rooms {
                let override_mode = overrides.get(&r.id).copied();
                if let Some(om) = override_mode {
                    mode_state.room_mode_overrides.insert(r.id, om);
                }
                map.entry(r.id).or_insert_with(|| RoomRecord {
                    id: r.id,
                    name: r.name,
                    mode: override_mode,
                    participants: 0,
                    bitrate_kbps: 0,
                    created_ms: if r.created_ms > 0 { r.created_ms } else { now_unix_ms() },
                });
            }
        }
        Ok(())
    }

    /// Пользователь в доменном виде (vks_core::User).
    pub fn domain_user(&self, id: &UserId) -> Option<User> {
        self.users.lock().expect("users store poisoned").get(id).map(|u| User {
            id: u.id,
            username: u.username.clone(),
            role: u.role,
            groups: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_mode_uses_global_when_no_override() {
        let state = AppState::new();
        let id = RoomId::new();
        let m = state.mode.lock().unwrap();
        assert_eq!(m.effective_mode(&id), RoomMode::Sfu);
    }

    #[test]
    fn effective_mode_prefers_override() {
        let state = AppState::new();
        let id = RoomId::new();
        {
            let mut m = state.mode.lock().unwrap();
            m.room_mode_overrides.insert(id, RoomMode::Mcu);
            m.global_mode = RoomMode::Sfu;
        }
        let m = state.mode.lock().unwrap();
        assert_eq!(m.effective_mode(&id), RoomMode::Mcu);
    }

    #[test]
    fn global_mode_change_does_not_affect_overridden_room() {
        let state = AppState::new();
        let id = RoomId::new();
        {
            let mut m = state.mode.lock().unwrap();
            m.room_mode_overrides.insert(id, RoomMode::Mcu);
            m.global_mode = RoomMode::Sfu;
        }
        // Override не меняется при смене global.
        let m = state.mode.lock().unwrap();
        assert_eq!(m.effective_mode(&id), RoomMode::Mcu);
    }

    #[test]
    fn room_record_defaults() {
        let r = RoomRecord::new(RoomId::new(), "conf", None);
        assert_eq!(r.participants, 0);
        assert_eq!(r.bitrate_kbps, 0);
        assert_eq!(r.mode, None);
        assert!(r.created_ms > 0, "created_ms — реальная метка создания");
    }

    #[tokio::test]
    async fn sync_from_storage_preserves_created_at_and_overrides() {
        use vks_core::ports::Storage;
        let storage: Arc<dyn Storage> = Arc::new(vks_storage::MemoryStorage::new());
        let mut room1 = vks_core::Room::new("room-1", RoomMode::Mcu);
        let rid1 = room1.id;
        let created_ts = 1_700_000_000_123u64;
        room1.created_ms = created_ts;
        storage.upsert_room(&room1).await.unwrap();
        // Set override for room1
        storage.set_room_mode_override(rid1, RoomMode::Mcu).await.unwrap();

        // room2: no override (inherited)
        let room2 = vks_core::Room::new("room-2", RoomMode::Sfu);
        let rid2 = room2.id;
        storage.upsert_room(&room2).await.unwrap();

        let state = AppState::with_storage(storage);
        state.sync_from_storage().await.unwrap();

        let rooms = state.rooms.lock().unwrap();
        let r1 = rooms.get(&rid1).expect("room1 exists");
        assert_eq!(r1.mode, Some(RoomMode::Mcu), "room1 has explicit override");
        assert!(r1.created_ms > 0);

        let r2 = rooms.get(&rid2).expect("room2 exists");
        assert_eq!(r2.mode, None, "room2 has None (inherited mode)");

        let mode_state = state.mode.lock().unwrap();
        assert_eq!(mode_state.room_mode_overrides.get(&rid1), Some(&RoomMode::Mcu));
        assert_eq!(mode_state.room_mode_overrides.get(&rid2), None);
    }
}
