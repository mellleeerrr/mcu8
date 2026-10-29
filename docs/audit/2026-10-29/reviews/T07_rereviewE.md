# T07 — независимое re-ревью E (frontend): deep link, тема, импорт, runtime gzip (commit `3e2983c`)

- **Дата/время:** 2026-10-29, 15:08–15:30 MSK.
- **Ревьюер:** независимый delegated frontend-ревьюер. Production-исходники не менялись; в живом дереве записан только этот файл.
- **Объект:** commit `3e2983c` («fix(T04b): derive production RoomCaps from the codec SSOT»), родитель `d046fc6`. T07-файлы в `3e2983c` не менялись: `git diff ca6c4b8 3e2983c -- client/pnm-frontend scripts/import-pnm-frontend.sh docs/decisions/0027-pnm-legacy-frontend-adopt-and-shim.md tests/e2e/deeplink_auth.js` — пусто (blob-hash совпадают). Сам `3e2983c` трогает только `crates/vks-server/src/main.rs` и `crates/vks-signaling/src/handler.rs`.
- **Pristine-копия:** `git archive 3e2983c | tar -x -C /dev/shm/t07_rvE_1793275729/repo` (55 MB). `/tmp` для scratch не использовался. Node **v24.21.0**, Playwright **1.64.1**/Chromium из live `tests/e2e/node_modules` (read-only). Архив PNM (`PNM-Frontend-20261016.zip`, sha256 `ea0314f8…cbdfa`) читался из live-репо read-only.
- **Предыдущие ревью:** A — 9.3/done:yes; B — 9.0/done:yes; C — 8.5/done:no (P1 build-break); D — 9.2/done:no (T07 substance 9.5, P1 `clippy::type_complexity` в `recorder.rs`, P3 docs).

## Вердикт

**9.5 / 10 — done: yes. 0 P0, 0 P1, 0 P2, 4 P3.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **0** |
| **P3** | **4** |

P1 ревью D закрыт: `cargo clippy -p vks-server --offline -- -D warnings` — **exit 0** (1m19s), alias `StoppedRecord` в `crates/vks-server/src/recorder.rs:535`. T07-приёмка пройдена реальным браузером: cold deep link, логин >60 с, security-края, тема, два `--force`-импорта 215/215, байт-идентичный проектный слой, 0 `.gz`, gzip + ровно один `Vary`, TERM-restore с очисткой TMP, импортированный рантайм. P3-1..3 ревью D (docs truth) закрыты в `ca6c4b8`; остались 4 неблокирующих P3 (два известных из D и два уточнения, см. §10).

---

## §1. Гейты

| Гейт | Команда | Результат |
|---|---|---|
| build | `CARGO_TARGET_DIR=/dev/shm/…/target cargo build -p vks-server --offline` | **PASS**, exit 0, 1m38s, pristine без shims |
| clippy | `cargo clippy -p vks-server --offline -- -D warnings` | **PASS**, exit 0, 1m19s |
| fmt | `cargo fmt --all -- --check` | **PASS**, exit 0 |
| frontend lib | `cargo test -p vks-server --lib frontend --offline` | **PASS**, 20 passed / 0 failed (133 filtered) |
| runtime checker | `python3 scripts/check-pnm-runtime.py` | `ok: 215 files, manifest matches, script refs exist, no .gz` |
| theme checker | `python3 scripts/check-pnm-theme.py` | `ok: meta == dark --c-bg, light token present, toggle uses computed token, manifest #17212B` |

Сборка и clippy выполнены из pristine-архива; незакоммиченные файлы живого дерева (T04b-правки других агентов) не подкладывались.

## §2. Стенд: эфемерные серверы и disk-фикстура

```bash
VKS_CONFIG_PATH=<pristine>/configs/default.toml
VKS_LISTEN_ADDR=127.0.0.1:18473   # затем 127.0.0.1:18475 после импорта
VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0
VKS_DATABASE_URL="sqlite:///dev/shm/…/run/{a,b}/{a,b}.db?mode=rwc"
VKS_FRONTEND_DIR=<pristine>/client/pnm-frontend/deployed/pnm
VKS_WEBRTC_TRANSPORT=0 VKS_SIP_UAS_ENABLED=0 VKS_H323_UAS_ENABLED=0
VKS_CANDIDATE_ADDR=127.0.0.1 VKS_ALLOW_INSECURE_DEFAULTS=true
```

`healthz`/`/pnm/` → 200; лог `vks-server ready and listening … addr=127.0.0.1:18473`, затем `:18475`. Живые `:8080`/`:18082` не перезапускались и не нагружались; SIP/H.323-листенеры не поднимались.

**Disk-full фикстура (честно).** Корень хоста заполнен на **96 %** (`df`), `/api/v3/ui/system/status` отдаёт `disk.full="WARNING"/"CRITICAL"`. Официальный `tests/e2e/deeplink_auth.js` на этом хосте дал **5/6 + SecurityError**: после логина вендорский SPA уходит с сессии (`about:blank`, пустой `document.body`), `sessionStorage` недоступен. Прямое воспроизведение без фикстуры: `finalUrl: "about:blank", title: "", body: ""`. Это не дефект T07-кода (review D, P3-6; окружение). Приёмка выполнена эквивалентным Playwright-прогоном, где route'ом подменялся **только** ответ `system/status` (`disk.full="NORMAL"`, `percent=42`); `deeplink.js`, серверный код и официальный тест не патчились. Все проверки §3–§6 — на этом стенде; официальный тест без фикстуры остаётся недетерминированным на заполненном хосте (P3-4).

## §3. Приёмка deep link: холодная ссылка → логин → та же сессия

Эквивалент `deeplink_auth.js`: **57/57 PASS** (полный прогон дважды; второй — после уточнения одного assert).

| # | Критерий | Результат |
|---|---|---|
| 1 | Cold deep link без токена → `/pnm/authorization` | **PASS** |
| 2 | Цель сохранена в `sessionStorage` | **PASS** `{"url":"…/sessions/eee2bd5d…","at":1793276338864}` |
| 3 | В JSON только `at,url`, токена нет | **PASS** (ключи ровно `at,url`; токен не встречается) |
| 4 | Логин через UI → тот же `/pnm/monitoring/sessions/<id>` | **PASS** |
| 5 | Сессия отрисована | **PASS** `{"card":1,"tabs":6,"body":true,"title":"PROSTOR"}` |
| 6 | Метка deep-link очищена | **PASS** (`null`) |
| 7 | Нет page errors / внешних запросов | **PASS** (`none`; только origin `127.0.0.1:18473`) |

## §4. Заявленный фикс: логин дольше 60 с

Cold deep link → authorization → поля заполнены → **пауза 66 000 мс** (старый poll-кап 60 000 мс) → submit.

```text
PASS slow: target saved before wait — {"url":"…/sessions/eee2bd5d…","at":1793276338864}
PASS slow: marker survives 66 s
PASS slow: same session after >60 s login — http://127.0.0.1:18473/pnm/monitoring/sessions/eee2bd5d…
PASS slow: session rendered and marker cleared — {"card":1,"tabs":6,"marker":null}
PASS slow: no page errors — none
```

`MAX_POLL_TRIES = (TTL_MS / 250) | 0 = 2400` (`deeplink.js:25-28`) переживает 60-секундный барьер. B-P2 закрыт.

## §5. Security / краевые случаи (57/57 PASS)

| Сценарий | Результат |
|---|---|
| **TTL 9 мин принят** (`at = now − 9 мин`, валидный токен) | **PASS**: возврат на целевую сессию, маркер удалён, ошибок нет |
| **TTL 11 мин отклонён** | **PASS**: цель не открыта (финал `/pnm/monitoring/sessions` — вендорский список по живому токену), маркер удалён |
| **Corrupted JSON** (`{broken`) | **PASS**: маркер удалён, возврата на цель нет, page errors нет |
| **`//evil.example/…`** | **PASS**: не перешёл, внешнего запроса нет, маркер удалён |
| **`https://evil.example/…`** | **PASS**: то же |
| **Уже авторизованный deep link** | **PASS**: остаётся на `/sessions/<id>`, сессия рендерится, маркера нет |
| **Протухший токен** | **PASS**: финал `/pnm/authorization`, маркер удалён, навигаций 5, loop нет; 10 vendor-объектов в `pageerror` — ожидаемые 401 SPA, не ошибка T07 |
| **Токена в saved JSON нет** | **PASS** (см. §3) |

## §6. Тема: инвариант `meta[theme-color] == computed --c-bg`

| Проверка | Результат |
|---|---|
| light: meta == computed | **PASS** `{"meta":"#f8f8f8","bg":"#f8f8f8","cls":"theme-light"}` |
| dark: meta == computed | **PASS** `{"meta":"#0e1621","bg":"#0e1621","cls":"theme-dark"}` |
| ровно один класс темы | **PASS** (одновременно `theme-light` и `theme-dark` не наблюдались) |
| toggle light → dark | **PASS** meta/bg `#0e1621`, `VKS_PNM_THEME=dark` |
| toggle dark → light | **PASS** meta/bg `#f8f8f8`, stored `light` |
| toggle sync + reload | **PASS** после reload `#0e1621`/`#0e1621`/`theme-dark`/stored `dark` |
| статический `meta theme-color` | **PASS** `#0e1621` (43 B-блок) |
| `manifest.webmanifest theme_color` | **PASS** `#17212B` |

## §7. Импорт: целостность, повторяемость, TMP, TERM

### 7.1. Два `--force` — оба exit 0

```bash
TMPDIR=/dev/shm/…/tmp VKS_PNM_ARCHIVE="…/PNM-Frontend-20261016.zip" \
  ./scripts/import-pnm-frontend.sh --force   # дважды
```

| Проверка | Прогон 1 | Прогон 2 |
|---|---|---|
| rc | **0** | **0** |
| файлов в `deployed/pnm` | **215** | **215** |
| строк `MANIFEST.sha256` | **215** | **215** |
| `sha256sum -c ../../MANIFEST.sha256` | **215 OK / 0 FAILED**, exit 0 | **215 OK / 0 FAILED**, exit 0 |
| `.gz` в рантайме | **0** | **0** |
| TMP (`pnm-import.*`) после успеха | **0**, каталог пуст | **0**, каталог пуст |
| `.pnm-stage`/`.pnm-prev` | отсутствуют | отсутствуют |

### 7.2. Проектный слой байт-в-байт

| Файл/блок | До/после импорта |
|---|---|
| `deeplink.js` | **IDENTICAL**, sha256 `7ee097f88cea…ae6de4a` |
| `theme-toggle.js` | **IDENTICAL**, sha256 `dc7faddf4590…3f303d45` |
| `perf-overrides.js`, `api-errors.js`, `codian-layouts.js` | **IDENTICAL** |
| project `<script>` block (`prostor:project-layer:start/end`) | 306 B, **identical** |
| `<style id="prostor-pnm-overrides">` | 11 198 B, **identical** |
| статический `meta theme-color` | 43 B, **identical** |
| `scripts/check-pnm-runtime.py` / `check-pnm-theme.py` после импорта | оба **ok** |

### 7.3. Восстановление при TERM (трапы активны)

Fault-injection в **копии** скрипта: после установки сигнальных трапов (`import-pnm-frontend.sh:237-241`) добавлен `sleep 8`; в `deployed/pnm` положен sentinel, сигнал отправлен в окне.

```text
TERM после "rvE fault window: traps active"
import rc=143
ИМПОРТ ОТМЕНЁН: прежний рантайм и манифест восстановлены
sentinel=RESTORED prev=absent stage=absent
files=216 manifest=216 gz=0
sha rc=0 ok=216 failed=0
tmp leftovers=0 entries=0
project layer identical=YES
```

После теста исходный скрипт восстановлен, sentinel удалён, финальный `--force` вернул **215/215**, sha exit 0, TMP 0. P3-5 ревью D (утечка TMP) закрыт: `cleanup_tmp` вызывается и на успехе (`:395`), и в `restore_previous_runtime` (`:228`).

### 7.4. Узкое окно сигнала до установки трапов (P3-2)

При искусственно расширенном окне **между** подменой дерева (`:214-217`) и установкой restore/TERM-трапов (`:237-241`) TERM получает default-действие: остаются `.pnm-prev` и сырой вендорский рантайм (наблюдалось 210 файлов, sentinel отсутствует, restore-сообщения нет). В обычном TERM-тесте (трапы активны) восстановление работает. Окно — несколько shell-инструкций (микросекунды), но это реальная остаточная хрупкость; не блокирует T07.

## §8. Runtime gzip и импортированный рантайм

Pristine и imported (`:18473` / `:18475`):

```text
GET /pnm/ (gzip)                              → 200, content-encoding: gzip, vary: Accept-Encoding (ровно 1), cache-control: no-cache
GET /mcu/chunks/index.bundle-2.2.1.66.js gzip → 200, content-encoding: gzip, vary: Accept-Encoding (ровно 1), cache-control: no-cache
GET /pnm/ (identity)                          → 200, без content-encoding, vary: Accept-Encoding
CSP + X-Content-Type-Options/X-Frame-Options/Referrer-Policy — присутствуют
```

После двух `--force` сервер перезапущен на `b.db` с импортированным `deployed/pnm`; прогон **12/12 PASS**:

```text
PASS imported: cold deep link -> auth
PASS imported: same session rendered, marker cleared — {"card":1,"tabs":6,"body":true,"marker":null}
PASS imported: no page errors
PASS imported: theme light/dark meta == --c-bg
PASS imported: toggle -> dark
PASS imported: static meta #0e1621
PASS imported: manifest theme_color #17212B
PASS imported: gzip + exactly one Vary value — enc=gzip vary=Accept-Encoding
```

## §9. T04b: `ui_system_status`, canonical codec lists и PNM-контракт

`3e2983c` в `shell.rs` **не** менял: canonical-правка `native_audio/native_video` → `PUBLIC_NATIVE_AUDIO/VIDEO` сделана в `8b47dfe`, а `3e2983c` — T04b SSOT в `main.rs`/`handler.rs`. Проверен итоговый рантайм `3e2983c`:

- `shell.rs:291-292` отдаёт `PUBLIC_NATIVE_AUDIO`/`PUBLIC_NATIVE_VIDEO` (`codec_capabilities.rs:23-25`).
- Ответ `GET /api/v3/ui/system/status` (200): `native_audio` = 11 канонических имён `PCMU,PCMA,G722,G723,G726-16,G726-24,G726-32,G726-40,G728,OPUS,G711`; `native_video` = `H264,H265`.
- Алиасов `G72616/G7231/HEVC` и дублей нет; `transcoder=false`; ключи контракта `{codecs,engine,database,network,disk,sip,h323,cluster,time}` не изменились; `disk.full` присутствует.
- В PNM-бандле нет обращений к `native_audio/native_video` (grep по `chunks/*.js` — пусто), deep-link/theme от этих полей не зависят.
- Live-браузер на этом ответе: page errors нет, deep link и тема — зелёные (§3–§6). **Влияния на PNM runtime-контракт не выявлено.**

## §10. Замечания (P0–P3, file:line)

### P0 — нет

### P1 — нет

`crates/vks-server/src/recorder.rs:136` использует alias `StoppedRecord` (`:535`); clippy exit 0. P1 ревью D закрыт.

### P2 — нет

### P3 — 4

1. **`scripts/import-pnm-frontend.sh:15-19`** — шапка «3.5 Восстанавливает ПРОЕКТНЫЙ слой» перечисляет только `perf-overrides.js`, `api-errors.js`, `codian-layouts.js` и CSS-блок; нет `theme-toggle.js`, `deeplink.js` и project `<script>`-блока. Реализация восстанавливает все пять файлов и оба блока (`:129-134`, `:254-300`); `NOTES.md:84-87` после `ca6c4b8` полон. Остаточная неточность именно в шапке скрипта (часть P3-2 ревью D).
2. **`scripts/import-pnm-frontend.sh:214-241`** — сигнальный зазор: между `mv DEST→PREV`/`mv STAGE→DEST` и установкой `trap restore_previous_runtime EXIT`/`trap … TERM HUP` TERM/INT не восстанавливают рантайм (искусственно расширенное окно: rc 143, `.pnm-prev` остался, сырое дерево без проектного слоя). В нормальном окне (трапы активны) TERM-restore проверен зелёным. Узкое pre-existing окно, не блокирует.
3. **`client/pnm-frontend/deployed/pnm/deeplink.js:25-28,74,116-137`** — `at` из будущего принимается (`Date.now() - saved.at` отрицательно) и не истекает; poll-окно считается от загрузки auth-страницы, а не от `at`, и `target` не перепроверяет TTL при возврате. Подтверждено исполнением: маркер `at = now + 1 ч` вернул на цель и был очищен. Open redirect нет (`DEEP_RE` якорит URL); риск — лишний bounce/мусор. Известный P3-4 ревью D, не исправлен.
4. **`tests/e2e/deeplink_auth.js:78-89`** — тест не учитывает `disk.full`: на хосте с 96 % корня вендор уводит на `about:blank`, `sessionStorage` даёт SecurityError, официальный прогон **5/6** (подтверждено; см. §2). Нужна фикстура `system/status` или skip при `disk.full != NORMAL`. Известный P3-6 ревью D; приёмка выполнена эквивалентом с фикстурой.

## §11. Docs truth (P3 ревью D)

| Утверждение | Факт | Итог |
|---|---|---|
| `deeplink.js` не заявляет покрытие 401 | grep `401` — пусто; `:11-14` честно: `pagehide` — **НЕ** страховка SPA-логаута; `:101-104` то же | **TRUE** (P3-1 D закрыт) |
| `NOTES.md` перечисляет весь проектный слой | `:84-87` — все пять JS + project script block + CSS override | **TRUE** (P3-2 D закрыт в NOTES) |
| ADR-0027 разграничивает 216 записей архива и 215 файлов рантайма | `:12` «216 записей архива / 215 файлов рантайма», `:14` «216/216 записей; в рантайм идут 215 файлов», `:47` «MANIFEST — 215 файлов» | **TRUE**, однозначно (P3-3 D закрыт) |
| `MANIFEST.sha256` — 215 строк и сходится | 215/215, `sha256sum -c` exit 0 | **TRUE** |
| `import-pnm-frontend.sh` не обещает `.gz`, `--gzip` игнорируется | `:386-388` только предупреждает; `.gz` = 0 | **TRUE** |

## §12. Гигиена

- Живая БД не открывалась и не менялась: `vks_data.db` — **3 248 128 B**, mtime `2026-10-29 12:38:31.642604976 +0300`, md5 **`a925e7fa52831ed3aa8480c0e9531ff0`** (до и после).
- Живые листенеры `:8080` (PID 3255206) и `:18082` (PID 3253281) — те же pre-existing процессы, не перезапускались.
- Эфемерные серверы `:18473`/`:18475` остановлены SIGTERM: `received SIGTERM, initiating shutdown` / `vks-server gracefully terminated`; порты свободны, scratch-процессов нет.
- Все scratch удалены после отчёта: pristine, run-БД, TMP, тестовые скрипты. `/tmp` для scratch не использовался; 3 каталога `/tmp/pnm-import.*` имеют mtime **13:05** (до начала ревью в 15:08, наследие старых прогонов до фикса `ca6c4b8`) — не мои; текущий импорт их не создаёт (TMPDIR был в `/dev/shm`).
- В живом дереве мною добавлен только этот файл; параллельные правки `crates/**`, `docs/**`, `tests/e2e/webrtc.js` — от других агентов, не мои.

## §13. Остаточные риски и ограничения

1. **Disk-фикстура** (§2): приёмка deep link выполнена с подменой только `disk.full/percent` в ответе `system/status`. Это не маскирует T07-логику: URL, `sessionStorage`, логин, карточка/вкладки и marker проверены реальным браузером; официальный тест на заполненном хосте недостоверен (P3-4).
2. **Узкое signal-окно импортёра** (P3-2): обычный TERM после установки трапов восстанавливает рантайм и чистит TMP; TERM в микроокне до установки трапов может оставить `.pnm-prev` и сырое дерево.
3. **TTL future-at** (P3-3): маркер с `at` в будущем не истекает; open redirect исключён, риск — лишний bounce.
4. **Импорт откатывает PROSTOR-ребренд** (title снова `TrueConf MCU`) — pre-existing, честно предупреждается импортёром (`NOTES.md:97-100`); не регрессия T07 и не входит в P0–P3.
5. **`VKS_WEBRTC_TRANSPORT=0`** отключал реальный WebRTC-транспорт; сервер поднимал legacy UDP media socket на эфемерном loopback-порту. На T07 не влияет.

---

**Итог:** T07 на `3e2983c` **принят**: гейты зелёные (build/clippy/fmt/frontend tests/checkers), cold deep link 57/57 (эквивалент официального теста с disk-фикстурой), логин после 66 с — та же сессия и очищенный маркер, security-края 57/57, тема light/dark + toggle + reload + static/manifest, импорт дважды 215/215 + `sha256sum -c` + байт-идентичный проектный слой + 0 `.gz` + TMP 0, TERM-restore 216/216 и снова 215/215, runtime gzip + ровно один `Vary`, импортированный рантайм 12/12, T04b canonical lists контракт PNM не ломают. **0 P0/P1, 4 P3 → 9.5/10, done: yes.**
