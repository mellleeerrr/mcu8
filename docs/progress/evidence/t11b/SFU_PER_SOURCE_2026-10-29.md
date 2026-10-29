# T11b — per-source SFU: N−1 mids, >2 пиров, честный cap

**Статус:** реализовано, юнит- и live-проверено; ожидает ревью.
**Коммиты:** `14e7027` (мульти-mid + cap), `6343f45` (найденный e2e-баг
идентичности участника), этот коммит (drift-guard, e2e, доки).

## Что именно сделано

### 1. Direction-aware offer/answer и пулы mid'ов (`crates/vks-media-sfu/src/webrtc.rs`)

- `parse_sections()` читает `a=mid:`/`a=sendrecv|sendonly|recvonly|inactive`
  и port 0 (→ `Inactive`); answer сохраняет **одну m-line на каждую
  предложенную** (RFC 3264 §6) — 1+7/1+7 offer даёт 16 m-lines в answer,
  а не сжатие до 1+1.
- `PeerMids`: пул mid'ов на `(publisher, kind)`. Сначала выдаются
  dedicated `sendonly`-слоты (pre-offered recvonly браузера), затем
  `sendrecv` (сохраняет легаси 1+1, где у пира только его publish-mid).
  `mid_for_source` не выдаёт один mid двум паблишерам; `retire_source`
  не возвращает mid в пул (поколения/retire), `source_for_mid` — обратный
  маппинг для RTCP PLI/FIR.
- `validate_offer`: до **8 audio + 8 video** m-lines (T11b ceiling) вместо
  «по одной на вид»; ошибка называет точный потолок. Кодек вне SSOT-среза
  answer'а — громкая `Sdp`-ошибка, а не `unreachable!`.
- `offer_publishes()` смотрит **все** секции: peer с первой `recvonly`-секцией
  и второй `sendrecv` больше не считается «только зрителем».

### 2. Fanout по `(publisher, mid)` (`crates/vks-media-sfu/src/webrtc_room.rs`)

- `PeerCommand::{EnsureSource, DropSource, Write{from}}`: регистрация
  источников идёт жадно при join (в порядке generation — детерминированно),
  ленивый fallback остаётся в `Write`. Исчерпание пула/retire = явный drop
  источника: `vks_webrtc_sources_total{result="dropped_no_mid"}` +
  `vks_webrtc_frames_dropped_total{reason="no_egress_mid"}`, **никогда** не
  переиспользование mid и не склейка двух RTP-часовых доменов.
- Keyframe-gate теперь **per publisher** (`HashSet<ParticipantId>` внутри
  `PeerHandle`): дельты паблишера не уходят подписчику до его IDR; gate
  перевзводится при re-subscribe и переполнении очереди; PLI-запросы
  коалесцируются (500 мс на `(room, publisher, kind)`), чтобы поток
  gated-дельт не штормил паблишера.
- `remove()` рассылает `DropSource` выжившим → retired mid остаётся retired.
- `RoomLimits` (`VKS_WEBRTC_MAX_PEERS_PER_ROOM`, default **8**;
  `VKS_WEBRTC_MAX_PEERS`, default 256) парсится **fail-closed**: пустое,
  нечисловое, `0`, `-1`, `1.5` — ошибка старта (exit 78), не тихий default.
  Сверх лимита — `room_full` (`retryable=true`); исчерпание UDP-портов —
  транспортная ошибка (`WebRtcError::Net`), не `room_full`.

### 3. Сервер

- `webrtc_runtime::preflight_candidate` валидирует лимиты до bind;
  `registry()` при ошибке парсинга **не создаёт** транспорт (offers fail
  closed). Предупреждение, если cap > `MAX_VIDEO_MIDS` (клиент с максимальным
  pre-offer'ом увидит не всех — это считается метрикой, а не молчит).
- `real_peer_tracks()`: настоящий пир объявляет **один** `MediaTrack` на
  опубликованный вид. 1+8 pre-offer — это 9 m-lines, но 2 трека; раньше
  `TrackAdded` дублировался 8× на video и раздувал publisher registry.
- `embedded_uuid` (коммит `6343f45`): `part-<room>-<uuid7>` содержит два
  UUID, и старый код брал первый — **все пиры комнаты получали один
  ParticipantId**. Второй браузер получал «DTLS fingerprint changed»,
  multi-peer был невозможен в принципе. Теперь берётся последний UUID;
  регрессионный тест запрещает room-uuid затенять participant-uuid.

### 4. D1: вендорский бандл SDP не содержит

План требовал `scripts/patch-pnm-webrtc-mids.py`. Проверка бандла:
`RTCPeerConnection` встречается только в feature detection
(`"RTCPeerConnection" in window`), вызовов SDP API (`createOffer`,
`setLocalDescription`, `addTransceiver`, `a=mid:`) нет. Патчить нечего —
поэтому деливерабл заменён на fail-closed drift-guard
`scripts/check-pnm-webrtc-drift.py`: сегодня это no-op (exit 0), но CI
падает (exit 1) в момент, когда бандл начнёт говорить SDP — тогда
per-source политику нужно перепроверить против реального клиента.
6 unit-тестов (clean ×2, drift ×2, feature-detection не drift, missing
vendor → exit 2, не-text файлы игнорируются) + шаг в
`frontend-integrity` job.

### 5. Метрики

`vks_webrtc_sources_total{result=allocated|retired|dropped_no_mid}`,
материализованы `vks_webrtc_frames_dropped_total{reason=
queue_full|awaiting_keyframe|no_egress_mid}`.

## Гейты (фактические числа)

| Гейт | Результат |
|---|---|
| `cargo fmt --all -- --check` | ok |
| `cargo clippy -p vks-media-sfu --features str0m --offline -- -D warnings` | ok |
| `cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets --offline -- -D warnings` | ok |
| `cargo test -p vks-media-sfu --features str0m --offline` | **49 passed** (после T11b v2 + P3 generation/cleanup/metric fix; базовый old suite 47 в review D убрали phase) |
| `cargo test -p vks-media-sfu --offline` (без feature) | 80 passed (str0m-тесты gated) |
| `cargo test -p vks-signaling --offline` | 49 passed |
| `cargo test -p vks-server --offline` | lib **160**, main 6, config_failclosed 1, h265 1, network_integration 22, operator_policy 7, recorder_tap 7 — 0 failed |
| `python3 scripts/check-pnm-webrtc-drift.py --check` | exit 0, «70 text files, no SDP API» |
| `python3 -m unittest test_check_pnm_webrtc_drift` | 6 tests ok |
| `node --check tests/e2e/webrtc.js` + все 70 vendor JS | ok (0 fail) |
| `sha256sum -c MANIFEST.sha256` | 215/215 (vendor не менялся) |
| workspace clippy `--all-features --all-targets -D warnings` | **не мой блокер**: падает на 2× `redundant closure` в `crates/vks-admin/src/api_v3/shell.rs` (незакоммиченный WIP другого агента); в файлах T11b находок нет, `-p vks-server --no-deps` — чисто |

Новые тесты media-sfu: 1+8/8+8 negotiation + 16 m-lines в answer, 8+8 reoffer
round-trip и отклонение перестановки mid'ов с сохранением сессии, уникальные
per-source mid'ы и исчерпание/retire, легаси 1+1 детерминизм, `offer_publishes`
по всем секциям, `parse_sections` (port 0/inactive/default), реальный
DTLS/SRTP handshake с 8 audio mid'ами и доставкой трём паблишерам на разные
mid'ы, cap 3 → 4-й `room_full`, fail-closed `RoomLimits::parse`, порт-эксауст
≠ `room_full`, 3 подписчика через mpsc с per-publisher gate, `DropSource`
broadcast, детерминированный порядок fanout.

## Live browser e2e (3 страницы, Chromium 1243)

Запуск: scratch-конфиг в `/dev/shm` (БД там же), `listen 127.0.0.1:18099`,
`media_listen_addr=127.0.0.1:0`, `VKS_CANDIDATE_ADDR=192.168.1.93`,
`VKS_WEBRTC_PORT_MIN/MAX=47000/47040`, `VKS_WEBRTC_MAX_PEERS_PER_ROOM=4`,
`VKS_E2E_PEER_CAP=4`. Артефакты: `docs/progress/evidence/t11b/e2e-2026-10-29/`.

**46/46 checks PASS** (`result.json`), в т.ч.:

- alice/bob/carol: offer 16 mids, answer зеркалит 16/16, `Opus+VP8 only`;
- ICE connected + DTLS `SRTP_AEAD_AES_256_GCM` на всех трёх;
- каждая страница получает **N−1 аудио-потоков на отдельных mid'ах**
  (`[["2",22154],["3",20828],["4",6898]]` у alice) и **2+ удалённых видео на
  разных mid'ах** (`[["9",326],["10",294],["11",67]]`);
- легаси 1+1 в комнате 4 паблишеров: answer ровно 2 mid'а, видит ровно одного
  паблишера (mid `1`), а `vks_webrtc_sources_total{result="dropped_no_mid"} 4`
  — деградация явная, без склейки часов;
- 5-й оффер при cap=4 → `room_full`, `retryable=true`, повтор на том же сокете
  снова `room_full` (FSM rollback);
- `audio_only`: аудио идёт, видео-секция согласована, но payload = 0;
- 0 pageerrors.

`h265_rtp` один раз упал по таймингу x265 и дважды прошёл подряд — это
известная нестабильность теста, не T11b (файл не менялся).

Первый прогон на `candidate_addr=127.0.0.1` показал ICE fail (браузер
собирает host-кандидаты 192.168.1.93/10.200.0.1, loopback не достижим) —
это ограничение окружения, не кода; с LAN-кандидатом всё соединилось.

## Остаточное (P2, вне T11b)

- `RoomLimits` читается из env в момент `registry()`; смена лимита требует
  рестарта (как и остальные `VKS_*`).
- Per-source пул — фиксированные 8 слотов на вид; simulcast/rid и выбор
  качества (T12) в этот пул не входят.
- Запись >2 источников (recording farm) остаётся T12 — T11b запись не трогал.
- `media_healthy=false` в e2e-прогоне: `media_listen_addr=127.0.0.1:0` — это
  dev-режим legacy UDP loop, WebRTC-транспорт от него не зависит.
