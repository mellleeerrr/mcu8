# План миграции фронтенда PNM (TrueConf MCU UI) в проект VKS

> Статус: **v0.3** — учтены находки финального цикла ревью v0.2 (агент-1 9.5, агент-2 9.3, агент-4 9.2:
> Vary-слой, полнота CSP-спеки, ETag-факт, lockout по BACKEND_RULES §1.4, отзыв токенов/HS256-долг,
> WS-лимиты, durability-заметки); **фаза 1 реализована и зелёная**.
> 2026-10-17: в `deployed/pnm` интегрирован полный ребренд PROSTOR (90 файлов; +2 пост-фикса:
> локали `\u0004MCU…`→`\u0004PROSTOR…`, cache-bust словника) — см. `client/pnm-frontend/NOTES.md` §«Ребренд PROSTOR».
> Дата: 2026-10-16. Автор: основной агент. Привязка: `PROJECT_RULES.md` (§4 ADR, §7 крейты, §13.1 фронтенд,
> §20.2 зависимости), `BACKEND_RULES.md` (§1 аутентификация, §2.4/2.5 лимиты, §6 ошибки/секреты, §7 версии API),
> `FRONTEND_RULES.md` (§10 legacy PNM), `DEVELOPMENT_PLAN.md` (Фаза 8), **ADR-0027**.

**Changelog v0.2 → v0.3:** §4.2 — исправлен факт ETag (его нет у ServeDir), добавлены Vary-слой,
спека security-заголовков/CSP и «5 inline-скриптов»; §4.3 — lockout приведён к BACKEND_RULES §1.4
(двухступенчатый), решён отзыв токенов (смена пароля инвалидирует; HS256→RS256 до prod); §4.4 —
WS-лимиты (§2.4/§2.5) и запрет логирования токенов (§6.3); §4.5 — `ui/sessions`-заглушка в фазе 2;
§4.1 — формула рантайма, размер legacy, судьба zip при git-init; §5 — ссылки/числа исправлены;
§6/§8 — верификация `cargo deny` и критерий «0 неожиданных 501».

**Changelog v0.1 → v0.2:** §4.2 заменён на `tower-http::ServeDir/ServeFile` с проверенной роутер-таблицей
axum 0.8.9 (B3/B4/B5/B6); §4.3 — полный контракт фазы 2 из фактических вызовов UI (B2/B8) + auth-решения
(B7); §4.4 — новая полная спека `/api/v3/ws` (B1); §4.1 — импорт-манифест и статус повреждённого архива;
§4.7 — метрики; §4.8 — `deny.toml`/`.gitignore`; §5 — реестр B1–B9 → решение/тест; §8 — критерии приёмки
с браузер-чеклистом.

---

## 1. Исходные данные (факты, проверено)

### 1.1. Архив `PNM-Frontend-20261016.zip`

- Размер 30 932 992 B, sha256 `ea0314f8…` (якорь), **zip повреждён**: `zipfile`/`unzip` не читают
  central directory (`BadZipFile`), но локальные заголовки на месте — восстановление потоковым
  сканированием `PK\x03\x04` даёт **484/484 записей**.
- Восстановлено в `client/pnm-frontend/deployed/pnm`: **482 файла из 483** файловых записей;
  **один нечитаемый** — `source/extracted_frontend/raw_decompiled/legacy/index-legacy.bundle-2.2.1.66/index-legacy.js`
  (обрыв deflate; дубликат рантайм-чанка); **весь `deployed/pnm` (216/216 записей) восстановлен полностью**
  (в рантайм импортированы 210 файлов — исключены 6 `*.bak*`; 26 MB, `MANIFEST.sha256`).
- Состав `deployed/pnm`: рабочий собранный UI TrueConf MCU Web v2.2.1-66 (Vue 3.5 + vue-router history
  base `/pnm/` + Pinia + axios + chart.js + video.js, Vite dual-билд), `index.html` с `<base href="/pnm/">`
  и абсолютными `/mcu/...`; чанки **не хэшированы** (`…-2.2.1.66.js`, ручные правки in-place: в архиве
  есть `*.bak.dbg2/step3/hotfix/…`); контент-хэши — только у `assets/fonts/*`, `assets/images/*`,
  корневых `faviconCWR_MkEZ.ico`, `*.webm`; service worker отсутствует (PWA-манифест есть);
  `index.html` содержит **5 inline-скриптов** (WS-shim для http→ws; probe `__vite_is_modern_browser`;
  vite-legacy loader; два `nomodule`-скрипта; в modern-браузере исполняются 3) и 1 inline-`<style>`.

### 1.2. Контракт UI (эмпирически, 2026-10-16)

- **Boot (без токена):** `GET /api/v3/system/about`, `GET /api/v3/references?language=ru`,
  `GET /api/v3/login/status`; `POST /api/v3/login {login,password} → {token}` →
  `localStorage "TrueConfMCU_AccessToken"`.
- **Shell (после логина):** `settings/audio_codecs`, `settings/video_codecs`, `profile`, `time-zones`,
  `locales`, `abonents/stop_reasons`, `abonents/count`, `ui/system/status` (поллинг 10 с),
  `license`, `features`, `engine/calibration`, `engine/diagnostics`, `system/time`,
  `settings/calls`, `settings/security`, `waitingroom`, `conferences/status`; `PATCH /api/v3/password`
  (смена пароля при 409/истёкшем пароле — эмулируется).
- **Дашборд (вкладка «Конференции»):** `GET /api/v3/ui/sessions?page&page_size&sort_field&sort_order&has_stopped`
  → `{count, sessions:[…]}`; `POST /api/v3/conferences` (создание).
- **WS `GET /api/v3/ws`** (сразу после логина, конверт JSON, см. §4.4).
- **Exempt-список интерцептора** (без заголовка): `/api/v3/about`, `/api/v3/references`, `/api/v3/login`;
  фактически без токена (до логина) вызываются ещё `system/about` и `login/status`.
- **Интерцептор:** `401` → `logout()` + `push('authorization')` (in-app, base `/pnm/`); на экране логина
  читается `error.description` → JSON-конверт `{"error":{"description","code"}}` обязателен для login.
- **Мапперы** (bundle): `re()` читает snake_case-ответ, отбрасывает отсутствующие поля (минимальные
  ответы безопасны). Ключевые: `PW` (system/about), `AW` (references), `DW` (ui/system/status),
  `RW` (profile), `LW` (time-zones: `{timeZoneId:id}`), `xW` (locales), `Fv` (сессии: `sessionId:"id"`,
  `displayName:"display_name"`…), `QT/JT` (пользователи), `VT` (тело создания конференции — snake_case).
- **Ссылка «Панель управления» → `/admin/`** (брендинг PROSTOR уже в архиве; футер TrueConf MCU © 2026
  АО «ТКС»).

### 1.3. Состояние проекта `mcu`

- `vks-server` (axum 0.8.9): `/api/v1/*`, `/metrics`, `/ws` (protobuf-сигналинг), `/demo` и `/` →
  `demo.rs::DEMO_HTML` (557 строк inline). Статики нет вообще.
- `vks-admin::handlers::router(state)` — v1-роутер (rooms/users/settings/audit/mode/healthz/readyz);
  `auth_middleware.rs` — fail-open при `jwt_issuer=None` и **в проде не смонтирован**; `AppState`:
  `storage`, `jwt_issuer: Option`, `rooms/users/layouts/records/audit/settings/mode`.
- `vks-auth`: `LocalDb` (PBKDF2-SHA256, register/authenticate/user_by_name), `JwtIssuer` (HS256,
  `ttl_secs=900` по умолчанию, `iss`/`jti`, `leeway=0`); `Config` (figment, env `VKS_*`) — без полей
  jwt/frontend.
- `vks-common::metrics` — глобальный реестр + `new_registry()` (7 семейств) + тест соответствия.
- `tower-http 0.6.11` есть в `Cargo.lock` (транзитивно), но НЕ в дереве зависимостей; `fs`-фича
  добавит `mime_guess 2.0.5`, `unicase 2.10.0`, `http-range-header 0.4.2` (лицензии MIT/Apache — ок).
- `deny.toml` невалиден для cargo-deny (`[cves]`, `[skip]`, `[sources.crates-io]` — не существуют);
  git-репозитория нет; путь содержит пробел (сборка через зеркало `/tmp/vksbuild`).

---

## 2. Корень проблемы (уточнён после ревью)

1. Архив — **только собранный minified UI** (исходников `.vue` нет; `source/` — частичная
   реконструкция). Полный реверс — недели, не нужен: рабочий билд уже есть и правится точечно.
2. «Один-в-один как в архиве» и `PROJECT_RULES §13.1` (Dioxus) несовместимы в лоб: Dioxus-порт не
   будет идентичен Vue-билду и стоит месяцы (Фаза 8). Решение — **adopt-and-shim** + ADR с явной
   пометкой миграции (фаза 5).
3. Разрыв контрактов `/api/v3` (TrueConf-модель) ↔ `/api/v1` (наша) — нужен адаптер; **но и разрыва
   «UI один» недостаточно**: UI после логина живёт не только REST-ом: без **JSON-WS `/api/v3/ws`**
   не собирается ни shell (B1), ни создание конференции.
4. Статика: в проекте нет ни точки монтирования, ни SPA-fallback. Главная ловушка — не «отдать файл»,
   а корректная семантика axum 0.8 (`{*path}` ≠ `/pnm/`, авто-redirect удалён) и безопасность
   (decode-once + component-валидация).

Суть решения: **точный рантайм старого UI + тонкий служебный слой** (`/pnm` `/mcu` статика,
`/api/v3` адаптер, `/api/v3/ws`) — и только потом Dioxus-миграция отдельной фазой.

---

## 3. Варианты и оценки (итог раунда 1, не изменился)

| Вариант | Оценка | Вердикт |
|---|---|---|
| **A. Adopt-and-shim** (рантайм 1:1 + адаптер v3 + WS) | **9/10** | **рекомендуется.** «Один-в-один» сегодня, вся цепочка работает, Dioxus — фаза 5 по ADR |
| B. Только статика | 3/10 | UI умирает на первом `/api/v3/login` — «мёртвый груз»; подшаг фазы 1 |
| C. Реверс исходников и сборка | 4/10 | недели ручной реконструкции, тихие ошибки; выгоды нет при живом билде |
| D. Порт на Dioxus сейчас | 6/10 | правильный «в перспективе», но не «1:1» и не сегодня; фаза 5 |
| E. Развивать demo-страницу | 2/10 | тупик, не покрывает требования |

---

## 4. Целевой дизайн (v0.2 — решения зафиксированы)

### 4.1. Импорт и размещение (структура = наша, внутренности = архива 1:1)

```
client/pnm-frontend/
├── archive/PNM-Frontend-20261016.zip      # оригинал 1:1, sha256 ea0314f8… (юридический якорь)
├── deployed/pnm/                          # рантайм, извлечён 1:1 (кроме *.bak*), раздаётся сервером
├── MANIFEST.sha256                        # sha256 каждого файла рантайма (генерирует скрипт)
└── NOTES.md                               # провенанс: sha256 архива, что исключено, что нечитаемо
scripts/import-pnm-frontend.sh             # импорт/переимпорт; режимы --force --with-source --gzip
scripts/pnm-smoke/{serve_pnm.py, observed_endpoints.json, README.md}  # стаб-сервер и контракт (из /tmp)
```

- Скрипт: `VKS_PNM_ARCHIVE=… ./scripts/import-pnm-frontend.sh [--force] [--with-source] [--gzip]`:
  1) проверяет sha256 архива; 2) `unzip -o` или, при повреждении, потоковое восстановление
  (встроенный python-фрагмент, скан local headers); 3) копирует `deployed/pnm` **кроме `*.bak*`**
  (список исключений печатается); 4) `--with-source` — дополнительно `source/extracted_frontend/`
  (не по умолчанию, реконструкция для истории); 5) `--gzip` — опциональная генерация `.gz`
  (предсжатие для `precompressed_gzip()`; по умолчанию ВЫКЛ — в архиве `.gz` нет); 6) пишет
  `MANIFEST.sha256` и печатает сводку (кол-во файлов/байт). Идемпотентен; без `--force` отказывается
  перезаписывать непустой `deployed/`.
- Решение по вопросу «28 MB в репо»: `deployed/` в репо — да (это продукт, 26 MB); `source/` — нет
  (архив хранится, распаковка по требованию); `.gitignore` — см. §4.8.
- **Формула состава рантайма:** замыкание ссылок `index.html` (+ `LICENSE.txt`) — при переимпорте
  сверять; legacy-сборка (`chunks/*-legacy.bundle-*`, **32 файла / 6 289 441 B**, ~22 % рантайма)
  входит в замыкание (три `nomodule`-ссылки) — сохранена ради 1:1 осознанно (ADR-0027).
- **Zip при git-init:** репозитория пока нет; до первого коммита решить судьбу `archive/*.zip`
  (~31 MB): git-lfs или внешнее хранилище (в историю «как есть» не коммитить) — запись в `NOTES.md`.

### 4.2. Статика: `tower-http 0.6.11` (`fs`), модуль `crates/vks-server/src/frontend.rs`

**Почему tower-http (ADR-0027 + запись в `deny.toml`, §20.2):** исходник 0.6.11 проверен построчно —
`ServeDir` реализует ровно нужную защиту (одна percent-декодировка, покомпонентная валидация
`RootDir|ParentDir|Prefix → reject`, NUL → 404), Range (206/416), HEAD, `If-Modified-Since → 304`,
`Last-Modified`; своего `ETag` и `Vary` у `ServeDir` **нет** (проверено: 0 вхождений по исходнику) —
`Vary: Accept-Encoding` добавляет наш слой (`frontend.rs::with_cache_control`), `ETag` не нужен
(ревалидация по `Last-Modified`); свой 200-строчный сервер повторил бы всё это с риском ошибок.
Лицензии зависимостей `fs`-фичи разрешены.

**Роутер-таблица (проверено на axum 0.8.9 + matchit 0.8.4; `{*rest}`, без `/*`):**

| Метод+путь | Ответ |
|---|---|
| `GET /` | `307 → /pnm/` |
| `GET /pnm` | `308 → /pnm/` (+query) |
| `GET /pnm/` | `index.html` (200) |
| `GET /pnm/{*rest}` | файл из `deployed/pnm/`; нет файла → `index.html` (SPA-fallback, 200) |
| `GET /mcu/{*rest}` | файл из `deployed/pnm/`; нет файла → **404** (НЕ SPA-fallback) |
| `GET /mcu`, `GET /mcu/` | **404** (`append_index_html_on_directories(false)`) |
| `GET /admin`, `GET /admin/` | `307 → /pnm/administration` (временно; фаза 8 → Dioxus-админка) |
| `POST /pnm/*` | 405 (не fallback; фиксируется тестом) |
| `GET /static/*` | 404 сейчас; фаза 4 (файлы сервера: avatars/media/…) |

**Монтаж (единственно рабочий рецепт axum 0.8, подтверждён по исходникам):**

```rust
Router::new()
    .route("/pnm", get(redirect_to_pnm_slash))                       // nest_service("/pnm/") НЕ регистрирует "/pnm"
    .nest_service("/pnm/", pnm_dir)   // ServeDir(root).append_index_html_on_directories(false)
                                      //   .precompressed_gzip()
                                      //   .fallback(ServeFile(root/index.html).precompressed_gzip())
    .nest_service("/mcu", mcu_dir)    // ServeDir(root).append_index_html_on_directories(false).precompressed_gzip()
    .route("/admin", get(redirect_admin)).route("/admin/", get(redirect_admin))
```

- Факты-ловушки (закрыты): `/pnm/{*rest}` **не матчит** `/pnm/` → явный `/pnm/` регистрирует сам
  `nest_service("/pnm/")`; `.route("/pnm", …)` + `nest_service("/pnm", …)` конфликтуют → монтируем
  на `/pnm/`; `not_found_service` даёт 404 с телом index → используем `.fallback(ServeFile)`; монтирование
  только `nest*` — без ручных `{*rest}`-хендлеров.
- Декодирование/обход: одна декодировка внутри `ServeDir`; и `/pnm/%2f…`, `/pnm/%2e%2e`, `..%2F`,
  `..%5C`, `%00`, overlong-UTF8 — вне корня никогда (доказано исходником + тест-канорейкой). Под
  `/pnm` такие пути дают SPA-fallback (200 index), под `/mcu` — 404; это фиксируется тестами.
- **Кэш-политика** (слой `Cache-Control`, по пути после strip; `immutable` — только реально
  хэшированные): `assets/fonts/**`, `assets/images/**`, `assets/*.webm` → `public, max-age=31536000,
  immutable`; **всё остальное, включая `chunks/**` и index** → `no-cache` (чанки правились in-place!).
  Тот же слой ставит **`Vary: Accept-Encoding`** (иначе reverse-proxy/CDN может отдать gzip-представление
  клиенту без gzip) и требует границу сегмента префикса (`/pnmx` — не статика). Процедура деплоя правок:
  любой ручной патч бандла сопровождается cache-bust (`?v=<build>` в `index.html`) — запись в NOTES.md.
  Тесты на заголовки: `chunks/index.bundle-…js` (no-cache+Vary), `assets/fonts/DxNvdLoR.ttf`
  (immutable+Vary), `/pnm/` (no-cache+Vary), `/pnmx` (нет заголовков статики).
- **Каталог рантайма:** env `VKS_FRONTEND_DIR` (в `Config`), дефолт `client/pnm-frontend/deployed/pnm`;
  канонизируется один раз при старте; при отсутствии — `error!` в лог (диагностика; сервер не падает)
  и статика отвечает 404; метрика `reason="not_configured"`.
- **Security-заголовки и CSP** (спека; слой `with_security_headers`, app-wide): `Content-Security-Policy`
  — `default-src 'self'; script-src 'self'` + sha256 **всех** inline-`<script>` `index.html`
  (в сборке их **5**; атрибут `src` (не `data-src`!) исключает тег из хэширования; вычисление — при
  старте из файла, правки index.html не ломают политику); `style-src 'self' 'unsafe-inline'`;
  `img-src 'self' data: blob:`; `font-src 'self' data:`; `media-src 'self' blob:` (video.js/записи);
  `connect-src 'self'` (same-origin `ws://`/`wss://` покрыт); `object-src 'none'`; `base-uri 'self'`;
  `frame-ancestors 'self'`; `manifest-src 'self'`; `form-action 'self'`. Плюс `X-Content-Type-Options:
  nosniff`, `X-Frame-Options: SAMEORIGIN`, `Referrer-Policy: same-origin`. HSTS — только prod-TLS.
- `precompressed_gzip()` включён (если `--gzip` не гоняли — просто не найдёт `.gz`); `.gz` не в архиве,
  генерация — опция скрипта импорта (не меняет 1:1 по умолчанию).
- Range/HEAD/304 — бесплатно от `ServeDir` (**ETag отсутствует**, ревалидация по `Last-Modified`;
  тесты: `Range: bytes=0-9` → 206 для `.webm`, `HEAD /pnm/` → 200 без тела, `If-Modified-Since` → 304).

### 4.3. Аутентификация (B7/B8)

- `Config`: `jwt_secret` (`VKS_JWT_SECRET`, fail-fast при пустом в prod-режиме — сейчас: WARN + v3
  fail-closed), `jwt_ttl_secs` (`VKS_JWT_TTL_SECS`, дефолт **86400** — обоснование: legacy UI не умеет
  refresh; отклонение от 900 c зафиксировано в ADR-0027), `frontend_dir`.
- Composition root (`main.rs`): `JwtIssuer` из конфига; bootstrap оператора в `LocalDb`
  (`VKS_ADMIN_LOGIN`/`VKS_ADMIN_PASSWORD`, дефолт `admin/admin` ТОЛЬКО для dev; в лог — предупреждение
  сменить); issuer кладётся в `AppState`.
- v3-middleware (`api_v3`): **fail-closed** — `jwt_issuer=None` → `503 {"error":{…}}` (включая login);
  нет/битый токен → `401 {"error":{"description":"Unauthorized","code":401}}`.
- **Exempt-list**: `POST /api/v3/login`, `GET /api/v3/login/status`, `GET /api/v3/system/about`,
  `GET /api/v3/about`, `GET /api/v3/references` — без токена; всё прочее — с токеном.
- Login: проверка `LocalDb::authenticate`; ошибка → `401` + `{"error":{"description":"Login rejected",…}}`
  (UI показывает description); успех → `{token}`.
- **Lockout + аудит** (BACKEND_RULES §1.4, двухступенчатый): **5 неудач / 15 мин per user+IP** → блок
  15 мин; **10 неудач / 5 мин per IP** → блок 15 мин; `401` «Too many attempts…» (+`Retry-After`),
  счётчики in-memory (Mutex<HashMap>), запись в AuditLog (login_ok/login_fail/locked).
  Метрика `vks_login_attempts_total{result}`.
- **IP за reverse-proxy** (долг, S3 ревью-цикла 2): `ConnectInfo` в проде всегда есть
  (`into_make_service_with_connect_info`), поэтому lockout/WS-rate-limit считаются по IP прокси;
  `X-Forwarded-For`/`X-Real-IP` — фолбэк только при встраивании роутера без connect-info (unit).
  Доверие к XFF осознанно выключено (подделываемый заголовок = обход lockout); конфигурируемое
  доверие — вместе с nginx-развёртыванием (фаза 4+).
- **Отзыв токенов** (решение): refresh у legacy UI нет; logout — клиентский (токен живёт до TTL);
  **смена пароля инвалидирует ранее выданные токены** (проверка `iat` против метки смены пароля
  пользователя в `LocalDb`); долг до production — `jti`-denylist при необходимости.
- **HS256 → RS256/PS256 + `kid`** — обязательный долг до production (BACKEND_RULES §1.1); сейчас HS256
  (dev/single-node). Токены и пароли **никогда не логируются** (§6.3); debug-дампы payload WS — маскировать.
- `PATCH /api/v3/password` `{username, old_password, new_password}` → verify old → update `LocalDb` →
  **инвалидация старых токенов** → новый `{token}` (UI ожидает token). Сценарий «409 password expired» —
  **EMULATED**: не инициируется нашим сервером (нет флага expiry), но путь смены пароля реализован.
- Middleware монтируется **только** на `/api/v3/*`; `/api/v1/*` не трогаем (вне скоупа этой задачи).

### 4.4. WS `GET /api/v3/ws` (B1) — JSON-протокол (спека)

Сокет открывается **без токена** (токен — в каждом сообщении), реконнект клиента 1s × 1.5ⁿ.

| Направление | Формат |
|---|---|
| C→S транзакция | `{"method": "<m>", "payload": {…}, "id": "<uuid>", "access_token": "<jwt>"}` |
| C→S подписка/отписка | это тоже транзакции: `method:"subscribe"/"unsubscribe"`, `payload:{"events":{"<event>": {фильтры}}}` |
| S→C ответ | `{"id": "<uuid>", "payload": {…}}` **или** `{"id": "<uuid>", "error": {"code": <int>, "description": "<s>"}}` |
| S→C событие | `{"event": "<name>", "payload": {…}}` (без `id`) |

- **Проверка токена на каждом сообщении** (verify JWT). Невалид → `error.code=401` → UI эмитит
  `logout`. Технические ошибки (неизвестный метод, битый payload) → **код ≠ 401**, соединение НЕ
  рвётся (иначе клиент уйдёт в бесконечный реконнект); вне лимитов — счётчик.
- **Лимиты** (BACKEND_RULES §2.4/§2.5): кадр ≤ 16 KB (больше — error без разрыва/RFC-close);
  keepalive ping 30 s; rate-limit новых соединений ≤ 100/s (per-IP, отказ — `error.code=429`);
  `access_token`/payload в логи не пишутся (§6.3).
- Методы: `subscribe` → `{id,payload:{}}` + немедленное событие подписок; `unsubscribe` → `{id,payload:{}}`;
  `ui:users:search` → `{users:[…], count}` (из `state.users`, маппер `QT`: `id`, `display_name`, …);
  `ui:call_strings:search` → `{call_strings:[], count:0}`; `ui:files:search` → `{files:[], count:0}`;
  `terminals:configurations:install` / `terminals:firmwares:install` → error `{code:501}`; прочее —
  `{code:404,"Unknown method"}`.
- Событие `system:status:disk` → `{"full":"NORMAL"}` (отправляется сразу после `subscribe`; далее — при
  изменениях). Дисковые пороги — из `ui/system/status`-источника.
- Тесты (`tokio-tungstenite`, vks-server/tests): подключение → subscribe → ответ+событие; неизвестный
  метод → error без разрыва; битый токен → 401-error; рестарт-совместимость (закрытие чистое).

### 4.5. Адаптер `/api/v3/*` (`crates/vks-admin/src/api_v3/`)

Файлы: `mod.rs` (router + error-конверт), `auth.rs` (middleware+login+lockout), `boot.rs` (about/references/
login/status), `shell.rs` (profile/…/settings/…/license/features/…), `sessions.rs` (ui/sessions, conferences,
ui/conferences), `ws.rs` (спека §4.4), `tests.rs` (tower::oneshot + фикстуры).

**Фаза 2 — «login + shell без ошибок в консоли» (полный список, из фактических вызовов UI):**

| Endpoint | Ответ (источник) |
|---|---|
| `POST /api/v3/login` | `{token}` / 401 `{"error":{…}}` (§4.3) |
| `PATCH /api/v3/password` | `{token}` (§4.3) |
| `GET /api/v3/login/status` | `{status:"enabled"}` |
| `GET /api/v3/system/about` | `{node_id, server_name, versions:{server}, state:{status:"READY", payload:{package:"",backup:"",export:""}}}` ← маппер `PW`; **без этого — полноэкранный оверлей** |
| `GET /api/v3/about` | (совместимость с exempt-списком интерцептора) тот же/минимальный |
| `GET /api/v3/references` | `{key,index,buy,contacts,trial_full_version,license_renewal,free_license_restrictions}` (пустые строки) |
| `GET /api/v3/profile` | `{user:{id,display_name,permissions:["all"] (для Operator; не-Operator — провизорные ключи, TODO(T3)),locale_id,time_zone_id,groups:[],emails:[],phones:[],avatars:[],description,creator_id,created_at,busy,defaults:{deletable:false,editable:true}}}` |
| `GET /api/v3/time-zones` | `{time_zones:[{id:"UTC"}]}` |
| `GET /api/v3/locales` | `{locales:[{id:"ru_RU",description:"Русский"},{id:"en_US",…}]}` |
| `GET /api/v3/abonents/stop_reasons` | `{}` (raw) |
| `GET /api/v3/abonents/count` | `{count: N}` (наши users) |
| `GET /api/v3/ui/system/status` | `{engine:{…},database:{…},network:{…},disk:{free,percent,full:"NORMAL"},sip:{…},h323:{…}}` ← маппер `DW`; навигация-гард деструктурирует `status` |
| `GET /api/v3/license` | `{state:"VALID",type:"PERMANENT",count:100,…}` |
| `GET /api/v3/features` | `{features:{}}` |
| `GET /api/v3/engine/calibration` / `engine/diagnostics` | `{}` (маппер мягкий) |
| `GET /api/v3/system/time` | `{time,timezone,ntp,ntp_name,ntp_synchronized}` |
| `GET /api/v3/settings/calls` | из наших `Settings` (codecs/limits) |
| `GET /api/v3/settings/security` | `{payload:{password_min_*(числа), password_no_login_match:false, password_ttl:0, password_ttl_enabled:false, login_max_attempts_count:5, login_block_timeout:900, login_block_enabled:true, conf_*:0/false}}` ← маппер `Ane` читает **`data.payload`** (без него boot-цепочка падает на `settings.password`); lockout-значения — константы `auth` (не дублировать). `PATCH` — долг |
| `GET /api/v3/users/credentials/expire` | `{users:[]}` ← маппер `MW` деструктурирует `users` (без него store re-throw → консоль); параметры UI `expire_in/sort_order/sort_field` игнорируются (TTL паролей не настроен) |
| `GET /api/v3/settings/audio_codecs` / `video_codecs` | `{audio_codecs:[…]}` / `{video_codecs:[…]}` (из `vks-core` codecs, иначе `[]`) |
| `GET /api/v3/waitingroom` | `{}` (дополнить формой из бандла при реализации) |
| `GET /api/v3/conferences/status` | `{status:"enabled"}` |
| `GET /api/v3/ui/sessions` | **заглушка** `{sessions:[],count:0}` — дашборд зовёт сразу после логина (иначе ошибка консоли); полноценно — фаза 3 (§N3 ревью) |
| `WS /api/v3/ws` | §4.4 |

**Фаза 3 — данные (наши комнаты):**

| Endpoint | Маппинг |
|---|---|
| `GET /api/v3/ui/sessions` | `RoomRecord` → сессии: `{sessions:[{id, display_name:name, …}], count}` (маппер `Fv`: `sessionId←id`, `displayName←display_name`); параметры `page/page_size/search` (подстрока имени), `sort_field` `display_name`(default)/`id`/`session_id`/`start_at`/`started_at`/`created_at` (по `created_ms`) + `sort_order` (0 asc / ≠0 desc); `has_stopped=true` и `state=inactive` → **пусто** (stopped/inactive комнат у нас нет — иначе дашборд показывает активную комнату в «Неактивных»), `state=active`/None → все; `exclude_tags/creators/…` игнорируются. **Контракт карточек** (из бандла, иначе TypeError деструктуризации): `abonents.{sip,h323,rtsp,vnc,media,file}` (числа, карточка сессии), `contacts.{users,sip,h323,rtsp,vnc,media,file}` (числа, карточка конференции), `payload.connections_limit` (мапперы `connectionsLimit←payload.connections_limit`, из `Settings.limits`), `started_at ← created_ms/1000` (unix-секунды; карточки умножают на 1e3, иначе «1970»/«58760»); `videorecord/broadcast:false` → `isRecording/isBroadcasting` |
| `GET /api/v3/ui/sessions/{id}` | одна комната → `{session:{…}}` (404 JSON если нет) |
| `GET /api/v3/sessions` | тот же список (не-ui форма) |
| `GET /api/v3/conferences` / `ui/conferences` | `{conferences:[…], count}` из комнат (мапперы `dy/zZ`); параметры/фильтры как у `ui/sessions` (`state=inactive` → пусто, `exclude_tags` игнорируется) |
| `POST /api/v3/conferences` | тело `VT` (snake_case: `display_name`, `pin`, `payload{…}`) → создание комнаты через тот же внутренний путь, что `POST /api/v1/rooms` (без дублирования логики); ответ `{conference:{id, display_name, …}}` |
| `GET/DELETE /api/v3/conferences/{id}` | чтение/удаление комнаты (аудит) |

**Catch-all:** `/api/v3/{*rest}` (любой метод) → `501 {"error":{"description":"Not implemented","code":501}}`
+ `tracing::warn!` + `vks_api_v3_requests_total{endpoint="unmatched"}`. Падение в 404 запрещено
(UI считает 404 «метод не существует»). `{*rest}` **не матчит пустой rest**: `/api/v3/` регистрируется
явно тем же 501-хендлером (единообразие конверта). Полная трассировка 104 REST-путей + 10 WS-методов —
приложение A (фазы 4+).

### 4.6. Удаление demo (фаза 1)

`demo.rs` → снапшот в `docs/progress/archive/demo-2026-10-16.rs`; удаление: `lib.rs:3`,
`main.rs:31,95-96`, `tests/network_integration.rs:113-114` + замена теста `test_live_demo_web_page`
(316–350) на composition-level тест `build_app` (главная страница = UI). **Вынос
`build_app(state, frontend) -> Router` в `lib.rs`** — единая точка сборки для main и тестов.

### 4.7. Наблюдаемость (новые метрики; в `new_registry()` + тест)

- `vks_http_requests_total{method, class, status}` (class: `static|spa|api_v1|api_v3|ws|metrics|other`;
  `method` — whitelist `GET|HEAD|POST|PUT|PATCH|DELETE|OPTIONS`, остальное → `other`)
- `vks_http_request_duration_seconds{class}` (histogram), `vks_http_response_bytes_total{class}`
- `vks_api_v3_requests_total{endpoint}` (`endpoint` = имя роута или `unmatched`)
- `vks_login_attempts_total{result}` (`ok|fail|locked`)
- `vks_frontend_denied_total{reason}` (`not_found|not_configured`; traversal неотличим от 404 —
  `ServeDir` отказывает мягко, отдельного reason нет — учитывается как `not_found`)
- Обновить `new_registry_exposes_all_metrics`.

### 4.8. Гигиена

- `deny.toml` → валидная схема cargo-deny: `[licenses]` (без изменений), `[advisories]` (ignore-список с
  обоснованиями), `[sources]` (`allow-registry`), `[graph]`; убрать `[cves]/[skip]/[sources.crates-io]`;
  добавить запись tower-http (MIT) — §20.2.
- `.gitignore` (на будущее, git пока нет): `target/`, `*.gz` в `client/pnm-frontend/deployed/`
  (регенерация скриптом), `client/pnm-frontend/source/` (opt-in), `*.bak*`, `.env`.
- Артефакты `/tmp` → репо: `scripts/pnm-smoke/` (§4.1); фикстуры контракта — тесты `api_v3/tests.rs`.

---

## 5. Реестр решений по отчёту баг-хантера №3 (B1–B9)

| # | Что было | Решение в v0.2 | Доказательство/тест |
|---|---|---|---|
| B1 | Нет спеки `/api/v3/ws` | §4.4: полная спека (конверты, методы, коды, reconnect-инвариант) | тесты tokio-tungstenite |
| B2 | Состав фазы 2 не совпадал с реальностью | §4.5 полный список по фактическим вызовам (вкл. `system/about`, `profile`, `ui/system/status`, `stop_reasons`, `time-zones`, codecs, `PATCH password`) | oneshot-тест каждого + браузер-чеклист |
| B3 | Роутинг axum 0.8 (`{*path}`, авто-redirect) | §4.2 точный рецепт (`/pnm` явный, `nest_service("/pnm/")`, 308/307 вручную) | тесты: `/pnm`→308, `/pnm/`→200 |
| B4 | Traversal `join()` с абсолютным путём | §4.2: `ServeDir` decode-once + component-валидация; каноразметка root при старте | тест-корпус traversal + канорейка |
| B5 | `immutable` на не-хэшированные файлы | §4.2 маска кэша; `chunks/**` → no-cache; процедура cache-bust | тесты заголовков |
| B6 | `/mcu/*` без fallback-семантики | §4.2: только `/pnm/**` SPA-fallback; `/mcu/**` 404; fallback-ответы no-cache | тесты `/mcu/nope.js`→404, deep-link→200 |
| B7 | JWT fail-open, TTL 900, нет ролей | §4.3: fail-closed 503, `VKS_JWT_TTL_SECS=86400` (ADR), bootstrap admin, lockout+аудит | тесты middleware/login |
| B8 | Exempt-list + JSON-конверт | §4.3/§4.5: exempt = login|login/status|system/about|about|references; login-fail → 401 JSON `error.description` | oneshot-тесты |
| B9 | `/admin/` — мёртвая ссылка | §4.2: `307 → /pnm/administration` (временный) | тест |
| — | MIME (woff/txt), Range, HEAD, ETag/304, security-заголовки, CSP | `ServeDir`+фиксы: mime_guess покрывает; Range/HEAD/304 бесплатно; **ETag отсутствует** (ревалидация по `Last-Modified`); `Vary: Accept-Encoding` — наш слой; §4.2: полная спека `X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`, **CSP со sha256 всех 5 inline-скриптов** (вычисляется при старте из index.html; `script-src 'self'` без unsafe-inline), `style-src 'unsafe-inline'`; HSTS — прод-TLS-чеклист | тесты заголовков; CSP-хэши сверяются с файлом |
| — | Ссылка на правила (FRONTEND_RULES §13) | исправлено: `PROJECT_RULES §13.1` + ADR-0010 |

---

## 6. Верификация

1. `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace` — зелёные
   (без `--all-features`: `vendored-protoc` ломается на пробеле в пути).
2. Юнит-тесты `frontend.rs`: роутер-таблица, traversal-корпус (`../`, `%2e%2e`, `%252e`, `%2f`, `%5c`,
   `%00`, `%FF`, `%c0%ae`, симлинк наружу), MIME, кэш-заголовки (+Vary), SPA-fallback, 404 `/mcu`, redirects, HEAD/Range/304, CSP.
3. `api_v3`: oneshot-тесты каждой формы ответа (фикстуры), e2e login→shell→ui/sessions (в памяти),
   catch-all 501 (вкл. `/api/v3/`), lockout (двухступенчатый), WS-тесты (tungstenite) + WS-лимиты.
4. **Браузер-чеклист (живая проверка):** `/pnm/` → логин без оверлеев и ошибок консоли → shell →
   «Конференции» показывает наши комнаты (создать через v1) → клик «Панель управления» (/admin/) →
   307 → administration; deep-link `/pnm/administration/security` → 200; отсутствующий чанк
   `/mcu/chunks/nope.js` → 404. Скриншоты в `docs/progress/evidence/`. Критерий: **0 неожиданных 501**
   в рамках чеклиста (вне реализуемых экранов 501 — ожидаем и помечен метрикой `endpoint="unmatched"`).
5. Регрессия: `curl` по `/api/v1/*` (rooms/users/settings/audit), `/ws`, `/metrics` — без изменений.
6. `cargo deny check` (гейт PROJECT_RULES §5.3): локально — бинарник пинованной версии
   (`cargo install cargo-deny --locked` или prebuilt-релиз), вывод зафиксировать в PROGRESS.md;
   в CI — `EmbarkStudios/cargo-deny-action` (latest). Если локальная установка недоступна — запись
   «вопрос к команде» с указанием причины.

## 7. Риски

| # | Риск | Митигация |
|---|---|---|
| R1 | Формы ответов не совпадут → белый экран | фикстуры из бандла до реализации; браузер-проверка на фазе; мапперы мягкие (проверено) |
| R2 | Лицензия TrueConf-билда (+ «PROSTOR») | решение пользователя/команды до релиза; отметка в ADR-0027; техника готова |
| R3 | Размер репо (~26 MB рантайм + ~30 MB архив) | только `deployed/`; `source/` opt-in; `.gitignore` |
| R4 | Отклонение от Dioxus (§13.1) | ADR-0027 + фаза 5; FRONTEND_RULES-пометка |
| R5 | Дублирование v1/v3 | v3 — тонкий адаптер; вся логика в домене; тесты приёмки на оба API |
| R6 | Пробел в пути → сборка | штатно: реальный каталог собирается (системный protoc); зеркало `/tmp/vksbuild` нужно только при `--all-features` (vendored protoc) |
| R7 | Ручные правки чанков и кэш | §4.2 cache-bust процедура + тесты заголовков |

## 8. Критерии приёмки итерации

- [x] `client/pnm-frontend/` (архив+рантайм 1:1 без `.bak*`+MANIFEST+NOTES) и `scripts/import-pnm-frontend.sh`;
      `scripts/pnm-smoke/` (стаб-сервер + observed-контракт) перенесены из `/tmp`.
- [x] `frontend.rs`: вся роутер-таблица §4.2 + traversal/MIME/кэш(+Vary)/CSP/Range-тесты зелёные; `/` и `/admin(/`→редиректы.
- [x] `api_v3` фаза 2 (весь список §4.5) + WS §4.4 + catch-all 501 (вкл. `/api/v3/`); фикстуры; тесты зелёные
      (73 unit + WS e2e; composition-тест `test_composition_api_v3_login_chain`). Ревью-фиксы B1/B2/D2–D4 закрыты.
- [x] Фаза 3 (код + тесты): `ui/sessions` = наши комнаты; `POST/DELETE conferences` (создание/удаление из UI
      видно в `/api/v1/rooms` — общее хранилище `state.rooms`). Браузер-скриншот с непустым списком — в чек-листе ниже.
- [x] Auth: fail-closed, exempt-list, lockout по §1.4 (двухступенчатый)+аудит, отзыв при смене пароля, TTL-решение в ADR.
- [x] demo удалён; `build_app` в lib; сетевые тесты обновлены (composition-level).
- [x] Метрики §4.7 в `new_registry()` (тест обновлён);
- [x] `cargo deny check` (0.20.2, prebuilt) прогнан на валидном `deny.toml` (см. §6.6) — 🟢
      advisories/bans/licenses/sources ok, вывод в PROGRESS; `.gitignore` создан [x].
- [x] `cargo fmt/clippy/test` (workspace); регрессия v1/ws/metrics; ADR-0027 + PROGRESS.md;
- [x] Браузер-чеклист (§6.4) + скриншоты в `docs/progress/evidence/` (0 ошибок консоли; фиксы
      консольных контрактов карточек/`settings/security`/`credentials/expire` — PROGRESS итер. 4.3).

## 9. Фазы

| Фаза | Содержание | Критерий |
|---|---|---|
| 1 | импорт + статика + удаление demo + гигиена | экран логина открывается, статика и редиректы корректны, v1 не сломан |
| 2 | auth-цепочка + весь boot/shell + WS + catch-all | логин из UI, shell без ошибок консоли |
| 3 | sessions/conferences (наши комнаты) | список комнат в UI; создание/удаление из UI |
| 4 | периферия (history/logs/engine/monitoring/static-files/…) | по таблице приложения A |
| 5 | (стратегическая) Dioxus-миграция | отдельный цикл |

---

## Приложение A. Трассировка API v3 (104 REST + 10 WS)

- **Ф2 (закрыто этим планом, §4.5):** login, password, login/status, system/about, about, references,
  profile, time-zones, locales, abonents/stop_reasons, abonents/count, users/credentials/expire, ui/system/status,
  license, features, engine/calibration, engine/diagnostics, system/time, settings/calls, settings/security,
  settings/audio_codecs, settings/video_codecs, waitingroom, conferences/status + **ws**.
- **Ф3 (закрыто):** ui/sessions, ui/sessions/{id}, sessions, conferences (GET/POST), conferences/{id}
  (GET/DELETE), ui/conferences.
- **Ф4 (501-заглушка сейчас; реализация по экранам):** abonents/*, addressbook-группы (groups, roles,
  users), audiochannels, call_history (+clean/export), call_strings, c/*, credentials/password, engine/*
  (abonents/command/log), files, log_events(+export), logs, registration/*, schedules, sessions/* (quick,
  presentation, invite, contacts, aspeaker, crown, videorecords, audiochannels…), settings/* (h323, sip,
  http, rtp, smtp, styles, video), smtp/*, system/* (export/import/network/ntp/packages/power/stats/timezone/
  utils), tags_to_objects, terminals/*, ui/* (остальные), users/*, video_layouts/*, video_layout_grids,
  videorecords, static-файлы.
- **WS-методы (10 найдено в бандле):** ui:users:search, ui:call_strings:search, ui:files:search,
  terminals:configurations:install, terminals:firmwares:install + subscribe/unsubscribe + события
  system:status:disk и системные (движок/БД/сеть/диск/SIP/H323) — таблица по мере реализации.
