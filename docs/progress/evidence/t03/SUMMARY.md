# T03 evidence — применение раскладки в gateway/MCU

Дата: 2026-10-21. Задача: T03 (P0), после T02 (`8788c46` + F1 `ed24d02`).
Ревью: T02 F1 — `media-engine`; T03 — `media-engine` / `protocols-net`, см. отчёты сессии.

## Что закрыто

1. **auto/default доезжает до существующей MCU-комнаты.** `gateways.rs::apply_layout`
   больше не делает no-op для `assignments.is_empty() && mode != manual`; для
   `mode=="auto"` отправляет пустую geometry, и движок сам строит сетку по числу
   забридженных паблишеров. Возврат ручная 2×2 → «Общая» даёт 2 ячейки на двух
   участниках (тест `gateway_manager_auto_layout_uses_participant_count_not_preset_13`),
   а не 13 плиток `preset_default_13`.
2. **Нет композита для пустой SFU-сессии.** Если MCU-комнаты ещё нет,
   `McuEngine::apply_layout` сохраняет pending-layout и не аллоцирует 2 МБ
   композита; `bridge_participant`/`ensure_room` применяют pending на первом
   bridge. Pending не теряется на audio-only первом bridge
   (`pending_layout_survives_audio_only_first_bridge`), очищается при
   `destroy_room`; `ensure_room` не перезаписывает уже существующую раскладку.
3. **Repair только по geometry.** `free_layout_slot` берёт свободные slot id
   из `layout.geometry`; при пустой geometry — legacy-скан u32; при отсутствии
   свободного geometry-слота участник не назначается (warn), full-canvas нет.
   `sanitize_assignments` отбрасывает назначения вне geometry до построения
   `Layout`. Тест: 4 geometry + 5 паблишеров → ровно 4 slot_rect.
4. **Явный 1-based контракт.** Inference по «есть ли ноль»/`max(position)` убран:
   `payload.position_base` (0/1, default 1); `calculate`/`attach_abonent` —
   1-based, `position_base:0` сохраняет разреженные 0-based `[2,3]` как 2/3.
   Позиция ниже base логируется и пропускается, а не молча сдвигается.
5. **z_order end-to-end.** `PixelRect.z` ← `Slot.z_index`; gateway для
   manual-пресетов берёт `geometry_for_manual` (вендорный z_order), compositor
   сортирует `(z, key)`; PiP/overlap реально перекрывает крупную зону.
   Сортировка до 128 слотов на стеке — без аллокации на кадр.
6. **Монотонная версия.** `apply_layout` ставит
   `max(incoming, current+1)`, bridge бампает один раз при посадке; двойного
   инкремента нет, auto без изменения назначений всё равно перестраивает
   композитор.

## Гейты

- `cargo test --workspace --all-targets` — **1005 passed / 0 failed / 6 ignored**.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
- `cargo fmt --all -- --check` — OK.
- F1 T02: `t02_canonical_create_persists_after_reload` — canonical «Общая»
  сохраняется в storage и находится после `sync_from_storage`; отдельный коммит
  `ed24d02`.

## Ограничения / следующие шаги

- Pending-layout живёт в памяти: при рестарте сервера комната создаётся заново,
  push раскладки повторяется при старте сессии; отдельного persistence нет.
- `manual` с пустыми slots и `preset_default_13` (legacy `c438981c`) остаётся
  явной 13-ячейковой сеткой — это выбранная вручную схема, не auto.
- Живой Playwright E2E и SIP/H.323-проверка смены 2×2 → «Общая» → 2×2 —
  после сборки/деплоя нового бинаря (T04a); live-БД/сервис в T03 не трогались.
