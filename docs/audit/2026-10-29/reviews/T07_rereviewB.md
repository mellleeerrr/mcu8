# T07 — независимое re-ревью B (frontend): deep link, тема, импорт, docs (HEAD `25c9d99`)

- **Дата/время:** 2026-10-29, 13:17–13:30 MSK.
- **Ревьюер:** независимый delegated frontend/UX-ревьюер. Production-исходники не менялись; в живом дереве записан только этот файл (параллельные изменения других агентов в `crates/**` мне не принадлежат).
- **Объект:** HEAD `25c9d99` (`fix(T06,T07): declared capacity growth + empty AUTO windows; gzip metrics, atomic import, docs/tests`), ветка `master`. Предыдущие ревью: [`T07_reviewA.md`](T07_reviewA.md) — 8.9/done:yes, [`T07_reviewB.md`](T07_reviewB.md) — 8.9/done:yes.
- **Pristine-копия:** `git archive 25c9d99 | tar -x -C /dev/shm/t07rrB.ZThx7z`; отдельный HEAD-рантайм `git archive 25c9d99 client/pnm-frontend/deployed/pnm | tar -x -C /dev/shm/t07rrB-headrt` (215 файлов). Импорт выполнялся только в scratch-копии. `/tmp` не использовался: `TMPDIR=/dev/shm/t07rrB-tmp`.
- **Окружение:** Rust **1.94.0** (`rust-toolchain.toml`, `PATH="$HOME/.cargo/bin:$PATH"`), Node **v24.21.0**, Playwright **1.64.1**/Chromium из `tests/e2e/node_modules` (live-дерево, read-only). Debug-бинарь `vks-server` собран из pristine-копии (`CARGO_TARGET_DIR=/dev/shm/t07rrB-target`).
- **Ephemeral-стенд:** `127.0.0.1:18091` — импортированный рантайм, `127.0.0.1:18092` — pristine HEAD-рантайм; у каждого свежая SQLite-БД в `/dev/shm`, `VKS_WEBRTC_TRANSPORT=0`, `VKS_SIP_UAS_ENABLED=0`, `VKS_H323_UAS_ENABLED=0`, `VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0`. Живые `:8080`/`:18082` и `vks_data.db` не открывались и не перезапускались.

## Вердикт

**9.0 / 10 — done: yes. 0 P0, 0 P1, 1 P2, 6 P3.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **1** |
| **P3** | **6** |

Заявленная приёмка T07 подтверждена реальным браузерным исполнением на pristine-копии `25c9d99`:

- `tests/e2e/deeplink_auth.js` — **7/7** на HEAD-рантайме (`:18092`) и **7/7** после двух полных импортов (`:18091`), включая `card=1`, `tabs=6`, очищенную метку;
- отдельный Playwright-прогон security/theme/import — **33/33** на обоих рантаймах: open-redirect, TTL 9/11 мин, corrupted JSON, already-authorized, отсутствие redirect-loop, light/dark инвариант, DOMContentLoaded, toggle, manifest;
- импорт дважды: `MANIFEST` — **215/215**, `sha256sum -c` — **215 OK / 0 FAILED**, `.gz` — **0**, `deeplink.js`/`theme-toggle.js` и все пять проектных JS побайтово равны HEAD, project `<script>`-блок (306 B) и `#prostor-pnm-overrides` (11 198 B) идентичны, оба чекера зелёные;
- gzip и CSP после импортированного рантайма: `/pnm/` и `chunks/index.bundle…js` отдают `content-encoding: gzip` + `vary: Accept-Encoding`, CSP содержит 5 `sha256-…` хэшей.

При этом найден **новый P2 в самом deep-link-сценарии**: если пользователь вводит логин дольше 60 с, poll в `deeplink.js` уже остановлен, и после успешного входа открывается список сессий, а не исходная сессия. Это не ломает «быстрый» e2e (7/7), но нарушает acceptance «логин через UI → та же сессия» для реалистичного медленного входа и противоречит заявленному TTL 10 минут. Поэтому score ниже 9.5; done остаётся yes, так как P0/P1 нет и штатный acceptance-сценарий проходит.

---

## §1. Что проверялось и как

### 1.1. Стенд и бинарь

```bash
S=$(mktemp -d /dev/shm/t07rrB.XXXXXX)
git -C "/home/pro/Рабочий стол/mcu" archive 25c9d99 | tar -x -C "$S"
git -C "/home/pro/Рабочий стол/mcu" archive 25c9d99 client/pnm-frontend/deployed/pnm \
  | tar -x -C /dev/shm/t07rrB-headrt
cd "$S"
CARGO_TARGET_DIR=/dev/shm/t07rrB-target cargo build -p vks-server   # exit 0
```

Сервер (пример HEAD-рантайма; импортированный — тот же запуск с `VKS_FRONTEND_DIR` на `deployed/pnm` scratch-копии):

```bash
VKS_LISTEN_ADDR=127.0.0.1:18092 \
VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0 \
VKS_DATABASE_URL="sqlite:///dev/shm/t07rrB-server/headrt.db?mode=rwc" \
VKS_FRONTEND_DIR=/dev/shm/t07rrB-headrt/client/pnm-frontend/deployed/pnm \
VKS_WEBRTC_TRANSPORT=0 VKS_SIP_UAS_ENABLED=0 VKS_H323_UAS_ENABLED=0 \
VKS_JWT_SECRET=dev_ec994ad4e03aa63eafc3131719a0a50cce4dbfb474c855b0 \
VKS_ADMIN_PASSWORD=admin_password VKS_ALLOW_INSECURE_DEFAULTS=true \
VKS_CANDIDATE_ADDR=127.0.0.1 \
/dev/shm/t07rrB-target/debug/vks-server
```

`healthz`/`/pnm/` — 200; в логе `vks-server ready and listening … addr=127.0.0.1:18092`. SIP/H.323-листенеры этим процессом не поднимались (`VKS_*_UAS_ENABLED=0`; `:1720` в системе — pre-existing live sidecar, не мой процесс).

### 1.2. Импорт в pristine-копии

Архив `PNM-Frontend-20261016.zip` (sha256 `ea0314f8…cbdfa`) в git не входит; скопирован из живого репо в scratch read-only. Два прогона:

```bash
cd "$S"
TMPDIR=/dev/shm/t07rrB-tmp ./scripts/import-pnm-frontend.sh --force   # exit 0
TMPDIR=/dev/shm/t07rrB-tmp ./scripts/import-pnm-frontend.sh --force   # exit 0
```

Оба лога: `восстановлено файлов: 482, пропущено: 1`, `манифест пересобран: 215 строк`, `meta theme-color: #0e1621; manifest theme_color: #17212B`, `ссылки на скрипты: ok`.

---

## §2. Deep link: приёмка и security/edges

### 2.1. E2E 7/7 — HEAD и imported

```text
HEAD :18092
[deeplink] OK   api login — 200
[deeplink] OK   conference создана — 200
[deeplink] OK   без токена уводит на авторизацию — http://127.0.0.1:18092/pnm/authorization
[deeplink] OK   целевой URL сохранён — {"url":"/pnm/monitoring/sessions/ac26c3c9-…","at":1793269394496}
[deeplink] OK   после логина возврат на ту же сессию — http://127.0.0.1:18092/pnm/monitoring/sessions/ac26c3c9-…
[deeplink] OK   сессия отрисована (карточка/вкладки/имя) — {"card":1,"tabs":6,"body":true}
[deeplink] OK   метка deep-link очищена — null
[deeplink] итог: 7/7 ok
```

На импортированном рантайме (`:18091`) — те же **7/7**, `{"card":1,"tabs":6,"body":true}`, `итог: 7/7 ok`. Тест запускался с `NODE_PATH` на live `tests/e2e/node_modules`, `VKS_BASE_URL` на ephemeral-порт; live `:8080` не затрагивался.

### 2.2. Отдельный Playwright-прогон: 33/33

Ключевые строки (идентичны на HEAD и imported):

```text
PASS cold deep link → authorization — …/pnm/authorization
PASS saved JSON has target and at — {"url":"/pnm/monitoring/sessions/<uuid>","at":1793269344886}
PASS saved JSON contains no token and only url/at — … (ключи ровно at,url; токена нет)
PASS after login same session URL renders — {"card":1,"tabs":6}
PASS marker cleared after return — null
PASS no external requests (cold flow) — none
PASS absolute https rejected (no open redirect) — …/pnm/authorization external=none
PASS protocol-relative rejected (no open redirect) — …/pnm/authorization external=none
PASS TTL 9 min accepted — {"url":"…/sessions/<uuid>","saved":null,"card":1}
PASS TTL 11 min rejected — …/pnm/monitoring/sessions
PASS TTL 11 min marker removed — null
PASS corrupted JSON rejected (stays auth) — …/pnm/authorization
PASS corrupted JSON no deeplink crash — none
PASS already authorized deep link stays — {"url":"…/sessions/<uuid>","card":1}
PASS already authorized no marker — null
PASS already authorized no bounce — navs=3 (все три — тот же URL)
PASS expired token ends on authorization (no loop) — …/pnm/authorization
PASS expired token navigation count stable — navs 3 -> 3
SUMMARY 33 pass, 0 fail
```

Что именно проверялось:

| # | Критерий | Результат |
|---|---|---|
| 1 | Cold deep link без токена → `/pnm/authorization` → сохранение цели → логин → тот же `/pnm/monitoring/sessions/<id>` → карточка/вкладки → метка очищена | **PASS** (7/7 + 33/33) |
| 2 | В сохранённом JSON только `{url, at}`, токена нет | **PASS** (`Object.keys == at,url`; токен не встречается) |
| 3 | `https://evil…` и `//evil…` не дают open redirect, внешних запросов 0 | **PASS** (`external=none`, остаётся authorization) |
| 4 | TTL 10 мин: 9 мин — возврат, 11 мин — отказ + `removeItem` | **PASS** |
| 5 | Corrupted JSON / неверная форма — отказ, без падения страницы | **PASS** |
| 6 | Уже авторизованный deep link остаётся на сессии, метки нет, bounce нет | **PASS** (card=1, marker=null) |
| 7 | Нет redirect-loop на протухшем токене | **PASS** (3 навигации, затем стабильно; финал — authorization) |
| 8 | Метка очищается после успешного возврата | **PASS** (null) |

Отдельно: `DEEP_RE` (`deeplink.js:25`) якорит `/pnm/monitoring/sessions/` и `location.replace` не выходит за origin; `recall()` (`:60-78`) проверяет `typeof url/at`, TTL и `//`. Внешних запросов в прогонах не было.

### 2.3. P2 — deep link теряется, если логин занимает больше 60 с

`deeplink.js:115-127` опрашивает `localStorage` 240 раз по 250 мс и на `tries > 240` останавливается. Заявленный TTL метки — 10 минут (`:23`). Если пользователь открыл deep link, попал на authorization и вводит пароль дольше минуты, к моменту успешного входа poll уже мёртв; вендор-SPA сам уводит на список сессий, а `deeplink.js` больше не исполняется (SPA-переход без reload). Воспроизведение (импортированный рантайм, `:18091`):

```text
saved before wait = {"url":"/pnm/monitoring/sessions/3375ad0e-…","at":1793269610037}
waiting 65s (poll cap is 60s)...
saved after wait  = {"url":"/pnm/monitoring/sessions/3375ad0e-…","at":1793269610037}
final url = http://127.0.0.1:18091/pnm/monitoring/sessions
marker final = {"url":"/pnm/monitoring/sessions/3375ad0e-…","at":1793269610037}
card = 0
```

Итог: acceptance-сценарий «логин через UI → та же сессия» выполняется только при входе быстрее 60 с. Для медленного входа deep link теряется, метка остаётся в `sessionStorage` до 10 минут и при последующем открытии authorization (в пределах TTL) может дать лишний bounce на старую сессию. Это функциональный P2; исправление: poll до TTL/по `storage`/`visibilitychange`/`pageshow`, либо восстановление после SPA-логина без жёсткого 60-секундного окна. Заголовочный комментарий обещает «как только появляется токен» (`:9-10`) без оговорки про 60 с.

---

## §3. Тема: инвариант `meta[theme-color] == computed --c-bg`

Playwright-контексты `colorScheme: light|dark`, тема через `localStorage["VKS_PNM_THEME"]`, замер сразу после `DOMContentLoaded` и после `load + 900 мс`:

```text
theme light applied at DOMContentLoaded — meta=#f8f8f8 computed=#f8f8f8 cls=theme-light
theme light computed --c-bg=#f8f8f8
theme light meta == computed — meta=#f8f8f8 computed=#f8f8f8
theme light single class — cls=theme-light both=false violations=0
theme light toggle mounted — true
toggle light→dark updates class/meta/computed — {"cls":"theme-dark","meta":"#0e1621","computed":"#0e1621"}
toggle persists VKS_PNM_THEME=dark — dark
toggle dark→light restores #f8f8f8 — {"cls":"theme-light","meta":"#f8f8f8","computed":"#f8f8f8"}

theme dark applied at DOMContentLoaded — meta=#0e1621 computed=#0e1621 cls=theme-dark
theme dark computed --c-bg=#0e1621
theme dark meta == computed — meta=#0e1621 computed=#0e1621
theme dark single class — cls=theme-dark both=false violations=0
theme dark toggle mounted — true
static meta theme-color = #0e1621
manifest theme_color = #17212B
```

`violations=0` — MutationObserver, поставленный до page-скриптов, ни разу не увидел одновременных `theme-dark` и `theme-light`. `meta` совпадает с computed уже на `DOMContentLoaded`, то есть обработчик `theme-toggle.js:92-95` действительно доводит инвариант, а не только `load`. Значения `light #f8f8f8`, `dark #0e1621`, статический meta `#0e1621`, manifest `#17212B` — на HEAD и imported.

---

## §4. Импорт: 215/215, байт-идентичность проектного слоя, gzip/CSP

После двух `--force`:

```text
файлов в deployed/pnm:            215
строк MANIFEST.sha256:            215
sha256sum -c:                     215 OK, 0 FAILED (exit 0)
.gz в рантайме:                   0
check-pnm-runtime.py:             ok: 215 files, manifest matches, script refs exist, no .gz
check-pnm-theme.py:               ok: meta == dark --c-bg, light token present, toggle uses computed token, manifest #17212B
```

Проектный слой:

| Файл/блок | Результат |
|---|---|
| `deeplink.js` | sha256 `05a434652ca16f7e6de587956d843820183b6efe6e6f99e2fa3a960c9640ddda`; `cmp` с blob `25c9d99` — **IDENTICAL** |
| `theme-toggle.js` | sha256 `dc7faddf45905130970805c1a729b793890d1ba71e7be6b540b25c5e3f303d45`; `cmp` — **IDENTICAL** |
| `perf-overrides.js`, `api-errors.js`, `codian-layouts.js` | **IDENTICAL** HEAD |
| project `<script>` block | 306 B, `identical=True` (HEAD vs imported) |
| `<style id="prostor-pnm-overrides">` | 11 198 B, `identical=True` (HEAD vs imported) |

Это подтверждает, что импорт не теряет `<script src="/mcu/deeplink.js">` и не срезает theme-токены light/dark. Браузерные прогоны §2–§3 на импортированном рантайме — **33/33** и **7/7**, то есть проверен не только файл на диске, но и исполнение после импорта.

HTTP после импортированного рантайма:

```text
GET /pnm/ (Accept-Encoding: gzip)              → 200, content-encoding: gzip, vary: Accept-Encoding, cache-control: no-cache
GET /mcu/chunks/index.bundle-2.2.1.66.js gzip  → 200, content-encoding: gzip, vary: Accept-Encoding, cache-control: no-cache
GET /pnm/ (identity)                           → 200, content-length: 28992, vary: Accept-Encoding
GET /mcu/deeplink.js                           → 200, content-length: 4516, cache-control: no-cache
CSP sha256-хэшей на /pnm/                       → 5
```

## §5. Документация: что стало правдой, что осталось

Коммит действительно понизил главный overclaim: шапка `deeplink.js:1-17` теперь прямо говорит, что SPA-логаут с истёкшим токеном идёт через History API, `pagehide` не срабатывает, и cold deep-link с протухшим токеном цель не сохраняет — «принятое ограничение T07, а не покрытый 401». `NOTES.md:27-32,63-66,115-121`, `MASTER_PLAN.md:109-112,186` и `ADR-0027:47-48` обновлены под 215 файлов и runtime gzip; `MASTER_PLAN` больше не ссылается на несуществующий `patch-pnm-auth-deeplink.py`; task-list использует `check-pnm-theme.py`, а не `.sh`.

Остались P3-расхождения (не влияют на runtime/acceptance, но «docs truth» не идеальна):

- **P3-1.** `deeplink.js:93` всё ещё называет `pagehide` «страховкой 401-разлогина», хотя строки `:11-15` и фактическое поведение говорят обратное. Это остаток ровно того overclaim, который коммит обещал понизить; шапку поправили, inline-комментарий — нет.
- **P3-2.** `NOTES.md:84-86` описывает проектный слой как `perf-overrides.js` + `api-errors.js` + CSS, а `scripts/import-pnm-frontend.sh:15-19` — как `perf-overrides.js` + `api-errors.js` + `codian-layouts.js` + CSS; оба списка не упоминают `theme-toggle.js` и `deeplink.js`, которые импорт реально восстанавливает (лог: `восстановлен: theme-toggle.js`, `deeplink.js`).
- **P3-3.** `scripts/import-pnm-frontend.sh:5,25-26` всё ещё документирует `--gzip` как «сгенерировать `.gz` для precompressed_gzip», хотя реализация флаг игнорирует (`:378-382`). NOTES и ADR поправлены, шапка скрипта — нет.
- **P3-4.** `ADR-0027:14` говорит «весь `deployed/pnm` (216/216) читается», а `:47` — «`MANIFEST.sha256` — 215 файлов»; фактически 215. `:12` («216 файлов рантайма») — исторический счёт записей архива, но в одном документе это выглядит противоречиво.
- **P3-5.** `deeplink.js:67,73` отвергает corrupted JSON и внешние/`//` URL, но не удаляет ключ на этих invalid-путях (удаление только по TTL, `:68-70`). Проверено: после загрузки authorization в `sessionStorage` остаются `"{not-json"` и `{"url":"https://evil…"}`. На безопасность не влияет (redirect не происходит), но мусор живёт до перезаписи/закрытия вкладки и может быть позже принят за метку.
- **P3-6 (pre-existing, documented).** Полный re-import в pristine-копии меняет **91 файл** относительно HEAD: иконки/`manifest.webmanifest`, 24 chunk-файла и `index.html`; `<title>`, `description`, `application-name` и manifest `name` возвращаются к `TrueConf MCU`. Все пять проектных JS и оба блока идентичны, оба чекера и `sha256sum -c` зелёные. `NOTES.md:95-98` честно предупреждает, что слой PROSTOR-ребренда (`frontend/06-rebrand/`) импорт не применяет и его надо накатывать вручную, поэтому это не overclaim T07 и не блокер acceptance; но «полная идентичность дерева после импорта» не достигается, и зелёные гейты этого не ловят.

---

## §6. Гигиена

- Живая БД не открывалась: до и после — `vks_data.db`, 3 248 128 B, mtime `2026-10-29 12:38:31.642604976 +0300`, md5 `a925e7fa52831ed3aa8480c0e9531ff0`.
- Живые листенеры `:8080` (PID 3255206) и `:18082` (PID 3253281) — те же pre-existing процессы, не перезапускались и не нагружались.
- Ephemeral-серверы `:18091`/`:18092` получили SIGTERM, в логах `received SIGTERM, initiating shutdown` / `gracefully terminated`; `ss` показывает, что порты свободны.
- Удалены все `/dev/shm/t07rrB*` (копии, target 2.2 GB, tmp 143 MB, БД, логи) и `/dev/shm/t07rrB-headrt`; проверено, что `t07rrB*` больше нет. `/tmp` этим ревью не использовался (старые `/tmp/pnm-import.*` имеют mtime 13:05, до старта ревью в 13:17, и мне не принадлежат).
- В живом дереве мной добавлен только `docs/audit/2026-10-29/reviews/T07_rereviewB.md`; параллельные правки `crates/**`/`Cargo.lock` в рабочем дереве — от других агентов, не мои.

---

## §7. Остаточные риски и ограничения

1. **SPA-логаут с протухшим токеном** по-прежнему теряет deep link; это документированное принятое ограничение T07 (`deeplink.js:11-15`), не re-фиксилось и не входит в текущий acceptance.
2. **P2 slow-login** — единственный функциональный дефект, найденный в этом ре-ревью; он не покрыт `deeplink_auth.js` (тест логинится сразу) и проявляется при входе >60 с.
3. **Rebrand revert** при `--force`-импорте — pre-existing, задокументирован в NOTES; при полном доверии к зелёным гейтам можно незаметно откатить брендинг.
4. `tests/e2e/deeplink_auth.js:20` по-прежнему по умолчанию целится в live `:8080` и не подключён к CI; запускать только с явным `VKS_BASE_URL` на ephemeral-стенд.

---

**Итог:** acceptance T07 на `25c9d99` подтверждён исполняемыми прогонами (7/7 × 2, 33/33 × 2, импорт 215/215, тема и security-края), P0/P1 отсутствуют. Блокирующих замечаний нет; score **9.0/10, done: yes** — ниже 9.5 из-за нового P2 (deep link теряется при логине >60 с) и остаточных P3 по документации/комментариям.
