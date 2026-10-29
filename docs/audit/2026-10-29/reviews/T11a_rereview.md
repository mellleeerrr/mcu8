# T11a — independent re-review (`08398b3`)

**Verdict: 8.0 / 10 — not done.** 0 P0, **2 open P1**, ~15 open P2.

Reviewed commit: `08398b3` (`fix(webrtc): T11a final review — keep session on str0m
parse rejects, reachable re-offers, regression tests`) on top of `a33e195`, which
follows the T11a feature `d9aa3ce`. Every line number below is from a pristine
`git archive 08398b3` copy at `/dev/shm/t11a_rr`; the live tree was dirty
(concurrent recorder/storage work) and was never used for evidence.

The headline fixes are real and were reproduced on the wire: the reported
malformed-SDP, extra-video, data-channel and changed-fingerprint re-offers now
return `Err` and leave the live peer registered and forwarding keyframes, and a
valid re-offer after those rejections reuses the same Rtc/socket/ICE ufrag.
Re-offers are also reachable through the signalling FSM now (`Negotiating`), and
the required SFU/signaling tests plus clippy are green. But two P1s remain:

1. the general "a re-offer str0m rejects never kills the call" guarantee is still
   false for a **pre-mutation `sdp_api().accept_offer` rejection** (e.g. changed
   m-line order): the peer is removed and forwarding stops;
2. this commit makes the required `cargo test -p vks-server --tests --locked`
   **red** — two deterministic `network_integration` failures caused by test
   edits committed without their implementation.

| Severity | Count | Items |
|---|---|---|
| **P0** | **0** | — |
| **P1** | **2** | P1-1b pre-mutation `accept_offer` rejection still closes the peer · P1-2 required server suite red |
| **P2** | **15** | grouped in §8 (gate re-arm wrong target, TrackAdded over-suppression, stale docs, …) |
| **P3** | **3** | cosmetic/nit (`is_reoffer` scope, test queue size, doc-comment mangling) |

**T11a can be marked done only after the two P1s are closed and the gate/TrackAdded
P2s have tests.** The T11a media-plane core is close; the commit is not mergeable
in its current state because CI is red.

---

## 1. Scope and method

* Read `docs/audit/2026-10-29/reviews/T11a.md` (7.0/10, 4 P1),
  `T11a_adversarial.md` (5.0/10), `T11a_final.md` (8.0/10, 2 P1), and
  `docs/audit/2026-10-29/pitfalls/T11a.md`; re-read the str0m 0.24.1
  `change/sdp.rs` / `SdpOffer` parser paths where the report depends on them.
* Built three throwaway probes in `/dev/shm` (not committed, removed at the end):
  * `/dev/shm/t11a_harness` — a **real-UDP, two-genuine-str0m-client harness**
    through the real `WebRtcRoomRegistry::accept_offer`; media (VP8 keyframes and
    deltas) flows A→SFU→B, then the re-offers are injected on the registry path;
  * a **signalling-FSM probe** driving the real `SignalingHandler` exactly like
    the repo e2e client (hello → join → offer → offer without `IceComplete`);
  * a **binary probe** running the real `vks-server` binary with a temporary
    config whose `candidate_addr` is empty.
* Spot-checked the regression tests by reverting the specific `08398b3` hunks in
  the pristine copy (backups + `md5sum -c`), then restoring them.
* Did not modify any repo source. The only file written in the repo is this
  report. The live `vks_data.db` was never opened: its mtime is still
  `2026-10-29 07:33:29.801`; the binary probe used `/dev/shm` only.

## 2. Required commands (pristine `08398b3`)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p vks-media-sfu --features str0m --locked
cargo test -p vks-signaling --locked
cargo test -p vks-server --tests --locked
cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets --locked -- -D warnings
```

| Command | Result |
|---|---|
| `cargo test -p vks-media-sfu --features str0m --locked` | **ok — 93 passed**, 0 failed |
| `cargo test -p vks-signaling --locked` | **ok — 44 passed**, 0 failed |
| `cargo test -p vks-server --tests --locked` | **FAILED — 171 executed: 166 passed, 3 ignored, 2 failed; lib 144/3 ignored, config_failclosed 1, h265_rtp 1, network_integration 20 passed / 2 failed (the run stops there; later test binaries are not run)** |
| `cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets --locked -- -D warnings` | **ok — exit 0, no warnings** |

Both server failures are deterministic and were rerun individually (both fail in isolation):

```
test_m5_recorder_lifecycle_integration ... FAILED
  assertion `left != right` failed: must return the record id
test_m5_recorder_idempotent_stop_persistent_storage_and_playable_container ... FAILED
  assertion `left != right` failed: start returns the record id
```

## 3. P1 findings

### P1-1 (as specified) — **fixed**; a sibling pre-mutation `accept_offer` path (P1-1b) is still open

**What is fixed (the task's P1-1 repros).** The previously reported repros are real fixes and were
verified end-to-end:

* `SdpOffer::from_sdp_string` failures are now `InvalidOffer`
  (`webrtc.rs:206-207`) and `run_peer` replies `Err` and `continue`s instead of
  breaking (`webrtc_room.rs:694-702`). `t=0 0` → `t=0 1` is rejected and the peer
  stays registered.
* Extra video m-line (`too many media lines`) and `m=application`
  (`unsupported media line`) are rejected by `validate_offer` before touching
  the Rtc (`webrtc.rs:434-467`); the peer stays.
* A changed DTLS fingerprint is rejected (`webrtc.rs:196-201`); the peer stays.
* A valid same-session re-offer **after** those four rejections is answered on
  the same Rtc, with the same server ICE ufrag and the same UDP candidate port;
  keyframes still reach the second client.

Real-UDP harness output (exit 0):

```
baseline keyframe forwarded to B: [(true, 8)]
malformed re-offer rejected: SDP: re-offer SDP is not valid: ... Unexpected `1` ...
after malformed rejection, keyframe forwarded to B: [(true, 8)]
extra-video re-offer rejected: SDP: too many media lines: audio=1 video=2
m=application re-offer rejected: SDP: unsupported media line: m=application 9 UDP/DTLS/SCTP webrtc-datachannel
changed-fingerprint re-offer rejected: SDP: DTLS fingerprint changed: ...
after fingerprint rejection, forwarded: [(true, 8)]
valid re-offer answer: ufrag=<same server ufrag> port=<same port> reoffered=true
after valid same-session re-offer, forwarded: [(true, 8)]
```

**P1-1b — what is still broken (same class, not in the reported repro list).** `accept_reoffer` still ends with

```rust
let answer = self.rtc.sdp_api().accept_offer(offer)?;   // webrtc.rs:208
```

and every error from that call is still fatal in `run_peer`
(`webrtc_room.rs:703-707`: reply `Err`, warn "closing peer", `break`), which
calls `remove_generation` and unregisters the live peer. Not every such error is
necessarily a post-mutation failure: the "Changed order for m-line" error is a
validation error raised against the existing remote description, before a new
answer is produced (whether str0m mutated intermediate state must be established).
The harness injected a re-offer that passes
`validate_offer` (one audio + one video, same ICE credentials and fingerprint)
but swaps the two `m=` sections:

```
swapped-m-line re-offer: rejected: SDP: str0m: remote sdp: Changed order for m-line with mid: NJ0
  peer still registered after swapped-m-line re-offer: false
  after swapped-m-line re-offer, forwarded: []
```

That is the same T11a defect class the commit claims to have removed: a
malformed re-offer from an authenticated client kills its own call (the SFU
removes the peer, the browser is told `webrtc_offer_rejected`, and no media is
forwarded). The task's explicit repros are fixed; the **general pre-mutation
rejection path is not**. This is an open P1 of the same class (call it P1-1b).

**Required fix:** either (a) compare the offered m-line order/mids against
`self.mids` in `validate_offer`/`accept_reoffer` before calling str0m and return
`InvalidOffer` for mismatches, or (b) snapshot the pre-offer remote SDP and only
treat `sdp_api().accept_offer` errors as fatal when str0m has demonstrably
mutated state; otherwise `reply.send(Err(...))` + `continue`. (b) without proof
of mutation is risky; (a) is the smallest safe fix. Add a regression test with a
swapped-m-line re-offer asserting the peer remains registered **and still
forwards**, not only that the call returns `Err`.

### P1-2 (new) — `08398b3` makes the required server test suite red

`08398b3` changes two recorder assertions from "record id == room id" to
"record id != room id":

* `crates/vks-server/tests/network_integration.rs:815`
  (`assert_ne!(rec_id, *room_id.as_uuid(), "must return the record id")`);
* `crates/vks-server/tests/network_integration.rs:1576`
  (`assert_ne!(rec_id, *room_id.as_uuid(), "start returns the record id")`).

At the same commit, `RecorderManager::start_recording` still returns the room id
on both the already-recording and the fresh path
(`crates/vks-server/src/recorder.rs:72,75`, `...:118` `Ok(room_uuid)`). The
implementation that returns a record id exists only as **uncommitted worktree
work** (`git diff` on the live tree changes `start_recording` to
`Ok(existing.record_id()...)` / `Ok(record_id)` and adds
`crates/vks-recorder/src/profile.rs` + migration `0006_video_records.sql`). The
pristine commit therefore contains tests for code it does not contain, and the
required gate fails.

This is not a WebRTC logic defect, but it is a **P1 release blocker for this
commit**: `cargo test -p vks-server --tests --locked` exits 101, so CI cannot be
green and T11a cannot be declared done. Fix by committing the recorder/storage
work together with these two assertions, or by reverting the two assertions in
`08398b3`. Add a check that the commit is self-contained (no test referring to
uncommitted API changes).

## 4. Prior `T11a_final` P1-2 (re-offers unreachable) — **fixed**

* `handler.rs:345` now allows `SdpOffer` in `SessionState::Negotiating` with an
  updated `bad_state` message.
* `session.rs:129` adds `(Negotiating, SdpOffer) -> Negotiating`.
* `rollback_offer` semantics are preserved: `on_sdp_offer` stores the current
  state in `pre_offer_state` (`handler.rs:359`), so a media-plane rejection of a
  re-offer rolls back to `Negotiating` (not to `InRoom`), and a retry is still
  accepted.

FSM probe (real `SignalingHandler`, no `IceComplete` from the client):

```
hello -> ServerHello state=Connected
join -> JoinRoomResult state=InRoom
offer#1 -> SdpAnswer state=Negotiating
offer#2 (no IceComplete) -> SdpAnswer state=Negotiating
offer#3 (no IceComplete) -> SdpAnswer state=Negotiating
after rollback_offer state=Negotiating
offer#4 after rejected re-offer -> SdpAnswer state=Negotiating
subscribe after negotiation -> Error{code=bad_state, msg=SubscribeRequest requires InRoom/MediaActive (current: Negotiating)}
ice_complete -> Ack state=MediaActive
offer#5 (MediaActive) -> SdpAnswer state=Negotiating
FSM PROBE OK: SdpOffer is reachable in Negotiating; rollback keeps Negotiating
```

The `lib.rs` path is then the same as for the first offer: a returned
`SdpAnswer` enters the `webrtc_runtime::accept_offer` branch; an existing peer
takes `PeerCommand::Reoffer` and is answered. This is a code-path argument plus
the FSM probe; there is still **no in-tree handler+registry integration test**
for two offers on one WebSocket (P2-3).

Residual, not a blocker: `SubscribeRequest`/`Unsubscribe` still require
`InRoom|MediaActive`, so a client that sends `subscribe` after its first offer
still gets `bad_state`. The repo's real clients subscribe before the offer, so
this is a P2 (and the same issue `T11a_final` noted).

## 5. New-behaviour verification

| Claim in `08398b3` | Verdict | Evidence |
|---|---|---|
| queue-full re-offer returns `Err` and keeps the old session | **met (test)** | `webrtc_room.rs:202-207`; `reoffer_on_a_full_command_queue_errors_instead_of_forking_a_new_session` passes. Note: the test helper's channel is `mpsc::channel(8)` (`webrtc_room.rs:867`), production is 64 (`:28`); the branch is identical but the production capacity is not exercised. |
| changed fingerprint rejected | **met (test)** | `webrtc.rs:196-201`; `reoffer_with_a_new_dtls_fingerprint_is_rejected` passes; the harness also rejects it over a live connected session and keyframes continue. |
| keyframe gate re-armed on re-offer | **NOT met — wrong peer** | `webrtc_room.rs:225-228` arms `handle.waiting_keyframe`, and `handle` is the **re-offering publisher's** handle. `waiting_keyframe` gates media sent **to** that peer (`run_peer` `Write` arm), not media it publishes. Subscribers' gates are untouched. Harness: after A re-offers, a delta B→A is dropped while a delta A→B is still delivered. See §7. |
| `TrackAdded` not duplicated on re-offer | **partially met — over-suppressed** | `lib.rs:1085` filters **all** tracks when `is_reoffer`, without diffing the track set. Duplicates are gone, but a re-offer that genuinely adds a track (audio-only peer starts a video m-line; accepted by `validate_offer`/str0m) announces nothing; only `add_publisher` changes. See §7. |
| `TrackInfo` audio 48 kHz/2 ch, video 90 kHz/0 | **met in code, untested** | `lib.rs:1106-1119`; audio `48_000/2`, everything else `90_000/0`. No test. |
| `retryable=true` for `webrtc_room_changed` | **met in code, untested** | `lib.rs:1003-1004` adds `"webrtc_room_changed"`. No test; the queue-full error is still `webrtc_offer_rejected`/`retryable=false` despite "retry" in the message. |
| early empty-candidate exit 78 **before** DB creation | **met, probed** | `main.rs:94-103` runs before `_db_lock` (`:108`) and storage. Binary probe with `candidate_addr = ""`: `EXIT=78`, log `VKS_WEBRTC_TRANSPORT is enabled but candidate_addr is empty ...`, and `/dev/shm/.../probe.db` was **not created**. |
| H264-only video does not produce a lying `TrackAdded` | **improved, but still accepted silently** | `lib.rs:1024-1053` filters tracks the answer did not negotiate, so the old "audio Opus + video VP8 TrackAdded" lie is gone. But an H264-video + Opus-audio offer still passes `validate_offer` (the codec check only requires *either* VP8 or Opus, `webrtc.rs:461-467`), gets an answer with a rejected video m-line, and the client gets a black video tile with no error. P2. |

## 6. Regression tests: real, spot-checked by reverting hunks

All three checks were done in the pristine copy by reverting the exact `08398b3`
hunk, running the new test, then restoring from a backup (`md5sum -c` verified).

| Test | Reverted hunk | Result on reverted tree | Result on restored tree |
|---|---|---|---|
| `webrtc::tests::malformed_reoffer_is_rejected_without_touching_the_live_session` | parse errors mapped back to `WebRtcError::Sdp` | **FAILED**: `expected a rejected re-offer, got Err(Sdp("SDP parse: ... Unexpected 1"))` | **ok** |
| `webrtc_room::tests::reoffer_on_a_full_command_queue_errors_instead_of_forking_a_new_session` | `TrySendError::Full` arm falls through to a fresh offer | **FAILED**: `invalid offer: missing ICE credentials` (the fresh-offer fallback, not "queue is full") | **ok** |
| `session::tests::negotiation_and_media_active` | removed `(Negotiating, SdpOffer)` | **FAILED**: `InvalidTransition { from: Negotiating, event: SdpOffer }` | **ok** |

`reoffer_with_a_new_dtls_fingerprint_is_rejected` is also green, but it is not a
regression test for *this* commit: the fingerprint rejection already exists in
`a33e195`. The three tests in the table are genuine regression guards. The queue-full
test's test-local capacity (8) is a P3 coverage nit, not a tautology: it proves
the `Full` branch, and on `a33e195` it fails because the fallback returns a
different error.

## 7. New defects introduced / newly exposed

1. **Gate re-arm applies to the wrong peer (P2).** `webrtc_room.rs:225-228`
   arms the re-offering participant's `waiting_keyframe`, which gates its
   **inbound** video. Harness evidence:

   ```
   B->A keyframe before re-offer: [(true, 8)]
   B->A delta with A's gate open: [(false, 8)]
   B->A delta right after A re-offered: []  (empty => A's own inbound gate was armed)
   B->A keyframe after re-offer: [(true, 8)]
   ```

   So a normal valid re-offer by A needlessly blanks A's incoming video until
   the next IDR, while B (A's subscriber) is **not** re-armed. The comment says
   "the new SSRC's first frames are not deltas", but the gate that decides
   forwarding to a subscriber lives on that subscriber's handle. Fix: on a
   re-offer that publishes video, arm the gates of the **other** peers in the
   room that subscribe to video (and/or request a keyframe), not the
   re-offering peer; cover it with a real UDP test.

2. **`TrackAdded` filter hides genuinely new tracks (P2).** `lib.rs:1085`
   suppresses the whole announcement loop for any re-offer. An audio-only peer
   that later adds a video m-line is accepted and registered
   (`validate_offer` allows 0→1 video; str0m accepts an appended m-line), but
   subscribers receive no `TrackAdded` and no `TrackRemoved`/`TrackUpdated`.
   Fix: diff the track set (announce only added/removed), not `!is_reoffer`.

3. **`is_reoffer` scope is fragile (P3).** `lib.rs:622` declares it once outside
   the per-message loop and `lib.rs:935` only assigns it on a successful real
   accept. It is not reset at the start of each offer; today the transport flag
   is process-constant so no exploitable sequence was found, but the next
   change to the fallback/legacy path can make a legacy answer inherit
   `is_reoffer=true` and silently skip all `TrackAdded`. Reset it to `false` at
   the top of the offer handling (next to `real_webrtc_peer`).

4. **`validate_offer` codec check is a substring heuristic (P2).** It accepts
   an offer if *either* `vp8/90000` or `opus/48000` appears anywhere
   (`webrtc.rs:461-467`), so H264-video + Opus-audio is accepted with a black
   video m-line (see §5). Conversely it rejects a legitimate offer that spells
   the codec with unusual (but legal) whitespace/parameters. Prefer parsing the
   `a=rtpmap` lines and checking per m-line; fail closed with a clear error.

5. **Empty-candidate early exit exists only in `main` (P2).** The new check
   (`main.rs:94`) protects the binary, but `build_app`/embedders still get a
   `None` registry and per-offer `webrtc_unavailable` instead of a startup
   failure (the old adversarial P2-4). Also the check duplicates the preflight
   predicate; a single `Config`-level validation would avoid drift.

6. **`accepted_video` can drop a video track whose m-line is negotiated as
   recvonly/sendonly in a re-offer without sending `TrackRemoved` (P2).** The
   `add_publisher` call replaces the track list, but the roster broadcast is
   still suppressed by `is_reoffer`; clients keep a stale tile.

None of these is a P0; none of the normal browser paths (same PC re-offer,
screen-share addTrack rejected, data-channel rejected, fingerprint change) kills
media. The swapped-m-line repro in §3 is a malformed client input, but it hits
the exact "re-offer rejection must not kill the live call" guarantee.

## 8. Remaining P2s and release-blocker decision

No **P2** by itself is a T11a release blocker. The blockers are the two P1s.
Status of the open P2s (prior IDs where applicable):

| # | P2 | Status / note |
|---|---|---|
| 1 | `SetSubscription` store-order window (`webrtc_room.rs:435-438`) | open; `video.store` precedes `waiting_keyframe.store`, a queued `Write` can slip between |
| 2 | Re-offer replies before its own timer flush (`webrtc_room.rs:672-680`) | open; unchanged from prior P2-3 |
| 3 | Test gaps | reduced (3 real tests) but still no test for gate re-arm, `TrackAdded`, `retryable`, early `exit 78`, `TrackInfo`, or a two-offer WS flow; queue-full test uses capacity 8 vs prod 64 |
| 4 | Process-global env mutation in tests | open (`network_integration.rs:119-123`, `webrtc_runtime.rs:208`) |
| 5 | `TrackInfo`/`MEDIA_BYTES` hardcoded | partial: audio clock/channels fixed; `codec` still hardcoded (`lib.rs:1091-1093`), `MEDIA_BYTES` still `"vp8"/"opus"` (`webrtc_room.rs:565-567`) |
| 6 | Preflight ordering / derived candidate | partial: empty candidate now exits before DB, but TEST-NET/not-assignable candidates still create the DB before `preflight` (`main.rs` order) |
| 7 | Stale docs | open: `webrtc_room.rs:4-6` still says "calls accept_offer synchronously"; `webrtc_runtime.rs:4-7` still says fresh registration is synchronous under the admission lock; `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:51,129` still claims atomic replacement/admission serialization; `deploy/systemd/vks-mcu-prod.service:19-21` still says "Opt-in" with the transport commented out |
| 8 | Nits | open: duplicated strip-trickle/mids block, double `handle_timeout` in the re-offer arm + common path |
| 9 | `retryable` for queue-full/`webrtc_unavailable` | partial: `webrtc_room_changed` fixed; queue-full still `webrtc_offer_rejected`/false |
| 10 | Metrics misuse | partial: `WEBRTC_FRAMES_DROPPED{reoffer_rejected}` removed; `WEBRTC_FALLBACK{offer_rejected}` still incremented for a live peer's rejected re-offer (`lib.rs:1006-1008`) |
| 11 | `TrackAdded` over-suppression on re-offer | new, see §7.2 |
| 12 | Gate re-arm wrong peer | new, see §7.1 |
| 13 | H264-only video silently accepted | residual from adversarial P1-4, see §7.4 |
| 14 | Early candidate/preflight only in `main` | residual adversarial P2-4 |
| 15 | Re-offer track removal has no `TrackRemoved` | new, see §7.6 |

P2-7 (docs/ops) and P2-6 (preflight ordering) should be fixed before a prod
rollout, but they do not block T11a's media-plane acceptance. P2-1/P2-3/P2-8 were
already deferred by the earlier reviews.

## 9. Counts, score, and path to done

* **P0: 0.**
* **P1: 2** — P1-1b (pre-mutation `accept_offer` rejection still closes the peer;
  the task's reported P1-1 repros are fixed) and P1-2 (new; required
  `cargo test -p vks-server --tests --locked` red).
* **P2: 15** (grouped above).
* **P3: 3.**

**Score: 8.0 / 10.** The commit genuinely fixes the reported P1-1 repros, fixes
the P1-2 unreachability end-to-end at the FSM, adds three real regression tests
(proved by reverting the hunks), and closes several P2s (queue-full, retryable
`webrtc_room_changed`, audio clock, early exit 78, one bogus metric). It stays at
8.0 because it still allows a rejected re-offer to kill the live call on another
pre-mutation path, and because it ships a red required test suite; the new
gate-arm and `TrackAdded` implementations are also wrong-target/over-broad and
untested. A 9.5 requires no open P1 — this commit has two.

**T11a can be marked done? No.** Before that:

1. route `sdp_api().accept_offer` rejections through `InvalidOffer` (or
   pre-validate m-line order/mids) and add the swapped-m-line live-media test;
2. make `cargo test -p vks-server --tests --locked` green by committing the
   recorder implementation together with its two assertions (or reverting them);
3. arm the **subscribers'** gates on a re-offer and add a real UDP gate test;
4. announce only genuinely added/removed tracks on re-offer;
5. fix the remaining stale docs and add tests for `retryable`, early `exit 78`,
   and `TrackInfo`.

## 10. Limitations

* No browser/Chromium e2e was run. The real-UDP harness uses two genuine str0m
  clients over loopback UDP, which covers ICE-lite/DTLS/SRTP and the registry,
  but not Chrome's exact re-offer shape.
* The swapped-m-line offer in §3 is malformed per RFC 8829 (m-line order must not
  change); a conforming browser will not send it. It is nevertheless an
  authenticated-client re-offer that the SFU rejects and that currently kills
  the call, i.e. the same failure class the commit claims to remove. It should
  be rejected gracefully, not necessarily accepted.
* The server full suite was run once (2 deterministic failures, both reproduced
  in isolation). The live worktree is dirty with the recorder/storage work; the
  failures are absent there only because that work is uncommitted, not because
  `08398b3` is green.
* `vks_data.db` was not touched (`mtime 2026-10-29 07:33:29.801`); the binary
  probe used `/dev/shm` and confirmed no DB file was created on the exit-78 path.
