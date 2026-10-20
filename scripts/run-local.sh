#!/usr/bin/env bash
# Единый локальный рантайм MCU: ровно один процесс из свежего release-бинаря.
#
# Проблема, которую решает скрипт: старый `vks-server` продолжает слушать :8080
# после пересборки, и UI работает со старой сборкой (405/501 на существующих
# маршрутах). Скрипт всегда останавливает предыдущий процесс перед запуском.
#
# Использование:
#   ./scripts/run-local.sh            # restart: stop + build + start
#   ./scripts/run-local.sh start      # stop + start (без сборки)
#   ./scripts/run-local.sh build      # только сборка
#   ./scripts/run-local.sh stop       # остановить по PID-файлу
#   ./scripts/run-local.sh status     # healthz/readyz/порт/версия бинаря
#
# Переменные:
#   VKS_CONFIG  — путь к TOML (по умолчанию configs/default.toml)
#   VKS_PORT    — порт для health-проверок (по умолчанию 8080)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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

# PID слушателя порта: $! после nohup может указывать на обёртку, а не на бинарь.
listener_pid() {
    ss -ltnp 2>/dev/null | grep ":${PORT} " | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2
}

terminate() {
    local pid="$1"
    [[ -n "$pid" ]] || return 0
    kill -0 "$pid" 2>/dev/null || return 0
    # Чужие процессы не трогаем: только наш release-бинарь. После пересборки
    # старый процесс держит уже удалённый файл — readlink отдаёт «$BIN (deleted)».
    local exe
    exe="$(readlink "/proc/${pid}/exe" 2>/dev/null || true)"
    exe="${exe% (deleted)}"
    if [[ "$exe" != "$BIN" ]]; then
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

stop() {
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

start() {
    [[ -x "$BIN" ]] || { log "нет бинаря $BIN — сначала build"; exit 1; }
    [[ -f "$CONFIG" ]] || { log "нет конфига $CONFIG"; exit 1; }
    log "старт: $BIN --config $CONFIG"
    # Свой session, отвязанные stdio и exec: обёртка не держит пайп и не получает SIGHUP.
    ( cd "$ROOT" && exec setsid "$BIN" --config "$CONFIG" </dev/null >>"$LOG" 2>&1 ) &
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
    log "healthz не поднялся за 30с; хвост лога:"
    tail -20 "$LOG" || true
    return 1
}

status() {
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
    start) stop; start ;;
    restart) stop; build; start ;;
    status) status ;;
    *) echo "usage: $0 {restart|start|build|stop|status}" >&2; exit 2 ;;
esac
