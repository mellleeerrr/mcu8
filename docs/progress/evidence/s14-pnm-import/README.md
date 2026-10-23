# S14 — импорт PNM: fail-closed патчи, сохранение проектного слоя, атомарная подмена дерева

Дата: 2026-10-23. Задачи: P1-1 (скрипт теряет `<script>`-теги), P1-2
(`patch-pnm-chartjs.py` не падает и не идемпотентен), P1-5 (наполовину
импортированное дерево).

## Что было не так

1. **P1-1.** `import-pnm-frontend.sh` сохранял из `index.html` только блок
   `<style id="prostor-pnm-overrides">`. Проектные файлы `api-errors.js` и
   `perf-overrides.js` восстанавливались, но **теги `<script src>` на них
   терялись** при копировании вендорского дерева 1:1 ⇒ правки T12b/T15
   (перехват console/fetch) молча откатывались, хотя импорт завершался «успешно».
2. **P1-2.** `patch-pnm-chartjs.py` правил бандл цепочкой `str.replace` без
   проверок: если вендорский минификатор менял строки, скрипт печатал
   `patched ...` и выходил с кодом 0, а графики оставались сломанными.
   Идемпотентность не проверялась. Второй патч (`patch-pnm-audio-in-layouts.py`)
   при этом уже был fail-closed — то есть два скрипта одного конвейера вели
   себя по-разному (дублирование логики, ~60 строк).
3. **P1-5.** `rm -rf "$DEST"` + `tar` поверх: падение распаковки на середине
   оставляло рантайм без части чанков и без манифеста.

## Что сделано

| Что | Где |
|---|---|
| Общее ядро патчей: правила с якорями `(name, old, new, expected_old, expected_new[, limit])`, `apply_rules` (fail-closed, «уже применено», `--check`), атомарная запись, `node --check` с откатом | `scripts/pnm_patch_lib.py` (новый, 160 строк) |
| `patch-pnm-chartjs.py` переписан на ядро: 13 правил index3 + 3 правила index9 с точными якорями и `--check`; удалён дублирующий блок правил index9 | `scripts/patch-pnm-chartjs.py` (95 → 160 строк, из них 80 — таблица якорей) |
| `patch-pnm-audio-in-layouts.py` переведён на общее ядро: удалены локальные копии `apply_rules`/`atomic_write`/`node_syntax_ok` | `scripts/patch-pnm-audio-in-layouts.py` (424 → 340 строк) |
| Проектный слой `<script src>` сохраняется и восстанавливается **блоком с маркерами** `<!-- prostor:project-layer:start/end -->` (байт-в-байт, без эвристик вставки) | `scripts/import-pnm-frontend.sh` |
| Атомарная подмена дерева: распаковка в `deployed/.pnm-stage`, проверка `index.html`, `rm -rf` + `mv` | `scripts/import-pnm-frontend.sh` |
| 9 unit-тестов ядра (идемпотентность, fail-closed на дрейфе, `--check`, откат при битом синтаксисе, limit) | `scripts/test_pnm_patch_lib.py`, шаг CI `Unit tests (PNM patch lib)` |

## Доказательства (команды и результаты)

Свежий вендор распакован в песочницу (архив без central directory → потоковое
восстановление, как в самом импорте): 482 файла, 1 пропущен.

```
$ python3 scripts/patch-pnm-chartjs.py --root <песочница>
  index3: применено 13 правил, ЗАПИСАНО (653952 → 653868 байт); node --check: OK
  index9: применено 3 правила,  ЗАПИСАНО (317107 → 317250 байт); node --check: OK
$ python3 <старый скрипт> --root <другая копия>
$ cmp index3.bundle-2.2.1.66.js → ИДЕНТИЧНО  (sha256 6eef6de023d4da70…)
$ cmp index9.bundle-2.2.1.66.js → ИДЕНТИЧНО  (sha256 f61ec91d67fcefaf…)
```

То есть новый скрипт даёт **байт-в-байт тот же результат**, что и прежний, но
теперь проверяет якоря.

```
$ python3 scripts/patch-pnm-chartjs.py --root <пропатченная копия>      # повтор
    уже применено: axes-y … (все 16 правил); exit=0, файл не изменён
$ python3 scripts/patch-pnm-chartjs.py --root <пропатченная копия> --check
    exit=0
$ python3 scripts/patch-pnm-chartjs.py --root <свежий вендор> --check
    exit=1   («требует патча»)
$ python3 -m unittest -v test_pnm_patch_lib
    Ran 9 tests … OK
```

Аудио-патч после перевода на общее ядро: `index6`/`index-legacy6` байт-в-байт
совпадают с выводом прежней версии скрипта.

Сквозной прогон импорта в песочнице (`scripts/` + архив + копия текущего
рантайма, `--force`, дважды подряд):

```
импорт 1: exit=0; index.html: блок prostor:project-layer вставлен;
          теги api-errors.js + perf-overrides.js стоят ДО вендорского module-бандла
импорт 2: exit=0; блок заменён (не задублирован): ровно 2 строки маркеров
CSS-override: 2 вхождения (сохранён)
deployed/.pnm-stage: не остался
манифест: 212 строк, совпадает с деревом (find+sha256sum | diff - → пусто)
```

## Важно: текущий рантайм НЕ воспроизводится из архива в репозитории

`diff -rq` текущего `client/pnm-frontend/deployed/pnm` против «вендор из
`archive/PNM-Frontend-20261016.zip` + патчи» даёт **91 расхождение**: 62 png,
24 js-чанка, `prostor-wordmark.svg`, `index.html`, `manifest.webmanifest`, `ico`.
Это слой **PROSTOR-ребренда** (`frontend/06-rebrand`, вне репозитория) плюс,
возможно, более новый вендорский билд: в реальных чанках есть строки `PROSTOR`,
в архиве — нет; размеры чанков отличаются на ~110 байт.

Следствие: **`import-pnm-frontend.sh --force` на рабочем дереве сотрёт ребренд**
(скрипт об этом предупреждает). Перед импортом нужно применять слой ребренда,
как описано в `client/pnm-frontend/NOTES.md`, «Как обновлять». Поэтому сквозной
прогон выполнен в песочнице, а рабочее дерево не перезаписывалось.

`MANIFEST.sha256` при этом **соответствует** текущему дереву (проверено
`find . -type f | sha256sum | diff - ../../MANIFEST.sha256` → пусто).

Проверка якорей на **рабочем** (ребренженном) дереве, без записи:

```
$ python3 scripts/patch-pnm-chartjs.py --check
    БЕЗ ИЗМЕНЕНИЙ: index3.bundle-2.2.1.66.js уже в нужном состоянии
    уже применено: labels-watcher-no-chart / labels-watcher-radar / data-watcher-line-only
    БЕЗ ИЗМЕНЕНИЙ: index9.bundle-2.2.1.66.js уже в нужном состоянии   exit=0
$ python3 scripts/patch-pnm-audio-in-layouts.py --check
    … уже применено (все правила) … CSS-override найден в index.html   exit=0
```

То есть правила не зависят от байтов ребренда: якоря Chart.js-конфигов и
минифицированных методов ребренд не задевает.
