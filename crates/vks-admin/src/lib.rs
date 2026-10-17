//! `vks-admin` — admin REST API (axum + utoipa).
//!
//! Фаза 5: `server.rs` (axum app), `rooms.rs` (CRUD + live status +
//! Observer), `users.rs` (CRUD + LDAP sync + bulk), `settings.rs`
//! (AuthProvider, TURN, codecs, mode/layout defaults, limits, retention,
//! TLS), `mode.rs` (MCU/SFU switch, глобально + per-room, ADR-0026,
//! публикует в NATS), `audit.rs` (ADR-0019), `health.rs` (liveness +
//! readiness), `handlers.rs` (Router + handlers, `/api/v1/...`).
//!
//! Дерево: vks-core, vks-proto, vks-auth, vks-storage.
//! API versioning (BACKEND_RULES §7): `/api/v1/...`.
//! utoipa — опциональная feature `openapi` (не в default-сборке).

pub mod api_v3;
pub mod audit;
pub mod auth_middleware;
pub mod handlers;
pub mod health;
pub mod mode;
pub mod rooms;
pub mod settings;
pub mod state;
pub mod users;

// Re-exports for convenience (flat API).
pub use audit::{AuditEntry, AuditFilter, AuditLog};
pub use auth_middleware::jwt_auth_middleware;
pub use handlers::{router, SharedState};
pub use health::{Liveness, Readiness};
pub use mode::{ModeChangedEvent, ModeError, ModeEventQueue, ModeService};
pub use rooms::{LiveView, Rooms};
pub use settings::{AuthProvider, Settings, SettingsService};
pub use state::{AppState, LayoutRecord, RecordEntry, RoomRecord, UserRecord};
pub use users::{LdapUser, MockLdapProvider, Users};
