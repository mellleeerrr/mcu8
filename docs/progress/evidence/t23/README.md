# T2.3 — административные настройки PNM (vendor 2.2.1.66)

Проверка сквозной вязки vendor-UI `/pnm/administration/*` с нашим `/api/v3`:
контракты настроек, сохранение через UI, multipart-TLS, SMTP, NTP/время/зона.
Всё проверено на реальном бинаре `vks-server` и UI, без моков.

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t23-evidence.js
```

Скрипт логинится через vendor-UI, проходит API-гейты, затем страницы
`/pnm/administration/{calls,video,date_time,layout_skins,conference_settings,security}`,
сохраняет настройки вызовов кнопкой «Сохранить» в UI и делает PNG.
Итог — `summary.json` (`ok`, acceptance-гейты, провенанс бинаря).

## Результат (clean tree, HEAD `cfffcb7`)

- **ok=true**, **25/25** acceptance-гейтов, 9 шагов, 3 PNG;
- 0 HTTP ≥ 400 и 0 console error за прогон;
- `clean_build=true` (дерево чистое на старте), `binary_built_after_head=true`;
- реальные запросы: SIP 5070 → persist → restore; RTP MTU 1400; H.323 id/e164;
  calls legacy `limits.max_participants_per_room=50`; multipart crt/key/port 8443;
  `http/check` 200; `http/reset` → port 443; SMTP реальный TCP 127.0.0.1:2525;
  NTP enable/disable; `PATCH /system/time`; timezone UTC → profile → restore MSK;
  `/time-zones` 428 записей.

## Что именно проверяет скрипт

| Гейт | Смысл |
|---|---|
| `settings_get_contracts` | `calls/sip/h323/rtp/styles/security/smtp` отдают vendor-поля в `payload` |
| `http_contract` | `settings/http` — плоский `{port,crt,key,has_password,auto_reset_cert}` как у vendor |
| `sip_patch_persist` / `rtp_patch_persist` / `h323_patch_persist` | PATCH меняет только переданные поля, GET подтверждает, непереданные не теряются |
| `settings_strict_schema` | неизвестное поле → 422 (не «молчаливое» сохранение) |
| `calls_vendor_payload` / `calls_patch_persist` | vendor-поля (`fecc/aspeaker/limits`) и legacy-лимиты живут в одном payload |
| `http_multipart_upload` / `http_check_after_upload` / `http_reset` | multipart `port+crt+key`, check пары, reset к vendor-дефолтам (443) |
| `smtp_empty_server_422` / `smtp_patch_masked` / `smtp_live_test` / `smtp_reset` | пустой сервер → 422; пароль не возвращается; тест реально подключается по TCP; reset |
| `ntp_toggle` | enable/disable отражается в `system/time.time.ntp` |
| `system_time_patch` | `PATCH /system/time` возвращает запрошенное время (логический offset) |
| `timezone_patch_profile` | `PATCH /system/timezone` меняет `profile.user.time_zone_id`, неизвестная зона → 422 |
| `time_zones_list` | `/time-zones` — 428 IANA-зон из vendor-эталона |
| `settings_ui_pages` | шесть страниц админки реально рендерят свои секции (не redirect) |
| `settings_ui_save` / `settings_ui_save_restored` | кнопка «Сохранить» шлёт `PATCH /settings/calls`, изменения видны в API, затем восстановлены |
| `ui_time_shows_ntp` / `ui_security_form` | формы «Дата и время» и «Безопасность» отрисованы с нашими данными |

## Реализация (что закрыто)

- `crates/vks-admin/src/api_v3/settings_admin.rs`:
  - vendor-дефолты `calls,sip,h323,video,rtp,http,styles,smtp,security,ntp,timezone,time_offset`
    (фикстуры `docs/progress/evidence/ref-92/t23/*.json` сняты с `https://192.168.1.92`, GET-only);
  - строгий рекурсивный `merge_known` — неизвестное поле/тип → 422, вложенные объекты
    (`limits`, `font`, `border`) сливаются, а не перезаписываются целиком;
  - durable-хранение в `resources.admin_settings` (тот же конфиг-документ, что и
    остальные ресурсы), переживает рестарт (тест `settings_survive_restart`);
  - пароли SMTP/HTTP маскируются в GET (`smtp_password` не возвращается);
  - `GET/PATCH /settings/{calls,sip,h323,rtp,styles,security,smtp}`, `GET/PATCH /settings/http`
    (multipart), `POST /settings/http/check|reset`, `PUT /smtp/test`, `POST /smtp/reset`;
  - `GET/PATCH /system/ntp`, `POST /system/ntp/enable|disable`, `GET/PATCH /system/time`,
    `PATCH /system/timezone`; `profile.user.time_zone_id` читает сохранённую TZ;
  - `PATCH /settings/audio_codecs|video_codecs` из vendor-UI обновляет media-настройки
    через `SettingsService` (страница «Вызовы» больше не получает 405 при сохранении).
- RBAC: все мутации — Operator+ (`forbid`), чтение — любой аутентифицированный
  (совпадает с vendor-UI, где админка доступна оператору).
- `video` и «Стили раскладок» остаются в существующем media-хранилище T1.4
  (`state.api_v3.video_settings`), а не дублируются в `admin_settings`.

## Честные ограничения

- `PATCH /system/time` меняет **логическое** время API (`time_offset`), а не OS-часы
  хоста: у процесса нет `CAP_SYS_TIME`, и подмена системного времени сломала бы
  TLS/токены. Это отражено в коде и тесте; UI показывает скорректированное время.
- `POST /settings/http/check` проверяет наличие/размер пары crt/key (≤1 MiB) и
  возвращает `valid:true`; полноценный парсинг X.509 не делается — vendor-UI в
  наших сценариях присылает PEM, а реальный TLS-терминатор настраивается отдельно.
- `PUT /smtp/test` делает реальный `TcpStream::connect` с таймаутом 5 с, но не
  выполняет SMTP-диалог `EHLO/AUTH` — проверяется доступность сервера.
- SMTP-пароль хранится в конфиг-документе как есть (как и остальные секреты
  ресурсов); в GET он маскируется. Шифрование at-rest — отдельная задача T3.
- `GET /settings/http` отдаёт метаданные сертификата из фикстуры (CN, сроки,
  алгоритмы); загруженный файл сохраняется как имя/флаг `user_crt`, содержимое
  PEM не парсится.

## Тесты

`cargo test -p vks-admin --lib` — **209** тестов (8 новых в `settings_tests.rs`):
дефолты/patch/persist, строгая схема, RBAC, vendor+legacy calls, маскирование и
сохранение SMTP-пароля, пустой и живой TCP SMTP, multipart TLS upload/check/reset,
NTP/time/timezone/profile, персистентность после рестарта.
`cargo clippy -p vks-admin --all-targets` и `cargo fmt --check` — без замечаний.
