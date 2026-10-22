# T11b — статистика: Chart.js v4 (часть)

Дата: 2026-10-22. HEAD: `fc983a3` + этот коммит.

## Исправлено
- В `chunks/index3.bundle-2.2.1.66.js` конфиги 6 графиков переведены с Chart.js v2
  на v4: `yAxes/xAxes` → `y/x`, `gridLines` → `grid`, `fontSize` → `font.size`,
  `legend.labels.fontColor` → `plugins.legend.labels.color`, `responsive` и
  `maintainAspectRatio` вынесены из `scales`.
- Живая проверка `/pnm/administration/statistics`: **0 console errors**
  (было 5 `Invalid scale configuration for scale: yAxes/xAxes/responsive`),
  рисуются все 6 виджетов, оси и легенды корректны, скриншот
  `/tmp/stats-statistics.png`.

## Остаётся (не закрыто)
- RTP/QoE-метрики из `engine/abonents` — после T09, пока нет источника.
- `workers/resources` в `/api/v3/engine/diagnostics` остаются честными нулями
  (SFU без транскодинга), но не «живыми» RTP-метриками; это отдельная часть T11b.
