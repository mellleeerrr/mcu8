# T11a — narrow certification re-review #4 (`57c2b4b`)

**Verdict: 9.0 / 10 — done (0 P0, 0 P1).** The docs commit removes the
"without touching the live session" overclaim from the two cited documents and
now states the real side effect correctly: a rejected re-offer keeps the peer,
UDP socket, DTLS and media alive, but str0m may already have applied the
offer's ICE credentials, so the next accepted re-offer can observe a rotated
server ufrag. Real-UDP evidence at this HEAD reproduces exactly that: BUNDLE-less
and echoed-`a=ice-lite` re-offers return `InvalidOffer`, the two-peer call keeps
forwarding media, the next valid re-offer reuses the same session/ufrag/socket,
and an appended video m-line is accepted. The gate re-arm block is absent and a
live probe now catches it if re-introduced. **However, the doc fix is not
complete**: three inline source comments still say str0m "fails before mutating"
/ the session is "untouched", and the rejected-re-offer ICE mutation itself is
still present. The regression-test gaps (gate, `TrackAdded`, preflight ordering,
queue-full retryable) also remain. This is why the score stays at 9.0 and is not
9.5.

Reviewed commit: `57c2b4b` (`docs(webrtc): T11a — document ICE side effect of
rejected re-offers`), six commits after the prior review's `661a2cb`. The commit
touches only `crates/vks-media-sfu/src/webrtc_room.rs` (module doc) and
`docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md`; `git diff 661a2cb 57c2b4b
-- crates/vks-media-sfu/src` is documentation-only, so all prior code evidence
applies unchanged. Evidence came from a pristine `git archive 57c2b4b` copy at
`/dev/shm/t11a_rr4`, a reviewer-only real-UDP harness at
`crates/vks-media-sfu/tests/t11a_rr4_harness.rs`, and the required gates on the
same archive. The live tree was not used for evidence and the live
`vks_data.db` was never opened; its size/mtime are unchanged (`3 248 128` bytes,
`2026-10-29 07:33:29.801017417 +0300`). Scratch copies are removed at the end.

| Severity | Count | Items |
|---|---|---|
| **P0** | **0** | — |
| **P1** | **0** | — |
| **P2** | **11** | `TrackAdded` e2e gap · hardcoded `TrackInfo` · rejected re-offer still mutates ICE + stale inline comments · gate fix untested · early-preflight ordering untested/dev skip · queue-full retryable untested + substring heuristic · `WEBRTC_FALLBACK` on live re-offer · `SetSubscription` store-order · reply-before-timer-flush · H264 video publisher · no `TrackRemoved` |
| **P3** | **5** | mid guard anchored to initial offer · BUNDLE tags/`a=setup` not validated · `InvalidOffer`→`Sdp` downgrade + stale log · duplicated trickle strip/double `handle_timeout` · process-global env in tests + candidate check only in `main` |

**T11a can be marked done? Yes.** 0 P0 / 0 open P1, every item in this
re-review's checklist was verified at `57c2b4b` with running code or by
re-running the exact prior probe on an unchanged code path, and no acceptance
blocker was found.

---

## 1. Docs: the cited claims are fixed, the inline comments are not

### 1.1 `webrtc_room.rs` module doc

At `57c2b4b`, `crates/vks-media-sfu/src/webrtc_room.rs:7-12` now reads:

> Signalling awaits [`WebRtcRoomRegistry::accept_offer`]: a re-offer is
> forwarded to the peer's task (same str0m session, no new socket/DTLS), a
> brand-new offer is registered under the process-wide admission lock
> re-validated after the await. A re-offer the peer cannot negotiate is rejected
> and the live call keeps flowing (peer, socket, DTLS and media survive), but
> str0m may have already applied the offer's ICE credentials before rejecting
> the SDP body, so the next accepted re-offer can observe a rotated server
> ufrag.

This is accurate and appropriately hedged ("may have already applied… can
observe"). It no longer claims the session is untouched.

### 1.2 `WEBRTC_TRANSPORT` doc

`docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:51-58` now reads:

> Re-offer переиспользует ту же str0m-сессию (ICE/DTLS ключи и сокет
> сохраняются); несовместимый re-offer отклоняется, не разрывая живой звонок
> (пир, сокет, DTLS и медиа продолжают работать). Важная деталь: str0m
> применяет ICE-креды оффера (`add_ice_details`) до валидации SDP, поэтому
> отклонённый re-offer с новым ufrag/pwd может ротировать серверный ufrag —
> следующий валидный re-offer увидит новое значение. Это не разрыв звонка и
> не новый сокет/DTLS; решение о форме комнаты перепроверяется под
> admission-lock после await.

This is also accurate: the call/socket/DTLS/media survive, the ICE credential
side effect is called out, and the "no new socket/DTLS" claim is true. The
`не разрывая живой звонок` phrase is now immediately qualified by the ICE
detail; it is not an overclaim in the sense the prior review flagged.

### 1.3 Confirmation against running code

The code path is unchanged since `661a2cb`, and str0m 0.24.1 still runs
`add_ice_details(self.rtc, &offer, None)?` at
`str0m-0.24.1/src/change/sdp.rs:84` before
`apply_offer(&mut self.rtc.session, offer)?` at `:112`, whose first statement
is `offer.assert_consistency()?` at `:947`. The new docs describe this ordering
exactly. The real-UDP probe in §2 observed the two halves:

* same-credential BUNDLE-less / ice-lite rejection → next valid re-offer kept
  the same server ufrag and candidate port (`ufrag_same=true`, `port_same=true`);
* new-credential BUNDLE-less rejection → the next valid re-offer produced a
  different server ufrag (`bEvlSOPoL3bYXghm` → `tE7EaEEZCIn3yuDF`), exactly as
  the docs now warn.

### 1.4 Remaining overclaims elsewhere

The grep is not clean; the public docs are fixed, but three source comments
still state the old, false version. These are developer-facing documentation
and contradict the new module/transport docs:

* `crates/vks-media-sfu/src/webrtc.rs:110-112` (`remote_mids` field doc):
  "…a re-offer must keep the same m-line order (**str0m rejects changes before
  mutating anything**, and we classify that as a rejected re-offer rather than
  a dead call)." FALSE for `add_ice_details`, which runs before the
  consistency check.
* `crates/vks-media-sfu/src/webrtc.rs:223-226` (`accept_reoffer`):
  "str0m's `accept_offer` validates the new SDP against the existing remote
  description and **fails before mutating it** (verified: answer and a
  subsequent valid re-offer still work)." The "verified" observation is true
  for the answer text but does not cover the ICE state, which is already
  mutated.
* `crates/vks-media-sfu/src/webrtc_room.rs:696-698` (`Reoffer` /
  `InvalidOffer` arm): "The live **session is untouched**: reject this re-offer
  only." FALSE when the rejected offer carried new ICE credentials; the peer,
  socket, DTLS and media survive, but the ICE agent has been restarted.

Also still overbroad (carried, not introduced here):
`WEBRTC_TRANSPORT_2026-10-22.md:52` says "несовместимый re-offer отклоняется",
but changed `a=group:BUNDLE` tags, changed `a=setup` and changed Opus payload
types are accepted (prior live probes on the unchanged code; §5, P3-2). That is
a rejection-semantics overclaim, separate from the ICE side effect.

The test name `malformed_reoffer_is_rejected_without_touching_the_live_session`
(`webrtc.rs:586`) is accurate for its own input (`t=0 1`, a parse error that
does happen before mutation) and is not counted as an overclaim; it simply does
not exercise the `add_ice_details`-then-reject case.

Historical audit reports (`docs/audit/.../T11a.md:64`, etc.) still contain the
old wording; they are superseded snapshots, not current implementation docs.

---

## 2. Real-UDP evidence at `57c2b4b`

Reviewer-only harness (never committed): two genuine str0m 0.24.1 clients bind
loopback UDP sockets, send real offers through
`WebRtcRoomRegistry::accept_offer`, complete ICE/DTLS/SRTP against the real peer
tasks, and exchange VP8 frames A→SFU→B and B→SFU→A. Malformed re-offers are
sent on the same registry path with the same participant. Run on the pristine
archive with `cargo test -p vks-media-sfu --features str0m --test
t11a_rr4_harness -- --nocapture --test-threads=1`:

```
HANDSHAKE: A ufrag=ACFKy97bLxz4G4Xz port=50914
BASELINE: B video_rx=2 audio_rx=0
MISSING a=group:BUNDLE -> SDP: re-offer rejected by str0m: SDP inconsistent: Session attribute a=group missing
AFTER MISSING BUNDLE: B video_rx=3
ECHOED a=ice-lite -> SDP: re-offer rejected by str0m: remote sdp: Both peers being ICE-Lite not supported
AFTER ICE-LITE ECHO: B video_rx=4
VALID RE-OFFER: reoffered=true ufrag_same=true port_same=true
AFTER VALID RE-OFFER: B video_rx=5 (A->B delta delivered)
B->A DELTA RIGHT AFTER A's VALID RE-OFFER: received=1 (gate not re-armed)
```

Interpretation:

* both malformed offers are rejected as `InvalidOffer` inside `WebRtcPeer`
  (`webrtc.rs:227-231`), and the registry maps them to `WebRtcError::Sdp` at
  `webrtc_room.rs:236-238` for the signalling layer; no peer is replaced
  (`peer_count == 2`, `has_peer(A) == true` after each rejection);
* a fresh keyframe A→B is still forwarded after each rejection, so the real
  SRTP/fanout path is alive;
* the next valid re-offer is answered by the existing peer task
  (`reoffered=true`), with the same server ufrag and candidate port, i.e. the
  same `Rtc` and UDP socket;
* the B→A delta immediately after A's valid re-offer is delivered, so the
  removed wrong-peer gate re-arm is not present at runtime.

The harness is sensitive to the gate regression: re-adding the removed block
(`if reoffer.publishes_video { handle.waiting_keyframe.store(true, …) }` in the
registry success arm) makes this exact harness fail at the B→A delta assertion
(2 passed / 1 failed at `t11a_rr4_harness.rs:230`); the pristine tree passes.
That is live verification of the fix, not a committed regression test.

The separate bare-peer probe for the ICE side effect (same HEAD, no UDP
needed):

```
NEW ICE + MISSING BUNDLE -> InvalidOffer: re-offer rejected by str0m: SDP inconsistent: Session attribute a=group missing
ufrag before=bEvlSOPoL3bYXghm after=tE7EaEEZCIn3yuDF
```

This confirms the docs' "may rotate the server ufrag" statement and confirms
the mutation is still there; it is low-likelihood input (a browser does not send
an ICE restart inside a BUNDLE-less SDP), so P2, not P1.

---

## 3. Keyframe gate: block absent, and the live probe catches it

`grep -n waiting_keyframe crates/vks-media-sfu/src/webrtc_room.rs` at
`57c2b4b` leaves only:

* `:438` — `set_subscription`: a subscriber turning video on is gated
  synchronously, before queued writes (correct);
* `:559` — fanout queue-full: the target subscriber that lost frames is gated
  and the publisher is asked for an IDR (correct);
* `:727` — the `SetSubscription` command arm: re-subscription re-arms the gate
  (correct);
* `:655-656` — the actual `gate_video` check on the `Write` path.

There is no `waiting_keyframe.store` in the `Reoffer` success arm
(`webrtc_room.rs:671-692`), and no `reoffer.publishes_video` condition arms a
gate there (the only `reoffer.publishes_video` use is the registry's
`current.publishes_video = true` update at `:220-226`). The prior re-review's
removed block is gone. The live evidence in §2 (B→A delta delivered immediately
after A's valid re-offer) confirms the user-visible effect.

Committed coverage is still missing: re-adding the block leaves the SFU suite
green (prior revert experiment; code unchanged here), and no test in
`webrtc_room.rs` asserts a delta after a re-offer. P2-4 remains.

---

## 4. Appended m-line through the real registry

A real str0m client joins audio-only, then performs a real SDP re-offer with a
video m-line appended (same session, same ICE credentials/fingerprint). The
same registry path answers:

```
APPENDED M-LINE: reoffered=true answer_mids=2 rtp_maps=[(111, "opus", 48000, 2), (96, "vp8", 90000, 0)]
```

So the full registry path accepts the appended section, keeps `peer_count == 1`
(no second socket/peer), returns one audio and one video m-line, negotiates VP8,
and the client remains connected after `accept_answer`. The committed
`audio_only_peer_can_append_a_video_section_on_reoffer` (`webrtc.rs:627`) still
covers the bare-peer case and passes in gate 2. The announcement/`TrackInfo`
side is not covered end-to-end (P2-1/P2-2).

---

## 5. P2 / P3 inventory at `57c2b4b`

None of the items below is P0 or P1. All are follow-ups: low-likelihood input,
observability, or missing tests.

### P2 (11 open; P2-3's public-doc half fixed, behavior open)

1. **`TrackAdded` announcement has no end-to-end regression test.** The filter
   is correct by inspection (`crates/vks-server/src/lib.rs:1099-1101`,
   `previous_kinds` from `publisher_track_kinds`), but the only wire tests are
   the legacy plaintext path (`crates/vks-server/tests/network_integration.rs:1006-1009`,
   `:1229-1232`). Prior revert (`filter(|_| true)`) left 176/176 green; no new
   e2e test exists at this HEAD.
2. **`TrackInfo` is hardcoded.** `crates/vks-server/src/lib.rs:1117-1140`,
   `ssrc: 0` at `:1122`, fixed 48 kHz/2 or 90 kHz/0. A client cannot correlate
   the announced track (including an appended video track) with the actual SSRC.
3. **A rejected re-offer still mutates str0m ICE state.** `webrtc.rs:227-231`
   calls str0m `accept_offer`; str0m applies `add_ice_details` (`sdp.rs:84`)
   before `apply_offer`'s consistency check (`:112`, `:947`). The call survives
   and media keeps flowing, but the server ufrag can rotate. The public docs now
   describe this; the three inline comments in §1.4 still deny it.
4. **Gate-re-arm removal has no committed regression test.** Live probe in §3
   catches a re-introduced block, but no repository test does.
5. **Early-preflight ordering is untested, and dev configs skip it.**
   `crates/vks-server/src/main.rs:98-107` runs the early check only when
   `!allow_insecure_defaults`; the DB lock is at `:112-115` and
   `SqliteStorage::init` at `:134`. A dev config with a TEST-NET candidate
   still creates the SQLite file before the late preflight (`:426`) exits 78.
   No automated test asserts "exit 78 without creating the DB".
6. **Queue-full `webrtc_offer_retry`/`retryable=1` is untested.** The full
   command queue returns `WebRtcError::Sdp("re-offer queue is full; retry")`
   (`webrtc_room.rs:212`); `crates/vks-server/src/lib.rs:978` maps any SDP error
   containing `"retry"` to `webrtc_offer_retry`, and `:1010-1014` makes it
   retryable. The only e2e assertion is `room_full`
   (`tests/e2e/webrtc.js:413`); no test drives the queue-full path at the
   signalling layer, and the substring heuristic is brittle.
7. **`WEBRTC_FALLBACK{offer_rejected}` is incremented for a live peer's
   rejected re-offer.** `crates/vks-server/src/lib.rs:1016-1018` increments the
   legacy-fallback metric on any `webrtc_failure`, including a re-offer that is
   rejected while the live call continues; this pollutes the fallback signal.
8. **`SetSubscription` store-order window.** `webrtc_room.rs:418-439` writes
   the subscriptions map, then the two atomics, then `try_send`s the command;
   a full queue leaves the map/atomics updated without the task seeing the
   command. No test covers it.
9. **Re-offer reply before the peer's timer actions are flushed.** The oneshot
   reply is sent at `webrtc_room.rs:676-680`, then `handle_timeout` is called at
   `:683-690`; the outer `run_peer` calls `handle_timeout` again at `:740-748`.
   The re-offer path therefore runs the timer twice and the answer can reach
   signalling before local ICE/DTLS actions are transmitted.
10. **H264 video publisher (with Opus audio) is accepted.** `validate_offer`
    (`webrtc.rs:462-496`) only requires Opus or VP8 anywhere in the SDP; an
    Opus-audio + H264-video offer passes validation, then the video m-line is
    answered rejected, so the publisher believes video works. The SFU advertises
    only Opus/VP8.
11. **No `TrackRemoved` on re-offer track removal.** No
    `Payload::TrackRemoved` is emitted from `crates/vks-server/src/lib.rs`
    (only `vks-signaling/src/handler.rs:233` recognizes the payload).

### P3 (5 carried)

1. The mid guard is anchored to the initial offer's `remote_mids`
   (`webrtc.rs:110-113`, set at `:185` and never updated after a successful
   re-offer), so renames/reorders of appended sections bypass our guard and are
   only caught by str0m (non-fatal `InvalidOffer`).
2. The offer's `a=group:BUNDLE` tags and `a=setup` changes are not validated
   (`validate_offer` at `webrtc.rs:462-496`); prior live evidence accepted
   changed BUNDLE tags, `a=setup:passive` and a changed Opus PT. This is also
   why the transport doc's "несовместимый re-offer отклоняется" is still broad.
3. `InvalidOffer` is downgraded to `WebRtcError::Sdp` at the registry boundary
   (`webrtc_room.rs:236-238`), losing the typed cause; the adjacent log says
   "keeping old session" even when the ICE state was already touched.
4. Duplicated `a=ice-options:trickle` stripping (`webrtc.rs:161` and `:234`)
   and the double `handle_timeout` on the re-offer path (P2-9) — cosmetic plus
   the timer issue.
5. Process-global env mutation in server tests
   (`network_integration.rs:121`, `VKS_WEBRTC_TRANSPORT=0` in a `Once`;
   `:2226`, `VKS_METRICS_TOKEN`), and the candidate check exists only in
   `main` (`main.rs:98`), so library embedders get a `None` registry instead of
   a startup error.

---

## 6. Gates on the pristine archive

All four required gates ran on `git archive 57c2b4b` at `/dev/shm/t11a_rr4`
with `--locked`; the reviewer harness was removed before the gate run.

| Gate | Result |
|---|---|
| `cargo test -p vks-server --tests` | **exit 0** — 144 lib + 1 `config_failclosed` + 1 `h265_rtp` + 23 `network_integration` + 7 `operator_policy` = **176 passed, 0 failed, 3 ignored** |
| `cargo test -p vks-media-sfu --features str0m` | **exit 0** — **97 passed, 0 failed** |
| `cargo test -p vks-signaling` | **exit 0** — **45 passed, 0 failed** |
| `cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets -- -D warnings` | **exit 0**, no warnings |

The two committed re-offer regression tests are present and pass in gate 2:
`reoffer_rejected_by_str0m_keeps_the_session` and
`audio_only_peer_can_append_a_video_section_on_reoffer`. They do not cover the
gate re-arm, the `TrackAdded` announcement, preflight ordering, or queue-full
retryable.

---

## 7. Score: why 9.0, not 9.5

The necessary condition for 9.5 is met: 0 P0, 0 open P1, the call survives a
rejected re-offer under real UDP, the next valid re-offer reuses the session,
appended m-lines are accepted, and all gates are green. But the prior rubric
for 9.5 also requires no material P2s and a fully consistent doc set. Both are
short of that:

* the rejected-re-offer ICE mutation is still real (P2-3), only now documented
  in the two public docs while three source comments still claim the opposite;
* the gate fix and the `TrackAdded` announcement have no repository regression
  test (P2-1/P2-4), so a future revert would be invisible to CI;
* `TrackInfo` is still unusable for SSRC correlation (P2-2);
* the preflight-ordering and queue-full retryable paths are untested, and the
  retry mapping is a substring heuristic (P2-5/P2-6).

None of these affects the acceptance scenario just verified with real UDP, which
is why the task is done; they are exactly the reason the score does not move
from 9.0. **9.5 is not justified** on this evidence.

---

## 8. Caveats and cleanup

* The harness uses two str0m 0.24.1 clients over loopback UDP, not Chromium;
  Chrome's exact re-offer SDP shape (mids, BUNDLE tags, DTLS setup) is not
  reproduced.
* The malformed offers are authenticated-client inputs; browsers do not send
  them. The ICE mutation is therefore low-likelihood but real.
* Media checks use synthetic VP8 frames with the P bit set (keyframe `0x10`,
  delta `0x11`), not a real encoder; SRTP forwarding, depayload/keyframe
  classification and the fanout are real.
* The changed-BUNDLE-tags / `a=setup` / PT acceptance evidence is carried from
  the prior review; the relevant code is unchanged between `661a2cb` and
  `57c2b4b` and was not re-probed here.
* The live `vks_data.db` was not opened; its size/mtime remain
  `3 248 128` / `2026-10-29 07:33:29.801017417 +0300`, unchanged before/after.
* Reviewer scratch (`/dev/shm/t11a_rr4`, the harness file, the gate log, the
  temporary `webrtc_room.rs` backup) was removed after this report. The repo
  contains only this report; the live tree was not modified.
