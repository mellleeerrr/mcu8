#!/usr/bin/env bash
# Импорт рантайма PNM-Frontend (TrueConf MCU Web UI, PROSTOR-сборка) из архива.
#
# Использование:
#   VKS_PNM_ARCHIVE=<путь к zip> ./scripts/import-pnm-frontend.sh [--force] [--with-source] [--gzip]
#
# Что делает:
#   1. Проверяет sha256 исходного архива (якорь: ea0314f8…cbdfa).
#   2. Распаковывает архив. Архив повреждён (нет central directory) — тогда
#      используется потоковое восстановление по локальным заголовкам (python3).
#   3. Копирует `deployed/pnm` в `client/pnm-frontend/deployed/pnm` 1:1,
#      исключая только служебные бэкапы `*.bak*` (следы ручных правок) и
#      удаляя симлинки (политика безопасности: ServeDir следует симлинкам;
#      каждый удалённый симлинк логируется).
#   4. Пишет `client/pnm-frontend/MANIFEST.sha256` (по каждому файлу рантайма).
#   5. Опции:
#        --force       — перезаписать непустой deployed/ (иначе отказ);
#        --with-source — дополнительно распаковать `source/extracted_frontend/`
#                        (реконструкция исходников; по умолчанию НЕ распаковывается);
#        --gzip        — сгенерировать `.gz` рядом с текстовыми файлами
#                        (для tower-http precompressed_gzip; в архиве .gz нет).
#
# Идемпотентность: без --force повторный запуск в непустой deployed/ не выполняется.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="${VKS_PNM_ARCHIVE:-$ROOT/client/pnm-frontend/archive/PNM-Frontend-20261016.zip}"
EXPECTED_SHA256="ea0314f85951a9abd9dcce3c9c6dfa79943941d36c03c64dde60a6ead23cbdfa"
DEST_PARENT="$ROOT/client/pnm-frontend/deployed"
DEST="$DEST_PARENT/pnm"
MANIFEST="$ROOT/client/pnm-frontend/MANIFEST.sha256"

FORCE=0
WITH_SOURCE=0
WITH_GZIP=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        --with-source) WITH_SOURCE=1 ;;
        --gzip) WITH_GZIP=1 ;;
        *) echo "неизвестная опция: $arg" >&2; exit 2 ;;
    esac
done

log() { printf '[import-pnm] %s\n' "$*"; }

[ -f "$ARCHIVE" ] || { echo "архив не найден: $ARCHIVE" >&2; exit 1; }

# 1. sha256-якорь.
ACTUAL_SHA256="$(sha256sum "$ARCHIVE" | cut -d' ' -f1)"
if [ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]; then
    log "ВНИМАНИЕ: sha256 архива отличается от якоря."
    log "  ожидался: $EXPECTED_SHA256"
    log "  получен:  $ACTUAL_SHA256"
    log "  продолжаю (архив мог быть обновлён командой); проверьте NOTES.md."
fi

# 2. Идемпотентность.
if [ -d "$DEST" ] && [ -n "$(ls -A "$DEST" 2>/dev/null || true)" ] && [ "$FORCE" -ne 1 ]; then
    echo "deployed/ уже непустой: $DEST" >&2
    echo "повторный импорт: добавьте --force" >&2
    exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/pnm-import.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
log "распаковка: $ARCHIVE → $TMP"

if unzip -qq -o "$ARCHIVE" -d "$TMP" 2>/dev/null; then
    log "архив прочитан штатным unzip"
else
    log "архив повреждён (нет central directory) — потоковое восстановление по local headers"
    python3 - "$ARCHIVE" "$TMP" <<'PYEOF'
import os
import struct
import sys
import zlib

src, dst = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
count = skipped = 0
i = 0
seen = set()
while True:
    j = data.find(b"PK\x03\x04", i)
    if j < 0:
        break
    i = j + 4
    try:
        _, _, _, method, _, _, _, csize, _, nlen, elen = struct.unpack_from("<IHHHHHIIIHH", data, j)
    except struct.error:
        continue
    name = data[j + 30 : j + 30 + nlen].decode("utf-8", "replace")
    if name in seen:  # встречается второй раз (ложная сигнатура) — пропускаем
        continue
    start = j + 30 + nlen + elen
    raw = data[start : start + csize]
    out = os.path.join(dst, name)
    if name.endswith("/"):
        os.makedirs(out, exist_ok=True)
        seen.add(name)
        continue
    try:
        content = raw if method == 0 else zlib.decompress(raw, -15)
        if len(content) == 0 and csize:
            raise ValueError("пустой поток")
    except Exception as e:  # noqa: BLE001 - salvage-режим: пропускаем битую запись
        skipped += 1
        print(f"  пропущено (нечитаемо): {name} ({e})")
        seen.add(name)
        continue
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "wb") as f:
        f.write(content)
    seen.add(name)
    count += 1
print(f"  восстановлено файлов: {count}, пропущено: {skipped}")
PYEOF
fi

[ -d "$TMP/deployed/pnm" ] || { echo "в архиве нет deployed/pnm" >&2; exit 1; }

# 3. Копирование рантайма 1:1 (кроме *.bak*; симлинки вычищаются — политика NOTES.md).
log "копирование рантайма (1:1, исключение: *.bak*, симлинки удаляются)"
# Симлинки в рантайме — escape-риск: ServeDir следует по ссылке за пределы корня.
SYMLINK_COUNT="$(find "$TMP/deployed/pnm" -type l | wc -l)"
if [ "$SYMLINK_COUNT" -gt 0 ]; then
    while IFS= read -r link; do
        log "  симлинк удалён: ${link#"$TMP"/deployed/pnm/}"
    done < <(find "$TMP/deployed/pnm" -type l | sort)
    find "$TMP/deployed/pnm" -type l -delete
fi
mkdir -p "$DEST_PARENT"
rm -rf "$DEST" "$DEST_PARENT/deployed"
tar -C "$TMP/deployed" -cf - --exclude='*.bak' --exclude='*.bak.*' pnm | tar -C "$DEST_PARENT" -xf -

BAK_COUNT="$(find "$TMP/deployed/pnm" -name '*.bak*' | wc -l)"
FILE_COUNT="$(find "$DEST" -type f | wc -l)"
TOTAL_BYTES="$(du -sb "$DEST" | cut -f1)"
log "исключено .bak-файлов: $BAK_COUNT"
log "удалено симлинков: $SYMLINK_COUNT"
log "файлов в рантайме: $FILE_COUNT ($TOTAL_BYTES байт)"

# 4. MANIFEST.sha256.
( cd "$DEST" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) > "$MANIFEST"
log "манифест: $MANIFEST ($(wc -l < "$MANIFEST") строк)"

# 5. Опционально: source/.
if [ "$WITH_SOURCE" -eq 1 ]; then
    log "копирование source/extracted_frontend (--with-source)"
    mkdir -p "$ROOT/client/pnm-frontend/source"
    rm -rf "$ROOT/client/pnm-frontend/source/extracted_frontend"
    tar -C "$TMP" -cf - source/extracted_frontend | tar -C "$ROOT/client/pnm-frontend/source" -xf -
fi

# 6. Опционально: предсжатие .gz.
if [ "$WITH_GZIP" -eq 1 ]; then
    log "генерация .gz (precompressed_gzip)"
    find "$DEST" -type f \
        \( -name '*.js' -o -name '*.css' -o -name '*.html' -o -name '*.svg' \
           -o -name '*.json' -o -name '*.xml' -o -name '*.webmanifest' -o -name '*.txt' \) \
        -exec gzip -9 -k -f {} \;
    log "сгенерировано .gz-файлов: $(find "$DEST" -name '*.gz' | wc -l)"
fi

log "готово."
