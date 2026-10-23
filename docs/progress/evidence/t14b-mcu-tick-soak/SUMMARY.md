# T14b — MCU tick SLO / soak (engine-level)

Дата: 2026-10-23. Ветка/HEAD на момент прогона: `cf11422` + локальные правки T14b.

## Что сделано

1. Метрика `vks_mcu_tick_duration_seconds{phase="audio"|"video"}` (16 бакетов,
   точная граница 2 мс) в `crates/vks-common/src/metrics.rs`. Регистрируется
   только при `VKS_MCU_TICK_METRICS=1`, child-хендлы кэшированы, `observe` —
   атомики без аллокаций.
2. Инструментированы **только** успешные тики двигателя
   (`RoomState::tick_audio_at` / `tick_video_at`) вокруг `process_audio` /
   `process_video`; throttle и админ-пересборки (`clear_participant_frame`,
   `unbridge_participant`, `remove_publisher`) в гистограмму не попадают.
3. In-process soak `crates/vks-media-mcu/tests/mcu_tick_soak.rs` (`#[ignore]`,
   только release): реальная комната, 4 участника, PCM + 1280×720 I420,
   каденция 20/33 мс, чтение гистограммы через `prometheus::gather()`, JSON-отчёт.
4. Оптимизирован композитор: 16.16 fixed-point вместо `f64`-деления на каждый
   пиксель, hoist YUV-проверок, row-slice вместо `get()` на пиксель, канвас
   одним аллоцированием (было: fill + `resize` 3 МБ). До: p99 ≈ 80 мс,
   после: p99 ≈ 19.8 мс (4×720p, канвас 1920×1080).

## Результат 60 с (4 участника, release)

| phase | samples | exact `le=2ms` | p99 | sum |
|---|---:|---:|---:|---:|
| audio mix | 3000 | **1.000** | **0.22 мс** | 0.196 с |
| video compose | 1182 | 0.000 | **19.8 мс** | 12.13 с |

- Аудио-SLO **p99 ≤ 2 мс выполнен** с запасом.
- Видео-compose укладывается в 33 мс бюджет кадра, но **не в 2 мс** — это
  честный разрыв, следующий срез: SIMD/zero-copy/параллельные тайлы, затем
  per-room actors вместо глобального `rooms`-мьютекса.

## Ограничения доказательства

Только движок: нет RTP/SIP/H.323, decode/encode, `resize_i420`, WebRTC-fan-out
и live DB. Одна комната не моделирует конкуренцию за глобальный мьютекс и
6–8 комнат. Это не полный media-SLO; 24–72 ч live soak требует реальных
вызовов/стенда и описан в `README.md`.

## Артефакты

- `README.md` — методика, PromQL, процедура 24–72 ч.
- `soak-60s-4p.json` — машинный отчёт прогона.
- `SHA256SUMS` — хеши файлов каталога (пути от корня репозитория).
