# T06b-4a — H.323 native G.722 (H.245 g722-64k, RTP PT9)

Дата: 2026-10-22. База: `3d1e4a2`. Первый шаг T06b-4: wideband audio в
H.323-ветке (outbound sidecar + inbound listener), с G.711U fallback.

## Что изменено

- **H.245 TCS**: вместо одной G.711U-capability sidecar/listener объявляют
  две записи — 1 `g722-64k` (INTEGER 20), 2 `g711Ulaw64k`; видео стало
  записью 3; `simultaneousCapabilities = [[1,2],[3]]` (один аудиослот:
  G.722 или G.711U). Без видео — `[[1,2]]`.
- **Выбор кодека**: из peer TCS собираются `remote_audio_rx` и
  `remote_audio_tx` по направлениям capability (`receive*`, `transmit*`,
  `receiveAndTransmit*`); выбирается только симметричный кодек
  `select_audio_codec`: G.722, иначе G.711U, иначе fail-fast. Асимметрия
  не допускается.
- **OLC/ACK**: исходящий audio OLC использует `("audioData", ("g722-64k",
  20))` и не выставляет `dynamicRTPPayloadType`; static G.722 = PT9.
  Входящий G.722 OLC принимается только при выбранном G.722, ACK без
  dynamic PT. ACK нашего OLC даёт `audio_tx_pt=9`; `connected` отдаёт
  симметричные `remote_audio_payload_type=local_audio_payload_type=9`.
- **Fallback**: при reject G.722 OLC (и если peer не открыл входящий G.722)
  один раз повторяем audio OLC на канале 3 как G.711U **только если G.711U
  есть и в peer rx, и в peer tx**; reject G.711 — terminal failure без
  60-секундного зависания. Направления проверены отдельным тестом.
- **Dynamic audio PT**: входящий audio OLC с `dynamicRTPPayloadType`
  отклоняется `dataTypeNotSupported`; ACK на наш static OLC с dynamic PT —
  явная ошибка, а не молчаливое зависание. `extendedAudioCapability`
  безопасно пропускается.
- **Inbound Rust**: `remote_audio_payload_type` и `local_audio_payload_type`
  обязаны образовать `(0,0)` или `(9,9)`, иначе hangup; добавлен
  `symmetric_h323_audio_pt` + тест.
- **Rust**: `ConnectedMedia::validate` принимает только `(0,0)`/`(9,9)`;
  `H323_AUDIO` включает G722; inbound event принимает PT9; media_details
  для H.323 теперь дают `G722`, PT9, clock 8000, sample rate 16000; bridge
  получает PT9 и `audio_codec=Some("G722")`.
- G.722 media path не менялся: 320 PCM samples/20 мс, 160 октетов,
  RTP timestamp +160; bridge уже это умеет.

## Проверки

- `cargo test --workspace --lib` — **16/16 test binaries ok, 0 failed**
  (281 admin, 90 SIP +1 ignored, 81 server +3, 50 H.323, 97 core и т.д.).
- H.323 Python **38 tests OK** (новые: TCS-записи, выбор G722/G711,
  reject/fallback с проверкой rx/tx, входящий dynamic PT, OLC PT9, ACK PT9).
- fmt/clippy clean; `gates.log`.
- **Живой H.323 на `.132` сейчас не проходит**: терминал отвечает Q.931
  ReleaseComplete `adaptiveBusy` (decoded из H.225) до H.245, то есть на
  уровне «терминал занят», независимо от G.722. Живой SIP на тот же `.132`
  в это же время проходит (audio PT9 G722, video PT97/110, RTP идёт).
  Поэтому живое G.722-доказательство H.323 не получено; нужен свободный
  H.323-порт терминала или второй H.323-эндпоинт.
- Повторные попытки 16:12–16:22 MSK дают тот же adaptiveBusy; это не
  ошибка TCS/OLC, H.245 ещё не начинается. Сырой Q.931-дамп и декод —
  `h323-adaptivebusy.txt`. `live_h323` теперь печатает PT и при
  `VKS_LIVE_EXPECT_G722=1` требует `(9,9)`, чтобы будущий прогон доказал
  именно G.722, а не «какой-то звук».

## Ограничения

- H.323 принимает G.722 только 64k, не 56k/48k; при отсутствии общего
  симметричного кодека — fail-fast.
- Bridge однокодековый: асимметричные PT отклоняются.
- Fallback выполняется только если входящий аудиоканал ещё G.722;
  смешанный G.722-in/G.711-out не поддерживается.
- G.726/G.723/G.728/Opus в H.323 остаются открытыми (T06b-4b+); AAC/
  G.719/G.722.1/C/G.729 по-прежнему требуют транскодера.
- Живой H.323 G.722 и 20-call soak не подтверждены из-за adaptiveBusy.
