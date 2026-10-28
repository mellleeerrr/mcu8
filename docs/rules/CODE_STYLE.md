# CODE STYLE — правила стиля кода

> Статус: **v0.1 (черновик)**.

## 1. Форматирование
- `rustfmt` со стандартными настройками (`.rustfmt.toml`).
- `cargo clippy --workspace --all-targets -- -D warnings`.
- Максимальная длина строки: 120 символов.

## 2. Ошибки
- Единый `vks_common::Error` (thiserror).
- SDK — собственный `vks_client_sdk::Error`.
- Не прокидывать `anyhow` в публичный API.
- Error messages: английский (identifiers), русский — в docs.

## 3. Логирование
- `tracing` (не `log`, не `println!`).
- Production: level `info`. Debug — только по требованию.
- Structured fields: `room_id`, `participant_id`, `track_id` (UUID, не имя).
- НЕ логировать: SDP, audio/video payload, JWT (только `jti`), password, API key (prefix).

## 4. Тесты
- Unit: каждый крейт, покрытие ≥ 90% для vks-core.
- Integration: mock-порты, без I/O.
- E2E: chromiumoxide (реальный браузер).
- Fuzzing: cargo-fuzz на vks-proto (deserialize), SDP parse, ICE parse.
- Property-based: proptest для layout (инварианты: rects не пересекаются; полные ряды покрывают ширину, неполный последний ряд центрируется, кроме холста уже cols — там ряд делит ширину).

## 5. Feature flags
- Кодеки: `vp8`, `vp9`, `h264`, `h265`, `av1`, `opus`, `g722`, `g711`.
- Runtime features: `dev-tools` (debug endpoints), `e2e` (test hooks).
- Запрещено: `#[cfg(feature = ...)]` для логики (только для опциональных зависимостей).

## 6. unsafe
- Только с ADR + `// SAFETY:` комментарием.
- Forbidden в `vks-core`.
- FFI (H323Plus, ffmpeg): одна граница, panic hook, watchdog.

## 7. Эмодзи
- Допустимы в «декоративных» деревьях (ARCHITECTURE.md §3).
- В остальной документации — без эмодзи.

## 8. Язык
- Код, идентификаторы, ADR-названия — английский.
- Комментарии, docs, MASTER_TASK_LIST — русский.
- ADR-названия в DECISIONS.md — русский (для людей), в файлах — английский.

## 9. Коммиты
- Формат: `type(scope): description`.
- Types: feat, fix, docs, refactor, test, chore, ci, perf.
- Каждый коммит с кодом обновляет статус в `docs/progress/MASTER_TASK_LIST.md` (единственный живой трекер).
