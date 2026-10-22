# WebRTC transport evidence — 2026-10-22

Command:

```
NODE_PATH=/home/pro/.npm/_npx/e41f203b7505f1fb/node_modules \
VKS_ARTIFACTS=/tmp/vks-webrtc-e2e \
VKS_CHROMIUM=/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome \
node tests/e2e/webrtc.js
```

Result: 29/29 checks passed, 0 fatal; commit `62b7187`, binary SHA-256 `400eb09fdaa6…` pinned
in `result.json`, effective server env (`VKS_WEBRTC_TRANSPORT=1`, ports
43000–43100) recorded there too. Two real Chromium peers through the
authenticated /ws, real str0m answers, ICE-lite host candidates on
192.168.1.93:43000/43001, DTLS-SRTP connected, inbound audio/video RTP and
decoded video frames on both pages; a native browser WebSocket with
`/ws?token=<JWT>` completes the ClientHello handshake (not just Node `ws`);
a third peer is rejected as `room_full`
(retryable) and can retry on the same socket; an `audio_only` subscriber gets
audio but zero video payload.

Key stats (from `result.json`):

- alice: DTLS connected, `SRTP_AEAD_AES_256_GCM`, inbound audio 618 pkts,
  inbound video 473 pkts / 185 frames, 320x240.
- bob: DTLS connected, `SRTP_AEAD_AES_256_GCM`, inbound audio 624 pkts,
  inbound video 334 pkts / 187 frames, 320x240.
- Third browser peer: explicitly rejected with `room_full` (`retryable=true`);
  a same-socket retry reaches `room_full` again, not `bad_state`, and the
  existing peers stay connected.
- `audio_only` subscriber: 131 audio packets, 0 video bytes / 0 decoded frames.
- Both answers: `a=ice-lite`, real 32-byte SHA-256 fingerprint (not
  `FF:00:11`), host candidate on 192.168.1.93:43000/43001, opus+VP8 only.

Legacy UI/API regression at the same HEAD: `tests/e2e/live.js` 233/233,
`ok=true`, `page_errors=[]` (`legacy-ui-result.json`). It does not exercise
`/ws` or RTCPeerConnection and is cited only as UI/API regression evidence.

Files: `result.json`, `alice-offer.sdp`, `alice-answer.sdp`, `bob-offer.sdp`,
`bob-answer.sdp`, `metrics.txt`, `server-log-tail.txt.

Mode-eviction coverage (separate pinned run, `TMPDIR=/var/tmp cargo test -p vks-admin evict`):
`mode-eviction-tests.txt` — 5/5 passed, including per-room/global/settings eviction
targeting and a regression test proving `Rooms::update` releases `state.rooms`
before the admission/eviction hook (no ABBA deadlock with gateway announce).
