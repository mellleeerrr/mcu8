# T2.3 — административные настройки PNM (vendor 2.2.1.66)

Проверка сквозной вязки vendor-UI `/pnm/administration/*` с нашим `/api/v3`:
контракты настроек, сохранение **через реальные формы UI**, multipart-TLS,
SMTP, NTP/время/зона. Всё проверено на реальном бинаре `vks-server` и UI,
без моков. Прогон на чистом дереве: HEAD `7f2fec7`, бинарник собран после
HEAD.

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t23-evidence.js
```

Скрипт логинится через vendor-UI, проходит 32 acceptance-гейта (API + UI),
делает 4 PNG и всегда пишет `summary.json`; при провале — `ok:false`, exit 1.
В конце восстанавливает изменённые значения (SIP/H.323/RTP, TLS, SMTP, NTP,
время, зона, calls), чтобы прогон был повторяемым.

## Результат

**ok=true, 32/32** acceptance-гейтов, 12 шагов, 4 PNG, 0 HTTP ≥ 400,
0 console error (vendor-allowlist `validationHandler` не понадобился).
`clean_build=true`, `binary_built_after_head=true`.

Ключевые vendor-пути, которые ранее падали, теперь проверены именно так, как
их шлёт UI:

| Гейт | Что доказывает |
|---|---|
| `ntp_ui_save` | в форме «Дата и время» включаем NTP, выбираем «Частота синхронизации = 1 час», вводим сервер, жмём «Сохранить» → UI отправляет `PATCH /system/ntp` **плоским** `{ntp:"pool.<stamp>.ntp.org",fallback:[],poll_interval_min:3600,poll_interval_max:21600}` → 200, после reload сервер на месте |
| `ntp_ui_restarted_services` | после сохранения UI вызывает `POST /system/ntp/enable` → 200 |
| `conference_settings_ui_one_abonent` | «Настройки конференции»: включаем «Один участник», Save → `one_abonent` число; после reload чекбокс включён; выключаем, Save → `one_abonent:null` → 200, reload показывает выключено |
| `sip_save_restart_services` | сохранение SIP (`PATCH /settings/calls/sip`) + `POST /system/restart_services` → 200 `{status:"ok"}` (раньше catch-all 501) |
| `system_time_patch_vendor_string` | `PATCH /system/time` c vendor-строкой `"2026-10-22 04:45:00"` в UTC-зоне даёт ровно ожидаемый epoch |
| `ntp_status_not_faked` | `system/time.ntp_synchronized` равно реальному `timedatectl NTPSynchronized`, а не флагу «NTP включён» |
| `http_check_single_file` | `POST /settings/http/check` по одному файлу: `crt` → 200, `key` → 200, мусор без PEM → 400 (vendor-UI проверяет слоты по отдельности) |
| `settings_ui_pages` | calls/video/date_time/layout_skins/conference_settings/security/network рендерят свои секции без redirect и 4xx |
| `settings_ui_save` / `settings_ui_save_restored` | кнопка «Сохранить» на «Вызовах» шлёт `PATCH /settings/calls`, изменения видны в API и восстановлены |
| `h323_restored` | после гейта H.323 `h323_id/e164` возвращаются к исходным (прогон не оставляет мусор) |

Остальные гейты — контракты и persist: `settings_get_contracts`, `http_contract`,
`sip/rtp/h323_patch_persist`, `settings_strict_schema` (неизвестное поле → 422),
`calls_vendor_payload`/`calls_patch_persist`, `http_multipart_upload`/`http_reset`,
`smtp_empty_server_422`/`smtp_patch_masked`/`smtp_live_test`/`smtp_reset`,
`ntp_toggle`, `system_time_patch`, `timezone_patch_profile`, `time_zones_list`
(428 IANA-зон), `ui_time_shows_ntp`, `ui_security_form`.

## Реализация (что закрыто)

- `crates/vks-admin/src/api_v3/settings_admin.rs`:
  - vendor-дефолты `calls,sip,h323,video,rtp,http,styles,smtp,security,ntp,timezone,time_offset`
    (фикстуры `docs/progress/evidence/ref-92/t23/*.json` сняты с `https://192.168.1.92`, GET-only);
  - строгий рекурсивный `merge_known` с **явной nullable-схемой**:
    `stop_rules.{empty,quit_all,one_abonent}` — number|null, `ntp.server` —
    string|null, `ntp.poll_interval_*` — number|null. Неверный не-null тип →
    422, `null` законно выключает правило; после одного 422 поле остаётся
    восстановимым (нет «дефолт = схема», который принимал `server:123`);
  - `PATCH /system/ntp` принимает и плоский vendor-shape
    `{ntp:String,fallback,poll_interval_min,max}`, и `{payload:{server,...}}`;
  - `PATCH /system/time` принимает unix-секунды, RFC3339 и vendor-строку
    `YYYY-MM-DD HH:mm:ss` (парсится в сохранённой IANA-зоне);
  - `calls_patch` атомарен: `SettingsService` (codecs/limits/turn) меняется
    только после успешной валидации и persist admin_settings; на 422 media
    настройки не тронуты;
  - `POST /system/restart_services` (и `restart_engine|web_api|webproxy`) —
    hot-reload конфигурации из БД, 200 `{status:"ok",applied,...}`, без
    перезапуска процесса (иначе оборвались бы звонки);
  - `ntp_synchronized` читается из `timedatectl show -p NTPSynchronized`
    (fallback `/run/systemd/timesync/synchronized`), не подделывается;
  - `/settings/http/check` проверяет один выбранный слот (PEM-маркеры или
    DER-заголовок, лимит 100 KiB), а не требует пару в одном запросе;
  - `PATCH /settings/http` разрешает загрузку одного слота (второй остаётся),
    пустые поля игнорируются;
  - durable-хранение в `resources.admin_settings`, переживает рестарт тестом
    `settings_survive_restart`; пароли SMTP маскируются в GET.
- RBAC: все **мутации** — глобальный Operator (`forbid` отклоняет и
  room-scoped Operator); чтение настроек — тоже Operator+ (см. ограничения
  ниже: это осознанно, тест `settings_tests` фиксирует 403 для нижних ролей).
  `update_settings_video` в `shell.rs` использует тот же `forbid` (раньше
  room-scoped Operator мог менять глобальные настройки видео).
- `video` и «Стили раскладок» остаются в `state.api_v3.video_settings`, не
  дублируются в `admin_settings`.

## Честные ограничения

- `PATCH /system/time` меняет **логическое** время API (`time_offset`), а не
  OS-часы хоста: у процесса нет `CAP_SYS_TIME`. UI показывает скорректированное
  время; реальные часы — NTP/systemd.
- NTP-клиент не запускается нашим процессом: `ntp.enabled` — сохранённый флаг
  конфигурации, `ntp_synchronized` — реальный статус ОС. Если timesyncd не
  настроен, UI честно покажет «не синхронизирован».
- `POST /settings/http/check` проверяет PEM-маркеры/размер (≤100 KiB), но не
  парсит X.509 и не сверяет пару cert/key; полноценный TLS-терминатор —
  отдельная задача. Пустой файл игнорируется, ответ 200 сохраняется.
- `PUT /smtp/test` делает реальный `TcpStream::connect` с таймаутом 5 с, но не
  выполняет SMTP-диалог `EHLO/AUTH`.
- SMTP/SIP/TURN-пароли хранятся в конфиг-документе как есть (шифрование at-rest
  — T3) и маскируются в GET; «очистить» SIP/TURN-пароль через пустой PATCH
  нельзя (пустое значение = «не менять»).
- Чтение `/settings/{calls,sip,http,...}` — Operator+ (не «любой
  аутентифицированный»); vendor-UI доступен только оператору, поэтому это не
  ломает админку, но является отличием от раннего README.
- `security`-настройки хранятся/отдаются, но не применяются к аутентификации
  (нет потребителя вне settings API); SIP/H.323/RTP/SMTP — конфигурация,
  реально влияют на media только codecs/limits/turn.

## Тесты

`cargo test -p vks-admin --lib` — **220** тестов, из них 11 в
`settings_tests.rs`, включая vendor-формы:
`vendor_ui_shapes_for_ntp_time_and_stop_rules_are_accepted` (плоский NTP,
строка времени, `null` для всех `stop_rules`, отказ `one_abonent:"soon"` с
последующей очисткой), `rejected_calls_patch_does_not_touch_media_codecs_and_restart_services_is_200`
(атомарность 422 + `restart_services` 200), `room_scoped_operator_cannot_change_global_video_settings`.
`cargo clippy -p vks-admin -p vks-server --all-targets` и `cargo fmt --check` —
без замечаний.
