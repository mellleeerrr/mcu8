# T06b-2a — G.726 16/24/32/40 kbit/s (SIP RTP, LSB-first)

Дата: 2026-10-22. Scope: SIP G.726 все четыре rate; H.323 по-прежнему G711U-only.
Полный T06b-2 (G723.1/G728/G729/Opus/AAC/G719/G722.1(C)) остаётся открытым.

## Реализация

- `crates/vks-gateway-sip/src/g726.rs` (новый):
  - `oxideav-adpcm = "=0.0.7"` (MIT, зависит только от уже присутствующего
    `oxideav-core`; `soundkit-g726` отвергнут — 29 транзитивных пакетов,
    включая crypto, и `Left` по умолчанию);
  - `G726Rate::{R16,R24,R32,R40}` ↔ PT 107/108/109/110, имена
    `G726-16/24/32/40`, RTP clock 8000;
  - `G726Codec` — раздельные `State` для encode/decode, `BitPacker`/
    `BitUnpacker` с **`BitOrder::LsbFirst` для всех четырёх rate**;
  - payload: 20 мс = 40/60/80/100 B, 40 мс = 80/120/160/200 B, 30 мс
    (60/90/120/150 B) тоже принимается как целое число кодовых групп;
  - `accepts_payload_len` проверяет `len % group_bytes == 0` и ≤ 40 мс;
    частичная группа (например 61 B для G726-24) отклоняется до декодера,
    чтобы residual bits не рассинхронизировали поток.

- `crates/vks-server/src/gateway_audio.rs`:
  - `AudioBridgeCodec` вместо флага `is_g722`: G711 / G722 / G726;
  - один 20-мс тик, 48-кГц MCU mix, timestamp +160 для всех rate;
  - G726 decode: i16 / 32768 → f32; encode: gain в f32, clamp, *32768 → i16;
  - декодирование выполняется **до** проверки mute, чтобы состояние
    декодера не рассинхронизировалось при снятии mute;
  - входящий пакет учитывается в `audio_rx_packets` только после успешного
    decode; oversize/partial → `rejected_packets`;
  - `max_payload` 200 B для G726 (40 мс G726-40), 480 B для G711/G722.

- Интеграция: `AUDIO_OFFER` PT 107–110 `native: true`; native SDP
  `m=audio ... 0 8 9 107 108 109 110 101`; `NATIVE_AUDIO` содержит только
  точные варианты `G72616/24/32/40`; `is_native_audio_variant(name,bitrate)`
  не даёт `G726:48`/`G726:null` пройти precheck; settings `supported`
  использует этот helper; UAS принимает G726-only оффер с этими PT и
  отвечает соответствующим `a=rtpmap`; `audio_codec_name` и media details
  отдают `G726-16..40`, `audio_sample_rate=8000`.

## Тесты

- `cargo test -p vks-gateway-sip --lib`: **78 passed, 1 ignored**.
  - длины 20/40 мс и 30 мс, group-boundary rejection;
  - roundtrip всех четырёх rate, энергия не тишина;
  - golden LSB-first `1..8 → 21 43 65 87`;
  - native offer, strict-first выбор каждого G726 rate;
  - динамический PT: `a=rtpmap:96 G726-32/8000` → PT96 сохраняется,
    answer содержит `m=audio ... 96` и `a=rtpmap:96 G726-32/8000`;
  - ignored external probe `ffmpeg_g726le_decodes_our_payloads_at_all_rates`.
- `cargo test -p vks-server --lib`: **74 passed, 3 ignored**.
  - wire-level UAS: G726-only `RTP/AVP 109` → 200 `m=audio ... 109` +
    `a=rtpmap:109 G726-32/8000`;
  - bridge на **peer dynamic PT 96** с `audio_codec=Some("G726-32")`:
    20 мс 80 B, timestamp +160, PCM доходит до 48-кГц MCU; 40 мс вход 160 B
    даёт два 20-мс пакета по 80 B с timestamp +160, не +320; oversize 161 B
    → `rejected_packets`, `audio_rx_packets` не растёт;
  - dynamic PCMA PT96: A-law identity сохраняется (encode PCMA ≠ PCMU,
    decode возвращает 0.25), чтобы кодек не зависел от номера PT;
  - precheck G726:16/24/32/40 проходит, G726:48 и G726 без bitrate
    отклоняются, H.323 G726 отклоняется до дозвона.
- `cargo test -p vks-common --lib`: **77 passed** (variant-aware capability;
  `G72616` с bitrate 48 тоже отклоняется).
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`:
  clean.

## FFmpeg interop

Наш поток декодируется FFmpeg в RFC-порядке `g726le`:

```
ffmpeg -f g726le -code_size {2|3|4|5} -ar 8000 -ac 1 -i out.g726 \
       -f s16le -ac 1 out.s16le
```

`ffmpeg_g726le_decodes_our_payloads_at_all_rates` (ignored test) прогнан на
хосте: все 4 rate → 320 сэмплов (640 B PCM) и ненулевая энергия. Порядок
`g726` (MSB-first/AAL2) для G726-16/24/40 даёт мусор — поэтому в коде
зафиксирован `BitOrder::LsbFirst`, а не default `MsbFirst`.

## Логи

- `docs/progress/evidence/t06b2a/gates.log` — fmt/clippy/test summary.
- `docs/progress/evidence/t06b2a/ffmpeg-probe.log` — ignored FFmpeg probe.
- `docs/progress/evidence/t06b2a/result-e2e-2026-10-22.json` — live low-video
  прогон после добавления G726 в native-оффер.

## Live E2E

`.132` не предлагает G726, поэтому live-вызов остаётся G722:
low-video Playwright **233/233, 0 failed, 146.5 s**, audio TX 541 / RX 343,
`rejected_packets=0`, G722/PT9/16000/8000 check проходит. Артефакт
`docs/progress/evidence/t06b2a/result-e2e-2026-10-22.json`; постоянный
evidence T06b-1 не перезаписывался, потому что G726 в живом вызове не участвует.

## Ограничения / что дальше

- Inbound динамический PT 96–127 поддержан для G726/PCMU/PCMA/G722; для
  H.323 динамические PT/TCS остаются T06b-4.
- G726-16 и G726-24 не toll quality по RFC 3551; это документированное
  свойство кодека, не дефект моста.
- Outbound dynamic PT по-прежнему ограничен нашим оффером 107–110;
  compliant answer зеркалит эти номера. Bridge-тесты — G726-32; unit-тесты
  покрывают 16/24/40 и 30 мс, отдельного 20-call G726 soak нет.
- 20-call G726 load/CPU soak не выделен в отдельный тест; общий 20-call UAS
  тест остаётся PCMU/G722. Bridge-тест покрывает 20 и 40 мс на динамическом
  PT, unit-тесты — все четыре rate и 30 мс.
