# T06b-8 live AAC-LATM proof (2026-10-23 07:11–07:13 MSK)

Environment: host 192.168.1.93. The first (single-peer and two-peer) runs used
release binary `4f551af9edcf27ce` (HEAD `08ecd1a`); the decisive attribution
run used `90672b97a2579f3f` (HEAD `cc93100`). Each run temporarily set
`VKS_AAC_LATM_LEGAL_ACCEPTED=1` and restarted the service; `/readyz=200`, log:
`gateway audio codec availability ... aac_latm=true aac_latm_legal=true` at
04:10:01Z and 04:50:47Z.

Test rig: `/var/tmp/aac_uas.py` — minimal UDP SIP UAS on port 5090 that
answers the MCU offer with AAC-LATM only:

```
m=audio 5090 RTP/AVP 113
a=rtpmap:113 MP4A-LATM/90000
a=fmtp:113 profile-level-id=1;object=2;cpresent=1
```

It sends 210 LATM RTP frames of a 1 kHz tone (FFmpeg `-smc-interval 1`,
48 kHz mono, 64 kbps) and decodes every RTP packet received from the MCU.

Session: conference/session `23bc65cf-f1ae-48a7-8197-7f5653339b4c`,
dial `sip:127.0.0.1:5090` via
`POST /api/v3/ui/sessions/{id}/call_strings`.

## Result

Server log (`server.log`):

```
SIP dialog confirmed ... audio_port=Some(5090) audio_codec="MP4A-LATM" video_codec=None
Outbound gateway call connected ... rtp_port=41000 has_video=false protocol="SIP"
AAC-LATM audio bridge started ... payload_type=113 encoder_pid=... decoder_pid=...
AAC-LATM audio bridge stopped ...   (73 s later, normal session teardown)
```

No `AAC RTP rejected by source validator` and no
`AAC decoder produced no PCM` lines for this call: the inbound tone was
accepted and decoded (the rolling 3 s liveness would have torn the bridge
down otherwise).

UAS log (`uas.log`):

```
sent 200 OK with MP4A-LATM/90000
ACK received; starting RTP
rx RTP #1   pt=113 marker=True seq=... len=28
rx RTP #25  pt=113 marker=True ...
rx RTP #100 pt=113 marker=True ...
decoded rx: samples=122880 rms=0.0000 nonzero=0
done: rx_packets=2706 sent_started=True
rx pts={113} markers={True}
first_ts_deltas=[1920, 1920, ... x20]
```

- MCU→UAS: 2706 RTP packets, every one PT113, marker=1, timestamp step
  exactly 1920 (90 kHz = 1024 AAC samples), first packet carries the SMC
  (28-byte LOAS payload), later silence frames 11 bytes.
- The UAS reassembled LOAS and decoded 122 880 samples (2.56 s) with
  FFmpeg 8.0.1, proving the MCU outbound AAC-LATM framing is decodable.
  RMS is 0 because the room had a single participant (no other audio to
  mix); this proves framing/timeline, not speech content.
- UAS→MCU: 210 tone frames over ~4.2 s; bridge stayed alive until the
  session was torn down, so the decoder produced PCM.

## Two-peer mix (stronger run, logs `uas-5091.log` / `uas-5092.log`)

A second identical UAS was added to the same session (ports 5091 and 5092,
because the first rig still owned 5090 during the parallel run):

- Both SIP dialogs confirmed `audio_codec="MP4A-LATM"`, both bridges started,
  four FFmpeg children (2 encoders + 2 decoders) stayed alive.
- API abonents showed growing counters for both participants:
  rx 2059 packets / 391 509 B (56 378 bps), tx 2599 / 326 954 B (47 082 bps).
- UAS2 decoded the MCU RTP mix: `samples=121856 rms=0.0887 nonzero=121179`.
  Both rigs used the same 1 kHz tone, so this run alone does not attribute the
  measured tone to UAS1 (own-loopback would look identical); the decisive
  attribution run below uses one tone and one silent peer.
- UAS1 decoded silence (`rms=0.0000`). This run does not *prove* mix-minus:
  UAS1 decoded its first ~120 packets at ~04:13:19, before UAS2 joined at
  04:13:38, so there was simply no second source in the mix yet. Mix-minus is
  a design property, not a measured result of this run.
- The 5091 log captured MCU RTCP sender reports before the PT113 stream
  (parsed as PT72 when the script masks the second RTP byte; three packets,
  then a steady 1920-step PT113 stream). The 5092 log's first `pt=72` packet
  arrived before its own INVITE and is RTCP from the earlier 5091 call sent to
  the reused port+1; its own stream then steps by 1920.

No `source validator`, `no PCM`, `stalled` or `tearing` warnings appeared;
bridges ended only when the UAS processes exited.

## Decisive attribution run (440 Hz vs silence)

A follow-up lab run used UAS1 (port 5091) = 440 Hz tone and UAS2 (port 5092)
= silence (FFmpeg `volume=0`), in one session (`88df831c…`, two SIP dialogs
confirmed MP4A-LATM, two bridges started).

- UAS2, which sent only silence, decoded `samples=122880 rms=0.0884
  nonzero=122852`, frequency ~440 Hz (FFT peak 440.9 Hz; `uas-attrib-silent.log`,
  `uas-attrib-440.latm`): it hears UAS1's tone through the MCU mixer, so this
  is cross-forwarding, not its own loopback.
- UAS1, which sent the 440 Hz tone, decoded `samples=122880 rms=0.0000
  nonzero=0` (`uas-attrib-440.log`): no self-loop / mix-minus holds.

This supersedes the ambiguous same-tone paragraph above.

## Caveats

- `uas-5092-rx.latm` is a cleaned LOAS stream: the test rig had wrapped one
  RTCP packet into a fake LOAS record; that record was removed, leaving 119
  AAC frames (sha256 in `SHA256SUMS`). It decodes to 121 856 samples,
  RMS 0.08871, 121 179 non-zero. `rx_packets=1784` in `uas-5092.log` includes
  RTCP (its `rx pts={72,113}`).
- The first single-peer run could only prove framing/timeline (RMS 0);
  the two-peer run above proves audible end-to-end audio.
- Legal opt-in was enabled only for these lab tests; default is off and was
  restored after each run (`aac_latm=false aac_latm_legal=false` at
  04:14:33Z and 04:52:14Z, `VKS_AAC` unset, lab conferences deleted).
- `cpresent=0`/config-only peers remain unsupported (explicit 488).
