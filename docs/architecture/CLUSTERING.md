# CLUSTERING — кластеризация и отказоустойчивость

> Статус: **v0.1 (черновик)**. Заполняется в фазе 7.
> ADR-0014 (NATS), ADR-0015 (TURN).

---

## 1. Архитектура кластера

```
                    ┌──────────────────────────────────────────┐
                    │              NATS Cluster (3 nodes)       │
                    │  JetStream (persistent) + HA-queue       │
                    │  vks.cluster.* topics                    │
                    └──────────┬───────────────────────────────┘
                               │
            ┌──────────────────┼──────────────────┐
            │                  │                  │
     ┌──────┴──────┐   ┌──────┴──────┐   ┌──────┴──────┐
     │ Media Inst. │   │ Media Inst. │   │ Media Inst. │
     │ (SFU/MCU)   │   │ (SFU/MCU)   │   │ (SFU/MCU)   │
     │ + Signaling │   │ + Signaling │   │ + Signaling │
     │ + Admin     │   │ + Admin     │   │ + Admin     │
     └──────┬──────┘   └──────┬──────┘   └──────┬──────┘
            │                  │                  │
     ┌──────┴──────────────────┴──────────────────┴──────┐
     │              PostgreSQL (primary + replica)        │
     │              Patroni (auto-failover)               │
     └───────────────────────────────────────────────────┘
     
     ┌─────────────┐
     │  TURN (×2)  │  (coturn, TLS 5349, DTLS 3478)
     └─────────────┘
     
     ┌───────────────────────────────────────────────────┐
     │  Load Balancer (TCP/UDP) → 443, 8443, 3478, 5349  │
     │  40000-60000 (RTP) — direct UDP (hairpin NAT)     │
     └───────────────────────────────────────────────────┘
```

## 2. Room ownership

- **Consistent hashing**: `room_id → instance_id`.
- **NATS lease**: TTL 30s. Instance heartbeat каждые 10s.
- **Split-brain**: если 2 инстанса claim room (network partition) → **lowest instance_id wins** (NATS lease + PostgreSQL advisory lock).
- **Room state**: метаданные (name, mode, layout, participants) — в БД. In-memory: RTCP/BWE/DTLS state (не persistent, восстанавливается через renegotiation).

## 3. Graceful shutdown

```
SIGTERM:
  1. Stop accepting new connections
  2. Active WS → ServerMaintaining{new_owner, grace_period}
  3. Media drain: RTP flush, DTLS close
  4. Storage flush: NATS queue drain
  5. Exit 0
Timeout: 30s → SIGKILL
```

## 4. Failover

| Сценарий | Детекция | Реакция | Outage |
|---|---|---|---|
| Media instance dies | NATS heartbeat 10s | Room re-assign (consistent hashing), `ServerMaintaining`, clients reconnect | 30–60s |
| NATS cluster down | Heartbeat timeout | Existing rooms continue (local), new rooms wait | 0 (existing) |
| PostgreSQL down | Connection pool retry (3×, backoff) | Active rooms continue (in-memory), new rooms blocked | 0 (existing) |
| TURN down | Client ICE timeout | Existing sessions OK, new sessions (symmetric NAT) fail | 0 (existing) |
| Signaling down | WS close | Media continues (RTP separate), clients reconnect | 5–30s |

## 5. Backpressure

- Internal queue > 10000 → drop oldest **non-critical** (layout updates), retry **critical** (join/leave).
- **DLQ**: NATS JetStream persistent queue, subject `vks.cluster.dlq.*`.
- **Idempotency**: `message_id` (UUID), дубликаты игнорируются.
- **Ordering**: per-subject (NATS гарантирует), cross-subject — eventual consistency.

## 6. Circuit breaker

| Dependency | Threshold | OPEN | Recovery |
|---|---|---|---|
| LDAP | 50% fail / 10s | 30s | 1 test request |
| PostgreSQL | 50% fail / 10s | 30s | 1 test request |
| NATS | 50% fail / 10s | 30s | heartbeat |
| TURN | 50% fail / 10s | 30s | client ICE retry |

Retry: exponential 1s→30s, max 3, jitter.
Timeout: LDAP 5s, DB 30s, NATS 5s, TURN 10s, DTLS 10s.

## 7. Resource limits (Helm)

| Component | CPU request | CPU limit | Memory | Notes |
|---|---|---|---|---|
| MCU instance | 2 | 4 | 4 GB | NUMA affinity |
| SFU instance | 1 | 2 | 2 GB | — |
| Signaling | 0.5 | 1 | 512 MB | 10k WS |
| Admin | 0.25 | 0.5 | 256 MB | — |
| PostgreSQL | 1 | 4 | 8 GB | primary |
| NATS | 0.5 | 2 | 2 GB | 3 nodes |
| TURN | 0.5 | 2 | 1 GB | ×2 |

- HPA: по metric `vks_rooms_per_instance`.
- File descriptors: 65535.

## 8. Backup / Restore

- **PostgreSQL**: pg_dump daily + WAL archiving (PITR). RPO 24h (WAL 5min), RTO 4h.
- **Recordings**: S3 daily.
- **Retention**: 30d daily + 12w weekly. Encryption: AES-256.
- **Restore test**: quarterly (автоматический).

## 9. TLS / Certificates

- **WSS/REST**: Let's Encrypt (ACME auto-renewal) или internal CA.
- **HSTS**: max-age=31536000.
- **TURN**: TLS 5349 + DTLS 3478.
- **DTLS (WebRTC)**: per-session ephemeral (не требуют rotation).
- **Min TLS 1.2**, prefer 1.3.
- **Rotation**: ACME auto-renewal, hot-reload (quinn/tokio-rustls).

## 10. NTP / Clock sync

- Все ноды: **chrony** (или ntpd).
- Metric: `vks_clock_offset_seconds` per instance.
- Alarm: offset > 100ms.
- Audit logs и записи: **UTC**. UI: local timezone.

## 11. SLO (v1)

| SLI | SLO |
|---|---|
| Signaling uptime | 99.5% |
| QoE (MOS > 3.5) per room | 99% |
| RTT p95 | < 300ms |
| Room failover time | < 60s |

## 12. Monitoring / Alerting

- Prometheus metrics per service (`/metrics`).
- QoE metrics from clients (MOS, jitter, RTT, loss).
- Grafana dashboards.
- **Alertmanager** rules:
  - CPU > 80% for 5m
  - Room ownership failover
  - JWT error rate > 1%
  - WS connections > 90% capacity
  - Clock offset > 100ms
- Channels: Telegram / email (webhook).

## 13. Runbooks (сценарии)

| # | Сценарий | Симптом | Диагностика | Действия |
|---|---|---|---|---|
| 1 | Падение media instance | NATS heartbeat miss | `nats server ls`, check logs | Room re-assign, clients reconnect. Если 2+ — scale up |
| 2 | Падение PostgreSQL | Connection refused | `pg_isready`, check replica | Patroni failover. Если replica down — restore from backup |
| 3 | DDoS (UDP) | CPU spike, packet loss | `iftop`, check per-IP rate | Enable per-IP limit, scale TURN, check LB |
| 4 | Рассинхрон часов | `clock_offset > 100ms` | `chronyc tracking` | Restart chrony, check NTP sources |
| 5 | JWT key rotation | Auth errors spike | Check `kid` in tokens | Deploy new key, overlap 30d |
| 6 | Откат релиза | Error rate spike | Grafana, logs | Helm `--rollback`, check migration |
