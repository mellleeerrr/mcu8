# T05b — H.323 H.241 и SFU b=AS (частично закрыто)

**Статус:** T05b-1 закрыт **9.5/10 APPROVED** (`media-engine`, HEAD `c26e185`):
H.323-часть и per-media `b=AS` реализованы и покрыты unit-тестами. Live H.323
и per-contact override — T07/T05b-2; полный T05b не закрывать до их приёмки. Коммиты part 1:
`7b05905` (H.323), `68da633` (SFU), `78153a1` (trace/диагностика), `5ae9924`
(P2: clamp/normalization/TCS-OLC-тест) и docs-фиксация part 1.

## H.323 H.241
- `H323VideoProfile { max_bitrate_kbps, level_idc }` в `vks-gateway-h323`;
  `dial_with_media_profile` кладёт `video_max_bitrate_100bps` (kbps*10) и
  `video_h264_level` (level_idc+40) в JSON sidecar.
- `gateways.rs` строит профиль из уже разрешённого `VideoProfile`
  (битрейт 4096 → 40960, 1080p30 → level 4.0/80); `dial_with_media`/`dial`
  остаются default 512 kbps/3.1 для обратной совместимости POC.
- Sidecar `video_capability(config)` использует переданные значения в TCS и
  OLC; невалидный H.241 level отклоняется до TCP.
- Тесты: Rust 512→5120, 4096→40960, 30→70, 31→71, 40→80, 45→82; неизвестные
  level_idc нормализуются вниз до таблицы `70/71/72/80/81/82/90/91`; Python
  22 теста, включая custom profile, TCS==OLC и отказ level=75;
  Polycom-захваты остаются на дефолтном 5120/71.
- **Live-попытка 2026-10-22:** `remote_release` и для 4096/4.0, и для
  дефолтного 512/3.1; это T07-поведение терминала, не регрессия профиля.
  H.323 live-proof не получен, пока терминал не принимает вызов. Диагностика
  `VKS_H323_TRACE=1`: терминал принимает TCP 1720, но не отвечает на SETUP;
  `acceptance/README` фиксирует, что SIP 5060 и H.323 1720 у терминала
  взаимоисключающие, поэтому нужен свободный от SIP слот (T07).

## SFU b=AS
- `SfuAnswerProfile.b_as/fmtp/rtcp_fb` теперь применяются только к video
  m-line; audio их не получает.
- `HandlerConfig.video_bitrate_kbps` (env `VKS_SFU_VIDEO_BITRATE_KBPS`,
  default 2048, clamp 64..20000 в конструкторе и в `answer_profile`) и
  `with_video_bitrate`; answer объявляет `b=AS` только на video.
- Тесты: audio `b_as=None/fmtp=[]`, video `b_as=Some(2048)/fmtp` есть;
  clamp 0→64 и 100000→20000.

## Гейты
`cargo test --workspace --all-targets --no-fail-fast` — **1025 passed / 0
failed / 6 ignored**; `clippy -D warnings` и `fmt --check` чисто; Python
sidecar 22/22 OK.

## T05b-2 (в работе)
- SFU answer теперь является подмножеством offer: RTP maps фильтруются по
  kind, payload type переиспользуется из offer, cross-kind maps не попадают
  в m-line, `SdpAnswer.rtp_map` содержит только выбранные maps; m-line без
  общего кодека получает port 0. Тест: offer opus 120/vp8 100 → answer
  120/100, audio без vp8, video без opus. Гейты: **1026 passed / 0 failed /
  6 ignored**, clippy/fmt чисто. Регрессионный SIP E2E:
  `artifacts/e2e-t05b-sfu-high/` — 114/114, page_errors 0, 0 failed.
- Per-room `b=AS`: `handle_ws_socket` перед `SdpOffer` резолвит
  `conference_settings[room].payload.bitrate` и вызывает
  `SignalingHandler::set_video_bitrate`, env остаётся fallback; тест сеттера
  и клампа 4096/0/100000.
- Caps SFU больше не обещают H.264: браузерный SFU объявляет только VP8
  (H.264 идёт через SIP/H.323-шлюз); H.264/H.265 в browser-leg — только после
  реального forwarding в T06b/T08.
- Осталось: call_string live proof, peer H.241 clamp, packetization/profile
  в sidecar, Python CI, live H.323/T07, H.264 SFU forwarding.

## Ограничения / следующие шаги
- SFU: нет per-room инъекции (env глобальный), H.264 отсутствует в
  `vks-media-sfu` RTP map; answer не фильтрует RTP maps по kind и не
  пересекается с offer. Нельзя объявлять H.264/H.264-high/H.265, пока SFU
  реально их не мостит (T06b/T08).
- H.323: peer H.241-лимиты не читаются/не клампятся; `media_details` не
  содержит H.241; packetization/profile захардкожены baseline; bearer 384
  оставлен как POC-решение.
- Call-string live proof: UI-сессия пишет контакт только в
  `scheduled_calls`, а `find_call_string_override` читает `resources.contacts`;
  нужен template-contact + PATCH + recall или product-fix персистентности.
- Python sidecar-тесты пока не в CI; прогон вручную:
  `/home/pro/.local/share/vks-mcu/h323-venv/bin/python3 -m unittest test_h323_sidecar`.
