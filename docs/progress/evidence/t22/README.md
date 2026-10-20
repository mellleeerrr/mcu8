# T2.2 — История конференций (call_history)

Дата: 2026-10-21. Evidence UI-прогона на vendor-оболочке 2.2.1.66 против
нашего `/api/v3` (бинарник собран после коммита, см. `summary.json.build`).

## Что проверено

Создана конференция `T22History_<stamp>` с vendor-идентификаторами:
`primary_dtmf` (9 цифр) и `primary_route` (имя маршрута), запущена сессия,
добавлен исходящий вызов `sip:127.0.0.1:5999` (реальная попытка дозвона),
сессия остановлена. Затем:

| Гейт | Проверка |
|---|---|
| `conference_dtmf` / `conference_route` | `POST /api/v3/conferences` отдаёт `primary_dtmf.name` = 9 цифр, `primary_route.name` = display_name |
| `history_list_schema` | `GET /api/v3/call_history` → `{count, sessions:[{id, display_name, started_at, stopped_at, stop_reason, abonents, creator, stopper, dtmf, route, payload}]}`; `stop_reason=Manual`, `payload.bitrate` число, пустой `stopped_by` → `null` как у vendor |
| `history_date_filter` | `date_from`/`date_to` (секунды, vendor api.bundle) сужают список и экспорт: будущая дата → 0, широкий диапазон → запись |
| `history_dtmf_route` | сессия в истории сохранила DTMF/маршрут конференции |
| `history_detail_calls` | `GET /api/v3/call_history/{id}` → `session.call_strings[]` с `connection_url`, `display_name`, `invitations[].finish_reason` |
| `ui_session_history` / `ui_call_history` | vendor-эндпоинты шапки и таймлайна карточки истории |
| `export_zip` / `export_zip_integrity` / `export_csv_rows` | `GET /api/v3/call_history/export` → `{filename}`; ZIP проверяется `unzip -t` и реальным извлечением: CSV с заголовком «Название», строкой конференции, DTMF и именем `Administrator` (а не UUID) |
| `export_invalidated_after_clean` | после `clean` ранее выданный `/static/call_history/{filename}` → 404 (старые выгрузки не живут вечно) |
| `history_pagination` | `page_size=1` не ломает `count` |
| `history_list_ui` | страница `/pnm/history` показывает конференцию, `ID: 9 цифр`, маршрут (PNG 01) |
| `history_detail_ui` | клик по строке открывает `/pnm/history/{id}`: DTMF, маршрут, вызов и URI на таймлайне (PNG 02) |
| `history_export_ui` | кнопка «Действия» в строке шлёт именно `GET /api/v3/call_history/{id}/export`, скачанный ZIP проходит `unzip -t` |
| `history_clean` | `POST /api/v3/call_history/clean` → backup-ZIP, список пуст |
| `history_clean_ui` | после очистки UI-список пуст без ошибок (PNG 03) |

Итог: **ok=true**, 20/20 acceptance (включая `clean_build` и
`binary_built_after_head`), 12 шагов, 3 PNG, 0 HTTP ≥ 400,
0 console-ошибок (vendor-allowlist не понадобился).

## Реализация (backend)

- `crates/vks-admin/src/api_v3/history.rs`: `HistoryState` (MAX 500, дедуп по
  `room_id+started_ms` — два старта в одну секунду не склеиваются), запись в
  `record_stopped` ДО очистки `call_attempts`/участников, снимок payload
  конференции, call `started_at` = время адреса/вызова, `abonents` из
  participant-снимка, ZIP (store, корректный local-header offset 0) + CSV
  (CRC32, имена вместо UUID, DTMF в ID, защита от CSV-формул), backup и
  инвалидация старых выгрузок при clean, `static_export` с непредсказуемым
  именем (uuid-токен, bounded 32 архива), per-session
  `GET /call_history/{id}/export`.
- Запись вызывается из `stop_session` (Manual) и delete-пути — только для
  реально запущенной сессии (маркер `started_at`) и после успешного удаления;
  никогда не запускавшаяся конференция в историю не попадает.
- Конференция получает `route`+`dtmf` при создании; quick-start
  (`POST /sessions/quick`) и повторный `start_session` дозаполняют
  creator/route/DTMF (`ensure_conference_identity`) — иначе vendor-UI
  показывает «ID: -». `start_session` после повторного старта сбрасывает
  `created_ms`, ошибки storage не проглатываются.
- Персистентность — в том же configuration-document, что ресурсы (переживает
  рестарт), покрыто тестом `history_survives_restart_from_configuration_document`;
  запись/clean держат resources-lock до durable-коммита.
- Доступ: чтение — Moderator+, clean — Operator+; Participant получает 403.
- `system/time` и `profile.time_zone_id` отдают IANA-таймзону хоста
  (`Europe/Moscow`), поэтому список и карточка истории показывают одно время.

## Тесты

`cargo test -p vks-admin --lib history` — 12 тестов (список/детали/ui, фильтры
дат/сортировка/пагинация, CSV-экранирование и формулы, ZIP store с реальным
извлечением/CRC, dedupe, restart, clean+инвалидация, DTMF/route/call-snapshot,
per-session export, quick-start identity, delete-семантика). Полный прогон:
190 vks-admin + 34 vks-auth, clippy/fmt без замечаний.

## Воспроизведение

```bash
cargo build --release -p vks-server
./scripts/run-local.sh restart
PLAYWRIGHT_MODULE=<path>/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t22-evidence.js
```

Скрипт сам чистит историю в начале и в конце, создаёт уникальные сущности;
`summary.json` пишется всегда, при провале — `ok:false` и exit 1.
