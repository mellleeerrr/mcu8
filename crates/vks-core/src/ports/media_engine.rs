// Ключевой порт: MediaEngine.
//
// Трейт [`MediaEngine`] — единый интерфейс медиа-плоскости, через
// который ядро управляет SFU И MCU. Две независимые реализации:
// `vks-media-sfu` (str0m) и `vks-media-mcu` (compositor).
//
// **Инвариант MCU/SFU-прозрачности (ARCHITECTURE §2):** серверный код
// работает только с трейтом, а не с конкретной реализацией. Клиент
// не знает о режиме — он получает либо N треков (SFU), либо 1
// композитный трек (MCU), но API единый.
//
// См. ARCHITECTURE.md §2.1 — exact trait signature.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::participant::ParticipantId;
use crate::room::{RoomId, RoomMode};
use crate::track::{MediaKind, Track, TrackId};

/// Дескриптор медиатрека, используемый в MediaEngine (подписки,
/// публикации). Это тот же [`Track`], что и в домене, но здесь мы
/// переиспользуем его напрямую.
pub type MediaTrack = Track;

/// Медиа-способности комнаты (какие кодеки/фичи доступны).
///
/// В SFU: simulcast, SVC; в MCU: композитные возможности.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomCaps {
    /// Кодеки видео (например, "vp8", "vp9", "h264").
    pub video_codecs: Vec<String>,
    /// Кодеки аудио (например, "opus", "g722").
    pub audio_codecs: Vec<String>,
    /// Поддержка simulcast (SFU).
    pub simulcast: bool,
    /// Поддержка SVC (SFU).
    pub svc: bool,
    /// Разрешена ли смена layout (both modes).
    pub layout_enabled: bool,
    /// Максимальное количество участников.
    pub max_participants: Option<u32>,
}

/// Конфигурация создания комнаты в MediaEngine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMediaCfg {
    /// Идентикатор комнаты.
    pub room_id: RoomId,
    /// Режим (SFU/MCU) — определяется вызывающим (ADR-0026).
    pub mode: RoomMode,
    /// Медиа-способности.
    pub caps: RoomCaps,
}

/// Ошибка MediaEngine.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum MediaEngineError {
    /// Комната не найдена.
    #[error("room not found: {0}")]
    RoomNotFound(RoomId),
    /// Комната уже существует.
    #[error("room already exists: {0}")]
    RoomAlreadyExists(RoomId),
    /// Комната уничтожена.
    #[error("room destroyed: {0}")]
    RoomDestroyed(RoomId),
    /// Публикатор не найден.
    #[error("publisher not found: {0}")]
    PublisherNotFound(ParticipantId),
    /// Подписчик не найден.
    #[error("subscriber not found: {0}")]
    SubscriberNotFound(ParticipantId),
    /// Трек не найден.
    #[error("track not found: {0}")]
    TrackNotFound(TrackId),
    /// Неподдерживаемый тип медиат.
    #[error("unsupported media kind: {0}")]
    UnsupportedMediaKind(MediaKind),
    /// Ошибка режима (несовместимая операция для режима).
    #[error("mode not supported: {0}")]
    ModeNotSupported(RoomMode),
    /// Прочая ошибка реализации.
    #[error("media engine error: {0}")]
    Other(String),
}

/// Handler публикации: идентифицирует участника-источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublisherHandle {
    /// Идентикатор комнаты, в которой опубликован.
    pub room: RoomId,
    /// Идентикатор участника-источника.
    pub publisher: ParticipantId,
}

/// Handler подписки: идентифицирует участника-подписчика.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriberHandle {
    /// Идентикатор комнаты, в которой подписан.
    pub room: RoomId,
    /// Идентикатор участника-подписчика.
    pub subscriber: ParticipantId,
}

/// Запрос на подписку к медиатрекам.
///
/// В SFU: список конкретных треков (N-to-N). В MCU: флаг "comp
/// osite" (1 трек). Единый запрос для обоих режимов — прозрачность.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRequest {
    /// Идентикаторы треков, на которые подписываемся. В MCU-режиме
    /// это обычно один композитный трек; в SFU — N треков.
    pub track_ids: Vec<TrackId>,
    /// Только аудио (заключ).
    pub audio_only: bool,
}

impl SubscribeRequest {
    /// Подписка на все указанные треки (audio + video).
    pub fn all(track_ids: Vec<TrackId>) -> Self {
        Self { track_ids, audio_only: false }
    }

    /// Только аудио.
    pub fn audio_only(track_ids: Vec<TrackId>) -> Self {
        Self { track_ids, audio_only: true }
    }
}

/// Описание раскладки (layout) для apply_layout.
///
/// В SFU: клиентский SDK использует это для CSS/Taffy-дескриптора.
/// В MCU: compositor использует это для pixel-rect.
///
/// Поля упрощены для ядра: конкретная геометрия живёт в vks-layout.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayoutDescriptor {
    /// Идентикатор layout (versioned).
    pub layout_id: String,
    /// Version (инкремент при изменении, vks-layout).
    pub version: u32,
    /// Присвоения: participant_id → slot (BTreeMap = deterministic).
    pub assignments: BTreeMap<ParticipantId, u32>,
}

/// Порт MediaEngine — единый интерфейс для SFU и MCU.
///
/// **КЛЮЧЕВОЙ ТРЕЙТ** (ARCHITECTURE §2.1). Две реализации:
/// - `vks-media-sfu::Engine` (str0m-based)
/// - `vks-media-mcu::Engine` (compositor-based)
///
/// Ядро и сигнализация работают только с этим трейтом.
///
/// ## Invariants
/// - Все методы `Send + Sync` safe (trait bound).
/// - Нет I/O в самом трейт: I/O — в реализациях (Sans-I/O, ADR-0016).
/// - Режим прозрачен: вызывающий не знает, SFU или MCU.
#[async_trait]
pub trait MediaEngine: Send + Sync + 'static {
    /// Режим этого движка (Sfu или Mcu).
    ///
    /// Вызывается для `ServerHello.room_mode`. Клиентский SDK
    /// использует это для выбора рендерера (внутри SDK, не в приложении).
    fn mode(&self) -> RoomMode;

    /// Создаёт комнату в медиа-плоскости.
    ///
    /// Возвращает handle, по которому можно публиковать/подписываться.
    async fn create_room(&self, cfg: RoomMediaCfg) -> Result<RoomMediaHandle, MediaEngineError>;

    /// Уничтожает комнату (terminal).
    async fn destroy_room(&self, room: RoomId) -> Result<(), MediaEngineError>;

    /// Переключает режим комнаты in-place (ADR-0026).
    ///
    /// Клиенты получают `RoomModeChanged` и renegotiate. Возвращает
    /// новый режим.
    async fn switch_room_mode(&self, room: RoomId, new_mode: RoomMode) -> Result<RoomMode, MediaEngineError>;

    /// Добавляет публикующего участника с указанными треками.
    ///
    /// В SFU: треки forward-ятся подписчикам. В MCU: треки
    /// декодируются и попадают в compositor.
    async fn add_publisher(
        &self,
        room: RoomId,
        publisher: ParticipantId,
        tracks: Vec<MediaTrack>,
    ) -> Result<PublisherHandle, MediaEngineError>;

    /// Удаляет публикующего участника.
    async fn remove_publisher(&self, room: RoomId, publisher: ParticipantId) -> Result<(), MediaEngineError>;

    /// Добавляет подписчика с указанным запросом подписки.
    ///
    /// В SFU: подписчик получает N треков. В MCU: подписчик
    /// получает 1 композитный трек.
    async fn add_subscriber(
        &self,
        room: RoomId,
        subscriber: ParticipantId,
        req: SubscribeRequest,
    ) -> Result<SubscriberHandle, MediaEngineError>;

    /// Удаляет подписчика.
    async fn remove_subscriber(&self, room: RoomId, subscriber: ParticipantId) -> Result<(), MediaEngineError>;

    /// Применяет раскладку (layout).
    ///
    /// В SFU: client-side (CSS/Taffy). В MCU: server-side
    /// (compositor pixel-rect). Единый вызов — прозрачность.
    async fn apply_layout(&self, room: RoomId, layout: LayoutDescriptor) -> Result<(), MediaEngineError>;

    /// Устанавливает медиа-способности комнаты.
    async fn set_room_capabilities(&self, room: RoomId, caps: RoomCaps) -> Result<(), MediaEngineError>;

    /// Возвращает список активных публикаторов в комнате.
    async fn list_publishers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError>;

    /// Возвращает список активных подписчиков в комнате.
    async fn list_subscribers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError>;
}

/// Handle на комнату в медиа-плоскости (возвращается из
/// [`MediaEngine::create_room`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomMediaHandle {
    /// Идентикатор комнаты.
    pub room: RoomId,
    /// Текущий режим (SFU/MCU).
    pub mode: RoomMode,
}

// --- Re-exports for convenience ---
// (TrackId, TrackKind, MediaKind, ParticipantId, RoomId, RoomMode are
// already in crate root; re-exported here for import convenience.)
