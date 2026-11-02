# T12 — implementation checkpoint (paused on request)

Status: **work stopped, nothing committed, no background jobs, no terminals, no live
server restarts.** This file is intentionally untracked; commit it only if the
parent decides the checkpoint belongs in the repo.

- Repo: `/home/pro/Рабочий стол/mcu`
- HEAD at pause: `9db7ae1` (moved from `6ac14c2` by other agents: T09b recorder tap fix
  + review docs). **The worktree is shared with concurrent agents**: many files are
  modified/untracked that are NOT mine (`vks-cluster/*`, `webtransport*`, `recorder.rs`,
  `docs/*`, `Cargo.toml` of `vks-signaling`…). Never `git add -A`.
- My changes (all uncommitted):
  - new `crates/vks-server/src/bridge.rs` (718 lines, untracked)
  - `crates/vks-media-mcu/src/engine.rs` (+271/−)
  - `crates/vks-media-sfu/src/webrtc.rs`, `webrtc_room.rs` (+27, +306)
  - `crates/vks-server/src/{lib.rs (+134), gateways.rs (+107), gateway_audio.rs, webrtc_runtime.rs}`
  - `crates/vks-signaling/src/handler.rs` (+13)

## 1. What is implemented (code complete)

### A. Asymmetric `bridge_available` + lock-order fixes
- `crates/vks-server/src/bridge.rs`:
  - `BridgeSide::{BrowserJoining, GatewayLanding}`; pure
    `bridge_allowed(caps, side, has_browser, has_gateway)`:
    browser needs a gateway, gateway needs a browser; browser+browser and
    gateway+gateway rooms return `false` (no bridge — they stay normal SFU/MCU).
  - `bridge_unavailable_reason(...)` stable labels (`hybrid_disabled`,
    `opus_unavailable`, `no_gateway`, `no_browser`, `allowed`).
  - `BridgeCaps` + `VKS_HYBRID_BRIDGE` kill switch, **default on**; `0/false/off/no`
    disables and restores the pre-T12 error codes; unknown value stays on + warns.
  - `gateway_kind`/`gateway_presence` (suffix match `@sip-gateway` / `@h323-gateway`,
    MCU ids via `parse_participant_id`), `gateway_bridge_available(room)`.
  - `bridge_available_with(state, registry, room, side)` (explicit registry; used by
    tests/e2e), `browser_join_room_mode(state, room)` (single derivation used by the
    guard, `ServerHello` and `JoinRoomResult`).
- `crates/vks-media-mcu/src/engine.rs`:
  - canonical lock order documented and enforced: `pending_layouts → rooms → frames → audio`
    (plus leaf `bridge_tick_rooms`); `apply_layout` (pending→rooms), `remove_publisher`
    and `unbridge_participant` (frames before audio) fixed.
  - `audio_ticks` counter + `audio_ticks_total()`; `mix_participants_audio(room, &[pid])`
    (audio-lock-only gateway-only mix); tests:
    `lock_order_stress_landing_vs_layout_and_remove`,
    `mix_participants_audio_is_scoped_to_the_listed_sources`,
    `audio_tick_counter_advances_once_per_accepted_tick`,
    `bridge_tick_ownership_blocks_other_tickers_and_is_released`.
  - **new**: `tick_audio_at_bridge` (privileged), `set_bridge_tick_owner(room, bool)`,
    `bridge_owns_tick(room)`; plain `tick_audio_at` is a no-op while a bridge owns the
    room — this is what makes "one ticker per room" engine-enforced (covers the AAC
    egress in `gateway_aac.rs`, which is outside my write scope).
- `crates/vks-server/src/gateways.rs`: all 7 guard sites now
  `room_has_peer && !gateway_bridge_available` (`incoming_sip_call`,
  `incoming_h323_call`, `land_gateway_participant_in_room`, `dial_with_contact_inner`,
  outbound announce under admission, inbound SIP UAS 486, inbound H.323 hangup);
  `gateway_room_conflict()` keeps the legacy messages byte-identical when the kill
  switch is off and names the reason when the bridge is enabled but caps are missing.
  `land_gateway_participant_in_room` drops the admission lock before
  `ensure_room_bridge`; the outbound dial does the same.
- `crates/vks-server/src/lib.rs`: WebRTC offer guard rewritten around the one predicate
  (`webrtc_mcu_room_unsupported` / `webrtc_gateway_room_unsupported` kept for the
  non-bridgeable shapes, new `bridge_unavailable` only when hybrid is on but caps are
  missing); `still_ok` revalidation uses the same predicate with "this peer present";
  `ensure_room_bridge` called after the browser is registered as an MCU publisher.

### B. `room_mode` on ClientHello → ServerHello/JoinRoomResult
- `vks-signaling`: `SignalingHandler::set_room_mode(i32)` (rebuilds caps from the codec
  SSOT, no enum literals).
- `vks-server/src/lib.rs`: before `handler.handle(env)`, a `ClientHello` resolves its
  room reference (`resolve_room_ref`, UUID or name, never creates a room) and sets the
  derived mode; `JoinRoomResult.room_mode` uses `browser_join_room_mode` too.

### C. Audio bridge (code complete, tests not yet run)
- `crates/vks-media-sfu`:
  - `webrtc_room`: `pub trait MediaTap { on_media(room, from, audio, &MediaData) }`,
    registry `set_media_tap/clear_media_tap/room_has_tap/tap_media`, `PeerCommand::WriteBridge`,
    `write_bridge_media(...) -> usize` (`try_send`, counted drop
    `webrtc_frames_dropped{result="bridge_queue_full"}`), `peer_participants(room)`;
    `WebRtcAction::Media` now taps before SFU fanout; `run_peer` handles `WriteBridge`
    (`bridge_egress_mid` → `payload_type_for_codec` → `write_media`, drops per frame on
    `UnknownMid`/codec-not-negotiated, never tears the peer down).
  - `webrtc.rs`: `bridge_source()` (OnceLock fixed UUID), `bridge_egress_mid(audio)`,
    `is_audio_mid` made `pub(crate)`.
  - new SFU tests: `media_tap_receives_frames_and_can_be_replaced`,
    `bridge_egress_is_bounded_and_targets_every_peer` (a compile fix was applied after
    the last run: `opus_params()` now binds the writer first; **not re-run yet**).
- `crates/vks-server/src/bridge.rs`: `BridgeTap` (bounded queue 64, drops counted),
  `ensure_room_bridge` (idempotent, one task per room, requires a Tokio runtime),
  `stop_room_bridge(mcu, registry, room)`, `bridge_active`, `bridge_owns_tick`,
  `run_bridge`: single 20 ms ticker per room → per-browser 960-sample re-framer
  (`MAX_BUFFER_SAMPLES = 6*960`, silence when no audio) → `set_audio_slice` →
  `tick_audio_at_bridge` (exactly one) → `mix_participants_audio(gateways)` (gateway-only
  mix, no browser echo/double delivery) → `OpusCodec::encode` → `write_bridge_media`.
  Watchdog stops when either plane disappears; teardown removes only its own entry and
  clears the tap under the same lock a restart uses.
- `gateway_audio.rs`: `media_clock_ms` is now `pub(crate)` and documented as the single
  clock; the RTP loop skips `tick_audio_at` when `bridge::bridge_owns_tick(room)` (plus
  engine-level enforcement).
- `webrtc_runtime.rs`: `existing_registry()` (never initializes) for predicates/start.

### D. H.264 egress — **blocked** (report not yet written, evidence already collected)
Codec SSOT says H264 is `webrtc: Unsupported(NoImplementation)`; `webrtc_answer_specs()`
is Opus+VP8 only; `accept_offer` matches Opus/VP8 with a fail-closed arm;
`webrtc.rs:854-864` meta-test asserts the answer carries exactly `["OPUS","VP8"]`; the
compositor emits H.264/H.265 and there is no VP8 encoder/decoder in the MCU, and str0m
0.24 does not packetize an arbitrary H.264 frame for egress without negotiated H264 PT.
`BridgeCaps.video_bridge = false` and the module docs state audio-only mixed rooms; there
is no VP8 fallback.

## 2. Verified state at pause

- `cargo check -p vks-server --lib --offline --locked` → **Finished** (clean, no warnings).
- `cargo check -p vks-media-sfu --features str0m --offline --locked` passed *before* the
  last test-only edit; the SFU test binary compile fix has **not** been re-verified.
- Not run yet: any test of the new code, `cargo fmt`, `cargo clippy`.
- Baseline counts (captured from clean `6ac14c2`, log `/dev/shm/t12_baseline.log`):
  sfu **47 passed**, mcu **128 passed / 1 ignored**, server `--lib` **154 passed / 3 ignored**,
  `network_integration` **22 passed** (one earlier server-lib run flaked 153/1; not
  reproduced in 3 reruns).

## 3. Remaining work (in order)

1. **Compile + run new unit tests** (expect the failures below to be fixed first):
   - `export PATH="$HOME/.cargo/bin:$PATH"`
   - `cargo test -p vks-media-sfu --features str0m --offline --locked` (expect 49)
   - `cargo test -p vks-media-mcu --offline --locked` (expect 129 + 1 ignored)
   - `cargo test -p vks-server --lib --offline --locked` (expect 154 + 6 new
     `bridge::tests` = 160, 3 ignored)
   - `cargo test -p vks-signaling --offline --locked`
   - `cargo test -p vks-server --test network_integration --offline --locked` (22)
   Likely needed fixes: `tests_support` imports in `bridge.rs` (`str0m` paths), clippy
   lints on new code, `GatewayMediaStats`/`GatewayMediaControls` visibility if the e2e
   uses them.
2. **Add the missing signaling test** for `set_room_mode`: ServerHello/JoinRoomResult
   report Mcu and caps keep Opus+VP8 + `dtls_srtp`; default stays Sfu.
3. **Write `crates/vks-server/tests/mixed_room.rs`** (real-media e2e, run 3×):
   - `AppState::new()`, `McuEngine::new()`, `MediaRegistry::new()`, both `create_room`;
   - browser: real str0m client with a loopback UDP socket, audio `sendrecv` offer,
     `webrtc_runtime::accept_offer(&HandlerConfig::default_test(), room, pid, sdp)`,
     `SdpAnswer::from_sdp_string`, pump `client.poll_output` ↔ `Input::Receive` against
     the `a=candidate: 127.0.0.1 <port>` from the answer (pattern:
     `crates/vks-media-sfu/src/webrtc.rs::real_dtls_srtp_handshake_and_media`);
   - mirror `lib.rs`: `mcu.add_publisher` + `seat_publisher` with an audio track;
   - gateway: `gateways::land_gateway_participant_in_room(&state, Some(&mcu), &room,
     "sip-<uuid>@sip-gateway", "SIP", false)` then
     `gateways::spawn_rtp_media_bridge(RtpBridgeSession { audio_codec: Some("OPUS"), … })`
     with a real UdpSocket; assert `bridge_active`/`bridge_owns_tick` after ~100 ms;
   - browser→gateway: send real Opus 1 kHz frames from the client; decode the RTP the
     gateway terminal receives (`OpusCodec::decode`) and assert 1 kHz energy;
   - gateway→browser: send PCMU 440 Hz RTP to the gateway socket; decode the `MediaData`
     the client receives and assert 440 Hz energy and **no** 1 kHz (gateway-only mix =
     no echo of the browser);
   - cadence: `mcu.audio_ticks_total()` grows ~50/s (not ~100/s), 20 ms packet spacing;
   - teardown: `terminate_gateway_participant_in_room(...)` → bridge stops within ~200 ms,
     tick ownership released, browser SFU media keeps working.
4. **Docs (E)**: `docs/architecture/MEDIA_PIPELINE.md` (§2 SFU, §3 MCU, codec table
   ~line 171), `docs/architecture/ARCHITECTURE.md`, `docs/audit/2026-10-29/00_MASTER_PLAN.md`
   (tracker row line 66 P5, line 200 T12), plus an explicit "H.264 mixed-room egress
   blocked, audio-only delivered" statement and the `VKS_HYBRID_BRIDGE` switch.
5. **Gates on a clean snapshot** (`git archive 9db7ae1` into `/dev/shm`, copy only my
   files, `--offline --locked`): `cargo fmt --check`, `cargo clippy -p vks-media-sfu
   --features str0m`, `-p vks-media-mcu`, `-p vks-server`, `-p vks-signaling` and the
   workspace `--all-features --all-targets -- -D warnings`; all test commands above 3×;
   compare counts to §2.
6. **Commit** focused, `T12:` prefix, staging only my files (never `git add -A`);
   do not amend other agents' commits. Then send the final report to the parent
   (`session-b0563da0-0a3a-41d2-9844-6a9c7d5a927c`).

## 4. Open risks / decisions to re-check on resume

- `ensure_room_bridge` in `lib.rs` sits inside `if !tracks.is_empty()`: an offer that
  negotiates no track does not start the bridge (deliberate — nothing to bridge).
- Hybrid default-on changes the behaviour of any *existing* test that expects a
  gateway+browser room to be rejected; a grep found no such assertions in
  `crates/vks-server/tests`, but re-run the whole affected set.
- `bridge_available_with`/`decide` are `pub` but only used by tests/e2e; keep or gate
  behind `#[cfg(test)]` if clippy complains (pub items do not warn).
- New metrics were not added (`vks-common/metrics.rs` is out of scope): only existing
  series labels are used (`webrtc_frames_dropped{result="bridge_*"}`, `webrtc_sources`).
- Admin eviction hooks (`vks-admin/src/{mode.rs,rooms.rs,settings.rs}`) are out of scope
  and untouched; mixed-room eviction on mode switch is preserved because the bridge
  watchdog stops the task when either plane disappears.
- `gateway_aac.rs` was not edited (out of scope); its `tick_audio_at` is neutralised by
  the engine-level tick ownership.
