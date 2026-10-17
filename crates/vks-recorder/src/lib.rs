//! `vks-recorder` — запись конференций (composite в MCU, multi-track в SFU).
//!
//! Фаза 7 (DEVELOPMENT_PLAN.md): recorder (композит + multi-track),
//! storage (S3/локальный + encryption at rest AES-256-GCM), retention
//! (90 дней, настраиваемо, auto-delete), consent, download (token TTL 1h).
//!
//! **В этом крейте — самодостаточное ядро** (без реального I/O,
//! кодеков, сети):
//! - [`recording`] — [`RecordingSession`], [`Segment`], [`RecordingMeta`]
//!   (детерминированное, часы параметризованы).
//! - [`storage`] — абстракция [`RecordingStore`] + [`MemoryStore`]
//!   (реальные S3/postgres — за фичами, позже).
//! - [`retention`] — [`RetentionPolicy`] (age + count),
//!   [`select_for_purge`].
//!
//! Что мокнуто / что реально (подробнее в [README.md](README.md)):
//! - **Реальное**: модель метаданных, детерминированный lifecycle
//!   (start/stop/append), retention-логика, in-memory store.
//! - **Мокнуто / за фичами позже**: бинарный контент (MP4/chunks),
//!   реальные codecs, S3/локальный диск, encryption at rest (DEK + KMS),
//!   download token (TTL 1h), consent flow (Meeting UI).
//!
//! Источник: `docs/architecture/CLUSTERING.md` (retention, encryption),
//! `docs/rules/BACKEND_RULES.md` §3.3, §5.3.
//!
//! Дерево: vks-core, vks-proto, vks-common (концептуально); по типам
//! крейт самодостаточен (dep-light: thiserror, uuid).
//!
//! ## Инварианты
//! - Детерминированность: время — через [`Clock`](recording::Clock).
//! - **No `unsafe`** (CODE_STYLE §6).
//! - English identifiers, Russian comments (CODE_STYLE §8).

mod recording;
mod retention;
mod storage;

pub use recording::{
    Clock, ClockMs, RecordingError, RecordingMeta, RecordingMode, RecordingSession, RecordingState, Segment,
    SegmentKind,
};
pub use retention::{
    is_expired, is_expired_by_age, is_expired_by_count, select_for_purge, RetentionPolicy, MS_PER_DAY,
};
pub use storage::{MemoryStore, RecordingStore};
