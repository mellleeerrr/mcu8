# vks-media-sfu

Реальная SFU-медиаплоскость (str0m) и roster-движок сервера.

Плоскость — **только DTLS/SRTP** на каждого браузера. Серверного
plaintext-форвардера, UDP-медиалупа, собственных BWE/simulcast/DTLS/ICE
модулей больше нет: ICE (host candidates, ICE-lite), DTLS, SRTP/SRTCP,
NACK/RTX, jitter buffer, pacing и TWCC/REMB живут внутри `str0m`.

## Модули

| Модуль | Что делает |
|--------|------------|
| [`registry`](src/registry.rs) | `MediaRegistry` — impl `MediaEngine`: roster комнат, паблишеров, треков и подписчиков для сигналинга (`TrackAdded`, раскладка, подписки). Медиа через него не идёт. |
| [`webrtc`](src/webrtc.rs) | `WebRtcPeer` — sans-I/O обёртка над `str0m`: приём датаграмм, реальный SDP-answer, direction-aware mids, per-source egress mids (T11b). |
| [`webrtc_room`](src/webrtc_room.rs) | `WebRtcRoomRegistry` — реестр реальных пиров, UDP-сокет и Tokio-задача на каждого пира, per-publisher keyframe gate, лимиты комнаты. |

## MediaEngine-порт

`MediaRegistry` реализует `vks_core::ports::media_engine::MediaEngine`:

- `mode() -> RoomMode::Sfu`
- `create_room` / `destroy_room` / `switch_room_mode`
- `add_publisher` / `remove_publisher` (announce/withdraw tracks)
- `add_subscriber` / `remove_subscriber`
- `apply_layout` (SFU: no server-side effect — client-side)
- `set_room_capabilities` / `list_publishers` / `list_subscribers`

Клиентский SDK работает с трейтом `MediaEngine` — MCU/SFU-прозрачность
(ARCHITECTURE.md §2).

## Модель доставки

`MediaRegistry` — это **состав** комнаты (кто публикует какие треки и кто на
что подписан), а не путь RTP. Доставку выполняет `WebRtcRoomRegistry`:
каждый паблишер получает собственный egress mid на вид, кадры уходят
подписчикам через их str0m-сессии, keyframe-gate — per publisher.

## Ёмкость

- `VKS_WEBRTC_MAX_PEERS_PER_ROOM` (по умолчанию 8, не выше 8 egress mid'ов
  на вид — `MAX_AUDIO_MIDS`/`MAX_VIDEO_MIDS`);
- `VKS_WEBRTC_MAX_PEERS` (256 на процесс);
- UDP-диапазон `VKS_WEBRTC_PORT_MIN..=MAX` (пусто = эфемерные порты).

Оба лимита парсятся fail-closed: нечисловое/нулевое значение — exit 78.

## Тесты

`cargo test -p vks-media-sfu` (feature `str0m` включён по умолчанию).
