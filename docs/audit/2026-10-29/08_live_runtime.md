# 08. Живой рантайм: снимок, аномалии, provenance

- **Агент:** `live-runtime` (независимая проверка фактов).
- **Время снимка:** 2026-10-29 06:55–07:15 MSK (Europe/Moscow).
- **Режим:** только чтение. SQLite — `mode=ro`; HTTP — только GET; сервис **не перезапускался**, БД SQL-запросами не менялась; создан только этот файл (плюс временные файлы в `/tmp`).
- **Стенд:** `vks-mcu.service` (systemd --user), PID 1814143, `:8080`; Python H.323 sidecar PID 1814221, `:1720`.
- **HEAD:** `d82ce4d` (2026-10-29 00:04:10 +0300), дерево чистое; `rustc 1.94.0 (4a4ef493e 2026-03-02)`.

---

## §1. Снимок рантайма и provenance-вердикт

### 1.1. Факты

| Параметр | Значение |
|---|---|
| Unit | `vks-mcu.service`, active since **2026-10-28 18:00:39 MSK**, `NRestarts=0` |
| Main PID | **1814143** (`/home/pro/mcu/target/release/vks-server --config /home/pro/mcu/configs/default.toml`) |
| Uptime | ~13 ч (12 ч 56 мин на 06:55) |
| `/proc/1814143/exe` | `-> /home/pro/Рабочий стол/mcu/target/release/vks-server **(deleted)**` |
| Инод/fразмер живого exe | `1474242`, **27 207 280 B**, mtime **2026-10-28 18:00:21.893** |
| sha256 живого exe | **`4dcbff7706dae580f1c265708691110b377042341001b6a1d3093d445a404db0`** |
| Бинарь на диске | inode `1475918`, **27 274 320 B**, mtime **2026-10-28 23:53:20.127** |
| sha256 диска | **`cbad4986b607f5caf37329114f7b4d521145dd63ce1c370d9cf1f3287c3fa93f`** |
| rustc в обоих exe (`.comment`) | `1.94.0 (4a4ef493e 2026-03-02)`, LLD 21.1.8 |
| CWD/PPID | `/home/pro/Рабочий стол/mcu` (`/home/pro/mcu` — симлинк), PPID 2294 (systemd --user) |
| Память (systemd) | **611.3M**, peak **669.5M**; cgroup `memory.current=640 761 856`, `memory.peak=702 115 840` |
| CPU | 45 min 40 s за ~12 ч 56 мин (в среднем ~5.9 %); мгновенно в простое ~0.8 % |
| Потоки/FD | **38** потоков, **21** FD, лимит `NOFILE=65536` |
| Sidecar | PID 1814221, Python **3.14.4**, 28 292 kB RSS, 1 поток, 5 FD, TCP `0.0.0.0:1720`, started 18:00:40 |

Ключевые env процесса (`/proc/1814143/environ`):

```
VKS_CANDIDATE_ADDR=192.168.1.93
VKS_SIP_UAS_ENABLED=1
VKS_H323_UAS_ENABLED=1
VKS_H323_UAS_ALLOW=127.0.0.1/32,192.168.1.0/24
VKS_H323_UAS_MAX_CALLS=20
VKS_H323_UAS_MAX_PER_IP=20
VKS_WEBRTC_TRANSPORT=1
VKS_WEBRTC_PORT_MIN=43000
VKS_WEBRTC_PORT_MAX=43100
```

`VKS_JWT_SECRET` и `VKS_METRICS_TOKEN` в окружении **отсутствуют** (JWT берётся из `configs/default.toml`).

### 1.2. Что запущено на самом деле

**Живой процесс — это удалённый (deleted) инод, собранный в 18:00:21 и запущенный в 18:00:39.** С тех пор рестартов не было (`NRestarts=0`, `ActiveEnterTimestamp=ExecMainStartTimestamp=18:00:39`, в journal только «Stopped/Started» в 18:00:39).

Содержимое живого бинаря — рабочее дерево, закоммиченное как **`645120d`** (18:05:17), т.е. снимок за ~5 минут до коммита; точный коммит недоказуем, но по составу это `645120d`, а не HEAD:

- **есть** role-фикс и auto-expand раскладки из `645120d`:
  - live `/api/v3/ui/video_layouts/presets` отдаёт 54 Codian-пресета, **507 слотов**, роли `AUTO` (486) / `PRESENTATION` (21);
  - в живом exe присутствует строка `auto-expanding the layout` (HEAD `engine.rs:678`).
- **нет** более поздних изменений:
  - нет строк `metrics are available from loopback only` / `metrics token required` → live `/metrics` открыт из LAN (§4);
  - анонимные `/api/v3/gateways/status|mcu/farm|cluster/status` → 200, хотя HEAD требует Moderator (`b73c188`, 18:37);
  - нет строк `payload_type_mismatch`/`source_changed`/`ssrc_changed` → live-лог ещё печатает старое `codec/PT mismatch` (`8116855`, 22:29);
  - нет `x-vks-layout-applied` / `vks_layout_apply_total` → S8 (`8d44b9c`) не в живом бинаре.
- **после `645120d` до HEAD — 45 коммитов.** Живой бинарь отстаёт на весь S6/S7/S8-раунд, gateway RTP-reason fix, e2e-фиксы, metrics/ops guards.

**Бинарь на диске (`cbad4986`, 23:53:20) — свежий release**, собран после `7c242e1` (23:49:14) и до `d82ce4d` (00:04:10). `d82ce4d` меняет в коде только комментарий в `crates/vks-layout/src/grids.rs` (проверено `git show`), т.е. дисковый бинарь **функционально соответствует HEAD**. Он **не запущен**.

### 1.3. Вердикт

1. **«Сервис на новом бинаре» — неверно для текущего процесса.** Живой sha `4dcbff77…` ≠ дисковый `cbad4986…`; exe удалён; сборка 18:00, запуск 18:00.
2. **Живой бинарь не привязан к коммиту**: он собран из рабочего дерева за 5 минут до `645120d`; по составу соответствует `645120d` и на 45 коммитов старше HEAD (точный коммит-источник недоказуем).
3. **Дисковый release `cbad4986` — корректный кандидат на рестарт**, он новее живого и функционально = HEAD.
4. Документ `docs/audit/2026-10-28/00_MASTER_PLAN.md:423` («сервис перезапущен на … `94484d42…`») и `docs/audit/2026-10-28-fix/03_hygiene.md:201` («живой бинарь `94484d42…`, PID 1418719») **устарели/ложны**. Более поздний `docs/audit/2026-10-28-fix/00_plan_review.md:480` честно фиксирует: PID 1814143 не перезапускался, release `cbad4986` проверен на временном инстансе `:18082`.
5. Следствие: **любые выводы о поведении HEAD по live-логу после 18:00 недействительны**; проверять S7/S8/metrics/ops можно только на новом бинаре (временный инстанс или согласованный restart).

---

## §2. Логи: топ аномалий

Файл: `vks_server.log` (1 265 101 B, append, без ротации; mtime 06:47:42). Внутри — **33 запуска** с 2026-10-23; текущий запуск начинается строкой 4500 (`2026-10-28T15:00:39Z` = 18:00:39 MSK). ANSI-коды сняты в `/tmp/vks_clean.log`; статистика ниже по нему.

| Метрика | Значение |
|---|---|
| Строк всего | **4 820** |
| INFO / WARN / ERROR | **2 742 / 1 226 / 0** |
| panic / `RUST_BACKTRACE` | **0 / 0** |
| «starting up» | 33 |
| неструктурированных строк (не начинаются с `2026-`) | **852 (17.7 %)** — многострочный ffmpeg stderr |
| `Last message repeated N times` | 147 |
| WARN/ERROR в текущем запуске | **17** (0 ERROR) |

### 2.1. Топ по частоте (весь лог / текущий запуск)

| # | Аномалия | Всего | Сейчас | Источник (live → HEAD) | Оценка |
|---|---|---|---|---|---|
| 1 | `audio RTP rejected by source validator (codec/PT mismatch)` | **616** (все 2026-10-25) | 0 | `gateway_audio.rs:288` → HEAD `:306` | Историческое; в live-бинаре ещё старый текст. По 616 событиям: `expected/received` = 0/0 — 223, 9/0 — 240, 9/9 — 153, т.е. в 376 случаях PT не расходится — причина в сообщении неверна (это source/SSRC/malformed). Fixed `8116855`, но **не в живом бинаре** |
| 2 | `FFmpeg codec diagnostic` (H.264 decode) | **477** WARN (475 — 25.10, 2 — сейчас) | 2 | `gateway_video.rs:419` | 39 % всех WARN — шум; топ: `non-existing PPS 0 referenced` 277, `missing picture in access unit` 31, `no frame!` 27, `Invalid data` 13 |
| 3 | `api_v3: unmatched route → 501` | **40** (13 — `POST …/previews/…`) | **7** (все previews) | `api_v3/mod.rs:622` | UI шлёт preview-запрос, сервер отвечает 501; в HEAD маршрута тоже нет. Плюс старые пути `/ui/presets`, `/api/v3/health`, `/auth/login`, `/system/info` |
| 4 | `encoder group failed: stdin closed` | **35** | **5** | `farm.rs:793` | WARN на teardown/отказе ffmpeg; 4 из 5 — при завершении звонков, не авария, но засоряет лог |
| 5 | `SECURITY: allow_insecure_defaults=true …` | **33** (по одному на старт) | **1** | `main.rs:213/214` | Прямое признание небезопасного режима; см. §6 |
| 6 | `layout has no free geometry slot; participant stays unassigned` | **8** (все 25.10 14:27) | 0 | `engine.rs:567` (live) → HEAD `:678` `auto-expanding` | Историческое: в живом exe уже есть auto-expand; на текущем бинаре не воспроизводилось (нет переполненных раскладок) |
| 7 | `WebRTC candidate IP defaulted to 127.0.0.1` | 3 (старые запуски) | 0 | `main.rs:310` | Сейчас candidate задан env `192.168.1.93`, preflight OK |
| 8 | `outbound dial failed` (H.323) | **3** | 0 | `call_strings.rs:396` | 2 интеропа: `No common symmetric H.323 audio codec`, `dataTypeALCombinationNotSupported`; 3-й — `remote_release:cause=21` |
| 9 | `api_v3 ws: active token expired` | 4 | 0 | `ws.rs:124` (live) → HEAD `:174` | Штатное закрытие WS по TTL |
| 10 | `encoder group orphaned; actor self-terminating` | **1** (28.10 15:02:49) | 1 | `farm.rs:780` | Гонка insert/retire; само-лечение сработало, ffmpeg не остался |

### 2.2. Текущий запуск (17 WARN), точный состав

- 7 × `unmatched route → 501` (все — `POST /api/v3/sessions/{id}/previews/{pid}`);
- 5 × `encoder group failed: stdin closed`;
- 1 × `encoder group orphaned`;
- 2 × `FFmpeg codec diagnostic` (H.264: `missing picture`, `no frame!`);
- 1 × `SECURITY: allow_insecure_defaults=true`;
- 1 × `restored layout type=default from COW alias evidence` (`resources.rs:746`).

ERROR/panic за текущий запуск — **нет**. Утечки ffmpeg-процессов на момент снимка — **нет** (0 процессов ffmpeg, 0 зомби).

### 2.3. Качество лога

- 852 строки (17.7 %) — неструктурированный многострочный вывод ffmpeg, попадающий в тот же файл; парсинг по timestamp/level ломается.
- Ротации нет: файл растёт в репозитории (`StandardOutput=append:%h/mcu/vks_server.log`).
- Секретов в логе нет: `eyJ…`, `Bearer …`, `password=` — 0 совпадений.

---

## §3. Процессы и ресурсы

| PID | Роль | RSS | Потоки | FD | Старт | Дети |
|---|---|---|---|---|---|---|
| 1814143 | `vks-server` | **613 964 kB** | **38** | **21** | 28.10 18:00:39 | sidecar 1814221 |
| 1814221 | H.323 sidecar (`python3 -u …/h323_listener.py`) | 28 292 kB | 1 | 5 | 28.10 18:00:40 | нет |

- **ffmpeg: 0 процессов** (при 0 активных encoder-групп это ожидаемо); зомби в системе — 0.
- **Сокеты vks-server:** TCP `0.0.0.0:8080` (LISTEN), UDP `0.0.0.0:5060` (SIP UAS), UDP `0.0.0.0:40000` (SFU media loop). WebRTC-диапазон 43000–43100 в простое не слушается (порты на peer); gateway RTP выделяется из пула **41000–41198** (код `RtpPortPool::default`, `crates/vks-gateway-sip/src/rtp_bridge.rs:20-25`) — по логу реально занимались `41000`, `41004`.
- **Sidecar:** TCP `0.0.0.0:1720` (LISTEN), Python 3.14.4 из `/home/pro/.local/share/vks-mcu/h323-venv`; 1 поток; входящий `connect` на 1720 успешен (read-only проверка).
- **Established HTTP:** 6 loopback-соединений (Firefox: 56844/56852/59400 + наши audit-запросы); внешних ESTAB нет.
- CPU в простое: 4 тика за 5 с = **0.8 %**; потоки преимущественно в `futex_do_wait` (ожидание), спиннинга нет.

### 3.1. Память — главный ресурсный риск

`smaps_rollup`: **Rss 613 964 kB, Anonymous 595 332 kB, Pss 611 123 kB**; `[heap]` — **579 MB Rss при 4.28 GB VSZ**. За 20 с простоя RSS не изменился ни на килобайт (613 964 → 613 964), т.е. это не активная утечка «на глазах», а **удержанная память**.

Сопоставление с историей (journal, предыдущий запуск):

| Запуск | Encoder-групп start/stop | Peak памяти | Heap сейчас/пик |
|---|---|---|---|
| Предыдущий (6 ч 16 мин, до 18:00:39) | **17 / 17** | **235 MB** (journal) | — |
| Текущий (12 ч 56 мин) | **66 / 66** | **669.5 MB** (systemd) / 702 MB cgroup | **579 MB heap** |

66 групп × ~9 MB ≈ 594 MB — совпадает с 595 MB anonymous почти один-в-один; 17 групп × ~10 MB ≈ 170 MB + база ≈ 235 MB. Гипотеза: **~9–10 MB Rust-heap удерживается на каждую завершённую encoder-группу** (буферы кадров/Arc-циклы/аллокатор), при этом ffmpeg-процессов уже нет. Это не доказанная утечка (аллокатор может не возвращать память ОС), но масштаб и корреляция требуют heap-профиля и soak-теста (§8).

---

## §4. Живые endpoint-проверки

Токены: JWT выпущен offline на dev-секрете из `configs/default.toml` (`HS256`, `iss=vks-mcu`, `sub=admin`, `generation=1`); это **read-only проверка auth**, не login. Все запросы — GET.

| Endpoint | Аноним | Viewer | Admin | Размер/тело |
|---|---|---|---|---|
| `/healthz` | **200** | 200 | 200 | 14 B `{"alive":true}` |
| `/readyz` | **200** | 200 | 200 | 54 B, все `true` (`cluster/db/gateway/media`) |
| `/metrics` | **200** (loopback и LAN) | — | — | 13 313 B, `text/plain; version=0.0.4` |
| `/api/v3/gateways/status` | **200** | **200** | 200 | 249 B: SIP/H.323 `READY`, `connected=true` |
| `/api/v3/mcu/farm` | **200** | **200** | 200 | 124 B: `groups:[]`, totals 0 |
| `/api/v3/cluster/status` | **200** | **200** | 200 | 117 B: `ACTIVE`, `alive_nodes:["vks-mcu"]` |
| `/api/v3/sessions` | **401** | **403** | **200** | 6 сессий |
| `/api/v3/profile` | 401 | 200 (viewer) | **200 (operator)** | forged admin token принят |
| `/pnm/` | **200** | 200 | 200 | 74 989 B, `text/html` |
| `/pnm/MANIFEST.sha256` | **200, text/html 74 989 B** | — | — | SPA-fallback отдаёт `index.html`, не манифест |
| `/api/v3/system/about` | **200** | — | — | версии UI/engine/server |
| `/openapi.json` | **200** | — | — | 473 B, стаб |
| `/api/v3/login/status` | **200** | — | — | `{"status":"enabled"}` |
| `/api/v1/health` | 404 | — | — | — |
| `/ws` (GET без upgrade) | 400 | — | — | — |

### 4.1. Ключевые находки

1. **Операционные endpoint’ы полностью анонимны.** `gateways/status`, `mcu/farm`, `cluster/status` отдают 200 без токена, с viewer-токеном и даже с **битым** `Authorization: Bearer bogus`. В HEAD на них стоит `require_moderator` + `token_middleware` (`crates/vks-server/src/lib.rs:157-188`) — живой бинарь этого не содержит (сборка 18:00 < `b73c188` 18:37).
2. **`/metrics` доступен из LAN.** Запрос с явным source `192.168.1.93` (подтверждено `bind`+`ss`: `192.168.1.93:59117 → 192.168.1.93:8080`) вернул **200** и 16 347 B. В HEAD `metrics_access` при пустом `VKS_METRICS_TOKEN` разрешает только loopback, иначе 403 (`lib.rs:231-258`). В живом exe строк loopback-guard нет.
3. **Dev-JWT подделывается.** Токен, подписанный опубликованным в репозитории `jwt_secret`, даёт `role=operator`, `permissions:["all"]`, доступ к `/api/v3/sessions` (200) и `/api/v3/profile` (200). `generation=1` совпала. Это не «взлом», а следствие `allow_insecure_defaults=true` + dev-секрета в конфиге; на `0.0.0.0:8080` это реальная граница без защиты.
4. **CSP корректна:** заголовок есть на `/pnm/`, статике, API и `/metrics`; 4 inline-скрипта `index.html` совпадают с 4 из 5 `sha256-…` в `script-src` (5-й хеш не совпал ни с одним inline-скриптом index — возможно, для динамического скрипта; поломкой не является). Также `x-content-type-options: nosniff`, `x-frame-options: SAMEORIGIN`, `referrer-policy: same-origin`.
5. **MANIFEST:** на диске `sha256sum -c ../../MANIFEST.sha256` из `client/pnm-frontend/deployed/pnm` → **214/214 OK** (exit 0). Но HTTP-путь `/pnm/MANIFEST.sha256` отдаёт `index.html` (SPA-fallback, 200 text/html), а наивный прогон `sha256sum -c MANIFEST.sha256` из `client/pnm-frontend` даёт 0 OK / 214 «No such file» — манифест лежит на уровень выше и использует пути `./…` относительно `deployed/pnm`.
6. Анонимно раскрываются версии (`/api/v3/system/about`), OpenAPI-стаб и `login/status`; это ниже по критичности, но в паре с LAN-доступом усиливает fingerprinting.

---

## §5. SQLite: снимок (read-only)

- Файл: `vks_data.db`, **3 248 128 B**; sha256 снимка на 07:12 — `3b901502bc9ea953b41d90e3ff1811c5720ca1e598704aea2a334d6145d86ee2`; mtime живой БД: 06:47:42 → 07:09:11 → **07:14:23** (сервис/UI пишут постоянно). Наши соединения — только `mode=ro`; новых audit-строк нет (max 06:47:42), counts не изменились, SQL-записей аудитор не выполнял.
- `journal_mode=delete`, **WAL/SHM отсутствуют**; `integrity_check=ok`, `quick_check=ok`, `foreign_key_check=[]`; `page_size=4096`, `page_count=793`, `freelist_count=313` (**~1.28 MB, 39.5 %** свободных страниц — фрагментация).
- Таблиц 14. Активные: `core_users` (4), `user_credentials` (2), `rooms` (6), `core_audit_log` (2 980), `configuration_documents` (1); legacy-пустышки: `users`, `layouts`, `api_keys`, `audit_log`, `server_settings`, `core_api_keys`, `core_layouts` (0 строк) — дублирование схем «v1/core».
- `_sqlx_migrations`: 5 успешных миграций (initial … configuration documents), все `success=1`.
- Пользователи: `admin` (operator), `Viewer Probe` (participant), `viewer_probe` (viewer), `уу` (participant); дублей username нет, orphan-credentials нет (2 credentials на 4 users — не у всех local-пароль).
- Комнаты: 6, дублей имён нет; **3 из 6 — «E2E Probe 1793…»** (созданы 28.10 18:59–19:06), плюс `Codian MCU 4200`, `Conf_003138425`, `Conf_087579889`.
- Аудит: 2 980 строк, max `2026-10-29 06:47:42`; `login_ok` 1 632, `session.started` **324**, `session.stopped` **313**, `conference.deleted` **322**, `login_fail` 19. **3 старта без stop/delete — ровно 3 E2E-probe**, у `07e67215…` после старта был outbound-инвайт (19:55) и `recall_list` (06:47), но `session.stopped` нет.
- `configuration_documents`: одна строка `pnm-definitions-v1`, JSON **1 283 386 B**. Внутри: 5 layouts (**у всех `slots=0`, `name=null`**), 9 `active_layouts` — для 3 комнат одновременно ключи `conferences:<id>:video_layouts` и `sessions:<id>:video_layouts`; 2 `layout_aliases`, 2 audiochannels, 13 links, 1 history, 4 directory. Секретов (password/secret/token/hash) в JSON — 0.
- Мусор/риски данных: 3 активные probe-сессии; 5 пустых layouts; дубли scope-ключей; 39.5 % freelist. Бэкапы: `backups/2026-10-28` — 30 MB.

**Вывод:** целостность БД в порядке, порчи нет; но live-данные загрязнены e2e-пробами и пустыми раскладками, а «активные» сессии — фактически утечка состояния, а не конференции (0 абонентов).

---

## §6. Конфиги vs факт

| Параметр | Заявлено | Факт | Расхождение/риск |
|---|---|---|---|
| `allow_insecure_defaults` | `true` (`configs/default.toml:4`) | принято, WARN `main.rs:213` | Dev-режим на `0.0.0.0` |
| `jwt_secret` | `dev_ec99…` (опубликован в git) | тем же секретом подписан admin-токен → 200 | **Подделка admin-JWT**; TTL 86 400 с (24 ч) |
| `admin_password` | `admin_password` | operator восстановлен из storage (`main.rs:200`); POST-login не проверялся | Dev-пароль в git; lockout 5/15 мин |
| `listen_addr` | `0.0.0.0:8080` | TCP 0.0.0.0:8080 | Доступ из LAN (192.168.1.93) |
| `candidate_addr` | `""` | env `VKS_CANDIDATE_ADDR=192.168.1.93`, preflight OK | Конфиг пуст, фактическое значение из env; при потере env — WARN/exit 78 |
| `database_url` | `sqlite://vks_data.db?mode=rwc` | файл пишется; WAL нет | `rwc`, не read-only; бэкап — копией файла |
| `frontend_dir` | `client/pnm-frontend/deployed/pnm` (относительный) | работает при cwd=репо | Хрупко: смена WorkingDirectory ломает UI |
| `media_listen_addr` | `0.0.0.0:40000` | UDP 40000 занят | Совпадает |
| WebRTC порты | env 43000–43100 | preflight OK; media на 40000 | Диапазон 43000–43100 — только peer-порты |
| Gateway RTP | нигде не задан | пул **41000–41198** (`rtp_bridge.rs:20-25`) | Не описан в конфиге/firewall; на удалёнке нужен ещё и 41000–41199 |
| SIP UAS | `VKS_SIP_UAS_ENABLED=1` | UDP 0.0.0.0:5060, без auth/TLS/allowlist | Открытый SIP-порт в LAN; защита только firewall/лимиты |
| H.323 allowlist | `127.0.0.1/32,192.168.1.0/24` | sidecar :1720; max calls 20, max per IP **20** | Per-IP лимит равен общему — фактически не ограничивает один IP |
| `/metrics` token | `VKS_METRICS_TOKEN` пуст | live отдаёт из LAN | HEAD должен 403; живой бинарь — нет |
| Unit (dev) | `StandardOutput=append:%h/mcu/vks_server.log`, `Restart=on-failure` | 1.3 MB, без ротации; `NRestarts=0` | Лог в репо; hardening отсутствует (в prod-unit есть) |
| Prod-шаблон | `deploy/systemd/vks-mcu-prod.service` | не используется | Не путать: live — dev-unit, не hardened |
| `deploy/vks.env.example` | H.323 OFF, allow loopback, WebRTC OFF | live: всё ON, /24 | Осознанный dev-стенд, но «безопасный дефолт» не соблюдён |

Env процесса **точно** соответствует unit + drop-in (`stand.conf`): candidate, SIP/H.323, WebRTC-диапазон. Секретов JWT/metrics в env нет — оба берутся из файла/пусты.

---

## §7. Регрессии и ложные утверждения

| # | Утверждение | Источник | Что показывает live | Вердикт |
|---|---|---|---|---|
| 1 | «Сервис перезапущен на новом бинаре `94484d42…`» | `docs/audit/2026-10-28/00_MASTER_PLAN.md:423` | PID 1814143, sha `4dcbff77…`, start 18:00:39, `NRestarts=0`, exe deleted | **Ложно** |
| 2 | «Живой бинарь `94484d42…`, PID 1418719» | `docs/audit/2026-10-28-fix/03_hygiene.md:201` | PID 1814143, sha `4dcbff77…`; `94484d42` на диске тоже нет (там `cbad4986`) | **Устарело/ложно** |
| 3 | «Живой бинарь не перезапускался; release `cbad4986` проверен на `:18082`» | `docs/audit/2026-10-28-fix/00_plan_review.md:480` | Совпадает: start 18:00, sha диска `cbad4986`, 27 274 320 B | **Верно** |
| 4 | S7/S8/gateway-фиксы «готовы» (9.5 PASS) | `00_plan_review.md:475-477` | В живом exe нет `payload_type_mismatch`, `x-vks-layout-applied`; в `/metrics` нет `vks_layout_apply_total`; previews → 501 | **Код, вероятно, готов; live — нет**. 9.5 — ревью кода, не рантайма |
| 5 | «role везде» | `00_MASTER_PLAN.md:417-423` | `/presets`: 54 Codian, 507 слотов, `AUTO` 486 / `PRESENTATION` 21 | **Верно для live-каталога** |
| 6 | «MANIFEST 214/214 OK» | `00_MASTER_PLAN.md:54` | На диске 214/214 OK; HTTP `/pnm/MANIFEST.sha256` → `index.html` 200 | **Верно по файлам; operational trap**: HTTP-проверка невозможна |
| 7 | «В БД 6 раскладок (5 пустых, 1 с 25×AUTO)» | `docs/audit/2026-10-28/07_chain.md:198` | В `configuration_documents`: 5 layouts, **все** `slots=0`; 25-слотовой нет | **Устарело** |
| 8 | «E2E чистый, probe-сессии убраны» (подразумевается) | e2e-доки/логи | 3 комнаты «E2E Probe», 3 ACTIVE-сессии без `session.stopped` | **Ложно** |
| 9 | «Dev defaults только для локалки, never expose» | `main.rs:213` | `0.0.0.0:8080` доступен из LAN; admin-JWT подделывается dev-секретом | **Ложно по факту** |
| 10 | «S6/S7 9.5 PASS» | `d82ce4d`, `00_plan_review.md` | Живой бинарь старше этих фиксов | **Не проверяемо live**; нужен рестарт/временный инстанс |
| 11 | «0 ERROR/panic» | — | 1 226 WARN, 0 ERROR, 0 panic | **Верно** |

Дополнительно: `vks_api_v3_requests_total{endpoint="unmatched"}` = 7 за текущий запуск; в логе 7 preview-501. Раньше UI-пути (`/api/v3/health`, `/auth/login`, `/ui/presets`) уже мертвы — это остатки старого фронта/скриптов, не регрессия HEAD.

---

## §8. Риски для наших изменений и план проверки

### 8.1. Что сломается/что проверить

1. **Рестарт — это деплой.** Живой процесс старый; после рестарта включится `require_moderator` на m5 и loopback-guard на `/metrics`. Любой монитор/скрипт, который сейчас ходит на `gateways/status|mcu/farm|cluster/status` анонимно, начнёт получать **401/403**; remote Prometheus без `VKS_METRICS_TOKEN` — **403**. Это ожидаемое поведение, но UI/ops надо предупредить.
2. **Порты:** 8080/tcp (HTTP/WS), 5060/udp (SIP UAS), 1720/tcp (H.323 sidecar), 40000/udp (SFU media), **41000–41199/udp** (gateway RTP/RTCP), 43000–43100/udp (WebRTC peer). Проверить, что firewall/NAT открывает не только 43000–43100, но и 41000–41199, иначе SIP/H.323 с удалёнки отвалится (на LAN работало).
3. **SQLite-миграции:** текущая схема — 5 миграций; новый бинарь может добавить миграции. Перед рестартом — копия `vks_data.db` (WAL нет, `journal_mode=delete`, копия консистентна при остановленном сервисе; есть бэкап 30 MB за 28.10), после — `integrity_check`, `_sqlx_migrations`. `VACUUM` (вернуть 39.5 % freelist) — только офлайн, не в живом.
4. **Sidecar:** сервер сам спавнит Python 3.14 venv; `KillMode=mixed`, `TimeoutStopSec=30`. После рестарта проверить: 1814221 убит, новый PID — ребёнок нового сервера, `:1720` LISTEN один, `/api/v3/gateways/status` → H.323 `READY`, `VKS_H323_UAS_ALLOW` не потерян.
5. **CSP/MANIFEST:** 4 inline-скрипта в `index.html` и 4 хеша в CSP; любая правка index/чанков требует пересчёта хешей, иначе UI белый. MANIFEST пересчитывать из `deployed/pnm` (`find . -type f … | sort -z | xargs -0 sha256sum > ../../MANIFEST.sha256`), проверять `sha256sum -c ../../MANIFEST.sha256` = 214/214; **не** проверять через HTTP (SPA отдаёт index).
6. **WASM:** в репозитории `.wasm`-артефактов нет (только `playwright-core/lib/webp_codec.wasm`); `client/vks-client-sdk` содержит wasm-совместимый код, но сборка/таргет wasm не проверена. Если в наших изменениях появится wasm-бандл — проверить CSP `script-src`/`connect-src` и загрузку.
7. **Память:** рестарт обнулит 595 MB anonymous/579 MB heap. До нагрузки — soak: 10/50 encoder-групп, замер RSS/heap до/после, цель — отсутствие роста ~9–10 MB/группу; иначе MemoryMax не ставить (замаскирует утечку), а чинить владение буферами.
8. **API previews:** 13 × 501 (7 за текущий запуск). Если UI полагается на preview для SIP/H.323 — после наших изменений либо добавить маршрут, либо убрать вызов; проверить XHR-лог UI.
9. **Auth/секреты:** до любого выноса в LAN — ротировать `jwt_secret` и `admin_password`, задать `VKS_METRICS_TOKEN`, сузить H.323 allowlist, выключить SIP UAS или закрыть firewall. Иначе forged admin-JWT повторится на новом бинаре (механизм не меняется).
10. **Лог:** добавить ротацию (logrotate/journald) и не писать многострочный ffmpeg stderr в структурированный лог (или префиксовать), иначе 17.7 % строк не парсятся.

### 8.2. Минимальный чек-лист после рестарта

```bash
# provenance
readlink /proc/$(systemctl --user show -p MainPID --value vks-mcu)/exe
sha256sum /proc/$(systemctl --user show -p MainPID --value vks-mcu)/exe \
          target/release/vks-server
systemctl --user show vks-mcu -p NRestarts -p ActiveEnterTimestamp
# здоровье/порты
curl -s localhost:8080/healthz; curl -s localhost:8080/readyz
ss -lntup | grep -E '8080|5060|1720|40000'
# безопасность (ожидаем 401/403, не 200)
curl -s -o /dev/null -w '%{http_code}\n' localhost:8080/api/v3/cluster/status
curl -s -o /dev/null -w '%{http_code}\n' \
  --interface 192.168.1.93 http://192.168.1.93:8080/metrics
# БД
python3 -c "import sqlite3;c=sqlite3.connect('file:vks_data.db?mode=ro',uri=True);print(c.execute('pragma integrity_check').fetchone())"
# sidecar
pgrep -af h323_listener.py; curl -s localhost:8080/api/v3/gateways/status
# UI/CSP/MANIFEST
curl -sI localhost:8080/pnm/ | grep -i content-security-policy
cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256 | tail -1
```

---

## §9. Оценка живого рантайма

**6.0 / 10.**

Плюсы: сервис жив и отвечает (`healthz/readyz` 200, все компоненты `true`), БД целостна (`integrity/quick/FK ok`), 0 ERROR/panic, 0 ffmpeg-сирот и зомби на снимке, sidecar H.323 `READY`, CSP корректна, MANIFEST 214/214 на диске.

Минусы, удерживающие оценку:
- **provenance 2/10:** живой бинарь — deleted inode, 45 коммитов позади, не равен релизу; «новый бинарь» в доках ложен;
- **security 3/10:** анонимные ops-endpoint’ы, LAN `/metrics`, dev-JWT даёт admin, `0.0.0.0` + dev-пароли;
- **deployment 2/10:** S6/S7/S8/gateway-фиксы не в рантайме, но заявлены «9.5 PASS»;
- **memory 4/10:** 595 MB anon / 579 MB heap в простое, корреляция ~9–10 MB на группу;
- **data 5/10:** 3 активные e2e-сессии, 5 пустых layouts, дубли scope-ключей;
- **observability 6/10:** 17.7 % неструктурированных строк, нет ротации, 616 старых RTP-reject с неверной причиной.

---

## §10. Спорные тезисы

### Тезис 1. Живой сервис не соответствует ни HEAD, ни релизу на диске; выводы о текущем коде по live-логу после 18:00 недействительны
**Доказательство:** `/proc/1814143/exe` → deleted inode `1474242`, sha `4dcbff77…`, mtime 18:00:21; диск — `cbad4986…`, 23:53:20; `NRestarts=0`, старт 18:00:39; в exe есть `auto-expanding` (`645120d`), но нет `metrics are available from loopback only` (`7694a0c`), нет `payload_type_mismatch` (`8116855`), нет `x-vks-layout-applied` (`8d44b9c`); анонимные m5 — 200.
**Что переубедит:** рестарт на `cbad4986` (или новый бинарь) с подтверждением sha `/proc/<pid>/exe` = sha диска и `NRestarts=1`, после чего m5 начнут отдавать 401/403, а `/metrics` из LAN — 403.

### Тезис 2. Открытые ops-endpoint’ы и `/metrics` — не баг HEAD, а неразвёрнутый security-фикс; рестарт их закроет
**Доказательство:** в HEAD `m5_api` обёрнут `require_moderator` + `token_middleware` (`lib.rs:157-188`), `/metrics` — `metrics_access` loopback-only (`lib.rs:231-258`); live отвечает 200 анонимно, с битым Bearer и из LAN; в exe отсутствуют строки guard’ов. Коммиты `b73c188` (18:37) и `7694a0c` (19:13) новее сборки 18:00.
**Что переубедит:** 200 анонимно на бинаре `cbad4986`/HEAD — тогда это регрессия кода, а не деплоя. Пока же live-факт объясняется провенансом.

### Тезис 3. Release удерживает ~9–10 MB Rust-heap на каждую завершённую encoder-группу; 66 групп → 579 MB heap
**Доказательство:** 66 start/stop в текущем запуске, 0 ffmpeg-процессов, 595 MB anonymous / 579 MB `[heap]`; предыдущий запуск 17 групп → peak 235 MB (journal); 66×~9 MB ≈ 594 MB, 17×~10 MB + база ≈ 235 MB; RSS стабилен 20 с в простое (не «течёт на глазах»).
**Что переубедит:** heap-профиль (massif/jemalloc) до/после одной группы, показывающий возврат памяти, либо `malloc_trim`/рестарт + 10 групп с ΔRSS < 2 MB/группу; также подойдёт объяснение VSZ 4.28 GB через арены аллокатора без роста после `MALLOC_ARENA_MAX=2`.

### Тезис 4. Три ACTIVE-сессии — утечка e2e-проб, а не реальные конференции; live-данные загрязнены
**Доказательство:** `/api/v3/sessions` → 6, из них 3 `E2E Probe 1793…` в состоянии `ACTIVE`, 0 абонентов; в аудите `session.started` есть, `session.stopped`/`conference.deleted` нет; созданы 28.10 18:59–19:06 админом; одна использовалась для outbound в 19:55 и `recall_list` в 06:47; в `rooms` — те же 3 probe-комнаты.
**Что переубедит:** записи `session.stopped` для этих id или наличие живых абонентов/медиа-потоков в них; также обратный сценарий — e2e-тест штатно оставляет «активную» сессию и это документированное поведение (но тогда это всё равно мусор в БД).

### Тезис 5. Опубликованный dev-JWT-секрет делает auth на этом стенде декоративным: любой, кто читал репозиторий, получает operator
**Доказательство:** `jwt_secret = dev_ec99…` в `configs/default.toml:24`; offline-токен `HS256` c `iss=vks-mcu`, `sub=admin`, `generation=1` принят: `/api/v3/profile` → 200 `role=operator`, `/api/v3/sessions` → 200; listener `0.0.0.0:8080`, LAN-доступ подтверждён.
**Что переубедит:** ротация секрета + проверка, что старый токен даёт 401; либо доказательство, что сервис слушает только loopback/firewall и LAN-доступ невозможен (сейчас `ss` показывает `0.0.0.0:8080`, а raw-запрос с `192.168.1.93` проходит).

### Тезис 6. «role везде» подтверждено, но S6/S7/S8 и gateway-фиксы в рантайме отсутствуют — деплой частичный, и «9.5 PASS» нельзя выдавать за live-verdict
**Доказательство:** live `/presets` — 54 Codian / 507 слотов / `AUTO` 486 + `PRESENTATION` 21 (role-фикс `645120d` жив); при этом нет строк `payload_type_mismatch`, `x-vks-layout-applied`, `vks_layout_apply_total`; previews → 501; анонимные m5 → 200; в логе старый текст `(codec/PT mismatch)`.
**Что переубедит:** рестарт на `cbad4986` и повтор этих же проверок: появление reason-полей, `x-vks-layout-applied`, `vks_layout_apply_total`, 401/403 на m5. До этого «9.5» — оценка кода и тестов, не живого стенда.

### Тезис 7. `MANIFEST 214/214` — правда только для файловой проверки; HTTP-путь манифеста сломан SPA-fallback’ом
**Доказательство:** `sha256sum -c ../../MANIFEST.sha256` из `deployed/pnm` → 214 OK; `/pnm/MANIFEST.sha256` → 200 `text/html` 74 989 B (`index.html`), потому что файла в `deployed/pnm` нет; наивная проверка из `client/pnm-frontend` → 0 OK/214 fail (пути `./…` и манифест на уровень выше).
**Что переубедит:** появление реального `MANIFEST.sha256` в `deployed/pnm` и `content-type: text/plain` на `/pnm/MANIFEST.sha256`; либо документированное решение не отдавать манифест по HTTP (тогда claim надо формулировать как «файловая проверка», а не «HTTP 214/214»).

### Тезис 8. Высокий RSS — это удержанная память аллокатора, а не прогрессирующая утечка; для оценки нужен не «RSS сейчас», а Δ на группу
**Доказательство:** 20 с простоя — RSS 613 964 kB без изменений; heap 579 MB при 4.28 GB VSZ; предыдущий запуск с 17 группами имел peak 235 MB; причинная связь «группа → ~9 MB» корреляционная, прямого профиля нет.
**Что переубедит:** `MALLOC_ARENA_MAX=2`/`malloc_trim` без снижения RSS (тогда это не арены, а именно удержанные объекты), либо massif, показывающий живые `Arc<Frame>` после retire группы. Обратно: если после рестарта и 10 групп RSS не растёт — тезис 3 ослабляется, а этот — усиливается.
