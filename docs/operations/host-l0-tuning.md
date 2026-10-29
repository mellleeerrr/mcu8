# L0: host tuning (sysctl, cgroup v2, UDP buffers, RT)

**Task:** T15 · **Статус:** артефакты и код готовы, живой хост **не тронут**
(нет passwordless sudo) · **Проверка:** `scripts/host-tune.sh check`,
`scripts/host-doctor.sh`.

Документ описывает версионируемый L0-профиль MCU: сетевые sysctl, cgroup v2
лимиты systemd и настройку буферов UDP-сокетов в коде. Всё, что требует root,
применяется на T17 (окно рестарта) — T15 только готовит и проверяет артефакты.

---

## 1. Что где лежит

| Артефакт | Назначение |
|---|---|
| `deploy/host/sysctl/90-vks-mcu.conf` | UDP rmem/wmem, softnet backlog/budget, ephemeral ports + reserved media ports |
| `deploy/host/systemd/vks.slice` | cgroup v2 slice `vks.slice`: `MemoryHigh/MemoryMax/CPUWeight/IOWeight/TasksMax` |
| `deploy/host/systemd/vks-mcu-prod.service.d/10-l0-resources.conf` | drop-in для `/etc/systemd/system/vks-mcu-prod.service` |
| `deploy/host/systemd/user/vks-mcu.service.d/10-l0-resources.conf` | drop-in для dev-стенда `~/.config/systemd/user/vks-mcu.service` |
| `deploy/systemd/vks-mcu-prod.service` | прод-юнит: те же `Slice/Memory*/CPUWeight/IOWeight/Nice` + `EnvironmentFile=/etc/vks/vks.env` |
| `scripts/host-tune.sh` | идемпотентные `apply / check / rollback / status` с `--dry-run` |
| `scripts/host-doctor.sh` | read-only проверка *эффективных* значений (sysctl, cgroup, NOFILE, `SO_RCVBUF`), exit 1 при расхождении |
| `crates/vks-common/src/net_tuning.rs` | `SO_RCVBUF`/`SO_SNDBUF` через `socket2` (без `unsafe`), env-конфиг, метрики |
| `crates/vks-gateway-sip/src/rtp_bridge.rs` | применяет буферы к per-call RTP/RTCP-сокетам SIP-моста |

Никакой «магии» в живом `~/.config/systemd/user/vks-mcu.service` нет: T15
**не перезаписывает** live-юнит и не добавляет drop-in; всё лежит в `deploy/`.

---

## 2. Требования к хосту

* systemd **259** (проверено: `systemd 259 (259.5-0ubuntu3.4)`), cgroup v2
  (`/sys/fs/cgroup/cgroup.controllers` содержит `memory cpu io`).
* root/sudo для sysctl и system-юнитов. Проверить **passwordless** sudo:

  ```bash
  sudo -n true && echo "passwordless sudo: yes" || echo "passwordless sudo: no"
  ```

  На текущем стенде — `no`: применять sysctl нельзя без интерактивного пароля.
  `host-tune.sh` в этом случае отказывается с exit 2, а не «молча ничего не делает».
* Утилиты: `sysctl`, `systemctl`, `ss` (iproute2), `systemd-analyze`.
  `shellcheck` опционален (на стенде отсутствует).

Делегирование контроллеров user-менеджеру (важно для dev-drop-in):

```bash
cat /sys/fs/cgroup/user.slice/user-$(id -u).slice/user@$(id -u).service/cgroup.controllers
# на стенде: cpuset cpu memory pids   <- io НЕ делегирован
```

Поэтому в user drop-in **нет** `IOWeight` (он был бы молча неэффективен) и
`Nice` только `0` (у user-менеджера `RLIMIT_NICE=0`). Реальный IO-вес и
отрицательный nice — только system-юнит.

---

## 3. Сетевые sysctl

Профиль: `deploy/host/sysctl/90-vks-mcu.conf`.

| Ключ | Значение | Зачем |
|---|---|---|
| `net.core.rmem_max` | 16 MiB | потолок `SO_RCVBUF`; дефолт 208 KiB обрезает все запросы кода |
| `net.core.wmem_max` | 16 MiB | потолок `SO_SNDBUF` |
| `net.core.rmem_default` / `wmem_default` | 1 MiB | сокеты без `setsockopt` (Python H.323 sidecar) |
| `net.core.netdev_max_backlog` | 5000 | очередь softnet на всплесках RTP/ICE |
| `net.core.netdev_budget` / `_usecs` | 600 / 8000 | NAPI-бюджет на 36-ядерном хосте |
| `net.ipv4.ip_local_port_range` | `10240 65000` | много исходящих RTP/STUN/TURN-сокетов |
| `net.ipv4.ip_local_reserved_ports` | `40000,41000-41199,43000-43100` | эфемерный порт не должен занять порт медиаплоскости |

`ip_local_reserved_ports` обязателен: без него исходящее TCP/UDP-соединение
может занять `:43000` за миг до `bind` ICE-сокета → `port_range_exhausted` и
отказ пиру. При смене `VKS_WEBRTC_PORT_MIN/MAX` или пула SIP RTP
(`41000-41199`) список нужно обновить в одном месте — в этом файле.

### Применение / проверка / откат

```bash
# только посмотреть, без root и без изменений
scripts/host-tune.sh apply --dry-run
scripts/host-tune.sh check          # exit 0 = все значения совпали

# применить (root)
sudo scripts/host-tune.sh apply
sudo sysctl --system                # делает сам скрипт; команда для наглядности

# откатить: удалить файл и восстановить значения, снятые перед первым apply
sudo scripts/host-tune.sh rollback
```

Идемпотентность:

* второй `apply` печатает `unchanged` и не переписывает файл (сравнение `cmp`);
* перед первым изменением скрипт сохраняет **текущие** значения в
  `/var/lib/vks/host-tune.previous` (переопределяется
  `VKS_HOST_TUNE_STATE_FILE`); `rollback` удаляет файл, делает
  `sysctl --system` и возвращает снятые значения через `sysctl -w`;
* чужой файл без маркера `Managed by scripts/host-tune.sh` не удаляется без
  `--force`;
* `rollback` без state-файла (например, после reboot) честно предупреждает:
  значения останутся до перезагрузки — вернуть вручную.

**На T15 live sysctl не применялись.** Read-only срез на момент работы:

```
net.core.rmem_max = 4194304          (профиль: 16777216)
net.core.wmem_max = 4194304          (профиль: 16777216)
net.core.rmem_default = 212992       (профиль: 1048576)
net.core.wmem_default = 212992       (профиль: 1048576)
net.core.netdev_max_backlog = 1000   (профиль: 5000)
net.core.netdev_budget = 300         (профиль: 600)
net.core.netdev_budget_usecs = 2000  (профиль: 8000)
net.ipv4.ip_local_port_range = 32768 60999 (профиль: 10240 65000)
net.ipv4.ip_local_reserved_ports = <empty>  (профиль: 40000,41000-41199,43000-43100)
```

`scripts/host-doctor.sh` на этих значениях корректно падает с exit 1 и
перечисляет 9 расхождений — это и есть доказательство, что доктор не
«всегда зелёный».

---

## 4. systemd 259 / cgroup v2

### 4.1 Prod (system unit)

`/etc/systemd/system/vks-mcu-prod.service` (в репозитории —
`deploy/systemd/vks-mcu-prod.service`) уже содержит:

```ini
EnvironmentFile=/etc/vks/vks.env   # fail-closed: без файла юнит не стартует
Slice=vks.slice
MemoryHigh=2G
MemoryMax=3G
CPUWeight=200
IOWeight=100
LimitNOFILE=65536
Nice=-5
CPUSchedulingPolicy=other
```

Slice (`/etc/systemd/system/vks.slice`): `MemoryHigh=2G`, `MemoryMax=3G`,
`CPUWeight=200`, `IOWeight=200`, `TasksMax=8192`. Лимиты дублируются на юните
намеренно: установка одного юнита без drop-in всё равно даёт потолок.

Установка (T17, с рестартом — это отдельное окно):

```bash
sudo install -D -m 0644 deploy/host/systemd/vks.slice /etc/systemd/system/vks.slice
sudo install -D -m 0644 deploy/host/systemd/vks-mcu-prod.service.d/10-l0-resources.conf \
  /etc/systemd/system/vks-mcu-prod.service.d/10-l0-resources.conf
sudo systemctl daemon-reload            # БЕЗ restart: лимиты применятся при следующем старте
sudo systemd-analyze verify /etc/systemd/system/vks-mcu-prod.service
systemctl show vks-mcu-prod.service -p Slice,MemoryHigh,MemoryMax,CPUWeight,IOWeight,LimitNOFILE,Nice
```

На живом сервисе без рестарта новые `MemoryMax/CPUWeight` не появятся в
cgroup — это ожидаемо; `host-doctor.sh --prod` покажет drift до рестарта.
`systemctl daemon-reload` не рвёт звонки; **restart — только T17**.

### 4.2 Dev (user unit, стенд)

```bash
scripts/host-tune.sh apply --user     # без root: ~/.config/systemd/user/.../10-l0-resources.conf
systemctl --user daemon-reload        # делает сам скрипт
systemctl --user show vks-mcu.service -p MemoryHigh,MemoryMax,CPUWeight,LimitNOFILE,Nice
```

Drop-in ставится рядом с live-юнитом, но T15 его **не устанавливал**.
Ограничения user-менеджера (см. §2): нет `IOWeight`, `Nice` только `0`.
Лимиты стенда: `MemoryHigh=1536M`, `MemoryMax=2G`, `CPUWeight=200`,
`LimitNOFILE=65536`.

### 4.3 Значения и обоснование

| Параметр | Dev | Prod | Почему |
|---|---|---|---|
| `MemoryHigh` | 1.5 GiB | 2 GiB | порог reclaim/троттлинга; текущий stand ~0.7 GiB |
| `MemoryMax` | 2 GiB | 3 GiB | жёсткий потолок (OOM вместо swap-шторма) |
| `CPUWeight` | 200 | 200 | дефолт 100; MCU важнее фонового обслуживания |
| `IOWeight` | — (нет io в user) | 100 (юнит) / 200 (slice) | ffmpeg/запись не должны голодать |
| `LimitNOFILE` | 65536 | 65536 | 1 сокет/пир + RTP/RTCP-пары + WS |
| `Nice` | 0 | -5 | латентность без RT |
| `CPUSchedulingPolicy` | other | other | см. §5 |

`MemoryMax` — не «на всякий случай побольше»: при 256 WebRTC-пирах и
encoder-группах 3 GiB — уже потолок для этого класса хоста. Если хост
легитимно требует больше, менять **в slice-файле** и в drop-in одновременно.

---

## 5. Приложение: буферы UDP и «real-time»

### 5.1 `SO_RCVBUF`/`SO_SNDBUF` (код)

Единая точка — `vks_common::net_tuning` (`crates/vks-common/src/net_tuning.rs`):

* `socket2::SockRef` (без `unsafe`), `set_recv_buffer_size`/`set_send_buffer_size`;
* фактический размер читается обратно `getsockopt` и пишется в лог +
  Prometheus: `vks_udp_rcvbuf_bytes{role="media_loop"|"gateway_rtp"}`,
  `vks_udp_sndbuf_bytes{role=...}`;
* Linux удваивает запрос: `setsockopt(N)` → `2*min(N, net.core.rmem_max)`.
  Поэтому «эффективное» значение всегда больше/равно запросу только при
  достаточном `rmem_max` — это и проверяет доктор.

Env-конфиг (значения в байтах; `0` = не трогать буфер, оставить дефолт ядра;
невалидное значение → WARN + дефолт, это тюнинг, а не fail-closed лимит):

| Переменная | Роль | Дефолт |
|---|---|---|
| `VKS_UDP_RCVBUF_BYTES` | `media_loop` | 4 MiB |
| `VKS_UDP_SNDBUF_BYTES` | `media_loop` | 1 MiB |
| `VKS_GATEWAY_UDP_RCVBUF_BYTES` | `gateway_rtp` | 512 KiB |
| `VKS_GATEWAY_UDP_SNDBUF_BYTES` | `gateway_rtp` | 512 KiB |

Где применяется сейчас:

* **SIP RTP/RTCP** — `RtpPortPool::allocate_pair` в
  `crates/vks-gateway-sip/src/rtp_bridge.rs`: каждый звонок получает новые
  сокеты; без `setsockopt` sysctl-профиль бесполезен. Сделано.
* **Per-call стоимость**: 20 звонков × 2 сокета × 512 KiB (лимит, не
  выделенная память) — подобрано так, чтобы не резервировать 4 MiB на звонок.

Где **нужно доделать на T17** (файлы вне зоны T15, их правят агенты
T11b/T11c):

* `crates/vks-media-sfu/src/webrtc_room.rs::bind_socket` — UDP-сокет на
  каждого WebRTC-пира (43000–43100). Это главная медиаплоскость после T11c
  (legacy plaintext media loop удалён). Нужен трёхстрочный хук:

  ```rust
  // after a successful std::net::UdpSocket::bind / port selection:
  let buffers = vks_common::net_tuning::UdpBufferConfig::for_gateway_rtp();
  if let Ok(effective) = vks_common::net_tuning::apply_udp_buffers(&socket, &buffers) {
      tracing::debug!(rcvbuf = effective.rcvbuf, sndbuf = effective.sndbuf, "webrtc peer UDP buffers");
  }
  ```

  Для 256 пиров 512 KiB — разумный per-peer лимит; если нужен отдельный
  дефолт, добавить роль `WebRtcPeer` в `net_tuning` (сейчас роли две).
* **H.323 sidecar (Python)** — сокеты создаёт Python; им достаётся только
  `net.core.rmem_default` (1 MiB из профиля). Явный `SO_RCVBUF` в
  `h323_listener.py` — отдельная задача владельца sidecar.

Проверка на живом процессе (после рестарта на новом бинаре):

```bash
ss -u -m -p -a | grep -A1 vks-server      # skmem:(r...,rb8388608,...)
curl -s localhost:8080/metrics | grep vks_udp_
scripts/host-doctor.sh                     # exit 0 только при совпадении
```

### 5.2 Real-time / nice — почему без `SCHED_FIFO`

Медиа-тики MCU идут на общем tokio-рантайме вместе с axum/HTTP, admin и
signalling. Асинхронный рантайм **не поддерживает** per-thread RT-классы
(нельзя «сделать RT только медиа-потокам» без собственного пула), а
`SCHED_FIFO` на tokio-воркере приводит к starvation HTTP-потоков и
priority inversion внутри процесса. Поэтому:

* `CPUSchedulingPolicy=other`, `Nice=-5` только у prod-юнита (systemd
  применяет до `DynamicUser`, root-права не нужны процессу);
* `LimitRTPRIO`/`LimitMEMLOCK` **не выставляются** — пока нет кода, который
  осознанно уходит в RT/`mlockall`, это был бы театр;
* CPU не пиннится (`CPUAffinity` не задаётся): 36 ядер, медиа и так
  распределяется планировщиком; cgroup `CPUWeight` решает приоритет без
  жёсткой привязки;
* альтернатива RT для медиа — `CPUWeight=200` + `IOWeight` + отсутствие
  своп-давления (`MemoryMax`), а не real-time.

Проверка:

```bash
chrt -p "$(systemctl show vks-mcu.service -p MainPID --value)"   # current policy: SCHED_OTHER
systemctl show vks-mcu.service -p Nice -p CPUSchedulingPolicy
```

---

## 6. Доктор: `scripts/host-doctor.sh`

Read-only, без root, exit-коды: **0** — всё совпало (skip'ы печатаются),
**1** — drift, **2** — usage/нет утилиты.

```bash
scripts/host-doctor.sh                 # dev user unit (vks-mcu.service)
scripts/host-doctor.sh --prod          # prod unit + vks.slice
scripts/host-doctor.sh --skip-socket   # пока сервис на старом бинаре без net_tuning
scripts/host-doctor.sh --require-active
```

Проверяет:

1. sysctl — через `host-tune.sh check` (все ключи профиля);
2. `systemctl show` юнита: `MemoryHigh`, `MemoryMax`, `CPUWeight`, `IOWeight`
   (если задан), `LimitNOFILE`, `Nice`, `CPUSchedulingPolicy`, `Slice`;
3. для `--prod` — значения `vks.slice` (cgroup v2);
4. `/proc/<pid>/limits` — эффективный soft `NOFILE`;
5. `ss -u -m -p -a` — фактический `SO_RCVBUF` медиа-сокетов по ролям
   (`:40000`, 43000–43100 → media_loop; 41000–41199 → gateway_rtp), дефолты
   берутся из `net_tuning.rs` (single source of truth), env — из
   `/proc/<pid>/environ`.

Текущий (read-only) прогон на живом стенде даёт exit 1: sysctl не применён,
drop-in не установлен, `MemoryMax=infinity`, `SO_RCVBUF=212992` у `:40000`.
Это ожидаемое состояние до T17 и одновременно доказательство, что доктор
ловит drift, а не «всегда зелёный».

---

## 7. Runbook T17 (root/live)

1. `scripts/host-tune.sh check` → зафиксировать drift (exit 1).
2. `sudo scripts/host-tune.sh apply` — sysctl + state-файл предзначений.
3. `sudo scripts/host-tune.sh apply --systemd` — slice + prod drop-in,
   `daemon-reload` (без restart).
4. `scripts/host-tune.sh apply --user` — dev-drop-in (если стенд).
5. Собрать/установить новый бинарь с `net_tuning`, перезапустить сервис
   (окно рестарта, звонки падают).
6. `scripts/host-doctor.sh --skip-socket` → 0 (sysctl/cgroup).
7. После первого звонка/WebRTC-пира: `scripts/host-doctor.sh` → 0
   (включая `SO_RCVBUF`); `ss -u -m` показывает `rb` ≥ запрошенного;
   `/metrics` содержит `vks_udp_rcvbuf_bytes`.
8. Откат: `sudo scripts/host-tune.sh rollback --sysctl --systemd` (и
   `--user` для стенда).

---

## 8. Тесты и dry-run (без root)

```bash
bash scripts/tests/test_host_tune.sh       # 26 тестов, 0 failed
bash -n scripts/host-tune.sh scripts/host-doctor.sh
# shellcheck на стенде не установлен — пропущен (тест это печатает)
```

Покрыто: parse/compare sysctl, fail на неверном значении и отсутствующем
ключе, `apply --dry-run` без записи, двойной `apply` (идемпотентность),
`rollback` + повторный `rollback`, отказ удалять чужой файл без `--force`,
установка/проверка/откат slice и drop-in'ов через фейковый `systemctl`,
доктор: pass на согласованных фикстурах, fail на sysctl/cgroup/`SO_RCVBUF`
drift, `--skip-socket`, `--require-active`, `--prod`.

Rust-тесты: `cargo test -p vks-common net_tuning`,
`cargo test -p vks-gateway-sip gateway_rtp_pair_applies_configured_socket_buffers`.

---

## 9. Известные ограничения

* sysctl — только root; в T15 не применялись (нет passwordless sudo).
* `net.core.rmem_max` — глобальный потолок: без него код с `setsockopt`
  получит урезанный буфер (`host-doctor` это покажет).
* user-менеджер (systemd 259, Ubuntu) не делегирует `io` и `RLIMIT_NICE=0`:
  IOWeight и отрицательный Nice только в system-юните.
* `ip_local_reserved_ports` нужно синхронизировать с фактическими диапазонами
  портов при их изменении.
* WebRTC per-peer сокеты и H.323 Python sidecar пока не вызывают
  `net_tuning` — это хук T17 для владельцев файлов (см. §5.1); без него
  `ss -u -m` для 43000–43100 останется 212 KiB.
* Доктор без `--skip-socket` на старом бинаре даёт exit 1 — это не баг, а
  drift до рестарта.
