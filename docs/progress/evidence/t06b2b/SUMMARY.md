# T06b-2b — Opus + G.728 native, остальные кодеки матрицы — явный 488

Дата: 2026-10-22. База: T06b-2a (`17b8660`, P2 `d6c32c8`).
Scope: OPUS и G.728 реально мостятся; G.723.1/G.729/G.722.1(C)/G.719/AAC-LATM
отклоняются до 200 OK с явной причиной (нет production-кодека или лицензия).
Полный T06b не закрыт: H.323/видео — T06b-3..5.

## Почему именно эти кодеки

| Кодек | Лицензия/патенты 2026 | Реализация | Решение |
|---|---|---|---|
| OPUS | royalty-free (BSD, патентный грант) | `rusty-opus 1.0.2`, pure Rust, 0 зависимостей, cached | **native** |
| G.728 | патенты истекли, MIT-реализация | `oxideav-g728 0.0.8`, Annex G fixed-point, pure Rust | **native** |
| G.723.1 | патенты истекли | `oxideav-g7231 0.0.9` есть, но 30 мс packetization/CPU не интегрированы | 488 (T06b-2c) |
| G.729 | royalty-free, но нет production-кодека | `oxideav-g729` не bit-exact; FFmpeg только decoder | 488 |
| G.722.1 / C | Polycom license | реализации нет; Polycom ставит G7221 раньше G722 | 488, не в native offer |
| G.719 | **Polycom + Ericsson license** | реализации нет | 488, license |
| AAC-LATM | патенты до ~2028/2031 | `oxideav-aac` есть, LATM нет | 488, patent |

FFmpeg GPL-сборка используется только как оракул, не линкуется.

## Opus

- `crates/vks-gateway-sip/src/opus.rs`:
  - `rusty-opus = "=1.0.2"` (BSD-3-Clause, pure Rust, без FFI/C-тулчейна);
  - 48 кГц — родная частота MCU-микса, поэтому **нет ресемплинга**;
  - 20 мс = 960 сэмплов, RTP timestamp **+960**, payload ≤1275 B;
  - моно-энкодер/декодер (`stereo=0`/`sprop-stereo=0` в fmtp), 64 кбит/с,
    complexity 5, `use_inband_fec=true`, `packet_loss_perc=10`; на приёме
    bridge отслеживает sequence-gap: при потере ровно одного пакета
    вызывает `decode_fec` следующего (RFC 6716 §4.5.3), иначе до 3 PLC-фреймов
    (`decode(&[])`); пустой payload Opus разрешён только для Opus-PT;
  - DTMF telephone-event ведёт отдельный 8-кГц timestamp (+160 за тик),
    не 48-кГц аудио-домен Opus;
  - пустой payload — защитный PLC-сигнал по потере (не валидный Opus-пакет
    по RFC 7587), даёт 960 сэмплов; oversize отклоняется до декодера;
  - декодер принимает все легальные длительности 2.5–120 мс (120…5760,
    кратные 120 сэмплам, включая 80/100 мс), а не только 20 мс; очередь
    bridge — до 120 мс (5760); тест `decodes_all_legal_opus_packet_durations`
    покрывает 10/40/80/100 мс.
- SDP: PT111 `opus/48000/2` + `a=fmtp:111 useinbandfec=1;stereo=0;sprop-stereo=0`,
  `a=ptime:20`, `a=maxptime:20`; native offer
  `0 8 9 15 107 108 109 110 111 101`.
- Inbound UAS: принимает `opus/48000/2` на любом dynamic PT 96–127,
  сохраняет PT пира, отдаёт `a=rtpmap:{pt} opus/48000/2` + fmtp + ptime 20;
  `InboundMedia` теперь несёт clock/channels/fmtp/ptime.
- Bridge: `AudioBridgeCodec::Opus`, 960 сэмплов за 20-мс тик, прямое
  48-кГц соединение с MCU, timestamp +960, packet buffer 1287 B.
- Settings: `NATIVE_AUDIO` содержит `OPUS`, `supported:true`; live API
  подтверждает `{"name":"OPUS","supported":true}`.

## G.728

- `crates/vks-gateway-sip/src/g728.rs`:
  - `oxideav-g728 = "=0.0.8"` (MIT, Annex G fixed-point, pure Rust; официальные
    ITU-векторы в репозитории не разложены, bit-exact против них здесь не
    перепроверялся);
  - 10-битный индекс / 5 сэмплов (0.625 мс), 20 мс = 32 вектора = **40 B**,
    160 сэмплов, timestamp +160;
  - float API ±4096, упаковка `encode_buffer`/`decode_bytes` спрятана в wrapper.
- SDP: static PT15 `G728/8000`, `native:true`; native offer содержит `15`.
- UAS: static PT15 принимается, answer `a=rtpmap:15 G728/8000`.
- Bridge: `AudioBridgeCodec::G728`, 160 сэмплов, 40 B, timestamp +160.
- Settings: `supported:true`, live API подтверждает.

## Явный 488

Политика выбора: UAS отвечает на первый **поддерживаемый** кодек в порядке
offer'а (RFC 3264); если в offer есть G729 и PCMU, выбирается PCMU — это не
«поздний fallback» и не «соединение без звука», а валидный выбор answerer'а.
Strict-first без fallback применяется к outbound UAC: если первый общий
кодек в answer не мостится, звонок не поднимается. Legacy
`SipGateway`/`CodecMap` (не production-путь, заменён custom UAS) по-прежнему
знает только G711/OPUS и не является источником истины о bridge-кодеках.
Тест
`licensed_or_unimplemented_audio_is_rejected_without_fallback` проверяет
G723 PT4, G729 PT18, G7221 16/32 кГц, G719, MP4A-LATM: `negotiate` → None →
UAS отвечает 488 до 200 OK; рядом стоящий PCMU всё ещё выбирается.
Для outbound UAC несовместимый answer даёт `UnsupportedAudio` и ACK+BYE;
native offer не содержит этих кодеков.

## Тесты и ворота

- `vks-gateway-sip --lib`: **86 passed, 1 ignored** (FFmpeg g726le probe).
  - Opus roundtrip 960 + PLC + oversize; G728 160/40 B + обрезка;
  - UAS Opus PT96 с 48 кГц/2 канала/fmtp, неверный clock отвергается;
  - UAS G728 PT15; 488 для G723/G729/G7221/G719/AAC;
  - native offer flags совпадают с `NATIVE_AUDIO` (12 записей: PCMU, PCMA,
    G711, G711U, G711A, G722, G72616/24/32/40, G728, OPUS).
- `vks-server --lib`: **78 passed, 3 ignored**;
  - bridge Opus на dynamic PT96: PCM доходит до MCU, исходящие PT96,
    timestamp +960, payload декодируется;
  - PLC/FEC: `opus_conceal_produces_plc_frames_and_bounds_long_gaps`
    (1 потеря = 960, длинный обрыв ограничен 3×960), пустой RTP-payload
    принимается только для Opus (`empty_rtp_payload_is_accepted_only_for_opus`),
    encode-failure больше не отправляет пустой пакет;
  - wire-level UAS 200 для G728 PT15; direct bridge-codec test G728
    160/40 B/timestamp +160 (полного RTP-loop теста G728 пока нет);
  - direct bridge-codec test Opus/G728 frame sizes + timestamp step;
  - UAC selection: native answer PT15 G728 и PT111 opus (`/48000/2`)
    выбираются без PT drift.
- `vks-common --lib`: **77 passed** (OPUS/G728 native, G729/G719/AAC/G7221C нет).
- fmt/clippy clean; `gates.log`.
- Live low-video E2E: **233/233, 0 failed, 148.3 s**, G722/PT9/16000,
  TX478/RX342, rejected 0; API `OPUS`/`G728` supported:true. В захваченном
  answer `.132` есть G728 PT15, но G722 PT9 идёт раньше, поэтому strict-first
  выбирает G722; Opus в answer отсутствует. Это regression proof, не
  доказательство live Opus/G728.

## Ограничения

- G.723.1: реализация есть, но 30 мс frame/timestamp + накопление 240
  сэмплов при 20-мс MCU-тике и CPU analysis-by-synthesis не введены;
  вынесено в T06b-2c. Сейчас 488, не «connected без звука».
- G.729/G.722.1(C): нет production-реализации; 488.
- G.719/AAC-LATM: лицензии/патенты; 488.
- Нет 20-call soak Opus/G728 и нет живого терминала с Opus/G728; общий
  20-call тест остаётся PCMU. Нет jitter-buffer/dejitter: PLC/FEC работают
  на sequence-gap, но не переупорядочивают пакеты.
- `DEFAULT_AUDIO_CODECS` по-прежнему содержит intent-список, включая
  ненативные кодеки; native-фильтр — `NATIVE_AUDIO`/SDP, не этот список.
