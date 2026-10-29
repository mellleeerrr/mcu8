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

### 2.1. Архитектура (T11c)

Медиаплоскость — **только WebRTC/DTLS-SRTP**. Отдельного plaintext RTP-пути,
UDP-медиалупа и серверного forwarder'а нет.

```
Browser A ──SRTP──→ str0m peer (per-participant task) ──SRTP──→ Browser B
Browser C ──SRTP──→ str0m peer                         ──SRTP──→ Browser D
```

- `WebRtcRoomRegistry` (`webrtc_room.rs`) держит `(RoomId, ParticipantId)` →
  `WebRtcPeer` + UDP-сокет + Tokio-задачу.
- `WebRtcPeer` (`webrtc.rs`) — sans-I/O обёртка `str0m::Rtc`: ICE-lite host
  candidate, DTLS, SRTP/SRTCP, jitter buffer, NACK/RTX, pacing, TWCC/REMB.
- `MediaRegistry` (`registry.rs`) — только roster (комнаты, паблишеры, треки,
  подписки) для сигналинга; RTP через него не проходит.

### 2.2. Доставка
- Каждый паблишер получает собственный egress mid на вид (T11b); два
  паблишера никогда не склеиваются в один mid/часовой домен.
- Подписка (`SubscribeRequest`) задаёт `(audio, video)` и `max_bitrate_bps`;
  без явной подписки — `(true, true)`.
- Keyframe-gate — per publisher: дельты не уходят подписчику до IDR;
  PLI-запросы коалесцируются (500 мс на `(room, publisher, kind)`).

### 2.3. Re-negotiation
- Повторный оффер на живом пире идёт в ту же str0m-сессию (`accept_reoffer`)
  без нового ICE/DTLS и без нового сокета; m-lines можно только добавлять.
- Смена DTLS fingerprint означает новый PeerConnection — сервер отвечает
  ошибкой, а не пересоздаёт транспорт молча.

### 2.4. Ёмкость и отказ
- `VKS_WEBRTC_MAX_PEERS_PER_ROOM` (8) и `VKS_WEBRTC_MAX_PEERS` (256) —
  fail-closed env; сверх лимита `room_full`/`webrtc_capacity`.
- Оффер без ICE/DTLS → `webrtc_offer_unsupported`; H264-only (без Opus/VP8)
  → `InvalidOffer` без паники; str0m-ошибка → `webrtc_offer_rejected`.
- Никакого fake SDP: answer всегда сгенерирован str0m.

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

<!-- codec-matrix:begin -->
| Codec | Kind | SIP | H.323 | WebRTC | Reason |
|---|---|---|---|---|---|
| PCMU | audio | native | native | — | — |
| PCMA | audio | native | native | — | — |
| G722 | audio | native | native | — | — |
| G723 | audio | native | native | — | — |
| G726-16 | audio | native | — | — | — |
| G726-24 | audio | native | — | — | — |
| G726-32 | audio | native | — | — | — |
| G726-40 | audio | native | — | — | — |
| G728 | audio | native | — | — | — |
| OPUS | audio | native | — | webrtc | — |
| MP4A-LATM | audio | worker | — | — | — |
| G729 | audio | — | — | — | encoder_not_bit_exact |
| G7221 | audio | — | — | — | no_implementation |
| G719 | audio | — | — | — | no_implementation |
| ILBC | audio | — | — | — | no_implementation |
| H264 | video | native | native | — | — |
| H264-HIGH | video | rx-only | — | — | tx_profile_unsupported |
| H265 | video | native | — | — | — |
| VP8 | video | — | — | webrtc | webrtc_only |
| VP9 | video | — | — | — | pending_payloader |
| AV1 | video | — | — | — | pending_payloader |
| H261 | video | — | — | — | no_implementation |
| H263 | video | — | — | — | no_implementation |
| H263+ | video | — | — | — | no_implementation |
| H264-SVC | video | — | — | — | svc_not_forwarded |
| telephone-event | dtmf | native | — | — | — |
<!-- codec-matrix:end -->

The matrix is **generated from** `crates/vks-common/src/codec_spec.rs` and checked
by `docs_public_matrix_matches_ssot` in `cargo test --workspace`; edit the SSOT,
not this table. `native` = bit-exact bridge, `worker` = isolated FFmpeg bridge
(license-gated for AAC-LATM), `webrtc` = present in the str0m answer,
`rx-only` = accepted but never offered, `—` = not available on that plane.
`H.264 High` is receive-only: the gateway always transmits baseline.
`G711` is an alias family (PCMU + PCMA), not a separate codec row.

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

## 7. T12: mixed room browser ↔ SIP/H.323 (audio bridge, honest scope)

`bridge.rs` is the single place that decides mixed rooms through the asymmetric
`bridge_available(caps, side, has_browser, has_gateway)` predicate:

- `BridgeSide::BrowserJoining`: a browser may enter a room with a SIP/H.323
  participant only when that gateway already exists;
- `BridgeSide::GatewayLanding`: a gateway may land in a room only when a real
  browser WebRTC peer already exists;
- browser-only and gateway-only rooms stay SFU/MCU rooms; the predicate is
  false there and admission keeps its old behaviour.

The kill switch is `VKS_HYBRID_BRIDGE` (`0/false/off/no` disables, default on).
When it is off, `gateway_room_conflict()` keeps the pre-T12 messages
byte-identical; when it is on but the caps matrix is missing, the error names
the reason (`hybrid_disabled`, `opus_unavailable`, `no_gateway`, `no_browser`,
`allowed`).

Delivered path is **audio-only**:
browser Opus frame → tap → Opus decode → 20 ms PCM → one MCU tick → gateway-only
`mix_participants_audio` → Opus encode → server-side bridge egress → every
browser on SRTP; no audio is delivered twice because browsers still receive each
other through SFU fanout and the bridge egress contains only the gateway mix.

`McuEngine` enforces **one ticker per room** via `bridge_owns_tick`:
while the bridge owns `tick_audio_at`, the legacy gateway RTP path's own
`tick_audio_at` call is a no-op; `audio_ticks_total` proves the cadence in
unit tests. The canonical lock order is
`pending_layouts → rooms → frames → audio`; `apply_layout`,
`remove_publisher`, `unbridge_participant` and `mix_participants_audio` obey it.

Mixed-room **video is deliberately not delivered**. The codec SSOT marks
`H264` as `webrtc: Unsupported(NoImplementation)`; `webrtc_answer_specs` is
Opus+VP8 only; the MCU compositor emits H.264/H.265 and the MCU has no VP8
decoder. A silent VP8 fallback would trade a building-block failure for a
black screen, so T12 ships **audio only**, with no composite H.264 egress to
the browser. Mixed gateway video continues to be consumed by MCU subscribers;
only the browser hybrid bridge egress is blocked. The rest of the groundwork
(StartView forwarding for `source_mid`, sprite view framing) is ready for a
future VP8 decoder or H.264 RTP payloader, but is not claimed as delivered.

No new admin hooks (mode switch, eviction) were added: mixed-room eviction
still invokes the bridge watchdog when either plane disappears.
