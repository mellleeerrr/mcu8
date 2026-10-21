# T2.4 — журнал событий, сеть, статистика, терминалы, медиафайлы

Сквозная вязка vendor-UI `/pnm/administration/{event_list,network,statistics,
terminals,mediafiles,diagnostics,backup,reset,license,about}` с нашим
`/api/v3` на реальном бинаре `vks-server`. Прогон на чистом дереве: HEAD
`7f2fec7`, бинарник собран после HEAD.

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t24-evidence.js
```

Скрипт логинится через vendor-UI, проходит 25 acceptance-гейтов (API + обход
10 страниц), делает 4 PNG и всегда пишет `summary.json`; при провале —
`ok:false`, exit 1. Все гейты read-only, кроме `PATCH interface`/`POST/DELETE
tcpdumps`/`POST terminals`/`DELETE videorecords`, которые обязаны вернуть 501
(мутации не поддерживаются — см. ограничения).

## Результат

**ok=true, 25/25** acceptance-гейтов, 9 шагов, 4 PNG, 0 HTTP ≥ 400,
0 console error. `vendor_errors=12` — известный баг Chart.js 2.x в
vendor-бандле статистики (`Invalid scale configuration for scale:
yAxes/xAxes/responsive/maintainAspectRatio`), он явноallowlisted и не
скрывает наши ошибки. `clean_build=true`, `binary_built_after_head=true`.

| Гейт | Что проверяет |
|---|---|
| `log_events_contract` | `GET /api/v3/log_events` → `{count, log_events:[{id,event_type,object_type,created_at,payload,invoker,event_object,additional_info}]}` |
| `log_events_pagination` | `page/page_size` не ломает `count` |
| `log_events_filter_search` | `event_types=login_success&search=admin` возвращает только login_success |
| `log_events_date_filter` | `date_from` (секунды) сужает журнал |
| `log_events_export_zip` | `GET /log_events/export` → `{filename}`, файл на `/static/log_events/{filename}` — валидный ZIP (PK..) с CSV (`ID;Дата;Событие;...`) |
| `log_events_static_404` | неизвестный файл → 404, попытка traversal (`..%2F`) не отдаёт данные |
| `network_interfaces_list` / `_detail` | `{count,interfaces:[{interface,mac,use_dhcp,gateway,nameservers,ready,addresses}]}`, detail `{interface:{...}}` |
| `network_interface_patch_501` | изменение сети не применяется (нет CAP_NET_ADMIN) → 501 с описанием |
| `utils_ping` | реальный `ping -c 3 -W 2 -n` для `127.0.0.1`; невалидный хост (`bad host; rm -rf /`) → 422 (валидация адреса, без shell) |
| `tcpdumps_status` / `_list` | `{running:false}` и `{files:[]}` |
| `tcpdumps_mutations_501` | старт/стоп захвата → 501 |
| `terminals_list` | `/api/v3/terminals` и alias `/api/v3/ui/terminals` → `{count:0,terminals:[]}` |
| `terminals_mutations_501` | POST терминала → 501, credentials отсутствующего терминала → 404 |
| `videorecords_list` / `_estimated` / `_delete_501` | `{count:0,videorecords:[]}`, `estimated_time` → `{recording_time:3600}`, удаление → 501 |
| `stats_memory` | `/system/stats/memory` → `{total,used,free}` в MB из `/proc/meminfo` |
| `stats_cpu` | `{count,load:[{user_mode,kernel_mode,idle}]}` — дельта `/proc/stat` за 120 мс по ядрам |
| `stats_disk` | `{count,spaces:[{name,total,used,free}]}` из `df -Pk` (KB) |
| `stats_network` | `{count,interfaces:[{name,rx,tx}]}` из `/proc/net/dev` |
| `system_packages` | `{packages:[]}` |
| `engine_log` | `{language,bounds:{id:{min,max},timestamp:{min,max}},log:[...]}` — запись о старте процесса |
| `ui_pages` | все 10 страниц рендерят свои секции, без 4xx и неизвестных console error |

## Реализация

- `crates/vks-admin/src/api_v3/observability.rs`:
  - `log_events`: audit-записи → 53 vendor-шаблона событий; события без
    vendor-шаблона (`create_room` и т.п.) не выдумываются, а отфильтровываются,
    поэтому журнал содержит только валидные `event_type`; фильтры
    `event_types/search/date_from/date_to`, пагинация, сортировка;
  - `log_events_export`: CSV (`ID;Дата;Событие;Объект;Пользователь;IP;Дополнительно`,
    экранирование `;`/кавычек, защита от CSV-формул) в ZIP; архив кладётся в
    общий `ExportStore` (TTL 1 ч, cap 32) и отдаётся `/static/log_events/{name}`
    без авторизации по непредсказуемому имени; traversal отвергается;
  - `zip_store.rs` — общий ZIP-писатель (store, CRC32, корректные DOS-даты и
    central directory), используется историей и журналом;
  - сеть читается из `ip -j addr`/`ip -j route`, `PATCH` — явный 501;
  - ping/traceroute — реальные процессы с валидацией адреса (никакого shell);
  - tcpdump/терминалы/видеозаписи — read-only с явными 501 на мутации;
  - статистика — `/proc/meminfo`, `/proc/stat`, `df -Pk`, `/proc/net/dev`;
    `engine/log` — bounded-запись о старте; `system/packages` — пустой список.
- Маршруты и `ExportStore` добавлены в `api_v3/mod.rs`; catch-all 501 остаётся
  для неизвестных `/api/v3/*`, поэтому новые страницы не получают 404.

## Честные ограничения

- Мутации T2.4 (настройка сети, tcpdump, терминалы, удаление видеозаписей,
  reboot/shutdown, backup) осознанно не выполняются: сервер работает без
  привилегий и не должен терять удалённый доступ. UI получает 501 с
  человекочитаемым описанием, а не молчаливый «успех».
- `/system/stats/disk` отдаёт `spaces` из `df`; vendor-эталон `.92` на этом
  запросе отвечал 500, поэтому форма сверена с кодом UI (`{count,spaces}`).
- `engine/log` — не полноценный журнал демона: одна запись о старте с
  bounded id/timestamp; реальные логи доступны в stdout/системном журнале.
- `system/packages` — пустой список: инвентаризация пакетов ОС не ведётся.
- Журнал событий содержит только audit-события, для которых есть vendor-шаблон
  (53 типа); CRUD комнат/пользователей не имеет vendor-шаблонов и не
  синтезируется.
- Chart.js в vendor-бандле ругается на конфиг осей статистики — это баг
  вендора, allowlisted в evidence (`vendor_errors`), а не ответ API.

## Тесты

`cargo test -p vks-admin --lib api_v3::observability` — 7 тестов: журнал
(пагинация/фильтры/поиск/даты/экспорт/static 404/RBAC), сеть
(список/detail/patch 501/404), ping/tcpdump/терминалы, видеозаписи, 403 для
нижних ролей, формы системной статистики. Полный прогон: 220 vks-admin lib +
35 vks-server lib, clippy/fmt без замечаний.
