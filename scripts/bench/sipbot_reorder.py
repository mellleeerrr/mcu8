#!/usr/bin/env python3
"""sipbot.py — управляемый SIP-бот (UAC) для нагрузочных прогонов MCU.

Один бот = один процесс = один вызов. Матрица запускает N процессов
(`scripts/bench/matrix.py`), поэтому бот не управляет другими ботами.

Происхождение: переработка живого прототипа Lead'а `/tmp/video_probe.py`
(INVITE→200 OK→ACK→PCMU+H.264→BYE). Что исправлено относительно прототипа:
  * аудио — ровно 50 pps по абсолютным дедлайнам (было ~500-1000 pps: 2 пакета
    за итерацию + sleep 2 мс);
  * видео — последовательная смена кадров (frame_idx += 1), а не
    `int(time.time()*FPS) % len(frames)` (при просадке кадры повторялись/прыгали);
  * H.264 RTP: marker=1 только на последнем пакете access unit (в прототипе
    marker ставился на каждом мелком NAL), корректный FU-A с S/E-битами;
  * приём — статистика по каждому SSRC по RFC 3550 A.1 (extended highest seq)
    и A.8 (jitter), плюс счётчики jitter-буфера и kernel-drops сокета;
  * CPU контура MCU — из cgroup (`/proc/<pid>/cgroup`, НЕ хардкод mcu.slice)
    плюс разложение по дереву PID (parent / ffmpeg enc / ffmpeg dec);
  * паспорт артефакта: sha256 `/proc/<mcu_pid>/exe` до и после прогона.

Чего бот НЕ делает (честно): RTCP RR/XR не отправляет (`rtt_ms=null`);
SRTP/TLS не умеет (наш UAS их и не принимает — 488).

Использование:
  scripts/bench/sipbot.py --room <UUID> --host 127.0.0.1 \\
      --local-ip 192.168.1.93 --audio-codec PCMU --video-profile 720p30 \\
      --duration 120 --json-out /tmp/vks-bench/runs/x/bot-0.json

  # синхронный старт N ботов (обычно через matrix.py):
  scripts/bench/matrix.py --bots 10 --video-profile 720p30 --duration 120 --room <UUID>
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import random
import re
import selectors
import socket
import struct
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
MEDIA_DIR = os.path.join(HERE, "media")

# `P0-ingest` (media-core): контролируемый reorder на видео-входе.
# SIPBOT_REORDER=1 — пакеты копятся окном SIPBOT_REORDER_WINDOW (по умолчанию
# 4) и уходят с перестановкой соседних пар (1,0,3,2) — так на стенде без root
# воспроизводится джиттер bot→MCU. По умолчанию 0 — прежнее поведение байт-в-байт.
REORDER = os.environ.get("SIPBOT_REORDER", "0") == "1"
REORDER_WINDOW = int(os.environ.get("SIPBOT_REORDER_WINDOW", "4"))

# ---------------------------------------------------------------- профили ---
VIDEO_PROFILES = {
    # имя: (ширина, высота, fps, h264 level, profile-level-id, целевой kbps)
    "1080p30": (1920, 1080, 30, "4.0", "42e028", 2500),
    "720p30": (1280, 720, 30, "3.1", "42c01f", 1500),
    "720p15": (1280, 720, 15, "3.1", "42c01f", 800),
    "360p15": (640, 360, 15, "3.1", "42c01f", 384),
}
AUDIO_CODECS = {
    "PCMU": (0, "PCMU/8000", 160, "g711u"),
    "PCMA": (8, "PCMA/8000", 160, "g711a"),
    "G722": (9, "G722/8000", 160, "g722"),
}
# Наш UAS принимает H.264 только Baseline (profile_idc=66) и level >= 31
# (crates/vks-gateway-sip/src/uas.rs:659-680) — поэтому уровни 3.1/4.0, не выше.

MTU_PAYLOAD = 1200          # полезная нагрузка RTP без заголовка
AUDIO_FRAME_MS = 20
AUDIO_FRAME_SAMPLES = 160   # 8 кГц × 20 мс


# --------------------------------------------------------------- media ---
def log(msg: str) -> None:
    print(f"[sipbot] {msg}", file=sys.stderr, flush=True)


def split_annexb(raw: bytes) -> list[bytes]:
    """Annex-B -> список NAL без стартовых кодов (00 00 01 / 00 00 00 01)."""
    nals: list[bytes] = []
    i, n = 0, len(raw)
    starts: list[int] = []
    while i + 2 < n:
        if raw[i] == 0 and raw[i + 1] == 0 and raw[i + 2] == 1:
            starts.append(i + 3)
            i += 3
        elif i + 3 < n and raw[i] == 0 and raw[i + 1] == 0 and raw[i + 2] == 0 and raw[i + 3] == 1:
            starts.append(i + 4)
            i += 4
        else:
            i += 1
    for idx, start in enumerate(starts):
        end = starts[idx + 1] if idx + 1 < len(starts) else n
        # отрезаем возможные хвостовые нули следующего стартового кода
        while end > start and raw[end - 1] == 0 and idx + 1 < len(starts):
            end -= 1
        if end > start:
            nals.append(raw[start:end])
    return nals


def group_access_units(nals: list[bytes]) -> list[list[bytes]]:
    """Группировка NAL в access units по AUD (nal_unit_type=9, x264 aud=1)."""
    aus: list[list[bytes]] = []
    cur: list[bytes] = []
    for nal in nals:
        if not nal:
            continue
        if (nal[0] & 0x1F) == 9 and cur:
            aus.append(cur)
            cur = []
        cur.append(nal)
    if cur:
        aus.append(cur)
    return aus


def ensure_video(profile: str, force: bool = False) -> tuple[list[list[bytes]], int]:
    """Сгенерировать/закешировать H.264 Annex-B и вернуть access units + fps."""
    width, height, fps, level, _plid, kbps = VIDEO_PROFILES[profile]
    os.makedirs(MEDIA_DIR, exist_ok=True)
    path = os.path.join(MEDIA_DIR, f"{profile}_baseline_{level.replace('.', '')}.h264")
    if force or not os.path.exists(path) or os.path.getsize(path) < 1024:
        seconds = 8
        cmd = [
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", f"testsrc2=size={width}x{height}:rate={fps}",
            "-t", str(seconds),
            "-c:v", "libx264", "-profile:v", "baseline", "-level:v", level,
            "-preset", "ultrafast", "-tune", "zerolatency",
            "-b:v", f"{kbps}k", "-maxrate", f"{kbps}k", "-bufsize", f"{kbps * 2}k",
            "-x264-params", f"repeat-headers=1:aud=1:keyint={fps}:min-keyint={fps}:scenecut=0:bframes=0",
            "-pix_fmt", "yuv420p", "-f", "h264", path,
        ]
        subprocess.run(cmd, check=True)
        log(f"сгенерирован {path} ({os.path.getsize(path)} B)")
    raw = open(path, "rb").read()
    aus = group_access_units(split_annexb(raw))
    if not aus:
        raise SystemExit(f"не смог разобрать Annex-B {path}")
    log(f"{profile}: {len(aus)} access units, {os.path.getsize(path)} B, fps={fps}")
    return aus, fps


def ensure_g722(force: bool = False) -> list[bytes]:
    """G.722 payload-поток (160 B/20 мс) через ffmpeg-энкодер; кешируется."""
    os.makedirs(MEDIA_DIR, exist_ok=True)
    path = os.path.join(MEDIA_DIR, "tone_g722.g722")
    if force or not os.path.exists(path) or os.path.getsize(path) < 1600:
        subprocess.run([
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=16000:duration=8",
            "-c:a", "g722", "-b:a", "64k", "-f", "g722", path,
        ], check=True)
        log(f"сгенерирован {path} ({os.path.getsize(path)} B)")
    raw = open(path, "rb").read()
    frames = [raw[i:i + 160] for i in range(0, len(raw) - 159, 160)]
    if not frames:
        raise SystemExit(f"пустой G.722 поток {path}")
    return frames


# ---------------------------------------------------------------- кодек ---
def _ulaw_table() -> list[int]:
    """PCM16 (-32768..32767, шаг через 4) -> µ-law; индекс = (sample>>2)+8192."""
    table = [0] * 16384
    for idx in range(16384):
        sample = (idx - 8192) * 4
        sign = 0x80 if sample < 0 else 0
        magnitude = min(abs(sample), 32635) + 0x84
        exponent = 7
        mask = 0x4000
        while exponent > 0 and not (magnitude & mask):
            exponent -= 1
            mask >>= 1
        mantissa = (magnitude >> (exponent + 3)) & 0x0F
        table[idx] = ~(sign | (exponent << 4) | mantissa) & 0xFF
    return table


def _alaw_table() -> list[int]:
    table = [0] * 16384
    for idx in range(16384):
        sample = (idx - 8192) * 4
        sign = 0x00 if sample >= 0 else 0x80
        magnitude = min(abs(sample), 32767)
        if magnitude < 256:
            a_val = magnitude >> 4
        else:
            exponent = 7
            mask = 0x4000
            while exponent > 0 and not (magnitude & mask):
                exponent -= 1
                mask >>= 1
            a_val = (exponent << 4) | ((magnitude >> (exponent + 3)) & 0x0F)
        table[idx] = (sign | a_val) ^ 0x55
    return table


ULAW = _ulaw_table()
ALAW = _alaw_table()


def pcm_tone_payloads(codec: str) -> list[bytes]:
    """1 секунда (50 кадров) тона 1 кГц с AM-огибающей — кодируется один раз."""
    payloads = []
    for frame in range(50):
        samples = []
        for n in range(AUDIO_FRAME_SAMPLES):
            t = (frame * AUDIO_FRAME_SAMPLES + n) / 8000.0
            amp = 0.35 + 0.25 * math.sin(2 * math.pi * 2.0 * t)
            samples.append(int(8000 * amp * math.sin(2 * math.pi * 1000.0 * t)))
        if codec == "PCMA":
            payloads.append(bytes(ALAW[(s >> 2) + 8192] for s in samples))
        else:
            payloads.append(bytes(ULAW[(s >> 2) + 8192] for s in samples))
    return payloads


# ------------------------------------------------------------------ RTP ---
def rtp_header(pt: int, seq: int, ts: int, ssrc: int, marker: bool) -> bytes:
    return struct.pack("!BBHII", 0x80, (0x80 if marker else 0) | (pt & 0x7F),
                       seq & 0xFFFF, ts & 0xFFFFFFFF, ssrc & 0xFFFFFFFF)


def parse_rtp(data: bytes):
    if len(data) < 12 or (data[0] >> 6) != 2:
        return None
    cc = data[0] & 0x0F
    marker = bool(data[1] & 0x80)
    pt = data[1] & 0x7F
    seq, ts, ssrc = struct.unpack("!HII", data[2:12])
    offset = 12 + cc * 4
    if data[0] & 0x10:  # extension header
        if len(data) < offset + 4:
            return None
        _, words = struct.unpack("!HH", data[offset:offset + 4])
        offset += 4 + words * 4
    if len(data) < offset:
        return None
    return pt, seq, ts, ssrc, marker, data[offset:]


def packetize_video(au: list[bytes], pt: int, seq: int, ts: int, ssrc: int,
                    mtu: int = MTU_PAYLOAD) -> tuple[list[bytes], int]:
    """H.264/RFC 6184: single NAL / FU-A, marker на последнем пакете AU."""
    packets: list[bytes] = []
    for nal_idx, nal in enumerate(au):
        if not nal:
            continue
        last_nal = nal_idx == len(au) - 1
        if len(nal) <= mtu:
            packets.append(rtp_header(pt, seq, ts, ssrc, last_nal) + nal)
            seq = (seq + 1) & 0xFFFF
            continue
        nal_type = nal[0] & 0x1F
        nri = nal[0] & 0x60
        indicator = bytes([nri | 28])
        body = nal[1:]
        chunk = mtu - 2
        offset = 0
        while offset < len(body):
            piece = body[offset:offset + chunk]
            offset += len(piece)
            start = offset == len(piece)
            end = offset >= len(body)
            fu_header = bytes([(0x80 if start else 0) | (0x40 if end else 0) | nal_type])
            marker = last_nal and end
            packets.append(rtp_header(pt, seq, ts, ssrc, marker) + indicator + fu_header + piece)
            seq = (seq + 1) & 0xFFFF
    return packets, seq


# --------------------------------------------------------- статистика ---
class StreamStats:
    """RFC 3550 A.1 (потери по extended highest seq) + A.8 (jitter).

    Уникальность пакета определяется битовой картой seq (64 КиБ): полный круг
    seq = 65536 пакетов = ровно цикл переноса A.1, поэтому карта очищается при
    каждом переносе cycles. `lost = expected - unique` — без «прощения» дублей,
    иначе дубль маскирует одну потерю (проверено на синтетике: 2 потери + 1 дубль
    дают lost=2, а не 1).
    """

    MAX_DROPOUT = 3000
    MAX_MISORDER = 100

    def __init__(self, clock: int):
        self.clock = clock
        self.arrivals = 0
        self.unique = 0
        self.bytes = 0
        self.base_seq = None
        self.max_seq = None
        self.cycles = 0
        self.ext_max = 0
        self.dup = 0
        self.reorder = 0
        self.seen = bytearray(1 << 16)
        self.jitter = 0.0
        self.prev_transit = None
        self.first_arrival = None
        self.last_arrival = None
        self.base_arrival = None
        self.base_ts = None
        self.jb_late = 0
        self.jb_early = 0
        self.jb_max_abs_ms = 0.0

    def on_packet(self, seq: int, ts: int, payload_len: int, now: float,
                  jitter_buffer_ms: float) -> None:
        self.arrivals += 1
        self.bytes += payload_len
        if self.first_arrival is None:
            self.first_arrival = now
        self.last_arrival = now
        fresh = not self.seen[seq]
        if self.base_seq is None:
            self.base_seq = seq
            self.max_seq = seq
            self.base_arrival = now
        elif seq < self.max_seq and (self.max_seq - seq) > 0x8000:
            # перенос вперёд: 65530 -> 3
            self.cycles += 1
            self.max_seq = seq
            self.seen = bytearray(1 << 16)
            fresh = True
        elif seq > self.max_seq and (seq - self.max_seq) > 0x8000:
            # перенос назад (пакет из прошлого цикла)
            pass
        elif seq > self.max_seq:
            if seq - self.max_seq <= self.MAX_DROPOUT:
                self.max_seq = seq
            else:
                self.cycles += 1
                self.max_seq = seq
                self.seen = bytearray(1 << 16)
                fresh = True
        elif seq < self.max_seq:
            if (self.max_seq - seq) <= self.MAX_MISORDER and fresh:
                self.reorder += 1
        if fresh:
            self.seen[seq] = 1
            self.unique += 1
        else:
            self.dup += 1
        self.ext_max = self.cycles * 0x10000 + self.max_seq
        # jitter (A.8)
        transit = now - ts / float(self.clock)
        if self.prev_transit is not None:
            delta = abs(transit - self.prev_transit)
            self.jitter += (delta - self.jitter) / 16.0
        self.prev_transit = transit
        # счётчики jitter-буфера: идеальный график по RTP-таймстемпу (не по seq:
        # у видео интервал 33 мс, у аудио 20 мс; seq-gap ≠ временной сдвиг)
        if self.base_ts is None:
            self.base_ts = ts
        if self.base_arrival is not None:
            d_ts = (ts - self.base_ts) & 0xFFFFFFFF
            if d_ts > 0x80000000:      # переход через 0
                d_ts -= 0x100000000
            ideal = self.base_arrival + d_ts / float(self.clock)
            delta_ms = (now - ideal) * 1000.0
            self.jb_max_abs_ms = max(self.jb_max_abs_ms, abs(delta_ms))
            if delta_ms > jitter_buffer_ms:
                self.jb_late += 1
            elif delta_ms < -jitter_buffer_ms:
                self.jb_early += 1

    def report(self) -> dict:
        if self.base_seq is None:
            return {"packets": 0, "lost": 0, "loss_pct": 0.0}
        expected = max(0, self.ext_max - self.base_seq + 1)
        lost = max(0, expected - self.unique)
        return {
            "packets": self.arrivals,
            "unique": self.unique,
            "bytes": self.bytes,
            "base_seq": self.base_seq,
            "ext_max_seq": self.ext_max,
            "expected": expected,
            "lost": lost,
            "loss_pct": round(lost / expected * 100.0, 4) if expected > 0 else 0.0,
            "dup": self.dup,
            "reorder": self.reorder,
            "jitter_ms": round(self.jitter * 1000.0, 3),
            "jb_late": self.jb_late,
            "jb_early": self.jb_early,
            "jb_max_abs_ms": round(self.jb_max_abs_ms, 2),
            "span_s": round((self.last_arrival or 0) - (self.first_arrival or 0), 3),
        }


# ------------------------------------------------------------ CPU/MCU ---
def mcu_cgroup_path(pid: int) -> str:
    """Путь cgroup v2 контура `pid`.

    Под root в netns (`sudo ip netns exec …`) `/sys/fs/cgroup` может быть
    пустым: процесс попадает в приватное cgroup-namespace окружения, и путь
    из `/proc/<pid>/cgroup` там не существует (поймано на T2: usage_usec=0
    у всех срезов). Поэтому при отсутствии пути читаем его через
    `/proc/<pid>/root` — mount namespace самого MCU.
    """
    with open(f"/proc/{pid}/cgroup") as fh:
        for line in fh:
            if line.startswith("0::"):
                path = "/sys/fs/cgroup" + line.split(":", 2)[2].strip()
                if not os.path.exists(path):
                    alt = f"/proc/{pid}/root{path}"
                    if os.path.exists(alt):
                        return alt
                return path
    return ""


def read_cpu_stat(cgroup: str) -> dict[str, int]:
    out: dict[str, int] = {}
    try:
        with open(os.path.join(cgroup, "cpu.stat")) as fh:
            for line in fh:
                key, _, value = line.partition(" ")
                if value.strip():
                    out[key] = int(value)
    except OSError:
        pass
    return out


def proc_ticks(pid: int) -> int:
    try:
        with open(f"/proc/{pid}/stat") as fh:
            fields = fh.read().rsplit(")", 1)[1].split()
        return int(fields[11]) + int(fields[12])
    except (OSError, IndexError, ValueError):
        return 0


def process_tree(root: int) -> list[int]:
    children: dict[int, list[int]] = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        try:
            with open(f"/proc/{pid}/stat") as fh:
                ppid = int(fh.read().rsplit(")", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            continue
        children.setdefault(ppid, []).append(pid)
    out: list[int] = []
    stack = [root]
    while stack:
        pid = stack.pop()
        out.append(pid)
        stack.extend(children.get(pid, []))
    return out


def proc_comm(pid: int) -> str:
    try:
        return open(f"/proc/{pid}/comm").read().strip()
    except OSError:
        return "?"


def ffmpeg_role(pid: int) -> str:
    try:
        joined = b" ".join(open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")).decode("utf-8", "replace")
    except OSError:
        return "unknown"
    return "encoder" if "rawvideo" in joined else "decoder"


def proc_rss_kb(pid: int) -> int:
    try:
        for line in open(f"/proc/{pid}/status"):
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except OSError:
        pass
    return 0


def exe_sha256(pid: int) -> str:
    h = hashlib.sha256()
    try:
        with open(f"/proc/{pid}/exe", "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return "unreadable"


def udp_socket_drops(port: int) -> int:
    """drops для нашего UDP-сокета из /proc/net/udp (последняя колонка)."""
    try:
        with open("/proc/net/udp") as fh:
            lines = fh.readlines()[1:]
        needle = f"{port:04X}"
        for line in lines:
            fields = line.split()
            if len(fields) >= 13 and fields[1].endswith(":" + needle):
                return int(fields[-1])
    except (OSError, ValueError):
        pass
    return 0


def mcu_contour_sample(pid: int) -> dict:
    cgroup = mcu_cgroup_path(pid)
    stat = read_cpu_stat(cgroup)
    tree = process_tree(pid)
    ffmpeg = [p for p in tree if proc_comm(p) == "ffmpeg"]
    enc = [p for p in ffmpeg if ffmpeg_role(p) == "encoder"]
    dec = [p for p in ffmpeg if ffmpeg_role(p) == "decoder"]
    return {
        "at": time.time(),
        "usage_usec": stat.get("usage_usec", 0),
        "user_usec": stat.get("user_usec", 0),
        "system_usec": stat.get("system_usec", 0),
        "cgroup": cgroup,
        "pids": len(tree),
        "threads": sum(len(os.listdir(f"/proc/{p}/task")) for p in tree if os.path.exists(f"/proc/{p}/task")),
        "rss_kb": sum(proc_rss_kb(p) for p in tree),
        "ffmpeg_count": len(ffmpeg),
        "ffmpeg_encoder_count": len(enc),
        "ffmpeg_decoder_count": len(dec),
        "ffmpeg_ticks": sum(proc_ticks(p) for p in ffmpeg),
        "ffmpeg_pid_ticks": {p: proc_ticks(p) for p in ffmpeg},
        "encoder_ticks": sum(proc_ticks(p) for p in enc),
        "decoder_ticks": sum(proc_ticks(p) for p in dec),
        "contour_ticks": sum(proc_ticks(p) for p in tree),
    }


# ------------------------------------------------------------------ SIP ---
class SipDialog:
    def __init__(self, host: str, sip_port: int, local_ip: str, room: str,
                 bind_port_hint: int = 0):
        self.host = host
        self.sip_port = sip_port
        self.local_ip = local_ip
        self.room = room
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            self.sock.bind((local_ip, bind_port_hint))
        except OSError:
            self.sock.bind((local_ip, 0))
        self.sock.setblocking(False)
        self.local_port = self.sock.getsockname()[1]
        self.call_id = f"{random.getrandbits(64):016x}@{local_ip}"
        self.from_tag = f"{random.getrandbits(32):08x}"
        self.to_tag = ""
        self.cseq = 1
        self.branch = f"z9hG4bK{random.getrandbits(32):08x}"

    def _msg(self, method: str, uri: str, body: str = "", extra_headers: str = "") -> str:
        headers = [
            f"{method} {uri} SIP/2.0",
            f"Via: SIP/2.0/UDP {self.local_ip}:{self.local_port};branch={self.branch};rport",
            "Max-Forwards: 70",
            f"From: <sip:bot@{self.local_ip}>;tag={self.from_tag}",
            f"To: <sip:{self.room}@{self.host}>" + (f";tag={self.to_tag}" if self.to_tag else ""),
            f"Call-ID: {self.call_id}",
            f"CSeq: {self.cseq} {method}",
            f"Contact: <sip:bot@{self.local_ip}:{self.local_port}>",
            "User-Agent: vks-sipbot/1",
        ]
        if extra_headers:
            headers.append(extra_headers)
        if body:
            headers.append("Content-Type: application/sdp")
        headers.append(f"Content-Length: {len(body)}")
        return "\r\n".join(headers) + "\r\n\r\n" + body

    def send_invite(self, sdp: str) -> None:
        self.sock.sendto(self._msg("INVITE", f"sip:{self.room}@{self.host}", sdp).encode(),
                         (self.host, self.sip_port))

    def send_ack(self) -> None:
        self.sock.sendto(self._msg("ACK", f"sip:{self.room}@{self.host}").encode(),
                         (self.host, self.sip_port))

    def send_bye(self) -> None:
        self.cseq += 1
        self.sock.sendto(self._msg("BYE", f"sip:{self.room}@{self.host}").encode(),
                         (self.host, self.sip_port))

    def read_responses(self) -> list[tuple[int, str]]:
        out = []
        while True:
            try:
                data, _ = self.sock.recvfrom(65535)
            except BlockingIOError:
                break
            text = data.decode("utf-8", "replace")
            m = re.match(r"SIP/2.0 (\d{3})", text)
            if not m:
                continue
            code = int(m.group(1))
            tag = re.search(r"^To:.*?;tag=([^;\r\n]+)", text, re.M | re.I)
            if tag:
                self.to_tag = tag.group(1)
            out.append((code, text))
        return out

    def wait_response(self, want: int, timeout: float) -> tuple[bool, str]:
        deadline = time.time() + timeout
        while time.time() < deadline:
            for code, text in self.read_responses():
                if code == want:
                    return True, text
                if code >= 300:
                    return False, text
            time.sleep(0.005)
        return False, ""


def parse_answer_sdp(sdp: str, fallback_ip: str) -> dict:
    c_ip = fallback_ip
    audio = video = None
    audio_pt = video_pt = None
    for line in sdp.splitlines():
        line = line.strip()
        if line.startswith("c=IN IP4 "):
            c_ip = line.split()[-1]
        elif line.startswith("m=audio "):
            parts = line.split()
            if len(parts) >= 4:
                port = int(parts[1])
                pts = [int(p) for p in parts[3:] if p.isdigit()]
                if port and pts:
                    audio = (c_ip, port)
                    audio_pt = pts[0]
        elif line.startswith("m=video "):
            parts = line.split()
            if len(parts) >= 4:
                port = int(parts[1])
                pts = [int(p) for p in parts[3:] if p.isdigit()]
                if port and pts:
                    video = (c_ip, port)
                    video_pt = pts[0]
    return {"audio": audio, "video": video, "audio_pt": audio_pt, "video_pt": video_pt,
            "c_ip": c_ip, "raw": sdp}


# --------------------------------------------------------------- main ---
def build_sdp(args, audio_port: int, video_port: int, audio_pt: int, rtpmap: str,
              video_plid: str) -> str:
    lines = [
        "v=0",
        f"o=- {random.getrandbits(32)} 1 IN IP4 {args.local_ip}",
        "s=vks-sipbot",
        f"c=IN IP4 {args.local_ip}",
        "t=0 0",
        f"m=audio {audio_port} RTP/AVP {audio_pt}",
        f"a=rtpmap:{audio_pt} {rtpmap}",
        "a=ptime:20",
        "a=sendrecv",
    ]
    if args.video_profile != "none":
        lines += [
            f"m=video {video_port} RTP/AVP 96",
            "a=rtpmap:96 H264/90000",
            f"a=fmtp:96 packetization-mode=1;profile-level-id={video_plid}",
            "a=sendrecv",
        ]
    return "\r\n".join(lines) + "\r\n"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="SIP-бот: аудио + H.264, статистика по SSRC")
    ap.add_argument("--room", required=True, help="UUID/route/alias комнаты (SIP user part)")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--sip-port", type=int, default=5060)
    ap.add_argument("--local-ip", default="192.168.1.93", help="IP, на который MCU шлёт RTP (candidate_addr)")
    ap.add_argument("--audio-codec", default="PCMU", choices=sorted(AUDIO_CODECS))
    ap.add_argument("--video-profile", default="none", choices=sorted(VIDEO_PROFILES) + ["none"])
    ap.add_argument("--duration", type=float, default=120.0, help="секунд медиа (после ACK)")
    ap.add_argument("--rtp-jitter-buffer", type=float, default=60.0, help="мс, только для счётчиков")
    ap.add_argument("--start-at", type=float, default=0.0, help="unix-время синхронного старта медиа")
    ap.add_argument("--connect-timeout", type=float, default=15.0)
    ap.add_argument("--json-out", required=True)
    ap.add_argument("--mcu-pid", type=int, default=0, help="PID vks-server (0 = найти по systemctl)")
    ap.add_argument("--no-mcu-cpu", action="store_true", help="не мерить CPU контура (для прогонов N>1)")
    ap.add_argument("--force-media", action="store_true")
    ap.add_argument("--index", type=int, default=-1,
                    help="номер бота: SIP-порт 5080+2i, RTP 20000+4i — вне пула MCU 41000-41200")
    ap.add_argument("--rx-sockbuf", type=int, default=4, help="МБ на SO_RCVBUF медиа-сокетов")
    ap.add_argument("--dump-rx", default="", help="файл: seq, ts, arrival, pt каждого принятого пакета")
    ap.add_argument("--skip-exe-hash", action="store_true",
                    help="не хешировать /proc/<mcu>/exe (matrix.py делает это один раз до/после)")
    ap.add_argument("--tag", default="")
    args = ap.parse_args(argv)

    audio_pt, rtpmap, _frame_bytes, _codec_name = AUDIO_CODECS[args.audio_codec]
    video_aus: list[list[bytes]] = []
    video_fps = 0
    reorder_queue = []
    video_plid = ""
    if args.video_profile != "none":
        video_aus, video_fps = ensure_video(args.video_profile, args.force_media)
        video_plid = VIDEO_PROFILES[args.video_profile][4]
    g722_frames: list[bytes] = []
    pcm_payloads: list[bytes] = []
    if args.audio_codec == "G722":
        g722_frames = ensure_g722(args.force_media)
    else:
        pcm_payloads = pcm_tone_payloads(args.audio_codec)

    # --- MCU pid и паспорт артефакта ---
    mcu_pid = args.mcu_pid
    if not mcu_pid:
        try:
            out = subprocess.run(["systemctl", "--user", "show", "-p", "MainPID", "--value", "vks-mcu.service"],
                                 capture_output=True, text=True, timeout=5).stdout.strip()
            mcu_pid = int(out) if out.isdigit() else 0
        except Exception:  # noqa: BLE001
            mcu_pid = 0
    exe_before = ("matrix" if args.skip_exe_hash else
                  (exe_sha256(mcu_pid) if mcu_pid else "no-mcu-pid"))
    contour_before = mcu_contour_sample(mcu_pid) if (mcu_pid and not args.no_mcu_cpu) else None

    # --- сокеты медиа ---
    def bind_port(sock: socket.socket, preferred: int) -> int:
        """Порт по индексу бота; при занятости — эфемерный (занятость логируем)."""
        if preferred > 0:
            try:
                sock.bind((args.local_ip, preferred))
                return preferred
            except OSError:
                log(f"порт {preferred} занят, беру эфемерный")
        sock.bind((args.local_ip, 0))
        return sock.getsockname()[1]

    idx = max(0, args.index)
    sockbuf = max(212992, args.rx_sockbuf * 1024 * 1024)

    def tune(sock: socket.socket) -> None:
        try:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, sockbuf)
        except OSError:
            pass

    audio_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    tune(audio_sock)
    audio_port = bind_port(audio_sock, 20000 + 4 * idx if args.index >= 0 else 0)
    audio_sock.setblocking(False)
    video_sock = video_port = None
    if args.video_profile != "none":
        video_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        tune(video_sock)
        video_port = bind_port(video_sock, 20000 + 4 * idx + 2 if args.index >= 0 else 0)
        video_sock.setblocking(False)

    dialog = SipDialog(args.host, args.sip_port, args.local_ip, args.room,
                       bind_port_hint=(5080 + 2 * idx) if args.index >= 0 else 0)
    sdp = build_sdp(args, audio_port, video_port, audio_pt, rtpmap, video_plid)
    dialog.send_invite(sdp)
    ok, answer_text = dialog.wait_response(200, args.connect_timeout)
    result: dict = {
        "tag": args.tag,
        "pid": os.getpid(),
        "room": args.room,
        "host": args.host,
        "local_ip": args.local_ip,
        "audio_codec": args.audio_codec,
        "video_profile": args.video_profile,
        "video_fps": video_fps,
        "duration_s": args.duration,
        "exe_sha256_before": exe_before,
        "mcu_pid": mcu_pid,
        "disposition": "connected" if ok else "failed",
        "sip_response": answer_text.split("\r\n", 1)[0] if answer_text else "timeout",
        "started_at": time.time(),
    }
    if not ok:
        result["reason"] = "no 200 OK"
        result["exe_sha256_after"] = ("matrix" if args.skip_exe_hash else
                                       (exe_sha256(mcu_pid) if mcu_pid else "no-mcu-pid"))
        result["exe_stable"] = result["exe_sha256_before"] == result["exe_sha256_after"]
        _write_json(args.json_out, result)
        log(f"вызов не установлен: {result['sip_response']}")
        return 1

    sdp_body = answer_text.split("\r\n\r\n", 1)[1] if "\r\n\r\n" in answer_text else ""
    remote = parse_answer_sdp(sdp_body, args.host)
    result["sdp_answer"] = sdp_body
    result["remote"] = {"audio": remote["audio"], "video": remote["video"],
                        "audio_pt": remote["audio_pt"], "video_pt": remote["video_pt"]}
    result["audio_pt_tx"] = audio_pt
    result["sip_local_port"] = dialog.local_port
    result["rtp_local"] = {"audio": audio_port, "video": video_port}
    if remote["video"] is None and args.video_profile != "none":
        result["video_status"] = "rejected_by_mcu"
    else:
        result["video_status"] = "active" if remote["video"] else "not_requested"
    dialog.send_ack()

    # --- ожидание синхронного старта ---
    # Важно: после ACK MCU уже шлёт RTP. Если просто спать, пакеты копятся в
    # SO_RCVBUF и в начале окна приходят пачкой: pps завышается (намерено 68 вместо
    # 50 при connect-window 8 с), jitter/jb-счётчики ломаются, а при 10 ботах
    # копится ~3 МБ видео на бота и буфер переполняется (потери 2-7 %). Поэтому
    # в фазе ожидания дренируем и ОТБРАСЫВАЕМ, а статистику начинаем с чистого листа.
    prestart_discarded = 0
    if args.start_at > time.time():
        pre_sel = selectors.DefaultSelector()
        pre_sel.register(audio_sock, selectors.EVENT_READ)
        if video_sock:
            pre_sel.register(video_sock, selectors.EVENT_READ)
        while True:
            remaining = args.start_at - time.time()
            if remaining <= 0:
                break
            for key, _mask in pre_sel.select(timeout=min(0.1, remaining)):
                while True:
                    try:
                        key.fileobj.recvfrom(2048)
                    except (BlockingIOError, OSError):
                        break
                    prestart_discarded += 1
        pre_sel.close()

    audio_ssrc = random.getrandbits(32)
    video_ssrc = random.getrandbits(32) if video_sock else 0
    tx_audio_pt = audio_pt if remote["audio_pt"] is None else audio_pt
    tx_video_pt = 96 if remote["video_pt"] is None else int(remote["video_pt"])

    audio_stream = StreamStats(8000)
    video_stream = StreamStats(90000)
    rx_unknown: dict[int, StreamStats] = {}
    sel = selectors.DefaultSelector()
    sel.register(dialog.sock, selectors.EVENT_READ)
    sel.register(audio_sock, selectors.EVENT_READ)
    if video_sock:
        sel.register(video_sock, selectors.EVENT_READ)

    dump_rx = open(args.dump_rx, "w") if args.dump_rx else None
    drops_before = udp_socket_drops(audio_port) + (udp_socket_drops(video_port) if video_port else 0)
    audio_seq = random.getrandbits(16)
    video_seq = random.getrandbits(16)
    audio_ts = random.getrandbits(32)
    video_ts = random.getrandbits(32)
    audio_idx = video_idx = 0
    audio_sent = video_sent_pkts = video_sent_frames = 0
    audio_skipped = video_skipped = 0
    lag_audio: list[float] = []
    lag_video: list[float] = []
    start = time.time()
    end = start + args.duration
    next_audio = start
    next_video = start
    audio_period = AUDIO_FRAME_MS / 1000.0
    video_period = (1.0 / video_fps) if video_fps else 0.0

    while time.time() < end:
        now = time.time()
        events = sel.select(timeout=max(0.0, min(next_audio, next_video if video_fps else next_audio, now + 0.05) - now))
        for key, _mask in events:
            sock = key.fileobj
            while True:
                try:
                    data, _src = sock.recvfrom(2048)
                except BlockingIOError:
                    break
                except OSError:
                    break
                if sock is dialog.sock:
                    continue  # ответы SIP в медиа-цикле игнорируем (кроме BYE 200)
                parsed = parse_rtp(data)
                if not parsed:
                    continue
                pt, seq, ts, ssrc, _marker, payload = parsed
                now2 = time.time()
                if dump_rx is not None:
                    dump_rx.write(f"{pt}\t{seq}\t{ts}\t{now2:.6f}\t{_src[0]}:{_src[1]}\n")
                if pt == (remote["audio_pt"] if remote["audio_pt"] is not None else audio_pt):
                    audio_stream.on_packet(seq, ts, len(payload), now2, args.rtp_jitter_buffer)
                elif remote["video_pt"] is not None and pt == int(remote["video_pt"]):
                    video_stream.on_packet(seq, ts, len(payload), now2, args.rtp_jitter_buffer)
                else:
                    rx_unknown.setdefault(ssrc, StreamStats(8000 if pt < 34 else 90000)).on_packet(
                        seq, ts, len(payload), now2, args.rtp_jitter_buffer)
        now = time.time()
        # --- аудио: ровно 50 pps ---
        if now >= next_audio:
            if now - next_audio > 0.1:
                missed = int((now - next_audio) / audio_period)
                audio_skipped += missed
                next_audio += missed * audio_period
            if next_audio <= now + audio_period:
                if args.audio_codec == "G722":
                    payload = g722_frames[audio_idx % len(g722_frames)]
                else:
                    payload = pcm_payloads[audio_idx % len(pcm_payloads)]
                audio_idx += 1
                pkt = rtp_header(audio_pt, audio_seq, audio_ts, audio_ssrc, False) + payload
                try:
                    audio_sock.sendto(pkt, remote["audio"] or (args.host, args.sip_port))
                    audio_sent += 1
                except OSError:
                    pass
                audio_seq = (audio_seq + 1) & 0xFFFF
                audio_ts = (audio_ts + AUDIO_FRAME_SAMPLES) & 0xFFFFFFFF
                lag_audio.append(max(0.0, now - next_audio))
                next_audio += audio_period
        # --- видео: ровно FPS, последовательные кадры ---
        if video_fps and video_sock and now >= next_video:
            if now - next_video > video_period:
                missed = int((now - next_video) / video_period)
                video_skipped += missed
                video_idx += missed
                next_video += missed * video_period
            au = video_aus[video_idx % len(video_aus)]
            video_idx += 1
            packets, video_seq = packetize_video(au, tx_video_pt, video_seq, video_ts, video_ssrc)
            sent_ok = 0
            if REORDER:
                reorder_queue.extend(packets)
                while len(reorder_queue) >= REORDER_WINDOW:
                    window = reorder_queue[:REORDER_WINDOW]
                    del reorder_queue[:REORDER_WINDOW]
                    order = list(range(REORDER_WINDOW))
                    for pair in range(0, REORDER_WINDOW - 1, 2):
                        order[pair], order[pair + 1] = order[pair + 1], order[pair]
                    for index in order:
                        try:
                            video_sock.sendto(window[index], remote["video"] or (args.host, args.sip_port))
                            sent_ok += 1
                        except OSError:
                            break
            else:
                for pkt in packets:
                    try:
                        video_sock.sendto(pkt, remote["video"] or (args.host, args.sip_port))
                        sent_ok += 1
                    except OSError:
                        break
            video_sent_pkts += sent_ok
            if sent_ok == len(packets):
                video_sent_frames += 1
            video_ts = (video_ts + int(90000 / video_fps)) & 0xFFFFFFFF
            lag_video.append(max(0.0, now - next_video))
            next_video += video_period

    for pkt in reorder_queue:
        try:
            video_sock.sendto(pkt, remote["video"] or (args.host, args.sip_port))
            video_sent_pkts += 1
        except OSError:
            break
    reorder_queue.clear()
    wall = time.time() - start
    if dump_rx is not None:
        dump_rx.close()
    drops_after = udp_socket_drops(audio_port) + (udp_socket_drops(video_port) if video_port else 0)
    result["media"] = {
        "wall_s": round(wall, 3),
        "prestart_packets_discarded": prestart_discarded,
        "audio": {
            "sent_packets": audio_sent,
            "pps": round(audio_sent / wall, 2) if wall > 0 else 0,
            "skipped_ticks": audio_skipped,
            "send_lag_p50_us": _pct(lag_audio, 50),
            "send_lag_p99_us": _pct(lag_audio, 99),
            "send_lag_max_us": round(max(lag_audio) * 1e6, 1) if lag_audio else 0.0,
        },
        "video": {
            "sent_packets": video_sent_pkts,
            "sent_frames": video_sent_frames,
            "fps": round(video_sent_frames / wall, 2) if wall > 0 else 0,
            "skipped_frames": video_skipped,
            "available_frames": len(video_aus),
            "send_lag_p50_us": _pct(lag_video, 50),
            "send_lag_p99_us": _pct(lag_video, 99),
        },
        "kernel_drops": drops_after - drops_before,
    }
    result["rx"] = {
        "audio": audio_stream.report(),
        "video": video_stream.report(),
        "unknown_ssrc": {str(k): v.report() for k, v in rx_unknown.items()},
    }
    if args.audio_codec in ("PCMU", "PCMA"):
        result["rx"]["audio"]["mos"] = _e_model_mos(audio_stream)
    result["self_cpu"] = {"user_s": round(os.times().user, 3), "sys_s": round(os.times().system, 3)}
    if contour_before is not None:
        contour_after = mcu_contour_sample(mcu_pid)
        elapsed = max(1e-6, contour_after["at"] - contour_before["at"])
        d_usage = contour_after["usage_usec"] - contour_before["usage_usec"]
        d_ff = contour_after["ffmpeg_ticks"] - contour_before["ffmpeg_ticks"]
        d_enc = contour_after["encoder_ticks"] - contour_before["encoder_ticks"]
        d_dec = contour_after["decoder_ticks"] - contour_before["decoder_ticks"]
        d_tree = contour_after["contour_ticks"] - contour_before["contour_ticks"]
        result["mcu_cpu"] = {
            "cgroup": contour_after["cgroup"],
            "p_contour_pct_one_core": round(d_usage / (elapsed * 1e6) * 100.0, 3),
            "p_ffmpeg_pct": round(d_ff / (elapsed * 100) * 100.0, 3),
            "p_encoder_pct": round(d_enc / (elapsed * 100) * 100.0, 3),
            "p_decoder_pct": round(d_dec / (elapsed * 100) * 100.0, 3),
            "ffmpeg_share": round(d_ff / d_tree, 4) if d_tree else 0.0,
            "pids": contour_after["pids"],
            "threads": contour_after["threads"],
            "rss_kb": contour_after["rss_kb"],
            "ffmpeg_count": contour_after["ffmpeg_count"],
            "ffmpeg_encoder_count": contour_after["ffmpeg_encoder_count"],
            "ffmpeg_decoder_count": contour_after["ffmpeg_decoder_count"],
            "single_session_only": True,
        }
    exe_after = ("matrix" if args.skip_exe_hash else
                 (exe_sha256(mcu_pid) if mcu_pid else "no-mcu-pid"))
    result["exe_sha256_after"] = exe_after
    result["exe_stable"] = exe_before == exe_after
    result["ended_at"] = time.time()

    # --- BYE ---
    dialog.send_bye()
    time.sleep(0.05)
    for code, _text in dialog.read_responses():
        if code == 200:
            result["bye_ok"] = True
    _write_json(args.json_out, result)
    log(f"готово: {result['disposition']}, аудио {audio_sent} пакетов "
        f"({result['media']['audio']['pps']} pps), видео {video_sent_pkts} пакетов "
        f"({result['media']['video']['fps']} fps) -> {args.json_out}")
    return 0


def _pct(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, int(round((q / 100.0) * (len(ordered) - 1)))))
    return round(ordered[idx] * 1e6, 1)


def _e_model_mos(stats: StreamStats) -> float | None:
    report = stats.report()
    if not report["packets"]:
        return None
    loss = report["loss_pct"]
    ie = 0.0
    bpl = 25.1
    ie_eff = ie + (95.0 - ie) * (loss / (loss + bpl)) if loss > 0 else ie
    d = AUDIO_FRAME_MS + max(0.0, report["jitter_ms"]) * 2.0
    i_d = 0.024 * d + (0.11 * (d - 177.3) if d > 177.3 else 0.0)
    r = max(0.0, min(100.0, 93.2 - 1.4 - i_d - ie_eff))
    mos = 1.0 + 0.035 * r + 7e-6 * r * (r - 60.0) * (100.0 - r)
    return round(max(1.0, min(4.5, mos)), 3)


def _write_json(path: str, data: dict) -> None:
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w") as fh:
        json.dump(data, fh, indent=2, ensure_ascii=False)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
