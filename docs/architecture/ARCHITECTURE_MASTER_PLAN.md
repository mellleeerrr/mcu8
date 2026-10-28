# Архитектурный мастер-план VKS/PROSTOR MCU

Версия: 1.0 (2026-10-25). Основание: аудит `docs/audit/2026-10-25/*`
(6 агентов, 4 раунда контраргументов), живые замеры на замороженном бинаре
`measure/PASSPORT.md`, эталоны `~/Документы/MCU` (Cisco CMS 5.6 ГБ,
TrueConf-реконструкция 373 МБ, Vinteo 46 ГБ).

Цель: **рабочий, полностью оптимизированный MCU** — смешанная конференция
(SIP + H.323 + WebRTC) с общим композитом, максимальная ёмкость на подписчика,
устойчивость к потерям/джиттеру, полная связка с бэкендом (PNM/`/api/v3`).

---

## 1. Исходное состояние (измерено, не оценено)

Стенд: 2×Xeon E5-2699 v3 @2.3 ГГц, **36 физ. ядер**, SMT off, 56 ГБ, 1 NUMA,
L3 32 МиБ, Ubuntu 26.04.1, kernel 7.0.0-38, KVM-гость, NIC `ens18`.

| Метрика | Итерация 1 | Итерация 2 (O3+O1+layout-fix) | Источник |
|---|---|---|---|
| Idle MCU | 0.02 ядра | 0.004 ядра | cgroup `vks-mcu.service` |
| 1 SIP-сессия 720p30 | 1.16 ядра (116 %) | **0.64 ядра (64 %)** | `BENCHMARK_REPORT.md` §7 |
| 10 сессий | 1.41 ядра/устр. | **0.81 ядра/устр.** | там же |
| 20 сессий | невалидно (13 окон, 7–8 вне композита) | **0.95 ядра/устр., все 20 в композите, разброс 1.002×** | там же |
| Ёмкость (36 ядер, 720p30) | 18–24 устройства (A/B/C) | **27–32 устройства** (26/32/37.7 при 70/85/100 %) | там же |
| Ёмкость после O7 (измерено) | — | **~2.8 устр./ядро на 720p30 (100 на 36 ядер), лимит теперь порты, не CPU** | `BENCHMARK_REPORT.md` §10 |
| Не-ffmpeg часть на устройство | 73.5 % ядра | **18.8 % ядра (×3.9)** | perf-lab, N=1 |
| RTP-порты | 100 пар (200 портов) = 50 видеозвонков / 100 аудио, hardcoded | `rtp_bridge.rs:22` |
| Раскладка | теряет участников сверх числа окон (13) | лог `engine.rs:605` |
| H.265 egress | 5.84 ядра/поток 1080p30 — непригоден | proto-chain §8.3 |
| Раскладка 20 участников | **исправлено** (`regrid_for_video`, авто-расширение сетки) | engine.rs, итерация 2 |
| H.264 interop | Baseline-only + level≥3.1; High/Main → `m=video 0` | live-калибровка |

**Главный вывод:** «20 устройств на одно физическое ядро» на программном
H.264-декодере недостижимы (один декодер 720p = 0.12–0.34 ядра). Реалистичная
цель — 2.5–5 устройств/ядро на 720p30, 1.7–3 на 1080p30; выше — только
аппаратный кодек (VAAPI/NVENC) или отказ от транскодинга (SFU-only).

---

## 2. Целевая архитектура: 8 слоёв

### L0. Хост и изоляция
- systemd user-slice: MCU `AllowedCPUs=0-15`, боты `bots.slice` `16-35`;
  `CPUWeight`, `MemoryMax`. Учёт — `cpu.stat`/`cpu.pressure` контура.
- sysctl-профиль (P4a, `deploy/sysctl/90-vks-mcu.conf`):
  `net.core.rmem_max=16 MiB`, `rmem_default=4 MiB`, `wmem_max=16 MiB`,
  `netdev_max_backlog=16384`, `net.core.netdev_budget=600`,
  `net.ipv4.udp_rmem_min=256 KiB`, `tcp_rmem/wmem` под TLS-сигналинг,
  `fs.file-max`, `vm.swappiness=10`.
- Сеть стенда: `veth`-пара `10.200.0.1 (MCU) ↔ 10.200.0.2 (bots)` в netns,
  `tc netem` только на стороне ботов; MCU — в отдельном netns без qdisc.

### L1. Транспорт и сигнализация
- **SIP**: UAS/UAC, UDP + **TCP/TLS** (P0), digest/REGISTER (P0),
  100rel/`Require` (P0: не 420, а поддержка или корректный 488),
  re-INVITE (P0: не 501), hold/transfer (P1).
- **H.323**: pycrate-сайдкар (Python 3.14 venv, `pycrate 0.8.1`), Q.931 1720,
  H.225 CS + OLC, H.245 MSD/TCS/OLC (в т.ч. reverse), fastStart/tunneling,
  `miscellaneousCommand`↔PLI, UserInputIndication. RAS/GK — P2.
- **WebRTC**: str0m (ICE-lite, DTLS-SRTP, TWCC), сейчас `MAX_PEERS_PER_ROOM=2`;
  для MCU-комнаты — новый `webrtc_mcu.rs` (транспорт str0m + McuEngine).
- **RTP/RTCP**: полноценный приём RTCP (RR/SR/PLI/FIR/NACK), AVPF,
  расширение seq по RFC 3550 A.1, TWCC на WebRTC.

### L2. Кодеки
- Аудио: PCMU/PCMA/G.722/G.726/G.728/G.723 (in-process oxideav), Opus
  (rusty-opus, PLC), AAC-LATM (FFmpeg-мост).
- Видео: H.264 CBP **+ High/Main на приём** (наш TX — CBP 3.1),
  H.265 — **только ingress** (egress H.264), VP8 — только WebRTC-плечо.
- FFmpeg 8.0.1 субпроцессы `-threads 1 -filter_threads 1`; целевое —
  пул воркеров с общим декодером/энкодером на профиль (O7), затем
  рассмотреть in-process (oxideav) для H.264 CBP.

### L3. Медиаядро (главный объект оптимизации)
- Композитор: холст **= профиль выхода** (O1), а не всегда 1920×1080;
  аллокации — из пула (M7 эталона); `scale_row` — строчные срезы + Bresenham
  (O3: 8.0 → 2.0–2.8 мс/кадр).
- **Encoder farm (O7)**: один энкодер на `(room, profile|layout-hash)`,
  refcount; per-viewer только RTP-пакетизация (свой SSRC/seq/ts), PLI →
  групповой keyframe. Снимает 19 из 20 энкодеров в комнате 20 участников.
- Аудио: вынести guard `tick_audio_at` наружу (AtomicU64 на комнату),
  убрать O(N²) копий до guard; SIMD-миксер (AVX2), `mix_minus` через
  префиксные суммы (по образцу эталона).
- Раскладки: авто-геометрия по числу участников + **пейджинг страниц по 25**
  (эталоны: Cisco/TrueConf), `LayoutDescriptor` с `resolution` (новый ADR).
- Egress-группы: единый композит → N подписчиков; NACK/RTX-кэш шарится по
  `(room, ssrc, seq)`; FEC 10:1 (XOR) — P4b.

### L4. Управление и API
- `/api/v3` — канон (167 роутов); `/api/v1` — deprecation + `410` через
  2 недели (0 потребителей, 3 запроса/5 ч против 36 878).
- PNM-фронтенд: `GET /ws` + `proto_version`, `/api/v3/*`; мониторинг
  `/pnm/monitoring/*` — SPA, данные через `/api/v3/sessions`.
- Конфиг: единый канон `configs/default.toml` + strict-валидация
  (`--check-config`, exit 78), убрать расхождение 9/16 дефолтов.

### L5. Хранилище и запись
- SQLite WAL (сейчас) → PostgreSQL (кластер); нормализация схемы (T10b).
- `vks-recorder`: модель битрейта `W·H·fps·bpp`, клампы kbps [64…8192],
  fps [15…60] (из эталона).

### L6. Кластер и наблюдаемость
- `vks-cluster` in-memory bus → NATS JetStream (ADR-0034) при >1 узла.
- Метрики Prometheus: `mcu_*` (сессии, участники, ffmpeg_count/ticks,
  composite_ms, resize_ms, rtp_*), `cpu.stat`-экспортёр контура,
  `cpu.pressure`. Трейсинг — OTLP.

### L7. Безопасность
- SRTP (SDES на SIP-плече; DTLS-SRTP на WebRTC), H.235 (H.323), TLS для SIP.
- JWT (jsonwebtoken 9.3.1), argon2-хеши, ротация секретов; dev-секреты
  запрещены в prod (`allow_insecure_defaults=false`).

---

## 3. Точные версии стека

Из `Cargo.lock` (434 пакета), rustc 1.94.0 (LLVM 21.1.8):

| Слой | Пакет | Версия |
|---|---|---|
| Async/RT | tokio | 1.53.3 |
| HTTP | axum / hyper / tower / tower-http | 0.8.9 / 1.12.0 / 0.5.3 / 0.6.11 |
| WebRTC | str0m | 0.24.1 |
| Protobuf | prost | 0.13.5 |
| WS | tokio-tungstenite | 0.29.0 |
| Crypto | ring / rcgen / sha2 / base64 | 0.17.14 / 0.14.10 / 0.10.9 / 0.22.1 |
| БД | sqlx | 0.8.6 |
| Сериализация | serde / serde_json | 1.0.229 / 1.0.x |
| ID | uuid | 1.28.0 |
| JWT | jsonwebtoken | 9.3.1 |
| Метрики | prometheus | 0.14.0 |
| Конфиг | figment | 0.10.19 |
| Синхронизация | parking_lot | 0.12.5 |
| Сокеты | socket2 | 0.6.5 |
| Ошибки | thiserror / anyhow | 2.0.21 / 1.0.104 |
| Libc | libc | 0.2.190 |
| Аудио | rusty-opus / oxideav-* | из lock (G.711/G.722/G.726/G.728/G.7231/ADPCM) |
| Сборка | cargo | release: opt-level=3, lto=thin, codegen-units=1, strip, panic=unwind; rustflags `-C target-cpu=native` (`.cargo/config.toml`) |

Системный стек: FFmpeg 8.0.1, GStreamer 1.28.2, Python 3.14.4 + pycrate 0.8.1
(venv `~/.local/share/vks-mcu/h323-venv`), Node 24.21.0, Playwright 1.64.1,
sqlite 3.x, systemd 258+ (cgroup v2).

**Решение по сборке:** стенд — `target-cpu=native` (Haswell, AVX2, без
AVX-512) и замороженный sha256 в паспорте; CI/прод — `x86-64-v3` для
воспроизводимости; бинарь с `native` не переносить на другой CPU (R-26,
`03_media_engine.md` Приложение D).

Порты: SIP 5060 (UDP/TCP), SIP-TLS 5061, H.323 1720, HTTP 8080, WebRTC 40000,
ICE 42000–65000, RTP 20000–39999 (после P0-патча; сейчас 41000–41200).

---

## 4. Фазы (порядок и критерии приёмки)

### P0 — функциональность дозвона (блокирует «полноценный MCU»)
1. H.264 gate relax: принимать Baseline/Main/High на приём, answer — наш CBP
   3.1 + `level-asymmetry-allowed`; `-level:v` из оффера при level<3.1.
   **Приёмка:** 42c01e/42e01e/4d001f/64001f → видео принято (сейчас только 42c01f).
2. RTCP: приём RR/SR/PLI/FIR, `a=rtcp-fb` в answer, PLI → keyframe.
3. DTMF full-duplex RFC 4733 + H.323 UserInputIndication.
4. SRTP (SDES) + TLS + digest/REGISTER; мост «шифрованный терминал ↔
   plaintext-движок» на границе RTP-сокета (не в движке).
5. Раскладка: авто-геометрия и пейджинг 25 — **блокер 20+ устройств**
   (сегодня 7–8 из 20 не попадают в композит).
6. RTP-пул из env `VKS_RTP_PORT_MIN/MAX`, дефолт 20000–39999.

### P1 — гигиена сборки и конфига
R-26 закрыт (флаги в `.cargo/config.toml`, паспорт); strict-config
(`--check-config`, 14 не-секретных ключей, exit 78); `/api/v1` deprecation;
6 ADR supersede; CI: собрать тем же тулчейном, что стенд (или `x86-64-v3`).

### P2 — медиаэффективность (главный прирост ёмкости)
O3 resize (строчные срезы) → O1 холст=профиль → O7 encoder farm → SIMD-миксер
→ пул аллокаций. **Приёмка:** 1.40 → ≤0.6 ядра/устройство на тех же прогонах,
`cargo test`/`clippy` зелёные, «до/после» на одном codegen-baseline.

### P3 — сеть и устойчивость
P4a: JB адаптивный `T=clamp(jitter+10,20,60)`, reorder-buffer, счётчики
wire/kernel/app, SO_RCVBUF/sysctl, `recvmmsg/sendmmsg` — **до** замера MOS.
P4b: NACK/RTX-кэш под egress-группы, FEC 10:1, PLC для G.711/G.722, pacing.

### P4 — измерительный стенд и матрица
Изоляция (slices, veth/netns, netem), SIP-боты (есть прототип), H.323-бот
(`h323_asn.py`), WebRTC-headless. Матрица: 20 SIP / 20 H.323 / 10+10 / 20+20+20
смешанная, 120 с, netem-профили (0/2 % loss, 30/100 мс jitter), MOS ≥ 4.0,
устройств/ядро A/B/C. **Приёмка:** отчёт с `c_device` и разложением.

### P5 — смешанная комната (отдельный ADR, 5–8 дней)
`webrtc_mcu.rs`: браузеры через str0m-транспорт в McuEngine; политика
«комната со шлюзом = MCU». Приёмка: браузер + SIP + H.323 в одной комнате
видят общий композит.

### P6 — H.323/H.235/H.239 и эталонные механизмы
OLC reverse, fastStart/tunneling, H.239 (BFCP↔H.239), H.235 baseline,
media_policy `resolve()` wiring, ресурсная модель, пейджинг/фокус из эталонов.

### P7 — кластер/хранилище/наблюдаемость
Postgres, NATS, OTLP, PNM-интеграция мониторинга, запись с моделью битрейта.

### P8 — приёмка и документация
Полный прогон матрицы, паспорт, ADR-набор, `MASTER_TASK_LIST` обновлён.

---

## 5. Риски

| Риск | Влияние | Смягчение |
|---|---|---|
| `native`-бинарь не переносим | SIGILL на другом CPU | CI/прод `x86-64-v3`; паспорт флагов |
| H.265 egress 5.84 ядра | ёмкость | только ingress, egress H.264 |
| Смешанная комната = новый модуль | сроки | отдельный ADR, не блокирует P0–P4 |
| pycrate/Python 3.14 в venv | H.323 не стартует | проверить venv, lock-файл, fallback Python 3.12 |
| Раскладка/пейджинг | 20+ участников невидимы | P0-5, авто-геометрия |
| Нет JB/RTCP | MOS недостоверен | P4a до матрицы |
| Нет аппаратного кодека | потолок 2.5–5/ядро | честно зафиксировать в ТЗ; VAAPI — исследование |

---

## 6. Ключевые ADR (нужно оформить)

1. Единый медиапуть MCU: str0m как транспорт, McuEngine как микшер (P5).
2. `resolution` в `LayoutDescriptor`/`Layout` (O1, ломает контракт vks-proto).
3. Encoder farm: ключ группы, refcount, PLI-групповой keyframe (O7).
4. SRTP-граница: шифрование на плече, plaintext в движке (P0-4).
5. H.265 — только ingress (кодек-политика).
6. Порт-диапазоны RTP/ICE и их конфигурируемость.
7. `/api/v3` — канон, `/api/v1` deprecated.
8. Замена in-memory cluster bus на NATS (ADR-0034).
9. H.235 baseline: минимальный subset аутентификации.
10. Пейджинг раскладок (25 на страницу) вместо роста сетки.

---

## 7. Что уже сделано в этом цикле

- Заморожен воспроизводимый артефакт (R-26): флаги в `.cargo/config.toml`,
  паспорт sha256/rustc/lock, сервис перезапущен на нём.
- Живые замеры ёмкости + разложение по компонентам + дефект раскладки.
- 6 артефактов аудита, 4 раунда контраргументов, консенсус по приоритетам.
- Прототип SIP-бота с видео (SIP+H.264+RTP+CPU-разложение).
- Назначены исполнители: media-core (O3→O1→O7), proto-chain (P0-1…3),
  perf-lab (боты/матрица), ref-miner (спека трансплантации).
