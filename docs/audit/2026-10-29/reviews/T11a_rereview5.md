# T11a — certification re-review #5 (`4686ac8`)

**Verdict: 8.5 / 10 — NOT certifiable as-is (done: no). 0 P0, 1 P1, 11 P2, 6 P3.**

The documentation half of `4686ac8` is complete and accurate: every
"fails before mutating" / "untouched" claim is gone from the current
implementation docs and inline comments, and a real str0m probe confirms the
replacement wording (same-credential rejection leaves the server ufrag alone; a
new-credential rejection rotates it). The real-UDP acceptance scenario also
reproduces at this HEAD: BUNDLE-less and echoed `a=ice-lite` re-offers are
rejected with the peer still forwarding media, the next valid re-offer reuses
the same session/port/ufrag, appended m-lines are accepted, and the removed
wrong-peer gate is absent at runtime.

**However, the new regression test does not test the claimed code and the
required clippy gate is red.** `new_track_kinds` (`crates/vks-server/src/lib.rs:1418-1431`)
has no caller at all — the `TrackAdded` loop still uses the old inline
`filter` at `:1099-1101`. Reverting that filter to `filter(|_| true)` leaves
the whole server lib suite green (145/145, including the new unit test), which
was verified by experiment. Consequently:

* acceptance criterion 2 ("`new_track_kinds` is actually used … its test fails
  if the filter is reverted") **fails**: the helper is dead code;
* `cargo clippy … --all-targets -- -D warnings` **fails with two errors**
  introduced by this commit (`dead_code` on the never-called helper, and
  `clippy::cloned_ref_to_slice_refs` in the new test at `:1444`), so acceptance
  gate 4 fails.

That is a P1 CI-gate regression, not an acceptance-scenario defect, and it is a
~3-line fix. It is enough to prevent certification at this HEAD.

Reviewed commit: `4686ac8` (`docs(webrtc)+test: T11a comment accuracy and
TrackAdded regression`) on top of `57c2b4b`. The diff touches four files:
`crates/vks-media-sfu/src/webrtc.rs`, `crates/vks-media-sfu/src/webrtc_room.rs`,
`crates/vks-server/src/lib.rs` and
`docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md`. Evidence came from a
pristine `git archive 4686ac8` at `/dev/shm/t11a_rr5`, a reviewer-only
real-UDP harness (`crates/vks-media-sfu/tests/t11a_rr5_harness.rs`, never
committed, removed before cleanup), and the required gates on the same archive.
The live tree was not used for evidence; the live `vks_data.db` was not opened
and its size/mtime/md5 are unchanged (`3 248 128` bytes,
`2026-10-29 07:33:29.801017417 +0300`, `6a23442016492077f311becf14badace`).

| Severity | Count | Items |
|---|---|---|
| **P0** | **0** | — |
| **P1** | **1** | clippy `-D warnings` red: dead `new_track_kinds` + new-test `clone` lint |
| **P2** | **11** | ineffective `TrackAdded` regression test · hardcoded `TrackInfo` · documented str0m ICE mutation · gate fix untested · early-preflight ordering untested/dev skip · queue-full retryable untested + substring heuristic · `WEBRTC_FALLBACK` on live re-offer · `SetSubscription` store-order · reply-before-timer-flush · H264 video publisher · no `TrackRemoved` |
| **P3** | **6** | mid guard anchored to initial offer · BUNDLE tags/`a=setup`/PT accepted (now documented) · `InvalidOffer`→`Sdp` + stale log · duplicated trickle strip/double `handle_timeout` · process-global env in tests + candidate check only in `main` · `without_touching` test name / "before mutation" comment narrowed to parse errors |

**Can T11a be marked done? No, not at this HEAD.** The functional acceptance
scenario passes with real UDP and there is no P0; but one P1 (red required
clippy gate) and the failed `TrackAdded` test criterion must be fixed first.
Nothing in the remaining P2/P3 list is an acceptance blocker.

---

## 1. Overclaims: clean in current implementation docs, accurate wording

Grep at `4686ac8` over `crates/vks-media-sfu/src/webrtc.rs`,
`crates/vks-media-sfu/src/webrtc_room.rs` and `docs/` for
`before mutating`, `without touching`, `untouched`, `fails before`, plus the
Russian equivalents (`не трогает`, `не изменяет`, `до мутации`):

* **No current implementation doc or inline comment contains any of them.**
  The only hits are superseded historical audit snapshots
  (`docs/audit/.../T11a.md`, `T11a_rereview*.md`, unrelated T02 "DB untouched"
  lines), which are review artefacts, not implementation documentation.
* The three comments flagged by re-review #4 are now correct:
  * `webrtc.rs:110-113` (`remote_mids`): "str0m rejects structural changes; the
    SDP body is not applied, but its ICE credentials may already be — see
    `accept_reoffer`";
  * `webrtc.rs:223-228` (`accept_reoffer`): "rejects it with an error … The DTLS
    session and media keep running, but str0m runs `add_ice_details` before that
    validation, so the server ufrag may already have rotated";
  * `webrtc_room.rs:694-699` (`InvalidOffer` arm): "The live call keeps flowing
    (peer/socket/DTLS/media), though str0m may already have applied the offer's
    ICE credentials".
* `webrtc_room.rs:7-12` and `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:51-58`
  say the same thing, hedge with "may/can", and explicitly distinguish the
  surviving call from the rotated ufrag. The transport doc now also names the
  rejected shapes (missing BUNDLE, echoed ice-lite, renamed mid) and notes that
  BUNDLE-tag / `a=setup` / payload-type changes are silently accepted by str0m.
* Residual wording, **not counted as an overclaim**: the unit test
  `malformed_reoffer_is_rejected_without_touching_the_live_session`
  (`webrtc.rs:588`) and its comment "the parse happens before mutation"
  (`:586-587`) describe a `t=0 1` parse error, which really is rejected before
  any session mutation. The name is narrower than it sounds; see P3-6.

### Real str0m probe at this HEAD

Bare-peer probe (same fingerprint/mids; ICE credentials replaced; BUNDLE
removed), run on the pristine archive:

```
SAME-CRED MISSING BUNDLE -> InvalidOffer: re-offer rejected by str0m: SDP inconsistent: Session attribute a=group missing
SAME-CRED ufrag before=Q0xvyLFlPyNxRzoM after=Q0xvyLFlPyNxRzoM
NEW-CRED MISSING BUNDLE -> InvalidOffer: re-offer rejected by str0m: SDP inconsistent: Session attribute a=group missing
NEW-CRED ufrag before=Q0xvyLFlPyNxRzoM after_reject_and_valid=6e7us36Pj7ngRkSn
```

Interpretation:

* a rejected re-offer with the **same** ICE credentials leaves the server ufrag
  unchanged (the credentials were already applied at initial accept);
* a rejected re-offer with **new** credentials causes the next valid re-offer
  to answer with a different server ufrag — exactly what the new docs and
  comments say. The valid re-offer still succeeds on the same `Rtc` and the same
  local address (`127.0.0.1:41032`), so the call/socket survive.
* This confirms the str0m ordering (`str0m-0.24.1/src/change/sdp.rs:84`
  `add_ice_details` before `:112` `apply_offer`, whose first statement is
  `offer.assert_consistency()?` at `:947`), and it confirms the wording is now
  behaviour-accurate, not merely softened.

---

## 2. The `TrackAdded` regression test is dead code and does not guard the loop

This is the material finding of this re-review.

### 2.1 No caller

`new_track_kinds` (`crates/vks-server/src/lib.rs:1418-1431`) is called **only**
from its own unit test (`:1436-1446`). `grep -rn new_track_kinds crates/` at
`4686ac8` returns exactly those two locations. The production `TrackAdded` loop
still has the original inline filter:

```rust
// crates/vks-server/src/lib.rs:1099-1101
for track in tracks
    .iter()
    .filter(|track| !previous_kinds.contains(&track.media_kind))
```

So the commit extracted the predicate into a helper but never wired it back into
the loop. The helper is dead in every non-test build; rustc says so:

```
warning: function `new_track_kinds` is never used
    --> crates/vks-server/src/lib.rs:1418:4
```

### 2.2 Revert experiment

On the pristine archive, the production filter was changed to
`filter(|_| true)` and the server lib suite re-run:

```
test tests::new_track_kinds_announces_only_unseen_kinds_once ... ok
test result: ok. 145 passed; 0 failed; 3 ignored
```

So the new test does **not** fail when the production `TrackAdded` filter is
reverted; it never executes that filter. It only guards the helper's own
condition (changing the helper's `!previous.contains(...)` would fail the first
assertion). The prior review's P2-1 — no committed regression test for the
`TrackAdded` filter — therefore remains open despite the commit message.

### 2.3 Additional gaps inside the new test

* The commit message claims "duplicate kinds in one offer de-duplicated", but
  the test never passes two tracks of the same kind; the `!kinds.contains(...)`
  branch is never exercised. It is also unreachable through the real loop:
  `validate_offer` rejects `audio > 1 || video > 1`
  (`webrtc.rs:481-484`).
* The test itself breaks the required clippy gate:
  `clippy::cloned_ref_to_slice_refs` at `lib.rs:1444` (`audio.clone()` should
  be `std::slice::from_ref(&audio)`), see §4.

The production behaviour is still correct by inspection for the supported offer
shapes (one audio + one video m-line, `previous_kinds` from
`publisher_track_kinds`), which is why this is P2 for behaviour; but the stated
purpose of the commit is not met and the CI gate is broken by it.

---

## 3. Real-UDP evidence at `4686ac8`

Reviewer harness: two genuine str0m 0.24.1 clients bind loopback UDP sockets,
send real offers through `WebRtcRoomRegistry::accept_offer`, complete
ICE/DTLS/SRTP against the real per-peer tasks, and exchange VP8 frames
A→SFU→B and B→SFU→A. Malformed re-offers use the same registry path and the
same participant; a keyframe opens the egress gate. Run with
`cargo test -p vks-media-sfu --features str0m --test t11a_rr5_harness -- --nocapture --test-threads=1`:

```
HANDSHAKE: A connected=true B connected=true A ufrag=IqY1sjRgY449jQt6 port=35978
BASELINE: B video_rx=1
BUNDLE-LESS -> SDP: re-offer rejected by str0m: SDP inconsistent: Session attribute a=group missing
AFTER BUNDLE-LESS: B video_rx=2 (+1)
ECHOED ICE-LITE -> SDP: re-offer rejected by str0m: remote sdp: Both peers being ICE-Lite not supported
AFTER ICE-LITE: B video_rx=3 (+1)
VALID RE-OFFER: reoffered=true ufrag_same=true port_same=true
AFTER VALID RE-OFFER: B video_rx=4 (+1)
B->A BASELINE: A video_rx=1
B->A DELTA RIGHT AFTER A's VALID RE-OFFER: received=1 (gate not re-armed)
```

and, for the appended section through the registry:

```
APPENDED M-LINE: reoffered=true rtp_maps=[(111, "opus", 48000, 2), (96, "vp8", 90000, 0)]
```

Interpretation:

* both malformed offers are rejected as `InvalidOffer` and mapped to
  `WebRtcError::Sdp` at the registry boundary (`webrtc_room.rs:236-239`); no
  peer is replaced (`peer_count == 2`, `has_peer(A) == true` after each);
* a fresh keyframe A→B is still forwarded after each rejection, so the real
  SRTP/fanout path is alive;
* the next valid re-offer is answered by the existing peer task
  (`reoffered=true`) with the same server ufrag and candidate port — same `Rtc`,
  same UDP socket;
* the B→A delta immediately after A's valid re-offer is delivered, so the
  removed wrong-peer gate re-arm is absent at runtime as well as in code;
* an audio-only peer that appends a video m-line is answered on the same peer
  (`reoffered=true`, `peer_count == 1`) with Opus + VP8 negotiated.

Code check for the gate: `waiting_keyframe.store` remains only at
`webrtc_room.rs:438` (`set_subscription`, synchronous), `:559` (fanout
queue-full target) and `:729` (`SetSubscription` command arm); the check is at
`:655`. There is no store in the re-offer success arm (`:671-695`).

This is equivalent to the prior review's live evidence, re-produced on the new
HEAD rather than carried over.

---

## 4. Gates on the pristine archive

All commands used `--locked` on `/dev/shm/t11a_rr5`. The reviewer harness is
not part of any of these targets and was removed before the final
media-SFU/clippy/fmt runs (the server suite does not build it). Build artefacts
were relocated to `/home/pro/.cache/t11a_rr5_target` when `/dev/shm` hit its
quota.

| Gate | Result |
|---|---|
| `cargo test -p vks-server --tests` | **exit 0** — 145 lib + 1 `config_failclosed` + 1 `h265_rtp` + 23 `network_integration` + 7 `operator_policy` = **177 passed, 0 failed, 3 ignored** (the new unit test is among the 145; 144 before) |
| `cargo test -p vks-media-sfu --features str0m` | **exit 0** — **97 passed, 0 failed** |
| `cargo test -p vks-signaling` | **exit 0** — **45 passed, 0 failed** |
| `cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets -- -D warnings` | **FAIL — exit 101, 2 errors** (below) |
| `cargo fmt --all -- --check` | **exit 0** |

Clippy output:

```
error: function `new_track_kinds` is never used
    --> crates/vks-server/src/lib.rs:1418:4
     = note: `-D dead-code` implied by `-D warnings`
error: could not compile `vks-server` (lib) due to 1 previous error

error: this call to `clone` can be replaced with `std::slice::from_ref`
    --> crates/vks-server/src/lib.rs:1444:54
     = note: `-D clippy::cloned-ref-to-slice-refs` implied by `-D warnings`
error: could not compile `vks-server` (lib test) due to 1 previous error
CLIPPY_EXIT:101
```

Both errors are introduced by `4686ac8`. This is the P1: the required clippy
gate cannot pass at this HEAD even though the runtime code is unchanged.
`cargo test -p vks-server --tests` also emits the same `dead_code` warning
(without failing); the suite itself is green.

Environment note: the first server-suite run, executed concurrently with the
harness build and other cargo jobs on the host, failed one timing test unrelated
to T11a
(`gateways::media_runtime_tests::gateway_audio_consumes_both_halves_of_a_40ms_packet_once`,
`gateways.rs:5410`). An isolated rerun passed 177/177; this review does not
count it as a T11a finding, but the test is load-sensitive.

---

## 5. Remaining P2 / P3 at `4686ac8`

None of these blocks the functional acceptance scenario. They are follow-ups:
missing tests, observability, hardcoded metadata, or inherent str0m behaviour
that is now documented.

### P2 (11)

1. **`TrackAdded` regression test does not cover the production loop.**
   `new_track_kinds` (`lib.rs:1418-1431`) has no caller; the loop still uses the
   inline filter at `:1099-1101`. Reverting that filter leaves 145/145 green;
   the dedup branch is untested and unreachable given `validate_offer`
   (`webrtc.rs:481-484`). See §2.
2. **`TrackInfo` is hardcoded.** `ssrc: 0` at `lib.rs:1122`, fixed
   48 kHz/2 for audio and 90 kHz/0 for video at `:1123-1134`. A client cannot
   correlate the announced track (including an appended video track) with the
   actual SSRC.
3. **A rejected re-offer still mutates str0m ICE state.** `webrtc.rs:223-231`
   calls str0m `accept_offer`; str0m applies `add_ice_details`
   (`str0m-0.24.1/src/change/sdp.rs:84`) before the consistency check at `:112`/
   `:947`. The call survives and media keeps flowing, but the server ufrag can
   rotate (reproduced in §1). This is inherent str0m behaviour, now documented
   in both public docs and all inline comments; non-blocking.
4. **Gate-re-arm removal has no committed regression test.** The code is absent
   and the reviewer harness catches a re-introduced block, but no repository
   test asserts that a delta after a re-offer is delivered.
5. **Early-preflight ordering is untested, and dev configs skip it.**
   `main.rs:98-107` runs the early check only when `!allow_insecure_defaults`;
   the DB lock is at `:112-115` and `SqliteStorage::init` at `:134`; the late
   preflight (`:426`) runs after storage. No automated test asserts
   "exit 78 without creating the DB" for a dev config.
6. **Queue-full `webrtc_offer_retry`/`retryable=1` is untested.**
   `webrtc_room.rs:212` returns `Sdp("re-offer queue is full; retry")`;
   `lib.rs:978-980` maps any SDP error containing `"retry"` to
   `webrtc_offer_retry`, and `:1010-1014` makes it retryable. No signalling-layer
   test drives the queue-full path; the substring heuristic is brittle.
7. **`WEBRTC_FALLBACK{offer_rejected}` counts a live peer's rejected re-offer.**
   `lib.rs:1016-1018` increments the legacy-fallback metric on any
   `webrtc_failure`, including a re-offer rejected while the call continues.
8. **`SetSubscription` store-order window.** `webrtc_room.rs:420-439` updates
   the map and the atomics, then `try_send`s; a full queue leaves the state
   updated without the task seeing the command. No test covers it.
9. **Re-offer reply before the peer's timer actions are flushed.** The oneshot
   reply is sent at `webrtc_room.rs:676-681`, then `handle_timeout` runs at
   `:683-690`, and `run_peer` calls it again at `:742-748`. The answer can reach
   signalling before local ICE/DTLS actions are transmitted, and the timer runs
   twice.
10. **H264 video publisher (with Opus audio) is accepted.** `validate_offer`
    (`webrtc.rs:462-496`) only requires Opus or VP8 anywhere in the SDP; an
    Opus-audio + H264-video offer passes validation, then the video m-line is
    answered rejected while the publisher believes video works. The SFU
    advertises only Opus/VP8.
11. **No `TrackRemoved` on re-offer track removal.** No `Payload::TrackRemoved`
    is emitted from `crates/vks-server/src/lib.rs`; only
    `vks-signaling/src/handler.rs:233` recognizes the payload.

### P3 (6)

1. The mid guard is anchored to the initial offer's `remote_mids`
   (`webrtc.rs:110-113`, set at `:185`, never updated after a successful
   re-offer), so appended-section renames/reorders bypass our guard and are
   caught only by str0m (non-fatal `InvalidOffer`).
2. The offer's `a=group:BUNDLE` tags and `a=setup` changes are not validated
   (`webrtc.rs:462-496`); changed BUNDLE tags, `a=setup:passive` and changed
   Opus PT are silently accepted. The transport doc now states this explicitly
   (`WEBRTC_TRANSPORT_2026-10-22.md:51-58`), so it is documented behaviour, not
   an overclaim.
3. `InvalidOffer` is downgraded to `WebRtcError::Sdp` at the registry boundary
   (`webrtc_room.rs:236-239`), losing the typed cause; the adjacent log still
   says "keeping old session" even when the ICE state was already touched.
4. Duplicated `a=ice-options:trickle` stripping (`webrtc.rs:161` and `:234`)
   plus the double `handle_timeout` on the re-offer path (same as P2-9) —
   cosmetic and timer-order.
5. Process-global env mutation in server tests (`network_integration.rs:121`,
   `VKS_WEBRTC_TRANSPORT=0` in a `Once`; `:2226`, `VKS_METRICS_TOKEN`), and the
   candidate check exists only in `main.rs:98`; library embedders get a `None`
   registry instead of a startup error.
6. The test name `malformed_reoffer_is_rejected_without_touching_the_live_session`
   (`webrtc.rs:588`) and its "before mutation" comment (`:586-587`) are accurate
   for a parse-error input but read more broadly than the `add_ice_details`
   case. Renaming would remove the last "without touching" string from the
   source; not an overclaim in its own context.

---

## 6. Score: 8.5, and why 9.5 is not justified on this evidence

The prior review's 9.0 withheld 9.5 because of (a) stale inline comments,
(b) no committed `TrackAdded`/gate regression test, and (c) hardcoded
`TrackInfo`. This commit **fixes (a) completely** and the docs now match the
observed str0m behaviour, including the ufrag rotation. It attempts (b) but
misses: the helper is dead code, the test never exercises the loop, and the
commit breaks the clippy gate. (c) is unchanged but was never an acceptance
blocker.

The necessary functional evidence for a high score is present and re-verified at
this HEAD:

* real ICE/DTLS/SRTP with two str0m clients;
* BUNDLE-less and true echoed-`a=ice-lite` re-offers → `InvalidOffer` with the
  peer registered and A→B media still forwarded (+1 frame after each);
* valid re-offer → `reoffered=true`, same ufrag, same candidate port, same
  session;
* appended video m-line → accepted on the same peer, Opus+VP8 negotiated;
* no wrong-peer gate re-arm (B→A delta delivered right after A's re-offer);
* docs/comments no longer overclaim; the ICE side effect is documented and
  reproduced.

But the hard bar is **0 open P0/P1 + acceptance met**, and it is not met:

* acceptance criterion 2 fails (`new_track_kinds` is not used, the test does not
  fail on a production-filter revert);
* acceptance gate 4 fails (`clippy -D warnings` exit 101, two errors).

Therefore **9.5 is not justified**, and 9.0 would be repeating the old score out
of habit while a new P1 is open. 8.5 reflects strong functional acceptance with
one easily-fixable CI blocker. If the helper is wired into the loop
(`new_track_kinds(previous_kinds, &tracks)` replacing the inline filter), the
test's `audio.clone()` becomes `std::slice::from_ref(&audio)`, clippy is green,
and the gate re-arm gets a committed test, then this same evidence set would
support **9.5**: 0 P0/P1, all acceptance criteria met, and only the documented
inherent ICE behaviour plus non-blocking P2/P3 remaining.

---

## 7. Caveats and cleanup

* The harness uses two str0m 0.24.1 clients over loopback UDP, not Chromium;
  Chrome's exact re-offer SDP shape is not reproduced.
* Media checks use synthetic VP8 keyframe/delta payloads (`0x10…`, not a real
  encoder); SRTP forwarding, depayload/keyframe classification and fanout are
  real.
* The changed-BUNDLE-tags / `a=setup` / PT acceptance evidence is carried from
  the prior review; the relevant code is unchanged between `57c2b4b` and
  `4686ac8` and was not re-probed here.
* The first parallel server-suite run flaked one unrelated gateway-audio timing
  test; the isolated rerun was 177/177. See §4.
* The live `vks_data.db` was not opened or modified; size/mtime/md5 are
  `3 248 128` / `2026-10-29 07:33:29.801017417 +0300` /
  `6a23442016492077f311becf14badace`, unchanged before/after.
* Reviewer scratch (`/dev/shm/t11a_rr5`, the harness file, gate logs, target
  dir, lib backup) was removed after this report. The live repo contains only
  this report from this review; the live tree was not modified.
