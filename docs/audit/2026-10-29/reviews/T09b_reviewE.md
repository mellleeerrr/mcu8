# T09b — независимое ревью E (финальный раунд): stale-success, tap-error replay, MP4 evidence

- **Дата:** 2026-10-29, 16:48–17:15 MSK.
- **Ревьюер:** независимый correctness-reviewer (агент). Живой репозиторий только читался; в нём создан только этот файл.
- **Объект:** `737f6e1dcc28824a7b4742ec4c92eca571ac7001` «style(vks-common): drop stray blank line flagged by cargo fmt» (HEAD). T09b-файлы в `737f6e1` побайтно совпадают с фиксом `0cb6434`: `recorder.rs`, `recorder_tap.rs`, `container.rs`, `tests/recorder_tap.rs` — проверено `git rev-parse`/`sha256sum`.
- **Полигон:** `git archive 737f6e1 | tar -x -C /dev/shm/rvE_t09b_1793281751` (без `.git`), Rust **1.94.0** (`rustc 1.94.0 (4a4ef493e 2026-03-02)`, `RUSTUP_TOOLCHAIN=1.94.0`), `CARGO_TARGET_DIR=/dev/shm/rvE_target_1793281751`, `CARGO_INCREMENTAL=0`, `--offline --locked`. `/tmp` для стенда, БД, MP4 и логов не использовался.
- **Чистота копии:** sha256 production-файлов T09b совпадают с коммитом: `recorder.rs` `b2fe9f9e…`, `recorder_tap.rs` `8fe2e815…`, `container.rs` `f3aa750e…`, `tests/recorder_tap.rs` `5addbba6…`. Review-only probe-тесты лежали только в `/dev/shm`-копии и после снятия доказательств удалены; `fmt`/`clippy` после удаления повторно зелёные.
- **Чистота стенда:** живая `vks_data.db` не менялась (`sha256 d2ccc444…`, mtime `16:31:10`, size `3334144` до/после), `backups/` не трогались, порты `:8080` (pid 1914707) и `:18082` (pid 3253281) остались те же. Все SQLite/MP4/лог-артефакты — только в `/dev/shm`.

## Вердикт

**done: нет. Score: 9.0/10.**

Точечный сценарий из задания («одна группа → start → закрыть группу → немедленный второй start») действительно исправлен: **8/8** прогонов второй `start` вернул явную ошибку `запись завершается: живой H.264-группы больше нет; повторите старт`, пустой MP4 не записан, окно автофинализации поймано 8/8. Повторный DELETE даёт ту же сырую строку без двойного префикса; detached finalize, идемпотентность успешного stop, MP4/avcC/AAC/esds, отсутствие скрытого FFmpeg — подтверждены исполнением.

Но **корневая причина P2-1 не устранена**: guard проверяет `active_h264_groups(room)`, а не живость tap **своей** сессии. Если в комнате остаётся вторая живая H.264-группа (реалистично при re-key по числу видимых окон и при перекрытии старой/новой группы), немедленный `start` после закрытия записываемой группы всё ещё возвращает `Ok(stale_id)`: 1/1 детерминированный прогон. Это ровно тот же ложный 200 «старт, который ничего не пишет», что и P2-1 review D, поэтому приёмка «must NOT return Ok(stale id)» в общей формулировке не выполнена.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **1** (остаточный stale-success при второй живой группе) |
| **P3** | **7** |

## §1. fmt, clippy, тесты (точные счётчики)

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all -- --check` | **0** | чисто (до probe-тестов и повторно после их удаления) |
| `cargo clippy -p vks-recorder -p vks-server --all-targets --offline --locked -- -D warnings` | **0** | `Finished dev profile … in 0.53s`, warning/error — **0** |
| `cargo test -p vks-recorder --offline --locked` | **0** | lib **54 passed**, `ffprobe_acceptance` **2 passed**, doc **0**; failed 0 |
| `cargo test -p vks-server --lib --offline --locked` | **0** | **154 passed, 0 failed, 3 ignored** |
| `cargo test -p vks-server --test recorder_tap --offline --locked` × **5** | **0** ×5 | **5/5** в каждом, итого **25 passed / 0 failed**; 2.49–2.61 с на прогон |
| `cargo test --workspace --offline --locked --no-fail-fast` | **0** | **1610 passed, 0 failed, 8 ignored**, 54 result-блока (38 test-бинарников + 16 doc-tests) |
| `cargo test -p vks-server --test network_integration recording_control_persists_immediately_and_reuses_the_room_slot_after_restart` | **0** | 1 passed, 0 failed (22 filtered) |

Рост относительно review D (1558/0/8) — чужие коммиты T10/T15/T16, не T09b. Заявленные ранее 149/3 для server lib уже неактуальны: фактически **154/3** (на 737f6e1).

## §2. Stale-success: исполнение probe (не только чтение кода)

Review-only probe (`/dev/shm`-копия, после снятия доказательств удалён) использовал реальную x264-группу, `RecorderControl`, `RecorderManager`, `McuEngine`; `#[tokio::test]` current-thread, поэтому после `drop(viewer)` detached-таск ещё не успевал снять сессию — окно фиксировалось флагом `recording_at_call=true`.

**Сценарий задания — одна группа, без кадров (8 прогонов):**

```text
PROBE_E stale#0..7 recording_at_call=true groups=0
  immediate=Err(запись завершается: живой H.264-группы больше нет; повторите старт)
PROBE_E stale summary: stale_ok=0/8 window_hits=8/8
finalize_ms=[2.211, 2.894, 1.951, 1.993, 2.784, 1.810, 1.980, 2.901]
```

После финализации в каждом прогоне: `group_count()==0`, recorder-участник снят, `manager.get_container(old_id)==None` (пустой MP4 не записан), строка `duration_sec=1 / size_bytes=0`, повторный `stop` вернул `"контейнер пуст: нет ни одного видеосэмпла"`. **Пункт «no empty mp4 written» выполнен.**

**Тот же сценарий с реальными кадрами (3 прогона):** немедленный `start` снова `Err` (0/3 stale), detached finalize за 14.9/17.3/15.1 мс, артефакты 2336/2402/2358 байт, ровно одна строка записи, `size_bytes == len(mp4)`.

**Остаточный дефект — вторая живая группа (1/1 детерминированный counterexample):**

```text
PROBE_E second_group recording_at_call=true groups=1
  immediate=Ok("18c03bb1-aba4-438e-ad6e-8296fe37bec3")
  old_id=18c03bb1-aba4-438e-ad6e-8296fe37bec3
PROBE_E second_group after_finalize groups=1
  row=RecordEntry{ duration_sec:1, size_bytes:0 } is_recording=false
PROBE_E second_group fresh_start=fb2c4577-… (would have been correct for the immediate call)
```

Сценарий: группа A (`bitrate 1024`) — на неё подписан tap; затем появляется группа B (другой `EncoderSpec`, тот же canvas, `bitrate 512`), `drop(viewer A)` закрывает A, B остаётся живой (`active_h264_groups==1`). Немедленный `start` идёт по idempotent-ветке (`is_recording=true`, группа в комнате есть) и возвращает **старый id**, не переподписываясь на B; detached finalize затем финализирует старую сессию (`duration=1, size=0`), группа B остаётся без tap. Только следующий `start` после `!is_recording` создаёт новую сессию для B. Это ложный 200 на запись, которая уже ничего не пишет, — тот же пользовательский дефект, что P2-1 review D.

Реалистичность: `gateway_video.rs:734-747` при смене числа видимых окон делает `acquire_video_viewer` с новым `farm_spec_for(...)` (новый ключ группы) и заменяет viewer; при перекрытии старая и новая группы какое-то время сосуществуют. Recorder в этот момент может быть подписан на закрывающуюся группу.

**Причина и фикс:** `RecorderControl::start` (`crates/vks-server/src/recorder.rs:51-63`) проверяет только `active_h264_groups(room)==0` (`:56`), то есть наличие **любой** живой группы комнаты, а не живость tap конкретной сессии. Нужен признак «tap жив» на сессии/в `taps` (например, состояние tap-таска), и при мёртвом tap — либо явная ошибка, либо остановка старой сессии и новый `start` с подпиской на живую группу; нельзя выводить живость из `active_group_count`.

## §3. MP4 evidence: независимый decode и разбор

Из committed-прогона `recorder_tap` файл перехвачен до удаления тестом: `t09b_tap_c86b25f4-….mp4` (4235 B, 43 кадра). Плюс три файла из stale-probe с реальными кадрами. Независимый Python-парсер ISOBMFF (боксы, `stsd`/`avcC`, `stts`/`stsz`/`stco`/`stss`, length-prefix) + `ffprobe`/`ffmpeg` 8.0.1:

| Файл | Байт | Сэмплов | NAL | `mdhd.ts` | `mdhd.duration` | `stss` | ffprobe кадров | decode |
|---|---:|---:|---:|---:|---:|---|---:|---|
| committed tap | 4235 | 43 | 91 | 90000 | 136187 | `[1, 31]` | 43 | exit 0, stderr 0 строк |
| stale_window_frames_0 | 2336 | 8 | 19 | 90000 | 23289 | `[1]` | 8 | exit 0, stderr 0 |
| stale_window_frames_1 | 2369 | 9 | 21 | 90000 | 25748 | `[1]` | 9 | exit 0, stderr 0 |
| stale_window_frames_2 | 2369 | 9 | 21 | 90000 | 26612 | `[1]` | 9 | exit 0, stderr 0 |

Для каждого live-файла проверено парсером: top-level ровно `ftyp`, `moov`, `mdat` (moov < mdat); один `vide`-трек; `avc1` + `avcC` (`configurationVersion=1`, `lengthSizeMinusOne=3` → 4-байтовый NAL length), 640×360; `mdhd.timescale=90000`; `sum(stts)==mdhd.duration`; `sum(stsz)==mdat payload`; `stco` contiguous и указывает на начало каждого сэмпла; каждый сэмпл полностью разбирается length-prefix'ами (Annex-B start code внутри NAL не встречается); `stss` содержит только сэмплы с NAL type 5; **нет `mp4a`/`esds`** (video-only by design). ffprobe: `codec_name=h264`, `codec_tag_string=avc1`, `is_avc=true`, `nal_length_size=4`, `time_base=1/90000`, `has_b_frames=0`.

**AAC/esds (synthetic acceptance).** Из `ffprobe_aac_esds_acceptance` перехвачен `aac.mp4_<pid>` (14035 B): 49 AAC-кадров, `mp4a`, `esds` с тегами `0x03/0x04/0x05/0x06`, ASC `11 90`, `mdhd.timescale=48000`, `duration=50176=49*1024`, `sum(stts)` совпадает, ADTS-заголовков в сэмплах нет. ffprobe: `aac/LC`, 2 канала, 48 кГц, 49 кадров, `1/48000`; `ffmpeg -map 0:a:0 -f null -` — exit 0 (stderr содержит ошибки намеренно синтетического H.264-видео, аудиотрек декодируется; committed-тест проверяет только audio). Фиктивного `mp4a` в live-файлах нет; committed unit-тест `audio_samples_without_config_are_rejected_and_no_fake_track_is_written` входит в 54 passed.

**>4 GiB fail-closed — частично.** В коде есть арифметический guard `mdat_end.checked_add(...)` + `u32::try_from(mdat_end)` (`crates/vks-recorder/src/container.rs:370-376`), unit-тест `oversized_offsets_fail_closed_without_allocation` (`:1342-1347`) passed; `build_stco(&[usize::MAX])`, `build_stsz(&[usize::MAX])`, `pts_us_to_ticks(u64::MAX)` дают `TooLarge` без аллокации. End-to-end файл >4 GiB не строился (потребовал бы >4 GiB RAM в tmpfs); это ограничение проверки, а не дефект, но committed 4-GiB теста нет.

**Скрытый FFmpeg.** `recorder.rs`/`recorder_tap.rs` не содержат `std::process::Command`/spawn FFmpeg; committed `recording_start_without_group_is_rejected_and_spawns_no_hidden_group` требует `group_count()==0`, пустые farm-stats, отсутствие recorder-участника и строки. Все прогоны — 0 скрытых групп; после тестов не осталось ffmpeg-процессов.

## §4. Group-loss finalize, tap subscription, stop idempotency, restart

- **Finalize-on-group-loss:** committed `recording_finalizes_when_encoder_group_closes` passed во всех 5 прогонах `recorder_tap`; probe подтверждает detached-финализацию за 1.8–17.3 мс, `duration_sec>=1`, снятие участника, `group_count==0`, отсутствие leak'а группы. Успешная запись сохраняет валидный MP4.
- **Tap на существующую группу / viewers>0:** committed тест `recording_tap_reuses_existing_group_without_becoming_a_viewer` passed: `refcount` не изменён, `viewers==1`, `group_count==1`, recorder не становится зрителем; второй группы не появляется. Наш probe использует тот же путь `video_farm().subscribe(...)`, без `acquire`.
- **Stop idempotency:** успешный `stop` → второй и третий `stop` вернули `Ok(())`, артефакт и `size_bytes` не изменились. Ошибочный путь: `first == second == third == "контейнер пуст: нет ни одного видеосэмпла"` — **без** `recording container finalize failed:` и без двойного префикса. `RecorderControl::stop` (`recorder.rs:126-137`) снимает `Container`-обёртку, `stopped_cache` хранит сырую строку (`:418`), повторный DELETE отдаёт её как есть (`:99-104`).
- **Restart/hydration:** real SQLite (`sqlite:///dev/shm/...?mode=rwc`) — первый finalize ошибся `"контейнер пуст…"`, durable-строка `duration_sec=1, size_bytes=0`; после переоткрытия БД и `state.sync_from_storage()` строка гидрируется с теми же полями. Повторный DELETE после «рестарта» возвращает **Ok**, потому что `stopped_cache` — in-memory и ошибка финализации не персистится (см. P3-5).
- **Tap writer error vs stop:** статически ошибка теперь кладётся в `StopOutcome.finalize_error` (`recorder.rs:410-417`, `550-556`), первый и повторный DELETE используют один и тот же сырой текст. Детерминированно воспроизвести writer-error с реальной x264-группой не удалось (PTS фермы монотонен); гонка вставки ошибки остаётся (P3-2).

## §5. Находки

### P2

**P2-1. Остаточный stale-success: guard проверяет группу комнаты, а не tap сессии.**
`crates/vks-server/src/recorder.rs:51-63`: при `is_recording=true` и `active_h264_groups(room)==0` возвращается честная ошибка (`:56-58`), иначе — `Ok(existing_id)` (`:59-63`). `active_h264_groups` считает **любую** живую H.264-группу комнаты с viewers>0, но tap сессии привязан к конкретному `EncoderKey`. Если записываемая группа закрылась, а другая живая группа осталась (re-key/перекрытие, `gateway_video.rs:734-747`), immediate `start` возвращает старый id, не переподписывается на живую группу; detached finalize затем убивает старую сессию, а живая группа остаётся без записи. Исполнено 1/1: `immediate=Ok("18c03bb1-…") == old_id`, после финализации `groups=1, is_recording=false, row duration=1/size=0`; только следующий `start` даёт новый id. Это тот же ложный 200, что P2-1 review D; сценарий задания с одной группой исправлен 8/8, но общая приёмка «must NOT return Ok(stale id)» не выполнена. Фикс: хранить/проверять живость tap (abort-handle/состояние tap-таска) и при мёртвом tap либо явно ошибаться, либо сначала завершать старую сессию и стартовать новую с `subscribe` на живую группу; не выводить живость из `active_group_count`.

### P3

**P3-1. TOCTOU `expected record id` вне lock.** `finalize_after_tap_stop` читает `still_same` под lock и отпускает его (`recorder.rs:509-511`), затем `stop_recording_async` удаляет **любую** сессию комнаты (`:353-356`). Если между проверкой и удалением пользовательский stop+start успел создать новую сессию, detached-таск финализирует уже новую запись. Не воспроизведено (микроокно); фикс — передавать `expected` в `stop_recording_internal` и удалять под тем же lock только при совпадении `record_id`. Коммит `0cb6434` этот guard не менял.

**P3-2. Гонка ошибки tap с удалением `tap_errors`.** Tap-таск вставляет ошибку только после возврата `run()` (`recorder.rs:309-314`), а stop сначала абортит handle и сразу удаляет `tap_errors` (`:369-380`). Если abort сработал до вставки — ошибка теряется, stop возвращает 200 на усечённый файл; если вставка произошла после удаления — ошибка остаётся в map и будет приписана следующей сессии (start не очищает `tap_errors`, очищается только `stopped_cache` на `:301`). Не воспроизведено (нужен writer-fault); фикс — передавать ошибку в единый completion-path под одним lock и очищать `tap_errors` на старте.

**P3-3. Неудачный `storage.upsert_record` на старте оставляет живую сессию.** `recorder.rs:83-93`: сессия/tap/участник созданы, затем upsert; при ошибке возвращается 500, но `is_recording=true`, участник в RoomHub; повторный `start` идёт по idempotent-ветке и возвращает `Ok`, не повторяя persist. Не исполнено (нужен refusing storage). Фикс — rollback старта или повтор upsert в idempotent-ветке.

**P3-4. Detached finalize — fire-and-forget.** `recorder.rs:508-522`: `tokio::spawn` не сохраняется/не join-ится; при shutdown/рантайм-терминации pending `storage.upsert_record` может потеряться, хотя `is_recording` уже false. Не воспроизведено; фикс — трекать задачи и дожидаться на shutdown.

**P3-5. Кэш ошибки finalize не durable.** `stopped_cache` — in-memory (`recorder.rs:153`, `:418`); после рестарта процесса повторный DELETE не знает об ошибке. Исполнено на real SQLite: первый DELETE `Err("контейнер пуст…")`, после reopen+hydration второй DELETE `Ok(())` при `duration_sec=1/size_bytes=0`. Идемпотентный replay ошибки действует только в пределах процесса.

**P3-6. Нет committed-регрессии на stale-success.** `tests/recorder_tap.rs:210-255` (`recording_finalizes_when_encoder_group_closes`) дожидается `!is_recording` и только затем делает `start`; немедленного `start` в окне финализации нет. Фикс `0cb6434` прикрыт только внешними probe; counterexample из P2-1 committed-тестом не ловится.

**P3-7. Нет committed-проверки `has_b_frames=0`.** Допущение «B-кадров нет, `ctts` не пишется» задокументировано (`container.rs:14-19`), но `ffprobe_acceptance` не проверяет `has_b_frames=0`; наш независимый ffprobe дал `has_b_frames=0` на 4 live-файлах, однако регрессия с B-кадрами тестом не закреплена.

## §6. Гигиена и ограничения

- В живом репозитории создан только `docs/audit/2026-10-29/reviews/T09b_reviewE.md`; production-файлы, `Cargo.toml`, `Cargo.lock`, `target/` не менялись.
- Живые `vks_data.db`, `backups/`, порты `:8080/:18082` и процессы не открывались; `sha256` БД и mtime до/после совпадают. Все БД/MP4/probe — в `/dev/shm`, после снятия доказательств удалены.
- Ограничения: (1) writer-error путь tap детерминированно не воспроизведён, поэтому P3-2 — статический; (2) файл >4 GiB не строился, проверен только arithmetic guard + unit-тесты; (3) AAC-файл содержит намеренно синтетический H.264, поэтому ffmpeg шумит на видео — аудиотрек при этом декодируется, committed-тест проверяет audio.
- Итог: **P0=0, P1=0, P2=1, P3=7, score 9.0, done: нет.** Точечный stale-probe задания, повторный DELETE, MP4/avcC/AAC/esds, detached finalize и gates — подтверждены исполнением; блокирует приёмку остаточный ложный 200 при второй живой группе.
