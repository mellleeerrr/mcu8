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
3. `vks-server` отправляет клиенту **только** реальный `AcceptedPeer.answer_sdp`
   от str0m. Оффер без ICE/DTLS-материала отклоняется
   (`webrtc_offer_unsupported`), реальный оффер, отвергнутый str0m, —
   `webrtc_offer_rejected`; оба — `Payload::Error` с откатом FSM. Собственного
   SDP-конструктора в signalling больше нет, `VKS_WEBRTC_TRANSPORT` не имеет
   значения «выключено» (T11c). Пир не регистрируется в plaintext-реестре
   endpoint/SSRC/PT.
4. `SubscribeRequest` (в proto добавлено `bool audio_only = 5`) сохраняется
   даже до SDP-оффера и задаёт `(audio, video)`; без явной подписки новый пир
   получает `(true, true)` для совместимости, audio-only пир не получает видео. `max_bitrate_bps`
   ограничивает BWE-потолок подписчика. `Unsubscribe` отключает оба потока. `Join`/`Connected` и переполнение очереди
   запрашивают keyframe у публишера; `Event::KeyframeRequest` от
   подписчика уходит публишеру через `Writer::request_keyframe`.
5. PT на выходе выбирается `Writer::match_params` по параметрам входящего
   кадра (у каждого пира свои PT), а не «первым negotiated PT».
   `answer.rtp_map` пересобирается из реально согласованных PT.
6. BWE реальный: `Event::EgressBitrateEstimate` применяется через
   `set_desired_bitrate`, пейсер str0m ограничивает отправку; NACK/RTX,
   jitter buffer и pacing — внутри str0m.
7. Ёмкость и per-source egress (T11b): клиент делает pre-offer
   `1 audio + (N-1) audio` и `1 video + (N-1) video` m-lines (1 sendrecv,
   остальные recvonly); answer зеркалит **каждую** m-line (RFC 3264 §6), а не
   сжимается до 1+1. Направление каждого m-line учитывается (`sendonly` для
   recvonly-слота, `inactive` для port 0). Сервер держит маппинг
   `(publisher, kind) -> mid`: у каждого паблишера свой egress mid, поэтому
   N−1 аудио-потоков идут отдельными mid'ами (без микширования и без склейки
   двух RTP-часовых доменов), а `mid` никогда не переиспользуется между
   паблишерами — при уходе пира mid «retire»-ится.
   Ёмкость комнаты: `VKS_WEBRTC_MAX_PEERS_PER_ROOM` (по умолчанию **8**;
   предел — 8 egress mid'ов на вид, `MAX_AUDIO_MIDS`/`MAX_VIDEO_MIDS`), плюс
   глобальный `VKS_WEBRTC_MAX_PEERS` (256) на процесс и размер UDP-диапазона.
   Лимиты парсятся fail-closed: пустое/нечисловое/нулевое значение — ошибка
   старта (exit 78), а не тихий default. Сверх лимита — явная `room_full`
   (`retryable=true`), исчерпание портов — транспортная ошибка, не `room_full`.
   Реальный 1+1 клиент в комнате 3+ паблишеров получает ровно одного
   детерминированного паблишера (по generation), остальные
   источники явно деградируют с метрикой `webrtc_sources{dropped_no_mid}` —
   без склейки часов и без тишины.
   Keyframe-gate — per publisher: дельты паблишера не уходят подписчику до его
   IDR, PLI-запросы коалесцируются (500 мс на `(room, publisher, kind)`).
8. Отказ закрытый: если str0m отверг оффер, сервер возвращает
   `Payload::Error` (`webrtc_offer_rejected`), а не поддельный SDP с
   `FF:00:11`; оффер без ICE/DTLS-материала отклоняется
   (`webrtc_offer_unsupported`). Legacy-SDP не существует: T11c удалил и
   серверный конструктор, и `SfuAnswerProfile`, и plaintext UDP-луп. В комнате
   с SIP/H.323-участником реальный браузерный WebRTC отклоняется
   (`webrtc_gateway_room_unsupported`), а шлюз не приземляется в комнату с
   реальным пиром; моста plaintext↔SRTP нет.
9. Наблюдаемость: `vks_webrtc_peers`, `vks_webrtc_offers_total`,
   `vks_webrtc_ice_failures_total`, `vks_webrtc_frames_dropped_total`,
   `vks_webrtc_sources_total{result=allocated|retired|dropped_no_mid}`,
   `vks_webrtc_fallback_total`; состояния ICE/Connected и ошибки таймеров
   логируются на `info`/`warn`. Пир удаляется при ошибке `Rtc`, по
   дедлайну подключения (15 с) и по idle-таймауту (30 с).

## T11c (2026-10-29): легаси-плоскость удалена

- `SignalingHandler` больше не строит SDP-ответ: он валидирует оффер и двигает
  FSM, ответ приходит только от `WebRtcRoomRegistry`/str0m.
- Удалены `SfuAnswerProfile`, `IceCandidate`-билдер, `b=AS`,
  `VKS_SFU_VIDEO_BITRATE_KBPS`, `SfuEngine`, `UdpMediaLoop` и модули
  `bwe/congestion/codec/dtls/forwarder/ice/simulcast`.
- `MediaRegistry` (roster) заменил `SfuEngine`; медиа идёт только через
  per-peer str0m-задачи.
- `/readyz` `media_healthy` = результат реального preflight, а не `true`.
- `WEBRTC_FALLBACK{reason}` теперь только `offer_rejected`.

## Конфигурация

- `VKS_WEBRTC_TRANSPORT` — **обязателен**: только `1/true/on/yes` (без
  учёта регистра) запускает сервер; unset/пусто/`0/false/off/no`/мусор —
  exit 78. Это не opt-out: другого media plane нет. Стартовый preflight
  проверяет `candidate_addr` (включая assignability на хосте), оба порта
  диапазона и bind до открытия storage; его результат питает
  `/readyz.media_healthy`. Loopback допустим только при `allow_insecure_defaults=true`
  или `VKS_ALLOW_LOOPBACK_MEDIA=1` (с предупреждением в лог).
- `VKS_WEBRTC_BWE_INITIAL_BPS` — стартовая оценка BWE (64 kbit/s..50 Mbit/s).
- `VKS_WEBRTC_MAX_PEERS_PER_ROOM` — лимит реальных пиров **на комнату**
  (по умолчанию 8, T11b); значение парсится fail-closed (нечисловое/0 —
  exit 78). Выше 8 клиент с максимальным pre-offer'ом увидит не всех
  паблишеров (`webrtc_sources{dropped_no_mid}`), т.к. egress mid'ов 8.
- `VKS_WEBRTC_MAX_PEERS` — глобальный лимит реальных пиров процесса
  (по умолчанию 256, тоже fail-closed); исчерпание даёт `webrtc_capacity`
  с `retryable=true`.
- `VKS_WEBRTC_PORT_MIN`/`VKS_WEBRTC_PORT_MAX` — UDP-диапазон для пиров;
  пусто = эфемерные порты. На стенде `43000..43100`.
- `VKS_ALLOW_LOOPBACK_MEDIA=1` разрешает loopback `candidate_addr` для
  локальных тестов; без него стартовый preflight завершает процесс с exit 78.
- Кандидат берётся из `HandlerConfig.candidate_addr` (`192.168.1.93` на
  стенде); IPv6-кандидат биндит `::`. Браузерный вход в signalling — `/ws`
  с `Authorization: Bearer` или `?token=<JWT>` (браузерный WebSocket не умеет
  заголовки); query-token проверяется по `Origin`/`Host`, исключение —
  `VKS_WS_ORIGIN_ALLOW`; JWT в URL попадает в логи прокси, поэтому нужен TLS,
  короткий TTL и запрет логирования query. UI пока не подключён, единственный
  клиент — `tests/e2e/webrtc.js`.
- `/readyz` проверяет БД/media/шлюзы; WebRTC-транспорт валидируется
  стартовым preflight (exit 78), а не отдельным полем readiness.
- Для внешнего развёртывания нужен открытый UDP-диапазон и
  `candidate_addr`, доступный клиентам. ICE-lite не собирает relay-
  кандидаты, поэтому TURN должен быть на стороне клиента; это ограничение
  задокументировано в `prod.example.toml`.

## Доказательство (live, Chromium)

`tests/e2e/webrtc.js`: **три** Chromium-пира (fake camera/mic) с реальным
signalling по `/ws` (Node `ws` с Bearer; media — в Chromium). Каждая страница
делает T11b pre-offer `1+7 audio + 1+7 video` (16 m-lines), поэтому answer
зеркалит все 16 m-line, а каждый пир получает два чужих аудио-потока и два
чужих видео на отдельных mid'ах. Итог: все три пира `connected`,
`dtlsState=connected`, `srtpCipher=SRTP_AEAD_AES_256_GCM`, входящие
audio/video `bytesReceived > 0`, page errors 0; браузерный WebSocket с
`?token=<JWT>` проходит ClientHello. `room_full` проверяется только когда
оракул запущен с `VKS_E2E_PEER_CAP`, совпадающим с
`VKS_WEBRTC_MAX_PEERS_PER_ROOM` (`cap+1`-пир получает `room_full`,
`retryable=true`, повторный оффер тоже `room_full` после FSM-rollback).
В `result.json` пишутся commit, sha256 бинаря, env,
`metrics.txt` и хвост server-лога. Прежний UI/API live-E2E
(`tests/e2e/live.js`) — **233/233**, но он не проверяет `/ws`/WebRTC и
служит только регрессией UI/API.

## Границы (честно)

- Это комнатный SFU-фан-аут браузер↔браузер с per-source egress: каждый
  подписчик получает N−1 отдельных аудио-mid'ов и N−1 видео-mid'ов (без
  микширования и без склейки RTP-часовых доменов). Ёмкость —
  `VKS_WEBRTC_MAX_PEERS_PER_ROOM` (по умолчанию 8) и 8 egress-mid'ов на вид
  (`MAX_AUDIO_MIDS`/`MAX_VIDEO_MIDS`); сверх лимита — явный `room_full`, а не
  тихая деградация. Клиент обязан сам pre-offer'ить `1+(N-1)` m-lines: сервер
  не может добавить их в answer (RFC 3264 §6).
- Мост SIP/H.323 (plaintext RTP) ↔ SRTP-браузер не реализован в этом
  коммите; для комнат со шлюзами нужен отдельный frame↔RTP мост.
- Симулкаст: если браузер пришлёт несколько rid, фан-аут пока не выбирает
  слой; это следующий шаг вместе с `SimulcastSelector`.
- Поддерживается до 8 audio- и 8 video-m-line в одном оффере (1 sendrecv +
  recvonly-слоты); 9-я m-line любого вида отвергается валидацией оффера
  (`InvalidOffer`). Дубли `a=mid:` **между** audio и video отвергаются явной
  `Sdp`-ошибкой (один mid не может быть двумя пулами); дубли внутри одного
  вида по-прежнему схлопывает str0m (унаследованное поведение 1+1, не T11b).
  Секция, отклонённая браузером через `m=… 0`, получает в answer порт 0 и
  `a=inactive` (RFC 3264 §6.1) и никогда не становится egress-слотом.
- Реальный WebRTC несовместим с MCU-режимом, recorder/mixing и комнатой с
  SIP/H.323-участником: при попытке такой комбинации обе стороны получают
  явный отказ, а не half-bridged «тишину». Live-переключение комнаты/глобального
  режима на MCU (включая `POST /api/v1/rooms/{id}/mode` и `/api/v1/settings`)
  выселяет реальных пиров и рассылает `RoomModeChanged` затронутым комнатам
  (при глобальной смене — всем без per-room override); комнаты с явным
  per-room SFU-override не затрагиваются. Проверка и регистрация пира/шлюза
  сериализованы общим admission-lock, поэтому оффер и SIP/H.323-прилэнд не
  могут проскочить между проверкой и регистрацией.
- Порт-диапазон/файрвол и TURN не автоматизированы; на стенде работает
  host-кандидат `192.168.1.93:43000/43001` из диапазона 43000–43100.
