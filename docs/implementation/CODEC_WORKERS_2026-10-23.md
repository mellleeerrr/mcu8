# Изолированные кодеки-воркеры (T06b, этап 1)

Дата: 2026-10-23. Связанные документы: `docs/architecture/ARCHITECTURE.md` §3,
`docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md`.

## Что сделано

В `vks-media-mcu` добавлен модуль `codec_worker` — граница процесса для
кодеков, которых нет в native-профиле шлюза:

- `FfmpegWorker` — один долгоживущий дочерний FFmpeg на направление
  (`pipe:0 -> pipe:1`), обмен фиксированными кадрами:
  - аудио: 20 мс mono `s16le` 48 кГц (960 сэмплов);
  - видео: один `yuv420p` кадр разрешения слота.
- `FfmpegCapabilities::probe` — один разовый разбор
  `ffmpeg -version/-encoders/-decoders` с таймаутом 5 с на команду
  (худший случай 15 с; при холодном старте один повтор после 300 мс — до ~30,3 с).
- `WorkerPool` — host-wide лимит процессов (`VKS_CODEC_WORKER_POOL`,
  по умолчанию 8), `try_spawn` отказывает, а не ставит в очередь.
- `SlotConfig` — per-slot bitrate/fps/width/height с раздельной валидацией:
  энкодер H.261 только 176×144/352×288, H.263 128×96/176×144/352×288/704×576/
  1408×1152, H.265 ≥16×16 и до 4096×2160; декодер принимает чётное
  положительное разрешение до 4096×2160 (FFmpeg scales to slot).
- Изоляция: `env_clear` (в дочерний процесс не попадают `VKS_*` секреты),
  минимальные `PATH/LANG/LC_ALL` (+`LD_LIBRARY_PATH` для приватного FFmpeg),
  `current_dir("/")`, отдельная process group,
  `kill_on_drop` + killpg, непрерывный drain stderr с последней строкой для
  диагностики. rlimits/`PR_SET_PDEATHSIG` не выставлены; единственный
  `unsafe` — `libc::kill(-pgid)` для group-kill (ADR-0028,
  SAFETY-комментарий). Это ограничение документально, не «сделано».
- Low-latency режим: `-probesize 32 -analyzeduration 0` (без них FFmpeg
  буферизует до ~1.6 КиБ/5 с перед первым кадром); H.265 дополнительно
  `-tune zerolatency -x265-params bframes=0:rc-lookahead=0:scenecut=0:
  frame-threads=1:pools=1:aud=1:repeat-headers=1` (AUD даёт мосту границу AU,
  repeat-headers — VPS/SPS/PPS для поздних участников; без `frame-threads=1:pools=1` x265 задерживает
  первый кадр на ~4 кадра).
- Частичное чтение/таймаут/ошибка разбора/превышение лимита «отравляют»
  worker: процесс убивается, последующий I/O возвращает
  `Exited("worker is poisoned; restart is required")`, повторное
  использование — только через `restart()`.
- Публичный `write_encoded` для decoder-направления; вызовы методов не того
  направления возвращают `InvalidConfig` до касания pipe. `read_encoded_video`
  возвращает `Ok(None)` на нормальном EOF (ошибки не смешиваются с концом потока).
- `read_encoded_video` отдаёт один Annex-B NAL (с start code) только для
  H.265; H.261/H.263 не Annex-B и читаются только `read_to_end` после
  `finish_input` (EOF-only). Последний NAL AU приходит со следующим AU, поэтому
  мосту нужен lookahead в один AU и `io_timeout` больше паузы потока.
- Самодекодирующиеся аудио-пакеты: ADTS (7-байтовый заголовок, 13-битная
  длина) и LOAS/LATM (3-байтовый заголовок) читаются целиком, без догадок
  о размере.

## Проверено

`TMPDIR=/var/tmp cargo test -p vks-media-mcu codec_worker` — 21 тест:

- probe видит AAC/G.729/H.261/H.263/H.265 на этом хосте;
- AAC ADTS encode→decode даёт ненулевую энергию PCM; LATM encode→decode — PCM нужной длины;
- потоковый AAC-энкодер отдаёт первый ADTS-пакет без EOF;
- H.261/H.263/H.265 encode→decode возвращают кадры нужного размера (H.265
  с realtime-параметрами x265);
- мусорный payload завершается ошибкой за <3 с и worker перезапускается;
- пул из 1 процесса отказывает второму и возвращает permit после drop;
- дочерний процесс не содержит `VKS_` в `/proc/<pid>/environ`, restart меняет pid;
- таймаут чтения отравляет worker, restart восстанавливает поток;
- все семь directional-методов при неверном направлении дают `InvalidConfig` без блокировки;
- H.265 отдаёт Annex-B NAL до EOF; H.263 704×576 проходит roundtrip;
- зависший probe убивает всю process group за timeout, включая forked sleep (Linux-тест);
- смешанные 3-/4-байтовые start codes не оставляют лишний нуль в предыдущем NAL;
- H.261 отклоняет потоковое NAL-чтение (нет Annex-B).

Сервер при старте вызывает `transcoder_runtime::init()`: probe и пул
инициализируются, capability логируются и публикуются в `/metrics` как
`vks_codec_worker_supported{codec,direction}` и `vks_codec_worker_pool_size`.
`VKS_TRANSCODER_REQUIRED=1` делает неудачный probe фатальным (exit 78), по
умолчанию сервис остаётся на native.

## Честные границы

- SDP пока **не** объявляет новые кодеки: `VKS_SDP_CODEC_MATRIX=full` без
  реального packetizer/bridge приводил бы к «advertise-then-refuse».
  Включение — следующий этап вместе с H.265 RFC 7798 и AAC-LATM.
- `VKS_TRANSCODER_REQUIRED=1` проверяет только наличие FFmpeg, а не полноту
  матрицы; старый `VKS_TRANSCODER_ENABLED` по-прежнему управляет SDP и пока
  не сверяется с probe. Не включать `full` до wiring.
- Установленный FFmpeg 8.0.1 (GPLv2+, libx264/libx265) **не содержит**
  энкодеров G.719, G.722.1/G.722.1C, G.728 и G.729; H.261/H.263/H.265
  энкодеры есть. Эти кодеки нельзя включить «флагом»: нужен отдельный
  лицензированный кодек/железо либо отказ 488.
- H.261/H.263/H.265 RTP payloaders (RFC 4587/4629/7798) ещё не реализованы;
  текущий H.264 packetizer для них не годится. H.265 encode/decode уже
  проверен roundtrip, но без RFC 7798 в SDP не выставляется.
- G.729 — только decode (recorder/monitor), не двусторонний SIP-мост.

## Конфигурация

| Переменная | По умолчанию | Назначение |
|---|---|---|
| `VKS_FFMPEG_BIN` | `ffmpeg` | путь к FFmpeg |
| `VKS_CODEC_WORKER_POOL` | `8` | максимум одновременных дочерних процессов |
| `VKS_CODEC_LICENSE_ACCEPTED` | off | список принятых лицензий (например `aac-latm`); без `aac-latm` AAC-LATM не рекламируется и не принимается даже при успешном probe |
| `VKS_AAC_LATM_LEGAL_ACCEPTED` | off | legacy-alias: `1` эквивалентен `aac-latm` в списке; оставлен для существующих конфигов |
| `VKS_TRANSCODER_REQUIRED` | `0` | `1` — падать при отсутствии FFmpeg (exit 78) |

## Следующий этап

1. H.265 RTP (RFC 7798): depacketizer/packetizer, VPS/SPS/PPS, FU/AP, IDR,
   per-slot profile, capacity refusal 1080p30.
2. AAC-LATM bridge: LOAS strip/re-add, SMC, 1024/2048 cadence, 90 кГц RTP,
   priming, playout buffer; отдельный PR.
3. H.261/H.263 — только после готового payloader (GStreamer или отдельный
   worker), не в одном PR с AAC/H.265.
