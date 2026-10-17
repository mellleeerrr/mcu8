# PNM Frontend — провенанс и правила работы

## Источник

- Оригинал: `archive/PNM-Frontend-20261016.zip` (30 932 992 B, 484 записи).
- sha256: `ea0314f85951a9abd9dcce3c9c6dfa79943941d36c03c64dde60a6ead23cbdfa`.
- Содержимое: собранный TrueConf MCU Web UI v2.2.1-66 (Vue 3.5 + vue-router 4 (history, base `/pnm/`)
  + Pinia + axios + chart.js + video.js), сборка Vite (modern + `-legacy`). Сборка уже пропатчена
  стороной заказчика: брендинг «PROSTOR» в шапке, ссылка «Панель управления» (`href="/admin/"`)
  и встроенный `<style id="prostor-pnm-nav-header-styles">`; футер «TrueConf MCU © 2026 АО «ТКС»».
  Полный ребрендинг рантайма (TrueConf MCU → PROSTOR) интегрирован 2026-10-17 — см. §«Ребренд PROSTOR».

## Восстановление архива

Архив **повреждён**: central directory отсутствует (`unzip`/python `zipfile` не открывают его).
`scripts/import-pnm-frontend.sh` восстанавливает потоки по локальным заголовкам `PK\x03\x04`
(потоковый inflate, битые записи пропускаются). Результат:

- восстановлено 482 файла из 483 файловых записей;
- не читается ровно один файл — `source/extracted_frontend/raw_decompiled/legacy/index-legacy.bundle-2.2.1.66/index-legacy.js`
  (обрыв deflate-потока) — это **дубликат** рантайм-чанка `deployed/pnm/chunks/index-legacy.bundle-2.2.1.66.js`
  в каталоге реконструкции исходников; на работу UI не влияет;
- **весь `deployed/pnm` восстановлен полностью** (216/216 записей).

## Что импортировано

- `deployed/pnm/` — рантайм 1:1, **210 файлов** (26 MB). Формула состава: **замыкание ссылок
  `index.html` (+ `LICENSE.txt`)** — при переимпорте сверять. Из 216 записей исключены 6 служебных
  бэкапов ручных правок: `chunks/index.bundle-2.2.1.66.js.bak.dbg2`, `.bak.step3`,
  `chunks/InfoMainDescription*.js.bak.{hotfix,2026-…}` и т.п. (полный список — см. вывод импорта).
- Legacy-сборка (`chunks/*-legacy.bundle-*`): **32 файла / 6 289 441 B** (~41 % `chunks`, ~22 %
  рантайма) — входит в замыкание `index.html` (три `nomodule`-ссылки), сохранена ради 1:1;
  стоимость принята осознанно (ADR-0027).
- `MANIFEST.sha256` — контрольные суммы каждого файла рантайма.
- `source/extracted_frontend/` **не распаковывается по умолчанию** (44 MB реконструкции; при
  необходимости: `./scripts/import-pnm-frontend.sh --with-source`).
- Git: репозиторий инициализирован. `archive/*.zip` (31 MB) **не трекается** (`.gitignore`;
  файл остаётся на диске для `scripts/import-pnm-frontend.sh`; из истории baseline-коммита
  удалён перезаписью); `deployed/` (26 MB) — в репо (это продукт); `source/` и `*.gz` —
  в `.gitignore`.

## Ребренд PROSTOR (2026-10-17)

Полный ребрендинг рантайма «TrueConf MCU» → PROSTOR применён из слоя
`frontend/06-rebrand/pnm-rebranded-2026-10-17` (мастерская аналитики; методика и правила
замен — `REBRAND.md` там же). В это дерево интегрированы 90 изменённых файлов
(26 текстовых + 63 иконки + словник) через `rsync -c`; затем применены 2 пост-правки,
найденные при ревью:

1. **Локали — 12 «мёртвых» ключей × 4 бандла.** Ребренд заменил call-site-строки
   `"MCU …"` → `"PROSTOR …"`, но не тронул i18n-ключи вида `"<context>\u0004MCU …"`
   (граница слова `\b` не срабатывает после цифры `4`). Итог: переводы этих 12 строк
   не находились. Заменено `u0004MCU` → `u0004PROSTOR` в `ru_RU`/`en_US` (+legacy).
   Машинная проверка: в JS call-sites `"MCU …"` — 0, `"PROSTOR …"` — 50 вхождений
   (28 `$pgettext`-вызовов, 14 уникальных строк);
   в локалях — по 16 ключей `u0004PROSTOR` на файл, `u0004MCU` — 0.
2. **Cache-bust словника.** `assets/images/**` раздаётся с `Cache-Control: immutable`,
   а словник ребрендом перезаписан → старое имя закэшировалось бы на год.
   `BiOEwveA.svg` → `assets/images/prostor-wordmark.svg`; ссылки обновлены в `index8`
   и `index-legacy8`; старый URL не занят. При будущих правках словника — снова менять имя.

После интеграции: `MANIFEST.sha256` пересобран командой импорт-скрипта
(`find … | sort -z | xargs -0 sha256sum`, 210 строк, `sha256sum -c` — OK);
`node --check` на изменённых бандлах — OK.

Инварианты whitelist соблюдены (`LICENSE.txt`, `TRUECONF`-тексты EULA, `trueconf.*`-контракты,
`TrueConfMCU*`-ключи localStorage, `/mcu/`-пути). Не входит (отдельные фазы, см. `REBRAND.md`):
цветовая тема `#0097A7` → PROSTOR-синий, localStorage-ключи, правовые тексты,
официальный оутлайн-словник.

## Как обновлять

```bash
VKS_PNM_ARCHIVE=/path/to/new.zip ./scripts/import-pnm-frontend.sh --force [--with-source] [--gzip]
```

Проверка целостности после импорта:

```bash
cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256
```

## Правки рантайма и кэш (важно)

- Чанки **не содержат content-hash** (`index.bundle-2.2.1.66.js` — это версия сборки, не хэш),
  исторически правились in-place. Поэтому сервер отдаёт `chunks/**` с `Cache-Control: no-cache`
  (клиент ревалидирует; `Last-Modified`/304 — от `ServeDir`), а `immutable` — только реально
  хэшированные `assets/fonts/**`, `assets/images/**`, `assets/*.webm`.
- Любая ручная правка файла обязана сопровождаться cache-bust: обновить ссылку в `index.html`
  (`…?v=<версия-правки>`) или имя файла. Иначе у клиента до года будет старый JS.
- `--gzip` генерирует предсжатые `.gz` (их читает `tower-http::ServeDir::precompressed_gzip`);
  в оригинальном архиве `.gz` нет. Ответы статики несут `Vary: Accept-Encoding` (обязательно
  при gzip-представлениях).
- CSP вычисляется сервером **при старте** из `index.html` (sha256 всех inline-скриптов) — правки
  inline-скриптов политику не ломают; если в `index.html` появятся внешние домены
  (`https://cdn…`), их нужно добавить в `frontend.rs::build_csp`.

## Политика симлинков

Рантайм `deployed/pnm/` **не содержит симлинков** — это инвариант, его проверяет тест
`runtime_tree_has_no_symlinks` (`crates/vks-server/src/frontend.rs`). Причина: `tower-http::ServeDir`
следует по симлинкам, поэтому ссылка внутри дерева = потенциальный escape за пределы корня статики.
`scripts/import-pnm-frontend.sh` при импорте удаляет все симлинки из скопированного дерева
(каждый удалённый — в логе импорта). Если в новой версии архива появятся симлинки — их появление
будет видно в логе импорта; дерево всё равно останется без симлинков.

## Лицензии

`LICENSE.txt` в рантайме — проприетарная лицензия TrueConf MCU; бренд «TrueConf» и футер «АО «ТКС»»
присутствуют в UI. Решение о правомерности использования кода в продукте VKS — за командой
(см. ADR-0027, риск R2 плана `docs/progress/FRONTEND_PORT_PLAN.md`).
