# T06b-2c — G.723.1 native (PT4, 30 ms)

Дата: 2026-10-22. База: T06b-2b `246ab97` / docs `15cd476`.
Закрывает последний реально реализуемый аудиокодек матрицы **для SIP**:
G.723.1 теперь мостится нативно, а не отклоняется 488 (H.323-ветка — T06b-4).

## Реализация

- `crates/vks-gateway-sip/src/g7231.rs`: `oxideav-g7231 = "=0.0.9"` (MIT, pure
  Rust, cached), `SpecEncoder(PackedRate::High)` = MP-MLQ 6.3 кбит/с,
  24 B / 240 сэмплов / 30 мс; декодер `SynthesisState` принимает и 24 B
  (6.3k), и 20 B (ACELP 5.3k) на том же PT4, несовместимые длины — отказ.
- RTP: static PT4 `G723/8000`, `a=fmtp:4 annexa=no` в offer (VAD/SID не
  включён), timestamp **+240** за пакет, `a=maxptime`/`ptime` не обещают 20 мс.
- Bridge: MCU-тик остаётся 20 мс (`frame_samples=160`), исходящий звук
  накапливается в 240 сэмплов (`packet_samples=240`), пакет уходит раз в
  30 мс; sequence/timestamp инкрементируются только при фактической
  отправке. Входящие 240 сэмплов ставятся в очередь и потребляются по 160
  за тик, поэтому 30-мс поток не рвётся.
- UAS: static PT4 и dynamic `G723`/`G7231` на 8 кГц; answer
  `m=audio ... 4`, `a=rtpmap:4 G723/8000`, `a=fmtp:4 annexa=no`,
  `a=ptime:30`; dynamic PT 96 с `G723/8000` даёт тот же fmtp и ptime 30;
  неверный clock (16 кГц) отвергается. Mixed offer
  по-прежнему выбирает первый поддерживаемый кодек.
- Settings: `NATIVE_AUDIO` содержит `G723`, API `supported:true`;
  native offer `0 8 9 15 4 107 108 109 110 111 101`.
- Outbound guard принимает PT4; H.323 sidecar по-прежнему только G711/H264,
  поэтому `h323:` с G723 будет отклонён до вызова (T06b-4).

## Тесты и ворота

- `vks-gateway-sip --lib`: **90 passed, 1 ignored**.
  - G723 roundtrip 240/24 B, обрезка/oversize;
  - UAS PT4: accepted, ptime 30, answer rtpmap + `annexa=no` + `maxptime:30`;
    dynamic PT96 G723 — тот же fmtp/ptime; неверный clock -> 488; 488
    остаётся для G729/G7221/G719/AAC-LATM; ACELP 20 B декодируется.
- `vks-server --lib`: **80 passed, 3 ignored**.
  - direct bridge: frame 160 / packet 240 / timestamp 240 / payload 24 B;
  - full RTP loop: inbound PT4 -> MCU -> outbound PT4, 24 B, timestamp +240,
    outbound payload декодируется в 240; 10 последовательных 30-мс пакетов
    и проверка отсутствия zero-underrun после 480-sample prefill;
  - wire-level UAS INVITE PT4 -> 200 + `a=ptime:30`; G728/PCMA/G722 тесты
    не сломаны.
- `vks-common --lib`: **77 passed**.
- fmt/clippy clean; `gates.log`.
- Live low-video E2E на финальном бинаре: **233/233, 149.2 s**, G722/PT9/
  16000, 0 rejected, 8 console errors (не pageerror; известный T12c UI-шум),
  native offer расширен G723, `.132` по-прежнему выбирает G722 (он раньше в
  answer), поэтому live G723 не доказан.

## Ограничения

- G.723.1 не проверен живым терминалом и 20-call soak; только unit/wire/RTP-loop.
- FEC/PLC для G723 нет (как и для G711/G722/G726/G728): при потере пакета
  очередь просто опустошается.
- G.723.1 Annex A (VAD/CNG/SID) выключен (`annexa=no`), 4-байтные SID и
  1-байтные untransmitted фреймы не принимаются.
- H.323 G723 не поддержан (sidecar PT0 only) — T06b-4; settings `supported`
  глобальные и для SIP, H.323-путь по-прежнему откажет до вызова.
- Encoder G723 имеет 7.5 мс lookahead; bridge его не компенсирует
  (дополнительная задержка ~7.5 мс, не ошибка формата).
- Legacy `CodecMap`/`incoming_sip_call` знает только G711/OPUS/G729 и
  отклонит G723 — не production-путь, документированное ограничение.
- Native offer без `a=ptime` из-за 30-мс G723; для 20-мс кодеков это лишь
  отсутствие предпочтения, не ошибка (RFC 4566).
