#!/usr/bin/env bash
# collect.sh — сбор метрик MCU-контура за окно измерения.
#
# Требование (Lead, поправка к методике): мерить НЕ `pidstat -p <pid>`, а
# контур MCU целиком: сам vks-server + все его дети (FFmpeg-энкодер/декодер на
# каждую видеосессию, python h323_listener) — потому что `pidstat -p $MCU_PID`
# их не видит и завышает ёмкость в разы.
#
# Два режима (выбираются автоматически, можно форсировать):
#   cgroup  — /sys/fs/cgroup/<unit>/cpu.stat: usage_usec/user_usec/system_usec
#             (монотонные счётчики, включают всех детей автоматически) +
#             cpu.pressure + pids.current
#   pidtree — если cgroup недоступен: сумма utime+stime по /proc/<pid>/stat
#             самого vks-server и всех его детей/внуков (обходим /proc).
#
# Использование:
#   scripts/bench/collect.sh <outdir> <duration_s> [--mode cgroup|pidtree|auto]
#                            [--mcu-pid PID] [--cgroup PATH] [--interval 1]
#                            [--perf 0|1]
set -euo pipefail
export LC_ALL=C

OUTDIR="${1:?usage: collect.sh <outdir> <duration_s>}"; shift
DURATION="${1:?usage: collect.sh <outdir> <duration_s>}"; shift

MODE="auto"
MCU_PID=""
CGROUP=""
INTERVAL=1
PERF=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) MODE="$2"; shift 2 ;;
    --mcu-pid) MCU_PID="$2"; shift 2 ;;
    --cgroup) CGROUP="$2"; shift 2 ;;
    --interval) INTERVAL="$2"; shift 2 ;;
    --perf) PERF="$2"; shift 2 ;;
    *) printf '[collect] unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
done

log() { printf '[collect] %s\n' "$*"; }
mkdir -p "$OUTDIR"

# 1. Найти PID и cgroup живого MCU, если не заданы.
if [[ -z "$MCU_PID" ]]; then
  MCU_PID="$(systemctl --user show -p MainPID --value vks-mcu.service 2>/dev/null || true)"
fi
[[ -n "$MCU_PID" && -d "/proc/$MCU_PID" ]] || { printf '[collect] FATAL: не найден PID vks-server\n' >&2; exit 1; }

CG_FROM_PROC="$(awk -F: '/^0::/{print $3}' "/proc/$MCU_PID/cgroup")"
CG_ROOT="/sys/fs/cgroup${CG_FROM_PROC}"
[[ -n "$CGROUP" ]] || CGROUP="$CG_ROOT"

if [[ "$MODE" == "auto" ]]; then
  if [[ -r "$CGROUP/cpu.stat" ]]; then MODE="cgroup"; else MODE="pidtree"; fi
fi

log "pid=$MCU_PID cgroup=$CGROUP mode=$MODE duration=${DURATION}s interval=${INTERVAL}s"
log "Cpus_allowed_list(pid)=$(awk '/Cpus_allowed_list/{print $2}' "/proc/$MCU_PID/status")"

# 2. Фон: pidstat по потокам, mpstat по ядрам MCU и по ядрам ботов, perf (опц.).
( pidstat -u -r -t -p "$MCU_PID" "$INTERVAL" "$DURATION" >"$OUTDIR/pidstat_mcu.log" 2>&1 || true ) &
PIDSTAT_PID=$!
# mpstat: ядра MCU 0-15 и ядра ботов 16-35 (контроль, что боты не протекли)
( mpstat -P 0-15 "$INTERVAL" "$DURATION" >"$OUTDIR/mpstat_mcu.log" 2>&1 || true ) &
MPSTAT1=$!
( mpstat -P 16-35 "$INTERVAL" "$DURATION" >"$OUTDIR/mpstat_bots.log" 2>&1 || true ) &
MPSTAT2=$!

if [[ "$PERF" == "1" ]]; then
  # perf_event_paranoid=1: для своих процессов `perf stat -p` разрешён.
  ( perf stat -p "$MCU_PID" -e task-clock,context-switches,cache-misses,branch-misses \
      -- sleep "$DURATION" >"$OUTDIR/perf_mcu.log" 2>&1 || true ) &
fi

# 3. Основной сэмплер: cpu.stat + cpu.pressure + дети (FFmpeg) + их CPU.
python3 - "$OUTDIR" "$MCU_PID" "$CGROUP" "$MODE" "$DURATION" "$INTERVAL" <<'PY'
import json, os, sys, time

outdir, mcu_pid, cgroup, mode, duration, interval = sys.argv[1:7]
mcu_pid = int(mcu_pid); duration = float(duration); interval = float(interval)
clock = os.sysconf("SC_CLK_TCK")
rows = []

def read_cpu_stat():
    out = {}
    try:
        with open(os.path.join(cgroup, "cpu.stat")) as fh:
            for line in fh:
                key, _, value = line.partition(" ")
                if value.strip():
                    out[key] = int(value)
    except OSError:
        pass
    return out

def read_pressure():
    out = {}
    try:
        with open(os.path.join(cgroup, "cpu.pressure")) as fh:
            for line in fh:
                parts = line.split()
                kind = parts[0].rstrip(":")
                out[kind] = {p.split("=")[0]: float(p.split("=")[1]) for p in parts[1:] if "=" in p}
    except OSError:
        pass
    return out

def read_int(path, default=0):
    try:
        with open(path) as fh:
            return int(fh.read().strip())
    except (OSError, ValueError):
        return default

def tree_pids(root):
    """root + все потомки (обход /proc/<pid>/task/<tid>/children не нужен: берём ppid-карту)."""
    children = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        try:
            with open(f"/proc/{pid}/stat") as fh:
                data = fh.read()
            ppid = int(data.rsplit(")", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            continue
        children.setdefault(ppid, []).append(pid)
    result, stack = [], [root]
    while stack:
        pid = stack.pop()
        result.append(pid)
        stack.extend(children.get(pid, []))
    return result

def proc_cpu_ticks(pid):
    try:
        with open(f"/proc/{pid}/stat") as fh:
            data = fh.read()
        fields = data.rsplit(")", 1)[1].split()
        return int(fields[11]) + int(fields[12])  # utime + stime
    except (OSError, IndexError, ValueError):
        return 0

def proc_comm(pid):
    try:
        with open(f"/proc/{pid}/comm") as fh:
            return fh.read().strip()
    except OSError:
        return "?"

def ffmpeg_role(pid):
    """'encoder' | 'decoder' по cmdline: у энкодера вход -f rawvideo, у декодера -f h264/hevc."""
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as fh:
            joined = b" ".join(fh.read().split(b"\0")).decode("utf-8", "replace")
    except OSError:
        return "unknown"
    return "encoder" if "rawvideo" in joined else "decoder"

def proc_allowed(pid):
    try:
        with open(f"/proc/{pid}/status") as fh:
            for line in fh:
                if line.startswith("Cpus_allowed_list:"):
                    return line.split()[1]
    except OSError:
        pass
    return "?"

start = time.monotonic()
prev = read_cpu_stat()
prev_tree = tree_pids(mcu_pid)
prev_pidtree = sum(proc_cpu_ticks(p) for p in prev_tree)
prev_ffmpeg = sum(proc_cpu_ticks(p) for p in prev_tree if proc_comm(p) == "ffmpeg")
prev_enc = sum(proc_cpu_ticks(p) for p in prev_tree
               if proc_comm(p) == "ffmpeg" and ffmpeg_role(p) == "encoder")
prev_dec = sum(proc_cpu_ticks(p) for p in prev_tree
               if proc_comm(p) == "ffmpeg" and ffmpeg_role(p) == "decoder")
prev_t = start
samples = 0

while True:
    now = time.monotonic()
    if now - start >= duration:
        break
    time.sleep(max(0.0, interval - (now % interval)))
    now = time.monotonic()
    dt = now - prev_t
    if dt <= 0:
        continue
    cur = read_cpu_stat()
    pids = tree_pids(mcu_pid)
    cur_pidtree = sum(proc_cpu_ticks(p) for p in pids)
    ffmpeg = [p for p in pids if proc_comm(p) == "ffmpeg"]
    encoders = [p for p in ffmpeg if ffmpeg_role(p) == "encoder"]
    decoders = [p for p in ffmpeg if ffmpeg_role(p) == "decoder"]
    ffmpeg_ticks = sum(proc_cpu_ticks(p) for p in ffmpeg)
    enc_ticks = sum(proc_cpu_ticks(p) for p in encoders)
    dec_ticks = sum(proc_cpu_ticks(p) for p in decoders)
    row = {
        "t": round(now - start, 3),
        "mode": mode,
        # cgroup-режим: абсолютные монотонные значения (делятся в формуле)
        "usage_usec": cur.get("usage_usec", 0),
        "user_usec": cur.get("user_usec", 0),
        "system_usec": cur.get("system_usec", 0),
        "d_usage_usec": cur.get("usage_usec", 0) - prev.get("usage_usec", 0),
        # pidtree-режим: тики по всему дереву
        "pidtree_ticks": cur_pidtree,
        "d_pidtree_ticks": cur_pidtree - prev_pidtree,
        # контур: процесс + дети
        "pids_total": len(pids),
        "ffmpeg_count": len(ffmpeg),
        "ffmpeg_encoder_count": len(encoders),
        "ffmpeg_decoder_count": len(decoders),
        "ffmpeg_ticks": ffmpeg_ticks,
        "encoder_ticks": enc_ticks,
        "decoder_ticks": dec_ticks,
        "d_ffmpeg_ticks": ffmpeg_ticks - prev_ffmpeg,
        "d_encoder_ticks": enc_ticks - prev_enc,
        "d_decoder_ticks": dec_ticks - prev_dec,
        "pids_current": read_int(os.path.join(cgroup, "pids.current")),
        "memory_current": read_int(os.path.join(cgroup, "memory.current")),
        "dt": round(dt, 6),
    }
    dtree = max(1, cur_pidtree - prev_pidtree)
    row["ffmpeg_share_of_contour"] = round((ffmpeg_ticks - prev_ffmpeg) / dtree, 4)
    row["encoder_share_of_contour"] = round((enc_ticks - prev_enc) / dtree, 4)
    row["decoder_share_of_contour"] = round((dec_ticks - prev_dec) / dtree, 4)
    row["mcu_allowed"] = proc_allowed(mcu_pid)
    row["ffmpeg_allowed"] = proc_allowed(ffmpeg[0]) if ffmpeg else "?"
    press = read_pressure()
    row["pressure_cpu_some_avg10"] = press.get("some", {}).get("avg10", 0.0)
    row["pressure_cpu_total"] = press.get("some", {}).get("total", 0.0)
    rows.append(row)
    prev, prev_pidtree, prev_ffmpeg, prev_enc, prev_dec, prev_t = (
        cur, cur_pidtree, ffmpeg_ticks, enc_ticks, dec_ticks, now,
    )
    samples += 1
    if samples % 15 == 0:
        print(f"[collect] t={row['t']:.0f}s usage_usec={row['usage_usec']} "
              f"pids={row['pids_total']} ffmpeg={row['ffmpeg_count']} "
              f"ffmpeg_share={row['ffmpeg_share_of_contour']}", flush=True)

with open(os.path.join(outdir, "samples.json"), "w") as fh:
    json.dump(rows, fh)
print(f"[collect] samples={samples} -> {outdir}/samples.json")
PY

wait "$PIDSTAT_PID" 2>/dev/null || true
wait "$MPSTAT1" 2>/dev/null || true
wait "$MPSTAT2" 2>/dev/null || true

# 4. Инвентарь детей MCU на конец окна (для отчёта: сколько ffmpeg и на каких ядрах).
{
  printf 'pid\tppid\tcomm\tcpus_allowed\tutime_stime_ticks\n'
  python3 - "$MCU_PID" <<'PY'
import os, sys
root = int(sys.argv[1])
children = {}
for name in os.listdir("/proc"):
    if not name.isdigit():
        continue
    pid = int(name)
    try:
        data = open(f"/proc/{pid}/stat").read()
        ppid = int(data.rsplit(")", 1)[1].split()[1])
    except (OSError, IndexError, ValueError):
        continue
    children.setdefault(ppid, []).append(pid)
stack, out = [root], []
while stack:
    pid = stack.pop()
    out.append(pid)
    stack.extend(children.get(pid, []))
for pid in out:
    try:
        data = open(f"/proc/{pid}/stat").read()
        fields = data.rsplit(")", 1)[1].split()
        comm = data.split("(", 1)[1].rsplit(")", 1)[0]
        ppid = fields[1]
        allowed = "?"
        for line in open(f"/proc/{pid}/status"):
            if line.startswith("Cpus_allowed_list:"):
                allowed = line.split()[1]
                break
        print(f"{pid}\t{ppid}\t{comm}\t{allowed}\t{int(fields[11]) + int(fields[12])}")
    except (OSError, IndexError, ValueError):
        continue
PY
} >"$OUTDIR/mcu_process_tree.tsv"

# 5. meta.json: провенанс прогона (см. 06_perf_lab.md §3.5).
python3 - "$OUTDIR" "$MCU_PID" "$CGROUP" "$MODE" "$DURATION" \
  "$(awk '/Cpus_allowed_list/{print $2}' "/proc/$MCU_PID/status")" <<'PY'
import json, os, subprocess, sys, time
outdir, pid, cgroup, mode, duration, allowed = sys.argv[1:7]

def run(cmd):
    try:
        return subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=10).stdout.strip()
    except Exception as error:  # noqa: BLE001 - провенанс не должен ронять замер
        return f"error: {error}"

meta = {
    "collected_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    "duration_s": float(duration),
    "mode": mode,
    "mcu_pid": int(pid),
    "mcu_cgroup": cgroup,
    "mcu_cpus_allowed": allowed,
    # --- паспорт артефакта (требование regress-hygiene, см. verify_artifact.sh) ---
    "mcu_exe_sha256": run(f"sha256sum /proc/{pid}/exe | cut -d' ' -f1"),
    "mcu_exe_deleted": run(f"readlink /proc/{pid}/exe | grep -c deleted || true").endswith("1"),
    "mcu_exe_inode": run(f"stat -L -c %i /proc/{pid}/exe"),
    "mcu_exe_mtime": run(f"stat -L -c %y /proc/{pid}/exe"),
    "disk_exe_sha256": run("sha256sum '/home/pro/Рабочий стол/mcu/target/release/vks-server' | cut -d' ' -f1"),
    "artifact_json": run(f"test -f {os.path.join(outdir, '..', 'artifact.json')} && echo present || echo absent"),
    "nproc": os.cpu_count(),
    "cpu_model": next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), "?"),
    "kernel": run("uname -r"),
    "git_sha": run("git -C \"$PWD\" rev-parse --short HEAD 2>/dev/null || git -C '/home/pro/Рабочий стол/mcu' rev-parse --short HEAD"),
    "git_dirty": run("git -C '/home/pro/Рабочий стол/mcu' status --porcelain | head -1 | wc -l"),
    "ffmpeg_version": run("ffmpeg -hide_banner -version | head -1"),
    "loadavg": open("/proc/loadavg").read().strip(),
    "isolation_env": open("/tmp/vks-bench/state/isolation.env").read().strip() if os.path.exists("/tmp/vks-bench/state/isolation.env") else "none",
    "netem_env": open("/tmp/vks-bench/state/netem.env").read().strip() if os.path.exists("/tmp/vks-bench/state/netem.env") else "none",
    "accounting_note": "P_mcu_contour = cgroup cpu.stat (usage_usec) включает vks-server + python h323_listener + все FFmpeg-дети; pidtree-режим — сумма utime+stime по дереву",
}
with open(os.path.join(outdir, "meta.json"), "w") as fh:
    json.dump(meta, fh, indent=2, ensure_ascii=False)
print("[collect] meta.json записан")
PY

log "готово: $OUTDIR/{samples.json,meta.json,pidstat_mcu.log,mpstat_mcu.log,mpstat_bots.log,mcu_process_tree.tsv}"
