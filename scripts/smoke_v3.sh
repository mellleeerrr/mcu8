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
CREATED=0

log() { printf '%s\n' "$*"; }
ok() { CHECKS=$((CHECKS + 1)); log "  ok  $*"; }
fail() { log "FAIL: $*" >&2; exit 1; }

cleanup() {
    local status=$?
    # shellcheck disable=SC2317  # вызывается через trap; переменные пусты, если шаг не дошёл
    del() {
        local path=$1
        [ -n "$TOKEN" ] || return 0
        "${CURL[@]}" -X DELETE "$BASE_URL$path" -H "Authorization: Bearer $TOKEN" -o /dev/null || true
    }
    if [ -n "$SESSION_ID" ]; then del "/api/v3/sessions/$SESSION_ID"; fi
    if [ -n "$CONF_ID" ]; then del "/api/v3/conferences/$CONF_ID"; fi
    if [ -n "$ADDRESS_ID" ]; then del "/api/v3/call_strings/$ADDRESS_ID"; fi
    if [ -n "$CHANNEL_ID" ]; then del "/api/v3/audiochannels/$CHANNEL_ID"; fi
    if [ -n "$LAYOUT_ID" ]; then del "/api/v3/video_layouts/$LAYOUT_ID"; fi
    if [ "$status" -ne 0 ]; then
        if [ "$CREATED" -eq 1 ]; then
            log "FAILED (cleanup выполнен, созданные сущности удалены)"
        else
            log "FAILED (cleanup выполнен, создавать было нечего)"
        fi
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

# http_status METHOD PATH -> код ответа (для проверки 404 после удаления)
http_status() {
    "${CURL[@]}" -o /dev/null -w '%{http_code}' -X "$1" "$BASE_URL$2" -H "Authorization: Bearer $TOKEN"
}

log "== smoke_v3: $BASE_URL"
req GET /healthz
ok "healthz"

LOGIN_BODY=$(jq -n --arg login "$ADMIN_LOGIN" --arg password "$ADMIN_PASSWORD" '{login: $login, password: $password}')
req POST /api/v3/login "$LOGIN_BODY"
TOKEN=$(jq -r '.token' <<<"$HTTP_BODY")
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || fail "login did not return a token"
ok "login ($ADMIN_LOGIN)"

# --- конференция и сессия -------------------------------------------------
req POST /api/v3/conferences "{\"display_name\":\"smoke_v3 $(date +%s)\"}"
CONF_ID=$(jq -r '.conference.id' <<<"$HTTP_BODY")
[ -n "$CONF_ID" ] && [ "$CONF_ID" != "null" ] || fail "create conference: $HTTP_BODY"
CREATED=1
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
    && ok "address searchable by uri substring" || fail "address not found by uri filter: $HTTP_BODY"

req GET "/api/v3/ui/call_strings?page=1&page_size=100&protocols=sip"
jq -e --arg id "$ADDRESS_ID" '
    ([.call_strings[] | select(.id == $id)] | length == 1)
    and all(.call_strings[]; (.uri | startswith("sip:")))
' <<<"$HTTP_BODY" >/dev/null \
    && ok "address included by protocols=sip filter" || fail "protocols=sip filter is wrong: $HTTP_BODY"

req GET "/api/v3/ui/call_strings?page=1&page_size=100&protocols=h323"
jq -e --arg id "$ADDRESS_ID" 'all(.call_strings[]; .id != $id)' <<<"$HTTP_BODY" >/dev/null \
    && ok "address excluded by protocols=h323 filter" || fail "protocols=h323 must not contain sip address: $HTTP_BODY"

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
req GET "/api/v3/ui/sessions/$SESSION_ID/audiochannels"
jq -e --arg id "$CHANNEL_ID" 'all(.audiochannels[]; .id != $id)' <<<"$HTTP_BODY" >/dev/null \
    && ok "audiochannel detached" || fail "audiochannel still attached: $HTTP_BODY"

# --- раскладка и слот ----------------------------------------------------
req POST /api/v3/ui/video_layouts '{"display_name":"smoke layout","payload":{"mode":"auto","type":"default"}}'
LAYOUT_ID=$(jq -r '.video_layout.id' <<<"$HTTP_BODY")
[ -n "$LAYOUT_ID" ] && [ "$LAYOUT_ID" != "null" ] || fail "create layout: $HTTP_BODY"
ok "layout created ($LAYOUT_ID)"

req POST "/api/v3/sessions/$SESSION_ID/video_layouts/$LAYOUT_ID"
req GET "/api/v3/sessions/$SESSION_ID/video_layouts"
jq -e --arg id "$LAYOUT_ID" '.video_layouts | any(.id == $id)' <<<"$HTTP_BODY" >/dev/null \
    && ok "layout visible in session layouts" || fail "layout not attached to session: $HTTP_BODY"
req GET "/api/v3/sessions/$SESSION_ID/video_layouts/$LAYOUT_ID/abonents"
jq -e '.count == 0 and (.abonents | length == 0)' <<<"$HTTP_BODY" >/dev/null \
    && ok "layout attached, slots empty until live call" || fail "layout has unexpected abonents: $HTTP_BODY"
req DELETE "/api/v3/sessions/$SESSION_ID/video_layouts/$LAYOUT_ID"
req GET "/api/v3/sessions/$SESSION_ID/video_layouts"
jq -e --arg id "$LAYOUT_ID" 'all(.video_layouts[]; .id != $id)' <<<"$HTTP_BODY" >/dev/null \
    && ok "layout detached from session" || fail "layout still attached after detach: $HTTP_BODY"

# --- опциональный живой вызов: медиа и контролы --------------------------
if [ -n "$SMOKE_URI" ]; then
    log "== live call: $SMOKE_URI (timeout ${SMOKE_TIMEOUT}s)"
    CALL_BODY=$(jq -n --arg uri "$SMOKE_URI" '{uri: $uri}')
    req POST "/api/v3/ui/sessions/$SESSION_ID/call_strings" "$CALL_BODY"
    pid=""
    for ((attempt = 0; attempt < SMOKE_TIMEOUT; attempt++)); do
        sleep 1
        req GET "/api/v3/sessions/$SESSION_ID/abonents"
        pid=$(jq -r '.connected[0].id // empty' <<<"$HTTP_BODY")
        [ -n "$pid" ] && break
        reason=$(jq -r '.disconnected[0].failure_reason // .disconnected[0].state // empty' <<<"$HTTP_BODY")
        [ -z "$reason" ] || fail "живой вызов $SMOKE_URI отклонён: $reason"
    done
    [ -n "$pid" ] || fail "живой вызов $SMOKE_URI не соединился за ${SMOKE_TIMEOUT}s"
    ok "call connected ($pid)"

    req GET "/api/v3/sessions/$SESSION_ID/abonents"
    rx1=$(jq -r --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.rx.packets' <<<"$HTTP_BODY")
    tx1=$(jq -r --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.tx.packets' <<<"$HTTP_BODY")
    rx2=$rx1; tx2=$tx1
    # Медиа-счётчики растут не мгновенно: терминал начинает отдавать RTP
    # после установления всех каналов, поэтому ждём до 20 секунд.
    for ((attempt = 0; attempt < 20; attempt++)); do
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
    req GET "/api/v3/sessions/$SESSION_ID/abonents"
    jq -e --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.tx.enabled == false' <<<"$HTTP_BODY" >/dev/null \
        && ok "microphone muted (audio.tx.enabled=false)" || fail "microphone mute did not apply: $HTTP_BODY"
    req POST "/api/v3/abonents/$pid/audio" '{"enable":true}'
    req POST "/api/v3/abonents/$pid/video" '{"enable":false}'
    req GET "/api/v3/sessions/$SESSION_ID/abonents"
    jq -e --arg p "$pid" '.connected[] | select(.id == $p) | .details.video.tx.enabled == false' <<<"$HTTP_BODY" >/dev/null \
        && ok "camera off (video.tx.enabled=false)" || fail "camera toggle did not apply: $HTTP_BODY"
    req POST "/api/v3/abonents/$pid/video" '{"enable":true}'
    req POST "/api/v3/abonents/$pid/volume" '{"volume":-60,"outgoing":false}'
    req GET "/api/v3/sessions/$SESSION_ID/abonents"
    jq -e --arg p "$pid" '.connected[] | select(.id == $p) | .details.audio.rx.volume == -60' <<<"$HTTP_BODY" >/dev/null \
        && ok "volume control applied (audio.rx.volume=-60)" || fail "volume control did not apply: $HTTP_BODY"

    req DELETE "/api/v3/sessions/$SESSION_ID/abonents/$pid"
    req GET "/api/v3/sessions/$SESSION_ID/abonents"
    jq -e --arg p "$pid" 'all(.connected[]; .id != $p)' <<<"$HTTP_BODY" >/dev/null \
        && ok "abonent kicked from session" || fail "abonent still connected after kick: $HTTP_BODY"
else
    log "== live call skipped (SMOKE_URI is empty)"
fi

# --- остановка -----------------------------------------------------------
STOPPED_SESSION=$SESSION_ID
req DELETE "/api/v3/sessions/$SESSION_ID"
req GET "/api/v3/ui/sessions?page=1&page_size=200"
jq -e --arg id "$STOPPED_SESSION" '.sessions | any(.id == $id and .state == "STOPPED" and .has_stopped == true)' <<<"$HTTP_BODY" >/dev/null \
    && ok "session stopped (state STOPPED)" || fail "session is not STOPPED after stop: $HTTP_BODY"
SESSION_ID=""
DELETED_CONF=$CONF_ID
req DELETE "/api/v3/conferences/$CONF_ID"
[ "$(http_status GET "/api/v3/conferences/$DELETED_CONF")" = "404" ] \
    && ok "conference deleted (GET 404)" || fail "conference still readable after delete"
CONF_ID=""
req DELETE "/api/v3/audiochannels/$CHANNEL_ID"
CHANNEL_ID=""
req DELETE "/api/v3/video_layouts/$LAYOUT_ID"
LAYOUT_ID=""
ok "cleanup complete"

trap - EXIT
log "PASS: $CHECKS проверок"
