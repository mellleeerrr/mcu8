# scripts/bench — стенд нагрузки и методика замера ёмкости

Полное обоснование, формулы и открытые вопросы: [`docs/audit/2026-10-25/06_perf_lab.md`](../../docs/audit/2026-10-25/06_perf_lab.md).
Здесь — только «как запускать» и «что нельзя».

> **Статус (20:15 MSK):** измерены итерации 1 `8ab81afd…`, 2 `f6446b81…` (N=20: **0.954 ядра/устройство**)
> и production-**3b `3353dcb2…`** (O7-ферма: N=20 **0.360 ядра/устройство**, ffmpeg-детей N+1, loss 0.000 %,
> MOS 4.365–4.369 — §9 артефакта). H.323: 20/20 (G.711U), смешанная 10 SIP-видео + 10 H.323-аудио — 10/10 и 10/10.
> Потолок — пул RTP-портов 41000–41200 = **100 пар → 50 видеозвонков** (видео берёт 2 пары) и `MAX_CALLS`
> (SIP 64 / H.323 20). **T2 (netem) снят на «Итерации 6» `95cc78fc…`, PID 1822353 — `/tmp/vks-bench/t2-iter6/`
> (`t2_summary.md`, `T2_ANALYSIS.md`, `postcheck.json clean=1`); все профили `valid=true`, сирот/зомби 0.**
> Найден и закрыт дефект ingest: 5 мс джиттера на входе роняли видео ×100; «Итерация 7» (`ef37731f…`) — регрессия
> T2-RTT30: видео 18 238 пкт/бот, `c_device` 38.93 %, loss 0.0055 %. **H.323-смесь 10 SIP-720p30 + 10 H.323
> (G.722+1080p30): 10/10 и 10/10, SIP loss 0/MOS 4.369, H.323 видео loss 0 %, 4.72 ядра на 20 устройств.**
> Изоляция — `taskset` (`systemd-run --user AllowedCPUs` игнорируется); netns `vks-bench` + NOPASSWD `ip`/`tc`.
> Правило неизменно: **прогон без совпавшего паспорта sha256 до/после — неатрибутируем**.

## T2 (netem) — один вход

```bash
scripts/bench/t2_matrix.sh <expect-sha256> /tmp/vks-bench/t2   # 6 профилей: baseline/loss/2/delay
scripts/bench/t2_matrix.sh --plan                             # только напечатать команды
```
Требует NOPASSWD на `/usr/sbin/ip` и `/usr/sbin/tc`; между профилями qdisc снимается,
`tc -s qdisc` до/после кладётся в каталог профиля как доказательство шейпинга.
`--only T2-4b,T2-5b` — подмножество профилей; `--plan` — только печать команд.

Что встроено в драйвер (по итогам T2 на 4/5/6):
- **три детектора churn/утечки**: `orphan_ffmpeg_max` в `results.json` (`ffmpeg_count_max − n_connected − 1`,
  ожидание 0), `jb_late_total` (constant delay не должен давать late — ненулевое = разрыв RTP-ts),
  число видео-пакетов на бота (обрыв потока виден сразу);
- **перед профилем** ждёт `abonents=0` и `ffmpeg ≤ base+1`, остаток пишет в `precheck.json`;
- **после серии** — `postcheck.json {ffmpeg, abonents, children, zombies, clean}`; `clean=1` только при
  `ffmpeg=0 ∧ abonents=0 ∧ zombies=0` (приёмочный критерий утечки);
- **`--allow-partial`**: под netem `valid` — по паспорту, недозвоны — отдельной колонкой;
- **netem limit:** при задержке 75/150 мс и ~3.6 тыс. pps дефолтный `limit 1000` теряет пакеты сам
  (tc `dropped` 3540/4535 → бот видит 1.9–2.5 % «потерь»). Для delay-профилей — `limit 100000`
  (профили `T2-4b`/`T2-5b`); без этого выводы о задержке недействительны;
- **CPU под root в netns:** `/sys/fs/cgroup` у root может быть пуст (cgroup-namespace DSH) —
  `sipbot.mcu_cgroup_path` падает на `/proc/<pid>/root/…`; иначе `c_device=0` во всех прогонах.
- **H.323-видео в смеси:** 10 вызовов × 1080p30 ≈ 310 пкт/с на вызов; при смеси с SIP python-бот
  не успевал читать сокеты (дефолтный rcvbuf) → `Udp:RcvbufErrors` ≈ потерям. Лечится `SO_RCVBUF=4 МБ`
  (уже в коде); диагностика: `grep ^Udp: /proc/net/snmp` до/после, `--rx-dump` — первые 40 RTP-заголовков.
- **`--dump-rx bot-00[,bot-05]`** (`matrix.py` и `t2_matrix.sh`): дамп всех принятых RTP пакетов бота в
  `rx-NN.jsonl` (`pt, seq, ts, arrival, src`) — таймлайн-разбор `jb_late`/пауз аудио; так закрыт P2 (артефакт,
  §13.1 артефакта).
- **`matrix.py --extra-decoders N`:** в смеси SIP+H.323 FFmpeg-детей = 1 enc + N_sip + N_h323; без флага
  `orphan_ffmpeg_max` ложно покажет N_h323 (формула считает только SIP-устройства).

## Порядок (см. §10 артефакта)

```bash
cd "/home/pro/Рабочий стол/mcu"
export VKS_PY=/home/pro/.local/share/vks-mcu/h323-venv/bin/python3   # pycrate 0.8.1 только отсюда

# 0) паспорт артефакта: живой бинарь == замороженному, иначе прогон неатрибутируем
scripts/bench/verify_artifact.sh
#    root-часть (делегирование cpuset) — один раз, выполняет Lead: ROOT_COMMANDS.md §A

# 1) эталонные медиа (без MCU): ffmpeg -> scripts/bench/media/*.h264, *.g722
python3 -c "import sys; sys.path.insert(0,'scripts/bench'); import sipbot; \
  [sipbot.ensure_video(p) for p in ('1080p30','720p30','720p15','360p15')]; sipbot.ensure_g722()"

# 2) смоук бота без MCU: заглушка UAS + 1 вызов (5 с)
scripts/bench/stub_uas.py --port 15060 --media-port 25000 --duration 20 &
scripts/bench/sipbot.py --room 00000000-0000-0000-0000-000000000001 --host 127.0.0.1 \
  --sip-port 15060 --local-ip 127.0.0.1 --audio-codec PCMA --video-profile 720p30 \
  --duration 5 --json-out /tmp/smoke/bot0.json --mcu-pid 0 --no-mcu-cpu

# 3) комнату создаём один раз (UUID обязателен для SIP UAS)
ROOM=$(scripts/bench/room.sh ensure --name "bench-$(date +%s)" | tail -1)

# 4) серия 1/2/5/10/20 × 60 с: боты pinned 16-35, MCU не пиннится, паспорт обязателен
scripts/bench/series.py --ns 1,2,5,10,20 --video-profile 720p30 --duration 60 \
  --room "$ROOM" --bot-cores 16-35 --max-load 8 \
  --expect-sha256 "$(sha256sum /proc/$(systemctl --user show -p MainPID --value vks-mcu.service)/exe | cut -d' ' -f1)"

# 5) формула по накопленным прогонам
scripts/bench/formula.py --runs /tmp/vks-bench/runs --limits 70,85,95 --markdown
```

## Файлы

| Файл | Что делает | Требует root |
|---|---|---|
| `verify_artifact.sh` | паспорт бинаря: sha256 живого `/proc/<pid>/exe` vs диска; gate для серии | нет |
| `sipbot.py` | **основной SIP-бот** (UAC): PCMU/PCMA/G722 + H.264 720p/1080p, ровно 50 pps / FPS, статистика по SSRC (RFC 3550 A.1) | нет |
| `matrix.py` | **основной прогон**: N ботов синхронно, CPU контура из cgroup, ffmpeg_share, `results.json` + `summary.md`, паспорт sha256 до/после | нет |
| `series.py` | серия N=1/2/5/10/20: α(N), γ=1+log2α(20/10), сводная `series.md`; помечает серию неатрибутируемой, если бинарь менялся | нет |
| `ROOT_COMMANDS.md` | ровно те команды, что требуют root (делегирование cpuset, netns/veth/netem), с откатом | — |
| `setup_isolation.sh` | `taskset`/cgroup-cpuset, `veth`+`netns` (root-режим) | только `--root` |
| `teardown_isolation.sh` | обратные действия, affinity 0-35 | только `--root` |
| `netem.sh` | loss/delay/jitter на bot-стороне veth | **да** |
| `prepare_media.sh` | Annex-B H.264/H.265 + PCM через ffmpeg 8.0.1 | нет |
| `room.sh` | login → создать/найти комнату → UUID (UUID обязателен для SIP UAS) | нет |
| `run_matrix.sh` | один прогон: idle → боты → окно → teardown → формула | нет |
| `collect.sh` | `cpu.stat` контура MCU + pressure + дети FFmpeg + `pidstat`/`mpstat`/`perf` | нет |
| `parse.py` | прогоны → Markdown + JSON | нет |
| `formula.py` | `c_device`, `N_per_core`, α/γ, E-model MOS | нет |
| `mix.py` | **смешанный прогон** SIP-видео + H.323 (G.722+1080p30): синхронный старт, `--extra-decoders`, UDP-счётчики до/после, postcheck, `mix_summary.md` | нет |
| `t2_matrix.sh` | **T2 netem-матрица**: 6 профилей (baseline/loss/delay) на обоих veth, паспорт-гейт, `precheck`/`postcheck`, `t2_summary.md` | **да** (`ip`/`tc` NOPASSWD) |
| `h323bot.py` | H.323-бот: sidecar как UAC + свой RTP; `h323-m1`=G.711U, `h323-m2`=G.722, `--video-profile 1080p30/720p30` — H.241/H.264 (10/10 в смеси, loss 0 %); 4 порта на вызов (аудио p,p+1 / видео p+2,p+3) | нет |
| `stub_uas.py` | SIP-заглушка для смоука бота без MCU | нет |
| `drafts/vks-sipbot.rs` | скелет Rust-бота (не компилировался); Python-бот заменяет его в этом раунде | — |

## Что нельзя (жёсткие правила)

1. **Не** `systemctl restart/stop vks-mcu.service`, **не** `scripts/run-local.sh` — живой сервер не трогаем.
2. **Не** писать в `configs/`, `deploy/`; не менять `sysctl` хоста.
3. Боты — только порты `5080+/20000-29999` (SIP), `2720+/30000-39999` (H.323).
   Пул шлюза `41000-41200` и WebRTC `43000-43100` занимает MCU: **не пересекать**.
4. H.323-боты — только `--local-ip 192.168.1.93` при `--target 192.168.1.93:1720` (LAN в allowlist
   `127.0.0.1/32,192.168.1.0/24`); H.245-адрес в sidecar должен совпадать с этой же LAN, иначе соединение рвётся.
5. Шумная машина = невалидный замер. `matrix.py --max-load 8` помечает прогон
   `noisy=true` (не отказывает, чтобы видеть тренд), `run_matrix.sh` — отказывает.
   При loadavg ≥ 12 наблюдались потери входящего аудио до 7.4 % при `kernel_drops=0`:
   официальные прогоны — только в согласованное окно.
6. **Не мерить без паспорта артефакта.** Живой `vks-server` может работать со старого
   удалённого inode, пока на диске лежит другой бинарь. `run_matrix.sh` откажется
   стартовать при `live_matches_disk != yes` (обход только `ALLOW_UNFROZEN=1`,
   тогда в meta `unfrozen=true`). Одна сборка на серию, отдельный `CARGO_TARGET_DIR`.

## Метрики `matrix.py` (основной путь)

`results.json`: `summary` (паспорт `mcu_exe_sha256_before/after`, `valid`/`invalid_reason`,
`p_idle`, `p_active`, **`c_device_pct_one_core`**, `n_per_core{U70,U85,U95}`,
`p_ffmpeg`/`ffmpeg_share` (тики по пересечению PID — набор детей меняется), `ffmpeg_count_max`, `pids_max`,
`threads_max`, `rss_kb_max`, `pressure_some_max`, **`orphan_ffmpeg_max`** (детектор утечки фермы),
`rx_audio`/`rx_video` с потерями по RFC 3550 A.1, `jitter_ms`, `mos`, `jb_late_total`, `rx_kernel_drops_total`),
`series` (срез 1/с), `farm_series` (снимок `/api/v3/mcu/farm` 1/с при `--api`),
`bots` (полный JSON каждого бота), `host`. Плюс `summary.md` с таблицей.

## Метрики `run_matrix.sh`/`collect.sh` (старый путь, оставлен для T2/изоляции)

- `runs/<name>/idle/` и `active/`: `samples.json` (per-second: `usage_usec`, `user_usec`,
  `system_usec`, `ffmpeg_ticks`, `ffmpeg_count`, `pids_total`, `pressure_cpu_some_avg10`,
  `memory_current`, `mcu_allowed`), `meta.json`, `pidstat_mcu.log`, `mpstat_mcu.log`,
  `mpstat_bots.log`, `mcu_process_tree.tsv`.
- `bot/*.jsonl`: по строке на вызов — `disposition`, `rx_packets`, `lost`, `loss_pct`,
  `jitter_ms`, `mos`, `fps`, `send_lag_p99_us`.
- `summary.json`, `formula.md`: `c_device` в % одного ядра, `N_per_core` для 70/85/95 %,
  `ffmpeg_share`, `α(20/10)`, `γ`, класс SLA.
