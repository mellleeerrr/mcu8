# Codian MCU 4200 в редакторе раскладок PNM

Снимки: session layout, ручной режим, 1600×1200.
Каталог эмулирован с `slots` (в запущенном бинаре поля ещё нет — он собран до правки
`crates/vks-admin/src/api_v3/shell.rs:784`); геометрия иконок — из этих `slots`.

- `01-manual-vendor-active.png` — вендорская схема активна: палитра диапазона в левой
  панели, ниже секция «Codian MCU 4200» (54 карточки, 8×7, 52×36).
- `02-manual-codian-selected.png` — выбрана `codian_07` (13 окон): палитра диапазона
  пуста (вендор фильтрует по `rangeMap`, ключа для `codian_*` нет), в предпросмотре
  13 плиток, карточка `codian_07` подсвечена `--active`.

Замеры DOM (Playwright): 54 элемента `[data-source=codian]`, 54 уникальных
`data-codian-id`, 7 визуальных рядов по 8, `preset=codian_07`, `slots=13`.
PATCH-перехват «Применить»: `{mode:"manual",preset:"codian_07",type:"default",slots:13}`.
