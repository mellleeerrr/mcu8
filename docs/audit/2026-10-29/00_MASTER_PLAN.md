# VKS / PROSTOR MCU — сводный аудит, обсуждение и план модернизации

- **Дата:** 2026-10-29
- **HEAD:** `d82ce4d` (master, дерево чистое; untracked — только этот каталог)
- **База (проверено на HEAD):** `cargo test --workspace --locked` = **1425 passed / 0 failed / 8 ignored**, fmt/clippy — чисто, wasm32 `cargo check -p vks-client-sdk` — OK.
- **Известный дефект базы:** `cargo test --workspace --all-features` **падает** на 2 тестах `vks-media-sfu::codec::tests::*` (assert'ы зашиты под default-набор фич). Входит в T03.
- **Живой сервис:** PID 1814143, бинарь запущен из **удалённого inode** (sha `4dcbff77…`), собран 18:00, на **45 коммитов позади** HEAD; на диске лежит релиз `cbad4986…` — функционально HEAD. Живые «дыры» (анонимные m5-эндпоинты, LAN `/metrics`, поддельный JWT) — **артефакт старого бинаря**: в HEAD они уже закрыты (`b73c188`, `7694a0c`). Перезапуск — T01.
- **Артефакты аудита:** `00_AGENT_PROMPTS.md` (13 дословных промптов), `01`…`08` — отчёты по зонам, этот файл — синтез и план.

---

## §1. Команда, зоны, промпты

Дословные промпты всех запусков — в [`00_AGENT_PROMPTS.md`](00_AGENT_PROMPTS.md). Ниже — состав и результат.

| # | Агент (зона) | Отчёт | Оценка зоны | Что дал |
|---|---|---|---|---|
| 01 | `sfu-p3p5` — SFU, P3/P5, две плоскости | [`01_sfu_p3p5.md`](01_sfu_p3p5.md) | 3,5/10 | 11 пунктов матрицы: 3 реализованы, 2 частично, 6 отсутствуют; 9 guard'ов P5; две несовместимые плоскости |
| 02 | `h323-p6` — H.323, сайдкар, нативный стек | [`02_h323_p6.md`](02_h323_p6.md) | 3,3/10 | Python-сайдкар на pycrate; `ffi.rs` — mock; **14 golden-векторов ASN.1 сняты живым pycrate**; staged-план 5,2–6,4k LOC |
| 03 | `codec-mcu` — кодеки, MCU, скейлер | [`03_codec_mcu.md`](03_codec_mcu.md) | 7/10 | матрица 13 аудио × 8 видео; H.264 High false-offer; AAC `cpresent` fail-open; 6 расходящихся кодек-списков; G.722.1/G.719 infeasible |
| 04 | `infra-l5l7` — WT, запись, T10b, Argon2, NATS/OTLP, L0 | [`04_infra_l5l7.md`](04_infra_l5l7.md) | 2,5/10 | ни один из 6 пунктов не реализован; Storage no-op теряет записи; directory в одном JSON; PBKDF2 вместо Argon2 |
| 05 | `frontend-sdk` — PNM, deep link, SDK, CSP | [`05_frontend_sdk.md`](05_frontend_sdk.md) | 6,5/10 | deep link **работает** при токене; реальный дефект — гард/логин теряют URL; gzip не включён; SDK выбрасывает `room_mode` |
| 06 | `hygiene-arch` — мёртвый код, доки, артефакты | [`06_hygiene_arch.md`](06_hygiene_arch.md) | 6,0/10 | −1230 строк безопасно; 7 `#[allow(dead_code)]` с обоснованием; «док ≡ код» 3,5/10; 67,6 GiB артефактов |
| 07 | `review-recent` — ревью последних изменений | [`07_review_recent.md`](07_review_recent.md) | 7,0/10 | DTMF E-бит; `preset_default_13`; S6; иерархии size; P2-список |
| 08 | `live-runtime` — живой рантайм и БД | [`08_live_runtime.md`](08_live_runtime.md) | — | stale-бинарь, 3 ACTIVE-сессии-зомби, layouts `slots=0`, дубли `conferences:/sessions:` |

Три оппонента (D1 медиа, D2 инфра/гигиена, D3 ревью) провели контр-разбор моих тезисов; их возражения учтены в §5. Два агента (H.323, frontend) были перезапущены после потери первого прогона; отчёты получены.

---

## §2. Главный вывод

Это **не «сырой прототип»**. Инженерная база взрослая: 96 799 строк Rust, 1425 тестов, чистые fmt/clippy, аккуратные порты/FSM/лимиты, реальный str0m-транспорт с ICE/DTLS/SRTP, рабочий композитор на FFmpeg, Codian-совместимый layout-движок с 54 эталонами. Оценка «готовности к работе» — 8/10.

Проблема в другом: **заявленное и фактическое разошлись системно**, а не точечно:

1. **Три «независимые» плоскости в одном продукте** — plaintext-RTP MCU (легаси), str0m SFU, композитор FFmpeg — не соединены: mixed room запрещён 9 guard'ами, SFU ограничен 2 пирами, легаси-аналитика (BWE/simulcast/congestion) не имеет продакшн-потребителя.
2. **Матрица кодеков существует в 6 местах и уже расходится**; публичная матрица обещает то, чего нет (G.722.1/G.719/SVC/FEC).
3. **Спецификация обещает фичи, которых нет в коде** (WT, NATS, OTLP, Argon2id, T10b, recorder) — «mock-трейт + докстринг» вместо реализации и честного статуса.
4. **P0-дефекты данных/безопасности**: записи теряются в no-op Storage; mock-LDAP пускает с любым паролем; `sync-ldap` возвращает фейкового пользователя; DTMF умирает через 60 мс; H.264 High-only терминал получает «connected без видео».
5. **Живой деплой отстал на 45 коммитов** — реальные дыры live уже исправлены в коде, но не выкачены.

Поэтому план — не «дописать фичи», а **сначала закрыть P0 и выровнять «док ≡ код», затем собрать продуктовые сценарии (SFU >2, mixed room, запись, справочник), и только потом — длинные эпики (нативный H.323, WT для браузера)**.

---

## §3. Диспозиция целевой матрицы

Статусы: **✅ сделано** · **🟡 частично** · **🟨 mock** · **❌ нет** · **⛔ отклонено с причиной** · **⏸ отложено с гейтом**.

### 3.1. `vks-media-sfu` (P3/P5)

| Пункт цели | Статус | Решение |
|---|---|---|
| str0m 0.24.1, ICE/DTLS/SRTP, TWCC/BWE, pacer, NACK/RTX | ✅ | Не переписывать. Тюнинг — в T11 (reorder, caps, RED). |
| 3-слойный simulcast | ❌ | **Реализуем через rid** (str0m умеет `a=simulcast`, `MediaData.rid`), но только после per-source egress. T11b. |
| SVC (VP8/VP9/AV1/H264/H265) | ⛔ | В str0m **нет SVC-forwarding API**; SIP-терминалы и браузерный SDK SVC не используют. Замена — simulcast. Матрица диспозиций, не ADR-«отказ». |
| TWCC/REMB BWE | 🟡 | TWCC — да (str0m), REMB — нет (REMB не нужен, т.к. TWCC основной); зафиксировать в доке. |
| Jitter buffer | ❌ | В SFU **не делать** (str0m сам буферизует, adaptive JB = ✗). На шлюзе — bounded reorder 40–80 мс + PLC, гейт: netem-замер. |
| FEC 10:1 | ⛔ | XOR-FEC в WebRTC не реализуем (нет в str0m, браузеры не договариваются). Замена: NACK/RTX + RED; Opus in-band FEC — отдельный fmtp. |
| Pacing P3 | ✅ | У str0m; не дублировать. |
| CSS/Taffy layouts | ⛔ | Серверный layout — **прямоугольники** (`vks-layout`), клиент рисует CSS. Taffy не нужен: тянет зависимость без выигрыша, Codian-эталоны — абсолютные координаты. Зафиксировать в доках. |
| Per-source egress > 2 пиров | ✅ | T11b: клиентский pre-offer `1 audio +(N-1) audio`, `1 video +(N-1) video` (1 sendrecv + recvonly), сервер `(publisher,kind)→mid` с direction-aware answer; `MAX_PEERS_PER_ROOM` заменён на `VKS_WEBRTC_MAX_PEERS_PER_ROOM=8` (fail-closed). Вендорский бандл SDP не содержит — вместо патча fail-closed drift-guard `scripts/check-pnm-webrtc-drift.py`. «8 mids в answer» — нельзя (RFC 3264), answer зеркалит все m-lines. |
| Keyframe-gate + адресный PLI | ❌ | T11a: `MediaData::is_keyframe`, PLI уже есть; не форвардить дельты до IDR. |
| Re-offer | ❌ | T11a: `PeerCommand::Reoffer` на том же peer (str0m не перезапускает DTLS), 150–250 строк. |
| P5 SFU↔MCU hybrid bridge | ⛔→❌ | T12: audio-мост + composite-egress; снятие guard'ов **после** моста (`bridge_available()`), иначе «connected но тишина». Композит — H.264-only политика (VP8-декодера в MCU нет). |
| Легаси plaintext-плоскость | 🟨 | Не «cfg(test)-эталон» (держит 777 строк аналитики). T11c: снять целиком (2176 prod/79 тестов) **после** default `VKS_WEBRTC_TRANSPORT=1` + fail-closed, заменить 6 браузерными тестами. |

### 3.2. H.323 (P6)

| Пункт | Статус | Решение |
|---|---|---|
| Нативный Rust Q.931/H.225/H.245, удаление Python | ❌ | **Отдельный эпик**: паритет с Python — 5,2–6,4k LOC, 18–24 дня; полная цель +4–8 недель. Готовых крейтов нет (crates.io: h323/h225 → 0). |
| `ffi.rs` (H323Plus FFI) | 🟨 | Mock 279 строк, 0 вызовов — **удалить в T16**. |
| Golden-векторы ASN.1 | ✅ | 14 векторов сняты pycrate (в отчёте) — основа паритета. Реальный путь: `rasn-compiler 0.16` codegen (проверено: 776 типов, компилируется после пост-обработки импортов) + патч rasn `decode_bmp_string`; **APER-паритет с pycrate не подтверждён** (rasn не декодировал Setup на golden-векторе) → это epic, не задача релиза. |
| fastStart / reverse OLC / H.239 / H.235 | ❌ | В плане эпика S6–S8; H.235 — fail-closed сейчас (T16). |
| Кодек-матрица H.323 (G.728/G.729/G.722.1C/G.719/Opus/AAC-LATM) | ⛔ | G.729 — нет валидируемого энкодера; G.722.1/C/G.719 — нет реализаций; Opus/AAC — нет стандартных H.245-альтернатив (generic OID + dynamic PT). Честная таблица `implemented/rejected`. |
| Python-сайдкар: медиа, безопасность, счётчики | 🟡 | T16: RX-латч по IP+SSRC (не по ACK), fail-closed H.235, UII-энкод, `active_calls`, DTMF-форвард, фикс стенда `h323bot.py`. |

### 3.3. Кодеки / MCU

| Пункт | Статус | Решение |
|---|---|---|
| G.729 двунаправленно | ❌ | Энкодер в `oxideav-g729 0.0.8` **есть**, но не bit-exact (README), а ITU-векторов в workspace нет (тесты skip). T04: **не объявлять**; документ + опциональный feature-путь после получения корпуса. |
| AAC-LATM разблокировать | 🟡 | T04: сначала `cpresent` fail-open (uas) + outbound без fmtp (rtp_bridge), потом переименовать флаг в `VKS_CODEC_LICENSE_ACCEPTED=aac-latm`. |
| G.722.1/C + G.719 | ⛔ | Нет крейтов и нет кода в FFmpeg 8.0.1; лицензия Polycom. В `UNSUPPORTED` с причиной. |
| Симметрия H.264 Baseline/Main/High | 🟡 | T04: P0 = убрать PT98 High из Native-оффера (2 строки, fail-closed); P1 = Main/High в answer + профили энкодера. |
| H.265 RFC 7798 | ✅ | Готов; не трогать. |
| H.263+ AVI demux | 🟨 | T04: мёртвый код — удалить; H.261 опционально позже. |
| 2D bilinear scaler | ❌ | T09: свои 150–250 строк (oxideav-scene не скейлер). |
| oxideav-scene по умолчанию | ⛔ | Фича пустая; удалить в T03/T09. |
| Единая `CodecSpec` | ❌ | T04: одна таблица `native/worker/webrtc/unsupported + reason`, 3 сгенерированных среза; мета-тест. |

### 3.4. Инфраструктура / данные / хост

| Пункт | Статус | Решение |
|---|---|---|
| WebTransport (wtransport) | 🟨 | T13: реальный UDP-листенер **для нативных клиентов**, `VKS_WT_PORT`, cert/key + `/wt/cert-hash`, Alt-Svc; браузер — ADR «не в этом релизе» (serverCertificateHashes). `MockWt` → cfg(test) MemoryTransport. |
| Recorder bitrate model + запись | 🟨 | T09: Storage-контракт обязательный + гидратация; MP4: avcC, Annex-B→length-prefix, timescale=sample_rate, esds, stco; **tap = subscribe к существующей группе фермы** (нет зрителя → 409 `no_active_h264_encoder_group`); RecorderProfile — клампы/оценки, не энкодинг. SFU-запись → 501. |
| T10b PostgreSQL normalization | ❌ | T10: `schema_state` one-shot маркер + 6 таблиц + partial unique индекс; удалить `directory/addresses` из JSON **в той же транзакции**, сохранив неизвестные поля; удалить поле из `ResourceState`; тесты sqlite==pg. |
| Argon2id + semaphore | ❌ | T02: m=19456,t=2,p=1, ≤128; **семафор 4 + bounded queue + 429**, глобальный token-bucket 50/с burst 200; лимит длины пароля; сравнение 32 байт; `register_async`; lazy rehash. |
| NATS bus + OTLP | 🟨 | T14: UUIDv7/traceparent в `Msg`; feature-gated NATS + CI-сервис nats с тестом lease **между двумя узлами**; LocalTransport-fallback; OTLP с collector в CI + shutdown guard; `[bans]` в deny.toml. |
| L0: sysctl + cgroup v2 | 🟨 | T15: `SO_RCVBUF` (socket2), RT-потоки, `sysctl.d`, systemd `Slice/CPUWeight/MemoryHigh/MemoryMax/LimitRTPRIO`; **без кода лимиты — театр**, поэтому код+юнит+проверка. |

### 3.5. Frontend / SDK / деплой

| Пункт | Статус | Решение |
|---|---|---|
| Deep link на сессию | ✅ | Работает при живом токене (проверено). **Реальный дефект** — гард/логин теряют URL; T07 решает внешним проектного слоя `deeplink.js` (без патча вендорского бандла). |
| `#0097A7` → `#17212B` | 🟡 | `#17212B` — surface, акцент `#0088cc`; `theme-color #0097A7` перетирается `theme-toggle.js`. T07: meta theme-color == computed `--c-bg` (`#0e1621` dark), manifest `#17212B`, вендорский CSS не перебирается. |
| MANIFEST/CSP | ✅ | MANIFEST — provenance (215 файлов, CI проверяет sha256 + count/refs/theme), CSP считается при старте из inline; CSP-правка требует рестарта — задокументировано. |
| Gzip (precompressed) | ❌ | T07: runtime gzip (`tower-http` `CompressionLayer`), проверено `content-encoding: gzip`. |
| WASM SDK `ServerHello.room_mode` | 🟡 | T08: SDK теряет поле — прокинуть, выбирать конвейер при join, `RoomModeChanged`; публичный `#[wasm_bindgen]` — минимальный. |
| CSS/Taffy в SDK | ⛔ | Клиент получает прямоугольники; Taffy не внедрять. |
| Host L0 | 🟨 | T15 (см. выше). |

---

## §4. Обсуждение: что я изменил после возражений

Полные ответы оппонентов — в сообщениях сессии (сжато здесь). Ключевое:

**Принято (моя позиция изменена):**

1. **Легаси нельзя оставить «эталоном под cfg(test)»** — `SfuEngine` держит `BweEstimator/CongestionLadder/SimulcastSelector` полями, 777 prod-строк останутся гнить. Решение: полное удаление (2176/79) после default `VKS_WEBRTC_TRANSPORT=1` + fail-closed, замена 6 тестами.
2. **Per-source egress — не одна задача**, и «предсоздать 8 mids в answer» невозможно (answer не добавляет m-lines). Дизайн: клиентский pre-offer 1 audio + 8 video; сервер маппит `(publisher,kind)→mid`; 1,5–2 задачи.
3. **Запись через «tap уже кодируемого композитора» невозможна без зрителя** — группа фермы кодирует на зрителя/профиль. Компромисс: `EncoderFarm::subscribe` к существующей группе, 409 при её отсутствии.
4. **`preset_default_13` → 400 намеренный** (тесты `resource_tests.rs:821–860` требуют 400; у базовых id нет size). Чинить оракул e2e (`vendor400Count===1`), а не сервер.
5. **S6: растягивать сетку до 100% нельзя** — разные окна/аспекты (n=7 → 1280×240), рассинхрон с вендорским `preset_equal_3`. Решение: центрирование оставить, добавить тест движка n=5@1280×720 (fill 83,28%) + строку в LAYOUT_ENGINE.md.
6. **Argon2: семафор 4** (не 2) + bounded queue + 429; глобальный rate-limit обязателен; m=19456 не снижать.
7. **T10b: `ON CONFLICT DO NOTHING` недостаточно** — нужен one-shot `schema_state` + удаление ключей из JSON в той же транзакции.
8. **room_mode вычислять в `on_client_hello`** (room_id приходит только там), guard'ы не снимать, а заменить на `bridge_available()`.
9. **H.264: P0 — 2 строки** (убрать PT98 High из Native), симметрия — P1.
10. **G.729 не добавлять** без векторов/bit-exact; **H.263+ удалить**, H.261 — опционально.
11. **WT — только «нативный клиент»**, не «R5 выполнен»; `MockWt` → cfg(test) MemoryTransport.
12. **OTLP/NATS** — feature + CI-сервисы (иначе честно «за фичей»); UUIDv7 до NATS.
13. **CI**: all-features тест, doc-links без `continue-on-error`, cache-key с `.cargo/config.toml`+toolchain, удалить oxideav-джобу и дубль python, e2e — manual-only (сначала `node --check` + MANIFEST + patch --check + Rust in-process calculate).
14. **Бюджет гигиены −1230 строк**, не −1500 (MP4 чинить, а не удалять).
15. **Артефакты**: `cargo clean` + явные пути; **нельзя** `git clean -fXd` (снесёт живую БД/backups/archive); git-историю (legacy/trace) — отдельное окно filter-repo.

**Оставлено как есть (я не согласен с возражением):**

- **H.323 native**: я согласен, что это эпик; но в план релиза включаю **инфраструктуру паритета** (golden + codegen-физибилити + удаление mock + P0-харднинг), иначе «нативный стек» остаётся лозунгом.
- **Матрица диспозиций** вместо ADR-«отказ»: и SVC/XOR-FEC/FEC — не «не хотим», а «нельзя на WebRTC»; это фиксируется таблицей статусов.

---

## §5. Стек и версии (проверено по sparse-index/исходникам)

| Крейт | Версия | Зачем | Риски/примечания |
|---|---|---|---|
| `str0m` | **0.24.1** (уже) | ICE/DTLS/SRTP/BWE/pacer/NACK | не обновлять в этом цикле |
| `rasn` | **0.29.0** + `rasn-compiler 0.16.0` (build/dev) | ASN.1 APER для H.323 | APER decode BMPString — `todo!()`; нужен локальный патч/вендоринг; паритет с pycrate не подтверждён → epic |
| `wtransport` | **0.7.2** | WebTransport для нативных клиентов | MSRV 1.88; браузер требует cert-hashes; 2 новых дубля (sha2 0.11/thiserror 2) → `[bans]` |
| `argon2` | **0.5.3** | Argon2id | 0.6 переписан/зависимости yanked |
| `async-nats` | **0.51.0** | NATS feature | MSRV 1.89, rustls |
| `opentelemetry` / `-sdk` / `-otlp` / `tracing-opentelemetry` | **0.33.1 / 0.33.1 / 0.33.1 / 0.34** | OTLP feature | tonic 0.14/prost 0.14 — дубли, `[bans]` |
| `taffy` | **не внедрять** | — | прямоугольники + CSS покрывают; Codian-эталоны абсолютные |
| `socket2` / `libc` | **0.6.5 / 0.2.190** (уже в lock) | SO_RCVBUF/RT | — |
| `oxideav-g729` | **0.0.8** — только для экспериментов, не объявлять | G.729 | энкодер не bit-exact, векторов нет |
| `oxideav-h261` | 0.0.7 — опционально | H.261 | только при продуктовой необходимости |
| FFmpeg | **8.0.1** (host) | libx264/libx265/h264/hevc/opus | G.722.1/G.719 отсутствуют |
| Rust | **1.94.0**, edition 2021, MSRV 1.94 | — | `export PATH="$HOME/.cargo/bin:$PATH"` |
| Node | в CI **нет** — добавить `node --check` в job | JS-гейт | Playwright — manual-only |

Новые зависимости добавляются только под feature-флагами (`nats`, `otlp`, `webtransport`), с `deny.toml [bans]` и записью в `ARCHITECTURE.md`.

---

## §6. Пошаговый план (задачи, зависимости, приёмка)

Правила: каждая задача = один логический коммит; перед задачей — субагент «подводные камни», после — 2–3 параллельных ревьюера, коммит только при ≥9,5/10 хотя бы у одного; трекер — `docs/progress/MASTER_TASK_LIST.md` (обновляется после каждой задачи).

### Фаза 0 — безопасность и живой деплой

- **T01. Перезапуск HEAD + проверка live. ✅ ВЫПОЛНЕНО 2026-10-29 07:33.** Release на HEAD (`cbad4986…` = дисковому релизу), `stop`, бэкап `vks_data.db` (`backups/2026-10-29/vks_data.db.pre-d82ce4d`, integrity=ok), smoke на `:18082` с копией БД/портами (healthz/readyz 200, m5 401, fake JWT 401, login 200, MANIFEST 214/214, CSP 5 hash), рестарт; `/proc/<pid>/exe` — реальный файл, sha совпал, sidecar — новый child (PID 3255343), LAN `/metrics` → 403, loopback → 200, anonymous m5 → 401. Остаток на T17: очистка stale-данных (3 ACTIVE-зомби, layouts slots=0, дубли active_layouts) — только через код/миграцию, не руками.
- **T02. Безопасность аутентификации.** mock-LDAP fail-closed (`accept=None` → ошибка; `allow_mock_ldap` только dev + явный env); `POST /api/v1/users/sync-ldap` → 501 без провайдера; Argon2id m=19456/t=2/p=1 + семафор 4 + bounded queue + 429; глобальный token-bucket 50/с burst 200; пароль ≤128; сравнение 32 байт; `register_async`; lazy rehash без сброса generation. Тесты: 6+ юнитов, нагрузочный лимит, миграция формата.
- **T03a. CI-гейты.** Фикс all-features тестов (cfg-aware ожидания; пока `--features` без nats/otlp/webtransport — их тесты появятся в T14); `cargo test --workspace --all-features`; cache-key; удалить oxideav-джобу/дубль python; `node --check` + MANIFEST + patch --check; снять continue-on-error у str0m. **T03b. Доки.** doc-links без continue-on-error (сначала починить ~30 ссылок); `DEVELOPMENT_PLAN.md:274-300`, `ARCHITECTURE.md:148,375`, дерево §3. **T03c. Мёртвый код.** Удалить `transparency.rs`, `qoe/slo/ratelimit`, `ffi.rs`, пустую oxideav-фичу, неиспользуемые зависимости; поправить 2 doc-ссылки. Приёмка: −1230 строк, fmt/clippy/test/all-features/doc/release зелёные.

### Фаза 1 — P0 продукта: кодеки, DTMF, layout, frontend

- **T04a. SDP/кодеки P0–P1 (не смешивать с CodecSpec).** H.264: PT98 High убрать из Native/Full-оффера (сейчас `native:false`, но в Full-матрице оффер уходит; answer отвергает `profile_idc!=66` → «connected без видео»); `a=sendrecv`/направление вывести из оффера; поиск кодека по identity, а не PT (перенумерация); G.726-32 PT2; AAC `cpresent` fail-open в `uas.rs:478` и outbound без fmtp (`rtp_bridge.rs:1360`); флаг `VKS_CODEC_LICENSE_ACCEPTED`; честная матрица G.722.1/G.719. Тесты — дословно из §5.1–5.3 отчёта 03. Удалить мёртвый H.263+ AVI-код.
- **T04b. CodecSpec SSOT.** Одна таблица `native/worker/webrtc/unsupported+reason` + 3 генерированных среза + мета-тест «у каждого элемента ровно один оффер/ветка»; замена 6 списков. Приёмка: нет лжи в публичной матрице.
- **T05. DTMF.** Маркер только на `step==0`, общий timestamp/`started`, `duration` +160 мс; `send_to` → метрика/лог; тесты `dtmf_packets()` + receiver_tone_ms()==200 (сейчас 60).
- **T06. Layout.** `normalize_loaded` recalc + единый резолвер size (4 порядка → 1); переписать `resource_tests.rs:2495` (size>preset → 25) после смены контракта; P2-5/6/8/9/13; тест движка n=5@1280×720 (центрирование, fill 83,28%); `preset_default_13` — 400 **намеренный** (тесты `resource_tests.rs:821–860`), чинить оракул `layout_constructor_ui.js:311` (vendor400Count>0, unexpected===0), не сервер; docs `BACKEND_RULES.md`/`LAYOUT_ENGINE.md`.
- **T07. Frontend PNM.** Сохранение deep link в гарде/логине (внешний JS или python-патч бандла); импорт сохраняет `theme-toggle.js`/manifest; `CompressionLayer` (не precompressed .gz — их 0 и они в .gitignore), curl-приёмка `content-encoding: gzip`; тема: инвариант `meta[theme-color] == computed --c-bg` (`#0E1621` dark; `#17212B` только в manifest), + `scripts/check-pnm-theme.py`; CI-гейты (`check-pnm-runtime.py`, `check-pnm-theme.py`). Приёмка: e2e «deep link без токена → логин → та же сессия», 215/215 MANIFEST + CSP-хэши после рестарта.
- **T08. SDK.** `ServerHello.room_mode` в структуре + выбор конвейера при join + `RoomModeChanged`; wasm32 check; тест 51/51.

### Фаза 2 — данные и запись

- **T09a. Запись: данные и API.** Обязательный Storage-контракт (убрать no-op default) + гидратация `records` в `state.rs`; `/api/v3/videorecords`; RecorderProfile (клампы/оценки); 409 `no_active_h264_encoder_group`; soak-лимит памяти фермы (66 групп → ~579 MB).
- **T09b. MP4-контейнер.** avcC, Annex-B→length-prefix, `mdhd timescale=sample_rate`, esds, stco; tap через `EncoderFarm::subscribe` к существующей группе; ffprobe/ffmpeg-тест на реальном Annex-B x264 (skip без ffprobe, как PG); bilinear scaler — отдельным модулем (T12, не здесь).
- **T10. T10b.** Миграция directory в SQL: `schema_state`, 6 таблиц, partial unique, удаление ключей из JSON в одной транзакции, удаление поля из `ResourceState`, идемпотентность, sqlite==pg.

### Фаза 3 — SFU/P5 и транспорт

- **T11a. SFU P3:** default `VKS_WEBRTC_TRANSPORT=1` + fail-closed preflight; keyframe-gate+PLI; re-offer (`PeerCommand::Reoffer`); включить VP9/AV1/H264/H265 в caps; RED/reorder тюнинг; 6 браузерных тестов.
- **T11b. Per-source:** серверный маппинг `(publisher,kind)→mid` (N−1 аудио-потоков отдельными mid'ами, без микширования; mid не переиспользуется между паблишерами) + `VKS_WEBRTC_MAX_PEERS_PER_ROOM` (default 8, fail-closed) вместо `MAX_PEERS_PER_ROOM`; keyframe-gate per publisher. Вендорский бандл PNM **не содержит SDP** (проверено: только feature detection `"RTCPeerConnection" in window`), поэтому деливерабл `scripts/patch-pnm-webrtc-mids.py` снят — вместо него no-op сегодня fail-closed `scripts/check-pnm-webrtc-drift.py` (exit 1, если бандл начнёт говорить SDP) + unit-тесты в CI. Гейты: 3+ пира в комнате, pre-offer 1+7/1+7 до `createOffer`, легаси 1+1 деградирует явно (`webrtc_sources{dropped_no_mid}`), а не склейкой часов.
- **T11c. Удаление легаси:** после T11a/T11b — 2176/79 строк, `VKS_WEBRTC_TRANSPORT` обязателен.
- **T12. P5 hybrid:** room_mode в `on_client_hello`; guards → `bridge_available()`; audio-мост; composite-egress (H.264-only) с VP8-политикой; e2e mixed room; удаление `evict`.
- **T13. WebTransport native:** wtransport UDP-листенер, порт, cert/key + `/wt/cert-hash`, Alt-Svc при on, 404 при off; auth до accept; лимиты; MemoryTransport в cfg(test); тесты нативных клиентов.
- **T14. NATS/OTLP:** UUIDv7 + traceparent; feature-gated NATS + CI nats + тест lease двух узлов + fallback; OTLP + collector в CI + shutdown guard.
- **T15. L0:** SO_RCVBUF/socket2, RT-потоки, sysctl.d, systemd Slice/CPUWeight/MemoryHigh/Max/LimitRTPRIO; проверка на живом процессе.

### Фаза 4 — H.323 и финал

- **T16. H.323 hardening:** удалить `ffi.rs`; RX-латч по IP+SSRC; fail-closed H.235; UII-энкод; `active_calls`; DTMF-форвард; фикс стенда; golden-харнесс + ADR «нативный стек: этапы/оценка» (с честной фиксацией, что APER-паритет rasn не подтверждён).
- **T17. Финал:** артефакты (cargo clean, target-impl/proto, measure, pycache, node_modules), stash-ревью, MANIFEST/доки, полный прогон (fmt/clippy/test/all-features/doc/deny/release/wasm), финальный рестарт+проверка live, `MASTER_TASK_LIST.md`, итоговый отчёт.

**Порядок:** T01 → T02 → T03 → (T04, T05, T06, T07 параллельно) → T08 → T09 → T10 → T11a → T11b → T11c → T12 → T13 → T14 → T15 → T16 → T17. T13/T14/T15 — feature-gated, могут идти параллельно с T09/T10 при наличии рук.

---

## §7. Протокол ревью (обязателен для каждой задачи)

1. **Перед задачей:** субагент «pitfalls» — 5–8 подводных камней с file:line и способом проверки; Lead учитывает.
2. **Реализация:** минимальный диф; тесты рядом с кодом.
3. **После:** 2–3 параллельных ревьюера (разные углы: корректность/безопасность, тесты/оракулы, регрессии/доки), каждый даёт оценку 1–10, слабые места и обязательные фиксы; ревьюеры **защищают позицию** до согласия.
4. **Коммит:** только если **нет незакрытых P0/P1** и медиана оценок ≥9,0 (хотя бы один ≥9,5); отчёт ревью — файлом `docs/audit/<date>/reviews/<task>.md`, автор ревью ≠ исполнитель; «9.5» не ставится автором в том же коммите (дефект d82ce4d).
5. **После коммита:** `MASTER_TASK_LIST.md` → done, следующий незакрытый таск. Финальный прогон fmt/clippy/test/all-features.

---

## §8. Риски и «что нельзя доказать»

- **H.323 e2e-медиа, fastStart/reverse, H.239, H.235-endpoint, H.261/263/265** — без реального терминала (Polycom/Cisco) не доказуемы; в отчёте — честный статус, не «[x]».
- **WT в браузере** — требует `serverCertificateHashes` + UDP/QUIC на пути; в этом релизе только нативные клиенты (ADR).
- **G.729** — нет ITU-корпуса → не объявлять; кодек-мост не включать по умолчанию.
- **SVC/FEC/adaptive JB** — не реализуемы на текущем стеке; фиксируются матрицей диспозиций.
- **Живая БД**: журнал `delete` (не WAL) → консистентная копия только при остановленном сервисе; бэкап до рестарта обязателен.
- **`.cargo/config.toml` `target-cpu=native`** → бинарь непереносим; перед `cargo clean` — копия релиза в `backups/`.

---

## §9. Приложения

- Отчёты: [`01`](01_sfu_p3p5.md) · [`02`](02_h323_p6.md) · [`03`](03_codec_mcu.md) · [`04`](04_infra_l5l7.md) · [`05`](05_frontend_sdk.md) · [`06`](06_hygiene_arch.md) · [`07`](07_review_recent.md) · [`08`](08_live_runtime.md)
- Промпты: [`00_AGENT_PROMPTS.md`](00_AGENT_PROMPTS.md)
- Трекер: `docs/progress/MASTER_TASK_LIST.md`
