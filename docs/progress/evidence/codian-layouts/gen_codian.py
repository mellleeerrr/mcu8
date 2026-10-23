#!/usr/bin/env python3
"""Генератор таблицы раскладок Codian (52 пресета + 2 кастомных) из спецификации.

Источник истины — текст `Codian_MCU4200_Video_Layouts_Specification.docx`, поле
«Геометрия: …» для каждого макета. Чертежи вендора (55 PNG) использованы как
НЕЗАВИСИМАЯ проверка топологии (число окон и структура объединений) — см.
`validate_topology.py`.

Модель: раскладка = список ОБЛАСТЕЙ на модульной сетке `den`.
Область (x0, y0, x1, y1, cols, rows) делится на `cols × rows` равных окон.
Наложенные окна PIP задаются отдельным списком (z = 1).

ВНИМАНИЕ (T11, 2026-10-24): для макетов 43/46/49 этот файл содержит
ИСТОРИЧЕСКИЕ измерения чертежей; production использует решения R4 (см.
`SUPERSEDED.md`): 43 — равная 4×5, 46 — симметричный центр, 49 — 0.4.
Кроме того, здесь нет R4-правок PIP 33/38/39/51/52. Не использовать этот
файл для перезаписи `crates/vks-layout/src/codian.rs` без сверки diff.
"""

import json
from fractions import Fraction as F

# Область: (x0, y0, x1, y1, cols, rows) в долях канваса.
R = lambda x0, y0, x1, y1, c=1, r=1: (F(x0), F(y0), F(x1), F(y1), c, r)

# Сетка N×M.
def grid(n, m):
    return [R(0, 0, 1, 1, n, m)]

# ── Угловой блок: крупное окно в левом верхнем углу (сетка cols×rows),
#    справа столбец из `rows-1` малых окон, снизу ряд из `cols` малых окон.
def corner(cols, rows, big_c, big_r):
    """Угловой блок: крупное окно `big_r × big_c` в левом верхнем углу базовой сетки
    `cols × rows`; справа — блок `big_r × (cols-big_c)`, снизу — `(rows-big_r) × cols`."""
    out = [R(0, 0, F(big_c, cols), F(big_r, rows))]
    if cols - big_c:
        out.append(R(F(big_c, cols), 0, 1, F(big_r, rows), cols - big_c, big_r))
    if rows - big_r:
        out.append(R(0, F(big_r, rows), 1, 1, cols, rows - big_r))
    return out

# ── Кольцо: крупный блок в центре базовой сетки cols×rows, вокруг — кольцо мелких.
def ring(cols, rows, big_c, big_r):
    """Центральный блок big_c×big_r, выровненный по центру сетки."""
    r0 = (rows - big_r) // 2
    c0 = (cols - big_c) // 2
    out = []
    for i in range(rows):
        for j in range(cols):
            inside = r0 <= i < r0 + big_r and c0 <= j < c0 + big_c
            if inside:
                continue
            out.append(R(F(j, cols), F(i, rows), F(j + 1, cols), F(i + 1, rows)))
    out.append(R(F(c0, cols), F(r0, rows), F(c0 + big_c, cols), F(r0 + big_r, rows)))
    return out

# ── Квадрант 2×2, один из которых разбит на 2×2.
def quadrant(split_idx):
    """split_idx: 0 — ЛВ, 1 — ПН, 2 — ЛН, 3 — ПВ."""
    out = []
    cells = [(0, 0), (0, 1), (1, 0), (1, 1)]
    for idx, (rr, cc) in enumerate(cells):
        if idx == split_idx:
            continue
        out.append(R(F(cc, 2), F(rr, 2), F(cc + 1, 2), F(rr + 1, 2)))
    sr, sc = cells[split_idx]
    out.append(R(F(sc, 2), F(sr, 2), F(sc + 1, 2), F(sr + 1, 2), 2, 2))
    return out

# ── Два яруса: верхний `top_h` высоты из `top_n` окон, нижний из `bot_n`.
def two_tiers(top_h, top_n, bot_n, bot_cols=0, bot_rows=1):
    bot_cols = bot_cols or bot_n
    out = [R(0, 0, 1, top_h, top_n, 1)]
    if bot_rows > 1:
        out.append(R(0, top_h, 1, 1, bot_cols, bot_rows))
    else:
        out.append(R(0, top_h, 1, 1, bot_n, 1))
    return out

def invert_tiers(regions):
    """Инверсия двух ярусов: верхний ↔ нижний (по площади ярусов сохраняется)."""
    top, bot = regions[0], regions[1]
    th = top[3] - top[1]
    bh = bot[3] - bot[1]
    new_top = (bot[0], 0, bot[2], bh, bot[4], bot[5])
    new_bot = (top[0], bh, top[2], bh + th, top[4], top[5])
    return [new_top, new_bot]


def invert_cols(regions):
    """Инверсия двух колонок: левая ↔ правая."""
    left, right = regions[0], regions[1]
    lw = left[2] - left[0]
    rw = right[2] - right[0]
    new_left = (0, right[1], rw, right[3], right[4], right[5])
    new_right = (rw, left[1], rw + lw, left[3], left[4], left[5])
    return [new_left, new_right]


# ── Два яруса, где один из них разбит на подсетку.
def split_tier(top_h, top_n, bot_n, bot_cols, bot_rows):
    return [R(0, 0, 1, top_h, top_n, 1), R(0, top_h, 1, 1, bot_cols, bot_rows)]

# ── Вертикальные два яруса: левый `left_w` ширины из `left_n` окон, правый.
def two_cols(left_w, left_n, right_n, right_cols=0, right_rows=1):
    right_cols = right_cols or right_n
    out = []
    if left_n == 1:
        out.append(R(0, 0, left_w, 1, 1, 1))
    else:
        out.append(R(0, 0, left_w, 1, 1, left_n))
    if right_rows > 1:
        out.append(R(left_w, 0, 1, 1, right_cols, right_rows))
    else:
        out.append(R(left_w, 0, 1, 1, right_n, 1))
    return out

LAYOUTS = {
    1: grid(1, 1),
    2: grid(2, 2),
    3: grid(3, 3),
    4: grid(4, 4),
    5: corner(3, 3, 2, 2),
    6: corner(4, 4, 3, 3),
    7: corner(4, 4, 2, 2),
    8: [R(0, 0, 1, F(1, 4), 3, 1), R(0, F(1, 4), 1, F(5, 8), 3, 1), R(0, F(5, 8), 1, 1, 3, 1)],
    9: [R(0, 0, 1, F(1, 5), 4, 1), R(0, F(1, 5), 1, F(7, 15), 4, 1),
        R(0, F(7, 15), 1, F(11, 15), 4, 1), R(0, F(11, 15), 1, 1, 4, 1)],
    10: two_tiers(F(2, 3), 2, 4),
    11: two_tiers(F(1, 3), 4, 2),
    12: [R(0, 0, 1, F(1, 2)), R(0, F(1, 2), 1, 1)],
    13: [R(0, 0, 1, F(1, 2)), R(0, F(1, 2), 1, 1, 2, 1)],
    14: [R(0, 0, 1, F(2, 3)), R(0, F(2, 3), 1, 1, 3, 1)],
    15: [R(0, 0, 1, F(3, 4)), R(0, F(3, 4), 1, 1, 4, 1)],
    16: [R(0, 0, F(1, 2), 1), R(F(1, 2), 0, 1, 1)],
    17: two_cols(F(1, 2), 1, 2, 1, 2),
    18: two_cols(F(2, 3), 1, 3, 1, 3),
    19: two_cols(F(3, 4), 1, 4, 1, 4),
    20: split_tier(F(1, 2), 1, 8, 4, 2),
    21: invert_tiers(split_tier(F(1, 2), 1, 8, 4, 2)),
    22: two_cols(F(1, 2), 1, 8, 2, 4),
    23: invert_cols(two_cols(F(1, 2), 1, 8, 2, 4)),
    24: [R(0, 0, 1, F(1, 4), 4, 1), R(0, F(1, 4), 1, F(3, 4)), R(0, F(3, 4), 1, 1, 4, 1)],
    25: [R(0, 0, 1, F(1, 4), 4, 1), R(0, F(1, 4), 1, F(3, 4), 2, 1), R(0, F(3, 4), 1, 1, 4, 1)],
    26: [R(0, 0, F(1, 4), 1, 1, 4), R(F(1, 4), 0, F(3, 4), 1), R(F(3, 4), 0, 1, 1, 1, 4)],
    27: [R(0, 0, F(1, 4), 1, 1, 4), R(F(1, 4), 0, F(3, 4), 1, 1, 2), R(F(3, 4), 0, 1, 1, 1, 4)],
    28: split_tier(F(1, 2), 2, 8, 4, 2),
    29: invert_tiers(split_tier(F(1, 2), 2, 8, 4, 2)),
    30: two_cols(F(1, 2), 2, 8, 2, 4),
    31: invert_cols(two_cols(F(1, 2), 2, 8, 2, 4)),
    32: ring(4, 4, 2, 2),
    34: quadrant(3),
    35: quadrant(2),
    36: quadrant(0),
    37: quadrant(1),
    40: split_tier(F(1, 2), 3, 10, 5, 2),
    41: [R(0, 0, 1, F(1, 4), 5, 1), R(0, F(1, 4), 1, F(3, 4), 3, 1), R(0, F(3, 4), 1, 1, 5, 1)],
    42: invert_tiers(split_tier(F(1, 2), 3, 10, 5, 2)),
    43: [R(0, 0, 1, F(5, 28), 4, 1), R(0, F(5, 28), 1, F(10, 28), 4, 1),
         R(0, F(10, 28), 1, F(16, 28), 4, 1), R(0, F(16, 28), 1, F(22, 28), 4, 1),
         R(0, F(22, 28), 1, 1, 4, 1)],
    44: [R(0, 0, F(1, 2), F(2, 5)), R(F(1, 2), 0, F(3, 4), F(1, 5)), R(F(3, 4), 0, 1, F(1, 5)),
         R(F(1, 2), F(1, 5), F(3, 4), F(2, 5)), R(F(3, 4), F(1, 5), 1, F(2, 5)),
         R(0, F(2, 5), 1, 1, 4, 3)],
    45: [R(F(1, 4), 0, F(3, 4), F(2, 5)), R(0, 0, F(1, 4), F(1, 5)), R(F(3, 4), 0, 1, F(1, 5)),
         R(0, F(1, 5), F(1, 4), F(2, 5)), R(F(3, 4), F(1, 5), 1, F(2, 5)),
         R(0, F(2, 5), 1, 1, 4, 3)],
    46: [R(0, 0, 1, F(1, 4), 4, 1), R(0, F(1, 4), F(3, 8), F(3, 4)), R(F(3, 8), F(1, 4), F(1, 2), F(1, 2)),
         R(F(3, 8), F(1, 2), F(1, 2), F(3, 4)), R(F(1, 2), F(1, 4), 1, F(3, 4)),
         R(0, F(3, 4), 1, 1, 4, 1)],
    47: ring(5, 5, 3, 3),
    48: grid(5, 5),
    49: [R(0, 0, 1, F(5, 12), 2, 1), R(0, F(5, 12), 1, 1, 4, 3)],
    50: [R(0, 0, 1, F(5, 6)), R(0, F(5, 6), 1, 1, 6, 1)],
}

# Наложенные окна PIP: (x0, y0, x1, y1) в долях канваса (из чертежей вендора).
OVERLAY = {
    33: [(0.0312, 0.0415, 0.2462, 0.2448), (0.2741, 0.0415, 0.4891, 0.2448),
         (0.5171, 0.0415, 0.7321, 0.2448), (0.7601, 0.0415, 0.9751, 0.2448),
         (0.0312, 0.2822, 0.2462, 0.4855), (0.7601, 0.2822, 0.9751, 0.4855),
         (0.0312, 0.5228, 0.2462, 0.7261), (0.7601, 0.5228, 0.9751, 0.7261),
         (0.0312, 0.7635, 0.2462, 0.9668), (0.2741, 0.7635, 0.4891, 0.9668),
         (0.5171, 0.7635, 0.7321, 0.9668), (0.7601, 0.7635, 0.9751, 0.9668)],
    38: [(0.0374, 0.7261, 0.3240, 0.9502), (0.3583, 0.7261, 0.6449, 0.9502),
         (0.6791, 0.7261, 0.9657, 0.9502)],
    39: [(0.7664, 0.0415, 0.9689, 0.2448), (0.7664, 0.2822, 0.9689, 0.4855),
         (0.7664, 0.5228, 0.9689, 0.7261), (0.7664, 0.7635, 0.9689, 0.9668)],
    51: [(0.6355, 0.6183, 0.9564, 0.9420)],
    52: [(0.7788, 0.7676, 0.9626, 0.9502)],
}

EXPECTED = {
    1: 1, 2: 4, 3: 9, 4: 16, 5: 6, 6: 8, 7: 13, 8: 9, 9: 16, 10: 6, 11: 6, 12: 2, 13: 3,
    14: 4, 15: 5, 16: 2, 17: 3, 18: 4, 19: 5, 20: 9, 21: 9, 22: 9, 23: 9, 24: 9, 25: 10,
    26: 9, 27: 10, 28: 10, 29: 10, 30: 10, 31: 10, 32: 13, 33: 13, 34: 7, 35: 7, 36: 7,
    37: 7, 38: 4, 39: 5, 40: 13, 41: 13, 42: 13, 43: 20, 44: 17, 45: 17, 46: 12, 47: 17,
    48: 25, 49: 14, 50: 7, 51: 2, 52: 2,
}


def den_for(regions, overlay):
    """Минимальный знаменатель, при котором целы все границы ОКОН (и областей)."""
    den = 1
    for (x0, y0, x1, y1) in windows(regions):
        for v in (x0, y0, x1, y1):
            den = den * v.denominator // _gcd(den, v.denominator)
    if overlay:
        # Наложенные окна — из измерений чертежа: фиксированная шкала 1/1000,
        # знаменатель раскладки приводим к кратному 1000.
        den = den * 1000 // _gcd(den, 1000)
    return den


def _gcd(a, b):
    while b:
        a, b = b, a % b
    return a


def windows(regions):
    """Разворачивает области в список окон (доля канваса)."""
    out = []
    for (x0, y0, x1, y1, c, r) in regions:
        for i in range(r):
            for j in range(c):
                out.append((x0 + (x1 - x0) * j / c, y0 + (y1 - y0) * i / r,
                            x0 + (x1 - x0) * (j + 1) / c, y0 + (y1 - y0) * (i + 1) / r))
    return out


def check_tiles(win, den):
    """Проверка: окна без наложений покрывают канвас без щелей."""
    cells = {}
    step = 1
    for (x0, y0, x1, y1) in win:
        gx0, gy0 = x0 * den, y0 * den
        gx1, gy1 = x1 * den, y1 * den
        assert gx0 == int(gx0) and gy0 == int(gy0) and gx1 == int(gx1) and gy1 == int(gy1), (x0, y0, x1, y1, den)
        for gx in range(int(gx0), int(gx1)):
            for gy in range(int(gy0), int(gy1)):
                assert (gx, gy) not in cells, f"перекрытие в ({gx},{gy})"
                cells[(gx, gy)] = 1
    missing = [(gx, gy) for gx in range(den) for gy in range(den) if (gx, gy) not in cells]
    return missing


if __name__ == "__main__":
    table = {}
    errors = []
    for num in sorted(EXPECTED):
        overlay = OVERLAY.get(num, [])
        regions = LAYOUTS.get(num)
        if regions is None:
            regions = [R(0, 0, 1, 1)]
        win = windows(regions)
        total = len(win) + len(overlay)
        if total != EXPECTED[num]:
            errors.append(f"#{num}: окон {total}, ожидалось {EXPECTED[num]}")
            continue
        den = den_for(regions, overlay)
        if num not in OVERLAY:
            miss = check_tiles(win, den)
            if miss:
                errors.append(f"#{num}: не покрыто {len(miss)} ячеек из {den}x{den}")
        table[num] = {"den": den, "regions": regions, "overlay": overlay}
    if errors:
        print("ОШИБКИ:")
        for e in errors:
            print("  ", e)
    else:
        print(f"все {len(table)} раскладок согласованы (число окон + покрытие канваса)")
    json.dump({str(k): {"den": v["den"],
                        "regions": [[str(x) for x in r] for r in v["regions"]],
                        "overlay": [list(o) for o in v["overlay"]]}
               for k, v in table.items()}, open("docs/progress/evidence/codian-layouts/codian_table.json", "w"), indent=1)
