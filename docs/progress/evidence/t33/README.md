# T3.3 — финальный обход страниц PNM и таблица паритета

Read-only сквозной прогон vendor-UI по всем основным и административным
страницам + проверка 18 контрактов `/api/v3`, которыми живёт UI. Ничего не
создаёт, не меняет и не удаляет. Прогон на чистом дереве: HEAD `21edc17`,
бинарник собран после HEAD (`clean_build`, `binary_built_after_head` — в
`summary.json`).

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t33-evidence.js
```

## Результат

**ok=true, 5/5** acceptance-гейтов: 22 страницы (каждая отдала HTTP 200, совпал finalUrl и не было missing-лейблов/console error),
19 проверок API (18 контрактов, один повтор), 7 PNG, per-page артефакт `page_walk` в summary, 0 HTTP ≥ 400,
0 наших console error, 24 известных vendor Chart.js console **error** (allowlisted). «Калибровка» удалена:
deep-link `/pnm/administration/calibration` уводит на статистику, API отдаёт
`{calibration:null}`.

## Таблица паритета (UI ↔ наш API)

| Страница PNM | Реальный URL | Ключевые запросы UI | Evidence |
|---|---|---|---|
| Конференции (дашборд) | `/pnm/monitoring/sessions` | `/ui/sessions`, `/conferences`, `/abonents/count` | `01-sessions.png`, t16 |
| Справочник | `/pnm/addressbook/users` | `/call_strings`, `/users`, `/groups`, `/references` | `02-addressbook.png`, t21 |
| История | `/pnm/history` | `/api/v3/call_history`, `/api/v3/ui/session_history/{id}`, export | `03-history.png`, t22 |
| Статистика | `/pnm/administration/statistics` | `/ui/system/status`, `/system/stats/*`, `/engine/diagnostics` | `04-statistics.png`, t24 |
| Вызовы | `/pnm/administration/calls` | `/settings/calls`, `/settings/calls/sip|h323` | `05-calls.png`, t23 |
| Сеть / интерфейсы | `/pnm/administration/network` | `/system/status/network`, `/system/network/interfaces` | `06-network.png`, t24 |
| Лицензия | `/pnm/administration/license` | `/license`, `/features` | `07-license.png`, t24 |
| Медиафайлы | `/pnm/administration/mediafiles` | `/api/v3/videorecords` (read-only) | t24 |
| Терминалы | `/pnm/administration/terminals` | `/api/v3/terminals` | t24 |
| Настройки конференции | `/pnm/administration/conference_settings` | `/settings/calls`, stop_rules | t23 |
| Видео | `/pnm/administration/video` | `/settings/video`, `/settings/video_codecs` | t23 |
| Калибровка | `/pnm/administration/calibration` | 308 → `/administration/statistics`; `/engine/calibration` = `null` | t24 |
| Дата/время | `/pnm/administration/date_time` | `/system/time`, `/system/ntp`, time-zones | t23 |
| Роли/безопасность | `/pnm/administration/roles`, `/security` | `/users`, `/groups`, `/settings/security` | t21, t23 |
| Журнал событий | `/pnm/administration/event_list` | `/log_events`, export, static ZIP | t24 |
| Импорт/экспорт, сброс, питание | `/backup`, `/reset`, `/power` | snapshot/clean/reboot-* — read-only 501 | t24 |
| Стили раскладок | `/pnm/administration/layout_skins` | `/settings/styles`, `/video_layouts/templates` | t23 |
| О программе | `/pnm/administration/about` | `/system/about` | t24 |

Полная JSON-таблица по 18 проверенным endpoint — в `summary.json →
totals.api_parity`.

## Честные ограничения

- Обход read-only: создание конференции/звонки проверяются отдельно в
  `t16` (живые SIP/H.323), здесь они не повторяются.
- Страницы «Питание», «Импорт/экспорт», «Сброс», «Терминалы», «Медиафайлы»
  рендерятся, но их мутации осознанно 501 (нет привилегий/бэкенда).
- 24 vendor console error — баг Chart.js 2.x в бандле статистики, не наши
  запросы; allowlisted по точному шаблону.
- Провенанс: `clean_build` = чистое дерево + HEAD + найденный бинарник;
  `binary_built_after_head` = mtime бинарника ≥ времени коммита.
