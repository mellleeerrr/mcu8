# BACKEND RULES — детальные правила бэкенда

> Статус: **v0.1 (черновик)**. Заполняется в фазах 1–7.
> Краткие правила — [`PROJECT_RULES.md` Часть II](../PROJECT_RULES.md). Здесь — детали.

## 1. Аутентификация

### 1.1. JWT
- **Алгоритм: RS256/PS256** (асимметричный). HS256 запрещён в production.
- **Access token**: TTL 15 минут, `kid` в header.
- **Refresh token**: opaque (не JWT), SHA-256 в БД, TTL 30 дней, одноразовый (rotation), reuse detection (revoke family).
- **Key rotation**: каждые 90 дней (настраиваемо), overlap 30 дней. `kid` — для валидации старых токенов.
- **Секрет**: `VKS_JWT_PRIVATE_KEY` (PEM, env var) или file (600). Никогда в config 644.

### 1.2. API-ключи
- Формат: `vks_key_<base62(32 bytes)>` (~43 символов).
- В БД: SHA-256 hash (never plain).
- Scopes: `read`, `write`, `admin` (bitmask).
- Rate limit: 600 req/min per key.
- Revocation: `DELETE /api-keys/{id}`, кеш TTL 60s.

### 1.3. Пароли (local_db)
- **Argon2id** (m=64MiB, t=3, p=4). Бcrypt/SHA запрещены.
- Min 12 символов, top-10k check.
- TTL 365 дней. Сброс — одноразовый токен (email, TTL 15min).

### 1.4. Lockout / brute force
- 5 failed login / 15min → lock 15min (per user+IP).
- 10 failed / 5min per IP → block 15min.
- Каждая неудача + блокировка → audit log.

### 1.5. LDAP
- ldap3 + GSSAPI/Kerberos (FreeIPA/AD).
- Bind DN, group mapping, timeout 5s.
- Circuit breaker: 50% fail / 10s → OPEN 30s.

### 1.6. MFA
- TOTP (RFC 6238), опционально. Обязательна для Operator.
- HMAC-secret: SHA-256 в БД, период 30s, окно ±1.

### 1.7. RBAC matrix (ADR-0022)

| Действие | Operator | Moderator | Participant | Viewer |
|---|---|---|---|---|
| Создать/удалить комнату | ✅ | ✅ | ❌ | ❌ |
| Переключить MCU/SFU (глобально) | ✅ | ❌ | ❌ | ❌ |
| Переключить режим комнаты | ✅ | ✅ | ❌ | ❌ |
| Мутить/удалить участника | ✅ | ✅ | ❌ | ❌ |
| Publishing | ✅ | ✅ | ✅ | ❌ |
| Subscribing | ✅ | ✅ | ✅ | ✅ |
| Screen sharing | ✅ | ✅ | ✅ | ❌ |
| Chat | ✅ | ✅ | ✅ | ❌ |
| Редактировать Layout | ✅ | ✅ | ❌ | ❌ |
| Управление пользователями | ✅ | ❌ | ❌ | ❌ |
| API-ключи | ✅ | ❌ | ❌ | ❌ |
| Audit log | ✅ | ✅ | ❌ | ❌ |
| Запись | ✅ | ✅ (confirm) | ✅ (confirm) | ✅ (confirm) |

## 2. Сетевая безопасность

### 2.1. TLS
- WSS: TLS обязательный. Let's Encrypt (ACME) или internal CA.
- HSTS: max-age=31536000.
- Min TLS 1.2, prefer 1.3.
- Ciphers: TLS_AES_256_GCM_SHA384, TLS_CHACHA20_POLY1305_SHA256, TLS_AES_128_GCM_SHA256.
- Rotation: ACME auto-renewal, hot-reload (quinn/tokio-rustls).

### 2.2. TURN
- TLS 5349, DTLS 3478.
- Credentials: TTL 1h (coturn use-auth-secret).
- ICE validation: host/srflx/prflx.
- Per-credential rate limit.

### 2.3. Порты (production)
| Порт | Протокол | Назначение |
|---|---|---|
| 443 | TCP | WSS + REST (TLS) |
| 8443 | TCP | WebTransport/QUIC (TLS) |
| 3478 | UDP | STUN |
| 5349 | UDP | TURN-TLS |
| 40000–60000 | UDP | RTP (SRTP) |
| 9090 | TCP | Prometheus (internal) |
| 4222 | TCP | NATS (internal) |
| 5432 | TCP | PostgreSQL (internal) |

### 2.4. Message limits
- WS frame: 16 KB max.
- Protobuf message: 64 KB max.
- JSON admin API body: 256 KB max.
- Превышение → `MessageTooLarge` (close 4008).

### 2.5. Keepalive
- Server ping 30s, 2 missed → close (1005).
- Reconnect: exponential backoff 1s→60s + jitter ±20%, max 5 попыток.
- Thundering herd: server rate-limits reconnects (100/s).

## 3. Безопасность медиа

### 3.1. DTLS-SRTP
- str0m: per-session self-signed X.509, fingerprint в SDP.
- SRTP: AES-128-CM, SRTCP: HMAC-SHA1 80-bit.
- PFS: ECDHE ephemeral.
- ICE-lite: SFU/MCU = host candidates only.

### 3.2. Media isolation
- Participant A получает медиа B только при active subscription.
- Zombie stream: grace period 5s → TrackRemoved.
- No implicit publish: только после PublishAccepted.

### 3.3. Записи
- Encryption at rest: AES-256-GCM, per-recording DEK + KMS master.
- Retention: 90 дней (настраиваемо).
- Download: временный token (TTL 1h).

## 4. Надёжность

### 4.1. Graceful shutdown
1. Stop accepting new connections.
2. Active WS → `ServerMaintaining` (new owner).
3. Media drain: RTP flush, DTLS close.
4. Storage flush: NATS queue drain.
5. Exit 0. Timeout 30s → SIGKILL.

### 4.2. Failover
- Media instance dies: NATS heartbeat 10s → room re-assign → clients reconnect (30–60s).
- NATS cluster: 3 nodes, HA-queue, JetStream (persistent).
- PostgreSQL: primary + replica (streaming), Patroni (auto-failover).
- Split-brain: NATS lease 30s, lowest instance_id wins.

### 4.3. Health checks
- `/healthz` (liveness): 200 всегда.
- `/readyz` (readiness): DB + NATS + media OK → 200, иначе 503.
- K8s: liveness 10s/3s, readiness 5s/3s, startup 30s.

### 4.4. Resource limits
- K8s: MCU 4CPU/4GB, SFU 1CPU/1GB.
- File descriptors: 65535.
- Per room: max 50 participants, max 100 tracks.
- Per participant: max 4 tracks (2 video + 1 audio + 1 screen).

### 4.5. Circuit breaker
- LDAP, PostgreSQL, NATS, TURN: 50% fail / 10s → OPEN 30s → HALF-OPEN.
- Retry: exponential 1s→30s, max 3, jitter.
- Timeout: LDAP 5s, DB 30s, NATS 5s, TURN 10s, DTLS 10s.

## 5. Данные

### 5.1. PostgreSQL
- `sslmode=verify-ca` (min) / `verify-full` (recommended).
- Pool: 50, idle 10min, statement_timeout 30s.
- Credentials: env vars.

### 5.2. Backup
- pg_dump daily + WAL (PITR). RPO 24h (WAL 5min), RTO 4h.
- Retention: 30d daily + 12w weekly. Encryption: AES-256.
- Quarterly restore test.

### 5.3. GDPR / 152-ФЗ
- PII: users, recordings metadata.
- Right to erasure: `DELETE /users/{id}` → soft-delete + hard-delete 30d.
- Retention: audit 2y, recordings 90d.
- Storage: on territory (default: RU).

## 6. Приложение

### 6.1. Input validation
- Protobuf: type-safe (prost), unknown fields ignored.
- JSON (admin): serde + validator.
- SQL: parameterized (sqlx `query_with`), никогда string concatenation.

### 6.2. Memory safety
- `unsafe`: только с ADR + `// SAFETY:`, forbidden в vks-core.
- FFI (H323Plus, ffmpeg): одна граница, panic hook, watchdog.
- Production: `panic = "abort"`.

### 6.3. Secrets
- Env vars (VKS_*), never in config 644, never in git/logs.
- Vault (опционально) для production.

### 6.4. Error handling
- Production: generic message, details in logs.
- Stack trace: только `?trace=1` или debug feature.
- Error codes: 400/401/403/404/409/422/429/500 + `error_code` string.

## 7. API versioning

- `/api/v1/...` (major). Breaking change = новый major.
- Deprecated: 2 major versions, `Deprecation: true` + `Sunset: <date>`.
- Changelog: `docs/CHANGELOG.md` (keep-a-changelog).
