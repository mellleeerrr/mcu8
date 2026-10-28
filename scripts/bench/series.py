#!/usr/bin/env python3
"""series.py — серия прогонов `matrix.py` (1/2/5/10/20) + α/γ и сводная таблица.

Зачем: `c_device` по одному N ничего не говорит о нелинейности. Серия даёт
α(N) = c_device(N) / c_device(1) — насколько цена устройства растёт с N
(конкуренция за ядра, softirq, планировщик), и γ = 1 + log2 α(20/10) — поправку
к формуле ёмкости (см. 06_perf_lab.md §4).

Каждый прогон — отдельный процесс `matrix.py` (свой каталог, свой паспорт
sha256 до/после); серия сама проверяет, что все прогоны `valid=true` и что
бинарь не менялся между прогонами — иначе вся серия помечается `attributable=false`.

Пример:
  scripts/bench/series.py --ns 1,2,5,10,20 --video-profile 720p30 --duration 120 \\
      --room <UUID> --outdir /tmp/vks-bench/series-$(date +%Y%m%d-%H%M%S)
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def log(msg: str) -> None:
    print(f"[series] {msg}", flush=True)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="Серия прогонов matrix.py + α/γ")
    ap.add_argument("--ns", default="1,2,5,10,20", help="список N через запятую")
    ap.add_argument("--room", required=True)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--sip-port", type=int, default=5060)
    ap.add_argument("--local-ip", default="192.168.1.93")
    ap.add_argument("--audio-codec", default="PCMU")
    ap.add_argument("--video-profile", default="720p30")
    ap.add_argument("--duration", type=float, default=120.0)
    ap.add_argument("--idle-baseline", type=float, default=5.0)
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--repeat", type=int, default=1, help="итераций на каждое N")
    ap.add_argument("--connect-window", type=float, default=15.0)
    ap.add_argument("--mcu-pid", type=int, default=0, help="только для стендов/тестов")
    ap.add_argument("--bot-cores", default="16-35", help="taskset-диапазон ботов ('' = не пиннить)")
    ap.add_argument("--max-load", type=float, default=8.0)
    ap.add_argument("--expect-sha256", default="",
                    help="sha256 замороженного бинаря (по умолчанию из PASSPORT.md, но там несколько итераций)")
    ap.add_argument("--allow-mismatch", action="store_true")
    ap.add_argument("--keep-going", action="store_true", help="не останавливаться на invalid прогоне")
    args = ap.parse_args(argv)

    ns = [int(x) for x in args.ns.split(",") if x.strip()]
    os.makedirs(args.outdir, exist_ok=True)
    runs: list[dict] = []
    failures: list[str] = []

    for n in ns:
        for rep in range(args.repeat):
            run_dir = os.path.join(args.outdir, f"N{n:02d}-r{rep + 1}")
            cmd = [
                sys.executable, os.path.join(HERE, "matrix.py"),
                "--bots", str(n), "--room", args.room, "--host", args.host,
                "--sip-port", str(args.sip_port), "--local-ip", args.local_ip,
                "--audio-codec", args.audio_codec, "--video-profile", args.video_profile,
                "--duration", str(args.duration), "--idle-baseline", str(args.idle_baseline),
                "--connect-window", str(args.connect_window), "--outdir", run_dir,
            ]
            if args.mcu_pid:
                cmd += ["--mcu-pid", str(args.mcu_pid)]
            if args.bot_cores:
                cmd += ["--bot-cores", args.bot_cores]
            cmd += ["--max-load", str(args.max_load)]
            if args.expect_sha256:
                cmd += ["--expect-sha256", args.expect_sha256]
            if args.allow_mismatch:
                cmd.append("--allow-mismatch")
            log(f"прогон N={n} итерация {rep + 1}/{args.repeat}: {run_dir}")
            started = time.time()
            proc = subprocess.run(cmd, capture_output=True, text=True)
            elapsed = time.time() - started
            summary = {}
            try:
                with open(os.path.join(run_dir, "results.json")) as fh:
                    summary = json.load(fh)["summary"]
            except (OSError, json.JSONDecodeError):
                failures.append(f"N{n}-r{rep + 1}: нет results.json (exit={proc.returncode})")
                if not args.keep_going:
                    log(f"ОСТАНОВ: прогон N={n} не дал results.json; stderr: {proc.stderr[-400:]}")
                    return 2
                continue
            summary["elapsed_s"] = round(elapsed, 1)
            summary["repeat"] = rep + 1
            runs.append(summary)
            log(f"  → valid={summary.get('valid')} c_device={summary.get('c_device_pct_one_core')} % "
                f"контур={summary.get('p_active_pct_one_core')} % ffmpeg_share={summary.get('ffmpeg_share')}")
            if not summary.get("valid") and not args.allow_mismatch:
                failures.append(f"N{n}-r{rep + 1}: invalid={summary.get('invalid_reason')}")
                if not args.keep_going:
                    log("ОСТАНОВ: прогон невалиден (паспорт артефакта). См. --allow-mismatch")
                    return 2

    # --- сводка: c_device по N, α и γ ---
    by_n: dict[int, list[float]] = {}
    for run in runs:
        if run.get("c_device_pct_one_core"):
            by_n.setdefault(run["n_bots_requested"], []).append(run["c_device_pct_one_core"])
    medians = {n: statistics.median(values) for n, values in sorted(by_n.items())}
    base = medians.get(1) or (min(medians.values()) if medians else 0.0)
    alpha = {f"N{n}": round(value / base, 3) for n, value in medians.items()} if base else {}
    alpha_20_10 = None
    if 10 in medians and 20 in medians and medians[10] and medians[20]:
        alpha_20_10 = round(medians[20] / medians[10], 3)
    gamma = round(1.0 + (alpha_20_10 - 1.0) / 2.0, 4) if alpha_20_10 else None
    # γ = 1 + log2(α) по определению §4.1 отчёта
    import math
    gamma_log = round(1.0 + math.log2(alpha_20_10), 4) if alpha_20_10 else None
    sha_set = {run.get("mcu_exe_sha256_before") for run in runs}
    attributable = (len(sha_set) == 1 and all(run.get("valid") for run in runs) and not failures)

    series = {
        "params": vars(args),
        "runs": runs,
        "failures": failures,
        "c_device_pct_by_n": {f"N{n}": round(v, 3) for n, v in medians.items()},
        "alpha_rel_N1": alpha,
        "alpha_20_10": alpha_20_10,
        "gamma_ratio": gamma,
        "gamma_log2": gamma_log,
        "mcu_exe_sha256": sha_set.pop() if len(sha_set) == 1 else sorted(sha_set),
        "attributable": attributable,
        "finished_at": time.time(),
    }
    with open(os.path.join(args.outdir, "series.json"), "w") as fh:
        json.dump(series, fh, indent=2, ensure_ascii=False)
    with open(os.path.join(args.outdir, "series.md"), "w") as fh:
        fh.write(_markdown(series))
    log(f"SERIES attributable={attributable} c_device={series['c_device_pct_by_n']} "
        f"α(20/10)={alpha_20_10} γ(log2)={gamma_log}")
    log(f"series.json -> {os.path.join(args.outdir, 'series.json')}")
    return 0 if attributable else 1


def _markdown(s: dict) -> str:
    p = s["params"]
    lines = [
        f"# Серия {p['video_profile']} / {p['audio_codec']}, по {p['duration']:.0f} с на точку",
        "",
        f"- атрибутируема: **{'да' if s['attributable'] else 'НЕТ'}**",
        f"- паспорт: `{str(s['mcu_exe_sha256'])[:16]}…`",
        f"- итераций на N: {p['repeat']}",
        "",
        "| N | c_device, % ядра | ядер/устройство | N/ядро @70/85/95 | контур, % | ffmpeg_share | RX loss аудио, % | RX loss видео, % | valid |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for run in s["runs"]:
        n = run["n_bots_requested"]
        npc = run["n_per_core"]
        lines.append(
            f"| {n} | {run['c_device_pct_one_core']:.2f} | {run['c_device_cores']:.3f} | "
            f"{npc['U70']}/{npc['U85']}/{npc['U95']} | {run['p_active_pct_one_core']:.1f} | "
            f"{run['ffmpeg_share']:.3f} | {run['rx_audio']['loss_pct_mean']:.3f} | "
            f"{run['rx_video']['loss_pct_mean']:.3f} | {'да' if run['valid'] else run['invalid_reason']} |"
        )
    lines += ["", f"**α(20/10)** = {s['alpha_20_10']}, **γ = 1 + log2 α** = {s['gamma_log2']}",
              "", f"α относительно N=1: {s['alpha_rel_N1']}"]
    if s["failures"]:
        lines += ["", "## Провалы", ""] + [f"- {f}" for f in s["failures"]]
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
