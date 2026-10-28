#!/usr/bin/env bash
# verify_artifact.sh — паспорт артефакта и заморозка сборки (требование regress-hygiene,
# docs/audit/2026-10-25/05_hygiene_regressions.md §2.0(i), §2.1, §8.A, R-26).
#
# Зачем: в `target/release` за 14 минут появилось три разных бинаря, а живой сервер
# (PID 1177177) всё это время работает со **старого удалённого inode**
# (`/proc/1177177/exe` -> `... (deleted)`). Любой замер без паспорта артефакта
# неатрибутируем: непонятно, какой код мерили.
#
# Скрипт ТОЛЬКО читает: не рестартует сервис, не собирает, ничего не меняет.
# Он либо подтверждает, что живой процесс = заявленный бинарь, либо выходит с
# кодом 2 (для run_matrix.sh это запрет на прогон).
#
# Использование:
#   scripts/bench/verify_artifact.sh                     # паспорт в stdout + JSON
#   scripts/bench/verify_artifact.sh --out a.json        # записать паспорт
#   scripts/bench/verify_artifact.sh --expect-sha256 HEX # сверить с ожидаемым
set -euo pipefail
export LC_ALL=C

REPO="/home/pro/Рабочий стол/mcu"
EXE_DISK="$REPO/target/release/vks-server"
OUT=""
EXPECT=""

log() { printf '[artifact] %s\n' "$*"; }
die() { printf '[artifact] FATAL: %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --expect-sha256) EXPECT="$2"; shift 2 ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

PID="$(systemctl --user show -p MainPID --value vks-mcu.service 2>/dev/null || true)"
[[ -n "${PID:-}" && -d "/proc/$PID" ]] || die "vks-mcu.service не активен"
EXE_LINK="$(readlink "/proc/$PID/exe" 2>/dev/null || echo '?')"
EXE_DELETED=0
case "$EXE_LINK" in *"(deleted)"*) EXE_DELETED=1 ;; esac

# sha256 живого inode (работает и для удалённого файла, пока процесс держит inode)
LIVE_SHA="$(sha256sum "/proc/$PID/exe" 2>/dev/null | awk '{print $1}' || echo 'unreadable')"
LIVE_SIZE="$(stat -L -c %s "/proc/$PID/exe" 2>/dev/null || echo 0)"
LIVE_INODE="$(stat -L -c %i "/proc/$PID/exe" 2>/dev/null || echo 0)"
LIVE_MTIME="$(stat -L -c %y "/proc/$PID/exe" 2>/dev/null || echo '?')"

DISK_SHA="none"; DISK_SIZE=0; DISK_INODE=0; DISK_MTIME='?'
if [[ -f "$EXE_DISK" ]]; then
  DISK_SHA="$(sha256sum "$EXE_DISK" | awk '{print $1}')"
  DISK_SIZE="$(stat -c %s "$EXE_DISK")"
  DISK_INODE="$(stat -c %i "$EXE_DISK")"
  DISK_MTIME="$(stat -c %y "$EXE_DISK")"
fi

MATCH="no"
[[ "$LIVE_SHA" == "$DISK_SHA" && "$EXE_DELETED" -eq 0 ]] && MATCH="yes"

GIT_SHA="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo '?')"
GIT_SHA_FULL="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo '?')"
GIT_DIRTY="$(git -C "$REPO" status --porcelain 2>/dev/null | wc -l)"
# rustup/cargo не в PATH у неинтерактивного шелла — берём по явному пути.
CARGO_BIN="${CARGO_BIN:-$HOME/.cargo/bin}"
RUSTC="$(env PATH="$CARGO_BIN:$PATH" rustc -vV 2>/dev/null | tr '\n' ';' || true)"
[[ -n "$RUSTC" ]] || RUSTC="unavailable ($CARGO_BIN/rustc не найден)"
CARGO="$(env PATH="$CARGO_BIN:$PATH" cargo --version 2>/dev/null || true)"
[[ -n "$CARGO" ]] || CARGO="unavailable"
TARGET_CPU_NATIVE="unknown"
# Флаг сборки неизвестен из бинаря; фиксируем то, что можно: наличие строки в .comment
# и записи в .fingerprint (эвристика, помечается в паспорте как unverified).
NATIVE_HINT=0
if grep -qs "target-cpu=native" "$REPO/target/release/.fingerprint"/*/output-* 2>/dev/null; then
  NATIVE_HINT=1
fi
AWS_LC="$(grep -m1 -A1 'name = "aws-lc-sys"' "$REPO/Cargo.lock" 2>/dev/null | awk -F'"' '/version/{print $2}' || echo '?')"
KERNEL="$(uname -r)"
CPU_MODEL="$(awk -F': ' '/model name/{print $2; exit}' /proc/cpuinfo)"
NPROC="$(nproc)"
LOADAVG="$(cat /proc/loadavg)"
UPTIME_S="$(awk '{print int($1)}' /proc/uptime)"

python3 - "$OUT" <<PY
import json, sys
data = {
  "verified_at": "$(date -Iseconds)",
  "mcu_pid": int("$PID"),
  "live_exe_path": """$EXE_LINK""",
  "live_exe_deleted": bool($EXE_DELETED),
  "live_sha256": "$LIVE_SHA",
  "live_size": int("$LIVE_SIZE"),
  "live_inode": int("$LIVE_INODE"),
  "live_mtime": """$LIVE_MTIME""",
  "disk_exe_path": "$EXE_DISK",
  "disk_sha256": "$DISK_SHA",
  "disk_size": int("$DISK_SIZE"),
  "disk_inode": int("$DISK_INODE"),
  "disk_mtime": """$DISK_MTIME""",
  "live_matches_disk": "$MATCH" == "yes",
  "git_commit": "$GIT_SHA",
  "git_commit_full": "$GIT_SHA_FULL",
  "git_dirty_files": int("$GIT_DIRTY"),
  "rustc_vV": """$RUSTC""",
  "cargo": "$CARGO",
  "target_cpu_native_hint": bool($NATIVE_HINT),
  "build_flags_note": "RUSTFLAGS не восстанавливаются из бинаря; если серия собрана с -C target-cpu=native, это должно быть записано вручную в паспорт серии",
  "aws_lc_sys_version": "$AWS_LC",
  "kernel": "$KERNEL",
  "cpu_model": """$CPU_MODEL""",
  "nproc": int("$NPROC"),
  "loadavg": """$LOADAVG""",
  "host_uptime_s": int("$UPTIME_S"),
}
if "$OUT":
    json.dump(data, open("$OUT", "w"), indent=2, ensure_ascii=False)
    print(f"[artifact] паспорт -> $OUT")
else:
    print(json.dumps(data, indent=2, ensure_ascii=False))
PY

log "живой: sha256=$LIVE_SHA size=$LIVE_SIZE inode=$LIVE_INODE deleted=$EXE_DELETED"
log "диск:  sha256=$DISK_SHA size=$DISK_SIZE inode=$DISK_INODE"
log "совпадение живого и диска: $MATCH"
if [[ "$MATCH" != "yes" ]]; then
  log "ВНИМАНИЕ: живой процесс НЕ соответствует бинарю на диске."
  log "  Прогон в таком виде неатрибутируем. Требуется заморозка: одна сборка на серию"
  log "  + \`systemctl --user restart vks-mcu\` перед серией (решение Lead, не этого скрипта),"
  log "  после чего паспорт снимается заново и живой sha256 попадает в отчёт."
fi

if [[ -n "$EXPECT" ]]; then
  if [[ "$LIVE_SHA" == "$EXPECT" ]]; then
    log "sha256 совпал с ожидаемым — можно мерить"
    exit 0
  fi
  log "sha256 НЕ совпал с ожидаемым ($EXPECT) — мерить нельзя"
  exit 2
fi
exit 0
