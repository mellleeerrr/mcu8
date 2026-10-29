# T04b — независимое ревью C (round 2): CodecSpec SSOT и матрица диспозиций

- **Ревизия:** `6aef749` (`T04b: clippy ...`), branch `master`; pristine-копия
  `git archive 6aef749` → `/dev/shm/t04b_rcC_1793279249_1730868` (не live-дерево).
- **Rust:** 1.94.0 (`cargo +1.94.0`, `rust-toolchain.toml`), все сборки —
  `--offline --locked`; `CARGO_TARGET_DIR=/home/pro/.cache/t04b_rcC_1793279249_target`.
- **Метод:** чтение диффа `a02daad..6aef749` и исходников на 6aef749 + прогон
  гейтов + **мутационное доказательство** (5 обязательных мутаций, doc-tamper и
  инъекция второй таблицы в существующий файл). Live-дерево не редактировалось,
  кроме этого отчёта; БД/порты/бэкапы не трогались.
- **Итог:** **принято (`done: yes`), 9.0/10.**
- **Дефекты:** **P0 = 0, P1 = 0, P2 = 1, P3 = 8.**
- Предыдущие ревью: A — `a02daad`, 8.5, `done: no` (P1=1, P2=8, P3=7); B —
  `a02daad`, 8.0, `done: no` (P1=2, P2=7, P3=5). Оба P1 закрыты; остаётся один
  P2 в механике сканера (не текущая ложь) и P3-хвост.

---

## 1. Гейты на pristine 6aef749

| Гейт | Команда (сокращённо) | Результат |
|---|---|---|
| fmt | `cargo +1.94.0 fmt --all -- --check` | exit 0 |
| clippy affected | 7 затронутых пакетов, `--all-targets --locked --offline -- -D warnings` | exit 0, **0 warnings** (1m17s) |
| clippy workspace | `--workspace --all-features --all-targets --locked --offline -- -D warnings` | exit 0, **0 warnings** (19.5s) |
| tests run 1 | `cargo +1.94.0 test --workspace --locked --offline --no-fail-fast` | **1580 passed / 0 failed / 8 ignored**, exit 0, 52 тест-бинаря |
| tests run 2 | то же | **1580 / 0 / 8**, exit 0 — флейков в этом ревью не воспроизведено |
| wasm32 | `cargo +1.94.0 check -p vks-client-sdk --target wasm32-unknown-unknown --locked --offline` | exit 0 |
| doc-tests | `cargo +1.94.0 test --workspace --doc --locked --offline` | exit 0 (0 doc-тестов) |

Заявленные **1580 / 0 / 8** подтверждены двумя полными прогонами; расхождений с
заявкой нет. Ключевые пакеты run 1: `vks-common` 98, `vks-gateway-sip` 163 (+1
ignored), `vks-gateway-h323` 46, `vks-signaling` 49, `vks-media-sfu` 112,
`vks-admin` 370, `vks-server` 155 (+3 ignored), интеграционные
`network_integration` 23, `operator_policy` 7, `directory_migration` 7,
`recorder_tap` 5, `codian_*` 18, `grids` 13 (+1 ignored), `ffprobe_acceptance` 2,
`h265_rtp` 1, `config_failclosed` 1, `metrics` 2, `dedup` 1.
Ignored: `live_h323`, `live_dial`, `mcu_tick_soak`, `grids_geometry_golden` (8).

Логи гейтов и мутаций временно лежали в `/dev/shm/t04b_rcC_logs/` и
`/home/pro/.cache/t04b_rcC_logs/`; при финальной уборке удалены (ключевые
выводы и точные имена тестов приведены в этом отчёте).

---

## 2. Мутационная матрица (обязательные пробы задания)

Все мутации — в pristine-копии, после каждой файл восстановлен из
`git show 6aef749:<path>` и проверен `git diff --work-tree ... 6aef749` = 0 строк.

| # | Мутация | Поймавший тест (точное имя) |
|---|---|---|
| a | `H263.sip = Native` (`codec_spec.rs:548`) | **compile-fail**: `vks-common` test-target не собирается — `E0277` в `codec_spec::tests::public_native_lists_are_canonical_and_deduplicated` (`codec_spec.rs:1280`, `[&str;3]` vs `[&str;2]`), `cargo test -p vks-common --lib` exit 101. Runtime: **`rtp_bridge::tests::offer_rows_match_spec`** (`rtp_bridge.rs:1094`: «H263 must have exactly one VIDEO_OFFER row», 0≠1) и **`rtp_bridge::tests::native_disposition_matches_offer_gate_and_public_matrix`** (`rtp_bridge.rs:1250`); sip 161 pass / 2 fail |
| b | в production `production_rtp_maps()` (`main.rs:52-58`) добавлены `RtpMap{h265,99,90k}` и `RtpMap{g722,9,8k}` | **`production_codec_caps_tests::production_rtp_maps_match_the_ssot_webrtc_answer_slice`** (`main.rs:638`: 4≠2, «one RtpMap row per SSOT WebRTC answer codec»); server bin 2 pass / 1 fail |
| c | `/license` `params`: `*key == "g729" \|\| available(name)` (`shell.rs:440`) | **`api_v3::shell::tests::license_params_are_derived_from_ssot_dispositions`** (`shell.rs:1262`: g729=true, ожидалось false) |
| d1 | новый файл `crates/vks-server/src/codec_table_dup.rs`: `const CODECS2: &[&str] = &["OPUS","VP8"];` | **`codec_spec::tests::no_codec_name_cluster_outside_allowlist`** (`codec_spec.rs:1731`, в сообщении файл и `{"OPUS","VP8"}`) |
| d2 | новый файл `crates/vks-server/src/new_consumer.rs`: `use vks_common::codec_spec as _ssot;` | **`codec_spec::tests::ssot_consumer_scan_list_is_complete`** (`codec_spec.rs:1644`: drift `SSOT_CONSUMER_FILES`, файл в left) |
| e | `CODEC_VARIANTS` 32k-вариант G7221: `rtp_clock 32_000 → 16_000` (`codec_spec.rs:623`) | **`codec_spec::tests::g7221_variants_model_both_clocks_truthfully`** (`codec_spec.rs:1236`); плюс **`rtp_bridge::tests::offer_rows_match_spec`** (`rtp_bridge.rs:1051`: «G7221 has no SSOT variant at clock 32000») |
| доп. | одна ячейка `G722` в `MEDIA_PIPELINE.md` → `—` | **`codec_spec::tests::docs_public_matrix_matches_ssot`** (`codec_spec.rs:1434`) |

Все пять обязательных мутаций **пойманы**. Особенность (a): на 6aef749 public
list запинен длиной массива, поэтому мутация ловится ещё и на компиляции
test-target; named runtime-падения SIP при этом есть, т.е. инвариант не
«держится только компилятором».

### 2.1. Инъекция в существующий файл — сканер слеп (P2-1)

В production-хвост `crates/vks-admin/src/api_v3/directory_migrate.rs` (после
`#[cfg(test)] use ... Storage;` на строке 21, перед тест-модулем) добавлена
вторая таблица `const CODECS2: &[&str] = &["OPUS","VP8"];`. Оба сканера
**прошли** (`31 codec_-тест ok`, в т.ч.
`no_codec_name_cluster_outside_allowlist ... ok`,
`no_second_codec_name_table_outside_allowlist ... ok`).

Причина: `production_source()` (`codec_spec.rs:1591-1600`) режет файл по
**первому** `#[cfg(test)]`, а комментарий утверждает, что «all Rust files in
this repo keep tests after production code» — это неверно. Файлы, у которых
тест-маркер стоит в начале (обычно `#[cfg(test)] use/fn`), теряют весь
production-хвост:

- `crates/vks-gateway-sip/src/rtp_bridge.rs:227` — `mod header_regressions` в
  середине файла; за ним `VIDEO_OFFER` (`:473`), `AUDIO_OFFER` (`:505`), т.е. строка 227
  отсекает 2 200 строк, включая обе офферные таблицы;
- `crates/vks-server/src/gateways.rs:133` — `#[cfg(test)] fn ensure_bridgeable...`;
  отсекаются 6 350 строк, включая `SIP_AUDIO_PTS` (`:1031`);
- `crates/vks-admin/src/api_v3/mod.rs:29` — три тест-модуля в начале, затем
  Router/`ApiV3State` (≈600 строк production);
- `crates/vks-admin/src/api_v3/directory_migrate.rs:21`;
- также `vks-auth/src/local_db.rs:87`, `vks-admin/src/state.rs:257`,
  `vks-common/src/ids.rs:37`, `vks-server/src/room_events.rs:67`,
  `vks-layout/src/{grid,focus}.rs` и др. — всего 16 файлов с маркером в первой
  половине (эвристика; для части из них тесты действительно в конце).

Текущей лжи это не создаёт: `AUDIO_OFFER`/`VIDEO_OFFER` проверяются
`offer_rows_match_spec`/`validate_offer_rows`, `SIP_AUDIO_PTS` —
`sip_pt_maps_match_ssot_disposition`. Но заявка коммита 4786414 «scan **every**
production file» неверна, и вторая литеральная таблица в этих двух ключевых
файлах пройдёт CI. Это ровно остаток класса review B P2-7 / A P2-3, поэтому —
**P2**, не P1 (нет текущего расхождения).

**Фикс (рекомендация):** резать не по первому `#[cfg(test)]`, а по
`#[cfg(test)]\nmod tests` / `#[cfg(test)]\n#[path = "..._tests.rs"]` (или
удалять только блоки тест-модулей); добавить проверку, что после среза не
осталось `pub fn`/`impl`/`const` production-кода. Тогда `rtp_bridge.rs` и
`gateways.rs` попадут в скан (сейчас их таблицы вне его).

---

## 3. Проверка находок A и B на 6aef749

### 3.1. Закрытые P1

- **A P1-1 / B P1-1 (production caps/RtpMap литералами).** Закрыто в `3e2983c`:
  `main.rs:388-403` строит `HandlerConfig::new(...).with_room_mode(...).with_dtls_srtp(...)`,
  `with_caps` удалён (grep по `crates/` пуст), codec-поля caps приходят из
  `webrtc_answer_specs()` (`handler.rs`). `production_rtp_maps()` остаётся
  литеральным транспортом (111/opus, 96/vp8), но проверяется
  `production_rtp_maps_match_the_ssot_webrtc_answer_slice` (`main.rs:629-645`) —
  мутация (b) падает. Плюс `production_main_does_not_overwrite_the_ssot_caps`
  (`main.rs:660`) сканирует production-текст на `.with_caps(`/`RoomCaps {`.
- **B P1-2 (Native video ⇒ оффер).** Закрыто в `5f25035`:
  `rtp_bridge::tests::native_disposition_matches_offer_gate_and_public_matrix`
  (`rtp_bridge.rs:1210-1255`) требует ровно одну строку `VIDEO_OFFER` для
  каждого Native video, отсутствие для RxOnly/Unsupported/WebRTC и сверяет
  gate/public matrix. Мутация (a) его валит.

### 3.2. P2

| Находка | Статус на 6aef749 |
|---|---|
| A P2-1 / B P2-4 G7221C clock-вариант | **закрыто**: `CODEC_VARIANTS` (`codec_spec.rs:621-624`) моделирует 16k/32k, `resolve_clock` учитывает clock; исключение из `offer_rows_match_spec` убрано; мутация (e) ловится |
| A P2-2 / B P2-5 exact-one + name→spec | **закрыто**: `validate_offer_rows` требует ровно одну строку на (id, clock, bitrate) и `lookup(name).id == spec`, `video` — ровно одна на Native; дубликат/чужой spec теперь падает |
| A P2-3 / B P2-7 сканер «second table» | **частично**: новые файлы ловятся (d1/d2), но существующие файлы с ранним `#[cfg(test)]` — нет (P2-1 выше) |
| A P2-4 PT-карты UAS/server | **закрыто**: `uas_static_pt_map_matches_ssot_disposition` (`uas.rs`), `sip_pt_maps_match_ssot_disposition` (`gateways.rs`), H.323 `h323_pts_match_spec_slice` |
| A P2-5 / B P2-1 VP9/AV1 reason | **закрыто**: `Reason::PendingPayloader` в `reason`, строки VP9/AV1 в `MEDIA_PIPELINE.md` обновлены, doc-тест зелёный |
| A P2-6 публичные native-списки (дубли) | **закрыто**: `PUBLIC_NATIVE_AUDIO/VIDEO` канонические, дедуп; `public_native_lists_are_canonical_and_deduplicated` (common), admin `public_codec_lists_are_canonical_and_pinned` (audio=11, video=["H264","H265"]) |
| A P2-7 / B P2-2 AAC reason | **закрыто**: три состояния (`main.rs:423-446` license/probe/pool), `audio_unavailable_reason` порядок license→probe→pool (`codec_capabilities.rs`), тест `aac_reason_order_is_license_then_probe_then_pool` |
| A P2-8 / B P3-1 timing-flake | **не воспроизведено**: 2 полных прогона 1580/0/8; изолированный `gateway_audio_bridges_g723_at_pt4_with_30ms_timestamps` — ok (0.49s). Остаётся известный риск, не дефект T04b |
| B P2-3 canonical `audio_codec`/clock | **закрыто**: `NegotiatedMedia.audio_codec = codec_spec::spec(codec.spec).name` (`rtp_bridge.rs:1976`), clock/sample rate в `gateways.rs` выводятся (`audio_clock_rate_for_codec`, `audio_sample_rate_for_codec`); тест на OPUS 48k |
| B P2-6 `/license` литералы | **закрыто** `884df63`: `license_codec_params` из `LICENSE_AUDIO_PARAMS` + `is_supported_audio_variant`; структурный guard; мутация (c) падает |

### 3.3. P3-хвост (см. §4)

Закрыт A P3-7 (caps audio `continue` → `panic!` на неизвестном answer codec:
`room_caps_for` паникует; тест покрывает). Остальные P3 — ниже.

---

## 4. Находки

### P2-1. Сканер «second table» не видит production-хвост файлов с ранним `#[cfg(test)]`

`crates/vks-common/src/codec_spec.rs:1591-1600` (комментарий утверждает
«все файлы держат тесты после production» — неверно).
Отсекаются: `crates/vks-gateway-sip/src/rtp_bridge.rs:227`
(`AUDIO_OFFER`/`VIDEO_OFFER`, `:473+`), `crates/vks-server/src/gateways.rs:133`
(`SIP_AUDIO_PTS`, `:1031`), `crates/vks-admin/src/api_v3/mod.rs:29`,
`crates/vks-admin/src/api_v3/directory_migrate.rs:21` и ещё ~19 файлов.
Доказательство: инъекция `const CODECS2 = ["OPUS","VP8"]` в
`directory_migrate.rs:512` — оба сканера зелёные (§2.1). Текущей лжи нет (таблицы
этих файлов проверяются своими disposition-тестами), но гарантия «нигде нет
второй таблицы» не выполняется.

### P3 (8)

1. `ALIAS_FAMILIES[0]` и `count + 1` захардкожены в генераторах native/H.323
   списков: `codec_spec.rs:794,814,918,938,978,995`; вторая alias-семья не
   попадёт в списки, тест этого не заметит.
2. H.323 inbound `audio_clock_rate: 8000` литералом (`gateways.rs:3651`).
   Сегодня корректно (все H.323-native аудио — 8 кГц), но не выводится из SSOT.
3. `publish_support` — второй литеральный список codec-меток
   (`transcoder_runtime.rs:76-96`), а тест `worker_metric_labels_are_subset_of_specs`
   (`:136-142`) перебирает свой литерал, а не реальные метки.
4. `h264_high_is_rx_only_and_never_offered` (`codec_spec.rs:1366-1372`) проверяет
   только H.323 audio-срез (`!H323_AUDIO_NAMES.contains("H264-HIGH")`), что для
   video тривиально; отсутствие в `VIDEO_OFFER` держит gateway-тест.
5. `specs_have_no_payload_type_field` (`codec_spec.rs:1405-1414`) — substring
   `"payload"`/`"packet_type"`; поля `pt`/`ptype` пройдут.
6. UAS `payload_is` (`uas.rs:407-422`) сравнивает wire-имя с каноническим:
   `a=rtpmap:4 G7231/8000` отвергается, хотя `lookup("G7231")` — Native
   (несогласованность статических и динамических PT; A P3-6).
7. `GET /api/v3/features` (`shell.rs:515-517`) — литеральный vendor-список,
   включающий `G.719`, `G.729`, `H.261`, `H.263`, `H.264-HIGH`, `AAC`, хотя
   `/license` и матрица говорят для них `false`/`Unsupported`; теста и связи с
   SSOT нет. Семантика «фич» спорна, но два публичных ответа расходятся.
8. Колонка `Reason` в `MEDIA_PIPELINE.md` SIP-центрична: у G726/G728/OPUS/H265
   H.323-ячейка `—` без причины, у G729 показан `encoder_not_bit_exact` при
   H.323 `—`; это не ложь, но не объясняет межплоскостные различия.

---

## 5. Truthfulness по кодекам (выборочно, 6aef749)

| Кодек | SSOT | SIP offer | H.323 | WebRTC | Публичные списки / docs |
|---|---|---|---|---|---|
| PCMU/PCMA/G722/G723/G726-16..40/G728/OPUS | Native | PT 0/8/9/4/107-110/15/111 | PCMU/PCMA/G722/G723 | Opus | native, supported |
| MP4A-LATM | Worker+license | PT113 только при license+probe+pool | — | — | `/license` false, reason по трём гейтам |
| G729 | Unsupported(encoder_not_bit_exact) | identity PT18, не отправляется | — | — | reason совпадает |
| G7221/G7221C | Unsupported(no_implementation) | identity PT102-106 (16k/32k), clock из варианта | — | — | reason совпадает |
| G719 | Unsupported(no_implementation) | identity PT112 | — | — | reason совпадает |
| ILBC | Unsupported(no_implementation) | нет строки | — | — | reason совпадает |
| H264 | Native | PT97 | PT97 (sidecar) | — | native |
| H264-HIGH | RxOnly(tx_profile_unsupported) | нет; RX принимает High | — | — | rx-only |
| H265 | Native (probe-гейт) | PT99 при probe | — | — | native |
| VP8 | SIP Unsupported(webrtc_only), WebRTC WebRtc | нет | — | answer | webrtc |
| VP9/AV1 | webrtc Unsupported(pending_payloader), reason `pending_payloader` | нет | — | нет | pending_payloader |
| H261/H263/H263+/H264-SVC | Unsupported | нет | — | — | reason |
| telephone-event | Dtmf/Native | PT101 | — | — | native |

Проверено: ровно одна строка/ветка на кодек/вариант (`offer_rows_match_spec`,
`validate_offer_rows`); Unsupported/RxOnly/WebRtcOnly не рекламируются как
claimable (identity-строки G729/G7221/G719/PT112 не отправляются
`audio_row_visible`); `audio_codec` канонический; proto-теги 0..8 не менялись
(`codec_id_wire_tags_are_stable`, `git diff` proto пуст); `answer_payload_types`
(`vks-server/src/lib.rs:656`) через `codec_spec::lookup(...).kind` (фикс
4786414); AAC-причины раздельные; `/license` производный.

---

## 6. Регрессии

- Золотые SDP-строки `a02daad` и `6aef749` **идентичны**:
  `m=audio 41000 RTP/AVP 0 8 9 15 4 107 108 109 110 111 101`,
  `m=video 41002 RTP/AVP 97 99`; порядок и номера PT не менялись,
  `sip_offer_has_no_duplicate_payload_types` зелёный.
- Реальная негативация/позитивация (Polycom-answer, G722 name+clock, AAC
  license on/off, H264/H265 probe, unknown video) — зелёные в обоих полных
  прогонах.
- SFU answer: Opus+VP8, builder из `webrtc_answer_specs()`, fail-closed на
  чужой id.
- H.323: PT-набор `[0,8,9,4]` (порядок — P3-1 A, не регресс), sidecar не
  расширен.
- Dead code/allowlist-трюков не добавлено: clippy `-D warnings` чистый,
  `NON_CONSUMER_EXCEPTIONS`/`NAME_SPELLING_ALLOWLIST` имеют непустые причины и
  проверяются; исключения (legacy `CodecMap`, `vks-core/media_policy.rs`)
  задокументированы, как и требовалось.

---

## 7. Приёмка

| Критерий | Статус |
|---|---|
| 0 P0 | да |
| 0 P1 | **да** (оба P1 A/B закрыты, проверено на 6aef749) |
| fmt/clippy/fmt/wasm/doc | да |
| workspace tests | 1580/0/8 дважды; флейков нет |
| 5 обязательных мутаций ловятся | да (a: compile+2 runtime; b,c,d1,d2,e — named) |
| Публичная матрица/docs не лгут | да (doc-tamper падает) |
| `/license` из SSOT | да (мутация c падает) |
| PT-карты UAS/server/H.323 ↔ disposition | да |
| Ровно одна строка/ветка на кодек | да |
| «Нигде нет второй таблицы» | **не гарантировано** (P2-1), текущей второй таблицы нет |
| SDP/PT/proto без регрессий | да |
| Гигиена | live-дерево не тронуто; см. §8 |

**Оценка: 9.0/10, `done: yes`.**

Почему 9.0: все требования приёмки по truthfulness и негативации выполнены и
подтверждены исполнением/мутациями; оба P1 закрыты; остаётся один P2 в механике
сканера (не текущая ложь) и 8 P3. Почему не 9.5: заявка «scan every production
file» ложна для `rtp_bridge.rs`/`gateways.rs` (инъекция проходит), т.е. headline
T1-гарантия не полная; плюс P3-хвост (§4). Если Lead считает полноту сканера
блокирующей для приёмки — это единственный аргумент за `done: no`/8.5; по
текущему состоянию продуктовой истины блокеров нет.

---

## 8. Гигиена

- Live-дерево `/home/pro/Рабочий стол/mcu` не редактировалось, кроме этого
  отчёта; БД, порты, бэкапы, systemd-юниты не трогались; сервер не
  запускался/не перезапускался.
- Все эксперименты — в `/dev/shm/t04b_rcC_1793279249_1730868` (pristine) и
  `/dev/shm/t04b_rcC_mut` (hardlink-копия, удалена); после мутаций файлы
  восстановлены и сверены `git diff` (0 строк). В `/tmp` ничего не создавалось.
- Во время ревью хост исчерпал usrquota `/tmp` (EDQUOT), из-за чего
  shell/terminal/glob-инструменты временно падали (`UNKNOWN: unknown error,
  write`). После восстановления окружения target-каталог (~7G), pristine-копия
  `/dev/shm/t04b_rcC_1793279249_1730868` и все скретч-логи удалены; в отчёте
  остались точные имена тестов, сообщения и file:line. На результаты ревью
  сбой не влияет.
