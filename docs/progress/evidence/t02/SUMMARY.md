# T02 evidence — целостность БД и жизненный цикл раскладок

Дата: 2026-10-21. Коммит: `265a80b` + follow-up (M1–M4: canonical/active helper, dedup predicate, docs).
Ревью: `layout-ui` — **9.5/10**; `media-engine`, `directory-db` — см. отчёты в сессии.

## Что закрыто

1. COW валидирует draft до вставки клона; на 422/409 документ не меняется
   (тест `t02_cow_validation_error_leaves_no_ghost`).
2. Явная карта `active_layouts`; attach не пересортировывает `links`;
   `A → B → «Применить A»` даёт A (`t02_attach_apply_sequence...`).
3. Canonical default применяется после manual; generic POST переиспользует
   «Общую» (два POST → один id).
4. `detach`/`remove_global` не удаляют shared/default: 404/409 с отсутствием
   мутаций; после удаления одиночной раскладки возвращается canonical default.
5. COW alias резолвится на read-path (`get_linked`, `abonents`, `templates`,
   `list_linked`), очищается при detach только в своём scope; conference COW
   обновляет session-mirror; attach раскладки (link) отвечает 204 без body,
   `X-VKS-Layout-Id`/200 `layout_id` — у abonent/contact attach.
6. Server + `vks-db-dedup --apply` используют один `flock` `<db>.lock`;
   backup `VACUUM INTO` + `quick_check`, dry-run read-only, `--apply` идемпотентен.
7. Dedup на копии живой БД: 30→22 layouts, 15→7 defaults, 6 legacy manual-default
   сохранены, 152 non-empty video links, active 152 (11 non-default по T01-effective,
   0 invalid), повтор — 0 изменений. Старый бинарь active не знает, порядок
   апгрейда: stop old → backup/dedup → start new (см. deploy/README.md).

## Гейты

- `cargo test --workspace --all-targets` — **996 passed / 0 failed / 6 ignored**.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
- `cargo fmt --all -- --check` — OK.
- Black-box: второй сервер на том же SQLite → exit 78; CLI `--apply` при held
  lock → exit 1, БД не изменена (`crates/vks-storage/tests/dedup_lock.rs`).

## Ограничения

- T02 закрывает контракт раскладок и push; фактическая геометрия/применение в
  MCU — T03. `default_room_mode=sfu` не меняется: composite строится для
  SIP/H.323 gateway, SFU-раскладка клиентская.
- Live-БД мигрируется только после остановки сервиса и backup; на момент
  evidence применено к копии, не к live.
- Живой Playwright E2E на T02 не прогонялся (нужен деплой нового бинаря):
  переносится на пост-деплой smoke/T03; API/unit-гейты и независимые curl-репро
  ревьюеров зелёные. Это формальный waiver, а не забытый пункт.
