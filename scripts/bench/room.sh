#!/usr/bin/env bash
# room.sh — комната для прогона: login -> найти/создать конференцию -> UUID.
#
# Наш SIP UAS принимает ТОЛЬКО существующую комнату по UUID:
#   gateways.rs:2348-2360 — Uuid::parse_str(user), иначе 404; и комната должна
#   быть в state.rooms, иначе тоже 404. Поэтому «сначала комната, потом боты».
# H.323 UAS умеет резолвить по UUID/route/alias/имени (t07d SUMMARY:27-31),
# поэтому route = имя комнаты — запасной ключ для H.323-бота.
#
# Использование:
#   scripts/bench/room.sh ensure [--name bench-<ts>]   # -> UUID в stdout (последняя строка)
#   scripts/bench/room.sh list
#   scripts/bench/room.sh delete <uuid>
#
# Переменные: VKS_API (http://127.0.0.1:8080), VKS_LOGIN (admin), VKS_PASSWORD (admin_password)
#   (dev-креды из configs/default.toml:32-33; на production-конфиге скрипт не сработает — так и должно быть)
set -euo pipefail
export LC_ALL=C

API="${VKS_API:-http://127.0.0.1:8080}"
LOGIN="${VKS_LOGIN:-admin}"
PASSWORD="${VKS_PASSWORD:-admin_password}"
CMD="${1:-ensure}"; shift || true

log() { printf '[room] %s\n' "$*" >&2; }
die() { printf '[room] FATAL: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null || die "нет curl"
curl -fsS "$API/healthz" >/dev/null 2>&1 || die "сервер не отвечает на $API/healthz"

TOKEN="$(
  curl -fsS -X POST "$API/api/v3/login" -H 'Content-Type: application/json' \
    -d "{\"login\":\"$LOGIN\",\"password\":\"$PASSWORD\"}" 2>/dev/null \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("token") or d.get("access_token") or d.get("jwt") or "")' 2>/dev/null || true
)"
[[ -n "$TOKEN" ]] || die "login не удался (проверь креды/allow_insecure_defaults); ответ выше"

auth=(-H "Authorization: Bearer $TOKEN")

case "$CMD" in
  ensure)
    NAME="bench-$(date +%Y%m%dT%H%M%S)"
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --name) NAME="$2"; shift 2 ;;
        *) die "unknown arg: $1" ;;
      esac
    done
    # Уже есть комната с таким именем? (409 при создании — берём существующую.)
    EXIST="$(curl -fsS "${auth[@]}" "$API/api/v3/conferences" 2>/dev/null \
      | python3 -c '
import json,sys
name=sys.argv[1]
d=json.load(sys.stdin)
items=d if isinstance(d,list) else (d.get("items") or d.get("data") or d.get("conferences") or [])
for it in items:
    if (it.get("display_name") or it.get("name")) == name:
        print(it.get("id") or it.get("uuid") or "")
        break
' "$NAME" 2>/dev/null || true)"
    if [[ -n "$EXIST" ]]; then
      log "комната уже есть: $NAME -> $EXIST"
      printf '%s\n' "$EXIST"
      exit 0
    fi
    RESP="$(curl -sS -X POST "${auth[@]}" -H 'Content-Type: application/json' \
      -d "{\"display_name\":\"$NAME\",\"route\":\"$NAME\"}" "$API/api/v3/conferences")" \
      || die "POST /api/v3/conferences не удался"
    UUID="$(printf '%s' "$RESP" | python3 -c '
import json,sys
d=json.load(sys.stdin)
print(d.get("id") or (d.get("conference") or {}).get("id") or (d.get("data") or {}).get("id") or "")
' 2>/dev/null || true)"
    if [[ -z "$UUID" ]]; then
      log "ответ сервера: $RESP"
      die "не смог извлечь UUID комнаты (формат ответа не совпал с ожиданием — проверь api_v3::sessions::create_conference)"
    fi
    log "создана комната $NAME -> $UUID (route=$NAME)"
    printf '%s\n' "$UUID"
    ;;
  list)
    curl -fsS "${auth[@]}" "$API/api/v3/conferences" | python3 -m json.tool 2>/dev/null || curl -fsS "${auth[@]}" "$API/api/v3/conferences"
    ;;
  delete)
    UUID="${1:?uuid}"; shift
    curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE "${auth[@]}" "$API/api/v3/conferences/$UUID"
    ;;
  *) die "usage: $0 {ensure [--name N] | list | delete <uuid>}" ;;
esac
