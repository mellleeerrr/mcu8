#!/usr/bin/env python3
"""formula.py — расчёт ёмкости одного физического ядра по методике из
docs/audit/2026-10-25/06_perf_lab.md §4.

Ключевые определения (см. §4.1):
  P — «процент одного физического ядра» (100 % = 1 ядро загружено целиком).
      В режиме cgroup: P = Δusage_usec / (Δt_s · 1e6) · 100
      В режиме pidtree: P = Δticks / (Δt_s · CLK_TCK) · 100
  c_device = (P_active − P_idle) / N                 [% одного ядра]
  N_per_core = floor(U_limit / c_device)             для U_limit ∈ {70, 85, 95} %
  α = c_device(20) / c_device(10); γ = 1 + log2(α)   (степенной закон)

C_total в формуле НЕ участвует: все величины уже в «процентах одного ядра».
Ошибка черновика (C_total в знаменателе) даёт множитель C_total — см. §4.1.

Использование:
  formula.py --run <dir>                      # один прогон: <dir>/idle, <dir>/active
  formula.py --runs <dir>                     # все прогоны в каталоге
  formula.py --run <dir> --limits 70,85,95 --json out.json --markdown
"""
from __future__ import annotations

import argparse
import glob
import json
import math
import os
import sys

CLK_TCK = os.sysconf("SC_CLK_TCK")
LIMITS_DEFAULT = (70.0, 85.0, 95.0)
CLASS_BOUNDS = {
    # класс -> (mos_min, fps_ratio_min, loss_pct_max, owd_ms_max, jitter_ms_max, U_limit)
    "A": (4.30, 1.00, 0.1, 100.0, 15.0, 70.0),
    "B": (4.00, 0.90, 0.5, 150.0, 30.0, 85.0),
    "C": (3.60, 0.60, 1.5, 250.0, 45.0, 95.0),
}


# ---------------------------------------------------------------- контур MCU ---
def p_from_samples(samples: list[dict]) -> dict:
    """P контура MCU по окну сэмплов (cgroup — приоритет, pidtree — fallback)."""
    if len(samples) < 2:
        raise ValueError("нужно >=2 сэмплов")
    first, last = samples[0], samples[-1]
    dt = last["t"] - first["t"]
    if dt <= 0:
        raise ValueError("нулевое окно")
    out: dict[str, float] = {"window_s": dt, "mode": last.get("mode", "?")}

    usage = last.get("usage_usec", 0) - first.get("usage_usec", 0)
    if last.get("mode") == "cgroup" and usage > 0:
        out["p_one_core"] = usage / (dt * 1e6) * 100.0
        out["source"] = "cgroup:usage_usec"
        user = last.get("user_usec", 0) - first.get("user_usec", 0)
        sys_ = last.get("system_usec", 0) - first.get("system_usec", 0)
        out["p_user"] = user / (dt * 1e6) * 100.0
        out["p_system"] = sys_ / (dt * 1e6) * 100.0
    else:
        ticks = last.get("pidtree_ticks", 0) - first.get("pidtree_ticks", 0)
        out["p_one_core"] = ticks / (dt * CLK_TCK) * 100.0
        out["source"] = "pidtree:utime+stime"

    # FFmpeg-дети: доля контура и число процессов, отдельно энкодеры и декодеры
    ff = last.get("ffmpeg_ticks", 0) - first.get("ffmpeg_ticks", 0)
    enc = last.get("encoder_ticks", 0) - first.get("encoder_ticks", 0)
    dec = last.get("decoder_ticks", 0) - first.get("decoder_ticks", 0)
    out["ffmpeg_p_one_core"] = ff / (dt * CLK_TCK) * 100.0
    out["encoder_p_one_core"] = enc / (dt * CLK_TCK) * 100.0
    out["decoder_p_one_core"] = dec / (dt * CLK_TCK) * 100.0
    if last.get("mode") == "cgroup" and usage > 0:
        denom = usage / 1e6
    else:
        denom = (last.get("pidtree_ticks", 0) - first.get("pidtree_ticks", 0)) / CLK_TCK
    out["ffmpeg_share"] = (ff / CLK_TCK) / denom if denom else 0.0
    out["encoder_share"] = (enc / CLK_TCK) / denom if denom else 0.0
    out["decoder_share"] = (dec / CLK_TCK) / denom if denom else 0.0
    out["ffmpeg_count_max"] = max((s.get("ffmpeg_count", 0) for s in samples), default=0)
    out["ffmpeg_count_mean"] = sum(s.get("ffmpeg_count", 0) for s in samples) / len(samples)
    out["encoder_count_max"] = max((s.get("ffmpeg_encoder_count", 0) for s in samples), default=0)
    out["decoder_count_max"] = max((s.get("ffmpeg_decoder_count", 0) for s in samples), default=0)
    out["pids_total_mean"] = sum(s.get("pids_total", 0) for s in samples) / len(samples)
    presses = [s.get("pressure_cpu_some_avg10", 0.0) for s in samples]
    out["cpu_pressure_avg10_mean"] = sum(presses) / len(presses)
    out["cpu_pressure_avg10_max"] = max(presses) if presses else 0.0
    out["mcu_allowed"] = last.get("mcu_allowed", "?")
    out["ffmpeg_allowed"] = last.get("ffmpeg_allowed", "?")
    return out


def load_window(path: str) -> dict:
    with open(os.path.join(path, "samples.json")) as fh:
        return p_from_samples(json.load(fh))


# ------------------------------------------------------------------ метрики ---
def e_model_r(mos_inputs: dict) -> dict:
    """E-model (G.107). Значения I_e — дефолты G.113 App. I; точные — из отчёта бота."""
    r0 = 93.2
    is_ = 1.4
    a = 0.0
    owd = float(mos_inputs.get("owd_ms", 0.0))
    jb = float(mos_inputs.get("jitter_buffer_ms", 0.0))
    codec_delay = float(mos_inputs.get("codec_delay_ms", 20.0))
    id_ = 0.024 * (owd + jb + 2 * codec_delay)
    if owd + jb + 2 * codec_delay > 177.3:
        id_ += 0.11 * ((owd + jb + 2 * codec_delay) - 177.3)
    ie = float(mos_inputs.get("ie", 0.0))
    bpl = float(mos_inputs.get("bpl", 25.1))
    ppl = max(0.0, float(mos_inputs.get("loss_pct", 0.0)))
    ie_eff = ie + (95.0 - ie) * (ppl / (ppl + bpl))
    r = r0 - is_ - id_ - ie_eff + a
    r = max(0.0, min(100.0, r))
    mos = 1.0 + 0.035 * r + 7e-6 * r * (r - 60.0) * (100.0 - r)
    return {"R": round(r, 3), "MOS": round(max(1.0, min(4.5, mos)), 3), "Ie_eff": round(ie_eff, 3), "Id": round(id_, 3)}


def classify(p: dict, sla: dict | None) -> str:
    if not sla:
        return "?"
    fps_ratio = sla.get("fps_ratio", 0.0)
    loss = sla.get("loss_pct", 999.0)
    owd = sla.get("owd_ms", 9999.0)
    jit = sla.get("jitter_ms", 9999.0)
    mos = sla.get("mos", 0.0)
    for name in ("A", "B", "C"):
        m, fr, lo, ow, jt, _ = CLASS_BOUNDS[name]
        if mos >= m and fps_ratio >= fr and loss <= lo and owd <= ow and jit <= jt:
            return name
    return "FAIL"


# ----------------------------------------------------------------- верхний ---
def capacity(p_idle: dict, p_active: dict, n: int, limits: tuple[float, ...]) -> dict:
    c_device = (p_active["p_one_core"] - p_idle["p_one_core"]) / n
    out = {
        "n": n,
        "p_idle": round(p_idle["p_one_core"], 4),
        "p_active": round(p_active["p_one_core"], 4),
        "delta": round(p_active["p_one_core"] - p_idle["p_one_core"], 4),
        "c_device_pct_one_core": round(c_device, 4),
        "c_device_cores": round(c_device / 100.0, 6),
        "ffmpeg_share": round(p_active.get("ffmpeg_share", 0.0), 4),
        "encoder_share": round(p_active.get("encoder_share", 0.0), 4),
        "decoder_share": round(p_active.get("decoder_share", 0.0), 4),
        "ffmpeg_p_one_core": round(p_active.get("ffmpeg_p_one_core", 0.0), 4),
        "ffmpeg_count_max": p_active.get("ffmpeg_count_max", 0),
        "encoder_count_max": p_active.get("encoder_count_max", 0),
        "decoder_count_max": p_active.get("decoder_count_max", 0),
        "n_per_core": {},
        "n_per_machine_36": {},
    }
    for limit in limits:
        if c_device > 0:
            per_core = math.floor(limit / c_device)
            out["n_per_core"][f"U{int(limit)}"] = per_core
            out["n_per_machine_36"][f"U{int(limit)}"] = per_core * 36
        else:
            out["n_per_core"][f"U{int(limit)}"] = None
            out["n_per_machine_36"][f"U{int(limit)}"] = None
    return out


def nonlinearity(points: dict[int, float]) -> dict:
    """α = c(20)/c(10), γ = 1 + log2(α). points: N -> c_device."""
    out: dict[str, float] = {}
    if 10 in points and 20 in points and points[10] > 0:
        alpha = points[20] / points[10]
        out["alpha_20_10"] = round(alpha, 4)
        out["gamma"] = round(1.0 + math.log2(alpha), 4) if alpha > 0 else None
    if 5 in points and 20 in points and points[5] > 0:
        out["alpha_20_5"] = round(points[20] / points[5], 4)
    return out


def run_one(run_dir: str, limits: tuple[float, ...], sla: dict | None) -> dict:
    idle = load_window(os.path.join(run_dir, "idle"))
    active = load_window(os.path.join(run_dir, "active"))
    meta_path = os.path.join(run_dir, "meta.json")
    meta = json.load(open(meta_path)) if os.path.exists(meta_path) else {}
    n = int(meta.get("n", 0)) or int(os.path.basename(run_dir).split("_n")[-1].split("_")[0] or 0)
    cap = capacity(idle, active, max(1, n), limits)
    mos = e_model_r(sla) if sla else None
    out = {
        "run": os.path.basename(run_dir),
        "profile": meta.get("profile", "?"),
        "topology": meta.get("topology", "?"),
        "isolation": meta.get("isolation_env", "none"),
        "accounting": active.get("source"),
        "mcu_allowed": active.get("mcu_allowed"),
        "ffmpeg_allowed": active.get("ffmpeg_allowed"),
        "cpu_pressure_avg10_mean": round(active.get("cpu_pressure_avg10_mean", 0.0), 3),
        "capacity": cap,
        "mos": mos,
        "sla_class": classify(cap, dict(sla or {}, mos=(mos or {}).get("MOS", 0.0))) if sla else "?",
    }
    return out


def to_markdown(rows: list[dict]) -> str:
    lines = [
        "| run | profile | T | N | `c_device`, % ядра | ядер/устройство | ffmpeg share | N/ядро @70 | @85 | @95 | MOS | класс |",
        "|---|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for r in rows:
        c = r["capacity"]
        lines.append(
            "| {run} | {profile} | {topo} | {n} | {cdev} | {cores} | {ffshare} | {n70} | {n85} | {n95} | {mos} | {cls} |".format(
                run=r["run"],
                profile=r["profile"],
                topo=r["topology"],
                n=c["n"],
                cdev=c["c_device_pct_one_core"],
                cores=c["c_device_cores"],
                ffshare=f"{c['ffmpeg_share'] * 100:.1f}",
                n70=c["n_per_core"].get("U70"),
                n85=c["n_per_core"].get("U85"),
                n95=c["n_per_core"].get("U95"),
                mos=(r["mos"] or {}).get("MOS", "—"),
                cls=r["sla_class"],
            )
        )
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="c_device / N_per_core / SLA (06_perf_lab.md §4)")
    ap.add_argument("--run", help="каталог одного прогона (содержит idle/ и active/)")
    ap.add_argument("--runs", help="каталог со всеми прогонами")
    ap.add_argument("--limits", default="70,85,95", help="U_limit в процентах одного ядра (по умолчанию 70,85,95)")
    ap.add_argument("--sla", help="JSON с метриками ботов (loss_pct, jitter_ms, owd_ms, fps_ratio, ie, bpl)")
    ap.add_argument("--json", help="куда записать summary.json")
    ap.add_argument("--markdown", action="store_true", help="напечатать Markdown-таблицу")
    args = ap.parse_args(argv)

    limits = tuple(float(x) for x in args.limits.split(",") if x.strip())
    sla = json.load(open(args.sla)) if args.sla else None

    dirs: list[str] = []
    if args.run:
        dirs = [args.run]
    elif args.runs:
        dirs = sorted(d for d in glob.glob(os.path.join(args.runs, "*")) if os.path.isdir(os.path.join(d, "active")))
    else:
        ap.error("нужен --run или --runs")
    if not dirs:
        print("не найдено ни одного прогона с подкаталогом active/", file=sys.stderr)
        return 2

    rows = [run_one(d, limits, sla) for d in dirs]

    points = {r["capacity"]["n"]: r["capacity"]["c_device_pct_one_core"] for r in rows}
    nl = nonlinearity(points)
    summary = {"runs": rows, "nonlinearity": nl, "limits": list(limits)}
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(summary, fh, indent=2, ensure_ascii=False)
        print(f"[formula] summary -> {args.json}")
    if args.markdown:
        print(to_markdown(rows))
    if not args.json and not args.markdown:
        json.dump(summary, sys.stdout, indent=2, ensure_ascii=False)
        print()
    if nl:
        print(f"[formula] α(20/10)={nl.get('alpha_20_10')} γ={nl.get('gamma')} "
              f"(γ>1 — дорожает с N, γ<1 — общий композит амортизируется)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
