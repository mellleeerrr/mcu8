# T2.4 — журнал событий, сеть, статистика, терминалы, медиафайлы

Сквозная вязка vendor-UI `/pnm/administration/{event_list,network,statistics,
terminals,mediafiles,diagnostics,backup,reset,license,about}` с нашим
`/api/v3` на реальном бинаре `vks-server`. Прогон на чистом дереве: HEAD
`08486e7`, бинарник собран после HEAD (явные гейты `clean_build`,
`binary_built_after_head` в `summary.json`).

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t24-evidence.js
```

Скрипт логинится через vendor-UI, проходит **38 acceptance-гейтов** (API +
обход 10 страниц; вкладка «Сетевые интерфейсы» открывается отдельным кликом),
делает 6 PNG и всегда
пишет `summary.json`; при провале — `ok:false`, exit 1. Мутации
сети/tcpdump/терминалов/видеозаписей проверяются как явные 501 (read-only).

## Результат

**ok=true, 38/38** acceptance-гейтов, 10 шагов, 6 PNG, 0 HTTP ≥ 400 на страницах UI
(API-негативы 422/501/503 проверяются намеренно), 0 **непреднамеренных**
console error (один 501 в гейте видимой ошибки сохранения сети исключён из
счётчика и записан как `intentional_console_501=1`). `vendor_errors=12` — известный баг Chart.js 2.x в
vendor-бандле статистики (`Invalid scale configuration for scale:
yAxes/xAxes/responsive/maintainAspectRatio`); он явно allowlisted и не скрывает
наши ошибки.

Ключевые проверки последнего прогона:

| Гейт | Что доказывает |
|---|---|
| `log_events_export_local_time_invoker` | CSV внутри ZIP содержит **локальное** (MSK) время события и непустого пользователя из `invoker`; `unzip -t` подтверждает CRC/central directory |
| `log_events_pagination` | `page=2&page_size=1` даёт ровно 1 запись и тот же `count` |
| `log_events_static_404` | неизвестный файл → 404, traversal (`..%2F`) → 400/404 |
| `stats_disk` | значения — **MB**: `total` совпадает с `df -Pk /` / 1024 в пределах 2% (округление ФС; UI делит на 1024 и пишет Gb) |
| `stats_network` | **kbps**, а не накопительные байты: значения — дельта двух замеров `/proc/net/dev` за 200 мс, правдоподобный диапазон; UI рисует «кбит/сек» |
| `ui_network_interfaces_tab` | клик по вкладке «Сетевые интерфейсы» реально отрисовывает интерфейсы: вкладка найдена, запросы `/system/status/network` + `/system/network/interfaces` ушли, в DOM есть `.network-manager-card`/`ens18`, 0 новых 4xx |
| `ui_license_not_expired` | страница лицензии не показывает «01.01.1970» и «Продлить лицензию» |
| `license_permanent_shape` | `PERMANENT`, будущий `last_expired`, `last_update_failed:false`, `serial_id`, `params.{audio,video,protocol,server,storage}` — все пять ключей проверяются гейтом |
| `features_array` | `/features` — объект `{features:[...]}` с массивом > 40 элементов (vendor-UI читает `data.features`) |
| `status_network_shape` | `{network:{editable:true}}` — без этого vendor-бандл рисует «Список пуст»; сохранение сети отвечает 501, а `api-errors.js` показывает эту ошибку оператору |
| `session_videorecords_alias` | `/sessions/{id}/videorecords[/estimated_time]` → 200; список **фильтруется по id из пути** (есть тест: своя запись = 1, чужая = 0) |
| `utils_traceroute_honest` | traceroute не установлен → честный **503** «not installed», а не 200 с OS-ошибкой |
| `utils_ping_rejects_option` | `address=-V` → 422: опции не уходят в процесс |
| `network_patch_error_visible` | страничный XHR по каноническому `PATCH /system/network/interfaces/ens18` → 501, а `api-errors.js` показывает красный тост с `error.description` (проверяется текст тоста); реальный клик Save в UI проверен вручную и шлёт тот же путь |
| `engine_log` | стартовая запись совпадает с реальным стартом процесса (`/proc/<pid>/stat starttime + /proc/stat btime`, HZ=100); стабильность между запросами покрыта отдельным unit-тестом |

Остальные гейты — контракты list/detail/patch-501, ping/tcpdump/терминалы/
видеозаписи, memory/cpu/packages, 10 страниц без 4xx и vendor Chart.js
warnings.

## Реализация

- `crates/vks-admin/src/api_v3/observability.rs`:
  - `log_events`: audit → 8 vendor-типов (`login_success/failed/rejected`,
    `updating_password_success`, `creating_credentials_success`,
    `updating_login_success`, `deleting_credentials_success`,
    `updating_configuration_security_success`); остальные действия не
    синтезируются. Фильтры `event_types/search/date_from/date_to`,
    пагинация, сортировка.
  - Экспорт: CSV (`;`, экранирование, защита от формул) в ZIP; даты — в
    сохранённой IANA-зоне (та же, что в UI), пользователь — `invoker.name` с
    fallback на `payload.login`, filename — локальное время + **128-битный**
    токен. Файл живёт в общем `ExportStore` (TTL 1 ч, cap 32), `/static/log_events`
    отдаётся без авторизации по непредсказуемому имени.
  - Сеть: интерфейсы из `ip -j addr`/`ip -j route`, статус
    `/system/status/network` → `{editable:true}`, `PATCH` → 501 + видимый тост.
  - `ping`/`traceroute`: реальные процессы, адрес валидируется (буквы/цифры/
    `.:-_`, не начинается с `-`), `kill_on_drop`, таймаут 10 с; отсутствующий
    бинарь → 503.
  - tcpdump/терминалы/видеозаписи — read-only, мутации 501; session-алиасы
    видеозаписей.
  - Статистика: memory/disk — MB, network — kbps (дельта 200 мс), cpu — дельта
    `/proc/stat`, disk — `df -Pk`/1024, packages — пустой список.
  - license/features/diagnostics/calibration — Operator+ (`forbid`), license
    постоянная с реальными `params`, features — массив.
- `shell.rs`: `/license`, `/features`, `/system/status/network`;
  `boot.rs`: `/system/about` с `versions.{server,installer,ui,api,engine,sip/h323,vile}`.

## Честные ограничения

- Мутации T2.4 (сеть, tcpdump, терминалы, удаление записей, reboot/shutdown,
  backup) не выполняются: нет привилегий, UI получает 501 с описанием.
- `/system/status/network` отдаёт `editable:true` только чтобы vendor-UI
  отрисовал список интерфейсов; сохранение сети не поддерживается (нет
  CAP_NET_ADMIN) — `PATCH /api/v3/system/network/interfaces/{interface}` (канонический; короткий `/system/network/{interface}` — алиас для старых сборок) честно 501, а глобальный
  `api-errors.js` показывает красный тост с `error.description` (иначе
  vendor-UI закрывал модалку молча).
- `traceroute` на этом хосте не установлен → 503; `ping` работает.
- `engine/log` — одна запись с реальным временем старта процесса, не полный
  журнал демона.
- `system/packages` — пустой список (инвентаризация ОС не ведётся).
- CSV журнала содержит только 8 vendor-типов событий; CRUD пользователей/
  credentials v3 (`user.created`, `user.credentials.*`,
  `profile.credentials.updated`) замаплены на шаблоны, остальные внутренние
  действия без vendor-шаблонов в журнал не попадают (остаются в audit API).
- Chart.js в vendor-бандле ругается на конфиг осей статистики — баг вендора,
  allowlisted (`vendor_errors`), а не ответ API.
- `log_events` хранится в памяти (AuditLog) и неограничен по размеру; экспорт
  собирает все события под Operator-ролью (осознанный trade-off, TTL/cap у
  выгрузок есть).

## Тесты

`cargo test -p vks-admin --lib api_v3::observability` — **13** тестов: журнал
(пагинация/фильтры/поиск/даты/экспорт/static 404/RBAC/v3-маппинг), сеть
(список/detail/patch 501/404), ping/tcpdump/терминалы, видеозаписи, 403 для
нижних ролей, статистика (форма), **единицы disk MB / network kbps**, CSV
локальная TZ + invoker, license/features/status/alias + Viewer 403,
traceroute 503/-V 422, стабильный engine log. Полный прогон: 230 vks-admin lib
+ 35 vks-server lib, clippy/fmt без замечаний.
