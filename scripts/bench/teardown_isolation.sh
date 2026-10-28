#!/usr/bin/env bash
# teardown_isolation.sh — вернуть стенд в исходное состояние.
# Возвращает affinity всем тредам MCU на 0-35, снимает netem, netns, veth и слайсы.
# Живой сервер не перезапускается.
set -euo pipefail
export LC_ALL=C

MODE="auto"
STATE_DIR="/tmp/vks-bench/state"
VETH_MCU="veth-mcu"
VETH_BOTS="veth-bots"
NETNS="bots"

log() { printf '[teardown] %s\n' "$*"; }
die() { printf '[teardown] FATAL: %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root) MODE="root"; shift ;;
    --no-root) MODE="no-root"; shift ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

if [[ "$MODE" == "auto" ]]; then
  MODE="no-root"
  [[ -f "$STATE_DIR/isolation.env" ]] && MODE="$(awk -F= '/^ISOLATION_MODE=/{print $2}' "$STATE_DIR/isolation.env")"
fi

# 1. netem снять всегда, если он есть и есть права (иначе молча пропустить)
if [[ "$(id -u)" -eq 0 ]] && ip netns list 2>/dev/null | grep -q "^$NETNS"; then
  ip -n "$NETNS" qdisc del dev "$VETH_BOTS" root 2>/dev/null || true
  log "netem снят (если был)"
fi

# 2. affinity MCU вернуть на все ядра
PID="$(systemctl --user show -p MainPID --value vks-mcu.service 2>/dev/null || true)"
if [[ -n "$PID" && -d "/proc/$PID" ]]; then
  taskset -ac 0-35 "$PID" && log "MCU pid=$PID affinity -> 0-35"
  log "  Cpus_allowed_list=$(awk '/Cpus_allowed_list/{print $2}' "/proc/$PID/status")"
else
  log "WARN: vks-mcu.service не активен, affinity не менялся"
fi

# 3. root-часть
if [[ "$MODE" == "root" && "$(id -u)" -eq 0 ]]; then
  for u in mcu.slice bots.slice; do
    systemctl set-property --runtime "$u" AllowedCPUs= 2>/dev/null || true
    systemctl stop "$u" 2>/dev/null || true
    rm -f "/etc/systemd/system/$u"
    log "слайс $u удалён"
  done
  systemctl daemon-reload || true
  ip netns del "$NETNS" 2>/dev/null || true
  ip link del "$VETH_MCU" 2>/dev/null || true
  log "netns/veth удалены"
fi

# 4. боты — добить, если остались (свои процессы)
pkill -f 'vks-sipbot' 2>/dev/null || true
pkill -f 'scripts/bench/h323bot.py' 2>/dev/null || true
log "процессы ботов остановлены (если были)"

rm -f "$STATE_DIR/isolation.env"
log "готово"
