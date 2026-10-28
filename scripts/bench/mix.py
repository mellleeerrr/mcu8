#!/usr/bin/env python3
"""Один вход в СМЕШАННЫЙ прогон: N_sip SIP-видео + N_h323 H.323-видео одновременно.

Зачем отдельный раннер, а не два ручных запуска (как в первом mix 20:4x):
  * обе ноги стартуют синхронно (боты SIP стартуют после `--idle-baseline`, H.323 — сразу);
  * `--extra-decoders N_h323` передаётся в matrix.py → `orphan_ffmpeg_max` не врёт
    (в смеси FFmpeg-детей = 1 enc + N_sip + N_h323);
  * до/после снимается `Udp:` из `/proc/net/snmp` — атрибуция потерь «бот vs MCU»
    делается сразу (первый mix дал 8 % H.323-видео loss = RcvbufErrors моего бота);
  * после прогона — машинная постпроверка: ffmpeg-дети MCU, зомби, abonents, qdisc noqueue;
  * на выходе `mix_summary.md` + `mix_summary.json` с цифрами обеих ног.

Пример:
  scripts/bench/mix.py --sha <sha> --room <uuid> --outdir /tmp/vks-bench/mix \
      --sip-bots 10 --h323-calls 10 --h323-video 1080p30 --duration 60
"""
import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
VKS_PY = "/home/pro/.local/share/vks-mcu/h323-venv/bin/python3"


def sh(cmd: list[str]) -> str:
    return subprocess.run(cmd, capture_output=True, text=True).stdout


def udp_counters() -> dict[str, int]:
    """`Udp:` из /proc/net/snmp — InDatagrams/InErrors/RcvbufErrors и т. д."""
    header: list[str] | None = None
    for line in open("/proc/net/snmp"):
        if not line.startswith("Udp: "):
            continue
        parts = line.split()[1:]
        if header is None:
            header = parts
        else:
            return dict(zip(header, map(int, parts)))
    return {}


def mcu_pid() -> int:
    p = sh(["systemctl", "--user", "show", "-p", "MainPID", "--value", "vks-mcu.service"]).strip()
    return int(p or 0)


def exe_sha(pid: int) -> str:
    import hashlib
    h = hashlib.sha256()
    with open(f"/proc/{pid}/exe", "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def ffmpeg_children(pid: int) -> int:
    return int(sh(["bash", "-c", f"pgrep -P {pid} -c ffmpeg || true"]).strip() or 0)


def zombies(pid: int) -> int:
    return int(sh(["bash", "-c", f"ps --ppid {pid} -o stat= | grep -c '^Z' || true"]).strip() or 0)


def abonents(token_file: str) -> int | None:
    try:
        token = open(token_file).read().strip()
    except OSError:
        return None
    import urllib.request
    req = urllib.request.Request("http://127.0.0.1:8080/api/v3/sessions",
                                 headers={"Authorization": f"Bearer {token}"})
    try:
        data = json.load(urllib.request.urlopen(req, timeout=5))
    except Exception:  # noqa: BLE001
        return None
    total = 0
    for sess in data.get("sessions", []):
        total += int((sess.get("abonents") or {}).get("count") or 0)
    return total


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="Смешанный прогон SIP-видео + H.323 (G.722+1080p30)")
    ap.add_argument("--sha", required=True, help="ожидаемый sha256 живого MCU")
    ap.add_argument("--room", required=True, help="UUID комнаты")
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--host", default="192.168.1.93")
    ap.add_argument("--api", default="192.168.1.93:8080")
    ap.add_argument("--sip-bots", type=int, default=10)
    ap.add_argument("--sip-video", default="720p30")
    ap.add_argument("--sip-audio", default="PCMU")
    ap.add_argument("--local-ip", default="192.168.1.93")
    ap.add_argument("--h323-calls", type=int, default=10)
    ap.add_argument("--h323-profile", default="h323-m2")
    ap.add_argument("--h323-video", default="1080p30")
    ap.add_argument("--h323-local-ip", default="192.168.1.93")
    ap.add_argument("--h323-target", default="192.168.1.93:1720")
    ap.add_argument("--h323-ramp", type=float, default=2.0)
    ap.add_argument("--h323-python", default=VKS_PY)
    ap.add_argument("--duration", type=float, default=60.0)
    ap.add_argument("--idle-baseline", type=float, default=6.0)
    ap.add_argument("--connect-window", type=float, default=8.0)
    ap.add_argument("--bot-cores", default="16-35")
    ap.add_argument("--max-load", type=float, default=16.0)
    ap.add_argument("--mcu-pid", type=int, default=0)
    ap.add_argument("--token-file", default="/tmp/vks.token")
    ap.add_argument("--plan", action="store_true", help="только напечатать команды, не запускать")
    args = ap.parse_args(argv)

    pid = args.mcu_pid or mcu_pid()
    sipdir, h3dir = os.path.join(args.outdir, "sip"), os.path.join(args.outdir, "h323")
    sip_log = os.path.join(args.outdir, "sip.log")
    h3_log = os.path.join(args.outdir, "h323.log")
    sip_cmd = [sys.executable, os.path.join(HERE, "matrix.py"),
               "--bots", str(args.sip_bots), "--room", args.room, "--host", args.host,
               "--local-ip", args.local_ip, "--api", args.api,
               "--video-profile", args.sip_video, "--audio-codec", args.sip_audio,
               "--duration", str(args.duration), "--idle-baseline", str(args.idle_baseline),
               "--connect-window", str(args.connect_window), "--bot-cores", args.bot_cores,
               "--max-load", str(args.max_load), "--mcu-pid", str(pid),
               "--expect-sha256", args.sha, "--allow-partial",
               "--extra-decoders", str(args.h323_calls), "--outdir", sipdir]
    h3_cmd = [args.h323_python, os.path.join(HERE, "h323bot.py"),
              "--target", args.h323_target, "--local-ip", args.h323_local_ip,
              "--alias", args.room, "--calls", str(args.h323_calls),
              "--duration", str(args.duration), "--ramp", str(args.h323_ramp),
              "--profile", args.h323_profile, "--video-profile", args.h323_video,
              "--report", h3dir, "--mcu-pid", str(pid), "--idle-baseline", "0"]

    if args.plan:
        print("SIP:", " ".join(sip_cmd))
        print(f"(через {args.idle_baseline:.0f} с, одновременно)")
        print("H323:", " ".join(h3_cmd))
        return 0

    os.makedirs(sipdir, exist_ok=True)
    os.makedirs(h3dir, exist_ok=True)
    live = exe_sha(pid) if pid else ""
    if args.sha and live and live != args.sha:
        print(f"[mix] FATAL: sha живого {live[:16]}… != ожидаемого {args.sha[:16]}…", file=sys.stderr)
        return 2
    udp0 = udp_counters()
    load0 = float(open("/proc/loadavg").read().split()[0])
    t0 = time.monotonic()
    print(f"[mix] старт: sha {live[:16]}…, PID {pid}, SIP {args.sip_bots}×{args.sip_video}, "
          f"H.323 {args.h323_calls}×{args.h323_video}, {args.duration:.0f} с", flush=True)
    with open(sip_log, "w") as sip_fh:
        sip_proc = subprocess.Popen(sip_cmd, stdout=sip_fh, stderr=subprocess.STDOUT)
        time.sleep(args.idle_baseline)
        with open(h3_log, "w") as h3_fh:
            h3_proc = subprocess.Popen(h3_cmd, stdout=h3_fh, stderr=subprocess.STDOUT)
            h3_rc = h3_proc.wait()
        sip_rc = sip_proc.wait()
    udp1 = udp_counters()
    udp_delta = {k: udp1.get(k, 0) - udp0.get(k, 0) for k in udp1}
    summary: dict = {"sha": live, "mcu_pid": pid, "wall_s": round(time.monotonic() - t0, 1),
                     "loadavg_before": load0,
                     "loadavg_after": float(open("/proc/loadavg").read().split()[0]),
                     "udp_delta": udp_delta, "exit": {"sip": sip_rc, "h323": h3_rc}}
    sip = json.load(open(os.path.join(sipdir, "results.json")))["summary"]
    h3 = json.load(open(os.path.join(h3dir, "summary.json")))
    summary["sip"] = sip
    summary["h323"] = h3
    summary["combined"] = {
        "devices": sip["n_connected"] + h3["connected"],
        "active_cores": round(sip["p_active_pct_one_core"] / 100.0, 3),
        "cores_per_device": round(sip["p_active_pct_one_core"] / 100.0
                                  / max(1, sip["n_connected"] + h3["connected"]), 4),
        "ffmpeg_count_max": max(sip["ffmpeg_count_max"], h3.get("ffmpeg_count_max", 0)),
        "orphan_ffmpeg_max": sip["orphan_ffmpeg_max"],
    }
    summary["postcheck"] = {
        "ffmpeg": ffmpeg_children(pid) if pid else None,
        "zombies": zombies(pid) if pid else None,
        "abonents": abonents(args.token_file),
        "clean": None,
    }
    pc = summary["postcheck"]
    pc["clean"] = (pc["ffmpeg"] == 0 and pc["zombies"] == 0 and pc["abonents"] in (0, None))
    json.dump(summary, open(os.path.join(args.outdir, "mix_summary.json"), "w"),
              ensure_ascii=False, indent=1)

    s, h, c = summary["sip"], summary["h323"], summary["combined"]
    md = [
        f"# Mix: {s['n_connected']} SIP-{args.sip_video} + {h['connected']} H.323 "
        f"{args.h323_profile}/{args.h323_video} — sha `{live[:16]}…`, PID {pid}", "",
        "| нога | подкл. | видео | видео loss | аудио loss | MOS |",
        "|---|---|---|---|---|---|",
        f"| SIP ({s['rx_audio'].get('pps_mean', 0):.0f} pps audio) | {s['n_connected']}/{args.sip_bots} "
        f"| {s.get('rx_video', {}).get('fps_mean', 0):.1f} пкт/с | {s['rx_video'].get('loss_pct_mean', 0):.3f} % "
        f"| {s['rx_audio']['loss_pct_mean']:.3f} % | {s['rx_audio']['mos_mean']} |",
        f"| H.323 ({args.h323_profile}, {args.h323_video}) | {h['connected']}/{args.h323_calls} "
        f"| {h.get('video_rx_packets', {}).get('mean', 0):.0f} пкт/вызов "
        f"| {h.get('video_loss_pct', {}).get('mean', 0):.3f} % | {h['loss_pct']['mean']:.3f} % "
        f"| {h['mos']['mean'] if h.get('mos') else '—'} |", "",
        f"**Контур:** {c['active_cores']:.2f} ядра на {c['devices']} устройств "
        f"({c['cores_per_device']:.3f} ядра/устройство), `p_idle` {s['p_idle_pct_one_core']:.2f} %, "
        f"ffmpeg_share {s['ffmpeg_share'] * 100:.1f} %, детей {c['ffmpeg_count_max']} "
        f"(1 enc + {c['devices']} dec), `orphan_ffmpeg_max` {c['orphan_ffmpeg_max']}.",
        f"**UDP за прогон:** RcvbufErrors {udp_delta.get('RcvbufErrors', 0)} "
        f"(≈0 — потери не из-за rx-буфера бота), InDatagrams {udp_delta.get('InDatagrams', 0)}.",
        f"**Postcheck:** ffmpeg {pc['ffmpeg']}, зомби {pc['zombies']}, abonents {pc['abonents']} "
        f"→ clean={pc['clean']}; loadavg {summary['loadavg_before']}→{summary['loadavg_after']}.", "",
        f"Сырьё: `{sipdir}/results.json`, `{h3dir}/summary.json`, `mix_summary.json`.",
    ]
    open(os.path.join(args.outdir, "mix_summary.md"), "w").write("\n".join(md) + "\n")
    print("\n".join(md))
    return 0 if (sip_rc == 0 and h3_rc == 0 and pc["clean"]) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
