# vks-proto

Проводной протокол VKS (protobuf + prost). Фаза 0: полный каркас.

## Файлы .proto
- `signaling.proto` — полный сигнальный протокол (envelope, handshake, join, SDP/ICE, subscribe, track/participant events, quality, layout, mode_changed). Источник: `docs/architecture/SIGNALING_PROTOCOL.md`.
- `media.proto` — RoomMode, CodecId, TrackInfo, RoomCaps, Subscribe, MediaStreamInfo. Источник: `docs/architecture/MEDIA_PIPELINE.md`.
- `cluster.proto` — NATS-сообщения (heartbeat, ownership, maintaining). Источник: `docs/architecture/CLUSTERING.md`.
- `admin.proto` — admin DTO (скелет; наполнение — фаза 5).

## Сборка
`build.rs` использует `protobuf-src` — компилирует собственный `protoc` в `OUT_DIR`,
системный `protoc` НЕ требуется (кросс-платформенно).

## Дерево зависимостей
`vks-proto` — standalone (не зависит от внутренних крейтов, ARCHITECTURE.md §3).
