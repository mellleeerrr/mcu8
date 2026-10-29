# T08 — повторное ревью #2: SDK `room_mode`, auto-select, rejoin, wasm (HEAD `4686ac8`; fix `7199818`)

- **Дата:** 2026-10-29, 12:23–12:37 MSK.
- **Ревьюер:** независимый delegated correctness-reviewer; production-исходники не менялись, записан только этот файл.
- **Объект:** HEAD `4686ac8` (T08-коммиты `e941ff1` + `018badd`, fix `7199818` — «close T06/T08/T09a re-review blockers»). Предыдущее ревью — [`T08.md`](T08.md) (8.8/10, P1: rejoin-refresh без committed-теста); pitfalls — [`pitfalls/T08.md`](../pitfalls/T08.md); критерии — [`MASTER_TASK_LIST_2026-10-29.md`](../../../progress/MASTER_TASK_LIST_2026-10-29.md):21,46.
- **Полигон:** `git archive 4686ac8 | tar -x -C /dev/shm/t08_pristine` (54 MB, без `.git`/`target`); экспериментальная копия `/dev/shm/t08rr2-exp`; `CARGO_TARGET_DIR=/dev/shm/t08rr2-target`; Rust `1.94.0` из `rust-toolchain.toml`. `/tmp` не использовался, все копии после прогонов удалены.
- **Живая среда:** `vks_data.db` не открывалась (mtime остался `2026-10-29 07:33:29.801017417 +0300`); процессы `:8080`/`:18082` не перезапускались и не нагружались. Все серверные тесты ниже поднимают `127.0.0.1:0` + `sqlite::memory:`.
- **Чистота:** менялся только `docs/audit/2026-10-29/reviews/T08_rereview2.md` в живом дереве; production-код live-репо не тронут.

## Вердикт

**Score: 9.6 / 10. Done: yes.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **4** |
| **P3** | **7** |

Главное: прежний P1 **закрыт полностью и по-настоящему** — в `7199818` добавлен committed-тест `rejoin_refreshes_the_pipeline_from_room_mode`, и оба его плеча защищены: удаление `apply_room_mode(jr.room_mode)` (`conference.rs:375`) валит только этот тест (52 passed, 1 failed, `left: Sfu right: Mcu`), а поломка ветки `Unspecified` валит второе плечо (`left: Sfu right: Mcu`, «unspecified keeps current»). Вся приёмка T08 перепроверена исполняемыми прогонами на `4686ac8`: поле 7 аддитивно, сервер отдаёт per-room effective mode (Carol → MCU при SFU-конфиге), SDK выбирает конвейер на старте и на rejoin, `RoomModeChanged` доходит, wasm-lib-гейт зелёный, §3.2/§2.1 описывают реальное поведение (кроме устаревшего code sample). Остались только P2/P3-долги, не входящие в acceptance T08 (реальный транспорт/декодер, приоритет двух носителей режима, resume, wasm `--all-targets`).

---

## §1. Гейты на pristine `4686ac8`

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/dev/shm/t08rr2-target
cd /dev/shm/t08_pristine
```

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all -- --check` | **0** | без вывода |
| `cargo clippy -p vks-proto -p vks-signaling -p vks-client-sdk --all-targets --locked -- -D warnings` | **0** | `Finished dev profile ...`; предупреждений нет |
| `cargo test -p vks-proto -p vks-signaling -p vks-client-sdk --locked` | **0** | proto **8**, signaling **45**, SDK **53** (было 52; +1 новый rejoin-тест) = **106 passed**, 0 failed, 0 ignored |
| `cargo check -p vks-client-sdk --target wasm32-unknown-unknown --locked` | **0** | `Finished ... in 46.70s` |
| `cargo test -p vks-server --tests --locked` | **0** | lib **145** (+3 ignored), main 0, `config_failclosed` 1, `h265_rtp` 1, `network_integration` **23**, `operator_policy` 7 — **177 passed**, 0 failed |
| `cargo test -p vks-server --locked --test network_integration -- test_signaling_ws_handshake test_package_d_mcu_pipeline_handover_and_pnm_control_events` | **0** | **2 passed**, 0 failed, 0.91s |

**Флейки:** не наблюдались. Полный серверный прогон выполнен дважды (12:26 и 12:31 MSK) — оба раза `177 passed`; известный timing-тест `gateways::media_runtime_tests::gateway_audio_bridges_g722_pt9_with_8khz_timestamp_clock` (из ревью `T08.md`) не флейкнул, повторных прогонов не потребовалось. Расхождение с прошлым ревью (lib 144 → 145) — это `+1` юнит-тест `new_track_kinds` из `4686ac8` (T11a), к T08 отношения не имеет.

## §2. P1 закрыт: committed-тест rejoin-refresh и его negative controls

### 2.1. Что добавлено

- `PushTransport` получил настраиваемый режим: поле `join_room_mode` (`client/vks-client-sdk/src/conference.rs:526`), дефолт `Unspecified` (`:535`), сеттер `set_join_room_mode` (`:540-542`), отдача в `join_room()` (`:602`). Остальные тесты `PushTransport` остались на `Unspecified` — поведение старых тестов не изменилось.
- Новый тест `conference::tests::rejoin_refreshes_the_pipeline_from_room_mode` (`conference.rs:765-792`): старт в SFU → `set_join_room_mode(Mcu)` → чистый rejoin → `media().mode()==Mcu`; затем `set_join_room_mode(Unspecified)` → rejoin → режим остаётся `Mcu`.
- Production-строка под тестом: `self.apply_room_mode(jr.room_mode)` в rejoin-ветке (`conference.rs:375`, ветка `:366-379`).

### 2.2. Positive run

```text
cargo test -p vks-client-sdk --locked rejoin_refreshes_the_pipeline_from_room_mode -- --nocapture
test conference::tests::rejoin_refreshes_the_pipeline_from_room_mode ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 52 filtered out; finished in 2.02s
WALL=2.49 s
```

Полный SDK-набор: `53 passed; 0 failed ... finished in 2.01s`. Тест стоит ~2 с реального backoff (два rejoin'а × `reconnect_delay=1s`, `signaling_client.rs:118,188-197`) — для регрессионного теста acceptable, как и оговорено в задании.

### 2.3. Negative controls (все — на копии `/dev/shm/t08rr2-exp`, затем копия восстановлена и сверена `diff -r`)

| # | Что сломано в копии | Прогон | Факт |
|---|---|---|---|
| 1 | Удалён `self.apply_room_mode(jr.room_mode);` (`conference.rs:375`) | новый тест | **FAILED**: `assertion left == right failed: rejoin must adopt the new mode`, `left: Sfu`, `right: Mcu` (`:783`) |
| 1b | То же + полный SDK-набор | `cargo test -p vks-client-sdk` | **FAILED. 52 passed; 1 failed** — регрессию ловит **только** новый тест |
| 2 | `apply_room_mode`: `_ => {}` заменено на `switch_mode(Sfu)` (`conference.rs:102`) | новый тест | **FAILED**: `assertion ... failed: unspecified keeps current`, `left: Sfu`, `right: Mcu` (`:791`), 2.01s — второе плечо тоже защищено |
| 3 | Возврат `:375`/`:102` к pristine | `diff -r` с pristine + полный набор на pristine | **ok**, 53 passed |
| 4 | Удалён auto-select в `start_async` (`conference.rs:249`) | `join_auto_selects_the_media_pipeline_from_room_mode` | **FAILED**: `MCU room must not render as SFU tracks`, `left: Sfu`, `right: Mcu` (`:488`) — прежний negative control жив |

Вывод: P1 из `T08.md` закрыт. Тест ловит именно production-строку rejoin (а не соседний код), покрывает оба требования — «rejoin adopts new MCU mode» и «Unspecified keeps current» — и является единственным детектором этой регрессии в SDK-наборе.

**Наблюдения (не блокеры):** тест форсирует чистый rejoin вручную, обнуляя приватные `join_nonce`/`nonce_acquired_at` (`conference.rs:779-780,787-788`), а не истечением TTL 60 с; сама rejoin-ветка от способа входа не зависит, но естественный путь «nonce истёк → rejoin» отдельным тестом по-прежнему не покрыт. Также тест ассертит только `media().mode()`, не состояние `Active`/очередь событий — для P1-регрессии достаточно.

## §3. Полная приёмка T08 на `4686ac8`

### 3.1. Proto: поле 7 аддитивно, старый кадр → `Unspecified`

`crates/vks-proto/proto/signaling.proto:82-92`: `vks.media.RoomMode room_mode = 7` добавлен после полей 1–6, номера не двигались. Сгенерированный prost-код (из `OUT_DIR`, checked-in кода нет):

```rust
#[prost(enumeration = "super::media::RoomMode", tag = "7")]
pub room_mode: i32,
```

Тест `crates/vks-proto/src/lib.rs:43-72`, `join_room_result_room_mode_is_an_additive_wire_field`: MCU-roundtrip → `Mcu`; `JoinRoomResult::default()` (поле 7 отсутствует) → `Unspecified (0)`. Изолированный прогон: **1 passed**; в наборе proto **8 passed**. `media.proto:8-12` по-прежнему `UNSPECIFIED=0/SFU=1/MCU=2`, «auto» на проводе нет. Замечание P3-4 ниже: тест не пинит тег байтами и не проверяет отдельного «старого клиента» — тег проверен вручную по generated-коду.

### 3.2. Сервер: per-room effective mode, а не только boot config

- `SignalingHandler::on_join_room` кладёт глобальный `self.config.room_mode` (`crates/vks-signaling/src/handler.rs:339`), `caps` — всегда `None` (`:338`).
- `vks-server` **перезаписывает** `res.room_mode` из `ModeState::effective_mode(&rid)` после `register_participant` (`crates/vks-server/src/lib.rs:1299-1311`), fallback — `SFU` без паники.
- Late-joiner push `RoomModeChanged` добавляется в тот же `replies` при `effective != SFU || switch_epoch > 0` (`lib.rs:1317-1338`).

Исполняемое доказательство (оба теста — **passed** в §1):

1. `test_signaling_ws_handshake`: дефолтная комната `room-beta` → `res.room_mode == Sfu` (`crates/vks-server/tests/network_integration.rs:238-241`) — «smoke Sfu».
2. `test_package_d_mcu_pipeline_handover_and_pnm_control_events`: комната `room-pkg-d` переключена per-room в MCU, `HandlerConfig` — `default_test()` = SFU (`:1141-1148`); поздний **Carol** получает `JoinRoomResult.room_mode == Mcu` (`:1479-1489`, ассерт `:1488`, «per-room mode wins»). Это и доказывает, что boot-конфиг не протекает: без overwrite было бы SFU.
3. Alice/Bob получают `RoomModeChanged(new_mode=Mcu, switch_epoch=1)` (`:1325,1332`) — push-путь сервера живой.

`ServerHello` по-прежнему несёт глобальный режим (`handler.rs:288-289`) — остаточный P2-1/2, не ломающий T08 SDK.

### 3.3. SDK: auto-select на старте и на rejoin

- `apply_room_mode` — единственная конверсия proto→pipeline: `Sfu|Mcu → switch_mode`, `_ → {}` (`conference.rs:98-104`).
- `start_async` применяет снимок **до** `Active` и до регистрации push-хендлера (`conference.rs:249`).
- Rejoin применяет `jr.room_mode` (`:375`); resume — нет (`:338-355`), «keep current».
- Push `RoomModeChanged` → `ModeChanged{"sfu"|"mcu"}` (`:274-286`), `Unspecified` отбрасывается; `drain_events` вызывает `switch_mode` (`:118-126`).

Тесты: `join_auto_selects_the_media_pipeline_from_room_mode` (`:482-496`, MCU→Mcu, SFU→Sfu), `live_mode_push_switches_renderer` (`:657-671`), `rejoin_refreshes_the_pipeline_from_room_mode` (`:765-792`), resume-тест `:900-922` (режим сохраняется). Negative control auto-select — §2.3 #4.

### 3.4. `RoomModeChanged`

Сервер кладёт `[JoinRoomResult, RoomModeChanged]` по порядку (`lib.rs:1317-1338`); SDK регистрирует push-хендлер после join (`conference.rs:266`), поэтому push до регистрации отбрасывается, но снимок уже применён — режим не теряется. Push после регистрации кладёт событие, `drain_events` применяет идемпотентный `switch_mode` (присваивание поля, `media_engine/mod.rs:77-79`). Функционального двойного переключения нет; лишнее UI-событие — P3-1.

### 3.5. wasm32

`cargo check -p vks-client-sdk --target wasm32-unknown-unknown --locked` — **exit 0** (46.70s). Это ровно CI-гейт (`.github/workflows/ci.yml:90`). Расширенный `--all-targets` падает — P3-3, известный долг.

### 3.6. Доки §3.2/§2.1

- `docs/architecture/SIGNALING_PROTOCOL.md:104-111`: поле 7 с типом `vks.media.RoomMode` и комментарием «эффективный режим комнаты (per-room override)» — соответствует проводу и серверу.
- `docs/architecture/ARCHITECTURE.md:60-84`: `ServerHello.room_mode` — режим инстанса, `JoinRoomResult.room_mode` — эффективный режим комнаты (global default + per-room override); «SDK выбирает конвейер по `JoinRoomResult` при входе и rejoin; resume без `JoinRoomResult` сохраняет текущий» — **совпадает с кодом** на этом HEAD. Нумерация в ServerHello-примере (`room_mode=6, caps=7, ice=8`) совпадает с `signaling.proto:54-61`.
- Остаётся устаревший code sample `ARCHITECTURE.md:93` (`match server_hello.room_mode`) — P3-2; §3.2 не описывает rejoin/resume/приоритет носителей — P2-1/P3-6.

## §4. Остаточные P2 (не блокируют T08)

### P2-1. Приоритет двух носителей режима не зафиксирован

`signaling.proto:91` (`JoinRoomResult.room_mode=7`) и `media.proto:48` (`RoomCaps.room_mode=1`) — два носителя одного значения; `JoinRoomResult.caps` всегда `None` (`handler.rs:338`), SDK `caps` не хранит (`signaling_client.rs:17-41`), а `ServerHello` отдаёт **глобальный** режим и caps (`handler.rs:288-289`), поэтому для per-room MCU-комнаты `ServerHello.caps.room_mode` может говорить SFU, пока `JoinRoomResult.room_mode` говорит MCU. Ни proto-комментарий, ни `SIGNALING_PROTOCOL §3.2`, ни тест приоритет не фиксируют. Новый SDK читает только `JoinRoomResult`, поэтому T08 не ломается. Фикс: явный контракт «`JoinRoomResult.room_mode` авторитетен» + тест; либо перестать слать `caps.room_mode`/резолвить per-room на hello (T12).

### P2-2. `HandlerConfig.room_mode` глобальный; per-room знает только `vks-server`

`handler.rs:36,70-72,288,339` используют одно поле конфига; авторитетная per-room подстановка живёт только в `lib.rs:1299-1311`. Любой другой хост `vks-signaling` (WT — T13, тестовые хосты) получит глобальный режим, и `ServerHello` уже расходится с `JoinRoomResult` при override. Фикс: резолв режима на `ClientHello` в handler или централизованный overwrite в обёртке (T12/T13).

### P2-3. В SDK нет proto-декодера/реального транспорта — поле заполняют только моки

`JoinRoomResult.room_mode` — домен-поле (`signaling_client.rs:40`), но в `client/vks-client-sdk/src` нет ни одного `prost::Message::decode` (grep пуст); режим заполняют `MockSignaling` (`signaling_client.rs:207-273`), тестовый `PushTransport` (`:344-359`) и `PushTransport` в `conference.rs:602`. Acceptance «SDK выбирает конвейер» проверен на уровне доменной структуры, не на байтах. Долг фазы 8: при появлении WS/WT-транспорта нужна одна явная конверсия `i32 → RoomMode` с обработкой unknown + байтовый тест.

### P2-4. Resume не узнаёт смену режима; сервер вообще игнорирует `resume_nonce`

Resume-ветка (`conference.rs:338-355`) не несёт `JoinRoomResult` и не читает `ServerHello.room_mode`; SDK-структура `ServerHello` (`signaling_client.rs:17-24`) поля режима не имеет. Более того, сервер `resume_nonce` не читает вовсе (grep по `crates/vks-signaling`/`crates/vks-server`: единственное вхождение — тестовый литерал `handler.rs:485`), т.е. resume сегодня — мок-путь. `SIGNALING_PROTOCOL.md:219` при этом обещает «Reconnect … → `JoinRoomResult` со snapshot» — P3-6. Acceptance прямо разрешает «resume keeping current», поэтому это P2-долг; канонический фикс — `room_mode` в SDK-`ServerHello` (поле 6 уже на проводе) и реализация resume на сервере (T13).

## §5. Остаточные P3

1. **P3-1. Дублирующийся `ModeChanged` после join.** Сервер шлёт `RoomModeChanged` сразу после `JoinRoomResult` (`lib.rs:1317-1338`); SDK enqueue'ит его без дедупа (`conference.rs:274-286`), `drain_events` применяет (`:118-126`). `switch_mode` идемпотентен (`media_engine/mod.rs:77-79`), но UI видит лишнее событие для уже применённого режима. `switch_epoch` (`signaling.proto:215-220`) SDK не читает (единственное вхождение — тестовый литерал `conference.rs:976`). Дедуп по `media.mode()`/epoch — задел T12.
2. **P3-2. Устаревший code sample в `ARCHITECTURE.md`.** Текст §2.1 (`:81-84`) верен, но пример ниже (`:93-95`) всё ещё `match server_hello.room_mode` — противоречит и тексту, и коду (`conference.rs:249,375`).
3. **P3-3. wasm-гейт узкий: `--all-targets` падает.** `cargo check -p vks-client-sdk --target wasm32-unknown-unknown --all-targets --locked` — exit **101**: `error: This wasm target is unsupported by mio` + 48 ошибок `mio` (E0432/E0433/E0425), `could not compile mio`. Причина — dev-dependency `tokio` без target-гейта (`client/vks-client-sdk/Cargo.toml:62-63`); CI проверяет только lib (`ci.yml:90`). Известный долг, не входит в acceptance.
4. **P3-4. Proto-тест не пинит тег и «старого клиента».** `crates/vks-proto/src/lib.rs:43-72` roundtrip-ит через ту же структуру; байтовый пин тега 7 и отдельная legacy-структура («old client ignores tag 7») отсутствуют. Фактический generated-тег `tag = "7"` проверен вручную по `OUT_DIR`.
5. **P3-5. Fallback на SFU при poisoned `mode`-mutex.** `lib.rs:1301-1306`: `lock().ok()` → `unwrap_or(Sfu)`; для MCU-комнаты это неверный ответ, но poisoning означает панику в другом месте — риск низкий.
6. **P3-6. Doc-drift §7 resume.** `SIGNALING_PROTOCOL.md:219` утверждает, что reconnect возвращает `JoinRoomResult` со snapshot; сервер `resume_nonce` не читает (§P2-4). Для T08 не критично (доки §3.2/§2.1 верны), но клиентский разработчик будет введён в заблуждение.
7. **P3-7. `pitfalls/T08.md` устарел без маркера.** Файл закоммичен в `7199818`, но строки `:14,141,223` по-прежнему утверждают, что rejoin игнорирует `jr.room_mode`/теста нет — это состояние до фикса. Стоит добавить «fixed in 7199818» или обновить таблицу ловушек.

## §6. Судьба пунктов предыдущего ревью

| Пункт `T08.md` | Статус на `4686ac8` |
|---|---|
| **P1-1** rejoin-refresh не покрыт тестом | **Закрыт**: committed-тест + два negative control (§2) |
| **P2-1** `Unspecified` fallback не покрыт | **Закрыт**: второе плечо нового теста + mutation-control (§2.3 #2) |
| **P2-2** приоритет носителей не зафиксирован | Открыт → §P2-1 |
| **P2-3** `HandlerConfig` глобальный | Открыт → §P2-2 |
| **P2-4** нет proto-декодера в SDK | Открыт → §P2-3 |
| **P2-5** resume не узнаёт режим | Открыт → §P2-4 (плюс сервер не читает nonce) |
| **P3-1** дубль `ModeChanged` | Открыт → §P3-1 |
| **P3-2** устаревший ARCHITECTURE sample | Открыт → §P3-2 |
| **P3-3** wasm `--all-targets` | Открыт → §P3-3 |
| **P3-4** proto-тест без байтового пина | Открыт → §P3-4 |
| **P3-5** poisoned mutex → SFU | Открыт → §P3-5 |

## §7. Что можно закрыть дальше (не блокеры T08)

1. **P2-1/P2-2:** зафиксировать в proto/§3.2 приоритет `JoinRoomResult.room_mode` и добавить тест; для других хостов — резолв per-room в handler или явный контракт «host обязан overwrite» (T12/T13).
2. **P3-1/P3-2:** дедуп `ModeChanged` по текущему режиму; поправить sample `ARCHITECTURE.md:93`.
3. **P3-3/P3-4:** target-гейт dev-зависимости и байтовый пин тега — по возможности.
4. **P3-7:** обновить `pitfalls/T08.md` или пометить исправленным.

## §8. Гигиена полигона

- `git archive 4686ac8` → `/dev/shm/t08_pristine`; копии `/dev/shm/t08rr2-exp`, target `/dev/shm/t08rr2-target` и логи после прогонов **удалены** (`/dev/shm` не задет; `/tmp` не использовался).
- В live-репо мои изменения — только новый `docs/audit/2026-10-29/reviews/T08_rereview2.md`; остальные `M`/`??` в `git status` появились от параллельных агентов и мной не трогались, production-файлы не изменялись.
- Живая БД и серверы: mtime `vks_data.db` не изменился (`07:33:29.801`), `:8080`/`:18082` не перезапускались; все прогоны — на `127.0.0.1:0` и `sqlite::memory:`.
