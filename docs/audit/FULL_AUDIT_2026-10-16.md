# VKS Project — Full Audit Report

> **Дата отчёта:** 2026-10-16 20:17 MSK
> **Метод:** 5 параллельных агентов (docs-vs-code, MCU-логика, UI/контролы, запуск/архитектура + синтез)
> **Область:** весь workspace (15 серверных крейтов + `client/vks-client-sdk`), доки, ADR, PNM frontend

---

## 1. Executive Summary

Кодбаза **структурно здорова, но операционно неполна**: 16 крейтов компилируются в один HTTP+WebSocket-сервер (`vks-server`) с чистой ациклической зависимостью, однако **7 из 15 серверных крейтов скомпилированы, но никогда не линкуются в бинарник** (`vks-media-mcu`, `vks-gateway-sip`, `vks-gateway-h323`, `vks-recorder`, `vks-cluster`, `vks-layout`, и фактически `vks-media-sfu`) — **в запущенном процессе нет ни медиа-плана, ни TURN, ни кластера, ни записи, ни SIP/H.323**. Крупнейшие риски: (a) инвариант прозрачности (ADR-0002) сломан в composition root — линкуется только SFU, MCU-режим обслуживать невозможно, а `ModeSwitcher`'s timeout/rollback почти недостижим (readiness хардкод-1 tick); (b) почти все «реальные» интеграции — in-memory-мок (NATS→`Bus`, SIP→FSM, H.323→mock FFI, LDAP→`MockLdapProvider`, транспорты→stub). Готово: доменная/портовая модель (hexagonal), signaling-конверт/FSM, auth flow end-to-end, admin REST + `/api/v3`-адаптер, MCU pipeline-логика (mixer/compositor/VAD/layout-bridge) — но с реальными корректностными пробелами.

---

## 2. Architecture Overview

### Крейты — что каждый реально делает

| Крейт | Реальная роль | Статус |
|---|---|---|
| **vks-core** | Доменные типы (`Room`, `Participant`, `Role`, `Track`, `RoomMode`) + порты (`MediaEngine`, `Storage`, `AuthProvider`, `Events`, `Clock`, `Rng`) + `transparency.rs` (`ModeSwitcher`). Hexagonal heart; no I/O. | **Implemented** |
| **vks-proto** | prost-build over 4 `.proto` + `openapi.json`. 5 roundtrip tests. | **Implemented** |
| **vks-common** | `error`, `tracing`, `metrics` (Prometheus, 6 HTTP families), `config` (figment), `ids` + **merged `qoe`/`slo`/`ratelimit`** (no separate `vks-monitoring` crate). | **Implemented** (qoe/slo/ratelimit **dead at runtime**) |
| **vks-auth** | `JwtIssuer` (**HS256**, not RS256), `ApiKey`, `LdapClient` (trait+mock), `LocalDb` (**PBKDF2**, not Argon2id). **No** `refresh.rs`/`password.rs`/`lockout.rs`/MFA. | **Partial** |
| **vks-storage** | `Memory`/`Sqlite`/`Postgres` on sqlx + `core_adapter` + `rows` + one migration. | **Implemented** (only SQLite/Memory constructed; Postgres DSN silently→Memory) |
| **vks-layout** | `core`/`rect`/`grid`/`focus`/`renderer` (`DrawCommand`). Self-implemented geometry; **no Taffy/hadrone-core**; `Rect` = **u32 px**, not normalized. | **Implemented** (orphaned from server) |
| **vks-signaling** | `ws` (mock+optional stub), `wt` (mock+optional no-op), `sdp`, `handler`, `envelope`, `session` (FSM), `reconnect`. **Both "real" transports = stubs behind optional features.** | **Partial** (FSM/envelope real; transports stub) |
| **vks-media-sfu** | `engine` (impl `MediaEngine`), `forwarder` (full-mesh), `simulcast`, `bwe` (TWCC), `dtls`, `congestion`, `ice` (ICE-lite), `codec` (feature-gated), `udp_loop`. str0m optional, not default. | **Implemented** (only in dev tests; not instantiated in `main.rs`) |
| **vks-media-mcu** | `engine` (McuEngine, 30fps tick), `compositor` (nearest-neighbor, 1-byte/px), `frame` (placeholder+mock codec), `audio_mixer`, `vad`, `layout_bridge`. Mock sources. | **Implemented** (orphaned from server; §4 bugs) |
| **vks-gateway-sip** | `codec` (SIP↔WebRTC map) + `call` (UAS FSM). **No rvoip**; SIP = state machine. | **Mock** (orphaned) |
| **vks-gateway-h323** | `codec` + `call` (H.225/H.245 FSM) + `ffi` (mock bridge, watchdog, panic hook). **No real H323Plus/OPAL FFI.** | **Mock** (orphaned) |
| **vks-recorder** | `recording` (session/segments), `storage` (**MemoryStore only**, no S3/encryption), `retention`. | **Partial** (orphaned; no S3, no AES-256-GCM) |
| **vks-cluster** | `bus` (**in-memory pub/sub**, not NATS), `ownership` (consistent hash+lease), `heartbeat`, `circuit`, `shutdown` (5-step), `backpressure`. | **Mock** (orphaned; no NATS crate anywhere) |
| **vks-admin** | `handlers` (v1 REST), `rooms`/`users`/`settings`/`mode`/`audit`/`health`/`state`, **`api_v3/`** (large legacy PNM adapter: auth, shell, sessions, ws). | **Implemented** (v1+v3; v1 unauthenticated, no layouts/records REST) |
| **vks-server** | `main.rs` (composition root), `lib.rs` (`build_app`), `frontend.rs` (PNM statics+CSP). | **Implemented** (thin; wires 7 crates, ignores 7) |
| **client/vks-client-sdk** | WASM (web-sys RtcPeerConnection) + native (str0m optional). | **Implemented** (client) |

> **Note:** `vks-monitoring` не существует как крейт — `qoe`/`slo`/`ratelimit`/`metrics` в `vks-common`. `PROGRESS.md`/Phase 7 описывают его как отдельный крейт.

### Dependency graph

```
vks-server ──► common, core, storage, auth, admin, signaling, media-sfu
                 │
        ┌────────┴───────────┬──────────────┐
   vks-admin             vks-signaling  vks-media-sfu
      │  (core,auth,common)   │ (proto)      │ (core)
      ├─► vks-auth (core)     └─► vks-proto  │
      │      └─► vks-core (leaf)
      ├─► vks-storage (core) ─► vks-core
      └─► vks-common (standalone)

ORPHANED (workspace members, compiled, NOT linked into vks-server):
   vks-media-mcu ─► core, layout
   vks-layout    ─► (thiserror, serde only)
   vks-gateway-sip/h323 ─► (core only)
   vks-recorder  ─► (no internal deps)
   vks-cluster   ─► (no internal deps)
vks-proto::cluster = dead generated code
```

No circular dependencies; all edges point downward to `vks-core`/`vks-proto`/`vks-common` leaves.

---

## 3. Documentation vs Code — Top 10 Discrepancies

| # | Documented | Actual | Gap | Severity |
|---|---|---|---|---|
| 1 | **vks-monitoring** = separate crate with 4 modules, 22 tests (PROGRESS.md, Phase 7) | No such crate; modules merged into `vks-common` | Architecture doc describes a crate that doesn't exist | **High** |
| 2 | **ARCHITECTURE §3 shows one `vks-gateway/`** crate with `sip/`+`h323/` subdirs | Two separate crates: `vks-gateway-sip`, `vks-gateway-h323` | Module tree is outdated | **High** |
| 3 | **vks-auth: RS256/PS256** with `kid` + 90-day rotation, **Argon2id** passwords, `refresh.rs`/`password.rs`/`lockout.rs`, **ldap3+GSSAPI**, MFA (TOTP) | **HS256** single secret (no kid/rotation), **PBKDF2-SHA256**, no refresh/password/lockout/MFA modules, LDAP is a trait+mock (no ldap3 dep) | ADR-0021 (RS256) debt unimplemented; crypto weaker than documented; 4 documented modules absent | **Critical** |
| 4 | **vks-signaling: tokio-tungstenite (WS)** primary + **wtransport/QUIC** mandatory (R5) | Both are **optional-feature stubs** (`TungsteniteSession`, `WtSession` no-op); default build has only mocks | R5 "mandatory second transport" is a no-op; doc presents stubs as real | **High** |
| 5 | **vks-cluster: NATS JetStream** (persistent, 3-node HA) | In-memory `Bus` pub/sub; **no async-nats/nats crate anywhere**; `/readyz` reports `nats=true` unconditionally | "Cluster" is simulated; ADR-0014 has zero dependency presence | **High** |
| 6 | **vks-media-mcu: oxideav-scene + libyuv bilinear** (ADR-0004), separate `decoder.rs`/`encoder.rs` | Nearest-neighbor compositor on 1-byte/px placeholders; `decoder.rs`+`encoder.rs` merged into `frame.rs`; **no libyuv/oxideav in default build** | Real pipeline is a mock; ADR-0004 not wired | **Medium** |
| 7 | **vks-layout: Taffy + hadrone-core** (ADR-0012), normalized `Rect{x,y,w,h:0..1}`, separate `assignment.rs`/`presets.rs`/`mcu_renderer.rs`/`client_renderer.rs`/`storage.rs` | Self-implemented geometry; `Rect` is **u32 px**; single `renderer.rs`; no Taffy/hadrone-core deps | Different representation + merged modules + missing deps | **Medium** |
| 8 | **vks-gateway-sip: rvoip (SIP↔WebRTC)** | `call.rs`+`codec.rs` state machine, **no rvoip crate**; no DTLS-server/ICE-lite here (they're in media-sfu) | Doc says a library that isn't used | **Medium** |
| 9 | **vks-gateway-h323: FFI over H323Plus/OPAL** (real .so/.dll, watchdog, panic hook) | Mock `ffi.rs` bridge; no real C linkage; watchdog is tick-based (see §4 off-by-one) | FFI is simulated | **Medium** |
| 10 | **vks-recorder: S3/local + AES-256-GCM (per-recording DEK + KMS)**, multi-track vs composite modes, 1-h TTL download token | **MemoryStore only**, no S3, no encryption; `RecordingMode` is only `Mcu\|Sfu` (no composite/multi-track distinction); no download token/share link | Storage+encryption+download all missing | **Medium** |

Additional (lower-severity): `vks-core`'s `transparency.rs` and `vks-media-sfu`'s `ice.rs`/`codec.rs`/`udp_loop.rs` are present in code but missing from ARCHITECTURE §3; `vks-server` has no `config.rs`/`bootstrap.rs` (config via `vks_common::config`, bootstrap inlined in `main.rs`) and an undocumented large `frontend.rs`; `vks-admin` names `handlers.rs` not `server.rs`, and lacks documented `layouts.rs`/`records.rs` REST routes; `vks-storage` has one migration (`0001_initial.sql`), not the documented `schema.sql` + multiple migrations.

---

## 4. MCU Engine — Logic Analysis

All references verified against `crates/vks-media-mcu/src/*`.

### 4.1 Audio mixer (`audio_mixer.rs`, `mix()` L34–51) — **partially correct**

- `N` = **count of non-empty buffers**, so a silent participant (empty buffer) **silently raises everyone else's gain** → discontinuous volume jump on join/leave/silence. Inconsistent with the "N Opus → 1 mixed" model.
- AGC `1/√N` keeps the *sum* bounded but is **not true AGC**: two identical +1.0 inputs already hit `tanh(√2)≈0.93` (near-clip from just 2 full-scale sources); power of N identical inputs scales as √N, not constant.
- Operates on **already-decoded PCM only** — no Opus decode/encode (the `oxideav` feature isn't even defined in the default build).
- Missing: sample-rate conversion, channel mixing, per-participant mute/AGC/level.

### 4.2 Compositor (`compositor.rs`, `compose()`/`blit()` L36–101) — **partially correct**

- Nearest-neighbor uniform-fit (letterbox), rects clamped to 1920×1080 canvas; deterministic. Fine as a mock.
- **Pixel format is 1 byte/pixel (grayscale placeholder)** — real RGB/YUV needs a format change; the header/size contract assumes 1 byte/px.
- **The compositor does NOT consume `vks_layout::Renderer`/`DrawCommand`** — it re-implements its own pixel math, so the two renderers can drift (the "matching vks_layout::Renderer" claim at L64 is only about scale).
- Minor: `col >= cw` `break` is dead code in normal cases; `dst < canvas.data.len()` guard is redundant.

### 4.3 VAD (`vad.rs`, `feed_frame()`/`frame_is_loud()` L86–136) — **partially correct, config trap**

- `frame_is_loud` gate is `rms > threshold && zcr >= zcr_threshold`. **`VadConfig::default` sets `zcr_threshold = 0.0` (L34)**, so `zcr >= 0` is always true → **by default only RMS drives the decision; the advertised ZCR "DC filter" is a no-op** unless a caller overrides it.
- Debounce commits at `pending_ms >= debounce_ms` (500 ms, 20 ms frames); the "first frame counts fully" semantics make effective debounce slightly under nominal for non-20 ms-multiple windows.
- No adaptive/noise-floor thresholding; mono only.

### 4.4 Layout bridge (`layout_bridge.rs`, `apply()`/`current()` L104–143) — **partially correct**

- **`crossfade_ms == 0` edge bug**: `current()` returns `previous` when `elapsed_ms == 0`, but with `crossfade_ms == 0` the branch `elapsed >= 0` also matches, so the **first branch wins and returns `previous` forever** — the fade never completes to the new layout.
- `lerp_rect` interpolates x,y,w,h independently → a slot that moves **and** resizes can pass through **zero-area mid-fade** → visible "pop," not a clean fade.
- **Crossfade is per-slot-id, not per-participant**: if two layouts assign different participants to the same slot id, the "fade" blends *geometries* of different participants — a semantic mismatch with "speaker fade."
- No z-order/overlap handling during fade.

### 4.5 Engine tick loop (`engine.rs`, `RoomState::tick` / `McuEngine::tick`) — **partially correct**

- **`self.elapsed_ms += 33` hard-codes the crossfade clock** per tick regardless of real time — a mock clock; if the caller ticks at a different rate, the 300 ms crossfade decouples from real ms.
- **Audio truncation**: `mix(&slices, self.mixed_audio.len().max(960))` — on the first tick `mixed_audio` is empty → output 960 (correct), but on later ticks it's the *previous* length (960), so **any publisher supplying >960 samples is silently truncated** each tick (no resample).
- **VAD sees the *mixed* multi-speaker audio, not per-participant audio** → "speaker detection" is really "room-activity detection"; it cannot attribute activity to a specific participant (architectural mismatch).
- `switch_room_mode` **ignores `_new_mode` and always returns `RoomMode::Mcu`** — a mode switch on an MCU engine is a silent no-op; asking for `Sfu` returns `Mcu` with no error.
- `destroy_room` removes the room, then cleans frames/audio under separate locks — a low-severity race (a concurrent `tick` between the two locks can observe a removed room, but returns `None` safely).
- `composite.clone()` is a full 1920×1080 copy every tick — expensive but correct.

### 4.6 SFU/MCU transparency (`vks-core/src/transparency.rs`, `ModeSwitcher`) — **NOT fully correct**

- **Dual-clock bug (L132–141)**: `switch_mode` creates a **local** `let mut clock = SwitchClock::new(1)` for the candidate's readiness, but the switcher's **own** `self.clock` (built from the `ready_after_ms` constructor arg) is *also* ticked in the loop yet **never consulted** for the readiness decision. So the candidate is "ready" after **exactly 1 tick**, the `ready_after_ms` parameter is effectively **dead**, and the advertised **timeout→rollback is nearly unreachable**.
- **Layout not re-applied on switch**: `apply_layout` (L126–128) only *stores* `current_layout`; `switch_mode` swaps in the candidate **without calling `candidate.apply_layout(current_layout)`**. The ADR-0026 "layout preserved across switch" invariant is **asserted by name but not implemented** (the test only checks storage).
- Synchronous blocking loop over a deterministic clock — a structural limitation for real async engines.
- No re-subscription / re-announcement of publishers+subscribers to the new engine on switch.

### 4.7 Gateway codecs — **not complete**

- **SIP (`vks-gateway-sip/src/codec.rs`, `negotiate()` L114–124)**: returns a **single** `(WebRtcCodec, bool)` chosen by "first audio else first video." For a **mixed audio+video offer it returns only the audio result and discards video** — a single `negotiate` can't represent both streams (the fuller `negotiate_streams` exists but isn't used by `SipCall::new`). `SipCall::new` (call.rs L69–74) also stores `sip_codec` as the *first* offer codec, not the negotiated one → recorded `sip_codec` and negotiated `web_codec` can be different media types.
- **SIP map is narrower than the H.323 map**: G.729 and H.263 are absent from the SIP codec map (present only in the H.323 map) → a SIP terminal offering G.729/H.263 is treated as unknown (H.263 → no video negotiated at all, silently dropped).
- **SIP UAS FSM has no Reject (480/603) path and no re-INVITE renegotiation**; no SDP (payload types/rates/attrs) and no ICE/DTLS in the gateway.
- **H.323 (`vks-gateway-h323`)**: codec mapping is consistent; the call FSM has **no Reject/timeout** (a Setup stuck in Alerting never releases).
- **H.323 FFI off-by-one (ffi.rs)**: the panic path increments `restarts` (L148) **before** returning `Err`, and `restart()` (L172–180) increments it **again** on the next restart → **a single crash+restart counts as 2 restarts**. The existing test only covers the *watchdog* (non-panic) path, so the double-count is untested.
- **H.323 watchdog unit mismatch**: `check` uses `ticks_used > limit` (L80–86); `PRODUCTION_WATCHDOG_LIMIT` is documented as **seconds (5)** but compared against **tick counts** → "5" means 5 ticks, not 5 seconds. The `FfiResult.success=false` branch (L162–164) is unreachable in the mock (success is always true unless the closure panics).

### 4.8 Supporting-port gap

The `MediaEngine` port has **no media-plane pull method** (`get_composite`/`get_mixed_audio`) — the MCU's `tick()` is engine-specific, so consumers must downcast to `McuEngine`. Also `LayoutDescriptor` carries only participant→slot assignments; `layout_from_descriptor` (engine.rs L89–103) **always uses `GridLayout`**, so `Focus`/`Speaker`/`Custom` `LayoutKind` is collapsed to a plain grid server-side.

---

## 5. UI & Control Elements

### 5.1 PNM frontend routes (extracted from `index.bundle-2.2.1.66.js`)

Served under `<base href="/pnm/">`; assets from `/mcu/`. Routes: `/`, `/addressbook` (+`/groups/:groupId`, `/users/:userId`), `/administration`, `/authorization`, `/c/:aliasName`, `/history` (+`/:sessionId`), `/monitoring`, `/no_connection`, `/no_disk_space`, `/no_permissions`, `/registration`, plus nested children (`sessions/:sessionId?`, `conferences/:conferenceId?`, `terminals/:terminalId?`, `roles/:roleId?`, `audiochannels/:audioChannelId?`, `layouts/:layoutId?`, `event_list/:eventId?`, monitoring `sessions`/`conferences`). A catch-all `/:matchAll(.*)` drives the `/no_permissions` 404-style screen.

- Backend SPA-fallback: `/pnm/` + `/pnm/{*rest}` → `index.html` (`frontend.rs` L79–93); `/admin` → 307 → `/pnm/administration` (L119–121); `/` → 307 → `/pnm/` (L104–106).
- **Most PNM routes (addressbook, terminals, roles, audiochannels, layouts, event_list, history, registration, `/c/:aliasName`) have no matching `/api/v3` endpoint** → they rely on the 501 catch-all and render empty/error.

### 5.2 API endpoints the server exposes

- **v1 REST** (`handlers.rs` L407–431): `/healthz`, `/readyz`, `/openapi.json`, `/api/v1/rooms` (+`/{id}`, `/live`, `/mode`), `/api/v1/mode`, `/api/v1/users` (+`/{id}`, `/bulk`, `/sync-ldap`), `/api/v1/settings`, `/api/v1/audit` (+`/export`). **No layouts or records REST routes** (despite `LayoutRecord`/`RecordEntry` in state).
- **v3 adapter** (`api_v3/mod.rs` L49–99): exempt `/api/v3/login`, `/login/status`, `/system/about`, `/about`, `/references`, `/ws`; protected `/password`, `/profile`, `/time-zones`, `/locales`, `/abonents/stop_reasons`, `/users/credentials/expire`, `/ui/system/status`, `/license`, `/features`, `/engine/calibration|diagnostics`, `/system/time`, `/settings/*`, `/waitingroom`, `/conferences/status`, `/ui/sessions`(+`/{id}`), `/sessions`, `/conferences`(GET/POST), `/conferences/{id}`(GET/DELETE), `/ui/conferences`; **catch-all `/api/v3/{*rest}` → 501**.
- **Server-level** (`lib.rs` L37–50): `/metrics` (Prometheus), `/ws` (protobuf signaling).

### 5.3 UI controls → endpoints (what actually works)

- **Login** → POST `/api/v3/login` ✓; **Password change** → PATCH `/api/v3/password` ✓ (fresh token).
- **Conferences** create → POST `/api/v3/conferences` ✓; delete → DELETE `/api/v3/conferences/{id}` ✓; list → GET `/api/v3/ui/sessions` + `/ui/conferences` ✓ (in-memory `RoomRecord`).
- **WS subscribe/unsubscribe** + `ui:users:search` ✓; only `system:status:disk` is pushed back (as `{full:"NORMAL"}`).
- **Mode switch (SFU/MCU): NO `/api/v3` endpoint exists** — the PNM UI has no v3 mode control; only the (orphaned) v1 `POST /api/v1/mode` + `/rooms/{id}/mode` expose it. **The switch is unreachable end-to-end from the legacy UI.**
- **Administration sub-screens** (backup, network, security save, calibration, diagnostics, registration online/offline/license, mediafiles, video settings) → 501 catch-all or static stubs; do not persist.
- **`terminals:configurations:install` / `firmwares:install`** → reply 501 (control exists in UI but is a no-op).

### 5.4 What's missing

- **Orphaned (implemented but never called by PNM):** the **entire `/api/v1/*` surface** — the frontend only speaks `/api/v3`, so v1 is a parallel admin API with no UI consumer.
- **Frontend-called but served only by the 501 catch-all:** `/api/v3/registration/*`, `/system/network/interfaces`, `/system/utils/tcpdumps`, `/abonents/{id}`(+dtmf/fecc), `/audiochannels`, `/c/`, `/call_strings`, `/call_history`(+export/clean), `/conferences/{id}/...` (clone, mail, videorecords, contacts, audiochannels, video_layouts), `/files`, `/groups`, `/roles`, `/schedules`, `/terminals*` (configurations/firmwares), `/users/{id}/credentials`, `/video_layouts`, `/video_layout_grids`, `/videorecords`, `/sessions/{id}/...` (abonents, broadcasts, clone, contacts, invite, rtmp, videorecords, aspeaker, crown, audiochannels), `/engine/abonents`, `/log_events`, `/logs`, `/ui/call_history`, `/ui/session_history`, `/ui/terminals`, `/ui/video_layouts.*`.
- **Stub endpoints (exist but static/empty):** `/engine/calibration`, `/engine/diagnostics`, `/license`, `/features`, `/waitingroom`, `/conferences/status`, `/settings/*` (GET only, no PATCH to persist), `/users/credentials/expire`, `/abonents/stop_reasons`, `/time-zones`, `/locales`, `/ui/system/status` (hardcoded READY).

### 5.5 Auth flow — works end-to-end (v3), with gaps

- **Working chain:** POST `/api/v3/login` → `local_db.authenticate` → `jwt_issuer.issue` → `{token}` (auth.rs L235–316). Two-step lockout (5 fails/15 min user+IP, 10 fails/5 min IP → 15 min block) with `Retry-After` (L34–125, 319–325). Protected routes re-verify the token **per message** (WS) and invalidate tokens issued before the last password change (`iat < password_changed_at`, L172–185). Fail-closed: `jwt_issuer` None → 503 for all `/api/v3` including login (L158–165).
- **Gaps:** (1) the **v1 admin routes use a separate `jwt_auth_middleware` that is NOT wired into the v1 router** (`handlers.rs` L407–431, no `.layer(...)`) → **v1 is effectively unauthenticated in this build**; auth is enforced only on v3. (2) **No RBAC on v3**: `token_middleware` doesn't check role and `/profile` hard-codes `permissions:["all"]` → every logged-in user is treated as fully permitted. (3) `local_db` None + `jwt_issuer` set → login 503 (fragile config coupling).

### 5.6 Monitoring UI (`/pnm/monitoring`)

- A dedicated `/monitoring` SPA route with children `monitoring-sessions`, `monitoring-conferences` + audiochannels/layouts sub-routes; renders `MonitoringLayout`, `MonitoringLeftSidebar`, `MonitoringList` (Session/Conference), `MonitoringRightSidebar`, CPU/Memory/Disk/Network/Resources/Workers widgets, `MonitoringAddressBook`, `MonitoringAudioChannel`, `MonitoringEvent`, and start/clone/contacts popups.
- **It has no `/api/v3/monitoring/*` endpoint** — it's sourced from the same `/api/v3/ui/sessions` + `/ui/conferences` lists as the dashboard, so **"monitoring" is functionally a re-render of the session/conference list with static resource widgets**.
- Resource widgets (CPU/Memory/Network/Disk) have **no live backing endpoint or pushed event** (`/ui/system/status` is a static READY stub, shell.rs L85–103) → numbers are static/zero. Per-session sub-resources (abonents, audiochannels, contacts, invite) all hit the **501 catch-all** → a monitoring session detail cannot load its participants.

**Event-flow gap (affects all of the above):** the server **never pushes server-originated `{"event":...}` frames** except the immediate `system:status:disk` ack; the core `EventStream` (VecDeque) and `ModeService`'s `ModeChangedEvent` (into an in-memory `ModeEventQueue`) are **fire-into-the-void** — no NATS, no WS fan-out is wired in `build_app`, so live updates (participant join/leave, mode change, resources) don't reach the client in real time.

---

## 6. Launch & Service Wiring

### 6.1 Step-by-step launch flow (`crates/vks-server/src/main.rs`)

1. `init_tracing(None, LogFormat::Human)` — **hardcoded Human**; `Json` available but never selected from env; no `RUST_LOG` parsing.
2. `init_metrics()` — registers Prometheus defaults.
3. `Config::load` — arg1 → `configs/default.toml` (if exists) → `None`; **no `VKS_CONFIG` env var**; malformed TOML silently falls back to defaults.
4. Storage init — **SQLite if `database_url` starts with `sqlite:`, else Memory**; SQLite failure falls back to Memory (L73–85). **Postgres DSN silently → Memory.**
5. `AppState` + `JwtIssuer` (HS256, **fail-closed when `VKS_JWT_SECRET` empty**) + `LocalDb` operator bootstrap + `Users.create` (L91–116) → `sync_from_storage` (L118).
6. `Frontend::load` (CSP from inline scripts in `index.html`; missing `index.html` → `/pnm/` 404s but server keeps running, `FRONTEND_DENIED` incremented).
7. `build_app` (L136).
8. Bind `TcpListener(config.listen_addr)` + `axum::serve` with graceful shutdown (SIGINT/SIGTERM on unix, `ctrl_c` otherwise; L139–176).

**No readiness gating before bind** — the listener binds immediately after `build_app`; `/readyz` is a static all-OK stub.

### 6.2 What's wired in the composition root

`vks-common` (config/tracing/metrics), `vks-core` (domain + Storage port), `vks-storage` (SQLite/Memory), `vks-auth` (JwtIssuer, LocalDb), `vks-admin` (v1 REST + v3 adapter + health + `api_v3/ws`), `vks-signaling` (Envelope + `SignalingHandler` on `/ws`), `vks-media-sfu` (**only in dev tests via `UdpMediaLoop`**). A single `Arc<AppState>` is shared by v1, v3, and auth middleware.

### 6.3 What's NOT wired (compiled but unused)

- **`vks-server` does not construct a `MediaEngine` at all** — neither `vks-media-sfu::engine` nor `vks-media-mcu` is instantiated in `main.rs`; **no room ever gets real media in the running binary.**
- **ORPHANED:** `vks-media-mcu`, `vks-gateway-sip`, `vks-gateway-h323`, `vks-recorder`, `vks-cluster`, `vks-layout` (and effectively `vks-media-sfu`) — 7 of 15 server-side crates are "compiled but never called."
- **No background tasks:** no `tokio::spawn` for heartbeat, lease renewal, recorder, or mode-event fan-out; no WebTransport/QUIC listener, no UDP media socket, no TURN service, no NATS connection, no vks-cluster `ShutdownSequence`.
- **Signaling uses `HandlerConfig::default_test()`** (`lib.rs` L133) — hardcoded `server_id "srv-0001"`, `server_name "vks-signaling-test"`, host candidate `203.0.113.7:49152`; `Config`'s `server_name` is not passed through.
- **Mode switch has no runtime effect on clients:** `ModeEventQueue` is an in-memory dead-end (no NATS, no WS fan-out in `build_app`).
- **QoE/SLO are dead code:** `vks-common::qoe`/`slo` are exported but referenced by no other crate; `/metrics` serves only HTTP/signaling/frontend counters.

### 6.4 Configuration

- `Config` (10 fields): `server_name`, `listen_addr`, `auth_provider`, `database_url`, `ldap_url`, `default_room_mode`, `frontend_dir`, `jwt_secret`, `jwt_ttl_secs`, `admin_login`, `admin_password`. Env override `VKS_*` (highest) → TOML → defaults. **Only one config file exists: `configs/default.toml`** (7 lines) — ARCHITECTURE §3's `dev.toml`/`production.toml` and the `crates/vks-server/configs/` dir are absent.
- **Not configurable at all:** media/UDP port, TURN url, TLS cert/key, log format/level, codec feature flags, NATS url, cluster `instance_id`, recorder settings.
- `default.toml` omits `frontend_dir`/`jwt_secret`/`jwt_ttl_secs`/`admin_login`/`admin_password` → they fall to `Config::default()` (jwt_secret empty → **v3 fail-closed**).
- **`VKS_JWT_PRIVATE_KEY` (PEM, for RS256) is documented in BACKEND_RULES §1.1 but never read** — `Config` only has `jwt_secret` (HS256).
- `auth_provider`/`ldap_url` are loaded but **never consumed** (binary uses `MockLdapProvider`); `LocalDb` operator is registered even when `auth_provider=ldap`. `default_room_mode` is logged but **the signaling `ServerHello` always emits `room_mode=0`** (`handler.rs` L163) — the configured mode is not propagated to clients. **No config validation** (`auth_provider`/`default_room_mode` are free Strings).

### 6.5 Port map

- Documented (CLUSTERING.md): LB → 443, 8443, 3478, 5349 + RTP 40000–60000 direct UDP; TURN TLS 5349 + DTLS 3478.
- `Config::default().listen_addr = "0.0.0.0:443"`, but **shipped `configs/default.toml` sets `127.0.0.1:8080`** — port 8080 appears **nowhere** in the documented port map.
- **Actual listening ports in the running binary: exactly ONE** — `config.listen_addr` (HTTP+WS, plain TCP, `main.rs` L139). No 8443, 3478/5349 (TURN), 40000–60000 (RTP), 5060 (SIP), 1720 (H.323).
- **No TLS at 443** despite the default being the documented TLS entry point — `axum::serve` is plain TCP (cleartext). No way to configure separate signaling/media/admin ports (single `listen_addr`).

---

## 7. ADR Compliance Matrix

| ADR | Status | Compliant? | Notes |
|---|---|---|---|
| ADR-0001 | Rust, MSRV 1.94 | ✅ Yes | 100% Rust; MSRV pinned in `rust-toolchain.toml` + `Cargo.toml`. |
| ADR-0002 | MCU/SFU transparency, single `MediaEngine` trait | ⚠️ Partial | Trait + both engines exist, but **binary only links SFU** → MCU mode can't be served; `ModeSwitcher` readiness is dead (1 tick) and layout not re-applied. |
| ADR-0003 | str0m SFU | ⚠️ Partial | str0m is **optional**, not default; engine not instantiated in `main.rs`. |
| ADR-0004 | oxideav-scene MCU | ⚠️ Partial | `oxideav` optional feature; nearest-neighbor placeholder compositor; crate orphaned from server. |
| ADR-0005 | protobuf, no JSON in hot path | ✅ Yes (for `/ws`) | `/ws` is prost-encoded. `/api/v3/ws` JSON is a documented ADR-0027 exception (legacy adapter only). |
| ADR-0006 | SIP = rvoip | ⚠️ Partial | `rvoip` optional, not default; crate orphaned; no 5060 listener. |
| ADR-0007 | H.323 = H323Plus FFI | ⚠️ Partial | Mock FFI behind empty `h323plus` feature; crate orphaned; no 1720 listener. |
| ADR-0008 | sqlx + PG prod / SQLite single-node / memory tests | ✅ Yes | All three exist; but shipped default is `sqlite::memory:` (data lost on restart). |
| ADR-0009 | JWT + API keys + LDAP | ⚠️ Partial | JWT (HS256) + `LocalDb` + `api_key` exist; LDAP is mock; **RS256 debt open** (ADR-0027). |
| ADR-0010 | Dioxus | ⏸️ Deferred | By accepted ADR-0027; UI is legacy PNM Vue SPA; no Dioxus crate. Phase-8 revisit trigger. |
| ADR-0011 | shadcn | ⏸️ Deferred | Same as ADR-0010. |
| ADR-0012 | Taffy + hadrone-core layout | ⚠️ Partial | `vks-layout` self-implemented; **no Taffy/hadrone-core dep**; `Rect` is u32 px, not normalized. |
| ADR-0013 | progenitor (OpenAPI client) | ⏸️ Deferred | No progenitor-generated client (ADR-0027). |
| ADR-0014 | NATS cluster bus | ⚠️ Partial | In-memory `Bus` only; **no NATS crate**; `/readyz` `nats=true` hardcoded; 3-node JetStream absent. |
| ADR-0015 | TURN = turn-rs | ❌ No | **No turn-rs/coturn anywhere**; `TurnConfig` is a settings DTO only; `ServerHello.ice = None`. |
| ADR-0016 | Hexagonal + Sans-I/O | ✅ Yes | `vks-core` ports; media crates socket-free; `vks-signaling` transport-agnostic. |
| ADR-0017 | Codec feature flags | ⚠️ Partial | opus/vp8 default + vp9/av1 flags; **no h264/h265/g722/g711 flags** → R3 codec list not fully flagged. |
| ADR-0018 | proto_version in ServerHello | ⚠️ Partial | Field exists + version window enforced, but **proto comment says min=2/max=3 while code uses `[1..=1]`** (`handler.rs` L19–20) → a v2/v3 client would be rejected. |
| ADR-0019 | Audit log mandatory | ✅ Yes | v1 `/api/v1/audit` + `audit.rs`; ADR-0027 adds v3 login/lockout audit. |
| ADR-0020 | chromiumoxide E2E | ⏸️ Deferred | CI e2e job commented out until phase 8. |
| ADR-0021 | E2EE deferred | ✅ Yes | No E2EE code path. |
| ADR-0022 | RBAC Operator/Moderator/Participant/Viewer | ⚠️ Partial | `Role` exists; v1 auth middleware optional; **no per-route RBAC** beyond Operator checks in mode/users; v3 has no role check. |
| ADR-0023 | License | 📝 Proposed | Not accepted; workspace license is `MIT OR Apache-2.0` placeholder. |
| ADR-0024 | MSRV 1.94 | ✅ Yes | — |
| ADR-0025 | WebRTC in WASM (web-sys) | ✅ Yes | Client SDK wasm uses web-sys; native str0m optional. |
| ADR-0026 | Global default + per-room mode override | ⚠️ Partial | `ModeState` (global + overrides + epoch) + v1 routes exist; but **effective mode never sent to clients** (`room_mode=0`) and no NATS fan-out. |
| ADR-0027 | PNM adopt-and-shim | ✅ Yes (most complete) | `frontend.rs`, `/api/v3` adapter (fail-closed JWT + 501 catch-all), `configs/default.toml`; **HS256→RS256 debt recorded**. |

**Summary:** 10 of 27 ADRs are only PARTIALLY met in the running binary — the recurring pattern is **"library implemented, composition root doesn't wire it"** (SFU/MCU engines, gateways, cluster, recorder). ADR-0014 (NATS) and ADR-0015 (TURN) have **zero** dependency presence.

---

## 8. Critical Issues (must fix before production)

1. **No media plane in the running binary.** `vks-server` never instantiates a `MediaEngine` — neither `vks-media-sfu` (only in dev tests) nor `vks-media-mcu` (orphaned) — so **no room ever gets real media**. `crates/vks-server/src/main.rs` (no engine construction), `lib.rs` (`build_app`). This breaks R2 (MCU/SFU) and the whole media path.
2. **Transparency invariant (ADR-0002/0026) broken at the composition root.** Only SFU is linked → `default_room_mode=mcu` can never be served; `ModeSwitcher::switch_mode` (`vks-core/src/transparency.rs` L132–150) has a **dead `ready_after_ms`** (readiness = 1 tick, so timeout→rollback is nearly unreachable) and **does not re-apply the stored layout** to the new engine (L126–128 only stores).
3. **Mode switch unreachable end-to-end from the UI.** No `/api/v3` mode endpoint; the PNM UI has no v3 mode control; even via v1, `ModeChangedEvent` goes to an in-memory `ModeEventQueue` that **no WS fan-out consumes** (`vks-admin/src/mode.rs`, `lib.rs` `build_app`). The in-place switch flow is incomplete.
4. **v1 admin API is effectively unauthenticated.** `jwt_auth_middleware` exists (`auth_middleware.rs`) but is **not layered onto the v1 router** (`handlers.rs` L407–431) — only v3 enforces auth.
5. **Auth crypto weaker than documented (ADR-0009/0021).** `vks-auth/jwt.rs` uses **HS256** (single secret, no `kid`/rotation) vs documented RS256/PS256; `local_db.rs` uses **PBKDF2-SHA256** vs documented **Argon2id**; documented `refresh.rs`/`password.rs`/`lockout.rs`/MFA modules are absent; LDAP is a trait+mock (no ldap3 dep).
6. **7 of 15 server-side crates orphaned.** `vks-media-mcu`, `vks-gateway-sip`, `vks-gateway-h323`, `vks-recorder`, `vks-cluster`, `vks-layout` (and effectively `vks-media-sfu`) are compiled but never linked/called — cluster, recording, SIP, H.323, and layout planes are absent from the process.
7. **No real NATS (ADR-0014) or TURN (ADR-0015).** `vks-cluster/bus.rs` is in-memory (no async-nats/nats crate); no turn-rs/coturn; `ServerHello.ice = None` (`handler.rs` L165) → clients are never given a TURN server; `/readyz` reports `nats=true` unconditionally. R11/R13 unmet end-to-end.
8. **ADR-0018 proto_version mismatch.** `signaling.proto` comment says `min_supported=2/max_supported=3`, but `SignalingHandler` hardcodes `[1..=1]` (`handler.rs` L19–20) → a v2/v3 client per the spec would be rejected.
9. **Shipped default is in-memory + cleartext + undocumented port.** `configs/default.toml` → `database_url=sqlite::memory:` (data lost on restart), `listen_addr=127.0.0.1:8080` (not in the documented port map), no TLS at the default 443, `jwt_secret` empty → **v3 fail-closed** out of the box.
10. **No deployment artifacts.** No `Dockerfile`, no `docker-compose.yaml`, no `deployments/` dir, no Grafana/Prometheus scrape config — the documented "server + postgres + nats + coturn + grafana" deployment is **unbuildable from the repo**.

---

## 9. Warnings (should fix)

1. **vks-monitoring doesn't exist as a crate** — `qoe`/`slo`/`ratelimit`/`metrics` are in `vks-common`, but `PROGRESS.md`/Phase 7 describe a separate crate. Also `qoe`/`slo` are **dead at runtime** (never instantiated) → R14 QoE/SLO not actually measured.
2. **ARCHITECTURE §3 module tree is stale** — shows a unified `vks-gateway/`, `client/vks-admin-ui`+`vks-meeting-ui`, and `deployments/` that don't exist; misses `transparency.rs`, `ice.rs`, `codec.rs`, `udp_loop.rs`, `frontend.rs`, `api_v3/`.
3. **Dependency tree deviates from the "strict tree" in 6+ places** — `vks-common` standalone (doc: ← vks-core); `vks-layout`, `vks-recorder`, `vks-cluster`, `vks-auth`, `vks-signaling`, `vks-media-sfu`, `vks-admin` all dropped documented edges (dep-light). `vks-proto::cluster` is dead generated code.
4. **Both real signaling transports are stubs behind optional features** (`ws` → `TungsteniteSession` stub; `wtransport` → `WtSession` no-op); default build has only mocks — R5's "mandatory second transport" is a no-op.
5. **MCU logic gaps (all `vks-media-mcu/src/*`):** audio mixer counts N by non-empty buffers (gain jumps on silence) and 1/√N is not true AGC; compositor is 1-byte/px + re-implements scaling (ignores `vks_layout::Renderer`); **VAD's ZCR filter is a no-op by default** (`zcr_threshold=0.0`); **layout bridge `crossfade_ms=0` never completes** and fades per-slot-geometry not per-participant; engine tick **truncates audio to 960**, **hard-codes a 33 ms mock clock**, and **feeds the mix (not per-participant) to the VAD**; `switch_room_mode` is a silent no-op always returning `Mcu`.
6. **Gateway SIP `negotiate` returns a single codec** (discards video on a mixed offer) and `SipCall` records a mismatched `sip_codec`; **SIP map is narrower than H.323** (G.729/H.263 missing); SIP UAS has **no Reject/re-INVITE**; H.323 FSM has **no Reject/timeout**.
7. **H.323 FFI off-by-one** (panic + restart counts as 2 restarts — untested path) and **watchdog unit mismatch** (documented "5 seconds" vs implemented "5 ticks"); `FfiResult.success=false` branch unreachable in the mock.
8. **`/api/v3` has no RBAC** (`/profile` hard-codes `permissions:["all"]`), and most PNM data screens (addressbook, terminals, roles, audiochannels, layouts, history, registration) hit the **501 catch-all**; room **update (PATCH)** exists only on v1, so the UI can create/delete but not edit a room.
9. **PostgreSQL DSN parsed but unused** — `main.rs` only checks `starts_with("sqlite:")`, so a `postgres://` DSN silently falls to `MemoryStorage`.
10. **Monitoring UI is a re-render** of `ui/sessions`+`ui/conferences` with **static resource widgets** (no live CPU/Mem/Disk/Net endpoint or pushed event) and per-session sub-resources hit the 501 catch-all.
11. **`vks-recorder` has no S3/encryption** (MemoryStore only), no composite-vs-multi-track distinction, no 1-h TTL download token; **no `/api/v1/records` or `/api/v1/layouts` REST routes** despite the state records existing.
12. **Config gaps:** no `VKS_CONFIG` env, no TLS/log/codec/TURN/NATS/cluster/recorder config fields, no validation on `auth_provider`/`default_room_mode` (free strings), `VKS_JWT_PRIVATE_KEY` documented but unread.
13. **Signaling uses `HandlerConfig::default_test()`** — hardcoded server id/name and a fake host candidate `203.0.113.7:49152`; `Config.server_name` isn't passed through; `ServerHello` always `room_mode=0`.
14. **`vks-storage` has a single migration** (`0001_initial.sql`), not the documented multi-file `schema.sql`; `vks-server` has no `config.rs`/`bootstrap.rs` (inlined), and `vks-admin` names it `handlers.rs` not `server.rs`.

---

## 10. Recommendations (next steps, prioritized)

| # | Action | Fixes |
|---|---|---|
| 1 | **Instantiate a `MediaEngine` in the composition root and wire both modes.** In `crates/vks-server/src/main.rs`, construct `vks-media-sfu::engine` and `vks-media-mcu::McuEngine`, select via `Config.default_room_mode`, and hold the active one in `AppState`. This alone unblocks R2 and makes the binary actually serve media. | Critical #1 |
| 2 | **Fix the transparency invariant.** In `vks-core/src/transparency.rs`: (a) make readiness use the **constructor's `ready_after_ms`** (drop the local 1-tick clock, or actually consult `self.clock`), and (b) **re-apply `current_layout` to the candidate in `switch_mode`** (`candidate.apply_layout(&self.current_layout)`), then re-attach publishers/subscribers. Add a test that asserts the new engine *receives* the layout. | Critical #2 |
| 3 | **Close the mode-switch event loop.** Add an `/api/v3/mode` + `/api/v3/conferences/{id}/mode` endpoint (mirror the v1 routes) and wire `ModeEventQueue` → a real WS/NATS fan-out in `build_app` so clients actually receive `room.mode_changed` and renegotiate. | Critical #3 |
| 4 | **Wire v1 auth and add v3 RBAC.** Layer `jwt_auth_middleware` onto the v1 router (`handlers.rs` L407–431), and make `token_middleware` enforce `Role` (stop hard-coding `permissions:["all"]`). | Critical #4 + ADR-0022 |
| 5 | **Reconcile auth crypto with the ADRs, or accept and document the debt.** Implement RS256/PS256 + `kid` + rotation and Argon2id (ADR-0009/0021), or formally downgrade the ADRs. Add the missing `refresh`/`password`/`lockout`/MFA modules or drop them from the docs. | Critical #5 |
| 6 | **Wire or explicitly defer the 7 orphaned crates.** Add `vks-cluster`, `vks-recorder`, `vks-gateway-sip`, `vks-gateway-h323` to the server (with background `tokio::spawn` tasks for heartbeat/lease/recorder) **or** mark them `// phase N` in the workspace with an ADR; remove the dead `vks-proto::cluster` artifact and the dep-light edges (or update ARCHITECTURE §3). | Critical #6 + Warning #3 |
| 7 | **Bring up real NATS + TURN, or stub them honestly.** Add an `async-nats` dependency to `vks-cluster` (back the `Bus` with JetStream) and a `turn-rs`/coturn for TURN; advertise the TURN server in `ServerHello.ice` instead of `None`; make `/readyz`'s `nats` flag reflect reality. | Critical #7 |
| 8 | **Fix the ADR-0018 version window** so the proto comment and `SignalingHandler` agree (use `[2..=3]` per the spec, or correct the comment), and pass `Config`-driven values (server name/id, ICE candidates, `room_mode`) into `SignalingHandler` instead of `HandlerConfig::default_test()`. | Critical #8 + Warning #13 |
| 9 | **Ship a sane production default + deployment.** Change `configs/default.toml` to a Postgres DSN + a documented port, add TLS (or a `VKS_TLS_*` config + an `axum::serve` TLS listener), add `dev.toml`/`production.toml`, a `VKS_CONFIG` env, and validation on `auth_provider`/`default_room_mode`; add `deployments/{Dockerfile,docker-compose.yml}` + a Prometheus/Grafana scrape config. | Critical #9/#10 |
| 10 | **Fix the MCU correctness gaps** in `crates/vks-media-mcu/src/*`: make the audio mixer's AGC power-normalizing and count *all* participants (not just non-empty); give the compositor a real RGB/YUV format and consume `vks_layout::Renderer`; set a non-zero default `zcr_threshold` (or document RMS-only); handle `crossfade_ms=0` in the layout bridge and fade per-participant; use a real clock + resample in the engine tick; make `switch_room_mode` honor the requested mode; and fix the gateway SIP `negotiate` to return per-stream results + add G.729/H.263 + Reject/re-INVITE, plus the H.323 FFI restart off-by-one and watchdog seconds-vs-ticks. | Warning #5/#6/#7 |

**Sprint sequencing: 1→2→3→4** (make the binary actually serve media, switch transparently, and notify clients), **5→6** (auth hardening + wiring the orphaned planes), **7→8** (real infra), **9→10** (deployment + correctness polish).
