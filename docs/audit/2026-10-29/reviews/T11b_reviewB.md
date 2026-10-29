# T11b — адверсариальное независимое ревью B (commit `14e7027`)

**Объект:** `14e7027` «T11b: per-source egress mids + honest per-room cap» — 6 файлов, +1526/−244
(`vks-media-sfu/{webrtc.rs,webrtc_room.rs,lib.rs}`, `vks-server/{lib.rs,webrtc_runtime.rs}`, `vks-common/src/metrics.rs`).

**Копия:** `git archive 14e7027` → `/dev/shm/rvB_t11b_1541544_1793277855` (не `/tmp`), Rust **1.94.0**
(`cargo +1.94.0`, toolchain `1.94.0-x86_64-unknown-linux-gnu`), `CARGO_TARGET_DIR=/dev/shm/rvB_target_*`
(уникальные на каждый прогон), всё с `--offline --locked`. Живая `vks_data.db`, порты `:8080/:18082`,
боевой диапазон `43000-43100` и `backups/` не открывались.

**Вердикт:** **8.5/10**, `done: yes` — **P0 = 0, P1 = 0, P2 = 1, P3 = 7**.

Центральные инварианты T11b держатся и подтверждены реальной str0m-негоциацией (1+8 → answer из 16 m-line,
8 различных audio-mid + 8 различных video-mid, доставка 16 отдельных потоков, снятие паблишера не рвёт
остальные). Единственная P2 — не закрытый путь «таск пира умер → паблишер вернулся с тем же
`ParticipantId`»: у подписчика остаётся старый `(pid → mid)` и открытый keyframe-гейт. Оговорка по
приёмке: на самом `14e7027` `tests/e2e/webrtc.js` и `docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md`
ещё описывают cap = 2 / `room_full` для третьего; это исправлено следующим коммитом `2b89bc6` (проверено),
поэтому вердикт дан для серии T11b, а не для одиночного коммита.

---

## 1. TL;DR

| Что проверял | Результат |
|---|---|
| Форма answer SDP для 1..8 × 1..8 (64 сочетания) | ✅ ровно по одной m-line на оффер, тот же порядок и mids, без дублей; 9-я m-line любого вида → явный `InvalidOffer`, паник нет |
| Направления (sendonly/recvonly/sendrecv/inactive), port 0, отсутствие `a=mid`, дубли mids, чужой BUNDLE, перестановка секций | ⚠️ без паник; отсутствие `a=mid`/направления → явная `Sdp`-ошибка; `inactive` → не слот; **port 0 и дубль mid принимаются** (P3-1, P3-3) |
| Пул mid: 8+8 источников, audio vs video, retire/re-add, 9-й источник | ✅ 16 уникальных mid, 9-й → `None`, retired не переиспользуется, audio-mid ≠ video-mid |
| Keyframe-гейт на паблишера, PLI-коалесинг, churn | ✅ 30 дельт одного паблишера → ровно 1 PLI и 0 пересланных кадров; IDR B не открывает гейт A | 
| Уход/возврат паблишера, «мёртвый» таск | ❌ гейт и mapping не сбрасываются (`remove_generation`/`DropSource`) — **P2-1** |
| Cap: env-границы, 8-й/9-й пир, гонка приёмок, per-room vs global, метрики | ✅ 8 приняты, 9-й `RoomFull`; 8 конкурентных приёмок при cap=3 → ровно 3; global cap=2 при 6 приёмках → ровно 2; `capacity_exceeded` считается |
| Реальный e2e 1+8/8+8 in-process (str0m) | ✅ 16 кадров на 16 различных mid, payloads совпали; после retire 7+7 продолжают идти, старый mid молчит |
| Гейты fmt/clippy/tests/MANIFEST | ✅ всё зелёное (см. §5) |

**Чего не делал:** браузерный e2e против живого сервера — запрещён условиями (живой сервер/БД/порты).
Вместо него — реальный str0m DTLS/SRTP in-process (это транспортный, а не браузерный оракул) и
кросс-проверка браузерного артефакта следующего коммита (`2b89bc6:docs/progress/evidence/t11b/e2e-2026-10-29/alice-answer-16mids.sdp`:
16 m-line, 16 `a=mid`, 48 `a=ssrc`; т.е. answer пригоден для Chrome).

---

## 2. Что именно атаковал (матрица)

Все адверсариальные тесты — в скретч-копии (не в репозитории): `webrtc/reviewb.rs` (12 тестов) и
`webrtc_room/reviewb.rs` (12 тестов, из них 2 — намеренно «expected fail» пробы). Запуск:
`cargo +1.94.0 test -p vks-media-sfu --features str0m --offline --locked reviewb -- --nocapture`.
Итог: **20 passed, 2 failed (обе — xfail-пробы P2-1)**; см. §3 и Приложение A.

| # | Атака | Наблюдение |
|---|---|---|
| 1 | 64 оффера `1 sendrecv + (n-1) recvonly` для audio × video из 1..8 | answer: `sections == n_a+n_v`, mids совпадают с оффером и уникальны, egress-секции `sendonly`; `publish_mid` есть только при наличии sendrecv |
| 2 | 9 audio / 9 video / 9+9 | `InvalidOffer("too many media lines…")`, без паники |
| 3 | all-recvonly 8+8 (view-only) | 16 секций, 8 egress audio-mid и 8 video-mid (это корректно: recvonly = слот), `publish_mid == None` |
| 4 | all-sendonly 8+8 | 16 секций, ни одного egress-mid (`mid_for_source == None`) — «немой» пир ничего не получает |
| 5 | битый SDP: дубль `a=mid`, нет `a=mid`, port 0, чужой `a=group:BUNDLE`, `a=inactive`, нет направления, обратный порядок секций | паник/`unreachable` — **нет**; нет `a=mid` и нет направления → явная `Sdp(...)`; `inactive` → `Inactive` и вне пула; обратный порядок (video первым) → корректный answer в порядке оффера; **port 0 → принимается и «раз-декларируется»**; **дубль mid (audio+video) → принимается** |
| 6 | Пул: 8 audio + 8 video источников | 16 уникальных mid, 9-й audio/video → `None`, `source_for_mid` обратен, `mid_for_source(p,audio) != mid_for_source(p,video)` |
| 7 | legacy 1+1: 2 паблишера | 1-й занимает sendrecv-mid, 2-й → `None` (не склейка), детерминированно |
| 8 | retire → снова `mid_for_source` того же paблишера | старый mid не переиспользуется, но выдаётся **новый** mid → «поздний» кадр после `DropSource` снова занимает слот (**P2-1**) |
| 9 | Гейт: 30 дельт паблишера A, затем IDR B, затем IDR A | 0 пересланных дельт A до IDR; ровно 1 PLI (коалесинг) на 30 дропов; IDR B не открывает A; IDR A открывает A |
| 10 | `remove(room, publisher)` затем дельта ушедшего и «возврат» того же pid | `DropSource` приходит, но гейт остаётся открытым: дельта ушедшего и дельта «вернувшегося» пересылаются без IDR (**P2-1**) |
| 11 | Таск пира умер (`remove_generation`) | `DropSource` подписчикам **не отправляется**, `last_keyframe_request` не чистится (**P2-1**, **P3-4**) |
| 12 | Cap: env пусто/`0`/`-1`/`1.5`/`8x`/`0x10`/`1_0`/`>usize` | все → `Err`; `" 4 "` → 4; `"+1"` → 1 (Rust parse); `usize::MAX` и `1000000` принимаются (только warn в preflight) (**P3-7**) |
| 13 | 8 приёмок в комнату, 9-я | 8 → `Ok`, 9-я → `RoomFull`, `peer_count==8`, `peer_total==8`; после `remove` повторная приёмка работает |
| 14 | 8 конкурентных приёмок при per-room cap = 3 | ровно 3 `Ok`, 5 `RoomFull`, `peer_count==peer_total==3` |
| 15 | 6 конкурентных приёмок в разные комнаты при global cap = 2 | ровно 2 `Ok`, 4 `CapacityExceeded`, `peer_total==2`, метрика `capacity_exceeded` +4 |
| 16 | per-room vs global в одной комнате | per-room → `RoomFull`, global → `CapacityExceeded` (различимы) |
| 17 | Реальный e2e 1+8/8+8 | 8 различных audio-mid + 8 различных video-mid, 16 кадров, payload каждого совпал; retire → 7+7 идут, старый mid молчит, replacement `None` (пул исчерпан) |

---

## 3. Находки

### P0 — нет

### P1 — нет

### P2-1. Путь «таск пира завершился» не ретирит источник и не закрывает keyframe-гейт; повторный вход с тем же `ParticipantId` переиспользует старый mid

**Где:**
- `crates/vks-media-sfu/src/webrtc_room.rs:604-627` — `remove_generation`: удаляет handle и уменьшает `peer_total`, но **не** рассылает `DropSource` выжившим и **не** чистит `last_keyframe_request`.
- `crates/vks-media-sfu/src/webrtc_room.rs:916-918` — обработчик `PeerCommand::DropSource` вызывает только `peer.retire_source(from)`; `PeerHandle.video_ready` (`:130-154`) не трогается, т.е. гейт остаётся открытым.
- `crates/vks-media-sfu/src/webrtc.rs:195-243` — `mid_for_source` при отсутствии записи в map выдаёт **новый** mid из пула; `retire_source` (`:227`) лишь удаляет запись и помечает mid retired.
- `crates/vks-media-sfu/src/webrtc_room.rs:922-935` — `PeerCommand::Write` лениво вызывает `mid_for_source`, поэтому «поздний» `Write`, попавший в очередь после `DropSource`, получает свежий mid.

**Наблюдение (реальные прогоны):**
```
retire -> re-allocate: first=ML3 again=Some(Mid(IUK))
xfail_retired_publisher_frame_must_not_allocate_a_fresh_mid ... FAILED
  assertion `left == right` failed: DESIRED: retired source must not get a fresh mid
  left: Some(Mid(Tk2))  right: None
xfail_gate_must_close_for_a_departed_publisher ... FAILED
  DESIRED: after DropSource the subscriber's per-publisher video gate must be closed again
```

**Почему это плохо.** (а) «Поздний» кадр ушедшего паблишера занимает новый egress-слот (пул конечен, 8/8)
и пишется в mid, по которому подписчик не видел IDR; (б) если паблишер reconnect-ится с **тем же**
`ParticipantId` (транспорт умер по ICE/idle/DTLS-close, а сигнализация жива → `remove_generation` вместо
`remove`), у подписчика сохраняется старая пара `(pid → mid)` **и** открытый гейт: новый RTP-домен
стартует на уже отслеживаемом mid без ожидания IDR — ровно тот класс, от которого T11b защищается.
Кросс-пирового повреждения нет (страдает только подписчик этого паблишера), поэтому P2, а не P1.
Прямой путь `remove()` (сигнальный уход) корректен — его и проверяет тест коммита
`removing_a_publisher_retires_its_source_in_every_subscriber`; дыра именно в task-exit пути.

**Рекомендация:** в `remove_generation` и в обработчике `DropSource` вызывать
`target.forget_video(from)`/`rearm` (как в `:814`) и чистить `last_keyframe_request`; в `PeerMids`
хранить `(pid, generation)` либо помечать pid как retired, чтобы «поздний» `Write` не выдавал mid
(`mid_for_source` для retired pid → `None`).

### P3-1. `validate_offer` принимает дубли `a=mid:` (кросс-вид): один mid попадает в оба пула, один вид немой, reverse-lookup неоднозначен

**Где:** `crates/vks-media-sfu/src/webrtc.rs:789-827` (нет проверки уникальности mids),
`:105-150` (`parse_sections` сохраняет дубль), `:290-320` (`egress_pool` кладёт один и тот же mid в
audio- и video-пул).
**Наблюдение:** оффер `m=audio … a=mid:X` + `m=video … a=mid:X` принимается; answer:
`[("mKG", true, SendRecv), ("mKG", false, SendRecv)]`; `audio egress mKG / video egress mKG`;
`writer codecs on the shared mid: opus=Some(Pt(111)) vp8=None` → видео-кадры этого источника уходят в
`UnknownMid`/`no_egress_mid`, `source_for_mid` возвращает только одного из двух.
Дубли одного вида str0m схлопывает (3 секции в оффере → 2 в answer), поэтому «склейка двух
одновидовых часов» через этот путь недостижима. Для 1+1 такой оффер принимался и до T11b, т.е. это не
регрессия, а незакрытая дыра fail-closed-гейта. Вред — самому себе (кривой клиент), паник нет.
**Рекомендация:** в `validate_offer` отклонять повторяющиеся `a=mid` (и `a=mid`, не совпадающие с BUNDLE).

### P3-2. `offer_publishes` игнорирует port 0: отклонённая секция считается публикующей

**Где:** `crates/vks-media-sfu/src/webrtc.rs:836-864`.
**Наблюдение:** `m=video 0 … a=sendonly` → `offer_publishes(..., false) == true` (`a=inactive`
обрабатывается корректно). Следствие: другие пиры заранее аллоцируют egress-mid под поток, которого не
будет, и получают PLI-цель без видео.

### P3-3. Оффер с port 0 «раз-декларируется» в answer и попадает в пул egress (RFC 3264 §6.1)

**Где:** `crates/vks-media-sfu/src/webrtc.rs:789-827` (гейт не отсекает port 0), `:105-150`
(`parse_sections` видит только answer).
**Наблюдение:** `port_zero: ACCEPTED with 2 sections: [("6Bv", true, SendRecv), ("HAD", false, SendRecv)]`;
`port-0 video section: direction=SendRecv can_send=true`; `mid_for_source` выдаёт этот mid. Т.е. отказ
клиента от секции (port 0) не сохраняется в answer — браузер может отвергнуть такой answer, плюс слот
тратится. Ветка «порт 0 никогда не в пуле» в `parse_sections` сама по себе корректна, но str0m не
порождает port 0 в answer для такого оффера. Это поведение str0m + отсутствие проверки в
`validate_offer`; на реальных браузерах не воспроизводится (они используют `inactive`).

### P3-4. `last_keyframe_request` не чистится при task-exit

**Где:** `crates/vks-media-sfu/src/webrtc_room.rs:567-602` (`remove` чистит) vs `:604-627`
(`remove_generation` — нет).
**Наблюдение:** записи `(room, publisher, audio)` живут до конца процесса, если паблишер ушёл не через
сигнальный `remove`; ключей максимум 2 на инцидент, но карта неограниченна. Плюс после reconnect того же
pid первый PLI может быть подавлен остатком 500-мс окна.

### P3-5. Два контракта конструктора: `new()` fail-open, `registry()` fail-closed

**Где:** `crates/vks-media-sfu/src/webrtc_room.rs:247-254` (`new()` при кривом env логирует и берёт
дефолты) vs `crates/vks-server/src/webrtc_runtime.rs:188-195` (`registry()` → `None`, «offers fail
closed») и `:89` (`preflight` → ошибка/exit 78).
Для серверного пути контракт D3 соблюдён; библиотечный/тестовый `new()` тихо откатывается на 8/256 —
это задокументировано в doc-комментарии, но расхождение стоит закрыть (сделать `try_new` или убрать
env из `new`).

### P3-6. На `14e7027` документация, e2e-оракул и трекер приёмки ещё описывают cap = 2

**Где (в дереве коммита):**
`tests/e2e/webrtc.js:400-420` («third peer: rejected with room_full» — теперь третий **принимается**);
`docs/implementation/WEBRTC_TRANSPORT_2026-10-22.md:48-50` («не более 2… третий получает `room_full`»),
`:112`, `:123` («жёстко ограничено 2 пирами»);
`deploy/README.md:83-88` (нет `VKS_WEBRTC_MAX_PEERS_PER_ROOM`, «101 peers» как единственная ёмкость);
`docs/progress/MASTER_TASK_LIST_2026-10-29.md:26,51` (критерий всё ещё требует
`scripts/patch-pnm-webrtc-mids.py`); `docs/audit/2026-10-29/pitfalls/T11a.md:505` (тот же патч).
**Статус:** `2b89bc6` переписывает e2e (`PEER_CAP`, cap+1 → `room_full`, 1+8 pre-offer), docs и добавляет
`scripts/check-pnm-webrtc-drift.py`; проверил его отдельно: `--check` → exit 0 (70 файлов), 6 unittest OK.
Остаётся неисправленной только ссылка `T11a.md:505`. Вывод: код коммита корректен, но **одиночный
`14e7027` не является самодостаточным T11b-деливераблом** — тесты/доки догоняют в следующем коммите.

### P3-7. Preflight предупреждает только о `per_room > MAX_VIDEO_MIDS`; связка «слоты × порты» не проверяется

**Где:** `crates/vks-server/src/webrtc_runtime.rs:89-97`.
Огромные (`1_000_000`, `usize::MAX`) значения принимаются молча (только warn для per-room > 8); нет
предупреждения, что `max_peers × комнаты` может превысить UDP-диапазон и что cap > 8 всё равно упирается
в 8 mid на подписчика (pitfall P1-5). Деградация честная (метрика `dropped_no_mid`), но оператор узнает
о ней только по метрике.

---

## 4. Что подтвердилось по каждому пункту задания

1. **Форма SDP.** 64 сочетания 1..8×1..8: answer = ровно по m-line на оффер, порядок и mids сохранены,
   дублей нет; 9-я → `InvalidOffer`. Port 0/дубль mid — см. P3-1/P3-3; паник, `unreachable!` и
   «тихих» ответов нет (в коммите, кстати, убран старый `unreachable!`-комментарий/ветка).
2. **Пул mid.** 8+8 → 16 уникальных; 9-й → `None`; retired не переиспользуется; audio≠video для одного
   паблишера; повторный add того же pid **после `retire_source`** получает новый mid (не коллизия);
   «поздний» кадр после `DropSource` — новый mid (P2-1); старый mid после retire не пишется никем,
   т.к. единственный путь записи идёт через map.
3. **Keyframe-гейт.** Per-publisher `HashSet` работает: 30 дельт A → 0 пересланных кадров и ровно 1 PLI;
   IDR B не размыкает A; IDR A размыкает только A; churn/`SetSubscription` переarm'ит гейт
   синхронно (`webrtc_room.rs:640-655`). Дыра — уход/возврат паблишера (P2-1).
4. **Cap.** Все границы env → `Err` (fail-closed) кроме `"+1"` (валидный `usize`), whitespace
   тримится, `usize::MAX` принимается; 8-й принят, 9-й `RoomFull`; конкурентные приёмки не
   переполняют ни per-room, ни global (`peer_count==peer_total==N`); per-room и global дают разные
   коды; метрики `capacity_exceeded`, `WEBRTC_SOURCES{allocated|retired|dropped_no_mid}`,
   `frames_dropped{awaiting_keyframe|no_egress_mid}` материализуются.
5. **Реальный 1+8 e2e (in-process, str0m DTLS/SRTP).** 16 m-line, 8 audio-mid + 8 video-mid, 16 кадров
   с разными payload на разных mid; после retire одного паблишера остальные 7+7 продолжают идти, на
   старом mid кадров нет. Браузерный прогон не выполнялся (запрет на живой сервер/БД/порты).
6. **Гейты.** См. §5; MANIFEST/drift — там же.
7. **Гигиена.** См. §6.

---

## 5. Гейты (точные команды и числа, Rust 1.94.0, `--offline --locked`)

| Гейт | Результат |
|---|---|
| `cargo +1.94.0 fmt --all -- --check` | exit 0 |
| `clippy -p vks-media-sfu -p vks-server -p vks-common --all-features --all-targets --locked -- -D warnings` | **exit 0** |
| `clippy --workspace --all-features --all-targets --locked -- -D warnings` | **exit 0** |
| `test --workspace --locked --no-fail-fast` | **1573 passed, 0 failed, 8 ignored**, 52 test-result блока (включая doc-tests), exit 0 |
| `test --workspace --all-features --locked --no-fail-fast` | **1573 passed, 0 failed, 8 ignored**, exit 0 |
| `test -p vks-media-sfu --features str0m` ×3 | **112 passed / 0 failed** каждый прогон (0.78–0.90 с), включая все 16 новых T11b-тестов |
| `python3 scripts/check-pnm-runtime.py` | ok, 215 файлов, manifest сходится |
| `sha256sum -c ../../MANIFEST.sha256` | **215 OK** |
| `python3 scripts/check-pnm-theme.py` | ok |
| 3 × `patch-pnm-{layout-crash,audio-in-layouts,chartjs}.py --check` | exit 0 |
| `node --check .../chunks/index.bundle-2.2.1.66.js` | ok |
| drift-скрипт | на `14e7027` **отсутствует** (добавлен в `2b89bc6`); кросс-проверка версии из `2b89bc6`: `--check` exit 0 (70 файлов), `unittest` 6/6 OK |

Новых тестов в коммите: **16** (8 в `webrtc.rs`, 7 в `webrtc_room.rs`, 1 в `vks-server/src/lib.rs`),
удалено 2 (старые `third_real_peer_is_rejected_before_binding`, `keyframe_gate_blocks_deltas_until_the_first_idr`);
net +14 в `vks-media-sfu` (112 тестов при `--features str0m` в этом дереве).

---

## 6. Гигиена и cleanup

- Репозиторий не менялся, кроме этого отчёта: все эксперименты — в `git archive`-копии и отдельной
  скретч-копии для адверсариальных тестов; `cargo` работал только в них.
- Живая `vks_data.db`, `:8080/:18082`, диапазон `43000-43100`, `backups/`, чужие audit-файлы не
  открывались. Тесты биндят только `127.0.0.1:0` (эфемерные порты) и in-process UDP-сокеты, наружу не
  ходят (все прогоны `--offline`).
- После отчёта удалены: `/dev/shm/rvB_t11b_*` (pristine), `/dev/shm/rvB_adv_*` (скретч с тестами),
  `/dev/shm/rvB_target*` (два уникальных target-каталога), `/dev/shm/rvB_logs`, `/dev/shm/rvB_drift_*`,
  временные diff/лог-файлы `rvB_*` в `/dev/shm`. Единственный артефакт — этот файл.

---

## Приложение A. Воспроизведение адверсариальных тестов

В скретч-копии дерева (`git archive 14e7027`):
1. добавить в `crates/vks-media-sfu/src/webrtc.rs` и `webrtc_room.rs` в конец:
   `#[cfg(test)] mod reviewb;`
2. положить `crates/vks-media-sfu/src/webrtc/reviewb.rs` (12 тестов) и
   `crates/vks-media-sfu/src/webrtc_room/reviewb.rs` (12 тестов) — модули-дети, поэтому доступны
   приватные `parse_sections`, `PeerMids::new`, `enqueue_frame`, `remove_generation`, `peer_total`;
3. `CARGO_TARGET_DIR=<уникальный> cargo +1.94.0 test -p vks-media-sfu --features str0m --offline --locked reviewb -- --nocapture`.

Ключевые проверки (имена тестов): `reviewb_fuzz_offer_shapes_answer_matches_offer_and_mids_are_unique`,
`reviewb_fuzz_direction_patterns`, `reviewb_handcrafted_sdp_mutations_never_panic_and_keep_answer_shape`,
`reviewb_cross_kind_duplicate_mid_puts_one_mid_in_both_pools`,
`reviewb_port_zero_offer_becomes_a_live_egress_slot`, `reviewb_offer_publishes_ignores_port_zero`,
`reviewb_mid_pool_eight_sources_no_collision_audio_vs_video`,
`reviewb_retirement_does_not_release_the_mid_but_a_late_frame_reallocates_one`,
`reviewb_legacy_single_slot_is_deterministic_and_explicitly_drops`,
`reviewb_real_e2e_eight_audio_and_eight_video_mids_distinct_sources`,
`reviewb_room_limits_env_edges`, `reviewb_cap_eighth_ninth_and_readmission`,
`reviewb_concurrent_admissions_do_not_overshoot_the_per_room_cap`,
`reviewb_concurrent_admissions_respect_the_global_cap`,
`reviewb_per_room_and_global_caps_are_distinguishable`,
`reviewb_gate_is_per_publisher_and_pli_is_coalesced`,
`reviewb_removing_a_publisher_leaves_the_keyframe_gate_open_for_its_return`,
`reviewb_a_dead_peer_task_never_tells_survivors_to_retire_it`,
`reviewb_keyframe_coalescing_state_is_cleaned_on_remove`,
`reviewb_legacy_subscriber_policy_is_counted_not_silent`, плюс 2 xfail-пробы P2-1.

Ключевые строки наблюдаемого поведения (из `--nocapture`):
```
cross-kind duplicate answer: [("mKG", true, SendRecv), ("mKG", false, SendRecv)]
audio egress mKG / video egress mKG / declared mKG
writer codecs on the shared mid: opus=Some(Pt(111)) vp8=None
port-0 video section: direction=SendRecv can_send=true
retire -> re-allocate: first=ML3 again=Some(Mid(IUK))
received 16 frames, audio mids {8 шт.}, video mids {8 шт.}
retired video mid BYZ -> replacement None
```
