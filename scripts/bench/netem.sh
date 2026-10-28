#!/usr/bin/env bash
# netem.sh — сетевые деградации для оси "Network Impairment" из ТЗ.
#
# ВНИМАНИЕ: требует root и netns ботов (setup_isolation.sh --root), потому что
# tc/netem = CAP_NET_ADMIN. В no-root режиме скрипт честно падает, а не
# притворяется, что деградация включена.
#
# Применяется на bot-стороне veth внутри netns: пакеты ботов->MCU теряются/
# задерживаются; обратный путь (MCU->боты) остаётся чистым. Это осознанно:
# нас интересует, как MCU переживает плохой вход от терминала.
#
# Использование:
#   scripts/bench/netem.sh apply --loss 2 --delay 30 --jitter 10
#   scripts/bench/netem.sh clear
#   scripts/bench/netem.sh show
set -euo pipefail
export LC_ALL=C

NETNS="bots"
DEV="veth-bots"
STATE_DIR="/tmp/vks-bench/state"

log() { printf '[netem] %s\n' "$*"; }
die() { printf '[netem] FATAL: %s\n' "$*" >&2; exit 1; }

require_root() {
  [[ "$(id -u)" -eq 0 ]] || die "netem требует root (CAP_NET_ADMIN). В no-root режиме ось сети недоступна: см. 06_perf_lab.md §2.1"
  ip netns list 2>/dev/null | grep -q "^$NETNS" || die "netns '$NETNS' отсутствует — сначала setup_isolation.sh --root"
}

cmd="${1:-show}"; shift || true
LOSS=""; DELAY=""; JITTER=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --loss) LOSS="$2"; shift 2 ;;
    --delay) DELAY="$2"; shift 2 ;;
    --jitter) JITTER="$2"; shift 2 ;;
    *) die "unknown arg: $1" ;;
  esac
done

case "$cmd" in
  apply)
    require_root
    [[ -n "$LOSS$DELAY$JITTER" ]] || die "нужен хотя бы один параметр (--loss/--delay/--jitter)"
    args=()
    if [[ -n "$DELAY" ]]; then
      args+=("delay" "${DELAY}ms")
      [[ -n "$JITTER" ]] && args+=("${JITTER}ms" "distribution" "normal")
    fi
    [[ -n "$LOSS" ]] && args+=("loss" "${LOSS}%")
    ip -n "$NETNS" qdisc replace dev "$DEV" root netem "${args[@]}"
    log "применено: netem ${args[*]} на $NETNS/$DEV"
    mkdir -p "$STATE_DIR"
    printf 'NETEM=loss=%s,delay=%s,jitter=%s\n' "${LOSS:-0}" "${DELAY:-0}" "${JITTER:-0}" >"$STATE_DIR/netem.env"
    ;;
  clear)
    require_root
    ip -n "$NETNS" qdisc del dev "$DEV" root 2>/dev/null || true
    rm -f "$STATE_DIR/netem.env"
    log "netem снят"
    ;;
  show)
    if [[ "$(id -u)" -ne 0 ]]; then
      log "root нет: показать qdisc внутри netns невозможно"
      [[ -f "$STATE_DIR/netem.env" ]] && cat "$STATE_DIR/netem.env" || log "нет сохранённого состояния"
      exit 0
    fi
    require_root
    ip -n "$NETNS" qdisc show dev "$DEV"
    ;;
  *) die "usage: $0 {apply --loss N --delay MS --jitter MS | clear | show}" ;;
esac
