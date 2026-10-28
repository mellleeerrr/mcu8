# 04. Сетевой стек и устойчивость к потерям: RTP/RTCP, сокеты, sysctl, netem

**Агент:** `net-resilience` (durable teammate, Lead `lead`) · **Дата:** 2026-10-25 · **Задача:** `task-4`
**Стенд:** ВМ `192.168.1.93`, Linux `7.0.0-38-generic`, 36 vCPU, 57.8 GiB RAM, `ens18` = `virtio_net`, 1 RX/TX queue.
**Live-процесс (не трогался):** PID `1177177`, `/home/pro/mcu/target/release/vks-server --config /home/pro/mcu/configs/default.toml`,
38 потоков (36 `tokio-rt-worker` + main + sqlx), cgroup `vks-mcu.service`, `VKS_WEBRTC_TRANSPORT=1`, `VKS_WEBRTC_PORT_MIN/MAX=43000/43100`, `VKS_SIP_UAS_ENABLED=1`, `VKS_H323_UAS_ENABLED=1`.
**Ограничения работы:** `sysctl`/`tc` не менялись (только чтение), live-сервер не перезапускался, `cargo build/test` не запускались. Правки — только этот файл.

---

## 0. Позиция и уверенность

**Позиция (в защищаемой формулировке).**
Сетевой путь нашего MCU не рассчитан на транспорт с потерями и джиттером, и это ограничитель №1 для «20 устройств сейчас / 500 абонентов потом». Конкретно:

1. **Медиа-сокет `0.0.0.0:40000` работает с дефолтным буфером 208 КиБ** (`ss -m`: `rb212992`) — приложение не вызывает `setsockopt(SO_RCVBUF)` нигде (`grep SO_RCVBUF|set_recv_buffer|socket2` по `crates/` → 0 совпадений), а `net.core.rmem_max=4 MiB` лишь разрешает больший буфер, но не задаёт его. 208 КиБ ≈ 150 пакетов по 1400 Б, т.е. **≈0.9 мс** трафика fan-out 20×19 (расчёт в §1.3).
2. **Приём — один `recv_from` на пакет в одной tokio-задаче** (`udp_loop.rs:160`), отправка — `send_to` в том же цикле последовательно **на каждого подписчика** (`udp_loop.rs:210-215`). Ни `recvmmsg`, ни `sendmmsg`, ни `SO_REUSEPORT`, ни N воркеров на порт.
3. **RTCP на legacy-пути не обрабатывается вообще:** входящий RTCP распознаётся и выбрасывается (`udp_loop.rs:189-197`), NACK/FIR/TWCC/REMB не парсятся; на SIP/H.323-плечах RTCP-сокет **только пишется** (`grep 'rtcp_socket.recv'` → 0 совпадений), входящие RR/XR/NACK не читаются. Отправляется один compound SR+SDES раз в 5 с (`gateway_audio.rs:434-439`), на видео — SR+PLI раз в 1 с (`gateway_video.rs:669-680`).
4. **Jitter buffer как механизм отсутствует:** SIP/H.323 аудио принимает пакет только если он новее последнего — любой переставленный пакет отбрасывается (`gateway_audio.rs:587-592`: `distance == 0 || distance >= 0x8000 → None`), а пропуск «заделывается» `conceal()`. Видео — вообще без буфера переупорядочивания: разрыв последовательности сбрасывает `fragment` (`gateway_video.rs:938-943`), т.е. теряется весь access unit.
5. **PLC есть только для Opus** (`gateway_audio.rs:103-117`: in-band FEC `decode_fec` + до 3 PLC-кадров; `opus.rs:43-49,79-90`). Для G.711/G.722/G.726/G.728/G.723.1 `conceal()` возвращает пусто → в микс уходят нули.
6. **FEC нет нигде** (ни ULPFEC RFC 5109, ни flexfec RFC 8627, ни XOR). NACK+RTX есть **только** на opt-in str0m-плече для браузеров, и оно ограничено **2 участниками на комнату** (`webrtc_room.rs:33 MAX_PEERS_PER_ROOM = 2`).
7. **sysctl дефолтные под ВКС:** `rmem_max=wmem_max=4 MiB`, `netdev_max_backlog=1000`, `netdev_budget=300`, `udp_rmem_min=4096`. Для сравнения, референс Vinteo ставит `rmem_max=wmem_max=5838860800` (5.4 ГБ), `rmem_default=502400000`, `udp_mem='2838860800 2258291200 4677721600'` (`vinteo/08_отчёты_анализа/11_Реестр_конфигурации_ВКС.md:859-873`).
8. **Портового запаса нет:** ICE-диапазон `43000-43100` = **101 порт** на весь сервер (`deploy/systemd/vks-mcu.service`), RTP-пул шлюзов `41000-41200` = **100 пар** (`rtp_bridge.rs:22`). 500 абонентов физически некуда садить.

**Уверенность по пунктам:** п.1-3, 5-8 — **высокая** (прямые `file:line` + значения команд, проверено на живом процессе). П.4 — **высокая** для аудио/видео SIP-плеча; для браузерного плеча — **средняя** (там reorder-окно str0m 200 мс, `webrtc.rs:35,51`, но это фиксированное окно, не адаптивный JB: str0m 0.24.1 в своей матрице фич ставит `Adaptive Jitter Buffer :x:`, `Fixed Depacketize Buffer :white_check_mark:` — `~/.cargo/registry/src/*/str0m-0.24.1/src/lib.rs:574-577`).

**Уточнённая формулировка позиции (после собственного расчёта — числа проверил, §4.4).**
Сырая формулировка «при 2 % потерь MOS≥4.0 недостижим» требует одного уточнения по единицам, но в связке **«2 % потерь + джиттер 30 мс»** она **подтверждается количественно**:

- Чистые равномерные 2 % для G.711 (`Ie=0, Bpl=25.1`, `d=100 мс`): `Ie_eff = 95·2/(2+25.1) = 7.01`, `R = 93.2 − 2.4 − 7.01 = 83.8`, **MOS = 4.16** — выше 4.0 (порог MOS 4.0 наступает при **3.43 %** потерь).
- **Но** джиттер 30 мс без jitter buffer конвертируется в потери: при `delay 30ms 15ms` (разброс 15-45 мс) и 20-мс пакетизации `P(d_n − d_{n+1} > 20) ≈ 5.6 %` (§4.3). Наш приёмник **выбрасывает** каждый переставленный пакет (`gateway_audio.rs:587-592`), поэтому эффективные потери = `2 % + ~5.6 % = 7.6 %` → `Ie_eff = 22.1`, `R = 68.7`, **MOS = 3.54** — **ниже 4.0**.
- Для Opus (`Ie=2, Bpl=20`) ещё жёстче: чистые 2 % дают **4.04** (на самом пороге, порог = 2.26 %), а 7.6 % → **MOS = 3.26**. Причина: `Bpl=20` у Opus меньше, чем 25.1 у G.711, т.е. Opus *менее* устойчив к потерям по E-model, а не более.
- Вывод: **позиция верна в форме «2 % потерь ∧ джиттер 30 мс ∧ отсутствие JB»**. Убирать из неё можно только «2 %» (без джиттера) — и тогда она верна для видео, bursty-профиля и Opus-на-границе, но не для G.711.

Позиция держится в трёх измеримых формах:
- **(а) Видео.** При 2 % потерь пакетов и без NACK/FEC каждый потерянный пакет убивает весь access unit (наш depacketizer именно так и делает, `gateway_video.rs:938-943`), а восстановление — только по безусловному PLI раз в 1 с (`gateway_video.rs:669-680`). Для 720p/30fps ≈ 240 pps и AU из ~8 пакетов вероятность «целого» кадра `0.98^8 = 85 %` → ~15 % кадров теряется, плюс до 1 с чёрного/замороженного экрана на каждый loss event. Это MOSv-катастрофа, а E-model её вообще не моделирует.
- **(б) Bursty-потери.** Реальные потери в LAN/Wi-Fi — не Bernoulli. При correlated loss (`netem loss 2% 25%`) серии ≥4 подряд идут с заметной вероятностью, а наш `conceal()` **жёстко ограничен 3 кадрами** (`gateway_audio.rs:112`, `for _ in 0..missing.min(3)`), дальше — нули в микс. E-model предполагает идеальный PLC (G.711 App. I), у нас для G.711 его нет. Измерять надо PESQ/POLQA (§4.4), а не E-model.
- **(в) Джиттер 30 мс.** См. выше: +5.6 % дропов → MOS 3.54 (G.711) / 3.26 (Opus) при `d=100 мс`; при `d=150 мс` (`I_d=3.6`) — ещё на 0.05-0.1 ниже. Без адаптивного JB эти 5.6 % не возвращаются никак.

Итог: **network-first остаётся верным** — «без устойчивости к потерям 20 устройств не удержать» подтверждается количественно (MOS < 4.0 на профиле «2 % + 30 мс джиттера»), для видео и bursty-профиля — тем более, и для ёмкости (500 некуда садить по портам и буферам).

---

## 1. Текущий путь пакета

### 1.1. Два разных медиа-пути в одном процессе

| Путь | Кто на нём | Сокет | Код |
|---|---|---|---|
| **A. Legacy SFU-петля** | «сырые» endpoint'ы, которым SDP выдаётся вручную; PLI-триггеры из signalling | `0.0.0.0:40000` (`configs/default.toml: media_listen_addr`), создаётся `UdpMediaLoop::bind` (`udp_loop.rs:110`) | `main.rs:283-293` → `udp_loop.start_routing_loop(engine.route_udp_packet)` |
| **B. str0m WebRTC** | браузеры, `VKS_WEBRTC_TRANSPORT=1` | порт на плечо из `43000-43100` (`webrtc_runtime.rs:59-78`), своя tokio-задача на плечо | `webrtc_room.rs:456-540` (`run_peer`) |
| **C. SIP/H.323 RTP-мосты** | аппаратные терминалы (20 устройств стенда) | пара RTP/RTCP из пула `41000-41200` (`rtp_bridge.rs:16-23`, `gateways.rs:2453`) | `gateway_audio.rs:268` (аудио), `gateway_video.rs:687` (видео), `gateways.rs:2200` (SIP-сигналинг) |

Все три живут одновременно в одном процессе; медиа-нагрузка стенда идёт по **C** (H.323-терминалы) и частично по **B**, путь **A** — legacy.

### 1.2. Путь A: NIC → kernel → socket → recvfrom → канал → forwarder

```
NIC ens18 (virtio_net, 1 RX queue, ring 1024)
  └─ NAPI poll, budget 300/2000us (net.core.netdev_budget=300)
     └─ softnet backlog (netdev_max_backlog=1000)         ← точка потерь №1 (drop col в /proc/net/softnet_stat)
        └─ UDP-слой, per-socket rcvbuf = 208 КиБ по умолчанию ← точка потерь №2 (Udp.RcvbufErrors)
           └─ tokio UdpSocket::recv_from()  [udp_loop.rs:160]  ← 1 пакет / 1 syscall / 1 задача
              ├─ classify_incoming()          [udp_loop.rs:68-97]
              │   ├─ STUN → построить ответ и send_to внутри цикла [udp_loop.rs:176-177]
              │   ├─ DTLS → packets_dropped += 1 (DTLS/SRTP не реализованы) [udp_loop.rs:185-188]
              │   ├─ RTCP → packets_rtcp += 1, пакет ВЫБРОШЕН [udp_loop.rs:189-197]
              │   └─ Unknown → drop [udp_loop.rs:198-201]
              ├─ Bytes::copy_from_slice(&buf[..size])   [udp_loop.rs:204]  ← копия №1 (buf — стековый [u8;2048], udp_loop.rs:152)
              ├─ router(src, packet) → SfuEngine::route_udp_packet  [engine.rs:229-340]
              │   ├─ lock addr_to_participant (Mutex<BTreeMap>)      [engine.rs:238]
              │   ├─ lock rooms, поиск room/ssrc/track               [engine.rs:242,265]
              │   ├─ get_endpoint → ЕЩЁ один lock rooms               [engine.rs:284,196-197]
              │   ├─ lock rooms (третий раз) и обход подписчиков      [engine.rs:319]
              │   │    └─ payload.clone() (refcount) ИЛИ payload.to_vec() при rewrite PT  [engine.rs:325-332] ← копия №2 (на каждого подписчика с другим PT)
              │   └─ Vec<(SocketAddr, Bytes)> на выход
              └─ for (dest, bytes) in targets { socket.send_to(...).await }  [udp_loop.rs:210-215]
                     ← последовательный await на КАЖДОГО подписчика (head-of-line на самом цикле), копий нет (Bytes::clone — refcount), системных вызовов = N_subscribers
```

Наблюдаемость: `UdpMediaStats` (8 `AtomicU64`, `udp_loop.rs:22-31`) инкрементится, но **нигде не экспортируется** в `/metrics` — live `/metrics` содержит только `vks_webrtc_*` (проверено `curl :8080/metrics`). `MEDIA_PACKETS_DROPPED` объявлен (`metrics.rs:48-51`), но в продакшн-коде не инкрементится ни разу (`grep nack_timeout` → только `metrics.rs:357-358`, это тест).

### 1.3. Где именно теряется при burst (с числами)

Расчёт для SFU full-mesh 20 участников (20 паблишеров аудио+видео, 19 получателей каждый):

| Поток | pps in | pps out (fan-out) |
|---|---:|---:|
| Аудио G.711 20 мс | 20×50 = 1000 | 20×19×50 = 19 000 |
| Видео 720p/30fps ≈ 240 pps | 20×240 = 4800 | 20×19×240 = 91 200 |
| **Итого** | **5800** | **110 200 pps** |

Ёмкость буферов при 1400 Б/пакет:
- `SO_RCVBUF=212992` → **≈152 пакета ≈ 1.4 мс** при 110 kpps. Любой провал планировщика ≥2 мс (конкуренция с FFmpeg-воркерами, видео-кейфрейм, работа tokio work-stealing) — и буфер переполнен.
- `netdev_max_backlog=1000` → **≈9 мс** burst-поглощения на весь хост, на **единственную** RX-очередь.
- RX ring 1024 (`ethtool -g ens18`) — то же порядка.
- TX ring **256** (`ethtool -g ens18`) и `qlen 1000` (`fq_codel limit 10240p`) — burst fan-out 91 kpps × ~2 мс видео-кадра = на порядок больше, чем ring.

Фактические счётчики сейчас (низкая нагрузка, стенд простаивает):
- `ip -s link show ens18`: RX packets 2 274 818, **RX dropped 2030** (0.089 % — исторические, не медиа), TX dropped 0, errors 0.
- `/proc/net/softnet_stat` (сумма по CPU): processed 17 287 714, **dropped 0**, time_squeeze 38.
- `/proc/net/snmp Udp`: `InErrors 456`, `RcvbufErrors 456` — **все 456 на сокете `:5353` (mDNS, `0x14E9`)**, медиа-сокет `:40000` имеет `drops=0` (`/proc/net/udp`).
- `ss -m` для `:40000` и `:5060`: `skmem:(r0,rb212992,t0,tb212992,...)` — **буфер ровно дефолтный**.

Вывод: потерь «прямо сейчас» нет только потому, что нет нагрузки. Все четыре точки потерь (softnet backlog, rcvbuf, single-thread recv, sequential send) — в готовом к отказу состоянии и ни одна не инструментирована.

### 1.4. Путь C: SIP/H.323 RTP-мост (аудио)

```
UDP :41000+2k (audio)   [rtp_bridge.rs:38-57, allocate_pair]
  └─ recv_from в tokio-select с 20-мс тикером   [gateway_audio.rs:268]
     ├─ RtpSource::accept: src-latch + PT + SSRC + «только новее» [gateway_audio.rs:520-600]
     │     └─ переставленный/дубль → None → rejected_packets  [gateway_audio.rs:587-592]
     ├─ gap = seq − last − 1 → conceal(missing, payload)        [gateway_audio.rs:301-310]
     │     └─ ТОЛЬКО Opus: decode_fec при missing==1, иначе ≤3 PLC-кадра [gateway_audio.rs:103-117]
     ├─ decode → VecDeque<f32> queue (лимит 5760 сэмплов = 120 мс)  [gateway_audio.rs:232-233,327]
     └─ тикер 20 мс: pop 960/160/320 сэмплов → MCU-микшер → encode → send_to [gateway_audio.rs:255,340-...]
        └─ MissedTickBehavior::Skip [gateway_audio.rs:255] → у ЭТОЙ сессии playout не сдвигается,
           очередь растёт → drop-oldest на лимите 120 мс (:232-233,307,327). Микс комнаты при этом
           не теряет 20 мс: тик общий и защищён guard'ом по now_ms (vks-media-mcu/src/engine.rs:264-268)
```
Видео (путь C, `gateway_video.rs`): `recv_from` → `RtpSource::accept` → `VideoDepacketizer::push`;
разрыв seq/timestamp внутри FU-A → `fragment.clear(); return Err(())` (`gateway_video.rs:938-943`) → **весь AU теряется**, кадр не доходит до декодера FFmpeg; восстановление — безусловный PLI+SR раз в 1 с (`gateway_video.rs:669-680`). Отправка закодированного AU — `for packet in packets { send_to().await }` (`gateway_video.rs:601-602`) без пейсинга: один кадр 1080p = ~60-100 пакетов подряд.

### 1.5. Путь B: str0m (для полноты; это «хороший» путь, но он не про 20 устройств)

`run_peer` (`webrtc_room.rs:456-540`): своя tokio-задача и свой UDP-сокет на плечо → `peer.handle_receive(Instant::now(), src, dst, buf)` → `str0m::Rtc::handle_input` → ICE/DTLS/SRTP/NACK/TWCC/pacer внутри str0m → `WebRtcAction::Send` → `socket.send_to` в `dispatch` (`webrtc_room.rs:579-...`).
Здесь реально есть: NACK (str0m `session.rs:47 NACK_MIN_INTERVAL=33ms`), RTX-ресенд (`error.rs:49 ResendRequiresRtxPt`), TWCC/REMB (`webrtc.rs:344-349`, `webrtc_room.rs:593-597` `set_desired_bitrate`), leaky-bucket pacer (str0m `pacer/leaky.rs`), SRTP (`str0m` DTLS).
Чего нет: **адаптивного JB** (fixed reorder window 200 мс, `webrtc.rs:35,51`), **FEC**, и — главное — **ёмкости**: `MAX_PEERS_PER_ROOM: usize = 2` (`webrtc_room.rs:33`), порт на плечо из 101-портового диапазона, очередь команд 64 (`webrtc_room.rs:28`) с дропом кадра и запросом keyframe при переполнении (`webrtc_room.rs:445-457`).

---

## 2. Механизмы устойчивости: как у нас, патч, метрика, RFC, риск

Легенда: **есть** — реализовано и включено; **частично** — есть на одном плече/в урезанном виде; **нет** — отсутствует.

### 2.1. `recvmmsg`/`sendmmsg` + батчинг + `SO_RCVBUF`

- **Как у нас:** **нет.** `udp_loop.rs:160` — `socket.recv_from(&mut buf)` по одному пакету; `udp_loop.rs:210-215` — `send_to` в `await`-цикле по подписчикам. `SO_RCVBUF` не выставляется нигде (`grep SO_RCVBUF|socket2|recv_buffer_size` → пусто), фактический буфер 208 КиБ (проверено `ss -m`). `buf = [0u8; 2048]` (`udp_loop.rs:152`) — при jumbo/больших пакетах обрезка.
- **Патч:** (1) на `bind()` выставить `SO_RCVBUF/SO_SNDBUF` (через `socket2`, до `UdpSocket::from_std`) — 4 МиБ для 20 устройств, 8-16 МиБ для 500; поднять `net.core.rmem_max` соответственно (см. §3); (2) заменить цикл на `AsyncFd<socket2::Socket>` + `recvmmsg` батчем 32-64 с одним общим `BytesMut` на батч — это убирает 1 syscall/пакет и позволяет нарезать батч на N воркеров; (3) на отправке — `sendmmsg` для fan-out: один и тот же payload уходит 19 адресатам одним вызовом; (4) буфер 2048 → 4096 и явный дроп oversized с метрикой.
- **Метрика проверки:** `vks_udp_recv_syscalls_per_pkt` (должно упасть с 1.0 до ~0.03), `vks_udp_rcvbuf_bytes`, `node_softnet_dropped_total`, `Udp.RcvbufErrors`; целевой pps/ядро ≥ 250 kpps на recv-воркер (на loopback легко проверяется).
- **RFC/стандарт:** RFC 3550 §5 (формат), RFC 8085 §3.1.6 (burst/pacing рекомендации). `recvmmsg/sendmmsg` — Linux `man 2`.
- **Риск:** `recvmmsg` с `MSG_DONTWAIT` требует ручного EAGAIN-цикла (ошибка = busy-loop); `Bytes` на батч ломает текущий контракт `RtpPacket{payload: Bytes}` (нужен `Bytes::slice`); при `sendmmsg` частичный успех (`mmsghdr.msg_len`) нужно обрабатывать per-message.

### 2.2. `SO_REUSEPORT` + N UDP-воркеров

- **Как у нас:** **нет.** Один `UdpSocket::bind` (`udp_loop.rs:110`) → одна tokio-задача (`main.rs:293`).
- **Патч:** N сокетов на тот же адрес с `SO_REUSEPORT` + `SO_INCOMING_CPU`/`SO_ATTACH_REUSEPORT_EBPF`; по умолчанию ядро хэширует 4-tuple, поэтому поток одного паблишера всегда попадает в один воркер — это сохраняет per-SSRC состояние без блокировок. N = min(8, nproc/4) для 20 устройств, 16-32 для 500. Дополнительно: `net.core.rps_sock_flow_entries=32768` + `rps_cpus` на единственной RX-очереди (сейчас `rps_cpus=0`, `rps_flow_cnt=0`, `rps_sock_flow_entries=0` — проверено).
- **Метрика:** распределение pps по воркерам (`std/mean` < 0.3), `time_squeeze` в `/proc/net/softnet_stat`, p99 задержки `recv→send`.
- **RFC:** нет (механизм ядра); совместимость с ICE — RFC 8445 (порт один, распределение внутри).
- **Риск:** RTCP для плеча должен попасть на тот же воркер (решается eBPF-хэшем по SSRC/адресу, иначе NACK/RR уедут «не туда»); при N>1 нельзя держать общий `Mutex<rooms>` в горячем пути (см. §5, п.2).

### 2.3. Adaptive jitter buffer (target delay, reordering, поздние пакеты)

- **Как у нас:** **нет для RTP-мостов; частично для браузеров.** SIP/H.323: приём только «новее последнего» (`gateway_audio.rs:587-592`) — переставленный пакет уничтожается; «буфер» — это `VecDeque<f32>` декодированных сэмплов с фиксированным лимитом 120 мс (`gateway_audio.rs:232-233`), дроп при переполнении `pop_front` (`gateway_audio.rs:307,327`). Видео — 0 буфера (`gateway_video.rs:938-943`). Браузеры — фиксированное reorder-окно 200 мс (`webrtc.rs:51`), не адаптивное (str0m: `Adaptive Jitter Buffer :x:`).
- **Патч:** per-track reorder-буфер (BTreeMap<seq, пакет>) с вычислением jitter по RFC 3550 A.8 и target delay = `clamp(4·J + 3·packetization, 20, 200)` мс; поздние пакеты (seq < head − target) — дропать с метрикой `late`, а не молча; для видео — буфер на 2-4 AU (не больше `MAX_NAL_BYTES`), чтобы FU-A переживал перестановку; для дублей — `duplicate` счётчик. Для браузеров ничего не менять (str0m), только вынести 200 мс в конфиг.
- **Метрика:** `vks_jb_target_ms`, `vks_jb_late_total`, `vks_jb_discarded_total`, p95 «packet→playout»; и главное — `MOS-LQ` по PESQ до/после (§4.4).
- **RFC:** RFC 3550 A.8 (jitter), RFC 3550 A.1 (seq-обработка), RFC 3611 §4.7 (VoIP Metrics: jitter buffer nominal/abs max).
- **Риск:** добавленный target delay напрямую режет `I_d` в E-model: каждые +20 мс JB ≈ −0.5 MOS на границе; буфер должен адаптироваться вниз медленнее, чем вверх (иначе flapping); при 500 плечах память буферов (2 МБ на видео-AU × 500) — считать заранее.

### 2.4. NACK + RTX (RFC 4585, RFC 4588)

- **Как у нас:** **браузеры — есть** (str0m: generic NACK `session.rs:47,290-337`, RTX-ресенд из буфера, `error.rs:49`); **SIP/H.323 — нет**: RTCP на этих плечах не читается вообще (`grep 'rtcp_socket.recv'` → 0), входящий RTCP в пути A выбрасывается (`udp_loop.rs:189-197`).
- **Патч:** для шлюзовых плеч: (1) resend-cache 100-200 мс на SSRC (RingBuffer пакетов), (2) отправка generic NACK (FMT=1, PID+BLP, 16 seq за 4 байта) при обнаружении gap в `RtpSource`, максимум 2 запроса с интервалом 33 мс (как str0m), (3) чтение входящих NACK на RTCP-сокете и ответ из кеша (для обратного направления), (4) RTX-PT для браузерных плеч уже закрыт str0m. **Важно:** NACK без JB бессмысленен — сначала §2.3, иначе ретрансмиссия придёт «поздно» и будет отброшена всё тем же `distance != 1` (`gateway_audio.rs:588-589`).
- **Метрика:** `vks_nack_sent_total`, `vks_rtx_sent_total`, `vks_nack_unanswered_total`, «post-repair loss» = (потери до восстановления − восстановлено)/всего; цель: ≥80 % потерь восстановлено при RTT ≤ 100 мс.
- **RFC:** RFC 4585 §6.2 (Generic NACK), RFC 4588 (RTX, `apt`), RFC 3550 §6.1 (RTCP-мультиплексирование).
- **Риск:** ретрансмиссии увеличивают исходящий поток ровно в момент перегрузки (нужен приоритет NACK-ответов и учёт в пейсере, иначе — congestion collapse); при `MAX_PEERS_PER_ROOM=2` эффект на 20 устройств нулевой — нужен именно шлюзовой путь.
  **Конструктивное требование к кэшу (согласовано с media-core, O7):** при шаринге кэша на egress-группу хранить в нём **открытый** payload и шифровать при отправке (per-viewer SRTP-ключи) — иначе общий пакет нельзя переиспользовать между зрителями. Стоимость шифрования при ретрансмите — O(1) на зрителя и платится только на loss-событиях.

### 2.5. FEC (RFC 5109 ULPFEC / RFC 8627 flexfec / Opus in-band FEC RFC 6716 §4.5.3)

- **Как у нас:** **нет ULPFEC/flexfec; частично Opus in-band FEC.** `opus.rs:43-49` — энкодер с `use_inband_fec=true`, `packet_loss_perc=10`; `opus.rs:79-90` — `decode_fec`; `gateway_audio.rs:103-117` — применяется **только если потерян ровно 1 пакет**, иначе до 3 PLC. Для G.711/G.722 — FEC нет вообще.
- **Патч:** (1) аудио: оставить Opus FEC, добавить RFC 2198 redundancy или простой XOR-FEC (1 из N=3) для G.711/G.722 — 33 % overhead, зато без RTX (не зависит от RTT); (2) видео: ULPFEC (RFC 5109) XOR-блок 10 пакетов / 2 repair-пакета = 20 % overhead, восстанавливает одиночные потери в AU **до** того, как depacketizer сбросит fragment (иначе поздно — `gateway_video.rs:938-943`); (3) референс у нас перед глазами: Vinteo держит FEC статически (OpenFEC 1.4.2 LDPC staircase + RS(2^8), `vinteo/09_Спецификация:85,210-216`, `07_Адвокат_дьявола:19`), плюс отдельный `panasfec` (`09:61`).
- **Метрика:** `vks_fec_repaired_total`, `vks_fec_overhead_bytes_ratio` (цель ≤ 25 %), residual loss после FEC; проверка — netem loss 5 % и сравнение «frames delivered» с/без.
- **RFC:** RFC 5109 (ULPFEC), RFC 8627 (flexfec), RFC 6716 §4.5.3 (Opus LBRR), RFC 2198 (redundancy).
- **Риск:** overhead FEC прямо уменьшает полезную полосу и на congested-линии ухудшает картину (классическая ловушка); flexfec сложнее в SDP-согласовании (`a=rtpmap:... flexfec-03`); XOR-FEC нельзя применить к видео с переменной длиной пакета без выравнивания по max.

### 2.6. PLC (G.711 App. I / G.722 App. III)

- **Как у нас:** **только Opus** (`gateway_audio.rs:103-117`, `opus.rs:104-120` — `decode(&[])` = PLC-кадр). G.711/G.722/G.726/G.728/G.723.1: `conceal()` возвращает пустой `Vec` (`gateway_audio.rs:105`: `let Self::Opus(codec) = self else { return out };`) → в очередь ничего не попадает → на выходе микса нули → «провал в тишину» и щелчок на стыке, что для E-model эквивалентно PLC худшего качества (G.113 `Bpl` не выполняется).
- **Патч:** G.711 — pitch-synchronous repetition с затуханием (G.711 App. I: повтор последнего 20-мс сегмента с шагом основного тона и −6…−12 дБ на кадр, максимум 3-5 кадров); G.722 — G.722 App. III (или waveform repeat с fade как допустимый упрощённый вариант); G.726/G.728/G.723.1 — повтор предыдущего кадра декодера с затуханием. Реализовать в `AudioBridgeCodec::conceal` рядом с Opus-веткой.
- **Метрика:** PESQ/POLQA на захвате при `loss 3%` (цель ≥ 3.5 POLQA для G.711), частота «жёстких нулей» в миксе (`vks_audio_silence_frames_total`), click-rate.
- **RFC/стандарт:** ITU-T G.711 Appendix I, G.722 Appendix III, G.113 App. I (Ie/Bpl), RFC 3389 (CNG как дополнение).
- **Риск:** плохой PLC (повтор без затухания) звучит хуже, чем тишина; при сериях >5 кадров любой PLC даёт «металлический» артефакт — нужен cap (как сейчас 3).

### 2.7. TWCC (RFC 8888) / REMB и обратная связь о потерях

- **Как у нас:** **путь A/шлюзы — нет; браузеры — есть.** `bwe.rs` — это заготовка: `BweEstimator` считает `bps = bytes·8/window` (`bwe.rs:149-165`), без delay-gradient (GCC), без loss-терма, без REMB (`bwe.rs:9-11` сам признаёт «фаза 3»), стартовое значение 500 kbps (`bwe.rs:40`), и **не подключён к отправке** — `route_udp_packet` его не читает, пейсера нет. Лестница деградации (`congestion.rs:47-52`) переключает только simulcast-слой, а `RtpPacket` для SFU непрозрачен (`forwarder.rs:18-19` «SFU не инспектирует») — т.е. снизить битрейт реально нечем. Для шлюзов обратной связи нет вообще: входящие RR/XR не читаются.
- **Патч:** (1) для шлюзовых плеч — читать RTCP RR и считать loss fraction + interarrival jitter (RFC 3550 §6.4.1); включить RFC 3611 XR VoIP Metrics на отправку (loss rate, discard rate, burst/gap density, JB nominal) — это же даёт нам измеримость для netem; (2) для пути A — TWCC header extension (RFC 8888 §3) + парсинг `Transport-CC` фидбэка и подключение результата к пейсеру/слою; (3) переписать `bwe.rs` на delay-based (тренд задержки, как GCC/`draft-ietf-rmcat-gcc`) либо просто переиспользовать str0m BWE (`webrtc.rs:344`) как единственный источник истины.
- **Метрика:** `vks_bwe_estimate_bps` vs фактический `tx_bps` (ошибка ≤20 %), overuse-события, `vks_rtcp_rr_loss_fraction`; проверка — netem rate 1 Мбит и смотреть, садится ли оценка.
- **RFC:** RFC 8888 (TWCC), RFC 3550 §6.4.1 (RR), RFC 3611 (XR), REMB — `draft-alvestrand-rmcat-remb`.
- **Риск:** TWCC требует согласования header extension в SDP (для аппаратных терминалов его нет — только RR/XR); при непрозрачном SFU-форвардинге смена слоя не меняет поток, пока не реализован селектор слоёв (уже есть `simulcast.rs:...`, но слой выбирается по bps `engine.rs:131-157`, а сам поток не фильтруется по слою в `route_udp_packet`).

### 2.8. Pacer (leaky bucket / token bucket)

- **Как у нас:** **браузеры — есть** (str0m `pacer/leaky.rs`, включается вместе с BWE: `webrtc.rs:127-145`); **шлюзы и путь A — нет.** `gateway_video.rs:601-602` отправляет весь AU подряд `for packet { send_to().await }`; `udp_loop.rs:210-215` — все подписчики подряд в одном цикле на каждый входящий пакет.
- **Патч:** token-bucket 1.05×target на плечо (аудио — не нужно, 1 пакет/20 мс; видео — обязательно) + разнесение отправки AU по интервалу кадра (например, 100 пакетов AU за 33 мс = 3 пакета/мс); на fan-out в пути A — пакетирование `sendmmsg` + ограничение «не больше K пакетов подряд, затем yield».
- **Метрика:** p99 размера TX-burst (`tc -s qdisc` backlog, `tx_queue`), дропы в `fq_codel`, `vks_tx_burst_p99`.
- **RFC:** нет отдельного RFC; RFC 8085 §3.1.6 (pacing для UDP), `draft-ietf-rmcat-cc` §3.
- **Риск:** пейсер добавляет задержку (при 1.5 Мбит/с и AU 10 КБ — 6.7 мс, приемлемо); при неправильном target может «зажать» keyframe → шторм PLI.

### 2.9 (бонус, не механизм устойчивости). DSCP-маркировка

- **Как у нас:** **нет** (DSCP нигде не выставляется; `setsockopt` в проекте отсутствует вовсе).
- **Почему важно:** референс Cisco маркирует DSCP на каждом сокете: `IP_TOS = dscp<<2` (`cisco/A6a_kokoro_config/README.md:338-346`), Vinteo — через Asterisk/MCU. Без маркировки на 500 абонентов QoS-очереди на коммутаторе не работают, и burst-потери начнутся раньше, чем кончится CPU.
- **Патч:** `IP_TOS=EF(46)` для аудио, `AF41(34)` для видео, `CS1` для RTCP; на сокете — `setsockopt` после bind. Метрика: `tcpdump -v` / `tc -s` по классам.

---

## 3. Профиль sysctl и лимитов

### 3.1. Что есть сейчас (измерено на стенде)

| Параметр | Значение | Оценка |
|---|---|---|
| `net.core.rmem_max` / `wmem_max` | `4194304` (4 MiB) | мало; но **не используется**, т.к. приложение не ставит `SO_RCVBUF` |
| `net.core.rmem_default` / `wmem_default` | не задан отдельно → 212992 | **фактический буфер медиа-сокета** (`ss -m`) |
| `net.core.netdev_max_backlog` | `1000` | ~9 мс burst |
| `net.core.netdev_budget` / `_usecs` | `300` / `2000` | ок для 20, мало для 500 |
| `net.ipv4.udp_mem` | `1383144 1844193 2766288` (стр., ≈5.3/7.0/10.5 ГиБ) | не ограничение (RAM 57 ГиБ) |
| `net.ipv4.udp_rmem_min` / `wmem_min` | `4096` / `4096` | дефолт |
| `net.ipv4.ip_local_port_range` | `32768 60999` (28 232) | ок, но ICE-порты фиксированы отдельно |
| `net.core.somaxconn` | `4096` | ок |
| `net.core.rps_sock_flow_entries` + `rps_cpus` | `0` + `0` (RPS выключен) | 1 RX-очередь на 36 ядер — узкое место |
| `ulimit -n` | shell `524288`; служба `LimitNOFILE=65536` (`/proc/1177177/limits` = 65536) | для 500 хватит, но с запасом лучше 262144 |
| cgroup | `0::/user.slice/.../vks-mcu.service`; контроллеры `cpuset cpu io memory hugetlb pids rdma misc dmem` | **`net_cls`/`net_prio` отсутствуют** (v2) |
| tc | `fq_codel root limit 10240p flows 1024 target 5ms interval 100ms memory_limit 32Mb` | дефолт Ubuntu, ок |
| NIC | `virtio_net`, 1 combined queue, RX ring 1024, TX ring **256**, TX UDP-segmentation on, LRO off, `receive-hashing: off [fixed]` | TX ring мал для fan-out |
| `txqueuelen` | 1000 | ок для 20, мало для 500 |

### 3.2. Профиль «20 паблишеров + 20 подписчиков» в ВМ (стенд, 1 Гбит/с)

```ini
# /etc/sysctl.d/60-vks-media-20.conf   (применять НЕ на живом стенде без окна)
net.core.rmem_max = 16777216          # 16 MiB — потолок для SO_RCVBUF
net.core.wmem_max = 16777216
net.core.rmem_default = 4194304       # 4 MiB — дефолт для всех UDP (в т.ч. наших)
net.core.wmem_default = 4194304
net.core.netdev_max_backlog = 8192    # 8x запас по burst
net.core.netdev_budget = 600
net.core.netdev_budget_usecs = 8000
net.core.rps_sock_flow_entries = 32768
net.ipv4.udp_rmem_min = 16384
net.ipv4.udp_wmem_min = 16384
net.ipv4.ip_local_port_range = 20000 65000
net.core.somaxconn = 8192
```
Плюс (не sysctl): `echo 32768 > /sys/class/net/ens18/queues/rx-0/rps_flow_cnt`, `rps_cpus = fffff` (все 36 CPU маской из 36 бит → `3f,ffffffff` в формате hex-списка), `txqueuelen 4096`, `ethtool -G ens18 tx 1024`.
Обоснование: расчётный burst §1.3 = 110 kpps; 16 MiB / 1400 Б ≈ 11 700 пакетов ≈ **106 мс** поглощения; 8 MiB socket-buffer ≈ 6000 пакетов ≈ 54 мс (переживает GC/планировщик-провал); backlog 8192 ≈ 73 мс.
`deploy/systemd/vks-mcu.service` — добавить `LimitNOFILE=262144` (сейчас 65536) и `Environment=VKS_MEDIA_RCVBUF=8388608` (новый ключ, см. §2.1).

### 3.3. Профиль «500 абонентов» (MCU-режим, 1→N микширование)

Сначала — **жёсткие архитектурные пределы**, которые никакой sysctl не снимает:

| Предел | Значение | Что нужно |
|---|---|---|
| ICE-порты | `43000-43100` = **101** (`deploy/systemd/vks-mcu.service`) | диапазон ≥ 500 + запас: `20000-40000` (20 100 портов) |
| RTP-пул шлюзов | `41000-41200` = **100 пар** (`rtp_bridge.rs:22`) | `20000-40000`; в MCU-режиме 2 пары/звонок → 100 пар = **50 видеозвонков** |
| Браузерные плечи | `MAX_PEERS_PER_ROOM = 2` (`webrtc_room.rs:33`) | конфигурируемый лимит + 500 портов |
| Единый recv-loop | 1 задача (`udp_loop.rs:151-227`) | `SO_REUSEPORT` + 16-32 воркера |
| Микшер минус-миксов | O(N²): 500² × 960 × 50 = 12·10⁹ операций/с + клоны `Vec<f32>` на каждого участника каждый тик (`vks-media-mcu/src/engine.rs:709-714` `audio_map.get(pid).cloned()`) | потоковые минус-миксы (full_mix − self), без аллокаций |
| Композитор | 500 зрителей × rescale 1080p (`compositor.rs:73-155` nearest-neighbour по пикселям) | **не масштабируется по CPU** — 500 достижимы только в SFU-режиме/с hardware-энкодерами |

```ini
# /etc/sysctl.d/60-vks-media-500.conf
net.core.rmem_max = 134217728         # 128 MiB — потолок
net.core.wmem_max = 134217728
net.core.rmem_default = 8388608       # 8 MiB
net.core.wmem_default = 8388608
net.core.netdev_max_backlog = 65536
net.core.netdev_budget = 1200
net.core.netdev_budget_usecs = 16000
net.core.rps_sock_flow_entries = 262144
net.ipv4.udp_rmem_min = 65536
net.ipv4.udp_wmem_min = 65536
net.ipv4.udp_mem = 3145728 4194304 6291456     # 12/16/24 GiB в страницах (при RAM ≥ 64 GiB)
net.ipv4.ip_local_port_range = 20000 65000     # 45 000 портов
net.core.somaxconn = 16384
net.netfilter.nf_conntrack_max = 1048576        # если есть NAT/firewall-stateful
net.netfilter.nf_conntrack_udp_timeout = 30
net.netfilter.nf_conntrack_udp_timeout_stream = 120
```
**Бюджет памяти под сокеты (обязательно считать!):** ядро удваивает `SO_RCVBUF`; при 8 МиБ на сокет и 500 сокетах это `500 × 2 × 8 МиБ = 8 ГиБ` только на RX-буферы → это и есть причина, по которой Vinteo держит `udp_mem` ≈ 4.6 ГиБ и `rmem_default` 479 МиБ (`vinteo/11_Реестр:859-873`). Поэтому: на 500 плечах `SO_RCVBUF = 2 МиБ` (≈2 ГиБ суммарно), 8-16 МиБ — только для агрегирующего сокета пути A.

**Порты/лимиты (не sysctl):**
- ICE: `20000-40000`, `VKS_WEBRTC_PORT_MIN/MAX`; RTP-пул шлюзов: тот же диапазон.
- `LimitNOFILE=524288` (500 плеч × до 4 UDP-сокетов + H.323 python-дети).
- `TasksMax` не ограничивать (сейчас 69375 по дефолту — ок).
- TX ring: `ethtool -G ens18 tx 4096` (сейчас 256); для ≥500 — 2×10 Гбит или RSS-многопоточность (`ethtool -L ens18 combined 8`, но сейчас `numrxqueues 1` — менять на стороне гипервизора).
- `txqueuelen 10000` + `fq_codel` (или `fq` для чистого пейсинга, но `fq_codel` даёт anti-bufferbloat — оставить).

### 3.4. cgroup v2: что реально применимо (и чего нет)

Проверено: `/sys/fs/cgroup/cgroup.controllers` = `cpuset cpu io memory hugetlb pids rdma misc dmem` — **`net_cls` и `net_prio` в v2 отсутствуют**. Значит:
- `net_cls.classid` + `tc filter ... cgroup` (классический способ резать трафик по cgroup) — **неприменимо**.
- Реально доступно:
  1. **`IPAccounting=yes`** в unit — ядро считает байты/пакеты per-unit (eBPF), видно в `systemctl show vks-mcu -p IPIngressBytes -p IPEgressBytes`. Это единственный «сетевой» per-unit счётчик в v2 — годится для алертов «медиа-трафик вышел за бюджет».
  2. **`MemoryMax=`** — косвенно ограничивает и socket-память (она в `memory.stat` как `sock`); при 500 плечах ставить `MemoryMax` с запасом ≥ 2× расчётного (буферы + фреймы).
  3. **`CPUWeight=`** — приоритезировать медиа-воркеры относительно БД/фронта (сейчас `CPUQuotaPerSecUSec=infinity`, `Nice=0` — все равны).
  4. **Маркировка/шейпинг по cgroup id в v2** — не через `net_cls`, а через `iptables -m cgroup --path` / `nft ... socket cgroupv2 level 1` + `DSCP --set-dscp 46`, либо `tc` + eBPF (`bpf` classifier по `bpf_get_cgroup_classid` уже не работает, нужен `bpf_skb_cgroup_id`).
  5. **Изоляция**: `net.core.*` — **per-netns**; `systemd` `PrivateNetwork=`/`NetworkNamespacePath=` даёт отдельный netns со своими sysctl — единственный штатный способ дать сервису «свои» буферы, не трогая хост.
- Практический вывод: **sysctl для медиа — хостовые**, cgroup v2 их не заменит; в v2 мы получаем только учёт (`IPAccounting`) и косвенное давление (`MemoryMax`, `CPUWeight`).
- Отдельно: `deploy/systemd/vks-mcu-prod.service` содержит `ProtectKernelTunables=true` — сервис **не сможет** применить sysctl сам, только `sysctl.d` на хосте. Это правильно, но означает, что sysctl-профиль обязан быть в поставке (нужен `deploy/sysctl/60-vks-media.conf` + проверка в preflight).
- `CapabilityBoundingSet=` (пусто) в prod-юните → `setsockopt(SO_RCVBUF)` не требует capabilities, ок; `tc`/`sysctl` из процесса невозможны — и не нужны.

---

## 4. Методика netem-тестов

**Жёсткое правило:** стенд `192.168.1.93` — единственная ВМ, `ens18` несёт и live-сервер, и SSH. `tc qdisc add` на `ens18` = гарантированный разрыв. Все тесты — **только в изолированном netns + veth**, и только при наличии root (сейчас `id -u = 1000`, `ip netns list` пуст, `sch_netem.ko` в системе есть — `modinfo` подтверждает; **сами команды не запускались** — «не проверено»).

### 4.1. Изоляция: veth + netns

```bash
# root, НЕ на ens18
ip netns add vks-loss
ip link add vks0 type veth peer name vks1
ip link set vks1 netns vks-loss
ip addr add 10.99.0.1/24 dev vks0 && ip link set vks0 up
ip netns exec vks-loss ip addr add 10.99.0.2/24 dev vks1
ip netns exec vks-loss ip link set vks1 up
ip netns exec vks-loss ip link set lo up
# сервер тестируемой сборки — внутри netns (свой netns = свои sysctl!)
ip netns exec vks-loss sysctl -w net.core.rmem_max=16777216 net.core.rmem_default=4194304
ip netns exec vks-loss ./target/release/vks-server --config /tmp/test.toml   # media_listen_addr = 0.0.0.0:40000
# клиент-эмулятор 20 терминалов — на хосте, в 10.99.0.1
```
Так live-процесс (`0.0.0.0:40000` в root-netns) не пересекается: тестовый сервер сидит в `vks-loss` на том же порту, но в другом netns. **Проверить до старта:** `ip netns exec vks-loss ss -lunp | grep 40000` — пусто; `ss -lunp | grep 40000` — live-сервер.

### 4.2. Точные команды `tc` (на `vks0`, направление «к серверу»; для обратного — на `vks1` в netns)

```bash
# базовый профиль задачи: 2 % потерь, джиттер 30 мс (среднее 30, разброс ±15), RTT ≈ 60 мс
tc qdisc add dev vks0 root handle 1: netem delay 30ms 15ms 25% loss 2% 
#   ^ 25% — корреляция (bursty), без неё — чистый Bernoulli:  tc qdisc add dev vks0 root netem delay 30ms 15ms loss 2%
# 0.5 % потерь
tc qdisc replace dev vks0 root netem delay 30ms 15ms loss 0.5%
# переупорядочивание (требует delay): 25 % пакетов с шансом 50 % «уйти вперёд» на 1 пакет
tc qdisc replace dev vks0 root netem delay 30ms 15ms reorder 25% 50% gap 1
# bursty потери по Gilbert-Elliott (реалистичный профиль Wi-Fi/LTE)
tc qdisc replace dev vks0 root netem delay 30ms 15ms loss gemodel 1% 10% 70% 0.1%
# ограничение полосы + netem как ребёнок (порядок важен: tbf снаружи)
tc qdisc replace dev vks0 root handle 1: tbf rate 2mbit burst 32k latency 50ms
tc qdisc add dev vks0 parent 1:1 handle 10: netem delay 30ms 15ms loss 2%
# «плохой Wi-Fi»: 5 % потерь, 100 мс джиттера, 1 % reorder
tc qdisc replace dev vks0 root netem delay 50ms 50ms loss 5% 25% reorder 1% 50%
# снять всё
tc qdisc del dev vks0 root
```
**Обязательная симметрия:** потери/jitter надо ставить на **оба** направления (`vks0` и `vks1`), иначе получим односторонний профиль и неверные выводы о RTCP-петле.

### 4.3. Как мерить потери/джиттер (у нас нет XR — берём снаружи)

**Откуда берётся «+5.6 % дропов от джиттера» (§0в).** Пакеты уходят каждые 20 мс; задержка `d ~ U(15, 45)` мс (`delay 30ms 15ms`). Прибытие `a_n = 20n + d_n`; перестановка ⇔ `a_{n+1} < a_n` ⇔ `d_n − d_{n+1} > 20`.
Разность двух независимых `U(15,45)` имеет треугольное распределение на `(−30, 30)`; `P(D > 20) = ½·(1 − 20/30)² = ½·(1/3)² = 1/18 ≈ 5.56 %`.
Наш `RtpSource::accept` такой пакет **отбрасывает** (`gateway_audio.rs:587-592`), а не буферизует ⇒ эффективные потери `0.02 + 0.0556 ≈ 7.6 %` — это и есть цифра из §0 и §4.4. **Проверяемая гипотеза:** после появления JB (§2.3) доля `late/discarded` в метриках должна упасть до `≈0 %`, а `loss%` из pcap — остаться 2 %.

1. **Захват:** `tcpdump -i vks1 -s 0 -w /tmp/cap.pcapng udp portrange 40000-41200` (внутри netns: `ip netns exec vks-loss tcpdump ...`).
2. **RTP-метрики из pcap (RFC 3550 A.8):**
```bash
tshark -r /tmp/cap.pcapng -Y rtp -T fields -e rtp.ssrc -e rtp.seq -e frame.time_epoch -e rtp.marker \
  | sort -k1,1 -k3,3 > /tmp/rtp.tsv
# jitter (RFC 3550 A.8): D = (Rj-Ri) - (Sj-Si); J += (|D|-J)/16; в мкс
# loss: N_expect = last_seq - first_seq + 1; loss% = (N_expect - N_recv)/N_expect*100
# reorder: доля пакетов с seq < last_seq
```
3. **Системные счётчики (до/после, дельта):**
```bash
nstat -n; <тест>; nstat -z | grep -E 'UdpRcvbufErrors|UdpInErrors|UdpNoPorts'
tc -s qdisc show dev vks0        # backlog/drops в netem
awk '{p+=$1;d+=$2;t+=$3} END{print p,d,t}' /proc/net/softnet_stat
ip -s link show vks0             # rx/tx dropped
ss -m -u | grep -E '40000|41000' # skmem rb/rcv, drops — per-socket (в netns)
```
4. **Целевые «гейты» теста** (для 20 устройств, аудио G.711 20 мс + видео 720p):
   - `loss 0.5 %`: потери после JB/NACK ≤ 0.2 %; MOS-LQ (PESQ) ≥ 4.0; видео: 0 «заморозок» > 100 мс.
   - `loss 2 %` Bernoulli: аудио PESQ ≥ 3.8 (E-model тут даст 4.16 — **не верить E-model как приёмке**, см. §4.4); видео — «≥1 keyframe/с» успехом не считается: цель — «frame intact» ≥ 97 % и нет заморозок > 200 мс.
   - `loss 2 % 25 %` (bursty) + `delay 30ms 15ms`: **главный профиль приёмки**; аудио ≥ 3.5 POLQA; видео — деградация только по битрейту (через TWCC/REMB/слой), без «чёрного экрана».
   - `reorder 25% 50%`: **ноль** лишних потерь от перестановки (сейчас — +5.6 % дропов, §0в и расчёт выше). Это самый дешёвый тест, который ловит отсутствие JB.
5. **Смоук «не развалилось»:** `ss -s`, `/metrics` (появились ли `vks_jb_late_total`, `vks_nack_sent_total`), RSS сервера, `fq_codel` drops.


### 4.4. Как считать MOS (и почему E-model нельзя брать как приёмку)

E-model (ITU-T G.107):
```
R = 93.2 − I_d − I_e_eff
I_d = 0.024d + 0.11·(d − 177.3)·1{d > 177.3}          # d — one-way delay, мс (включая JB target и packetization)
I_e_eff = I_e + (95 − I_e)·P_pl / (P_pl + B_pl)        # P_pl — доля потерь, %
MOS = 1 + 0.035R + 7e-6·R·(R−60)(100−R),  clamp [1, 4.5]
```
Параметры (G.113 App. I / RFC 6716 рекомендации): G.711 PCMU `Ie=0, Bpl=25.1`; G.722 `Ie=0, Bpl≈25`; Opus 48k/64kbps `Ie≈2, Bpl≈20`.
Расчёты для профиля задачи (`d=100 мс` → `I_d=2.4`; `P_pl` — в процентах, как в G.107; проверено скриптом):

| Кодек | P_pl | I_e_eff | R | MOS | Комментарий |
|---|---:|---:|---:|---:|---|
| G.711 | 0.5 % | 1.86 | 88.9 | 4.31 | чистый Bernoulli |
| G.711 | 2.0 % | 7.01 | 83.8 | **4.16** | проходит (>4.0) |
| G.711 | 3.43 % | 11.3 | 79.5 | **4.00** | ← порог MOS 4.0 |
| G.711 | **7.6 %** (=2 % + reorder 5.6 %) | 22.1 | 68.7 | **3.54** | ← **наш реальный профиль, <4.0** |
| G.711 | 10 % | 27.1 | 63.7 | 3.29 | |
| G.711 | 20 % | 42.1 | 48.7 | 2.51 | |
| Opus | 2.0 % | 10.45 | 80.3 | **4.04** | уже на пороге (порог 2.26 %) |
| Opus | **7.6 %** | 27.6 | 63.2 | **3.26** | ← наш профиль для Opus |
| G.722 | 2.0 % / 7.6 % | 7.04 / 22.2 | 83.8 / 68.7 | 4.16 / 3.53 | как G.711 |

Вывод: **равномерные 2 % E-model переживает** (G.711 — 4.16), но **наш профиль «2 % + джиттер 30 мс» — нет** (7.6 % эффективных потерь → 3.54 / 3.26), и это ровно исходная позиция. Для Opus даже чистые 2 % дают 4.04 — «впритык». Поэтому приёмка — не E-model, а:
- **PESQ (ITU-T P.862) / POLQA (P.863)** на реальном захвате: подать эталонный WAV в терминал-эмулятор, захватить с выхода микшера (`tcpdump` → decode G.711/G.722/Opus → WAV), выровнять по задержке, посчитать. Инструменты: `pesq`, `polqa` CLI; для G.711 достаточно `pypesq`.
- **Видео:** `ffmpeg -i rtsp/rtp-поток -vf freezedetect=n=0.003:d=0.2 -f null -` (счётчик «заморозок» > 200 мс) + VMAF/PSNR против эталонного клипа; метрики — `freeze_count`, `freeze_duration_total`, `frame_drop_ratio`.
- **E-model — только как быстрое аналитическое прикидочное средство** с явной пометкой «равномерные потери, идеальный PLC, без учёта нашего drop-on-reorder». Ограничения: E-model не моделирует burst-loss (для этого нужен PESQ на GE-профиле), не моделирует видео вообще, и предполагает PLC уровня G.711 App. I — которого у нас для G.711 нет.
- **RTCP XR (RFC 3611 §4.7)** — правильный машинный путь: как только научимся отправлять VoIP Metrics (loss rate, discard rate, burst/gap density, JB nominal/abs-max delay), стенд сам начнёт отдавать «MOS-CQ»-прокси и данные для JB. Сейчас XR не реализован ни на одном плече (`grep -c '3611\|XrBlock' crates/` → 0).

### 4.5. Чувствительность к модели джиттера и цена JB по задержке (превентивная защита)

Два места, где позицию будут атаковать: (а) «5.6 % — это артефакт одной модели netem», (б) «JB добавит задержку и сам испортит MOS». Считаю оба заранее.

**(а) Вывод не зависит от модели джиттера.** `P(reorder)` при пакетизации 20 мс для «джиттера 30 мс» в разных трактовках netem:

| Модель netem | P(reorder) | P_eff при 2 % loss | MOS G.711 | MOS Opus |
|---|---:|---:|---:|---:|
| `delay 30ms 15ms` (равномерно ±15) | 5.56 % | 7.56 % | **3.54** | 3.27 |
| `delay 30ms 15ms distribution normal` (σ=15) | 9.12 % | 11.12 % | **3.18** | 2.87 |
| `delay 15ms 30ms` (равномерно 0…60, если «30 мс» = размах) | 22.22 % | 24.22 % | **2.27** | 1.96 |
| `reorder 25% 50%` (явный reorder) | 12.50 % | 14.50 % | **2.89** | 2.56 |

Во **всех** вариантах MOS < 4.0, т.е. вывод «позиция верна для связки 2 % + 30 мс джиттера» устойчив; разброс только в сторону **хуже** (от 3.54 до 1.96). Единственный способ получить ≥4.0 — убрать reorder-дропы (JB), а не переопределить модель.

**(б) Цена JB по задержке на порядок меньше выигрыша от устранения reorder-дропов.** JB увеличивает one-way delay на target: `d = 100 + target`. Потери фиксируем 2 % (reorder «вылечен» JB):

| JB target | d, мс | I_d | MOS G.711 | MOS Opus |
|---:|---:|---:|---:|---:|
| 0 (сейчас) | 100 | 2.40 | 4.16 | 4.04 |
| 20 | 120 | 2.88 | 4.14 | 4.02 |
| 40 | 140 | 3.36 | 4.13 | **4.00** |
| 60 | 160 | 3.84 | 4.11 | 3.98 |
| 80 | 180 | 4.62 | 4.08 | 3.95 |
| 100 | 200 | 7.30 | 3.98 | 3.84 |
| 150 | 250 | 14.00 | 3.70 | 3.54 |

Баланс: JB на 40-60 мс **стоит** 0.03-0.05 MOS, а **возвращает** 0.5-1.3 MOS (3.54 → 4.11 для G.711; 3.27 → 4.00 для Opus). Отсюда — конструктивное ограничение для §2.3: **target ≤ 40-60 мс, не 200 мс** (fixed 200 мс, как в str0m для видео, для аудио недопустимы: при `d=300` `I_d=18.1`, MOS 3.55 даже при нулевых потерях). Это и есть ответ на «JB сам испортит MOS»: испортит только при target > 80-100 мс, поэтому адаптивный JB обязан иметь верхнюю границу и приоритет «лучше потерять 1 кадр, чем держать 200 мс».


---

## 5. Контраргументы media-core и ответы

**К1. «CPU горит в композиторе/MCU-микшере — сеть вторична, сначала оптимизируй композитор».**
*Ответ:* (1) Это не альтернатива. Смотрим факты: микшер клонирует `Vec<f32>` на каждого участника каждый тик (`vks-media-mcu/src/engine.rs:709-714`) и считает минус-миксы O(N²) (`engine.rs:400-411`), композитор — nearest-neighbour по пикселям в Rust-цикле (`compositor.rs:73-155`, `scale_row:155`). Да, для 500 это стена. Но: (2) **эта стена обходится архитектурно** (SFU-режим без композитора уже есть: `default_room_mode="sfu"` в `configs/default.toml`, `SfuEngine` не декодирует — `forwarder.rs:18-19`), а (3) **сетевая стена не обходится ничем, кроме сетевого кода**: без `SO_RCVBUF`/`recvmmsg`/JB/NACK ни один оптимизированный композитор не доставит кадр, потерянный в 208-КиБ буфере; потеря пакета не компенсируется ни одним FLOPS. (4) Порядок работ определяется стоимостью: sysctl+`SO_RCVBUF`+`sendmmsg` — это ~1-2 дня и конфиг; JB+NACK+FEC — 2-3 недели; переделка композитора на 500 — месяцы. При равном риске сначала берут дешёвое.
*Оговорка (честная, уточнена по данным media-core после раунда 1):* при **20** участниках на 36 vCPU композитор действительно не предел — предел именно транспорт (порты, буферы, один recv-loop). Более того, формулировка «композитор дорог» неточна: resize 720p измерен как 8.0-8.6 мс/кадр, identity-1080p — 18.8-20.9 мс/кадр, так что узкое место CPU — **per-device egress** (resize + N энкодеров + декод), а не сам блендинг. Вывод для меня не меняется: «network-first» — не догма, а следствие конкретных чисел этого стенда (порты, буферы, drop-on-reorder, отсутствие JB).

**К2. «Всё это уже есть в str0m: NACK, RTX, TWCC, pacer, SRTP. Значит сетевой стек решён».**
*Ответ:* есть — но ровно для плеча **B** и ровно для **2 участников на комнату** (`webrtc_room.rs:33`). 20 устройств стенда — это SIP/H.323 (путь **C**), где из перечисленного нет **ничего**: RTCP не читается (`grep 'rtcp_socket.recv'` → 0), NACK/RTX нет, FEC нет, JB нет, пейсера нет (burst `for packet { send_to }` — `gateway_video.rs:601-602`). Плюс str0m не даёт того, что нужно даже на пути B: FEC (`FEC` в матрице отсутствует), адаптивного JB (`:x:` в матрице str0m), а `MAX_PEERS_PER_ROOM=2` — это прямое «не масштабируется». Итог: «есть в str0m» = «есть у 2 браузеров», а задача про 20 устройств и 500 абонентов.

**К3. «sysctl/сокеты — это тюнинг, а не архитектура; на 20 устройствах 208 КиБ хватит».**
*Ответ:* арифметика §1.3: 20 устройств в SFU full-mesh = 110 kpps fan-out; 208 КиБ = 152 пакета = 1.4 мс. Достаточно провала планировщика на 2 мс из-за конкуренции с FFmpeg-воркерами (композитор/энкодеры — заведомо CPU-жадные: resize 720p измерен как 8.0-8.6 мс/кадр, identity-1080p 18.8-20.9 мс/кадр, IPC 2.04 — данные media-core) — и буфер переполняется. «Хватит» верно ровно до первого одновременного «все заговорили + видео-кейфрейм» — то есть до типового сценария конференции. И это не тюнинг: `SO_RCVBUF` не выставлен в коде вообще (0 совпадений `setsockopt` в проекте) — это отсутствующая функциональность, а не неоптимальное значение.

**К4. «Референсы (Cisco/TrueConf) тоже считают ресурсы по CPU: `max-calls 500`, `call-bandwidth 5000 Кбит/с`, `limits.encoders/decoders` — значит вендоры считают CPU, а не сеть».**
*Ответ:* они считают CPU **поверх уже решённого сетевого слоя**. Cisco: полный инвентарь `setsockopt` — `SO_RCVBUF`, `SO_SNDBUF`, `SO_BINDTODEVICE`, `SO_RCVTIMEO`, `IP_TOS`=DSCP (`cisco/A6a_kokoro_config/README.md:322-346`), UDP port-pair диапазон **32768-65535** = 32 768 портов (`:286-287`, в 324 раза больше нашего), DTLS-SRTP с отдельным retransmit-потоком (`:366`), и даже **встроенный эмулятор потерь**: `configureTxDropPercentage`, `configureRxDelay/TxDelay`, `configure{Rx,Tx}Mtu` (`:351-357`) — то есть вендор **тестирует сеть как отдельный класс отказов**, потому что считает её критичной. Vinteo: `rmem_max=5.4 ГБ`, `rmem_default=479 МБ`, `udp_mem` 4.6 ГБ, `lo mtu 1600`, `txqueuelen 1000` (`vinteo/11:859-873`), RTP 20000-65000 (`:337-338`), FEC OpenFEC 1.4.2 статически + panasfec + libavqos + libwebrtc-RemoteBitrateEstimator (REMB) в `proxy`. TrueConf в нашем артефакте задокументирован только по CPU-модели (`resource_model.md`) — это **отсутствие данных** о его сетевом слое, а не доказательство, что слоя нет. Вывод: «считать CPU» — это то, что делает вендор, **после** того как выставил гигабайтные буферы и 32 тысячи портов. У нас — ни того, ни другого.

**К5. «Сначала нужно 20 устройств удержать, а FEC/NACK/TWCC — это для 500; не раздувай объём».**
*Ответ:* согласен с приоритизацией — поэтому в §2 разделено на «сейчас» (P0) и «потом» (P1):
**P0 (20 устройств, дни):** `SO_RCVBUF`+`rmem_max`+`netdev_max_backlog`; JB с target-delay для аудио; in-order-only → reorder-буфер; PLC для G.711/G.722; NACK на шлюзовых плечах; порты `43000-43100` → шире; метрики потерь/дропов в `/metrics`; netem-стенд как регрессия.
**P1 (500, недели):** `recvmmsg/sendmmsg` + `SO_REUSEPORT` воркеры; TWCC/REMB-петля; ULPFEC/flexfec; `MAX_PEERS_PER_ROOM` конфигурируемый; `IPAccounting`/`MemoryMax`/`CPUWeight`.
Механизмы из §2.1-2.2 (батчинг/воркеры) — это одновременно и «20 устройств», и «500»: 110 kpps на одной задаче с двумя syscall/пакет — уже 220 ksyscalls/s в одном потоке, что на 36 vCPU выглядит расточительно, но именно этот поток станет первым Bottleneck при любом росте.

**К6. «Ты не измерил — это гипотезы».**
*Ответ:* всё, что можно измерить без изменения стенда, измерено и приведено со значениями (§1.3, §3.1, §4): `skmem rb212992`, `rmem_max`, `netdev_max_backlog`, `RX dropped 2030`, `softnet dropped 0/time_squeeze 38`, `Udp.RcvbufErrors 456 (все на :5353)`, 101 порт ICE, 100 пар RTP, `MAX_PEERS_PER_ROOM=2`, отсутствие `setsockopt`/`recvmmsg`/`rtcp recv`. Не проверено (и помечено): фактическое поведение под netem (нет root), реальный PESQ/MOS, поведение 20 реальных терминалов одновременно — это ровно те измерения, которые даёт методика §4; их отсутствие — аргумент за то, чтобы её внедрить, а не против диагноза.

---

## 6. Открытые вопросы (≤5)

1. **Какой профиль потерь считать приёмочным для 20 устройств стенда** — Bernoulli 2 % (тогда текущий аудио-путь проходит по E-model, и надо признать, что деградация — в видео) или bursty GE (`loss gemodel`/`2% 25%`), который ближе к реальному LAN/Wi-Fi? От этого зависит, идёт ли в P0 NACK или сразу FEC.
2. **Что считать «устройством» в 20+20:** 20 H.323/SIP-терминалов (путь C, наш текущий), 20 браузеров (путь B, упирается в `MAX_PEERS_PER_ROOM=2`), или смесь? Для каждого варианта разный объём P0.
3. **Допустим ли рост JB target-delay** (например, 60-80 мс на аудио) ради устойчивости к джиттеру 30 мс? → **отвечено в §7.3**: оптимум `T = 30-40 мс`; при `T ≥ 60` у Opus MOS падает ниже 4.0, при `T < 30` JB **хуже, чем без него** (1.82 против 3.54). Продуктового выбора почти нет — есть инженерное ограничение.
4. **Кто владеет сетевыми настройками хоста** — нужен ли `deploy/sysctl/60-vks-media.conf` в поставке и проверка в preflight (`sysctl` сейчас вне репозитория; `vks-mcu-prod.service` с `ProtectKernelTunables=true` сам их не применит), или профиль остаётся «ручной операцией эксплуатации»?
5. **Расширяем ли портовые диапазоны в коде** (`rtp_bridge.rs:22` 41000-41200; `VKS_WEBRTC_PORT_MIN/MAX`) или полагаемся на то, что оператор выставит их в env/юните? Сейчас дефолт в коде — 100 пар, и это жёсткий предел в 50 видеозвонков, независимо от sysctl. → **отвечено в §7.2.**

---

## 7. Согласовано с Lead (раунд 2) и ответы на вопросы A/B/C

### 7.1. Зафиксированные решения (вносить в итоговый план как есть)

1. **P(reorder) = 5.56 %** для `netem delay 30ms 15ms` при 20 мс пакетизации: разность двух iid `U[15,45]` — треугольное распределение на `[-30,30]`, площадь ниже `-20 мс` = `(10²/2)/900 = 1/18 = 5.56 %`. Принято Lead, расчёт сходится.
2. **P4a** (JB + reorder-buffer + `SO_RCVBUF` + метрики потерь в `/metrics`) идёт **до** бенчмарка ёмкости, как дешёвый (~0 CPU) и как **предусловие валидности замера**; P3 (композитор, media-core) — параллельно. Не «вместо», а «в другой плоскости»: P3 — CPU-ёмкость, P4a — достоверность измерения.
3. **Joint-design** NACK/RTX-кэш под egress-группы (O7): при 1 энкодере на N зрителей пакеты идентичны → кэш шарится по `(room, ssrc, seq)`, O7 его **удешевляет**. `worker-per-core` (O6) + `SO_REUSEPORT` — один дизайн, с оговоркой §7.2/§2.2 про eBPF-хэш по SSRC.

### 7.2. A) Порты: что hardcoded, что конфиг

| Диапазон / лимит | Где именно | Тип | Что делать |
|---|---|---|---|
| **RTP/RTCP-пул шлюзов `41000-41200` = 100 пар** | `rtp_bridge.rs:22` (`Default → Self::new(41000, 41200)`), вызывается как `RtpPortPool::default()` из `gateways.rs:359` | **HARDCODED, env/конфиг-ключа НЕТ** | **P0, код-патч**: в `gateways.rs:359` читать `VKS_RTP_PORT_MIN/MAX` (или `configs/*.toml`), дефолт оставить `20000-40000` |
| ICE-порты `43000-43100` = 101 порт | `webrtc_room.rs:102-108` — env `VKS_WEBRTC_PORT_MIN/MAX`; аллокация `for port in min..=max` (`:134`), при незаданных — эфемерный `bind(0)` (`:146`) | **КОНФИГ (env)** | **НЕ P0**: правка unit/env (`VKS_WEBRTC_PORT_MIN/MAX`), код не меняется |
| `MAX_PEERS_PER_ROOM = 2` | `webrtc_room.rs:33` (const), проверки `:167,222` | **HARDCODED, с архитектурной причиной** | P0/P1 патч в env — но **только вместе с O7** (см. ниже) |
| `VKS_WEBRTC_MAX_PEERS` (по умолчанию 256) | `webrtc_room.rs:119-123` | конфиг (env) | env |

Тонкости, которые важны для патча:
- `RtpPortPool::new` берёт **чётные** порты и идёт `while p < end` → `end` **эксклюзивен**: `41000-41200` даёт `41000,41002,…,41198` = ровно 100 пар. При расширении до `20000-40000` получится **10 000 пар** (20 000 портов) — этого хватает на 10 000 аудио- или 5 000 видеозвонков (видео = 2 пары).
- `allocate_pair` (`rtp_bridge.rs:38-52`) биндит `port` и `port+1` и требует **оба** свободными, иначе идёт к следующему — т.е. коллизия с ICE-диапазоном приводит не к ошибке, а к тихой потере ёмкости пула. Поэтому диапазоны обязаны быть **непересекающимися**.
- `MAX_PEERS_PER_ROOM=2` нельзя просто поднять: комментарий `webrtc_room.rs:29-32` объясняет причину — «один audio + один video источник на подписчика, иначе RTP clock domains смешаются в одном egress mid». Это **архитектурная** связка с egress-группами (O7 media-core), а не забытая константа. Правильный патч: `MAX_PEERS_PER_ROOM` из env **после** того, как egress строится per-track/mid, а не per-peer.

**Предлагаемое разбиение (без коллизий, ничего не ломает):**

| Назначение | Диапазон | Ёмкость | Обоснование |
|---|---|---|---|
| RTP/RTCP-пул шлюзов | `20000-39999` | 10 000 пар | 2 пары на видеозвонок ⇒ 5 000 звонков |
| legacy `media_listen_addr` | `40000` (как сейчас) | — | остаётся между диапазонами |
| ICE / WebRTC per-peer | `42000-65000` | 23 000 портов | 500 плеч = 2 % диапазона |
| SIP/H.323 сигналинг | `5060` / `1720` | — | вне диапазонов |

Ориентир по референсам: Vinteo `rtpstart=20000 rtpend=65000` (`vinteo/11_Реестр:337-338`), Cisco UDP-пары `32768-65535` (`cisco/A6a_kokoro_config/README.md:286-287`). Наш `20000-39999 + 42000-65000` — того же порядка.

### 7.3. B) JB target-delay vs `I_d`: сколько стоит и сколько возвращает

Модель: `loss=2 %`, джиттер 30 мс = разброс 30 мс peak-to-peak (`±15`), пакетизация 20 мс, базовый one-way `d=100 мс`. При JB с target `T` приёмник сортирует по seq и играет в слоте `min_delay + T`; пакет с относительной задержкой `> T` — **поздний** (дроп), переставленные — восстановлены. Поздние: `P_late = max(0, (30 − T)/30)`.

| Конфиг | P_eff, % | d, мс | MOS G.711 | MOS Opus | Комментарий |
|---|---:|---:|---:|---:|---|
| **без JB (сейчас)** | 7.56 | 100 | **3.54** | **3.27** | 2 % + 5.56 % reorder-дропов |
| **JB 20 мс** | 35.33 | 120 | **1.82** | **1.57** | ⚠️ **хуже, чем без JB** |
| JB 25 мс | 18.67 | 125 | 2.56 | 2.23 | ⚠️ всё ещё хуже |
| **JB 30 мс** | 2.00 | 130 | **4.13** | **4.01** | полный ремонт, оптимум по задержке |
| **JB 40 мс** | 2.00 | 140 | **4.13** | **4.00** | оптимум для Opus (ровно 4.00) |
| JB 60 мс | 2.00 | 160 | 4.11 | 3.98 | G.711 ещё ок, Opus уже <4.0 |
| JB 100 мс | 2.00 | 200 | 3.98 | 3.84 | обе ниже 4.0 |

**Главный вывод (неочевидный и важный для плана):** JB с target **меньше разброса джиттера хуже, чем отсутствие JB** — он начинает дропать «поздние» пакеты, которые при немедленном воспроизведении хотя бы прозвучали бы. Поэтому патч §2.3 обязан быть **условным**: `if T_candidate < p99(arrival − min_arrival) { не буферизовать вовсе / играть сразу }`, иначе получим регресс с 3.54 до 1.82.

**Чистая сеть (jitter 0) — JB не ухудшает MOS (проверка требования Lead):**

| Конфиг | loss 2 %, d | MOS G.711 | MOS Opus | loss 0 %, d | MOS G.711 | MOS Opus |
|---|---:|---:|---:|---:|---:|---:|
| без JB | 100 | 4.16 | 4.04 | 100 | 4.36 | 4.31 |
| JB 20 мс | 120 | 4.14 | 4.02 | 120 | 4.35 | 4.30 |
| JB 30 мс | 130 | 4.13 | 4.01 | 130 | 4.34 | 4.29 |
| JB 40 мс | 140 | 4.13 | 4.00 | 140 | 4.34 | 4.28 |
| JB 60 мс | 160 | 4.11 | 3.98 | 160 | 4.32 | 4.27 |

Цена JB на чистой сети — **0.02-0.05 MOS** при `T ≤ 40`, и она полностью перекрывается выигрышем в 0.5-1.7 MOS на сети с джиттером. Требование «JB не ухудшает MOS на чистой сети» выполняется при `T ≤ 40`; при `T ≥ 60` у Opus появляется регресс ниже 4.0.

**Адаптивный target — правильная формула (не «2×jitter» в лоб):** `T = clamp(peak_to_peak_jitter + 10 мс, 20, 60)`:

| Профиль | p-p джиттер | T | P_eff | MOS G.711 | MOS Opus |
|---|---:|---:|---:|---:|---:|
| чистая сеть | 0 | 20 мс | 2.00 % | 4.14 | 4.02 |
| стенд | 30 мс | 40 мс | 2.00 % | 4.13 | 4.00 |
| плохой Wi-Fi | 60 мс | 60 мс | 2.00 % | 4.11 | 3.98 |

`2·J` из RFC 3550 использовать **нельзя как target напрямую**: `J` — сглаженное среднее отклонение (для `U[15,45]` ≈ 10 мс), `2·J = 20 мс` дало бы ровно худший режим из таблицы (1.82/1.57). Нужен **p99 разброса прибытия относительно минимума** (peak-to-peak), а `J` — только для детекции «джиттер вырос» и как вход сглаживания.

### 7.4. C) Как измерять потери класса A (`loss < 0.1 %`) на нашем стенде

**Стенд «боты + MCU на одной ВМ через veth»:** «проводов» нет, поэтому физических потерь быть не должно; любая ненулевая потеря = баг (это делает стенд идеальным **регрессионным гейтом**), а 0.1 % проверяется только с `netem loss 0.1%` на veth. Значит нужны **два разных** измерения: (1) нулевой baseline (должно быть ровно 0), (2) инжектированные 0.1 % (должно быть ≈0.1 ± точность).

**Метрики и формулы (для парсера perf-lab):**

1. **RTP seq-gap на приёмнике (бот), по каждому SSRC** — основная метрика:
   - `expected = (last_ext_seq − first_seq + 1)` с расширением 16-битного seq (логика RFC 3550 A.1);
   - `received = count(distinct seq)`, `dup = total − received`;
   - `lost_pre_jb = expected − received`;
   - `lost_post_jb = lost_pre_jb − repaired_by_jb − concealed` (то, что реально слышно как артефакт);
   - `loss_ratio = lost_pre_jb / expected`.
2. **Разложение по источнику (обязательно, иначе цифра бесполезна):**
   `sent_by_bot == received_app + loss_wire + loss_kernel + loss_app`,
   где `loss_kernel` = Δ(`Udp.RcvbufErrors`) + Δ(`softnet_stat` dropped) + Δ(`/proc/net/udp` socket drops), `loss_app` = `rejected` (reorder/dup/PT/SSRC — `RtpSource`) + `late` (JB) + `tick_missed` (`MissedTickBehavior::Skip`, по сессии; микс комнаты защищён guard'ом `engine.rs:264-268`), `loss_wire` = Δ(sent) − Δ(tshark `-Y rtp` count на veth). **Остаток (identity не сходится) — это баг, его и печатать в отчёт.**
3. **RTCP-форма (эквивалент RR, RFC 3550 §6.4.1)** — чтобы цифры совпадали с индустриальными:
   `cumulative_lost = expected − received` (24-bit signed), `fraction_lost = ((expected_interval − received_interval) << 8) / expected_interval`. Мы RR **не отправляем** (`grep 'rtcp_socket.recv'` → 0), поэтому бот считает это сам и отдаёт в Prometheus: `vks_rtp_loss_ratio{ssrc,phase="wire|kernel|app|post_jb"}`, `vks_rtp_expected_total`, `vks_rtp_late_total`.
4. **Статистическая достаточность (это главное, что нужно в парсер):**
   - **Сертификация «0 потерь»**: правило трёх — при 0 событиях на `n` пакетах 95 % верхняя граница = `3/n`; `< 0.1 %` требует **`n ≥ 3000` пакетов** (при 50 pps — 1 мин, при 240 pps — 12.5 с) **и ноль потерь**.
   - **Оценка ненулевых 0.1 %**: для ±10 % относительной точности при `p = 0.001` нужно `n = (1.96/0.1)²·(1−p)/p ≈ 384 000` пакетов (50 pps → 2.1 ч; 240 pps → 27 мин). Для ±20 % — ~96 000.
   - **Гейт класса A:** прогон валиден, только если `n ≥ 3000` **и** (`lost == 0` **или** `n ≥ 384 000`); иначе печатать «недостаточно выборки», а не «loss = 0 %».
5. **Почему не «просто tshark»:** tshark на veth видит **wire**-сторону и не видит app-дропы (reorder/late/tick) — на нашем коде он покажет wire-loss и «0» потерь приложения, что методологически неверно. tshark — только арбитр для `loss_wire`.
6. **Дополнительно (если появится XR):** RFC 3611 §4.1 Loss RLE даёт burst/gap density — единственный способ отличить Bernoulli 0.1 % от серии 4 подряд (для нас критично, см. §0б). Пока `grep -c 'XrBlock'` → 0, бот может считать простейший burst-детектор по seq-gap’ам сам (`max_run`, `gap_count`).

**Итог для perf-lab:** в парсер — 4 счётчика на SSRC (`expected`, `received_distinct`, `late`, `rejected`) + дельты kernel-счётчиков + `n`; формула `loss_ratio = (expected − received_distinct)/expected`; обязательный вывод `n` и признака достаточности выборки рядом с любой цифрой потерь.

---

## Приложение A. Сводка «механизм → статус → приоритет»

| # | Механизм | Статус у нас | Где | P0/P1 | RFC |
|---|---|---|---|---|---|
| 1 | `recvmmsg`/`sendmmsg`, батчинг, `SO_RCVBUF` | **нет** | `udp_loop.rs:160,210-215` | P0(buff)/P1(batch) | RFC 8085 §3.1.6 |
| 2 | `SO_REUSEPORT` + N воркеров | **нет** | `udp_loop.rs:110` | P1 | — |
| 3 | Adaptive jitter buffer | **нет** (мосты) / fixed 200 мс (str0m) | `gateway_audio.rs:587-592`; `webrtc.rs:51` | P0 | RFC 3550 A.8, RFC 3611 |
| 4 | NACK + RTX | **есть** у str0m / **нет** у шлюзов | str0m; `grep rtcp_socket.recv`=0 | P0 (шлюзы) | RFC 4585 §6.2, RFC 4588 |
| 5 | FEC (ULPFEC/flexfec/Opus LBRR) | **нет** ULPFEC; Opus LBRR частично | `opus.rs:43-49,79-90`; `gateway_audio.rs:103-117` | P1 | RFC 5109, 8627, 6716 §4.5.3 |
| 6 | PLC G.711/G.722 | **нет** (только Opus) | `gateway_audio.rs:105` | P0 | G.711 App. I, G.722 App. III |
| 7 | TWCC/REMB | **есть** у str0m; `bwe.rs` — заглушка | `bwe.rs:149-165`; `webrtc.rs:344` | P1 | RFC 8888, RFC 3611 |
| 8 | Pacer | **есть** у str0m; **нет** у шлюзов/пути A | `gateway_video.rs:601-602` | P0 (видео) | RFC 8085 §3.1.6 |
| 9 | DSCP | **нет** | — | P1 | RFC 2474, Cisco `A6a:338-346` |
| 10 | Наблюдаемость потерь (XR/метрики) | **нет** (`UdpMediaStats` не экспортируется) | `udp_loop.rs:22-31`; `metrics.rs:48-51` | P0 | RFC 3611 §4.7 |

## Приложение B. Доказательная база (команды и результаты, снятые 2026-10-25)

```
sysctl net.core.rmem_max wmem_max netdev_max_backlog netdev_budget netdev_budget_usecs \
       net.ipv4.udp_mem udp_rmem_min udp_wmem_min ip_local_port_range somaxconn
 → 4194304 / 4194304 / 1000 / 300 / 2000 / 1383144 1844193 2766288 / 4096 / 4096 / 32768 60999 / 4096
ulimit -n → 524288 ;  /proc/1177177/limits → Max open files 65536
ss -m -lunp → :40000  skmem:(r0,rb212992,t0,tb212992,...) ; :5060 то же ; :43107 python3
ss -lntup → udp 0.0.0.0:40000 (vks-server fd=12), 0.0.0.0:5060 (fd=13), python3 *:43107
ip -s link show ens18 → RX 2274818 packets, dropped 2030; TX 1854501, dropped 0
awk softnet_stat → processed=17287714 dropped=0 time_squeeze=38
/proc/net/snmp Udp → InErrors 456, RcvbufErrors 456 ; /proc/net/udp → все дропы на :14E9 (5353/mDNS), :9C40 (40000) drops=0
tc qdisc show dev ens18 → fq_codel 0: root limit 10240p flows 1024 quantum 1514 target 5ms interval 100ms memory_limit 32Mb ecn
ethtool -k ens18 → virtio_net; rx-checksumming on; LRO off[fixed]; receive-hashing off[fixed]; tx-udp-segmentation on
ethtool -g ens18 → RX 1024, TX 256 ; ethtool -l ens18 → Combined 1
ethtool -i ens18 → driver virtio_net 1.0.0, bus 0000:00:12.0
/sys/class/net/ens18/queues/rx-0/rps_cpus = 0,00000000 ; rps_flow_cnt = 0 ; net.core.rps_sock_flow_entries = 0
ip -d link show ens18 → numtxqueues 1 numrxqueues 1 qlen 1000 mtu 1500
/proc/1177177/cgroup → 0::/user.slice/user-1000.slice/user@1000.service/app.slice/vks-mcu.service
/sys/fs/cgroup/cgroup.controllers → cpuset cpu io memory hugetlb pids rdma misc dmem  (net_cls/net_prio НЕТ)
systemctl show vks-mcu → CPUQuotaPerSecUSec=infinity MemoryMax=infinity TasksMax=69375 Nice=0
/proc/1177177/environ → VKS_WEBRTC_TRANSPORT=1 VKS_WEBRTC_PORT_MIN=43000 VKS_WEBRTC_PORT_MAX=43100
  VKS_SIP_UAS_ENABLED=1 VKS_H323_UAS_ENABLED=1 VKS_H323_UAS_MAX_CALLS=20 VKS_CANDIDATE_ADDR=192.168.1.93
/proc/1177177/task → 38 потоков (36 tokio-rt-worker)
curl :8080/metrics | grep -i media → только vks_webrtc_* (vks_media_packets_dropped_total отсутствует)
grep -rn "SO_RCVBUF|setsockopt|recvmmsg|sendmmsg|SO_REUSEPORT" crates/ → 0 совпадений
grep -rn "recv_from" crates/ → udp_loop.rs:160, webrtc_room.rs:491, gateway_audio.rs:268,
  gateway_video.rs:687, gateway_aac.rs:188, gateways.rs:2200 (сигналинг), uac.rs (SIP)
grep -rn "rtcp_socket.recv" crates/ → 0 совпадений (RTCP не читается)
modinfo sch_netem → /lib/modules/7.0.0-38-generic/kernel/net/sched/sch_netem.ko.zst (есть; не применялся)
id -u → 1000 (root нет ⇒ tc/netns в рамках этой сессии не выполнялись — «не проверено»)
```

## Приложение C. Что нужно от Lead/соседей по цеху

- **media-core/композитор:** подтвердить расчёт O(N²) микшера и O(пиксели) композитора для 500 — нужен общий вывод, что 500 в MCU-режиме недостижимы на CPU и сетевой бюджет (750 Мбит/с egress при 500×1.5 Мбит/с) — отдельная стена.
- **sip/h323-агент:** подтвердить, что H.323-плечи идут через `gateway_audio.rs`/`gateway_video.rs` (а не через str0m), т.е. NACK/JB/FEC для них надо строить в Rust-мосте.
- **deploy-агент:** включить `deploy/sysctl/60-vks-media.conf` (§3.2) в поставку; `LimitNOFILE=262144`; портовые диапазоны в unit/env.
- **signalling-агент:** `MAX_PEERS_PER_ROOM=2` и `VKS_WEBRTC_PORT_MIN/MAX=43000/43100` — это SDP/ICE-контракт; расширение диапазона должно быть согласовано с firewall-документацией.
