# Live E2E (Playwright API)

`live.js` uses the `playwright` Node API directly (not the `@playwright/test`
runner), because the scenario is a live browser session against a running
stand, not a fixture-based test suite. There is no `playwright.config.ts` and
`npx playwright test` is not used.

## Install

```bash
cd tests/e2e
npm ci
npx playwright install chromium
```

`npm test` runs `node live.js`. The script auto-detects a local Chromium only
when `VKS_CHROMIUM` is unset; override it if your cache path differs.

## Run

```bash
VKS_BASE_URL=http://127.0.0.1:8080 \
VKS_LOGIN=admin VKS_PASSWORD=admin_password \
VKS_SIP_URI=sip:probe-e2e@192.168.1.132 \
npm test
```

Artifacts go to `VKS_ARTIFACTS` (default `artifacts/e2e`): `result.json`,
`network.json`, screenshots, `trace.zip`. The script creates and removes its
own test rooms; it requires the live server and the remote SIP terminal.
Set `VKS_PASSWORD` for non-dev configs.

## `label-zoom.js` — подпись в плитке раскладки + счётчики мониторинга

Живой измеритель (Playwright API) без внешнего SIP-терминала:

- (A) открывает страницу раскладок конференции, перебирает все размеры
  раскладки (1, 2, 3, 4, 5+, 8, 9, 10+, 25, 34+), заново сажает контакт в
  ячейку 1 (при смене размера посадка сбрасывается) и меряет подпись
  `.layout-grid-slot-dn__text` относительно плитки: font-size, bbox,
  `dx`/`dy` (центровка), попадание внутрь плитки, переполнение;
- (B) сверяет счётчики списка мониторинга («Участники (SIP/H.323)»,
  «Устройства (VNC/RTSP) и медиафайлы») с `/api/v3/ui/conferences`, с
  `/api/v3/conferences/{id}/contacts` и с шапкой карточки комнаты.

```bash
VKS_BASE_URL=http://127.0.0.1:8080 \
VKS_LOGIN=admin VKS_PASSWORD=admin_password \
NODE_PATH=tests/e2e/node_modules node tests/e2e/label-zoom.js
```

Переменные: `VKS_CONFERENCE_ID` (по умолчанию — комната с контактом
«T12b Label Probe»), `VKS_PARTICIPANT`, `VKS_ARTIFACTS` (по умолчанию
`artifacts/e2e-label-zoom`), `VKS_CHROMIUM`. Коды выхода: `0` — все
проверки прошли, `1` — провалена проверка, `2` — прогон упал (логин/страница).
Скрипт ничего не пишет в живую БД: «Создать» не нажимается, единственный
не-GET запрос — `POST /api/v3/ui/video_layouts/calculate`.

