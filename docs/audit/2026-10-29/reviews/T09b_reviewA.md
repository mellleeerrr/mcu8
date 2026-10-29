# T09b — независимое ревью A: MP4-контейнер (`avcC`/length-prefix/timescale/`esds`) + tap существующей группы энкодера

- **Дата:** 2026-10-29, 13:47–14:10 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент); проверка только чтением и исполнением, в живом репозитории создан только этот файл.
- **Объект:** коммит `8e4197f` («T09b: record existing encoder farm stream into a real MP4»), `8e4197f^ = fff51a7` (T06/T07 fix), ветка `master`. Diff T09b — 11 файлов, +2515/−463.
- **Pitfalls:** [`docs/audit/2026-10-29/pitfalls/T09b.md`](../pitfalls/T09b.md). Приёмка — [`docs/progress/MASTER_TASK_LIST_2026-10-29.md:48`](../../../progress/MASTER_TASK_LIST_2026-10-29.md) строка 48: «MP4 играется ffprobe/ffmpeg; avcC/esds/timescale/stco корректны; tap не спавнит скрытую группу».
- **Полигон:** `git archive 8e4197f | tar -x -C /dev/shm/t09b_revA_1793270870_647265/src` (без `.git`), Rust **1.94.0** из `rust-toolchain.toml` (`rustc 1.94.0 (4a4ef493e 2026-03-02)`), `CARGO_TARGET_DIR=/home/pro/.cache/t09b_revA_1793270870_647265_target`, `--offline`, `--locked` где применимо, FFmpeg/ffprobe **8.0.1-3ubuntu2**.
- **Чистота:** живая `vks_data.db`, `backups/`, порты `:8080/:18082`, рабочий `target/` и running server не открывались и не менялись; все SQLite/MP4-артефакты — только `/dev/shm/...`; `/tmp` не использовался. После снятия доказательств scratch/target удалены.

## Вердикт

**Контейнерная часть T09b и tap «без скрытой группы» приёмку проходят и подтверждены исполнением.** Но коммит `8e4197f` в целом **нельзя сертифицировать как done**: найден один P1 внутри T09b (жизненный цикл tap при исчезновении группы) и один P1-гейт вне T09b (pristine-архив не собирается без uncommitted T04b). Плюс два P2 по метаданным/идемпотентности `stop`.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **2** (P1-1 — T09b lifecycle; P1-2 — pristine build gate, не T09b) |
| **P2** | **2** |
| **P3** | **12** |

**Score: 8.5/10. Done: нет** (по критерию «0 P0/P1 + acceptance»). Если исправить P1-1 и закоммитить T04b (P1-2), по контейнеру/tap T09b тянет на 9.5: доказательства исполнения ниже реальные, не «ffprobe exit 0».

Кратко:

- H.264-запись, снятая **реальным recorder-tap с реальной x264-группы FFmpeg**, играется: `codec_tag_string=avc1`, `is_avc=true`, `nal_length_size=4`, `time_base=1/90000`, `avcC` 39 байт с SPS/PPS, `stco`/`stsz`/`stts` сходятся, `ffprobe -count_frames` читает 96/96 кадров; полный `ffmpeg -f null -` — exit 0 на прогоне того же теста (94-кадровый файл), репозиторный 60-кадровый тест полный decode также проходит.
- Старый гейт «ffprobe показывает кадры» — **ложноположительный**, доказано на реальном старом writer'е: 60/60 кадров и `ffmpeg -f null -` exit 0 при `is_avc=false`, `nal_length_size=0`, отсутствии `avcC` и 363 Annex-B start code в `mdat`.
- Аудио: в production записи **video-only by design** (`audio: None`), пустой/фиктивный `mp4a` не пишется. Синтетический AAC-путь (`mp4a`+`esds`) реализован и проверен реальным ADTS AAC: 49 кадров, `esds` с ES/DecoderConfig/SLConfig, `time_base=1/48000`, `mdhd duration = sum(stts) = 49*1024`, полное декодирование.
- Tap: `subscribe` только к существующей живой H.264-группе текущего canvas с `viewers>0`; без группы — ошибка/409, скрытого FFmpeg/группы/строки/участника нет; `refcount`/`viewers` не растут; кадры доходят (94–96 кадров в файле).
- **P1-1:** после `RecvError::Closed` (группа ретировалась вместе с последним зрителем) tap просто выходит, но сессия записи остаётся `Running`, строка — ACTIVE (`duration_sec=0`), recorder-участник — в RoomHub. Если затем появляется новая группа, `RecorderControl::start` по idempotent-ветке возвращает тот же id и **не переподписывается**: POST выглядит успешным, но новые кадры не пишутся. Воспроизведено: 20 кадров опубликовано до потери группы, 20 после — в MP4 только 16 кадров (исключительно первый отрезок).
- **P1-2:** pristine `git archive 8e4197f` не компилируется: `vks-admin/src/api_v3/shell.rs:577,605` вызывает `audio_unavailable_reason`/`video_unavailable_reason`, которых в коммите нет (они только в незакоммиченном T04b). Это унаследовано от `fff51a7`, не T09b, но гейт «clippy pristine» красный и заявление коммита о pristine-прогоне не воспроизводится.

---

## §1. Гейты на pristine-копии

До каких-либо правок scratch-копии:

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all -- --check` | **0** | чисто |
| `cargo clippy -p vks-recorder -p vks-server --all-targets --offline -- -D warnings` | **101** | **КРАСНЫЙ**, 2×E0425 (см. §1.1) |
| `cargo test -p vks-recorder --offline` | **0** | **54 passed** lib + **2 passed** `ffprobe_acceptance` = 56; baseline до T09b — 33 (container 2 → 20, recording 12 → 15, retention/storage/profile без изменений) |
| `cargo test -p vks-server --lib --offline` | **0** на shim-копии | **147 passed, 3 ignored**; на pristine не запускается из-за того же E0425 |
| `cargo test -p vks-server --test recorder_tap --offline` | **0** | **3 passed** |
| `cargo test --workspace --locked --offline --no-fail-fast` | **0** | **1495 passed, 0 failed, 8 ignored**; из них 2 теста — мои review-only evidence-тесты, т.е. для коммита **1493/0/8**. Заявлено в сообщении коммита 1492/8 — расхождение +1 (не дефект, вероятно счёт doc/иного target) |

Первый прогон `cargo test -p vks-recorder` — 54 lib + 2 integration, 0 failed. `ffprobe_acceptance` на реальном x264 печатает:

```text
ffprobe h264: codec=Some("h264") tag=Some("avc1") is_avc=Some("true")
  nal_length_size=Some("4") time_base=Some("1/90000")
  profile=Some("Constrained Baseline") frames=Some("60") duration=Some("2.000000")
```

и AAC:

```text
ffprobe aac: codec=Some("aac") profile=Some("LC") time_base=Some("1/48000")
  frames=Some("49") duration=Some("1.045333")
```

### 1.1. P1-2 (вне T09b): pristine-архив не собирается

```text
error[E0425]: cannot find function `audio_unavailable_reason` in module `vks_common::codec_capabilities`
   --> crates/vks-admin/src/api_v3/shell.rs:577:58
error[E0425]: cannot find function `video_unavailable_reason` in module `vks_common::codec_capabilities`
   --> crates/vks-admin/src/api_v3/shell.rs:605:59
error: could not compile `vks-admin` (lib) due to 2 previous errors
```

`git grep 8e4197f -- crates/` подтверждает: в коммите есть только вызовы, определений нет. В живой рабочей копии определения лежат в **незакоммиченном** `crates/vks-common/src/codec_capabilities.rs` (T04b), которого в архиве нет. То же самое на `fff51a7` (`git log -S audio_unavailable_reason -- crates/vks-admin/src/api_v3/shell.rs` → только `fff51a7`), т.е. **HEAD^ уже был сломан**, T09b это не вносил и не чинил.

Чтобы всё же прогнать T09b-гейты, в scratch-копию добавлены **только** две совместимые заглушки в `vks-common` (возвращают `None`/`Some("unavailable")`, никак не участвуют в recorder/tap). После них:

- `cargo clippy -p vks-recorder -p vks-server --all-targets --offline -- -D warnings` → **exit 0**;
- `cargo test -p vks-server --lib` → 147/0/3;
- workspace → 1495/0/8.

Заглушки — только полигон, в отчёт и в коммит не входят. Факт остаётся: **из самого `8e4197f` без T04b дерево не собирается**, и «pristine»-заявление коммита невоспроизводимо. Рекомендация: закоммитить T04b (или хотя бы эти две функции) до сертификации HEAD.

---

## §2. Контейнер: исполнение на реальной записи recorder-tap

### 2.1. Как получен файл

Evidence-тест (review-only, `crates/vks-server/tests/t09b_review_evidence.rs`, в коммит не входит) поднимает `McuEngine` + `EncoderFarm` с реальным FFmpeg/x264 (`encoder_params=repeat-headers=1:aud=1:scenecut=0`, 640×360@30), создаёт группу через `acquire_video_viewer` (это зритель, не recorder), затем `RecorderControl::start` подписывается на неё (`subscribe`), публикует 100 серых кадров с шагом 34 мс, делает `stop` и оставляет `tap_recorder.mp4`.

Итоговый файл (последний прогон): `7464` байта, sha256 `9128f8c0cabe93ff1d215739e7fc9c00de417ce16ef5510f87ece2ad9d82e831`.

`ffprobe`:

```text
codec_name=h264
profile=Constrained Baseline
codec_tag_string=avc1
width=640 height=360
is_avc=true
nal_length_size=4
time_base=1/90000
duration=3.353789
nb_frames=96
nb_read_frames=96
extradata_size=39
```

`ffprobe -count_frames` прочитал **96/96** кадров (полное чтение). Отдельный `ffmpeg -v error -i tap_recorder.mp4 -map 0:v:0 -f null -` на предыдущем прогоне того же теста (94-кадровый файл, та же структура) дал **exit 0**; репозиторный `ffprobe_h264_x264_acceptance` делает полный `ffmpeg` decode 60-кадрового x264 и проходит. В файле **один** trak (`h264/avc1`), `mp4a`/audio trak нет.

### 2.2. Боксы (независимый Python-парсер, не код репозитория)

```text
top: ftyp(0..32) moov(32..2194) mdat(2194..7464)
mdat payload 2202, len 5262
video trak handler=vide
avc1 payload start 429, size 125
avcC: configVersion=1 profile=66 compat=192 level=31
      lengthSizeMinusOne=3 numSPS=1
      SPS len=24 6742c01fda0280bfe584000003000400000300f23c60ca80
      PPS len=4  68ce3c80
mdhd timescale=90000 duration=301841
stts entries=95 sum=301841
stsz count=96 sum=5262 == mdat payload 5262
stco count=96 first=2202 last=7447
stss count=4 [1,31,61,91]
mdat: 00 00 01 count=0, 00 00 00 01 count=0
```

Проверки:

- **`avcC` несёт именно SPS/PPS первого IDR-сэмпла** без start code: `avcC SPS == sample SPS: True`, `PPS ==: True`. Первый сэмпл — NAL types `[(9,2),(7,24),(8,4),(6,668),(5,709)]`, т.е. AUD+SPS+PPS+SEI+IDR, все length-prefixed.
- **`stco`/`stsz`:** каждый offset указывает на начало очередного сэмпла, `offset[i] == offset[i-1]+size[i-1]`, все NAL внутри сэмпла — `u32 len + payload`, сумма размеров = payload `mdat`; start code внутри сэмплов отсутствует.
- **`stts`/`mdhd`:** `sum(stts)=301841 == mdhd.duration`; `mdhd.timescale=90000`.
- **`stss`:** 4 ключевых кадра `[1,31,61,91]` при `keyint=30`, без фиктивного «кадра 1» и без пропусков.

Замечание по таймлайну: `AccessUnit.pts_us` в ферме — это `started.elapsed()` на моменте **эмиссии** AU (`crates/vks-media-mcu/src/farm.rs:983`), а не захваченный PTS энкодера. Поэтому у реального tap первый delta может быть 2–4 мс, а `avg_frame_rate` — не ровно 30 (в этом файле 96 кадров за 3.3538 с). `stts` при этом честно описывает то, что реально записано; синтетический 60-кадровый тест даёт ровно 30/1 и `duration=2.000000`. Это ограничение источника, не порча контейнера; отмечено ниже как P3.

### 2.3. Старый false-positive гейт — доказан на реальном старом writer'е

Из `fff51a7` извлечён старый `crates/vks-recorder/src/container.rs` (Annex-B в `mdat`, `avcC` нет, time_base 1/1000, фейковый `mp4a`) и собран отдельным scratch-крейтом; тот же x264 Annex-B (60 AU, 640×360@30) прогнан через него. Результат `old_writer.mp4` (484079 байт, sha256 `9e129dc04f77942c4094868ae687c16d6634cbd86f33b697f5f056b492cb59b3`):

```text
codec_name=h264
codec_tag_string=avc1
is_avc=false
nal_length_size=0
time_base=1/1000
duration=1.947000
nb_frames=60
nb_read_frames=60
streams: 0=h264/avc1, 1=aac/mp4a
avcC occurrences=0, mp4a occurrences=1
mdat payload=482607, startcode 00 00 01 = 363, 00 00 00 01 = 363
ffmpeg -v error -i old_writer.mp4 -map 0:v:0 -f null -  → exit 0
```

То есть старый гейт «ffprobe/ffmpeg показывает 60 кадров, exit 0» **проходил бы** на заведомо невалидном Annex-B-контейнере без `avcC`. Новый тест это ловит жёсткими assert'ами `is_avc=true`, `nal_length_size=4`, `time_base=1/90000`, `avcC` с SPS/PPS, отсутствием start code и точным числом кадров (`crates/vks-recorder/tests/ffprobe_acceptance.rs:248-396`). Это ровно тот P0 из pitfalls, и он закрыт.

### 2.4. `stco`/`stsz` при аудио

Синтетический AAC-файл (`aac_esds.mp4`, 14035 байт, sha256 `93e7127345519156150cf634dfbe9812e4c220b588a586d424ebb964097aef66`) содержит video+audio. Независимая проверка:

```text
vide timescale 90000 mdhd_dur 3000 stts_sum 3000 samples 1 sum_sizes 28 first_off 1487
soun timescale 48000 mdhd_dur 50176 stts_sum 50176 samples 49 sum_sizes 12520 first_off 1515
mdat payload 12548 == 28+12520
audio stco/sizes contiguous after video; total stsz == mdat payload OK
```

`stco` аудио начинается сразу после видео-сэмпла, таблицы сходятся. Это отвечает на «stco/stsz consistency» и для multi-track.

---

## §3. `esds`/аудио: что именно реализовано

**Реализовано:** отдельный путь `mp4a`+`esds` в `crates/vks-recorder/src/container.rs:235-260, 602-700, 805-852`; `AudioConfig` с ASC, timescale = sample_rate, `stts` = 1024 сэмпла/кадр, `tkhd`/`mdhd`/`smhd`/`stsd`; `esds` строится как ES_Descriptor → DecoderConfigDescriptor (objectType 0x40, streamType 0x15) → DecoderSpecificInfo (ASC) + SLConfigDescriptor (0x02). Пустой/фиктивный `mp4a` без `AudioConfig`/сэмплов **не пишется**: `write_audio_sample` → `UnsupportedAudio` (`container.rs:235-247`), `finalize` создаёт audio trak только при `config.audio.is_some() && !audio_samples.is_empty()`.

**В production записи MCU аудио пока нет by design:** `container_config_for` (`crates/vks-server/src/recorder.rs:467-477`) ставит `audio: None`; живого AAC-источника в tap нет. Поэтому реальная запись — video-only, и это правильно: в `tap_recorder.mp4` ровно один trak, никакого пустого `mp4a`.

**Синтетическая проверка esds — реальная, не «exit 0»:** evidence-тест сгенерировал ADTS AAC (FFmpeg, 48 кГц, stereo, 1.024 с), распарсил ADTS-заголовки, записал 49 raw AAC access units через writer, оставил файл. `esds`:

```text
esds payload len 31:
00 00 00 00 03 19 0001 00 04 11 40 15 000000 00000000 00000000 05 02 11 90 06 01 02
descriptor 0x03 (ES) len 25, ES_ID=1
  child 0x04 (DecoderConfig) len 17: objectType=0x40 streamType=0x15
    ASC = 05 02 11 90
  child 0x06 (SLConfig) len 1: 0x02
```

`ffprobe`: `codec_name=aac`, `profile=LC`, `codec_tag_string=mp4a`, `sample_rate=48000`, `channels=2`, `time_base=1/48000`, `nb_read_frames=49`, `duration=1.045333` (= 50176/48000). `ffmpeg -map 0:a:0 -f null -` → exit 0. Репозиторный `ffprobe_aac_esds_acceptance` (`ffprobe_acceptance.rs:417-510`) проверяет те же `esds`-теги, ASC, `mdhd`, `stts` и декодирование.

Ограничение теста: видео-сэмпл в AAC-тесте использует синтетический 9-байтовый SPS (`ffprobe_acceptance.rs:28-29`), поэтому при probing видео ffmpeg пишет `sps_id 32 out of range`; аудио-путь от этого не зависит, тест мапит только `0:a:0` и проверяет аудио. Это P3 качества теста, не дефект `esds`.

---

## §4. Tap: существующая группа, без скрытого FFmpeg

### 4.1. Код и unit-тесты

`EncoderFarm::subscribe(room, codec)` (`crates/vks-media-mcu/src/farm.rs:429-455`) под `groups`-мьютексом выбирает **живую** группу (`group_is_live`, `farm.rs:373-377`) с `key.room`, `key.spec.codec == codec`, `key.canvas_version == versions[room]` и `refcount > 1` (т.е. `viewers > 0`), возвращает `FarmTap { key, rx: group.tx.subscribe(), parameter_sets }`. Он **не** вызывает `acquire`/`viewer`, не трогает `refcount`, не вызывает `spawner` и не вставляет группу. `active_group_count`/`has_active_h264_group` (`farm.rs:456-474`) используют тот же предикат.

Три теста в `farm.rs`:

- `subscribe_without_group_returns_none_and_does_not_spawn` (`:1573-1583`): нет группы → `None`, `group_count=0`, `spawns=0`, `active_group_count=0`;
- `subscribe_reuses_existing_group_without_refcount_or_spawn` (`:1585-1604`): после `acquire` `spawns=1`; `subscribe` → `spawns` всё ещё 1, `group_count=1`, `refcount`/`viewers` не изменились;
- `subscribe_filters_codec_and_stale_canvas_versions` (`:1609-1628`): H.265-группа не подписывается как H.264; после `invalidate_room` старая canvas-группа не подписывается.

### 4.2. Server-тесты и HTTP-семантика

`crates/vks-server/tests/recorder_tap.rs`:

- `recording_start_without_group_is_rejected_and_spawns_no_hidden_group` (`:61-77`): `start` → `Err` с `no_active_h264_encoder_group`, `is_recording=false`, `farm.group_count=0`, `video_farm_stats().is_empty()`, `active_h264_groups=0`, нет recorder-участника и нет строки записи;
- `recording_tap_reuses_existing_group_without_becoming_a_viewer` (`:79-...`): после `acquire_video_viewer` `start` тапает группу; после `start` ровно одна группа, `refcount`/`viewers` те же, 41 кадр в MP4, `is_avc=true`, `nal_length_size=4`, `time_base=1/90000`;
- `recording_stop_surfaces_container_error_but_still_detaches_room_hub` (`:178-205`): без кадров `stop` → ошибка `контейнер пуст`/`Empty`, `get_container=None`, recorder-участник снят, строка финализирована (`duration_sec=1`, `size_bytes=0`).

HTTP: `videorecords_start` (`crates/vks-admin/src/api_v3/observability.rs:780-791`) до `control.start` отдаёт **409** `no_active_h264_encoder_group`, если `active_h264_groups==0`; при гонке `control.start` вернёт `Err` и handler отдаст 500 (`:797-800`). То есть нормальный «нет группы» — 409, гонка — 500 (P3).

Evidence-тест с реальным recorder: до/во время `start` `refcount=2`, `viewers=1`, `group_count=1`, `spawn` нет; 96 кадров дошли до MP4. `FarmTap` — non-owning, без `Drop`/refcount.

### 4.3. RoomHub-loss P1 из отчёта — закрыт

`stop_recording_internal` снимает RoomHub/WS-участника **до** возможного early-return по ошибке (`recorder.rs:342-343`), строка финализируется (`:355-379`), `size_bytes`/`duration_sec` обновляются; ошибка finalize возвращается наружу (`:448-454`). Тест `recording_stop_surfaces...` и мой PROBE2 подтверждают: после `Err(Empty)` `participants=0`, строка `duration_sec=1`, контейнера нет. Это P1 закрыт.

### 4.4. P1-1 (T09b): `Closed`/потеря группы не завершает запись, а `start` не переподписывается

Evidence PROBE1 (review-only, scratch):

```text
PROBE1 after_viewer_drop: is_recording=true farm_groups=0 active=0
  participants=1 row=duration_sec=0
PROBE1 after_viewer2: farm_groups=1 active=1
PROBE1 restart_start=Ok("97874950-...") same_id=true
PROBE1 stop=Ok(()) container_len=Some(2622)
ffprobe group_loss_restart.mp4: is_avc=true, duration=0.868933, nb_read_frames=16
```

Сценарий: 20 кадров записаны, последний зритель уходит → группа ретируется, `rx.recv()` даёт `RecvError::Closed`; `recorder_tap::run` (`crates/vks-server/src/recorder_tap.rs:68-71`) только логирует и делает `break`. Сессия остаётся `Running`, `AppState.records[room].duration_sec=0` (ACTIVE), recorder-участник остаётся в RoomHub. Приходит новый зритель/новая группа; `RecorderControl::start` (`crates/vks-server/src/recorder.rs:50-57`) по idempotent-ветке вызывает `start_recording` → `start_recording_inner` (`:201-205`) возвращает **тот же** id, не проверяя, жив ли tap и не переподписываясь. Публикуются ещё 20 кадров — в MP4 их нет (16 кадров = только первый отрезок, last pts 75204 тика). POST при этом успешен.

Это тихая потеря записи после смены группы/ухода зрителей и залипшая ACTIVE-строка + участник. Рекомендация: либо на `Closed`/ошибке tap завершать сессию через manager (finalize + detach), либо в `RecorderControl::start` при `is_recording`, но мёртвом tap, переподписываться/заменять tap; как минимум не возвращать успех без живого tap. Нужен тест «viewer ушёл → группа закрылась → новый viewer → start → кадры снова пишутся» (сейчас его нет).

---

## §5. Найденные проблемы (P0–P3, file:line)

### P0 — нет

Контейнер валиден, ffprobe/ffmpeg играет, avcC/esds/timescale/stco сходятся; скрытой группы/FFmpeg tap не спавнит.

### P1 — 2

**P1-1 (T09b).** `Closed`/смерть tap не завершает запись и не даёт переподписаться; `start` возвращает success с мёртвым tap. `crates/vks-server/src/recorder_tap.rs:68-71`, `crates/vks-server/src/recorder.rs:50-57`, `:201-205`. Воспроизведение: PROBE1, в `group_loss_restart.mp4` 16 кадров — только первый отрезок, кадры после новой группы отсутствуют; ACTIVE-строка/участник после ухода зрителя.

**P1-2 (вне T09b, gate).** Pristine `git archive 8e4197f` не собирается: `crates/vks-admin/src/api_v3/shell.rs:577,605` → отсутствующие `audio_unavailable_reason`/`video_unavailable_reason` (uncommitted T04b). `cargo clippy ... -D warnings` exit 101. Унаследовано от `fff51a7`; T09b это не вносил, но коммит не самодостаточен, и заявленные pristine-гейты невоспроизводимы.

### P2 — 2

**P2-1.** Повторный `DELETE`/`stop` после ошибки finalize возвращает `Ok(())`, а не ту же ошибку: `RecorderControl::stop` при `!is_recording` уходит в ветку «финализировать незавершённые строки» и возвращает `Ok` (`crates/vks-server/src/recorder.rs:92-112`); `stop_recording_internal` для кэша возвращает `finalize_error: None` (`:317-319`). PROBE2: первый `stop=Err("...контейнер пуст...")`, второй `stop=Ok(())`. Клиент видит 500→200, ошибка теряется.

**P2-2.** При ошибке finalize, когда сэмплы уже записаны, `RecordingMeta.size_bytes` = сумма **сырых Annex-B** байт, а не 0 и не размер MP4, и контейнера нет: `crates/vks-recorder/src/recording.rs:348-362`, `crates/vks-server/src/recorder.rs:376-377`. PROBE (MissingParameterSets): `segments=1 size_bytes=6 container=None` — строка в records выглядит завершённой с ненулевым размером, но артефакта нет. Существующий тест `container_finalize_error_is_surfaced_not_swallowed` проверяет только `Empty`, где raw=0, поэтому кейс не пойман.

### P3 — 12

1. H.265 в запись не поддерживается (fail-closed `UnsupportedCodec`, нет `hvcC`); `subscribe` при этом может вернуть H.265 tap, сервер просит только H.264. `crates/vks-recorder/src/container.rs:206-209`, `crates/vks-server/src/recorder.rs:73`.
2. Нет HTTP-маршрута для получения MP4: `RecorderManager::get_container` (`crates/vks-server/src/recorder.rs:167-169`) используется только тестами; файл — in-process RAM-артефакт, не durable. Это ограничение T09a/T09b, в отчёте фиксируется явно.
3. `ContainerConfig.max_size_bytes` захардкожен 50 МБ в `container_config_for` (`crates/vks-server/src/recorder.rs:467-477`), не берётся из настроек/профиля.
4. `set_parameter_sets` берёт первый SPS/PPS и молча игнорирует конфликтующие наборы (`crates/vks-recorder/src/container.rs:262-285`); смена ловится только в `write_video_sample`, API выглядит обманчиво.
5. `build_avcc` для High profile всегда пишет extension `FD F8 F8 00` (4:2:0, 8-bit) (`container.rs:798-802`), без реального chroma/bit-depth из SPS; для baseline-групп не проявляется.
6. B-кадры/`ctts`: writer пишет только `stts`, предполагая PTS==DTS; энкодер-группа baseline, но допущение нигде не проверяется (`container.rs`, `recorder_tap.rs:56-58`). При появлении B-кадров декодирование/таймлайн поедут.
7. `Segment.data_size` в `write_video_sample` остаётся длиной сырого Annex-B, а не length-prefixed/MP4 (`crates/vks-recorder/src/recording.rs:280-290`); используется только как fallback на ошибке — отсюда P2-2.
8. Tap держит `std::sync::Mutex` на `sessions` во время `write_video_sample` (копирование/парсинг) в async-таске (`crates/vks-server/src/recorder_tap.rs:51-58`) — краткая блокировка runtime-потока.
9. Гонка 409→500: admin проверяет `active_h264_groups` до `control.start`, при исчезновении группы между проверкой и старт handler отдаёт 500 без `reason` (`crates/vks-admin/src/api_v3/observability.rs:780-800`).
10. `ffprobe_aac_esds_acceptance` использует невалидный 9-байтовый SPS/PPS для видео, из-за чего ffmpeg печатает `sps_id ... out of range`; тест это игнорирует, проверяя только аудио (`crates/vks-recorder/tests/ffprobe_acceptance.rs:28-29, 417-510`). Для esds-гейта допустимо, но шумно.
11. Оба ffprobe-теста молча `return`, если нет `ffmpeg`/`ffprobe` (`crates/vks-recorder/tests/ffprobe_acceptance.rs:252-256, 420-424`): зелёный CI без FFmpeg не является доказательством контейнера.
12. `check_capacity` оценивает overhead как 16 байт/сэмпл (`container.rs:442-443`), а VFR-`stts`+`stss`+`stsz`+`stco` могут дать до ~20; точная финальная проверка ловит `StorageFull`, но квота может быть превышена на единицы КБ до ошибки.

---

## §6. Регрессии и границы

- **T06/T07 не тронуты T09b:** `git diff --name-only 8e4197f^ 8e4197f` — только 11 T09b-файлов (`vks-media-mcu/farm.rs`, `lib.rs`, `vks-recorder/{container,lib,recording}.rs`, `tests/ffprobe_acceptance.rs`, `vks-server/{lib,recorder,recorder_tap}.rs`, `tests/{network_integration,recorder_tap}.rs`). Ни `layout`, ни `frontend`, ни T06/T07-файлов.
- **T04b:** в архиве отсутствует (как и предупреждено); его незакоммиченные `codec_capabilities.rs`/`codec_spec.rs` — причина P1-2. В рабочем дереве T04b-изменения не использовались как источник, кроме диагностики отсутствующих символов.
- **T09a:** `network_integration` M5-тест обновлён осознанно (video-only, avcC, без fake `mp4a`) и restart-тест переведён на существующую группу; workspace 1493/0/8 (без review-тестов) — регрессий T09a нет.
- **T09b vs H.265:** writer fail-closed, `avc1` для H.265 не пишется; это соответствует pitfalls P2, отдельный `hvcC` — вне задачи.

---

## §7. Гигиена

- Живые `vks_data.db`, `backups/`, `target/`, порты `:8080/:18082`, запущенные серверы/FFmpeg-дети не открывались; `AppState::new()` в тестах — in-memory (`storage=None`), без SQLite.
- Все MP4/логи/цели — в `/dev/shm/t09b_revA_1793270870_647265` и `/home/pro/.cache/t09b_revA_1793270870_647265_target`; `/tmp` не использовался.
- В живом репозитории создан только этот файл; scratch и target после снятия доказательств удалены.
- Evidence-тесты (`t09b_review_evidence.rs`, `t09b_review_audio_evidence.rs`, `t09b_review_lifecycle.rs`, `t09b_review_error_size.rs`) — только в scratch-копии, в коммит/рабочее дерево не добавлялись.

---

## §8. Итог

- **T09b container/esds/timescale/stco:** принято, 0 P0; реальный recorder-tap MP4 играется (96/96 читаются, decode exit 0 на том же тесте) и проходит жёсткие проверки; старый false-positive гейт доказан и закрыт.
- **T09b tap/no-hidden-group:** принято по существу (`subscribe` без spawn/refcount, 409 без группы, кадры идут), **но P1-1** — потеря группы не завершает запись и не даёт переподписаться; это тихая потеря записи.
- **P1-2** — pristine-архив не собирается из-за uncommitted T04b; не T09b, но блокирует «чистый HEAD».
- **Score 8.5/10, done: нет.** После фикса P1-1 (завершение/переподписка tap + тест) и коммита T04b — повторное ревью может дать 9.5 при сохранении текущих доказательств.
