# MEDIA PIPELINE — спецификация медиа-пайплайна

> Статус: **v0.1 (черновик)**. Заполняется в фазах 2–3.
> Инвариант: **MediaEngine** — единый трейт, две реализации (SFU, MCU). ADR-0003 (str0m), ADR-0004 (MCU compositor).

---

## 1. Трейт MediaEngine (vks-core/ports/media_engine.rs)

```rust
pub trait MediaEngine: Send + Sync {
    fn start(&self, room: &Room, config: &RoomMediaConfig) -> Result<EngineHandle>;
    fn stop(&self, handle: &EngineHandle) -> Result<()>;
    fn switch_mode(&self, handle: &EngineHandle, new_mode: RoomMode) -> Result<SwitchHandle>;
    // Publisher API
    fn add_publisher(&self, handle: &EngineHandle, participant: &Participant) -> Result<PublisherHandle>;
    fn remove_publisher(&self, handle: &EngineHandle, participant_id: &ParticipantId) -> Result<()>;
    // Subscriber API
    fn add_subscriber(&self, handle: &EngineHandle, participant: &Participant) -> Result<SubscriberHandle>;
    fn remove_subscriber(&self, handle: &EngineHandle, participant_id: &ParticipantId) -> Result<()>;
    // Layout
    fn apply_layout(&self, handle: &EngineHandle, layout: &Layout) -> Result<()>;
}
```

## 2. SFU pipeline (vks-media-sfu)

### 2.1. Архитектура
```
Client A (publish) ──RTP──→ SFU Forwarder ──RTP──→ Client B (subscribe)
Client C (publish) ──RTP──→ SFU Forwarder ──RTP──→ Client D (subscribe)
```

### 2.2. Forwarder
- `HashMap<TrackId, Vec<SubscriberHandle>>`.
- 1-to-1: один publisher → один subscriber.
- N-to-N: множественные publishers → subscribers.
- Subscription model: full-mesh по умолчанию.

### 2.3. Simulcast
- Клиент шлёт 3 spatial layers (low/med/high) — разные SSRC.
- `SimulcastSelector.select(bwe)`: High ≥ 800kbps, Med ≥ 200kbps, Low.
- При смене layer — PLI/FIR на keyframe нового layer.

### 2.4. BWE
- **TWCC**: subscriber → SFU (transport-wide RTCP feedback).
- **REMB**: subscriber → SFU (estimated bitrate RTCP).
- `effective_bps = min(remb, twcc_estimate)`.
- SFU НЕ шлёт BWE обратно (subscriber знает свою downlink).

### 2.5. Congestion ladder
```
Normal → DegradeSpatial (High→Med→Low) → DegradeTemporal (drop temporal) → AudioOnly
Recovery: hysteresis +5% (anti-flap)
```

### 2.6. NACK / FEC
- NACK: subscriber → SFU, SFU retransmits из jitter buffer (~200ms, 60 pkts @ 30fps).
- FEC: SFU НЕ генерирует (задача publisher). SFU форвардит RED/UlpFec.

### 2.7. DTLS / SRTP
- str0m: per-session self-signed X.509, fingerprint в SDP.
- SRTP: AES-128-CM, SRTCP: HMAC-SHA1 80-bit.
- PFS: ECDHE ephemeral.
- WebRTC: client = DTLS client. Gateway: gateway = DTLS server.

### 2.8. ICE
- SFU/MCU = **ICE-lite** (RFC 8445 §2): host candidates only.
- Client = full ICE (host + srflx + relay).
- TURN: client → TURN (ALLOCATE, CREATE_PERMISSION, BINDING to SFU).

## 3. MCU pipeline (vks-media-mcu)

### 3.1. Архитектура
```
Client A (publish) ──RTP──→ [decode] ──YUV──┐
Client B (publish) ──RTP──→ [decode] ──YUV──┤→ [Compositor 1920×1080] → [encode H.264] → RTP → ALL subscribers
Client C (publish) ──RTP──→ [decode] ──YUV──┘
```

### 3.2. Decoder
- str0m: SRTP decrypt → depacketize → FEC → jitter → decoder (VP8/VP9/H.264/AV1) → YUV.
- N decoded frames → mpsc → Compositor.

### 3.3. Compositor
- Canvas: **1920×1080** (фикс, v1).
- Layout: `vks-layout::Layout` → `vks-media-mcu::layout_bridge::PixelRect` (пиксели канваса).
  Функции `to_pixel_rects` в `vks-layout` **нет**: преобразование выполняет
  `LayoutBridge` (`crates/vks-media-mcu/src/layout_bridge.rs`).
- Scale: libyuv `ScalePlane` (bilinear) — за `oxideav`-фичей; в сборке по
  умолчанию используется детерминированный nearest-neighbour с letterbox.
- Aspect mismatch: **letterbox** (default) / crop (slot fit="cover").
- Frame rate: **30 fps** (drop/repeat, без interpolation).
- Layout change: **crossfade 300ms** (old rects α-blend → new rects).
- Participant join/leave: auto-assign / remove → grid re-flow → crossfade 300ms.

### 3.4. Encoder
- **H.264** (max compatibility: все браузеры + SIP + H.323). VP9/AV1 — future.
- Resolution: 1080p. FPS: 30. Bitrate: **2.5 Mbps CBR**.
- Per-subscriber VBR — future.

### 3.5. Audio mixing
- N Opus (48kHz mono) → decode PCM → sum с per-participant gain (default 1.0, **AGC = 1/√N**) → soft-clip (tanh) → 1 Opus.
- EC/NS на MCU НЕ нужны (сервер не слышит свой output; NS на клиенте).

### 3.6. Speaker detection (VAD)
- RMS + zero-crossing, 20ms frames, **500ms debounce**.
- Active speaker → `LayoutKind::Speaker` preset (speaker → крупный, остальные thumbnails).
- `SpeakingStateChanged` шлётся и SFU-клиентам.

## 4. Переключение MCU ↔ SFU на лету

### 4.1. In-place switch (ADR)
```rust
enum MediaEngineState {
    Sfu(Forwarder),
    Mcu(Compositor),
    Switching { from: Box<dyn MediaEngine>, to: Box<dyn MediaEngine>, ready: Arc<AtomicBool> }
}
```

**SFU → MCU**:
1. SFU forwarder продолжает (publisher'ы не меняют).
2. MCU compositor поднимается **параллельно**.
3. Когда готов (first frame) → subscriber'ы переключаются на композит.
4. SFU forwarder для этих subscriber'ов отключается.

**MCU → SFU** — наоборот.

### 4.2. Publishers
- RTP **продолжает** (publisher не знает о mode).
- DTLS/SRTP session не меняется.
- Simulcast: MCU декодирует все SSRC, берёт highest; SFU — BWE выбирает.

### 4.3. Subscribers
- SDK **transparent**.
- Server: `unsubscribe(old)` + `subscribe(new)`.
- SFU→MCU: N tracks → 1 композит; MCU→SFU: 1 → N tracks.
- **SDP renegotiation**: да (m-lines меняются). ICE — DTLS resumption.

### 4.4. SDK steps
1. `RoomModeChanged` received.
2. Unsubscribe all.
3. Create new SdpOffer (new m-lines) → send.
4. Wait SdpAnswer.
5. ICE (resumption).
6. Switch renderer (`sfu_renderer` ↔ `mcu_renderer`).
7. Layout из сообщения (сохраняется).

### 4.5. Layout при переключении
- **Сохраняется** (operator set layout, not mode).
- SFU: client-side CSS. MCU: server-side compositor.
- Тот же layout, разный apply.

### 4.6. Audio
- MCU: 1 mixed stream. SFU: N unmixed (client Web Audio mix).
- Transition: **10–20ms silence gap** (acceptable).

### 4.7. Timing
| Фаза | Время |
|---|---|
| `RoomModeChanged` → SDK | 0–0.1s |
| Unsubscribe + SdpOffer | 0.1–0.3s |
| SdpAnswer + ICE | 0.3–0.8s |
| First frame | 0.8–1.5s |
| Renderer cut | 1.5s |
| **Worst** | **3s** (ICE re-gather) |

### 4.8. Failure (rollback)
- Timeout 3s → **rollback**: subscriber'ы остаются на old mode.
- `tracing::error`, admin UI «switch failed, stayed in SFU».
- Operator может retry.

## 5. Codecs

| Codec | SFU | MCU (decode) | MCU (encode) | Gateway (SIP) | Gateway (H.323) |
|---|---|---|---|---|---|
| VP8 | ✅ | ✅ | — | — | — |
| VP9 | ✅ | ✅ | — | — | — |
| H.264 | ✅ | ✅ | ✅ (composite) | ✅ | ✅ |
| H.265 | ✅ (feature) | ✅ | — | — | — |
| AV1 | ✅ (feature) | ✅ | — | — | — |
| Opus | ✅ | ✅ (mix) | ✅ | ✅ (transcode) | ✅ (transcode) |
| G.722 | ✅ | ✅ | — | ✅ | — |
| G.711 | — | — | — | ✅ | ✅ (transcode) |

Feature flags: `vp8`, `vp9`, `h264`, `h265`, `av1`, `opus`, `g722`, `g711` (ADR-0017).

## 6. Performance targets (v1)

| Метрика | SFU | MCU |
|---|---|---|
| Комнат на инстанс | 50 | 20 |
| Участников на комнату | 10 | 8 |
| Resolution | 720p (simulcast) | 1080p (composite) |
| Bitrate per subscriber | 500kbps–2Mbps | 2.5Mbps |
| CPU per room | 0.5 core | 4 cores |
| Upstream/Downstream | 2 Gbps | 2.5 Mbps × 8 = 20 Mbps |
| Signaling | 10k concurrent WS | — |
