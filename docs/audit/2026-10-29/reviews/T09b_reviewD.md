# T09b — независимое ревью D (round 3): detached durable finalize + tap существующей группы

- **Дата:** 2026-10-29, 15:08–15:32 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент). Живой репозиторий только читался; в нём создан только этот файл.
- **Объект:** `3e2983cbbbc7f9381c2ace1c55a13624d85a0a4b` «fix(T04b): derive production RoomCaps from the codec SSOT». T09b-фикс пришёл из `8b47dfe` («detached durable finalize») и в `3e2983c` не менялся; проверялись `crates/vks-server/src/recorder.rs`, `crates/vks-server/src/recorder_tap.rs`, `crates/vks-recorder/src/container.rs`.
- **Полигон:** `git archive 3e2983c | tar -x -C /dev/shm/t09b_reviewD_1793275792` (994 файла, без `.git`), Rust **1.94.0** (`rustc 1.94.0 (4a4ef493e 2026-03-02)`, `RUSTUP_TOOLCHAIN=1.94.0`), `CARGO_TARGET_DIR=/dev/shm/rvD_target`, `--offline`, `--locked`, FFmpeg/ffprobe **8.0.1-3ubuntu2**.
- **Чистота копии:** sha256 production-файлов T09b совпадают с коммитом: `recorder.rs` `1ec76370…`, `recorder_tap.rs` `8521895f…`, `container.rs` `f3aa750e…`. Shim/revert не добавлялся. Review-only probe-тесты лежали только в `/dev/shm`-копии, после снятия доказательств удалены.
- **Чистота стенда:** живая `vks_data.db`, `backups/`, порты `:8080/:18082`, живой `target/` и запущенный сервер не открывались и не менялись. Все SQLite/MP4/probe-артефакты — только в `/dev/shm`; `/tmp` не использовался.

## Вердикт

**done: нет. Score: 9.0/10.**

P1 из review C закрыт по-настоящему: detached finalize с `expected record id` завершает `storage.upsert_record` после потери группы, строка в реальном SQLite становится `duration_sec>=1 / size_bytes>0`, переживает переоткрытие БД и hydration, RoomHub-участник снимается. Ошибка writer'а (out-of-order PTS) также финализируется durable. Clippy alias, повторный stop и документирование B-кадров подтверждены. Контейнер независимо разобран и полностью декодируется, AAC/esds синтетика проходит.

Но **заявленное «stale-success window gone» не выполнено**: если вызвать `start` сразу после ухода последнего зрителя, до того как detached-таск успел снять сессию, `RecorderControl::start` по idempotent-ветке возвращает **Ok со старым record_id** и не переподписывается на новую группу — 8/8 прогонов. Окно короткое (финализация занимает 11.9–22.9 мс), durable ACTIVE-строки не остаётся, но клиент получает ложный 200 на старт, который ничего не записывает. Это P2, а не P1: данные не теряются, сессия затем корректно финализируется.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **1** (ложный 200 старта в окне автофинализации) |
| **P3** | **5** |

## §1. Сборка, clippy, fmt

| Команда | Exit | Результат |
|---|---:|---|
| `cargo check -p vks-recorder -p vks-server -p vks-admin --all-targets --offline --locked` | **0** | `Finished dev profile … in 1m 24s`; shim не нужен |
| `cargo clippy -p vks-recorder -p vks-server -p vks-admin --all-targets --offline --locked -- -D warnings` | **0** | `Finished … in 47.59s`, warning/error нет |
| `cargo fmt --all -- --check` | **0** | чисто (повторно после удаления probe-тестов) |

`clippy::type_complexity` действительно закрыт алиасом `type StoppedRecord = (RecordingMeta, Option<String>)` (`crates/vks-server/src/recorder.rs:535`), gate `-D warnings` зелёный.

## §2. P1 durability: detached finalize с реальным SQLite

Полигон: `AppState::with_storage(Arc<SqliteStorage>)`, файл `sqlite:///dev/shm/rvD_probe_lossN_*.db?mode=rwc` (реальные migrations, не MemoryStorage). Сценарий: живая x264-группа → `start` → публикация кадров → `drop(viewer)` → ожидание `!manager.is_recording` → ожидание `storage.get_record`. Сделано 3 независимых прогона.

Факты (вывод probe; артефакты удалены после снятия):

```text
loss#0: finalized_in=22.515ms  mem=RecordEntry{duration_sec:2,size_bytes:1831}
        db =RecordEntry{duration_sec:2,size_bytes:1831}  container=1831 bytes
loss#1: finalized_in=11.910ms  mem/db duration_sec=2, size_bytes=1831
loss#2: finalized_in=22.915ms  mem/db duration_sec=2, size_bytes=1831
second#0: id2 != id, bytes2=4194, db2={duration_sec:2,size_bytes:4194}
second#1: id2 != id, bytes2=4202, db2={duration_sec:2,size_bytes:4202}
second#2: id2 != id, bytes2=4202, db2={duration_sec:2,size_bytes:4202}
```

Проверено в каждом прогоне:

- `manager.is_recording(&room) == false`;
- `state.room_hub.list_participants(&room).is_empty()`;
- in-memory `RecordEntry` финализирован (`duration_sec=2`, `size_bytes=1831`);
- SQLite `get_record` содержит ровно те же `duration_sec`/`size_bytes`; `list_records(Some(room))` не содержит строк с `duration_sec==0` (ACTIVE);
- тот же файл БД открыт **новым** `SqliteStorage`, `get_record` даёт `duration_sec=2/size=1831`; новый `AppState::sync_from_storage()` гидрирует строку с теми же значениями;
- контейнер сохранён (`manager.get_container(id)` = 1831 байт).

**Writer-error path (out-of-order PTS).** Собран реальный x264 IDR + 12 AU, затем в канал отправлен ранний AU после новых; `write_video_sample` вернул `некорректный timestamp 3511127 us`. Вывод probe:

```text
PROBE_D writer_error: finalized_in=11.078929ms  aus=15  forward=12
  db=RecordEntry{duration_sec:1,size_bytes:1798}  bytes=1798
  first="некорректный timestamp 3511127 us"
  second="некорректный timestamp 3511127 us"
  manager_err=Container("некорректный timestamp 3511127 us")
```

То есть detached finalize завершил `storage.upsert_record` и на error-пути; durable-строка не осталась `duration=0/active`, контейнер с 12 записанными сэмплами сохранён, первый и второй `stop` вернули **одинаковую** ошибку, manager-уровень тоже `Err`. Вывод: **P1 review C исправлен, регрессии не найдено.**

## §3. Новый старт после потери группы и «stale-success window»

После подтверждённой финализации (`!is_recording`, `db.duration>=1`) новый `start` создаёт **новую** сессию (`id2 != id`), пишет 42 сэмпла (4194–4202 байта), durable-строка `duration_sec=2/size_bytes=4194…4202`. Это подтверждено 3/3.

Однако отдельный adversarial-прогон (8 итераций) вызвал `control.start(room)` **сразу** после `drop(viewer)`, не дожидаясь detached-таска:

```text
race#0..7: recording_at_call=true
immediate=Ok("787b3976-…") == id
…
stale_success=8  fresh_or_error=0
```

Причина в коде: `RecorderControl::start` сначала проверяет `self.recorder.is_recording(&room_id)` и по истине возвращает `start_recording(...)` без проверки живости tap/группы (`crates/vks-server/src/recorder.rs:51-57`). `finalize_after_tap_stop` снимает сессию только в отдельном таске (`recorder.rs:482-496`), поэтому между `Closed` и `sessions.remove` окно существует. Через 11.9–22.9 мс сессия финализируется, и следующий `start` уже корректен, но первый вызов уже отдал 200/Ok на пустой старт.

**Оценка:** P2. Это не прежний P1 (сессия не остаётся ACTIVE навсегда), но пункт приёмки «stale-success window gone» не выполняется. Фикс: в `start` при существующей сессии проверять, что её tap ещё жив (или что группа ещё подписана), иначе сначала завершать старую сессию/возвращать `no_active_h264_encoder_group`, а не `Ok(old_id)`.

## §4. Повторный stop и ошибка writer'а

- `repeated_stop_replays_finalize_error` (committed) — 5/5 проходит.
- На writer-error пути первый и второй `control.stop` вернули идентичную строку `некорректный timestamp 3511127 us`; `manager.stop_recording_async` — `Err(Container(...))`.
- Session-level `container_finalize_error_is_surfaced_not_swallowed` входит в 54 passed `vks-recorder` lib.

## §5. Контейнер: независимая проверка (без кода репозитория)

Собственный Python-парсер ISOBMFF (боксы, `avcC`, `stsd/stts/stsz/stco/stss`, NAL length-prefix, отсутствие Annex-B) + `ffprobe`/`ffmpeg` 8.0.1. Файлы сняты **реальным recorder-tap с реальной x264-группы**, не синтетикой:

| Файл | Байт | Сэмплов | NAL | `mdhd` | `stss` | ffprobe |
|---|---:|---:|---:|---:|---|---|
| `group_loss_0.mp4` | 1831 | 13 | 28 | 58524 (0.650 с) | `[1]` | h264/avc1, is_avc=true, nal_length_size=4, 1/90000, has_b_frames=0 |
| `group_loss_1.mp4` | 1831 | 13 | 28 | 59890 | `[1]` | то же |
| `group_loss_2.mp4` | 1831 | 13 | 28 | 59391 | `[1]` | то же |
| `second_start_0.mp4` | 4194 | 42 | 89 | 196439 (2.183 с) | `[1,31]` | то же, 42/42 кадра |
| `writer_error.mp4` | 1798 | 12 | 26 | 45299 | `[1]` | то же, 12/12 кадра |

Для всех файлов проверено:

- top-level ровно `ftyp`, `moov`, `mdat` по одному и в порядке `moov < mdat`;
- `avcC` — ребёнок `avc1`, `configurationVersion=1`, `lengthSizeMinusOne=3` (NAL length 4), SPS 24 байта / PPS 4 байта, profile 66 (Baseline), level 31;
- `mdhd.timescale=90000`, `sum(stts)==mdhd.duration`, `sum(stsz)==mdat payload`;
- `stco` монотонно и точно совпадает с началом каждого сэмпла; length-prefix заполняет сэмпл ровно;
- в `mdat` нет Annex-B start code (`00 00 01` / `00 00 00 01`);
- `stss` в точности равен фактическим IDR-сэмплам (первые NAL type 5);
- нет `mp4a` и audio-trak (video-only by design, фиктивный AAC не пишется);
- `ffprobe -count_frames`: `nb_read_frames == stsz count`, `codec_tag_string=avc1`, `is_avc=true`, `nal_length_size=4`, `time_base=1/90000`, `has_b_frames=0`;
- полный `ffmpeg -v error -i file -map 0:v:0 -f null -` — exit 0, stderr пуст (0 строк) на всех пяти файлах.

**AAC/esds synthetic.** Отдельный review-only writer сгенерировал `/dev/shm/rvD_artifacts/aac_esds.mp4`: 49 AAC-кадров из реального ADTS, 14035 байт. Парсер: `mp4a` + `esds` с тегами `0x03/0x04/0x05/0x06`, ASC `11 90`, DecoderConfig `0x40…`, `mdhd.timescale=48000`, `mdhd.duration=50176=49*1024`, `stts` sum совпадает, ADTS-заголовков в сэмплах нет; `ffprobe`: `aac/LC`, 2 канала, 48 кГц, 1/48000, 49 кадров; `ffmpeg -map 0:a:0 -f null -` — exit 0. Синтетический video-AU в этом review-only файле намеренно невалиден, поэтому ffmpeg может ругаться на H.264 при открытии контейнера; аудиотрек при этом декодируется, а production tap-файлы (таблица выше) дают пустой stderr. Committed `ffprobe_acceptance::ffprobe_aac_esds_acceptance` тоже passed.

## §6. Гейты

| Команда | Exit | Точный результат |
|---|---:|---|
| `cargo test -p vks-recorder --locked --offline` | **0** | **54 passed** lib + **2 passed** `ffprobe_acceptance`; 0 failed |
| `cargo test -p vks-server --lib --locked --offline` | **0** | **150 passed, 0 failed, 3 ignored** |
| `cargo test -p vks-server --test recorder_tap --locked --offline` × **5** | **0** ×5 | **5/5 passed** в каждом, итого **25 passed / 0 failed**; 2.57–2.72 с на прогон |
| `cargo test --workspace --locked --offline --no-fail-fast` | **0** | **1558 passed, 0 failed, 8 ignored**, 52 result-блока (включая doc-tests) |

Примечания:

- Заявленные в задании **149/3** для server lib на `3e2983c` уже неверны: коммит T04b после review C добавил `gateways::tests::sip_pt_maps_match_ssot_disposition`, поэтому фактически **150/3**. Это не дефект T09b.
- Первый прогон workspace упал не на коде, а на **дисковой квоте `/dev/shm`** (`rustc-LLVM ERROR: IO failure on output stream: Disk quota exceeded`, `os error 122`) из-за параллельных сборок других агентов. Повтор с `CARGO_INCREMENTAL=0 -j 8` после освобождения 2.2 ГБ incremental прошёл: **1558/0/8**.
- Flake `recorder_tap` под нагрузкой, который видел review C (1/12), в этот раз **не воспроизвёлся**: 25/25. В review-only probe при публикации первого кадра раньше готовности FFmpeg группа под нагрузкой умирала по 2-секундному `write_all` timeout, но committed-тесты после прогрева стабильны; это P3-риск тестового стенда, не T09b-дефект.

## §7. Список находок

### P2

**P2-1. Ложный успех `start` в окне автофинализации (stale-success window).**
`RecorderControl::start` (`crates/vks-server/src/recorder.rs:51-57`) при `is_recording=true` возвращает `Ok(old_record_id)` через `start_recording`, не проверяя, что tap ещё жив и группа ещё существует. После `RecvError::Closed` сессия снимается только detached-таском (`recorder.rs:482-496`), поэтому в окне 11.9–22.9 мс `start` отдаёт 200 на старт, который не подписывается на новую группу. Воспроизведено **8/8**. Durable ACTIVE-строки не остаётся, данные не теряются — поэтому P2, а не P1. Прямо противоречит заявлению (d) «stale-success window gone». Фикс: проверять живость tap/группы перед idempotent-возвратом либо сначала дожидаться/выполнять финализацию старой сессии.

### P3

**P3-1. TOCTOU между `expected record id` guard и удалением сессии.**
Guard читает `sessions` и отпускает lock (`recorder.rs:483-485`), затем вызывает `stop_recording_async`, а `stop_recording_internal` удаляет **любую** сессию комнаты (`recorder.rs:336-338`). Если detached-таск вытеснен сразу после guard, а пользовательский `stop`+`start` успел создать новую сессию, detached-таск финализирует уже новую запись. Не воспроизведено (окно микроскопическое), но guard не атомарен с удалением. Фикс: передавать `expected` в `stop_recording_internal` и удалять под lock только при совпадении `record_id`.

**P3-2. Ошибка tap может быть проглочена гонкой пользовательского stop.**
Tap кладёт ошибку в `tap_errors` (`recorder_tap.rs:66-68`, `recorder.rs:294-296`), затем запускает detached finalize. Если пользовательский `stop` успевает между вставкой ошибки и снятием сессии, `stop_recording_internal` читает `tap_error` (`recorder.rs:367`) и кладёт его в `stopped_cache` (`:398`), но `StopOutcome.finalize_error` формируется только из `stop_result.err()` (`:434`), поэтому первый `control.stop` через активную ветку может вернуть `Ok`, а ошибка появится только на втором stop. Не воспроизведено. Фикс: `finalize_error = stop_result.err().or(tap_error.map(RecordingError::Container))`.

**P3-3. Ошибка `storage.upsert_record` на старте оставляет активную сессию.**
`start` сначала создаёт сессию/tap/участника и лишь затем пишет строку; при ошибке storage (`recorder.rs:80-87`) возвращается 500, но запись остаётся `is_recording=true` и участник в RoomHub. Повторный `start` вернёт Ok по idempotent-ветке, не пытаясь снова persist. Не тестировалось (нужен refusing storage); статический дефект cleanup-порядка.

**P3-4. Detached finalize — fire-and-forget, не отслеживается.**
`tokio::spawn` (`recorder.rs:482`) не сохраняется и не join-ится; при завершении процесса/рантайма pending `storage.upsert_record` может быть потерян, хотя `manager.is_recording` уже false. В тестах это закрыто ожиданием БД; в production — только shutdown-порядком. Не воспроизведено.

**P3-5. Допущение «B-кадров нет» задокументировано, но не закреплено тестом.**
`container.rs:14-19` честно фиксирует отсутствие `ctts` и требует DTS/ctts при B-кадрах. Однако committed-тесты не проверяют `has_b_frames=0`; в этом ревью независимый ffprobe на реальных tap-файлах дал `has_b_frames=0`, то есть допущение выполняется для текущего x264 baseline, но регрессионного теста нет.

## §8. Гигиена и ограничения

- В живом репозитории создан только `docs/audit/2026-10-29/reviews/T09b_reviewD.md`; production-файлы, `Cargo.toml`, `Cargo.lock` не менялись.
- Живые `vks_data.db`, `backups/`, порты и процессы не открывались; все probe-БД/MP4 — в `/dev/shm`, после снятия доказательств удалены.
- Ограничение: `recorder_tap` flake под экстремальной нагрузкой не удалось воспроизвести (25/25 passed); вывод «стабильно» ограничен этим прогоном.
- Итог: **P0=0, P1=0, P2=1, P3=5, score 9.0, done: нет** — из-за невыполненного пункта «stale-success window gone». Остальные заявленные фиксы (durable finalize, clippy, repeat stop, writer error, B-frame docs, container/AAC) подтверждены исполнением.
