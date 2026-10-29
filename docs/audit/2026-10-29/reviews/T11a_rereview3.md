# T11a — independent re-review #3 (`661a2cb`)

**Verdict: 9.0 / 10 — done (0 P0, 0 open P1).** The reported P1 residual is
fixed with live evidence: a BUNDLE-less re-offer and an echoed `a=ice-lite`
re-offer now return `InvalidOffer`, the live peer stays registered and keeps
forwarding media, and a valid re-offer after them reuses the same `Rtc`, UDP
socket and ICE ufrag. The wrong-peer keyframe gate re-arm block is gone from the
diff and from the code, and live media shows no blanking. Appended m-lines are
accepted through the real registry, reordered/renamed mids are rejected, the
early preflight is real (TEST-NET and empty candidates exit 78 without creating
the SQLite file), and all four required gates are green on a pristine copy.
11 P2 and 5 P3 remain, mostly test gaps and carried items; one new P2 is real
(a rejected re-offer can still mutate str0m's ICE state), which is why this is
not 9.5.

Reviewed commit: `661a2cb` (`fix(layout): T06 review pass …`) two commits above
the T11a tip `4d7b2df` (`fix(webrtc): T11a re-review #2 — str0m pre-mutation
rejects keep the call, appended m-lines, early preflight for real`). There is no
WebRTC diff between `4d7b2df` and `661a2cb`
(`git diff --stat 4d7b2df 661a2cb -- crates/vks-media-sfu crates/vks-server`
is empty), so the code under review is exactly the T11a tip. Evidence comes from
a pristine `git archive 661a2cb` copy at `/dev/shm/t11a_rr3` (and harness /
revert copies at `/dev/shm/t11a_rr3_h`, `/dev/shm/t11a_revert3`, all removed
afterwards). The live tree was not used for evidence, the live `vks_data.db` was
never opened, and its fingerprint is unchanged before/after this review: md5
`6a23442016492077f311becf14badace`, mtime `2026-10-29 07:33:29.801017417 +0300`,
3 248 128 bytes.

| Severity | Count | Items |
|---|---|---|
| **P0** | **0** | — |
| **P1** | **0** | P1-1b residual closed (real-UDP two-client evidence) |
| **P2** | **11** | TrackAdded e2e test gap · `TrackInfo` ssrc=0 · rejected re-offer mutates ICE state + doc overclaim · gate fix untested · early-preflight untested (dev configs skip it) · queue-full `retryable` untested · `WEBRTC_FALLBACK` on rejected re-offers · `SetSubscription` store-order window · reply-before-timer-flush · H264-only publisher · no `TrackRemoved` |
| **P3** | **5** | mid guard anchored to the initial offer · no `a=group:BUNDLE` tag/setup validation · `InvalidOffer`→`Sdp` downgrade + stale log · duplicated strip-trickle / double `handle_timeout` · process-global env in server tests + candidate check only in `main` |

**T11a can be marked done? Yes.** 0 P0 / 0 open P1, and every item in this
review's checklist was verified at `661a2cb` with running code, not by reading
commit messages. The remaining P2/P3 are follow-ups, not blockers for T11a.

---

## 1. Scope and method

* Read `T11a_rereview2.md` (8.5/10, 1 open P1), `T11a_rereview.md`,
  `T11a_final.md`, `T11a_adversarial.md`, `T11a.md`, and the T11a diffs
  `ddd033b..4d7b2df` (including `4d7b2df` in full). Re-read the str0m 0.24.1
  `change/sdp.rs` `accept_offer`/`apply_offer` paths used below.
* Built a **real-UDP, two-genuine-str0m-client harness**
  (`crates/vks-media-sfu/tests/t11a_rr3_harness.rs`, added only to a throw-away
  `/dev/shm` copy, never committed) that drives the real
  `WebRtcRoomRegistry::accept_offer` path: two independent `str0m::Rtc` clients
  bind loopback UDP sockets, complete ICE/DTLS/SRTP against the registry's peer
  tasks, exchange VP8 keyframes/deltas A→SFU→B and B→SFU→A, then inject
  malformed-but-parseable re-offers on the same registry path.
* Ran the real `vks-server` binary built from the pristine copy with four
  `/dev/shm` configs (empty candidate / TEST-NET candidate / dev loopback /
  dev loopback + TEST-NET), checking exit codes, logs, and whether the SQLite
  file and its `.lock` appear.
* Proved the new regression tests by reversing the exact hunks in
  `/dev/shm/t11a_revert3` (copy of `git archive 661a2cb`), and proved the two
  non-tests by re-introducing the removed/blocked behaviour.
* Ran the four required gates on the pristine copy (shared cargo target dir,
  `--locked`): see §8.

### Evidence log (real registry, real UDP)

```
HANDSHAKE: two str0m clients connected over loopback UDP; A answer ufrag=hveO8dsPgzJL4KMI port=43757
BASELINE: B got 1 video keyframes from A; A got 1 from B
MISSING a=group:BUNDLE -> SDP: re-offer rejected by str0m: SDP inconsistent: Session attribute a=group missing
  after missing-BUNDLE: A->B keyframes=1 peer_count=2
ECHOED a=ice-lite -> SDP: re-offer rejected by str0m: remote sdp: Both peers being ICE-Lite not supported
  after ice-lite echo: A->B keyframes=1 peer_count=2
VALID RE-OFFER -> reoffered=true ufrag_same=true port_same=true
  after valid re-offer: A->B keyframes=1
B->A DELTA RIGHT AFTER A's VALID RE-OFFER -> received=1 (gate not re-armed)
changed a=group:BUNDLE tags -> ACCEPTED reoffered=true mids=2 answer_group=Some("a=group:BUNDLE i2s Qol") answer_ufrag_same=true vp8_in_rtp_map=true
  changed a=group:BUNDLE tags: A->B keyframes after probe=1 peer_count=2
renamed a=mid -> REJECTED SDP: m-line/mid order changed: re-offers may only append new sections
  renamed a=mid: A->B keyframes after probe=1 peer_count=2
changed a=setup:actpass->passive -> ACCEPTED reoffered=true mids=2 …
changed Opus PT 111->120 -> ACCEPTED reoffered=true mids=2 …
MISSING BUNDLE + NEW ICE ufrag/pwd -> SDP: re-offer rejected by str0m: SDP inconsistent: Session attribute a=group missing
  after that reject: A->B keyframes=1 peer_count=2
  next VALID re-offer: ufrag_same_as_first=false (first=hveO8dsPgzJL4KMI now=37a7xpCty4EUMmua) => rejected offer changed str0m ICE state
  after that valid re-offer: A->B keyframes=1
FINAL: has_peer(A)=true peer_count=2
```

```
AUDIO-ONLY HANDSHAKE: A answer mids=1
AUDIO A->B before append: frames=1
APPENDED OFFER: group=Some("a=group:BUNDLE vKT aJ6")
APPENDED VIDEO M-LINE -> accepted reoffered=true answer_mids=2 rtp_maps_vp8=true rtp_maps=[(111, "opus", 48000, 2), (96, "vp8", 90000, 0)]
AUDIO A->B after append: frames=1
```

---

## 2. Item 1 — pre-mutation re-offers keep the call (P1 closed)

### 2.1 Code at `661a2cb`

`WebRtcPeer::accept_reoffer` (`webrtc.rs:199-232`) validates first
(`validate_offer`, prefix-only mid guard at `:204-210`, fingerprint at
`:211-217`, SDP parse at `:220-222`) and then maps **every** str0m
`accept_offer` error to `InvalidOffer`:

```rust
let answer = self
    .rtc
    .sdp_api()
    .accept_offer(offer)
    .map_err(|error| WebRtcError::InvalidOffer(format!("re-offer rejected by str0m: {error}")))?;  // :227-231
```

The registry's re-offer path returns `Err(WebRtcError::Sdp(...))` to the
signalling handler on rejection and **does not** touch the room map
(`webrtc_room.rs:234-238`); `run_peer`'s `Reoffer` arm only closes the peer on a
*different* error class (`webrtc_room.rs:692-705`), so an `InvalidOffer` no
longer reaches the `break`/`remove_generation` path.

### 2.2 Live evidence

For the two named repros, through the real registry with two connected str0m
clients (A publisher, B subscriber, 2 peers in one room):

| Probe | Result | Peer registered | Media still forwarded |
|---|---|---|---|
| missing `a=group:BUNDLE` | `InvalidOffer` ("Session attribute a=group missing") | `has_peer(A)=true`, `peer_count=2` | next A→B keyframe delivered |
| echoed `a=ice-lite` | `InvalidOffer` ("Both peers being ICE-Lite not supported") | `has_peer(A)=true`, `peer_count=2` | next A→B keyframe delivered |
| valid re-offer afterwards | `Ok`, `reoffered=true` | same handle | next A→B keyframe delivered |
| same ICE ufrag / socket | `ufrag_same=true`, `port_same=true` | — | — |

The ufrag/port comparison is the observable proxy for "same `Rtc`/socket": a
fresh `accept_fresh_offer` would bind a new ephemeral port and generate a new
ICE ufrag. The harness saw the same `a=ice-ufrag` and the same candidate port in
the first answer and in the post-rejection valid re-offer, and `reoffered=true`.

A second, independent proof that the session object survived: B→A media. A's
DTLS/SRTP keys were untouched by the rejections and A's inbound stream still
decrypts and depayloads (see §3), which cannot happen if the `Rtc` had been
dropped.

### 2.3 The other malformed-but-parseable re-offers

Three extra shapes were injected on the same live peer, plus a renamed mid:

| Probe | Server reaction | Call killed? |
|---|---|---|
| `a=group:BUNDLE` tags changed to `tag<mid>` (tags no longer match `a=mid`) | **ACCEPTED** (`reoffered=true`, answer regenerates the original group `a=group:BUNDLE i2s Qol`, `ufrag_same=true`) | No — A→B keyframes continue, `peer_count=2` |
| `a=mid` renamed (and group tags renamed with it) | **REJECTED** `InvalidOffer("m-line/mid order changed: re-offers may only append new sections")` | No — keyframes continue, `peer_count=2` |
| `a=setup:actpass` → `a=setup:passive` | **ACCEPTED** (`reoffered=true`; `init_dtls` is a no-op once DTLS is inited, so the server silently ignores the role change) | No — keyframes continue |
| Opus payload type 111 → 120 (valid SDP) | **ACCEPTED** (`reoffered=true`, `answer_ufrag_same=true`) | No — video keyframes continue (audio PT changes are applied; the harness client keeps sending the old PT, so audio is an artifact of the crafted SDP, not a server defect) |

None of them kills the call, so there is **no second P1** in this class. Two
remarks belong to the P2/P3 list: (a) a changed BUNDLE group is not validated at
all — `validate_offer` only counts m-lines and the str0m 0.24.1
`assert_consistency` comparison of group tags vs `a=mid` does not fire in
practice (verified on a bare `WebRtcPeer`: even `a=group:BUNDLE bogus` is
accepted and the answer is regenerated from the session), so the "reject what we
cannot negotiate" guarantee does not cover group tags; (b) `a=setup` changes are
accepted silently.

### 2.4 New P2: a rejected re-offer can still mutate str0m ICE state

The commit message claims str0m "validates against the existing remote
description before mutating" and the module/implementation docs claim a rejected
re-offer is handled "without touching the live session". That is **not fully
true**. In str0m 0.24.1 `accept_offer` runs

```rust
add_ice_details(self.rtc, &offer, None)?;   // change/sdp.rs:84 — may perform a remote ICE restart
…
apply_offer(&mut self.rtc.session, offer)?;  // :112 — its first check is assert_consistency() at :947
```

so a re-offer that carries a **new** `a=ice-ufrag`/`a=ice-pwd` *and* a defect
that `apply_offer` rejects (e.g. missing `a=group:BUNDLE`) performs the ICE
restart first, then fails. Live evidence: after that rejected re-offer the peer
is still registered and media still flows, but the next valid re-offer produces
a **different** server ICE ufrag (`hveO8dsPgzJL4KMI` → `37a7xpCty4EUMmua`; for a
same-credentials rejection the ufrag stays identical, §2.2). So:

* the call is not torn down (no P1 by the task's criterion: peer stays
  registered, media keeps being forwarded in the observed window);
* but the "rejection is side-effect free" claim is false, the live ICE agent was
  restarted with new local credentials that the client never received, and the
  next re-offer rotates the ufrag. Likelihood is low (a browser never sends a
  malformed BUNDLE group, and an ICE restart inside a syntactically broken offer
  is pathological), which is why this is **P2**, not P1;
* the fix is either to snapshot/restore the ICE credentials around the str0m
  call, or pre-validate the group/consistency conditions ourselves before
  handing the SDP to str0m (which also closes the P3 group-tag gap).

---

## 3. Item 2 — wrong-peer keyframe gate re-arm: **gone**

`git show 4d7b2df -- crates/vks-media-sfu/src/webrtc_room.rs` deletes exactly
the block the previous commit only *claimed* to delete:

```rust
-                    // A re-offer that starts publishing video needs a fresh IDR: re-arm
-                    // the gate so the new SSRC's first frames are not deltas.
-                    if reoffer.publishes_video {
-                        handle.waiting_keyframe.store(true, Ordering::Relaxed);
-                    }
```

`grep waiting_keyframe crates/vks-media-sfu/src/webrtc_room.rs` at `661a2cb`
leaves only the two semantically correct arm sites:

* `:426-438` `set_subscription`: a subscriber that turns video on is gated until
  the next IDR (and the arm is synchronous, before the queued `Write`s);
* `:548-562` fanout queue-full: the *target* subscriber whose frames were
  dropped waits for a fresh keyframe (and the publisher is asked for one).

No arm site remains on the re-offering publisher's own handle.

### Live evidence (real UDP, two clients)

```
B->A DELTA RIGHT AFTER A's VALID RE-OFFER -> received=1 (gate not re-armed)
```

B sends a **delta only** immediately after A's valid re-offer; A receives it.
Before the fix this delta was dropped and A stayed gated until B's next IDR, so
the wrong-peer blanking is gone. Subscribers are not left gated either: B's gate
opens on A's first keyframe (baseline `B got 1 video keyframes from A`), and A→B
keyframes keep arriving after every rejection/re-offer. No new blanking path was
introduced by the appended-m-line change: the gate is per-peer (not per-mid) and
opens on the first keyframe of the newly appended stream.

**But the fix has no regression test**: re-adding the removed block to a copy of
`661a2cb` leaves the whole SFU suite green (97/97, §8.3). That is P2-4 below, not
a correctness doubt about the fix itself.

---

## 4. Item 3 — appended m-lines

### 4.1 Code and committed tests

`webrtc.rs:202-210` now requires the new mid list to keep the previous list as a
prefix (order-preserving superset) instead of exact equality:

```rust
let keeps_prefix =
    next_mids.len() >= self.remote_mids.len() && next_mids[..self.remote_mids.len()] == self.remote_mids[..];
if !next_mids.is_empty() && !keeps_prefix { … InvalidOffer … }
```

Two committed tests cover the new behaviour and are genuine (§8.1/§8.2):

* `audio_only_peer_can_append_a_video_section_on_reoffer` (`webrtc.rs:627`):
  an audio-only offer is accepted, a re-offer with the appended video section is
  accepted, `peer.mids().len() == 2`. Reverting the guard to exact equality
  makes it **fail**.
* `reoffer_rejected_by_str0m_keeps_the_session` (`webrtc.rs:640`): the
  BUNDLE-less re-offer yields `WebRtcError::InvalidOffer`, the answer SDP is
  byte-identical afterwards, and a valid re-offer still succeeds. Reverting the
  `map_err` makes it **fail** with the pre-fix fatal error
  `Err(Rtc(Sdp(Inconsistent("Session attribute a=group missing"))))`.

Reordered/renamed mids are rejected by the same guard; `renamed a=mid` was also
verified through the real registry in §2.3 (`InvalidOffer`, peer alive, media
continues). The committed swapped-m-line test
(`swapped_m_line_reoffer_is_rejected_and_keeps_the_session`) exercises the same
prefix predicate and is proven by the earlier reverts; the new prefix guard
treats a swap as a non-prefix.

### 4.2 Real-registry evidence

With A joined **audio-only** and B joined audio+video, and an appended-video
re-offer built from A's own offer (same ufrag/fingerprint/mid space) plus a
video section whose mid is added to the BUNDLE group:

```
APPENDED VIDEO M-LINE -> accepted reoffered=true answer_mids=2 rtp_maps_vp8=true rtp_maps=[(111, "opus", 48000, 2), (96, "vp8", 90000, 0)]
AUDIO A->B after append: frames=1
```

So the full registry path (not just the bare `WebRtcPeer`) accepts the appended
section, returns a two-mid answer, and keeps the existing audio flowing.

### 4.3 Residual risk in `rtp_maps` / announcement for the appended track

* `rtp_maps` is a **flat** answer-derived list (`peer.rtp_maps()`), not keyed by
  mid. For the normal audio+video case it is unambiguous (Opus PT 111, VP8 PT
  96), and the harness confirms VP8 is present after the append — which is what
  `lib.rs`'s `accepted_video` check consumes. It cannot represent two sections
  of the same kind, but `validate_offer` caps the peer at one audio + one video
  m-line, so that is unreachable today.
* `TrackInfo` in the announcement is hardcoded (`ssrc: 0`, fixed 48000/2 or
  90000/0, `lib.rs:1115-1140`), so a client cannot correlate the announced
  appended track with the browser's actual SSRC. Carried P2-2.
* The guard is anchored to the **initial** offer's mids: `remote_mids` is set
  only in `accept_offer` (`webrtc.rs:185`) and never updated by a successful
  re-offer. After an appended section is accepted, a later rename/reorder of
  that appended section passes our guard (the prefix still matches) and is only
  caught by str0m's own `Changed order` error, still mapped to non-fatal
  `InvalidOffer`. No kill, but the guard is weaker than its comment implies
  (P3-1).
* `mid_audio`/`mids` are rebuilt from the answer on every successful re-offer
  (`webrtc.rs:234-247`), and `publishes_video` is updated in the registry
  (`webrtc_room.rs:216-226`), so egress mapping for the appended mid is correct.

---

## 5. Item 4 — early preflight is real

`main.rs:92-107` now calls `webrtc_runtime::preflight_candidate(configured,
false)` **before** the DB lock (`:112`) and `SqliteStorage::init` (`:133`), and
exits 78 on error; it is gated on `enabled() && !allow_insecure_defaults`.

### Binary evidence (real `vks-server`, pristine build)

Configs were `/dev/shm/rr3_probe/{empty,testnet,dev2,devtestnet}.toml` with
`database_url = sqlite:///dev/shm/rr3_probe/<name>.db?mode=rwc`.

| Case | Exit | SQLite file | Log |
|---|---|---|---|
| `candidate_addr = ""` (prod) | **78** | **absent** (`ls: cannot access … empty.db: No such file or directory`); no `.lock` either | `configuration rejected; aborting startup error=VKS_WEBRTC_TRANSPORT is enabled but candidate_addr is empty …` |
| `candidate_addr = "192.0.2.10"` TEST-NET (prod) | **78** | **absent** (`testnet.db` never created; no `.lock`) | `WebRTC preflight failed: candidate_addr 192.0.2.10 is a documentation (TEST-NET) address; set the real reachable IP` |
| dev loopback `127.0.0.1`, `allow_insecure_defaults=true`, fresh DB + strong admin password | **124** (killed by `timeout`) | created at 11:44 in `/dev/shm` | `WebRTC preflight OK candidate=127.0.0.1 ports="ephemeral"` then `vks-server ready and listening for HTTP/WebSocket addr=127.0.0.1:0` |

Timestamps/state: the probe directory contains only the four configs and their
logs; `empty.db`/`testnet.db` do not exist after the run (no file, no `.lock`).
The earlier "TEST-NET creates the DB before exit 78" defect is therefore closed
for production configs. The dev loopback config still starts, as required.

**Residual (P2-5):** because of the `!config.allow_insecure_defaults` condition,
the early check is skipped for dev configs: a dev config with TEST-NET candidate
still creates the SQLite file (verified: 167 936 bytes at 11:48) and exits 78
later at the **late** preflight (`main.rs:426`). Also, no automated test covers
the "exit 78 without creating the DB" ordering; it was verified manually here.

---

## 6. Item 5 — `TrackAdded`

Code inspection at `661a2cb` (`lib.rs:1083-1142`):

```rust
let previous_kinds = ctx.sfu_engine.publisher_track_kinds(m.room_id, pid);   // :1083-1084
let _ = ctx.sfu_engine.add_publisher(…).await;
…
for track in tracks.iter().filter(|track| !previous_kinds.contains(&track.media_kind)) { … }  // :1099-1102
```

`tracks` is built from the **offer's** media lines, filtered by
`accepted_audio`/`accepted_video` taken from the re-offer's answer
(`lib.rs:1040-1074`), so:

* a **genuinely new** kind is announced: after the accepted appended-video
  re-offer `tracks = [Audio, Video]`, `previous_kinds = [Audio]`, the filter
  emits exactly one `TrackAdded(Video)`. The inputs are verified at the registry
  level by the harness: the appended re-offer returns an answer with
  `accepted_video = true` (`rtp_maps` contains VP8) and `sdp.media` has two
  sections;
* a **same-kind** re-offer announces nothing: `tracks` ⊆ `previous_kinds`, so
  the filter is empty — no duplicate `TrackAdded`;
* the helper `SfuEngine::publisher_track_kinds` (`engine.rs:344`) is correct and
  unit-tested (`engine.rs:635`, real: stubbing it to `vec![]` fails the test,
  proven in the previous review).

**Not tested end-to-end (P2-1).** Reverting the filter to `filter(|_| true)`
(announce on every re-offer, i.e. reintroducing duplicates) leaves the **entire
server suite green — 176 passed / 0 failed** (§8.3). The only committed
`TrackAdded` test (`network_integration.rs:1003-1006, 1226-1229`) drives the
*legacy plaintext* path, not a real-WebRTC re-offer.

**Exact difficulty of a proper test (assessed, not done):** moderate, ~1-2 h.
A new integration-test binary is needed because `network_integration.rs:121`
sets `VKS_WEBRTC_TRANSPORT=0` process-wide and its offers carry no ICE/DTLS
material. The new test has to: (a) run the server with
`VKS_ALLOW_LOOPBACK_MEDIA=1` + `candidate_addr=127.0.0.1` (or
`allow_insecure_defaults`); (b) join two WS clients (reuse the existing
`tokio-tungstenite` helpers in `network_integration.rs`); (c) have client A send
a **valid audio-only** SDP offer with `a=ice-ufrag/pwd/fingerprint` (the peer
never has to connect — the announcement only needs a successful negotiation);
(d) send a second `SdpOffer` on the same socket with the video m-line appended
and the same ICE credentials/fingerprint; (e) assert exactly one
`TrackAdded{kind: Video}` is broadcast to B and that a third same-kind re-offer
emits none. No UDP client is required, which is why this is a test-writing task,
not a harness task.

---

## 7. Item 6 — queue-full `retryable`, docs

### 7.1 Queue-full → `webrtc_offer_retry`

Correct in code and unchanged by this commit:

* `webrtc_room.rs:206-211`: a full per-peer command queue returns
  `WebRtcError::Sdp("re-offer queue is full; retry")` and does **not** fork a
  second DTLS session; the registration survives
  (unit test `reoffer_on_a_full_command_queue_errors_instead_of_forking_a_new_session`,
  `webrtc_room.rs:1030-1042`, with a test-channel capacity of 8 vs the
  production 64);
* `lib.rs:974-984`: any `Sdp` message containing `"retry"` maps to
  `webrtc_offer_retry`;
* `lib.rs:1010-1014`: `webrtc_offer_retry` is in the retryable set
  (`room_full | webrtc_capacity | webrtc_room_changed | webrtc_offer_retry`);
* the capacity path (`webrtc_capacity`, retryable) also matches the docs.

Still a **test gap (P2-6)**: no Rust or e2e test asserts
`code == webrtc_offer_retry` **and** `retryable == 1`; `tests/e2e/webrtc.js`
only checks `room_full`. The `contains("retry")` heuristic is brittle (P3-3
territory): any future SDP error text containing "retry" becomes client-
retryable, and `InvalidOffer` is downgraded to `WebRtcError::Sdp` at the registry
boundary (`webrtc_room.rs:237`), so the signalling layer cannot distinguish a
policy rejection from a transport SDP error except by string matching.

### 7.2 Docs: remaining overclaims

The implementation doc was not updated by `4d7b2df` (the commit only touched
`webrtc.rs`, `webrtc_room.rs`, `main.rs`, and the prior report). Remaining
overclaims at `661a2cb`:

* `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:52`: "несовместимый
  re-offer отклоняется, не разрывая живой звонок" — the "not tearing the call
  down" part is now true in every probe, but "incompatible re-offers are
  rejected" is not: a changed `a=setup` role, a changed payload type and a
  changed BUNDLE group are **accepted** (§2.3);
* `webrtc_room.rs:8-10`: "A re-offer the peer cannot negotiate is rejected
  without touching the live session" — false for the ICE-restart-in-broken-SDP
  case (§2.4): the peer survives, but the live session's ICE credentials are
  rotated;
* the commit message's own "str0m validates against the existing remote
  description before mutating" is false for `add_ice_details` (same evidence);
* no doc mentions `webrtc_offer_retry` at all (under-documentation, not an
  overclaim).

---

## 8. Item 7 — gates and regression-test reality

### 8.1 Required gates (pristine `661a2cb`, `--locked`)

| Gate | Result |
|---|---|
| `cargo test -p vks-server --tests` | **exit 0** — 144 lib + 1 `config_failclosed` + 1 `h265_rtp` + 23 `network_integration` + 7 `operator_policy` = **176 passed, 0 failed, 3 ignored** |
| `cargo test -p vks-media-sfu --features str0m` | **exit 0** — **97 passed, 0 failed** (+2 tests added by this commit) |
| `cargo test -p vks-signaling` | **exit 0** — **44 passed, 0 failed** |
| `cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets -- -D warnings` | **exit 0**, no warnings |

### 8.2 Reverts that make a test fail (real regression tests)

| Test | Reverted hunk | Result on reverted tree |
|---|---|---|
| `webrtc::tests::reoffer_rejected_by_str0m_keeps_the_session` | `accept_reoffer` back to `accept_offer(offer)?` (drop the `InvalidOffer` mapping) | **FAILED**: `expected InvalidOffer for a str0m pre-mutation rejection, got Err(Rtc(Sdp(Inconsistent("Session attribute a=group missing"))))` — the pre-fix fatal class |
| `webrtc::tests::audio_only_peer_can_append_a_video_section_on_reoffer` | prefix guard back to `next_mids == self.remote_mids` | **FAILED** |

### 8.3 Reverts that stay green (no regression test exists)

| Behaviour | Re-introduced code | Result |
|---|---|---|
| gate re-arm removal | re-added `if reoffer.publishes_video { handle.waiting_keyframe.store(true, …) }` in the re-offer success arm | **SFU suite 97/97 green** → no test protects the gate fix |
| TrackAdded announcement diff | `filter(|_| true)` (announce duplicates on every re-offer) | **server suite 176/176 green** → no test covers the announcement path |

So the two *new* tests are real, the gate fix and the `TrackAdded` announcement
are only code/live-verified and remain unprotected.

---

## 9. Counts, score, path to done

* **P0: 0.**
* **P1: 0.** P1-1b residual closed with real-UDP evidence: BUNDLE-less and
  echoed-`a=ice-lite` re-offers are `InvalidOffer`, the peer stays registered and
  forwards media, and a valid re-offer afterwards reuses the same `Rtc`, socket
  and ICE ufrag. No other malformed-but-parseable shape kills the call.
* **P2: 11**
  1. `TrackAdded` announcement has no end-to-end regression test (revert →
     green); the new-track path is reachable and correct by inspection only.
  2. `TrackInfo` for the announced (including appended) track is hardcoded
     (`ssrc: 0`, fixed clock/channels), so clients cannot match it to the actual
     SSRC.
  3. A rejected re-offer can still mutate str0m ICE state (`add_ice_details`
     before `apply_offer`); the docs' "without touching the live session" claim
     is false for that input.
  4. The gate-re-arm removal has no regression test (re-adding the block → green).
  5. The early-preflight ordering (exit 78 without creating the DB) has no
     automated test, and dev configs (`allow_insecure_defaults=true`) skip it,
     so a dev TEST-NET config still creates the DB before exiting 78.
  6. Queue-full `webrtc_offer_retry`/`retryable=1` has no test at the signalling
     layer (only a registry-level error test; the `"retry"` substring heuristic
     is brittle).
  7. `WEBRTC_FALLBACK{offer_rejected}` is still incremented for a **live** peer's
     rejected re-offer (`lib.rs:1016`), polluting the legacy-fallback metric.
  8. `SetSubscription` store-order window (`webrtc_room.rs:418-439`): subscription
     map/atomics/command are updated in three separate steps; not covered by a
     test.
  9. Re-offer reply is sent before the peer's own timer actions are flushed
     (`webrtc_room.rs:673-690`), and `handle_timeout` runs twice on that path.
  10. H264-only publisher is still accepted (`validate_offer` only needs Opus or
      VP8 anywhere in the SDP); the video m-line is then answered rejected.
  11. Re-offer track removal never emits `TrackRemoved` (no such message anywhere
      in `lib.rs`).
* **P3: 5**
  1. The mid guard is anchored to the initial offer's `remote_mids`, never
     updated after a successful re-offer, so renames/reorders of appended
     sections bypass it (str0m then rejects non-fatally).
  2. The offer's `a=group:BUNDLE` tags (and `a=setup` changes) are not validated
     at all; even `a=group:BUNDLE bogus` is accepted on a live peer (str0m
     0.24.1 regenerates the answer group from the session).
  3. `InvalidOffer` is downgraded to `WebRtcError::Sdp` at the registry boundary
     (`webrtc_room.rs:237`), losing the typed cause, and the log at `:236` says
     "keeping old session" for a rejected re-offer.
  4. Duplicated `a=ice-options:trickle` stripping (two call sites) and the double
     `handle_timeout` on the re-offer path (see P2-9; the duplication itself is
     cosmetic).
  5. Process-global env mutation in server tests (`network_integration.rs:121`
     `VKS_WEBRTC_TRANSPORT=0` in a `Once`, `:2220` `VKS_METRICS_TOKEN`), and the
     candidate check exists only in `main` so library embedders still get a
     `None` registry instead of a startup error.

**Score: 9.0 / 10.** Necessary condition for 9.5 (0 P0/P1 + real evidence) is
met, but 9.5 also implies no material P2s; the rejected-re-offer ICE mutation,
the missing e2e coverage for the announcement/gate/retryable paths and the
hardcoded `TrackInfo` keep it at 9.0.

**Path to 9.5:** pre-validate (or snapshot/restore) the ICE/group state before
str0m's `accept_offer` so a rejection is truly side-effect free; add the
end-to-end `TrackAdded` test (§6) and the WS-level `retryable` assertion; add one
live test each for the gate (delta after a re-offer) and the preflight ordering;
populate `TrackInfo` from the negotiated parameters.

---

## 10. Caveats

* The harness is two genuine str0m 0.24.1 clients over real loopback UDP through
  the real `WebRtcRoomRegistry`, but it is a str0m client, not Chromium; Chrome's
  exact re-offer shape (mids, BUNDLE, DTLS setup) is not reproduced.
* The malformed-but-parseable offers are authenticated-client inputs, the same
  class as the previously reported swapped-m-line repro; browsers do not send
  them. The ICE-mutation P2 is therefore low likelihood but real.
* Media checks use tiny synthetic VP8 frames (VP8 frame header P bit) rather than
  a real encoder; keyframe/delta classification and SRTP forwarding are real.
* The queue-full `retryable` mapping was verified by code inspection; the queue
  itself was only exercised by the registry unit test.
* The dev-loopback start was verified by reading the server's own
  "ready and listening" log line and killing it after 8 s (`timeout` → exit 124).
* Scratch artifacts under `/dev/shm` were reviewer-only and removed at the end:
  `t11a_rr3`, `t11a_rr3_h`, `t11a_revert3`, `rr3_probe`, probe logs/DBs,
  harness/probe test files, and the two cargo target caches. The repo contains
  only this report. The live `vks_data.db` was never opened (md5/mtime unchanged:
  `6a23442016492077f311becf14badace` / `2026-10-29 07:33:29.801`).
