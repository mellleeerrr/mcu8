# ROOT_COMMANDS — ровно то, что требует root (и ничего больше)

Составлено после эмпирической проверки 2026-10-25 17:2x (uid=1000, sudo без пароля недоступен).
Проверено **без выполнения** root-команд: `sudo -n true` → отказ; `unshare -rn true` → отказ
(`/proc/self/uid_map: Операция не позволена`, `apparmor_restrict_unprivileged_userns=1`).
Поэтому netns/netem/cpuset **нельзя** получить непривилегированно — это не пессимизм, а факт.

Каждый блок: зачем → команды → проверка → откат. Все команды идемпотентны (повторный запуск
не ломает стенд) и не трогают конфиги сервиса на диске (`--runtime`/прямые записи в cgroup).

---

## A. [ОТМЕНЕНО 18:00] Делегировать cpuset пользовательскому менеджеру

> **Не выполнять.** Lead проверил дважды: даже после делегирования cpuset по всей цепочке
> (`user.slice` → `user-1000.slice` → `user@1000.service`, `cgroup.subtree_control` содержит `cpuset`,
> у cgroup `cpuset.cpus=0-35`) `systemd-run --user --scope -p AllowedCPUs=16-19` **молча игнорируется**:
> у процесса `Cpus_allowed_list: 0-35`. Изоляция ставится через `taskset` (см. §B).
> Блок оставлен как запись отрицательного результата — не тратить на него время.

### Историческая справка (не выполнять)

**Зачем.** `AllowedCPUs=` на user-юнитах сейчас **молча игнорируется**: поймано на живом
эксперименте — `systemd-run --user --scope -p AllowedCPUs=16-35 --unit=test-cpuset-b sleep 5`
создал scope без контроллера cpuset, у процесса `Cpus_allowed_list: 0-35`, файлов
`cpuset.cpus` в cgroup нет. Причина: в `user@1000.service/cgroup.subtree_control` только
`cpu memory pids`; чтобы включить cpuset в поддереве, у этого cgroup должны быть
непустые `cpuset.cpus`/`cpuset.mems` (особенность cpuset v2).

После этой правки изоляция ядер ставится **без root** — и для ботов, и для MCU — командами
`systemctl --user set-property` / `systemd-run --user --scope` (см. §B). Это резко сокращает
число root-итераций: одна правка вместо root-команд на каждый прогон.

```bash
U=/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service
sudo sh -c "echo 0-35 > $U/cpuset.cpus"
sudo sh -c "echo 0    > $U/cpuset.mems"
sudo sh -c "echo +cpuset > $U/cgroup.subtree_control"
```

Проверка (может выполняться без root, сразу после):

```bash
cat /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/cgroup.subtree_control
# ожидается: cpu memory pids cpuset
systemd-run --user --scope -p AllowedCPUs=16-35 --unit=test-cpuset-ok --quiet sleep 5 &
sleep 1
CG=/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/test-cpuset-ok.scope
cat "$CG/cpuset.cpus"                      # ожидается: 16-35
PID=$(head -1 "$CG/cgroup.procs"); grep Cpus_allowed_list /proc/$PID/status   # 16-35
wait
```

Откат (полный, возвращает исходное состояние):

```bash
U=/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service
sudo sh -c "echo -cpuset > $U/cgroup.subtree_control"
sudo sh -c "echo '' > $U/cpuset.cpus" 2>/dev/null || true
```

---

## B. Изоляция ядер: MCU 0-15, боты 16-35 (после §A — без root)

**Зачем.** Без изоляции `c_device` «плывёт»: MCU и боты конкурируют за одни ядра,
`%steal`/`cpu.pressure` растут, дисперсия между итерациями убивает доверие к цифрам.
Это мой главный методический аргумент (§2.2 отчёта `06_perf_lab.md`).

**Рабочий способ — `taskset` (проверено Lead'ом; root не нужен):**

```bash
# MCU на 0-15: применяется к уже запущенному процессу, рестарт не нужен
taskset -acp 0-15 $(pgrep -x vks-server)      # все 39 потоков переезжают
# проверка (новые FFmpeg-дети наследуют affinity родителя):
PID=$(systemctl --user show -p MainPID --value vks-mcu.service)
grep Cpus_allowed_list /proc/$PID/status      # 0-15
for c in $(pgrep -P $PID ffmpeg); do grep Cpus_allowed_list /proc/$c/status; done

# боты на 16-35 (matrix.py сам меряет MCU; --bot-cores оборачивает каждого бота в taskset):
python3 scripts/bench/series.py --ns 1,2,5,10,20 --bot-cores 16-35 --duration 60 --room <UUID>
```

Откат:

```bash
taskset -acp 0-35 $(pgrep -x vks-server)
grep Cpus_allowed_list /proc/$(systemctl --user show -p MainPID --value vks-mcu.service)/status  # 0-35
```

Важно: (1) `pgrep -x vks-server` может поймать debug-инстансы — для MCU бери
`systemctl --user show -p MainPID --value vks-mcu.service`; (2) `taskset -acp` меняет affinity
и уже запущенных потоков, и будущих детей, но **не** даёт `cpu.stat`-изоляции: accounting всё равно
по cgroup юнита, это и есть методика (§3.2); (3) боты в netns пиннить так же — `taskset` работает
внутри `ip netns exec`.

---

## C. Сеть: netns + veth + netem (root; только если нужна ось «сеть» — классы B/C)

**Зачем.** Без этого SLA-классы B/C (loss 0.5–2 %, jitter 30 мс, RTT 150/300 мс) не проверить:
loopback даёт 0 потерь и <0.1 мс, т.е. измерим только класс A.
`unshare -rn` недоступен (проверено), поэтому только root.

```bash
# C1. Создать (идемпотентно)
ip netns list | grep -qx vks-bench || sudo ip netns add vks-bench
ip link show veth-bench >/dev/null 2>&1 || sudo ip link add veth-bench type veth peer name veth-mcu
sudo ip link set veth-mcu netns vks-bench 2>/dev/null || true
sudo ip addr add 10.200.0.1/24 dev veth-bench 2>/dev/null || true
sudo ip link set veth-bench up
sudo ip netns exec vks-bench ip addr add 10.200.0.2/24 dev veth-mcu 2>/dev/null || true
sudo ip netns exec vks-bench ip link set veth-mcu up
sudo ip netns exec vks-bench ip link set lo up

# C2. Проверка связности до MCU (SIP 192.168.1.93:5060)
sudo ip netns exec vks-bench ping -c1 -W1 192.168.1.93
sudo ip netns exec vks-bench timeout 2 bash -c 'echo > /dev/udp/192.168.1.93/5060' && echo "udp ok"
```

Запуск ботов внутри netns (**нужен root**: `ip netns exec` без CAP_SYS_ADMIN даёт
`setting the network namespace "vks-bench" failed: Operation not permitted` — проверено).
**Решено (Lead, 18:5x): NOPASSWD на `/usr/sbin/ip` и `/usr/sbin/tc`** (`/etc/sudoers.d/vks-t2`,
`visudo -c` OK), поэтому всю серию T2 perf-lab гоняет сам — через готовый драйвер:

```bash
# 6 профилей + паспорт-гейт + precheck/postcheck; --plan — только напечатать команды
scripts/bench/t2_matrix.sh <expect-sha256> /tmp/vks-bench/t2-iter6
scripts/bench/t2_matrix.sh <expect-sha256> <outdir> --only T2-4b,T2-5b   # переигровка подмножества
```

Драйвер сам делает: `ip netns exec … sh -c 'umask 022; exec python3 matrix.py …'`
(umask обязателен — иначе файлы root-600 и сводка молча пустая), `--api 192.168.1.93:8080`,
`--mcu-pid` из `systemctl --user show -p MainPID`, `--expect-sha256`, дампы `tc -s qdisc` до/после
в `tc-host.txt`/`tc-netns.txt` как доказательство применённого профиля.

**Важно про `limit`:** при задержке 75/150 мс и ~3.6 тыс. pps дефолтный `netem limit 1000`
теряет пакеты **сам** (`tc`: `dropped` 3540/4535) — бот видит 1.9–2.5 % «потерь», которых MCU
не делал. Для delay-профилей ставить `limit 100000` (профили `T2-4b`/`T2-5b` в драйвере).

netem — **направления важны**:

```bash
# MCU -> бот: qdisc на хостовой стороне veth (egress из хоста в netns)
sudo tc qdisc replace dev veth-bench root netem loss 2%            # loss-профили
sudo tc qdisc replace dev veth-bench root netem limit 100000 delay 75ms   # delay-профили
# бот -> MCU: то же внутри netns (одна переменная за раз — ровно те же аргументы)
sudo ip netns exec vks-bench tc qdisc replace dev veth-mcu root netem loss 2%
sudo ip netns exec vks-bench tc -s qdisc show dev veth-mcu
sudo tc -s qdisc show dev veth-bench
```

**Протокол T2 (одна переменная за раз, с откатом между профилями):**

| # | Профиль | qdisc (veth-bench = MCU→бот; veth-mcu = бот→MCU) | Что смотрим |
|---|---|---|---|
| T2-0 | baseline | `tc qdisc del … root` (чисто) | контроль: loss 0, jitter как в §9 |
| T2-1 | loss0.5 | `netem loss 0.5%` на **обоих** концах | loss_pct, MOS, `keyframe_requests` |
| T2-2 | loss2 | `netem loss 2%` на **обоих** концах | деградация MOS, `dropped_frames`, рестарты |
| T2-3 | delay30 | `netem delay 15ms 5ms distribution normal` на **обоих** (сумма RTT ≈ 30±10 мс) | jitter_ms, `jb_late_total`, MOS |
| T2-4 | delay150 | `netem delay 75ms` на обоих (RTT ≈ 150 мс) | класс B; MOS, JB |
| T2-5 | delay300 | `netem delay 150ms` на обоих (RTT ≈ 300 мс) | класс C; MOS→2, `jb_late_total` |

После каждого профиля — `sudo tc qdisc del dev veth-bench root; sudo ip netns exec vks-bench tc qdisc del dev veth-mcu root;`
и `tc -s qdisc show` в лог; только потом следующий профиль. `--expect-sha256` — новый sha от Lead.

Откат:

```bash
sudo tc qdisc del dev veth-bench root 2>/dev/null || true
sudo ip netns exec vks-bench tc qdisc del dev veth-mcu root 2>/dev/null || true
sudo ip netns del vks-bench 2>/dev/null || true          # veth-mcu умирает вместе с netns
sudo ip link del veth-bench 2>/dev/null || true
```

Ограничение, которое надо принять явно: SIP-боты из netns приходят на MCU с адресом
10.200.0.2. SIP UAS **не имеет** allowlist (в отличие от H.323) — проверено по `uas.rs`,
поэтому это допустимо; H.323-боты при этом обязаны остаться на 127.0.0.1
(`VKS_H323_UAS_ALLOW=127.0.0.1/32,192.168.1.0/24`), т.е. ось «сеть» для H.323 в этом
раунде недоступна без рестарта сервиса.

---

## D. Чего root НЕ даёт (чтобы не тратить итерации)

| Что | Почему нельзя |
|---|---|
| Поднять `MAX_CALLS`/пул портов H.323 и SIP | это env сервиса, нужен рестарт `vks-mcu` → решение Lead, не root-команда |
| Увеличить пул RTP 41000-41200 | хардкод `rtp_bridge.rs:20-22`, только правка кода |
| Включить SRTP/TLS/BFCP/H.239 | нет в коде (488 / заглушка), root не помогает |
| `perf stat -p <mcu>` | уже доступно: `perf_event_paranoid=1`, MCU под тем же uid |
| `cpupower frequency-set` | в KVM-госте обычно нет; если есть — не использовать, меняет масштаб частот и ломает сравнение серий |

---

## E. Порядок для Lead (что от тебя нужно)

**Снято полностью:** A (cpuset-делегирование) отменён — изоляция ставится `taskset -acp`;
C1-C2 (netns `vks-bench`, veth, маршрут) подняты, NOPASSWD на `ip`/`tc` выдан, T2 снят на
«Итерации 6» (`95cc78fc…`). От Lead по-прежнему нужны только: **новый замороженный sha**
и подтверждение окна (loadavg < 8); рестарт сервиса — решение Lead (сбрасывает паспорт, §3.6).
