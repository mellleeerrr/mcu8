# S12 (фаза 3) — голосовая пересадка по Codian §2.2: главное окно, недавность речи, перенос кадров

Дата: 2026-10-23. Спецификация: `Codian_MCU4200_Video_Layouts_Specification.docx`,
§2.2 «Voice-controlled layout», §2.1 (статичность 52 PNM-макетов).

## Требования спецификации и что сделано

| §2.2 | Требование | Реализация |
|---|---|---|
| 1 | Активный спикер — в **самом крупном** окне | `RoomState::refresh_seating`: главное окно = `vks_layout::main_window` (максимум по площади), в него — `important.or(active_speaker)` |
| 2 | «Важный» участник принудительно в крупном окне независимо от речи | `McuEngine::set_important_participant` + приоритет `important` над `active_speaker` |
| 3 | Мелкие ячейки — по **убыванию недавности речи** | `RoomState::recent_speakers` (MRU, 16 записей), заполняет неглавные слоты по возрастанию номера |
| 4 | Никто не исчезает из раскладки при пересадке | жильцы только меняются местами; множество участников в слотах сохраняется |
| §2.1 | 52 PNM-макета статичны — голос их не трогает | `refresh_seating` выходит сразу для `Grid`/`Custom` (у PNM-раскладок `LayoutKind::Custom`, геометрия из `payload.preset`) |

Дополнительно требуется, чтобы **видео переезжало вместе с участником**: кадр
принадлежит участнику, а не номеру слота. Без переноса после пересадки в главном
окне остаётся видео прежнего жильца (до следующего кадра этого участника, а если
он выключил камеру — навсегда).

## Как устроено

```
process_audio (VAD-флип «заговорил»)
   └─ note_recent_speaker(pid)                      // MRU
   └─ refresh_seating()                             // одно правило на всё размещение
         ├─ main = main_window(layout)              // максимум площади
         ├─ order = important|active_speaker → recent_speakers → join_order
         ├─ desired = {main: order[0], остальные слоты по возрастанию: order[1..]}
         ├─ размещение уже верное → None            // нет ложных LayoutChanged
         └─ запись + bump_version + sync_descriptor_assignments
              → SeatingMove { old_assignments, new_layout }
McuEngine::tick_audio_at / set_important_participant
   └─ rekey_frames(old_assignments, new_layout)     // пиксели едут за участниками
   └─ emit(events)                                  // после снятия блокировки rooms
```

`promote_to_main_window` из `vks-layout` удалён: он решал ту же задачу
наполовину (только главное окно, без порядка мелких ячеек), и в движке было два
прохода вместо одного. Осталось одно правило — в движке, который знает и
спикера, и недавность.

## Доказательства

```
$ cargo test -p vks-media-mcu --lib -- thumbnails voice_activation
test engine::tests::voice_activation_leaves_grid_and_custom_layouts_alone ... ok
test engine::tests::voice_activation_moves_the_main_window_to_the_active_speaker ... ok
test engine::tests::voice_activation_works_without_an_event_sink ... ok
test engine::tests::thumbnails_are_ordered_by_speech_recency ... ok
test engine::tests::voice_activation_moves_the_frames_with_the_participants ... ok
test result: ok. 5 passed
```

- `voice_activation_moves_the_frames_with_the_participants` — у A и B разные
  яркости; после того как заговорил B, в главном окне пиксели B, в полосе —
  A (`rekey_frames`). Семплирование по центру окна: кадр 320×180 вписывается с
  letterbox, у края слота — чёрная полоса.
- `thumbnails_are_ordered_by_speech_recency` — 1+3: A говорил, затем B ⇒
  главное окно у B, A (недавний) — первая мелкая ячейка, C (не говоривший) —
  последняя.
- `important_participant_outranks_the_active_speaker` — «Важный» вытесняет
  спикера, снятие статуса возвращает голосовую активацию; проверяет и
  идемпотентность пересадки (нет `LayoutChanged` при повторном пересчёте).
- `voice_activation_leaves_grid_and_custom_layouts_alone` — §2.1.

Полный гейт: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` — exit 0,
45 наборов, 0 провалов (`vks-media-mcu` 101 тест, `vks-layout` 51).

## Что осталось по S12

Автовызов `assign_by_role` (`vks_layout::roles`, роли окон `default`/`fixed`/
`active`/`rolling`) при применении комбинированной раскладки Codian. Подводные
камни:

1. В `vks_core::ports::media_engine::LayoutDescriptor` **нет поля ролей** —
   нужен либо `roles: Vec<SlotRole>` (аддитивно, `#[serde(default)]`, но
   ломает ~15 литералов в тестах и `vks-media-sfu`), либо идентификатор
   Codian-раскладки в `layout_id`, по которому `vks_layout::codian::roles(id)`
   сам отдаёт роли.
2. Сервер уже резолвит `payload.preset` в `vks_layout::preset::resolve`, но в
   дескриптор попадает `layout["id"]` из UI — семантика этого поля не
   подтверждена живым запросом; без неё `codian::roles` вернёт `None`.
3. Пересчёт по ролям должен идти через тот же хук `refresh_seating` (он уже
   вызывается на каждый флип VAD и умеет `rekey_frames`), иначе получится
   третий путь пересадки.
