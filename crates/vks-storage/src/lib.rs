//! `vks-storage` — хранилище (postgres/sqlite/memory + migrations).
//!
//! Фаза 1 (DEVELOPMENT_PLAN.md), ADR-0008 (sqlx + PostgreSQL/SQLite/memory):
//! - [`memory::MemoryStorage`] — thread-safe in-memory (тесты);
//! - [`sqlite::SqliteStorage`] — SQLite (single-node);
//! - [`postgres::PostgresStorage`] — PostgreSQL (cluster/prod);
//! - [`migrations/0001_initial.sql`](migrations) — общая схема (users, rooms,
//!   room_mode_override, layouts, api_keys, audit_log).
//!
//! Crate самодостаточен: порт `Storage` строится параллельно в `vks-core`,
//! поэтому здесь свои типизированные строки ([`rows`]) и [`error::Error`].

pub mod core_adapter;
pub mod dedup;
pub mod error;
pub mod memory;
pub mod postgres;
pub mod rows;
pub mod sqlite;

pub use dedup::{dedup_document, DedupReport};
pub use error::{Error, Result};
pub use memory::MemoryStorage;
pub use postgres::PostgresStorage;
pub use sqlite::SqliteStorage;
