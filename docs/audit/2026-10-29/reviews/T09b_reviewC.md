# T09b — независимое ревью C (round 2): MP4-контейнер + tap существующей группы

- **Дата:** 2026-10-29, 14:28–14:45 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент). Живой репозиторий только читался; в нём создан только этот файл.
- **Объект:** `f602f72d19df0a5311cbad55392509582f444d3f` «fix(T09b): finalize on encoder group loss, replay finalize errors, surface tap writer errors», parent `a02daad237c99321a69eeec9f72681755dc36786` (T04b; подтверждено `git merge-base --is-ancestor a02daad f602f72` → yes).
- **Полигон:** `git archive f602f72 | tar -x -C /dev/shm/t09bC_1793273305` (976 файлов, без `.git`), Rust **1.94.0** (`rustc 1.94.0 (4a4ef493e 2026-03-02)`, `RUSTUP_TOOLCHAIN=1.94.0`), `CARGO_TARGET_DIR=/dev/shm/t09bC_target`, `--offline`, `--locked` где применимо, FFmpeg/ffprobe **8.0.1-3ubuntu2**.
- **Чистота копии:** `sha256` production-файлов T09b совпадают с коммитом: `recorder.rs` `852796c8…`, `recorder_tap.rs` `8521895f…`, `container.rs` `f3aa750e…`. Shim/revert не добавлялся; `codec_capabilities::audio_unavailable_reason`/`video_unavailable_reason` уже есть в самом `f602f72` (`crates/vks-common/src/codec_capabilities.rs:149,164`). Probe-тесты лежали только в `/dev/shm`-копии и удалены.
- **Чистота стенда:** живая `vks_data.db`, `backups/`, порты `:8080/:18082`, живой `target/` и запущенный сервер не открывались и не менялись. Все SQLite/MP4/probe-артефакты — только в `/dev/shm`; `/tmp` для распаковки не использовался. После снятия доказательств scratch и target удалены.

## Вердикт

**done: нет. Score: 8.5/10.**

Коммит действительно закрывает прежние блокеры по сборке, повторному stop и ошибке writer'а, а контейнерная часть подтверждена независимым парсером и полным декодом реального tap-файла. Но заявленное «no ACTIVE leftovers» **не выполняется на durable/API-уровне**: автоматическая финализация при потере группы запускается внутри tap-таска и абортит сама себя, из-за чего `storage.upsert_record` отменяется на первом pending-await. В результате in-memory-строка финализирована, а строка в SQLite остаётся `duration_sec=0/size_bytes=0`; `GET /api/v1/records` и `GET /api/v3/videorecords` читают storage и показывают запись **active=true** (и она переживает рестарт до следующего `start`). Это остаток P1-1 и прямое противоречие пункту приёмки «row duration>=1 not ACTIVE». Дополнительно обязательный clippy-гейт `-D warnings` красный на новом коде.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** (P1-1 residual: ACTIVE-строка в storage после group loss) |
| **P2** | **2** (clippy red; flaky tap-тесты) |
| **P3** | **6** |

## §1. Сборка и гейты на pristine-копии

| Команда | Exit | Результат |
|---|---:|---|
| `cargo check -p vks-recorder -p vks-server -p vks-admin --all-targets --offline` | **0** | Finished; shim не нужен |
| `cargo check -p vks-server -p vks-admin --offline` | **0** | Finished |
| `cargo fmt --all -- --check` | **0** | чисто |
| `cargo clippy -p vks-recorder --all-targets --offline -- -D warnings` | **0** | чисто |
| `cargo clippy -p vks-admin --all-targets --offline -- -D warnings` | **0** | чисто |
| `cargo clippy -p vks-server --all-targets --offline -- -D warnings` | **101** | **1 error** (P2-1) |
| `cargo test -p vks-recorder --offline` | **0** | **54 passed** lib + **2 passed** `ffprobe_acceptance`; 0 failed |
| `cargo test -p vks-server --lib --offline` | **0** | **149 passed, 0 failed, 3 ignored** |
| `cargo test -p vks-server --test recorder_tap --offline` (12 прогонов) | 11×0, 1×101 | **58 passed / 60**; 1 прогон: 3 passed, 2 failed (P2-2) |
| `cargo test --workspace --locked --offline --no-fail-fast` | **0** | **1528 passed, 0 failed, 8 ignored**, 51 target |

Вывод: **сборка pristine-дерева восстановлена** (T04b `a02daad` действительно содержит недостающие функции), `check`/`fmt`/тесты/workspace зелёные, но **clippy-гейт красный на новом коде** — см. P2-1.

### 1.1. P2-1: clippy `-D warnings` красный

```text
error: very complex type used. Consider factoring parts into `type` definitions
   --> crates/vks-server/src/recorder.rs:136:20
    |
136 |     stopped_cache: Arc<Mutex<HashMap<RoomId, (RecordingMeta, Option<String>)>>>,
    |                    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    = help: ... clippy::type_complexity
error: could not compile `vks-server` (lib) due to 1 previous error
```

Команда из п.1 задания (`-p vks-recorder -p vks-server -p vks-admin --all-targets -D warnings`) завершается 101. Это ровно новый тип, добавленный `f602f72`; `vks-recorder` и `vks-admin` по отдельности чистые. Лечится `type StoppedCache = Arc<Mutex<HashMap<RoomId, (RecordingMeta, Option<String>)>>>;`. Формально это P2 (код компилируется), но гейт приёмки в текущем виде не проходит.

## §2. P1-1 (residual): group loss финализирует сессию, но оставляет ACTIVE-строку в storage

### 2.1. Что проверялось

Сценарий: реальная x264-группа (`acquire_video_viewer`), `RecorderControl::start` (tap на существующую группу), 30 кадров, `drop(viewer)` (последний зритель ушёл → `RecvError::Closed`), ожидание финализации, затем новая группа, новый `start`, 30 кадров, `stop`. Отдельно — тот же сценарий с `AppState::with_storage(SqliteStorage)` (production-путь) и контрольные прогоны: обычный admin `stop` и `MemoryStorage`.

### 2.2. Что работает (in-memory)

Probe (review-only, `AppState::new`, storage=None) — 3 прогона:

```text
PROBE1 finalized=true in=25.530031ms roomhub_participants=0
  row=RecordEntry { duration_sec: 1, size_bytes: 2952, ... }
PROBE1 container1_present=true len=Some(2952)
PROBE1 id2=d706e90e-... new_id=true
PROBE1 container2_len=2955 id2=d706e90e-...
```

- сессия действительно финализируется за ~25 мс после `drop(viewer)`;
- `manager.is_recording == false`;
- recorder-участник из RoomHub снят (`participants=0`);
- in-memory `state.records` имеет `duration_sec=1`, `size_bytes>0`;
- новый `start` даёт **новый** record id (не stale success), и при здоровом энкодере второй файл содержит кадры (2955 байт).
- Ошибка writer'а в tap (см. §4) тоже завершает сессию и реплеится из `stop`.

То есть прежний P1-1 «сессия остаётся Running, участник висит, повторный start не переподписывается» **в in-memory-модели закрыт**.

### 2.3. Что не работает: durable-строка остаётся ACTIVE

Probe с `SqliteStorage` (production-подобный `AppState::with_storage`), 3 прогона — одинаково:

```text
PROBE4 before_db=[RecordEntry { duration_sec: 0, size_bytes: 0, ... }]   // строка создана start'ом
PROBE4 finalized=true
  mem=RecordEntry { duration_sec: 1, size_bytes: 2897, ... }
  db =RecordEntry { duration_sec: 0, size_bytes: 0, ... }                // <-- не обновлена
```

Контрольные прогоны, доказывающие, что дело не в storage:

```text
PROBE5 (тот же flow, но обычный control.stop): db duration_sec=1, size_bytes=2930
PROBE6 (group loss, MemoryStorage — ready-future): db duration_sec=1, size_bytes=2864
```

`MemoryStorage::upsert_record` завершается на первом poll (ready), поэтому self-abort не успевает её отменить; SQLite/Postgres дают pending-future, и отмена срабатывает. Механизм:

1. `start_recording_inner` кладёт `AbortHandle` tap-таска в `taps` (`recorder.rs:293-304`).
2. При естественном выходе tap-таск вызывает `finalize_after_tap_stop` (`recorder.rs:302`, `:473-483`), тот — `stop_recording_async`.
3. `stop_recording_internal` достаёт **свой же** `AbortHandle` и вызывает `handle.abort()` (`recorder.rs:356-358`); для текущего таска это выставляет cancel-флаг.
4. Синхронная часть успевает сохранить контейнер, снять участника, обновить `state.records` и `stopped_cache`; затем `stop_recording_async` доходит до первого pending-await — `storage.upsert_record(&entry).await` (`recorder.rs:506-508`) — и таск отменяется. Durable-запись теряется.
5. API читает именно storage: `GET /api/v1/records` возвращает `storage.list_records` (`crates/vks-admin/src/handlers.rs:671-678`), `GET /api/v3/videorecords` — `collect_records` → `storage.list_records` (`crates/vks-admin/src/api_v3/observability.rs:675-681`), а `active` вычисляется как `duration_sec == 0` (`observability.rs:650-654`).

Итог: после потери группы админ-API показывает запись **active=true, size=0**, хотя файл (в памяти менеджера) уже финализирован; после рестарта строка гидрируется из storage (`state.rs:632-637`) и остаётся ACTIVE до следующего `start` (который удаляет stale-строки, `recorder.rs:59-68`) или ручного DELETE. Это ровно тот класс «ACTIVE leftovers», который закрывал P1-1, и он детерминирован на основном production-пути (`state.storage` в `main.rs` всегда `Some`). Тот же self-abort затрагивает и автоматическую финализацию после ошибки writer'а (§4): `stop` вернёт ошибку, но durable-строка тоже останется ACTIVE.

Коммит-тест `recording_finalizes_when_encoder_group_closes` этого не ловит, потому что использует `AppState::new()` (`tests/recorder_tap.rs:216`), где `storage=None`.

**Рекомендация:** не вызывать `stop_recording_async` из таска, чей `AbortHandle` абортится; например, финализировать в отдельном `tokio::spawn` (`manager.clone().stop_recording_async(...)`), либо не абортить tap, если это текущий таск (передавать `AbortHandle`/флаг), либо сначала снимать handle из `taps`, а persistence выполнять в независимом таске. После исправления добавить тест с `AppState::with_storage(SqliteStorage)` и проверкой `storage.list_records(room).duration_sec >= 1` после group loss.

## §3. Повторный stop (прежний P2-1 review A/B): реплей ошибки — подтверждено

```text
PROBE2 first ="recording container finalize failed: контейнер пуст: нет ни одного видеосэмпла"
       second="recording container finalize failed: контейнер пуст: нет ни одного видеосэмпла"
       equal=true
```

- `RecorderControl::stop` при `!is_recording` сначала смотрит `stopped_finalize_error` (`recorder.rs:93-97`) и возвращает сохранённую ошибку, а не 200/Ok.
- Committed-тест `repeated_stop_replays_finalize_error` (`tests/recorder_tap.rs:259-279`) проходит во всех 12 прогонах.
- Session-level поведение не изменено: `container_finalize_error_is_surfaced_not_swallowed` (`crates/vks-recorder/src/recording.rs:576-587`) утверждает, что второй `RecordingSession::stop` тоже возвращает ошибку; 54 lib-теста `vks-recorder` прошли.

Замечание P3: при прямом вызове `RecorderManager::stop_recording_async` (не через `RecorderControl`) реплей из `stopped_cache` заворачивает уже префиксованную строку в `RecordingError::Container` (`recorder.rs:342-347`), и Display добавляет префикс второй раз (`recording container finalize failed: recording container finalize failed: …`). Семантика Err сохраняется, но текст дрейфует. Через `RecorderControl` путь не затронут.

## §4. Ошибка tap-writer'а (прежний P2-2 review B): поверх stop — подтверждено исполнением

Чтобы получить ошибку writer'а, **при которой контейнер всё ещё финализируется**, в probe создан собственный `FarmTap` (публичные поля) и в него переслан реальный `Arc<AccessUnit>` от x264-группы в обратном порядке: старый PTS после нового → `ContainerError::InvalidTimestamp` на записи, при этом ранее записанные сэмплы остаются. Результат:

```text
PROBE3 captured=12 pts=[... 256658 ...]
PROBE3 finalized=true container_present=true len=Some(2468)
  first_stop =Err("некорректный timestamp 256658 us")
  second_stop=Err("некорректный timestamp 256658 us")
```

- tap-таск вернул `Some(error)` (`recorder_tap.rs:65-69`), ошибка сохранена в `tap_errors` (`recorder.rs:295-296`), затем `stop_recording_internal` забрал её (`recorder.rs:367`) и положил в `stopped_cache` (`:397`);
- `stop` вернул **Err**, а не 200/Ok, хотя `finalize` был успешным и файл (усечённый на один AU) существует; файл `tap_writer_error.mp4` (2468 байт, 12 сэмплов) полностью декодируется (см. §6);
- повторный `stop` вернул ту же ошибку.

Коммит-теста на этот путь нет (P3-6), проверено только review-only probe. Остаточная гонка: если пользовательский stop успеет забрать `tap_errors` до того, как tap-таск вставит ошибку (`recorder.rs:295-296` vs `:367`), stop вернёт Ok, а ошибка останется в `tap_errors` навсегда (P3-3).

## §5. B-кадры/ctts (прежний P2-3 review B): допущение зафиксировано

`crates/vks-recorder/src/container.rs:14-19` теперь явно документирует: B-кадров нет, `ctts` не пишется, `AccessUnit` несёт только PTS; при энкодере с B-кадрами writer обязан получить DTS/ctts, иначе `stts`/порядок будут неверны. Это закрывает P2-3 в редакции «зафиксировать допущение» (не «реализовать ctts»). На реальном tap-файле `ffprobe` даёт `has_b_frames=0`, `ctts` в боксах отсутствует. Замечание P3: committed-тесты по-прежнему не утверждают `has_b_frames=0`; проверка только в этом ревью.

## §6. Контейнер: независимая проверка реального tap-файла

Метод: собственный Python-парсер боксов (не код репозитория), `ffprobe -count_frames`, полный `ffmpeg -v error -i … -f null -`. Файлы получены реальным recorder-tap'ом с x264 (640×360@30, `repeat-headers=1:aud=1:scenecut=0`):

| Файл | Байт | Сэмплов | Боксы/поля | Декод |
|---|---:|---:|---|---|
| `group_loss.mp4` (финализация по потере группы) | 2952 | 26 | `ftyp(32)+moov(1038)+mdat(1860)`, `avc1/avcC` profile 66 level 31 lengthSize 4, SPS 24/PPS 4, `mdhd` 90000/79927, `stts` sum=79927, `stss=[1]` = IDR, `stco[0]=mdat.payload`, 0 Annex-B в NAL, 55 NAL, trak только `vide` | **26/26**, `ffmpeg` exit 0, **0** строк stderr |
| `second_start.mp4` (после повторного start) | 2955 | 25 | та же структура; `stts` sum=77015, `stss=[1]` | **25/25**, exit 0 |
| `tap_writer_error.mp4` (усечён writer error) | 2468 | 12 | та же структура; `stts` sum=39748, `stss=[1]` | **12/12**, exit 0 |

`ffprobe` на `group_loss.mp4`:

```text
codec_name=h264
codec_tag_string=avc1
is_avc=true
nal_length_size=4
time_base=1/90000
has_b_frames=0
nb_read_frames=26
extradata_size=39
```

Проверено: top-level ровно `ftyp, moov, mdat` один раз и в порядке; размеры боксов сходятся; `avcC` — ребёнок `avc1`; `stco[i]` указывает точно на `mdat.payload_start + Σ(stsz[0..i])`; `Σ stts.count == stsz.count`, `Σ stts.delta == mdhd.duration`; `stss` совпадает с фактическими IDR-сэмплами; **ни одного Annex-B start code** внутри length-prefixed сэмплов; **ни одного `mp4a`/audio trak** (фейкового аудио нет). `ctts`/`elst` отсутствуют.

Синтетический AAC/`esds` не сломан: в 54 lib-тестах `vks-recorder` проходит `esds_aac_lc_asc_roundtrip` и `audio_samples_without_config_are_rejected_and_no_fake_track_is_written`; `ffprobe_acceptance` на реальном ADTS AAC:

```text
ffprobe aac: codec=Some("aac") profile=Some("LC") time_base=Some("1/48000") frames=Some("49") duration=Some("1.045333")
test result: ok. 2 passed; 0 failed
```

То есть контейнерная приёмка (`avc1`, `is_avc`, `nal_length_size`, 90000, `stco/stsz/stts/stss`, полный decode, отсутствие фейкового `mp4a`, живой `esds`) **подтверждена независимо**.

## §7. Гейты и флейки

- `cargo test -p vks-recorder`: **54 lib + 2 integration = 56 passed, 0 failed**; 0-test doc-target.
- `cargo test -p vks-server --lib`: **149 passed, 0 failed, 3 ignored**.
- `cargo test -p vks-server --test recorder_tap`: 12 прогонов, 11 зелёных, 1 красный:
  - run 3: `3 passed; 2 failed` (15.14 s):
    - `recording_finalizes_when_encoder_group_closes` — `tests/recorder_tap.rs:246`: `recorded prefix saved as MP4` (container `None`);
    - `recording_tap_reuses_existing_group_without_becoming_a_viewer` — `tests/recorder_tap.rs:115`: `stop finalizes a non-empty container: "…контейнер пуст…"`.
  - Итого **58 passed / 60** исполнений; прогоны 4–12 (включая run 9 длительностью 19.75 s) зелёные.
  - Флейк возник при load average ≈ 24–28 (параллельно шли чужие сборки/тесты); симптом — энкодер не отдал ни одного AU за окно теста, tap финализировал пустой контейнер. При load ≈ 16 (прогоны 10–12) не воспроизводился. Это не доказательство логической ошибки `f602f72`, но приёмочный тест **недетерминирован под нагрузкой** и не должен считаться надёжным доказательством без запаса/повторов. Наш restart-probe один раз из трёх дал тот же симптом на втором старте; при здоровом энкодере второй файл записывался (2955 байт).
- `cargo test --workspace --locked --offline --no-fail-fast`: **exit 0, 1528 passed, 0 failed, 8 ignored** (51 target).

## §8. Список находок

### P1

**P1-1 (residual; T09b).** Автофинализация после потери группы оставляет durable-строку ACTIVE: `RecorderManager::finalize_after_tap_stop` (`crates/vks-server/src/recorder.rs:473-483`) вызывается внутри tap-таска, `stop_recording_internal` абортит его же `AbortHandle` (`recorder.rs:356-358`), и `stop_recording_async` отменяется на pending `storage.upsert_record` (`recorder.rs:506-508`). In-memory строка финализирована, SQLite/Postgres — `duration_sec=0/size_bytes=0`. API читает storage: `handlers.rs:671-678`, `api_v3/observability.rs:675-681`, `active = duration_sec == 0` (`observability.rs:650-654`). Детерминировано с SQLite (3/3), не воспроизводится с ready-only `MemoryStorage` (там строка обновляется) и не воспроизводится на обычном `control.stop` (там строка обновляется). Прямо противоречит пункту приёмки «row duration>=1 not ACTIVE» и утверждению (a) «no ACTIVE leftovers». Тот же дефект — на пути автофинализации после tap-writer error.

### P2

**P2-1.** `cargo clippy -p vks-server --all-targets --offline -- -D warnings` = **101**: `type_complexity` на новом поле `stopped_cache: Arc<Mutex<HashMap<RoomId, (RecordingMeta, Option<String>)>>>` (`crates/vks-server/src/recorder.rs:136`). Обязательный гейт п.1 красный; `vks-recorder`/`vks-admin` clippy чистые.

**P2-2.** Флейк приёмочных tap-тестов под нагрузкой: 2/5 fail в 1 из 12 прогонов (58/60 исполнений), `recorder_tap.rs:115` и `:246`; симптом — пустой контейнер из-за отсутствия AU. Воспроизводится только при load average ≈ 24–28; при 16 — 4/4 зелёных. Приёмка не воспроизводима на загруженном стенде без ретраев/увеличенных таймаутов.

### P3

1. **P3-1.** Гонка `finalize_after_tap_stop` с новым `start`: проверка `is_recording` (`recorder.rs:474`) и последующий `stop_recording_async` (`:477`) не сверяют поколение/record id; новый сеанс, вставленный между ними, может быть остановлен старым финализатором.
2. **P3-2.** Окно stale-success в `RecorderControl::start` осталось: idempotent-ветка `recorder.rs:51-55` вернёт старый id, если сессия ещё не финализирована; окно после `Closed` — единицы/десятки мс (probe: ~25 мс), но теоретически существует.
3. **P3-3.** Гонка `tap_errors`: вставка (`recorder.rs:295-296`) и изъятие пользовательским stop (`:367`) не синхронизированы; stop может вернуть Ok, а ошибка останется в `tap_errors` без потребителя (утечка записи, ошибка не всплывёт).
4. **P3-4.** Прямой `RecorderManager::stop_recording_async` после ошибки даёт двойной префикс в тексте (`recorder.rs:342-347`, `:397`); через `RecorderControl` путь корректен. Семантика Err не страдает.
5. **P3-5.** `stopped_cache` растёт по комнатам без ограничения/очистки (`recorder.rs:136`, `:398`) — прежний паттерн, теперь хранит и ошибки.
6. **P3-6.** Нет committed-регрессии на P2-2 (tap writer error → stop Err при успешном `finalize`); проверено только review-probe. Committed-тест group-loss после второго `start` не проверяет кадры: он публикует кадр до `start` (`tests/recorder_tap.rs:250`), затем игнорирует результат `stop` (`:253`), поэтому «frames recorded again» тестом не покрыто (наш probe покрывал и при здоровом энкодере дал 2955 байт).

## §9. Приёмка по пунктам задания

| Пункт | Статус |
|---|---|
| 1. Pristine buildability (`check`/`clippy -D warnings`, без shim) | **check — да; clippy — нет** (P2-1); shim не нужен |
| 2. P1-1: group loss → finalize, RoomHub gone, row finalized, new start new id, frames again | **in-memory — да; durable/API row — нет** (P1-1); новый id и кадры — да (при здоровом энкодере) |
| 3. P2-1: repeat stop Err = first Err | **да** (probe + committed test) |
| 4. P2-2: tap writer error surfaced by stop, not silent 200 | **да** (probe: `InvalidTimestamp`, Err дважды; см. §4); committed-теста нет |
| 5. Container acceptance (ffprobe/avc1/is_avc/nal 4/90000/stco-stsz-stts/stss/decode/no fake mp4a/esds) | **да** (3 реальных tap-файла + независимый парсер + decode) |
| 6. Gates (fmt/clippy/tests/workspace, counts, flakes) | fmt/check/tests/workspace — да; clippy — нет; флейк tap — да |
| 7. Hygiene | да: живая БД/порты/бэкапы не тронуты; в репозитории только этот отчёт |

## §10. Итог

`f602f72` — шаг вперёд: дерево снова собирается (`check`/workspace/fmt/тесты зелёные), контейнер подтверждён на реальном tap-файле, повторный stop и ошибка writer'а честно всплывают, допущение о B-кадрах зафиксировано. Но **done: no**: остаётся детерминированный P1 — после потери группы durable-строка и API показывают запись активной (self-abort tap-таска съедает `storage.upsert_record`), плюс красный clippy-гейт и флейк приёмочных tap-тестов под нагрузкой.

**Score: 8.5/10. done: no.**
