# T04b — независимое ревью A: CodecSpec SSOT и матрица диспозиций

- **Ревизия:** `a02daad` (`T04b: single codec SSOT ...`), parent `15561ff`, branch `master`.
- **Метод:** pristine-копия `git archive a02daad` → `/dev/shm/rvA_t04b_1793272050_815173`
  (не live-дерево); Rust **1.94.0**; `CARGO_TARGET_DIR=/home/pro/.cache/rvA_t04b_1793272050_815173_target`;
  все сборки/тесты — `--offline`; live-дерево не редактировалось, кроме этого отчёта.
- **Итог:** **не принято (`done: no`)**; **8.5/10**.
- **Дефекты:** **P0 = 0, P1 = 1, P2 = 8, P3 = 7.**

Коротко: содержательно SSOT сделан хорошо — 26 строк, генерируемые const-fn-срезы,
неизменные proto-теги, H.264 High RX-only, причины у Unsupported, license/probe-гейт
AAC, doc-check матрицы и отсутствие дрейфа SDP/PT подтверждены исполнением.
Приёмку блокирует один P1: **production `main.rs` по-прежнему собирает codec-поля
`RoomCaps` литералами и перекрывает SSOT-производные caps**; заявленное «signaling
RoomCaps ... consume the SSOT» для боевого пути неверно. Плюс не выполнена часть
принятого дизайна про вариант `G7221C` (clock 32k) и мета-инвариант «ровно одна
строка/ветка на кодек» фактически не проверяется.

---

## 1. Счётчики, гейты и доказательства исполнением

### 1.1. Гейты

| Гейт | Команда (сокращённо) | Результат |
|---|---|---|
| fmt | `cargo fmt --all -- --check` | exit 0 |
| clippy targeted | 7 затронутых пакетов, `--all-targets -- -D warnings` | exit 0, 0 warnings |
| clippy workspace | `--workspace --all-features --all-targets --locked -- -D warnings` | exit 0, 0 warnings |
| tests affected | `-p vks-common -p vks-gateway-sip -p vks-gateway-h323 -p vks-signaling -p vks-media-sfu -p vks-admin -p vks-server` | exit 0; 0 failed |
| tests workspace | `cargo test --offline --workspace --locked --no-fail-fast` | **1526 passed, 0 failed, 8 ignored, 51 binaries**, `TEST_WS2_EXIT=0` |
| wasm32 | `cargo check -p vks-client-sdk --target wasm32-unknown-unknown --locked` | exit 0 (49.8 s) |
| doc-check | `cargo test -p vks-common --lib docs_public_matrix_matches_ssot` | pass; после подмены строки таблицы — **fail** (см. §1.3) |
| proto | diff `crates/vks-proto/proto/media.proto` parent..a02daad | пусто; тег-пин проходит |

Затронутые пакеты в чистом прогоне: `vks-common` 92, `vks-gateway-sip` 159 (+1 ignored),
`vks-gateway-h323` 46 (+1 ignored), `vks-signaling` 48, `vks-media-sfu` 98,
`vks-admin` 357, `vks-server` 149 lib (+3 ignored) + 1 `config_failclosed` + 1
`h265_rtp` + 23 `network_integration` + 7 `operator_policy` + 3 `recorder_tap`.

### 1.2. Счётчики против baseline

Заявленный baseline: **1463 passed / 8 ignored**.

Факт на `a02daad`: **1526 passed / 0 failed / 8 ignored** → **+63 passed, ignored
без изменений, failed 0**. В таблице pitfalls на `2442a16` было 1457/7 — то есть
+69 к ней, но это не дельта одной T04b: в baseline уже входили тесты T05/T09b/T06/T07.
Сама T04b добавляет **35 новых тестовых функций** и **3 заменяет** (см. ниже), т.е.
net **+32**; остальные +31 — тесты соседних задач, уже присутствующие в `a02daad`.

**Потерь тестов нет.** Три замены — не ослабление, а расширение/усиление:

| Было | Стало | Почему легитимно |
|---|---|---|
| `supported_audio_variant_tracks_the_aac_probe` | `supported_audio_variant_tracks_the_aac_probe_and_license` | добавлен license-кейс, старый probe-кейс сохранён |
| `offer_native_flags_match_shared_capabilities` | `offer_rows_match_spec` | проверка расширена на disposition/clock/видимость всех строк |
| `unavailable_h265_is_not_offered` | `unknown_and_unsupported_video_is_never_offered` | старый проверял H261 fail-open; новый запрещает H261/H263/unknown |

35 добавленных функций перечислены в diff; ключевые: `ssot_specs_have_unique_ids_names_and_aliases`,
`native_slices_are_derived_and_complete`, `webrtc_caps_do_not_exceed_answer`,
`specs_have_no_payload_type_field`, `docs_public_matrix_matches_ssot`,
`no_second_codec_name_table_outside_allowlist`, `h323_pts_match_spec_slice`,
`offer_rows_match_spec`, `sip_offer_has_no_duplicate_payload_types`,
`unsupported_audio_rows_keep_identity_and_reason`, `aac_offer_follows_worker_license_and_probe`,
`h264_high_is_rx_only_and_has_public_reason`, `codec_id_wire_tags_are_stable`,
`proto_caps_match_ssot_webrtc_slice`, `unknown_codec_does_not_land_on_video_mline`.

### 1.3. Один флейк, не регрессия

Первый прогон workspace дал **1525 passed / 1 failed / 8 ignored**:
`gateways::media_runtime_tests::gateway_audio_bridges_g726_32_at_peer_dynamic_pt_with_8khz_clock`,
timeout «G.726 PCM must reach the 48 kHz MCU mix» (`gateways.rs:5663`). Это известный
флейк media-runtime-тестов под параллельной нагрузкой (P2 из `T04b.md` §«Счётчики»).
Проверено:

- изолированно 3/3 — `ok` (0.08–0.09 s);
- `cargo test -p vks-server --lib` — **149 passed, 0 failed, 3 ignored** (20.0 s);
- повторный полный workspace — **1526/0/8**, exit 0.

В регрессию T04b не записываю, но гейт `cargo test --workspace` без изоляции/ретрая
остаётся красно-нестабильным.

### 1.4. Эксперимент с doc-check (tamper)

В pristine-копии в таблицу `MEDIA_PIPELINE.md` добавлена строка `| BOGUS | ... |`;
`docs_public_matrix_matches_ssot` упал:

```
test result: FAILED. 0 passed; 1 failed; ...
codec_spec::tests::docs_public_matrix_matches_ssot
```

После восстановления файла SHA-256 совпал с `git show a02daad:...`. Doc-check
действительно ловит редактирование таблицы, а не только наличие маркеров.

---

## 2. SSOT: что действительно едино, а что осталось вторым источником

### 2.1. Сделано правильно

`crates/vks-common/src/codec_spec.rs` — одна таблица `CODECS` (26 строк) с
`id/name/aliases/kind/rtp_clock/sample_rate/bitrate/sip/h323/webrtc/
webrtc_negotiable/license/reason` (`codec_spec.rs:187`), без payload-типов.
Срезы генерируются const-fn: `native_audio_names` (`:863`), `native_video_names`
(`:869`), `worker_codec_names` (`:875`), `webrtc_answer_names` (`:881`),
H.323-срезы (`:814`), `ALIAS_FAMILIES` (`:592`). `NATIVE_AUDIO`/`NATIVE_VIDEO`
в фасаде — ссылки на эти срезы (`codec_capabilities.rs:14,17`), а не литералы.
`normalize`/`lookup` едины и покрывают старые написания (`G.711U`, `G72616`,
`G7221C`, `G7231`, `MP4A-LATM`, `HEVC`, `telephone-event`), это подтверждено
тестом `lookup_accepts_all_previously_accepted_spellings`.

Консолидированы (проверено кодом и тестами):

- SIP-оффер: строки `AUDIO_OFFER`/`VIDEO_OFFER` несут `spec: CodecId`, видимость
  вычисляется из `spec.sip`/`is_bidirectional`; High отсутствует; `offer_video_enabled`
  исчерпывающий, без `_ => true`.
- UAS: имя кодека берётся из `codec_spec::spec(id).name` (статические PT) и
  `codec_spec::lookup(name)` (динамические 96–127), а не из своей таблицы имён.
- Server: `audio_codec_id`, `audio_sample_rate`, `bridgeable_audio_pts`,
  `H323_AUDIO`/`H323_VIDEO` выводятся из `CODECS`; `gateway_audio` диспетчеризует
  по `CodecId` из `lookup`; `gateway_aac` сравнивает с `CodecId::AacLatm`.
- H.323: `H323_NATIVE_AUDIO_PTS` генерируется из SSOT (`h323/src/lib.rs:73`).
- Signaling: `webrtc_room_caps()` строится из `webrtc_answer_specs()` (`handler.rs:50-74`);
  `sdp.rs` определяет kind через `lookup`, unknown больше не уходит в video.
- SFU: str0m answer включает только `webrtc_answer_specs()`, иные id — `unreachable!`
  (`media-sfu/src/webrtc.rs:134-150`).
- Admin: матрица поддерживаемых/причин через фасад (`api_v3/shell.rs:567-620`),
  дефолтные записи проверяются `default_codec_matrix_entries_map_to_specs`.
- License: `accepted_codecs` берёт токены из `spec.license.token()`
  (`vks-server/src/codec_policy.rs:94-101`).
- Docs: матрица между маркерами генерируется `render_markdown_matrix` и проверяется
  doc-тестом.

### 2.2. Остались независимые источники/непроверенные потребители

| Место | Что осталось | Статус |
|---|---|---|
| `vks-server/src/main.rs:365-366`, `:390` | литеральные `video_codecs=[Vp8]`, `audio_codecs=[Opus]`, затем `.with_caps()` поверх SSOT-производных | **P1**, см. §3.1 |
| `vks-server/src/main.rs:383-386` | transport-конфиг `RtpMap` 111/96 | легитимно (проволока), но не связан с SSOT |
| `vks-gateway-sip/src/uas.rs:521-530` | вторая PT→`CodecId` карта; disposition не проверяется | **P2**, §3.4 |
| `vks-server/src/gateways.rs:993-1045` | PT-карты `sip_static_pt`/`audio_codec_id`; native-фильтр + PT-карта не сверяются мета-тестом | **P2**, §3.4 |
| `vks-server/src/transcoder_runtime.rs:80-96` | 7 литеральных metric-меток | **P3**, тест проверяет свой литерал |
| `vks-core/src/media_policy.rs:71-73` | `DEFAULT_AUDIO_CODECS` с `G726` (без битрейта), `DEFAULT_VIDEO_CODECS` | **P3**, не используется, не проверяется SSOT |
| `vks-gateway-h323/src/codec.rs:84-175`, `vks-gateway-sip/src/codec.rs:78-170` | мёртвые transcode-таблицы (G729→OPUS, H264→VP9), тесты их закрепляют | **P3**, T04c |
| `vks-admin/src/settings.rs:67-90` | дефолтная матрица «намерения» | легитимно, покрыто тестом |
| `vks-proto/proto/media.proto` | wire-теги 0..8 | легитимно, пин-тест |
| `vks-admin/src/api_v3/shell.rs:288-289` | публичный JSON `native_audio` | **P2/P3**, см. §3.6 |

Итого: «одна таблица» для **исполняемой истины** в основном достигнута, но
production caps — второй литерал, а UAS/server PT-карты и metric-метки не
связаны с SSOT мета-тестами. Это ровно тот класс, который T04b должен был закрыть.

---

## 3. Находки

### P1-1. Production `RoomCaps` собирается литералами и перекрывает SSOT

`crates/vks-server/src/main.rs:365-366`:

```rust
video_codecs: vec![vks_proto::media::CodecId::Vp8 as i32],
audio_codecs: vec![vks_proto::media::CodecId::Opus as i32],
```

`main.rs:390` вызывает `.with_caps(caps)`. При этом `HandlerConfig::new` уже
установил caps из SSOT (`handler.rs:99`), а `with_caps` их заменяет. Функция
`webrtc_room_caps()` используется только внутри `handler.rs` (конструкторы и
тесты; `grep` по `crates/`+`client/` других вызовов не находит), в боевом
`main` — нет. Тест `proto_caps_match_ssot_webrtc_slice` (`handler.rs:762-780`)
проверяет именно неиспользуемую функцию, а не тот `RoomCaps`, который уходит
клиенту.

Последствие: добавление/включение WebRTC-кодека в `CODECS` (например, VP9 после
payloader) обновит `webrtc_answer_specs()` и SFU-answer, но `ServerHello.caps`
останется Opus/VP8. Это не текущий проволочный баг (сейчас значения совпадают),
но прямое нарушение принятого требования «signaling RoomCaps ints derived from
or explicitly checked against CODECS». Коммит-сообщение «signaling RoomCaps ...
consume the SSOT» для production неверно.

**Фикс:** строить codec-поля caps в `main` из `webrtc_answer_specs()` (или
экспортировать из signaling `room_caps_for(room_mode, dtls_srtp)`), оставив
динамическими `room_mode`/`dtls_srtp`; добавить тест на **production-builder**,
а не только на `webrtc_room_caps()`. Заодно `webrtc_room_caps()` сейчас
жёстко ставит `room_mode: 1` и `dtls_srtp: true`, поэтому «просто убрать
`with_caps`» нельзя — нужен параметризованный helper.

### P2-1. `G7221C` clock 32k не смоделирован в SSOT; тест это исключает

Принятый дизайн: `G7221C` — алиас `G7221` с `clock=32000`, варианты —
`(spec, clock, bitrate)`. В коде spec один и имеет `rtp_clock: 16_000`
(`codec_spec.rs:370-375`), а пять identity-строк `AUDIO_OFFER` (PT102–106)
делятся на 16k (PT102/103) и 32k (PT104–106) (`rtp_bridge.rs:310-355`). Мета-тест **явно пропускает**
проверку clock для G7221 (`rtp_bridge.rs:948-952`):

```rust
// G7221 identity rows cover both ... the spec documents the 16 kHz variant.
if codec.spec != CodecId::G7221 {
    assert_eq!(spec.rtp_clock, codec.clock, ...);
}
```

То есть 32k-вариант живёт только в transport-таблице; `lookup("G7221C")`
вернёт 16k, а инвариант «clock строки = clock spec» для всех пяти строк
отключён (для трёх 32k-строк он бы падал).
Функционально G7221/C не офферятся, поэтому это не проволочная ложь, но
пункт дизайна «G7221C variant clock» не выполнен, и мета-тест не защищает
будущие правки.

**Фикс:** либо `variants: &[(clock, bitrate)]` в `CodecSpec`, либо отдельный
`G7221C` spec; убрать исключение и проверять clock для всех строк.

### P2-2. Инвариант «ровно одна строка/ветка на кодек» не enforced

`offer_rows_match_spec` (`rtp_bridge.rs:940-975`) использует `audio_offer(...).iter().any(...)`,
т.е. проверяет «есть хотя бы одна», а не «ровно одна». Он также **не проверяет
`lookup(codec.name) == codec.spec`**: имя строки и её spec могут расходиться.
`advertised_supported_implies_negotiable` также построен на `any`.

Эксперимент (pristine, затем восстановлено): в `AUDIO_OFFER` добавлена лишняя
identity-строка

```rust
OfferCodec { payload: 114, name: "G729", clock: 48000, ... spec: CodecId::G719, ... }
```

`cargo test -p vks-gateway-sip --lib` → **159 passed, 0 failed, 1 ignored**.
Лишняя строка с чужим spec и чужим reason прошла все мета-тесты. Значит:
- дубликат identity-строки не ловится;
- несоответствие wire-имени и spec не ловится;
- «exactly one identity row» из §3.4 дизайна фактически декларация.

**Фикс:** сгруппировать строки по `spec` и требовать count==1 (для G7221 —
по варианту), проверять `lookup(codec.name).id == codec.spec`, а также
обратную полноту `VIDEO_OFFER` (каждый Native video spec присутствует).

### P2-3. `no_second_codec_name_table_outside_allowlist` слабый и неполный

Тест (`codec_spec.rs:1187-1225`) проверяет 8 файлов на подстроку `codec_spec`
и 9 точных старых needles. Он **не сканирует** `vks-server/src/main.rs`,
`vks-signaling/src/handler.rs`, `vks-media-sfu/src/webrtc.rs`,
`vks-admin/src/settings.rs`, `vks-core/src/media_policy.rs`, `gateway_video.rs`
и любые новые файлы; allowlist неявный (мёртвые карты просто не упомянуты).
Новая литеральная таблица имён в новом файле пройдёт тест. Это ровно T1-риск
из pitfalls: «SSOT может стать седьмым списком, и CI этого не заметит».

**Фикс:** сканировать `crates/**/*.rs` (и `client/**`, если нужно) на токены
имён из `CODECS`, с **явным** allowlist `(file, reason)`; либо добить
оставшиеся таблицы и проверять отсутствие литералов по всем крейтам.

### P2-4. UAS и server PT-карты не сверяются с `Disposition`

`uas.rs:521-530` — литеральная карта PT→`CodecId`; ниже берётся только
`codec_spec::spec(id).name`, но **не проверяется `spec.sip`**. Аналогично
`gateways.rs:993-1008` фильтрует `CODECS` по `Native`, но `sip_static_pt`
(`:1013`) и `audio_codec_id` (`:1030`) — отдельные литеральные карты.

Если в SSOT поменять `G728.sip` на `Unsupported`, оффер и `bridgeable_audio_pts`
перестанут включать PT15, но UAS всё равно ответит на PT15 как на G728.
Ни один тест не связывает эти карты с disposition. Это не текущий баг
(G728 Native), но «все потребители derived/checked» не выполнено.

**Фикс:** в UAS фильтровать по `spec.sip == Native` (кроме DTMF/AAC-спецслучая);
мета-тест: каждый static PT, который UAS принимает, соответствует Native,
и каждый Native audio spec присутствует ровно в одной PT-карте.

### P2-5. VP9/AV1 в публичной матрице: «WebRTC —», но reason `webrtc_only`

`CodecSpec` для VP9/AV1: `webrtc: Unsupported(PendingPayloader)`, но
`reason: Some(WebRtcOnly)` (`codec_spec.rs:477-505`). `render_markdown_matrix`
берёт `spec.reason` для любой не-Native плоскости (`codec_spec.rs:900-907`),
поэтому `MEDIA_PIPELINE.md:198-199` показывает:

```
| VP9 | video | — | — | — | webrtc_only |
| AV1 | video | — | — | — | webrtc_only |
```

Публичная матрица утверждает «только WebRTC», хотя WebRTC-колонка — «—»
(нет payloader). Для VP8 reason `webrtc_only` корректен (WebRTC-колонка `webrtc`),
для VP9/AV1 — нет. Truthful-matrix пункт приёмки для этих двух строк не выполнен.

**Фикс:** для VP9/AV1 `reason = PendingPayloader` (или reason на плоскость),
рендерить причину для конкретной недоступной плоскости.

### P2-6. Публичный `NATIVE_AUDIO` изменился по составу и содержит дубликаты

Старый список — 13 имён (`PCMU,PCMA,G711,G711U,G711A,G722,G72616..40,G728,G723,OPUS`),
новый — 18: добавлены канонические `G726-16..40`, `G7231`, `G711` переехал в конец;
`G726-16` и `G72616` теперь оба в списке. `/api/v3/net/status.codecs.native_audio`
отдаёт именно `NATIVE_AUDIO` (`api_v3/shell.rs:288`), тест-пина на содержимое нет
(есть только сравнение с generated-срезом, `codec_capabilities.rs:319-324`).

Коммит говорит «public API unchanged» — это верно для типа/сигнатуры, но не для
значений: UI может показать 8 строк G726 вместо 4. Добавление `G7231` полезно,
дубликаты — шум; контракт стоит зафиксировать осознанно.

**Фикс:** решить, что отдаёт публичный JSON (канонические имена), алиасы оставить
только для `is_*`/`lookup`; добавить snapshot-тест публичного ответа.

### P2-7. Причина AAC всегда `license_not_accepted`, даже когда виноват probe/pool

`audio_unavailable_reason` (`codec_capabilities.rs:149-158`) для любого Worker-спека
возвращает `license_not_accepted`. В `main` AAC включается только при
license **и** probe **и** pool≥2 (`main.rs:408-420`), а глобальный флаг один.
Если оператор лицензию принял, но FFmpeg-проба не прошла или pool<2, админка
всё равно пишет `license_not_accepted` — неверная машиночитаемая причина.
Там же `G726:20` даёт `unknown_codec`, хотя это known codec с неподдержанным
bitrate (лучше `unsupported_bitrate`).

**Фикс:** хранить/пробрасывать раздельные состояния (license/probe/pool) или
возвращать `probe_unavailable`/`worker_pool_unavailable`; для G726 — отдельный
reason.

### P2-8. Нестабильный media-runtime тест в workspace-гейте (известный флейк)

См. §1.3: первый прогон — `148 passed; 1 failed; 3 ignored` в `vks-server --lib`,
падение `gateway_audio_bridges_g726_32_at_peer_dynamic_pt_with_8khz_clock`
(`gateways.rs:5663`, timeout 3 s). Изолированно и в полном `-p vks-server --lib`
— зелено; повторный workspace — 1526/0/8. Это не регрессия T04b и не должно
блокировать приёмку, но CI-гейт с параллельными тестами остаётся флейковым;
для приёмки нужен ретрай или `--test-threads=1` для media-runtime.

### P3-находки

1. **`is_native_audio_variant("G711", Some(bitrate))` → false.** В
   `codec_capabilities.rs:93-100` fallback для alias family требует
   `bitrate.is_none()`. Старый код игнорировал bitrate. `PATCH` настройки
   `G711:8` теперь покажет «не поддержан». Семейство G711 не имеет bitrate,
   но это изменение поведения; нужен тест/явное решение.
2. **Порядок `H323_NATIVE_AUDIO_PTS` изменился** с `[0,4,8,9]` на `[0,8,9,4]`
   (`h323/src/lib.rs:73`); тест проверяет только отсортированное множество
   (`:101-103`). На проволоке это может изменить порядок предпочтения sidecar;
   множество не изменилось. Зафиксировать порядок тестом или сделать
   сортировку явной.
3. **Мёртвые transcode-таблицы остались**: `h323/src/codec.rs:84-175`,
   `sip/src/codec.rs:78-170` с тестами, закрепляющими G729→OPUS/H264→VP9.
   Они исключены из `no_second_...` неявно; до T04c это принято, но в отчёте
   должно быть явно.
4. **`vks-core/src/media_policy.rs:71`** содержит `G726` без битрейта —
   `lookup("G726")` = None; список не используется и не проверяется SSOT.
   Либо удалить, либо привести к `G726:32`/вариантам.
5. **`worker_metric_labels_are_subset_of_specs`** (`transcoder_runtime.rs:136-142`)
   перебирает литерал в тесте, а не реальные метки `publish_support`
   (`:80-96`); добавление/удаление метки в проде тест не заметит. Сделать
   `publish_support` источником списка или вынести константу.
6. **UAS `payload_is` сравнивает wire-имя с каноническим** (`uas.rs:407-421`),
   поэтому `a=rtpmap:4 G7231/8000` отвергается, хотя `lookup("G7231")` — Native
   и задача требует «G7231 accepted». Для PT4 без rtpmap работает, с алиасом —
   нет; несогласованность статических и динамических PT (динамические идут
   через `lookup`). Приводить имя через `lookup(...).name` или сравнивать id.
7. **`proto_caps_match_ssot_webrtc_slice`** (`handler.rs:762-780`) для audio
   ожидает ровно `[Opus]`; `webrtc_room_caps` при неизвестном audio-id делает
   `continue`, поэтому новый negotiable audio-кодек будет молча пропущен,
   а тест останется зелёным. Добавить `panic!`/явное покрытие, как для video.

---

## 4. Truthfulness по кодекам (выборочная проверка)

| Кодек | SSOT | SIP offer | H.323 | WebRTC | Админка / docs | Вердикт |
|---|---|---|---|---|---|---|
| PCMU/PCMA/G722/G723/G726-16..40/G728/OPUS | Native | да (PT 0,8,9,4,107-110,15,111) | PCMU/PCMA/G722/G723 | Opus — webrtc | supported | ок |
| MP4A-LATM/AAC | Worker, license `aac-latm` | только при probe+license; PT113 | нет | нет | false + `license_not_accepted` | ок (причина спорна, P2-7) |
| G729 | Unsupported(`encoder_not_bit_exact`) | identity PT18, не отправляется | нет | нет | reason ok | ок |
| G7221/G7221C | Unsupported(`no_implementation`) | identity PT102-106 (16k/32k) | нет | нет | reason ok | clock-вариант P2-1 |
| G719 | Unsupported(`no_implementation`) | identity PT112 | нет | нет | reason ok | ок |
| ILBC | Unsupported(`no_implementation`) | нет строки (не офферится) | нет | нет | reason ok | ок |
| H264 | Native | PT97 | PT97 | нет | supported | ок |
| H264-HIGH | RxOnly(`tx_profile_unsupported`) | нет; RX принимает High | нет | нет | rx-only | ок |
| H265 | Native (probe-гейт) | PT99 при probe | нет | нет | supported | ок |
| VP8 | SIP Unsupported(`webrtc_only`), WebRTC WebRtc | нет | нет | answer | reason webrtc_only | ок |
| VP9/AV1 | WebRTC Unsupported(`pending_payloader`), reason `webrtc_only` | нет | нет | нет | docs: «—», reason webrtc_only | **P2-5** |
| H261/H263/H263+/H264-SVC | Unsupported | нет | нет | нет | reason | ок (SVC reason per-plane, P3) |
| telephone-event | Dtmf/Native | PT101 | нет | нет | native | ок (спец-исключение) |
| G711 (family) | AliasFamily PCMU+PCMA, `offerable=false` | по имени не офферится | — | — | supported (intent) | ок |

Ключевые положительные факты:

- `build_offer` Native/Full не содержит H264-HIGH и G729/G7221/G719; `Full`
  не расширяет видео на H261/H263; unknown video не проходит
  (`offer_video_enabled` исчерпывающий).
- H264/H265 probe-флаги по-прежнему гейтят оффер; AAC — probe+license+pool.
- G711-семейство не стало отдельным `CodecSpec` и не офферится по имени.
- G7231 — алиас G723, Native в SIP/H.323; H.323 PT-набор `[0,8,9,4]`.
- Причины Unsupported заполнены и проверены словарём; identity-строки
  G729/G7221/G719 сохранены с reason (PT18, 102-106, 112).
- `render_markdown_matrix` и docs совпадают; tamper-тест падает.
- `MEDIA_PIPELINE.md` больше не содержит старой ложной таблицы.

---

## 5. Proto, probe, SDP/PT-регрессия

### 5.1. Proto

`crates/vks-proto/proto/media.proto` в diff отсутствует. Тег-пин
`codec_id_wire_tags_are_stable` (`vks-proto/src/lib.rs:213-230`) проверяет
`Unspecified=0, Vp8=1, Vp9=2, H264=3, H265=4, Av1=5, Opus=6, G722=7, G711=8`.
`CodecId` SSOT — отдельный enum, не proto-тег; signaling/SFU маппят id явно.
`Unspecified` не попадает в caps (`unspecified_is_never_advertised`).
Замечаний нет.

### 5.2. Probe

`H264_AVAILABLE`/`H265_AVAILABLE`/`AAC_LATM_AVAILABLE` — process-global atomics;
`video_codec_available` для unknown возвращает false; `is_native_video` для
H261/H263/High — false; `offer_video_enabled` не имеет fail-open. `main`
выставляет video-флаги по `video_bridge_availability()` (`main.rs:399-401`),
AAC — по license+pool+probe (`main.rs:408-420`). Тесты с `_with`-хелперами
не гоняются на глобалах. Замечаний по fail-open нет.

### 5.3. SDP/PT

- Золотая строка native-оффера не изменилась:
  `m=audio 41000 RTP/AVP 0 8 9 15 4 107 108 109 110 111 101`,
  `m=video RTP/AVP 97 99`.
- Порядок/номера PT не менялись; `sip_offer_has_no_duplicate_payload_types` зелёный.
- UAS static PT 0/8/9/15/4/107-110 ведёт себя как раньше; динамические PT — через
  `lookup`, PT2 G726-32 — только с rtpmap.
- `sdp.rs` больше не классифицирует неизвестное имя как video; тест
  `unknown_codec_does_not_land_on_video_mline` зелёный.
- SFU answer по-прежнему Opus+VP8; тест `webrtc_answer_slice_matches_peer_builder`
  проверяет отсутствие VP9/AV1/H264/H265.
- H.323: набор PT тот же, порядок `[0,8,9,4]` (P3); sidecar-тест только set.

Регрессии SDP/PT не обнаружено.

---

## 6. Гигиена

- Live-дерево `/home/pro/Рабочий стол/mcu` не редактировалось; создан только
  этот отчёт. Все эксперименты — в `/dev/shm/rvA_t04b_1793272050_815173`.
- После tamper и mutation SHA-256 `rtp_bridge.rs` и `MEDIA_PIPELINE.md` совпали
  с `git show a02daad:...`; `BOGUS`/`payload: 114` отсутствуют.
- Живые БД, порты, бэкапы, systemd-юниты не трогались; сервер не перезапускался.
- В `/tmp` ничего не создавалось; target-каталог отдельный.

---

## 7. Оценка

**8.5/10, `done: no`.**

Почему не ниже: ядро SSOT, генерируемые срезы, proto-пин, probe/license-гейты,
H.264 High, причины Unsupported, doc-check и отсутствие дрейфа оффера/PT
подтверждены и работают; 1526 тестов зелёные, clippy/fmt/wasm чистые.

Почему не 9.5: production `RoomCaps` — второй литерал (P1), `G7221C`-вариант
не смоделирован и исключён из проверки (P2), инвариант «ровно одна строка/ветка»
не enforced (P2, доказано инъекцией), а «no second table» не покрывает новые
файлы и оставшиеся карты (P2). Для `done: yes` достаточно закрыть P1 и
P2-1/P2-2/P2-3; остальные P2/P3 можно вынести в T04c/T20.

**Рекомендация Lead:** не принимать T04b как завершённую; вернуть на короткий
fix-up: (1) производные codec-caps в `main` + тест production-builder;
(2) вариант G7221C в SSOT и убрать clock-исключение; (3) усилить
`offer_rows_match_spec` на уникальность и `lookup(name)==spec`; (4) расширить
`no_second_...`/явный allowlist. После этого — повторное ревью, ожидаемая
оценка ≥9.0 без P1.
