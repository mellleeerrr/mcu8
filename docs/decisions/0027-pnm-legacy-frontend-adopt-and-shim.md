# ADR-0027: Фронтенд PNM (legacy TrueConf MCU UI) — adopt-and-shim с поэтапным переходом на Dioxus

- **Статус:** accepted
- **Дата:** 2026-10-16
- **Решение принято:** пользователь (запрос на перенос архива 1:1) + анализ команды (FRONTEND_PORT_PLAN.md)
- **Связанные ADR:** 0010 (Dioxus), 0013 (progenitor), 0009 (JWT), 0019 (аудит), 0022 (RBAC), 0026 (MCU/SFU)
- **Связанные требования:** задача пользователя 2026-10-16 (перенос `PNM-Frontend-20261016.zip` 1:1); R6 (конфигурируемость сервера)

## Контекст (Context)

- Встроенная demo-страница (`vks-server/src/demo.rs`, ADR-0003/0025) — прямой WebRTC-звонок без админ-UI; проектным фронтендом заявлен Dioxus (ADR-0010, `FRONTEND_RULES §13.1`).
- Пользователь передал архив `PNM-Frontend-20261016.zip` (sha256 `ea0314f8…`, ~31 MB) — собранный UI TrueConf MCU 2.2.1.66 (Vue 3 SPA + legacy-сборка, брендинг «PROSTOR», 216 файлов рантайма, ~26 MB) — и требует перенести содержимое **1:1** в проект, дописав серверную цепочку до рабочего логина/шела.
- UI вызывает `/api/v3/*` (TrueConf-совместимый REST, 104 эндпоинта по трассе) и WS `/api/v3/ws` (JSON-конверты), логин — `POST /api/v3/login` → `{token}`.
- Архив частично повреждён (нет central directory) — восстановлен скриптом по local headers: 482/483 файлов; весь `deployed/pnm` (216/216) читается; нечитаем один неиспользуемый дубликат `source/…/index-legacy.js`.
- Зависимости: нужен раздатчик статики с Range/HEAD/304/SPA-fallback; свой 200-строчный веб-сервер повторял бы защиту от traversal с риском ошибок.

## Варианты (Options)

### Вариант A: adopt-and-shim (рантайм legacy UI + адаптер `/api/v3` к нашему домену)
- Плюсы: UI работает 1:1 сразу; вся логика — в нашем домене (комнаты/пользователи/аудит); адаптер тонкий; Dioxus-миграция остаётся стратегическим путём.
- Минусы: временно два API (v1/v3) и legacy-ассеты в репо; CSP/TTL-компромиссы под старый клиент.
- Риски: лицензия TrueConf-сборки (R2), формы ответов (митигируются фикстурами из бандла).
- **Оценка раунда 1: 9/10 (рекомендован).**

### Вариант B: переписать UI на Dioxus «1:1»
- Минусы: недели работы до первого рабочего экрана; невозможно гарантировать 1:1; блокирует пользователя.
- **Оценка: 3/10.**

### Вариант C: пересобрать UI из декомпилированного `source/`
- Минусы: исходники неполные/декомпилированные, сборка не воспроизводима.
- **Оценка: 4/10.**

### Вариант D: свой минимальный UI (Dioxus/HTML) вместо 1:1
- Плюсы: чисто.
- Минусы: не решает требование «1:1 как в архиве».
- **Оценка: 6/10.**

### Вариант E: отказаться от переноса (оставить demo)
- **Оценка: 2/10.**

## Решение (Decision)

Принят **вариант A (adopt-and-shim)**. Конкретно:

1. **Размещение** (структура — наша, внутренности — архива 1:1):
   - `client/pnm-frontend/archive/PNM-Frontend-20261016.zip` — оригинал (юридический якорь, не распаковывается);
   - `client/pnm-frontend/deployed/pnm/` — рантайм (извлечён 1:1, исключены только `*.bak*`; `MANIFEST.sha256` — 210 файлов); формула: **замыкание ссылок `index.html` (+ LICENSE.txt)**;
   - `scripts/import-pnm-frontend.sh` — идемпотентный импорт/переимпорт (sha256-якорь, recovery, `--with-source`, `--gzip`).
2. **Раздача статики**: крейт `vks-server`, модуль `frontend.rs` на `tower_http::ServeDir`/`ServeFile` 0.6.11 (ADR + запись в `deny.toml`). Роуты: `/` 307→`/pnm/`; `/pnm` 308→`/pnm/` (+query); `/pnm/…` — файл, иначе SPA-fallback `index.html`; `/mcu/…` — файл, иначе 404; `/admin(/` 307→`/pnm/administration` (до Dioxus-админки, фаза 8).
3. **Кэш**: `immutable` — только реально хэшированные (`assets/fonts/**`, `assets/images/**`, `assets/*<hash>.webm`); всё остальное (в т.ч. `chunks/**`, `index.html`) — `no-cache`; `Vary: Accept-Encoding` (precompressed gzip); процедура cache-bust при ручных правках — `NOTES.md`.
4. **CSP и заголовки**: `script-src 'self'` + sha256 **всех** inline-скриптов `index.html` (в сборке их 5; вычисляются при старте — правки index.html не ломают политику), `style-src 'self' 'unsafe-inline'`, `media-src 'self' blob:`, `img-src 'self' data: blob:`, `connect-src 'self'`; app-wide `X-Content-Type-Options: nosniff`, `X-Frame-Options: SAMEORIGIN`, `Referrer-Policy: same-origin`; HSTS — только prod-TLS.
5. **Адаптер `/api/v3`** (`crates/vks-admin/src/api_v3/`): тонкий маппер на домен (комнаты = `ui/sessions`/`conferences`); catch-all → `501` JSON + `tracing::warn!` + метрика (не 404: 404 UI трактует как «метода нет»); WS `/api/v3/ws` — токен в каждом сообщении, `error.code=401` → logout клиента, технические ошибки не рвут соединение; лимиты: кадр ≤16 KB, ping 30 s, rate-limit реконнектов 100/s (BACKEND_RULES §2.4/§2.5).
6. **Аутентификация**: `/api/v3` middleware **fail-closed** (`jwt_issuer=None` → 503, включая login); `POST login`/`GET login/status`/`system/about`/`about`/`references` — exempt; TTL **86400 c** (отклонение от BACKEND_RULES §1.1 «900»: legacy UI не умеет refresh — зафиксировано здесь); lockout — по BACKEND_RULES §1.4 (5 неудач/15 мин per user+IP; 10/5 мин per IP; блок 15 мин) + аудит (ADR-0019) + метрика; токены/пароли **не логируются** (§6.3).
7. **Отзыв токенов** (решение): logout — клиентский (токен живёт до TTL); **смена пароля инвалидирует старые токены** (проверка `iat`/epoch в middleware); refresh не поддерживается (legacy UI). Долг: **HS256 → RS256/PS256 + `kid` до production** (BACKEND_RULES §1.1).
8. **Отклонение от ADR-0010 / §13.1**: временное, с записью в `FRONTEND_RULES`; Dioxus-миграция — фаза 5 (стратегическая), критерий пересмотра: Dioxus-шелл покрывает ≥80 % экранов без регрессий; тогда `/api/v3` и legacy-ассеты удаляются вместе.
9. **API v3 — compat-слой, не наш public API**: не попадает в `openapi.json`/progenitor (ADR-0013), конверт ошибок `{"error":{"description","code"}}` — исключение из BACKEND_RULES §6.4 только для этого слоя.

## Последствия (Consequences)

### Положительные
- UI из архива работает 1:1; пользователь получает рабочий логин/shell/список комнат сразу.
- Логика остаётся в домене: адаптер тонкий, тесты v1 и v3 смотрят на одну модель.
- CSP со строгим `script-src`; traversal закрыт `ServeDir` (decode-once + покомпонентная валидация, тест-канорейка).

### Отрицательные / компромиссы
- ~26 MB рантайма + ~31 MB архива в репо (решение: `deployed/` — да; `source/` — opt-in; при git-init решить отдельно судьбу zip — LFS или внешнее хранилище, до первого коммита).
- legacy-сборка в замыкании `index.html` — 32 файла / ~6,3 MB (~22 % рантайма); сохранена ради 1:1, стоимость принята осознанно (аннотировано в `NOTES.md`).
- Дублирование v1/v3 до фазы 5; TTL 86400 и HS256 — долги до production.
- Критический путь первой загрузки UI: ~479 125 B gz (index+css+ru_RU) ≤ 800 KB (PROJECT_RULES §18.4) — в бюджете; стража-теста бюджета нет (долг фазы 4).

### Что нужно сделать для реализации
- [x] Импорт архива + `scripts/import-pnm-frontend.sh` + `MANIFEST.sha256` + `NOTES.md` (фаза 1).
- [x] Полный ребренд рантайма TrueConf MCU → PROSTOR (2026-10-17): 90 файлов + фиксы локалей и
  cache-bust словника — см. `client/pnm-frontend/NOTES.md` §«Ребренд PROSTOR»; юр. гейт R2 остаётся.
- [x] `frontend.rs` + роуты/кэш/CSP + тесты (фаза 1).
- [x] Удаление demo (`docs/progress/archive/demo-2026-10-16.rs`) + `build_app` в `lib.rs` (фаза 1).
- [ ] `/api/v3` фаза 2 (boot/shell/ws/auth) + фаза 3 (sessions/conferences) — по `FRONTEND_PORT_PLAN.md` §4.5/Приложение A.
- [ ] Фаза 5: Dioxus-миграция; снятие отклонения (удаление `/api/v3` и legacy).
- [ ] Юридический гейт R2: лицензия TrueConf-сборки (+ «PROSTOR») — до релиза (отметка в PROGRESS.md).

## Обоснование выбора библиотек

| Библиотека | Версия | Лицензия | Почему | Альтернативы отвергнуты |
|---|---|---|---|---|
| tower-http (`fs`) | 0.6.11 | MIT | ServeDir: decode-once, покомпонентная валидация путей, Range/HEAD/304/Last-Modified, precompressed_gzip, fallback | свой сервер (риск уязвимостей), `tower::ServeDir` (устар.), `axum-extra` (не покрывает) |
| sha2 | 0.10 | MIT OR Apache-2.0 | sha256 inline-скриптов для CSP | `ring` (тяжелее, не для wasm-соседей) |
| base64 | 0.22 | MIT OR Apache-2.0 | base64 для CSP-хэшей | ручной энкодер |
| tokio-tungstenite (dev) | 0.29 | MIT (OR Apache-2.0) | тесты WS `/api/v3/ws` | сырой TCP-клиент |

## Документация, изученная перед решением

- axum 0.8.9: `src/routing/path_router.rs` (nest/route, трейлинг-слэш, отсутствие авто-редиректов) — https://docs.rs/axum/0.8
- tower-http 0.6.11: `src/services/fs/serve_dir/*` (валидация путей, fallback, Range/HEAD/304, precompressed) — https://docs.rs/tower-http/0.6
- cargo-deny 0.18: схема `[graph]/[licenses]/[advisories]/[sources]` — https://embarkstudios.github.io/cargo-deny/checks/cfg.html
- Контракт UI: `scripts/pnm-smoke/observed_endpoints.json` + трассировка бандла (`FRONTEND_PORT_PLAN.md` Приложение A)
