# T2.3 — административные настройки PNM (vendor 2.2.1.66)

Проверка сквозной вязки vendor-UI `/pnm/administration/*` с нашим `/api/v3`:
контракты настроек, сохранение **через реальные формы UI**, multipart-TLS,
SMTP, NTP/время/зона. Всё проверено на реальном бинаре `vks-server` и UI,
без моков. Прогон на чистом дереве: HEAD `aa9a1ec`, бинарник собран после
HEAD.

## Как воспроизвести

```bash
PLAYWRIGHT_MODULE=/path/to/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t23-evidence.js
```

Скрипт логинится через vendor-UI, проходит 35 acceptance-гейтов (API + UI),
делает 4 PNG и всегда пишет `summary.json`; при провале — `ok:false`, exit 1.
В конце восстанавливает изменённые значения (SIP/H.323/RTP, TLS, SMTP, NTP,
время, зона, calls), чтобы прогон был повторяемым.

## Результат

**ok=true, 35/35** acceptance-гейтов, 13 шагов, 4 PNG, 0 HTTP ≥ 400 на
`/api/v3/*`, 0 console error. `clean_build=true`, `binary_built_after_head=true` —
явные гейты в `summary.json` вместе с git/worktree-провенансом. `clean_build`
означает чистое дерево + 40-символьный HEAD + найденный бинарник, а
`binary_built_after_head` — `mtime` бинарника ≥ времени коммита (для
`aa9a1ec`); это не доказывает побайтовое соответствие сборки HEAD.

Ключевые vendor-пути, которые ранее падали, теперь проверены именно так, как
их шлёт UI:

| Гейт | Что доказывает |
|---|---|
| `ntp_ui_save` | в форме «Дата и время» включаем NTP, выбираем «Частота синхронизации = 1 час», вводим сервер, жмём «Сохранить» → UI отправляет `PATCH /system/ntp` **плоским** `{ntp:"pool.<stamp>.ntp.org",fallback:[],poll_interval_min:3600,poll_interval_max:21600}` → 200; после **reload страницы** сервер на месте (сохранение `fallback`/интервалов проверяет unit-тест `partial_flat_ntp_patch_preserves_fallback_and_intervals`) |
| `ntp_ui_enable` | после сохранения UI вызывает `POST /system/ntp/enable` → 200 (раньше гейт назывался `ntp_ui_restarted_services` и проверял не рестарт) |
| `conference_settings_ui_one_abonent` | «Настройки конференции»: включаем «Один участник», Save → `one_abonent` число; после reload чекбокс включён; выключаем, Save → `one_abonent:null` → 200, reload показывает выключено |
| `sip_save_restart_services` | сохранение SIP (`PATCH /settings/calls/sip`) + `POST /system/restart_services` → 200 `{status:"ok"}` (раньше catch-all 501) |
| `restart_services_preserves_active_session` | активная сессия (state=ACTIVE) после `POST /system/restart_services` остаётся ACTIVE (раньше hot-reload помечал все сохранённые комнаты остановленными) |
| `system_time_patch_vendor_string` | `PATCH /system/time` c vendor-строкой `"2026-10-22 04:45:00"` в UTC-зоне даёт ожидаемый epoch с допуском ±120 с (граница между «время применено» и «часы тикают») |
| `ntp_status_not_faked` | `system/time.ntp_synchronized` равно реальному `timedatectl NTPSynchronized`, а не флагу «NTP включён» |
| `http_check_single_file` | `POST /settings/http/check` по одному файлу: `crt` → 200, `key` → 200, мусор без PEM → 400 (vendor-UI проверяет слоты по отдельности) |
| `settings_ui_pages` | calls/video/date_time/layout_skins/conference_settings/security/network рендерят свои секции без redirect и 4xx |
| `settings_ui_save` / `settings_ui_save_restored` | кнопка «Сохранить» на «Вызовах» шлёт несколько PATCH (calls/sip/h323/кодеки), гейт требует ≥3 запросов, все 200, изменения видны в API и восстановлены |
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
    настройки не тронуты. Частичные legacy-объекты (`limits`,
    `turn`, `codecs`) накладываются на текущие значения; неизвестное поле или
    неверный тип → 422 вместо молчаливого 200 без изменений;
  - `POST /system/restart_services` (и `restart_engine|web_api|webproxy`) —
    hot-reload конфигурации из БД, 200 `{status:"ok",applied:true}`, без
    перезапуска процесса; `hot_reload_from_storage` **не** помечает живые
    сессии остановленными (startup-гидратация — отдельный путь). Ошибка
    reload → 500, а не фейковый `ok`. GET-метод теперь 405 (мутация только POST);
  - `ntp_synchronized` читается из `timedatectl show -p NTPSynchronized`
    (fallback `/run/systemd/timesync/synchronized`), не подделывается;
  - `/settings/http/check` проверяет один слот: настоящий PEM-блок (BEGIN/END
    нужного типа + base64-тело) либо правдоподобный DER-SEQUENCE, лимит 100 KiB.
    Маркеры вокруг мусора и 1-байтный `0x30` отклоняются; X.509 по-прежнему не
    парсится (см. ограничения);
  - `PATCH /settings/http` разрешает загрузку одного слота (второй остаётся),
    пустые поля игнорируются;
  - durable-хранение в `resources.admin_settings`, переживает рестарт тестом
    `settings_survive_restart`; пароли SMTP маскируются в GET.
- RBAC: все **мутации** — глобальный Operator (`forbid` отклоняет и
  room-scoped Operator). Чтение `/settings/calls|sip|http` — Operator+;
  legacy-кодеки/видео/`system/time` читаются и нижними ролями (это поведение
  vendor-UI, README не заявляет обратного). `update_settings_video` в
  `shell.rs` использует тот же `forbid` (раньше room-scoped Operator мог менять
  глобальные настройки видео).
- `video` остаётся в `state.api_v3.video_settings`; «Стили раскладок»
  сохраняются в `admin_settings` (`styles`), как и остальные admin-настройки.

## Честные ограничения

- `PATCH /system/time` меняет **логическое** время API (`time_offset`), а не
  OS-часы хоста: у процесса нет `CAP_SYS_TIME`. UI показывает скорректированное
  время; реальные часы — NTP/systemd.
- NTP-клиент не запускается нашим процессом: `ntp.enabled` — сохранённый флаг
  конфигурации, `ntp_synchronized` — реальный статус ОС. Если timesyncd не
  настроен, UI честно покажет «не синхронизирован».
- `POST /settings/http/check` проверяет структуру PEM/DER и размер (≤100 KiB),
  но не парсит X.509 и не сверяет пару cert/key; полноценный TLS-терминатор —
  отдельная задача. Запрос без непустого `crt`/`key` → 422 (а не молчаливый 200).
- `PUT /smtp/test` делает реальный `TcpStream::connect` с таймаутом 5 с, но не
  выполняет SMTP-диалог `EHLO/AUTH`.
- SMTP/SIP/TURN-пароли хранятся в конфиг-документе как есть (шифрование at-rest
  — T3) и маскируются в GET; «очистить» SIP/TURN-пароль через пустой PATCH
  нельзя (пустое значение = «не менять»).
- Чтение `/settings/calls|sip|http` — Operator+; чтение legacy
  `/settings/video|audio_codecs|video_codecs` и `/system/time` доступно any
  authenticated (так ходит vendor-UI). Мутации всего перечисленного — только
  Operator.
- `security`-настройки хранятся/отдаются, но не применяются к аутентификации
  (нет потребителя вне settings API); SIP/H.323/RTP/SMTP — конфигурация,
  реально влияют на media только codecs/limits/turn.

## Тесты

`cargo test -p vks-admin --lib` — **230** тестов, из них 13 в
`settings_tests.rs`, включая vendor-формы:
`vendor_ui_shapes_for_ntp_time_and_stop_rules_are_accepted` (плоский NTP,
строка времени, `null` для всех `stop_rules`, отказ `one_abonent:"soon"` с
последующей очисткой), `partial_legacy_calls_objects_are_merged_and_invalid_ones_rejected`
(частичный limits применяется, неизвестное поле/тип → 422),
`partial_flat_ntp_patch_preserves_fallback_and_intervals`,
`rejected_calls_patch_does_not_touch_media_codecs_and_restart_services_is_200`
(атомарность 422 + `restart_services` 200 + GET 405),
`hot_reload_preserves_live_sessions_but_startup_marks_stopped` (state.rs),
`room_scoped_operator_cannot_change_global_video_settings`.
`cargo clippy -p vks-admin -p vks-server --all-targets` и `cargo fmt --check` —
без замечаний.
