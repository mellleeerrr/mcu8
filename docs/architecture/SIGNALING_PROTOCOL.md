# SIGNALING PROTOCOL — спецификация сигнального протокола

> Статус: **v0.1 (черновик)**. Заполняется в фазе 2.
> Протокол: **protobuf** (prost). JSON запрещён в hot path (ADR-0005).
> Транспорт: **WS/WSS** (основной) + **WebTransport/QUIC** (обязательный, R5).
> Версионирование: `proto_version` в `ServerHello` (ADR-0018).

---

## 1. Envelope (фрейминг)

Каждый WS/WSS-фрейм — один protobuf `SignalingMessage`. Каждый запрос обязан получить reply (`reply_to = msg_id`) или `Error`.

```protobuf
syntax = "proto3";
package vks.signaling;

message SignalingMessage {
  uint64 msg_id   = 1;  // ULID, уникаемый идентификатор сообщения
  uint64 reply_to = 2;  // msg_id запроса; 0 = push (server → client)
  oneof payload {
    ClientHello       client_hello       = 10;
    ServerHello       server_hello       = 11;
    JoinRoom          join_room          = 20;
    JoinRoomResult    join_room_result   = 21;
    LeaveRoom         leave_room         = 30;
    SdpOffer          sdp_offer          = 40;
    SdpAnswer         sdp_answer         = 41;
    IceCandidate      ice_candidate      = 50;
    IceComplete       ice_complete       = 51;
    SubscribeRequest  subscribe_request  = 60;
    SubscribeResponse subscribe_response = 61;
    Unsubscribe       unsubscribe        = 62;
    TrackAdded        track_added        = 70;
    TrackRemoved      track_removed      = 71;
    LayoutChanged     layout_changed     = 80;
    RoomModeChanged   room_mode_changed  = 90;
    ParticipantJoined participant_joined = 100;
    ParticipantLeft   participant_left   = 101;
    SpeakingStateChanged speaking_state_changed = 102;
    QualityReport     quality_report     = 120;
    Announcement      announcement       = 121;
    Error             error              = 200;
    Ack               ack                = 201;
    ServerMaintaining server_maintaining = 210;
  }
}
```

## 2. Auth handshake

### 2.1. ClientHello
```protobuf
message ClientHello {
  uint32 proto_version = 1;      // версия протокола клиента
  string client_id      = 2;     // ULID, стабильный (для resume/reconnect)
  string sdk_version    = 3;     // версия SDK
  string user_agent     = 4;     // browser/OS
  string auth_token     = 5;     // короткий room JWT (§9.1 PROJECT_RULES)
  string room_id        = 6;     // UUID комнаты
  repeated string capabilities = 7; // "simulcast","svc","mcu_renderer","sfu_renderer","e2ee_sframe"
  optional string resume_nonce = 8; // reconnect (см. §7)
}
```

### 2.2. ServerHello
```protobuf
message ServerHello {
  uint32 proto_version = 1;
  uint32 min_supported = 2;      // минимальная поддерживаемая версия
  uint32 max_supported = 3;      // максимальная поддерживаемая версия
  string server_id     = 4;      // UUID инстанса
  string server_name   = 5;
  RoomMode room_mode   = 6;      // SFU | MCU
  RoomCaps caps        = 7;      // кодеки, ICE, TURN
  IceConfig ice        = 8;      // STUN/TURN URL + credential
}

message IceConfig {
  repeated IceServer servers = 1;
}
message IceServer {
  string url        = 1;  // "turn:turn.example.com:3478?transport=udp,tls"
  string username   = 2;  // short-lived credential
  string credential = 3;
}
```

**Версионирование**: server принимает client если `client.proto_version ∈ [min_supported, max_supported]`. Mismatch → `Error{code:"version_mismatch"}` + close. Новые поля — `optional`/default; breaking — только новый major.

## 3. Join / Leave

### 3.1. JoinRoom
```protobuf
message JoinRoom {
  string room_id      = 1;
  string display_name = 2;   // имя участника
  bool   is_recorder  = 3;   // если это recorder (не обычный participant)
}
```

### 3.2. JoinRoomResult
```protobuf
message JoinRoomResult {
  string participant_id     = 1;  // назначает сервер (UUIDv7)
  uint64 join_nonce         = 2;  // для resume/reconnect (TTL 60s)
  repeated ParticipantInfo participants = 3; // snapshot всех участников
  repeated TrackInfo publisher_tracks   = 4; // треки publisher'ов
  Layout layout               = 5;          // текущий layout комнаты
  RoomCaps caps               = 6;
}

message ParticipantInfo {
  string participant_id = 1;
  string display_name   = 2;
  bool   is_recorder    = 3;
  bool   is_speaking    = 4;
  repeated string track_ids = 5;
}
```

### 3.3. LeaveRoom
```protobuf
message LeaveRoom {
  string room_id = 1;
}
```

## 4. SDP / ICE

```protobuf
message SdpOffer  { string sdp = 1; }
message SdpAnswer {
  string sdp        = 1;
  repeated RtpMap rtp_map = 2;
}
message RtpMap {
  uint32 payload  = 1;
  string codec    = 2;
  uint32 clock    = 3;
  uint32 channels = 4;
}
message IceCandidate {
  string sdp_mid           = 1;
  uint32 sdp_mline_index   = 2;
  string candidate         = 3;
  bool   end_of_candidates = 4;
}
message IceComplete {
  bool   success = 1;
  uint64 rtt_ms  = 2;
}
```

**ICE-lite**: SFU/MCU = host candidates only (RFC 8445 §2). Client = full ICE (host+srflx+relay). TURN allocation: client → TURN (ALLOCATE, CREATE_PERMISSION, BINDING to SFU).

**DTLS**: WebRTC — client = DTLS client (RFC 8831). Gateway (SIP/H.323) — gateway = DTLS server.

## 5. Subscribe / Unsubscribe

```protobuf
message SubscribeRequest {
  string subscriber_id          = 1;
  repeated string source_track_ids = 2; // SFU: чьи tracks; MCU: ["composite"]
  repeated string preferred_codecs = 3;
  uint32 max_bitrate_bps       = 4;
}
message SubscribeResponse {
  bool   ok                 = 1;
  repeated MediaStreamInfo streams = 2;
  string error_code         = 3;
}
message MediaStreamInfo {
  uint32 ssrc      = 1;
  string codec     = 2;
  string direction = 3;  // "sendonly" | "recvonly"
  string track_id  = 4;
}
message Unsubscribe {
  repeated string track_ids = 1;
}
```

**Subscription model (SFU)**: по умолчанию full-mesh (subscriber получает все треки кроме своего). SFU: `HashMap<TrackId, Vec<SubscriberHandle>>`. Subscriber может `Unsubscribe` с конкретного track.

**MCU**: subscriber подписывается на `["composite"]` (один композитный трек).

## 6. Track / Participant events

```protobuf
message TrackAdded {
  string participant_id = 1;
  TrackInfo track       = 2;
}
message TrackRemoved {
  string participant_id = 1;
  string track_id       = 2;
}
message ParticipantJoined {
  string participant_id = 1;
  string display_name   = 2;
  bool   is_recorder    = 3;
}
message ParticipantLeft {
  string participant_id = 1;
  uint64 reason         = 2;  // 0=normal, 1=timeout, 2=kicked, 3=mode_switch
}
message SpeakingStateChanged {
  string participant_id = 1;
  bool   speaking       = 2;
  uint32 level          = 3;  // 0-100
}
```

## 7. Reconnect / Resume

1. `JoinRoomResult.join_nonce` (UUIDv7), привязан к `(room_id, participant_id)`, **TTL 60s**.
2. Reconnect: `ClientHello{client_id, resume_nonce, room_id}` → server валидирует nonce → `JoinRoomResult` со snapshot.
3. ICE/DTLS resumption (если DTLS жив), иначе полный ICE+DTLS. `participant_id` сохраняется.
4. Истёк nonce → полный re-join (новый `participant_id`).

**Client backoff**: exponential 1s→16s, max 5 попыток. `ServerMaintaining` (failover) — основной механизм.

## 8. RoomModeChanged

```protobuf
message RoomModeChanged {
  RoomMode new_mode     = 1;  // SFU | MCU
  RoomCaps caps         = 2;
  uint64 switch_epoch   = 3;  // для идемпотентности
  bool   graceful       = 4;  // true = in-place switch (1-2s), false = full restart
}
```

**SDK handling**:
1. Unsubscribe all.
2. Create new SdpOffer (new m-lines).
3. Wait SdpAnswer.
4. ICE (resumption).
5. Switch renderer (`sfu_renderer` ↔ `mcu_renderer`).
6. Layout из сообщения (сохраняется при переключении).

**Timing**: 0–0.1s `RoomModeChanged`; 0.1–0.3s unsubscribe+offer; 0.3–0.8s SDP/ICE; 0.8–1.5s first frame; 1.5s renderer cut. **Worst 3s**. Timeout → rollback.

## 9. LayoutChanged

```protobuf
message LayoutChanged {
  Layout layout = 1;
}

// Layout (эффективная геометрия из vks-layout + метаданные админки)
message Layout {
  uint32 version = 1;
  repeated Slot slots = 2;
  string kind     = 3;  // "grid" | "focus" | "speaker" | "custom"
  string preset   = 4;  // T10a: raw id схемы из админки; "" = неизвестно
  string mode     = 5;  // T10a: "auto" | "dynamic" | "manual"; "" = неизвестно
}
message Slot {
  uint32 id        = 1;
  Rect   rect      = 2;  // normalized (0..1)
  uint32 z         = 3;
  SlotAssignment assignment = 4;
  optional string participant_id = 5; // для FIXED
}
message Rect {
  float x = 1;  // 0..1
  float y = 2;
  float w = 3;
  float h = 4;
}
enum SlotAssignment {
  SLOT_ASSIGNMENT_AUTO   = 0;
  SLOT_ASSIGNMENT_FIXED  = 1;  // + participant_id
  SLOT_ASSIGNMENT_PINNED = 2;
}
```

**Синхронизация**: push по комнате через `RoomHub` → `LayoutChanged` broadcast.
Pull — снимок раскладки в `JoinRoomResult` (для позднего участника).

> **Факт на 2026-10-23 (после правки Ц7 и ADR-0030):** сообщение
> `LayoutChanged` **формируется и отправляется**: `GatewayManager::apply_layout`
> после успешного применения раскладки в MCU рассылает `LayoutChanged`
> участникам комнаты (`crates/vks-server/src/layout_signal.rs`).
> Поздний участник получает снимок раскладки **в самом `JoinRoomResult`**
> (поле 5) — отдельного push-кадра после ответа нет (он создавал гонку:
> push мог прийти позже ответа).
> NATS-канал `vks.cluster.room.layout_changed` по-прежнему заглушка
> (`vks-cluster`) — межузловая рассылка раскладки не реализована.
>
> **Источник истины:** на провод уходит **эффективная раскладка композитора**
> (`McuEngine::layout_snapshot`), а не дескриптор: в режиме `auto` дескриптор
> приходит без геометрии (её строит движок по числу участников), и клиент
> получил бы пустой экран. Прямоугольники композитора уже кламплены в холст.
>
> **Единицы координат:** `Rect` в протоколе — нормализованные `[0.0, 1.0]`
> относительно холста MCU 1920×1080 (пересчёт пикселей делает сервер, клиент
> рисует плитки в своём разрешении).
>
> **Нумерация слотов:** `Slot.id` — всегда 0-based (контракт
> `LayoutDescriptor`); `position` в админском API остаётся 1-based.
>
> **Значение `assignment`:** `PINNED` — закреплённый слот, `FIXED` — занятый
> участником, `AUTO` — свободный.
>
> **Метаданные схемы (T10a, ADR-0032):** `Layout.preset`/`Layout.mode` — это
> справочная проекция админ-состояния (raw `preset`, нормализованный `mode`),
> а не вторая истина: геометрия авторитетна. `mode` — зеркало редактора, а не
> топология комнаты (`RoomModeChanged`) и не «как построена геометрия»:
> `auto + явный preset` (Codian/конструктор) даёт `mode:"auto"` и геометрию
> пресета. Авто-сетка, созданная самим движком, тоже помечается `mode:"auto"`
> (`register_publisher`) — это провенанс «сетку построил движок», а не выбор
> пользователя. `preset` — raw-id хранимой схемы; `size` на провод не
> выводится, полный id/число окон клиент восстанавливает по `slots.len()`.
> Пустая строка = «сервер не сообщает» (legacy-кадр), не `auto`.
> `layout_id` на провод не выводится (COW-нестабильный UUID админ-записи).
> `preset` длиннее 128 байт нормализуется в `""` (unknown).
> Обновление — только в одном `LayoutChanged` (без отдельного
> `LayoutModeChanged`), источник — атомарный `McuEngine::layout_snapshot`.
>
> Полный контракт — `docs/decisions/0030-layout-signaling-contract.md`,
> `docs/decisions/0032-layout-metadata-on-the-wire.md`.

## 10. Quality / Error / Ack

```protobuf
message QualityReport {
  float   mos        = 1;  // 1-5
  uint32  rtt_ms     = 2;
  uint32  jitter_ms  = 3;
  uint32  loss_pct   = 4;
  string  direction  = 5;  // "uplink" | "downlink"
}
message Announcement {
  string text       = 1;
  uint64 issued_at  = 2;  // unix ms
}
message Error {
  string code      = 1;  // "auth_failed" | "room_not_found" | "version_mismatch" | ...
  string message   = 2;
  bool   retryable = 3;
}
message Ack {
  string acked_msg_id = 1;
}
message ServerMaintaining {
  string new_owner_id   = 1;  // UUID нового инстанса-владельца
  string connect_url    = 2;  // куда переподключиться
  uint64 grace_period_ms = 3; // время на переподключение
}
```

## 11. Session FSM

```
Disconnected → Authenticating → Connected → Joining → InRoom
  InRoom → Negotiating → MediaActive ↔ InRoom
  InRoom → Leaving → Disconnected
  any → Disconnected (timeout / error / close)
```

| Переход | Триггер |
|---|---|
| → Authenticating | WS connect, `ClientHello` |
| → Connected | JWT valid, `ServerHello` sent |
| → Joining | `JoinRoom` received |
| → InRoom | `JoinRoomResult` sent |
| → Negotiating | `SdpOffer` received |
| → MediaActive | `IceComplete` (success) |
| → Leaving | `LeaveRoom` received |
| → Disconnected | timeout / error / close |

Каждый переход — `tracing::event!` + метрика.

## 12. Disconnect handling

| Тип | Инициирует | Server action | Client action |
|---|---|---|---|
| Unexpected (FIN/RST, QUIC close) | transport | remove participant, `ParticipantLeft`, destroy media | onclose → reconnect (backoff) |
| Timeout (keepalive 30s × 3 miss) | server | close WS, remove, broadcast | onclose → reconnect |
| Graceful (`LeaveRoom`) | client | ack, remove, broadcast | onclose → done |
| ServerMaintaining (failover) | server | `ServerMaintaining{new_owner}` | reconnect to new owner |

## 13. Congestion control (SFU)

- **BWE**: subscriber → SFU (TWCC + REMB). SFU НЕ шлёт BWE обратно.
- **Simulcast**: клиент шлёт 3 spatial layers (low/med/high, разные SSRC). SFU выбирает layer по BWE: High ≥ 800kbps, Med ≥ 200kbps, Low.
- **SVC**: VP9/AV1 — 1 spatial + N temporal. Деградация: сначала drop temporal, потом spatial.
- **NACK/FEC**: subscriber → SFU, SFU retransmits из jitter buffer (~200ms). FEC — задача publisher, SFU форвардит.
- **Congestion ladder**: BWE < high → High→Med; BWE < med → Med→Low; BWE < audio → drop video, audio only. Recovery: hysteresis +5%.
