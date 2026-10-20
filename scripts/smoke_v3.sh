#!/usr/bin/env bash
# smoke_v3.sh — end-to-end smoke check for the PNM-compatible /api/v3 surface.
#
# Покрывает вертикаль, на которой стоит UI TrueConf PNM 2.2.1.66:
#   login → конференция → сессия → адресная книга → аудиоканал →
#   раскладка/слот → (опционально) живой SIP/H.323 вызов → медиа-счётчики →
#   контролы абонента (mute/volume/kick) → clean stop.
#
# Использование:
#   ./scripts/smoke_v3.sh
#   BASE_URL=http://127.0.0.1:8080 ADMIN_LOGIN=admin ADMIN_PASSWORD=admin_password \
#   SMOKE_URI=h323:192.168.1.132 ./scripts/smoke_v3.sh   # + живой вызов
#
# Переменные:
#   BASE_URL            (default http://127.0.0.1:8080)
#   ADMIN_LOGIN         (default admin)
#   ADMIN_PASSWORD      (default admin_password)
#   SMOKE_URI           (default empty) — sip:/h323: адрес для живого вызова;
#                       без него проверка живого медиа пропускается.
#   SMOKE_TIMEOUT       (default 90) — секунд на установку соединения.
#   SMOKE_INSECURE      (default 0) — 1 = -k для curl (self-signed TLS).
#
# Требования: bash, curl, jq. Скрипт всегда убирает за собой созданные
# конференцию/сессию/адрес/канал/раскладку и печатает PASS/FAIL-сводку.
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
ADMIN_LOGIN="${ADMIN_LOGIN:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-admin_password}"
SMOKE_URI="${SMOKE_URI:-}"
SMOKE_TIMEOUT="${SMOKE_TIMEOUT:-90}"
CURL=(curl -sS --max-time 20)
if [ "${SMOKE_INSECURE:-0}" = "1" ]; then CURL+=(-k); fi

TOKEN=""
CONF_ID=""
SESSION_ID=""
ADDRESS_ID=""
CHANNEL_ID=""
LAYOUT_ID=""
CHECKS=0

log() { printf '%s\n' "$*"; }
ok() { CHECKS=$((CHECKS + 1)); log "  ok  $*"; }
fail() { log "FAIL: $*" >&2; exit 1; }

cleanup() {
    local status=$?
    if [ -n "$SESSION_ID" ] && [ -n "$TOKEN" ]; then
        "${CURL[@]}" -X DELETE "$BASE_URL/api/v3/sessions/$SESSION_ID" \
            -H "Authorization: Bearer $TOKEN" -o /dev/null || true
    fi
    if [ -n "$CONF_ID" ] && [ -n "$TOKEN" ]; then
        "${CURL[@]}" -X DELETE "$BASE_URL/api/v3/conferences/$CONF_ID" \
            -H "Authorization: Bearer $TOKEN" -o /dev/null || true
    fi
    if [ -n "$ADDRESS_ID" ] && [ -n "$TOKEN" ]; then
        "${CURL[@]}" -X DELETE "$BASE_URL/api/v3/call_strings/$ADDRESS_ID" \
            -H "Authorization: Bearer $TOKEN" -o /dev/null || true
    fi
    if [ -n "$CHANNEL_ID" ] && [ -n "$TOKEN" ]; then
        "${CURL[@]}" -X DELETE "$BASE_URL/api/v3/audiochannels/$CHANNEL_ID" \
            -H "Authorization: Bearer $TOKEN" -o /dev/null || true
    fi
    if [ -n "$LAYOUT_ID" ] && [ -n "$TOKEN" ]; then
        "${CURL[@]}" -X DELETE "$BASE_URL/api/v3/video_layouts/$LAYOUT_ID" \
            -H "Authorization: Bearer $TOKEN" -o /dev/null || true
    fi
    if [ "$status" -ne 0 ]; then
        log "FAILED (cleanup выполнен, созданные сущности удалены)"
    else
        log "PASS: $CHECKS проверок"
    fi
}
trap cleanup EXIT

# req METHOD PATH [JSON] -> body; ставит HTTP_STATUS
req() {
    local method=$1 path=$2 body=${3:-} out
    if [ -n "$body" ]; then
        out=$("${CURL[@]}" -X "$method" "$BASE_URL$path" \
            -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
            -d "$body" -w $'\n%{http_code}')
    else
        out=$("${CURL[@]}" -X "$method" "$BASE_URL$path" \
            -H "Authorization: Bearer $TOKEN" -w $'\n%{http_code}')
    fi
    HTTP_STATUS="${out##*$'\n'}"
    HTTP_BODY="${out%$'\n'*}"
    if [ "$HTTP_STATUS" -ge 400 ]; then
        fail "$method $path -> $HTTP_STATUS: $HTTP_BODY"
    fi
}

log "== smoke_v3: $BASE_URL"
req GET /healthz
ok "healthz"

req POST /api/v3/login "{\"login\":\"$ADMIN_LOGIN\",\"password\":\"$ADMIN_PASSWORD\"}"
TOKEN=$(jq -r '.token' <<<"$HTTP_BODY")
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || fail "login did not return a token"
ok "login ($ADMIN_LOGIN)"

# --- конференция и сессия -------------------------------------------------
req POST /api/v3/conferences "{\"display_name\":\"smoke_v3 $(date +%s)\"}"
CONF_ID=$(jq -r '.conference.id' <<<"$HTTP_BODY")
[ -n "$CONF_ID" ] && [ "$CONF_ID" != "null" ] || fail "create conference: $HTTP_BODY"
ok "conference created ($CONF_ID)"

req POST /api/v3/sessions "{\"conference_id\":\"$CONF_ID\"}"
SESSION_ID=$(jq -r '.session.id' <<<"$HTTP_BODY")
[ -n "$SESSION_ID" ] && [ "$SESSION_ID" != "null" ] || fail "start session: $HTTP_BODY"
ok "session started ($SESSION_ID)"

req GET "/api/v3/ui/sessions?page=1&page_size=100"
jq -e --arg id "$SESSION_ID" '.sessions | any(.id == $id or ._id == $id or ._key == $id)' <<<"$HTTP_BODY" >/dev/null \
    && ok "session visible in ui/sessions" || fail "session not listed in ui/sessions: $HTTP_BODY"

# --- адресная книга ------------------------------------------------------
ADDRESS_URI="sip:10.99.$((RANDOM % 250 + 1)).$((RANDOM % 250 + 1))"
req POST /api/v3/call_strings "{\"uri\":\"$ADDRESS_URI\",\"display_name\":\"smoke address\"}"
ADDRESS_ID=$(jq -r '.call_string.id' <<<"$HTTP_BODY")
[ -n "$ADDRESS_ID" ] && [ "$ADDRESS_ID" != "null" ] || fail "create address: $HTTP_BODY"
ok "address created ($ADDRESS_ID)"

req GET "/api/v3/ui/call_strings?page=1&page_size=100&uri=${ADDRESS_URI%%:*}"
jq -e --arg id "$ADDRESS_ID" '.call_strings | any(.id == $id)' <<<"$HTTP_BODY" >/dev/null \
    && ok "address searchable by protocol filter" || fail "address not found by uri filter: $HTTP_BODY"

req PATCH "/api/v3/call_strings/$ADDRESS_ID" '{"display_name":"smoke address patched"}'
[ "$(jq -r '.call_string.display_name' <<<"$HTTP_BODY")" = "smoke address patched" ] \
    && ok "address patched" || fail "address patch did not apply: $HTTP_BODY"
req DELETE "/api/v3/call_strings/$ADDRESS_ID"
ADDRESS_ID=""
ok "address deleted"

# --- аудиоканал ----------------------------------------------------------
req POST /api/v3/audiochannels '{"display_name":"smoke channel","payload":{}}'
CHANNEL_ID=$(jq -r '.audiochannel.id' <<<"$HTTP_BODY")
[ -n "$CHANNEL_ID" ] && [ "$CHANNEL_ID" != "null" ] || fail "create audiochannel: $HTTP_BODY"
ok "audiochannel created ($CHANNEL_ID)"

req POST "/api/v3/sessions/$SESSION_ID/audiochannels/$CHANNEL_ID"
req GET "/api/v3/ui/sessions/$SESSION_ID/audiochannels"
jq -e --arg id "$CHANNEL_ID" '.audiochannels | any(.id == $id)' <<<"$HTTP_BODY" >/dev/null \
    && ok "audiochannel attached and visible" || fail "audiochannel not visible after attach: $HTTP_BODY"
req DELETE "/api/v3/sessions/$SESSION_ID/audiochannels/$CHANNEL_ID"
ok "audiochannel detached"

# --- раскладка и слот ----------------------------------------------------
req POST /api/v3/ui/video_layouts '{"display_name":"smoke layout","payload":{"mode":"auto","type":"default"}}'
LAYOUT_ID=$(jq -r '.video_layout.id' <<<"$HTTP_BODY")
[ -n "$LAYOUT_ID" ] && [ "$LAYOUT_ID" != "null" ] || fail "create layout: $HTTP_BODY"
ok "layout created ($LAYOUT_ID)"

req POST "/api/v3/sessions/$SESSION_ID/video_layouts/$LAYOUT_ID"
req GET "/api/v3/sessions/$SESSION_ID/video_layouts/$LAYOUT_ID/abonents"
ok "layout attached ($(jq -r '.count' <<<"$HTTP_BODY") abonents)"
req DELETE "/api/v3/sessions/$SESSION_ID/video_layouts/$LAYOUT_ID"
ok "layout detached"

# --- опциональный живой вызов: медиа и контролы --------------------------
if [ -n "$SMOKE_URI" ]; then
    log "== live call: $SMOKE_URI (timeout ${SMOKE_TIMEOUT}s)"
    req POST "/api/v3/ui/sessions/$SESSION_ID/call_strings" "{\"uri\":\"$SMOKE_URI\"}"
    pid=""
    for _ in $(seq 1 "$SMOKE_TIMEOUT"); do
        sleep 1
        req GET "/api/v3/sessions/$SESSION_ID/abonents"
        pid=$(jq -r '.connected[0].id // empty' <<<"$HTTP_BODY")
        [ -n "$pid" ] && break
    done
    [ -n "$pid" ] || fail "живой вызов $SMOKE_URI не соединился за ${SMOKE_TIMEOUT}s"
    ok "call connected ($pid)"

    req GET "/api/v3/sessions/$SESSION_ID/abonents"
    rx1=$(jq -r --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.rx.packets' <<<"$HTTP_BODY")
    tx1=$(jq -r --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.tx.packets' <<<"$HTTP_BODY")
    rx2=$rx1; tx2=$tx1
    # Медиа-счётчики растут не мгновенно: терминал начинает отдавать RTP
    # после установления всех каналов, поэтому ждём до 20 секунд.
    for _ in $(seq 1 20); do
        sleep 1
        req GET "/api/v3/sessions/$SESSION_ID/abonents"
        rx2=$(jq -r --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.rx.packets' <<<"$HTTP_BODY")
        tx2=$(jq -r --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.tx.packets' <<<"$HTTP_BODY")
        if [ "${rx2:-0}" -gt "${rx1:-0}" ] && [ "${tx2:-0}" -gt "${tx1:-0}" ]; then break; fi
    done
    [ "${rx2:-0}" -gt "${rx1:-0}" ] && [ "${tx2:-0}" -gt "${tx1:-0}" ] \
        && ok "RTP audio counters grow (rx $rx1->$rx2, tx $tx1->$tx2)" \
        || fail "RTP audio counters do not grow (rx $rx1->$rx2, tx $tx1->$tx2)"

    req POST "/api/v3/abonents/$pid/audio" '{"enable":false}'
    ok "microphone muted via control API"
    req POST "/api/v3/abonents/$pid/audio" '{"enable":true}'
    req POST "/api/v3/abonents/$pid/video" '{"enable":false}'
    ok "camera toggled via control API"
    req POST "/api/v3/abonents/$pid/video" '{"enable":true}'
    req POST "/api/v3/abonents/$pid/volume" '{"volume":-60,"outgoing":false}'
    ok "volume control accepted"

    req DELETE "/api/v3/sessions/$SESSION_ID/abonents/$pid"
    ok "abonent kicked from session"
else
    log "== live call skipped (SMOKE_URI is empty)"
fi

# --- остановка -----------------------------------------------------------
req DELETE "/api/v3/sessions/$SESSION_ID"
SESSION_ID=""
ok "session stopped"
req DELETE "/api/v3/conferences/$CONF_ID"
CONF_ID=""
ok "conference deleted"
req DELETE "/api/v3/audiochannels/$CHANNEL_ID"
CHANNEL_ID=""
req DELETE "/api/v3/video_layouts/$LAYOUT_ID"
LAYOUT_ID=""
ok "cleanup complete"

trap - EXIT
log "PASS: $CHECKS проверок"
