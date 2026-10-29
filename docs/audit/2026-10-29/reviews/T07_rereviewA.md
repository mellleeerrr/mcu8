# T07 re-review A — backend/static serving (объект `25c9d99`)

- **Дата:** 2026-10-29, 13:17–13:40 MSK.
- **Ревьюер:** независимый delegated backend/static-serving reviewer. Production-исходники не менялись; в живом дереве записан только этот файл.
- **Объект:** коммит `25c9d99` («fix(T06,T07): declared capacity growth + empty AUTO windows; gzip metrics, atomic import, docs/tests»), ветка `master`. На момент `git archive` HEAD был `25c9d99`; к концу ревью живой HEAD уже был `42922b7`/выше — это чужие docs-коммиты и незакоммиченные правки других задач (T11c и др.), к объекту ревью отношения не имеют. Все выводы ниже — только про `25c9d99`.
- **Pristine-копия:** `git archive 25c9d99 | tar -x -C /dev/shm/t07rerevA_1793269057_433803/repo` (без `.git`/`target`). `CARGO_TARGET_DIR=/home/pro/.cache/t07rerevA_target`; Rust **1.94.0** (`rust-toolchain.toml`), все Cargo-команды `--offline`/`--locked`, где применимо. `/tmp` для копии/сборки не использовался.
- **Архив PNM:** `client/pnm-frontend/archive/PNM-Frontend-20261016.zip` не трекается, поэтому скрипт запускался с `VKS_PNM_ARCHIVE` на живой файл только на чтение; sha256 = `ea0314f85951a9abd9dcce3c9c6dfa79943941d36c03c64dde60a6ead23cbdfa` совпадает с якорем. Архив битый (нет central directory), поэтому импорт шёл документированным salvage-путём: восстановлено 482 файла, пропущен 1 (`source/.../index-legacy.js`).
- **Живая среда не тронута:** live `vks_data.db` не открывалась; размер/mtime/md5 до и после ревью совпали (`3 248 128 B`, `2026-10-29 12:38:31.642604976 +0300`, `a925e7fa52831ed3aa8480c0e9531ff0`). Live-порты `:8080` (PID 3255206) и `:18082` (PID 3253281) не перезапускались и не использовались. Ревью-серверы работали только на `127.0.0.1:49400` (HEAD-рантайм) и `:49401` (импортированный рантайм) со scratch-SQLite в `/dev/shm`; оба убиты, порты закрыты.

**Вердикт: 9.3 / 10 — done: yes. 0 P0, 0 P1, 1 P2, 7 P3.**

Ключевые заявки коммита подтверждены исполнением, а не только тестами: (P2-1) `vks_http_response_bytes_total` теперь растёт ровно на байты gzip-тела (и по-прежнему — на `Content-Length` identity-ответов); (P2-2) при провале после swap прежний рантайм **и** манифест восстанавливаются байт-в-байт (проверено двумя разными точками отказа). Импорт дважды (и третий раз с `--gzip`) даёт 215/215, 0 `.gz`, байт-идемпотентное дерево; проектный слой (`theme-toggle.js`, `deeplink.js`, override-блоки) сохраняется; оба чекера зелёные. Из открытого остаётся один известный P2 (pre-existing rebrand-гейт, честно задокументирован) и P3-хвост (устаревший заголовок `--gzip` в самом скрипте, атомарность при SIGINT/SIGTERM, first-match чекеров и т.п.).

---

## 1. Гейты (pristine `25c9d99`)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/home/pro/.cache/t07rerevA_target
cd /dev/shm/t07rerevA_1793269057_433803/repo
```

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all -- --check` | **0** | пусто |
| `cargo clippy -p vks-server --all-targets --locked --offline -- -D warnings` | **0** | без предупреждений |
| `cargo test -p vks-server --lib --locked --offline` | **0** | **147 passed, 0 failed, 3 ignored**; 20 тестов `frontend::tests`, включая `head_static_response_keeps_vary_and_is_safe`, `compressible_static_is_gzipped_at_runtime`, `tests::streaming_response_bytes_are_counted_without_content_length` |
| `cargo metadata --locked --offline --no-deps --format-version 1` | **0** | lock согласован |
| `cargo build -p vks-server --bin vks-server --locked --offline` | **0** | бинарь для живого матрикса |
| `python3 scripts/check-pnm-runtime.py` | **0** | `PNM runtime check ok: 215 files, manifest matches, script refs exist, no .gz` |
| `python3 scripts/check-pnm-theme.py` | **0** | `meta == dark --c-bg, light token present, toggle uses computed token, manifest #17212B` |
| `sha256sum -c ../../MANIFEST.sha256` (из `deployed/pnm`) | **0** | **215 OK, 0 FAILED** |

Три `ignored` — pre-existing ffmpeg/Python-зависимые тесты (`h323_gateway_negotiates_video_tx96_rx109...`, `ffmpeg_encoder_sps_respects_negotiated_level_30`, `ffmpeg_h264_rtp_roundtrip_preserves_color_and_stops_workers`), не связаны с T07.

## 2. Static gzip: живой матрикс

Сервер: debug-бинарь из pristine-сборки, `VKS_LISTEN_ADDR=127.0.0.1:49400`, `VKS_FRONTEND_DIR=<pristine>/client/pnm-frontend/deployed/pnm`, `VKS_DATABASE_URL=sqlite:///dev/shm/.../server/scratch.db?mode=rwc`, `VKS_WEBRTC_TRANSPORT=0`, `VKS_SIP_UAS_ENABLED=0`, `VKS_H323_UAS_ENABLED=0`, `VKS_MEDIA_LISTEN_ADDR=127.0.0.1:0`. Запросы — `curl -H 'Accept-Encoding: gzip' -D -`; тело сохранялось в файл.

| Запрос | Статус | `Content-Encoding` | `Content-Length` | `Vary` | Байт тела |
|---|---:|---|---:|---:|---:|
| `GET /pnm/` | 200 | **gzip** | нет (chunked) | 1 × `Accept-Encoding` | 15 770 |
| `GET /pnm/` identity | 200 | — | 75 463 | 1 | 75 463 |
| `GET /pnm/authorization` (SPA fallback) | 200 | **gzip** | нет | 1 | 15 770 |
| `GET /mcu/chunks/index.bundle-2.2.1.66.js` | 200 | **gzip** | нет | 1 | 347 698 |
| то же identity | 200 | — | 1 267 133 | 1 | 1 267 133 |
| `GET /mcu/chunks/index.bundle-2.2.1.66.css` | 200 | **gzip** | нет | 1 | 53 049 (identity 581 227) |
| `GET /mcu/assets/images/q48DLrUJ.svg` | 200 | **gzip** | нет | 1 | 20 772 (identity 110 645) |
| `GET /mcu/assets/images/prostor-wordmark.svg` | 200 | **gzip** | нет | 1 | 60 463 (identity 88 734) |
| `GET /mcu/assets/manifest.webmanifest` (`application/manifest+json`) | 200 | **gzip** | нет | 1 | 351 (identity 1 556) |
| `GET /mcu/chunks/sleep.bundle-2.2.1.66.js` (71 B) | 200 | — | 71 | 1 | 71 |
| `GET /mcu/assets/yandex-browser-manifest.json` (160 B) | 200 | — | 160 | 1 | 160 |
| `GET /mcu/assets/browserconfig.xml` (418 B) | 200 | — | 418 | 1 | 418 |
| `GET /mcu/assets/android-chrome-144x144.png` | 200 | — | 14 106 | 1 | 14 106 |
| `GET /mcu/chunks/index.bundle….js` + `Range: bytes=0-9` | 206 | — | 10 | 1 | 10; `Content-Range: bytes 0-9/1267133` |
| `GET /mcu/assets/auto_scroll8oj5ttnk.webm` + `Range: bytes=0-9` | 206 | — | 10 | 1 | 10 |
| `GET /pnm/authorization` + `Range: bytes=0-99` | 206 | — | 100 | 1 | 100; `bytes 0-99/75463` |
| `HEAD /mcu/chunks/index.bundle….js` + gzip | 200 | **gzip** | нет | 1 | тела нет (hyper отбрасывает) |
| `GET /mcu/chunks/index.bundle….js` + свежий `If-Modified-Since` | 304 | — | нет | 1 | 0 |
| `GET /mcu/chunks/nope.js` | 404 | — | 0 | 1 | 0 |
| `GET /pnm` | 308 → `/pnm/` | — | 0 | 1 | 0 |
| `GET /metrics` | 200 | — | 6 322 | **0** | 6 322 |
| `GET /api/v1/rooms` | 401 | — | 79 | **0** | 79 |
| `GET /healthz` | 200 | — | 14 | **0** | 14 |

Проверки целостности:

- gzip-тела — настоящий RFC 1952 (`gzip -t` OK, магия `1f 8b`), а `gzip -dc` байт-в-байт равен identity-ответу для `/pnm/`, 1.27 MB JS, 581 KB CSS и SVG (`cmp` — identical). SPA-fallback декодируется в тот же `index.html`, что и `/pnm/`.
- `Vary` ровно один на всех static-ответах (200/206/304/308/404/HEAD) и **ноль** на `/metrics`, `/api/v1/*`, `/healthz`; API-ответы не сжимаются, `content-encoding` у них отсутствует.
- `HEAD` отражает GET-заголовки (gzip допустим, длины нет — как и у chunked GET); 304/404/308 и Range не сжимаются; `Accept-Ranges: bytes` у Range сохранён.
- CSP-инвариант: в `index.html` 5 inline-скриптов, в CSP 5 `sha256-`-хэшей; все вычисленные хэши совпали с заголовком (и на HEAD-рантайме, и на импортированном после рестарта). Это подтверждает «CSP-хэши после рестарта» из приёмки T07.

## 3. P2-1: счётчик байт для gzip — доказательство

На живом сервере:

```text
static_bytes (m0)                    = 2 926 331
GET big JS (gzip, body 347 698 B)    → m1 = 3 274 029   delta = 347 698  (== gzip body)
GET same JS identity (1 267 133 B)   → m2 = 4 541 162   delta = 1 267 133 (== Content-Length)
5 × GET big JS gzip                  → delta = 1 738 490 (== 5 × 347 698)
GET /pnm/ gzip (class spa)           → delta = 15 770    (== gzip body)
```

Сырые строки:

```text
vks_http_response_bytes_total{class="static"} 2926331
vks_http_response_bytes_total{class="static"} 3274029   # после одного gzip-запроса
vks_http_response_bytes_total{class="static"} 4541162   # после identity-запроса
vks_http_response_bytes_total{class="api_v1"} 79
vks_http_response_bytes_total{class="health"} 28
vks_http_response_bytes_total{class="metrics"} 23801
```

То есть старый дефект (delta 0 на сжатой статике) закрыт: `CountingBody` (`crates/vks-server/src/lib.rs:381-427`) считает фактические кадры тела, а identity-ветка по `Content-Length` (`:361-371`) не сломана. Побочно проверено, что обёртка тела не ломает WebSocket-upgrade: сырой handshake на `/ws` вернул `101 Switching Protocols` с корректным `sec-websocket-accept`.

## 4. Импорт: идемпотентность и атомарность

Копия: `/dev/shm/t07rerevA_1793269057_433803/import/repo` (тот же `git archive 25c9d99`), `TMPDIR` — в scratch, архив — живой read-only.

### 4.1. Три прогона

| Прогон | Exit | Манифест | `.gz` | Дерево |
|---|---:|---:|---:|---|
| `--force` (run 1) | 0 | 215 строк | 0 | `sha256`-список A |
| `--force` (run 2) | 0 | 215 строк, sha `b062a2b1…` | 0 | **байт-идентично A** |
| `--force --gzip` (run 3) | 0 | тот же sha | 0 | **байт-идентично A**; warning «`--gzip` больше не нужен» |

После каждого прогона: `sha256sum -c` — **215 OK / 0 FAILED**, `check-pnm-runtime.py` — 0, `check-pnm-theme.py` — 0. Salvage: 482 восстановлено / 1 пропущено, исключено 6 `*.bak*`, удалено 0 симлинков. Тема после импорта: `meta theme-color: #0e1621`, `manifest theme_color: #17212B`.

### 4.2. Проектный слой сохранён

Хэши импортированных файлов совпали с pristine-HEAD:

```text
theme-toggle.js   dc7faddf45905130970805c1a729b793890d1ba71e7be6b540b25c5e3f303d45
deeplink.js       05a434652ca16f7e6de587956d843820183b6efe6e6f99e2fa3a960c9640ddda
perf-overrides.js 238e8d75aa29d47f1c0e00807061a70d1756f940396eb08fd57123081c76264f
api-errors.js     452a4b16745e2f25d0eb0e25feb9f0da04b301d1082063fcb29a9b2b3b44f55c
codian-layouts.js 953e7ec4a24fcb9019ec6ce781e4a398836af584b2c0014cd4361669b5ac2882
```

Блок `<!-- prostor:project-layer:start -->…end -->` (306 символов) и `<style id="prostor-pnm-overrides">` (11 198 символов) в `index.html` байт-идентичны pristine; в импортированном `index.html` присутствуют ссылки на все пять проектных скриптов, в т.ч. `theme-toggle.js` и `deeplink.js`.

### 4.3. Провал после swap: восстановление

**(a) Удалён `scripts/patch-pnm-chartjs.py`** (ошибка после swap, до манифеста):

```text
FAIL_EXIT=1
нет патч-скрипта: .../scripts/patch-pnm-chartjs.py
ИМПОРТ ОТМЕНЁН: прежний рантайм и манифест восстановлены
RUNTIME_RESTORED_IDENTICAL
MANIFEST_RESTORED_IDENTICAL
no stage/prev leftovers
RUNTIME_EXIT=0  THEME_EXIT=0
```

**(b) Синтетический архив без `meta[name=theme-color]`** (провал на шаге темы, уже после патчей):

```text
THEME_FAIL_EXIT=1
не найден статический meta[name=theme-color]
ИМПОРТ ОТМЕНЁН: прежний рантайм и манифест восстановлены
RUNTIME_RESTORED_IDENTICAL
MANIFEST_RESTORED_IDENTICAL
no stage/prev leftovers
```

В обоих случаях восстановленное дерево совпало с pre-failure `sha256`-списком всех 215 файлов, манифест — байт-в-байт, `.pnm-prev`/`.pnm-stage` не остались, чекеры снова зелёные. Это закрывает P2-2 в объёме, который проверяли prior-ревью.

### 4.4. Остаточная щель атомарности (P3)

Прерывание импорта сигналом после swap **не** восстанавливает состояние. Эксперимент: SIGTERM в момент «применение вендорских патчей» (после swap, до манифеста):

```text
IMPORTER_RC -15
--- state ---
client/pnm-frontend/deployed/.pnm-prev   # остался
RUNTIME_EXIT=1   THEME_EXIT=1   SHA_EXIT=1
```

Причина: EXIT-trap получает `$?=0` при завершении по SIGTERM/SIGINT (проверено отдельным trap-тестом: `TRAP rc=0` для TERM/INT/HUP), условие `rc -ne 0` не срабатывает, restore пропускается. Дополнительно trap ставится **после** двух `mv` (`:214-215`), поэтому провал самого swap-окна тоже не покрыт. Новое дерево остаётся в `deployed/`, манифест — старый; гейты красные, `.pnm-prev` хранит прежний рантайм, то есть состояние восстановимо вручную, но не автоматически. Это P3 (не тихий успех, обычные ошибки покрыты), однако формулировка «любым провалом» в комментарии `:212` шире фактического поведения.

## 5. Документация: truth-check

- `client/pnm-frontend/NOTES.md`: **215 файлов** (стр. 27, 31, 66), команда манифеста с `! -name '*.gz'` (стр. 66), `--gzip` больше не нужен, сжатие в рантайме (стр. 118-120), `.gz` не должны появляться. Старых «212/210» нет. Корректно.
- `docs/audit/2026-10-29/00_MASTER_PLAN.md`: 215/215 (стр. 111, 186), runtime gzip вместо precompressed (стр. 112, 186), `check-pnm-theme.py` (стр. 186). Корректно.
- `docs/decisions/0027-pnm-legacy-frontend-adopt-and-shim.md`: 215 файлов (стр. 47), `--gzip` не нужен — сжатие в рантайме (стр. 48). Корректно; «216 записей» в контексте относится к архиву, а не к рантайму.
- **Расхождение:** шапка самого `scripts/import-pnm-frontend.sh:5,25-26` всё ещё обещает «`--gzip` — сгенерировать `.gz` … для `precompressed_gzip`», хотя флаг игнорируется (`:381`). Это ровно prior P3-4, не исправленный; предупреждение по-прежнему печатается в конце, после манифеста.
- Rebrand: `NOTES.md:95-99` честно говорит, что слой PROSTOR не применяется, а `scripts/import-pnm-frontend.sh:351-355` печатает предупреждение. Но гейты `ci.yml:255-258` брендинг не проверяют (см. P2).
- E2E: в CI явно manual-only (`ci.yml:307-310`, `00_MASTER_PLAN.md:137,164`). Документация это не выдаёт за CI-гейт; overclaim нет.
- Overclaim в commit message: «reviewA/B P2-1..P3» читается шире фактического набора (P2-3 rebrand и часть P3 остались). Это не docs-файл, но при следующем коммите формулировку стоит сузить.

## 6. Findings

### P2 (1, pre-existing, not introduced by `25c9d99`)

**P2-1. Re-import откатывает rebrand, все integrity-гейты при этом зелёные.**
`scripts/import-pnm-frontend.sh:351-355` только предупреждает; `.github/workflows/ci.yml:255-258` проверяет манифест/refs/тему, но не `PROSTOR`-брендинг. Факт на импортированной копии: **90 файлов** отличаются от HEAD (`diff` 215-файловых списков), `<title>` становится `TrueConf MCU` (был `PROSTOR`), `manifest.name` — `TrueConf MCU` (был `PROSTOR`), при этом `sha256sum -c`, `check-pnm-runtime.py`, `check-pnm-theme.py` — все exit 0. Это prior P2-3; `25c9d99` его не закрывает (и не заявляет закрытие), документация честно предупреждает. Риск: maintainer делает документированный `--force`, видит зелёный CI и коммитит регресс бренда. Рекомендация: дешёвый CI-ассерт `title.includes("PROSTOR")` + `manifest.name=="PROSTOR"` (или явный TODO/issue, чтобы гейт не выглядел полным).

### P3 (7)

1. **Устаревшая документация `--gzip` в самом скрипте.** `scripts/import-pnm-frontend.sh:5,25-26` обещает генерацию `.gz` для `precompressed_gzip`; фактически флаг игнорируется, warning — только на `:381` после манифеста. Функционально безвредно (0 `.gz`), но прямо противоречит docs-truth и prior P3-4.
2. **Атомарность не покрывает сигналы/окно swap.** `scripts/import-pnm-frontend.sh:212-230`: trap ставится после `mv` (`:214-215`), а `rc=$?` при SIGTERM/SIGINT равен 0, поэтому restore не выполняется. Живой тест: SIGTERM после swap → код -15, `.pnm-prev` остался, рантайм новый, манифест старый, чекеры красные. Плюс сценарий первого импорта без `$PREV` вообще не восстанавливает дерево. Рекомендация: ставить trap до `mv`, добавить `trap 'exit 130' INT TERM` (или проверять `swap_committed` независимо от `rc`), и явно проверять отсутствие `$PREV` после успеха.
3. **`check-pnm-theme.py` по-прежнему first-match.** `scripts/check-pnm-theme.py:20,37` берёт первый `meta`/`theme_color`; дубликат второго `meta theme-color` со старым `#0097A7` пройдёт (prior P3-6). Рекомендация: `findall` + `count == 1`.
4. **`check-pnm-runtime.py` — names-only.** `scripts/check-pnm-runtime.py:22-47` сверяет имена, но не digests; standalone-запуск примет устаревшие хэши. В CI перед ним идёт `sha256sum -c`, поэтому гейт в CI полный; dotfile-фикс на `:30` корректен (`name[2:] if startswith("./")`, а не `lstrip("./")`).
5. **Ref-check остаётся частичным.** `scripts/import-pnm-frontend.sh:341`: одинарные кавычки и `?query` теперь обрабатываются (prior P3-2 частично закрыт), но сканируются только `src="/mcu/…"`; ссылки `/pnm/…`, `src = "…"` и обратное направление («файл есть, но не referenced») не проверяются. Для текущего `index.html` это не дефект, но гейт неполный.
6. **E2E не в CI.** `.github/workflows/ci.yml:307-310` и `00_MASTER_PLAN.md:137,164` явно фиксируют manual-only. Это известный residual, не overclaim; acceptance T07 (cold deep link) проверялся prior review B вручную, в этом backend-ревью e2e не перезапускался.
7. **Predicate path-agnostic.** `crates/vks-server/src/frontend.rs:120-145` смотрит только статус/заголовки; отсутствие сжатия у `/api`/`/metrics` держится исключительно на том, что `CompressionLayer` вложен в `Frontend::router` (`:113`). Сейчас live-проверено (API/metrics identity, Vary 0), но hoist слоя в `build_app` вернёт BREACH-риск. Комментарий-предупреждение/тест на будущее были бы дешевле.

## 7. Hygiene

- Все сборки/импорты/серверы — только в `/dev/shm` scratch и `/home/pro/.cache/t07rerevA_target`; `/tmp` для копии не использовался.
- Live `vks_data.db` не открывалась; размер/mtime/md5 до и после совпали. Live-порты `:8080`/`:18082` не тронуты.
- Ревью-серверы `:49400`/`:49401` убиты, порты закрыты; временные процессы не остались.
- Pristine-копии, импорт-копии, synth-архив, логи и target удалены после снятия доказательств; в живом дереве этим ревью добавлен только `docs/audit/2026-10-29/reviews/T07_rereviewA.md`.
- Замечание о конкурентности: во время ревью другие агенты меняли живой worktree/HEAD (T11c и docs); pristine-копия `25c9d99` от этого не зависела.

## 8. Итог

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **1** (pre-existing rebrand gate) |
| **P3** | **7** |

**Score: 9.3/10. Done: yes.** Приёмка T07 в backend/static-части выполнена и подтверждена исполнением: gzip + ровно один `Vary` на всей статике, честные 304/404/308/Range/HEAD, API/metrics без сжатия, точный счётчик байт для gzip и identity, импорт 215/215 с идемпотентными байтами и восстановлением рантайма+манифеста при провале после swap, зелёные fmt/clippy/test/metadata/build/checkers, docs без старых 212/210/precompressed-утверждений. Открытый P2 — pre-existing rebrand-гейт, честно задокументированный и вне acceptance T07; P3 — хвост hardening/доков, не блокирующий.
