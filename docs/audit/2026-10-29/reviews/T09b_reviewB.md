# T09b — независимое ревью B: MP4-контейнер + tap существующей группы

- **Дата:** 2026-10-29, 13:00–14:25 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент); живой репозиторий только читался, изменён только этот файл.
- **Объект:** коммит `8e4197f19980277900813559220f2e7ed3cc250c` «T09b: record existing encoder farm stream into a real MP4» (parent `fff51a7`).
- **Полигон:** `git archive 8e4197f` → `/dev/shm/t09b_reviewB` (976 файлов, без `.git`); `CARGO_TARGET_DIR=/home/pro/.cache/t09b_reviewB_target`; Rust **1.94.0** (`rust-toolchain.toml`), `--offline`, `--locked` где применимо. Временные probe-тесты лежали только в копии и удалены; в копии есть **review-only shim** из двух функций `vks-common` (см. §1), production-файлы T09b не менялись (sha256 `container.rs` = `a2372288…` совпадает с коммитом).
- **Чистота:** живая `vks_data.db`, `backups/`, живой `target/`, порты и рабочие процессы не открывались. Все MP4/JSON/probe-артефакты — в `/dev/shm`; в `/tmp` копий не создавалось. В репозитории появился только этот файл.
- **Pitfalls:** [`docs/audit/2026-10-29/pitfalls/T09b.md`](../pitfalls/T09b.md). Acceptance — [`MASTER_TASK_LIST_2026-10-29.md:48`](../../../progress/MASTER_TASK_LIST_2026-10-29.md#L48).

## Вердикт

**T09b по существу закрыт: контейнер пишет реальный `avc1`-MP4, который полностью декодируется ffmpeg; tap не спавнит скрытую группу, не становится зрителем, переживает исчезновение группы и сохраняет RoomHub при ошибке finalize. Собственных P0/P1 у T09b не найдено.**

Но **done: no** по формальному критерию «0 P0/P1 на проверяемом объекте»: pristine-копия `8e4197f` **не собирает `vks-admin`/`vks-server`** из-за двух функций, отсутствующих в `vks-common` на этом коммите (P1 ниже). Это дефект порядка коммитов T04b/T09b, а не код T09b; на текущем HEAD `a02daad` (T04b) функции уже есть. На `8e4197f` новые `recorder_tap`-тесты физически неисполнимы без review-only shim.

**Score: 9.0/10.** До 9.5 не хватает именно сертифицируемости объекта: acceptance-тесты tap нельзя запустить на pristine `8e4197f`; плюс три P2 (повторный stop скрывает ошибку finalize, ошибка tap не пробрасывается, допущение «B-кадров нет» не зафиксировано тестом).

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** — pristine `8e4197f` не компилируется (`vks-admin`/`vks-server`); **в самом T09b P1 = 0** |
| **P2** | **3** |
| **P3** | **7** |

Главное:

- MP4 проверен **независимо** (свой Python box-parser + ffprobe/ffmpeg 8.0.1): `ftyp/moov/mdat` ровно один раз и в правильном порядке, размеры сходятся, `avcC` — ребёнок `avc1`, `lengthSizeMinusOne=3`, SPS/PPS байт-в-байт из потока, `stts`/`stsc`/`stsz`/`stco`/`stss` согласованы, `stss` совпадает с фактическими IDR, Annex-B в сэмплах отсутствует, **60/60 кадров декодируются с 0 warning/error**. Дополнительно: 1 кадр, VFR-разрыв 0.5 с, duplicate PTS, 1920×1080, High-profile `avcC` с extension, AAC/esds — всё декодируется.
- Tap: без зрителей `start` → явная ошибка `no_active_h264_encoder_group`, `group_count=0`; с зрителем refcount/viewers **не меняются**, второй subscriber получает те же AU, при исчезновении группы `Closed` → tap завершается, скрытая группа не появляется; `stop` при активном tap и при исчезнувшей группе не паникует и снимает recorder-участника RoomHub.
- **P2-1:** повторный `DELETE`/`stop` после ошибки finalize возвращает **200/Ok**, а не ту же ошибку (`recorder.rs:317-318`); probe: первый stop — `Err("…контейнер пуст…")`, второй — `Ok(true)`. Это противоречит committed-тесту `RecordingSession` (`recording.rs:586`) и обещанию «repeat stop returns the same error».
- **P2-2:** ошибка writer'а в tap (`ParameterSetsChanged`, `StorageFull`, `InvalidTimestamp`) только логируется и завершает tap (`recorder_tap.rs:59-61`); если finalize потом успешен, `stop` возвращает 200 с молча усечённым MP4.
- **P2-3:** writer не пишет `ctts` и не проверяет наличие B-кадров; допущение «baseline без B» не зафиксировано ни в docs, ни тестом (`has_b_frames` в acceptance не проверяется), хотя pitfalls T09b P2-2 это прямо требует. Production сейчас baseline (`gateway_video.rs:69`), поэтому практический риск низкий.

---

## §1. Pristine-сборка: P1 (база/T04b, не T09b)

На копии `git archive 8e4197f`, **до** добавления review-only shim:

```text
$ cargo +1.94.0 check -p vks-server --all-targets --offline
error[E0425]: cannot find function `audio_unavailable_reason` in module `vks_common::codec_capabilities`
  --> crates/vks-admin/src/api_v3/shell.rs:577:58
error[E0425]: cannot find function `video_unavailable_reason` in module `vks_common::codec_capabilities`
  --> crates/vks-admin/src/api_v3/shell.rs:605:59
error: could not compile `vks-admin` (lib) due to 2 previous errors
[exit 101]
```

Статическое подтверждение:

```text
$ git show 8e4197f:crates/vks-admin/src/api_v3/shell.rs | grep -n unavailable_reason
577: ... audio_unavailable_reason ...
605: ... video_unavailable_reason ...
$ git show 8e4197f:crates/vks-common/src/codec_capabilities.rs | grep -c unavailable_reason
0
```

То же самое уже на parent `fff51a7` (0 совпадений в `vks-common`, вызовы в `shell.rs`), то есть **T09b это не внёс**. Функции появились только в следующем коммите T04b `a02daad` (8 совпадений); на текущем HEAD они есть.

**Последствие:** `vks-server` (а значит и три новых `recorder_tap`-теста) на `8e4197f` не компилируется. Заявленные в сообщении коммита «workspace 1492 passed» на pristine `8e4197f` невоспроизводимы: они могли быть получены только на рабочем дереве с незакоммиченным T04b. Для проверки server-гейтов я добавил в копию две review-only функции-заглушки; это не меняет T09b-код и не является доказательством корректности T04b.

**Что это значит для приёмки:** сам T09b чист, но объект `8e4197f` нельзя сертифицировать как самодостаточный. Рекомендация Lead: принимать T09b на `a02daad` (или после rebase на него), а не на `8e4197f`; тогда P1 снимается. В таблице он оставлен как P1 именно потому, что проверяемый объект — `8e4197f`.

---

## §2. Независимая проверка MP4

Метод: собственный Python-парсер боксов (рекурсивно `moov/trak/mdia/minf/stbl`, `stsd`, `avc1/avcC`, `mp4a/esds`), плюс `ffprobe -count_frames` и полный декод `ffmpeg -v error -i file -f null -` с подсчётом строк stderr. Генерация — реальным x264/AAC, не только committed-тестами.

| Файл | Сэмплов | Длительность | Ключевые проверки | Декод |
|---|---:|---:|---|---|
| `h264_60.mp4` (30 fps) | 60 | 2.000000 с, mdhd 180000/90000 | ftyp 32 + moov 1115 + mdat 492166 = 493313; avcC profile 66/compat 192/level 30, SPS 25, PPS 4, lengthSize=4; `stss=[1,31]` = фактические IDR; stco непрерывен, stsz==mdat payload; `stts` sum=180000; `elst`/`ctts` нет; **0** Annex-B в NAL | 60/60, **0 error, 0 warning** |
| `h264_kf.mp4` | 60 | 2.0 с | 4 IDR, `stss` совпадает | 60/60, 0 |
| `h264_gap.mp4` | 60 | 2.5 с, mdhd 225000 | PTS-разрыв +500 мс; `stts` sum=225000; ffprobe avg 24/1 (VFR) | 60/60, 0 |
| `h264_dup.mp4` | 20 | 60001 тик (0.666678 с) | duplicate PTS принят, delta=1 тик; ffmpeg пишет 1 error `non monotonically increasing dts … 9 >= 9`, exit 0 | 20/20, 1 error |
| `h264_one.mp4` | 1 | 3000 тик (0.033333 с) | одиночный IDR, `stss=[1]`, размеры сходятся | 1/1, 0 |
| `h264_hd.mp4` | 10 | 1920×1080 | avcC level 40, 666743 байт | 10/10, 0 |
| `h264_high.mp4` | 15 | High profile | extension `fd f8 f8 00`, ffprobe profile=High | 15/15, 0 |
| `aac.mp4` | 49 audio | 1.045333 с, mdhd 50176/48000 | `mp4a` 2ch/48000, `esds` tags 0x03/0x04/0x05/0x06, ASC `11 90`, offsets непрерывны, ADTS-заголовков нет | 49/49, 0 |
| `tap.mp4` (из server-пробы) | 56 | 1.955567 с | 56 == `GroupStat.frames`; `stss=[1,31]` = IDR; refcount 2/viewers 1 до и после | 56/56, 0 |
| `tap_group_gone.mp4` | 15 | 0.494744 с | группа исчезла, скрытой группы нет, `subscribe` после drop = None | 15/15, 0 |

Дополнительно для каждого видео проверено:

- top-level ровно `ftyp`, `moov`, `mdat`, `moov` **до** `mdat`; заявленные размеры боксов равны фактическим;
- `stco[i]` указывает точно на `mdat.payload_start + Σ(stsz[0..i])`, сумма `stsz` == payload `mdat`;
- все сэмплы начинаются с 4-байтовой длины NAL; **ни один NAL не начинается с Annex-B start code**, и внутри NAL нет `00 00 01` (365 NAL в `h264_60`, 117 в `tap`, 0 совпадений);
- `stss` — ровно индексы сэмплов, содержащих NAL type 5 (IDR), без «фейкового» первого кадра;
- `mdhd`/`mvhd`/`tkhd` duration согласованы; edit list отсутствует; `ctts` отсутствует (см. P2-3);
- `avcC` лежит внутри `avc1` (не рядом), `lengthSizeMinusOne=3`, SPS/PPS из `avcC` байт-в-байт равны SPS/PPS первого AU; High-profile extension присутствует;
- AAC: `esds` реально вложен в `mp4a`, `AudioSpecificConfig=0x11 0x90` (AAC-LC, 48 кГц, 2 ch), `stts` = 49×1024, аудио-оффсеты идут подряд без ADTS.

Это закрывает обязательные пункты «MP4 plays ffprobe/ffmpeg; avcC/esds/timescale/stco correct» для writer'а. Оговорка по `esds`: в **production-записи** аудио нет (§6, P3-7), поэтому `esds` проверен только синтетическим тестом, а не tap-путём.

---

## §3. Tap: non-owning подписка и lifecycle

Проверено кодом и серверными пробами (5 сценариев, все прошли):

1. **Нет группы — нет записи.** `RecorderControl::start` без зрителей возвращает `no_active_h264_encoder_group`; `video_farm().group_count()==0`, `video_farm_stats().is_empty()`, recorder-участник и строка записи не создаются (`recorder_tap.rs:60-76`). Скрытого FFmpeg нет: `subscribe` не вызывает spawner/insert, только `tx.subscribe()` (`farm.rs:429-451`).
2. **Refcount не меняется.** До/после `start` при одном зрителе: `refcount=2`, `viewers=1`, `group_count=1`; tap не зритель (`recorder_tap.rs:101-108`).
3. **Multi-subscriber.** Два независимых tap на одну группу получили по 18 AU, ключ группы одинаков, refcount/viewers не изменились. Broadcast не «съедает» AU у второго.
4. **Группа исчезла.** После drop зрителя и ретира группы: `group_count=0`, `subscribe` = None; tap получил `Closed`; `stop` вернул `Ok` с 15-кадровым/2567-байт MP4, новых групп 0. Скрытой группы не появилось.
5. **Stop при активном tap.** 3× сразу после start (без кадров): все три `Err(Empty)`, без паники; `is_recording=false`, RoomHub=0, группа осталась (`group_count=1`). E2E с кадрами: stop после 45 опубликованных кадров даёт валидный MP4, строка 640×360/1024 kbps/duration 2/size=4664.

RoomHub: при ошибке finalize `stop_recording_internal` всё равно снимает recorder-участника (`recorder.rs`, detach после stop) — проба `room_hub.list_participants == 0`. При успешном stop — тоже 0; чужие участники не затрагиваются (код удаляет только `recorder-{room}`).

`FarmTap` держит только `EncoderKey`, `broadcast::Receiver` и snapshot SPS/PPS; сам `Arc<Group>` не держит, поэтому не продлевает жизнь группы. `Closed` после ретира — ожидаемая семантика, не ошибка.

---

## §4. P2-1: повторный stop скрывает ошибку finalize

Код:

```rust
// crates/vks-server/src/recorder.rs:92-114
async fn stop(&self, room_id: RoomId) -> Result<(), String> {
    let state = self.state.upgrade().ok_or_else(|| "server state is gone".to_string())?;
    if !self.recorder.is_recording(&room_id) {
        // ... при storage Some финализирует duration-0 строки ...
        return Ok(());                       // <- ошибка finalize уже потеряна
    }
    self.recorder.stop_recording_async(&state, &room_id).await.map(|_| ()).map_err(|error| error.to_string())
}
```

Плюс сам менеджер в cache-ветке (`crates/vks-server/src/recorder.rs:317-318`) возвращает `StopOutcome { meta, entry: None, finalize_error: None }`, то есть теряет сохранённую ошибку и при прямом вызове `stop_recording_async`.

`stop_recording_async` (`:448-453`) и `RecordingControl::stop` (`:114`) возвращают `Ok`, то есть admin `DELETE` — **200**.

Проба (in-memory `AppState`, комната без кадров):

```json
{"first":"Err(\"recording container finalize failed: контейнер пуст: нет ни одного видеосэмпла\")",
 "second":"Ok(true)",
 "container_present":false,
 "roomhub_participants":0}
```

Первый stop корректно отдаёт 500 и удаляет сессию/контейнер; повторный — 200, хотя файла нет, а строка осталась `duration_sec=1, size_bytes=0`. `RecordingSession` сам хранит `finalize_error` и на повторный `stop()` возвращает ту же ошибку (`recording.rs:338-340`, тест `:576-586`), но `RecorderManager::stopped_cache` хранит только `RecordingMeta`.

**Влияние:** DELETE-retry после 500 (типовое поведение UI/клиента) маскирует провал finalize; историю можно принять за успешно завершённую запись с 0 байт. Новых данных при этом не теряется — потому P2, не P1.

**Фикс:** кэшировать `(RecordingMeta, Option<RecordingError>)` (или `StopOutcome`) и возвращать сохранённую ошибку на повторном stop; в `RecordingControl::stop` при `!is_recording` сначала смотреть в этот cache, а не сразу отдавать `Ok(())`; добавить тест «stop дважды после Empty → оба раза Err».

---

## §5. P2-2: ошибка tap не пробрасывается в stop

```rust
// crates/vks-server/src/recorder_tap.rs:56-62
let result = session.set_video_parameter_sets(&parameter_sets).and_then(|()| {
    session.write_video_sample(access_unit.data(), access_unit.pts_us(), access_unit.is_idr())
});
if let Err(error) = result {
    warn!(room = %room, %error, "recorder tap stopped: writer rejected a video sample");
    break;
}
```

Tap завершается, но ни `RecordingSession`, ни `RecorderManager` не узнают причину. `stop` затем вызывает `session.stop()`: если контейнер непустой и finalize успешен, возвращается `Ok` (`recorder.rs:448-453`), admin отдаёт 200. Так `ParameterSetsChanged` (смена SPS/PPS), `StorageFull` или `InvalidTimestamp` в середине записи дают **молча усечённый** MP4, неотличимый от полного.

В committed-тестах этот путь не покрыт: `recorder_tap.rs` проверяет только «нет группы» и happy path/empty. По коду это fail-open на уровне API (хотя writer fail-closed). P2.

**Фикс:** сохранить ошибку tap в `RecordingSession`/`RecorderManager` (например, `tap_error: Option<...>`), на stop вернуть её как finalize-ошибку или отдельный 5xx; добавить тест со сменой SPS.

---

## §6. P2-3: B-кадры / `ctts` — допущение не зафиксировано

- В `container.rs` нет `ctts` (grep 0); `video_stts_deltas` (`:871-879`) строит `stts` из `pts_us` эмиссии AU, а `build_video_trak` пишет только `stts`. Writer принимает Main/High SPS и не проверяет `has_b_frames`.
- `MASTER_TASK_LIST`/pitfalls T09b P2-2 требуют **зафиксировать** допущение «B-кадров нет» в docs/тесте (`ffprobe has_b_frames=0` на фикстуре). В `ffprobe_acceptance.rs:311` поля `has_b_frames` нет; в `recorder_tap.rs` — тоже. Мой независимый ffprobe на фикстуре дал `has_b_frames=0`, но committed-тест этого не утверждает.
- Production сейчас действительно baseline (`crates/vks-server/src/gateway_video.rs:69`), а x264 baseline не содержит B; поэтому реальный MCU-путь безопасен. Но публичный writer принимает и High-profile поток, для которого MP4 без `ctts` даст неверный порядок показа.

P2 как незакрытое явное требование pitfalls; не блокер текущего baseline-пути.

---

## §7. P3 (7 пунктов)

1. **Duplicate PTS молча клампится до 1 тика.** `container.rs:216-219` отвергает только `pts_us < last`, равенство пропускает; `video_stts_deltas:874` делает `.max(1)`. Проба `h264_dup.mp4`: 20 сэмплов, delta=1, mdhd 60001; ffmpeg пишет `non monotonically increasing dts … 9 >= 9` (exit 0). Нет ни `warn!`, ни явного `InvalidTimestamp` для duplicate. Лучше логировать/отвергать duplicate, а не молча менять таймлайн.
2. **Оценка quota 16 байт/сэмпл занижена для худшего VFR.** `container.rs:438-443`: 16·N + 8192; худший `stbl` — `stsz` 4 + `stco` 4 + `stts` 8 (уникальная delta на каждый сэмпл) + `stss` 4 (все keyframes) = 20·N. Точная проверка в `finalize` есть, поэтому fail-closed, но `StorageFull` может прийти только на stop после записи всего media_data, а не на write. Для near-quota это поздняя ошибка.
3. **`descriptor()` кодирует длину не по ISO 14496-1 для payload ≥ 0x80.** `container.rs:835-849` использует форму «`0x80|N`, затем N полных байт» (`0x81`, `0x82`, `0x83`), тогда как длина MPEG-4 descriptor — это 7-bit continuation: каждый байт `0x80|data`, старший бит = «есть ещё байт». Для `len=0x80` код пишет `0x81 0x80`: spec-парсер прочитает `0x81` (data=1, продолжение), затем `0x80` (data=0, продолжение) и потребует третий байт — запись невалидна. Сейчас `aac_lc` даёт ASC 2 байта, поэтому все длины < 0x80 и баг недостижим; публичный `AudioConfig.asc` допускает произвольную длину.
4. **Гонка 409 → 500.** Admin делает pre-check `active_h264_groups` (`observability.rs:780`) и отдаёт 409; если группа ретируется до `control.start`, `subscribe` возвращает `None`, а `start` в admin маппит **любую** ошибку в 500 (`observability.rs:799-801`), хотя причина — тот же `no_active_h264_encoder_group`. Комментарий `recorder.rs:69-75` обещает закрыть гонку, но статус-код не 409.
5. **Ошибка persist на start оставляет активный tap/сессию.** `recorder.rs:80-86`: после `start_recording_with_tap` (сессия и tap уже живы) ошибка `upsert_record` возвращает 500, но не откатывает сессию/tap. Повторный `start` идёт в idempotent-ветку `:51-56`, которая возвращает существующий id **без повторного upsert**. При транзиентном сбое SQLite запись идёт без строки в БД; retry — 200, но строки всё ещё нет; при падении процесса история теряется. Паттерн унаследован от T09a, но с tap стал заметнее.
6. **Acceptance-тесты слабы по кадрам/keyframes.** `ffprobe_acceptance.rs:391` проверяет только `stss[0]==1`, а не весь `stss`; `recorder_tap.rs:164` — `frames > 10`, а не `frames == опубликовано`/точное число. Моя независимая проверка дала 60/60 и 56/56, но регрессия «потеряли половину кадров» или «keyframe-флаг не тот» пройдёт committed-тесты. Нужны точные `nb_read_frames` и полный `stss` vs фактические IDR.
7. **`esds` не проверен production-путём.** `container_config_for` ставит `audio: None` (`recorder.rs:473-475`), все tap-записи video-only; AAC/esds проверяется только синтетическим `ffprobe_acceptance` (`:470-507`). Решение «пока video-only» задокументировано в сообщении коммита и комментарии, но master-acceptance (строка 48) всё ещё перечисляет `esds`. Либо обновить формулировку acceptance, либо завести отдельную задачу на audio tap + AAC.

---

## §8. Гейты

Все числа — на копии с review-only shim (кроме recorder, которому shim не нужен); probe-тесты удалены перед финальными прогонами.

| Команда | Exit | Результат |
|---|---:|---|
| `cargo +1.94.0 test -p vks-recorder --offline` | **0** | **54 lib + 2 ffprobe acceptance = 56 passed**, 0 failed, 0 ignored |
| `cargo +1.94.0 test -p vks-server --offline --lib` | **0**/флейк | **147 passed**, 0 failed, 3 ignored (см. ниже) |
| `cargo +1.94.0 test -p vks-server --offline --tests` | **0** | **35 passed**: 3 `recorder_tap` + 23 `network_integration` + 7 `operator_policy` + 1 `config_failclosed` + 1 `h265_rtp` |
| `cargo +1.94.0 test --workspace --locked --offline --no-fail-fast` | **0** | **51 suite, 1493 passed, 0 failed, 8 ignored** |
| `cargo +1.94.0 clippy -p vks-recorder -p vks-server --all-targets --offline -- -D warnings` | **0** | green |
| `cargo +1.94.0 fmt --all -- --check` | **0** | green; commit-файлы не переформатированы (sha256 совпадают) |

Повторы:

- `recorder_tap` 3× — **3 passed** каждый раз, 41 кадр в каждом файле (`nb_read_frames=41`, `is_avc=true`, `time_base=1/90000`).
- `ffprobe_acceptance` 3× — **2 passed** каждый раз; H.264 60 кадров/2.000000/90000, AAC 49 кадров/1.045333.
- `gateway_audio_bridges_g726_32_at_peer_dynamic_pt_with_8khz_clock` — **флейк окружения**: 2 из 3 полных параллельных прогонов `vks-server --lib` упали по 3-секундному таймауту (`gateways.rs:5645`, `G.726 PCM must reach the 48 kHz MCU mix`), изолированно 3/3 passed, полный lib `--test-threads=1` — **147 passed**. T09b не трогает `gateways.rs`; в P не включён, но отмечен как ограничение параллельного прогона под нагрузкой.

Pristine-гейты T09b-только (`vks-recorder`) зелёные без shim; `vks-server` на `8e4197f` без shim не компилируется (§1).

---

## §9. Acceptance checklist

- [x] MP4 играется ffprobe/ffmpeg: **да**, полный декод 0 ошибок на 10 независимых файлах.
- [x] `avcC`/`timescale`/`stco` корректны: **да**, независимый box-parser + ffprobe.
- [x] `esds`: **проверен синтетически** (`aac.mp4`, 49 кадров, 0 ошибок); production-запись video-only — см. P3-7.
- [x] Tap не спавнит скрытую группу: **да**, `group_count=0` без зрителей и после исчезновения группы.
- [x] Tap не становится зрителем: **да**, refcount/viewers не меняются.
- [x] RoomHub сохраняется при ошибке stop: **да**, recorder-участник снимается, запись не остаётся ACTIVE.
- [x] Ошибки finalize пробрасываются: **на первом stop — да**; **на повторном — нет** (P2-1).
- [x] Zero frames/one frame/gap/duplicate/finalize twice/stop while active/group disappears/large frames: прогнаны; zero/one/gap/large — корректно; duplicate — warning (P3-1); repeat stop — P2-1.

---

## §10. Итог

T09b — качественная реализация: writer действительно строит валидный `avc1`-MP4 (а не «контейнер-заглушку»), tap спроектирован правильно (non-owning subscribe, без скрытого FFmpeg, с явным 409-путём), lifecycle и RoomHub не сломаны. Независимые доказательства — не только committed-тесты: свой парсер, полный ffmpeg-декод, VFR/duplicate/1080p/High/AAC и серверные пробы.

Блокирует сертификацию только P1 порядка коммитов (pristine `8e4197f` не собирает server) и три P2, из которых два (повторный stop, ошибка tap) стоит закрыть до production, а третий (B-кадры) — зафиксировать тестом/докой. **Score 9.0/10, done: no на `8e4197f`; на текущем HEAD `a02daad` после подтверждения сборки/тестов T09b может быть принят как done.**
