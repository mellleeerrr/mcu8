#!/usr/bin/env python3
"""stub_uas.py — SIP-заглушка для смоука бота БЕЗ живого MCU.

Зачем: шаг 3 плана (06_perf_lab.md §10) требует проверить бота (INVITE -> SDP ->
RTP -> статистика) до того, как он пойдёт на живой vks-server. Заглушка
принимает INVITE, отвечает 200 OK с собственным медиа-портом, принимает RTP и
считает пакеты. Никакой связи с MCU, никакого влияния на живой сервер.

Использование:
  scripts/bench/stub_uas.py --port 15060 --media-port 25000
  # в другом терминале:
  target/release/vks-sipbot --target 127.0.0.1:15060 \\
      --room 00000000-0000-0000-0000-000000000001 --local-ip 127.0.0.1 \\
      --calls 1 --signal-base-port 5080 --profile m1 --duration 10 --ramp 1

Проверка: заглушка печатает по строке на вызов: `rx_packets`, `bytes`,
`max_seq`, `lost`. Ожидаем ~50 пакетов/с на вызов при 20 мс.
"""
from __future__ import annotations

import argparse
import json
import socket
import struct
import sys
import time


def parse_sdp(body: str):
    """Достаём c= и первый m=audio порт."""
    ip, port = None, None
    for line in body.splitlines():
        if line.startswith("c=IN IP4 "):
            ip = line.split()[-1]
        elif line.startswith("m=audio ") and port is None:
            port = int(line.split()[1])
    return ip, port


def parse_rtp(data: bytes):
    if len(data) < 12 or (data[0] >> 6) != 2:
        return None
    seq = struct.unpack("!H", data[2:4])[0]
    pt = data[1] & 0x7F
    return pt, seq


class CallState:
    def __init__(self, call_id: str, peer):
        self.call_id = call_id
        self.peer = peer
        self.rx = 0
        self.bytes = 0
        self.max_seq = None
        self.lost = 0
        self.started = time.monotonic()


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=15060)
    ap.add_argument("--media-port", type=int, default=25000)
    ap.add_argument("--bind", default="127.0.0.1")
    ap.add_argument("--media-ip", default="127.0.0.1")
    ap.add_argument("--duration", type=float, default=0, help="0 = до Ctrl+C")
    args = ap.parse_args(argv)

    sig = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sig.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sig.bind((args.bind, args.port))
    sig.settimeout(0.2)
    media = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    media.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    media.bind((args.bind, args.media_port))
    media.setblocking(False)

    print(f"[stub] SIP UAS на {args.bind}:{args.port}, RTP на {args.media_ip}:{args.media_port}", flush=True)
    calls: dict[str, CallState] = {}
    start = time.monotonic()
    try:
        while True:
            if args.duration and time.monotonic() - start > args.duration:
                break
            try:
                data, peer = sig.recvfrom(65535)
            except socket.timeout:
                data = b""
            if data:
                text = data.decode("utf-8", "replace")
                first = text.split("\r\n", 1)[0]
                call_id = ""
                for line in text.split("\r\n"):
                    if line.lower().startswith("call-id:"):
                        call_id = line.split(":", 1)[1].strip()
                if first.startswith("INVITE"):
                    c_ip, c_port = parse_sdp(text)
                    print(f"[stub] INVITE от {peer}, их медиа {c_ip}:{c_port}", flush=True)
                    sdp = (
                        "v=0\r\n"
                        f"o=- 1 1 IN IP4 {args.media_ip}\r\n"
                        f"s=stub\r\nc=IN IP4 {args.media_ip}\r\nt=0 0\r\n"
                        f"m=audio {args.media_port} RTP/AVP 8\r\n"
                        "a=rtpmap:8 PCMA/8000\r\na=ptime:20\r\na=sendrecv\r\n"
                    )
                    resp = (
                        "SIP/2.0 200 OK\r\n"
                        f"Via: {next((l for l in text.split(chr(13)+chr(10)) if l.lower().startswith('via:')), 'Via: SIP/2.0/UDP x')}\r\n"
                        f"Call-ID: {call_id}\r\n"
                        "CSeq: " + (next((l.split(':', 1)[1].strip() for l in text.split("\r\n") if l.lower().startswith("cseq:")), "1 INVITE")) + "\r\n"
                        "From: " + (next((l.split(':', 1)[1].strip() for l in text.split("\r\n") if l.lower().startswith("from:")), "sip:bot@x")) + "\r\n"
                        "To: " + (next((l.split(':', 1)[1].strip() for l in text.split("\r\n") if l.lower().startswith("to:")), "sip:stub@x")) + ";tag=stubtag\r\n"
                        f"Contact: <sip:stub@{args.media_ip}:{args.port}>\r\n"
                        f"Content-Type: application/sdp\r\nContent-Length: {len(sdp)}\r\n\r\n{sdp}"
                    )
                    sig.sendto(resp.encode(), peer)
                    calls[call_id] = CallState(call_id, peer)
                elif first.startswith("ACK"):
                    print(f"[stub] ACK {call_id}", flush=True)
                elif first.startswith("BYE"):
                    sig.sendto(
                        (f"SIP/2.0 200 OK\r\nCall-ID: {call_id}\r\nCSeq: {next((l.split(':', 1)[1].strip() for l in text.split(chr(13)+chr(10)) if l.lower().startswith('cseq:')), '2 BYE')}\r\n"
                         "Content-Length: 0\r\n\r\n").encode(),
                        peer,
                    )
                    state = calls.pop(call_id, None)
                    if state:
                        print(json.dumps({"event": "bye", "call_id": call_id, "rx_packets": state.rx,
                                          "bytes": state.bytes, "max_seq": state.max_seq,
                                          "lost": state.lost}), flush=True)
            # RTP
            while True:
                try:
                    pkt, src = media.recvfrom(2048)
                except BlockingIOError:
                    break
                parsed = parse_rtp(pkt)
                if not parsed:
                    continue
                pt, seq = parsed
                state = next((c for c in calls.values() if c.peer[0] == src[0]), None)
                if state is None:
                    continue
                state.rx += 1
                state.bytes += len(pkt)
                if state.max_seq is None:
                    state.max_seq = seq
                else:
                    delta = (seq - state.max_seq) & 0xFFFF
                    if delta < 0x8000 and delta > 0:
                        if delta > 1:
                            state.lost += delta - 1
                        state.max_seq = seq
                if state.rx % 50 == 0:
                    print(f"[stub] call={state.call_id[:8]} rx={state.rx} lost={state.lost} pt={pt}", flush=True)
    except KeyboardInterrupt:
        pass
    print("[stub] итог:")
    for state in calls.values():
        print(json.dumps({"event": "summary", "call_id": state.call_id, "rx_packets": state.rx,
                          "bytes": state.bytes, "lost": state.lost}), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
