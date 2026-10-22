# T11b — статистика: Chart.js v4 (часть)

Дата: 2026-10-22. HEAD: `1109508` + этот коммит.

## Исправлено
- 6 line-конфигов переведены с Chart.js v2 на v4: `yAxes/xAxes` → `y/x`,
  `gridLines` → `grid`, `fontSize` → `font.size`,
  `legend.labels.fontColor` → `plugins.legend.labels.color`,
  `responsive`/`maintainAspectRatio` вынесены из `scales` в корень options,
  `min/max` перенесены с `ticks` на уровень `scales.y`.
- В `index9.bundle` оба варианта `shouldUpdateLabels` (TcLineChart/TcBarChart)
  и `shouldUpdateData` TcLineChart защищены от `chart === undefined` —
  переключение памяти pie↔line больше не даёт `TypeError: Cannot read
  properties of undefined (reading 'data')`.
- Правка воспроизводима байт-в-байт: `scripts/patch-pnm-chartjs.py` из
  pristine `1109508` даёт текущие index3/index9 (sha256 `8b49d3…`/`f61ec9…`),
  совпадающие с `MANIFEST.sha256`.
- Живой Playwright (Chromium 1243, 1600×1200):
  `/pnm/administration/statistics` — **0 console errors, 0 pageerrors,
  0 HTTP>=400** в pie-режиме и после клика memory→line; 6 карточек/canvas.
  Оси: CPU/resources/workers 0..100%, network 0..150 (auto), memory line
  0..58 Gb. Скриншоты `/tmp/stats-pie-fixed.png`, `/tmp/stats-line-fixed.png`.
- Другие страницы (sessions, conferences, addressbook, history, statistics) —
  без console/page/HTTP ошибок.

## Остаётся (не закрыто)
- RTP/QoE-метрики из `engine/abonents` — после T09, пока нет источника.
- `workers/resources` в `/api/v3/engine/diagnostics` остаются честными нулями
  (SFU без транскодинга), но не «живыми» RTP-метриками; это отдельная часть T11b.
- `index-legacy3/9` не пропатчены — legacy-fallback для старых браузеров не
  поддерживается в живом Chromium-прогоне; при необходимости применить
  `scripts/patch-pnm-chartjs.py` и к ним.
