# VKS MCU — мастер-отчёт и пошаговый план (2026-10-28)

> **Исторический снимок (до S6/T3a):** пункты о полном покрытии сетки и `WASTE_WEIGHT` устарели; актуальный статус — `docs/audit/2026-10-28-fix/00_plan_review.md` §8.

Аудит всей цепочки: PNM UI / конструктор → API v3 → SQLite/PostgreSQL → resources/push →
`vks-layout` → MCU engine/compositor/farm → SIP/H.323/WebRTC → CI/конфиги/зависимости.
Режим: **read-only** (агенты писали только свои отчёты `docs/audit/2026-10-28/01..05`), сервис не
перезапускался, `vks_data.db` не изменялась, релиз не собирался.

---

## 0. TL;DR — что на самом деле происходит

1. **Прозрачные окна Codian доказаны end-to-end.** Живой каталог
   `GET /api/v3/ui/video_layouts/presets` (проверено с токеном) отдаёт 119 пресетов, из них 54
   Codian, и **ни в одном слоте нет `role`**. Вендорский редактор рисует фон и иконку только из
   `slot.role` (`slotClass`/`slotIcon`, `chunks/index6.bundle…js`), а CSS даёт фон **только**
   role-классам (`--autofill #715839` и т.д.); у `.layout-grid-slot` и wrapper своего
   `background-color` нет → окно без роли = прозрачное. Наш же конструктор и вендорский
   fallback ставят `role:"AUTO"` — дыра только в Codian-ветке каталога.
2. **Превью ≠ применение.** Клиент перехватывает `POST /video_layouts/calculate`
   (`codian-layouts.js:1722-1739,1768-1780`) и синтезирует геометрию своей формулой; серверный
   эндпоинт при этом живой (`api_v3/mod.rs:354` → `shell.rs:707-766`, live `POST` → 200). Итог:
   два алгоритма сетки дают разные rows×cols для **36 из 100** N (напр. N=43: клиент 7×7, Rust
   9×5; расхождение до **1745 px** на 1920×1080), а синтез ещё и **выбрасывает `role`**.
3. **Входящий SIP/H.323 участник не садится в окно.** ID `sip-<uuid>-in@sip-gateway`
   (`gateways.rs:2720`), `h323-…-in@…` (`:3452`) отбрасывается фильтром «последние 36 символов»
   (`gateways.rs:1279-1286`); UI получает этот ID как `abonent_id` (resources.rs:486-534) → цепочка
   достижима, тесты слепы. **P0.**
4. **H.323: «звонок есть, звука нет».** 616 логов «codec/PT mismatch», из них 376 с
   `expected==received`: реальная причина — пиннинг source-порта (`source_latch=false`,
   `gateways.rs:3667` + строгий порт `gateway_audio.rs:605-610`), лог маскирует её под кодек.
   Отдельно DTMF: события шлются 8-кГц шкалой, SDP для Opus объявляет `/48000` →
   тоны в 6 раз короче или не согласуются вовсе; H.323-DTMF не отправляется.
5. **Геометрия сама по себе — лучшая часть системы:** 0 пересечений/выходов на 6 режимах ×
   N=1..100 × 7 холстах × 6 наборах gap/margin (пересчитано независимым портом, 174/174 golden).
   Это верно для **чистых** функций; клиентская обёртка `formatCalculatedSlots` из-за независимого
   `Math.round` даёт 1 px overlap на 323 из 1602 ширин и выход за холст на 740 (включая 1920) —
   лечится удалением обёртки. Единственная аномалия Rust — `grid.rs` (авто-сетка): при
   N≡1 (mod cols) единственная плитка последнего ряда растягивается на всю ширину (1920×540).
6. **Рантайм-выводам нельзя верить без пересборки:** живой PID 2965299 крутит бинарь
   **25 окт 20:37**, а 7 `.rs` новее (включая `gateway_audio.rs`, `engine.rs`, `grids.rs`),
   незакоммичено 21 файл (+4510/−846). Пересборка `--release --locked` — предусловие всех проверок.

---

## 1. Проверенное состояние (факты, не гипотезы)

| Что | Факт | Как проверено |
|---|---|---|
| Сервис | user-unit `vks-mcu.service`, PID 2965299, `0.0.0.0:8080`, `target/release/vks-server` | `systemctl --user cat`, `ss -ltnp`, `/proc/<pid>/exe` |
| Бинарь | 27 178 544 B, mtime **2026-10-25 20:37**, старт 26 окт 14:20 MSK | `ls/stat`, `ps -o lstart` |
| Исходники | 7 `.rs` новее бинаря; 21 файл незакоммичен | `find -newer`, `git status` |
| UI | PNM `2.2.1.66`, `/pnm/` → 200, 214 файлов, MANIFEST 214/214 OK | curl, `sha256sum -c` |
| Каталог | 119 пресетов = 65 vendor + **54 Codian** (`codian_01..52` + `codian_custom_14/22`), **role отсутствует** | live GET с токеном |
| Calculate | `POST /api/v3/ui/video_layouts/calculate` жив: с токеном 200 и slots, без токена 401 | live curl |
| Раскладки | 6 сохранённых раскладок, ни одной `codian_*`/`vks_*` (по прошлому аудиту 25 окт) | db |
| H.323 | реальный Python-процесс PID 2965383, venv `~/.local/share/vks-mcu/h323-venv`, pycrate 0.8.1 | `ps`, unit env |
| WebRTC | `VKS_WEBRTC_TRANSPORT=1`, UDP 43000-43100 | unit drop-in |
| Метрики | `/api/v3/mcu/farm`, `/metrics` → 200 без токена с localhost | curl |
| Логи | 616 «codec/PT mismatch», 206 участников (131 h323 / 75 sip) | `vks_server.log` |

**Граница достоверности:** всё, что наблюдалось в рантайме — про бинарь 25 окт. Текущие правки
(`farm.rs`, `rtcp.rs`, `engine.rs`, UI-слой) в рантайме **не проверены**.

---

## 2. Полная структура цепочки

Rust workspace: **16 крейтов**, 188 файлов `.rs`, **92 885 строк**.
Ядро по размеру: `gateways.rs` 6297, `engine.rs` 3190, `gateway_video.rs` 2779, `farm.rs` 1487,
`preset.rs` 1438, `compositor.rs` 529, `gateway_audio.rs` 861, `codian.rs` 754, `grids.rs` 705.
UI: `codian-layouts.js` 1833 строки + вендорский бандл (212 файлов, ~26 MB), legacy-сборка 32 файла.

```
UI (Vue 3.5.30 / vue-router 5.0.3 / Pinia, axios)
 ├─ вендорский редактор: layout-editor store (setSlots/editValueField/modifiedValue)
 ├─ проектный слой codian-layouts.js: каталог 54 Codian, превью, перехваты fetch/XHR
 └─ API v3 (/ui/video_layouts/presets, /calculate, /video_layouts CRUD, attach/detach)
        │
 vks-admin (api_v3)  ── resources.rs (payload, COW, merge_slots, size) ─┐
        │                                                              │
 vks-storage (SQLite/PostgreSQL: layouts, links, configuration_documents)
        │
 sessions.rs: push_layout_to_engine (per-room mutex, silent no-op/warn)
        │
 GatewayManager::apply_layout (gateways.rs:1721)
   ├─ assignments: slots[].position (1-based) + UUID из abonent_id/contact_id
   ├─ geometry: Codian/constructor → vks-layout на 1920×1080 (клиентские x/y не перекрывают)
   └─ mcu.apply_layout → vks-media-mcu engine (pending → adopt), LayoutChanged
        │
 vks-media-mcu: seat_video_or_expand → regrid, compositor (SlotRects), farm (общий энкодер)
        │
 vks-gateway-sip / vks-gateway-h323 (python sidecar) / vks-media-sfu (str0m WebRTC)
```

Инварианты, которые надо зафиксировать контрактом:
- позиция окна: UI/API 1-based, `LayoutDescriptor`/MCU 0-based (`position_base`);
- `role` — **только UI-семантика отображения**, MCU её не читает; посадка — `position`+ID;
- `size`/число окон — один источник: параметрический id → `slot_count`, иначе `slots.len()`, иначе
  `size`; два текущих места (`resources.rs:1183-1199`, `gateways.rs:1785-1789`) должны звать один
  хелпер;
- геометрия explicit-схем — **из id на 1920×1080**, клиентские x/y для них не источник истины;
- статус применения: `saved ≠ pending ≠ engine-accepted ≠ frame-confirmed`.

---

## 3. Реестр дефектов (по приоритетам)

### P0
| # | Дефект | Доказательство | Исправление |
|---|---|---|---|
| P0-1 | Codian-слоты без `role` → прозрачные окна, без иконок | live /presets; `shell.rs:785-799`; CSS `index7…css`; `slotClass` в `index6…js`; `setSlots` не нормализует (bundle ~980880) | role в каталоге + нормализация на выборе/чтении; пустым `AUTO`, занятым `PEER` |
| P0-2 | Входящий SIP/H.323 `-in` ID отбрасывается фильтром назначения → участник не в окне | `gateways.rs:1279-1286,1770-1776`; id `:2720,3452`; UI-цепочка `resources.rs:486-534`; тесты `5097-5106` | разбирать ID через `parse_participant_id`/суффикс, а не «последние 36»; тесты на оба `-in` |
| P0-3 | H.323-аудио отбрасывается по source-порту; диагностика лжёт («PT mismatch» при равных PT) | `gateways.rs:3667`; `gateway_audio.rs:605-610,288`; 376 логов класс B | политика `source_latch` (latch после первого валидного PT + reason-код в лог/метрику) |

### P1
| # | Дефект | Доказательство | Исправление |
|---|---|---|---|
| P1-1 | Клиентский синтез `calculate`: для `vks_*` считается своя формула (36/100 N расходятся, до 1745 px), для Codian берётся каталог; независимый `Math.round` даёт **1 px overlap на 323 из 1602 ширин** (320..1921) и выход за холст на 740 (включая 1920) | `codian-layouts.js:1648-1688,1722-1739,1768-1780`; серверный `shell.rs:707-766` (live 200) и Rust-геометрия чисты | удалить синтез, использовать серверный ответ — фикс закрывает и расхождение алгоритмов, и 1 px округления |
| P1-2 | Два алгоритма rows×cols в одном крейте: `grid.rs` 7×7 vs `grids.rs` 9×5 для N=43 | пересчёт 174/174 + JS 600/600; `grids.rs:248-281` | `WASTE_WEIGHT=0.0` (`grids.rs:280`): 0/174 golden меняются, совпадение с клиентом и `balanced_grid` на N=2..100 |
| P1-3 | DTMF: 8-кГц метки при Opus/48000 (тоны ×1/6); при `/8000` не согласуется; H.323-DTMF отсутствует | `gateway_audio.rs:437-452`; `uas.rs:305-311,756-770` | согласовать clock в ответе, отправлять UII в H.323 |
| P1-4 | H.323 late video / camera off→on не обрабатываются | `h323_listener.py:399-449` без event_sink; Rust не знает `channel_opened` | добавить событие + H.245 OLC/повторный open |
| P1-5 | Сохранённые ранее раскладки без role останутся прозрачными | вендор `setSlots` не нормализует; БД не мигрируется | идемпотентная нормализация на read-path + `update_resource` |
| P1-6 | 200/204 при неприменении: нет статуса saved/applied/engine/frame; ветка `warn` вообще недостижима — `engine.apply_layout` всегда `Ok`, `gateways.apply_layout` при отсутствии MCU/пустой раскладке молча возвращает `Ok` | `sessions.rs:491-509`; `engine.rs:1639-1652,1700-1705`; `gateways.rs:1724-1726,1906-1908` | audit+метрика+`x-vks-layout-applied`, без 5xx |
| P1-7 | **Анонимный доступ** к `/metrics`, `/api/v3/mcu/farm`, `/api/v3/gateways/status`, `/api/v3/cluster/status` на `0.0.0.0:8080` (проверено: 200 без токена и локально, и с LAN `192.168.1.93`; каталог/calculate при этом 401) | `lib.rs:168-175,193` — farm/metrics смонтированы вне auth-слоя `api_v3`; IP-проверки в коде нет | закрыть auth-middleware (или loopback-only для `/metrics`), тест «anon → 401/403» |

### P2
| # | Дефект | Доказательство | Исправление |
|---|---|---|---|
| P2-1 | auto-expand (14-й паблишер) меняет геометрию, сбрасывает preset, **не эмитит LayoutChanged** | `engine.rs:164-186,712-746,1411-1432` vs `400-406` | эмитить событие при геометрическом изменении |
| P2-2 | Гонка холста: два моста задают размер → `invalidate_room` → `Closed` → `break`, видео мертво до конца вызова | `gateway_video.rs:573-614,777-781` | не ретирить группу из output-таска; пересоздание per-room |
| P2-3 | farm кодирует до 1.25×base при объявленном `b=AS=base`/max-br | `rtp_bridge.rs:651`; `gateways.rs:1461-1462`; `gateway_video.rs:514-532` | `min(group_bitrate, negotiated_max_br)` |
| P2-4 | `/settings/video` hot reload не доходит до живых мостов; width/height на ходу невозможны | `gateways.rs:623-637`; `gateway_video.rs:741-760`; `engine.rs:1078-1096` | поколение настроек + re-key; размер — «со следующего вызова» |
| P2-5 | `grid.rs`: N≡1 (mod cols) → плитка на всю ширину (1920×540 / 1921×156) | `grid.rs:78-84`; достижимо в auto и `regrid_for_video` | центрировать неполный ряд (как `grids.rs::gallery`), тест обновить |
| P2-6 | attach сверх `size` дописывает position вне окна | `resources.rs:836-880` | 409/ошибка без записи |
| P2-7 | дубли `position` в слотах: второй участник молча без окна | `gateways.rs:1775-1778`; `engine.rs:230-236` | детерминированный перенос в свободный слот + warn |
| P2-8 | гонка `loadLayoutCount`: ответ A перезаписывает открытую B; `failed` каталога терминален | `codian-layouts.js:143-161,383-419` | сверка key в `.then`, сброс failed по visibility/online |
| P2-9 | двойной `setSlots` на клик/Enter | `codian-layouts.js:1214-1218,1397-1401` (плюс вложенные `455-456/1329-1331`) | удалить дублирующие ветки (−8 строк) |
| P2-10 | RTCP PLI/FIR только `debug!`, NACK нет; читатель RTCP только inbound-SIP | `gateway_video.rs:876,901,932`; `gateways.rs:2801` | force-IDR по PLI либо документировать |

### P3
`oxideav` — пустая фича (`pub use` и 0 использований) при CI-джобе; `build_pli/build_fir` мертвы;
`layout_descriptor` с pending без прод-потребителей; `constructorCount` (WT) и
`onConstructorActivate` мертвы; латентный `lib.rs:392` срез без `is_char_boundary`;
`crates/vks-proto/openapi.json` (0 ссылок); `bytes/uuid/tracing/serde` в манифестах без
использования; `target-impl`+`target-proto` 10.6 ГБ вне `.gitignore`.

Дополнение red-team (проверено Lead): **`vks-layout/src/viewer.rs` 215 строк** — `plan_viewer`/
`ViewerPlan` вне крейта не используются, единственная ссылка — re-export `lib.rs:50`;
**mock-кодеки ~85 строк** — `VideoEncoder/VideoDecoder`+`MockVideo*` (`frame.rs:86-126`) вне
`frame.rs` не используются, только собственные тесты `frame.rs:152-191` и re-export `lib.rs:61`;
**44 прямых `env::var("VKS_…")` против 16 полей `Config`** — SIP/H.323/WebRTC/codec/farm нельзя
задать в TOML; `Config::default` (`config.rs:195-210`) слушает `0.0.0.0:443`/`127.0.0.1:0` при
пустом jwt, тогда как `configs/default.toml` — 8080/40000; RTP-порт жёстко `41000-41200`
(`rtp_bridge.rs:20-24`), ICE 43000-43100, `media_listen_addr=40000` — три несогласованные
политики портов.

---

## 4. Целевая архитектура (что должно стать истиной)

1. **Один источник геометрии** — `vks-layout`. `POST /calculate` и `apply_layout` вызывают одну
   функцию; клиент ничего не считает (никакого JS-синтеза). Preview == apply by construction.
2. **Один алгоритм сетки.** `grids::gallery` с `WASTE_WEIGHT=0`; `grid.rs` либо удаляется, либо
   становится тонкой обёрткой над ним (авто-сетка = gallery без pin/dual). Тест-parity
   «id → слоты» прогоняется в CI (node + Rust дают одинаковый JSON).
3. **role — на границе каталога.** Сервер всегда отдаёт `role` для слотов, у которых его нет
   (пустые `AUTO`, занятые `PEER`), и делает то же на read-path/update для старых записей.
   Известные роли (`PEER/ROLL/ACTIVE/PRESENTATION/EMPTY/FIXED`) не перезаписываются.
4. **Назначение участника — по разобранному ID.** Никаких «последних 36 символов»: единый
   `parse_participant_id` (+ суффиксы `-in`, `@sip-gateway`, `@h323-gateway`), уникальность
   позиции, явная политика дублей.
5. **Статус раскладки честный:** `saved` (200) → `pending` (нет комнаты) → `engine-accepted`
   (apply_layout вернул Ok) → `frame-confirmed` (следующий композит собран). API отдаёт статус,
   audit + метрика; 200 не меняется (вендорский контракт).
6. **События:** любое изменение геометрии/состава (включая auto-expand/regrid) эмитит
   `LayoutChanged` с версией; клиент не расходится с кадром молча.
7. **Медиа-политика:** согласованный `b=AS`/max-br — жёсткий потолок для farm;
   `source_latch` — явная политика; RTCP-фидбек либо управляет энкодером, либо это метрика.
8. **Границы:** WebRTC↔SIP/H.323 mixed-room — отдельная задача терминации DTLS-SRTP, не «фикс».

---

## 5. Стек и версии (на 2026-10-28)

### Стенд
| Компонент | Версия |
|---|---|
| ОС | Ubuntu **26.04.1 LTS**, Linux **7.0.0-38-generic** |
| Rust | **1.94.0** (`rustc`/`cargo`), MSRV/edition 2021, targets x86_64 + wasm32 |
| UI | PNM/TrueConf **2.2.1.66**, Vue **3.5.30**, vue-router **5.0.3**, axios 1.13.6 (метка в бандле), Pinia (версия не маркирована) |
| Node/npm | **24.21.0 / 11.19.0** |
| Playwright | **1.64.1** (`tests/e2e/package.json`) |
| Python H.323 | **3.14.4** venv `~/.local/share/vks-mcu/h323-venv`, pycrate **0.8.1** |
| FFmpeg / OpenSSL | **8.0.1-3ubuntu2** / **3.5.5** |
| БД | SQLite `vks_data.db` (production-путь PostgreSQL 16 в CI) |
| CI | ubuntu-24.04, Rust 1.94.0, Python 3.13, PostgreSQL 16 |

### Rust-зависимости (Cargo.lock, 434 записи)
| Группа | Версии |
|---|---|
| async/API | tokio **1.53.3**, axum **0.8.9**, tower 0.5.3, tower-http 0.6.11, futures-util 0.3.34, async-trait 0.1.92, bytes 1.12.1 |
| данные/протокол | serde 1.0.229, serde_json 1.0.154, uuid 1.28.0, ulid 1.2.1, chrono 0.4.45, chrono-tz 0.10.4, prost/prost-build 0.13.5, protoc-bin-vendored 3.3.0 |
| storage/auth | sqlx 0.8.6, jsonwebtoken 9.3.1, pbkdf2 0.12.2, sha2 0.10.9, base64 0.22.1, subtle 2.6.1, rand 0.8.8 (+0.9.5, 0.10.3 в графе) |
| медиа | str0m 0.24.1, rusty-opus 1.0.2, oxideav-g722 0.0.7, oxideav-adpcm 0.0.7, oxideav-g728 0.0.8, oxideav-g7231 0.0.9, oxideav-scene 0.1.4 (optional, 0 использований) |
| наблюдаемость | tracing 0.1.44, tracing-subscriber 0.3.23, prometheus 0.14.0, figment 0.10.19, uncased 0.9.10, thiserror 2.0.21 (+1.0.69), anyhow 1.0.104, libc 0.2.190, ring 0.17.14 |
| wasm/тесты | wasm-bindgen 0.2.129, wasm-bindgen-futures 0.4.79, js-sys/web-sys 0.3.106, proptest 1.12.0, tokio-tungstenite 0.29.0 |

Дубли в графе: `rand` ×3, `getrandom` ×3 (0.2.17/0.3.4/0.4.3), `thiserror` ×2 — норма для
транзитивных, но `rand 0.10` появился из новой зависимости; после чистки манифестов пересчитать.

### Конфиги (прочитаны полностью)
`configs/default.toml` (dev: insecure defaults, SQLite, `default_room_mode="sfu"`),
`configs/prod.example.toml` (fail-closed, JWT из env, PostgreSQL, candidate_addr обязателен),
`.cargo/config.toml` (`target-cpu=native` для стенда → для прода `x86-64-v3`), `rust-toolchain.toml`,
`deny.toml` (cargo-deny 0.20.2), `.rustfmt.toml`, `.github/workflows/ci.yml`, unit
`~/.config/systemd/user/vks-mcu.service` + drop-in `stand.conf`, `deploy/vks.env.example`.
Конфликтов «файл vs env» нет: unit намеренно не задаёт H.323/лимиты, они в env.

---

## 6. Пошаговый план (фазы, критерии, откат)

### Фаза 0. Сделать состояние воспроизводимым (блокер)
1. Закоммитить/ветвить WIP (21 файл), тег `audit-2026-10-28-base`.
2. `cargo build --release --locked -p vks-server`; sha256; сравнить с исходниками; не подменять
   работающий бинарь без окна.
3. Снять read-only снимки: `/presets` (54 Codian, role отсутствует), 6 раскладок из БД, `/metrics`.
4. Изолированный контур: копия `vks_data.db` + отдельный порт; все мутации — только там.
**Критерий:** известен sha256 бинаря = текущие исходники; live-поведение объяснимо.

### Фаза 1. P0 UI: role + каталог + сохранённые раскладки (≈ +40/−15 строк)
1. `shell.rs:785-799` — `"role": "AUTO"` в слотах каталога (для custom_14/22 — по `codian::roles()`,
   если размечено).
2. `codian-layouts.js` — `normalizeRoles(slots)` в `select()`/драфте: нет/неизвестна роль →
   `AUTO` пустым, `PEER` занятым; роли строго UPPERCASE, как ждёт вендор (`FIXED` в его карте
   отсутствует, `SlotRole` сериализуется snake_case — не смешивать); известные роли/ID/геометрию
   не трогать.
3. `resources.rs` — идемпотентная нормализация в `update_resource` (после `merge_layout_slots`) и в
   read-path (`list_linked/get_linked/list_global/get_global`); БД не мигрируется.
4. `codian-layouts.js:143-161` — анти-гонка (`key` в замыкании, сверка в `.then`);
   `:1633` — ретрай каталога после failed (visibilitychange/online).
5. `codian-layouts.js:1214-1218,1397-1401` — убрать дублирующие обработчики (−8).
6. Пересчитать `MANIFEST.sha256` (без импорта: `find . -type f -print0 | sort -z | xargs -0 sha256sum`).
**Тесты:** e2e после выбора `codian_07` у всех 13 окон непустой `background-color` и role-класс;
клик и Enter → ровно один `setSlots`; A→B→A; сохранение/перезагрузка; старая раскладка без role
рендерится, известные роли не изменены.
**Откат:** UI — вернуть файл из git (ServeDir читает с диска); Rust — предыдущий бинарь.

### Фаза 2. P1 геометрия: один алгоритм и parity (≈ −70..−150 строк)
1. `grids.rs:280` `WASTE_WEIGHT = 0.0`; проверить, что 174/174 golden не меняются.
2. `grid.rs:48-89` — центрировать неполный ряд (ширины `base_w`, высота последнего ряда
   `base_h`), иначе при N≡1 (mod cols) единственная плитка = вся ширина (N=3,7,13,21,31,43…;
   N=43 → 1920×156). Обновить инварианты `lib.rs:23`/`rect.rs:8-10`/`grid.rs:42-47` и тесты
   `grid.rs:108-118,139,174,210-225`. `grid.rs` (u32, полное покрытие, N из комнаты) и
   `grids.rs` (f64, gap/margin, id) — **не дубли**: оставить оба, вынести общий chooser rows/cols
   (≈ −30 строк).
3. Удалить клиентский синтез `calculate` (`codian-layouts.js:1648-1711,1722-1739,1768-1780`),
   оставить только сброс кэша N после save; проверить, не остался ли `computeSlots` без вызовов.
4. CI-тест parity: node генерирует `id → slots` и сравнивает с `POST /calculate` / Rust-эталоном.
**Критерий:** preview==apply для всех 100 N, 6 режимов, Codian; 0 пересечений; golden зелёные.

### Фаза 3. P1 назначения и API-контракт (≈ +60)
1. `layout_identifier_has_uuid`/`parse_participant_id` — разбирать `-in`-суффиксы; тесты на
   `sip-<uuid>-in@sip-gateway`, `h323-<uuid>-in@h323-gateway`, Unicode/UUID/посторонние id.
2. Уникальность позиции: при дубле — перенос в свободный слот + warn (не 500).
3. attach сверх `size` → 409; detach реально удаляет ID из payload (оставляет окно `AUTO`).
4. Один хелпер `layout_size(payload)` для `resources.rs` и `gateways.rs`.
**Критерий:** входящий SIP/H.323 садится в своё окно; повторный PATCH стабилен; размер один.

### Фаза 4. P1 шлюзы (SIP/H.323/RTP)
1. `source_latch`: латч после первого валидного PT; reason-код в логе/метрике вместо «PT mismatch».
2. DTMF: согласовать clock в ответе (`telephone-event/<clock>`), отправлять события с той же
   шкалой; H.323 — H.245 UII.
3. H.323 late video: `channel_opened` → event → bridge; camera off/on.
4. RTCP: PLI/FIR → force-IDR (`force_key_frames`/restart группы) или честная метрика; убрать
   `build_pli/fir`.
**Тесты:** RTP-injector (G.722 PT9, чужой порт, PT mismatch), DTMF-офферы /8000 и /48000,
h323bot late-video, освобождение портов/ffmpeg после вызова.

### Фаза 5. P2 состояние, события, безопасность
1. Статус применения: `x-vks-layout-applied` + audit + метрика `vks_layout_apply_total{result}`;
   сделать достижимой (или убрать) мёртвую ветку warn (engine всегда `Ok`); 200 сохранить.
2. Безопасность: закрыть auth-middleware `/metrics`, `/api/v3/mcu/farm`, `/api/v3/gateways/status`,
   `/api/v3/cluster/status` (сейчас 200 anon на LAN); тест «anon → 401/403».
3. auto-expand/regrid → `LayoutChanged`; pending vs applied в snapshot.
4. Гонка холста: не ретирить ферму из output-таска; пересоздание per-room.
5. farm: `min(group_bitrate, max-br)`.
6. video_settings: поколение + re-key; width/height — «следующий вызов».
**Тесты:** 9 детерминированных сценариев из `02_mcu_core.md` §13 (pending/adopt, auto-expand,
canvas race, bitrate≤max-br, Closed, self-view, attach>size, canonical dup, lock-order).

### Фаза 6. Гигиена (небольшими коммитами)
1. Сначала **закоммитить untracked-код**: `farm.rs`, `farm_metrics.rs`, `rtcp.rs` отсутствуют в
   `HEAD 543bc7b` — без них рабочее дерево не собирается (`cargo build` сломан checkout'ом).
   Затем `.gitignore`: `target-impl/`, `target-proto/` (2 строки, сразу); удаление 10.6 ГБ — после
   копирования `target-impl/release/vks-server` (sha256 `05f1ce81…`, 25 окт 21:46) как evidence.
2. Мёртвое: `constructorCount` (1), `onConstructorActivate` (6), `build_pli/build_fir`,
   `layout_descriptor`-pending (P3), `openapi.json` (6.4 КБ), `vks-layout/src/viewer.rs` + re-export
   `lib.rs:50` (215 строк, 0 потребителей вне крейта), mock-кодеки `frame.rs:86-126` + re-export
   `lib.rs:61` (~85 строк, только собственные тесты).
3. Манифесты: убрать неиспользуемые `tracing/uuid/serde/bytes` (11 строк, 12 зависимостей),
   `serde` в media-mcu → dev-deps.
4. `oxideav`: заморозить (убрать CI-джобу `--features oxideav`, пометить `reserved`, инвариант
   `cargo tree -i oxideav-scene`), не удалять — ADR-0004/0029.
5. Конфиги/CI (найдено 17 конфликтов, 13 CI-дыр): двойная портовая политика
   (`media_listen_addr=40000` в SDP vs UDP 43000-43100 в unit — нужен один источник);
   `target-cpu=native` в `.cargo/config.toml` с ложным заявлением о воспроизводимости sha256
   (для прода — `x86-64-v3`); тройной пин тулчейна (rust-toolchain/CI env/actions);
   E2E Playwright отсутствует в CI целиком (22 скрипта, 0 упоминаний), 8 `#[ignore]`-тестов не
   запускаются нигде, Python 3.13 в CI vs 3.14.4 на стенде. Добавить джобы: `manifest` (sha256),
   e2e-роль/цвет, parity calculate, Python 3.14 в sidecar, `cargo-audit`; убрать
   `continue-on-error` у doc после починки ссылок.
6. `target/debug` (44 ГБ) — после финальной release-верификации; release-бинарь не трогать.
**Критерий:** `cargo fmt/check/clippy/test/deny/doc/build --locked` зелёные в изолированном контуре.

### Фаза 7. Оптимизация по измерениям (после P0–P2)
1. Baseline: CPU/RSS, время ожидания локов, задержка аудио/видео, FPS, 1 и несколько комнат.
2. Общий Mutex комнат держится весь композитинг (`engine.rs:640,857-870`) — измерить, потом
   сужать; порядок `rooms→frames→audio` сохранить.
3. farm vs per-session: сравнить на 1/4/8 подписчиках, сохранив self-view и ограничения профилей.
4. Только после цифр — менять архитектуру энкодера.

### Фаза 8. Выпуск
1. `cargo build --release --locked`, sha256, сверка с исходниками и lock.
2. Поэтапное включение: canary-комната → все комнаты; откат = предыдущий бинарь + БД-бэкап.
3. Повторить проверки: /presets role, сохранение/перезагрузка, реальный SIP/H.323-вызов,
   кадры после MCU (не только API).

---

## 7. «Можно ли было меньше кода?» — да, и заметно

| Что | Экономия | Почему не теряем поведение |
|---|---|---|
| Клиентский синтез calculate + `formatCalculatedSlots` | −70 строк (+ возможно `computeSlots` −60..−100) | серверный эндпоинт уже покрывает `codian_*` и `vks_*`, live 200 |
| Два алгоритма сетки (`grid.rs` vs `grids.rs`) | −150..−220 строк и один класс багов | 3 из 4 реализаций дают 7×7; `WASTE_WEIGHT=0` сохраняет golden |
| Мёртвый код (`onConstructorActivate`, `build_pli/fir`, `layout_descriptor`-pending, `openapi.json`, неиспользуемые deps, `viewer.rs`+re-export, mock-кодеки) | −~320 строк, −6.4 КБ, −12 зависимостей | 0 вызовов в репо (grep + cargo metadata + red-team) |
| Дубли `gain()`, SR-билдеров, RTP-циклов AAC | −80..−150 строк | общий валидатор/счётчики уже вынесены |
| Статус применения, нормализация role, reason-коды | **+60..+100** | это цена честности; без неё баги невидимы |

Итог: **≈ −500…−750 строк** при одновременном закрытии P0/P1. Главный вывод: система страдает не
от нехватки кода, а от **двух источников истины** (геометрия, size) и **неявных границ** (role на
UI, ID на MCU, статус применения). Элегантная реализация = удалить дубли и зафиксировать один
контракт на границе, а не добавлять слой.

---

## 8. Агенты: задачи, дебаты, вердикты

Все агенты — read-only по исходникам, писали только свои отчёты. Внешний `subagent` из teammate
недоступен (`depth 2 > maxDepth 1`) — это ограничение рантайма; поэтому у каждого агента дебаты
шли в режиме «внутренний адвокат дьявола + независимая проверка», а Lead провёл свой раунд
контраргументов и перепроверки (включая live-запросы). Два внешних red-team subagent'а были
запущены Lead'ом, но не успели к финалу — их роль закрыта перекрёстной проверкой (см. ниже).

| Агент | Промпт (суть) | Главные результаты | Итог дебатов (кто уступил) |
|---|---|---|---|
| `layout-ui` | Разобрать UI-цепочку role→каталог→редактор→save/reload, регрессии, минимальный патч | Каталог без role; CSS-механика прозрачности; двойной setSlots; гонка A→B; failed терминален; 54=52+2, `slot_count`≠хвост id у 51/52; правило «пустым AUTO, занятым PEER» | Lead принял PEER-правило (setFreeUsers перезаписывает AUTO-занятые); агент принял, что серверный фикс каталога недостаточен для старых записей |
| `mcu-core` | API→storage→sessions/push→engine/compositor, конкурентность, pending, farm | `role` не читается движком; COW/detach/attach; silent save; auto-expand без LayoutChanged; canvas race → Closed; farm 1.25×; порядок локов без инверсий; дубли реестров | Lead понизил 3 тезиса (pending-LayoutChanged, `layout_descriptor`, no-op dialer — не прод); агент сам опроверг их в §9 |
| `geometry` | Rust grids vs JS calculate, 6 режимов×N=1..100, PiP, Codian | 0 пересечений/выходов (174/174 golden, 600/600 JS); 36/100 N расходятся по rows×cols; N=43: 7×7 vs 9×5; 1921px — в `grid.rs`, не в vks_grid_5; PiP-наложения намеренные | Lead снял формулировку «MCU игнорирует x/y» (это контракт explicit-схем); принят консенсус 7×7 через `WASTE_WEIGHT=0` |
| `gateways` | SIP/H.323/WebRTC: ID, RTP/RTCP, DTMF, late video, mixed-room | P0 `-in` фильтр (end-to-end); P0 H.323 source latch + лживая диагностика (616 логов); DTMF 8k/48k и нет H.323-DTMF; late video; RTCP P2; mock-тезис опровергнут | Lead потребовал достижимость — агент доказал цепочку через `resources.rs:486-534`; (b),(c),(f) понижены самим агентом |
| `hygiene` | Мёртвый код/файлы/конфиги, зависимости, версии, CI, «меньше кода» | 7 строк JS-мусора; 11 строк манифестов/12 зависимостей; `openapi.json`; target-* 10.6 ГБ вне gitignore; 16.7 ГБ target/debug; CI-дыры; `oxideav` заморозить; бинарь 25 окт vs 7 файлов новее | Lead **опроверг** «маршрута calculate нет» (live 200, `mod.rs:354`); агент исправил §; `oxideav` — решение «заморозить», не удалять |
| `red-team #1` (внешний subagent, Lead) | Независимо перепроверить 10 ключевых тезисов (role, calculate, геометрия, size, silent save, `-in`, дубли, метрики) | Подтвердил отсутствие role в каталоге (507 слотов, 0 role) и CSS-прозрачность; уточнил: клиент считает только `vks_*`, Codian берёт каталог; нашёл 1 px overlap у клиентской обёртки на 323/1602 ширин и OOB на 740; `apply_layout` всегда `Ok` → ветка warn мертва (полностью бесшумное неприменение); анонимные `/metrics`,`/mcu/farm`,`/gateways/status`,`/cluster/status` на LAN (проверено 200) | Пришёл после финала; Lead сам проверил анонимные эндпоинты (200 на LAN), принял уточнение про Codian и 1 px, добавил P1-7 (security) |
| `red-team #2` (внешний subagent, Lead) | Независимо оспорить гигиену/версии/конфиги | Мёртвые JS-символы, mock-кодеки, `viewer.rs`; `farm/farm_metrics/rtcp` — load-bearing и **отсутствуют в HEAD** (коммитить, не удалять); `Config::default` 443/`127.0.0.1:0` vs `default.toml`; 44 `env::var` vs 16 полей Config; RTP 41000-41200/ICE 43000-43100/media 40000; бинарь `ef37731f…` старше oct-27 | Пришёл после финала; Lead проверил `viewer::`/`MockVideo*` (0 внешних), sha бинарей, 44 env-ключа, порты — принято |

Перекрёстные проверки, заменившие внешний red-team: `layout-ui` ↔ `geometry` (2 раунда, роль и
геометрия), Lead ↔ `mcu-core` (5 контртезисов), Lead ↔ `gateways` (8), Lead ↔ `hygiene` (6,
включая live-проверку маршрута), плюс независимые live-факты Lead: `/presets` без role,
`POST /calculate` 200, 119 пресетов/54 Codian, `codian_07`=13 окон.

**Моё мнение (Lead):** система здорова по геометрии и по большей части инженерии, но сломана на
двух стыках — **UI↔каталог (role)** и **ID↔назначение (‑in)**. Первый фикс — час работы и
закрывает видимый дефект; второй требует теста и правки фильтра. Всё остальное (preview≠apply,
DTMF, H.323 audio, farm) — реальные, но более узкие P1/P2, и их нельзя чинить «оптом» без
изолированного контура и реальных звонков. Переписывать MCU не нужно: нужно убрать дубли
источников истины.

---

## 9. Непроверенное / что нельзя утверждать без следующего шага
- Живой DOM/скриншот после фикса role (нужна авторизованная сессия и запись — только в
  изолированном контуре).
- Какие `width/height` вендор шлёт в `calculate` (в бандле нет вызовов) — влияет только на ≤2 px.
- Реальные SIP/H.323-звонки, кадры после MCU, дропы на терминале (нужен pcap/боты).
- Точная причина 616 отбросов по каждому случаю (лог не различает кодек и порт).
- Эффект общего Mutex комнат и farm vs per-session — только после замеров.
- `cargo udeps/machete` недоступны; список неиспользуемых deps — grep-эвристика, требует
  подтверждения на nightly-раннере.
- Внешний red-team (subagent) пришёл после первого финала: выводы **статические** (grep/stat/
  `cargo metadata`) + вырезанные функции в node и curl. Lead перепроверил ключевое: `viewer::` и
  `MockVideo*` — 0 внешних ссылок; `target/release` sha `ef37731f…` ≠ `target-impl/release`
  `05f1ce81…`; 44 `env::var("VKS_…")`; `RtpPortPool::default()` = 41000-41200;
  `Config::default` = 443/`127.0.0.1:0`/пустой jwt; анонимные `/metrics`,`/mcu/farm`,
  `/gateways/status`,`/cluster/status` → 200 на LAN. Эти правки учтены в фазах 0/1/5/6.
- «0 пересечений/выходов» верно для **чистых** функций `grids.rs`/`grid.rs` на целевом холсте;
  клиентская обёртка `formatCalculatedSlots` даёт 1 px overlap на 323/1602 ширин 320..1921 и
  выход за холст на 740 — это отдельный дефект preview, снимаемый вместе с обёрткой.
- «Rust 9×5 — ошибка» переоценено: разрыв 0.988 балла из-за константы `waste=20`, а не формулы;
  3 из 4 реализаций дают 7×7, MCU auto-путь `grids.rs` вообще не использует.
- `target-impl/release/vks-server` (05f1ce81…, 25 окт 21:46) — единственный post-edit артефакт,
  на который опирается evidence 25 окт; перед удалением каталога скопировать.

---

## 10. ВЫПОЛНЕНО 2026-10-28: прозрачные окна Codian исправлены

**Симптом:** в конструкторе раскладка выбирается, окна канвы рисуются, но без фона и иконок — видны только чипы с номерами (DOM: `class="layout-grid-slot"`, `background-color: rgba(0,0,0,0)`).

**Причина (цепочка):** вендорский `slotClass()`/`slotIcon()` (`index6.bundle-2.2.1.66.js`, offset ~1017629) берут класс и иконку **только** из `slot.role` (6 UPPERCASE-значений), а базовый `.layout-grid-slot` в `index7…css` фона не имеет. Сервер отдавал слоты без `role` в трёх местах: каталог 54 Codian (`shell.rs:785-799`), `calculate` (`shell.rs:755-762`) и сохранённые payload'ы (read/create/update); плюс клиентский `formatCalculatedSlots` вырезал роль. Пустые окна обязаны быть `AUTO` (не `EMPTY`): вендорский `setFreeUsers` заполняет только `AUTO` → `PEER`.

**Изменения:**
* `crates/vks-admin/src/api_v3/shell.rs:698` — `ui_slot_role(z_order)`; `:769` и `:805` — `role` в ответах `calculate` и каталога (`AUTO`, PiP → `PRESENTATION`).
* `crates/vks-admin/src/api_v3/resources.rs:1190` — закрытый словарь `UI_SLOT_ROLES` + `ensure_slot_roles()` (идемпотентно: валидную роль не перезаписывает; неизвестную/пустую достраивает по занятости: `PEER` при `abonent_id/contact_id/call_string_id` в snake_case и camelCase, `PRESENTATION` при `z_order>0`, иначе `AUTO`). Вызовы: `normalize_resource:433` (create), `with_layout_abonents:538` (linked read), `normalize_loaded:995` (boot), `update_resource:1280` (save), `list_global:1344`, `get_global:1377`, `templates:2235`.
* `client/pnm-frontend/deployed/pnm/codian-layouts.js:87` — `uiSlotRole()`, `:103` — `normalizeSlots()`; `:561` — нормализация в `select()`, `:1719` — роль в `formatCalculatedSlots` (перехват preview больше не теряет роль).
* Тесты: каталог/`calculate` проверяют наличие и значение роли (`tests.rs`), unit-тест `ensure_slot_roles_backfills_legacy_payloads` (`resource_tests.rs`), `MANIFEST.sha256` перегенерирован (214/214 OK).

**Проверено:**
* `cargo test -p vks-admin --locked` — 297 passed, 0 failed; `cargo test -p vks-layout --locked` — без провалов.
* Live API: `/presets` — 54 Codian, 507 слотов, роли `{AUTO, PRESENTATION}`; `calculate codian_07` — 13 слотов с `role`; сохранённая 25-слотовая раскладка в list/linked — 25 слотов с `role`.
* Браузер (Playwright, живой UI, ручной режим): все **54/54** схемы — 0 прозрачных окон, число окон совпадает с каталогом; сохранённая 25-слотовая раскладка при повторном открытии — 25/25 непрозрачных; pageerror 0. Скриншот: `docs/audit/2026-10-28/evidence/codian_07_after.png`.
* Сервис перезапущен на новом бинаре `target/release/vks-server` sha256 `94484d42…` (инкрементальная сборка `--release --locked`).

**Осталось (осознанно не в этом фиксе):** клиентский перехват `calculate` всё ещё синтезирует геометрию `vks_*` (двойное округление до 1 px; план — удалить ~87 строк, Фаза 2); сохранённая запись «Codian 07 — 13 окон» имеет `payload.slots=[]`, `mode=auto` — выбор схемы в auto/dynamic и сохранение макроса требуют отдельной проверки (кнопка «Создать» требует получателей — без звонка не воспроизводится); маппинг spec-ролей `custom_14/22` (`fixed→PEER`, `active→ACTIVE`, `rolling→ROLL`) — P2.
