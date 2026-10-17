//! `vks-core` — чистая доменная логика (no I/O).
//!
//! Фаза 1: Room / Participant / Session (FSM) / Track + порта
//! (MediaEngine, Storage, Auth, Events, Clock, Rng).
//!
//! Инвариант MCU/SFU-прозрачности живёт здесь (ARCHITECTURE.md §2).
//!
//! ## Модули
//! - [`room`] — Room, RoomId, RoomState, RoomMode.
//! - [`participant`] — Participant, Role, Permissions (RBAC, ADR-0022).
//! - [`session`] — Session lifecycle FSM.
//! - [`track`] — TrackId, TrackKind, MediaKind, Track.
//! - [`ports`] — гексагональные порты (MediaEngine, Storage, Auth,
//!   Events, Clock, Rng).
//!
//! ## Инварианты
//! - **Sans-I/O** (ADR-0016): ядро не владеет сокетами/файлами/БД.
//!   Все внешние взаимодействия — через порты (traits).
//! - **MCU/SFU-прозрачность** (ARCHITECTURE §2): единый
//!   [`MediaEngine`]-трейт; клиент не знает о режиме.
//! - **Deterministic**: все время/случайность через `Clock`/`Rng`-порты.
//! - **No `unsafe`** (CODE_STYLE §6).
//!
//! Дерево зависимостей: `vks-core` — корень, не зависит от других
//! внутренних крейтов (ARCHITECTURE §3).

pub mod participant;
pub mod ports;
pub mod room;
pub mod session;
pub mod track;
pub mod transparency;

// Re-exports for convenience (flat API).
pub use participant::{Participant, ParticipantId, Permission, Permissions, Role, UserId};
pub use ports::{
    // storage
    ApiKey,
    // auth
    AuthError,
    AuthProvider,
    // clock
    Clock,
    ClockError,
    Credentials,
    // rng
    DeterministicRng,
    // events
    Event,
    EventId,
    EventKind,
    EventStream,
    Events,
    EventsError,
    FixedClock,
    Layout,
    // media
    LayoutDescriptor,
    MediaEngine,
    MediaEngineError,
    MediaTrack,
    PublisherHandle,
    Rng,
    RngError,
    RoomCaps,
    RoomMediaCfg,
    RoomMediaHandle,
    Storage,
    StorageError,
    SubscribeRequest,
    SubscriberHandle,
    Subscription,
    SubscriptionHandle,
    SystemClock,
    User,
};
pub use room::{Room, RoomId, RoomMode, RoomState, RoomTransitionError};
pub use session::{Session, SessionError, SessionEvent, SessionId, SessionState};
pub use track::{MediaKind, Track, TrackId, TrackKind};
pub use transparency::{ClientView, ModeSwitcher, RoomModeChanged, SwitchClock, SwitchOutcome};
