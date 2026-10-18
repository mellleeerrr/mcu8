// Гексаонские порта (Ports (ARCHITECTURE §4.1, ADR-0016).
//
// Ядро (`vks-core`) определяет **порта** — трейты — для взаимодействия
// с внешним миром:
//
// - [`media_engine::MediaEngine`] — media plane (SFU/MCU);
// - [`storage::Storage`] — persistence (memory/sqlite/postgres);
// - [`auth::AuthProvider`] — identity (local/LDAP/OIDC);
// - [`events::Events`] — internal event bus (NATS adapter);
// - [`clock::Clock`] — для детерминированных тестов;
// - [`rng::Rng`] — для детерминированных тестов.
//
// Это позволяет заменять WebSocket на WebTransport, PostgreSQL на
// SQLite, strfom на mediasoup — **не меняя логику ядра**.
//
// Ядро **Sans-I/O** (ADR-0016): порты — чистые интерфейсы, I/O — в
// реализациях (адаптерах).

pub mod auth;
pub mod clock;
pub mod events;
pub mod media_engine;
pub mod rng;
pub mod storage;

// Re-exports for convenience.
pub use auth::{AuthError, AuthProvider, Credentials, User};
pub use clock::{Clock, ClockError, FixedClock, SystemClock};
pub use events::{Event, EventId, EventKind, EventStream, Events, EventsError, Subscription, SubscriptionHandle};
pub use media_engine::{
    LayoutDescriptor, MediaEngine, MediaEngineError, MediaTrack, PublisherHandle, RoomCaps, RoomMediaCfg,
    RoomMediaHandle, SubscribeRequest, SubscriberHandle,
};
pub use rng::{DeterministicRng, Rng, RngError};
pub use storage::{ApiKey, AuditEntry, Layout, Storage, StorageError, StoredUser, UserCredentials};
