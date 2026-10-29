# T04b — независимое ревью B: CodecSpec SSOT и матрица диспозиций

- **Ревизия:** `a02daad` (`T04b: single codec SSOT ...`), parent `15561ff`, branch `master`.
- **Метод:** pristine-копия `git archive a02daad` → `/dev/shm/t04b_reviewB/repo`
  (не live-дерево); Rust **1.94.0**; `CARGO_TARGET_DIR=/home/pro/.cache/t04b_reviewB_target`;
  все сборки/тесты — `--offline --locked`; live-дерево не редактировалось, кроме этого отчёта.
- **Итог:** **не принято (`done: no`)**; **8.0/10**.
- **Дефекты:** **P0 = 0, P1 = 2, P2 = 7, P3 = 5.**

Коротко: содержательно SSOT сделан хорошо — 26 строк, генерируемые `const fn`
срезы, неизменные proto-теги, H.264 High RX-only, причины у Unsupported,
license/probe-гейт AAC, doc-check матрицы и отсутствие дрейфа SDP/PT подтверждены
исполнением. Приёмку блокируют два P1:

1. production `main.rs` по-прежнему собирает codec-поля `RoomCaps` и `RtpMap`
   литералами и перекрывает SSOT-производные caps; заявленное «signaling
   RoomCaps ... consume the SSOT» для боевого пути неверно;
2. мета-инвариант «видеокодек со `sip: Native` обязан быть в оффере/negotiable»
   не проверяется: мутация `H263.sip = Native` проходит **все 1526** продуктовых
   тестов, публичная матрица начинает говорить `native`, а админка — `false`.

---

## 1. Гейты и доказательства исполнением

### 1.1. Гейты

| Гейт | Команда | Результат |
|---|---|---|
| fmt | `cargo fmt --all -- --check` | exit 0 |
| clippy affected | 7 затронутых пакетов, `--all-targets --locked --offline -- -D warnings` | exit 0, 0 warnings |
| clippy workspace all-features | `--workspace --all-targets --all-features --locked --offline -- -D warnings` | exit 0, 0 warnings |
| workspace, первый чистый прогон | `cargo test --workspace --locked --offline --no-fail-fast` | **1526 passed, 0 failed, 8 ignored**, exit 0 |
| workspace, повторный прогон | то же | **1525 passed, 1 failed, 8 ignored** — только timing-flake `gateway_audio_bridges_g723_at_pt4_with_30ms_timestamps`; см. P3-1 |
| `vks-server --lib` 3× | `cargo test -p vks-server --lib --locked --offline` | 149 passed / 0 failed каждый раз |
| ключевые тесты 3× | `codec_spec` (16), `vks-gateway-sip --lib` (159), H.323 PT (1), signaling caps (1), admin `codec_matrix` (3), media-sfu `webrtc_answer_slice` (1) | все 3 прогона зелёные |

Счётчики по пакетам в первом чистом прогоне: `vks-common` 92 lib (+2
integration), `vks-gateway-sip` 159 (+1 ignored), `vks-gateway-h323` 46 (+1
ignored), `vks-signaling` 48, `vks-media-sfu` 98, `vks-admin` 357, `vks-server`
149 lib (+3 ignored) + 35 integration. Итого **1526 / 0 / 8**, 51 бинарь.

### 1.2. Reviewer-пробы (не входят в продуктовый счёт)

В скретч-копию `mut` (не в отчётный diff) добавлены 4 SIP-пробы и 1 admin-проба:

- `probe_every_sent_audio_row_negotiates_without_pt_drift` — все отправляемые
  Native-строки, Native и Full, реально согласуются, PT не дрейфует; G722,
  переименованный пиром в PT96, матчится по name+clock;
- `probe_aac_license_state_controls_negotiation` — `aac=false` → типизированный
  `UnsupportedAudio{113}`, `aac=true` → `MP4A-LATM`;
- `probe_video_probe_state_and_unknown_video` — пробы H264/H265 управляют видео,
  неизвестный `h246` не становится видео;
- `probe_identity_rows_reject_with_typed_reason` — 7 identity-строк отдают
  `UnsupportedAudio` с identity+reason;
- `probe_status_codecs_json_matches_generated_slices` — `GET
  /api/v3/ui/system/status`: `codecs.native_audio == NATIVE_AUDIO`,
  `codecs.native_video == ["H264","H265","HEVC"]`, `transcoder == false`,
  G7231/G711U/G711A/G726-16/G728 присутствуют, G729/G7221/G719/ILBC/MP4A-LATM
  отсутствуют.

Все 5 проб на pristine прошли. Они использованы только как внешний оракул для
мутаций M8/M9; в продуктовый счёт не включены.

---

## 2. P1 — блокирует приёмку

### P1-1. Production `RoomCaps`/`RtpMap` собираются литералами и перекрывают SSOT

`crates/vks-server/src/main.rs:363-367`:

```rust
let caps = vks_proto::media::RoomCaps {
    room_mode: proto_room_mode,
    video_codecs: vec![vks_proto::media::CodecId::Vp8 as i32],
    audio_codecs: vec![vks_proto::media::CodecId::Opus as i32],
    ...
};
```

`crates/vks-server/src/main.rs:384-387` — литеральные `RtpMap` `opus/111` и
`vp8/96`; `main.rs:388-391` вызывает `.with_caps(caps)`.

При этом `HandlerConfig::new` уже ставит caps из SSOT
(`crates/vks-signaling/src/handler.rs:47-75`, вызов `:99`), а `with_caps`
(`handler.rs:125-129`) **заменяет** их целиком. Функция `webrtc_room_caps()`
используется только внутри `handler.rs` (конструкторы и тест
`proto_caps_match_ssot_webrtc_slice`); в боевом `main` её нет. То есть тест
проверяет неиспользуемую функцию, а не тот `RoomCaps`, который уходит клиенту.

Последствие: добавление/включение WebRTC-кодека в `CODECS` обновит
`webrtc_answer_specs()`, SFU-answer и тест caps, но `ServerHello.caps` и
legacy SDP-answer останутся Opus/VP8. Это не текущий проволочный баг (значения
сейчас совпадают), но прямое нарушение требования «signaling RoomCaps ints
derived from or explicitly checked against CODECS» и T1-риска «SSOT стал седьмым
списком».

**Доказательство мутацией M9.** В `main.rs` добавлены `H265`/`G722` в caps и
`h265/99`, `g722/9` в `RtpMap`. `cargo test -p vks-signaling` — **48/48 pass**;
`cargo test -p vks-server --lib` — **149/149 pass** в двух прогонах (третий упал
только на двух независимых timing-тестах, см. P3-1). Ни один тест не заметил,
что production caps/SDP-answer разошлись с SSOT. Мутация убрана.

**Фикс:** экспортировать из signaling параметризованный
`room_caps_for(room_mode, dtls_srtp)` (или строить codec-поля в `main` из
`webrtc_answer_specs()`), убрать литералы; `RtpMap` строить из тех же спеков,
оставив только transport-local PT. Добавить тест на **production-builder**, а не
на приватную `webrtc_room_caps()`.

### P1-2. Нет мета-инварианта «`sip: Native` video ⇒ оффер/negotiable»

`native_slices_are_derived_and_complete` (`crates/vks-common/src/codec_spec.rs:968-1002`)
проверяет только membership `NATIVE_VIDEO` против `spec.sip == Native`; он не
требует наличия строки в `VIDEO_OFFER`. `offer_rows_match_spec`
(`crates/vks-gateway-sip/src/rtp_bridge.rs:939-966`) обходит **строки оффера**, а
не все video-спеки, поэтому Native-видео без строки проходит. Level-3 тест
`advertised_supported_implies_negotiable` (`rtp_bridge.rs:976-1002`) написан
**только для audio** (`filter(kind == Audio)`), хотя комментарий обещает «every
codec the admin reports as supported».

**Доказательство мутацией M8.** В SSOT `H263.sip` заменён на `Native`,
`reason: None`, строка таблицы `MEDIA_PIPELINE.md` — на `native`.
`cargo test --workspace --no-fail-fast`:

```
1530 passed; 1 failed; 8 ignored
```

1530 = 1526 продуктовых + 4 reviewer-пробы; единственный failed — **внешняя**
проба `t04b_probe_status_codecs_json_matches_generated_slices`, которая увидела
`H263` в `native_video`. Все 1526 продуктовых тестов прошли. При этом:

- публичная матрица говорит `H263 | video | native | ...`, т.е. «мостится
  bit-exact»;
- `video_unavailable_reason("H263")` возвращает `probe_unavailable`
  (`crates/vks-common/src/codec_capabilities.rs:164-180`), админка говорит
  `supported=false`;
- в `VIDEO_OFFER` строки H263 нет, в H.323 — нет;
- `GET /api/v3/ui/system/status.codecs.native_video` включил бы H263.

Это ровно «ложь в публичной матрице», которую T04b должен был исключить.
Аналогичная дыра существует для любого будущего Native-видео (H265 после
изменения probe-политики, AV1 и т.п.).

**Фикс:** добавить video-версию Level-3 теста: для каждого `CodecKind::Video`
со `sip: Native` требовать строку в `VIDEO_OFFER` (и `offer_video_enabled(...,
true, true)`), а для `RxOnly/Unsupported/WebRtc` — отсутствие; плюс обратную
полноту «каждая Native video-спека ровно один раз». Это закроет и P2-5.

---

## 3. P2

### P2-1. VP9/AV1: причина `webrtc_only` при WebRTC-колонке `—`

`codec_spec.rs:477-490` (VP9) и `:492-505` (AV1):

```rust
sip: Disposition::Unsupported(Reason::WebRtcOnly),
h323: Disposition::Unsupported(Reason::WebRtcOnly),
webrtc: Disposition::Unsupported(Reason::PendingPayloader),
webrtc_negotiable: false,
reason: Some(Reason::WebRtcOnly),
```

`render_markdown_matrix` (`codec_spec.rs:892-913`) печатает
`spec.webrtc.label()` (для VP9/AV1 — `—`) и `spec.reason` (для SIP —
`webrtc_only`). В `MEDIA_PIPELINE.md:200-202` получается:

```
| VP9 | video | — | — | — | webrtc_only |
| AV1 | video | — | — | — | webrtc_only |
```

Легенда того же файла (`MEDIA_PIPELINE.md:207-210`) говорит: `webrtc` = present
in the str0m answer, `—` = not available on that plane. Читатель видит «кодек
доступен только на WebRTC» и одновременно «на WebRTC его нет». Фактическая
причина — `pending_payloader`; `Reason::WebRtcOnly` — причина для SIP, а колонка
Reason в таблице SIP-центрична, но это нигде не сказано. Это текущая ложь в
публичном документе, а не только будущий риск.

**Фикс:** либо печатать причину по plane (`sip_reason`/`webrtc_reason`), либо
для VP9/AV1 ставить `PendingPayloader` и в `reason`, либо явно подписать колонку
«SIP reason». Заодно проверить, что `webrtc_negotiable == (webrtc == WebRtc)`
для всех строк, кроме задокументированных исключений.

### P2-2. Причина AAC всегда `license_not_accepted`, даже если лицензия принята

`crates/vks-common/src/codec_capabilities.rs:149-159`:

```rust
Some(spec) if spec.sip == Disposition::Worker => Some(Reason::LicenseNotAccepted.as_id()),
```

Комментарий честно признаёт: «the process exposes only the combined availability
flag». Но combined-флаг считается в `crates/vks-server/src/main.rs:414-421` как
`legal && pool.max_workers() >= 2 && has_audio_latm_bridge`. Если оператор принял
лицензию, а probe/pool отсутствует, админка покажет `supported=false` с причиной
`license_not_accepted`, хотя лицензия принята. Это нарушает T22-контракт
«reason — машинно-читаемая причина, а не догадка» и вводит оператора в
заблуждение при диагностике.

**Фикс:** хранить `aac_latm_legal` и `aac_latm_probe` отдельно (или возвращать
`probe_unavailable`/`worker_unavailable` при legal=true), а `License`-поле SSOT
использовать для юридической части. Либо явно назвать причину
`license_or_probe_unavailable`, если разделить нельзя.

### P2-3. `NegotiatedMedia.audio_codec` не канонический для Opus → неверный clock в media details

`crates/vks-gateway-sip/src/rtp_bridge.rs:1232-1233` документирует:

```rust
/// Canonical name of the negotiated audio codec (always natively bridged).
pub audio_codec: &'static str,
```

но присваивание — `audio_codec: codec.name` (`rtp_bridge.rs:1680`), а строка
`AUDIO_OFFER` для Opus имеет `name: "opus"` (`rtp_bridge.rs:394-398`, нижний
регистр). В `crates/vks-server/src/gateways.rs:1549-1550` далее:

```rust
"audio_clock_rate": match audio_codec { "OPUS" => 48000, "MP4A-LATM" => 90000, _ => 8000 },
```

Для Opus `audio_codec == "opus"`, поэтому ветка `"OPUS"` мертва и в media details
уходит **8000 вместо 48000**; `audio_sample_rate` рядом (строка 1551) уже
использует SSOT и вернёт 48000 — в одном JSON два противоречащих поля. Это
текущий баг, а не только стилистика: `audio_clock_rate` видят админка и
диагностика.

**Фикс:** в `rtp_bridge.rs:1680` писать
`vks_common::codec_spec::spec(codec.spec).name` (каноническое `OPUS`), либо
сравнивать case-insensitive и брать `rtp_clock` из спека. Тест на Opus
media-details отсутствует.

### P2-4. `G7221C` (clock 32k) не смоделирован в SSOT; мета-тест это исключает

`CodecSpec` для G7221 один: `rtp_clock: 16_000`, alias `G7221C`
(`codec_spec.rs:370-375`). При этом `AUDIO_OFFER` содержит пять identity-строк
G7221: PT102/103 — 16k, PT104-106 — **32k** (`rtp_bridge.rs:320-355`). Мета-тест
`offer_rows_match_spec` **явно отключает** проверку clock для всех строк G7221
(`rtp_bridge.rs:948-952`):

```rust
// G7221 identity rows cover both ... the spec documents the 16 kHz variant.
if codec.spec != CodecId::G7221 {
    assert_eq!(spec.rtp_clock, codec.clock, ...);
}
```

`lookup("G7221C")` вернёт 16k; 32k-вариант живёт только в transport-таблице, а
инвариант «clock строки = clock spec» для трёх 32k-строк отключён. Функционально
G7221/C не офферятся, поэтому это не проволочная ложь, но принятый дизайн
«G7221C variant clock» не выполнен, и тест не защищает будущие правки.

**Фикс:** `variants: &[(clock, bitrate)]` в `CodecSpec` (или отдельный
`G7221C` spec), убрать исключение и проверять clock для всех строк.

### P2-5. «Ровно одна строка/ветка на кодек» и соответствие `name → spec` не enforced

`offer_rows_match_spec` (`rtp_bridge.rs:939-966`) использует
`audio_offer(...).iter().any(...)`, т.е. проверяет «есть хотя бы одна», а не
«ровно одна»; он также **не проверяет** `lookup(codec.name).id == codec.spec`.
`advertised_supported_implies_negotiable` тоже построен на `any`.

**Эксперимент (mut, затем восстановлено).** В `AUDIO_OFFER` добавлена лишняя
identity-строка:

```rust
OfferCodec { payload: 114, name: "G729", clock: 48000, channels: 1,
             fmtp: None, spec: CodecId::G719, dtmf: false }
```

`cargo test -p vks-gateway-sip --lib` → **163 passed, 0 failed, 1 ignored**.
Лишняя строка с чужим spec, чужим именем и чужим clock прошла все мета-тесты.
(При clock 8000 падает только clock-assert; при 48000 — проходит.) Значит:
дубликат identity-строки не ловится, несоответствие wire-имени и spec не
ловится, «exactly one identity row» — декларация.

**Фикс:** сгруппировать строки по `spec` и требовать count==1 (для G7221 — по
варианту), проверять `lookup(codec.name).id == codec.spec`, а также обратную
полноту `VIDEO_OFFER` (см. P1-2).

### P2-6. `/api/v3/license` `params` — жёстко `true` для нереализованных кодеков

`crates/vks-admin/src/api_v3/shell.rs:374-390`:

```rust
"g711": true, "g719": true, "g722": true, "g723": true, "g726": true,
"g728": true, "g729": true, "ilbc": true, "opus": true, "aac-lc": true,
"g722.1": true, "g722.1c": true
...
"vp8": true, "h261": true, "h263": true, "h264": true, "h265": true,
"h264-svc": true, "h264-high": true, ...
```

Это публичный ответ, который vendor-UI читает для вкладок возможностей
(комментарий `shell.rs:355-362`). Он говорит `true` для G719/G729/ILBC/G722.1/
H261/H263/H264-SVC/H264-HIGH/AAC-LC, тогда как SSOT и админ-матрица говорят
`Unsupported`/`RxOnly` с причинами (`no_implementation`,
`encoder_not_bit_exact`, `tx_profile_unsupported`, `svc_not_forwarded`).
Предсуществующее (файл не входит в diff `a02daad`), но T04b-заявка «публичная
матрица больше не лжёт» его не покрывает; `no_second_codec_name_table` этот файл
не сканирует.

**Фикс:** генерировать `params` из SSOT/`is_supported_*` или явно назвать его
`license_entitlements` и не смешивать с runtime-возможностями; добавить
doc/route-тест.

### P2-7. `no_second_codec_name_table_outside_allowlist` — слабый и неполный

`codec_spec.rs:1187-1225` проверяет 8 файлов на подстроку `codec_spec` и 9
точных старых needles. Он **не сканирует** `vks-server/src/main.rs`,
`vks-signaling/src/handler.rs`, `vks-media-sfu/src/webrtc.rs`,
`vks-admin/src/api_v3/shell.rs`, `vks-admin/src/settings.rs`,
`vks-core/src/media_policy.rs`, `gateway_video.rs` и любые новые файлы; allowlist
неявный (мёртвые карты просто не упомянуты). Поэтому P1-1 и P2-6 существуют, а
новая литеральная таблица в новом файле пройдёт тест. Это ровно T1-риск
«SSOT может стать седьмым списком, и CI этого не заметит».

**Фикс:** сканировать `crates/**/*.rs` (и `client/**`) на токены имён из
`CODECS` с **явным** allowlist `(file, reason)`; либо добить оставшиеся таблицы
и проверять отсутствие литералов по всем крейтам.

---

## 4. P3

### P3-1. Timing-тесты `vks-server` флакуют под параллельной нагрузкой

Повторный полный `cargo test --workspace` дал 1525/1:

```
gateways::media_runtime_tests::gateway_audio_bridges_g723_at_pt4_with_30ms_timestamps
  panicked at gateways.rs:5907: "G.723.1 playout underran despite prefill: 2 gaps"
```

Тот же тест отдельно — **8/8 pass** (0.50–0.62 s). В другом прогоне под
нагрузкой упали `gateway_audio_bridges_g722_pt9_with_8khz_timestamp_clock`
(`gateways.rs:5493`) и `gateway_audio_consumes_both_halves_of_a_40ms_packet_once`;
каждый отдельно — 5/5 pass. Это не дефект T04b (тесты не менялись коммитом), но
приёмка «workspace tests stable» не выполняется детерминированно; в отчёте
нельзя писать «всегда 1526/0» без оговорки.

**Фикс:** убрать зависимость playout-тестов от wall-clock (детерминированные
часы/канал) или пометить `#[ignore]`/serial.

### P3-2. Дублирование sample-rate/clock в inbound media details

`crates/vks-server/src/gateways.rs:2942-2948` для inbound UAS снова матчит
`"G722" => 16_000`, `"OPUS" | "MP4A-LATM" => 48_000`, `_ => 8_000`, хотя рядом
есть SSOT-хелперы `audio_sample_rate` (строка 969) и
`audio_sample_rate_for_codec` (строка 977). Сейчас имена из UAS канонические
(верхний регистр), поэтому значения верны, но это третья копия истины: мутация
`sample_rate`/`rtp_clock` в SSOT не обновит этот JSON. Аналогично
`gateways.rs:3646` для H.323 жёстко `8000`.

**Фикс:** использовать `audio_sample_rate(media.audio_payload_type)`/
`audio_sample_rate_for_codec(media.audio_codec)` и `spec.rtp_clock`.

### P3-3. `ALIAS_FAMILIES[0]` и `+1` захардкожены

`native_audio_count` (`codec_spec.rs:678-687`) и `h323_audio_count`
(`:801-812`) добавляют ровно `+1` под G711, а `build_native_audio`
(`:690-708`) и `h323_audio_names` (`:814-833`) дописывают ровно
`ALIAS_FAMILIES[0].name`. Вторая alias-семья не попадёт ни в `NATIVE_AUDIO`, ни
в H.323, и тест `native_slices_are_derived_and_complete` этого не заметит.

**Фикс:** генерировать family-имена циклом по `ALIAS_FAMILIES`, считать
`count += ALIAS_FAMILIES.len()`.

### P3-4. `h264_high_is_rx_only_and_never_offered` проверяет только H.323 audio

`codec_spec.rs:1111-1117`:

```rust
assert!(!H323_AUDIO_NAMES.contains(&"H264-HIGH"));
```

H264-HIGH — video, поэтому эта проверка тривиальна; отсутствие в `VIDEO_OFFER`
проверяет только gateway-тест `full_offer_never_advertises_h264_high...`.
Common-тест не защищает инвариант при изменении `OfferCodec`/`VIDEO_OFFER`.

**Фикс:** проверять `VIDEO_OFFER`/`offer_video_enabled` в common или вынести
общий мета-тест в gateway.

### P3-5. `specs_have_no_payload_type_field` — слабый substring-check

`codec_spec.rs:1150-1159` ищет в теле структуры только `"payload"` и
`"packet_type"`. Поле `pt: u8` или `payload_type` (содержит `payload` — поймает),
но `pt`/`ptype`/`payloadtype` пройдут. Это скорее страховка, чем инвариант.

**Фикс:** явно проверять отсутствие любых полей с `pt`/`type` или
инспектировать AST; либо убрать тест и оставить комментарий.

---

## 5. Мутационная матрица

Все мутации — в `/dev/shm/t04b_reviewB/mut` (копия pristine), после каждой
файлы восстановлены. «Pass» = мутация не поймана; это P1/P2-покрытие.

| # | Мутация | Кто поймал (точные тесты) | Итог |
|---|---|---|---|
| M1 | `G722.sip = Unsupported(NoImplementation)` + reason | common: `codec_capabilities::tests::native_names_match_gateway_offer`, `codec_capabilities::tests::supported_audio_variant_tracks_the_aac_probe_and_license`, `codec_spec::tests::docs_public_matrix_matches_ssot`; sip: `g722_and_strict_first_regressions::native_offer_selects_g722_when_peer_lists_it_before_pcmu`, `negotiation_regressions::renumbered_dynamic_payload_is_matched_by_name_and_clock`, `t04b_probe_tests::probe_every_sent_audio_row_negotiates_without_pt_drift`, `tests::full_offer_carries_only_codecs_the_runtime_can_carry`, `tests::native_profile_offers_only_bridgeable_codecs`; admin: `audio_codec_matrix_marks_unsupported_variants_instead_of_promising_them`, `t04b_probe_status_codecs_json_matches_generated_slices`; server: `conference_intent_is_a_fallback_when_no_contact_or_address`, `room_contact_intent_wins_over_global_address_and_uses_camel_keys`, `sip_keeps_g728_and_opus_while_h323_does_not` | caught |
| M2 | `PCMA.aliases += "G711U"` (дубль) | common: `ssot_specs_have_unique_ids_names_and_aliases`, `native_slices_are_derived_and_complete`, `every_spec_has_one_disposition_per_plane_and_resolves_by_name` | caught |
| M3 | `H264High.sip = Native`, добавлена PT98, открыт `offer_video_enabled` | common: `native_slices_are_derived_and_complete`, `native_consts_equal_generated_slices`, `docs_public_matrix_matches_ssot`, `h264_high_is_rx_only_and_never_offered`, `unsupported_reasons_are_complete`; sip: `tests::offer_rows_match_spec`, `tests::full_offer_never_advertises_h264_high_we_cannot_encode`, `tests::h264_high_is_rx_only_and_has_public_reason`, `tests::native_profile_offers_only_bridgeable_codecs`, `tests::offer_carries_bridgeable_video_matrix`, `h265_offer_tests::unknown_and_unsupported_video_is_never_offered`, `negotiation_regressions::h264_baseline_in_answer_is_bridged_even_when_high_profile_is_listed_first`, `t04b_probe_tests::probe_video_probe_state_and_unknown_video`; admin: `video_codec_matrix_reports_reasons_for_unsupported_entries`, `t04b_probe_status_codecs_json_matches_generated_slices` | caught |
| M4 | `G723.sip = Unsupported(NoImplementation)` | common: `native_names_match_gateway_offer`, `native_consts_equal_generated_slices`, `docs_public_matrix_matches_ssot`, `g7231_alias_is_supported`, `native_slices_are_derived_and_complete`; sip: `g722_and_strict_first_regressions::native_offer_selects_g722_when_peer_lists_it_before_pcmu`, `tests::full_offer_carries_only_codecs_the_runtime_can_carry`, `tests::native_profile_offers_only_bridgeable_codecs`, `t04b_probe_tests::probe_every_sent_audio_row_negotiates_without_pt_drift`; admin: `t04b_probe_status_codecs_json_matches_generated_slices`; server: `sip_keeps_g728_and_opus_while_h323_does_not` | caught |
| M5 | `G729.reason = None` (осталось Unsupported) | common: `unsupported_specs_have_a_reason_from_the_dictionary`, `every_spec_has_one_disposition_per_plane_and_resolves_by_name`, `docs_public_matrix_matches_ssot`, `unsupported_reasons_are_complete`; sip: `unsupported_audio_rows_keep_identity_and_reason`, `t04b_probe_tests::probe_identity_rows_reject_with_typed_reason`; admin: `audio_codec_matrix_marks_unsupported_variants_instead_of_promising_them` | caught |
| M6 | `Vp8.webrtc_negotiable = false` (answer slice) | common: `worker_and_webrtc_slices_match_dispositions`; signaling: `codec_caps_tests::proto_caps_match_ssot_webrtc_slice`; media-sfu: **compile error** `E0277` в `webrtc_answer_slice_matches_peer_builder` (`webrtc.rs:859`, `names: [&str;1]` vs `["OPUS","VP8"]`) | caught (compile-time) |
| M7 | Одна ячейка `G722` в `MEDIA_PIPELINE.md` заменена на `webrtc` | common: `docs_public_matrix_matches_ssot` — единственный fail; gateway-sip 163 pass | caught |
| M8 | `H263.sip = Native`, `reason=None`, doc-строка `native` | все 1526 продуктовых тестов **pass**; только внешняя проба `t04b_probe_status_codecs_json_matches_generated_slices` fail | **P1-2 gap** |
| M9 | `main.rs` caps += H265/G722, `RtpMap` += h265/g722 | signaling 48/48 pass; server lib 149/149 pass (2 прогона); ни один тест не заметил | **P1-1 gap** |

Дополнительно (P2-5): лишняя identity-строка PT114 `name:"G729"`,
`spec:G719`, `clock:48000` — sip **163 pass**. Дополнительно (P2-4): clock
PT104-106 = 32000 при `G7221.rtp_clock=16000` — мета-тест исключает проверку.

---

## 6. Что подтверждено и работает

- SSOT — 26 строк, `CODECS`; `NATIVE_AUDIO`/`NATIVE_VIDEO`/`WORKER_CODECS` —
  `const fn`-срезы, compatibility-консты в `codec_capabilities.rs` равны
  проекциям (`native_consts_equal_generated_slices`).
- SIP: 19 строк `AUDIO_OFFER` (7 identity `native:false`), 2 строки
  `VIDEO_OFFER` (97 H264 baseline, 99 H265), High отсутствует; `audio_row_visible`
  учитывает Native/Worker+AAC; `offer_video_enabled` — probe-гейт, не fail-open.
- Реальная негативация: все 10 отправляемых Native-строк согласуются без PT
  drift; G722 с peer PT96 матчится по name+clock; AAC: license false →
  `UnsupportedAudio{113}`, true → `MP4A-LATM`; H264/H265 probe-состояния
  управляют видео; `h246` не классифицируется как video.
- H.323: `H323_NATIVE_AUDIO_PTS = [0,4,8,9]`, `H323_VIDEO_NAMES = ["H264"]`,
  G728/OPUS/G726/MP4A-LATM не в H.323.
- WebRTC: answer slice `["OPUS","VP8"]`; VP9/AV1/H264/H265 не в answer;
  `webrtc_caps_do_not_exceed_answer`; media-sfu builder использует
  `webrtc_answer_specs()`, `unreachable!` для чужих id.
- Admin: `audio_codec_matrix`/`video_codec_matrix` — false ⇒ reason,
  true ⇒ без reason; `/api/v3/ui/system/status` `codecs` JSON == срезы
  (reviewer-проба); `transcoder=false`.
- Proto wire tags 0..8 запинены; PT в `CodecSpec` отсутствует (слабая, но
  есть проверка); license — отдельное поле, не `Disposition`.
- Doc-check: правка любой ячейки `MEDIA_PIPELINE.md` ловится ровно
  `docs_public_matrix_matches_ssot`.

---

## 7. Приёмка

| Критерий | Статус |
|---|---|
| 0 P0 | да |
| 0 P1 | **нет** (P1-1, P1-2) |
| fmt/clippy | да |
| workspace tests | 1526/0/8 в первом прогоне; повторный 1525/1 из-за timing-flake (P3-1) |
| SSOT не седьмой список | **нет**: production caps/rtp_maps — литералы (P1-1), `/license` — литералы (P2-6) |
| Публичная матрица не лжёт | **нет**: VP9/AV1 reason (P2-1), video Native без оффера (P1-2) |
| WebRTC slice не из registry | да |
| license — state, не disposition | да (но reason смешан, P2-2) |
| H.264 High RX-only | да |
| G7231 | да |
| G7221C clock | нет (P2-4) |
| 7 identity rows | да, identity+reason; но exact-one/name↔spec не enforced (P2-5) |
| proto tags/magic vec![1]/[6] | теги — да; production caps всё ещё `vec![Vp8 as i32]`/`vec![Opus as i32]` (P1-1) |

**Оценка: 8.0/10, `done: no`.**

Почему не выше: два P1 (production caps и video-native offer) — это ровно те
инварианты, ради которых делался SSOT; оба подтверждены мутациями, а не только
чтением. Почему не ниже: ядро SSOT, генерация срезов, SIP/H.323/WebRTC
негативация, AAC-гейт, admin-причины и doc-check реально работают и проверены
исполнением; критических проволочных регрессий (PT drift, потеря рабочего
кодека, новая ложная реклама High) нет.

Для `done: yes` достаточно: (1) убрать литералы caps/rtp_maps в `main` и
покрыть production-builder тестом; (2) добавить video-версию Level-3
(`Native` ⇒ строка в `VIDEO_OFFER`/negotiable); (3) закрыть P2-1/P2-2/P2-3
(текущие публичные/диагностические lie) и P2-4/P2-5 (мета-инварианты). После
этого возможен re-review на 9.5+.

---

## 8. Гигиена

- Live-дерево `/home/pro/Рабочий стол/mcu` не редактировалось, кроме этого
  файла; БД/порты/бэкапы не трогались; live-сервер не запускался.
- Все сборки/тесты — в `/dev/shm/t04b_reviewB/*`, target —
  `/home/pro/.cache/t04b_reviewB_target`; `--offline`.
- Скретч-копии и target удаляются после сдачи отчёта (в отчёте остаются только
  пути/логи как доказательства).
