#!/usr/bin/env python3
"""matrix.py — N SIP-ботов синхронно + замер контура MCU + results.json.

Запускает N процессов `sipbot.py` (один бот = один процесс = один вызов),
стартует медиа у всех в один момент (`--start-at`), пока они устанавливают
вызовы, снимает базовую линию «простоя», затем каждую секунду пишет срез
контура MCU (cgroup `cpu.stat`, `cpu.pressure`, дерево PID, FFmpeg-дети,
pids/threads/RSS) и по завершении собирает per-SSRC статистику ботов.

Атрибуция артефакта: sha256 `/proc/<mcu_pid>/exe` до и после серии
(заморозка сборки, см. docs/audit/2026-10-25/06_perf_lab.md §3.6).
Если хеш изменился — `valid=false`, прогон в выводы по ёмкости не идёт.

Примеры:
  # калибровка: 1 бот, 720p30, 120 с, базовая линия 5 с
  scripts/bench/matrix.py --bots 1 --video-profile 720p30 --duration 120 --room <UUID>

  # ряд калибровки и 20 ботов
  for n in 1 2 5 10 20; do
    scripts/bench/matrix.py --bots $n --video-profile 720p30 --duration 120 --room <UUID>
  done
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import statistics
import subprocess
import sys
import threading
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import sipbot  # noqa: E402  (переиспользуем sampling/кодеки/профили)

DEFAULT_OUT = "/tmp/vks-bench/runs"


def log(msg: str) -> None:
    print(f"[matrix] {msg}", flush=True)


def sha256_file_like_proc(path: str) -> str:
    h = hashlib.sha256()
    try:
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return "unreadable"


def read_cpu_pressure(cgroup: str) -> dict:
    out = {}
    try:
        with open(os.path.join(cgroup, "cpu.pressure")) as fh:
            for line in fh:
                name, _, rest = line.partition(" ")
                fields = dict(part.split("=") for part in rest.split() if "=" in part)
                out[name] = float(fields.get("avg10", 0.0))
    except OSError:
        pass
    return out


def sample_contour(mcu_pid: int) -> dict:
    sample = sipbot.mcu_contour_sample(mcu_pid)
    sample["pressure"] = read_cpu_pressure(sample["cgroup"])
    net = {}
    try:
        with open("/proc/net/udp") as fh:
            net["udp_inuse"] = max(0, len(fh.readlines()) - 1)
        with open("/proc/net/snmp") as fh:
            for line in fh:
                if line.startswith("Udp:"):
                    net["udp_snmp"] = line.split()[1:]
                    break
    except OSError:
        pass
    sample["net"] = net
    return sample


def _farm_agg(doc: dict) -> dict:
    """Свернуть JSON `/api/v3/mcu/farm` в плоскую строку метрик.

    Чистая функция (тестируется без сети). Нужна T2: под loss/jitter должны
    расти `keyframe_requests`/`dropped_frames`/`restarts`, а не только потери
    на боте — иначе netem-матрица слепа к реакции фермы.
    """
    groups = doc.get("groups") or []
    agg = {"groups": len(groups), "viewers": 0, "frames": 0, "keyframes": 0,
           "dropped_frames": 0, "restarts": 0, "keyframe_requests": 0,
           "viewer_rtp_packets": 0, "viewer_rtp_bytes": 0, "viewer_rtp_dropped": 0}
    for g in groups:
        vrtp = g.get("viewer_rtp") or {}
        agg["viewers"] += int(g.get("viewers") or 0)
        agg["frames"] += int(g.get("frames") or 0)
        agg["keyframes"] += int(g.get("keyframes") or 0)
        agg["dropped_frames"] += int(g.get("dropped_frames") or 0)
        agg["restarts"] += int(g.get("restarts") or 0)
        agg["keyframe_requests"] += int(g.get("keyframe_requests") or 0)
        agg["viewer_rtp_packets"] += int(vrtp.get("packets") or 0)
        agg["viewer_rtp_bytes"] += int(vrtp.get("bytes") or 0)
        agg["viewer_rtp_dropped"] += int(vrtp.get("dropped_frames") or 0)
    return agg


def farm_snapshot(api: str) -> dict:
    """Снимок фермы энкодеров (`P4a`) с таймаутом 2 с; `{}` — API недоступен."""
    try:
        with urllib.request.urlopen(f"http://{api}/api/v3/mcu/farm", timeout=2.0) as resp:
            return _farm_agg(json.loads(resp.read().decode()))
    except Exception:
        return {}


def run_mcu_pid(explicit: int) -> int:
    if explicit:
        return explicit
    try:
        out = subprocess.run(["systemctl", "--user", "show", "-p", "MainPID", "--value", "vks-mcu.service"],
                             capture_output=True, text=True, timeout=5).stdout.strip()
        return int(out) if out.isdigit() else 0
    except Exception:  # noqa: BLE001
        return 0


def spawn_bot(idx: int, args, start_at: float, outdir: str) -> subprocess.Popen:
    cmd = [
        sys.executable, os.path.join(HERE, "sipbot.py"),
        "--room", args.room,
        "--host", args.host,
        "--sip-port", str(args.sip_port),
        "--local-ip", args.local_ip,
        "--audio-codec", args.audio_codec,
        "--video-profile", args.video_profile,
        "--duration", str(args.duration),
        "--start-at", f"{start_at:.3f}",
        "--rtp-jitter-buffer", str(args.rtp_jitter_buffer),
        "--json-out", os.path.join(outdir, f"bot-{idx:02d}.json"),
        "--tag", f"bot-{idx:02d}",
        "--index", str(idx),
        "--mcu-pid", str(args.mcu_pid or 0),
        "--skip-exe-hash",  # паспорт снимает matrix.py один раз до/после, не N ботов
        "--no-mcu-cpu",  # CPU контура меряет matrix.py, иначе N ботов посчитают одно и то же
    ]
    if args.dump_rx and f"bot-{idx:02d}" in args.dump_rx.split(","):
        cmd += ["--dump-rx", os.path.join(outdir, f"rx-{idx:02d}.jsonl")]
    if args.bot_cores:
        cmd = ["taskset", "-c", args.bot_cores] + cmd
    logfile = open(os.path.join(outdir, f"bot-{idx:02d}.log"), "w")
    return subprocess.Popen(cmd, stdout=logfile, stderr=subprocess.STDOUT)


def _ticks_delta(prev: dict | None, cur: dict | None, dt: float) -> float:
    """% ядра по тикам FFmpeg-детей.

    Сумма тиков по PID некорректна, когда набор детей меняется: новый ffmpeg
    приходит с тиками ~0, старый исчезает вместе со своими тиками → дельта
    может стать отрицательной (поймано: ffmpeg_share = -0.047 на N=1). Поэтому
    считаем по пересечению PID, а новые PID берём целиком (их тики накоплены
    внутри окна). Ушедшие PID не вычитаются — это приближение, помечено полями
    ffmpeg_entered/ffmpeg_left.
    """
    prev = prev or {}
    cur = cur or {}
    if dt <= 0:
        return 0.0
    gained = 0
    for pid, ticks in cur.items():
        if pid in prev:
            gained += max(0, ticks - prev[pid])
        else:
            gained += max(0, ticks)
    return max(0.0, gained / (dt * 100.0) * 100.0)


def percentile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, int(round((q / 100.0) * (len(ordered) - 1)))))
    return ordered[idx]


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="N SIP-ботов + замер контура MCU")
    ap.add_argument("--bots", type=int, required=True, help="сколько одновременных вызовов")
    ap.add_argument("--dump-rx", default="",
                    help="теги ботов через запятую (например bot-00): дамп всех принятых RTP-пакетов "
                         "в rx-<idx>.jsonl (pt, seq, ts, arrival, src) для таймлайн-разбора jb_late")
    ap.add_argument("--extra-decoders", type=int, default=0,
                    help="устройств не из этого прогона (например, H.323-вызовы в смеси): "
                         "вычитаются из ожидания FFmpeg-детей, чтобы orphan не был ложным")
    ap.add_argument("--room", required=True, help="UUID/route/alias комнаты")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--sip-port", type=int, default=5060)
    ap.add_argument("--local-ip", default="192.168.1.93")
    ap.add_argument("--audio-codec", default="PCMU", choices=sorted(sipbot.AUDIO_CODECS))
    ap.add_argument("--video-profile", default="720p30",
                    choices=sorted(sipbot.VIDEO_PROFILES) + ["none"])
    ap.add_argument("--duration", type=float, default=120.0, help="секунд медиа")
    ap.add_argument("--idle-baseline", type=float, default=5.0, help="секунд простоя до старта")
    ap.add_argument("--connect-window", type=float, default=15.0, help="запас на INVITE до старта медиа")
    ap.add_argument("--rtp-jitter-buffer", type=float, default=60.0)
    ap.add_argument("--sample-period", type=float, default=1.0)
    ap.add_argument("--mcu-pid", type=int, default=0)
    ap.add_argument("--api", default="127.0.0.1:8080",
                    help="HTTP API MCU для снимка фермы (P4a); из netns — 192.168.1.93:8080")
    ap.add_argument("--outdir", default="")
    ap.add_argument("--expect-sha256", default="", help="ожидаемый sha256 живого бинаря (заморозка)")
    ap.add_argument("--allow-mismatch", action="store_true", help="пометить прогон invalid, но выполнить")
    ap.add_argument("--allow-partial", action="store_true",
                    help="T2: valid по паспорту, даже если дозвонились не все (потери под netem — ожидаемы)")
    ap.add_argument("--bot-cores", default="", help="taskset-диапазон для ботов, напр. 16-35 (root не нужен)")
    ap.add_argument("--max-load", type=float, default=8.0,
                    help="порог loadavg, выше которого прогон помечается noisy (не отказ)")
    args = ap.parse_args(argv)

    stamp = time.strftime("%Y%m%d-%H%M%S")
    scenario = f"N{args.bots:02d}-{args.video_profile}-{args.audio_codec}"
    outdir = args.outdir or os.path.join(DEFAULT_OUT, f"{stamp}-{scenario}")
    os.makedirs(outdir, exist_ok=True)
    log(f"outdir={outdir} scenario={scenario} duration={args.duration}s")

    args.mcu_pid = run_mcu_pid(args.mcu_pid)
    if not args.mcu_pid or not os.path.isdir(f"/proc/{args.mcu_pid}"):
        log("ОШИБКА: не найден живой vks-server (systemctl --user show MainPID)")
        return 2
    cgroup = sipbot.mcu_cgroup_path(args.mcu_pid)
    exe_before = sha256_file_like_proc(f"/proc/{args.mcu_pid}/exe")
    frozen_expected = args.expect_sha256
    if not frozen_expected:
        passport = os.path.join(os.path.dirname(HERE), "..", "docs", "audit", "2026-10-25",
                                "measure", "PASSPORT.md")
        try:
            with open(passport) as fh:
                m = re.search(r"([0-9a-f]{64})", fh.read())
            frozen_expected = m.group(1) if m else ""
        except OSError:
            frozen_expected = ""
    mismatched = bool(frozen_expected and frozen_expected != exe_before)
    if mismatched:
        log(f"ВНИМАНИЕ: живой бинарь {exe_before[:16]}… != замороженному {frozen_expected[:16]}…")
        if not args.allow_mismatch:
            log("отказ: прогон неатрибутируем. Нужна заморозка сборки (§3.6) или --allow-mismatch")
            return 2
    load1 = float(open("/proc/loadavg").read().split()[0])
    def _allowed(pid: int) -> str:
        try:
            return next(l.split()[1] for l in open(f"/proc/{pid}/status") if l.startswith("Cpus_allowed_list"))
        except OSError:
            return "?"
    mcu_allowed = _allowed(args.mcu_pid)
    noisy = load1 > args.max_load
    log(f"mcu_pid={args.mcu_pid} cgroup={cgroup} exe_sha256={exe_before[:16]}… frozen_ok={not mismatched}")
    log(f"mcu_cpus_allowed={mcu_allowed} bots_cores={args.bot_cores or 'не пиннятся'} "
        f"load1={load1:.2f} noisy={noisy}")

    # --- медиа генерируется один раз, чтобы N ботов не делали это одновременно ---
    if args.video_profile != "none":
        sipbot.ensure_video(args.video_profile)
    if args.audio_codec == "G722":
        sipbot.ensure_g722()

    # --- базовая линия простоя ---
    samples: list[dict] = []
    idle_samples: list[dict] = []
    farm_samples: list[dict] = []
    stop_flag = threading.Event()
    started = time.time()
    if args.idle_baseline < 2 * args.sample_period:
        log(f"ВНИМАНИЕ: idle-baseline {args.idle_baseline} с < 2 × период {args.sample_period}: P_idle будет 0")
    log(f"базовая линия {args.idle_baseline:.1f} с (простой MCU)…")
    while time.time() - started < args.idle_baseline:
        idle_samples.append(sample_contour(args.mcu_pid))
        time.sleep(args.sample_period)
    idle_usage = [(idle_samples[i]["usage_usec"] - idle_samples[i - 1]["usage_usec"]) /
                  ((idle_samples[i]["at"] - idle_samples[i - 1]["at"]) * 1e6) * 100.0
                  for i in range(1, len(idle_samples))]
    p_idle = statistics.median(idle_usage) if idle_usage else 0.0
    log(f"P_idle={p_idle:.2f} % одного ядра")

    # --- синхронный старт: все боты получают один и тот же start_at ---
    start_at = time.time() + args.connect_window
    procs: list[subprocess.Popen] = []
    for idx in range(args.bots):
        procs.append(spawn_bot(idx, args, start_at, outdir))
        time.sleep(0.02)  # не бомбим MCU INVITE-ами в один микросекунд
    log(f"запущено {len(procs)} ботов, медиа стартует в {start_at - time.time():.1f} с")

    def sampler() -> None:
        while not stop_flag.is_set():
            samples.append(sample_contour(args.mcu_pid))
            farm_samples.append(farm_snapshot(args.api))
            stop_flag.wait(args.sample_period)

    sampler_thread = threading.Thread(target=sampler, daemon=True)
    sampler_thread.start()

    deadline = start_at + args.duration + 60.0
    while any(p.poll() is None for p in procs) and time.time() < deadline:
        time.sleep(1.0)
    for p in procs:
        if p.poll() is None:
            p.terminate()
    stop_flag.set()
    sampler_thread.join(timeout=5)

    # --- срезы контура внутри окна медиа ---
    active = [s for s in samples if s["at"] >= start_at]
    deltas = []
    for prev, cur in zip(active, active[1:]):
        dt = cur["at"] - prev["at"]
        if dt <= 0.2:
            continue
        deltas.append({
            "dt": dt,
            "usage_pct": (cur["usage_usec"] - prev["usage_usec"]) / (dt * 1e6) * 100.0,
            "user_pct": (cur["user_usec"] - prev["user_usec"]) / (dt * 1e6) * 100.0,
            "sys_pct": (cur["system_usec"] - prev["system_usec"]) / (dt * 1e6) * 100.0,
            "ffmpeg_pct": _ticks_delta(prev.get("ffmpeg_pid_ticks"), cur.get("ffmpeg_pid_ticks"), dt),
            "ffmpeg_entered": len(set(cur.get("ffmpeg_pid_ticks") or {}) - set(prev.get("ffmpeg_pid_ticks") or {})),
            "ffmpeg_left": len(set(prev.get("ffmpeg_pid_ticks") or {}) - set(cur.get("ffmpeg_pid_ticks") or {})),
            "ffmpeg_count": cur["ffmpeg_count"],
            "pids": cur["pids"],
            "threads": cur["threads"],
            "rss_kb": cur["rss_kb"],
            "pressure_some": cur.get("pressure", {}).get("some", 0.0),
        })
    p_active = statistics.mean([d["usage_pct"] for d in deltas]) if deltas else 0.0
    p_ffmpeg = statistics.mean([d["ffmpeg_pct"] for d in deltas]) if deltas else 0.0
    # FFmpeg-дети появляются/исчезают (вход/выход вызова): срезы с уходом детей
    # помечаем нестабильными и не берём в ffmpeg_share, иначе доля «плывёт».
    stable = [d for d in deltas if not d.get("ffmpeg_left")]
    p_enc = p_dec = 0.0
    if stable:
        p_ffmpeg = statistics.mean([d["ffmpeg_pct"] for d in stable])
    p_ffmpeg = max(p_ffmpeg, 0.0)
    del p_enc, p_dec

    # --- сбор результатов ботов ---
    bots: list[dict] = []
    for idx in range(args.bots):
        path = os.path.join(outdir, f"bot-{idx:02d}.json")
        if not os.path.exists(path):
            bots.append({"tag": f"bot-{idx:02d}", "disposition": "missing_json"})
            continue
        with open(path) as fh:
            bots.append(json.load(fh))

    connected = [b for b in bots if b.get("disposition") == "connected"]
    audio_rx = [b["rx"]["audio"] for b in connected if b.get("rx", {}).get("audio", {}).get("packets", 0)]
    video_rx = [b["rx"]["video"] for b in connected if b.get("rx", {}).get("video", {}).get("packets", 0)]
    audio_loss = [r["loss_pct"] for r in audio_rx]
    video_loss = [r["loss_pct"] for r in video_rx]
    audio_mos = [r["mos"] for r in audio_rx if r.get("mos")]
    kernel_drops = sum(b.get("media", {}).get("kernel_drops", 0) for b in connected)
    exe_after = sha256_file_like_proc(f"/proc/{args.mcu_pid}/exe") if os.path.isdir(f"/proc/{args.mcu_pid}") else "gone"
    exe_stable = exe_before == exe_after
    partial = len(connected) < args.bots
    valid = exe_stable and not mismatched and (not partial or args.allow_partial)

    c_device_pct = (p_active - p_idle) / args.bots if args.bots else 0.0
    c_device_cores = c_device_pct / 100.0
    n_per_core = {f"U{u}": int(u / c_device_pct) if c_device_pct > 0 else 0 for u in (70, 85, 95)}
    ffmpeg_share = p_ffmpeg / p_active if p_active else 0.0

    summary = {
        "scenario": scenario,
        "valid": valid,
        "invalid_reason": ("" if valid else
                           ("exe_sha256_changed" if not exe_stable else
                            "frozen_sha256_mismatch" if mismatched else "not_all_connected")),
        "n_bots_requested": args.bots,
        "extra_decoders": args.extra_decoders,
        "n_connected": len(connected),
        "partial_connect": partial,
        "audio_codec": args.audio_codec,
        "video_profile": args.video_profile,
        "duration_s": args.duration,
        "p_idle_pct_one_core": round(p_idle, 3),
        "p_active_pct_one_core": round(p_active, 3),
        "p_contour_cores": round(p_active / 100.0, 4),
        "c_device_pct_one_core": round(c_device_pct, 3),
        "c_device_cores": round(c_device_cores, 4),
        "n_per_core": n_per_core,
        "p_ffmpeg_pct_one_core": round(p_ffmpeg, 3),
        "ffmpeg_share": round(ffmpeg_share, 4),
        "ffmpeg_entered_total": sum(d.get("ffmpeg_entered", 0) for d in deltas),
        "ffmpeg_left_total": sum(d.get("ffmpeg_left", 0) for d in deltas),
        "ffmpeg_stable_samples": len(stable),
        "ffmpeg_count_max": max((d["ffmpeg_count"] for d in deltas), default=0),
        # Детектор утечки фермы: ожидается N декодеров + 1 энкодер на комнату (+ декодеры
        # чужих ног в смеси, --extra-decoders). Всё, что выше, — сироты прошлых групп
        # (поймано на iter4: 26 при 0 абонентов).
        "orphan_ffmpeg_max": max(0, max((d["ffmpeg_count"] for d in deltas), default=0)
                                 - len(connected) - 1 - args.extra_decoders),
        "pids_max": max((d["pids"] for d in deltas), default=0),
        "threads_max": max((d["threads"] for d in deltas), default=0),
        "rss_kb_max": max((d["rss_kb"] for d in deltas), default=0),
        "pressure_some_max": max((d["pressure_some"] for d in deltas), default=0.0),
        "rx_audio": {
            "bots": len(audio_rx),
            "pps_mean": round(statistics.mean([r["packets"] / max(1e-9, r["span_s"]) for r in audio_rx]), 2) if audio_rx else 0.0,
            "loss_pct_mean": round(statistics.mean(audio_loss), 4) if audio_loss else 0.0,
            "loss_pct_max": round(max(audio_loss), 4) if audio_loss else 0.0,
            "jitter_ms_mean": round(statistics.mean([r["jitter_ms"] for r in audio_rx]), 3) if audio_rx else 0.0,
            "mos_mean": round(statistics.mean(audio_mos), 3) if audio_mos else None,
            "jb_late_total": sum(r.get("jb_late", 0) for r in audio_rx),
        },
        "rx_video": {
            "bots": len(video_rx),
            "fps_mean": round(statistics.mean([r["packets"] / max(1e-9, r["span_s"]) for r in video_rx]), 2) if video_rx else 0.0,
            "loss_pct_mean": round(statistics.mean(video_loss), 4) if video_loss else 0.0,
            "loss_pct_max": round(max(video_loss), 4) if video_loss else 0.0,
            "jitter_ms_mean": round(statistics.mean([r["jitter_ms"] for r in video_rx]), 3) if video_rx else 0.0,
            "jb_late_total": sum(r.get("jb_late", 0) for r in video_rx),
        },
        "rx_kernel_drops_total": kernel_drops,
        "tx_audio_pps_mean": round(statistics.mean([b["media"]["audio"]["pps"] for b in connected]), 2) if connected else 0.0,
        "tx_video_fps_mean": round(statistics.mean([b["media"]["video"]["fps"] for b in connected]), 2) if connected else 0.0,
        "bot_self_cpu_s_mean": round(statistics.mean([b["self_cpu"]["user_s"] + b["self_cpu"]["sys_s"] for b in connected]), 3) if connected else 0.0,
        "mcu_pid": args.mcu_pid,
        "mcu_cgroup": cgroup,
        "mcu_cpus_allowed": mcu_allowed,
        "bot_cores": args.bot_cores,
        "loadavg_before": load1,
        "loadavg_after": float(open("/proc/loadavg").read().split()[0]),
        "noisy": noisy,
        "mcu_exe_sha256_before": exe_before,
        "mcu_exe_sha256_after": exe_after,
        "mcu_exe_stable": exe_stable,
        "frozen_expected_sha256": frozen_expected,
        "started_at": started,
        "ended_at": time.time(),
        "idle_samples": len(idle_samples),
        "active_samples": len(deltas),
    }

    farm_active = [f for f in farm_samples if f and f.get("groups")]
    farm_max = {k: max((f.get(k, 0) for f in farm_active), default=0) for k in
                ("groups", "viewers", "frames", "keyframes", "dropped_frames", "restarts",
                 "keyframe_requests", "viewer_rtp_packets", "viewer_rtp_bytes", "viewer_rtp_dropped")}
    summary["farm"] = {
        "samples": len(farm_active),
        "max": farm_max,
        "last": farm_active[-1] if farm_active else {},
        "api": args.api,
    }

    results = {
        "summary": summary,
        "series": deltas,
        "farm_series": farm_samples,
        "bots": bots,
        "argv": argv,
        "host": {"loadavg": open("/proc/loadavg").read().strip(),
                 "nproc": os.cpu_count()},
    }
    with open(os.path.join(outdir, "results.json"), "w") as fh:
        json.dump(results, fh, indent=2, ensure_ascii=False)
    with open(os.path.join(outdir, "summary.md"), "w") as fh:
        fh.write(_markdown(summary, deltas))
    log(f"RESULTS {json.dumps({k: summary[k] for k in ('valid','n_connected','p_active_pct_one_core','c_device_pct_one_core','c_device_cores','n_per_core','ffmpeg_share','rx_audio','rx_video')}, ensure_ascii=False)}")
    log(f"results.json -> {os.path.join(outdir, 'results.json')}")
    return 0 if valid else 1


def _markdown(s: dict, series: list[dict]) -> str:
    lines = [
        f"# Прогон {s['scenario']}",
        "",
        f"- действителен: **{'да' if s['valid'] else 'НЕТ (' + s['invalid_reason'] + ')'}**"
        + (f"  |  **ШУМНО**: loadavg {s['loadavg_before']:.2f} > порога, числа завышены и дисперсны" if s.get("noisy") else ""),
        f"- ботов: {s['n_connected']}/{s['n_bots_requested']}, длительность {s['duration_s']:.0f} с",
        f"- паспорт: `{s['mcu_exe_sha256_before'][:16]}…` → `{s['mcu_exe_sha256_after'][:16]}…`, "
        f"стабилен: {s['mcu_exe_stable']}",
        "",
        "| метрика | значение |",
        "|---|---|",
        f"| P_idle (простой) | {s['p_idle_pct_one_core']:.2f} % ядра |",
        f"| P_active (контур) | {s['p_active_pct_one_core']:.2f} % ядра = {s['p_contour_cores']:.2f} ядра |",
        f"| **c_device** | **{s['c_device_pct_one_core']:.2f} % ядра** ({s['c_device_cores']:.3f} ядра) |",
        f"| N_per_core @70/85/95 % | {s['n_per_core']['U70']} / {s['n_per_core']['U85']} / {s['n_per_core']['U95']} |",
        f"| FFmpeg-дети (доля контура) | {s['p_ffmpeg_pct_one_core']:.2f} % ({s['ffmpeg_share'] * 100:.1f} %), "
        f"стабильных срезов {s['ffmpeg_stable_samples']}/{s['active_samples']} |",
        f"| FFmpeg-процессов (max) | {s['ffmpeg_count_max']} (ожидалось ≤ {s['n_connected'] + 1 + s.get('extra_decoders', 0)}); PID {s['pids_max']}, "
        f"тредов {s['threads_max']}, RSS {s['rss_kb_max'] // 1024} МиБ |",
        f"| **orphan FFmpeg (утечка фермы)** | **{s['orphan_ffmpeg_max']}** |",
        f"| cpu.pressure some (max) | {s['pressure_some_max']:.2f} % |",
        f"| Ферма энкодеров (max за окно) | групп {s['farm']['max'].get('groups', 0)}, "
        f"зрителей {s['farm']['max'].get('viewers', 0)}, кадров {s['farm']['max'].get('frames', 0)}, "
        f"keyframe-req {s['farm']['max'].get('keyframe_requests', 0)}, "
        f"dropped {s['farm']['max'].get('dropped_frames', 0)}, рестартов {s['farm']['max'].get('restarts', 0)}, "
        f"RTP отправлено {s['farm']['max'].get('viewer_rtp_packets', 0)} пакетов |",
        f"| RX аудио | {s['rx_audio']['pps_mean']:.1f} pps, потери {s['rx_audio']['loss_pct_mean']:.3f} % "
        f"(max {s['rx_audio']['loss_pct_max']:.3f}), jitter {s['rx_audio']['jitter_ms_mean']:.2f} мс, "
        f"MOS {s['rx_audio']['mos_mean']} |",
        f"| RX видео | {s['rx_video']['fps_mean']:.1f} пакетов/с, потери {s['rx_video']['loss_pct_mean']:.3f} % "
        f"(max {s['rx_video']['loss_pct_max']:.3f}), jitter {s['rx_video']['jitter_ms_mean']:.2f} мс |",
        f"| kernel-drops UDP (наши сокеты) | {s['rx_kernel_drops_total']} |",
        f"| CPU ботов (сумма user+sys, среднее) | {s['bot_self_cpu_s_mean']:.3f} с |",
        "",
        "## Срезы контура (1/с)",
        "",
        "| t, с | контур, % | ffmpeg, % | PID | тредов | RSS, МиБ | pressure, % |",
        "|---|---|---|---|---|---|---|",
    ]
    for d in series:
        lines.append(f"| {d['dt']:.1f} | {d['usage_pct']:.1f} | {d['ffmpeg_pct']:.1f} | {d['pids']} | "
                     f"{d['threads']} | {d['rss_kb'] // 1024} | {d['pressure_some']:.1f} |")
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
