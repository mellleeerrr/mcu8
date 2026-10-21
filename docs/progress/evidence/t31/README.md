# T3.1 — мёртвый код и лишние зависимости

Проверка, что legacy-роуты `/records` удалены, актуальные `/videorecords`
работают, а workspace не держит неиспользуемые **прямые** зависимости.
Скрипт не поднимает браузер: он проверяет API/файлы Cargo.toml и запускает
тесты. Провенанс (`git_head`, `git_dirty`, `binary_*`, `server_pid`) — в
`summary.json.build`; на прогоне `08486e7` дерево чистое.

## Как воспроизвести

```bash
node scripts/evidence/t31-evidence.js
```

## Результат

**ok=true, 7/7** гейтов; `admin_lib=231` тест, integration-тест пакета D
зелёный, `cargo check --workspace --all-targets` — 0.

| Гейт | Что реально проверяет |
|---|---|
| `legacy_records_removed` | `GET /api/v3/records` и `/api/v3/ui/records` → **501** (роуты не зарегистрированы) |
| `videorecords_works` | `GET /api/v3/videorecords` → 200 и `{count:0}` |
| `unused_deps_removed` | в `crates/vks-server/Cargo.toml` нет **прямого** `vks-layout`; в `vks-gateway-sip`/`vks-gateway-h323` нет прямого `vks-core` |
| `no_dead_records_handler` | `grep` по всему `crates/vks-admin/src` не находит `ui_records`/`/api/v3/records`/`/api/v3/ui/records` |
| `layout_mode_contract_test` | `cargo test -p vks-server --test network_integration test_package_d_mcu_pipeline_handover_and_pnm_control_events` — 1 passed |
| `admin_tests_green` | `cargo test -p vks-admin --lib` ≥ 220 passed (факт — 231) |
| `workspace_checks` | `cargo check --workspace --all-targets` — exit 0 |

`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` и
`cargo test --workspace` (892 passed, 4 ignored) проверены вручную перед
коммитом; в `summary.json` они не записываются.

## Что удалено/починено

- `/api/v3/records`, `/api/v3/ui/records` и обработчик `shell::ui_records` —
  мёртвый дубль `/videorecords`; UI-страница «Медиафайлы» ходит в
  `/api/v3/videorecords`.
- `vks-layout` больше не **прямая** зависимость `vks-server` (остаётся
  транзитивной через `vks-media-mcu` — это ожидаемо); `vks-core` убран из
  прямых зависимостей `vks-gateway-sip`/`vks-gateway-h323`.
- `POST /system/restart_*` — только POST (GET → 405), см. T2.3.
- Integration-тест `test_package_d_*`: создаёт раскладку, attach’ит её к сессии
  и передаёт `video_layout_id`; проверяет `mode=auto`/`topology=mcu` вместо
  прежнего ожидания 422.

## Ограничения

- Гейты `unused_deps_removed`/`no_dead_records_handler` — статические проверки
  конкретных файлов, а не полный анализ символов/роутов; полный `cargo tree`
  намеренно не пуст из-за транзитивного `vks-layout`.
- Legacy `/api/v3/records` удалён без deprecation-периода: он никогда не был
  публичным контрактом (только внутренний дубль).
- `workspace_checks` — только `cargo check`; fmt/clippy/test запускаются в
  общем pre-commit цикле и отдельном CI, а не внутри этого скрипта.
