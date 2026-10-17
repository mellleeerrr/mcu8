# LAYOUT ENGINE — спецификация движка раскладок

> Статус: **v0.1 (черновик)**. Заполняется в фазе 4.
> ADR-0012 (Taffy/hadrone-core) — proposed (Q4).

---

## 1. Формат

```rust
// vks-layout/src/layout.rs
#[derive(Clone, Serialize, Deserialize)]
pub struct Layout {
    pub version: u32,          // increment на каждое изменение
    pub kind: LayoutKind,      // Grid | Focus | Speaker | Custom
    pub slots: Vec<Slot>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Slot {
    pub id: u32,
    pub rect: Rect,            // normalized (0..1)
    pub z: u32,                // z-index (layering)
    pub assignment: SlotAssignment,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,  // 0..1
    pub y: f32,  // 0..1
    pub w: f32,  // 0..1
    pub h: f32,  // 0..1
}

#[derive(Clone, Serialize, Deserialize)]
pub enum SlotAssignment {
    Auto,                      // не привязан, round-robin
    Fixed(ParticipantId),      // привязан к конкретному участнику
    Pinned,                    // как Fixed, но sticky (не перемещается при re-layout)
}
```

## 2. Сериализация

- **БД**: JSON (`jsonb` в PostgreSQL). Serde.
- **Signaling**: protobuf `Layout` (см. SIGNALING_PROTOCOL.md §9).
- `vks-layout/storage.rs`: `Layout ↔ JSON` + `Layout ↔ protobuf` (field-by-field).

## 3. Versioning

- `Layout.version: u32`, increment на каждое изменение.
- Новые поля — `optional`/default → backward compat.
- Breaking → новый major + ADR.

## 4. БД-схема (migrations)

```sql
CREATE TABLE layouts (
    id          UUID PRIMARY KEY,              -- UUIDv7
    name        VARCHAR(255) NOT NULL,
    kind        VARCHAR(50) NOT NULL,          -- grid|focus|speaker|custom
    version     INT NOT NULL DEFAULT 1,
    slots       JSONB NOT NULL,                -- [{id, rect:{x,y,w,h}, z, assignment}]
    is_default  BOOLEAN DEFAULT FALSE,
    created_at  TIMESTAMPTZ DEFAULT now(),
    updated_at  TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE room_layouts (
    room_id    UUID REFERENCES rooms(id),
    layout_id  UUID REFERENCES layouts(id),
    is_default BOOLEAN DEFAULT FALSE,
    PRIMARY KEY (room_id, layout_id)
);

CREATE UNIQUE INDEX idx_room_active_layout ON room_layouts(room_id) WHERE is_default;
```

## 5. MCU apply (server-side)

```rust
// vks-layout/src/mcu_renderer.rs
pub fn to_pixel_rects(layout: &Layout, width: u32, height: u32) -> HashMap<SlotId, PixelRect> {
    layout.slots.iter().map(|s| {
        (s.id, PixelRect {
            x: (s.rect.x * width as f32) as u32,
            y: (s.rect.y * height as f32) as u32,
            w: (s.rect.w * width as f32) as u32,
            h: (s.rect.h * height as f32) as u32,
        })
    }).collect()
}
```

**Transitions**: layout change → **crossfade 300ms** (old rects α-blend → new rects). Participant join/leave → auto-assign / remove → grid re-flow → crossfade.

## 6. Client apply (SFU, browser)

**v1: CSS absolute % positioning**
```css
/* vks-layout/src/client_renderer.rs → CSS */
.tile-1 { position: absolute; left: 0%;   top: 0%;   width: 50%; height: 50%; }
.tile-2 { position: absolute; left: 50%;  top: 0%;   width: 50%; height: 50%; }
/* ... */
```
- `<video>` elements positioned by CSS.
- Transition: `transition: all 200ms ease`.

**v2: Taffy** (complex layouts, flexbox/grid).
**WebGL**: не v1 (только effects, future).

## 7. Assignment semantics

```rust
pub fn assign_participants(layout: &mut Layout, participants: &[ParticipantId]) {
    // 1. Fixed slots: match participant_id
    // 2. Pinned slots: keep previous assignment (sticky)
    // 3. Auto slots: fill with remaining participants (round-robin)
    // 4. Unfilled auto slots: empty
}
```

| Assignment | Behavior |
|---|---|
| `Auto` | Не привязан. New participant → next free auto-slot (round-robin). Leave → free slot. |
| `Fixed(pid)` | Привязан. Pid не в room → пустой slot. |
| `Pinned` | Как Fixed, но sticky (не перемещается при re-layout). |

## 8. Presets

```rust
// vks-layout/src/presets.rs
pub fn grid_2x2() -> Layout { ... }     // 4 equal slots
pub fn grid_3x3() -> Layout { ... }     // 9 equal slots
pub fn focus_layout(n: usize) -> Layout { ... }  // 1 large (80%) + n thumbnails (bottom)
pub fn speaker_layout() -> Layout { ... }        // 1 large (center) + n thumbnails
```

## 9. Editor (admin UI)

**Оператор перетаскивает SLOTS (не participants)**:
- Drag slot → update `rect.x/y/w/h`.
- Resize edge → update `w/h`.
- Drag to top/bottom → update `z`.
- Participant avatar onto slot → `assignment = Fixed(pid)`.
- Save → `PUT /api/layouts/{id}`.

**Библиотека**: hadrone-core (Q4) — drag-and-drop.

## 10. Sync (server ↔ clients)

**Push (primary)**:
```
Admin save → vks-admin → storage (БД) → NATS "vks.cluster.room.layout_changed"
  → ALL vks-signaling for room → LayoutChanged broadcast → clients < 1s
```

**Pull (fallback)**: `JoinRoomResult.layout` snapshot (if missed mid-change).

**NOT**: client fetches from admin API (admin = operators only).

**Runtime change**: push via §10. MCU: crossfade 300ms. SFU: CSS transition 200ms. **Total < 2s**. Push-only, no polling.

## 11. Invariants

1. Все rects — normalized (0..1).
2. Rects НЕ пересекаются (для grid-раскладок).
3. Sum of areas ≤ 1.0 (canvas).
4. `z` определяет порядок отрисовки (больше z = поверх).
5. Один participant — один slot (в grid-раскладках).
6. Focus/Speaker: 1 large + N thumbnails (large z > thumbnails z).
