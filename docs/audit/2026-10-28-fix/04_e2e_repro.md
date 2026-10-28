# T1 — E2E-репро `sessionLayoutListByType.default` и доказательство фикса

Дата: 2026-10-28, 18:00–18:12 MSK. Стенд: vks-server PID 1814143 (release, sha256 бинаря
`target/release/vks-server`, mtime 18:00:21), конфиг `configs/default.toml`, UI `/pnm/`
(вендор 2.2.1.66 + project-слой). Playwright 1.64.1 / chromium-1243, SIP-абонент —
`scripts/bench/sipbot.py` через реальный SIP UAS стенда.

Скрипт: [tests/e2e/repro_session_default_layout.js](../../../tests/e2e/repro_session_default_layout.js).
Своя комната на прогон (`E2E Crash <ts>`), sipbot гасится, сессия/конференция удаляются
в `cleanup` — пользовательские данные не трогаются (сессия `49b599d7…` только read-only).

## Что именно проверяется

1. **Иммутабельность типа «Общей» на backend.** UI-редактор шлёт
   `PATCH /api/v3/ui/sessions/<id>/video_layouts/<default>` c `payload.type=individual`
   (так делал `codian-layouts.js` до фикса). Сервер обязан ответить 200, но сохранить
   `payload.type=default`; в session-scope должна остаться ровно одна «Общая».
2. **Отсутствие крэша** `TypeError: … reading 'some'` / `sessionLayoutListByType.default`
   на странице `/pnm/monitoring/sessions/<id>` с connected-абонентом (и после reload).
3. **JS-guard в вендорском бандле.** `--guard-only` через Playwright route вырезает все
   default-строки из ответа `GET /api/v3/ui/sessions/*/video_layouts` и отдаёт браузеру
   ровно «битый» список. С патчем `(…default||[])` страница обязана устоять.
4. **Чувствительность теста.** `--guard-only --legacy-bundle` подменяет в ответе чанка
   строку guard'а обратно на pre-fix `this.sessionLayoutListByType.default.some(...)` —
   крэш ДОЛЖЕН воспроизвестись. Это доказывает, что «зелёный» прогон не ложный.

## Прогоны

| # | Команда | Результат | Ключевые checks |
|---|---------|-----------|-----------------|
| 1 | `node repro_session_default_layout.js --expect-clean` | **exit 0**, `reproduced=false`, `scenario_ok=true` | `known session 49b599d7: durable repair keeps exactly one default` (`["8a75f3e4…:default"]` после repair; в прогоне своя комната — `12cf832c…:default`); `constructor: save request captured` — UI шлёт `payload.type=default` (фикс `codian-layouts.js`); `diagnostic: flip outcome` — default retained (`immutable=true`); `crash: TypeError from hasIndividualLayout captured` = **false**; `ui: connected participant row present` = true; `mode: expect-clean satisfied` |
| 2 | `node … --expect-clean --guard-only` | **exit 0**, `reproduced=false` | `guard-only: broken layout list did not crash the session page (JS guard active)` = true; вердикт `guard-only-expect-clean satisfied` |
| 3 | `node … --guard-only --legacy-bundle` | **exit 0**, `reproduced=true`, `mode=guard-only-expect-crash` | `[repro] CRASH (console) TypeError: Cannot read properties of undefined (reading 'some')` ×N — ровно исходный дефект; `bundle.injection=guard-removed`; вердикт `guard-only-expect-crash satisfied` |

Все три прогона делают cleanup: `sipbot: stopped (SIGTERM)`, `session: stopped 204`,
`conference: removed (purge room-scoped layouts) 204`. Артефакты:
`~/.local/state/vks/artifacts/repro-crash/{result,network,pageerrors}.json`,
`01-session-baseline.png`, `02-session-after-flip.png`, `03-no-crash.png`.

## Прогон 1 — детали (пост-фикс инвариант)

```
known session 49b599d7: ["8a75f3e4…:default"]   ← durable-ремонт при загрузке документа
conference/session: started                       ← своя комната
session: canonical default attached at start: ["<id>:default"]
sip: participant connected (media flows): sip-…-in@sip-gateway
baseline: session page with connected abonent renders without crash: []
constructor: save request captured:
  POST /api/v3/ui/video_layouts/mode  {new_mode:"dynamic", payload:{mode:"auto",preset:"preset_default_13",type:"default"}}
  PATCH …/video_layouts/<id>          {payload:{mode:"dynamic",preset:"preset_default_13",type:"default",size:13,autoscroll:0}}
session: default still present after editor save: ["12cf832c…:default"]
flip: PATCH executed (200); scope_after=["12cf832c…:default"]; immutable=true
crash: TypeError from hasIndividualLayout captured: no crash captured
ui: connected participant row present in DOM: true
mode: expect-clean satisfied (reproduced=false scenario_ok=true)
```

Вывод: оба write-пути (`PATCH …/video_layouts/<id>` через `edit_linked` и
`POST /api/v3/ui/video_layouts/mode`) сохраняют `type=default`; после сохранения редактора
«Общая» остаётся в scope; крэш не воспроизводится.

## Прогон 3 — доказательство исходного дефекта

`--legacy-bundle` не меняет файлы на диске: Playwright route подменяет только тело ответа
`**/chunks/*bundle-*.js*` (`(this.sessionLayoutListByType.default||[])` →
`this.sessionLayoutListByType.default`). При битом списке раскладок (route вырезал default)
pre-fix код падает в `hasIndividualLayout` с точной сигнатурой бага. Значит:
- JS-guard из `scripts/patch-pnm-layout-crash.py` — рабочий слой защиты;
- тест ловит именно этот дефект, а не «зеленеет» из-за отсутствия сценария.

## Ограничения

- Проверяется Chromium; Firefox-специфичный текст `can't access property "some"` ловится
  тем же `isCrashSignature`, но отдельного Firefox-прогона в T1 нет (Playwright firefox
  не установлен в окружении) — риск низкий: код и guard одинаковы для всех браузеров.
- `--guard-only` не мутирует backend и не проверяет durable-ремонт; для этого прогон 1
  (реальные API-вызовы) + юнит-тесты `t00_*` в `resource_tests.rs`.
- Сессия `cac8ed13…` из форензики к моменту рестарта уже не существует (404
  `Conference or session not found`) — её scope чинить нечего; живой документ содержит
  только существующие комнаты.
