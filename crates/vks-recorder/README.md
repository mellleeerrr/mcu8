# vks-recorder

Запись конференций: композит (MCU) и multi-track (SFU), storage, retention.

Фаза 7 (DEVELOPMENT_PLAN.md) — recorder, storage, retention, download,
consent. Этот крейт реализует **самодостаточное ядро** (без реального I/O,
кодеков и сети), полностью тестируемое.

## Что записывает

- **MCU (composite)** — один MP4-файл: композитное видео
  (`SegmentKind::CompositeVideo`) + общий audio mix (`SegmentKind::Audio`).
- **SFU (multi-track)** — отдельные файлы per participant: видео/аудио-треки
  (`SegmentKind::TrackTrack`) + общий audio track (`SegmentKind::Audio`).

Ядро оперирует только метаданными (`Segment`, `RecordingMeta`). Бинарный
контент (MP4/chunks) живёт в реальном бэкенде (коэки + S3/лоksный диск)
и привязывается к `RecordingMeta::id`.

## Модули

- `recording.rs` — `RecordingSession` (start/stop/append_segment),
  `Segment`, `RecordingMeta`, `SegmentKind`, `RecordingMode`,
  `RecordingState`, `Clock` (параметризация часов → детеризность в
  тестах).
- `storage.rs` — trait `RecordingStore` (`save`/`get`/`list`/
  `purge_expired` + `list_all`) + `MemoryStore` (in-memory для
  для Реальные Бэкенды (S3, S3+postgres, локальный диск) —
  за фичами.
- `retention.rs` — `RetentionPolicy` (`max_age_days`, `max_rooms`),
  `is_expired_by_age`, `is_expired_by_count`, `is_expired`,
  `select_for_purge`. Дефолт: 90 дней (BACKEND_RULES §3.3) + 10 записей
  на комнату.

## Что реально / что мокнуто

**Реальное (в этом крейте):**
- Модель метаданных: `RecordingMeta`, `Segment`, `SegmentKind`,
  `RecordingMode`.
- Детерминированный lifecycle: `start`/`stop`/`append_segment`; время —
  через `Clock` (`Box<dyn Fn() -> u64>`); в тестах — «замерзшие» часы
  (`new_at_zero`) или кастомная функция.
- Retention: age + count критеры (OR), `select_for_purge` (вруппат.
- In-memory storage: `MemoryStore`.

**Мокнуто / за фучами позже (не в этом кренте):**
- Бинарный контент (MP4/chunks) — реальные codecs + файловый бэст.
- S3 / postgres / локальный диск (реальные имплементации
  `RecordingStore`).
- Encryption at rest: AES-256-GCM, per-recording DEK + KMS master
  (BACKEND_RULES §3.3).
- Download: временный token (TTL 1h), share-ссылка.
- Recording consent (banner + modal в Meeting UI, RBAC §1.7).

## Зависимости

Dep-light (workspace): `thiserror`, `serde`, `uuid`. Без `tokio`, без
`anyhow` в публичном API (CODE_STYLE §2).

## Тесты

`cargo test -t vks-recorder` — полностью без I/O: session lifecycle,
storage, retention (age/count/boundary), `select_for_purge`.
