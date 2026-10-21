# T3.1 — мёртвый код и лишние зависимости

Проверка, что фронтенд больше не ходит в снятые legacy-роуты, а workspace не
тянет неиспользуемые крейты. Прогон на чистом дереве: HEAD `045666b`,
бинарь `vks-server` собран после HEAD.

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t31-evidence.js
```

## Результат

**ok=true, 7/7** acceptance-гейтов, 3 шага, 0 HTTP ≥ 400, 0 console error,
`clean_build=true`, `binary_built_after_head=true`.

| Гейт | Что доказывает |
|---|---|
| `legacy_records_removed` | `GET /api/v3/records` и `/api/v3/ui/records` оба → 501 (роуты удалены, а не отдают пустоту) |
| `videorecords_works` | `/api/v3/videorecords` и `/api/v3/system/about` → 200, UI-страница `mediafiles` рендерится без 4xx |
| `unused_deps_removed` | `cargo tree -p vks-server | grep vks-layout` пуст; в `vks-gateway-sip`/`vks-gateway-h323` нет прямого `vks-core` |
| `no_dead_records_handler` | в `crates/vks-admin/src` нет `fn ui_records`/маршрута `/records` |
| `layout_mode_contract_test` | `cargo test -p vks-server --test network_integration test_package_d_mcu_pipeline_handover_and_pnm_control_events` зелёный (create/attach layout + `video_layout_id`, `mode=auto`, `topology=mcu`) |
| `admin_tests_green` | `cargo test -p vks-admin --lib` ≥ 220 passed |
| `workspace_checks` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-run` |

## Что удалено/починено

- `/api/v3/records`, `/api/v3/ui/records` и обработчик `shell::ui_records` —
  мёртвый дубль `/videorecords`; UI-страница «Медиафайлы» ходит в
  `/api/v3/videorecords`.
- `vks-layout` больше не direct-зависимость `vks-server` (используется только
  через vks-conference); `vks-core` убран из `vks-gateway-sip`/`vks-gateway-h323`.
- `POST /system/restart_*` — только POST (GET → 405), см. T2.3.
- Integration-тест `test_package_d_*`: вместо ожидания 422 создаёт раскладку,
  attach’ит её к сессии и передаёт `video_layout_id`; проверяет `auto`/`mcu`.

## Ограничения

- «Неиспользуемые крейты» проверяются `cargo tree`/`cargo machete`-эвристикой,
  а не полным анализом символов: `vks-layout` остаётся транзитивной
  зависимостью и это ожидаемо.
- Legacy `/api/v3/records` удалён без deprecation-периода: он никогда не был
  публичным контрактом (только внутренний дубль).
