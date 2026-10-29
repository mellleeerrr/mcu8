# T09a — повторное ревью записи (HEAD 661a2cb; T09a 650a55f + fix f04c28b)

- **Дата:** 2026-10-29, 11:31–11:45 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент); проверялся только HEAD, рабочее дерево не менялось.
- **Объект:** HEAD `661a2cb`; T09a-коммит `650a55f`; review-fix `f04c28b`; Storage-контракт `vks-core` из `8af77db`. Предыдущее ревью: [`T09a.md`](T09a.md) (3/10).
- **Полигон:** `git archive 661a2cb` → `/dev/shm/t09a-rereview` (без `.git`, **ноль правок**); независимый probe — `/dev/shm/t09a-probe`; negative control — `/dev/shm/t09a-revert`. `CARGO_TARGET_DIR` — только `/dev/shm/*`, Rust 1.94.0 (`rust-toolchain.toml`), `--locked`, `CARGO_NET_OFFLINE=true`.
- **Чистота:** живая `vks_data.db`, `backups/` и живой репозиторий не открывались; все SQLite-файлы — только в `/dev/shm`, после ревью удалены. PG-execute не выполнялся (нет `VKS_STORAGE_PG_URL`, `docker`, `psql`, порт 5432 не слушается) — как и в первом ревью; PG-путь проверен чтением кода/CI.
- **Pitfalls:** [`docs/audit/2026-10-29/pitfalls/T09a.md`](../pitfalls/T09a.md).

## Вердикт

**T09a нельзя помечать done. Score: 8.0/10.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **3** |
| **P2** | **13** |
| **P3** | **4** |

Главное: HEAD `661a2cb` **самодостаточен** — Storage-контракт уже в дереве, все пять приёмочных команд зелёные (exit 0). P0 первого ревью закрыт: 650a55f исторически несамодостаточен, но это P2-гигиена, а не блокер HEAD. Основные P1 f04c28b (синхронный start, restart-stop, stale-row dedupe, ManageRecording) действительно исправлены и подтверждены реальным SQLite-probe. Однако остаются три P1: короткая (<1 с) завершённая запись навсегда выглядит активной и теряется при следующем start; room-scoped JWT не ограничивает запись; ошибка записи при stop проглатывается и DELETE отвечает 200, оставляя ACTIVE-строку.

---

## §1. Самодостаточность HEAD: реальные прогоны

Из `/dev/shm/t09a-rereview` (чистый архив, правок нет):

| Команда | Exit | Результат |
|---|---:|---|
| `cargo test -p vks-storage --locked` | **0** | 23 lib + 1 `dedup_lock` = **24 passed**, 0 failed |
| `cargo test -p vks-admin --lib --locked` | **0** | **347 passed**, 0 failed |
| `cargo test -p vks-recorder --locked` | **0** | **33 passed**, 0 failed |
| `cargo test -p vks-server --tests --locked` | **0** | **176 passed**, 0 failed, 3 ignored (144 lib + 1 `config_failclosed` + 1 `h265_rtp` + 23 `network_integration` + 7 `operator_policy`) |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **0** | `Finished ... in 1m 08s`, предупреждений нет |

Самодостаточность подтверждена и по истории:

- `git show 650a55f:crates/vks-core/src/ports/storage.rs` — `RecordEntry` всё ещё из 5 полей; record-методы всё ещё с no-op телами (`:294-311`). То есть **сам коммит T09a не собирается**.
- `git show 8af77db:crates/vks-core/src/ports/storage.rs` — `bitrate_kbps/width/height` и обязательные record-методы (`:300-309`). Хунк действительно приземлился в T11a-коммите `8af77db`.
- `git log -S 'async fn upsert_record(&self, record: &RecordEntry) -> Result<(), StorageError>;' -- crates/vks-core/src/ports/storage.rs` → единственный коммит `8af77db`.
- `661a2cb` содержит и `650a55f`, и `8af77db`; архив `661a2cb` собирается целиком. Для HEAD это **P2-гигиена истории** (T09a размазан по `5d46507`/`650a55f`/`8af77db`/`f04c28b`), не P0.

---

## §2. Персистентность: код + committed test + реальный SQLite-probe

### 2.1. Код

`RecorderControl::start` (`crates/vks-server/src/recorder.rs:42-67`) больше не запускает detached task:

- `:59-64` — `storage.upsert_record(&entry).await.map_err(...)`; ошибка возвращается наружу;
- `tokio::spawn` в start-пути отсутствует; `grep -n "tokio::spawn" crates/vks-server/src/recorder.rs` даёт только `:300` (sync-хелпер `stop_recording`, см. P1-3/P2-2).

`RecorderManager::start_recording` (`:145-211`) только вставляет строку в `AppState.records` (`:196-207`), а персистит её вызывающий `RecorderControl::start`. API читает storage-first (`observability.rs:683-689`), поэтому после `await` запись видна сразу.

### 2.2. Committed test

`crates/vks-server/tests/network_integration.rs:2228-2280` — `recording_control_persists_immediately_and_reuses_the_room_slot_after_restart`:

- start → `get_record` без polling (`:2243-2249`);
- restart → start без stop → ровно одна строка (`:2251-2264`);
- restart → stop → `duration_sec >= 1`, активных строк 0 (`:2266-2280`).

Тест настоящий, но использует **`MemoryStorage`** (`:2234`), а не SQLite. Поэтому ниже — независимый probe на реальном файле.

### 2.3. Probe на реальном SQLite (`/dev/shm/t09a-probe`)

Временный integration-test (не коммитился) на `SqliteStorage::init("sqlite:///dev/shm/...db?mode=rwc")`, `RecorderControl` + реальный `McuEngine`, HTTP-слой `vks_admin::api_v3::router`. Вывод:

```text
IMMEDIATE_ROW id=89f480c3-... duration=0 bitrate=2350 1280x720
IMMEDIATE_API count=1 ids=[...]
AFTER_RESTART_START rows=1 ids=[("0e058fd5-...", 0)]
AFTER_RESTART_STOP duration=1 active_rows=0
AFTER_FINALIZED_START total=2 active=1
  ids=[("0e058fd5-...", 1), ("b0302cb5-...", 0)]
```

То есть:

- строка существует **сразу** после `control.start`, без polling; API `GET /api/v3/videorecords` отдаёт её немедленно (`count=1`);
- restart + повторный start заменяет stale-строку: остаётся **одна** активная;
- restart + DELETE финализирует: `duration_sec=1`, `ACTIVE=0`;
- после финализированного restart новый start даёт `total=2`, `active=1` — финализированная история сохраняется.

### 2.4. Ошибки записи

Инъекция через SQLite-триггеры на временном файле:

```text
INSERT_FAIL status=500 Internal Server Error
  body={"error":{"code":500,"description":"Recording could not be started"}}
  storage_rows=0
  in_memory_record=1 recording_running=true

UPDATE_FAIL stop_result=Ok(()) duration_after_stop=0 (0 == still ACTIVE)
```

- **Start:** ошибка **видна** (500), строки в storage нет. Но остаётся side effect: сессия и in-memory запись уже созданы; POST ответил 500, а запись идёт (P2-1).
- **Stop:** ошибка **не видна** — `stop_recording_async` делает `let _ = storage.upsert_record(...)` (`recorder.rs:344`), restart-stop — тоже `let _` (`:79`), sync-хелпер — `tokio::spawn` + `let _` (`:300-302`). DELETE отвечает 200, строка остаётся `duration_sec=0`, то есть ACTIVE. Это **P1-3**.

### 2.5. P1-1: завершённая запись короче 1 секунды

Нормальный stop считает `duration_sec = (meta.duration_ms / 1000) as u32` (`recorder.rs:288`) **без** `.max(1)`; restart-stop, наоборот, использует `.max(1)` (`:78`). `record_json` считает активной любую строку с `duration_sec == 0` (`observability.rs:658`). Probe:

```text
SUBSCOND_STOP duration_sec=0
SUBSCOND_JSON active=true stopped_at=null started_at=1793263229
SUBSCOND_HISTORY new_id=9a41b540-... rows=[("9a41b540-...", 0)]
```

Первая запись была остановлена, но API показывает её активной; следующий start удаляет её как stale (`recorder.rs:50-52`), и финализированная история короткой записи **теряется**. Это прямое нарушение п.4 приёмки («finished rows keep numeric stopped_at») и данных.

---

## §3. RBAC

### 3.1. Роли

`forbid_recording` (`observability.rs:47-53`) использует `Permissions::has(role, ManageRecording)`; матрица (`vks-core/src/participant.rs:253-278`) даёт это право **Moderator + Operator**, Viewer/Participant — нет. Start/stop используют `forbid_recording` (`observability.rs:765,816`).

Probe (HTTP, реальные токены):

```text
RBAC START Operator -> 200 OK
RBAC START Moderator -> 200 OK
RBAC START Participant -> 403 Forbidden
RBAC START Viewer -> 403 Forbidden
RBAC STOP  Operator -> 200 OK
RBAC STOP  Moderator -> 200 OK
RBAC STOP  Participant -> 403 Forbidden
RBAC STOP  Viewer -> 403 Forbidden
```

То есть заявленный матричный RBAC **выполнен** и для start, и для stop.

### 3.2. P1-2: room-scoped JWT не проверяется

`AuthContext.room_scope` — существующий механизм: `sessions.rs:321-331` (`check_permission`) требует, чтобы `room_scope` совпадал с целевой комнатой для комнатных мутаций. Новые recording-хендлеры проверяют только роль и **игнорируют** `room_scope`. Probe с токеном Moderator, выписанным на **чужую** комнату:

```text
ROOM_SCOPE token_room=d1bf907e-... target_room=749d809e-...
  start=200 OK stop=200 OK read=200 OK
```

Это авторизационный bypass через границу комнаты: room-scoped Moderator управляет записью и читает записи другой комнаты. Для сравнения, комнатные мутации sessions такой токен получают 403 (`Forbidden: token room scope mismatch`). Нужен `check_permission(auth, Some(session_id))` (или эквивалент) до вызова порта.

### 3.3. Read-only endpoints

- `GET /api/v3/videorecords` — по-прежнему `forbid` = Operator-only (`observability.rs:699`); probe: `global GET moderator=403`.
- `GET /api/v3/sessions/{id}/videorecords` — **изменён** с `forbid` на `forbid_recording` (`observability.rs:715`), то есть Operator → Moderator+. Probe: `session GET moderator=200 OK viewer=403`. Это не регрессия для Operator, но буквальное требование «read-only endpoints unchanged» не выполнено (P2-4). Для room-scoped токена этот GET также не проверяет scope.

---

## §4. JSON активной и завершённой записи

`record_json` (`observability.rs:654-681`): `active = duration_sec == 0`; `stopped_at` = `null` для активной, иначе `started_at + duration_sec`. Probe:

```json
// active row
{"id":"probe-active","active":true,"started_at":1700000000,"stopped_at":null,
 "payload":{"bitrate":2350,"profile":"1280x720","resolution":[1280,720],"meta":{"size":0}}}
// finished row
{"id":"probe-done","active":false,"started_at":1700000000,"stopped_at":1700000061,
 "payload":{"bitrate":2350,"profile":"1280x720","resolution":[1280,720],"meta":{"size":1234}}}
```

- Для нормальной активной строки — `active=true`, `stopped_at=null`.
- Для завершённой строки `duration_sec=61` — numeric `stopped_at=1700000061`.
- Для завершённой строки с `duration_sec=0` (P1-1) — снова `active=true/stopped_at=null`, что неверно.
- Форма вендорского JSON не сузилась: ключи `id/display_name/started_at/stopped_at/session/video_layout/payload` на месте, `active` — аддитивный ключ; регрессии для UI по форме нет. Legacy-строки с `width=height=0` по-прежнему дают строку `"profile":"null"` и `[0,0]` (P2-7) — это pre-existing, не T09a-регрессия.

---

## §5. estimated_time: профиль + свободный диск, не 3600

Код: `videorecords_estimated_time` (`observability.rs:839-851`) берёт `RecorderProfile::default().estimate_seconds(free_bytes)`; `estimate_seconds` (`vks-recorder/src/profile.rs:48-54`) = `free_bytes / (bitrate_kbps*1000/8)`, `bitrate_kbps` (`:42-45`) = `W*H*fps*0.085/1000`, default 1280×720@30 → **2350 kbps**.

- Unit-test `profile.rs:62-68` (`default_profile_matches_the_spec_bitrate`): 2350 kbps; 1 GiB → 3655 s (в диапазоне 3600..3700). Прогон `cargo test -p vks-recorder` — 33 passed.
- Ручной расчёт на момент probe: `df -Pk /` → `free_bytes=52 085 846 016`; `2350*1000/8 = 293 750` байт/с; `52 085 846 016 / 293 750 = 177 313,52` → **177 313 с**.
- Probe endpoint: `ESTIMATED free_bytes=52085846016 expected=177313 api=177313` — совпало точно. Константа 3600 не используется.
- Замечание P3-1: committed API-test `observability_tests.rs:669-680` проверяет только `!= 3600` и `> 3600`, а не формулу; точная формула покрыта unit-тестом профиля, но не endpoint’ом.

---

## §6. «Тесты настоящие»: negative control с откатом f04c28b

Метод: второй чистый архив `661a2cb` → `/dev/shm/t09a-revert`; обратным патчем откачены **только production-хунки** `f04c28b`:

```text
git diff f04c28b^ f04c28b -- \
  crates/vks-admin/src/api_v3/observability.rs \
  crates/vks-server/src/recorder.rs | git apply -R
```

Тесты f04c28b и ре-экспорт `RecorderControl` оставлены, чтобы они компилировались и падали именно на поведении. После отката: `tokio::spawn` в start, нет stale-dedupe/restart-finalize, `forbid` вместо `forbid_recording`, нет `active/stopped_at=null`.

| Проверка | Результат на откаченном коде |
|---|---|
| `cargo test -p vks-server --test network_integration recording_control_persists_immediately_and_reuses_the_room_slot_after_restart` | **FAILED, exit 101**, `network_integration.rs:2246: persisted immediately` — fire-and-forget не гарантирует запись |
| debug-вариант того же сценария с `sleep(100ms)` после start | после restart-start `rows=2` (обе `duration=0`) → assertion `rows.len()==1` (`:2263`) упал бы; после restart-stop `rows` обе `duration=0` → assertion `duration>=1`/`active==0` (`:2277-2280`) упал бы |
| `cargo test -p vks-admin --lib recording_control_is_moderator_plus_and_unknown_session_is_404` | **FAILED, exit 101**, `:649: left 403, right 200` — Moderator снова Operator-only |

Вывод: committed-тесты **реальные** и ловят откат persistence-await, stale-dedupe, restart-stop и RBAC. Оговорка: первый прогон negative control без `cargo clean -p vks-server` подхватил stale-артефакт fixed-сборки из общего target и ложно прошёл; после `cargo clean -p vks-server` откаченный код падает детерминированно. Это caveat окружения/кэша, не результат кода.

Что negative control **не** покрывает: stop-ошибку (P1-3), room-scope (P1-2) и active JSON/sub-second (P1-1) — отдельных committed-регрессий на них нет (P2-6).

---

## §7. Открытые P1

### P1-1. Завершённая запись < 1 с выглядит активной и удаляется как stale

- `recorder.rs:288` — `duration_sec = duration_ms/1000` без `.max(1)`; restart-stop использует `.max(1)` (`:78`).
- `observability.rs:658` — `active = duration_sec == 0`; `stopped_at=null`.
- Probe: `SUBSCOND_STOP duration_sec=0` → `active=true/stopped_at=null`; следующий start удаляет строку (`recorder.rs:50-52`).
- Следствие: нарушение п.4 приёмки, потеря финализированной истории короткой записи. Нужен либо `duration_sec.max(1)` на normal stop, либо явный статус/поле `stopped_at`/`finalized` в схеме, а не эвристика `duration==0`.

### P1-2. Room-scoped JWT не ограничивает start/stop/read

- `forbid_recording` (`observability.rs:47-53`) проверяет только роль; `AuthContext.room_scope` не читается.
- `sessions.rs:321-331` уже содержит готовый `check_permission(auth, target_room)`; recording-хендлеры его не вызывают.
- Probe: токен на чужую комнату → `start=200, stop=200, read=200` для target-комнаты.
- Следствие: авторизационный bypass через комнату. Нужно `check_permission(auth, Some(&session_id))` до `control.start/stop` и scope-фильтр в session-list.

### P1-3. Ошибка записи при stop проглатывается: DELETE 200, строка остаётся ACTIVE

- `stop_recording_async` — `let _ = storage.upsert_record(...)` (`recorder.rs:344`).
- restart-stop — `let _` (`:79`); sync-хелпер — `tokio::spawn` + `let _` (`:300-302`).
- Probe с `BEFORE UPDATE ... RAISE(ABORT)`: `stop_result=Ok(())`, `duration_after_stop=0`.
- Следствие: при ошибке storage DELETE отвечает 200, запись не финализирована; «write error surfaces» и prior P1-1 выполнены только для start. Нужно вернуть ошибку наверх (500) и добавить warn/metric; sync-хелпер либо удалить, либо сделать await.

---

## §8. Открытые P2 (13)

1. **Start failure без rollback.** При ошибке upsert API отдаёт 500, но сессия/recorder-участник/in-memory запись уже созданы (`INSERT_FAIL: storage_rows=0, in_memory_record=1, recording_running=true`). Ретрай идемпотентен, но первый ответ врёт; нужен rollback/компенсация.
2. **Sync `RecorderManager::stop_recording` всё ещё fire-and-forget.** `tokio::spawn` + `let _` (`recorder.rs:296-304`); production API его не использует (только тесты), но инвариант «no detached persistence» не глобальный.
3. **Stale-cleanup слишком широкий и без обработки ошибок.** Удаляются **все** `duration_sec==0` строки комнаты (`recorder.rs:49-52`), включая финализированные короткие (P1-1) и, в multi-instance, активную строку другого узла; `list_records/delete_record` ошибки игнорируются. Restart-stop аналогично финализирует все `duration==0` комнаты (`:75-81`).
4. **Read-only session-list изменён.** `GET /sessions/{id}/videorecords` — `forbid` → `forbid_recording` (`observability.rs:715`), Operator → Moderator+, хотя требование «read-only unchanged». Global list остался Operator.
5. **Историческая атомарность T09a.** `650a55f` не собирается; Storage-контракт в `8af77db` (T11a). HEAD coherent, но T09a размазан по 3–4 коммитам; ревью «одного коммита» невозможно.
6. **Test coverage.** Committed persistence-test — `MemoryStorage`, не SQLite; RBAC-test — только start/Moderator/Viewer, без stop/Participant/Operator/room-scope; нет регрессий на active JSON, sub-second, stop-error.
7. **Legacy JSON.** Для `width==0 || height==0` — `"profile":"null"` (строка) и `resolution:[0,0]` (`observability.rs:670-677`); pre-existing, но T09a менял именно `record_json`.
8. **Профиль/клампы.** `RecorderProfile::new` клампит W/H `2..3840/2..2160` (`profile.rs:35-36`), а pitfalls/зона требовали 160–1920/120–1080; профиль всегда default 1280×720/2350, `settings/profile` из тела только эхо (`observability.rs:768,796-800`).
9. **Одна битая строка ломает список.** `record_from_row` → `Err`; `list_records` — `collect::<Result>`; API молча падает в RAM-fallback (`observability.rs:683-689`), гидратация — `unwrap_or_default` (`state.rs:632`), без `warn!`.
10. **Stop-роут без 404 и с игнорированием `connectionId`.** `videorecords_stop` не проверяет комнату/запись; для неизвестного UUID вернёт 200, а в ответе отдаёт `connection_id`, не record id (`observability.rs:809-831`).
11. **Soak — только арифметика.** `farm_soak_budget`/константы (`farm.rs:281-288`) используются только unit-тестом; release-soak harness `tests/farm_group_soak.rs` отсутствует.
12. **`estimated_time` блокирует async-рантайм.** `df -Pk /` через `std::process::Command` в async-хендлере (`observability.rs:853-861`); `quality` игнорируется. Pre-existing, но T09a менял функцию.
13. **`active_h264_groups` не фильтрует `canvas_version`/`cancel`.** `recorder.rs:89-99` смотрит только `room + H264 + viewers>0`; `GroupStat` не отдаёт `cancel` (`farm.rs:292-309`). Риск низкий (retire синхронный), но семантика «активная группа» слабее pitfalls T5.

## §9. Открытые P3 (4)

1. **Слабый API-test `estimated_time`.** `observability_tests.rs:669-680` проверяет только `!=3600` и `>3600`; точная формула покрыта unit-тестом профиля, но не endpoint’ом.
2. **500 при start без `reason`/метрики.** Только `tracing::warn!` и generic description (`observability.rs:802-805`); вендор/ops не получают машиночитаемую причину.
3. **Docs/test-name drift.** Pitfalls обещают `start_and_stop_recording_persists_once`, `start_recording_is_idempotent`, `estimated_time_uses_bitrate_not_constant`; в HEAD имена другие/тестов нет.
4. **Commit/scope noise.** `f04c28b` включает прошлый review-документ; T09a-изменения размазаны по `5d46507`/`650a55f`/`8af77db`/`f04c28b`.

---

## §10. Приёмка ↔ факт

| Пункт T09a | Статус | Доказательство |
|---|---|---|
| 1. HEAD самодостаточен, все гейты | **✅** | все 5 команд exit 0; §1; историческая атомарность — P2-5 |
| 2. Persistence: start await, API сразу, ошибка видна | **⚠️** | start — ✅ (`IMMEDIATE_ROW/API`, `INSERT_FAIL 500`); stop-error — ❌ P1-3; sub-second — ❌ P1-1 |
| 3. RBAC: ManageRecording, Viewer/Participant 403, Operator 200 | **⚠️** | роль-матрица ✅ (probe); room-scope ❌ P1-2; read-only session-list изменён P2-4 |
| 4. Active JSON: active/null, finished numeric, UI | **⚠️** | normal active/finished ✅; duration=0 finished ❌ P1-1; форма UI сохранена |
| 5. estimated_time из bitrate+disk | **✅** | `profile.rs:48-54`; unit 2350/3655; probe `177313 == 177313`; §5 |
| 6. Тесты реальные (negative control) | **✅** | revert f04c28b: server `:2246` FAILED, admin `:649` FAILED; debug: rows=2, duration=0; §6 |
| 7. P0/P1/P2/P3, score | **✅** | 0 / 3 / 13 / 4; score 8.0 |

---

## §11. Чистота и ограничения

- Живая `vks_data.db`, `backups/` и живой репозиторий не открывались; все SQLite-файлы — `/dev/shm/t09a-probe-*.db`, удалены; временные integration-тесты — `/dev/shm/t09a-probe` и `/dev/shm/t09a-revert`, после ревью удалены.
- В `/tmp` копий не создавалось. PG-execute не выполнялся: в окружении нет `VKS_STORAGE_PG_URL`, `docker`, `psql`, порт 5432 не слушается; PG-путь проверен статически/CI (как в первом ревью). Это единственная непроверенная исполнением часть.
- Negative control потребовал `cargo clean -p vks-server` из-за stale-артефактов в общем target; после очистки откат падает детерминированно.

---

## §12. Короткий ответ

- **Score: 8.0/10.**
- **P0 = 0, P1 = 3, P2 = 13, P3 = 4.**
- **Можно ли T09a пометить done: нет** — три открытых P1 (P1-1/P1-2/P1-3). После их исправления и committed-регрессий (sub-second stop, room-scope, stop-error) T09a станет кандидатом на 9.5+.
- **Top-3 оставшихся:**
  1. **P1-1:** завершённая запись < 1 с навсегда `active=true/stopped_at=null` и удаляется как stale при следующем start — потеря истории.
  2. **P1-2:** room-scoped JWT не ограничивает start/stop/read — Moderator чужой комнаты управляет записью.
  3. **P1-3:** ошибка записи при stop проглатывается — DELETE 200, строка остаётся ACTIVE.
