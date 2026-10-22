# T06b-1 — G.722 end-to-end (SIP outbound + inbound), strict-first negotiation

Дата: 2026-10-22. Статус: код/тесты/E2E готовы; ревью ref-engines **9.5/10 APPROVED**.
Scope: только SIP + G.722; остальная матрица аудио/видео и H.323 — T06b-2..T06b-5.

## Что сделано

1. **G.722 native (MIT, `oxideav-g722 =0.0.7`)**
   - новый `crates/vks-gateway-sip/src/g722.rs`: `G722Codec`, 14-bit PCM
     scaling, тесты 20 мс = 160 октетов/320 samples и стабильность шкалы;
   - `NATIVE_AUDIO` + `AUDIO_OFFER` PT9/8000/1 `native: true`;
   - native SDP-оффер: `m=audio ... 0 8 9 101`.

2. **Строгий выбор первого кодека (P1 dead-audio fix)**
   - `negotiate_media_detailed` выбирает **первый общий** payload из answer;
     если он не native — `UnsupportedAudio{codec,payload}`, без позднего
     PCMU fallback (Polycom перечисляет G722.1C раньше PCMU);
   - static PT учитывается только если реально присутствует в `m=` строке
     (раньше PT9 «существовал» в PCMU-only offer и давал ложный G722);
   - outbound UAC на 200 с несовместимым первым кодеком: ACK + BYE +
     типизированный `NoCommonCodec` (UAC не может отправить 488 после 200 —
     это возможно только в UAS до 200 OK); тест теперь проверяет и BYE.

3. **Inbound SIP UAS G.722**
   - порядок payload в offer = порядок предпочтения; первый мостимый
     (PCMU/PCMA/G722) выбирается, PT0/8/9 проверяются по `a=rtpmap`
     (PCMU/PCMA/G722, clock 8000);
   - 200 OK для G722-only: `m=audio ... 9` + `a=rtpmap:9 G722/8000` +
     `a=ptime:20`; G729-only получает wire-level 488; подмена PT0/8/9
     другим кодеком отвергается (unit + wire-level test).

4. **RTP-мост**
   - G722: decode 16 кГц → 48 кГц MCU mix → encode 16 кГц, 20 мс;
   - payload **160 октетов**, timestamp **+160** (RTP clock 8000, не 16000);
   - decoded 14-bit PCM clamp в [-1,1] до MCU-микшера;
   - G711 40 мс (320 B) пакет не режется (`max_payload=480`) — P0-регресс,
     пойманый в ходе работы, закрыт тестом;
   - `RtpSource` принимает PT9; DTMF/RTCP/mute/gain не изменены.

5. **Наблюдаемость**
   - media details: `audio_codec=G722`, `audio_payload_type=9`,
     `audio_sample_rate=16000`, `audio_clock_rate=8000`;
   - лог остановки моста содержит codec, без «G711 bridge» для G722.

6. **Кросс-протокольная честность**
   - H.323 precheck использует отдельный `H323_AUDIO` (G711U/PCMU/G711):
     G722-only на `h323:` отклоняется до дозвона, SIP-wideband не протекает
     в H.323; H.323 остаётся G711U/H264 (T06b-4).

## Доказательства

- `docs/progress/evidence/t06b1/result.json`: live Playwright `ok=true`,
  **233/233** проверок, 146.6 с, `VKS_VIDEO_LOW=1` (640×360/512 kbps);
  audio TX **545** / RX **361**, `rejected_packets=0`, участник в раскладках,
  подписи/UI/preview проверки пройдены; отдельный check требует
  `audio_codec=G722`, PT9 на RX/TX, `audio_sample_rate=16000`,
  `audio_clock_rate=8000`, `audio_codec_running=true` — молчаливый
  PCMU-fallback больше не пройдёт E2E.
- `docs/progress/evidence/t06b1/network.json`: **84** тел ответов с media-полями
  `"audio_codec":"G722"`, `"audio_payload_type":9`, `audio_sample_rate:16000`,
  `audio_clock_rate:8000`, `audio_codec_running:true`. Пример:
  `audio_tx_payload_type:9`, `audio_tx_packets:157`.
- E2E-команда: `VKS_VIDEO_LOW=1 NODE_PATH=/home/pro/.npm/_npx/e41f203b7505f1fb/node_modules
  VKS_ARTIFACTS=docs/progress/evidence/t06b1
  VKS_CHROMIUM=/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome
  node tests/e2e/live.js`. `result.json`: 233 checks, 0 failed, 146.6 с;
  `page_errors=[]`; в `console_errors` 8 вендорских TypeError (T12c, не
  проверки E2E; страница работает).
- Wire-level тесты: inbound UAS G722-only → 200 PT9, G729-only → 488;
  outbound UAC unsupported-first → ACK + BYE; spoofed PT0/8/9 → отказ.
- Rust:
  - `cargo test -p vks-gateway-sip --lib` — **72 passed**;
  - `cargo test -p vks-server --lib` — **72 passed, 3 ignored**;
  - `cargo test --workspace` — все suite зелёные;
  - `cargo clippy --workspace --all-targets -- -D warnings` — clean;
  - `cargo fmt --check` — clean.

## Что осознанно вне T06b-1

- Остальные аудиокодеки (G729/G728/G723/G726 16/24/32/40/Opus/AAC/G719/
  G722.1/G722.1C) и видео (H261/H263/H264-high/H265) — T06b-2..T06b-5.
  Отсутствующие encoders (G719/G722.1(C), H265 software) требуют
  лицензированного кодека или явного 488 — см. T06b-2..5.
- H.323 G722/остальные кодеки — T06b-4; H.323 остаётся G711U/H264.
- G.722 поддерживается только Mode1 64 kbit/s (offer `G722/8000` не несёт
  bitrate); 56/48 kbit/s режимы и data insertion — T06b-2. Legacy
  `CodecMap`/`incoming_sip_call` не используются UAS-путём и не знают G722 —
  оставлено как P2 до T06b-4/удаления legacy API. Ресемплер
  16k↔48k — линейная интерполяция без anti-alias, для голоса достаточно,
  качественный resampler — T06b-2.
- 1080p30 high-profile live fps 13 на текущем хосте — существующая
  CPU-проблема FFmpeg/libx264 `-threads 1` (вне T06b-1); для T06b-1
  использован low-video профиль, все 233 проверки UI/раскладок прошли.
