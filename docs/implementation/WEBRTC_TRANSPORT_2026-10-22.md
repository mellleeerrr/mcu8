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
   отправки клиенту. Если оффер не содержит ICE/DTLS-материала или str0m
   вернул ошибку, остаётся legacy-ответ (осознанный fallback; включается
   `VKS_WEBRTC_TRANSPORT=0`). Для реального пира сервер **не** регистрирует
   plaintext-endpoint/SSRC/PT в `SfuEngine`, чтобы SRTP-трафик не смешивался
   с plaintext-путём.
4. `SubscribeRequest`/`Unsubscribe` управляют подпиской пира: audio-only
   подписчик не получает видео; `Join` нового пира запрашивает keyframe у
   уже подключённых; `Event::KeyframeRequest` от подписчика уходит
   публишеру через `Writer::request_keyframe`.
5. PT на выходе выбирается `Writer::match_params` по параметрам входящего
   кадра (у каждого пира свои PT), а не «первым negotiated PT». NACK/RTX,
   jitter buffer, pacing и BWE — внутри str0m; второй BWE на этом пути не
   запускается.

## Конфигурация

- `VKS_WEBRTC_TRANSPORT` — `0` выключает реальный транспорт и оставляет
  прежний SDP. Не задан — включён (текущий live-стенд).
- `VKS_WEBRTC_BWE_INITIAL_BPS` — стартовая оценка BWE (64 kbit/s..50 Mbit/s).
- Кандидат берётся из `HandlerConfig.candidate_addr` (`192.168.1.93` на
  стенде); порт — фактически занятый эфемерный UDP-порт.
- Для внешнего развёртывания нужен открытый диапазон UDP-портов и
  `candidate_addr`, доступный клиентам (NAT/TURN — задача эксплуатации).

## Доказательство (live, Chromium)

`tests/e2e/webrtc.js`: два независимых браузерных пира (fake camera/mic),
каждый со своим `/ws` (JWT), реальный offer → реальный str0m answer →
ICE/DTLS/SRTP → взаимные аудио/видео. Итог: **22/22 проверок**, оба пира
`connected`, `dtlsState=connected`, `srtpCipher=SRTP_AEAD_AES_256_GCM`,
входящие audio/video `bytesReceived > 0`, `framesDecoded` 89/91,
`frameWidth=320`, `frameHeight=240`; page errors 0.

Артефакты: `docs/progress/evidence/webrtc/result.json` и SDP offer/answer
обоих пиров. Полный прежний live-E2E (`tests/e2e/live.js`) при включённом
транспорте: **233/233**, `page_errors=[]` (те же 6 вендорских TypeError и
2 ожидаемых 404).

## Границы (честно)

- Это 1:1/комнатный SFU-фан-аут браузер↔браузер. N-way с несколькими
  источниками требует, чтобы клиент заранее предложил recv-m-line на
  каждый источник (answer не добавляет m-lines); сейчас один источник
  audio+video на пира.
- Мост SIP/H.323 (plaintext RTP) ↔ SRTP-браузер не реализован в этом
  коммите; для комнат со шлюзами нужен отдельный frame↔RTP мост.
- Симулкаст: если браузер пришлёт несколько rid, фан-аут пока не выбирает
  слой; это следующий шаг вместе с `SimulcastSelector`.
- Порт-диапазон/файрвол и TURN не автоматизированы; на стенде работает
  host-кандидат `192.168.1.93:<ephemeral>`.
