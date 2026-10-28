#!/usr/bin/env bash
# t2_matrix.sh — netem-матрица T2: 6 профилей, одна переменная за раз.
#
# Профили (N=10 SIP 720p30, 60 с медиа, боты pinned 16-35, паспорт-гейт):
#   T2-0 baseline  без qdisc
#   T2-1 loss0.5   netem loss 0.5%  на обоих концах veth
#   T2-2 loss2     netem loss 2%    на обоих концах
#   T2-3 rtt30     netem delay 15ms 5ms distribution normal  (сумма RTT 30±10 мс)
#   T2-4 rtt150    netem delay 75ms  (сумма RTT 150 мс)
#   T2-5 rtt300    netem delay 150ms (сумма RTT 300 мс)
#
# Направления: veth-bench (хост) = MCU→бот, veth-mcu (netns) = бот→MCU.
# Между профилями qdisc снимается и проверяется `noqueue`; дампы `tc -s qdisc`
# кладутся в каталог профиля как доказательство применённого шейпинга.
#
# Требуется: NOPASSWD на /usr/sbin/ip и /usr/sbin/tc (см. ROOT_COMMANDS.md §C).
#
# Использование:
#   scripts/bench/t2_matrix.sh <expect-sha256> [outdir] [--plan]
#   scripts/bench/t2_matrix.sh --plan            # только напечатать команды
#   scripts/bench/t2_matrix.sh <sha> <outdir> --only T2-3b --dump-rx bot-00
#                                                # + rx-00.jsonl всех принятых RTP (для media-core)
set -uo pipefail

ROOM="1e0337c6-dfc3-491a-becd-545b7e15bcca"
HOST_MCU="192.168.1.93"
API="192.168.1.93:8080"
NETNS="vks-bench"
BOTS=10
DURATION=60
IDLE=6
CONNECT=8
MAX_LOAD=8
BOT_CORES="16-35"
SETTLE=10          # секунд покоя между профилями

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PLAN=0
SHA=""
OUT=""
ONLY=""
DUMP_RX=""

while [ $# -gt 0 ]; do
    case "$1" in
        --plan) PLAN=1 ;;
        --only) ONLY="$2"; shift ;;
        --dump-rx) DUMP_RX="$2"; shift ;;  # напр. bot-00: rx-00.jsonl для таймлайн-разбора
        -*) echo "неизвестный флаг: $1" >&2; exit 2 ;;
        *) if [ -z "$SHA" ]; then SHA="$1"; else OUT="$1"; fi ;;
    esac
    shift
done

if [ "$PLAN" -eq 0 ] && [ -z "$SHA" ]; then
    echo "usage: $0 <expect-sha256> [outdir] [--plan]" >&2
    exit 2
fi
TS="$(date +%Y%m%d-%H%M%S)"
OUT="${OUT:-/tmp/vks-bench/t2-$TS}"
mkdir -p "$OUT"

log() { printf '[t2 %s] %s\n' "$(date +%H:%M:%S)" "$*"; }
run() {
    if [ "$PLAN" -eq 1 ]; then printf '  + %s\n' "$*"; return 0; fi
    "$@"
}

# --- профили: "имя|netem-аргументы" (пусто = baseline) --------------------
PROFILES=(
    "T2-0-baseline|"
    "T2-1-loss0.5|loss 0.5%"
    "T2-2-loss2|loss 2%"
    "T2-3-rtt30|delay 15ms 5ms distribution normal"
    "T2-4-rtt150|delay 75ms"
    "T2-5-rtt300|delay 150ms"
    # Переигровки после первого прохода: netem limit по умолчанию 1000 пакетов
    # при задержке 75/150 мс и ~3.6 тыс. pps переполнялся (tc: dropped 3540/4535),
    # и бот видел 1.9-2.5 % «потерь», которых MCU не делал. limit 100000 убирает
    # артефакт; T2-3b — воспроизводимость найденного обрыва видео под 5 мс джиттером.
    "T2-3b-rtt30-confirm|limit 100000 delay 15ms 5ms distribution normal"
    "T2-4b-rtt150-limit|limit 100000 delay 75ms"
    "T2-5b-rtt300-limit|limit 100000 delay 150ms"
)

room_abonents() {
    curl -s -m 3 -H "Authorization: Bearer $(cat /tmp/vks.token 2>/dev/null)" \
        "http://127.0.0.1:8080/api/v3/sessions" 2>/dev/null | python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    print(-1); raise SystemExit
for s in doc.get("sessions", []):
    if str(s.get("conference_id", "")).startswith("1e0337c6"):
        print(int((s.get("abonents") or {}).get("count") or 0)); break
else:
    print(-1)' 2>/dev/null || echo -1
}

mcu_ffmpeg_now() {          # только дети MCU (на хосте могут быть чужие ffmpeg)
    local pid c
    pid=$(mcu_pid)
    c=$(pgrep -P "$pid" -c ffmpeg 2>/dev/null || true)
    echo "${c:-0}"
}

# Ждём чистого состояния до профиля: 0 абонентов в комнате и не больше
# base+1 FFmpeg-детей у MCU (сироты от предыдущего профиля ломают сравнение).
wait_idle() {
    local base="$1" timeout="${2:-90}" t0 ab ff
    t0=$(date +%s)
    while [ $(( $(date +%s) - t0 )) -lt "$timeout" ]; do
        ab=$(room_abonents); ff=$(mcu_ffmpeg_now)
        if [ "$ab" = "0" ] && [ "$ff" -le $((base + 1)) ]; then
            return 0
        fi
        sleep 3
    done
    return 1
}

mcu_children() {            # сколько прямых детей MCU (включая деки/энки/sidecar)
    local pid
    pid=$(mcu_pid)
    ps --ppid "$pid" -o pid= 2>/dev/null | wc -l
}

mcu_zombies() {             # <defunct> среди детей MCU = симптом «wait не сделан»
    local pid
    pid=$(mcu_pid)
    ps --ppid "$pid" -o stat= 2>/dev/null | grep -c '^Z' || true
}

live_sha() {
    local pid
    pid=$(systemctl --user show -p MainPID --value vks-mcu.service)
    sha256sum "/proc/$pid/exe" 2>/dev/null | cut -d' ' -f1
}

mcu_pid() { systemctl --user show -p MainPID --value vks-mcu.service; }

apply_qdisc() {          # $1 = netem args ("" — baseline)
    local args="$1"
    if [ -z "$args" ]; then
        run sudo -n tc qdisc del dev veth-bench root
        run sudo -n ip netns exec "$NETNS" tc qdisc del dev veth-mcu root
        return 0
    fi
    run sudo -n tc qdisc replace dev veth-bench root netem $args
    run sudo -n ip netns exec "$NETNS" tc qdisc replace dev veth-mcu root netem $args
}

cleanup() {
    [ "$PLAN" -eq 1 ] && return 0
    sudo -n tc qdisc del dev veth-bench root 2>/dev/null || true
    sudo -n ip netns exec "$NETNS" tc qdisc del dev veth-mcu root 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# --- шапка ----------------------------------------------------------------
[ -s /tmp/vks.token ] || log "ВНИМАНИЕ: /tmp/vks.token отсутствует — farm-снимки будут пустыми (эндпоинт под Moderator)"
log "SHA ожидаемый: ${SHA:-<plan>}"
log "outdir: $OUT  (N=$BOTS, ${DURATION}s, боты $BOT_CORES, netns $NETNS)"
if [ "$PLAN" -eq 0 ]; then
    LIVE="$(live_sha)"
    log "SHA живой:    $LIVE"
    if [ "$LIVE" != "$SHA" ]; then
        log "ОТКАЗ: живой sha не совпадает с ожидаемым (паспорт-гейт до старта)"
        exit 3
    fi
    if ! sudo -n tc qdisc show dev veth-bench 2>/dev/null | grep -q 'noqueue'; then
        log "ОТКАЗ: veth-bench не чист (остался qdisc?), сначала сними: sudo -n tc qdisc del dev veth-bench root"
        exit 4
    fi
    log "qdisc чист (noqueue), стартую профили"
fi
BASE_FFMPEG=$(mcu_ffmpeg_now)
log "базовая линия FFmpeg-детей MCU до старта: $BASE_FFMPEG"

# --- прогоны --------------------------------------------------------------
for entry in "${PROFILES[@]}"; do
    name="${entry%%|*}"
    if [ -n "$ONLY" ]; then
        keep=0
        IFS=',' read -ra want <<< "$ONLY"
        for w in "${want[@]}"; do
            case "$name" in "$w"*) keep=1 ;; esac
        done
        [ "$keep" -eq 0 ] && continue
    fi
    netem="${entry#*|}"
    dir="$OUT/$name"
    mkdir -p "$dir"
    log "=== $name  netem: '${netem:-нет}' ==="
    apply_qdisc "$netem"
    run sleep "$SETTLE"
    if [ "$PLAN" -eq 0 ]; then
        { sudo -n tc -s qdisc show dev veth-bench; } > "$dir/tc-host.txt" 2>&1 || true
        { sudo -n ip netns exec "$NETNS" tc -s qdisc show dev veth-mcu; } > "$dir/tc-netns.txt" 2>&1 || true
        curl -s -m 3 -H "Authorization: Bearer $(cat /tmp/vks.token 2>/dev/null)" \
            "http://127.0.0.1:8080/api/v3/mcu/farm" > "$dir/farm-idle.json" || true
    fi
    MPID="$(mcu_pid)"
    if [ "$PLAN" -eq 0 ]; then
        if wait_idle "$BASE_FFMPEG" 90; then
            CLEAN=1
        else
            CLEAN=0
            log "ВНИМАНИЕ ($name): перед стартом не чисто — abonents=$(room_abonents), ffmpeg=$(mcu_ffmpeg_now) > $BASE_FFMPEG+1"
        fi
        printf '{"profile":"%s","abonents":%s,"ffmpeg":%s,"base_ffmpeg":%s,"clean":%s}\n' \
            "$name" "$(room_abonents)" "$(mcu_ffmpeg_now)" "$BASE_FFMPEG" "$CLEAN" > "$dir/precheck.json"
    fi
    run sudo -n ip netns exec "$NETNS" sh -c "umask 022; VKS_API_TOKEN='$(cat /tmp/vks.token 2>/dev/null)' exec python3 '$REPO/scripts/bench/matrix.py' \
        --bots $BOTS --room $ROOM --host $HOST_MCU --local-ip 10.200.0.2 \
        --api $API --video-profile 720p30 --audio-codec PCMU \
        --duration $DURATION --idle-baseline $IDLE --connect-window $CONNECT \
        --bot-cores $BOT_CORES --max-load $MAX_LOAD \
        --mcu-pid ${MPID:-0} --expect-sha256 '$SHA' --allow-partial --outdir '$dir/run' \
        ${DUMP_RX:+--dump-rx $DUMP_RX}"
    if [ "$PLAN" -eq 0 ]; then
        { sudo -n tc -s qdisc show dev veth-bench; } > "$dir/tc-host-after.txt" 2>&1 || true
        { sudo -n ip netns exec "$NETNS" tc -s qdisc show dev veth-mcu; } > "$dir/tc-netns-after.txt" 2>&1 || true
    fi
    run cleanup
    run sleep "$SETTLE"
    if [ "$PLAN" -eq 0 ]; then
        if ! sudo -n tc qdisc show dev veth-bench | grep -q 'noqueue'; then
            log "ВНИМАНИЕ: после $name veth-bench не вернулся в noqueue — снимаю принудительно"
            cleanup
        fi
    fi
    [ "$PLAN" -eq 1 ] || log "$name готов: $dir/run/results.json"
done

# --- постпроверка: ферма не оставила сирот (приёмочный критерий итерации 5) ---
if [ "$PLAN" -eq 0 ]; then
    post_ok=0
    for _ in $(seq 1 30); do
        if [ "$(mcu_ffmpeg_now)" -eq 0 ] && [ "$(room_abonents)" = "0" ] && [ "$(mcu_zombies)" -eq 0 ]; then post_ok=1; break; fi
        sleep 3
    done
    printf '{"ffmpeg":%s,"abonents":%s,"children":%s,"zombies":%s,"clean":%s}\n' \
        "$(mcu_ffmpeg_now)" "$(room_abonents)" "$(mcu_children)" "$(mcu_zombies)" "$post_ok" > "$OUT/postcheck.json"
    if [ "$post_ok" -eq 1 ]; then
        log "постпроверка: ферма чиста (0 ffmpeg, 0 abonents, детей $(mcu_children), зомби $(mcu_zombies))"
    else
        log "ВНИМАНИЕ: после серии ffmpeg=$(mcu_ffmpeg_now), abonents=$(room_abonents), детей=$(mcu_children), зомби=$(mcu_zombies) — сироты/утечка фермы"
    fi
    if [ "$(mcu_zombies)" -gt 0 ]; then
        log "ВНИМАНИЕ: у MCU $(mcu_zombies) зомби-детей (<defunct>) — retire не делает wait()"
    fi
fi

# --- сводка ---------------------------------------------------------------
if [ "$PLAN" -eq 0 ]; then
    python3 - "$OUT" <<'PY'
import json, os, sys
out = sys.argv[1]
rows = []
for name in sorted(os.listdir(out)):
    path = os.path.join(out, name, "run", "results.json")
    if not os.path.exists(path):
        continue
    d = json.load(open(path))
    s = d["summary"]
    farm = (s.get("farm") or {}).get("max") or {}
    pre = {}
    pre_path = os.path.join(out, name, "precheck.json")
    if os.path.exists(pre_path):
        pre = json.load(open(pre_path))
    a, v = s["rx_audio"], s["rx_video"]
    rows.append((name, s["valid"], s["n_connected"], pre.get("ffmpeg", -1), s["c_device_pct_one_core"], s["ffmpeg_share"],
                 farm.get("groups", 0), farm.get("keyframe_requests", 0),
                 farm.get("dropped_frames", 0), farm.get("restarts", 0),
                 a["loss_pct_mean"], a["jitter_ms_mean"], a["mos_mean"], a["jb_late_total"],
                 v["loss_pct_mean"], v["jitter_ms_mean"], s["rx_kernel_drops_total"]))
with open(os.path.join(out, "t2_summary.md"), "w") as fh:
    fh.write("# T2: netem-матрица (N=10 SIP 720p30, 60 с)\n\n")
    fh.write("| профиль | valid | подкл. | остатки ffmpeg | c_device % | ffmpeg_share | farm групп | kf-req | dropped | рестарты | "
             "RX ауд. loss % | ауд. jitter мс | MOS | jb_late | RX вид. loss % | вид. jitter мс | kernel-drops |\n")
    fh.write("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n")
    for r in rows:
        fh.write("| %s | %s | %d | %d | %.2f | %.1f %% | %d | %d | %d | %d | %.3f | %.2f | %s | %d | %.3f | %.2f | %d |\n" % (
            r[0], "да" if r[1] else "НЕТ", r[2], r[3], r[4], r[5] * 100, r[6], r[7], r[8], r[9],
            r[10], r[11], r[12], r[13], r[14], r[15], r[16]))
    post = {}
    post_path = os.path.join(out, "postcheck.json")
    if os.path.exists(post_path):
        post = json.load(open(post_path))
    fh.write("\n**Постпроверка фермы:** ffmpeg после серии = %s, abonents = %s, прямых детей = %s, зомби = %s → %s\n" % (
        post.get("ffmpeg", "?"), post.get("abonents", "?"), post.get("children", "?"),
        post.get("zombies", "?"),
        "чисто" if post.get("clean") else "СИРОТЫ/УТЕЧКА"))
print("t2_summary.md ->", os.path.join(out, "t2_summary.md"))
PY
fi
log "готово: $OUT"
