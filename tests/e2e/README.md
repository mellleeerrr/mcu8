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
- (A2) проверяет, что аватар участника в плитке НЕ отображается: принимается
  любой механизм скрытия (`display:none`, `visibility:hidden`, `opacity:0`,
  нулевой bbox, обрезка/вынос из плитки, отсутствие в DOM) — и что подпись при
  этом ничем не отжимается вниз (0 em вместо 1.335 em). Плюс негативные
  контроли: аватары ВНЕ плитки (панель видеслота, таблица «Получатели», шапка)
  обязаны остаться видимыми, иконка роли в пустой плитке не должна сдвинуться,
  а плитка с превью кадра (MediaPreview 24×24, где аватара в DOM нет) — тоже
  центрирована;
- (A3) проверяет, что аватар убран **из потока**, а не просто «невидим»: его
  бокс должен иметь нулевую площадь (иначе `visibility:hidden`, `opacity:0` и
  скрытие только внутреннего `.tc-avatar` прошли бы как «скрыто», оставив дыру
  2em);
- (A4) проверяет, что страховка плитки с превью **живая**: сдвиг превью-плитки
  должен отличаться от сдвига обычной ровно на `12px + .335em` (половина
  превью 24px + половина gap .67em) — иначе правило умерло молча;
- (A5) проверяет, что подпись не переполняется по вертикали, а горизонтальная
  обрезка длинного имени идёт **только** через вендорский `ellipsis` и не
  выходит за плитку;
- (B) сверяет счётчики списка мониторинга («Участники (SIP/H.323)»,
  «Устройства (VNC/RTSP) и медиафайлы») с `/api/v3/ui/conferences`, с
  `/api/v3/conferences/{id}/contacts` и с шапкой карточки комнаты.

```bash
VKS_BASE_URL=http://127.0.0.1:8080 \
VKS_LOGIN=admin VKS_PASSWORD=admin_password \
NODE_PATH=tests/e2e/node_modules node tests/e2e/label-zoom.js
```

Два служебных режима (нужны, чтобы харнесс сам себя проверял):

```bash
# прогон с кандидатом фикса, инъектированным в страницу (index.html не трогаем):
VKS_APPLY_CANDIDATE=1 node tests/e2e/label-zoom.js     # ожидается exit 0
# самопроверка: откатываем фикс (аватар снова виден) — прогон ОБЯЗАН упасть:
VKS_REVERT_CHECK=1 node tests/e2e/label-zoom.js         # ожидается exit 1
```

`CANDIDATE_CSS` в `label-zoom.js` обязан совпадать с продом
(`client/pnm-frontend/deployed/pnm/index.html`, блок `#prostor-pnm-overrides`),
иначе режим `VKS_APPLY_CANDIDATE=1` проверяет не то, что реально раздаётся.

Переменные: `VKS_CONFERENCE_ID` (по умолчанию — комната с контактом
«T12b Label Probe»), `VKS_PARTICIPANT`, `VKS_ARTIFACTS` (по умолчанию
`artifacts/e2e-label-zoom`), `VKS_CHROMIUM`, `VKS_APPLY_CANDIDATE`,
`VKS_REVERT_CHECK`. Коды выхода: `0` — все проверки прошли, `1` — провалена
проверка, `2` — прогон упал (логин/страница/нет контакта в комнате).
Скрипт ничего не пишет в живую БД: «Создать» не нажимается, единственный
не-GET запрос — `POST /api/v3/ui/video_layouts/calculate`.

