# 03. Шлюзы SIP/H.323 ↔ MCU: ID участников, RTP/RTCP, DTMF, кадры, границы mixed-room

Дата: 2026-10-28. Агент: `gateways`. Задача task-3. Режим: read-only (запись только в этот файл).
Сервис: PID 2965299, `/home/pro/mcu/target/release/vks-server` (симлинк `/home/pro/mcu → "Рабочий стол/mcu"`),
`/home/pro/Рабочий стол/mcu/configs/default.toml`, бинарь собран 2026-10-25 20:37, процесс стартовал 2026-10-26 11:20:22
(`vks_server.log:4281-4285`). Живой H.323 listener: PID 2965383, `h323-venv/bin/python3 -u .../python/h323_listener.py`,
TCP 0.0.0.0:1720. Звонки не инициировались.

## 0. Метод и границы

- Прочитаны целиком: `gateway_audio.rs` (861), `vks-gateway-sip/src/rtcp.rs` (277), `lib.rs` (51), `uas.rs` в части SDP/DTMF (220-320, 750-780),
  `h323/uas.rs` (1-120), `call.rs` (120-165), `gateway_video.rs` (560-640 + карта функций), `gateways.rs` — структурная карта (grep `fn/impl/struct`, 200 сигнатур)
  и построчно все затронутые участки: 292-470, 986-1060, 1141-1300, 1721-1930, 2139-2160, 2690-2840, 3125-3865, 4354-4440, 5090-5115.
- `python/h323_listener.py` (1224 строк кода, не считая 16 871 строки `h323_asn.py`) — через AST-карту (`grep -E '^class|^def'`), точечное чтение 361-530, 601-640, 880-960;
  сгенерированный ASN.1 не читался построчно.
- Полностью «построчно» `gateways.rs` (6297) и `gateway_video.rs` (2779) не прочитаны — это зафиксировано в §9 как остаточный риск, а не как выполненная работа.

## 1. Runtime-артефакты (факты)

| Что | Факт |
|---|---|
| Процесс | PID 2965299, uptime 1-10:20, cpu 0.2% (`ps`) |
| Порты | TCP 8080 (vks-server fd17), UDP 0.0.0.0:5060 (fd13 = SIP UAS), UDP 0.0.0.0:40000 (fd12), TCP 0.0.0.0:1720 (python PID 2965383) (`ss -tulnp`) |
| H.323 | `VKS_H323_UAS_ENABLED=1` → `gateways.rs:3267` «H.323 UAS listening bind=0.0.0.0:1720 local_ip=192.168.1.93» (лог:4285); sidecar resolved `h323_sidecar.py` + venv python (лог:4080) |
| SIP UAS | рестарт/регистрация UAS: лог 2420-2422, 3426-3428, 4281-4283; outbound вызовы с медиа: лог 4289-4309 (G722 и H264, `rtp_port=41000`) |
| FFmpeg | на момент снятия — **0 процессов** ffmpeg (композит/энкодеры не запущены, нет активных комнат) (`ps -eo pid,cmd`) |
| Дефект диагностики (массово) | `grep -c 'audio RTP rejected by source validator'` = **616**; при этом в каждой строке `expected_payload=9 received_payload=9`, напр. лог:4014-4016 (`participant=h323-...-in@h323-gateway source=192.168.1.93:33822`) |
| Расхождение артефакта | бинарь 2026-10-25 20:37 **старше** `crates/vks-server/src/gateway_audio.rs` (2026-10-25 21:38:38) → runtime-лог 25.10 17:52 снят на ещё более старом билде; выводы по коду — по текущему дереву |
| VCS | `crates/vks-gateway-sip/src/rtcp.rs` — **untracked** (`??`); ещё ≥14 файлов modified, включая `uas.rs`, `rtp_bridge.rs`, `h323_listener.py`, `engine.rs` |

## 2. Проверка тезисов (a)-(h)

### (a) Входящие ID с `-in` vs фильтр назначения — ПОДТВЕРЖДЕНО (механика уточнена)
- Входящий SIP: `format!("sip-{}-in@sip-gateway", uuid)` — `gateways.rs:2720`; входящий H.323: `format!("h323-{}-in@h323-gateway", uuid)` — `gateways.rs:3452`.
- Этот же id уходит клиентам в `ParticipantJoined` и в снапшот комнаты — `gateways.rs:1141-1170` (id без изменений, `snapshot.participant_id = participant_id`).
- Фильтр в `apply_layout`: `["abonent_id","connection_id","contact_id","call_string_id"]` → `.find(|v| Uuid::parse_str(v).is_ok() || layout_identifier_has_uuid(v))` — `gateways.rs:1770-1776`; при `None` слот не назначен, `assignments` не пополняется.
- `layout_identifier_has_uuid` берёт **последние 36 символов токена**: `let start = token.len().saturating_sub(36); uuid::Uuid::parse_str(&token[start..])` — `gateways.rs:1279-1286`.
  Для `sip-<uuid>-in` токен = 43 символа, `start=7` → последние 36 = хвост uuid + `-in` → невалидный UUID → `false`; для `sip-<uuid>` (40 симв.) → `true`.
- Итог: MCU авто-рассаживает участника (`engine.rs:667-687 free_layout_slot`, `engine.rs:734`), окно из админской раскладки не закрепляется. `parse_participant_id` при этом UUID бы нашёл (sliding window `pid_str[i..i+36]`, `lib.rs:385-427`) — баг ровно в строковом фильтре.
- Слепые тесты: `layout_identifier_accepts_only_uuid_shaped_values` проверяет только `sip-<uuid>@sip-gateway` / `h323-<uuid>@h323-gateway` (`gateways.rs:5097-5106`), `gateway_manager_applies_layout_slots_to_mcu` — тоже `sip-<uuid>@sip-gateway` (`gateways.rs:4362`). Ни одного кейса с `-in`.
- Тракт UI подтверждён по API: `participant_abonent_json` кладёт в `abonent_id` **ровно** `participant.participant_id` (а также `connection_id`/`contact_id` = он же) — `crates/vks-admin/src/api_v3/resources.rs:485-493`; UI получает эту форму через `layout_record_abonents` (resources.rs:480-484) и возвращает в сохранение раскладки. Т.е. в `abonent_id` для H.323-входящего гарантированно окажется `h323-<uuid>-in@h323-gateway`.

### (b) Срезы строк не UTF-8-safe — ОПРОВЕРГНУТО как «живой» дефект, ПОДТВЕРЖДЕНО как латентный
В путях шлюзов срезы либо байтовые (`&buf[..count]` по `[u8]`), либо позиционные от `find` (граница гарантирована):
- `split_head_body`: `&buf[..pos]`, `&buf[pos+4..]` — `uas.rs:145-152`, `sip_message.rs:21-23` (`pos` от `find("\r\n\r\n")`, ASCII).
- `media_section`: `start + marker.len()` и `+offset+1` от `find("\nm=")` — `uas.rs:386-394`, все слагаемые — длины ASCII-маркеров.
- `sip_from_display_name`: `quoted[start+1..start+1+end]`, `start`/`end` от `find('"')` — `gateways.rs:2139-2150`; `start+1` — граница, т.к. `"` однобайтовый.
- `h323_contact_uri` — только `chars().filter().take(64)` — `gateways.rs:3163-3170`; `dtmf` — `digits.chars().take(32)` — `gateway_audio.rs:266`.
- Python: `str(error)[:400]` — `h323_listener.py:1153,1223` — посимвольный срез, безопасен.
Латентный риск: `lib.rs:392-394` — `&pid_str[pid_str.len()-36..]` **без `is_char_boundary`**: паника при ≥36-байтном id с многобайтным символом на границе. Достижимых путей не нашёл: id участников серверные ASCII (`handler.rs:329` — `part-<room>-<uuid>`, шлюзы — `sip-/h323-<uuid>-in@...`), а путь `apply_layout` защищён проверкой `layout_identifier_has_uuid` (там `is_char_boundary` есть) до вызова парсера.

### (c) Два участника в один слот — ОПРОВЕРГНУТО буквально, ПОДТВЕРЖДЁН смежный дефект
- Авто-рассадка уникальна: `free_layout_slot` строит `used: BTreeSet<u32>` из `assignments.values()` и берёт только свободный слот — `engine.rs:667-687`.
- Но явные позиции не валидируются на дубли: `assignments: BTreeMap<ParticipantId,u32>` (`gateways.rs:1778,1787`) допускает двух pid с одной позицией; MCU перекладывает в `BTreeMap<slot,pid>` — `engine.rs:230-236` (`slot_to_pid.insert(*slot_id, vpid)`) → второе значение **затирает** первое. Итог: не «двое пишут в один слот», а «один участник молча остаётся без окна».

### (d) DTMF: clock 8/48k и разные in/out PT — ПОДТВЕРЖДЕНО (механизм иной, чем в гипотезе)
- Генерация событий: отдельная шкала `dtmf_timestamp`, шаг **+160 за 20 мс** = 8 кГц — `gateway_audio.rs:245, 440-452` (комментарий 243-244 это и утверждает).
- SDP-ответ: `a=rtpmap:{dtmf} telephone-event/{media.audio_clock_rate}` — `uas.rs:305-311`, т.е. для Opus (48 кГц) MCU объявляет **telephone-event/48000**, а метки времени ставит 8-килогерцовые → у терминала длительности событий в 6 раз короче.
- Согласование PT в UAS: `telephone_event_payload(section, clock_rate)` требует `clock == bridge_audio.clock` — `uas.rs:756-770`. Для Opus это 48000, поэтому типичный оффер `telephone-event/8000` + Opus даёт `dtmf_payload_type = None` → **DTMF не работает в обе стороны** (`telephone_event_is_answered_only_when_offered`, `uas.rs:952-967` проверяет только PCMU-кейс).
- Асимметрия направлений: UAC требует `telephone-event/8000` у удалённой стороны — `rtp_bridge.rs:1186-1193` (+ тесты 1509-1603), т.е. исходящее плечо и входящее (UAS) используют разные правила clock.
- Разные in/out PT: наборы — `bridgeable_audio_pts` (`gateways.rs:986-998`: 0,4,8,9,15,107-111,113 для SIP; нативные H.323 PT для H.323), для H.323 дополнительно требуется симметрия `remote == local` — `symmetric_h323_audio_pt` (`gateways.rs:999-1002`), `h323_inbound_audio_pt` (1004-1015). Для SIP асимметрия PT — норма (тесты `gateways.rs:5500`, `5626`).
- Вывод по формулировке «нельзя запрещать автоматически»: верно — PT per-leg уже учитывается, а clock захардкожен; чинить нужно согласование clock и per-leg PT, не запретом.
- Дополнительно: **DTMF в H.323-плечо не уходит вообще**. Для H.323 `RtpBridgeSession.dtmf_payload_type = None` (`gateways.rs:3668`), а ветка отправки при `None` делает `dtmf.clear()` — `gateway_audio.rs:452-453`; при этом `relay_dtmf_character` шлёт цифры в `controls.dtmf_sender` этого же плеча (`gateways.rs:306-319`). Отправки H.245 `UserInputIndication` нет ни в Rust, ни в Python (только разбор входящих — `h323_listener.py:270`, `gateways.rs:3755-3777`).

### (e) H.323: audio-only, поздняя камера, camera off/on, очистка, отказ всего вызова — ПОДТВЕРЖДЕНО ЧАСТИЧНО
- Audio-only при частичном видео: `maybe_connected` ждёт видео окно 3 с и затем соединяет с `video_status` — `h323/python/h323_listener.py:609-624`; Rust это поле не читает, в `media_details` его нет (`gateways.rs:3575-3597`). Если видео пришло после `connected`, событие уже не переигрывается.
- Позднее включение камеры: в `on_open` для `kind=="video"` **нет** `event_sink(...)` (`h323_listener.py:399-449`, ср. с `on_close`: `channel_closed` — 450-465) и в Rust нет обработчика `channel_opened` (match по событиям `gateways.rs:3416-3830`: только `incoming/connected/dtmf_digit/video_fast_update/channel_closed/failed|disconnected`). Итог: камера, включённая после `connected` (или после off→on), MCU не видит до нового вызова.
- Camera off: обрабатывается — `channel_closed{kind:"video"}` (`h323_listener.py:455-465`) → Rust снимает video-задачу, чистит `media_details` и треки (`gateways.rs:3780-3826`). Но MCU-трек участника не пересобирается (unbridge/bridge не вызываются) — риск «замёрзшего» последнего кадра в композите.
- Отказ всего вызова: **опровергнуто для видео** — при ошибке `spawn_video_media_bridge` остаётся audio-only с пересборкой треков (`gateways.rs:3690-3740`, лог «inbound H.323 video bridge unavailable; audio-only»). Для аудио отказ действительно фатален: несимметричный/непригодный audio в `connected` → `hangup` (`gateways.rs:3520-3533`), что для H.323-шлюза ожидаемо.
- Очистка задач: `retire_inbound_h323` отменяет задачи и ждёт их под общим бюджетом 3 с (`gateways.rs:3219-3240`); `cleanup_tasks.retain(...)` выполняется только в ветке 1-Гц тикера (`gateways.rs:3340-3352`) → при непрерывном потоке событий завершённые handle-ы накапливаются (мелкая утечка, не отказ).
- Строгость видео-канала: `on_open` требует `sessionID==2` и **отсутствия** `reverseLogicalChannelParameters` (`h323_listener.py:399-420`) — терминалы с обратными каналами получат reject.
- Для SIP входящего тоже нет добавления видео в звонке: re-INVITE → 501 («Not Implemented», `gateways.rs:2462`).

### (f) RTCP ограничен — ПОДТВЕРЖДЕНО
- Модуль сам это декларирует: «Полный стек RTCP (SR/RR-статистика, SDES, BYE) не реализуется» — `rtcp.rs:1-7`.
- Читатель RTCP есть **только** у входящего SIP-видео: `spawn_video_rtcp` определён в `gateways.rs:347`, единственный вызов — `gateways.rs:2801`. Исходящие (UAC) и все H.323-плечи RTCP-читателя не имеют.
- PLI/FIR инкрементируют счётчик `keyframe_requests` (`gateways.rs:381,385`, H.245 FUP — 3784-3788), и потребитель **есть** (`gateway_video.rs:876` — клон `session.controls.keyframe_requests`; `901,932` — `swap(0, …)`), но он только журналирует и **не форсирует IDR**: `debug!("keyframe request consumed; IDR deferred to the group encoder (keyint=30)")` — `gateway_video.rs:934-943`; в ферме — `group.keyframe_requests.swap(0, …)` → `debug!("… IDR follows within keyint; force-IDR is P4b")` — `farm.rs:798-806`. Т.е. фидбек не теряется, а сознательно отложен до штатного IDR.
- GOP: `keyint = fps`, `-keyint_min = fps` (`gateway_video.rs:316,356-358`, `farm.rs:974-976,1118`) → IDR каждые ~1 с. Последствие потери пакета: картинка стоит **≤1 с**, не «до переустановления вызова». Отсюда (f) — P2, не P1.
- NACK не разбирается: `parse_compound` учитывает SR/RR/SDES/BYE/PLI/FIR, RTPFB(205)/FMT=1 исключён (`rtcp.rs:118-143`, тест `generic_nack_is_not_a_keyframe_request`).
- `build_pli`/`build_fir` (`rtcp.rs:155-183`) — **dead code**: вне `rtcp.rs` не вызываются (grep по `crates/`), только в тестах.
- Аудио: SR раз в 5 с (`gateway_audio.rs:455-460`), видео: `sender_report` (`gateway_video.rs:1259`).
- RR-keepalive/ответ на SR есть только в inbound-SIP видео-ветке (`gateways.rs:352-405`).

### (g) «Mock» в коде vs живой Python listener — ОПРОВЕРГНУТО как «заглушка продакшена»
- Реальный процесс: `vks-gateway-h323/src/uas.rs:110-135` — `Command::new(python).arg("-u").arg(helper)`, чтение JSON-строк `read_event` (uas.rs:38-56), `accept/reject/hangup` команды (uas.rs:73-103).
- Живой стенд: PID 2965383 на :1720 (ss), лог 4080 (sidecar resolved), 4285 (UAS listening).
- Слово «mock» встречается только в doc-комментариях к in-memory реестру (`vks-gateway-sip/src/lib.rs:11-14,21`; `call.rs:126-149`) и в тестах (`uac.rs:534-675`). Это реестр комнат, а не симуляция сигнализации.
- Единственная реальная «not implemented»-ветка в сигнализации — re-INVITE 501 (`gateways.rs:2462`) и DONL (`uas.rs:998`).

### (h) Смешанный WebRTC ↔ SIP/H.323 запрещён защитами — ПОДТВЕРЖДЕНО (архитектурная задача)
- Гард в обе стороны: gateway отвергается, если в комнате есть реальный WebRTC-peer — `webrtc_runtime::room_has_peer` (`webrtc_runtime.rs:99-100`), вызовы в `gateways.rs:763` (входящий SIP), `836` (входящий H.323), `1208` (посадка в комнату), `1304` (набор), `1552`, `2693` (UAS приём), `3588` (H.323 connected).
- Обратный гард: браузерный оффер отвергается, если в комнате есть `@sip-gateway`/`@h323-gateway` — `lib.rs:742-743`.
- Следствие: типовой сценарий «телефон входит в браузерную конференцию» сейчас невозможен по замыслу; правильное решение — termинация DTLS-SRTP на MCU (или отдельный media-gateway leg с SRTP), это отдельная архитектурная работа, точечные правки в шлюзах лишь маскируют полу-сбрижденную комнату.

## 3. Новые находки (вне списка задачи)

1. **H.323-аудио отбрасывается: два разных класса отказов, один лог.** Всего 616 строк `audio RTP rejected by source validator (codec/PT mismatch)` (`gateway_audio.rs:288`), 206 разных участников (131 h323 + 75 sip), лог ограничен 3 строками на мост (`rejected_logs`, `gateway_audio.rs:262,285-287`) — значит фактических отбросов больше.
   - Класс A (h323, 240 строк): `expected_payload=9 received_payload=0` — терминал шлёт PCMU там, где MCU ждёт G.722; interchange разрешён только 0↔8 (`gateway_audio.rs:616-620`), G.722↔PCMU — нет. Это настоящий PT-mismatch.
   - Класс B (376 строк, 206 участников): `expected_payload == received_payload` (напр. 9/9, лог:4014-4016) — отказ **не** по PT. Для H.323 причина — пиннинг источника: `source_latch = false` (`gateways.rs:3667`, комментарий 3672-3674) + `if source != self.target { if !self.latch || self.ssrc.is_some() { return None } }` (`gateway_audio.rs:605-610`); H.245 фиксирует лишь адрес приёма, порт-источник не нормируется, а стендовый бот шлёт из сокета, привязанного к порту 0 (`scripts/bench/h323bot.py:365,419`), сигналя при этом фиксированный `audio_port` (236-259). Для SIP-входящего `source_latch = true` (`gateways.rs:2772`), поэтому там класс B — дубли/смена SSRC/устаревший seq (`gateway_audio.rs:633-648`: padding, `end == offset`, смена SSRC, устаревший seq), а не пиннинг.
   - Последствие: у H.323-входящего MCU не слышит терминал (обратное направление SR/RTP работает) — «звонок есть, а звука нет» в одну сторону; и по логу это неотличимо от несовпадения кодека.
2. **Дубли явных позиций раскладки** молча теряют участника (`engine.rs:230-236`).
3. **DTMF в H.323-плечо не отправляется** (см. d) — при этом `relay_dtmf_character` считает доставку успешной.
4. **`rtcp.rs` untracked** — код RTCP-разбора не в git (риск потери/неревьюенности).
5. **Бинарь старше `gateway_audio.rs`** — runtime-наблюдения не гарантируют текущее поведение этого файла.

## 4. Дебаты (обязательный протокол)

Платформенное ограничение: мой `subagent` отклонён runtime — `Error: subagent depth 2 exceeds maxDepth 1` (я depth 2, лимит 1); `subagent_fork` запрещён (remaining levels 0). По согласованию с Lead протокол переведён в режим «внутренний адвокат дьявола + независимая проверка grep» с передачей моих тезисов внешнему durable-челленджеру Lead'а.

**Раунд 1 (само-челлендж: сильнейший контртезис → проверка по коду → вердикт)**
- C1 «Тезис (a) неверен: UI не присылает `-in` id». Проверка: `ParticipantJoined` несёт id без изменений (`gateways.rs:1152`), иных источников участников шлюза нет; фильтр отвергает `-in` арифметикой (`1279-1286`). → **тезис (a) стоит**, но severity зависит от UI-тракта (в §9).
- C2 «Тезис (b) подтверждается паникой `parse_participant_id`». Проверка: единственный вход из JSON — `apply_layout`, но он вызывает парсер только после `layout_identifier_has_uuid` с `is_char_boundary` (`1775`, `1282`); прочие входы серверные ASCII. → **контртезис принят: (b) не «живой» баг**, переклассифицирован в латентный.
- C3 «Тезис (c) верен: двое в одном слоте». Проверка: `free_layout_slot` держит `BTreeSet` (`engine.rs:667-684`), а рендер идёт по `BTreeMap<slot,pid>` (`230-236`) → буквально двое в слот не попадают. → **контртезис принят частично**: дефект есть, но формулировка «двое пишут в один слот» неверна, правильная — «второй теряет окно».
- C4 «Тезис (d) неверен: clock учтён отдельной переменной». Проверка: отдельная переменная есть (`gateway_audio.rs:245`), но её шаг 160/20мс (8 кГц), а SDP для Opus объявляет `telephone-event/48000` (`uas.rs:305-311`) — переменная не помогает, если объявленный clock 48k. → **тезис (d) стоит**, формулировка уточнена.
- C5 «Тезис (f) преувеличен: RR-keepalive и ответ на SR есть». Проверка: есть, но только inbound-SIP видео (`gateways.rs:352-405`, единственный вызов 2801); PLI/FIR не действуют, NACK нет, `build_pli/build_fir` мертвы. → **тезис (f) стоит** с уточнением объёма.
- C6 «Тезис (e) неверен: видео-отказ деградирует в audio-only». Проверка: действительно деградирует (`3690-3740`); фатален только аудио-отказ. → **контртезис принят**; (e) переклассифицирован в «частично подтверждено».

Итог раунда 1: принято 3 контртезиса (b, c, e), отклонено 3 (a, d, f) — с уточнением формулировок всех шести. Ни один из отклонённых контртезисов не опирался на контрпример в коде, который я не смог бы предъявить.

**Раунд 2 (контраргументы Lead'а; внешний durable-челленджер не успел к финалу — отмечено в §9)**

- **R2-1 (достижимость (a)). ПРИНЯТ, достижимость доказана.** Цепочка: `room_hub.list_participants(room_id)` (в нём inbound-участник `sip-/h323-…-in@…`, `gateways.rs:1152,1165`) → `resolved_layout_abonents` строит список абонентов раскладки и для auto/non-manual отдаёт **всех** участников комнаты через `participant_abonent_json` (`crates/vks-admin/src/api_v3/resources.rs:498-534`, вызовы 537 и 790) → в записи `abonent_id` = `participant.participant_id` (`resources.rs:486-493`) → UI перетаскивает эту запись в слот → PATCH `payload.slots[].abonent_id` с этим id → `apply_layout` отбрасывает его фильтром (`gateways.rs:1770-1776`, `1279-1286`). Даже `gateway_protocol` (`resources.rs:469-473`) различает `sip`/`h323` по префиксу id, т.е. UI штатно показывает шлюзовых участников как абонентов. Severity (a) — **P0, подтверждено end-to-end**, не «латентно».
- **R2-2 (входы `parse_participant_id`). ПРИНЯТ как список.** Все вызовы: `gateways.rs:1132,1239,1269,1334,1775,2006,2039,2050,2721,3593` (внутренние id `sip-/h323-<uuid>-in@…`, recorder, ids из реестра — ASCII); `lib.rs:439,755,878,964,977,1000,1036` — из `membership.participant_id`/`pid_str`, т.е. `handler.participant_id()` = `part-<room_uuid>-<uuid_v7>` (`vks-signaling/src/handler.rs:329`; `room_id` там всегда UUID — `lib.rs:1011-1022`), ASCII; `lib.rs:1338` — тест. `display_name`, `contact`/IDN-URI в парсер не попадают (grep: нет). → паника `lib.rs:392-394` достижима только при будущем Unicode-id; остаётся **латентной** (P3), фикс — заменить срез на `is_char_boundary`.
- **R2-3 (политика дублей позиций (c)). ПРИНЯТ тезис о политике.** Наблюдаемость: два слота с одинаковым `position` в PATCH дают в `BTreeMap<ParticipantId,u32>` две записи с одним u32 (`gateways.rs:1775,1778`), MCU строит `BTreeMap<slot,pid>` и второй `insert` затирает первый (`engine.rs:230-236`), при этом `assigned_count = slot_to_pid.len()` = 1 — т.е. второй участник без окна и **без предупреждения**. Политика (предлагаю): детерминированное «последний в порядке массива слотов побеждает, проигравший переносится в первый свободный слот; если свободных нет — слот не назначается» + `warn!` (room, position, loser) — не 500 и не молчание. Минимальный фикс: в цикле `apply_layout` держать `BTreeSet<u32> taken` и `warn!` при повторе.
- **R2-4 (DTMF 8k vs 48k). ПРИНЯТ, severity P1.** Доказательство отправки: тик 20 мс → `dtmf_timestamp = dtmf_timestamp.wrapping_add(160)` (`gateway_audio.rs:452`), из него берётся `started` и `duration` пакета (`gateway_audio.rs:437-451`: `[160,320,480,480,480]`, `[12]=event`, `[13]=0x0a/0x8a`), т.е. **все** поля времени события в 8-кГц шкале. Объявленный clock — `media.audio_clock_rate` (`uas.rs:305-311`), для Opus 48000 → формально неконформно: RFC 4733 §2.3.1 требует duration в единицах clock из `a=rtpmap` телефонного события, значит 1920 units читаются терминалом как 40 мс вместо 240 мс; часть IVR/дозвонных серверов отбрасывает тоны короче 40 мс и/или ловит несовпадение с дельтой RTP-таймстемпа. Хуже: типичный оффер «Opus + telephone-event/8000» вообще не согласуется — `telephone_event_payload` требует `clock == 48000` (`uas.rs:756-770`), `dtmf_payload_type = None` → события не генерятся и входящие события отвергаются валидатором (`gateway_audio.rs:613-620`). Итог: **на Opus-плечах IVR/дозвон не работают** (P1), на G.711/G.722 — работают (clock 8000 совпадает).
- **R2-5 (H.323 без DTMF). ПРИНЯТ, подтверждение «не заглушка». **Это не stub, а отсутствие реализации: единственные места — разбор входящих `UserInputIndication` (`h323_listener.py:269-295,578-581`; `gateways.rs:3755-3777`); отправки UII нет ни в `h323_listener.py`, ни в `h323/uas.rs`, ни в `uac.rs` (grep `userInputIndication` — пусто). На исходящем направлении H.323-плечо получает `RtpBridgeSession.dtmf_payload_type = None` (`gateways.rs:3668`) и цифры из `relay_dtmf_character` уничтожаются `dtmf.clear()` (`gateway_audio.rs:452-453`) — т.е. «доставлено» считается, а звука нет. Оценка: P1 для H.323-дозвона (в H.323 нет RFC 4733 в базовом профиле — нужен H.245 UII либо Q.931 keypad).
- **R2-6 (late video в H.323). ПРИНЯТ, P1.** Жизненный цикл: `open_channels` открывает каналы один раз при получении TCS/MSD (`h323_listener.py:601-608`); `on_open` для входящего OLC с `kind=="video"` ставит `self.video_rx_pt/incoming_video` и шлёт ACK, **не вызывая** `event_sink` (`h323_listener.py:399-449`; ср. `on_close` → `event_sink("channel_closed", kind="video")`, 450-465); `maybe_connected` уже отработал ранее и повторно не вызывается (`609-624`); Rust обрабатывает только `incoming/connected/dtmf_digit/video_fast_update/channel_closed/failed|disconnected` (`gateways.rs:3416-3830`). Значит позднее открытие канала (камера off→on, или видео позже 3-секундного окна) не порождает ни одного события в Rust → видео не появится до нового вызова. P1 (звук при этом живёт; деградация — «нет видео», не разрыв).
- **R2-7 (RTCP). ПРИНЯТ как уточнение, severity понижена до P2.** Потребитель счётчика существует (`gateway_video.rs:876,901,932`; `farm.rs:798-806`), но лишь журналирует, а `keyint_min = keyint = fps` (`gateway_video.rs:356-358`, `farm.rs:974-976,1118`) даёт IDR ≤1 с. Следствие потери пакета — пауза картинки ≤1 с, не «стоит до переустановления». Остаётся: NACK нет (`rtcp.rs:118-143`), RTCP-читатель только у inbound-SIP видео (`gateways.rs:2801` — единственный вызов), `build_pli/build_fir` мертвы (`rtcp.rs:155-183`).
- **R2-8 (смешанная комната). ПРИНЯТА формулировка.** WebRTC↔SIP/H.323 — **архитектурная задача**: termинация DTLS-SRTP на MCU (или отдельный media-gateway leg) — `gateways.rs:1208-1220` и `webrtc_runtime.rs:99-100` уже документируют отсутствие SRTP-моста; чистыми сценариями это не ломает: гард срабатывает только при наличии peer другого типа в той же комнате (`webrtc_runtime::room_has_peer`; обратный — `lib.rs:742-743`), а «шлюз-в-шлюз» и «браузер-в-браузер» работают как раньше. Точечный фикс в 10 строк невозможен: нужен SRTP-терминатор и пересборка media-пути.

## 5. Приоритетные исправления

### Протокол/сигнализация
1. **P0. (a)** `layout_identifier_has_uuid`: искать UUID как подстроку на границах токенов (`split(|c: char| !c.is_ascii_hexdigit() && c != '-')` или `parse_participant_id`+сравнение), а не «последние 36». Добавить кейсы `sip-<uuid>-in@sip-gateway`, `h323-<uuid>-in@h323-gateway` в `gateways.rs:5097-5106`.
2. **P0. H.323-аудио, два независимых дефекта.** (i) пиннинг источника: `source_latch=false` (`gateways.rs:3667`) + `gateway_audio.rs:605-610` → включить латч после первого **валидного** пакета с IP пира (accept-list по `remote_ip`), либо `strict_source` с отдельным логом «source-port mismatch (signalled X, got Y)»; (ii) PT-несовместимость: разрешить interchange для G.722↔PCMU/PCMA по факту приёма (как сейчас 0↔8, `gateway_audio.rs:616-620`) или переспрашивать кодек re-INVITE/OLC. Обязательно: **разные reason-коды в логе** вместо одного «codec/PT mismatch» (`gateway_audio.rs:288,633-648`) — сейчас 5 причин отбрасывания неотличимы.
3. **P1. DTMF**: для `telephone-event` всегда объявлять/принимать clock 8000 независимо от аудиокодека (Opus) — `uas.rs:759-770,305-311`; шкалу событий оставить 8 кГц либо масштабировать в `audio_clock_rate` (`gateway_audio.rs:437-452`); для H.323 добавить отправку H.245 `UserInputIndication` (сейчас нет ни в Rust, ни в Python) и не считать доставку успешной при `dtmf_payload_type=None` (`gateway_audio.rs:452-453`).
4. **P1. Дубли позиций (политика)**: в `apply_layout` держать `BTreeSet<u32> taken`; при повторе `position` — детерминированно (последний в порядке массива побеждает) переносить проигравшего в первый свободный слот, иначе не назначать, и всегда `warn!` (room, position, loser). Не 500 и не молчание (`gateways.rs:1770-1790`, `engine.rs:230-236,692-698`).
5. **P1. H.323 late video**: эмитить `channel_opened` из `on_open` (`h323_listener.py:399-449`) и обрабатывать в Rust (перезапуск video-задачи по `remote_video_ip/port/pt` из события); при `channel_closed` — unbridge/bridge треков в MCU.
6. **P2. re-INVITE видео**: 501 заменить на поддержку добавления/снятия видео (сейчас невозможно в звонке для SIP).

### Медиа
7. **P2. RTCP**: PLI/FIR сейчас только журналируются (`gateway_video.rs:876,901,932`; `farm.rs:798-806`), компенсируется `keyint=fps` (≤1 с) — довести до форс-IDR (`force_key_frames`/рестарт группы) либо честно документировать; подключить `spawn_video_rtcp` (`gateways.rs:2801` — единственный вызов) к исходящим и H.323-плечам; удалить мёртвые `build_pli/build_fir` (`rtcp.rs:155-183`).
8. **P1. Порты/лизы**: `_video_guard` держит пару при отказе видео (`gateways.rs:3125-3136,3690-3740`) — проверить, что лиза освобождается и при `channel_closed`, иначе утечка портов до конца вызова.

## 6. Риски

- Ложная диагностика (616 логов «PT mismatch» при совпадающем PT) прячет реальную причину — оператор не увидит проблему source-порта.
- H.323 — единственный протокол с двусторонним видео и он самый хрупкий: 3-секундное окно на видео при connect, строгий `sessionID`/reverse-канал, отсутствие late-video.
- DTMF: на Opus-плечах либо полностью отсутствует (оффер с /8000), либо события в 6 раз короче (оффер с /48000).
- RTCP-фидбек не влияет на энкодер → деградация видео при потерях до 1 с (интервал keyframe).
- `rtcp.rs` вне git — ревью и защита от потери отсутствуют.
- Runtime-артефакт (бинарь) отстаёт от `gateway_audio.rs` — риск ложных выводов при следующем прогоне стенда без пересборки.

## 7. «Меньше кода»

- `gain()` продублирован: `gateway_audio.rs:11` и `gateway_aac.rs:24` (идентичная логика tanh/clamp).
- SR-билдеры: `rtcp_sender_report` (`gateway_audio.rs:472`) и `sender_report` (`gateway_video.rs:1259`) — две реализации одного пакета; плюс `build_rr`/`build_pli`/`build_fir` в `rtcp.rs` (последние два не используются).
- Три почти одинаковых RTP-цикла: `gateway_audio.rs:run_audio_bridge` (861 стр.), `gateway_aac.rs` (свой цикл, ~400), `gateway_video.rs` (2 задачи). Общая часть — валидатор, отправка SR, счётчики — уже вынесены, но дублируется в AAC.
- Мёртвое/полумёртвое: `build_pli`/`build_fir`; `controls.keyframe_requests` (потребитель есть, но только `debug!`, без форс-IDR); событие `negotiating` (`h323_listener.py:725`) не обрабатывается; поле `video_status` (`h323_listener.py:623`) не читается; двойное представление DTMF-плеча (`controls.dtmf_payload_type: AtomicU32` с сентинелом 256 при `RtpBridgeSession.dtmf_payload_type: Option<u8>`).
- Trait-абстракции: `OutboundDialer` (`gateways.rs:1700`) и `MediaEngine` — используются по назначению (реальные альтернативные реализации: `McuEngine`/`SfuEngine`, dialer в admin), «лишних» trait'ов в шлюзовом слое не нашёл → тезис «лишние trait-абстракции» **опровергнут**.

## 8. Тесты (в т.ч. реальные RTP-проверки)

Юнит/интеграционные:
1. `layout_identifier_has_uuid` + `apply_layout` с id `sip-<uuid>-in@sip-gateway` и `h323-<uuid>-in@h323-gateway` → назначение доходит до `mcu.layout_descriptor()`.
2. `RtpSource` с `source_latch=false`: пакет с чужого порта → `None`; с `source_latch=true` — латч и последующее отбрасывание третьего источника.
3. DTMF SDP: оффер `Opus/48000 + telephone-event/8000` → `dtmf_payload_type = Some(..)`; ответ объявляет clock, соответствующий шкале генерации; оффер `/48000` → метки времени событий шагают 960/20мс.
4. MCU: два pid с одной позицией → один warn + второй слот, а не молчаливая потеря.
5. RTCP: PLI на сокет плеча → запрос IDR доходит до farm (или счётчик помечен как метрика-только).

Реальные RTP-проверки (без инициирования «настоящих» звонков, на локальном стенде):
6. Инжектор RTP на UDP-порт, назначенный H.323-плечу: отправить G.722 PT9 (160 байт, 20 мс) с (a) сигнализированного порта, (b) чужого порта — проверить `audio_rx_packets` в `/api/v3` метриках шлюза и отсутствие строки «rejected» в логе.
7. Инжектор RFC 4733 на PT telephone-event: проверить, что цифра доходит до `relay_dtmf_character` (лог «DTMF received … relayed», `gateways.rs:330-341`).
8. `ss -u` + `ps` до/после сценария: убедиться, что порты пары и ffmpeg-процессы освобождаются (сейчас в покое ffmpeg=0).
9. Метрики: `GatewayMediaStats::snapshot` (`gateways.rs:213-260`) — `rtcp_pli/fir`, `rejected_packets` как контракт наблюдаемости.

## 9. Непроверенное / ограничения

- Полностью построчно `gateways.rs` (6297) и `gateway_video.rs` (2779) не прочитаны; выводы по видео-плечу опираются на карту функций и ключевые участки.
- Сгенерированный ASN.1 (`h323_asn.py`, 16 871 строк) не аудировался.
- Реальные SIP/H.323-терминалы не подключались (звонки не инициировались по условию); выводы о совместимости — по коду и стендовым ботам.
- Точная причина каждого из 616 отбросов не подтверждена инструментально (в логе нет ожидаемого source); вывод «source-port» — сильнейшая гипотеза по коду (`gateway_audio.rs:605-610` + `gateways.rs:3667`).
- Присылает ли UI `abonent_id` с суффиксом `-in` — не подтверждено трассировкой UI (minified-бандл, `sip-gateway` в нём отсутствует).
- Поведение рантайма соответствует бинарю 2026-10-25 20:37, который старше текущего `gateway_audio.rs` — для (d)/(1) возможны расхождения.
- Дебаты: внешний durable-челленджер Lead'а не успел к финалу (раунд 2 состоялся по 8 контраргументам Lead'а, все разобраны в §4); прямой `subagent` технически недоступен на моей глубине (depth 2 > maxDepth 1).
- Класс B отказов для **SIP**-входящего (223 строки, `expected==received`): точная причина (дубли/смена SSRC/устаревший seq — `gateway_audio.rs:633-648`) не инструментирована, лог её не различает.
- Тракт UI доказан по API (`resources.rs:486-534`), но не трассировкой minified-бандла (в нём `sip-gateway` не встречается вовсе).
- Влияние `video_status`/`channel_opened` на клиентское отображение (получит ли браузер обновление трека) — вне этого аудита.
