# 01. Reference mining: 3 эталонных MCU → что переносим в наш движок

> **Агент:** `ref-miner` (durable teammate) · **Дата:** 2026-10-25 · **ВМ:** 192.168.1.93
> **Задача:** task-1 · **Артефакт единственный:** этот файл. Сборок/рестартов/sysctl-правок не выполнялось.
> **Легенда путей (сокращения, чтобы цитаты оставались точными):**
> `[TC]` = `/home/pro/Документы/MCU/mcu trueconf/reconstruction/` (TrueConf MCU 2.2.1.66-deb12)
> `[VI]` = `/home/pro/Документы/MCU/vinteo/08_отчёты_анализа/` (Vinteo MCU 6.12.77/30.2.43, RHEL6)
> `[CS]` = `/home/pro/Документы/MCU/cisco CMS/analysis/meta/` (Cisco CMS 3.13.1)
> `[OUR]` = рабочая директория проекта (`/home/pro/Рабочий стол/mcu`).
> **Метод:** отчёты эталонов читались как источник `file:line` (они сами построены по байтам/дизасму/живому рантайму), наш код — прямой инспекцией.
> **Границы честности:** §A в конце — что прочитано полностью, что выборочно; всё непроверенное помечено явно.

---

## 0. Позиция, которую я защищаю

**Формулировка (без изменений, защищаю фактами):**

> Донор №1 нашего медиаядра — **TrueConf 2.2.1.66**: per-abonent команды движку `ABOOK BITRATE/FRAME SIZE/FRAME RATE/ACODEC/VCODEC`, 6-уровневое наследование медиапараметров (L1…L6, приоритет **FALLBACK** «адрес → сессия», а не min), **разрешение = свойство раскладки, а не участника**, кодек = **пересечение** профиля ∩ whitelist ∩ лицензии ∩ поддержки терминала.
> **Vinteo** — донор SIP/H.323-цепочки, сетевых параметров, конфиг-гигиены и мониторинга (не донор модели медиаядра).
> **Cisco CMS** — донор media-framework element API и телеметрии (24 медиаэлемента, 115 ключей телеметрии); **H.323 у Cisco отсутствует — доказанный факт**, а не мнение.
>
> ⚠ **Уточнение к формулировке (не отказ, а приведение к проверяемому виду):** цифра «6 уровней» в эталоне противоречива — канонический отчёт `I.2-bis` даёт 5 уровней (L1…L5), а сводная строка говорит «6 (L1…L6)»; в другом отчёте нумерация L0…L7 (8 уровней). Разбор и маппинг на наш `media_policy.rs` — §0.2. Существо позиции (FALLBACK + разрешение-раскладка + пересечение кодеков) от цифры не зависит.

### 0.1. Степень уверенности по каждому тезису

| # | Тезис | Уверенность | Ключевое доказательство (эталон) |
|---|---|---|---|
| T1 | Медиапараметры TrueConf — **многоуровневое** наследование (канон I.2-bis: 5 уровней; сводная строка эталона: 6 — разобрано в §0.2), приоритет FALLBACK | **0.80** | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:34` («6 уровней L1…L6»), таблица `:125-131` (L1…L5), `:146-157` (эмпирика: contact 1024 побеждает conference 2048), `[TC]01_DATABASE/mediamodel.md:31` (L6 = `contacts_to_*._`), `[TC]00_DOCUMENTATION/AGENT_ROSTER.md:113-114` («L6 = только права») |
| T2 | Per-abonent команды движку `ABOOK …` существуют и применяются | **0.95** | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:29-31`, `:100-107` (полный список команд), `:201` (R1 per-абонент, help-строки движка) |
| T3 | Разрешение — свойство раскладки, а не участника | **0.90** | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:181-189`; `[TC]00_DOCUMENTATION/LIVE_VALUES.md:56-57`; `[TC]09_MEDIA_CORE/vsm_spec.md:48-49` (`Channel #%u … default resolution set to %ux%u`) |
| T4 | Кодек = пересечение 4 множеств (профиль ∩ whitelist ∩ лицензия ∩ терминал) | **0.85** | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:35-36`, `:113`, `:170`; `[TC]00_DOCUMENTATION/LIVE_VALUES.md:39-40` (лицензия режет `h264-svc`/`vp8`, БД их показывает разрешёнными) |
| T5 | Vinteo — донор SIP/H.323-цепочки, сетевых параметров, конфиг-гигиены, мониторинга | **0.85** | `[VI]02_SIP_H323_цепочка_вызова.md:101-290`; `[VI]13_Раскладки_Надписи_Сеть.md:768-947`; `[VI]11_Реестр_конфигурации_ВКС.md:989-1048`; `[VI]08_Функциональная_карта_подсистем.md:251-283` |
| T6 | Vinteo — **не** донор модели медиаядра (проприетарно и невоспроизводимо) | **0.90** | `[VI]09_Спецификация_воспроизведения_стенда.md:10-36`, `:768-802` (ядро `mcu`/`app_mixer`/`proxy` — только при наличии исходников Vinteo); `[VI]14_Реконструкция_исходников_MCU.md:797-819` (тексты функций не восстановимы) |
| T7 | Cisco CMS — донор media-framework element API | **0.90** | `[CS]10_MEDIA_FRAMEWORK_ELEMENTS.md:1-34` (24 элемента, единый call-table lifecycle); `[CS]15_MIXER_ELEMENTS_API.md:6-35` (file:line по функциям) |
| T8 | Cisco CMS — донор телеметрии (115 ключей, 14 report-типов) | **0.95** | `[CS]06_MEDIA_TELEMETRY_KEYS.md:1-20`; `[CS]MASTER_INDEX.md` (строка про 06) |
| T9 | H.323 у Cisco CMS 3.13.1 отсутствует | **0.99** | `[CS]16_H323_ABSENCE_PROOF.md:1-84` (0 совпадений по 2 286 путям DWARF и по 321 750 строкам бинарника; все 9 контейнеров = 0); `[CS]19_DEBATE_RESOLUTION.md:35-40` (D3 — жёсткое доказательство) |
| T10 | Наш MCU уже содержит модель TrueConf, но она не подключена | **0.95** | `[OUR]crates/vks-core/src/media_policy.rs:1-16` (заголовок «L1…L6, fallback, пересечение»), `:87` (`resolve()`, единственная точка); внедрение отсутствует: `[OUR]crates/vks-core/src/lib.rs:36` — только re-export, вызовов `resolve(` в дереве нет (grep по `crates/`) |

### 0.2. Уточнение, которое я обязан зафиксировать сам (слабое место позиции)

«6 уровней» — **не бесспорная цифра в самом эталоне**, и я не буду делать вид, что это так. В трёх отчётах — **три разные нумерации**:

| Отчёт | Нумерация | Что это значит |
|---|---|---|
| `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:121-131` (канон проекта: «Итог раундов 1–2 контраргументации A2+A8+R1+ведущий **и живая проверка**») | **L1…L5**, 5 строк: L1 движок, L2 `configurations._`, L3 `profiles._`→`conferences._`→`sessions._`, L4 `call_strings._`, L5 `contacts_to_*._` | Пять уровней; разрешение есть на L3/L4, **на L5 его нет** |
| та же строка 34 той же работы | «**6 уровней** (L1…L6, см. I.2-bis)» | Самоопровержение: ссылается на таблицу с 5 строками |
| `[TC]01_DATABASE/mediamodel.md:24-34` | **L0…L7**, 8 строк: L0 `configurations`, L1 `profiles`, L2 `conferences`, L3 `sessions`, L4 `video_layouts`, L5 `call_strings`, L6 `contacts_to_*`, L7 `abonents_to_sessions` | Раскладка — отдельный уровень L4; появляется per-абонент L7 |
| `[TC]00_DOCUMENTATION/AGENT_ROSTER.md:113-114` | «L6 = только права», «`cascade`/`self_view` — это L5» | Третья нумерация: сдвиг на +1 относительно I.2-bis |

**Вывод, который я защищаю (проверяемая формулировка, а не цифра):**
**(а)** в эталоне есть **многоуровневая цепочка слияния параметров**, и её ядро — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:121-131`: 5 уровней, из них **L3 — три таблицы** (`profiles` → `conferences` → `sessions`, слияние `||`, правый побеждает; затем копирование в сессию), **L4 — адрес/пользователь** (`call_strings.payload[protocol]`, 13 протоколов), **L5 — только права** участника;
**(б)** число «6» — из сводной строки `:34`, и я его **не воспроизвожу как факт**, пока Lead не решит, считать ли шестым уровнем дефолты Python (`:177-179`) или `negotiated_with_peer` (`:165-170`);
**(в)** наша нумерация `L1…L6` в `media_policy.rs:1-16` — **третья** (не совпадает ни с одной из трёх эталонных), поэтому для переноса нужен **канон I.2-bis + маппинг**, а не заимствование цифры.

| Наш `media_policy.rs` | Канон I.2-bis | Что переносим |
|---|---|---|
| L1 (defaults/движок) | L1 движок (`max_bitrate` 8192, `frame/size` [3840,2160]) | потолки движка |
| L2 (global) | L2 `configurations._` (101 строка; **ортогональный слой** — профиль его ключи не перекрывает) | `max_frame_rate` 30, `adaptive_bitrate`, `border`, `packet_recovery` |
| L3 (profile) | L3 первая таблица | `bitrate`, `resolution`, `codecs[]` |
| L4 (conference) | L3 вторая таблица | то же, перекрывает профиль |
| L5 (session) | L3 третья таблица (копия конференции) | то же |
| L6 (address/terminal) | **L4** `call_strings.payload[protocol]` | `bitrate`, `codecs`, `resolution`, `self_view`, `cascade`, `volume`, `bfcp`, `enable_content`, `fec`, `dtmf` |
| — (нет слота прав) | **L5** `contacts_to_*.payload` | `video_rx/tx_enabled`, `audio_rx/tx_enabled`, `volume_rx/tx`, `noise_suppression`, `auto_gain_control`, `slot_render_type`, `auto_call`, `crown` |

⇒ Наш `media_policy.rs` **не имеет уровня прав** (L5 канона) — это дыра, которую надо закрыть, а не «шестой уровень». Зафиксировано в C5 и Q4.

### 0.2-bis. FALLBACK ≠ min: как они сосуществуют (важно, иначе позицию легко разбить)

Мою формулировку «приоритет FALLBACK, а не min» легко атаковать: в `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:100-118` написано «**ИТОГОВЫЙ БИТРЕЙТ = min(…)**», а в `I.4` (строки 276-296) — пять формул с `min`. Разрешение противоречия (оно же — суть модели):

* **Выбор значения между уровнями — FALLBACK**: побеждает более конкретный уровень, а не меньшее число. Доказано эмпирикой `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:146-157` (контакт 1024 побеждает конференцию 2048; в БД нет ни одной операции `min`/`max` между уровнями — только `||` и копирование).
* **После выбора — clamp через `min` с потолками**: `bitrate_out = min(profile.bitrate, engine.max_bitrate, negotiated_bitrate_peer)`, `width_out = min(profile.w, engine.frame.size.w, slot_w × layout_total_w)`, `fps_out = min(profile.fps_limit, engine.max_frame_rate)` — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:279-295`.
* ⇒ Оба утверждения верны **на разных шагах**: FALLBACK — селектор, `min` — финальный ограничитель. Наш `media_policy.rs` делает ровно это: `resolve()` (fallback) + clamp (`:77-82`), но **клампы шире эталона** (см. M1).

### 0.3. Что опровергло бы мою позицию (falsifiability)

Я обязан сам назвать условия поражения — иначе позиция нефальсифицируема:

1. **Если у Vinteo найдётся per-abonent команда исходящего битрейта/кодека** (аналог `ABOOK BITRATE/ACODEC`), а не только `videomixer set composition param` для раскладки — тогда «Vinteo не донор модели» падает, и доноров модели два. Проверять: полный список 45 AI-команд `app_mixer.so` (`[VI]14_Реконструкция_исходников_MCU.md:797-819` честно говорит, что соответствие аргументов подтверждено вручную только для 3 команд) + строки `mcu` по `ABOOK`. **Не проверено.**
2. **Если разрешение в TrueConf окажется и per-участника** (поле в `contacts_to_*`/`abonents`, а не только `video_layouts.resolution`) — тогда тезис T3 ослабляется до «есть и то, и другое». Проверять: `[TC]01_DATABASE/mediamodel.md` (L6-поля) и `init.sql` по `resolution` вне `video_layouts`. Частично проверено: в прочитанном `I.2-bis` `resolution{width,height,fps_limit}` есть на L3 и L4, на L5/L6 — нет (`MASTER_ARCHITECTURE.md:129-131`).
3. **Если Cisco всё-таки содержит H.323** (например, в неисследованном контейнере или в `cucm`) — тезис T9 падает. Сейчас доказательство сильное (`[CS]16_H323_ABSENCE_PROOF.md`), но `A4_protocols_h323/README.md` (957 строк) я не читал — см. §A.3.
4. **Если наш `media_policy.rs` всё-таки вызывается** (через макрос/генерацию, которую не поймал grep) — тезис T10 падает. Проверено grep’ом по `crates/` (0 вызовов `resolve(`), но не проверено по `tests/`/`scripts/`.

---

## 1. Механизмы-кандидаты (12) и что из них реально даёт выигрыш на 36 ядрах

**Ответ на вопрос Lead (§7.1 плана):** топ-7 по (выигрыш CPU/ёмкость) × (готовность нашего кода) × (риск) =
**M1 per-output encode → M3 encoder farm/дедуп → M2 разрешение раскладки → M4 resource model → M7 I420/page-allocator → M6 audio buses → M5 backpressure-очереди.**
Остальные (M8–M12) — качество/устойчивость/наблюдаемость, не «скорость».

Оценка «эффект» даётся как **направление и метрика проверки**, а не как выдуманный процент: замеров на этой ВМ я не делал (запрет на сборки/нагрузку). Калибровка — задача `perf-lab` (методика `PLAN_ORCHESTRATION_v0.md:135-167`).

### M1. Per-abonent команды кодирования (per-output encode)

| Поле | Значение |
|---|---|
| **Эталон** | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:29-31` (`ABOOK BITRATE/FRAME SIZE/FRAME RATE/ACODEC/VCODEC`), `:100-107`, `:201` (R1 = «исходящий поток конкретного участника»); `[TC]09_MEDIA_CORE/vsm_spec.md:41-49` (**output = отдельный кодировщик на каждого получателя**, `Channel #%d has %u outputs.`) |
| **Как устроено** | На каждого получателя — свой `output`/энкодер, поэтому битрейт, кодек, fps и способ вписывания (`SET RENDER AUTO|FILL|FULL`) индивидуализируются, а полотно раскладки — одно. `Channel #%d has %u outputs.` — это и есть счётчик отдельных кодировщиков |
| **Что у нас** | Композит **один на всех**: `[OUR]crates/vks-media-mcu/src/engine.rs:1087` (`composite_frame`), `:790`/`:807` (`tick_video_for_viewer`/`frame_for_viewer` возвращают `Arc<Frame>` — **несжатый** кадр, без энкодера); per-subscriber encode = `proposed` ADR-0029 `[OUR]docs/decisions/DECISIONS.md:36`; реальные кодеки — только на gateway-пути: `[OUR]crates/vks-server/src/gateway_video.rs:477-489` (по паре FFmpeg subprocess `encoder`+`decoder` на одно SIP/H.323-соединение) |
| **План переноса** | 1) Ввести `OutputProfile {w,h,fps,codec,bitrate,fit}` в `[OUR]crates/vks-core/src/ports/media_engine.rs:165-183` (`LayoutDescriptor`) и в `RoomState` (`engine.rs:45`). 2) `tick_video_for_viewer` → возвращать не `Frame`, а ключ канала (`ChannelKey`), по которому `[OUR]crates/vks-media-mcu/src/codec_worker/` спавнит/переиспользует энкодер (`codec_worker/mod.rs:184` `SlotConfig`, `:203` `default_720p`). 3) На gateway-пути `gateway_video.rs:304 spawn_codec` заменить на общий пул (`transcoder_runtime.rs:111 pool()`), чтобы не было пары процессов на вызов. 4) **Клампы взять из эталона, а не шире:** `bitrate ∈ [64…8192]` кбит/с, `fps ∈ [15…60]`, `width/height` не валидируются — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:159-161`; у нас сейчас `media_policy.rs:77-82`: `[32…50 000]`/`[1…60]` — шире эталона в 6 раз по битрейту, надо согласовать (для локальной записи 4K потолок может быть выше, но **per-abonent egress** — 8192) |
| **Эффект на 36C E5-2699 v3** | Убирает «N процессов FFmpeg на N вызовов» (сейчас — по 2 процесса на gateway-сессию) и даёт **адресное** управление битрейтом. Метрика проверки: CPU/сессия на матрице M3/M4 (цель P3 плана — ≥30 %) |
| **Риск/цена** | M (2–5 дней). Риск: кодирование per-output увеличивает число энкодеров; **без M3/M4 это регрессия CPU**, а не выигрыш. Зависимости: ADR-0029 (per-viewer), единый пул воркеров |

### M2. Три уровня разрешения: глобальный / per-abonent / **per-layout** (R0/R1/R2)

| Поле | Значение |
|---|---|
| **Эталон** | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:194-226` (R0 `SET FRAME SIZE`, R1 `ABOOK FRAME SIZE`, R2 `LAYOUT FRAME SIZE`; help-строки дословно); `:163-175` (формулы: кадр = L3/L4 resolution, fps = min(…, L2=30)); `[TC]00_DOCUMENTATION/LIVE_VALUES.md:56-57` (исходящее разрешение задаёт раскладка, глобальный потолок 4K) |
| **Как устроено** | Разрешение живёт в `video_layouts._.payload.resolution` и применяется к каналу: `Channel #%u (%s) default resolution set to %ux%u` (`[TC]09_MEDIA_CORE/vsm_spec.md:48`). Участнику разрешение не назначается; ему назначаются битрейт/fps/кодек/fit. Дословно из эталона: «"разное разрешение каждому участнику" в классической модели **отсутствует**. Индивидуализируются битрейт, кодек, fps и способ вписывания (`SET RENDER AUTO|FILL|FULL`). Размер картинки — свойство раскладки» — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:181-185`; «картинка в СЛОТЕ — свойство РАСКЛАДКИ, не участника: `rendering_width/height` общий для всех connection'ов раскладки» — `:167-168`. Формулы, где разрешение выводится из раскладки: `width_out = min(profile.w, engine.frame.size.w, slot_w × layout_total_w)`, `height_out = min(profile.h, engine.frame.size.h, slot_h × layout_total_h)`, `fps_out = min(profile.fps_limit, engine.max_frame_rate=30)` — `:284-293` |
| **Что у нас** | Полотно **жёстко 1920×1080**: `[OUR]crates/vks-media-mcu/src/frame.rs:13-15`, `compositor.rs:20`; `vks_layout::Layout` **не имеет** поля resolution — `[OUR]crates/vks-layout/src/core.rs:206-213` (`version/kind/slots`); геометрия принимает w/h лишь как **аргументы вызова**: `preset.rs:632`, `codian.rs:727`; `LayoutDescriptor` без resolution — `[OUR]crates/vks-core/src/ports/media_engine.rs:165-183`; canvas подставляется константой `engine.rs:140` |
| **План переноса** | 1) `Layout { resolution: Option<Resolution> }` + `LayoutDescriptor.resolution` (аккуратно: ADR-0030/0032 фиксируют контракт раскладки — нужен новый ADR, иначе нарушение). 2) `McUEngine::tick_video*` берёт цель кодирования из layout, а не `CANVAS_SIZE`. 3) `gateway_video.rs:228 SIZE_LADDER` + `video_profile` (`gateways.rs:441-473`) уже умеют клампить по `max-fs` — подключить к layout-resolution |
| **Эффект на 36C** | 720p-раскладка = 0.44 от пикселей 1080p; при том же бюджете CPU — примерно вдвое больше одновременных композитов/энкодеров (точно — через `c_device`, `PLAN…:144-151`). Отдельный выигрыш: 4K-раскладки становятся возможны как явный выбор, а не как «всё или ничего» |
| **Риск/цена** | M. Риск: смена разрешения раскладки на лету = перезапуск энкодера + пересборка SDP (`gateway_video.rs`, `rtp_bridge.rs:613-651`); ломает ADR-0030/0032 при неаккуратном добавлении поля |

### M3. Модель «канал → outputs» и ферма энкодеров с дедупом (encoder farm)

| Поле | Значение |
|---|---|
| **Эталон (TrueConf)** | `[TC]09_MEDIA_CORE/vsm_spec.md:30-49` (`Conference → Layout=Channel → Stream/Output`; `Channel #%d has %u outputs.`), `:268-280` (чек-лист: дерево каналов, сокращение outputs при дефиците) |
| **Эталон (Vinteo, независимое подтверждение идеи)** | `[VI]12_Архитектура_микшера_MCU.md:186-192` (`_encs`: `clientscnt`, `clients`, `pk[1000]` кольцевые буферы), `:569` (`MultiVideoEncoder::attachClient`/`detachClient`) — «один энкодер → много клиентов» реализовано и в Vinteo, т.е. это отраслевой, а не TrueConf-специфичный приём |
| **Как устроено** | Выход канала — сущность с параметрами (w,h,fps,codec,bitrate). Одинаковые outputs переиспользуют один энкодер; при дефиците ресурсов outputs сокращаются. |
| **Что у нас** | «Нет»: один композит на комнату, `engine.rs:1087`; дедупа по параметрам нет; `codec_worker` не является пулом кэшируемых энкодеров (`codec_worker/ffmpeg.rs:24-40` — обёртка над процессом) |
| **План переноса** | `struct Channel { layout_version, w,h,fps, outputs: BTreeMap<OutputKey, Vec<SubscriberId>> }` в `vks-media-mcu` (модуль `channel.rs`), `EncoderFarm { encoders: HashMap<EncoderKey, Arc<Mutex<Worker>>>, refs }`. `EncoderKey = (w,h,fps,codec,bitrate,profile,level)`; одинаковые ключи → 1 энкодер, N RTP-подписчиков. Инвалидция по `layout.version` |
| **Эффект на 36C** | Главный множитель ёмкости: для 8 зрителей одной 720p-раскладки с одинаковыми профилями — 1 энкодер вместо 8; различающиеся профили (per-abonent bitrate) добавляют энкодеры, но их число ограничено числом **различных** профилей, а не числом участников |
| **Риск/цена** | M/L. Риск: кэш энкодеров с разными bitrate даёт N энкодеров → нужен лимит (M4) и eviction; FFmpeg-процесс на энкодер — дорогая сущность (spawn ~мс), нужен прогрев/пул |

### M4. Модель ресурсов: единицы 1/16, стоимость от класса разрешения × fps × outputs, калибровка, деградация

| Поле | Значение |
|---|---|
| **Эталон** | `[TC]09_MEDIA_CORE/resource_model.md:33-45` (счётчики в 1/16, вывод `(raw+8)>>4`), `:73-86` (`idx = class−21`; `cost_enc = max(16,(N*C+B)*fps/16)`, `cost_one = max(16,A*fps/16)`), `:228-241` (states normal/restricted/insufficient; порядок деградации: сократить outputs → отключить поток → ограничить декодеры → сменить формат); `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:307-373` (**дизассемблированная первооснова**: `vtsrv@0xa64770`, 80 классов `21..100`, массивы `A/B/C` по `VSM+0x4E0/0x4E4/0x4E8`, единица = 1/16, `16` = минимум; NNLSM-калибровка по базису `[w,h,w·h]`, target `983040/fps`, `load = (a·w + b·h + c·w·h)·(fps/30)`, `units = 32768` = 100 %, `encoder_limit = (total_load+8)>>4`; **вывод 106**: `total_load ≈ (106<<4)−8 = 1688` единиц), `:228-238` (строки деградации движка: «VSM resource shortage: optimizing channel #%d», «Critical resource shortage. Restricting decoders.», «Stream #%d muted by performance manager.»), `:624-631` (лимит 106 = **калибровка**, а не лицензия) |
| **Как устроено** | `required`/`current` энкодеров и декодеров в 1/16; `limits.performance` = измеренный предел железа; деградация — **на уровне канала**, а не «качества каждого». Триггер — счётчики vsm (энкодеры `+0x5ac`, декодеры `+0x5e4`) против лимита (`:237`) |
| **Что у нас** | «Нет»: только `McuRenderStats { frames, dropped, last_pass_us }` — `[OUR]crates/vks-media-mcu/src/engine.rs:32-42`; лимитов/деградации нет; в `[OUR]configs/default.toml` (47 строк) нет ни `max_calls`, ни лимитов кодировщиков; единственный «лимит» — `LimitNOFILE=65536` (`deploy/systemd/vks-mcu.service:37`) |
| **План переноса** | Новый модуль `crates/vks-media-mcu/src/resource.rs`: `units = 1/64` (наши размеры мельче — 1/16 может «округлить в ноль» на мелких ячейках; это **осознанное отступление**, требует ADR), `cost(w,h,fps)` с таблицей классов разрешения (аналог `[TC]…resource_model.md:90-140`), при старте — калибровка на этой ВМ (замер пропускной способности энкодеров при 720p30/1080p30 **в рамках `perf-lab`**, не здесь), экспорт в `/metrics` и в API. Деградация: `Outputs → mute stream → decoder limit → format change` |
| **Эффект на 36C** | Без модели per-output encode (M1/M3) **нельзя включать** — иначе «thundering herd» энкодеров и рост latency. Модель даёт предсказуемую ёмкость и мягкую деградацию вместо дропов; целевые числа — из `pidstat`-методики Lead |
| **Риск/цена** | M. Риск: константы TrueConf (106/1696) **не переносятся** — это калибровка на их CPU; копирование = гарантированная ошибка. Явно помечаю это как «не переносить значение, переносить метод» |

### M5. Backpressure: кольцевые очереди заданий и две очереди сообщений

| Поле | Значение |
|---|---|
| **Эталон** | `[TC]09_MEDIA_CORE/scheduler_spec.md:20-33` (jobs 65 536, ready 9 728, dependencies 262 144, write-through 1 048 576, discardable 65 536; regular/urgent = 255 сообщений / 79 871 Б), `:41-58` (семантика: discardable можно терять, write-through — нельзя), `:62-77` (воркеров 37, SIP/H.323 — свои worker-loop'ы) |
| **Как устроено** | Две независимые подсистемы: планировщик **заданий** (медиаработа, пул воркеров) и очереди **сообщений** (сигнализация regular/urgent). Ограничение «ready < jobs» — намеренный backpressure |
| **Что у нас** | Частично и не там: `unbounded_channel` в gateway DTMF — `[OUR]crates/vks-server/src/gateways.rs:243-249`, `:1130`, `:2566`, `:3243`; `bounded(64)` только для одного event-канала `:3084`. Медиа-очередей с границами нет; tokio runtime — общий |
| **План переноса** | 1) Заменить `unbounded` на `bounded` там, где продюсер — сеть (dtmf/events). 2) Отдельная **urgent**-полоса для сигнализации (SIP/H.323/Pli) и **discardable** для телеметрии. 3) Воркеры: не копировать «37», а связать с `num_cpus` (=36 физ. ядер, SMT off — `PLAN…:26`) и измерить (гипотеза «37 ≈ ядра+1» **не проверена**: `scheduler_spec.md:77,84,249` честно говорит, что связь не установлена) |
| **Эффект на 36C** | Защита от tail-latency и OOM при burst (Lead-сценарий «burst 20/с», `PLAN…:158`); без этого per-output encode даст пики, а не пропускную способность |
| **Риск/цена** | S/M. Риск: `bounded` без обработки `Full` роняет события → нужен явный policy (drop-oldest для discardable, block для write-through) |

### M6. Аудио: шины каналов (SPEAKERS/LISTENERS), политика AMS, per-call кодек, задержка ≤300 мс

| Поле | Значение |
|---|---|
| **Эталон** | `[TC]09_MEDIA_CORE/asm_spec.md:25-38` (`AudioMixer → AudioChannel[] → SPEAKERS/LISTENERS`, дефолтная шина, `Switching peer %s to default audio mixer`), `:53-71` (`SET AMS SPEECHLESS|SPEECH|MULTILINGUAL`, `ASPEAKER`/DSI), `:75-84` (задержка обработки аудио, live `asm.delay.max=300 мс` — `[TC]00_DOCUMENTATION/LIVE_VALUES.md:53`), `:167-175` (чек-лист) |
| **Как устроено** | Участник может быть speaker в нескольких шинах и listener в нескольких (мультиязычные конференции); микширование в PCM, на выходе — **перекодирование в кодек каждого вызова**; политика AMS управляет тем, сколько говорящих попадает в шину. Обязательность per-call кодека зафиксирована в формуле эталона: «аудио = микшер ОДИН на конференцию (или на `audiochannel`), кодек — **ВСЕГДА per-call**; перекодирование неявное и обязательное (микшер в PCM → свой кодек у каждого)» — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:171` |
| **Что у нас** | Частично, без шин: `[OUR]crates/vks-media-mcu/src/audio_mixer.rs:18` (48 кГц), `:56 mix`, `:62 mix_minus` (N-1), `:70 mix_and_mix_minus`; VAD 20 мс/дебаунс 500 мс — `vad.rs:10-16`, `:91`; активный спикер/хайстерезис — `engine.rs:961-971`; `set_important_participant` — `engine.rs:751`; **шины SPEAKERS/LISTENERS и политики AMS — «нет»** |
| **План переноса** | `AudioBus { speakers: BTreeSet<Pid>, listeners: BTreeSet<Pid>, codec_policy }` в `RoomState`; `AudioMixer::mix_bus(bus)`; AMS-политика как enum (speech/speechless/multilingual), активный спикер — существующий VAD; для каждого listener — mix-minus своей шины + свой кодек (gateway уже кодирует per-call: `gateway_video.rs`-аналог для аудио — `gateways.rs:925+ spawn_rtp_media_bridge`) |
| **Эффект на 36C** | Аудио дёшево (<1 ядра на десятки потоков), выигрыш — **качество/сценарии** (мультиязычность, N-1 без эха), а не CPU. Но даёт основание не тащить «один микс на конференцию» в SFU-режим |
| **Риск/цена** | S/M. Риск: перекодирование per-call требует кодека на каждого listener (у нас есть Opus/G.711/G.722/G.726/G.728/G.7231); задержка буфера нужно считать (цель 300 мс как эталон) |

### M7. IPROC: точный аллокатор I420, страничный пул, счётчики памяти, режимы fit

| Поле | Значение |
|---|---|
| **Эталон** | `[TC]09_MEDIA_CORE/iproc_spec.md:26-50` (формула `size_I420 = w*h + 2*ceil(w/2)*ceil(h/2)`, валидация, «2 буфера на канал»), `:56-85` (счётчики `internal/codec/user/total/memory`; live: total 76, user 72, memory 442 765 736 Б ≈ 422 МиБ при `images.total = 76` ⇒ **≈5.8 МБ/кадр**; независимая проверка той же цифры: `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:364-365`), `:89-172` (`SET RENDER AUTO|FILL|FULL`, рамки/имена/водяные знаки/фон) |
| **Как устроено** | Страничный аллокатор кадров, отдельные категории буферов (внутренние/кодек/приложение), точная валидация размера, режимы вписывания |
| **Что у нас** | Формула **неточная** для нечётных размеров: `[OUR]crates/vks-media-mcu/src/frame.rs:33-41` проверяет `w*h*3/2` (для odd-size расходится с эталонной формулой). Канвас аллоцируется **на каждый композит** (3 МБ YUV): `compositor.rs:47-56` — там же честно помечено «per-room scratch-буфер вынесен в S20». Режим вписывания — только letterbox: `compositor.rs:38-73`; счётчиков памяти нет |
| **План переноса** | 1) `Frame::i420_len(w,h) = w*h + 2*ceil(w/2)*ceil(h/2)` + валидация в `Frame::new`. 2) `ScratchPool` (per-room или per-worker) в `compositor.rs` — переиспользовать канвас вместо `vec![...; 3MB]` на тик. 3) Категории счётчиков `internal/codec/user` в `McuRenderStats` + `/metrics`. 4) `fit: letterbox | fill | crop` как свойство слота (у нас `crop` описан в `MEDIA_PIPELINE.md:92`, но в `compositor.rs` не реализован) |
| **Эффект на 36C** | Убирает 1 аллокацию 3 МБ + заливку на тик × комнат; при 20 комнатах ×30 fps это заметная доля allocator/bandwidth. Точная формула важна, как только появятся odd-size канвасы (720×576 SD-раскладки) |
| **Риск/цена** | S. Риск: пул канвасов = память 3 МБ × комнат (20 комнат = 60 МБ, приемлемо); ошибка в формуле ломает валидацию существующих тестов — прогон `cargo test` обязателен после (сейчас не делаю по правилам) |

### M8. Транспортная стойкость RTP: NACK/RTX, FEC, jitter-буферы, RTCP SR/RR, синхронизация

| Поле | Значение |
|---|---|
| **Эталон** | `[VI]12_Архитектура_микшера_MCU.md:161-184` (`RTPSession`: 1 072 784 Б, `fdec: FECDecoder`, `jb/jbj/jbs: _jb ×3`, `rtlrx: timeval[65536]`), `:543-571` (`rtpsession.cpp` — 16 функций/73 998 Б; `FECEncoder`; `sync.cpp`), `[VI]14_Реконструкция_исходников_MCU.md:763-796` (`rtpsession.cpp` 40 функций: jitter, NACK/RTX, SRTP, RTCP SR/RR); FEC в `mcu` **статически**: `[VI]07_Адвокат_дыявола_проверка_консенсуса.md:21` (160 символов `of_*`, OpenFEC 1.4.2, сломан только неиспользуемый `libopenfec.so`) |
| **Как устроено** | Собственный RTP-стек с тремя jitter-буферами на сессию, FEC-декодером, NACK/RTX и RTCP-отчётами; синхронизация видео/аудио отдельным объектом |
| **Что у нас** | На SFU/WebRTC — есть: `[OUR]crates/vks-media-sfu/src/webrtc.rs:5` (NACK/RTX/FEC в транспорте str0m), `:209` (rtx), `webrtc_room.rs:10`. На **gateway/MCU** — «нет»: в `[OUR]crates/vks-gateway-sip/src/` нет jitter-буфера и NACK/RTX (grep по `jitter|Jitter` → 0 совпадений); FEC не генерируется |
| **План переноса** | 1) Jitter-буфер в `crates/vks-gateway-sip/src/rtp_bridge.rs` (сейчас там только сборка/разбор SDP и RTP-мост). 2) RTX/NACK на MCU-egress — через общий RTP-слой, а не копируя Vinteo. 3) FEC (RFC 5109/6015) — по итогам `net-resilience`, не тащить OpenFEC-код |
| **Эффект на 36C** | Устойчивость при 2 % loss (сценарий Lead `PLAN…:158`, P4). CPU-цена FEC/RTX умеренная, но требует своей полосы; на 36 ядрах это не «скорость», а SLA-класс B/C |
| **Риск/цена** | M/L. Риск: дублирование с str0m; правильнее расширить общий RTP-слой, чем иметь два. Зависимость от `net-resilience` (владелец темы) |

### M9. Сетевые параметры и конфиг-гигиена Vinteo (sysctl, таймауты, матрица портов, аудит мёртвого конфига)

| Поле | Значение |
|---|---|
| **Эталон** | `[VI]13_Раскладки_Надписи_Сеть.md:770-800` (`/etc/sysctl.conf`: `shmmax=68719476736`, `shmall=4294967296`; `rc.local`: `rmem_max/wmem_max=5838860800`, `rmem_default/wmem_default=502400000`, `/proc/sys/net/ipv4/udp_mem='2838860800 2258291200 4677721600'`, `txqueuelen 1000`, `lo mtu 1600`, `sched_latency_ns=1000000`, `sched_min_granularity_ns=500000`, `sched_wakeup_granularity_ns=500000`, `sched_nr_migrate=2`, `vfs_cache_pressure=10000`); `:917-947` (матрица портов); `[VI]02_SIP_H323_цепочка_вызова.md:274-286` (порты 5060/5061/3478/1720/1024-65000/20000-65000/40000-50000/5000/5038/3306); `[VI]08_Функциональная_карта_подсистем.md:251-283` (`rtptimeout=60`, `bombercount 70/15000`, call_duration 30 с, 3 попытки, интервал 60 с, JWT 3600/3720) |
| **Как устроено** | Настройка ОС под RTP-нагрузку + жёсткая матрица портов + дисциплина таймаутов. Всё это **не зависит** от проприетарного ядра и потому переносимо |
| **Что у нас** | Только `LimitNOFILE=65536` (`[OUR]deploy/systemd/vks-mcu.service:37`); sysctl-профиля нет; порты: WebRTC 43000-43100 (`vks-mcu.service:32-33`), media 40000 (`configs/default.toml:13`), SIP 5060/1720 (Lead-проверка `PLAN…:32`); матрицы портов как документа нет |
| **План переноса** | Документ `docs/architecture/PORT_MATRIX.md` + `scripts/sysctl-mcu.conf` с **подмножеством**: `net.core.rmem_max/wmem_max` (не 5.8 ГБ! — на 56 GiB RAM это опасный оверкилл), `net.ipv4.udp_mem`, `txqueuelen`, `net.core.somaxconn`, `sched_*` (только после замера `perf-lab`). Таймауты: `rtptimeout≈60 с` как watchdog мёртвого медиапотока; лимиты дозвона (30 с/3 попытки) — в админ-модель |
| **Эффект на 36C** | Снижение дропов в сокете при burst; **не** «ускоряет» MCU. На KVM-стенде часть sysctl — no-op (гипервизор), это надо проверить |
| **Риск/цена** | S (документ/скрипт) + риск: `sysctl -w` на живой ВМ запрещён мне; менять — только Lead по согласованию. Значения Vinteo 2013 г. (5.8 ГБ буферы) **не копировать** (см. §3) |

### M10. SIP/H.323-цепочка Vinteo: что реально переносимо (и что нет)

| Поле | Значение |
|---|---|
| **Эталон** | `[VI]02_SIP_H323_цепочка_вызова.md:101-131` (Asterisk `chan_sip`+`app_mixer` → AI-протокол TCP:5000 → `mcu`), `:205-290` (схема трансдинга, таблица «этап → файл → порт → протокол»), `:371-461` (полный список путей стека), `[VI]01_Движок_MCU.md:203-451` (точные пути SIP/H.323), `[VI]14_Реконструкция_исходников_MCU.md:763-796` (карта `src/*.cpp` микшера) |
| **Как устроено** | Asterisk 1.4.x-форк (вендорский ярлык «1.2.1»/«31.2.8», `[VI]07_Адвокат…:13-16`) + `chan_h323` (OpenH323Plus 1.15.6/PWLib 1.8.7, `[VI]04_Пакеты_и_библиотеки_REPORT.md:226-233`) + собственный `app_mixer` по закрытому AI-протоколу + отдельный процесс `mcu` |
| **Что у нас** | Своя цепочка: SIP UAS/UAC (`uas.rs` 1089 строк, `uac.rs` 706), RTP-мост (`rtp_bridge.rs` 1769), кодеки (G.722/G.726/G.728/G.723.1/AAC-LATM/H.265/Opus); H.323 — Python-сайдкар на `pycrate==0.8.1` (`h323_listener.py:1-16`, `h323_asn.py:1-16`), **не** H323Plus-FFI, хотя ADR-0007 (`docs/decisions/DECISIONS.md:14`) это обещает |
| **План переноса** | Переносить **знание, не код**: матрица портов (M9), `rtptimeout`-семантика, H.235-аутентификаторы (SimpleMD5/CAT — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:949-950` для TrueConf и `[VI]08…:125-160` про мёртвый T.38 — не тащить), правила interop (Vinteo отбрасывает H.265 в H.323: `[VI]08…:125-160`, п.25; TrueConf отвечает PCMU, но шлёт PCMA — наш фикс уже есть: `[OUR]docs/progress/MASTER_TASK_LIST.md:59-68` T07b). Обновить ADR-0007 под фактическую реализацию (pycrate) — иначе документ врёт |
| **Эффект на 36C** | Нулевой по CPU; выигрыш — interop-надёжность с legacy-терминалами (Polycom/LifeSize/Huawei), без которой бенчмарк 20 устройств нерепрезентативен |
| **Риск/цена** | S (документация) / M (если менять ADR). Риск: соблазн «взять Asterisk как у Vinteo» — это 2012-й, EOL-ветка, запрещено (§3) |

### M11. Media-framework element API Cisco (call-table lifecycle, 24 элемента)

| Поле | Значение |
|---|---|
| **Эталон** | `[CS]10_MEDIA_FRAMEWORK_ELEMENTS.md:1-34` (24 элемента: analysis, audio_dec/enc/mixer/streamer/file/grab/token_reader, mari_dec/enc, pipe_sink/source, rtp_muxer, sync_pipe, video_bitmapifier/canvas/channel/compose/decoder/encoder/file/memstreamer/modifier/scaler; lifecycle `create/destroy/process_message/periodic_run/periodic_tasks/run/get_input/get_output/call_table`); `[CS]15_MIXER_ELEMENTS_API.md:6-35` (`mf_video_compose.cpp:57/74/83/90/97/1011/1048/1172`; `mf_audio_mixer.cpp:109/128/136/177/287/294/301/308/318`); `[CS]09_MEDIA_FRAMEWORK_API.txt` (547 символов `mf_*`) |
| **Как устроено** | Конвейер из однородных объектов с типовым контрактом портов входа/выхода и периодикой; `mf_load()` регистрирует 24 объекта. Управление — сообщениями (`process_message`) |
| **Что у нас** | «Нет»: `Compositor`/`AudioMixer` — статические структуры (`compositor.rs:24`, `audio_mixer.rs:22`), конвейер = прямые вызовы в `engine.rs::tick*`; портов/сообщений/периодики нет |
| **План переноса** | Не копировать 24 элемента, а взять **форму контракта**: `trait MediaElement { fn process(&mut self, msg: MediaMsg) -> …; fn periodic(&mut self, now_ms: u64); fn inputs(&self) -> &[Port]; fn outputs(&self) -> &[Port]; }` для `Compositor`, `AudioMixer`, `EncoderFarm`, `VadDetector`. Это даёт единый instrumentation-хук (M12) без переписывания всего |
| **Эффект на 36C** | Нулевой напрямую; выигрыш — возможность прицельно профилировать/заменять элементы и вставлять zero-copy/SIMD без переписывания `engine.rs` (монолит 2540 строк — `[OUR]crates/vks-media-mcu/src/engine.rs`) |
| **Риск/цена** | M. Риск: over-engineering и конфликт с инвариантом «MediaEngine трейт» (`[OUR]docs/architecture/MEDIA_PIPELINE.md:8-24`) и ADR-0016 (Sans-I/O). Брать **минимум**: порты + periodic, без message-шины на первом шаге |

### M12. Телеметрия и runtime-параметры движка (Cisco) как каркас наблюдаемости

| Поле | Значение |
|---|---|
| **Эталон** | `[CS]06_MEDIA_TELEMETRY_KEYS.md:1-20` (115 `key_*`, 14 `report_*`: `report_VideoChannel`, `report_VideoStreamIngress/Egress`, `report_SessionIngress/Egress`, `report_KeyFrameRequest`, `report_LostPacket`, `report_MariFec`, `report_AudioSelector`, …), `[CS]18_MCU_RUNTIME_PARAMETERS.md:8-33` (`Kokoro::Cmgr::Parameters`: `m_resolution` enum 1080p/720p/audio, `m_max_calls`=500, `m_call_bandwidth`=5000 кбит/с, `m_sip_tcp_port`=5060, `m_sip_tls_port`=5061), `:67-73` (`MediaFrameworkManagerParameters`: `max_media`, `max_objects`, `max_compose_report_cells`, **`max_mixer_slots`** = «размер микшера») |
| **Как устроено** | Телеметрия — это отчётные объекты по типам (ingress/egress на поток и сессию, каналы, потерянные пакеты, keyframe-запросы), а лимиты движка — явные параметры с CLI-границей, а не «магические константы» |
| **Что у нас** | Тонко: `McuRenderStats` — 3 поля (`engine.rs:32-42`); Prometheus есть (`prometheus 0.14` в `[OUR]Cargo.toml`), метрика тика — `observe_mcu_tick` за флагом `VKS_MCU_TICK_METRICS` (`engine.rs` модульная документация); лимитов вида `max_calls`/`max_mixer_slots` нет (`configs/default.toml` — 47 строк, только `media_listen_addr`/`candidate_addr`/`default_room_mode`/`ice_servers`) |
| **План переноса** | 1) Ввести `McuRuntimeLimits { max_calls, max_rooms, max_mixer_slots, max_encoders }` в `vks-common` конфиг + fail-closed валидация. 2) Телеметрия: набор счётчиков по образцу report-типов (ingress/egress на поток, keyframe-requests, lost-packets, fec) в `/metrics` + `/api/v3` диагностику. 3) Не тащить имена 1:1 — взять **семантику** (иначе получим мёртвые ключи). 4) **Бонус-механизм из TrueConf, проверенный численно:** битрейт записи `bitrate = W·H·fps·bpp/1000`, `bpp = {BEST:0.1, GOOD:0.067, ECONOMY:0.033}`; проверка «1920×1080@30 → 6220 / 4167 / 2052 кбит/с — совпадение до 0.01 %» — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:173-174`. У нас `vks-recorder` не содержит модели битрейта вообще (grep `bpp|bitrate` по `crates/vks-recorder/src/` → 0). Это дешёвый и точный перенос (S) |
| **Эффект на 36C** | Без телеметрии нельзя ни калибровать `c_device`, ни доказать ≥30 % в P3. Прямого CPU-эффекта нет (сэмплирование) |
| **Риск/цена** | S/M. Риск: кардинальность Prometheus (метки по участникам) — нужен bounded набор |

### 1.13. Сводная матрица: механизм → фаза плана Lead → что уже есть → цена

| # | Механизм | Фаза (`PLAN_ORCHESTRATION_v0`) | Уже есть у нас | Цена | Владелец темы |
|---|---|---|---|---|---|
| M1 | per-output encode (ABOOK-аналог) | **P3** | нет (композит один на всех; gateway — по процессу на вызов) | M | `ref-miner` + `media-core` |
| M2 | разрешение = свойство раскладки (R0/R1/R2) | **P3** | нет (канвас-константа) | M | `ref-miner` + `media-core` |
| M3 | канал/outputs + ферма энкодеров | **P3** | нет | M/L | `media-core` |
| M4 | resource model + деградация | **P3** | нет (только `McuRenderStats`) | M | `media-core` + `perf-lab` (калибровка) |
| M5 | bounded/urgent/discardable очереди | **P3/P4** | частично (`bounded(64)` в одном месте) | S/M | `net-resilience` |
| M6 | аудио-шины + AMS + per-call кодек | P3 | частично (mix/mix-minus/VAD/active speaker) | S/M | `media-core` |
| M7 | I420-формула + page pool + fit-режимы | **P3** | нет (только letterbox; аллокация на тик) | S | `media-core` |
| M8 | NACK/RTX/FEC/jitter (Vinteo RTP-сессия) | **P4** | SFU-часть есть (str0m), gateway/MCU — нет | M/L | `net-resilience` |
| M9 | sysctl/порты/таймауты/гигиена (Vinteo) | P4/P0 | `LimitNOFILE` есть; матрицы/sysctl — нет | S | `net-resilience` + `regress-hygiene` |
| M10 | SIP/H.323 interop-знания (Vinteo) | P2 | своя цепочка есть; ADR-0007 устарел | S/M | `proto-chain` |
| M11 | element API (Cisco) | P3/P8 | нет (статические структуры) | M | `media-core` |
| M12 | лимиты + телеметрия (Cisco/TrueConf) | P3/P5 | тонко (3 поля статистики) | S/M | `ref-miner` + все |

### 1.14. Чего в эталонах НЕТ, а нам нужно (чтобы не создавать ложного впечатления полноты)

Эталоны покрывают **модель, API и наблюдаемость**, но **не** покрывают то, что Lead вынес в P3/P4:

1. **SIMD-микшер и SIMD-масштабирование под AVX2** — ни TrueConf (их движок на x86 с IPP/Intel Compiler, `[VI]04…:19-29`, `[TC]MASTER_ARCHITECTURE.md:191-192`), ни Cisco (собственный scaler) не дают переносимого кода; у нас `compositor.rs` — nearest-neighbour на скалярах (`compositor.rs:73 blit`). Это наша инженерная работа, не майнинг.
2. **Zero-copy / shared composite canvas** — Cisco имеет элемент `mf_video_canvas` (`[CS]10…:26`), но без реализации; TrueConf имеет «полотно одно + outputs» (`vsm_spec.md:47-49`) как модель, не как код.
3. **worker-per-core + cgroup/IRQ-affinity** — ни один эталон не документирует привязку к 36 ядрам; это зона `perf-lab`.
4. **recvmmsg/sendmmsg, SO_REUSEPORT, io_uring** — не найдено ни в одном прочитанном артефакте (это не значит «нет»: не искал специально — **не проверено**).
5. **Наш инвариант прозрачности MCU/SFU** (ADR-0002/0003) — у эталонов аналога нет: у Cisco MCU = отдельный продукт без SFU-режима, у TrueConf/Vinteo SFU-режим не документирован.

⇒ Майнинг даёт **модель и параметры** (M1–M12), но **не** даёт готового «быстрого медиаядра»: скорость — это P3-инженерия (`media-core`), а майнинг задаёт ей правильные требования и пределы.

---

## 2. Таблица пакетов/версий/параметров эталонов → что именно применить у нас

### 2.1. TrueConf MCU 2.2.1.66-deb12 (главный донор медиамодели)

| Компонент / параметр | Версия/значение (эталон) | Источник | Применять у нас? Что именно |
|---|---|---|---|
| Движок | `vtsrv` 119 МБ, stripped; `veng 1.5.5.2`, product 2.2.1.0 | `[TC]09_MEDIA_CORE/asm_spec.md:3`; `LIVE_VALUES.md:45,79` | Нет (проприетарный). Применяем **протокол/модель**, не код |
| web_api | Python 3.8 (Nuitka-бинарь), `internal/engine/*.py` — транслятор в команды движка | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:27`, `LAYOUT_CHAIN_SPEC.md:3` | Нет. Но применяем **принцип**: контрольная плоскость = транслятор «модель → команды движка», а не сам движок |
| БД | PostgreSQL 15, порт 5433 | `[TC]04_DEPLOYMENT/env.conf:3`, `config.yaml:8`, `ctrlmachine_lin.xml:3` | Да, если пойдём в PG-профиль; сейчас у нас SQLite/PG через sqlx — совместимо. Схему TrueConf **не копировать** (109 схем/95 таблиц — своя) |
| Порты | `vile` unix-socket + 5080; webapi 8081; redirector 6081; postgres 5433 | `[TC]04_DEPLOYMENT/config.yaml:8-20`, `env.conf:3`, `web.toml:19-22` | Да, как ориентир для **разделения плоскостей** (engine unix-socket ↔ web api ↔ ui). У нас всё в одном процессе — архитектурное расхождение, фиксирую |
| Лимиты ОС | `LimitNOFILE=65536`, `LimitCORE=infinity`, `TasksMax=infinity`, `Restart=on-failure` | `[TC]04_DEPLOYMENT/tcmcu-engine.service:16-19` | Да: у нас уже `LimitNOFILE=65536` (`deploy/systemd/vks-mcu.service:37`), добавить `TasksMax=infinity`/`LimitCORE` по политике (нужен Lead) |
| Разделение конфигов | `env.conf` (окружение) + `cpu.conf` (генерируется `numagen2.py`, NUMA) | `[TC]04_DEPLOYMENT/tcmcu-engine.service:10-11`, `tcmcu-cpu-update.service:9` | Да, идея: NUMA-профиль как **генерируемый** конфиг. У нас 1 NUMA (`PLAN…:26`) — польза мала, но паттерн «генерируемые env» переносим |
| Видеокодеки | БД 8: H261✗ H263✗ H263+ H264 H264-HIGH H264-SVC H265✗ VP8; лицензия режет `h264-svc`, `vp8` | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:43-48`, `LIVE_VALUES.md:39-40,72-73` | Да — как **реестр** для нашего capability-модуля (у нас уже есть: `master task list` T06a, `gateway_video.rs:143-161`) |
| Аудиокодеки | 18: AAC G711 G719✗ G722 G7221@24/32 G7221C@24/32/48 G723 G726@16/24/32/40 G728 G729 OPUS ILBC | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:44-47`, `LIVE_VALUES.md:77` | Да, как чек-лист: у нас есть Opus/G.711/G.722/G.726/G.728/G.723.1/AAC-LATM; G.729/GSM/G.722.1 — в бэклоге |
| Разрешения | 16 фронтовых вариантов (4K…352×240) + 61 имя форматов движка | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:244-257` | Да: канонический список разрешений/классов = основа M2/M4. У нас `gateway_video.rs:228 SIZE_LADDER` (6 ступеней) — расширить до 16 |
| Профили | Hi 4096/1920×1080; Optimal 2048/1280×720 (`is_default`); Economy 1024/1280×720; Locked 4096/1280×720 | `[TC]00_DOCUMENTATION/LIVE_VALUES.md:82-89` | Да: наши дефолты в `media_policy.rs:96-100` уже 2048/1280×720@30 — **совпадают с Optimal**. Это аргумент, что модель TrueConf уже «прошита» в нашем коде |
| H.264 profile/level | quality profile (7 значений) → `profile_idc`: BALANCED/RESOLUTION/FRAME_RATE/DEFAULT→0x42; PRIMARY/SECONDARY_COMPLIANT→0x64; PACKETIZATION→0x53 + packetization-mode=1; `level_idc`: QCIF→1.0 (max-fs 256), CIF→1.3 (512), SD→3.0 (1792), 720P→3.1 (3840), 1080P→4.0 (8192), 4K→5.1 (36864) | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:204-226` | **Да**: у нас есть `profile-level-id`/`max-fs` в `rtp_bridge.rs:397-430` и `gateway_video.rs:143-161`, но **таблица классов** — только 6 ступеней (`gateway_video.rs:228`). Расширить до 6 классов эталона (проверяемо тестами на fmtp) |
| Запись | `bitrate = W·H·fps·bpp/1000`, `bpp`: BEST 0.1 / GOOD 0.067 / ECONOMY 0.033; 1920×1080@30 → 6220/4167/2052 кбит/с (совпадение до 0.01 %) | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:173-174` | **Да**: перенести формулу в `vks-recorder` (сейчас модели нет) — S-задача с готовой проверкой |
| Клампы значений | `bitrate ∈ [64…8192]` кбит/с (в swagger 4096 — устарело), `fps ∈ [15…60]`, `width/height` **не валидируются**, `connections_limit` default 100 (в swagger 256) | `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:159-161` | **Да, как source of truth для админ-валидации**; наши `media_policy.rs:75-80` шире — согласовать (см. M1) |
| Лимиты движка | `max_active=100`, `max_calls=100`, `max_conferences=5` (live; в доке 4 — расхождение `MASTER_ARCHITECTURE.md:630` vs `LIVE_VALUES.md:44`), `connections_limit=100` (=лицензия) | `[TC]00_DOCUMENTATION/LIVE_VALUES.md:43-44,69-79` | Да, но значения — **свои** (зависят от нашего CPU). Ввести как конфиг (M12) |
| Лимит энкодеров | `performance=106`, `encoders=106`, `decoders=1696 (=106×16)` | `[TC]00_DOCUMENTATION/LIVE_VALUES.md:42`; `resource_model.md:45` | Только **метод** (калибровка), значение не переносить |
| Телеметрия движка | iproc: images total 76/user 72/codec 4, memory 442 765 736 Б; asm.delay.max 300 мс; scheduler.workers 37 | `[TC]00_DOCUMENTATION/LIVE_VALUES.md:51-53` | Да: набор ключей — образец M12; числа — не переносить |
| Логи/ротация | `engine.log`, retention 10 файлов (`head -n -10 | xargs rm`), api.log 5MB×5 | `[TC]04_DEPLOYMENT/tcmcu-engine.service:14-15`, `config.yaml:22-25` | Да: у нас `StandardOutput=append:vks_server.log` без ротации — принять retention |
| NUMA | `VSM_ENCODERS_ON_NUMA`, `Channel … on NUMA node #%d` | `[TC]09_MEDIA_CORE/vsm_spec.md:26` | Нет смысла на 1 NUMA; зафиксировать как «не переносим» |

### 2.2. Vinteo MCU (SIP/H.323-цепочка, сеть, конфиг-гигиена)

| Компонент / параметр | Версия/значение | Источник | Применять у нас? |
|---|---|---|---|
| ОС базы | RHEL 6.3 (2012), anaconda-образ | `[VI]04_Пакеты_и_библиотеки_REPORT.md:19-29` | **Нет** (EOL, 2012). Только как источник «как не надо» |
| Пакетов в rpmdb | 941 (04) / 747 (01) — расхождение не разрешено | `[VI]04…:25`; `[VI]01_Движок_MCU.md:17` | Нет |
| Vinteo-пакетов в rpmdb | **0** — весь продукт «руками» | `[VI]04…:26-29` | Нет; урок: продукт без пакетной воспроизводимости = боль. У нас cargo/CI — уже лучше |
| Asterisk | 1.4.x-форк под ярлыками «1.2.1»/«31.2.8», вендорские модули `chan_sip/chan_h323/app_mixer/...` | `[VI]07_Адвокат…:13-16`; `[VI]09…:20-22` | **Нет** (EOL, закрытые модули). Наш путь — свои протоколы + pycrate |
| SIP/H.323-стек | PWLib/PJSIP 1.8.7, H323Plus 1.15.6, libnice 0.1.4 | `[VI]04…:226-233` | Нет; H323Plus 1.15.6 — 2010-е, ADR-0007 надо переписать под реальность (pycrate) |
| Медиа-стек | FFmpeg 0.11.1, x264 0.129, opus 1.0.2, vpx 1.2.0 — source-сборка 2012-2013 | `[VI]09…:30-36` | Нет. Урок: пиннинг версий и воспроизводимость — у нас Cargo.lock |
| GPU/ускорение | NVENC (CUDA 550.163.01), Intel MFX 1.19, IPP 7.1, ICC 18 | `[VI]09…:31-36`, `:768-802` | Нет: у нас CPU-only, AVX2 (AVX-512 нет — lscpu). NVENC — отдельный бэклог при наличии GPU |
| Порты | 5060/5061/3478/1720/1024-65000/20000-65000/40000-50000 (T.38)/5000/5038/3306/11211 | `[VI]02…:274-286`; `[VI]13…:917-947` | **Да** — как матрица портов (M9); у нас другая нумерация, документ нужен |
| sysctl | `shmmax=64 GiB`, `shmall=2^32`, `rmem_max=5.8 ГБ`, `udp_mem` в гигабайтах, `lo mtu 1600`, `txqueuelen`, `sched_nr_migrate=2`, `vfs_cache_pressure=10000` | `[VI]13…:794-795` | **Частично**: `udp_mem`/`rmem_max` — да, но **разумными** значениями; `shmmax` — только если появится SHM; 5.8 ГБ буферов — нет |
| Таймауты | `rtptimeout=60`; bombercount 70/15000; call_duration 30 с; 3 попытки; интервал 60 с; JWT 3600/3720 | `[VI]08…:251-283` | **Да**: `rtpx`-watchdog и лимиты автодозвона — прямо в админ-модель; JWT TTL у нас 86400 (`configs/default.toml:25`) — можно ужесточить |
| Мёртвый конфиг | 48 из 62 файлов `/etc/asterisk` мертвы; 27 «заявлено, но не работает»; 16 «выдуманных» имён | `[VI]11_Реестр…:989-1048`; `[VI]08…:125-160` | **Да — как процесс**, не как код: наш `regress-hygiene` должен вести такой же реестр мёртвых ключей |
| Мониторинг | collector.php (сломан `mysql_connect` в PHP7), MRTG-крон (бинаря нет), SNMP off | `[VI]08…:125-160` | Нет. Урок: Prometheus — правильный выбор |

### 2.3. Cisco CMS 3.13.1 (element API, телеметрия, параметры)

| Компонент / параметр | Версия/значение | Источник | Применять у нас? |
|---|---|---|---|
| Media framework | 24 элемента, call-table lifecycle, `mf_load` ×24 | `[CS]10_MEDIA_FRAMEWORK_ELEMENTS.md:1-34` | **Да** — форма контракта (M11) |
| Микшер/композит API | `mf_video_compose` (`:57/74/83/90/97/1011/1048/1172`), `mf_audio_mixer` (`:109/128/136/177/287/294/301/308/318`) | `[CS]15_MIXER_ELEMENTS_API.md:6-35` | **Да** — как чек-лист «какие операции вообще нужны» при проектировании наших элементов |
| Телеметрия | 115 `key_*`, 14 `report_*`, 1 `event_*` | `[CS]06_MEDIA_TELEMETRY_KEYS.md:1-20` | **Да** — семантика счётчиков (M12) |
| Runtime-параметры | 23 поля `Cmgr::Parameters`; `-max-calls` 500; `-call-bandwidth` 5000; `-resolution 1080p/720p/audio`; SIP 5060/5061 | `[CS]18_MCU_RUNTIME_PARAMETERS.md:8-33` | **Да** — состав лимитов; значения свои |
| Лимиты микшера | `max_mixer_slots`, `max_media`, `max_objects`, `max_compose_report_cells` | `[CS]18…:67-73` | **Да** — аналог в `McuRuntimeLimits` |
| H.323 | **отсутствует**: 0 совпадений по 2 286 путям и 321 750 строкам; 9 контейнеров = 0 | `[CS]16_H323_ABSENCE_PROOF.md:1-84`; `[CS]19…:35-40` | **Нет и нечего переносить.** Это подтверждает: H.323-плоскость у нас — своя (pycrate), а не «как у Cisco» |
| Медиа-путь | **не** FFmpeg: `DT_NEEDED` без `libav*/x264`, `dlopen` нет; FFmpeg — только CLI-запись | `[CS]19_DEBATE_RESOLUTION.md:9-20` (D1) | **Да — как предупреждение**: не строить медиапуть на GPL-FFmpeg. У нас FFmpeg-сайдкар за процессной границей — это приемлемо (нет линковки) |
| Лицензии | libsrtp2 2.2.0 (BSD), OpenSSL 1.1.1zd (Apache), Speex 1.2rc1 (BSD), **Fraunhofer AAC — proprietary, not redistributable**, x264/FFmpeg — GPL | `[CS]A8_packages_regressions/README.md:89-190` | **Частично**: libsrtp2/Opus/Speex — ок; Fraunhofer AAC и GPL-связку — **нет** |
| Прочее | nginx 1.30.4, MongoDB 7.0.3 (SSPL), Java 11, Poco 1.11, Boost 1.74, Unbound 1.13.1 | `[CS]A8…:89-190` | Нет (не наш профиль) |

### 2.4. Сводный «применить у нас» (одним списком, чтобы не потерялось)

1. **Модель:** 6 шагов наследования + FALLBACK + пересечение кодеков + разрешение-как-свойство-раскладки (M1/M2, §0).
2. **Код, который уже есть и надо подключить:** `media_policy.rs` → `McUEngine` (сейчас не вызывается, §0 T10).
3. **Код, который надо написать:** `channel.rs` (M3), `resource.rs` (M4), `ScratchPool`/I420-формула (M7), `AudioBus` (M6), `McuRuntimeLimits` + телеметрия (M12).
4. **Конфиг/ОС:** `LimitNOFILE=65536` (есть), `TasksMax`, retention логов, `PORT_MATRIX.md`, sysctl-подмножество (M9).
5. **Процесс:** реестр мёртвых ключей/фич (по образцу Vinteo `11:989-1048`), запрет на «магические числа» из эталонов (106/37/500 — калибровать).

### 2.5. Карта «наш модуль ↔ понятие эталона» (что переименовывать/достраивать, а не изобретать)

| Понятие эталона | Источник | Наш эквивалент (файл) | Состояние |
|---|---|---|---|
| `Channel` / `Stream` / `Output` | `[TC]09_MEDIA_CORE/vsm_spec.md:30-49` | `RoomState` + `compositor::Compositor` (`engine.rs:45`, `compositor.rs:24`) | **нет разделения**: одна комната = один композит |
| `video_layouts` (payload + resolution) | `[TC]00_DOCUMENTATION/LAYOUT_CHAIN_SPEC.md:18-31` | `vks_layout::Layout` (`core.rs:206`) + `LayoutDescriptor` (`media_engine.rs:165`) | геометрия есть, resolution **нет** |
| резолв раскладки (3 ступени: call_string → contact → default) | `LAYOUT_CHAIN_SPEC.md:36-53` | `layout_signal.rs` + `apply_layout` (`vks-server/src/layout_signal.rs`, `transparency.rs:134`) | есть авто-раскладка по числу участников (T03), **нет** ступени «адрес→дефолт» в явном виде — **не проверено до конца** |
| `ABOOK BITRATE/FRAME SIZE/…` | `[TC]MASTER_ARCHITECTURE.md:100-113` | `media_policy::EffectiveMedia` (`media_policy.rs:59-68`) | **не подключено** |
| `AudioChannel` (SPEAKERS/LISTENERS) | `[TC]09_MEDIA_CORE/asm_spec.md:25-38` | `AudioMixer` (`audio_mixer.rs:22`) | одна шина на комнату |
| `VAD`/`ASPEAKER` (DSI, FAST/SLOW) | `[TC]09_MEDIA_CORE/asm_spec.md:65-71` | `vad.rs` (Rms+ZCR, 500 мс) + `active_speaker` (`engine.rs:961`) | есть база, нет политик DSI/FAST-SLOW |
| `IPROC` (кадры/счётчики/рендер) | `[TC]09_MEDIA_CORE/iproc_spec.md` | `Frame` (`frame.rs`) + `Compositor::blit` (`compositor.rs:73`) | нет счётчиков/пула/режимов fit |
| element call-table (`mf_*`) | `[CS]10_MEDIA_FRAMEWORK_ELEMENTS.md:1-34` | нет (прямые вызовы из `engine.rs`) | — |
| `report_*`/`key_*` телеметрии | `[CS]06_MEDIA_TELEMETRY_KEYS.md:1-20` | `McuRenderStats` (`engine.rs:32`) + Prometheus | 3 поля против 115 ключей |
| `rtptimeout`/порты/таймауты | `[VI]02…:274-286`, `[VI]08…:251-283` | `gateways.rs` media-bridge watchdog (`media_bridge_dead`, `gateways.rs:227`) | есть частично; матрицы портов нет |

> Практический вывод: **6 из 10 понятий уже имеют у нас точку в коде** — переименовывать не нужно, нужно достроить и связать. Это аргумент за поэтапный перенос (M1→M3→M2→M4), а не за переписывание медиаядра.

---

## 3. Что НЕ переносить и почему

| # | Не переносим | Почему (факт + источник) | Риск, если перенести |
|---|---|---|---|
| N1 | Код/бинарь `mcu`, `app_mixer`, `proxy`, `libavqos`, `libfec`, `libmcuclient`, `libnicehelper` (Vinteo) | Проприетарны; исходников нет; тексты функций не восстановимы — `[VI]09…:768-802`, `[VI]14…:797-819` | Юридический + невозможно поддерживать |
| N2 | Всю ОС-базу Vinteo (RHEL 6.3/2012, ICC 2018, Intel IPP/MFX, CUDA-рантайм) | 2012-й userland; ICC/MFX требуют платных лицензий — `[VI]09…:30-36`, `[VI]04…:19-29` | EOL, CVE-долг, лицензионные ограничения |
| N3 | Asterisk 1.4.x-форк + `chan_sip`/`chan_h323` как сигнальный слой | EOL-ветка; модули не существуют в upstream — `[VI]07…:13-16`, `[VI]09…:20-22` | Невозможность обновления безопасности; ломает наш инвариант «свой протокольный слой» |
| N4 | «Магические» константы эталонов: 106 энкодеров, 37 воркеров, 500 max-calls, 79 871 Б очереди | 106 — калибровка на их CPU (`[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:624-631`); 37 — связь не установлена (`[TC]09_MEDIA_CORE/scheduler_spec.md:77,84,249`); 500 — их лицензия | Неверная ёмкость, ложные SLA |
| N5 | Vinteo sysctl-значения 2013 г. (rmem_max 5.8 ГБ, `shmmax` 64 ГБ) | `[VI]13…:794-795` — рассчитаны под их нагрузку/память | OOM/деградация на 56 GiB; на KVM часть no-op |
| N6 | T.38/UDPTL и факс-подсистему Vinteo | Мертва: `reload_exten_fax()` закомментирован, `app_fax.so` нет, каталога `/install` нет — `[VI]08…:125-160` (п.13), `[VI]11…:989-1048` | Мёртвый код + лишние порты 40000-50000 |
| N7 | Мёртвый конфиг Vinteo (48/62 файлов, `h323conf`-таблица, `gl_slides`, `allow_reactions`, `connection_param`, `licensestimelimit`) | `[VI]11…:989-1048`, `[VI]08…:125-160` (27 пунктов) | Копирование багов, ложные ожидания |
| N8 | Модель раскладок Vinteo (файлы `.cfg/.s/.set`, 151 файл; конструктор отключён лицензией) | `[VI]13…:261-304`, `:427-460`; конструктор disabled — `[VI]08…:125-160` (п.16) | Регресс: у нас уже есть 52 Codian-пресета + 22 раскладки в БД (T02/T03), файловая модель хуже |
| N9 | GPL-медиапуть Cisco (FFmpeg/x264 внутри процесса) и Fraunhofer AAC | GPL-заражение и proprietary-компонент — `[CS]19…:9-20`, `[CS]A8…:89-190` | Нарушение лицензий, юридический риск |
| N10 | 1:1-реплика Cisco (1 014 файловых манифестов, 1 272 файла) | Это скелет клона проприетарного продукта — `[CS]MASTER_INDEX.md`; в наших инвариантах — «свой движок» (ADR-0002/0004) | Неподъёмный объём, регресс к чужой архитектуре |
| N11 | H.323-«донорство» Cisco | H.323 отсутствует доказанно — `[CS]16…:1-84` | Потеря времени на поиск несуществующего |
| N12 | Проприетарные кодеки TrueConf (H264-SVC движка, VP8 через лицензию) | Лицензия режет `h264-svc`/`vp8` — `[TC]00_DOCUMENTATION/LIVE_VALUES.md:39-40` | Непоставляемые фичи |

---

## 4. Контраргументы, которые я ожидаю от Lead, и мои ответы

**C1. «Cisco CMS — самый современный, почему донор №1 медиаядра не он? У него 24 элемента, телеметрия, полная реплика 1 272 файла.»**
Ответ: element API — это **интерфейс**, а не модель медиаядра; телеметрия — наблюдаемость, не механизм кодирования. Модель «per-abonent параметры + разрешение-раскладка» у Cisco **не документирована** в прочитанных артефактах: `Cmgr::Parameters` даёт глобальные `-resolution 1080p/720p/audio` и `max_mixer_slots` (`[CS]18…:8-33,67-73`), т.е. уровень «канал/микшер», а не «per-abonent ABOOK». Плюс медиапуть проприетарен (свой H.264) — переносить нечего, только форму API (`[CS]19…:9-20`). Поэтому: Cisco = API + телеметрия, TrueConf = модель, Vinteo = цепочка/сеть. Позиция не меняется.

**C2. «Vinteo-то как раз имеет per-участника: `differentvideoparts`, персональные раскладки, `videomixer set composition param <conf> <part> …`, encoder farm `_encs` с `clientscnt`. Это опровергает "Vinteo не донор медиаядра".»**
Ответ: различаю два слоя. Vinteo действительно даёт **персональную раскладку/источник** (`[VI]13…:263-304`: `participant_layouts`, `differentvideoparts`, `videomixer set composition type … <participant>`) и **ферму энкодеров** (`[VI]12…:186-192`). Но: (а) у него **нет** per-abonent команд битрейта/кодека/разрешения исходящего потока — есть `composition param` (titers/watermark/font/borders/placesmap, `[VI]13…:282-288`) и общий `resolution` на композицию; (б) его медиаядро невоспроизводимо (`[VI]09…:768-802`). Поэтому M3 (encoder farm) я **беру из обоих** (Vinteo подтверждает отраслевой характер приёма), а M1/M2 (per-abonent + разрешение-раскладка как отдельные уровни) — из TrueConf. Это уточнение позиции, не капитуляция: «донор №1 модели» остаётся TrueConf.

**C3. «"Разрешение = свойство раскладки" — у нас уже так: `geometry(preset, width, height)` принимает размеры.»**
Ответ: передача w/h в **функцию расчёта геометрии** — это не свойство раскладки. У нас: `Layout{version,kind,slots}` без resolution (`[OUR]crates/vks-layout/src/core.rs:206-213`), `LayoutDescriptor` без resolution (`[OUR]crates/vks-core/src/ports/media_engine.rs:165-183`), канвас — константа 1920×1080 (`frame.rs:13-15`, `compositor.rs:20`, `engine.rs:140`). В эталоне разрешение хранится в payload раскладки и применяется командой к каналу (`[TC]00_DOCUMENTATION/LIVE_VALUES.md:56-57`, `vsm_spec.md:48`). Разница практическая: у нас нельзя сделать комнату с 720p-раскладкой, не меняя глобальную константу.

**C4. «H.323 отсутствует у Cisco — и что? Cisco и не был донором H.323, тезис избыточен.»**
Ответ: тезис не избыточен, он **закрывает ловушку**: при выборе донора «самого современного» легко предположить, что H.323-плоскость можно смоделировать по CMS. Нельзя: 0 совпадений по 2 286 путям DWARF и 321 750 строкам, 9 контейнеров = 0 (`[CS]16…:1-84`). Следствие: H.323 у нас — своя плоскость на pycrate, и ADR-0007 (обещает FFI H323Plus, `docs/decisions/DECISIONS.md:14`) **устарел**; это надо исправить отдельным ADR. Плюс Vinteo-цепочка (H323Plus 1.15.6/PWLib 1.8.7) — источник interop-знаний, но не кода (EOL).

**C5. «"6 уровней" — вы сами показали, что в эталоне 5 строк таблицы. Значит, ваша цифра — натяжка.»**
Ответ: я показываю это первым (§0.2), причём в эталоне **три нумерации**: I.2-bis = 5 уровней (L1…L5), сводная строка = «6», `mediamodel.md` = L0…L7 (8). Я **не держусь за цифру**: канон — I.2-bis (5 уровней, L3 = три таблицы `profiles`→`conferences`→`sessions`, L4 = «уровень адреса = уровень пользователя» `call_strings.payload[protocol]`, L5 = только права `contacts_to_*`), а «6» — число шагов применения с дефолтами Python (`MASTER_ARCHITECTURE.md:177-179`) либо спорная нумерация. Наш `media_policy.rs:1-16` использует третью нумерацию — нужен канон+маппинг (§0.2, таблица). Что важно и защищается дальше: (а) приоритет FALLBACK, а не min — доказан эмпирикой (`[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:146-157`); (б) кодек — пересечение (`:113,170`); (в) разрешение — раскладка (`:181-189`). Если Lead настаивает на «5», я принимаю правку формулировки на «5 уровней данных + дефолты», не меняя существа.

**C6. «6-уровневое наследование у нас уже реализовано в T05a (env → global → conference → call_string). Зачем тащить TrueConf?»**
Ответ: реализовано **в gateway-слое** и частично: `gateways.rs:441-473` (`video_profile` + `apply_call_string_settings`), `rtp_bridge.rs:613-651` (`b=AS`, `max-fs`, `max-mbps`), профиль применяется к SDP/транскодеру. Чего нет: (а) единого резолвера — `media_policy.rs:87 resolve()` не вызывается (только re-export `lib.rs:36`); (б) уровня **раскладки** (R2) и уровня **движка** (L1 caps, `max_bitrate 8192`-аналог); (в) прав участника (mute/gain/slot_render_type) как слоя. Т.е. у нас 3–4 уровня из 5–6, размазанные по gateway. План: один резолвер, один вызов, все слои — иначе расхождения (уже есть: дефолт 2048/720p в `media_policy.rs:90-93` дублирует дефолты gateway).

**C7. «36 ядер и AVX2 — какой смысл в resource model? Просто дадим всем 1080p.»**
Ответ: per-output encode (M1) превращает нагрузку в произведение «комнаты × зрители × профили». При 8 участниках и 20 комнатах это до 160 энкодеров, а лимит TrueConf на их CPU был 106 (`LIVE_VALUES.md:42`) — это **не** наш лимит, но порядок показывает: без учёта ресурсов система уйдёт в дропы и tail-latency. Модель нужна не «чтобы ограничить», а чтобы **деградировать предсказуемо** (порядок из `resource_model.md:240`): outputs → mute → decoders → format. Иначе деградация будет случайной (OOM/дропы). AVX-512 тут ни при чём: x264/FFmpeg используют AVX2 (`lscpu`: avx/avx2/fma есть, avx512 нет) — калибровку всё равно делать на этой ВМ.

**C8. «Таблица пакетов/версий бесполезна: мы на Rust, а не на их стеке.»**
Ответ: из §2 переносится не стек, а **параметры и границы**: `LimitNOFILE`/`TasksMax`, retention логов, матрица портов, `rtptimeout`-watchdog, реестр разрешений/кодеков/профилей, состав лимитов (`max_calls`/`max_mixer_slots`/`max_encoders`), способ калибровки. Плюс таблица фиксирует **что не тащить** (N1–N12) — это защита от «давайте как у Vinteo». Для нашего Cargo/CI версии — свои (`Cargo.lock` 434 пакета, `tokio 1.53.3`, `str0m 0.24.1` — `PLAN…:38-43`), их и надо писать в финальный план.

**C9. «Ваш M8 (NACK/RTX/FEC) — это тема `net-resilience`, не ваша.»**
Принимаю частично: M8 остаётся в моём списке как **механизм эталона** (Vinteo RTP-сессия) с пометкой «владелец — `net-resilience`», но я фиксирую факт: на gateway-пути jitter-буфера/NACK нет (`grep` по `crates/vks-gateway-sip/src/` → 0), и это надо закрыть в P4. Своих планов на реализацию M8 не заявляю.

**C10. «Вы не сделали ни одного замера — значит, "эффект" недоказуем.»**
Ответ: верно и намеренно — правила задачи запрещают сборки и нагрузку. Поэтому «эффект» в §1 сформулирован как метрика проверки + механизм, а не как процент. Числа эталонов (106 энкодеров, 37 воркеров, ≈5.8 МБ/кадр, 442 765 736 Б) приведены как **факты эталона**, а не как наши цели. Единственные числа, которые я утверждаю как «наши», — это измеренное на этой ВМ: 36 CPU, AVX2 без AVX-512, 56 GiB RAM, L3 32 MiB (`lscpu`, `free`, `PLAN…:26`).

---

## 5. Открытые вопросы к Lead (максимум 5)

1. **Калибровка.** Разрешаете ли запуск калибровочного прогона MCU (не сборки — существующий release-бинарь) для получения `c_device` энкодера на 720p30/1080p30? Без этого M4 остаётся методом без констант, а P3-цель «≥30 %» нечем проверить.
2. **Контракт раскладки.** Добавление `resolution` в `Layout`/`LayoutDescriptor` — это новый ADR (меняет ADR-0030/0032) или правка существующих? Нужно решение до реализации M2, иначе будет конфликт с фронтом.
3. **Целевая ёмкость.** Берём ли ориентир `MEDIA_PIPELINE.md:189-197` (20 комнат × 8 участников, 1080p-композит, 4 ядра/комнату) как приёмочный, или целимся в TrueConf-подобные 100 вызовов (`LIVE_VALUES.md:43`)? От этого зависят `max_calls`/`max_encoders` и смысл M3/M4.
4. **Канон уровней.** Фиксируем ли канон I.2-bis (**5 уровней**: движок, `configurations._`, `profiles`→`conferences`→`sessions`, `call_strings`, `contacts_to_*`; права — отдельный L5) и маппим на него наш `media_policy.rs:1-16`, или оставляем нашу нумерацию L1…L6 как есть? В эталоне четыре разные нумерации (§0.2), поэтому без канона перенос превратится в спор о словах. Плюс: нужен ли наш **слой прав** (L5 канона) в v1 — у нас его нет.
5. **H.323 scope.** H.235 (SimpleMD5/CAT) и H.239-презентация входят в v1, или v1 = audio+video? От ответа зависит, нужен ли нам Vinteo-опыт по H.239/token-механике (`[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:949-955`; `[VI]14_Реконструкция_исходников_MCU.md:763-796` — строка `videomixer.cpp` про H.239-презентацию) или это P2-бэклог.

---

## Приложение A. Покрытие чтения и честные «не проверено»

### A.1. Прочитано полностью (файл целиком)

| Файл | Строк |
|---|---|
| `[VI]01_Движок_MCU.md` | 468 |
| `[VI]07_Адвокат_дыявола_проверка_консенсуса.md` | 102 |
| `[VI]12_Архитектура_микшера_MCU.md` (строки 129-571: §§б-и + приложение) | 443 из 571 |
| `[TC]00_DOCUMENTATION/LIVE_VALUES.md` | 94 |
| `[TC]00_DOCUMENTATION/LAYOUT_CHAIN_SPEC.md` | 271 |
| `[TC]09_MEDIA_CORE/vsm_spec.md`, `iproc_spec.md`, `asm_spec.md`, `scheduler_spec.md`, `resource_model.md` | 280+190+175+97+250 |
| `[CS]10_MEDIA_FRAMEWORK_ELEMENTS.md` (1-250), `15_MIXER_ELEMENTS_API.md` (1-160), `16_H323_ABSENCE_PROOF.md` (84), `18_MCU_RUNTIME_PARAMETERS.md` (77), `19_DEBATE_RESOLUTION.md` (1-200) | 411+258+84+77+323 |
| `[TC]04_DEPLOYMENT/config.yaml, env.conf, tcmcu-engine.service, tcmcu-cpu-update.service, ctrlmachine_lin.xml, web.toml` | 100% |
| `[OUR]crates/vks-core/src/media_policy.rs` | 216 |

### A.2. Прочитано выборочно (разделы, несущие `file:line`-доказательства)

`[VI]02` (§§1,3-7, таблица портов), `[VI]04` (§§0-2.3, 3.4), `[VI]08` (§§3, 5), `[VI]09` (§0 вердикты, §д.4), `[VI]11` (§8 мёртвый конфиг), `[VI]13` (§§2.1-2.8, 3.1, 4.1-4.3, 6-7), `[VI]14` (§е карта исходников, §ж что не восстановимо), `[TC]MASTER_ARCHITECTURE.md` (строки 1-47, 100-244, 259-375, 570-1189 — т.е. §I.1-I.2-quinque, §I.3-I.4, §I.2-oct, §V.0.1-V.0.4, §VI.1-VI.2, §VII.1-VII.5, §VIII; **§§II-III, V.1-V.4, V.6, VI.4, IA.\* не читал**), `[CS]06` (шапка+список ключей), `[CS]A8` (§2.3-2.4), `[CS]MASTER_INDEX.md`, `[CS]A1/A2/A3b/A6a/A8` (только заголовки разделов).

### A.3. Не прочитано (и на что это влияет)

- `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md` §§II-III, V.1-V.3, V.6, VI.2.5*, VII.1-VII.3, IA.1-IA.7, IB.* — не влияет на медиаядро-модель, но может содержать нюансы лицензии/лимитов (IA.2-IA.6). Если Lead потребует — дочитаю.
- `[TC]00_DOCUMENTATION/BUILD_PLAN.md`, `PACKAGE_FOLDER_MAP.md`, `AGENT_ROSTER.md` (полностью), `[TC]10_EVENT_PIPELINE/*` — event/WS-pipeline; к медиаядру не относится, но относится к M12 (телеметрия/события).
- `[VI]03`, `[VI]05`, `[VI]06`, `[VI]10`, `[VI]15` — регрессии, веб-панель, установка, БД-схема, пути; для этой задачи не критичны.
- `[CS]ANALYSIS_REPORT*.md`, `A4_protocols_h323/README.md` (957 строк, H.323-агент Cisco), `A9` — не прочитаны; при споре о H.323-плоскости прочту A4 — **содержание не проверено**, поэтому не утверждаю, что он подтверждает или опровергает T9.

### A.4. Явно «не проверено» (не выдаю за факт)

1. Связь числа воркеров TrueConf (37) с числом ядер — `[TC]09_MEDIA_CORE/scheduler_spec.md:77,84,249` говорит «не установлено»; моя гипотеза «36 физ. ядер + 1» **не доказана**.
2. Кто получает `remoteip` в Vinteo (реальный клиент или Asterisk) — ключевой нерешённый вопрос `[VI]07…:100`; влияет только на M8 (транспорт), не на модель.
3. Точные **числовые** значения `A[]/B[]/C[]` по 80 классам (`idx = class−21`) — не извлечены: в `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:330-336` даны формулы и смещения (`VSM+0x4E0/0x4E4/0x4E8`), но не сами числа; в `[TC]09_MEDIA_CORE/resource_model.md` — модель, не полная таблица. Для M4 нужен либо отдельный проход, либо (предпочтительно) **своя калибровка** на этой ВМ.
4. Число пакетов Vinteo: 941 (`[VI]04…:25`) vs 747 (`[VI]01…:17`) — расхождение в эталоне не разрешено; на перенос не влияет.
5. `max_conferences`: 4 (`[TC]MASTER_ARCHITECTURE.md:630`) vs 5 (live `LIVE_VALUES.md:44`) — в таблицах привожу live-значение 5 с пометкой.
6. H.265: `MASTER_ARCHITECTURE.md:43-48` помечает H265 «disabled по умолчанию», а `:191-192` — «выключен лицензией»; live-лицензия говорит `h265: T` (`LIVE_VALUES.md:72-73`). Причина расхождения не установлена; в §2.1 я привожу **оба** факта без выбора.
