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
  `ffmpeg -encoders/-decoders` с таймаутом 2 с.
- `WorkerPool` — host-wide лимит процессов (`VKS_CODEC_WORKER_POOL`,
  по умолчанию 8), `try_spawn` отказывает, а не ставит в очередь.
- `SlotConfig` — per-slot bitrate/fps/width/height с валидацией под кодек
  (H.261 только 176×144/352×288, H.263 до CIF, H.265 до 1080p).
- Изоляция: `env_clear` (в дочерний процесс не попадают `VKS_*` секреты),
  минимальные `PATH/LANG/LC_ALL`, `current_dir("/")`, отдельная process group,
  `kill_on_drop`, непрерывный drain stderr с последней строкой для диагностики.
- Low-latency режим: `-probesize 32 -analyzeduration 0` (без них FFmpeg
  буферизует до ~1.6 КиБ/5 с перед первым кадром).
- Самодекодирующиеся аудио-пакеты: ADTS (7-байтовый заголовок, 13-битная
  длина) и LOAS/LATM (3-байтовый заголовок) читаются целиком, без догадок
  о размере.

## Проверено

`TMPDIR=/var/tmp cargo test -p vks-media-mcu codec_worker` — 11 тестов:

- probe видит AAC/G.729/H.261/H.263/H.265 на этом хосте;
- AAC ADTS encode→decode и LATM encode→decode дают ненулевую энергию PCM;
- потоковый AAC-энкодер отдаёт первый ADTS-пакет без EOF;
- H.261/H.263/H.265 encode→decode возвращают кадры нужного размера;
- мусорный payload завершается ошибкой за <3 с и worker перезапускается;
- пул из 1 процесса отказывает второму и возвращает permit после drop;
- дочерний процесс не содержит `VKS_` в `/proc/<pid>/environ`, restart меняет pid.

Сервер при старте вызывает `transcoder_runtime::init()`: probe и пул
инициализируются, capability логируются и публикуются в `/metrics` как
`vks_codec_worker_supported{codec,direction}` и `vks_codec_worker_pool_size`.
`VKS_TRANSCODER_REQUIRED=1` делает неудачный probe фатальным (exit 78), по
умолчанию сервис остаётся на native.

## Честные границы

- SDP пока **не** объявляет новые кодеки: `VKS_SDP_CODEC_MATRIX=full` без
  реального packetizer/bridge приводил бы к «advertise-then-refuse».
  Включение — следующий этап вместе с H.265 RFC 7798 и AAC-LATM.
- Установленный FFmpeg 8.0.1 (GPLv2+, libx264/libx265) **не содержит**
  энкодеров G.719, G.722.1/G.722.1C, G.728 и G.729; H.261/H.263/H.265
  энкодеры есть. Эти кодеки нельзя включить «флагом»: нужен отдельный
  лицензированный кодек/железо либо отказ 488.
- H.261/H.263/H.265 RTP payloaders (RFC 4587/4629/7798) ещё не реализованы;
  текущий H.264 packetizer для них не годится.
- G.729 — только decode (recorder/monitor), не двусторонний SIP-мост.

## Конфигурация

| Переменная | По умолчанию | Назначение |
|---|---|---|
| `VKS_FFMPEG_BIN` | `ffmpeg` | путь к FFmpeg |
| `VKS_CODEC_WORKER_POOL` | `8` | максимум одновременных дочерних процессов |
| `VKS_TRANSCODER_REQUIRED` | `0` | `1` — падать при отсутствии FFmpeg (exit 78) |

## Следующий этап

1. H.265 RTP (RFC 7798): depacketizer/packetizer, VPS/SPS/PPS, FU/AP, IDR,
   per-slot profile, capacity refusal 1080p30.
2. AAC-LATM bridge: LOAS strip/re-add, SMC, 1024/2048 cadence, 90 кГц RTP,
   priming, playout buffer; отдельный PR.
3. H.261/H.263 — только после готового payloader (GStreamer или отдельный
   worker), не в одном PR с AAC/H.265.
