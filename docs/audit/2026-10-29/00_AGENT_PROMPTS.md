# 00 — Точные промпты запущенных субагентов (Stage 1)

Дата: 2026-10-29. Сессия Lead: `session-b0563da0-0a3a-41d2-9844-6a9c7d5a927c`.
Промпты извлечены дословно из журнала сессии (`~/.dsh/sessions/.../session.v4.jsonl.zstd`) — это не реконструкция.

| # | Роль | Subagent id | Выход |
|---|---|---|---|
| A1 | `sfu-p3p5` | `4d156fa6-7e15-460d-8c67-81c1ebeae325` | docs/audit/2026-10-29/01_sfu_p3p5.md |
| A2 | `h323-p6` | `ad6ab6e5-7420-43d1-ab1c-4869c09a0474` | docs/audit/2026-10-29/02_h323_p6.md (не создан; перезапуск A2b) |
| A3 | `codec-mcu` | `7c882251-4386-4167-9eba-39587093e4eb` | docs/audit/2026-10-29/03_codec_mcu.md |
| A4 | `infra-l5l7` | `d9f29012-367a-4b44-9377-0ca94575761a` | docs/audit/2026-10-29/04_infra_l5l7.md |
| A5 | `frontend-sdk` | `dc96c30f-ff37-42e0-9d18-8fa7c1fe8784` | docs/audit/2026-10-29/05_frontend_sdk.md (не создан; перезапуск A5b) |
| A6 | `hygiene-arch` | `b34c7f54-fb19-433c-9603-8de087176e48` | docs/audit/2026-10-29/06_hygiene_arch.md |
| A7 | `review-recent` | `30c7d508-1377-4b51-8008-e00a107acd46` | docs/audit/2026-10-29/07_review_recent.md |
| A8 | `live-runtime` | `79fa89b2-ed79-4d45-b140-9b18ba4ed309` | docs/audit/2026-10-29/08_live_runtime.md |
| A2b | `h323-p6-v2` | `68e63f07-6a7d-481e-99d4-76122508a393` | docs/audit/2026-10-29/02_h323_p6.md |
| A5b | `frontend-sdk-v2` | `ba39778c-3da1-4470-b16c-4173f76f7343` | docs/audit/2026-10-29/05_frontend_sdk.md |
| D1 | `debate-media` | `6b9757fb-d7f4-4228-9f6f-74cb4da1d4d1` | устный ответ Lead'у (без файла) |
| D2 | `debate-infra-hygiene` | `eecf9575-e8d7-4d28-b179-b4582d4b3c30` | устный ответ Lead'у (без файла) |
| D3 | `debate-review` | `ffa37d84-4bf9-4acb-9f44-ea7708d87a26` | устный ответ Lead'у (без файла) |

## A1 — `sfu-p3p5` (id `4d156fa6-7e15-460d-8c67-81c1ebeae325`)

```text
Ты — саб-агент `sfu-p3p5` в команде модернизации VKS/PROSTOR MCU.

ОБЩИЙ КОНТЕКСТ
- Репозиторий: /home/pro/Рабочий стол/mcu, ветка master, HEAD d82ce4d, рабочее дерево ЧИСТОЕ.
- Rust 1.94.0 (PATH: export PATH="$HOME/.cargo/bin:$PATH"), baseline: `cargo test --workspace --locked` = 1425 passed / 0 failed / 8 ignored. Сеть к crates.io есть.
- Живой сервис на :8080 НЕ перезапускать, БД vks_data.db не менять, файлы кода не менять.
- Единственный файл, который тебе разрешено создать: docs/audit/2026-10-29/01_sfu_p3p5.md. Больше ничего не писать.
- Ты работаешь в режиме глубокого read-only аудита. Полные файлы читай целиком (`cat`/read), не по фрагментам; для поиска — grep/glob. Тяжёлые сборки не запускай (максимум один `cargo check -p vks-media-sfu -p vks-server --locked`).

ТВОЯ ЗОНА
`crates/vks-media-sfu` (engine.rs, forwarder.rs, simulcast.rs, bwe.rs, congestion.rs, codec.rs, dtls.rs, ice.rs, udp_loop.rs, webrtc.rs, webrtc_room.rs, lib.rs), `crates/vks-server/src/webrtc_runtime.rs`, `crates/vks-server/src/transcoder_runtime.rs`, `crates/vks-core` MediaEngine/ServerHello.room_mode, `crates/vks-layout` CSS/Taffy-дескрипторы, а также места в `crates/vks-server/src/gateways.rs`, где выбирается режим MCU/SFU и смешанная комната.

ЦЕЛЕВОЕ СОСТОЯНИЕ (заявка заказчика, проверь её критически)
Полный SfuEngine на str0m 0.24.1; 3 слоя simulcast Low/Mid/High + SVC (VP8/VP9/AV1/H264/H265); TWCC/REMB BWE; адаптивный джиттер-буфер; NACK/RTX; FEC 10:1 (XOR); RTP pacing (P3); раскладки CSS/Taffy из vks-layout; гибридный мост SFU<->MCU (P5): браузерные участники WebRTC + терминалы SIP/H.323 в одной комнате видят общий композит; `ServerHello.room_mode` — единственный источник истины.
В заявке приведены примеры кода (RtpTranslator, ResilienceBuffer, TwccBweState, HybridRoomBridge) — проверь их на корректность: переключение слоёв/seq/ts, XOR-FEC при пакетах разной длины, token-bucket, реальность HybridRoomBridge (в примере цикл ничего не делает). Не копируй сомнительное.

ЧТО СДЕЛАТЬ
1. Составь матрицу: что реально реализовано / mock / dead / отсутствует — по каждому пункту целевого состояния, с file:line.
2. Найди баги и регрессии: некорректная арифметика wrapping seq/ts, переполнения, паники на unwrap/индексах, блокировки/блокирующий I/O в async, утечки задач/сокетов, гонки, неверная обработка RTCP, отсутствие keyframe-gate, деградация при NACK/FEC, несовпадение PT/SSRC, безопасность (ICE/DTLS, анонимный доступ).
3. Оцени, можно ли реализовать целевое МЕНЬШИМ кодом и без дублей: какие абстракции лишние, что уже есть в str0m (например TWCC/jitter/NACK у str0m) и не надо писать руками; где единственный источник истины.
4. Предложи минимальный элегантный план внедрения P3/P5 с точными файлами, оценкой строк, тестами (unit + детерминированные сценарии) и критериями приёмки.
5. Проверь утверждение «ServerHello.room_mode — единственный источник истины»: найди все места, где режим определяется иначе.
6. Отдельно: P5-мост. Какие данные реально нужно передавать (RTP пакеты/кадры/сэмплы), где граница кодеков, что ломается при транскодинге, какой минимально достаточный контракт.

ФОРМАТ ОТЧЁТА (docs/audit/2026-10-29/01_sfu_p3p5.md)
- §1 Вердикт (3–6 строк).
- §2 Таблица состояние/цель (пункт | статус | file:line | доказательство командой).
- §3 Дефекты по severity (P0/P1/P2), каждый: симптом, причина, file:line, минимальный фикс, тест.
- §4 Оценка примеров кода заказчика: корректно/некорректно, почему.
- §5 «Меньше кода»: конкретные удаления/упрощения с оценкой строк.
- §6 Пошаговый план P3/P5 (задачи 0.5–2 дня, зависимости, риски, тесты).
- §7 Открытые вопросы/что нельзя утверждать без живого прогона.
- §8 Оценка текущего состояния зоны 1–10 и что нужно для 9.5.

ДЕБАТЫ: ты обязан отстаивать свою позицию. В конце отчёта добавь раздел §9 «Спорные тезисы» — 5–8 утверждений в формате: ТЕЗИС / ДОКАЗАТЕЛЬСТВО (file:line, команда) / ЧТО МЕНЯ ПЕРЕУБЕДИТ. Если ты не уверен — так и напиши, но не выдавай гипотезу за факт. Lead будет контраргументировать, ты должен либо принять аргумент с обоснованием, либо убедить Lead.

В финальном сообщении верни сжатую выжимку (≤60 строк): вердикт, топ-7 находок с severity и file:line, 5 главных тезисов, оценка/10.
```

## A2 — `h323-p6` (id `ad6ab6e5-7420-43d1-ab1c-4869c09a0474`)

```text
Ты — саб-агент `h323-p6` в команде модернизации VKS/PROSTOR MCU.

ОБЩИЙ КОНТЕКСТ
- Репозиторий: /home/pro/Рабочий стол/mcu, ветка master, HEAD d82ce4d, дерево ЧИСТОЕ.
- Rust 1.94.0 (export PATH="$HOME/.cargo/bin:$PATH"), baseline 1425 passed / 0 failed / 8 ignored. Сеть к crates.io есть.
- Живой сервис :8080 и Python-сайдкар на TCP 1720 (PID ~1814221, venv ~/.local/share/vks-mcu/h323-venv, pycrate 0.8.1) НЕ трогать/не перезапускать. БД и код не менять.
- Единственный файл, который можно создать: docs/audit/2026-10-29/02_h323_p6.md.
- Режим read-only. Полные файлы читай целиком. Тяжёлые сборки не запускай.

ТВОЯ ЗОНА
`crates/vks-gateway-h323/` целиком: src/lib.rs, ffi.rs, q931.rs, codec.rs, call.rs, uac.rs, uas.rs; python/ (h323_sidecar.py, h323_listener.py, h323_audio.py, h323_asn.py, requirements.lock, тесты); `crates/vks-server/src/gateways.rs` и `gateway_audio.rs`/`gateway_video.rs` в части H.323; deploy/README.md, deploy/vks.env.example, deploy/systemd/*, unit drop-in; конфиг VKS_H323_*.

ЦЕЛЕВОЕ СОСТОЯНИЕ (заявка заказчика)
Нативный Rust TCP-стек H.323 на порту 1720 (TPKT + Q.931 + H.225 + H.245), удаление Python-sidecar и `ffi.rs`; полная матрица аудио: G.711 A/u, G.722, G.722.1/Annex C (Siren14), G.719, G.723.1, G.726 16/24/32/40, G.728, G.729 A/B (двунаправленный), Opus, AAC-LATM; видео: H.261/H.263/H.263+/H.264 Baseline/Main/High/H.265/H.239; fastStart, reverse OLC, H.239 (второй поток презентации), H.235 (SRTP). Пример кода заказчика (derive_h235_srtp_key через X25519 + HMAC-SHA256, Q.931 Connect «как есть») — проверь криптографически и протокольно: это не H.235 и, вероятно, неверно.

ЧТО СДЕЛАТЬ
1. Точная матрица: что реально работает (Rust + Python), что mock/dead. Докажи file:line и, где можно, живыми фактами (ps/lsof/log, без изменений).
2. Проанализируй Python-сайдкар: какие ASN.1 сообщения pycrate кодирует/декодирует, какие call flow покрыты (Setup/CallProceeding/Alerting/Connect, H.245 TCS/MSD/OLC, fastStart, reverse OLC, H.239, UII DTMF), где реальный медиапуть. Это спецификация для нативной реализации.
3. Оцени реализуемость нативного стека: какой объём ASN.1 PER нужен, можно ли использовать готовые Rust-крейты (проверь crates.io/metadata), какие сообщения минимально необходимы для inbound/outbound G.711/G.722-вызова и H.264/H.239; предложи staged-план с golden-векторами: сгенерировать эталонные байты pycrate из живого venv (только чтение/запуск python-скрипта в /tmp) и проверять байт-паритет кодека.
4. Найди дефекты/регрессии/риски безопасности: неверная адресация H.245, утечки портов/процессов, таймауты, source-latch, DTMF clock, H.239, H.235 (в т.ч. почему пример заказчика негоден), анонимный доступ 1720, allowlist.
5. Матрица аудиокодеков: какие реально доступны в oxideav-* (посмотри ~/.cargo/registry/src/*/oxideav-*/) — G.729 encode/decode, G.722.1/C, G.719, G.728, G.723.1, Opus; чего нет и что делать (FFmpeg-мост/транскодер) без обмана в SDP.
6. Оцени «можно ли меньше кода»: что удалить (ffi.rs, python, мёртвые пути), что переиспользовать.
7. Дай пошаговый план P6 с оценкой строк/дней, тестами и критериями приёмки; честно раздели «сделаем сейчас» и «требует реального терминала».

ФОРМАТ ОТЧЁТА (docs/audit/2026-10-29/02_h323_p6.md)
§1 Вердикт. §2 Матрица состояние/цель (file:line). §3 Спецификация сайдкара (что переносить). §4 Дефекты P0/P1/P2 + минимальный фикс + тест. §5 Оценка примера H.235/Q.931 заказчика. §6 Кодек-матрица (реально доступно/нет). §7 План нативного стека staged (модули, строки, тесты, golden-байты). §8 «Меньше кода»/что удалить. §9 Риски и что нельзя доказать без терминала. §10 Оценка/10.
§11 «Спорные тезисы»: 5–8 ТЕЗИС / ДОКАЗАТЕЛЬСТВО / ЧТО ПЕРЕУБЕДИТ. Отстаивай позицию; Lead будет спорить.

Финальное сообщение ≤60 строк: вердикт, топ-7 находок, 5 тезисов, оценка/10.
```

## A3 — `codec-mcu` (id `7c882251-4386-4167-9eba-39587093e4eb`)

```text
Ты — саб-агент `codec-mcu` в команде модернизации VKS/PROSTOR MCU.

ОБЩИЙ КОНТЕКСТ
- /home/pro/Рабочий стол/mcu, master, HEAD d82ce4d, дерево чистое. Rust 1.94.0 (export PATH="$HOME/.cargo/bin:$PATH"), baseline 1425 passed/0 failed/8 ignored. Сеть есть.
- Живой сервис/БД/код не менять, не перезапускать.
- Единственный создаваемый файл: docs/audit/2026-10-29/03_codec_mcu.md. Read-only аудит, полные файлы через cat/read. Тяжёлые сборки не запускать.

ТВОЯ ЗОНА
`crates/vks-common/src/codec_capabilities.rs`; `crates/vks-gateway-sip/src/{codec,rtp_bridge,aac_latm,g722,g7231,g726,g728,h265,opus,uas}.rs`; `crates/vks-media-mcu/src/` целиком (compositor.rs, codec_worker/*, frame.rs, engine.rs, audio, lib.rs); `crates/vks-server/src/{gateway_audio,gateway_aac,gateway_video}.rs` в части кодеков/FFmpeg; `crates/vks-media-mcu/Cargo.toml`; oxideav-крейты в ~/.cargo/registry/src/*/oxideav-*/ (проверь реальные API/возможности).

ЦЕЛЕВОЕ СОСТОЯНИЕ (заявка заказчика — проверь критически)
1) Снять блокировку AAC-LATM (VKS_AAC_LATM_LEGAL_ACCEPTED), публичная матрица NATIVE_AUDIO/NATIVE_VIDEO включает G.729(A/B), G.722.1/C, G.719, G.726, G.728, G.723.1, Opus, AAC/MP4A-LATM; видео H.264 Main/High, H.265, VP8/VP9/AV1, H.261/263/H.263+.
2) Двунаправленный G.729 (PT18, 20 байт/20 мс) и нативные G.722.1/C, G.719; SDP a=sendrecv.
3) Симметричные профили H.264 (Baseline 42c028 / Main 4d0028 / High 640028) в ffmpeg-воркере; H.265 RFC 7798 (DON=0, repeat-headers, aud); извлечение h263p из AVI («00dc»/«00db»).
4) 2D билинейный скейлер YUV + быстрый memcpy-путь; oxideav-scene по умолчанию.
Примеры кода заказчика (NegotiatedH264Profile, extract_h263p_from_avi_chunk, scale_yuv_plane_bilinear) — проверь на баги: парсинг profile_idc/level, чётность/выравнивание AVI, индексация/округление билинейного фильтра, граничные случаи, переполнение u32/u64.

ЧТО СДЕЛАТЬ
1. Матрица: каждый кодек × (parse SDP / offer / answer / encode / decode / реальный мост) — статус и file:line. Отдельно: что доступно в oxideav (проверь API в registry src), что только через FFmpeg, что недоступно в принципе → нельзя объявлять.
2. Дефекты: несогласованные PT/clock/каналы, a=sendrecv vs sendonly, ложное объявление кодеков, паники/переполнения, неверный YUV stride, гонки/утечки FFmpeg-процессов, отсутствие fallback, дубли SR-билдеров/gain/RTP-циклов.
3. Для каждого пункта целевого состояния: минимальный элегантный патч (файл, строки) + тест; отметь, что уже сделано и не требует работы.
4. Оцени «меньше кода»: дубли/мёртвые абстракции (mock-кодеки, oxideav-обёртки), что удалить/слить.
5. Проверь инвариант «одна матрица возможностей — один источник истины»: где кодек-списки расходятся (settings/UI/SDP/answer/движок).
6. Проверь безопасность/лицензии: G.729/G.722.1 патенты — заказчик требует «снять блокировку», оцени риск и как оформить (флаг/документация) без юридического обмана.

ФОРМАТ ОТЧЁТА docs/audit/2026-10-29/03_codec_mcu.md
§1 Вердикт. §2 Матрица кодеков. §3 Дефекты P0/P1/P2 (симптом/причина/file:line/фикс/тест). §4 Разбор примеров кода заказчика (корректно/нет). §5 План по пунктам (строки/тесты/критерии). §6 «Меньше кода». §7 Риски и неизвестное. §8 Оценка/10.
§9 «Спорные тезисы» (5–8: ТЕЗИС/ДОКАЗАТЕЛЬСТВО/ЧТО ПЕРЕУБЕДИТ). Отстаивай позицию — Lead будет контраргументировать.

Финальное сообщение ≤60 строк: вердикт, топ-7 находок, 5 тезисов, оценка/10.
```

## A4 — `infra-l5l7` (id `d9f29012-367a-4b44-9377-0ca94575761a`)

```text
Ты — саб-агент `infra-l5l7` в команде модернизации VKS/PROSTOR MCU.

ОБЩИЙ КОНТЕКСТ
- /home/pro/Рабочий стол/mcu, master, HEAD d82ce4d, дерево чистое. Rust 1.94.0 (export PATH="$HOME/.cargo/bin:$PATH"), baseline 1425 passed/0 failed/8 ignored. Сеть к crates.io есть (можешь проверять версии/API, `cargo search`/curl).
- Живой сервис/БД/код не менять, не перезапускать.
- Единственный создаваемый файл: docs/audit/2026-10-29/04_infra_l5l7.md. Read-only аудит, полные файлы через cat. Тяжёлые сборки не запускать.

ТВОЯ ЗОНА
`crates/vks-signaling/src/{wt,ws,handler,envelope,session,sdp}.rs` + Cargo; `crates/vks-server/src/{lib,main,recorder,cluster,frontend}.rs` в части транспорта/записи/кластера/телеметрии; `crates/vks-recorder/` целиком; `crates/vks-storage/` (миграции + код); `crates/vks-auth/src/local_db.rs` + Cargo; `crates/vks-cluster/` целиком; configs/*, deploy/, .github/workflows/ci.yml; `crates/vks-admin/src/api_v3/*` в части videorecords.

ЦЕЛЕВОЕ СОСТОЯНИЕ (заявка заказчика — проверь критически)
1) Реальный WebTransport поверх QUIC (wtransport, self-signed, bi-streams + datagrams) вместо mock WS/WT; `ServerHello.room_mode`; обработка ошибок/таймаутов/лимитов.
2) vks-recorder: RecorderProfile `W·H·fps·bpp 0.085`, клампы; интеграция записи сессий и `/api/v3/videorecords`.
3) T10b: нормализация directory (roles/groups/accounts/addresses) миграцией PostgreSQL; перенос из JSON-документа T10a; согласованность со SQLite (dev) и существующими миграциями.
4) Argon2id (m=19456,t=2,p=1) в vks-auth с прозрачной проверкой старых pbkdf2-хешей (в примере заказчика fallback отсутствует — баг).
5) Кластерная шина NATS + OTLP-экспорт телеметрии.
6) L0: sysctl-профиль + systemd cgroup v2 (Slice/CPUWeight/MemoryHigh/Max/LimitNOFILE/LimitRTPRIO) для порта 40000/ICE/RTP.

ЧТО СДЕЛАТЬ
1. Матрица: реализовано/mock/отсутствует с file:line и доказательством.
2. Проверь примеры кода заказчика на корректность и совместимость: API wtransport (актуальная версия на crates.io — какая? пример с Endpoint::server/accept_bi может не соответствовать), argon2 verify, SQL-миграция (FK/типы/идемпотентность/rollback, SQLite vs PostgreSQL), NATS (какой крейт/версия, JetStream vs core, offline-поведение), OTLP (opentelemetry-otlp версия/совместимость с tracing-subscriber 0.3).
3. Дефекты/регрессии/безопасность: незакрытые эндпоинты, слабые секреты, отсутствие auth, DoS на QUIC/WS, потеря записей, несовместимость миграций с текущей схемой, двойные источники конфигурации, блокирующий I/O, утечки.
4. Для каждого пункта — минимальный элегантный план (файлы/строки/тесты) с точными версиями крейтов; отметь, что делать НЕ надо (например, если фича уже есть или требует прод-инфраструктуры).
5. «Меньше кода»: что удалить/слить (mock WT, дубли шин, лишние абстракции).
6. Проверь, какие зависимости уже в Cargo.lock (таблица), чего нет и какие версии нужно добавить; оцени риск добавления (MSRV/дубли/TLS-бэкенды).

ФОРМАТ ОТЧЁТА docs/audit/2026-10-29/04_infra_l5l7.md
§1 Вердикт. §2 Матрица. §3 Дефекты P0/P1/P2. §4 Разбор примеров заказчика. §5 План по пунктам (файлы/строки/тесты/версии). §6 «Меньше кода». §7 Риски/неизвестное. §8 Оценка/10.
§9 «Спорные тезисы» (5–8: ТЕЗИС/ДОКАЗАТЕЛЬСТВО/ЧТО ПЕРЕУБЕДИТ). Отстаивай позицию; Lead будет спорить.

Финальное сообщение ≤60 строк: вердикт, топ-7 находок, 5 тезисов, оценка/10.
```

## A5 — `frontend-sdk` (id `dc96c30f-ff37-42e0-9d18-8fa7c1fe8784`)

```text
Ты — саб-агент `frontend-sdk` в команде модернизации VKS/PROSTOR MCU.

ОБЩИЙ КОНТЕКСТ
- /home/pro/Рабочий стол/mcu, master, HEAD d82ce4d, дерево чистое. Rust 1.94.0 (export PATH="$HOME/.cargo/bin:$PATH"), baseline 1425 passed/0 failed/8 ignored. Node/npm есть, Playwright установлен в tests/e2e (тяжёлые e2e без нужды не запускай). Сеть есть.
- Живой сервис :8080 не перезапускать; БД/код не менять (read-only аудит).
- Единственный создаваемый файл: docs/audit/2026-10-29/05_frontend_sdk.md. Полные файлы через cat/read.

ТВОЯ ЗОНА
`client/pnm-frontend/deployed/pnm/` (index.html, perf-overrides.js, codian-layouts.js, theme-toggle.js, manifest.webmanifest, MANIFEST.sha256, assets/chunks), `client/pnm-frontend/` скрипты импорта/патчей (scripts/import-pnm-frontend.sh, patch-pnm-*.py); `crates/vks-server/src/frontend.rs` (SPA fallback, CSP, MANIFEST); `client/vks-client-sdk/` целиком (Cargo.toml, src/*, wasm); tests/e2e (структура, что покрыто); docs/DARK_THEME_PNM_PLAN.md, docs/rules/FRONTEND_RULES.md, ADR-0027/0030/0032.

ЦЕЛЕВОЕ СОСТОЯНИЕ (заявка заказчика — проверь критически)
1) Прямой переход /pnm/monitoring/sessions/<id> не рендерит сессию, пока не выбрать вручную; нужна гидратация прямой ссылки (пример: IIFE в perf-overrides.js с fetch('/api/v3/sessions'), window.__PROSTOR_INITIAL_SESSION__, CustomEvent 'prostor:select-session').
2) Старый акцент #0097A7 в index.html/manifest.webmanifest → #17212B, обновить MANIFEST.sha256 (сервер пересчитывает CSP-хэши).
3) WASM SDK: автоматическое переключение пайплайна по ServerHello.room_mode (mcu — один композит + пиксельные координаты; sfu — N потоков simulcast + CSS Grid/Taffy из vks-layout), пример с VksClientSession.
4) Ранее заявленные дефекты: 482/483 файлов из архива, грязное дерево — проверь, актуально ли.

ЧТО СДЕЛАТЬ
1. Матрица: каждый пункт — статус (сделано/нет/частично) с file:line и доказательством; проверь, не реализован ли уже прямой роутинг (SPA fallback frontend.rs, вендорский router, codian-layouts.js) и не конфликтует ли пример с существующей логикой (event names, store, auth, race).
2) Проверь пример perf-overrides: соответствует ли фактическому API (`/api/v3/sessions` shape/items, credentials, авторизация), работает ли до DOMContentLoaded у вендорского Vue, не ломает ли CSP (inline/fetch), что реально нужно для SPA; предложи минимальный корректный вариант (возможно, серверный SPA fallback уже решает).
3) Цвет: найди все вхождения #0097A7/#17212B (включая CSS/чанки/manifest), оцени, какой источник истины (переменная темы), как обновлять MANIFEST и CSP-хэши правильно; проверь, не перетирает ли импорт PNM правки (маркеры project-layer).
4) SDK: прочти conference.rs/types.rs/layout.rs/media_engine/*; проверь, есть ли room_mode/ServerHello, как выбирается mcu/sfu-рендерер, где дубли/мёртвый код; предложи минимальный WASM API и тесты (wasm-bindgen, отсутствие сети).
5) Дефекты/регрессии/безопасность: XSS/CSP, гонки, 401-редирект, устаревшие URL, утечки токенов в query, cache MANIFEST.
6) «Меньше кода»: что удалить (мёртвые JS-символы, перехваты calculate, дублирующие обработчики), где один источник истины.
7) Проверь, что реально можно проверить без браузера: node --check, unit-тесты, curl живого API (только GET, без мутаций), Playwright --list.

ФОРМАТ ОТЧЁТА docs/audit/2026-10-29/05_frontend_sdk.md
§1 Вердикт. §2 Матрица. §3 Дефекты P0/P1/P2 (симптом/причина/file:line/фикс/тест). §4 Разбор примеров заказчика. §5 План (файлы/строки/тесты). §6 «Меньше кода». §7 Риски/неизвестное. §8 Оценка/10.
§9 «Спорные тезисы» (5–8: ТЕЗИС/ДОКАЗАТЕЛЬСТВО/ЧТО ПЕРЕУБЕДИТ). Отстаивай позицию; Lead будет спорить.

Финальное сообщение ≤60 строк: вердикт, топ-7 находок, 5 тезисов, оценка/10.
```

## A6 — `hygiene-arch` (id `b34c7f54-fb19-433c-9603-8de087176e48`)

```text
Ты — саб-агент `hygiene-arch` в команде модернизации VKS/PROSTOR MCU.

ОБЩИЙ КОНТЕКСТ
- /home/pro/Рабочий стол/mcu, master, HEAD d82ce4d, дерево чистое, 506 коммитов, 16 крейтов. Rust 1.94.0 (export PATH="$HOME/.cargo/bin:$PATH"), baseline 1425 passed/0 failed/8 ignored. Сеть есть.
- Живой сервис/БД не трогать, не перезапускать. НИЧЕГО не удалять и не перемещать — только анализ и предложения.
- Единственный создаваемый файл: docs/audit/2026-10-29/06_hygiene_arch.md. Полные файлы через cat.

ЦЕЛЬ
Максимальная гигиена всей цепочки: мёртвый код, полные/конфликтующие файлы/директории/конфиги, дубли источников истины, лишние зависимости, устаревшие артефакты, рискованные настройки, несоответствия документации коду. И главный вопрос заказчика: «можно ли было реализовать задачу меньшим кодом — стремимся к элегантным реализациям».

ЧТО ПРОВЕРИТЬ (не ограничиваясь)
1) Мёртвый код Rust: `pub`-символы без потребителей вне крейта, mock-модули, закомментированные блоки, `#[allow(dead_code)]`, re-export'ы, feature-флаги, которые ничего не включают (oxideav и т.п.). Метод: grep по workspace + cargo metadata, без nightly-udeps (если доступен — используй). Дай точные file:line и оценку строк.
2) Мёртвый/дублирующий фронтенд-код: constructorCount, onConstructorActivate, перехваты calculate, неиспользуемые скрипты/чанки/патчи, старые сборки.
3) Зависимости: неиспользуемые в манифестах (fresh grep), дубли версий в Cargo.lock (rand/getrandom/thiserror и т.п.), тяжёлые/небезопасные, feature-раздувание; предложи точные правки. Проверь `cargo tree -d` (если быстро).
4) Конфиги и окружение: 44+ `env::var("VKS_...")` vs поля Config, несогласованные дефолты/порты (RTP 41000-41200 / ICE 43000-43100 / media 40000), `target-cpu=native` последствия, дубли systemd-юнитов, deploy/vks.env.example vs configs, CI gaps (.github/workflows/ci.yml), rust-toolchain/CI тулчейн.
5) Артефакты/мусор: target 55G, target-impl 5.9G, target-proto 4.7G, vks_data.db 3.2M, vks_server.log 1.2M, backups/, .m7/, scripts/__pycache__, *.pyc, stash@{0,1}, docs/audit measure бинари, .accept*; что игнорируется, что закоммичено зря, что противоречит .gitignore/README. Проверь `git ls-files | wc -l`, крупнейшие файлы в git (`git ls-files -z | xargs -0 du` top).
6) Документация vs код: MASTER_TASK_LIST/ARCHITECTURE/audit-отчёты содержат устаревшие статусы/числа тестов/обещания; найди 10 самых опасных расхождений (особенно «закрыто 9.5», а в коде mock).
7) Дубли источников истины в архитектуре: room_mode, геометрия/раскладки, кодек-матрица, размер раскладки, порты, конфиг, версии. На каждый — где должно быть одно место.
8) Безопасность/гигиена: анонимные эндпоинты (/metrics, /api/v3/mcu/farm, gateways/status, cluster/status), dev-секреты, 0.0.0.0, H.323 plaintext, SQLite в репо, логи с токенами/PII, allowlist.
9) «Меньше кода»: 7–10 конкретных предложений с оценкой экономии строк и риска (не трогая работающее поведение); приоритизируй.
10) CI: что не проверяется (ALL-Features? wasm? python? e2e? manifest parity? cargo-deny/audit?), какие джобы фиктивны/continue-on-error; точные правки.

ФОРМАТ ОТЧЁТА docs/audit/2026-10-29/06_hygiene_arch.md
§1 Вердикт. §2 Таблица мёртвого кода (путь:строки | строк | доказательство). §3 Зависимости/манифесты. §4 Конфиги/env/порты. §5 Артефакты и git-гигиена (с размерами). §6 Документация vs код (топ-10). §7 Дубли источников истины. §8 Безопасность. §9 «Меньше кода» (с оценкой). §10 CI-дыры. §11 План гигиены по приоритетам (что делать первым, что не трогать, риски). §12 Оценка/10.
§13 «Спорные тезисы» (5–8: ТЕЗИС/ДОКАЗАТЕЛЬСТВО/ЧТО ПЕРЕУБЕДИТ). Отстаивай позицию; Lead будет спорить. Не предлагай удалять работающие load-bearing модули без доказательства 0 ссылок.

Финальное сообщение ≤60 строк: вердикт, топ-10 находок с путями/размерами, 5 тезисов, оценка/10.
```

## A7 — `review-recent` (id `30c7d508-1377-4b51-8008-e00a107acd46`)

```text
Ты — саб-агент `review-recent` в команде модернизации VKS/PROSTOR MCU. Твоя роль — независимый строгий code reviewer активных изменений.

ОБЩИЙ КОНТЕКСТ
- /home/pro/Рабочий стол/mcu, master, HEAD d82ce4d, дерево чистое. Rust 1.94.0 (export PATH="$HOME/.cargo/bin:$PATH"). Baseline `cargo test --workspace --locked` = 1425 passed/0 failed/8 ignored.
- Живой сервис/БД/код не менять. Единственный создаваемый файл: docs/audit/2026-10-29/07_review_recent.md.
- Можно и нужно запускать: `git log/show/diff`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, точечные `cargo test -p ...`.

ЗАДАЧА
Проведи код-ревью последних ~30 коммитов (диапазон `722c807..HEAD`, при необходимости шире — `d039104..HEAD`), с фокусом на:
- S6/S7 раскладки (grids.rs, grid.rs, rect.rs, lib.rs, resources.rs, gateways.rs, shell.rs);
- e2e-тесты (tests/e2e) — не фиктивны ли, не подгоняют ли результат, threat model oracle;
- gateway fixes (8116855 DTMF/RTP rejection, 8d44b9c unique/size/apply status);
- hygiene-коммит 77e36a1 и CI 2e27225;
- docs-коммиты на предмет ложных/недоказанных утверждений (9.5 verdict, «0 пересечений», «214/214»).

ЧТО СДЕЛАТЬ
1) По каждому кластеру: что реально изменилось, корректно ли, есть ли регрессии/краевые случаи/паники/гонки/несоответствие тестов поведению.
2) Найди минимум 8 конкретных проблем (если есть): file:line, почему проблема, минимальная правка. Не выдумывай — если проблем нет, докажи.
3) Проверь качество тестов: ассерты, детерминизм, ignored-тесты, покрытие новых веток, отсутствие «зелёных» заглушек.
4) Проверь clippy/fmt; отдельно — предупреждения/ошибки, которые скрыты `#[allow]`.
5) Оцени каждую подсистему из списка по 1–10 с обоснованием; дай общий балл ревью.
6) Сформулируй 5–8 «спорных тезисов» для дебатов с Lead.

ФОРМАТ docs/audit/2026-10-29/07_review_recent.md
§1 Вердикт и общий балл. §2 Ревью по кластерам (что сделано/оценка/проблемы). §3 Список проблем P0/P1/P2 с file:line и фиксом. §4 Качество тестов. §5 fmt/clippy результаты (точные команды и вывод). §6 Оценки по подсистемам. §7 Спорные тезисы (ТЕЗИС/ДОКАЗАТЕЛЬСТВО/ЧТО ПЕРЕУБЕДИТ).
Отстаивай позицию: если считаешь коммит плохим — докажи; если Lead не согласен — требуй контрдоказательств. Не ставь 9.5 без оснований.

Финальное сообщение ≤60 строк: общий балл, топ-8 проблем с file:line, 5 тезисов.
```

## A8 — `live-runtime` (id `79fa89b2-ed79-4d45-b140-9b18ba4ed309`)

```text
Ты — саб-агент `live-runtime` в команде модернизации VKS/PROSTOR MCU. Твоя роль — анализ регрессий по живому рантайму и артефактам, независимая проверка фактов.

ОБЩИЙ КОНТЕКСТ
- /home/pro/Рабочий стол/mcu, master, HEAD d82ce4d, дерево чистое. Rust 1.94.0 (export PATH="$HOME/.cargo/bin:$PATH").
- Живой сервис: systemd --user unit vks-mcu, PID на :8080, release-бинарь; Python H.323 sidecar на :1720. НЕ перезапускать, НЕ убивать, БД не менять (только read-only sqlite `mode=ro`, GET-запросы).
- Единственный создаваемый файл: docs/audit/2026-10-29/08_live_runtime.md. Можно писать временные файлы в /tmp.

ЧТО ИЗУЧИТЬ
1) Провенанс: сравнить mtime/размер/sha256 release-бинаря с исходниками (какие .rs новее бинаря), версия rustc, соответствие HEAD. Проверить, крутится ли бинарь из HEAD или старее.
2) Логи vks_server.log (1.2 МБ, не коммитить, читать) — найти аномалии/регрессии: ошибки, паники, «codec/PT mismatch», отказы RTP, таймауты, утечки ffmpeg-процессов, рост памяти; сгруппировать по типам, посчитать частоты, привязать к коду (file:line).
3) Процессы/порты: ps/lsof/ss — висящие ffmpeg/python, число потоков, open files, RSS; сравнить с ожиданиями.
4) Живые GET-проверки (только чтение, без мутаций): /healthz, /readyz, /metrics, /api/v3/gateways/status, /api/v3/mcu/farm, /api/v3/cluster/status — с токеном и без; проверить анонимный доступ/безопасность; /pnm/ и MANIFEST (sha256sum -c), CSP-заголовки.
5) SQLite read-only: схема, integrity_check, размеры таблиц, дубли/мусор (раскладки/сессии/адреса), незакрытые сессии, WAL; не изменять.
6) Конфиги: configs/default.toml, deploy/*, unit/drop-in — сверка с фактическим процессом (/proc/<pid>/environ, /proc/<pid>/cmdline), найти расхождения «заявлено vs реально» и небезопасные дефолты.
7) Регрессии: сопоставить последние коммиты (S6/S7/layout/gateway/e2e) с фактическим поведением и evidence; найти утверждения, которые живой рантайм опровергает (например «MANIFEST 214/214», «role везде», «service на новом бинаре»).
8) Составить список «что сломается/что проверить после наших изменений»: порты, рестарт, миграции БД, sidecar, CSP, wasm.

ФОРМАТ docs/audit/2026-10-29/08_live_runtime.md
§1 Снимок рантайма (PID/sha/mtime/uptime/env) и provenance-вердикт. §2 Логи: топ-аномалий с частотами и file:line. §3 Процессы/ресурсы. §4 Живые endpoint-проверки (что реально ответило). §5 SQLite снимок. §6 Конфиги vs факт. §7 Регрессии/ложные утверждения. §8 Риски для наших изменений и план проверки. §9 Оценка/10.
§10 «Спорные тезисы» (5–8: ТЕЗИС/ДОКАЗАТЕЛЬСТВО/ЧТО ПЕРЕУБЕДИТ). Отстаивай позицию; Lead будет контраргументировать.

Финальное сообщение ≤60 строк: provenance-вердикт, топ-8 аномалий, 5 тезисов, оценка/10.
```

## A2b — `h323-p6-v2` (id `68e63f07-6a7d-481e-99d4-76122508a393`)

```text
Ты — субагент анализа H.323 для VKS/PROSTOR MCU. Рабочая директория: /home/pro/Рабочий стол/mcu (quote paths). Только чтение; единственный файл, который можно создать: docs/audit/2026-10-29/02_h323_p6.md. НЕ трогай сервис/БД/конфиги/код, не перезапускай.

ВАЖНО — ПРОТОКОЛ ВЫЖИВАНИЯ: в первые 5 минут создай файл-скелет (заголовок + §1 предварительный вердикт + §5 план-заготовка) и сразу запиши; затем дополняй его по мере работы. Если время/контекст кончаются — у тебя уже есть файл с максимумом проверенного. Закончи не позже 20 минут.

ЦЕЛЬ: аудит пункта P6 целевой матрицы — «нативный Rust H.323-стек, удалить Python-сайдкар и ffi.rs, полная кодек-матрица (G.728/G.729/G.722.1C/G.719/Opus/AAC-LATM), fastStart, reverse OLC, H.239, H.235 SRTP».

ЧТО УЖЕ УСТАНОВЛЕНО LEAD'ом (проверь и дополни, не повторяй впустую):
- crates/vks-gateway-h323/src/{lib,ffi,q931,codec,call,uac,uas}.rs; q931.rs — только TPKT/Q.931 framing; lib.rs прямо говорит: H.225/H.245 ASN.1 декодирует sidecar. uac.rs/uas.rs — супервизоры Python-процесса (stdin/stdout JSON), не ASN.1.
- python/: h323_sidecar.py 747, h323_listener.py 1224, h323_asn.py 16871 (pycrate), schemas/{h225,h235,h245}.asn (2326/311/4198 строк) — схемы полные, лежат в репо.
- Live: H.323 sidecar PID 1814221 на TCP 1720, READY, allowlist 192.168.1.0/24; живой бинарь сервера на 45 коммитов позади HEAD (deleted inode).
- FEASIBILITY-ФАКТ Lead'а (перепроверь и оцени): установлен rasn-compiler 0.16.0 CLI; схемы после iconv WINDOWS-1252→UTF-8 компилируются в Rust-биндинги rasn 0.29 (out.rs 555 КБ, ~? строк), типы H323UserInformation/H323UUPDU/SetupUUIE/TerminalCapabilitySet/OpenLogicalChannel/MasterSlaveDetermination/MultimediaSystemControlMessage присутствуют. НО сгенерированный одним файлом код не компилируется: 98 ошибок «cannot find type NonStandardParameter», кросс-модульные импорты не сгенерированы. Проверь, лечится ли это RasnBackend Config { default_wildcard_imports: true } (я не успел), и оцени: build.rs-кодогенерация vs вендоринг 555 КБ. Если кодогенерация проходит — нативный стек реален; если нет — hand-written UPER.
- Известно: pycrate sidecar использует только 2 верхнеуровневых ASN.1 типа (H323_UserInformation, MultimediaSystemControlMessage) и ~8 сообщений (Setup/CallProceeding/Alerting/Connect/ReleaseComplete/TCS/MSD/OLC/CLC), см. h323_sidecar.py:282-560, h323_listener.py:339-710.
- Целевой объём: удаление Python — это перенос конечного автомата H.225/H.245 (~2000 строк Python) на Rust + ASN.1 PER. Оцени честно: что реально в одну итерацию, что staged.

ВОПРОСЫ, НА КОТОРЫЕ ОТВЕТИТЬ (с file:line и командами):
1) Матрица: какие H.323-возможности есть в Rust сейчас, какие в Python, какие отсутствуют; где fastStart/reverse OLC/H.239/H.235/SRTP; какие кодеки реально согласуются (G.711/G.722/G.728/G.729/G.723.1/Opus/AAC-LATM/H.264/H.263/H.265), где ложные объявления.
2) ASN.1-стратегия: rasn-кодогенерация (проверь компиляцию с wildcard imports, замерь размер/время) vs ручной UPER. Дай рекомендацию с критерием «меньше кода».
3) План staged: что делать в первую очередь (native outbound UAC? inbound UAS?), как получить golden-вектора из живого pycrate venv (~/.local/share/vks-mcu/h323-venv), как проверять без H.323-терминала (фикстуры pycrate), когда можно удалять Python/ffi.
4) H.239 (BFCP? H.239 — отдельный ASN.1 H.239 channel) и H.235 (SRTP key derivation, DH): реализуемо ли честно, что документировать как отложенное.
5) Спорные тезисы (§9, 5-8 штук) с «что меня переубедит».
6) Оценка зоны /10 (соответствие заявке, качество кода, готовность плана) + оценка трудоёмкости в LOC/днях.

Формат файла: §1 Вердикт, §2 Матрица возможностей, §3 Дефекты P0/P1/P2, §4 Разбор кода sidecar (карта FSM), §5 План (staged, файлы, тесты, зависимости/версии), §6 «Меньше кода», §7 Риски, §8 Оценка /10, §9 Спорные тезисы. Каждый факт — с file:line или командой. В конце короткое резюме. Писать по-русски.
```

## A5b — `frontend-sdk-v2` (id `ba39778c-3da1-4470-b16c-4173f76f7343`)

```text
Ты — субагент анализа фронтенда PNM, WASM SDK и L0-хоста для VKS/PROSTOR MCU. Рабочая директория: /home/pro/Рабочий стол/mcu (quote paths, в пути пробел и кириллица). Только чтение; единственный создаваемый файл: docs/audit/2026-10-29/05_frontend_sdk.md. НЕ трогай сервис/БД/код/frontend-файлы.

ПРОТОКОЛ ВЫЖИВАНИЯ: в первые 5 минут создай файл-скелет (§1 вердикт + §5 план) и сразу запиши; дальше дополняй. Если контекст кончается — файл уже есть. Закончи не позже 20 минут.

ЦЕЛЬ: аудит пунктов матрицы:
A) PNM-фронтенд: прямая гидратация `/pnm/monitoring/sessions/<id>` (deep link), тема `#0097A7`→`#17212B`, MANIFEST/CSP целостность.
B) WASM SDK `client/vks-client-sdk`: `ServerHello.room_mode` как единственный источник истины, режимы SFU/MCU, wasm32-сборка, CSS/Taffy layout-дескрипторы для SFU.
C) L0-хост: sysctl + systemd cgroup v2 (Slice/CPUWeight/MemoryHigh/MemoryMax/LimitNOFILE/LimitRTPRIO), проверяемость.

ЧТО УЖЕ УСТАНОВЛЕНО Lead'ом (проверь/дополни):
- deployed/pnm/index.html 74 989 B, theme-color #0097A7 (строка ~37); assets/manifest.webmanifest theme_color #0097A7; CSS-переменные уже #17212B. MANIFEST.sha256 лежит в client/pnm-frontend/MANIFEST.sha256 (22 078 B), 214 файлов, sha256sum -c = 214 OK на диске. `/pnm/MANIFEST.sha256` по HTTP отдаёт index.html (SPA fallback), не манифест.
- perf-overrides.js 11 685 B: XHR-кэш кодеков, прямой гидратации сессии нет; codian-layouts.js 68 124 B; chunks/; vendor SPA роутер в index.html (нужно найти, как он матчит /monitoring/sessions/<id> и есть ли fetch /api/v3/sessions/<id>).
- Сервер: frontend.rs SPA-fallback для /pnm/*, CSP sha256 4 inline-скриптов, security headers; /pnm/ 200.
- SDK client/vks-client-sdk/src/: lib.rs 24, conference.rs 916, signaling_client.rs 396 (ServerHello с room_mode: Option<RoomMode> в handler.rs:288), media_engine/mod.rs уже имеет RoomMode{Sfu,Mcu} + switch_mode, layout.rs 105, types.rs. CI: cargo check -p vks-client-sdk --target wasm32-unknown-unknown — проходит (проверено Lead'ом); wasm-bindgen/JS bindings отсутствуют.
- vks-signaling handler.rs: server_hello строит room_mode статически из глобального режима (на старте), per-room override не доезжает; switch_room_mode в SDK не вызывается из продакшна.
- L0: deploy/systemd/ (юниты vks-mcu.service dev и vks-mcu-prod.service), deploy/vks.env.example; в коде нет sched_setscheduler/SO_RCVBUF/mlockall (grep 0) — LimitRTPRIO/rmem_max без правок кода бесполезны.

ВОПРОСЫ (отвечай с file:line и командами):
1) Deep link: как vendor SPA обрабатывает /pnm/monitoring/sessions/<id>? Найди роутер (index.html/chunks/codian-layouts.js/perf-overrides.js), функцию рендера страницы сессий, откуда берётся id (location.pathname? hash?), какие XHR уходят. Есть ли `GET /api/v3/sessions/<id>`? Как авторизация (Bearer в localStorage? cookie?) — т.е. возможен ли прямой fetch из inline-скрипта. Предложи минимальный патч (~20-40 строк) БЕЗ переписывания vendor-кода, с тестом/проверкой (Playwright live.js или perf-cache.js). Учти CSP (inline-script хеши) и MANIFEST.
2) Тема: где ещё #0097A7 (grep по deployed/pnm и client/pnm-frontend), что сломается при замене на #17212B, как пересчитать MANIFEST и CSP-хеши, какие тесты (node --check? python patch-тесты? manifest parity?). Предложи гейт в CI против возврата цвета.
3) SDK: как room_mode доходит до MediaEngine; нужен ли per-room override через ServerHello + JoinRoomResult; что добавить для wasm (wasm-bindgen обёртка? или только cargo check?), как отдавать CSS/Taffy-дескрипторы раскладок (vks-layout уже имеет CSS grid descriptors? проверь); что реально нужно, чтобы SDK был «WASM-ready» честно.
4) L0: какие sysctl/cgroup-настройки дадут эффект при текущем коде (SO_RCVBUF через socket2, sched_setscheduler, MemoryHigh), какие — театр; предложи минимальный deploy-набор + критерий проверки (sysctl --system, systemctl show, getsockopt).
5) Спорные тезисы (§9, 5-8) с «что меня переубедит».
6) Оценка зоны /10 по каждому из A/B/C + трудоёмкость LOC.

Формат: §1 Вердикт, §2 Факты/матрица, §3 Дефекты P0/P1/P2, §4 Frontend deep-link/theme/manifest, §5 SDK/CSS/room_mode, §6 L0, §7 План (файлы/строки/тесты), §8 «Меньше кода», §9 Спорные тезисы, §10 Оценка /10. Русский язык.
```

## D1 — `debate-media` (id `6b9757fb-d7f4-4228-9f6f-74cb4da1d4d1`)

```text
Ты — оппонент Lead'а в технической дискуссии по VKS/PROSTOR MCU (рабочая директория /home/pro/Рабочий стол/mcu). Не пиши файлов, не меняй код/сервис. Прочитай отчёты: docs/audit/2026-10-29/01_sfu_p3p5.md, 03_codec_mcu.md, 08_live_runtime.md (можно выборочно §1/§9, но проверяй факты по коду при сомнении). Твоя задача: защищать позиции отчётов или признавать правоту Lead'а; в конце — сводка согласия и итоговые изменения плана. Ответь одним сообщением ≤120 строк, по пунктам: AGREE / COUNTER (с доказательством file:line) / COMPROMISE (формулировка).

Позиции Lead'а для дискуссии:
1) База — str0m 0.24.1, не свой RTP-стек. Согласен удалить мёртвую аналитику (bwe/congestion/simulcast ~1000 строк) и дубли dtls/ice/codec (~500), НО легаси-плоскость (SfuEngine+udp_loop+forwarder) предлагаю не удалять целиком, а оставить под cfg(test) как эталон/тестовый harness, если браузерный путь всё равно только str0m (браузер не умеет plaintext RTP). Вопрос: есть ли у легаси-плоскости хоть один реальный потребитель вне тестов? Если нет — какой объём можно удалить без потери покрытия (какие тесты на ней держатся и чем заменить)?
2) Целевая матрица заказчика (3-слойный simulcast, SVC, XOR-FEC 10:1, адаптивный JB) содержит невыполнимое/вредное. Моя позиция: (a) 3-слойный simulcast через rid — реализовать (это реально в str0m? проверь MediaData.rid/API); (b) SVC — не реализовывать, задокументировать; (c) XOR-FEC — не реализовывать на WebRTC (str0m не умеет, RFC 5109 браузеры не обязаны), оставить NACK/RTX + Opus RED/in-band FEC; (d) адаптивный JB — не делать в SFU, только шлюзовое плечо. Возрази, если считаешь иначе; предложи, как честно закрыть пункт матрицы (ADR/документ об отказе vs реализация).
3) P0, которые я беру в работу: per-source egress (снять MAX_PEERS_PER_ROOM=2), keyframe-gate, re-offer без нового DTLS, room_mode как производная (composite ⇔ есть SIP/H.323; hello на соединение), P5 composite-egress для браузера + аудио-мост, снятие 9 guard'ов. Проверь: per-source egress — это переписывание mid/clock-domain логики (R:30-32,513-519); реально ли за 1 задачу без регрессии 1:1? Может, проще: room_mode=MCU при наличии шлюза, браузер получает композит; в чисто-SFU комнате до 2 источников? Дай минимальный корректный дизайн.
4) Кодеки: (a) H.264 Main/High симметрия — P0, чинить в первую очередь; (b) AAC cpresent fail-open — чинить перед снятием флага; (c) G.729 — добавлять oxideav-g729, но объявлять только после ITU-векторов (какие вектора доступны?); (d) G.722.1/C и G.719 — не объявлять, задокументировать как infeasible; (e) единый CodecSpec вместо 6 списков; (f) свой билинейный скейлер ~150 строк, oxideav-scene не подключать; (g) H.263+ AVI-демукс — либо подключить payloader RFC 4629+offer (FFmpeg h263p encoder есть), либо удалить мёртвый код. Что из этого — обязательный минимум, что можно срезать?
5) Live: provenance подтверждён; моя позиция — финальный шаг: rebuild из HEAD, backup БД, рестарт, проверка 401/403 + sha. Согласен? Какие 3 проверки обязательны до/после?
6) Приоритеты на 10 задач по всему стеку: назови 3 самые ценные для продукта и 3, которые можно отложить/свести к документу.

Не поддакивай: если Lead неправ — докажи. Если прав — зафиксируй согласие и точную формулировку изменения плана.
```

## D2 — `debate-infra-hygiene` (id `eecf9575-e8d7-4d28-b179-b4582d4b3c30`)

```text
Ты — оппонент Lead'а в технической дискуссии по VKS/PROSTOR MCU (рабочая директория /home/pro/Рабочий стол/mcu). Не пиши файлов, не меняй код/сервис. Прочитай отчёты: docs/audit/2026-10-29/04_infra_l5l7.md и 06_hygiene_arch.md (§1/§9-§11 достаточно, факты проверяй по коду). Защищай позиции отчётов или признавай правоту; ответь одним сообщением ≤120 строк: AGREE / COUNTER (file:line) / COMPROMISE.

Позиции Lead'а:
1) WebTransport: заказчик требует реальный QUIC, поэтому делаем opt-in wtransport 0.7.2 + общий `EnvelopeTransport` + общий signaling_session; интеграционный тест нативным wtransport-клиентом (ClientHello→ServerHello); MockWt удаляем; браузерный сценарий документируем как требующий serverCertificateHashes (вне скоупа). Достаточно ли этого для честного «WT реализован»? Предложи точный минимальный набор (порт/сертификат/Alt-Svc/лимиты) и тесты.
2) Запись: моя позиция — в MCU-режиме tap уже кодируемого H.264 композитора, без скрытой viewer-группы; в SFU-режиме запись не поддерживается (501/409 с честным сообщением) до появления hidden viewer. Это меньше кода и проверяемо. Возрази/согласись; если согласен — как проверить (ffprobe на реальном файле) и какие P0 закрыть (Storage no-op, avcC, Annex-B→length-prefixed, timescale/stco).
3) T10b: directory уходит из ResourceState в SQL в том же изменении; layouts/links/history остаются в JSON-документе; старый ключ document.directory больше не читается, resurrection невозможен. Нужен ли schema_state или достаточно идемпотентных INSERT ... ON CONFLICT DO NOTHING? Какие таблицы/индексы минимальны, чтобы покрыть users/roles/groups/addresses/call_strings? Дай финальный список и порядок миграции данных.
4) Argon2id: m=19456,t=2,p=1 + семафор 2 + lazy rehash + длина пароля ≤128. При MemoryHigh=75% (~?) на стенде — безопасно? Или снизить m до 12288? Нужен ли глобальный rate-limit логина (сейчас per-(login,ip))?
5) NATS/OTLP: моя позиция — feature-gated NatsTransport + LocalTransport по умолчанию, CI-сервис nats:2.10-alpine с e2e переезда lease, OTLP под feature + тест с in-memory exporter + deploy/README с примером collector; без docker-compose. Переубеждает ли это тезис T5 «непроверяемо»? Если нет — что ещё обязательно.
6) Гигиена P0: mock LDAP (принимает любой пароль), sync-ldap, MP4-контейнер, oxideav-фича-пустышка, ARCHITECTURE-док. Что обязательно в первую очередь? Что из dead code (transparency.rs 447, qoe/slo/ratelimit 504, ffi.rs 279) можно удалить без риска — подтверди grep'ом сейчас.
7) CI: --all-features test (я уже нашёл 2 падающих теста vks-media-sfu при --all-features: codec::tests::default_features_include_opus_and_vp8, codec_names_for_caps — из-за cfg-фич), doc broken links, дубль python-теста, e2e job, cache key. Какие 5 правок максимально ценны?
8) Артефакты: 65.6 ГБ (target-impl 5.9, target-proto 4.7, target 55), 232 МБ measure, 6.29 МБ legacy-чанков в git, 4.1 МБ evidence, 2 stash'а. Что удалять/чистить и с какой проверкой (git clean -ndX, sha до/после)? Риски для сборки/CI.
9) Приоритеты: какие 3 задачи из инфры/гигиены нельзя резать, какие 3 можно свести к документу. «Меньше кода»: согласен на −430 (infra) и −1500 (hygiene), но перечисли, что реально берём в план.
```

## D3 — `debate-review` (id `ffa37d84-4bf9-4acb-9f44-ea7708d87a26`)

```text
Ты — оппонент Lead'а в технической дискуссии по VKS/PROSTOR MCU (рабочая директория /home/pro/Рабочий стол/mcu). Не пиши файлов, не меняй код/сервис. Прочитай docs/audit/2026-10-29/07_review_recent.md (§1, топ-находки, Т1-Т8, приложения с доказательствами) и проверь спорные факты по коду. Ответь одним сообщением ≤120 строк: по каждому пункту AGREE / COUNTER (file:line/команда) / COMPROMISE (формулировка).

Позиции Lead'а:
1) DTMF E-бит (gateway_audio.rs:458): фиксим — E=1 только в последнем пакете; тест должен проверять провод (реальный 16-байтовый пакет, длительность 200 мс = 10 пакетов), а не константу с константой. Согласен? Дай точную формулировку теста и как убедиться в отсутствии регрессии (какие ещё DTMF-тесты есть).
2) preset_default_13: calculate → 400, но preset::geometry строит 13 слотов, и e2e закрепляет 400 (vendor400Count===1). Моя позиция: 400 — баг сервера; чинить calculate+shell, e2e-оракул ОБНОВИТЬ (ожидать 200/13 слотов), а не подгонять сервер под тест. Возрази: может ли 400 быть намеренным (preset не должен быть ручным/вычисляемым)? Проверь, кто клиентски вызывает calculate и что UI делает с preset_default_13; если 400 намеренный — обоснуй и предложи, как убрать ложный оракул.
3) S6-центрирование (grid.rs:89 → engine.rs:714): 5 участников в 1280x720 дают 16.7% чёрного (fill 83.3%). Моя позиция: для динамической gallery-сетки растягивать строки/столбцы по высоте (fill=100%), Codian-пресеты (golden 54) не трогать, добавить тесты движка на n%cols!=0 и обновить ожидания. Альтернатива — оставить центрирование и задокументировать. Что выбираешь и почему (продукт: MCU-композит)?
4) resources.rs:374 vs 389/1432: две иерархии size (attach size→preset→slots vs validate preset→size→slots) и recalc не вызывается при загрузке БД. Моя позиция: единый порядок preset→размер→слоты, recalc при загрузке/attach, тест на legacy {preset,size}; тест :2495 переписать. Согласен? Какие ещё места затронуты?
5) P2-находки: WASTE_WEIGHT=0.0 мёртвая ветка — удалить параметр/ветку или оставить с комментарием; doc-комментарий парсера приклеился к константе — переставить; eager get_or_insert(await) — исправить на ленивое; `let _ =` на must_use — логировать; position==0→409 vs 422 — унифицировать на 422. Что ещё из P2 стоит взять (максимум 5)?
6) Docs-честность: BACKEND_RULES два «### 4.3»; 00_plan_review «49/49» (на самом деле test-бинарники); «9.5 PASS» самооценкой. Моя позиция: исправить структуру, переписать формулировки, в итоговом отчёте дать честные числа (1425/0/8 на baseline; после задач — новые). Нужен ли отдельный «review artifact»-шаблон, чтобы 9.5 больше не было самооценкой?
7) E2E в CI: добавлять job с Playwright (live.js требует живой сервер — какой сьют безопасен для CI: статические тесты форм/лейаутов без live?) или ограничиться node --check + manifest? Что реально можно гонять в CI без живого стенда?
8) Приоритеты: какие 3 находки из 13 обязательны к исправлению сейчас, какие можно отложить; что из этого должно попасть в тот же коммит, что и соответствующая фича (DTMF E-бит рядом с audio-задачей; preset_default_13 рядом с layout-задачей).
```
