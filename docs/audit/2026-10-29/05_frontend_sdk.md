# 05. PNM-фронтенд (legacy UI) и VKS Client SDK — аудит

- **Дата:** 2026-10-29
- **Агент:** `frontend-sdk` (зона: `client/pnm-frontend/deployed/pnm/`, `scripts/import-pnm-frontend.sh`,
  `scripts/patch-pnm-*.py`, `crates/vks-server/src/frontend.rs`, `client/vks-client-sdk/`, `tests/e2e`,
  ADR-0027/0030/0032, `docs/rules/FRONTEND_RULES.md`, `docs/DARK_THEME_PNM_PLAN.md`).
  L0-хост (sysctl/cgroup) в этот отчёт не входит — см. `08_live_runtime.md`.
- **Коммит:** `d82ce4d` (master). Отслеживаемое дерево чистое; в `git status` только untracked-каталог
  `docs/audit/2026-10-29/` (продукт этого раунда аудита).
- **Режим:** read-only. Код, БД, сервис на `:8080` не менялись и не перезапускались. Единственный
  записанный файл — этот отчёт. Для браузерной проверки deep link сделан один `POST /api/v3/login`
  (получение токена; бизнес-данные не менялись, в журнале аудита сервера останется запись `login_ok`).
- **Окружение:** Rust 1.94.0, Node + Playwright (Chromium 1243) из `tests/e2e`, живой сервис
  `http://127.0.0.1:8080`.
- **Базлайн:** `cargo test --workspace` — 1425 passed / 0 failed / 8 ignored; `cargo test -p vks-client-sdk --lib`
  — 51 passed; `cargo check -p vks-client-sdk --target wasm32-unknown-unknown` — exit 0;
  `sha256sum -c client/pnm-frontend/MANIFEST.sha256` — 214/214 OK.

> Обозначение ссылок на минифицированные бандлы: `chunks/index.bundle-2.2.1.66.js:1@542341` — файл
> состоит из одной строки, число после `@` — смещение в байтах (проверено `str.find` по точной подстроке).

---

## §1. Вердикт

**6,5/10.**

1. **Deep link на сессию сегодня работает — если токен жив.** Маршрут `/monitoring/sessions/:sessionId?`
   есть в вендорском роутере, компонент сессии сам вызывает `refreshSession` по `watch(..., immediate)`,
   сервер отдаёт `index.html` через SPA-fallback. Проверено живым прогоном: холодная загрузка
   `/pnm/monitoring/sessions/02785e07-…` с токеном → карточка сессии, заголовок из данных, 6 вкладок,
   0 ошибок в консоли. **Hydration-IIFE из примера заказчика не нужна.**
2. **Реальный дефект — не рендеринг, а вход.** Без токена (новый браузер, ссылка из письма, истёкший
   токен) гард уводит на `/pnm/authorization` **без сохранения `to.fullPath`**, а обработчик логина всегда
   делает `push({name:"monitoring"})`. Проверено: после логина URL — `/pnm/monitoring/sessions`, id сессии
   потерян. Это и есть «пока не выбрать вручную», но лечится патчем гарда/логина, а не fetch-ом в
   `perf-overrides.js`.
3. **Цветовой тезис бьёт мимо цели.** `#17212B` — это `--c-surface` (Telegram Surface), а не акцент;
   акцент проекта — `--c-primary: #0088cc`. `#0097A7` — вендорский primary в скомпилированном CSS,
   его миграция **осознанно отложена** (`client/pnm-frontend/NOTES.md:69-71`). В `index.html` `#0097A7`
   встречается один раз (`theme-color`), и это значение всё равно перетирается `theme-toggle.js` при
   загрузке. Единого источника цвета нет: три писателя.
4. **SDK: конвейеры есть, автопереключения по `ServerHello.room_mode` — нет.** `RoomMode{Sfu,Mcu}`,
   `switch_mode`, `McuRenderer`/`SfuRenderer`, обработка push-`RoomModeChanged` реализованы и покрыты
   тестами. Но собственная структура SDK `ServerHello` (`signaling_client.rs:17-23`) **выбрасывает**
   `room_mode`, и `Conference::start_async` его не читает — сервер поле шлёт (`handler.rs:288`), SDK его
   не применяет. Публичного WASM-API (`#[wasm_bindgen]`) нет ни одного: «WASM SDK» сейчас — Rust-крейт,
   компилируемый под wasm32, а не библиотека для браузера.
5. **Старые претензии (482/483, грязное дерево) неактуальны.** В архиве 483 файловых записи, 1 битая
   (дубль `index-legacy.js` в `source/`), т.е. 482/483 — точная и безобидная цифра; отсутствующий
   `BiOEwveA.svg` заменён на `prostor-wordmark.svg` в ребренде (`NOTES.md:61`). Дерево чистое.
6. **Главные риски — не в UI, а в процессе.** (а) `import-pnm-frontend.sh --force` сохраняет только
   `perf-overrides.js`, `api-errors.js`, `codian-layouts.js` и CSS-блок, но **не** `theme-toggle.js`, хотя
   тег `<script src="/mcu/theme-toggle.js">` остаётся; (б) `precompressed_gzip` включён, но `.gz`-файлов
   в дереве **ноль** — 12,75 МБ JS/CSS/HTML уезжает в браузер без сжатия; (в) в CI нет ни `node --check`
   для проектного JS, ни одного e2e.

---

## §2. Матрица

Статусы: ✅ сделано и проверено · ⚠️ сделано частично / есть оговорка · ❌ не сделано / неверно.

### A. Deep link на сессию (`/pnm/monitoring/sessions/<id>`)

| № | Утверждение / элемент | Статус | Доказательство |
|---|---|---|---|
| A1 | Сервер отдаёт SPA-shell на любой `/pnm/*`, включая deep link | ✅ | `crates/vks-server/src/frontend.rs:82-88` (`ServeDir…fallback(ServeFile(index))`); тест `pnm_index_and_spa_fallback` `frontend.rs:480`; live: `GET /pnm/monitoring/sessions/probe-id` → `200 text/html` 74 989 B |
| A2 | Вендорский роутер знает параметр сессии | ✅ | `chunks/index.bundle-2.2.1.66.js:1@542341` — `path:"/monitoring"` с детьми `sessions/:sessionId?`, `layouts/:layoutId?`, `audiochannels/:audioChannelId?` |
| A3 | `sessionId` берётся из маршрута, а не из выбора в списке | ✅ | `index.bundle…js:1@1133044` — `sessionId(){return this.$route.params.sessionId}` (миксин `vy`) |
| A4 | Компонент сессии сам грузит данные при прямом заходе | ✅ | `chunks/index6.bundle-2.2.1.66.js:1@976252` — `watch:{sessionId:{handler(){this.refreshSession(this.sessionId,!0)…},immediate:!0}}` |
| A5 | Прямой заход с валидным токеном реально рисует сессию | ✅ | Прогон 2026-10-29 (Приложение A, п.1): `sessionCard:true`, header «E2E Probe 1793203161384», 6 вкладок (`session-main-tabs-general`, `-layouts`, `-skins`, `record-and-stream`, `-additional`, `-control-panel`), `errors:[]`; ранее — `tests/e2e/audio-in-layouts.js` (артефакт 2026-10-23 `ok:true`, проверка «session page: tab bar rendered») |
| A6 | Прямой заход без токена / после истечения токена | ❌ | `index.bundle…js:1@1218293` — `…? (t({name:"monitoring"}),…) : (t({name:"authorization"}), Promise.reject())`; `index.bundle…js:1@1261077` — `401 → t.logout(), Ys.push({name:"authorization"})`; `chunks/index4.bundle-2.2.1.66.js:1@1805` — после логина `this.$router.push({name:"monitoring"})`. Live (Приложение A, п.2): было `/pnm/monitoring/sessions/<id>` → стало `/pnm/authorization` → после логина `/pnm/monitoring/sessions` (id потерян) |
| A7 | Предложенная IIFE / `__PROSTOR_INITIAL_SESSION__` / `prostor:select-session` | ❌ | Символов нет нигде: `grep -rl` по `client/`, `scripts/`, `docs/`, `crates/`, `tests/` → 0 совпадений; в `perf-overrides.js` (273 строки) — только codec-кэш, previews, PATCH-инвалидация, перехват XHR. Контракт API неверен — см. §4.1 |
| A8 | Неизвестный/устаревший id сессии даёт внятную ошибку | ⚠️ | `chunks/index6.bundle-2.2.1.66.js:1@976349` — `watch:{sessionId:{handler(){this.refreshSession(this.sessionId,!0).catch(()=>{this.$router.push({name:"monitoring-sessions"})})},immediate:!0}}`: молчаливый редирект в список без сообщения «сессия не найдена» |

### B. Цвет, тема, MANIFEST, CSP

| № | Утверждение / элемент | Статус | Доказательство |
|---|---|---|---|
| B1 | `#0097A7` в `index.html` | ⚠️ | `index.html:37` — `<meta name="theme-color" content="#0097A7">` (единственное вхождение; две записи `#17212B` — это `--c-surface` и комментарий, `index.html:270,283`) |
| B2 | `#0097A7` в манифесте | ⚠️ | `assets/manifest.webmanifest:11` — `"theme_color": "#0097A7"`, `:9` — `"start_url": "/?homescreen=1"` |
| B3 | В рантайме `theme-color` перетирается | ✅ | `theme-toggle.js:24-27` — `meta.setAttribute('content', isDark ? '#0E1621' : '#0088CC')`; скрипт подключён до бандла (`index.html:32`) |
| B4 | `#17212B` — акцент проекта | ❌ | Это **surface**: `index.html:283` `--c-surface:#17212b`, комментарий `index.html:270` «Surface / Cards / Header: #17212B (Telegram Surface)». Акцент — `--c-primary:#0088cc` (`index.html:290`) |
| B5 | `#0097A7` — вендорский primary в CSS, миграция отложена | ✅ | `chunks/index.bundle-2.2.1.66.css:1@27504` (`.mdc-button--text{color:#0097a7}`), градиент статуса в `index5…css`, спиннер в `index7…css`, 4 SVG; `NOTES.md:69-71`: «Не входит … цветовая тема `#0097A7` → PROSTOR-синий» |
| B6 | Сервер пересчитывает CSP-хэши | ✅ | `frontend.rs:66` (`Frontend::load`) → `frontend.rs:246` (`build_csp`), `extract_inline_scripts` `frontend.rs:264`; live-CSP содержит 5 `sha256-…`, ровно по числу inline-скриптов `index.html` (разбор: 5 тегов без `src`, длины 558/108/400/314/84) |
| B7 | `MANIFEST.sha256` связан с CSP | ❌ | Манифест — только provenance (`sha256sum -c` 214/214; CI `ci.yml:255-269`). CSP считается из байтов `index.html` при **старте** сервиса: правка inline-скрипта без рестарта → новый скрипт будет заблокирован; правка только `<meta>` — безопасна |
| B8 | Правки цвета переживают импорт PNM | ❌ | `scripts/import-pnm-frontend.sh:136,224` — проектный слой ограничен 3 JS + CSS-блоком; `<meta name="theme-color">`, `manifest.webmanifest` и `theme-toggle.js` не сохраняются и не патчатся |

### C. VKS Client SDK (конвейеры MCU/SFU)

| № | Утверждение | Статус | Доказательство |
|---|---|---|---|
| C1 | Модель режима и переключение | ✅ | `client/vks-client-sdk/src/media_engine/mod.rs:22-40` (`RoomMode{Sfu(default),Mcu}`, `as_str`/`parse`), `:77-92` (`switch_mode` — сбрасывает раскладку), `:94` (`clear_layout`) |
| C2 | Два конвейера рендера | ✅ | `media_engine/mcu_renderer.rs` — всегда 1 `DrawCommand{participant_id:"composite",rect:(0,0,w,h)}`; `media_engine/sfu_renderer.rs:27-45` — `GridLayout::compute(&Canvas, n)`; `:50-72` `compose_from_layout`; `:76-84` `compose_from_normalized` |
| C3 | Диспетчеризация по режиму | ✅ | `media_engine/mod.rs:100-140` — `render()` выбирает composite / layout / auto-grid; тесты `:143-180` |
| C4 | Push-смена режима | ✅ | `conference.rs:113-114` (`RoomModeChanged` → `RoomMode::parse` → `switch_mode`), `conference.rs:96-121` (`drain_events` применяет); тест `conference.rs:623` |
| C5 | `ServerHello.room_mode` доезжает до SDK | ❌ | Сервер шлёт: `crates/vks-signaling/src/handler.rs:288` (`room_mode: self.config.room_mode`), proto `crates/vks-proto/proto/signaling.proto:60` (`RoomMode room_mode = 6`). SDK-структура `signaling_client.rs:17-23` содержит только `server_version`, `session_id`, `server_time_ms` — поле теряется при декодировании |
| C6 | Автовыбор конвейера при join | ❌ | `conference.rs:206-224`: проверяется только `hello.server_version.is_empty()`; `hello.room_mode` не существует, `self.media.switch_mode` не вызывается. Единственный путь смены режима — push `RoomModeChanged` (C4), т.е. до первого push клиент остаётся в `Sfu` по умолчанию |
| C7 | «SFU = N simulcast + CSS Grid/Taffy» | ❌ | Taffy в репозитории отсутствует (`grep -rn taffy` → 0); `vks-layout` без зависимостей (`crates/vks-layout/Cargo.toml` — только `thiserror`, `serde`) считает **нормализованные rect**; SDK отдаёт `DrawCommand` с пиксельными rect, а не CSS Grid. Раскладку в DOM применяет потребитель через `RenderSurface` |
| C8 | Публичный WASM-API (`#[wasm_bindgen]`, `VksClientSession`) | ❌ | `grep -rn "wasm_bindgen" client/vks-client-sdk/src` → 0 экспортов; `Cargo.toml` содержит wasm-зависимости, но наружу не выставлено ничего. `FRONTEND_RULES §7.1` (`join/on_event/mute/start_screen_share/Participant/Track`) не реализован ни в одном пункте |
| C9 | `ConferenceConfig.surface` нужен | ⚠️ | `conference.rs:31` — поле хранится, но нигде не используется (рендер принимает surface аргументом, `conference.rs:606`); мёртвый `Arc<dyn RenderSurface>` в конфиге мешает wasm-обёртке |
| C10 | Тесты SDK | ✅/⚠️ | 51/51 passed, wasm32 `cargo check` — OK; но нет `wasm-bindgen-test`, нет проверки «join с `room_mode=mcu` → composite», нет сетевых/браузерных тестов |

### D. Целостность рантайма, импорт, эксплуатация

| № | Утверждение | Статус | Доказательство |
|---|---|---|---|
| D1 | «482 из 483 файлов» в архиве | ✅ | 484 записи ZIP = 1 каталог + 483 файла; 1 нечитаем (`source/extracted_frontend/raw_decompiled/legacy/index-legacy.bundle-2.2.1.66/index-legacy.js`, битый deflate) → 482/483 точно; это дубль, на рантайм не влияет |
| D2 | «Грязное дерево» | ✅ (неактуально) | `git status --porcelain=v1 -uall` → только 5 untracked-файлов в `docs/audit/2026-10-29/`; изменённых отслеживаемых файлов — 0 |
| D3 | Нет symlink’ов и `.bak` в рантайме | ✅ | `find … \( -name '*.bak*' -o -type l \)` → 0; тест `runtime_tree_has_no_symlinks` `frontend.rs:626` |
| D4 | `MANIFEST.sha256` актуален и проверяется | ✅ | 214 строк, `sha256sum -c` 214/214 OK; `index.html` sha256 `faac8233…` совпадает с манифестом (строка 212); CI `ci.yml:255-269` (манифест + `patch-pnm-layout-crash.py --check`) |
| D5 | Импорт сохраняет проектный слой | ⚠️ | Сохраняются `perf-overrides.js`, `api-errors.js`, `codian-layouts.js` (`import…sh:136,224`) и CSS-блок (`:145-260`); **`theme-toggle.js` не сохраняется**, хотя тег остаётся (`index.html:32`); `prostor-wordmark.svg` не сохраняется, но это покрыто оговоркой про ребренд-слой (`NOTES.md:94`) |
| D6 | Сжатие статики | ❌ | `frontend.rs:85,86,87` — `.precompressed_gzip()`; `.gz` в дереве **0** (`find -name '*.gz'`); live `GET /mcu/chunks/index.bundle…js` → `content-length: 1267133`, **без** `content-encoding`. 12,75 МБ JS/CSS/HTML raw против ≈2,9 МБ gz (замер `gzip -9` по всему дереву) |
| D7 | PWA | ⚠️ | Манифест есть, иконки существуют (0 отсутствующих по `icons[].src`), но service worker отсутствует (`grep serviceWorker` → 0), `theme_color` — teal, `start_url` — `/` (вне базы `/pnm/`, лишний 307 при запуске) |
| D8 | SPA-fallback не подменяет статику | ❌ | `GET /pnm/chunks/nope.js` → `200 text/html` (fallback на `index.html`); `GET /mcu/chunks/nope.js` → `404`. Для отсутствующего чанка под `/pnm/` браузер получит HTML вместо JS |
| D9 | CI проверяет проектный JS | ❌ | В `ci.yml` нет `node --check`, нет `playwright`; 19 e2e-скриптов в `tests/e2e/` не подключены к раннеру (`npx playwright test --list` → 0 tests). Ловится только манифест и 1 из 3 патч-скриптов |
| D10 | Документация соответствует дереву | ⚠️ | ADR-0027: «MANIFEST — 210 файлов», `NOTES.md:41`: «deployed (26 MB)» и «210 строк» — факт: 214 файла, 27 MB; `docs/DARK_THEME_PNM_PLAN.md` описывает invert-пилот, которого в дереве уже нет |

---

## §3. Дефекты

P0 (блокеры/утечки/потеря данных) — **не обнаружено**. Ниже P1 (ломает сценарий или тихо регрессирует при
штатной процедуре) и P2 (качество/перф/сопровождение).

### F-01 (P1). Deep link теряется на неаутентифицированном старте

- **Симптом.** Ссылка `/pnm/monitoring/sessions/<id>` открыта в новом браузере или после истечения
  токена: пользователь попадает на форму входа, после входа — в **список** сессий, id теряется.
  Это ровно та картина, которую заказчик описал как «не отрисовывается, пока не выбрать вручную».
- **Причина.** Гард вызывает `next({name:"authorization"})` без `redirect`/`query`; 401-интерсептор
  делает `logout()` + `push({name:"authorization"})`; обработчик логина безусловно
  `push({name:"monitoring"})`; в форме смены пароля — то же.
- **Файлы/строки.** `chunks/index.bundle-2.2.1.66.js:1@1218293`, `:1@1261077`;
  `chunks/index4.bundle-2.2.1.66.js:1@1805`; `index.bundle…js:1@1261548` (`beforeEach`).
- **Исправление (минимальное).** Новый идемпотентный `scripts/patch-pnm-auth-deeplink.py` по образцу
  существующих: (1) гард → `t({name:"authorization", query:{redirect:e.fullPath}})`; (2) обработчик
  логина → `const r=this.$route.query.redirect; this.$router.push(r && r.startsWith("/") ? r : {name:"monitoring"})`;
  (3) то же в ChangePasswordForm. Патч обязан падать при несовпадении числа якорей; `--check` — в CI.
- **Тест.** Playwright: `goto(deepLink)` без токена → `pathname==="/pnm/authorization"` и `search`
  содержит `redirect=`; после UI-логина `pathname` равен исходному deep link, карточка сессии отрисована.
  Плюс `patch-pnm-auth-deeplink.py --check` (exit 0) в CI и на «свежем» бандле.

### F-02 (P1). Импорт PNM теряет `theme-toggle.js` (и не покрывает тему/манифест)

- **Симптом.** После штатного `scripts/import-pnm-frontend.sh --force` (документированный путь
  обновления рантайма) в дереве остаётся `<script src="/mcu/theme-toggle.js">` (`index.html:32`), но
  самого файла нет → `404`, кнопка темы исчезает, `meta[theme-color]` больше не обновляется; правки
  `theme-color` в `index.html` и `assets/manifest.webmanifest` откатываются к архивным.
- **Причина.** Список сохраняемого проектного слоя жёстко задан тремя файлами; `theme-toggle.js`
  появился позже (коммит `7eb0b6f`, «Task 2»), в список не добавлен; цвет/манифест не покрыты
  patch-скриптом.
- **Файлы/строки.** `scripts/import-pnm-frontend.sh:129-138` (сохранение), `:224-228` (восстановление),
  `:163` (`PROJECT_JS = ("api-errors.js","perf-overrides.js","codian-layouts.js")`); `index.html:29-34`.
- **Исправление.** (1) Добавить `theme-toggle.js` в оба списка. (2) Добавить проверку целостности:
  каждый `<script src>` из `index.html` должен существовать в `DEST` (python-шаг в конце импорта,
  fail-closed). (3) Для цвета — либо patch-скрипт `patch-pnm-theme-color.py` (index.html + manifest,
  идемпотентный), либо включить `theme-color` в маркерный блок проекта; выбрать одно и записать в
  `NOTES.md`.
- **Тест.** `bash -n` + прогон импорта в `mktemp -d` (или `--check`-режим скрипта проверки ссылок);
  CI-шаг «все `<script src>`/`<link rel=manifest>` из `index.html` существуют».

### F-03 (P1). В проде нет сжатия статики: `.gz` не сгенерированы

- **Симптом.** Первая загрузка UI — 12,75 МБ JS/CSS/HTML без `Content-Encoding`; критический путь
  (index bundle 1,27 МБ + CSS 0,58 МБ + ru_RU 0,40 МБ + index.html) ≈ 2,3 МБ вместо ≈0,5 МБ gz
  (бюджет ADR-0027/FRONTEND_RULES §18.4 — «479 125 B gz ≤ 800 KB»).
- **Причина.** `precompressed_gzip()` отдаёт `.gz` только если файл существует; в дереве их ноль, а
  `--gzip` в импорте не запускался. Хуже: `--gzip` выполняется **после** пересборки манифеста
  (`import…sh:303-310`), а `*.gz` в `.gitignore:13-14` — значит `.gz` не проверяются манифестом и не
  попадают в git; при правке исходника старый `.gz` продолжит отдаваться (классическая ловушка
  stale-gzip).
- **Файлы/строки.** `crates/vks-server/src/frontend.rs:85-87`; `scripts/import-pnm-frontend.sh:303-310`;
  `.gitignore:13-14`; `Cargo.toml:62` (`tower-http` features = `["fs"]`, без `compression`).
- **Исправление (выбрать один).** (A, рекомендуется) включить feature `compression` и
  `CompressionLayer` (gzip/br) в `build_app` — без файлов, без manifest, без gitignore-конфликта;
  (B) отдельный `scripts/pnm-gzip.sh` (генерация + проверка свежести `gzip -dc | sha256sum` против
  манифеста) и запуск его в деплое/CI **после** манифеста, с шагом «.gz существует для каждого
  текстового ассета и не старше исходника».
- **Тест.** Интеграционный: `curl -H 'Accept-Encoding: gzip' …/mcu/chunks/index.bundle…js` →
  `content-encoding: gzip`, размер < 500 КБ; unit-тест `frontend.rs` уже проверяет механизм
  (`precompressed_gzip`, `frontend.rs:331-332`), но не фактическое дерево.

### F-04 (P2). `theme-color`: три писателя, неверная семантика `#17212B`, правка не переживает импорт

- **Симптом.** В `index.html:37` и `manifest:11` — teal `#0097A7`; в рантайме `theme-toggle.js:27`
  ставит `#0E1621`/`#0088CC`; заказчик предлагает заменить «акцент» на `#17212B`, который в проекте —
  surface, а не акцент. Визуально правка `<meta>` почти не наблюдаема (перетирается при загрузке).
- **Причина.** Нет единственного источника токенов темы; статический HTML, манифест и JS-переключатель
  живут отдельно; вендорский `#0097A7` в CSS — отложенная фаза (`NOTES.md:69-71`).
- **Файлы/строки.** `index.html:37,270,283,290`; `theme-toggle.js:24-27`; `manifest.webmanifest:9-11`;
  вендор: `chunks/index.bundle-2.2.1.66.css:1@27504` и др.
- **Исправление.** Зафиксировать один источник: константы в `theme-toggle.js` (или отдельный
  `theme-tokens.css`) + синхронные значения в `index.html`/манифесте, проверяемые тестом; для PWA
  `theme_color` = цвет фона тёмной темы (`#0E1621`), не primary. Миграцию вендорского `#0097A7` **не**
  делать точечно в `index.html` — это отдельная задача с cache-bust (правится CSS, не meta).
- **Тест.** `scripts/check-theme-tokens.py --check`: три значения (`index.html` meta, manifest
  `theme_color`, константы `theme-toggle.js`) совпадают с ожидаемыми; запуск в CI.

### F-05 (P2). SDK игнорирует `ServerHello.room_mode` — нет автовыбора конвейера при join

- **Симптом.** В комнате MCU клиент после join остаётся в `Sfu`-режиме (авто-сетка из rect’ов) до
  первого `RoomModeChanged`; в комнате SFU — наоборот. «Переключение по `room_mode` из ServerHello»,
  заявленное заказчиком, отсутствует.
- **Причина.** SDK-локальная структура `ServerHello` не содержит поля; `start_async` проверяет только
  версию.
- **Файлы/строки.** `signaling_client.rs:17-23`, `:150-200` (декодирование hello); `conference.rs:206-224`;
  сервер: `handler.rs:282-292`, `proto/signaling.proto:52-63`; ADR-0026 (режим комнаты).
- **Исправление.** (1) Добавить в SDK-`ServerHello` `pub room_mode: Option<RoomMode>` (парсить
  `vks_proto::media::RoomMode` по i32, `Unspecified → None`). (2) В `start_async` после успешного hello:
  `if let Some(mode)=hello.room_mode { self.media.switch_mode(mode) }` **до** применения снимка раскладки.
  (3) Не порождать при этом `ModeChanged`-событие (состояние уже в `media()`; иначе дубль с push).
- **Тест.** `Conference` с фейковым транспортом, hello `room_mode=MCU` → `media().mode()==Mcu` и
  `render()` даёт ровно 1 `DrawCommand{participant_id:"composite"}`; hello `SFU` → N команд.

### F-06 (P2). Нет публичного WASM-API: «SDK для браузера» не существует

- **Симптом.** Потребитель не может подключить SDK из JS: нет ни одного `#[wasm_bindgen]` экспорта,
  нет `VksClientSession`, нет способа получить события/раскладку без Rust-обвязки.
- **Причина.** Крейт развивался как Rust-библиотека (native + wasm32 check), JS-граница не проектировалась;
  `ConferenceConfig` требует `Arc<dyn RenderSurface>` (F-09), что не конструируется из JS.
- **Файлы/строки.** `client/vks-client-sdk/Cargo.toml:30-55` (wasm-зависимости есть),
  `src/lib.rs` (нет wasm-модуля), `conference.rs:31`, `FRONTEND_RULES.md §7.1` (целевой API).
- **Исправление (минимальный контракт, ~120 строк).** Отдельный `src/wasm.rs` под `#[cfg(target_arch="wasm32")]`:
  `#[wasm_bindgen] pub struct VksClientSession` c `new(url, token)`, `join_async()`,
  `drain_events_json() -> String`, `mode() -> String`, `layout_json() -> String`,
  `render_json(width,height) -> String` (массив `DrawCommand`, рисует JS), `disconnect()`. Внутри —
  `Conference` + `SignalingClient` (WS-транспорт web-sys уже есть в `webrtc_glue`), без `RenderSurface`.
  Из `ConferenceConfig` убрать `surface` (F-09). Ошибки — `JsValue` со строкой; события — JSON-строки,
  чтобы не тянуть serde-wasm-bindgen.
- **Тест.** `wasm-bindgen-test` (node): конструктор + `mode()` без сети; `cargo check --target wasm32`;
  в CI — сборка `wasm-pack build --target web` (артефакт не коммитить).

### F-07 (P2). В CI нет проверки проектного JS и e2e

- **Симптом.** Синтаксическая ошибка в `codian-layouts.js`/`perf-overrides.js` или регрессия deep link
  уедет в прод незамеченной: CI проверяет манифест и один патч.
- **Причина.** e2e — plain-Node скрипты, не `@playwright/test`; в `ci.yml` нет шагов для них и нет
  `node --check`.
- **Файлы/строки.** `.github/workflows/ci.yml:255-269`; `tests/e2e/*.js` (19 файлов);
  `scripts/patch-pnm-*.py` (все три поддерживают `--check`, вызывается только `layout-crash`).
- **Исправление.** CI: `for f in client/pnm-frontend/deployed/pnm/*.js; do node --check "$f"; done`;
  `python3 scripts/patch-pnm-chartjs.py --check`; `python3 scripts/patch-pnm-audio-in-layouts.py --check`;
  один smoke-спектакль Playwright (deep link + логин + список) на поднятом сервере.
- **Тест.** Собственно эти шаги; «красный» прогон при внесении заведомой опечатки.

### F-08 (P2). SPA-fallback отдаёт `200 text/html` на отсутствующий статический файл под `/pnm/`

- **Симптом.** `GET /pnm/chunks/<нет>.js` → 200 HTML → браузер: «Expected a JavaScript module script but
  the server responded with MIME type text/html»; диагностика затруднена, кэш получает мусор.
- **Причина.** `ServeDir::fallback(ServeFile(index))` не различает navigation и subresource.
- **Файлы/строки.** `frontend.rs:82-88`.
- **Исправление.** Fallback-сервис, который отдаёт `index.html` только при `Accept: text/html` (и/или
  для путей без расширения), иначе `404`. Navigation-запросы браузера всегда шлют `text/html`.
- **Тест.** Добавить в `frontend.rs` (рядом с `pnm_index_and_spa_fallback:480`):
  `/pnm/chunks/missing.js` + `Accept: */*` → 404; `/pnm/monitoring/sessions/<uuid>` + `Accept: text/html`
  → 200 `index.html`.

### F-09 (P2). `ConferenceConfig.surface` — мёртвое поле, мешающее wasm-обёртке

- **Симптом/причина.** `conference.rs:31` хранит `Arc<dyn RenderSurface>`, но `Conference` его не
  использует (рендер принимает surface аргументом). Из JS такой конфиг не собрать.
- **Файлы/строки.** `conference.rs:31,438-460` (тесты передают `NullSurface`).
- **Исправление.** Удалить поле и `NullSurface` из тестов (4 места) — минус ~15 строк и один барьер.
- **Тест.** `cargo test -p vks-client-sdk` (51) остаётся зелёным; wasm32 check.

### F-10 (P2). Тройное перехватывание XHR/fetch + мёртвая ветка previews

- **Симптом.** На каждый XHR навешиваются три обёртки (`open` × 3, `send` × 2), на fetch — две;
  порядок загрузки скриптов влияет на поведение; `previews` синтезируется в клиенте, хотя сервер уже
  отдаёт `{previews: []}`.
- **Причина.** Три независимых модуля решают близкие задачи (ошибки, кэш кодеков, инвалидация после
  сохранения раскладки), каждый ставит свой прототип-патч; ветка previews осталась с тех пор, как
  сервер не умел этот эндпоинт.
- **Файлы/строки.** `api-errors.js:41-57`; `perf-overrides.js:79-84,200-260,237-243`;
  `codian-layouts.js:1350-1395`; сервер: `crates/vks-admin/src/api_v3/sessions.rs:1902-1908`
  (`abonents_previews` → `{previews: []}`).
- **Исправление.** Один `net-hooks.js` с реестром `{match, onRequest, onResponse}`, три модуля
  регистрируются в нём; ветку `PREVIEWS_RE` удалить (сервер — источник истины); `watchSaveButton`
  (эвристика по тексту «Сохранить» + таймеры 1,5/3 с, `codian-layouts.js:1400-1418`) удалить, если
  fetch/XHR-хук покрывает axios (покрывает).
- **Тест.** Юнит в браузере (Playwright): один GET настроек → 1 сетевой запрос; `previews` проходит
  насквозь; после PATCH раскладки кэш числа окон сброшен.

### F-11 (P2). Клиентский TTL 30 с поверх серверного `no-cache` + ETag

- **Симптом.** После смены кодеков в другой вкладке/другим админом список до 30 с может быть старым;
  при этом сервер уже отдаёт `Cache-Control: private, no-cache` и strong ETag, т.е. ревалидация дешёвая.
- **Причина.** `perf-overrides.js:24` TTL 30 с кэширует тело и не делает условный запрос; инвалидация —
  только свой PATCH, `focus` и `storage`-события.
- **Файлы/строки.** `perf-overrides.js:24,250-262`; `crates/vks-admin/src/api_v3/shell.rs:595-619`.
- **Исправление.** Оставить только коалесинг in-flight (N+1 → 1), TTL убрать или снизить до 2-3 с;
  на `visibilitychange` — всегда ревалидировать. Это меньше кода и не спорит с сервером.
- **Тест.** Два «клиента» (контекста) в Playwright: PATCH в одном → GET во втором после `focus`
  возвращает новое значение.

### F-12 (P3). Дрейф документации

- ADR-0027: «MANIFEST — 210 файлов»; `NOTES.md:41`: «deployed (26 MB)», «210 строк»; факт — 214 файла,
  27 MB, 214 строк. `docs/DARK_THEME_PNM_PLAN.md` описывает invert-пилот (`filter: invert(85%)`),
  которого в дереве нет; его же §Risks утверждает «edits are live without restart» — верно только без
  изменения inline-скриптов (CSP считается при старте, B7).
- Правка: синхронизировать числа, пометить план как исторический, уточнить оговорку про CSP.

---

## §4. Разбор примеров заказчика

### 4.1. «IIFE в `perf-overrides.js` + `fetch('/api/v3/sessions')` + `__PROSTOR_INITIAL_SESSION__` + CustomEvent»

**Что утверждается:** прямой заход на `/pnm/monitoring/sessions/<id>` не рендерит сессию, пока не
выбрать её вручную; лечится ранним fetch-ем списка сессий, записью `window.__PROSTOR_INITIAL_SESSION__`
и событием `prostor:select-session`.

**Что на самом деле:**

1. **Рендеринг уже есть** (A2-A5). Маршрут с `:sessionId?`, `sessionId` из `$route`, `immediate`-watch →
   `refreshSession`; живой прогон с токеном это подтвердил. Никакой гидратации не нужно.
2. **Симптом воспроизводится только без токена** (A6): гард теряет `to.fullPath`, логин всегда идёт в
   `{name:"monitoring"}`. IIFE этого не исправит: она выполнится на странице авторизации, получит 401 и
   ничего не отрисует; после логина страница откроется списком.
3. **Контракт API неверен.** `fetch('/api/v3/sessions')`:
   - эндпоинт админский, требует роль **Moderator** и `Authorization: Bearer …` из
     `localStorage["TrueConfMCU_AccessToken"]` (куки не используются) — «голый» fetch даёт `401`;
   - форма ответа — `{sessions:[…], count}` (`crates/vks-admin/src/api_v3/sessions.rs`, `list`),
     **не** `.items`;
   - сам UI ходит иначе: список — `GET /api/v3/ui/sessions` (`index.bundle…js:1@986254`), одна сессия —
     `GET /api/v3/ui/sessions/{id}` → `{session}` (`:1@1025634`). То есть пример использует эндпоинт,
     которого UI не использует вообще.
4. **Событие некому слушать.** `prostor:select-session` и `__PROSTOR_INITIAL_SESSION__` не встречаются
   в репозитории ни разу; вендорский код о них не знает. Запись в Pinia возможна только через хак
   `document.getElementById('root').__vue_app__.config.globalProperties.$pinia` (так делает
   `codian-layouts.js:202-213`), но на момент выполнения проектного скрипта (`index.html:30-33`,
   до модульного бандла) `__vue_app__` ещё нет — потребуется polling, а это гонка с собственным
   `refreshSession` компонента (двойной запрос, возможен откат на «чужую» сессию).
5. **CSP/манифест.** Внешний файл политику не ломает (`script-src 'self'`), inline — потребовал бы
   рестарта сервиса (CSP считается при старте), чего в рамках этой задачи делать нельзя.

**Минимально правильный вариант:** ничего не добавлять для авторизованного сценария; для «холодной»
ссылки — патч гарда/логина (F-01). Если нужен именно клиентский мост без патча бандла, единственный
корректный путь — `sessionStorage['vks:returnTo']` на странице авторизации + перехват успешного логина
через наблюдатель `history.pushState`, но это более хрупко, чем 6 строк патча, и всё равно не решает
случай истёкшего токена на уже открытой странице (там 401-интерсептор). Рекомендация: патч.

### 4.2. «Заменить старый акцент `#0097A7` на `#17212B`, обновить MANIFEST.sha256 (сервер пересчитает CSP)»

- **`#17212B` — не акцент.** В проекте это `--c-surface` (`index.html:283`), комментарий прямо говорит
  «Telegram Surface» (`:270`). Акцент — `--c-primary:#0088cc` (`:290`), вторичный — `#2aabee`.
  Замена акцента на surface сделает кнопки/ссылки неотличимыми от фона.
- **`#0097A7` — вендорский primary в CSS, а не «забытый» цвет в HTML.** `.mdc-button--text{color:#0097a7}`
  (`chunks/index.bundle-2.2.1.66.css:1@27504`), градиенты статусов, спиннеры, 4 SVG. Миграция
  зафиксирована как отдельная незакрытая фаза (`NOTES.md:69-71`), и она требует правки CSS с
  cache-bust, а не `<meta>`.
- **В `index.html` `#0097A7` — только `theme-color` (строка 37), и он перетирается** `theme-toggle.js`
  (`:27`) при загрузке: правка строки 37 без правки переключателя визуально ничего не изменит
  (останется лишь значение для PWA-манифеста/панели браузера до первого скрипта).
- **`MANIFEST.sha256` с CSP не связан.** CSP строится при старте из inline-скриптов
  (`frontend.rs:66,246`); манифест — sha256 дерева (`sha256sum -c` 214/214, CI). Обновлять манифест
  нужно после любой правки файлов — это provenance, а не CSP.
- **Чего стоит хотеть вместо этого:** (1) один источник токенов темы (F-04); (2) `theme_color` в
  манифесте = фон тёмной темы; (3) перенос вендорского teal — отдельная задача с правкой CSS/патчем и
  пересборкой манифеста; (4) при правке inline-скриптов — рестарт сервиса (иначе CSP заблокирует новый
  скрипт), при правке только `<meta>`/внешних файлов — не нужен.

### 4.3. «WASM SDK автоматически выбирает конвейер по `ServerHello.room_mode`: mcu — один композит + пиксельные координаты; sfu — N simulcast + CSS Grid/Taffy из `vks-layout`; пример `VksClientSession`»

**Что есть:**

- `RoomMode{Sfu(default), Mcu}` + `switch_mode` + сброс раскладки (`media_engine/mod.rs:22-92`);
- `McuRenderer` — ровно один `DrawCommand{participant_id:"composite", rect:(0,0,w,h)}`;
- `SfuRenderer` — `GridLayout::compute(&Canvas, n)` → N прямоугольников; `compose_from_layout` и
  `compose_from_normalized` (масштабирование нормализованной раскладки в пиксели);
- push `RoomModeChanged` → `switch_mode` (`conference.rs:113-114`), снимок раскладки из
  `JoinRoomResult` (`conference.rs:240`), guard по версии (`drain_events`);
- 51 тест, wasm32-сборка проходит.

**Чего нет:**

- `ServerHello.room_mode` **отбрасывается** (`signaling_client.rs:17-23`), `start_async` его не читает
  (`conference.rs:206-224`) → автовыбора при join нет (F-05). Сервер поле шлёт (`handler.rs:288`).
- Публичного WASM-API нет: ни `#[wasm_bindgen]`, ни `VksClientSession` (F-06). `FRONTEND_RULES §7.1`
  (`join`, `on_event`, `mute/unmute`, `start_screen_share`, `Participant`, `Track`,
  `create_render_surface`) не реализован ни в одном пункте; реальный API —
  `Conference::new(cfg)` + `start_async(&client)` + `drain_events()`.
- «CSS Grid/Taffy» — неточно: Taffy в проекте отсутствует; `vks-layout` считает нормализованные rect
  (доли холста, ADR-0030), SDK отдаёт пиксельные `DrawCommand`; в DOM их раскладывает потребитель
  (`RenderSurface`). «N simulcast» — тоже про медиа-слой (WebRTC), а не про layout: SDK раскладывает
  N **участников** (по одному активному треку), simulcast-слои выбирает браузер.

**Минимальный корректный каркас (если делать):**

```rust
// src/wasm.rs  — только wasm32, ~120 строк
#[wasm_bindgen]
pub struct VksClientSession { conf: Conference, client: SignalingClient }

#[wasm_bindgen]
impl VksClientSession {
    #[wasm_bindgen(constructor)]
    pub fn new(url: String, token: String) -> Result<VksClientSession, JsValue> { /* config без surface */ }
    pub async fn join(&mut self) -> Result<(), JsValue> { /* start_async + switch_mode(hello.room_mode) */ }
    pub fn mode(&self) -> String { /* "mcu" | "sfu" */ }
    pub fn drain_events_json(&mut self) -> String { /* очередь → JSON-массив */ }
    pub fn layout_json(&self) -> String { /* нормализованная раскладка */ }
    pub fn render_json(&self, w: u32, h: u32) -> String { /* Vec<DrawCommand> → JSON, рисует JS */ }
    pub fn disconnect(&mut self) { /* leave + close */ }
}
```

Порядок работ: F-05 (2 строки в структуре + 2 в `start_async`, тест) → F-09 (убрать `surface`) →
`wasm.rs` + `wasm-bindgen-test` → `wasm-pack` в CI. Без F-05 обёртка будет «врать» о режиме до первого
push-кадра.

---

## §5. План (файлы/строки/тесты)

Порядок — по соотношению «риск ↓ / польза ↑». Все правки — с пересборкой `MANIFEST.sha256`
(`cd client/pnm-frontend/deployed/pnm && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum > ../../MANIFEST.sha256`,
как в `import…sh`); рестарт сервиса — только если меняются inline-скрипты `index.html` (CSP).

| # | Приоритет | Файлы | Изменение | Тесты |
|---|---|---|---|---|
| 1 | P1 | `scripts/patch-pnm-auth-deeplink.py` (новый), `chunks/index.bundle…js`, `chunks/index4.bundle…js`, `import…sh`, `.github/workflows/ci.yml` | F-01: `redirect` в гарде/401, возврат после логина и смены пароля; идемпотентный патч с `--check` | `--check` в CI; Playwright deep link без токена → логин → та же сессия |
| 2 | P1 | `import-pnm-frontend.sh:129-138,163,224-228` | F-02: `theme-toggle.js` в проектный слой; финальная проверка «каждый `<script src>` существует» | `bash -n`; прогон в `mktemp -d`; CI-шаг проверки ссылок |
| 3 | P1 | `Cargo.toml:62` + `crates/vks-server/src/lib.rs` (`build_app`) **или** `scripts/pnm-gzip.sh` | F-03: `CompressionLayer` (рестарт) **или** генерация `.gz` после манифеста + проверка свежести | `curl -H 'Accept-Encoding: gzip'` → `content-encoding: gzip`; CI: `.gz` свежее исходника |
| 4 | P2 | `index.html:37`, `manifest.webmanifest:9-11`, `theme-toggle.js:24-27` | F-04: один источник токенов; `theme_color` = фон темы; `start_url` → `/pnm/` | `scripts/check-theme-tokens.py --check` в CI |
| 5 | P2 | `crates/vks-server/src/frontend.rs:82-88,480` | F-08: fallback только для `Accept: text/html` | новый тест: `/pnm/chunks/missing.js` → 404 |
| 6 | P2 | `client/vks-client-sdk/src/signaling_client.rs:17-23`, `conference.rs:206-224` | F-05: `room_mode` в hello + `switch_mode` при join | тест: hello MCU → 1 composite; hello SFU → N |
| 7 | P2 | `conference.rs:31`, `src/wasm.rs` (новый), `Cargo.toml` | F-06/F-09: убрать `surface`, добавить `VksClientSession` | `wasm-bindgen-test`; `wasm-pack build --target web` в CI |
| 8 | P2 | `perf-overrides.js:24,237-262`, `codian-layouts.js:1400-1418`, `api-errors.js:41-57` | F-10/F-11: единый `net-hooks.js`, удалить previews, TTL, `watchSaveButton` | Playwright: 1 GET настроек; PATCH → второй контекст видит новое |
| 9 | P2 | `ci.yml:255-269` | F-07: `node --check` ×4, `--check` всех трёх патчей, один Playwright smoke | Красный на заведомой опечатке |
| 10 | P3 | `NOTES.md:41,61,94`, ADR-0027, `DARK_THEME_PNM_PLAN.md` | F-12: 214/27 MB, оговорка про CSP-рестарт, план — в историю | — |

**Что делать НЕ нужно:** hydration-IIFE, `__PROSTOR_INITIAL_SESSION__`, `prostor:select-session`,
`fetch('/api/v3/sessions')` — см. §4.1.

**Оценка трудозатрат:** п.1-3 ≈ 8-10 ч (с тестами), п.4-6 ≈ 6 ч, п.7 ≈ 10-12 ч, п.8-9 ≈ 6 ч,
п.10 ≈ 1 ч. Итого ≈ 31-35 ч; из них P1 — 10-12 ч.

---

## §6. «Меньше кода»

| Артефакт | Строк | Что не нужно | Экономия |
|---|---|---|---|
| `perf-overrides.js` — ветка previews (`:27,237-243`, статистика `:34`) | ~15 | Сервер уже отдаёт `{previews: []}` (`api_v3/sessions.rs:1902-1908`); клиентская подмена маскирует будущую реализацию | −15 |
| `perf-overrides.js` — TTL-кэш (`:24,250-262`) | ~20 | Сервер: `no-cache` + strong ETag (`shell.rs:595-619`); оставить только коалесинг in-flight | −15…−20 |
| Три перехватчика XHR/fetch (`perf-overrides.js:79-84,200`; `api-errors.js:41-57`; `codian-layouts.js:1350-1395`) | ~110 | Один `net-hooks.js` с реестром правил | −40…−60 |
| `codian-layouts.js` — `watchSaveButton` (`:1400-1418`) | ~19 | Дублирует fetch/XHR-хук; эвристика по тексту кнопки + два таймера | −19 |
| `ConferenceConfig.surface` + `NullSurface` в тестах (`conference.rs:31,438-460`) | ~15 | Поле не используется; блокирует wasm-обёртку | −15 |
| `media_engine::Dc` (публичный alias) | 1 | Не используется нигде (`grep "::Dc"` → 0) | −1 |
| `SfuRenderer::compose` (`sfu_renderer.rs:27-45`) | ~19 | Используется только в тестах; боевой путь — `compose_from_layout/normalized` | −19 (если API не нужен) |
| `theme-toggle.js` — `MutationObserver` + `DOMContentLoaded` + `load` + `pageshow` + `storage` | ~25 | Достаточно `apply()` при старте + `storage`; класс темы больше никто не меняет | −15 |

Суммарно: **≈120-150 строк** можно убрать без потери функций; главный выигрыш — не строки, а
**один** слой сетевых хуков вместо трёх (предсказуемый порядок, одна точка отладки).

Отдельно: `docs/DARK_THEME_PNM_PLAN.md` (≈200 строк) описывает несуществующий пилот — перенести в
`docs/progress/archive/`, чтобы не порождал новых «правок по плану».

---

## §7. Риски и неизвестное

| Риск | Влияние | Митигация |
|---|---|---|
| Патч вендорского бандла (F-01) ломается при следующем апгрейде PNM | глубокие ссылки снова теряются, импорт падает | патч fail-closed по числу якорей + `--check` в CI (как у существующих трёх) |
| `theme-toggle.js` теряется при импорте (F-02) | нет кнопки темы, 404 в консоли, «мигание» meta | добавить в проектный слой + проверка ссылок в конце импорта |
| Включение gzip (F-03) меняет набор файлов/манифест | дрейф CI, stale `.gz` | вариант A (`CompressionLayer`) не трогает файлы; вариант B — проверка свежести |
| Правка inline-скрипта `index.html` без рестарта | новый скрипт заблокирован CSP | правило: inline не трогать; внешние файлы — можно |
| `unsafe-eval` в CSP | XSS-усиление при инъекции | вендор требует; не расширять; следить за отсутствием `v-html` с пользовательскими данными (исчерпывающе не проверял) |
| Токен в `localStorage` | кража при XSS | вендорская схема; менять — отдельная задача (ADR по безопасности) |
| Playwright-проверки шли под `admin` на живом стенде | логин пишет запись в аудит; сессии — тестовые (`E2E Probe …`) | данные не менялись; для CI — отдельный стенд/пользователь |
| «Сессия не найдена» молча редиректит в список (A8) | пользователь не понимает, почему ссылка «не работает» | добавить тост/сообщение (P2, вне критического пути) |
| Неизвестно: полный XSS-аудит вендорского `v-html`/`innerHTML` | потенциальный XSS | отдельная задача; CSP уже без `unsafe-inline` для скриптов |
| Неизвестно: поведение PWA-установки без SW на Android | установка может не предлагаться | добавить SW (кэш shell) или убрать `display: standalone` из ожиданий |

---

## §8. Оценка /10

| Зона | Оценка | Обоснование |
|---|---|---|
| Статика и сервер (`frontend.rs`) | **9/10** | SPA-fallback, CSP из байтов, cache-policy, traversal/symlink-тесты, 304/Range; минус — fallback для subresource (F-08) и отсутствие сжатия (F-03) |
| Проектный слой (4 JS, 1872 строки) | **7/10** | Работает и аккуратно написан (XSS-безопасно: `textContent`, атрибуты, числовой SVG; `node --check` OK), но три перехватчика, дубли кэша, мёртвые ветки, ноль юнит-тестов |
| Импорт/целостность | **6/10** | Идемпотентность, манифест, CI-проверка — сильные; но `theme-toggle.js`/цвет/манифест не переживают импорт (F-02, F-04) |
| SDK (`vks-client-sdk`) | **6/10** | Renderers, `RoomMode`, reconnect, 51 тест, wasm32 — хорошо; нет wasm-API (F-06), игнор `room_mode` (F-05), мёртвый `surface` (F-09), нет интеграционных тестов |
| Тесты/e2e | **5/10** | 19 содержательных сценариев, включая deep link (доказали A5), но вне раннера и вне CI (F-07) |
| Документация | **5/10** | ADR точные и полезные; дрейф чисел (210/212/214, 26/27 MB), устаревший план тёмной темы |
| **Итого** | **6,5/10** | Функционально лучше, чем следует из претензий заказчика; процесс (импорт, сжатие, CI) — слабое место |

---

## §9. Спорные тезисы

### Т1. «Прямой переход на сессию не работает без ручного выбора»

- **Тезис:** без hydration-IIFE deep link не рендерит сессию.
- **Доказательство (обратное):** маршрут и `immediate`-watch в бандле (A2-A4) + живой холодный заход с
  токеном: карточка, заголовок из API, 6 вкладок, 0 ошибок; ранее — `audio-in-layouts.js` (2026-10-23,
  `ok:true`). SPA-fallback сервера отдаёт 200 `index.html` на любой `/pnm/*`.
- **Что переубедит:** воспроизводимый прогон, где при **валидном** токене и пустом кэше страница
  остаётся пустой >2 с и в сети нет `GET /api/v3/ui/sessions/<id>`; или скриншот с пустым
  `.main-card` при статусе 200 на документе.

### Т2. «Причина — отсутствие гидратации, лечится fetch-ом списка сессий»

- **Тезис:** достаточно IIFE с `fetch('/api/v3/sessions')` и записью в Pinia.
- **Доказательство:** без токена fetch вернёт 401 (гард уже увёл на `/pnm/authorization`); форма ответа —
  `{sessions,count}`, а UI использует `/api/v3/ui/sessions/{id}` → `{session}`; событие
  `prostor:select-session` никто не слушает; `__vue_app__` на момент проектного скрипта не существует;
  после логина роутер всё равно уходит в `{name:"monitoring"}`. Живой прогон: deep link → форма →
  логин → список.
- **Что переубедит:** патч или демонстрация, где без токена ссылка после логина возвращает на исходный
  id, и это объясняется только IIFE (а не правкой гарда/логина).

### Т3. «`#17212B` — новый акцент вместо `#0097A7`»

- **Тезис:** старый акцент остался в `index.html`/манифесте, заменить на `#17212B`.
- **Доказательство:** `#17212B` — `--c-surface` (`index.html:283`, комментарий `:270`); акцент —
  `#0088cc` (`:290`). В `index.html` teal один раз — `theme-color` (`:37`), и его перетирает
  `theme-toggle.js:27`. Вендорский teal — в CSS (`.mdc-button--text`, `:1@27504`), и его миграция —
  отдельная отложенная фаза (`NOTES.md:69-71`).
- **Что переубедит:** дизайн-спека, где `#17212B` явно назначен акцентом (primary), и макет с кнопками
  этого цвета на фоне `#0E1621`; либо подтверждение, что PWA-манифест — единственный потребитель
  `theme-color` и вендорский CSS не важен.

### Т4. «MANIFEST.sha256 нужен для CSP, сервер сам пересчитает»

- **Тезис:** правка цвета требует обновить манифест, потому что сервер пересчитывает CSP-хэши.
- **Доказательство:** CSP строится из inline-скриптов при старте (`frontend.rs:66,246`), манифест — sha256
  дерева (`sha256sum -c` 214/214) и используется CI/процедурой импорта. Правка `<meta>` не меняет CSP;
  добавление inline-скрипта — меняет, но требует **рестарта** (иначе скрипт заблокируется).
- **Что переубедит:** код, где CSP читается с диска на каждый запрос (сейчас — нет), или тест,
  показывающий иное.

### Т5. «Автопереключение MCU/SFU по `ServerHello.room_mode` работает»

- **Тезис:** SDK сам выбирает конвейер по hello.
- **Доказательство:** `signaling_client.rs:17-23` — поля `room_mode` нет; `conference.rs:206-224` —
  hello читается только на `server_version`; сервер шлёт поле (`handler.rs:288`, `signaling.proto:60`).
  Переключение происходит только по push `RoomModeChanged` (`conference.rs:113-114`), т.е. после первого
  изменения в комнате.
- **Что переубедит:** тест (или лог), где join в MCU-комнату сразу даёт `media().mode()==Mcu` и 1
  `DrawCommand{composite}` до каких-либо push-кадров.

### Т6. «Старые дефекты (482/483 файла, грязное дерево) актуальны»

- **Тезис:** рантайм неполный, дерево грязное.
- **Доказательство:** 483 файловых записи, 1 битая (дубль `index-legacy.js` в `source/`, не рантайм);
  отсутствующий `BiOEwveA.svg` заменён на `prostor-wordmark.svg` в ребренде (`NOTES.md:61`);
  `git status` — 0 изменённых отслеживаемых файлов (только untracked `docs/audit/2026-10-29/`).
- **Что переубедит:** список файлов, на которые ссылается `index.html`/локали, но которых нет в дереве;
  или `git status`/`git diff` с изменениями в `deployed/pnm` без коммита.

### Т7. «Импорт PNM безопасен: проектный слой под маркерами»

- **Тезис:** `import-pnm-frontend.sh` сохраняет все наши правки.
- **Доказательство:** сохраняются 3 JS + CSS-блок (`import…sh:129-138,224-260`); `theme-toggle.js`
  (коммит `7eb0b6f`, добавлен позже) в списке нет, но его `<script>` — в сохраняемом блоке
  (`index.html:32`); `theme-color` (`index.html:37`) и `manifest.webmanifest` не сохраняются/не патчатся.
  Ребренд-слой (словник) официально требует ручного реапплая (`NOTES.md:94`), т.е. процедура уже
  предполагает ручные шаги — и один из них забыт для `theme-toggle.js`.
- **Что переубедит:** прогон импорта в чистом каталоге с последующим `sha256sum -c` и живой страницей,
  где кнопка темы работает и 404 в консоли нет.

---

## Приложение A. Журнал проверок (2026-10-29)

1. **Deep link с токеном (Chromium 1243, headless).** Логин `POST /api/v3/login` → 200, токен;
   `GET /api/v3/ui/sessions?has_stopped=false` → 3 сессии; `addInitScript` кладёт токен в
   `localStorage["TrueConfMCU_AccessToken"]`; `goto /pnm/monitoring/sessions/02785e07-3156-4fc7-b172-0c218ac694e4`.
   Итог: `{url:"/pnm/monitoring/sessions/02785e07-…", sessionCard:true,
   tabs:[control-panel, general, layouts, skins, record-and-stream, additional],
   header:"E2E Probe 1793203161384", errors:[]}`; сетевые запросы — `…/abonents`, `…/audiochannels`.
2. **Deep link без токена.** `goto` того же URL → `/pnm/authorization`; UI-логин `admin` →
   `/pnm/monitoring/sessions` (id потерян), вкладок сессии нет.
3. **Сервер.** `GET /pnm/monitoring/sessions/probe-id` → `200 text/html`, 74 989 B, CSP c 5 `sha256-…`;
   `GET /pnm/chunks/nope.js` → `200 text/html`; `GET /mcu/chunks/nope.js` → `404`;
   `GET /mcu/assets/manifest.webmanifest` → `200 application/manifest+json`, `Cache-Control: no-cache`;
   `GET /mcu/chunks/index.bundle…js` с `Accept-Encoding: gzip` → **без** `content-encoding`, 1 267 133 B.
4. **Целостность.** `sha256sum -c MANIFEST.sha256` → 214/214 OK; `find -name '*.gz'` → 0;
   `find \( -name '*.bak*' -o -type l \)` → 0; `git status --porcelain` → 0 изменённых.
5. **JS.** `node --check` для `api-errors.js`, `perf-overrides.js`, `theme-toggle.js`,
   `codian-layouts.js` → OK. `npx playwright test --list` → «No tests found» (скрипты — plain Node).
6. **SDK.** `cargo test -p vks-client-sdk --lib` → 51 passed; `cargo check -p vks-client-sdk --target
   wasm32-unknown-unknown` → Finished; `cargo check -p vks-client-sdk` — 0 warnings;
   `grep -rn "wasm_bindgen" client/vks-client-sdk/src` → 0; `grep -rn taffy` → 0.
7. **Символы из примера.** `grep -rl "prostor:select-session\|__PROSTOR_INITIAL_SESSION__"` по
   `client/ scripts/ docs/ crates/ tests/` → 0 совпадений.
8. **Архив.** Разбор ZIP: 484 записи (1 каталог + 483 файла), 1 файл с битым deflate
   (`source/…/index-legacy.js`) → 482/483.

---

# Дополнение субагента «L0/frontend» (2026-10-29, 07:16–07:45)

> **О коллизии файла.** Этот отчёт писали два субагента в один путь: основной корпус (§1–§5,
> Приложение A) — агент `frontend-sdk` (записан 07:23); ниже — дополнение второго субагента
> (зона L0 + независимые перепроверки). Основной корпус **не перезаписывался** и не правился:
> всё, что ниже, — аддитивно. Расхождений по фактам нет, кроме явно отмеченных уточнений в §12
> (одно из них меняет план §5, п.6).

## §11. L0-хост: sysctl + cgroup v2 (в основном корпусе отсутствует — он вынесен в `08_live_runtime.md`, но проверен и здесь)

### 11.1 Что есть в дереве

`deploy/systemd/vks-mcu-prod.service` (prod):

| Параметр | Значение | Комментарий |
|---|---|---|
| `DynamicUser` | `yes` | uid выделяется на старт; cgroup-учёт работает штатно |
| `LimitNOFILE` | `65536` | **единственный реально работающий лимит** (live: `/proc/1814143/limits` → `Max open files 65536 65536`) |
| `CapabilityBoundingSet` | **пусто** | ⇒ нет `CAP_SYS_NICE`/`CAP_IPC_LOCK`; любой будущий `sched_setscheduler`/`mlockall` упадёт с `EPERM` |
| `ProtectSystem=strict`, `ProtectKernelTunables=true` | да | `/opt`,`/etc` ro; sysctl из процесса писать нельзя (и не нужно) |
| `MemoryDenyWriteExecute=true` | да | запрещает JIT — учесть, если на сервере когда-нибудь появится wasm/JIT |
| `NoNewPrivileges`, `PrivateTmp/Devices`, `RestrictAddressFamilies` | да | hardening в порядке |
| **`Slice`/`CPUWeight`/`MemoryHigh`/`MemoryMax`/`TasksMax`/`LimitRTPRIO`/`LimitMEMLOCK`** | **отсутствуют** | P2-4: заявленных в матрице cgroup-настроек в юните нет |
| `RestartPreventExitStatus=78`, `KillMode=mixed`, `TimeoutStopSec=30` | да | ок |

`deploy/systemd/vks-mcu.service` — **user-unit** стенда (`%h/mcu`, `StandardOutput=append:%h/mcu/vks_server.log`),
лимитов кроме `LimitNOFILE=65536` нет; живёт в
`user.slice/user-1000.slice/user@1000.service/app.slice/vks-mcu.service`. Запись лога в
репозиторий — известный компромисс dev-юнита (в prod-юните `LogsDirectory=vks`).

### 11.2 Состояние хоста (live, 07:2x)

```
stat -fc %T /sys/fs/cgroup            → cgroup2fs            (unified, v2)
cat /sys/fs/cgroup/cgroup.controllers → cpuset cpu io memory hugetlb pids rdma misc dmem
sysctl net.core.rmem_max net.core.wmem_max net.core.netdev_max_backlog kernel.sched_rt_runtime_us
                                      → 4194304  4194304  1000  950000
grep -E 'Max open files|Max nice' /proc/1814143/limits
                                      → 65536 65536 ; Max nice priority 0   (rtprio = 0)
cat /proc/1814143/cgroup              → 0::/user.slice/…/app.slice/vks-mcu.service
```

Т.е. ядро готово (`memory`, `cpu`, `pids` контроллеры доступны), но **юнит их не использует**:
`memory.max=max`, `cpu.weight=100` — дефолты.

### 11.3 Код: почему sysctl/rtprio сейчас некуда применить

```
grep -rn "socket2\|SO_RCVBUF\|so_rcvbuf\|sched_setscheduler\|mlockall" crates Cargo.toml  → 0
```

UDP-сокеты создаются только через `tokio::net::UdpSocket::bind`:
`crates/vks-gateway-sip/src/rtp_bridge.rs:47-48` (RTP+RTCP пары), `crates/vks-gateway-sip/src/uac.rs:195`,
`crates/vks-server/src/gateways.rs:2274`. `tokio` не даёт `setsockopt` — нужен `socket2` (или
`std::net::UdpSocket` + `setsockopt` → `UdpSocket::from_std`), плюс `socket2` в `Cargo.toml`
(сейчас его там нет).

### 11.4 Что даст эффект, а что театр (при текущем коде)

| Настройка | Эффект | Почему |
|---|---|---|
| `LimitNOFILE=65536` | **реальный** (уже есть) | ядро применяет к процессу; проверено live |
| `MemoryHigh` (напр. `2G`) | **реальный** | ядро тормозит cgroup при превышении; кода не требует; предохранитель от OOM-каскада под MCU-нагрузкой |
| `MemoryMax` (напр. `3G`) | **реальный, грубый** | OOM-kill всей cgroup; ставить выше `MemoryHigh` |
| `TasksMax` (напр. `4096`) | **реальный** | защита от fork-бомбы sidecar'ов (`VKS_H323_UAS_MAX_CHILDREN` уже есть в env) |
| `Slice=vks.slice` | организационный | нужен, чтобы weight/memory применялись к группе (сервис + h323/ffmpeg sidecar'ы) |
| `CPUWeight` | **слабый** | влияет только при конкуренции; на стенде сервис один ⇒ почти ноль |
| `LimitRTPRIO` | **театр** | нет `sched_setscheduler`; плюс `CapabilityBoundingSet=` пуст ⇒ `EPERM` даже с лимитом |
| `LimitMEMLOCK` + `mlockall` | **театр** | `mlockall` в коде нет; без `CAP_IPC_LOCK` не сработает |
| `net.core.rmem_max=4 MiB` | **театр** | это потолок для `SO_RCVBUF`; приложение буфер не растит ⇒ действует `rmem_default` (~208 KiB) |
| `net.core.netdev_max_backlog=1000` | почти театр | важен при burst на 10G; для UDP-медиа на 1G редко узкое место |
| `vm.swappiness`, THP, `kernel.sched_*` | театр/риск | без профиля — вредно |

### 11.5 Минимальный deploy-набор

1. `deploy/systemd/vks.slice` (новый, ~6 строк): `CPUWeight=200`, `MemoryHigh=2G`, `MemoryMax=3G`,
   `TasksMax=4096`; в юнит — `Slice=vks.slice`.
2. `LimitNOFILE` **не менять**: при `VKS_WEBRTC_MAX_PEERS=256` это ~600 fd против 65 536 —
   запас >100×; решение зафиксировать в `deploy/README.md` как осознанное.
3. `/etc/sysctl.d/90-vks.conf` (~6 строк): `rmem_max/wmem_max=8388608`,
   `rmem_default/wmem_default=1048576`, `netdev_max_backlog=16384`, `udp_rmem_min=262144`.
   Честно: **до патча `SO_RCVBUF`** из этого набора реально работает только `rmem_default`
   (влияет на все UDP-сокеты без правок кода); остальное — «потолки», которые код не использует.
4. **Не добавлять** `LimitRTPRIO`, `LimitMEMLOCK`, `AmbientCapabilities=CAP_SYS_NICE|CAP_IPC_LOCK`
   до появления `sched_setscheduler`/`mlockall`: расширение привилегий без потребителя.

### 11.6 Критерии проверки (обязательны — иначе «настроено» = «записано»)

```bash
sysctl --system | tail -5
sysctl -n net.core.rmem_max net.core.rmem_default net.core.netdev_max_backlog
systemctl show vks-mcu-prod -p Slice -p LimitNOFILE -p MemoryHigh -p MemoryMax -p CPUWeight -p TasksMax
cat /sys/fs/cgroup/vks.slice/memory.high /sys/fs/cgroup/vks.slice/memory.max
systemd-analyze verify deploy/systemd/vks-mcu-prod.service deploy/systemd/vks.slice
pid=$(systemctl show -p MainPID --value vks-mcu-prod); grep -E 'Max open files|realtime' /proc/$pid/limits
chrt -p $pid                                     # SCHED_FIFO — только после патча sched_*
ss -u -a -m | head -20                           # skmem:(r<rcvbuf>) — единственное доказательство, что sysctl не театр
nstat -az UdpRcvbufErrors UdpInErrors            # до/после эталонной нагрузки; цель — 0 приростов
```

Приёмка L0: (а) `systemctl show` подтверждает `Slice/MemoryHigh/MemoryMax/CPUWeight`;
(б) `ss -u -m` показывает rcvbuf ≈ 1–8 MiB на медиа-сокетах; (в) `nstat` не растёт
`UdpRcvbufErrors`; (г) `chrt -p` = `SCHED_FIFO` **только если** появился `sched_setscheduler`,
иначе пункт не заявлять.

### 11.7 Дефекты и оценка L0

- **P2-4.** cgroup v2 не используется: нет `Slice/CPUWeight/MemoryHigh/MemoryMax/TasksMax`
  (хост поддерживает — `cgroup.controllers` содержит `memory cpu pids`).
- **P2-5.** `CapabilityBoundingSet=` пуст + `ProtectKernelTunables=true`: любые будущие
  `sched_setscheduler`/`mlockall` молча упадут (`EPERM`). Это надо знать **до** их написания.
- **P2-6.** В `deploy/` нет ни одного критерия проверки (sysctl/show/getsockopt); «настройка
  применена» сейчас недоказуема.

**Оценка C: 2/10.** +2: `LimitNOFILE` работает и проверен; юниты аккуратные (hardening,
`DynamicUser`, `RestartPreventExitStatus=78`); хост на cgroup v2. −2: заявленных cgroup-настроек
нет. −2: `LimitRTPRIO`/`rmem_max` — театр без кода, и пустой `CapabilityBoundingSet` блокирует
будущий `sched_*`. −2: нет критериев проверки. −1: dev-юнит пишет лог в репозиторий.
**Трудоёмкость:** ~12 строк ini + 6 строк sysctl + 15 LOC Rust (`socket2`/`SO_RCVBUF`) + ~25 LOC
check-скрипта (`ss -m`/`nstat`) ≈ **0,5–1 день**, из них 0,5 дня — доказательства.

## §12. Уточнения к основному корпусу (проверено независимо)

**12.1. Меняет план §5, п.6: `ServerHello.room_mode` недостаточно — поля нет в `JoinRoomResult`.**
`crates/vks-proto/proto/signaling.proto:82-89` — `JoinRoomResult{participant_id, join_nonce,
participants, publisher_tracks, layout, caps}`; `room_mode` есть только в `ServerHello`
(`signaling.proto:60`, поле 6). При этом `ServerHello` отправляется в ответ на `ClientHello`
(`handler.rs:280-292`), а `room_id` появляется лишь в `JoinRoom` (`handler.rs:296+`) ⇒ **per-room
режим в hello физически не помещается**, только глобальный дефолт. Правильный минимум:

1. добавить `vks.media.RoomMode room_mode = 7;` в `JoinRoomResult` (`signaling.proto:82-89`);
2. в `on_join_room` заполнить из того же источника, что media-решения —
   `state.mode.effective_mode(&room_id)` (`crates/vks-server/src/lib.rs:840-846`), а не из
   `self.config.room_mode`;
3. в SDK поле сейчас **теряется при декодировании**: собственные структуры
   `signaling_client.rs:17-24` (`ServerHello{server_version, session_id, server_time_ms}`) и
   `signaling_client.rs:27-37` (`JoinRoomResult`) — поля нет ни в одной; добавить в обе +
   применить в `conference.rs:206-224` (сейчас проверяется только `hello.server_version`).

Приоритет источников: `JoinRoomResult` (per-room) > `ServerHello` (дефолт сервера) > `Default::Sfu`.
Тест, который сейчас невозможен: join в mcu-комнату → `current_mode()==Mcu` **без** единого
`RoomModeChanged`.

**12.2. Per-room режим уже существует на сервере — его надо не «добавить», а «донести».**
`room_mode_override` (`crates/vks-storage/src/postgres.rs:208-245`; таблица есть и в sqlite-стенде
`vks_data.db`, 0 строк) + `rooms.mode` (в стенде 5 комнат, все `sfu`) + `effective_mode(&room_id)`
(`lib.rs:840-846`, решение «WebRTC в MCU-комнату не бриджим»). То есть асимметрия: сервер знает
per-room режим, клиент — нет.

**12.3. Мелкая неточность в §2, C1: `switch_mode` не сбрасывает раскладку.**
`media_engine/mod.rs:77-79` — только `self.mode = mode;`; сброс раскладки — отдельный
`clear_layout` (`:90-92`). Влияет на ожидания теста из §5, п.6: после `switch_mode` раскладка
остаётся, и `render()` в MCU-ветке её игнорирует, а при возврате в SFU — применит снова.

**12.4. Независимое подтверждение F-01 (живой прогон, чистый контекст Playwright).**
`goto /pnm/monitoring/sessions/1111…5555` → `status 200`, итоговый URL `/pnm/authorization`,
запросов к `/api/v3/sessions` — **0**, ошибок консоли/`pageerror` — **0**. Совпадает с A6 и
Приложением A, п.2: id теряется, авторизация не помнит цель.

**12.5. Добавка к F-04 (тема): `theme-color` после загрузки = `#0088CC`, и dark — не дефолт.**
Замер: после `goto /pnm/` (headless Chromium, пустой localStorage) `meta[theme-color]` =
`#0088CC`, `html.className` = `theme-light` — **при** `class="theme-dark"` в разметке
(`index.html:2`). Причина: `theme-toggle.js:9-15` при пустом ключе уважает
`prefers-color-scheme`, а `mount()` (`:80-82`) перезаписывает и класс, и meta. Следствие для
приёмки «тёмного PNM»: на светлой ОС пользователь видит светлую тему; статический `#0097A7`
(`index.html:37`) не виден никогда (перезаписывается в первые миллисекунды). Целевой инвариант
для F-04: `meta[theme-color] == computed --c-bg` активной темы (`#0E1621` для dark — совпадает
с тем, что уже пишет `theme-toggle.js:27`), а `#17212B` (`--c-surface`, `index.html:283`) — это
surface, его место в `manifest.theme_color`, не в `meta`.

**12.6. Альтернатива для F-01 (на выбор Lead'а): внешний скрипт вместо патча бандлов.**
План §5, п.1 патчит `chunks/index.bundle`/`index4.bundle` python-скриптом с `--check` в CI —
это соответствует уже принятому в репозитории подходу (`scripts/patch-pnm-*.py`). Более дешёвая
(и менее хрупкая к пересборке vendor) альтернатива — ~40 строк внешнего `deeplink-restore.js`
в project-layer: запомнить `/pnm/monitoring/sessions/<id>` в `sessionStorage` до логина,
перехватить `history.pushState/replaceState`, и когда вендор после логина делает
`push({name:"monitoring"})` (`index4.bundle:1@1805`), подменить URL на запомненный и
дёрнуть `PopStateEvent` (vue-router слушает `popstate` — `index.bundle…js:1@521089`).
Плюсы: не трогает vendor, выживает при пересборке, CSP не меняется (внешний `'self'`-скрипт).
Минусы: ещё один файл в project-layer (надо внести в `import-pnm-frontend.sh`, иначе потеряется
при импорте — ровно как `theme-toggle.js` в F-02), и поведение зависит от того, что вендор
ходит именно через `history.pushState` (проверяется тем же Playwright-тестом).
Решение — за Lead'ом; оба варианта требуют одного e2e-теста «чистый контекст → deep link →
логин → та же сессия».

**12.7. Чего в этом дополнении нет.** Сборки не запускались (`cargo test`/`cargo check`
не выполнялись этим субагентом, чтобы не писать в `target/`): цифры «51 passed» и «wasm32
Finished» — из основного корпуса/проверки Lead'а. БД читалась только `mode=ro`; сервис не
перезапускался; `vks_data.db` не изменялась.
