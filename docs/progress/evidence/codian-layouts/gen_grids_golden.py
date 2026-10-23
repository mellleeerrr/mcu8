#!/usr/bin/env python3
"""Перегенерация golden геометрии конструктора (`vks_grid_*` / `vks_pin_*`).

Golden — **снимок текущей реализации** `grids::geometry` на холсте 1280×720,
а не независимый оракул (так же устроен `codian_geometry_golden.rs`). Он нужен,
чтобы сокращение геометрии (r6-elegance) не меняло пиксели незаметно. Поэтому
скрипт запускает ignored-тест, который пишет fixture **тем же кодом**, что и
production; независимые инварианты проверяет `tests/grids.rs`.

Порядок:
  1. осознанно изменили `crates/vks-layout/src/grids.rs`;
  2. `python3 docs/progress/evidence/codian-layouts/gen_grids_golden.py`;
  3. глазами проверили diff `crates/vks-layout/tests/fixtures/grids_geometry_1280x720.json`;
  4. `cargo test -p vks-layout --locked`.

Запускать из любого каталога; cargo вызывается из корня workspace.
"""

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
FIXTURE = ROOT / "crates/vks-layout/tests/fixtures/grids_geometry_1280x720.json"
COMMAND = [
    "cargo",
    "test",
    "-p",
    "vks-layout",
    "--locked",
    "--test",
    "grids_geometry_golden",
    "regenerate_golden",
    "--",
    "--ignored",
    "--exact",
    "--nocapture",
]


def main() -> int:
    print("$", " ".join(COMMAND), f"(cwd={ROOT})", flush=True)
    result = subprocess.run(COMMAND, cwd=ROOT, check=False)
    if result.returncode != 0:
        print("golden НЕ обновлён: тест завершился с ошибкой", file=sys.stderr)
        return result.returncode

    data = json.loads(FIXTURE.read_text(encoding="utf-8"))
    windows = sum(len(cells) for cells in data.values())
    print(f"обновлён {FIXTURE.relative_to(ROOT)}: {len(data)} id, {windows} окон")
    print("проверьте diff: эталон должен меняться только там, где это осознанно")
    return 0


if __name__ == "__main__":
    sys.exit(main())
