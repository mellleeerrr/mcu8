# T1 — E2E-репро `sessionLayoutListByType.default` и доказательство фикса

Скрипт: [tests/e2e/repro_session_default_layout.js](../../../tests/e2e/repro_session_default_layout.js)
(CommonJS, Playwright 1.64.1 / chromium-1243, SIP-абонент — `scripts/bench/sipbot.py`).

Стенд: `http://127.0.0.1:8080/pnm/`, vks-server PID 1814143 (release, старт 18:00:39 MSK;
конфиг `configs/default.toml`), вендор UI 2.2.1.66 + project-слой, коммит фикса `645120d`.
Проверки выполнены 28.10.2026, 17:43–18:12 MSK.

## 1. Крэш воспроизведён: ДА (на pre-fix коде), с точным стеком

Реальный прогон в браузере на живом стенде:

```
TypeError: Cannot read properties of undefined (reading 'some')
    at Proxy.hasIndividualLayout (http://127.0.0.1:8080/mcu/chunks/index6.bundle-2.2.1.66.js:1:720556)
    at $A (http://127.0.0.1:8080/mcu/chunks/index.bundle-2.2.1.66.js:2:28177)
    at get value (http://127.0.0.1:8080/mcu/chunks/index.bundle-2.2.1.66.js:2:40308)
    at Object.get [as hasIndividualLayout] (http://127.0.0.1:8080/mcu/chunks/index.bundle-2.2.1.66.js:2:60966)
    at Object.get (http://127.0.0.1:8080/mcu/chunks/index.bundle-2.2.1.66.js:2:59272)
    at http://127.0.0.1:8080/mcu/chunks/index6.bundle-2.2.1.66.js:1:727641
    at r (http://127.0.0.1:8080/mcu/chunks/index.bundle-2.2.1.66.js:2:44329)
    at Ct (http://127.0.0.1:8080/mcu/chunks/index.bundle-2.2.1.66.js:2:58070)
    at Proxy.<anonymous> (http://127.0.0.1:8080/mcu/chunks/index6.bundle-2.2.1.66.js:1:568484)
    at mS (http://127.0.0.1:8080/mcu/chunks/index.bundle-2.2.1.66.js:2:66691)
```

Первый живой крэш пойман 17:43:54 MSK ещё на **незафиксированном** бандле: 3 подряд
`console.error` с этой сигнатурой (`/tmp/probe_crash.png`, стек — тот же).
Ошибка повторяется на каждом цикле обновления таблицы абонентов: в финальном прогоне
**9 записей** с шагом ~5 c (15:07:26.490Z … 15:07:46.366Z) — это и есть «падает при
обновлении таблицы».

**Важная честная оговорка:** `pageerror` (window.onerror) = **0**. Vue 3 без error boundary
перехватывает исключение render-функции и логирует его через `console.error` (`logError`),
поэтому скрипт считает крэшем `console.error` с сигнатурой `hasIndividualLayout` /
`sessionLayoutListByType.default`, а не только `pageerror`. Семантика та же: render-проход
компонента падает, страница остаётся полу-отрисованной. В `pageerrors.json` пишутся оба
списка (`pageerrors`, `console_errors`) и `crash_records` с полным стеком.

## 2. Минимальная последовательность (что делает скрипт)

1. Login `admin`, токен из `localStorage['TrueConfMCU_AccessToken']`.
2. Read-only сверка с сессией из баг-репорта `49b599d7…` (только GET, ничего не меняем).
3. `POST /api/v3/conferences {display_name:"E2E Crash <ts>"}` → `POST /api/v3/sessions {conference_id}`;
   проверяем, что сервер выдал ровно одну «Общую» (`payload.type="default"`).
4. Поднимаем `sipbot.py --room <session_id> …` (room = UUID сессии, иначе SIP UAS отдаёт 404),
   ждём connected-абонента (`…@sip-gateway`, `audio_rx_packets > 0`).
5. Базовая линия: `/pnm/monitoring/sessions/<id>` 9 c — крэша быть не должно.
6. Best-effort редактор «Раскладки → Общая → Динамическая раскладка → Применить»:
   перехватываем реальные запросы сохранения (payload для root cause).
7. Ломаем состояние через публичный API:
   `PATCH /api/v3/ui/sessions/<sid>/video_layouts/<defaultId>` c `{"payload":{"type":"individual"}}`.
   На pre-fix бэкенде тип менялся, «Общая» исчезала из session-scope (`["<id>:individual"]`).
8. Грузим/перезагружаем страницу сессии (3 захода по 12 c) → ловим `TypeError`.
9. Cleanup: sipbot SIGTERM → `DELETE /api/v3/sessions/<id>` (204) →
   `DELETE /api/v3/conferences/<id>` (204, purge room-scoped раскладок).

Начиная с 18:00:39 стенд перезапущен с полным фиксом: `PATCH payload.type` больше не меняет
тип (иммутабельность), а legacy-сессия `49b599d7…` durable-отремонтирована (её единственная
`individual` стала `default`). Поэтому state-step 7 на живом стенде битый scope не создаёт —
скрипт в этом случае инжектит **то же клиентское состояние** на границе API
(`route.fulfill` вырезает строки `type=default` из ответа списка) и фиксирует
`flip.forced="api-route-strips-default"` в `result.json`. Бэкенд при этом не мутируется.

## 3. Payload сохранения «Общей» (root cause)

Перехваченные реальные запросы редактора (пост-фикс UI):

```
POST /api/v3/ui/video_layouts/mode
{"session_id":"<sid>","video_layout_id":"<defaultId>","new_mode":"dynamic",
 "display_name":null,"payload":{"mode":"auto","preset":"preset_default_13","type":"default"}}

PATCH /api/v3/ui/sessions/<sid>/video_layouts/<defaultId>
{"display_name":null,"payload":{"mode":"dynamic","preset":"preset_default_13",
 "type":"default","size":13,"autoscroll":0}}
```

То есть **`payload.type` отправляется всегда** (сериализатор `Th()` кладёт `type` из
`layoutEditorStore.modifiedValue`), и сейчас это `default`. Гипотеза «UI шлёт
`type:"individual"` при правке Общей» для текущего кода **не подтвердилась** — и это
ожидаемо: фикс `codian-layouts.js` убрал запись `editor.editValueField('type','individual')`.
На pre-fix коде она была (`git show 645120d^:client/pnm-frontend/deployed/pnm/codian-layouts.js`,
строки 382 и 1239) — именно так default превращался в individual, сессия оставалась без
default, и UI падал. Детали цепочки — в [01_crash_forensics.md](01_crash_forensics.md).

## 4. Как запускать до/после фикса

```bash
cd tests/e2e
node repro_session_default_layout.js                # до фикса: 0 = крэш пойман, 1 = не поймали
node repro_session_default_layout.js --expect-clean # после фикса: 0 = крэша нет и сценарий отработал
node repro_session_default_layout.js --legacy-bundle        # откат guard'а только в ответе бандла
node repro_session_default_layout.js --expect-clean --guard-only
node repro_session_default_layout.js --guard-only --legacy-bundle
```

- `--legacy-bundle` не трогает файлы: Playwright route подменяет в **ответе**
  `**/chunks/*bundle-*.js*` **все** вхождения guard'а
  `(this.sessionLayoutListByType.default||[])` → `this.sessionLayoutListByType.default`
  (в бандле их два: `hasIndividualLayout` и broadcast-popup). Нужен, когда фикс уже
  задеплоен: доказывает, что тест ловит именно этот дефект.
- `--guard-only` не мутирует бэкенд: route вырезает `type=default` из ответа
  `GET /api/v3/ui/sessions/*/video_layouts` и проверяет JS-guard на «битом» списке.
- `--keep` оставляет конференцию/сессию для разбора; по умолчанию всё удаляется.
- Артефакты: `artifactsDir('repro-crash')` = `~/.local/state/vks/artifacts/repro-crash`
  (`result.json`, `network.json`, `pageerrors.json`, `01-session-baseline.png`,
  `02-session-after-flip.png`, `03-crash.png`/`03-no-crash.png`).
  `sipbot.json` не появляется: e2e останавливает sipbot SIGTERM, а `sipbot.py`
  пишет JSON только при нормальном завершении (см. `sipbot.py:_write_json`).
  Можно переопределить: `VKS_ARTIFACTS=/path node …`.
  Сырые артефакты финальных прогонов из таблицы ниже лежат в
  `repro-crash/evidence-{legacy,clean,guard-legacy,postfix-default}/`
  (`evidence-legacy/03-crash.png`, `evidence-clean/result.json` и т.д.).
  Для прогона C отдельного сохранённого каталога нет (он выполнялся как
  guard-only проверка без копирования evidence); воспроизводимость C
  подтверждена повторным прогоном `--expect-clean --guard-only` (exit 0,
  `layout_injection="stripped 1 default(s)"`).

### Матрица прогонов 18:07–18:12 (финальная версия скрипта)

| # | Команда | exit | Что получилось |
|---|---------|------|----------------|
| A | `--legacy-bundle` | **0** | `reproduced=true`, 9 записей `TypeError … reading 'some'` (пары с интервалом ~5 c), stack `index6…:1:720556`, сессия `5a559d7e…`, 51 c |
| B | `--expect-clean` | **0** | `reproduced=false`, `scenario_ok=true`, 0 console-ошибок, connected-строка в DOM, сессия `57a7ec48…`, 80 c |
| C | `--expect-clean --guard-only` | **0** | `guard-only-expect-clean`, `reproduced=false`, `layout_injection="stripped 1 default(s)"` |
| D | `--guard-only --legacy-bundle` | **0** | `reproduced=true`, 9 записей, `bundle.injection="guard-removed"`, `layout_injection="stripped 1 default(s)"`, сессия `6d2ab33c…`, 31 c |
| E | без флагов (fixed stand) | **1** | `reproduced=false`, `scenario_ok=true` — сценарий реально отработал (арминг + connected + reload), крэша нет; так и должно быть после фикса |

Прогоны A и D — доказательство, что тест **чувствителен** (падает на pre-fix клиенте),
B и C — что после фикса он **зелёный** и не флапает. Прогон E показывает, что «без флага»
на починенном стенде честно возвращает 1 (нечего ловить), а не «зеленеет» молча.

Cleanup во всех прогонах: `sipbot: stopped (SIGTERM)`, `session: stopped 204`,
`conference: removed 204`. После прогонов на стенде не осталось ни одной `E2E Crash…`
конференции (проверено `GET /api/v3/conferences`).

## 5. Что не сработало и почему

- **`pageerror` не ловится.** Vue 3 перехватывает ошибку render-функции и пишет
  `console.error`, `window.onerror` не вызывается. Если проверять только `pageerror`,
  тест будет ложно-зелёным. Решение: сигнатура по тексту ошибки в `console`.
- **Живой бандл к моменту финальных прогонов уже содержал guard** (правка 17:43:55,
  затем коммит `645120d`). Плейн-прогон после этого крэшить не может — отсюда
  `--legacy-bundle` (клиент) и `--guard-only` (состояние).
- **После рестарта 18:00:39 бэкенд стал иммутабельным**: `PATCH payload.type=individual`
  отвечает 200, но сохраняет `default` (COW-клон), legacy-сессия отремонтирована.
  Поэтому «сломать» scope публичным API больше нельзя; скрипт инжектит битый список
  на границе API (только ответы браузеру прогона) и помечает `flip.forced`.
- **Параллельные прогоны** скрипта делят `artifactsDir('repro-crash')` и могут перетирать
  `result.json` друг друга; sipbot-порты (5080+2i) тоже конфликтуют. В скрипте sipbot
  получает `--index` от PID, но артефакты надо разводить через `VKS_ARTIFACTS` или
  запускать последовательно.
- **Firefox не проверялся** (в окружении нет playwright-firefox); текст ошибки там
  `can't access property "some"`, `isCrashSignature` его покрывает, но отдельного прогона нет.
- Крэш **не выглядит как белый экран**: Vue логирует ошибку и продолжает рендер остального
  дерева, поэтому скриншот `03-crash.png` показывает рабочую страницу. Единственный
  надёжный признак — запись в console со стеком; это записано в `result.json`/`pageerrors.json`.

## 6. Риски для живого стенда

- Прогон создаёт **настоящую** конференцию/сессию и SIP-абонента с медиа (~30–80 c),
  то есть кратковременно нагружает MCU и SIP-гейтвей. После прогона всё удаляется
  (`DELETE session`, `DELETE conference`), но при обрыве сети возможна «висячая»
  конференция `E2E Crash…` — её нужно удалить вручную.
- State-step 7 на **pre-fix** бэкенде реально менял `payload.type` «Общей» — но только
  в собственной сессии, которая затем удаляется. На текущем (иммутабельном) бэкенде
  этот шаг ничего не меняет, а инжекция живёт только в браузерном route.
- Сессия `49b599d7…` **не трогается** (только GET; в отчёте фиксируется её состояние).
- Никаких изменений кода приложения, бандлов или конфигов скрипт не делает; сервис
  не перезапускает; работает только через публичный HTTP API и браузер.
- SIP-порты: слот `5080+2i`/RTP `20000+4i`; при одновременном прогоне двух экземпляров
  возможен конфликт — запускать последовательно.
