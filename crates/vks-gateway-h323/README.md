# vks-gateway-h323

H.323 → WebRTC gateway (phase 6, ADR-0007 «H.323: FFI wrapper on top of
H323Plus»).

Accepts H.323 calls (H.225/H.245 signaling, via H323Plus/OPAL) and "lands"
them in WebRTC-rooms: each H.323 call becomes a **participant** of the
room with a display name `<from>@h323-gateway` (ARCHITECTURE.md §2 —
transparent to WebRTC clients).

## What is included in this wave (self-contained core)

- **`src/codec.rs`** — `H323CodecMap`: deterministic H.323↔WebRTC codec
  negotiation.
- **`src/ffi.rs`** — `H323PlusBridge`: mock FFI boundary with `Watchdog`
  and a panic hook.
- **`src/call.rs`** — `H323Call` (H.225 FSM) + `H323Gateway` (in-memory,
  mock room registry).
- `src/lib.rs` — re-exports.

## Codec map (H.323 → WebRTC)

| H.323 offer | WebRTC codec | transcoding |
|---|---|---|
| G.711 (PCMU/PCMA) | Opus | **yes** |
| G.729 | Opus | **yes** |
| OPUS | Opus | no |
| H.263 | VP8 | **yes** |
| H.264 | VP9 | **yes** |
| VP8 / VP9 | VP8 / VP9 | no |

`H323CodecMap::negotiate(&[H323Codec]) -> (WebRtcCodec, needs_transcode)` —
the first audio codec has priority; if absent, the first video codec.
`negotiate_streams` returns a pair `(audio, video)` for a full bridge.
Video: H.263 → VP8, H.264 → VP9.

## FFI boundary (ADR-0007, BACKEND_RULES §6.2)

The FFI boundary is **one point** — `H323PlusBridge::call`.

- **Panic hook** — a panic in FFI does not kill the process: it is
  intercepted (`catch_unwind`) and converted to `Err`, the module is
  marked `Crashed`.
- **Watchdog** — if an FFI call exceeds the limit (in production 5 s =  5
  ticks; parameterizable), the module is marked `Crashed`,
  `restart()` resets it. Deterministic: instead of real time / threads,
  a tick counter is used.

```
H323PlusBridge::call(op, ticks)
  └─ watchdog.check(ticks > limit)?
       └─ yes → ModuleState::Crashed (restart required)
       └─ no  → ModuleState::Ok
```

## Call (H.225 / Q.931 FSM)

```
Idle ──Setup──► Alerting ──Proceeding──► Connect ──Release──► Terminated
```

- `H323Gateway::on_setup(call_id, from, offer)` → creates a call in
  `Alerting` state, negotiates codecs.
- `H323Gateway::proceeding` / `connect` — H.245 capability exchange,
  Connect.
- `H323Gateway::release` — Release; removes the participant from the room.
- `H323Gateway::land_in_room(room_id, call_id)` — "landing" in the room:
  the call becomes a participant (mock registry: room_id → names).

## What is mocked, and what is for the future

- **Mock:** the FFI boundary (H323Plus), the H.225 transport, the room
  registry. No I/O, deterministic (tick counter).
- **Future (heavy feature `h323plus`):** the real FFI wrapper on top of
  H323Plus/OPAL (C++), the only `unsafe` blocks (`// SAFETY:`), real
  transcoding H.263/H.264→VP8/VP9, G.711/G.729→Opus.

## Dependencies

Default: `vks-core`, `vks-common`, `thiserror`, `tracing`, `serde`,
`uuid`. `h323plus` — optional feature (FFI). No `unsafe` in the default
build.
