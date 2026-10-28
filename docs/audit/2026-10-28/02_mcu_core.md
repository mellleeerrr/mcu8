# 02. MCU core: API → storage → sessions/push → engine/compositor

Агент `mcu-core` (task-2). Read-only по исходникам; единственная запись — этот файл. Сервис
PID 2965299 не перезапускался, `vks_data.db` не изменялась, release-сборка не запускалась.
Прочитано полностью: `vks-media-mcu/src/{lib,layout_bridge,compositor,engine,farm}.rs`,
`vks-server/src/{layout_signal,room_events,farm_metrics}.rs` + срезы `lib.rs`, `gateways.rs`,
`gateway_video.rs`; `vks-admin/src/api_v3/{resources,sessions,shell}.rs`, `state.rs`,
`vks-storage` (миграции, `core_adapter`), `vks-proto/proto/{signaling,admin}.proto`.
Runtime — только анонимные read-only GET; авторизованные GET дают 401, `POST /api/v3/login`
пишет audit (auth.rs:284) — не выполнялся (запрет мутаций).
Subagent спавнить нельзя (depth 1 = max 1) → дебаты в режиме «адвокат дьявола» + внешний
челленджер Lead (§10).

## 1. Контракт PNM-раскладки

| Поле | Читает | Факт |
|---|---|---|
| `payload.type` | admin | строковое сравнение `default`/`individual` (resources.rs:475-482); движку не передаётся |
| `payload.preset` | admin+gateway | id геометрии → `codian::geometry`/`grids::geometry`/`preset::geometry_for_manual` (gateways.rs:1834-1838) |
| `payload.size` | admin+gateway | число окон; пересчитывается из preset/slots (resources.rs:1183-1199); в движок идёт лишь как `preset_size` (gateways.rs:1789) |
| `payload.slots[].position` | gateway | 1-based, база `payload.position_base` 0/1, по умолчанию 1 (gateways.rs:1749-1760) |
| `slot.role` | только UI | движок `role` НЕ читает; `AUTO`/`PEER` пишут только attach/detach (resources.rs:855-860, 926-930); пересадка — по `important`/active speaker (engine.rs:400-467) |
| `slot.abonent_id/connection_id/contact_id/call_string_id` | gateway | первый со встроенным UUID (gateways.rs:1768-1773); иначе слот молча пропускается |

Следствия: `role` — мёртвая для media-плоскости метка (AUTO-окно с непустым `abonent_id`
всё равно рисуется); `size` и число окон `preset` — два источника (recalc_layout_size
предпочитает slot_count: resources.rs:1188-1196), расхождение ловится только
авто-расширением; геометрия клэмпится в 1920×1080 (engine.rs:258-271), отрицательные
координаты UI не валидируются (`round().max(0.0)`, gateways.rs:1841-1848).

## 2. active vs templates, create/PATCH, COW, attach/detach

* «Шаблоны» — не сущность: `GET …/video_layouts/templates` читает те же записи, выбирая последнюю non-default (resources.rs:2134-2185); отдельного стора нет.
* `active` — карта `active_layouts: scope_key → id` (resources.rs:30-36), не порядок links.
  Чтение: `active_id_for` sessions→conferences (591-601); без active — последняя
  non-default, затем default (sessions.rs:470-487).
* `PATCH`: `direct && owners<=1` → мутация на месте (id жив), иначе COW-клон с новым UUID и
  алиасом `scope|old→new` (resources.rs:622-667; edit_linked:1560-1590, attach/detach:865-880/930-945).
* `detach` НЕ удаляет окно и id: слот → `{position, role:"AUTO"}` (926-930), запись и алиас
  живы; detach несуществующего абонента — 204 без ошибки. Старый id живёт как алиас (552-560).
* `attach` сохраняет `position`/геометрию, занимая первое окно без `abonent_id`; если таких
  нет — дописывает `position=max+1`, проверив только `occupied < payload.size`
  (836-880). Окно вне `size` → `slot_count=max+1` (gateways.rs:1785-1789) → движок строит
  лишнее окно, пресет и `size` расходятся.
* Каноническая «Общая» = точное равенство payload (vks-common/layout_defaults.rs:13-21): любое лишнее поле плодит вторую «Общую», а выбор `find_canonical_layout_id` недетерминирован (обход BTreeMap по случайным UUID, resources.rs:669-675).

## 3. pending, LayoutChanged, layout_snapshot

* `apply_layout` без комнаты кладёт дескриптор в `pending_layouts` и возвращает `Ok`
  (engine.rs:1639-1647); с комнатой — снимает pending и применяет (1648-1652). Потребляют
  pending: `ensure_room:1357`, `bridge_participant:1374`, `seat_publisher:1398`, `create_room:1527`.
* `layout_snapshot` без комнаты СТРОИТСЯ из pending (1203-1217); `layout_descriptor` тоже
  отдаёт pending (1180-1192). `LayoutChanged` собирается из snapshot после каждого apply
  (gateways.rs:929-945, 1920).
* Достижимость «LayoutChanged с неотрендеренной раскладкой»: broadcast идёт через room_hub,
  участники появляются только после JoinRoom, а JoinRoom сначала создаёт MCU-комнату
  (lib.rs:1037-1042 → create_room забирает pending, engine.rs:1527), потом кладёт snapshot
  в `JoinRoomResult` (lib.rs:1101-1112). Пока комнаты нет — получателей нет ⇒
  **пользовательски недостижимо**; остаётся «snapshot не доказывает кадр» (P3, §9).
* Реальный P2 рядом: авто-расширение при входе паблишера меняет геометрию и сбрасывает
  `preset` (engine.rs:164-186 → 712-746), но **не эмитит LayoutChanged**:
  `register_publisher` только `bridge.apply`+`current_layout` (1411-1432), а
  `refresh_seating` эмитит лишь Focus/Speaker (400-406). В Grid 14-й участник в
  `preset_default_13` даёт новую сетку на экране MCU, клиент остаётся с 13 окнами до
  следующего apply/PATCH.

## 4. Четыре состояния «сохранено / ожидает / в движке / подтверждено»

| Состояние | Где | Из API видно |
|---|---|---|
| сохранено | `configuration_documents` (0005_configuration_documents.sql:2-5), весь ResourceState одним JSON (resources.rs:250-267) | 200/204 |
| ожидает | `pending_layouts` (engine.rs:652) | нет |
| установлено | `RoomState.layout`+`current_layout` (312-323) | нет (только тестовый descriptor) |
| подтверждено кадром | нигде: `frames_rendered`/`last_duration_us` (380-391) не связаны с версией | нет |

Разделения в контракте нет: PATCH/attach/detach коммитят storage, затем дергают
`push_layout_to_engine`, ошибка — только `warn` (sessions.rs:491-511), ответ уже 200/204.
`engine.apply_layout` **никогда не возвращает Err** (1639-1705 — обе ветки `Ok`), поэтому
`map_err` (gateways.rs:1911-1917) мёртв. Тихие no-op в проде: (а) `assignments.is_empty()
&& geometry.is_empty() && mode != "auto"` → `Ok` без применения (gateways.rs:1906-1908);
(б) слот без `position`/UUID-id молча пропускается (1762-1773; warn только при
`position < position_base`).

## 5. video_settings: hot reload нет

PATCH `/settings/video` пишет документ и память (shell.rs:664-691), но профиль применяется только при дозвоне: `video_profile()` (gateways.rs:623-637) → `apply_video_settings` (gateway_video.rs:1516-1548) из `dial_with_contact_inner` (gateways.rs:1462). Живой мост держит копию профиля (gateway_video.rs:568-614), единственный re-key — по видимым окнам (741-760). `hot_reload_from_storage` (state.rs:552-554, из `/system/restart_services`) перечитывает документ, но мосты не перезапускает (MASTER_TASK_LIST.md:703). Смена width/height на ходу невозможна: холст фиксируется после первого рендера (engine.rs:1078-1096).

## 6. Конкурентность: блокировки и гонки

Порядок локов: `rooms → frames → audio`, `rooms → pending`; `farm.groups` и `visible` —
отдельные, вне цепочки. Инверсии `pending → rooms` нет: pending берётся временным guard'ом
в одном выражении (engine.rs:1358, 1377, 1399) либо после снятия rooms (1544). `emit` — только
после снятия rooms (892-916, 941-958); `rekey_frames` — наоборот, под rooms (1655-1662) ради
атомарности «назначения+кадры». Дедлоков не найдено.

| # | Гонка | Механизм | Итог |
|---|---|---|---|
| R1 | `create_room` ↔ `apply_layout(pending)` | apply отпускает rooms (1643-1647), create берёт rooms+pending (1527-1534) | pending осиротеет/подхватится первым bridge; до него snapshot=default, descriptor=pending; destroy чистит (1544) |
| R2 | два PATCH комнаты | per-room `layout_pushes` (sessions.rs:493-499) | сериализовано — ок |
| R3 | auto-expand ↔ apply_layout | оба под rooms; версия `max(incoming, old+1)` (1684-1686) | откатов нет; «кто позже» ≠ «кто новее в UI» |
| R4 | пересадка ↔ apply_layout | rooms сериализует, rekey под rooms | ок |
| R5 | два моста задают холст | `ensure_composite_size` (1078-1096), затем `farm.acquire` (gateway_video.rs:573-614) | A уже взял группу v1, B сменил холст → `invalidate_room` (farm.rs:558-580) ретирит группу; output-таск ловит `Closed` и **break** (gateway_video.rs:777-781) → видео у зрителя умирает до конца вызова, восстановления нет |
| R6 | self-view после старта моста | проверка только на входе (592); в farm кадр = `tick_video_for_viewer` → `publish_video_frame` (892-893) | персональная дельта уходит в общий битстрим группы. Латентно: админ-маршрута set_self_view нет (только тесты) |
| R7 | общий Mutex комнат | engine.rs:640; `tick` держит его весь `process_video` (857-870 → 380-391) | композитинг A блокирует B; 10 сессий сериализуют рендер |

## 7. Farm encoder path vs per-session, self-view, профили

* Ключ группы = room + canvas_version + весь EncoderSpec (bitrate/profile/level/params):
  farm.rs:126-166, 216-224. Разные профили зрителей → разные группы (gateway_video.rs:500-503):
  до 20 FFmpeg на комнату; лимит 128 (farm.rs:368-372, env `VKS_VIDEO_FARM_MAX_GROUPS`), при
  `Overloaded` — fail-open на per-session child (gateway_video.rs:614-621), т.е. лимит
  энкодеров не соблюдается end-to-end.
* Per-session (ферма off / self-view / лимит): декодер+энкодер на зрителя (614-625),
  `resize_i420` + `spawn_codec` (302-405).
* Битрейт группы `group_bitrate_kbps(base, visible)` = clamp(base×(1+0.02×(v−1)), base,
  1.25×base) (engine.rs:621-631) → в спеку как bitrate=maxrate, bufsize=2×
  (gateway_video.rs:514-532) → `-b:v/-maxrate/-bufsize` (farm.rs:966-978).
* Согласование: offer объявляет `b=AS:{base}` (vks-gateway-sip/src/rtp_bridge.rs:651), answer
  клампит `bitrate_kbps = min(base, max-br/b=AS)` (gateways.rs:1461-1462;
  gateway_video.rs:163-179). Коэффициента 1.25 в SDP/H.241 нет ⇒ для терминала с
  `max_br < base/0.8` поток превышает объявленный ему `b=AS`/`max-br` до +25 % → риск дропа
  на приёмнике (P2: нарушение объявленного значения, не доказанный баг потерь).
* Дедуп публикации: `min_gap = 3/4 интервала кадра`, очередь mpsc(1) (farm.rs:508-545) —
  корректно; IDR-гейт и `Lagged` учтены (gateway_video.rs:764-780).
* `visible_windows` = занятые ячейки `current_layout` (engine.rs:1237-1244); канал с дебаунсом
  500 мс пишет только движок (1252-1294) — thrash устранён; подтверждено логом
  (vks_server.log:3990-4058: 1→13→14→17→…→20, группы пересоздаются, старые живы).

## 8. Runtime (read-only, без мутаций)

```
/healthz                  200 {"alive":true}
/readyz                   200 {"cluster":true,"db":true,"gateway":true,"media":true}
/metrics                  200; 14 семейств, encoder_group_* отсутствуют (групп нет)
/api/v3/mcu/farm          200 {"groups":[],"totals":{"frames":0,"groups":0,"keyframe_requests":0,
                             "viewer_rtp_bytes":0,"viewer_rtp_packets":0,"viewers":0}}
/api/v3/gateways/status   200 sip/h323 READY, active_calls=0
/api/v3/cluster/status    200 {"status":"ACTIVE","owned_rooms_count":0,"node_id":"vks-mcu"}
/api/v3/ui/video_layouts  401 · /api/v3/sessions 401 · /api/v3/video_layouts 401
```
Сервис простаивает (0 комнат/групп) — живой раскладки для сверки геометрии нет. m5-роуты
(farm/gateways/cluster) отвечают без токена, а `/api/v3/ui/*` — 401: периметр неоднороден
(lib.rs:160-190; наблюдение для безопасности, вне рамок ядра).

## 9. Опровергнутое

1. «LayoutChanged врёт клиенту pending-раскладкой» — **не достижимо** (§3): без комнаты нет
   получателей, JoinRoom создаёт комнату до snapshot.
2. «layout_descriptor из pending видит админка» — **нет**: прод-вызовов нет, только тесты
   (gateways.rs test-модуль с 3858; engine.rs — с 1714).
3. «no-op `OutboundDialer::apply_layout` (state.rs:150-158) = тихий путь в проде» — **нет**:
   dialer ставится при сборке роутера (lib.rs:135); тихие пути другие (§4).
4. «detach удаляет старый id/окно» — **нет**: окно остаётся AUTO, id — алиасом (§2).
5. «ферма и self-view несовместимы всегда» — только на входе моста; при включении после
   старта групповой битстрим получит персональный кадр (R6, латентно).
6. «`role` влияет на посадку в окно» — нет, влияют только `position`+id (§1).
7. «общий Mutex комнат держится только на lookup» — нет: он держится через весь
   композитинг `process_video` (engine.rs:857-870, 380-391).

## 10. Дебаты (адвокат дьявола, 2 раунда; внешний челленджер — Lead)

**Р1 (я): «snapshot из pending — контрактный дефект P1».** Контртезис: получателей broadcast
без комнаты не существует, JoinRoom создаёт комнату раньше snapshot (lib.rs:1037-1042 →
engine.rs:1527) — дефект наблюдаемости, не клиента. Вердикт: **я проиграл** (P3), но в споре
нашёлся более сильный дефект P2: auto-expand без LayoutChanged (1411-1432 vs 400-406).

**Р2 (я): «layout_descriptor отдаёт pending админке → UI показывает применённое».**
Контртезис: в прод-коде вызовов нет, API мёртв. Вердикт: **я проиграл** → «лишнее» (§12).

**Р3 (я): «тихое сохранение = default no-op dialer».** Контртезис: в проде dialer всегда Some;
тишина в другом — `apply_layout` не умеет возвращать Err, ранний `Ok` при пустых
assignments/geometry и пропуск слотов без UUID (gateways.rs:1906-1908, 1768-1773).
Вердикт: **контртезис сильнее**, формулировка исправлена, статус P2 без 5xx.

**Р4 (я): «ферма завышает согласованный битрейт — баг».** Контртезис: 1.25× — заявленный
headroom (engine.rs:621-631, BENCHMARK §10), в SDP не уходит, согласование не нарушено.
Вердикт: **частично**. В SDP уходит `b=AS=base` в offer (rtp_bridge.rs:651) и терминальный
`max-br` клампит base (gateways.rs:1461-1462); 1.25×base может превысить оба ⇒ риск, но не
доказанный дроп. P2 «нарушение объявленного b=AS/max-br», фикс — клампить группу.

**Р5 (я): «/settings/video — hot reload».** Контртезис: hot reload в проекте относится к
конфиг-документу (`hot_reload_from_storage`), а не к живому профилю; для media — «со
следующего вызова» by design. Вердикт: **согласился наполовину** — как hot reload видео не
реализовано; как «к следующему дозвону» работает, но оператору это нигде не сказано (P2).

## 11. Минимальные исправления по приоритетам

**P1**
1. Эмитить LayoutChanged при авто-расширении/пересадке в Grid: из `seat_video_or_expand`/
   `register_publisher` через sink после снятия rooms (образец — `tick_audio_at`,
   engine.rs:892-916). Тест: 14-й паблишер в preset_default_13 → клиент получает событие.
2. Farm: `Closed` в output-таске не должен убивать видео (gateway_video.rs:777-781) —
   retry-loop на текущую версию/спеку, иначе смена холста (R5) = немая потеря до конца вызова.

**P2**
3. Статус применения без ломки вендорских форм: audit-запись `layout.apply.skipped` +
   метрика `vks_layout_apply_total{result=applied|pending|skipped|error}` + заголовок
   `x-vks-layout-applied: 0|1` в 200/204 (рядом с `x-vks-layout-id`, resources.rs:952-958). 5xx не вводить.
4. Farm bitrate: `min(group_bitrate_kbps(base,v), negotiated_max_br)`; не поднимать выше
   base, если base уже clamped ответом (gateway_video.rs:514-532, 163-179).
5. video_settings → живые мосты: поколение (`AtomicU64`) + watch; farm render-таск на смену
   пересобирает `farm_spec` и re-acquire (образец visible_rx:744-757); per-session — рестарт
   child; width/height — явно «со следующего вызова» (холст фиксирован, 1078-1096).
6. self-view в живой комнате: принудительно выводить зрителей из фермы
   (`set_self_view` → `farm.invalidate_room` + re-key per-session), иначе R6.
7. `is_canonical_default` — по `type`/`preset`, а не по полному payload
   (vks-common/layout_defaults.rs:19-21): иначе дубли «Общей» и недетерминированный выбор (669-675).
8. attach за пределы `size`: не дописывать `position=max+1`, а расширять `size`/окна явно
   либо 409 (resources.rs:836-880).

**P3**
9. Убрать/переименовать публичный `layout_descriptor`-с-pending (engine.rs:1180-1192) — ловушка.
10. R1: вставлять pending под тем же удержанием rooms (или перепроверять комнату), чтобы не
    терять раскладку между 1643 и 1647.
11. Документировать «role не влияет на MCU» и «size ≠ число окон geometry».

## 12. «Меньше кода»: дубли и лишние слои

* Два реестра раскладок: `AppState.layouts` (state.rs:226, гидратация из `core_layouts`
  686-698) и `api_v3.resources.layouts` (resources.rs:25). Первый читает только
  `GET /api/v3/video_layouts` (shell.rs:966-1005), пишут API только второй ⇒ два эндпоинта
  могут отвечать разными наборами. `core_layouts` (+ `LayoutDto`, admin.proto:30-35) —
  мёртвый слой персистентности.
* `remove_publisher` (engine.rs:1580-1618) и `unbridge_participant` (1465-1503) — построчный
  дубль очистки (~90 строк): вынести `purge_participant`.
* `add_publisher` (1565-1579) регистрирует паблишера без посадки в слот, а
  `bridge_participant`/`seat_publisher` — с посадкой; в проде `add_publisher` не вызывается
  (только тесты) — второй, расходящийся путь регистрации: удалить или делегировать.
* `layout_snapshot` + `current_layout`-обёртка (1194-1201) + `layout_descriptor` — три
  публичных чтения одной сущности; достаточно `layout_snapshot`.
* `payload.abonents`/`contacts` дублируются в записи и досчитываются на чтении
  (`with_layout_abonents`, resources.rs:536-540) — можно не хранить.
* `EncoderSpec::encoder_params` дублирует profile/level (farm.rs:126-150) и входит в ключ
  группы — правка строки params плодит новый энкодер.

## 13. Тесты (детерминированные сценарии)

1. **pending/adopt**: apply_layout до create_room → create_room → snapshot = применённая;
   destroy чистит pending (engine.rs:1527-1544, 1203-1217).
2. **auto-expand без LayoutChanged**: 13-window preset + 14-й паблишер → `slots.len()==14`,
   `preset==""`, sink получил LayoutChanged (сейчас нет — фикс P1.1) (712-746, 1411-1432).
3. **гонка create_room ↔ pending**: вставить pending вручную (как engine.rs:3099), затем
   create_room; проверить отсутствие pending без комнаты (есть тест 3083-3111).
4. **farm bitrate ≤ согласованного**: base=512, max_br=512, visible=20 →
   `farm_spec_for(...).bitrate_kbps <= 512` (сейчас 640).
5. **Closed не убивает egress**: invalidate_room во время работы → переподключение, не break
   (gateway_video.rs:777-781).
6. **self-view после старта**: включить `set_self_view` при активной группе — кадр в группе
   должен быть общим (сейчас персональный).
7. **attach сверх size**: 13 окон заняты, attach 14-го → 409 либо size=14 с согласованной
   geometry (сейчас position=14 и расхождение).
8. **canonical dup**: payload с лишним полем → `is_canonical_default==false` → вторая «Общая».
9. **lock-order smoke**: tick (rooms→frames→audio) ∥ apply_layout (rooms→pending) ∥ unbridge
   (rooms→frames/audio), 10⁴ итераций — дедлоков быть не должно.

## 14. Непроверенное

* Живые раскладки/комнаты: 401 без токена, login мутирует audit (auth.rs:284) — запрещено; геометрия превью против живого композита не сверялась.
* Дропы SIP/H.323 при групповом битрейте > `max-br` — нужен терминал/pcap; оценка «риск» по объявленным SDP-значениям.
* `VKS_VIDEO_FARM_MAX_GROUPS` в окружении сервиса не читал (не перезапускал); default 128 (farm.rs:368-372).
* `/metrics` без `encoder_group_*` (групп нет) — сверка с логом Oct-25 (`vks_server.log:3985-4058`), не с живым `/metrics`.
* Стоимость композитинга под общим Mutex (R7) не измерялась: `mcu_tick_*` есть в коде (engine.rs:352-358), семейств `mcu_*` в `/metrics` нет.
