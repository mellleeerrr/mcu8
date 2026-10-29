# T11b — независимое ревью A: per-source egress mids и честный per-room cap

- **Ревизия:** `14e7027` (`T11b: per-source egress mids + honest per-room cap`),
  родитель `3e2983c`, ветка `master`.
- **Метод:** pristine-копия `git archive 14e7027` → `/dev/shm/t11b_revA_1934931384`
  (не live-дерево); Rust **1.94.0**; `CARGO_TARGET_DIR=/dev/shm/t11b_revA_target`;
  все сборки/тесты — `--offline --locked`; live-дерево не редактировалось, кроме
  этого отчёта; live `vks_data.db`, порты `:8080`/`:18082` и `backups/` не
  открывались.
- **Вердикт:** **done: no**, **7.5/10**.
- **Дефекты:** **P0 = 0, P1 = 2, P2 = 3, P3 = 7.**

Коротко: серверная часть T11b сделана содержательно хорошо. Direction-aware
answer, 16 m-line на 8+8, уникальные per-source mids, retirement без
переиспользования, per-publisher keyframe gate, fail-closed парсинг cap,
дедуп `TrackAdded` — всё подтверждено исполнением на реальном str0m
(`AcceptedPeer`, DTLS/SRTP). Но ревизия **не является приёмочно-полной**:
(1) в дереве `14e7027` живёт старый баг `embedded_uuid`, из-за которого все
пиры одной UUID-комнаты получают один `ParticipantId`, — реальная комната на
2+ пира на этом коммите невозможна, то есть cap 8/per-source fanout между
настоящими пирами не достижимы; (2) единственный e2e-клиент не обновлён и
по-прежнему ждёт `room_full` на третьем пире при cap по умолчанию 8, а e2e
«1+8» в этом коммите отсутствует. Оба P1 — блокеры приёмки; первый
исправлен в `6343f45` **вне** ревьюируемого коммита, второй — в `2b89bc6`.

---

## 1. Гейты и счётчики (pristine `14e7027`)

| Гейт | Команда | Результат |
|---|---|---|
| fmt | `cargo +1.94.0 fmt --all -- --check` | **exit 0** |
| clippy targeted | `cargo +1.94.0 clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets --locked --offline -- -D warnings` | **exit 0**, 0 warnings |
| clippy workspace | `cargo +1.94.0 clippy --workspace --all-features --all-targets --locked --offline -- -D warnings` | **exit 0**, 0 warnings |
| media-sfu | `cargo +1.94.0 test -p vks-media-sfu --all-features --locked --offline` | **112 passed, 0 failed, 0 ignored**; 3 прогона подряд — 112/112/112 |
| signaling | `cargo +1.94.0 test -p vks-signaling --locked --offline` | **49 passed, 0 failed** |
| server lib | `cargo +1.94.0 test -p vks-server --lib --locked --offline` | **151 passed, 0 failed, 3 ignored** |
| e2e | `tests/e2e/webrtc.js` | **не запускался**: на `14e7027` он заведомо не соответствует новой семантике (P1-2), а multi-peer путь блокирован P1-1 |

Счётчики **точно совпадают** с заявленным baseline: media-sfu 112,
signaling 49, server lib 151. Потерь/ослабления тестов нет; `+14` media-sfu
и `+1` server — это новые T11b-тесты из коммита.

Дополнительно, чтобы не путать «тесты зелёные» с «приёмка выполнена», в
pristine-копию (только в `/dev/shm`, **не в коммит**) были добавлены четыре
review-probe теста:

| Probe (scratch, не закоммичен) | Результат |
|---|---|
| `review_probe_accepted_peer_answers_16_mids_and_default_cap_is_8` | **pass**: реальный `WebRtcRoomRegistry::accept_offer` → `AcceptedPeer`; answer 16 `a=mid:`, 14 `a=sendonly` + 2 `a=sendrecv`, порядок mids **байт-в-байт как в offer**; 8 пиров приняты, 9-й — `RoomFull` |
| `review_probe_h264_offer_is_an_error_never_a_panic` | **pass**: H264-only → `InvalidOffer`, паники нет; Opus+H264 → answer 2 секции, audio `SendRecv`, video `Inactive` (T04b SSOT: H264 в WebRTC Unsupported, без `unreachable!`) |
| `review_probe_multi_peer_participant_ids_are_distinct` | **FAIL**: `part-<room>-<alice>` и `part-<room>-<bob>` дают **один** `ParticipantId` (room UUID), а не разные — см. P1-1 |
| `review_probe_view_only_peer_is_not_announced_as_publisher` | **FAIL**: all-recvonly pre-offer 8+8 даёт 2 локальных трека (Audio+Video) — см. P2-1 |

---

## 2. Что реально работает (проверено кодом и исполнением)

1. **RFC 3264 answer.** `accept_offer` больше не сворачивает оффер к 1+1:
   на 8 audio + 8 video приходит 16 m-line в том же порядке; направления
   `sendrecv` для своих publish-секций и `sendonly` для 7+7 pre-offered
   egress-слотов. Подтверждено scratch-probe на реальном `AcceptedPeer`
   (не только unit).
2. **Per-source mids.** `PeerMids` (webrtc.rs:160-307) выдаёт каждому
   паблишеру отдельный mid; `retire_source` уносит mid в `retired` и не
   возвращает в пул; `rebuild` на реоффере сохраняет живые аллокации.
   Тест `per_source_audio_mids_are_unique...` — 8 уникальных audio-mid,
   9-й `None`, retired не переиспользуется. Реальный DTLS/SRTP-тест
   `real_dtls_handshake_with_eight_audio_mids...` пишет три разных payload
   в три разных mid и получает их на клиенте.
3. **Keyframe gate per publisher.** `PeerHandle.video_ready:
   Arc<Mutex<HashSet<ParticipantId>>>` (webrtc_room.rs:117), `admits_video`,
   `forget_video`, `rearm_video`; тест `keyframe_gate_is_per_publisher`
   проверяет, что IDR паблишера B не открывает gate A, а `forget/rearm`
   работают. Глобального `AtomicBool` больше нет.
4. **Retirement при уходе.** `remove()` рассылает `DropSource` выжившим
   (webrtc_room.rs:567-600); тест `removing_a_publisher_retires_its_source_in_every_subscriber`.
   `DropSource` в `run_peer` вызывает `retire_source` и чистит
   `no_egress_mid` (webrtc_room.rs:543-547).
5. **Честный cap.** `RoomLimits` (webrtc_room.rs:50-103), default 8,
   fail-closed parse (`8x`, `""`, `" "`, `"0"`, `"-1"`, `"1.5"` → Err);
   admission использует `self.max_peers_per_room` в двух проверках
   (webrtc_room.rs:392-405 и :464-475); глобальный `max_peers` сохранён.
   Тест на cap 3 (`peers_are_admitted_up_to_the_per_room_cap`) и scratch-probe
   на default 8 (9-й — `RoomFull`) проходят. `port_range_exhausted` — `Net`,
   не `RoomFull`.
6. **Дедуп треков.** `real_peer_tracks` + `new_track_kinds` дают 2 трека на
   1+7 pre-offer вместо 16; тест `real_peer_tracks_are_one_per_kind` и
   серверный тест на 8+8 (`tracks.len() == 2`) проходят.
7. **T04b SSOT.** `webrtc_answer_specs()` = Opus+VP8; answer не содержит
   H264/VP9/AV1/H265; при H264-only — `InvalidOffer`, при Opus+H264 —
   видео-секция `Inactive`, паники нет.
8. **Метрики.** `vks_webrtc_sources_total{result=allocated|retired|dropped_no_mid}`
   и `vks_webrtc_frames_dropped_total{reason=awaiting_keyframe|no_egress_mid}`
   материализуются в `init_default` (metrics.rs:268-276); `room_full`,
   `capacity_exceeded`, `port_range_exhausted` уже были.

---

## 3. Findings

### P1-1. Все пиры одной UUID-комнаты получают один `ParticipantId`; реальная комната на 2+ пира на `14e7027` невозможна

**Где:**
- `crates/vks-signaling/src/handler.rs:352` — id сессии:
  `format!("part-{}-{}", room_id, Uuid::now_v7())`, то есть
  `part-<room_uuid>-<participant_uuid>` (два UUID).
- `crates/vks-server/src/lib.rs:553-567` — `embedded_uuid` возвращает
  **первый** parseable UUID (room), `:576-587` — `parse_participant_id`
  использует его.
- Следствие в сервере: `lib.rs:1019,1092` и далее — `m.participant_id`
  парсится в один и тот же `ParticipantId` для alice и bob; второй оффер
  уходит в `accept_reoffer` и отклоняется по fingerprint
  (`crates/vks-media-sfu/src/webrtc.rs:501-506`), либо заменяет пира после
  connect-deadline.

**Почему это P1 для T11b:** это не косметика, а полный блокер
multi-peer-приёмки. Пока все пиры комнаты — один participant, `WebRtcRoomRegistry`
видит второй браузер как re-offer первого; `max_peers_per_room = 8`
недостижим, per-source fanout между настоящими пирами не запускается,
e2e «1 publisher + N viewers» невозможен. In-process тесты этого не ловят,
потому что генерируют `ParticipantId::new()` напрямую, минуя сигналинг.

**Доказательство исполнением:** scratch-probe
`review_probe_multi_peer_participant_ids_are_distinct` **упал**:
`part-<room>-<alice>` → room UUID, а не alice UUID. Это же подтверждает
follow-up `6343f45` («every peer in a room shared one ParticipantId…
blocks T11b's 3+ peers acceptance»), но `6343f45` **не входит** в
ревьюируемый `14e7027`.

**Статус:** исправлено вне этого коммита (`6343f45` — последний UUID).
Для приёмки T11b как серии коммитов это надо учитывать; для приёмки
именно `14e7027` — блокер.

---

### P1-2. E2e-клиент не обновлён: он всё ещё 1+1 и ждёт `room_full` на третьем; e2e «1+8» в коммите отсутствует

**Где:** `tests/e2e/webrtc.js`:
- `:5-7` — заголовок «Two Chromium pages»;
- `:211-247` — `createBrowserPeer` делает только `pc.addTrack(...)` для
  audio+video, то есть оффер 1+1; `addTransceiver('video', {direction:'recvonly'})`
  и pre-offer 1+7 отсутствуют;
- `:402-425` — третий пир обязан получить `room_full`, `retryable`, и
  повторный оффер тоже `room_full`.

После `14e7027` default cap = 8, а третий пир при 1+1-офферах **принимается**.
Единственный end-to-end клиент репозитория становится красным; проверок
«16 m-line в answer», «N−1 audio-mid на подписчика», «у каждого 2+ удалённых
видео на разных mid» нет. Приёмочный пункт «e2e 1+8 / 3+ пира» в этом
коммите не выполнен; он появляется только в `2b89bc6` (3 страницы, 1+7,
`VKS_E2E_PEER_CAP`), который в данном ревью не рассматривается.

**Усугубляющий фактор:** из-за P1-1 любой реальный e2e на 2+ пира в одной
UUID-комнате падает ещё раньше — на втором участнике. То есть на `14e7027`
нельзя получить end-to-end доказательство ни старого 1+1-сценария, ни
нового multi-peer.

---

### P2-1. `real_peer_tracks` игнорирует направление SDP: view-only пир анонсируется как паблишер

**Где:** `crates/vks-server/src/lib.rs:1498-1525` — `real_peer_tracks`
перебирает `media` и проверяет только kind + accepted codec; поле
`Media.direction` (`crates/vks-signaling/src/sdp.rs:92-93`) не читается.
Для all-recvonly оффера (8 audio + 8 video recvonly) функция возвращает
Audio+Video `MediaTrack`; далее `add_publisher`/`seat_publisher` и
`TrackAdded` (`lib.rs:1150-1197`) регистрируют пира как паблишера, занимают
слот раскладки и рассылают два фальшивых `TrackInfo`.

`offer_publishes` (`crates/vks-media-sfu/src/webrtc.rs:836-862`) для такого
оффера корректно возвращает `false`, но в `real_peer_tracks` не используется.
Коммит-месседж утверждает «a view-only peer is no longer announced as a
publisher» — для серверного анонса это **неверно**.

**Доказательство:** scratch-probe
`review_probe_view_only_peer_is_not_announced_as_publisher` **упал**,
вернув `[Audio, Video]` на all-recvonly 8+8.

**Оценка:** P2 — медиа-путь не ломается (fanout от view-only пира не идёт),
но реестр паблишеров/раскладка/UI получают фальшивый трек; для T11b с его
pre-offer-формой это реальный сценарий «смотрю, не публикую».

---

### P2-2. `WebRtcRoomRegistry::new` не fail-closed, хотя документация обещает обратное

**Где:** `crates/vks-media-sfu/src/webrtc_room.rs:247-253`:

```rust
let limits = RoomLimits::from_env().unwrap_or_else(|error| {
    tracing::error!(%error, "WebRTC admission limits are invalid; refusing the whole WebRTC plane");
    RoomLimits::default()
});
Self::with_limits(config, advertise_ip, limits)
```

При `VKS_WEBRTC_MAX_PEERS_PER_ROOM=8x` лог говорит «refusing the whole
WebRTC plane», а конструктор возвращает рабочий реестр с default 8. Это
публичный API; серверный путь безопасен (`webrtc_runtime::preflight` →
`:89` и `registry` → `:188-195` используют `with_limits` и возвращают
`None`/ошибку), но library-embedder или тест, вызвавший `new`, получает
fail-open. Либо `new` должен возвращать `Result`, либо комментарий/лог
должны быть честными.

**Оценка:** P2; приёмочный пункт «invalid -> startup error» для `vks-server`
выполнен, для публичного `new` — нет.

---

### P2-3. Документация не обновлена: cap=2 всё ещё описан как норма, политика деградации и mid-ceiling не задокументированы

**Где:**
- `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:48-50` — «не более 2
  реальных WebRTC-пиров на комнату (`MAX_PEERS_PER_ROOM`)»;
  `:112` — «третий пир получает `room_full`»; `:123` — «жёстко ограничено
  2 пирами на комнату».
- `deploy/README.md:86-88` — эффективная ёмкость считается только как
  `min(VKS_WEBRTC_MAX_PEERS, размер UDP-диапазона)`; `VKS_WEBRTC_MAX_PEERS_PER_ROOM`
  и формула `min(per-room × rooms, max_peers, ports)` отсутствуют.
- `docs/audit/2026-10-29/00_MASTER_PLAN.md:63` — всё ещё «8 mids в answer —
  нельзя (RFC 3264)» (неверно; answer обязан зеркалить все m-line).

Политика «legacy 1+1: дроп источника + метрика `dropped_no_mid`, без
склейки двух Opus-доменов» живёт только в комментариях
(webrtc_room.rs:526-539 и :562-573), но не в `deploy/README`/архитектурной
доке. Приёмочный пункт «documented policy» в `14e7027` не выполнен;
обновление доков — в `2b89bc6`.

**Оценка:** P2. Код честный, внешняя документация врёт (cap=2) и молчит о
новых ключах/деградации.

---

### P3-1. Drift-детектор и его CI/import-обвязка в этом коммите отсутствуют

`scripts/check-pnm-webrtc-drift.py` и `scripts/test_check_pnm_webrtc_drift.py`
на `14e7027` **нет** (`ls` → No such file); в `.github/workflows/ci.yml`,
`scripts/import-pnm-frontend.sh`, `client/pnm-frontend/NOTES.md` нет
`webrtc-drift`. `client/pnm-frontend/MANIFEST.sha256` — **215** строк,
не изменён. Это не ложный зелёный гейт (гейта вообще нет), поэтому P3, а
не P0-1 из pitfalls: деливерабл «замена вендорского патча на drift-check»
просто перенесён в `2b89bc6`. Если Lead считает его частью T11b, приёмка
по пункту 6 на `14e7027` — «не выполнено», а не «выполнено».

---

### P3-2. Audio-запрос keyframe проходит через проверку `publishes_video`

`request_keyframe_from` (`webrtc_room.rs:720-739`) при `audio=true` всё
равно шлёт команду только если `handle.publishes_video`; а
`enqueue_frame` вызывает её при переполнении очереди и для audio
(`:818`), после чего `run_peer` делает `publish_mid(true)` +
`request_keyframe(mid)` (`:992-1006`). Для audio это семантически
бессмысленный PLI (ключевых кадров у Opus нет), но транспорт не ломается.
Следует либо не коалесить audio-запросы, либо явно игнорировать их.

---

### P3-3. `accept_reoffer` не обновляет `remote_mids`

`webrtc.rs:489-533`: префикс-проверка `keeps_prefix` сравнивает новый
оффер с `self.remote_mids`, но после успешного реоффера `remote_mids`
остаётся от **первого** оффера. Если браузер один раз добавил секции, а
следующим реоффером переставил/убрал их, guard проверяет только старый
префикс; порядок добавленных секций не контролируется. str0m обычно
отклонит структурное изменение сам, но инвариант «re-offer only appends»
становится неполным. Нужно `self.remote_mids = next_mids;` после успеха.

---

### P3-4. Eager-аллокация может быть обогнана lazy-аллокацией из `Write`

`accept_fresh_offer` вставляет handle в `rooms` (`webrtc_room.rs:478`),
затем собирает паблишеров и шлёт `EnsureSource` (`:486-513`), и только
потом спавнит `run_peer` (`:537-538`). Между вставкой и `EnsureSource`
паблишер может успеть отправить кадр: `targets_for` уже видит нового
подписчика, `Write` попадает в очередь раньше `EnsureSource`, и
`run_peer` в `PeerCommand::Write` лениво вызывает `mid_for_source`
(`:922-947`). Тогда слот получает не самый ранний по generation паблишер,
а тот, чей кадр пришёл первым; для legacy 1+1 с одним слотом это ломает
обещанную детерминированность (комментарий `targets_for`, :701-704).
Окно узкое, но реальное; лечение — либо не лениво аллоцировать в `Write`,
либо резервировать/сортировать слоты до вставки в `rooms`.

---

### P3-5. Метрики per-source не полностью «честные»

- `WEBRTC_SOURCES{allocated}` инкрементится только в ветке `EnsureSource`
  (`webrtc_room.rs:522-525`), но не при ленивой аллокации в `Write`
  (`:922-947`) — счётчик занижен.
- `WEBRTC_SOURCES{retired}` инкрементится на каждого выжившего при
  `DropSource` (`:543-547`), даже если у этого выжившего не было mid для
  ушедшего (например, ушёл подписчик, а не паблишер) — счётчик завышен.
- `no_egress_mid` (HashSet) не очищается при появлении нового слота после
  реоффера; если источник уже был помечен dropped, успешная аллокация
  позже не даст `allocated`/снятия метки. Для наблюдаемости это шум.

---

### P3-6. `video_ready` не чистится при уходе паблишера

`DropSource` вызывает `peer.retire_source(from)`, но `PeerHandle.video_ready`
(`webrtc_room.rs:543-547`) не удаляет `from`. Запись безвредна (кадра от
ушедшего уже не будет), но множество растёт при churn и вводит в
заблуждение при отладке gate. Следует `video_ready.remove(&from)` в той же
команде.

---

### P3-7. `MidDirection` используется в двух противоположных системах отсчёта

`parse_sections` (`webrtc.rs:101-147`) трактует `MidDirection` как
**SFU-local** (answer: `sendonly` = SFU шлёт), а `offer_publishes`
(`:836-862`) — как **browser-local** (offer: `sendonly` = браузер шлёт).
Сейчас это работает, потому что `can_send()` в обоих местах означает
«отправитель этой секции», но одна и та же enum-семантика в двух
перспективах — источник P2-1 и будущих ошибок. Лучше разделить
`MidDirection` (SFU) и `PublishDirection` (browser) или явно
документировать перспективу в сигнатуре.

---

## 4. Приёмка по пунктам задания

| # | Пункт | Статус на `14e7027` |
|---|---|---|
| 1 | In-process str0m / real ephemeral server + e2e: 1 pub + N viewers, N−1 audio, per-source video, answer order/directions, real `AcceptedPeer` | **частично**: in-process + `AcceptedPeer`-probe — pass (16 m-line, порядок, направления, 8/9 cap); **real e2e — нет** (P1-1, P1-2). «1 publisher + 8 viewers» = 9 пиров при default cap 8 недостижимо **by design**; проверена форма 8 пиров (1+7), 9-й `room_full`. Если Lead требует именно 9 пиров, нужен явный cap≥9 и это надо записать в приёмку |
| 2 | Mid lifecycle: no reuse, retire, per-publisher gate, remove one leaves others | **pass** по коду и unit/probe (`per_source_audio_mids...`, `keyframe_gate_is_per_publisher`, `removing_a_publisher_retires...`); нет функционального теста «после удаления A кадры B идут» — P3-тест-гэп |
| 3 | Cap default 8, fail-closed parse, admission 9th, global cap, metrics | **pass** для parser/admission в процессе (scratch 9-й `RoomFull`, cap 3 тест, global cap тест); fail-closed startup — код есть (`preflight`/`registry`), но нет e2e-теста с env; `new` — fail-open (P2-2) |
| 4 | No silent degradation legacy 1+1: drop/error+metric, never glue two Opus | **код pass**: drop + `dropped_no_mid` + warn, второй паблишер `None` (`legacy_1_plus_1...`); **документация — нет** (P2-3) |
| 5 | Dedup TrackAdded/TrackInfo, map consistency | **pass** для 1+7 → 2 трека (`real_peer_tracks_are_one_per_kind`); **view-only — ложный паблишер** (P2-1) |
| 6 | Drift detector + test, exit 0/1, CI/no-op, MANIFEST 215 | **отсутствует** в `14e7027` (P3-1); MANIFEST 215 не изменён; появится в `2b89bc6` |
| 7 | fmt; clippy targeted/workspace; tests counts; 3x flake | **pass**: fmt 0; оба clippy 0; 112/49/151; media-sfu 3×112 |
| 8 | Hygiene | **pass**: live DB/ports/backups не тронуты; единственный артефакт — этот отчёт; scratch удалён |

---

## 5. Счёт, вердикт и путь к 9.5+

**7.5/10, done: no.** Ядро T11b (per-source mids, direction-aware answer,
gate, cap, дедуп) реализовано качественно и подтверждено реальным str0m;
гейты и счётчики чистые. Но ревизия `14e7027` сама по себе не приёмочна:
P1-1 делает реальную multi-peer-комнату невозможной, P1-2 оставляет
единственный e2e-клиент на старой семантике cap=2. P2-1/P2-2 остаются
даже после follow-up-коммитов, поэтому «серия T11b» тоже не тянет на 9.5
без отдельной правки.

Чтобы принять T11b:
1. **P1-1:** включить в приёмку `6343f45` (или cherry-pick) и подтвердить
   реальным e2e, что два браузера одной UUID-комнаты имеют разные
   `ParticipantId`; без этого cap 8/per-source не проверить.
2. **P1-2:** обновить `tests/e2e/webrtc.js` до 1+7/1+7 (16 m-line), 3+
   страниц, N−1 inbound audio, per-source mids, cap+1 `room_full` при
   заданном `VKS_E2E_PEER_CAP` (это и делает `2b89bc6`).
3. **P2-1:** в `real_peer_tracks` учитывать `Media.direction` (`sendonly`/
   `sendrecv` только для publish-секций), иначе view-only пир — фальшивый
   паблишер.
4. **P2-2:** `WebRtcRoomRegistry::new` → `Result`/panic или убрать
   «refusing the plane» из лога; серверный путь уже fail-closed.
5. **P2-3:** обновить `WEBRTC_TRANSPORT`, `deploy/README`, план/трекер:
   cap 8, `VKS_WEBRTC_MAX_PEERS_PER_ROOM`, формула ёмкости, политика
   legacy-деградации, mid-ceiling.
6. **P3-1…P3-7** — по возможности в том же проходе; P3-2/P3-4/P3-5
   дешёвые и улучшают наблюдаемость/детерминизм.

---

## 6. Ограничения ревью

- E2e-браузер не запускался: на `14e7027` он заведомо красный (P1-2), а
  P1-1 блокирует multi-peer; запуск дал бы шумный FAIL не по T11b-коду.
  Вместо него — реальные str0m unit/DTLS-тесты и `AcceptedPeer`-probe.
- Drift-детектор не проверялся: его нет в ревизии (P3-1); `MANIFEST` 215
  подтверждён `wc -l`, файлы вендора не менялись.
- Scratch-probe тесты добавлялись только в `/dev/shm`-копию и после
  прогонов удалены; коммит `14e7027` не изменялся.
- Follow-up `6343f45`/`2b89bc6` упоминаются только как контекст; их код в
  этом ревью не проверялся.
