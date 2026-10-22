# vks-gateway-h323

Outbound H.323 calls use a separate Python ASN.1/PER process. H.225 CONNECT,
H.245 capability exchange, master/slave determination and both audio logical
channels must succeed before `H323UacClient::dial*()` returns a participant.
Timeouts, ALERTING and TCP closure are errors. No RTP port is guessed.

## Runtime setup

Python 3 with the separate hash-pinned Pycrate 0.8.1 package is required.
On installations with pip/ensurepip:

```sh
python3 -m venv "$HOME/.local/share/vks-mcu/h323-venv"
"$HOME/.local/share/vks-mcu/h323-venv/bin/python3" -m pip install \
  --require-hashes --only-binary=:all: -r crates/vks-gateway-h323/python/requirements.lock
```

The minimal VM Python has no ensurepip. The equivalent stdlib-only installer
creates `venv --without-pip`, verifies the same official wheel SHA256, and installs
only its library modules/license metadata into that venv:

```sh
python3 crates/vks-gateway-h323/python/install_runtime.py
```

`VKS_H323_PYTHON` overrides the Python executable. By default the UAC checks
`$HOME/.local/share/vks-mcu/h323-venv/bin/python3`, then uses `python3`.
`VKS_H323_HELPER` overrides the helper path. Source-tree builds use
`<crate>/python/h323_sidecar.py`; a binary deployment must include that helper,
`h323_listener.py`, `h323_audio.py` and `h323_asn.py`, and point the environment
variable to the installed helper.
See [codec provenance](python/PROVENANCE.md) for separate dependency licensing,
input schemas, regeneration and protocol references.

## Media contract

```rust,ignore
client.dial_with_media(target, local_audio_rtp_port, Some(local_video_rtp_port), timeout).await
```

The caller owns/reserves each advertised RTP/RTCP port pair. The helper does not
bind or transform media. It reports negotiated remote endpoints and separate
outgoing/incoming payload types to the Rust media bridge.

Supported outbound negotiation:

- Direct IPv4 H.225 TCP, separate H.245 TCP control; no gatekeeper/RAS requirement.
- Native symmetric audio: G.722 (PT9), G.711U (PT0), G.711A/PCMA (PT8),
  G.723.1 (PT4, maxAl-sduAudioFrames=1, no silence suppression), all with
  static H.245 payload types. Dynamic audio PTs and G.728 are rejected.
- Optional H.264 baseline capability, level 3.1, H.241 non-interleaved RTP
  packetization, negotiated dynamic PTs in each direction. Video is reported only
  when both logical channels negotiate. It also requires the server's H.264 media
  bridge, encoder and decoder; successful signaling alone proves no decoded video.
- H.245 round-trip requests and remote end-session/channel closure remain active
  throughout the call. H.225 RELEASE COMPLETE is observed independently.

The initial implementation rejects non-IPv4, H.245 tunneling-only peers,
unsupported/reverse logical channel forms, and media/control addresses different
from the dialed endpoint. It does not advertise encryption, G.729, FECC, H.239
presentation, gatekeeper registration or transcoding that is not running.

## Lifecycle integration

Keep `ActiveH323Session` for the lifetime of the call. Subscribe immediately with
`subscribe_disconnect()` and remove the participant/media bridge when the watch
value becomes `Some(reason)`. `is_closed()` is atomic. Explicit `hangup()` is
idempotent and waits for the child process to close. Dropping the last session
owner or cancelling a pending dial requests end-session/RELEASE through stdin;
a bounded supervisor kills/reaps an unresponsive helper. Stdout is bounded JSON
IPC; stderr is independently drained with bounded retained diagnostics.

## Verification

```sh
cargo test -p vks-gateway-h323
"$HOME/.local/share/vks-mcu/h323-venv/bin/python3" -m unittest discover \
  -s crates/vks-gateway-h323/python -p test_h323_sidecar.py -v
```

Python tests include independent Polycom CONNECT/OLC/ACK wire captures,
fragmentation, malformed/private-format rejection and negotiation state gates.
Rust tests cover IPC size limits, ringing/EOF failures, remote disconnect,
graceful hangup and cancellation cleanup. These tests place no real calls.

`call`, `codec` and `ffi` still contain the earlier deterministic core/FFI mocks;
they are not a running H323Plus/OPAL implementation. The `h323plus` feature alone
does not provide a native telephony stack. Production outbound UAC uses the
explicit sidecar path above.
