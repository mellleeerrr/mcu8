# T11b — независимое ревью C (раунд 2, финальный коммит)

- **Коммит:** `1f5185a` (серия T11b + T15/T16 после `14e7027`).
- **Метод:** чистый `git archive 1f5185a` в `/dev/shm/rvC_t11b_gk8VHyqE`; отдельная
  scratch-копия `/dev/shm/rvC_probe_t11b` для независимых проб; Rust/cargo
  `1.94.0`, `--offline --locked`, `CARGO_TARGET_DIR` на ext4
  (`/home/pro/rvC_t11b_targets/*`), чтобы не выжигать tmpfs-квоту.
- **Принцип:** всё, что заявлено как исправленное, проверялось **исполнением**
  (тесты репозитория + собственные пробы), а не чтением диффа.
- **Итог:** функционально T11b принят, P2-1 закрыт и подтверждён; но
  обязательный `cargo fmt --check` на самом `1f5185a` **красный** (одна пустая
  строка), поэтому формально сдача не «зелёная».

| | |
|---|---|
| **Оценка** | **9.0 / 10** |
| **Done** | **no** — единственный блокер `P3-1` (fmt); после исправления одной строки → **yes** |
| **P0** | 0 |
| **P1** | 0 |
| **P2** | 0 |
| **P3** | 6 |

---

## 1. Гейты: фактические результаты

| Гейт | Команда | Результат |
|---|---|---|
| fmt | `cargo +1.94.0 fmt --all -- --check` | **exit 1**, один `Diff in crates/vks-common/src/net_tuning.rs:283` (пустая строка на 286) |
| clippy (пакеты) | `cargo clippy -p vks-media-sfu -p vks-signaling -p vks-server --all-targets --offline --locked -- -D warnings` | **PASS**, 1m30s, `warning:` — 0 |
| clippy (workspace) | `cargo clippy --workspace --all-features --all-targets --locked --offline -- -D warnings` | **PASS**, 1m44s, `warning:` — 0 → регрессия T15 (`net_tuning`) зелёная |
| media-sfu | `cargo test -p vks-media-sfu --features str0m --offline --locked` | **120 passed / 0 failed**, 3 прогона (плюс 4-й ранее), 0.7–1.1 с |
| signaling | `cargo test -p vks-signaling --offline --locked` | **49 passed** |
| server lib | `cargo test -p vks-server --lib --locked --offline` | **154 passed / 0 failed / 3 ignored** (в bounded-прогоне; в двух обычных — 153/1/3, единственный падавший — T16 G.723 flake, см. ниже) |
| workspace | `cargo test --workspace --locked --offline --no-fail-fast` ×2 | оба прогона: **153 passed / 1 failed / 3 ignored** в `vks-server --lib`, падает только `gateways::media_runtime_tests::gateway_audio_bridges_g723_at_pt4_with_30ms_timestamps`; **все остальные таргеты зелёные** |
| workspace (bounded) | то же + `-- --test-threads=8` ×2 | **1602 passed / 0 failed / 8 ignored** в обоих прогонах |
| T16 flake изолированно | `vks_server-… --exact gateways::media_runtime_tests::gateway_audio_bridges_g723_at_pt4_with_30ms_timestamps` ×2 | **1 passed, 0.49 с / 0.46 с** — это перегрузка 36 потоков, не T11b |
| drift guard | `python3 scripts/check-pnm-webrtc-drift.py --check` | **exit 0**: «70 text files, no SDP API» |
| drift guard unit | `python3 scripts/test_check_pnm_webrtc_drift.py` | **6 tests OK** |
| drift guard negative | временный `--root` с `pc.createOffer()` в vendor | **exit 1**, сообщение «contains 'createOffer' (offer creation)» |
| MANIFEST | `sha256sum -c ../../MANIFEST.sha256` из `deployed/pnm` | **215/215 OK** (215 строк) |
| PNM runtime | `python3 scripts/check-pnm-runtime.py` | **ok, 215 files**, manifest matches, no `.gz` |

**Про «server lib 152/3 ignored».** На `1f5185a` в `vks-server --lib` **157**
тестов: 154 активных + 3 ignored. Число 152 в evidence-доке — снимок на момент
T11b; +2 теста добавили коммиты T15/T16 после него. Это не дефект T11b, но
документ устарел (см. P3-2). `120` для media-sfu и `49` для signaling совпали
ровно.

**Про workspace-флейк.** Оба обычных прогона (36 test-threads) падают **только**
на T16 G.723 playout-тесте (`crates/vks-server/src/gateways.rs:6065`,
«3 gaps» в первом прогоне и «2 gaps» во втором), который сам по себе и в bounded-режиме проходит. Ни один тест
T11b в этих прогонах не падал. Два bounded-прогона полностью зелёные, так что
регрессии нет.

---

## 2. P2-1: подтверждение исполнением

Собственные пробы (12 тестов, `reviewc`, 3 прогона по 12/12, плюс отдельные
прогоны; scratch-копия, в репозиторий не входят):

- `rc_retire_then_ensure_is_per_kind_and_late_write_gets_none` — после
  `DropSource`/`retire_source` `mid_for_source` = `None` для **обоих** видов,
  поздний `Write` слот не занимает; свежий mid выдаётся **только** после явного
  `EnsureSource`, старый остаётся в `retired`.
- `rc_remove_generation_retires_and_closes_gate_but_stale_generation_does_not` —
  task-exit `remove_generation` (gen 7) снимает peer, шлёт `DropSource` каждому
  выжившему, **синхронно** закрывает его per-publisher `video_ready` и чистит
  `last_keyframe_request`; поздний exit **старой** генерации (gen 7 при живой
  gen 9) не удаляет новый handle, не закрывает его gate, не чистит coalescing и
  не шлёт `DropSource`.
- `rc_e2e_1plus7_answer_mirrors_and_n_minus_one_streams` — реальный str0m
  DTLS/SRTP: offer `1+7`/`1+7` → answer **16 m-lines**, порядок и mids
  зеркалированы; 7 удалённых паблишеров дают **14 медиа-потоков** (7 audio + 7
  video) на **различных** mids (14 уникальных), VP8-writer присутствует.
- `rc_eight_real_peers_admitted_ninth_is_room_full` — 8 реальных `accept_offer`
  (каждый 16 m-lines) приняты, у всех разные `ParticipantId`; 9-й —
  `WebRtcError::RoomFull`, состояние комнаты остаётся 8.
- `rc_legacy_1plus1_second_publisher_is_explicitly_dropped` — легаси 1+1:
  второй паблишер получает `None`, без склейки двух Opus-часов в один mid.
- `rc_port_zero_first_section_is_answered_inactive_and_excluded` — port-0
  секция получает `m=… 0` + `a=inactive`, не становится publish-mid и не
  занимает egress-слот; остальные recvonly-слоты работают.
- `rc_cross_kind_duplicate_mid_variants_are_rejected` — duplicate `a=mid`
  между audio/video в трёх вариантах → `WebRtcError::Sdp`.
- `rc_offer_publishes_edge_matrix` — port 0, `inactive`, `recvonly`,
  `sendrecv` и отсутствие `a=mid` обрабатываются консервативно.
- `rc_mid_ceiling_is_eight_per_kind` — 8 слотов на вид, 9-й `None`.
- `rc_registry_new_fails_closed_on_invalid_limits` — `WebRtcRoomRegistry::new`
  возвращает `Err` с именем переменной на мусорный cap (fail-closed, не
  fallback).
- `rc_feasibility_warnings_cover_all_operator_traps` — warnings покрывают mid
  ceiling и UDP-range, «implausibly large» cap, недостижимый `max_peers`.
- `rc_late_drop_source_can_retire_a_fresh_reannouncement` — детерминированно
  воспроизводит остаточную гонку P3-3 (ниже).

Плюс репозиторные тесты, прошедшие в составе 120: `drop_source_command_…`,
`a_dead_peer_task_retires_the_source_and_rearms_survivor_gates`,
`duplicate_mid_across_kinds_is_rejected_with_an_sdp_error`,
`port_zero_section_is_answered_inactive_…`,
`real_dtls_handshake_with_eight_audio_mids_…`,
`feasibility_warnings_cover_mid_ceiling_ports_and_huge_caps`.

**Итог по P2-1: закрыт.** Изменения `a60b631` (retire источника + закрытие
gate + очистка coalescing на task-exit), `edfa198` (per-kind retire, port-0,
cross-kind mid) и `7857756` работают так, как заявлено.

### Участники и TrackAdded (6343f45, T11b-дедуп)

- `rc_participant_ids_are_distinct_for_peers_of_one_room` — 8 id вида
  `part-<room>-<uuid>` дают 8 разных `ParticipantId`, каждый равен **хвостовому**
  uuid; room-uuid не затеняет участника; legacy-формы (`sip-…-in`,
  `urn:uuid:…`, bare) не регрессировали.
- `rc_track_added_dedup_for_1_plus_7` — 16 m-lines (`1+7`/`1+7`) → ровно **2**
  `TrackAdded` (audio+video), повторный оффер тех же видов не объявляет ничего,
  video-only даёт 1 трек.

---

## 3. Находки

### P3-1. `cargo fmt --check` красный на `1f5185a` — блокер мержа
`crates/vks-common/src/net_tuning.rs:286` (rustfmt указывает hunk `:283`):
после замены `assert!` на `const _: () = assert!(…)` осталась пустая строка
перед `}`. `cargo fmt --all -- --check` → exit 1, ровно один diff. Это
регрессия самого `1f5185a` (T15-фикс), не T11b. Правка — удалить строку 286.

### P3-2. Evidence-док устарел к финальному коммиту
`docs/progress/evidence/t11b/SFU_PER_SOURCE_2026-10-29.md`:
- строка 86: `cargo fmt --all -- --check | ok` — **неверно** на `1f5185a`;
- строка 88: media-sfu **112 passed** — на `1f5185a` **120**;
- строка 91: server lib **152** — на `1f5185a` **154** активных (+3 ignored);
- строка 96: workspace clippy «падает на redundant closure в shell.rs» — на
  `1f5185a` workspace-clippy **зелёный** (WIP уже влит/исправлен).

Числа были верны для своего коммита; к `1f5185a` документ надо пересчитать или
пометить «на момент 2b89bc6».

### P3-3. `DropSource` не привязан к генерации: поздний drop может «похоронить» свежий mid
`crates/vks-media-sfu/src/webrtc_room.rs:685-688` (`remove`) и `:729-732`
(`remove_generation`) снимают peer под lock, **отпускают** `rooms`, затем шлют
`DropSource`. `announce_source` (`:630-648`) для нового поколения того же
`ParticipantId` может успеть отправить `EnsureSource` **раньше**, чем старый
task-exit отправит `DropSource` (разные отправители, порядок mpsc не
гарантирован). Тогда handler `DropSource` (`:1032-1044` → `retire_source`,
`webrtc.rs:258-268`) ретирит **только что выданный** mid, и вернувшийся
паблишер навсегда (до следующего явного `EnsureSource` и свободного слота)
исчезает у этого подписчика.

Проба `rc_late_drop_source_can_retire_a_fresh_reannouncement` детерминированно
держит `last_keyframe_request`, чтобы `remove_generation` завис между снятием
handle и отправкой `DropSource`; затем регистрирует gen 9 и вызывает
`announce_source` → в канале выжившего порядок `EnsureSource` → `DropSource`;
свежий mid ретирится, `mid_for_source` = `None`, восстановление — только новым
`EnsureSource` и при наличии свободного слота.

Окно — микросекунды (между `rooms`-lock и `try_send` нет await), поэтому это
**P3**, а не P2; но это остаток ровно того класса, который закрывал P2-1.
Рекомендация: добавить `generation` в `PeerCommand::DropSource` и игнорировать
drop от не-current генерации (либо рассылать под `rooms`-lock до регистрации
нового поколения).

### P3-4. `remove_generation` не чистит subscriptions/bitrate_caps
`remove` чистит `(room, participant)` в `subscriptions` и `bitrate_caps`
(`webrtc_room.rs:653-659`), `remove_generation` — только
`last_keyframe_request` (`:725-728`). При смерти task без последующего
signalling-`remove` записи о мёртвом участнике остаются до `cleanup_active_-
membership`; утечка ограничена числом пиров, но метрики/поведение
несимметричны. P3.

### P3-5. Метрика `allocated` считает повторный `EnsureSource`
`webrtc.rs:240-243`: `ensure_source` возвращает `Some(existing)` и для уже
смапленного паблишера; `webrtc_room.rs:1011-1013` в этом случае всё равно
инкрементит `vks_webrtc_sources_total{result="allocated"}`. При повторном
включении вида (off→on через reoffer) счётчик завышается. Функционального
эффекта нет, только observability. P3.

### P3-6. Browser-e2e артефакт — со старого коммита
`docs/progress/evidence/t11b/e2e-2026-10-29/result.json:3` — `commit
14e7027…`, 46/46; это **до** `a60b631` (P2-1 fix) и `edfa198`. На `1f5185a`
browser-e2e не перегенерирован; как доказательство финального P2-1 он не
годится. Задача помечает browser-e2e опциональным, поэтому это не блокер:
in-process str0m-оракул (16 m-lines, 14 потоков, 8 пиров, room_full) прогнан на
финальном коде. Но evidence-док должен либо обновить `commit`, либо явно
сказать «e2e на 14e7027; финальные фиксы проверены in-process».

---

## 4. Что зелено (сводка)

- P2-1: task-exit ретирит источник, закрывает gate, чистит coalescing;
  stale-генерация — no-op; поздний `Write` слот не берёт; свежий mid — только
  через `EnsureSource`.
- 8 пиров в комнате, 9-й `room_full`; 16 m-lines в answer, порядок/mids
  зеркалированы; N−1 поток на подписчика на разных mids; per-source video mids.
- Легаси 1+1 — явный `None`/`dropped_no_mid`, без склейки.
- Участники: разные id для разных пиров одной комнаты (хвостовой uuid).
- `TrackAdded` дедуплицирован по видам.
- Port-0 → inactive + `m=…0`; cross-kind duplicate mid → `Sdp`;
  `WebRtcRoomRegistry::new` fail-closed; feasibility warnings.
- fmt — **красный** (P3-1), всё остальное из списка гейтов — зелёное.

## 5. Гигиена

Live-БД, `:8080`, `:18082`, бэкапы не трогались; серверы не запускались,
порты не занимались (тесты — эфемерные/41001). Все сборки — в
`/home/pro/rvC_t11b_targets/*`, архивы/логи — в `/dev/shm/rvC_*`; после
написания отчёта scratch удалён. В репозиторий добавлен только этот файл.

## 6. Оценка

**9.0/10, done: no.** Функционально все заявленные фиксы T11b доказаны
исполнением, P2-1 действительно закрыт, multi-peer и 16-mid evidence есть.
Снять «no» можно одной строкой: удалить пустую строку в
`crates/vks-common/src/net_tuning.rs:286` и перепрогнать `cargo fmt --check`.
После этого — done: yes; для 9.5+ стоит также закрыть P3-3 (generation-scoped
`DropSource`), обновить числа в evidence-доке и перегенерировать browser-e2e на
финальном коммите.
