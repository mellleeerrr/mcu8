#!/usr/bin/env bash
# Единый локальный рантайм MCU.
#
# Владелец процесса ровно один:
#  - если установлен systemd-юнит (prod `vks-mcu-prod` или dev `vks-mcu`),
#    скрипт управляет ТОЛЬКО им и никогда не запускает второй процесс вручную;
#  - если юнита нет, запускает свежий release-бинарь из репозитория.
#
# Использование:
#   ./scripts/run-local.sh            # restart: build + restart/start
#   ./scripts/run-local.sh start      # start (unit или manual)
#   ./scripts/run-local.sh build      # только сборка
#   ./scripts/run-local.sh stop       # остановить владельца
#   ./scripts/run-local.sh status     # unit/healthz/readyz/порт/бинарь
#   ./scripts/run-local.sh logs       # journalctl для unit, иначе repo-лог
#
# Переменные:
#   VKS_CONFIG  — TOML для manual-режима (по умолчанию configs/default.toml)
#   VKS_PORT    — порт health-проверок (по умолчанию 8080)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
CONFIG="${VKS_CONFIG:-$ROOT/configs/default.toml}"
PORT="${VKS_PORT:-8080}"
BIN="$ROOT/target/release/vks-server"
LOG="$ROOT/vks_server.log"
PIDFILE="${VKS_PIDFILE:-/tmp/vks-server-local.pid}"

log() { printf '[run-local] %s\n' "$*"; }

build() {
    log "сборка release ($ROOT)"
    cargo build --release -p vks-server --manifest-path "$ROOT/Cargo.toml"
}

listener_pid() {
    ss -ltnp 2>/dev/null | grep ":${PORT} " | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2
}

# Установленный unit — не только активный: остановленный unit всё равно
# владелец, и manual-запуск создаст конфликт при следующем systemctl start.
unit_spec() {
    command -v systemctl >/dev/null 2>&1 || return 1
    if systemctl cat vks-mcu-prod.service >/dev/null 2>&1; then
        printf '# system vks-mcu-prod.service\n'
        return 0
    fi
    if systemctl --user cat vks-mcu.service >/dev/null 2>&1; then
        printf '# user vks-mcu.service\n'
        return 0
    fi
    return 1
}

unit_do() {
    local scope="$1" unit="$2"; shift 2
    if [[ "$scope" == "user" ]]; then
        systemctl --user "$@" "$unit"
    else
        systemctl "$@" "$unit"
    fi
}

check_unit_overrides() {
    local spec scope unit
    spec="$(unit_spec)" || return 0
    read -r _ scope unit <<<"$spec"
    if [[ -n "${VKS_CONFIG:-}" || -n "${VKS_PORT:-}" ]]; then
        log "VKS_CONFIG/VKS_PORT игнорируются: активен $scope unit $unit (его ExecStart/порт — источник истины)"
        return 1
    fi
}

wait_health() {
    for _ in $(seq 1 30); do
        if curl -fsS "http://127.0.0.1:${PORT}/healthz" >/dev/null 2>&1; then
            local pid
            pid="$(listener_pid)"
            [[ -n "$pid" ]] && echo "$pid" > "$PIDFILE"
            log "healthz OK, pid=${pid:-?}"
            return 0
        fi
        sleep 1
    done
    log "healthz не поднялся за 30с; журнал/лог:"
    show_logs 20 || true
    return 1
}

terminate() {
    local pid="$1"
    [[ -n "$pid" ]] || return 0
    kill -0 "$pid" 2>/dev/null || return 0
    local exe bin_real
    exe="$(readlink -f "/proc/${pid}/exe" 2>/dev/null || readlink "/proc/${pid}/exe" 2>/dev/null || true)"
    exe="${exe% (deleted)}"
    bin_real="$(readlink -f "$BIN" 2>/dev/null || printf '%s' "$BIN")"
    if [[ "$exe" != "$bin_real" ]]; then
        log "pid=$pid не наш бинарь ($exe) — не трогаю"
        return 0
    fi
    log "SIGTERM pid=$pid (graceful shutdown)"
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 1
    done
    log "процесс не завершился за 30с, SIGKILL pid=$pid"
    kill -KILL "$pid" 2>/dev/null || true
}

stop_manual() {
    if [[ -f "$PIDFILE" ]]; then
        terminate "$(cat "$PIDFILE" 2>/dev/null || true)"
        rm -f "$PIDFILE"
    fi
    terminate "$(listener_pid)"
    for _ in $(seq 1 10); do
        ss -ltn 2>/dev/null | grep -q ":${PORT} " || return 0
        sleep 1
    done
    log "ВНИМАНИЕ: порт :${PORT} всё ещё занят:"
    ss -ltnp 2>/dev/null | grep ":${PORT} " || true
    return 1
}

start_manual() {
    [[ -x "$BIN" ]] || { log "нет бинаря $BIN — сначала build"; exit 1; }
    [[ -f "$CONFIG" ]] || { log "нет конфига $CONFIG"; exit 1; }
    log "старт: $BIN --config $CONFIG"
    ( cd "$ROOT" && exec setsid "$BIN" --config "$CONFIG" </dev/null >>"$LOG" 2>&1 ) &
    wait_health
}

stop() {
    local spec scope unit
    if spec="$(unit_spec)"; then
        read -r _ scope unit <<<"$spec"
        check_unit_overrides || return 2
        if [[ "$scope" == "system" && "$(id -u)" -ne 0 ]]; then
            log "unit $scope $unit не остановлен: нужен root (sudo)"
            return 2
        fi
        log "останавливаю $scope unit $unit"
        unit_do "$scope" "$unit" stop
        rm -f "$PIDFILE"
        # Unit мог быть inactive, а порт держал старый manual-процесс.
        stop_manual || return 1
        return 0
    fi
    stop_manual
}

start() {
    local spec scope unit
    if spec="$(unit_spec)"; then
        read -r _ scope unit <<<"$spec"
        check_unit_overrides || return 2
        if [[ "$scope" == "system" && "$(id -u)" -ne 0 ]]; then
            log "unit $scope $unit не запущен: нужен root (sudo)"
            return 2
        fi
        log "запускаю $scope unit $unit"
        unit_do "$scope" "$unit" start
        wait_health
        return
    fi
    start_manual
}

restart() {
    local spec scope unit
    build
    if spec="$(unit_spec)"; then
        read -r _ scope unit <<<"$spec"
        check_unit_overrides || return 2
        if [[ "$scope" == "system" && "$(id -u)" -ne 0 ]]; then
            log "unit $scope $unit не перезапущен: нужен root (sudo)"
            return 2
        fi
        log "перезапускаю $scope unit $unit"
        if [[ "$scope" == "system" ]]; then
            log "ВНИМАНИЕ: system-unit запускает /opt/vks/bin; сборка в repo не разворачивается автоматически"
        fi
        unit_do "$scope" "$unit" restart
        wait_health
        return
    fi
    stop_manual
    start_manual
}

show_logs() {
    local lines="${1:-200}" spec scope unit
    if spec="$(unit_spec)"; then
        read -r _ scope unit <<<"$spec"
        if [[ "$scope" == "user" ]]; then
            journalctl --user -u "$unit" -n "$lines" --no-pager 2>/dev/null || tail -n "$lines" "$LOG"
        else
            journalctl -u "$unit" -n "$lines" --no-pager 2>/dev/null || tail -n "$lines" "$LOG"
        fi
        return
    fi
    tail -n "$lines" "$LOG"
}

status() {
    if spec="$(unit_spec)"; then
        read -r _ scope unit <<<"$spec"
        if [[ "$scope" == "user" ]]; then
            printf 'unit: user %s\n' "$unit"
            systemctl --user show "$unit" -p ActiveState -p MainPID -p NRestarts 2>/dev/null || true
        else
            printf 'unit: system %s\n' "$unit"
            systemctl show "$unit" -p ActiveState -p MainPID -p NRestarts 2>/dev/null || true
        fi
    else
        printf 'unit: не установлен (manual-режим)\n'
    fi
    printf 'binary: %s\n' "$(stat -c '%y %s bytes' "$BIN" 2>/dev/null || echo 'нет')"
    printf 'sources HEAD: %s\n' "$(git -C "$ROOT" log -1 --format='%h %cI %s' 2>/dev/null || echo '?')"
    if [[ -f "$PIDFILE" ]]; then printf 'pidfile: %s (%s)\n' "$(cat "$PIDFILE")" "$PIDFILE"; fi
    ss -ltnp 2>/dev/null | grep ":${PORT} " || printf 'порт :%s свободен\n' "$PORT"
    for path in /healthz /readyz; do
        code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}${path}" || true)"
        printf '%s -> %s\n' "$path" "${code:-нет ответа}"
    done
    code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/pnm/" || true)"
    printf '/pnm/ -> %s\n' "${code:-нет ответа}"
}

case "${1:-restart}" in
    build) build ;;
    stop) stop ;;
    start) start ;;
    restart) restart ;;
    status) status ;;
    logs) show_logs ;;
    *) echo "usage: $0 {restart|start|build|stop|status|logs}" >&2; exit 2 ;;
esac
