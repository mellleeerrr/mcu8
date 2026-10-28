#!/usr/bin/env bash
# setup_isolation.sh — разделение CPU (и сети) между MCU и генераторами нагрузки.
#
# Живой vks-server НЕ перезапускается. Всё, что делается, обратимо
# (teardown_isolation.sh возвращает affinity 0-35 и снимает слайсы/netem).
#
# Режимы:
#   --no-root  (по умолчанию, если нет sudo): taskset -ac на живом процессе MCU,
#              CPUWeight/CPUQuota для ботов через systemd --user, сеть — loopback.
#   --root     cgroup v2 cpuset через systemd (mcu.slice/bots.slice) +
#              veth-пара 10.200.0.1<->10.200.0.2 + netns для ботов.
#
# Использование:
#   scripts/bench/setup_isolation.sh --no-root [--mcu-cores 0-15] [--bot-cores 16-35]
#   scripts/bench/setup_isolation.sh --root    [--mcu-cores 0-15] [--bot-cores 16-35]
set -euo pipefail
export LC_ALL=C

MODE="no-root"
MCU_CORES="0-15"
BOT_CORES="16-35"
VETH_MCU="veth-mcu"
VETH_BOTS="veth-bots"
NETNS="bots"
VETH_MCU_IP="10.200.0.1"
VETH_BOTS_IP="10.200.0.2"
STATE_DIR="/tmp/vks-bench/state"

log() { printf '[iso] %s\n' "$*"; }
die() { printf '[iso] FATAL: %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root) MODE="root"; shift ;;
    --no-root) MODE="no-root"; shift ;;
    --mcu-cores) MCU_CORES="$2"; shift 2 ;;
    --bot-cores) BOT_CORES="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

mkdir -p "$STATE_DIR"

mcu_pid() {
  systemctl --user show -p MainPID --value vks-mcu.service 2>/dev/null || true
}

have_root() { [[ "$(id -u)" -eq 0 ]]; }

verify_affinity() {
  local pid="$1" allowed="$2" label="$3" bad=0 seen=""
  [[ -n "$pid" && -d "/proc/$pid" ]] || { log "WARN: $label pid пуст — пропускаю проверку"; return 0; }
  for t in /proc/"$pid"/task/*; do
    local a
    a="$(taskset -pc "$t" 2>/dev/null | sed 's/.*: //')" || continue
    seen+="$a "
    # грубая проверка: печатаем уникальные маски, точное сравнение делает collect.sh
  done
  log "affinity $label (pid=$pid): $(printf '%s' "$seen" | tr ' ' '\n' | sort -u | tr '\n' ' ')"
  # точная проверка через Cpus_allowed_list главного потока
  local list
  list="$(awk '/Cpus_allowed_list/{print $2}' "/proc/$pid/status" 2>/dev/null || true)"
  log "  Cpus_allowed_list($pid) = ${list:-?} (ожидаем $allowed)"
  if [[ -n "$list" && "$list" != "$allowed" ]]; then
    log "  WARN: главный поток не на ожидаемом диапазоне"
    bad=1
  fi
  return "$bad"
}

case "$MODE" in
  no-root)
    log "режим no-root: taskset + systemd --user (cpuset/netem недоступны)"
    P="$(mcu_pid)"
    [[ -n "$P" ]] || die "vks-mcu.service не активен (systemctl --user show -p MainPID)"
    log "MCU pid=$P -> cores $MCU_CORES (все треды, -a)"
    taskset -ac "$MCU_CORES" "$P" || die "taskset -ac не удался (нужна своя задача: uid совпадает?)"
    # Новые треды наследуют affinity создателя, но созданные ДО setup надо перечекать:
    sleep 0.3
    bad_threads="$(ps -L -o tid=,psr= -p "$P" | awk -v lo="${MCU_CORES%-*}" -v hi="${MCU_CORES#*-}" '$2<lo||$2>hi' | wc -l)"
    log "тредов MCU вне $MCU_CORES прямо сейчас: $bad_threads (0 не гарантирован мгновенно: миграция планировщиком)"
    verify_affinity "$P" "$MCU_CORES" "MCU" || true

    # Боты: user-slice с весом. cpu-контроллер делегирован (cgroup.subtree_control= cpu memory pids),
    # поэтому CPUWeight/CPUQuota работают без root.
    if systemctl --user status bots.slice >/dev/null 2>&1 || systemctl --user start bots.slice 2>/dev/null; then
      log "bots.slice доступен (user)"
    else
      log "WARN: bots.slice (user) не создан; боты будут запускаться обычными процессами с taskset"
    fi
    cat >"$STATE_DIR/isolation.env" <<EOF
ISOLATION_MODE=no-root
MCU_CORES=$MCU_CORES
BOT_CORES=$BOT_CORES
MCU_PID=$P
ACCOUNTING=cgroup
BOT_NETNS=0
EOF
    ;;

  root)
    have_root || die "--root требует uid=0 (запускать через sudo)"
    log "режим root: cgroup cpuset + veth + netns"

    # 1. cpuset должен быть делегирован детям root cgroup.
    ctl="$(cat /sys/fs/cgroup/cgroup.controllers)"
    [[ " $ctl " == *" cpuset "* ]] || die "cpuset нет в cgroup.controllers ($ctl)"
    if ! grep -q cpuset /sys/fs/cgroup/cgroup.subtree_control; then
      log "включаю cpuset в root subtree_control (одноразово, необратимо в рамках загрузки)"
      echo "+cpuset" > /sys/fs/cgroup/cgroup.subtree_control
    fi
    grep -q cpuset /sys/fs/cgroup/user.slice/cgroup.subtree_control 2>/dev/null \
      || echo "+cpuset" > /sys/fs/cgroup/user.slice/cgroup.subtree_control || true
    grep -q cpuset /sys/fs/cgroup/user.slice/user-1000.slice/cgroup.subtree_control 2>/dev/null \
      || echo "+cpuset" > /sys/fs/cgroup/user.slice/user-1000.slice/cgroup.subtree_control || true

    # 2. Юниты слайсов: только файлы в /etc/systemd/system (dev-стенд, не production).
    install -m 0644 /dev/stdin /etc/systemd/system/mcu.slice <<'UNIT'
[Unit]
Description=MCU CPU island (16 cores)
[Slice]
AllowedCPUs=0-15
CPUWeight=1000
MemoryMax=24G
UNIT
    install -m 0644 /dev/stdin /etc/systemd/system/bots.slice <<'UNIT'
[Unit]
Description=Load-generator CPU island (20 cores)
[Slice]
AllowedCPUs=16-35
CPUWeight=100
UNIT
    systemctl daemon-reload
    systemctl start mcu.slice bots.slice

    # 3. Перевести живой юнит в mcu.slice без рестарта:
    #    Slice= меняется только drop-in + restart, поэтому делаем точечно:
    #    set-property AllowedCPUs на живом юните (cgroup-атрибут применяется сразу).
    log "AllowedCPUs=$MCU_CORES на живом vks-mcu.service (без рестарта)"
    systemctl --user set-property vks-mcu.service "AllowedCPUs=$MCU_CORES"
    log "  cpuset.cpus.effective = $(cat /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/vks-mcu.service/cpuset.cpus.effective 2>/dev/null || echo '?')"

    # 4. Сеть: veth + netns ботов.
    if ip link show "$VETH_MCU" >/dev/null 2>&1; then
      log "veth уже существует, переиспользую"
    else
      ip link add "$VETH_MCU" type veth peer name "$VETH_BOTS"
    fi
    ip addr replace "$VETH_MCU_IP/24" dev "$VETH_MCU"
    ip link set "$VETH_MCU" up
    ip netns add "$NETNS" 2>/dev/null || true
    if ! ip link show "$VETH_BOTS" >/dev/null 2>&1; then
      log "veth-bots уже перемещён в netns $NETNS"
    else
      ip link set "$VETH_BOTS" netns "$NETNS"
    fi
    ip -n "$NETNS" addr replace "$VETH_BOTS_IP/24" dev "$VETH_BOTS"
    ip -n "$NETNS" link set "$VETH_BOTS" up
    ip -n "$NETNS" link set lo up
    ip -n "$NETNS" route replace default via "$VETH_MCU_IP"

    # 5. RTP-порты ботов (20000-39999) не должны пересекаться с пулом MCU (41000-41200).
    log "порты MCU-пула 41000-41200 заняты самим MCU; боты используют 20000-39999"

    cat >"$STATE_DIR/isolation.env" <<EOF
ISOLATION_MODE=root
MCU_CORES=$MCU_CORES
BOT_CORES=$BOT_CORES
MCU_PID=$(mcu_pid)
ACCOUNTING=cgroup
BOT_NETNS=1
BOT_VETH_IP=$VETH_BOTS_IP
EOF
    ;;
esac

log "готово; состояние: $STATE_DIR/isolation.env"
log "проверка: ps -L -o pid,tid,psr,comm -p \$(systemctl --user show -p MainPID --value vks-mcu.service)"
