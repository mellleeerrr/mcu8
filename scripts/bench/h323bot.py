#!/usr/bin/env python3
"""h323bot.py — H.323-бот: N вызовов на наш H.323 UAS.

Минимальный жизнеспособный путь (06_perf_lab.md §7.1): переиспользуем
проверенный sidecar `h323_sidecar.py` как UAC (он делает Q.931/H.225/H.245,
FastStart не умеет — его и не тестируем), а RTP-сторону держим здесь, потому
что контракт sidecar прямо говорит: "The caller owns/reserves each advertised
RTP/RTCP port pair. The helper does not bind or transform media."
(crates/vks-gateway-h323/README.md).

Ограничения (обязательны в отчёте):
  * H.323-стек знает только G.722(9)/G.711U(0)/G.711A(8)/G.723.1(4) со
    статическими PT (crates/vks-gateway-h323/python/h323_audio.py:9-14);
  * bitstream для G.722 синтетический (RTP-кадрирование корректное, сигнал —
    нет): CPU MCU тратится так же, но качество/MOS по H.323-G.722 НЕ измеряется,
    в отчёт идёт bitstream=synthetic;
  * наш H.323 UAS читает allowlist при старте: бот должен идти с 127.0.0.1
    (входит в живой VKS_H323_UAS_ALLOW=127.0.0.1/32,192.168.1.0/24).

Использование:
  $VKS_PY scripts/bench/h323bot.py --target 127.0.0.1:1720 --alias <room> \\
      --calls 20 --signal-base-port 2720 --rtp-base-port 30000 \\
      --profile h323-m1 --duration 150 --ramp 2 --report /tmp/vks-bench/runs/x/bot
"""
from __future__ import annotations

import argparse
import json
import math
import os
import select
import selectors
import socket
import struct
import subprocess
import sys
import threading
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import sipbot  # noqa: E402 — переиспользуем cgroup-сэмплер и sha256 (та же методика, что в matrix.py)
SIDECAR_DIR = os.path.join(REPO, "crates", "vks-gateway-h323", "python")
SIDECAR = os.path.join(SIDECAR_DIR, "h323_sidecar.py")

# ---------------------------------------------------------------- G.711 µ-law ---
# Канонический G.711 µ-law (RFC 3551 §4.5.14): 8 кГц, 14-битный динамический
# диапазон; используется только для того, чтобы бот отправлял валидный сигнал,
# который MCU декодирует без предупреждений.
ULAW_BIAS = 0x84     # 132
ULAW_CLIP = 32635


def pcm16_to_ulaw(sample: int) -> int:
    sign = (sample >> 8) & 0x80
    if sign:
        sample = -sample
    sample = min(sample, ULAW_CLIP) + ULAW_BIAS
    exponent = 7
    mask = 0x4000
    while exponent > 0 and not (sample & mask):
        exponent -= 1
        mask >>= 1
    mantissa = (sample >> (exponent + 3)) & 0x0F
    return ~(sign | (exponent << 4) | mantissa) & 0xFF


def ulaw_to_pcm16(byte: int) -> int:
    byte = (~byte) & 0xFF
    sign = byte & 0x80
    exponent = (byte >> 4) & 0x07
    mantissa = byte & 0x0F
    magnitude = (((mantissa << 3) + ULAW_BIAS) << exponent) - ULAW_BIAS
    return -magnitude if sign else magnitude


# ------------------------------------------------------------------ RTP ---
def rtp_packet(pt: int, seq: int, ts: int, ssrc: int, marker: bool, payload: bytes) -> bytes:
    return struct.pack("!BBHII", 0x80, (0x80 if marker else 0) | (pt & 0x7F), seq & 0xFFFF,
                       ts & 0xFFFFFFFF, ssrc & 0xFFFFFFFF) + payload


def parse_rtp(data: bytes):
    if len(data) < 12 or (data[0] >> 6) != 2:
        return None
    cc = data[0] & 0x0F
    pt = data[1] & 0x7F
    if 192 <= pt <= 223:
        # RTCP (SR/RR/SDES/BYE) на RTP-порту (RFC 5761 mux): версия тоже 2,
        # без фильтра его поля читаются как «скачок ts» → jitter 60 000 мс (поймано смоуком G.722)
        return None
    seq, ts, ssrc = struct.unpack("!HII", data[2:12])
    header = 12 + cc * 4
    if data[0] & 0x10:  # extension
        if len(data) < header + 4:
            return None
        _, words = struct.unpack("!HH", data[header:header + 4])
        header += 4 + words * 4
    if len(data) < header:
        return None
    return pt, seq, ts, ssrc, data[header:]


class CallStats:
    def __init__(self, call_id: str, codec: str):
        self.call_id = call_id
        self.codec = codec
        self.disposition = "pending"
        self.reason = ""
        self.t_created = time.monotonic()
        self.t_connected = None
        self.rx_packets = 0
        self.rx_bytes = 0
        self.expected = 0
        self.lost = 0
        self.dup = 0
        self.reorder = 0
        self.max_seq = None
        self.jitter = 0.0
        self.prev_ts = None
        self.prev_arrival = None
        self.clock = 8000
        self.remote_ip = ""
        self.remote_port = 0
        self.tx_packets = 0
        self.send_lag_max_us = 0
        self.send_lag_sum_us = 0
        self.send_lag_n = 0
        self.remote_pt = None
        # видео (H.241/H.264): включается --video-profile; None = только аудио
        self.first_rx = None
        self.last_rx = None
        self.has_video = False
        self.video_reason = ""
        self.video = None              # sipbot.StreamStats(90000)
        self.video_frames = 0          # принятых AU (по marker-биту)
        self.video_tx_packets = 0
        self.video_remote = None

    def on_rx(self, pt: int, seq: int, ts: int, payload_len: int) -> None:
        now = time.monotonic()
        self.rx_packets += 1
        self.rx_bytes += payload_len
        if self.first_rx is None:
            self.first_rx = now
        self.last_rx = now
        if self.max_seq is None:
            self.max_seq = seq
            self.expected = 1
        else:
            delta = (seq - self.max_seq) & 0xFFFF
            if delta == 0:
                self.dup += 1
            elif delta < 0x8000:
                if delta > 1:
                    self.lost += delta - 1
                self.expected += delta
                self.max_seq = seq
            else:
                self.reorder += 1
        if self.prev_ts is not None:
            arrival = now - self.prev_arrival
            transit = arrival - (ts - self.prev_ts) / self.clock
            self.jitter += (abs(transit) - self.jitter) / 16.0
        self.prev_ts, self.prev_arrival = ts, now

    def rx_span(self) -> float:
        if self.first_rx is None or self.last_rx is None:
            return 0.0
        return self.last_rx - self.first_rx

    def loss_pct(self) -> float:
        total = self.expected + self.lost
        return 0.0 if total <= 0 else self.lost / total * 100.0

    def mos(self) -> float:
        # E-model G.107, узкополосный дефолт; для synthetic-bitstream не показывать
        loss = self.loss_pct()
        ie = 0.0 if self.codec == "g711u" else 7.0
        ie_eff = ie + (95.0 - ie) * (loss / (loss + 25.1)) if loss else ie
        d = 20.0  # codec delay 20 мс, OWD на loopback ~0
        i_d = 0.024 * d
        r = max(0.0, min(100.0, 93.2 - 1.4 - i_d - ie_eff))
        mos = 1.0 + 0.035 * r + 7e-6 * r * (r - 60.0) * (100.0 - r)
        return round(max(1.0, min(4.5, mos)), 3)

    def report(self) -> dict:
        connected = self.disposition == "connected"
        return {
            "call_id": self.call_id,
            "disposition": self.disposition,
            "reason": self.reason,
            "setup_ms": None if self.t_connected is None else round((self.t_connected - self.t_created) * 1000, 1),
            "remote_ip": self.remote_ip,
            "remote_port": self.remote_port,
            "remote_payload_type": self.remote_pt,
            "codec": self.codec,
            "bitstream": "synthetic" if self.codec == "g722" else "g711",
            "rx_packets": self.rx_packets,
            "rx_bytes": self.rx_bytes,
            "tx_packets": self.tx_packets,
            "lost": self.lost,
            "dup": self.dup,
            "reorder": self.reorder,
            "loss_pct": round(self.loss_pct(), 4),
            "jitter_ms": round(self.jitter * 1000.0, 3),
            "rtt_ms": None,          # RTCP в sidecar не реализован → честно пусто
            "mos": self.mos() if connected and self.codec != "g722" else None,
            "has_video": self.has_video,
            "video_reason": self.video_reason,
            "video": self.video.report() if self.video is not None else None,
            "video_tx_packets": self.video_tx_packets,
            "fps": (self.video_frames / self.rx_span()
                    if self.has_video and self.video_frames and self.rx_span() > 0 else None),
            "send_lag_p99_us": round(self.send_lag_sum_us / self.send_lag_n, 1) if self.send_lag_n else None,
            "send_lag_max_us": self.send_lag_max_us,
        }


# --------------------------------------------------------------- одна сессия ---
class Call:
    def __init__(self, index: int, args, report_dir: str):
        self.index = index
        self.args = args
        self.report_dir = report_dir
        # профиль называется h323-m2, а не *g722*: прежняя проверка "g722" in profile
        # не срабатывала и бот слал µ-law с PT 0 в канал G.722 (поймано smoke-тестом).
        self.codec = "g722" if args.profile in ("h323-m2", "g722", "g722u") else "g711u"
        self.pt = 9 if self.codec == "g722" else 0
        # 4 порта на вызов: аудио (p, p+1), видео (p+2, p+3). Шаг 2 был багом:
        # видео вызова i совпадало с аудио вызова i+1 (SO_REUSEADDR → пакеты видео
        # попадали в аудио-сокет соседа → jitter 62 869 мс / loss 4.8 %, поймано смоуком).
        self.rtp_port = args.rtp_base_port + 4 * index
        self.rtcp_port = self.rtp_port + 1
        self.video_profile = getattr(args, "video_profile", "none")
        self.video_port = (self.rtp_port + 2) if self.video_profile not in ("none", "", None) else None
        self.video_sock: socket.socket | None = None
        self.video_tx_sock: socket.socket | None = None
        self.video_remote = None
        self.call_id = str(uuid.uuid4())
        self.call_ref = (index % 0x7FFF) + 1
        self.stats = CallStats(self.call_id, self.codec)
        self.stop = threading.Event()
        self.proc: subprocess.Popen | None = None
        self.tx_sock: socket.socket | None = None
        self.rx_sock: socket.socket | None = None
        self.remote = None

    # -- sidecar -------------------------------------------------------------
    def start_sidecar(self) -> None:
        target_host, target_port = self.args.target.rsplit(":", 1)
        config = {
            "target_ip": target_host,
            "target_port": int(target_port),
            "local_ip": self.args.local_ip,
            "audio_port": self.rtp_port,
            "video_port": self.video_port,
            # H.241 generic H.264: 1080p30 требует max bitrate 2500 кбит/с = 25000 (100bps),
            # level 4.0 = 40. Дефолт sidecar (5120 / 71) для 1080p30 мал.
            # H.241 level-коды sidecar: 71 = H.264 3.1 (720p30), 80 = 4.0 (1080p30);
            # допустимы только 70/71/72/80/81/82/90/91 (sidecar:228), 31/40 — ProtocolError
            "video_max_bitrate_100bps": 25000 if self.video_profile == "1080p30" else 15000,
            "video_h264_level": 80 if self.video_profile == "1080p30" else 71,
            "video_h264_profile_bits": 64,
            "timeout_seconds": min(180, self.args.duration + 30),
            "call_id": self.call_id,
            "call_ref": self.call_ref,
            "trace": False,
            "target_alias": self.args.alias,
        }
        self.proc = subprocess.Popen(
            [sys.executable, SIDECAR],
            cwd=SIDECAR_DIR,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write(json.dumps(config) + "\n")
        self.proc.stdin.flush()

    def read_events(self) -> None:
        """Читать события sidecar НЕ блокируясь на EOF stdout.

        Раньше здесь был `for line in self.proc.stdout`: цикл завершался только
        когда sidecar закрывал stdout (через timeout_seconds), поэтому бот висел
        весь setup и не записывал отчёт (поймано: пустой /tmp/h323b). Теперь
        select по fd stdout с дедлайном: connected/disconnected/failed приходят
        сразу, а по дедлайну выходим сами.
        """
        assert self.proc and self.proc.stdout
        fd = self.proc.stdout.fileno()
        deadline = time.monotonic() + min(60.0, self.args.duration / 2 + 10)
        buf = b""
        while not self.stop.is_set() and time.monotonic() < deadline:
            ready, _, _ = select.select([fd], [], [], 0.1)
            if not ready:
                continue
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            buf += chunk
            while b"\n" in buf:
                raw, buf = buf.split(b"\n", 1)
                if not raw.strip():
                    continue
                try:
                    event = json.loads(raw)
                except ValueError:
                    continue
                name = event.get("event")
                if name == "connected":
                    self.stats.disposition = "connected"
                    self.stats.t_connected = time.monotonic()
                    self.stats.remote_ip = event.get("remote_ip") or ""
                    self.stats.remote_port = int(event.get("remote_audio_port") or 0)
                    self.stats.remote_pt = event.get("remote_audio_payload_type")
                    self.remote = (self.stats.remote_ip, self.stats.remote_port)
                    self._start_media()
                elif name == "channel":
                    if event.get("direction") == "outgoing" and event.get("kind") == "video":
                        self.stats.has_video = True
                        self.stats.video_remote = (event.get("remote_ip"), event.get("remote_port"))
                        self.stats.video_pt = event.get("payload_type")
                        self.stats.video_reason = ""
                        self._start_video_tx(event.get("remote_ip"), int(event.get("remote_port") or 0),
                                             int(event.get("payload_type") or 96))
                    elif event.get("kind") == "video" and event.get("direction") == "incoming":
                        # входящий канал видео: sidecar уже открыл OLC на наш video_port
                        self.stats.has_video = True
                elif name == "video_unavailable":
                    self.stats.video_reason = event.get("reason") or "video_unavailable"
                elif name == "disconnected":
                    self.stats.reason = event.get("reason") or ""
                    self.stop.set()
                    break
                elif name == "failed":
                    if self.stats.disposition != "connected":
                        self.stats.disposition = "failed"
                        self.stats.reason = event.get("reason") or ""
                    self.stop.set()
                    break
        if self.stats.disposition != "connected":
            self.stats.disposition = "timeout"
            self.stats.reason = "setup_timeout"

    # -- media ---------------------------------------------------------------
    def _start_media(self) -> None:
        self.rx_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.rx_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        # 4 МБ rx-буфера: при смеси 10+10 host-нагрузка задерживает python-потоки
        # (GIL) и дефолтных ~200 пакетов не хватает → RcvbufErrors (атрибуция 20:5x:
        # 14 481 ошибка ≈ 14 403 «потерянных» видео-пакетов, MCU не виноват)
        self.rx_sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
        self.rx_sock.bind((self.args.local_ip, self.rtp_port))
        self.rx_sock.setblocking(False)
        self.tx_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        # Источник RTP — эфемерный порт: UAS сверяет IP, а не порт; иначе
        # порт отправителя пересекался бы с RTP-портами других вызовов.
        self.tx_sock.bind((self.args.local_ip, 0))
        if self.video_port:
            self.video_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            self.video_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            self.video_sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
            self.video_sock.bind((self.args.local_ip, self.video_port))
            self.video_sock.setblocking(False)
            self.stats.video = sipbot.StreamStats(90000)
            threading.Thread(target=self._video_rx_loop, daemon=True).start()
            if self.video_remote:
                self._start_video_tx(*self.video_remote, self.stats.video_pt)
        threading.Thread(target=self._rx_loop, daemon=True).start()
        threading.Thread(target=self._tx_loop, daemon=True).start()

    def _rx_loop(self) -> None:
        sel = selectors.DefaultSelector()
        sel.register(self.rx_sock, selectors.EVENT_READ)
        while not self.stop.is_set():
            for _ in sel.select(timeout=0.05):
                try:
                    data, _ = self.rx_sock.recvfrom(2048)
                except BlockingIOError:
                    continue
                parsed = parse_rtp(data)
                if parsed:
                    pt, seq, ts, _, payload = parsed
                    self.stats.on_rx(pt, seq, ts, len(payload))
                    self._dump_rtp("audio", pt, seq, ts, len(payload))

    def _tx_loop(self) -> None:
        if not (self.tx_sock and self.remote):
            return
        ssrc = (self.index + 1) * 0x01010101
        seq = 0
        ts = 0
        step = 160
        t0 = time.monotonic()
        tick = 0
        while not self.stop.is_set():
            target = t0 + tick * 0.02
            now = time.monotonic()
            if now < target:
                time.sleep(min(target - now, 0.02))
            tick += 1
            # 20 мс тон 1 кГц в µ-law; для G.722 — синтетический payload (см. docstring)
            if self.codec == "g711u":
                payload = bytes(
                    pcm16_to_ulaw(int(6000 * math.sin(2 * math.pi * 1000 * (ts + n) / 8000)))
                    for n in range(step)
                )
            else:
                payload = bytes(((i * 37 + tick) & 0xFF) for i in range(160))
            packet = rtp_packet(self.pt, seq, ts, ssrc, False, payload)
            try:
                self.tx_sock.sendto(packet, self.remote)
            except OSError:
                break
            lag_us = (time.monotonic() - target) * 1e6
            self.stats.send_lag_sum_us += lag_us
            self.stats.send_lag_n += 1
            self.stats.send_lag_max_us = max(self.stats.send_lag_max_us, int(lag_us))
            seq += 1
            ts += step
            self.stats.tx_packets += 1

    # -- видео (H.241/H.264) -------------------------------------------------
    def _dump_rtp(self, kind: str, pt: int, seq: int, ts: int, payload_len: int) -> None:
        """Первые 40 RTP-заголовков потока на вызов — в --rx-dump (разбор аномалий G.722/RTCP)."""
        path = self.args.rx_dump
        if not path:
            return
        counter = self.__dict__.setdefault("_dumped", 0)
        if counter >= 40:
            return
        self._dumped = counter + 1
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "a") as fh:
            fh.write(f"{self.index}\t{kind}\tpt={pt}\tseq={seq}\tts={ts}\tlen={payload_len}\n")

    def _start_video_tx(self, remote_ip: str, remote_port: int, pt: int) -> None:
        if not remote_ip or not remote_port or self.stop.is_set():
            return
        if self.video_tx_sock is not None:
            return
        self.video_tx_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.video_tx_sock.bind((self.args.local_ip, 0))
        threading.Thread(target=self._video_tx_loop,
                         args=(remote_ip, remote_port, pt), daemon=True).start()

    def _video_tx_loop(self, remote_ip: str, remote_port: int, pt: int) -> None:
        """H.264 1080p30/720p30: те же AUs, что SIP-бот, pacing по абсолютным дедлайнам."""
        aus, fps = sipbot.ensure_video(self.video_profile)
        remote = (remote_ip, remote_port)
        ssrc = 0x5A5A0000 | (self.index + 1)
        seq = 0
        t0 = time.monotonic()
        frame = 0
        while not self.stop.is_set():
            target = t0 + frame / fps
            now = time.monotonic()
            if now < target:
                time.sleep(min(target - now, 0.02))
                continue
            au = aus[frame % len(aus)]
            ts = (frame * 90000 // fps) & 0xFFFFFFFF
            packets, seq = sipbot.packetize_video(au, pt, seq, ts, ssrc)
            for packet in packets:
                try:
                    self.video_tx_sock.sendto(packet, remote)
                except OSError:
                    return
                self.stats.video_tx_packets += 1
            frame += 1

    def _video_rx_loop(self) -> None:
        assert self.video_sock is not None
        sel = selectors.DefaultSelector()
        sel.register(self.video_sock, selectors.EVENT_READ)
        while not self.stop.is_set():
            for _ in sel.select(timeout=0.05):
                try:
                    data, _ = self.video_sock.recvfrom(2048)
                except BlockingIOError:
                    continue
                parsed = sipbot.parse_rtp(data)
                if not parsed:
                    continue
                pt, seq, ts, _ssrc, marker, payload = parsed
                self._dump_rtp("video", pt, seq, ts, len(payload))
                if self.stats.video is None:
                    self.stats.video = sipbot.StreamStats(90000)
                self.stats.video.on_packet(seq, ts, len(payload), time.monotonic(), 60.0)
                if marker:
                    self.stats.video_frames += 1
                    self.stats.has_video = True

    # -- жизненный цикл ------------------------------------------------------
    def run(self) -> None:
        try:
            self.start_sidecar()
            # События читаем в фоне: read_events блокирует до connect+deadline, а
            # медиа-окно должно отсчитываться от t_created (иначе бот живёт
            # setup_deadline=17.5 с вместо duration=15 с и tx завышается: 863 вместо 750).
            threading.Thread(target=self.read_events, daemon=True).start()
            remaining = max(0.0, self.args.duration - (time.monotonic() - self.stats.t_created))
            end = time.monotonic() + remaining
            while time.monotonic() < end and not self.stop.is_set():
                time.sleep(0.2)
        except Exception as error:  # noqa: BLE001 — бот не должен падать от одной сессии
            self.stats.disposition = "failed"
            self.stats.reason = f"{type(error).__name__}: {error}"
        finally:
            self.stop.set()
            try:
                if self.proc and self.proc.stdin:
                    self.proc.stdin.close()  # EOF -> sidecar завершает вызов штатно
                if self.proc:
                    try:
                        self.proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        self.proc.kill()
            except OSError:
                pass
            for sock in (self.tx_sock, self.rx_sock, self.video_tx_sock, self.video_sock):
                if sock:
                    sock.close()
            self.write_report()

    def write_report(self) -> None:
        os.makedirs(self.report_dir, exist_ok=True)
        path = os.path.join(self.report_dir, f"h323-{self.index:03d}.jsonl")
        with open(path, "w") as fh:
            fh.write(json.dumps(self.stats.report(), ensure_ascii=False) + "\n")


class McuSampler:
    """Контур MCU по cgroup — тот же способ, что в `matrix.py`/`sipbot.py`.

    Нужен, чтобы H.323-нога давала не только потери/MOS, но и CPU-цену на устройство
    (для аудио-профилей M0/M1 это единственная живая точка: SIP-серия их не мерила).
    """

    def __init__(self, pid: int, idle_s: float):
        self.pid = pid
        self.idle_s = idle_s
        self.idle: list[dict] = []
        self.active: list[dict] = []
        self.stop = threading.Event()

    def sample_idle(self) -> None:
        t0 = time.monotonic()
        while time.monotonic() - t0 < self.idle_s:
            self.idle.append(sipbot.mcu_contour_sample(self.pid))
            time.sleep(1)

    def start(self) -> None:
        def loop() -> None:
            while not self.stop.is_set():
                self.active.append(sipbot.mcu_contour_sample(self.pid))
                time.sleep(1)
        threading.Thread(target=loop, daemon=True).start()

    @staticmethod
    def _pcts(samples: list[dict]) -> list[float]:
        """% одного ядра по дельтам cpu.stat.usage_usec (1 ядро = 1e6 мкс/с).

        В сэмпле нет готового `usage_pct` (его считает matrix.py на парах срезов),
        поэтому здесь то же самое локально — иначе c_device молча получался 0.
        """
        out = []
        for a, b in zip(samples, samples[1:]):
            dt = b["at"] - a["at"]
            if dt > 0:
                out.append((b["usage_usec"] - a["usage_usec"]) / (dt * 1e6) * 100.0)
        return out

    def summary(self) -> dict:
        idle = self._pcts(self.idle)
        active = self._pcts(self.active)
        p_idle = round(sum(idle) / len(idle), 3) if idle else 0.0
        p_active = round(sum(active) / len(active), 3) if active else 0.0
        return {
            "p_idle_pct_one_core": p_idle,
            "p_active_pct_one_core": p_active,
            "c_device_pct_one_core": round(max(0.0, p_active - p_idle), 3),
            "ffmpeg_count_max": max((x.get("ffmpeg_count", 0) for x in self.active), default=0),
            "pids_max": max((x.get("pids", 0) for x in self.active), default=0),
            "threads_max": max((x.get("threads", 0) for x in self.active), default=0),
            "rss_kb_max": max((x.get("rss_kb", 0) for x in self.active), default=0),
            "active_samples": len(self.active),
            "idle_samples": len(self.idle),
        }


# -------------------------------------------------------------------- main ---
def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="H.323-бот (sidecar как UAC + собственный RTP)")
    ap.add_argument("--target", default="127.0.0.1:1720")
    ap.add_argument("--alias", required=True, help="room UUID / route / имя комнаты")
    ap.add_argument("--local-ip", default="127.0.0.1")
    ap.add_argument("--calls", type=int, default=1)
    ap.add_argument("--signal-base-port", type=int, default=2720)
    ap.add_argument("--rtp-base-port", type=int, default=30000)
    ap.add_argument("--profile", default="h323-m1", help="h323-m1 (G.711U) | h323-m2 (G.722)")
    ap.add_argument("--rx-dump", default="", help="дамп первых 40 RTP-заголовков на вызов (разбор аномалий)")
    ap.add_argument("--video-profile", default="none",
                    help="none | 720p30 | 1080p30 — H.241/H.264 видео по H.323 (третья пара портов на вызов)")
    ap.add_argument("--duration", type=float, default=150)
    ap.add_argument("--ramp", type=float, default=2.0, help="вызовов в секунду")
    ap.add_argument("--report", required=True)
    ap.add_argument("--mcu-pid", type=int, default=0, help="PID vks-server: снять cgroup-цену контура")
    ap.add_argument("--idle-baseline", type=float, default=6.0, help="с idle-сэмплов до старта, 1/с")
    args = ap.parse_args(argv)

    if not os.path.exists(SIDECAR):
        print(f"[h323bot] FATAL: нет sidecar {SIDECAR}", file=sys.stderr)
        return 1
    if args.calls > 20:
        # MAX_CALLS=20 / MAX_PER_IP=20 у живого H.323 UAS (env /proc/<listener>/environ)
        print("[h323bot] WARN: живой UAS имеет MAX_CALLS=20/MAX_PER_IP=20, "
              "вызовы сверх 20 получат отказ", file=sys.stderr)

    os.makedirs(args.report, exist_ok=True)
    mcu = None
    exe_before = ""
    load_before = float(open("/proc/loadavg").read().split()[0])
    if args.mcu_pid:
        exe_before = sipbot.exe_sha256(args.mcu_pid)
        mcu = McuSampler(args.mcu_pid, args.idle_baseline)
        print(f"[h323bot] idle-база {args.idle_baseline:.0f} с (контур MCU)…", flush=True)
        mcu.sample_idle()
        mcu.start()
    interval = 1.0 / args.ramp if args.ramp > 0 else 0.0
    calls = [Call(i, args, args.report) for i in range(args.calls)]
    threads = []
    for i, call in enumerate(calls):
        thread = threading.Thread(target=call.run, daemon=False)
        thread.start()
        threads.append(thread)
        if interval:
            time.sleep(interval)      # ramp: не устраиваем burst в Q.931 listener
    for thread in threads:
        thread.join()
    if mcu:
        mcu.stop.set()
        time.sleep(0.3)
    connected = sum(1 for c in calls if c.stats.disposition == "connected")
    summary = {
        "n_calls": args.calls,
        "connected": connected,
        "profile": args.profile,
        "codec": calls[0].codec if calls else None,
        "has_video": any(c.stats.has_video for c in calls),
        "duration_s": args.duration,
        "ramp_per_s": args.ramp,
        "loadavg_before": load_before,
        "loadavg_after": float(open("/proc/loadavg").read().split()[0]),
    }
    if mcu:
        summary.update(mcu.summary())
        exe_after = sipbot.exe_sha256(args.mcu_pid)
        summary["mcu_exe_sha256_before"] = exe_before
        summary["mcu_exe_sha256_after"] = exe_after
        summary["mcu_exe_stable"] = exe_before == exe_after
        summary["valid"] = summary["mcu_exe_stable"] and connected == args.calls
    rx = [c.stats.rx_packets for c in calls]
    tx = [c.stats.tx_packets for c in calls]
    losses = [c.stats.loss_pct() for c in calls]
    mos = [c.stats.mos() for c in calls if c.stats.rx_packets]
    summary["rx_packets"] = {"min": min(rx), "max": max(rx), "mean": round(sum(rx) / len(rx), 1)}
    summary["tx_packets"] = {"min": min(tx), "max": max(tx), "mean": round(sum(tx) / len(tx), 1)}
    summary["loss_pct"] = {"mean": round(sum(losses) / len(losses), 4), "max": round(max(losses), 4)}
    summary["mos"] = {"mean": round(sum(mos) / len(mos), 3), "min": round(min(mos), 3)} if mos else None
    # видео-нога (если --video-profile): потери/fps по принятым потокам
    vids = [c.stats.video.report() for c in calls
            if c.stats.video is not None and c.stats.has_video and c.stats.video.base_seq is not None]
    if vids:
        vloss = [v["loss_pct"] for v in vids]
        vfps = [(c.stats.video_frames / c.stats.rx_span()) for c in calls
                if c.stats.has_video and c.stats.video_frames and c.stats.rx_span() > 0]
        summary["video_rx_packets"] = {"mean": round(sum(v["packets"] for v in vids) / len(vids), 1)}
        summary["video_loss_pct"] = {"mean": round(sum(vloss) / len(vloss), 4), "max": round(max(vloss), 4)}
        summary["video_fps"] = {"mean": round(sum(vfps) / len(vfps), 2)} if vfps else None
        summary["video_jitter_ms"] = {"mean": round(sum(v["jitter_ms"] for v in vids) / len(vids), 3)}
    summary["calls"] = [c.stats.report() for c in calls]
    with open(os.path.join(args.report, "summary.json"), "w") as fh:
        json.dump(summary, fh, ensure_ascii=False, indent=1)
    print(f"[h323bot] connected={connected}/{args.calls} -> {args.report}")
    if mcu:
        print(f"[h323bot] c_device={summary['c_device_pct_one_core']}% ядра, "
              f"loss={summary['loss_pct']}, mos={summary['mos']}, valid={summary['valid']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
