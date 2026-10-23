# Стенд проектного слоя Codian (headless chromium, file://)

Команда:
```
~/.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell \
  --no-sandbox --disable-gpu --allow-file-access-from-files \
  --user-data-dir=/tmp/codian-harness-profile --virtual-time-budget=8000 \
  --dump-dom file:///.../frontend-harness.html
```

Стенд детерминированный: синхронные `requestAnimationFrame` и `fetch`, мок pinia-стора
`layout-editor`, реальный ответ каталога (снят с рабочего сервера, встроен в `frontend-harness.html`).
Проверяется: число карточек, число SVG, заголовок группы, `viewBox`, геометрия зазоров,
подписи, выбор схемы (preset + slots), активный класс, ошибки консоли.
Слой подключается как `codian-layouts.js` (копируется рядом со стендом из
`client/pnm-frontend/deployed/pnm/`).

## Результат прогона (JSON из `<pre id="result">`)

```json
{"installed":true,"items":54,"svgs":54,"header":true,"groupInPanel":false,"vendorPaletteIntact":true,"viewBox_01":"0 0 1280 720","viewBox_04":"0 0 1280 720","rects_04":[[0,307],[333,294],[653,294],[973,307],[0,307]],"titles":["Полный экран — 1 окно","Сетка 2×2 — 4 окна","Сетка 3×3 — 9 окон"],"windows":["1","4","9"],"clicked":"codian_07","active":"layout-scheme-selector-item layout-scheme-selector-item--active","calls":[["preset","codian_07"],["slots","codian_07",13]],"errors":[]}
```

`groupInPanel=false` — ожидаемо: по итогам живой проверки (Playwright, 1600×1200) группа
вставляется секцией на полную ширину под `.base-layout-configurator`, а не в левую панель
300 px: 54 карточки в панели поднимают её до ~694 px и ломают 16:9 предпросмотра, а 8 в ряд
сжимаются до ~26 px. `items=54`, `svgs=54`, `viewBox="0 0 1280 720"` и зазор 26 канвас-px
между ячейками (правый край 1280 flush) подтверждают геометрию иконок.
