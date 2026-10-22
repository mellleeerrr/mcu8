# T06b-4b — H.323 native audio matrix: G.728, G.723.1, PCMA

Дата: 2026-10-22. База: `73be959`. Продолжение T06b-4a: к H.323
(G.722/PT9 + G.711U/PT0) добавлены G.723.1 (PT4) и G.711A/PCMA (PT8) в
outbound sidecar и inbound listener. G.728 в H.323 **не включён**: наш bridge принимает только RFC 3551-кадр
40 байт/20 мс, а семантика H.245 `g728` (maxAl-sduAudioFrames) и фактическая
пакетизация H.323-терминалов не подтверждены живым пиром. Реклама capability
без проверенного кадра рискует дать «тихий» вызов; SIP-путь G.728 (40 B/20 мс)
остаётся рабочим.

## Реализация

- Новый общий модуль `crates/vks-gateway-h323/python/h323_audio.py`:
  единая таблица `AUDIO_CODECS`, порядок предпочтения
  `G722 > G711U > G711A > G723`, выбор только симметричных кодеков,
  H.245 data type/static PT; G.723.1 допускается только при
  `maxAl-sduAudioFrames == 1` (иначе пакеты 40/48 байт не влезают в bridge).
- H.245 TCS (sidecar и listener): записи строятся из `AUDIO_PREFERENCE` —
  1 G.722, 2 G.711U, 3 G.711A, 4 G.723.1, видео — 5;
  `simultaneousCapabilities = [[1,2,3,4],[5]]` (один
  аудиослот-альтернатива + видео). Без видео — `[[1,2,3,4]]`.
- Выбор: пересечение peer rx/tx, первый кандидат в порядке предпочтения.
  OLC использует static PT (9/0/8/4) и не выставляет
  `dynamicRTPPayloadType`; входящий OLC принимается только для выбранного
  PT; ACK даёт `audio_tx_pt` выбранного кодека; connected отдаёт
  симметричные remote/local PT.
- Отказ OLC: перебор оставшихся кандидатов на каналах 3,4,… пока не открыт
  входящий аудиоканал; при исчерпании — terminal failure. Учитывается только
  reject текущего `audio_channel`, PT берётся из `requested_audio_codec`
  канала; повторный/поздний reject старого канала игнорируется. Входящий
  audio OLC принимается один раз; повторный — reject. Повторный TCS после
  открытия каналов только ACK-ается, кодек не меняется.
- Rust:
  - `H323_AUDIO` = PCMU/G711U/PCMA/G711A/G711/G722/G723/G7231;
  - единая константа `vks_gateway_h323::H323_NATIVE_AUDIO_PTS = [0,4,8,9]`
    используется UAC, inbound и сервером; G.728/PT15 отклоняется.
  - UAC `ConnectedMedia` принимает только симметричные PT 0/4/8/9;
  - inbound UAS `h323_inbound_audio_pt`/`symmetric_h323_audio_pt` — те же
    четыре PT; PT15/G.728 отклоняется до bridge;
  - media_details: `audio_codec_name(pt)`, PT, clock 8000,
    sample rate 16000 только для G.722; bridge уже умеет PT4/8/15.
- G.723.1: 30 мс, 240 samples, timestamp +240, bridge prefill 480;
  PCMA: 160 B/20 мс/8000.

## Проверки

- `cargo fmt --all --check` — OK; `cargo clippy --workspace --all-targets
  -- -D warnings` — OK.
- `cargo test --workspace --lib` — 16/16 бинарей green, 0 failed
  (server 81+3, H.323 51, SIP 90+1, admin 281 и т.д.).
- H.323 Python — **48 tests OK**; новые: capability-записи и simultaneous,
  data type/PT для G722/G723/G711U/G711A, предпочтение G711/G711A,
  многошаговый fallback G722→G711U→G723 (sidecar и listener), peer G723
  maxAl=2 не выбирается/отклоняется, повторный входящий audio OLC
  отклоняется, peer-selected входящий PT переключает кодек и переоткрывает
  исходящий OLC, capabilityDescriptors ограничивают используемые записи,
  one-way TCS даёт явную ошибку «two-way audio is required», UAC accept PT4/8.
- deploy/README, crate README и PROVENANCE включают `h323_audio.py`;
  CI запускает `test_h323_sidecar test_h323_listener`.
- SIP-путь не затронут: allowlist payload types разделён по протоколу
  (`bridgeable_audio_pts`), G.728/PT15 и Opus/PT111 остаются SIP-only;
  отдельный тест это фиксирует.
- Live H.323 `.132` по-прежнему `adaptiveBusy` до H.245; SIP live и
  матрица layout E2E не затронуты (233/233 checks ранее). Живого
  H.323-доказательства новых кодеков нет.

## Ограничения

- H.323 G.728 отложен до проверки H.245 `g728` frame-count и реальной
  пакетизации терминала (SIP-путь G.728 40 B/20 мс остаётся рабочим).
- H.323 Opus/G.726 не добавлены: для них нет стандартного H.245
  static-PT описания в текущем sidecar; G.726 требует genericAudioCapability
  OID, Opus — вендорский OID.
- G.723.1 поддерживается 24 B MP-MLQ (encoder), ACELP 20 B — decoder;
  `annexa=no`.
- Bridge однокодековый: асимметричные PT отклоняются.
- Пересборка OLC после reject использует тот же sessionID/локальный порт;
  нужен живой interop-прогон, чтобы подтвердить поведение конкретного
  терминала.
- `adaptiveBusy` — внешний блок терминала; live H.323 остаётся открытым
  в T06b-4.
