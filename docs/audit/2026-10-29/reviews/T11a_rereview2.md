# T11a — independent re-review #2 (`8af77db`)

**Verdict: 8.5 / 10 — not done.** 0 P0, **1 open P1** (P1-1b residual),
~14 P2, 3 P3.

Reviewed commit: `8af77db` (`fix(webrtc): T11a re-review — reject pre-mutation
re-offers, correct TrackAdded gating, early preflight`) on top of `650a55f`
(T09a). Every line number below is from a pristine `git archive 8af77db` copy at
`/dev/shm/t11a_rr2`; the live tree was clean (`git status --short` empty) and was
not used for evidence. The live `vks_data.db` was never opened: its mtime is
still `2026-10-29 07:33:29.801` and md5 `6a23442016492077f311becf14badace`
before and after this review.

The commit does close the *reported* P1-1b repro and P1-2: a swapped
m-line/mid-order re-offer now returns `InvalidOffer` before str0m sees it, the
peer stays registered and keeps forwarding both keyframes and deltas, and a
valid re-offer afterwards reuses the same Rtc/socket/ICE ufrag. The required
server suite is green again. But the *general* pre-mutation guarantee is still
false (one more `accept_offer` rejection still kills a healthy peer), and two
headline claims in the commit message are not in the diff at all:

1. the wrong-peer keyframe gate re-arm was **not removed** (the block is
   byte-identical to `08398b3`), so a valid re-offer by a publisher still blanks
   *its own* inbound video until the next IDR;
2. "preflight runs before storage" was **not wired**: only the helper
   `preflight_candidate` was added and it has no caller outside `preflight`;
   a TEST-NET candidate still creates the SQLite file before exiting 78.

The TrackAdded announcement filter is correct by inspection and its helper is
unit-tested, but the commit's stated "audio-only peer adds video IS announced"
scenario is rejected by the new mid-order guard, and no test covers the
announcement path (reverting it to the old suppress-all leaves the whole suite
green).

| Severity | Count | Items |
|---|---|---|
| **P0** | **0** | — |
| **P1** | **1** | P1-1b residual: a pre-mutation `sdp_api().accept_offer` `RtcError` (missing `a=group:BUNDLE`, echoed `a=ice-lite`) still removes the live peer |
| **P2** | **14** | gate re-arm not removed · TrackAdded new-track path unreachable/test gap · preflight ordering · docs overclaim · test gaps · 9 carried P2s |
| **P3** | **3** | `mids_of` stole `validate_offer`'s doc comment · unrelated Storage-trait break in this commit · `contains("retry")` code heuristic |

**T11a can be marked done? No.** The named P1-1b/P1-2 repros are fixed and the
media-plane core is solid, but one P1 class is still open, the gate P2 was not
implemented (contrary to its commit message), and the gate/TrackAdded behaviours
still have no end-to-end regression tests.

---

## 1. Scope and method

* Read `T11a_rereview.md` (8.0/10) and the prior `T11a*.md` reports; re-read the
  str0m 0.24.1 `change/sdp.rs`, `sdp/data.rs` and VP8 depacketizer paths where
  this review depends on them.
* Built a **real-UDP, two-genuine-str0m-client harness**
  (`crates/vks-media-sfu/tests/t11a_rr2_harness.rs`, added only to the pristine
  scratch copy, never to the repo) that drives the real
  `WebRtcRoomRegistry::accept_offer` path: two independent `str0m::Rtc` clients
  bind loopback UDP sockets, complete ICE/DTLS/SRTP against the registry's peer
  tasks, and exchange VP8 keyframes/deltas A→SFU→B. Re-offers are injected on
  the same registry path.
* Ran the real `vks-server` binary built from the pristine copy with three
  `/dev/shm` configs (`candidate_addr` empty / TEST-NET / loopback), checking
  exit codes, logs and whether the SQLite file appears.
* Proved regression tests by reversing exact hunks in throw-away copies
  (`/dev/shm/t11a_revert`, `/dev/shm/str0m_patch`), then discarding them.
* Wrote only this file in the repo. No live DB, no live-tree build, no source
  edit.

## 2. Required commands (pristine `8af77db`)

```bash
export CARGO_TARGET_DIR=/home/pro/.cache/t11a_rr2_target
cargo test -p vks-server --tests --locked
cargo test -p vks-media-sfu --features str0m --locked
cargo test -p vks-signaling --locked
cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets --locked -- -D warnings
```

| Command | Result |
|---|---|
| `cargo test -p vks-server --tests --locked` | **ok — exit 0**, 175 passed / 0 failed / 3 ignored (lib 144/3 ignored, config_failclosed 1, h265_rtp 1, network_integration 22, +7) |
| `cargo test -p vks-media-sfu --features str0m --locked` | **ok — 95 passed**, 0 failed |
| `cargo test -p vks-signaling --locked` | **ok — 44 passed**, 0 failed |
| clippy `-D warnings` | **ok — exit 0**, no warnings |

Both previously red recorder tests now pass in isolation:

```
test test_m5_recorder_lifecycle_integration ... ok
test test_m5_recorder_idempotent_stop_persistent_storage_and_playable_container ... ok
test result: ok. 2 passed; 0 failed; ...
```

`RecorderManager::start_recording` now returns `record_id` on both the
already-running path (`recorder.rs:123`) and the fresh path (`Ok(record_id)`,
`recorder.rs` after the session start), so the committed assertions match the
committed implementation. **P1-2 from the previous review is fixed.**

Note: the `RecordEntry` fields and the change of the `Storage` record methods
from default no-ops to required methods are in **this** commit
(`crates/vks-core/src/ports/storage.rs`), not in T09a as T09a's message claims.
Functionally fine (all four in-tree impls implement them; suite green), but it
is an unmentioned breaking trait change bundled into a WebRTC fix (P3).

## 3. P1-1b — reported repro fixed, general pre-mutation path still open

### 3.1 What is fixed (verified on the wire)

The new guard in `WebRtcPeer::accept_reoffer` (`webrtc.rs:201-207`) compares
`mids_of(offer_sdp)` with the first offer's mids and returns `InvalidOffer`
before `SdpOffer::from_sdp_string`/`sdp_api().accept_offer`. Real-UDP harness
output (exit 0):

```
HANDSHAKE: both str0m clients connected over UDP
baseline A->B forwarded: [(false, true, 161)]
B->A keyframe=true delta(before A re-offer)=true events=[(false, true, 177), (false, false, 178)]
swapped-m-line re-offer rejected: SDP: m-line/mid order changed: this peer only supports re-negotiating the existing sections
after swapped rejection, A keyframe forwarded: [(false, true, 162)]
valid re-offer: reoffered=true ufrag_same=true port_same=true
after valid re-offer, A keyframe forwarded: [(false, true, 163)]
A->B delta after A's re-offer reached B: true events=[(false, false, 165)]
```

So, for the task's P1-1b repro:

* the swapped audio/video m-line re-offer returns `Err(InvalidOffer)` **and** the
  peer stays registered (`has_peer` and `peer_count == 2` are asserted, and the
  next keyframe is forwarded);
* media keeps flowing after the rejection (marker `0xA2`);
* the valid re-offer after the rejection has `reoffered=true`, the **same server
  ICE ufrag** and the **same candidate port** (hence the same `Rtc`, socket and
  ICE agent);
* subscriber B was not re-armed by A's re-offer: A→B deltas still arrive.

The new regression test `swapped_m_line_reoffer_is_rejected_and_keeps_the_session`
(`webrtc.rs:593`) is genuine: removing the guard makes it fail with
`Err(Rtc(RemoteSdp("Changed order for m-line with mid: TvS")))` — i.e. the
pre-fix fatal path — see §6.

### 3.2 What is still broken — a pre-mutation `accept_offer` error kills the peer

`accept_reoffer` still ends with

```rust
let answer = self.rtc.sdp_api().accept_offer(offer)?;   // webrtc.rs:219
```

and the registry's `run_peer` treats every non-`InvalidOffer` error as fatal
(`webrtc_room.rs:707-709`: reply `Err`, warn "re-offer failed; closing peer",
`break`), which runs `remove_generation` and unregisters the peer. The new
mid-order guard is not a general pre-validation: the `Sdp` is still handed to
str0m, and str0m has pre-mutation validation errors of its own. A re-offer that
omits the session-level `a=group:BUNDLE` line passes `validate_offer` (ICE/
fingerprint/m-line counts) and the mid guard (the `a=mid:` list is unchanged),
then fails `offer.assert_consistency()` inside `apply_offer` **before** the
session/m-lines are touched:

```
missing a=group:BUNDLE re-offer rejected: SDP: str0m: SDP inconsistent: Session attribute a=group missing
after missing-BUNDLE rejection: has_peer(A)=false peer_count=1
after missing-BUNDLE kill, A->B forwarded: []
```

and, on a bare `WebRtcPeer`, the same offer provably mutates nothing:

```
direct missing-BUNDLE: Rtc(Sdp(Inconsistent("Session attribute a=group missing")))
direct: session survives the rejected pre-mutation offer and accepts a valid re-offer
```

A second instance of the same class (a client echoing the answer's
`a=ice-lite`) is also a raw pre-mutation `RtcError`:

```
re-offer with echoed a=ice-lite: Rtc(RemoteSdp("Both peers being ICE-Lite not supported"))
```

Reachability through the real signalling path: `lib.rs:581` `offer_supports_real_webrtc`
only checks for the ICE/fingerprint substrings, and `vks_signaling::sdp::Sdp::parse`
accepts an SDP without `a=group`; the handler does not reject it before
`lib.rs:895` calls the registry. So this is not only an artificial direct-peer
path.

**Severity:** this is the same T11a defect class the commit claims to remove and
the module doc now states as a general guarantee (`webrtc_room.rs:8`: "A re-offer
the peer cannot negotiate is rejected without touching the live session").
I count it **P1** because the guarantee is the point of P1-1b; likelihood is low
(browsers always send BUNDLE and never send `a=ice-lite`, so this is a
buggy/malicious-client path, not a normal browser path). If the team explicitly
scopes re-offers to BUNDLE-shaped SDP and documents that, it can be downgraded
to P2 — but then the doc/code claim must be narrowed too.

**Required fix:** either (a) pre-validate `a=group:BUNDLE` (present, mids match
the `a=mid:` list) and reject `a=ice-lite` in `validate_offer`/`accept_reoffer`,
or (b) map every `accept_offer` error that can occur before `apply_offer` to
`InvalidOffer` (`sdp_api().accept_offer` is called after our own validation, so
the safe distinction must be established, e.g. by snapshot/`assert_consistency`
first). Add a live test with the missing-BUNDLE re-offer asserting the peer is
still registered **and still forwards**, not only that the call returns `Err`.

## 4. Item 3 — gate re-arm removal: **not done** (claim is false)

The commit message says "Removed the wrong-peer keyframe gate re-arm on
re-offer". `git show 8af77db -- crates/vks-media-sfu/src/webrtc_room.rs` touches
only the module doc; the block is byte-identical at `08398b3` and `8af77db`:

```rust
if reoffer.publishes_video {
    handle.waiting_keyframe.store(true, Ordering::Relaxed);   // webrtc_room.rs:229-230
}
```

`handle` is again the re-offering publisher's handle; that gate controls media
sent **to** that peer. Harness evidence confirms the prior P2 is unchanged:

```
A->B delta after A's re-offer reached B: true events=[(false, false, 165)]
B->A delta right after A's valid re-offer passed through: false (events=[])
B->A keyframe after A re-offer: [(false, true, 180)]
```

So subscribers are not gated (B keeps getting A's deltas — nobody is left gated
forever), but the re-offering publisher's own inbound video is blanked until the
next IDR, exactly as reported before. The correct fix (arm the *subscribers'*
gates / request a keyframe, not the publisher's own gate) is still absent, and
there is no test for it. **Carried P2, not fixed.**

## 5. Item 4 — TrackAdded: filter correct, stated scenario unreachable, no e2e test

* `SfuEngine::publisher_track_kinds` (`engine.rs:344`) is added and its unit
  test (`engine.rs:635`) is real: stubbing the helper to return an empty vector
  fails it (`left: []`, `right: [Audio]`).
* `lib.rs:1083-1101` now captures `previous_kinds` before `add_publisher` and
  filters announcements by `!previous_kinds.contains(&track.media_kind)`. A
  same-mid re-offer that re-sends the same kinds therefore does **not**
  duplicate `TrackAdded` (code inspection; no end-to-end test).
* The commit's stated case — "a genuinely new track (audio-only peer adds
  video)" — cannot work: adding a video m-line appends an `a=mid`, and the new
  guard rejects any mid-list change. Direct evidence:

  ```
  audio-only + appended video m-line: InvalidOffer("m-line/mid order changed: this peer only supports re-negotiating the existing sections")
  ```

  The peer survives (non-fatal `InvalidOffer`), but video is never accepted and
  so `TrackAdded` is never emitted. The only reachable "new kind" is a same-mid
  codec change (initial offer had a video m-line but it was rejected, later
  re-offer adds VP8) — plausible but untested and not what the commit message
  claims.
* No test covers the announcement path: reverting `lib.rs` to the old
  `filter(|_| !is_reoffer)` suppress-all leaves the entire server suite green
  (§6). **P2: over-suppression narrowed in code, new-track path blocked, no
  end-to-end regression test.**

## 6. Item 5 — retryable and preflight

### 6.1 Queue-full `retryable=true` — correct in code, untested

`webrtc_room.rs:210` returns `WebRtcError::Sdp("re-offer queue is full; retry")`;
`lib.rs:978-980` maps any `Sdp` message containing `"retry"` to
`webrtc_offer_retry`, and `lib.rs:1013` adds it to the retryable set. The code
path is correct, but no Rust/e2e test asserts `code == webrtc_offer_retry` and
`retryable == 1`; `tests/e2e/webrtc.js` only checks `room_full`. The substring
heuristic is also brittle (P3): any future SDP error containing "retry" becomes
retryable.

### 6.2 Early preflight before DB — **empty only; TEST-NET still creates the DB**

Commit claim: "Preflight runs before storage: empty/bad candidate exits 78
without creating or locking the SQLite database." Real binary from the pristine
copy:

```
# candidate_addr = ""
EMPTY_EXIT=78
probe_empty.db: No such file or directory
error=VKS_WEBRTC_TRANSPORT is enabled but candidate_addr is empty ...

# candidate_addr = 192.0.2.10 (TEST-NET)
TESTNET_EXIT=78
-rw------- 1 pro pro 167936 ... /dev/shm/probe_testnet.db
INFO ... main.rs:129: connecting to SQLite database url=sqlite:///dev/shm/probe_testnet.db?mode=rwc
ERROR ... main.rs:423: WebRTC preflight failed error=candidate_addr 192.0.2.10 is a documentation (TEST-NET) address
```

`main.rs` order is unchanged: early empty check at 94-103 → DB lock 108 →
`SqliteStorage::init` 130 → preflight 422. The new `preflight_candidate`
(`webrtc_runtime.rs:82`) is called only by `preflight` (`:76`) — it has no
pre-storage caller, so it is dead code for the stated purpose. **The prior P2-6
is still open and the commit message is wrong.**

The dev loopback escape hatch still works (candidate `127.0.0.1`,
`allow_insecure_defaults=true`): the server logs
`WebRTC preflight OK candidate=127.0.0.1 ports="ephemeral"`, reaches
`vks-server ready and listening ... addr=127.0.0.1:0`, and runs until the test
timeout (exit 124). So "dev loopback still allowed" is met.

## 7. Item 6 — docs and regression-test reality

Docs were partly updated and mostly accurate:

* `webrtc_room.rs:1-11` and `webrtc_runtime.rs:1-10` now describe the async
  registration/re-offer flow correctly (re-validation under the admission lock
  matches `lib.rs:904-943`);
* `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:50-53` and
  `deploy/systemd/vks-mcu-prod.service:19-21` now say the real transport is on
  by default (matches `webrtc_runtime::enabled()`, default `true`).

But both the module doc and the implementation doc still claim a general
guarantee that the code does not provide:

* `webrtc_room.rs:8`: "A re-offer the peer cannot negotiate is rejected without
  touching the live session" — false for missing `a=group`/echoed `a=ice-lite`;
* `WEBRTC_TRANSPORT_2026-10-22.md:52`: "несовместимый re-offer отклоняется, не
  разрывая живой звонок" — same problem.

Regression tests proven by reverting exact hunks in a pristine copy:

| Test | Reverted hunk | Result on reverted tree |
|---|---|---|
| `webrtc::tests::swapped_m_line_reoffer_is_rejected_and_keeps_the_session` | removed the `remote_mids` guard (`webrtc.rs:201-207`) | **FAILED**: `expected a rejected re-offer, got Err(Rtc(RemoteSdp("Changed order for m-line with mid: TvS")))` |
| `engine::tests::publisher_track_kinds_diffs_new_tracks_for_reoffer_announcements` | `publisher_track_kinds` returns an empty vec | **FAILED**: `left: []`, `right: [Audio]` |
| (no such test) TrackAdded announcement in `lib.rs` | restored `filter(|_| !is_reoffer)` suppress-all + `is_reoffer` flag | **server suite still green — 175 passed / 0 failed**, i.e. the announcement fix is untested |

So one regression test is real and the TrackAdded *helper* test is real, but the
TrackAdded *announcement*, the gate re-arm, the queue-full `retryable` mapping,
the TEST-NET ordering and the gate behaviour have no regression test at all.

## 8. New defects found in this review

1. **Appended m-line re-offer is now hard-rejected (new P2 regression).** At
   `08398b3` an audio-only peer that later added a first video m-line was
   accepted by `validate_offer`/str0m (only the `TrackAdded` was over-
   suppressed); `8af77db`'s exact-equality mid guard rejects every appended
   m-line, i.e. the normal "join with mic, then enable camera" renegotiation
   now fails with `InvalidOffer` and the camera never publishes. The call
   survives, but this is a user-visible feature regression and it contradicts
   the commit's own TrackAdded claim. Fix: allow *appended* m-lines (prefix
   match / order-preserving superset) while rejecting reorders, and add the new
   kinds to the announcement diff.
2. **Doc-comment mangling (P3).** `mids_of` (`webrtc.rs:441`) was inserted
   between `validate_offer`'s doc comment and `validate_offer`, so the
   "Rejects offers this SFU cannot safely serve…" doc is now attached to
   `mids_of`.
3. **Unrelated Storage-trait break in a WebRTC fix (P3).** See §2; no functional
   failure in-tree, but it belongs to T09a and is not in the commit message.

No new P0. No new P1 beyond the P1-1b residual above (which is the carried,
partially fixed P1, not a new class).

## 9. Counts, score, path to done

* **P0: 0.**
* **P1: 1** — P1-1b residual: the reported swapped-mline repro is fixed, but a
  pre-mutation `RtcError` from `sdp_api().accept_offer` (missing
  `a=group:BUNDLE`, echoed `a=ice-lite`) still removes the healthy peer. Low
  likelihood (non-browser SDP), but the code/doc general guarantee is false.
* **P2: 14**:
  1. gate re-arm wrong peer — **not removed** (live evidence, no test);
  2. TrackAdded new-track scenario unreachable + no announcement test;
  3. preflight ordering: TEST-NET creates the DB before exit 78, helper dead;
  4. stale/overclaim docs (module + implementation doc "without touching the
     live session");
  5. test gaps (queue-full retryable, gate, TrackAdded, TEST-NET exit, TrackInfo;
     queue-full test capacity 8 vs prod 64);
  6. `SetSubscription` store-order window (`webrtc_room.rs:437-441`);
  7. re-offer reply before its own timer flush (`webrtc_room.rs:680-688`);
  8. process-global env mutation in tests;
  9. `TrackInfo`/`MEDIA_BYTES` hardcoded;
  10. duplicated strip-trickle/mids + double `handle_timeout` nits;
  11. `WEBRTC_FALLBACK{offer_rejected}` still incremented for a live peer's
      rejected re-offer;
  12. H264-only video silently accepted (substring codec check);
  13. early candidate check only in `main`; embedders still get `None` registry;
  14. re-offer track removal sends no `TrackRemoved`.
* **P3: 3** — doc-comment mangling, unrelated Storage-trait break, `"retry"`
  substring heuristic.

**Score: 8.5 / 10.** The commit genuinely fixes the reported P1-1b repro (live
media proof), fixes P1-2 (all required gates green, both recorder tests pass),
adds a real swapped-mline regression test, implements queue-full `retryable` in
code, and narrows TrackAdded over-suppression. It stays below 9.5 because one
pre-mutation rejection class still kills the live call, and because two claimed
fixes are simply absent from the diff (gate re-arm, preflight ordering) while
the TrackAdded "new track" claim is blocked by the new guard and untested. The
commit message and docs must not state guarantees the code does not implement.

**T11a can be marked done? No**, until:

1. either pre-validate `a=group`/`a=ice-lite` (or map all pre-mutation
   `accept_offer` errors to `InvalidOffer`) and add the live
   missing-BUNDLE test, **or** explicitly scope re-offers to BUNDLE SDP and
   narrow the docs (then it is a P2);
2. actually remove the wrong-peer gate re-arm (arm subscribers / request a
   keyframe) and add a real UDP gate test;
3. make the TrackAdded claim true end-to-end (allow appended m-lines or correct
   the claim) and add a test that reverts the filter;
4. move the real preflight (not just the helper) before `_db_lock`/storage and
   probe TEST-NET with the binary;
5. correct the stale module/implementation-doc guarantee.

## 10. Limitations

* No browser/Chromium e2e. The harness uses two genuine str0m clients over
  loopback UDP, which covers ICE-lite/DTLS/SRTP and the real registry, but not
  Chrome's exact re-offer shape.
* The missing-BUNDLE and echoed-`a=ice-lite` re-offers are malformed/non-browser
  SDP; they are authenticated-client re-offers, the same class as the previously
  reported swapped m-line order. The P1 severity reflects the stated general
  guarantee; a team that scopes re-offers to BUNDLE-only can treat it as P2.
* The queue-full `retryable` mapping was verified by code inspection plus the
  SFU unit test, not through a WebSocket client.
* The server full suite was run once on the pristine commit (green); both
  recorder tests were also run individually.
* Scratch artifacts under `/dev/shm` (`t11a_rr2`, `t11a_revert`, `str0m_patch`,
  probe configs/DBs, target cache) were reviewer-only and removed at the end;
  the repo contains only this report. The live `vks_data.db` was never opened
  (mtime/md5 unchanged).
