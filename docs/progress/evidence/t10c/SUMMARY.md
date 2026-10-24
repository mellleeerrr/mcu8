# T10c evidence: прод-потребитель LayoutChanged в SDK

Дата: 2026-10-24. Ветка: master (HEAD 379f7cf + T10c).

## Что закрыто

- `ConferenceEvent::LayoutChanged { layout, preset, mode }` — raw `preset`
  (`codian_07`, `preset_equal`) и нормализованный сервером `mode`
  (`auto|dynamic|manual`); `""` = unknown, не `auto`.
- `Conference` хранит метаданные последнего снимка: `layout_preset()`,
  `layout_mode()`; join-снимок и rejoin их заполняют (событием не дублируются).
- Version-guard: источник истины — `media().layout().version`; `drain_events`
  выбрасывает кадры `version <= current` (не применяет и не отдаёт UI),
  `on_layout_changed` удалён как мёртвый второй путь.
- Reconnect: rejoin-снимок сбрасывает эпоху версий (снимок применяется,
  очередь чистится; пустой снимок очищает layout/meta). Успешный resume
  снимка не несёт — layout/meta очищаются, queued `LayoutChanged` прежней
  эпохи отбрасываются, не-layout события сохраняются.
- Попутно (отдельно отмечено в коммите): `RoomModeChanged` мапился в
  `"room_mode_mcu"` вместо `"mcu"` — push-переключение SFU/MCU молча не
  работало; исправлено + тест `live_mode_push_switches_renderer`.
- PNM admin-WS событие не добавлялось (YAGNI): в PNM нет JS-подписчика на
  layout, редактор работает через REST/resources; мастер-план допускает
  «PNM WS **или** SDK».

## Гейты (2026-10-24, всё зелёное)

| Гейт | Команда | Результат |
|---|---|---|
| fmt | `cargo fmt --all -- --check` | exit 0 |
| clippy | `cargo clippy --workspace --all-features --all-targets --locked -- -D warnings` | exit 0 |
| tests | `cargo test --workspace --all-targets --locked` | 1333 passed, 0 failed, 8 ignored |
| wasm | `cargo check -p vks-client-sdk --target wasm32-unknown-unknown --locked` | exit 0 |

SDK-тесты: 51 (было 44): join-снимок + metadata, raw push, stale
(equal/lower) end-to-end, v0-снимок, unknown metadata затирает подпись,
rejoin epoch, rejoin без снимка (clear), resume без снимка (stale layout-кадры
отброшены, mode-событие сохранено), push-переключение режима.

## Ревью

Два ревьюера: 7/10 и 8.5/10 (P1 fmt, resume-ветка, мёртвый
`on_layout_changed`, слабый empty-metadata тест, docs/evidence). Все пункты
исправлены; дельта-ревью — см. итог раунда.
