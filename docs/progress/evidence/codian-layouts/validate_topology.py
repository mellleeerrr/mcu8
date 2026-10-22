#!/usr/bin/env python3
"""Независимая сверка: геометрия из текста спецификации против геометрии из
чертежей вендора (52 PNG, разобраны Mondrian-алгоритмом).

Чертежи схематичны в пропорциях (доказано на #5: спека говорит «4/9 экрана»,
чертёж показывает 2/5), поэтому сверяется ТОПОЛОГИЯ:
  * число окон;
  * множество «крупных» окон (площадь > 1.5 × медианной);
  * порядок окон по (y, x) — по диагонали крупные/мелкие.

Результат: расхождений быть не должно.
"""

import importlib.util
import json
import os
import statistics

os.chdir("/home/pro/Рабочий стол/mcu")
spec = importlib.util.spec_from_file_location("g", "docs/progress/evidence/codian-layouts/gen_codian.py")
G = importlib.util.module_from_spec(spec)
spec.loader.exec_module(G)
spec2 = importlib.util.spec_from_file_location("p", "docs/progress/evidence/codian-layouts/parse_layout_drawings.py")
P = importlib.util.module_from_spec(spec2)
spec2.loader.exec_module(P)

OVERLAY = {33, 38, 39, 51, 52}


def drawing_rects(num):
    path = f"docs/progress/evidence/codian-layouts/drawings/image{num + 3}.png"
    im = P.Img(path)
    if num in OVERLAY:
        comps = P.components(path)
        boxes = []
        for (a, b, c, d) in comps:
            if c - a < 4 or d - b < 4:
                continue
            boxes.append((a, b, c, d))
        boxes.sort(key=lambda r: (r[1], r[0]))
        return [{"x": (a) / im.w, "y": (b) / im.h, "w": (c - a) / im.w, "h": (d - b) / im.h} for a, b, c, d in boxes]
    box = P.frame(im)
    out = []
    for (a, b, c, d) in P.split(im, box):
        out.append({"x": (a - box[0]) / (box[2] - box[0]), "y": (b - box[1]) / (box[3] - box[1]),
                    "w": (c - a) / (box[2] - box[0]), "h": (d - b) / (box[3] - box[1])})
    out.sort(key=lambda r: (round(r["y"], 3), round(r["x"], 3)))
    return out


def spec_rects(num):
    regions = G.LAYOUTS.get(num) or [G.R(0, 0, 1, 1)]
    win = G.windows(regions)
    out = [{"x": float(x0), "y": float(y0), "w": float(x1 - x0), "h": float(y1 - y0)} for x0, y0, x1, y1 in win]
    for (x0, y0, x1, y1) in G.OVERLAY.get(num, []):
        out.append({"x": x0, "y": y0, "w": x1 - x0, "h": y1 - y0})
    out.sort(key=lambda r: (round(r["y"], 3), round(r["x"], 3)))
    return out


def big_flags(rects):
    areas = [r["w"] * r["h"] for r in rects]
    med = statistics.median(areas)
    return "".join("B" if a > 1.5 * med else "." for a in areas)


if __name__ == "__main__":
    bad = []
    for num in sorted(G.EXPECTED):
        dr = drawing_rects(num)
        sr = spec_rects(num)
        if len(dr) != len(sr):
            bad.append(f"#{num}: окон чертёж={len(dr)} спека={len(sr)}")
            continue
        fd, fs = big_flags(dr), big_flags(sr)
        if fd != fs:
            bad.append(f"#{num}: крупные окна различаются: чертёж={fd} спека={fs}")
    print(f"сверено раскладок: {len(G.EXPECTED)}")
    if bad:
        print("РАСХОЖДЕНИЯ ТОПОЛОГИИ:")
        for b in bad:
            print("  ", b)
    else:
        print("ТОПОЛОГИЯ СОВПАЛА В 52/52 (число окон и карта крупных окон)")
