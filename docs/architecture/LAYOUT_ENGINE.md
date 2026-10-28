# LAYOUT ENGINE — движок раскладок

> Статус: **v0.2 — приведено в соответствие с кодом (2026-10-23)**.
> Документ описывает **фактическую** реализацию. Нереализованные части явно
> помечены как «План» и не выдаются за работающие.
>
> Источник правок: ревью `dead-code-scout` / `elegance-reviewer` /
> `layout-spec-audit` (2026-10-22…23) и сверка с `crates/vks-layout/src/**`.

---

## 0. Карта модулей

| Файл | Что в нём |
|---|---|
| `crates/vks-layout/src/core.rs` | `Layout`, `Slot`, `SlotId`, `ParticipantId`, `LayoutKind`, `NormalizedLayout`/`NormalizedSlot` |
| `crates/vks-layout/src/rect.rs` | `Rect` (пиксели), `Canvas`, `NormalizedRect` |
| `crates/vks-layout/src/grid.rs` | `GridLayout::compute` — равномерная сетка N тайлов |
| `crates/vks-layout/src/focus.rs` | `FocusLayout::slot_rects` — геометрия «крупный тайл + полоса мелких» (единственная реализация: движок не держит копию); `main_window` — главное окно по площади (Codian §2.2). Пересадку жильцов делает движок (`RoomState::refresh_seating`), а не `vks-layout`: правило одно — «кто в каком слоте», включая порядок мелких ячеек по недавности речи |
| `crates/vks-layout/src/preset.rs` | 65 вендорных пресетов (`MANUAL_SCHEMES`), `cell_rect` — единая арифметика пикселей |
| `crates/vks-layout/src/codian.rs` | 54 раскладки Codian MCU 4200 (52 пресета + 2 кастомные) |
| `crates/vks-layout/src/roles.rs` | `SlotRole`, `assign_by_role`, `RotationState` — роли слотов (§2 спецификации Codian) |
| `crates/vks-media-mcu/src/layout_bridge.rs` | `PixelRect`, `LayoutBridge` — кроссфейд 300 мс |
| `crates/vks-media-mcu/src/compositor.rs` | `Compositor` — блит в канвас 1920×1080 |

---

## 1. Формат

```rust
// crates/vks-layout/src/core.rs
pub struct Layout {
    pub version: u32,             // инкремент на изменение
    pub kind: LayoutKind,         // Grid | Focus | Speaker | Custom
    pub slots: Vec<Slot>,         // порядок = z-порядок отрисовки (первый — снизу)
}

pub struct Slot {
    pub id: SlotId,                          // стабильный в рамках раскладки
    pub participant_id: Option<ParticipantId>, // None — слот свободен
    pub rect: Rect,                          // ПИКСЕЛИ канваса (не 0..1)
    pub z_index: i32,                        // больше — поверх
    pub pinned: bool,                        // «закреплён»
    pub is_focus: bool,                      // фокусный (крупный) слот
}

pub struct Rect { pub x: u32, pub y: u32, pub width: u32, pub height: u32 }
```

**Изменения относительно прежней версии документа (v0.1):**

1. `Rect` — **пиксельные** координаты, а не нормализованные `f32 0..1`.
   Нормализация — отдельный тип `NormalizedRect`/`NormalizedLayout`
   (`Layout::to_normalized(&Canvas)`), используемый на границе с клиентом.
2. `Slot.assignment: SlotAssignment` **не существует**. Привязка участника —
   это `Option<ParticipantId>` в слоте; «закреплённость» — флаг `pinned`.
   Роли (`Fixed`/`Active`/`Rolling`) живут отдельно, в `roles.rs`, и приходят
   из раскладки Codian, а не из общего `Layout`.
3. Тип идентификатора слота — `SlotId` (newtype над `u32`), не «голый `u32`».
4. Внешний `id` в API — **1-based** (`calculate` возвращает `position` 1..N),
   внутренний — **0-based**. Расхождение нормализуется в `vks-admin`
   (`base`-смещение), в `vks-layout` слоты всегда 0-based.

---

## 2. Роли слотов (Codian §2)

```rust
// crates/vks-layout/src/roles.rs
pub enum SlotRole { Default, Fixed, Active, Rolling }

pub fn assign_by_role(
    roles: &[SlotRole],
    fixed: &BTreeMap<usize, ParticipantId>,
    active: Option<ParticipantId>,
    participants: &[ParticipantId],
) -> Vec<Option<ParticipantId>>
```

Приоритет: `fixed` → `active` → `rolling` (порядок прихода) → `default`
(порядок прихода). Покрыта 5 юнит-тестами.

> **План:** движок MCU пока не применяет `assign_by_role` при авто-назначении.
> Порядок участников теперь **по приходу** (`join_order`), но роли
> (`Fixed`/`Active`/`Rolling`) движок не разбирает: голосовая активация
> использует отдельное правило §2.2 (см. §8). Остаток — в мастер-списке, S12.

---

## 3. Сериализация и хранение

* **Хранилище — SQLite** (`sqlx 0.8.6`), таблица `core_layouts`
  (`id, name, version, assigned_rooms, is_default`). PostgreSQL/`jsonb`,
  упомянутые в v0.1, в проекте **не используются**.
* **Сигналинг** — protobuf (`prost 0.13.5`), схема `vks-proto`.
  Сообщение `LayoutChanged` **отправляется** по двум поводам:
  1) `GatewayManager::apply_layout` — после применения раскладки из UI;
  2) голосовая активация — `McuEngine::refresh_seating` пересадил главное окно
     (`crates/vks-server/src/room_events.rs`).
  В обоих случаях источник — эффективная раскладка композитора
  (`crates/vks-server/src/layout_signal.rs`), поздний участник получает
  раскладку сразу после `JoinRoomResult`.
* Файл `vks-layout/src/storage.rs` **не существует**: преобразования
  `Layout ↔ JSON` и `Layout ↔ protobuf` живут в `vks-admin` и `vks-server`.

---

## 4. Версионирование

* `Layout.version: u32`, инкремент на каждое изменение (монотонная
  гарантия — в `vks-media-mcu::apply_layout` через `version_floor`).
* Новые поля — `#[serde(default)]` ⇒ обратная совместимость.
* Ломающие изменения — новый major + ADR.

---

## 5. MCU apply (server-side)

Фактический путь: `vks-media-mcu/src/layout_bridge.rs` + `compositor.rs`.

```rust
// layout_bridge.rs
pub struct PixelRect { pub x: u32, pub y: u32, pub w: u32, pub h: u32, pub z: i32 }

// compositor.rs — сортировка по (z, slot_id): больше z — поверх
stack_keys.sort_unstable_by_key(|key| (rects[key].z, *key));
```

* `PixelRect` дублирует `vks_layout::Rect` (+ поле `z`). Планируется свести
  к `vks_layout::Rect` с отдельной картой z-порядков.
* **Кроссфейд 300 мс** (`CROSSFADE_MS`) реализован в `LayoutBridge`.
* **Устаревший кадр** (участник переставлен в другой слот) устранён:
  `McuEngine::rekey_frames` переносит кадр за участником.
* Файл `vks-layout/src/mcu_renderer.rs` **не существует**.

---

## 6. Клиент

* Клиентский рендер — собственный `DrawCommand` в `client/vks-client-sdk`,
  а **не** CSS-позиционирование (v1 из старого документа не реализован).
* Раскладка на клиент **доходит**: `LayoutChanged` → push-хендлер
  транспорта → очередь конференции (`Conference::drain_events`) →
  `MediaEngine::set_layout` → плитки на экране; пока раскладки нет, клиент
  рисует авто-сетку. `Rect` в протоколе нормализован (0..1) — клиент
  масштабирует под свой холст (`NormalizedLayout::scale_to_canvas`).
* Владелец состояния раскладки — **конференция**: в SDK нет второй копии
  раскладки. Приложение рисует через `Conference::media()`.
* Источник истины на сервере — **эффективная раскладка композитора**
  (`McuEngine::current_layout`), а не дескриптор: в режиме `auto` дескриптор
  приходит без геометрии, и клиент получил бы пустой экран.
  Контракт зафиксирован в ADR-0030.

---

## 7. Пресеты

* **65 вендорных пресетов** (`MANUAL_SCHEMES` в `preset.rs`) — геометрия
  пересчитывается через `cell_rect` (целочисленная арифметика по модульной
  сетке, `floor`), без плавающей точки.
* **54 раскладки Codian** (`codian.rs`) — те же 65 + 54 доступны через
  `is_known_preset`; геометрия Codian разворачивается **тем же** `cell_rect`.
  Формат: области `Region{x0,y0,x1,y1,cols,rows}` на модульной сетке `den`
  + список PIP-окон. Подробно — `ARCHITECTURE_PLAN_2026-10-23.md` §3.
* Функций вида `grid_2x2()` / `focus_layout(n)` **нет**: есть
  `GridLayout::compute(&Canvas, n)` и `FocusLayout` (в `focus.rs`).
* `catalog()` и модуль `renderer.rs` **удалены** в 2026-10-23 (мёртвый код;
  `renderer.rs` дополнительно игнорировал `z_index`, т.е. был заведомо неверен).

---

## 8. Назначение участников

Фактическая семантика:

1. `pinned`/`is_focus` — флаги слота; автоназначение по ним **не реализовано**
   (в `vks-media-mcu` `pinned` и `is_focus` жёстко `false`).
2. `assign_by_role` (§2) — чистая функция, используется для кастомных
   раскладок Codian; движок пока не вызывает её автоматически.
3. `seat_participant` (`vks-media-mcu`) сажает участника в первый свободный
   слот раскладки; назначения вне `geometry` отбрасываются
   (`sanitize_assignments`), чтобы участник не получил «поверх сетки весь канвас».
4. **Голосовая активация (Codian §2.2)** — реализована: `RoomState::refresh_seating`
   пересаживает главное окно к активному спикеру, «Важный» участник
   (`set_important_participant`) важнее спикера. Главное окно определяется
   **по площади** (`vks_layout::main_window`), жильцы меняются местами —
   никто не пропадает из раскладки. Пересадка идёт по событию смены спикера,
   а не пересборкой на каждом тике; `Layout.version` растёт только при
   фактическом изменении. Равная сетка и фиксированный макет PNM голосом не
   перестраиваются (Codian §2.1: 52 макета статичны по геометрии).
   Остаток: порядок заполнения малых ячеек по убыванию речевой активности
   (per-viewer, фаза 4 ADR-0029) — мастер-список, S12.

---

## 9. Редактор (admin UI)

* Редактор раскладок — **вендорный UI TrueConf PNM 2.2.1.66**
  (`client/pnm-frontend/deployed/pnm/`), а не собственный.
* Операции API: `POST /api/v3/ui/video_layouts/calculate` (1-based `position`),
  `GET /api/v3/ui/video_layouts/presets` (справочник 119 пресетов).
* Hadron-core / Taffy / WebGL — **план**, не реализовано.

---

## 10. Синхронизация

| Слой | Состояние |
|---|---|
| Admin → storage (SQLite) | работает |
| Storage → signaling (NATS broadcast) | NATAS — заглушка (`vks-cluster`) |
| `LayoutChanged` → клиенты | работает (рассылка по комнате, источник — эффективная раскладка композитора) |
| Snapshot при входе в комнату | работает: снимок в поле `JoinRoomResult.layout` (ADR-0030) |
| Метаданные схемы (`preset`, `mode`) | работают: поля 4/5 `Layout`, атомарный `McuEngine::layout_snapshot` (ADR-0032) |
| `layout_id` на проводе | сознательно отсутствует: COW-нестабильный UUID админ-записи (ADR-0032) |

Полная карта разрывов — `ARCHITECTURE_PLAN_2026-10-23.md`, шаг 3.

---

## 11. Инварианты (фактические)

1. Слоты раскладки не пересекаются — гарантируется построителем
   (`GridLayout`, `cell_rect`, области Codian); проверяется тестом
   `crates/vks-layout/tests/codian_layouts.rs`.
2. Полные ряды основной сетки покрывают ширину канваса (без дыр и
   перекрытий); неполный последний ряд сохраняет ширину колонки и
   центрируется, поэтому сумма площадей равна площади канваса только при
   `n % cols == 0`. Исключение — холст уже `cols`: там ряд делит свою ширину
   (нулевые тайлы недопустимы). PIP-окна накладываются поверх с `z_order = 1`.
3. `z` определяет порядок отрисовки: больше — поверх; при равенстве —
   по `slot_id` (детерминированно).
4. Один участник — один слот в сетке.
5. `Layout.version` монотонно растёт.
6. `LayoutDescriptor.assignments` и `geometry[].slot` — всегда **0-based**
   (в `vks-admin` наружу отдаётся 1-based `position`).
