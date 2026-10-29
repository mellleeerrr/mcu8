# T11a — certification re-review #6 (`2442a16`)

**Verdict: 9.0 / 10 — done: yes. 0 P0, 0 P1, 11 P2, 6 P3.**

The re-review #5 P1 is genuinely fixed by `ce4f45f`: `new_track_kinds` is now
called by the production `TrackAdded` loop (`crates/vks-server/src/lib.rs:1099`),
the helper is no longer dead code, and the `cloned_ref_to_slice_refs` lint in the
new test is gone. On a pristine `git archive 2442a16` the required clippy gate is
**green (exit 0)**, as is the CI-exact `--workspace --all-features --all-targets
--locked` clippy invocation. All other required gates pass too (`fmt`, server
178 tests, media-SFU 97, signaling 45), and the committed
`new_track_kinds_announces_only_unseen_kinds_once` test passes.

All five T11a acceptance criteria in
`docs/progress/MASTER_TASK_LIST_2026-10-29.md:50` remain met. The real-UDP
acceptance evidence from re-review #5 carries over verbatim: `webrtc.rs`,
`webrtc_room.rs` and the transport doc are **byte-identical** between `4686ac8`
and `2442a16` (md5 below), and no later commit touched them.

The score is **9.0, not 9.5**, because the one condition re-review #5 attached
to 9.5 beyond the P1 fix — a committed gate-re-arm/behavioural re-offer test —
was not added. That is a test-coverage gap (P2), not a functional defect, and it
does not block certification: the hard bar (0 P0/P1 + acceptance met) is now
met. Nothing in the residual list is an acceptance blocker.

Reviewed commit: `2442a16` (`docs(tracker): close T08 (9.6) and T09a (9.0); T09a
clippy P1 was T11a's, fixed in ce4f45f`), fix commit `ce4f45f`, on top of
`4686ac8`. Evidence came from `git archive 2442a16 | tar -x -C
/dev/shm/t11a_rr6_229326_1793266896`, `CARGO_TARGET_DIR=/home/pro/.cache/t11a_rr6_target`,
Rust 1.94.0 (pinned by the archived `rust-toolchain.toml`), all cargo runs
`--offline`.

| Severity | Count | Items |
|---|---|---|
| **P0** | **0** | — |
| **P1** | **0** | — (the old dead-code/clone clippy P1 is closed, §1) |
| **P2** | **11** | `TrackAdded` loop still has no behavioural test · hardcoded `TrackInfo` · documented str0m ICE mutation · **gate-re-arm fix untested** · early-preflight ordering untested/dev skip · queue-full retryable untested + substring heuristic · `WEBRTC_FALLBACK` on live re-offer · `SetSubscription` store-order · reply-before-timer-flush · H264 video publisher · no `TrackRemoved` |
| **P3** | **6** | mid guard anchored to initial offer · BUNDLE tags/`a=setup`/PT accepted (documented) · `InvalidOffer`→`Sdp` + stale log · duplicated trickle strip/double `handle_timeout` · process-global env in tests + candidate check only in `main` · `without_touching` test name / "before mutation" comment |

**Can T11a be marked done? Yes, at this HEAD.** 0 P0/P1, every acceptance
criterion verified, and the required gates reproduce green on a pristine archive.
The remaining P2/P3 are follow-ups (mostly tests and observability); the one that
keeps the score at 9.0 is the missing committed behavioural test for the
`TrackAdded` loop and the keyframe-gate re-arm (§4, P2-1/P2-4).

---

## 1. P1 closure: helper is wired, test is live, clippy is green

### 1.1 The fix itself

`ce4f45f` is a 4-line production change plus one test line:

```diff
-                                                    for track in tracks
-                                                        .iter()
-                                                        .filter(|track| !previous_kinds.contains(&track.media_kind))
+                                                    let new_kinds = new_track_kinds(&previous_kinds, &tracks);
+                                                    for track in
+                                                        tracks.iter().filter(|track| new_kinds.contains(&track.media_kind))
...
-        assert!(new_track_kinds(&[MediaKind::Audio], &[audio.clone()]).is_empty());
+        assert!(new_track_kinds(&[MediaKind::Audio], std::slice::from_ref(&audio)).is_empty());
```

Callers at HEAD (grep over `crates/`):

```
crates/vks-server/src/lib.rs:1099:  let new_kinds = new_track_kinds(&previous_kinds, &tracks);
crates/vks-server/src/lib.rs:1418:  fn new_track_kinds(
crates/vks-server/src/lib.rs:1441..1446: the unit test
```

So there is exactly one production call site, inside the `TrackAdded` loop, and
the helper (`lib.rs:1418-1431`) is no longer dead in non-test builds. The loop
semantics are unchanged for supported offers: `previous_kinds` still comes from
`publisher_track_kinds` (`lib.rs:1089`), and `tracks` still contains only
accepted audio/video media tracks, so "kinds in `new_kinds`" == "kinds not in
`previous_kinds`". No behaviour regression in the 4-line change.

### 1.2 The committed test still passes, and it does guard the helper logic

From the pristine server suite log:

```
test tests::new_track_kinds_announces_only_unseen_kinds_once ... ok
```

Revert experiment A (scratch copy only): drop the `!previous.contains(...)`
term from the helper, then run just that test:

```
assertion failed: new_track_kinds(&[MediaKind::Audio], std::slice::from_ref(&audio)).is_empty()
test result: FAILED. 0 passed; 1 failed; ... 148 filtered out
EXPERIMENT_A_EXIT=101
```

So the committed test genuinely fails if the "don't re-announce a previously
published kind" logic is removed. That part of re-review #5's criterion 2 is now
satisfied.

### 1.3 …but it still does not guard the production loop wiring

Revert experiment B (scratch copy only): restore the old inline filter in the
loop, keeping the helper and its test, then run the same test:

```
test tests::new_track_kinds_announces_only_unseen_kinds_once ... ok
test result: ok. 1 passed; 0 failed; ... 148 filtered out
EXPERIMENT_B_EXIT=0
```

A *complete* disconnect of the helper from production is nevertheless caught by
the required lint gate (clippy on the same reverted tree):

```
error: function `new_track_kinds` is never used
    --> crates/vks-server/src/lib.rs:1418:4
EXPERIMENT_B_CLIPPY_EXIT=101
```

So the wiring is protected by `dead_code` (i.e. by clippy `-D warnings`), but no
committed test exercises the WS loop → `TrackAdded` emission on a re-offer. A
semantic regression that keeps the helper referenced (e.g. filtering on the
wrong set) would not be caught by any test. This is the residual P2-1 below,
not a live defect: the code is correct by inspection for the only shapes
`validate_offer` admits (≤1 audio + ≤1 video m-line, `webrtc.rs:481-484`).

Note the test comment "appears exactly once even if the re-offer lists it twice"
(`lib.rs:1443`) is still not exercised: the test never passes two tracks of the
same new kind, and such an input is unreachable through the loop because
`validate_offer` rejects duplicate m-lines. That overclaim lives in a test
comment only and is folded into P2-1.

---

## 2. Gates on the pristine archive (`2442a16`)

All commands ran in `/dev/shm/t11a_rr6_229326_1793266896` with
`CARGO_TARGET_DIR=/home/pro/.cache/t11a_rr6_target`, Rust 1.94.0, `--offline`.
`Cargo.lock` in the scratch tree is byte-identical to `2442a16`'s
(`f48907f393813050455a3777dcd03d4e`), so `--locked` was safe; it is used only in
the CI-exact run below.

| Gate | Result |
|---|---|
| `cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets -- -D warnings` | **exit 0** — `Finished dev profile ... in 57.72s` |
| `cargo clippy --workspace --all-features --all-targets --locked -- -D warnings` (CI-exact) | **exit 0** — `Finished ... in 56.64s` |
| `cargo fmt --all -- --check` | **exit 0** (no output) |
| `cargo test -p vks-server --tests` | **exit 0** — **178 passed, 0 failed, 3 ignored** |
| `cargo test -p vks-media-sfu --features str0m` | **exit 0** — **97 passed, 0 failed** |
| `cargo test -p vks-signaling` | **exit 0** — **45 passed, 0 failed** |

Server-suite breakdown:

```
unittests src/lib.rs                 146 passed; 0 failed; 3 ignored
unittests src/main.rs                  0 passed
tests/config_failclosed.rs             1 passed
tests/h265_rtp.rs                      1 passed
tests/network_integration.rs          23 passed
tests/operator_policy.rs               7 passed
```

178, not the ~177 expected, is explained by the unrelated `916824b` (T07) test
change: it removed `frontend.rs::precompressed_gzip_is_served` and added
`compressible_static_is_gzipped_at_runtime` + `head_request_is_not_compressed`
(net +1 lib test). The T11a test is among the 146 and passed. No test emitted a
`dead_code` warning; no failure was observed in either server run (the suite was
run twice, both green, so the load-sensitive `gateways::media_runtime_tests`
flake noted in re-review #5 did not recur).

---

## 3. Acceptance criteria (row T11a, `MASTER_TASK_LIST_2026-10-29.md:50`)

| # | Criterion | Verdict | Evidence |
|---|---|---|---|
| 1 | Default transport = 1 + preflight (fail-closed) | **PASS** | `webrtc_runtime.rs:23-33`: env unset/empty → on, only `0/false/off/no` opt out; `main.rs:96-108` early preflight exits 78 before the DB lock (`:112`) and storage init (`:134`); `validate_candidate` rejects empty/invalid/TEST-NET/loopback without dev opt-in (`webrtc_runtime.rs:38-70`) |
| 2 | Keyframe gate | **PASS** | `webrtc_room.rs:576-584` `gate_video`; gate check at `:655`; drops counted as `awaiting_keyframe`; keyframe request rate-limited to 500 ms (`:659-661`); unit test `keyframe_gate_blocks_deltas_until_the_first_idr` (`:1051`) passes. Real-UDP observation from #5 (deltas blocked until first IDR; fresh keyframe forwarded) still applies — code identical |
| 3 | Re-offer without a new DTLS session | **PASS** | `webrtc_room.rs:671-695` answers through `PeerCommand::Reoffer` on the existing task; `webrtc.rs:205-250` rejects structural changes but keeps the session; real-UDP #5: `reoffered=true`, same server ufrag and candidate port, both peers still forwarding, appended m-line accepted on the same peer. Verified files byte-identical to #5 |
| 4 | VP9/AV1/H264/H265 caps advertised honestly | **PASS** | `webrtc.rs:138-143`: `clear_codecs().enable_opus(true,false).enable_vp8(true)`; answer test forbids `H264/90000`, `VP9/90000`, `AV1/90000`, `H265/90000` (`webrtc.rs:704-707`); transport doc `:20` states "only Opus/VP8 (+VP8 RTX)" |
| 5 | Docs accurate | **PASS** | No `before mutating` / `without touching` / `untouched` / `fails before` / `не трогает` strings in `crates/vks-media-sfu/src` implementation docs or `docs/implementation/` (only the underscored test name at `webrtc.rs:588`, already P3). Transport doc `:51-58` correctly states session reuse, rejected structural shapes, silently accepted BUNDLE-tag/`a=setup`/PT changes, and the `add_ice_details`-before-validation ufrag rotation. Files byte-identical to #5, where the str0m probe reproduced both the same-credential (no rotation) and new-credential (rotation) cases |

Plus this review's specific requirement: `new_track_kinds` is called by the
production loop (`lib.rs:1099`) and the committed test passes (§1). **PASS.**

Carry-over check for #5's real-UDP evidence (functional half), computed from git:

```
crates/vks-media-sfu/src/webrtc.rs                    781343f6e49a3e876b815ac50fc3f635  IDENTICAL
crates/vks-media-sfu/src/webrtc_room.rs               558f6bde79498d72fe74d08976d69e6f  IDENTICAL
docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md    3f06a052b8e3f172d635cafbc1a8b86c  IDENTICAL
```

Consequently #5's real-UDP results (BUNDLE-less and echoed `a=ice-lite`
re-offers rejected as `InvalidOffer` while A→B media continued; valid re-offer
reusing ufrag/port/session; appended video m-line accepted; no wrong-peer gate
re-arm) remain valid evidence for this HEAD. This review did not re-run a UDP
harness; that is acceptable because the reviewed files are byte-identical and
the harness itself was reviewer-only.

Docs/comments overclaim spot-check: the transport doc and the three key comments
(`webrtc.rs:110-113`, `:223-228`, `webrtc_room.rs:694-699`) still hedge
("may/can") and explicitly separate the surviving call from the rotated ufrag.
No regression since #5.

---

## 4. Residual issues

### P2 (11)

1. **`TrackAdded` loop still has no behavioural test.** The helper is now wired
   (`lib.rs:1099`), and the unit test guards its logic, but the test calls the
   helper directly: restoring the old inline loop filter leaves the test green
   (§1.3). No test drives a re-offer through the WS loop and asserts the number
   of `TrackAdded` broadcasts (0 for a same-tracks re-offer, 1 for a newly added
   kind). The dedupe branch is untested; a full disconnect is caught only by
   clippy's `dead_code`, not by a test. The test comment at `lib.rs:1443` also
   claims duplicate-new-kind coverage it does not exercise (unreachable through
   `validate_offer` anyway).
2. **`TrackInfo` is hardcoded.** `ssrc: 0` (`lib.rs:1122`), fixed 48 kHz/2 for
   audio and 90 kHz/0 for video (`:1123-1134`). A client cannot correlate the
   announced track (including an appended video track) with the real SSRC.
3. **A rejected re-offer still mutates str0m ICE state.** `webrtc.rs:223-231`
   calls str0m `accept_offer`; str0m applies `add_ice_details`
   (`str0m-0.24.1/src/change/sdp.rs:84`) before the consistency check
   (`:112`/`:947`). The call survives and media flows, but the server ufrag can
   rotate. Inherent str0m behaviour, reproduced in #5 and now documented in the
   transport doc and all inline comments; non-blocking.
4. **The keyframe-gate re-arm removal has no committed regression test.** This
   is the specific item re-review #5 named as the remaining 9.5 condition, and
   it is still absent: `waiting_keyframe.store` remains only at
   `webrtc_room.rs:438` (`set_subscription`), `:559` (queue-full) and `:729`
   (`SetSubscription` arm); the success re-offer arm (`:671-695`) contains no
   store, but `keyframe_gate_blocks_deltas_until_the_first_idr` only tests the
   pure predicate, and `reoffer_is_answered_by_the_existing_peer_task` asserts
   session/peer count, not gate state. A re-introduced re-arm would be caught by
   the reviewer-only harness from #5, not by `cargo test`. Test-coverage gap
   only; the removal itself is verified in code and was verified at runtime in
   #5.
5. **Early-preflight ordering is untested, and dev configs skip it.**
   `main.rs:98-107` runs the early check only when `!allow_insecure_defaults`;
   the DB lock is at `:112-115` and `SqliteStorage::init` at `:134`. No
   automated test asserts "exit 78 without creating the DB" for a dev config.
6. **Queue-full `webrtc_offer_retry`/`retryable=1` is untested.**
   `webrtc_room.rs:212` returns `Sdp("re-offer queue is full; retry")`;
   `lib.rs:978-980` maps any SDP error containing `"retry"` to
   `webrtc_offer_retry` and `:1010-1014` makes it retryable. No signalling-layer
   test drives the queue-full path; the substring heuristic is brittle.
7. **`WEBRTC_FALLBACK{offer_rejected}` counts a live peer's rejected re-offer.**
   `lib.rs:1016-1018` increments the legacy-fallback metric on any
   `webrtc_failure`, including a rejected re-offer while the call continues.
8. **`SetSubscription` store-order window.** `webrtc_room.rs:420-439` updates
   the map and the atomics, then `try_send`s; a full queue leaves the state
   updated without the task seeing the command. No test covers it.
9. **Re-offer reply before the peer's timer actions are flushed.** The oneshot
   reply is sent at `webrtc_room.rs:676-681`, then `handle_timeout` runs at
   `:683-690`, and `run_peer` calls it again (`:742-748`). The answer can reach
   signalling before local ICE/DTLS actions are transmitted, and the timer runs
   twice.
10. **An H264 video publisher (with Opus audio) is accepted.** `validate_offer`
    (`webrtc.rs:462-496`) only requires Opus or VP8 anywhere in the SDP; an
    Opus-audio + H264-video offer passes, then the video m-line is answered
    rejected while the publisher believes video works. The SFU advertises only
    Opus/VP8.
11. **No `TrackRemoved` on re-offer track removal.** No `Payload::TrackRemoved`
    is emitted from `crates/vks-server/src/lib.rs`; only
    `vks-signaling/src/handler.rs:233` recognizes the payload.

### P3 (6)

1. The mid guard is anchored to the initial offer's `remote_mids`
   (`webrtc.rs:110-113`, set at `:185`, never updated after a successful
   re-offer), so appended-section renames/reorders bypass it and are caught only
   by str0m (non-fatal `InvalidOffer`).
2. The offer's `a=group:BUNDLE` tags and `a=setup` changes are not validated
   (`webrtc.rs:462-496`); changed BUNDLE tags, `a=setup:passive` and changed
   Opus PT are silently accepted. The transport doc states this explicitly
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
   for a parse-error input but read more broadly than the `add_ice_details` case.
   Renaming would remove the last "without touching" string from the source; not
   an overclaim in its own context.

No new P2/P3 was introduced by `ce4f45f` (4-line diff, §1.1) or by the
intervening `916824b`/`2442a16` (they do not touch the SFU transport or the
`TrackAdded` loop).

---

## 5. Score: 9.0, and why 9.5 is still not justified

Re-review #5 stated the exact condition for 9.5: "If the helper is wired into
the loop, clone fixed, clippy green, **and a committed gate-re-arm test is
added**, this same evidence would justify 9.5."

Three of those four are now true and independently reproduced here:

* helper wired into the production loop — yes (`lib.rs:1099`);
* clone lint fixed — yes (`std::slice::from_ref`);
* clippy green — yes, both the required three-crate gate and the CI-exact
  workspace/all-features gate, plus `fmt`;
* committed gate-re-arm test — **no**. The gate-re-arm fix (the removal of the
  re-offer `waiting_keyframe.store`) still has no repository test; the only
  evidence is code inspection plus the reviewer-only real-UDP harness from #5.
  The `TrackAdded` loop is likewise still without a behavioural test.

The functional acceptance scenario is met, and the P1 is genuinely closed, so
`done: yes` is correct: the project rule is "no P0/P1 + acceptance met", and a
P2 test-coverage gap does not block certification (nor does it block the
existing P2 list from #5, which was already non-blocking). But awarding 9.5 on
this evidence would contradict the explicit, still-unmet condition from the
previous review without any new evidence to justify relaxing it. A score of 9.0
reflects: all acceptance criteria verified on a pristine archive, all required
gates green, the P1 fixed, and two concrete test-coverage gaps (P2-1 and P2-4)
that are the difference between "certifiable" and "excellent".

9.5 becomes defensible with exactly two additions, neither of which is a
functional fix:

1. a committed test that drives a re-offer through the WS/signalling loop (or a
   lower-level equivalent) and asserts `TrackAdded` is emitted 0 times for a
   same-tracks re-offer and exactly once for a newly added kind;
2. a committed test asserting the gate is not re-armed by a successful re-offer
   (a delta after the re-offer is delivered without a new keyframe).

---

## 6. Hygiene

* **Pristine evidence only.** Every command above ran in
  `/dev/shm/t11a_rr6_229326_1793266896`, created by
  `git archive 2442a16 | tar -x`. The live tree was read only (git log/diff/show,
  `stat`/`md5sum` on the DB); it was never used to build or test.
* **Live DB untouched.** `/home/pro/Рабочий стол/mcu/vks_data.db` was not
  opened, copied, locked or migrated by this review. At the start and end of the
  evidence run: `3 248 128` bytes, mtime `2026-10-29 12:38:31.642604976 +0300`,
  md5 `a925e7fa52831ed3aa8480c0e9531ff0` (unchanged). The live server may write
  its own DB; no reviewer process did.
* **No live services touched.** Nothing connected to :8080 or :18082; no server
  was started or restarted; no UDP ranges were bound outside the pristine test
  binaries (tests use loopback/ephemeral ports).
* **HEAD moved concurrently.** While this review ran, another task advanced the
  live branch from `2442a16` to `0febfe5` (`fix(T06): one occupancy predicate…`).
  The reviewed archive is exactly `2442a16`; `git diff --name-only 2442a16
  0febfe5` shows no changes to `crates/vks-media-sfu`, `crates/vks-server/src/lib.rs`
  or `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md`, so this verdict applies
  unchanged to the T11a surface at `0febfe5`. Other tasks' gates at `0febfe5`
  are out of scope here.
* **Scratch removed.** The archive tree, its logs/backup copies and
  `/home/pro/.cache/t11a_rr6_target` were deleted before this report was
  finalised; the `/dev/shm` dir file pointer was removed too.
* **Only one file written:** this review,
  `docs/audit/2026-10-29/reviews/T11a_rereview6.md`. No source edits, no other
  files.
