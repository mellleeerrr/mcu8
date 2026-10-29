# T07 — независимое re-ревью D (frontend): deep link, тема, импорт, runtime gzip (commit `f602f72`)

- **Дата/время:** 2026-10-29, 14:27–14:45 MSK.
- **Ревьюер:** независимый delegated frontend-ревьюер. Production-исходники не менялись; в живом дереве записан только этот файл.
- **Объект:** commit `f602f72` («fix(T09b): finalize on encoder group loss, replay finalize errors, surface tap writer errors»), родитель `a02daad`. T07-файлы в коммите не менялись относительно `fff51a7` (проверено по blob-hash).
- **Pristine-копия:** `git archive f602f72 | tar -x -C /dev/shm/t07d_…` (55 MB); `/tmp` для scratch не использовался. Node **v24.21.0**, Playwright **1.64.1**/Chromium из live `tests/e2e/node_modules` (read-only). Архив PNM читался из live-репо как read-only файл.
- **Предыдущие ревью:** A — 9.3/done:yes; B — 9.0/done:yes (P2 медленного логина); C — 8.5/done:no (P1: `fff51a7` не собирался из чистого архива).
- **Проверка без shims:** pristine `cargo build -p vks-server --offline` — **exit 0**, без копирования незакоммиченных T04b-файлов и без подмены исходников. T07-статика и import-скрипт — ровно из `f602f72`.

## Вердикт

**9.2 / 10 — done: no. 0 P0, 1 P1, 0 P2, 6 P3.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** |
| **P2** | **0** |
| **P3** | **6** |

**T07-функционал принят по существу на 9.5:** приёмка deep link, фикс логина >60 с, security-края, тема, импорт 215/215, байт-идентичный проектный слой, runtime gzip, импортированный рантайм и TERM/INT-восстановление импорта проверены исполнением. **`done: no` — из-за одного P1, не относящегося к T07:** commit `f602f72` не проходит обязательный clippy-гейт (`crates/vks-server/src/recorder.rs:136`, `clippy::type_complexity`, exit 101). Это T09b-часть коммита; CI-workflow `clippy` (`cargo clippy --workspace --all-features --all-targets --locked -- -D warnings`) с таким деревом красный. После исправления этого одного линта и P3-документации T07 сертифицируется как `done: yes` на 9.5.

---

## §1. Стенд: чистая сборка и эфемерный сервер

### 1.1. Pristine и сборка

```text
$ git -C "/home/pro/Рабочий стол/mcu" archive f602f72 | tar -x -C /dev/shm/t07d_…
$ CARGO_TARGET_DIR=/dev/shm/t07d_target cargo build -p vks-server --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 36s   # exit 0
```

Сборка прошла **без shims**: в pristine-дереве нет незакоммиченных файлов, `codec_spec.rs`/`codec_capabilities.rs` не подкладывались, `VKS_FRONTEND_DIR` указывал на статику из этого же архива. Это прямо закрывает причину `done: no` в ревью C (P1 build-break `fff51a7`).

### 1.2. Эфемерный сервер

```bash
VKS_CONFIG_PATH=<pristine>/configs/default.toml
VKS_LISTEN_ADDR=127.0.0.1:18457
VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0
VKS_DATABASE_URL="sqlite:///dev/shm/t07d_run/a.db?mode=rwc"
VKS_FRONTEND_DIR=<pristine>/client/pnm-frontend/deployed/pnm
VKS_WEBRTC_TRANSPORT=0 VKS_SIP_UAS_ENABLED=0 VKS_H323_UAS_ENABLED=0
VKS_CANDIDATE_ADDR=127.0.0.1 VKS_ALLOW_INSECURE_DEFAULTS=true
```

`healthz`/`/pnm/` → 200; в логе `vks-server ready and listening … addr=127.0.0.1:18457`. Сервер слушал только loopback, SIP/H.323-листенеры не поднимал (живые `:5060`/`:1720` — pre-existing процессы, не мои). Legacy UDP media socket получил эфемерный `127.0.0.1:48187`; внешних/UDP-портов live-стенда это не касалось. После импорта сервер перезапускался на том же эфемерном порту с рантаймом `deployed/pnm` после двух `--force` (БД `b.db`), чтобы CSP/индекс читались заново.

### 1.3. Официальный e2e и детерминированная приёмка

Официальный `tests/e2e/deeplink_auth.js` на этом хосте дал **5/6 + SecurityError**: корневая ФС занята на 95 %, `/api/v3/ui/system/status` отдаёт `disk.full="CRITICAL"`, и вендорский SPA через ~1.5–2 с уводит страницу на `/pnm/no_disk_space` → `about:blank`; `sessionStorage` после этого недоступен. Это не дефект T07-кода и не регрессия (в ревью C тот же эффект при 90 %). Для приёмки использован **эквивалентный** прогон с единственной стендовой фикстурой: ответ `system/status` заменялся на `disk.full="NORMAL"` (percent 42) через Playwright route; ни `deeplink.js`, ни серверный код, ни тест не патчились. На этом стенде cold-URL, логин и рендер сессии проверены полностью; официальный тест без фикстуры остаётся недетерминированным — это P3-6.

---

## §2. Приёмка deep link: холодная ссылка → логин → та же сессия

Эквивалентный прогон: **35/35 PASS**, в том числе приёмочные пункты:

| # | Критерий | Результат |
|---|---|---|
| 1 | Cold deep link без токена → `/pnm/authorization` | **PASS** |
| 2 | Цель сохранена в `sessionStorage` (`vks_deeplink_return`) | **PASS** |
| 3 | В JSON только `{url, at}`, токена нет | **PASS** (ключи ровно `at,url`; токен не встречается) |
| 4 | Логин через UI → тот же `/pnm/monitoring/sessions/<id>` | **PASS** |
| 5 | Сессия отрисована (карточка/вкладки/имя) | **PASS** `card=1, tabs=6, body=true` |
| 6 | Метка deep-link очищена | **PASS** (`null`) |
| 7 | Нет page errors / внешних запросов | **PASS** (`none`; только origin `127.0.0.1:18457`) |

Фрагмент лога:

```text
PASS cold deep link -> authorization — http://127.0.0.1:18457/pnm/authorization
PASS target saved with numeric at — {"url":"/pnm/monitoring/sessions/…","at":1793273672329}
PASS saved JSON keys exactly url,at and no token — at,url
PASS after UI login same session URL — http://127.0.0.1:18457/pnm/monitoring/sessions/…
PASS session rendered after 2.5s (card/tabs/name) — {"card":1,"tabs":6,"body":true,"bothThemes":false}
PASS marker cleared after return — null
PASS no page errors (cold) — none
PASS no external requests (cold) — http://127.0.0.1:18457
```

Скриншот страницы сессии после холодного логина (dark, 1280×720) визуально подтверждает карточку сессии, 6 вкладок и имя комнаты. Официальный тест на нефикстуренном стенде дал те же URL/маркер, но не смог проверить рендер из-за disk-редиректа (см. §1.3 и P3-6).

---

## §3. Заявленный фикс: логин дольше 60 с

Сценарий: cold deep link → authorization → поля логина заполнены → **пауза 68 000 мс** (старый poll-кап 60 000 мс) → submit. Полный прогон: **10/10 PASS**, elapsed **75 379 мс**.

```text
PASS cold deep link -> authorization
PASS target saved before wait — {"url":"/pnm/monitoring/sessions/2efd…","at":1793273718793}
waiting 68000 ms before submit (old poll cap 60000 ms)
PASS marker survives 68 s wait — {"url":"…/sessions/2efd…","at":1793273718793}
PASS login after >60 s returns to the SAME session — http://127.0.0.1:18457/pnm/monitoring/sessions/2efd…
PASS session rendered (card/tabs/name) — {"card":1,"tabs":6,"body":true,"marker":null}
PASS marker cleared after slow return — null
PASS no page errors — none
SUMMARY 10 pass, 0 fail
```

Это прямое доказательство, что `MAX_POLL_TRIES = TTL_MS/250 = 2400` (`deeplink.js:28,125-137`) переживает 60-секундный барьер и возвращает на исходную сессию. B-P2 закрыт. Замечание о том, что poll-окно считается от загрузки страницы, а не от `at` метки, остаётся P3-4 (не влияет на приёмку).

---

## §4. Security / краевые случаи

Тот же прогон: **35/35 PASS**. Проверено:

- **TTL 9 мин принят:** маркер `at = now − 9 мин` + валидный токен → возврат на ту же сессию, маркер удалён.
- **TTL 11 мин отклонён:** возврата на целевую сессию нет; финальный URL — `/pnm/monitoring` (вендорский редирект по живому токену), не `/sessions/<id>`; маркер удалён. Это корректный отказ, а не «остался на auth любой ценой».
- **Corrupted JSON:** `{broken` → остаётся authorization, маркер удалён, page errors нет.
- **`//evil.example/...`:** отклонён, внешнего запроса нет, маркер удалён.
- **`https://evil.example/...`:** отклонён, внешнего запроса нет, маркер удалён.
- **Уже авторизованный deep link:** остаётся на `/pnm/monitoring/sessions/<id>`, сессия рендерится, маркера нет.
- **Протухший токен:** заканчивает на `/pnm/authorization`, без loop (`navs 3 → 3`), маркера нет. Вендорские 401-объекты в pageerror здесь ожидаемы; это не ошибка T07-скрипта.
- **Токена в saved JSON нет:** ключи ровно `at,url`, значение токена не встречается.

Кодовые P3 вне приёмки: `deeplink.js:25-28,116-137` — poll ограничен 600 с от загрузки auth-страницы, а `target` захвачен замыканием и TTL повторно не проверяется; `deeplink.js:74` принимает `at` из будущего (`Date.now() - saved.at` отрицательно), поэтому такой маркер не истекает. Open redirect нет (URL якорится `DEEP_RE`), риск — только лишний bounce/мусор.

---

## §5. Тема: инвариант `meta[theme-color] == computed --c-bg`

**14/14 PASS** на pristine-рантайме и **14/14 PASS** после двух импортов (Playwright, fresh context, `colorScheme: light|dark`, ключ `VKS_PNM_THEME`):

```text
theme light: meta == computed — {"meta":"#f8f8f8","bg":"#f8f8f8","cls":"theme-light"}
theme dark:  meta == computed — {"meta":"#0e1621","bg":"#0e1621","cls":"theme-dark"}
toggle: light -> dark — meta=#0e1621, bg=#0e1621, stored=dark
toggle: dark -> light — meta=#f8f8f8, bg=#f8f8f8, stored=light
never both classes — ["theme-light","theme-dark","theme-light"]
static meta theme-color = #0e1621
manifest theme_color = #17212B
```

Инвариант держится уже на `DOMContentLoaded`; одновременно `theme-light` и `theme-dark` не наблюдались. Проверены `scripts/check-pnm-runtime.py` и `scripts/check-pnm-theme.py` — оба `ok`. Кнопка toggle монтируется с небольшой задержкой (на auth-странице до ~1.5 с); после settle работает и сохраняет `VKS_PNM_THEME`.

---

## §6. Импорт: целостность, повторяемость, сигналы, gzip

### 6.1. Два `--force` — оба exit 0

```bash
TMPDIR=/dev/shm/t07d_tmp VKS_PNM_ARCHIVE="…/PNM-Frontend-20261016.zip" \
  ./scripts/import-pnm-frontend.sh --force    # дважды, оба exit 0
```

| Проверка | Результат |
|---|---|
| файлов в `deployed/pnm` | **215** |
| строк `MANIFEST.sha256` | **215** |
| `sha256sum -c ../../MANIFEST.sha256` | **215 OK, 0 FAILED, exit 0** |
| `.gz` в рантайме | **0** |
| `scripts/check-pnm-runtime.py` | `ok: 215 files, manifest matches, script refs exist, no .gz` |
| `scripts/check-pnm-theme.py` | `ok: meta == dark --c-bg, light token present, toggle uses computed token, manifest #17212B` |
| `node --check` пяти проектных JS | OK |

### 6.2. Проектный слой байт-в-байт

| Файл/блок | Результат |
|---|---|
| `deeplink.js` | **IDENTICAL**, sha256 `7e69446d…6cd386` |
| `theme-toggle.js` | **IDENTICAL**, sha256 `dc7faddf…3f303d45` |
| `perf-overrides.js`, `api-errors.js`, `codian-layouts.js` | **IDENTICAL** |
| project `<script>` block | 306 B, **identical=True** |
| `<style id="prostor-pnm-overrides">` | 11 198 B, **identical=True** |
| статический `meta theme-color` | 43 B, **identical=True** |

### 6.3. Runtime gzip + ровно один `Vary` (pristine и imported)

```text
GET /pnm/ (gzip)                              → 200, content-encoding: gzip, vary: Accept-Encoding, cache-control: no-cache
GET /mcu/chunks/index.bundle-2.2.1.66.js gzip → 200, content-encoding: gzip, vary: Accept-Encoding, cache-control: no-cache
GET /pnm/ (identity)                          → 200, без content-encoding, vary: Accept-Encoding
число строк Vary на /pnm/                     → 1
```

### 6.4. Импортированный рантайм: deep link + тема

После двух импортов сервер перезапущен на `b.db` с импортированным `deployed/pnm`:

- deep-link/security эквивалент: **35/35 PASS**, `card=1, tabs=6, marker=null`, page errors нет;
- тема: **14/14 PASS** (light/dark, toggle, static meta, manifest);
- gzip после импорта: `content-encoding: gzip`, `vary` ровно 1; `.gz` — 0.

### 6.5. Восстановление при TERM/INT

Fault-injection в **копии** дерева: в копию import-скрипта после установки трапов добавлен `sleep 20`, в `deployed/pnm` положен sentinel. Сигнал отправлялся в окно после swap (`.pnm-prev` уже создан).

- **TERM:** import rc **143**, `ИМПОРТ ОТМЕНЁН: прежний рантайм и манифест восстановлены`; `.pnm-prev` отсутствует, sentinel восстановлен, 216 файлов (215 + sentinel), манифест 216, `sha256sum -c` exit 0.
- **INT (реальный Ctrl-C в pty):** то же восстановление, rc 130, sentinel на месте, 216/216, sha exit 0.

Это подтверждает заявку `restore_previous_runtime` на обоих сигналах. P3-5: сам `TMP` всё ещё не удаляется на успехе/сигнале — после двух импортов в `TMPDIR` осталось два каталога `pnm-import.*` общим объёмом **143 MB** (trap `rm -rf "$TMP"` на `:71` перекрыт restore-трапом `:231` и снят на успехе `:372`). Pre-existing, но живёт в T07-скрипте.

---

## §7. Docs truth

| Утверждение | Факт | Итог |
|---|---|---|
| `import-pnm-frontend.sh:25-26` не обещает генерацию `.gz`; `--gzip` — legacy, игнорируется | реализация `:385-387` только предупреждает; `.gz` 0 | **TRUE** |
| `deeplink.js:11-14` честно говорит, что `pagehide` не страховка SPA-логаута | шапка честная | **TRUE** |
| В `deeplink.js` нет claim про покрытие 401 | `:101-104`: «pagehide — это страховка 401-разлогина» — прямое противоречие шапке и факту | **FALSE** (P3-1) |
| `NOTES.md` перечисляет весь проектный слой | `:84-87` — только `perf-overrides.js`, `api-errors.js`, CSS; нет `codian-layouts.js`, `theme-toggle.js`, `deeplink.js`; `import-pnm-frontend.sh:15-19` тоже неполный, хотя `:129-134` перечисляет все пять | **FALSE** (P3-2) |
| ADR-0027 разграничивает 216 записей архива и 215 файлов рантайма | `:12` — «216 файлов рантайма», `:14` — «весь deployed/pnm (архив 216/216)», `:47` — «MANIFEST — 215 файлов»; разграничение не сформулировано | **FALSE/ambiguous** (P3-3) |
| `docs/audit/2026-10-29/00_MASTER_PLAN.md:111,186` — 215/215 | фактически 215/215 | **TRUE** |

Остальные текущие числа (215) в NOTES/MASTER_PLAN сходятся; расхождения 212/214 остались только в исторических evidence/архивных отчётах.

---

## §8. Гейты

| Гейт | Команда | Результат |
|---|---|---|
| build | `cargo build -p vks-server --offline` | **PASS**, exit 0, 1m36s |
| frontend tests | `cargo test -p vks-server --lib frontend` | **PASS**, 20 passed / 0 failed |
| fmt | `cargo fmt --all -- --check` | **PASS**, exit 0 |
| clippy | `cargo clippy -p vks-server -- -D warnings` | **FAIL**, exit 101 |
| python checkers | `check-pnm-runtime.py`, `check-pnm-theme.py` | **PASS**, оба `ok` |

Точный вывод clippy:

```text
error: very complex type used. Consider factoring parts into `type` definitions
   --> crates/vks-server/src/recorder.rs:136:20
    |
136 |     stopped_cache: Arc<Mutex<HashMap<RoomId, (RecordingMeta, Option<String>)>>>,
    |                    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    = note: `-D clippy::type-complexity` implied by `-D warnings`
error: could not compile `vks-server` (lib) due to 1 previous error
```

Это **P1**: тип изменён именно в `f602f72` (в `a02daad` был простой `HashMap<RoomId, RecordingMeta>`); CI clippy-job (`.github/workflows/ci.yml:71`) с `-D warnings` будет красным. К T07-файлам отношения не имеет, но commit-level «зелёным» не является.

---

## §9. Замечания (P0–P3, file:line)

### P0

Нет.

### P1 — 1

1. **`crates/vks-server/src/recorder.rs:136`** — `stopped_cache: Arc<Mutex<HashMap<RoomId, (RecordingMeta, Option<String>)>>>` даёт `clippy::type_complexity`; `cargo clippy -p vks-server -- -D warnings` → exit 101. Тип введён в `f602f72` (T09b finalize-error cache); до коммита был простой тип. Нужен `type StoppedRecording = (RecordingMeta, Option<String>)` / alias, либо узкий `#[allow(clippy::type_complexity)]` с обоснованием. Это не T07, но блокирует CI/сертификацию коммита.

### P2

Нет. Предыдущий P2 (логин >60 с) закрыт и подтверждён §3. Pre-existing rebrand-rollback импортёра (после `--force` title снова `TrueConf MCU`, 91 файл ребренда откатывается) — известное ограничение, задокументированное в NOTES; в P2/P3 этого T07-ревью не включаю, так как оно не является регрессией T07.

### P3 — 6

1. **`client/pnm-frontend/deployed/pnm/deeplink.js:101-104`** — inline-комментарий называет `pagehide` «страховкой 401-разлогина», хотя шапка `:11-14` и фактическое поведение SPA-логаута это опровергают. Остаточный overclaim в docs truth.
2. **`client/pnm-frontend/NOTES.md:84-87`** (+ `scripts/import-pnm-frontend.sh:15-19`) — неполный список проектного слоя: нет `theme-toggle.js`, `deeplink.js` (в NOTES также нет `codian-layouts.js`), хотя импорт восстанавливает все пять (`:129-134`, лог импорта). Читатель документации может решить, что эти файлы не сохраняются.
3. **`docs/decisions/0027-pnm-legacy-frontend-adopt-and-shim.md:12,14` vs `:47`** — «216 файлов рантайма» / «архив 216/216» и «MANIFEST — 215 файлов» в одном ADR без явного разграничения «216 записей архива / 215 файлов рантайма».
4. **`client/pnm-frontend/deployed/pnm/deeplink.js:25-28,74,116-137`** — poll ограничен 600 с от загрузки auth-страницы, а не от `at`; `target` не перепроверяет TTL, поэтому маркер, принятый на 9:59, может вернуть на сессию и после 10 минут. `at` из будущего принимается и не истекает. Open redirect нет; риск — лишний bounce/мусор.
5. **`scripts/import-pnm-frontend.sh:70-71,231,372`** — `TMP` не удаляется: `trap 'rm -rf "$TMP"' EXIT` перекрыт restore-трапом и снят на успехе; после двух импортов осталось **143 MB** (2 × ~72 MB) в `TMPDIR`. Pre-existing; в живом дефолтном `/tmp` это тихая утечка на каждый импорт.
6. **`tests/e2e/deeplink_auth.js:78-89`** — проверка карточки через 1.5 с не фиксирует `disk.full`; на хосте с 95 % корня вендор уводит на `/pnm/no_disk_space` → `about:blank`, и официальный прогон даёт 5/6 + SecurityError. Нужна либо фикстура `system/status`, либо ожидание/скип при `disk.full != NORMAL`. Это не регрессия T07-кода, но тест недетерминирован на заполненном хосте.

---

## §9.1. Сводка по приёмке

| Блок | Результат |
|---|---|
| Cold deep link → auth → UI login → same session → card/tabs → marker cleared | **PASS** (эквивалент 35/35; official 5/6 из-за disk-артефакта) |
| >60 s логин (68 s) → same session, marker cleared, no page errors | **PASS** 10/10 |
| Security/edges (//evil, https, TTL 9/11, corrupt, already-auth, no loop, no token) | **PASS** 35/35 |
| Theme light/dark + static meta + manifest + toggle + checkers | **PASS** 14/14 + 14/14 (imported) |
| Import `--force` ×2, 215/215, sha, project layer, 0 `.gz` | **PASS** |
| Runtime gzip + ровно один `Vary` | **PASS** |
| TERM/INT restore | **PASS** (TERM и INT) |
| Docs truth | **частично FALSE** (P3-1..3) |
| Gates | build/test/fmt/checkers **PASS**; clippy **FAIL** (P1) |

---

## §10. Гигиена

- Живая БД не открывалась и не менялась: до и после — `vks_data.db`, **3 248 128 B**, mtime `2026-10-29 12:38:31.642604976 +0300`, md5 **`a925e7fa52831ed3aa8480c0e9531ff0`**.
- Живые листенеры `:8080` (PID 3255206) и `:18082` (PID 3253281) — те же pre-existing процессы; не перезапускались и не нагружались.
- Эфемерные серверы `:18457` остановлены SIGTERM: `received SIGTERM, initiating shutdown` / `vks-server gracefully terminated`; порт свободен, процессов `t07d_target/debug/vks-server` нет.
- Все scratch удалены: pristine, target (≈2.5 GB), `t07d_run*`, `t07d_tmp` (включая 143 MB утечки импортёра), baseline-layer, signal-test; `/tmp` для scratch не использовался. В живом дереве мною добавлен только этот файл; параллельные `crates/**`-правки — от других агентов, не мои.

---

## §11. Остаточные риски и ограничения

1. **P1 clippy** — единственная причина `done: no`; к T07-файлам отношения не имеет, но делает `f602f72` не CI-green. После type-alias/allow и перепроверки `cargo clippy -p vks-server -- -D warnings` T07 закрывается на 9.5.
2. **Приёмка deep link выполнена с одной стендовой фикстурой `disk.full=NORMAL`** (§1.3): иначе корневой диск 95 % уводит вендорский SPA на `/pnm/no_disk_space`. Это не маскирует T07-логику: URL, sessionStorage, логин, карточка/вкладки и marker проверены реальным браузером; official-тест без фикстуры на этом хосте недетерминирован (P3-6).
3. **`VKS_WEBRTC_TRANSPORT=0`** отключал реальный WebRTC-транспорт для SDP, но сервер всё равно поднял legacy UDP media socket на эфемерном loopback-порту; SIP/H.323-листенеры не поднимались. На T07 не влияет.
4. **После импорта откатывается PROSTOR-ребренд** (title снова `TrueConf MCU`, 91 файл) — pre-existing, честно предупреждается импортёром (`NOTES.md:97-100`); зелёные манифест/чекеры этого не ловят. Не является регрессией T07.
5. **P3-4 (TTL future/poll)** — не open redirect, но маркер может жить дольше 10 минут; исправление — проверять TTL по `at` в poll и отклонять `at > now`.

---

**Итог:** T07-функционал `f602f72` **принят**: cold deep link 35/35 (эквивалент), >60 s фикс 10/10, security 35/35, тема 14/14 на обоих рантаймах, импорт 215/215 + байт-идентичный проектный слой + 0 `.gz`, gzip/один `Vary`, TERM/INT restore, imported runtime probes. Чистая сборка теперь проходит — P1 ревью C закрыт. Но commit-level **done: no** из-за нового **P1 clippy** в T09b (`recorder.rs:136`); T07-часть — **9.5**, общая оценка **9.2/10**.
