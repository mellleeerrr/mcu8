# T14b — MCU engine tick SLO / soak: итог

Дата: 2026-10-23. Артефакт прогона: commit `aadd264` (`perf(mcu): exact
compositor, honest tick soak, zero-count metric init`), dirty=false.
Полная методика, команды и ограничения: [`README.md`](README.md); машинный
отчёт: [`soak-60s-4p.json`](soak-60s-4p.json).

## Сделано

1. `vks_mcu_tick_duration_seconds{phase="audio"|"video"}` — 17 бакетов с точными
   границами 2 мс и 33 мс, env-gate `VKS_MCU_TICK_METRICS=1` (default off),
   кэшированные child-хендлы, gate внутри `observe_mcu_tick`. При включении
   создаются zero-count series, **не** синтетический `observe(0.0)`; при
   выключении family отсутствует в `/metrics`.
2. Инструментированы только успешные `RoomState::tick_audio_at` /
   `tick_video_at`; throttle и админ-пересборки (`clear_participant_frame`,
   `unbridge_participant`, `remove_publisher`) в гистограмму не попадают — это
   проверяется soak-тестом, а не только глазами.
3. `new_registry()` теперь регистрирует Arc-клоны реальных коллекторов вместо
   рукописных `Opts`; тест действительно вызывает `new_registry()` и проверяет
   все 21 family. Добавлены тест default-off в отдельном процессе и тест
   монотонности/наличия границ 2 мс и 33 мс.
4. In-process release-soak: часы реального времени, 4 участника, звук около
   полной шкалы (ветка `tanh`), кадры выставляются один раз, warm-up 3 с
   отбрасывается, отчёт пишется до assert'ов, считаются attempts/success/
   drops/effective fps, провенанс (git sha, rustc, CPU), точные `le`-отношения
   и интервал бакета p99. Гейты: count == успешные тики, throttled ==
   `frames_dropped`, админ-путь не тикает, ≥45 audio/s и ≥28 fps, точные
   отношения `le=2ms` и `le=33ms` ≥0.99.
5. Композитор: точная целочисленная nearest-neighbour-раскладка
   (`dx*fw/dw` инкрементально), целочисленная letterbox-геометрия, row-slice,
   один аллоцированный канвас; golden-тесты на градиент и letterbox.

## Результат 60 с (57.0 с измерено, 4 участника, release)

| phase | samples | exact `le=2ms` | p99 bucket (estimate) | mean |
|---|---:|---:|---:|---:|
| audio engine work | 2566 | **1.000** | (0.1, 0.25] мс (0.249 мс) | 0.130 мс |
| video compose | 1720 | 0.000 | (10, 20] мс (19.6 мс) | 9.87 мс |

Video: 30.17 fps, exact `le=33ms` = 1.000, drops 0. Аудио-SLO **p99 ≤2 мс**
выполнен. Видео укладывается в 33 мс бюджет кадра, но **не в 2 мс**; p99 известен
только как интервал (10, 20] мс. Цель 2 мс для видео — S20
(SIMD/zero-copy/параллельные тайлы, per-room actors), не этот срез.

## Ограничения

Только движок: без RTP/SIP/H.323, decode/encode, `resize_i420`, WebRTC-fan-out,
live DB и сети. Одна комната: нет конкуренции за глобальный `rooms`-мьютекс,
allocator pressure и 6–8 комнат. 2566/1720 samples достаточны для точных
rank-гейтов, но не для точной точки p99. Живой `/metrics` этим артефактом не
проверялся: на live сейчас нет активных медиакомнат; default-off проверен
отдельным тестом.

## История ревью

Первичный независимый ревью-раунд: 7.5/10 и 8.4/10 (adversarial, elegance
8.4). Замечания P1: фальшивый startup-sample, `new_registry` тестировал не ту
функцию, harness шёл на ~20 fps из-за логических часов и скрывал 35% дропов,
команда воспроизведения указывала в чужой каталог. Все P1 и существенные P2
исправлены в `aadd264`; после исправлений harness даёт честные 30.17 fps,
точные отношения и провенанс. Повторное ревью — см. журнал задачи.
