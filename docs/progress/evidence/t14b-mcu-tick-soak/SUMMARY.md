# T14b — MCU engine tick SLO / soak: итог

Дата: 2026-10-23. Артефакт: commit `dce2add`, clean worktree (`git_dirty=false`).
Единственная таблица результатов и полная методика — в [`README.md`](README.md);
машинный отчёт — [`soak-60s-4p.json`](soak-60s-4p.json). Здесь намеренно нет
копии чисел, чтобы они не расходились с JSON.

## Сделано

1. `vks_mcu_tick_duration_seconds{phase="audio"|"video"}` — 17 бакетов с точными
   границами 2 мс и 33 мс, env-gate `VKS_MCU_TICK_METRICS=1` (default off),
   кэшированные child-хендлы, gate внутри `observe_mcu_tick`. При включении
   создаются zero-count series, **не** синтетический `observe(0.0)`; при
   выключении family отсутствует в `/metrics`.
2. Инструментированы только успешные `RoomState::tick_audio_at` /
   `tick_video_at`; throttle и админ-пересборки (`clear_participant_frame`,
   `unbridge_participant`) в гистограмму не попадают — это проверяется soak-тестом.
3. `new_registry()` — документированное живое представление над глобальными
   коллекторами (Arc-клоны). Тест сравнивает полный набор family с
   `prometheus::gather()` и доказывает общее состояние по значению счётчика.
   `MCU_TICK_DURATION` приватна. Добавлены тест default-off (включая вызов
   `observe_mcu_tick` с выключенным флагом) и тест env-on: обе фазы стартуют с
   count=0/sum=0, реальный тик 1 мс даёт count=1.
4. In-process release-soak: реальные 1280×720 I420-кадры (2-D luma + градиенты
   chroma), 4 участника, звук около полной шкалы (ветка `tanh`); измерение
   делится на последовательные окна audio 20 мс и video 34 мс (сегодня глобальный
   `rooms`-мьютекс исказил бы аудиокаденс; per-room actors — S20). Warm-up 3 с
   отбрасывается, отчёт пишется до assert'ов, считаются attempts/first
   successes/throttled/second-call successes/drops/effective rates, провенанс
   (git sha, dirty, rustc, CPU, точная команда), точные `le`-отношения и интервал
   бакета p99. Каждый период делает дублирующий same-timestamp вызов; второй
   должен быть throttled, видео-throttle обязан увеличить `frames_dropped`.
   Гейты: count == successes, attempts−ticks == throttled, drops == throttled,
   админ-путь не тикает, ≥45 audio/s и ≥28 fps, exact `le=2ms`/`le=33ms` ≥0.99.
5. Композитор: точная целочисленная nearest-neighbour-раскладка
   (`floor(dx*fw/dw)` инкрементально), целочисленная letterbox-геометрия,
   row-slice, один аллоцированный канвас; golden-тесты на 2-D luma, chroma по
   обеим осям, letterbox и PiP-полосы (полосы не закрашиваются — сохраняют
   нижний слой; так и задокументировано).

## Результат

Audio engine-work p99 ≤2 мс **выполнен** (exact `le=2ms` = 1.000; p99 в
(0.25, 0.5] мс). Video compose укладывается в 33 мс бюджет (exact `le=33ms` =
1.000, p99 в (20, 33] мс), но **не в 2 мс** (exact `le=2ms` = 0.000); цель 2 мс —
S20 (SIMD/zero-copy/параллельные тайлы, per-room actors). Точные числа и
ограничения — только в README/JSON.

## Ограничения

Только движок: без RTP/SIP/H.323, decode/encode, `resize_i420`, WebRTC-fan-out,
live DB и сети. Одна комната, фазы измеряются последовательно: нет конкуренции
за глобальный `rooms`-мьютекс, allocator pressure и 6–8 комнат. 1500/883 samples
достаточны для точных rank-гейтов, но не для точной точки p99. Живой `/metrics`
этим артефактом не проверялся (на live сейчас нет активных медиакомнат);
default-off и env-on zero-count проверены отдельными тестами.

## История ревью

Раунд 1: 7.5–8.4/10 (фальшивый startup-sample, logical-clock soak, тест не
вызывал `new_registry`, неверный путь, «80 мс» без доказательств). Раунд 2:
6.5–8.6/10 (серые вместо I420 кадры, устаревшие числа после re-pin, хрупкий
45 Гц гейт, узкий `git_dirty`, debug-сборка сломана const assert). Раунд 3
исправил всё перечисленное: I420 + раздельные окна, точные и невакуумные
throttle/drop-гейты, провенанс из чистого worktree, runtime debug guard,
single-source числа, новые тесты метрик. Повторная оценка — см. журнал задачи.
