#!/usr/bin/env python3
"""Точная геометрия раскладок Codian из чертежей docx (рекурсивное разбиение).

Алгоритм: Mondrian-разбиение прямоугольника.
  box → вертикальные линии, пересекающие box на всю высоту → колонки
      → в каждой колонке горизонтальные линии на всю ширину → строки
      → листья = окна.

Линии ищутся по «полосам» (максимальные прогоны соседних закрашенных
столбцов/строк); координата реза = центр полосы — поэтому толщина линии
(3–7 px) не даёт «полос-мусор» и не смещает границы окон.
"""

import json
import os
import sys

from PIL import Image

INK = 170
MARGIN = 6  # половина максимальной полосы линии + запас


class Img:
    def __init__(self, path):
        im = Image.open(path).convert("L")
        self.w, self.h = im.size
        px = im.load()
        w, h = self.w, self.h
        self.raw = [[1 if px[x, y] < INK else 0 for x in range(w)] for y in range(h)]
        self.cp = []
        for x in range(w):
            acc = [0] * (h + 1)
            for y in range(h):
                acc[y + 1] = acc[y] + self.raw[y][x]
            self.cp.append(acc)
        self.rp = []
        for y in range(h):
            acc = [0] * (w + 1)
            row = self.raw[y]
            for x in range(w):
                acc[x + 1] = acc[x] + row[x]
            self.rp.append(acc)

    def col_full(self, x, y0, y1):
        if y1 <= y0:
            return False
        return self.cp[x][y1] - self.cp[x][y0] == (y1 - y0)

    def row_full(self, y, x0, x1):
        if x1 <= x0:
            return False
        return self.rp[y][x1] - self.rp[y][x0] == (x1 - x0)


def bands(flags, offset=0):
    """Максимальные прогоны True → (центр в абсолютных координатах, длина полосы)."""
    out = []
    start = None
    for i, f in enumerate(flags):
        if f and start is None:
            start = i
        elif not f and start is not None:
            out.append((offset + (start + i - 1) / 2.0, i - start))
            start = None
    if start is not None:
        out.append((offset + (start + len(flags) - 1) / 2.0, len(flags) - start))
    return out


def split(im, box, depth=0):
    x0, y0, x1, y1 = box
    if depth > 24 or x1 - x0 < 1.5 or y1 - y0 < 1.5:
        return [box]
    yi0, yi1 = int(round(y0)), int(round(y1))
    xi0, xi1 = int(round(x0)), int(round(x1))

    lo, hi = xi0 + MARGIN, xi1 - MARGIN
    if hi > lo:
        vb = bands([im.col_full(x, yi0, yi1) for x in range(lo, hi)], lo)
        if vb:
            edges = [x0] + [c for c, _ in vb] + [x1]
            out = []
            for i in range(len(edges) - 1):
                out.extend(split(im, (edges[i], y0, edges[i + 1], y1), depth + 1))
            return out

    lo, hi = yi0 + MARGIN, yi1 - MARGIN
    if hi > lo:
        hb = bands([im.row_full(y, xi0, xi1) for y in range(lo, hi)], lo)
        if hb:
            edges = [y0] + [c for c, _ in hb] + [y1]
            out = []
            for i in range(len(edges) - 1):
                out.extend(split(im, (x0, edges[i], x1, edges[i + 1]), depth + 1))
            return out

    return [box]


def frame(im):
    """Координаты центров внешней рамки чертежа."""
    rows = [y for y in range(im.h) if any(im.raw[y])]
    cols = [x for x in range(im.w) if any(im.raw[y][x] for y in rows)]
    y0, y1 = rows[0], rows[-1]
    x0, x1 = cols[0], cols[-1]
    vb = bands([im.col_full(x, y0, y1 + 1) for x in range(x0, x1 + 1)], x0)
    hb = bands([im.row_full(y, x0, x1 + 1) for y in range(y0, y1 + 1)], y0)
    return (vb[0][0], hb[0][0], vb[-1][0], hb[-1][0])


def leaves(path):
    im = Img(path)
    box = frame(im)
    rects = split(im, box)
    x0, y0, x1, y1 = box
    W, H = x1 - x0, y1 - y0
    out = []
    for (rx0, ry0, rx1, ry1) in rects:
        out.append(
            {
                "x": round((rx0 - x0) / W, 6),
                "y": round((ry0 - y0) / H, 6),
                "w": round((rx1 - rx0) / W, 6),
                "h": round((ry1 - ry0) / H, 6),
            }
        )
    out.sort(key=lambda r: (round(r["y"], 3), round(r["x"], 3)))
    return out, (W, H)


def main():
    args = sys.argv[1:]
    out_json = None
    if args and args[0] == "--json":
        out_json = args[1]
        args = args[2:]
    result = {}
    for path in args:
        name = os.path.basename(path)
        rects, size = leaves(path)
        result[name] = {"size": size, "rects": rects}
        print(f"{name}: {len(rects)} окон")
    if out_json:
        with open(out_json, "w") as f:
            json.dump(result, f, indent=1)


if __name__ == "__main__":
    main()


def components(path):
    """Связные компоненты чернил → bounding box каждой (для PIP-оверлеев)."""
    im = Img(path)
    w, h = im.w, im.h
    seen = [[False] * w for _ in range(h)]
    out = []
    for y in range(h):
        for x in range(w):
            if not im.raw[y][x] or seen[y][x]:
                continue
            stack = [(x, y)]
            seen[y][x] = True
            minx = maxx = x
            miny = maxy = y
            while stack:
                cx, cy = stack.pop()
                minx, maxx = min(minx, cx), max(maxx, cx)
                miny, maxy = min(miny, cy), max(maxy, cy)
                for nx, ny in ((cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)):
                    if 0 <= nx < w and 0 <= ny < h and im.raw[ny][nx] and not seen[ny][nx]:
                        seen[ny][nx] = True
                        stack.append((nx, ny))
            out.append((minx, miny, maxx + 1, maxy + 1))
    return out


def overlay_leaves(path):
    """Раскладка с наложенными PIP: фон + отдельные прямоугольники PIP."""
    im = Img(path)
    box = frame(im)
    x0, y0, x1, y1 = box
    W, H = x1 - x0, y1 - y0
    comps = components(path)
    rects = []
    for (a, b, c, d) in comps:
        if (c - a) < 4 or (d - b) < 4:
            continue
        rects.append(
            {
                "x": round((a - x0) / W, 6),
                "y": round((b - y0) / H, 6),
                "w": round((c - a) / W, 6),
                "h": round((d - b) / H, 6),
                "px": [a - x0, b - y0, c - a, d - b],
            }
        )
    rects.sort(key=lambda r: (round(r["y"], 3), round(r["x"], 3)))
    return rects, (W, H)
