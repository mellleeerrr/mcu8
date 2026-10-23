# VKS Code Review — Итоговый отчёт

> **Это срез на 2026-10-16, а не текущее состояние.** Многие находки уже
> исправлены (см. `docs/progress/MASTER_TASK_LIST.md`, разделы «Сделано»).
> Актуальная доска — мастер-список; документ не обновляется.


> **Дата:** 2026-10-16 21:39 MSK
> **Метод:** 4 параллельных саб-агента (регрессии, баг-хантинг, агизки, архитектура) + ground-truth-ификация каждого кода код
> **Цикл:** не завершён — ни один саб-агент не дал ≥9.5/10. Повторная итерация требуется после исправления.

---

## 1. Оценки саб-агентов (Итерация 1)

| # | Саб-агент | Задача | Оценка | Что мешает 9.5+ |
|---|---|---|---|---|
| 1 | Регрессии + мёртвый код | Анализ регрессий Ф5/6/7, лишний код | **6.0** | 7 орфанных крейтов; `vks-proto` = `v` (мёртвый codegen); `ModeEventQueue` → fire-into-the-void; `vks-common` vmonitor` описан в доках, но не существует |
| 2 | Баг-хантинг | Логи-буги, race, off-by-one, state transitions | **5.5** | Dual-clock в `ModeSwitcher`; `elapsed_ms += 33` mock clock; `switch_room_mode` → всегда `Mcu`; H.323 FFI restart off-by-one; VAD ZCR no-op; SIP negotiate丢弃 video |
| 3 | Агизки | Rust idioms, error handling, concurrency, API design | **6.5** | `HandlerConfig::default_test()` в production; no RBAC on v3; `unwrap()` in non-test code; no config validation; God module `handlers.py` |
| 4 | Архитектурная целостность | Hexagonal, ports, ADEv7ks | **5.5** | 7 orchestrated crates; `MediaEvent` has no pull method; no real TURN; `AT; `vks-proto` = `v` dead |

**Средний: 5.9/10.** Ни один ≥9.5 → **цикл ревью НЕ завершён.**

---

## 2. Root Cause — корень всех проблем

**Одна системная причина порождает 90% всех проблем:**

> **Composition root (`vks-server/main.rs`) инструментирует только 8 из 13 серверных крейтов. 7 крейтов (MCU, layout, SIP, H.323, recorder, cluster, SFU) скомпилированы, но **никогда не линкуются** в бинар. Следовательно: нет видео, нет переключения SFU↔MCU, нет SIP/H.323, нет записи, нет кластера, нет QoE/SLO. Всё это — «библиотеки для будущего», которые ещё не стали частью работающего продукта.**

Вторичные корни:
- **Порты неполные**: `MediaEvent` не имеет `get_pull()` / `get_mixed_audio()` → MCU должен downcast. `MediaEvent`-Event-поток не подписан к любому consumer (vEventQueue = мёртвый fire).
- **Конверсия не подключена**: `ModeChangedEvent` → `EventQueue` → **контакт** (нет → нет WS fan-out, нет NAT). Клиент никогда не узнает о переключении.
- **Auth слабее, чем задокументировано**: HS256 (не RS256), PBKDF2 (не Argon2id), нет refresh/lockout/MFA. ADR-0021 (RS256) — долг.
- **Конфиг не валидирует**: `auth_provider`/`default_mode` — свободные строки; `VPC_CONFIG` 20` не читается; PostgreSQL DSN → тихие memory.

---

## 3. Варианты решения — оценти по каждой оценка.5

### Вариант A: «Спешный фикс» — починить только критические bugs
| Что | Как |
|---|---|
| Dual-clock `ModeSwitcher` | Использовать `self.clock` (constructor), убрать local 1-tick |
| `switch_room_mode` → honor mode | `match new_mode { Mcu => Ok(Mcu), Sfu => Ok(Sfu) }` |
| H.323 FFI off-by-one | Убрать `restarts += 1` из panic path (оставить только в `restart()`) |
| VAD ZCR no-op | `zcr_threshold: 0.0 → 0.1` (или document RMS-only) |
| `elapsed_ms += 33` | Заменить на `clock.now_ms()` (injected `Clock` port) |

**Плюсы:** 2-3 часа, 5 файлов, 2, без критических багов.  
**Минусы:** 7 орйтов по-прежнему не связаны; конверсия по-прежнему не работает; media нет; ADR-0020/20 по-прежнему partial.  
**Оценка: 7.0/10** — лучше, но не 9.5. Продукт по-прежнему «сервер сигнализации + UI», а не ВКС.

### Вариант B: «Связать всё» — wire all 7 orchestrated crates + конверсия
| Что | Как |
|---|---|
| MediaEngine in main.rs | Конструктор SFU + MCU, select by `config.default_room_mode`, add |
|ModeSwitcher fix | Dual-clock + re-apply layout on transition + `re
- Event fan-out | `ModeEventQueue` → WS broadcast in `channel.rs` (нет NATS) |
| |
| `/api/v3/mode` | Endpoint for SFU↔MCVC
| |
| v3 | Layer `jwt_auth_token` on v3 + enforce `Role` in `token_middleware` |
| |
| HandlerConfig | Replace `default_test()` with `Config`-driven values |

**Плюсы:** Продукт становится настоящим ВКС: media есть, конверсия работает, клиент получает события. 8-12 часов, 12-15 files.  
**Минусы:** Нет реATHS (в NAT); не real TURN; SIP/H.323 still "FSM, not real"; auth still HS256 (not RS256).  
**Оценка: 8.5/10** — большой скачок, но всё ещё не 9.5: infra-ещё mock, auth ещё слабее, чем ADR.

### Вариант C: «Полный прогон» — B + real NATS + TURN + RS256 + deploy
| Что | Как |
|---|---|
| Всё из B | + |
| async-nats | `vks-cluster` → real JetStream, 3-node HA |
| turn-rs / coturn | Real TURN, `ServerHello.ice` = real candidates |
| RS256 + kid + rotation | `vks-auth` → real asymmetric JWT (ADR-0021) |
| Argon2id | Replace PBKDF2 |
| Dockerfile + compose | `deployments/` dir, production-ready |
| Config validation | `auth_provider`/`default_room_mode` → typed enums, `VKS_CONFIG` |
| v2 monitor | Remove + `v3` → real QoE/SLO (v + Prometheus) |

**Плюсы:** 9.5+/10. Продукт-92. В production-ready, 13, ADR-compliant.  
**Минусы:** 2-3 дня, 25-30 files. Зависимости: async-nats, turn-rs, ring (RS256), argon2.  
**Оценка: 9.5/10** — единственный вариант, который проходит порог.

### Вариант D: «Минималистичный» — B minus SIP/H.323/recorder
| Что | Как |
|---|---|
| B, но без gateways + recorder | Только SFU+MCU+event + WS + auth |

**Плюсы:** 6-8 часов. Быстрее B.  
**Минусы:** SIP/H.323/recorder по-прежнему орфаны (не "DR-2" не "deferred" — просто "не связаны"). ADR-0006/06/07 partial.  
**Оценка: 8.0/10** — лучше A, хуже B.

---

## 4. Рекомендация

**Вариант C** — единственный, который даёт ≥9.5/10. Но он объёмный (2-3 дня).

**Практическая рекомендация: B → C (two commits):**
1. **С1 (сегодня, 8-12 час):** Вариант B — связать 7 орфтов, конверсимсия, auth, config. Это делает в **не ВКС**
2. **С2 (завтра, 8-12 час):** Вариант C minus B — NATS, TURN, RS256, deploy. Это делает в **production-92.**

---

## 5. Пошаговый план доработок

### Шаг 1: Fix transparency (Critical #1, #2)
- [ ] `vks-core/src/transparency.rs`: убрать local 1-tick clock, использовать `self.clock` (constructor `ready_after_ms`)
- [ ] `switch_mode`: re-apply `current_layout` to candidate (`candidate.apply_layout(&self.current_layout)`)
- [ ] Add test: assert new engine **receives** layout after switch
- [ ] `vks-media-mcu/src/engine.rs`: `switch_room_mode` → honor `new_mode` (не игнорировать)

### Шаг 2: Wire MediaEngine in composition root (Critical #1)
- [ ] `vks-server/src/main.rs`: construct `vks-suflcuEngine` + `vks-mcuEngine`, select by `config.default_room_mode`, hold in `AppState`
- [ ] `vks-server/src/lib.rs`: pass engine to signaling handlers
- [ ]

`vks-core/src/ports/media-event.rs`: `get_pull()` / `get_mixed_audio()` (so MCU doesn't need to downcast)

### Шаг 3: Close mode-switch event loop (Critical #3)
- [ ] `vks-admin/src/mode.rs`: `ModeEventQueue` → WS broadcast in `channel.rs`
- [ ] `vks-server/src/lib.rs`: wire `ModeEventQueue` into WS handler (broadcast `room.mode_changed`)
- [ ] `vks-admin/src/api.py`:`: add `/api/v3/mode` + `/api/v3/mode/{id}` (mirror v3)
- [ ] `vks-signaling/src/hand.py`: propagate `config.default_room_mode` to `room_mode` in `ServerHello` (не не `0`)

### Шаг 4: Wire v1 auth + v3 RB (Critical #4)
- [ ] `vks-admin/src/hand.py`: layer `jwt_auth_token` on v3 router
- [ ] `vks-admin/src/api.py`: enforce `Role` in `token_middleware` (не не `permissions:["all"]`)

### Шаг 5: Fix MCU logic (Warning #5)
- [ ] `vks-media-mcu/src/audio_mixer.rs`: count **all** participants (не not just non-empty); true AGC (power per power)
- [ ] `vks-media-mcu/src/vad.rs`: `zcr_threshold: 0.0 → 0.1` (or document RMS-only)
- [ ] `vks-media-mcu/src/layout_bridge.rs`: handle `current_event_ms == 0` (не не fade); transition) cross- per participant (не not crossfade)
- [ ] `vks-media-mcu/src/engine.rs`: replace `elapsed_ms += 33` with injected `Clock` port; fix `35: `audio truncation` (not use `ms` 960 or res)

### Шаг 6: Fix gateways (Warning #6)
- [ ] `vks-gateway-sip/src/codec.rs`: `negotiate` → return per-stream results (audio+video, не not single); add G.729/H.263 to SIP map
- [ ] `vks-gateway-h323/src/ffi.rs`: fix restart off-by-one (не not `restarts += 1` in panic path); fix watchdog seconds-vs-ticks
- [ ] `vks-gateway-sip/src/call.rs`: add Reject (480/603) + re-INVITE paths

### Шаг 7: Wire cluster + recorder (Critical #6)
- [ ] `vks-server/src/main.rs`: `tokio::spawn` for heartbeat + lease renewal (vks-cluster)
- [ ] `vks-server/src/main.rs`: wire recorder (vks-recorder) for room recording
- [ ] Remove dead `vks-proto` = `vks_cluster` (v or mark as deferred)

### Шаг 8: Real NATS + TURN (Critical #7)
- [ ] Add `async-nats` to `vks-cluster` (back `Bus` with JetStream)
- [ ] Add `turn-rs` (or coturn) for TURN
- [ ] `ServerHello.ice` = real TURN candidates (не `0`)
- [ ] `/readyz` `nats` = real check (не not `always true)

### Step 9: RS256 + Argon2id + config validation (Critical #5, #9)
- [ ] `vks-auth/src/jwt.rs`: RS256 + `kid` + 90d rotation (ADR-0021)
- [ ] `vks-auth/src/local_db.rs`: Argon2id (not PBKDF2)
- [ ] `vks-common/src/config.rs`: validate `auth_provider`/`default_room_mode` (typed enums); add `VPC_CONFIG` 20`
- [ ] `configs/default.tomc`: (default` + documented port + `TLS`

### Step 10: Deployment (Critical #10)
- [ ] `deployments/Dockerfile`, `compose/ ]
- [ ] Prometheus/Grafana scrape config
- [ ] `dev/`, `production.toml` for config

---

## 6. Итоговая таблица

| Шаг | Что чинит | Files | Время | Оценка после |
|---|---|---|---|---|
| 1 | Transparency (dual-clock, layout re-apply) | 2 | 4h | 7.0 |
| 2 | MediaEngine in main.rs | 3 | 8h | 7.5 |
| 3 | Mode-switch event loop + /api/v3/mode | 4 | 6h | 8.0 |
| 4 | v1 auth + v3 RB | 2 | 4h | 8.0 |
| 5 | MCU logic (AGC, VAD, crossfade, clock) | 3 | 8h | 8.5 |
| 6 | Gateways (SIP negotiate, H323 FFI) | 3 | 4h | 8.5 |
| 7 | Cluster + recorder wiring | 2 | 4h | 8.5 |
| 8 | Real NATS + TURN | 4 | 8h | 9.0 |
| 9 | RS256 + Argon2id + config validation | 4 | 8h | 9.5 |
| 10 | Deployment | 3 | 4h | 9.5 |

**Total: ~60h (8 working days), ~30 files. After Step 9-10.5: 9.5/10.**

**Next review iteration:** after Steps 1-5 (S1), re-run 4 → verify ≥9.5. If not 10, continue to Steps 6-10.

---

## 7. Баг-хантер: 12 конкретных находок (саб-агент 0f28291e)

### Критические (паники в library-коде)

| # | Файл:строка | Баг | Риск |
|---|---|---|---|
| B1 | `vks-media-mcu/src/engine.rs:241,246` | `add_participant` **паникует**: `Track::new(*pid)` при `pid=0` (default UUID); `participant_to_slot` паникует если layout не применён | Паник в `tokio::spawn` → «зависший» участник без cleanup |
| B2 | `vks-media-mcu/src/audio_mixer.rs:65` | `channels = 0` → division by zero → **panic** | `AudioMixConfig` не имеет assert в `Default` |

### Средние (семантика ≠ контракт)

| # | Файл:строка | Баг | Рекомендация |
|---|---|---|---|
| B3 | `vks-media-sfu/src/congestion.rs:23-25` | `HYS_DEGRADE_PCT`/`HYS_RECOVER_PCT` — имена **в обратную сторону** (degrade-порог используется для recovery) | Переименовать: `RECOVER_FROM_DEGRADE_PCT` |
| B4 | `vks-cluster/src/ownership.rs:138` | `renew_lease` — «lowest wins» при renew создаёт **ping-pong** между инстансами (не только в конфликте) | Явный комментарий или fix: только owner может renew |
| B5 | `vks-auth/src/jwt.rs:123` | `leeway = 0` (hardcoded) — clock drift в кластере → expired tokens rejected | Вынести `leeway` в `JwtConfig` |
| B6 | `vks-gateway-sip/src/codec.rs:118` | `negotiate` возвращает **один** кодек (audio priority) → **video теряется** в `SipCall`; `negotiate_streams` существует но не вызывается | Использовать `negotiate_streams` в `SipCall::new` |
| B7 | `vks-admin/src/api_v3/auth.rs:94-117` | Lockout user+IP: счётчик **не сбрасывается после BLOCK** → повторная блокировка при 5 неудачах, 4 из которых были во время блокировки | Документировать или fix: clear counter on unblock |

### Мелкие (стиль/повторяемость/док)

| # | Файл:строка | Баг | Рекомендация |
|---|---|---|---|
| B8 | `vks-gateway-{sip,h323}/src/codec.rs:37-40` | **Дублирование** `WebRtcCodec` в двух крейтах (идентичные структуры) | Вынести в `vks-common` или `vks-core` |
| B9 | `vks-common/src/ratelimit.rs:161` | Тест `refill_over_time` не покрывает **refill по времени** (только «no drift» на tick 0) | Переписать с явным `allow("k", 3)` → `allow("k", 6)` |
| B10 | `vks-admin/src/mode.rs:174` | `no_event_when_same_mode` зависит от **дефолта** `global_mode=Sfu` — если дефолт поменяют, тест сломается тихо | Явно выставить `global_mode = Sfu` перед тестом |
| B11 | `vks-cluster/src/bus.rs:227` | `Duplicate` → `RetriedCritical` → вызывающий повторит push → **замкнутый цикл** | Duplicate → `Accepted` (идемпотентность = успех) |
| B12 | `vks-auth/src/local_db.rs:89` | Dummy hash (PBKDF2 100k iter) **под Mutex** → блокирует весь `LocalDb` на 50-100ms при unknown user | Вынести dummy hash **после** drop(map) |
| B13 | `vks-media-sfu/src/bwe.rs:183` | `bps_from_arrivals(0 bytes, 0ms)` → `bps=0` → simulcast → `Low` — **не покрыто тестами** | Добавить тест: empty feedback → Low |
| B14 | `vks-core/src/transparency.rs:182` | `ClientView::apply` **паникует** на `#[non_exhaustive]` future variant | Return `Result`/`bool` вместо panic |

### Тесты: что НЕ покрыто
1. `McuEngine::add_participant` **без** `apply_layout` (panic B1)
2. `McuEngine::remove_participant` для несуществующего (layout-слот не освобождается)
3. `SfuEngine::remove_publisher` → `forwarder.subscribers_of(track)` empty (не проверяется)
4. `RateLimiter::admin()` — eviction (только bounded_capacity, capacity=10, not 600)
5. `Bus::push_backpressure` — duplicate (B11)
6. `JwtIssuer` — `leeway > 0` (B5)
7. `ModeSwitcher::switch` — `switch_epoch > 3000` wraparound
8. `SipGateway` — multiple calls in one room (append, no dedup)

---

## 8. Консолидированный план (обновлённый)

Оригинальные 10 шагов + 14 багов баг-хантера. Приоритизация:

### P0 — Критические (фикс до следующего ревью)
| # | Что | Файлы | Время |
|---|---|---|---|
| 1 | Fix ModeSwitcher (dual-clock + layout re-apply) | `vks-core/src/transparency.rs` | 4h |
| 2 | Fix MCU `add_participant` panic (B1) | `vks-media-mcu/src/engine.rs` | 2h |
| 3 | Fix audio mixer channels=0 panic (B2) | `vks-media-mcu/src/audio_mixer.rs` | 1h |
| 4 | Wire MediaEngine in main.rs | `vks-server/src/main.rs` | 8h |
| 5 | Fix `switch_room_mode` → honor mode | `vks-media-mcu/src/engine.rs` | 1h |

### P1 — Высокий (фикс в S1)
| # | Что | Файлы | Время |
|---|---|---|---|
| 6 | Close mode-switch event loop (WS fan-out + /api/v3/mode) | `vks-admin/src/mode.rs`, `vks-server/src/lib.rs`, `vks-signaling/src/handler.rs` | 6h |
| 7 | Fix MCU: AGC, VAD ZCR, crossfade ms=0, real clock | `vks-media-mcu/src/*` (3 files) | 8h |
| 8 | Fix SIP negotiate → per-stream (B6) + H323 FFI off-by-one | `vks-gateway-{sip,h323}/src/*` | 4h |
| 9 | Wire v1 auth + v3 RBAC | `vks-admin/src/handlers.rs`, `api_v3/mod.rs` | 4h |
| 10 | Wire cluster + recorder in main.rs | `vks-server/src/main.rs` | 4h |

### P2 — Средний (фикс в S2)
| # | Что | Файлы | Время |
|---|---|---|---|
| 11 | Real NATS + TURN | `vks-cluster/src/bus.rs` + new deps | 8h |
| 12 | RS256 + Argon2id + leeway in JwtConfig (B5) | `vks-auth/src/*` | 8h |
| 13 | Fix lockout edge case (B7) + dummy hash after drop(map) (B12) | `vks-auth/src/*` | 2h |
| 14 | Fix bus duplicate → Accepted (B11) | `vks-cluster/src/bus.rs` | 1h |
| 15 | Fix ClientView::apply → Result (B14) | `vks-core/src/transparency.rs` | 1h |
| 16 | Deduplicate WebRtcCodec → vks-common (B8) | `vks-common/src/` + both gateways | 2h |
| 17 | Fix congestion hysteresis names (B3) | `vks-media-sfu/src/congestion.rs` | 1h |
| 18 | Fix ownership renew lease doc (B4) | `vks-cluster/src/ownership.rs` | 1h |
| 19 | Config validation + VKS_CONFIG | `vks-common/src/config.rs` | 4h |
| 20 | Deployment (Dockerfile, compose, Grafana) | `deployments/` | 4h |

### P3 — Тесты (добавить с каждым фиксом)
- [ ] `McuEngine::add_participant` without `apply_layout` (B1)
- [ ] `McuEngine::remove_participant` for non-existent (layout slot freed)
- [ ] `SfuEngine::remove_publisher` → subscribers empty
- [ ] `RateLimiter::admin()` eviction (capacity=600)
- [ ] `Bus::push_backpressure` duplicate (B11)
- [ ] `JwtIssuer` leeway > 0 (B5)
- [ ] `ModeSwitcher::switch` epoch wraparound
- [ ] `SipGateway` multiple calls in one room
- [ ] `vks-common/src/ratelimit.rs` refill over time (B9)
- [ ] `vks-media-sfu/src/bwe.rs` empty feedback → Low (B13)

### Итого: ~70h, ~35 files. After P0+P1: 8.5/10. After P2: 9.0/10. After P3: 9.5/10.

**Next review iteration:** after P0 (5 fixes), re-run subagent → verify score ≥ 8.0. After P0+P1, verify ≥ 8.5. After all, verify ≥ 9.5.

---

## 9. Архитектурный саб-агент (145a2bdc): 13 нарушений, оценка 6.5/10

### Новые находки (не в других отчётах)

| # | Файл:строка | Нарушение | Severity |
|---|---|---|---|
| F1 | `vks-core/src/events.rs:231`, `room.rs:27`, `participant.rs`, `track.rs` | Core ID types (RoomId/ParticipantId/TrackId/EventId) use `Uuid::new_v4()` — **bypass the Rng port** | MED |
| F4 | `vks-core/src/ports/events.rs` | **Events port has ZERO implementations** — ModeEventQueue (admin) and Bus (cluster) are parallel non-ports | HIGH |
| F5 | `vks-auth/src/provider.rs` + `vks-core/src/ports/auth.rs` | **Two parallel AuthEngine traits** with different signatures (Credentials struct vs &str; 3 methods vs 2), bridged by hand-adapter | MED |
| F6 | ARCHITECTURE.md §3 | **SignalingTransport port** documented in core but absent from code (exists only in client + vks-signaling local ports) | MED |
| F10 | `vks-core/src/layout.rs`, `vks-layout/src/core.rs`, `vks-admin/src/state.rs` | **Three layout representations** (core::Layout metadata, LayoutDescriptor assignments, vks-layout::Layout geometry) — never meet through port | MED |
| F12 | `vks-core/src/session.rs` + `vks-signaling/src/session.rs` | **Two parallel Session FSMs** — core's unused outside its tests; signaling rolls its own | HIGH |
| F14 | `vks-server/src/main.rs` | 6-7 crates compile but never linked; **no ADR records the deferral** | MED |
| F15 | `vks-admin/src/state.rs:121`, `vks-server/src/main.rs:73` | Storage is **read-only in practice** (sync_from_storage at boot, no write-back on create/delete); postgres DSN silently → Memory | MED |
| F16 | `vks-signaling/src/handler.rs:73` | **Shadow types**: admin uses RoomRecord, signaling uses `Option<String>` room_id — core domain not the source of truth | MED-HIGH |

### Подтверждённые находки (из других агентов)
- F7: MediaEngine trait is control-plane only; data plane (tick/route/feedback) requires downcast — **confirmed**
- F13: No MediaEngine in composition root — **confirmed, CRITICAL**
- F17: Transparency invariant not closed end-to-end (3 unconnected mode mechanisms) — **confirmed, CRITICAL**
- F18: ModeSwitcher bugs (dual-clock, layout not re-applied, no re-attach) — **confirmed, HIGH**
- F8: `switch_room_mode` asymmetric (SFU claims MCU, MCU ignores arg) — **confirmed**

### Вердикт архитектора
> "The shape of the architecture is good: hexagonal core is genuinely port-based, dependency direction is clean (no cycles), MediaEngine trait is well-designed. What it is NOT yet is an **integrated** architecture. Adapters and domain objects are built in parallel silos and never plugged together in the composition root. The design is sound; the wiring is incomplete — the difference between a 6.5 and a 9.5."

**Score: 6.5/10**

---

## 10. Итоговые оценки (все 4 саб-агента + lead)

| # | Саб-агент | Область | Оценка | Главный блокер 9.5+ |
|---|---|---|---|---|
| 1 | Регрессии + мёртвый код | Cross-crate | **6.0** | 7 орфанных крейтов; ModeEventQueue → void; vks-proto::cluster dead |
| 2 | Баг-хантинг | Logic bugs | **5.5** | 2 panics (engine, audio_mixer); SIP video loss; FFI off-by-one; VAD ZCR no-op |
| 3 | Агизки | Rust idioms | **6.5** | default_test() in prod; no RBAC v3; unwrap() in non-test; no config validation |
| 4 | Архитектура | Hexagonal/ports | **6.5** | No MediaEngine in main.rs; 3 unconnected mode mechanisms; Events port dead; 2 Session FSMs; shadow types |
| — | **Lead (я)** | Консолидация | **5.9 avg** | **Ни один ≥9.5** |

**Цикл ревью НЕ завершён.** Средний 5.9/10. Нужно: P0+P1 fixes → re-run → verify ≥8.5. Then P2+P3 → verify ≥9.5.

### Что мешает 9.5 (консолидированный список)

1. **Media plane absent from the process** — no engine constructed in main.rs (F13)
2. **Transparency invariant broken end-to-end** — 3 unconnected mode mechanisms (F17)
3. **Events port dead** — zero implementations (F4)
4. **Core domain not the source of truth** — shadow types everywhere (F12, F16)
5. **2 panics in library code** — add_participant + audio_mixer (B1, B2)
6. **Auth weaker than ADRs** — HS256 not RS256, PBKDF2 not Argon2id (F5)
7. **No real NATS or TURN** — in-memory Bus, ice=None (F14)
8. **7 orphaned crates** — no ADR records deferral (F14)
9. **SIP video loss** — negotiate returns single codec (B6)
10. **Storage read-only** — no write-back on create/delete (F15)
11. **No deployment artifacts** — no Dockerfile/compose (F14)
12. **Config not validated** — free strings, no VKS_CONFIG (F-adj)

### Что нужно для 9.5 (prioritized)

| Priority | Action | Closes |
|---|---|---|
| P0 | Instantiate MediaEngine + core domain in AppState (F13, F12, F16) | 1, 4 |
| P0 | Fix ModeSwitcher: real clock, re-apply layout, re-attach, async (F18) | 2 |
| P0 | Close mode event loop: drain → WS broadcast → ServerHello (F17) | 2 |
| P0 | Fix 2 panics: add_participant + audio_mixer (B1, B2) | 5 |
| P1 | Implement Events port on cluster Bus (F4) | 3 |
| P1 | Fix SIP negotiate → per-stream (B6); H323 FFI off-by-one (B7) | 9 |
| P1 | Wire v1 auth + v3 RBAC | 6 |
| P1 | Fix MCU: AGC, VAD ZCR, crossfade, real clock | 5 |
| P2 | Real NATS + TURN | 7 |
| P2 | RS256 + Argon2id + leeway in JwtConfig | 6 |
| P2 | Write-back to Storage on create/delete (F15) | 10 |
| P2 | Config validation + VKS_CONFIG | 8 |
| P2 | Deployment (Dockerfile, compose) | 12 |
| P3 | Add missing tests (10 gaps) | all |
