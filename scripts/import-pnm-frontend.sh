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
#   3.5 Восстанавливает ПРОЕКТНЫЙ слой (perf-overrides.js, api-errors.js,
#      codian-layouts.js и блок CSS-override в index.html — их нет в
#      вендорском архиве, а копирование 1:1 их бы уничтожило) и применяет
#      вендорские патчи (scripts/patch-pnm-*.py) — без них фичи T11b/T15
#      молча откатываются к вендорскому виду.
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
# Сначала сохраняем ПРОЕКТНЫЙ слой: perf-overrides.js, api-errors.js,
# codian-layouts.js и блок CSS-override из index.html. Их нет в вендорском
# архиве, а копирование 1:1 (rm -rf + распаковка) уничтожило бы их — вместе
# с правками T1.1/T12b/T13/T14/T15, которые в них живут.
log "сохранение проектного слоя (perf-overrides.js, api-errors.js, codian-layouts.js, theme-toggle.js, deeplink.js, CSS-override)"
PROJECT_LAYER="$TMP/project-layer"
mkdir -p "$PROJECT_LAYER"
for f in perf-overrides.js api-errors.js codian-layouts.js theme-toggle.js deeplink.js; do
    if [ -f "$DEST/$f" ]; then
        cp -p "$DEST/$f" "$PROJECT_LAYER/$f"
        log "  сохранён: $f"
    else
        log "  нет в текущем рантайме (пропуск): $f"
    fi
done
if [ -f "$DEST/index.html" ]; then
    python3 - "$DEST/index.html" "$PROJECT_LAYER" <<'PYEOF'
import re, sys, pathlib

src, layer = sys.argv[1], pathlib.Path(sys.argv[2])
text = open(src, encoding="utf-8").read()
m = re.search(r'<style id="prostor-pnm-overrides">.*?</style>', text, re.S)
if m:
    (layer / "index-overrides.html").write_text(m.group(0), encoding="utf-8")
    print("  сохранён: index-overrides.html (%d символов)" % len(m.group(0)))
else:
    print("  в index.html нет блока prostor-pnm-overrides (пропуск)")

# P1-1: проектные <script src> — не часть вендорского архива, копирование 1:1
# их теряло: файлы восстанавливались, а ссылок на них в index.html не было,
# и правки T12b/T15 молча откатывались. Сохраняем блок с маркерами целиком:
# повторный импорт восстанавливает его байт-в-байт (без эвристик вставки).
block = re.search(r'<!-- prostor:project-layer:start -->.*?<!-- prostor:project-layer:end -->', text, re.S)
if not block:
    PROJECT_JS = ("api-errors.js", "perf-overrides.js", "codian-layouts.js", "theme-toggle.js", "deeplink.js")
    tags = [
        tag
        for tag in re.findall(
            r'<script[^>]*src="[^"]*(?:%s)"[^>]*></script>' % "|".join(map(re.escape, PROJECT_JS)), text
        )
    ]
    block_text = (
        "<!-- prostor:project-layer:start -->\n"
        + "\n".join(tags)
        + "\n<!-- prostor:project-layer:end -->"
        if tags
        else ""
    )
else:
    block_text = block.group(0)
if block_text:
    (layer / "index-scripts.html").write_text(block_text, encoding="utf-8")
    print("  сохранён: index-scripts.html (%d символов)" % len(block_text))
else:
    print("  в index.html нет проектных <script src> (пропуск)")
PYEOF
fi

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
# P1-5: распаковка в staging и подмена целиком. Прежний вариант (`rm -rf` +
# tar поверх) оставлял НАПОЛОВИНУ импортированное дерево, если tar падал на
# середине: рантайм без части чанков и без манифеста.
STAGE="$DEST_PARENT/.pnm-stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
tar -C "$TMP/deployed" -cf - --exclude='*.bak' --exclude='*.bak.*' pnm | tar -C "$STAGE" -xf -
if [ ! -f "$STAGE/pnm/index.html" ]; then
    echo "распаковка неполная: нет $STAGE/pnm/index.html; рантайм не тронут" >&2
    exit 1
fi
rm -rf "$DEST"
mv "$STAGE/pnm" "$DEST"
rmdir "$STAGE" 2>/dev/null || true

BAK_COUNT="$(find "$TMP/deployed/pnm" -name '*.bak*' | wc -l)"
FILE_COUNT="$(find "$DEST" -type f | wc -l)"
TOTAL_BYTES="$(du -sb "$DEST" | cut -f1)"
log "исключено .bak-файлов: $BAK_COUNT"
log "удалено симлинков: $SYMLINK_COUNT"
log "файлов в рантайме: $FILE_COUNT ($TOTAL_BYTES байт)"

# 3.5. Проектный слой + вендорские патчи.
# Порядок важен: сначала проектный слой (он правит index.html и добавляет
# собственные скрипты), затем патчи вендорских чанков, затем — манифест
# (шаг 4), который считается уже по итоговым байтам.
log "восстановление проектного слоя"
for f in perf-overrides.js api-errors.js codian-layouts.js theme-toggle.js deeplink.js; do
    if [ -f "$PROJECT_LAYER/$f" ]; then
        cp -p "$PROJECT_LAYER/$f" "$DEST/$f"
        log "  восстановлен: $f"
    fi
done
if [ -f "$PROJECT_LAYER/index-scripts.html" ]; then
    python3 - "$DEST/index.html" "$PROJECT_LAYER/index-scripts.html" <<'PYEOF'
import re, sys
target, block_path = sys.argv[1], sys.argv[2]
text = open(target, encoding="utf-8").read()
block = open(block_path, encoding="utf-8").read()
marked = re.compile(r'<!-- prostor:project-layer:start -->.*?<!-- prostor:project-layer:end -->', re.S)
if marked.search(text):
    text = marked.sub(lambda _: block, text, count=1)
    action = "заменён"
else:
    # Вставляем перед первым вендорским module-скриптом — проектный слой
    # обязан грузиться ДО бандла приложения (перехват console/fetch).
    anchor = re.search(r'<script[^>]*type="module"[^>]*src="[^"]*index\.bundle', text)
    if anchor:
        text = text[: anchor.start()] + block + "\n" + text[anchor.start() :]
    else:
        text = text.replace("</body>", block + "\n</body>", 1)
    action = "вставлен"
open(target, "w", encoding="utf-8").write(text)
print("  проектный слой <script> %s в index.html" % action)
PYEOF
fi
if [ -f "$PROJECT_LAYER/index-overrides.html" ]; then
    python3 - "$DEST/index.html" "$PROJECT_LAYER/index-overrides.html" <<'PYEOF'
import re, sys
target, block_path = sys.argv[1], sys.argv[2]
text = open(target, encoding="utf-8").read()
block = open(block_path, encoding="utf-8").read()
pattern = re.compile(r'<style id="prostor-pnm-overrides">.*?</style>', re.S)
if pattern.search(text):
    text = pattern.sub(lambda m: block, text, count=1)
    action = "заменён"
else:
    text = text.replace("</head>", block + "\n</head>", 1)
    action = "вставлен"
open(target, "w", encoding="utf-8").write(text)
print("  CSS-override %s в index.html" % action)
PYEOF
fi

log "применение вендорских патчей"
for patch in "$ROOT/scripts/patch-pnm-chartjs.py" "$ROOT/scripts/patch-pnm-audio-in-layouts.py" "$ROOT/scripts/patch-pnm-layout-crash.py"; do
    [ -f "$patch" ] || { echo "нет патч-скрипта: $patch" >&2; exit 1; }
    log "  патч: $(basename "$patch")"
    if ! python3 "$patch" --root "$ROOT"; then
        echo "ПАТЧ НЕ ПРИМЕНЁН: $(basename "$patch")" >&2
        echo "  причина (подробности выше): вендор изменился — правьте патч-скрипт;" >&2
        echo "  ИЛИ отсутствует проектный слой (CSS-override в index.html) — тогда" >&2
        echo "  восстановите его из NOTES.md, раздел T15." >&2
        exit 1
    fi
done

# 3.55. Тема: вендорский index.html/manifest несут старые цвета (#0097A7),
# а токен --c-bg есть только в тёмной теме. Проектный слой задаёт оба токена;
# здесь детерминированно правим статический meta и manifest (идемпотентно),
# иначе повторный импорт откатывал бы T07.
log "применение инварианта темы (meta theme-color, manifest)"
python3 - "$DEST/index.html" "$DEST/assets/manifest.webmanifest" <<'PYEOF'
import re, sys, pathlib
index_path, manifest_path = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
text = index_path.read_text(encoding="utf-8")
dark = re.search(r"html\.theme-dark\s*\{[^}]*--c-bg:\s*(#[0-9a-fA-F]{6})", text, re.S)
if not dark:
    print("не найден html.theme-dark --c-bg: тема не применена", file=sys.stderr)
    sys.exit(1)
bg = dark.group(1)
text, count = re.subn(r'(<meta name="theme-color" content=")#[0-9a-fA-F]{6}(")', r"\g<1>" + bg + r"\g<2>", text, count=1)
if count != 1:
    print("не найден статический meta[name=theme-color]", file=sys.stderr)
    sys.exit(1)
index_path.write_text(text, encoding="utf-8")
manifest = manifest_path.read_text(encoding="utf-8")
manifest, count = re.subn(r'("theme_color"\s*:\s*")#[0-9a-fA-F]{6}(")', r"\g<1>#17212B\g<2>", manifest, count=1)
if count != 1:
    print("не найден theme_color в manifest", file=sys.stderr)
    sys.exit(1)
manifest_path.write_text(manifest, encoding="utf-8")
print(f"  meta theme-color: {bg}; manifest theme_color: #17212B")
PYEOF

# 3.6. Fail-closed: каждый проектный <script src="/mcu/...js"> обязан быть в
# рантайме. Импорт без theme-toggle.js уже давал 404 при зелёном sha256sum,
# потому что манифест считается по фактическим файлам.
log "проверка ссылок на проектные скрипты"
python3 - "$DEST/index.html" "$DEST" <<'PYEOF'
import re, sys, pathlib
index, dest = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
text = index.read_text(encoding="utf-8")
missing = []
for src in re.findall(r'<script[^>]*src="(/mcu/[^"]+)"[^>]*></script>', text):
    path = dest / src[len("/mcu/"):]
    if not path.is_file():
        missing.append(src)
if missing:
    print("ОТСУТСТВУЮТ скрипты, на которые ссылается index.html: " + ", ".join(missing), file=sys.stderr)
    sys.exit(1)
print("  ссылки на скрипты: ok")
PYEOF

log "ВНИМАНИЕ: слой PROSTOR-ребренда (frontend/06-rebrand, вне репозитория) этим"
log "  скриптом НЕ применяется. После импорта проверьте index.html (title=PROSTOR,"
log "  локали ru_RU/en_US) и примените ребренд вручную — см. NOTES.md, «Как обновлять»."

# 4. MANIFEST.sha256.
# LC_ALL=C: порядок должен совпадать с Python `sorted()` (codepoint), который
# пишет patch-pnm-layout-crash.py; иначе локаль ru_RU переставляет 55 строк.
# *.gz не должны попадать в манифест: это gitignored артефакты, их наличие
# в рантайме сделало бы CI-checkout невалидным (T07).
( cd "$DEST" && find . -type f ! -name '*.gz' -print0 | LC_ALL=C sort -z | xargs -0 sha256sum ) > "$MANIFEST"
log "манифест: $MANIFEST ($(wc -l < "$MANIFEST") строк)"

# 5. Опционально: source/.
if [ "$WITH_SOURCE" -eq 1 ]; then
    log "копирование source/extracted_frontend (--with-source)"
    mkdir -p "$ROOT/client/pnm-frontend/source"
    rm -rf "$ROOT/client/pnm-frontend/source/extracted_frontend"
    tar -C "$TMP" -cf - source/extracted_frontend | tar -C "$ROOT/client/pnm-frontend/source" -xf -
fi

# 6. Предсжатие .gz удалено (T07): static-сжатие делает tower-http
# CompressionLayer в рантайме, а .gz-артефакты ломали манифест/CI.
if [ "$WITH_GZIP" -eq 1 ]; then
    log "ВНИМАНИЕ: --gzip больше не нужен (runtime CompressionLayer), флаг игнорируется"
fi

log "готово."
