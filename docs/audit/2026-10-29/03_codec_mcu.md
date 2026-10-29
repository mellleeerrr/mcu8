# 03 — Кодеки и MCU: аудит матрицы, мостов и целевого состояния заказчика

- **Роль:** саб-агент `codec-mcu`
- **Дата:** 2026-10-29, master, HEAD `d82ce4d`, дерево чистое
- **Зона:** `vks-common/codec_capabilities.rs`; `vks-gateway-sip/{codec,rtp_bridge,aac_latm,g722,g7231,g726,g728,h265,opus,uas}.rs`; `vks-media-mcu/**`; `vks-server/{gateway_audio,gateway_aac,gateway_video}.rs` (кодечная часть); `vks-media-mcu/Cargo.toml`; oxideav-крейты в `~/.cargo/registry/src/*/oxideav-*`
- **Метод:** чтение всех файлов зоны целиком; точечная проверка API oxideav-крейтов в registry (версии, `lib.rs`, наличие `encode`/`decode`/`scale`); `ffmpeg -encoders/-decoders/-muxers/-demuxers` на этом хосте; `cargo test -p vks-gateway-sip --lib --offline` → **148 passed / 0 failed / 1 ignored**.
- **Чего не делал:** не трогал живой сервис/БД, не перезапускал, не запускал тяжёлых сборок (кроме одного прогона юнит-тестов уже собранного крейта), не менял ни один файл, кроме этого отчёта. E2E-проверки с реальным SIP-терминалом не было — все выводы статические + юнит-тесты.

---

## §1 Вердикт

**Коротко: матрица «честная, но разорванная на шесть списков»; из четырёх пунктов заказчика два выполнимы дешёво, один (H.264-симметрия) — средняя работа с одним P0 в оффере, один (G.722.1/C + G.719) — невозможен без написания кодека; уже сделаны и не требуют работы H.265 (DON/aud/repeat-headers) и LOAS/90 кГц; найдено 2 × P0, 7 × P1, 10 × P2.**

Что действительно в порядке:

1. **Native-аудио мост работает для 8 кодеков** (PCMU/PCMA, G.722, G.726-16/24/32/40, G.728, G.723.1, Opus) — не «объявлено», а реально декодируется и кодируется в процессе: `crates/vks-gateway-sip/src/{g722,g7231,g726,g728,opus}.rs` + `crates/vks-server/src/gateway_audio.rs:41-194`.
2. **H.265/RFC 7798 сделан полностью и качественно** — NAL-заголовок, FU (49), AP (48), MTU 1200, границы последовательностей, DON: `crates/vks-gateway-sip/src/h265.rs` + тест `crates/vks-server/tests/h265_rtp.rs`.
3. **Запрет на ложное объявление уже встроен** — `strict-first` в `rtp_bridge.rs:1326-1351` и `488` в UAS (`uas.rs:196-262`) вместо «соединено, но тишина». Это редкая зрелость для шлюза.
4. **AAC-LATM мост технически готов** (LOAS strip/add, SMC каждые 100 мс, 90 кГц, каденция 1024/960): `aac_latm.rs`, `codec_worker/latm.rs`, `gateway_aac.rs`. Его блокирует **только** флаг `VKS_AAC_LATM_LEGAL_ACCEPTED` (`main.rs:390`).

Что не так с целевым состоянием (критически):

| Пункт заказчика | Реальность |
|---|---|
| Публичная матрица включает G.729, G.722.1/C, G.719 | **G.722.1/C и G.719 невозможно объявить**: нет ни реализации в oxideav (проверил registry: крейтов `oxideav-g7221`/`g719` не существует), ни энкодера/декодера в FFmpeg на этом хосте (`ffmpeg -encoders/-decoders` — пусто). Это не «блокировка», а отсутствие кодека. G.729 возможен, но **только** через `oxideav-g729` (в FFmpeg только декодер). |
| VP8/VP9/AV1 в NATIVE_VIDEO | SIP-терминалы их не говорят, payloader'ов нет, в FFmpeg они есть, но к SIP-мосту отношения не имеют. Это разрез **транспорта** (WebRTC), а не кодирования. Добавление в `NATIVE_VIDEO` сделает публичную матрицу ложной. |
| H.264 Baseline/Main/High симметрично | Сейчас: offer умеет Baseline+High (`rtp_bridge.rs:440-458`), answer умеет **только** Baseline (`rtp_bridge.rs:1284`), энкодер жёстко baseline (`gateway_video.rs:128-130`). Итог: `full`-профиль **уже сегодня** обещает High (PT98), ответ которым мост отвергает → «соединено без видео» (P0, §3 D1). |
| H.265: DON=0, repeat-headers, aud | **Уже сделано**: `-x265-params ... aud=1:repeat-headers=1` (`codec_worker/ffmpeg.rs:1101`, `gateway_video.rs:44`), `h265_don_compatible` отвергает `sprop-max-don-diff ≠ 0` (`rtp_bridge.rs:1215`). |
| h263p из AVI («00dc»/«00db») | Извлечение **уже написано** (`ffmpeg.rs:1132-1185`), но это **мёртвый код**: нет payloader'а H.263, нет строки в `VIDEO_OFFER`, нет моста. Ни один продакшн-вызов не доходит до `CodecWorker::VideoEncode(H263Plus)`. |
| 2D билинейный скейлер | Отсутствует. Есть только nearest + memcpy (`compositor.rs:176-213`, `gateway_video.rs:1058-1140`). |
| oxideav-scene по умолчанию | **Не обосновано и вредно**: `oxideav-scene 0.1.4` не содержит ни `scale`, ни `resize`, ни `bilinear` (проверил `src/render.rs`, `raster.rs`, `raster_renderer.rs`, `paint.rs`), зато тянет `oxideav-raster`/`oxideav-scribe`/`oxideav-ttf`. Сейчас это только re-export (`vks-media-mcu/src/lib.rs:64-66`). |
| Снять блокировку AAC-LATM | 1 строка дефолта + документация — но **сначала** надо починить латентный дефект `cpresent` (§3 D2), иначе снятие флага даёт 200 OK и тишину. Юридически честная форма — «профиль лицензий, принятый оператором», а не «патентно-чисто». |

**Оценка зрелости кодечной подсистемы: 7/10.** Payload-слой и «честность» объявлений — на 9; матрица возможностей, профили H.264 и AAC-гейт — на 5.

---

## §2 Матрица кодеков: parse SDP / offer / answer / encode / decode / реальный мост

Легенда: ✅ работает · ⚠️ работает с оговоркой · ❌ нет · 🚫 нельзя объявлять (нет кодека/лицензии). «oxideav» — проверено в registry `~/.cargo/registry/src/*/`; «FFmpeg» — проверено на этом хосте.

### Аудио

| Кодек (PT) | parse SDP | Offer | Answer | Encode | Decode | Реальный мост | oxideav (registry) | FFmpeg здесь | Итог |
|---|---|---|---|---|---|---|---|---|---|
| PCMU (0) | ✅ `uas.rs:577`, `rtp_bridge.rs:1158` | ✅ `rtp_bridge.rs:288` | ✅ | ✅ `gateway_audio.rs:175` | ✅ `:144` | ✅ in-process G.711 (свой ulaw/alaw) | `oxideav-g711 0.0.8` (не используется — свой код) | — | ✅ native |
| PCMA (8) | ✅ | ✅ `:289` | ✅ | ✅ | ✅ | ✅ | то же | — | ✅ native |
| G.722 (9, clock **8000**) | ✅ `uas.rs:580` | ✅ `:290` clock 8000 | ✅ | ✅ `g722.rs:33` | ✅ `:42` | ✅, 160 Б/20 мс, ts+160 | `oxideav-g722 0.0.7` — реальный SB-ADPCM | — | ✅ native |
| G.726-16/24/32/40 (107-110) | ✅ `uas.rs:582-585`, `g726.rs:33-50` | ✅ `:355-358` | ✅ | ✅ `g726.rs:150` | ✅ `:180` | ✅ LSB-first по RFC 3551 (`g726.rs:105-110`), 20/40 мс | `oxideav-adpcm 0.0.7` (`g726`) | `g726le` совпадает с нашим packing | ✅ native, **кроме PT2** (§3 D5) |
| G.728 (15) | ✅ `uas.rs:577-585` | ✅ `:291` | ✅ | ✅ `g728.rs:37` | ✅ `:60` | ✅ 40 Б/20 мс | `oxideav-g728 0.0.8` (Annex G); ITU-вектора не ставились (`g728.rs:6-8`) | — | ✅ native |
| G.723.1 (4) | ✅ `uas.rs:581,588-596` | ✅ `:292-300` fmtp `annexa=no` | ✅ ptime 30 | ✅ `g7231.rs:30` (6.3k MP-MLQ) | ✅ (и 5.3k ACELP, `:60`) | ✅ с накоплением 240/160 (`gateway_audio.rs:249-252`) | `oxideav-g7231 0.0.9` | — | ✅ native |
| Opus (111, 48k/2) | ✅ `uas.rs:556-573` | ✅ `:359-369` `stereo=0` | ✅ | ✅ `opus.rs` (`rusty_opus`) | ✅ + FEC/PLC (`gateway_audio.rs:103-118`) | ✅ моно-мост, clock 48000, ts+960 | `oxideav-opus 0.0.14` — **scaffold** («orphan-rebuild»), не годится | — | ✅ native |
| G.729 (18) | ⚠️ UAS: только G.729 → 488; рядом с PCMU — молчаливое понижение до PCMU (легально, `uas.rs:1090-1094`). Outbound — strict-first → 488 | ❌ `native:false` (`rtp_bridge.rs:301-309`) | ❌ | ❌ FFmpeg без энкодера; `oxideav-g729` в зависимости не подключён | ⚠️ только `FfmpegWorker AudioDecode(G729)` (`codec_worker/mod.rs:41`, тест `ffmpeg.rs:1743`) — мёртвый путь без RTP | ❌ | **`oxideav-g729 0.0.8`: encoder::make_encoder + `encode_frame(&[f32;80])`, декодер end-to-end** (README: round 452), MIT | только декодер (`g729`) | ❌ сейчас; ✅ достижимо (см. §5.2) |
| G.722.1 / C (Siren7) | ❌ | ❌ `native:false` 5 строк (`:310-354`) | ❌ | ❌ | ❌ | ❌ | **крейта нет** | **нет ни энкодера, ни декодера** | 🚫 нельзя объявлять |
| G.719 (Siren22) | ❌ | ❌ `native:false` (`:370`) | ❌ | ❌ | ❌ | ❌ | **крейта нет** | нет | 🚫 нельзя объявлять |
| AAC-LC / MP4A-LATM (113, 90k) | ✅ `uas.rs:504-560`, fmtp-гейт `:462-502` | ⚠️ только probe+legal+pool≥2 (`main.rs:390-397`) | ✅ `uas.rs:462` | ✅ FFmpeg `aac`+`latm` (`latm.rs:40-70`) | ✅ `aac_latm`+`loas` | ✅ 16:15 каденция, 90 кГц (`gateway_aac.rs`, `aac_latm.rs`) | `oxideav-aac 0.1.7` — AAC-LC encoder+decoder, MIT (**потенциально без FFmpeg**) | есть | ⚠️ + латентный дефект `cpresent` (§3 D2) |
| telephone-event (101) | ✅ `uas.rs:754` | ✅ `:384-392` `0-16` | ✅ `0-15` (расхождение, §3 D15) | ✅ | ✅ | ✅ relay `gateway_audio.rs:311-329` | — | — | ✅ |

### Видео

| Кодек (PT) | parse SDP | Offer | Answer | Encode | Decode | Реальный мост | oxideav (registry) | FFmpeg здесь | Итог |
|---|---|---|---|---|---|---|---|---|---|
| H.264 Baseline/Constrained (97) | ✅ `uas.rs:723-753` | ✅ `rtp_bridge.rs:441-449` (fmtp пересчитывается, см. §6) | ✅ всегда `42c0{level}` (`uas.rs:743`) | ✅ libx264 `-profile:v baseline` (`gateway_video.rs:344-346`) | ✅ FFmpeg `h264` | ✅ FU-A payloader `gateway_video.rs:1204-1250` + depacketizer `:1416-1508` | **`oxideav-h264 0.1.9` — пустой крейт** («no decode/encode functionality yet») | libx264 + h264 | ✅ native |
| H.264 Main (4d00xx) | ✅ принимается (`uas.rs:714-721`) | ❌ нет PT | ❌ всегда baseline | ❌ | ✅ | ⚠️ | — | libx264 умеет | ⚠️ половина (см. §5.3) |
| H.264 High (98) | ✅ | ✅ **только full** (`:450-458`) | ❌ `profile_idc != 66` (`rtp_bridge.rs:1284`) | ❌ | ✅ | ❌ outbound | — | умеет | **P0, D1** |
| H.265/HEVC (99) | ✅ `uas.rs:814-828` (DON-гейт `:824`) | ✅ `:459-470` | ✅ `profile-id=1` | ✅ libx265 + `aud/repeat-headers` | ✅ FFmpeg `hevc` | ✅ RFC 7798 полностью (`h265.rs`) | `oxideav-h265 0.0.15` декодер end-to-end, энкодер **PCM-only bootstrap** | libx265 | ✅ native |
| H.261 | ❌ нет `VIDEO_OFFER`-строки (Polycom реально предлагает PT31 `H261/90000` — `tests/fixtures/polycom_rpd_3_5_answer.sdp`) | ❌ (`offer_video_enabled` мёртв, `:516-523`) | ❌ | ❌ | ❌ | ❌ | `oxideav-h261 0.0.7` — **декодер+энкодер** (реальные) | `h261` encoder/decoder есть | ❌ возможно, но не сделано |
| H.263 / H.263+ | ❌ (Polycom предлагает PT34 `H263` и PT96 `H263-1998`) | ❌ | ❌ | ⚠️ `h263`/`h263p` в мёртвом `codec_worker` (`ffmpeg.rs:1100-1185`) | ⚠️ там же | ❌ нет payloader (RFC 4629/4587) | `oxideav-h263 0.0.10` — только структурный парсер (round 4, без IDCT) → **не годится** | `h263`, `h263p` + AVI-муксер | ❌ «сделано на 30 %» (AVI-разбор есть) |
| VP8/VP9/AV1 | ❌ | ❌ | ❌ | — | — | ❌ в SIP | нет крейтов (`oxideav-vp8/vp9/av1` отсутствуют) | libvpx/libaom/librav1e есть | 🚫 в SIP; ✅ отдельный WebRTC-разрез (`main.rs:361-366`) |

Свидетели из реального парка (fixture `polycom_rpd_3_5_answer.sdp`, снятый с Polycom RPD 3.5):

- видео: `111 H264 64001e;packetization-mode=1;max-mbps=49000;max-br=20010` идёт **первым**, затем `109 H264 42801e` (**без** packetization-mode = 0), `110 H264 42801e;packetization-mode=1`, `96 H263-1998`, `34 H263`, `31 H261`, `106/105 H264-SVC`. Т.е. требования заказчика по H.261/H.263 — не гипотеза, а живой терминал; но и «симметричный High» — тоже он.
- аудио: `118 SIRENLPR`, `115/114/113/102/101/103 G7221`, `99/98/97 SIREN14` стоят **перед** `9 G722`, `18 G729`, `15 G728`, `0 PCMU`. Именно этот порядок — причина, по которой UAS обязан понижать до первого bridgeable (`uas.rs:1090-1094`), а исходящий strict-first обязан отказывать (`rtp_bridge.rs:1326`); иначе «connected + dead audio».

### Что это значит для «одной матрицы»

Кодек-списки живут **в шести местах**, и они уже расходятся:

1. `vks-common/codec_capabilities.rs:12-16` — `NATIVE_AUDIO` (13 алиасов) / `NATIVE_VIDEO` (`H264,H265,HEVC`).
2. `vks-gateway-sip/rtp_bridge.rs:287-393` — `AUDIO_OFFER` (19 строк, включая 5 G.722.1 и G.719, которые никогда не отправляются) и `:440-471` `VIDEO_OFFER`.
3. `vks-gateway-sip/uas.rs:504-601` — `bridgeable_audio` (другая нормализация имён: `:537` не срезает пробелы, `codec_capabilities.rs:72` — срезает) и `uas.rs:723-828` — видео.
4. `vks-server/gateway_audio.rs:41-75` — `AudioBridgeCodec::new` (третий маппинг имя→кодек + fallback по PT).
5. `vks-gateway-sip/codec.rs:104-160` — `CodecMap` (WebRTC-модель: `PCMU→OPUS`, `H264→VP8`, «transcode=да»), **противоречит** реальному SIP↔SIP мосту; её единственный потребитель — `tracing::info!` (`call.rs:223`).
6. `vks-server/gateways.rs:125-137` — `H323_AUDIO`/`H323_VIDEO`; плюс `main.rs:361-366` — WebRTC-список (opus/vp8) и `vks-admin/src/settings.rs:69-93` — UI-«намерение» (18 аудио, 8 видео, из которых native 0–1).

Инвариант «одна матрица — один источник истины» **не выполнен**. Подробнее — §3 D7 и §5.1.3.

---

## §3 Дефекты

Формат: симптом → причина → file:line → фикс → тест. Приоритет: P0 — видимая потеря медиа/тишина в достижимой конфигурации; P1 — потеря функциональности/интеропа; P2 — риск, гонка, ресурс.

### P0

#### D1. `full`-профиль объявляет H.264 High (PT98), ответ которым мост гарантированно отвергает → «соединено, но без видео»

- **Симптом.** Терминал отвечает `m=video … 98` (или любым PT с `profile-level-id=6400xx`) **без** Baseline-альтернативы. Звонок устанавливается, аудио идёт, **видео нет вообще**: `video_codec = None`, ни warning'а, ни метрики.
- **Причина.** PT98 в оффере имеет `native:false` и `6400…` (`rtp_bridge.rs:450-458`), т.е. попадает в SDP при `VKS_SDP_CODEC_MATRIX=full` + `VKS_TRANSCODER_ENABLED=1` (`:549-608`). При разборе ответа `video_identity` → H264, но `h264_baseline_level` требует `profile_idc == 66` (`rtp_bridge.rs:1284`) → `None` → видео-медиа не выбирается (`:1367-1381`). При этом энкодер всё равно baseline (`gateway_video.rs:128-130`), т.е. «High» мы и не смогли бы отдать.
- **Важно:** это **не незамеченный баг, а зафиксированное тестом ограничение** — и именно поэтому оно опаснее. Тест `h264_high_interleaved_and_default_mode0_are_not_offered_as_usable_video` (`rtp_bridge.rs:1563-1579`) **утверждает**, что High-only ответ даёт `video_port == None`; док-комментарий `:538-550` сам признаёт, что Polycom RPD 3.5 на полной матрице шлёт H.264 High (PT 98) и «the current bridge … would drop that RTP». Живой fixture `tests/fixtures/polycom_rpd_3_5_answer.sdp` это подтверждает: реальный терминал перечисляет `111 H264 64001e; packetization-mode=1; max-mbps=49000` **первым**, а Baseline — только как `109/110`.
- **Почему P0 для заказчика.** Пункт 3 требует симметрии Main/High; `full` уже сегодня обещает High, зная, что ответ будет отброшен, — ровно класс «ложное объявление», который база сама себе запрещает (`rtp_bridge.rs:242-256`). Включение `full` ради «полной матрицы» ухудшает видео у Polycom.
- **Минимальный фикс (2 строки, безопасно сегодня):** убрать PT98 из `VIDEO_OFFER` до появления `-profile:v high` (см. §5.3.1) + инвариантный тест «каждый видео-кодек в оффере обязан приниматься `video_identity` и `local_video_payload`» (тогда PT98 не сможет вернуться раньше моста).
- **Полный фикс:** §5.3.1 (симметричные профили + `-profile:v` из согласованного профиля).
- **Тест:** `negotiate_media_detailed(offer_full, answer_with_only_pt98)` → `Some("H264")` после фикса; сейчас либо PT98 отсутствует в оффере, либо тест-инвариант из минимального фикса.

#### D2. AAC-LATM: отсутствие `cpresent` в fmtp = «SMC нет», но мост принимает и обещает in-band SMC → 200 OK + тишина

- **Симптом.** Пир предлагает `m=audio … 113` + `a=fmtp:113 profile-level-id=1;object=2` (без `cpresent`). Мы отвечаем 200 OK, запускаем `LatmDecoder`, шлём RTP, но декодер не видит `StreamMuxConfig` (пир его не шлёт, т.к. по RFC 3016 `cpresent` по умолчанию 0 и конфиг должен быть в `config=`), и в обратную сторону уходит мусор. Проверка «bridgeable» проходит.
- **Причина — два fail-open'а, оба в гейте:**
  1. `aac_fmtp_parameters_bridgeable` (`uas.rs:480-509`) инициализирует `let mut cpresent_ok = true;` (`:492`) и проверяет только *присутствующий* `cpresent` (`:498`). Отсутствие параметра → `true`, хотя по RFC 3016 значение по умолчанию **0**. При этом док-комментарий над функцией (`:459-461`) утверждает обратное — «SMC must travel in-band (`cpresent=1`)»; код с комментарием расходятся.
  2. Outbound-путь проверяет fmtp ещё мягче: `rendered.as_deref().is_some_and(|value| !bridgeable(value))` (`rtp_bridge.rs:1359-1363`). Если в ответе **нет** `a=fmtp` для PT 113, `fmtp` = `None` → `is_some_and` = `false` → AAC **принимается**. Это худший случай: без fmtp конфиг вообще может быть только в `config=`, т.е. in-band SMC заведомо не будет.
  Плюс ответ всегда декларирует `cpresent=1` (`rtp_bridge.rs:380`, `uas.rs:466-475`), т.е. мы обещаем то, чего пир не делал; и `codec_worker/latm.rs` не умеет принимать внешний `config=` (AudioSpecificConfig) — только in-band SMC. Существующий тест (`rtp_bridge.rs:1710-1721`) всегда подаёт `cpresent=1`, поэтому дыру не видит.**
- **Латентность.** Дефект не виден сегодня только потому, что AAC заблокирован флагом (`main.rs:390`). **Снятие флага (пункт 1 заказчика) делает его боевым.**
- **Фикс:** (а) `let mut cpresent_ok = false;` + отдельный флаг `seen_cpres` (или `cpresent_ok &= value == "1"` при инициализации `false`) — 2 строки; (б) там же `if fmtp.is_none() { return Err(UnsupportedAudio) }` для PT 113 — 3 строки; (в) в `LatmDecoder` принимать `config=`/ASC, чтобы законно отвечать на `cpresent=0` (10–20 строк); (г) явно писать `cpresent=1` в оффере — уже есть.
- **Тест:** `uas::negotiate("… a=fmtp:113 profile-level-id=1;object=2")` → `None` (488); то же без строки `a=fmtp` → `None`; с `cpresent=1` → `Some`. Для outbound: ответ с PT113 без fmtp → `UnsupportedAudio` (`rtp_bridge.rs:1359`).

### P1

#### D3. Направление медиа свёрнуто в `bool`: `a=sendonly`/`a=recvonly` ответы отвергаются, а наш answer вообще не содержит `a=` направления (RFC 3264 §6.1)

- **Симптом.** Терминал-«интерком» отвечает `a=sendonly` (он только шлёт) — исходящий звонок падает в `NoCommonAudio`. Обратный случай: входящий оффер `a=sendonly` — мы отвечаем **без** строки направления, т.е. `sendrecv` по умолчанию, и шлём медиа туда, где её не ждут.
- **Причина.** `parse_sdp` схлопывает четыре направления в `duplex: bool` (`rtp_bridge.rs:1139-1145`), `usable_media` требует `media.duplex` (`:1155`); `build_answer` (`uas.rs:265-337`) не пишет ни `a=sendrecv`, ни `a=recvonly`. Для оффера `a=sendrecv` есть только в `build_offer` (`rtp_bridge.rs:646,653`).
- **Фикс:** `enum Direction { SendRecv, SendOnly, RecvOnly, Inactive }` в `SdpMedia`; `usable_media` для **ответа** принимает `SendRecv | SendOnly` (пир шлёт) и `RecvOnly` (пир только принимает — мы шлём); `build_answer` выводит зеркальное направление (`uas.rs:298`, сразу после `a=rtpmap`). 30–40 строк с тестами.
- **Тест:** `parse_sdp`+`negotiate_media` на ответе с `a=sendonly` → `Some`; `uas::build_answer` для оффера `a=sendonly` содержит `a=recvonly`.

#### D4. Жёсткие минимальные лимиты `max-fs=920 / max-mbps=13800 / max-br=256` отбрасывают CIF/QCIF-терминалы вместо понижения профиля

- **Симптом.** Терминал уровня CIF (max-fs 396) отвечает `max-fs=396;max-mbps=11880` → видео нет, хотя `VideoProfile::clamp_to_answer` (`gateway_video.rs:163-212`) умеет аккуратно спуститься по лестнице 1280×720→960×540→…→320×180.
- **Причина.** `h264_baseline_level` (`rtp_bridge.rs:1287`) требует, чтобы лимиты пира были **не меньше** захардкоженных констант, никак не связанных с `VideoSdpProfile` (у которого реально `max_fs=3600`, `max_mbps=108000`, `br=2048`; `rtp_bridge.rs:408-433`). Т.е. порог одновременно и лживый (обещает меньше, чем шлём), и слишком строгий для слабых терминалов. Кламп при этом недостижим — до него не доходит.
- **Фикс:** убрать порог из `h264_baseline_level`; передавать в `local_video_payload` требования из `VideoSdpProfile` (max_fs/max_mbps/bitrate) и **никогда** не отвергать пира с меньшими лимитами — только клампить (тест ниже). Сигнатура: `local_video_payload(media, codec, remote_level, &VideoSdpProfile)`.
- **Тест:** answer с `max-fs=396;max-mbps=11880;max-br=256` → `video_codec == Some("H264")`; после `clamp_to_answer` профиль ≤ 352×288.

#### D5. G.726-32 по статическому PT 2 не принимается → исходящий звонок падает вместо использования G.726-32

- **Симптом.** Терминал отвечает `m=audio 3230 RTP/AVP 2` + `a=rtpmap:2 G726-32/8000` (RFC 3551 §4.5.4 объявляет PT 2 устаревшим, но он широко используется). `audio_identity`/`audio_offer_entry` не знают PT 2 → `UnsupportedAudio` → ACK+BYE, хотя кодек native.
- **Причина.** Статическая таблица `uas.rs:577-586` и `AUDIO_OFFER` (`rtp_bridge.rs:287`) не содержат PT 2; `audio_offer_entry` ищет только по номеру в таблице (`rtp_bridge.rs:1158`).
- **Фикс:** в `audio_offer_entry` сопоставлять PT 2 → `G726-32` **только** при совпадении `a=rtpmap` (fail-closed, как уже сделано для статики в `uas.rs:505-516`), в `uas` добавить `2 => G726-32`. 6–8 строк.
- **Тест:** `negotiate_media` на ответе `RTP/AVP 2` + rtpmap `G726-32/8000` → `audio_codec == "G726-32"`; без rtpmap → `NoCommonAudio`.

#### D6. Политика кодеков из настроек не влияет на SDP: оператор выбирает кодеки, шлюз предлагает весь native-список

- **Симптом.** В настройках комнаты/шлюза включён только PCMU (или только H.265), но в `m=audio` уходит весь `AUDIO_OFFER`, а в `m=video` — H.264+H.265. Терминал законно выбирает G.722/H.264.
- **Причина.** `ensure_bridgeable_codec_selection_for` (`gateways.rs:147-190`) только **валидирует** «есть хотя бы один native», а оффер строится из `AUDIO_OFFER`/`VIDEO_OFFER` без параметра «разрешённые кодеки» (`rtp_bridge.rs:478-485`, `:609-660`).
- **Фикс:** `build_offer_with_video(profile, &allowed)` — фильтровать `audio_offer()`/`video_offer()` по списку из настроек (`CodecConfig::audio_parts` уже нормализует `NAME:bitrate`, `settings.rs:114-121`). ~20 строк + прокинуть список в `gateways.rs:1456`.
- **Тест:** настройки `["PCMU","H265"]` → в оффере ровно PT0 + PT99 (и telephone-event).

#### D7. Матрица разорвана: один и тот же кодек описан 6 раз, и списки уже расходятся

- **Симптомы (конкретные расхождения, не теория):**
  - `NATIVE_AUDIO` содержит алиасы `G711/G711U/G711A` (`codec_capabilities.rs:12-13`), которых нет в `AUDIO_OFFER` (там только `PCMU/PCMA`); `opus` в оффере строчными (`rtp_bridge.rs:361`), в матрице — `OPUS`.
  - `normalize()` в `codec_capabilities.rs:72` срезает пробелы, копии в `uas.rs:537`, `gateway_audio.rs:44`, `gateway_aac.rs:81` — нет. `"MP4A-LATM "` проходит в одной ветке и падает в другой.
  - `H323_AUDIO` (`gateways.rs:125`) объявляет `G723`, но H.323-путь не имеет накопителя 30/20 мс, который есть у SIP (`gateway_audio.rs:249`), — расхождение поведения при одинаковом «native».
  - `CodecMap` (`codec.rs:118-160`) утверждает «PCMU→OPUS, transcode=да», тогда как реальный мост — SIP↔SIP native (декод→микс→энкод тем же кодеком).
  - UI-настройки (`settings.rs:69-93`) перечисляют 18 аудио/8 видео, из которых native — 8/2; API отдаёт `supported:false` (`shell.rs:579`), но `native_audio` в `shell.rs:288` — это `NATIVE_AUDIO`, где AAC отсутствует, хотя AAC **мостится** (пусть и за флагом).
- **Фикс:** §5.1.3 — ввести один тип `CodecSpec` (canonical name, sdp name, clock, channels, PT, transport-множество, backend, bridge-конструктор) и **сгенерировать** из него `NATIVE_*`, оффер, `bridgeable_*` и UI-список. Инвариантный тест: «для каждого элемента матрицы есть ровно один оффер-ряд и одна ветка моста; для каждого оффер-ряда — запись в матрице».
- **Тест:** мета-тест по таблице (см. §5.1.3) + существующий `offer_native_flags_match_shared_capabilities` (`rtp_bridge.rs:867-880`) расширить на fmtp и на UAS-таблицу.

#### D8. H.264 payloader/depacketizer живут в `vks-server` (private), H.265 — публичным модулем в `vks-gateway-sip`; тесты H.264-уровня невозможны без сервера

- **Симптом.** `packetize_h264` (`gateway_video.rs:1204-1255`) и `H264Depacketizer` (`:1416-1508`) недоступны ни SIP-крейту, ни интеграционным тестам; у H.265 есть `h265.rs` с 30+ тестами. Дублирование логики (FU/AP/MTU/marker) в двух крейтах.
- **Фикс:** `crates/vks-gateway-sip/src/h264.rs` по образцу `h265.rs` (payloader + depacketizer + MTU + тесты), `gateway_video.rs` использует его. −120 строк в сервере, +тесты.
- **Тест:** перенести существующие server-тесты H.264 FU-A/STAP-A в новый модуль + вектор «SPS/PPS/IDR проходят без изменений» (как `tests/h265_rtp.rs`).

#### D9. Публичная матрица уже́ реальных возможностей и одновременно шире: AAC не в `NATIVE_AUDIO`, зато UI обещает G.722.1/G.719/ILBC

- **Симптом.** Интегратор читает `/api/v3/settings/video` и видит `native_audio` без AAC (хотя AAC мостится) и `codec list` с G.719/G.722.1/ILBC (которые не мостятся никогда). Ни один ответ API не объясняет **почему**.
- **Фикс:** §5.1.3 + машиночитаемая причина: `UNSUPPORTED_AUDIO: &[(&str, &str)]` (`"G7221", "no codec implementation: neither FFmpeg nor oxideav"`), `WORKER_AUDIO` для AAC. UI получает `supported` + `reason`.
- **Тест:** `settings_audio_codecs` для `G7221:24` → `supported:false, reason:"no codec implementation"`.

### P2

| # | Дефект | file:line | Фикс |
|---|---|---|---|
| D10 | У `gateway_video` и `latm` нет `process_group(0)`/`kill(-pgid)`; только `kill_on_drop` — обёртки/hwaccel-дети переживают родителя. У `codec_worker`/`farm` это есть. | `gateway_video.rs:403`, `latm.rs:96`, ср. `ffmpeg.rs:227,238`, `farm.rs:979,988` | один общий `FfmpegChild` (§6) |
| D11 | `debug_assert_eq!(destination.len(), dw)` при `dw=1` (прямоугольник шириной 1 px) → паника в debug/тестах: `chroma_dw=0`, а передаётся `cw_dest=1`. Плюс документированное «кровотечение» хромы на нечётных rect'ах. | `compositor.rs:140-163,176-177` | `if chroma_dw == 0 { continue; }`; центрировать хрому по чётному origin |
| D12 | Неограниченная аллокация холста: `set_composite_size(room, 65535, 65535)` → `vec![128; 6.4 ГБ]` (OOM = abort процесса, не паника). Продуктовый кап `MAX_VIDEO_WIDTH/HEIGHT = 4096×2160` существует только в `codec_worker`. | `engine.rs:1055-1079,1105-1116`; ср. `codec_worker/mod.rs:19-22` | проверять кап в `set_composite_size`/`compose_sized`, одна константа |
| D13 | Мёртвый продакшн-код: `codec_worker` видео-кодеки (H261/H263/H263Plus/H265) вызываются только из `tests/h265_rtp.rs`; AVI-извлечение — только из них. 5 независимых спавнеров FFmpeg. | `codec_worker/ffmpeg.rs:1018-1185`, `latm.rs:88`, `gateway_video.rs:304`, `farm.rs:922` | §5.3.3 (переиспользовать) либо §6 (удалить) |
| D14 | Если последний NAL в AU пустой, marker не ставится → получатель не завершит AU (латентно: `AnnexB::push` пустые NAL отбрасывает). | `gateway_video.rs:1206-1218` | считать marker по последнему *непустому* NAL |
| D15 | Оффер обещает `telephone-event 0-16`, ответ сужает до `0-15`. | `rtp_bridge.rs:389` vs `uas.rs:307` | согласовать `0-16` (RFC 4733 знает 16) |
| D16 | AVI-уокер: паддинг считается по **необрезанной** длине (`size & 1`), при усечённом чанке (`end == len`) указатель уезжает на 1 байт — для битого файла. Отдельно: `00db` (несжатый) не отфильтрован явно. | `ffmpeg.rs:1180-1183` | `if end < len { end + (size & 1) } else { len }`; принимать только `dc` |
| D17 | AAC-энкодер жёстко 64 кбит/с, без учёта `b=AS`/fmtp битрейта. | `gateway_aac.rs:111` | брать из профиля сессии |
| D18 | `resize_i420` аллоцирует новый `Vec` на каждый кадр каждого зрителя (720p ≈ 1.4 МБ × N × 30 fps). | `gateway_video.rs:1059-1064` | переиспользуемый буфер на сессию |
| D19 | Аудио требует **совпадения номеров PT** в оффере и ответе (видео — терпит перенумерацию по identity). Терминал, ответивший `G722` динамическим PT (как Polycom делает с видео в fixture), получает 488. RFC 3264 §6.1 перенумерацию запрещает, поэтому риск средний-низкий, но асимметрия с видео необъяснима. | `rtp_bridge.rs:1310-1345` (правило и комментарий `:1291-1300`), ср. видео-путь `:1367-1381` | искать локальный кодек по identity, а не по номеру; на провод брать PT из ответа (5 строк) |

---

## §4 Разбор примеров кода заказчика

Файлов с `NegotiatedH264Profile` / `extract_h263p_from_avi_chunk` / `scale_yuv_plane_bilinear` в репозитории нет — я разбираю **аналоги в дереве** (они и есть «тот же код», просто с другими именами) и отдельно перечисляю канонические баги таких сниппетов: что подтверждено в репо, а что станет багом, если сниппет взять дословно.

### 4.1 `NegotiatedH264Profile` (аналоги: `h264_baseline_level` `rtp_bridge.rs:1267-1309`, `h264_payload` `uas.rs:723-753`, `parse_profile_level_id` `uas.rs:802-813`, `VideoProfile::profile_level_id` `gateway_video.rs:152-157`)

Корректно в репо:

- `profile-level-id` — **hex**, три поля (`profile_idc`, `profile_iop`/constraints, `level_idc`), значения whitelisted: `parse_profile_level_id` (`uas.rs:802`) и `rtp_bridge.rs:1270-1283`. Уровни `< 10` (например `1b` = 9) отбрасываются — это осознанно.
- Ответ **не** эхом повторяет оффер, а объявляет профиль *отправителя* (`42c0…`, `uas.rs:743`) — это верно по RFC 6184 §8.2.2 и уже чинило реальный десинк SDP↔SPS (`gateway_video.rs:152-155`: `42e0…` → `42c0…`).
- `level` пересчитывается при понижении геометрии (`clamp_to_answer:210`).

Баги/риски, подтверждённые в репо:

1. **`constraints & 3 != 0 → reject`** (`rtp_bridge.rs:1284`): отвергает Main (`4d00`) и High (`6400`), т.е. «симметричные профили» невозможны в принципе (D1). Проверка constraint-флагов должна быть *профиль-зависимой*: для 66 допустимы `0x00/0x40/0x80/0xc0`, для 77 — `0x00/0x80`, для 100 — `0x00`.
2. **Профиль жёстко зашит в трёх местах** (`rtp_bridge.rs:1284`, `uas.rs:743`, `gateway_video.rs:129`), а `VideoSdpProfile::high_profile_level_id` (`rtp_bridge.rs:421-424`) правит только строку оффера. Для Main/High нужен один `H264Profile { idc, constraints }` и `profile_level_id()` из него.
3. **`level` не может быть «выведен из геометрии» дважды**: `VideoSdpProfile.profile_level_id` приходит от `VideoProfile` (`gateways.rs:682-689`), а `h264_baseline_level` возвращает уровень из **ответа** пира; они не сверяются. Рассинхрон «в SDP 3.1, в SPS 4.0» ловится только тестом на реальный SPS (у нас такого нет).
4. **Чего не хватает для симметрии:** при `level-asymmetry-allowed=0` (по умолчанию) наш TX-уровень обязан быть ≤ уровня пира; при `=1` — может быть выше. Сейчас параметр игнорируется: `h264_payload` всегда ставит `tx_level = min(offer, 31)` (`uas.rs:735-745`), но не объявляет `level-asymmetry-allowed` в ответе — это допустимо, но при высоком оффере мы молча режем уровень и не сообщаем об этом в fmtp.
5. Дословный сниппет заказчика почти наверняка содержит `u8::from_str_radix(&s[0..2])` без проверки длины/hex — в репо от этого защищаются (`get(0..2)?` + `from_str_radix(...).ok()?`), сохранить.

### 4.2 `extract_h263p_from_avi_chunk` (аналог: `extract_avi_video_es` `codec_worker/ffmpeg.rs:1132-1185`)

Корректно:

- RIFF-уокер **пропускает 4-байтовый FORM-type** после `RIFF`/`LIST` (`data[start+4..end]`, `:1149-1150`) — классический баг «AVI » как chunk-id здесь не допущен.
- Берётся только `dc` (сжатые кадры), `db` (несжатый) игнорируется — верно для H.263+.
- `start.saturating_add(size).min(len)` — защита от 0xFFFFFFFF в битом файле (`:1179`).
- Кадры в AVI лежат без start-code-обёртки, но каждый кадр H.263 начинается с PSC (20 бит), поэтому конкатенация чанков даёт валидный Annex-B-поток — тест `h263_plus_encodes_320x240_which_base_h263_rejects` (`ffmpeg.rs:1499`) это подтверждает.

Баги/риски:

1. **`chunk_id.starts_with(b"0")`** покрывает только поток 0. В AVI первые два символа — номер **потока** в hex, не индекс кадра. Для нашего муксера это всегда `00`, но если FFmpeg положит видео потоком 1 (`01dc`), извлечение вернёт пусто → `WorkerError::Codec`. Дёшево: `chunk_id[2..] == b"dc" && chunk_id[0..2].iter().all(u8::is_ascii_hexdigit)`.
2. **Паддинг по необрезанной длине** (`:1183`) — для усечённого файла указатель уезжает; фикс в D16.
3. **`00db`**: задание требует «00dc/00db»; в репо `db` не обрабатывается вообще — для h263p это правильно (в `db` лежал бы raw YUV), но стоит явно вернуть `Err` с внятным сообщением, а не молча «нет кадров».
4. **AVI1/OpenDML**: при записи в непоисковый pipe FFmpeg ставит `AVI1` и, после ~1 ГБ, `AVIX`-списки. Сейчас вызывается на один кадр (`read_to_end`, `:788-795`), поэтому не стреляет; при переходе на потоковую запись извлечение обязано обходить и `AVIX`, иначе кадры после границы теряются.
5. Чётность/выравнивание: сниппет заказчика, судя по названию, ищет `00dc` **внутри одного чанка** без учёта паддинга — это и есть типовой баг; в репо выравнивание учтено (но см. п.2).

### 4.3 `scale_yuv_plane_bilinear` (аналога нет; ближайшее — nearest `compositor.rs:176-213` и `resize_i420` `gateway_video.rs:1058-1140`)

Проверить и не повторить:

1. **Полупиксельный сдвиг.** `sx = dx * sw / dw`, затем интерполяция `(sx, sx+1)` — это *не* центрированная выборка: при downscale 2:1 картинка уезжает на 0.5 исходного пикселя влево-вверх. Правильно: `pos = ((dx as u64 * 2 + 1) * sw as u64).saturating_sub(dw as u64)` в половинных единицах, `x0 = pos / (2*dw)`, `frac = pos % (2*dw)`, и clamp `x0 ≥ 0`.
2. **Индексация на правой/нижней кромке.** `x1 = min(x0+1, sw-1)`, `y1 = min(y0+1, sh-1)` обязательны, иначе OOB-паника на последнем столбце/строке (в nearest-пути это неявно защищено `get()`).
3. **Переполнение u8 в аккумуляторе.** `a*(256-f) + b*f` в `u8` переполняется уже при f=128; нужно `u32` (или `u16`) + `+128` перед `>>8` для round-half-up. `(a*(256-f)+b*f)>>8` без `+128` даёт систематическое затемнение на пол-LSB.
4. **Квантование веса.** `f = frac*256/dw` теряет точность; лучше 16.16 (`f = ((frac << 16) / dw) >> 8`) или держать `pos` в 16.16 и брать старшие 8 бит дробной части.
5. **Хрома.** У хромы **своя** дробная часть (в 2 раза мельче); нельзя переиспользовать `frac` лума-плана. Размеры назначения: `floor(w/2) × floor(h/2)`; при нечётных `w/h` — не читать за границу плана (в I420 это уже OOB, т.к. хрома-план меньше).
6. **u32/u64.** `dx * sw` для 4K (3840×2160) = 8.3e6 — влезает в u32, но `w*h*3/2` для 65535² уже нет (4.3e9 > u32::MAX). В репо `Frame::pixel_count` — u64 (`frame.rs:48`), аллокации — `usize`; новый скейлер обязан считать позиции в `u64`/`usize`, иначе регресс на 8K.
7. **memcpy-путь.** Для I420 «весь кадр одним `copy_from_slice`» корректен **только** при совпадении размеров И раскладки (репо: `gateway_video.rs:1070-1075` — с проверкой `is_yuv420p()` и длины). Для частичного rect — построчный `copy_from_slice` (репо: `compositor.rs:178-183`). Билинейный путь не должен ломать оба этих случая: identity обязан остаться побайтовым (тест `resize_identity_is_memcpy_and_keeps_chroma`, `gateway_video.rs:1701`).
8. **Паника на 1-px rect** (D11): `debug_assert_eq!(dst.len(), dw)` при `dw=1`/`chroma_dw=0`. В новом коде — ранний `return`.

---

## §5 План по пунктам целевого состояния

### 5.1 Пункт 1: снять блокировку AAC-LATM + публичная матрица

| # | Что | Файл:строки | Тест | Критерий |
|---|---|---|---|---|
| 5.1.1 | Заменить булев «legal» на **профиль лицензий**: `VKS_CODEC_LICENSE_ACCEPTED=aac-latm` (список), `VKS_AAC_LATM_LEGAL_ACCEPTED=1` — как алиас; логировать принятые кодеки на старте; вынести разбор в `vks-server::codec_policy` (тестируемо) | `main.rs:383-403`, `deploy/vks.env.example:56-58` | unit: `license_accepted("aac-latm")==true`, пусто == false, мусор == false | флаг остаётся явным действием оператора, но имя честное и журналируемое |
| 5.1.2 | **Сначала** починить D2 (`cpresent`) | `uas.rs:480-509`, `rtp_bridge.rs:1359-1363` | тест из D2 | без фикса AAC включать нельзя |
| 5.1.3 | Один тип `CodecSpec` + генерация списков: `NATIVE_AUDIO/NATIVE_VIDEO` (in-process), `WORKER_AUDIO/WORKER_VIDEO` (FFmpeg), `WEBRTC_VIDEO` (opus/vp8/vp9/av1), `UNSUPPORTED` (с причиной) | `codec_capabilities.rs:12-16`, `rtp_bridge.rs:287-393,440-471`, `uas.rs:504-601`, `shell.rs:288-289,579`, `settings.rs:69-93` | мета-тест: у каждого элемента матрицы ровно один оффер и одна ветка моста; `supported` для G7221/G719/ILBC == false с `reason` | исчезают 6 списков, остаётся 1 таблица + 3 сгенерированных среза |
| 5.1.4 | G.722.1/C/G.719 — в `UNSUPPORTED` с причиной `no codec implementation`, **не** в `NATIVE_AUDIO` | `codec_capabilities.rs` | тест причины | публичная матрица не лжёт |
| 5.1.5 | VP8/VP9/AV1 — только в `WEBRTC_VIDEO` | `codec_capabilities.rs`, `main.rs:361` | тест: `is_native_video("VP8")==false`, `is_webrtc_video("VP8")==true` | SIP-матрица не расширяется фиктивно |
| 5.1.6 | Политика кодеков из настроек фильтрует оффер (D6) | `rtp_bridge.rs:478-485,609-660`, `gateways.rs:1456-1458`, `settings.rs:114-121` | настройки `["PCMU","H265"]` → в оффере ровно PT0+PT99+TE | оператор управляет SDP |

### 5.2 Пункт 2: двунаправленный G.729 + G.722.1/C + G.719 + `a=sendrecv`

| # | Что | Файл:строки | Тест | Критерий |
|---|---|---|---|---|
| 5.2.1 | **G.729 через `oxideav-g729 = "=0.0.8"`** (как соседние oxideav-пины, `vks-gateway-sip/Cargo.toml:25-29`): новый `g729.rs` (~90 строк) — 10-байтовый wire-кадр, упаковка 2 кадров в 20 Б/20 мс, `FRAME_SAMPLES=80`, PLC/`decode(&[])`; ветка `AudioBridgeCodec::G729`; `bridgeable_audio` → PT18; `AUDIO_OFFER` PT18 `native:true`; `NATIVE_AUDIO += "G729"` | `Cargo.toml`, `g729.rs` (новый), `gateway_audio.rs:41-194`, `uas.rs:504-601`, `rtp_bridge.rs:301-309`, `codec_capabilities.rs:12` | 20 Б/20 мс roundtrip с энергией; len 0/9/11/31 → reject; `timestamp_step()==160`; оффер содержит `a=rtpmap:18 G729/8000`; `strict-first` принимает PT18 | G.729 звучит в обе стороны на 20 мс |
| 5.2.1a | **Валидация перед объявлением:** прогнать ITU-векторы из `oxideav-g729/docs/audio/g729/conformance`; если бит-экзактности нет — оставить `native:false` и не объявлять (честность важнее пункта заказа) | — | скрипт/тест на корпус | 0 расхождений или кодек не объявлен |
| 5.2.2 | G.722.1/C/G.719: **не объявлять**. Вариант (b) — написать G.722.1 (MLT/MDCT, ~2–3 kLOC + таблицы) — отдельный проект, не «минимальный патч»; лицензия Polycom royalty-free, но требует принятия соглашения (см. §7) | — | — | статус `UNSUPPORTED` с причиной |
| 5.2.3 | `a=sendrecv` в ответе + вывод направления из оффера | `uas.rs:265-337` (после `:298`), `rtp_bridge.rs:1139-1156` | тесты из D3 | RFC 3264 §6.1 |
| 5.2.4 | Аудио: искать локальный кодек по identity, а не по номеру PT (D19); на провод — PT из ответа | `rtp_bridge.rs:1310-1345` | ответ с `a=rtpmap:96 G722/8000` при нашем `9` → `Some`, `audio_payload_type == 96` | перенумерация не ломает звонок |
| 5.2.5 | G.726-32 по PT 2 (D5) | `uas.rs:577-586`, `rtp_bridge.rs:1158-1165` | тест из D5 | PT2 работает при совпадении rtpmap |

### 5.3 Пункт 3: симметричные H.264 + H.265 (DON/aud/repeat-headers) + h263p

| # | Что | Файл:строки | Тест | Критерий |
|---|---|---|---|---|
| 5.3.1 | `H264Profile { idc: 66|77|100, constraints }`: один `profile_level_id()` для оффера, ответа и `VideoProfile`; снять `profile.profile = "baseline"` (`gateway_video.rs:128-130`); `-profile:v` из согласованного профиля (`:344-346`); `h264_baseline_level` → `h264_level_for(profile)` с профиль-зависимыми constraint-масками | `rtp_bridge.rs:408-434,1259-1309,440-458`, `uas.rs:714-753,802-813`, `gateway_video.rs:109-157,304-360` | unit: `4d0028`/`640028` принимаются; offer содержит 97(42c0xx)/98(4d00xx)/99(6400xx); integration: `ffprobe`/`h264_metadata` показывает `profile_idc` из SPS == fmtp | Baseline/Main/High симметричны, SDP↔SPS не расходится |
| 5.3.2 | H.265: добавить `sprop-max-don-diff=0` в оффер (сейчас контракт только в коде) | `rtp_bridge.rs:459-470` | оффер содержит параметр; ответ с `=1` → видео нет (`h265_don_compatible` уже так) | DON=0 объявлен явно |
| 5.3.3 | h263p end-to-end: (1) вынести H.264 payloader в `vks-gateway-sip/h264.rs` (D8); (2) RFC 4629 payloader для H.263+ (границы AU по PSC, без FU, ~150 строк); (3) `VIDEO_OFFER` PT101 `H263-1998/90000` (PT — в пределах `m=video`, пересечение с аудио-PT101 `telephone-event` безвредно и законно, RFC 4566 §5.14; если хотим избежать путаницы — взять 104); (4) `gateway_video` decode `-f h263`; (5) переиспользовать `extract_avi_video_es` из `codec_worker` (сейчас мёртв) | `h264.rs` (новый), `h263.rs` (новый), `rtp_bridge.rs:440-471`, `uas.rs:626-828`, `gateway_video.rs:304-390` | `h263p encode → payloader → depacketizer → decode` тем же числом кадров; AVI-разбор на 3-кадровом файле | h263p виден в SDP и реально мостится |
| 5.3.4 | Починить AVI-паддинг и явно отвергать `00db` (D16) | `ffmpeg.rs:1180-1183` | тест на усечённый чанк/`db` | уокер не уезжает |
| 5.3.5 | **H.261 — самый дешёвый недостающий видеокодек**: `oxideav-h261 0.0.7` имеет реальный энкодер и декодер (не заглушку), а FFmpeg — `-c:v h261`. RFC 4587 payloader ~120 строк + `VIDEO_OFFER` PT100 `H261/90000` + `VideoCodec::H261` в `gateway_video`. Даёт живому Polycom третий вариант видео и закрывает пункт «H.261/263» наполовину | `h261.rs` (новый), `rtp_bridge.rs:440-471`, `uas.rs:626-828`, `gateway_video.rs:304-390` | h261 encode→payloader→depacketize→decode (те же кадры); оффер содержит `a=rtpmap:100 H261/90000` в `m=video` | H.261 в SDP и мосте |

### 5.4 Пункт 4: билинейный скейлер + memcpy + oxideav-scene

| # | Что | Файл:строки | Тест | Критерий |
|---|---|---|---|---|
| 5.4.1 | `scale_plane_bilinear(dst, src, sw, sh)` с центрированной 16.16-выборкой (пп. 4.3.1–4.3.6), per-plane; включить для `Compositor::blit` и `resize_i420` | `compositor.rs:176-213`, `gateway_video.rs:1058-1140` | identity == memcpy (побайтово); 2:1 downscale рампы == среднее ±1; кромки без OOB; 1-px rect без паники; `u64` на 8K | билинейно, без сдвига на 0.5 px |
| 5.4.2 | memcpy-путь: уже есть (`gateway_video.rs:1070`, `compositor.rs:178`); добавить «rect == весь холст → один `copy_from_slice`» в композитор | `compositor.rs:49-90` | `compose_sized(rect=full)` → `data == frame.data` | 0.26 мс/1080p сохраняется |
| 5.4.3 | `oxideav-scene` — **не** включать по умолчанию; либо убрать фичу (сейчас только re-export, `lib.rs:64-66`), либо оставить opt-in | `vks-media-mcu/Cargo.toml:40-44`, `lib.rs:64` | `cargo build -p vks-media-mcu` без фичи | деп-вес не растёт |

---

## §6 «Меньше кода»: дубли, мёртвые абстракции

| Что удалить/слить | Где | Объём | Почему |
|---|---|---|---|
| **`CodecMap` + `SipCodec`/`WebRtcCodec`/`NegotiatedCodec`** | `vks-gateway-sip/src/codec.rs` (266 строк) и близнец `vks-gateway-h323/src/codec.rs` | ~500 строк | единственный потребитель `web_codec`/`needs_transcode` — `tracing::info!` (`call.rs:223`); таблица «PCMU→OPUS, H264→VP8» **противоречит** реальному SIP↔SIP native-мосту. Заменить на `canonical_codec_name()` в `codec_capabilities` |
| **5 спавнеров FFmpeg → один `FfmpegChild`** | `codec_worker/ffmpeg.rs:222-272` (эталон: `process_group`, `kill(-pgid)`, stderr-ring, poison, timeout), `latm.rs:88-120`, `gateway_video.rs:304-420`, `farm.rs:922-1010` | ~400 строк | дублируется всё: спавн, дренаж stderr, kill, wait; и только один из четырёх убивает группу (D10) |
| **Видео-кодеки `codec_worker` (H261/H263/H263Plus/H265)** | `codec_worker/mod.rs:20-140`, `ffmpeg.rs:960-1185` | ~300 строк | продакшн-вызовов нет (только `tests/h265_rtp.rs` и внутренние тесты). Либо удалить, либо (лучше) переиспользовать для 5.3.3 и H.261 |
| **`resize_i420` vs `Compositor::blit` vs `reference_resize`** | `gateway_video.rs:1058-1140,1590-1650`, `compositor.rs:96-213` | ~200 строк | три реализации одной floor-карты; одна из них — «эталон» в тестах. Слить в `vks-media-mcu::scale` (один модуль, два алгоритма: nearest/bilinear) |
| **`H264Depacketizer` vs `H265Depacketizer`** | `gateway_video.rs:1416-1508` vs `h265.rs` | ~120 строк | H.264 должен жить рядом с H.265 (D8) |
| **`gain()` ×2** | `gateway_audio.rs:11-17`, `gateway_aac.rs:24-30` | ~14 строк | буквальный дубль |
| **`normalize()` ×4** | `codec_capabilities.rs:72`, `uas.rs:537`, `gateway_audio.rs:44`, `gateway_aac.rs:81` | ~12 строк | разные наборы разделителей → расхождения (D7) |
| **`VIDEO_OFFER.fmtp` — мёртвые строки** | `rtp_bridge.rs:446,455` | 2 строки, но вводят в заблуждение | `push_video_attrs` **пересчитывает** fmtp из профиля (`:500-515`), а строки обещают `42801e` / `max-fs=920`, тогда как в SDP уходит `42c01f` / `max-fs=3600` |
| **`offer_video_enabled` для H261/H263** | `rtp_bridge.rs:516-523` | ~8 строк | нет строк в `VIDEO_OFFER` → ветка недостижима (мёртвая «заготовка на будущее») |
| **`AudioCodec::G729` worker** | `codec_worker/mod.rs:41`, `ffmpeg.rs:651` | ~30 строк | декодер без энкодера и без RTP-пути; либо достроить (5.2.1), либо удалить |
| **`mock`-кодеки** | `engine.rs:629-632` (FrameSource/AudioSource) | — | **не удалять**: это тест-инфраструктура движка, не мёртвый код |
| **`oxideav` feature / `oxideav_scene` re-export** | `vks-media-mcu/Cargo.toml:40-44`, `lib.rs:64-66` | — | фича не даёт ни скейлера, ни композитора; либо использовать, либо убрать |

Итог: **≈1.5 тыс. строк** можно убрать/слить без потери функций; из них ~500 — мёртвая WebRTC-модель кодеков.

---

## §7 Риски и неизвестное

**Технические**

1. **`oxideav-g729 0.0.8` — «clean-room rebuild in progress».** README заявляет оба направления и регистрацию в oxideav-core, но это не то же самое, что подтверждённая бит-экзактность/интероп. Риск: мост «работает» на своём кодеке и разваливается на реальном терминале. Митигация: 5.2.1a (ITU-векторы) до объявления в SDP.
2. **G.722.1 (2–3 kLOC) и G.719** — не «патч», а разработка кодека + конформанс. В рамках текущей итерации невозможны.
3. **AAC-LATM:** снятие флага без 5.1.2 = тишина (D2). Плюс `-smc-interval` есть в этом FFmpeg (проверил, `1..65535`), но на другой версии опция может отсутствовать → воркер не стартует (спасает probe `has_audio_latm_bridge`, но он не проверяет опцию муксера).
4. **H.264 Main/High:** `-profile:v high` у libx264 включает 8×8-трансформ/CABAC — реальные терминалы (Polycom) исторически капризны к High; нужен e2e-прогон, которого у меня не было.
5. **H.263:** после появления payloader'а интероп всё равно под вопросом (RFC 4629 vs «H.263-1998» в терминалах, PSC-границы AU). Оценка 1 PR + живой терминал.
6. **FFmpeg-версии.** `h263p` требует AVI-муксера, `latm` — `-smc-interval`; оба есть здесь, но probe не проверяет ни муксер `avi`, ни опцию `smc-interval`.

**Юридические (без обмана)**

7. **G.729 — royalty-free с 2017** (патентный пул Sipro истёк; публично: [Wikipedia G.729, раздел Licensing](https://en.wikipedia.org/wiki/G.729)). Значит «снять блокировку G.729» — легитимно; блокер технический (энкодер), а не юридический.
8. **G.722.1/C и G.719 — royalty-free лицензия Polycom, но по соглашению**, не public domain; кода нет ни в FFmpeg, ни в oxideav. Формулировка «снять блокировку» здесь неприменима: блокирует не флаг, а отсутствие реализации.
9. **AAC-LC.** Патентный пул (Via LA) исторически лицензировал AAC; проверить срок действия конкретных патентов я не могу (нет доступа к первоисточнику), поэтому **не утверждаю** «патенты истекли». Практический вывод: флаг `VKS_AAC_LATM_LEGAL_ACCEPTED` **не является юридической защитой** — FFmpeg с AAC-энкодером уже в дереве и используется для H.264/H.265; флаг лишь убирает AAC из SDP. Честная форма — `VKS_CODEC_LICENSE_ACCEPTED=aac-latm` + запись в `deploy/vks.env.example` «оператор принял решение, кто и на каком основании», + лог на старте. Никаких формулировок «patent-free» в документации.

**Неизвестное (нужен живой стенд)**

10. Какой профиль реально выбирает Polycom RPD 3.5 в ответ на наш `full` (PT98-only?) — от этого зависит, P0 D1 стреляет всегда или иногда.
11. Отвечает ли кто-то из парка `a=sendonly`/`a=recvonly` и G.726-32 как PT2 (D3/D5) — влияет на приоритет.
12. Реальная задержка AAC-LATM (SMC каждый кадр = +3 байта/кадр, priming) — не измерялась.

---

## §8 Оценка /10

| Разрез | Оценка | Обоснование |
|---|---|---|
| Payload-слой (RTP, RFC 7798/3551/3016) | **9** | `h265.rs` — образцовый: типы, FU/AP, MTU, DON, sequence-wrap, 30+ тестов; G.726 LSB-first по RFC; G.722 cadence 160/20 мс; AAC LOAS strip/add + SMC |
| Честность объявлений | **8** | `strict-first` + 488 вместо «тишины»; probe-гейт для видео/AAC. Минус — P0 D1 (High) и D2 (`cpresent`) |
| Матрица возможностей (единственный источник истины) | **4** | 6 списков, расходятся уже сейчас (D7/D9) |
| Симметрия профилей H.264 | **4** | offer ≠ answer ≠ encoder; Main нет, High не отвечается |
| G.729 / G.722.1 / G.719 | **2** | ничего не сделано; G.722.1/G.719 невозможны без нового кодека |
| Скейлер/композитор | **6** | корректный nearest + memcpy-путь, тесты на эталон; билинейного нет, есть D11/D12 |
| Управление FFmpeg-процессами | **6** | `codec_worker`/`farm` — образцово (`kill(-pgid)`); `gateway_video`/`latm` — слабее (D10); 5 спавнеров |
| Тесты | **8** | 148 unit в `vks-gateway-sip`; интеграционные roundtrip x265; минус — нет H.264-payload-тестов в SIP-крейте, нет тестов профилей, нет e2e-терминала |
| **Итого** | **7/10** | Крепкий payload-слой и редкая для шлюза честность; но матрица разорвана, «full»-профиль обещает неотвечаемое, а половина целевого состояния требует не патчей, а кодеков |

---

## §9 Спорные тезисы

### Т1. G.722.1/C и G.719 нельзя объявлять ни при каком флаге — это не «блокировка», а отсутствие кодека

**ТЕЗИС.** Пункт 2 заказчика («нативные G.722.1/C, G.719») невыполним в этой итерации: в дереве нет реализации, в registry нет крейтов `oxideav-g7221`/`oxideav-g719` (есть только `oxideav-g722`, это другой кодек), в FFmpeg на этом хосте нет ни энкодера, ни декодера (`ffmpeg -encoders | grep -Ei 'g7221|g719'` пусто; `ffmpeg -decoders` тоже). В `AUDIO_OFFER` эти строки есть, но с `native:false` и в SDP не попадают никогда (`rtp_bridge.rs:478-485` + тест `:852-855`).

**ДОКАЗАТЕЛЬСТВО.** 5 строк `G7221` + 1 `G719` в `rtp_bridge.rs:310-370` не имеют ни одной ветки в `bridgeable_audio` (`uas.rs:504-601`), ни arms в `AudioBridgeCodec::new` (`gateway_audio.rs:41-75`), ни модуля. Реализация G.722.1 — MLT/MDCT 16 кГц + таблицы + VQ, ~2–3 kLOC; G.719 — ещё больше. Это не «минимальный элегантный патч».

**ЧТО ПЕРЕУБЕДИТ.** (а) Работающий энкодер/декодер под любой совместимой лицензией (FFmpeg-патч, oxideav-крейт, GStreamer-плагин), прогнанный на ITU-векторах; (б) либо явное решение заказчика «G.722.1/G.719 не нужны как нативные, достаточно транскодера через внешний сервис». Без этого тезис стоит.

### Т2. Двунаправленный G.729 достижим только через `oxideav-g729`, не через FFmpeg

**ТЕЗИС.** «Снять блокировку G.729» упирается не в лицензию (патентный пул истёк в 2017), а в то, что FFmpeg умеет G.729 **только декодировать**; энкодера в апстриме нет и не было. Единственный путь — `oxideav-g729 0.0.8` (MIT, encoder::make_encoder + `encode_frame(&[f32;80])`, декодер end-to-end).

**ДОКАЗАТЕЛЬСТВО.** Репозиторий сам это пишет: `codec_worker/mod.rs:41` — `AudioCodec::G729` «FFmpeg can decode raw G.729 but does not encode it», `ffmpeg.rs:651` — `Err(Unavailable("FFmpeg has no G.729 encoder"))`, тест `ffmpeg.rs:1279` — `assert!(!caps.has_audio_encoder(G729))`. Мой прогон `ffmpeg -encoders` на хосте подтверждает: только `A....D g729` в декодерах. При этом в registry `oxideav-g729-0.0.8` есть `src/encode_chain.rs:213 pub fn encode_frame`, L_FRAME=80, и регистрация в oxideav-core под id `"g729"`. В `vks-gateway-sip/Cargo.toml` его нет — вот и вся «блокировка».

**ЧТО ПЕРЕУБЕДИТ.** Сборка FFmpeg с G.729-энкодером (например, с `libbcg729`), доступная как `VKS_FFMPEG_BIN`; либо доказательство, что `oxideav-g729` не проходит ITU-векторы (тогда моё предложение из 5.2.1 снимается, а G.729 остаётся 488).

### Т3. `full`-профиль SDP сегодня опаснее `native`: он обещает H.264 High, ответ которым мост отвергает

**ТЕЗИС.** Включение `VKS_SDP_CODEC_MATRIX=full` (то, что заказчик просит для полной матрицы) добавляет PT98 `6400…`, но `h264_baseline_level` принимает только `profile_idc == 66` (`rtp_bridge.rs:1284`). Терминал, выбравший High, получает звонок без видео — молча.

**ДОКАЗАТЕЛЬСТВО.** `VIDEO_OFFER` PT98: `rtp_bridge.rs:450-458` (native:false → только в Full). Разбор ответа: `video_identity` (`:1248-1257`) → `local_video_payload` → `h264_baseline_level` (`:1284`). Тест `:1545-1560` проходит только потому, что в его ответе присутствует и PT97. Собственный комментарий кода (`:544-547`) прямо описывает этот сценарий для Polycom RPD 3.5 и называет его «drop that RTP».

**ЧТО ПЕРЕУБЕДИТ.** Захваченный answer от реального терминала с `m=video … 98` (только High), в котором `negotiate_media_detailed` вернул `video_codec: Some("H264")`. Либо `VKS_GATEWAY_VIDEO_PROFILE=high`, который реально доезжает до `-profile:v`. Пока ни того, ни другого в коде нет.

### Т4. «Одна матрица» не может жить в `codec_capabilities`: нужен один тип `CodecSpec`, а не шесть списков

**ТЕЗИС.** Пока матрица — это `&[&str]` без связи с оффером и мостом, любой новый кодек требует правок в 6 местах, и расхождения неизбежны. Уже сейчас: `PCMU/PCMA` в матрице названы `G711/G711U/G711A`, а в оффере — `PCMU/PCMA`; `opus` в оффере строчными; `normalize` в 4 копиях с разными разделителями; H.323 объявляет G.723 без 30/20-мс накопителя SIP.

**ДОКАЗАТЕЛЬСТВО.** §2 «Что это значит для одной матрицы» + D7. Инвариантный тест `offer_native_flags_match_shared_capabilities` (`rtp_bridge.rs:867-880`) сверяет **только** `native`-флаг и **только** AUDIO_OFFER/VIDEO_OFFER — он не видит ни `bridgeable_audio`, ни `AudioBridgeCodec`, ни settings/UI. Т.е. тест создаёт иллюзию согласованности.

**ЧТО ПЕРЕУБЕДИТ.** Показать, что все шесть списков выводятся из одного (сейчас — нет: G7221 есть в оффере и в настройках, но не в матрице; AAC есть в мосте, но не в матрице). Либо доказать, что расхождение «алиасы vs имена» безвредно — но тогда зачем `is_supported_audio_variant` с отдельной нормализацией (`codec_capabilities.rs:86-105`)?

### Т5. `oxideav-scene` по умолчанию — регресс, а не улучшение; билинейный скейлер надо писать самим (~150 строк)

**ТЕЗИС.** `oxideav-scene` — модель сцены/анимации/таймлайна с векторным растеризатором (`rasterize_vector`, `build_frame`), а не YUV-композитор. В 0.1.4 нет ни `scale`, ни `resize`, ни `bilinear` (проверил `render.rs`, `raster.rs`, `raster_renderer.rs`, `paint.rs`), зато в зависимостях `oxideav-raster`, `oxideav-scribe`, `oxideav-ttf` (шрифты). Включение по умолчанию добавит вес сборки и не даст ни одного пикселя билинейной интерполяции.

**ДОКАЗАТЕЛЬСТВО.** `Cargo.toml` `oxideav-scene-0.1.4`: deps `oxideav-core/pixfmt/raster/scribe`; `src/raster.rs:33 pub fn rasterize_vector(frame: &VectorFrame, w, h) -> VideoFrame` — растеризация вектора, не ресемплинг I420. В нашем дереве `oxideav_scene` — **только re-export** (`vks-media-mcu/src/lib.rs:64-66`), ни одного вызова. При этом текущий nearest-скейлер уже имеет memcpy-путь 0.26 мс/1080p и эталонный тест.

**ЧТО ПЕРЕУБЕДИТ.** (а) Бенчмарк: `oxideav-scene 0.1.4` компонует 1080p I420 быстрее 2.0 мс/кадр и умеет билинейную выборку (я такой API не нашёл); (б) либо решение «нам нужна анимация сцены/таймлайн, а не скейлер» — тогда это отдельная задача O-серии, а не замена композитора.

### Т6. Снятие AAC-флага без фикса `cpresent` даёт 200 OK и тишину — то есть ровно то, от чего флаг «защищал»

**ТЕЗИС.** Флаг `VKS_AAC_LATM_LEGAL_ACCEPTED` задумывался как «не объявлять, пока нет уверенности». Но реальная защита от «мёртвого аудио» — fmtp-гейт; и в нём дыра: отсутствующий `cpresent` считается `true`, хотя по RFC 3016 значение по умолчанию — **0** (конфиг в `config=`, in-band SMC нет). Наш `LatmDecoder` умеет **только** in-band SMC (`codec_worker/latm.rs`, `add_loas` + loas-демуксер).

**ДОКАЗАТЕЛЬСТВО.** `uas.rs:476-502`: `let mut cpresent_ok = true;` и проверка только присутствующего параметра; `bridgeable_audio` для AAC возвращает `Some` без проверки наличия `config=`; ответ объявляет `cpresent=1` (`uas.rs:462-475`). Т.е. пир с `object=2` без `cpresent` (легальный) получает 200 OK и неработающий декод.

**ЧТО ПЕРЕУБЕДИТ.** Захват потока от такого пира, где SMC всё-таки приходит в первом `audioMuxElement` (это легально, но тогда пир обязан был объявить `cpresent=1`), **и** подтверждение, что FFmpeg-`loas`-демуксер синхронизируется без SMC. До этого фикс §5.1.2 обязателен — иначе «снятие блокировки» ухудшает продукт.

### Т7. Флаг «LEGAL_ACCEPTED» — не юридическая защита, а журнал решения; правильная форма — профиль лицензий

**ТЕЗИС.** Булев `VKS_AAC_LATM_LEGAL_ACCEPTED` не меняет юридическую экспозицию: FFmpeg с AAC-энкодером уже в дереве и уже используется для H.264/H.265 (libx264/libx265 — тоже лицензируемые), а флаг лишь убирает AAC из SDP. Он не фиксирует **кто**, **на каком основании** и **для каких кодеков** принял решение — т.е. не является аудиторским следом. Честная форма: `VKS_CODEC_LICENSE_ACCEPTED=aac-latm,g729` (список), лог на старте, запись в `deploy/vks.env.example` с формулировкой «решение оператора», и **никогда** — «patent-free».

**ДОКАЗАТЕЛЬСТВО.** `main.rs:390`: `is_ok_and(|v| v.trim() == "1")` — ни кодеков, ни личности, ни даты; `docs/implementation/CODEC_WORKERS_2026-10-23.md:100` называет это «явное юридическое согласие»; при этом тот же FFmpeg запускается для H.264/H.265 без всякого согласия (`gateway_video.rs:304`), т.е. «согласие» избирательно и не следует из фактической экспозиции. G.729, для сравнения, royalty-free с 2017 ([источник](https://en.wikipedia.org/wiki/G.729)) — и никакого флага не имеет.

**ЧТО ПЕРЕУБЕДИТ.** Юридическое заключение, что AAC-LC в этой юрисдикции требует лицензии и что флаг-булев достаточен как согласие; либо требование заказчика «флаг должен быть именно булевым» с указанием, кто его читает.

### Т8. VP8/VP9/AV1 в `NATIVE_VIDEO` — категориальная ошибка, а не «полная матрица»

**ТЕЗИС.** `NATIVE_VIDEO` — это «что шлюз кодирует/декодирует в SIP-мосте». VP8/VP9/AV1 не имеют ни payloader'а, ни SDP-строки (`VIDEO_OFFER` — только H264/H265), ни одного SIP-терминала, который их предложит. Их место — `WEBRTC_VIDEO` (WebRTC-разрез уже живёт отдельным списком в `main.rs:361-366`: opus/vp8). Добавление их в `NATIVE_VIDEO` сделает ложным `ensure_bridgeable_codec_selection` (`gateways.rs:147-190`) и публичный API (`shell.rs:288`).

**ДОКАЗАТЕЛЬСТВО.** `rtp_bridge.rs:440-471` — три строки, ни одной VP8/VP9/AV1; `uas.rs:626-828` — только H264/H265; в registry нет `oxideav-vp8/vp9/av1`; `NATIVE_VIDEO = ["H264","H265","HEVC"]` (`codec_capabilities.rs:16`) и тест `offer_native_flags_match_shared_capabilities` немедленно упадёт при добавлении VP8 без оффера.

**ЧТО ПЕРЕУБЕДИТ.** Реальный SIP/H.323-терминал, отвечающий `VP8/90000`; либо явное требование «публичная матрица = матрица WebRTC-комнаты, а не SIP-шлюза» — тогда надо переименовать API и разделить `transport`, а не смешивать.
