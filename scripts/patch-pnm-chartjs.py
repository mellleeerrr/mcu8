#!/usr/bin/env python3
"""Идемпотентно переводит графики статистики PNM 2.2.1.66 на Chart.js v4.

Бандл собран под Chart.js v2 (`yAxes/xAxes`, `gridLines`, `fontSize`,
`legend.labels.fontColor`), а в комплекте v4: консоль даёт
`Invalid scale configuration`, оси/легенды не рисуются. Скрипт правит
in-place минифицированные чанки; запускать после импорта vendor-архива.

Правила — с якорями (см. `scripts/pnm_patch_lib.py`): если вендорский минификатор
переставит символы, скрипт **падает** (`exit 1`), а не «молча ничего не делает»,
и импорт (`import-pnm-frontend.sh`) прерывается с указанием, что патч требует
ревизии. Повторный запуск на пропатченном бандле — no-op с `exit 0`.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from pnm_patch_lib import apply_rules  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
CHUNKS_REL = "client/pnm-frontend/deployed/pnm/chunks"
INDEX3 = "index3.bundle-2.2.1.66.js"
INDEX9 = "index9.bundle-2.2.1.66.js"

# (name, old, new, expected_old, expected_new). Порядок важен: правки
# последовательные, якоря следующих правил учитывают результат предыдущих.
# `expected_old`/`expected_new` измерены на вендоре 2.2.1.66 (PNM-Frontend-20261016).
INDEX3_RULES = (
    ("axes-y", "yAxes:[{", "y:{", 6, 12),
    ("axes-x", "xAxes:[{", "x:{", 5, 6),
    ("grid-lines", "gridLines:", "grid:", 9, 11),
    ("font-size", "fontSize:12", "font:{size:12}", 3, 3),
    ("close-yAxes-after-x", "}],x:{", "},x:{", 5, 6),
    ("close-bracket-before-responsive", "]},responsive", "},responsive", 2, 5),
    ("close-bracket-before-aspect", "]},maintainAspectRatio", "},maintainAspectRatio", 2, 2),
    ("close-bracket-before-responsive-plain", "],responsive", ",responsive", 2, 5),
    (
        "grid-display-close",
        "x:{grid:{display:!0}},responsive:!0,maintainAspectRatio:!1}}}",
        "x:{grid:{display:!0}}},responsive:!0,maintainAspectRatio:!1}}",
        2,
        3,
    ),
    (
        "legend-fontcolor",
        'legend:{labels:{fontColor:"rgb(255, 0, 0)"}}',
        'plugins:{legend:{labels:{color:"rgb(255, 0, 0)"}}}',
        3,
        3,
    ),
    # Chart.js v4: min/max живут на scale, а не в ticks.
    ("scale-min-max-100", "y:{ticks:{min:0,max:100,", "y:{min:0,max:100,ticks:{", 3, 3),
    (
        "scale-min-max-memory",
        "y:{ticks:{min:0,max:this.memory.total,",
        "y:{min:0,max:this.memory.total,ticks:{",
        1,
        1,
    ),
    ("scale-min-only", "y:{ticks:{min:0,", "y:{min:0,ticks:{", 2, 2),
)

# index9: два разных Chart-компонента, watcher срабатывает до создания chart.
# Третий якорь встречается дважды (`TcLineChart` и `TcRadarChart`), а патчить
# надо только первый ⇒ limit = 1 (шестой элемент правила).
INDEX9_RULES = (
    (
        "labels-watcher-no-chart",
        "shouldUpdateLabels(){(0,g.default)(this.chartData.labels,this.chart.data.labels)"
        "||(this.chart.chart.data.labels=this.chartData.labels)}",
        "shouldUpdateLabels(){const c=this.chart;if(!c||!c.data)return;"
        "(0,g.default)(this.chartData.labels,c.data.labels)||(c.data.labels=this.chartData.labels)}",
        1,
        1,
    ),
    (
        "labels-watcher-radar",
        "shouldUpdateLabels(){const f=this.chartData.labels,m=this.chart.data.labels;"
        "(0,g.default)(f,m)||f.forEach(((y,_)=>{m[_]=y}))}",
        "shouldUpdateLabels(){const f=this.chartData.labels,"
        "m=this.chart&&this.chart.data&&this.chart.data.labels;if(!m)return;"
        "(0,g.default)(f,m)||f.forEach(((y,_)=>{m[_]=y}))}",
        2,
        2,
    ),
    (
        "data-watcher-line-only",
        "shouldUpdateData(){const f=this.chartData.datasets,m=this.chart.data.datasets;",
        "shouldUpdateData(){const f=this.chartData.datasets,"
        "m=this.chart&&this.chart.data&&this.chart.data.datasets;if(!m)return;",
        2,
        1,
        1,
    ),
)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Патч PNM: графики статистики на Chart.js v4")
    parser.add_argument("--root", type=Path, default=ROOT, help="корень репозитория")
    parser.add_argument("--check", action="store_true", help="только проверить, ничего не писать")
    args = parser.parse_args(argv)

    chunks = args.root / CHUNKS_REL
    print("[patch-chartjs] root=%s" % args.root)
    ok = True
    for name, rules in ((INDEX3, INDEX3_RULES), (INDEX9, INDEX9_RULES)):
        print("  %s:" % name)
        file_ok, messages = apply_rules(chunks / name, rules, args.check)
        ok = ok and file_ok
        for line in messages:
            print(line)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
