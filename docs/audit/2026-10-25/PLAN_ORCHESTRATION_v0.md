# MCU: план оркестрации и целевой архитектуры (v0, Lead)

> Статус: **v0 — план до раунда агентов.** После раунда контраргументации → `ARCHITECTURE_MASTER_PLAN.md` (v1, финал).
> Дата: 2026-10-25. Lead-сессия. Все факты ниже проверены на этой ВМ (не взяты из ТЗ на веру).

---

## 0. Проверка ТЗ: что подтвердилось, а что нет

| Утверждение ТЗ | Факт (проверено) | Следствие |
|---|---|---|
| «наша ВМ 192.168.1.92» | Мы **на 192.168.1.93** (`ens18`, MAC `bc:24:11:bc:96:40`). `192.168.1.92` — **живой TrueConf MCU** (порт 5060 open, ssh open), эталон для cross-MCU тестов T07b | Не трогаем `.92`. Документы в `doc/` писались под старый IP. Весь стенд — локально на `.93` |
| «20 SIP-ботов и 20 H.323-ботов» | **Ботов нет в системе**: нет `sipp`, `callgen323`, `baresip`, `pjsua`, `sshpass`. Есть `ffmpeg 8.0.1`, `gstreamer 1.28.2`, `python 3.14` | Загрузчики надо **создать** (свой Rust-бот на базе `vks-gateway-sip` + H.323-бот на `pycrate 0.8.1`) — иначе бенчмарк невыполним |
| «вызвать `/pnm/monitoring/sessions`» | Отдаёт **HTML SPA** (fallback), JSON-эндпоинта нет. Есть `/api/v3/sessions` (401 без токена), `/metrics` (Prometheus, 13 KB) | Нужно реализовать TrueConf-совместимый `monitoring/sessions` поверх `api_v3::sessions` |
| «собрать с -O3 -march=native -flto» | `Cargo.toml`: release `opt-level=3`, `lto="thin"`, `codegen-units=1`, `strip=symbols`; **нет `-march=native`, нет `target-cpu`** | Добавить `RUSTFLAGS="-C target-cpu=native"` / `.cargo/config` + `lto="fat"` кандидат |
| «убрать лишнее, освободить порты» | Порты 5060/40000/8080 держит **наш** `vks-server` (PID 1177177), 1720 — python-сайдкар. Чужих демонов нет | «Конфликты» — внутренние (дубли конфигов/артефактов), а не внешние процессы |
| «диск достаточно» | `/` 266G, занято 195G, **свободно 60G**; `target/` = **91G** (из них `target/debug` 87G) | Чистка `target/debug` — обязательный шаг перед LTO-сборкой/бенчмарком |
| «эталон = 3 движка» | 52G: `cisco CMS` 5.6G/54 059 файлов (готовые отчёты + `19_DEBATE_RESOLUTION.md`), `mcu trueconf` 373M/297 (реконструкция + MASTER_ARCHITECTURE), `vinteo` 46G/384 249 (15 отчётов + полная ФС) | Читать отчёты, а не сырые образы; сырьё — точечно |

---

## 1. Верифицированное окружение (базис всех решений)

| Параметр | Значение |
|---|---|
| CPU | 2× Intel Xeon E5-2699 v3 @2.30 GHz, **36 физических ядер**, SMT **выключен**, 1 NUMA, L3 32 MiB (2×16) |
| RAM | 56 GiB (свободно ~51 GiB) |
| Диск | `/` 60 GiB free; `target/` 91 GiB |
| OS | Ubuntu 26.04.1 LTS, kernel 7.0.0-38-generic, cgroup v2 |
| Rust | rustup, `cargo/rustc 1.94.0` (в `~/.cargo/bin`, в PATH не попадает в non-login shell) |
| Проект | `/home/pro/Рабочий стол/mcu` (= symlink `/home/pro/mcu`), 15 крейтов + SDK, 452 коммита, ветка `master`, рабочее дерево чистое |
| Сервер | `vks-server` release (2026-10-23), 38 потоков, idle 0.4 % CPU; SIP UAS 0.0.0.0:5060, H.323 UAS 0.0.0.0:1720, HTTP 8080, WebRTC UDP 40000, ICE 43000-43100 |
| Транскодер | ffmpeg-sidecar pool `max_workers=8`, H.264/H.265/G.711/G.722/G.726/G.728/G.729/Opus; AAC-LATM выключен |
| H.323 | python-сайдкар `/home/pro/.local/share/vks-mcu/h323-venv` (Python 3.14.4, `pycrate==0.8.1`) |
| Метрики | `/metrics` Prometheus; `vks_api_v3_requests_total`; MCU tick soak evidence `t14b` |
| Боты | **отсутствуют** — блокер бенчмарка |

### Текущий tech-stack (из `Cargo.lock`, 434 пакета)
`tokio 1.53.3`, `axum 0.8.9`, `hyper 1.12.0`, `tower 0.5.3`, `tower-http 0.6.11`, `str0m 0.24.1`,
`prost 0.13.5`, `tokio-tungstenite 0.29.0`, `rustls`/`ring 0.17.14`, `rcgen 0.14.10`,
`sqlx 0.8.6`, `serde 1.0.229`, `uuid 1.28.0`, `jsonwebtoken 9.3.1`, `prometheus 0.14.0`,
`figment 0.10.19`, `parking_lot 0.12.5`, `socket2 0.6.5`, `sha2 0.10.9`, `base64 0.22.1`,
`thiserror 2.0.21`, `anyhow 1.0.104`, `libc 0.2.190`. FFmpeg 8.0.1 (system), GStreamer 1.28.2.

---

## 2. Конфликт приоритетов — что будут доказывать агенты (ядро дебатов)

ТЗ требует «всё сразу». Ресурс один. Поэтому 6 направлений получают **несовместимые позиции**,
и каждый обязан защищать свою до консенсуса или до моего поражения:

| # | Агент | Позиция, которую защищает | Критерий победы |
|---|---|---|---|
| A | `ref-miner` | «Донор №1 для медиаядра — TrueConf (per-abonent ABOOK/6-уровневое наследование/разрешение = свойство раскладки); Vinteo — донор SIP/H.323 и сетевых параметров; Cisco — донор media-framework API/телеметрии» | Каждый перенос подтверждён `file:line` эталона + точкой встраивания у нас |
| B | `proto-chain` | «Главный блокер — неполная протокольная цепочка (SRTP/TLS, H.245/H.239, BFCP↔H.239, PLI↔videoFastUpdatePicture, ASN.1 PER), а не скорость медиа» | Список дыр с `file:line` + оценка, сколько из 20 устройств реально дозвонятся |
| C | `media-core` | «Главный блокер — compositor/mixer (nearest-neighbor, per-viewer encode, монолит `engine.rs` 120 KB); нужен shared composite canvas + SIMD + zero-copy» | Профиль CPU по функциям (`perf`) + план рефакторинга с ожидаемым выигрышем |
| D | `net-resilience` | «При 2 % loss / 30 мс jitter MCU деградирует из-за отсутствия NACK/RTX/FEC/PLC, примитивного jitter buffer, `recvfrom`-по-одному, дефолтных sysctl» | Замер потерь/джиттера + список механизмов с RFC-номерами |
| E | `regress-hygiene` | «Любая оптимизация утонет в регрессиях/дублях/конфликтах конфигов; сначала гигиена, единый канонический конфиг, чистка 87 GB debug» | Инвентаризация дублей/регрессий + чек-лист зачистки |
| F | `perf-lab` | «Без собственных ботов и изоляции ядер любые цифры — фикция; сначала harness + cgroup/veth, потом перф» | Рабочий harness + методика измерения только MCU-процесса |

**Правила дебатов (обязательны для всех):**
1. Только `cat`/`read` полного файла, ни одного «слепого» утверждения. Цитата = `путь:строка`.
2. Каждый тезис → «а что если наоборот?» → контртезис → доказательство.
3. Побеждает не риторика, а (а) воспроизводимый замер, (б) соответствие инвариантам проекта (`docs/PROJECT_RULES.md`, ADR), (в) отсутствие регрессий (тесты/клиппи).
4. Нельзя трогать: `.92`, живой `vks-server` (рестарт — только Lead), чужие файлы отчётов.
5. Раунд длится до консенсуса; несогласие фиксируется как «открытый вопрос» с обеими позициями.

---

## 3. Состав агентов

### Волна 1 — направления (durable teammates, параллельно, с диалогом)
| Агент | Задача | Артефакт |
|---|---|---|
| `ref-miner` | Полный разбор 3 эталонов; что переносим, что нет; пакеты/библиотеки/конфиги/параметры | `01_reference_mining.md` |
| `proto-chain` | SIP + H.323 (Q.931/H.225/H.245/H.239) + BFCP + SDP/ASN.1 + SRTP/H.235 + DTMF; дыры и патчи | `02_protocol_chain.md` |
| `media-core` | `vks-media-mcu` (compositor/mixer/vad/engine/codec_worker) + `vks-media-sfu` + транскодер; профиль и рефакторинг | `03_media_engine.md` |
| `net-resilience` | RTP/RTCP, сокеты, jitter, FEC/NACK/RTX/PLC, TWCC/REMB, sysctl, netem | `04_network_stack.md` |
| `regress-hygiene` | Регрессии, дубли, конфликты конфигов, мёртвый код, артефакты, версии пакетов | `05_hygiene_regressions.md` |
| `perf-lab` | Изоляция, боты, harness, формула ёмкости, SLA-матрица | `06_perf_lab.md` + `scripts/bench/*` |

### Волна 2 — послойные суб-агенты (после консенсуса волны 1)
Отдельные глубокие погружения по каждой цепочке: `sip-chain`, `h323-chain`, `sfu-webrtc-chain`,
`mcu-compositor-chain`, `codec-chain`, `presentation-bfcp-h239-chain`, `security-srtp-h235-chain`,
`cluster-storage-chain`. Каждый — с той же дисциплиной «позиция → контраргумент → доказательство».

### Итоговые документы Lead
- `AGENT_ROSTER_AND_DEBATE.md` — промты, ход диалога, кто кого переубедил, моё мнение.
- `ARCHITECTURE_MASTER_PLAN.md` — пошаговый план до версий пакетов, полный стек.
- `BENCHMARK_REPORT.md` — матрица прогонов и ёмкость 1 физического ядра.
- Код: патчи по цепочке + `scripts/bench/`.

---

## 4. Целевая архитектура (draft v0 — уточняется агентами)

### 4.1. Слои
```
┌──────────────────────────────────────────────────────────────────────────┐
│ L7  Backend API: axum 0.8 /api/v3 + PNM /pnm/monitoring/sessions (JSON)   │
│     auth (HS256→RS256 долг), RBAC, Prometheus /metrics, event bus        │
├──────────────────────────────────────────────────────────────────────────┤
│ L6  Signaling: SIP UAS/UAC (UDP/TCP/TLS, SRTP/SDP, 100rel, session-timer) │
│     H.323: Q.931/H.225 RAS/CS, H.245, FastStart, Tunneling, H.239, H.235  │
│     WebRTC/WS: str0m 0.24 (DTLS-SRTP, ICE, TWCC, simulcast)               │
│     Interworking bus: PLI/FIR ↔ H.245 videoFastUpdatePicture,             │
│     BFCP ↔ H.239 tokenRequest, DTMF 2833 ↔ UserInputIndication            │
├──────────────────────────────────────────────────────────────────────────┤
│ L5  Conference core: room FSM, layout engine (22 layouts/7 defaults),     │
│     per-viewer composition policy, floor/presentation arbitration        │
├──────────────────────────────────────────────────────────────────────────┤
│ L4  Media: MCU compositor (shared composite canvas per output profile),   │
│     audio mixer (N-1, top-K VAD, SIMD), codec worker pool (ffmpeg),       │
│     SFU forwarder (simulcast, BWE/TWCC/REMB), recorder                  │
├──────────────────────────────────────────────────────────────────────────┤
│ L3  Transport: RTP/RTCP (SRTP, NACK/RTX, FEC, PLI/FIR, REMB, XR),        │
│     adaptive jitter buffer, PLC, packet pacing, recvmmsg/sendmmsg        │
├──────────────────────────────────────────────────────────────────────────┤
│ L2  OS/threading: io_uring/epoll, worker-per-core, cgroup cpuset,        │
│     SO_REUSEPORT, busy-poll, hugepages, IRQ affinity                     │
├──────────────────────────────────────────────────────────────────────────┤
│ L1  Storage/cluster: SQLite/Postgres, cluster fan-out, recording         │
└──────────────────────────────────────────────────────────────────────────┘
```

### 4.2. Инварианты (не обсуждаются)
1. `MediaEngine` trait — прозрачность MCU/SFU для клиента (ADR-0003/0004).
2. Один канонический конфиг; prod fail-closed; секреты не в dev-дефолтах.
3. Паника одной сессии не роняет сервер (`panic=unwind`).
4. Любая оптимизация — под тестами (сейчас 1300+), без потери клиппи/фмт.
5. Ноль ручных шагов: всё в `scripts/`, воспроизводимо.

---

## 5. Методика бенчмарка (исправленная математика из `doc/`)

### 5.1. Изоляция
- **MCU**: cgroup v2 `mcu.slice` + `cpuset.cpus=0-15` (16 физ. ядер), `MemoryMax` 24G, `CPUWeight=1000`.
- **Боты**: `bots.slice`, `cpuset.cpus=16-35` (20 ядер), `CPUWeight=100`.
- **Сеть**: `veth`-пара `veth-mcu 10.200.0.1` ↔ `veth-bots 10.200.0.2` (+ netns для ботов), RTP-порты MCU 10000-30000, SIP 5060, H.323 1720; боты — 5080+/2720+.
- Замер **только** по PID MCU: `pidstat -u -r -t -p <pid> 1 120` + `mpstat -P 0-15` (исключая `%sys` ботов).
- Верификация: `ps -o pid,psr,comm` — потоки MCU и ботов на разных ядрах.

### 5.2. Формула (исправленная — в `doc/` была ошибка с `Ctotal` в знаменателе)
`pidstat` даёт %CPU, где 100 % = одно ядро. Если снимаем **только процесс MCU**:
```
c_device = (P_mcu_active − P_mcu_idle) / N        # % одного физ. ядра на устройство
N_per_core = floor(U_limit / c_device)
```
где `U_limit` = 70 % (High SLA, MOS≥4.3), 85 % (Work, MOS≥4.0), 95 % (Max).
Если снимаем всю систему: `c_device = (U_active − U_idle)·C_total / N` (только тогда делим на N).

### 5.3. Матрица
- Протоколы: P1 20 SIP, P2 20 H.323, P3 10+10, P4 15+5 / 5+15.
- Медиа: M0 signaling-only, M1 G.711a audio, M2 гетерогенное аудио (Opus/G.722/G.711/G.729),
  M3 гомогенное видео 720p30, M4 зоопарк (1080p60 H.265, 1080p30 H.264, 720p30 VP8/H.264, 360p15),
  M5 M4 + SRTP/H.235 + BFCP↔H.239.
- Динамика: ramp 2 вызова/с vs burst 20/с; netem loss 0.5 % / 2 %, jitter 30 мс.
- Каждый прогон: 10 с idle → 10 с ramp → **120 с замер** → 30 с смена спикера/презентации → teardown; 3 итерации.

### 5.4. SLA-классы приёмки
| Класс | MOS | FPS | Loss | OWD | Jitter | U_limit |
|---|---|---|---|---|---|---|
| A | ≥4.3 | 100 % целевого, 0 фризов | <0.1 % | <100 мс | <15 мс | 70 % |
| B | ≥4.0 | ≥90 % | 0.1–0.5 % | 100–150 мс | 15–30 мс | 85 % |
| C | ≥3.6 | ≥60 % | 0.5–1.5 % | 150–250 мс | 30–45 мс | 95 % |

---

## 6. Пошаговый план исполнения (после консенсуса агентов)

| Фаза | Шаг | Приёмка |
|---|---|---|
| **P0** | Гигиена: чистка `target/debug`, дубли конфигов, единый конфиг, инвентарь портов | `git status` чист, `cargo metadata` ок, конфиг один |
| **P1** | Native-сборка `RUSTFLAGS=-C target-cpu=native`, `lto=fat` A/B, `panic=unwind`, `strip` | бинарь работает, `cargo test`/`clippy` зелёные |
| **P2** | Протокольная цепочка: SDP/ASN.1, PLI↔H.245, BFCP↔H.239, DTMF, SRTP/TLS, H.235 | live SIP+H.323 дозвон 20/20, PLI доходит, презентация переключается |
| **P3** | Медиаядро: shared composite canvas, per-profile encode, SIMD mixer, zero-copy, worker-per-core | `perf` показывает снижение CPU/сессию ≥30 % на M3/M4 |
| **P4** | Сеть: recvmmsg/sendmmsg, jitter buffer, NACK/RTX/FEC, PLC, sysctl, SO_REUSEPORT, pacing | loss 2 % → MOS≥4.0, FPS≥27; нет дропов в сокете |
| **P5** | Backend: `/pnm/monitoring/sessions` JSON (TrueConf-совместимый), агрегаты, WS-события | curl возвращает JSON по 20 сессиям с `audio_rx/video_rx/rejected` |
| **P6** | Стенд: veth/netns/cgroup, Rust-бот SIP (audio+videо из ffmpeg-плейлиста), H.323-бот (pycrate), MOS-модель | 20 ботов живут 120 с, замер только MCU |
| **P7** | Матрица бенчмарков + отчёт | `BENCHMARK_REPORT.md` с `c_device` и `N_per_core` для A/B/C |
| **P8** | Финальная гигиена/доки/коммиты | всё воспроизводимо одной командой |

---

## 7. Открытые вопросы к агентам (каждый обязан ответить)

1. `ref-miner`: какие 7 конкретных механизмов эталонов дают максимальный выигрыш **на 36 ядрах E5-2699 v3 без AVX-512** и что из них уже есть у нас?
2. `proto-chain`: сколько из 20 SIP/H.323 устройств дозвонятся **сейчас** и какие 5 дыр закрыть первыми?
3. `media-core`: где именно горит CPU (функция/строка) и какой architecture-патч даёт ≥30 %?
4. `net-resilience`: какие 5 механизмов дают устойчивость при 2 % loss и как их измерить?
5. `regress-hygiene`: список конкретных дублей/конфликтов/регрессий с путями и что удалять.
6. `perf-lab`: минимальный harness, который даёт честные цифры за 1 день, и точная формула.

---

## 8. Риски
- **Диск**: 60 GB free; LTO-сборка + боты + дампы могут упереться. План: `target/debug` → 0, дампы в `/tmp` (tmpfs 29G).
- **Изоляция на KVM**: 36 vCPU — это vCPU хоста; соседи по гипервизору могут влиять. Фиксируем «внутри ВМ», в отчёте — оговорка.
- **Боты = наш же код**: риск, что бот меряет не MCU, а себя. Изоляция cgroup + замер по PID MCU решает.
- **H.323-стек Python** может стать bottleneck на 20 вызовах — тогда боты в отдельном cgroup и `pycrate` PER-passthrough.
- **Регрессии**: любые правки только под тестами; после каждой фазы — полный `cargo test`/`clippy`/`fmt`.
