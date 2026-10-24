# ADR-0032: Метаданные раскладки на проводе (preset/mode) — без layout_id и без LayoutModeChanged

- **Статус:** accepted
- **Дата:** 2026-10-24
- **Решение принято:** владелец проекта (T10a трекера R6)
- **Связанные ADR:** 0030 (контракт раскладки в сигналинге), 0012 (layout engine), 0026 (режим MCU/SFU)
- **Связанные требования:** T10a/T10c раунда 6; L-6 в `LAYOUTS_UI_CHAIN_PLAN_2026-10-23.md`; `SIGNALING_PROTOCOL.md` §9

## Контекст (Context)

`LayoutChanged` отдавал клиенту только `version/kind/slots`. Схема, которую
выбрал модератор (Codian `codian_07`, конструктор `vks_grid_9`, вендорский
`preset_equal_25`), терялась на границе `admin → gateway → MCU`: `preset` и
`mode` читались локально в `GatewayManager::apply_layout` только для расчёта
геометрии и не попадали в `LayoutDescriptor`. `layout_id` тоже терялся, но
это UUID админ-записи (COW выдаёт новый на каждое «Применить»), а не
идентификатор схемы.

T10c требует прод-потребителя `LayoutChanged`. Такой потребитель есть —
`vks-client-sdk`: он получает `LayoutChanged`/`JoinRoomResult.layout` и не
имеет доступа к админскому REST, поэтому по одной геометрии не может
отличить выбранную схему от авто-сетки. PNM-редактор ходит в JSON
`/api/v3/ws`/REST и на signaling не подписан.

## Варианты (Options)

### Вариант A: `layout_id + preset + mode` в `Layout` + `LayoutModeChanged`

- Плюсы: максимум информации на проводе, отдельное событие о смене режима.
- Минусы: `layout_id` — внутренний COW-нестабильный UUID без потребителя;
  смена режима **всегда** уже приводит к `LayoutChanged` (mode-endpoint →
  `push_layout_to_engine` → `apply_layout` → broadcast), поэтому второе
  сообщение дублирует состояние и создаёт гонку порядка кадров без общей
  версии; снапшот `JoinRoomResult` пришлось бы дублировать в двух каналах.
- Риски: рассинхрон «layout v3 + mode v2», лишний трафик, больше кода.

### Вариант B: не расширять signaling, только ADR

- Плюсы: ноль кода; рендеру хватает effective geometry.
- Минусы: SDK-потребитель не узнаёт схему; L-6 остаётся открытым; T10c
  вырождается в «PNM читает REST» и не проверяет signaling-контракт.
- Риски: контрактный долг копится, аддитивные поля всё равно понадобятся.

### Вариант C: `preset + mode` в `Layout`, один канал, атомарный снимок

- Описание: в `Layout` добавляются `string preset = 4`, `string mode = 5`;
  метаданные хранятся одной копией в `LayoutDescriptor`; `McuEngine` отдаёт
  `layout_snapshot` (геометрия + preset + mode) **под одним локом**; три
  производителя (`gateways::broadcast_layout`, `room_events`, снимок
  `JoinRoomResult`) переведены на него.
- Плюсы: аддитивно (старый клиент игнорирует теги 4/5); один канал —
  одно событие на одно изменение; версия геометрии и метаданные из одного
  применения; `layout_id` не утекает; T10c получает ровно то, что нужно.
- Минусы: два поля в `LayoutDescriptor` (механические `..Default::default()`) и нормализация на
  границе gateway.
- Риски: `mode` — зеркало админки, а не «как построена геометрия» (для
  `auto + явный preset` это разные вещи). Зафиксировано ниже и покрыто тестом.

## Решение (Decision)

Принят вариант C.

1. **`Layout.preset = 4`, `Layout.mode = 5`** (string). Без `layout_id`:
   это внутренний UUID админ-записи, COW-нестабильный, потребителя нет;
   PNM получает id через админский JSON.
2. **`LayoutModeChanged` не добавляется.** Смена режима — это изменение
   раскладки; отдельное сообщение дублировало бы `LayoutChanged` и требовало
   бы общей версии/дедупа. Если появится потребитель, которому нужен режим
   **без** геометрии, сообщение можно добавить аддитивно (поле 81 свободно).
3. **`LayoutDescriptor.preset/mode`** (`#[serde(default)]`, `"" = unknown`) —
   единственная копия метаданных. Заполняются ровно в одной точке:
   `GatewayManager::apply_layout` (`layout_meta`: trim, lowercase, mode только
   `auto|dynamic|manual`, preset ≤128 байт). `register_publisher` ставит
   `mode:"auto"` — авто-сетка без явного применения.
4. **`McuEngine::layout_snapshot(room) -> LayoutSnapshot`** читает
   `current_layout` и применённый дескриптор под одним локом (pending —
   под `pending_layouts`); пара `current_layout` + `layout_descriptor` двумя
   локами дала бы геометрию vN с метаданными vN±1.
5. **Семантика `mode`** — зеркало админ-состояния, нормализованное; геометрия
   авторитетна. `auto + явный preset` (Codian/конструктор) даёт `mode:"auto"`
   и геометрию пресета — это документированное поведение, а не баг.
   `mode:""` = «сервер не сообщает» (legacy-кадр), клиент не должен
   сбрасывать состояние в `auto`.
6. **`preset` — raw-строка** редактора (`preset_equal`, `codian_07`), не
   resolved (`preset_equal_25`): resolved уже вычисляется для геометрии,
   второй источник истины не нужен. `size` на провод не выводится; полный
   id/число окон клиент восстанавливает по `slots.len()` (тест
   `raw_preset_is_not_resolved_on_the_wire`).
7. **Совместимость:** additive-поля по `SIGNALING_PROTOCOL.md` §2.2; bump
   `proto_version` не требуется. Старый кадр декодируется в `""`; старый
   клиент новые теги игнорирует. PNM REST не меняется (`payload.mode/preset`
   остаются как есть).

### Отклонено

- `layout_id` в `Layout`: внутренний UUID, COW-нестабилен, утекает в провод.
- `LayoutModeChanged`: дубль `LayoutChanged`, второй канал без общей версии.
- Поля в `vks_layout::Layout`/`NormalizedLayout`: горячий путь композитора,
  лишние аллокации, churn публичной модели; прото-конвертер принимает
  `Layout` + метаданные отдельно.
- Кэш/чтение админского JSON в момент рассылки: stale и вторая точка правды.

## Последствия (Consequences)

- T10a гарантирует только провод. T10c (следующая задача) обязан расширить
  consumer API SDK (`NormalizedLayout`/`ConferenceEvent`) полями `preset/mode`
  и добавить version-guard: кадры с `version` не больше текущей игнорируются
  (защита от гонки VAD-пересадки и admin-apply). До T10c поля на проводе
  есть, но SDK их осознанно отбрасывает.
- Поля 6/81 остаются свободными; `LayoutModeChanged` — только при появлении
  конкретного потребителя режима без геометрии.
- Тесты-гейты: proto roundtrip/legacy, `layout_snapshot` (в т.ч. pending),
  Codian manual на проводе, auto с raw preset, нормализация mode/preset,
  снимок позднего участника в `network_integration`.
