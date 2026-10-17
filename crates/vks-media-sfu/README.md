# vks-media-sfu

SFU-движок (фаза 2, DEVELOPMENT_PLAN.md). Реализует `vks_core::MediaEngine`
в SFU-режиме: forwarding, simulcast, BWE, congestion, DTLS-SRTP state,
ICE-lite, codec feature flags.

## Архитектура

```
Client A (publish) ──RTP──→ SfuEngine ──RTP──→ Client B (subscribe)
Client C (publish) ──RTP──→ SfuEngine ──RTP──→ Client D (subscribe)
```

SFU — forwarding-only: сервер не декодирует, не смешивает, не перекодирует.
Он маршрутизирует RTP-пакеты от publishers к subscribers.

## Модули

| Модуль | Что делает |
|--------|------------|
| [`engine`](src/engine.rs) | `SfuEngine` — impl `MediaEngine` (SFU). In-memory, loopback-ран. |
| [`forwarder`](src/forwarder.rs) | N-to-N fan-out. **Full-mesh по умолчанию** (новый подписчик получает все активные track'и). |
| [`simulcast`](src/simulcast.rs) | 3 spatial layer (Low/Med/High), выбор по bps. Hysteresis +5%. |
| [`bwe`](src/bwe.rs) | TWCC bandwidth estimator (sliding window, deterministic). |
| [`congestion`](src/congestion.rs) | Congestion ladder (degrade: spatial → temporal → audio-only; recovery с hysteresis). |
| [`dtls`](src/dtls.rs) | DTLS-SRTP session state (Handshaking → Active → Closed) + SRTP keying placeholder. |
| [`ice`](src/ice.rs) | ICE-lite (RFC 8445 §2): host candidates only. |
| [`codec`](src/codec.rs) | Codec feature flags (ADR-0017): `opus`, `vp8`, `vp9`, `av1`. |

## MediaEngine-порт

`SfuEngine` реализует `vks_core::ports::media_engine::MediaEngine`:

- `mode() -> RoomMode::Sfu`
- `create_room` / `destroy_room` / `switch_room_mode`
- `add_publisher` / `remove_publisher` (announce/withdraw tracks)
- `add_subscriber` / `remove_subscriber` (full-mesh auto-subscribe)
- `apply_layout` (SFU: no server-side effect — client-side)
- `set_room_capabilities` / `list_publishers` / `list_subscribers`

Клиентский SDK работает с трейтом `MediaEngine` — MCU/SFU-прозрачность
(ARCHITECTURE.md §2).

## Full-mesh model

По умолчанию (full-mesh):

1. `add_subscriber(sub)` — `sub` автоматически подписывается на **все**
   активные track'и комнаты.
2. `add_publisher(pub, tracks)` — track'и annunciated; новые подписчики
   получат их автоматически.
3. `route(track, packet)` — пакет → **все** подписчики track'а.

1-to-1 — частный случай (1 publisher, 1 subscriber).

## Congestion ladder

```
Normal → DegradeSpatial(Med) → DegradeSpatial(Low) → DegradeTemporal → AudioOnly
```

- **De-гратация**: при `bps < threshold[level]` — шаг вниз (многошагово).
- **Восстановление**: при `bps ≥ threshold[level] × 1.05` — один шаг вверх
  (hysteresis, anti-flap).
- Пороги (default): 500k / 250k / 120k / 60k bps.

Ladder взаимодействует с `SimulcastSelector`: при `DegradeSpatial(target)`
и `target < current_layer` — принудительный spatial down. При `AudioOnly`
— `Low` layer.

## BWE (TWCC)

`BweEstimator` — детерминированный sliding-window throughput-estimator.
Вводит TWCC feedback (arrival times), выводит bps. Без RNG/внутренних
таймеров — синтетический feedback воспроизводит тот же bps.

В фазе3 сюда добавится REMB (`effective = min(remb, twcc)`,
MEDIA_PIPELINE §2.4) и delay-based correction.

## Simulcast

3 spatial layer (разные SSRC на клиенте):

| Layer | Threshold |
|-------|-----------|
| Low   | 0 bps (always) |
| Med   | ≥ 200 kbps |
| High  | ≥ 800 kbps |

- **Downgrade**: при `bps < threshold[current]` — шаг вниз (без hysteresis).
- **Upgrade**: при `bps ≥ threshold[next] × 1.05` — один шаг вверх (hysteresis).
- При смене layer — PLI/FIR на keyframe нового layer (задача клиентского SDK,
  фаза3).

## ICE-lite

SFU/MCU = **ICE-lite** (RFC 8445 §2): host candidates only (без srflx/relay).
Client — full ICE. TURN — задача client.

`IceLite::loopback()` — provider с 127.0.0.1 + ::1. `StaticAddrs` —
mock для тестов. `HostCandidate::to_sdp_fragment()` — строка для SDP (фаза3).

## DTLS-SRTP

`DtlsSession` — thin state model: `Handshaking → Active → Closed`.
SRTP keying material — placeholder (реальная генерация через str0m — фаза3).

Feature `str0m` (optional) добавляет `pub use str0m;` — для фаз3
(реальный DTLS handshake + SRTP). Без feature крейт компилируется.

## Codec feature flags (ADR-0017)

Features: `opus`, `vp8`, `vp9`, `av1` (default: `opus`, `vp8`).
Каждый enabled feature → один entry в `CodecRegistry`. SFU не декодирует —
registry для SDP offer/answer (фаза3) и `RoomCaps`.

## Build

```bash
cargo build -p vks-media-sfu                 # default (opus, vp8)
cargo build -p vks-media-sfu --features str0m  # + str0m (DTLS-SRTP)
cargo build -p vks-media-sfu --features av1   # + av1
cargo test -p vks-media-sfu
cargo clippy -p vks-media-sfu --all-targets -- -D warnings
```

## Тесты

54 unit tests: bwe (feedback → bps), simulcast (thresholds + hysteresis),
congestion (ladder degrade/recover), forwarder (N-to-N routing, add/remove),
engine (loopback, feedback → layer), codec (feature registry), ice (host
candidates), dtls (state machine).
