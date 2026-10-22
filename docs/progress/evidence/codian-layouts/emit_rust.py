#!/usr/bin/env python3
"""Генерация Rust-модуля `crates/vks-layout/src/codian.rs` из проверенной таблицы."""

import importlib.util
import os

os.chdir("/home/pro/Рабочий стол/mcu")
spec = importlib.util.spec_from_file_location("g", "docs/progress/evidence/codian-layouts/gen_codian.py")
G = importlib.util.module_from_spec(spec)
spec.loader.exec_module(G)

# ── Две кастомные комбинированные раскладки (спека §1) ────────────────────────
# Модульная сетка 12×5 (крупные блоки шириной 2,5 колонки) и 6×4.
CUSTOM = {
    "codian_custom_22": {
        "den": (12, 5),
        "regions": [
            (0, 0, 5, 2, 1, 1),    # крупный блок 1 — Fixed
            (5, 0, 10, 2, 1, 1),   # крупный блок 2 — Active/Rolling
            (10, 0, 12, 1, 1, 1),  # 6-я колонка, верхняя ячейка — Rolling
            (10, 1, 12, 2, 1, 1),  # 6-я колонка, нижняя ячейка — Rolling
            (0, 2, 12, 5, 6, 3),   # нижний ярус 6×3
        ],
        "roles": ["Fixed", "Active", "Rolling", "Rolling",
                  "Rolling", "Default", "Rolling", "Default", "Default", "Rolling",
                  "Rolling", "Rolling", "Rolling", "Rolling", "Rolling", "Rolling",
                  "Rolling", "Rolling", "Default", "Rolling", "Rolling", "Rolling"],
    },
    "codian_custom_14": {
        "den": (6, 4),
        "regions": [
            (0, 0, 3, 2, 1, 1),   # крупный блок 1 — Fixed
            (3, 0, 6, 2, 1, 1),   # крупный блок 2 — Active/Rolling
            (0, 2, 6, 3, 6, 1),   # 3-й ряд
            (0, 3, 6, 4, 6, 1),   # 4-й ряд
        ],
        "roles": ["Fixed", "Active",
                  "Rolling", "Rolling", "Default", "Rolling", "Rolling", "Default",
                  "Default", "Rolling", "Default", "Default", "Default", "Rolling"],
    },
}

NAMES = {
    1: "Полный экран", 2: "Сетка 2×2", 3: "Сетка 3×3", 4: "Сетка 4×4",
    5: "Угловая 1+5 (3×3)", 6: "Угловая 1+7 (4×4)", 7: "Угловая 1+12 (4×4)",
    8: "Асимметричная 3+6 (3×3)", 9: "Асимметричная 4+12 (4×4)",
    10: "2+4 сверху-внизу", 11: "4+2 сверху-внизу", 12: "Горизонтальный сплит",
    13: "1+2 горизонтальный", 14: "1+3 горизонтальный", 15: "1+4 горизонтальный",
    16: "Вертикальный сплит", 17: "1+2 вертикальный", 18: "1+3 вертикальный",
    19: "1+4 вертикальный", 20: "1+8 малые снизу", 21: "8+1 малые сверху",
    22: "1+8 малые справа", 23: "8+1 малые слева", 24: "4+1+4 горизонтальный центр",
    25: "4+2+4 горизонтальный центр", 26: "4+1+4 вертикальный центр",
    27: "4+2+4 вертикальный центр", 28: "2+8 сверху-внизу", 29: "8+2 сверху-внизу",
    30: "2+8 слева-направо", 31: "8+2 слева-направо", 32: "Рамка 1+12 (4×4)",
    33: "Наложенная PIP-рамка 1+12", 34: "Квадрант ПН 3+4", 35: "Квадрант ЛН 3+4",
    36: "Квадрант ЛВ 3+4", 37: "Квадрант ПВ 3+4", 38: "PIP снизу 1+3",
    39: "PIP справа 1+4", 40: "3+10 сверху-внизу", 41: "5+3+5 горизонтальный центр",
    42: "10+3 сверху-внизу", 43: "Сетка 4×5 (20 окон)", 44: "Угловая 1+16 (4×5)",
    45: "Верхний центр 1+16 (4×5)", 46: "Симметричный центр 2+10",
    47: "Рамка 1+16 (5×5)", 48: "Сетка 5×5 (25 окон)", 49: "2+12 сверху-внизу",
    50: "1+6 нижняя полоса", 51: "PIP укрупнённый 1+1", 52: "PIP малый 1+1",
}


def region_tuples(regions, den):
    """Области в единицах модульной сетки `den`."""
    out = []
    for (x0, y0, x1, y1, c, r) in regions:
        out.append((int(x0 * den), int(y0 * den), int(x1 * den), int(y1 * den), c, r))
    return out


PIP_SCALE = 1000


def pip_tuples(pips, den):
    """Наложенные окна в шкале 1/1000 (измерения чертежа)."""
    out = []
    for (x0, y0, x1, y1) in pips:
        out.append((int(round(x0 * PIP_SCALE)), int(round(y0 * PIP_SCALE)),
                    int(round(x1 * PIP_SCALE)), int(round(y1 * PIP_SCALE))))
    return out


def emit():
    lines = []
    for num in sorted(G.EXPECTED):
        regions = G.LAYOUTS.get(num) or [G.R(0, 0, 1, 1)]
        pips = G.OVERLAY.get(num, [])
        den = G.den_for(regions, pips)
        regs = region_tuples(regions, den)
        pips_d = pip_tuples(pips, den)
        name = NAMES[num]
        lines.append(f'    ("codian_{num:02}", "{name}", {den}, &[')
        for r in regs:
            lines.append(f"        r({r[0]}, {r[1]}, {r[2]}, {r[3]}, {r[4]}, {r[5]}),")
        lines.append("    ], &[")
        for p in pips_d:
            lines.append(f"        p({p[0]}, {p[1]}, {p[2]}, {p[3]}),")
        lines.append("    ]),")
    return lines


if __name__ == "__main__":
    for line in emit():
        print(line)
