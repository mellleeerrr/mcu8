# 05 — Регрессии, дубли, конфликты конфигов и гигиена кодовой базы

> **Позиция (агент `regress-hygiene`).** Любая оптимизация производительности на текущем
> состоянии репозитория **невоспроизводима**: у одного и того же «релиза» нет однозначного
> источника истины — ни в конфиге (три слоя дефолтов, два systemd-юнита, два API-поколения),
> ни в артефакте (собранный бинарь ≠ протестированный CI ≠ живой процесс). Пока это не
> сведено к одному канону и не вычищен мусор, числа перфа нельзя ни доказать, ни повторить,
> ни защитить от регрессий. **Сначала гигиена и единый канонический конфиг — потом перф.**
>
> **Уверенность:** в фактах — 0.95 (каждый пункт ниже проверен командой на этой ВМ
> 2026-10-25, 17:00–17:10 MSK); в приоритете «гигиена раньше перфа» — 0.86
> (остаток — за то, что часть гигиены можно вести параллельно с перфом при
> замороженном артефакте; см. §6.7).
>
> **Границы работы.** Только чтение/инвентаризация. `cargo build/test`, рестарты,
> удаления и правки вне этого файла **не выполнялись**. Живой `vks-server` (PID 1177177)
> не трогался; из обращений к нему — только `GET /healthz`, `/readyz`, `/metrics`,
> `/api/v1/rooms`, `/api/v3/sessions`, `/pnm/monitoring/sessions`, `/openapi.json`.
> Всё, что не проверено, помечено «не проверено».
>
> **Важное предупреждение о наблюдении (с поправкой атрибуции от Lead).** Во время моей
> инвентаризации (между 17:01 и 17:07 MSK) параллельно шли **намеренные** шаги Lead:
> пересборка `cargo build --release` (4m41s, exit 0 — верификация воспроизводимости
> тулчейна) и удаление `target/debug/incremental` (−32 ГБ, освободило 66→105 ГБ).
> Аномалией это не является, но даёт два **обязательных** вывода для плана:
> (1) живой сервер при этом продолжал исполнять **старый** (удалённый) inode, т.е. перед
> любым замером нужен `systemctl restart` и фиксация `md5`/`sha256` бинаря в отчёте
> (R-01 → §2.0 i.1); (2) замеры, снятые в минуты параллельной сборки, несопоставимы —
> метки времени ниже сохранены как факт, но baseline берётся только после заморозки (§6.7).

---

## 1. Таблица конфликтов (дубли и расхождения)

Формат: `путь A ↔ путь B | в чём расходятся (ключ = значение A / значение B) | что ломается |
что оставить (канон) | как проверить`.

### 1.1. Конфигурация

| # | A ↔ B | Расхождение (конкретные ключи/значения) | Что ломается | Канон | Как проверить |
|---|---|---|---|---|---|
| K-01 | `crates/vks-common/src/config.rs:195–212` (`impl Default for Config`) ↔ `configs/default.toml` | **9 из 16 ключей расходятся** (`config.rs:196–211` ↔ TOML): `server_name` `vks`↔`vks-mcu`; `listen_addr` `0.0.0.0:443`↔`0.0.0.0:8080`; `media_listen_addr` `127.0.0.1:0`↔`0.0.0.0:40000`; `database_url` `""`↔`sqlite://vks_data.db?mode=rwc`; `jwt_secret` `""`↔`dev_ec994ad4…`; `jwt_ttl_secs` (совпадает 86400); `admin_password` `""`↔`admin_password`; `ice_servers` `[]`↔google STUN; `allow_insecure_defaults` `false`↔`true`; `candidate_addr` `None`↔`Some("")`. Совпадают 7: `auth_provider`, `frontend_dir`, `jwt_ttl_secs`, `admin_login`, `storage_allow_fallback`, `ldap_url`, `default_room_mode` | Причина архитектурная: `Config::load` (`config.rs:226–239`) делает `Serialized::defaults(Config::default())` → `.merge(Toml::file(path))` → `.merge(Env::prefixed("VKS_"))`, т.е. **TOML — это оверрай над кодом, а не источник**. Ключ, выпавший/опечатанный в TOML, молча остаётся код-дефолтом: `listen_addr=0.0.0.0:443` (привилегированный порт → bind-ошибка или чужой TLS-порт), `database_url=""` (fail-closed по `NoPersistentStorage`, но не «обязательно указать»). Тихая подмена значения, а не ошибка | **Источник правды — TOML**; `Config::default()` только для тестов. Патч — §8.A (strict-загрузка: проверка «все 16 ключей присутствуют в TOML» + лог источника каждого ключа через `Figment::find_metadata`) | `sed -n '195,212p' crates/vks-common/src/config.rs`; `grep -E '^[a-z_]+ *=' configs/default.toml` |
| K-02 | `configs/default.toml` ↔ `configs/prod.example.toml` | `jwt_ttl_secs`: `86400` ↔ `28800`; `database_url`: sqlite ↔ `postgres://vks:CHANGE_ME@…`; `frontend_dir`: `client/pnm-frontend/deployed/pnm` (относительный) ↔ `/opt/vks/frontend/pnm`; `ice_servers`: public Google ↔ собственные TURN/STUN; `admin_password`: задан (`admin_password`) ↔ отсутствует; `candidate_addr`: `""` ↔ `""` (оба пусты — префлайт при `VKS_WEBRTC_TRANSPORT=1` даст exit 78) | Расхождение частично намеренное (dev/prod), но **не зафиксировано списком разрешённых отличий** ⇒ любой дрейф выглядит как «норма». Относительный `frontend_dir` зависит от cwd процесса | Канон = prod-шаблон; dev-отличия — явный allowlist из 6 ключей (`allow_insecure_defaults`, `database_url`, `jwt_ttl_secs`, `admin_password`, `frontend_dir`, `ice_servers`) + тест, который падает на любом седьмом отличии | `diff <(grep -E '^[a-z_]+ *=' configs/default.toml \| sort) <(grep -E '^[a-z_]+ *=' configs/prod.example.toml \| sort)` |
| K-03 | `configs/prod.example.toml` ↔ `deploy/vks.env.example` | Пример env заявляет «VKS_* override server.toml» и **намеренно** не содержит `VKS_DATABASE_URL`/`VKS_CANDIDATE_ADDR`/`VKS_FRONTEND_DIR`, но `Config::load` (`config.rs:245`) принимает их как env, а `deploy/README.md` в одном месте велит править TOML, в другом — env | Оператор не знает, какой файл «главный» при конфликте; `candidate_addr` в dev-стенде живёт **только** в drop-in `~/.config/systemd/user/vks-mcu.service.d/stand.conf`, т.е. вне репозитория и вне любого аудита | Env — только секреты (`VKS_JWT_SECRET`, `VKS_ADMIN_PASSWORD`, H.323-пути). Остальное — TOML. Зафиксировать в `deploy/README.md` таблицей «ключ → единственный носитель» | `cat deploy/vks.env.example`; `grep -n 'candidate_addr' deploy/README.md configs/*.toml` |
| K-04 | `deploy/systemd/vks-mcu.service` (dev) ↔ `deploy/systemd/vks-mcu-prod.service` | dev: `ExecStart=%h/mcu/target/release/vks-server --config %h/mcu/configs/default.toml`, `VKS_WEBRTC_TRANSPORT=1`, `VKS_SIP_UAS_ENABLED=1`, лог в `%h/mcu/vks_server.log`, БД в `%h/mcu/vks_data.db`; prod: `/opt/vks/bin`, `LoadCredential=server.toml`, `DynamicUser=yes`, `VKS_WEBRTC_TRANSPORT=0`, `StateDirectory/LogsDirectory` | Dev-юнит — единственный, кто пишет **в рабочее дерево репозитория** (`vks_server.log`, `vks_data.db`, `.lock`); после «чистки» (tracker S30) они возвращаются на каждом старте. Плюс `VKS_WEBRTC_TRANSPORT=1` включает ICE/префлайт и требует `candidate_addr` из drop-in | Оставить оба, но dev-юнит перевести на `%h/mcu/var/` (или `StateDirectory`) и в `deploy/README.md` добавить явную таблицу «dev vs prod отличия». Tracker S17/B7 закрыл это как «конфликта нет» — **по факту конфликт сместился в сторону рабочего дерева** | `diff -u deploy/systemd/vks-mcu.service deploy/systemd/vks-mcu-prod.service`; `ls -la vks_server.log vks_data.db*`; `systemctl --user cat vks-mcu.service \| diff - deploy/systemd/vks-mcu.service` |
| K-05 | Путь репозитория: `%h/mcu` (юнит) ↔ `/home/pro/Рабочий стол/mcu` (реальный) ↔ `/home/pro/mcu` (argv работающего процесса, лог `configuration loaded … path=Some("/home/pro/mcu/configs/default.toml")`) | Симлинк `/home/pro/mcu -> /home/pro/Рабочий стол/mcu` создан 2026-10-21 19:39 и стал **неявной зависимостью** юнита; `scripts/run-local.sh:31` считает корень через `pwd -P` (реальный путь) | Удаление/переименование симлинка ломает systemd-юнит молча (`ExecStart` не найдётся); три разных написания пути попадают в логи/доки/проверки, из-за чего `run-local.sh status` сравнивает exe не по той строке | Канон — реальный путь (`pwd -P`); симлинк — только алиас, зафиксировать его в `deploy/README.md` как обязательный предусловие dev-юнита | `readlink -f /home/pro/mcu`; `grep -n '%h/mcu' deploy/systemd/vks-mcu.service`; `grep -o 'path=Some([^)]*)' vks_server.log \| tail -1` |
| K-06 | `docs/rules/BACKEND_RULES.md` §2.3 (карта портов) ↔ фактические порты | Документ: `443 TCP` WSS+REST, `8443` WebTransport, `3478/5349` STUN/TURN, `40000–60000` RTP, `9090` Prometheus, `4222` NATS, `5432` PG. Факт (`ss -lntup`): `8080 TCP` (plain HTTP, `/metrics` там же), `5060 UDP` SIP, `40000 UDP` media, `1720 TCP` H.323 sidecar | Любой новый перф-замер «по документу» не совпадёт с реальностью; порт-скан/фаервол-чеклист по документации открывает не те порты; `/metrics` на публичном HTTP-порту, а не на `9090` | Канон — **факт** (`8080/5060/40000/1720`), документ переписать таблицей «порт → носитель (юнит/сайдкар) → default». Отдельно отметить, что TLS/443 в репозитории отсутствует (терминируется вне) | `ss -lntup`; `grep -n '8080\|443\|9090\|4222' docs/rules/BACKEND_RULES.md deploy/README.md configs/*.toml` |

### 1.2. API

| # | A ↔ B | Расхождение | Что ломается | Канон | Как проверить |
|---|---|---|---|---|---|
| K-07 | `/api/v1/*` (`crates/vks-admin/src/handlers.rs:734–752`: **14 путей / 24 метод-обработчика**) ↔ `/api/v3/*` (`crates/vks-admin/src/api_v3/mod.rs`: **167 регистраций = 149 уникальных путей**) ↔ `/pnm/*` (`crates/vks-server/src/frontend.rs:94`, статика с SPA-fallback) | Три пространства имён с разными контрактами и разной аутентификацией (`v1` — `route_layer(jwt_auth_middleware)`, `handlers.rs:756`; `v3` — свой адаптер; `/pnm/*` — вообще файлы). UI PNM использует **только** `/api/v3` (561 вхождение в `deployed/pnm/chunks/*.js`, 0 вхождений `/api/v1`) | `GET /pnm/monitoring/sessions` → **200 `text/html`, 57 542 Б** (SPA-fallback), а не JSON: клиент, ждущий TrueConf-совместимый эндпоинт, получает HTML с кодом успеха. **Мёртвый `/api/v1`:** за всё время жизни процесса — **3 запроса** (`api_v1`: GET 401 ×2, GET 404 ×1) против **36 000+** у `api_v3` (см. §8.B). `GET /api/v3/{rooms,health,stats,monitoring/sessions}` → **501** (9 записей в логе) — **осознанное решение** адаптера: catch-all задокументирован (`api_v3/mod.rs:4`), реализован `:483`/`:617`, «падение в 404 запрещено: UI считает 404 „метод не существует“» (`:622`) | Канон: `/api/v3` — единственный продуктовый API; `/api/v1` — deprecation-план §8.B (410 + `Deprecation`/`Sunset`, затем ADR); `/pnm/*` — **только** файлы, JSON-эндпоинты под `/pnm/` запрещены контракт-тестом; `501` **не менять** без ADR, но закрепить тестом «catch-all последний» | `curl -s -o /dev/null -w '%{http_code} %{content_type}\n' http://127.0.0.1:8080/pnm/monitoring/sessions`; `curl -s http://127.0.0.1:8080/metrics \| grep 'class="api_v1"'`; `grep -c 'unmatched route' vks_server.log` |
| K-08 | `GET /openapi.json` (`handlers.rs:254`, стаб 473 Б, **10** путей `/api/v1`) ↔ **163** уникальных реальных пути (`149` `/api/v3` + `14` `/api/v1`); в `handlers.rs` всего 17 регистраций = 14 (`/api/v1`) + `/healthz` + `/readyz` + `/openapi.json` | Схема не генерируется; `utoipa` удалён (tracker S30) | Контракт нельзя проверить машинно; SDK/клиенты генерировать не из чего; регрессия «эндпоинт исчез» ловится только E2E-краулером Playwright | Либо генерируемый контракт (повторное введение `utoipa` = ADR), либо `tests/api_routes_inventory.rs`: статический список «путь → метод → модуль» + тест, что роутер содержит ровно его | `curl -s http://127.0.0.1:8080/openapi.json \| jq '.paths \| keys \| length'` → `10`; `grep -c '\.route("/api/v1' crates/vks-admin/src/handlers.rs` → `14` |
| K-09 | Роуты v3 в `vks-admin` ↔ роуты v3 в `vks-server` (`crates/vks-server/src/lib.rs:149–175`: `/api/v3/gateways/status`, `/api/v3/cluster/status`) | Инвентарь API физически разнесён по двум крейтам и двум `Router::merge` (`lib.rs:178–180`) | Нельзя ответить «сколько у нас эндпоинтов» без чтения двух крейтов; дубль пути в двух местах даст панику на старте, а не тест | Все `/api/v3/*` — в `vks-admin::api_v3` (composition root только мержит один роутер) | `grep -rn '"/api/v3/' crates/vks-server/src crates/vks-admin/src \| wc -l` |
| K-10 | Алиасы v3: `/api/v3/users`, `/api/v3/users/`, `/api/v3/ui/users`, `/api/v3/ui/users/` (`mod.rs:207–212`) — 4 регистрации на один хендлер; всего **18** путей зарегистрированы дважды (слэш) и **36** `/ui/`-алиасов | Дубли намеренные (совместимость с вендором), но не сведены в один helper | Разрастание поверхности: правка одного хендлера требует помнить про 4 пути; тест «алиасы идентичны canonical» уже был нужен (tracker T10a) | Оставить алиасы, но генерировать их из таблицы `(path, handler)` в одном месте + тест «каждый canonical имеет `/ui` и `/`-алиасы» | `grep -c '\.route("/api/v3' crates/vks-admin/src/api_v3/mod.rs` → `167` |

### 1.3. Сборка и артефакты

| # | A ↔ B | Расхождение | Что ломается | Канон | Как проверить |
|---|---|---|---|---|---|
| K-11 | `crates/vks-server/Cargo.toml:20` `vks-media-mcu = { path = "../vks-media-mcu" }` (без фичи) ↔ `.github/workflows/ci.yml:53,71` `--all-features` ↔ `crates/vks-media-mcu/Cargo.toml:43` `oxideav = ["dep:oxideav-scene"]` | **В поставляемом бинаре `oxideav` выключен**: `strings target/release/vks-server \| grep -c oxideav-scene` = `0` (и для живого inode — тоже `0`). Композитор = «deterministic nearest-neighbour scaler» (`compositor.rs:5–7`), а ADR-0004/0029 и soak-эвиденс говорят о «реальном compositor». CI `check/clippy` компилирует `--all-features` (то есть другой граф), `test` — дефолтные фичи (`ci.yml:110`) | **Перф-цель не определена**: оптимизировать NN-скейлер или oxideav-композитор? Числа soak (mean 16.95 мс) относятся к дефолтному пути, а «реальный» композитор вообще не в артефакте и не в тестах. Любая правка в feature-коде не проверяется тестами (S15 в трекере прямо: «`oxideav` — no-op фича») | Решить и зафиксировать: либо `oxideav` включается в release-профиль `vks-server` и покрывается тестом, либо фича/крейт удаляются и ADR-0004 помечается `superseded` | `grep -n 'vks-media-mcu' crates/vks-server/Cargo.toml`; `strings target/release/vks-server \| grep -c oxideav-scene`; `grep -n 'all-features' .github/workflows/ci.yml` |
| K-12 | `ci.yml:110` `cargo test --workspace --locked` (дефолтные фичи) ↔ `ci.yml:53,71` `--all-features` ↔ `scripts/run-local.sh:33` `cargo build --release -p vks-server` (**без `--locked`**) | Три разных графа сборки в трёх местах. Локальная сборка может молча обновить `Cargo.lock` (`--locked` есть только в CI) | «CI зелёный» не означает «поставляемый бинарь протестирован»; локальная сборка может уехать на другие версии крейтов без единого изменения в git | Единый скрипт `scripts/build-release.sh` c `--locked --release -p vks-server` + фиксированный набор фич; все CI-джобы приводятся к тому же набору | `grep -n 'cargo build\|cargo test\|all-features\|--locked' .github/workflows/ci.yml scripts/run-local.sh` |
| K-13 | `Cargo.toml` `[profile.release] panic = "unwind"` ↔ `BACKEND_RULES.md` §6.2 «Production: `panic = abort`» | Правило документа противоречит манифесту; фактическое решение задокументировано только комментарием в `Cargo.toml` | Новый участник «исправит» профиль по правилам и сломает `catch_unwind` в FFI/медиа-пути | Оставить `unwind` (обоснование в `Cargo.toml` — валидное), **обновить** `BACKEND_RULES.md` §6.2. ADR не требуется: правило, а не архитектурное решение | `grep -n 'panic' Cargo.toml docs/rules/BACKEND_RULES.md` |
| K-14 | `rust-toolchain.toml` `channel = "1.94.0"` ↔ `docs/decisions/DECISIONS.md` ADR-0024 «MSRV 1.94.0 (CI), **1.99.0** (dev/release)» ↔ `ci.yml:22` `RUST_TOOLCHAIN: 1.94.0` | «Dev/release 1.99» не существует ни в тулчейне, ни в CI (на ВМ `rustc 1.94.0`, `1.94.0-x86_64-unknown-linux-gnu (active)`); при этом в `MASTER_TASK_LIST` (G5) упоминаются «новые Clippy lints Rust 1.99.0» | Документ врёт о версии; любые «мы проверяли на 1.99» — артефакт другой среды. Линт-дрейф не воспроизводим | ADR-0024 привести к факту: MSRV 1.94.0 везде, 1.99 — удалить или явно `proposed` | `cat rust-toolchain.toml`; `/home/pro/.cargo/bin/rustc --version`; `grep -n '1.99' docs/decisions/DECISIONS.md docs/progress/MASTER_TASK_LIST.md` |
| K-15 | `target/` на диске ↔ «clean worktree» в эвиденсе soak (`docs/progress/evidence/t14b-mcu-tick-soak/README.md`) | Инструкция замера: `git worktree add --detach … ; CARGO_TARGET_DIR="$OLDPWD/target" cargo test --release …` — «чистое» дерево, но **общий** `target/` с прод-сборкой и другими агентами. В `target/release/.fingerprint` — **54** директории `vks-server-*` и ≥5 `vks-media-mcu-*` (разные наборы фич/флагов в одном каталоге) | «Clean worktree» не гарантирует «clean artifact»: кэш общий, параллельные сборки конкурируют за lock, а живой сервер исполняет бинарь из того же каталога (см. R-01) | Для замеров — отдельный `CARGO_TARGET_DIR=/var/tmp/vks-perf-target` + запись `sha256` бинаря, `rustc -vV`, полного набора фич и профиля в артефакт эвиденса | `ls target/release/.fingerprint \| grep -c '^vks-server'` → `54`; `grep -n 'CARGO_TARGET_DIR' docs/progress/evidence/t14b-mcu-tick-soak/README.md` |

### 1.4. Документация и реестр решений

| # | A ↔ B | Расхождение | Что ломается | Канон | Как проверить |
|---|---|---|---|---|---|
| K-16 | `docs/decisions/DECISIONS.md` (32 ADR) ↔ фактический граф `Cargo.lock`/код | ADR-0006 (`rvoip`) — `accepted`, но `rvoip` **удалён** (S30; `Cargo.lock` — NOT_IN_LOCK), нового ADR нет. ADR-0014 (NATS) — `accepted`, фактически `crates/vks-cluster/src/bus.rs:1` «in-memory pub/sub, **заменяющий** NATS JetStream (ADR-0014)». ADR-0015 (TURN) — `accepted`, в коде 0 совпадений `turn`. ADR-0010/0011/0013 (Dioxus/Tailwind/progenitor/utoipa) — `accepted`, но `dioxus`/`taffy`/`hadrone-core`/`utoipa` в `Cargo.lock` отсутствуют, а фронт — legacy PNM (ADR-0027). ADR-0029 — `proposed`, хотя фазы 0–2 реализованы (tracker S21 🟡) | Нарушен `PROJECT_RULES.md` §4.4 («запрещено менять решение без нового ADR»). Реестр решений перестаёт быть источником истины: по нему нельзя понять, что реально принято. Для перф-работы это критично: ADR-0004 (композитор) и ADR-0029 (per-viewer) — прямые входы | Синхронизировать: 0006/0010/0011/0013/0014/0015 → `superseded by 0027/новый`; 0029 → `accepted` (или оставить `proposed` и убрать реализованные фазы из «сделано»). Один коммит `docs(adr): sync registry` | `cat docs/decisions/DECISIONS.md`; `for p in rvoip nats dioxus utoipa taffy hadrone-core; do grep -c "name = \"$p\"" Cargo.lock; done` |
| K-17 | `docs/rules/BACKEND_RULES.md` §1.1 («RS256/PS256; **HS256 запрещён в production**») ↔ `crates/vks-auth/src/jwt.rs:131` (`Algorithm::HS256`), `crates/vks-server/src/main.rs:182` (`"API v3 auth enabled (HS256; RS256 — долг §4.3)"`) | Код прямо логирует долг; правило уже нарушено в проде | Документ-правило перестаёт блокировать: «долг» зафиксирован в логе, а не в трекере с приоритетом | Долг в трекер как `C1` P1 c критерием; правило оставить (оно верное) | `grep -rn 'HS256' crates/vks-auth/src/jwt.rs crates/vks-server/src/main.rs` |
| K-18 | `docs/rules/BACKEND_RULES.md` §1.3 (Argon2id) ↔ `crates/vks-auth/src/local_db.rs:3,18–20` (PBKDF2-SHA256, 100 000 итераций) | Отклонение задокументировано в коде («argon2 не в разрешённых пинах»), но не в ADR/трекере | Смена хэша = миграция БД; без ADR её нельзя планировать | Отдельный ADR «хэш паролей: PBKDF2 сейчас, Argon2id — план миграции» | `sed -n '1,25p' crates/vks-auth/src/local_db.rs` |
| K-19 | `docs/PROJECT_RULES.md` Часть III (Dioxus, `client/vks-admin-ui/`, `client/vks-meeting-ui/`) ↔ `client/` (только `vks-client-sdk` + `pnm-frontend`) | Раздел описывает несуществующую структуру; ADR-0027 её отменил де-факто, но раздел не помечен | Новый агент идёт писать Dioxus-UI в несуществующий каталог | Пометить Часть III «superseded ADR-0027» (баннер в начале раздела) | `ls client`; `grep -n 'vks-admin-ui\|Dioxus' docs/PROJECT_RULES.md \| head` |
| K-20 | `deny.toml` (комментарий: «После удаления rvoip … TLS-стек в графе отсутствует») ↔ `Cargo.lock`: `aws-lc-rs 1.18.2` ← `dimpl` / `rcgen` / `x509-parser`; `aws-lc-sys 0.46.0` | TLS/C-крипто **есть** (через str0m→dimpl). `aws-lc-sys` — C-сборка: в `target/debug/build/aws-lc-sys-*` лежит `libaws_lc_0_46_0_crypto.a` (3 каталога × 32 МБ) | Комментарий вводит в заблуждение при аудите лицензий/supply-chain; «герметичность» ADR-0031 покрывает protoc, но **не** C-тулчейн | Поправить комментарий: TLS-граф есть; добавить в `deploy/README.md` предусловие C-компилятора (`cc 15.2.0`) | `grep -A2 'name = "aws-lc-rs"' Cargo.lock`; `ls target/debug/build \| grep aws-lc-sys`; `command -v cmake clang` → отсутствуют |
| K-21 | `docs/progress/MASTER_TASK_LIST.md` ↔ факты (выборочно) | (а) G3: «PID 479640» — фактически **1177177**; (б) S13/B5: «`candidate_addr` → `203.0.113.10`» — в `configs/default.toml` сейчас `""`; (в) `scripts/accept_sip.sh` упомянут как приёмка (A1a) — файла **нет**; (г) README «Тесты: 1333», трекер в разных местах 1326/1317; (д) G5 «lints Rust 1.99.0» при тулчейне 1.94 | Трекер — «единственный живой документ» (`PROJECT_RULES` §3.1), но содержит устаревшие факты; на них будут ссылаться агенты перф-раунда | Перед раундом — «сверить трекер»: PID/порты/пути/счётчики тестов/существование скриптов. Отдельный дешёвый чек-лист в §5 | `cat vks_server.log \| head -3`; `ls scripts/accept_sip.sh`; `grep -n '203.0.113.10' docs/progress/MASTER_TASK_LIST.md`; `grep -E '^[a-z_]+ *=' configs/default.toml \| grep candidate` |
| K-22 | `crates/vks-server/src/frontend.rs:94` `nest_service("/pnm/", …)` + `:86` `fallback(ServeFile(index.html))` ↔ будущие JSON-эндпоинты `/pnm/...` | SPA-fallback перехватывает **всё** под `/pnm/`; порядок merge в `crates/vks-server/src/lib.rs:177–184`: `api_v3` → `signaling` → `m5_api` → `/metrics` → `frontend.router()`, т.е. статика монтируется **последней**, а правило «новый JSON-роут — только до неё» нигде не зафиксировано | Через полгода кто-то добавит `"/pnm/monitoring/sessions"` и получит либо панику overlap, либо HTML-ответ — как сейчас с живым стендом | Правило в `frontend.rs` doc-комментарии: всё под `/pnm/` — файлы; контракт-тест «`/pnm/{known-json-path}` → не `text/html`» + тест «catch-all v3 зарегистрирован последним» | `curl -sI http://127.0.0.1:8080/pnm/monitoring/sessions \| head -3`; `sed -n '177,184p' crates/vks-server/src/lib.rs` |
| K-23 | `docs/architecture/SIGNALING_PROTOCOL.md:5` («WS/WSS + **WebTransport/QUIC** — обязательный, R5») ↔ код | В коде нет ни HTTP-роута `/wt`, ни QUIC-транспорта: только порт-абстракция `WebTransportSession` + in-memory `MockWt` (`crates/vks-signaling/src/wt.rs:1–5,9,29`), 12 упоминаний «WebTransport» — и все в виде комментариев/трейтов. `quinn`/`webtransport` в `Cargo.lock` отсутствуют | Спецификация объявляет обязательный транспорт, которого нет; при этом требование R5 формально «выполнено» трейтом. В инвентарь роутов `/wt` попасть не должен — это дыра, а не контракт | Либо реализовать (quinn + сертификаты + `Alt-Svc`) — отдельный ADR, либо снять «обязательный» из спецификации и пометить `deferred` | `grep -rn 'WebTransport' crates \| wc -l` → `12`; `grep -rn '"/wt"' crates` → пусто; `grep -c 'name = "quinn"' Cargo.lock` → `0` |

---

## 2. Регрессии и долги

Приоритеты: **P0** — блокируют доказательность работы (перф/безопасность/данные);
**P1** — ломают воспроизводимость или контракт; **P2** — долг гигиены.
Столбец «доказано» — конкретика, а не мнение.

### 2.0. Разделение приоритетов для плана Lead (обязательная нарезка)

Требование Lead: в план до бенчмарка идёт **только (i)**. Нарезка по моим id:

**(i) P0 — блокирует честный бенчмарк** (без этого числа неатрибутируемы):

| # | Что | Мой id | Почему блокирует | Оценка |
|---|---|---|---|---|
| i.1 | Перезапуск стенда с новым бинарём + фиксация `md5`/`sha256` в отчёте замера | R-01 | Живой процесс исполняет удалённый inode; «до/после» меряется на разных бинарях | 5 мин + 1 строка в шаблоне замера |
| i.2 | Единый канонический конфиг: TOML как источник, strict-загрузка (все 16 ключей), лог источника по ключу | R-09, K-01 | `listen_addr=443`/пустая БД при выпавшем ключе → замер на другом сетевом/хранилищном профиле | 0,5 дня (§8.A) |
| i.3 | Отсутствие расхождения дефолтов `Config::default` ↔ `configs/default.toml` | R-09 | То же; плюс тест `config_parity` не даёт регрессии вернуться | 2 ч |
| i.4 | Запись паспорта артефакта: `sha256` бинаря + набор фич + `rustc -vV` + `RUSTFLAGS`/`target-cpu` + версия C-тулчейна | R-20, K-15 | Без этого soak-эвиденс невоспроизводим (и не принимается как доказательство) | 10 строк в harness/README |
| i.5 | Отдельный `CARGO_TARGET_DIR` для замеров (не общий `target/`) | K-15, R-03 | 54 fingerprint-набора `vks-server-*` = неопределённость, какой бинарь измерен | 1 env-переменная в скрипте замера |
| i.6 | Решение по `oxideav`: включить в release или снять фичу | R-02, K-11 | Определяет, **что** оптимизируется (NN-скейлер vs oxideav); сейчас цель не определена | 1 ADR + 1 строка |

**(ii) P1 — мешает качеству, не блокирует замер** (после бенчмарка): API-дубли/`/api/v1`
deprecation (K-07/§8.B), инвентарь роутов + `openapi` (K-08, R-11), реестр ADR (§8.C, K-16,
R-12), `/pnm`-порядок и контракт-тесты (K-22), dev-юнит и лог/БД в рабочем дереве (R-10),
`--locked` в `run-local.sh` (R-08), CI test на поставляемом наборе фич (R-07), `cargo-deny`
advisories (§8.D), `/wt`-дыра в спецификации (K-23).

**(iii) P2 — косметика/артефакты**: стэши и висячие worktree (R-13), старые бинари и
`~/.cache/r5-head-target` 6,5 ГБ (R-14), venv без pip (R-15), крупные файлы в git (R-16/R-18),
дубли версий в lock (R-19), карта портов в правилах (R-23), несуществующий `accept_sip.sh`
(R-24), `thiserror` ×2 (R-25).

| id | file:line | Суть | P | Доказано |
|---|---|---|---|---|
| R-01 | `/proc/1177177/exe` (deleted) ↔ `target/release/vks-server` | **Живой сервер исполняет удалённый inode.** PID 1177177 (start 12:23:11) → `target/release/vks-server (deleted)`, size 26 476 960, mtime 2026-10-23 23:53:49, inode 1470906, `md5 a59ca1bf…`. На диске — size 26 522 112, mtime **2026-10-25 17:06:33**, inode 1469631, `md5 37006a1d…`. Git при этом чист (`22f0417`) | **P0** | `ls -la /proc/1177177/exe`; `md5sum /proc/1177177/exe target/release/vks-server`; `stat -Lc '%s %y %i' …` |
| R-02 | `crates/vks-server/Cargo.toml:20` | Поставляемый бинарь собирается **без** фичи `oxideav`; композитор = NN-скейлер (`crates/vks-media-mcu/src/compositor.rs:5–7`). `strings … \| grep -c oxideav-scene` = `0` и для старого, и для нового бинаря | **P0** | `strings target/release/vks-server \| grep -c oxideav-scene` → `0`; `grep -n 'oxideav' crates/vks-media-mcu/Cargo.toml` |
| R-03 | `target/release/.fingerprint` | 54 набора `vks-server-*` в одном `target/`; ≥5 `vks-media-mcu-*`. Нет записи, какой набор соответствует живому бинарю | **P0** | `ls target/release/.fingerprint \| grep -c '^vks-server'` → `54` |
| R-04 | `target/debug` | Мусор сборки: на 17:01 — `target` 86 ГБ (`debug` 84 ГБ: `deps` 38 ГБ, `incremental` 32 ГБ, `build` 2,3 ГБ), 15 957 файлов в `deps`; на 17:07 после чужой чистки — 46 ГБ (`debug` 41 ГБ). Диск: было 188/266 ГБ (66 свободно) → стало 149/266 (105 свободно) | **P1** | `du -sh target/*` ×2 (17:01 и 17:07); `ls target/debug/deps \| wc -l` |
| R-05 | `target/debug/build/*` | **534,8 МБ** build-каталогов пакетов, которых **нет в `Cargo.lock`**: `protobuf-src` 260,2 МБ (удалён ADR-0031), `libmimalloc-sys` 40,4, `stacker` 34,1, `psm` 34,2, `openssl`+`openssl-sys` 33,7, `bigdecimal` 17,2, `rust_decimal` 16,2, `memoffset` 17,0, `nix` 16,4 и др. — всего **18** пакетов | **P1** | скрипт сверки `target/debug/build/*` с `Cargo.lock` (в §3, шаг 3) |
| R-06 | `target/debug/libvks_gateway.rlib` (4 064 Б, Oct 15), `libvks_monitoring.rlib` (1 305 236 Б, Oct 15) | Остаточные артефакты **удалённых крейтов** (`crates/vks-gateway`, `crates/vks-monitoring` отсутствуют; в workspace 15 крейтов + `client/vks-client-sdk`) | **P1** | `ls -la target/debug/libvks_gateway* target/debug/libvks_monitoring*`; `ls crates` |
| R-07 | `.github/workflows/ci.yml:110` | CI-тесты идут на **дефолтных фичах**, `check/clippy` — на `--all-features`; поставляемый набор не тестируется. Feature-код (`oxideav`, часть `str0m`) не покрыт тестами | **P1** | `grep -n 'cargo test\|all-features' .github/workflows/ci.yml` |
| R-08 | `scripts/run-local.sh:33` | Сборка без `--locked`: локальный билд может изменить `Cargo.lock`, а CI этого не увидит | **P1** | `sed -n '30,36p' scripts/run-local.sh` |
| R-09 | `crates/vks-common/src/config.rs:193–215` ↔ `configs/default.toml` | Двойной слой дефолтов с расходящимися значениями (K-01): `443` vs `8080`, пустая БД vs sqlite. Ошибки нет — есть тихий откат | **P1** | `sed -n '193,215p' crates/vks-common/src/config.rs`; §1.1 K-01 |
| R-10 | `vks_server.log` (корень репо) | Dev-юнит пишет лог и БД **в рабочее дерево**: `vks_server.log` 105 840 Б (16:58), `vks_data.db` 327 680 Б (16:30), `vks_data.db.lock`. Tracker S30 их «удалил» — они вернулись на следующем старте | **P1** | `ls -la vks_server.log vks_data.db*`; `grep -n 'StandardOutput' deploy/systemd/vks-mcu.service` |
| R-11 | `vks_admin::api_v3::mod.rs:4,483,617,622` | Неизвестный `/api/v3/*` → **501** — **осознанное** решение (конверт TrueConf-адаптера; «падение в 404 запрещено: UI считает 404 „метод не существует“»). В логе 9 таких запросов (`/api/v3/rooms`, `/health`, `/stats`, `/monitoring/sessions`), т.е. клиенты уже ходят за несуществующими путями. Остаточный риск — не код, а **порядок**: catch-all должен быть последним, иначе новые JSON-роуты молча уедут в 501 | P2 | `sed -n '612,624p' crates/vks-admin/src/api_v3/mod.rs`; `grep -c 'unmatched route' vks_server.log` → `9` |
| R-12 | `docs/decisions/DECISIONS.md` | 6 ADR де-факто отменены/изменены без supersede (K-16) — нарушение `PROJECT_RULES` §4.4 | **P1** | §1.4 K-16 |
| R-13 | `git stash list`, `git worktree list` | 2 стэша (`temp-taskwork`, `t1-wip`) и 2 «висячих» worktree: `/tmp/t14b-r4` (**prunable**), `/var/tmp/vks-s20-clean` | P2 | `git stash list`; `git worktree list` |
| R-14 | `/home/pro/.local/bin/vks-server`, `/home/pro/.cache/r5-head-target/` | Старые бинари вне репо: 26 476 640 Б (Oct 23 22:13, `md5 4a5d47f7…` ≠ репо) и debug-бинарь 169 591 592 Б (Oct 23 21:48); **6,5 ГБ** в `~/.cache/r5-head-target` | P2 | `ls -la /home/pro/.local/bin/vks-server /home/pro/.cache/r5-head-target/debug/vks-server`; `du -sh /home/pro/.cache/r5-head-target` |
| R-15 | `/home/pro/.local/share/vks-mcu/h323-venv` | Python-сайдкар: 157 МБ, Python 3.14.4, `pycrate 0.8.1`; создан `python3 -m venv --without-pip` ⇒ **в venv нет pip**, обновление/проверка зависимостей возможны только через `crates/vks-gateway-h323/python/install_runtime.py`. Пины: `requirements.lock` (1 строка, sha256). Prod-путь другой (`/opt/vks/h323-venv`) | P2 | `cat …/pyvenv.cfg`; `cat crates/vks-gateway-h323/python/requirements.lock`; `du -sh …` |
| R-16 | `crates/vks-gateway-h323/python/h323_asn.py` | 1 274 156 Б сгенерированного ASN.1-кода в git; нет проверки «сгенерировано из `schemas/*.asn` тем же генератором» в CI | P2 | `git ls-files -z \| xargs -0 stat -c '%s %n' \| sort -rn \| head` |
| R-17 | `.github/workflows/ci.yml` (job `doc`) | ~30 битых intra-doc-ссылок в 10 крейтах ⇒ `doc` намеренно **мягкий** (`continue-on-error`) | P2 | `grep -n 'continue-on-error' .github/workflows/ci.yml`; трекер, «Найдено, но не исправлено» |
| R-18 | `crates/vks-gateway-h323/python/h323_asn.py`, `docs/progress/evidence/**` | Крупные бинарно-подобные артефакты в git: `docs/progress/evidence` — **294** файла, 14,0 МБ tracked; `.git` — 66 МБ; крупнейшие: `evidence/t06b1/network.json` 2,14 МБ, `evidence/t16/http-trace.json` 1,25 МБ, `evidence/t24/export.zip` 689 КБ | P2 | `git ls-files -z docs/progress/evidence \| xargs -0 du -cb \| tail -1` |
| R-19 | `Cargo.lock` | 434 пакета, **28** пакетов с >1 версией; самые «дорогие»: `rand` ×3 (0.8.8/0.9.5/0.10.3), `getrandom` ×3 (0.2.17/0.3.4/0.4.3), `syn` ×2 (2.0.119/**3.0.9**), `thiserror` ×2 (1.0.69/2.0.21), `windows-sys` ×3, `toml_edit` ×2, `nom` ×2 | P2 | скрипт в §4.1 |
| R-20 | `docs/progress/evidence/t14b-mcu-tick-soak/README.md` | Эвиденс-замер честный (commit `62cff91`, `dirty=false`), но **не фиксирует**: набор фич, `RUSTFLAGS`/`target-cpu`, `sha256` бинаря, версию C-тулчейна (`aws-lc-sys`). Плюс `CARGO_TARGET_DIR` — общий | P1 | §1.3 K-15; `grep -n 'CARGO_TARGET_DIR\|rustc' docs/progress/evidence/t14b-mcu-tick-soak/README.md` |
| R-21 | `crates/vks-server/src/main.rs:182` | HS256 + `jwt_ttl_secs` 86 400 (dev) / 28 800 (prod); refresh-токена нет; `BACKEND_RULES` §1.1 требует RS256 и TTL 15 мин. Живой стенд: `allow_insecure_defaults=true` (`WARN` в логе) на `0.0.0.0:8080` | **P1** | `grep -n 'HS256' crates/vks-server/src/main.rs`; `grep -c 'allow_insecure_defaults=true' vks_server.log` |
| R-22 | `configs/default.toml:18` | Опубликованный dev-JWT-секрет (`dev_ec994ad4…`) захардкожен в репо и в константе `DEV_JWT_SECRET` (`config.rs:76`); `admin_password = "admin_password"` — в `KNOWN_WEAK_PASSWORDS` (`config.rs:230`) и блокируется только при `allow_insecure_defaults=false`. Стенд работает на `0.0.0.0` | **P1** | `grep -n 'jwt_secret\|admin_password' configs/default.toml`; `grep -n 'KNOWN_WEAK_PASSWORDS' crates/vks-common/src/config.rs` |
| R-23 | `docs/rules/BACKEND_RULES.md` §2.3 | Карта портов не соответствует факту (K-06); `/metrics` на публичном `8080` (14 424 Б отдаётся `200`) | P2 | `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/metrics` |
| R-24 | `scripts/` | `scripts/accept_sip.sh` (приёмка A1a в трекере) не существует; `scripts/` содержит 9 файлов, из них 3 — одноразовые патчеры PNM (`patch-pnm-audio-in-layouts.py` 19 615 Б, `patch-pnm-chartjs.py`, `pnm_patch_lib.py`), у которых есть свой тест (`test_pnm_patch_lib.py`) в CI | P2 | `ls scripts`; `git ls-files scripts` |
| R-25 | `Cargo.toml` (workspace) | `thiserror = "2"` в workspace-зависимостях, в графе одновременно `1.0.69` и `2.0.21`. Не дефект, но при обновлении `sqlx`/`str0m` легко получить вторую копию «навсегда» | P2 | §4.1 |
| R-26 | `RUSTFLAGS` вне репозитория | **Флаги сборки не являются частью определения сборки.** Наблюдено 2026-10-25 17:16–17:18: в системе работал другой процесс сборки (`ps -o cmd= -ww -p 1506062`): `export PATH="$HOME/.cargo/bin:$PATH" && RUSTFLAGS="-C target-cpu=native" time cargo build --release`. Ни `.cargo/config.toml` (там только комментарии), ни `Cargo.toml:82–92` (`[profile.release]`), ни CI этот флаг не задают. Следствие: один и тот же commit+lock даёт **разные** бинари (sha256) в зависимости от оболочки; `target-cpu=native` привязывает кодоген к конкретному CPU (`E5-2699 v3`, без AVX-512) и запрещает сравнение с CI-бинарём | **P0** (для замеров) | `ps -o cmd= -ww -p <pid>` в момент сборки; `cat .cargo/config.toml`; `grep -n 'target-cpu' Cargo.toml .cargo/config.toml .github/workflows/ci.yml` → пусто |

### 2.1. Атрибуция наблюдённых изменений (поправки Lead, приняты)

| Наблюдение (мой §2/§6) | Фактическая атрибуция | Что это меняет в отчёте |
|---|---|---|
| «Другой агент пересобрал `target/release/vks-server` под работающим сервером» (R-01, §6.1) | **Lead**, намеренная верификация сборки: `cargo build --release`, 4m41s, exit 0 — проверка воспроизводимости тулчейна перед нативным билдом | Не «аномалия», а штатный шаг плана. Вывод усиливается: рестарт **обязателен** до замеров, иначе сервер живёт на старом inode; `md5`/`sha256` — в отчёт замера (§2.0 i.1/i.4) |
| «`target/debug/incremental` (−32 ГБ) исчез между замерами» (R-04) | **Lead**, P0-гигиена без потери `deps`; диск 66→105 ГБ | Числа «86 ГБ → 46 ГБ» — динамика по шагам плана, а не дрейф; в §2.0 i.2 это уже учтено как «до/после» |
| Дубли содержимого (8 групп) / 0 `TODO` в коде (§2.2) | Принято Lead как снимающее часть тревоги о «долгах в коде» | Позиция «гигиена-first» сужена до **блокирующих замер** пунктов — см. §2.0: в план до бенчмарка идёт только (i), остальное — после |
| **Живая проверка 17:16–17:20 MSK (добавлено при ответе на поправки)** | Идёт **третья по счёту** пересборка `target/release`: `ps -o cmd= -ww -p 1506062` → `RUSTFLAGS="-C target-cpu=native" time cargo build --release`; в `target/release/deps` пишутся `libvks_server-*.rlib` (17:18), `libvks_admin.rlib` 27,9 МБ (17:18). Бинарь на диске на момент проверки — ещё от 17:06 (sha256 `cf80eeed…`), но будет заменён по завершении линковки | Это ровно та ситуация, от которой защищает §2.0 i.1/i.4/i.5: **без заморозки и паспорта** любой замер снимается с «того бинаря, что успел лечь». Флаг `-C target-cpu=native` нигде в репо не зафиксирован → R-26 |
| **Итог наблюдения (17:20:02)** | За 14 минут — **три разных бинаря**, ни один не совпадает с исполняемым: (1) живой inode (собран 2026-10-23 23:53) md5 `a59ca1bf…` / sha256 `23086454…`; (2) диск 17:06:33 md5 `37006a1d…` / sha256 `cf80eeed…`; (3) диск 17:20:02 (сборка с `RUSTFLAGS=-C target-cpu=native`, `cargo build --release` 17:15:35→17:20:02) md5 `d1c0656a…` / sha256 `8ab81afd…`, size 26 650 832, inode 1478939. `healthz=200` всё время | Это и есть измеряемая цена отсутствия канона сборки: «последний билд» ≠ «живой бинарь» ≠ «билд из CI». Любая серия замеров без паспорта даст три разных baseline — §2.0 i.1/i.4/i.5 |

### 2.2. Что проверено и **не** является регрессией (честная поправка к моей же позиции)

Важно, чтобы позиция не выглядела риторикой: часть гигиены в проекте сделана хорошо.

* **Маркеры мёртвого кода:** в `crates/**` и `client/vks-client-sdk` — **0** `TODO`/`FIXME`/`HACK`/`XXX`/`todo!()`/`unimplemented!()`; `#[allow(dead_code)]` — 5 штук, все с обоснованием (`vks-storage/src/postgres.rs:51`, `sqlite.rs:19,50`, `vks-media-sfu/src/codec.rs:51`, `vks-core/src/transparency.rs:238`). «Долги в коде» живут в трекере, а не в комментариях.
* **Дубли файлов:** среди 860 tracked-файлов всего **8** групп побайтово идентичного содержимого (иконки вендора, 2 пары evidence-JSON, 2 PNG). Дублирование — **не файловое**, а конфигурационное/API-уровневое (см. §1).
* **`.gitignore`:** `git check-ignore` подтверждает `/target/`, `*.db`, `*.log`, `tests/e2e/node_modules/`; `git status` чист, кроме `?? docs/audit/2026-10-25/` (артефакты раунда).
* **protoc:** герметичный (`protoc-bin-vendored 3.3.0`), системный `protoc` в PATH отсутствует — сборка не зависит от системного бинаря (ADR-0031). Проверено: `command -v protoc` → пусто.
* **protobuf/signaling-контракт:** byte-level тесты есть и для signaling-части они сильнее OpenAPI —
  `crates/vks-proto/src/lib.rs:118–131` `layout_metadata_wire_tags_are_stable` пинит точные байты
  (`preset=4 (0x22), mode=5 (0x2a)`), `:95–117` `layout_roundtrip`; envelope-версия —
  `crates/vks-signaling/src/handler.rs:608–613` (`version_mismatch_replaced`). **Требование
  «генерировать OpenAPI» для signaling я снимаю** (спасибо proto-chain, раунд 1); остаётся
  только для admin REST (163 пути).
* **`501` catch-all — решение, а не регрессия:** задокументирован (`api_v3/mod.rs:4`),
  реализован `:483`/`:617` с метрикой `endpoint="unmatched"`; «падение в 404 запрещено:
  UI считает 404 „метод не существует“» (`:622`). R-11 понижен до P2: остаточный риск —
  **порядок** регистрации, а не сам код.
* **Эвиденс soak:** методика t14b (warm-up, wall-clock гейт, `git_dirty=false`, machine report) — образцовая; проблема не в ней, а в том, что **артефакт не записан** (R-20).
* **CI:** 11 джобов, `--locked` везде, отдельный job «путь с пробелом», `--test-threads=1`, `cargo deny --all-features` — это сильнее, чем у 90 % проектов.

## 3. Безопасный план зачистки

Принципы: (1) ничего не удалять, пока не записан baseline и не подтверждено, что процесс
не держит файлы; (2) сначала «сохранить доказательность» (sha256 артефакта), потом чистить;
(3) каждое удаление — отдельным коммитом/шагом с проверкой `git status` **до/после**;
(4) `docs/`, `tests/`, `deploy/`, `evidence/`, ADR — **не трогать**.

### 3.0. Baseline и «стоп-кран» на артефакт (P0)

```bash
cd '/home/pro/Рабочий стол/mcu'
{ git status --porcelain=v1; git rev-parse HEAD; sha256sum target/release/vks-server;
  ls -la /proc/1177177/exe; md5sum /proc/1177177/exe 2>/dev/null; du -sh target target/debug target/release;
  df -h /; ss -lntup; } > /tmp/hygiene-before.txt 2>&1
curl -s -o /dev/null -w 'healthz=%{http_code}\n' http://127.0.0.1:8080/healthz
```
Ожидание: `git status` — только `?? docs/audit/2026-10-25/`; `healthz=200`.

**R-01:** живой процесс работает с удалённого inode ⇒ любой замер относится к неизвестной
сборке. Сохранить доказательство и паспорт артефакта (обязательно для §2.0 i.4/i.5):

```bash
mkdir -p /var/tmp/vks-artifacts-2026-10-25
cp /proc/1177177/exe /var/tmp/vks-artifacts-2026-10-25/vks-server-running-1177177
{ echo "commit: $(git rev-parse HEAD)"; echo "dirty: $(git status --porcelain | wc -l)";
  echo "flags: ${RUSTFLAGS:-<empty>}"; sha256sum target/release/vks-server;
  /home/pro/.cargo/bin/rustc -vV | sed -n '1p;7p'; } \
  > /var/tmp/vks-artifacts-2026-10-25/ARTIFACT-PASSPORT.txt
```

`systemctl --user restart vks-mcu` — **только** после канонической сборки (шаг 2): рестарт
поднимет другой бинарь, чем тот, на котором шли текущие проверки. Иначе — сначала паспорт.

### 3.1. Шаг 1 — канонизировать сборку (P0/P1)

```bash
cargo build --locked --release -p vks-server     # --locked обязателен; фичи — зафиксированы
sha256sum target/release/vks-server              # ожидание: одинаков между двумя сборками
cp target/release/vks-server /var/tmp/vks-artifacts-2026-10-25/vks-server-$(git rev-parse --short HEAD)
```
Флаги (`RUSTFLAGS="-C target-cpu=native"`, R-26) должны попасть в паспорт: сегодня они живут
только в командной строке сборщика и меняют sha256. Рекомендация: `scripts/build-release.sh`
(сейчас `run-local.sh:33` собирает без `--locked`) + перевод CI `build` на него.

### 3.2. Шаг 2 — чистка мусора сборки (P1), точечно

```bash
# 2a. Read-only: сколько мусора и что именно (18 пакетов / 534.8 MB на 17:07)
python3 - <<'EOF'
import re,os,glob
names=set(re.findall(r'^name = "([^"]+)"$',open('Cargo.lock').read(),re.M))
pkgs=sorted({d.rsplit('-',1)[0] for d in os.listdir('target/debug/build')})
stale=[p for p in pkgs if p.replace('_','-') not in names and p not in names]
tot=0
for s in stale:
    sz=sum(os.path.getsize(os.path.join(dp,f)) for d in glob.glob(f'target/debug/build/{s}-*') for dp,_,fs in os.walk(d) for f in fs)
    tot+=sz; print(f'{s:32s} {sz/1e6:8.1f} MB')
print(f'TOTAL {tot/1e6:.1f} MB')
EOF
# 2b. Список на удаление — печатаем, глазами проверяем, потом раскомментируем rm
python3 - <<'EOF' > /tmp/hygiene-stale-dirs.txt
import re,os,glob
names=set(re.findall(r'^name = "([^"]+)"$',open('Cargo.lock').read(),re.M))
for p in sorted({d.rsplit('-',1)[0] for d in os.listdir('target/debug/build')}):
    if p.replace('_','-') not in names and p not in names:
        for d in glob.glob(f'target/debug/build/{p}-*'): print(d)
EOF
cat /tmp/hygiene-stale-dirs.txt
# xargs -d '\n' rm -rf < /tmp/hygiene-stale-dirs.txt
# 2c. Остатки удалённых крейтов (крейтов нет в workspace) и цели, не нужные приёмке
# rm -f target/debug/libvks_gateway.rlib target/debug/libvks_monitoring.rlib
# rm -rf target/debug target/doc target/wasm32-unknown-unknown   # 41–84 ГБ + 57 МБ + 225 МБ
```
**Порядок:** 2a/2b до 2c — иначе теряется доказательство «сколько было». `target/release`
**не** трогаем (4,5 ГБ — это измеряемый артефакт).

### 3.3. Шаг 3 — внерабочие артефакты (P2, отдельными командами)

```bash
md5sum /home/pro/.local/bin/vks-server /home/pro/.cache/r5-head-target/debug/vks-server
# rm -f  /home/pro/.local/bin/vks-server        # 26 476 640 B, Oct 23 22:13, md5 4a5d47f7…
# rm -rf /home/pro/.cache/r5-head-target        # 6,5 ГБ
git worktree prune; git worktree list; git stash list
git stash show -p stash@{0} | head -40          # посмотреть diff, потом решать
```
**Не автоматизировать:** `git stash drop`, `git worktree remove --force` — стэш
`temp-taskwork` может содержать чужую незакоммиченную работу.

### 3.4. Шаг 4 — слить конфиги в один канон (P1, требует коммита)

Порядок (каждый пункт — отдельный коммит, не один «большой» диф):

1. `crates/vks-common/src/config.rs`: привести `Default` к значениям `configs/default.toml`
   **или** сделать TOML обязательным (`load(None)` → ошибка). Рекомендация — второе:
   fail-closed лучше тихого `443`.
2. Добавить тест `config_parity`: `keys(configs/default.toml) == fields(Config)` (сейчас 16 == 16,
   проходит) **и** значение каждого ключа равно `Config::default()` (сейчас падает на 6 ключах —
   это и есть фиксируемая регрессия).
3. `deploy/README.md`: таблица «ключ → единственный носитель» (TOML vs env) для
   `jwt_secret`, `admin_password`, `database_url`, `candidate_addr`, `frontend_dir`.
4. `deploy/systemd/vks-mcu.service`: лог/БД из рабочего дерева → `%h/mcu/var/`
   (`RuntimeDirectory`/`StateDirectory`), чтобы S30-чистка не откатывалась (R-10).
5. `docs/rules/BACKEND_RULES.md`: §2.3 порты → факт; §6.2 `panic` → `unwind`; §1.1/§1.3 —
   оставить как требование, но сослаться на `C1`/ADR-хэша.

### 3.5. Шаг 5 — API-канон (P1, **после** бенчмарка)

Поправки после ответа proto-chain (раунд 1): `501` не трогаем, signaling-OpenAPI не требуем.

1. `/api/v1/*` — **deprecation, не удаление** (обоснование и цифры — §8.B):
   middleware с `Deprecation: true`, `Sunset: <дата>`, `Link: </api/v3/...>`; сохранить
   функциональность; добавить предупреждение в лог на каждом запросе.
2. Через 2 недели нулевого не-тестового трафика (`vks_http_requests_total{class="api_v1"}`
   не растёт) — `410 Gone` + ADR «каноническое поколение API = v3» и обновление
   `BACKEND_RULES` §7 (сейчас он предписывает `/api/v1`).
3. `/api/v3/*` — **оставить 501** (осознанное решение адаптера, `api_v3/mod.rs:4,622`);
   вместо смены кода — тест «catch-all зарегистрирован последним: любой новый JSON-роут
   матчится раньше него» + метрика `endpoint="unmatched"` уже есть.
4. Контракт-тесты (дешёвые, без генерённого OpenAPI): инвентарь роутов
   `tests/api_routes_inventory.rs` (163 пути) и «`/pnm/<json-путь>` не отдаёт `text/html`»;
   `/pnm/monitoring/sessions` (если будет JSON) регистрировать **до** `frontend.router()`
   (`lib.rs:178–183`) — это и есть единственное жёсткое требование по порядку.
5. `openapi.json`: переименовать в `openapi.v1.stub.json` + предупреждение в `README`
   (генерация — отдельный ADR, не в этом раунде).

### 3.6. Шаг 6 — что НЕ трогать

| Не трогать | Почему |
|---|---|
| `docs/decisions/*` (переписывать) | ADR не удаляются (`PROJECT_RULES` §4.3). Только статусы `superseded` + дата |
| `docs/progress/evidence/**` (294 файла, 14 МБ) | доказательная база приёмки; на неё ссылается трекер |
| `client/pnm-frontend/deployed/pnm/**` и `MANIFEST.sha256` | вендорские ассеты; правка = рассогласование манифеста (214/214) |
| `tests/e2e/**`, `tests/e2e/node_modules` | E2E-гейт раунда; `node_modules` в `.gitignore`, но нужен локально |
| `configs/prod.example.toml`, `deploy/vks-mcu-prod.service` | prod-шаблоны; правки — только с ревью безопасности |
| `docs/audit/2026-10-25/**` | артефакты текущего раунда |
| `.git/` любые операции записи под работающим сервером | риск потери чужой незакоммиченной работы (2 стэша) |
| `vks_data.db` | живая БД стенда (данные пользователя); только бэкап, не удаление |

### 3.7. Шаг 7 — проверка после (обязательно)

```bash
git status --porcelain=v1 > /tmp/hygiene-git-after.txt
diff /tmp/hygiene-git-before.txt /tmp/hygiene-git-after.txt   # ожидание: пусто
du -sh target target/debug target/release; df -h /
curl -s -o /dev/null -w 'healthz=%{http_code}\n' http://127.0.0.1:8080/healthz   # 200
ss -lntup | grep -E 'vks-server|python3' | wc -l                                 # как было
```

---

## 4. Версии пакетов и зависимостей

Источник: `Cargo.lock` (434 пакета, проверено скриптом), системные бинарники — на ВМ
2026-10-25. Столбец «решение» — моя рекомендация для перф-раунда.

### 4.1. Rust-граф (фактические версии из lock)

| Пакет | Версия (lock) | Роль | Решение |
|---|---|---|---|
| rustc / cargo | **1.94.0** (`1.94.0-x86_64-unknown-linux-gnu active`) | MSRV, тулчейн | **запинить**; `rust-toolchain.toml` уже 1.94.0; ADR-0024 «1.99» исправить (K-14) |
| tokio | **1.53.3** | runtime | не трогать в раунде перфа (смена runtime = новый baseline) |
| axum | **0.8.9** | HTTP | не трогать |
| hyper / tower / tower-http | 1.12.0 / 0.5.3 / **0.6.11** | HTTP, middleware, статика | не трогать (`fs`-фича единственная, ADR-0027) |
| str0m | **0.24.1** | WebRTC SFU | **запинить** (pre-1.0, API churn); фича `str0m` включена в `vks-server` |
| prost / prost-build / protoc-bin-vendored | **0.13.5** / 0.13.5 / 3.3.0 (protoc 36.2) | protobuf + герметичный protoc | **запинить** (ADR-0031; детерминизм codegen подтверждён байт-в-байт) |
| sqlx | **0.8.6** | БД (PG/SQLite) | не трогать; PG 16 в CI — отдельный job |
| serde / serde_json / uuid | 1.0.229 / 1.x / 1.28.0 | сериализация, id (v4/v7) | не трогать |
| jsonwebtoken | **9.3.1** | JWT (HS256) | не трогать до ADR о RS256; см. R-21 |
| ring / rcgen | 0.17.14 / 0.14.10 | крипто (JWT/sha2-стек) и self-signed X.509 (WebRTC) | не трогать |
| aws-lc-rs / aws-lc-sys | 1.18.2 / **0.46.0** | TLS/DTLS (str0m→dimpl) | **запинить**; C-сборка, требует `cc` (есть 15.2.0), `cmake`/`clang` отсутствуют — зафиксировать предусловие (K-20) |
| prometheus | 0.14.0 | метрики | не трогать (`/metrics` = 14 424 Б, 200) |
| figment | **0.10.19** | конфиг | не трогать; поведение дефолтов — см. R-09 |
| pbkdf2 | (в lock) | пароли (PBKDF2-SHA256, 100k) | **не менять** без ADR миграции на Argon2id |
| rusty-opus | **`=1.0.2`** (точный пин) | Opus | не трогать |
| oxideav-scene | **0.1.4** (optional, **выключен**) | «реальный» композитор | **решить**: включить в release или удалить фичу (K-11) |
| oxideav-core/g722/g7231/g728/adpcm | 0.1.40 / 0.0.7 / 0.0.9 / 0.0.8 / 0.0.7 | аудиокодеки MCU | **запинить** (0.0.x — нестабильные) |
| thiserror | 2.0.21 (+ 1.0.69 транзитивно) | ошибки | допустимо; при обновлении зависимостей перепроверить дубль (R-25) |
| anyhow / libc / sha2 / base64 / tokio-tungstenite (dev) | 1.0.104 / 0.2.190 / 0.10.9 / 0.22.1 / 0.29.0 | утилиты, CSP-хэши, тесты WS | не трогать |

**Дубли версий в графе (28 пакетов):** `rand` ×3 (0.8.8, 0.9.5, 0.10.3), `getrandom` ×3
(0.2.17, 0.3.4, 0.4.3), `syn` ×2 (**2.0.119 и 3.0.9** — proc-macro-инфраструктура уже
разъехалась по мажору), `thiserror` ×2, `windows-sys` ×3 (0.48/0.52/0.61), `toml_edit` ×2
(0.22.27 / 0.25.17+spec-1.1.0), `nom` ×2 (7.1.3 / 8.0.0), `hashbrown` ×2, `chacha20` ×2,
`bit-vec` ×2, `untrusted` ×2, `redox_syscall` ×2.
**Значение для перфа:** каждая пара — отдельный код в бинаре (I-cache, размер, link-time);
перед «-O3/LTO-fat» надо сначала убрать дубли, иначе измеряем не то.

### 4.2. Системные пакеты и рантайм

| Компонент | Версия (факт) | Решение / риск |
|---|---|---|
| OS / kernel | Ubuntu **26.04.1 LTS**, `7.0.0-38-generic` | не трогать; зафиксировать в эвиденсе замеров |
| CPU / RAM | 2× Xeon E5-2699 v3, **36 phys cores**, SMT off, 1 NUMA, L3 32 MiB; RAM 56 GiB | **запинить в эвиденсе**: замер без `taskset`/NUMA-фиксации не воспроизводим |
| Rust | 1.94.0 (`~/.cargo/bin`, **не в PATH** non-login shell) | предусловие: `export PATH="$HOME/.cargo/bin:$PATH"` в скриптах |
| ffmpeg / ffprobe | **8.0.1** (`/usr/bin`) | **запинить версию в эвиденсе**; в коде нет проверки min-версии, только capability-probe (`FfmpegCapabilities`) |
| Python (sidecar) | **3.14.4** (`/usr/bin/python3.14`) | ок; venv `--without-pip` — см. R-15 |
| pycrate | **0.8.1** (`requirements.lock`, sha256) | запинить; единственная python-зависимость сайдкара |
| Node / Playwright | **24.21.0** / **1.64.1** (`tests/e2e/package.json`, точный пин) | ок |
| GStreamer | 1.28.2 установлен, **0 упоминаний в коде** | **не считать зависимостью проекта**; убрать из «стека» в доках, чтобы не путать |
| protoc (системный) | **отсутствует** в PATH | ок (герметичный vendored) |
| cmake / ninja / clang | **отсутствуют** | **риск**: `aws-lc-sys` собирается C-кодом; зафиксировать предусловие C-тулчейна в `deploy/README.md` + CI |
| cc (gcc) | 15.2.0 | ок |

### 4.3. EOL / риск безопасности (что известно и что нет)

* **EOL:** ни один компонент не EOL на 2026-10-25 (Ubuntu 26.04 LTS, Rust 1.94, Python 3.14,
  Node 24 LTS, FFmpeg 8.0.1 из дистрибутива). Утверждение «EOL-компонентов нет» —
  **по версиям**, не по CVE.
* **CVE/RustSec — НЕ ПРОВЕРЕНО:** `cargo deny check` не запускался (запрет на сборку/сетевые
  вызовы в моей роли). CI-джоб `deny` есть (`.github/workflows/ci.yml`, `cargo-deny-action@v2` =
  0.20.2). **Требуется прогон** до перф-раунда: `cargo deny check --all-features`.
* **Риски по коду (не по CVE):**
  * HS256 + TTL 24 ч на стенде (R-21), `allow_insecure_defaults=true` (WARN в логе),
    опубликованный dev-секрет (R-22), стенд на `0.0.0.0:8080` без TLS — **P1 для стенда**;
  * PBKDF2 100k вместо Argon2id (K-18) — снижает стоимость офлайн-перебора;
  * SIP UAS (`0.0.0.0:5060`) — без auth/TLS/allowlist по дизайну (отмечено в `deploy/README.md`);
    H.323 UAS (`0.0.0.0:1720`) — plaintext, allowlist задан только в drop-in стенда
    (`127.0.0.1/32,192.168.1.0/24`);
  * pre-release крейты (`str0m 0.24`, `oxideav-* 0.0.x/0.1.x`) — риск API/поведения, не CVE.
* **Не проверено:** `cargo test --workspace` (трекерные «1333/0/8» не воспроизводил), `cargo clippy`,
  наличие обновлений (`cargo outdated`).

---

## 5. Чек-лист гигиены для коммита и критерии приёмки

### 5.1. Чек-лист «одного коммита» (перед каждым коммитом раунда)

```
[ ] git status --porcelain: только ожидаемые пути (нет target/, *.db, *.log, node_modules)
[ ] HEAD зафиксирован; рабочее дерево чистое ИЛИ изменения ровно по задаче
[ ] cargo fmt --all -- --check                                    (exit 0)
[ ] cargo clippy --workspace --all-targets -- -D warnings          (exit 0)
[ ] cargo test --workspace --locked --no-fail-fast                 (0 failed; ignored — известно)
[ ] cargo deny check --all-features                                (exit 0)
[ ] нет новых /api/v3/* без записи в инвентаре роутов
[ ] при правке конфига: keys(Config) == keys(default.toml) и diff-values ⊆ allowlist
[ ] при правке systemd: diff repo-юнита и установленного пуст
       (systemctl --user cat vks-mcu.service | diff - deploy/systemd/vks-mcu.service)
[ ] если менялся perf-код: записан паспорт артефакта (commit + фичи + sha256 бинаря)
[ ] MASTER_TASK_LIST обновлён В ТОМ ЖЕ коммите (PROJECT_RULES §3.1)
[ ] ни один ADR не «изменён молча» (иначе — новый ADR + статус superseded)
```

### 5.2. Критерии приёмки гигиены (Definition of Done)

| # | Критерий | Метрика проверки |
|---|---|---|
| A1 | Артефакт однозначен | `sha256sum target/release/vks-server` совпадает с записанным паспортом; `/proc/<pid>/exe` **не** `(deleted)`; `md5` живого = `md5` на диске |
| A2 | Сборка воспроизводима | две последовательные `cargo build --locked --release -p vks-server` дают одинаковый `sha256` |
| A3 | Конфиг — один канон | тест `config_parity` зелёный; `git grep -c 'listen_addr' crates/ configs/ deploy/` не находит нового дубля |
| A4 | Нет мусора сборки | `target/` ≤ 15 ГБ; `target/debug/build` не содержит пакетов вне `Cargo.lock` (проверка скриптом §3.2 = 0 строк) |
| A5 | Рабочее дерево чисто от рантайма | `vks_server.log`/`vks_data.db*` отсутствуют в корне репо (живут в `~/.local/state` или `var/`) |
| A6 | API-инвентарь | `grep -c '"/api/v3/' crates/vks-admin/src` и `vks-server/src` дают 0 во втором; тест инвентаря зелёный |
| A7 | Неизвестный роут = 404 | `curl -o /dev/null -w '%{http_code}' /api/v3/no-such` → `404` |
| A8 | `/pnm/*` — только файлы | контракт-тест: ни один `/pnm/...` не отдаёт `application/json` |
| A9 | ADR-реестр синхронен | у каждого `accepted` ADR есть либо код, либо строка `superseded by`; `grep -L` по 0006/0010/0011/0013/0014/0015 |
| A10 | Трекер не врёт | PID/порты/пути/счётчики тестов в `MASTER_TASK_LIST` совпадают с фактом (проверка: `pgrep -af vks-server`, `ss -lntup`, `ls scripts/`) |
| A11 | `git status` до/после равен | `diff /tmp/hygiene-git-before.txt /tmp/hygiene-git-after.txt` пусто |
| A12 | Стенд жив | `healthz=200`, `readyz` = `{"cluster":true,"db":true,"gateway":true,"media":true}`, порты `8080/5060/40000/1720` на месте |

## 6. Контраргументы против позиций `media-core` и `proto-chain` и ответы

### 6.1. «Гигиена — не приоритет, приоритет — цифры перфа»

**Ответ.** Цифры уже нельзя приписать артефакту: в 17:06 MSK, пока живой процесс работал,
`target/release/vks-server` был **перезаписан другим бинарём** (inode 1469631, `md5 37006a1d…`),
а процесс продолжил исполнять удалённый inode (`md5 a59ca1bf…`, R-01). Любой замер «до/после»
в таких условиях измеряет разные бинари. Плюс 54 fingerprint-набора в одном `target/release`
(R-03) и общий `CARGO_TARGET_DIR` в методике soak (K-15). Гигиена здесь — **условие
существования цифр**, а не «уборка после».

### 6.2. «Сначала перф, чистота подождёт — она не влияет на скорость»

**Ответ.** Влияет, тремя конкретными путями:
1. **Диск/IO:** `target` занимал 86 ГБ при 66 ГБ свободных из 266 ГБ (75 %); `lto="fat"`/
   `codegen-units=1` требуют ещё больше — риск ENOSPC на середине линковки был реальным.
2. **Время итерации:** 15 957 файлов в `target/debug/deps` + 606 каталогов `build`
   (включая 260 МБ `protobuf-src` от **удалённой** зависимости) — это лишние секунды/минуты
   на каждой пересборке, то есть меньше итераций замера.
3. **Шум измерений:** параллельные сборки в общем `target/` конкурируют за
   `.cargo-build-lock` и грузят CPU; soak-README сам предупреждает, что wall-clock гейт
   чувствителен к нагрузке хоста.

### 6.3. «`oxideav`/композитор — это и есть перф, дайте нам его оптимизировать»

**Ответ (по существу, не по форме).** Оптимизировать сейчас нечего: в поставляемом бинаре
фича выключена (`strings | grep -c oxideav-scene` = `0`), композитор — NN-скейлер
(`compositor.rs:5–7`), а CI-тесты идут на дефолтных фичах (`ci.yml:110`). То есть:
(а) код, который предполагается ускорять, не в артефакте; (б) если его включить — это
новый baseline и новые тесты; (в) если не включать — ADR-0004 и soak-эвиденс описывают
не тот код, что работает. **Выбор «включить/удалить» — это и есть гигиена-решение**,
без него перф-работа начнётся с ложной предпосылки. Стоимость решения: 1 ADR + 1 строка
в `Cargo.toml` + 1 тест.

### 6.4. «`proto-chain`: протокол — hot path, конфиги не важны»

**Ответ (уточнён после раунда 1).** Proto-контракт **защищён лучше, чем я предполагал**:
byte-level тесты `crates/vks-proto/src/lib.rs:118–131` пинят точные байты `Layout`
(`preset=4/mode=5`), envelope-версия проверяется в `crates/vks-signaling/src/handler.rs:608–613`.
Поэтому «генерировать OpenAPI для signaling» — требование снимаю. Что остаётся реальной дырой —
**HTTP-граница admin REST**: 163 уникальных пути в двух крейтах (149 `/api/v3` в `vks-admin`
+ 14 `/api/v1`; плюс 2 v3-роута живут в `vks-server/src/lib.rs:149–175`), `openapi.json` — стаб
на 10 путей `/api/v1`, `/api/v3` недокументирован. Proto-изменения, которые доезжают до UI
через REST, не проверяются машинно: только 383-кнопочный Playwright-краулер, который не ловит
«код/`content-type` сменился». Один тест-инвентарь роутов (163 строки, генерируется один раз)
+ тест «`/pnm/...` не отдаёт JSON» закрывают это без OpenAPI-генератора.

### 6.5. «У нас есть soak-эвиденс с commit и `git_dirty=false` — этого достаточно»

**Ответ.** Почти: методика действительно хорошая (warm-up, отбрасывание первых 3 с,
wall-clock гейт, machine report, `dirty=false`). Не хватает ровно четырёх полей, чтобы
замер стал воспроизводимым: `sha256` бинаря, набор фич, `rustc -vV` + `RUSTFLAGS`/`target-cpu`,
и версия C-тулчейна (для `aws-lc-sys`). Их добавление — ~10 строк в README/JSON. И,
конечно, отдельный `CARGO_TARGET_DIR` на замер (K-15). Это дешевле, чем повторный спор
о цифрах через неделю.

### 6.6. «Чистка `target/debug` — потеря кэша, станет медленнее»

**Ответ.** 32 ГБ `incremental` + 38 ГБ `deps`, из которых 1 754 файла старше 2026-10-20 и
**535 МБ относятся к пакетам, удалённым из `Cargo.lock`** (`protobuf-src`, `openssl`, `nix`,
`stacker`…) — ускорить они не могут по определению. `target/release` (4,1–4,5 ГБ) не трогается.

### 6.7. «Гигиена заблокирует раунд на неделю»

**Ответ.** Предлагаемый объём — **1 рабочий день** (шаги 3.0–3.4 — механические, 0 строк кода;
3.5 — 5 небольших коммитов; 3.6 — 3 теста). И это не «последовательно», а **параллельно**
с перфом при одном условии: **заморозить артефакт** — договориться, что до конца раунда
`target/release/vks-server` не перезаписывается (или каждый замер идёт из своего
`CARGO_TARGET_DIR`), а живой стенд не рестартится без записи sha256. Если заморозку
обеспечить нельзя — тогда да, гигиена строго раньше перфа, потому что без неё замеры
невозможны.

---

## 7. Открытые вопросы (≤5)

1. **Композитор в артефакте.** Включать `oxideav` в release-профиль `vks-server` (тогда
   меняется baseline и нужны тесты на feature-путь) или удалять фичу и помечать ADR-0004
   `superseded`? Решение владельца; влияет на все перф-цели (§2.0 i.6). **Блокирующий для (i).**
2. **Живой стенд — статус.** Lead подтвердил: рестарт запланирован после сборки. Нужен
   явный момент «заморозки»: `systemctl --user restart vks-mcu` → в отчёт замера пишется
   `md5` **нового** `/proc/<pid>/exe`; до этого ни один замер не считается baseline.
3. **`/api/v1` — срок deprecation.** Нужна дата `Sunset` и решение, мигрируются ли 9 ссылок
   в `crates/vks-server/tests/network_integration.rs` на `/api/v3` (иначе тесты будут
   единственным «потребителем» v1 и метрика не обнулится). §8.B, §3.5.
4. **Dev-юнит и рабочее дерево.** Разрешаете перевести `deploy/systemd/vks-mcu.service`
   на `%h/mcu/var/` (лог/БД вне репо) — или dev-лог в репо нужен для текущих Playwright-
   сценариев? (R-10; не блокирует замер.)
5. **CVE-гейт — решено технически, нужно исполнение.** `cargo-deny` **не установлен**
   (`command -v cargo-deny` → exit 2, в `~/.cargo/bin` отсутствует). Точная команда установки
   и оценка времени — §8.D. Прогон `cargo deny check advisories` **не собирает крейты проекта**
   (читает `Cargo.lock`), т.е. запрет «без сборок» не нарушает. Решение Lead: прогнать до
   бенчмарка (рекомендую — это часть (i)) или после.


---

## 8. Ответы на вопросы Lead (A–D) и review правки R-26 (§8.E)

### 8.A. Расхождение дефолтов: точное место, полная таблица, патч, риск

**Точное место.** `crates/vks-common/src/config.rs:193` — `impl Default for Config`;
тело `Self { … }` — строки **195–212**, значения полей — **196–211**. Причина, почему это
опасно, — порядок слияния в `Config::load` (`config.rs:226–239`):
`Serialized::defaults(Config::default())` → `.merge(Toml::file(path))` → `.merge(Env::prefixed("VKS_"))`.
TOML — **оверрай над кодом**, а не самостоятельный источник.

**Полная матрица трёх источников (16 ключей):**

| # | Ключ | `Config::default()` (config.rs:196–211) | `configs/default.toml` | `configs/prod.example.toml` | `deploy/vks.env.example` |
|---|---|---|---|---|---|
| 1 | `server_name` | `vks` | `vks-mcu` ⚠ | `vks-mcu-prod` ⚠ | — |
| 2 | `listen_addr` | `0.0.0.0:443` 🔴 | `0.0.0.0:8080` ⚠ | `0.0.0.0:8080` ⚠ | — |
| 3 | `media_listen_addr` | `127.0.0.1:0` 🔴 | `0.0.0.0:40000` ⚠ | `0.0.0.0:40000` ⚠ | — (порты WebRTC отдельно: `VKS_WEBRTC_PORT_MIN/MAX`) |
| 4 | `database_url` | `""` 🔴 | `sqlite://vks_data.db?mode=rwc` ⚠ | `postgres://vks:CHANGE_ME@…` ⚠ | — (намеренно) |
| 5 | `jwt_secret` | `""` 🔴 | `dev_ec994ad4…` ⚠ | — (только env/credential) | `VKS_JWT_SECRET=` |
| 6 | `jwt_ttl_secs` | `86400` | `86400` | `28800` ⚠ | — |
| 7 | `admin_login` | `admin` | `admin` | `admin` | — |
| 8 | `admin_password` | `""` 🔴 | `admin_password` ⚠ | — (только env) | `VKS_ADMIN_PASSWORD=` |
| 9 | `auth_provider` | `local` | `local` | `local` | — |
| 10 | `frontend_dir` | `client/pnm-frontend/deployed/pnm` | то же | `/opt/vks/frontend/pnm` ⚠ | — |
| 11 | `storage_allow_fallback` | `false` | `false` | `false` | — |
| 12 | `allow_insecure_defaults` | `false` | `true` ⚠ | `false` | `#VKS_ALLOW_INSECURE_DEFAULTS=1` (закомм.) |
| 13 | `ice_servers` | `[]` 🔴 | google STUN ⚠ | свои STUN/TURN ⚠ | — |
| 14 | `candidate_addr` | `None` 🔴 | `""` (= `Some("")`) ⚠ | `""` ⚠ | `# VKS_CANDIDATE_ADDR=…` (закомм.) |
| 15 | `ldap_url` | `""` | `""` | `""` | — |
| 16 | `default_room_mode` | `sfu` | `sfu` | `sfu` | — |

⚠ — ключ есть, значение расходится; 🔴 — код-дефолт небезопасен/неработоспособен
(`443` — привилегированный порт; `media 127.0.0.1:0` — эфемерный; `db=""` — падение
`NoPersistentStorage`; `jwt_secret=""` — 503; `ice_servers=[]` — ICE без STUN).
**Итог: 9 ключей расходятся** (`server_name`, `listen_addr`, `media_listen_addr`,
`database_url`, `jwt_secret`, `admin_password`, `allow_insecure_defaults`, `ice_servers`,
`candidate_addr`); совпадают 7.

**Патч (3 этапа, по возрастанию риска):**

1. **Тест-страж (риск 0).** В `crates/vks-common/src/config.rs` рядом с существующими
   тестами `:595` (`prod example must parse`) и `:659` (`dev config must parse`) добавить
   `default_toml_matches_code_defaults`: `serde_json::to_value(Config::default())` св.
   `toml::from_str(include_str!("../../../configs/default.toml"))`. Сегодня **падает на 9
   ключах** — это и есть фиксируемая регрессия. Дальше либо привести `Config::default()`
   к dev-значениям (тест зелёный), либо явно оставить allowlist из 9 ключей с комментарием.
2. **Strict-загрузка (риск P1, только старт).** `Config::load_strict(path)`:
   требовать присутствия в TOML **14 не-секретных** ключей (исключения — `jwt_secret`,
   `admin_password`: их в `prod.example.toml` нет **по дизайну**, они приходят из env/credential;
   их отсутствие уже ловит `validate()` → `WeakJwtSecret`). Ошибка — `ConfigError::MissingTomlKeys`,
   код выхода **78** (EX_CONFIG, как уже сделано в `main.rs:80–84`).
   `Config::load(None)` **сохранить** — им пользуются тесты `config.rs:357,368,383`.
3. **Лог источника (риск 0, польза высокая).** После `fig.extract()` пройти по 16 ключам
   `fig.find_metadata(key).map(|m| m.name)` и залогировать
   `key=value (source=TOML|env|defaults)`; в release при `source=defaults` — `ERROR`.
   Это ровно та наблюдаемость, которой сейчас нет: сейчас лог пишет только
   `configuration loaded successfully path=Some(...)` (`main.rs:76`).

**Оценка риска регрессии и как снять:**
* Prod-юнит использует `LoadCredential=server.toml:/etc/vks/server.toml` — содержимое вне
  репозитория; strict может сломать старт прода. Снять: `vks-server --check-config <file>`
  (dry-run, печатает отсутствующие ключи и источники) + одна релиз-итерация с
  `--allow-default-keys` (WARN вместо exit).
* `VKS_*`-переменные, не входящие в `Config` (H.323-пути, `VKS_WEBRTC_*`, codec-worker) —
  strict их не касается: проверяется только TOML.
* Drop-in `stand.conf` (`VKS_CANDIDATE_ADDR=192.168.1.93`) остаётся рабочим: env — высший приоритет.
* Проверка на обоих шаблонах уже есть в тестах (`:595`, `:659`) — расширяем их, CI ловит сразу.

### 8.B. `/api/v1` — полный список и доказательство отсутствия потребителей

**14 путей / 24 метод-обработчика** (`crates/vks-admin/src/handlers.rs:734–752`):
`/api/v1/rooms` (GET, POST) · `/api/v1/rooms/{id}` (GET, PATCH, DELETE) ·
`/api/v1/rooms/{id}/live` (GET) · `/api/v1/rooms/{id}/mode` (GET, POST) ·
`/api/v1/mode` (POST) · `/api/v1/users` (GET, POST) · `/api/v1/users/{id}` (GET, PATCH, DELETE) ·
`/api/v1/users/bulk` (POST) · `/api/v1/users/sync-ldap` (POST) · `/api/v1/settings` (GET, POST) ·
`/api/v1/records` (GET) · `/api/v1/records/{id}` (GET, DELETE) · `/api/v1/audit` (GET, POST) ·
`/api/v1/audit/export` (GET).

**Кто вызывает — проверено по слоям:**

| Слой | Упоминаний `/api/v1` | Вывод |
|---|---|---|
| `client/pnm-frontend/deployed/pnm/**/*.js` (561 ссылка на `/api/v3`) | **0** | UI не использует |
| `tests/e2e/*.js` (Playwright) | **0** | E2E не использует |
| `scripts/` (вкл. `smoke_v3.sh`) | **0** | smoke не использует |
| `client/vks-client-sdk` | **0** | SDK не использует |
| **живой `/metrics`** (с 12:23 MSK) | `vks_http_requests_total{class="api_v1"}` = **3** (`GET 401` ×2, `GET 404` ×1) | **из них 1 — мой собственный зонд**; второй 401 и 404 — чужие, разовые |
| для сравнения | `api_v3` = **36 878** запросов, `static` = 12 926, `spa` = 500 | v3 — единственный реальный трафик |
| Rust-тесты | `crates/vks-server/tests/network_integration.rs` — 9 | **единственный «потребитель»** |
| Документация | 8 файлов; `BACKEND_RULES` §7 прямо предписывает `/api/v1` | причина существования — правило, а не клиент |

**Deprecation-план (не удаление сразу):**
1. Middleware на v1-роутере: `Deprecation: true`, `Sunset: <дата>`, `Link: </api/v3/...>; rel="successor-version"`;
   функция не меняется; `WARN` в лог на каждый вызов (сейчас `v1` даже не логируется отдельно).
2. Критерий: 2 недели без роста `class="api_v1"` сверх тестового шума. Метрика уже существует
   (`crates/vks-server/src/lib.rs:205–207`, `classify_path` → `api_v1`), новых зависимостей не нужно.
3. `410 Gone` + конверт `{"error":{"code":410,…}}` + ADR «каноническое поколение API = v3»
   + правка `BACKEND_RULES` §7 (иначе правило будет противоречить коду).
4. Миграция 9 ссылок в `network_integration.rs` на `/api/v3` **или** гейт
   `#[cfg(feature = "legacy-api-v1")]` — иначе метрика никогда не обнулится.

### 8.C. ADR-реестр: таблица supersede (docs-долг, не блокер перфа)

| ADR | Статус сейчас | Факт (проверено 2026-10-25) | Что писать |
|---|---|---|---|
| 0006 | `accepted` (rvoip) | `rvoip` — **0 упоминаний** в `crates/`/`client/`, отсутствует в `Cargo.lock`; SIP реализован в `crates/vks-gateway-sip` (свой стек) | `superseded by 0033 «собственный SIP-стек vks-gateway-sip»` |
| 0010 | `accepted` (Dioxus UI) | `dioxus`/`taffy`/`hadrone-core` — 0 в коде и в lock; фронт — legacy PNM (ADR-0027) | `superseded by 0027` |
| 0011 | `accepted` (Tailwind/dioxus-shadcn) | `tailwind` — 0 в коде; UI вендорский | `superseded by 0027` |
| 0013 | `accepted` (progenitor/utoipa) | `progenitor`/`utoipa` — 0 в коде; `openapi.json` — стаб 473 Б | `superseded by 0027` + новый ADR «OpenAPI: заглушка/инвентарь» |
| 0014 | `accepted` (NATS JetStream) | `crates/vks-cluster/src/bus.rs:1–7` — in-memory pub/sub «заменяющий NATS JetStream»; `nats` в lock нет | `superseded by 0034 «cluster bus: in-memory сейчас, NATS отложен»` |
| 0015 | `accepted` (TURN) | только DTO: `crates/vks-admin/src/settings.rs:49,173,196` (`turn:vks.example:5349`), TURN-клиента/relay нет; `PROJECT_RULES` §8.4 требует TURN | `accepted, not implemented` → новый ADR «TURN отложен до X» |
| 0024 | `accepted` (MSRV) | «1.99 dev/release» не существует: `rust-toolchain.toml` = 1.94.0, CI = 1.94.0, на ВМ 1.94.0 | правка текста (**не** supersede): «MSRV 1.94.0 везде» |
| 0029 | `proposed` (per-viewer composition) | фазы 0–2 реализованы (трекер S21 🟡; `5a98922` fast-path) | `accepted` (или разбить на 0029a/0029b) |

Требование `PROJECT_RULES` §4.4 («решение меняется только новым ADR; старый не удаляется»)
соблюдается именно так: новый файл `0033-*.md`/`0034-*.md` + строка в таблице `DECISIONS.md`
+ в старом ADR — `superseded by`. Объём: ~8 строк, 1 коммит `docs(adr): sync registry`,
ревью не требует кода.

### 8.D. CVE-гейт: `cargo-deny` — команда и время

**Статус:** `cargo-deny` **не установлен**. `command -v cargo-deny` → exit 2 (не найдено),
в `/home/pro/.cargo/bin` отсутствует. При этом `~/.cargo/registry` = **1,1 ГБ** (кэш есть,
часть зависимостей можно взять локально). В CI используется `cargo-deny-action@v2` = **0.20.2**.

**Вариант 1 — без компиляции (рекомендую, ~10–20 с):** скачать prebuilt-бинарь той же версии,
что в CI:
```bash
cd /tmp && V=0.20.2 && \
curl -sSfLO "https://github.com/EmbarkStudios/cargo-deny/releases/download/$V/cargo-deny-$V-x86_64-unknown-linux-musl.tar.gz" && \
tar xzf "cargo-deny-$V-x86_64-unknown-linux-musl.tar.gz" && \
install -m 0755 "cargo-deny-$V-x86_64-unknown-linux-musl/cargo-deny" ~/.cargo/bin/cargo-deny
```

**Вариант 2 — из исходников:** `cargo install cargo-deny --version 0.20.2 --locked`
(собирает сам инструмент, ~150–200 крейтов; на 36 ядрах **2–4 мин**, кэш реестра 1,1 ГБ уже есть).
Установка **не собирает крейты проекта** — это отдельный бинарь.

**Прогон (крейты проекта НЕ собираются, читается `Cargo.lock`):**
```bash
cd '/home/pro/Рабочий стол/mcu' && ~/.cargo/bin/cargo-deny check advisories   # ~10–30 с (git-clone RustSec advisory-db)
~/.cargo/bin/cargo-deny check licenses bans sources
```
`~/.cargo/bin` в `PATH` non-login shell отсутствует → вызывать по полному пути (та же ловушка,
что с `rustc`/`cargo`). Перед прогоном обновить advisory-DB: `cargo-deny fetch` (или первый
`check` сделает это сам, ~30–60 МБ). Замечание: `cargo deny check --all-features` из CI и
локальный `check advisories` смотрят на один и тот же `Cargo.lock`, поэтому результат сопоставим;
разница — в лицензионных правилах (`deny.toml`), которые на advisory не влияют.

### 8.E. Review правки R-26 (внесена в 17:21 MSK, **не мной**)

На момент ответа в рабочем дереве появилась незакоммиченная правка `.cargo/config.toml`
(`git diff`): добавлена секция `[target.x86_64-unknown-linux-gnu] rustflags = ["-C", "target-cpu=native"]`
с комментарием, ссылающимся на R-26. Оценка (это мой профиль, поэтому фиксирую):

✅ **Направление верное:** флаги стали частью определения сборки — `cargo build --release` без env
даёт воспроизводимый sha256 на этом хосте; мой §3.1 «флаги в паспорт» этим закрывается.

⚠ **Пять оговорок до бенчмарка:** (1) `native` невоспроизводим между машинами — CI идёт на
`runs-on: ubuntu-24.04` (`.github/workflows/ci.yml:26`), там CPU другой ⇒ `release`-бинарь CI ≠
стенд; детерминированнее явный `-C target-cpu=haswell` (E5-2699 v3 = Haswell, AVX2). (2) `native`
на машине с AVX-512 даст бинарь, падающий `SIGILL` на Haswell-стенде. (3) Все прежние цифры сняты
**без** `native` (медиа-эвиденс `16.95/5.23` мс — коммиты `62cff91`/`5a98922`) ⇒ после правки это
другой codegen-baseline, «до/после» через границу правки сравнивать нельзя (§2.0 i.4). (4) Комментарий
ссылается на `ARCHITECTURE_MASTER_PLAN.md` — **файла в репо нет** (`find . -name 'ARCHITECTURE_MASTER_PLAN*'`
→ пусто); заменить на `docs/architecture/ARCHITECTURE.md`. (5) Убедиться, что env `RUSTFLAGS` не
остаётся у сборщиков (два источника флагов); фактические флаги — в паспорт
(`docs/audit/2026-10-25/measure/PASSPORT.md` уже ссылается на эту секцию — протокол начал применяться).
