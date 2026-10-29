#!/usr/bin/env python3
"""Статический контроль рантайма PNM (T07).

Проверки:
1. MANIFEST.sha256 перечисляет ровно фактические файлы рантайма (без *.gz) —
   ни пропусков, ни лишних строк.
2. Каждый `<script src="/mcu/...js">` из index.html существует в рантайме
   (импорт без theme-toggle.js уже давал 404 при зелёном sha256sum).
3. Нет `.gz`-файлов: сжатие делает tower-http CompressionLayer, а stale .gz
   попадал бы в манифест и ломал CI-checkout.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PNM = ROOT / "client/pnm-frontend/deployed/pnm"
MANIFEST = ROOT / "client/pnm-frontend/MANIFEST.sha256"

failures = []

actual = sorted(
    p.relative_to(PNM).as_posix() for p in PNM.rglob("*") if p.is_file() and not p.name.endswith(".gz")
)
listed = []
for line in MANIFEST.read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    digest, _, name = line.partition("  ")
    listed.append(name.lstrip("./"))
    if len(digest) != 64:
        failures.append(f"MANIFEST: bad digest line: {line!r}")
if listed != actual:
    only_files = set(actual) - set(listed)
    only_manifest = set(listed) - set(actual)
    if only_files:
        failures.append(f"MANIFEST: not listed: {sorted(only_files)[:5]}")
    if only_manifest:
        failures.append(f"MANIFEST: listed but missing: {sorted(only_manifest)[:5]}")

index = (PNM / "index.html").read_text(encoding="utf-8")
for src in re.findall(r'<script[^>]*src="(/mcu/[^"]+\.js)"[^>]*></script>', index):
    if not (PNM / src[len("/mcu/"):]).is_file():
        failures.append(f"index.html references missing script: {src}")

for gz in PNM.rglob("*.gz"):
    failures.append(f"stale .gz file in runtime: {gz.relative_to(PNM)}")

if failures:
    print("PNM runtime check FAILED", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(1)
print(f"PNM runtime check ok: {len(actual)} files, manifest matches, script refs exist, no .gz")
