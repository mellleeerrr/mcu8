# T07 — независимое re-ревью C (frontend): deep link, тема, импорт, runtime gzip, docs (commit `fff51a7`)

- **Дата/время:** 2026-10-29, 13:47–14:12 MSK.
- **Ревьюер:** независимый delegated frontend-ревьюер. Production-исходники не менялись; в живом дереве записан только этот файл.
- **Объект:** commit `fff51a7` («fix(T06,T07): mode capacity declared only by size/preset; load-time prune; deeplink TTL poll; import signal restore»), родитель `42922b7`, следующий `8e4197f` (T09b) — **не** рассматривался.
- **Pristine-копия:** `git archive fff51a7 | tar -x -C /dev/shm/t07c` (54 MB); `/tmp` не использовался, `TMPDIR=/dev/shm/t07c-tmp`. Node **v24.21.0**, Playwright **1.64.1**/Chromium из live `tests/e2e/node_modules` (read-only).
- **Предыдущие ревью:** A — 9.3/done:yes, B — 9.0/done:yes (P2: медленный логин >60 с терял deep link).

## Вердикт

**8.5 / 10 — done: no. 0 P0, 1 P1, 0 P2, 7 P3.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** |
| **P2** | **0** |
| **P3** | **7** |

`done: no` — из-за единственного **P1, не относящегося к T07-файлам**: коммит `fff51a7` **не собирается из чистого архива**. `crates/vks-admin/src/api_v3/shell.rs:577,605` (T06-часть коммита) вызывает `vks_common::codec_capabilities::{audio,video}_unavailable_reason`, которых в коммите нет — они существуют только в незакоммиченных правках рабочего дерева (`codec_capabilities.rs` M + untracked `codec_spec.rs`). `cargo check -p vks-admin` на pristine → **exit 101**; ancestor `25c9d99` собирается (exit 0). Это блокер CI/воспроизводимости коммита (см. §1.2 и P1 в §8).

**T07-функционал при этом принят полностью** (по существу T07-части — 9.5): приёмка deep link пройдена официальным e2e **7/7** на pristine-рантайме и стабильным ручным прогоном **8/8**; заявленный P2-фикс (логин >60 с) **воспроизведён и подтверждён реальным браузером** (пауза 68 с → возврат на ту же сессию, 9/9); импорт дважды даёт **215/215**, `sha256sum -c` 215 OK, 0 `.gz`, проектный слой байт-в-байт; тема **18/18** на обоих рантаймах; security-края **27/27**; TERM/INT-восстановление импорта проверено исполнением. Если P1 закрыть отдельным коммитом T06 (внести недостающие функции в `codec_capabilities.rs`/`codec_spec.rs`), T07 готов к `done: yes` на 9.5.

---

## §1. Стенд: pristine-копия, сборка, эфемерный сервер

### 1.1. Pristine и окружение

```bash
git -C "/home/pro/Рабочий стол/mcu" archive fff51a7 | tar -x -C /dev/shm/t07c
```

Node v24.21.0; Playwright запускался через `NODE_PATH="/home/pro/Рабочий стол/mcu/tests/e2e/node_modules"` (live-дерево, read-only). Архив PNM `PNM-Frontend-20261016.zip` (sha256 `ea0314f8…cbdfa`) в git не входит — читался из live-репо как read-only файл для импорта.

### 1.2. P1: чистая сборка коммита падает

```text
$ CARGO_TARGET_DIR=/dev/shm/t07c-target cargo build -p vks-server --offline
error[E0425]: cannot find function `audio_unavailable_reason` in module `vks_common::codec_capabilities`
   --> crates/vks-admin/src/api_v3/shell.rs:577:58
error[E0425]: cannot find function `video_unavailable_reason` in module `vks_common::codec_capabilities`
   --> crates/vks-admin/src/api_v3/shell.rs:605:59
error: could not compile `vks-admin` (lib) due to 2 previous errors
# exit 101 (cargo check -p vks-admin, тот же результат)
```

Проверено по коммиту: `git show fff51a7 -- crates/vks-admin/src/api_v3/shell.rs` добавляет **обе** строки (`+`), а `git show --stat fff51a7 -- crates/vks-common/src/codec_capabilities.rs` — **пуст**; в `25c9d99` и в фактическом родителе `42922b7` этих вызовов нет; ancestor `25c9d99` собирается:

```text
$ cd /dev/shm/t07c-prev && cargo build -p vks-server --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 49.84s   # exit 0
```

То есть коммит зависит от незакоммиченных T04b-правок (`codec_capabilities.rs` M, `codec_spec.rs` untracked), которые есть в рабочем дереве, но не в `fff51a7`. Это T06-часть коммита; T07-файлы (`deeplink.js`, `import-pnm-frontend.sh`) к ошибке отношения не имеют.

### 1.3. Как всё-таки проверялся T07 (и почему это валидно)

Сервер собран из ancestor-коммита `25c9d99` (`CARGO_TARGET_DIR=/dev/shm/t07c-target`), а фронтенд-рантайм подавался **из pristine `fff51a7`** через `VKS_FRONTEND_DIR`. Между `25c9d99` и `fff51a7` нет изменений в `crates/vks-server/**` (в т.ч. `frontend.rs` — ServeDir/CSP/CompressionLayer/Cache-Control) и в статике, кроме `deeplink.js` + одной строки манифеста; diff коммита — только `vks-admin` (T06), T07-файлы и docs. Поэтому поведение статики/логина/API, релевантное T07, идентично; отличие бинаря — только T06-код, который в T07 не участвует.

Эфемерный стенд:

```bash
VKS_LISTEN_ADDR=127.0.0.1:18431 \
VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0 \
VKS_DATABASE_URL="sqlite:///dev/shm/t07c-run/a.db?mode=rwc" \
VKS_FRONTEND_DIR=/dev/shm/t07c/client/pnm-frontend/deployed/pnm \
VKS_WEBRTC_TRANSPORT=0 VKS_SIP_UAS_ENABLED=0 VKS_H323_UAS_ENABLED=0 \
VKS_JWT_SECRET=dev_ec994ad4e03aa63eafc3131719a0a50cce4dbfb474c855b0 \
VKS_ADMIN_PASSWORD=admin_password VKS_ALLOW_INSECURE_DEFAULTS=true \
VKS_CANDIDATE_ADDR=127.0.0.1 /dev/shm/t07c-target/debug/vks-server
```

`healthz`/`/pnm/` → 200; в логе `vks-server ready and listening … addr=127.0.0.1:18431`. SIP/H.323-листенеры этим процессом не поднимались (`VKS_*_UAS_ENABLED=0`; `:5060`/`:1720` в системе — pre-existing live-процессы, не мои). После импорта сервер перезапускался на том же эфемерном порту с импортированным рантаймом (`b.db`), чтобы CSP/индекс читались заново.

---

## §2. Приёмка deep link: холодная ссылка → логин → та же сессия

### 2.1. Официальный `tests/e2e/deeplink_auth.js` — 7/7 на pristine

```text
[deeplink] OK   api login — 200
[deeplink] OK   conference создана — 200
[deeplink] OK   без токена уводит на авторизацию — http://127.0.0.1:18431/pnm/authorization
[deeplink] OK   целевой URL сохранён — {"url":"/pnm/monitoring/sessions/439ea751-…","at":1793271169362}
[deeplink] OK   после логина возврат на ту же сессию — http://127.0.0.1:18431/pnm/monitoring/sessions/439ea751-…
[deeplink] OK   сессия отрисована (карточка/вкладки/имя) — {"card":1,"tabs":6,"body":true}
[deeplink] OK   метка deep-link очищена — null
[deeplink] итог: 7/7 ok
```

### 2.2. Стабильное ручное воспроизведение (без гонки 1.5 с) — 8/8

Отдельный Playwright-прогон с проверкой **после 3 с** (официальный тест смотрит карточку через 1.5 с; на этом хосте см. §10):

```text
OK   cold deep link -> /pnm/authorization — http://127.0.0.1:18431/pnm/authorization
OK   target saved — {"url":"/pnm/monitoring/sessions/b529827e-…","at":1793271332789}
OK   after login same session URL — …/pnm/monitoring/sessions/b529827e-…
OK   same session URL stable after 3 s — /pnm/monitoring/sessions/b529827e-…
OK   session rendered stably (card/tabs/name) — {"url":"…/sessions/b529827e-…","card":1,"tabs":6,"body":true}
OK   marker cleared
SUMMARY 8 pass, 0 fail
```

Тот же прогон после двух импортов (импортированный рантайм) — **8/8**, `card=1,tabs=6,body=true`.

### 2.3. Таблица приёмки

| # | Критерий | Результат |
|---|---|---|
| 1 | Cold deep link без токена → `/pnm/authorization` | **PASS** (7/7 + 8/8) |
| 2 | Цель сохранена в `sessionStorage` (`vks_deeplink_return`) | **PASS** |
| 3 | Логин через UI → тот же `/pnm/monitoring/sessions/<id>` | **PASS** (обе ветки: pristine + imported) |
| 4 | Сессия отрисована (карточка/вкладки/имя) | **PASS** `card=1, tabs=6, body=true` |
| 5 | Метка очищена после возврата | **PASS** (`null`) |
| 6 | В сохранённом JSON только `{url, at}`, токена нет | **PASS** (ключи ровно `at,url`; токен не встречается) |

---

## §3. Новый P2-фикс: логин дольше 60 с

### 3.1. Реальное воспроизведение (68 000 мс на странице авторизации)

Сценарий: холодный deep link → authorization → поля логина заполнены сразу → **пауза 68 с** (старый poll-кап был 60 с: 240×250 мс) → submit. Полный лог:

```text
[slow] OK   api login — 200
[slow] OK   conference created — 200
[slow] OK   cold deep link -> /pnm/authorization — http://127.0.0.1:18431/pnm/authorization
[slow] OK   target saved in sessionStorage — {"url":"/pnm/monitoring/sessions/73bd3aef-…","at":1793271384014}
[slow] login page open; waiting 68000 ms before submit (old poll cap: 60000 ms)
[slow] OK   marker survives 68 s wait (TTL 10 min) — {"url":"…/sessions/73bd3aef-…","at":1793271384014}
[slow] OK   login after >60 s returns to the SAME session — http://127.0.0.1:18431/pnm/monitoring/sessions/73bd3aef-…
[slow] OK   session rendered (card/tabs/name) — {"card":1,"tabs":6,"body":true}
[slow] OK   marker cleared after slow return — null
[slow] OK   no page errors — none
[slow] elapsed ms 71877; navs=[…/sessions/73bd3aef-…, …/sessions/73bd3aef-…, …/sessions/73bd3aef-…]
[slow] total: 9/9 ok
```

Это прямое доказательство, что `MAX_POLL_TRIES = TTL_MS/250 = 2400` (`deeplink.js:28,125-137`) действительно переживает 60-секундный барьер и возвращает на исходную сессию. Заявленный B P2 закрыт.

### 3.2. Удаление испорченной/просроченной метки

```text
PASS corrupted JSON rejected (stays auth) — http://127.0.0.1:18431/pnm/authorization
PASS corrupted JSON marker removed — null
PASS corrupted JSON no page error — none
PASS expired TTL (11 min, no token) stays auth — http://127.0.0.1:18431/pnm/authorization
PASS expired TTL (11 min) marker removed
```

`recall()` (`deeplink.js:63-88`) теперь вызывает `removeItem` и на битом JSON/неверной форме (`:70-73`), и на истёкшем TTL (`:74-77`), и на невалидном URL (`:79-82`), и в `catch` через `forget()` (`:84-87`). Проверено с `addInitScript` (метка ставится до `deeplink.js`).

---

## §4. Security / краевые случаи (27/27 на pristine и 27/27 на imported)

```text
PASS cold: saved JSON has target and numeric at — {"url":"…/sessions/8ff648ae-…","at":1793271346211}
PASS cold: saved JSON keys exactly url,at and no token — at,url
PASS cold: no external requests — none
PASS TTL 9 min accepted (returns to session) — …/pnm/monitoring/sessions/8ff648ae-…
PASS TTL 9 min marker removed — null
PASS TTL 9 min session rendered — card=1
PASS TTL 11 min rejected (no return to session) — http://127.0.0.1:18431/pnm/monitoring/sessions
PASS TTL 11 min marker removed — null
PASS protocol-relative //evil rejected (no external request) — none
PASS protocol-relative //evil marker removed — null
PASS protocol-relative //evil same-origin final URL — …/pnm/monitoring/sessions
PASS absolute https://evil rejected (no external request) — none
PASS absolute https://evil marker removed — null
PASS absolute https://evil same-origin final URL — …/pnm/monitoring/sessions
PASS already authorized deep link stays — …/pnm/monitoring/sessions/d5010807-…
PASS already authorized session rendered — card=1
PASS already authorized no marker — null
PASS expired token ends on authorization — http://127.0.0.1:18431/pnm/authorization
PASS expired token no redirect loop — navs 3 -> 3
SUMMARY 27 pass, 0 fail
```

`DEEP_RE` (`deeplink.js:24`) якорит `/pnm/monitoring/sessions/`, `location.replace` не выходит за origin; `//evil` и `https://evil` отбрасываются, внешних запросов — 0, метка удаляется. TTL 9 мин принимается, 11 мин — отклоняется с удалением; уже авторизованный deep link остаётся на сессии; на протухшем токене — authorization без loop (3 навигации, затем стабильно).

---

## §5. Тема: инвариант `meta[theme-color] == computed --c-bg`

18/18 на pristine-рантайме и 18/18 после двух импортов (Playwright, `colorScheme: light|dark`, MutationObserver до page-скриптов):

```text
theme light at DOMContentLoaded — {"meta":"#f8f8f8","bg":"#f8f8f8","cls":"theme-light","violations":0}
theme dark  at DOMContentLoaded — {"meta":"#0e1621","bg":"#0e1621","cls":"theme-dark","violations":0}
PASS theme light: meta == computed — meta=#f8f8f8 computed=#f8f8f8
PASS theme dark:  meta == computed — meta=#0e1621 computed=#0e1621
PASS theme X: toggle updates class/meta/computed; persists VKS_PNM_THEME; toggles back
PASS theme X: never both classes — violations=0
PASS static meta theme-color = #0e1621
PASS manifest theme_color = #17212B
```

Значения: light `#f8f8f8`, dark `#0e1621`, статический `<meta name="theme-color">` `#0e1621`, `assets/manifest.webmanifest` `theme_color` `#17212B`. Инвариант держится уже на `DOMContentLoaded` (не только на `load`), одновременных `theme-light`+`theme-dark` наблюдатель не видел ни разу.

---

## §6. Импорт: целостность, повторяемость, сигналы, gzip

### 6.1. Два прогона `--force` — оба exit 0

```bash
TMPDIR=/dev/shm/t07c-tmp VKS_PNM_ARCHIVE="…/PNM-Frontend-20261016.zip" \
  ./scripts/import-pnm-frontend.sh --force    # дважды
```

```text
 восстановлено файлов: 482, пропущено: 1
 восстановлен: perf-overrides.js / api-errors.js / codian-layouts.js / theme-toggle.js / deeplink.js
 проектный слой <script> вставлен в index.html
 манифест пересобран: 215 строк
 meta theme-color: #0e1621; manifest theme_color: #17212B
 ссылки на скрипты: ok
 [import-pnm] готово.
```

Проверки после импорта:

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
| `deeplink.js` | sha256 `7e69446d…6cd386`; `cmp` с pristine `fff51a7` — **IDENTICAL** |
| `theme-toggle.js` | sha256 `dc7faddf…3f303d45`; **IDENTICAL** |
| `perf-overrides.js`, `api-errors.js`, `codian-layouts.js` | **IDENTICAL** |
| project `<script>` block | 306 B, `identical=True` |
| `<style id="prostor-pnm-overrides">` | 11 198 B, `identical=True` |
| статический `meta theme-color` | 43 B, `identical=True` |

Полное дерево после `--force` отличается от pristine-рантайма на **91 файл** (иконки/`manifest.webmanifest`/чанки/`index.html`: откат PROSTOR-ребренда) — это pre-existing, честно предупреждается самим скриптом (`NOTES.md:95-98`), не относится к T07-коду; оба чекера и `sha256sum -c` при этом зелёные.

### 6.3. Runtime gzip + ровно один `Vary` (после импорта)

```text
GET /pnm/ (gzip)                              → 200, content-encoding: gzip, vary: Accept-Encoding, cache-control: no-cache
GET /mcu/chunks/index.bundle-2.2.1.66.js gzip → 200, content-encoding: gzip, vary: Accept-Encoding, cache-control: no-cache
GET /pnm/ (identity)                          → 200, без content-encoding, vary: Accept-Encoding
число строк Vary на /pnm/                     → 1
```

### 6.4. Восстановление при TERM/INT (заявка коммита) — проверено

Fault-injection в **копии** дерева: в `scripts/import-sig.sh` (копия pristine-скрипта) после swap и установки трапов добавлен `sleep 30/60`, в `deployed/pnm` положен sentinel-файл; сигнал посылался в окно после swap (`.pnm-prev` уже создан).

TERM (kill в скрипт, ожидание завершения):

```text
signal=TERM exit=143
prev_dir=absent
sentinel=restored
files=216 manifest=216
sha_exit=0
restore_log_lines=1        # «ИМПОРТ ОТМЕНЁН: прежний рантайм и манифест восстановлены»
```

INT (реальный Ctrl-C в pty-терминале, `terminal_signal SIGINT` в foreground process group; в фоне SIGINT у bash игнорируется — это особенность неинтерактивного запуска, не скрипта):

```text
sentinel=restored
no .pnm-prev
files=216
ИМПОРТ ОТМЕНЁН: прежний рантайм и манифест восстановлены
manifest=216; sha_exit=0
```

Оба сигнала действительно восстанавливают прежний рантайм и манифест; `trap 'exit 130' INT` / `trap 'exit 143' TERM HUP` + `restore_previous_runtime` работают.

### 6.5. P3: утечка `TMP` — 72 MB на каждый импорт

`trap 'rm -rf "$TMP"' EXIT` (`scripts/import-pnm-frontend.sh:71`) перекрывается на `:231` (`trap restore_previous_runtime EXIT`), а на успехе `:372` (`trap - EXIT`) очищает трап вовсе — `$TMP` не удаляется никогда. После моих прогонов в `TMPDIR` осталось 7 каталогов `pnm-import.*` по ~72 MB (итого **500 MB**); на хосте с дефолтным `/tmp` это тихая утечка на каждый импорт. Дефект pre-existing (в `25c9d99` те же строки 71/230/367), но T07-скрипт его не чинит.

---

## §7. Docs truth

Что стало правдой и подтверждено:

- `scripts/import-pnm-frontend.sh:4-26` больше **не** обещает генерацию `.gz`: `--gzip` описан как legacy-флаг, который игнорируется (реализация `:385-387` — только предупреждение); `.gz` в рантайме 0, gzip делает `CompressionLayer`.
- `deeplink.js:1-16` — шапка честно говорит, что `pagehide` **не** страховка SPA-логаута и что cold deep link с протухшим токеном цель не сохраняет (принятое ограничение). Это подтверждено и поведением.
- Числа: `NOTES.md:27-33` — 215 файлов; `ADR-0027:47` — 215; `docs/audit/2026-10-29/00_MASTER_PLAN.md:111,186` — 215/215; фактически 215/215. Расхождения 212/214 остались только в исторических evidence/архивных отчётах (не текущая документация).

Осталось (P3):

- `deeplink.js:101-104`: «с токеном цель запоминается только на pagehide — **это страховка 401-разлогина**» — прямое противоречие шапке `:11-14` и факту. Это ровно тот overclaim, который коммит обещал убрать; шапку поправили, inline-комментарий — нет.
- `NOTES.md:84-87`: «Проектный слой — сохраняются `perf-overrides.js`, `api-errors.js` и блок CSS-override» — не упомянуты `codian-layouts.js`, `theme-toggle.js`, `deeplink.js`, которые импорт реально восстанавливает (лог: `восстановлен: theme-toggle.js`, `deeplink.js`). В `scripts/import-pnm-frontend.sh:15-19` список тоже неполный (`codian-layouts.js` есть, `theme-toggle.js`/`deeplink.js` нет), хотя `:129-134` перечисляет все пять.
- `ADR-0027:12,14` («216 файлов рантайма», «216/216») vs `:47` («MANIFEST — 215 файлов»): исторический счёт записей архива vs фактический манифест; в одном документе читается противоречиво.

---

## §8. Замечания (P0–P3, file:line)

### P1 — блокер коммита (не T07-файлы)

1. **`crates/vks-admin/src/api_v3/shell.rs:577,605`** — вызовы несуществующих `vks_common::codec_capabilities::{audio,video}_unavailable_reason`; в `crates/vks-common/src/codec_capabilities.rs` (коммит его не трогает) функций нет; pristine `cargo check/build` → exit 101, ancestor `25c9d99` собирается. Коммит не самодостаточен: зависит от незакоммиченных T04b-правок (`codec_capabilities.rs` M + untracked `codec_spec.rs`); у фактического родителя `42922b7` вызовов нет, ancestor `25c9d99` собирается (exit 0). Нужно либо внести эти функции в коммит, либо закоммитить зависимость до `fff51a7`; до этого CI/чистая сборка/приёмка «с коммита» невозможны.

### P2 — нет

Предыдущий P2 (медленный логин >60 с) **закрыт и подтверждён исполнением** (§3.1).

### P3

1. **`client/pnm-frontend/deployed/pnm/deeplink.js:101-104`** — `pagehide` назван «страховкой 401-разлогина», вопреки шапке `:11-14`; остаточный overclaim (docs truth).
2. **`client/pnm-frontend/NOTES.md:84-87`** (+ `scripts/import-pnm-frontend.sh:15-19`) — неполный список проектного слоя: нет `theme-toggle.js`, `deeplink.js` (и `codian-layouts.js` в NOTES); фактически импорт восстанавливает все пять.
3. **`docs/decisions/0027-pnm-legacy-frontend-adopt-and-shim.md:12,14` vs `:47`** — 216/216 записей архива и 215 файлов манифеста в одном ADR без явного разграничения.
4. **`deeplink.js:25-28,116-137`** — poll ограничен 600 с **от загрузки страницы авторизации**, а не от `at` метки; `target` захвачен в замыкание и TTL повторно не проверяется, поэтому метка, принятая на 9:59, может вернуть на сессию и через ~20 мин после сохранения. Комментарий «ограничен тем же TTL, что и метка» неточен.
5. **`deeplink.js:74`** — проверяется только `Date.now() - saved.at > TTL_MS`; `at` из будущего принимается и не истекает никогда (клиентская метка, риска open redirect нет — только мусор/лишний bounce).
6. **`scripts/import-pnm-frontend.sh:71,231,372`** — `TMP` не удаляется: `trap 'rm -rf "$TMP"' EXIT` перекрыт restore-трапом и снят на успехе; ~72 MB на импорт (в моих прогонах 7 × 72 MB = 500 MB). Pre-existing (в `25c9d99` — 71/230/367), но живёт в T07-скрипте.
7. **`tests/e2e/deeplink_auth.js:78-88`** — проверка карточки через 1.5 с хрупка на этом хосте: root FS занят на 90 %, `GET /api/v3/ui/system/status` отдаёт `disk.full="WARNING"`, и вендор-SPA через ~1.7 с уводит авторизованную страницу на `/pnm/no_disk_space` → `about:blank`. Официальный прогон на pristine дал 7/7, на импортированном рантайме (тот же код) — 5/6; стабильный ручной прогон с фиксацией `disk.full=NORMAL` — 8/8. Тест не фиксирует disk-статус и не является детерминированным на «заполненном» хосте (это не регрессия T07-кода).

---

## §9. Гигиена

- Живая БД не открывалась: до и после — `vks_data.db`, 3 248 128 B, mtime `2026-10-29 12:38:31.642604976 +0300`, md5 `a925e7fa52831ed3aa8480c0e9531ff0` (совпадает с записью review B).
- Живые листенеры `:8080` (PID 3255206) и `:18082` (PID 3253281) — те же pre-existing процессы; не перезапускались и не нагружались.
- Эфемерный сервер `:18431` получил SIGTERM: `received SIGTERM, initiating shutdown` / `vks-server gracefully terminated`; порт свободен, процессов `t07c*` нет.
- Удалены все scratch: `/dev/shm/t07c` (pristine), `t07c-prev`, `t07c-headrt`, `t07c-target` (2.5 GB), `t07c-run`, `t07c-probe`, `t07c-sig`, `t07c-tmp` (включая 500 MB утечки импортёра). `/tmp` для scratch не использовался (только внутренние профили Chromium Playwright).
- В живом дереве мной добавлен только `docs/audit/2026-10-29/reviews/T07_rereviewC.md`; параллельные правки `crates/**`, `Cargo.lock` — от других агентов, не мои.

---

## §10. Остаточные риски и ограничения

1. **P1 build-break** — единственная причина `done: no`; к T07-файлам отношения не имеет, но делает коммит непоставляемым из чистого checkout. Исправление: закоммитить `codec_capabilities`-функции (или зависимость T04b) и перепроверить `cargo build -p vks-server`.
2. **Проверка T07 выполнена бинарём `25c9d99` + рантаймом `fff51a7`** (см. §1.3): статический серверный код между коммитами идентичен, T07-статика — ровно из `fff51a7`; это ограничение зафиксировано, а не замаскировано.
3. **SPA-логаут с протухшим токеном** по-прежнему теряет deep link — документированное принятое ограничение T07 (`deeplink.js:11-14`), не входит в acceptance и не регрессировало.
4. **Диск хоста 90 %** → вендорский `/pnm/no_disk_space` на авторизованных страницах; на продуктовую логику T07 не влияет, но делает официальный e2e недетерминированным (P3-7) и мешает «стабильной» приёмке без фиксации disk-статуса.
5. Повторный импорт по-прежнему откатывает PROSTOR-ребренд (91 файл) — pre-existing, задокументировано в NOTES; зелёные гейты этого не ловят.

---

**Итог:** T07-часть `fff51a7` — принята: deep link (7/7 + 8/8), P2-фикс медленного логина (68 с, 9/9), удаление corrupt/expired меток, security-края (27/27), тема (18/18), импорт 215/215 + байт-идентичный проектный слой + gzip/один Vary/0 `.gz`, TERM/INT-восстановление импорта. Единственный блокер — **P1: коммит не собирается из чистого архива** (`shell.rs:577,605`) из-за незакоммиченной зависимости. По критерию «0 P0/P1» — **done: no, 8.5/10**; после устранения P1 T07 закрывается на **9.5**.
