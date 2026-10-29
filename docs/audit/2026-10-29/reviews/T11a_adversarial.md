# T11a adversarial review — commit `d9aa3ce`

- **Repo:** `/home/pro/Рабочий стол/mcu`, branch `master`.
- **Commit under review:** `d9aa3ce` "feat(webrtc): real transport by default, keyframe gate,
  same-session re-offer (T11a)" (2026-10-29 08:56:52 +0300), 14 files, +919/−65.
- **Reviewer role:** independent adversarial review. Read-only for the repo: **no source file was
  modified**; the only file written in the repo is this report. Throwaway harness:
  `/tmp/t11a-c` (probe crate) with path dependencies on a pristine **`git archive d9aa3ce`**
  copy in `/tmp/d9aa3ce`; build target `/tmp/t11a-target`. The live DB (`vks_data.db`) was not
  opened, and no server was started from this workspace.
- **Pristine source use:** because the worktree was concurrently edited by another agent (see
  below), every probe was compiled against `/tmp/d9aa3ce`, not the worktree. Line numbers below
  are from `d9aa3ce` (`git show d9aa3ce:<path>:<line>`).
- **Baseline:** `8dfc449` (previous review target); context read:
  `docs/audit/2026-10-29/pitfalls/T11a.md` (prior analysis, hypotheses H4/H5/H7/H8/H9/H10/H12),
  `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md`, `deploy/README.md`,
  `deploy/systemd/*`, `configs/*.toml`, the touched crates, and the str0m 0.24.1 sources
  (`~/.cargo/registry/.../str0m-0.24.1`).

## Worktree state at review time (important)

The tree was **dirty from concurrent work**, not from this review. At ~09:09–09:17 MSK another
agent edited and then committed the T11a follow-ups as **`a33e195`** ("T11a review — survive
rejected re-offers, revalidate room shape…"; `ddd033b` for T02), so HEAD is no longer `d9aa3ce`.
Several defects below are already patched by `a33e195` (marked **[fixed in follow-up
`a33e195`, not in `d9aa3ce`]**). The verdict and score are for **`d9aa3ce` only**; with
`a33e195` applied the same code would score ~7.5 (see §8). All probe evidence was compiled
against a pristine `git archive d9aa3ce` in `/tmp/d9aa3ce`, so it reflects the commit and not
the concurrent edits.

## Verdict

**5.0 / 10 — not shippable as committed.** The core keyframe gate is real and works
end-to-end (verified on live DTLS/SRTP with two peers, §7), and the fail-closed preflight
closes the prior `""`/TEST-NET/loopback/non-assignable holes. However the commit ships one
**P0 media-plane regression**: a perfectly normal browser renegotiation (screen share, data
channel, or a re-created `RTCPeerConnection`) **unregisters the live peer and kills its
audio+video**, while the registry logs "keeping old session" and the signalling session is
rolled back to `MediaActive`. Four P1s follow: the re-offer fallback silently starts a second
session, the headline same-session re-offer is **unreachable from the real signalling flow**
(no client ever sends `IceComplete`), preflight still passes multicast/broadcast candidates,
and an H264-only offer is accepted while the answer rejects video and the server advertises a
VP8 track that cannot flow.

| Count | Severity |
|---|---|
| 1 | **P0** — live media session is destroyed by a rejected re-offer |
| 4 | **P1** — second-session fallback; re-offer unreachable (FSM); multicast/broadcast preflight; H264-only accepted with a lying TrackAdded |
| 9 | **P2** — admission TOCTOU; port-range reservation & MAX_PEERS; preflight ordering; preflight only in `main`; stale docs/units; flag parsing; gate re-arm without PLI; metrics/PLI accounting; test-quality gaps |
| 5 | **P3** — fingerprint-change re-offer; disabled transport + real offer; `MAX_PEERS=0`/range parse; legacy simulcast echo; per-offer flag reads |

## P0

### P0-1. A rejected re-offer removes the live peer: audio and video die for the whole room

**Where:** `crates/vks-media-sfu/src/webrtc_room.rs:205-213` (registry side),
`:643-670` (peer task `Reoffer` arm), `:732` (`remove_generation` after the loop);
`crates/vks-media-sfu/src/webrtc.rs:430-460` (`validate_offer`).

**What happens.** `accept_offer` clones the existing `PeerHandle`, sends `PeerCommand::Reoffer`
and awaits the reply. The peer task calls `peer.accept_reoffer(&offer_sdp)`. `validate_offer`
rejects exactly the renegotiations browsers actually produce with `WebRtcError::InvalidOffer`:

- a second video m-line (`getDisplayMedia`/screen share): `too many media lines: audio=1 video=2`;
- any `m=application` line (data channel): `unsupported media line: m=application 9 UDP/DTLS/SCTP webrtc-datachannel`;
- a re-created `RTCPeerConnection` (new random mids): str0m's `accept_offer` returns
  `Rtc(RemoteSdp("Changed order for m-line with mid: …"))`.

The `Reoffer` arm has only `Ok(..)` and a catch-all `Err(error)`; the catch-all sends `Err` to
the registry, logs `"WebRTC re-offer failed; closing peer"` and **`break`s**. The registry's
`Ok(Ok(Err(error)))` branch then logs `"WebRTC re-offer rejected; keeping old session"` and
returns `Err(WebRtcError::Sdp(..))` — **but the peer task has already exited**, and the cleanup at
`:732` calls `registry.remove_generation(room, participant, generation)`, deleting the live
registration. The comment in the registry is a lie; the session is gone.

**Evidence (pristine `d9aa3ce`, real two-peer UDP/DTLS/SRTP room, media flowing first):**

```
[probe4] after keyframe: has_a=true B media=1, peers=2
[probe4] re-offer with 2 video m-lines -> Err(Sdp("invalid offer: too many media lines: audio=1 video=2"))
[probe4] after rejected re-offer: has_a=false has_b=true peers=1
DEFECT reproduced: a rejected (unsupported) re-offer removed the live WebRTC peer A

[probe5] data-channel re-offer -> Err(Sdp("invalid offer: unsupported media line: m=application …"))
[probe5] after rejected re-offer: has_a=false

[probe12] accept_offer -> Err(Sdp("invalid offer: too many media lines: audio=1 video=2"))
[probe12] t=0ms   has_a=false peers=1     (already gone when accept_offer returns)
```

**Impact.** Participant A's audio+video stop being fanned out to everyone; the browser gets
`webrtc_offer_rejected` (non-retryable, `lib.rs:936-944`), while `handler.rollback_offer()`
(`lib.rs:956-965`) restores the session to `MediaActive`, so the app still believes media is up.
The browser's ICE/DTLS eventually fails; recovery requires a reconnect. Because
`getDisplayMedia`/data channels are ordinary browser features, this is a user-triggerable call
kill on a previously working session — a media-plane regression (pre-commit, a re-offer did not
send `Reoffer` to the live task, so the old peer survived the rejection).

**Fix.** Never let a re-offer command terminate a live peer: match
`Err(WebRtcError::InvalidOffer(_))` (or a dedicated `WebRtcError::RenegotiationUnsupported`)
in the `Reoffer` arm, `reply.send(Err(..))`, increment a counter, and `continue`. Add a
regression test: after a 2-video/application re-offer, `registry.has_peer(room, pid) == true`
and a subsequent keyframe still reaches B. (Follow-up `a33e195` adds this arm;
`d9aa3ce` does not.)

## P1

### P1-1. `Reoffer` queue-full or 2 s timeout silently replaces the session with a second one

**Where:** `webrtc_room.rs:194-225`. `if handle.tx.try_send(command).is_ok() { … 2 s timeout … }`
then falls through to `self.accept_fresh_offer(..)`. `try_send` failure (queue of 64 full) and
timeout are treated identically to "task closed": a **new UDP socket, new str0m `Rtc`, new DTLS
handshake and new ICE credentials** are created for the same participant, the old registration is
overwritten, and the answer (new `a=ice-ufrag`/`pwd`, same shared certificate fingerprint) is
returned to the client's **existing** `RTCPeerConnection`.

**Why it is broken.** An ICE restart renegotiates ICE but does **not** re-handshake DTLS. The
browser will not send a new `ClientHello` on the new 5-tuple, so the fresh server peer never
completes DTLS; the old server peer's socket is no longer the nominated 5-tuple. Media is
silently dead until `IDLE_TIMEOUT` (30 s), on top of the old session being killed.
str0m's client even rejects such an answer (`Ice restart in answer without one in the preceding
offer`); browsers validate an answer's ICE credentials against the offer, and an unrequested
ICE restart in an answer is invalid (RFC 8829 §5.3.1). The full-queue case is exactly the
overload scenario in which media must not be renegotiated.

**Fix.** Distinguish `TrySendError::Full` (return a retryable `webrtc_busy`, keep the session,
increment `webrtc_reoffer_fallback{reason="full"}`) from `Closed` (fresh peer is correct). Do not
replace a live session on timeout; return `webrtc_reoffer_timeout` retryable. Add a test with a
stalled peer task asserting `peer_count` stays 1 and no second `accepted.answer_sdp` appears.
**[fixed in follow-up `a33e195`, not in `d9aa3ce`]**

### P1-2. Same-session re-offer is unreachable from the real signalling flow

**Where:** `crates/vks-signaling/src/handler.rs:343-349`,
`crates/vks-signaling/src/session.rs:123-128`.

`on_sdp_offer` accepts `SdpOffer` only in `InRoom | MediaActive`. The first offer moves
`InRoom → Negotiating`; only a successful `IceComplete` moves `Negotiating → MediaActive` — and
**no client sends `IceComplete`**: `grep -rln "IceComplete\|ice_complete" client tests crates`
matches only `session.rs`, `handler.rs`, the proto definition and
`crates/vks-server/tests/network_integration.rs` — nothing in `client/` or `tests/e2e/`. The server never advances the
session on a successful `SdpAnswer` either. Consequently every second `SdpOffer` on the same WS
(e.g. the screen-share renegotiation that P0-1 handles) is rejected earlier with
`bad_state "SdpOffer requires InRoom or MediaActive (current: Negotiating)"` (non-retryable).
The commit's two registry-level re-offer tests bypass the handler entirely; there is no
handler+registry integration test.

**Impact.** The advertised headline ("same-session re-offer") is dead code in production. The
only path that reaches it is a client that sends `IceComplete` (none exist) or a
hand-crafted WS trace. The e2e client submits `subscribe` *before* the offer; after the first offer the session is
`Negotiating`, so a later `subscribe` would be rejected for the same reason.

**Fix.** Transition to `MediaActive` when the real transport accepts the offer (add a handler
event, e.g. `TransportReady`, driven by `lib.rs:895+` on `Some(Ok(accepted))`), and/or accept
`SdpOffer` in `Negotiating` when the previous offer was answered. Add an integration test that
sends two `SdpOffer` envelopes over one live WS and expects two `SdpAnswer`s and one
`webrtc` peer.

### P1-3. Preflight still passes unreachable multicast/broadcast candidates (fail-open)

**Where:** `crates/vks-server/src/webrtc_runtime.rs:37-73` (`validate_candidate`) and `:75-110`
(bind probe). The new assignability probe binds `(ip, 0)`, which **succeeds on Linux for
`224.0.0.1`, `239.255.255.250` and `255.255.255.255`**, so the preflight returns `Ok` and the
server starts, advertising a multicast/broadcast candidate that no browser can send ICE to.
`is_multicast`/`is_broadcast`/IPv6 multicast are not checked; `0.0.0.0`/`::` are accepted when
`allow_loopback` is set (the default dev config!).

**Evidence (`d9aa3ce`, probe9):**

```
[probe9] "255.255.255.255": strict=Ok(()) dev=Ok(())
[probe9] "224.0.0.1":       strict=Ok(()) dev=Ok(())
[probe9] "239.255.255.250": strict=Ok(()) dev=Ok(())
[probe9] "0.0.0.0":         strict=Err(...) dev=Ok(())      (default.toml is the dev config)
[probe9] "192.168.1.94":    strict=Err("not assignable … EADDRNOTAVAIL")   (good)
```

**Impact.** A typo (`VKS_CANDIDATE_ADDR=255.255.255.255` / a leftover multicast address) yields
a *successful* startup with `caps.dtls_srtp=true` and zero reachable media. This defeats the
commit's stated "fail-closed" property for exactly the class of mistakes the preflight was
added for. Also: the default `configs/default.toml` now ships `candidate_addr = "127.0.0.1"`
(commit changed it from `""`), so the dev systemd unit starts and silently serves loopback-only
media instead of the previous loud exit 78 (see P2-5).

**Fix.** In `validate_candidate`, reject `ip.is_multicast()`, IPv4 broadcast
(`Ipv4Addr::BROADCAST`), `is_documentation()`, `is_link_local()`, `is_unspecified()` in strict
mode, and only allow unicast global/private addresses; keep the bind probe as a secondary
check. Add the matrix from probe9 as a unit test (the only existing test,
`candidate_validation_is_fail_closed`, covers just one case and mutates process env).

### P1-4. H264-only offer is accepted, video m-line is rejected, `TrackAdded` still claims VP8

**Where:** `webrtc.rs:430-460` (no codec check), `lib.rs:1028-1046` (hardcoded
`TrackKind::Audio → Opus`, everything else → `Vp8`), `lib.rs:1013-1024` (`add_publisher`/seat).

**Evidence (probe6, pristine):**

```
[probe6] H264-only offer ACCEPTED; answer m-line="m=video 0 UDP/TLS/RTP/SAVPF "; mids=[Mid(BnD)]; rtp_maps=[]
[probe6] answer contains VP8: false
```

The offer passes `validate_offer` (1 audio, 1 video), str0m rejects the video m-line (port 0,
no formats), yet `accept_offer` returns `Ok`. `lib.rs` then registers the publisher and
broadcasts `TrackAdded{ codec: Vp8 }` (audio → Opus, any video → VP8) for a track that does not
exist; `write_media_matched` to the rejected mid returns `Ok([])` (probe7), so even a local
fanout is silently swallowed — no error reaches the publisher or subscribers. A client that
offers only H264 (hardware/SIP endpoints and any client whose codec list is not under our
control) gets a "successful" session with a permanently black video tile and a lying codec in the
roster.

**Fix.** After `accept_offer`, if `rtp_maps` contains no video codec (or the answer's video
m-line is port 0), return `webrtc_codec_unsupported` and fail the offer closed, or at minimum
register no video track, send no `TrackAdded`, and log once. Derive `TrackInfo.codec` from
`answer.rtp_map` instead of hardcoding VP8. Add an H264-only-negative test.

## P2

### P2-1. Admission lock is dropped before registration; the module doc claims the opposite

`webrtc_runtime.rs:6-7` states "a brand-new offer is registered synchronously under an admission
lock". `lib.rs:885` does `drop(_admission)` before `.await` (necessary for `!Send`), so the
room-shape checks (gateway presence, `effective_room_mode`, `!offer_supports_real_webrtc`) run
under the lock but the registration does not. A SIP/H.323 participant join or an MCU mode switch
can land in between; `evict_rooms` (`webrtc_runtime.rs:121+`) then sees nothing to evict, or an
MCU-mode room gets a real peer. Fix: re-validate the room shape after `accept_offer` returns and
evict/rollback on mismatch (the follow-up `a33e195` adds exactly this), or use a `tokio::sync::Mutex`
that can be held across `.await`, and fix the module doc. **[fixed in follow-up `a33e195`, not in `d9aa3ce`]**

### P2-2. Preflight does not reserve the port range; `MAX_PEERS` vs range width is unchecked

`preflight` binds `min` and **drops** the socket (`webrtc_runtime.rs:101-104`), so the check is a
probe, not a reservation. Probe10: two preflights on `43077..43077` both return `Ok(())`; the
first live registry binds `127.0.0.1:43077`, the second dies at the first offer with
`network: WebRTC port range 43077-43077 exhausted`. Two processes started "successfully" can
therefore race the same range (and the first peer of the first process takes the very port the
second process's preflight would have caught only if it were already bound). Separately, the
default `VKS_WEBRTC_MAX_PEERS=256` with `43000..43100` (101 ports) is accepted at startup;
`deploy/README.md:86` documents the effective `min(MAX_PEERS, range)` but nothing verifies it.
Fix: bind the whole range at startup and keep the sockets (pass them to peers), or at least hold
the start port and validate `width >= min(MAX_PEERS, per_room_cap * rooms)`; log the effective
concurrency and emit a startup warning when it is smaller than `MAX_PEERS`.

### P2-3. Preflight runs after storage init and operator bootstrap

`main.rs:410` calls `preflight` **after** storage/DB init (`main.rs:96-166`) and
`ensure_operator` (`main.rs:188`), which runs the operator/admin bootstrap (create/rotate the
admin account depending on policy). A bad candidate (exit 78) therefore still creates/locks
`vks_data.db` and can mutate the DB before refusing to start.
Fix: run `preflight` before any storage/operator work (it only needs `HandlerConfig` + env).

### P2-4. Preflight exists only in the binary

Only `main.rs:410` calls `preflight`. `build_app` (used by `network_integration`, e2e and any
embedder) never runs it; a bad candidate then fails later per offer (`registry()` returns
`None` → `webrtc_unavailable`), i.e. the "aborts startup" claim holds only for `vks-server`.
Fix: call `preflight` from `build_app`/registry construction (or expose a `validate()` that
embedders must call) and test it.

### P2-5. Documentation and units contradict the new default (operator traps)

- `README.md:37-41` (as committed): "При выключенном транспорте (по умолчанию) поле не
  используется — остаётся legacy SDP" — the commit made the transport **on by default**;
  stale. **[fixed in follow-up `a33e195`, not in `d9aa3ce`]**
- `deploy/systemd/vks-mcu-prod.service:19-22`: "Opt-in: uncomment together with an open UDP
  range…" with `#Environment=VKS_WEBRTC_TRANSPORT=1` commented out. After `d9aa3ce` the prod
  unit enables the real transport **without** any opt-in; the comment is misleading, and the
  unit never sets `VKS_WEBRTC_PORT_MIN/MAX`, so it runs on ephemeral ports (the README/firewall
  story in `deploy/README.md` does not apply).
- `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:71-72`: "Стартовый preflight проверяет
  `candidate_addr` …, **оба порта диапазона и bind**" — false: only `min` is bound once and
  dropped (`webrtc_runtime.rs:101-104`).
- `configs/default.toml` `candidate_addr = "127.0.0.1"` + `allow_insecure_defaults = true`:
  `preflight(.., true)` → `Ok(())` (probe11). The dev unit now starts with a loopback-only
  candidate where the pitfall doc assumed the missing candidate would fail closed. Remote
  browsers get black video with only a `tracing::warn` (registry) and no startup failure.

### P2-6. `VKS_WEBRTC_TRANSPORT` parsing: quoted/typo opt-outs silently mean ON

`enabled()` (`webrtc_runtime.rs:24-30`) matches only the exact trimmed lowercase tokens
`0|false|off|no`. Probe8 (pristine):

```
"0"->false  " false "->false  "OFF "->false       (good)
"\"0\""->true   "'0'"->true   "00"->true   "0x0"->true
"disabled"->true  "Flase"->true  "offf"->true
""->true  " "->true                                 (documented: unset/empty = default ON)
```

A quoted `"0"`/`'0'` is realistic in docker `--env-file`/compose, where quotes are kept;
the documented rollback escape hatch then silently enables the real transport. Fix: strip
matching surrounding quotes before matching, treat any value not in the opt-out set as enabled
but **log the interpreted value once at startup**, and add a unit test with this matrix.

### P2-7. Re-subscribing to video arms the gate but does not request a keyframe

`PeerCommand::SetSubscription` (`webrtc_room.rs:686-692`) sets `waiting_keyframe = true` when
`video` is true, but sends no `RequestKeyframe`. The next forwarded delta is dropped and a PLI is
sent only by the drop path, coalesced to 500 ms (`webrtc_room.rs:628-642`). Probe2:
`SetSubscription(video=true)` + a delta → the delta is dropped and A's PLI count stays `2`
(no new PLI), so the subscriber waits for the next natural IDR (up to the encoder's GOP) or for
a later drop outside the 500 ms window. Fix: call `request_keyframe_from(room, participant,
false)` when `video` turns on (and keep the coalescing).

### P2-8. Metrics/PLI accounting

- `forward` increments `MEDIA_BYTES` at enqueue (`webrtc_room.rs:536`), before the subscriber's
  gate decides to drop the frame, so downlink bytes/bitrate are overstated exactly when the
  awaiting-keyframe gate is closed.
- No metric distinguishes re-offer success/rejection/fallback; only the follow-up `a33e195` adds
  the `reoffer_rejected` label. `WEBRTC_OFFERS{reoffered}` exists,
  but queue-full vs closed vs timeout are invisible.
- The queue-full path in `forward` re-arms the gate **and** calls `request_keyframe_from` for
  every dropped frame (no 500 ms coalescing), i.e. up to one PLI per frame on the publisher.
  Pre-existing, but the commit's "PLI coalescing" claim only covers the gate path.

### P2-9. Test quality: the new tests do not cover the failure modes they claim

- **No test for `enabled()` and no test for `preflight()`** at all. The only new server-side test
  is `candidate_validation_is_fail_closed`, which uses `std::env::remove_var` (process-global) and
  does not exercise the bind probe, port pair, IPv6, multicast or `MAX_PEERS`.
- `accept_reoffer_reuses_the_same_session_and_socket` (`webrtc.rs:488-510`) never connects and
  passes the **same** offer twice; "no second DTLS" is asserted by the absence of a `Connected`
  action on a peer that never ran DTLS. Misleading name; it cannot catch a re-offer regression.
  (The real behavior is verified by probe3, not by this test.)
- `reoffer_is_answered_by_the_existing_peer_task` feeds `"v=0\r\n"` (invalid SDP) and a fake peer
  task that answers `Ok`; it asserts plumbing only. No negative test (rejected re-offer must keep
  the session — P0-1) and no full-queue/timeout test (P1-1).
- The keyframe gate is tested only as a pure function (`keyframe_gate_…`); `run_peer`'s audio
  bypass, queue-overflow re-arm, PLI coalescing, `SetSubscription` arming and the
  `MediaData::is_keyframe` input are untested (probe1/probe2 cover them manually; there is no
  video frame in `real_dtls_srtp_handshake_and_media`).
- `network_integration.rs:119-121` `force_legacy_webrtc_transport()` uses a process-global `Once`
  to set `VKS_WEBRTC_TRANSPORT=0` before every spawn and never restores it. It is the only test
  binary that sends `IceComplete`, and it hides the new default from the *entire* test binary, so
  no server-side test exercises the real path or the default-on flag.
- No e2e re-offer step (`tests/e2e/webrtc.js` sends one offer; `subscribe()` before the offer;
  `IceComplete` nowhere). The pitfall doc's test matrix explicitly required one.

## P3

1. **Fingerprint change on the same mids is silently accepted.** `accept_reoffer` (`webrtc.rs:
   190-206`) does not capture the remote fingerprint; str0m stores it only once
   (`sdp.rs:92-99`), so a same-mids re-offer with a different `a=fingerprint` returns `Ok` and
   the browser's new DTLS cannot complete (probe13: `→ Ok(...)`). A real new `RTCPeerConnection`
   also changes mids, so it hits P0-1 instead; still worth the explicit guard.
   **[fixed in follow-up `a33e195`, not in `d9aa3ce`]**
2. **Disabled transport + real ICE offer → legacy fake SDP, no error.** `webrtc_runtime::
   accept_offer` returns `None` when `enabled()==false`; `lib.rs:940-946` only fails closed when
   enabled, so the client receives the legacy answer with fake DTLS credentials and no
   `webrtc_offer_rejected` (just a metric). Expected for the rollback escape hatch, but log it
   loudly and consider rejecting ICE offers instead of faking them.
3. `VKS_WEBRTC_MAX_PEERS=0` → `unwrap_or(256)` (0 means 256, not "no peers");
   an invalid `PORT_MIN/MAX` pair is silently `None` in the registry (ephemeral) when preflight
   is not run (library path, P2-4), ignoring the operator's firewall range.
4. The legacy answer echoes the offer's `a=simulcast` (`crates/vks-signaling/src/sdp.rs:368-369`)
   while `caps.simulcast=false` — only on the legacy plane, but the caps/answer are inconsistent.
5. `VKS_WEBRTC_TRANSPORT` is read per offer (`lib.rs:853/940`), never once at startup; the
   decision and the `VKS_ALLOW_LOOPBACK_MEDIA=1` bypass (`webrtc_runtime.rs:56`) are not logged
   as a startup summary, so operators cannot see which plane is active from the journal.

## 7. Verified good (evidence, not claims)

From the pristine `d9aa3ce` probe run (real UDP/DTLS/SRTP, two peers, `str0m 0.24.1`):

- **Gate input is correct for real VP8**: three client-written VP8 frames (keyframe/delta/
  keyframe) produce `MediaData::is_keyframe = [true, false, true]`; Opus produces `[false, false,
  false]` and audio bypasses the gate (probe1/probe2: all Opus frames delivered).
- **Gate works end-to-end**: a delta before the IDR delivers 0 frames to B; the IDR delivers
  exactly 1; the next delta delivers 2; 2 Opus frames deliver 2 `MediaData`s (probe2).
  No PLI storm observed; the gate-drop PLI is coalesced (A PLI stays 2→2 within 500 ms).
- **Same-session ICE-restart re-offer works** (when it reaches the registry): the answer
  advertises the same `127.0.0.1:<port>` before and after, `peers=2`, both sides stay
  `connected`, and post-re-offer media reaches B (probe3). No second socket/DTLS.
- **Preflight closes the prior holes**: `""`, `192.0.2.10`/`198.51.100.7`/`203.0.113.9`,
  `127.0.0.1`/`0.0.0.0`/`::`/`::1` (strict), `169.254.1.1`, non-local `192.168.1.94`,
  `2001:db8::1`, `fe80::1`, `"localhost"` all → `Err`; the real LAN IP `192.168.1.93` (and the
  trimmed `" 192.168.1.93 "`) → `Ok` (probe9). Fail-closed preflight is real for these classes.
- **Answer is honest**: `a=fingerprint:sha-256`, `a=ice-ufrag/pwd`, `a=setup:passive`,
  `a=rtcp-mux`, Opus+VP8 only; H264/VP9/AV1/H265 absent; `FF:00:11` gone; caps in `main.rs` say
  `simulcast:false, dtls_srtp:enabled()` (tests `real_dtls_srtp_handshake_and_media`).
- **No `MutexGuard` across `.await`** in the offer path (the admission guard is dropped first);
  no panic/deadlock across probes 1–13 (including a two-peer DTLS/SRTP run); `write` to a rejected mid returns
  `Ok([])` (does not kill the task).
- **Repo checks green at `d9aa3ce`** (log `/tmp/t11a_tests.log`, taken before the 09:09 worktree
  edits): `cargo test -p vks-media-sfu --features str0m --locked` → 90 passed / 0 failed;
  `cargo test -p vks-server --tests --locked` → lib 144 passed / 3 ignored, `network_integration`
  22, `operator_policy` 7, `config_failclosed` 1, `h265_rtp` 1, two 0-test shells; all green; `cargo clippy -p
  vks-media-sfu -p vks-server --all-targets --locked -- -D warnings` → clean.

## 8. Score

**5.0 / 10 for `d9aa3ce`.** 1 P0 + 4 P1. The keyframe gate and the preflight core are genuinely
good work and are proven on the wire, but the re-offer error path destroys live media, the
headline feature is unreachable through the FSM, and the "fail-closed" preflight still passes
multicast/broadcast. With the follow-up `a33e195` fixes for P0-1/P1-1/P2-1/P3-1 applied and the stale
docs/units corrected, the same code would be ~7.5/10; the remaining gap is the FSM
`IceComplete`/re-offer integration (P1-2), the H264-only fail-open (P1-4) and the missing tests.

## 9. Reproduction

```bash
# Repo checks (pristine d9aa3ce, run 09:00 before the worktree edits)
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p vks-media-sfu --features str0m --locked
cargo test -p vks-server --tests --locked
cargo clippy -p vks-media-sfu -p vks-server --all-targets --locked -- -D warnings

# Pristine source + throwaway probes (never writes to the repo)
rm -rf /tmp/d9aa3ce && mkdir -p /tmp/d9aa3ce
git archive d9aa3ce | tar -x -C /tmp/d9aa3ce
cd /tmp/t11a-c    # probe crate, path deps on /tmp/d9aa3ce/crates/*
CARGO_TARGET_DIR=/tmp/t11a-target cargo test --offline -- --nocapture --test-threads=1
CARGO_TARGET_DIR=/tmp/t11a-target cargo test --offline --test srv -- --nocapture --test-threads=1
```

Probe mapping: `probe1` keyframe flags; `probe2` gate + audio; `probe3` ICE-restart re-offer;
`probe4` 2-video re-offer teardown (P0-1); `probe5` data-channel teardown (P0-1); `probe6`
H264-only (P1-4); `probe7` write to rejected mid; `probe8` flag parsing (P2-6); `probe9`
preflight candidate matrix (P1-3); `probe10` port range not reserved (P2-2); `probe11` dev
loopback config (P2-5); `probe12` teardown timeline (P0-1); `probe13` fingerprint-change
re-offer (P3-1). `probe4/5` intentionally fail with "DEFECT reproduced"; all other probes pass.

## 10. Not verified / limits

- No browser/Chromium e2e was run in this session; the P1-2 conclusion is from
  `handler.rs`/`session.rs` and a repo-wide grep for `IceComplete`, not from a live WS trace.
- The full-queue path of P1-1 was not forced (requires 64 pending commands with a stalled peer);
  it is established from the code (`try_send` fall-through) and the str0m DTLS/ICE semantics.
- The multicast candidates were tested on this Linux host only; the check is still missing from
  the code on every platform.
- Multi-host NAT/firewall behavior was not tested (no second host); `deploy/README.md`'s firewall
  contract was reviewed as documentation only.
