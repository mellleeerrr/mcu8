# Реальный WebRTC-транспорт: str0m ICE-lite + DTLS-SRTP

Дата: 2026-10-22. Коммиты: `99be70d` (примитив peer), `0a14572` (серверная
интеграция), далее — этот документ и E2E.

## Что было и что стало

До работы сервер отдавал браузеру **синтетический** SDP-ответ
(`SfuAnswerProfile`: статические `sfuufrag`/`sfupwd`, fingerprint
`sha-256 FF:00:11`, кандидат `203.0.113.7:49152`, без реального ICE/DTLS).
Браузер не мог установить транспорт, а сервер не имел SRTP-ключей.

Теперь для каждого браузера:

1. `WebRtcPeer::accept_offer` в `crates/vks-media-sfu/src/webrtc.rs` вызывает
   настоящий `str0m::Rtc::sdp_api().accept_offer`; ответ содержит реальный
   сертификат DTLS (общий на процесс, 32-байтный SHA-256 fingerprint),
   случайные per-peer `a=ice-ufrag`/`a=ice-pwd`, `a=ice-lite`,
   `a=setup:passive`, host-кандидат `advertise_ip:bound_port` и только
   Opus/VP8 (+VP8 RTX).
2. `WebRtcRoomRegistry` (`webrtc_room.rs`) держит реестр
   `(RoomId, ParticipantId)`, для каждого пира — отдельный UDP-сокет
   `0.0.0.0:0` и Tokio-задачу. Задача — единственный владелец `Rtc`:
   принимает датаграммы, отдаёт их `Rtc::accepts` + `handle_input`, шлёт
   `Output::Transmit`, вызывает `handle_timeout` по `next_timeout()`
   (ICE/DTLS-ретарсмиты, RTCP, BWE/pacer), а медиа-кадры передаёт другим
   пирам комнаты.
3. `vks-server` подменяет legacy `SdpAnswer.sdp` на реальный ответ до
   отправки клиенту. Оффер без ICE/DTLS-материала считается legacy-клиентом
   и получает прежний ответ; реальный оффер, отвергнутый str0m или
   неинициализированным транспортом, получает `Payload::Error` (fail-closed).
   `VKS_WEBRTC_TRANSPORT=0|false|off|no` — единственный путь к legacy-SDP.
   Для реального пира сервер **не** регистрирует plaintext-endpoint/SSRC/PT
   в `SfuEngine` и очищает ранее зарегистрированный endpoint/reverse-map.
4. Подписка обязательна: `SubscribeRequest` (в proto добавлено
   `bool audio_only = 5`) сохраняется даже до SDP-оффера и задаёт
   `(audio, video)`; audio-only пир не получает видео. `max_bitrate_bps`
   ограничивает BWE-потолок подписчика. `Unsubscribe` отключает оба потока. `Join`/`Connected` и переполнение очереди
   запрашивают keyframe у публишера; `Event::KeyframeRequest` от
   подписчика уходит публишеру через `Writer::request_keyframe`.
5. PT на выходе выбирается `Writer::match_params` по параметрам входящего
   кадра (у каждого пира свои PT), а не «первым negotiated PT».
   `answer.rtp_map` пересобирается из реально согласованных PT.
6. BWE реальный: `Event::EgressBitrateEstimate` применяется через
   `set_desired_bitrate`, пейсер str0m ограничивает отправку; NACK/RTX,
   jitter buffer и pacing — внутри str0m.
7. Ёмкость: не более **2** реальных WebRTC-пиров на комнату
   (`MAX_PEERS_PER_ROOM`). Третий оффер получает явную ошибку
   `room_full`, а не молчаливое смешивание двух RTP-часовых доменов в один
   mid. Re-offer атомарно заменяет поколение пира; задача старого
   поколения не может удалить новую запись.
8. Отказ закрытый: если транспорт включён и str0m отверг оффер, сервер
   возвращает `Payload::Error` (`webrtc_offer_rejected`), а не поддельный
   legacy-SDP с `FF:00:11`. В комнате с SIP/H.323-участником реальный
   браузерный WebRTC отклоняется (`webrtc_gateway_room_unsupported`), пока
   нет моста plaintext↔SRTP.
9. Наблюдаемость: `vks_webrtc_peers`, `vks_webrtc_offers_total`,
   `vks_webrtc_ice_failures_total`, `vks_webrtc_frames_dropped_total`,
   `vks_webrtc_fallback_total`; состояния ICE/Connected и ошибки таймеров
   логируются на `info`/`warn`. Пир удаляется при ошибке `Rtc`, по
   дедлайну подключения (15 с) и по idle-таймауту (30 с).

## Конфигурация

- `VKS_WEBRTC_TRANSPORT` — по умолчанию **выключен**. `1` включает реальный
  транспорт, `0|false|off|no` — выключает и оставляет прежний SDP. На стенде
  включается в `deploy/systemd/vks-mcu.service`. При включении стартовый
  preflight проверяет `candidate_addr`, оба порта диапазона и bind; ошибка —
  exit 78, а не тихий откат на фиктивный SDP.
- `VKS_WEBRTC_BWE_INITIAL_BPS` — стартовая оценка BWE (64 kbit/s..50 Mbit/s).
- `VKS_WEBRTC_PORT_MIN`/`VKS_WEBRTC_PORT_MAX` — UDP-диапазон для пиров;
  пусто = эфемерные порты. На стенде `43000..43100`.
- `VKS_ALLOW_LOOPBACK_MEDIA=1` разрешает loopback `candidate_addr` для
  локальных тестов; без него выводится предупреждение.
- Кандидат берётся из `HandlerConfig.candidate_addr` (`192.168.1.93` на
  стенде); IPv6-кандидат биндит `::`. Браузерный вход в signalling — `/ws`
  с `Authorization: Bearer` или `?token=<JWT>` (браузерный WebSocket не умеет
  заголовки); UI пока не подключён, единственный клиент — `tests/e2e/webrtc.js`.
- Для внешнего развёртывания нужен открытый UDP-диапазон и
  `candidate_addr`, доступный клиентам. ICE-lite не собирает relay-
  кандидаты, поэтому TURN должен быть на стороне клиента; это ограничение
  задокументировано в `prod.example.toml`.

## Доказательство (live, Chromium)

`tests/e2e/webrtc.js`: два Chromium-пира (fake camera/mic) с реальным
signalling по `/ws` (Node `ws` с Bearer; media — в Chromium), реальный
offer → реальный str0m answer → ICE/DTLS/SRTP → взаимные аудио/видео.
Итог: **29/29 проверок**, оба пира `connected`,
`dtlsState=connected`, `srtpCipher=SRTP_AEAD_AES_256_GCM`, входящие
audio/video `bytesReceived > 0`, `framesDecoded` 189/191,
`frameWidth=320`, `frameHeight=240`; третий пир получает `room_full`;
браузерный WebSocket с `?token=<JWT>` проходит ClientHello; page errors 0.
В `result.json` пишутся commit, sha256 бинаря, env,
`metrics.txt` и хвост server-лога. Прежний UI/API live-E2E
(`tests/e2e/live.js`) — **233/233**, но он не проверяет `/ws`/WebRTC и
служит только регрессией UI/API.

## Границы (честно)

- Это 1:1/комнатный SFU-фан-аут браузер↔браузер. N-way с несколькими
  источниками требует per-source transceiver/MSID-маршрутизации; сейчас
  жёстко ограничено 2 пирами на комнату, третий получает `room_full`.
- Мост SIP/H.323 (plaintext RTP) ↔ SRTP-браузер не реализован в этом
  коммите; для комнат со шлюзами нужен отдельный frame↔RTP мост.
- Симулкаст: если браузер пришлёт несколько rid, фан-аут пока не выбирает
  слой; это следующий шаг вместе с `SimulcastSelector`.
- Поддерживается ровно один audio- и один video-m-line (без screen share);
  лишние m-lines отвергаются валидацией оффера.
- Реальный WebRTC несовместим с MCU-режимом, recorder/mixing и комнатой с
  SIP/H.323-участником: при попытке такой комбинации обе стороны получают
  явный отказ, а не half-bridged «тишину».
- Порт-диапазон/файрвол и TURN не автоматизированы; на стенде работает
  host-кандидат `192.168.1.93:<ephemeral>`.
