# T04b — независимое ревью D (round 3, финальное): CodecSpec SSOT и матрица диспозиций

- **Ревизия:** `6ac14c2` (`fix(T04b): drop SSOT consumers retired by T11c`), branch `master`.
  Pristine-копия: `git archive 6ac14c2` → `/dev/shm/rvD_1793282838` (live-дерево не
  использовалось для сборок).
- **Rust:** 1.94.0 (`cargo +1.94.0`, `rust-toolchain.toml`), все сборки
  `--offline --locked`; `CARGO_TARGET_DIR=/home/pro/.cache/rvD_1793282838_target`.
- **Метод:** run гейтов на pristine + **мутационное доказательство** закрытия P2
  (4 пробы + lexer-проба), повтор мутаций (a)–(e) review C (с поправкой на код,
  удалённый T11c), два полных прогона workspace-тестов, изолированный прогон
  упавшего теста. Live-дерево не редактировалось, кроме этого отчёта; БД/порты/
  бэкапы не трогались.
- **Итог:** **`done: no`, 8.5/10.**
- **Дефекты:** **P0 = 0, P1 = 1, P2 = 1, P3 = 8.**
  P2-1 review C (слепота сканера к production-хвосту) **закрыт исполнением**;
  единственный P1 — красный workspace-тест на HEAD, привнесённый T11c, не
  коммитами T04b; P2 — заявленные counts `1610/0/8` не воспроизводятся на
  `6ac14c2` (фактически `1534/1/8`; после внешнего фикса t10 — `1535/0/8`).

> Работа остановлена по команде Lead; состояние pristine-копии, target и логов
> сохранено для продолжения (см. §9). Все мутации откачены и сверены по sha256.

---

## 1. Гейты на pristine 6ac14c2

| Гейт | Команда (сокращённо) | Результат |
|---|---|---|
| fmt | `cargo +1.94.0 fmt --all -- --check` | **exit 0** |
| clippy affected | 7 пакетов (`vks-common`, `vks-gateway-sip`, `vks-server`, `vks-admin`, `vks-signaling`, `vks-media-sfu`, `vks-gateway-h323`), `--all-targets --locked --offline -- -D warnings` | **exit 0**, 0 warnings |
| clippy workspace | `--workspace --all-features --all-targets --locked --offline -- -D warnings` | **exit 0**, **0 warnings** (1m17s) |
| tests run 1 | `cargo +1.94.0 test --workspace --locked --offline --no-fail-fast` | **1534 passed / 1 failed / 8 ignored**, exit 101, 54 таргета |
| tests run 2 | то же | **1534 / 1 / 8**, exit 101; per-target counts **побайтово совпали** с run 1 |
| изолированный t10 | `cargo test -p vks-server --test t10_directory_failclosed` | **1 failed** (детерминированно, не флейк) |
| wasm32 | `cargo check -p vks-client-sdk --target wasm32-unknown-unknown --locked --offline` | **exit 0** (1m02s) |
| doc-tests | в составе workspace-прогона | 16 doc-таргетов, 0 тестов, все ok |
| `vks-common --lib` (финал) | после отката всех мутаций | **108 passed / 0 failed** |
| `vks-server --lib` | отдельно | **154 passed / 0 failed / 3 ignored** |

Ключевые пакеты run 1: `vks-common` 108, `vks-gateway-sip` 164 (+1 ignored),
`vks-gateway-h323` 48, `vks-signaling` 41, `vks-media-sfu` 47, `vks-admin` 375,
`vks-server` lib 154 (+3 ignored), `network_integration` 22, `directory_migration` 8,
`media-mcu` 128, `cfg`-интеграционные наборы — все зелёные, кроме
`t10_directory_failclosed` (P1-1).

**Расхождение с ожиданием `1610/0/8`:** на `6ac14c2` фактический итог —
`1534/1/8` (54 таргета, 1543 теста с ignored). После фикса t10 (вне `6ac14c2`,
см. §7) sibling-прогон в live-дереве даёт **1535/0/8** — то есть база `6ac14c2`
это ~1535, а не 1610. Причина: T11c `2d5c73e` удалил fake-SFU плоскость
(`vks-media-sfu`: 112 → 47 unit-тестов, −65), `044249f` — fake SDP builder
(`vks-signaling`: 49 → 41), при этом другие наборы выросли (`media-mcu`
112 → 128, `common` 98 → 108 и т.д.). Заявка `1610` снята с другого
дерева/ревизии и на HEAD не подтверждается (P2-1).

---

## 2. Закрытие P2-1 review C — доказано исполнением

P2 review C: `production_source()` резал файл по **первому** `#[cfg(test)]`,
поэтому production-хвост `directory_migrate.rs`, `gateways.rs`, `rtp_bridge.rs`,
`api_v3/mod.rs` был вне скана; инъекция второй таблицы проходила.

Фикс `c16d838`: `production_source` → `strip_test_modules` (lexer: удаляются
только тест-**модули** `#[cfg(test)] mod …`, `#[path] mod …;`, bare `mod tests`
с `#[test]`; `#[cfg(test)] use/fn/static` остаются в сканируемом тексте).
Код: `crates/vks-common/src/codec_spec.rs:1701-1726,1730-1739,1745-1789`,
lexer `1892-2041`, `is_test_source_file` `2046-2049`.

### 2.1. Мутации (все — pristine, после каждой файл откачен и сверен sha256)

| # | Мутация | Результат |
|---|---|---|
| M1 | `const CODECS2: &[&str] = &["OPUS","VP8"];` в production-хвост `crates/vks-admin/src/api_v3/directory_migrate.rs` (перед `/// Test helper`, строка 541; старый срез был на `#[cfg(test)] use` строке 21) | **FAILED**: `codec_spec::tests::no_codec_name_cluster_outside_allowlist` (`codec_spec.rs:2172`), в сообщении `"vks-admin/src/api_v3/directory_migrate.rs: {\"OPUS\", \"VP8\"}"` |
| M2 | `match pt { 107 => "G726-16", _ => "" }` в production-хвост `crates/vks-server/src/gateways.rs` (строка 1071, после `SIP_AUDIO_PTS`; старый срез был на `#[cfg(test)] fn` строке 133) | **FAILED**: `codec_spec::tests::no_second_codec_name_table_outside_allowlist` (`codec_spec.rs:2121`): `vks-server/src/gateways.rs still contains a literal codec table ("107 => \"G726-16\"")` |
| M3 | fresh `crates/vks-server/src/reviewd_plain_scratch.rs`: bare `mod tests { #[test] fn hidden(){} }`, затем `const CODECS2=["OPUS","VP8"]` | **FAILED**: cluster-тест назвал файл `reviewd_plain_scratch.rs: {"OPUS","VP8"}`; tail после вырезанного тест-модуля сканируется |
| M4 | fresh `reviewd_helper_scratch.rs`: `#[cfg(test)] fn helper(){}`, затем та же таблица | **FAILED**: cluster-тест назвал файл `reviewd_helper_scratch.rs`; helper не режет хвост |
| M5 | текущее дерево без мутаций | `vks-common --lib` **108/0**; оба сканера и repo-wide tail-тест зелёные — **ложных срабатываний нет** |

M3/M4 удалены после прогонов; M1/M2-файлы восстановлены
(`sha256 == git show 6ac14c2:<path>`). Промежуточная особенность M3: старый
сканер тоже поймал бы файл без `#[cfg(test)]`, поэтому M3 доказывает, что новый
lexer **не пере-вырезает** bare `mod tests` и сохраняет production-tail.

### 2.2. Lexer: raw strings / comments / chars

Коммит-тесты покрывают: string, `//`-комментарий, raw string
(`production_source_strips_bare_test_modules_and_ignores_literals`,
`codec_spec.rs:2231-2244`), плюс repo-wide tail (`:2247-2279`) и fixture с
ранними `#[path] mod` + поздней таблицей (`:2196-2228`).

Char-литералы, блочные комментарии и byte/raw-byte строки **отдельного
коммит-теста не имеют**. Поведение проверено временно добавленным тестом
(после прогона удалён): `#[cfg(test)] mod tests { const BRACE: char='}'; const
OPEN: char='{'; }`, `/* } */` внутри модуля, `b"}"`, `r#"}"#` — `strip_test_modules`
вернул ровно production-tail, тест **passed**. Это P3-7: путь корректен, но не
запинен.

---

## 3. Полная приёмка T04b — re-run на 6ac14c2

Все инварианты подтверждены прогонами run 1/run 2 (54 таргета); ниже — точные
имена тестов из логов.

| Инвариант | Тест(ы) (все `ok`) |
|---|---|
| ровно одна строка оффера на кодек/вариант | `rtp_bridge::tests::offer_rows_match_spec`, `sip_offer_has_no_duplicate_payload_types`, `validate_offer_rows` (внутри) |
| Native video ⇒ оффер; Unsupported/RxOnly/WebRTC — нет | `native_disposition_matches_offer_gate_and_public_matrix`, `advertised_supported_implies_negotiable`, `unknown_and_unsupported_video_is_never_offered`, `disabled_or_unsupported_audio_is_rejected` |
| нет Unsupported в рекламе | `advertised_supported_implies_negotiable`, `unsupported_specs_have_a_reason_from_the_dictionary`, `audio_codec_matrix_marks_unsupported_variants_instead_of_promising_them` |
| матрица/MEDIA_PIPELINE/license/net status/native lists согласованы | `docs_public_matrix_matches_ssot`, `license_params_are_derived_from_ssot_dispositions`, `public_codec_lists_are_canonical_and_pinned` (`/ui/system/status`), `public_native_lists_are_canonical_and_deduplicated`, `alias_families_are_audio_native_and_present_in_every_native_audio_slice` |
| AAC reason order | `codec_capabilities::tests::aac_reason_order_is_license_then_probe_then_pool` |
| канонические имена+clock | `media_details_clock_and_rate_are_derived_from_the_ssot`, `gateway_audio_bridges_opus_at_peer_dynamic_pt_with_48khz_clock`, `…g722_pt9_with_8khz…`, `…g726_32…` |
| proto wire tags не менялись | `vks-proto::tests::codec_id_wire_tags_are_stable`; `git diff a02daad..6ac14c2 -- crates/vks-proto` **пуст** |
| PT-карты UAS/server/H.323 ↔ disposition | `uas_static_pt_map_matches_ssot_disposition`, `sip_pt_maps_match_ssot_disposition`, `h323_slices_match_spec_dispositions` |
| сканирование **каждого** production-файла | `no_codec_name_cluster_outside_allowlist`, `no_second_codec_name_table_outside_allowlist`, `ssot_consumer_scan_list_is_complete`, `production_source_keeps_real_repo_production_tails` |
| production не перетирает SSOT caps | `production_handler_config_keeps_ssot_caps_across_room_mode_and_dtls`, `production_main_does_not_overwrite_the_ssot_caps` |
| SDP/PT без регрессий | `sip_offer_has_no_duplicate_payload_types`, Polycom/G722/AAC/H264/H265 негативация — ok в обоих прогонах |

### 3.1. Мутации review C (a)–(e), адаптированные к HEAD

| # | Мутация | Поймавший тест / результат |
|---|---|---|
| a | `H263.sip = Native` (`codec_spec.rs:549`) | **compile-fail** `E0277: [&str;3] vs [&str;2]` в common test-target; runtime **`offer_rows_match_spec`** (`rtp_bridge.rs:1144`): «H263 must have exactly one VIDEO_OFFER row» |
| b | оригинал (`production_rtp_maps`) **удалён T11c** — N/A; эквивалент: `.with_caps(` в production `main.rs` | **`no_second_codec_name_table_outside_allowlist`** ловит `vks-server/src/main.rs: ".with_caps("` («production must not overwrite SSOT-derived RoomCaps»); плюс `production_main_does_not_overwrite_the_ssot_caps` |
| c | `license_codec_params`: `*key == "g729" \|\| available(name)` | **`license_params_are_derived_from_ssot_dispositions`** (`shell.rs:1262`): «license audio g729 must derive from the SSOT» |
| d1 | новый `codec_table_dup.rs` с `["OPUS","VP8"]` | **cluster-тест**, файл назван в сообщении |
| d2 | новый `new_consumer.rs` с `use vks_common::codec_spec as _ssot;` | **`ssot_consumer_scan_list_is_complete`** (`codec_spec.rs:2085`), drift-список содержит файл |
| e | `CODEC_VARIANTS` 32k: `rtp_clock: 32_000 → 16_000` (`codec_spec.rs:623`) | **`g7221_variants_model_both_clocks_truthfully`** (`codec_spec.rs:1252`) |
| доп. | `G722` H.323-ячейка в `MEDIA_PIPELINE.md` → `—` | **`docs_public_matrix_matches_ssot`** (`codec_spec.rs:1546`) |

Все пробы пойманы; файлы восстановлены по `git show 6ac14c2:<path>`, sha256
совпал (7/7).

---

## 4. P1-1. Workspace-тест на HEAD красный (регрессия T11c, не T04b)

`crates/vks-server/tests/t10_directory_failclosed.rs::corrupt_document_with_fallback_exits_78_and_keeps_directory_rows`
**FAILED** в обоих полных прогонах и изолированно (0.06s, детерминированно —
не флейк).

- Падение: `t10_directory_failclosed.rs:71`
  (`assert!(logs.contains("failed to hydrate directory data from storage"))`).
- Причина: тест запускает бинарь с `.env_clear()` и без
  `VKS_WEBRTC_TRANSPORT` (`t10_directory_failclosed.rs:60-67`); на HEAD
  `vks-server/src/main.rs:97` вызывает `require_transport()` **до открытия
  storage**, `webrtc_runtime.rs:42` возвращает «VKS_WEBRTC_TRANSPORT is
  required and has no default», `main.rs:39 config_exit` завершает процесс с
  кодом 78. Exit-код совпадает (строка 70 проходит), но сообщения о hydration
  нет — тест ждёт более позднюю ветку.
- Атрибуция: регрессию внесли T11c-коммиты `e363e40`/`b6dd9cb` (обязательный
  транспорт), а не `c16d838`/`6ac14c2`. Тест T10 не был обновлён под
  обязательный env.
- В live-дереве sibling-агент уже получил зелёный прогон:
  `/dev/shm/t11c_final3_test1.log` — 54 таргета, **1535/0/8**, t10 ok
  (`t11c_final3_test1.log:1681`), т.е. фикс существует вне ревизии `6ac14c2`.

Формально это блокер приёмки HEAD (`0 P1`), поэтому `done: no`. Если Lead
ревьюит T04b в отрыве от T11c — code-level приёмка T04b проходит (P2 закрыт,
инварианты зелёные); P1 принадлежит T11c/T10 и должен быть закоммичен в HEAD.

---

## 5. P2-1. Заявленные counts `1610/0/8` не воспроизводятся

На `6ac14c2`: **1534 passed / 1 failed / 8 ignored** (54 таргета) дважды
идентично; база без t10-падения = **1535/0/8** (подтверждено sibling-логом
`t11c_final3_test1.log`). `1610` не соответствует этой ревизии: T11c удалил
65 unit-тестов `vks-media-sfu` (112 → 47; `2d5c73e`: engine/udp_loop/dtls/ice/
forwarder/simulcast) и 8 тестов signaling (`044249f`), добавив тесты в
`media-mcu`/`common`/`admin`. Итог `6ac14c2` ≈ 1535, а не 1610.

Рекомендация: обновить ожидаемую базу в приёмке (1535/0/8 для HEAD после
t10-фикса; 1543 с ignored) либо указать, с какой ревизии снят `1610`.

---

## 6. P3-хвост (8)

Закрыты в `c16d838` (проверено): P3-1 review C (генераторы теперь итерируют
`ALIAS_FAMILIES`, `codec_spec.rs:795,816-817,924,945-946,989,1007-1008`; тест
`alias_families_are_audio_native_and_present_in_every_native_audio_slice`) и
P3-5 (PT-поле проверяется по идентификаторам, `specs_have_no_payload_type_field`,
`codec_spec.rs:1497-1525`). Остаются:

1. **`crates/vks-server/src/gateways.rs:3702`** — H.323 inbound
   `audio_clock_rate: 8000` литералом; `audio_sample_rate` выводится из SSOT,
   а clock — нет (сегодня корректно: все H.323-native аудио 8 кГц).
2. **`crates/vks-server/src/transcoder_runtime.rs:81-97`** — `publish_support`
   второй литеральный список codec-меток; тест
   `worker_metric_labels_are_subset_of_specs` (`:136-142`) перебирает тот же
   литерал, а не реальные метки.
3. **`crates/vks-common/src/codec_spec.rs:1406-1412`** —
   `h264_high_is_rx_only_and_never_offered` проверяет
   `!H323_AUDIO_NAMES.contains("H264-HIGH")` (для video тривиально); отсутствие
   в оффере держит gateway-тест.
4. **`crates/vks-gateway-sip/src/uas.rs:407-422`** (вызовы `:552`, `:560`) —
   `payload_is` сравнивает wire-имя с каноническим: `a=rtpmap:4 G7231/8000`
   отвергается, хотя `lookup("G7231")` — Native (алиас SSOT).
5. **`crates/vks-admin/src/api_v3/shell.rs:511-520`** — `GET /api/v3/features`
   литеральный vendor-список, включающий `G.719`, `G.729`, `H.261`, `H.263`,
   `H.264-HIGH`, `AAC`, хотя `/license` и матрица для них `false`/Unsupported;
   теста связи с SSOT нет.
6. **`docs/architecture/MEDIA_PIPELINE.md:177-184,194`** — колонка `Reason`
   SIP-центрична: у G726/G728/OPUS/H265 H.323-ячейка `—` без причины, у G729
   `encoder_not_bit_exact` при H.323 `—`.
7. **`crates/vks-common/src/codec_spec.rs:2231-2244`** — коммит-фикстуры lexer
   не покрывают char-литералы, блочные комментарии и byte/raw-byte строки;
   поведение верно (доказано временной пробой), но не запинено.
8. **`crates/vks-server/src/main.rs:568`** —
   `production_main_does_not_overwrite_the_ssot_caps` всё ещё режет по первому
   `\n#[cfg(test)]` вместо нового lexer; сейчас main.rs держит тесты в конце
   (`:512+`), поэтому разрыва нет, но ранний `#[cfg(test)]` helper снова ослепит
   этот guard.

---

## 7. Регрессии и truthfulness

- `git diff a02daad..6ac14c2 -- crates/vks-proto` пуст; proto-теги 0..8 не
  менялись (`codec_id_wire_tags_are_stable`).
- Золотые SDP-строки и порядок PT не менялись с round 2: тесты
  `sip_offer_has_no_duplicate_payload_types`, Polycom-answer, G722 name+clock,
  AAC license on/off, H264/H265 probe — зелёные в обоих прогонах.
- SFU answer (Opus+VP8) строится из `webrtc_answer_specs()`; fail-closed на
  чужой id — тесты зелёные.
- H.323 PT-набор `[0,8,9,4]`, sidecar не расширен — зелёные.
- Dead code/allowlist-трюков не добавлено: clippy `-D warnings` чистый,
  `NON_CONSUMER_EXCEPTIONS`/`NAME_SPELLING_ALLOWLIST` с непустыми причинами
  проверяются (`ssot_consumer_scan_list_is_complete`).

---

## 8. Оценка

| Критерий | Статус |
|---|---|
| 0 P0 | да |
| 0 P1 | **нет** (P1-1: t10 на HEAD, регрессия T11c) |
| fmt/clippy/wasm/doc | да |
| workspace tests | 1534/1/8 дважды, идентично; изолированный t10 падает так же |
| counts vs 1610/0/8 | **нет** (см. P2-1; база 6ac14c2 ≈ 1535) |
| 5 мутаций (a–e) + doc-tamper | да (a: compile+2 runtime; b адаптирован; c,d1,d2,e,doc — named) |
| P2-1 review C закрыт исполнением | **да** (M1–M4, ложных срабатываний нет) |
| «нигде нет второй таблицы» | да (включая production-хвосты) |
| SDP/PT/proto без регрессий | да |
| Гигиена | да; см. §9 |

**Оценка: 8.5/10, `done: no`.**

Почему 8.5: сам T04b-фикс качественный — P2 review C закрыт с реальным
мутационным доказательством (4 инъекции + lexer), все T04b-инварианты и
мутации (a)–(e) зелёные, clippy/fmt/wasm чистые. Но формальная приёмка HEAD
невозможна: workspace-тест красный (P1-1) и заявленная база counts не
воспроизводится (P2-1). Оба дефекта вне diff `c16d838`/`6ac14c2` (T11c), но
приёмка выносится по HEAD. Если P1 будет закрыт коммитом в HEAD и база counts
обновлена — повторный прогон T04b проходит без замечаний (code-level уже
зелёный).

---

## 9. Гигиена и состояние для продолжения

- Live-дерево `/home/pro/Рабочий стол/mcu` не редактировалось, кроме этого
  отчёта; сервер не запускался, БД/порты/бэкапы/systemd не трогались.
- Все эксперименты — в `/dev/shm/rvD_1793282838` (pristine `git archive
  6ac14c2`) и target `/home/pro/.cache/rvD_1793282838_target`; в `/tmp`
  ничего не создавалось.
- Все 7 мутированных путей восстановлены и сверены
  `sha256 == git show 6ac14c2:<path>` (codec_spec.rs, main.rs, shell.rs,
  gateways.rs, directory_migrate.rs, MEDIA_PIPELINE.md; rtp_bridge.rs не
  мутировался). Scratch-файлы `reviewd_*`, `codec_table_dup.rs`,
  `new_consumer.rs` удалены; `vks-common --lib` после отката — 108/0.
- **Сохранено для продолжения** (по команде Lead работа остановлена, уборка
  отложена): pristine-копия `/dev/shm/rvD_1793282838`, target
  `/home/pro/.cache/rvD_1793282838_target`, логи
  `/dev/shm/rvD_1793282838_{clippy_ws,clippy_aff,test1,test2,wasm,common_lib,
  final_common,final_server_lib,isolated_t10}.log` и `rvD_mut_*.log`.
- Осталось при возобновлении: (1) дождаться/проверить коммит фикса t10 в HEAD
  (P1-1) и повторить два полных прогона; (2) обновить базу counts (P2-1);
  (3) убрать за собой pristine/target/логи и тестовые БД
  `vks-t10-failclosed-*.db` в `/dev/shm`, появившиеся из упавших прогонов;
  (4) при желании — закрыть P3-7/P3-8 и остальной P3-хвост.
