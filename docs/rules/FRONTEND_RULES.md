# FRONTEND RULES — детальные правила фронтенда

> Статус: **v0.1 (черновик)**. Заполняется в фазе 8.
> Краткие правила — [`PROJECT_RULES.md` Часть III](../PROJECT_RULES.md). Здесь — детали.

## 1. Трое SPA

| SPA | Назначение | Bundle |
|---|---|---|
| `vks-admin-ui` | Управление сервером | ≤ 800 KB gz |
| `vks-meeting-ui` | UI участника конференции | ≤ 500 KB gz |
| `vks-client-sdk` | SDK (WASM) | ≤ 500 KB gz |

## 2. Admin UI — страницы

| Страница | Содержание |
|---|---|
| `/login` | JWT login, MFA |
| `/dashboard` | **Health indicators** (server, DB, NATS, TURN, media, recorder) + KPI (rooms, participants, bitrate, CPU, uptime) |
| `/rooms` | CRUD + **live viewer** (Observer) + bulk actions (mute/kick) + pin/favorites |
| `/rooms/:id` | Участники (live), треки, layout, записи, аудит, QoE per participant |
| `/users` | CRUD + LDAP sync + bulk actions |
| `/layouts` | **Редактор** (drag slots, resize, z, assignment, save, assign) |
| `/settings` | Имя/IP, БД, **AuthProvider (local/ldap/oidc) + params**, TURN, Кодеки, Режим, Layout, Лимиты, Retention, Rate limiting, TLS, **Backup/Restore** |
| `/monitors` | Time-series (bitrate, participants, QoE MOS), алерты |
| `/records` | Список, inline-превью, скачивание, удаление, retention |
| `/audit` | Фильтры (user/action/date/IP), export CSV, пагинация |
| `/logs` | Live tail (WS/SSE), filter level/crate/room |

## 3. Meeting UI — экраны и компоненты

| Компонент | Описание |
|---|---|
| **PreJoinScreen** | Предпросмотр камеры/микрофона, имя, выбор устройства, «Присоединиться»/«Без звука» |
| **VideoGrid** | vks-layout + vks-client-sdk, **layout switcher** (grid/speaker/focus/custom) |
| **Controls** | Mic, cam, **screen share**, **raise hand**, settings (devices, quality, background), выйти |
| **ParticipantList** | Moderator: mute, remove, promote, pin, **waiting room admit** |
| **Chat** | (опционально) + **Reactions** (👏😂❤️) |
| **InMeetingNotifications** | Join/leave, recording, mode switch, announcement, auto-quality (toast 5s) |
| **RecordingIndicator** | «⏺ Запись» + consent modal |
| **QualityIndicator** | Popup: bitrate, jitter, RTT, loss, codec, layer + «Качество адаптировано» |
| **ReconnectionOverlay** | «reconnecting...» (backoff), «disconnected» + retry/leave |
| **Timer** | Длительность встречи, auto-end (moderator) |
| **LeaveConfirmation** | Participant: dialog; Moderator: «Завершить для всех» |

## 4. UX-правила

### 4.1. Hotkeys
| Key | Действие |
|---|---|
| M | Mute/unmute |
| C | Cam on/off |
| S | Screen share |
| R | Raise hand |
| F | Fullscreen |
| ? | Help (list of shortcuts) |
| Esc | Close modal / exit fullscreen |
| Ctrl+K (admin) | Global search |
| G R/U/L (admin) | Rooms/Users/Layouts |

### 4.2. Mobile
- Portrait: vertical list + active speaker top.
- Landscape: grid.
- Touch: swipe (next/prev), tap (spotlight), long-press (reactions).
- Haptic: navigator.vibrate on mute/unmute/reaction.
- Touch targets ≥ 44px.

### 4.3. A11y
- ARIA roles, keyboard navigation, contrast.
- `prefers-reduced-motion`, `prefers-contrast: more`.
- Font size: auto/small/medium/large.
- High contrast theme.

### 4.4. i18n
- ru (default) + en.
- `locales/ru.json`, `locales/en.json`.
- Intl API for dates/numbers.

### 4.5. Themes
- Dark / Light / High contrast (CSS variables).
- `prefers-color-scheme` + manual toggle.

## 5. PWA
- Manifest + service worker.
- Cache-first for assets (hash-named).
- Offline banner + auto-reconnect.
- Installable (Add to Home Screen).

## 6. SDK ↔ UI интеграция

### 6.1. State sync
- SDK = **source of truth** (participants, tracks, quality, connection).
- UI signals = **mirror** (updated by events).
- UI **не** делает read-modify-write на SDK state.
- Конфликты → в пользу SDK (re-sync по event).

### 6.2. Events (SDK → UI)
- SDK events: очередь внутри `Conference`, забирается `drain_events()`.
- Dioxus: poll/interval → `drain_events()` → обновление signals.
- Каждый event → соответствующий signal.

### 6.3. Commands (UI → SDK)
- Direct call через Context: `conf.mute().await?`.
- Debounce (быстрые нажатия = 1 команда).
- UI **не** дублирует состояние SDK.

### 6.4. Cleanup
- UI вызывает `conf.leave()` при unmount.
- SDK idempotent (leave() ×2 = OK).
- Navigation: cleanup перед unmount.

### 6.5. Errors
- SDK errors → Event `Error(Error)` → UI toast (non-critical) / modal (critical: AuthFailed, Disconnected).

## 7. SDK публичный API

### 7.1. Types
- `Conference`: `join(url, token)`, `leave()`, `create_render_surface(target)`, `mute()`, `unmute()`, `start_screen_share()`, `stop_screen_share()`, `on_event(cb)`, `state()`.
- `Participant`: `id`, `name`, `role`, `is_speaking`, `is_muted`.
- `Track`: `id`, `kind` (Audio/Video/Screen), `codec`, `bitrate`.
- `RenderSurface`: abstraction over DOM node.
- `Events`: `ParticipantJoined/Left`, `TrackAdded/Removed`, `QualityChanged`, `Reconnecting`, `Disconnected`, `ModeChanged`, `RecordingStarted/Stopped`, `Announcement`, `RaiseHand`, `React`, `Error`.
- `Error`: `AuthFailed`, `NetworkLost`, `IceFailed`, `CodecUnsupported`, `ModeSwitchInProgress`, `PermissionDenied`, `Other(String)`.

### 7.2. Reconnection
- Auto: exponential backoff 1s→16s, max 5.
- Token refresh: SDK хранит refresh token.
- ServerMaintaining: SDK получает нового owner.
- Events: `Reconnecting(attempt, next_in_ms)`, `Disconnected(reason)`.

### 7.3. Devices
- `list_audio_inputs/outputs()`, `set_audio_input/output(id)`.
- `list_video_inputs()`, `set_video_input(id)`.
- Hot-plug: `devicechange` → auto-switch + Event.

### 7.4. Q8: WebRTC in WASM
- **(а) web-sys + WebRTC API браузера**: RTCPeerConnection, codecs = browser. Bundle < 500KB.
- **(б) str0m in wasm32**: full stack, codecs from feature flags. Bundle ~1-2MB.
- **Решение: ADR (Q8 open)**.

## 8. Deployment
- Docker image (multi-arch amd64/arm64, cosign signature, SBOM).
- Helm chart (K8s, probes, resources, HPA).
- Standalone binary (musl-static + systemd).
- Docker-compose (single-node).

## 9. Release
- SemVer (X.Y.Z).
- Tag → CI: cargo publish, Docker, Helm.
- Changelog: `docs/CHANGELOG.md`.
- Rollback: image-tagging, Helm `--rollback`.
- Миграции БД: add-only в minor, destructive в major.

## 10. Legacy UI PNM (временно; снятие — фаза 5)

> Отклонение от §13.1 PROJECT_RULES (Dioxus) зафиксировано в **ADR-0027**; здесь — детали.

- До фазы 5 admin-интерфейс обслуживается рантаймом legacy TrueConf MCU UI
  (`client/pnm-frontend/deployed/pnm`, раздаётся сервером: `/pnm/`, `/mcu/*`); UI обращается
  к compat-слою `/api/v3/*` — это **не** наш public API (в `openapi.json`/progenitor не попадает).
- Бюджет первой загрузки: критический путь (index bundle + css + ru_RU) ≈ 479 125 B gz ≤ 800 KB
  (§18.4 PROJECT_RULES); legacy-сборка (~6,3 MB, ~22 % рантайма) — в замыкании `index.html`, принята осознанно.
- Правки рантайма — только с cache-bust (`client/pnm-frontend/NOTES.md` §«Правки рантайма и кэш»).
- Фаза 5: Dioxus-шелл покрывает ≥80 % экранов ⇒ `/api/v3` и legacy-ассеты удаляются вместе (ADR-0027).
