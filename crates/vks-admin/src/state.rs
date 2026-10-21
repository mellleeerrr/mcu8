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
pub fn now_unix_ms() -> u64 {
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
    /// Монотонный счетчик поколения учетной записи (M1.2).
    #[serde(default)]
    pub generation: u64,
}

/// Уведомление об отзыве сессий и токенов пользователя в реальном времени (M1.2 / M1.8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationNotice {
    pub user_id: String,
    pub generation: u64,
}

/// Запись видео-записи (records, M5.5).
pub use vks_core::ports::storage::RecordEntry;

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

#[async_trait::async_trait]
pub trait OutboundDialer: Send + Sync {
    /// Инициирует исходящий вызов абонента в указанную комнату по URI/строке вызова.
    /// Возвращает идентификатор участника (participant_id).
    async fn dial(&self, room_id: RoomId, uri: &str, display_name: Option<&str>) -> Result<String, String>;

    /// Передаёт выбранную раскладку комнаты в медиа-движок (MCU).
    ///
    /// `layout` — PNM-ресурс `video_layouts` целиком (`id` + `payload.slots[]`
    /// с `position` и идентификаторами абонентов). Реализации без
    /// медиа-плоскости оставляют no-op, чтобы старт сессии не зависел от MCU.
    async fn apply_layout(&self, _room_id: RoomId, _layout: serde_json::Value) -> Result<(), String> {
        Ok(())
    }

    /// Measured media counters for an active gateway connection.
    fn media_stats(&self, _participant_id: &str) -> Option<serde_json::Value> {
        None
    }

    /// Завершает исходящий/шлюзовой вызов абонента (BYE / Release).
    fn hangup(&self, _participant_id: &str) -> bool {
        false
    }

    /// Управляет медиа-потоками абонента (Mute TX/RX, регулировка громкости).
    #[allow(clippy::too_many_arguments)]
    fn set_media_control(
        &self,
        _participant_id: &str,
        _audio_tx_muted: Option<bool>,
        _audio_rx_muted: Option<bool>,
        _video_tx_muted: Option<bool>,
        _video_rx_muted: Option<bool>,
        _volume_tx: Option<u32>,
        _volume_rx: Option<u32>,
    ) -> bool {
        false
    }

    /// Передаёт DTMF-последовательность терминалу через RFC 4733 / telephone-event.
    fn send_dtmf(&self, _participant_id: &str, _sequence: &str) -> Result<(), String> {
        Ok(())
    }

    /// Передаёт команды Far End Camera Control (FECC / PTZ).
    fn send_fecc(&self, _participant_id: &str, _command: &str) -> Result<(), String> {
        Ok(())
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
    /// Исходящий дозвонщик (SIP/H.323 шлюзы) (V1-V4).
    pub outbound_dialer: std::sync::RwLock<Option<Arc<dyn OutboundDialer>>>,
    /// Комнаты (id → запись).
    pub rooms: Mutex<BTreeMap<RoomId, RoomRecord>>,
    /// Пользователи (id → запись).
    pub users: Mutex<BTreeMap<UserId, UserRecord>>,
    /// Раскладки (id → запись).
    pub layouts: Mutex<BTreeMap<String, LayoutRecord>>,
    /// API-ключи (id/hash → запись) (M1.1 / M1.6).
    pub api_keys: Mutex<BTreeMap<String, vks_core::ports::storage::ApiKey>>,
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
    /// Канал уведомлений об отзыве токенов/сессий в реальном времени (M1.2 / M1.8).
    pub revocation_tx: tokio::sync::broadcast::Sender<RevocationNotice>,
    /// Флаг здоровья медиа-подсистемы (UDP media loop / pipelines) (M0.6 / M6.3).
    pub media_healthy: std::sync::atomic::AtomicBool,
    /// Набор активных инъекций сбоев (M0.6 / M6.3).
    pub faults: Mutex<std::collections::HashSet<String>>,
    /// Адрес MCU для conference-адресов (`addresses.sip/h323`). `OnceLock` — потому
    /// что известен только после bind UDP-лупа (нормализованный ICE candidate).
    pub server_addr: std::sync::OnceLock<String>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("has_storage", &self.storage.is_some())
            .field("has_jwt_issuer", &self.jwt_issuer.is_some())
            .field("has_local_db", &self.local_db.is_some())
            .field("has_outbound_dialer", &self.outbound_dialer.read().map(|g| g.is_some()).unwrap_or(false))
            .field("api_v3", &self.api_v3)
            .field("rooms", &self.rooms)
            .field("users", &self.users)
            .field("layouts", &self.layouts)
            .field("api_keys", &self.api_keys)
            .field("records", &self.records)
            .field("audit", &self.audit)
            .field("settings", &self.settings)
            .field("mode", &self.mode)
            .field("server_addr", &self.server_addr)
            .field("event_queue", &self.event_queue)
            .field("media_healthy", &self.media_healthy.load(std::sync::atomic::Ordering::Relaxed))
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
        let (revocation_tx, _revocation_rx) = tokio::sync::broadcast::channel(256);
        Self {
            storage: None,
            auth_provider: None,
            jwt_issuer: None,
            local_db: None,
            api_v3: crate::api_v3::ApiV3State::default(),
            outbound_dialer: std::sync::RwLock::new(None),
            rooms: Mutex::new(BTreeMap::new()),
            users: Mutex::new(BTreeMap::new()),
            layouts: Mutex::new(BTreeMap::new()),
            api_keys: Mutex::new(BTreeMap::new()),
            records: Mutex::new(BTreeMap::new()),
            audit: Mutex::new(crate::audit::AuditLog::new()),
            settings: Mutex::new(crate::settings::Settings::default()),
            mode: Mutex::new(ModeState::new()),
            event_queue: crate::mode::ModeEventQueue::new(),
            room_hub: Arc::new(crate::hub::RoomHub::new()),
            admin_events: admin_tx,
            revocation_tx,
            media_healthy: std::sync::atomic::AtomicBool::new(true),
            faults: Mutex::new(std::collections::HashSet::new()),
            server_addr: std::sync::OnceLock::new(),
        }
    }

    /// Подключает исходящий дозвонщик шлюзов (V1-V4).
    pub fn with_dialer(self, dialer: Arc<dyn OutboundDialer>) -> Self {
        self.set_dialer(dialer);
        self
    }

    /// Устанавливает исходящий дозвонщик на уже обернутом состоянии.
    pub fn set_dialer(&self, dialer: Arc<dyn OutboundDialer>) {
        if let Ok(mut lock) = self.outbound_dialer.write() {
            *lock = Some(dialer);
        }
    }

    /// Возвращает текущий активный исходящий дозвонщик (если сконфигурирован).
    pub fn outbound_dialer(&self) -> Option<Arc<dyn OutboundDialer>> {
        self.outbound_dialer.read().ok().and_then(|g| g.clone())
    }

    /// Внедряет сбой в подсистему для проверки деградации (M0.6 / M6.3).
    pub fn inject_fault(&self, fault: impl Into<String>) {
        if let Ok(mut f) = self.faults.lock() {
            f.insert(fault.into());
        }
    }

    /// Снимает внедренный ранее сбой (M0.6 / M6.3).
    pub fn clear_fault(&self, fault: &str) {
        if let Ok(mut f) = self.faults.lock() {
            f.remove(fault);
        }
    }

    /// Проверяет, активен ли сбой в указанной подсистеме (M0.6 / M6.3).
    pub fn has_fault(&self, fault: &str) -> bool {
        self.faults.lock().map(|f| f.contains(fault)).unwrap_or(false)
    }

    /// Устанавливает статус здоровья медиа-подсистемы (M0.6 / M6.3).
    pub fn set_media_healthy(&self, healthy: bool) {
        self.media_healthy.store(healthy, std::sync::atomic::Ordering::SeqCst);
    }

    /// Проверяет здоровье медиа-подсистемы (M0.6 / M6.3).
    pub fn is_media_healthy(&self) -> bool {
        self.media_healthy.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Отправляет push-уведомление администраторам (в `/api/v3/ws`).
    pub fn notify_admin(&self, topic: &str, payload: serde_json::Value) {
        let _ = self.admin_events.send(crate::hub::AdminBroadcastEvent { topic: topic.to_string(), payload });
    }

    /// Отправляет уведомление об отзыве токенов/сессий пользователя (M1.2).
    pub fn notify_revocation(&self, user_id: impl Into<String>, generation: u64) {
        let _ = self.revocation_tx.send(RevocationNotice { user_id: user_id.into(), generation });
    }

    /// Рассылает сигнальное сообщение всем участникам комнаты.
    pub fn broadcast_room(&self, room: &RoomId, sender_pid: Option<&str>, payload: bytes::Bytes) {
        self.room_hub.broadcast(room, sender_pid, payload);
    }

    /// Унифицированная валидация JWT access-токена (fail-closed, ADR-0009 / ADR-0021 / M1.2).
    ///
    /// Проверяет:
    /// 1. Подпись, `exp` и `issuer` через `jwt_issuer`.
    /// 2. Монотонный счетчик поколения (`generation >= current_user_generation`).
    /// 3. Инвалидацию по смене пароля (`iat <= password_changed_at`).
    pub fn verify_token(&self, token: &str) -> Option<vks_auth::jwt::VerifiedJwt> {
        if token.is_empty() {
            return None;
        }
        let issuer = self.jwt_issuer.as_ref()?;
        let verified = issuer.verify(token).ok()?;

        // Проверка монотонного поколения для исключения same-second race (M1.2)
        let (user_gen, is_deleted) = if let Ok(users) = self.users.lock() {
            users
                .values()
                .find(|u| u.id.to_string() == verified.user_id || u.username == verified.user_id)
                .map(|u| (Some(u.generation), u.deleted))
                .unwrap_or((None, false))
        } else {
            (None, false)
        };

        if !is_deleted {
            let expected_gen = user_gen
                .unwrap_or_else(|| self.local_db.as_ref().map(|db| db.generation(&verified.user_id)).unwrap_or(1));
            if verified.generation < expected_gen {
                return None;
            }
        }

        if let Some(db) = &self.local_db {
            if verified.iat < db.password_changed_at(&verified.user_id) {
                return None;
            }
        }
        Some(verified)
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
            audit.record(entry.clone());
        }
        if let Some(storage) = &self.storage {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let storage = Arc::clone(storage);
                let core_entry = vks_core::ports::storage::AuditEntry {
                    ts_ms: entry.ts_ms,
                    user: entry.user,
                    action: entry.action,
                    room: entry.room,
                    ip: entry.ip,
                };
                drop(handle.spawn(async move {
                    let _ = storage.append_audit(&core_entry).await;
                }));
            }
        }
    }

    /// Добавляет доверенную запись в аудит-лог с подтвержденной записью в Storage (M0-M1).
    pub async fn record_audit_entry_async(
        &self,
        user: impl Into<String>,
        action: impl Into<String>,
        room: Option<String>,
        ip: impl Into<String>,
    ) -> Result<crate::audit::AuditEntry, vks_core::ports::storage::StorageError> {
        let entry = crate::audit::AuditEntry {
            ts_ms: now_unix_ms(),
            user: user.into(),
            action: action.into(),
            room,
            ip: ip.into(),
        };
        if let Some(storage) = &self.storage {
            let core_entry = vks_core::ports::storage::AuditEntry {
                ts_ms: entry.ts_ms,
                user: entry.user.clone(),
                action: entry.action.clone(),
                room: entry.room.clone(),
                ip: entry.ip.clone(),
            };
            storage.append_audit(&core_entry).await?;
        }
        if let Ok(mut audit) = self.audit.lock() {
            audit.record(entry.clone());
        }
        Ok(entry)
    }

    /// Создаёт состояние с подключенным хранилищем ядра.
    pub fn with_storage(storage: Arc<dyn vks_core::ports::Storage>) -> Self {
        let mut state = Self::new();
        state.storage = Some(storage);
        state
    }

    /// Синхронизирует начальное состояние комнат, оверрайдов и пользователей из хранилища (M0-M1).
    ///
    /// Startup-гидратация: живых сессий после рестарта процесса нет, поэтому
    /// все сохранённые комнаты помечаются остановленными.
    pub async fn sync_from_storage(&self) -> Result<(), String> {
        self.sync_from_storage_inner(true).await
    }

    /// Hot-reload настроек без перезапуска процесса (`POST /system/restart_services`).
    ///
    /// В отличие от [`Self::sync_from_storage`] не помечает живые сессии
    /// остановленными: vendor-UI вызывает его после сохранения SIP/сетевых
    /// настроек, и активные звонки не должны «обрываться» в списке.
    pub async fn hot_reload_from_storage(&self) -> Result<(), String> {
        self.sync_from_storage_inner(false).await
    }

    async fn sync_from_storage_inner(&self, mark_stopped: bool) -> Result<(), String> {
        if let Some(storage) = &self.storage {
            let rooms = storage.list_rooms().await.map_err(|e| e.to_string())?;
            let overrides = storage.list_room_mode_overrides().await.unwrap_or_default();

            {
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

            let users = storage.list_users().await.map_err(|e| e.to_string())?;
            {
                let mut users_map = self.users.lock().map_err(|e| e.to_string())?;
                for u in users {
                    users_map.entry(u.id).or_insert_with(|| UserRecord {
                        id: u.id,
                        username: u.username,
                        role: u.role,
                        muted: u.muted,
                        deleted: u.deleted,
                        generation: 1,
                    });
                }
            }

            // M1.7: Hydrate audit log from durable storage to eliminate audit amnesia after restart
            let audit_entries = storage.list_audit().await.unwrap_or_default();
            if let Ok(mut audit) = self.audit.lock() {
                for a in audit_entries {
                    audit.record(crate::audit::AuditEntry {
                        ts_ms: a.ts_ms,
                        user: a.user,
                        action: a.action,
                        room: a.room,
                        ip: a.ip,
                    });
                }
            }

            // M1.3: Hydrate user credentials and generation counters into LocalDb and users map
            let creds = storage.list_user_credentials().await.unwrap_or_default();
            if let Some(db) = &self.local_db {
                let users_map = self.users.lock().map_err(|e| e.to_string())?;
                for c in &creds {
                    let username =
                        users_map.get(&c.user_id).map(|u| u.username.clone()).unwrap_or_else(|| c.user_id.to_string());
                    db.restore_user(
                        &c.user_id.to_string(),
                        &username,
                        &c.password_hash_hex,
                        &c.salt_hex,
                        c.password_changed_at,
                        Some(c.generation),
                    );
                }
            }
            {
                let mut users_map = self.users.lock().map_err(|e| e.to_string())?;
                for c in &creds {
                    if let Some(u) = users_map.get_mut(&c.user_id) {
                        u.generation = c.generation;
                    }
                }
            }

            // M1.5: Hydrate server settings
            if let Ok(Some(settings_json)) = storage.get_settings().await {
                if let Ok(mut loaded) = serde_json::from_str::<crate::settings::Settings>(&settings_json) {
                    loaded.codecs.migrate_legacy();
                    let mut current = self.settings.lock().map_err(|e| e.to_string())?;
                    *current = loaded.clone();
                    self.mode.lock().map_err(|e| e.to_string())?.global_mode = loaded.default_mode;
                }
            }

            // M0.4: Hydrate custom layouts from storage
            if let Some(document) = storage
                .get_configuration_document(crate::api_v3::resources::CONFIGURATION_KEY)
                .await
                .map_err(|e| e.to_string())?
            {
                let value: serde_json::Value = serde_json::from_str(&document).map_err(|e| e.to_string())?;
                if let Some(resources) = value.get("resources") {
                    let mut loaded: crate::api_v3::resources::ResourceState =
                        serde_json::from_value(resources.clone()).map_err(|e| e.to_string())?;
                    crate::api_v3::resources::normalize_loaded(&mut loaded);
                    *self.api_v3.resources.lock().await = loaded;
                }
                if let Some(settings) = value.get("conference_settings") {
                    *self.api_v3.conference_settings.lock().map_err(|e| e.to_string())? =
                        serde_json::from_value(settings.clone()).map_err(|e| e.to_string())?;
                }
                if let Some(video) = value.get("video_settings") {
                    *self.api_v3.video_settings.lock().map_err(|e| e.to_string())? = video.clone();
                }
            }
            // Stored definitions survive a restart; live calls and sessions do not.
            // При hot-reload живые сессии сохраняются — маркер не ставим.
            if mark_stopped {
                let room_ids: Vec<_> = self.rooms.lock().map_err(|e| e.to_string())?.keys().copied().collect();
                self.api_v3.stopped_sessions.lock().map_err(|e| e.to_string())?.extend(room_ids);
            }

            // M0.4: Hydrate custom layouts from storage
            let layouts = storage.list_layouts().await.unwrap_or_default();
            {
                let mut layouts_map = self.layouts.lock().map_err(|e| e.to_string())?;
                for l in layouts {
                    let doc = serde_json::json!({
                        "version": l.version,
                        "is_default": l.is_default,
                        "assigned_rooms": l.assigned_rooms,
                    })
                    .to_string();
                    layouts_map.insert(l.id.clone(), LayoutRecord { id: l.id, name: l.name, document: doc });
                }
            }

            // M1.1 / M1.6: Hydrate API keys from storage
            let keys = storage.list_api_keys().await.unwrap_or_default();
            {
                let mut keys_map = self.api_keys.lock().map_err(|e| e.to_string())?;
                for k in keys {
                    keys_map.insert(k.id.clone(), k.clone());
                    let hash = vks_auth::api_key::sha256_hex(k.id.as_bytes());
                    keys_map.insert(hash, k);
                }
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

    #[tokio::test]
    async fn hot_reload_preserves_live_sessions_but_startup_marks_stopped() {
        use vks_core::ports::Storage;
        let storage: std::sync::Arc<dyn Storage> = std::sync::Arc::new(vks_storage::MemoryStorage::new());
        let room = vks_core::Room::new("Live", RoomMode::Sfu);
        storage.upsert_room(&room).await.expect("persist room");
        let state = AppState::with_storage(storage);
        state.rooms.lock().unwrap().insert(
            room.id,
            RoomRecord {
                id: room.id,
                name: room.name.clone(),
                mode: None,
                participants: 0,
                bitrate_kbps: 0,
                created_ms: 1,
            },
        );

        // Hot-reload (restart_services после сохранения SIP) не должен трогать
        // активные сессии — иначе список UI показывает их остановленными.
        state.hot_reload_from_storage().await.expect("hot reload");
        assert!(
            !state.api_v3.stopped_sessions.lock().unwrap().contains(&room.id),
            "live session must stay active after hot reload"
        );

        // Startup-гидратация по-прежнему считает все сохранённые комнаты
        // остановленными: после рестарта процесса живых звонков нет.
        state.sync_from_storage().await.expect("startup hydrate");
        assert!(state.api_v3.stopped_sessions.lock().unwrap().contains(&room.id));
    }

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
