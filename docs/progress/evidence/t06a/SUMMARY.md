# T06a — честная матрица кодеков без транскодера

**Статус:** реализовано и проверено live, коммит `T06a-коммит (git log --grep T06a)`.
**Ветка:** master. **Гейты:** clippy `-D warnings` чисто; `cargo test
--workspace --all-targets --no-fail-fast` — **1019 passed / 0 failed / 6
ignored**; оба E2E — **114/114 checks, 0 failed, page_errors 0**.

## Что было нечестно

- Настройки/конференция/карточка контакта хранили полную матрицу (18 audio/8
  video, 16/2, пустой intent), но SDP строился из жёстких `AUDIO_OFFER`/
  `VIDEO_OFFER`; выбор пользователя ни на что не влиял.
- `VKS_SDP_CODEC_MATRIX=full` объявлял немостимые кодеки без транскодера.
- Ответ терминала с чужим кодеком терял причину (`UnsupportedAudio` →
  `InvalidSdpAnswer` → generic ошибка в UI).
- Терминал `192.168.1.132` отвечает G.722 первым, хотя Native-оффер его не
  содержит; без проверки RX это давало «connected» с мёртвым входящим аудио.

## Изменения

1. `vks-common::codec_capabilities` — единый источник правды:
   `NATIVE_AUDIO = PCMU/PCMA/G711`, `NATIVE_VIDEO = H264`,
   `matrix_mode(native|full)`, `full_matrix_allowed`.
2. `CodecProfile::from_config/from_env`: `full` разрешён только при
   `VKS_TRANSCODER_ENABLED=1` или `VKS_SDP_CODEC_MATRIX_LAB=1`; иначе Native +
   WARN. `H264-HIGH` определяется по `profile-level-id=6400…`, не по PT98.
3. `Gateways::ensure_bridgeable_codecs`: intent комнаты/адреса проверяется до
   дозвона; список только из G.722/H.265/H.264-high даёт явную ошибку
   `no common audio|video codec: … enable transcoding (T06b)`, а не молчаливый
   fallback. Пустой список = Native.
4. `SipDialError::NoCommonCodec(reason)` — UAC сохраняет типизированную причину
   `UnsupportedAudio { codec, payload }` вместо generic `InvalidSdpAnswer`.
5. RTP-наблюдаемость: первые 3 отвергнутых аудио-пакета логируются с
   expected/received payload type и source (codec/PT mismatch виден в журнале).
6. API: `settings/audio_codecs` → аддитивное `supported`; `settings/
   video_codecs` → `supported_video_codecs`; `ui/system/status` → `codecs
   {matrix, transcoder, native_audio, native_video}`. Полные списки и shape не
   урезаны — матрица T06b остаётся intent.
7. `VideoProfile::clamp_to_answer` при `max-mbps` сначала понижает размер по
   лестнице с fps ≥15 (1080p + 49000 → 960×540@24), а не оставляет 1080p@6;
   для вырожденных значений — 320×180 с минимальным fps.

## Live-доказательства

`artifacts/e2e-t06a-high4/` (1920×1080/4096):
- 114 checks / 0 failed / **page_errors 0**, 116.7 с;
- audio TX **585** / RX **384** пакетов, rejected **0**;
- video RTP tx **3 896 997 bps**, effective 1920×1080.

`artifacts/e2e-t06a-low/` (640×360/512):
- 114 checks / 0 failed / **page_errors 0**, 116.5 с;
- audio TX **571** / RX **380** пакетов, rejected **0**;
- video RTP tx **542 058 bps**, effective 640×360.

Терминал начинает слать аудио с задержкой ~5 с после 200 OK; проверка RX
перенесена с 4 с на 12 с. Это свойство терминала, а не скрытая потеря:
до 12 с rejected=0 и в журнале нет PT-mismatch.

## Ограничения / T06b

- Полная матрица (AAC, G.719, G.722.1/C, G.723/726/728/729, OPUS, H.261/263/
  265, H.264-high) по-прежнему требует транскодера (T06b) и включается только
  lab-флагом; production объявляет Native.
- Входящий SIP-wire UAS и ответ 488 на провод — T08; сейчас `on_invite`
  возвращает типизированную in-memory ошибку.
- H.323 H.241 (maxBitRate/level) — T05b/T07.
- `CodecMap::negotiate`/`H323CodecMap::negotiate` сохраняют legacy fallback для
  тестов; wire-пути используют строгий `try_negotiate`.
