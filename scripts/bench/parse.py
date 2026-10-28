#!/usr/bin/env python3
"""parse.py — агрегация прогонов в одну Markdown-таблицу + summary.json.

Читает каталоги прогонов, созданные run_matrix.sh:
    /tmp/vks-bench/runs/<scenario>_n<N>_<profile>_<topo>/
        meta.json
        idle/{samples.json,meta.json,...}
        active/{samples.json,meta.json,pidstat_mcu.log,mpstat_mcu.log,...}
        bot/*.jsonl          (отчёты ботов: loss/jitter/fps/MOS)
        run.log

Использование:
    parse.py /tmp/vks-bench/runs                 # Markdown в stdout
    parse.py /tmp/vks-bench/runs --json out.json # + машиночитаемый агрегат
    parse.py /tmp/vks-bench/runs --runs-filter m3
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import statistics
import sys


def read_json(path: str, default=None):
    try:
        with open(path) as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return default


def read_jsonl(path: str) -> list[dict]:
    out = []
    try:
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except ValueError:
                    continue
    except OSError:
        pass
    return out


def window_p(path: str) -> dict:
    samples = read_json(os.path.join(path, "samples.json"), []) or []
    if len(samples) < 2:
        return {}
    first, last = samples[0], samples[-1]
    dt = last["t"] - first["t"]
    if dt <= 0:
        return {}
    if last.get("mode") == "cgroup" and last.get("usage_usec"):
        usage = last["usage_usec"] - first.get("usage_usec", 0)
        p = usage / (dt * 1e6) * 100.0
        ff = last.get("ffmpeg_ticks", 0) - first.get("ffmpeg_ticks", 0)
        ff_p = ff / (dt * os.sysconf("SC_CLK_TCK")) * 100.0
    else:
        ticks = last.get("pidtree_ticks", 0) - first.get("pidtree_ticks", 0)
        p = ticks / (dt * os.sysconf("SC_CLK_TCK")) * 100.0
        ff = last.get("ffmpeg_ticks", 0) - first.get("ffmpeg_ticks", 0)
        ff_p = ff / (dt * os.sysconf("SC_CLK_TCK")) * 100.0
    return {
        "p": p,
        "ffmpeg_p": ff_p,
        "ffmpeg_count_max": max((s.get("ffmpeg_count", 0) for s in samples), default=0),
        "pressure": statistics.fmean([s.get("pressure_cpu_some_avg10", 0.0) for s in samples]),
        "pids_mean": statistics.fmean([s.get("pids_total", 0) for s in samples]),
        "source": "cgroup" if last.get("mode") == "cgroup" else "pidtree",
        "mcu_allowed": last.get("mcu_allowed", "?"),
    }


def bot_stats(run_dir: str) -> dict:
    calls = []
    for path in glob.glob(os.path.join(run_dir, "bot", "*.jsonl")):
        calls.extend(read_jsonl(path))
    if not calls:
        return {}
    def agg(key, default=0.0):
        vals = [c.get(key) for c in calls if isinstance(c.get(key), (int, float))]
        return statistics.fmean(vals) if vals else default
    connected = sum(1 for c in calls if c.get("disposition") == "connected")
    return {
        "calls": len(calls),
        "connected": connected,
        "connect_ratio": connected / len(calls),
        "loss_pct": agg("loss_pct"),
        "jitter_ms": agg("jitter_ms"),
        "rtt_ms": agg("rtt_ms"),
        "mos": agg("mos"),
        "fps": agg("fps"),
        "rx_packets": agg("rx_packets"),
        "tx_lag_p99_us": agg("send_lag_p99_us"),
    }


def summarize(run_dir: str) -> dict | None:
    meta = read_json(os.path.join(run_dir, "meta.json"))
    if not meta:
        return None
    idle = window_p(os.path.join(run_dir, "idle"))
    active = window_p(os.path.join(run_dir, "active"))
    if not idle or not active:
        return None
    n = int(meta.get("n", 0) or 1)
    c_device = (active["p"] - idle["p"]) / n
    bots = bot_stats(run_dir)
    return {
        "scenario": meta.get("scenario", "?"),
        "n": n,
        "profile": meta.get("profile", "?"),
        "topology": meta.get("topology", "?"),
        "duration_s": meta.get("duration_s"),
        "p_idle": round(idle["p"], 4),
        "p_active": round(active["p"], 4),
        "c_device_pct_one_core": round(c_device, 4),
        "c_device_cores": round(c_device / 100.0, 6),
        "ffmpeg_p": round(active["ffmpeg_p"], 4),
        "ffmpeg_share": round(active["ffmpeg_p"] / active["p"], 4) if active["p"] else 0.0,
        "ffmpeg_count_max": active["ffmpeg_count_max"],
        "cpu_pressure_avg10": round(active["pressure"], 3),
        "accounting": active["source"],
        "mcu_allowed": active["mcu_allowed"],
        "n_per_core": {f"U{u}": (int(u / c_device) if c_device > 0 else None) for u in (70, 85, 95)},
        "bot": bots,
        "noisy": meta.get("noisy", False),
        "isolation": meta.get("isolation_env", "none"),
        "netem": meta.get("netem_env", "none"),
        "run_dir": run_dir,
    }


def to_markdown(rows: list[dict]) -> str:
    head = (
        "| scenario | profile | T | N | `c_device` % ядра | ядер/устр. | ffmpeg %MCU | ffmpeg шт. | N/ядро@70 | @85 | @95 "
        "| connect | loss % | jitter мс | MOS | класс | accounting |"
    )
    sep = "|" + "---|" * 17
    lines = [head, sep]
    for r in rows:
        b = r["bot"] or {}
        n70, n85, n95 = (r["n_per_core"][k] for k in ("U70", "U85", "U95"))
        cls = "—"
        if b.get("mos"):
            mos, ratio, loss = b["mos"], (b.get("fps") or 0) / max(1.0, float(b.get("fps") or 0)), b.get("loss_pct", 0)
            del ratio
            if mos >= 4.30 and loss <= 0.1:
                cls = "A"
            elif mos >= 4.00 and loss <= 0.5:
                cls = "B"
            elif mos >= 3.60 and loss <= 1.5:
                cls = "C"
            else:
                cls = "FAIL"
        lines.append(
            f"| {r['scenario']} | {r['profile']} | {r['topology']} | {r['n']} | {r['c_device_pct_one_core']} "
            f"| {r['c_device_cores']} | {r['ffmpeg_share'] * 100:.1f} | {r['ffmpeg_count_max']} "
            f"| {n70} | {n85} | {n95} "
            f"| {b.get('connected', 0)}/{b.get('calls', 0)} | {b.get('loss_pct', 0):.3f} "
            f"| {b.get('jitter_ms', 0):.2f} | {b.get('mos', 0):.3f} | {cls} | {r['accounting']} |"
        )
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="прогоны -> Markdown + JSON")
    ap.add_argument("runs_dir")
    ap.add_argument("--json")
    ap.add_argument("--runs-filter", default="", help="подстрока в имени каталога прогона")
    ap.add_argument("--include-noisy", action="store_true")
    args = ap.parse_args(argv)

    dirs = sorted(d for d in glob.glob(os.path.join(args.runs_dir, "*")) if os.path.isdir(d))
    if args.runs_filter:
        dirs = [d for d in dirs if args.runs_filter in os.path.basename(d)]
    rows = [r for r in (summarize(d) for d in dirs) if r]
    if not args.include_noisy:
        clean = [r for r in rows if not r["noisy"]]
        if len(clean) != len(rows):
            print(f"[parse] отброшено шумных прогонов: {len(rows) - len(clean)} "
                  f"(--include-noisy чтобы показать)", file=sys.stderr)
        rows = clean
    if not rows:
        print("[parse] нечего агрегировать", file=sys.stderr)
        return 2
    print(to_markdown(rows))
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(rows, fh, indent=2, ensure_ascii=False)
        print(f"[parse] JSON -> {args.json}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
