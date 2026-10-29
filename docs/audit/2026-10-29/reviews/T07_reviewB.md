# T07 — независимое ревью B (frontend): deep-link survival, тема, импорт, gzip (HEAD `2442a16`, T07 `916824b`)

- **Дата:** 2026-10-29, 12:40–13:05 MSK.
- **Ревьюер:** независимый delegated UX/JS-reviewer (frontend-половина T07); production-исходники не менялись, записан только этот файл.
- **Объект:** HEAD `2442a16`; T07-код — `916824b` («feat(T07): PNM deep-link survival, theme invariant, runtime gzip, import integrity»). Приёмка — [`MASTER_TASK_LIST_2026-10-29.md`](../../../progress/MASTER_TASK_LIST_2026-10-29.md):45 и [`00_MASTER_PLAN.md`](../00_MASTER_PLAN.md):186.
- **Полигон:** `git archive 2442a16 | tar -x -C /dev/shm/t07revB-222922/repo`; импорт-проверка — в этой же scratch-копии. `/tmp` не использовался: `TMPDIR=/dev/shm/t07revB-222922/tmp`.
- **Живая среда не тронута:** live `vks_data.db` не открывалась (mtime `2026-10-29 12:38:31.642604976 +0300` до начала ревью); live `:8080`/`:18082` не использовались. Ephemeral `vks-server` (debug-бинарь, scratch SQLite в `/dev/shm`) поднимался на `127.0.0.1:42433`–`:42436` с `VKS_WEBRTC_TRANSPORT=0`, `VKS_SIP_UAS_ENABLED=0`, `VKS_H323_UAS_ENABLED=0`, `VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0`, `VKS_FRONTEND_DIR` на HEAD- и импортированный рантайм; все процессы убиты.
- **Чистота:** в живом дереве добавлен только `docs/audit/2026-10-29/reviews/T07_reviewB.md`; scratch-копии/логи/БД удалены, `ps`/`ss` не показывают оставшихся серверов на `4243x`.

## Вердикт

**Verdict: 8.9 / 10 — done: yes. 0 P0, 0 P1, 1 P2, 4 P3.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **1** |
| **P3** | **4** |

Вся заявленная приёмка T07 выполнена и перепроверена исполняемыми прогонами на pristine-копии `2442a16`: e2e «deep link без токена → логин → та же сессия» — **7/7** (и на HEAD-рантайме, и после полного re-import); gzip `content-encoding` на `/pnm/` и `/mcu/chunks/*.js` при `Vary: Accept-Encoding`; `meta[theme-color] == computed --c-bg` после загрузки в light и dark (HEAD и импортированный рантайм); импорт дважды + `--gzip` сохраняет `theme-toggle.js`/`deeplink.js` и инвариант темы, `MANIFEST` — **215/215**, `sha256sum -c` — OK; оба CI-чекера зелёные. Deep-link-скрипт устойчив к open redirect, TTL, multi-tab и corrupted JSON; в реальном runtime не наблюдалось ни зацикливания, ни утечки токена.

Единственный P2 — **заявленный в коммите/комментарии «pagehide страхует 401-разлогин» не подтверждается для штатного SPA-разлогина**: вендор уходит на `/pnm/authorization` через History API, `pagehide` не срабатывает, цель не запоминается (а при уже сохранённой цели и протухшем токене страница авторизации делает лишний bounce и съедает метку). Это не входит в acceptance T07 (там только cold deep link), поэтому done: yes; но формулировку «401 logout» в коммите/коде надо понизить до «hard unload/reload» либо чинить.

---

## §1. Воспроизведение и инфраструктура

```bash
D=/dev/shm/t07revB-222922
mkdir -p "$D/repo" && cd "/home/pro/Рабочий стол/mcu"
git archive 2442a16 | tar -x -C "$D/repo"

# архив PNM gitignored (30 MB), поэтому скопирован из живого репо в scratch (read-only источник)
cp -p client/pnm-frontend/archive/PNM-Frontend-20261016.zip \
      "$D/repo/client/pnm-frontend/archive/"

cd "$D/repo"
VKS_LISTEN_ADDR=127.0.0.1:42433 \
VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0 \
VKS_FRONTEND_DIR="$D/repo/client/pnm-frontend/deployed/pnm" \
VKS_DATABASE_URL="sqlite://$D/scratch.db?mode=rwc" \
VKS_WEBRTC_TRANSPORT=0 VKS_SIP_UAS_ENABLED=0 VKS_H323_UAS_ENABLED=0 \
VKS_JWT_SECRET=dev_ec994ad4e03aa63eafc3131719a0a50cce4dbfb474c855b0 \
VKS_ADMIN_PASSWORD=admin_password VKS_ALLOW_INSECURE_DEFAULTS=true \
VKS_CANDIDATE_ADDR=127.0.0.1 \
"/home/pro/Рабочий стол/mcu/target/debug/vks-server" &  # ready: 127.0.0.1:42433
```

Для импорт-проверки сервер перезапускался с `VKS_FRONTEND_DIR="$D/repo/client/pnm-frontend/deployed/pnm"` уже после `import-pnm-frontend.sh --force` (порт `:42434`), отдельный прогон — `:42435`/`:42436`. Debug-бинарь `target/debug/vks-server` (mtime 12:34, собран из того же рабочего дерева) содержит `CompressionLayer` и тему T07 — это подтверждено поведением ниже.

## §2. Deep-link: критерии

| # | Критерий | Результат | Доказательство |
|---|---|---|---|
| 1 | Cold deep link без токена → `/pnm/authorization` → логин → та же сессия | **PASS** | `tests/e2e/deeplink_auth.js` **7/7**, дважды (HEAD и imported) |
| 2 | Запоминание только в `sessionStorage` | **PASS** | ключ `vks_deeplink_return` только в sessionStorage; в localStorage — только вендорные ключи; tab B его не видит |
| 3 | TTL 10 мин | **PASS** | `deeplink.js:20`; 11 мин → отказ + `removeItem`; 9 мин → возврат |
| 4 | Open redirect / `//` / scheme | **PASS** | `https://evil.example`, `//evil.example` отвергнуты, внешних запросов 0; `DEEP_RE` (`deeplink.js:22`) якорит `/pnm/monitoring/sessions/` и `location.replace` остаётся same-origin |
| 5 | No token leakage | **PASS** | сохраняется только `{url, at}`; token читается, но не пишется/не логируется; в e2e saved = `/pnm/monitoring/sessions/<uuid>` без токена |
| 6 | No infinite redirect | **PASS** | stale-token сценарий: 4 main-frame навигации и остановка на `/pnm/authorization`; за 6 с число навигаций не растёт |
| 7 | Уже авторизован | **PASS** | deep link с валидным токеном остаётся на сессии, метки нет, bounce нет |
| 8 | После логина: poll 250 мс, максимум 60 с | **PASS** | `deeplink.js:113-115`: `setInterval(..., 250)`, `tries > 240` ⇒ 60 с; e2e возврат происходит |
| 9 | 401 logout via `pagehide` | **FAIL (частично)** | `pagehide` пишет цель только при hard unload/reload; штатный 401 — SPA-навигация без `pagehide` ⇒ цель не сохраняется. P2-1 |
| 10 | Multiple tabs | **PASS** | cold deep link в tab A сохраняет метку; tab B (same context) — `saved=null`, остаётся на authorization |
| 11 | Corrupted JSON | **PASS** | `JSON.parse` в try/catch, неверная форма отвергается; P3-3 — ключ не удаляется |
| 12 | Ссылка в `index.html` project block и сохранение импортом | **PASS** | `index.html:29-35`, `deeplink.js` — строка 34; импорт сохраняет байт-в-байт (sha256 ниже) |

### 2.1. E2E (7/7)

```bash
cd "$D/repo"
NODE_PATH="/home/pro/Рабочий стол/mcu/tests/e2e/node_modules" \
VKS_BASE_URL="http://127.0.0.1:42433" node tests/e2e/deeplink_auth.js
```

```text
[deeplink] OK   api login — 200
[deeplink] OK   conference создана — 200
[deeplink] OK   без токена уводит на авторизацию — http://127.0.0.1:42433/pnm/authorization
[deeplink] OK   целевой URL сохранён — {"url":"/pnm/monitoring/sessions/916c4036-…","at":1793267037631}
[deeplink] OK   после логина возврат на ту же сессию — http://127.0.0.1:42433/pnm/monitoring/sessions/916c4036-…
[deeplink] OK   сессия отрисована (карточка/вкладки/имя) — {"card":1,"tabs":6,"body":true}
[deeplink] OK   метка deep-link очищена — null
[deeplink] итог: 7/7 ok
```

Повтор на импортированном рантайме (`:42434`): те же 7/7, `{"card":1,"tabs":6,"body":true}`. Это подтверждает, что `deeplink.js` реально исполняется до бандла и что импорт не теряет `<script src="/mcu/deeplink.js">`.

### 2.2. Security/edge-пробы (13/13)

Отдельный Playwright-прогон на ephemeral-сервере с реальной сессией (конференция создана через API):

```text
PASS valid target redirects to session
PASS absolute https URL rejected (no open redirect)
PASS protocol-relative // rejected
PASS valid-prefix URL with // stays same-origin
PASS corrupted JSON rejected
PASS wrong shape rejected
PASS expired (>10min) rejected + removed
PASS 9min old accepted
PASS future timestamp accepted (clock skew)
PASS tab isolation: A saved, B unaffected
PASS already authorized deep link stays, no bounce/no key
PASS 401-ish: pagehide saved target before auth redirect
PASS 401-ish: after login returns to same session
[security] 13/13 pass
```

Во всех кейсах `external=none` (ни одного запроса за пределы `BASE`). «401-ish» здесь — искусственный **hard reload** после удаления токена; именно в этом сценарии `pagehide` действительно пишет цель и возврат работает. Это и есть граница корректности P2-1.

### 2.3. P2-1: `pagehide` не покрывает штатный 401-разлогин

Сценарий A — cold deep link с протухшим токеном (`localStorage["TrueConfMCU_AccessToken"]="expired.invalid.jwt"`):

```text
final = http://127.0.0.1:42435/pnm/authorization
main-frame navs (3):
  /pnm/monitoring/sessions/<id>
  /pnm/monitoring/sessions/<id>
  /pnm/authorization
trace (sessionStorage, переживает reload): pagehide для main frame — НЕТ
final sessionStorage["vks_deeplink_return"] = null
```

Вендор обрабатывает 401 через SPA-router (`history`), а не через unload документа: `pagehide` (единственный писатель цели в этой ветке, `deeplink.js:98`) не срабатывает. После логина пользователь попадает на список сессий, deep link потерян.

Сценарий B — уже есть сохранённая цель и протухший токен на странице авторизации:

```text
final = /pnm/authorization
main-frame navs (4): authorization → sessions/<id> → sessions/<id> → authorization
final vks_deeplink_return = null, token = null
```

Здесь `deeplink.js` на auth-странице видит **наличие** токена (`if (token())`, `deeplink.js:106-109`), делает `forget()` + `location.replace(target)`, target получает 401 и возвращает на auth — метка уже съедена, deep link потерян. Зацикливания в текущем runtime нет (`4` навигации), но проверка «токен есть» вместо «токен валиден» оставляет латентный риск бесконечного bounce, если вендор когда-нибудь начнёт делать hard-navigation на 401.

Дополнительно: `pagehide` пишет метку при **любом** hard-уходе с deep-link-страницы (закрытие/переход), поэтому в течение 10 минут последующий разлогин из другой страницы той же вкладки может вернуть на старую сессию. Это следствие выбранной стратегии, не блокер.

**Вердикт по критерию:** `pagehide` корректен как страховка hard unload/reload, но не как «401 logout». Формулировки `deeplink.js:90` («страхует 401-разлогин») и commit message («pagehide covers 401 logout») — overclaim относительно фактического поведения; e2e этот путь не покрывает.

## §3. Тема: инвариант `meta[theme-color] == computed --c-bg`

### 3.1. Статика через HTTP

```bash
curl -sS http://127.0.0.1:42433/pnm/ | grep -o '<meta name="theme-color" content="[^"]*"'
# <meta name="theme-color" content="#0e1621"
curl -sS http://127.0.0.1:42433/mcu/assets/manifest.webmanifest | grep -o '"theme_color"[^,]*'
# "theme_color": "#17212B"
curl -sS http://127.0.0.1:42433/pnm/ | grep -oE 'html\.theme-(light|dark) \{[^}]*\}'
# html.theme-light { --c-bg: #f8f8f8; }
# html.theme-dark { --c-bg: #0e1621; }
```

`deeplink.js`/`theme-toggle.js` подключены в project block до vendor-бандла (`index.html:29-35`); CSS-блок `prostor-pnm-overrides` — `index.html:41-45`.

### 3.2. Браузерный инвариант (HEAD и imported)

Playwright, `colorScheme: light|dark`, предпочтение темы через `localStorage["VKS_PNM_THEME"]`, замер после `load` + 600 мс:

```text
[theme light] class=theme-light computed=#f8f8f8 meta=#f8f8f8 equal=true bothSeen=false paint=292
[theme light] PASS (single class=true, invariant=true)
[theme dark]  class=theme-dark  computed=#0e1621 meta=#0e1621 equal=true bothSeen=false paint=352
[theme dark]  PASS (single class=true, invariant=true)
```

- `bothSeen=false` — `theme-dark` и `theme-light` никогда не присутствуют одновременно; в light тема-класс выставлен уже на первом rAF (`t≈275 мс`) до first paint (`292 мс`), то есть вспышки тёмного класса нет.
- В imported-рантайме (`:42434`) те же значения: light `#f8f8f8`, dark `#0e1621`, `equal=true`, `bothSeen=false`.
- `theme-toggle.js` читает именно computed (`getPropertyValue('--c-bg')`, `theme-toggle.js:30`) и пишет в meta (`:34`); `DOMContentLoaded`-повтор (`:92-95`) доводит инвариант после разбора документа.
- Toggle-пробы: **7/7** — light→dark и dark→light меняют класс/meta/computed согласованно, `localStorage` сохраняет выбор, после reload тема держится, кнопка смонтирована и на auth-, и на авторизованной странице.

### 3.3. P3-1: meta в light остаётся тёмным до загрузки

В light-профиле первый paint происходит с `meta=#0e1621` (статический дефолт), хотя `html.class=theme-light` уже корректен; meta обновляется только на `DOMContentLoaded`/`load` (в замере — `t≈1031 мс` против first paint `292 мс`). Причина: `theme-toggle.js` стоит в `<head>` **до** `<meta name="theme-color">`, поэтому ранний `apply()` meta не находит. Это не нарушает acceptance «после загрузки» и не даёт class-flash, но даёт ~0.7 с несоответствия цвета браузерного chrome фактическому фону. Косметика, P3.

## §4. Импорт: сохранение проектного слоя и инварианта

Архив `PNM-Frontend-20261016.zip` (sha256 `ea0314f8…cbdfa`, 30 MB) в git не входит (`.gitignore:21`), поэтому в scratch он скопирован из живого репо; сам `deployed/pnm` — pristine `2442a16`.

```bash
cd "$D/repo"
TMPDIR="$D/tmp" VKS_PNM_ARCHIVE="$D/repo/client/pnm-frontend/archive/PNM-Frontend-20261016.zip" \
  ./scripts/import-pnm-frontend.sh --force          # IMPORT_EXIT=0
# повторно:
TMPDIR="$D/tmp" VKS_PNM_ARCHIVE=… ./scripts/import-pnm-frontend.sh --force   # IMPORT_EXIT=0
# и с флагом:
TMPDIR="$D/tmp" VKS_PNM_ARCHIVE=… ./scripts/import-pnm-frontend.sh --force --gzip  # IMPORT_EXIT=0
```

Факты:

- лог: `восстановлено файлов: 482, пропущено: 1`; сохранены/восстановлены `perf-overrides.js`, `api-errors.js`, `codian-layouts.js`, **`theme-toggle.js`**, **`deeplink.js`**; `index-overrides.html` (11 198 симв.), `index-scripts.html` (306 симв.); `манифест: … (215 строк)`.
- `файлов в рантайме: 210` в логе — это **до** восстановления пяти проектных файлов; итог — **215** (`find … | wc -l` = 215, `MANIFEST` = 215 строк).
- `sha256sum -c ../../MANIFEST.sha256` из `deployed/pnm/` — **OK, 215/215**; `check-pnm-runtime.py` — `ok: 215 files, manifest matches, script refs exist, no .gz`; `check-pnm-theme.py` — `ok: meta == dark --c-bg, light token present, toggle uses computed token, manifest #17212B`.
- `deeplink.js` и `theme-toggle.js` после двух импортов побайтово равны HEAD:
  - `0fc975426ad2b479b3a84f6b5508cbb0ec186a72bf73fa6640a9818f2a9b36f2` — `deeplink.js`;
  - `dc7faddf45905130970805c1a729b793890d1ba71e7be6b540b25c5e3f303d45` — `theme-toggle.js`.
- project script block и `<style id="prostor-pnm-overrides">` после импорта **диффом не отличаются** от `2442a16` (`diff -u` пуст).
- `--gzip` теперь игнорируется (`import-pnm-frontend.sh:354`), `.gz` в рантайме — **0**; манифест их исключает.
- fail-closed на `<script src>` проверен косвенно: чекер ссылок и сам импорт зелёные; при отсутствующем `deeplink.js` импорт упал бы на шаге «проверка ссылок на проектные скрипты» (`import-pnm-frontend.sh:315`).

## §5. Gzip (frontend acceptance)

```text
GET /pnm/ (Accept-Encoding: gzip)          → 200, content-encoding: gzip, vary: Accept-Encoding
GET /mcu/chunks/index.bundle-…js (gzip)    → 200, content-encoding: gzip, vary: Accept-Encoding
GET /pnm/ (без Accept-Encoding)            → 200, content-length: 75463, vary: Accept-Encoding
GET /mcu/deeplink.js                       → 200, content-length: 4130, cache-control: no-cache
```

`Vary` присутствует и на gzip-, и на identity-ответе; `/pnm/` и JS-чанки сжимаются; `deeplink.js` в этом curl-запросе отдавался без `Accept-Encoding` (identity, 4130 B), а пропуск файлов < 1 KiB покрыт unit-тестом `frontend.rs:625-632`. CSP-заголовок после рестарта содержит 5 `sha256-…` inline-хэшей; страницы с project-layer скриптами грузятся без CSP-ошибок (e2e/темы прошли на перезапущенном сервере).

## §6. Качество `tests/e2e/deeplink_auth.js`

- 7 проверок, 7/7 на HEAD и imported; сценарий соответствует acceptance «deep link без токена → логин → та же сессия»; cleanup удаляет созданную конференцию.
- P3-4: тест не подключён к CI (в `.github/workflows/ci.yml` только `check-pnm-runtime.py`/`check-pnm-theme.py`); check «сессия отрисована» лоялен (`card>=1 || tabs>=3 || body includes name`) — фактически наблюдались `card=1, tabs=6, body=true`, но при регрессе одного из трёх достаточно любого; при незаданном `VKS_BASE_URL` тест по проектной традиции (`live.js` тоже) падает на live `:8080`, хотя в шапке написано «на живом :8080 не запускать». Для T07-приёмки это не блокер, но 401-путь тест не покрывает (см. P2-1).

## §7. Документация: что заявлено vs что работает

| Документ | Заявление | Факт | Оценка |
|---|---|---|---|
| `00_MASTER_PLAN.md:186` (T07 acceptance) | deep link → логин → та же сессия; gzip; meta == computed; импорт сохраняет theme-toggle.js; 215/215 | Всё проверено и выполнено | **OK** |
| `MASTER_TASK_LIST_2026-10-29.md:45` | те же 5 критериев | Всё выполнено; строка T07 (`:20`) остаётся `⏳` — overclaim «done» нет | **OK** |
| `MASTER_TASK_LIST…:45` | `check-pnm-theme.sh` | В репо `scripts/check-pnm-theme.py` | P3-2 |
| `00_MASTER_PLAN.md:109` | «T07 патчит `patch-pnm-auth-deeplink.py`» | Такого файла нет; реализован `deeplink.js` | P3-2 |
| `00_MASTER_PLAN.md:112` | «Gzip (precompressed) ❌ … отдавать `.gz`» | T07 сознательно сделал runtime gzip; строка устарела и противоречит `:186` | P3-2 |
| `ADR-0027:47` | `MANIFEST` — 215 файлов; runtime gzip | Верно | **OK** |
| `ADR-0027:12,14` | «216 файлов рантайма», «deployed/pnm (216/216)» | Фактически 215; 216 — исторический счёт архива/до исключения `*.bak*` | P3-2 |
| `NOTES.md:389` | `sha256sum -c` → 215 строк OK | Верно | **OK** |
| `NOTES.md:76,117-119` | `--gzip` генерирует `.gz` для `precompressed_gzip` | `--gzip` игнорируется, `.gz` не создаются; runtime `CompressionLayer` | P3-2 |
| `NOTES.md:79-96` | «четыре слоя»: project layer = perf/api/CSS; patches = 2 скрипта | Не упомянуты `theme-toggle.js`, `deeplink.js`, theme-invariant шаг и `patch-pnm-layout-crash.py` | P3-2 |
| `scripts/import-pnm-frontend.sh:25` | `--gzip — сгенерировать .gz` | Строка 354: «флаг игнорируется» | P3-2 |
| commit `916824b` message / `deeplink.js:90` | «pagehide covers 401 logout» / «страхует 401-разлогин» | Штатный 401 — SPA-навигация без `pagehide`; цель не сохраняется (P2-1) | **P2-1** |

Итог: acceptance-доки не завышают результат; overclaim 401 — в commit message и комментарии кода, а не в MASTER_PLAN/ADR/NOTES. Остальные расхождения — устаревшие строки плана/заметок, не влияющие на работу.

## §8. Остаточные риски

| # | Severity | Файл/место | Проблема | Влияние / что сделать |
|---|---|---|---|---|
| P2-1 | **P2** | `deeplink.js:90,98,106-109`; commit `916824b` | `pagehide` не срабатывает при SPA-навигации 401; проверка «токен присутствует» на auth-странице делает bounce и съедает метку при протухшем токене. Возможен латентный цикл, если 401 станет hard-navigation. | Deep link теряется при истечении токена — заявленная «страховка» не работает. Варианты: валидировать токен (`/api/v3/login/status`) перед redirect; запоминать цель при входе на deep link и сбрасывать после успешного рендера; ловить SPA-навигацию (history/popstate) или 401-событие вендора. |
| P3-1 | P3 | `index.html:38`, `theme-toggle.js:92` | В light `meta[theme-color]` = `#0e1621` от first paint до `DOMContentLoaded`/`load` (~0.7 с в замере). | Косметика browser chrome; class-flash нет. Можно обновлять meta сразу после появления элемента (mutation observer) или ставить light-meta до загрузки. |
| P3-2 | P3 | `NOTES.md:76,79,117-119`; `00_MASTER_PLAN.md:109,112`; `0027…:12,14`; `import-pnm-frontend.sh:25` | Устаревшие/несогласованные утверждения: `--gzip` vs runtime gzip; список project layer; несуществующий `patch-pnm-auth-deeplink.py`; `check-pnm-theme.sh` vs `.py`; 216 vs 215. | Не влияет на runtime, но вводит в заблуждение при следующем импорте/ревью. Обновить строки. |
| P3-3 | P3 | `deeplink.js:61-71` | Битый JSON, absolute URL, неверная форма, `//` отвергаются, но ключ не удаляется (удаляется только по TTL). | Мусор в sessionStorage до перезаписи/закрытия вкладки; на поведение не влияет. Удалять ключ на любом invalid-пути. |
| P3-4 | P3 | `tests/e2e/deeplink_auth.js` | Не в CI; лояльная проверка рендера; дефолт `VKS_BASE_URL` → live `:8080`; 401-путь не покрыт. | Риск ложноположительного «rendered» и случайного запуска на live; добавить в CI/guard и отдельный 401-тест. |

## §9. Hygiene

- В живом дереве изменён только этот файл; `git status` до/после — без новых правок production-кода.
- Live DB не открывалась: mtime `2026-10-29 12:38:31.642604976 +0300` (до начала ревью) не изменился; live `:8080`/`:18082` не перезапускались и не нагружались.
- Все ephemeral-серверы (`:42433`–`:42436`) убиты; `ps`/`ss` чист; scratch-каталог `/dev/shm/t07revB-222922`, логи, probe-скрипты и scratch SQLite удалены; `/tmp` не использовался (`TMPDIR` указывал в `/dev/shm`).
- Внешние сети не использовались; `web_search`/`web_fetch` не вызывались.

**Итог:** T07 принимается по всем заявленным критериям (8.9/10, done: yes). Единственный содержательный долг — P2-1: «401 logout via pagehide» не покрывает штатный SPA-разлогин; это нужно либо починить, либо убрать overclaim из комментария и commit message.
