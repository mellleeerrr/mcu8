# T05b — H.323 H.241 и SFU b=AS

**Статус:** T05b-1 закрыт **9.5/10 APPROVED** (`media-engine`, HEAD `c26e185`).
T05b-2 SFU-часть доведена до ревью-кандидата **9.5** на HEAD с кодом `25505fe`
+ последующие фиксы; полный T05b **не закрыт** до live H.323/T07 и
call_string live proof.

## H.323 H.241 (part 1 + T05b-2)
- `H323VideoProfile { max_bitrate_kbps, level_idc, profile_bits }`;
  `dial_with_media_profile` кладёт `video_max_bitrate_100bps`,
  `video_h264_level` и `video_h264_profile_bits` в JSON sidecar.
- `gateways.rs` строит профиль из разрешённого `VideoProfile`
  (4096 → 40960, 1080p30 → level 4.0/80, baseline bits 64); `dial`/
  `dial_with_media` остаются default 512/3.1/64 для POC-совместимости.
- Sidecar: TCS и OLC используют один и тот же профиль; level вне
  `70/71/72/80/81/82/90/91` и profile != 64 отклоняются до TCP.
- **Peer H.241**: входящий TCS парсит `maxBitRate`, generic parameter 42
  (level_idc) и 41 (profile bits); `connected` отдаёт эти поля, Rust
  `ActiveH323Session` хранит их, gateway клампит битрейт и уровень энкодера
  по лимитам терминала (`h264_level_name` — белый список).
- Packetization: sidecar объявляет H.241 non-interleaved RTP packetization
  и проверяет её в OLC; bearer 384 kbit/s оставлен как POC-решение.
- Тесты: Rust bitrate/level/profile, live-профиль; Python **23/23**
  (custom profile 40960/80/64, TCS==OLC, отказ level=75 и profile=128,
  peer H.241 helper, Polycom-захваты на 5120/71).
- **Live-попытка:** `remote_release` и для 4096/4.0, и для 512/3.1; терминал
  принимает TCP 1720, но не отвечает на SETUP. `acceptance/README`: SIP 5060
  и H.323 1720 у терминала взаимоисключающие — нужен свободный слот (T07).
  H.323 live-proof не получен, это не регрессия профиля.

## SFU b=AS и RTP maps (T05b-2)
- Answer — подмножество offer: RTP maps фильтруются по kind, PT берётся из
  offer, cross-kind maps не попадают в m-line, `SdpAnswer.rtp_map` содержит
  только выбранные maps; нет общего кодека → port 0.
- Rejected m-line валиден по RFC 8866: сохраняет fmt из offer (или `0` для
  Data без форматов), `fmtp`/`rtcp-fb` пустые, SDP парсится обратно.
- Per-subscriber PT rewrite: `SfuEngine::set_subscriber_payload_type` +
  `route_udp_packet` переписывают байт PT на egress, marker сохраняется;
  тест двух подписчиков 96/100. `answer_payload_types` — ровно та функция,
  что вызывает WS после `SdpAnswer`.
- `rtcp-fb`/`fmtp` привязаны к выбранному PT; H.264 fmtp не приклеивается к
  VP8. При будущем H.264 SFU нужен per-codec PT/fmtp/feedback — T06b/T08.
- Per-room `b=AS`: перед `SdpOffer` резолвится
  `conference_settings[room].payload.bitrate`; при отсутствии — env/конструктор
  не затирается (`Option<u32>`); тесты resolver→answer и `answer_payload_types`.
- Payload адреса из модалки доходит до `dial_with_contact`: явные
  `bitrate`/`resolution` приоритетнее сохранённого contact, а null-поля
  `default_contact_payload` не считаются override.
- Caps SFU: только VP8 (H.264/H.265 — SIP/H.323-шлюз), чтобы не обещать
  forwarding, которого нет.

## Гейты (HEAD с фиксами T05b-2)
- `cargo test --workspace --all-targets --no-fail-fast` — **1036 passed /
  0 failed / 6 ignored**.
- `clippy --workspace --all-targets -D warnings` и `fmt --check` — чисто.
- Python sidecar — **23/23 OK**; CI job с pinned Pycrate добавлен (`0a4dd89`).
- Живой SIP E2E на финальном бинарнике:
  `artifacts/e2e-t05b2-final-high/` — 114/114, `final-low/` — 114/114,
  page_errors 0, failed 0.

## Остаток T05b-2 / ограничения
- Live H.323/T07: CONNECT + двусторонние audio/video + H.241 peer, только
  на свободном от SIP терминале.
- call_string live proof: нужен template-contact + PATCH + recall с реальным
  абонентом (product-fix персистентности session-контакта не делался).
- H.264/H.265 в browser SFU — только после реального forwarding (T06b/T08);
  сейчас caps честно VP8-only.
- WS-интеграционный тест ограничен проверкой `answer_payload_types` и
  resolver→answer, без поднятия реального WebSocket-соединения.
- Future H.264: один PT на kind недостаточен для RTX/FEC и двух видеокодеков.
