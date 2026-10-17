# vks-gateway-sip

SIP → WebRTC gateway (phase 6, ADR-0006 «SIP: rvoip»).

Accepts SIP calls (SIP UAS) and "lands" them in WebRTC rooms: each
SIP call becomes a **participant** of the room with a display name
`<from>@sip-gateway` (ARCHITECTURE.md §2 — transparent to WebRTC
clients). For WebRTC rooms, the gateway participates as an ordinary
participant, and the room does not know that a SIP terminal is
connected to it.

## What is included in this wave (self-contained core)

- **`src/codec.rs`** — `CodecMap`: deterministic SIP↔WebRTC codec
  negotiation (SIP SDP → WebRTC SDP).
- **`src/call.rs`** — `SipCall` (call FSM) + `SipGateway` (in-memory UAS
  with mock room registry).
- `src/lib.rs` — re-exports.

## Codec map (SIP → WebRTC)

| SIP SDP offer | WebRTC codec | transcoding |
|---|---|---|
| PCMU / PCMA (G.711) | Opus | **yes** |
| OPUS | Opus | no |
| H.264 | VP8 | **yes** |
| VP8 | VP8 | no |

`CodecMap::negotiate(&[SipCodec]) -> (WebRtcCodec, needs_transcode)` —
takes the first audio codec (audio has priority), if absent — the first
video codec. `negotiate_streams` returns a pair `(audio, video)` for
a complete bridge.

## Call (FSM)

```
Idle ──INVITE──► Ringing (180) ──accept──► Active (200 OK) ──BYE──► Terminated
```

- `SipGateway::register_uas()` — "registration" of the SIP UAS.
- `SipGateway::on_invite(call_id, from, sdp_offer)` → creates a call in
  `Ringing` state, negotiates codecs.
- `SipGateway::accept(call_id)` — 200 OK.
- `SipGateway::bye(call_id)` — BYE; removes the participant from the room.
- `SipGateway::land_in_room(room_id, call_id)` — "landing" in the room:
  the call becomes a participant (mock registry: room_id → list of names).

## What is mocked, and what is for the future

- **Mock:** the room registry (room → participant names), the SIP
  transport (in-memory UAS). No I/O, deterministic.
- **Future (heavy feature `rvoip`):** real SIP UAS (rvoip), DTLS server
  (RFC 8831, ULP — gateway is DTLS server, WebRTC client is DTLS client),
  ICE-lite (host candidates), actual transcoding G.711↔Opus, H.264↔VP8.

## Dependencies

Default: `vks-core`, `vks-common`, `thiserror`, `tracing`, `serde`,
`uuid`. `rvoip` — optional (feature `rvoip`). No `unsafe` in the
default build.
