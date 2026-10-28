#!/usr/bin/env python3
"""Защита вендорского UI от пустого списка «Общих» раскладок.

Симптом (живой стенд): при обновлении таблицы участников страница сессии
падает с

    TypeError: can't access property "some",
               this.sessionLayoutListByType.default is undefined

в `hasIndividualLayout` (`chunks/index6.bundle-2.2.1.66.js`).

Причина именно в вендоре: computed `sessionLayoutListByType` строит объект
только по фактически пришедшим раскладкам, а `hasIndividualLayout` и
`SessionBroadcastPopup.created` обращаются к `.default`/`.individual` без
проверки. Пока сессия гарантированно имеет «Общую» (default) раскладку,
баг не виден; любое состояние без неё — включая первые миллисекунды загрузки
страницы, когда `layoutList` ещё пуст, — роняет рендер.

Серверная часть инварианта («в сессии всегда есть default», тип раскладки не
меняется при редактировании) чинится в Rust. Этот патч — страховка уровня
представления: он не даёт UI упасть на любом промежуточном состоянии и
заодно закрывает такой же незащищённый доступ в попапе трансляции.

Правила (современный и legacy-бандл):
  1. `hasIndividualLayout`: `default` → `(default || [])`.
  2. `SessionBroadcastPopup.created`: `default[0]`/`individual[0]` →
     `(default || [])[0]`/`(individual || [])[0]`.

Запуск:
    python3 scripts/patch-pnm-layout-crash.py             # применить
    python3 scripts/patch-pnm-layout-crash.py --check     # только проверить
    python3 scripts/patch-pnm-layout-crash.py --root DIR  # другой рантайм
    python3 scripts/patch-pnm-layout-crash.py --modern-only

Свойства — как у остальных патчей PNM (см. `scripts/pnm_patch_lib.py`):
идемпотентность, fail-closed при дрейфе вендора, атомарная запись, откат при
битом синтаксисе, `node --check`.
"""
from __future__ import annotations

import argparse
import hashlib
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from pnm_patch_lib import apply_rules, node_syntax_ok  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
CHUNKS_REL = "client/pnm-frontend/deployed/pnm/chunks"
MODERN = "index6.bundle-2.2.1.66.js"
LEGACY = "index-legacy6.bundle-2.2.1.66.js"

# Якоря одинаковы в modern/legacy: минификатор сохранил имена методов и
# порядок обращений. Число вхождений жёстко зафиксировано: если вендор
# пересоберёт чанк, патч обязан упасть, а не «молча ничего не сделать».
HAS_INDIVIDUAL_OLD = "hasIndividualLayout(){return!this.sessionLayoutListByType.default.some("
HAS_INDIVIDUAL_NEW = "hasIndividualLayout(){return!(this.sessionLayoutListByType.default||[]).some("

COMMON_RULES = [
    ("hasIndividualLayout: guard default", HAS_INDIVIDUAL_OLD, HAS_INDIVIDUAL_NEW, 1, 1),
]

# Попап трансляции: modern использует временную переменную `e` и `==null?`,
# legacy — опциональную цепочку `?.`. Якоря разные, смысл правки один.
BROADCAST_MODERN_OLD = (
    "this.currentLayoutId=(e=this.activeBroadcastLayout||this.sessionLayoutListByType.default[0]"
    "||this.sessionLayoutListByType.individual[0])==null?void 0:e.layoutId"
)
BROADCAST_MODERN_NEW = (
    "this.currentLayoutId=(e=this.activeBroadcastLayout"
    "||(this.sessionLayoutListByType.default||[])[0]"
    "||(this.sessionLayoutListByType.individual||[])[0])==null?void 0:e.layoutId"
)
BROADCAST_LEGACY_OLD = (
    "this.currentLayoutId=(this.activeBroadcastLayout||this.sessionLayoutListByType.default[0]"
    "||this.sessionLayoutListByType.individual[0])?.layoutId"
)
BROADCAST_LEGACY_NEW = (
    "this.currentLayoutId=(this.activeBroadcastLayout"
    "||(this.sessionLayoutListByType.default||[])[0]"
    "||(this.sessionLayoutListByType.individual||[])[0])?.layoutId"
)

MODERN_RULES = COMMON_RULES + [
    ("broadcast popup: guard default/individual", BROADCAST_MODERN_OLD, BROADCAST_MODERN_NEW, 1, 1),
]
LEGACY_RULES = COMMON_RULES + [
    ("broadcast popup: guard default/individual", BROADCAST_LEGACY_OLD, BROADCAST_LEGACY_NEW, 1, 1),
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=str(ROOT), help="корень репозитория (по умолчанию — этот)")
    parser.add_argument("--check", action="store_true", help="только проверить, ничего не писать")
    parser.add_argument("--modern-only", action="store_true", help="патчить только современный бандл")
    args = parser.parse_args()

    chunks = Path(args.root) / CHUNKS_REL
    targets = [(MODERN, MODERN_RULES)]
    if not args.modern_only:
        targets.append((LEGACY, LEGACY_RULES))

    ok_all = True
    for name, rules in targets:
        path = chunks / name
        print("  %s" % path)
        ok, messages = apply_rules(path, rules, check_only=args.check)
        for message in messages:
            print(message)
        ok_all = ok_all and ok

    if ok_all and not args.check:
        # Патч меняет байты вендорского рантайма → MANIFEST.sha256 обязан
        # пересчитаться. Делаем это здесь же, иначе `sha256sum -c` краснеет
        # до следующего полного импорта.
        manifest = Path(args.root) / "client/pnm-frontend/MANIFEST.sha256"
        runtime = chunks.parent
        if manifest.parent.is_dir() and runtime.is_dir():
            digest = hashlib.sha256()
            lines = []
            for path in sorted(p for p in runtime.rglob("*") if p.is_file()):
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                rel = path.relative_to(runtime).as_posix()
                lines.append("%s  ./%s\n" % (digest, rel))
            manifest.write_text("".join(lines), encoding="utf-8")
            print("  манифест пересобран: %s строк" % len(lines))

    # Проверка синтаксиса после записи уже выполнена внутри apply_rules; для
    # --check дополнительно убеждаемся, что итоговые файлы читаются node.
    if args.check:
        for name, _rules in targets:
            syntax_ok, syntax_msg = node_syntax_ok(chunks / name)
            print("    %s: %s" % (name, syntax_msg))
            ok_all = ok_all and syntax_ok

    return 0 if ok_all else 1


if __name__ == "__main__":
    sys.exit(main())
