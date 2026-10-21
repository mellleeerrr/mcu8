# T3.3 — финальный обход страниц PNM и таблица паритета

Read-only сквозной прогон vendor-UI по всем основным и административным
страницам + проверка 18 контрактов `/api/v3`, которыми живёт UI. Ничего не
создаёт, не меняет и не удаляет. Прогон на чистом дереве: HEAD `657f7ae`,
бинарник собран после HEAD (`clean_build`, `binary_built_after_head` — в
`summary.json`).

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t33-evidence.js
```

## Результат

**ok=true, 9/9** acceptance-гейтов: 23 страницы без 4xx/console error,
18/18 API-контрактов, 6 PNG, 0 HTTP ≥ 400, 0 наших console error,
24 известных vendor Chart.js warning (allowlisted). «Калибровка» удалена:
deep-link `/pnm/administration/calibration` уводит на статистику, API отдаёт
`{calibration:null}`.

## Таблица паритета (UI ↔ наш API)

| Страница PNM | Ключевые запросы UI | Наш ответ | Evidence |
|---|---|---|---|
| `/pnm/` панель | `/ui/system/status`, `/ui/sessions`, `/conferences` | 200, READY, конференции | `01-root.png`, t16 |
| Конференции | `/ui/sessions`, `/conferences`, `/abonents/count` | 200, список/создание | `02-conferences.png`, t16 |
| Справочник | `/call_strings`, `/users`, `/groups`, `/references` | 200, CRUD | `03-directory.png`, t21 |
| История | `/call_history`, `/session_history`, export | 200, фильтры/сортировка/ZIP | `04-history.png`, t22 |
| Статистика | `/ui/system/status`, `/system/stats/{memory,cpu,disk,network}`, `/engine/diagnostics` | 200, MB/kbps/delta | `05-statistics.png`, t24 |
| Медиафайлы | `/videorecords`, `/conferences/{id}/videorecords` | 200, read-only | t24 |
| Терминалы | `/system/terminals` | 200 `{count,terminals}` | t24 |
| Сеть / интерфейсы | `/system/status/network`, `/system/network/interfaces` | 200, `editable:true`, список ens18 | `06-calls.png`, t24 |
| Вызовы | `/settings/calls`, `/settings/calls/sip`, `/settings/calls/h323` | 200, PATCH сохраняет | `06-calls.png`, t23 |
| Настройки конференции | `/settings/calls`, stop_rules | 200, `one_abonent` | t23 |
| Видео | `/settings/video`, `/settings/video_codecs` | 200, PATCH | t23 |
| Калибровка | `/engine/calibration` | 200 `{calibration:null}`, deep-link → статистика | t24 |
| Импорт/экспорт, сброс, питание | snapshot/clean/reboot-* | read-only 501 с описанием | t24 |
| Дата/время | `/system/time`, `/system/ntp`, time-zones | 200, плоский NTP PATCH | t23 |
| Роли/безопасность | `/users`, `/groups`, `/settings/security` | 200, RBAC | t21, t23 |
| Журнал событий | `/log_events`, export, static ZIP | 200, CSV локальная TZ | t24 |
| Стили раскладок | `/settings/styles`, `/video_layouts/templates` | 200, PATCH | t23 |
| Лицензия | `/license`, `/features` | 200, PERMANENT/50 фич | t24 |
| О программе | `/system/about` | 200, versions/READY | t24 |

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
