# MASTER_TASK_LIST — приоритетный план доведения VKS MCU до приёмки

> Рабочий трекер. Обновляется в коммите каждой задачи (статус + ссылка на коммит + ревью-оценка).
> Источники: закрытый `docs/progress/archive/REWORK_PLAN.md`, `docs/architecture/*`, аудит 2026-10-16,
> живая проверка 2026-10-19/20, документ TrueConf MCU 2.2.1.66 (страница мониторинга сессии).
>
> Главный приёмочный сценарий (P0): **frontend → создание комнаты → настройка → запуск →
> вызов 192.168.1.132 → двусторонние аудио/видео → управление → завершение**.
> SIP и H.323 проверяются отдельными ветками, но обе должны доходить до живого участника.
>
> Последний приёмочный прогон: 2026-10-21, HEAD `aa9a1ec`, SIP и H.323 live-проверены.
>
> **T01 (production fail-closed/гигиена) закрыт:** `64337d6` + `78ca9ed`, ревью 9.5/10,
> 970 passed / 6 ignored, evidence `docs/progress/evidence/t01/`. Детальный план — `MASTER_PLAN_2026-10-21.md`.
>
> **T02 (целостность БД/жизненный цикл раскладок) закрыт:** ревью 9.5/10 (`layout-ui`),
> 996 passed / 0 failed / 6 ignored, dedup 30→22 layouts / 15→7 defaults, evidence
> `docs/progress/evidence/t02/`. F1-фикс сохранения canonical default — `ed24d02`.
>
> **T03 (применение раскладки в gateway/MCU) закрыт:** ревью 9.5/10
> (`protocols-net`, `media-engine`, `layout-ui`), 1006 passed / 0 failed / 6 ignored,
> pending layout + auto по числу участников + geometry-only repair + z_order +
> монотонная версия, evidence `docs/progress/evidence/t03/`.
>
> **T04a (live-smoke раскладок) закрыт:** ревью 9.5/10 (`layout-ui`), 98 checks /
> 0 failed / 109.9 s, pageerror 0, 5xx/501 0; 7 подвариантов 2×2 + UI-посадка
> 1/N/2/N для 4/8/9/25 + calculate 4/8/9/25 + detach→визуальная «Общая»;
> deploy нового бинаря и dedup 6→5; evidence `docs/progress/evidence/t04a/`.
> Полная матрица 11 диапазонов/34 ячеек — T04b.
>
> **T05a (реальные max frame size / bitrate) закрыт:** `a2f934e`, ревью 9.5/10
> (`media-engine`, `protocols-net`), 1014 passed / 0 failed / 6 ignored.
> Единый профиль env → global → conference → call_string; SDP `b=AS:4096`,
> `max-fs=8160`, `profile-level-id=42c028`; живой SIP 1920×1080/4096 —
> фактический RTP tx **4.02 Mbps**, 640×360/512 — **0.61 Mbps**; оба E2E
> 108/108, page_errors 0. Удалён жёсткий 640×360 в admin-статистике.
> H.323 H.241 и SFU b=AS — T05b. Evidence `docs/progress/evidence/t05/`.
> **T06a (честная матрица без транскодера) закрыт:** `22c7003`, ревью **9.5/10 APPROVED** (`media-engine`), см.
> `docs/progress/evidence/t06a/SUMMARY.md`. Единый capability-модуль, Full
> только с транскодером/lab-флагом, явная ошибка `no common audio|video codec`
> до дозвона, типизированный `NoCommonCodec`, `supported` в API, live
> 114/114 checks ×2, audio TX/RX >0, rejected 0. T05b-1 закрыт **9.5/10 APPROVED**: H.323 H.241 + per-media SFU `b=AS`
(`7b05905`, `68da633`, `5ae9924`, `c26e185`), evidence
`docs/progress/evidence/t05b/`. **T05b-2:** SFU-часть (per-room b=AS,
RTP-map kind-фильтр/PT-reuse, per-subscriber PT rewrite, rejected m-line,
peer H.241 parse/clamp, profile-bits, Python CI) закрыта по коду и тестам
(1036/0/6 + 114/114 high/low); открыты live H.323 (T07) и call_string live
proof с реальным терминалом. H.264/H.265 в browser SFU — T06b/T08.
>
> **T10a (UI/справочник) закрыт:** `a8c9d66`, ревью **9.5/10 APPROVED**
> (`directory-db`). Одна SQLite, один configuration-document; `/api/v3/users` = 2 active,
> `/api/v3/call_strings` = 2 canonical URI без дублей; vendor-алиасы коллекций
> (11 trailing-slash + users/directory + detail) 200 и идентичны canonical;
> startup-dedup canonical адресов + переписывание личных ссылок; рестарт сохраняет;
> 274/0 admin, clippy/fmt чисто. **T10b (SQL-нормализация)** — открыта отдельно:
> durable unique index `call_strings`, перенос addresses/directory из document,
> миграция history `contact_id`, drop пустых legacy-таблиц.
>
> **T07b (кросс-MCU SIP, 20 вызовов) — живой прогон выполнен, interop-баг исправлен:**
> HEAD `885371b`. 20 SIP call string на TrueConf `35a87982...@192.168.1.92`,
> 17 одновременно с двусторонним audio+video (`audio_rx=27402`,
> `video_rx=23001`, `rejected=50`, H.264 1280×720@30, PCMU/PCMA).
> Найдено: TrueConf отвечает PCMU, но шлёт PCMA PT8; `RtpSource` отбрасывал
> аудио. Исправлена взаимозаменяемость G.711 0↔8 с декодом по PT пакета;
> видео-PT оставлен строгим, иначе ломался приём H.264. Тест
> `accepts_alternate_g711_payload_and_reports_actual_pt`. Evidence
> `docs/progress/evidence/t07b-cross-mcu/`.
> H.323-ветка: `h323:<alias>@<ip>` передаёт alias в H.225/Q.931, принимает любой
> H.264-профиль/level пира; живой прогон **20/20 H.323** на
> `h323:Conf_293662065@192.168.1.92` → `connected=20`, `audio_rx=33841`,
> `audio_tx=34304`, rejected=0 (TrueConf H.323 отдаёт audio-only; SIP-ветка
> даёт H.264 video). **T07c (входящий SIP UAS) — закрыт** на `078339c`: UDP listener 5060,
> 180/200 с To-tag, ACK/BYE/CANCEL с диалоговой валидацией, 20 одновременных
> входящих, media-timeout с BYE и очисткой registry/RoomHub, re-INVITE 501,
> graceful shutdown с BYE всем активным диалогам. Ревью: `protocols-net`
> **9.5/10**, `media-engine` **9.0/10**, P1 нет; LAN-scope принят.
> Evidence `docs/progress/evidence/t07c/`.
>
> **T07d (входящий H.323 UAS) закрыт:** ревью `media-engine` **9.5/10**,
> `stats-deps` **9.5/10**, `protocols-net` **9.6/10**, 1078 passed / 0 failed /
> 6 ignored, clippy clean. Q.931/H.225/H.245 listener (TCP 1720, fork-per-call),
> двусторонние audio PT0 + video PT96, 20 одновременных вызовов в сессию
> `4d313d01-...`, 20/20 connected + video, 0 детей/0 зомби после cleanup.
> Security: opt-in/allowlist fail-closed, peer IP pinning, отключён source-latch,
> max calls 20, pending TTL + hard fallback, video-close не рвёт audio,
> cancellation-safe reader task, single reaper без stale PID, `KillMode=mixed`,
> process group, pending leases до остановки listener. Evidence
> `docs/progress/evidence/t07d-h323-inbound/`.
>
> **T12a (подписи без emoji/стикеров) закрыт:** `a4629a4`, ревью **9.5/10 APPROVED**
> (`layout-ui`). Единый denylist-санитайзер `vks-common::text` (кириллица/combining/
> типографика сохраняются, emoji/флаги/ZWJ/keycap/bidi/control удаляются,
> grapheme-safe лимит 128); санитизируется только явный `display_name` на
> write-boundary, raw identity/URI/history/audit/slot не перезаписываются;
> PATCH без имени сохраняет raw-имя; live POST с emoji → очищено, URI не изменён;
> 1059/0, clippy/fmt чисто.
>
> **T12b (CSS-центр подписи, previews, лишние codec-запросы) закрыт:** ревью
> **9.5/10 APPROVED** (`stats-deps`; `protocols-net` — 9.2 без P1). Подписи
> отцентрованы на всех 11 диапазонах (`|dx|,|dy|<0.01 px`); codec-GET схлопнуты
> до 2 network/документ через pre-bundle XHR coalesce с двухфазной PATCH-
> инвалидацией и cross-tab/focus revalidation; мёртвый previews-polling убран,
> чекбокс скрыт; codec/calls PATCH атомарны, persist fallible (503+rollback),
> ETag/304 для strong/weak/list/`*`. `e2e-t12b6`: 232/232; targeted race
> `perf-cache.js` fresh=true; 281/0, clippy/fmt чисто, manifest OK.
> Evidence: `docs/progress/evidence/t12b/`.
>
> **T06b-1 (G.722 SIP end-to-end) закрыт `f8456b0`, ревью 9.5/10 APPROVED:** G722 PT9/8000
> native, native offer `0 8 9 101`, inbound SIP UAS отвечает G722, строгий выбор
> первого общего кодека (нет позднего PCMU fallback), G722 16k↔48k RTP-мост
> 160 B/20 мс + timestamp 160, media details sample_rate 16000/clock 8000.
> Live low-video E2E 233/233, audio TX545/RX361, rejected 0, G722/PT9/16000/8000
> asserted; 72 SIP + 72 server тестов, workspace clippy/fmt clean. Evidence `docs/progress/evidence/t06b1/`.
> **T06b-2a (G.726 all rates) реализован, ожидает ревью/коммита:** oxideav-adpcm 0.0.7,
> LSB-first, PT107–110, 20/30/40 мс, AudioBridgeCodec, UAS G726-only, variant-aware
> settings precheck, FFmpeg g726le probe; live E2E остаётся G722 233/233.
> Inbound произвольный dynamic PT 96–127 с `a=rtpmap` поддержан (кодек отдельно от PT).
> Полный T06b (G723/G728/G729/Opus/AAC/H265/H323-матрица) остаётся открытым: T06b-2..5.
>
> Правило цикла: перед задачей — агент «подводные камни»; после реализации — параллельное
> code-review (оценка /10, обязательные правки); цикл не закрывается, пока хотя бы один
> независимый ревьюер не поставит **≥9.5/10**. Затем — агент «условия задачи/элегантность»
> (≥9/10), правки, commit, отметка задачи.

## Легенда

⚪ не начата · 🟡 в работе · 🟢 завершена · 🔴 заблокирована · ⏸️ отложена

## Факты, уже подтверждённые живой проверкой (2026-10-20)

| Проверка | Результат | Следствие |
|---|---|---|
| SIP-вызов `sip:192.168.1.132` на текущей debug-сборке | `201` → `connecting` → `connected`, participant `sip-…@sip-gateway`, PCMU + H264, RX/TX растут ≥10 c | SIP-ветка рабочая; «исчезновение за 1 c» — не SIP-транспорт, а lifecycle/UI/старый бинарник |
| H.323 Setup с **нулевым/почти нулевым** GUID | Polycom отвечает Q.931 Release Complete `invalidCID` | `conferenceID`/`callIdentifier` нельзя брать из детерминированного нулевого UUID |
| H.323 Setup с **v4 UUID** | `ringing` → H.245 TCP → TCS/MSD → openLogicalChannel; каналы audio PT 0, video PT 109/96 | Сигнализация H.323 доходит до медиа-негоциации; причина прежнего отказа — GUID/состояние endpoint |
| H.323 sidecar | venv `~/.local/share/vks-mcu/h323-venv` рабочий; Pycrate 0.8.1 + `h323_asn` | Не пересоздавать venv; не ломать `PYTHONPATH` |
| H.323 после `resolve_helper()` | `connected`, audio RX/TX, video RX PT 109 / TX PT 96, decoded/encoded >0, жив >12 c | helper-путь был единственной причиной `stdout closed`; H.323-ветка рабочая |
| `DELETE /api/v3/sessions/{id}` после SIP/H.323 | 204; `connected/connecting/disconnected/scheduled/count` = 0 | ghost-ростер закрыт прунингом `call_attempts`/`settings`; «комната не пустеет» больше не воспроизводится |
| `GET /api/v3/sessions/{id}/abonents` | `connected[]` живёт, `RoomHub` отдаёт participant | Ростер не обязан «испаряться»; искать гонку cleanup/старый процесс |

## P0 — приёмка (блокирует всё остальное)

### A0. Triage «участник исчез за 1 c»: один бинарник, один лог, один источник истины

**Статус:** 🟢 (2026-10-20)
**Цель:** исключить ложные симптомы: старый процесс, два сервера, чужой конфиг, UI-кэш.

**Что проверено:**
- `target/debug/vks-server` новее исходников; старый процесс на 18081 остановлен перед пересборкой;
- SIP и H.323 вызовы на пересобранном бинарнике дают `connected` и живут >12 c;
- после `DELETE /sessions/{id}` `connected/connecting/disconnected/scheduled/count` = 0;
- UI-кэш: `MANIFEST.sha256`; для приёмки — hard reload/новый профиль.

**Диагноз:** «исчезновение» воспроизводилось двумя путями: (а) видео-воркер падал уже после
регистрации в RoomHub, и `hangup` снимал участника через десятки мс; (б) `stop_session` не чистил
`call_attempts`, поэтому после завершения в ростере оставался `disconnected` с `count=1`.
Оба пути закрыты в A5/B3. Третий путь — смена identity `cs-…` → `sip-…@sip-gateway`, из-за чего
фронт пересоздаёт строку; это не потеря вызова, но требует стабильного ключа (B3).

### A1a. SIP acceptance-harness: комната → запуск → вызов → ростер → завершение

**Статус:** 🟢 (ручной API-прогон 2026-10-20; автоматизация — C2)
**Цель:** один сценарий на чистой БД: логин, создание комнаты, запуск сессии, вызов
`sip:192.168.1.132`, ожидание `connected`, проверка media-счётчиков, mute/volume/DTMF,
`DELETE /sessions/{id}`, проверка пустого ростера и отсутствия портов.

**Факты прогона:** `POST /conferences` → `POST /sessions` → `POST /ui/sessions/{id}/call_strings`
→ 201/`connecting` → `connected` за ~2 c; PCMU + H264; через 12 c connected=1, RX/TX растут;
mute mic (`microphone`, outgoing=false) даёт `audio.rx.enabled=false`; `DTMF 123#` → 200;
`DELETE` → 204, roster `count=0`.

**Подводные камни:**
- не использовать `/tmp` для лога/БД; только workspace `.accept/`;
- `POST /sessions` без `conference_id` создаёт Quick Session и может автоматически звонить по
  шаблону — для приёмки всегда передавать `conference_id`;
- `POST /sessions/{id}/invite/call_strings` — не то же самое, что `/ui/sessions/{id}/call_strings`;
- ответ 201 не равен `connected`: poll `abonents` до `connected[]`;
- завершение: `DELETE /api/v3/sessions/{id}` (204), затем `abonents` и `engine` должны быть пусты;
- старый процесс/порт 18081/40010 убивает воспроизводимость — проверять `ss -ltnup`.

**Приёмка:** `scripts/accept_sip.sh` (TODO, C2) или ручной прогон с отчётом; повторный запуск
не оставляет сессий/портов.

### A1b. H.323 acceptance-harness: helper path, валидный GUID, connected, медиа

**Статус:** 🟢 (2026-10-20)
**Цель:** тот же сценарий для `h323:192.168.1.132`, с проверкой sidecar и H.245.

**Факты прогона:** после `resolve_helper()` sidecar запускается из runtime-дерева; `connected`
за ~4 c; audio PT 0 RX/TX; video RX PT 109 / TX PT 96; за 20 c decoded 150, encoded 169;
`video_rx.resolution=640x360`; `DELETE` → 204, roster `count=0`.

**Подводные камни:**
- `env!("CARGO_MANIFEST_DIR")` нельзя использовать как runtime-путь; `resolve_helper()` возвращает
  `Result` и ищет: `VKS_H323_HELPER` (обязан быть файлом) → рядом с exe → `../share/vks-mcu` →
  cwd/source-tree **только в debug** → manifest fallback; при отсутствии — ошибка со списком путей;
- sidecar не должен принимать nil UUID (`invalidCID` у Polycom); генерировать v4 внутри Rust;
- видео-reject (чужой sessionID/reverse/не baseline) не должен вешать `connected`: ставить
  `video_unavailable` и подключать аудио; если peer анонсировал H.264, но 2.5 c не открыл
  входящий video OLC (камера выключена), подключить audio-only, а не ждать дедлайн;
- неизвестный/неполный H.245 ACK не должен рвать живой вызов;
- `.132` может быть занят SIP/H.323 — сначала завершить предыдущий вызов;
- не запускать два sidecar на один вызов; stdout — только JSON lines.

**Приёмка:** H.323 `connected`, оба направления RTP, `was_connected=true`, повторный вызов успешен.

### A2. H.323-вызов `.132`: `invalidCID`, `connected`, медиа

**Статус:** 🟢 (2026-10-20)
**Цель:** исходящий H.323-вызов из комнаты доходит до `connected`, sidecar отдаёт валидные
audio/video endpoints, Rust-бридж отправляет/принимает RTP; вызов завершается Release Complete.

**Подводные камни:**
- `conferenceID` и `callIdentifier` — 16-байтовые GUID; **никогда** не использовать нулевой/
  детерминированный UUID (`00000000-…`): Polycom отвечает `invalidCID`;
- в `send_setup` оба поля сейчас равны `uuid.UUID(call_id).bytes`; проверить, что `call_id`
  всегда v4 и не приходит извне как произвольная строка; добавить guard `uuid.UUID(...).version == 4`
  или генерировать внутри sidecar;
- `activeMC`/`conferenceGoal`/`sourceInfo` для direct point-to-point: не выдавать себя за MCU,
  если endpoint ждёт terminal; проверить допустимые значения Pycrate (`conferenceGoal` — не `"call"`,
  а `("call",0)`/`("create",0)` по ASN.1; `sourceInfo` — choice, а не dict);
- `conferenceID` может быть опционален; но если Polycom требует, не удалять без проверки;
- после Setup Polycom может прислать `Alerting` и открыть H.245 только после «ответа»; таймаут
  sidecar 25 c может быть меньше ручного ответа;
- H.245: TCS ACK, MSD ACK, openLogicalChannel ACK; `connected` эмитится только при
  `incoming_audio && remote_audio` (и video, если запрошено); не считать `ringing` успехом;
- `remote_audio` приходит в `openLogicalChannelAck`; если Polycom не присылает ACK, проверить
  `sessionID`, `mediaControlChannel`, `dynamicRTPPayloadType`, `mediaPacketization`;
- video PT: входящий 109, исходящий 96; не перепутать; H.264 baseline 640×360@15;
- sidecar stdout — JSON lines; любая библиотека не должна печатать в stdout (Pycrate уже
  перенаправлен в stderr);
- завершение: `endSessionCommand` + Q.931 Release Complete; не оставлять TCP 1720/H.245 сокеты;
- `.132` может быть занят SIP-вызовом или старым H.323-соединением; перед тестом завершить
  предыдущую сессию и подождать 2–3 c.

**Приёмка:** H.323-вызов `.132` даёт `connected`; в отчёте `remote_audio_port`, `remote_video_port`,
PT; RTP-пакеты идут в обе стороны; `was_connected=true` при завершении; повторный вызов успешен.

### A3. Frontend E2E через PNM UI: создание комнаты, настройка, запуск, вызов, управление, завершение

**Статус:** 🟡 (API-контракт проверен 2026-10-20; визуальный Playwright недоступен в сандбоксе)
**Цель:** оператор в браузере на `http://<host>:8080/pnm/` проходит сценарий без API-хаков;
участник появляется и **не исчезает**, счётчики и таблица обновляются.

**Факты прогона:** `POST /conferences` (payload mode=MCU) → `POST /sessions` → все ключевые
`/api/v3/ui/sessions/{id}/*` (`session`, `abonents`, `contacts`, `video_layouts`) и
`/api/v3/engine/abonents` отвечают 200; `sip:192.168.1.132` появляется в `connected[]` и живёт
>12 c; после `DELETE` ростор пуст. PNM UI — **административная** консоль: в её бандле нет
`getUserMedia`/исходящего WebRTC, поэтому «браузер как участник» не является приёмочным требованием.

**Подводные камни:**
- UI 2.2.1.66 ходит в `/api/v3/ui/*`, а не в v1; проверить все ответы и формы DTO;
- список сессий обновляется поллингом 10 c, участники — 5 c; WS-события `sessions:joined/left`
  фронт **не слушает**; «исчезновение» может быть гонкой между poll и cleanup;
- `RoomHub` — in-memory; после рестарта сервера комната пустая, а UI показывает старую сессию;
- `create-conference-button` создаёт **описание**, а не живую комнату; запуск — отдельный
  `POST /api/v3/sessions {conference_id}`; в UI это разные страницы;
- `display_name`/`route`/`dtmf` генерируются фронтом; route должен быть уникален;
- кнопка «Добавить по адресу» → `POST /ui/sessions/{id}/call_strings`; payload `auto_call`,
  `crown`, `volume_*`; ошибки 409/3 (адрес уже в сессии);
- `connection_id` — число из движка, `abonent_id` — UUID; кнопки управления используют первый;
- после `DELETE /sessions/{id}` UI должен уйти на карточку конференции; проверить отсутствие
  «фантомной» строки;
- кэш браузера: `MANIFEST.sha256`/cache-bust; hard reload или новый профиль;
- не тестировать на старой сборке: `cargo build -p vks-server` и запуск именно её.

**Приёмка:** видеозапись/скриншоты или Playwright-тест: комната создана, запущена, `.132` в
`connected[]`, счётчик «👥 1» и таблица не пустеют ≥30 c, кнопки mute/камера/отключить работают,
сессия завершается, строка исчезает.

### A4. Двусторонние аудио/видео: доказательство, а не «счётчики растут»

**Статус:** 🟢 (media-level proof 2026-10-20; визуальная проверка на `.132` — за оператором)
**Цель:** подтвердить, что `.132` слышит комнату и видит видео, а комната слышит/видит `.132`;
для SIP и H.323 отдельно.

**Факты прогона:**
- SIP: PCMU/H264; за 12 c `audio_rx=422`, `audio_tx=175`, `video_rx=99`, `video_tx=196`,
  `decoded_video_frames=54`, `encoded_video_frames=59`;
- H.323: PCMU, RX PT 109 / TX PT 96; за 20 c `decoded_video_frames=150`, `encoded=169`,
  `video_rx.resolution=640x360`; вызов жив >12 c;
- `active:false` при растущих packets — это VAD, а не отсутствие потока.

**Подводные камни:**
- `active:false`/`present:true` в `engine/abonents` может означать «поток есть, но VAD не активен»;
  проверять `packets`, `bitrate`, `power_sB`, `quality.downlink`, а не только флаг;
- SIP SDP: `sendrecv`, корректные `m=audio/video`, PT; `build_plain_sdp` должен включать H264 96
  и telephone-event 101; проверить, что `.132` принял video;
- G.711 8k ↔ 48k: ресемплинг не должен давать тишину/артефакты; проверить PCM-уровень;
- MCU mix-minus: участник не должен слышать себя; AGC 1/√N; tanh soft clip;
- H.264: FFmpeg baseline, keyframe при старте, `resize_i420`; при отсутствии IDR — чёрный экран;
- RTP-порты 41000–41200: lease, RTCP, symetric NAT; не переиспользовать порт до освобождения;
- firewall/iptables на `.93`; `.132` должен иметь маршрут к RTP-портам;
- для H.323 проверить оба направления PT (109 in / 96 out) и H.245 master/slave.

**Приёмка:** capture/pcap или media-отчёт: аудио RX/TX > 0 в обе стороны, видео RX/TX > 0,
`power_sB` меняется при речи, на `.132` видно видео комнаты; для SIP и H.323 — отдельные записи.

### A5. Управление участником и чистое завершение

**Статус:** 🟢 (2026-10-20; H.323 DTMF остаётся ограничением — 501)
**Цель:** mute микрофона/камеры, «не слышит/не видит комнату», kick, DTMF, громкость, завершение
сессии работают и не оставляют зомби.

**Факты прогона:** mute mic (`microphone`, outgoing=false) → `audio.rx.enabled=false`; mute
conference audio (outgoing=true) → `audio.tx.enabled=false`; restore → true; SIP `DTMF 123#` → 200;
`DELETE /sessions/{id}` → 204, roster `count=0`, participant/attempts/ports очищены.
H.323 DTMF сейчас не реализован (H.245 userInputIndication) — API должен отдавать явный 501,
а не 500.

**Подводные камни:**
- направления: `audio {outgoing:false}` — то, что участник **шлёт** (микрофон);
  `{outgoing:true}` — то, что он **получает** (динамик); для video аналогично;
- `DELETE /api/v3/abonents/{connection_id}` vs `DELETE /sessions/{id}/abonents/{abonent_id}`;
- после kick должен уйти `ParticipantLeft reason=2`, WS close 4003, RTP-порт освобождён;
- `DELETE /sessions/{id}` должен снять все соединения и остановить запись/трансляцию;
- `stopped_sessions`/`in_flight_calls`/`PendingOutboundGuard` не должны удерживать запись;
- BYE/CANCEL: если вызов ещё `connecting`, нужен CANCEL, а не BYE;
- H.323: Release Complete и `endSessionCommand`, затем закрыть H.245/TCP;
- повторный `DELETE` должен быть идемпотентным (204/404 по контракту).

**Приёмка:** после каждого действия состояние в API/движке совпадает; после завершения нет
активных `connection_id`, портов, задач; повторный вызов того же адреса работает.

### A6. Гигиена тестовых артефактов и конфигов

**Статус:** 🟢 (2026-10-20; критичный код добавлен в индекс, `.accept/`/`.accept_*`/`.h323_*`/
`__pycache__/` в `.gitignore`, `git status` содержит только осмысленные изменения)
**Цель:** убрать мусор, который мешает запуску и ревью: `.accept_*`, `.h323_*`, `__pycache__`,
логи, временные БД, архивы; зафиксировать `.gitignore`.

**Важно:** до `git clean` добавить в индекс критичный untracked-код: `crates/vks-server/src/gateway_audio.rs`,
`gateway_video.rs`, `crates/vks-gateway-h323/python/` (кроме `__pycache__`), `crates/vks-gateway-sip/tests/`,
`crates/vks-admin/src/api_v3/resources.rs`, `call_lifecycle_tests.rs`, `resource_tests.rs`,
`migrations/0005_configuration_documents.sql`. Иначе `git clean -fd` уничтожит реализацию.

**Подводные камни:**
- `crates/vks-gateway-h323/python/__pycache__` — удалить, но не удалять `h323_asn.py`,
  `schemas/*.asn`, `requirements.lock`, `PROVENANCE.md`;
- `vks_data.db`, `vks-server.log`, `vks_server.log` — не коммитить; добавить в `.gitignore`;
- `client/pnm-frontend/archive/*.zip` (26 MB) — не тащить в git; проверить, что уже ignored;
- `.accept_token` содержит JWT — удалить, не логировать;
- `.h323_*.py` — временные копии, удалить;
- не удалять пользовательскую БД/данные без подтверждения; тестовые — только workspace.

**Приёмка:** `git status` содержит только осмысленные изменения; `git check-ignore` подтверждает
игнор; сборка/тесты не зависят от мусора.

## P1 — архитектура и медиа-контур

### B1. Движки SFU/MCU в `AppState`, per-room режим, тики

**Статус:** ⚪ (было T2.1–T2.3)
**Цель:** UI v3 переключает режим комнаты, `MediaEngine` реально используется, метрики и тики
идут из composition root, а не из тестов.

**Подводные камни:**
- сейчас `ModeSwitcher` используется только в `network_integration.rs`; в рантайме его нет;
- `HandlerConfig.room_mode` фиксируется на старте из глобального `effective_mode`; per-room
  override не доходит до signaling;
- две реализации (`SfuEngine`, `McuEngine`) могут дублировать комнаты/тики; один источник истины;
- `spawn` тиков: не блокировать async runtime, backpressure, отмена при удалении комнаты;
- метрики pull не должны брать lock на горячем пути;
- при переключении режима отправить `RoomModeChanged` всем, включая late joiner;
- не сломать SFU-путь, который уже работает в браузере.

**Приёмка:** `POST /api/v3/ui/video_layouts/mode` меняет режим; `GET /api/v3/engine/...` отдаёт
метрики; тест на late joiner; нет утечки тиков после закрытия комнаты.

### B2. Доставка MCU-композита WebRTC-клиентам

**Статус:** ⚪
**Цель:** в MCU-режиме браузер получает единый композит, а не отдельные потоки; SFU-режим
продолжает работать.

**Подводные камни:**
- сейчас MCU-композит потребляет только gateway video; WebRTC-клиенты не получают composite;
- браузер не принимает I420; нужен H.264/VP8 энкодер (FFmpeg или Rust), keyframe, тайминг 30 fps;
- CPU: 1920×1080@30 на комнату; ограничить, если нет аппаратного кодека;
- SSRC/PT/RTCP должны совпадать с SDP; не отдавать один и тот же SSRC разным клиентам;
- backpressure: медленный клиент не должен тормозить композитор;
- прозрачность: API/клиент не должны знать режим; `MediaEngine` — единственная точка.

**Приёмка:** браузер в MCU-комнате видит сетку; SFU-комната не деградировала; CPU/латентность
в бюджете; тест на переключение.

### B3. Единый lifecycle сессии/ростера (in-memory + storage)

**Статус:** ⚪ (было T2.6)
**Цель:** participant не исчезает через 1 c; `RoomHub`, `Rooms`, `in_flight_calls`, `stopped_sessions`
согласованы; рестарт не оставляет фантомов.

**Подводные камни:**
- `RoomHub::unregister_participant` удаляет комнату при `remaining==0 && no receivers`; гонка с
  повторной регистрацией;
- `cleanup_active_membership` может сработать дважды (WS close + timeout);
- `PendingOutboundGuard`/`InFlightCallGuard` должны освобождаться на всех путях (drop, cancel, error);
- `connection_id` vs `participant_id` vs `call_id`: не перепутать ключи;
- storage-sync асинхронный; не держать lock через `.await`;
- при рестарте `RoomHub` пуст, а `sessions` в БД активны — нужен reconcile или остановка.

**Приёмка:** тест «join → 1 c → leave» не удаляет комнату с другими; параллельные join/leave;
рестарт; 100 циклов без утечек.

### B4. WS-события и UI-обновления

**Статус:** ⚪
**Цель:** UI получает `sessions:joined/left/updated`, `abonents:*` без 5-секундного поллинга;
события совпадают по имени/форме с фронтом.

**Подводные камни:**
- фронт 2.2.1.66 не подписан на `sessions:joined/left`; нужно либо добавить подписку, либо
  оставить polling и не ломать его;
- WS auth — `access_token` в кадре, не заголовок; PING/PONG;
- `admin_events` broadcast может терять события при lag; ring buffer;
- не отправлять события до регистрации participant;
- payload должен быть JSON-совместим с DTO; не слать Rust enum как строку без serde.

**Приёмка:** два браузера видят join/leave <1 c; нет дублей; reconnect.

### B5. Конфиг: один источник истины, per-room mode, секреты

**Статус:** ⚪ (было T3.3)
**Цель:** `listen_addr`, `media_listen_addr`, `candidate_addr`, `frontend_dir`, `room_mode` не
дублируются; TOML читается полностью; `VKS_CONFIG_PATH` работает.

**Подводные камни:**
- сейчас 4 источника `listen_addr`; env/TOML/CLI/default расходятся;
- `candidate_addr` должен быть реальным IP `.93`, иначе ICE/SDP врёт;
- `frontend_dir` относительный — зависит от cwd; использовать абсолютный или `CARGO_MANIFEST_DIR`;
- `jwt_secret` dev-значение; не логировать; для прода — RS256 (T3.1);
- `database_url` SQLite `?mode=rwc`; для in-memory `cache=shared`;
- `server_name`/`default_room_mode` должны попадать в `AppState` и `HandlerConfig`;
- не хранить секреты в `configs/default.toml` для прода.

**Приёмка:** `Config::validate`, тест «TOML → AppState», запуск из другого cwd; нет дублей.

### B6. Надёжность gateway-медиа: порты, RTCP, DTMF, H.264

**Статус:** ⚪ (было T2.5)
**Цель:** RTP-бридж не течёт, порты освобождаются, RTCP/DTMF работают, видео не чёрное.

**Подводные камни:**
- `RtpPortLease` удерживает порт до drop; при ошибке dial — освободить;
- RTCP SR/RR и PLI; без PLI — чёрный экран;
- DTMF RFC 4733: PT 101, sequence/timestamp; не путать с in-band;
- ресемплинг 48↔8: целочисленные ошибки, дрейф;
- H.264 FFmpeg: профиль baseline, GOP, bitrate; `-fflags nobuffer`; не блокировать async;
- mix-minus: не отправлять участнику его же аудио;
- при `connected` запускать media loop только после ACK; при disconnect остановить.

**Приёмка:** 100 циклов dial/hangup без утечки портов; pcap с RTP/RTCP; DTMF слышен; видео
на `.132` не чёрное.

## P2 — качество, безопасность, инфраструктура

### C1. Auth/RBAC: RS256, `iss`, permissions sync

**Статус:** ⚪ (P2, ex-REWORK C1)
**Подводные камни:** HS256 dev-секрет; `iss` опционален; `getIsProfileHasPermissions` TODO;
не сломать v1 RBAC; refresh-токена нет.
**Приёмка:** тесты JWT (alg none/HS256/RS256, iss), 403 на UI, профиль `permissions`.

### C2. Тесты/CI: unit, integration, flaky, clippy

**Статус:** ⚪ (T4.1–T4.2)
**Подводные камни:** 527 тестов в PROGRESS не соответствуют текущему дереву; сетевые тесты
требуют `.132`; sidecar-тесты зависят от venv; `cargo test --workspace` может быть долгим.
**Приёмка:** CI зелёный; отдельные job для network/acceptance (skip без env).

### C3. Документация и трекер

**Статус:** ⚪ (T4.3–T4.4)
**Подводные камни:** архивный PROGRESS заявлял завершённость фаз, которой не было; REWORK_PLAN устарел;
не коммитить ложные «527 tests».
**Приёмка:** README/ARCHITECTURE/MASTER_TASK_LIST соответствуют коду; acceptance checklist.

### C4. Заглушки кластера/рекордера/TURN/NATS

**Статус:** ⏸️ (T3.4)
**Подводные камни:** in-memory NATS simulation; recorder metadata only; TURN не реальный.
**Приёмка:** либо реальный адаптер, либо явная пометка «not for production» и fail-fast.

## Порядок исполнения

1. A0 (triage) — исключить старый бинарник/процесс; уже сделано.
2. A1a (SIP) → A4 (SIP media) → A3 (UI API) → A5 (controls/stop) — основной P0-путь.
3. A6 (git add критичного кода → чистка) — до коммита.
4. A1b/A2 (H.323) — отдельная ветка; SIP уже зелёный.
5. B3 (единый lifecycle/identity) → B1 (per-room mode) → B6 (порты/RTCP/DTMF).
6. B2 (композит в WebRTC) — **не блокирует** приёмку PNM: UI админский, не медиа-клиент.
7. B4 → B5 → C1 (authz-scope/RS256) → C2 → C3 → C4.

## Ревью-журнал

| Задача | Раунд | Ревьюер | Оценка | Вердикт/правки |
|---|---|---|---|---|
| A1a SIP happy path | 1 | lifecycle reviewer | 7.5 | Найден 5-секундный watchdog, убивавший медленный первый RTP/удержание; видео-сбой рвал аудио; утечка `abonent_settings`; hot-spin на recv error. |
| A1a/A5 lifecycle | 2 | lifecycle reviewer | **9.5** | Watchdog 15 c + `connected_at` перед announce; видео-сбой → audio-only, без phantom leave; очистка settings; bounded recv errors; добавлены 2 теста предиката. Ship. |
| A1b/A2 H.323 | 1 | H.323 reviewer | 7.0 | `resolve_helper()` без тестов и без диагностики; cwd-hijack; нет video-grace; reject чужого канала; PT до валидации. |
| A1b/A2 H.323 | 2 | H.323 reviewer | **9.5** | Pure resolver + 3 теста, fail-closed override, `info!` пути, cwd только debug; 2.5 c video-grace, reject только своего канала, PT до assign; 19 Python tests, live RTP. Ship. |
| A3 UI API | 1 | acceptance reviewer | 5.0 | PNM UI — админская control plane, не WebRTC-медиа-клиент; визуальный E2E в sandbox недоступен, API/asset-проверка. |
| A4 SIP/H.323 media | 1 | acceptance reviewer | 6.0 | Счётчики росли, но не было доказательства двустороннего decoded/encoded; добавлены live-замеры PT/разрешения. |
| A6 task list | 1 | task-list reviewer | 7.5 | Список раздут/устарел; сокращён, статусы A0–A6 приведены к фактам. |
| A6 hygiene | 1 | elegance reviewer | 6.0 | CRLF в shell/lib раздувал diff; нормализованы, `.accept*`/`__pycache__` в ignore. |

## План 2026-10-20 (утверждён): рабочий фронтенд, SIP/H.323, раскладки, чистка

Правило цикла — как выше: «подводные камни» → реализация → параллельное ревью
(закрытие при ≥9.5 хотя бы у одного) → агент элегантности/приёмки (≥9) → commit.
Границы: UI не переписываем; bundle не патчим; запись/трансляция/терминалы/кластер —
честный read-only или явный отказ; эталон `.92` — только GET.

| ID | Задача | Статус | Коммит/артефакт |
|---|---|---|---|
| T0.1 | Единый рантайм: один release-процесс на :8080, runbook build→restart | 🟢 | `38f9a54`, `scripts/run-local.sh` |
| T1.1 | Калибровка: `{"calibration":null}`, скрыть пункт меню/плитку, deep-link 308 → статистика | 🟢 | см. `git log --grep T1.1` |
| T1.2 | DTO конференции/абонента: `addresses`, null-паритет, `details.user_agent/enabled`, engine-фильтр | 🟢 | см. `git log --grep T1.2` |
| T1.3 | DTO абонента/сессии: `engineAbonent.statuses`, `connection.protocol`, `contact`, `layoutId` | 🟢 | наследование+«Общая» раскладка при старте (без orphan), `video_layout_id` (individual→default), полный `payload` (ref-92), гибрид `abonents`, `abonents` раскладки в форме `my→Mee`; приёмка UI — `docs/progress/evidence/acceptance/summary.json` |
| T1.4 | Раскладки: `.../video_layouts/{id}/abonents`, `calculate`, `settings/video`, attach в любой слот | 🟢 | abonents/attach/detach: получатели по `type`, занятие свободных окон (422/409/404), detach без reindex, purge при удалении комнаты, calculate, templates, settings/video GET+PATCH (RBAC+аудит+durable), mode→запись с откатом; UI-сценарий «Настроить раскладку → Ручная → Заполнить → Создать» зелёный; live RTP H.323/H264 — `evidence/acceptance/` |
| T1.5 | «Добавить по адресу»: autocomplete, адресная книга, `/ui/sessions/{id}/audiochannels` | 🟢 | vendor не имеет `/pnm/addresses` и `/ui/addresses` — вместо них глобальная книга: `GET/POST/PATCH/DELETE /api/v3/call_strings[/{id}]` (+`/ui`), фильтры/пагинация/сортировка, WS `ui:call_strings:search`, автодополнение по `uri/protocols`; аудиоканалы: list с `page/page_size/search/sort`, счётчики speakers/listeners, `/ui`-алиасы; evidence `docs/progress/evidence/t15/` |
| T1.6 | Приёмка SIP+H.323 из UI + `scripts/smoke_v3.sh` + evidence | 🟢 | evidence `docs/progress/evidence/t16/`; acceptance-gate `ok=true` (connected, рост RTP, 3 контрола off/restored, раскладка 4/поз.1, reload, kick, 14 уникальных шагов, 16 PNG); smoke 24/31/31 |
| T1.7 | Тест boot-reconcile `stopped_sessions` (логика уже есть) | 🟢 | тест: persisted-сессии STOPPED после старта, start → ACTIVE, рестарт → снова STOPPED |
| T2.1 | Справочник: groups, users CRUD, roles, credentials, addresses | 🟢 | `bdef85c`+`4f56a03`+`95ef73c`+`b06bc18`+`4597e35`+`99062cb` (код+тесты: 31 directory-тест / 178 vks-admin / 34 vks-auth, clippy 0; cross-user credentials alias закрыт), evidence-коммиты `268f24d`+`8a3516c`+финальный (ok=true, 20 acceptance, 15 шагов/9 PNG, expected 404=2 только созданных пользователей, clean_build+binary_after_head); см. `docs/progress/evidence/t21/` |
| T2.2 | История: call_history (+ui, export, clean), session_history | 🟢 | `654b964`+`c078821` (код: вынесены `history/export.rs`, `export_store` на `bytes::Bytes`, `zip_store`; `HistoryFilter`+`ExportQuery`/`PageSort`; durable `history_recorded_at` + сброс при повторном старте; чистый URI входящего; newest-first по умолчанию, экспорт уважает `sort_order`; 23 history-теста + 3 export_store, всего 230 vks-admin/35 vks-server; clippy/fmt 0); evidence `docs/progress/evidence/t22/`: ok=true, 23/23 acceptance, 13 шагов/3 PNG, 0 HTTP/console, clean_build+binary_after_head (HEAD aa9a1ec), UI list/detail/export, CSV-время в Europe/Moscow полное (не только дата), ZIP через общий zip_store |
| T2.3 | Администрирование: settings/calls/{sip,h323}, video, rtp, http, styles, smtp, security, time/tz/ntp | 🟢 | `654b964`+`c078821` (код: vendor-UI контракты NTP flat/time-string/stop_rules-null, атомарный calls PATCH + merge частичных legacy-объектов (unknown/type → 422), `hot_reload_from_storage` не гасит живые сессии (B7), GET restart → 405, reload failure → 500, честный ntp_synchronized, структурная PEM/DER-проверка, room-scope forbid; 13 settings-тестов, всего 230 vks-admin; clippy/fmt 0); evidence `docs/progress/evidence/t23/`: ok=true, **35/35** acceptance, 13 шагов/4 PNG, 0 HTTP/console, clean_build+binary_after_head (HEAD aa9a1ec), реальные UI-save: NTP (плоский PATCH + частота + reload), conference_settings one_abonent, SIP/h323/кодеки (≥3 PATCH все 200), restart_services сохраняет ACTIVE-сессию |
| T2.4 | Наблюдаемость/сервис: stats (/proc), network, log_events, roles, license, videorecords, terminals read-only; destructive → явный отказ | 🟢 | `9c5ac55`+`c078821` (код: log_events по 8 реальным vendor-шаблонам, ZIP/CSV в локальной TZ + invoker + 128-битный static-токен, сеть/статус `{editable:true}` (список рендерится)/session-алиасы, ping/traceroute без интерполяции в shell (503 если нет бинаря, `-` отвергается), tcpdump/терминалы/видео read-only 501, stats disk MB/network kbps/engine-log стабильный timestamp, license PERMANENT+params, features-массив, RBAC Operator+; 13 observability-тестов (всего 231 vks-admin); clippy/fmt 0); evidence `docs/progress/evidence/t24/`: ok=true, **38/38** acceptance, 10 шагов/6 PNG, 0 непреднамеренных HTTP/console, 10 страниц; вкладка «Сетевые интерфейсы» реально рендерит ens18, сохранение показывает видимый тост с 501, vendor Chart.js warnings allowlisted (12), clean_build+binary_after_head (HEAD aa9a1ec) |
| T3.1 | Мёртвый код/зависимости (/records→/videorecords, неиспользуемые модули/крейты) | 🟢 | `1405f8c`: удалены `/api/v3/records`, `/ui/records` и мёртвый `ui_records`; неиспользуемые direct-зависимости (`vks-layout` в server, `vks-core` в gateways) убраны; restart-роуты POST-only; layout-mode контракт починен в integration-тесте (create/attach layout + `video_layout_id`); evidence `docs/progress/evidence/t31/` — ok=true, 7/7 гейтов (cargo check workspace + 231 admin lib + integration; fmt/clippy/workspace tests проверены вручную) |
| T3.2 | Доки/конфиг/git: один трекер, README, dev/prod, CI master, .gitignore, .cargo | 🟢 | один живой трекер `MASTER_TASK_LIST.md`, `PROGRESS/REWORK_PLAN/FRONTEND_PORT_PLAN/TEMPLATE_ERROR` → `docs/progress/archive/` (+README), ссылки в PROJECT_RULES/CODE_STYLE/ARCHITECTURE/ADR-0027 обновлены; корневой README переписан (dev/prod, 893 теста, честный статус); `configs/prod.example.toml` + пометка dev-only в `configs/default.toml`; CI `push.branches: [master]`; `.gitignore` (+pid/db-journal); профили Cargo только в корневом `Cargo.toml` (дубли из `.cargo/config.toml` убраны) |
| T3.3 | Артефакты + финальный обход страниц + таблица паритета | 🟢 | evidence `docs/progress/evidence/t33/`: ok=true, 5/5 гейтов (22 страницы, 18 API-контрактов, 7 PNG, 0 HTTP и 0 собственных console, 24 vendor Chart.js allowlisted), таблица паритета UI↔API; финальная приёмка `docs/progress/evidence/t16/`: живой H.323 и SIP на `192.168.1.132` (10/10 гейтов: H.323 и SIP, 16 PNG, 0 HTTP, 12 vendor TypeError), smoke 24/31/31 при HEAD `aa9a1ec`; полный workspace: 893 теста, clippy/fmt 0 |

Приёмка (AC1–AC9) — в утверждённом плане; ключевые: один процесс; SIP и H.323
до живого участника с ростом медиа-счётчиков; участник ставится в любой слот и
переживает reload; 0 console errors и 0 необъявленных 501 на сценарии; калибровки
нет в меню; `smoke_v3.sh` зелёный; CI на master.

### Pre-existing дефект тестов (закрыт в T3.1)

`test_package_d_mcu_pipeline_handover_and_pnm_control_events` падал `422` на
`POST /api/v3/ui/video_layouts/mode` из-за отсутствия раскладки у сессии:
тест теперь создаёт/прикрепляет layout, передаёт `video_layout_id` и проверяет
`mode:"auto"`/`topology:"mcu"`; passes на `1405f8c`.

## Вопросы к команде

Открытых вопросов нет.
>
> **T11a (честный system status) закрыт:** `8a75350`, ревью **9.5/10 APPROVED**
> (`stats-deps`), 1041 passed / 0 failed / 6 ignored, Python 24/24, clippy/fmt.
> Реальный IP 192.168.1.93, `database.size_bytes` = main+WAL, SIP `registration`
> от UAS, H.323 `READY` только по факту H.225 setup или реального Q.931-ответа
> (до вызова — `DISABLED`), структурная классификация terminal/transport без
> подстрочных эвристик, TTL-самолечения нет. Evidence `docs/progress/evidence/t11a/`.
>
> **T04b (полная матрица раскладок) закрыт по live-матрице:** E2E 201 checks /
> 0 failed / 145.3 s, page_errors 0; все 11 диапазонов 1/2/3/4/5+/8/9/10+/25/34+
> + icon-only overlap (9 схем), участник в 1/ceil(N/2)/N от фактического числа
> ячеек (5+→6, 10+→16, 34+→36), overlap-схема с 11 ячейками, `calculate` для
> 14 пресетов. Evidence `docs/progress/evidence/t04b/`.
