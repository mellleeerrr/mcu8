# T09a — повторное ревью #3 записи (HEAD 4686ac8; fix 7199818)

- **Дата:** 2026-10-29, 12:23–12:47 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент); живой репозиторий только читался, менялся только этот файл.
- **Объект:** HEAD `4686ac8`; fix-коммит `7199818` «fix: close T06/T08/T09a re-review blockers» (T09a-часть: `stop()` пробрасывает ошибку `list_records`). Предыдущее ревью — [`T09a_rereview2.md`](T09a_rereview2.md) (9.0/10, P1 = restart-finalize проглатывал storage READ error).
- **Полигон:** `git archive 4686ac8` → `/dev/shm/t09a-rr3` (953 файла, без `.git`); `CARGO_TARGET_DIR=/dev/shm/t09a-rr3-target`; Rust 1.94.0 (`rust-toolchain.toml`), `CARGO_NET_OFFLINE=true`, `--locked`. Временный probe — `crates/vks-server/tests/t09a_rr3_probe.rs` в копии, после прогонов удалён. Production-код копии не менялся, кроме одного обратного патча для negative control; файл восстановлен по sha256 (`6c15bca7…` = HEAD).
- **Чистота:** живая `vks_data.db`, `backups/`, живой `target/` и рабочие процессы не открывались. Все SQLite-файлы — только `/dev/shm/t09a-rr3-probe-*.db`, удалены. В `/tmp` копий не создавалось. PG-execute не выполнялся (нет `VKS_STORAGE_PG_URL`, `docker`, `psql`).
- **Pitfalls:** [`docs/audit/2026-10-29/pitfalls/T09a.md`](../pitfalls/T09a.md).

## Вердикт

**T09a функционально закрыта: прежний P1 (read-failing restart-finalize) исправлен и подтверждён исполнением. Но HEAD `4686ac8` нельзя сертифицировать как done: обязательный clippy-гейт на pristine-копии красный из-за коммита `4686ac8` (T11a), а не T09a. Score: 9.0/10.**

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** — clippy gate на HEAD (T11a-регрессия); в самом T09a открытых P1 **0** |
| **P2** | **14** |
| **P3** | **10** |

Главное:

- P1 из `T09a_rereview2.md` **закрыт**: при падении `list_records` restart-finalize возвращает ошибку, DELETE отвечает **500**, строка остаётся ACTIVE; после восстановления storage повторный DELETE — **200**, `duration_sec=1`. Обратный патч снова даёт **200**, и probe падает — фикс реальный.
- Оба прежних P1 (sub-second stop и room scope) остаются закрытыми на этом HEAD: `duration_sec=1`, `active=false`, numeric `stopped_at`, история переживает следующий start; матрица ролей/scope — 200/403 как ожидалось, rejected-вызовы не создают строк.
- Ошибка upsert на stop по-прежнему даёт **500** в обоих путях; retry после снятия сбоя — **200**.
- **Новый P1 не в T09a:** `cargo clippy --workspace --all-targets --locked -- -D warnings` падает на `crates/vks-server/src/lib.rs:1418` (`new_track_kinds` never used) и `:1444` (`cloned_ref_to_slice_refs`). Оба места добавлены коммитом `4686ac8` (T11a). Более того, `new_track_kinds` вызывается **только из собственного `#[cfg(test)]`**, то есть новый TrackAdded-тест не проверяет production-путь.

---

## §1. HEAD, fix-коммит и гейты

`7199818` — предок `4686ac8`. Между `57c2b4b` (HEAD предыдущего ревью) и `4686ac8` T09a-relevant diff — **только** `crates/vks-server/src/recorder.rs`, 6 строк (`+`/`−`), ровно проброс `list_records`:

```text
$ git diff 57c2b4b 4686ac8 -- crates/vks-server/src/recorder.rs
-                if let Ok(records) = storage.list_records(Some(room_id)).await {
+                let records = storage
+                    .list_records(Some(room_id))
+                    .await
+                    .map_err(|error| format!("recording metadata read failed: {error}"))?;
```

Остальные изменения `4686ac8` — T11a (webrtc/docs/test) и T06/T08 из `7199818`; `observability.rs`, `observability_tests.rs`, `recording.rs` в диапазоне не менялись.

На pristine-копии (`/dev/shm/t09a-rr3`, probe ещё не добавлен):

| Команда | Exit | Результат |
|---|---:|---|
| `cargo test -p vks-storage --locked` | **0** | 23 lib + 1 `dedup_lock` = **24 passed**, 0 failed |
| `cargo test -p vks-admin --lib --locked` | **0** | **349 passed**, 0 failed |
| `cargo test -p vks-recorder --locked` | **0** | **33 passed**, 0 failed |
| `cargo test -p vks-server --tests --locked` | **0** | **177 passed**, 0 failed, 3 ignored (145 lib + 1 `config_failclosed` + 1 `h265_rtp` + 23 `network_integration` + 7 `operator_policy`) |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **101** | **КРАСНЫЙ**: 2 ошибки в `vks-server` (см. §1.1) |

Первый полный прогон `vks-server --tests` дал один нестабильный несвязанный тест `gateways::video::tests::production_decoder_path_fills_visible_windows_and_rekeys` (`decoded=0` под параллельной нагрузкой). Изолированный прогон — `skipping: libx264 not available`, ok; повторный полный прогон — **177 passed, 0 failed**. Это флейк/условие окружения (тест сам пропускается без libx264), не дефект T09a; в счёт P не включён, но отмечен как ограничение.

### 1.1. P1 (HEAD, не T09a): clippy-гейт красный

```text
error: function `new_track_kinds` is never used
    --> crates/vks-server/src/lib.rs:1418:4
    = note: `-D dead-code` implied by `-D warnings`

error: this call to `clone` can be replaced with `std::slice::from_ref`
    --> crates/vks-server/src/lib.rs:1444:54
    = note: `-D clippy::cloned-ref-to-slice-refs` implied by `-D warnings`

error: could not compile `vks-server` (lib test) due to 1 previous error
[exit 101]
```

- `new_track_kinds` (`lib.rs:1418-1432`) — новая свободная функция из `4686ac8`, в production **не вызывается**: `grep -R new_track_kinds crates` даёт определение и вызовы только в `#[cfg(test)] mod tests` (`:1436-1446`). В non-test lib это dead code.
- Production-цикл остался inline (`lib.rs:1100-1101`): `for track in tracks.iter().filter(|track| !previous_kinds.contains(&track.media_kind))`. Он **не дедуплицирует** одинаковые `media_kind` внутри одного offer, тогда как тестируемая `new_track_kinds` дедуплицирует. То есть TrackAdded-тест проверяет не только неиспользуемую копию, но и поведение, которого в production нет; заявленный «duplicate kinds de-duplicated» не реализован. Это одновременно gate-блокер и дефект тестового покрытия T11a.
- `audio.clone()` в `new_track_kinds(&[MediaKind::Audio], &[audio.clone()])` (`:1444`) — второй warning в lib-test target.
- В `57c2b4b` функции не было (`git show 57c2b4b:...lib.rs | grep -c new_track_kinds` → 0), то есть регрессия внесена именно HEAD-коммитом.

Никакого отношения к T09a этот P1 не имеет, но обязательный гейт «clippy workspace -D warnings» на проверяемом HEAD не выполнен, поэтому сертифицировать HEAD как зелёный нельзя.

---

## §2. P1 из предыдущего ревью закрыт: read-failing restart-finalize

### 2.1. Код

`RecorderControl::stop` (`crates/vks-server/src/recorder.rs:71-93`), ветка «после рестарта in-memory сессии нет»:

```rust
if let Some(storage) = state.storage.clone() {
    let records = storage
        .list_records(Some(room_id))
        .await
        .map_err(|error| format!("recording metadata read failed: {error}"))?;
    {
        for mut entry in records.into_iter().filter(|entry| entry.duration_sec == 0) {
            ...
            storage.upsert_record(&entry).await
                .map_err(|error| format!("recording metadata persistence failed: {error}"))?;
            ...
        }
    }
}
return Ok(());
```

Ошибка чтения больше не превращается в `Ok(())`; handler `videorecords_stop` (`observability.rs:822-827`) на `Err` отдаёт `500 Recording could not be stopped`.

### 2.2. Positive probe: реальный SQLite, реальный `/api/v3` router

Сценарий: start через router → строка ACTIVE; «рестарт» (новый `AppState` + новый `RecorderManager`, тот же файл SQLite); таблица переименована (`ALTER TABLE video_records RENAME TO video_records_hidden`) → `list_records` падает; DELETE через реальный router.

```text
P1_READFAIL_START id=fb859257-… rows=[("fb859257-…", 0)]
P1_READFAIL_DELETE status=500 body={"error":{"code":500,"description":"Recording could not be stopped"}}
P1_READFAIL_DIRECT_STOP=Err("recording metadata read failed: storage error: error returned from database: (code: 1) no such table: video_records")
P1_READFAIL_DB_HIDDEN=[("fb859257-…", 0)]
P1_RECOVERED_DELETE status=200 body={"id":"cd92f0a0-…","session":{"id":"3bbd7f61-…"}}
P1_RECOVERED_DB=[("fb859257-…", 1)]
P1_RECOVERED_LIST status=200 body={"count":1,…,"active":false,…,"stopped_at":1793266603,…}
```

То есть:

- failing READ → DELETE **500**, не 200; строка остаётся видимо ACTIVE (`duration_sec=0`);
- прямой `control.stop()` возвращает `Err` с причиной (не `Ok(())`);
- после восстановления таблицы повторный DELETE — **200**, строка финализирована (`duration_sec=1`, `active=false`, numeric `stopped_at`).

In-memory путь в той же ситуации отдельно проверен через падающий UPDATE (trigger) — DELETE **500**, строка ACTIVE; retry после `DROP TRIGGER` — **200**, `duration=1` (§3.3).

### 2.3. Negative control: откат фикса

В pristine-копии `recorder.rs:74-79` возвращены к старому `if let Ok(records) = ...`. Файл после прогона восстановлен, sha256 `6c15bca7…` совпадает с HEAD.

```text
P1_READFAIL_DELETE status=200 body={"id":"3e342e0c-…","session":{"id":"44ff0ec6-…"}}
assertion `left == right` failed: restart-finalize must surface a storage READ failure instead of 200
  left: 200
 right: 500
test result: FAILED. 0 passed; 1 failed
```

То есть probe ловит именно тот дефект, который закрыл `7199818`.

---

## §3. Прежние P1 на этом HEAD: остаются закрытыми

Код sub-second/scope/upsert между `57c2b4b` и `4686ac8` не менялся (кроме read-фикса выше), поэтому проверялись positive-исполнением на реальном SQLite + реальном router.

### 3.1. Sub-second stop: `duration>=1`, `active=false`, история

```text
SUBSEC_DB=[("8405867f-…", 1)]
SUBSEC_LIST status=200 body={"count":1,…,"active":false,…,"started_at":1793266602,"stopped_at":1793266603,…}
SUBSEC_NEXT_START=[("8405867f-…", 1), ("980ac881-…", 0)]
SUBSEC_FINISHED=[("8405867f-…", 1), ("980ac881-…", 1)]
```

- stop через миллисекунды после start сохранил `duration_sec=1`;
- API: `active=false`, `stopped_at` numeric, разница `stopped_at-started_at=1`;
- после «рестарта» новый start **не удалил** завершённую строку: в комнате две строки (finished + новый ACTIVE).

### 3.2. Scope matrix: 200/403, rejected не создают строк

```text
MATRIX Operator start=200 stop=200 list=200 rows=1
MATRIX Moderator start=200 stop=200 list=403 rows=1
MATRIX Viewer start=403 stop=403 list=403 rows=0
MATRIX Participant start=403 stop=403 list=403 rows=0
SCOPE foreign_moderator start=403 stop=403 list=403 rows=0
SCOPE foreign_operator start=403 stop=403 list=403 rows=0
SCOPE own_operator start=200 stop=200 list=200 rows=1
```

Проверка `rows=0` сделана по **storage** (`SqliteStorage::list_records`), а не по `state.records`: отклонённые start/stop действительно не оставляют строк. Own-scope Operator работает; foreign-scope Moderator/Operator получают 403 на start/stop/list. Read-only session-list остался Operator-only (Moderator 403), как и в предыдущем ревью.

### 3.3. Upsert failure → 500 в обоих путях, retry финализирует

Инъекция: SQLite-триггер `BEFORE UPDATE ON video_records ... RAISE(ABORT, ...)`. Start делает INSERT (триггер не срабатывает), stop — UPSERT→UPDATE (падает).

```text
STOP_INMEMORY status=500 body={"error":{"code":500,"description":"Recording could not be stopped"}}
STOP_INMEMORY_DB=[("025add57-…", 0)]
STOP_INMEMORY_RECOVERED status=200 row=[("025add57-…", 1)]
STOP_RESTART status=500 body={"error":{"code":500,"description":"Recording could not be stopped"}}
STOP_RESTART_DB=[("544bd5f6-…", 0)]
STOP_RESTART_RECOVERED status=200 row=[("544bd5f6-…", 1)]
```

- in-memory stop: DELETE **500**, строка ACTIVE; после снятия триггера retry — **200**, `duration=1`;
- restart-finalize: DELETE **500**, строка ACTIVE; retry — **200**, `duration=1`.

### 3.4. Real-SQLite happy path

```text
HAPPY_IMMEDIATE_DB=[("c75ed628-…", 0, 2350)]
HAPPY_IMMEDIATE_API={"count":1,…,"active":true,…,"stopped_at":null,…}
HAPPY_RESTART_DEDUPE=[("7bac7c9c-…", 0)]
HAPPY_FINALIZED=[("7bac7c9c-…", 1)]
HAPPY_HISTORY=[("7bac7c9c-…", 1), ("98d72333-…", 0)]
```

- после start строка видна в storage **сразу** (`duration=0`, `bitrate=2350`), API отдаёт `count=1`, `active=true`, `stopped_at=null`;
- рестарт + start без stop → одна ACTIVE-строка (stale заменён, не задублирован);
- рестарт + DELETE → `duration=1`;
- рестарт + новый start → finished-история сохранена (`total=2`, ACTIVE=1).

---

## §4. P2-остатки и их классификация

Три остатка, названные в задании, воспроизведены/подтверждены на HEAD. **Ни один из них не является P0.** Классификация P1/P2:

| # | Остаток | Статус на HEAD | P0/P1? |
|---|---|---|---|
| P2-4 | Failed stop оставляет ACTIVE-строку; следующий start удалит её как stale | **подтверждено**: оба пути 500, `duration=0`; retry после восстановления — 200/`duration=1` | **P2.** DELETE честно сообщает 500, строка не выдаётся за завершённую; потеря истории возможна только если 500 проигнорировать и затем сделать start. Не P1 при текущем контракте. |
| P2-5 | Глобальный `GET /api/v3/videorecords` игнорирует `room_scope` | **подтверждено**: `GLOBAL_LIST foreign_scoped_operator status=200 body={"count":1,…,"session":{"id":"<foreign room>"}}`; session-list того же токена — 403 | **P2 в рамках принятой матрицы**, но потенциальный **P1**, если scoped-Operator-токены считаются поддерживаемым деплоем: это чтение метаданных чужих комнат, тогда как `sessions/{id}/videorecords`, `history`, `settings` и `directory` scoped-токены ограничивают. |
| P2-6 | Нет committed-регрессии на failing storage (`list_records`/`upsert`) | **подтверждено**: в `crates/vks-server/tests` и `observability_tests.rs` нет теста с failing Storage; negative control делался только временным probe | **P2.** Фикс реален, но CI не поймает его откат. |

Остальные 11 P2 из `T09a_rereview2.md` (start без rollback, sync-stop `tokio::spawn`, stale-cleanup все `duration==0`, историческая атомарность T09a, legacy JSON `profile:"null"`, clamp 2..3840, одна битая строка ломает список, stop-роут не проверяет запись/`connectionId`, soak-арифметика, blocking `estimated_time`, `active_h264_groups` без `canvas_version`/`cancel`) переносятся без изменений: соответствующий код в диапазоне `57c2b4b..4686ac8` не менялся. P3 — те же 10, новых P3 в T09a не появилось.

---

## §5. Приёмка ↔ факт

| Пункт ре-ревью | Статус | Доказательство |
|---|---|---|
| 1. Failing storage READ в restart-finalize → DELETE 5xx, не 200; in-memory 500; retry после восстановления работает; показать код/path; negative control падает | **✅** | §2: `P1_READFAIL_*`, `P1_RECOVERED_*`; negative control `left: 200, right: 500`; `recorder.rs:74-87` |
| 2. Прежние P1 закрыты: sub-second (`duration>=1`, `active=false`, история), scope-матрица (403/200, rejected не создают строк), upsert → 500 оба пути | **✅** | §3: `SUBSEC_*`, `MATRIX_*`, `SCOPE_*`, `STOP_*`; проверка строк по storage |
| 3. P2-остатки; сказать, есть ли P0/P1 | **✅** | §4: P2-4/P2-5/P2-6; P0 нет; P2-5 — потенциальный P1 при scoped-Operator-политике |
| 4. Гейты на pristine; real-SQLite happy path | **⚠️** | §1: 4 тестовых гейта зелёные, **clippy красный** (T11a-регрессия); §3.4 happy path — ✅ |
| 5. Score 1–10; 9.5+ только при 0 P0/P1 и real evidence | **✅** | 9.0: T09a P1 = 0, но HEAD P1 (clippy) = 1; 9.5 не достигнут |

---

## §6. Почему 9.0, а не 9.5

Сам T09a на этом HEAD заслуживает высокой оценки: прежний P1 закрыт в коде и подтверждён реальным SQLite+HTTP probe с negative control; sub-second/scope/upsert-контракты остаются исполненными; happy path и retry-путь работают. Функциональных P0/P1 в T09a **нет**.

Но 9.5 по условию требует «0 открытых P0/P1 + real evidence» на проверяемом HEAD. Здесь открыт **P1 (HEAD, T11a)**: обязательный clippy-гейт падает на `new_track_kinds` (dead code) и `audio.clone()`; при этом TrackAdded-тест не покрывает production-путь. Это не позволяет назвать HEAD зелёным и не позволяет поставить 9.5. 9.0 отражает «T09a готов, HEAD-гейт — нет».

---

## §7. Чистота и ограничения

- Живая `vks_data.db`, `backups/` и живой репозиторий не открывались; все SQLite-файлы — `/dev/shm/t09a-rr3-probe-*.db`, после прогонов удалены. В `/tmp` копий не создавалось.
- `recorder.rs` копии временно патчился только для negative control; восстановлен из сохранённой копии, sha256 `6c15bca7cd439fb771af0daeef2c0e8fb3de2cf3ace6e5815332e37fc36876eb` совпадает с `4686ac8`.
- Probe — временный integration-тест в копии; после прогонов удалён. Он использует `ProbeControl`, переопределяющий только `active_h264_groups()→1`, чтобы дойти до production persistence-пути; сам 409-гейт не перепроверялся.
- PG-execute не выполнялся: нет `VKS_STORAGE_PG_URL`/`docker`/`psql`; PG-путь (`core_adapter.rs:1241-1320`) проверен чтением, как и в предыдущих ревью.
- clippy-гейт проверялся на pristine-копии **до** добавления probe; probe в гейт не входит.

---

## §8. Короткий ответ

- **Score: 9.0/10.**
- **P0 = 0, P1 = 1, P2 = 14, P3 = 10.** P1 — clippy-гейт на HEAD из-за `4686ac8` (T11a); в T09a открытых P0/P1 нет.
- **Done: нет** — T09a-фикс закрыт, но обязательный `cargo clippy --workspace --all-targets --locked -- -D warnings` на pristine `4686ac8` красный; HEAD нельзя сертифицировать.
- **Top issues:**
  1. **P1 (HEAD/T11a):** `new_track_kinds` (`lib.rs:1418`) — dead code в non-test lib (`-D dead-code`), и он не используется production-циклом; `audio.clone()` (`:1444`) — `cloned_ref_to_slice_refs`. Clippy exit 101.
  2. **P2-5:** глобальный `GET /api/v3/videorecords` отдаёт scoped-Operator записи всех комнат (200), тогда как session-list — 403; потенциальный P1 при политике scoped-токенов.
  3. **P2-4/P2-6:** failed stop оставляет ACTIVE-строку (500 корректен, но следующий start может удалить её как stale); committed-теста на failing Storage нет — откат фикса `7199818` CI не поймает.
