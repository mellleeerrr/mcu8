#!/usr/bin/env python3
"""Идемпотентно переводит графики статистики PNM 2.2.1.66 на Chart.js v4.

Бандл собран под Chart.js v2 (`yAxes/xAxes`, `gridLines`, `fontSize`,
`legend.labels.fontColor`), а в комплекте v4: консоль даёт
`Invalid scale configuration`, оси/легенды не рисуются. Скрипт правит
in-place минифицированные чанки; запускать после импорта vendor-архива.
"""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHUNKS = ROOT / "client/pnm-frontend/deployed/pnm/chunks"
INDEX3 = CHUNKS / "index3.bundle-2.2.1.66.js"
INDEX9 = CHUNKS / "index9.bundle-2.2.1.66.js"


def resolve_paths(root: Path | None = None) -> tuple[Path, Path]:
    """Пути к чанкам. --root нужен скрипту импорта (песочница/копия репозитория)."""
    base = (root or ROOT) / "client/pnm-frontend/deployed/pnm/chunks"
    return base / INDEX3.name, base / INDEX9.name


def patch_index3(text: str) -> str:
    text = text.replace("yAxes:[{", "y:{").replace("xAxes:[{", "x:{")
    text = text.replace("gridLines:", "grid:").replace("fontSize:12", "font:{size:12}")
    text = (
        text.replace("}],x:{", "},x:{")
        .replace("]},responsive", "},responsive")
        .replace("]},maintainAspectRatio", "},maintainAspectRatio")
        .replace("]},elements", "},elements")
        .replace("],responsive", ",responsive")
    )
    text = text.replace(
        "x:{grid:{display:!0}},responsive:!0,maintainAspectRatio:!1}}}",
        "x:{grid:{display:!0}}},responsive:!0,maintainAspectRatio:!1}}",
    )
    text = text.replace(
        'legend:{labels:{fontColor:"rgb(255, 0, 0)"}}',
        'plugins:{legend:{labels:{color:"rgb(255, 0, 0)"}}}',
    )
    # Chart.js v4: min/max живут на scale, а не в ticks.
    text = text.replace("y:{ticks:{min:0,max:100,", "y:{min:0,max:100,ticks:{")
    text = text.replace("y:{ticks:{min:0,max:this.memory.total,", "y:{min:0,max:this.memory.total,ticks:{")
    text = text.replace("y:{ticks:{min:0,", "y:{min:0,ticks:{")
    return text


def patch_index9(text: str) -> str:
    # Два разных Chart-компонента: watcher срабатывает до создания chart.
    text = text.replace(
        "shouldUpdateLabels(){(0,g.default)(this.chartData.labels,this.chart.data.labels)||(this.chart.chart.data.labels=this.chartData.labels)}",
        "shouldUpdateLabels(){const c=this.chart;if(!c||!c.data)return;"
        "(0,g.default)(this.chartData.labels,c.data.labels)||(c.data.labels=this.chartData.labels)}",
    )
    text = text.replace(
        "shouldUpdateLabels(){const f=this.chartData.labels,m=this.chart.data.labels;"
        "(0,g.default)(f,m)||f.forEach(((y,_)=>{m[_]=y}))}",
        "shouldUpdateLabels(){const f=this.chartData.labels,"
        "m=this.chart&&this.chart.data&&this.chart.data.labels;if(!m)return;"
        "(0,g.default)(f,m)||f.forEach(((y,_)=>{m[_]=y}))}",
    )
    text = text.replace(
        "shouldUpdateLabels(){const f=this.chartData.labels,m=this.chart.data.labels;"
        "(0,g.default)(f,m)||f.forEach(((y,_)=>{m[_]=y}))}",
        "shouldUpdateLabels(){const f=this.chartData.labels,"
        "m=this.chart&&this.chart.data&&this.chart.data.labels;if(!m)return;"
        "(0,g.default)(f,m)||f.forEach(((y,_)=>{m[_]=y}))}",
    )
    # Только первый (TcLineChart); TcRadarChart в HEAD не защищён.
    text = text.replace(
        "shouldUpdateData(){const f=this.chartData.datasets,m=this.chart.data.datasets;",
        "shouldUpdateData(){const f=this.chartData.datasets,"
        "m=this.chart&&this.chart.data&&this.chart.data.datasets;if(!m)return;",
        1,
    )
    return text


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Патч PNM: графики статистики на Chart.js v4")
    parser.add_argument("--root", type=Path, default=ROOT, help="корень репозитория")
    args = parser.parse_args()

    index3, index9 = resolve_paths(args.root)
    for path, patch in ((index3, patch_index3), (index9, patch_index9)):
        text = path.read_text(encoding="utf-8")
        updated = patch(text)
        path.write_text(updated, encoding="utf-8")
        print(f"patched {path.relative_to(args.root)} ({len(updated)} bytes)")


if __name__ == "__main__":
    main()
