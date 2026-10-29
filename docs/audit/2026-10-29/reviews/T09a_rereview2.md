# T09a — повторное ревью #2 записи (HEAD 57c2b4b; fix 9b46c12)

- **Дата:** 2026-10-29, 11:58–12:12 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент); живой репозиторий только читался, менялся только этот файл.
- **Объект:** HEAD `57c2b4b`; fix-коммит `9b46c12` «T09a re-review P1s — scope, stop errors, sub-second duration»; предыдущее ревью — [`T09a_rereview.md`](T09a_rereview.md) (8.0/10, 3 открытых P1).
- **Полигон:** `git archive 57c2b4b` → `/dev/shm/t09a-rr2` (948 файлов, без `.git`, **ноль правок production-кода**); `CARGO_TARGET_DIR=/dev/shm/t09a-rr2-target`; Rust 1.94.0 (`rust-toolchain.toml`), `CARGO_NET_OFFLINE=true`. Временный probe — `crates/vks-server/tests/t09a_probe.rs` в копии, после прогонов удалён; дерево сверено с новым `git archive` (`diff -r` → 0 строк).
- **Чистота:** живая `vks_data.db`, `backups/`, живой `target/` и рабочие процессы не открывались. Все SQLite-файлы — только `/dev/shm/t09a-rr2-probe-*.db`, удалены. В `/tmp` копий не создавалось. PG-execute не выполнялся (нет `VKS_STORAGE_PG_URL`, `docker`, `psql`, порт 5432 не слушается) — PG-путь проверен чтением кода/CI, как и в предыдущих ревью.
- **Pitfalls:** [`docs/audit/2026-10-29/pitfalls/T09a.md`](../pitfalls/T09a.md).

## Вердикт

**T09a по-прежнему нельзя помечать done. Score: 9.0/10.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** |
| **P2** | **14** |
| **P3** | **10** |

Главное: два из трёх прежних P1 закрыты полностью и подтверждены реальным SQLite+HTTP probe с negative control — sub-second stop (`duration_sec=1`, `active=false`, numeric `stopped_at`, строка переживает следующий start) и room scope (матрица ролей + foreign-scope Moderator/Operator; откат `check_permission` снова даёт 200 и валит committed-тест). Третий P1 закрыт только для ошибок **записи**: оба пути stop возвращают 500. Но в restart-finalize путь по-прежнему **проглатывает ошибку чтения** `list_records` и отвечает **200, оставляя ACTIVE-строку** — это новый/оставшийся P1 (probe воспроизведён). Все пять гейтов на pristine-копии зелёные, real-SQLite happy path работает.

---

## §1. HEAD, fix-коммит и гейты

`9b46c12` — предок `57c2b4b`. После него только `018badd` (T08 SDK) и `57c2b4b` (T11a docs); `git diff 9b46c12 57c2b4b --stat` — 7 файлов, среди них **нет** `recorder.rs`, `observability.rs`, `observability_tests.rs`, `recording.rs`. То есть проверяемое поведение — ровно `9b46c12`, а поздние коммиты к T09a отношения не имеют.

На pristine-копии (`/dev/shm/t09a-rr2`, production-файлы не менялись):

| Команда | Exit | Результат |
|---|---:|---|
| `cargo test -p vks-storage --locked` | **0** | 23 lib + 1 `dedup_lock` = **24 passed**, 0 failed |
| `cargo test -p vks-admin --lib --locked` | **0** | **348 passed**, 0 failed (было 347; +1 новый scope-тест) |
| `cargo test -p vks-recorder --locked` | **0** | **33 passed**, 0 failed |
| `cargo test -p vks-server --tests --locked` | **0** | **176 passed**, 0 failed, 3 ignored (144 lib + 1 `config_failclosed` + 1 `h265_rtp` + 23 `network_integration` + 7 `operator_policy`) |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **0** | `Finished ... in 1m 04s`, предупреждений нет |

Суммарно гейты ~5m51s. `9b46c12` самодостаточен: он меняет 4 файла и компилируется; историческая размазанность T09a по `5d46507`/`650a55f`/`8af77db`/`f04c28b`/`9b46c12` остаётся P2-гигиеной (P2-7), не блокером HEAD.

---

## §2. Sub-second stop: P1-1 прежнего ревью закрыт

### 2.1. Код

- `RecorderManager::stop_recording_internal` (`crates/vks-server/src/recorder.rs:293`): `duration_sec: ((meta.duration_ms / 1000) as u32).max(1)` — завершённая запись больше не может иметь `duration_sec=0`.
- Restart-finalize (`:78`) уже использовал `.max(1)`.
- `record_json` (`crates/vks-admin/src/api_v3/observability.rs:646-673`): `active = duration_sec == 0`; для непустой длительности `stopped_at = started_at + duration_sec` (numeric), для активной — `null`.
- Stale-cleanup (`recorder.rs:47-55`) удаляет только `duration_sec == 0`; финализированная короткая строка теперь под фильтр не попадает.

### 2.2. Positive probe (реальный SQLite + реальный `/api/v3` router)

Временный integration-тест: `SqliteStorage::init("sqlite:///dev/shm/...db?mode=rwc")`, `AppState::with_storage`, реальный `RecorderControl`/`RecorderManager`/`McuEngine`, HTTP через `vks_admin::api_v3::router`. Обёртка `ProbeControl` делегирует start/stop в `RecorderControl` и переопределяет только `active_h264_groups()` → 1, чтобы дойти до production-пути персистентности (сам 409-гейт проверен отдельным T09a-тестом).

```text
SUBSCOND_ELAPSED_MS=4
SUBSCOND_DB=[("6554d75f-fb5d-47a0-a648-87b1d3eeac48", 1)]
SUBSCOND_JSON={"active":false,...,"started_at":1793264866,"stopped_at":1793264867,...}
SUBSCOND_NEXT_START=[("6554d75f-...", 1), ("2e300387-...", 0)] new_id=2e300387-...
```

То есть: stop через 4 мс после start сохранил `duration_sec=1`; API отдаёт `active=false` и numeric `stopped_at`; после «рестарта» (новый `RecorderManager`, тот же SQLite-файл) следующий start **не удалил** завершённую строку — в комнате две строки (finished + новый ACTIVE).

### 2.3. Negative control: откат `duration_sec.max(1)`

В pristine-копии заменена ровно одна строка `:293` на `(meta.duration_ms / 1000) as u32`, остальное не тронуто; после прогона файл восстановлен по sha256 (`3c11efc8...`, совпадает с HEAD).

```text
SUBSCOND_ELAPSED_MS=7
SUBSCOND_DB=[("d1c571f5-...", 0)]
SUBSCOND_JSON={"active":true,...,"stopped_at":null,...}
SUBSCOND_NEXT_START=[("1d826098-...", 0)] new_id=1d826098-...
assertion failed: a finished <1s row must store duration >= 1
```

Старое поведение воспроизведено точно: завершённая <1 с запись снова `active=true/stopped_at=null`, а следующий start удаляет её как stale (остаётся только новая строка). Probe ловит именно этот дефект.

---

## §3. Room scope и RBAC: P1-2 прежнего ревью закрыт для start/stop/session-list

### 3.1. Код

- `videorecords_start` (`observability.rs:762`) и `videorecords_stop` (`:813`) вызывают `super::sessions::check_permission(&auth, Some(&session_id))`.
- `check_permission` (`crates/vks-admin/src/api_v3/sessions.rs:321-331`): `require_moderator` + при наличии `room_scope` требует `target_room_id.eq_ignore_ascii_case(scope)`, иначе 403 `token room scope mismatch`.
- `session_videorecords_list` (`observability.rs:700-718`) снова использует `forbid()` (Operator-only, как до T09a) и дополнительно проверяет `auth.room_scope` против `session_id` (403).
- `forbid_recording` удалён; read-only session-list больше не Moderator+.

### 3.2. Матрица (HTTP, реальные JWT, реальный SQLite)

```text
MATRIX START role=Operator    status=200 OK
MATRIX STOP  role=Operator    status=200 OK
MATRIX LIST  role=Operator    status=200 OK
MATRIX START role=Moderator   status=200 OK
MATRIX STOP  role=Moderator   status=200 OK
MATRIX LIST  role=Moderator   status=403 Forbidden
MATRIX START role=Viewer      status=403 Forbidden
MATRIX STOP  role=Viewer      status=403 Forbidden
MATRIX LIST  role=Viewer      status=403 Forbidden
MATRIX START role=Participant status=403 Forbidden
MATRIX STOP  role=Participant status=403 Forbidden
MATRIX LIST  role=Participant status=403 Forbidden
SCOPE foreign_moderator start=403 stop=403 list=403
SCOPE foreign_operator  start=403 stop=403 list=403
SCOPE own_operator      start=200 stop=200 list=200
```

- Start/stop: Operator и Moderator — 200 (матрица `ManageRecording` = Moderator+, `participant.rs:253-278`), Viewer/Participant — 403.
- Read-only session-list: только Operator — 200, остальные — 403; это восстановленное pre-T09a поведение, а не Moderator+.
- Для всех rejected start/stop проверено `storage.list_records(room)` = **пусто**: отклонённые вызовы не создают строк (не только `state.records`).
- Own-scope Operator работает (200/200/200); foreign-scope Moderator и foreign-scope Operator получают 403 на start/stop/list.

### 3.3. Negative control: откат `check_permission`

Обратным патчем `git diff 9b46c12^ 9b46c12 -- observability.rs` возвращены `forbid_recording` и старые проверки (и старый Moderator+ list без scope). После прогона файл восстановлен по sha256 (`f5de04e6...`).

```text
MATRIX LIST role=Moderator status=200 OK      # ожидалось 403
assertion failed: session record list is read-only Operator-only (role Moderator)
left: 200, right: 403

cargo test -p vks-admin --lib recording_control_honours_the_token_room_scope
assertion failed: room-scoped token must not record a foreign room
left: 200, right: 403
```

То есть committed scope-тест **реальный** и ловит откат; probe дополнительно ловит откат Operator-only list.

### 3.4. Остаточная щель: глобальный список не читает `room_scope`

`GET /api/v3/videorecords` (`observability.rs:685-696`) проверяет только `forbid()` (Operator) и отдаёт записи **всех** комнат. Probe с Operator-токеном, выписанным на чужую комнату:

```text
GLOBAL_LIST foreign_scoped_operator status=200 OK count=3
```

Session-list для того же токена даёт 403, глобальный — 200. Это не входит в заявленную матрицу (там foreign-scope Moderator, а он получает 403 по роли), но если в деплое выдаются room-scoped Operator-токены, это чтение метаданных чужих комнат. P2-5; при политике «любой scoped-токен ограничен комнатой» это надо поднимать до P1.

---

## §4. Stop error propagation: write-ошибки закрыты, read-ошибка осталась (P1)

### 4.1. Код

- In-memory stop: `RecorderManager::stop_recording_async` (`recorder.rs:342-353`) после `stop_recording_internal` делает `storage.upsert_record(&entry).await.map_err(|error| RecordingError::Storage(...))?` (`:350`); ошибка доходит до `RecorderControl::stop` (`:89`) и до handler’а.
- Restart-finalize: `RecorderControl::stop` (`:71-88`) на каждый `duration_sec == 0` делает `upsert_record(...).map_err(...)?` (`:82`).
- Handler `videorecords_stop` (`observability.rs:822-827`) на `Err` возвращает `error_json(500, "Recording could not be stopped")`.
- `RecordingError::Storage(String)` добавлен в `crates/vks-recorder/src/recording.rs:56-62`.

### 4.2. Positive probe: инъекция падения UPDATE в реальном SQLite

Перед открытием пула создана схема, затем SQLite-триггер `BEFORE UPDATE ON video_records ... RAISE(ABORT, 'injected stop persistence failure')`; start делает INSERT (триггер не срабатывает), stop — UPSERT→UPDATE (падает).

```text
STOP_INMEMORY status=500 Internal Server Error
  body={"error":{"code":500,"description":"Recording could not be stopped"}}
STOP_INMEMORY_DB=[("8cc2168a-...", 0)]
STOP_DIRECT_INMEMORY err=Err("recording metadata persistence failed: storage error: error returned from database: (code: 1811) injected stop persistence failure")
STOP_RESTART_FINALIZE status=500 Internal Server Error
  body={"error":{"code":500,"description":"Recording could not be stopped"}}
STOP_RESTART_FINALIZE_DB=[("8cc2168a-...", 0)]
STOP_RECOVERED status=200 OK
STOP_RECOVERED_DB=[("8cc2168a-...", 1)]
```

- In-memory stop: DELETE **500**, строка остаётся ACTIVE (`duration=0`) — ошибка видна.
- Restart-finalize: DELETE **500**, строка остаётся ACTIVE — ошибка видна.
- После снятия триггера повторный DELETE (новый `AppState`, тот же файл) — **200**, строка финализирована (`duration=1`). То есть retry-путь рабочий.

### 4.3. P1: restart-finalize проглатывает ошибку **чтения** `list_records`

`RecorderControl::stop` (`recorder.rs:74-87`):

```rust
if let Some(storage) = state.storage.clone() {
    if let Ok(records) = storage.list_records(Some(room_id)).await {
        for mut entry in records.into_iter().filter(|entry| entry.duration_sec == 0) { ... }
    }
}
return Ok(());
```

Если `list_records` вернул `Err` (БД недоступна, таблица/схема сломана, ошибка чтения), цикл просто пропускается, и функция возвращает `Ok(())` → handler отвечает **200**. Ни upsert, ни ошибка не выполняются. Это ровно запрещённое приёмкой «DELETE 200 while row ACTIVE».

Probe: после start строка ACTIVE; имитирован рестарт (новый `RecorderManager`); затем `ALTER TABLE video_records RENAME TO video_records_hidden` (failing Storage read), DELETE через реальный router:

```text
READFAIL_START rec=8c50ac9c-25b3-4f9c-a14a-1afaadcd66dd
READFAIL_DELETE status=200 OK body={"id":"8c50ac9c-...","session":{"id":"dd2d8cf1-..."}}
READFAIL_DIRECT_STOP result=Ok(())
SQLITE_QUERY SELECT id,duration_sec FROM video_records_hidden -> [('8c50ac9c-...', 0)]
SQLITE_QUERY SELECT id,duration_sec FROM video_records -> [('8c50ac9c-...', 0)]
assertion failed: restart-finalize must surface a storage READ failure
left: 200, right: 500
```

Строка после «успешного» DELETE осталась `duration_sec=0`, то есть ACTIVE. In-memory путь в этой же ситуации корректен: он не читает storage, а падающий upsert даёт 500.

**Классификация: P1.** Это тот же user-visible дефект, что прежний P1-3, только триггер — ошибка чтения, а не записи; при недоступной БД `list_records` падает раньше upsert, поэтому DELETE врёт. Исправление: `list_records(...).await.map_err(...)?` (и не возвращать `Ok(())`, если storage есть, но прочитать/финализировать не удалось), плюс committed-тест с failing Storage, у которого падает `list_records`.

### 4.4. Test-proven vs code-inspected

| Утверждение | Статус |
|---|---|
| In-memory stop: ошибка upsert → DELETE 500, строка ACTIVE | **test-proven** (SQLite trigger + HTTP) |
| Restart-finalize: ошибка upsert → DELETE 500, строка ACTIVE | **test-proven** |
| Restart-finalize: ошибка `list_records` → DELETE 200, строка ACTIVE | **test-proven** (P1, §4.3) |
| Retry после восстановления storage финализирует строку | **test-proven** (200, duration=1) |
| `RecorderControl::start` игнорирует ошибки `list_records`/`delete_record` при stale-cleanup | **code-inspected** (`recorder.rs:47-55`); возможны дубли ACTIVE |
| Sync `RecorderManager::stop_recording` всё ещё fire-and-forget | **code-inspected** (`recorder.rs:305-307`); production API его не использует |
| После failed stop строка ACTIVE будет удалена следующим start как stale | **code-inspected** (та же ветка `:47-55`); данные теряются, если 500 проигнорирован и процесс перезапущен |

---

## §5. Real-SQLite happy path (регрессия)

Тот же probe на реальном файле SQLite:

```text
HAPPY_IMMEDIATE_DB row=(6674fcbe-..., 0, 2350)
HAPPY_IMMEDIATE_API count=1
HAPPY_RESTART_DEDUPE=[("a23c46a3-...", 0)]
HAPPY_FINALIZED=[("a23c46a3-...", 1)]
HAPPY_HISTORY=[("a23c46a3-...", 1), ("6d4ee5f1-...", 0)] new=6d4ee5f1-...
```

- после `start` строка видна в storage **сразу** (`duration=0`, `bitrate=2350`) и API отдаёт `count=1` без polling;
- рестарт + start без stop → одна ACTIVE-строка (stale заменён, не задублирован);
- рестарт + DELETE → `duration=1`, ACTIVE=0;
- рестарт + новый start → finished-история сохранена (`total=2`, ACTIVE=1).

---

## §6. «Тесты настоящие»: negative controls

| Откат | Что показал |
|---|---|
| `duration_sec.max(1)` (`recorder.rs:293`) → без max | `duration=0`, `active=true/stopped_at=null`, следующий start удаляет строку; probe FAILED |
| `check_permission` в start/stop + scope в session-list (обратный патч `observability.rs`) | Moderator list снова 200; foreign-scope Moderator start 200; committed scope-тест FAILED |
| Падение UPDATE в реальном SQLite (без отката кода) | DELETE 500 в обоих путях, строка ACTIVE; retry 200 — фикс реален |
| Падение READ `list_records` (без отката кода) | DELETE 200, строка ACTIVE — **P1 остаётся** |

Что **не** покрыто committed-тестами: sub-second stop, ошибка stop (upsert и list), полная матрица ролей/own-scope, глобальный list со scope. Committed `recording_control_honours_the_token_room_scope` (`observability_tests.rs:668-701`) проверяет только foreign-scope Moderator + start/list/stop на `FakeRecording`; «no row created» там проверяется по `state.records`, а не по storage. Это P2-6.

---

## §7. Открытый P1 (1)

### P1-1. Restart-finalize: ошибка чтения `list_records` → DELETE 200 при ACTIVE-строке

- **Код:** `crates/vks-server/src/recorder.rs:74-87` — `if let Ok(records) = storage.list_records(...) { ... }` и безусловный `return Ok(())`.
- **Probe:** таблица переименована (failing Storage read); DELETE реального `/api/v3` router → `200 OK`; `duration_sec` строки остаётся 0; прямой `control.stop()` → `Ok(())`.
- **Следствие:** нарушен п.3 приёмки «DELETE не 200, пока строка ACTIVE» для случая недоступного/сломанного чтения; при БД, которая не читается, оператор получает успех, а запись числится активной. Следующий start удалит её как stale (P2-4), то есть возможна потеря финализированной истории.
- **Фикс:** `list_records(...).await.map_err(|e| format!("...: {e}"))?`; не возвращать `Ok(())` при наличии storage, если не удалось прочитать/финализировать; committed integration-тест с failing Storage (list + upsert), проверяющий 500 и ACTIVE-строку до восстановления.

---

## §8. Открытые P2 (14)

| # | Проблема | Статус / комментарий |
|---|---|---|
| P2-1 | Start failure без rollback: upsert упал → 500, но сессия/рекордер-участник/in-memory запись уже созданы; запись идёт | Прежний P2-1, не менялся (`recorder.rs:57-64`, `:170-230`) |
| P2-2 | Sync `RecorderManager::stop_recording` всё ещё `tokio::spawn` + `let _` | Прежний P2-2, `recorder.rs:301-308`; production его не вызывает |
| P2-3 | Stale-cleanup удаляет **все** `duration_sec==0` комнаты, игнорирует ошибки list/delete; multi-instance риск | Прежний P2-3; sub-second finished теперь защищён, failed-stop ACTIVE — нет |
| P2-4 | Failed stop оставляет ACTIVE-строку, неотличимую от crash-active; следующий start удаляет её как stale | Новый; code-inspected, следствие §4.3/P2-3 |
| P2-5 | Глобальный `GET /api/v3/videorecords` игнорирует `room_scope`; scoped Operator читает все комнаты | Новый; probe `200 count=3`; при политике scoped-токенов — потенциальный P1 |
| P2-6 | Нет committed-регрессий на sub-second, stop-error (upsert/list), полную role/scope-матрицу; scope-тест слабый (FakeRecording, `state.records`) | Прежний P2-6, частично улучшен |
| P2-7 | Историческая атомарность T09a: `650a55f` не собирается, контракт в `8af77db`; T09a размазан по 5+ коммитам | Прежний P2-5, HEAD coherent |
| P2-8 | Legacy JSON: `width==0 || height==0` → `"profile":"null"` (строка) и `[0,0]` | Прежний P2-7, pre-existing |
| P2-9 | Профиль/клампы: `RecorderProfile::new` 2..3840/2..2160 вместо 160–1920/120–1080; `settings/profile` из тела только эхо | Прежний P2-8 |
| P2-10 | Одна битая строка ломает список: `collect::<Result>` → RAM-fallback без `warn!` | Прежний P2-9 |
| P2-11 | Stop-роут не проверяет запись/комнату, игнорирует `connectionId`; неизвестный UUID → 200 | Прежний P2-10, `observability.rs:807-829` |
| P2-12 | Soak — только арифметика; release-soak harness `farm_group_soak.rs` отсутствует | Прежний P2-11 |
| P2-13 | `estimated_time` блокирует async-рантайм (`df -Pk /` через `std::process::Command`) | Прежний P2-12 |
| P2-14 | `active_h264_groups` не фильтрует `canvas_version`/`cancel` | Прежний P2-13 |

Прежний P2-4 (read-only session-list изменён Operator→Moderator+) **закрыт**: снова `forbid()` + scope, матрица подтверждена.

---

## §9. Открытые P3 (10)

1. **Слабый API-тест `estimated_time`** (`observability_tests.rs:704+`): только `!=3600`/`>3600`; формула покрыта unit-тестом профиля, но не endpoint’ом (прежний P3-1).
2. **500 при start без `reason`/метрики**: только `tracing::warn!` + generic description (`observability.rs:799-802`) (прежний P3-2).
3. **Docs/test-name drift**: pitfalls обещают `start_and_stop_recording_persists_once`, `start_recording_is_idempotent`, `estimated_time_uses_bitrate_not_constant`; в HEAD имена другие/тестов нет (прежний P3-3).
4. **Commit/scope noise**: `9b46c12` включает и прошлые review-документы; T09a-изменения размазаны (прежний P3-4).
5. **`check_permission` проверяет роль `Moderator+`, а не `Permission::ManageRecording`**: сегодня матрица совпадает, но семантика уже, чем право; при будущих ролях разъедется.
6. **Дублирование scope-логики**: session-list проверяет scope inline (`observability.rs:710-714`), start/stop — через `check_permission`; глобальный list не проверяет вовсе. Единая функция была бы надёжнее.
7. **Restart-finalize абортит оставшиеся строки при первой ошибке upsert**: при нескольких ACTIVE-строках возможна частичная финализация (`recorder.rs:76-84`).
8. **`duration_sec.max(1)` может дать `stopped_at` на 1 с в будущем** для записи короче 1 с; косметика/семантика, не потеря данных.
9. **Legacy-строки `duration=0`, завершённые до фикса**, не бэкфиллятся: при следующем start будут удалены как stale. Если фикс выкатывается на БД с такими строками — нужна миграция/ручная проверка.
10. **Commit-сообщение `9b46c12` переоценивает покрытие**: «room-scope matrix on POST/GET/DELETE (403 + no row)» — в committed-тесте только foreign Moderator и `FakeRecording`; sub-second/stop-error тестов нет.

---

## §10. Приёмка ↔ факт

| Пункт ре-ревью | Статус | Доказательство |
|---|---|---|
| 1. Sub-second: finished <1 с → `duration>=1`, `active=false`, numeric `stopped_at`, следующий start не удаляет; откат `max(1)` показывает failure | **✅** | §2: `SUBSCOND_*`; negative control `duration=0/active=true/stopped_at=null`, строка удалена |
| 2. Scope: матрица Operator/Moderator/Viewer/Participant + foreign Moderator на start/stop/list; rejected не создают строк; откат `check_permission` показывает failure | **✅** (для session-list) / **⚠️** global list P2-5 | §3: все 403/200 ожидаемы, storage пуст; negative control 200/403 и FAILED committed-тест |
| 3. Stop errors: failing Storage → DELETE 5xx, не 200 с ACTIVE; in-memory + restart-finalize | **⚠️ частично** | §4: upsert-ошибки — 500 оба пути (test-proven); read-ошибка restart-finalize — **200/ACTIVE = P1-1** |
| 4. No regression: 5 гейтов зелёные; real-SQLite happy path | **✅** | §1 (24/348/33/176, clippy 0); §5 `HAPPY_*` |
| 5. New P0/P1 только если реально сломано; P2/P3 перечислены | **✅** | P1-1 воспроизведён; 14 P2 / 10 P3 |
| 6. Score 1–10; 9.5+ только при 0 P0/P1 | **✅** | 9.0: 0 P0, 1 P1; 9.5 не достигнут |

---

## §11. Чистота и ограничения

- Живая `vks_data.db`, `backups/` и живой репозиторий не открывались; все SQLite-файлы — `/dev/shm/t09a-rr2-probe-*.db`, после прогонов удалены. В `/tmp` копий не создавалось.
- Production-файлы копии дважды временно патчились только для negative controls; после каждого прогона восстановлены из живого HEAD, sha256 совпал: `recorder.rs` `3c11efc8...`, `observability.rs` `f5de04e6...`. Итоговый `diff -r` против нового `git archive 57c2b4b` — 0 строк (удалён только сгенерированный `__pycache__`).
- PG-execute не выполнялся: нет `VKS_STORAGE_PG_URL`, `docker`, `psql`, порт 5432 не слушается. PG-путь (`core_adapter.rs:1241-1320`) проверен чтением; это единственная непроверенная исполнением часть.
- `FakeRecording`/`ProbeControl` в probe переопределяет только `active_h264_groups`, чтобы дойти до persistence-пути; сам 409-гейт и SFU-501 покрыты committed-тестами и не перепроверялись.

---

## §12. Короткий ответ

- **Score: 9.0/10.**
- **P0 = 0, P1 = 1, P2 = 14, P3 = 10.**
- **Done: нет** — открыт P1-1: restart-finalize проглатывает ошибку чтения `list_records` и отвечает 200, оставляя ACTIVE-строку; п.3 приёмки выполнен только для ошибок записи.
- **Top issues:**
  1. **P1-1:** failing Storage READ в restart-finalize → `DELETE 200`, строка `duration_sec=0` (ACTIVE); in-memory путь в той же ситуации — 500.
  2. **P2-4:** после surfaced 500 строка остаётся ACTIVE и при следующем start удаляется как stale — возможна потеря финализированной истории, если ошибку проигнорировать.
  3. **P2-5 / P2-6:** глобальный `/videorecords` не уважает `room_scope`; committed-регрессий на sub-second/stop-error/read-failure нет (negative controls делались только временным probe).
