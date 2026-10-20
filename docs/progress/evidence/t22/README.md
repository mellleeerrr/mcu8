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
| `history_list_schema` | `GET /api/v3/call_history` → `{count, sessions:[{id, display_name, started_at, stopped_at, stop_reason, abonents, creator, stopper, dtmf, route, payload}]}`; `stop_reason=Manual`, `payload.bitrate` число |
| `history_dtmf_route` | сессия в истории сохранила DTMF/маршрут конференции |
| `history_detail_calls` | `GET /api/v3/call_history/{id}` → `session.call_strings[]` с `connection_url`, `display_name`, `invitations[].finish_reason` |
| `ui_session_history` / `ui_call_history` | vendor-эндпоинты шапки и таймлайна карточки истории |
| `export_zip` / `export_csv_header` | `GET /api/v3/call_history/export` → `{filename}`, ZIP по `/static/call_history/{filename}`, CSV с заголовком «Название» (UTF-8, `;`) |
| `history_pagination` | `page_size=1` не ломает `count` |
| `history_list_ui` | страница `/pnm/history` показывает конференцию, `ID: 9 цифр`, маршрут (PNG 01) |
| `history_detail_ui` | клик по строке открывает `/pnm/history/{id}`: DTMF, маршрут, вызов и URI на таймлайне (PNG 02) |
| `history_export_ui` | кнопка «Действия» в строке скачивает ZIP (`GET /api/v3/call_history/{id}/export`, `Content-Disposition` с реальным именем) |
| `history_clean` | `POST /api/v3/call_history/clean` → backup-ZIP, список пуст |
| `history_clean_ui` | после очистки UI-список пуст без ошибок (PNG 03) |

Итог: **ok=true**, 15/15 acceptance, 12 шагов, 3 PNG, 0 HTTP ≥ 400,
0 console-ошибок (vendor-allowlist не понадобился).

## Реализация (backend)

- `crates/vks-admin/src/api_v3/history.rs`: `HistoryState` (MAX 500, дедуп по
  `(room_id, started_at)`), запись в `record_stopped` ДО очистки
  `call_attempts`/участников, снимок payload конференции, ZIP (store) + CSV
  (CRC32) без внешних зависимостей, backup при clean, `static_export`,
  per-session `GET /call_history/{id}/export`.
- Запись вызывается из `stop_session` (Manual) и `delete_room` (Deleted), а
  также при остановке через UI; `start_session` после повторного старта
  сбрасывает `created_ms`, чтобы новая сессия не склеивалась со старой.
- Конференция получает `route`+`dtmf` при создании; quick-start-сессия
  дозаполняется в `start_session` (`ensure_conference_identity`) — иначе
  vendor-UI показывает «ID: -».
- Персистентность — в том же configuration-document, что ресурсы (переживает
  рестарт), покрыто тестом `history_survives_restart_from_configuration_document`.
- Доступ: чтение — Moderator+, clean — Operator+; Participant получает 403.

## Тесты

`cargo test -p vks-admin --lib history` — 10 тестов (список/детали/ui, фильтры/
сортировка/пагинация, CSV-экранирование, ZIP store, dedupe, restart, clean,
DTMF/route/call-snapshot, per-session export). Полный прогон: 188 vks-admin +
34 vks-auth, clippy/fmt без замечаний.

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
