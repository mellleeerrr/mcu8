# MASTER PLAN 2026-10-21 — рабочий MCU, полная цепочка

Статус: активный план. База: HEAD `108df90`, живой сервис `vks-mcu.service`
(PID 1242813, `/healthz` 200). Цель: довести стенд до полностью рабочего,
оптимизированного MCU/SFU с SIP/H.323, всеми раскладками, кодеками,
справочником, статистикой и доказательством через Playwright.

## 0. Результаты стартового ревью (до плана)

| Ревьюер | Оценка | Главный вывод |
|---|---:|---|
| `ref-engines` (гигиена) | **6/10** | prod может стартовать `admin/admin`; 39 MB tracked, 7.1 MB evidence, мёртвый playwright-конфиг |
| `directory-db` (регрессии) | **6/10** | COW неатомарен; attach не меняет активную раскладку; detach/auto не доезжает до MCU; create плодит default; repair рисует лишнего на весь экран |
| `stats-deps` (готовность) | **5.5/10** | SIP-сценарий ~7/10; H.323 сейчас `remote_release`; max frame/bitrate не применяются; справочник не единый; 2/6 виджетов статистики фиктивны |
| `rtcp-demux` (элегантность) | **6/10** | ~2.5–3k prod-строк можно снять; дубли time/CSV/URI; 21 неиспользуемая зависимость; мёртвый compat-блок |

Все оценки ниже порога 9.5 — это и есть перечень работ. План выровнен по
принципу «сначала корректность и безопасность, затем доказательства, затем
транспорт/кодеки, затем производительность и уборка».

## 1. Приоритеты и порядок

Порядок выполнения (выровнен после ревью плана, 7/10):
**T01 → T02 → T03 → T04a → T05 → T06a → T07 → T11a → T10 → T08 → T09 →
T11b → T04b → T12 → T13 → T14 → T15.**

P0 (блокируют приёмку): T01, T02, T03, T04a, T05, T06a, T07, T11a.
P1 (заявленный функционал): T06b, T08, T09, T10, T11b, T04b.
P2 (качество/эксплуатация): T12, T13, T14, T15.

Логика: сначала «не терять данные и не врать» (T01–T03), затем узкое
доказательство цепочки (T04a), затем честные настройки/матрица/status
(T05, T06a, T11a), затем живой H.323 (T07), затем крупные блоки (T10,
T08, T09, T11b), затем полная матрица раскладок (T04b), полировка и
производительность.

---

## T01. Production fail-closed и гигиена процесса (P0)

**Объём T01 (P0):** только безопасность запуска и один менеджер процесса.
Доки/CI/README — в T15, чтобы не дублировать.

**Проблема.** `configs/prod.example.toml` не задаёт `admin_password`, а
`Config::default()` = `admin/admin` (`vks-common/src/config.rs`). `main.rs`
только `warn!`. Production-шаблон молча поднимает дефолтного оператора.
Плюс два менеджера процесса (`systemd` + `scripts/run-local.sh`), общий лог,
мёртвый `playwright.config.ts` (удалён), устаревшие комментарии `Cargo.toml`/README (исправлены).

**Что сделать.**
1. Ввести `allow_insecure_defaults: bool` (**secure-by-default `false`**;
   `configs/default.toml` явно `true`). При `false`: `jwt_secret` ≥32 байта,
   не dev-секрет; `admin_password`, если задан, не weak/placeholder; пустой
   допустим для уже созданной БД. Отказ старта — **exit 78 (EX_CONFIG)**.
2. `prod.example.toml`: добавить `allow_insecure_defaults = false`,
   `admin_password` не задавать; документировать `VKS_ADMIN_PASSWORD`.
3. Не менять пароль уже существующей БД на каждом старте: `VKS_ADMIN_PASSWORD`
   — только первый bootstrap. Если в БД слабый `admin`/`admin_password` и задан
   сильный env-пароль — разово ротировать и сохранить в `user_credentials`;
   иначе exit 78 с инструкцией (без окна небезопасного старта).
4. `deploy/systemd/`: prod-unit с обязательным
   `EnvironmentFile=/etc/vks/vks.env`, `DynamicUser=yes`,
   `LoadCredential=server.toml:/etc/vks/server.toml`,
   `ExecStart=... --config %d/server.toml`, `NoNewPrivileges=true`,
   `ProtectSystem=strict`, `ProtectHome=true`, `ReadWritePaths=`,
   `StateDirectory`/`LogsDirectory`, `StartLimit*` в `[Unit]`,
   `RestartPreventExitStatus=78`.
5. Один менеджер процесса: `run-local.sh` определяет **установленный**
   unit через `systemctl cat`, не запускает второй процесс вручную, для
   system-unit требует root; dev user-unit и prod system-unit не смешиваются.
6. Удалить/игнорировать: `vks-server.log` (старый), `.accept/`,
   `.h323-venv/`, `__pycache__`, `artifacts/`; **не** `vks_data.db` и не
   `target/release/vks-server` без остановки сервиса.
7. Удалён мёртвый `playwright.config.ts`; E2E — прямой `playwright` API
   (`tests/e2e/live.js`), добавлены README и `package-lock.json`.
8. Убрать устаревшие комментарии: «фаза 0» в `Cargo.toml`, число тестов в
   README.

**Подводные камни.**
- `git clean -xdf` уничтожит живую БД/бинарь — запрещено.
- Смена `admin_password` в конфиге не должна менять существующего
  пользователя; иначе lockout.
- `ProtectSystem=strict` ломает запись SQLite/логов, если не задан
  `ReadWritePaths`.
- `EnvironmentFile` не должен попадать в git; права `600`.
- При отказе старта systemd будет рестартить цикл — добавить `Restart=on-failure`
  + `StartLimitBurst`, чтобы не флудить.
- Проверять на **отдельном** prod-конфиге/порту, не на живом :8080.

**Приёмка.**
- prod-конфиг без секретов → exit **78**, внятное сообщение, `/healthz` не
  поднимается; dev-конфиг стартует; существующая слабая БД + сильный env →
  ротация и старт; без env → exit 78; после ротации рестарт без env работает.
- dev-конфиг стартует как раньше.
- `cargo test --workspace --all-targets`, clippy, fmt — зелёные.
- `systemctl --user show` — один MainPID; prod-unit проходит
  `systemd-analyze verify` (кроме отсутствующего `/opt/vks/bin` до установки).

---

## T02. Целостность БД и жизненный цикл раскладок (P0)

**Статус:** 🟢 закрыт 2026-10-21, ревью 9.5/10 (`layout-ui`), 996 passed / 0 failed / 6 ignored.
**Реализовано (отличия от исходного плана — осознанные):**
- Явная `active_layouts: scope_key → layout_id` вместо «последний в links»: порядок
  `links` остался UI-контрактом и не пересортировывается; active восстанавливается
  из links при загрузке старого документа.
- COW распространён на все мутаторы: `edit_linked`, `switch_video_layout_mode`,
  abonent/contact attach/detach, `edit_global`; alias старого id резолвится на
  write- и read-path, зеркало сессии обновляется при COW конференции, alias
  очищается при detach.
- `detach`/`remove_global`: scoped 404 для inherited/absent, 409 для default,
  409 для shared (>1 scope) без мутаций; после удаления — canonical default + push.
- `create` canonical default переиспользует существующую запись (два POST → один id).
- Per-room сериализация push в движок; advisory lock на `<db>.lock` у сервера и
  `vks-db-dedup --apply`; `VACUUM INTO` + `quick_check`; dedup remap active/aliases,
  заполняет active для всех video-keys по T01-effective (последняя non-default,
  иначе canonical auto/empty); canonical-предикат совпадает с сервером.
- Тесты: 20 T02 API-сценариев + 2 `current_layout_json`, read-path/alias/mirror, dedup active/idempotent,
  black-box отказ CLI при held lock. Контракт cross-scope закреплён тестом:
  после старта сессии раскладку меняет session-scope; conference-правки
  применяются к следующей сессии (live-propagation — точка T03).

**Проблемы (подтверждены кодом).**
1. COW (`resources.rs:989–1021`) клонирует и переключает ссылку **до**
   валидации body; при 422 в памяти остаётся «призрак», следующий commit его
   сохранит. `update_resource` успевает записать `display_name`.
2. `attach` не переставляет id в конец, а `current_layout_json` выбирает по
   типу, а не по порядку применения → «Применить A» может отправить B;
   «Общая» не применяется при привязанной ручной.
3. `remove_global` удаляет shared default у всех комнат; push в MCU нет.
4. `inherit_and_ensure_default_layout` уже переиспользует default, но
   generic `POST /api/v3/video_layouts` (`resources.rs:785–805`) вставляет
   запись безусловно; UI создаёт «Общую» через этот POST → дубли вернутся
   после чистки. Чинить именно generic create, не inherit.
5. `vks-db-dedup` копирует БД без проверки, что сервер остановлен.

**Что сделать.**
1. Валидировать body **до** COW; на всех error-путях восстанавливать
   `*resources = previous` (или строить клон локально и вставлять только
   после успеха).
2. `attach`: `retain(id !=)` + `push(id)` — порядок применения = порядок
   links. `current_layout_json`: брать последний существующий id (явное
   поле `active_layout_id` — предпочтительно; миграция — fallback).
3. `remove_global`: если layout referenced > 1 — **строго `409 Conflict`**
   (без scope-detach); для default — запрет удаления при ссылках. Решение
   зафиксировано, без «или».
4. `create` default: канонизация payload, поиск существующего default с
   равным payload, возврат его id; тест «два POST default → count не растёт».
5. `detach`/default: push auto/default в MCU (см. T03).
6. dedup-инструмент: guard по `/healthz`/lock-файлу, backup через SQLite
   backup API, dry-run по умолчанию, `--apply` явно.

**Подводные камни.**
- `commit` пишет весь документ; при ошибке после клонирования нельзя
  оставлять мутации.
- Ссылки живут в двух scope: `conferences:{id}:video_layouts` и
  `sessions:{id}:video_layouts`; attach/detach должны работать для обоих.
- Порядок links — часть контракта UI; сортировка/дедуп links может сломать
  «активную».
- Удаление default может оставить комнату без раскладки — предусмотреть
  восстановление канонической.
- Существующие 97 комнат/30 layouts: миграция должна быть идемпотентной.

**Приёмка.**
- Тест: PATCH с невалидным payload → 422, затем любой успешный commit —
  документ байт-идентичен исходному.
- Тест: attach A → attach B → «Применить A» → MCU получает A.
- Тест: default + manual; attach default → активна default, MCU auto.
- Тест: DELETE shared default → 409 (или scope-detach), другие комнаты целы.
- Тест: два POST default → один layout.
- `cargo test -p vks-admin --lib`, E2E без регрессий.

---

## T03. Применение раскладки в gateway/MCU (P0)

**Статус:** 🟢 закрыт 2026-10-21, ревью 9.5/10 (`protocols-net`, `media-engine`); 1006 passed / 0 failed / 6 ignored.
**Реализовано (отличия от исходного плана — осознанные):**
- Вместо «early-return только если комнаты нет» — **pending layout**: движок
  хранит раскладку до первого bridge и не создаёт 2 МБ композита для пустой
  SFU-сессии; audio-only первый bridge pending не теряет.
- auto/«Общая» с пустыми slots не синтезирует фиксированную сетку 13:
  gateway шлёт пустую geometry, MCU строит сетку по числу забридженных;
  `manual` с пустыми slots остаётся preset-геометрией.
- Repair: только свободные geometry-слоты, full-canvas удалён и для назначений
  вне geometry (`sanitize_assignments`).
- 1-based: явный `payload.position_base` (default 1, opt-in 0) вместо inference.
- `z_order` end-to-end: `PixelRect.z` + сортировка compositor `(z,key)`;
  gateway сохраняет вендорный `z_order` manual-пресетов.
- Версия монотонна без двойного инкремента; pending чистится в `destroy_room`.
- Тесты: pending/audio-only, 4 geometry + 5 паблишеров, sparse 0-based,
  manual 2×2 → auto 2 ячейки, gateway z-order, reload F1 canonical default.
- Подробности: `docs/progress/evidence/t03/SUMMARY.md`.

**Проблемы.**
1. `gateways.rs:760`: `assignments.is_empty() && mode != "manual"` → no-op
   даже при существующей MCU-комнате; detach/auto не доезжает.
2. `engine.rs:741–766`: repair ищет слот в `0..u32::MAX` без geometry; при
   5 паблишерах и 4 слотах пятый получает full-canvas rect.
3. `gateways.rs:685`: эвристика 1-based по наличию нуля ломает разреженные
   0-based `[2,3]`.
4. `bridge_participant` (`engine.rs:525`) — тот же неограниченный скан.

**Что сделать.**
1. `apply_layout`: early-return только если MCU-комнаты ещё нет; для
   существующей — применять auto/default геометрию с пустыми assignments
   (движок сам назначит по числу участников) и инкрементить версию.
2. Repair: назначать только свободные slot id из `layout.geometry`; если
   свободных нет — не назначать (лог + метрика), никакого full-canvas.
3. 1-based: зафиксировать явный контракт API (`calculate`/`attach_abonent`
   — 1-based) полем/версией; убрать inference по `max(position)` и
   «нет нуля».
4. `bridge_participant`: тот же ограниченный пул слотов.

4. `z_order` end-to-end: overlap/pip-пресеты должны реально перекрывать
   окна; либо убрать их из UI, пока композитор не умеет. Сейчас это дыра
   в ТЗ, а не «примечание».

**Подводные камни.**
- Auto-раскладка без slots не должна создавать MCU-комнату на каждую
  SFU-сессию (2 МБ композита) — early-return только при отсутствии комнаты.
- Инкремент версии layout обязателен, иначе композитор не перестроится.
- `z_order` пока игнорируется композитором — не считать это исправленным.
- Пустые slots + существующая комната: движок должен получить режим auto,
  а не пустую геометрию.

**Приёмка.**
- Тест: 4 слота + 5 паблишеров → ровно 4 slot_rect, без full-canvas.
- Тест: detach ручной → default доезжает до MCU, версия растёт.
- Тест: sparse 0-based `[2,3]` → слоты 2/3, не 0/1.
- Живой E2E: смена 2×2 → «Общая» → снова 2×2, композит меняется.

---

## T04a. Узкий P0-smoke раскладок (P0-доказательство)

**Статус:** 🟢 E2E-прогон 2026-10-22: 98 checks / 0 failed / 109.9 s, pageerror 0,
5xx/501 0, live SIP; deploy нового T03-бинаря и dedup 6→5; evidence
`docs/progress/evidence/t04a/SUMMARY.md`.
**Реализовано:** `tests/e2e/live.js`: 7 подвариантов 2×2; UI-посадка в ячейки
1/N/2/N для 4/8/9/25 (bbox+текст); calculate 4/8/9/25 (1-based); clear-all→AUTO;
detach→визуальная «Общая»; final pageerror/4xx/console после cleanup; удаление
созданных layout/комнаты. Полная матрица 11 диапазонов/34 ячеек — T04b.

**Проблема.** E2E 34/34 покрывает только manual 2×2 и ячейку 1. Нет
detach/возврата к «Общей». Полная матрица 34 ячеек — отдельный T04b.

**Объём T04a (P0, ≤3 мин):** 2×2 и его 7 подвариантов; ячейки 1, N/2, N для
4/8/9/25; clear-all → AUTO; detach → «Общая»; pageerror 0.

**Что сделать.**
1. `tests/e2e/live.js`: 2×2 → 7 подвариантов; для 4/8/9/25 — ячейки
   1, N/2, N (bbox + текст); clear-all → AUTO; detach → «Общая»; pageerror 0,
   5xx/501 0.
2. Полная матрица 11 диапазонов и 34 ячеек — в T04b (P1, ≤15 мин).
2. Скриншоты на каждую проверку, JSON-отчёт, trace.
3. Скрипт идемпотентен: удаляет созданные комнаты/раскладки после прогона.

**Подводные камни.**
- SIP-терминал может быть занят — E2E должен падать с явным сообщением
  «remote busy», а не таймаутом.
- Селекторы вендорские/локализованные: `button[title=...]`, `.address-manager`,
  `getByTitle(/Очистить все видеоокна/)`; фиксировать по ARIA/title.
- Вендорские `TypeError` (null route/displayName/mode/connectionsLimit) —
  известный шум; фильтровать точечно, pageerror оставлять 0.
- 97 тестовых комнат в списке замедляют UI; чистить префиксы.
- Артефакты не коммитить в git.

**Приёмка T04a.** Один прогон ≤3 мин, все checks pass, отчёт с числами схем
и координатами ячеек; E2E запускается без ручных шагов.
**T04b (P1):** 11 диапазонов, 34 ячейки, подписи/emoji, ночной прогон ≤15 мин.

---

## T05. Реальные max frame size / bitrate (P0)

**Проблема.** UI и payload конференции хранят `1280x720@30`, `bitrate=8192`,
но gateway использует фикс `1280x720/2048/30`; SDP `max-fs=920;max-br=256`,
`b=AS` нет. Настройки не влияют на эфир.

**Что сделать.**
1. Прочитать настройки в `GatewayManager`/`gateway_video`: приоритет
   call_string → conference settings → global video settings → env.
2. Профиль `VideoProfile`: width/height/fps/bitrate; валидация (16..1920,
   fps 1..60, bitrate 64..8192).
3. SDP: `b=AS:<kbps>`, `a=fmtp` `max-fs` (макроблоки 16×16), `max-mbr`,
   `profile-level-id`; для H264-high — level.
4. FFmpeg/энкодер: реальные `-s`, `-r`, `-b:v`, `-maxrate`, `-bufsize`;
   проверить, что смена 1920×1080/4096 меняет битрейт и пакеты.
5. Тест: два профиля → разные SDP и разные `runtime_video_size/bitrate`.

**Подводные камни.**
- Единицы: UI kbps, SDP `b=AS` kbps, FFmpeg `-b:v` bits/s; max-fs — блоки
  16×16 (1280×720 → 80×45 = 3600, не 920).
- Терминал может не поддержать профиль → 488/fallback, не молчание.
- H264 level/profile: high требует level ≥4.0 для 1080p.
- Не сломать Native-профиль и существующий E2E.

**Приёмка.** Живой вызов с 1920×1080/4096: SDP содержит `b=AS:4096`,
`max-fs` соответствует; **фактический RTP-битрейт** (`engine/abonents` или
pcap) ≈4 Mbps; с 640×360/512 — меньше. Тест на парсинг SDP.

**Статус 2026-10-22 — T05a закрыт `a2f934e`, ревью 9.5/10.** SDP high:
`b=AS:4096;max-fs=8160;max-mbps=244800;profile-level-id=42c028`, live tx
4.02 Mbps; low: `b=AS:512;max-fs=920`, live tx 0.61 Mbps; E2E 108/108 оба
профиля. H.323 H.241/SFU `b=AS` — T05b:
H.323 H.241 maxBitRate/level из VideoProfile и per-media `b=AS` — **T05b-1
закрыт 9.5/10 APPROVED** (`media-engine`, `c26e185`). T05b-2 SFU-часть
закрыта по коду: per-room `b=AS`, RTP-map kind-фильтр и PT-reuse,
per-subscriber PT rewrite на egress, валидный rejected m-line, peer H.241
parse/clamp, profile bits, Python CI; 1036/0/6 + 114/114 high/low.
Открыты live H.323 (`remote_release` даже на default, T07) и call_string
live proof. H.264/H.265 в browser SFU — только с реальным forwarding
(T06b/T08). Полный T05b не закрывать до live/T07.

---

## T06a. Честная кодек-матрица без транскодера (P0, дешёвый)

**Что сделать.** Синхронизировать `/settings/calls`, конференцию и UI с
реально мостимыми кодеками; не объявлять в SDP то, что не мостим; при
несовпадении — явный `488 no_common_codec` с cause. Это gate: решаем
«native-only» до T05/T14.

**Приёмка.** UI/настройки/конференция/SDP согласованы; G722-first терминал
получает понятный 488 или PCMU fallback, а не молчаливую потерю.

**Статус 2026-10-22 — T06a закрыт `22c7003`, ревью 9.5/10 APPROVED.** На момент T06a
native-оффер был PCMU/PCMA; в T06b-1 он расширен G722 PT9 (см. ниже), полные
списки по-прежнему intent, API помечает `supported`; Full включается только
с `VKS_TRANSCODER_ENABLED=1`/lab-флагом; выбор только немостимых кодеков даёт
явную ошибку до дозвона; UAC сохраняет `NoCommonCodec{codec,payload,peer_status}`. Live
high/low T06a: 114/114 checks, audio TX/RX 562/367 и 542/348, rejected 0, video 3.55/0.55 Mbps. T06b —
транскодер полной матрицы; H.323 — T05b/T07.

**T06b-1 (G.722 SIP end-to-end) — закрыт `f8456b0`, ревью ref-engines 9.5/10 APPROVED.**
G722 PT9/8000 native в SDP (offer `0 8 9 101`, inbound UAS отвечает PT9),
строгий выбор первого общего кодека без позднего PCMU fallback, G722
16 кГц↔48 кГц мост с payload 160 B/20 мс и timestamp +160, media details
`audio_codec=G722`/`audio_sample_rate=16000`. Live low-video E2E 233/233,
audio TX 545/RX 361, rejected 0, G722/PT9/16000/8000 asserted; unit 72 SIP +
72 server, clippy/fmt clean.
Evidence: `docs/progress/evidence/t06b1/`. Остальные кодеки/видео — T06b-2..5,
H.323 — T06b-4; полный T06b не закрыт. В T06b-2a добавлен G726 all-rates.

**T06b-2a (G.726 16/24/32/40 SIP) — закрыт `17b8660`, ревью 9.5/10 (media-engine, ref-engines) APPROVED.**
`oxideav-adpcm =0.0.7`, LSB-first для всех четырёх rate, PT107–110 в
native-оффере, 20/30/40 мс frame lengths, `AudioBridgeCodec` в мосте,
UAS отвечает G726-only и принимает произвольный inbound PT 96–127 с
`a=rtpmap` (кодек едет отдельно от PT), variant-aware
`is_native_audio_variant`, FFmpeg `g726le` probe. Live E2E остаётся G722
233/233 (`.132` G726 не предлагает). H.323 динамические PT/TCS — T06b-4.
Evidence: `docs/progress/evidence/t06b2a/`.

**T06b-2b (Opus + G.728, остальное — явный 488) — реализован 2026-10-22, ожидает ревью/коммита.**
`rusty-opus =1.0.2` (pure Rust, 960 сэмплов/+960, bridge-side FEC/PLC по
sequence-gap, PT111, dynamic inbound PT, mono `stereo=0`, DTMF в отдельном
8-кГц домене), `oxideav-g728 =0.0.8` (PT15, 40 B/20 мс, +160), native offer `0 8 9 15 107 108 109 110 111 101`. G723/G729/G722.1(C)/
G719/AAC-LATM — 488 до 200 с явной причиной; G723 (30 мс) — T06b-2c.
Live E2E 233/233 остаётся G722 (`.132`), settings `OPUS`/`G728` supported.
Evidence: `docs/progress/evidence/t06b2b/`.

## T06b. Транскодирование полной матрицы (P1)

**Проблема (состояние до T06b-1).** В настройках 18 audio/8 video, в
конференции 16/5, в UI 17; реально мостились PCMU/PCMA/H264, G722 первым у
терминала отбрасывался. В T06b-1 G722 SIP/T06b-1 уже native; остальные
кодеки полной матрицы по-прежнему только intent/env.

**Что сделать (T06b, после T06a).**
1. `vks-transcoder` на GStreamer 1.28.2/FFmpeg 8.0.1 (LGPL),
   матрица: AAC, G711, G719, G722, G722.1 24/32, G722.1c 24/32/48, G723,
   G726 16/24/32/40, G728, G729, OPUS; H261, H263, H264, H264-high, H265.
4. Динамические PT: G722 PT9, PCMU/96, OPUS 111, H264 97/98, H265 99.
5. Тест: для каждого кодека — offer/answer + фактический RTP payload.

**Подводные камни.**
- Лицензии: libx264 GPL; для поставки — OpenH264 или LGPL FFmpeg.
- CPU: транскод 1080p дорог; нужен per-session budget и отказ.
- G722 — 16 kHz, clock rate 8000 в SDP; не путать sample rate.
- H265 в браузере не везде; не обещать.
- Нельзя молча отбрасывать выбранный терминалом кодек — это баг приёмки.

**Приёмка.** Матрица из ТЗ проходит либо явно отклоняется с 488; в логах нет
«codec dropped» без причины. E2E проверяет payload.

---

## T07. H.323 live (P0 — требование пользователя)

**Проблема.** Сейчас `remote_release` при доступном терминале (5060/1720
open); ранее были CONNECT. Нужен живой вызов с двусторонними медиа.

**Что сделать.**
1. Освободить/перезапустить H.323 на `192.168.1.132` (не ломая SIP).
2. Снять Q.931 cause, H.245 OLC, pcap; проверить RAS/registration/fastStart.
3. Если причина внешняя — зафиксировать как ограничение и взять второй
   терминал/эмулятор; не подменять успех «код готов».
4. Добавить ignored live-тест `live_h323.rs` с явным сообщением.

**Подводные камни.** Терминал занят другим вызовом; неправильный
`candidate_addr`; H.245 tunneling; codec OLC; таймауты; не трогать
prod-настройки терминала без согласования.

**Приёмка.** Живой H.323: CONNECT, аудио/видео в обе стороны, hangup, UI
показывает состояние; лог без ошибок.

---

## T08. WebRTC-транспорт (P1)

**Проблема.** `udp_loop` дропает DTLS, STUN без MESSAGE-INTEGRITY, str0m
опционален и не включён. Браузерный SFU — не production.

**Что сделать.** Включить `str0m 0.24.1`, ICE-lite/full, DTLS-SRTP,
STUN Binding c FINGERPRINT/MESSAGE-INTEGRITY, TURN (coturn 4.7), ICE restart,
renomination; браузерный Playwright-тест двустороннего медиа.

**Подводные камни.** Сертификаты/фингерпринты; ICE candidate IP; TURN creds;
browser autoplay; SDP munging; не сломать SIP-путь.

**Приёмка.** Chromium открывает комнату, публикует камеру, второй участник
видит/слышит; ICE connected, DTLS established, SRTP.

---

## T09. Устойчивость медиа (P1)

**Проблема.** RTCP дропается, нет NACK/RTX/FEC/jitter buffer/PLI; на потерях
видео деградирует.

**Что сделать.** NACK/RTX, FlexFEC, jitter buffer, PLI/FIR/keyframe, RTCP
RR/SR, TWCC/REMB, pacer; тест через `tc netem loss 10%` (если есть права)
или симулятор.

**Подводные камни.** RTCP compound; SSRC/PT; не смешивать с RTP; бюджет
задержки; тесты без root.

**Приёмка.** 10–20% loss — видео восстанавливается, метрики loss/jitter
видны в статистике.

---

## T10. Единый справочник и БД (P1)

> **T10a (UI/справочник) закрыт 2026-10-22, 9.5/10 APPROVED (`directory-db`), HEAD `a8c9d66`.**
> Одна SQLite, один configuration-document; UI-справочник показывает active-пользователей
> и привязанный адрес, дублей canonical URI нет, рестарт сохраняет, vendor-алиасы 200,
> startup-dedup адресов с переписыванием личных ссылок. **T10b** — SQL-нормализация
> (`call_strings` + unique index, перенос directory, миграция history `contact_id`,
> drop пустых legacy-таблиц) остаётся открытой.

**Проблема.** Два контура (SQL `core_*` + JSON-документ), 4 записи call_strings на 3 URI (JSON-коллекция), UI-справочник показывает только 2 пользователей без
адресов, пустые legacy-таблицы.

**Что сделать.**
1. Один источник: `core_users` + `call_strings` + `user_credentials`;
   JSON-документ — только PNM-ресурсы (или наоборот, но один).
2. Канонический URI: один `canonical_uri` (схема/хост/порт/регистр), уникальный
   индекс; миграция дублей с backup.
3. UI `/ui/call_strings` и `/ui/users` — единый список контактов; адреса
   видны в справочнике.
4. Удалить пустые legacy-таблицы после миграции.
5. Тест: один URI → одна запись; справочник показывает адреса и контакты.

**Подводные камни.** Живая БД; стабильность canonical id; не потерять
историю; FK/каскады; конкурентные запросы.

**Приёмка.** В UI справочника все контакты и адреса; дублей нет; рестарт
сохраняет.

---

## T11a. Честный system status (P0, дешёвый)

**Статус 2026-10-22 — закрыто, 9.5/10 APPROVED (`stats-deps`), HEAD `8a75350`:**
реальный IP 192.168.1.93, `database.size_bytes` = main+WAL (2048000 == `ls -l`,
файл канонизирован), SIP `registration` от реальной UAS; H.323 `READY` только
по факту H.225 setup или реального Q.931-ответа, до вызова `DISABLED`;
структурная классификация terminal/transport (SIP `SipDialError`, H.323
точный sentinel `remote_release`), TTL-самолечения нет; evidence
`docs/progress/evidence/t11a/`.

**Проблема.** `/api/v3/ui/system/status` отдаёт `network.ip=127.0.0.1`,
`sip/h323 local_address=127.0.0.1`, `database.size_bytes=0`, sip/h323 READY
при `remote_release`.

**Что сделать.** Реальный IP из конфига/сокета, размер БД через `stat`,
READY/FAILED по фактическому состоянию шлюзов, без статики.

**Приёмка.** Ответ совпадает с `ip addr` и размером файла БД; при упавшем
H.323 — FAILED, а не READY.

## T11b. Статистика и QoE (P1, после T09)

**Проблема.** 4/6 виджетов реальные; workers/resources статичны; Chart.js
оси битые; нет RTP/QoE; `network.ip=127.0.0.1`, `database.size_bytes=0`.

**Что сделать.**
1. `engine/diagnostics`: реальные workers (пул), resources по нагрузке.
2. Chart.js **v4.5.1** (в бандле): заменить v2-ключи `yAxes/xAxes` на
   `scales:{x:{},y:{}}`, проверить responsive.
3. RTP/QoE из `engine/abonents` **после T09** (иначе метрик нет).
4. Тест: страница без console.error, виджеты не плоские.

**Подводные камни.** Первый сэмпл после простоя — нули; не блокировать
запросы; единицы; вендорский UI ожидает конкретные поля.

**Приёмка.** 6/6 виджетов живые, оси корректны, RTP-метрики видны.

---

## T12. UI: поллинг, превью, подписи (P2)

> **T12a (подписи без emoji) закрыт 2026-10-22, 9.5/10 APPROVED (`layout-ui`), HEAD `a4629a4`.**
> Единый denylist-санитайзер в `vks-common::text`: сохраняет кириллицу/combining/
> типографику, убирает emoji/флаги/ZWJ/keycap/bidi/control, grapheme-safe лимит 128;
> санитизируется только явный `display_name` на write-boundary, raw identity/URI/
> history/audit/slot не перезаписываются; PATCH без имени сохраняет raw-имя.
> Evidence `docs/progress/evidence/t12a/`. **T12b** (CSS-центр, previews, лишние
> codec-запросы) остаётся открытой.

**Проблема.** 1550×2 запросов codecs за 79 с; `GET /api/v3/abonents/previews`
сейчас **200 с пустым списком** (кнопка мертва, не 501); вертикальный центр
подписи для 25 окон dy=18.7; emoji в именах.

**Что сделать.** ETag/304 или кэш на страницу; один запрос настроек на
сессию; previews — реальные кадры или убрать кнопку; CSS-центрирование
подписи; санитайз имён участников.

**Подводные камни.** Вендорский бандл минифицирован; не ломать другие
страницы; кэш не должен отдавать устаревшие настройки после PATCH.

**Приёмка.** ≤2 запросов codecs на страницу; previews 200; подпись по центру
вертикально и горизонтально; emoji нет.

---

## T13. Элегантность и мёртвый код (P2)

**Кандидаты (из аудита).** ~300 строк compat в `preset.rs` (`catalog`,
`PresetInfo`, `GRID_SCALE`, `slots_to_grid`, `expected_slot_count`);
23 мёртвые `pub fn`; 21 неиспользуемая зависимость; дубли `now_*` (10),
`csv_escape` (3, с расхождением), `canonical_uri` (2); `common::ratelimit`
vs `WsRateLimit`; неподключённые модули (media_policy, retention, circuit,
dtls, reconnect, focus, slo, rng, clock) — решение wire/delete/feature-gate.

**Правила.** Удалять только то, что не публичный контракт; `cargo machete`
не установлен — проверять grep; не ломать feature-гейты; после каждого
пакета — clippy/tests.

**Приёмка.** Минус ≥700 prod-строк без изменения поведения; `cargo test`,
clippy, E2E зелёные; README/доки обновлены.

---

## T14. Производительность и масштабирование (P2)

**Проблема.** Глобальный Mutex комнат, f64-композитор, аллокация canvas на
кадр, нет бенчмарков; 25/34 не проверены.

**Что сделать.** Per-room actors, пул буферов, целочисленный/SIMD композит,
бенчмарки criterion 0.7; SLO: composite p99 ≤2 мс 1080p/4, 1 vCPU ≈6–8
комнат 720p30; soak.

**Подводные камни.** Не оптимизировать до корректности; не ломать sans-I/O;
измерять до/после.

**Приёмка.** Бенчмарки в CI, 25/34 без sleep/потерь, CPU в бюджете;
нагрузочный сценарий 25/34 — после T04b.

---

## T15. Документация, CI, релиз (P2)

**Что сделать.** Обновить `ARCHITECTURE_AND_VERSIONS`, README, ADR; запинить
GitHub Actions по SHA; `--locked`; добавить `cargo machete`/`udeps` (deny уже
в CI); тег после приёмки; бэкап/восстановление. **Не дублировать** prod-unit
(T01) и deny-джоб.

**Приёмка.** CI зелёный, релиз собран, тег, документация соответствует коду.

---

## 2. Definition of Done для каждой задачи

1. Код + тесты (unit/integration) на новый сценарий.
2. `cargo test --workspace --all-targets` — 0 failed.
3. `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
4. `cargo fmt --all -- --check` — OK.
5. Живой Playwright/E2E, если задача меняет UI/медиа.
6. Независимый саб-агент-ревьюер: оценка **≥9.5/10**, замечания закрыты.
7. Commit с описанием; задача отмечена выполненной в этом файле.

## 3. Риски верхнего уровня

| Риск | Уровень | Митигация |
|---|---|---|
| H.323 терминал недоступен/занят | P0 | отдельный терминал/эмулятор, не блокировать остальное |
| Транскодинг всей матрицы = CPU/лицензии | P0 | этапный план, явный 488, LGPL/OpenH264 |
| Живая БД при миграциях | P0 | backup, dry-run, одна транзакция |
| Вендорский UI/лицензия | P1 | release-artifact + sha, не трекать 25 MB |
| Регрессии в media-пути | P1 | тесты + E2E + лог-контроль |
| Нехватка времени на все P1/P2 | P2 | порядок T01–T05 сначала, остальное итеративно |

## 4. Текущий фокус

**T01 закрыт**: оценка ревьюеров 9.5/10 (`directory-db`, `rtcp-demux`,
`ref-engines`), gates 970/0/6, clippy/fmt зелёные, evidence
`docs/progress/evidence/t01/SUMMARY.md`. Первая незакрытая задача — **T02
(COW/attach/default)**; далее T03 (MCU apply), T04a (E2E smoke), T05
(bitrate/frame size).
