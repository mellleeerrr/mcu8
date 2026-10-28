# vks-layout — движок раскладок

Самодостаточное ядро движка раскладок VKS: модель раскладки + чистая
математика расстановки + рендерер (план композиции). Используется и
MCU-сервером (пиксельные rect), и клиентом (проценты от canvas).

Фаза 4 (DEVELOPMENT_PLAN.md). ADR-0012. Источник:
`docs/architecture/LAYOUT_ENGINE.md`.

## Что это

Раскладка — абстрактное, рендер-независимое описание: набор **слотов**
с пиксельными rect на канвасе и привязкой участника. Один и тот же
`Layout` применяется в любом режиме — это ключевой инвариант
MCU/SFU-прозрачности.

## Модель (core)

- `Layout { version, kind, slots }` — версия инкрементируется на каждое
  изменение (backward compat).
- `LayoutKind` — `Grid | Focus | Speaker | Custom`.
- `Slot { id, participant_id, rect }` — слот; `participant_id: None` =
  свободный (авто-назначение).
- `ParticipantId` / `SlotId` — opaque идентификаторы (самодостаточны,
  dep-light: без `vks-core`).

## Геометрия (rect)

`Rect { x, y, w, h }` — **пиксели** (u32) на `Canvas { width, height }`.
Пиксели — «общая валюта»: клиент масштабирует rect в проценты, MCU —
как есть. Helpers: `area`, `center`, `contains` (half-open), `intersects`,
`clamp`.

## Расстановки (arrangements)

| Расстановка | Алгоритм |
|---|---|
| **Grid** (`GridLayout`) | N тайлов в балансированную сетку (rows/cols выбираются минимизацией аспект-искажения). Детерминировано. Неполный последний ряд сохраняет ширину колонки и центрируется; полные ряды занимают всю ширину. Исключение — холст уже сетки (`w / cols == 0`): там ряд делит свою ширину; положительную ширину гарантирует последний тайл ряда, остальные на таком холсте могут иметь нулевую ширину. |
| **Focus** (`FocusLayout`) | Фокус-участник — крупная область (верх 75% canvas, вся ширина). Остальные — полоса снизу (25%), равномерно. |
| **Speaker** | `LayoutKind::Speaker` — семантически аналогичен Focus (1 крупный центр + thumbnails); в v1 реализован через тот же фокус-математик. |
| **Custom** | Пользовательская (редактор, drag-and-drop slots) — `slots` заполняются внешним кодом. |

### Invariants

- Grid: rects не пересекаются; полные ряды покрывают ширину, неполный
  последний ряд центрируется (`total area == canvas.size` при `n % cols == 0`
  либо на холсте уже `cols`, где ряд делится по ширине).
- Focus: фокус > 50% площади; остальные мелкие; `total area == canvas`.
- Все rect внутри canvas.

## Рендерер (renderer)

`Renderer::render(canvas, &Layout, &HashMap<ParticipantId, FrameMeta>)
→ Vec<DrawCommand>`. `DrawCommand { participant_id, rect, scale }` —
план композиции, который потребляет GL/wasm-рендерер (MCU compositor)
или CSS/Taffy (клиент). **No pixel ops** — только геометрия + uniform
fit-scale (letterbox). Свободные слоты и слоты без кадров пропускаются.

## Как MCU и клиент потребляют

- **MCU** (сервер, compositor): `rect` в пикселях → прямые координаты
  на целевом canvas.
- **Клиент** (SFU, браузер): `rect` → проценты: `left% = x/canvas.w`,
  `top% = y/canvas.h`, `width% = w/canvas.w`, `height% = h/canvas.h` →
  CSS absolute positioning (v1) / Taffy (v2).
- Scale применяется при композиции кадра (uniform fit, letterbox).

## Зависимости

dep-light: `serde` (workspace), `thiserror` (workspace). Dev: `serde_json`
(только для тестов). Нет `unsafe`.

## Тесты

```
cargo test -p vks-layout
```

Покрытие: grid (n=1..100, балансировка, полные ряды покрывают ширину / неполный центрируется, без пересечений),
focus (фокус >50%, мелкие остальные, покрытие canvas), renderer
(4-person grid → 4 DrawCommand, letterbox scale, skip unassigned).
