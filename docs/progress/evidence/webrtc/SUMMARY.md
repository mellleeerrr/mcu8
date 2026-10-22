# WebRTC transport evidence — 2026-10-22

Command:

```
NODE_PATH=/home/pro/.npm/_npx/e41f203b7505f1fb/node_modules \
VKS_ARTIFACTS=/tmp/vks-webrtc-e2e \
VKS_CHROMIUM=/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome \
node tests/e2e/webrtc.js
```

Result: 24/24 checks passed, 0 fatal; two real Chromium peers through the
authenticated /ws, real str0m answers, ICE-lite host candidates, DTLS-SRTP
connected, inbound audio/video RTP and decoded video frames on both pages.

Key stats (from `result.json`):

- alice: DTLS connected, `SRTP_AEAD_AES_256_GCM`, inbound audio 300 pkts,
  inbound video 364 pkts / 113 frames, 320x240.
- bob: DTLS connected, `SRTP_AEAD_AES_256_GCM`, inbound audio 305 pkts,
  inbound video 202 pkts / 115 frames, 320x240.
- Third browser peer: explicitly rejected with `room_full`; existing peers stay connected.
- Both answers: `a=ice-lite`, real 32-byte SHA-256 fingerprint (not
  `FF:00:11`), host candidate on 192.168.1.93, opus+VP8 only.

Files: `result.json`, `alice-offer.sdp`, `alice-answer.sdp`, `bob-offer.sdp`,
`bob-answer.sdp`.
