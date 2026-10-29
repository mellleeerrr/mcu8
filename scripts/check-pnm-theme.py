#!/usr/bin/env python3
"""Статический инвариант темы PNM (T07).

- static `<meta name="theme-color">` равен token-у `--c-bg` тёмной темы;
- `html.theme-light` определяет `--c-bg` (иначе computed значение пустое в
  светлой теме и meta не может ему соответствовать);
- `theme-toggle.js` пишет в meta ВЫЧИСЛЕННЫЙ `--c-bg`, а не константу primary;
- manifest `theme_color` = `#17212B` (surface), не старый `#0097A7`.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PNM = ROOT / "client/pnm-frontend/deployed/pnm"

failures = []
index = (PNM / "index.html").read_text(encoding="utf-8")

meta = re.search(r'<meta name="theme-color" content="(#[0-9a-fA-F]{6})"', index)
dark = re.search(r"html\.theme-dark\s*\{[^}]*--c-bg:\s*(#[0-9a-fA-F]{6})", index, re.S)
light = re.search(r"html\.theme-light\s*\{[^}]*--c-bg:\s*(#[0-9a-fA-F]{6})", index, re.S)
if not meta:
    failures.append("static meta[name=theme-color] missing")
if not dark:
    failures.append("html.theme-dark --c-bg missing")
if not light:
    failures.append("html.theme-light --c-bg missing (light theme has no background token)")
if meta and dark and meta.group(1).lower() != dark.group(1).lower():
    failures.append(f"meta theme-color {meta.group(1)} != dark --c-bg {dark.group(1)}")

toggle = (PNM / "theme-toggle.js").read_text(encoding="utf-8")
if "getPropertyValue('--c-bg')" not in toggle:
    failures.append("theme-toggle.js does not read computed --c-bg")

manifest = (PNM / "assets/manifest.webmanifest").read_text(encoding="utf-8")
theme_color = re.search(r'"theme_color"\s*:\s*"(#[0-9a-fA-F]{6})"', manifest)
if not theme_color:
    failures.append("manifest theme_color missing")
elif theme_color.group(1).lower() != "#17212b":
    failures.append(f"manifest theme_color {theme_color.group(1)} != #17212B")

if failures:
    print("PNM theme check FAILED", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(1)
print("PNM theme check ok: meta == dark --c-bg, light token present, toggle uses computed token, manifest #17212B")
