# T07d — входящий H.323 UAS (Q.931/H.225/H.245)

Дата: 2026-10-22. Стенд: `vks-mcu.service`, `127.0.0.1:8080`, сессия
`4d313d01-8300-450c-ab08-28c8a00f9f25` («Addr Inspect», alias `addr-inspect`).

## Что сделано

- `crates/vks-gateway-h323/python/h323_listener.py` — H.323 UAS:
  Q.931 Setup/Release, H.225 Setup (alias, callIdentifier, conferenceID),
  CallProceeding + Connect с отдельным H.245 address, H.245 TCS/MSD/OLC,
  PT 0 (G.711 µ-law) и PT 96 (H.264), EndSession/Release.
  Родитель принимает TCP 1720, на каждый вызов `fork()`; дочерние события
  идут через control-socket, stdout пишет только родитель.
- `crates/vks-gateway-h323/src/uas.rs` — Rust-обёртка процесса, readiness,
  события, accept/reject/hangup, process group, bounded shutdown.
- `crates/vks-server/src/gateways.rs` — `serve_h323_uas` и
  `serve_h323_uas_session`: разрешение комнаты (UUID / `dtmf` / `route` /
  `alias` / имя / `VKS_H323_UAS_DEFAULT_ROOM`), RTP-порты, audio/video
  bridges, RoomHub/MCU, registry, admin hangup, watchdog, cap и graceful stop.
- `crates/vks-server/src/main.rs` — запуск UAS только при
  `VKS_H323_UAS_ENABLED=1`; остановка через `begin_shutdown()` до любых await.
- Security: opt-in, `VKS_H323_UAS_ALLOW` CIDR allowlist, лимит вызовов
  (`VKS_H323_UAS_MAX_CALLS`, по умолчанию 20), лимит дочерних процессов
  (`VKS_H323_UAS_MAX_CHILDREN=64`, `VKS_H323_UAS_MAX_PER_IP=8/20`), запрет
  redirect Q.931 Connect/H.245 на чужой IP, проверка source H.245, запрет
  PCMA (асимметрия), peer IP == media IP, audio source latch ОТКЛЮЧЁН для H.323
  (нельзя перехватить egress первым UDP-пакетом), TTL pending 30 с + жёсткий
  fallback 10 с, media watchdog 90 с; allowlist fail-closed (пустой
  `VKS_H323_UAS_ALLOW` не запускает listener).
- Lifecycle: один reaper (`waitpid(-1)`) без stale PID, идентичный
  `release_child` для EOF/waitpid (дренаж событий, failed ровно один раз,
  peer_counts один раз), parent SIGKILL зависшего child через 5 с после
  hangup; Rust читает события только в отдельной задаче (read_until не
  отменяется ticker'ом), pending-lease живут до остановки listener-дерева;
  все 20 отключений убираются конкурентно (0.54 с).
- systemd `KillMode=mixed`: SIGTERM получает только vks-server, supervisor
  сам освобождает вызовы; sample env `VKS_H323_UAS_ENABLED=0`.

## Живые проверки

1. 20 одновременных входящих H.323-вызовов (20 UAC-sidecar на
   `192.168.1.93:1720`), все 20 дошли до `connected` с audio+video:
   - `abonents-20-summary.json`: `connected=20`, `protocols=["h323"]`,
     `with_video=20`;
   - `server-landing-20.log`: 20 строк `H.323 inbound participant landed`;
   - `listener-processes-20.txt`: 20 forked children, `zombies_before_cleanup=0`,
     после kill UAC — `children_after_cleanup=0`.
2. UI: `session-two-h323.png` — два входящих H.323-участника в сессии,
   кнопки audio/video активны.
3. Graceful restart с активным вызовом (KillMode=mixed): UAC получил
   `disconnected reason=remote_end_session`, сервис поднялся, новый listener
   один; посреди shutdown новых `H.323 UAS listening` нет
   (`graceful-shutdown-uac.log`, `watch3`).
4. Admin kick: `DELETE /api/v3/sessions/{id}/abonents/{pid}` → 204,
   участник удалён, listener шлёт Release; registry очищен.
5. Автотесты: `cargo test --workspace --all-targets` — 1078 passed / 0 failed /
   6 ignored; `test_h323_listener` 6 + `test_h323_sidecar` 24 — 30 passed;
   clippy workspace clean.

## Ограничения

- Проверка с реальным терминалом Polycom `192.168.1.132` и reference MCU
  `192.168.1.92` не выполнялась в этом прогоне (синтетический UAC и loopback
  H.245); SIP UAS остаётся основным путём для reference MCU.
- H.323 — plaintext, без H.235; включать только в доверенной LAN через
  `VKS_H323_UAS_ALLOW`.
- H.245 tunneling/fastStart не реализованы: терминал должен открыть
  отдельный H.245 TCP, как Polycom/TrueConf на этом стенде.

## Воспроизведение

```bash
# сервис
VKS_H323_UAS_ENABLED=1 VKS_H323_UAS_ALLOW=192.168.1.0/24 \
  VKS_H323_UAS_MAX_CALLS=20 VKS_H323_UAS_MAX_PER_IP=20 \
  /home/pro/mcu/target/release/vks-server --config /home/pro/mcu/configs/default.toml

# 20 вызовов: /tmp/h323_uas_parallel.py (N=20), 20 sidecar-процессов,
# target_alias=4d313d01-8300-450c-ab08-28c8a00f9f25
```
