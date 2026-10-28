# 07. Reference transplant spec: media_policy wiring, encoder farm, resource model, clamps

> **Агент:** `ref-miner` · **Дата:** 2026-10-25 · **Статус:** спека для реализации (`media-core` — O1/O7), **кода не менял**.
> **Задача Lead (turn 2):** A) wiring `media_policy::resolve`; B) encoder farm (M3) как спека под наш код; C) resource model в наших единицах с сравнением с live-цифрами; D) формула записи и клампы для v1.
> **Границы:** только `docs/`. Файлы `crates/vks-server/src/gateway_video.rs`, `crates/vks-media-mcu/src/engine.rs` сейчас пишет `media-core` — здесь **только дифф-скетчи**, не патчи.

## 0. Базис, обозначения, что заморожено

**Все «live» числа взяты из замороженного бинаря и замеров команды, не из ТЗ:**
`sha256 = 8ab81afd70a7f168aa3c4e105f110def59a4d4270b8cfc0b6a749ee522868007`, `docs/audit/2026-10-25/measure/PASSPORT.md:7`; флаги сборки — `-C target-cpu=native` (`[target.x86_64-unknown-linux-gnu] rustflags`, `.cargo/config.toml:20`), т.е. Haswell/AVX2. Любая цифра калибровки в этом документе подписывается этим sha256.

**Про номера строк:** дерево активно правят (`media-core` — `gateway_video.rs`, `gateways.rs`, `engine.rs`), поэтому `файл:строка` — снимок на момент написания; ориентир для поиска — **имя функции/теста**, указанное рядом. Замеры и эталоны от этого не зависят.

**Измеренные константы, на которые опирается спека** (`[MEAS]BENCHMARK_REPORT.md` §2, тот же бинарь sha256, `cpu.stat` cgroup-контура без изоляции CPU ⇒ верхняя оценка «сколько сожрёт»):

| Компонент | Значение | Источник |
|---|---|---|
| Декодер 720p25, **худший** синтетический контент (`testsrc2`) | **0.34** ядра | `BENCHMARK_REPORT.md:42` |
| Декодер 720p, локальный ffmpeg на том же потоке | **0.12** ядра | `:76-77` |
| Энкодер 720p30 (`libx264`, `-threads 1`) | **0.31** ядра | `:41` |
| 1 SIP-сессия 720p25 «под ключ» | **1.40** ядра = parent 0.76 + enc 0.31 + dec 0.34 | `:31`, `:36-43` |
| Аудио-only при согласованном видео (чёрный кадр всё равно кодируется) | **0.89** ядра | `:45-46` |
| Композит 1920×1080 | **5.23** мс/кадр (p99 ≈ 13.3) | `[MEAS]03_media_engine.md:119` |
| `resize_i420` 1080→720, **до O3** | **8.0** мс/кадр | `BENCHMARK_REPORT.md:67` |
| `resize_i420` 1080→720, **после O3** (строчные срезы + Bresenham) | **2.0–2.8** мс/кадр | `:67` |
| Idle контура | 0.02 ядра | `:30` |

`[ОЦЕНКА]` остаётся только там, где замера нет (например, `C_enc` — маргинальная цена зрителя в пакетизации: её даст P4, до этого 5–10 mcore).

**Метки достоверности:**
- `[ЗАМЕР]` — измерено командой на этой ВМ (источник указан: файл:строка).
- `[РАСЧЁТ]` — арифметика из `[ЗАМЕР]`, мной, без новых прогонов.
- `[ЭТАЛОН]` — факт из отчётов эталонов.
- `[ОЦЕНКА]` — инженерная оценка, требует замера (не выдаётся за факт).
- `[НЕ ПРОВЕРЕНО]` — так и помечено.

Легенда путей эталонов: `[TC]` = `/home/pro/Документы/MCU/mcu trueconf/reconstruction/`, `[CS]` = `cisco CMS/analysis/meta/`, `[VI]` = `vinteo/08_отчёты_анализа/`, `[MEAS]` = `docs/audit/2026-10-25/`.

**Факты-основания для всех разделов (проверены по коду/замерам):**

| # | Факт | Источник |
|---|---|---|
| F1 | `media_policy::resolve` существует, покрыт 5 unit-тестами, **не вызывается нигде** | `crates/vks-core/src/media_policy.rs:87`, `:156` (`mod tests`), `crates/vks-core/src/lib.rs:36` (только re-export) |
| F2 | Gateway уже реализует 3-уровневый приоритет **вручную**: env → global → conference → call_string | `crates/vks-server/src/gateways.rs:441-459`, `crates/vks-server/src/gateway_video.rs:1059-1097` (`apply_video_settings`, `apply_call_string_settings`) |
| F3 | Inbound SIP видео-мост берёт из SDP только `codec`/`level`, остальное — **дефолт 1280×720@30/2048**; настройки комнаты/глобала игнорируются | `crates/vks-server/src/gateways.rs:2626-2628` (`profile: VideoProfile { codec: …, ..default() }`, F3) |
| F4 | Inbound H.323 берёт `manager.video_profile(room, None)` | `crates/vks-server/src/gateways.rs:3475` |
| F5 | Outbound SIP/H.323: `video_profile(...)` + `clamp_to_answer(max_fs, max_br, max_mbps)` | `crates/vks-server/src/gateways.rs:1188`, `:1272-1275`; `gateway_video.rs:163` |
| F6 | Публичный контракт раскладки **не содержит** resolution | `crates/vks-core/src/ports/media_engine.rs:165-183` |
| F7 | Канвас композитора — константа 1920×1080 | `crates/vks-media-mcu/src/frame.rs:13-15`, `compositor.rs:20`, `engine.rs:140` |
| F8 | Максимум кодек-воркера 4096×2160 (продуктовый потолок), но фактический выход шлюза ограничен ladder 1080p | `crates/vks-media-mcu/src/codec_worker/mod.rs:28-31`, `gateway_video.rs:228` |
| F9 | На шлюзовом egress каждый зритель — отдельный FFmpeg-энкодер и отдельный скалярный `resize_i420` | `gateway_video.rs:488-489` (2 процесса на сессию), `:629-660` (render→resize), `:754-783` (`resize_i420`) |
| F10 | Per-viewer RTP **уже** свой: свой SSRC, свой seq, свой timestamp | `gateway_video.rs:506` (ssrc), `:572` (seq=1), `:580` (ts), `:582-600` (packetize) |
| F11 | keyint у энкодера = `fps` (IDR раз в секунду; `gateway_video.rs:312` → `-g` `:351`), `-threads 1`, `ultrafast/zerolatency` | `gateway_video.rs:317-370` |
| F12 | Шлюзовой SIP-путь — **plain RTP** (SRTP отбивается на UAS), SRTP только в WebRTC-плече (str0m) | `crates/vks-gateway-sip/src/uas.rs:992` (тест `rejects_srtp_proto…`) |
| F13 | Кадровые метрики шлюза — per-session `GatewayMediaStats` (rx/tx/encoded/decoded/dropped) | `crates/vks-server/src/gateways.rs:183-200` |

---

## A. Wiring `media_policy::resolve` — точки подключения и дифф-скетчи

### A.0. Что уже есть и чего не хватает

`MediaLayer` — ровно тот вход, который нужен (`crates/vks-core/src/media_policy.rs:20-42`: `audio_codecs`, `video_codecs`, `max_bitrate_kbps`, `width`, `height`, `fps`, `profile`, `level`), `EffectiveMedia` — ровно выход (`:59-68`). Т.е. типы совпадают с `VideoProfile` шлюза 1:1 по полям (`gateway_video.rs:51-59`), кроме `codec` (enum vs список) и `bitrate` (kbps совпадает).

Расхождение, которое wiring закрывает **бесплатно**: inbound SIP (F3) сегодня физически не видит ни `conference.payload.bitrate`, ни `resolution`, ни `max_frame_rate` — при том что outbound видит. Это не «новая функция», а устранение расхождения путей.

Карта уровней (канон `I.2-bis` = **5** уровней; наша нумерация `media_policy` = **6**, см. `01_reference_mining.md` §0.2) ↔ источники у нас. Строки L1–L5 — канон; «наш L6» — наш дополнительный уровень терминала:

| Уровень канона | Наш источник | Где читается сейчас | Нужно |
|---|---|---|---|
| L1 движок (потолки) | константы продукта | `codec_worker/mod.rs:28-31`, `gateway_video.rs:228` | `MediaLayer` из констант (ceiling-слой) |
| L2 `configurations._` | глобальные настройки | `AppState.api_v3.video_settings` (`gateways.rs:445-447`) | маппинг в `MediaLayer` |
| L3 профиль/конференция/сессия | payload комнаты | `api_v3.conference_settings[room]` (`gateways.rs:448-450`) | маппинг в `MediaLayer` |
| L4 адрес/пользователь | `call_strings.payload[protocol]` | `call_string_video_profile()` (`gateways.rs:462-473`) | маппинг в `MediaLayer` |
| L5 права | `contacts_to_*.payload` | **нет** | вне scope v1 (см. `01_reference_mining.md` §0.2) |
| наш L6: терминал (caps) — вне канона | SDP fmtp / H.241 | `VideoAnswerLimits` + `clamp_to_answer` (F5) | в v1 остаётся отдельным clamp-шагом (A.4) |

### A.1. Три варианта точки подключения (плюсы/минусы)

| Вариант | Где | Объём | Плюсы | Минусы / риск |
|---|---|---|---|---|
| **A1. Внутри `GatewayManager::video_profile`** | `gateways.rs:441-457` + 1 строка в inbound SIP (`:2628`) | ~60 строк, 2 файла | Единая точка для outbound SIP/H.323 и inbound H.323 **сразу**; тесты приоритета уже есть (`gateway_video.rs:1324`, `:1345`); нет изменений в engine/proto; снимает баг F3 | Не даёт профиль в `McuEngine` (farm пока не сможет группировать); caps остаются отдельным clamp |
| **A2. На конструкции сессии** | 3 сайта `gateways.rs:2600/3475/1408` + `spawn_video_media_bridge` | ~120 строк, 3–4 файла | Один вызов `resolve_for_call(room, uri, caps)` = уровни + caps + ceiling; сессия получает **финальный** профиль; готовит ключ farm | Конфликт с `media-core` (те же файлы сейчас в работе); caps из H.245 TCS inbound требуют парсинга (сейчас только outbound H.241) |
| **A3. В engine: `set_output_profile`** | `vks-media-mcu/src/engine.rs` + farm | ~150 строк, 3 крейта | Профиль живёт рядом с раскладкой; farm/O7 берут ключ из `RoomState`; сервер не дублирует clamp | Новый публичный API (нужен ADR/ревью), максимальный объём, позже всех |

**Рекомендация (порядок):** **A1 → A2 → A3**, каждый шаг самостоятелен и проверяем.
A1 даёт 80 % пользы (единая семантика + фикс F3) за 1 файл; A2 нужен, когда появится L5/caps-слой; A3 — часть O7 (farm), потому что ферме нужен профиль в комнате.

**Решение Lead (turn 3):** A1 **не в мой код** — это отдельная задача P1 после P0 (владелец `proto-chain`, у него уже правки в `gateways.rs`; пересечение запрещено). Эта спека фиксирует форму изменения и критерии, исполнение — не здесь.

### A.2. Дифф-скетч A1 (не патч, а форма изменения)

```rust
// crates/vks-server/src/gateways.rs  (в impl GatewayManager; чтение state — как в текущем
// video_profile(), gateways.rs:442-453: self.state.read() → Weak::upgrade)

/// Слои медиаполитики в порядке L1→L4 (низкий → высокий).
/// Строится из тех же источников, что читает video_profile(), поэтому
/// поведение админки не меняется — меняется только место склейки.
fn media_layers(&self, room_id: RoomId, call_string: Option<&serde_json::Value>) -> Vec<MediaLayer> {
    let mut layers = Vec::with_capacity(4);
    // L1: потолки продукта (engine ceiling). НЕ конфигурируются.
    layers.push(video::ceiling_layer());            // см. A.5
    let state = self.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade));
    if let Some(state) = state {
        let global = state.api_v3.video_settings.lock().map(|g| g.clone()).unwrap_or_default();
        layers.push(video::layer_from_global_settings(&global));      // new, ~25 строк
        // L3: payload комнаты.
        if let Some(conf) = state.api_v3.conference_settings.lock().ok()
            .and_then(|s| s.get(&room_id).cloned())
        {
            layers.push(video::layer_from_conference_payload(&conf)); // new, ~30 строк
        }
    }
    // L4: адрес/терминал (call_string.sip|h323).
    if let Some(value) = call_string {
        layers.push(video::layer_from_call_string(value));            // new, ~30 строк
    }
    layers
}

fn video_profile(&self, room_id: RoomId, call_string: Option<&serde_json::Value>) -> video::VideoProfile {
    let layers = self.media_layers(room_id, call_string);
    let effective = vks_core::media_policy::resolve(&layers);
    video::VideoProfile::from_effective(&effective)                   // new, ~20 строк
}
```

```rust
// crates/vks-server/src/gateway_video.rs  (new; маппинг 1:1 по полям)

impl VideoProfile {
    pub(super) fn from_effective(effective: &vks_core::EffectiveMedia) -> Self {
        let codec = match effective.video_codecs.first().map(String::as_str) {
            Some("H265") => GatewayVideoCodec::H265,   // см. A.5: egress запрет
            _ => GatewayVideoCodec::H264,
        };
        Self {
            codec,
            width: effective.width,
            height: effective.height,
            fps: effective.fps,
            bitrate_kbps: effective.max_bitrate_kbps,
            profile: effective.profile.clone(),
            level: effective.level.clone(),
        }
    }
}
```

```rust
// crates/vks-server/src/gateways.rs:2626-2628  (inbound SIP — фикс F3)

-                                profile: video::VideoProfile {
-                                    codec: if media.video_codec == Some("H265") { H265 } else { H264 },
-                                    ..video::VideoProfile::default()
-                                },
+                                profile: manager_clone.video_profile(room_id, None),
```

Проверяемость A1: существующие тесты `conference_settings_override_global_and_env` (`gateway_video.rs:1324`) и `call_string_settings_override_conference` (`:1345`) должны остаться зелёными **без изменения ожиданий** — это и есть критерий, что семантика сохранена. Новый тест: inbound-SIP профиль == outbound-профиль при одинаковой комнате (сейчас они разные).

### A.3. FALLBACK: что делать на каждом шаге

1. Слой с `None` — «не задаёт», не «запрещает» (`media_policy.rs:15-16`).
2. Кодек: **пересечение**, пустое пересечение не «запретить всё» — сохраняется предыдущий набор (`media_policy.rs:99-109` — уже реализовано, менять не надо).
3. Битрейт/размер/fps: значение верхнего слоя, зажатое в `[MIN, MAX]` (`media_policy.rs:77-82`, применение — `:132-140`).
4. Итог детерминирован; порядок слоёв задаёт вызывающий (наш `media_layers` — L1..L4 по возрастанию).
5. **Не путать**: FALLBACK выбирает значение, `min` ограничивает его потолком (эталонная формула `bitrate_out = min(profile, engine.max_bitrate, peer)` — `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:279-295`). У нас оба шага уже есть, но в разных местах: `resolve` (fallback+clamp) и `clamp_to_answer` (peer).

### A.4. Device caps: откуда брать (и что в v1 не трогаем)

| Транспорт | Источник caps | Есть у нас | v1-решение |
|---|---|---|---|
| SIP outbound | SDP **answer**: `max-fs`, `max-mbps`, `max-br`, `b=AS` | `video_answer_limits()` (`crates/vks-gateway-sip/src/rtp_bridge.rs:691-716`), применяется `gateway_video.rs:163` | Оставить отдельным clamp-шагом **после** `resolve` (как сейчас, F5) |
| SIP inbound | SDP **offer** (у нас его нет как отдельного парсера — `video_answer_limits` читает answer) | `[НЕТ]` | A2: добавить `video_offer_limits()` с той же логикой; до этого — не регрессировать |
| H.323 outbound | H.241 `maxBitRate` + `level_idc` | `H323VideoProfile { max_bitrate_kbps, level_idc, profile_bits }` (`crates/vks-gateway-h323/src/uac.rs:169-181`), `remote_video_max_bitrate_100bps` (`:73-74`) | Оставить clamp после resolve |
| H.323 inbound | H.245 TCS `videoCapability.maxBitRate` | частично: listener отдаёт caps, но Rust-маппинг в профиль не найден — `[НЕ ПРОВЕРЕНО]` | A2 (после `proto-chain`) |
| WebRTC | SDP answer браузера (str0m caps) | есть в SFU-плече | **вне scope v1**: SFU получает медиа fanout'ом, MCU-egress-профиль к нему не применяется |

Почему caps — **не** слой `MediaLayer` в v1: `max-fs` — это макроблоки (16×16), конвертация в `width/height` (`max_fs*256 → dims`) добавляет второй источник правды по геометрии; отдельный `clamp_to_answer` после `resolve` проще и уже протестирован (`gateway_video.rs:1363`, `:1406`). В A2 caps становятся 5-м слоем, но **после** маппинга в пиксели одной функцией.

### A.5. Engine ceiling: где именно кламп (и почему H.265 egress = запрет)

```
EGRESS ceiling (v1):
  width  ≤ 1920   (канвас композитора, F7)
  height ≤ 1080   (там же)
  bitrate ∈ [64, 8192] кбит/с   (эталонный кламп, см. §D)
  fps ∈ [15, 60]                (эталонный кламп, см. §D)
  codec = H264 only             (H.265 egress запрещён, причина ниже)
```

Обоснование цифр:

* **Размер**: отдавать больше канваса бессмысленно — композит всё равно 1920×1080 (апскейл только жжёт CPU). Потолок кодек-воркера 4096×2160 (F8) **не** является egress-потолком; это потолок для будущих канвасов (O1) и записи.
* **Битрейт 8192**: при 1080p30 формула записи даёт 6220 кбит/с для BEST (см. §D), т.е. 8192 покрывает лучший класс с запасом; выше — уже не про этот канвас.
* **H.265 egress — запрет**: измерено `[ЗАМЕР]` 5.84 CPU-секунды на 1 секунду видео 1080p30 (`[MEAS]02_protocol_chain.md:529`, libx265 ultrafast, `-threads 1`) → **5.84 ядра за один real-time поток** `[РАСЧЁТ]`. При мандате 60 устройств это физически неприемлемо; политика Lead — «H.265 только ingress». Значит `from_effective` не должен выбирать `H265` для egress **даже если** L2/L3/L4 разрешают: нужен финальный гейт `if egress && codec == H265 { H264 }` + однократный `warn!` (иначе оператор не поймёт, почему H.265 «не работает»).
* **fps 15**: ниже 15 на композитном egress rate-control деградирует (x264 при `-g fps` и малом fps тратит бюджет на IDR); эталон тоже не принимает <15 (`[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:159-161`). Детали и компромисс — §D.

### A.6. Почему это дешёвый выигрыш (без преувеличений)

| Аргумент | Факт |
|---|---|
| Логика уже написана и протестирована | `resolve()` + 5 тестов (`media_policy.rs:156+`), включая пересечение кодеков и clamp |
| Заменяются 3 ad-hoc ветки, а не пишется новая | `apply_video_settings` + `apply_call_string_settings` + `from_env` (F2) → 1 вызов + 2 маппинга |
| Снимает реальный баг | inbound SIP игнорирует настройки комнаты (F3) — воспроизводимо тестом без нагрузки |
| Нет изменений публичных контрактов | `LayoutDescriptor`/`MediaEngine` не трогаются (F6); ADR не нужен |
| Не требует замеров для приёмки | Критерий — совпадение существующих тестов + 1 новый тест равенства путей |
| Риск конфликта с `media-core` | низкий: `gateways.rs` и `gateway_video.rs` правятся точечно (маппинги), engine не трогается |

Ожидаемый эффект: **не CPU**, а корректность (единая политика) и предсказуемость (inbound=outbound). CPU-выигрыш даёт только §B.

---

## B. Encoder farm (M3) — спека под O7

### B.1. Цель, границы, взаимные исключения

**Цель:** один H.264-энкодер на группу зрителей с идентичными параметрами выхода; каждому зрителю — свой RTP-поток (свой SSRC/seq/ts), как уже сделано (F10).

**Не цель:** per-viewer композиция (ADR-0029, `proposed`). **Жёсткое ограничение:** ферма и per-viewer дельта **несовместимы на уровне энкодера** — если двум зрителям нужен разный кадр (self-view), один битстрим им отдать нельзя. Варианты:
1. v1 фермы: только зрители без self-view (или комната целиком без self-view — сейчас self-view по умолчанию выключен (`engine.rs:120`), переключатель — `engine.rs:957`);
2. позже: self-view зритель получает **отдельный** энкодер (таких мало), остальные — групповой.

Ферма **не** заменяет ADR-0029; она фиксирует, что «персональная дельта» — это отдельный, дорогой профиль.

### B.2. Ключ группы

```rust
/// Полный набор параметров, от которых зависит битстрим H.264.
/// Если хоть одно поле различается — битстримы несовместимы.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EncoderKey {
    pub room: RoomId,              // изоляция комнат (раскладка и кадры разные)
    pub width: u32,                // выход, px (из OutputProfile)
    pub height: u32,
    pub fps: u32,
    pub codec: GatewayVideoCodec,  // v1: только H264
    pub bitrate_kbps: u32,         // из resolve (L2/L3/L4)
    pub maxrate_kbps: u32,         // сейчас = bitrate (gateway_video.rs:362-365)
    pub bufsize_kbps: u32,         // сейчас = 2×bitrate
    pub profile: String,           // baseline/main/high
    pub level: String,             // 3.0/3.1/4.0
    pub keyint: u32,               // сейчас = fps (IDR раз в секунду, F11)
    pub canvas_w: u32,             // 1920
    pub canvas_h: u32,             // 1080
    pub canvas_version: u64,       // layout version/geometry hash — инвалидция при смене раскладки
}
```

**Layout hash:** включать не саму раскладку, а `canvas_version` — монотонный счётчик, который инкрементится в `RoomState` при **любом** изменении геометрии/раскладочного hash (**контракт Lead**, владелец `media-core`; сейчас есть `layout.version` в `LayoutDescriptor`, `crates/vks-core/src/ports/media_engine.rs:169`, и `LayoutSnapshot`, `engine.rs:627-634`). Геометрия раскладки на битстрим не влияет (влияет только содержимое кадра), поэтому version нужен лишь для того, чтобы **не переиспользовать** энкодер, если канвас/размер изменился (O1).

### B.3. Где хранить

| Что | Где | Почему |
|---|---|---|
| `BTreeMap<EncoderKey, Arc<EncoderGroup>>` | `vks-media-mcu::farm::EncoderFarm`, поле в `McuEngine` | Ферма — свойство медиаядра, а не HTTP-слоя; тестируется без сервера |
| `BTreeMap<ParticipantId, EncoderKey>` | `RoomState.outputs` (новое поле) | Профиль участника теперь живёт рядом с раскладкой (готовит A3) |
| `Arc<EncoderGroup>` (refcount + broadcast) | внутри farm | Время жизни = число зрителей + 1 (сам энкодер) |
| Статистика группы | `EncoderGroup.stats` (`AtomicU64`: frames, keyframes, dropped_au, restarts) | Per-session `encoded_video_frames` больше не принадлежит сессии (см. B.8) |

`RoomState` уже держит `subscribers`, `layout`, `stats`, `frames` (`engine.rs:45-90`) — добавление `outputs` не меняет публичный контракт `MediaEngine` (F6 остаётся в силе).

### B.4. API (форма; реализует `media-core`)

```rust
pub struct EncoderFarm { groups: Mutex<BTreeMap<EncoderKey, Arc<EncoderGroup>>> }

pub struct ViewerHandle {
    pub key: EncoderKey,
    pub idr_rx: tokio::sync::broadcast::Receiver<Arc<AccessUnit>>,
}

pub struct AccessUnit { pub data: Arc<[u8]>, pub is_idr: bool, pub pts_us: u64 }

impl EncoderFarm {
    /// Идемпотентно: группа создаётся, если её нет; refcount += 1.
    pub async fn acquire(&self, key: EncoderKey) -> Result<ViewerHandle, FarmError>;

    /// refcount -= 1; при нуле группа останавливается и удаляется.
    pub fn release(&self, key: &EncoderKey, viewer: ViewerHandle);

    /// Смена раскладки/канваса: помечает все группы комнаты устаревшими;
    /// новые acquire идут по новому canvas_version, старые доживают своих зрителей.
    pub fn invalidate_room(&self, room: RoomId);

    /// Диагностика для /metrics и /api/v3.
    pub fn snapshot(&self) -> Vec<GroupStat>; // key, refcount, fps_actual, keyframes, restarts
}
```

**Реализация энкодера:** переиспользовать `vks_media_mcu::codec_worker::ffmpeg::FfmpegWorker` (изолированный процесс, kill process-group по ADR-0028): `spawn(WorkerMode::Encode, slot, config)` (`codec_worker/ffmpeg.rs:377`), `write_video(&[u8])` (`:656`), `read_encoded_video() -> Option<Vec<u8>>` (`:546`), `restart()` (`:832`), `kill()` (`:815`). Это заменяет «сырой» `Command` в `gateway_video.rs:304-400` и даёт единый механизм перезапуска.

**Условие фермы — убрать «один resize на зрителя» (parent 0.76 ядра/устройство в замере).** По порядку, как в мастер-плане (`P2`):

1. **O3 (основной, уже измерен):** оптимизировать `resize_i420` строчными срезами + Bresenham — `[ЗАМЕР]` 8.0 → **2.0–2.8 мс/кадр**, это **−0.14 ядра/устройство** (`[MEAS]BENCHMARK_REPORT.md:67,173`). Resize остаётся в Rust — проще, чем новый процесс.
2. **O1 (следом):** холст = профиль выхода (`identity` вместо resize) — **−0.20 ядра/устройство** (`:68-69`); для 720p-комнаты композит 1080p (5.23 мс) → ~2.3 мс/кадр — **прогноз**, не замер.
3. **Альтернатива (не выбирать без нужды):** масштаб в FFmpeg (`-vf scale`, вход = канвас) — убирает resize из parent, но **не измерена**; брать только если O3+O1 не хватает.

Порядок важен: O3/O1 дают эффект и **без** фермы; ферма добавляет «энкодер 0.31 на комнату, а не на устройство» (O7, `:70-71`).

### B.5. Жизненный цикл

| Событие | Действие |
|---|---|
| **join** зрителя | `acquire(key)`; если группа новая — spawn энкодер, первый IDR придёт ≤1 с (keyint=fps, F11); зритель дропает не-IDR до первого IDR |
| **join**, группа уже есть | attach к broadcast; **не** ждать IDR-спавна; при этом допустима задержка до 1 с — приемлемо, либо `PLI` вверх (см. B.6) |
| **leave** | `release`; при refcount=0 — `finish_input` + `kill` |
| **смена раскладки** | `invalidate_room(room)` → новые ключи с новым `canvas_version`; старые группы доживают; **никакого drop'а зрителей** |
| **смена профиля участника** (L4) | release старый ключ → acquire новый; между ними ≤1 с чёрного кадра (IDR гейт) — зафиксировать в приёмке |
| **краш энкодера** | `FfmpegWorker::restart()` (перезапуск процесса, до 3 попыток); если не поднялся — деградация: зрители группы переводятся на per-viewer энкодер (fallback-путь текущего кода), группа удаляется; счётчик `restarts` в метрики |
| **комната пуста** | все группы room удаляются (нет утечки процессов) |
| **OOM/pressure** | M12-лимит `max_encoders`: `acquire` сверх лимита → `FarmError::Overloaded`, зритель получает `503/488` (существующая семантика отказа, `gateways.rs:795`) — **не** молчаливый дроп |

### B.6. PLI → групповой keyframe

Факты: шлюз шлёт `PLI` **вверх** раз в секунду по таймеру (`gateway_video.rs:673-680`), keyint=1 с (F11). Политика фермы:

1. **Не форсировать IDR перезапуском энкодера** — перезапуск x264 стоит десятки мс и рвёт поток всем зрителям.
2. Новый зритель: ждать очередного IDR (≤1 с) — при keyint=fps это дешёво; опционально — один `PLI` вверх при attach, чтобы ускорить (существующий механизм).
3. Если keyint когда-нибудь увеличат (>2 с) — добавить x264 `-force_key_frames`/`intra-refresh` политику; **в v1 не нужно**.
4. PLI от зрителя (потеря SPS/PPS): обрабатывать на уровне сессии — зрителю нужен повтор SPS/PPS, а не новый IDR: держать кэш SPS/PPS последнего IDR в группе и отдавать новому зрителю (иначе после потери пакета чёрный экран до 1 с). **Это новое требование фермы**, в текущем per-session коде не нужно (энкодер один и тот же и живёт с зрителем).

### B.7. Per-viewer RTP (что остаётся, что меняется)

| Слой | Сейчас (F10) | При ферме |
|---|---|---|
| SSRC | свой на сессию (`gateway_video.rs:506`) | **остаётся свой** |
| seq | свой счётчик (`:572`) | **остаётся свой** (по зрителю) |
| timestamp | свой, от `Instant::now()` (`:580`) | **остаётся**; но для одинакового AU у разных зрителей ts может расходиться — допустимо (разные сессии) |
| packetize | per-session (`:582-600`) | per-session, из общего `Arc<[u8]>` (нулевые копии NAL) |
| буфер кадра → энкодер | канал ёмкостью 1 (`:502`) | broadcast-приёмник с drop-oldest при отставании (`cap=2`), чтобы медленный зритель не тормозил энкодер |
| RTCP SR | per-session (`:673`) | **остаётся** per-session (SR — про поток зрителя) |

### B.8. Что ломается и как чиним

| Ломается | Почему | Митигация в спеке |
|---|---|---|
| `GatewayMediaStats.encoded_video_frames` | сейчас инкрементится в per-session цикле вывода энкодера (`gateway_video.rs:609`) | **Контракт Lead (§E.2):** `encoded_video_frames` — **только группа**, семейство `encoder_group_*` (fps, kbps, dropped, restarts); per-viewer — семейство `viewer_rtp_*` (packets_sent, seq/loss/jitter). Не делить на зрителей и не дублировать |
| `dropped_video_frames` | при ферме дроп — это «зритель отстал», а не «энкодер не успел» | разделить: `group_dropped_au` (энкодер) и `viewer_dropped_au` (broadcast lag) |
| Self-view / ADR-0029 дельта | групповой битстрим не может содержать разные кадры | B.1: v1 — без self-view; self-view — отдельный энкодер |
| SRTP per-viewer | **не ломается**: шлюз plain RTP (F12); WebRTC-плечо (str0m) шифрует каждое соединение отдельно и в v1 композит не получает | ничего делать не нужно; при будущем MCU→WebRTC композите SRTP останется per-connection |
| Статистика по кодекам в `/api/v3` | `video_codec_running` — per-session Bool (`gateways.rs:196`) | оставить (значение = «зритель получает видео»), но добавить `group_refcount` в диагностику |
| Нагрузка на CPU при join-шторме | 60 зрителей одновременно = до 60 acquire | keyint-гейт и общий broadcast — дёшево; проверить soak'ом |
| Разные caps у зрителей (max-fs/max-br) | зрители с разными caps не могут делить группу | caps применяются **до** ключа: `resolve → clamp_to_answer → EncoderKey`; если caps различаются — разные группы (это правильная деградация, но число групп растёт; лимит `max_encoders`) |

### B.9. Ожидаемый эффект и приёмка

`[РАСЧЁТ]` на основе замеров (`[MEAS]BENCHMARK_REPORT.md` §2, §2.2): сейчас 1 SIP 720p25 = **1.40** ядра (0.76 parent + 0.31 enc + 0.34 dec). После O1+O3+O7 для комнаты на 20: **0.39** ядра/устройство (dec 0.34 + RTP ≈0.05) + общее на комнату (enc 0.31 + композит 0.1 + аудио 0.05) ≈ 8.3 ядра ⇒ **≈0.415 ядра/устройство, 2.4 устройства/ядро** на **худшем** контенте (прогноз Lead, `BENCHMARK_REPORT.md:73-77`). Итог — **≈3.4×** по CPU на устройство (1.40 → 0.415); на реальном контенте (dec 0.12, enc 0.15) — **≈5 устройств/ядро** (`:76-77`). Вклад: ферма снимает 0.31 ядра/устройство (энкодер), O3+O1 — остальное.

Приёмка O7:
1. Битстрим группы побайтово одинаков для всех зрителей группы (тест: 2 зрителя, сравнить NAL-последовательности без RTP-заголовков).
2. `SSRC` у зрителей разные (тест).
3. При leave одного из двух — второй продолжает получать кадры (тест на refcount).
4. При смене раскладки зрители не отваливаются; `canvas_version` инкрементится, старые группы умирают.
5. IDR-гейт: новый зритель не получает дельта-кадры до первого IDR (тест).
6. Краш энкодера: до 3 restart, затем fallback; счётчик `restarts`.
7. Процессов FFmpeg = число групп + число декодеров, не 2×зрителей (тест на `pgrep -c ffmpeg`).
8. Метрики: `group_refcount`, `group_frames`, `viewer_dropped_au` в `/metrics`.

---

## C. Resource model: перенос формулы в наши единицы и сверка с live-цифрами

### C.1. Что именно говорит эталон (в его единицах)

`[ЭТАЛОН]` `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:307-373` (дизассемблирование `vtsrv@0xa64770`):

```
idx            = resolution_class − 21                    # 80 классов (21..100)
cost_enc(N,fps)= max(16, (N*C[idx] + B[idx]) * fps / 16)  # mode 1: кодирование
cost_one(fps)  = max(16,  A[idx] * fps / 16)              # mode 0: декодирование
units          = 32768                                    # 100 % нагрузки
raw_load       = round(load / 16);  encoder_limit = (total_load + 8) >> 4
калибровка     = NNLSM по базису [w, h, w*h]:  a*w + b*h + c*w*h = 983040 / fps
load(w,h,fps)  = (a*w + b*h + c*w*h) * (fps/30)
```

`[ЭТАЛОН]` live-значения: `limits.performance = limits.encoders = 106`, `decoders = 1696 = 106 × 16` (`[TC]00_DOCUMENTATION/LIVE_VALUES.md:42`), т.е. **бюджет ≈ 106 × 16 = 1696 единиц 1/16 ≈ 1688** (`MASTER_ARCHITECTURE.md:352-357`).

**Структурные выводы (важны для нас):**
1. `N` (число outputs) стоит **внутри** стоимости энкодера: `(N*C + B)` ⇒ эталонная модель предполагает **отдельный кодировщик на получателя** без совместного использования (подтверждает `vsm_spec.md:41-49`). Фермы (M3) в модели TrueConf **нет** — она приходит от Vinteo (`[VI]12_Архитектура_микшера_MCU.md:186-192`, `clientscnt`).
2. Минимум 16 единиц (= 1 целая единица ресурса) — floor; именно поэтому `106 = 1688/16` в точности.
3. Микширование/композит в модели **не учитываются** вообще (счётчики только enc/dec).

### C.2. Перевод в наши единицы

Примем `1 mcore = 1/1000 ядра`. Измерено на замороженном бинаре (`[MEAS]BENCHMARK_REPORT.md`): энкодер 720p30 = **0.31** ядра (`:41`), декодер 720p = **0.34** (худший синтетический контент, `:42`) и **0.12** (локальный ffmpeg на том же потоке, `:76-77`).

Если 106 — предел для **класса 720p30** (иначе арифметика не сходится: 106 × 0.54 = 57 ядер > 36 `[РАСЧЁТ]`), то:

```
1 единица эталона (1/16) = 310 mcore / 16 = 19.4 mcore
бюджет 1688 единиц ≈ 32.7 ядра   [РАСЧЁТ] — 91 % от 36 ядер, остальное ОС/аудио
```

Отсюда **эталонная** цена в наших единицах:

| Класс | Формула эталона | mcore (1 ед. = 19.4) | Наш `[ЗАМЕР]` | Расхождение |
|---|---|---|---|---|
| encoder 720p30 | 16 ед. (floor) | 310 | 310 (`BENCHMARK_REPORT.md:41`) | 0 % (по построению) |
| encoder 1080p30 | если ∝ площади: 36 ед. | 700 | 540 (`02_protocol_chain.md:524`) | эталон **завышает 1.3×** |
| decoder 720p30 | 1 ед. (1696 = 106×16) | 19 | 120 (локальный) / 340 (худший) (`BENCHMARK:42,76`) | **занижает 6.2× / 17.5×** |
| decoder 1080p30 | 1 ед. (там же) | 19 | 180 (`02_protocol_chain.md:528`) | **занижает 9.3×** |
| mixing/композит 1080p30 | — (нет в модели) | — | 157 mean / 399 p99 (5.23 мс × 30) | **модель не учитывает вовсе** |

### C.3. Где модель эталона расходится с нашими замерами (и почему)

| # | Расхождение | Факт | Гипотеза причины (помечена) |
|---|---|---|---|
| D1 | Декодер в модели в 6–17 раз дешевле нашего | 1696 декодеров на 1688 ед. ⇒ 1 ед./декодер | `[ОЦЕНКА]` их декодеры идут через Intel IPP/MFX и/или счётчик считает канал, а не поток; **не проверено** (нет доступа к их коду) |
| D2 | Энкодер 1080p по площади дороже нашего | 0.70 (эталон) vs 0.54 (замер) | `[ОЦЕНКА]` класс-таблица не строго ∝ площади; x264 ultrafast на Haswell эффективнее их калибровки |
| D3 | У эталона `N` зрителей увеличивают стоимость энкодера линейно | `(N*C+B)` | `[ЭТАЛОН]` следствие модели «output = отдельный кодировщик»; при ферме маргинальная стоимость зрителя = только пакетизация (≈5–10 mcore `[ОЦЕНКА]` — даст P4) |
| D4 | Композит/микс не в модели | 5.23 мс/кадр mean, 13.3 p99 (`[MEAS]03_media_engine.md:119`) | 157 mcore/комната mean, 399 p99 `[РАСЧЁТ]` — наш бюджет обязан включать эту строку |
| D5 | fps-масштабирование линейно у эталона | `*fps/16` | `[ЗАМЕР]` 1080p30 0.54 vs 1080p60 0.98 (`02_protocol_chain.md:524-525`) = 1.81× при 2× fps ⇒ **сублинейно**; наша формула должна иметь базовую часть (per-frame overhead) |
| D6 | Эталонный `resize` невидим (внутри iproc), у нас — отдельная строка | 8.0 мс = 0.24 ядра (до O3), 2.0–2.8 мс = 0.06–0.08 (после O3) | `[ЗАМЕР]` `BENCHMARK_REPORT.md:67`; при O1 (канвас=профиль) resize заменяется identity `[РАСЧЁТ]` |

### C.4. Рекомендуемая модель для нас (v1)

Сохранить **форму** эталона (класс → база; fps-множитель; N внутри), но калибровать на нашей ВМ:

```
mcore_unit = 1000 mcore = 1 ядро
cost_enc(key, N) = clamp_min(16, (N*C_enc + B_enc[class]) * fps / 30)   # mcore
cost_dec(class)  = B_dec[class] * fps / 30                              # mcore, не зависит от N
cost_mix(room)   = 157 * fps / 30                                       # mcore/комната
budget           = 1000 * (cores_available)                             # mcore
encoder_limit    = (budget - cost_mix_total) / cost_enc(1)
```

Измеренные константы (все из замороженного бинаря): `B_enc[720p30]=310`, `B_enc[1080p30]=540`, `B_dec[720p]=340` (худший синтетический — для SLA) или `120` (реальный контент), `B_dec[1080p]=180`, `cost_mix=157` mean / `399` p99. **Единственная оставшаяся `[ОЦЕНКА]`** — `C_enc = 5..10` mcore (маргинальная цена зрителя в пакетизации); её даст P4 (матрица), до этого консервативно берём 10. Деградация — лестница эталона (`MASTER_ARCHITECTURE.md:228-238`): сократить outputs → mute stream → restrict decoders → format change; триггер — `current > budget`.

**Почему не копировать 106/37/1696:** они калиброваны на их CPU; копирование = гарантированная ошибка ёмкости (зафиксировано в `01_reference_mining.md` M4). Наш бюджет — 32.7 «эталонных» ядра на 36 физических, но с **нашими** декодерами (340 mcore против эталонных 19) реальная ёмкость в 6–17 раз ниже эталонной по декодерам.

### C.5. Таблица «класс устройства → ожидаемое ядро/сессия»

**(а) Сейчас — измерено (`BENCHMARK_REPORT.md` §2):**

| Класс | Ядро/устройство | Статус |
|---|---|---|
| 1 SIP 720p25, худший синтетический контент | **1.40** (parent 0.76 + enc 0.31 + dec 0.34) | `[ЗАМЕР]`, `:31,:36-43` |
| 5 сессий в комнате | **1.56** | `[ЗАМЕР]`, `:32` |
| 10 сессий в комнате | **1.69** | `[ЗАМЕР]`, `:33` |
| 20 сессий | 0.60 — **ложное занижение** (7–8 не в композите) | `[ЗАМЕР]`, `:34,:96-110` |
| Аудио-only при согласованном видео | **0.89** | `[ЗАМЕР]`, `:45-46` |
| H.265 1080p30 egress | **5.84** (непригодно) | `[ЗАМЕР]`, `02_protocol_chain.md:529` ⇒ запрет (A.5) |
| WebRTC-участник (SFU, без MCU-egress) | — | `[НЕ ПРОВЕРЕНО]` (бота нет) |
| 1080p30-устройство | — | `[НЕ ПРОВЕРЕНО]` (замер только 720p25) |

**(б) После O1+O3+O7 — прогноз Lead, не замер (`BENCHMARK_REPORT.md` §2.2):**

| Класс | Ядро/устройство | Комментарий |
|---|---|---|
| 720p30, худший синтетический контент | **0.39 + общие/комната** (комната 20: ≈0.415) | dec 0.34 + RTP 0.05; enc/композит/аудио — на комнату |
| 720p30, реальный контент | ≈0.2 | dec 0.12, enc ~0.15 (`:76-77`) |
| 1080p30 | ×1.5–2 к 720p30 `[ОЦЕНКА]` | dec 1080p не измерен ⇒ замер до приёмки |
| Композит | 5.23 мс → ~2.3 мс для 720p-канваса (O1) | `:68-69`, прогноз |

**Важно:** обе таблицы — про **CPU контура** (parent+дети), без изоляции; это верхняя оценка, а не SLA (задержка/потери не измерялись, `BENCHMARK_REPORT.md:5-8,126-138`).

### C.6. Арифметика мандата (60 участников: 20 SIP + 20 H.323 + 20 WebRTC)

База — измеренное (`BENCHMARK_REPORT.md` §2): 720p25-устройство = 1.40 ядра; 5 → 1.56; 10 → 1.69. WebRTC-устройство — SFU-fanout, без MCU-egress ⇒ `[ОЦЕНКА]` 0.03 (бота нет). Распределение 40 шлюзовых (20 SIP + 20 H.323) — сценарий, не факт мандата.

| Сценарий | Расчёт | Итог |
|---|---|---|
| **Сегодня** | 40 × 1.40 … 40 × 1.69 + 20 × 0.03 + idle 0.02 | **56–68 ядер > 36** `[РАСЧЁТ]` ⇒ мандат **невыполним** |
| **После O1+O3+O7** (худший контент) | 40 × 0.39 + 3 группы × 0.31 + композит 0.1 + аудио 0.05 + 20 × 0.03 + 0.02 | **≈17.3 ядра** `[РАСЧЁТ]` ⇒ выполним, запас ~2× |
| **После O1+O3+O7, все 40 в 1080p30** | decoder 1080p не измерен: `[ОЦЕНКА]` 0.5–0.7/устройство | **≈22–30 ядер** ⇒ на границе; без замера приёмка невозможна |
| **С H.265-egress** | +5.84 на поток | **невыполним** ⇒ запрет A.5 обязателен |

`devices_per_core` по мандату: сегодня `60/56…60/68 ≈ 0.9–1.1`; после O1/O3/O7 `60/17.3 ≈ 3.5` `[РАСЧЁТ]`.

**Функциональный блокер важнее CPU:** при 20 участниках дефолтная раскладка (13 окон) **молча теряет 7–8** из композита (`BENCHMARK_REPORT.md:96-110`, лог `engine.rs:605`) — и CPU при этом *падает*, т.е. «20 устройств» на дефолтном пресете физически не тестируются. Для мандата 60 нужен P0-5 (авто-геометрия + пейджинг 25) **до** замеров ёмкости; без него цифры §C.5/C.6 не про мандат.

### C.7. Метрика «устройств на 1 физ. ядро при MOS ≥ 4.0» — как считать

Определение (для `perf-lab`, после P4a):

```
devices_per_core = N_video_devices / CPU_cores_used_by_media_path
CPU_cores_used   = usage_usec контура / wall_time        # cpu.stat, ловит parent+ffmpeg+сайдкар
SLA-класс        = U (70/85/95 %) — запас по CPU под джиттер/пики
MOS ≥ 4.0        = p95 OWD ≤ 150 мс, loss ≤ 0.5 %, jitter ≤ 30 мс (E-model R≥80)  [ЗАДАНИЕ]
```

**Измерено сейчас** (`BENCHMARK_REPORT.md:51-57`): `c_device = 1.40 ядра` ⇒ nominal `1/1.40 = 0.71` устройства/ядро; по SLA-классам A/B/C (U = 70/85/95 %) — **0.50 / 0.61 / 0.68** устройства/ядро, т.е. **18–24 устройства на 36 ядер**. Оценка «1.48 сейчас» из моей спеки (60/40.7) — nominal по смешанному сценарию C.6, а не SLA-замер; для приёмки используем SLA-цифры.

**После O1+O3+O7 (прогноз §2.2):** 2.4 устройства/ядро на **худшем синтетическом** контенте, **≈5** на реальном (`BENCHMARK_REPORT.md:73-77`).

**Порог приёмки O7 — решение Lead (turn 4), двухрежимный:** **≥2.4 устройства/ядро на `testsrc2`** (худший синтетический контент) **и ≥3.0 на реальном контенте 720p30**. Почему два порога: по прогнозу §2.2 худший синтетический даёт **2.4**, реальный — **≈5**, поэтому единый порог 3.0 на синтетике недостижим, а 2.4 на реальном — занижен. На 1080p30 — ожидаемо ×1.5–2 меньше (порог пересчитать после замера 1080p-декодера). MOS до P4a = `[НЕ ПРОВЕРЕНО]`: у нас нет приёма RTCP и JB (`BENCHMARK_REPORT.md:132-134`), поэтому SLA-классы пока — по CPU, а не по MOS.

### C.8. Что инструментировать, чтобы закрыть C

1. `vks_mcu_tick_duration_seconds{phase}` — уже есть (`vks-common/src/metrics.rs:135-177`, env `VKS_MCU_TICK_METRICS=1`).
2. Добавить (M12-набор): `mcu_encoders{key}` / `mcu_encoder_refcount`, `mcu_encode_cores_estimate`, `mcu_resize_seconds`, `mcu_decoder_queue_depth`, `mcu_group_restarts`.
3. `budget/used` в `/api/v3/engine/diagnostics`-аналоге: `{budget_mcore, used_mcore, encoders_limit, encoders_current}` — это прямой перенос идеи `limits.performance` из эталона (`[TC]00_DOCUMENTATION/LIVE_VALUES.md:42`), но со своими числами.
4. Калибровочный прогон: 1 энкодер N-классов + 1 декодер, замер `utime/stime` существующего бинаря (sha256 `8ab81afd…`) — заполняет `B_enc/B_dec/C_enc`.

---

## D. Формула записи и клампы: что выставить в v1 и почему

### D.1. Эталон

```
[ЭТАЛОН] bitrate_kbps = W × H × fps × bpp / 1000     (floor),  bpp = {BEST: 0.1, GOOD: 0.067, ECONOMY: 0.033}
         проверка эталона: 1920×1080@30 → 6220 / 4167 / 2052 кбит/с (совпадение до 0.01 %)
         источник: [TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:173-174, :301-303
[ЭТАЛОН] клампы ABOOK: bitrate ∈ [64…8192] кбит/с, fps ∈ [15…60], width/height НЕ валидируются
         источник: [TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:159-161
```

Проверка формулы нашим калькулятором: `1920*1080*30*0.1/1000 = 6220.8 → 6220`; `×0.067 → 4167.936 → 4167`; `×0.033 → 2052.864 → 2052` — эталонные числа воспроизводятся точно (floor).

### D.2. Наши два пути сегодня (и расхождение между ними)

| Параметр | Gateway (в работе) | `media_policy` (не подключён) | Совпадает? |
|---|---|---|---|
| bitrate | `clamp(64, 8192)` — `gateway_video.rs:1071`, `:1093` | `clamp(32, 50 000)` — `media_policy.rs:77-78` (применение `:132-140`) | **нет** |
| width | `clamp(16, 1920)` — `gateway_video.rs:1075`, `:1097` | `clamp(16, 7680)` — `:79-80` | **нет** |
| height | `clamp(16, 1080)` — `gateway_video.rs:1078`, `:1100` | (то же) | **нет** |
| fps | `clamp(1, 60)` — `gateway_video.rs:1066`, `:1081`, `:1103` | `clamp(1, 60)` — `:81-82` | да |
| запись | модели нет (`vks-recorder` не содержит bpp/bitrate — grep пуст) | — | — |

Т.е. гейтвей уже живёт по эталонным клампам (кроме fps-min), а неиспользованный `media_policy` — по более широким. После A1 это расхождение стало бы **багом** (какой из путей главный), поэтому его надо закрыть до/вместе с wiring.

### D.3. Решение v1: три набора потолков (ceiling), а не один

```rust
// crates/vks-core/src/media_policy.rs (форма)
pub struct MediaCeilings {
    pub min_bitrate_kbps: u32, pub max_bitrate_kbps: u32,
    pub min_width: u32, pub max_width: u32,
    pub min_height: u32, pub max_height: u32,
    pub min_fps: u32, pub max_fps: u32,
}

pub const EGRESS: MediaCeilings = MediaCeilings {   // per-abonent видео шлюза
    min_bitrate_kbps: 64,   max_bitrate_kbps: 8192,
    min_width: 16,          max_width: 1920,
    min_height: 16,         max_height: 1080,
    min_fps: 15,            max_fps: 60,
};

pub const RECORDING: MediaCeilings = MediaCeilings { // запись (vks-recorder)
    min_bitrate_kbps: 64,   max_bitrate_kbps: 50_000,
    min_width: 16,          max_width: 3840,
    min_height: 16,         max_height: 2160,
    min_fps: 5,             max_fps: 60,             // запись медленного контента допустима
};

pub fn resolve(layers: &[MediaLayer]) -> EffectiveMedia;              // = resolve_with(layers, EGRESS)
pub fn resolve_with(layers: &[MediaLayer], c: &MediaCeilings) -> EffectiveMedia;
```

**Почему так:**
* `EGRESS` = эталонный кламп + наш канвас (1920×1080): отдавать больше канваса бессмысленно (A.5), а эталонный потолок 8192 покрывает 1080p30 BEST (6220) с запасом.
* `RECORDING` сознательно **шире эталона**: у эталона L1 ставит `max_bitrate = 8192` и размер до `[3840,2160]` (`[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:123-124`), т.е. 4K-запись там существует, но зажата в 8 Мбит/с (заметно для 4K). У нас 4K30 BEST = `3840*2160*30*0.1/1000 = 24 883` кбит/с, GOOD = 16 671, ECONOMY = 8 211 — все выше 8192, кроме ECONOMY. Поэтому recording-потолок 50 000 (наш текущий `MAX_BITRATE`), а формула сама решает, сколько нужно. **Это осознанное отклонение от эталона** (в сторону качества записи); цена — полоса на запись 4K, `[НЕ ПРОВЕРЕНО]` (не измеряли).
* `INGRESS` (декодирование) не клампится: мы декодируем то, что прислал терминал; ограничение — `MAX_VIDEO_*` кодек-воркера (4096×2160, `codec_worker/mod.rs:28-31`), а не политика.

**Про fps-min = 15 в EGRESS — компромисс, который надо зафиксировать явно:** эталон не принимает <15 (ABOOK), и наш x264-egress при fps<15 теряет эффективность rate-control. Но админка сейчас позволяет `fps_limit` от 1. Решение v1: egress-кламп поднимает <15 до 15 **с однократным `tracing::warn!`** (оператор видит, что значение изменено), а не молча. Если появится продуктовое требование 5–10 fps на egress — добавить отдельный preset, не снижая общий минимум.

### D.4. Формула записи: перенос и проверочные векторы

```rust
// Форма для параметров записи. В `crates/vks-recorder/src/` сегодня НЕТ ни bitrate, ни
// quality, ни перекодирования (grep пуст: запись не энкодит) — функция нужна, когда
// появится энкодер записи; место — модуль параметров записи, не hot path.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecordingQuality { Best, Good, Economy }

fn bpp(q: RecordingQuality) -> f32 { match q { Best => 0.1, Good => 0.067, Economy => 0.033 } }

pub fn recording_bitrate_kbps(width: u32, height: u32, fps: u32, q: RecordingQuality) -> u32 {
    let raw = (width as f64 * height as f64 * fps as f64 * bpp(q) as f64 / 1000.0).floor() as u64;
    raw.clamp(64, 50_000) as u32
}
```

| Вход | BEST | GOOD | ECONOMY |
|---|---|---|---|
| 1920×1080@30 | **6220** (эталон 6220) | **4167** (4167) | **2052** (2052) |
| 1280×720@30 | 2764 | 1852 | 912 |
| 3840×2160@30 | 24 883 | 16 671 | 8 211 |
| 3840×2160@60 | 49 766 | 33 343 | 16 422 |

Все значения `[РАСЧЁТ]` по формуле эталона; первые три сверены с эталонными числами (совпадение). Тест-векторы из таблицы — прямая приёмка реализации.

### D.5. Что НЕ переносить из клампов

* **«width/height не валидируются»** — не переносить: у нас канвас и кодек-воркер имеют жёсткие границы, невалидированные размеры = падение FFmpeg или OOM. Наша валидация (16..7680, чётные) — улучшение, оставляем.
* **bitrate 4096 из swagger** — устаревшее значение самого эталона (`[TC]…MASTER_ARCHITECTURE.md:159`); не используем.
* **4K-egress** — не вводим в v1: канвас 1080p, а H.265-egress запрещён (A.5); 4K остаётся для записи (`RECORDING` ceiling) и будущих канвасов (O1).

---

## E. Приёмка и открытые вопросы

### E.1. Чек-лист реализации (по разделам)

| # | Проверка | Раздел |
|---|---|---|
| 1 | Существующие тесты `gateway_video.rs:1324/1345` зелёные без правки ожиданий | A |
| 2 | Новый тест: inbound-SIP профиль == outbound при одинаковой комнате | A |
| 3 | `from_effective` никогда не отдаёт H265 на egress + один `warn!` | A.5 |
| 4 | Битстрим фермы идентичен у 2 зрителей группы; SSRC разные | B |
| 5 | refcount: leave одного не рвёт второго; пустая комната → процессов нет | B |
| 6 | IDR-гейт при join; кэш SPS/PPS для нового зрителя | B |
| 7 | O3: `resize` 1080→720 ≤ 2.8 мс/кадр; O1: композит ≤ 2.5 мс для 720p-канваса (метрики `resize_ms`/`composite_ms`) | B.4 |
| 8 | `/metrics`: group vs viewer метрики не смешаны | B.8 |
| 9 | Калибровка: `budget/used` в диагностике; лестница деградации | C |
| 10 | `recording_bitrate_kbps` проходит векторы D.4 | D |
| 11 | EGRESS/RECORDING ceilings применяются в разных путях; <15 fps → 15 + warn | D.3 |
| 12 | Метрики: `encoded_video_frames` только на группе; `encoder_group_*` vs `viewer_rtp_*` разделены | B.8, E.2 |
| 13 | `canvas_version` инкрементится движком при смене геометрии; ключ фермы его включает | B.2, E.2 |
| 14 | Answer SDP: H.265 не предлагается как send (recvonly); egress — H.264 CBP | A.5, E.2 |
| 15 | Приёмка O7: ≥2.4 устройства/ядро на `testsrc2` **и** ≥3.0 на реальном контенте 720p30 | C.7, E.2 |

### E.2. Решения Lead по открытым вопросам (закрыто) и контракты

Ответы Lead (turn 3) — это **решения**, а не вопросы; спека ниже обязана:

1. **Метрики.** Разделять `encoder_group_*` (на группу/битстрим: fps, kbps, dropped, restarts) и `viewer_rtp_*` (per-viewer: seq/loss/jitter). `encoded_video_frames` — **только group**; per-viewer — `viewer_packets_sent`. (См. B.8.)
2. **fps-min = 15 на EGRESS** — принято: поднимать до 15 с `warn!`, не молча (D.3).
3. **`canvas_version`** инкрементит **движок (`RoomState`)** при любом изменении геометрии/раскладочного hash; ключ фермы включает `canvas_version`; владелец — `media-core`. Это **контракт** (B.2, B.5).
4. **H.265 = только ingress** — принято: декодируем H.265 от терминала, **никогда** не кодируем на выход; в answer SDP H.265 не предлагаем как send-кодек (recvonly), egress — **H.264 CBP**. Причина: софт-x265 1080p30 = 5.84 ядра/поток.
5. **Калибровочный прогон** — делают Lead/`media-core` после O7 на живом стенде, **не ref-miner**; в спеке уже стоят измеренные константы (`BENCHMARK_REPORT.md` §2, см. §0, C.2, C.4), `[ОЦЕНКА]` осталась только для `C_enc` (маргинальная пакетизация зрителя).
6. **Приёмка O7 — двухрежимная (принято Lead, turn 4):** ≥**2.4** устройства/ядро на `testsrc2` (худший синтетический) **и** ≥**3.0** на реальном контенте 720p30 (C.7).

**A1 (wiring `media_policy`) — не в мой код:** отдельная задача **P1** после P0, владелец `proto-chain` (у него уже правки в `gateways.rs`). Спека фиксирует форму и приёмку; исполнение — вне ref-miner.

**Остаются открытыми (честно, без выдумывания):**
- **Декодер 1080p30 не измерен** (замер только 720p25) — от него зависит, выполним ли мандат 60 при верхней границе 1080p30 (C.6: ≈22–30 ядер без замера).
- **H.323 — 0 замеров** (бота нет, `BENCHMARK_REPORT.md:128-131`): ёмкость 20 H.323-устройств в C.6 — арифметика от SIP-замера, не измерение.
- **MOS/потери** — только после P4a (JB + приём RTCP, `:132-134`); до этого SLA-классы по CPU.
---

## Приложение. Индекс процитированного

**Наш код (файл:строка):** `media_policy.rs:20-42,59-68,77-82,87,99-109,132-140,156`; `lib.rs:36`; `gateways.rs:183-200,441-459,462-473,795,1188,1272-1275,2600,2626-2628,3475`; `gateway_video.rs:51-59,163,228,245-259,304-400,312,351,356-379,477-520,502,506,546,572,580-600,609,629-660,673-680,754-783,1059-1103,1250,1296,1324,1345,1363,1406`; `codec_worker/mod.rs:28-31,184-258`; `codec_worker/ffmpeg.rs:377,546,656,815,832`; `frame.rs:13-15`; `compositor.rs:20`; `engine.rs:45-90,120,140,627-634,957`; `media_engine.rs:165-183`; `uas.rs:992`; `uac.rs:73-74,170-176`; `rtp_bridge.rs:691-716`; `.cargo/config.toml:20`.

**Замеры команды:** `[MEAS]measure/PASSPORT.md:7` (sha256); `[MEAS]BENCHMARK_REPORT.md:26-83` (live-контур: 1.40/0.34/0.31 ядра, resize 8.0→2.0–2.8 мс, прогноз O1+O3+O7), `:96-138` (дефект раскладки, что не измерено); `[MEAS]02_protocol_chain.md:524-534`; `[MEAS]03_media_engine.md:28-29,119,132`; `[MEAS]06_perf_lab.md` (методика).

**Эталоны:** `[TC]00_DOCUMENTATION/MASTER_ARCHITECTURE.md:121-131,159-161,173-174,228-238,279-295,301-303,307-373`; `[TC]00_DOCUMENTATION/LIVE_VALUES.md:42`; `[TC]09_MEDIA_CORE/vsm_spec.md:41-49`; `[VI]12_Архитектура_микшера_MCU.md:186-192`; `[VI]07_Адвокат_дыявола_проверка_консенсуса.md:21`.
