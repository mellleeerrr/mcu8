# T11a — final adversarial review (`a33e195`)

**Verdict: 8.0 / 10.** Three of the four prior P1s are fully addressed (the
fourth, P1-1, only for the `InvalidOffer` cases), verified with a real-UDP
two-client str0m harness; the m5 bucket is real and charged once per request,
and the required test/clippy suite is green on a pristine `a33e195`. However, the commit's own guarantee — *"a re-offer this peer
cannot negotiate is rejected with an error but the live Rtc/DTLS session is
kept"* — is **not complete**: a pre-mutation str0m SDP parse error still tears
down the healthy peer (P1-1). In addition, the only real clients in the repo
never reach `MediaActive`, so every browser re-offer is rejected by the signalling
FSM as `bad_state` before the fixed code is consulted (P1-2). The fix commit also
adds **no test** for any of the changed behaviour.

| Severity | Count | Items |
|---|---|---|
| **P0** | **0** | — |
| **P1** | **2** | P1-1 pre-mutation Sdp error closes the live peer · P1-2 re-offer unreachable through the FSM |
| **P2** | **12** | P2-1 … P2-12 (P2-1/P2-2/P2-9 are small; the rest are material) |

Reviewed commit: `a33e195` (`fix(webrtc): T11a review — survive rejected
re-offers, revalidate room shape, limit m5`), on top of the T11a feature
`d9aa3ce`. Base for every line number below: the clean archive of `a33e195`
(`git archive a33e195 | tar -x -C /tmp/t11a_tree`), **not** the live working tree
(which was dirty with concurrent agent work during this review; see §7).

---

## 1. Scope and method

* Read `docs/audit/2026-10-29/reviews/T11a.md` (7.0/10, 4 P1 + 11 P2) and
  `docs/audit/2026-10-29/pitfalls/T11a.md`.
* Re-read the str0m 0.24.1 sources in
  `~/.cargo/registry/src/index.crates.io-*/str0m-0.24.1/` for the exact
  `accept_offer`/`change/sdp.rs`/SDP-parser semantics rather than trusting the
  project comments.
* Ran the required commands on the pristine archive.
* Built a harness and three throwaway probes in `/tmp` (not committed):
  * a **real-UDP harness** with two genuine str0m clients, VP8 media through the
    real `WebRtcRoomRegistry`, and four re-offers (extra video, data channel,
    changed DTLS fingerprint, valid same-session re-offer);
  * a **signalling-FSM probe** driving the real `SignalingHandler` exactly like
    `tests/e2e/webrtc.js` does;
  * an **m5 bucket probe** driving the real `build_app_with_services` router
    (`oneshot`) and a **preflight probe** running the real `vks-server` binary
    with a temporary config/DB.
* Did not modify any repo source, did not touch the live `vks_data.db`
  (mtime stayed `2026-10-29 07:33:29.801`), and did not start the live server.

---

## 2. Required commands (pristine `a33e195`)

```
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p vks-media-sfu --features str0m --locked
cargo test -p vks-signaling --locked
cargo test -p vks-server --tests --locked
cargo clippy -p vks-media-sfu -p vks-server --all-targets --locked -- -D warnings
```

| Command | Result |
|---|---|
| `cargo test -p vks-media-sfu --features str0m --locked` | **ok — 90 passed**, 0 failed (70 s) |
| `cargo test -p vks-signaling --locked` | **ok — 44 passed**, 0 failed (18 s) |
| `cargo test -p vks-server --tests --locked` | **ok — 175 passed**, 3 ignored (141 s): lib 144, config_failclosed 1, h265_rtp 1, network_integration 22, operator_policy 7 |
| `cargo clippy -p vks-media-sfu -p vks-server --all-targets --locked -- -D warnings` | **ok — exit 0, no warnings** (62 s) |
| isolated `test_package_d_mcu_pipeline_handover_and_pnm_control_events` with a clean env | **ok — 1 passed** (1.05 s) |

The isolated run is the real proof for prior P1-3 (the helper now calls
`force_legacy_webrtc_transport()`); the full file also passes.

---

## 3. What the fixes actually do (verified)

### 3.1 Rejected `InvalidOffer` re-offers keep the live Rtc (harness)

Real-UDP harness output (clean run, exit 0):

```
peer1 answer: ufrag=xzyDeIIvOnzXGYY5 port=51260
wrote VP8 keyframe on peer1
  before re-offers: forwarded to subscriber: is_keyframe=true len=8
extra-video re-offer rejected: SDP: too many media lines: audio=1 video=2
  after extra-video rejection: forwarded to subscriber: is_keyframe=true len=8
data-channel re-offer rejected: SDP: unsupported media line: m=application 9 UDP/DTLS/SCTP webrtc-datachannel
changed-fingerprint re-offer rejected: SDP: DTLS fingerprint changed: the browser created a new PeerConnection; open a new transport
  after data-channel/fingerprint rejection: forwarded to subscriber: is_keyframe=true len=8
valid re-offer answer: ufrag=xzyDeIIvOnzXGYY5 port=51260
  after valid same-session re-offer: forwarded to subscriber: is_keyframe=true len=8
HARNESS OK: rejected re-offers keep the live Rtc; media still fans out; valid re-offer reuses it
```

This confirms, behaviourally:

* `PeerCommand::Reoffer` + `WebRtcError::InvalidOffer` → `continue` in
  `run_peer` (`webrtc_room.rs:681-690`); peer task survives and keeps forwarding
  keyframes to the second client after every rejection.
* `validate_offer` is enforced before the fingerprint check
  (`webrtc.rs:430-462`): extra video m-line and `m=application` are rejected
  with specific messages.
* Changed DTLS fingerprint is rejected (`webrtc.rs:195-201`; harness shows the
  new certificate case), and the live peer stays registered/connected.
* A valid re-offer is answered on the **same** Rtc/socket: identical local ICE
  ufrag **and** candidate port (`xzyDeIIvOnzXGYY5`, `51260`) before/after, no new
  UDP socket, no second `Connected`, media still fans out.
* Queue-full no longer silently creates a second DTLS session: `try_send` `Full`
  returns `Err("re-offer queue is full; retry")` (`webrtc_room.rs:200-205`);
  `Closed` falls through to a fresh registration (`:196-199`). (Code inspection;
  no test — see P2-4.)

### 3.2 m5 endpoints: bucket covered, one charge per request, still 401 anon

`m5_probe` (real `build_app_with_services`, fresh bucket per burst):

```
/api/v3/mcu/farm anonymous -> 401 Unauthorized
/api/v3/gateways/status anonymous -> 401 Unauthorized
/api/v3/cluster/status anonymous -> 401 Unauthorized
/api/v3/mcu/farm: 260 requests -> allowed=200 limited=60
/api/v3/gateways/status: 260 requests -> allowed=200 limited=60
/api/v3/cluster/status: 260 requests -> allowed=200 limited=60
single-charge: 300 requests in 38.432524ms -> allowed=200 limited=100 sample=[401, 401]
shared bucket: /api/v3/login immediately after burst -> limited=12/12
M5 PROBE OK
```

* All three endpoints are behind the **same process-wide**
  `global_bucket_middleware` (`lib.rs:191-195`); 200 requests pass, then 429.
* The bucket is charged **exactly once** per request (allowed = 200 of 300, not
  100; a double charge would halve it).
* Anonymous is still **401** (not 200/403); the 401/403/200 path is covered by
  the existing `test_farm_metrics…` integration test, which passed.
* The shared bucket is the same one `/api/v3/login` uses (12/12 429 immediately
  after the burst).

### 3.3 Admission re-validation (code inspection)

`lib.rs:890-1005`: the admission guard is dropped before the async
`accept_offer` (`:888`), and the room shape is re-read under the guard *after*
registration (`:903-919`). On failure: `registry.remove(room, pid)`,
`real_webrtc_peer=false`, `webrtc_failure=webrtc_room_changed`; the pending
`SdpAnswer` is retained away (`:999`) and only an error is sent. `AcceptedPeer` is
dropped at the end of the arm, so the peer task exits when the channel closes.
This fixes prior P1-2. No test drives the race (P2-4).

---

## 4. Findings

### P1-1 — A pre-mutation SDP parse error still closes a healthy live peer

**The commit's central promise is not met for every rejection path.** The
`InvalidOffer` arm keeps the peer (`webrtc_room.rs:681-690`), but every other
error still does `break` (`:691-695`), and `accept_reoffer` maps a **pre-mutation**
str0m parse failure to `WebRtcError::Sdp`:

* `webrtc.rs:203` — `SdpOffer::from_sdp_string(offer_sdp).map_err(|e| WebRtcError::Sdp(e.to_string()))?`
* `validate_offer` (`webrtc.rs:430-462`) only checks size/ICE-credential
  substrings/fingerprint presence/m-line counts; it does **not** parse the SDP,
  so malformed grammar passes it.
* `run_peer`, `webrtc_room.rs:691-695` — `Err(error) => { reply.send(Err(...)); break; }`
  → `remove_generation` removes the peer.

Harness (the only difference from the valid offer is `t=0 0` → `t=0 1`, which
keeps the ICE creds, fingerprint and m-lines intact):

```
malformed-SDP re-offer rejected: SDP: SDP: SDP parse: Parse error ...
Unexpected `1`
Unexpected `t`
...
peer still registered after malformed re-offer: false
thread 'main' panicked ... P1: a malformed pre-mutation re-offer closed the live peer
```

This is exactly the prior P1-1 class: a re-offer the SFU must reject kills the
live call (self-inflicted; the user loses their own media and must renegotiate
from scratch). The fix is small: map `SdpOffer::from_sdp_string` errors (and any
other pre-mutation error in `accept_reoffer`) to `InvalidOffer`, or split
`accept_reoffer` into "pre-mutation rejection" vs "post-mutation failure" and
make the former non-fatal. Add a unit test: malformed SDP → peer still
registered and still forwarding.

**Impact:** an authenticated/buggy client can kill its own live call with a
malformed re-offer; no cross-tenant effect. Still a P1 because the commit
explicitly claims the opposite.

### P1-2 — A browser re-offer is unreachable: the signalling FSM rejects it `bad_state`

The fixed registry path is never reached by a real client on a second offer:

* `handler.rs:343-354` — `SdpOffer` is allowed only in
  `SessionState::InRoom | SessionState::MediaActive`.
* `session.rs:126` — `MediaActive` is reached **only** via
  `SessionEvent::IceComplete` (success).
* No client in the repo sends proto `IceComplete`:
  `tests/e2e/webrtc.js` (`SignalPeer`, lines 130-208) has only
  hello/join/subscribe/offer; `client/vks-client-sdk/src` contains no
  `IceComplete`; the PNM bundle has `RTCPeerConnection` but no `SdpOffer` at all.
* `lib.rs:850` calls the real registry only if `handler.handle` produced an
  `SdpAnswer`; a `bad_state` error means the registry/`accept_offer` is never
  consulted.

FSM probe (same sequence as the e2e client):

```
hello -> ServerHello, state=Connected
join -> ..., state=InRoom
offer#1 -> SdpAnswer, state=Negotiating
offer#2 (no IceComplete, like tests/e2e/webrtc.js) -> Error{code=bad_state, retryable=false, msg=SdpOffer requires InRoom or MediaActive (current: Negotiating)}
control IceComplete -> Ack, state=MediaActive
control offer#2 after IceComplete -> SdpAnswer
PROBE OK: real-client re-offer is rejected as bad_state in a33e195
```

Consequences: the T11a same-session re-offer is not exercisable end-to-end by
the repo's only real client; only the registry unit test/my harness (which call
`accept_offer` directly) exercise the fixed code. The live *working tree* has an
uncommitted one-line allowance for `Negotiating` in `handler.rs` + a new
`(Negotiating, SdpOffer)` transition in `session.rs`; **neither is in
`a33e195`**. Note that even that change does not fix `SubscribeRequest`/
`Unsubscribe` after the first offer (both still require `InRoom|MediaActive`,
`handler.rs:443-464`), so a client that subscribes after negotiating still gets
`bad_state`.

**Impact:** the feature is dead for real clients; the fix commit's behavioural
benefit is unproven in production. Fix: allow the transition (the working-tree
change) *and* send `IceComplete` from the e2e client (or have the real transport
apply it on DTLS connect), and add an e2e re-offer step.

---

### P2 findings

#### P2-1 — `SetSubscription(video=true)` still has a store-order window (prior P2-2 residual)

`webrtc_room.rs:420-425` stores `video=true` **before**
`waiting_keyframe=true`; the target task can process a queued video `Write`
between the two stores and forward a delta without a keyframe
(`gate_video` is evaluated at processing time, `:642`). The comment says
"before any queued Write can slip in", but the arm is after the subscription
flag. Swap the two stores (arm the gate first). Tiny window, decoder
glitch until the next keyframe.

#### P2-2 — A successful re-offer does not re-arm the gate / refresh subscription atomics (prior P2-3)

`webrtc_room.rs:212-219` only flips `publishes_video` false→true; it never
re-arms `waiting_keyframe`, re-applies `set_subscription`, or clears
`publishes_video` when the re-offer stops publishing video. A re-offer that
changes direction can leave the fanout/filters stale until an explicit
`SetSubscription`. Add a regression test.

#### P2-3 — Re-offer replies before its own timer flush (prior P2-5)

`run_peer`, `webrtc_room.rs:662-667`: the `Ok` reply is sent **before**
`peer.handle_timeout(now)` at `:671-677`. If the timer fails, the client already
has a "successful" answer but the task `break`s and the peer is removed. Flush
first, then reply.

#### P2-4 — The fix commit adds no test for any of its changes (prior P2-6)

`git show --stat a33e195` shows the test diff is exactly **one line**
(`force_legacy_webrtc_transport()` in the mcu helper). There is no test for:
rejected re-offer keeps media; changed fingerprint; queue-full; `InvalidOffer`
vs `Sdp`; admission re-validation; m5 429/single charge; preflight/exit 78;
`enabled()`/caps/forward gate. The 90 SFU tests cover only the happy-path
`accept_reoffer_reuses_the_same_session_and_socket`; the 22 network tests force
the legacy path. My harness/probes are **external** and not committed, so CI
would not catch P1-1 or a regression of the fixed paths.

#### P2-5 — Tests still mutate process-global env from parallel threads (prior P2-7)

`network_integration.rs:119-123` (`set_var` under `Once`; still a data race
against any concurrent `enabled()` read), `:2072` `remove_var("VKS_METRICS_TOKEN")`
and `:2216-2217` restore it, `webrtc_runtime.rs:208` `remove_var`. The `Once`
fixes the order dependence of the mcu test but not the unsoundness. Use a
serialized test env (e.g. one process per binary or a global test lock) and
pass the switch explicitly.

#### P2-6 — `TrackInfo`/`MEDIA_BYTES` labels are not derived (prior P2-8)

`lib.rs:1077-1085` announces every `TrackAdded` with `clock_rate: 90000,
channels: 2, ssrc: 0` — an audio track is therefore announced as 90 kHz /
stereo, which is wrong (`Opus` is 48 kHz). `webrtc_room.rs:552-555` labels every
non-audio byte `"vp8"` and every audio byte `"opus"`; that is accurate **today**
because the answer is VP8/Opus-only, but it is hardcoded, not derived from
`params.spec().codec`. The prior review's "not a lie today" caveat still holds;
the `TrackInfo` audio clock/channels is a real mislabel now.

#### P2-7 — Preflight validates a derived candidate and runs after storage init (prior P2-9)

`main.rs:294-306` substitutes `127.0.0.1` (or the media-loop IP) when
`config.candidate_addr` is empty; `handler_config` (`:359-375`) and
`preflight(&handler_config, …)` (`:410`) therefore never see an empty candidate.
Probe with the real binary:

```
# candidate_addr = "192.0.2.7" (TEST-NET)
EXIT: 78
... WebRTC preflight failed error=candidate_addr 192.0.2.7 is a documentation (TEST-NET) address
# candidate_addr = "" (prod config)
EXIT: 78
... candidate_ip=127.0.0.1
... WebRTC preflight failed error=candidate_addr 127.0.0.1 is loopback/unspecified
```

So an operator who left `candidate_addr = ""` gets the *loopback* message, not
`webrtc_runtime.rs:39-40`'s `"candidate_addr is empty"`. Worse, the probe shows
the SQLite DB is **created before** the preflight: the server logs
`connecting to SQLite database` at `:117`, the DB file appears
(`/tmp/t11a_preflight.db`, 155648 bytes, 09:37:45), and only then preflight
fails (09:37:46). A config typo creates/opens the DB and the `DbLock` before
`exit 78` (pitfalls H12). Validate `config.candidate_addr` directly and move
preflight above storage init; it only needs `Config`.

#### P2-8 — Stale docs / mangled doc comments (prior P2-10)

* `webrtc_room.rs:4-6` — "Signalling calls `accept_offer` **synchronously**"; it
  is `async` and awaits a peer round-trip (`:179`).
* `webrtc_runtime.rs:4-7` — "a brand-new offer is registered **synchronously
  under an admission lock**"; registration happens in the peer task after the
  lock is released (`lib.rs:888-896`).
* `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:51` — "Re-offer атомарно
  заменяет поколение пира" (it reuses the same generation/Rtc);
  `:129` — "Проверка и регистрация пира/шлюза сериализованы общим
  admission-lock" (registration is async, re-validated after the fact).
* `deploy/systemd/vks-mcu-prod.service:19-20` — "Opt-in: uncomment together
  with …" / `#Environment=VKS_WEBRTC_TRANSPORT=1`; the transport is default-on.
* `webrtc.rs:420-429` — `fingerprint_of` was inserted between `validate_offer`'s
  doc block and `validate_offer`, so `fingerprint_of` now carries a hybrid doc
  ("Rejects offers… Extracts the first `a=fingerprint…`") and `validate_offer`
  itself has no doc at all (the same class of mangling the prior review flagged
  at `webrtc_runtime.rs:182`, where a bare `///` line remains).
* `HandlerConfig::new`/`default_test` still hardcode `dtls_srtp: true` and
  VP8/Opus caps (`handler.rs:77`, `:132`); `main` overrides them, other
  constructors get a lie.

#### P2-9 — Nits (prior P2-11)

`webrtc.rs:155-176` and `:203-224` duplicate the "strip trickle + rebuild
mids/mid_audio" block; `run_peer` calls `handle_timeout` in the re-offer arm
(`:671`) and again on the common path (`:728`); the re-offer unit test doesn't
assert `a=candidate` (the harness did parse the candidate port, so the answer
does carry it). All cosmetic/low.

#### P2-10 — `retryable=false` for errors whose message says "retry"

`lib.rs:1000`: `retryable = matches!(code, "room_full" | "webrtc_capacity")`.
`webrtc_room_changed` ("Room shape changed during negotiation …; retry") and the
queue-full `webrtc_offer_rejected` ("re-offer queue is full; retry",
`webrtc_room.rs:204`) are returned with `retryable=false`, so a conforming
client will not retry a transient condition. `webrtc_unavailable` (transient
registry init failure) is also false. Derive the flag from the error class, or
set it for the retryable messages.

#### P2-11 — Metrics misuse on rejected re-offers

`lib.rs:1002-1004` increments `WEBRTC_FALLBACK{result="offer_rejected"}` when a
**live** peer's re-offer is rejected — that is not a fallback (the peer is still
real and forwarding). `webrtc_room.rs:688` increments
`WEBRTC_FRAMES_DROPPED{reason="reoffer_rejected"}` for an SDP rejection, which
is not a dropped frame. Dashboards/alerting will misread both. Add a dedicated
`WEBRTC_REOFFERS{result=…}` counter or use the existing
`WEBRTC_OFFERS{result="reoffer_rejected"}` that the registry already increments.

#### P2-12 — Re-offer re-announces every track via `TrackAdded`

`lib.rs:1044-1090`: on every accepted `SdpOffer` (including a re-offer with the
same tracks) the server broadcasts `TrackAdded` for **all** tracks of the
publisher, with the same `track_id` (and `ssrc: 0`). The SFU engine's
`add_publisher` replaces the track list, but the client-side protocol has no
dedupe; a client that treats `TrackAdded` as "new" will add duplicates. There is
no test. Announce only genuinely added/removed tracks on a re-offer (or add a
`TrackUpdated`).

---

## 5. Prior findings — status

| Prior | Status | Evidence |
|---|---|---|
| P1-1 rejected re-offer tears down the live call | **Partial** | `InvalidOffer` cases fixed (harness: media keeps flowing); `Sdp` pre-mutation error still closes the peer (P1-1 here) |
| P1-2 admission lock released before registration | **Fixed** | `lib.rs:903-1005` re-validates under the guard, removes + errors, no answer; code-inspected only |
| P1-3 order-dependent mcu integration test | **Fixed** | helper calls `force_legacy_webrtc_transport()`; isolated run passed |
| P1-4 root README default | **Fixed** | README now says real transport on by default; verified |
| P2-1 queue-full → fresh DTLS | **Fixed** | `try_send` `Full` → error, `Closed` → fresh (`webrtc_room.rs:194-205`) |
| P2-2 `SetSubscription` re-arm race | **Partial** | arm moved before `try_send`, but after `video.store` (P2-1 here) |
| P2-3 re-offer does not re-arm the gate | **Open** | P2-2 here |
| P2-4 new DTLS fingerprint accepted silently | **Fixed** | `webrtc.rs:193-201`; harness rejects it and keeps media |
| P2-5 reply before timer flush | **Open** | P2-3 here |
| P2-6 test coverage | **Open** | one-line test diff; no regression tests (P2-4 here) |
| P2-7 env mutation from parallel tests | **Open** | P2-5 here |
| P2-8 `MEDIA_BYTES`/`TrackInfo` hardcoded | **Open** | P2-6 here (audio clock/channels now wrong) |
| P2-9 preflight derived candidate / after storage | **Open** | P2-7 here, now probed end-to-end |
| P2-10 stale docs | **Open** | P2-8 here |
| P2-11 nits | **Open** | P2-9 here |

Prior P1-2 is fixed but untested; prior P1-1 is only fixed for the three
`InvalidOffer` cases and still fails for a malformed pre-mutation SDP.

---

## 6. Counts, score, and the path to 9.5+

* **P0: 0.**
* **P1: 2** (P1-1 incomplete rejected-re-offer survival; P1-2 re-offer
  unreachable through the FSM).
* **P2: 12** (P2-1 … P2-12).

**Score: 8.0 / 10.** Justification: the transport-level prior P1s are real
fixes, behaviourally verified (rejected `InvalidOffer` keeps media; fingerprint
change rejected; valid re-offer reuses ufrag/port; queue-full no longer forks
DTLS; admission re-validation present; m5 bucket single-charge verified; the
required suite and clippy are green; the isolated mcu test passes). That is a
large improvement over 7.0. But the commit's `no path closes a healthy peer`
guarantee still fails (P1-1), the feature is unreachable from the only real
client (P1-2), and the commit adds zero regression tests for any fix (P2-4) —
which is exactly how P1-1 slipped through. A 9.5 requires no open P1; this
commit has two.

Minimal path to 9.5+:

1. In `accept_reoffer`, classify `SdpOffer::from_sdp_string` (and other
   pre-mutation) failures as `InvalidOffer`; keep `break` only for post-mutation
   failures. Add a unit test (malformed SDP → peer alive + media flows).
2. Allow `SdpOffer` in `Negotiating` (handler + session transition) and send
   proto `IceComplete` from `tests/e2e/webrtc.js`; add an e2e re-offer step
   (and allow `SubscribeRequest`/`Unsubscribe` after negotiation).
3. Add committed regression tests: rejected re-offer keeps media; changed
   fingerprint; queue-full; m5 429/single-charge; preflight/exit 78;
   `validate_candidate` empty case reached from `main`.
4. Swap the gate/subscription stores; re-arm on re-offer; reply after the timer
   flush; fix `retryable` and the two metrics.
5. Update the stale docs listed in P2-8.

---

## 7. Limitations / notes

* The live working tree is **dirty** with concurrent agent work (uncommitted
  `vks-signaling/src/handler.rs`, `session.rs`, `vks-storage`, `vks-recorder`,
  etc.); at review time the same clippy command failed there in
  `vks-storage/src/core_adapter.rs` (`StorageError::Internal` mismatch),
  unrelated to `a33e195`. All required commands above were therefore run on the
  pristine `git archive a33e195` tree.
* The working tree already contains an uncommitted one-line
  `Negotiating` allowance in `handler.rs` + `session.rs` (not in `a33e195`);
  P1-2 is reported against the reviewed commit.
* No browser/Chromium e2e was run (not part of the required commands); the
  real-UDP harness uses two genuine str0m clients over loopback UDP, which is
  the closest available equivalent.
* The live `vks_data.db` was not touched (`mtime` unchanged
  `2026-10-29 07:33:29.801`); the preflight probes used temporary
  `/tmp/t11a_preflight*.db` files.
