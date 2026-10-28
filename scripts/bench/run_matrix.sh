#!/usr/bin/env bash
# run_matrix.sh — один прогон матрицы: комната -> боты -> сбор -> teardown -> формула.
#
# Схема окна (06_perf_lab.md §3.4):
#   10 с IDLE (сбор P_idle) -> ramp -> 20 с прогрев (отбрасывается) ->
#   DURATION с замером (P_active + SLA) -> teardown
#
# Изоляция: MCU и боты должны быть уже разведены setup_isolation.sh.
# Скрипт НЕ рестартует vks-server и не меняет его конфиг.
#
# Использование:
#   scripts/bench/run_matrix.sh <SCENARIO> <N> <PROFILE> <DURATION> [T1|T2] [ROOM_UUID]
# Примеры:
#   scripts/bench/run_matrix.sh M1 5 m1 60 T1
#   scripts/bench/run_matrix.sh M3 20 m3 130 T1 4d313d01-8300-450c-ab08-28c8a00f9f25
#   scripts/bench/run_matrix.sh P2H 20 h323-m1 130 T1
#
# Переменные:
#   VKS_PY      — venv python для H.323 (по умолчанию ~/.local/share/vks-mcu/h323-venv/bin/python3)
#   BENCH_ROOT  — корень артефактов (по умолчанию /tmp/vks-bench)
#   SIPBOT      — путь к бинарю (по умолчанию target/release/vks-sipbot)
#   MAX_LOAD    — порог load average, выше которого прогон не стартует (по умолчанию 4)
set -euo pipefail
export LC_ALL=C

SCENARIO="${1:?usage: run_matrix.sh <SCENARIO> <N> <PROFILE> <DURATION> [T1|T2] [ROOM]}"; shift
N="${1:?N}"; shift
PROFILE="${1:?PROFILE}"; shift
DURATION="${1:?DURATION}"; shift
TOPOLOGY="${1:-T1}"; [[ $# -gt 0 ]] && shift || true
ROOM="${1:-}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
BENCH_ROOT="${BENCH_ROOT:-/tmp/vks-bench}"
VKS_PY="${VKS_PY:-$HOME/.local/share/vks-mcu/h323-venv/bin/python3}"
SIPBOT="${SIPBOT:-$ROOT_DIR/target/release/vks-sipbot}"
MAX_LOAD="${MAX_LOAD:-4}"
IDLE_SECONDS="${IDLE_SECONDS:-10}"
WARMUP_SECONDS="${WARMUP_SECONDS:-20}"
RAMP_PER_SECOND="${RAMP_PER_SECOND:-2}"

STAMP="$(date +%Y%m%dT%H%M%S)"
RUN_NAME="${SCENARIO}_n${N}_${PROFILE}_${TOPOLOGY}_${STAMP}"
RUN_DIR="$BENCH_ROOT/runs/$RUN_NAME"

log() { printf '[matrix] %s\n' "$*"; }
die() { printf '[matrix] FATAL: %s\n' "$*" >&2; exit 1; }

mkdir -p "$RUN_DIR"/{idle,active,bot}

# --- 0. предполётные проверки -----------------------------------------------
[[ "$(id -u)" -ne 0 ]] || log "WARN: запущено от root; боты окажутся в root cgroup — измерение ботов ухудшится"
MCU_PID="$(systemctl --user show -p MainPID --value vks-mcu.service 2>/dev/null || true)"
if [[ -z "${MCU_PID:-}" || ! -d "/proc/$MCU_PID" ]]; then
  # запасной путь: найти по порту 5060
  MCU_PID="$(ss -lunp 2>/dev/null | awk '/:5060 /{match($0,/pid=[0-9]+/); if (RSTART) print substr($0,RSTART+4,RLENGTH-4)}' | head -1)"
fi
[[ -n "${MCU_PID:-}" && -d "/proc/$MCU_PID" ]] || die "не найден живой vks-server (systemctl --user / ss)"

# --- 0a. паспорт артефакта и заморозка сборки (regress-hygiene, §2.0(i)/R-26) --
# Без этого прогон неатрибутируем: в target/release за 14 минут появилось три
# разных бинаря, а живой сервер работал со старого удалённого inode.
"$ROOT_DIR/scripts/bench/verify_artifact.sh" --out "$RUN_DIR/artifact.json" \
  >"$RUN_DIR/artifact.log" 2>&1 || true
ART_OK="$(python3 -c 'import json,sys; print("yes" if json.load(open(sys.argv[1])).get("live_matches_disk") else "no")' \
  "$RUN_DIR/artifact.json" 2>/dev/null || echo no)"
if [[ "$ART_OK" != "yes" ]]; then
  if [[ "${ALLOW_UNFROZEN:-0}" == "1" ]]; then
    log "ВНИМАНИЕ: живой бинарь != диск (ALLOW_UNFROZEN=1 — прогон разрешён вручную, помечается unfrozen=yes)"
  else
    die "живой vks-server не соответствует бинарю на диске (см. $RUN_DIR/artifact.log). Нужна заморозка: одна сборка на серию + рестарт по решению Lead; либо ALLOW_UNFROZEN=1 для явно помеченного прогона"
  fi
fi

LOAD1="$(awk '{print $1}' /proc/loadavg)"
if awk -v l="$LOAD1" -v m="$MAX_LOAD" 'BEGIN{exit !(l>m)}'; then
  die "load average $LOAD1 > MAX_LOAD=$MAX_LOAD: машина шумит, замер невалиден (см. 06_perf_lab.md §1.1)"
fi

log "scenario=$SCENARIO N=$N profile=$PROFILE duration=${DURATION}s topology=$TOPOLOGY"
log "mcu_pid=$MCU_PID load1=$LOAD1 run=$RUN_DIR"

# --- 1. комната -------------------------------------------------------------
if [[ -z "$ROOM" ]]; then
  ROOM="$("$ROOT_DIR/scripts/bench/room.sh" ensure --name "bench-${STAMP}")" || die "room.sh не смог создать комнату"
fi
log "room=$ROOM"

# --- 2. meta.json прогона ---------------------------------------------------
python3 - "$RUN_DIR/meta.json" "$SCENARIO" "$N" "$PROFILE" "$DURATION" "$TOPOLOGY" "$ROOM" "$MCU_PID" "$ART_OK" <<'PY'
import json, os, subprocess, sys
path, scenario, n, profile, duration, topology, room, pid, artifact_ok = sys.argv[1:10]
iso = "/tmp/vks-bench/state/isolation.env"
netem = "/tmp/vks-bench/state/netem.env"
meta = {
    "scenario": scenario, "n": int(n), "profile": profile, "duration_s": float(duration),
    "topology": topology, "room": room, "mcu_pid": int(pid),
    "isolation_env": open(iso).read().strip() if os.path.exists(iso) else "none",
    "netem_env": open(netem).read().strip() if os.path.exists(netem) else "none",
    "mcu_cpus_allowed": next((l.split()[1] for l in open(f"/proc/{pid}/status") if l.startswith("Cpus_allowed_list")), "?"),
    "loadavg_before": open("/proc/loadavg").read().strip(),
    "artifact_verified": artifact_ok == "yes",
    "unfrozen": artifact_ok != "yes",
    "artifact_json": os.path.join(os.path.dirname(path), "artifact.json"),
    "git_sha": subprocess.run("git -C '/home/pro/Рабочий стол/mcu' rev-parse --short HEAD",
                              shell=True, capture_output=True, text=True).stdout.strip(),
}
json.dump(meta, open(path, "w"), indent=2, ensure_ascii=False)
print(f"[matrix] meta -> {path}")
PY

# --- 3. IDLE-окно -----------------------------------------------------------
log "IDLE-окно ${IDLE_SECONDS}s"
"$ROOT_DIR/scripts/bench/collect.sh" "$RUN_DIR/idle" "$IDLE_SECONDS" --mcu-pid "$MCU_PID" --interval 1 >"$RUN_DIR/idle.log" 2>&1

# --- 4. запуск ботов --------------------------------------------------------
BOT_PIDS=(); BOT_KIND="none"
start_bots() {
  case "$SCENARIO" in
    P2H|H323*)
      BOT_KIND="h323"
      log "H.323-бот: $N вызовов (sidecar + RTP), alias=$ROOM"
      "$VKS_PY" "$ROOT_DIR/scripts/bench/h323bot.py" \
        --target 127.0.0.1:1720 --alias "$ROOM" \
        --calls "$N" --signal-base-port 2720 --rtp-base-port 30000 \
        --profile "$PROFILE" --duration "$((DURATION + WARMUP_SECONDS))" \
        --ramp "$RAMP_PER_SECOND" --report "$RUN_DIR/bot" &
      BOT_PIDS+=("$!")
      ;;
    P3MIX*)
      BOT_KIND="mixed"
      local half=$((N / 2)); [[ "$half" -lt 1 ]] && half=1
      local rest=$((N - half))
      log "MIX: $half SIP + $rest H.323"
      "$SIPBOT" --target 127.0.0.1:5060 --room "$ROOM" --local-ip 192.168.1.93 \
        --calls "$half" --signal-base-port 5080 --call-base-port 20000 \
        --profile "$PROFILE" --duration "$((DURATION + WARMUP_SECONDS))" \
        --ramp "$RAMP_PER_SECOND" --report "$RUN_DIR/bot" &
      BOT_PIDS+=("$!")
      "$VKS_PY" "$ROOT_DIR/scripts/bench/h323bot.py" \
        --target 127.0.0.1:1720 --alias "$ROOM" \
        --calls "$rest" --signal-base-port 2720 --rtp-base-port 30000 \
        --profile "$PROFILE" --duration "$((DURATION + WARMUP_SECONDS))" \
        --ramp "$RAMP_PER_SECOND" --report "$RUN_DIR/bot" &
      BOT_PIDS+=("$!")
      ;;
    *)
      BOT_KIND="sip"
      [[ -x "$SIPBOT" ]] || die "нет бинаря $SIPBOT (cargo build --release -p vks-gateway-sip --features bench-bot --bin vks-sipbot)"
      log "SIP-бот: $N вызовов, profile=$PROFILE"
      local video_args=()
      case "$PROFILE" in
        m3) video_args=(--video-file "$BENCH_ROOT/media/720p30_baseline.h264" --video-fps 30) ;;
        m4) video_args=(--video-file "$BENCH_ROOT/media/720p30_baseline.h264" --video-fps 30) ;;
      esac
      "$SIPBOT" --target 127.0.0.1:5060 --room "$ROOM" --local-ip 192.168.1.93 \
        --calls "$N" --signal-base-port 5080 --call-base-port 20000 \
        --profile "$PROFILE" --duration "$((DURATION + WARMUP_SECONDS))" \
        --ramp "$RAMP_PER_SECOND" "${video_args[@]}" --report "$RUN_DIR/bot" &
      BOT_PIDS+=("$!")
      ;;
  esac
  printf '%s\n' "${BOT_PIDS[@]}" >"$RUN_DIR/bot_pids.txt"
}
start_bots
log "боты запущены (pids: ${BOT_PIDS[*]}), вид=$BOT_KIND"

cleanup() {
  log "teardown: SIGTERM ботам (они обязаны отправить BYE)"
  for pid in "${BOT_PIDS[@]:-}"; do
    [[ -n "$pid" ]] && kill -TERM "$pid" 2>/dev/null || true
  done
  sleep 3
  for pid in "${BOT_PIDS[@]:-}"; do
    [[ -n "$pid" ]] && kill -KILL "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT INT TERM

# --- 5. прогрев и ожидание готовности --------------------------------------
log "прогрев ${WARMUP_SECONDS}s (в расчёт не входит)"
sleep "$WARMUP_SECONDS"
ACTIVE_PIDS="$(cat "$RUN_DIR/bot_pids.txt" 2>/dev/null | tr '\n' ' ')"
log "живых ботов: $(for p in $ACTIVE_PIDS; do kill -0 "$p" 2>/dev/null && echo x; done | wc -l)"

# --- 6. ОКНО ИЗМЕРЕНИЯ ------------------------------------------------------
log "окно измерения ${DURATION}s"
"$ROOT_DIR/scripts/bench/collect.sh" "$RUN_DIR/active" "$DURATION" --mcu-pid "$MCU_PID" --interval 1 >"$RUN_DIR/active.log" 2>&1

# --- 7. teardown и проверка чистоты ----------------------------------------
cleanup
trap - EXIT INT TERM
sleep 2
LEFTOVER="$(cat "/sys/fs/cgroup$(awk -F: '/^0::/{print $3}' "/proc/$MCU_PID/cgroup")/cgroup.procs" 2>/dev/null | wc -l)"
log "pids в cgroup MCU после teardown: $LEFTOVER (ожидаем 1-2: сам сервер + h323_listener)"
ss -ltnu 2>/dev/null | grep -cE ':(4(1|3)[0-9]{3})\b' >/dev/null || true
log "занятых портов пула MCU (41000-41200/43000-43100): $(ss -lun 2>/dev/null | grep -cE ':4(1|3)[0-9]{3} ' || true)"

# --- 8. формула для этого прогона ------------------------------------------
"$ROOT_DIR/scripts/bench/formula.py" --run "$RUN_DIR" --json "$RUN_DIR/summary.json" --markdown \
  >"$RUN_DIR/formula.md" 2>>"$RUN_DIR/formula.err" || log "formula.py вернул ошибку (см. $RUN_DIR/formula.err)"
log "готово: $RUN_DIR"
log "сводка: $RUN_DIR/formula.md"
