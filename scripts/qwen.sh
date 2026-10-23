#!/usr/bin/env bash
# Мост к локальной модели qwen3.8-27b-uncensored-code (OpenAI-compatible API).
#
# Использование:
#   export QWEN_BASE_URL=http://127.0.0.1:11434/v1   # или LM Studio/Ollama/vLLM
#   export QWEN_MODEL=qwen3.8-27b-uncensored-code
#   export QWEN_API_KEY=...                          # если сервер требует ключ
#   ./scripts/qwen.sh "Сколько окон в vks_grid_g8_5?" [--system "..."] [--raw]
#
# Выход: текст ответа в stdout; ошибки — в stderr, код 2 (нет конфигурации),
# 3 (сеть/HTTP), 4 (некорректный ответ). Таймаут — QWEN_TIMEOUT (по умолчанию 120 с).
# Секреты и медиа в промпт не отправлять: модель локальная, но канал может быть общим.
set -euo pipefail

BASE_URL="${QWEN_BASE_URL:-}"
MODEL="${QWEN_MODEL:-qwen3.8-27b-uncensored-code}"
KEY="${QWEN_API_KEY:-}"
TIMEOUT="${QWEN_TIMEOUT:-120}"

if [ -z "$BASE_URL" ]; then
  echo "qwen: не задан QWEN_BASE_URL (OpenAI-compatible, например http://127.0.0.1:11434/v1)" >&2
  echo "qwen: см. docs/implementation/QWEN_INTEGRATION.md; без endpoint совместная работа невозможна" >&2
  exit 2
fi

prompt=""
system=""
raw=0
while [ $# -gt 0 ]; do
  case "$1" in
    --system) system="${2:?--system требует значение}"; shift 2 ;;
    --raw) raw=1; shift ;;
    *) prompt="${prompt:+$prompt }$1"; shift ;;
  esac
done
if [ -z "$prompt" ]; then
  echo "qwen: пустой промпт" >&2
  exit 2
fi

payload="$(PROMPT="$prompt" SYSTEM="$system" MODEL="$MODEL" python3 - <<'PY'
import json, os
messages = []
if os.environ.get("SYSTEM"):
    messages.append({"role": "system", "content": os.environ["SYSTEM"]})
messages.append({"role": "user", "content": os.environ["PROMPT"]})
print(json.dumps({"model": os.environ["MODEL"], "messages": messages, "temperature": 0.2}, ensure_ascii=False))
PY
)"

auth=()
if [ -n "$KEY" ]; then auth=(-H "Authorization: Bearer $KEY"); fi

response="$(curl -sS --max-time "$TIMEOUT" \
  -H 'Content-Type: application/json' "${auth[@]}" \
  -d "$payload" "${BASE_URL%/}/chat/completions" 2>&1)" || {
  echo "qwen: ошибка запроса к ${BASE_URL%/}/chat/completions" >&2
  echo "$response" >&2
  exit 3
}

if [ "$raw" -eq 1 ]; then
  printf '%s\n' "$response"
  exit 0
fi

RESPONSE="$response" python3 - <<'PY' || exit 4
import json, os, sys
try:
    data = json.loads(os.environ["RESPONSE"])
except json.JSONDecodeError:
    print("qwen: ответ не JSON: " + os.environ["RESPONSE"][:400], file=sys.stderr)
    sys.exit(4)
choice = (data.get("choices") or [{}])[0]
message = choice.get("message") or {}
content = message.get("content")
if not content:
    print("qwen: в ответе нет choices[0].message.content: " + json.dumps(data, ensure_ascii=False)[:400], file=sys.stderr)
    sys.exit(4)
print(content)
PY
