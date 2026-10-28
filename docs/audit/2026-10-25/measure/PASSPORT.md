# Паспорт бинаря (заморозка перед замерами)

- UTC: 2026-10-25T14:21:20Z / local: 2026-10-25T17:21:20+03:00
- git_commit: 22f04177c75905141f92762f5143aad797765652 (master)
- git_dirty: 3 файлов (docs/audit + .cargo/config.toml — ожидаемо)
- binary: target/release/vks-server
- sha256: 8ab81afd70a7f168aa3c4e105f110def59a4d4270b8cfc0b6a749ee522868007
- size: 26650832 B
- rustc: rustc 1.94.0 (4a4ef493e 2026-03-02) binary: rustc commit-hash: 4a4ef493e3a1488c6e321570238084b38948f6db commit-date: 2026-03-02 host: x86_64-unknown-linux-gnu release: 1.94.0 LLVM version: 21.1.8 
- cargo: cargo 1.94.0 (85eff7c80 2026-01-15)
- Cargo.lock sha256: 57499a472a2f30e711589a01a0872728bb7c4d836b1065e917303c044f6521b3
- build flags (источник: .cargo/config.toml): [target.x86_64-unknown-linux-gnu] rustflags = ["-C", "target-cpu=native"]
- env RUSTFLAGS при сборке: НЕ использовался (config-only, проверено no-op rebuild 0.66s)
- profile: release opt-level=3 lto=thin codegen-units=1 strip=symbols panic=unwind
- service unit: /home/pro/.config/systemd/user/vks-mcu.service sha256=e94161da719193a9b3965dcbbec33aaffd51455ba4f536b836ea450af99ee958
- config: configs/default.toml sha256=b441d0dc84e1e8fffc43f284fa66b615ee86b836ba788805553ce934b8c8351e
- cpu: Intel(R) Xeon(R) CPU E5-2699 v3 @ 2.30GHz, cores=36, kernel=7.0.0-38-generic
- frozen copy: docs/audit/2026-10-25/measure/vks-server.frozen

## Итерация 2 (O3/O1/layout-fix + P0-1/P0-2/P0-3), 2026-10-25 17:44 MSK

| Параметр | Значение |
|---|---|
| sha256 | `f6446b81e7d2fd88526717d2ff844d3ff44f7d62813e45f4bd26d9dd291e0b39` |
| размер | 26 735 536 B |
| rustc/cargo | 1.94.0 (`/home/pro/.cargo/bin`) |
| профиль | release: opt-level=3, lto=thin, codegen-units=1, strip=symbols, panic=unwind; rustflags `-C target-cpu=native` |
| git HEAD | `22f0417` + 18 изменённых файлов (не коммичено) |
| копия | `measure/vks-server.iter2` |
| сервис | PID 1611566, `ActiveEnterTimestamp 17:44:46`, `/proc/<pid>/exe` md5 == диск |
| что вошло | media-core: O3 resize (строчные срезы/Брезенхем), layout-fix (`seat_video_or_expand`, 20/13), O1 (холст=профиль, `compose_sized`); proto-chain: P0-1 H.264 gate relax + level clamp (`42c0XX`, Annex A), P0-2 RTCP RX/AVPF + PLI/FIR→`keyframe_requests`, P0-3 DTMF full-duplex RFC 4733 |
| live-проверка | 42c01f→принято (720p); 42c01e→принято, stream 640×360 (level clamp); 4d001f/64001f→принято, answer `42c01f`; аудио 51 pps, rx_video идёт |
| тесты | gateway-sip 146/0/1; media-mcu 109/0; vks-server 118+31/0; clippy чисто |
| известные ограничения | keyframe force-IDR отложен (P4b, keyint=30); O7 не начат; H.323 бот не готов |

## Итерация 3 (O7 ферма энкодеров) — ЭКСПЕРИМЕНТАЛЬНАЯ, в production НЕ принята

| Параметр | Значение |
|---|---|
| sha256 | `1d8950cfe78d296a2bc916826e463ae2d52a33dd79b0a9bf08bec133ce660ba9` |
| размер | 27 039 280 B, собрано 18:11:58, rustc 1.94.0, native |
| копия | `measure/vks-server.iter3` |
| тесты | `cargo test -p vks-media-mcu -p vks-server` = 271 passed, 0 failed (в target-impl) |
| live-результат | **ПРОВАЛ**: 1 бот 720p30 получает 11–23 пакета видео за 8–14 с (итерация 2: 513/8 с); энкодер фермы 6.5 % CPU вместо ~30 %; `encoder group started` есть, композит в группу почти не публикуется |
| вывод | unit/integration-тесты фермы не ловят live-путь; приёмка O7 — только живым прогоном. Production откатан на `f6446b81…` (итерация 2), проверено: видео 513 пакетов/8 с |

## Итерация 3b (O7 ферма + фикс клинча) — ПРИНЯТА как production

| Параметр | Значение |
|---|---|
| sha256 | `3353dcb2df1ee3a435fb62ce13ea2f70d1c1337f8c0e2b7b4f2ad329b3d7ebba` |
| размер | 27 039 280 B, собрано 18:19:29, rustc 1.94.0, native |
| копия | `measure/vks-server.iter3b` |
| тесты | 271 passed, 0 failed (120 mcu + 120 server lib + 31 integration), clippy exit 0 (в target-impl) |
| что вошло | ферма энкодеров (1 энкодер на room+профиль, refcount, групповой keyframe); фикс клинча актора (writer/reader разделены); `VKS_VIDEO_FARM=0` — аварийный откат на per-session путь; PLI PSFB-фикс proto-chain; H.323 videoFastUpdatePicture/userInputIndication |
| live-приёмка | 1 бот 720p30: `rx_video` = **513 пакетов / 31 326 B за 12 с** (итерация 2: 513/31 254), `rx_audio` = 608; `pgrep -c ffmpeg` = **2 (1 dec + 1 enc)**; CPU энкодера **21 %** (было 6.5 % при клинче); в логе нет `starving` |
| вывод | production; предыдущие итерации 1/2 сохранены как база сравнения, итерация 3 (`1d8950cf…`) — как зафиксированный провал live-приёмки |

## Итерация 4a (P4a: метрики фермы) — промежуточная, живая проверка пройдена

| Параметр | Значение |
|---|---|
| sha256 | `81d8e509e775fd077818dff55be3b84df126d7d6f8f3c314a8a637ee215bf050` |
| размер | 27 039 280+ B, собрано 18:39, rustc 1.94.0, native |
| копия | `measure/vks-server.iter4a` |
| тесты | 276 passed, 0 failed (121 mcu + 123 server lib + 23 network_integration), clippy exit 0 |
| live-проверка | `/metrics`: `encoder_group_active=1`, `frames=115`, `keyframes=4`, `viewers=1`, `viewer_rtp_packets=247`, `viewer_rtp_bytes=15832`, `refcount=2`; `/api/v3/mcu/farm` отдаёт `groups[0]` (bitrate_kbps=2048, level=3.1, baseline) и `totals`; бот: `rx_video` 429 пакетов / 27 155 B, `rx_audio` 509 за 10 с |
| статус | промежуточная: ждём P1-битрейт группы → сборка «Итерация 4» (финальная для T2) |

## Итерация 4 (P4a метрики + P1 битрейт группы) — ЗАМОРОЖЕНА (с оговоркой по P1)

| Параметр | Значение |
|---|---|
| sha256 | `a7177bbc4e3e03e17715ddc8323d010f4e0785358e853758162652d976cb142c` |
| собрано | 18:55, rustc 1.94.0, native, тесты 280 passed / 0 failed, clippy exit 0 |
| копия | `measure/vks-server.iter4` |
| live-приёмка P4a | `/metrics` → `encoder_group_active=1`, `frames`, `keyframes`, `viewers=3`, `viewer_rtp_packets`; `/api/v3/mcu/farm` → JSON `groups[].bitrate_kbps/fps/profile/level/viewer_rtp` + `totals` ✓ |
| live-приёмка P1 | **НЕ ПРОШЛА (диагностика)**: при 3 зрителях `bitrate_kbps` остался 2048 вместо 2088 — `McuEngine::visible_windows()` читает `self.frames` (FrameSource, заполняется только в тестах через `set_frame`), а production-кадры лежат в другом хранилище → `visible` всегда 0 → кламп всегда base. Функциональной регрессии нет (битрейт = base), видео у всех 3 зрителей 510–512 пакетов |
| статус | T2 (netem) идёт на этом артефакте — P1 инертен, битрейт постоянный base (консервативно); фикс P1 → итерация 5 |

## Итерация 6 (фикс гонки acquire → утечка энкодеров) — ЗАМОРОЖЕНА, приёмка пройдена

| Параметр | Значение |
|---|---|
| sha256 | `95cc78fc63e5d40918eba7aa7354757cd4608830bffe526802e3c9f016be82ab` |
| собрано | 19:42, rustc 1.94.0, native, тесты 288 passed / 0 failed, clippy exit 0 |
| копия | `measure/vks-server.iter6` |
| корень утечки | гонка в `EncoderFarm::acquire` (check→spawn→insert без блокировки): при re-key несколько render-тасков одновременно спавнили FFmpeg, второй `insert` затирал первый; осиротевший `Arc<Group>` не попадал в мапу → retire его не видел → child жил вечно, `wait()` не вызывался (зомби при ручном kill) |
| фикс | `creation: Mutex` (сериализация медленного пути, вставка в мапу до спавна актора), watchdog актора по `Weak<FarmInner>`+`ptr_eq` (каждые 20 мс; нет в мапе → выход + kill child), `retire`/`invalidate_room` kill(-pgid)+повтор, `kill_child` с `wait()`, `kill_on_drop`, Drop-backstop |
| live-приёмка | **5 циклов × 3 SIP-бота 720p30**: в цикле `ffmpeg=4` (1 enc + 3 dec), `-b:v=2129k`, одна группа `(2129,3)`; после каждого цикла `ffmpeg=0`, детей MCU 1 (H.323 sidecar), **зомби 0**, `groups=0`; видео у всех ~400–420 пакетов/10 с; 0 `starving`/`did not stop` |
| статус | T2 (netem) идёт на этом артефакте |

## Итерация 7 (ingest reorder-буфер: джиттер bot→MCU) — ЗАМОРОЖЕНА, регрессия T2-RTT30 в проверке

| Параметр | Значение |
|---|---|
| sha256 | `ef37731f8a6c569a2552f81d0c149697a644df4c7c58456f336dd118b9bfd2fd` |
| собрано | 20:37, rustc 1.94.0, native |
| копия | `measure/vks-server.iter7`, тесты 131 server lib + suite (media-core: +4 новых incl real-ffmpeg e2e с reorder-прокси) |
| корень | три слоя приёма RTP: (1) `RtpSource::accept` отбрасывал «старые» seq как реплей (для видео джиттер = легальный reorder); (2) FU-A депакетизатор требовал seq==last+1 → Err+clear → SPS/PPS/IDR терялись; (3) pending_access чистился при смене timestamp → AU уничтожался до маркера |
| фикс | `VideoReorderBuffer` (окно 32 пакета / 30 мс, выдача по seq, дубликаты/дыры/таймаут), `RtpSource::accept_reordered()` только для видео-сессии, аудио-семантика не изменена |
| моя smoke-проверка | netem RTT30±10 (delay 15ms 5ms distribution normal на оба veth) → видео идёт (probe 641 пакет / 38 988 B за 15 с = обычный профиль пробника), `ffmpeg=0`, `zombies=0` после; qdisc снят |
| регрессия | T2-RTT30 (perf-lab): критерий видео ~18–19 тыс. пакетов/бот, c_device ~38 %, jb_late=0, orphan=0 |

**Регрессия T2-RTT30 на итерации 7 — ПРОЙДЕНА (приёмка):** видео **18 238 пакетов/бот** (304 pps) против 2 560 на iter6, loss 0.0055 %, jitter 2.60 мс; `c_device` **38.93 %** против 6.96 %; MOS 4.364, аудио loss 0; `orphan_ffmpeg_max=0`, `ffmpeg_count_max=11`, postcheck clean (`ffmpeg=0, zombies=0, children=1`). Остаточный дефект вне ingest: под видео+netem аудио-`jb_late`=5 238 (аудио-онли прогон — 0), т.е. паузы эгресса аудио при обработке видео (~1/с, совпадает с IDR) — тикет media-core. H.323-видео смоук: 2/2 connected, `has_video=true`, 560 пкт/13 с, `video_unavailable` нет.
